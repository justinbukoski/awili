//! Unit 4 item 5 — the MCP coordinator: pump, waiters, linearized
//! cancellation (design r2/r3/r4/r5/r6, verdict r7 item 5).
//!
//! One [`McpCoordinator`] per runtime lifetime is the **sole owner** of the
//! [`McpRuntimeHandle`] and the **sole consumer** of the `McpEvent` stream,
//! on a dedicated pump thread it owns. No other component ever holds the
//! event receiver: the handle's `Arc` lives only inside the coordinator's
//! own two threads, and the seam-facing [`McpFacade`] is clonable
//! command-side only — a bounded request channel plus read-only
//! diagnostics. The seam blocks on the intent-scoped completion channel
//! only.
//!
//! ## Waiter state (two tables, deliberately separate)
//!
//! - **Intent waiters**, keyed by `intent_id`: reserved EXCLUSIVELY for the
//!   intent's single `CallFinished`. The primary submission's completion
//!   channel is registered here BEFORE `submit_until` (r5 Delta B′
//!   registration order) and receives exactly one terminal.
//! - **Submission state**, keyed by `SubmissionId`, coordinator-private:
//!   every submission (primary included) registers its own waiter before
//!   submit, so a [`McpEvent::SubmissionRejected`] retires exactly its own
//!   waiter and delivers the duplicate's ticket as a typed failure. The
//!   intent table additionally carries the r5 primary/secondary
//!   bookkeeping (`primary` + `secondaries`), established PROVISIONALLY in
//!   the same register-before-submit lock section — before the command can
//!   enter the runtime — and rolled back synchronously on refusal. The
//!   ACCEPTED submission is therefore identifiable without any acceptance
//!   ack AND without any post-submit bookkeeping: a `CallFinished` the
//!   runtime emits the instant the command lands still finds
//!   `primary == Some(..)`, and a fast `SubmissionRejected` still finds the
//!   submission already recorded as a secondary.
//!
//! ## The single-lock commit point (r3 Delta 3)
//!
//! Outcomes for an intent are committed through ONE linearization point:
//! the waiter-table entry, mutated only by the pump thread (terminals) and
//! the cancel-refusal path (both under the same state lock). A real
//! `CallFinished` wins **iff the pump commits it before the coordinator
//! commits cancel-submission failure**. Once failure is committed and the
//! waiter removed, ANY later terminal is quarantined — retained with its
//! ticket for replay reconciliation, never honored, never finalized.
//!
//! ## Ticket conservation
//!
//! The coordinator is the sole submitter and allocates every `SubmissionId`.
//! `submit_until` gives each submission exactly ONE enqueue outcome; on
//! final refusal the COMPLETE command — non-`Clone` ticket included — is
//! returned to the caller by move inside [`CallSubmitFailure`] (no clone,
//! no re-mint, nothing dropped). Cancel refusal is asymmetric and
//! deliberately ticket-free ([`CancelSubmitFailure`]): `Cancel` carries no
//! ticket, so a typed cancel refusal is not ticket loss. Unknown, late, or
//! duplicate terminals route their tickets to a quarantine record instead
//! of being dropped — and quarantine is LOSSLESS: an ownership-bearing
//! record is never evicted, only drained by the owner, with the operational
//! bound surfaced as a diagnostic. The one admitted loss beyond that is
//! stream closure, which wakes every live waiter with the typed
//! [`SubmissionOutcome::StreamClosed`] failure and leaves started,
//! unfinalized intents for B2 replay to classify — no synthetic terminals,
//! ever.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use awili_engine::events::ToolResultRecord;
use awili_engine::generation::ConnectionGeneration;
use awili_engine::ToolCall;
use awili_tools::attest::AttestationClass;
use awili_tools::runtime::{
    CallDisposition, DeathCause, McpCommand, McpEvent, McpRuntimeHandle, RefusalKind, SubmissionId,
};
use awili_tools::ToolOrigin;

/// Bounded capacity of the facade→coordinator request channel.
const COORD_CMD_CAPACITY: usize = 64;

/// Bounded retention of RETIRED intent entries (terminal or cancel-failed).
/// Mirrors the runtime's `RECENT_TERMINAL_CAP` honesty: the coordinator's
/// late-terminal classification is defense-in-depth with a window, not an
/// unbounded guarantee. Live entries are never evicted.
const RETIRED_INTENT_CAP: usize = 1024;

/// Bounded diagnostic / ignored-roots retention. Diagnostics carry no
/// ticket, so eviction here is bookkeeping, not conservation.
const RECORD_CAP: usize = 256;

/// Operational bound on the quarantine record — the count past which the
/// coordinator says so out loud. This is NOT an eviction cap: a
/// quarantined record owns a ticket, and r2's binding rule retains those
/// "never dropped." The bound survives as the
/// [`CoordinatorDiagnostic::QuarantineOverOperationalBound`] diagnostic so
/// an operator still learns the record is growing past its envelope.
const QUARANTINE_OPERATIONAL_BOUND: usize = 256;

/// What one submission's waiter receives — exactly one of these, ever.
#[derive(Debug)]
pub enum SubmissionOutcome<T> {
    /// The intent's single terminal (r5: the `CallFinished` retires ONLY
    /// the primary's no-ack waiter state). Definite → the engine finalizes
    /// with the RETURNED ticket; Uncertain → replay classifies.
    Terminal {
        ticket: T,
        disposition: CallDisposition,
        result: ToolResultRecord,
    },
    /// THIS submission was rejected as a duplicate (pending ∪
    /// recently-terminal admission, unit 4 r6 Delta B″). Not a terminal:
    /// the original intent's `CallFinished` is untouched. The duplicate's
    /// own ticket returns here — conserved, typed.
    Rejected { ticket: T, reason: String },
    /// A `Cancel` submission finally refused (`Closed`/deadline). A typed
    /// runtime failure that WAKES the waiter (r3 Delta 3): the call's
    /// outcome is not knowable through this path, an Uncertain intent is
    /// NOT finalized, and any later real terminal is quarantined.
    /// Distinguishable from call refusal and deliberately ticket-free —
    /// `Cancel` never carried one.
    CancelSubmitRefused { kind: RefusalKind },
    /// The runtime event stream closed. Every live waiter (primary and
    /// secondary) is woken with this typed failure; started, unfinalized
    /// intents are left for replay to classify. No synthetic terminals.
    StreamClosed,
}

/// A final call-submission refusal: the refusal kind plus the COMPLETE
/// command — the non-`Clone` ticket included — returned by move. Nothing
/// was enqueued, so no rejection can arrive for it; the waiter is removed
/// synchronously before this returns.
#[derive(Debug)]
pub struct CallSubmitFailure<T> {
    pub kind: RefusalKind,
    pub command: McpCommand<T>,
}

/// A final cancel-submission refusal. Carries NO ticket — `Cancel` never
/// had one — and MUST NOT be treated as ticket loss (r3 Delta 3's
/// command-recovery asymmetry). Typed distinctly from
/// [`CallSubmitFailure`] so the two refusals cannot be confused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelSubmitFailure {
    pub kind: RefusalKind,
}

/// The caller-side handle returned by a successful [`McpFacade::submit_call`].
/// `wait` blocks on the intent-scoped completion channel (primary) or this
/// submission's own rejection channel (secondary) — the seam blocks here
/// and nowhere else.
#[derive(Debug)]
pub struct CallWaiter<T> {
    primary: bool,
    rx: async_channel::Receiver<SubmissionOutcome<T>>,
}

impl<T> CallWaiter<T> {
    /// Whether this submission is the intent's PRIMARY (the accepted one
    /// whose channel carries the single `CallFinished`).
    pub fn is_primary(&self) -> bool {
        self.primary
    }

    /// Block for this submission's one outcome. The pump guarantees
    /// delivery on every path — terminal, rejection, cancel refusal, or
    /// stream closure — so an empty channel here is coordinator death and
    /// fails loudly rather than hanging.
    pub fn wait(self) -> SubmissionOutcome<T> {
        match self.rx.recv_blocking() {
            Ok(outcome) => outcome,
            Err(_) => panic!("mcp coordinator dropped a live submission waiter"),
        }
    }

    /// Deadline-bounded wait (test/seam-shutdown discipline): `None` when
    /// the deadline passes first. Deadline-sliced poll, never a hot loop.
    /// Borrows, so a rig can poll for absence of a further outcome after
    /// the one already received.
    pub fn wait_deadline(&self, deadline: Instant) -> Option<SubmissionOutcome<T>> {
        loop {
            if let Ok(outcome) = self.rx.try_recv() {
                return Some(outcome);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(5).min(deadline - now));
        }
    }
}

/// Hard diagnostics recorded by the coordinator. Every one of these is an
/// invariant violation or a fault the operator must see; none is silently
/// absorbed. Ticket-bearing residue from terminal-side violations lives in
/// the quarantine record instead ([`McpFacade::take_quarantine`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordinatorDiagnostic {
    /// A second submission was enqueued for an intent that already has a
    /// primary (or is already retired). Normal unit-4 flow never produces
    /// one — the coordinator is the sole submitter — so this firing means
    /// the caller violated the one-submission-per-intent discipline. The
    /// machinery still handles it honestly: the rejection routes back.
    SecondarySubmission {
        intent_id: String,
        submission_id: SubmissionId,
    },
    /// A `CallFinished` for an intent the coordinator never saw live.
    UnknownTerminal { intent_id: String },
    /// A `CallFinished` after cancel-failure was already committed — the
    /// post-failure terminal the linearization rule quarantines.
    LateTerminal { intent_id: String },
    /// A second `CallFinished` for an intent that already got its one
    /// terminal (INV-C2-5 violated on the wire).
    DuplicateTerminal { intent_id: String },
    /// A `SubmissionRejected` whose submission waiter is already gone.
    RejectionWithoutWaiter { submission_id: SubmissionId },
    /// A connected server advertised more tools than the bounded capture
    /// allowed — registered, and the drop surfaced (r2 acceptance 9).
    ToolsDropped { origin: String, count: u32 },
    /// A cancel submission finally refused AFTER the intent's terminal was
    /// already committed. Absorbed: the terminal won the linearization and
    /// the refusal cannot revise it.
    CancelRefusalAfterTerminal {
        intent_id: String,
        kind: RefusalKind,
    },
    /// Stream closure arrived with this many live submission waiters woken.
    StreamClosedWithLiveWaiters { count: usize },
    /// A waiter channel was full or abandoned at delivery — the
    /// one-outcome-per-channel invariant, or a caller that dropped its
    /// waiter before receiving. Ticket-bearing outcomes were quarantined.
    WaiterDeliveryOverrun { intent_id: String },
    /// The quarantine record grew past its operational bound. Every ticket
    /// is still retained — quarantine is lossless — but the record is now
    /// larger than its envelope and someone must drain it (replay
    /// reconciliation) before it grows without limit.
    QuarantineOverOperationalBound { retained: usize },
}

/// A ticket-bearing event the coordinator could not deliver to any live
/// waiter. Retained — never dropped — for ticket conservation and B2 replay
/// reconciliation. Drain via [`McpFacade::take_quarantine`].
#[derive(Debug)]
pub struct QuarantinedTicket<T> {
    pub intent_id: String,
    pub submission_id: Option<SubmissionId>,
    pub ticket: T,
    /// Terminal classification when the quarantined event was a
    /// `CallFinished`; `None` for a quarantined rejection.
    pub disposition: Option<CallDisposition>,
    pub result: Option<ToolResultRecord>,
    pub note: String,
}

/// Latest ingested lifecycle state for one origin (continuously maintained
/// by the pump — while calls block, while turns idle).
///
/// Unit 4b (design r9 §1/§1.1): five states, FACTS ONLY. The value — tools,
/// token, claim — never sits in this `Clone` snapshot; it lives in the
/// coordinator's `Arc` side table (landing with the app-composition commit).
/// Within one generation, status is monotonic: `Connecting → Awaiting →
/// Ready → Dead`, never backwards. `Dead(g)` is stable — no disconnected
/// write follows a death, so the diagnostic survives until the next
/// generation's `OriginConnecting`.
#[derive(Debug, Clone, PartialEq)]
pub enum OriginStatus {
    /// The catalog origin exists; nothing has connected yet.
    Disconnected,
    Connecting {
        generation: ConnectionGeneration,
    },
    AwaitingAcceptance {
        generation: ConnectionGeneration,
        attestation: AttestationClass,
        claimed_server: String,
        tool_count: usize,
        dropped_count: usize,
        roots_epoch: Option<u64>,
    },
    Ready {
        generation: ConnectionGeneration,
        accepted_epoch: u64,
        attestation: AttestationClass,
    },
    Dead {
        generation: ConnectionGeneration,
        cause: DeathCause,
        detail: String,
        attestation: Option<AttestationClass>,
    },
}

/// RC2-ENG4 (F0659): a lifecycle step within one generation, in order.
const STEP_CONNECTING: u8 = 0;
const STEP_AWAITING: u8 = 1;
const STEP_READY: u8 = 2;
const STEP_DEAD: u8 = 3;

/// RC2-ENG4 (F0659): where a status stands — its generation and its step
/// within that generation. `Disconnected` (nothing connected yet) has none.
fn lifecycle_position(s: &OriginStatus) -> Option<(ConnectionGeneration, u8)> {
    match s {
        OriginStatus::Disconnected => None,
        OriginStatus::Connecting { generation } => Some((*generation, STEP_CONNECTING)),
        OriginStatus::AwaitingAcceptance { generation, .. } => Some((*generation, STEP_AWAITING)),
        OriginStatus::Ready { generation, .. } => Some((*generation, STEP_READY)),
        OriginStatus::Dead { generation, .. } => Some((*generation, STEP_DEAD)),
    }
}

/// RC2-ENG4 (F0659): a lifecycle event (generation, step) applies only when
/// it does not move the origin backwards — an older generation never
/// overwrites a newer one, and within one generation a step never undoes a
/// later one (a "connecting" or "connected" that arrives after the death).
/// Events reach the pump from several sinks, so their order is not
/// guaranteed.
fn lifecycle_applies(
    current: Option<&OriginStatus>,
    generation: ConnectionGeneration,
    step: u8,
) -> bool {
    current
        .and_then(lifecycle_position)
        .is_none_or(|cur| (generation, step) >= cur)
}

/// A stale accepted-roots update the runtime refused (ingested + traced).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredRootsUpdate {
    pub origin: String,
    pub stale_epoch: u64,
    pub current_epoch: u64,
}

/// Snapshot of the pump's continuously-ingested registry/lifecycle view.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LifecycleSnapshot {
    pub origins: HashMap<String, OriginStatus>,
    pub ignored_roots_updates: Vec<IgnoredRootsUpdate>,
    /// Monotonic change counter (unit 4b, design r9 §1): every lifecycle
    /// mutation the pump ingests — including a refused roots update —
    /// advances it. `lifecycle_since` gates on it; reconciliation's
    /// revision guard compares against it.
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntentStatus {
    /// Has a live primary path; its single terminal is still owed.
    Live,
    /// The single `CallFinished` was committed to the primary waiter.
    Terminal,
    /// Cancel-submission failure was committed; later terminals quarantine.
    Failed,
}

struct IntentEntry<T> {
    /// The PRIMARY's intent-scoped completion channel (r2: the waiter table
    /// keyed by `intent_id`, reserved exclusively for the one
    /// `CallFinished`). `None` once terminal/failure committed it.
    waiter: Option<async_channel::Sender<SubmissionOutcome<T>>>,
    status: IntentStatus,
    /// The accepted submission (first enqueued; r5 Delta B′), recorded
    /// PROVISIONALLY in the register-before-submit lock section and rolled
    /// back synchronously if that submission is finally refused.
    primary: Option<SubmissionId>,
    /// Later enqueued submissions for the same intent — each still owed its
    /// own `SubmissionRejected`.
    secondaries: HashSet<SubmissionId>,
}

struct SubmissionEntry<T> {
    waiter: async_channel::Sender<SubmissionOutcome<T>>,
    intent_id: String,
}

struct State<T> {
    intents: HashMap<String, IntentEntry<T>>,
    /// Insertion order for bounded eviction of retired entries.
    intent_order: VecDeque<String>,
    submissions: HashMap<SubmissionId, SubmissionEntry<T>>,
    quarantine: VecDeque<QuarantinedTicket<T>>,
    diagnostics: VecDeque<CoordinatorDiagnostic>,
    origins: HashMap<String, OriginStatus>,
    ignored_roots: VecDeque<IgnoredRootsUpdate>,
    /// Lifecycle change counter (design r9 §1). Bumped by every ingested
    /// lifecycle mutation, including a refused roots update.
    revision: u64,
    /// The app-lifetime side table (design r9 §5.1): THE observation per
    /// origin, shared by Arc so workers A and B hold the same immutable
    /// value with no lock spanning policy or projection (ruling B).
    /// Holding a fact is not authority: the pump never validates, never
    /// accepts, never projects.
    connected: HashMap<String, Arc<awili_tools::runtime::ConnectedLaunch>>,
    /// RC2-ENG4 (F0658): set by the pump, under this lock, when it wakes the
    /// waiters at stream closure. A submission that would register after
    /// that is refused instead — no waiter can be left that nothing wakes.
    closed: bool,
}

impl<T> State<T> {
    fn new() -> Self {
        Self {
            intents: HashMap::new(),
            intent_order: VecDeque::new(),
            submissions: HashMap::new(),
            quarantine: VecDeque::new(),
            diagnostics: VecDeque::new(),
            origins: HashMap::new(),
            ignored_roots: VecDeque::new(),
            revision: 0,
            connected: HashMap::new(),
            closed: false,
        }
    }

    fn push_diagnostic(&mut self, d: CoordinatorDiagnostic) {
        while self.diagnostics.len() >= RECORD_CAP {
            self.diagnostics.pop_front();
        }
        self.diagnostics.push_back(d);
    }

    /// Lossless quarantine: an ownership-bearing record is NEVER evicted —
    /// only the owner's drain ([`McpFacade::take_quarantine`]) removes it.
    /// The former pop-front-at-cap ring destroyed conserved tickets (the
    /// item-5 C5 review, finding 2); the count past the operational bound
    /// is surfaced as a diagnostic instead, so the envelope is still
    /// visible without ever transferring a ticket to nowhere.
    fn quarantine(&mut self, record: QuarantinedTicket<T>) {
        if self.quarantine.len() == QUARANTINE_OPERATIONAL_BOUND {
            self.push_diagnostic(CoordinatorDiagnostic::QuarantineOverOperationalBound {
                retained: QUARANTINE_OPERATIONAL_BOUND + 1,
            });
        }
        self.quarantine.push_back(record);
    }

    /// Bounded FIFO eviction of RETIRED intent entries. Live entries are
    /// never evicted; the bound keeps a buggy or adversarial submitter from
    /// growing coordinator memory without limit, mirroring the runtime's
    /// own bounded tombstone honesty.
    fn evict_retired(&mut self) {
        let mut scanned = 0;
        while self.intent_order.len() > RETIRED_INTENT_CAP && scanned < self.intent_order.len() {
            scanned += 1;
            match self.intent_order.pop_front() {
                Some(id) => match self.intents.get(&id) {
                    Some(entry) if entry.status != IntentStatus::Live => {
                        self.intents.remove(&id);
                    }
                    // Live ids re-queue at the back; live entries leave via
                    // terminal or cancel failure, never eviction.
                    Some(_) => self.intent_order.push_back(id),
                    // RC2-ENG4 (F0660): an id whose entry is already gone (a
                    // refused submission removed it) leaves the queue — it
                    // was re-queued forever before.
                    None => {}
                },
                None => return,
            }
        }
    }
}

struct Shared<T> {
    /// The single waiter-table lock: the pump's terminal commits and the
    /// cancel-refusal commits contend HERE and only here (r3 Delta 3).
    state: Mutex<State<T>>,
    next_submission: AtomicU64,
}

/// Deliver one outcome to a waiter. `try_send` never blocks: each waiter
/// channel receives exactly one outcome on a capacity-1 channel, so `Full`
/// is the one-outcome invariant broken and `Closed` is an abandoned
/// waiter — both hand the outcome back for quarantine.
fn deliver<T>(
    tx: &async_channel::Sender<SubmissionOutcome<T>>,
    outcome: SubmissionOutcome<T>,
) -> Result<(), SubmissionOutcome<T>> {
    match tx.try_send(outcome) {
        Ok(()) => Ok(()),
        Err(async_channel::TrySendError::Full(back)) => Err(back),
        Err(async_channel::TrySendError::Closed(back)) => Err(back),
    }
}

/// Recover a ticket-bearing outcome the waiter could not receive into
/// quarantine. Ticket-free outcomes (`CancelSubmitRefused`, `StreamClosed`)
/// have nothing to conserve: they surface as the overrun diagnostic alone.
fn quarantine_undelivered<T>(
    st: &mut State<T>,
    intent_id: String,
    submission_id: Option<SubmissionId>,
    outcome: SubmissionOutcome<T>,
    note: &str,
) {
    st.push_diagnostic(CoordinatorDiagnostic::WaiterDeliveryOverrun {
        intent_id: intent_id.clone(),
    });
    let (ticket, disposition, result) = match outcome {
        SubmissionOutcome::Terminal {
            ticket,
            disposition,
            result,
        } => (ticket, Some(disposition), Some(result)),
        SubmissionOutcome::Rejected { ticket, .. } => (ticket, None, None),
        // No ticket exists on these paths; the intent's ticket is still
        // runtime-side and replay classifies it. Nothing to quarantine.
        SubmissionOutcome::CancelSubmitRefused { .. } | SubmissionOutcome::StreamClosed => return,
    };
    st.quarantine(QuarantinedTicket {
        intent_id,
        submission_id,
        ticket,
        disposition,
        result,
        note: note.to_string(),
    });
}

enum CoordCmd<T> {
    SubmitCall {
        ticket: T,
        intent_id: String,
        origin: ToolOrigin,
        call: ToolCall,
        generation: ConnectionGeneration,
        root_epoch: u64,
        deadline: Instant,
        reply: async_channel::Sender<SubmitReply<T>>,
    },
    SubmitCancel {
        intent_id: String,
        deadline: Instant,
        reply: async_channel::Sender<Result<(), CancelSubmitFailure>>,
    },
    PublishRoots {
        origin: ToolOrigin,
        generation: ConnectionGeneration,
        epoch: u64,
        deadline: Instant,
        reply: async_channel::Sender<Result<(), RefusalKind>>,
    },
    ConnectOrigin {
        origin: ToolOrigin,
        deadline: Instant,
        reply: async_channel::Sender<Result<(), RefusalKind>>,
    },
}

enum SubmitReply<T> {
    Enqueued {
        primary: bool,
        waiter: async_channel::Receiver<SubmissionOutcome<T>>,
    },
    Refused(CallSubmitFailure<T>),
}

/// The clonable, command-side-only request surface. Holds the bounded
/// request channel to the coordinator's submission thread plus read-only
/// access to diagnostics, quarantine, and lifecycle state — never the
/// event receiver, never the runtime handle.
pub struct McpFacade<T> {
    cmds: async_channel::Sender<CoordCmd<T>>,
    shared: Arc<Shared<T>>,
}

impl<T: Send + 'static> Clone for McpFacade<T> {
    fn clone(&self) -> Self {
        Self {
            cmds: self.cmds.clone(),
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T: Send + 'static> McpFacade<T> {
    /// Submit one already-authorized, already-intent-logged call.
    ///
    /// The coordinator allocates the [`SubmissionId`], registers the
    /// submission waiter — and, in the same lock section, PROVISIONAL
    /// primary/secondary ownership — BEFORE `submit_until`, and performs
    /// the bounded lossless retry itself — so this returns exactly one
    /// enqueue outcome:
    /// - `Ok(waiter)` — enqueued. `waiter.is_primary()` says whether this
    ///   submission is the intent's accepted primary or a secondary (a
    ///   hard diagnostic; the machinery still routes its rejection).
    /// - `Err(failure)` — finally refused. The waiter was removed
    ///   synchronously (the command never entered the runtime, no
    ///   rejection can arrive) and the COMPLETE command — ticket included —
    ///   returns by move. Nothing cloned, re-minted, or dropped.
    // The refused command (ticket included) travels back by value in the Err
    // variant - that is the contract, not an accident; and the seven
    // parameters (plus self) are the complete call record. Both lints are intentional.
    #[allow(clippy::result_large_err, clippy::too_many_arguments)]
    pub fn submit_call(
        &self,
        deadline: Instant,
        ticket: T,
        intent_id: String,
        origin: ToolOrigin,
        call: ToolCall,
        generation: ConnectionGeneration,
        root_epoch: u64,
    ) -> Result<CallWaiter<T>, CallSubmitFailure<T>> {
        let (reply_tx, reply_rx) = async_channel::bounded(1);
        if self
            .cmds
            .send_blocking(CoordCmd::SubmitCall {
                ticket,
                intent_id,
                origin,
                call,
                generation,
                root_epoch,
                deadline,
                reply: reply_tx,
            })
            .is_err()
        {
            panic!("mcp coordinator submission thread is gone");
        }
        match reply_rx.recv_blocking() {
            Ok(SubmitReply::Enqueued { primary, waiter }) => Ok(CallWaiter {
                primary,
                rx: waiter,
            }),
            Ok(SubmitReply::Refused(failure)) => Err(failure),
            Err(_) => panic!("mcp coordinator submission thread died mid-submit"),
        }
    }

    /// Cancel a submitted call (idempotent, keyed by B2 `intent_id`).
    ///
    /// On final refusal (`Closed`/deadline) the coordinator commits
    /// cancel-submission failure at the single waiter-table lock: a live
    /// primary waiter is woken with the typed, ticket-free
    /// [`SubmissionOutcome::CancelSubmitRefused`], and any later real
    /// terminal for that intent is quarantined — the refusal cannot be
    /// revised and the intent is left unfinalized for replay.
    pub fn submit_cancel(
        &self,
        deadline: Instant,
        intent_id: String,
    ) -> Result<(), CancelSubmitFailure> {
        let (reply_tx, reply_rx) = async_channel::bounded(1);
        if self
            .cmds
            .send_blocking(CoordCmd::SubmitCancel {
                intent_id,
                deadline,
                reply: reply_tx,
            })
            .is_err()
        {
            panic!("mcp coordinator submission thread is gone");
        }
        reply_rx
            .recv_blocking()
            .expect("mcp coordinator submission thread died mid-cancel")
    }

    /// Begin connecting one catalog origin (unit 4b, design r9 §5.3/§5.4).
    /// Idempotent at the runtime: a live origin mints no generation.
    /// Refusal returns the typed kind; `ConnectOrigin` carries no ticket.
    pub fn connect_origin(&self, deadline: Instant, origin: ToolOrigin) -> Result<(), RefusalKind> {
        let (reply_tx, reply_rx) = async_channel::bounded(1);
        if self
            .cmds
            .send_blocking(CoordCmd::ConnectOrigin {
                origin,
                deadline,
                reply: reply_tx,
            })
            .is_err()
        {
            panic!("mcp coordinator submission thread is gone");
        }
        reply_rx
            .recv_blocking()
            .expect("mcp coordinator submission thread died mid-connect")
    }

    /// Publish C1's accepted root-capability epoch for an origin (the
    /// coordinator is the sole submitter, so C1's publication also crosses
    /// here). Refusal returns the typed kind; `UpdateAcceptedRoots`
    /// carries no ticket.
    pub fn publish_accepted_roots(
        &self,
        deadline: Instant,
        origin: ToolOrigin,
        generation: ConnectionGeneration,
        epoch: u64,
    ) -> Result<(), RefusalKind> {
        let (reply_tx, reply_rx) = async_channel::bounded(1);
        if self
            .cmds
            .send_blocking(CoordCmd::PublishRoots {
                origin,
                generation,
                epoch,
                deadline,
                reply: reply_tx,
            })
            .is_err()
        {
            panic!("mcp coordinator submission thread is gone");
        }
        reply_rx
            .recv_blocking()
            .expect("mcp coordinator submission thread died mid-publish")
    }

    /// Hard diagnostics recorded so far, oldest first.
    pub fn diagnostics(&self) -> Vec<CoordinatorDiagnostic> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .diagnostics
            .iter()
            .cloned()
            .collect()
    }

    /// Drain the quarantine record — tickets retained from undeliverable
    /// or post-failure terminal/rejection events, for ticket conservation
    /// and replay reconciliation.
    pub fn take_quarantine(&self) -> Vec<QuarantinedTicket<T>> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .quarantine
            .drain(..)
            .collect()
    }

    /// How many ticket-bearing records quarantine currently retains. The
    /// record is lossless, so this only ever falls when the owner drains
    /// it — a monotonic climb is the operational signal the
    /// [`CoordinatorDiagnostic::QuarantineOverOperationalBound`]
    /// diagnostic names.
    pub fn quarantine_retained(&self) -> usize {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .quarantine
            .len()
    }

    /// Number of live submission waiters (leak checks: a refused or
    /// resolved submission unregisters synchronously).
    pub fn live_submission_waiters(&self) -> usize {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .submissions
            .len()
    }

    /// The app-lifetime observation for `(origin, generation)` — an owned
    /// `Arc` clone of THE value; locks, clones, unlocks; no guard escapes
    /// (ruling B). `Some` only while the stored observation's generation
    /// matches.
    pub fn connected(
        &self,
        origin: &ToolOrigin,
        generation: ConnectionGeneration,
    ) -> Option<Arc<awili_tools::runtime::ConnectedLaunch>> {
        let st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        st.connected
            .get(&origin.origin)
            .filter(|c| c.generation() == generation)
            .cloned()
    }

    /// The pump's CURRENT lifecycle answer for this origin: `Some((g, e))`
    /// iff the origin is `Ready(g, e)` at THIS read, `None` otherwise.
    /// Bare `Copy` values deliberately (ruling E): possession of a
    /// (generation, epoch) pair is not authority anywhere in 4b — every
    /// consumer that acts on liveness performs its own current read.
    pub fn live_generation(&self, origin: &ToolOrigin) -> Option<(ConnectionGeneration, u64)> {
        let st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        match st.origins.get(&origin.origin) {
            Some(OriginStatus::Ready {
                generation,
                accepted_epoch,
                ..
            }) => Some((*generation, *accepted_epoch)),
            _ => None,
        }
    }

    /// Snapshot of the pump's continuously-ingested lifecycle state.
    /// Unconditional CURRENT state — reconciliation uses this, because a
    /// `Dead(g)` origin nobody has touched advances no revision (design r9
    /// §1).
    pub fn lifecycle(&self) -> LifecycleSnapshot {
        let st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        LifecycleSnapshot {
            origins: st.origins.clone(),
            ignored_roots_updates: st.ignored_roots.iter().cloned().collect(),
            revision: st.revision,
        }
    }

    /// Change detection (design r9 §1): `Some(snapshot)` iff the pump's
    /// revision has advanced past `revision`, else `None`.
    pub fn lifecycle_since(&self, revision: u64) -> Option<LifecycleSnapshot> {
        let st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.revision <= revision {
            return None;
        }
        Some(LifecycleSnapshot {
            origins: st.origins.clone(),
            ignored_roots_updates: st.ignored_roots.iter().cloned().collect(),
            revision: st.revision,
        })
    }
}

/// How a `CallFinished` must be committed, decided under the lock from the
/// intent entry's committed state — the classification and the mutation
/// are separate steps so neither borrows the other.
enum TerminalDecision {
    /// A live waiter exists: the pump commits the real terminal NOW — the
    /// single-lock race against cancel-failure commit, which this wins.
    Commit,
    /// Cancel-failure was already committed: quarantine, do not honor.
    Late,
    /// The intent already got its one terminal: quarantine the duplicate.
    Duplicate,
    /// No entry at all: quarantine the unknown terminal.
    Unknown,
}

/// The sole event-consuming pump. Runs on its own dedicated thread for the
/// runtime lifetime; ingests `ServerConnected` / `ServerDied` /
/// `RootsUpdateIgnored` continuously — including while calls block — and
/// commits terminals under the single waiter-table lock.
fn pump_thread<T: Send + 'static>(handle: Arc<McpRuntimeHandle<T>>, shared: Arc<Shared<T>>) {
    while let Some(event) = handle.recv_event_blocking() {
        match event {
            McpEvent::CallFinished {
                ticket,
                intent_id,
                disposition,
                result,
            } => {
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                let decision = match st.intents.get(&intent_id) {
                    Some(entry) if entry.status == IntentStatus::Live && entry.waiter.is_some() => {
                        TerminalDecision::Commit
                    }
                    Some(entry) if entry.status == IntentStatus::Failed => TerminalDecision::Late,
                    Some(_) => TerminalDecision::Duplicate,
                    None => TerminalDecision::Unknown,
                };
                match decision {
                    TerminalDecision::Commit => {
                        let entry = st
                            .intents
                            .get_mut(&intent_id)
                            .expect("decision read the entry");
                        let waiter = entry.waiter.take().expect("decision saw a waiter");
                        entry.status = IntentStatus::Terminal;
                        // The terminal retires ONLY the primary's no-ack
                        // waiter state (r5): secondaries stay live for
                        // their own rejections — the runtime's pending ∪
                        // recently-terminal admission still owes each one a
                        // `SubmissionRejected`.
                        let primary = entry.primary.take();
                        if let Err(outcome) = deliver(
                            &waiter,
                            SubmissionOutcome::Terminal {
                                ticket,
                                disposition,
                                result,
                            },
                        ) {
                            quarantine_undelivered(
                                &mut st,
                                intent_id.clone(),
                                primary,
                                outcome,
                                "terminal waiter abandoned",
                            );
                        }
                        if let Some(primary) = primary {
                            st.submissions.remove(&primary);
                        }
                        st.evict_retired();
                    }
                    TerminalDecision::Late => {
                        st.push_diagnostic(CoordinatorDiagnostic::LateTerminal {
                            intent_id: intent_id.clone(),
                        });
                        st.quarantine(QuarantinedTicket {
                            intent_id,
                            submission_id: None,
                            ticket,
                            disposition: Some(disposition),
                            result: Some(result),
                            note: "terminal after committed cancel-submission failure".to_string(),
                        });
                    }
                    TerminalDecision::Duplicate => {
                        st.push_diagnostic(CoordinatorDiagnostic::DuplicateTerminal {
                            intent_id: intent_id.clone(),
                        });
                        st.quarantine(QuarantinedTicket {
                            intent_id,
                            submission_id: None,
                            ticket,
                            disposition: Some(disposition),
                            result: Some(result),
                            note: "terminal for an already-terminal intent".to_string(),
                        });
                    }
                    TerminalDecision::Unknown => {
                        st.push_diagnostic(CoordinatorDiagnostic::UnknownTerminal {
                            intent_id: intent_id.clone(),
                        });
                        st.quarantine(QuarantinedTicket {
                            intent_id,
                            submission_id: None,
                            ticket,
                            disposition: Some(disposition),
                            result: Some(result),
                            note: "terminal for an intent no live waiter owns".to_string(),
                        });
                    }
                }
            }
            McpEvent::SubmissionRejected {
                ticket,
                intent_id,
                submission_id,
                reason,
            } => {
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                // Each `SubmissionRejected` retires EXACTLY its own
                // submission waiter, delivering the duplicate's returned
                // ticket as a typed failure (r5 retirement rule).
                match st.submissions.remove(&submission_id) {
                    Some(submission) => {
                        if let Err(outcome) = deliver(
                            &submission.waiter,
                            SubmissionOutcome::Rejected { ticket, reason },
                        ) {
                            quarantine_undelivered(
                                &mut st,
                                intent_id.clone(),
                                Some(submission_id),
                                outcome,
                                "rejection waiter abandoned",
                            );
                        }
                        if let Some(entry) = st.intents.get_mut(&intent_id) {
                            entry.secondaries.remove(&submission_id);
                        }
                    }
                    None => {
                        st.push_diagnostic(CoordinatorDiagnostic::RejectionWithoutWaiter {
                            submission_id,
                        });
                        st.quarantine(QuarantinedTicket {
                            intent_id,
                            submission_id: Some(submission_id),
                            ticket,
                            disposition: None,
                            result: None,
                            note: "rejection with no live submission waiter".to_string(),
                        });
                    }
                }
            }
            McpEvent::OriginDisconnected { origin } => {
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                // RC2-ENG4 (F0659): the construction-time "nothing has
                // connected yet" never overwrites a generation's status.
                if st
                    .origins
                    .get(&origin.origin)
                    .and_then(lifecycle_position)
                    .is_none()
                {
                    st.origins
                        .insert(origin.origin.clone(), OriginStatus::Disconnected);
                    st.revision += 1;
                }
            }
            McpEvent::OriginConnecting { origin, generation } => {
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if !lifecycle_applies(st.origins.get(&origin.origin), generation, STEP_CONNECTING) {
                    eprintln!(
                        "awili-mcp-coordinator: stale OriginConnecting for {:?} {generation} ignored",
                        origin.origin
                    );
                    continue;
                }
                st.origins.insert(
                    origin.origin.clone(),
                    OriginStatus::Connecting { generation },
                );
                st.revision += 1;
            }
            McpEvent::ServerConnected(connected) => {
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if !lifecycle_applies(
                    st.origins.get(&connected.origin().origin),
                    connected.generation(),
                    STEP_AWAITING,
                ) {
                    eprintln!(
                        "awili-mcp-coordinator: stale ServerConnected for {:?} {} ignored",
                        connected.origin().origin,
                        connected.generation()
                    );
                    continue;
                }
                let dropped_count = connected.dropped_at_capture().len();
                if dropped_count > 0 {
                    st.push_diagnostic(CoordinatorDiagnostic::ToolsDropped {
                        origin: connected.origin().origin.clone(),
                        count: dropped_count as u32,
                    });
                }
                // Facts only in the snapshot (design r9 §1.1)...
                st.origins.insert(
                    connected.origin().origin.clone(),
                    OriginStatus::AwaitingAcceptance {
                        generation: connected.generation(),
                        attestation: connected.attestation_class(),
                        claimed_server: connected.claimed_server().to_string(),
                        tool_count: connected.tools().len(),
                        dropped_count,
                        roots_epoch: connected.advertised_epoch(),
                    },
                );
                // ...and THE value itself in the Arc side table (design r9
                // §5.1), replacing any earlier generation's entry.
                st.connected
                    .insert(connected.origin().origin.clone(), Arc::new(connected));
                st.revision += 1;
            }
            McpEvent::RootsAccepted {
                origin,
                generation,
                epoch,
            } => {
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                let key = origin.origin.clone();
                match st.origins.get(&key) {
                    Some(OriginStatus::AwaitingAcceptance {
                        generation: g,
                        attestation,
                        ..
                    }) if *g == generation => {
                        let attestation = *attestation;
                        st.origins.insert(
                            key,
                            OriginStatus::Ready {
                                generation,
                                accepted_epoch: epoch,
                                attestation,
                            },
                        );
                        st.revision += 1;
                    }
                    Some(OriginStatus::Ready {
                        generation: g,
                        accepted_epoch,
                        ..
                    }) if *g == generation && epoch <= *accepted_epoch => {
                        // RC2-ENGF4: acceptances can arrive out of order; a
                        // late, older (or repeated) epoch never moves the
                        // reported one back.
                    }
                    Some(OriginStatus::Ready {
                        generation: g,
                        attestation,
                        ..
                    }) if *g == generation => {
                        // Monotonic re-acceptance for the same generation:
                        // record the newer epoch.
                        let attestation = *attestation;
                        st.origins.insert(
                            key,
                            OriginStatus::Ready {
                                generation,
                                accepted_epoch: epoch,
                                attestation,
                            },
                        );
                        st.revision += 1;
                    }
                    other => {
                        // Fail closed: a RootsAccepted for a generation the
                        // pump is not awaiting is traced, never applied
                        // (design r9 §1 monotonicity).
                        eprintln!(
                            "awili-mcp-coordinator: RootsAccepted for {key:?} {generation} \
                             ignored in state {other:?}"
                        );
                    }
                }
            }
            McpEvent::ServerDied {
                origin,
                generation,
                cause,
                selected_class,
                detail,
            } => {
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                // RC2-ENG4 (F0659): an older generation's death never
                // overwrites a newer generation's status (its side-table
                // entry, if still held, is dropped below all the same).
                let applies =
                    lifecycle_applies(st.origins.get(&origin.origin), generation, STEP_DEAD);
                // Dead.attestation comes from selected_class DIRECTLY, never
                // copied from a prior status — correct on every path
                // (design r9 §1.2).
                if applies {
                    st.origins.insert(
                        origin.origin.clone(),
                        OriginStatus::Dead {
                            generation,
                            cause,
                            detail,
                            attestation: selected_class,
                        },
                    );
                } else {
                    eprintln!(
                        "awili-mcp-coordinator: stale ServerDied for {:?} {generation} ignored",
                        origin.origin
                    );
                }
                // The side table drops the observation IFF it is this
                // generation's — a newer generation's value survives an
                // out-of-order death report (design r9 §5.1).
                let removed = st
                    .connected
                    .get(&origin.origin)
                    .is_some_and(|c| c.generation() == generation);
                if removed {
                    st.connected.remove(&origin.origin);
                }
                if applies || removed {
                    st.revision += 1;
                }
            }
            McpEvent::RootsUpdateIgnored {
                origin,
                stale_epoch,
                current_epoch,
            } => {
                // Traced, never silently dropped: a stale security-authority
                // update is operationally meaningful (item 6 pins the full
                // trace contract; here the pump ingests it continuously).
                eprintln!(
                    "awili-mcp-coordinator: stale accepted-roots update ignored for {:?}: \
                     {stale_epoch} <= accepted {current_epoch}",
                    origin.origin
                );
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                while st.ignored_roots.len() >= RECORD_CAP {
                    st.ignored_roots.pop_front();
                }
                st.ignored_roots.push_back(IgnoredRootsUpdate {
                    origin: origin.origin,
                    stale_epoch,
                    current_epoch,
                });
                // A refused security-authority update is a lifecycle
                // mutation: it advances the revision and writes ONLY
                // ignored_roots_updates (design r9 §1).
                st.revision += 1;
            }
        }
    }

    // Stream closure: wake EVERY live submission waiter — primary and
    // secondary — with the typed failure. Started, unfinalized intents are
    // left for replay to classify; no synthetic terminals are fabricated.
    let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    st.closed = true;
    let live = st.submissions.len();
    let waiters: Vec<(String, async_channel::Sender<SubmissionOutcome<T>>)> = st
        .submissions
        .drain()
        .map(|(_, entry)| (entry.intent_id.clone(), entry.waiter))
        .collect();
    for (intent_id, waiter) in waiters {
        if let Err(outcome) = deliver(&waiter, SubmissionOutcome::StreamClosed) {
            quarantine_undelivered(
                &mut st,
                intent_id,
                None,
                outcome,
                "closure waiter abandoned",
            );
        }
    }
    for entry in st.intents.values_mut() {
        entry.waiter = None;
    }
    if live > 0 {
        st.push_diagnostic(CoordinatorDiagnostic::StreamClosedWithLiveWaiters { count: live });
    }
}

/// The coordinator's submission thread: the sole submitter to the runtime
/// (facade requests are serialized here, which is what makes
/// register-before-submit and one-enqueue-outcome sound).
fn submission_thread<T: Send + 'static>(
    handle: Arc<McpRuntimeHandle<T>>,
    shared: Arc<Shared<T>>,
    cmds: async_channel::Receiver<CoordCmd<T>>,
) {
    while let Ok(cmd) = cmds.recv_blocking() {
        match cmd {
            CoordCmd::SubmitCall {
                ticket,
                intent_id,
                origin,
                call,
                generation,
                root_epoch,
                deadline,
                reply,
            } => {
                let outcome = submit_call_impl(
                    &handle, &shared, deadline, ticket, intent_id, origin, call, generation,
                    root_epoch,
                );
                let _ = reply.send_blocking(outcome);
            }
            CoordCmd::SubmitCancel {
                intent_id,
                deadline,
                reply,
            } => {
                let outcome = submit_cancel_impl(&handle, &shared, deadline, intent_id);
                let _ = reply.send_blocking(outcome);
            }
            CoordCmd::ConnectOrigin {
                origin,
                deadline,
                reply,
            } => {
                let result =
                    match handle.submit_until(deadline, McpCommand::ConnectOrigin { origin }) {
                        Ok(()) => Ok(()),
                        Err(refused) => Err(refused.kind),
                    };
                let _ = reply.send_blocking(result);
            }
            CoordCmd::PublishRoots {
                origin,
                generation,
                epoch,
                deadline,
                reply,
            } => {
                let result = match handle.submit_until(
                    deadline,
                    McpCommand::UpdateAcceptedRoots {
                        origin,
                        generation,
                        epoch,
                    },
                ) {
                    Ok(()) => Ok(()),
                    Err(refused) => Err(refused.kind),
                };
                let _ = reply.send_blocking(result);
            }
        }
    }
    // Every facade is gone. Order the runtime to shut down so the event
    // stream closes, the pump's recv unblocks, and the last handle `Arc`
    // can drop (whose `Drop` performs the drain-and-join). The signal MUST
    // be the non-maskable one (item-5 C5 review, finding 3): a best-effort
    // `submit(Shutdown)` alone is refused by a Full command channel, and a
    // runtime whose queue was full then waits forever for another command
    // while this thread's handle `Arc` keeps the sender open — the pump
    // never sees stream closure and `McpCoordinator::drop` never finishes.
    // `signal_shutdown` closes the command sender, which backpressure
    // cannot mask and which the runtime treats as an explicit `Shutdown`.
    handle.signal_shutdown();
}

// Same call record as `submit_call`, plus the handle.
#[allow(clippy::too_many_arguments)]
fn submit_call_impl<T: Send + 'static>(
    handle: &McpRuntimeHandle<T>,
    shared: &Shared<T>,
    deadline: Instant,
    ticket: T,
    intent_id: String,
    origin: ToolOrigin,
    call: ToolCall,
    generation: ConnectionGeneration,
    root_epoch: u64,
) -> SubmitReply<T> {
    // The coordinator is the sole submitter and allocates every
    // SubmissionId — monotonic per coordinator.
    let submission_id = SubmissionId(shared.next_submission.fetch_add(1, Ordering::Relaxed));
    let (waiter_tx, waiter_rx) = async_channel::bounded(1);

    // REGISTER BEFORE SUBMIT (r5 Delta B′) — with PROVISIONAL OWNERSHIP
    // established inside the same lock section, before the command can
    // enter the runtime (item-5 C5 review, finding 1). The role is decided
    // once, here, and written down: a first submission for the intent owns
    // the intent-scoped waiter AND is recorded as the primary; any
    // submission against an intent that already has (or had) a primary is
    // recorded in `secondaries`. Nothing is left to post-submit
    // bookkeeping, so a `CallFinished` the runtime emits the instant the
    // command lands still finds `primary == Some(..)` — it can no longer
    // observe `primary == None` for an accepted submission and mis-file
    // the terminal — and a fast `SubmissionRejected` can no longer retire
    // a submission entry the submit thread has not yet classified.
    let command = McpCommand::CallTool {
        ticket,
        intent_id: intent_id.clone(),
        submission_id,
        origin,
        call,
        generation,
        root_epoch,
    };
    let primary = {
        let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        // RC2-ENG4 (F0658): the pump already woke every waiter at stream
        // closure; a waiter registered now would never be woken. Refuse as
        // Closed — the command never entered the runtime, so the caller gets
        // it back whole, ticket included.
        if st.closed {
            return SubmitReply::Refused(CallSubmitFailure {
                kind: RefusalKind::Closed,
                command,
            });
        }
        let primary = match st.intents.get_mut(&intent_id) {
            // The intent already has — or already had — its one primary
            // path: a live intent with an open intent-waiter, or one
            // retired by its terminal / cancel failure / stream closure.
            // This submission is a prospective secondary either way.
            Some(entry) => {
                entry.secondaries.insert(submission_id);
                false
            }
            // First submission for the intent: THIS one owns the intent
            // waiter — the table reserved exclusively for the single
            // `CallFinished` — provisionally, from before submit.
            None => {
                st.intents.insert(
                    intent_id.clone(),
                    IntentEntry {
                        waiter: Some(waiter_tx.clone()),
                        status: IntentStatus::Live,
                        primary: Some(submission_id),
                        secondaries: HashSet::new(),
                    },
                );
                st.intent_order.push_back(intent_id.clone());
                st.evict_retired();
                true
            }
        };
        st.submissions.insert(
            submission_id,
            SubmissionEntry {
                waiter: waiter_tx,
                intent_id: intent_id.clone(),
            },
        );
        primary
    };
    match handle.submit_until(deadline, command) {
        Ok(()) => {
            // One enqueue outcome: enqueued, with the role already
            // recorded. A secondary is a coordinator invariant violation in
            // normal unit-4 flow — recorded as a hard diagnostic here,
            // after the enqueue it describes — while the machinery still
            // handles it honestly.
            if !primary {
                let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                st.push_diagnostic(CoordinatorDiagnostic::SecondarySubmission {
                    intent_id: intent_id.clone(),
                    submission_id,
                });
            }
            SubmitReply::Enqueued {
                primary,
                waiter: waiter_rx,
            }
        }
        Err(refused) => {
            // Synchronous refusal cleanup: the command never entered the
            // runtime, so no rejection can arrive for it. Roll the
            // provisional ownership back under the same lock — the
            // submission entry, and either the intent entry this
            // submission created (a provisional primary's entry has no
            // other live submissions: submissions are serialized on this
            // thread, so no secondary can predate a primary) or this
            // submission's `secondaries` membership.
            let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            st.submissions.remove(&submission_id);
            if primary {
                let owns_entry = st
                    .intents
                    .get(&intent_id)
                    .map(|e| {
                        e.status == IntentStatus::Live
                            && e.waiter.is_some()
                            && e.primary == Some(submission_id)
                    })
                    .unwrap_or(false);
                if owns_entry {
                    st.intents.remove(&intent_id);
                }
            } else if let Some(entry) = st.intents.get_mut(&intent_id) {
                entry.secondaries.remove(&submission_id);
            }
            SubmitReply::Refused(CallSubmitFailure {
                kind: refused.kind,
                command: refused.command,
            })
        }
    }
}

fn submit_cancel_impl<T: Send + 'static>(
    handle: &McpRuntimeHandle<T>,
    shared: &Shared<T>,
    deadline: Instant,
    intent_id: String,
) -> Result<(), CancelSubmitFailure> {
    match handle.submit_until(
        deadline,
        McpCommand::Cancel {
            intent_id: intent_id.clone(),
        },
    ) {
        Ok(()) => Ok(()),
        Err(refused) => {
            let kind = refused.kind;
            // THE cancel/terminal commit point: the same waiter-table lock
            // the pump commits terminals under. If the pump committed the
            // real terminal first, the waiter is already gone and this is
            // absorbed — the refusal cannot revise a committed terminal.
            // If the waiter is still live, failure commits NOW: the waiter
            // is woken with the typed ticket-free failure, and any later
            // terminal finds no waiter and is quarantined (r3 Delta 3).
            let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let live_waiter = st
                .intents
                .get(&intent_id)
                .map(|e| e.status == IntentStatus::Live && e.waiter.is_some())
                .unwrap_or(false);
            let already_terminal = st
                .intents
                .get(&intent_id)
                .map(|e| e.status == IntentStatus::Terminal)
                .unwrap_or(false);
            if live_waiter {
                let entry = st
                    .intents
                    .get_mut(&intent_id)
                    .expect("live_waiter read the entry");
                let waiter = entry.waiter.take().expect("live_waiter saw a waiter");
                entry.status = IntentStatus::Failed;
                let primary = entry.primary.take();
                if let Err(outcome) =
                    deliver(&waiter, SubmissionOutcome::CancelSubmitRefused { kind })
                {
                    quarantine_undelivered(
                        &mut st,
                        intent_id.clone(),
                        primary,
                        outcome,
                        "cancel-refusal waiter abandoned",
                    );
                }
                if let Some(primary) = primary {
                    st.submissions.remove(&primary);
                }
            } else if already_terminal {
                st.push_diagnostic(CoordinatorDiagnostic::CancelRefusalAfterTerminal {
                    intent_id,
                    kind,
                });
            }
            Err(CancelSubmitFailure { kind })
        }
    }
}

/// The MCP coordinator: sole owner of the runtime handle, sole consumer of
/// the event stream. Construct one per runtime lifetime and hand out
/// clonable [`McpFacade`]s; nothing else ever touches the runtime.
///
/// Dropping the coordinator (and every facade) orders the runtime to shut
/// down; the pump drains to stream closure and the handle's own
/// drain-and-join `Drop` reaps the runtime thread.
pub struct McpCoordinator<T> {
    facade: McpFacade<T>,
    _shared: Arc<Shared<T>>,
    _pump: Option<std::thread::JoinHandle<()>>,
    _submission: Option<std::thread::JoinHandle<()>>,
}

impl<T: Send + 'static> McpCoordinator<T> {
    /// Take ownership of the runtime handle and start the dedicated pump
    /// and submission threads. The handle's `Arc` moves into those threads
    /// — after this call not even the coordinator struct can receive an
    /// event or submit a command except through its own facade.
    pub fn new(handle: McpRuntimeHandle<T>) -> Self {
        let handle = Arc::new(handle);
        let (tx, rx) = async_channel::bounded(COORD_CMD_CAPACITY);
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new()),
            next_submission: AtomicU64::new(0),
        });
        let pump = std::thread::Builder::new()
            .name("awili-mcp-pump".into())
            .spawn({
                let handle = Arc::clone(&handle);
                let shared = Arc::clone(&shared);
                move || pump_thread(handle, shared)
            })
            .expect("spawn awili-mcp-pump thread");
        let submission = std::thread::Builder::new()
            .name("awili-mcp-coord".into())
            .spawn({
                let handle = Arc::clone(&handle);
                let shared = Arc::clone(&shared);
                move || submission_thread(handle, shared, rx)
            })
            .expect("spawn awili-mcp-coord thread");
        Self {
            facade: McpFacade {
                cmds: tx,
                shared: Arc::clone(&shared),
            },
            _shared: shared,
            _pump: Some(pump),
            _submission: Some(submission),
        }
    }

    /// A clonable, command-side-only facade onto this coordinator.
    pub fn facade(&self) -> McpFacade<T> {
        self.facade.clone()
    }
}

impl<T> Drop for McpCoordinator<T> {
    fn drop(&mut self) {
        // Deterministic teardown, and the reason the coordinator owns the
        // runtime's lifetime: closing the request channel (idempotent, and
        // effective even if a facade clone is still out) ends the
        // submission thread, which signals shutdown through the
        // NON-maskable order — best-effort `Shutdown`, then command-sender
        // closure — so a full runtime command queue cannot strand the
        // pump; the runtime then drains, reaps its server children, and
        // closes the event stream, which is what unblocks the pump. Joining
        // both threads here — instead of detaching them — is what makes the
        // reap deterministic: a detached teardown would race process exit
        // and could orphan server children.
        self.facade.cmds.close();
        if let Some(join) = self._submission.take() {
            let _ = join.join();
        }
        if let Some(join) = self._pump.take() {
            let _ = join.join();
        }
    }
}

// ============================================================================
// The executor seam (unit 4 item 6)
// ============================================================================

/// Deadline for `publish_roots` — bring-up publication is not
/// latency-critical, but it is still lossless-bounded like every other
/// submit.
const PUBLISH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Unit 4 item 6: the production facade IS the seam's MCP executor. The
/// seam cannot name this type (it is defined here, above it in the
/// dependency direction), so it defines the [`McpExecutor`] surface and
/// THIS crate implements it — the closed executor enum's `Mcp` variant
/// then holds this facade, and nothing else: the trait exposes no runtime
/// handle, no event receiver, and no lifecycle view.
impl awili_seam::McpExecutor for McpFacade<awili_engine_log::IntentTicket> {
    fn dispatch_call(
        &self,
        dispatch: awili_seam::McpDispatch,
    ) -> Result<Box<dyn awili_seam::McpCall + '_>, awili_seam::McpSubmitRefused> {
        let awili_seam::McpDispatch {
            deadline,
            ticket,
            intent_id,
            origin,
            call,
            generation,
            root_epoch,
        } = dispatch;
        match self.submit_call(
            deadline,
            ticket,
            intent_id.clone(),
            origin,
            call,
            generation,
            root_epoch,
        ) {
            Ok(waiter) => Ok(Box::new(FacadeCall {
                facade: self,
                intent_id,
                waiter,
            })),
            // Final submit refusal. The complete command — the non-`Clone`
            // ticket included — came back by move inside `failure.command`.
            // The ticket is MOVED ON to the seam's typed failure, which is
            // then its last owner. Dropping it here and calling that
            // "conserved" was the item-6 review's first P1: the command
            // never entered the runtime, so nothing else can ever hold it.
            Err(failure) => {
                let ticket = match failure.command {
                    McpCommand::CallTool { ticket, .. } => ticket,
                    // `submit_call` only ever refuses the CallTool it was
                    // given; any other shape here is a coordinator bug, and
                    // losing the ticket silently is exactly what this arm
                    // exists to prevent.
                    other => unreachable!(
                        "submit_call refused a non-CallTool command: {:?}",
                        std::mem::discriminant(&other)
                    ),
                };
                Err(awili_seam::McpSubmitRefused {
                    kind: failure.kind,
                    ticket,
                })
            }
        }
    }

    fn publish_roots(
        &self,
        origin: ToolOrigin,
        generation: ConnectionGeneration,
        epoch: u64,
    ) -> Result<(), RefusalKind> {
        self.publish_accepted_roots(Instant::now() + PUBLISH_DEADLINE, origin, generation, epoch)
    }
}

/// One live call as the seam sees it (unit 4 item 6): the coordinator
/// waiter plus the facade needed to submit this intent's `Cancel`. Holds no
/// runtime handle and no event receiver — the executor surface stays as
/// narrow as it was when it only knew how to block.
struct FacadeCall<'a> {
    facade: &'a McpFacade<awili_engine_log::IntentTicket>,
    intent_id: String,
    waiter: CallWaiter<awili_engine_log::IntentTicket>,
}

impl awili_seam::McpCall for FacadeCall<'_> {
    fn poll_outcome(&mut self, deadline: Instant) -> Option<awili_seam::McpDispatchOutcome> {
        self.waiter
            .wait_deadline(deadline)
            .map(|outcome| match outcome {
                SubmissionOutcome::Terminal {
                    ticket,
                    disposition,
                    result,
                } => awili_seam::McpDispatchOutcome::Terminal {
                    ticket,
                    disposition,
                    result,
                },
                SubmissionOutcome::Rejected { ticket, reason } => {
                    awili_seam::McpDispatchOutcome::Rejected { ticket, reason }
                }
                SubmissionOutcome::CancelSubmitRefused { kind } => {
                    awili_seam::McpDispatchOutcome::CancelSubmitRefused { kind }
                }
                SubmissionOutcome::StreamClosed => awili_seam::McpDispatchOutcome::StreamClosed,
            })
    }

    fn cancel(&mut self, deadline: Instant) -> Result<(), RefusalKind> {
        // `Cancel` is keyed by the B2 intent_id and carries no ticket, so a
        // refusal here is ticket-free by construction. On refusal the
        // coordinator commits at the waiter table and wakes this waiter with
        // CancelSubmitRefused — which the seam drains through poll_outcome.
        self.facade
            .submit_cancel(deadline, self.intent_id.clone())
            .map_err(|f| f.kind)
    }
}

#[cfg(test)]
mod rc2_eng4_tests {
    use super::*;

    /// RC2-ENG4 (F0660): an id whose intent entry is already gone (a refused
    /// submission removed it) leaves the eviction queue; before, eviction
    /// re-queued it forever and the queue never shrank.
    #[test]
    fn rc2eng4_eviction_drops_ids_whose_entry_is_gone() {
        let mut st = State::<()>::new();
        for i in 0..RETIRED_INTENT_CAP + 10 {
            st.intent_order.push_back(format!("gone-{i}"));
        }
        st.evict_retired();
        assert!(
            st.intent_order.len() <= RETIRED_INTENT_CAP,
            "{} ids left",
            st.intent_order.len()
        );
    }
}
