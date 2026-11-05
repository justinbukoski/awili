//! C2-u2/u3: the engine-owned MCP runtime thread (contract §2/§5/§6-Q5).
//!
//! One dedicated OS thread hosts a **current-thread Tokio runtime** with
//! every rmcp client inside it. The boundary vocabulary — commands,
//! events, the handle — lives in `awili_tools::runtime`, a crate that
//! cannot name tokio or rmcp (crate-level INV-C2-1).
//!
//! ## Concurrency shape (u3: connect lives in the task)
//!
//! The loop `select!`s over command intake, dispatch-task completion, and
//! housekeeping. Session CONNECT happens inside the dispatch task behind a
//! per-origin async mutex (u2's inline connect — which blocked command
//! intake — is gone), so the loop is always responsive: cancellation and
//! shutdown are processable at any moment, including mid-connect.
//!
//! ## Cancellation (§5.1/§5.2) — join-before-synthesis
//!
//! `Cancel { intent_id }` marks the pending call and aborts its task. The
//! terminal is synthesized ONLY from the join result:
//! - `Ok(TaskDone)` — the real result ALWAYS wins (tokio abort cannot
//!   replace a finished task's output). No synthetic terminal ever races
//!   a queued completion (§5.2 race 3).
//! - `Err(cancelled)` + uncommitted — the call provably never touched the
//!   transport: `CancelledBeforeDispatch` (cancel) or `NeverDispatched`
//!   (shutdown), both Definite.
//! - `Err(cancelled)` + committed — `InFlightUnresolved` (Uncertain).
//!
//! A cancel that precedes its CallTool is caught by a BOUNDED tombstone
//! set (FIFO command order makes this deterministic); a cancel for an
//! unknown or already-terminal intent is absorbed (idempotent).
//!
//! ## Root epochs (§6 Q5) — refuse-until-published
//!
//! C1 publishes accepted epochs via `UpdateAcceptedRoots` (monotonic;
//! lower updates are refused LOUDLY via `RootsUpdateIgnored`, equal ones
//! are idempotent). A `CallTool` must carry the origin's exact published
//! epoch; an origin C1 never published for refuses everything — there is
//! no implicit epoch-0 authority. Server advertisements
//! (`ServerConnected.roots_epoch`, from `_meta["awili/rootsEpoch"]`) are
//! PROPOSALS and never mutate the accepted table: C2 emits → C1
//! re-accepts → C3 enforces.
//!
//! ## Ticket accounting (unchanged from u2, extended to u3 paths)
//!
//! Every ticket that enters comes back exactly once: through the
//! intent's single `CallFinished` (completion, cancel at any phase,
//! panic, refusal — shutdown / tombstone / epoch / catalog / connect
//! failure — or shutdown drain), EXCEPT a duplicate submission against a
//! live OR already-terminal intent (within the bounded recent-terminal
//! window), whose ticket returns via the non-terminal
//! `SubmissionRejected` — with its own `SubmissionId` — so the
//! original's one terminal stays unique (INV-C2-5 is per-intent). The
//! same bounded recently-terminal set absorbs late `Cancel`: one
//! structure, two consumers (unit 4 r6 Delta B″). The one admitted loss
//! path is catastrophic
//! death of this thread; B2 replay classifies those intents Uncertain —
//! the truth.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use awili_engine::events::ToolResultRecord;
use awili_engine::generation::ConnectionGeneration;
use awili_engine::AwiliCallId;
use awili_tools::attest::AttestationClass;
use awili_tools::runtime::{
    event_channel, CallDisposition, DeathCause, McpCommand, McpEvent, McpEventSink,
    McpRuntimeHandle,
};
use awili_tools::ToolOrigin;
use std::sync::Arc;

use crate::mcp_transport::{
    DispatchVerdict, McpServerCatalog, Session, CALL_TIMEOUT, CONNECT_TIMEOUT,
};

/// Test-only synchronization probe around the LOOP's own event sends
/// (Codex u3 rounds 3–4). `blocked_on_full` increments ONLY when the
/// loop's own `try_send` positively observed a full channel — the wedge
/// condition itself, not a timing inference; a descheduled-but-unblocked
/// loop can never produce it. `completed` counts delivered loop sends
/// (either path) for the causal-unwedge check.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct LoopSendProbe {
    pub(crate) blocked_on_full: std::sync::atomic::AtomicU64,
    pub(crate) completed: std::sync::atomic::AtomicU64,
}

/// Test-only deterministic lever at the REAL runtime boundary (the
/// f3a386d HostHooks discipline, and the `send_probe` precedent: the
/// type and its config field are `cfg(test)`, so a normal runtime
/// carries no lever by construction). It exists for one proof — the
/// binding r6 Delta B″ coordinator test,
/// `terminal_before_duplicate_rejection_retires_only_the_primary`:
///
/// - the loop, having just CONSUMED and dispatched its first admitted
///   `CallTool` (the original), parks on this gate holding BOTH further
///   command consumption AND completion processing, so the test can
///   enqueue the duplicate against a runtime that provably cannot move
///   either way — both submissions are in the command channel before
///   the original's completion is processed, by construction, not
///   timing;
/// - the test's single release lets completions flow again while
///   command consumption stays held; the loop then processes the
///   original's REAL completion and emits its ONE `CallFinished`, and
///   only that emission lifts the hold — so the queued duplicate is
///   necessarily consumed AFTER the terminal, by the real
///   `pending ∪ recently_terminal` admission rule.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct LoopGate {
    /// Loop→test, stored once — exactly when the first admitted
    /// `CallTool` engages the hold. No await point sits between that
    /// store and the loop's park, so observing it positively observes
    /// "the original was consumed and dispatched, and the loop is (or is
    /// about to be, with nothing runnable in between) parked".
    engaged: std::sync::atomic::AtomicBool,
    /// Test→loop: the one release — "process the completion now".
    release_tx: async_channel::Sender<()>,
    release_rx: async_channel::Receiver<()>,
}

/// The lever's phase machine — loop-thread-local state, so plain
/// program order (no atomics) governs every transition.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopGatePhase {
    /// No admitted call yet (or no gate configured): the loop runs free.
    Armed,
    /// The first `CallTool` was consumed and dispatched: the loop parks
    /// BEFORE its select — no command consumed, no completion processed.
    HoldAll,
    /// Completions flow; command intake stays held until the original's
    /// `CallFinished` has been emitted.
    HoldCommands,
    /// The terminal was emitted: normal loop behavior, forever after.
    Released,
}

/// Failure hygiene for the park below — a test that dies before its
/// release must not park the runtime (and the coordinator's teardown)
/// forever. On a healthy run the release arrives in microseconds; the
/// watchdog can only fire in an already-failing test.
#[cfg(test)]
const GATE_PARK_WATCHDOG: Duration = Duration::from_secs(10);

#[cfg(test)]
impl LoopGate {
    pub(crate) fn new() -> Self {
        let (release_tx, release_rx) = async_channel::bounded(1);
        Self {
            engaged: std::sync::atomic::AtomicBool::new(false),
            release_tx,
            release_rx,
        }
    }

    /// Loop-side: engage the hold (called at the end of the first
    /// admitted `CallTool` arm).
    fn engage(&self) {
        self.engaged
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Test-side fence input: has the loop consumed and dispatched the
    /// original (and thereby parked) yet?
    pub(crate) fn engaged(&self) -> bool {
        self.engaged.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Test-side: the single release — completion processing resumes;
    /// command consumption stays held until the original's
    /// `CallFinished` has been emitted.
    pub(crate) fn release(&self) {
        let _ = self.release_tx.send_blocking(());
    }
}

#[cfg(test)]
impl Default for LoopGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Bounded channel capacities and deadlines (public fields — config, not
/// policy).
#[derive(Clone, Debug)]
pub struct McpRuntimeConfig {
    pub command_capacity: usize,
    pub event_capacity: usize,
    pub connect_timeout: Duration,
    pub call_timeout: Duration,
    /// How long shutdown waits for in-flight calls before aborting them
    /// into `InFlightUnresolved`.
    pub shutdown_grace: Duration,
    /// Override for the attested-servers control-plane file (unit 4b).
    /// `None` = the default `~/.config/awili/attested-servers.toml`; tests
    /// point it at a fixture. An absent FILE is not an error (every origin
    /// selects Unverified); an absent PATH override just means default.
    pub setup_path: Option<std::path::PathBuf>,
    /// Test-only: observes entry/exit of the loop's RootsUpdateIgnored
    /// event sends, so a test can PROVE the loop is inside a blocked send.
    #[cfg(test)]
    pub(crate) send_probe: Option<std::sync::Arc<LoopSendProbe>>,
    /// Test-only: the deterministic park lever for the binding r6
    /// terminal-before-queued-duplicate coordinator proof (see
    /// [`LoopGate`]). Absent from normal builds, and `None` in every
    /// test that does not opt in.
    #[cfg(test)]
    pub(crate) gate: Option<std::sync::Arc<LoopGate>>,
}

impl Default for McpRuntimeConfig {
    fn default() -> Self {
        Self {
            command_capacity: 64,
            event_capacity: 64,
            connect_timeout: CONNECT_TIMEOUT,
            call_timeout: CALL_TIMEOUT,
            shutdown_grace: Duration::from_secs(5),
            setup_path: None,
            #[cfg(test)]
            send_probe: None,
            #[cfg(test)]
            gate: None,
        }
    }
}

/// Cap on remembered cancel-before-CallTool intent ids. Bounded so an
/// attacker (or bug) spraying unknown cancels cannot grow memory without
/// limit; FIFO eviction (Codex u3 design review, blocking change 5).
const TOMBSTONE_CAP: usize = 1024;

/// Cap on remembered already-terminal intent ids (late-cancel absorption).
const RECENT_TERMINAL_CAP: usize = 4096;

/// Bounded FIFO id set (tombstones, recently-terminal memory).
struct BoundedIdSet {
    set: std::collections::HashSet<String>,
    order: std::collections::VecDeque<String>,
    cap: usize,
}

impl BoundedIdSet {
    fn new(cap: usize) -> Self {
        Self {
            set: std::collections::HashSet::new(),
            order: std::collections::VecDeque::new(),
            cap,
        }
    }

    fn insert(&mut self, intent_id: String) {
        if self.set.contains(&intent_id) {
            // REFRESH the FIFO position (Codex u3 round 2, P1): a reused
            // id that just went terminal again must age from NOW, or it
            // can be evicted immediately and a late cancel would poison
            // the next reuse.
            self.order.retain(|x| x != &intent_id);
            self.order.push_back(intent_id);
            return;
        }
        while self.set.len() >= self.cap {
            if let Some(evicted) = self.order.pop_front() {
                self.set.remove(&evicted);
            } else {
                break;
            }
        }
        self.order.push_back(intent_id.clone());
        self.set.insert(intent_id);
    }

    fn contains(&self, intent_id: &str) -> bool {
        self.set.contains(intent_id)
    }

    /// Consume an id if present.
    fn take(&mut self, intent_id: &str) -> bool {
        if self.set.remove(intent_id) {
            self.order.retain(|x| x != intent_id);
            true
        } else {
            false
        }
    }

    #[allow(dead_code)] // size query kept for the bounded-set invariant checks
    fn len(&self) -> usize {
        self.set.len()
    }
}

type SessionSlot = Rc<tokio::sync::Mutex<Option<Rc<Session>>>>;
type Sessions = Rc<RefCell<HashMap<String, SessionSlot>>>;

/// Spawn the engine-owned MCP runtime thread. Returns the canonical-only
/// handle; the thread owns everything else.
pub fn spawn_mcp_runtime<T: Send + 'static>(
    catalog: McpServerCatalog,
    config: McpRuntimeConfig,
) -> McpRuntimeHandle<T> {
    let (cmd_tx, cmd_rx) = async_channel::bounded::<McpCommand<T>>(config.command_capacity);
    // The one sink/stream pair (unit 4b, design r9 §2.2): the sink is the
    // sole issuer onto this stream; the handle is assembled from the
    // opaque stream half.
    let (evt_tx, evt_rx) = event_channel::<T>(config.event_capacity);
    let join = std::thread::Builder::new()
        .name("awili-mcp-runtime".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => {
                    let local = tokio::task::LocalSet::new();
                    local.block_on(&runtime, run_loop(catalog, config, cmd_rx, evt_tx));
                }
                Err(err) => {
                    // Runtime construction failure: fail every queued command
                    // closed with DEFINITE terminals (nothing was dispatched),
                    // then close both channels.
                    cmd_rx.close();
                    while let Ok(cmd) = cmd_rx.try_recv() {
                        if let McpCommand::CallTool {
                            ticket,
                            intent_id,
                            call,
                            ..
                        } = cmd
                        {
                            let _ = evt_tx.send_blocking(McpEvent::CallFinished {
                                ticket,
                                intent_id,
                                disposition: CallDisposition::NeverDispatched,
                                result: ToolResultRecord {
                                    awili_call_id: call.awili_call_id,
                                    content: format!(
                                        "MCP runtime failed to start (tokio build: {err}); \
                                     call was never dispatched."
                                    ),
                                    is_error: true,
                                },
                            });
                        }
                    }
                }
            }
        })
        .expect("spawn awili-mcp-runtime thread");
    McpRuntimeHandle::from_parts(cmd_tx, evt_rx, join)
}

/// Loop-owned state for one in-flight call. The ticket lives HERE, never
/// in the dispatch future — a panicking or aborted dispatch cannot
/// destroy it.
struct PendingCall<T> {
    ticket: T,
    intent_id: String,
    awili_call_id: AwiliCallId,
    origin: ToolOrigin,
    /// Set true at the task's COMMIT POINT (first poll reaching the
    /// pre-call check). Single-thread program order makes this Cell
    /// synchronizing; see the u2 review record.
    committed: Rc<Cell<bool>>,
    /// A Cancel was processed for this call (distinguishes cancel-abort
    /// from shutdown-abort when the join reports `cancelled`).
    cancel_requested: Rc<Cell<bool>>,
    /// The EXACT session slot this call dispatched against. Teardown
    /// identity-checks this (Rc::ptr_eq) so a stale failure can never
    /// kill a replacement session (Codex u3 round 1, P0-1: ABA).
    slot: SessionSlot,
    abort: tokio::task::AbortHandle,
}

/// What a dispatch task reports back. Deliberately ticket-free.
struct TaskDone {
    intent_id: String,
    verdict: DispatchVerdict,
    /// The origin AND the exact slot the failed call ran on.
    broke_session: Option<(ToolOrigin, SessionSlot)>,
}

/// The dispatch task: session acquire (connect if first), commit point,
/// call. Runs concurrently with the loop and with other dispatches; a
/// hung connect or call blocks nothing but itself.
async fn dispatch_task(
    slot: SessionSlot,
    origin: ToolOrigin,
    call: awili_engine::ToolCall,
    intent_id: String,
    committed: Rc<Cell<bool>>,
    shutting_down: Rc<Cell<bool>>,
    call_timeout: Duration,
) -> TaskDone {
    // Session acquire: admission (the LOOP) admitted this call only in
    // Ready, and Ready is set only after a successful connect populated
    // the slot — so the dispatch task never connects (unit 4b: connection
    // is ConnectOrigin's job). An empty slot here is a death race: the
    // session was torn down between admission and this lock. No byte was
    // sent — refuse DEFINITE.
    let guard = slot.lock().await;
    let session = match guard.as_ref() {
        Some(s) => {
            let s = Rc::clone(s);
            drop(guard);
            s
        }
        None => {
            return TaskDone {
                intent_id,
                verdict: DispatchVerdict::never_dispatched(
                    call.awili_call_id.clone(),
                    "session torn down between admission and dispatch; never sent.",
                ),
                broke_session: None,
            };
        }
    };

    // COMMIT POINT: past this check the call is treated as dispatched.
    if shutting_down.get() {
        return TaskDone {
            intent_id,
            verdict: DispatchVerdict::never_dispatched(
                call.awili_call_id.clone(),
                "shutdown before dispatch commit; call never ran.",
            ),
            broke_session: None,
        };
    }
    committed.set(true);

    // Test-only panic injection AFTER the commit point: a mid-dispatch
    // panic (session live, request notionally in flight) — the case the
    // fault gate cannot reach. Loop-owned state must recover the ticket,
    // tear the session down, and classify InFlightUnresolved.
    #[cfg(test)]
    if call.name == "__awili_test_panic" {
        panic!("test-injected dispatch panic");
    }

    let (verdict, broke) = session
        .call(
            call.awili_call_id.clone(),
            &call.name,
            &call.arguments,
            call_timeout,
        )
        .await;
    TaskDone {
        intent_id,
        verdict,
        broke_session: broke.then_some((origin, slot)),
    }
}

/// The runtime's OWN lifecycle authority for one origin (unit 4b, design
/// r9 §1): the producer-side mirror of the pump's `OriginStatus`, used for
/// admission (a call is admitted only in `Ready` under the exact accepted
/// epoch) and for revoke-before-emit on death (row 9). Facts only.
#[derive(Debug, Clone, Copy, PartialEq)]
enum RuntimeOriginState {
    Disconnected,
    Connecting {
        generation: ConnectionGeneration,
    },
    AwaitingAcceptance {
        generation: ConnectionGeneration,
        class: AttestationClass,
    },
    Ready {
        generation: ConnectionGeneration,
        accepted_epoch: u64,
        class: AttestationClass,
    },
    Dead {
        generation: ConnectionGeneration,
    },
}

/// Drop the runtime's Ready/Awaiting authority for one origin, BEFORE its
/// death event is emitted (row 9): once this returns, admission refuses.
/// Returns the dying generation and its retained Copy class; `None` when
/// no live generation exists (already Dead/Disconnected — nothing to
/// revoke, no event owed by this caller).
fn revoke_origin(
    origin_states: &mut HashMap<String, RuntimeOriginState>,
    key: &str,
) -> Option<(ConnectionGeneration, Option<AttestationClass>)> {
    let (generation, class) = match origin_states.get(key) {
        Some(RuntimeOriginState::Ready {
            generation, class, ..
        })
        | Some(RuntimeOriginState::AwaitingAcceptance { generation, class }) => {
            (*generation, Some(*class))
        }
        Some(RuntimeOriginState::Connecting { generation }) => (*generation, None),
        _ => return None,
    };
    origin_states.insert(key.to_string(), RuntimeOriginState::Dead { generation });
    Some((generation, class))
}

/// RC2-APP (F0202): a connect task ended without its [`ConnectDone`] (it
/// panicked or was cancelled). If its origin is still `Connecting` under
/// THAT generation, the origin is `Dead` now (so `ConnectOrigin` mints a
/// fresh generation instead of only re-announcing a connect nobody runs).
/// Returns whether the caller owes the `ServerDied` event. A newer
/// generation, or a state the loop already moved on, is left alone.
fn settle_lost_connect(
    origin_states: &mut HashMap<String, RuntimeOriginState>,
    key: &str,
    generation: ConnectionGeneration,
) -> bool {
    if origin_states.get(key) != Some(&RuntimeOriginState::Connecting { generation }) {
        return false;
    }
    origin_states.insert(key.to_string(), RuntimeOriginState::Dead { generation });
    true
}

/// RC2-APP (F0204): how long shutdown lets the kills of aborted connect
/// attempts' children run before the runtime goes away.
const CONNECT_ABORT_SETTLE: Duration = Duration::from_millis(100);

/// One finished connect attempt, applied by the LOOP (state mutation and
/// event emission never happen inside the connect task itself).
struct ConnectDone {
    origin: ToolOrigin,
    generation: ConnectionGeneration,
    slot: SessionSlot,
    result: Result<crate::mcp_transport::ConnectSuccess, crate::mcp_transport::ConnectFailure>,
}

async fn run_loop<T: Send + 'static>(
    catalog: McpServerCatalog,
    config: McpRuntimeConfig,
    cmd_rx: async_channel::Receiver<McpCommand<T>>,
    evt_tx: McpEventSink<T>,
) {
    // The sink is not Clone; per-call tasks share it as Arc — the
    // Arc::clone below replaces the raw evt_tx.clone() (design r9 §2.2).
    let evt_tx = Arc::new(evt_tx);
    // Generations come from the PROCESS-long allocator (unit 5 r8 §2,
    // ruling 41): every runtime in the process shares one monotonic
    // source, so a respawned worker can never re-mint a predecessor's
    // generation and cross-worker generation comparisons are sound.
    let alloc = awili_engine::generation::process();
    // The runtime's own per-origin lifecycle authority (admission +
    // revoke-before-emit). Loop-owned; mutated only on this thread.
    let mut origin_states: HashMap<String, RuntimeOriginState> = HashMap::new();
    // The attested-servers control-plane PATH (impl review r1 P1-4,
    // ruling H): records are loaded FRESH PER GENERATION inside each
    // connect attempt, so replacing or revoking an installed record while
    // the worker lives affects the very next generation. Construction
    // surfaces the current problems once; the per-generation load is the
    // authority.
    let setup_path: Rc<Option<std::path::PathBuf>> = Rc::new(
        config
            .setup_path
            .clone()
            .or_else(awili_attest::setup_record::default_setup_path),
    );
    if let Some(p) = setup_path.as_ref() {
        for problem in awili_attest::setup_record::load_setup_records(p).problems() {
            eprintln!("awili-mcp-runtime: {problem}");
        }
    }
    // In-flight connect attempts; outcomes applied in the select loop.
    let mut connect_tasks: tokio::task::JoinSet<ConnectDone> = tokio::task::JoinSet::new();
    // RC2-APP (F0202): which origin and generation each connect task is for,
    // so a task that dies without its `ConnectDone` (a panic) still settles
    // its origin instead of leaving it `Connecting` forever.
    let mut connect_meta: HashMap<tokio::task::Id, (ToolOrigin, ConnectionGeneration)> =
        HashMap::new();
    // Emitted once per catalog origin at construction (design r9 §1).
    for key in catalog.origins() {
        origin_states.insert(key.clone(), RuntimeOriginState::Disconnected);
        let _ = evt_tx
            .emit_origin_disconnected(ToolOrigin::new(key.clone()))
            .await;
    }
    let shutting_down: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let sessions: Sessions = Rc::new(RefCell::new(HashMap::new()));
    // C1's PUBLISHED accepted epochs. No entry = refuse everything for
    // that origin (refuse-until-published; no implicit epoch-0 authority).
    let mut accepted_epochs: HashMap<String, u64> = HashMap::new();
    let mut pending: HashMap<String, PendingCall<T>> = HashMap::new();
    let mut tasks: tokio::task::JoinSet<TaskDone> = tokio::task::JoinSet::new();
    let mut task_intent: HashMap<tokio::task::Id, String> = HashMap::new();
    // Session teardowns run detached from the select loop (a teardown
    // must not block command intake) and are drained at shutdown.
    let mut housekeeping: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    let mut tombstones = BoundedIdSet::new(TOMBSTONE_CAP);
    // Recently-terminal intent ids (bounded): lets a LATE cancel (after
    // the terminal was delivered) be absorbed as a true no-op instead of
    // tombstoning an id that may legitimately be reused (Codex u3 round
    // 1, P1-4).
    let mut recently_terminal = BoundedIdSet::new(RECENT_TERMINAL_CAP);

    // The test lever's phase (cfg(test): a normal runtime carries no
    // lever — the field does not even exist). Loop-thread-local, so
    // every transition below is plain program order.
    #[cfg(test)]
    let mut gate_phase = LoopGatePhase::Armed;

    /// Emit a refusal terminal for a call that never got a pending entry.
    /// Every refusal IS the intent's single terminal, so it is recorded
    /// in the recently-terminal memory like any other.
    async fn refuse<T>(
        evt_tx: &McpEventSink<T>,
        recently_terminal: &mut BoundedIdSet,
        ticket: T,
        intent_id: String,
        awili_call_id: AwiliCallId,
        disposition: CallDisposition,
        content: String,
    ) {
        debug_assert!(disposition != CallDisposition::Completed);
        recently_terminal.insert(intent_id.clone());
        let _ = evt_tx
            .send(McpEvent::CallFinished {
                ticket,
                intent_id,
                disposition,
                result: ToolResultRecord {
                    awili_call_id,
                    content,
                    is_error: true,
                },
            })
            .await;
    }

    /// Reject a DUPLICATE submission without creating a call (unit 4 r6
    /// Delta B″). NOT a terminal: the intent's single `CallFinished` is
    /// either still coming (duplicate against a live intent) or was
    /// already delivered (duplicate against a recently-terminal intent —
    /// the SAME bounded set that absorbs late `Cancel`; one structure,
    /// two consumers). The duplicate's ticket and its OWN `SubmissionId`
    /// return so the submitter retires exactly this rejection.
    async fn reject_duplicate<T>(
        evt_tx: &McpEventSink<T>,
        ticket: T,
        intent_id: String,
        submission_id: awili_tools::runtime::SubmissionId,
        reason: String,
    ) {
        let _ = evt_tx
            .send(McpEvent::SubmissionRejected {
                ticket,
                intent_id,
                submission_id,
                reason,
            })
            .await;
    }

    let shutdown_reason: &str = loop {
        // The cfg(test) park: while the gate holds EVERYTHING, the loop
        // never reaches the select below — no command is consumed and no
        // completion is processed. The park await is also what drives the
        // dispatch task (the real connect and the real call run against
        // the real server while the loop stands still), so the original's
        // outcome lands in the JoinSet and waits there. The watchdog is
        // failure hygiene only: an already-failing test that never
        // releases still lets shutdown proceed, loudly, instead of
        // parking the runtime forever.
        #[cfg(test)]
        if gate_phase == LoopGatePhase::HoldAll {
            let gate = config
                .gate
                .as_ref()
                .expect("HoldAll is reachable only with a configured gate");
            match tokio::time::timeout(GATE_PARK_WATCHDOG, gate.release_rx.recv()).await {
                Ok(Ok(())) => gate_phase = LoopGatePhase::HoldCommands,
                Ok(Err(_)) | Err(_) => {
                    eprintln!(
                        "awili-mcp-runtime: test loop gate lifted by watchdog or channel \
                         closure, not by its test — the forced ordering is gone"
                    );
                    gate_phase = LoopGatePhase::Released;
                }
            }
            continue;
        }
        #[cfg(test)]
        let holds_commands = gate_phase == LoopGatePhase::HoldCommands;
        #[cfg(not(test))]
        let holds_commands = false;

        tokio::select! {
            cmd = cmd_rx.recv(), if !holds_commands => match cmd {
                Ok(McpCommand::Shutdown) => {
                    shutting_down.set(true);
                    break "shutdown requested";
                }
                Err(_) => {
                    shutting_down.set(true);
                    break "command channel closed";
                }
                Ok(McpCommand::ConnectOrigin { origin }) => {
                    // IDEMPOTENT (design r9 §5.3): only Disconnected/Dead
                    // mint a generation; live states re-announce at most.
                    let key = origin.origin.clone();
                    match origin_states.get(&key) {
                        Some(RuntimeOriginState::Connecting { generation }) => {
                            let generation = *generation;
                            let _ = evt_tx.emit_origin_connecting(origin, generation).await;
                        }
                        Some(RuntimeOriginState::AwaitingAcceptance { .. })
                        | Some(RuntimeOriginState::Ready { .. }) => {
                            // The pump already holds this fact durably;
                            // nothing to re-mint, nothing lost.
                        }
                        None | Some(RuntimeOriginState::Disconnected)
                        | Some(RuntimeOriginState::Dead { .. }) => {
                            let generation = alloc.next();
                            match catalog.resolve(&origin) {
                                Some(spec) => {
                                    origin_states.insert(
                                        key.clone(),
                                        RuntimeOriginState::Connecting { generation },
                                    );
                                    let _ = evt_tx
                                        .emit_origin_connecting(origin.clone(), generation)
                                        .await;
                                    let slot: SessionSlot = sessions
                                        .borrow_mut()
                                        .entry(key)
                                        .or_insert_with(|| Rc::new(tokio::sync::Mutex::new(None)))
                                        .clone();
                                    let spec = spec.clone();
                                    let setup_path = Rc::clone(&setup_path);
                                    let connect_timeout = config.connect_timeout;
                                    let call_timeout = config.call_timeout;
                                    let meta = (origin.clone(), generation);
                                    let task = connect_tasks.spawn_local(async move {
                                        // FRESH control-plane read for THIS
                                        // generation (ruling H): the record
                                        // current at spawn is the one every
                                        // generation independently attests
                                        // against.
                                        let records = match setup_path.as_ref() {
                                            Some(p) => {
                                                awili_attest::setup_record::load_setup_records(p)
                                            }
                                            None => Default::default(),
                                        };
                                        let record = match records.lookup(&origin) {
                                            awili_attest::setup_record::RecordLookup::Valid(r) => {
                                                Some(r.clone())
                                            }
                                            awili_attest::setup_record::RecordLookup::Absent => None,
                                            awili_attest::setup_record::RecordLookup::Invalid => {
                                                // HARD ERROR, ORIGIN
                                                // UNAVAILABLE (impl review
                                                // r1 P1-3): fail closed
                                                // WITHOUT spawning — an
                                                // invalid installer record
                                                // never becomes a usable
                                                // unverified launch.
                                                return ConnectDone {
                                                    origin,
                                                    generation,
                                                    slot,
                                                    result: Err(
                                                        crate::mcp_transport::ConnectFailure {
                                                            cause: DeathCause::LaunchFailed,
                                                            selected_class: None,
                                                            detail: "invalid setup record — \
                                                                     origin unavailable (fail \
                                                                     closed)"
                                                                .into(),
                                                        },
                                                    ),
                                                };
                                            }
                                        };
                                        let result = Session::connect(
                                            &spec,
                                            &origin,
                                            generation,
                                            record.as_ref(),
                                            connect_timeout,
                                            call_timeout,
                                        )
                                        .await;
                                        ConnectDone {
                                            origin,
                                            generation,
                                            slot,
                                            result,
                                        }
                                    });
                                    connect_meta.insert(task.id(), meta);
                                }
                                None => {
                                    // Not a catalog origin: dies visibly,
                                    // no dangling Connecting.
                                    origin_states.insert(
                                        key,
                                        RuntimeOriginState::Dead { generation },
                                    );
                                    let _ = evt_tx.emit_origin_connecting(origin.clone(), generation).await;
                                    let _ = evt_tx
                                        .emit_server_died(
                                            origin,
                                            generation,
                                            DeathCause::LaunchFailed,
                                            None,
                                            "no MCP transport configured for this origin".into(),
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                }
                Ok(McpCommand::UpdateAcceptedRoots { origin, generation, epoch }) => {
                    // Generation-bound acceptance (design r9 §1, row 3):
                    // every publication names the generation it validated.
                    // The MONOTONIC FLOOR is checked FIRST — a stale epoch
                    // is refused loudly regardless of which generation
                    // carries it (a reconnect cannot launder a lower epoch
                    // through a fresh generation, row 11) — and only a
                    // publication matching the origin's current awaiting
                    // or ready generation confers authority.
                    let key = origin.origin.clone();
                    let awaiting = match origin_states.get(&key) {
                        Some(RuntimeOriginState::AwaitingAcceptance { generation: g, class })
                            if *g == generation =>
                        {
                            Some((*g, *class))
                        }
                        Some(RuntimeOriginState::Ready { generation: g, class, .. })
                            if *g == generation =>
                        {
                            // Monotonic re-acceptance for the SAME
                            // generation is legal (epoch raise).
                            Some((*g, *class))
                        }
                        _other => None,
                    };
                    {
                        match accepted_epochs.get(&key) {
                            Some(&current) if epoch < current => {
                                // Loud refusal: a stale security-authority
                                // update is operationally meaningful.
                                let ev = McpEvent::RootsUpdateIgnored {
                                    origin,
                                    stale_epoch: epoch,
                                    current_epoch: current,
                                };
                                #[cfg(not(test))]
                                {
                                    let _ = evt_tx.send(ev).await;
                                }
                                // Test builds route through try_emit so the
                                // probe records a POSITIVE observation of a
                                // full channel — the wedge condition itself.
                                #[cfg(test)]
                                match evt_tx.try_emit(ev) {
                                    Ok(()) => {
                                        if let Some(p) = &config.send_probe {
                                            p.completed
                                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                        }
                                    }
                                    Err(awili_tools::runtime::TryEmitError::Full(ev)) => {
                                        if let Some(p) = &config.send_probe {
                                            p.blocked_on_full
                                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                        }
                                        let _ = evt_tx.send(ev).await;
                                        if let Some(p) = &config.send_probe {
                                            p.completed
                                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                        }
                                    }
                                    Err(awili_tools::runtime::TryEmitError::Closed(_)) => {}
                                }
                            }
                            Some(&current) if epoch == current
                                && matches!(
                                    origin_states.get(&key),
                                    Some(RuntimeOriginState::Ready { generation: g, .. })
                                        if *g == generation
                                ) => {} // idempotent SAME-generation re-publication
                            _ => {
                                if let Some((generation, class)) = awaiting {
                                    // Authority-conferring acceptance:
                                    // ONLY here does the floor move (impl
                                    // review r1 P1-2 — a foreign-
                                    // generation publication must neither
                                    // confer authority NOR mutate the
                                    // floor, or it wedges the current
                                    // generation as stale).
                                    accepted_epochs.insert(key.clone(), epoch);
                                    origin_states.insert(
                                        key,
                                        RuntimeOriginState::Ready {
                                            generation,
                                            accepted_epoch: epoch,
                                            class,
                                        },
                                    );
                                    let _ = evt_tx
                                        .emit_roots_accepted(origin, generation, epoch)
                                        .await;
                                } else {
                                    // LOUD refusal, no state touched.
                                    eprintln!(
                                        "awili-mcp-runtime: acceptance for {key:?} {generation} \
                                         epoch {epoch} refused — not the current awaiting/ready \
                                         generation; floor unchanged"
                                    );
                                }
                            }
                        }
                    }
                }
                Ok(McpCommand::Cancel { intent_id }) => {
                    if let Some(p) = pending.get(&intent_id) {
                        // Join-before-synthesis: mark + abort; the
                        // terminal comes from join_next. A task that
                        // already completed keeps its real result.
                        p.cancel_requested.set(true);
                        p.abort.abort();
                    } else if recently_terminal.contains(&intent_id) {
                        // LATE cancel: the intent already got its one
                        // terminal. TRUE no-op — no tombstone, no state
                        // mutation, a reused id is not poisoned (P1-4).
                    } else {
                        // Unknown / not-yet-arrived intent: tombstone
                        // (bounded) so a FIFO cancel-before-CallTool is
                        // honored.
                        tombstones.insert(intent_id);
                    }
                }
                Ok(McpCommand::CallTool { ticket, intent_id, submission_id, origin, call, generation, root_epoch }) => {
                    // Refusal ladder — every rung returns the ticket.
                    if cmd_rx.is_closed() {
                        refuse(&evt_tx, &mut recently_terminal, ticket, intent_id, call.awili_call_id,
                            CallDisposition::NeverDispatched,
                            "shutdown before dispatch; call never ran.".into()).await;
                        continue;
                    }
                    if tombstones.take(&intent_id) {
                        refuse(&evt_tx, &mut recently_terminal, ticket, intent_id, call.awili_call_id,
                            CallDisposition::CancelledBeforeDispatch,
                            "cancelled before dispatch (cancel preceded the call); never sent.".into()).await;
                        continue;
                    }
                    if pending.contains_key(&intent_id) {
                        // NOT a call terminal: the original intent's single
                        // CallFinished is still coming (INV-C2-5 is
                        // per-intent). The duplicate's ticket returns via
                        // its own non-terminal rejection (Codex u3 P0-2).
                        let reason = format!(
                            "duplicate in-flight intent_id {intent_id:?}; refused — never dispatched."
                        );
                        reject_duplicate(&evt_tx, ticket, intent_id, submission_id, reason).await;
                        continue;
                    }
                    if recently_terminal.contains(&intent_id) {
                        // Unit 4 r6 Delta B″: admission is
                        // `pending ∪ recently_terminal`. A duplicate whose
                        // original already got its one terminal — the
                        // completion was processed before this queued
                        // CallTool was consumed — must never be admitted
                        // as a fresh call within the bounded window. The
                        // tombstone is NOT consumed: late `Cancel` still
                        // reads the same set (one structure, two
                        // consumers), and a submission does not un-terminal
                        // an intent.
                        let reason = format!(
                            "duplicate intent_id {intent_id:?}: already terminal \
                             (bounded recent-terminal history); refused — never dispatched."
                        );
                        reject_duplicate(&evt_tx, ticket, intent_id, submission_id, reason).await;
                        continue;
                    }
                    // Admission (unit 4b, rows 2/3): a call runs ONLY in
                    // Ready, under the exact accepted epoch of the CURRENT
                    // generation. AwaitingAcceptance refuses (C1 has not
                    // accepted this generation); everything else refuses
                    // with its state named.
                    match origin_states.get(&origin.origin) {
                        Some(RuntimeOriginState::Ready { generation: g, accepted_epoch, .. })
                            if *g == generation && *accepted_epoch == root_epoch => {}
                        Some(RuntimeOriginState::Ready { generation: g, .. })
                            if *g != generation =>
                        {
                            // The retained-binding refusal (impl review r1
                            // P1-1): the authorization was validated
                            // against a generation that is not the CURRENT
                            // one — an identical reconnect (same name,
                            // epoch, tools) must not silently inherit it.
                            let current = *g;
                            refuse(&evt_tx, &mut recently_terminal, ticket, intent_id, call.awili_call_id,
                                CallDisposition::NeverDispatched,
                                format!("binding from a retired generation: authorized under \
                                         {generation}, current is {current} — never dispatched.")).await;
                            continue;
                        }
                        Some(RuntimeOriginState::Ready { accepted_epoch, .. }) => {
                            let current = *accepted_epoch;
                            refuse(&evt_tx, &mut recently_terminal, ticket, intent_id, call.awili_call_id,
                                CallDisposition::NeverDispatched,
                                format!("stale root epoch: authorized under {root_epoch}, \
                                         accepted is {current} — never dispatched.")).await;
                            continue;
                        }
                        Some(RuntimeOriginState::AwaitingAcceptance { .. }) => {
                            refuse(&evt_tx, &mut recently_terminal, ticket, intent_id, call.awili_call_id,
                                CallDisposition::NeverDispatched,
                                format!("origin {:?} is awaiting root acceptance; \
                                         C1 must publish before dispatch — never dispatched.",
                                        origin.origin)).await;
                            continue;
                        }
                        other => {
                            let state = format!("{other:?}");
                            refuse(&evt_tx, &mut recently_terminal, ticket, intent_id, call.awili_call_id,
                                CallDisposition::NeverDispatched,
                                format!("origin {:?} is not ready (runtime state {state}); \
                                         never dispatched.", origin.origin)).await;
                            continue;
                        }
                    }
                    if catalog.resolve(&origin).is_none() {
                        refuse(&evt_tx, &mut recently_terminal, ticket, intent_id, call.awili_call_id,
                            CallDisposition::NeverDispatched,
                            format!("no MCP transport configured for origin {:?}; \
                                     failing closed — never dispatched.", origin.origin)).await;
                        continue;
                    }
                    let committed = Rc::new(Cell::new(false));
                    let cancel_requested = Rc::new(Cell::new(false));
                    // Resolve the origin's CURRENT slot generation here,
                    // synchronously — the task and the pending entry both
                    // reference this exact slot (ABA guard).
                    let slot: SessionSlot = sessions
                        .borrow_mut()
                        .entry(origin.origin.clone())
                        .or_insert_with(|| Rc::new(tokio::sync::Mutex::new(None)))
                        .clone();
                    let handle = tasks.spawn_local(dispatch_task(
                        Rc::clone(&slot),
                        origin.clone(),
                        call.clone(),
                        intent_id.clone(),
                        Rc::clone(&committed),
                        Rc::clone(&shutting_down),
                        config.call_timeout,
                    ));
                    task_intent.insert(handle.id(), intent_id.clone());
                    pending.insert(intent_id.clone(), PendingCall {
                        ticket,
                        intent_id,
                        awili_call_id: call.awili_call_id,
                        origin,
                        committed,
                        cancel_requested,
                        slot,
                        abort: handle,
                    });
                    // The test lever engages HERE — the first admitted
                    // `CallTool` was consumed and dispatched. No await
                    // point sits between this and the park above, so the
                    // gate's `engaged` flag positively observes a loop
                    // that has already stopped consuming commands and
                    // processing completions.
                    #[cfg(test)]
                    if gate_phase == LoopGatePhase::Armed {
                        if let Some(gate) = &config.gate {
                            gate_phase = LoopGatePhase::HoldAll;
                            gate.engage();
                        }
                    }
                }
            },
            done = tasks.join_next_with_id(), if !tasks.is_empty() => {
                if let Some(done) = done {
                    handle_task_done(
                        done,
                        &mut pending,
                        &mut task_intent,
                        &sessions,
                        &mut housekeeping,
                        &mut recently_terminal,
                        &evt_tx,
                        &mut origin_states,
                    ).await;
                    // The lever's second edge: the completion above
                    // emitted the original's `CallFinished` (the send is
                    // awaited inside `handle_task_done`, so returning
                    // means EMITTED). Command consumption — held since
                    // engagement — resumes from the next iteration, so
                    // the queued duplicate necessarily reaches the real
                    // admission code post-terminal.
                    #[cfg(test)]
                    if gate_phase == LoopGatePhase::HoldCommands {
                        gate_phase = LoopGatePhase::Released;
                    }
                }
            }
            connected = connect_tasks.join_next_with_id(), if !connect_tasks.is_empty() => {
                if let Some(done) = connected {
                    match done {
                        Ok((task_id, done)) => {
                            connect_meta.remove(&task_id);
                            let key = done.origin.origin.clone();
                            match done.result {
                                Ok(success) => {
                                    // The Copy class, retained before the
                                    // token moves into the observation
                                    // (design r9 §2.4) — later deaths of
                                    // this generation report it.
                                    let class = success.launch.class();
                                    *done.slot.lock().await = Some(Rc::new(success.session));
                                    origin_states.insert(
                                        key,
                                        RuntimeOriginState::AwaitingAcceptance {
                                            generation: done.generation,
                                            class,
                                        },
                                    );
                                    let _ = evt_tx
                                        .emit_server_connected(
                                            success.launch,
                                            success.claimed_server,
                                            success.tools,
                                            success.dropped_at_capture,
                                            success.advertised_epoch,
                                        )
                                        .await;
                                }
                                Err(failure) => {
                                    // Also to stderr, not only the in-band
                                    // channel: today's live-smoke debugging
                                    // (E1) had NO trace of connect failures
                                    // anywhere a terminal could see.
                                    eprintln!(
                                        "awili-mcp-runtime: {} {} connect failed: {:?} ({:?}): {}",
                                        done.origin.origin, done.generation, failure.cause,
                                        failure.selected_class, failure.detail
                                    );
                                    // Revoke BEFORE the death is
                                    // observable (row 9's ordering rule
                                    // applies to every authority drop).
                                    origin_states.insert(
                                        key,
                                        RuntimeOriginState::Dead {
                                            generation: done.generation,
                                        },
                                    );
                                    let _ = evt_tx
                                        .emit_server_died(
                                            done.origin,
                                            done.generation,
                                            failure.cause,
                                            failure.selected_class,
                                            failure.detail,
                                        )
                                        .await;
                                }
                            }
                        }
                        Err(join_err) => {
                            eprintln!("awili-mcp-runtime: connect task died: {join_err}");
                            // RC2-APP (F0202): the attempt is over; its origin
                            // dies visibly (and may be connected again)
                            // instead of staying `Connecting` for good.
                            if let Some((origin, generation)) =
                                connect_meta.remove(&join_err.id())
                            {
                                if settle_lost_connect(
                                    &mut origin_states,
                                    &origin.origin,
                                    generation,
                                ) {
                                    let _ = evt_tx
                                        .emit_server_died(
                                            origin,
                                            generation,
                                            DeathCause::LaunchFailed,
                                            None,
                                            format!(
                                                "the connect attempt ended unexpectedly: {join_err}"
                                            ),
                                        )
                                        .await;
                                }
                            }
                        }
                    }
                }
            }
            _ = housekeeping.join_next(), if !housekeeping.is_empty() => {}
        }
    };

    // ── Orderly shutdown ────────────────────────────────────────────────
    // 1. Refuse new work; drain queued-but-undispatched commands into
    //    DEFINITE terminals.
    cmd_rx.close();
    while let Ok(cmd) = cmd_rx.try_recv() {
        if let McpCommand::CallTool {
            ticket,
            intent_id,
            call,
            ..
        } = cmd
        {
            refuse(
                &evt_tx,
                &mut recently_terminal,
                ticket,
                intent_id,
                call.awili_call_id,
                CallDisposition::NeverDispatched,
                format!("{shutdown_reason} before dispatch; call never ran."),
            )
            .await;
        }
        // Queued Cancel / UpdateAcceptedRoots at shutdown: absorbed.
    }

    // 2. Grace deadline for in-flight calls (their real terminals win).
    let deadline = tokio::time::Instant::now() + config.shutdown_grace;
    while !tasks.is_empty() {
        match tokio::time::timeout_at(deadline, tasks.join_next_with_id()).await {
            Ok(Some(done)) => {
                handle_task_done(
                    done,
                    &mut pending,
                    &mut task_intent,
                    &sessions,
                    &mut housekeeping,
                    &mut recently_terminal,
                    &evt_tx,
                    &mut origin_states,
                )
                .await;
            }
            Ok(None) => break,
            Err(_) => break, // grace expired
        }
    }

    // 3. Abort the rest, then DRAIN the JoinSet to completion — the join
    //    results (not a separate synthesis pass) produce the remaining
    //    terminals via the same single handler, and after the drain no
    //    task can still hold a session Rc or an event sender clone.
    tasks.abort_all();
    while let Some(done) = tasks.join_next_with_id().await {
        handle_task_done(
            done,
            &mut pending,
            &mut task_intent,
            &sessions,
            &mut housekeeping,
            &mut recently_terminal,
            &evt_tx,
            &mut origin_states,
        )
        .await;
    }

    // 4. INVARIANT: the full join-drain above resolved every pending
    //    entry — join-before-synthesis is absolute (Codex u3 P1-3). A
    //    non-empty map here is task↔pending accounting corruption: fail
    //    LOUDLY in debug; in release the tickets still come back (never
    //    lose a ticket to an assertion), marked as the violation they are.
    debug_assert!(
        pending.is_empty(),
        "task/pending accounting broken: {:?} unresolved after full join drain",
        pending.keys().collect::<Vec<_>>()
    );
    for (_, p) in pending.drain() {
        recently_terminal.insert(p.intent_id.clone());
        let _ = evt_tx
            .send(McpEvent::CallFinished {
                ticket: p.ticket,
                intent_id: p.intent_id,
                disposition: CallDisposition::InFlightUnresolved,
                result: ToolResultRecord {
                    awili_call_id: p.awili_call_id,
                    content: format!(
                        "INVARIANT VIOLATION (task/pending accounting): unresolved after \
                     full join drain at {shutdown_reason}; outcome unknown."
                    ),
                    is_error: true,
                },
            })
            .await;
    }

    // 4b. RC2-APP (F0204): connect attempts still in flight are stopped and
    //     DRAINED too, so none outlives the loop. A connect that finished
    //     meanwhile hands its session to its slot (swept in step 5); an
    //     aborted one drops its child, whose kill is queued on this runtime
    //     — given a moment to run before the runtime goes away.
    connect_tasks.abort_all();
    let mut aborted_connects = false;
    while let Some(done) = connect_tasks.join_next().await {
        match done {
            Ok(ConnectDone {
                slot,
                result: Ok(success),
                ..
            }) => {
                *slot.lock().await = Some(Rc::new(success.session));
            }
            Ok(_) => {}
            Err(_) => aborted_connects = true,
        }
    }
    connect_meta.clear();
    if aborted_connects {
        tokio::time::sleep(CONNECT_ABORT_SETTLE).await;
    }

    // 5. Let in-flight teardowns finish, then sweep every session:
    //    cancel handshake + child reaped, force_kill as the fallback.
    while housekeeping.join_next().await.is_some() {}
    let slots: Vec<(String, SessionSlot)> = sessions.borrow_mut().drain().collect();
    for (_, slot) in slots {
        let mut guard = slot.lock().await;
        if let Some(rc) = guard.take() {
            match Rc::try_unwrap(rc) {
                Ok(session) => session.teardown().await,
                Err(shared) => shared.force_kill().await,
            }
        }
    }

    // 6. The event sender drops here, closing the stream — the handle's
    //    drain loop terminates and joins us.
}

/// Tear down the EXACT slot a failed call ran on, OFF the loop
/// (housekeeping task). The origin's map entry is removed ONLY if it is
/// still this same slot (`Rc::ptr_eq`) — a stale failure arriving after
/// a replacement session was created must never kill the replacement
/// (Codex u3 round 1, P0-1: ABA).
fn teardown_session_detached(
    sessions: &Sessions,
    housekeeping: &mut tokio::task::JoinSet<()>,
    origin: &ToolOrigin,
    failed_slot: &SessionSlot,
) -> bool {
    {
        let mut map = sessions.borrow_mut();
        match map.get(&origin.origin) {
            Some(current) if Rc::ptr_eq(current, failed_slot) => {
                map.remove(&origin.origin);
            }
            _ => {
                // The map already moved on (entry gone or replaced).
                // Still tear down the failed slot below — its child must
                // be reaped — but do NOT touch the map, and do NOT
                // announce a ServerDied for a session that is not the
                // origin's current one.
                let slot = Rc::clone(failed_slot);
                housekeeping.spawn_local(async move {
                    let mut guard = slot.lock().await;
                    if let Some(rc) = guard.take() {
                        match Rc::try_unwrap(rc) {
                            Ok(session) => session.teardown().await,
                            Err(shared) => shared.force_kill().await,
                        }
                    }
                });
                return false;
            }
        }
    }
    let slot = Rc::clone(failed_slot);
    housekeeping.spawn_local(async move {
        let mut guard = slot.lock().await;
        if let Some(rc) = guard.take() {
            match Rc::try_unwrap(rc) {
                Ok(session) => session.teardown().await,
                Err(shared) => shared.force_kill().await,
            }
        }
    });
    true
}

#[allow(clippy::too_many_arguments)]
async fn handle_task_done<T>(
    done: Result<(tokio::task::Id, TaskDone), tokio::task::JoinError>,
    pending: &mut HashMap<String, PendingCall<T>>,
    task_intent: &mut HashMap<tokio::task::Id, String>,
    sessions: &Sessions,
    housekeeping: &mut tokio::task::JoinSet<()>,
    recently_terminal: &mut BoundedIdSet,
    evt_tx: &McpEventSink<T>,
    origin_states: &mut HashMap<String, RuntimeOriginState>,
) {
    match done {
        Ok((task_id, done)) => {
            task_intent.remove(&task_id);
            if let Some((origin, failed_slot)) = done.broke_session {
                if teardown_session_detached(sessions, housekeeping, &origin, &failed_slot) {
                    if let Some((generation, class)) = revoke_origin(origin_states, &origin.origin)
                    {
                        // Revoked FIRST (row 9): admission already refuses
                        // by the time the death event is observable.
                        let _ = evt_tx
                            .emit_server_died(
                                origin,
                                generation,
                                DeathCause::TransportFailed,
                                class,
                                "transport failed mid-call; session torn down".into(),
                            )
                            .await;
                    }
                }
            }
            let Some(p) = pending.remove(&done.intent_id) else {
                // Already resolved — never a second terminal (INV-C2-5).
                return;
            };
            recently_terminal.insert(p.intent_id.clone());
            let _ = evt_tx
                .send(McpEvent::CallFinished {
                    ticket: p.ticket,
                    intent_id: p.intent_id,
                    disposition: done.verdict.disposition,
                    result: done.verdict.result,
                })
                .await;
        }
        Err(join_err) => {
            let task_id = join_err.id();
            let Some(intent_id) = task_intent.remove(&task_id) else {
                return;
            };
            let Some(p) = pending.remove(&intent_id) else {
                return;
            };
            recently_terminal.insert(p.intent_id.clone());
            if join_err.is_panic() {
                // Fail closed on the EXACT session a panicked dispatch
                // used (identity-checked — ABA guard).
                if teardown_session_detached(sessions, housekeeping, &p.origin, &p.slot) {
                    if let Some((generation, class)) =
                        revoke_origin(origin_states, &p.origin.origin)
                    {
                        let _ = evt_tx
                            .emit_server_died(
                                p.origin.clone(),
                                generation,
                                DeathCause::TransportFailed,
                                class,
                                "dispatch task panicked; session torn down".into(),
                            )
                            .await;
                    }
                }
                let _ = evt_tx
                    .send(McpEvent::CallFinished {
                        ticket: p.ticket,
                        intent_id: p.intent_id,
                        disposition: CallDisposition::InFlightUnresolved,
                        result: ToolResultRecord {
                            awili_call_id: p.awili_call_id,
                            content: format!(
                                "MCP dispatch task panicked: {join_err}; outcome unknown."
                            ),
                            is_error: true,
                        },
                    })
                    .await;
                return;
            }
            // Aborted (cancel or shutdown). Join-before-synthesis: the
            // COMMITTED cell decides certainty, the CANCEL flag decides
            // attribution.
            let (disposition, content) = if p.committed.get() {
                (
                    CallDisposition::InFlightUnresolved,
                    if p.cancel_requested.get() {
                        "cancelled post-dispatch; outcome unknown.".to_string()
                    } else {
                        "shutdown while the call was in flight; outcome unknown.".to_string()
                    },
                )
            } else if p.cancel_requested.get() {
                (
                    CallDisposition::CancelledBeforeDispatch,
                    "cancelled before dispatch commit; call never ran.".to_string(),
                )
            } else {
                (
                    CallDisposition::NeverDispatched,
                    "shutdown before dispatch commit; call never ran.".to_string(),
                )
            };
            let _ = evt_tx
                .send(McpEvent::CallFinished {
                    ticket: p.ticket,
                    intent_id: p.intent_id,
                    disposition,
                    result: ToolResultRecord {
                        awili_call_id: p.awili_call_id,
                        content,
                        is_error: true,
                    },
                })
                .await;
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Live acceptance tests against the C2-u2 gate: crates/awili-mcp-testserver,
// a REAL fault-injecting MCP server child process (the build plan's whole
// point — these tests cannot pass for the wrong reason against a stub).
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mcp_transport::{McpServerCatalog, StdioServerSpec};
    use awili_engine::{RiskClass, ToolCall};
    use awili_tools::runtime::{ExecutionCertainty, McpCommand, McpEvent, SubmissionId};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[cfg(not(unix))]
    #[allow(unused_variables, dead_code)]
    pub(crate) fn testserver_bin() -> PathBuf {
        unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
    }
    /// A direct test-binary invocation can start outside the workspace. The
    /// fixture's nested Cargo build must still find the workspace manifest.
    #[cfg(unix)]
    #[test]
    fn testserver_build_works_from_foreign_cwd() {
        const CHILD: &str = "AWILI_TESTSERVER_FOREIGN_CWD_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert!(testserver_bin().is_file());
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .current_dir(dir.path())
            .env(CHILD, "1")
            .args(["--exact", "mcp_runtime::tests::testserver_build_works_from_foreign_cwd", "--nocapture"])
            .output().unwrap();
        assert!(output.status.success(), "child failed:\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }

    #[cfg(unix)]
    /// The testserver binary for this test process: built once, then handed
    /// out from a path nothing ever rewrites.
    ///
    /// RC2-R4 (flaky `rc1m_fix1_owner_servers_reach_the_first_in_app_turn`,
    /// gate-fixc: "testserver binary at …/target/debug/awili-mcp-testserver"
    /// missing right after its build). Every test process ran `cargo build
    /// -p awili-mcp-testserver` into the workspace target, and cargo re-links
    /// `target/debug/awili-mcp-testserver` on every build, even a fresh one
    /// (it removes the file, then links it again): under a parallel run one
    /// process's build removed the binary another was checking or starting
    /// (6 processes × 30 builds: 1 missing in 180). Now the build goes to its
    /// own target dir (`target/testserver`, never the one `cargo test` uses),
    /// one process at a time (a file lock), and each process starts the copy
    /// named by the binary's content (`run/awili-mcp-testserver-<hash>`),
    /// written once by rename and never replaced while anyone may run it.
    pub(crate) fn testserver_bin() -> PathBuf {
        use std::hash::{Hash, Hasher};
        static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        BIN.get_or_init(|| {
            let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            root.pop(); // crates/
            root.pop(); // workspace root
            let dir = root.join("target").join("testserver");
            std::fs::create_dir_all(dir.join("run")).expect("testserver dir");
            let lock = std::fs::File::create(dir.join("build.lock")).expect("testserver lock file");
            lock.lock().expect("testserver build lock");
            let status = std::process::Command::new(env!("CARGO"))
                .current_dir(&root)
                .args([
                    "build",
                    "--locked",
                    "-p",
                    "awili-mcp-testserver",
                    "--target-dir",
                ])
                .arg(&dir)
                .status()
                .expect("run cargo build for the testserver gate");
            assert!(status.success(), "testserver gate must build");
            let built = std::fs::read(dir.join("debug").join("awili-mcp-testserver"))
                .expect("the built testserver");
            let mut h = std::collections::hash_map::DefaultHasher::new();
            built.hash(&mut h);
            let stable = dir
                .join("run")
                .join(format!("awili-mcp-testserver-{:016x}", h.finish()));
            if !stable.exists() {
                use std::os::unix::fs::PermissionsExt;
                let tmp = dir
                    .join("run")
                    .join(format!(".awili-mcp-testserver-{}", std::process::id()));
                std::fs::write(&tmp, &built).expect("copy the testserver");
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
                    .expect("make the testserver executable");
                std::fs::rename(&tmp, &stable).expect("place the testserver");
            }
            drop(lock);
            assert!(
                stable.is_absolute() && stable.exists(),
                "testserver binary at {stable:?}"
            );
            stable
        })
        .clone()
    }

    pub(crate) fn spec(fault: &[(&str, &str)]) -> StdioServerSpec {
        StdioServerSpec::new(
            testserver_bin(),
            vec![],
            fault
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .expect("absolute testserver path")
    }

    pub(crate) fn test_config() -> McpRuntimeConfig {
        McpRuntimeConfig {
            command_capacity: 8,
            event_capacity: 8,
            // RC2-APP (K-flake1): the production-like 5 s connect deadline
            // failed under a parallel build; a test that wants a short one
            // sets it itself.
            connect_timeout: TEST_CONNECT_TIMEOUT,
            call_timeout: Duration::from_millis(1500),
            shutdown_grace: Duration::from_millis(300),
            setup_path: None,
            send_probe: None,
            gate: None,
        }
    }

    /// Unit 4b bring-up: drive the REAL machine to Ready for one origin.
    /// Submits `ConnectOrigin`, waits for the `ServerConnected`
    /// observation (its generation is the machine's own), then submits the
    /// generation-bound acceptance. Command FIFO makes a subsequent
    /// `CallTool` ordered strictly after the acceptance — no event wait is
    /// needed (`next_event` skips the lifecycle markers anyway).
    pub(crate) fn bring_up(
        handle: &awili_tools::runtime::McpRuntimeHandle<String>,
        origin: &str,
        epoch: u64,
    ) -> (
        awili_engine::generation::ConnectionGeneration,
        awili_tools::runtime::ConnectedLaunch,
    ) {
        retry_full(|| {
            handle.submit(McpCommand::ConnectOrigin {
                origin: ToolOrigin::new(origin),
            })
        });
        let ev = next_event(handle, EVENT_WAIT);
        let McpEvent::ServerConnected(connected) = ev else {
            panic!("expected ServerConnected during bring-up, got {ev:?}");
        };
        assert_eq!(connected.origin().origin, origin, "bring-up origin");
        let generation = connected.generation();
        TEST_GENERATIONS.with(|m| m.borrow_mut().insert(origin.to_string(), generation));
        retry_full(|| {
            handle.submit(McpCommand::UpdateAcceptedRoots {
                origin: ToolOrigin::new(origin),
                generation,
                epoch,
            })
        });
        (generation, connected)
    }

    /// Bring-up WITHOUT acceptance: connect the origin and return its
    /// generation and observation while it is still AwaitingAcceptance.
    pub(crate) fn connect_only(
        handle: &awili_tools::runtime::McpRuntimeHandle<String>,
        origin: &str,
    ) -> (
        awili_engine::generation::ConnectionGeneration,
        awili_tools::runtime::ConnectedLaunch,
    ) {
        handle
            .submit(McpCommand::ConnectOrigin {
                origin: ToolOrigin::new(origin),
            })
            .expect("submit ConnectOrigin");
        let ev = next_event(handle, EVENT_WAIT);
        let McpEvent::ServerConnected(connected) = ev else {
            panic!("expected ServerConnected, got {ev:?}");
        };
        assert_eq!(connected.origin().origin, origin);
        TEST_GENERATIONS.with(|m| {
            m.borrow_mut()
                .insert(origin.to_string(), connected.generation())
        });
        (connected.generation(), connected)
    }

    /// Deterministically drain the bring-up's RootsAccepted marker (and
    /// any lifecycle markers before it). Capacity-1 wedge tests MUST call
    /// this after bring_up, or the marker occupies the slot the wedge
    /// arithmetic assigns to its first Ignored.
    pub(crate) fn drain_until_roots_accepted(
        handle: &awili_tools::runtime::McpRuntimeHandle<String>,
    ) {
        let deadline = Instant::now() + EVENT_WAIT;
        loop {
            match handle.try_recv_event() {
                Some(McpEvent::RootsAccepted { .. }) => break,
                Some(McpEvent::OriginDisconnected { .. })
                | Some(McpEvent::OriginConnecting { .. })
                | None => {
                    assert!(Instant::now() < deadline, "RootsAccepted never arrived");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Some(other) => panic!("unexpected pre-wedge event {other:?}"),
            }
        }
    }

    pub(crate) fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            awili_call_id: awili_engine::AwiliCallId(id.to_string()),
            provider_call_id: Some(format!("prov-{id}")),
            name: name.to_string(),
            arguments: args.to_string(),
        }
    }

    /// Publish C1's accepted epoch for an origin (refuse-until-published:
    /// every test origin needs this before calls dispatch).
    pub(crate) fn publish(
        handle: &awili_tools::runtime::McpRuntimeHandle<String>,
        origin: &str,
        generation: awili_engine::generation::ConnectionGeneration,
        epoch: u64,
    ) {
        retry_full(|| {
            handle.submit(McpCommand::UpdateAcceptedRoots {
                origin: awili_tools::ToolOrigin::new(origin),
                generation,
                epoch,
            })
        });
    }

    thread_local! {
        /// origin → the generation the LAST bring_up on this test thread
        /// drove to acceptance. `submit`/`submit_epoch` read it so the 44
        /// call sites stay generation-correct without threading the value
        /// through every test by hand; the retained-binding row uses the
        /// explicit variant instead.
        static TEST_GENERATIONS: std::cell::RefCell<
            std::collections::HashMap<String, awili_engine::generation::ConnectionGeneration>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }

    /// Bounded retry for tests with deliberately tiny command channels:
    /// transient Full is backpressure working, not a failure.
    fn retry_full(mut f: impl FnMut() -> Result<(), awili_tools::runtime::SubmitError>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match f() {
                Ok(()) => return,
                Err(awili_tools::runtime::SubmitError::Full) => {
                    assert!(Instant::now() < deadline, "channel stayed Full");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(other) => panic!("submit: {other}"),
            }
        }
    }

    /// Spawn a runtime and bring each origin to Ready at accepted epoch 1
    /// — the common-case setup for tests not ABOUT lifecycle semantics
    /// (unit 4b: connection is ConnectOrigin's job; a call is admitted
    /// only in Ready).
    fn spawn_published(
        catalog: McpServerCatalog,
        config: McpRuntimeConfig,
        origins: &[&str],
    ) -> awili_tools::runtime::McpRuntimeHandle<String> {
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        for o in origins {
            let _ = bring_up(&handle, o, 1);
        }
        handle
    }

    pub(crate) fn submit(
        handle: &awili_tools::runtime::McpRuntimeHandle<String>,
        ticket: &str,
        origin: &str,
        c: ToolCall,
    ) {
        submit_epoch(handle, ticket, origin, c, 1);
    }

    fn submit_epoch(
        handle: &awili_tools::runtime::McpRuntimeHandle<String>,
        ticket: &str,
        origin: &str,
        c: ToolCall,
        root_epoch: u64,
    ) {
        // The CURRENT generation this test thread drove to acceptance for
        // the origin — REQUIRED (impl review r2 finding 1): a fabricated
        // fallback let a test pass by allocator coincidence. Refusal-path
        // tests on never-accepted origins state their intent with
        // submit_epoch_with_generation.
        let generation = TEST_GENERATIONS
            .with(|m| m.borrow().get(origin).copied())
            .unwrap_or_else(|| {
                panic!(
                    "origin {origin:?} was never brought up on this thread — use \
                     submit_epoch_with_generation for refusal-path tests"
                )
            });
        submit_epoch_with_generation(handle, ticket, origin, c, generation, root_epoch);
    }

    /// The explicit variant — the retained-binding row submits under an
    /// OLD generation on purpose.
    pub(crate) fn submit_epoch_with_generation(
        handle: &awili_tools::runtime::McpRuntimeHandle<String>,
        ticket: &str,
        origin: &str,
        c: ToolCall,
        generation: awili_engine::generation::ConnectionGeneration,
        root_epoch: u64,
    ) {
        retry_full(|| {
            handle.submit(McpCommand::CallTool {
                ticket: ticket.to_string(),
                intent_id: format!("intent-{ticket}"),
                submission_id: SubmissionId(0),
                origin: awili_tools::ToolOrigin::new(origin),
                call: c.clone(),
                generation,
                root_epoch,
            })
        });
    }

    fn cancel(handle: &awili_tools::runtime::McpRuntimeHandle<String>, ticket: &str) {
        retry_full(|| {
            handle.submit(McpCommand::Cancel {
                intent_id: format!("intent-{ticket}"),
            })
        });
    }

    /// Poll for the next event with a deadline — no test can hang the suite.
    pub(crate) fn next_event(
        handle: &awili_tools::runtime::McpRuntimeHandle<String>,
        within: Duration,
    ) -> McpEvent<String> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(ev) = handle.try_recv_event() {
                // Pure lifecycle markers are skipped here: these tests
                // assert the material event order (connected / died /
                // terminals); the lifecycle rows assert the markers.
                match ev {
                    McpEvent::OriginDisconnected { .. }
                    | McpEvent::OriginConnecting { .. }
                    | McpEvent::RootsAccepted { .. } => continue,
                    other => return other,
                }
            }
            assert!(Instant::now() < deadline, "no event within {within:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// RC2-APP (K-flake1, S-N1): how long a test waits for an event. Longer
    /// than [`TEST_CONNECT_TIMEOUT`] so a connect (attestation included)
    /// under load ends in its own outcome, never in the test's wait.
    pub(crate) const EVENT_WAIT: Duration = Duration::from_secs(40);

    /// RC2-APP (K-flake1): the test runtimes' connect deadline.
    pub(crate) const TEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

    #[test]
    fn echo_round_trips_and_advertises_classified_tools() {
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let (_g, connected) = bring_up(&handle, "mcp:test", 1);
        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", r#"{"text":"hello-c2"}"#),
        );
        assert_eq!(connected.origin().origin, "mcp:test");
        assert_eq!(connected.dropped_at_capture().len(), 0);
        // RC1-M-FIX1: no MCP_ROOTS_EPOCH env => no `awili/rootsEpoch` key =>
        // a rootless standard server, bound at the rootless epoch.
        assert_eq!(
            connected.advertised_epoch(),
            Some(crate::mcp_transport::ROOTLESS_EPOCH),
            "no MCP_ROOTS_EPOCH env => a rootless server"
        );
        let tools = connected.tools().to_vec();
        // The gate advertises echo (read_only) + touch_file (mutating),
        // each with BOTH annotations and awili/sideEffect — agreeing
        // signals keep the declared class.
        let echo = tools.iter().find(|t| t.name == "echo").expect("echo");
        assert_eq!(echo.risk, RiskClass::ReadOnly);
        let touch = tools
            .iter()
            .find(|t| t.name == "touch_file")
            .expect("touch_file");
        assert_eq!(touch.risk, RiskClass::Mutating);
        assert!(echo.arg_schema_canonical.contains("\"text\""));

        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            intent_id,
            disposition,
            result,
        } = finished
        else {
            panic!("expected CallFinished, got {finished:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(intent_id, "intent-t1");
        assert_eq!(disposition, CallDisposition::Completed);
        assert_eq!(disposition.certainty(), ExecutionCertainty::Definite);
        assert!(!result.is_error);
        assert_eq!(result.content, "hello-c2");
        assert_eq!(result.awili_call_id.0, "c1");

        // Session REUSE: a second call must not re-advertise.
        submit(
            &handle,
            "t2",
            "mcp:test",
            call("c2", "echo", r#"{"text":"again"}"#),
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished { ticket, result, .. } = finished else {
            panic!("expected CallFinished (no second ServerConnected), got {finished:?}");
        };
        assert_eq!(ticket, "t2");
        assert_eq!(result.content, "again");

        let leftovers = handle.shutdown_and_drain();
        assert!(
            leftovers
                .iter()
                .all(|e| !matches!(e, McpEvent::CallFinished { .. })),
            "no stray terminals at shutdown"
        );
    }

    #[test]
    fn touch_file_performs_a_real_write() {
        let dir = crate::test_tmp::fresh_dir("c2u2");
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("touched.txt");
        let _ = std::fs::remove_file(&target);

        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_published(catalog, test_config(), &["mcp:test"]);
        submit(
            &handle,
            "t1",
            "mcp:test",
            call(
                "c1",
                "touch_file",
                &format!(
                    r#"{{"path":{}}}"#,
                    serde_json::to_string(target.to_str().unwrap()).unwrap()
                ),
            ),
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            disposition,
            result,
            ..
        } = finished
        else {
            panic!("expected CallFinished, got {finished:?}");
        };
        assert_eq!(disposition, CallDisposition::Completed);
        assert!(!result.is_error);
        // The REAL side effect, not the reply text (gate discipline).
        assert!(target.exists(), "touch_file must actually write");
        let _ = handle.shutdown_and_drain();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn child_death_mid_call_is_uncertain_and_never_finalized() {
        let catalog = McpServerCatalog::new()
            .with_stdio(
                awili_tools::ToolOrigin::new("mcp:crash"),
                spec(&[("MCP_FAULT", "crash_on_call")]),
            )
            .with_stdio(awili_tools::ToolOrigin::new("mcp:ok"), spec(&[]));
        let handle = spawn_published(catalog, test_config(), &["mcp:crash", "mcp:ok"]);

        submit(
            &handle,
            "t1",
            "mcp:crash",
            call("c1", "echo", r#"{"text":"boom"}"#),
        );
        let mut saw_died = false;
        let mut saw_uncertain = false;
        // Expect (ServerDied, CallFinished-Uncertain); the bring-up already
        // consumed ServerConnected.
        while !(saw_died && saw_uncertain) {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::OriginDisconnected { .. }
                | McpEvent::OriginConnecting { .. }
                | McpEvent::RootsAccepted { .. } => {}
                McpEvent::ServerConnected { .. } => {}
                McpEvent::ServerDied { origin, .. } => {
                    assert_eq!(origin.origin, "mcp:crash");
                    saw_died = true;
                }
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    result,
                    ..
                } => {
                    assert_eq!(ticket, "t1");
                    assert_eq!(
                        disposition,
                        CallDisposition::InFlightUnresolved,
                        "a crash after the request may have reached the server is UNCERTAIN"
                    );
                    assert_eq!(disposition.certainty(), ExecutionCertainty::Uncertain);
                    assert!(result.is_error);
                    saw_uncertain = true;
                }
                McpEvent::RootsUpdateIgnored { .. } => panic!("no epoch updates in this test"),
                McpEvent::SubmissionRejected { .. } => panic!("no duplicates in this test"),
            }
        }
        assert!(saw_died && saw_uncertain);

        // The RUNTIME survives its server's death: a different origin works.
        submit(
            &handle,
            "t2",
            "mcp:ok",
            call("c2", "echo", r#"{"text":"alive"}"#),
        );
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerConnected { .. } => {}
                McpEvent::CallFinished { ticket, result, .. } => {
                    assert_eq!(ticket, "t2");
                    assert_eq!(result.content, "alive");
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }

        // And the dead origin reconnects as a NEW GENERATION when told to
        // (unit 4b: reconnection is ConnectOrigin's job — row 10 lineage).
        let (_g2, c) = bring_up(&handle, "mcp:crash", 1);
        assert_eq!(c.origin().origin, "mcp:crash");
        let reconnected = true;
        submit(
            &handle,
            "t3",
            "mcp:crash",
            call("c3", "echo", r#"{"text":"boom2"}"#),
        );
        let mut finished = false;
        while !finished {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerConnected(c) => {
                    assert_eq!(c.origin().origin, "mcp:crash");
                }
                McpEvent::ServerDied { .. } => {}
                McpEvent::OriginDisconnected { .. }
                | McpEvent::OriginConnecting { .. }
                | McpEvent::RootsAccepted { .. } => {}
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    ..
                } => {
                    assert_eq!(ticket, "t3");
                    assert_eq!(disposition, CallDisposition::InFlightUnresolved);
                    finished = true;
                }
                McpEvent::RootsUpdateIgnored { .. } => panic!("no epoch updates in this test"),
                McpEvent::SubmissionRejected { .. } => panic!("no duplicates in this test"),
            }
        }
        assert!(reconnected && finished);
        let _ = handle.shutdown_and_drain();
    }

    #[test]
    fn wrong_id_and_malformed_are_uncertain_not_clean_errors() {
        for fault in ["wrong_id", "malformed"] {
            let catalog = McpServerCatalog::new().with_stdio(
                awili_tools::ToolOrigin::new("mcp:faulty"),
                spec(&[("MCP_FAULT", fault)]),
            );
            let handle = spawn_published(catalog, test_config(), &["mcp:faulty"]);
            submit(
                &handle,
                "t1",
                "mcp:faulty",
                call("c1", "echo", r#"{"text":"x"}"#),
            );
            let mut disposition = None;
            for _ in 0..3 {
                if let McpEvent::CallFinished {
                    ticket,
                    disposition: d,
                    result,
                    ..
                } = next_event(&handle, EVENT_WAIT)
                {
                    assert_eq!(ticket, "t1");
                    assert!(result.is_error);
                    disposition = Some(d);
                    break;
                }
            }
            assert_eq!(
                disposition,
                Some(CallDisposition::InFlightUnresolved),
                "{fault}: the request reached the server; outcome is unknowable"
            );
            let _ = handle.shutdown_and_drain();
        }
    }

    #[test]
    fn unknown_origin_fails_closed_definite_never_dispatched() {
        let handle = spawn_mcp_runtime::<String>(McpServerCatalog::new(), test_config());
        submit_epoch_with_generation(
            &handle,
            "t1",
            "mcp:ghost",
            call("c1", "echo", r#"{"text":"x"}"#),
            awili_engine::generation::GenerationAllocator::new().next(),
            1,
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            disposition,
            result,
            ..
        } = finished
        else {
            panic!("expected CallFinished, got {finished:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(disposition, CallDisposition::NeverDispatched);
        assert!(result.is_error);
        assert!(result.content.contains("never dispatched"));
        let _ = handle.shutdown_and_drain();
    }

    #[test]
    fn shutdown_with_call_in_flight_returns_ticket_as_uncertain() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:hang"),
            spec(&[("MCP_FAULT", "hang")]),
        );
        let mut config = test_config();
        config.call_timeout = Duration::from_secs(60); // hang outlives shutdown
        let handle = spawn_published(catalog, config, &["mcp:hang"]);
        submit(
            &handle,
            "t1",
            "mcp:hang",
            call("c1", "echo", r#"{"text":"x"}"#),
        );
        // Wait for connect so the call is genuinely in flight.
        // Give the dispatch task time to pass its commit point (the loop
        // no longer serializes connect, so ServerConnected precedes the
        // actual tools/call write by a hair).
        std::thread::sleep(Duration::from_millis(200));

        let drained = handle.shutdown_and_drain();
        let terminals: Vec<_> = drained
            .iter()
            .filter_map(|e| match e {
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    ..
                } => Some((ticket.clone(), *disposition)),
                _ => None,
            })
            .collect();
        assert_eq!(
            terminals,
            vec![("t1".to_string(), CallDisposition::InFlightUnresolved)],
            "exactly one terminal, Uncertain, ticket returned"
        );
    }

    #[test]
    fn saturated_event_channel_cannot_deadlock_shutdown_and_no_ticket_is_lost() {
        // Tiny event channel, no draining: the runtime WILL block mid-send.
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let mut config = test_config();
        config.event_capacity = 1;
        config.command_capacity = 16;
        let handle = spawn_published(catalog, config, &["mcp:test"]);

        let n = 8;
        for i in 0..n {
            submit(
                &handle,
                &format!("t{i}"),
                "mcp:test",
                call(&format!("c{i}"), "echo", r#"{"text":"y"}"#),
            );
        }
        // Give the runtime time to wedge against the full event channel.
        std::thread::sleep(Duration::from_millis(300));

        // shutdown_and_drain drains while joining — this must terminate.
        let drained = handle.shutdown_and_drain();
        let mut tickets: Vec<String> = drained
            .into_iter()
            .filter_map(|e| match e {
                McpEvent::CallFinished { ticket, .. } => Some(ticket),
                _ => None,
            })
            .collect();
        tickets.sort();
        let mut expected: Vec<String> = (0..n).map(|i| format!("t{i}")).collect();
        expected.sort();
        assert_eq!(
            tickets, expected,
            "every submitted ticket returns in exactly one terminal"
        );
    }

    #[test]
    fn command_path_backpressure_refuses_typed_and_conserves_the_ticket() {
        // DETERMINISTIC wedge with a SYNCHRONIZATION POINT (Codex u3
        // round 3): the probe proves the loop ENTERED its second
        // RootsUpdateIgnored send and has not completed it — a wedged
        // loop, not a descheduled one. event_capacity=1, nobody drains.
        // The epoch floor comes from a REAL brought-up origin (impl
        // review r1 P1-2: a foreign publication no longer moves the
        // floor), whose acceptance marker is drained deterministically;
        // two stale publishes then wedge: Ignored #1 fills the slot,
        // Ignored #2 blocks the loop mid-send.
        use std::sync::atomic::Ordering::SeqCst;
        let probe = std::sync::Arc::new(LoopSendProbe::default());
        let mut config = test_config();
        config.command_capacity = 1;
        config.event_capacity = 1;
        config.send_probe = Some(std::sync::Arc::clone(&probe));
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        let (g, _c) = bring_up(&handle, "mcp:test", 5);
        drain_until_roots_accepted(&handle);
        publish(&handle, "mcp:test", g, 3); // stale → Ignored #1 fills the slot
        publish(&handle, "mcp:test", g, 2); // stale → Ignored #2 blocks mid-send

        // FIRED-PROOF: the loop's OWN try_send observed a full channel —
        // the wedge condition, positively witnessed. From that point the
        // loop is inside the arm awaiting the recovered send and cannot
        // consume commands until we drain.
        let deadline = Instant::now() + Duration::from_secs(10);
        while probe.blocked_on_full.load(SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "loop never observed Full: completed={}",
                probe.completed.load(SeqCst)
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // With the loop provably blocked, the cap-1 command channel gives
        // exactly one Ok then Full — deterministically.
        let stale = || McpCommand::UpdateAcceptedRoots {
            origin: awili_tools::ToolOrigin::new("mcp:test"),
            generation: g,
            epoch: 1,
        };
        assert_eq!(handle.submit(stale()), Ok(()), "one slot buffers");
        assert_eq!(
            handle.submit(stale()),
            Err(awili_tools::runtime::SubmitError::Full),
            "the bounded channel must refuse — backpressure, not buffering"
        );

        // CAUSAL unwedge: draining the plug completes send #2.
        let first = handle.recv_event_blocking().expect("the plug event");
        assert!(matches!(first, McpEvent::RootsUpdateIgnored { .. }));
        let deadline = Instant::now() + Duration::from_secs(10);
        while probe.completed.load(SeqCst) < 2 {
            assert!(
                Instant::now() < deadline,
                "send #2 never completed after drain"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // Shutdown: the remaining Ignored events (send #2's, plus the one
        // buffered stale command's) drain out — nothing else.
        let drained = handle.shutdown_and_drain();
        assert_eq!(
            drained
                .iter()
                .filter(|e| matches!(e, McpEvent::RootsUpdateIgnored { .. }))
                .count(),
            2,
            "expected exactly the two remaining Ignored events: {drained:?}"
        );
        assert_eq!(drained.len(), 2, "nothing but Ignored events: {drained:?}");
    }

    /// Marker argv makes pgrep -f match ONLY this test's children —
    /// tests run in parallel and other tests spawn testservers too.
    fn marked_spec(marker: &str, fault: &[(&str, &str)]) -> StdioServerSpec {
        let mut envs: Vec<(String, String)> = fault
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        envs.push(("MCP_TEST_MARKER".into(), marker.into()));
        // The testserver ignores unknown argv; the marker only exists so
        // `pgrep -f` can find these exact children.
        StdioServerSpec::new(testserver_bin(), vec![marker.to_string()], envs).unwrap()
    }

    fn assert_no_children_matching(marker: &str) {
        let out = std::process::Command::new("pgrep")
            .args(["-f", marker])
            .output()
            .expect("pgrep");
        assert!(
            out.stdout.is_empty(),
            "zombie testserver children for {marker}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    #[test]
    fn no_zombie_children_after_clean_shutdown() {
        let marker = format!("awili-zombie-clean-{}", std::process::id());
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:test"),
            marked_spec(&marker, &[]),
        );
        let handle = spawn_published(catalog, test_config(), &["mcp:test"]);
        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", r#"{"text":"x"}"#),
        );
        let _finished = next_event(&handle, EVENT_WAIT);
        let _ = handle.shutdown_and_drain();
        assert_no_children_matching(&marker);
    }

    /// The leak the first run of this suite actually caught: a HUNG call
    /// is aborted at shutdown while its task still shares the session Rc
    /// — teardown must still reap the child, never skip it.
    #[test]
    fn no_zombie_children_after_shutdown_with_hung_call() {
        let marker = format!("awili-zombie-hang-{}", std::process::id());
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:hang"),
            marked_spec(&marker, &[("MCP_FAULT", "hang")]),
        );
        let mut config = test_config();
        config.call_timeout = Duration::from_secs(60);
        config.shutdown_grace = Duration::from_millis(200);
        let handle = spawn_published(catalog, config, &["mcp:hang"]);
        submit(
            &handle,
            "t1",
            "mcp:hang",
            call("c1", "echo", r#"{"text":"x"}"#),
        );
        std::thread::sleep(Duration::from_millis(200)); // pass the commit point
        let drained = handle.shutdown_and_drain();
        assert!(drained.iter().any(|e| matches!(
            e,
            McpEvent::CallFinished {
                disposition: CallDisposition::InFlightUnresolved,
                ..
            }
        )));
        assert_no_children_matching(&marker);
    }

    /// The C1→B2→C2 seam with REAL types: policy evidence → intent →
    /// Dispatchable ticket → dispatch_started → runtime executes →
    /// terminal returns THE ticket → dispatch_finalized. The ticket is
    /// not Clone; this test compiling is half the point.
    #[test]
    fn b2_ticket_flows_through_the_runtime_and_finalizes() {
        use awili_engine_log::writer::{ExecutionLog, IntentOutcome};
        use awili_engine_log::{test_bundle, MemLog};

        // The shared fixture (unit 4 r7) owns this test process's sole
        // authority issuance.
        let bundle = test_bundle();
        let gate = &bundle.0;
        let mut log = ExecutionLog::open(MemLog::default(), "sess-c2u2").unwrap();
        let c = call("c1", "echo", r#"{"text":"seam"}"#);
        let evidence = gate
            .policy_evidence(
                &bundle.2.mint(awili_engine::ToolIdentity::new(
                    awili_engine::ToolOrigin::new("builtin:fs"),
                    "echo",
                    awili_engine::DescriptorDigest([0u8; 32]),
                )),
                "turn-1",
                &c,
                awili_engine::RiskClass::ReadOnly,
                None,
            )
            .unwrap();
        let outcome = log.intent("turn-1", &c, evidence).unwrap();
        let ticket = match outcome {
            IntentOutcome::Dispatchable(t) => t,
            other => panic!("expected Dispatchable, got {other:?}"),
        };
        log.dispatch_started(&ticket).unwrap();

        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle =
            spawn_mcp_runtime::<awili_engine_log::writer::IntentTicket>(catalog, test_config());
        handle
            .submit(McpCommand::ConnectOrigin {
                origin: awili_tools::ToolOrigin::new("mcp:test"),
            })
            .unwrap();
        let generation = loop {
            match handle.recv_event_blocking() {
                Some(McpEvent::ServerConnected(c)) => break c.generation(),
                Some(_) => continue,
                None => panic!("event stream closed during bring-up"),
            }
        };
        handle
            .submit(McpCommand::UpdateAcceptedRoots {
                origin: awili_tools::ToolOrigin::new("mcp:test"),
                generation,
                epoch: 1,
            })
            .unwrap();
        handle
            .submit(McpCommand::CallTool {
                ticket,
                intent_id: "turn-1-intent-0000".into(),
                submission_id: SubmissionId(0),
                origin: awili_tools::ToolOrigin::new("mcp:test"),
                call: c,
                generation,
                root_epoch: 1,
            })
            .unwrap();

        let deadline = Instant::now() + EVENT_WAIT;
        let (ticket_back, disposition, result) = loop {
            if let Some(McpEvent::CallFinished {
                ticket,
                disposition,
                result,
                ..
            }) = handle.try_recv_event()
            {
                break (ticket, disposition, result);
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(disposition, CallDisposition::Completed);
        assert_eq!(disposition.certainty(), ExecutionCertainty::Definite);
        assert_eq!(result.content, "seam");

        // Engine-side rule: Definite → finalize with the RETURNED ticket.
        log.dispatch_finalized(&ticket_back, !result.is_error, &result.content)
            .unwrap();
        let _ = handle.shutdown_and_drain();
    }

    /// A duplicate in-flight intent_id is rejected via the NON-terminal
    /// SubmissionRejected (ticket returned); the original intent still
    /// gets its single CallFinished — INV-C2-5 per-intent (Codex u3 P0-2).
    #[test]
    fn duplicate_inflight_intent_id_is_rejected_without_a_second_terminal() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:hang"),
            spec(&[("MCP_FAULT", "hang")]),
        );
        let mut config = test_config();
        config.call_timeout = Duration::from_secs(60);
        config.shutdown_grace = Duration::from_millis(200);
        let handle = spawn_published(catalog, config, &["mcp:hang"]);

        // Same intent_id twice: first hangs in flight, second must be
        // refused (never dispatched, Definite).
        let hang_g = TEST_GENERATIONS
            .with(|m| m.borrow().get("mcp:hang").copied())
            .expect("spawn_published brought mcp:hang up");
        let dup = move |ticket: &str, cid: &str| McpCommand::CallTool {
            ticket: ticket.to_string(),
            intent_id: "intent-dup".to_string(),
            submission_id: SubmissionId(2),
            origin: awili_tools::ToolOrigin::new("mcp:hang"),
            call: call(cid, "echo", r#"{"text":"x"}"#),
            generation: hang_g,
            root_epoch: 1,
        };
        handle.submit(dup("t1", "c1")).unwrap();
        // FIRED-PROOF (Codex round 2, P1): affirmative evidence the first
        // call is still blocked — with `hang` silently off, its Definite
        // terminal would arrive here and this assertion would fail.
        let quiet_until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < quiet_until {
            match handle.try_recv_event() {
                None
                | Some(McpEvent::OriginDisconnected { .. })
                | Some(McpEvent::OriginConnecting { .. })
                | Some(McpEvent::RootsAccepted { .. }) => {}
                Some(other) => {
                    panic!("first call completed — the hang fault did not fire: {other:?}")
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.submit(dup("t2", "c2")).unwrap();

        let refused = next_event(&handle, EVENT_WAIT);
        // INV-C2-5 is PER-INTENT: the duplicate must NOT get a CallFinished
        // (the original's single terminal is still coming). Its ticket
        // returns via the non-terminal SubmissionRejected (Codex u3 P0-2).
        let McpEvent::SubmissionRejected {
            ticket,
            intent_id,
            submission_id,
            reason,
        } = refused
        else {
            panic!("expected SubmissionRejected, got {refused:?}");
        };
        assert_eq!(ticket, "t2");
        assert_eq!(intent_id, "intent-dup");
        assert_eq!(submission_id, SubmissionId(2));
        assert!(reason.contains("duplicate"));

        // Shutdown resolves the original as Uncertain — the intent's ONE
        // and ONLY CallFinished.
        let drained = handle.shutdown_and_drain();
        let terminals: Vec<_> = drained
            .iter()
            .filter_map(|e| match e {
                McpEvent::CallFinished {
                    ticket,
                    intent_id,
                    disposition,
                    ..
                } => Some((ticket.clone(), intent_id.clone(), *disposition)),
                _ => None,
            })
            .collect();
        assert_eq!(
            terminals,
            vec![(
                "t1".to_string(),
                "intent-dup".to_string(),
                CallDisposition::InFlightUnresolved
            )],
            "exactly ONE CallFinished for the intent"
        );
    }

    /// Finding 4: a PANICKING dispatch task must not lose the ticket, must
    /// resolve Uncertain, and must tear its session down (fail closed) —
    /// the next call to that origin reconnects.
    #[test]
    fn dispatch_panic_returns_ticket_uncertain_and_tears_session_down() {
        let marker = format!("awili-panic-{}", std::process::id());
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:test"),
            marked_spec(&marker, &[]),
        );
        let handle = spawn_published(catalog, test_config(), &["mcp:test"]);

        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "__awili_test_panic", "{}"),
        );
        let mut saw_died = false;
        let mut saw_terminal = false;
        while !(saw_died && saw_terminal) {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::OriginDisconnected { .. }
                | McpEvent::OriginConnecting { .. }
                | McpEvent::RootsAccepted { .. } => {}
                McpEvent::ServerConnected { .. } => {}
                McpEvent::ServerDied { detail, .. } => {
                    assert!(detail.contains("panicked"));
                    saw_died = true;
                }
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    result,
                    ..
                } => {
                    assert_eq!(ticket, "t1");
                    assert_eq!(disposition, CallDisposition::InFlightUnresolved);
                    assert!(result.is_error);
                    assert!(result.content.contains("panicked"));
                    saw_terminal = true;
                }
                McpEvent::RootsUpdateIgnored { .. } => panic!("no epoch updates in this test"),
                McpEvent::SubmissionRejected { .. } => panic!("no duplicates in this test"),
            }
        }
        assert!(saw_died && saw_terminal);

        // Session was torn down: an explicit reconnect (a NEW generation,
        // unit 4b) brings the origin back and the next call works.
        let (_g2, _c) = bring_up(&handle, "mcp:test", 1);
        let reconnected = true;
        submit(
            &handle,
            "t2",
            "mcp:test",
            call("c2", "echo", r#"{"text":"after-panic"}"#),
        );
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerConnected { .. } => {}
                McpEvent::CallFinished { ticket, result, .. } => {
                    assert_eq!(ticket, "t2");
                    assert_eq!(result.content, "after-panic");
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(reconnected, "torn-down session must reconnect fresh");
        let _ = handle.shutdown_and_drain();
        assert_no_children_matching(&marker);
    }

    /// Finding 4 + blocker 1: commands still QUEUED when shutdown begins
    /// are refused as Definite never-dispatched — proven by wedging the
    /// loop on a full event channel (crash server: ServerDied blocks) so
    /// submissions genuinely queue.
    #[test]
    fn queued_commands_at_shutdown_are_definite_never_dispatched() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:crash"),
            spec(&[("MCP_FAULT", "crash_on_call")]),
        );
        let mut config = test_config();
        config.event_capacity = 1;
        let handle = spawn_published(catalog, config, &["mcp:crash"]);

        submit(
            &handle,
            "t1",
            "mcp:crash",
            call("c1", "echo", r#"{"text":"x"}"#),
        );
        // Drain ServerConnected so the dispatch proceeds past its commit
        // point; the crash then wedges the loop mid-ServerDied-send (cap
        // 1, nobody draining).
        std::thread::sleep(Duration::from_millis(500));
        submit(
            &handle,
            "t2",
            "mcp:crash",
            call("c2", "echo", r#"{"text":"x"}"#),
        );
        submit(
            &handle,
            "t3",
            "mcp:crash",
            call("c3", "echo", r#"{"text":"x"}"#),
        );

        let drained = handle.shutdown_and_drain();
        let mut by_ticket: std::collections::HashMap<String, (CallDisposition, String)> =
            std::collections::HashMap::new();
        let mut server_died = false;
        for e in &drained {
            match e {
                McpEvent::OriginDisconnected { .. }
                | McpEvent::OriginConnecting { .. }
                | McpEvent::RootsAccepted { .. } => {}
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    result,
                    ..
                } => {
                    let prev =
                        by_ticket.insert(ticket.clone(), (*disposition, result.content.clone()));
                    assert!(prev.is_none(), "duplicate terminal for {ticket}");
                }
                McpEvent::ServerDied { origin, .. } => {
                    assert_eq!(origin.origin, "mcp:crash");
                    server_died = true;
                }
                McpEvent::ServerConnected { .. } => {}
                McpEvent::RootsUpdateIgnored { .. } => panic!("no epoch updates in this test"),
                McpEvent::SubmissionRejected { .. } => panic!("no duplicates in this test"),
            }
        }
        // FIRED-PROOF (Codex round 2, P1): nothing consumed events before
        // the drain, so t1's terminal and the ServerDied are BOTH here.
        // With crash_on_call silently off, t1 would be Definite-ok and no
        // ServerDied would exist — the wedge would have been a different
        // (unproven) path.
        assert!(server_died, "the crash fault did not fire (no ServerDied)");
        let (t1_disposition, _) = &by_ticket["t1"];
        assert_eq!(
            *t1_disposition,
            CallDisposition::InFlightUnresolved,
            "t1 must be the crashed call"
        );
        // t2/t3 MUST be here; t1's Uncertain terminal may legitimately
        // arrive pre-drain or in the drain — either, but never twice (the
        // insert assert above enforces uniqueness).
        assert!(
            by_ticket.contains_key("t2") && by_ticket.contains_key("t3"),
            "t2/t3 terminals drained: {by_ticket:?}"
        );
        for t in ["t2", "t3"] {
            let (disposition, content) = &by_ticket[t];
            assert_eq!(
                *disposition,
                CallDisposition::NeverDispatched,
                "{t} never dispatched"
            );
            assert!(content.contains("never"), "{t}: {content}");
        }
        // NOTE: t1's Uncertain terminal was consumed pre-drain via the
        // wedged ServerDied ordering or is in the drain — accept either,
        // but never twice.
        let t1_terminals = drained
            .iter()
            .filter(|e| matches!(e, McpEvent::CallFinished { ticket, .. } if ticket == "t1"))
            .count();
        assert!(t1_terminals <= 1);
    }

    /// Blocker 1: shutdown arriving DURING a slow connect refuses the call
    /// as Definite never-dispatched (gate fault: slow_initialize).
    #[test]
    fn shutdown_during_connect_is_definite_never_dispatched() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:slowinit"),
            spec(&[("MCP_FAULT", "slow_initialize"), ("MCP_DELAY_MS", "1500")]),
        );
        let dir = crate::test_tmp::fresh_dir("u3-sdc");
        std::fs::create_dir_all(&dir).unwrap();
        let canary = dir.join("must-not-exist.txt");
        let _ = std::fs::remove_file(&canary);
        let mut config = test_config();
        config.connect_timeout = Duration::from_secs(10);
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        // Unit 4b: dispatch never connects. The origin is still CONNECTING
        // (handshake sleeps 1500ms server-side) when the call arrives, so
        // admission refuses it DEFINITE — and shutdown mid-handshake must
        // leave exactly that one refusal terminal and an untouched canary.
        handle
            .submit(McpCommand::ConnectOrigin {
                origin: awili_tools::ToolOrigin::new("mcp:slowinit"),
            })
            .unwrap();
        // The origin is still CONNECTING: no accepted generation exists,
        // so the refusal-path submission fabricates one explicitly —
        // admission refuses on state before generation.
        submit_epoch_with_generation(
            &handle,
            "t1",
            "mcp:slowinit",
            call(
                "c1",
                "touch_file",
                &format!(
                    r#"{{"path":{}}}"#,
                    serde_json::to_string(canary.to_str().unwrap()).unwrap()
                ),
            ),
            awili_engine::generation::GenerationAllocator::new().next(),
            1,
        );
        // Shut down while the handshake is still sleeping server-side.
        std::thread::sleep(Duration::from_millis(300));
        let drained = handle.shutdown_and_drain();
        let terminals: Vec<_> = drained
            .iter()
            .filter_map(|e| match e {
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    result,
                    ..
                } => Some((ticket.clone(), *disposition, result.content.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(terminals.len(), 1);
        let (ticket, disposition, content) = &terminals[0];
        assert_eq!(ticket, "t1");
        // Aborted mid-connect: uncommitted, no cancel => NeverDispatched.
        assert_eq!(
            *disposition,
            CallDisposition::NeverDispatched,
            "the call never left the process: {content}"
        );
        assert!(content.contains("never"), "{content}");
        // Transport-touch canary: give any stray write a moment, then
        // assert the call really never ran.
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !canary.exists(),
            "shutdown-during-connect call must never run"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Finding 4: one broken call must not strand a CONCURRENT call on the
    /// same session — both resolve (Uncertain), one ServerDied, no zombie.
    #[test]
    fn concurrent_calls_on_a_breaking_session_all_resolve() {
        let marker = format!("awili-concurrent-{}", std::process::id());
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:break"),
            marked_spec(&marker, &[("MCP_FAULT", "slow"), ("MCP_DELAY_MS", "800")]),
        );
        // slow + crash_after can't combine (single MCP_FAULT); instead:
        // slow makes call 1 occupy the server while calls 2..3 are already
        // written to its stdin, then we SIGKILL via shutdown—— no: this
        // test wants a mid-run break. Use crash_after=1: call 1 is served,
        // calls 2 and 3 die with the server.
        drop(catalog);
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:break"),
            marked_spec(&marker, &[("MCP_FAULT", "crash_after=1")]),
        );
        let handle = spawn_published(catalog, test_config(), &["mcp:break"]);
        submit(
            &handle,
            "t1",
            "mcp:break",
            call("c1", "echo", r#"{"text":"one"}"#),
        );
        submit(
            &handle,
            "t2",
            "mcp:break",
            call("c2", "echo", r#"{"text":"two"}"#),
        );
        submit(
            &handle,
            "t3",
            "mcp:break",
            call("c3", "echo", r#"{"text":"three"}"#),
        );

        let mut terminals: std::collections::HashMap<String, CallDisposition> =
            std::collections::HashMap::new();
        let mut died = 0;
        let deadline = Instant::now() + EVENT_WAIT;
        while terminals.len() < 3 {
            assert!(Instant::now() < deadline, "got {terminals:?}, died={died}");
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::OriginDisconnected { .. }
                | McpEvent::OriginConnecting { .. }
                | McpEvent::RootsAccepted { .. } => {}
                McpEvent::ServerConnected { .. } => {}
                McpEvent::ServerDied { .. } => died += 1,
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    ..
                } => {
                    let prev = terminals.insert(ticket.clone(), disposition);
                    assert!(prev.is_none(), "duplicate terminal for {ticket}");
                }
                McpEvent::RootsUpdateIgnored { .. } => panic!("no epoch updates in this test"),
                McpEvent::SubmissionRejected { .. } => panic!("no duplicates in this test"),
            }
        }
        // Call 1 was served before the crash; 2 and 3 were on the dead
        // session — sent or sitting in its stdin, either way unknowable.
        assert_eq!(terminals["t1"], CallDisposition::Completed);
        assert_eq!(terminals["t2"], CallDisposition::InFlightUnresolved);
        assert_eq!(terminals["t3"], CallDisposition::InFlightUnresolved);
        assert_eq!(died, 1, "one ServerDied for the shared session");
        let _ = handle.shutdown_and_drain();
        assert_no_children_matching(&marker);
    }

    /// Finding 4 (bounds, live): an oversized echo result is truncated
    /// UNDER the cap with the visible marker — still Definite, still ok.
    #[test]
    fn oversized_result_content_is_bounded_with_marker() {
        use crate::mcp_transport::MAX_RESULT_CONTENT_BYTES;
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_published(catalog, test_config(), &["mcp:test"]);
        let big = "z".repeat(MAX_RESULT_CONTENT_BYTES + 4096);
        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", &format!(r#"{{"text":"{big}"}}"#)),
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            disposition,
            result,
            ..
        } = finished
        else {
            panic!("expected CallFinished, got {finished:?}");
        };
        assert_eq!(disposition, CallDisposition::Completed);
        assert!(!result.is_error);
        assert!(result.content.len() <= MAX_RESULT_CONTENT_BYTES);
        assert!(result.content.contains("truncated"));
        let _ = handle.shutdown_and_drain();
    }

    // ── C2-u3: cancellation ────────────────────────────────────────────

    /// §5.2 race 1 + the cancel-during-connect race Codex named: cancel
    /// lands while the handshake sleeps server-side (deterministic — the
    /// loop is connect-free in u3). touch_file canary proves NO tools/call
    /// was ever sent.
    #[test]
    fn cancel_during_connect_is_definite_and_never_touches_transport() {
        let dir = crate::test_tmp::fresh_dir("u3-cancel");
        std::fs::create_dir_all(&dir).unwrap();
        let canary = dir.join("must-not-exist.txt");
        let _ = std::fs::remove_file(&canary);

        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:slowinit"),
            spec(&[("MCP_FAULT", "slow_initialize"), ("MCP_DELAY_MS", "1500")]),
        );
        let mut config = test_config();
        config.connect_timeout = Duration::from_secs(10);
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        // Unit 4b: dispatch never connects. While the origin is still
        // CONNECTING (the handshake sleeps 1500ms server-side), a call is
        // refused DEFINITE at admission — it cannot touch the transport,
        // which is the property this test pins.
        handle
            .submit(McpCommand::ConnectOrigin {
                origin: awili_tools::ToolOrigin::new("mcp:slowinit"),
            })
            .unwrap();
        // The origin is still CONNECTING: no accepted generation exists,
        // so the refusal-path submission fabricates one explicitly —
        // admission refuses on state before generation.
        submit_epoch_with_generation(
            &handle,
            "t1",
            "mcp:slowinit",
            call(
                "c1",
                "touch_file",
                &format!(
                    r#"{{"path":{}}}"#,
                    serde_json::to_string(canary.to_str().unwrap()).unwrap()
                ),
            ),
            awili_engine::generation::GenerationAllocator::new().next(),
            1,
        );

        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            disposition,
            result,
            ..
        } = finished
        else {
            panic!("expected CallFinished, got {finished:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(disposition, CallDisposition::NeverDispatched);
        assert_eq!(disposition.certainty(), ExecutionCertainty::Definite);
        assert!(result.is_error);

        // Runtime is alive and the origin becomes usable once the slow
        // handshake lands and C1 accepts its generation.
        let connected = next_event(&handle, Duration::from_secs(20));
        let McpEvent::ServerConnected(connected) = connected else {
            panic!("expected ServerConnected, got {connected:?}");
        };
        handle
            .submit(McpCommand::UpdateAcceptedRoots {
                origin: awili_tools::ToolOrigin::new("mcp:slowinit"),
                generation: connected.generation(),
                epoch: 1,
            })
            .unwrap();
        // Register the manually accepted generation — the plain helper
        // must never guess (impl review r2 finding 1).
        TEST_GENERATIONS.with(|m| {
            m.borrow_mut()
                .insert("mcp:slowinit".to_string(), connected.generation())
        });
        submit(
            &handle,
            "t2",
            "mcp:slowinit",
            call("c2", "echo", r#"{"text":"after"}"#),
        );
        loop {
            match next_event(&handle, Duration::from_secs(20)) {
                McpEvent::ServerConnected { .. } => {}
                McpEvent::CallFinished { ticket, result, .. } => {
                    assert_eq!(ticket, "t2");
                    assert_eq!(result.content, "after");
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        let _ = handle.shutdown_and_drain();
        assert!(!canary.exists(), "tools/call must never have been sent");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Tombstone FIFO: Cancel(X) then CallTool(X) — pure command order,
    /// no wedge. The call is refused CancelledBeforeDispatch and the
    /// canary proves it never ran.
    #[test]
    fn cancel_preceding_the_call_tombstones_it() {
        let dir = crate::test_tmp::fresh_dir("u3-tomb");
        std::fs::create_dir_all(&dir).unwrap();
        let canary = dir.join("must-not-exist.txt");
        let _ = std::fs::remove_file(&canary);

        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_published(catalog, test_config(), &["mcp:test"]);
        cancel(&handle, "t1"); // BEFORE the CallTool
        submit(
            &handle,
            "t1",
            "mcp:test",
            call(
                "c1",
                "touch_file",
                &format!(
                    r#"{{"path":{}}}"#,
                    serde_json::to_string(canary.to_str().unwrap()).unwrap()
                ),
            ),
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            disposition,
            ..
        } = finished
        else {
            panic!("expected CallFinished, got {finished:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(disposition, CallDisposition::CancelledBeforeDispatch);
        let _ = handle.shutdown_and_drain();
        assert!(!canary.exists(), "tombstoned call must never dispatch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.2 race 2: cancel after the request was sent (server sleeping in
    /// `slow`) → InFlightUnresolved, exactly one terminal. FIRED-PROOF:
    /// the call is touch_file; after the cancel terminal, the file MUST
    /// eventually appear — the sleeping server finishes its work — which
    /// proves the request had genuinely reached it when we cancelled. If
    /// the cancel had beaten the send, the file could never appear (and
    /// the disposition would have been CancelledBeforeDispatch, also
    /// failing the assert).
    #[test]
    fn cancel_in_flight_is_unresolved_exactly_once() {
        let dir = crate::test_tmp::fresh_dir("u3-race2");
        std::fs::create_dir_all(&dir).unwrap();
        let canary = dir.join("appears-later.txt");
        let _ = std::fs::remove_file(&canary);

        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:slow"),
            spec(&[("MCP_FAULT", "slow"), ("MCP_DELAY_MS", "2000")]),
        );
        let mut config = test_config();
        config.call_timeout = Duration::from_secs(60);
        let handle = spawn_published(catalog, config, &["mcp:slow"]);
        submit(
            &handle,
            "t1",
            "mcp:slow",
            call(
                "c1",
                "touch_file",
                &format!(
                    r#"{{"path":{}}}"#,
                    serde_json::to_string(canary.to_str().unwrap()).unwrap()
                ),
            ),
        );
        std::thread::sleep(Duration::from_millis(300));
        cancel(&handle, "t1");

        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            disposition,
            result,
            ..
        } = finished
        else {
            panic!("expected CallFinished, got {finished:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(disposition, CallDisposition::InFlightUnresolved);
        assert!(result.content.contains("cancelled"), "{}", result.content);

        // The canary appears: the request WAS in flight (fired-proof).
        let deadline = Instant::now() + Duration::from_secs(10);
        while !canary.exists() {
            assert!(
                Instant::now() < deadline,
                "canary never appeared — the request never reached the server, \
                 so this test proved nothing"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        // §5.2 race 3 shape + idempotency: cancel again — no second
        // terminal, ever.
        cancel(&handle, "t1");
        let quiet_until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < quiet_until {
            if let Some(ev) = handle.try_recv_event() {
                assert!(
                    !matches!(ev, McpEvent::CallFinished { .. }),
                    "second terminal for a cancelled call: {ev:?}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let drained = handle.shutdown_and_drain();
        assert!(
            drained
                .iter()
                .all(|e| !matches!(e, McpEvent::CallFinished { .. })),
            "no stray terminals: {drained:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.2 race 3, INTEGRATION (deterministic): a COMPLETED result is
    /// wedged undelivered (event channel full) when Cancel arrives. The
    /// real result must win — one terminal, disposition Completed, real
    /// content, no synthetic cancellation.
    #[test]
    fn cancellation_during_a_live_call_returns_the_linearized_winner() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:slow"),
            spec(&[("MCP_FAULT", "slow"), ("MCP_DELAY_MS", "500")]),
        );
        let mut config = test_config();
        config.event_capacity = 1;
        config.call_timeout = Duration::from_secs(10);
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        let _ = bring_up(&handle, "mcp:slow", 1);
        // Capacity-1 wedge: drain the acceptance marker deterministically
        // before sequencing the wedge.
        drain_until_roots_accepted(&handle);

        // Sequence the wedge to exercise the LIVE arbitration (Codex u3
        // round 2, P1): the loop must be BLOCKED — not merely behind — so
        // the slow call COMPLETES while its pending entry is still
        // un-joined. Two stale publishes: the first RootsUpdateIgnored
        // occupies the single event slot, the second blocks the loop
        // mid-send. The 500ms call then finishes into the JoinSet with
        // pending intact; the queued Cancel and the ready join result
        // arbitrate ONLY after we unwedge — and in either select order
        // the real result must win (Cancel-first: abort is a no-op on a
        // finished task, join Ok still delivers; join-first: the late
        // cancel is absorbed by recently-terminal memory).
        submit(
            &handle,
            "t1",
            "mcp:slow",
            call("c1", "echo", r#"{"text":"real"}"#),
        );
        publish(
            &handle,
            "mcp:slow",
            awili_engine::generation::GenerationAllocator::new().next(),
            0,
        ); // stale → Ignored occupies the slot
        publish(
            &handle,
            "mcp:slow",
            awili_engine::generation::GenerationAllocator::new().next(),
            0,
        ); // stale → loop BLOCKS mid-send
        std::thread::sleep(Duration::from_millis(900)); // call completes, un-joined
        cancel(&handle, "t1"); // queued behind the wedge

        // Unwedge and read through: two Ignored, then the REAL result.
        let mut call_terminals = Vec::new();
        let mut ignored = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while call_terminals.is_empty() {
            assert!(Instant::now() < deadline);
            match handle.try_recv_event() {
                Some(McpEvent::CallFinished {
                    ticket,
                    disposition,
                    result,
                    ..
                }) => {
                    call_terminals.push((ticket, disposition, result.content.clone()));
                }
                Some(McpEvent::RootsUpdateIgnored { .. }) => ignored += 1,
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert_eq!(
            ignored, 2,
            "the wedge did not fire (both Ignored must precede)"
        );
        assert_eq!(
            call_terminals,
            vec![(
                "t1".to_string(),
                CallDisposition::Completed,
                "real".to_string()
            )],
            "the queued completion must win over the cancel"
        );
        // And nothing else ever arrives for that intent.
        let quiet_until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < quiet_until {
            if let Some(ev) = handle.try_recv_event() {
                assert!(
                    !matches!(ev, McpEvent::CallFinished { .. }),
                    "second terminal: {ev:?}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = handle.shutdown_and_drain();
    }

    /// §5.2 race 3 mechanism pin: tokio abort AFTER task completion does
    /// not destroy the completed output — `join_next` yields `Ok`. The
    /// queued completion always wins.
    #[test]
    fn abort_after_completion_keeps_the_real_result() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let mut set: tokio::task::JoinSet<u32> = tokio::task::JoinSet::new();
            let h = set.spawn_local(async { 42u32 });
            // Let the task complete.
            tokio::task::yield_now().await;
            tokio::task::yield_now().await;
            h.abort(); // too late — the output is already stored
            match set.join_next().await {
                Some(Ok(v)) => assert_eq!(v, 42, "real result must win"),
                other => panic!("abort destroyed a completed result: {other:?}"),
            }
        });
    }

    /// Cancel racing the call timeout: whichever fires, EXACTLY ONE
    /// InFlightUnresolved terminal.
    #[test]
    fn cancel_racing_timeout_yields_exactly_one_unresolved_terminal() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:slow"),
            spec(&[("MCP_FAULT", "slow"), ("MCP_DELAY_MS", "5000")]),
        );
        let mut config = test_config();
        config.call_timeout = Duration::from_millis(600);
        let handle = spawn_published(catalog, config, &["mcp:slow"]);
        submit(
            &handle,
            "t1",
            "mcp:slow",
            call("c1", "echo", r#"{"text":"x"}"#),
        );
        std::thread::sleep(Duration::from_millis(550)); // knife-edge
        cancel(&handle, "t1");

        let mut terminals = 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(McpEvent::CallFinished {
                ticket,
                disposition,
                ..
            }) = handle.try_recv_event()
            {
                assert_eq!(ticket, "t1");
                assert_eq!(disposition, CallDisposition::InFlightUnresolved);
                terminals += 1;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(terminals, 1, "exactly one terminal under the race");
        let _ = handle.shutdown_and_drain();
    }

    /// Cancel after the terminal was already delivered: absorbed, no
    /// second terminal (idempotency at the already-terminal edge). The
    /// reuse probe below keeps proving the P1-4 invariant — a late
    /// cancel must NOT tombstone the id — under the unit 4 r6 Delta B″
    /// admission rule: reuse inside the bounded terminal-history window
    /// is refused as a DUPLICATE (non-terminal `SubmissionRejected`),
    /// never poisoned into a `CancelledBeforeDispatch` refusal (the
    /// tombstone ladder runs first, so a tombstoned id would produce
    /// exactly that refusal terminal instead).
    #[test]
    fn cancel_after_completion_is_a_noop() {
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_published(catalog, test_config(), &["mcp:test"]);
        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", r#"{"text":"done"}"#),
        );
        let _finished = next_event(&handle, EVENT_WAIT);
        cancel(&handle, "t1"); // ABSORBED: intent already terminal (P1-4)
        let quiet_until = Instant::now() + Duration::from_millis(400);
        while Instant::now() < quiet_until {
            assert!(
                handle.try_recv_event().is_none(),
                "no event after late cancel"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // The late cancel must NOT have tombstoned the id (P1-4: a true
        // no-op, no state mutation). Reusing the id hits the
        // recently-terminal ADMISSION rule (unit 4 r6 Delta B″): a
        // non-terminal duplicate rejection carrying the reuse's own
        // ticket — never a poisoned CancelledBeforeDispatch refusal.
        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1b", "echo", r#"{"text":"reused"}"#),
        );
        let refused = next_event(&handle, EVENT_WAIT);
        let McpEvent::SubmissionRejected {
            ticket,
            intent_id,
            reason,
            ..
        } = refused
        else {
            panic!("expected SubmissionRejected for the reused id, got {refused:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(intent_id, "intent-t1");
        assert!(
            reason.contains("already terminal"),
            "admission history, not a tombstone poison: {reason}"
        );
        let _ = handle.shutdown_and_drain();
    }

    /// Tombstone memory is BOUNDED: unknown-cancel spray cannot grow it
    /// past the cap, and eviction is FIFO.
    #[test]
    fn tombstones_are_bounded_fifo() {
        let mut t = BoundedIdSet::new(3);
        for i in 0..10 {
            t.insert(format!("i{i}"));
        }
        assert_eq!(t.len(), 3);
        assert!(!t.take("i0"), "oldest evicted");
        assert!(!t.take("i6"), "evicted");
        assert!(t.take("i9"), "newest kept");
        assert!(t.take("i8"));
        assert!(!t.take("i8"), "take consumes");
    }

    // ── C2-u3: root epochs ─────────────────────────────────────────────

    /// Unit 5 r8 §2 (ruling 41): two runtimes in ONE process receive
    /// DIFFERENT first-connection generations — the process-long allocator
    /// is what makes a retained pre-respawn generation refusable by value.
    #[test]
    fn two_runtimes_first_connections_get_distinct_generations() {
        let mk = || {
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]))
        };
        let h1 = spawn_mcp_runtime::<String>(mk(), test_config());
        let (g1, _c1) = bring_up(&h1, "mcp:test", 1);
        let h2 = spawn_mcp_runtime::<String>(mk(), test_config());
        let (g2, _c2) = bring_up(&h2, "mcp:test", 1);
        assert_ne!(g1, g2, "respawn fence: generations are process-unique");
        assert!(g2 > g1, "and monotonic across runtimes");
        let _ = h1.shutdown_and_drain();
        let _ = h2.shutdown_and_drain();
    }

    /// Refuse-until-published: no implicit epoch-0 authority.
    #[test]
    fn calls_are_refused_until_c1_publishes_an_epoch() {
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        // NOTE: plain spawn — nothing published.
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        // Refusal path on a never-accepted origin: the generation is an
        // explicit fabrication — admission refuses on state first.
        submit_epoch_with_generation(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", r#"{"text":"x"}"#),
            awili_engine::generation::GenerationAllocator::new().next(),
            0,
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            disposition,
            result,
            ..
        } = finished
        else {
            panic!("expected refusal, got {finished:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(disposition, CallDisposition::NeverDispatched);
        assert!(
            result.content.contains("is not ready"),
            "{}",
            result.content
        );
        let _ = handle.shutdown_and_drain();
    }

    /// Stale-epoch refusal + §5.2 race 4 (narrowing invalidates a queued
    /// call): publish 1, then FIFO-submit [UpdateAcceptedRoots(2),
    /// CallTool(epoch 1, touch_file canary)] — the call must be refused
    /// and must never run.
    #[test]
    fn epoch_narrowing_invalidates_the_queued_call() {
        let dir = crate::test_tmp::fresh_dir("u3-epoch");
        std::fs::create_dir_all(&dir).unwrap();
        let canary = dir.join("must-not-exist.txt");
        let _ = std::fs::remove_file(&canary);

        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_published(catalog, test_config(), &["mcp:test"]); // epoch 1
                                                                             // The CURRENT accepted generation, from the per-thread registry
                                                                             // (unit 5: generations are process-unique, so a fabricated fresh
                                                                             // allocator's g1 no longer coincides with the runtime's mint).
        let g = TEST_GENERATIONS
            .with(|m| m.borrow().get("mcp:test").copied())
            .expect("bring_up registered the generation");
        publish(&handle, "mcp:test", g, 2); // C1 narrows: epoch bump
        submit_epoch(
            &handle,
            "t1",
            "mcp:test",
            call(
                "c1",
                "touch_file",
                &format!(
                    r#"{{"path":{}}}"#,
                    serde_json::to_string(canary.to_str().unwrap()).unwrap()
                ),
            ),
            1, // authorized under the OLD epoch
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            disposition,
            result,
            ..
        } = finished
        else {
            panic!("expected refusal, got {finished:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(disposition, CallDisposition::NeverDispatched);
        assert!(
            result.content.contains("stale root epoch"),
            "{}",
            result.content
        );

        // The CURRENT epoch works.
        submit_epoch(
            &handle,
            "t2",
            "mcp:test",
            call("c2", "echo", r#"{"text":"ok"}"#),
            2,
        );
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerConnected { .. } => {}
                McpEvent::CallFinished { ticket, result, .. } => {
                    assert_eq!(ticket, "t2");
                    assert_eq!(result.content, "ok");
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        let _ = handle.shutdown_and_drain();
        assert!(!canary.exists(), "stale-epoch call must never run");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Monotonicity: a LOWER update is refused loudly (RootsUpdateIgnored),
    /// an EQUAL update is silently idempotent, and the table is unchanged
    /// (current-epoch calls still work).
    #[test]
    fn stale_roots_updates_are_refused_loudly_equal_ones_silently() {
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let _ = bring_up(&handle, "mcp:test", 5);
        publish(
            &handle,
            "mcp:test",
            awili_engine::generation::GenerationAllocator::new().next(),
            3,
        ); // stale → RootsUpdateIgnored
        let ev = next_event(&handle, EVENT_WAIT);
        let McpEvent::RootsUpdateIgnored {
            origin,
            stale_epoch,
            current_epoch,
        } = ev
        else {
            panic!("expected RootsUpdateIgnored, got {ev:?}");
        };
        assert_eq!(origin.origin, "mcp:test");
        assert_eq!((stale_epoch, current_epoch), (3, 5));

        publish(
            &handle,
            "mcp:test",
            awili_engine::generation::GenerationAllocator::new().next(),
            5,
        ); // equal → idempotent, silent
        let quiet_until = Instant::now() + Duration::from_millis(400);
        while Instant::now() < quiet_until {
            assert!(
                handle.try_recv_event().is_none(),
                "equal update must be silent"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        // Table intact: epoch 5 dispatches.
        submit_epoch(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", r#"{"text":"ok"}"#),
            5,
        );
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerConnected { .. } => {}
                McpEvent::CallFinished { ticket, result, .. } => {
                    assert_eq!(ticket, "t1");
                    assert_eq!(result.content, "ok");
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        let _ = handle.shutdown_and_drain();
    }

    /// The server's ADVERTISED epoch crosses as a numeric proposal in
    /// `ServerConnected.roots_epoch` — and it does NOT change the accepted
    /// table (a proposal is inert until C1 accepts).
    #[test]
    fn advertised_epoch_is_a_proposal_not_authority() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:test"),
            spec(&[("MCP_ROOTS_EPOCH", "7")]),
        );
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let (_g, connected) = bring_up(&handle, "mcp:test", 1); // accepted = 1
        assert_eq!(
            connected.advertised_epoch(),
            Some(7),
            "numeric _meta proposal surfaces"
        );
        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", r#"{"text":"x"}"#),
        ); // epoch 1

        // The call authorized under ACCEPTED epoch 1 dispatched fine —
        // the advertisement did not mutate the table.
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            disposition,
            ..
        } = finished
        else {
            panic!("expected CallFinished, got {finished:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(disposition, CallDisposition::Completed);

        // And a call claiming the ADVERTISED epoch is refused: never accepted.
        submit_epoch(
            &handle,
            "t2",
            "mcp:test",
            call("c2", "echo", r#"{"text":"x"}"#),
            7,
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            disposition,
            ..
        } = finished
        else {
            panic!("expected refusal, got {finished:?}");
        };
        assert_eq!(ticket, "t2");
        assert_eq!(disposition, CallDisposition::NeverDispatched);
        let _ = handle.shutdown_and_drain();
    }

    // ── C2-u3: flood backpressure ──────────────────────────────────────

    /// A server spraying 50k notifications mid-call must not starve a
    /// clean origin (progress deadline) nor wedge the runtime.
    #[test]
    fn notification_flood_does_not_starve_another_origin() {
        let catalog = McpServerCatalog::new()
            .with_stdio(
                awili_tools::ToolOrigin::new("mcp:flood"),
                spec(&[("MCP_FAULT", "flood"), ("MCP_FLOOD_N", "50000")]),
            )
            .with_stdio(awili_tools::ToolOrigin::new("mcp:ok"), spec(&[]));
        // The flood call's round-trip legitimately outlasts the 1.5s
        // default call timeout under parallel-suite load (the 50k-
        // notification burst is the point of the fixture): with the
        // default, the call's honest timeout terminal
        // (`InFlightUnresolved`) lands inside this test's 20s flood
        // window and reads as a wedge. This test's own deadlines (3s
        // clean / 20s flood) assume a call timeout long enough to
        // outlast the flood — same pattern as the other latency-
        // sensitive tests in this module, which override
        // `config.call_timeout`. A real wedge still fails here: it
        // outruns the 20s flood deadline.
        let mut config = test_config();
        config.call_timeout = Duration::from_secs(10);
        let handle = spawn_published(catalog, config, &["mcp:flood", "mcp:ok"]);

        submit(
            &handle,
            "tf",
            "mcp:flood",
            call("cf", "echo", r#"{"text":"flooded"}"#),
        );
        submit(
            &handle,
            "tc",
            "mcp:ok",
            call("cc", "echo", r#"{"text":"clean"}"#),
        );

        // Clean-origin PROGRESS deadline: its terminal within 3s
        // regardless of the flood.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut clean_done = false;
        let mut flood_done = false;
        while Instant::now() < deadline && !clean_done {
            if let Some(McpEvent::CallFinished {
                ticket,
                disposition,
                result,
                ..
            }) = handle.try_recv_event()
            {
                match ticket.as_str() {
                    "tc" => {
                        assert_eq!(disposition, CallDisposition::Completed);
                        assert_eq!(result.content, "clean");
                        clean_done = true;
                    }
                    "tf" => {
                        assert_eq!(disposition, CallDisposition::Completed);
                        flood_done = true;
                    }
                    other => panic!("unexpected ticket {other}"),
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(clean_done, "clean origin starved by the flood");

        // The flood call itself also completes (bounded handling).
        let deadline = Instant::now() + Duration::from_secs(20);
        while !flood_done {
            assert!(Instant::now() < deadline, "flood call never completed");
            if let Some(McpEvent::CallFinished {
                ticket,
                disposition,
                ..
            }) = handle.try_recv_event()
            {
                assert_eq!(ticket, "tf");
                assert_eq!(disposition, CallDisposition::Completed);
                flood_done = true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // Runtime survives a SECOND flood.
        submit(
            &handle,
            "tf2",
            "mcp:flood",
            call("cf2", "echo", r#"{"text":"again"}"#),
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                Instant::now() < deadline,
                "second flood call never completed"
            );
            if let Some(McpEvent::CallFinished { ticket, result, .. }) = handle.try_recv_event() {
                assert_eq!(ticket, "tf2");
                assert_eq!(result.content, "again");
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = handle.shutdown_and_drain();
    }

    // ----- Unit 4b commit 3b: the exact-name gate rows for the runtime
    // machine (design r9 §6.3; addresses from the r4 row table). -----

    /// Gate row 22.
    #[test]
    fn every_catalog_origin_is_explicitly_disconnected_before_any_connect() {
        let catalog = McpServerCatalog::new()
            .with_stdio(awili_tools::ToolOrigin::new("mcp:a"), spec(&[]))
            .with_stdio(awili_tools::ToolOrigin::new("mcp:b"), spec(&[]));
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        // RAW drain (next_event skips lifecycle markers): the FIRST facts
        // on the stream are one OriginDisconnected per catalog origin,
        // before anything has connected.
        let mut disconnected = std::collections::HashSet::new();
        let deadline = Instant::now() + EVENT_WAIT;
        while disconnected.len() < 2 {
            assert!(Instant::now() < deadline, "got {disconnected:?}");
            match handle.try_recv_event() {
                Some(McpEvent::OriginDisconnected { origin }) => {
                    disconnected.insert(origin.origin);
                }
                Some(other) => panic!("expected only OriginDisconnected first, got {other:?}"),
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert!(disconnected.contains("mcp:a") && disconnected.contains("mcp:b"));
        let _ = handle.shutdown_and_drain();
    }

    /// Gate row 1.
    #[test]
    fn connect_origin_reaches_awaiting_acceptance_without_any_call() {
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        // ConnectOrigin ALONE produces the full connect: OriginConnecting,
        // then ServerConnected — and nothing is dispatched.
        let (_g, connected) = connect_only(&handle, "mcp:test");
        assert!(!connected.tools().is_empty(), "advertisement captured");
        // No terminal of any kind follows: nothing was called.
        let quiet_until = Instant::now() + Duration::from_millis(300);
        while Instant::now() < quiet_until {
            match handle.try_recv_event() {
                None
                | Some(McpEvent::OriginDisconnected { .. })
                | Some(McpEvent::OriginConnecting { .. }) => {}
                Some(other) => panic!("no call was made, yet {other:?}"),
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = handle.shutdown_and_drain();
    }

    /// Gate row 2.
    #[test]
    fn call_is_refused_while_awaiting_acceptance() {
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let (_g, _c) = connect_only(&handle, "mcp:test");
        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", r#"{"text":"x"}"#),
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            ticket,
            disposition,
            result,
            ..
        } = finished
        else {
            panic!("expected refusal, got {finished:?}");
        };
        assert_eq!(ticket, "t1");
        assert_eq!(disposition, CallDisposition::NeverDispatched);
        assert!(
            result.content.contains("awaiting root acceptance"),
            "{}",
            result.content
        );
        let _ = handle.shutdown_and_drain();
    }

    /// Gate row 3.
    #[test]
    fn call_is_admitted_only_after_generation_matched_acceptance() {
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let (g, _c) = connect_only(&handle, "mcp:test");

        // Acceptance bound to a FOREIGN generation, at a HIGHER epoch than
        // the eventual real acceptance (impl review r1 P1-2): it must be
        // refused, confer nothing, AND leave the floor untouched — if it
        // moved the floor to 9, the real epoch-1 acceptance below would
        // wedge as stale.
        let foreign = {
            let mut a = awili_engine::generation::GenerationAllocator::new();
            let mut f = a.next();
            while f == g {
                f = a.next();
            }
            f
        };
        handle
            .submit(McpCommand::UpdateAcceptedRoots {
                origin: ToolOrigin::new("mcp:test"),
                generation: foreign,
                epoch: 9,
            })
            .unwrap();
        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", r#"{"text":"x"}"#),
        );
        let finished = next_event(&handle, EVENT_WAIT);
        let McpEvent::CallFinished {
            disposition,
            result,
            ..
        } = finished
        else {
            panic!("expected refusal, got {finished:?}");
        };
        assert_eq!(disposition, CallDisposition::NeverDispatched);
        assert!(
            result.content.contains("awaiting root acceptance"),
            "the mismatched acceptance must not have been applied: {}",
            result.content
        );

        // The GENERATION-MATCHED acceptance admits.
        handle
            .submit(McpCommand::UpdateAcceptedRoots {
                origin: ToolOrigin::new("mcp:test"),
                generation: g,
                epoch: 1,
            })
            .unwrap();
        submit(
            &handle,
            "t2",
            "mcp:test",
            call("c2", "echo", r#"{"text":"admitted"}"#),
        );
        // Left for a human: the loop body always breaks or panics on its first event.
        #[allow(clippy::never_loop)]
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::CallFinished { ticket, result, .. } => {
                    assert_eq!(ticket, "t2");
                    assert_eq!(result.content, "admitted");
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        let _ = handle.shutdown_and_drain();
    }

    /// Gate row 5. The setup record classifies the testserver `direct`
    /// with a requirement its (unsigned-for-that-identity) binary cannot
    /// satisfy: Phase A attests the LIVE pid, fails, kills and reaps the
    /// child, and NO handshake happens — no ServerConnected ever exists
    /// for the generation.
    #[test]
    fn attestation_failure_kills_child_before_any_handshake_byte() {
        use std::io::Write;
        let marker = format!("awili-attfail-{}", std::process::id());
        let dir = crate::test_tmp::fresh_dir(&marker);
        std::fs::create_dir_all(&dir).unwrap();
        let setup = dir.join("attested-servers.toml");
        let bin = testserver_bin();
        let mut f = std::fs::File::create(&setup).unwrap();
        write!(
            f,
            r#"
[[server]]
origin        = "mcp:test"
program       = {program}
launch_kind   = "direct"
requirement   = 'identifier "com.example.never-this-identity"'
protocol_name = "awili-mcp-testserver"
trust         = "prompt"
"#,
            program = toml_string(&bin)
        )
        .unwrap();

        // The handshake-byte proof (commit-6 lever): the testserver
        // appends to this file the moment ANY stdin byte arrives. Its
        // absence after the death is POSITIVE evidence no MCP byte was
        // ever written to the attestation-failed child.
        let handshake_log = dir.join("handshake.log");
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:test"),
            marked_spec(
                &marker,
                &[("MCP_HANDSHAKE_LOG", handshake_log.to_str().unwrap())],
            ),
        );
        let mut config = test_config();
        config.setup_path = Some(setup);
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        handle
            .submit(McpCommand::ConnectOrigin {
                origin: ToolOrigin::new("mcp:test"),
            })
            .unwrap();
        let ev = next_event(&handle, EVENT_WAIT);
        let McpEvent::ServerDied {
            origin,
            cause,
            selected_class,
            ..
        } = ev
        else {
            panic!("expected AttestationFailed death, got {ev:?}");
        };
        assert_eq!(origin.origin, "mcp:test");
        assert!(
            matches!(cause, DeathCause::AttestationFailed { ref pinned_requirement }
                if pinned_requirement.contains("com.example.never")),
            "{cause:?}"
        );
        assert_eq!(selected_class, None, "no token was selected");
        let _ = handle.shutdown_and_drain();
        // Killed AND reaped: no child carrying the marker survives.
        assert_no_children_matching(&marker);
        assert!(
            !handshake_log.exists(),
            "the kill preceded EVERY handshake byte — the log lever never fired"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Gate row 26. With NO setup record the launch selects an UNVERIFIED
    /// token — and Phase B (initialize + tools/list) is one common path:
    /// the observation still carries the claimed name and the full
    /// advertisement.
    #[test]
    fn an_unverified_token_takes_the_same_initialize_and_list_path() {
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let (_g, connected) = connect_only(&handle, "mcp:test");
        assert_eq!(
            connected.attestation_class(),
            awili_tools::attest::AttestationClass::Unverified
        );
        assert!(!connected.claimed_server().is_empty(), "initialize ran");
        assert!(
            connected.tools().iter().any(|t| t.name == "echo"),
            "tools/list ran"
        );
        let _ = handle.shutdown_and_drain();
    }

    /// Gate row 9. The refusal of a call submitted AFTER observing
    /// ServerDied proves the runtime's Ready authority was dropped no
    /// later than the death became observable — revoke-before-emit.
    #[test]
    fn death_drops_runtime_ready_authority_before_server_died_is_observable() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:crash"),
            spec(&[("MCP_FAULT", "crash_on_call")]),
        );
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let _ = bring_up(&handle, "mcp:crash", 1);
        submit(
            &handle,
            "t1",
            "mcp:crash",
            call("c1", "echo", r#"{"text":"boom"}"#),
        );
        // Drain until the death is OBSERVABLE.
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerDied { origin, .. } => {
                    assert_eq!(origin.origin, "mcp:crash");
                    break;
                }
                McpEvent::CallFinished { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        // A call submitted now MUST refuse: authority was revoked before
        // the emission we just observed.
        submit(
            &handle,
            "t2",
            "mcp:crash",
            call("c2", "echo", r#"{"text":"late"}"#),
        );
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    result,
                    ..
                } if ticket == "t2" => {
                    assert_eq!(disposition, CallDisposition::NeverDispatched);
                    assert!(result.content.contains("not ready"), "{}", result.content);
                    break;
                }
                McpEvent::CallFinished { .. } | McpEvent::ServerDied { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        let _ = handle.shutdown_and_drain();
    }

    /// Gate row 10.
    #[test]
    fn reconnect_with_identical_name_epoch_and_tools_is_a_new_generation() {
        // The launch-counter lever (commit 6): one line per PROCESS
        // launch, appended before any I/O — the reconnect below must show
        // exactly two.
        let counter_dir = crate::test_tmp::fresh_dir("relaunch");
        std::fs::create_dir_all(&counter_dir).unwrap();
        let counter = counter_dir.join("launches.log");
        let _ = std::fs::remove_file(&counter);
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:crash"),
            spec(&[
                ("MCP_FAULT", "crash_on_call"),
                ("MCP_LAUNCH_COUNTER", counter.to_str().unwrap()),
            ]),
        );
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let (g1, c1) = bring_up(&handle, "mcp:crash", 1);
        submit(
            &handle,
            "t1",
            "mcp:crash",
            call("c1", "echo", r#"{"text":"boom"}"#),
        );
        // Consume the death (and the terminal, in either order).
        let mut died = false;
        let mut terminal = false;
        while !(died && terminal) {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerDied { .. } => died = true,
                McpEvent::CallFinished { .. } => terminal = true,
                other => panic!("unexpected {other:?}"),
            }
        }
        // Identical server, identical advertisement — and a NEW generation.
        let (g2, c2) = bring_up(&handle, "mcp:crash", 1);
        assert_ne!(g1, g2);
        assert!(g1 < g2, "generations are monotonic");
        assert_eq!(c1.claimed_server(), c2.claimed_server());
        assert_eq!(c1.tools().len(), c2.tools().len());

        // Impl review r1 P1-1: a binding RETAINED from the retired
        // generation — same name, same epoch, same tools — must refuse
        // Definite rather than silently surviving into the fresh child.
        submit_epoch_with_generation(
            &handle,
            "t-retained",
            "mcp:crash",
            call("c-ret", "echo", r#"{"text":"stale-binding"}"#),
            g1,
            1,
        );
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    result,
                    ..
                } if ticket == "t-retained" => {
                    assert_eq!(disposition, CallDisposition::NeverDispatched);
                    assert!(
                        result.content.contains("retired generation"),
                        "{}",
                        result.content
                    );
                    break;
                }
                McpEvent::CallFinished { .. } | McpEvent::ServerDied { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        let _ = handle.shutdown_and_drain();
        let launches = std::fs::read_to_string(&counter).unwrap_or_default();
        assert_eq!(
            launches.lines().count(),
            2,
            "a fresh CHILD per generation — the counter lever proves two launches: {launches:?}"
        );
        let _ = std::fs::remove_dir_all(&counter_dir);
    }

    /// Gate row 11. The accepted-epoch floor SURVIVES the generation
    /// change: a reconnect cannot launder a lower epoch through a fresh
    /// generation — the stale acceptance is refused LOUDLY.
    #[test]
    fn reconnect_advertising_a_lower_epoch_is_refused_loudly() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:crash"),
            spec(&[("MCP_FAULT", "crash_on_call"), ("MCP_ROOTS_EPOCH", "5")]),
        );
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let (_g1, c1) = bring_up(&handle, "mcp:crash", 5);
        assert_eq!(c1.advertised_epoch(), Some(5));
        submit_epoch(
            &handle,
            "t1",
            "mcp:crash",
            call("c1", "echo", r#"{"text":"boom"}"#),
            5,
        );
        let mut died = false;
        let mut terminal = false;
        while !(died && terminal) {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerDied { .. } => died = true,
                McpEvent::CallFinished { .. } => terminal = true,
                other => panic!("unexpected {other:?}"),
            }
        }
        // Reconnect; try to accept a LOWER epoch for the new generation.
        let (g2, _c2) = connect_only(&handle, "mcp:crash");
        handle
            .submit(McpCommand::UpdateAcceptedRoots {
                origin: ToolOrigin::new("mcp:crash"),
                generation: g2,
                epoch: 3,
            })
            .unwrap();
        let ev = next_event(&handle, EVENT_WAIT);
        let McpEvent::RootsUpdateIgnored {
            origin,
            stale_epoch,
            current_epoch,
        } = ev
        else {
            panic!("expected the loud refusal, got {ev:?}");
        };
        assert_eq!(origin.origin, "mcp:crash");
        assert_eq!((stale_epoch, current_epoch), (3, 5));
        let _ = handle.shutdown_and_drain();
    }

    /// Gate row 15. Every failure path reaps: handshake failure and
    /// mid-call crash each leave no child (the attestation-failure reap is
    /// row 5's own assertion).
    #[test]
    fn child_is_reaped_on_every_failure_path() {
        // (a) handshake failure: initialize hangs past the connect deadline.
        let marker = format!("awili-reap-{}", std::process::id());
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:slowinit"),
            marked_spec(
                &marker,
                &[("MCP_FAULT", "slow_initialize"), ("MCP_DELAY_MS", "8000")],
            ),
        );
        let mut config = test_config();
        config.connect_timeout = Duration::from_millis(400);
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        handle
            .submit(McpCommand::ConnectOrigin {
                origin: ToolOrigin::new("mcp:slowinit"),
            })
            .unwrap();
        let ev = next_event(&handle, EVENT_WAIT);
        let McpEvent::ServerDied { cause, .. } = ev else {
            panic!("expected HandshakeFailed death, got {ev:?}");
        };
        assert_eq!(cause, DeathCause::HandshakeFailed);
        let _ = handle.shutdown_and_drain();
        assert_no_children_matching(&marker);

        // (b) mid-call crash: the runtime reaps the crashed child.
        let marker2 = format!("awili-reap2-{}", std::process::id());
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:crash"),
            marked_spec(&marker2, &[("MCP_FAULT", "crash_on_call")]),
        );
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let _ = bring_up(&handle, "mcp:crash", 1);
        submit(
            &handle,
            "t1",
            "mcp:crash",
            call("c1", "echo", r#"{"text":"boom"}"#),
        );
        let mut died = false;
        let mut terminal = false;
        while !(died && terminal) {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerDied { .. } => died = true,
                McpEvent::CallFinished { .. } => terminal = true,
                other => panic!("unexpected {other:?}"),
            }
        }
        let _ = handle.shutdown_and_drain();
        assert_no_children_matching(&marker2);
    }

    /// RC2-APP (F0202): a connect task that dies without its outcome leaves
    /// its origin `Dead` (so the next `ConnectOrigin` starts a fresh
    /// attempt), never `Connecting` forever; a newer attempt or a state the
    /// loop already moved on is left alone.
    #[test]
    fn rc2app_a_lost_connect_attempt_settles_its_origin() {
        let alloc = awili_engine::generation::process();
        let (g1, g2) = (alloc.next(), alloc.next());
        let mut states = HashMap::new();
        states.insert(
            "mcp:a".to_string(),
            RuntimeOriginState::Connecting { generation: g1 },
        );
        assert!(settle_lost_connect(&mut states, "mcp:a", g1));
        assert_eq!(states["mcp:a"], RuntimeOriginState::Dead { generation: g1 });
        assert!(!settle_lost_connect(&mut states, "mcp:a", g1), "once");
        states.insert(
            "mcp:a".to_string(),
            RuntimeOriginState::Connecting { generation: g2 },
        );
        assert!(!settle_lost_connect(&mut states, "mcp:a", g1), "newer");
        assert!(!settle_lost_connect(&mut states, "mcp:missing", g1));
    }

    /// RC2-APP (F0204): shutting the runtime down while a server is still
    /// connecting (its initialize is slow) stops that child too — no
    /// connect attempt outlives the runtime.
    #[test]
    fn rc2app_shutdown_stops_a_connect_in_flight() {
        let marker = format!("awili-rc2app-connect-{}", std::process::id());
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:slowinit"),
            marked_spec(
                &marker,
                &[("MCP_FAULT", "slow_initialize"), ("MCP_DELAY_MS", "20000")],
            ),
        );
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        handle
            .submit(McpCommand::ConnectOrigin {
                origin: ToolOrigin::new("mcp:slowinit"),
            })
            .unwrap();
        let running = |m: &str| {
            !std::process::Command::new("pgrep")
                .args(["-f", m])
                .output()
                .expect("pgrep")
                .stdout
                .is_empty()
        };
        let deadline = Instant::now() + EVENT_WAIT;
        while !running(&marker) {
            assert!(Instant::now() < deadline, "the server never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = handle.shutdown_and_drain();
        let deadline = Instant::now() + Duration::from_secs(5);
        while running(&marker) {
            assert!(
                Instant::now() < deadline,
                "a connecting server outlived the runtime"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// RC2-APP (K-flake1, S-N1): the test runtimes wait long enough for a
    /// connect (attestation included) under a parallel build, and a test's
    /// event wait outlasts the connect deadline.
    #[test]
    fn rc2app_test_deadlines_outlast_a_loaded_connect() {
        assert!(test_config().connect_timeout >= Duration::from_secs(30));
        assert!(EVENT_WAIT > test_config().connect_timeout);
    }

    /// Gate row 37. Token selection SUCCEEDS (unverified class), then
    /// initialize fails: the death carries Some(class) — the Phase-B
    /// boundary transports the selected class (design r9 §2.4).
    #[test]
    fn handshake_failure_after_token_selection_reports_the_selected_class() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:slowinit"),
            spec(&[("MCP_FAULT", "slow_initialize"), ("MCP_DELAY_MS", "8000")]),
        );
        let mut config = test_config();
        config.connect_timeout = Duration::from_millis(400);
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        handle
            .submit(McpCommand::ConnectOrigin {
                origin: ToolOrigin::new("mcp:slowinit"),
            })
            .unwrap();
        let ev = next_event(&handle, EVENT_WAIT);
        let McpEvent::ServerDied {
            cause,
            selected_class,
            ..
        } = ev
        else {
            panic!("expected HandshakeFailed death, got {ev:?}");
        };
        assert_eq!(cause, DeathCause::HandshakeFailed);
        assert_eq!(
            selected_class,
            Some(awili_tools::attest::AttestationClass::Unverified),
            "every Phase-B failure carries the selected class"
        );
        let _ = handle.shutdown_and_drain();
    }

    /// Gate row 46. A death AFTER Ready still reports the class the
    /// runtime retained when the token moved into the observation.
    #[test]
    fn a_post_ready_child_death_reports_the_selected_class() {
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:crash"),
            spec(&[("MCP_FAULT", "crash_on_call")]),
        );
        let handle = spawn_mcp_runtime::<String>(catalog, test_config());
        let _ = bring_up(&handle, "mcp:crash", 1);
        submit(
            &handle,
            "t1",
            "mcp:crash",
            call("c1", "echo", r#"{"text":"boom"}"#),
        );
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerDied {
                    cause,
                    selected_class,
                    ..
                } => {
                    assert_eq!(cause, DeathCause::TransportFailed);
                    assert_eq!(
                        selected_class,
                        Some(awili_tools::attest::AttestationClass::Unverified),
                        "the slot-retained Copy class reaches the post-Ready death"
                    );
                    break;
                }
                McpEvent::CallFinished { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
        let _ = handle.shutdown_and_drain();
    }

    /// TOML string escape for a filesystem path in a test fixture.
    fn toml_string(p: &std::path::Path) -> String {
        format!("{:?}", p.to_string_lossy())
    }

    /// Gate row 12 (unit 4b): the identity bound into the evidence at turn
    /// open must equal the registry's CURRENT resolution at dispatch — a
    /// descriptor replaced in between (a re-advertisement re-projecting
    /// the same name with a different schema) is a HARD, DEFINITE refusal
    /// before any byte is sent, never a silent switch of implementation.
    #[test]
    fn descriptor_replacement_between_turn_open_and_dispatch_is_refused_definite() {
        use awili_tools::{CanonicalDescriptor, ToolDef, ToolRegistry};

        let desc = |schema: serde_json::Value| CanonicalDescriptor {
            schema,
            side_effect: "read_only".into(),
            required_capabilities: vec!["mcp".into()],
            executor: "mcp:test".into(),
        };
        // Turn open: v1 registered; the evidence binds v1's identity.
        let v1 = ToolDef::from_parts(
            awili_tools::ToolOrigin::new("mcp:test"),
            "probe",
            desc(serde_json::json!({"type": "object", "properties": {}})),
        );
        let bound = v1.identity.clone();
        // Between turn open and dispatch: the origin re-advertises and the
        // per-turn registry now resolves a REPLACED descriptor (different
        // schema, therefore a different digest, therefore a different
        // identity).
        let v2 = ToolDef::from_parts(
            awili_tools::ToolOrigin::new("mcp:test"),
            "probe",
            desc(serde_json::json!({"type": "object", "properties": {"x": {"type": "string"}}})),
        );
        assert_ne!(
            v1.identity, v2.identity,
            "the replacement changed the digest"
        );
        let mut registry = ToolRegistry::with_validator(awili_tools::AllowAllValidator);
        registry.register(v2).expect("replacement registers");

        let err = awili_seam::verify_bound_identity(&registry, "probe", &bound)
            .expect_err("a replaced descriptor must refuse");
        assert!(
            matches!(err, awili_seam::IdentityCheckError::Mismatch { .. }),
            "refused Definite before dispatch: {err:?}"
        );
    }

    /// Gate row 8. A REAL verified launch: the setup record pins the
    /// testserver's own CDHash (read from its ad-hoc signature), Phase A
    /// attests the live child successfully, the catalog is accepted, and
    /// a dispatched call finalizes DEFINITE — the full happy path under a
    /// Verified class.
    #[test]
    #[cfg(target_os = "macos")] // reads the testserver's Mach-O CDHash via `codesign`
    fn accepted_work_catalog_dispatches_under_a_verified_launch_and_finalizes_definite() {
        use std::io::Write;
        // The binary's own CDHash, from its (ad-hoc) code signature,
        // read with Apple's `codesign` tool — a macOS-only gate. On
        // hosts without the tool (e.g. Linux) the gate cannot be
        // exercised, so skip rather than fail.
        let bin = testserver_bin();
        let Ok(out) = std::process::Command::new("codesign")
            .args(["-dvvv", bin.to_str().unwrap()])
            .output()
        else {
            eprintln!("SKIP: `codesign` (macOS) unavailable on this host");
            return;
        };
        let text = String::from_utf8_lossy(&out.stderr);
        let Some(cdhash) = text
            .lines()
            .find_map(|l| l.strip_prefix("CDHash="))
            .map(|h| h.trim().to_string())
        else {
            panic!("no CDHash in codesign output: {text}");
        };

        let marker = format!("awili-verified-{}", std::process::id());
        let dir = crate::test_tmp::fresh_dir(&marker);
        std::fs::create_dir_all(&dir).unwrap();
        let setup = dir.join("attested-servers.toml");
        let mut f = std::fs::File::create(&setup).unwrap();
        write!(
            f,
            r#"
[[server]]
origin        = "mcp:test"
program       = {program}
launch_kind   = "direct"
requirement   = 'cdhash H"{cdhash}"'
protocol_name = "awili-mcp-testserver"
trust         = "prompt"
"#,
            program = toml_string(&bin)
        )
        .unwrap();

        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let mut config = test_config();
        config.setup_path = Some(setup);
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        let (_g, connected) = bring_up(&handle, "mcp:test", 1);
        assert_eq!(
            connected.attestation_class(),
            awili_tools::attest::AttestationClass::Verified,
            "the live pid satisfied its own pinned CDHash"
        );
        submit(
            &handle,
            "t1",
            "mcp:test",
            call("c1", "echo", r#"{"text":"attested"}"#),
        );
        // Left for a human: the loop body always breaks or panics on its first event.
        #[allow(clippy::never_loop)]
        loop {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::CallFinished {
                    ticket,
                    disposition,
                    result,
                    ..
                } => {
                    assert_eq!(ticket, "t1");
                    assert_eq!(disposition, CallDisposition::Completed);
                    assert_eq!(disposition.certainty(), ExecutionCertainty::Definite);
                    assert!(!result.is_error);
                    assert_eq!(result.content, "attested");
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        let _ = handle.shutdown_and_drain();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Impl review r1 P1-3: an INVALID installer record leaves the origin
    /// UNAVAILABLE — fail closed, no spawn, never a usable unverified
    /// launch.
    #[test]
    fn an_invalid_setup_record_leaves_the_origin_unavailable() {
        use std::io::Write;
        let dir = crate::test_tmp::fresh_dir("invalid-rec");
        std::fs::create_dir_all(&dir).unwrap();
        let setup = dir.join("attested-servers.toml");
        let mut f = std::fs::File::create(&setup).unwrap();
        // trust = "install" off the reserved origin: a HARD error.
        write!(
            f,
            r#"
[[server]]
origin        = "mcp:test"
program       = {program}
launch_kind   = "direct"
requirement   = 'cdhash H"00"'
protocol_name = "awili-mcp-testserver"
trust         = "install"
"#,
            program = toml_string(&testserver_bin())
        )
        .unwrap();
        let catalog =
            McpServerCatalog::new().with_stdio(awili_tools::ToolOrigin::new("mcp:test"), spec(&[]));
        let mut config = test_config();
        config.setup_path = Some(setup);
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        handle
            .submit(McpCommand::ConnectOrigin {
                origin: ToolOrigin::new("mcp:test"),
            })
            .unwrap();
        let ev = next_event(&handle, EVENT_WAIT);
        let McpEvent::ServerDied {
            cause,
            selected_class,
            detail,
            ..
        } = ev
        else {
            panic!("an invalid record must not connect, got {ev:?}");
        };
        assert_eq!(cause, DeathCause::LaunchFailed);
        assert_eq!(selected_class, None);
        assert!(detail.contains("invalid setup record"), "{detail}");
        let _ = handle.shutdown_and_drain();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Impl review r1 P1-4 (ruling H): the record CURRENT AT SPAWN governs
    /// each generation — a record installed while the worker lives
    /// upgrades the very next generation from Unverified to Verified.
    #[test]
    #[cfg(target_os = "macos")] // reads the testserver's Mach-O CDHash via `codesign`
    fn a_replaced_record_governs_the_next_generation() {
        use std::io::Write;
        let bin = testserver_bin();
        // Same macOS-only `codesign` gate as above: skip where the
        // tool is absent, run verbatim where it is present.
        let Ok(out) = std::process::Command::new("codesign")
            .args(["-dvvv", bin.to_str().unwrap()])
            .output()
        else {
            eprintln!("SKIP: `codesign` (macOS) unavailable on this host");
            return;
        };
        let text = String::from_utf8_lossy(&out.stderr);
        let cdhash = text
            .lines()
            .find_map(|l| l.strip_prefix("CDHash="))
            .map(|h| h.trim().to_string())
            .expect("CDHash");

        let dir = crate::test_tmp::fresh_dir("reload-rec");
        std::fs::create_dir_all(&dir).unwrap();
        let setup = dir.join("attested-servers.toml");
        // Generation 1: NO record on disk → Unverified.
        let catalog = McpServerCatalog::new().with_stdio(
            awili_tools::ToolOrigin::new("mcp:crash"),
            spec(&[("MCP_FAULT", "crash_on_call")]),
        );
        let mut config = test_config();
        config.setup_path = Some(setup.clone());
        let handle = spawn_mcp_runtime::<String>(catalog, config);
        let (_g1, c1) = bring_up(&handle, "mcp:crash", 1);
        assert_eq!(
            c1.attestation_class(),
            awili_tools::attest::AttestationClass::Unverified
        );
        // Kill g1.
        submit(
            &handle,
            "t1",
            "mcp:crash",
            call("c1", "echo", r#"{"text":"boom"}"#),
        );
        let mut died = false;
        let mut terminal = false;
        while !(died && terminal) {
            match next_event(&handle, EVENT_WAIT) {
                McpEvent::ServerDied { .. } => died = true,
                McpEvent::CallFinished { .. } => terminal = true,
                other => panic!("unexpected {other:?}"),
            }
        }
        // INSTALL the record while the worker lives.
        let mut f = std::fs::File::create(&setup).unwrap();
        write!(
            f,
            r#"
[[server]]
origin        = "mcp:crash"
program       = {program}
launch_kind   = "direct"
requirement   = 'cdhash H"{cdhash}"'
protocol_name = "awili-mcp-testserver"
trust         = "prompt"
"#,
            program = toml_string(&bin)
        )
        .unwrap();
        // Generation 2 attests against the record CURRENT AT ITS SPAWN.
        let (_g2, c2) = bring_up(&handle, "mcp:crash", 1);
        assert_eq!(
            c2.attestation_class(),
            awili_tools::attest::AttestationClass::Verified,
            "the freshly installed record governs the new generation"
        );
        let _ = handle.shutdown_and_drain();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
