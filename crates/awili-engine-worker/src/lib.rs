//! awili-engine-worker — `awili-engine` as a [`EngineWorker`] behind the D1
//! seam (ENGINE-PIVOT-PLAN r3.1, D2-u1).
//!
//! This crate owns ONE thing: the bridge between the app's channel
//! vocabulary (`WorkCmd` in, `WorkEvent` out) and the adopted `awili-seam`
//! composition (C1 authorization → B2 durable log → C2 tools → C3 sandbox →
//! C4 context). The turn loop itself lives in `awili-seam`; nothing here
//! reimplements it.
//!
//! Scope (Codex D2 reslice, 2026-08-13): u1 is the headless worker core,
//! **Chat Completions only** — the dialect is explicit in
//! [`AwiliEngineConfig`] and any other value is refused typed. App wiring
//! (config flag, persistence, recall, folder/artifact surface) is u2a;
//! hosted dialects and the model dropdown are u2b. "D2 complete" means the
//! full signed app-level local-plus-hosted gate, not this crate.
//!
//! Not here by design: gpui, tokio, and HTTP details (the transport comes
//! from `awili_seam::HttpTransport`). The `HostAuthority` is still INJECTED
//! (`Arc`) into every worker — a worker never issues its own — but [`authority`]
//! now owns the one process-global issuance on the app's behalf (u2a), so the
//! sole-issue invariant is enforced in one place instead of trusted to each
//! caller.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

mod channel_split;
pub mod floor_owner;
pub mod mcp_catalog;
pub mod mcp_coordinator;
mod warm_gate;

use awili_engine::events::AgentEvent;
use awili_engine::AwiliCallId;
use awili_engine::EngineError;
use awili_engine_authz::{Authorizer, UserVerdict};
use awili_engine_log::authz::issue_bundle;
use awili_engine_log::writer::ExecutionLog;
use awili_engine_log::RiskClass;
use awili_seam::{
    run_turn, ApprovalOutcome, ContextHistory, Executors, HostAuthority, ObserverGone,
    RootsAcceptance, SeamError, StopCause, Transport, TurnHost, DEFAULT_WINDOW_TOKENS,
};
use awili_work::approval_mode::{ApprovalMode, SharedApprovalMode};
use awili_work::run_events::{ApprovalRisk, RunStatus};
use awili_work::{EngineWorker, WorkCmd, WorkEvent, WorkerContext};

/// The only dialect u1 speaks. Explicit so u2b's hosted dialects are a
/// visible addition rather than a silent assumption (Codex P0-5).
pub const CHAT_COMPLETIONS: &str = "chat-completions";

/// How long to wait on one blocking approval poll before re-checking the
/// cancel flag. Cancellation must be observed even when no `WorkCmd`
/// arrives (Codex P0-1).
const APPROVAL_POLL: Duration = Duration::from_millis(100);

/// The closed risk class a gate offers scopes from (S2(a)): the engine's
/// canonical `RiskClass` mapped 1:1 into the seam's typed `ApprovalRisk` —
/// never round-tripped through the wire string (RVS2A-FIX1).
fn approval_risk_of(r: RiskClass) -> ApprovalRisk {
    match r {
        RiskClass::ReadOnly => ApprovalRisk::ReadOnly,
        RiskClass::Mutating => ApprovalRisk::Mutating,
        RiskClass::Irreversible => ApprovalRisk::Irreversible,
        RiskClass::Privileged => ApprovalRisk::Privileged,
    }
}

/// The process-wide issued authority, created on first use and shared by
/// every engine session after it.
///
/// `issue_bundle()` succeeds exactly ONCE per process by construction, and
/// claims all three authority bits — gate, approval capability, registry
/// authority — in one atomic operation. Issuing them sequentially could
/// consume an earlier bit and then fail on a later one, stranding it and
/// caching that failure below forever (Codex P1). The Work surface respawns its worker on
/// every folder change and again whenever a command channel dies, so issuance
/// must never sit on the spawn path — the second folder bind would fail. This
/// `OnceLock` is that single bootstrap: callers get an `Arc` clone, and the
/// gates are issued at most once no matter how many sessions come and go.
///
/// The registry authority minted with the bundle is the process's sole
/// [`awili_engine_log::RegistryAuthority`]; it rides the shared authority
/// into the seam composition, which mints the resolved identity for every
/// registry resolution (unit 4 item 3).
///
/// The error is retained (not re-attempted) so a process that somehow lost the
/// race reports the same honest failure every time rather than appearing to
/// recover.
pub fn authority() -> Result<Arc<HostAuthority>, AuthorityError> {
    static AUTHORITY: OnceLock<Result<Arc<HostAuthority>, AuthorityError>> = OnceLock::new();
    AUTHORITY
        .get_or_init(|| {
            let (gate, approval_cap, registry_authority, install_seed) =
                issue_bundle().map_err(|_| AuthorityError::AlreadyIssued)?;
            // Unit 5 custody (rulings 29-32): the fourth member moves
            // STRAIGHT into the crate-private, process-long catalog owner
            // — it never enters HostAuthority and no accessor exists.
            crate::floor_owner::adopt_authority(install_seed);
            Ok(Arc::new(HostAuthority {
                authorizer: Authorizer::new(gate),
                approval_cap,
                registry: registry_authority,
            }))
        })
        .clone()
}

/// The process-global authority gates were already issued elsewhere, so this
/// process cannot run the `awili` engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityError {
    AlreadyIssued,
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyIssued => write!(
                f,
                "the engine authority gates were already issued in this process"
            ),
        }
    }
}

impl std::error::Error for AuthorityError {}

/// One prior (user, assistant) turn restored from the persisted
/// conversation (ship-fix P0-2): the UI transcript survived a relaunch or
/// worker respawn, so the model's context must too. Text-only — historical
/// tool calls are not replayed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RestoredTurn {
    pub user: String,
    pub assistant: String,
}

/// RC1-J: the model's own history for one conversation — exactly what the
/// model was shown and said: user prompts, its replies, its tool calls and
/// their results. Never the app's transcript text (approval notices, outcome
/// lines, failure lines are Awili's words, not the model's). A worker
/// publishes it after every turn; the next worker for the same conversation
/// starts from it, so a respawn looks continuous to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelHistory(ContextHistory);

impl ModelHistory {
    /// Stored form (one JSON value), for keeping it across launches.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.0).unwrap_or_default()
    }

    /// `None` for anything that is not a stored history.
    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok().map(Self)
    }
}

/// RC1-J: where a worker publishes its [`ModelHistory`] after each turn.
pub type HistorySlot = Arc<std::sync::Mutex<Option<ModelHistory>>>;

/// Configuration for one engine session.
#[derive(Clone, PartialEq)]
pub struct AwiliEngineConfig {
    /// OpenAI-compatible server root (the Chat Completions path is appended).
    pub base_url: String,
    pub model: String,
    /// Bearer credential for the engine endpoint, already resolved by the
    /// app (`[engine].api_key` over the `AWILI_ENGINE_KEY` env var — the
    /// transport never reads the environment). Empty = send no
    /// Authorization header. Redacted in `Debug` so a logged config cannot
    /// disclose it (H4).
    pub api_key: String,
    /// Must be [`CHAT_COMPLETIONS`] in u1.
    pub dialect: String,
    /// C4 context window for the session profile.
    pub window_tokens: u64,
    /// Per-request transport timeout.
    pub request_timeout: Duration,
    /// Durable B2 execution log path (the replay artifact).
    pub log_path: PathBuf,
    /// C1 grant store path.
    pub grant_path: PathBuf,
    /// Prior turns to hydrate the model context with (ship-fix P0-2):
    /// the persisted transcript of THIS conversation, oldest first,
    /// excluding the turn about to be submitted.
    pub restored: Vec<RestoredTurn>,
    /// RC1-J: this conversation's exact model history, when the app has it
    /// for the start of the transcript. The worker starts from it; `restored`
    /// (what came after it) follows (RC1-J1-FIX2).
    pub history: Option<ModelHistory>,
}

// Custom `Debug` so a debug-logged worker config never leaks the engine
// bearer (H4); presence stays observable.
impl std::fmt::Debug for AwiliEngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwiliEngineConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    "<unset>"
                } else {
                    "<redacted>"
                },
            )
            .field("dialect", &self.dialect)
            .field("window_tokens", &self.window_tokens)
            .field("request_timeout", &self.request_timeout)
            .field("log_path", &self.log_path)
            .field("grant_path", &self.grant_path)
            .field("restored", &self.restored)
            .field("history", &self.history.is_some())
            .finish()
    }
}

impl AwiliEngineConfig {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        log_path: PathBuf,
        grant_path: PathBuf,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: String::new(),
            dialect: CHAT_COMPLETIONS.into(),
            window_tokens: DEFAULT_WINDOW_TOKENS,
            request_timeout: Duration::from_secs(180),
            log_path,
            grant_path,
            restored: Vec::new(),
            history: None,
        }
    }
}

/// An `awili-engine` Work session. One worker = one session = one B2 log =
/// one C4 [`ContextHistory`]; each `Prompt` is one turn over that history
/// (so compaction is reachable exactly as the C4 live gate proved).
/// Live MCP composition for one Work worker (unit 4b commit 5): the
/// executor composition (the coordinator facade behind
/// `Executors::mcp`), the app-owned per-turn DRIVE (reconcile + prepare +
/// commit + publish over the app's `WorkMcpState`, returning `run_turn`'s
/// two MCP inputs), and the coordinator itself — owned HERE so the
/// runtime's lifetime is the worker's: worker drop → coordinator drop →
/// runtime shutdown, in that order.
pub struct WorkerMcp {
    pub executors: awili_seam::Executors,
    /// Turn-open drive: tools, accepted roots, degradation lines, and —
    /// unit 5 — the OPTIONAL install seed candidate for this turn (it
    /// crosses the boundary once, ruling 16; the worker mints it through
    /// the store service before any authorization).
    #[allow(clippy::type_complexity)]
    pub turn_open: Box<
        dyn FnMut() -> (
                Vec<awili_tools::ToolDef>,
                RootsAcceptance,
                Vec<String>,
                Option<awili_engine_log::authz::InstallSeedCandidate>,
                // Ship-fix P0-5 (runtime half): REQUIRED origins that did
                // not become available within the composition's bounded
                // wait — a non-empty list FAILS the turn instead of
                // silently running without them.
                Vec<String>,
            ) + Send,
    >,
    /// The per-call install authority query (unit 5 §5): app-implemented
    /// over current config/setup/coordinator state.
    pub install_authority: std::sync::Arc<dyn awili_engine_authz::InstallAuthorityQuery>,
    pub coordinator: crate::mcp_coordinator::McpCoordinator<awili_engine_log::IntentTicket>,
}

/// Test/observer hook fired when a deadline-bounded approval verdict is
/// rejected as stale (`None`) or names the rejected id.
#[cfg(test)]
pub(crate) type ApprovalRejectedHook = Arc<dyn Fn(Option<&str>) + Send + Sync>;

/// RC2-ENGG4: a test rig's stand-in for one attempt to lock the
/// conversation's log.
#[cfg(test)]
pub(crate) type LogTryLock =
    Arc<dyn Fn(&std::fs::File) -> Result<(), std::fs::TryLockError> + Send + Sync>;

pub struct AwiliEngineWorker {
    cfg: AwiliEngineConfig,
    authority: Arc<HostAuthority>,
    /// Transport factory — injected so tests drive scripted bodies through
    /// the REAL worker. Defaults to `awili_seam::HttpTransport`.
    transport: Box<dyn Transport + Send>,
    /// Live MCP composition (unit 4b commit 5). `None` = builtin-only —
    /// exactly unit 4's posture, and the Hermes engine's always.
    mcp: Option<WorkerMcp>,
    /// U4 (D2/D7): the conversation-wide Ask/Auto cell, read at each
    /// approval gate. `Default` is Ask, so a worker built without
    /// `with_approval_mode` always asks.
    approval_mode: SharedApprovalMode,
    /// RC1-J: where this worker publishes its model history after each turn.
    history_slot: Option<HistorySlot>,
    /// RC2-R2F7: its model works for the owner anywhere (a council member):
    /// the file tools reach any folder by a full path (reads pass, changes
    /// still ask). `false` (the default): the session's folder bounds them.
    any_folder: bool,
    /// Test-only instrumentation (design r2, Codex verdict 2026-08-14).
    /// The field itself does not exist outside `cfg(test)` builds, so a
    /// normal worker can carry no hook by construction.
    #[cfg(test)]
    hooks: HostHooks,
    /// RC1-N-FIX4 test-only fault (see [`WorkerLogStore`]); absent from
    /// every normal worker.
    #[cfg(test)]
    log_fault: Option<&'static str>,
}

/// RC1-N-FIX4: the one plain line when the execution log could not be
/// written. The worker ends; the next prompt starts a new worker whose
/// startup replay names any step that may have run and holds new work
/// behind the owner's answer.
pub(crate) const LOG_WRITE_FAILED: &str = "Awili could not save its record of this turn, so the \
session stopped. A step may have run; your next message shows it before anything runs again.";

/// RC2-ENG4 (F0645): how long a new worker waits for an earlier worker of
/// the same conversation (being stopped during a respawn) to let go of the
/// conversation's log.
const LOG_LOCK_WAIT: Duration = Duration::from_secs(20);

/// RC2-ENG4 (F0645): shown when the earlier worker never let go.
pub(crate) const LOG_BUSY: &str = "This conversation is still open in an Awili helper that has \
not finished stopping. Try again in a moment.";

/// RC2-ENG4 (F0644): shown when the saved permissions cannot be read at start.
pub(crate) const GRANTS_UNREADABLE: &str = "Awili could not read its saved permissions, so it \
cannot check whether an earlier session left a step unfinished. Nothing was run.";

/// RC2-ENG4 (F0645): hold the conversation's log exclusively for this
/// worker's whole life. Session ids are unique per worker (RC1-I2-FIX1), but
/// turn ids continue from a scan of the log at startup — so an earlier
/// worker still writing the same log during a respawn could mint the same
/// turn id. The new worker waits (up to `wait`) until the earlier one is
/// gone, then scans; `None` when it never let go. The lock is an advisory
/// `flock` on the log file itself, released when the handle drops.
/// `on_wait` runs once, when the log is first found held (RC2-ENGF4).
/// `try_lock` is one attempt to take it (`File::try_lock`; RC2-ENGG4: a
/// test rig controls it).
fn lock_log(
    path: &std::path::Path,
    wait: Duration,
    on_wait: impl FnOnce(),
    mut try_lock: impl FnMut(&std::fs::File) -> Result<(), std::fs::TryLockError>,
) -> Result<Option<std::fs::File>, String> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open execution log {}: {e}", path.display()))?;
    let deadline = std::time::Instant::now() + wait;
    let mut on_wait = Some(on_wait);
    loop {
        match try_lock(&file) {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                if let Some(note) = on_wait.take() {
                    note();
                }
                if std::time::Instant::now() >= deadline {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!("cannot lock execution log {}: {e}", path.display()))
            }
        }
    }
}

/// RC1-N-FIX4: the worker's execution-log storage — the log file, plus
/// (test builds only) a one-shot fault: the first append whose record holds
/// the given text fails cleanly, as a full disk would.
struct WorkerLogStore {
    file: awili_engine_log::storage::FileLog,
    #[cfg(test)]
    fail_append_of: Option<&'static str>,
}

impl awili_engine_log::storage::AppendOnlyLog for WorkerLogStore {
    fn append(&mut self, bytes: &[u8], sync: bool) -> Result<(), awili_engine_log::LogError> {
        #[cfg(test)]
        if let Some(text) = self.fail_append_of {
            if bytes.windows(text.len()).any(|w| w == text.as_bytes()) {
                self.fail_append_of = None;
                return Err(awili_engine_log::LogError::Storage(
                    "injected append failure".into(),
                ));
            }
        }
        self.file.append(bytes, sync)
    }

    fn read_all(&self) -> Result<Vec<u8>, awili_engine_log::LogError> {
        self.file.read_all()
    }
}

/// Test-only instrumentation points at the actual `ChannelHost` decision
/// and observation boundaries (design r2 / Codex P0, P1). Injected through
/// test construction ([`AwiliEngineWorker::with_transport_and_hooks`]) only;
/// absent from every normal worker.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct HostHooks {
    /// Fires on the `ChannelHost::observe` path at the canonical
    /// completed-tool event, immediately before `WorkEvent::ToolFinished`
    /// is forwarded. This is the synchronous completion boundary on the
    /// worker thread: it happens-before `run_turn`'s next cancellation
    /// poll, which precedes every transport request — so a rig that trips
    /// its cancel flag here proves the between-rounds stop without racing
    /// the worker.
    pub(crate) on_tool_completed: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Fires in `ChannelHost::approve` after each mismatched-`id` verdict
    /// is classified and before the wait loop resumes, carrying the
    /// rejected id form (`Some(wrong_id)` or `None`). A rig waits —
    /// deadline-bounded — on the record to prove a stale verdict was
    /// dropped before asserting anything.
    pub(crate) on_approval_rejected: Option<ApprovalRejectedHook>,
    /// RC2-ENGF4: fires once when the conversation's log is found held by
    /// an earlier worker, before this worker waits for it — a rig's
    /// deterministic proof that the new worker is waiting (no sleep).
    pub(crate) on_log_wait: Option<Arc<dyn Fn() + Send + Sync>>,
    /// RC2-ENGG4: replaces each attempt to lock the log, so a rig decides
    /// when the earlier worker lets go and writes its last turn at that
    /// very moment — proof that the turn scan follows the lock.
    pub(crate) log_try_lock: Option<LogTryLock>,
}

impl AwiliEngineWorker {
    /// Build a worker over the real HTTP transport. Tests only: production
    /// callers build the transport themselves (`awili_seam::HttpTransport`
    /// with its request preflight) and use [`Self::with_transport`], so no
    /// production path can reach a model host without the app's gate.
    #[cfg(test)]
    pub fn new(cfg: AwiliEngineConfig, authority: Arc<HostAuthority>) -> Result<Self, ConfigError> {
        let transport = Box::new(awili_seam::HttpTransport::with_api_key(
            &cfg.base_url,
            cfg.request_timeout,
            Some(cfg.api_key.clone()),
        ));
        Self::with_transport(cfg, authority, transport)
    }

    /// Build a worker over a caller-supplied transport (tests, future
    /// dialect factories).
    pub fn with_transport(
        cfg: AwiliEngineConfig,
        authority: Arc<HostAuthority>,
        transport: Box<dyn Transport + Send>,
    ) -> Result<Self, ConfigError> {
        if cfg.dialect != CHAT_COMPLETIONS {
            return Err(ConfigError::UnsupportedDialect(cfg.dialect.clone()));
        }
        Ok(Self {
            cfg,
            authority,
            transport,
            mcp: None,
            approval_mode: SharedApprovalMode::default(),
            history_slot: None,
            any_folder: false,
            #[cfg(test)]
            hooks: HostHooks::default(),
            #[cfg(test)]
            log_fault: None,
        })
    }

    /// RC1-N-FIX4 (tests): the first execution-log append whose record holds
    /// `text` fails cleanly.
    #[cfg(test)]
    pub(crate) fn with_log_fault(mut self, text: &'static str) -> Self {
        self.log_fault = Some(text);
        self
    }

    /// Attach the live MCP composition (unit 4b commit 5). The FIRST
    /// production launch path: without this call the worker is
    /// builtin-only.
    pub fn with_mcp(mut self, mcp: WorkerMcp) -> Self {
        self.mcp = Some(mcp);
        self
    }

    /// U4 (D2): attach the conversation's shared Ask/Auto cell. Clones of
    /// the cell the app holds keep the running turn in step with a mode
    /// switch: the worker reads the cell at each approval gate.
    pub fn with_approval_mode(mut self, mode: SharedApprovalMode) -> Self {
        self.approval_mode = mode;
        self
    }

    /// RC2-R2F7: let the file tools reach any folder by a full path (a
    /// council member, which has no folder of its own to work in).
    pub fn with_any_folder(mut self, any_folder: bool) -> Self {
        self.any_folder = any_folder;
        self
    }

    /// RC1-J: publish the model history into `slot` after every turn.
    pub fn with_history_slot(mut self, slot: HistorySlot) -> Self {
        self.history_slot = Some(slot);
        self
    }

    /// Test construction: the same worker, plus instrumentation hooks at
    /// the `ChannelHost` boundaries (design r2). No production path can
    /// reach this constructor, and normal workers carry no hooks.
    #[cfg(test)]
    pub(crate) fn with_transport_and_hooks(
        cfg: AwiliEngineConfig,
        authority: Arc<HostAuthority>,
        transport: Box<dyn Transport + Send>,
        hooks: HostHooks,
    ) -> Result<Self, ConfigError> {
        let mut worker = Self::with_transport(cfg, authority, transport)?;
        worker.hooks = hooks;
        Ok(worker)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// u1 speaks Chat Completions only; u2b adds Responses/Anthropic.
    UnsupportedDialect(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedDialect(d) => write!(
                f,
                "unsupported dialect {d:?}: this build speaks {CHAT_COMPLETIONS} only"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

// ============================================================================
// The bridge
// ============================================================================

/// Implements [`TurnHost`] over the worker's channels: the approval dialog
/// is a `WorkEvent` out + a blocking `WorkCmd` wait, live events are
/// `WorkEvent`s, and cancellation is the shared flag.
/// RC1-N (usability round 4 M3): the text a failed turn shows. A turn
/// that needed more room than the model's window — Awili's own assembly
/// ("cannot fit", still over budget after compaction) or the model server
/// refusing an over-long request — gets one plain sentence, never the raw
/// engine error (which goes to stderr). Every other failure is unchanged.
/// RC2-GEMMA8 (S-N1): the recovery line for one unfinished or unchecked step
/// of an earlier session — it names the file the step was working on (else
/// its command), so the owner knows what to check.
pub(crate) fn recovery_line(intent: &awili_engine_log::replay::IntentRecovery) -> Option<String> {
    use awili_engine_log::replay::IntentStatus;
    let what = step_subject(&intent.raw_arguments)
        .map(|s| format!("\u{201C}{s}\u{201D}"))
        .unwrap_or_else(|| "its target".to_string());
    match &intent.status {
        IntentStatus::Uncertain => Some(format!(
            "recovery: {} on {what} (turn {}, step {}) started but Awili never saw it finish, \
             so {what} may or may not have changed. Check it before you try again.",
            intent.tool, intent.turn_id, intent.intent_id
        )),
        IntentStatus::AuthzInvalid { reason } => Some(format!(
            "recovery: {} on {what} (turn {}) has a record whose approval could not be checked \
             ({reason}), so treat what it did as unchecked.",
            intent.tool, intent.turn_id
        )),
        _ => None,
    }
}

/// RC2-GEMMA8: what a step worked on, from its logged arguments: the file
/// (`path`, `file_path`, `file`), else the command — shortened past 120
/// characters, on one line.
fn step_subject(raw_arguments: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw_arguments).ok()?;
    let s = ["path", "file_path", "file", "command"]
        .iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()))?;
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return None;
    }
    Some(if one_line.chars().count() > 120 {
        one_line.chars().take(119).collect::<String>() + "\u{2026}"
    } else {
        one_line
    })
}

/// RC2-GEMMA4: an error that says the model server itself broke (mlx_lm:
/// "generation thread died").
fn server_broken(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains("generation thread died")
}

pub(crate) fn plain_turn_failure(e: &SeamError, window_tokens: u64) -> String {
    let overflow = match e {
        // Pre-review P1 (pass 6): a summarizer request the model server
        // refused as too long reads the same.
        SeamError::Context(m) => {
            let l = m.to_ascii_lowercase();
            l.contains("cannot fit")
                || l.contains("over budget")
                || awili_seam::context_overflow_text(m)
        }
        SeamError::Transport(m) => awili_seam::context_overflow_text(m),
        // A model server's error body ends the turn through the reducer.
        SeamError::TurnFailed { reason, .. } => awili_seam::context_overflow_text(reason),
        _ => false,
    };
    // RC2-GEMMA4 (R7-A3): plain words for a reply cut off (twice) and for a
    // model server whose generation died, never "stream truncated
    // unexpectedly after N deltas".
    if let SeamError::TurnFailed { report, .. } = e {
        for ev in &report.events {
            match ev {
                AgentEvent::TurnFailed(EngineError::UnexpectedTruncation { .. }) => {
                    eprintln!("awili-engine-worker: {e}");
                    return REPLY_CUT_TWICE.into();
                }
                AgentEvent::TurnFailed(EngineError::Provider { message, .. })
                    if server_broken(message) =>
                {
                    eprintln!("awili-engine-worker: {e}");
                    return SERVER_BROKEN.into();
                }
                _ => {}
            }
        }
    }
    if overflow {
        eprintln!("awili-engine-worker: turn needed more room than the window: {e}");
        // RC1-N-FIX1 (whole-slice review P2): steps of this turn may already
        // have run (a file written, an MCP action) — never claim otherwise.
        format!(
            "This turn needed more room than the model can hold ({window_tokens} tokens), so it \
             stopped here. Steps it had already taken may have completed \u{2014} check the \
             steps list. Ask for a smaller step, or use a model with a larger window."
        )
    } else {
        format!("{e}")
    }
}

struct ChannelHost<'a> {
    events: &'a async_channel::Sender<WorkEvent>,
    cmds: &'a std::sync::mpsc::Receiver<WorkCmd>,
    cancel: &'a std::sync::atomic::AtomicBool,
    model: String,
    turn_id: String,
    /// RC2-R1F1 (usability RC2 round 1, N4): the folder the turn runs in,
    /// so a file card names the whole path (folder and file).
    workdir: String,
    /// `awili_call_id → tool name`, so a finished tool closes the row the
    /// started tool opened (Codex P0-3). Cleared per turn.
    call_names: std::collections::HashMap<String, String>,
    /// RC1-I3: `awili_call_id → arguments`, for the approval card's plain
    /// words. Cleared per turn.
    call_args: std::collections::HashMap<String, String>,
    /// RC1-O: calls to a tool that is not one of the engine's own file tools
    /// and whose registry floor is not read-only (an owner MCP server's
    /// `write_file`). Taken out when the call's result arrives.
    may_change: std::collections::HashSet<String>,
    /// Set when the blocking dialog saw a Shutdown — the loop exits after
    /// the turn unwinds.
    shutdown_seen: bool,
    /// Set when the command channel closed while blocked.
    disconnected: bool,
    /// RC1-N: the owner answered a card with `deny-stop` — the denial is
    /// recorded and the turn ends there (`StopCause::Denied`).
    stop_after_deny: bool,
    /// U4 (D2/D7): a clone of the conversation's shared mode cell, read at
    /// each approval gate so a mid-turn switch applies at the next gate.
    approval_mode: SharedApprovalMode,
    /// RC2-R2F7: the worker's `any_folder`.
    any_folder: bool,
    /// RC2-R4: Gemma's thinking spans are moved out of the reply text.
    channel_split: channel_split::ChannelSplit,
    /// Test-only instrumentation (design r2); always default in normal
    /// workers.
    #[cfg(test)]
    hooks: HostHooks,
}

impl ChannelHost<'_> {
    /// RC1-O-FIX6: report every dispatched call still waiting for its result
    /// (see `may_change`) as one that may have changed files, then forget
    /// them. Best effort: a gone surface has nothing to show.
    fn flush_may_change(&mut self) {
        let mut ids: Vec<String> = self.may_change.drain().collect();
        ids.sort();
        for id in ids {
            if let Some(name) = self.call_names.get(&id).cloned() {
                let _ = self.send(WorkEvent::ToolMayHaveChangedFiles { name });
            }
        }
    }

    fn send(&self, ev: WorkEvent) -> Result<(), ObserverGone> {
        self.events.send_blocking(ev).map_err(|_| ObserverGone)
    }

    /// RC2-R4: reply text as tokens, Gemma's thinking spans as reasoning.
    fn send_pieces(&self, pieces: Vec<channel_split::Piece>) -> Result<(), ObserverGone> {
        for piece in pieces {
            self.send(match piece {
                channel_split::Piece::Text(t) => WorkEvent::Token(t),
                channel_split::Piece::Thinking(t) => WorkEvent::ReasoningDelta { text: t },
            })?;
        }
        Ok(())
    }

    /// Best-effort file path for a builtin fs call, so the app's
    /// touched-files rail works (Codex P1). Engine-side knowledge of its
    /// OWN builtins — not a guess about arbitrary tools.
    fn builtin_file_path(name: &str, arguments: &str) -> Option<String> {
        if !name.starts_with("fs.") {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(arguments).ok()?;
        v.get("path")?.as_str().map(|s| s.to_string())
    }

    /// A denial the host hook produced (P3a): emit the TYPED
    /// `{source, reason, recovery}` fact first — so the transcript renders
    /// `Blocked by <source>: <reason>` with only the supplied recovery —
    /// then return the deny verdict (which the authorizer records durably as
    /// an `AuthzDenied`). A missing surface is a `Disconnected` outcome: the
    /// turn stops before resolution, so nothing is granted or recorded.
    fn denied(&mut self, reason: &str, recovery: Option<&str>) -> ApprovalOutcome {
        if self
            .send(WorkEvent::HookDenial {
                source: "authorization".into(),
                reason: reason.to_string(),
                recovery: recovery.map(str::to_string),
            })
            .is_err()
        {
            return ApprovalOutcome::Disconnected;
        }
        ApprovalOutcome::Verdict(UserVerdict::Deny {
            reason: reason.to_string(),
        })
    }
}

impl TurnHost for ChannelHost<'_> {
    /// U4 (D7): a non-floor Ask is auto-approved when the conversation is
    /// in Auto. The cell is read HERE, at each gate, so a switch applies at
    /// the next gate — flipping to Ask mid-turn takes effect immediately,
    /// and the seam's floor check still forces a card for secret files and
    /// reads outside the folder. `approve` is never touched.
    fn auto_approve(&mut self, _call_id: &str, _prompt: &str, _risk: RiskClass) -> bool {
        self.approval_mode.clone().get() == ApprovalMode::Auto
    }

    fn any_folder(&self) -> bool {
        self.any_folder
    }

    /// RC2-R3 (round 3 Major 1): a Work turn's folder is the owner's Work
    /// folder; a council member (any folder) has none.
    fn relative_paths_mean_the_folder(&self) -> bool {
        !self.any_folder
    }

    fn tool_dispatching(&mut self, call_id: &str, floor: RiskClass) {
        let own = self
            .call_names
            .get(call_id)
            .is_some_and(|n| BUILTIN_FS_TOOLS.contains(&n.as_str()));
        if !own && floor != RiskClass::ReadOnly {
            self.may_change.insert(call_id.to_string());
        }
    }

    fn approve(&mut self, call_id: &str, prompt: &str, risk: RiskClass) -> ApprovalOutcome {
        // Stable and UNIQUE per call — a batch raising several dialogs must
        // not reuse one id (round-1 P1). The surface echoes it back on
        // `WorkCmd::Approval`, and a verdict carrying any other id is dropped
        // below: that correlation is what closes the D2-u1 loop.
        let approval_id = format!("{}-approval-{}", self.turn_id, call_id);
        // RC1-I3: plain words for the built-in file tools; RC2-R1F1: with
        // the whole path ("Write the file /Users/me/k3/notes.txt (40
        // characters)"); the authorizer's prompt for everything else.
        let description = self
            .call_names
            .get(call_id)
            .zip(self.call_args.get(call_id))
            .and_then(|(name, args)| {
                plain_search_card(name, args, &self.workdir, prompt)
                    .or_else(|| plain_approval_card(name, args, &self.workdir))
                    .or_else(|| plain_tool_call(name, args, true))
            })
            .unwrap_or_else(|| format!("[{}] {prompt}", risk.as_str()));
        if self
            .send(WorkEvent::ApprovalRequested {
                approval_id: Some(approval_id.clone()),
                command: None,
                description: Some(description),
                allow_permanent: false,
                // P3a/S2a: the TYPED risk class rides the gate to the surface,
                // which decides which scopes to offer from it (never `always`
                // for irreversible/privileged).
                risk: approval_risk_of(risk),
            })
            .is_err()
        {
            return ApprovalOutcome::Disconnected;
        }
        // Block until the answer — but poll, so a cancel FLAG trip resolves
        // the dialog even with no WorkCmd (Codex P0-1).
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return ApprovalOutcome::Cancelled;
            }
            match self.cmds.recv_timeout(APPROVAL_POLL) {
                Ok(WorkCmd::Approval { id, choice }) => {
                    // FAIL CLOSED on correlation. A verdict is only an answer
                    // to THIS gate if it echoes this gate's id; anything else
                    // — a stale verdict from a dialog already resolved, or an
                    // uncorrelated `None` from a surface that never plumbed
                    // the id — is dropped, and the gate keeps waiting for its
                    // own answer. Dropping rather than denying is deliberate:
                    // treating a stale message as a deny would let a race kill
                    // legitimate work, and the loop still observes `cancel`
                    // every `APPROVAL_POLL`, so this can never wedge.
                    if id.as_deref() != Some(approval_id.as_str()) {
                        eprintln!(
                            "awili-engine-worker: dropping approval for {:?} while gate {approval_id:?} \
                             is pending",
                            id.as_deref().unwrap_or("<no id>")
                        );
                        // Test-only fence (design r2, Codex P1): runs after
                        // the verdict is classified and before the loop
                        // resumes, so a rig can wait — deadline-bounded —
                        // until the stale input was provably dropped.
                        #[cfg(test)]
                        if let Some(on_rejected) = &self.hooks.on_approval_rejected {
                            on_rejected(id.as_deref());
                        }
                        continue;
                    }
                    // P3a: the scope crosses verbatim. The AUTHORIZER stays
                    // the minting authority — once/session go to the session
                    // log, folder/always to the grant store, `install` is
                    // ungrantable, and `always` is refused for
                    // irreversible/privileged (contract §8). This hook only
                    // shapes the verdict and reports the typed denial fact.
                    return match choice.as_str() {
                        "once" | "session" | "folder" => {
                            ApprovalOutcome::Verdict(UserVerdict::Approve {
                                scope: choice.to_string(),
                            })
                        }
                        "always" => {
                            // Hook-level mirror of the contract §8 floor (the
                            // authorizer refuses it too — fail closed): the
                            // surface must not offer `always` for these risks,
                            // and a strayed one is a TYPED denial, never a
                            // silent grant.
                            if matches!(risk, RiskClass::Irreversible | RiskClass::Privileged) {
                                return self.denied(
                                    "irreversible and privileged actions cannot be granted 'always'",
                                    Some(
                                        "approve for this session (Allow for session), or deny",
                                    ),
                                );
                            }
                            ApprovalOutcome::Verdict(UserVerdict::Approve {
                                scope: "always".into(),
                            })
                        }
                        "deny" => self.denied("denied by the user", None),
                        // RC1-N: the Work surface's Deny. The authorizer
                        // records the denial durably (the seam resolves the
                        // Deny verdict first), then the turn ends: the owner
                        // gets one plain line and the request back, so no
                        // HookDenial line and no model reply about the
                        // refusal reach the transcript.
                        "deny-stop" => {
                            self.stop_after_deny = true;
                            ApprovalOutcome::Verdict(UserVerdict::Deny {
                                reason: "denied by the user".into(),
                            })
                        }
                        // A scope this build cannot honor must never be
                        // silently accepted (fail closed): refuse it as a
                        // typed denial naming what the surface offers.
                        other => {
                            eprintln!(
                                "awili-engine-worker: unsupported approval choice {other:?}; \
                                 treating as deny"
                            );
                            self.denied(
                                &format!("unsupported approval scope {other:?}"),
                                Some(
                                    "this surface offers once, session, folder, always, deny, \
                                     and deny-stop",
                                ),
                            )
                        }
                    };
                }
                Ok(WorkCmd::Cancel) => return ApprovalOutcome::Cancelled,
                Ok(WorkCmd::Shutdown) => {
                    self.shutdown_seen = true;
                    return ApprovalOutcome::Shutdown;
                }
                // Cannot submit mid-turn (matches the Hermes worker); other
                // response kinds have no pending request in this engine.
                Ok(_) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // A cancel already in flight wins over an incidental
                    // channel teardown: the user cancelled, THEN the app tore
                    // the worker down, and the surface should see the stop.
                    if self.cancel.load(Ordering::SeqCst) {
                        return ApprovalOutcome::Cancelled;
                    }
                    self.disconnected = true;
                    return ApprovalOutcome::Disconnected;
                }
            }
        }
    }

    fn observe(&mut self, event: &AgentEvent) -> Result<(), ObserverGone> {
        // RC2-R4: a reply piece held back (a possible `<|channel>` start) goes
        // out before anything else is reported.
        if !matches!(event, AgentEvent::TextDelta(_)) {
            let held = self.channel_split.flush();
            self.send_pieces(held)?;
        }
        match event {
            AgentEvent::TurnStarted => self.send(WorkEvent::TurnStarted {
                model: Some(self.model.clone()),
            }),
            AgentEvent::TextDelta(t) => {
                let pieces = self.channel_split.push(t);
                self.send_pieces(pieces)
            }
            AgentEvent::ReasoningDelta(t) => {
                self.send(WorkEvent::ReasoningDelta { text: t.clone() })
            }
            AgentEvent::ToolCallReady(c) => {
                self.call_names
                    .insert(c.awili_call_id.0.clone(), c.name.clone());
                self.call_args
                    .insert(c.awili_call_id.0.clone(), c.arguments.clone());
                // RC1-J: Progress and the Tools list show the card's plain
                // words ("Write notes.txt (40 characters)"), never the raw
                // JSON arguments; a tool without plain words shows its name.
                self.send(WorkEvent::ToolStarted {
                    name: Some(c.name.clone()),
                    detail: plain_approval(&c.name, &c.arguments)
                        .or_else(|| plain_tool_call(&c.name, &c.arguments, false)),
                    file_path: Self::builtin_file_path(&c.name, &c.arguments),
                })
            }
            AgentEvent::ToolResultIngested(r) => {
                // Test-only completion hook (design r2, Codex P0): this is
                // the canonical completed-tool event — the synchronous
                // boundary where a rig trips its cancel flag ON the worker
                // thread, ahead of run_turn's next cancellation poll.
                #[cfg(test)]
                if let Some(on_done) = &self.hooks.on_tool_completed {
                    on_done();
                }
                let name = self.lookup_name(&r.awili_call_id);
                // RC1-O: another tool that may change files ran; the footer
                // must not say "no files changed" — even when it returned an
                // error, as a command can write and then fail (RC1-O-FIX5).
                if self.may_change.remove(&r.awili_call_id.0) {
                    if let Some(n) = &name {
                        self.send(WorkEvent::ToolMayHaveChangedFiles { name: n.clone() })?;
                    }
                }
                self.send(WorkEvent::ToolFinished {
                    name,
                    detail: plain_result(&r.content),
                    duration_s: None,
                    ok: Some(!r.is_error),
                })
            }
            // RC1-F4b: each request's usage reaches the run's token count
            // (the app sums the frames, as for the agent lane).
            AgentEvent::UsageReported(u) => self.send(WorkEvent::UsageReported {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            }),
            // Facts the Work surface has no row for today. They are durable
            // in B2; emitting nothing preserves existing app behavior.
            AgentEvent::ContinuationBlobCaptured(_)
            | AgentEvent::InputRejected { .. }
            | AgentEvent::ToolCallInvalid { .. } => Ok(()),
            // Terminal events: the LOOP emits the turn-boundary vocabulary
            // (Codex P0-2), so the observer stays stateless about outcome.
            AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnCancelled { .. }
            | AgentEvent::TurnFailed(_) => Ok(()),
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn stop_after_denial(&self) -> bool {
        self.stop_after_deny
    }

    fn compaction_started(&mut self) -> Result<(), ObserverGone> {
        // The app renders kind="compacting" as an explicit progress line —
        // Awili's no-silent-compaction stance, now true for its own engine.
        self.send(WorkEvent::StatusUpdate {
            kind: Some("compacting".into()),
            message: Some("compacting context…".into()),
        })
    }

    /// RC2-GEMMA3 (R7-speed): while the model server reads a long request
    /// the owner sees how far it got, in plain words, instead of a silent
    /// wait (round 7: "no activity for 44s").
    fn prompt_progress(&mut self, done: u64, total: u64) {
        let _ = self.send(WorkEvent::ThinkingStatus {
            text: Some(reading_line(done, total)),
        });
    }

    /// RC2-GEMMA4 (R7-A3): the server cut the reply off and the round is
    /// asked again: the app takes back the text this round showed
    /// (`kind = "reply_restart"`, the text as the message) and says so.
    fn reply_cut(&mut self, shown: &str) {
        let _ = self.send(WorkEvent::StatusUpdate {
            kind: Some(REPLY_RESTART.into()),
            message: Some(shown.to_string()),
        });
    }
}

/// RC2-GEMMA4: the status kind of a reply the server cut off and the engine
/// asks for again.
pub const REPLY_RESTART: &str = "reply_restart";

/// RC2-GEMMA4 (R7-A3): the reply was cut off again on the retry; the text
/// above it stays.
pub(crate) const REPLY_CUT_TWICE: &str = "The model server stopped before finishing this reply \
     (twice), so it may be cut off. Ask again to get the whole answer.";

/// RC2-GEMMA4 (R7-A3): mlx_lm's server after its generation thread died.
pub(crate) const SERVER_BROKEN: &str = "The model server stopped working and needs a restart, \
     so nothing was answered. You can send this to the cloud model instead, or try again once \
     the server is back.";

/// RC2-R617: the progress line while a turn waits for its model server to
/// finish reading the warm-up (this folder's instructions and tools).
pub(crate) fn warm_line(done: u64, total: u64) -> String {
    let pct = (done.min(total) * 100).checked_div(total).unwrap_or(0);
    format!("Reading this folder\u{2019}s instructions first\u{2026} {pct}% (once per folder)")
}

/// RC2-R617: the host of a warm-up. Nothing runs, so nothing is approved;
/// the server's reading progress goes to the warm gate and, while a prompt
/// waits behind the warm-up, to the app; a Shutdown or Cancel that arrives
/// stops the reading. Commands that arrive meanwhile are kept, in order,
/// for the worker's loop.
struct WarmHost<'a> {
    events: &'a async_channel::Sender<WorkEvent>,
    cmds: &'a std::sync::mpsc::Receiver<WorkCmd>,
    stash: std::cell::RefCell<std::collections::VecDeque<WorkCmd>>,
    /// The warm-up under way (RC2-R6M2: none while it waits for another
    /// warm-up of its server).
    warming: Option<warm_gate::Warming>,
    stop: std::cell::Cell<bool>,
}

impl WarmHost<'_> {
    fn poll(&self) {
        while let Ok(cmd) = self.cmds.try_recv() {
            if matches!(cmd, WorkCmd::Shutdown | WorkCmd::Cancel) {
                self.stop.set(true);
            }
            self.stash.borrow_mut().push_back(cmd);
        }
    }

    /// RC2-R6M2: while this warm-up waits for another of its server, a
    /// prompt that waits behind it says what it waits for.
    fn waiting(&self, done: u64, total: u64) {
        self.poll();
        if self.prompt_waits() {
            let _ = self.events.send_blocking(WorkEvent::ThinkingStatus {
                text: Some(warm_line(done, total)),
            });
        }
    }

    fn prompt_waits(&self) -> bool {
        self.stash
            .borrow()
            .iter()
            .any(|c| matches!(c, WorkCmd::Prompt { .. }))
    }
}

impl TurnHost for WarmHost<'_> {
    fn approve(&mut self, _call_id: &str, _prompt: &str, _risk: RiskClass) -> ApprovalOutcome {
        ApprovalOutcome::Cancelled
    }

    fn cancelled(&self) -> bool {
        self.poll();
        self.stop.get()
    }

    fn prompt_progress(&mut self, done: u64, total: u64) {
        if let Some(w) = &self.warming {
            w.progress(done, total);
        }
        self.waiting(done, total);
    }
}

/// RC2-GEMMA3: the progress line while the model server reads the request.
pub(crate) fn reading_line(done: u64, total: u64) -> String {
    let pct = (done.min(total) * 100).checked_div(total).unwrap_or(0);
    format!("Reading your request\u{2026} {pct}%")
}

/// RC1-I2-FIX1: a session id unique to one worker instance — process id,
/// wall-clock nanoseconds and a process-wide counter (so two workers started
/// in the same instant, or a reused process id after a relaunch, never
/// share one).
fn new_session_id() -> String {
    static WORKER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = WORKER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("sess-awili-{}-{nanos}-{seq}", std::process::id())
}

/// RC1-I3-FIX1 / -FIX2 / -FIX3: a model-supplied path as the card shows it —
/// unambiguous and reversible, on one line. An ALLOWLIST: a character is
/// shown as itself only if it is visible — general category Letter, Mark,
/// Number, Punctuation or Symbol, and not Default_Ignorable_Code_Point — or
/// the ASCII space. A backslash is shown doubled (`\\`); a tab, carriage
/// return or newline as `\t` `\r` `\n`; every other character (controls,
/// format characters, separators other than the ASCII space, default
/// ignorables, private-use and unassigned code points) as `\u{…}`. So a real
/// newline (`\n`), the two characters `\` `n` (`\\n`), U+034F (`\u{34f}`)
/// and nothing at all never look alike.
pub fn shown_path(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    // RC2-UX (S-I1): a word (a run of letters and marks) that mixes Latin
    // letters with Greek, Cyrillic, Armenian or Cherokee ones ("p\u{430}ypal":
    // a Cyrillic а among Latin letters) shows those letters escaped, so it
    // can never pass for the all-Latin word it imitates. A word in one of
    // those alphabets alone ("отчёт.txt") is shown as itself.
    let mut lookalike = vec![false; chars.len()];
    let mut start = 0;
    while start < chars.len() {
        let word = |c: char| c.is_alphabetic() || is_mark(c);
        if !word(chars[start]) {
            start += 1;
            continue;
        }
        let end = (start..chars.len())
            .find(|&i| !word(chars[i]))
            .unwrap_or(chars.len());
        let run = &chars[start..end];
        if run.iter().any(|&c| is_latin_letter(c)) && run.iter().any(|&c| is_lookalike_script(c)) {
            for (i, &c) in run.iter().enumerate() {
                lookalike[start + i] = is_lookalike_script(c);
            }
        }
        start = end;
    }
    let mut out = String::with_capacity(path.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' | '\r' | '\n' => out.extend(c.escape_default()),
            c if lookalike[i] => out.extend(c.escape_unicode()),
            c if shown_as_is(c) => out.push(c),
            c => out.extend(c.escape_unicode()),
        }
    }
    out
}

/// RC2-UX (S-I1): a combining mark (general category Mark).
fn is_mark(c: char) -> bool {
    use unicode_properties::{GeneralCategoryGroup as G, UnicodeGeneralCategory};
    c.general_category_group() == G::Mark
}

/// RC2-UX (S-I1): a Latin letter (ASCII, Latin-1, Latin Extended-A/B,
/// Latin Extended Additional).
fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
        || matches!(c as u32, 0x00C0..=0x00D6 | 0x00D8..=0x00F6 | 0x00F8..=0x024F | 0x1E00..=0x1EFF)
}

/// RC2-UX (S-I1): a letter of a script whose letters pass for Latin ones
/// (Greek, Cyrillic, Armenian, Cherokee): the scripts of the common
/// look-alike names (Unicode UTS #39 confusables).
fn is_lookalike_script(c: char) -> bool {
    matches!(
        c as u32,
        0x0370..=0x03FF
            | 0x1F00..=0x1FFF
            | 0x0400..=0x052F
            | 0x1C80..=0x1C8F
            | 0x2DE0..=0x2DFF
            | 0xA640..=0xA69F
            | 0x0531..=0x058F
            | 0x13A0..=0x13FF
            | 0xAB70..=0xABBF
    ) && c.is_alphabetic()
}

/// RC1-I3-FIX3: whether the card shows `c` as itself (see [`shown_path`]).
/// General categories come from `unicode-properties` (Unicode 17.0.0).
pub fn shown_as_is(c: char) -> bool {
    use unicode_properties::{GeneralCategoryGroup as G, UnicodeGeneralCategory};
    c == ' '
        || (matches!(
            c.general_category_group(),
            G::Letter | G::Mark | G::Number | G::Punctuation | G::Symbol
        ) && !is_default_ignorable(c))
}

/// RC1-I3-FIX3: Default_Ignorable_Code_Point, exactly as Unicode lists it in
/// DerivedCoreProperties-17.0.0.txt (the same 27 lines as 16.0.0). These
/// characters normally display as nothing, whatever their category.
const DEFAULT_IGNORABLE: [(u32, u32); 27] = [
    (0x00AD, 0x00AD),
    (0x034F, 0x034F),
    (0x061C, 0x061C),
    (0x115F, 0x1160),
    (0x17B4, 0x17B5),
    (0x180B, 0x180D),
    (0x180E, 0x180E),
    (0x180F, 0x180F),
    (0x200B, 0x200F),
    (0x202A, 0x202E),
    (0x2060, 0x2064),
    (0x2065, 0x2065),
    (0x2066, 0x206F),
    (0x3164, 0x3164),
    (0xFE00, 0xFE0F),
    (0xFEFF, 0xFEFF),
    (0xFFA0, 0xFFA0),
    (0xFFF0, 0xFFF8),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0000, 0xE0000),
    (0xE0001, 0xE0001),
    (0xE0002, 0xE001F),
    (0xE0020, 0xE007F),
    (0xE0080, 0xE00FF),
    (0xE0100, 0xE01EF),
    (0xE01F0, 0xE0FFF),
];

fn is_default_ignorable(c: char) -> bool {
    let c = c as u32;
    DEFAULT_IGNORABLE
        .iter()
        .any(|&(lo, hi)| (lo..=hi).contains(&c))
}

/// RC1-I3: the approval card's words for the in-app built-in file tools,
/// from the call's own arguments: "Write notes.txt (40 characters)",
/// "Edit notes.txt", "Read notes.txt". The path is shown whole up to 80
/// characters (cut on characters, never bytes, with "…"); the written text is
/// counted, never shown. `None` for any other tool or unreadable arguments.
fn plain_approval(name: &str, arguments: &str) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    // RC1-I3-FIX1..FIX3: only visible characters are shown as themselves;
    // everything else is escaped ("a\nb.txt", "a\u{34f}b.txt"), so one
    // target can never look like several lines or a different name.
    let path = shown_path(args.get("path")?.as_str()?);
    let path = if path.chars().count() > 80 {
        path.chars().take(79).collect::<String>() + "\u{2026}"
    } else {
        path.to_string()
    };
    match name {
        "fs.write" => {
            let n = args
                .get("contents")
                .or_else(|| args.get("content"))
                .and_then(|c| c.as_str())
                .map_or(0, |c| c.chars().count());
            let unit = if n == 1 { "character" } else { "characters" };
            Some(format!("Write {path} ({n} {unit})"))
        }
        "fs.edit" => Some(format!("Edit {path}")),
        "fs.read" => Some(format!("Read {path}")),
        _ => None,
    }
}

/// RC2-R1F1 (usability RC2 round 1, N4: "Write hello6.txt (2 characters)"
/// named neither the folder nor that it is a file write): the approval
/// card's words for the in-app built-in file tools — what happens and the
/// whole path, folder and file: "Write the file /Users/me/k3/notes.txt
/// (40 characters)", "Edit the file …", "Read the file …". A relative path
/// is shown inside the turn's folder (`workdir`, where the tool resolves
/// it); an absolute one as it is. The path is never cut, so the file name
/// always shows, and it is escaped as in [`shown_path`]. The written text is
/// counted, never shown. `None` for any other tool or unreadable arguments.
fn plain_approval_card(name: &str, arguments: &str, workdir: &str) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let raw = args.get("path")?.as_str()?;
    let full = if raw.starts_with('/') || workdir.is_empty() {
        raw.to_string()
    } else {
        let rel = raw.trim_start_matches("./");
        format!("{}/{rel}", workdir.trim_end_matches('/'))
    };
    let path = shown_path(&full);
    match name {
        "fs.write" => {
            let n = args
                .get("contents")
                .or_else(|| args.get("content"))
                .and_then(|c| c.as_str())
                .map_or(0, |c| c.chars().count());
            let unit = if n == 1 { "character" } else { "characters" };
            Some(format!("Write the file {path} ({n} {unit})"))
        }
        "fs.edit" => Some(format!("Edit the file {path}")),
        "fs.read" => Some(format!("Read the file {path}")),
        _ => None,
    }
}

/// RC2-R2G2: plain words for a search's card (`fs.glob`, `fs.grep`), with
/// the whole folder ("Search the files in /Users/me/app for "key"") and,
/// when the search reaches secret files, their names from the authorizer's
/// `prompt` ("…, including the secret files /Users/me/app/.env"): what an
/// Allow shows. `None` for any other tool.
fn plain_search_card(name: &str, arguments: &str, workdir: &str, prompt: &str) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let full = |raw: &str| {
        if raw.starts_with('/') || workdir.is_empty() {
            raw.to_string()
        } else {
            let rel = raw.trim_start_matches("./");
            let base = workdir.trim_end_matches('/');
            if rel.is_empty() {
                base.to_string()
            } else {
                format!("{base}/{rel}")
            }
        }
    };
    let head = match name {
        "fs.glob" => format!(
            "List the files matching {}",
            shown_path(&full(args.get("pattern")?.as_str()?))
        ),
        "fs.grep" => format!(
            "Search the files in {} for \"{}\"",
            shown_path(&full(args.get("under")?.as_str()?)),
            shown_path(args.get("needle")?.as_str()?).replace('"', "\\\"")
        ),
        _ => return None,
    };
    Some(
        match prompt.split_once(awili_engine_authz::SECRET_FILES_NOTE) {
            Some((_, names)) => {
                let names = names
                    .split(", ")
                    .map(|n| shown_path(&full(n)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{head}, including the secret files {names}")
            }
            None => head,
        },
    )
}

/// RC1-Q (round 5 minor 5): the Tools list's line for a finished call — the
/// tool's own result when it is one short line of prose ("wrote 2 bytes to
/// a.txt"), else nothing, so the surface shows "<name> done" instead of raw
/// JSON (`{ "degraded_reason": null, …` from a memory `recall`) or a page of
/// text. The model still gets the whole result; this is display only.
fn plain_result(content: &str) -> Option<String> {
    let t = content.trim();
    // RC2-R51 (usability RC2 round 4, Major 3): a step Awili's rules refused
    // shows its first sentence ("Not done: … is outside this conversation's
    // folder, …"), whatever its length — the guidance after it is the
    // model's.
    if t.starts_with("Not done:") {
        let first = t.split(". ").next().unwrap_or(t).trim_end_matches('.');
        return Some(format!("{first}."));
    }
    let shown = !t.is_empty()
        && t.chars().count() <= 160
        && !t.contains(['\n', '\r'])
        && !t.starts_with(['{', '[']);
    shown.then(|| t.to_string())
}

/// The engine's own file tools (canonical names, `builtin:fs`).
const BUILTIN_FS_TOOLS: [&str; 5] = ["fs.read", "fs.write", "fs.edit", "fs.glob", "fs.grep"];

/// RC1-O (round 5 K3): plain words for any other tool (an owner MCP server's
/// `write_file`, `run_command`, …), in place of the authorizer's
/// `write_file [privileged] {"content": …}`: `Run "write_file" — "content":
/// "hi", "path": "hello3.txt"`. Arguments in key order as `"name": value`;
/// the tool's name, argument names and text values are quoted (pre-review 5
/// P1: a tool named `x — "a": 1` must not read like `x` with `a: 1`) (pre-review 2 P1: `{"a":"x, b: y"}` and
/// `{"a":"x","b":"y"}`, or "false" and false, never read alike), their text
/// escaped as in [`shown_path`] (a newline shows as `\n`) with a quote shown
/// as `\"`; numbers, true/false/null, lists and objects as their compact
/// JSON, escaped the same way. `full` (the approval card, pre-review P1):
/// every argument and every value whole — the owner approves exactly what
/// will run. Not `full` (Progress and the Tools list): at most four
/// arguments, then "and N more"; a value longer than 60 characters is
/// counted instead. `None` for the engine's own file tools (they have
/// [`plain_approval`]) and when the arguments are not a JSON object.
fn plain_tool_call(name: &str, arguments: &str, full: bool) -> Option<String> {
    const MAX_ARGS: usize = 4;
    const MAX_TEXT: usize = 60;
    if BUILTIN_FS_TOOLS.contains(&name) {
        return None;
    }
    let args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let args = args.as_object()?;
    let quoted = |t: &str| format!("\"{}\"", shown_path(t).replace('"', "\\\""));
    let shown_value = |value: &serde_json::Value| {
        let (text, is_text) = match value {
            serde_json::Value::String(t) => (t.clone(), true),
            other => (other.to_string(), false),
        };
        let n = text.chars().count();
        if !full && n > MAX_TEXT {
            format!("{n} {}", if n == 1 { "character" } else { "characters" })
        } else if is_text {
            quoted(&text)
        } else {
            shown_path(&text)
        }
    };
    let take = if full { args.len() } else { MAX_ARGS };
    let mut parts: Vec<String> = args
        .iter()
        .take(take)
        .map(|(key, value)| format!("{}: {}", quoted(key), shown_value(value)))
        .collect();
    let more = args.len().saturating_sub(take);
    if more > 0 {
        parts.push(format!("and {more} more"));
    }
    let head = format!("Run {}", quoted(name));
    Some(if parts.is_empty() {
        head
    } else {
        format!("{head} \u{2014} {}", parts.join(", "))
    })
}

impl ChannelHost<'_> {
    fn lookup_name(&self, id: &AwiliCallId) -> Option<String> {
        self.call_names.get(&id.0).cloned()
    }
}

// ============================================================================
// The worker
// ============================================================================

impl EngineWorker for AwiliEngineWorker {
    fn run(self: Box<Self>, ctx: WorkerContext) {
        let this = *self;
        let mut transport = this.transport;
        transport.set_cancel_flag(ctx.cancel.clone());
        let cfg = this.cfg;
        let authority = this.authority;
        let mcp = this.mcp;
        // U4 (D2): the shared mode cell — each turn's host clones it.
        let approval_mode = this.approval_mode;
        let any_folder = this.any_folder;
        let history_slot = this.history_slot;
        // Test-only instrumentation (design r2): cloned into each turn's
        // host; nothing in a normal worker.
        #[cfg(test)]
        let hooks = this.hooks;

        // RC2-ENG4 (F0645): this worker owns the conversation's log for its
        // whole life — taken BEFORE the startup scan that numbers its turns.
        let note_log_wait = || {
            #[cfg(test)]
            if let Some(note) = &hooks.on_log_wait {
                note();
            }
        };
        let try_lock = |file: &std::fs::File| {
            #[cfg(test)]
            if let Some(attempt) = &hooks.log_try_lock {
                return attempt(file);
            }
            file.try_lock()
        };
        let _log_lock = match lock_log(&cfg.log_path, LOG_LOCK_WAIT, note_log_wait, try_lock) {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                let _ = ctx.events.send_blocking(WorkEvent::Failed(LOG_BUSY.into()));
                return;
            }
            Err(e) => {
                let _ = ctx.events.send_blocking(WorkEvent::Failed(e));
                return;
            }
        };
        // Durable stores. A failure here is worker-fatal: there is no engine
        // without its audit log.
        let log_store = match awili_engine_log::storage::FileLog::open(&cfg.log_path) {
            Ok(file) => WorkerLogStore {
                file,
                #[cfg(test)]
                fail_append_of: this.log_fault,
            },
            Err(e) => {
                let _ = ctx.events.send_blocking(WorkEvent::Failed(format!(
                    "cannot open execution log {}: {e:?}",
                    cfg.log_path.display()
                )));
                return;
            }
        };
        // Unit 5 (design r9 §2 / C1 contract §5): the per-user grant store
        // is reached through the process-global service — the worker holds
        // no long-lived local handle. cfg.grant_path names the fixed
        // per-user store; the app initializes the service (with legacy
        // migration) before spawning, and a bare worker (tests) initializes
        // it here idempotently.
        let grant_service = match awili_engine_log::grant_service::GrantStoreService::init(
            cfg.grant_path.clone(),
            None,
        ) {
            Ok(s) => s,
            Err(e) => {
                let _ = ctx.events.send_blocking(WorkEvent::Failed(format!(
                    "cannot open grant store {}: {e:?}",
                    cfg.grant_path.display()
                )));
                return;
            }
        };
        // RC1-I2-FIX1: every worker instance is its own session. The app
        // reopens a conversation's log when it respawns that conversation's
        // worker (after a switch away and back, or a relaunch); a fresh id
        // keeps the earlier worker's "Allow for this session" grants from
        // matching — a new worker asks again.
        let session_id = new_session_id();
        // S6: this session's remembered "this conversation" approvals end
        // with it (they stop being listed on every exit path).
        let _session_grants = SessionGrantsGuard(session_id.clone());
        let mut log = match ExecutionLog::open(log_store, session_id.clone()) {
            Ok(l) => l,
            Err(e) => {
                let _ = ctx
                    .events
                    .send_blocking(WorkEvent::Failed(format!("execution log: {e:?}")));
                return;
            }
        };
        let mut grants = awili_engine_log::grant_service::ServiceHandle(grant_service);

        // Ship-fix P0-1: crash recovery is WIRED, not discarded. Classify
        // the durable log at startup — before any new turn — and surface
        // every unresolved effect on the recovery channel: an UNCERTAIN
        // dispatch (started, never finalized) may or may not have run; an
        // AUTHZ-INVALID intent failed evidence verification. Neither is
        // ever auto-resumed; the surface tells the user the truth instead
        // of silently continuing. The scan also yields the next turn
        // index, so a respawned worker can never mint a turn id an
        // earlier incarnation already closed (the aliasing hazard).
        let mut turn_index = 0usize;
        // Ship-fix P0-1 (r2 bar): unresolved effects GATE the session —
        // prompts are HELD behind an explicit acknowledgement, not merely
        // warned past. The summary is what the acknowledgement card shows.
        let mut recovery_gate: Vec<String> = Vec::new();
        // RC1-N-FIX2: the log as found at startup, for the crash path below.
        let startup_records = {
            use awili_engine_log::grant_service::GrantStoreAccess as _;
            let session_records = match log.read_records() {
                Ok((r, _)) => r,
                Err(e) => {
                    let _ = ctx
                        .events
                        .send_blocking(WorkEvent::Failed(format!("execution log replay: {e:?}")));
                    return;
                }
            };
            // RC2-ENG4 (F0644): an unreadable grant store is NOT an empty
            // one — classifying against no grants would call every grant-
            // backed step "failed authorization". Stop instead, plainly.
            let grant_records = match grants.read_grant_records() {
                Ok((r, _)) => r,
                Err(e) => {
                    eprintln!("awili-engine-worker: grant store unreadable at startup: {e:?}");
                    let _ = ctx
                        .events
                        .send_blocking(WorkEvent::Failed(GRANTS_UNREADABLE.into()));
                    return;
                }
            };
            for turn in
                awili_engine_log::replay::classify_with_grants(&session_records, &grant_records)
            {
                for intent in &turn.intents {
                    // RC2-GEMMA8 (S-N1): the line names what the step was
                    // working on (its file, else its command), in plain words.
                    let line = recovery_line(intent);
                    if let Some(line) = line {
                        recovery_gate.push(line.clone());
                        if ctx
                            .events
                            .send_blocking(WorkEvent::StatusUpdate {
                                kind: Some("recovery".into()),
                                message: Some(line),
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
            // Continue turn numbering past every id the log already holds.
            for rec in &session_records {
                if let awili_engine_log::schema::LogRecordV1::TurnOpened { turn_id, .. } = rec {
                    if let Some(n) = turn_id
                        .strip_prefix("turn-awili-")
                        .and_then(|v| v.parse::<usize>().ok())
                    {
                        turn_index = turn_index.max(n + 1);
                    }
                }
            }
            session_records
        };

        if ctx
            .events
            .send_blocking(WorkEvent::Started(session_id))
            .is_err()
        {
            return;
        }

        // RC1-J / RC1-J1-FIX2: the conversation's exact model history when the
        // app has it, then (ship-fix P0-2) the restored turns that came after
        // it — or all of them — so a relaunch/respawn looks continuous to the
        // MODEL, not just to the UI. Empty rows are skipped; a pair whose
        // assistant half is empty seeds the user half alone.
        let mut history = cfg
            .history
            .clone()
            .map_or_else(ContextHistory::new, |ModelHistory(h)| h);
        for turn in &cfg.restored {
            if !turn.user.trim().is_empty() {
                history.push_user(turn.user.clone());
            }
            if !turn.assistant.trim().is_empty() {
                let _ = history.push_exchange(turn.assistant.clone(), Vec::new(), Vec::new());
            }
        }
        // RC1-N-FIX2 (whole-slice R2 P1, the crash path; pre-review pass 4):
        // the work of logged turns whose outcome the kept history does not
        // hold — the app or worker died, or the turn was stopped, shut down
        // or failed before its history was kept — is shown to the model as a
        // labelled note of earlier steps, so the next turn does not repeat
        // them; the history is then marked as covering the whole log.
        if awili_seam::recover_completed_work(&startup_records, &mut history) {
            eprintln!("awili-engine-worker: steps of an interrupted turn recovered from the log");
        }

        // The executor seam (unit 4 item 6), composition-owned and borrowed
        // per turn. Unit 4 ships NO live MCP wiring — the coordinator facade
        // exists and implements `McpExecutor` (see mcp_coordinator.rs), but
        // instantiating the real runtime and launching catalog servers is
        // unit 4b, so this composition is builtin-only and its accepted-roots
        // table is empty. A non-builtin tool may still be registered and
        // advertised here — the model may never call it — but DISPATCHING one
        // is a composition error: `Executors::route` fails closed at the
        // dispatch site, which is where a silent builtin fallback would
        // otherwise occur. There is no refuse-at-turn-open guarantee, and
        // later live wiring must not assume one.
        // Unit 4b commit 5: a composed worker dispatches through the
        // coordinator facade and re-drives the app-owned reconciliation at
        // every turn open; an uncomposed worker is byte-for-byte unit 4's
        // builtin-only posture.
        let (executors, mut mcp_turn_open, install_authority, _mcp_coordinator) = match mcp {
            Some(WorkerMcp {
                executors,
                turn_open,
                install_authority,
                coordinator,
            }) => (
                executors,
                Some(turn_open),
                install_authority,
                Some(coordinator),
            ),
            None => (
                Executors::builtin_only(),
                None,
                std::sync::Arc::new(awili_engine_authz::NoInstallAuthority)
                    as std::sync::Arc<dyn awili_engine_authz::InstallAuthorityQuery>,
                None,
            ),
        };

        // RC2-R617: commands that arrived while a warm-up read (taken first),
        // and the MCP status lines a warm-up's turn-open produced (shown with
        // the next turn's).
        let mut stash: std::collections::VecDeque<WorkCmd> = std::collections::VecDeque::new();
        let mut carried_lines: Vec<String> = Vec::new();
        // Exits when the UI drops the sender.
        loop {
            let cmd = match stash.pop_front() {
                Some(cmd) => cmd,
                None => match ctx.cmd_rx.recv() {
                    Ok(cmd) => cmd,
                    Err(_) => break,
                },
            };
            match cmd {
                // RC2-R617 (usability RC2 round 5, Major 1): read what this
                // session's requests begin with before the first question.
                WorkCmd::Warm { system } => {
                    let mut host = WarmHost {
                        events: &ctx.events,
                        cmds: &ctx.cmd_rx,
                        stash: Default::default(),
                        warming: None,
                        stop: Default::default(),
                    };
                    // RC2-R6M2 (RC2-R6 review P2 on 0fc8434b): a server busy
                    // with another warm-up (another conversation's or a
                    // council member's text) is waited for, not skipped.
                    let warming = warm_gate::begin(
                        &cfg.base_url,
                        || host.cancelled(),
                        |done, total| host.waiting(done, total),
                    );
                    host.warming = warming;
                    let ok = host.warming.is_some() && {
                        let system = system
                            .map(|s| awili_seam::split_past_context(&s).0)
                            .filter(|r| !r.trim().is_empty());
                        let mut probe = history.clone();
                        probe.set_preamble(system);
                        let (mcp_tools, _roots, degradations, _seed, required_missing) =
                            match &mut mcp_turn_open {
                                Some(turn_open) => turn_open(),
                                None => (
                                    Vec::new(),
                                    RootsAcceptance::default(),
                                    Vec::new(),
                                    None,
                                    Vec::new(),
                                ),
                            };
                        carried_lines.extend(degradations);
                        required_missing.is_empty()
                            && match awili_seam::warm_prompt_cache(
                                transport.as_mut(),
                                &cfg.model,
                                &mcp_tools,
                                cfg.window_tokens,
                                &probe,
                                &mut host,
                            ) {
                                Ok(sent) => sent && !host.stop.get(),
                                Err(e) => {
                                    eprintln!(
                                        "awili-engine-worker: the warm-up was not read: {e:?}"
                                    );
                                    false
                                }
                            }
                    };
                    drop(host.warming.take());
                    stash.extend(host.stash.into_inner());
                    // RC2-R6M2: the app counts the server warm only on this.
                    let _ = ctx.events.send_blocking(WorkEvent::Warmed { ok });
                }
                WorkCmd::Prompt { text, system } => {
                    // The recovery gate (P0-1): the FIRST prompt after a
                    // crash blocks on an explicit acknowledgement card.
                    // Approve = the user has seen the unresolved effects
                    // and takes responsibility for proceeding; Deny =
                    // stay gated (the prompt is dropped as a failed
                    // turn), so nothing runs until a human decides.
                    if !recovery_gate.is_empty() {
                        let summary = format!(
                            "Awili stopped before {} earlier step(s) finished:\n{}\nApprove \
                             to go on; deny to hold everything until you have checked.",
                            recovery_gate.len(),
                            recovery_gate.join("\n"),
                        );
                        if ctx
                            .events
                            .send_blocking(WorkEvent::ApprovalRequested {
                                approval_id: Some("recovery-gate".into()),
                                command: None,
                                description: Some(summary),
                                allow_permanent: false,
                                // The acknowledgement gate gates no tool call,
                                // so the gate is conservative: the typed
                                // `Privileged` class, for which `always` is
                                // never offered (P3a/S2a).
                                risk: ApprovalRisk::Privileged,
                            })
                            .is_err()
                        {
                            return;
                        }
                        let approved = loop {
                            match ctx.cmd_rx.recv() {
                                Ok(WorkCmd::Approval { id, choice }) => {
                                    // A stale verdict for some other gate
                                    // is dropped, exactly like the seam's
                                    // approval correlation rule.
                                    if id.as_deref() != Some("recovery-gate") {
                                        continue;
                                    }
                                    // RC1-N (pre-review P1/P2): only an
                                    // explicit approval acknowledges; any
                                    // deny (`deny-stop` is the Work
                                    // surface's) or unknown choice holds.
                                    break matches!(
                                        choice.as_str(),
                                        "once" | "session" | "folder" | "always" | "acknowledge"
                                    );
                                }
                                Ok(WorkCmd::Cancel) => break false,
                                Ok(WorkCmd::Shutdown) | Err(_) => return,
                                Ok(_) => continue,
                            }
                        };
                        if !approved {
                            if ctx
                                .events
                                .send_blocking(WorkEvent::TurnCompleted {
                                    status: RunStatus::Failed,
                                    output: Some(
                                        "recovery gate: not acknowledged — nothing was run. \
                                         Resolve the unresolved effects (or approve the \
                                         gate) and retry."
                                            .into(),
                                    ),
                                    cost_usd: None,
                                    context_percent: None,
                                })
                                .is_err()
                            {
                                return;
                            }
                            if ctx.events.send_blocking(WorkEvent::TurnComplete).is_err() {
                                return;
                            }
                            continue;
                        }
                        recovery_gate.clear();
                    }
                    // Ship-fix P0-3: the CURRENT directives/recall pin
                    // replaces last turn's preamble — first group, never
                    // summarized, never stale. RC2-GEMMA1 (R7-speed): the
                    // recalled context changes with every message, so it
                    // leaves the pinned system text (which then stays the
                    // same bytes turn after turn, and the model server
                    // answers it from its cache) and opens this turn's user
                    // message instead.
                    let (system, turn_context) = match system {
                        Some(s) => {
                            let (rest, block) = awili_seam::split_past_context(&s);
                            (Some(rest).filter(|r| !r.trim().is_empty()), block)
                        }
                        None => (None, None),
                    };
                    history.set_preamble(system);
                    // RC2-R1F6 (usability RC2 round 1, N5: warm Gemma turns
                    // took 8-9 s to first text). The recalled notes open
                    // this turn's user message AND STAY there in the kept
                    // history, so the next turn's request begins with this
                    // turn's request byte for byte and the model server
                    // reads only what is new. Measured on the Studio: with
                    // the notes taken out at the turn's end (RC2-GEMMA1)
                    // the next turn's request no longer matched from the
                    // conversation's first message on, and the server read
                    // it again (5.7 s for 1,095 tokens with a partial cache,
                    // 24 s with none); kept, it read 1,036 new tokens in
                    // 3.4 s.
                    let text = match turn_context.as_deref().map(str::trim) {
                        Some(block) if !block.is_empty() => format!("{block}\n\n{text}"),
                        _ => text,
                    };
                    let turn_id = format!("turn-awili-{turn_index}");
                    turn_index += 1;
                    let mut host_io = ChannelHost {
                        events: &ctx.events,
                        cmds: &ctx.cmd_rx,
                        cancel: &ctx.cancel,
                        model: cfg.model.clone(),
                        turn_id: turn_id.clone(),
                        workdir: ctx.workdir.clone(),
                        call_names: Default::default(),
                        call_args: Default::default(),
                        may_change: Default::default(),
                        shutdown_seen: false,
                        disconnected: false,
                        stop_after_deny: false,
                        approval_mode: approval_mode.clone(),
                        any_folder,
                        channel_split: channel_split::ChannelSplit::for_model(&cfg.model),
                        #[cfg(test)]
                        hooks: hooks.clone(),
                    };
                    // The turn-open re-drive (design r9 §5.4): both MCP
                    // inputs come from ONE locked read of the worker's own
                    // authority, after reconciliation acted on the pump's
                    // CURRENT lifecycle facts.
                    let (mcp_tools, mcp_roots, mcp_degradations, seed_candidate, required_missing) =
                        match &mut mcp_turn_open {
                            Some(turn_open) => turn_open(),
                            None => (
                                Vec::new(),
                                RootsAcceptance::default(),
                                Vec::new(),
                                None,
                                Vec::new(),
                            ),
                        };
                    // Ship-fix P0-5: a REQUIRED origin that is not live is
                    // a failed turn, never a silent builtin-only run — the
                    // memory system disappearing quietly was the exact
                    // hazard. The failure is recoverable: the next prompt
                    // re-drives the connection.
                    if !required_missing.is_empty() {
                        let msg = format!(
                            "required MCP server(s) unavailable: {} — the turn was NOT run; \
                             fix or disable the server and retry",
                            required_missing.join(", ")
                        );
                        if ctx
                            .events
                            .send_blocking(WorkEvent::TurnCompleted {
                                status: RunStatus::Failed,
                                output: Some(msg),
                                cost_usd: None,
                                context_percent: None,
                            })
                            .is_err()
                        {
                            return;
                        }
                        if ctx.events.send_blocking(WorkEvent::TurnComplete).is_err() {
                            return;
                        }
                        continue;
                    }
                    // Unit 5 seed (design §5.1, rulings 3/16): the worker —
                    // holder of the approval capability and the store
                    // service — mints the install grants BEFORE any
                    // authorization this turn; the outcome feeds the same
                    // degradation surface as every other MCP status line.
                    if let Some(candidate) = seed_candidate {
                        match grant_service.seed_install_grants(&authority.approval_cap, candidate)
                        {
                            Ok(outcome) => {
                                let mut lines = Vec::new();
                                if !outcome.seeded.is_empty() {
                                    lines.push(format!(
                                        "mcp: install grants seeded for {}",
                                        outcome.seeded.join(", ")
                                    ));
                                }
                                if !outcome.skipped_irreversible.is_empty() {
                                    lines.push(format!(
                                        "mcp: install seed skipped irreversible {}",
                                        outcome.skipped_irreversible.join(", ")
                                    ));
                                }
                                for line in lines {
                                    if ctx
                                        .events
                                        .send_blocking(WorkEvent::StatusUpdate {
                                            kind: Some("mcp".into()),
                                            message: Some(line),
                                        })
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                            Err(e) => {
                                if ctx
                                    .events
                                    .send_blocking(WorkEvent::StatusUpdate {
                                        kind: Some("mcp".into()),
                                        message: Some(format!("mcp: install seed failed: {e:?}")),
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                        }
                    }
                    // Degradation surfacing (impl review r1 P2-5, §5.5):
                    // per-origin status, projection drops, and reconcile
                    // diagnostics reach the event stream — never silent.
                    for line in carried_lines.drain(..).chain(mcp_degradations) {
                        if ctx
                            .events
                            .send_blocking(WorkEvent::StatusUpdate {
                                kind: Some("mcp".into()),
                                message: Some(line),
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    // RC2-R617: a turn whose model server is still reading a
                    // warm-up (a council member's, on a worker of its own)
                    // waits for it, and says so.
                    warm_gate::wait_idle(
                        &cfg.base_url,
                        || ctx.cancel.load(std::sync::atomic::Ordering::SeqCst),
                        |done, total| {
                            let _ = ctx.events.send_blocking(WorkEvent::ThinkingStatus {
                                text: Some(warm_line(done, total)),
                            });
                        },
                    );
                    let result = run_turn(
                        &authority,
                        transport.as_mut(),
                        &cfg.model,
                        &ctx.workdir,
                        &text,
                        &mut host_io,
                        &mut log,
                        &mut grants,
                        &mcp_tools,
                        &executors,
                        &*install_authority,
                        &mcp_roots,
                        cfg.window_tokens,
                        &turn_id,
                        &mut history,
                    );
                    // RC1-O-FIX6 (whole-slice review P2): a call that was
                    // dispatched but never delivered its result (the turn was
                    // cancelled, or its stream closed) may still have changed
                    // files; say so before the turn's end reaches the app.
                    host_io.flush_may_change();
                    // RC1-J: publish what the model has now seen, BEFORE the
                    // turn's end reaches the app.
                    if let Some(slot) = &history_slot {
                        if let Ok(mut g) = slot.lock() {
                            *g = Some(ModelHistory(history.clone()));
                        }
                    }
                    let shutdown_seen = host_io.shutdown_seen;
                    let disconnected = host_io.disconnected;
                    match result {
                        Ok(report) => {
                            // Terminal vocabulary (Codex P0-2): status +
                            // output, THEN the turn boundary.
                            if ctx
                                .events
                                .send_blocking(WorkEvent::TurnCompleted {
                                    status: RunStatus::Completed,
                                    output: Some(report.final_text.clone()),
                                    cost_usd: None,
                                    context_percent: None,
                                })
                                .is_err()
                            {
                                return;
                            }
                            if ctx.events.send_blocking(WorkEvent::TurnComplete).is_err() {
                                return;
                            }
                        }
                        Err(SeamError::Cancelled(cause)) => {
                            match cause {
                                // The user cancelled: exactly one Terminal.
                                StopCause::Cancelled => {
                                    let _ = ctx.events.send_blocking(WorkEvent::Terminal {
                                        status: RunStatus::Stopped,
                                        output: None,
                                        model: Some(cfg.model.clone()),
                                    });
                                }
                                // RC1-N: the owner's Deny ended the turn. The
                                // session goes on (the turn's history was
                                // rolled back), so this is a stopped turn
                                // boundary, not the worker's end.
                                StopCause::Denied => {
                                    if ctx
                                        .events
                                        .send_blocking(WorkEvent::TurnCompleted {
                                            status: RunStatus::Stopped,
                                            output: None,
                                            cost_usd: None,
                                            context_percent: None,
                                        })
                                        .is_err()
                                    {
                                        return;
                                    }
                                    if ctx.events.send_blocking(WorkEvent::TurnComplete).is_err() {
                                        return;
                                    }
                                    if shutdown_seen || disconnected {
                                        return;
                                    }
                                    continue;
                                }
                                // Shutdown and a vanished surface exit silently.
                                StopCause::Shutdown | StopCause::Disconnected => {}
                            }
                            return;
                        }
                        // RC1-N-FIX4 (whole-slice R3 P1): the execution log
                        // could not be written — a step may have run without
                        // its result recorded (a file op runs before its
                        // `dispatch_finalized`), and this worker's uncertainty
                        // gate is built only at startup. End the worker: the
                        // next prompt starts a new one, whose replay holds new
                        // work behind the owner's answer. Worker-fatal on
                        // purpose (`WorkEvent::Failed`).
                        Err(SeamError::Storage(_)) => {
                            let _ = ctx
                                .events
                                .send_blocking(WorkEvent::Failed(LOG_WRITE_FAILED.into()));
                            return;
                        }
                        Err(e) => {
                            // RECOVERABLE turn failure: the session survives,
                            // so this is a failed turn boundary — NOT
                            // WorkEvent::Failed, which the app treats as
                            // worker-fatal (Codex P0-2).
                            if ctx
                                .events
                                .send_blocking(WorkEvent::TurnCompleted {
                                    status: RunStatus::Failed,
                                    output: Some(plain_turn_failure(&e, cfg.window_tokens)),
                                    cost_usd: None,
                                    context_percent: None,
                                })
                                .is_err()
                            {
                                return;
                            }
                            if ctx.events.send_blocking(WorkEvent::TurnComplete).is_err() {
                                return;
                            }
                        }
                    }
                    if shutdown_seen || disconnected {
                        return;
                    }
                }
                WorkCmd::Cancel => {
                    let _ = ctx.events.send_blocking(WorkEvent::Terminal {
                        status: RunStatus::Stopped,
                        output: None,
                        model: Some(cfg.model.clone()),
                    });
                    return;
                }
                WorkCmd::Shutdown => return,
                // No blocking request is outstanding outside a turn, and this
                // engine has no terminal/sudo/secret requests in u1 — a
                // response with nothing to answer is a no-op, matching the
                // app's "no live worker → no-op" approval semantics.
                WorkCmd::Approval { .. }
                | WorkCmd::Clarify { .. }
                | WorkCmd::Steer(_)
                | WorkCmd::Attach { .. }
                | WorkCmd::TerminalReadRespond { .. }
                | WorkCmd::SudoRespond { .. }
                | WorkCmd::SecretRespond { .. } => {}
            }
        }
    }
}

/// Drops a finished session's in-memory approvals from the Allowed actions
/// registry (S6).
struct SessionGrantsGuard(String);

impl Drop for SessionGrantsGuard {
    fn drop(&mut self) {
        awili_engine_log::remembered::forget_session(&self.0);
    }
}

#[cfg(test)]
mod tests;

// Unit 4 item 5: included at the CRATE ROOT (not inside a module) so each
// scripted coordinator test's bare name is its full path — `cargo test
// refused_submission_unregisters_synchronously_and_returns_the_ticket
// -- --exact` must address it (the item-2/item-4 lesson).
#[cfg(test)]
include!("mcp_coordinator_test.rs");

// Unit 4 item 6: same crate-root include, same reason — every binding
// executor-seam test's bare name must be its full path so
// `cargo test <name> -- --exact` addresses exactly one. They live in this
// crate (not awili-seam) because they run through the REAL coordinator and
// the production `McpExecutor` adapter.
#[cfg(test)]
include!("executor_seam_test.rs");
