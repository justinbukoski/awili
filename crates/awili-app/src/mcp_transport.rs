//! C2-u2 MCP transport (ported from the July prior art at `1d09777^`,
//! Codex-reviewed then; re-gated for the engine pivot): launch specs, the
//! origin-keyed server catalog, one connected session, and the rmcp
//! call/result mapping.
//!
//! Every Tokio and rmcp type in this module stays on the MCP runtime OS
//! thread (`mcp_runtime.rs`); none crosses the `awili_tools::runtime`
//! boundary (INV-C2-1 — enforced crate-level: the boundary vocabulary
//! lives in `awili-tools`, which cannot name tokio/rmcp).
//!
//! Safety guards carried from the reviewed prior art:
//! - **Trusted static config only.** A [`StdioServerSpec`] is an absolute
//!   executable plus literal argv and literal env pairs. No shell, no
//!   interpolation, nothing model-controlled ever reaches a launch.
//! - **Finite connect + call deadlines.** A stalled server cannot pin the
//!   runtime thread.
//! - **Child always reaped.** Connect failure, call failure, timeout,
//!   session drop — the child is killed and awaited to reaping.
//! - **Fail closed, never panic.** Failures become terminals with honest
//!   certainty (see [`DispatchVerdict`]), never an unwind, never a lost
//!   turn.
//!
//! New since July (Codex design review 2026-08-13):
//! - Sessions are **persistent per origin** and reused across calls
//!   (July connected per call). A failed session is torn down, reported
//!   via `ServerDied`, and lazily reconnected on next use — the lifecycle
//!   u3's restart/root-epoch semantics extend without replacement.
//! - Outcome **certainty is explicit**: an error after the request may
//!   have reached the server is `Uncertain`, never dressed up as a
//!   definite error (contract §5.1, honest-uncertainty rule).
//! - **Bounded payloads**: result content and advertised tool schemas are
//!   size-capped; deadlines alone do not prevent memory exhaustion.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use awili_attest_types::{AttestationClass, LaunchToken};
use awili_engine::generation::ConnectionGeneration;
use awili_engine::{AwiliCallId, ToolResultRecord};
use awili_tools::runtime::DeathCause;
use awili_tools::runtime::{
    classify_advertised, AdvertisedTool, CallDisposition, SideEffectSignals,
};
use awili_tools::ToolOrigin;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::ServiceExt;
use tokio::process::Command;

/// Connect deadline: a server that never completes MCP initialize is abandoned.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-call deadline: a server that never answers is abandoned and reaped.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Result content cap. Truncation appends an explicit marker; it never
/// changes `is_error` or certainty.
pub const MAX_RESULT_CONTENT_BYTES: usize = 512 * 1024;
/// Advertised-tool bounds: a server may advertise at most this many tools,
/// each with a canonical schema at most this large. Over-limit TOOLS are
/// dropped (and counted) — a truncated schema would digest to a different
/// identity, so schemas are never truncated.
pub const MAX_ADVERTISED_TOOLS: usize = 256;
pub const MAX_TOOL_SCHEMA_BYTES: usize = 64 * 1024;

/// A configured stdio launch spec was invalid.
#[derive(Debug, PartialEq, Eq)]
pub enum SpecError {
    /// The executable path is not absolute. Trusted-static config only: a
    /// relative program would resolve against ambient CWD — exactly the
    /// non-deterministic, potentially-attacker-influenced launch we forbid.
    ProgramNotAbsolute(PathBuf),
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecError::ProgramNotAbsolute(p) => {
                write!(f, "MCP server program must be an absolute path, got {p:?}")
            }
        }
    }
}

impl std::error::Error for SpecError {}

/// How to launch one MCP server over stdio. **Trusted static configuration
/// only** — absolute program, literal argv, literal env pairs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdioServerSpec {
    program: PathBuf,
    args: Vec<String>,
    /// Literal environment pairs set on the child (e.g. a fault selector
    /// for the test server). Never inherited from model-visible state.
    envs: Vec<(String, String)>,
}

/// The minimal platform environment an MCP child is allowed to inherit (S6(a),
/// review F0179). Everything else — including any bearer token or other
/// connector credential present in the parent's environment — is stripped by
/// `env_clear()` before these are added. A server that needs a credential must
/// receive it through its declared `spec.envs`, never through ambient state.
///
/// - `PATH` — required to locate child processes the server may spawn; kept
///   only when the parent has one (an empty PATH is a broken environment, not
///   something we should fabricate).
/// - `HOME` — many servers resolve config/state under `$HOME`; kept only when
///   set.
pub(crate) const PLATFORM_ENV_VARS: &[&str] = &["PATH", "HOME"];

impl StdioServerSpec {
    /// The complete environment for a child launched from this spec (S6(a)):
    /// the documented minimal platform variables, then ONLY the declared
    /// `envs`. Nothing else is inherited.
    pub(crate) fn child_environment(&self) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = PLATFORM_ENV_VARS
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();
        env.extend(self.envs.iter().cloned());
        env
    }
}

/// S6(a): the ONE place a launch command gets its argv and environment.
/// The child gets NO inherited environment — only the documented minimal
/// platform variables plus this server's declared envs. An ambient bearer
/// token or unrelated connector variable in the parent must not reach the
/// child (review F0179).
pub(crate) fn apply_child_launch(cmd: &mut Command, spec: &StdioServerSpec) {
    cmd.args(&spec.args);
    cmd.env_clear();
    for (k, v) in spec.child_environment() {
        cmd.env(k, v);
    }
}

impl StdioServerSpec {
    /// Build a spec, rejecting a non-absolute program (fail closed at
    /// config time — no launch is ever attempted for an invalid spec).
    pub fn new(
        program: impl Into<PathBuf>,
        args: Vec<String>,
        envs: Vec<(String, String)>,
    ) -> Result<Self, SpecError> {
        let program = program.into();
        if !program.is_absolute() {
            return Err(SpecError::ProgramNotAbsolute(program));
        }
        Ok(Self {
            program,
            args,
            envs,
        })
    }
}

/// Maps a [`ToolOrigin`] to its transport spec. Keyed by the full origin
/// string; unknown origin fails closed (no launch, definite error).
#[derive(Clone, Default)]
pub struct McpServerCatalog {
    stdio: HashMap<String, StdioServerSpec>,
}

impl McpServerCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a stdio server under its origin. Chainable.
    pub fn with_stdio(mut self, origin: ToolOrigin, spec: StdioServerSpec) -> Self {
        self.stdio.insert(origin.origin, spec);
        self
    }

    pub(crate) fn resolve(&self, origin: &ToolOrigin) -> Option<&StdioServerSpec> {
        self.stdio.get(&origin.origin)
    }

    /// Every catalog origin key, for the runtime's construction-time
    /// `OriginDisconnected` emission (unit 4b, design r9 §1).
    pub(crate) fn origins(&self) -> impl Iterator<Item = &String> {
        self.stdio.keys()
    }
}

/// Build a launch catalog from the `[mcp]` config section.
///
/// Returns the catalog PLUS one human-readable problem per rejected server.
/// A bad entry is skipped, never fatal: one mistyped path should not stop Awili
/// from starting, but it must not vanish silently either — the caller surfaces
/// these the same way it surfaces a config parse error.
///
/// Disabled servers are skipped without comment; that is a choice, not a fault.
pub fn catalog_from_config(cfg: &awili_config::McpConfig) -> (McpServerCatalog, Vec<String>) {
    let mut catalog = McpServerCatalog::new();
    let mut problems = Vec::new();
    let mut seen: HashMap<String, ()> = HashMap::new();

    for server in cfg.active() {
        let origin = server.origin();

        let envs: Vec<(String, String)> = server
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Validate BEFORE reserving the origin. Reserving first let an
        // invalid entry consume the name, so a later VALID server sharing it
        // was misreported as a duplicate and never launched — the one case
        // where a bad entry did stop a good one (Codex P2, 2026-08-13).
        let spec = match StdioServerSpec::new(server.command.clone(), server.args.clone(), envs) {
            Ok(spec) => spec,
            Err(e) => {
                problems.push(format!("mcp server {:?}: {e}", server.name));
                continue;
            }
        };

        // Duplicate origins would silently last-wins into the catalog map,
        // launching one server while the config appears to declare two.
        if seen.insert(origin.clone(), ()).is_some() {
            problems.push(format!(
                "mcp server {:?}: duplicate name — each server needs a unique name \
                 (origin {origin} was already declared)",
                server.name
            ));
            continue;
        }

        catalog = catalog.with_stdio(awili_tools::ToolOrigin::new(origin), spec);
    }

    (catalog, problems)
}

/// The certainty-honest outcome of one dispatch attempt (runtime-thread
/// internal; the loop pairs it with the caller's ticket).
pub(crate) struct DispatchVerdict {
    pub disposition: CallDisposition,
    pub result: ToolResultRecord,
}

impl DispatchVerdict {
    /// The server ANSWERED — a result or a server-reported error. The
    /// outcome is known either way (disposition `Completed`).
    fn completed(awili_call_id: AwiliCallId, content: String, is_error: bool) -> Self {
        Self {
            disposition: CallDisposition::Completed,
            result: ToolResultRecord {
                awili_call_id,
                content: bound_content(content),
                is_error,
            },
        }
    }

    /// A call refused before anything was sent: unknown origin, connect
    /// failure, non-object arguments, shutdown pre-commit. Definite.
    pub(crate) fn never_dispatched(awili_call_id: AwiliCallId, detail: &str) -> Self {
        Self {
            disposition: CallDisposition::NeverDispatched,
            result: ToolResultRecord {
                awili_call_id,
                content: bound_content(detail.to_string()),
                is_error: true,
            },
        }
    }

    fn uncertain(awili_call_id: AwiliCallId, detail: String) -> Self {
        Self {
            disposition: CallDisposition::InFlightUnresolved,
            result: ToolResultRecord {
                awili_call_id,
                content: bound_content(detail),
                is_error: true,
            },
        }
    }
}

/// Truncation marker. Space for it is RESERVED inside the cap — the
/// bounded value, marker included, never exceeds `MAX_RESULT_CONTENT_BYTES`
/// (Codex C2-u2 round 1, blocker 2).
const TRUNCATION_MARKER: &str = "\n[awili: result truncated at 512KiB]";

pub(crate) fn bound_content(mut s: String) -> String {
    if s.len() > MAX_RESULT_CONTENT_BYTES {
        // Reserve marker space, then truncate on a char boundary.
        let mut cut = MAX_RESULT_CONTENT_BYTES - TRUNCATION_MARKER.len();
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str(TRUNCATION_MARKER);
        debug_assert!(s.len() <= MAX_RESULT_CONTENT_BYTES);
    }
    s
}

// The wrapper serializes every signal and reap against forced teardown. The
// child remains unreaped while its numeric process-group identity is in use.
#[cfg(unix)]
#[derive(Clone, Debug, Default)]
struct OwnedMcpGroup(std::sync::Arc<std::sync::Mutex<Option<u32>>>);

#[cfg(unix)]
impl OwnedMcpGroup {
    fn kill(&self) {
        let guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pid) = *guard {
            crate::code_provider::kill_process_group(pid as i32);
        }
    }
}

#[cfg(unix)]
impl process_wrap::tokio::CommandWrapper for OwnedMcpGroup {
    fn pre_spawn(
        &mut self,
        command: &mut tokio::process::Command,
        _: &process_wrap::tokio::CommandWrap,
    ) -> std::io::Result<()> {
        command.process_group(0);
        Ok(())
    }
    fn wrap_child(
        &mut self,
        inner: Box<dyn process_wrap::tokio::ChildWrapper>,
        _: &process_wrap::tokio::CommandWrap,
    ) -> std::io::Result<Box<dyn process_wrap::tokio::ChildWrapper>> {
        let pid = inner
            .id()
            .ok_or_else(|| std::io::Error::other("MCP child has no pid"))?;
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(pid);
        Ok(Box::new(OwnedMcpChild {
            inner: Some(inner),
            group: self.clone(),
        }))
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct OwnedMcpChild {
    inner: Option<Box<dyn process_wrap::tokio::ChildWrapper>>,
    group: OwnedMcpGroup,
}

#[cfg(unix)]
impl process_wrap::tokio::ChildWrapper for OwnedMcpChild {
    fn inner(&self) -> &dyn process_wrap::tokio::ChildWrapper {
        self.inner.as_deref().unwrap().inner()
    }
    fn inner_mut(&mut self) -> &mut dyn process_wrap::tokio::ChildWrapper {
        self.inner.as_deref_mut().unwrap().inner_mut()
    }
    fn into_inner(mut self: Box<Self>) -> Box<dyn process_wrap::tokio::ChildWrapper> {
        self.group.kill();
        *self.group.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.inner.take().unwrap()
    }
    fn start_kill(&mut self) -> std::io::Result<()> {
        self.group.kill();
        Ok(())
    }
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let mut guard = self.group.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pid) = *guard {
            match crate::proc_compat::child_exit_observed(pid) {
                Ok(false) => return Ok(None),
                Ok(true) => {}
                Err(error) => {
                    // Identity is no longer proven; never signal this id again.
                    *guard = None;
                    return Err(error);
                }
            }
            crate::code_provider::kill_process_group(pid as i32);
            *guard = None;
        }
        self.inner.as_deref_mut().unwrap().try_wait()
    }
    fn wait(
        &mut self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::io::Result<std::process::ExitStatus>> + Send + '_,
        >,
    > {
        Box::pin(async move {
            loop {
                if let Some(status) = self.try_wait()? {
                    return Ok(status);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
    }
}

#[cfg(unix)]
impl Drop for OwnedMcpChild {
    fn drop(&mut self) {
        if self.inner.is_some() {
            let mut guard = self.group.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pid) = guard.take() {
                crate::code_provider::kill_process_group(pid as i32);
            }
        }
    }
}

/// One connected MCP session: the rmcp client plus the child pid for
/// reap verification. Lives on the runtime thread, inside an `Rc` shared
/// between concurrent calls (rmcp's client multiplexes requests).
pub(crate) struct Session {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    child_pid: Option<u32>,
    #[cfg(unix)]
    owned_group: OwnedMcpGroup,
}

/// A successful connect (unit 4b, design r9 §2.4): session + advertisement
/// facts + the SELECTED launch token, moved out to the runtime's one emit
/// site. `Session::connect` never touches the event sink.
pub(crate) struct ConnectSuccess {
    pub session: Session,
    pub claimed_server: String,
    pub tools: Vec<AdvertisedTool>,
    pub dropped_at_capture: Vec<String>,
    pub advertised_epoch: Option<u64>,
    pub launch: LaunchToken,
}

/// A typed connect failure (design r9 §2.4, revision 6): every Phase-B
/// failure carries `Some(class)` — enforced structurally by the ONE
/// mapping site in `connect`, since `PhaseBFailure` has no class field.
#[derive(Debug)]
pub(crate) struct ConnectFailure {
    pub cause: DeathCause,
    pub selected_class: Option<AttestationClass>,
    pub detail: String,
}

/// Phase B's error: cause + detail, NO class field — the class is wrapped
/// in exactly once, in `connect`'s single `map_err`.
struct PhaseBFailure {
    cause: DeathCause,
    detail: String,
}

impl Session {
    /// Launch + Phase A token selection + MCP initialize + tools/list, all
    /// under deadlines (design r9 §2.4). Phase A runs BETWEEN spawn and
    /// `serve` — no MCP byte is written before the token is selected, and
    /// an attestation failure kills and reaps the child without ever
    /// handshaking. Phase B is one common path for both classes.
    ///
    /// Failure here is DEFINITE: no tool call was sent.
    pub(crate) async fn connect(
        spec: &StdioServerSpec,
        origin: &awili_tools::ToolOrigin,
        generation: ConnectionGeneration,
        record: Option<&awili_attest::InstalledSetupRecord>,
        connect_timeout: Duration,
        call_timeout: Duration,
    ) -> Result<ConnectSuccess, ConnectFailure> {
        // Windows: never a bare name (searched for in more places than anyone
        // intends): resolved to an absolute program or a plain "not found".
        let program =
            crate::agent_bin::resolve_program(spec.program.as_ref()).map_err(|detail| {
                ConnectFailure {
                    cause: DeathCause::LaunchFailed,
                    selected_class: None,
                    detail: format!("failed to launch MCP server process: {detail}"),
                }
            })?;
        let command = Command::new(&program).configure(|cmd| apply_child_launch(cmd, spec));
        #[allow(unused_mut)]
        let mut command = process_wrap::tokio::CommandWrap::from(command);
        // Windows: created suspended, put in a Job Object by its own handle, then
        // resumed, so a forced stop ends the server's whole tree (no-op elsewhere).
        #[cfg(windows)]
        command.wrap(crate::winjob::TrackedJob);
        #[cfg(unix)]
        let owned_group = OwnedMcpGroup::default();
        #[cfg(unix)]
        command.wrap(owned_group.clone());
        let child = TokioChildProcess::new(command).map_err(|err| ConnectFailure {
            cause: DeathCause::LaunchFailed,
            selected_class: None,
            detail: format!("failed to launch MCP server process: {err}"),
        })?;
        let child_pid = child.id();

        // ---- Phase A: select the token. No MCP byte has been written. ----
        let spawn_program = spec.program.to_string_lossy();
        let launch = match awili_attest::select_token(
            record,
            origin,
            generation,
            spawn_program.as_ref(),
            child_pid,
        ) {
            awili_attest::TokenSelection::Selected(launch) => launch,
            awili_attest::TokenSelection::AttestationFailed(failure) => {
                // Kill + reap: NO handshake with a process that failed
                // attestation (row 5).
                drop(child);
                reap_after_failure(child_pid).await;
                return Err(ConnectFailure {
                    cause: DeathCause::AttestationFailed {
                        pinned_requirement: failure.pinned_requirement,
                    },
                    selected_class: None,
                    detail: failure.detail,
                });
            }
        };
        // The Copy class, retained BEFORE the token moves anywhere: the
        // one Phase-B mapping site below wraps every failure with it.
        let class = launch.class();

        // ---- Phase B: one common handshake path for both classes. ----
        let phase_b = Self::phase_b(child, child_pid, connect_timeout, call_timeout).await;
        let (client, claimed_server, tools, dropped_at_capture, advertised_epoch) = phase_b
            .map_err(|f: PhaseBFailure| ConnectFailure {
                cause: f.cause,
                selected_class: Some(class),
                detail: f.detail,
            })?;

        Ok(ConnectSuccess {
            session: Session {
                client,
                child_pid,
                #[cfg(unix)]
                owned_group,
            },
            claimed_server,
            tools,
            dropped_at_capture,
            advertised_epoch,
            launch,
        })
    }

    async fn phase_b(
        child: TokioChildProcess,
        child_pid: Option<u32>,
        connect_timeout: Duration,
        call_timeout: Duration,
    ) -> Result<
        (
            rmcp::service::RunningService<rmcp::RoleClient, ()>,
            String,
            Vec<AdvertisedTool>,
            Vec<String>,
            Option<u64>,
        ),
        PhaseBFailure,
    > {
        let client = match tokio::time::timeout(connect_timeout, ().serve(child)).await {
            Ok(Ok(client)) => client,
            Ok(Err(err)) => {
                reap_after_failure(child_pid).await;
                return Err(PhaseBFailure {
                    cause: DeathCause::HandshakeFailed,
                    detail: format!("MCP connect/initialize failed: {err}"),
                });
            }
            Err(_) => {
                reap_after_failure(child_pid).await;
                return Err(PhaseBFailure {
                    cause: DeathCause::HandshakeFailed,
                    detail: format!("MCP connect timed out after {}s", connect_timeout.as_secs()),
                });
            }
        };

        // tools/list under the call deadline. A server that initializes
        // but cannot list tools is not usable — tear it down.
        let listed = match tokio::time::timeout(call_timeout, client.list_tools(None)).await {
            Ok(Ok(listed)) => listed,
            Ok(Err(err)) => {
                let pid = child_pid;
                let _ = client.cancel().await;
                reap_after_failure(pid).await;
                return Err(PhaseBFailure {
                    cause: DeathCause::HandshakeFailed,
                    detail: format!("MCP tools/list failed: {err}"),
                });
            }
            Err(_) => {
                let pid = child_pid;
                let _ = client.cancel().await;
                reap_after_failure(pid).await;
                return Err(PhaseBFailure {
                    cause: DeathCause::HandshakeFailed,
                    detail: format!("MCP tools/list timed out after {}s", call_timeout.as_secs()),
                });
            }
        };

        let (tools, dropped) = bound_advertised(listed.tools);

        // The server's claimed identity: `server_info.name`, REQUIRED
        // present and non-empty (design r9 §2.4 Phase B) — an anonymous
        // server cannot satisfy the four-way protocol-name comparisons.
        let claimed_server = client
            .peer_info()
            .map(|info| info.server_info.name.to_string())
            .unwrap_or_default();
        if claimed_server.trim().is_empty() {
            let pid = child_pid;
            let _ = client.cancel().await;
            reap_after_failure(pid).await;
            return Err(PhaseBFailure {
                cause: DeathCause::HandshakeFailed,
                detail: "server_info.name absent or empty; refusing an anonymous server".into(),
            });
        }

        let roots_epoch = advertised_roots_epoch(
            client
                .peer_info()
                .and_then(|info| info.meta.clone())
                .as_ref()
                .map(|meta| meta.get("awili/rootsEpoch")),
        );

        Ok((client, claimed_server, tools, dropped, roots_epoch))
    }

    /// Invoke one tool under the call deadline, mapping the outcome to an
    /// honest [`DispatchVerdict`]:
    ///
    /// - server answered with a result → `Definite` (ok or tool error);
    /// - server answered with a JSON-RPC error → `Definite` error (the
    ///   server processed and refused — it told us so);
    /// - transport failure or timeout after the request may have been
    ///   written → `Uncertain`;
    /// - non-object argument JSON → `Definite` error, nothing sent.
    ///
    /// Returns `(verdict, session_broken)`; a broken session must be torn
    /// down by the caller (ServerDied + lazy reconnect).
    pub(crate) async fn call(
        &self,
        awili_call_id: AwiliCallId,
        tool_name: &str,
        arguments_json: &str,
        call_timeout: Duration,
    ) -> (DispatchVerdict, bool) {
        // The engine validated the arguments well-formed at assembly time;
        // parse to the object map rmcp requires. A non-object cannot be an
        // MCP argument set — definite, nothing sent.
        let arguments = match serde_json::from_str::<serde_json::Value>(arguments_json) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            Ok(serde_json::Value::Null) => None,
            Ok(_) => {
                return (
                    DispatchVerdict::never_dispatched(
                        awili_call_id,
                        "MCP arguments are not a JSON object; failing closed.",
                    ),
                    false,
                );
            }
            Err(err) => {
                return (
                    DispatchVerdict::never_dispatched(
                        awili_call_id,
                        &format!("MCP arguments failed to parse: {err}; failing closed."),
                    ),
                    false,
                );
            }
        };

        let mut params = CallToolRequestParams::new(tool_name.to_string());
        if let Some(map) = arguments {
            params = params.with_arguments(map);
        }

        match tokio::time::timeout(call_timeout, self.client.call_tool(params)).await {
            Ok(Ok(result)) => (result_verdict(awili_call_id, result), false),
            Ok(Err(err)) => match &err {
                // The server processed the request and answered with a
                // JSON-RPC error: definite — it told us nothing ran.
                rmcp::service::ServiceError::McpError(e) => (
                    // The server processed and refused — it ANSWERED.
                    DispatchVerdict::completed(
                        awili_call_id,
                        format!("MCP server refused the call: {e}"),
                        true,
                    ),
                    false,
                ),
                // Anything else (torn transport, closed channel, codec
                // garbage): the request may have reached the server.
                other => (
                    DispatchVerdict::uncertain(
                        awili_call_id,
                        format!("MCP transport failed mid-call: {other}"),
                    ),
                    true,
                ),
            },
            Err(_) => (
                DispatchVerdict::uncertain(
                    awili_call_id,
                    format!(
                        "MCP tool call timed out after {}s; outcome unknown",
                        call_timeout.as_secs()
                    ),
                ),
                true,
            ),
        }
    }

    /// Graceful teardown: cancel the client (shutdown handshake) and await
    /// the child to reaping. Bounded — a wedged reap cannot pin the thread.
    pub(crate) async fn teardown(self) {
        let pid = self.child_pid;
        let _ = self.client.cancel().await;
        reap_after_failure(pid).await;
    }

    /// Forced teardown for a session still SHARED with an in-flight task
    /// (mid-run transport failure): the graceful `cancel(self)` needs
    /// ownership we cannot take, so kill the child directly and await the
    /// reap. The dying transport errors the remaining calls out (their
    /// tasks then drop their `Rc` clones), and rmcp's drop-kill finds the
    /// pid already gone. Child-always-reaped stays unconditional.
    pub(crate) async fn force_kill(&self) {
        if let Some(pid) = self.child_pid {
            #[cfg(unix)]
            self.owned_group.kill();
            #[cfg(not(unix))]
            crate::proc_compat::kill_pid_reporting(pid);
            reap_after_failure(Some(pid)).await;
        }
    }
}

/// Bound + classify a server's advertisement: at most
/// [`MAX_ADVERTISED_TOOLS`] tools, each with a canonical schema of at most
/// [`MAX_TOOL_SCHEMA_BYTES`]. Over-limit tools drop WHOLE and are counted —
/// a truncated schema would digest to a different identity.
pub(crate) fn bound_advertised(
    advertised: Vec<rmcp::model::Tool>,
) -> (Vec<AdvertisedTool>, Vec<String>) {
    let mut tools = Vec::new();
    // Dropped tool NAMES, not a count (unit 4b commit 1: duplicate
    // detection over kept ++ dropped needs the names; design r9 §3).
    let mut dropped: Vec<String> = Vec::new();
    for tool in advertised {
        if tools.len() >= MAX_ADVERTISED_TOOLS {
            dropped.push(tool.name.to_string());
            continue;
        }
        let schema_value = serde_json::Value::Object((*tool.input_schema).clone());
        let canonical = awili_tools::canonical_json(&schema_value);
        if canonical.len() > MAX_TOOL_SCHEMA_BYTES {
            dropped.push(tool.name.to_string());
            continue;
        }
        let signals = side_effect_signals(&tool);
        tools.push(AdvertisedTool {
            name: tool.name.to_string(),
            arg_schema_canonical: String::from_utf8_lossy(&canonical).into_owned(),
            risk: classify_advertised(signals),
            description: awili_tools::runtime::bounded_description(
                tool.description.as_deref().unwrap_or(""),
            ),
        });
    }
    (tools, dropped)
}

/// The server's roots-epoch proposal, from its initialize `_meta`
/// (`entry` = `None` when there is no `_meta` at all, else the
/// `"awili/rootsEpoch"` value if any).
///
/// - A JSON number: that epoch.
/// - RC1-M-FIX1 (owner design principle, 2026-10-03 — the in-app engine gets
///   the same MCP servers his Claude Code has): NO `"awili/rootsEpoch"` key
///   at all is a standard MCP server that takes no part in Awili's roots
///   handshake — the owner's interlock memory server, Awili's own connector
///   server (the RC1-I3 "advertises no roots epoch, RC2" gap). It is bound
///   at [`ROOTLESS_EPOCH`], the epoch at which nothing has been published,
///   instead of being refused forever. Its calls still go through Awili's
///   authorization: read-only tools by its own annotations, everything else
///   a card in "Ask before changes".
/// - A key whose value is not a non-negative integer (string, negative,
///   float, null) is a malformed proposal: `None`, never coerced.
pub(crate) fn advertised_roots_epoch(entry: Option<Option<&serde_json::Value>>) -> Option<u64> {
    match entry {
        None | Some(None) => Some(ROOTLESS_EPOCH),
        Some(Some(v)) => v.as_u64(),
    }
}

/// RC1-M-FIX1: the epoch a server that takes no part in the roots handshake
/// is bound at (see [`advertised_roots_epoch`]).
pub(crate) const ROOTLESS_EPOCH: u64 = 0;

/// Extract side-effect signals from an advertised rmcp tool: MCP 2025-03-26
/// `annotations` plus Awili's namespaced `_meta["awili/sideEffect"]`.
/// Absent or malformed pieces stay `None` / present-but-malformed, and
/// `classify_advertised` fails them closed.
fn side_effect_signals(tool: &rmcp::model::Tool) -> SideEffectSignals {
    let (read_only_hint, destructive_hint) = match &tool.annotations {
        Some(a) => (a.read_only_hint, a.destructive_hint),
        None => (None, None),
    };
    let awili_side_effect = tool
        .meta
        .as_ref()
        .and_then(|m| m.get("awili/sideEffect"))
        .map(|v| {
            v.as_str().and_then(|s| match s {
                "read_only" => Some(awili_engine::RiskClass::ReadOnly),
                "mutating" => Some(awili_engine::RiskClass::Mutating),
                "irreversible" => Some(awili_engine::RiskClass::Irreversible),
                "privileged" => Some(awili_engine::RiskClass::Privileged),
                _ => None, // present-but-malformed → classify_advertised fails closed
            })
        });
    SideEffectSignals {
        read_only_hint,
        destructive_hint,
        awili_side_effect,
    }
}

/// Map a successful rmcp [`CallToolResult`] to a definite verdict. Text
/// content blocks are concatenated; an MCP-level `isError` becomes an
/// error result. Definite either way: the server answered.
fn result_verdict(awili_call_id: AwiliCallId, result: CallToolResult) -> DispatchVerdict {
    let text = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    DispatchVerdict::completed(awili_call_id, text, result.is_error.unwrap_or(false))
}

/// Wait (bounded, ~2 s) for a failed connection's child to EXIT, after a
/// failure that may leave rmcp's drop-spawned kill task undriven (ported
/// from the July prior art).
///
/// RC2-APP (F0175): it waits for the exit itself, not for the pid to
/// vanish. `waitid(WNOWAIT)` sees the exit without reaping — the child's
/// tokio handle still owns the reap (reaping here would let that handle
/// later signal a reused pid). An exited, not-yet-reaped child counts as
/// done at once (the old `kill(pid, 0)` probe saw such a zombie as alive
/// and spent the whole bound), and so does a child already reaped by its
/// owner. Never blocks the runtime thread past the bound.
pub(crate) async fn reap_after_failure(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    for _ in 0..100 {
        if crate::proc_compat::child_has_exited(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
mod tests {

    #[cfg(unix)]
    #[tokio::test]
    async fn owned_mcp_group_force_and_reap_end_helper_without_stale_signal() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("helper-ran");
        let ready = dir.path().join("ready");
        let script = format!(
            "(sleep 1; touch {}) & echo ready > {}; wait",
            marker.display(),
            ready.display()
        );
        let group = super::OwnedMcpGroup::default();
        let mut command = process_wrap::tokio::CommandWrap::with_new("/bin/sh", |cmd| {
            cmd.args(["-c", &script]);
        });
        command.wrap(group.clone());
        let mut child = command.spawn().unwrap();
        for _ in 0..100 {
            if ready.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready.exists());
        group.kill();
        tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(
            group.0.lock().unwrap().is_none(),
            "identity cleared before reap"
        );
        group.kill(); // no stale numeric identity remains
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(!marker.exists(), "helper must not survive forced teardown");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn concurrent_mcp_force_and_reap_clear_the_owned_identity() {
        for _ in 0..16 {
            let group = super::OwnedMcpGroup::default();
            let mut command = process_wrap::tokio::CommandWrap::with_new("/bin/sh", |cmd| {
                cmd.args(["-c", "sleep 0.02"]);
            });
            command.wrap(group.clone());
            let mut child = command.spawn().unwrap();
            let force = group.clone();
            let task = tokio::task::spawn_blocking(move || {
                force.kill();
            });
            tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .unwrap()
                .unwrap();
            task.await.unwrap();
            assert!(group.0.lock().unwrap().is_none());
            group.kill();
        }
    }

    /// RC2-APP (F0175): an exited child that nobody has reaped yet ends the
    /// wait at once (the old existence probe saw it as alive for the whole
    /// bound), and the wait never reaps it — its owner still can.
    #[tokio::test]
    async fn rc2app_reap_waits_for_the_exit_and_leaves_the_reap_to_the_owner() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(200)); // it has exited by now
        let started = std::time::Instant::now();
        reap_after_failure(Some(pid)).await;
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "an exited child ends the wait at once: {:?}",
            started.elapsed()
        );
        let status = child.wait().expect("the owner can still reap it");
        assert_eq!(status.code(), Some(7));
        // Already reaped by its owner: also done at once.
        let started = std::time::Instant::now();
        reap_after_failure(Some(pid)).await;
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    /// RC1-M-FIX1: a standard MCP server with no `awili/rootsEpoch` key (no
    /// `_meta` at all, or `_meta` without the key) is bound at the rootless
    /// epoch; a number is that epoch; a present but malformed value is still
    /// no proposal (refused downstream, never coerced).
    #[test]
    fn rc1m_fix1_a_server_without_the_roots_handshake_is_rootless() {
        use serde_json::json;
        assert_eq!(advertised_roots_epoch(None), Some(ROOTLESS_EPOCH));
        assert_eq!(advertised_roots_epoch(Some(None)), Some(ROOTLESS_EPOCH));
        assert_eq!(advertised_roots_epoch(Some(Some(&json!(7)))), Some(7));
        for bad in [json!("7"), json!(-1), json!(1.5), json!(null), json!({})] {
            assert_eq!(advertised_roots_epoch(Some(Some(&bad))), None, "{bad}");
        }
    }

    use super::*;

    #[test]
    fn bound_content_never_exceeds_the_cap_marker_included() {
        // ASCII overflow.
        let big = "x".repeat(MAX_RESULT_CONTENT_BYTES + 100);
        let bounded = bound_content(big);
        assert!(bounded.len() <= MAX_RESULT_CONTENT_BYTES);
        assert!(bounded.ends_with(TRUNCATION_MARKER));
        // Multibyte overflow: the cut must land on a char boundary and
        // still respect the cap.
        let big = "é".repeat(MAX_RESULT_CONTENT_BYTES / 2 + 100);
        let bounded = bound_content(big);
        assert!(bounded.len() <= MAX_RESULT_CONTENT_BYTES);
        assert!(bounded.ends_with(TRUNCATION_MARKER));
        // Under the cap: untouched.
        let small = "hello".to_string();
        assert_eq!(bound_content(small.clone()), small);
        // Exactly at the cap: untouched (no spurious marker).
        let exact = "x".repeat(MAX_RESULT_CONTENT_BYTES);
        assert_eq!(bound_content(exact.clone()), exact);
    }

    fn synthetic_tool(name: &str, schema_bytes: usize) -> rmcp::model::Tool {
        // Pad a valid object schema to approximately the requested size.
        let pad = "p".repeat(schema_bytes);
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"x": {"type": "string", "description": pad}}
        });
        let serde_json::Value::Object(map) = schema else {
            unreachable!()
        };
        rmcp::model::Tool::new(
            name.to_string(),
            "synthetic".to_string(),
            std::sync::Arc::new(map),
        )
    }

    /// RC1-R: the server's own tool description is kept (bounded, without
    /// control characters) so the model knows what the tool does.
    #[test]
    fn rc1r_the_servers_description_is_kept() {
        let mut t = synthetic_tool("write_file", 16);
        t.description = Some(
            "Write a file.\u{7}\nRelative paths are anchored to HOME."
                .to_string()
                .into(),
        );
        let (kept, _) = bound_advertised(vec![t, synthetic_tool("plain", 16)]);
        assert_eq!(
            kept[0].description,
            "Write a file.\nRelative paths are anchored to HOME."
        );
        assert_eq!(kept[1].description, "synthetic");
    }

    #[test]
    fn oversized_schemas_drop_whole_and_are_counted_never_truncated() {
        let tools = vec![
            synthetic_tool("ok", 64),
            synthetic_tool("huge", MAX_TOOL_SCHEMA_BYTES + 1024),
        ];
        let (kept, dropped) = bound_advertised(tools);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "ok");
        assert_eq!(dropped.len(), 1);
        // No metadata on synthetic tools → privileged (fail closed).
        assert_eq!(kept[0].risk, awili_engine::RiskClass::Privileged);
    }

    #[test]
    fn tool_count_bound_drops_the_excess_and_counts_it() {
        let tools: Vec<_> = (0..MAX_ADVERTISED_TOOLS + 5)
            .map(|i| synthetic_tool(&format!("t{i}"), 16))
            .collect();
        let (kept, dropped) = bound_advertised(tools);
        assert_eq!(kept.len(), MAX_ADVERTISED_TOOLS);
        assert_eq!(dropped.len(), 5);
    }

    // -- catalog_from_config (mac-shell slice, unit 3) -----------------------

    fn server(name: &str, command: &str) -> awili_config::McpServerConfig {
        awili_config::McpServerConfig {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            env: Default::default(),
            trust: awili_config::McpTrust::Prompt,
            modes: vec![awili_config::McpMode::Work],
            enabled: true,
        }
    }

    #[test]
    fn config_becomes_a_catalog_keyed_by_mcp_origin() {
        let cfg = awili_config::McpConfig {
            servers: vec![server("mac-shell", "/usr/local/bin/mac-shell-mcp")],
        };
        let (catalog, problems) = catalog_from_config(&cfg);
        assert!(problems.is_empty(), "{problems:?}");
        assert!(catalog
            .resolve(&awili_tools::ToolOrigin::new("mcp:mac-shell"))
            .is_some());
    }

    /// A relative program is rejected — it would resolve against ambient CWD.
    /// The whole app must still start: one bad entry is surfaced, not fatal.
    #[test]
    fn relative_command_is_skipped_with_a_named_problem() {
        let cfg = awili_config::McpConfig {
            servers: vec![
                server("bad", "mac-shell-mcp"),
                server("good", "/usr/local/bin/mac-shell-mcp"),
            ],
        };
        let (catalog, problems) = catalog_from_config(&cfg);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("bad"), "{}", problems[0]);
        assert!(
            catalog
                .resolve(&awili_tools::ToolOrigin::new("mcp:good"))
                .is_some(),
            "one bad entry must not take the good ones down with it"
        );
        assert!(catalog
            .resolve(&awili_tools::ToolOrigin::new("mcp:bad"))
            .is_none());
    }

    /// An invalid entry must not reserve the name for a later valid one.
    /// Reserving before validating turned "one bad entry is skipped" into
    /// "one bad entry silently disables a good server" (Codex P2).
    #[test]
    fn an_invalid_entry_does_not_reserve_the_name_for_a_valid_one() {
        let cfg = awili_config::McpConfig {
            servers: vec![
                server("mac-shell", "mac-shell-mcp"),
                server("mac-shell", "/usr/local/bin/mac-shell-mcp"),
            ],
        };
        let (catalog, problems) = catalog_from_config(&cfg);
        assert_eq!(
            problems.len(),
            1,
            "only the relative path is a problem: {problems:?}"
        );
        assert!(
            !problems[0].contains("duplicate"),
            "the valid server must not be reported as a duplicate: {}",
            problems[0]
        );
        assert!(
            catalog
                .resolve(&awili_tools::ToolOrigin::new("mcp:mac-shell"))
                .is_some(),
            "the valid server must still be launchable"
        );
    }

    /// Two servers sharing a name would last-wins into the catalog map —
    /// launching one server while the config appears to declare two. Refuse
    /// the second and say so.
    #[test]
    fn duplicate_names_are_refused_not_silently_collapsed() {
        let cfg = awili_config::McpConfig {
            servers: vec![
                server("dup", "/opt/a/server"),
                server("dup", "/opt/b/server"),
            ],
        };
        let (_catalog, problems) = catalog_from_config(&cfg);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("duplicate"), "{}", problems[0]);
    }

    #[test]
    fn disabled_servers_are_skipped_without_a_problem() {
        let mut s = server("parked", "/opt/x/server");
        s.enabled = false;
        let (catalog, problems) =
            catalog_from_config(&awili_config::McpConfig { servers: vec![s] });
        assert!(problems.is_empty(), "disabling is a choice, not a fault");
        assert!(catalog
            .resolve(&awili_tools::ToolOrigin::new("mcp:parked"))
            .is_none());
    }

    #[test]
    fn env_pairs_reach_the_spec() {
        let mut s = server("withenv", "/opt/x/server");
        s.env.insert("FOO".into(), "bar".into());
        let (catalog, _) = catalog_from_config(&awili_config::McpConfig { servers: vec![s] });
        let spec = catalog
            .resolve(&awili_tools::ToolOrigin::new("mcp:withenv"))
            .expect("registered");
        assert_eq!(spec.envs, vec![("FOO".to_string(), "bar".to_string())]);
    }

    /// RAII guard: records the parent's current value for each env var it sets
    /// and restores (or removes) it on drop. The test must never touch shared
    /// variables like `PATH` or `HOME` — only these dedicated keys.
    struct EnvGuard {
        keys: Vec<(String, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let prev = std::env::var_os(key);
            std::env::set_var(key, value);
            Self {
                keys: vec![(key.to_string(), prev)],
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, prev) in &self.keys {
                match prev {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// S6(a) proof (review F0179) against a LAUNCHED child, not the helper
    /// vector: the child's real environment is exactly the documented minimal
    /// platform variables plus the declared `spec.envs`. Any credential-looking
    /// variable present in the parent — a bearer token, an unrelated connector
    /// key — must NOT reach the child.
    #[test]
    fn mcp_child_gets_only_declared_environment() {
        // Simulate a parent environment that carries credentials.
        let _guard = EnvGuard::set("AWILI_S6_TEST_PARENT_BEARER", "test-bearer-should-not-leak");
        let _guard = EnvGuard::set(
            "AWILI_S6_TEST_UNRELATED_CONNECTOR_KEY",
            "unrelated-credential",
        );

        let spec = StdioServerSpec::new(
            "/usr/bin/env",
            Vec::new(),
            vec![("DECLARED_VAR".to_string(), "declared-value".to_string())],
        )
        .expect("valid spec");

        let mut cmd = Command::new(&spec.program);
        apply_child_launch(&mut cmd, &spec);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let out = rt
            .block_on(async { cmd.output().await })
            .expect("child runs");
        assert!(out.status.success(), "the launched child must succeed");

        let stdout_text = String::from_utf8_lossy(&out.stdout);
        let mut env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        for line in stdout_text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                env.insert(k.to_string(), v.to_string());
            }
        }

        // Declared envs are present, verbatim.
        assert_eq!(env.get("DECLARED_VAR"), Some(&"declared-value".to_string()));

        // No inherited credential reaches the child.
        assert!(!env.contains_key("AWILI_S6_TEST_PARENT_BEARER"));
        assert!(!env.contains_key("AWILI_S6_TEST_UNRELATED_CONNECTOR_KEY"));

        // Every key is either a documented platform variable or declared:
        // nothing else from the parent can slip through env_clear().
        for key in env.keys() {
            let is_platform = PLATFORM_ENV_VARS.contains(&key.as_str());
            let is_declared = spec.envs.iter().any(|(k, _)| k == key);
            assert!(is_platform || is_declared, "unexpected env var: {key}");
        }

        // The platform variables are carried through exactly when the parent
        // has them — same value, absent otherwise.
        for key in PLATFORM_ENV_VARS {
            match std::env::var(key) {
                Ok(v) => assert_eq!(env.get(*key), Some(&v), "platform var {key}"),
                Err(_) => assert!(
                    !env.contains_key(*key),
                    "platform var {key} must be absent when the parent has none"
                ),
            }
        }
    }
}
