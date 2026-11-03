//! C2-u2: the canonical boundary vocabulary for the MCP runtime thread.
//!
//! This module is the compile-level enforcement of INV-C2-1 (no Tokio
//! escapes): it lives in `awili-tools`, whose Cargo.toml names neither
//! `tokio` nor `rmcp`, so nothing declared here CAN carry a Tokio or rmcp
//! type. The runtime implementation (awili-app, per the triad decision that
//! rmcp is awili-app-only) depends on this crate and constructs an
//! [`McpRuntimeHandle`] via [`McpRuntimeHandle::from_parts`]; engine-side
//! code holds only the handle and the canonical values that flow through it.
//!
//! ## The ticket discipline (Codex design review 2026-08-13, point 2)
//!
//! Commands carry the caller's dispatch ticket by MOVE (the B2
//! `IntentTicket` is deliberately not `Clone`), and every terminal event
//! returns it. Move semantics make a DUPLICATE terminal unrepresentable —
//! but they cannot make a MISSING terminal unrepresentable. The runtime
//! implementation therefore guarantees, structurally:
//!
//! - each ticket is held in the runtime loop's own pending-call state,
//!   never inside a dispatch future, so a panicking dispatch cannot
//!   destroy it — the loop converts the panic into a terminal;
//! - shutdown drains queued-but-undispatched commands into definite error
//!   terminals and in-flight calls into uncertain terminals;
//! - catastrophic runtime-thread death (the loop itself dies) is the ONE
//!   admitted ticket-loss path. That is not silent data loss: B2 replay
//!   classifies an intent with `dispatch_started` and no
//!   `dispatch_finalized` as Uncertain, which is exactly the truth.
//!
//! The type parameter `T` is the ticket type. Production instantiates it
//! with `awili_engine_log::IntentTicket`; tests may use any `Send` type.
//! Keeping it generic spares this pure crate a dependency on the log
//! writer while keeping "the runtime never sees an unticketed call"
//! (contract addendum §2) structural: [`McpCommand::CallTool`] cannot be
//! built without one.

use awili_engine::{RiskClass, ToolCall, ToolResultRecord};

use crate::identity::ToolOrigin;

/// Execution certainty of a terminal outcome (contract §5.1, ported to u2
/// scope per the Codex design review: "don't claim an error terminal for
/// outcomes that are actually Uncertain").
///
/// - `Definite`: the outcome is known. Either the server answered (success
///   or a well-formed tool/protocol error), or the call provably never
///   left this process (unknown origin, launch failure, connect failure,
///   queued at shutdown). Engine-side code calls `dispatch_finalized`.
/// - `Uncertain`: the request may have reached the server and a side
///   effect may have happened (crash after send, timeout, torn transport,
///   dispatch panic). Engine-side code must NOT `dispatch_finalized`; the
///   intent stays open and B2 replay classifies it Uncertain. Per the
///   signed C1 contract, no uncertain intent auto-dispatches again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionCertainty {
    Definite,
    Uncertain,
}

/// What actually happened to a call — the descriptive half of a terminal.
/// Certainty is DERIVED from this ([`Self::certainty`]), never stored
/// separately: an invalid disposition/certainty pair is unconstructable
/// (Codex C2-u3 design review, blocking change 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallDisposition {
    /// The server answered: a successful result OR a server-reported
    /// tool/protocol error. Definite either way — the outcome is known.
    Completed,
    /// Refused before the dispatch commit point: unknown origin, no/stale
    /// accepted root epoch, connect failure, shutdown, runtime-
    /// construction failure. (A DUPLICATE submission is not a terminal at
    /// all — see [`McpEvent::SubmissionRejected`].) The call provably
    /// never ran.
    NeverDispatched,
    /// Cancelled before the commit point (§5.1): definite, no side effect.
    CancelledBeforeDispatch,
    /// The request may have reached the server and its outcome is
    /// unknowable: crash/timeout/torn transport after send, dispatch
    /// panic, cancel or shutdown while in flight. The ONLY Uncertain
    /// disposition — engine-side code must NOT `dispatch_finalized`.
    InFlightUnresolved,
}

impl CallDisposition {
    /// The B2 finalize decision derives from the disposition — single
    /// source of truth for the certainty⇔disposition biconditional.
    pub fn certainty(self) -> ExecutionCertainty {
        match self {
            CallDisposition::InFlightUnresolved => ExecutionCertainty::Uncertain,
            CallDisposition::Completed
            | CallDisposition::NeverDispatched
            | CallDisposition::CancelledBeforeDispatch => ExecutionCertainty::Definite,
        }
    }
}

/// Correlation id for ONE submission of a call (unit 4, design r4 Delta B).
///
/// Allocated by the submitting facade, monotonic per coordinator, and
/// carried on [`McpCommand::CallTool`] so a
/// [`McpEvent::SubmissionRejected`] can be correlated to exactly the
/// submission that was refused — the intent waiter table stays reserved
/// for the single `CallFinished` and is never keyed on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubmissionId(pub u64);

/// A single unit of work for the runtime thread. Canonical values only.
#[derive(Debug)]
pub enum McpCommand<T> {
    /// Execute one already-authorized, already-intent-logged call.
    ///
    /// The ticket moves in and returns with the intent's single terminal
    /// [`McpEvent::CallFinished`] — or, for a DUPLICATE submission
    /// against a live or recently-terminal intent, with the non-terminal
    /// [`McpEvent::SubmissionRejected`] (the original's terminal stays
    /// unique; INV-C2-5 is per-intent). `intent_id` is carried alongside
    /// (the ticket exposes it, but the runtime treats the ticket as
    /// opaque cargo) so per-call state and cancellation key on the B2
    /// intent identity, never on a provider call id.
    CallTool {
        ticket: T,
        intent_id: String,
        /// Correlates this submission with a possible
        /// [`McpEvent::SubmissionRejected`] (duplicate-in-flight-intent).
        /// Accepted submissions receive NO separate acknowledgement — they
        /// stay correlated by `intent_id` until their single
        /// `CallFinished` or a [`McpRuntimeHandle::submit_until`] refusal
        /// (ports addendum §6).
        submission_id: SubmissionId,
        origin: ToolOrigin,
        call: ToolCall,
        /// The GENERATION this authorization was validated against (unit
        /// 4b, impl review r1 P1-1): the runtime refuses a call whose
        /// generation is not the origin's CURRENT Ready generation — a
        /// binding retained across an identical reconnect (same name,
        /// same epoch, same tools) is refused Definite rather than
        /// silently surviving into the fresh child.
        generation: awili_engine::generation::ConnectionGeneration,
        /// The C1 root-capability epoch this authorization is bound to
        /// (contract §6 Q5). The runtime refuses a call whose epoch does
        /// not equal the origin's PUBLISHED accepted epoch — and refuses
        /// everything for an origin C1 has never published for.
        root_epoch: u64,
    },
    /// Cancel a submitted call, keyed by B2 `intent_id` (ports addendum
    /// §3 — never a provider call id). Idempotent: unknown or already-
    /// terminal intents are absorbed (a bounded tombstone catches the
    /// cancel-before-CallTool FIFO race). Terminal synthesis is
    /// join-before-synthesis: the real result always wins.
    Cancel { intent_id: String },
    /// Begin connecting one catalog origin (unit 4b, design r9 §1/§5.3).
    /// IDEMPOTENT: an origin already Connecting / AwaitingAcceptance /
    /// Ready mints no generation and re-emits its current lifecycle fact,
    /// so N workers each submitting ConnectOrigin after observing Dead(g)
    /// are harmless. Only Disconnected/Dead mint the next generation.
    ConnectOrigin { origin: ToolOrigin },
    /// C1 publishes its accepted root-capability epoch for an origin
    /// (monotonic), BOUND TO THE GENERATION it validated (unit 4b: a
    /// publication for a generation the runtime is not awaiting is refused
    /// loudly, never applied to a newer generation). Advertisements
    /// (`ServerConnected.advertised_epoch`) are PROPOSALS and never mutate
    /// the accepted table — C2 emits, C1 re-accepts, C3 enforces.
    UpdateAcceptedRoots {
        origin: ToolOrigin,
        generation: awili_engine::generation::ConnectionGeneration,
        epoch: u64,
    },
    /// Begin orderly shutdown: refuse new work, drain, emit terminals,
    /// reap children, close the event stream, exit the thread.
    Shutdown,
}

/// A tool advertised by a connected MCP server, reduced to canonical parts.
///
/// This is deliberately NOT a full `ToolDef`: descriptor construction and
/// registration policy stay engine-side with the registry. The runtime
/// only reports what the server said, bounded and pre-classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedTool {
    pub name: String,
    /// The tool's input schema as canonical JSON text (recursively
    /// key-sorted by the runtime before crossing the channel). Bounded at
    /// capture; an over-limit schema drops the TOOL, never truncates the
    /// schema (a truncated schema would digest to a different identity).
    pub arg_schema_canonical: String,
    /// Risk classification derived from the server's metadata via
    /// [`classify_advertised`]. Absent or contradictory metadata is
    /// `Privileged` — fail closed.
    pub risk: RiskClass,
    /// RC1-R: the server's own description of the tool, bounded and
    /// cleaned at capture ([`bounded_description`]); empty when it gave none.
    pub description: String,
}

/// What the server declared about a tool's side effects, as observed on
/// the wire. All fields optional because real servers may provide any
/// subset (the testserver gate's explicit rule).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SideEffectSignals {
    /// MCP 2025-03-26 `annotations.readOnlyHint`.
    pub read_only_hint: Option<bool>,
    /// MCP 2025-03-26 `annotations.destructiveHint`.
    pub destructive_hint: Option<bool>,
    /// Awili's own namespaced `_meta["awili/sideEffect"]`, parsed as a
    /// canonical risk string. `Some(None)` means present-but-malformed.
    pub awili_side_effect: Option<Option<RiskClass>>,
}

/// Classify an advertised tool's risk from its wire metadata. Fail closed:
///
/// - no metadata at all → `Privileged` (real servers may provide none);
/// - a present-but-malformed `awili/sideEffect` → `Privileged`;
/// - contradictory signals (e.g. `awili/sideEffect: read_only` but
///   `destructiveHint: true`, or `readOnlyHint: true` with
///   `destructiveHint: true`) → the MOST restrictive of the candidates,
///   which for a contradiction involving destruction is `Irreversible`;
/// - otherwise the declared class, with annotations alone mapping
///   readOnlyHint=true → `ReadOnly`, destructiveHint=true → `Irreversible`,
///   destructiveHint=false (with readOnlyHint=false) → `Mutating`.
///
/// The result is a FLOOR for C1 (`AuthzContext.registry_floor`); C1 may
/// escalate per-arguments, never de-escalate.
pub fn classify_advertised(signals: SideEffectSignals) -> RiskClass {
    // Present-but-malformed Awili metadata: the server tried to classify
    // and failed. Trust nothing it said.
    if let Some(None) = signals.awili_side_effect {
        return RiskClass::Privileged;
    }
    let awili = signals.awili_side_effect.flatten();
    let annot = match (signals.read_only_hint, signals.destructive_hint) {
        (None, None) => None,
        (Some(true), Some(true)) => Some(RiskClass::Irreversible), // contradiction
        (Some(true), _) => Some(RiskClass::ReadOnly),
        (_, Some(true)) => Some(RiskClass::Irreversible),
        (Some(false), _) | (_, Some(false)) => Some(RiskClass::Mutating),
    };
    match (awili, annot) {
        (None, None) => RiskClass::Privileged,
        (Some(l), None) => l,
        (None, Some(a)) => a,
        // Both present: most restrictive wins. Privileged outranks all,
        // then Irreversible, Mutating, ReadOnly.
        (Some(l), Some(a)) => most_restrictive(l, a),
    }
}

fn severity(r: RiskClass) -> u8 {
    match r {
        RiskClass::ReadOnly => 0,
        RiskClass::Mutating => 1,
        RiskClass::Irreversible => 2,
        RiskClass::Privileged => 3,
    }
}

fn most_restrictive(a: RiskClass, b: RiskClass) -> RiskClass {
    if severity(a) >= severity(b) {
        a
    } else {
        b
    }
}

/// Why one generation of one origin died (unit 4b, design r9 §1.2/§2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeathCause {
    /// The child process could not be spawned. No token was ever selected.
    LaunchFailed,
    /// Phase A attestation failed; the process was killed and reaped before
    /// any MCP byte was written. No token was selected — by construction
    /// `selected_class` is `None` on this cause.
    AttestationFailed { pinned_requirement: String },
    /// Phase B (`initialize` / `tools/list` / server-name check) failed
    /// after token selection.
    HandshakeFailed,
    /// The transport broke or the child died after the handshake.
    TransportFailed,
}

/// ONE lifecycle observation of ONE successful connection (design r9 §2.2).
/// Carries everything downstream authority is derived from.
///
/// No public constructor anywhere. Not `Clone`, not `Copy`, no serde, no
/// `Deref`, no accessor returning an owned launch. The ONLY code that can
/// build one is [`McpEventSink::emit_server_connected`], in this crate, and
/// the origin/generation are COPIED FROM THE TOKEN — no argument can
/// disagree with them, so [`ConnectedLaunch::bind`] is infallible.
///
/// `Debug + Send + Sync` are load-bearing: `McpEvent<T>` derives `Debug` and
/// existing panic paths format the event; the coordinator shares the value
/// as `Arc<ConnectedLaunch>` across threads. `Debug` prints facts and cannot
/// produce a `&VerifiedLaunch`.
#[derive(Debug)]
pub struct ConnectedLaunch {
    generation: awili_engine::generation::ConnectionGeneration,
    origin: ToolOrigin,
    launch: awili_attest_types::LaunchToken,
    claimed_server: String,
    tools: Vec<AdvertisedTool>,
    dropped_at_capture: Vec<String>,
    advertised_epoch: Option<u64>,
}

impl ConnectedLaunch {
    pub fn generation(&self) -> awili_engine::generation::ConnectionGeneration {
        self.generation
    }
    pub fn origin(&self) -> &ToolOrigin {
        &self.origin
    }
    pub fn claimed_server(&self) -> &str {
        &self.claimed_server
    }
    pub fn tools(&self) -> &[AdvertisedTool] {
        &self.tools
    }
    pub fn dropped_at_capture(&self) -> &[String] {
        &self.dropped_at_capture
    }
    pub fn advertised_epoch(&self) -> Option<u64> {
        self.advertised_epoch
    }
    pub fn attestation_class(&self) -> awili_attest_types::AttestationClass {
        self.launch.class()
    }

    /// The one authority-bearing read. Binds the token against THIS
    /// observation's own origin and generation — which are the token's own
    /// by construction, so the low-level refusal path is unreachable here
    /// and treated as an internal invariant violation.
    pub fn bind(&self) -> awili_attest_types::BoundLaunch<'_> {
        self.launch
            .bind_scoped(&self.origin, self.generation)
            .expect("ConnectedLaunch identity is token-sourced; bind cannot mismatch")
    }

    /// Stricter: `Err` for the unverified class (design r9 §2.6 clause 1).
    pub fn bind_verified(
        &self,
    ) -> Result<&awili_attest_types::VerifiedLaunch, awili_attest_types::UnverifiedReason> {
        match self.bind() {
            awili_attest_types::BoundLaunch::Verified(v) => Ok(v),
            awili_attest_types::BoundLaunch::Unverified(u) => Err(u.reason()),
        }
    }
}

/// The emit half of one event stream (design r9 §2.2). Not `Clone` at the
/// type level — the runtime shares it among its per-call tasks as
/// `Arc<McpEventSink<T>>`.
pub struct McpEventSink<T> {
    tx: async_channel::Sender<McpEvent<T>>,
}

/// The stream half, opaque: wraps the receiver and never exposes it. The
/// only consumer surface is assembly into [`McpRuntimeHandle::from_parts`].
pub struct McpEventStream<T> {
    rx: async_channel::Receiver<McpEvent<T>>,
}

/// The event side of the sink is closed (stream dropped); the runtime is
/// shutting down and the emission was not delivered.
#[derive(Debug, PartialEq, Eq)]
pub struct EmitClosed;

/// The ONLY way to obtain a sink, and the ONLY way to obtain the stream a
/// handle can be assembled from (design r9 §2.2). After this factory, code
/// outside `awili-tools` cannot obtain the raw endpoints paired with a
/// stream, nor convert an arbitrary receiver into an [`McpEventStream`] —
/// the raw channel TYPES stay nameable wherever `async-channel` is a
/// dependency; the property is endpoint possession.
pub fn event_channel<T>(capacity: usize) -> (McpEventSink<T>, McpEventStream<T>) {
    let (tx, rx) = async_channel::bounded(capacity);
    (McpEventSink { tx }, McpEventStream { rx })
}

/// A refused [`McpEventSink::try_emit`], returning the event by move so
/// nothing is lost — mirror of the raw channel's `TrySendError`.
#[derive(Debug)]
pub enum TryEmitError<T> {
    /// Bounded capacity reached; the event was NOT enqueued.
    Full(McpEvent<T>),
    /// The stream side is gone.
    Closed(McpEvent<T>),
}

impl<T> McpEventSink<T> {
    async fn emit(&self, ev: McpEvent<T>) -> Result<(), EmitClosed> {
        self.tx.send(ev).await.map_err(|_| EmitClosed)
    }

    /// Send an already-built event, awaited (bounded backpressure).
    /// Provenance is unaffected: [`McpEvent::ServerConnected`] cannot be
    /// built outside this crate, so a generic send cannot introduce a
    /// second observation issuer.
    pub async fn send(&self, ev: McpEvent<T>) -> Result<(), EmitClosed> {
        self.emit(ev).await
    }

    /// Non-async send for non-async contexts (e.g. the runtime-construction
    /// failure path that drains queued commands with DEFINITE terminals).
    pub fn send_blocking(&self, ev: McpEvent<T>) -> Result<(), EmitClosed> {
        self.tx.send_blocking(ev).map_err(|_| EmitClosed)
    }

    /// Non-blocking send for the runtime's backpressure lever: on `Full`
    /// the caller gets the event back and decides whether to await.
    // The Err variant carries the rejected event back by value on purpose
    // (the caller re-queues it); boxing would add an allocation per refusal.
    #[allow(clippy::result_large_err)]
    pub fn try_emit(&self, ev: McpEvent<T>) -> Result<(), TryEmitError<T>> {
        self.tx.try_send(ev).map_err(|e| match e {
            async_channel::TrySendError::Full(ev) => TryEmitError::Full(ev),
            async_channel::TrySendError::Closed(ev) => TryEmitError::Closed(ev),
        })
    }

    /// The sole constructor of [`ConnectedLaunch`]. The token is the single
    /// source of identity: origin and generation are COPIED FROM IT.
    /// Consumes the token by value; `LaunchToken` is not `Clone`, so one
    /// attested launch yields at most one observation. Async and awaited —
    /// bounded backpressure is preserved.
    pub async fn emit_server_connected(
        &self,
        launch: awili_attest_types::LaunchToken,
        claimed_server: String,
        tools: Vec<AdvertisedTool>,
        dropped_at_capture: Vec<String>,
        advertised_epoch: Option<u64>,
    ) -> Result<(), EmitClosed> {
        let origin = launch.origin().clone();
        let generation = launch.generation();
        self.emit(McpEvent::ServerConnected(ConnectedLaunch {
            generation,
            origin,
            launch,
            claimed_server,
            tools,
            dropped_at_capture,
            advertised_epoch,
        }))
        .await
    }

    /// Blocking variant of [`Self::emit_server_connected`] for synchronous
    /// contexts (scripted seam/coordinator tests, non-async drivers). Same
    /// sole-constructor and token-sourced-identity properties.
    pub fn emit_server_connected_blocking(
        &self,
        launch: awili_attest_types::LaunchToken,
        claimed_server: String,
        tools: Vec<AdvertisedTool>,
        dropped_at_capture: Vec<String>,
        advertised_epoch: Option<u64>,
    ) -> Result<(), EmitClosed> {
        let origin = launch.origin().clone();
        let generation = launch.generation();
        self.tx
            .send_blocking(McpEvent::ServerConnected(ConnectedLaunch {
                generation,
                origin,
                launch,
                claimed_server,
                tools,
                dropped_at_capture,
                advertised_epoch,
            }))
            .map_err(|_| EmitClosed)
    }

    pub async fn emit_origin_disconnected(&self, origin: ToolOrigin) -> Result<(), EmitClosed> {
        self.emit(McpEvent::OriginDisconnected { origin }).await
    }

    pub async fn emit_origin_connecting(
        &self,
        origin: ToolOrigin,
        generation: awili_engine::generation::ConnectionGeneration,
    ) -> Result<(), EmitClosed> {
        self.emit(McpEvent::OriginConnecting { origin, generation })
            .await
    }

    pub async fn emit_roots_accepted(
        &self,
        origin: ToolOrigin,
        generation: awili_engine::generation::ConnectionGeneration,
        epoch: u64,
    ) -> Result<(), EmitClosed> {
        self.emit(McpEvent::RootsAccepted {
            origin,
            generation,
            epoch,
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn emit_server_died(
        &self,
        origin: ToolOrigin,
        generation: awili_engine::generation::ConnectionGeneration,
        cause: DeathCause,
        selected_class: Option<awili_attest_types::AttestationClass>,
        detail: String,
    ) -> Result<(), EmitClosed> {
        self.emit(McpEvent::ServerDied {
            origin,
            generation,
            cause,
            selected_class,
            detail,
        })
        .await
    }

    pub async fn emit_roots_update_ignored(
        &self,
        origin: ToolOrigin,
        stale_epoch: u64,
        current_epoch: u64,
    ) -> Result<(), EmitClosed> {
        self.emit(McpEvent::RootsUpdateIgnored {
            origin,
            stale_epoch,
            current_epoch,
        })
        .await
    }

    pub async fn emit_call_finished(
        &self,
        ticket: T,
        intent_id: String,
        disposition: CallDisposition,
        result: ToolResultRecord,
    ) -> Result<(), EmitClosed> {
        self.emit(McpEvent::CallFinished {
            ticket,
            intent_id,
            disposition,
            result,
        })
        .await
    }

    pub async fn emit_submission_rejected(
        &self,
        ticket: T,
        intent_id: String,
        submission_id: SubmissionId,
        reason: String,
    ) -> Result<(), EmitClosed> {
        self.emit(McpEvent::SubmissionRejected {
            ticket,
            intent_id,
            submission_id,
            reason,
        })
        .await
    }
}

/// Events emitted by the runtime thread. Canonical values only.
#[derive(Debug)]
pub enum McpEvent<T> {
    /// The single terminal for one submitted call. Returns the ticket.
    ///
    /// Engine-side rule: `Definite` → `dispatch_finalized(&ticket,
    /// ok=!result.is_error, …)` then feed `result` to the reducer;
    /// `Uncertain` → do NOT finalize (replay classifies Uncertain), but
    /// still feed the error `result` to the reducer so the turn continues.
    CallFinished {
        ticket: T,
        intent_id: String,
        disposition: CallDisposition,
        result: ToolResultRecord,
    },
    /// Emitted once per catalog origin at runtime construction: the origin
    /// exists and nothing has connected yet (design r9 §1).
    OriginDisconnected { origin: ToolOrigin },
    /// A generation was minted and the connect attempt started.
    OriginConnecting {
        origin: ToolOrigin,
        generation: awili_engine::generation::ConnectionGeneration,
    },
    /// A server completed MCP initialize + tools/list. ONE opaque field:
    /// the variant is public, but it cannot be constructed without a
    /// [`ConnectedLaunch`], which has no public constructor — the sink is
    /// the sole issuer (design r9 §2.2). The advertised roots epoch it
    /// carries is a PROPOSAL, inert until C1 publishes acceptance via
    /// [`McpCommand::UpdateAcceptedRoots`].
    ServerConnected(ConnectedLaunch),
    /// C1's accepted root-capability epoch was published for this
    /// generation (design r9 §1): the origin becomes `Ready{g, e}`.
    RootsAccepted {
        origin: ToolOrigin,
        generation: awili_engine::generation::ConnectionGeneration,
        epoch: u64,
    },
    /// A submission was REJECTED without creating a call: the duplicate
    /// case — the same intent is in flight, or already terminal within
    /// the runtime's bounded recent-terminal history (unit 4 r6 Delta
    /// B″: admission checks `pending ∪ recently_terminal`). This is NOT
    /// a call terminal — the original intent's single CallFinished is
    /// still coming or was already delivered (INV-C2-5 per-intent
    /// uniqueness) — but it RETURNS the duplicate submission's ticket
    /// (conservation).
    SubmissionRejected {
        ticket: T,
        intent_id: String,
        /// The `submission_id` of the refused
        /// [`McpCommand::CallTool`] — the duplicate submission's own
        /// correlation id, so its waiter retires exactly this rejection
        /// (r4 Delta B).
        submission_id: SubmissionId,
        reason: String,
    },
    /// C1 sent a NON-monotonic accepted-roots update (stale epoch). The
    /// update was refused; this event makes that operationally visible —
    /// a stale security-authority update must never be silently dropped.
    RootsUpdateIgnored {
        origin: ToolOrigin,
        stale_epoch: u64,
        current_epoch: u64,
    },
    /// One generation of one origin died — launch, attestation, handshake,
    /// or transport (design r9 §1.2). Calls that were in flight against it
    /// get their own `CallFinished` terminals; this event is lifecycle
    /// information, not a terminal.
    ServerDied {
        origin: ToolOrigin,
        generation: awili_engine::generation::ConnectionGeneration,
        cause: DeathCause,
        /// `Some(class)` for every death AFTER token selection — including
        /// a handshake failure in `Connecting(g)`. `None` only when no
        /// token was ever selected: `LaunchFailed` and `AttestationFailed`.
        selected_class: Option<awili_attest_types::AttestationClass>,
        detail: String,
    },
}

/// Submitting a command failed without queueing it (ported verbatim from
/// the July prior art — the contract §5 non-queueing outcomes).
#[derive(Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// The bounded channel is at capacity; the command was NOT enqueued.
    /// Backpressure: defer/retry, never spin identical submissions.
    Full,
    /// The runtime thread is not running. Do not retry.
    Closed,
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::Full => f.write_str("mcp runtime channel is at capacity (backpressure)"),
            SubmitError::Closed => f.write_str("mcp runtime thread is not running"),
        }
    }
}

impl std::error::Error for SubmitError {}

/// Why a [`McpRuntimeHandle::submit_until`] finally refused (r3 Delta 1,
/// option 2). Both refusals hand the COMPLETE command back — ticket
/// included — so nothing is lost or re-minted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// The deadline passed with the bounded channel still at capacity.
    /// The command was never enqueued; retry with a fresh deadline is a
    /// caller decision.
    DeadlineExhausted,
    /// The runtime thread is not running. Do not retry.
    Closed,
}

/// A final [`McpRuntimeHandle::submit_until`] refusal: the refusal reason
/// plus the COMPLETE command — the non-`Clone` dispatch ticket included —
/// returned by move to the caller. Ticket conservation is structural:
/// nothing cloneable ever existed, so what comes back is what went in.
#[derive(Debug)]
pub struct RefusedSubmit<T> {
    pub kind: RefusalKind,
    pub command: McpCommand<T>,
}

/// Capacity-wait retry slice for `submit_until`. async-channel 2.x offers
/// no deadline-bounded blocking send, so a full channel is retried on
/// fixed time slices up to the caller's absolute deadline — never a hot
/// spin loop.
const SUBMIT_UNTIL_SLICE: std::time::Duration = std::time::Duration::from_millis(1);

/// Handle to the engine-owned MCP runtime thread. Canonical-only surface.
///
/// Constructed by the runtime implementation via [`Self::from_parts`];
/// held by engine-side code that submits commands and drains events.
/// Dropping the handle performs a best-effort `shutdown_and_drain`.
pub struct McpRuntimeHandle<T> {
    commands: async_channel::Sender<McpCommand<T>>,
    events: async_channel::Receiver<McpEvent<T>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl<T> McpRuntimeHandle<T> {
    /// Assemble a handle. The runtime implementation calls this at spawn
    /// time; nothing else should.
    ///
    /// NARROWED (unit 4b, design r9 §2.2): the events parameter is the
    /// opaque [`McpEventStream`], not a raw receiver. This is the removal
    /// of the bypass — after it, code outside `awili-tools` cannot obtain
    /// the raw endpoints paired with a stream, nor convert an arbitrary
    /// receiver into an `McpEventStream`, so no second emit path onto a
    /// stream the coordinator drains can exist. (The raw channel TYPES stay
    /// nameable wherever `async-channel` is a dependency; the property is
    /// endpoint possession.)
    pub fn from_parts(
        commands: async_channel::Sender<McpCommand<T>>,
        events: McpEventStream<T>,
        join: std::thread::JoinHandle<()>,
    ) -> Self {
        Self {
            commands,
            events: events.rx,
            join: Some(join),
        }
    }

    /// Submit a command. Non-queueing past capacity (contract §5): either
    /// accepted into the bounded channel or refused with [`SubmitError`].
    pub fn submit(&self, cmd: McpCommand<T>) -> Result<(), SubmitError> {
        match self.commands.try_send(cmd) {
            Ok(()) => Ok(()),
            Err(async_channel::TrySendError::Full(_)) => Err(SubmitError::Full),
            Err(async_channel::TrySendError::Closed(_)) => Err(SubmitError::Closed),
        }
    }

    /// Submit a command, retrying through channel capacity until `deadline`
    /// (unit 4, r3 Delta 1 option 2 — the retry lives where the sender
    /// lives). One absolute deadline, deadline-sliced `try_send` backoff,
    /// never a hot loop. On final refusal the COMPLETE command — the
    /// non-`Clone` ticket included — returns to the caller inside
    /// [`RefusedSubmit`]: no clone, no re-mint, no ticket loss. Additive to
    /// [`Self::submit`]; existing callers are unaffected.
    // Same rationale as `try_emit`: the refused command travels back by value.
    #[allow(clippy::result_large_err)]
    pub fn submit_until(
        &self,
        deadline: std::time::Instant,
        command: McpCommand<T>,
    ) -> Result<(), RefusedSubmit<T>> {
        let mut cmd = command;
        loop {
            match self.commands.try_send(cmd) {
                Ok(()) => return Ok(()),
                Err(async_channel::TrySendError::Closed(back)) => {
                    return Err(RefusedSubmit {
                        kind: RefusalKind::Closed,
                        command: back,
                    });
                }
                Err(async_channel::TrySendError::Full(back)) => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Err(RefusedSubmit {
                            kind: RefusalKind::DeadlineExhausted,
                            command: back,
                        });
                    }
                    std::thread::sleep(SUBMIT_UNTIL_SLICE.min(deadline - now));
                    cmd = back;
                }
            }
        }
    }

    /// Blocking receive of the next event. `None` when the runtime thread
    /// has exited and the stream is fully drained.
    pub fn recv_event_blocking(&self) -> Option<McpEvent<T>> {
        self.events.recv_blocking().ok()
    }

    /// Non-blocking event poll.
    pub fn try_recv_event(&self) -> Option<McpEvent<T>> {
        self.events.try_recv().ok()
    }

    /// Orderly shutdown (Codex design review, point 3: `join()` must never
    /// wait while the runtime blocks sending an event nobody drains):
    /// signal shutdown, then DRAIN the event stream to closure — which is
    /// what unblocks a runtime mid-send — then join the thread. Returns
    /// the drained events; every queued or in-flight call's terminal is in
    /// there, tickets included.
    pub fn shutdown_and_drain(mut self) -> Vec<McpEvent<T>> {
        self.shutdown_inner()
    }

    /// Signal shutdown WITHOUT consuming the handle — the order a shared
    /// (`Arc`-held) handle needs, because its owner cannot call
    /// [`Self::shutdown_and_drain`] while other threads still hold clones
    /// (unit 4 item 5 C5 review, finding 3). Best-effort explicit
    /// [`McpCommand::Shutdown`] first — the ordinary, drain-friendly path —
    /// then CLOSE the command channel, which the runtime treats exactly
    /// like an explicit `Shutdown`. The close is what makes the signal
    /// unmaskable: a bounded queue pinned `Full` refuses the command but
    /// cannot refuse closure, so a consumer that keeps the runtime waiting
    /// for commands still gets the shutdown order.
    pub fn signal_shutdown(&self) {
        let _ = self.submit(McpCommand::Shutdown);
        self.commands.close();
    }

    fn shutdown_inner(&mut self) -> Vec<McpEvent<T>> {
        // Best-effort: if the thread already exited, the send just fails.
        let _ = self.commands.try_send(McpCommand::Shutdown);
        // Closing the command channel is the second shutdown signal; the
        // runtime treats sender-closure exactly like an explicit Shutdown,
        // so a full command queue cannot mask the request.
        self.commands.close();
        // RC2-ENG5 (F0960): drain until the runtime THREAD has finished — not
        // until every event sender is gone. Another sink (a clone held by a
        // server reader, or a test) may outlive the thread, and waiting for
        // channel closure then blocked shutdown forever. RC2-ENGF3: and a
        // sink that keeps SENDING must not hold it open either.
        let join = &self.join;
        let drained = drain_until_finished(
            || self.events.try_recv(),
            || self.events.len(),
            || join.as_ref().is_none_or(|j| j.is_finished()),
        );
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        drained
    }
}

/// RC2-ENGF3 (RC2-ENG slice review P2): the shutdown drain. `next` reads one
/// queued event, `queued` says how many are queued now, `finished` whether
/// the runtime thread has finished. Every round FIRST asks whether the
/// thread has finished: once it has, everything it sent is already queued,
/// so the drain takes exactly what is queued at that moment and stops — a
/// sink that outlives the thread and keeps sending cannot hold shutdown
/// open (before, the question was asked only when the queue was empty,
/// which a busy sink never lets happen). Until then it drains, which is
/// what unblocks a runtime mid-send, resting 1 ms when the queue is empty.
fn drain_until_finished<E>(
    mut next: impl FnMut() -> Result<E, async_channel::TryRecvError>,
    queued: impl Fn() -> usize,
    finished: impl Fn() -> bool,
) -> Vec<E> {
    let mut drained = Vec::new();
    loop {
        if finished() {
            for _ in 0..queued() {
                match next() {
                    Ok(ev) => drained.push(ev),
                    Err(_) => break,
                }
            }
            return drained;
        }
        match next() {
            Ok(ev) => drained.push(ev),
            Err(async_channel::TryRecvError::Closed) => return drained,
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

impl<T> Drop for McpRuntimeHandle<T> {
    fn drop(&mut self) {
        if self.join.is_some() {
            let _ = self.shutdown_inner();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deliberately non-`Clone` ticket: the submit_until conservation
    /// proofs rely on the command coming back by MOVE — nothing copyable
    /// exists on the path.
    #[derive(Debug, PartialEq)]
    struct NonCloneTicket(&'static str);

    fn nonclone_command(sub: u64, ticket: &'static str) -> McpCommand<NonCloneTicket> {
        McpCommand::CallTool {
            ticket: NonCloneTicket(ticket),
            intent_id: format!("i-{ticket}"),
            submission_id: SubmissionId(sub),
            origin: ToolOrigin::new("test"),
            generation: awili_engine::generation::GenerationAllocator::new().next(),
            call: awili_engine::ToolCall {
                awili_call_id: awili_engine::AwiliCallId(format!("c-{ticket}")),
                provider_call_id: None,
                name: "echo".into(),
                arguments: "{}".into(),
            },
            root_epoch: 1,
        }
    }

    /// Handle over channels nobody serves: the runtime thread "exists" (a
    /// dummy join handle) but never drains commands.
    fn unserved<T>() -> (
        McpRuntimeHandle<T>,
        async_channel::Receiver<McpCommand<T>>,
        McpEventSink<T>,
    ) {
        let (tx, rx) = async_channel::bounded(1);
        let (etx, erx) = event_channel(1);
        (
            McpRuntimeHandle::from_parts(tx, erx, std::thread::spawn(|| {})),
            rx,
            etx,
        )
    }

    #[test]
    fn submit_until_retries_full_then_succeeds() {
        let (handle, rx, etx) = unserved();
        // Occupy the single capacity slot; a drainer frees it a moment
        // later. The drainer keeps the receiver alive and receives BOTH
        // commands — dropping it after one receive would close the
        // channel and masquerade as a Closed refusal.
        assert!(handle.submit(nonclone_command(0, "a")).is_ok());
        let drainer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            (
                rx.recv_blocking(),
                rx.recv_blocking(), // takes the retried command once it lands
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        assert!(handle
            .submit_until(deadline, nonclone_command(1, "b"))
            .is_ok());
        // The drainer took exactly the first command, then the retried
        // one — proof the Full-retry enqueued it rather than losing it.
        let (first, second) = drainer.join().unwrap();
        let ticket_of = |cmd: Result<McpCommand<NonCloneTicket>, _>| match cmd {
            Ok(McpCommand::CallTool { ticket, .. }) => ticket,
            other => panic!("unexpected command shape: {other:?}"),
        };
        assert_eq!(ticket_of(first), NonCloneTicket("a"));
        assert_eq!(ticket_of(second), NonCloneTicket("b"));
        drop(etx); // or shutdown_and_drain would block forever
        let _ = handle.shutdown_and_drain();
    }

    #[test]
    fn submit_until_deadline_returns_command_and_ticket() {
        let (handle, _rx, etx) = unserved::<NonCloneTicket>();
        // Fill capacity and never drain: the deadline must fire, and the
        // refused command must come back COMPLETE — non-Clone ticket,
        // submission id, and all — by move.
        assert!(handle.submit(nonclone_command(0, "a")).is_ok());
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
        let refused = handle
            .submit_until(deadline, nonclone_command(1, "b"))
            .expect_err("deadline must be refused");
        assert_eq!(refused.kind, RefusalKind::DeadlineExhausted);
        match refused.command {
            McpCommand::CallTool {
                ticket,
                intent_id,
                submission_id,
                ..
            } => {
                assert_eq!(ticket, NonCloneTicket("b"));
                assert_eq!(intent_id, "i-b");
                assert_eq!(submission_id, SubmissionId(1));
            }
            other => panic!("refused command changed shape: {other:?}"),
        }
        drop(etx);
        let _ = handle.shutdown_and_drain();
    }

    #[test]
    fn submit_until_closed_returns_command_and_ticket() {
        let (handle, rx, etx) = unserved::<NonCloneTicket>();
        rx.close();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let refused = handle
            .submit_until(deadline, nonclone_command(2, "c"))
            .expect_err("closed channel must be refused");
        assert_eq!(refused.kind, RefusalKind::Closed);
        match refused.command {
            McpCommand::CallTool {
                ticket,
                intent_id,
                submission_id,
                ..
            } => {
                assert_eq!(ticket, NonCloneTicket("c"));
                assert_eq!(intent_id, "i-c");
                assert_eq!(submission_id, SubmissionId(2));
            }
            other => panic!("refused command changed shape: {other:?}"),
        }
        drop(etx);
        let _ = handle.shutdown_and_drain();
    }

    /// Unit 4 item 5 (C5 review, finding 3): the shutdown order a FULL
    /// command channel cannot mask. `signal_shutdown` must close the
    /// command sender even when the bounded queue is pinned `Full` and
    /// refuses the explicit `Shutdown` — that closure is the signal a
    /// shared (`Arc`-held) handle's owner relies on to tear down without
    /// draining.
    #[test]
    fn signal_shutdown_closes_the_command_channel_even_when_full() {
        let (handle, rx, etx) = unserved::<NonCloneTicket>();
        // Pin the single capacity slot; nothing drains it.
        assert!(handle.submit(nonclone_command(0, "a")).is_ok());
        assert!(
            matches!(handle.submit(McpCommand::Shutdown), Err(SubmitError::Full)),
            "the explicit Shutdown is masked by backpressure"
        );
        handle.signal_shutdown();
        assert!(rx.is_closed(), "closure is the unmaskable signal");
        // Closure does not destroy queued work: the runtime still drains
        // the pinned command first, then observes the closed stream.
        assert!(matches!(
            rx.recv_blocking(),
            Ok(McpCommand::CallTool { .. })
        ));
        assert!(rx.recv_blocking().is_err(), "closure terminates intake");
        drop(etx);
        let _ = handle.shutdown_and_drain();
    }

    #[test]
    fn absent_metadata_classifies_privileged() {
        assert_eq!(
            classify_advertised(SideEffectSignals::default()),
            RiskClass::Privileged
        );
    }

    #[test]
    fn malformed_awili_side_effect_classifies_privileged() {
        // Present-but-unparseable Awili metadata must not fall back to the
        // (more permissive) annotations.
        assert_eq!(
            classify_advertised(SideEffectSignals {
                read_only_hint: Some(true),
                destructive_hint: Some(false),
                awili_side_effect: Some(None),
            }),
            RiskClass::Privileged
        );
    }

    #[test]
    fn annotations_alone_map_to_canonical_classes() {
        assert_eq!(
            classify_advertised(SideEffectSignals {
                read_only_hint: Some(true),
                destructive_hint: Some(false),
                awili_side_effect: None,
            }),
            RiskClass::ReadOnly
        );
        assert_eq!(
            classify_advertised(SideEffectSignals {
                read_only_hint: Some(false),
                destructive_hint: Some(false),
                awili_side_effect: None,
            }),
            RiskClass::Mutating
        );
        assert_eq!(
            classify_advertised(SideEffectSignals {
                read_only_hint: Some(false),
                destructive_hint: Some(true),
                awili_side_effect: None,
            }),
            RiskClass::Irreversible
        );
    }

    #[test]
    fn contradictory_signals_take_most_restrictive() {
        // Server says read_only in Awili metadata but destructive in MCP
        // annotations: believe the scarier one.
        assert_eq!(
            classify_advertised(SideEffectSignals {
                read_only_hint: None,
                destructive_hint: Some(true),
                awili_side_effect: Some(Some(RiskClass::ReadOnly)),
            }),
            RiskClass::Irreversible
        );
        // readOnlyHint and destructiveHint both true is itself a
        // contradiction → Irreversible.
        assert_eq!(
            classify_advertised(SideEffectSignals {
                read_only_hint: Some(true),
                destructive_hint: Some(true),
                awili_side_effect: None,
            }),
            RiskClass::Irreversible
        );
    }

    #[test]
    fn agreeing_signals_keep_the_declared_class() {
        assert_eq!(
            classify_advertised(SideEffectSignals {
                read_only_hint: Some(true),
                destructive_hint: Some(false),
                awili_side_effect: Some(Some(RiskClass::ReadOnly)),
            }),
            RiskClass::ReadOnly
        );
        assert_eq!(
            classify_advertised(SideEffectSignals {
                read_only_hint: Some(false),
                destructive_hint: Some(false),
                awili_side_effect: Some(Some(RiskClass::Mutating)),
            }),
            RiskClass::Mutating
        );
    }

    #[test]
    fn disposition_derives_certainty_biconditionally() {
        use CallDisposition::*;
        assert_eq!(
            InFlightUnresolved.certainty(),
            ExecutionCertainty::Uncertain
        );
        for d in [Completed, NeverDispatched, CancelledBeforeDispatch] {
            assert_eq!(d.certainty(), ExecutionCertainty::Definite);
        }
    }

    #[test]
    fn submit_reports_full_and_closed_distinctly() {
        let (tx, rx) = async_channel::bounded::<McpCommand<String>>(1);
        let (etx, erx) = event_channel::<String>(1);
        // A handle over a channel nobody serves: first submit fills, second
        // reports Full, and after close() reports Closed.
        let join = std::thread::spawn(|| {});
        let handle = McpRuntimeHandle::from_parts(tx, erx, join);
        let cmd = |t: &str| McpCommand::CallTool {
            ticket: t.to_string(),
            intent_id: format!("i-{t}"),
            submission_id: SubmissionId(0),
            origin: ToolOrigin::new("test"),
            generation: awili_engine::generation::GenerationAllocator::new().next(),
            call: awili_engine::ToolCall {
                awili_call_id: awili_engine::AwiliCallId(format!("c-{t}")),
                provider_call_id: None,
                name: "echo".into(),
                arguments: "{}".into(),
            },
            root_epoch: 1,
        };
        assert!(handle.submit(cmd("a")).is_ok());
        assert_eq!(handle.submit(cmd("b")).unwrap_err(), SubmitError::Full);
        rx.close();
        assert_eq!(handle.submit(cmd("c")).unwrap_err(), SubmitError::Closed);
        // Drop the event sender BEFORE draining, or the drain would block
        // forever on a stream nobody will ever close.
        drop(etx);
        let _ = handle.shutdown_and_drain();
    }

    /// RC2-ENG5 (F0960): shutdown returns once the runtime thread has
    /// finished, even while another event sink stays alive — and still
    /// hands back every event queued before it. Before, it waited for the
    /// channel to close and blocked forever.
    #[test]
    fn rc2eng5_shutdown_returns_while_another_sink_is_alive() {
        let (handle, _rx, etx) = unserved::<NonCloneTicket>();
        etx.send_blocking(McpEvent::OriginDisconnected {
            origin: ToolOrigin::new("test"),
        })
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let drained = handle.shutdown_and_drain();
            let _ = tx.send(drained.len());
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(1),
            "shutdown must return, with the queued event"
        );
        // The other sink is still alive here.
        drop(etx);
    }

    /// RC2-ENGF3 (RC2-ENG slice review P2): a sink that outlives the
    /// runtime thread and KEEPS SENDING never holds shutdown open. Before
    /// RC2-ENGF3 the drain checked whether the thread had finished only
    /// when the queue was empty — which a busy sink never lets happen — so
    /// shutdown spun forever. Now the check comes first on every round and
    /// the final drain takes only what was queued when the thread was seen
    /// finished.
    #[test]
    fn rc2engf3_shutdown_returns_while_another_sink_keeps_emitting() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        /// Stops the emitters on every exit, a failed assertion included.
        struct Stop(Arc<AtomicBool>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        const CAPACITY: usize = 64;
        let (tx, _rx) = async_channel::bounded::<McpCommand<NonCloneTicket>>(1);
        let (etx, erx) = event_channel::<NonCloneTicket>(CAPACITY);
        let handle = McpRuntimeHandle::from_parts(tx, erx, std::thread::spawn(|| {}));
        let noisy = || McpEvent::OriginDisconnected {
            origin: ToolOrigin::new("noisy"),
        };
        // Fill the queue, then keep it full from two threads.
        while etx.try_emit(noisy()).is_ok() {}
        let etx = Arc::new(etx);
        let stop = Stop(Arc::new(AtomicBool::new(false)));
        let emitters: Vec<_> = (0..2)
            .map(|_| {
                let (etx, stop) = (etx.clone(), stop.0.clone());
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = etx.try_emit(noisy());
                    }
                })
            })
            .collect();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let drained = handle.shutdown_and_drain();
            let _ = done_tx.send(drained.len());
        });
        let drained = done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("shutdown must return while the other sink keeps sending");
        assert!(
            drained >= CAPACITY,
            "everything queued when the thread was seen finished is drained: {drained}"
        );
        drop(stop);
        for e in emitters {
            e.join().unwrap();
        }
    }

    /// RC2-ENGF3: the drain itself, over a queue that is NEVER empty (every
    /// read finds another event — what a busy surviving sink looks like at
    /// worst). It drains while the thread runs, and once the thread is seen
    /// finished takes exactly what is queued then and stops. Deterministic:
    /// the drain before RC2-ENGF3 asked about the thread only on an empty
    /// queue, so here it never returns (mutation-checked).
    #[test]
    fn rc2engf3_the_drain_stops_once_the_thread_is_done_however_busy_the_queue() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let reads = std::cell::Cell::new(0u32);
            let next = || {
                reads.set(reads.get() + 1);
                Ok::<u32, async_channel::TryRecvError>(reads.get())
            };
            // The thread finishes after five reads; two are queued then.
            let drained = drain_until_finished(next, || 2, || reads.get() >= 5);
            let _ = tx.send(drained);
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(vec![1, 2, 3, 4, 5, 6, 7]),
            "five read while it ran, then the two queued when it finished"
        );
    }
}

/// RC1-R: the most characters of a server's tool description kept for the
/// model (a description is model-facing text from the server; many tools with
/// long descriptions would crowd a small context window).
pub const MAX_TOOL_DESCRIPTION_CHARS: usize = 512;

/// RC1-R: a server's tool description as the model gets it: control
/// characters other than newline and tab removed, then cut to
/// [`MAX_TOOL_DESCRIPTION_CHARS`] characters (never inside a character).
pub fn bounded_description(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(MAX_TOOL_DESCRIPTION_CHARS)
        .collect()
}

#[cfg(test)]
mod rc1r_tests {
    use super::*;

    /// RC1-R: a server's description reaches the model without control
    /// characters (newline and tab kept), cut on a character boundary.
    #[test]
    fn rc1r_descriptions_are_cleaned_and_bounded() {
        assert_eq!(bounded_description("a\u{1b}[31mb\n\tc"), "a[31mb\n\tc");
        let long: String = "é".repeat(MAX_TOOL_DESCRIPTION_CHARS + 10);
        let cut = bounded_description(&long);
        assert_eq!(cut.chars().count(), MAX_TOOL_DESCRIPTION_CHARS);
        assert_eq!(bounded_description(""), "");
    }
}
