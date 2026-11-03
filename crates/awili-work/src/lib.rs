//! awili-work — the engine-neutral Work seam (ENGINE-PIVOT-PLAN r3.1, D1).
//!
//! `WorkCmd` in, `WorkEvent` out, an [`EngineWorker`] on its own dedicated
//! thread. This crate is the boundary an engine implements to sit behind the
//! Work surface: the Hermes gateway adapter (in `awili-app::work`) is the first
//! implementation; D2 adds `awili-engine` as the second.
//!
//! Design rules (Codex D1 consensus, 2026-08-13 — binding):
//! - Events are FACTS the engine observed, never UI prose. Rendering,
//!   transcript strings, thresholds, and stream-row policy live app-side in
//!   the `WorkEvent → WorkUpdate` adapter.
//! - No Hermes wire types cross this boundary: no `GatewayEvent`, no
//!   `GatewayEventKind`, no WebSocket vocabulary. The neutral
//!   [`SubagentUpdate`] mirror exists precisely so the raw gateway enum stays
//!   behind the Hermes adapter.
//! - No tokio, no gpui, no HTTP. Dependencies are serde + async-channel only.
//! - `WorkCmd` deliberately derives no `Debug`: `SudoRespond`/`SecretRespond`
//!   carry secret values that must never be loggable.
//!
//! The `run_events` module (RunStatus / RunEvent vocabulary) moved here
//! verbatim from `awili-gateway`, which re-exports it for compatibility.
//! `RunStatus` is seam vocabulary ([`WorkEvent::Terminal`],
//! [`WorkEvent::TurnCompleted`]); `RunEvent` remains app-side compatibility
//! vocabulary — engines must not synthesize it.

pub mod approval_mode;
pub use approval_mode::{ApprovalMode, SharedApprovalMode};
pub mod run_events;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use run_events::RunStatus;

// ============================================================================
// Commands — UI → engine
// ============================================================================

/// Commands sent from the UI thread to the persistent engine worker.
///
/// Deliberately NO `Debug` derive: `SudoRespond` and `SecretRespond` carry
/// secret values, so the whole enum is kept unloggable (same discipline as the
/// `GatewayCmd` it replaces).
pub enum WorkCmd {
    /// Submit a new prompt to the active session. `system` is the CURRENT
    /// pinned system context for this turn (standing directives + recall,
    /// ship-fix P0-3) — engines that own their memory integration (the
    /// Hermes gateway) ignore it; the owned engine pins it FIRST in the
    /// request context, replacing the previous turn's preamble.
    Prompt {
        text: String,
        system: Option<String>,
    },
    /// RC2-R617: have the model server read, ahead of the owner's first
    /// question, what this session's requests begin with (`system` as a
    /// prompt would pin it, the kept history, the tools), so that question
    /// is answered from the server's cache. Nothing is shown and nothing
    /// runs; an engine with no such cache ignores it.
    Warm { system: Option<String> },
    /// Respond to an approval gate.
    ///
    /// `id` is the [`WorkEvent::ApprovalRequested::approval_id`] this verdict
    /// answers, echoed back so the engine can prove the answer belongs to the
    /// call it is blocked on. `None` means the surface had no id to echo.
    /// Engines that raise correlated gates MUST drop a verdict whose id does
    /// not match the pending call: without that check a stale verdict from an
    /// earlier dialog silently resolves whatever gate happens to be open now
    /// (D2-u2a).
    ///
    /// `choice` is the EXACT scope string the surface offered (P3a):
    /// `"once" | "session" | "folder" | "always" | "deny"` (see
    /// [`run_events::ApprovalDecision::scope_str`]). An engine that cannot
    /// honor a scope must fail closed (refuse it as a denial with a typed
    /// [`WorkEvent::HookDenial`]) — it must never silently accept it as a
    /// narrower one.
    Approval { id: Option<String>, choice: String },
    /// Respond to a clarification request.
    Clarify { request_id: String, answer: String },
    /// Steer the agent mid-turn with additional guidance.
    Steer(String),
    /// Attach a file to the current session.
    Attach { path: String },
    /// Respond to a terminal.read request with screen text.
    TerminalReadRespond { request_id: String, text: String },
    /// Respond to a sudo request. An empty password is the safe refusal.
    SudoRespond {
        request_id: String,
        password: String,
    },
    /// Respond to a secret/credential request. Empty value = safe refusal.
    SecretRespond { request_id: String, value: String },
    /// Cancel the current turn and close the session.
    Cancel,
    /// Shut down the worker thread entirely (app exit or folder change).
    Shutdown,
}

// ============================================================================
// Subagent facts — the neutral mirror
// ============================================================================

/// Lifecycle phase of a subagent update. Neutral mirror of the gateway's
/// `subagent.*` suffix vocabulary; `Other` is the forward-compat safety net.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentPhase {
    SpawnRequested,
    Start,
    Thinking,
    Tool,
    Progress,
    Complete,
    /// A subagent event whose phase this build does not model.
    Other,
}

/// One subagent progress fact. A lossless, flat mirror of the fields the
/// engine observed — the app-side fold (store, tree nesting, dedup) consumes
/// these; the engine just reports them.
#[derive(Clone, Debug, PartialEq)]
pub struct SubagentUpdate {
    pub phase: SubagentPhase,
    pub id: Option<String>,
    pub parent_id: Option<String>,
    pub goal: Option<String>,
    pub model: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub text: Option<String>,
    pub summary: Option<String>,
    /// Dollar cost as the raw string form (a number is stringified upstream)
    /// so the consumer parses it.
    pub cost_usd: Option<String>,
    /// The child agent's own session, when it has one.
    pub session_id: Option<String>,
    /// Delegation batch size and this agent's slot in it.
    pub task_count: Option<u64>,
    pub task_index: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Running count of tool calls this agent has made.
    pub tool_count: Option<u64>,
    /// Paths the agent has touched (may be empty).
    pub files_read: Vec<String>,
    pub files_written: Vec<String>,
    /// The tool the agent is running right now, if any.
    pub current_tool: Option<String>,
}

// ============================================================================
// Events — engine → UI
// ============================================================================

/// A fact the engine observed, sent from the worker thread to the UI thread.
///
/// Facts only: no transcript prose, no thresholds, no Markdown. The app-side
/// adapter turns these into whatever the surface renders. Variants that carry
/// normalized values say so — normalization of *engine-specific quirks*
/// (status spellings, percent units) happens engine-side because that
/// knowledge belongs to the engine; *policy* (noise floors, warning
/// thresholds) stays app-side.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkEvent {
    /// Session created / run started; carries the session or run ID.
    Started(String),
    /// The assistant turn began. `model` is the resolved model name when the
    /// engine reports it.
    TurnStarted { model: Option<String> },
    /// A chunk of streamed assistant text. May be empty; the adapter filters.
    Token(String),
    /// The turn finished. `status` is normalized engine-side (the engine owns
    /// its own status spellings); `context_percent` is normalized to 0-100.
    TurnCompleted {
        status: RunStatus,
        output: Option<String>,
        cost_usd: Option<f64>,
        context_percent: Option<f64>,
    },
    /// A tool began running. `detail` is the engine's own live description of
    /// the call, when it sends one. `file_path` is the path the tool is
    /// operating on, when the engine reports one.
    ToolStarted {
        name: Option<String>,
        detail: Option<String>,
        file_path: Option<String>,
    },
    /// A tool finished. `detail` is the engine's own completion summary.
    /// `ok` is the factual outcome when the engine knows it (D2-u1 P0-3):
    /// `Some(false)` = the tool reported an error; `None` = the engine does
    /// not report outcomes (the Hermes gateway path today) and the surface
    /// renders completion as before.
    ToolFinished {
        name: Option<String>,
        detail: Option<String>,
        duration_s: Option<f64>,
        ok: Option<bool>,
    },
    /// RC1-O (round 5 K3): a tool that is not one of the engine's own file
    /// tools, and is not read-only by its registry risk (an owner MCP
    /// server's `write_file`, `run_command`, …), finished successfully. The
    /// engine cannot say which files it touched, so the surface must not
    /// claim "no files changed". Sent just before that call's `ToolFinished`.
    ToolMayHaveChangedFiles { name: String },
    /// RC2-R6M2 (RC2-R6 review P2 on 0fc8434b): a [`WorkCmd::Warm`] ended;
    /// `ok` when the model server read the warm-up through (it was sent and
    /// not stopped), so the app counts that server warm only then. Nothing
    /// is shown.
    Warmed { ok: bool },
    /// The engine is blocking on a command approval. `risk` is the TYPED,
    /// closed risk class of the gated call (S2(a): one of
    /// `"read_only"`/`"mutating"`/`"irreversible"`/`"privileged"`) — the
    /// app surface decides which approval scopes to offer from it (e.g.
    /// `always` is never offered for irreversible/privileged). A no-reported
    /// risk is [`run_events::ApprovalRisk::Privileged`], the conservative
    /// closed-set member (S2(a), correction RVS2A-FIX1: the seam carries the
    /// typed enum, never a string).
    ApprovalRequested {
        approval_id: Option<String>,
        command: Option<String>,
        description: Option<String>,
        allow_permanent: bool,
        risk: run_events::ApprovalRisk,
    },
    /// A TYPED denial observed at the host-approval hook or delivered by the
    /// gateway: the `{source, reason, recovery}` fact (P3a). This is the ONE
    /// denial shape both engine paths emit — it must be preserved verbatim
    /// through the app-side normalization and rendered as
    /// `Blocked by <source>: <reason>` with only the supplied recovery
    /// (never collapsed into a generic `Failed`). `recovery: None` means the
    /// denier supplied no recovery; the surface says so explicitly and never
    /// invents bypass instructions.
    HookDenial {
        /// Who denied — the denier's own name (e.g. `"authorization"`,
        /// `"gateway"`).
        source: String,
        /// Why it was denied — the denier's own reason text, verbatim.
        reason: String,
        /// The permitted recovery the denier supplied, if any.
        recovery: Option<String>,
    },
    /// A gateway approval RPC completed; id is the local delivery correlation.
    ApprovalDelivery { id: Option<String>, delivered: bool },
    /// The engine is blocking on a clarification answer.
    ClarificationRequested {
        request_id: Option<String>,
        question: Option<String>,
    },
    /// A chunk of reasoning text streamed. May be empty; the adapter filters.
    ReasoningDelta { text: String },
    /// The engine finished reasoning and the trace is available.
    ReasoningAvailable,
    /// The engine's live status line ("face + verb" spinner text), when it
    /// sends one.
    ThinkingStatus { text: Option<String> },
    /// A backend status update. `kind` crosses as the engine's own fact
    /// vocabulary (e.g. "compacting"); interpreting it is app policy.
    StatusUpdate {
        kind: Option<String>,
        message: Option<String>,
    },
    /// A turn completed at the stream level (the engine signalled end-of-turn
    /// without a full completion record). The session stays open for
    /// multi-turn.
    TurnComplete,
    /// RC1-F4: THIS turn's token counts, when the engine reports them (the
    /// gateway path: Hermes's `message.complete` usage, turned from session
    /// totals into the turn's own share).
    UsageReported {
        input_tokens: u64,
        output_tokens: u64,
    },
    /// The worker's terminal report — the fail-closed source of truth when the
    /// worker itself ends the run (e.g. cancel).
    Terminal {
        status: RunStatus,
        output: Option<String>,
        model: Option<String>,
    },
    /// The engine's self-improvement review saved something and reported a
    /// summary. Non-empty by construction (engine trims).
    ReviewSummary { text: String },
    /// Submit, streaming, or transport failed.
    Failed(String),
    /// The engine is blocking on terminal screen text.
    TerminalReadRequest { request_id: String },
    /// The engine is blocking on a sudo password. Carries only the request id
    /// — never a secret value — so it is safe to Debug-log.
    SudoRequest { request_id: String },
    /// The engine is blocking on a credential value for an environment
    /// variable. Carries the env-var name and prompt text, never the value.
    SecretRequest {
        request_id: String,
        env_var: String,
        prompt: String,
    },
    /// A subagent progress fact (boxed — the payload is large).
    Subagent(Box<SubagentUpdate>),
}

// ============================================================================
// The worker seam
// ============================================================================

/// Everything a worker needs to run: where to work, how commands arrive, how
/// to report facts, and the cancel flag the UI can trip at any time.
pub struct WorkerContext {
    /// The working directory for the session (the bound folder).
    pub workdir: String,
    /// Command channel from the UI. The worker exits when it closes.
    pub cmd_rx: std::sync::mpsc::Receiver<WorkCmd>,
    /// Tripped by the UI on cancel; the worker must observe it promptly.
    pub cancel: Arc<AtomicBool>,
    /// Fact channel to the UI. The worker exits when the receiver drops.
    pub events: async_channel::Sender<WorkEvent>,
}

/// An engine that can run Work sessions. Implementations own their transport
/// and event decode entirely; only [`WorkCmd`]/[`WorkEvent`] cross this
/// boundary. `run` blocks for the worker's whole life and is called on a
/// dedicated thread by [`spawn_worker`] — implementations never see GPUI, the
/// app state, or each other.
pub trait EngineWorker: Send + 'static {
    fn run(self: Box<Self>, ctx: WorkerContext);
}

/// Spawn a worker on its own OS thread. The thread name aids diagnostics.
pub fn spawn_worker(
    worker: Box<dyn EngineWorker>,
    ctx: WorkerContext,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("awili-work-engine".into())
        .spawn(move || worker.run(ctx))
        .expect("spawn awili-work engine thread")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// The seam is object-safe and the spawn helper drives a worker through
    /// its full life: context in, facts out, clean exit when commands close.
    #[test]
    fn spawn_worker_runs_and_exits() {
        struct Echo;
        impl EngineWorker for Echo {
            fn run(self: Box<Self>, ctx: WorkerContext) {
                let _ = ctx.events.send_blocking(WorkEvent::Started("s1".into()));
                while let Ok(cmd) = ctx.cmd_rx.recv() {
                    match cmd {
                        WorkCmd::Prompt { text, .. } => {
                            let _ = ctx.events.send_blocking(WorkEvent::Token(text));
                            let _ = ctx.events.send_blocking(WorkEvent::TurnComplete);
                        }
                        WorkCmd::Shutdown => return,
                        _ => {}
                    }
                    if ctx.cancel.load(Ordering::SeqCst) {
                        return;
                    }
                }
            }
        }

        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = async_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = spawn_worker(
            Box::new(Echo),
            WorkerContext {
                workdir: "/tmp".into(),
                cmd_rx,
                cancel,
                events: ev_tx,
            },
        );

        cmd_tx
            .send(WorkCmd::Prompt {
                text: "hello".into(),
                system: None,
            })
            .unwrap();
        cmd_tx.send(WorkCmd::Shutdown).unwrap();
        handle.join().unwrap();

        let mut got = Vec::new();
        while let Ok(ev) = ev_rx.try_recv() {
            got.push(ev);
        }
        assert_eq!(
            got,
            vec![
                WorkEvent::Started("s1".into()),
                WorkEvent::Token("hello".into()),
                WorkEvent::TurnComplete,
            ]
        );
    }

    /// The typed denial fact (P3a) is a plain, lossless data bag: the
    /// `{source, reason, recovery}` fields survive a clone verbatim, and an
    /// absent recovery stays `None` all the way to the surface.
    #[test]
    fn hook_denial_fact_is_lossless() {
        let with_recovery = WorkEvent::HookDenial {
            source: "authorization".into(),
            reason: "denied by the user".into(),
            recovery: Some("retry from a read-only scope".into()),
        };
        assert_eq!(with_recovery.clone(), with_recovery);
        let bare = WorkEvent::HookDenial {
            source: "gateway".into(),
            reason: "denied".into(),
            recovery: None,
        };
        assert!(matches!(bare, WorkEvent::HookDenial { recovery: None, .. }));
    }

    /// The subagent mirror is a plain data bag: constructible, cloneable,
    /// comparable — the properties the app-side fold relies on.
    #[test]
    fn subagent_update_is_a_plain_data_bag() {
        let u = SubagentUpdate {
            phase: SubagentPhase::Start,
            id: Some("a1".into()),
            parent_id: None,
            goal: Some("goal".into()),
            model: None,
            name: None,
            status: None,
            text: None,
            summary: None,
            cost_usd: Some("0.01".into()),
            session_id: None,
            task_count: Some(2),
            task_index: Some(1),
            input_tokens: None,
            output_tokens: None,
            tool_count: None,
            files_read: vec!["a.txt".into()],
            files_written: vec![],
            current_tool: None,
        };
        assert_eq!(u.clone(), u);
    }
}
