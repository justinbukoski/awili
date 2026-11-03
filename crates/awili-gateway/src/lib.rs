//! `awili-gateway` — a WebSocket client for the **Hermes Agent Gateway**, the
//! bidirectional JSON-RPC 2.0 interface that Hermes's own desktop app uses.
//!
//! Where the (now-removed) `awili-hermes` crate drove the OpenAI-compatible REST
//! surface at `:8642/v1`,
//! this crate drives the gateway WebSocket at `:8645/api/ws` — the transport that
//! gives Awili folder-scoped sessions (`session.create{cwd}`), real-time event
//! streaming, tool approval loops, and the full workspace model.
//!
//! Architecture:
//! - A background thread owns the tungstenite WebSocket and runs the read loop.
//! - Commands (requests, close) flow TO the thread via an mpsc channel.
//! - Events and RPC responses flow FROM the thread via mpsc channels.
//! - Request/response correlation uses monotonic u64 IDs, matching the reference
//!   implementation's `nextId` counter pattern.
//!
//! The client is synchronous (blocking `request()` calls). This matches
//! `awili-hermes`'s blocking-reqwest pattern and works well when called from a
//! gpui background task.
//!
//! Reference implementation studied: `apps/shared/src/json-rpc-gateway.ts` and
//! `apps/desktop/src/` from the Hermes Desktop source (Nous Research, MIT).

use std::collections::HashMap;
use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use url::Url;

/// Typed decode of the gateway's server-push events (the spine the work.rs /
/// gateway_chat.rs reducers re-base onto). See [`events::decode_event`].
pub mod events;

pub mod reconnect;
pub mod rest;
/// Bidirectional map between Hermes session stored IDs and runtime IDs,
/// tracking session identity duality and lineage (fork / branch parents).
pub mod session_id_map;

/// Run lifecycle types — the Work-mode reducer's event vocabulary (`RunStatus`,
/// `RunEvent`, `ApprovalDecision`, …). Ported 1:1 from the deleted `awili-hermes`
/// crate (strangler plan unit 17); the gateway worker synthesizes these from
/// typed gateway pushes for the `awili-app::work` reducer. See
/// [`run_events::decode_run_event`].
// Moved to `awili-work` (D1): the run/status vocabulary is seam vocabulary
// now. Re-exported here verbatim so every existing `awili_gateway::run_events`
// import path keeps compiling; the type identities are awili-work's.
pub use awili_work::run_events;

/// The `subagent.*` reducer (strangler plan unit 18) — folds the typed
/// [`events::GatewayEventKind::Subagent`] pushes into a `SubagentProgress` store
/// and the parent-nested live tree the Work panel's agents view renders. Pure and
/// fixture-testable: arrival time is passed in, never read from a global clock.
/// See [`subagents::upsert_subagent`] / [`subagents::build_subagent_tree`].
pub mod subagents;

// ============================================================================
// Error
// ============================================================================

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("gateway connection failed: {0}")]
    Connect(String),
    #[error("gateway not connected")]
    NotConnected,
    #[error("gateway connection closed")]
    Closed,
    #[error("request timed out: {method}")]
    RequestTimeout { method: String },
    #[error("gateway RPC error: {message}")]
    Rpc {
        /// The JSON-RPC error `code`, if the backend supplied one. The standard
        /// `-32601` means "method not found" (the verb is newer than the running
        /// backend); see [`GatewayError::is_unknown_method`].
        code: Option<i64>,
        message: String,
    },
    #[error("gateway transport error: {0}")]
    Transport(String),
    /// A REST `/api/*` request reached the backend but returned a non-2xx
    /// status. Distinct from [`Transport`](GatewayError::Transport) (the request
    /// never completed) and [`Rpc`](GatewayError::Rpc) (a JSON-RPC error frame on
    /// the WebSocket). The later REST units branch on `status` — e.g. a `404`
    /// from a verb path means the endpoint is newer than the running backend.
    #[error("gateway HTTP {status}")]
    Http { status: u16, body: String },
    #[error("gateway response did not parse: {0}")]
    Decode(String),
    /// The single-use WS `?ticket=` mint failed because the caller's session/cookie
    /// is no longer valid (HTTP 401/403, or the backend returned an empty ticket).
    /// Distinct from [`Http`](GatewayError::Http) so the connect path can prompt a
    /// full reauth instead of retrying with a dead ticket (slice-0 §3.2).
    #[error("gateway reauthentication required")]
    ReauthRequired,
}

impl GatewayError {
    /// Whether this error indicates the backend does not implement the requested
    /// RPC method — i.e. the verb is newer than the running backend. This is the
    /// per-verb defense the [`backend_out_of_date`](SessionCreateResponse::backend_out_of_date)
    /// soft warning references: even when the `desktop_contract` version looks
    /// current (or is absent), a verb can still be rejected as unknown.
    ///
    /// Detected via the JSON-RPC `-32601` "method not found" code, or — for
    /// backends that report a generic code — a message naming an unknown or
    /// unimplemented method.
    pub fn is_unknown_method(&self) -> bool {
        match self {
            GatewayError::Rpc { code, message } => {
                if *code == Some(-32601) {
                    return true;
                }
                let m = message.to_ascii_lowercase();
                m.contains("unknown method")
                    || m.contains("method not found")
                    || m.contains("unknown rpc method")
                    || m.contains("unimplemented method")
            }
            _ => false,
        }
    }

    /// True when the error means the caller must perform a full reauthentication
    /// before it can connect (a single-use ticket could not be minted). The
    /// connect path branches on this to avoid retrying with a dead ticket.
    pub fn is_reauth_required(&self) -> bool {
        matches!(self, GatewayError::ReauthRequired)
    }
}

// ============================================================================
// Connection state
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Idle,
    Connecting,
    Open,
    Closed,
    Error,
}

// ============================================================================
// Protocol — JSON-RPC 2.0 framing (internal)
// ============================================================================

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    params: Value,
}

#[derive(Deserialize)]
struct JsonRpcFrame {
    id: Option<Value>,
    method: Option<String>,
    params: Option<EventParams>,
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    message: Option<String>,
    code: Option<i64>,
}

#[derive(Deserialize)]
struct EventParams {
    #[serde(rename = "type")]
    event_type: Option<String>,
    session_id: Option<String>,
    payload: Option<Value>,
}

// ============================================================================
// Gateway events (server-push)
// ============================================================================

/// A server-push event from the Hermes gateway. Events arrive asynchronously
/// and represent the agent's real-time lifecycle: message streaming, tool
/// progress, approval requests, etc.
#[derive(Debug, Clone)]
pub struct GatewayEvent {
    /// Event type, e.g. `"message.delta"`, `"tool.start"`, `"approval.request"`.
    pub event_type: String,
    /// The session this event belongs to, if scoped. Unscoped events (like
    /// `gateway.ready`) have `None` — they apply to the focused/active session.
    pub session_id: Option<String>,
    /// Event-specific payload. Structure varies by event type.
    pub payload: Option<Value>,
}

impl GatewayEvent {
    /// Whether this event type requires an explicit session ID to be safely
    /// attributed (only `subagent.*` — everything else can fall through to the
    /// focused session).
    pub fn requires_session_id(&self) -> bool {
        self.event_type.starts_with("subagent.")
    }
}

// ============================================================================
// Typed RPC response structs
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCreateResponse {
    pub session_id: String,
    pub stored_session_id: Option<String>,
    pub info: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionResumeResponse {
    pub session_id: String,
    pub info: Option<Value>,
}

// ============================================================================
// desktop_contract version check (slice-0 spec §3.5 / review-01)
// ============================================================================

/// The backend `desktop_contract` version this client requires. Contract v2 is
/// the version in which `file.attach` exists. A backend reporting a contract
/// `< REQUIRED_BACKEND_CONTRACT`, or omitting it entirely, is "out of date" and
/// the consumer should surface a *soft* warning (no hard gate — individual verbs
/// are additionally defended by sniffing an `'unknown method'` RPC error). The
/// crate only reports the number and the warning condition; the UI owns display
/// and any snooze policy.
pub const REQUIRED_BACKEND_CONTRACT: u32 = 2;

/// Extract the integer `desktop_contract` off a `SessionRuntimeInfo` blob (the
/// `info` field of a `session.create` / `session.resume` response).
///
/// Returns `None` when `info` is absent, is not an object, lacks the key, or the
/// value is not a non-negative integer. Accepts the value as a JSON number
/// (`u64`/`i64`) or a numeric string, tolerating either backend encoding. A
/// value exceeding `u32::MAX` saturates to `u32::MAX` (still `>= REQUIRED`).
pub fn parse_backend_contract(info: &Option<Value>) -> Option<u32> {
    let v = info.as_ref()?.get("desktop_contract")?;
    let n: i64 = if let Some(u) = v.as_u64() {
        u.min(u32::MAX as u64) as i64
    } else if let Some(i) = v.as_i64() {
        i
    } else if let Some(s) = v.as_str() {
        // Mirror the numeric path: a positive value above u32::MAX saturates
        // (try u64 first so strings above i64::MAX still saturate, not reject);
        // negatives fall through to i64 and are rejected below.
        let s = s.trim();
        match s.parse::<u64>() {
            Ok(u) => u.min(u32::MAX as u64) as i64,
            Err(_) => s.parse::<i64>().ok()?,
        }
    } else {
        return None;
    };
    if n < 0 {
        return None;
    }
    Some(n.min(u32::MAX as i64) as u32)
}

impl SessionCreateResponse {
    /// The backend's reported `desktop_contract` version, if present and valid.
    /// `None` means the backend omitted it (treated as out of date).
    pub fn backend_contract(&self) -> Option<u32> {
        parse_backend_contract(&self.info)
    }

    /// Whether the backend is older than this client requires (or omits the
    /// contract). `true` is the soft "backend out of date" warning condition.
    pub fn backend_out_of_date(&self) -> bool {
        self.backend_contract()
            .is_none_or(|c| c < REQUIRED_BACKEND_CONTRACT)
    }
}

impl SessionResumeResponse {
    /// The backend's reported `desktop_contract` version, if present and valid.
    /// `None` means the backend omitted it (treated as out of date).
    pub fn backend_contract(&self) -> Option<u32> {
        parse_backend_contract(&self.info)
    }

    /// Whether the backend is older than this client requires (or omits the
    /// contract). `true` is the soft "backend out of date" warning condition.
    pub fn backend_out_of_date(&self) -> bool {
        self.backend_contract()
            .is_none_or(|c| c < REQUIRED_BACKEND_CONTRACT)
    }
}

// ============================================================================
// Internal: command channel to the WS thread
// ============================================================================

enum Command {
    Request {
        id: u64,
        frame: String,
        response_tx: mpsc::Sender<Result<Value, GatewayError>>,
    },
    Close,
}

// ============================================================================
// URL helper
// ============================================================================

/// Build a gateway WebSocket URL from host, port, and auth token.
///
/// Returns `ws://host:port/api/ws?token=TOKEN`.
pub fn gateway_url(host: &str, port: u16, token: &str) -> String {
    format!("ws://{host}:{port}/api/ws?token={token}")
}

/// Build a gateway WebSocket URL that authenticates with a single-use
/// `?ticket=` (OAuth/remote profiles) instead of a static `?token=`.
/// Returns `ws://{host}:{port}/api/ws?ticket=TICKET`. The ticket is minted per
/// connect via `POST /api/auth/ws-ticket`; see [`parse_ws_ticket`].
pub fn ws_ticket_url(host: &str, port: u16, ticket: &str) -> String {
    format!("ws://{host}:{port}/api/ws?ticket={ticket}")
}

/// Parse the outcome of a `POST /api/auth/ws-ticket` mint into a fresh
/// single-use ticket or a typed error. Pure: the caller performs the (cookie-
/// authenticated, [LIVE]) HTTP request and hands the `(status, body)` here.
///
/// - `401` / `403` → [`GatewayError::ReauthRequired`] (cookie/session expired —
///   the caller must reauthenticate; it must NOT reuse any prior ticket).
/// - any other non-2xx → [`GatewayError::Http`].
/// - `2xx` with `{"ticket":"..."}` non-empty → `Ok(ticket)`.
/// - `2xx` with an empty `ticket` string → [`GatewayError::ReauthRequired`]
///   (a blank ticket is a dead ticket — never build a URL from it).
/// - `2xx` with an absent `ticket` field or an unparseable body →
///   [`GatewayError::Decode`].
pub fn parse_ws_ticket(status: u16, body: &str) -> Result<String, GatewayError> {
    if status == 401 || status == 403 {
        return Err(GatewayError::ReauthRequired);
    }
    if !(200..300).contains(&status) {
        return Err(GatewayError::Http {
            status,
            body: body.to_string(),
        });
    }
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| GatewayError::Decode(e.to_string()))?;
    let ticket = v.get("ticket").and_then(|t| t.as_str()).ok_or_else(|| {
        GatewayError::Decode("mint response missing `ticket` string field".to_string())
    })?;
    if ticket.is_empty() {
        return Err(GatewayError::ReauthRequired);
    }
    Ok(ticket.to_string())
}

// ============================================================================
// Client
// ============================================================================

/// A blocking WebSocket client for the Hermes Agent gateway.
///
/// The client connects to the gateway, sends JSON-RPC 2.0 requests, and
/// receives both RPC responses and server-push events. Events are queued in an
/// internal channel and can be polled with [`try_recv_event`](Self::try_recv_event)
/// or waited on with [`recv_event`](Self::recv_event).
///
/// # Example
///
/// ```no_run
/// use awili_gateway::{GatewayClient, gateway_url};
///
/// let url = gateway_url("10.77.68.94", 8645, "my-token");
/// let client = GatewayClient::connect(&url).unwrap();
///
/// let resp = client.session_create("/tmp/my-project").unwrap();
/// println!("session: {}", resp.session_id);
///
/// client.prompt_submit(&resp.session_id, "hello").unwrap();
///
/// // Poll for events (message.delta, tool.start, etc.)
/// while let Some(event) = client.recv_event(std::time::Duration::from_secs(1)) {
///     println!("{}: {:?}", event.event_type, event.payload);
/// }
/// ```
pub struct GatewayClient {
    cmd_tx: mpsc::Sender<Command>,
    event_rx: mpsc::Receiver<GatewayEvent>,
    state: Arc<Mutex<ConnectionState>>,
    next_id: AtomicU64,
    thread: Option<thread::JoinHandle<()>>,
}

impl GatewayClient {
    /// Connect to the Hermes gateway WebSocket.
    ///
    /// `ws_url` is the full WebSocket URL including the token query parameter,
    /// e.g. `ws://10.77.68.94:8645/api/ws?token=...`. Use [`gateway_url`] to
    /// build it from parts.
    ///
    /// Blocks until the TCP + WebSocket handshake completes (or fails). Both the
    /// TCP connect and the handshake are time-bounded (M4): a host that accepts
    /// the connection but never completes the WebSocket upgrade fails fast with a
    /// [`GatewayError::Connect`] instead of blocking the caller thread forever.
    pub fn connect(ws_url: &str) -> Result<Self, GatewayError> {
        Self::connect_with_timeouts(ws_url, CONNECT_TIMEOUT, HANDSHAKE_TIMEOUT)
    }

    /// Connect with explicit connect/handshake deadlines. Split out of [`connect`]
    /// so the handshake-stall timeout is testable against a silent server without
    /// waiting the full production bound.
    fn connect_with_timeouts(
        ws_url: &str,
        connect_to: Duration,
        handshake_to: Duration,
    ) -> Result<Self, GatewayError> {
        let url =
            Url::parse(ws_url).map_err(|e| GatewayError::Connect(format!("invalid URL: {e}")))?;

        let state = Arc::new(Mutex::new(ConnectionState::Connecting));
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (event_tx, event_rx) = mpsc::channel::<GatewayEvent>();

        let state_clone = state.clone();

        let mut socket = ws_handshake(&url, connect_to, handshake_to)?;

        set_read_timeout(&mut socket, Duration::from_millis(50))?;

        *state.lock().unwrap() = ConnectionState::Open;

        let thread = thread::Builder::new()
            .name("awili-gateway-ws".into())
            .spawn(move || {
                ws_read_loop(socket, cmd_rx, event_tx, state_clone);
            })
            .map_err(|e| GatewayError::Connect(format!("thread spawn failed: {e}")))?;

        Ok(Self {
            cmd_tx,
            event_rx,
            state,
            next_id: AtomicU64::new(1),
            thread: Some(thread),
        })
    }

    /// Send a JSON-RPC request and block until the response arrives.
    /// Default timeout: 30 seconds (the Hermes Desktop app uses 30s).
    pub fn request(&self, method: &str, params: Value) -> Result<Value, GatewayError> {
        self.request_timeout(method, params, Duration::from_secs(30))
    }

    /// Like [`request`](Self::request) but with a custom timeout.
    pub fn request_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, GatewayError> {
        if *self.state.lock().unwrap() != ConnectionState::Open {
            return Err(GatewayError::NotConnected);
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = serde_json::to_string(&JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        })
        .map_err(|e| GatewayError::Transport(e.to_string()))?;

        let (response_tx, response_rx) = mpsc::channel();

        self.cmd_tx
            .send(Command::Request {
                id,
                frame,
                response_tx,
            })
            .map_err(|_| GatewayError::Closed)?;

        response_rx.recv_timeout(timeout).map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => GatewayError::RequestTimeout {
                method: method.to_string(),
            },
            mpsc::RecvTimeoutError::Disconnected => GatewayError::Closed,
        })?
    }

    // ---- Typed convenience methods (the RPCs Awili needs most) ----

    /// Create a new session scoped to a working directory.
    pub fn session_create(&self, cwd: &str) -> Result<SessionCreateResponse, GatewayError> {
        let result = self.request(
            "session.create",
            serde_json::json!({ "cwd": cwd, "cols": 96 }),
        )?;
        serde_json::from_value(result).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// Create a session with full options (model, provider, reasoning effort).
    pub fn session_create_full(
        &self,
        cwd: &str,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<SessionCreateResponse, GatewayError> {
        let mut params = serde_json::json!({ "cwd": cwd, "cols": 96 });
        if let Some(m) = model {
            params["model"] = Value::String(m.to_string());
        }
        if let Some(p) = provider {
            params["provider"] = Value::String(p.to_string());
        }
        let result = self.request("session.create", params)?;
        serde_json::from_value(result).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// Resume an existing session by its stored ID.
    pub fn session_resume(&self, session_id: &str) -> Result<SessionResumeResponse, GatewayError> {
        let result = self.request(
            "session.resume",
            serde_json::json!({ "session_id": session_id }),
        )?;
        serde_json::from_value(result).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// Close a session.
    pub fn session_close(&self, session_id: &str) -> Result<(), GatewayError> {
        self.request(
            "session.close",
            serde_json::json!({ "session_id": session_id }),
        )?;
        Ok(())
    }

    /// Get usage stats for a session.
    pub fn session_usage(&self, session_id: &str) -> Result<Value, GatewayError> {
        self.request(
            "session.usage",
            serde_json::json!({ "session_id": session_id }),
        )
    }

    /// Submit a user message to an active session.
    pub fn prompt_submit(&self, session_id: &str, text: &str) -> Result<Value, GatewayError> {
        self.request(
            "prompt.submit",
            serde_json::json!({ "session_id": session_id, "text": text }),
        )
    }

    /// Respond to an approval request (tool approval gate).
    ///
    /// `choice` matches the desktop protocol: `"once"` to allow this invocation,
    /// `"always"` to allow permanently, `"deny"` to reject.
    pub fn approval_respond(&self, session_id: &str, choice: &str) -> Result<Value, GatewayError> {
        self.request(
            "approval.respond",
            serde_json::json!({ "session_id": session_id, "choice": choice }),
        )
    }

    /// Respond to a clarification request.
    pub fn clarify_respond(&self, session_id: &str, answer: &str) -> Result<Value, GatewayError> {
        self.request(
            "clarify.respond",
            serde_json::json!({ "session_id": session_id, "answer": answer }),
        )
    }

    /// Respond to a terminal read request from the agent.
    pub fn terminal_read_respond(
        &self,
        session_id: &str,
        request_id: &str,
        text: &str,
    ) -> Result<Value, GatewayError> {
        self.request(
            "terminal.read.respond",
            serde_json::json!({
                "session_id": session_id,
                "request_id": request_id,
                "text": text,
            }),
        )
    }

    /// Respond to a `sudo.request` (a tool asking for a sudo password). Mirrors
    /// the Hermes desktop contract `sudo.respond {request_id, password}` exactly
    /// (no `session_id`). An EMPTY password is the documented safe refusal: the
    /// backend treats it as a failed sudo (no command runs), so a declined prompt
    /// unblocks the agent without granting elevation. The password is sent once
    /// and never stored or logged by the caller.
    pub fn sudo_respond(&self, request_id: &str, password: &str) -> Result<Value, GatewayError> {
        self.request(
            "sudo.respond",
            serde_json::json!({ "request_id": request_id, "password": password }),
        )
    }

    /// Respond to a `secret.request` (a skill asking for a credential value).
    /// Mirrors `secret.respond {request_id, value}`. An empty value is a refusal.
    /// The value is sent once and never stored or logged by the caller.
    pub fn secret_respond(&self, request_id: &str, value: &str) -> Result<Value, GatewayError> {
        self.request(
            "secret.respond",
            serde_json::json!({ "request_id": request_id, "value": value }),
        )
    }

    /// Get available model options.
    pub fn model_options(&self) -> Result<Value, GatewayError> {
        self.request("model.options", serde_json::json!({}))
    }

    /// Get a gateway config value.
    pub fn config_get(&self, key: &str) -> Result<Value, GatewayError> {
        self.request("config.get", serde_json::json!({ "key": key }))
    }

    /// Attach an image by filesystem path (the gateway reads the file).
    pub fn image_attach(&self, session_id: &str, path: &str) -> Result<Value, GatewayError> {
        self.request(
            "image.attach",
            serde_json::json!({ "session_id": session_id, "path": path }),
        )
    }

    /// Attach an image as base64 data.
    pub fn image_attach_bytes(
        &self,
        session_id: &str,
        content_base64: &str,
        filename: &str,
    ) -> Result<Value, GatewayError> {
        self.request(
            "image.attach_bytes",
            serde_json::json!({
                "session_id": session_id,
                "content_base64": content_base64,
                "filename": filename,
            }),
        )
    }

    /// Detach the current image attachment.
    pub fn image_detach(&self, session_id: &str) -> Result<Value, GatewayError> {
        self.request(
            "image.detach",
            serde_json::json!({ "session_id": session_id }),
        )
    }

    /// Attach a file (code, data) to the session. Returns `{ref_text}`.
    pub fn file_attach(
        &self,
        session_id: &str,
        name: &str,
        path: &str,
    ) -> Result<Value, GatewayError> {
        self.request(
            "file.attach",
            serde_json::json!({
                "session_id": session_id,
                "name": name,
                "path": path,
            }),
        )
    }

    /// Set the session's working directory.
    pub fn session_cwd_set(&self, session_id: &str, cwd: &str) -> Result<Value, GatewayError> {
        self.request(
            "session.cwd.set",
            serde_json::json!({ "session_id": session_id, "cwd": cwd }),
        )
    }

    /// Interrupt the current agent execution.
    pub fn session_interrupt(&self, session_id: &str) -> Result<Value, GatewayError> {
        self.request(
            "session.interrupt",
            serde_json::json!({ "session_id": session_id }),
        )
    }

    /// Steer the agent mid-turn with additional guidance.
    pub fn session_steer(&self, session_id: &str, text: &str) -> Result<Value, GatewayError> {
        self.request(
            "session.steer",
            serde_json::json!({ "session_id": session_id, "text": text }),
        )
    }

    /// Execute a slash command in the session.
    pub fn slash_exec(&self, session_id: &str, command: &str) -> Result<Value, GatewayError> {
        self.request(
            "slash.exec",
            serde_json::json!({ "session_id": session_id, "command": command }),
        )
    }

    /// Get the commands catalog (available slash commands).
    pub fn commands_catalog(&self) -> Result<Value, GatewayError> {
        self.request("commands.catalog", serde_json::json!({}))
    }

    /// Path completion (for file picker / autocomplete).
    pub fn complete_path(&self, word: &str, cwd: Option<&str>) -> Result<Value, GatewayError> {
        let mut params = serde_json::json!({ "word": word });
        if let Some(c) = cwd {
            params["cwd"] = serde_json::json!(c);
        }
        self.request("complete.path", params)
    }

    // ---- Event polling ----

    /// Non-blocking check for the next gateway event. Returns `None` if no
    /// event is queued.
    pub fn try_recv_event(&self) -> Option<GatewayEvent> {
        self.event_rx.try_recv().ok()
    }

    /// Blocking wait for the next gateway event, up to `timeout`. Returns
    /// `None` on timeout or if the connection is closed.
    pub fn recv_event(&self, timeout: Duration) -> Option<GatewayEvent> {
        self.event_rx.recv_timeout(timeout).ok()
    }

    /// Drain all currently queued events into a Vec.
    pub fn drain_events(&self) -> Vec<GatewayEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.event_rx.try_recv() {
            events.push(ev);
        }
        events
    }

    // ---- Lifecycle ----

    /// Current connection state.
    pub fn connection_state(&self) -> ConnectionState {
        *self.state.lock().unwrap()
    }

    /// Whether the connection is open and ready for requests.
    pub fn is_connected(&self) -> bool {
        self.connection_state() == ConnectionState::Open
    }

    /// Close the gateway connection and join the background thread.
    pub fn close(&mut self) {
        let _ = self.cmd_tx.send(Command::Close);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for GatewayClient {
    fn drop(&mut self) {
        self.close();
    }
}

// ============================================================================
// WebSocket thread internals
// ============================================================================

/// Production TCP-connect deadline (M4). Matches `HermesClient`'s connect bound.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Production WebSocket-handshake deadline (M4). Read/write timeouts installed on
/// the socket before the upgrade, so a silent server can't stall the handshake.
/// RC2-SVC4 (F0747): the write timeout stays at this bound after the upgrade.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Open a time-bounded WebSocket to `url` (M4).
///
/// For the `ws://` scheme (the only one [`gateway_url`] builds) we drive the TCP
/// connect with [`TcpStream::connect_timeout`] and install read/write timeouts
/// *before* handing the stream to tungstenite for the upgrade — so neither the
/// TCP connect nor the handshake can block indefinitely. `url::Url::socket_addrs`
/// resolves the host (handling IPv6 brackets) and honours the explicit/default
/// port. The `wss://` branch falls back to `tungstenite::connect`, which performs
/// the TLS+WS upgrade itself but without a pre-handshake timeout; it is a
/// defensive path only, since Awili always connects over plain `ws://`.
fn ws_handshake(
    url: &Url,
    connect_to: Duration,
    handshake_to: Duration,
) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, GatewayError> {
    if url.scheme() != "ws" {
        let (socket, _response) =
            tungstenite::connect(url.as_str()).map_err(|e| GatewayError::Connect(e.to_string()))?;
        return Ok(socket);
    }

    let addrs = url
        .socket_addrs(|| url.port_or_known_default())
        .map_err(|e| GatewayError::Connect(format!("resolve gateway address: {e}")))?;

    let mut last_err = "no addresses resolved".to_string();
    let mut tcp = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, connect_to) {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    let tcp =
        tcp.ok_or_else(|| GatewayError::Connect(format!("tcp connect failed: {last_err}")))?;

    tcp.set_read_timeout(Some(handshake_to))
        .map_err(|e| GatewayError::Connect(format!("set_read_timeout: {e}")))?;
    tcp.set_write_timeout(Some(handshake_to))
        .map_err(|e| GatewayError::Connect(format!("set_write_timeout: {e}")))?;

    let (socket, _response) = tungstenite::client(url.as_str(), MaybeTlsStream::Plain(tcp))
        .map_err(|e| GatewayError::Connect(format!("ws handshake: {e}")))?;

    // RC2-SVC4 (F0747): the write timeout STAYS installed after the upgrade.
    // A server that stops draining the socket must not block a send — and with
    // it the read loop, Close and every pending request — forever; a send that
    // cannot finish within the same bound fails the connection instead (see
    // `ws_read_loop`). The caller installs the 50ms *read* timeout separately
    // for the poll cadence.
    Ok(socket)
}

fn set_read_timeout(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Duration,
) -> Result<(), GatewayError> {
    let result = match socket.get_mut() {
        MaybeTlsStream::Plain(tcp) => tcp.set_read_timeout(Some(timeout)),
        // rustls (Owner 2026-08-13): the inner TcpStream lives in `sock`.
        MaybeTlsStream::Rustls(tls) => tls.sock.set_read_timeout(Some(timeout)),
        _ => Ok(()),
    };
    result.map_err(|e| GatewayError::Connect(format!("set_read_timeout: {e}")))
}

fn ws_read_loop(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    cmd_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::Sender<GatewayEvent>,
    state: Arc<Mutex<ConnectionState>>,
) {
    let mut pending: HashMap<u64, mpsc::Sender<Result<Value, GatewayError>>> = HashMap::new();

    loop {
        // 1. Try to read a WebSocket message (times out after 50ms if no data).
        match socket.read() {
            Ok(Message::Text(text)) => {
                handle_frame(&text, &mut pending, &event_tx);
            }
            Ok(Message::Close(_)) => {
                break;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                // Read timeout — no data yet, fall through to drain commands.
            }
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                break;
            }
            Err(_) => {
                *state.lock().unwrap() = ConnectionState::Error;
                reject_all_pending(&mut pending);
                return;
            }
        }

        // 2. Drain pending commands from the caller.
        loop {
            match cmd_rx.try_recv() {
                Ok(Command::Request {
                    id,
                    frame,
                    response_tx,
                }) => {
                    if let Err(e) = socket.send(Message::Text(frame.into())) {
                        let _ = response_tx.send(Err(GatewayError::Transport(e.to_string())));
                        // RC2-SVC4 (F0747): a send that failed (a write
                        // timeout included) may have left part of a frame on
                        // the wire, so the connection cannot carry another
                        // frame: fail it and every pending request.
                        *state.lock().unwrap() = ConnectionState::Error;
                        reject_all_pending(&mut pending);
                        return;
                    } else {
                        pending.insert(id, response_tx);
                    }
                }
                Ok(Command::Close) => {
                    let _ = socket.close(None);
                    *state.lock().unwrap() = ConnectionState::Closed;
                    reject_all_pending(&mut pending);
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    let _ = socket.close(None);
                    *state.lock().unwrap() = ConnectionState::Closed;
                    reject_all_pending(&mut pending);
                    return;
                }
            }
        }
    }

    *state.lock().unwrap() = ConnectionState::Closed;
    reject_all_pending(&mut pending);
}

fn handle_frame(
    text: &str,
    pending: &mut HashMap<u64, mpsc::Sender<Result<Value, GatewayError>>>,
    event_tx: &mpsc::Sender<GatewayEvent>,
) {
    let frame: JsonRpcFrame = match serde_json::from_str(text) {
        Ok(f) => f,
        Err(_) => return,
    };

    // RPC response (has an `id` field).
    if let Some(id_val) = &frame.id {
        let id = match id_val {
            Value::Number(n) => n.as_u64(),
            _ => None,
        };
        if let Some(id) = id {
            if let Some(tx) = pending.remove(&id) {
                if let Some(err) = frame.error {
                    let msg = err.message.unwrap_or_else(|| "Hermes RPC failed".into());
                    let _ = tx.send(Err(GatewayError::Rpc {
                        code: err.code,
                        message: msg,
                    }));
                } else {
                    let _ = tx.send(Ok(frame.result.unwrap_or(Value::Null)));
                }
            }
        }
        return;
    }

    // Server-push event (method == "event", params.type is the event name).
    if frame.method.as_deref() == Some("event") {
        if let Some(params) = frame.params {
            if let Some(event_type) = params.event_type {
                let _ = event_tx.send(GatewayEvent {
                    event_type,
                    session_id: params.session_id,
                    payload: params.payload,
                });
            }
        }
    }
}

fn reject_all_pending(pending: &mut HashMap<u64, mpsc::Sender<Result<Value, GatewayError>>>) {
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(GatewayError::Closed));
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Connect/handshake timeout (M4) ----

    #[test]
    fn handshake_stall_fails_fast_as_connect_error() {
        // A server that accepts the TCP connection but never speaks the WebSocket
        // upgrade must NOT block the caller forever. With a 300ms handshake bound
        // the connect attempt should return a `Connect` error well under a second,
        // not hang until some OS-default timeout.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Hold accepted connections open but silent for the duration of the test.
        thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept() {
                held.push(stream);
                if held.len() >= 4 {
                    break;
                }
            }
            thread::sleep(Duration::from_secs(2));
        });

        let url = format!("ws://{addr}/api/ws?token=x");
        let start = std::time::Instant::now();
        let res = GatewayClient::connect_with_timeouts(
            &url,
            Duration::from_millis(500),
            Duration::from_millis(300),
        );
        let elapsed = start.elapsed();

        // `res.err()` drops the Ok value without requiring `GatewayClient: Debug`.
        let err = res
            .err()
            .expect("stalled handshake must error, not connect");
        assert!(
            matches!(err, GatewayError::Connect(_)),
            "timeout should surface as GatewayError::Connect, got {err:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "must fail fast via the handshake timeout, took {elapsed:?}"
        );
    }

    /// RC2-SVC4 (F0747): after the upgrade, a server that stops reading must
    /// not block a send forever. The send fails within the write bound, the
    /// connection is marked failed, and Close still returns promptly.
    #[test]
    fn rc2svc4_a_stalled_reader_fails_the_send_instead_of_blocking() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (held_tx, held_rx) = mpsc::channel();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Complete the WebSocket upgrade, then never read again.
            let ws = tungstenite::accept(stream).unwrap();
            let _ = held_tx.send(());
            thread::sleep(Duration::from_secs(30));
            drop(ws);
        });
        let url = format!("ws://{addr}/api/ws?token=x");
        let client = GatewayClient::connect_with_timeouts(
            &url,
            Duration::from_millis(500),
            Duration::from_millis(300),
        )
        .expect("connect");
        held_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Never dropped on a failed assertion: a pre-fix client would block
        // the test forever in `close` (its read loop is stuck in `send`).
        let client = std::mem::ManuallyDrop::new(client);
        let big = "x".repeat(48 * 1024 * 1024);
        let start = std::time::Instant::now();
        let res = client.request_timeout(
            "probe",
            serde_json::json!({ "pad": big }),
            Duration::from_secs(10),
        );
        assert!(
            matches!(
                res,
                Err(GatewayError::Transport(_)) | Err(GatewayError::Closed)
            ),
            "a stalled send must fail the request, got {res:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "took {:?}",
            start.elapsed()
        );
        assert_eq!(client.connection_state(), ConnectionState::Error);
        let mut client = std::mem::ManuallyDrop::into_inner(client);
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            client.close();
            let _ = done_tx.send(());
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(3)).is_ok(),
            "close must return"
        );
    }

    // ---- JSON-RPC frame parsing ----

    #[test]
    fn parse_rpc_response() {
        let text = r#"{"jsonrpc":"2.0","id":1,"result":{"session_id":"abc-123"}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(text).unwrap();
        assert!(frame.id.is_some());
        assert!(frame.result.is_some());
        assert!(frame.error.is_none());
    }

    #[test]
    fn parse_rpc_error_response() {
        let text =
            r#"{"jsonrpc":"2.0","id":2,"error":{"message":"session not found","code":-32000}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(text).unwrap();
        assert!(frame.error.is_some());
        assert_eq!(
            frame.error.as_ref().unwrap().message.as_deref(),
            Some("session not found")
        );
    }

    #[test]
    fn parse_event_frame_with_session() {
        let text = r#"{"method":"event","params":{"type":"message.delta","session_id":"s1","payload":{"text":"hello"}}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(text).unwrap();
        assert_eq!(frame.method.as_deref(), Some("event"));
        let params = frame.params.unwrap();
        assert_eq!(params.event_type.as_deref(), Some("message.delta"));
        assert_eq!(params.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn parse_event_frame_no_session() {
        let text = r#"{"method":"event","params":{"type":"gateway.ready"}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(text).unwrap();
        let params = frame.params.unwrap();
        assert_eq!(params.event_type.as_deref(), Some("gateway.ready"));
        assert!(params.session_id.is_none());
        assert!(params.payload.is_none());
    }

    // ---- handle_frame integration ----

    #[test]
    fn handle_frame_dispatches_event() {
        let mut pending = HashMap::new();
        let (event_tx, event_rx) = mpsc::channel();

        let text = r#"{"method":"event","params":{"type":"tool.start","session_id":"s1","payload":{"tool":"terminal"}}}"#;
        handle_frame(text, &mut pending, &event_tx);

        let event = event_rx.try_recv().unwrap();
        assert_eq!(event.event_type, "tool.start");
        assert_eq!(event.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn handle_frame_resolves_pending_ok() {
        let (response_tx, response_rx) = mpsc::channel();
        let mut pending = HashMap::new();
        pending.insert(42u64, response_tx);
        let (event_tx, _) = mpsc::channel();

        let text = r#"{"jsonrpc":"2.0","id":42,"result":{"ok":true}}"#;
        handle_frame(text, &mut pending, &event_tx);

        let result = response_rx.try_recv().unwrap().unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
        assert!(pending.is_empty());
    }

    #[test]
    fn handle_frame_rejects_pending_on_error() {
        let (response_tx, response_rx) = mpsc::channel();
        let mut pending = HashMap::new();
        pending.insert(7u64, response_tx);
        let (event_tx, _) = mpsc::channel();

        let text = r#"{"jsonrpc":"2.0","id":7,"error":{"message":"boom","code":-32000}}"#;
        handle_frame(text, &mut pending, &event_tx);

        let result = response_rx.try_recv().unwrap();
        match result {
            Err(GatewayError::Rpc { code, message }) => {
                // The JSON-RPC `code` is surfaced onto the typed error (unit 7).
                assert_eq!(code, Some(-32000));
                assert_eq!(message, "boom");
            }
            other => panic!("expected GatewayError::Rpc, got {other:?}"),
        }
        assert!(pending.is_empty());
    }

    #[test]
    fn handle_frame_error_without_code_surfaces_none() {
        let (response_tx, response_rx) = mpsc::channel();
        let mut pending = HashMap::new();
        pending.insert(8u64, response_tx);
        let (event_tx, _) = mpsc::channel();

        // No `code` field — `code` decodes to None, message preserved.
        let text = r#"{"jsonrpc":"2.0","id":8,"error":{"message":"boom"}}"#;
        handle_frame(text, &mut pending, &event_tx);

        match response_rx.try_recv().unwrap() {
            Err(GatewayError::Rpc { code, message }) => {
                assert_eq!(code, None);
                assert_eq!(message, "boom");
            }
            other => panic!("expected GatewayError::Rpc, got {other:?}"),
        }
    }

    // ---- is_unknown_method (unit 7: per-verb out-of-date defense) ----

    fn rpc(code: Option<i64>, message: &str) -> GatewayError {
        GatewayError::Rpc {
            code,
            message: message.into(),
        }
    }

    #[test]
    fn unknown_method_by_jsonrpc_code() {
        // -32601 is the JSON-RPC standard "method not found" code.
        assert!(rpc(Some(-32601), "anything at all").is_unknown_method());
    }

    #[test]
    fn unknown_method_by_message_when_code_generic() {
        // Backends that report a generic code still get caught by message sniff.
        assert!(rpc(Some(-32000), "Unknown method: file.attach").is_unknown_method());
        assert!(rpc(None, "method not found").is_unknown_method());
        assert!(rpc(Some(0), "RPC handler unimplemented method").is_unknown_method());
        // Case-insensitive.
        assert!(rpc(None, "UNKNOWN METHOD foo").is_unknown_method());
    }

    #[test]
    fn unknown_method_false_for_ordinary_rpc_error() {
        // A real runtime error (e.g. session not found) is NOT an out-of-date verb.
        assert!(!rpc(Some(-32000), "session not found").is_unknown_method());
        assert!(!rpc(None, "boom").is_unknown_method());
        assert!(!rpc(Some(-32602), "invalid params").is_unknown_method());
        // A known verb can validly fail with a generic "not implemented" — that
        // is a runtime/feature condition, NOT a missing method (Codex unit-7 BLOCK).
        assert!(!rpc(Some(-32000), "feature not implemented").is_unknown_method());
        assert!(!rpc(None, "provider not implemented for this model").is_unknown_method());
    }

    #[test]
    fn unknown_method_false_for_non_rpc_errors() {
        assert!(!GatewayError::NotConnected.is_unknown_method());
        assert!(!GatewayError::Closed.is_unknown_method());
        assert!(!GatewayError::Transport("boom".into()).is_unknown_method());
        assert!(!GatewayError::Decode("bad".into()).is_unknown_method());
        assert!(!GatewayError::RequestTimeout {
            method: "file.attach".into()
        }
        .is_unknown_method());
    }

    #[test]
    fn handle_frame_ignores_malformed_json() {
        let mut pending = HashMap::new();
        let (event_tx, event_rx) = mpsc::channel();

        handle_frame("not json at all {{{", &mut pending, &event_tx);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn handle_frame_ignores_unknown_id() {
        let mut pending = HashMap::new();
        let (event_tx, _) = mpsc::channel();

        let text = r#"{"jsonrpc":"2.0","id":999,"result":{"orphan":true}}"#;
        handle_frame(text, &mut pending, &event_tx);
        // No panic, no crash — orphaned responses are silently dropped.
    }

    // ---- Typed response deserialization ----

    #[test]
    fn session_create_response_deserializes() {
        let v = serde_json::json!({
            "session_id": "live-123",
            "stored_session_id": "stored-456",
            "info": { "cwd": "/tmp/project" }
        });
        let resp: SessionCreateResponse = serde_json::from_value(v).unwrap();
        assert_eq!(resp.session_id, "live-123");
        assert_eq!(resp.stored_session_id.as_deref(), Some("stored-456"));
    }

    #[test]
    fn session_create_response_minimal() {
        let v = serde_json::json!({ "session_id": "live-789" });
        let resp: SessionCreateResponse = serde_json::from_value(v).unwrap();
        assert_eq!(resp.session_id, "live-789");
        assert!(resp.stored_session_id.is_none());
        assert!(resp.info.is_none());
    }

    #[test]
    fn session_resume_response_deserializes() {
        let v = serde_json::json!({
            "session_id": "resumed-1",
            "info": { "cwd": "/home/user/project" }
        });
        let resp: SessionResumeResponse = serde_json::from_value(v).unwrap();
        assert_eq!(resp.session_id, "resumed-1");
    }

    // ---- desktop_contract check ----

    fn create_with_info(info: Value) -> SessionCreateResponse {
        let v = serde_json::json!({ "session_id": "s", "info": info });
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn contract_present_at_required_is_current() {
        let r = create_with_info(serde_json::json!({ "desktop_contract": 2 }));
        assert_eq!(r.backend_contract(), Some(2));
        assert!(!r.backend_out_of_date());
    }

    #[test]
    fn contract_above_required_is_current() {
        let r = create_with_info(serde_json::json!({ "desktop_contract": 5 }));
        assert_eq!(r.backend_contract(), Some(5));
        assert!(!r.backend_out_of_date());
    }

    #[test]
    fn contract_below_required_is_out_of_date() {
        let r = create_with_info(serde_json::json!({ "desktop_contract": 1 }));
        assert_eq!(r.backend_contract(), Some(1));
        assert!(r.backend_out_of_date());
    }

    #[test]
    fn contract_absent_key_is_out_of_date() {
        let r = create_with_info(serde_json::json!({ "cwd": "/tmp" }));
        assert_eq!(r.backend_contract(), None);
        assert!(r.backend_out_of_date());
    }

    #[test]
    fn contract_absent_info_is_out_of_date() {
        let v = serde_json::json!({ "session_id": "s" });
        let r: SessionCreateResponse = serde_json::from_value(v).unwrap();
        assert!(r.info.is_none());
        assert_eq!(r.backend_contract(), None);
        assert!(r.backend_out_of_date());
    }

    #[test]
    fn contract_info_not_object_is_none() {
        let r = create_with_info(serde_json::json!("not-an-object"));
        assert_eq!(r.backend_contract(), None);
        assert!(r.backend_out_of_date());
    }

    #[test]
    fn contract_numeric_string_is_parsed() {
        let r = create_with_info(serde_json::json!({ "desktop_contract": "3" }));
        assert_eq!(r.backend_contract(), Some(3));
        assert!(!r.backend_out_of_date());
    }

    #[test]
    fn contract_non_integer_value_is_none() {
        // Floats and non-numeric strings are not valid integer contracts.
        let r = create_with_info(serde_json::json!({ "desktop_contract": "v2" }));
        assert_eq!(r.backend_contract(), None);
        assert!(r.backend_out_of_date());
        let r2 = create_with_info(serde_json::json!({ "desktop_contract": 2.5 }));
        assert_eq!(r2.backend_contract(), None);
        assert!(r2.backend_out_of_date());
    }

    #[test]
    fn contract_oversized_numeric_string_saturates() {
        // A numeric string above u32::MAX (and above i64::MAX) saturates to
        // u32::MAX, mirroring the JSON u64::MAX number path — not rejected.
        let r = create_with_info(serde_json::json!({ "desktop_contract": "4294967296" }));
        assert_eq!(r.backend_contract(), Some(u32::MAX));
        assert!(!r.backend_out_of_date());
        let r2 =
            create_with_info(serde_json::json!({ "desktop_contract": "18446744073709551615" }));
        assert_eq!(r2.backend_contract(), Some(u32::MAX));
        assert!(!r2.backend_out_of_date());
    }

    #[test]
    fn contract_negative_is_none() {
        let r = create_with_info(serde_json::json!({ "desktop_contract": -1 }));
        assert_eq!(r.backend_contract(), None);
        assert!(r.backend_out_of_date());
    }

    #[test]
    fn contract_huge_value_saturates_and_is_current() {
        let r = create_with_info(serde_json::json!({ "desktop_contract": u64::MAX }));
        assert_eq!(r.backend_contract(), Some(u32::MAX));
        assert!(!r.backend_out_of_date());
    }

    #[test]
    fn contract_resume_response_parity() {
        let v = serde_json::json!({ "session_id": "r", "info": { "desktop_contract": 2 } });
        let r: SessionResumeResponse = serde_json::from_value(v).unwrap();
        assert_eq!(r.backend_contract(), Some(2));
        assert!(!r.backend_out_of_date());

        let v2 = serde_json::json!({ "session_id": "r" });
        let r2: SessionResumeResponse = serde_json::from_value(v2).unwrap();
        assert_eq!(r2.backend_contract(), None);
        assert!(r2.backend_out_of_date());
    }

    // ---- GatewayEvent helpers ----

    #[test]
    fn event_requires_session_id_for_subagent() {
        let ev = GatewayEvent {
            event_type: "subagent.started".into(),
            session_id: None,
            payload: None,
        };
        assert!(ev.requires_session_id());
    }

    #[test]
    fn event_does_not_require_session_id_for_message() {
        let ev = GatewayEvent {
            event_type: "message.delta".into(),
            session_id: None,
            payload: None,
        };
        assert!(!ev.requires_session_id());
    }

    // ---- URL helper ----

    #[test]
    fn gateway_url_construction() {
        let url = gateway_url("10.77.68.94", 8645, "test-token-123");
        let parsed = Url::parse(&url).unwrap();
        assert_eq!(parsed.scheme(), "ws");
        assert_eq!(parsed.host_str(), Some("10.77.68.94"));
        assert_eq!(parsed.port(), Some(8645));
        assert_eq!(parsed.path(), "/api/ws");
        assert_eq!(parsed.query(), Some("token=test-token-123"));
    }

    // ---- reject_all_pending ----

    #[test]
    fn reject_all_pending_clears_map() {
        let mut pending = HashMap::new();
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        pending.insert(1u64, tx1);
        pending.insert(2u64, tx2);

        reject_all_pending(&mut pending);

        assert!(pending.is_empty());
        assert!(rx1.try_recv().unwrap().is_err());
        assert!(rx2.try_recv().unwrap().is_err());
    }

    // ---- Live integration (ignored by default, run with --ignored) ----

    /// Live integration test against a running Hermes dashboard.
    ///
    /// The dashboard serves `/api/ws` on a dynamic port. Set env vars:
    ///   HERMES_DASHBOARD_PORT — the port (find via `lsof -p <pid> -i -sTCP:LISTEN`)
    ///   HERMES_DASHBOARD_TOKEN — the session token (scrape from the dashboard
    ///     index page: `curl -s http://127.0.0.1:<port>/ | grep __HERMES_SESSION_TOKEN__`)
    ///
    /// Run: `HERMES_DASHBOARD_PORT=60142 HERMES_DASHBOARD_TOKEN=<tok> cargo test -p awili-gateway -- --ignored`
    #[test]
    #[ignore]
    fn live_connect_and_session_create() {
        let port = std::env::var("HERMES_DASHBOARD_PORT")
            .expect("set HERMES_DASHBOARD_PORT to the dashboard's listening port");
        let token = std::env::var("HERMES_DASHBOARD_TOKEN")
            .expect("set HERMES_DASHBOARD_TOKEN (from window.__HERMES_SESSION_TOKEN__)");

        let url = gateway_url("127.0.0.1", port.parse().unwrap(), &token);
        eprintln!("connecting to {url}");
        let mut client = GatewayClient::connect(&url).expect("connect failed");
        assert!(client.is_connected());

        let resp = client
            .session_create("/tmp")
            .expect("session.create failed");
        assert!(!resp.session_id.is_empty());
        eprintln!("session created: {}", resp.session_id);

        // Clean up.
        let _ = client.session_close(&resp.session_id);
        client.close();
    }

    // ---- unit 26: single-use WS ticket minting ----

    #[test]
    fn ws_ticket_url_exact_format() {
        assert_eq!(
            ws_ticket_url("h.example", 8645, "TKT-1"),
            "ws://h.example:8645/api/ws?ticket=TKT-1"
        );
    }

    #[test]
    fn ws_ticket_url_uses_ticket_not_token() {
        let url = ws_ticket_url("h.example", 8645, "abc");
        assert!(
            url.contains("?ticket="),
            "expected ?ticket= param, got: {url}"
        );
        assert!(
            !url.contains("?token="),
            "must not contain ?token=, got: {url}"
        );
    }

    #[test]
    fn parse_ws_ticket_ok() {
        let got = parse_ws_ticket(200, r#"{"ticket":"abc123"}"#).unwrap();
        assert_eq!(got, "abc123");
    }

    #[test]
    fn parse_ws_ticket_ok_tolerates_extra_fields() {
        let got = parse_ws_ticket(
            200,
            r#"{"ticket":"xyz","expires_in":300,"kind":"single_use"}"#,
        )
        .unwrap();
        assert_eq!(got, "xyz");
    }

    #[test]
    fn parse_ws_ticket_201_created_ok() {
        let got = parse_ws_ticket(201, r#"{"ticket":"abc-201"}"#).unwrap();
        assert_eq!(got, "abc-201");
    }

    #[test]
    fn parse_ws_ticket_empty_ticket_is_reauth() {
        let err = parse_ws_ticket(200, r#"{"ticket":""}"#).unwrap_err();
        assert!(matches!(err, GatewayError::ReauthRequired));
    }

    #[test]
    fn parse_ws_ticket_missing_field_is_decode() {
        let err = parse_ws_ticket(200, r#"{"session":"ok"}"#).unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn parse_ws_ticket_unparseable_is_decode() {
        let err = parse_ws_ticket(200, "not json").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn parse_ws_ticket_401_is_reauth() {
        let err = parse_ws_ticket(401, r#"{"error":"session expired"}"#).unwrap_err();
        assert!(matches!(err, GatewayError::ReauthRequired));
    }

    #[test]
    fn parse_ws_ticket_403_is_reauth() {
        let err = parse_ws_ticket(403, "").unwrap_err();
        assert!(matches!(err, GatewayError::ReauthRequired));
    }

    #[test]
    fn parse_ws_ticket_500_is_http() {
        let err = parse_ws_ticket(500, "boom").unwrap_err();
        match err {
            GatewayError::Http { status, body } => {
                assert_eq!(status, 500);
                assert_eq!(body, "boom");
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[test]
    fn is_reauth_required_predicate() {
        assert!(GatewayError::ReauthRequired.is_reauth_required());
        assert!(!GatewayError::NotConnected.is_reauth_required());
        assert!(!GatewayError::Decode("x".into()).is_reauth_required());
        assert!(!GatewayError::Http {
            status: 401,
            body: "".into()
        }
        .is_reauth_required());
    }
}
