//! C4 — agent-aware context assembly + compaction (pure).
//!
//! The engine-owned context contract D2 integrates: a [`ContextHistory`] of
//! atomic [`ContextGroup`]s folded from the canonical [`AgentEvent`]
//! transcript, assembled deterministically against an explicit
//! [`ModelContextProfile`], with compaction as a two-step pure protocol —
//! [`assemble`] returns a [`CompactionPlan`], the CALLER produces the summary
//! with any model it likes, [`apply_summary`] returns a NEW history. Nothing
//! here does IO, knows a model name heuristic, or ever truncates raw context:
//! when even maximal compaction cannot fit, the answer is the typed
//! [`ContextError::CannotFit`], not a silent cut. The one bounded step is at
//! the tool-result boundary (RC1-N): a NEW exchange's results are fitted to
//! what the window can still hold ([`exchange_result_budget`],
//! [`fit_tool_results`]) before they enter the history, with a plain marker
//! saying so — never silently, and never anything already in the history.
//!
//! Invariants (Codex C4 design gate, 2026-08-13 — binding):
//! - A tool exchange (assistant text + calls + ALL results) is INDIVISIBLE.
//!   Ingest enforces a strict call↔result bijection at the boundary; results
//!   are stored in call order regardless of arrival order.
//! - [`ContextGroup::Pinned`] (system policy, approval records, unresolved
//!   blocking state) is never dropped and never summarized.
//! - Continuation artifacts are byte-opaque: the only reads are
//!   [`ContinuationArtifact::replayable_to`] and a byte count. They are
//!   excluded (never summarized, never migrated) when the profile's dialect
//!   cannot replay them or the provenance does not match.
//! - The ACTIVE SUFFIX — the last [`ContextGroup::User`] and everything after
//!   it — is preserved verbatim, never selectable for compaction.
//! - `assemble` is deterministic and idempotent; `apply_summary` never
//!   mutates its input.
//! - Summaries are produced from canonical material only ([`CompactionPlan::
//!   to_summarize`]): raw reasoning deltas and blob payloads never enter it,
//!   so a summary cannot import hidden reasoning (B1 rule).

use serde::{Deserialize, Serialize};

use crate::events::{AgentEvent, ToolCall, ToolResultRecord};
use crate::provenance::ContinuationArtifact;

// ============================================================================
// Errors
// ============================================================================

/// Typed context failures. Never a panic; never a silent fix-up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContextError {
    /// The event slice is not exactly one completed turn (missing terminal,
    /// events after the terminal, unexpected event).
    MalformedTurnSlice(String),
    /// The slice's terminal is `TurnFailed`/`TurnCancelled` — failed turns
    /// are not ingested; the caller decides what a crashed turn means.
    IngestOfFailedTurn(String),
    /// A tool batch violated the call↔result bijection (missing result at a
    /// boundary, text/calls arriving over a partially-resulted batch, or an
    /// open batch at the terminal).
    IncompleteExchange(String),
    /// Two calls in one batch share an `awili_call_id`.
    DuplicateCallId(String),
    /// A result arrived for a call not in the open batch.
    OrphanResult(String),
    /// A second result arrived for the same call.
    DuplicateResult(String),
    /// The history holds no `User` group — there is no active request to
    /// assemble for.
    NoActiveRequest,
    /// Even summarizing every selectable group cannot fit the budget.
    CannotFit {
        needed_tokens: u64,
        budget_tokens: u64,
    },
    /// `apply_summary` was given an id that does not exist.
    UnknownGroupId(u64),
    /// `apply_summary` was given a `Pinned` id — policy is never summarized.
    PinnedNotSummarizable(u64),
    /// `apply_summary` was given a `Continuation` id — blobs are never
    /// summarized (opaque; B1).
    NotSummarizable(u64),
    /// `apply_summary` was given an id inside the active suffix.
    ActiveSuffixSelected(u64),
    /// `apply_summary` was given an empty or duplicated selection.
    InvalidSelection(String),
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedTurnSlice(m) => write!(f, "malformed turn slice: {m}"),
            Self::IngestOfFailedTurn(m) => write!(f, "ingest of failed turn: {m}"),
            Self::IncompleteExchange(m) => write!(f, "incomplete exchange: {m}"),
            Self::DuplicateCallId(m) => write!(f, "duplicate call id: {m}"),
            Self::OrphanResult(m) => write!(f, "orphan result: {m}"),
            Self::DuplicateResult(m) => write!(f, "duplicate result: {m}"),
            Self::NoActiveRequest => write!(f, "no active request (no User group)"),
            Self::CannotFit { needed_tokens, budget_tokens } => write!(
                f,
                "cannot fit: {needed_tokens} tokens needed after maximal compaction, budget {budget_tokens}"
            ),
            Self::UnknownGroupId(id) => write!(f, "unknown group id {id}"),
            Self::PinnedNotSummarizable(id) => write!(f, "group {id} is pinned; never summarized"),
            Self::NotSummarizable(id) => write!(f, "group {id} is not summarizable"),
            Self::ActiveSuffixSelected(id) => {
                write!(f, "group {id} is in the active suffix; never summarized")
            }
            Self::InvalidSelection(m) => write!(f, "invalid selection: {m}"),
        }
    }
}

impl std::error::Error for ContextError {}

// ============================================================================
// Groups + history
// ============================================================================

/// Stable identifier for a group within one history lineage. Ids survive
/// `apply_summary` (unselected groups keep theirs; the summary gets a fresh
/// one) and never recycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GroupId(pub u64);

/// One atomic unit of context. Compaction retains or summarizes WHOLE groups.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContextGroup {
    /// Caller-pinned material: system policy, approval records, unresolved
    /// blocking state. Never dropped, never summarized. The engine does not
    /// know authz types; pinning is the mechanism, the caller owns policy.
    Pinned { id: GroupId, text: String },
    /// One user input.
    User { id: GroupId, text: String },
    /// One assistant round: text + tool calls + ALL their results, results in
    /// call order. Indivisible.
    Exchange {
        id: GroupId,
        assistant_text: String,
        calls: Vec<ToolCall>,
        results: Vec<ToolResultRecord>,
    },
    /// A prior compaction summary — canonical, cross-model text.
    Summary {
        id: GroupId,
        text: String,
        replaces: Vec<GroupId>,
    },
    /// An opaque provider-native continuation artifact. Payload never read.
    Continuation {
        id: GroupId,
        artifact: ContinuationArtifact,
    },
}

impl ContextGroup {
    pub fn id(&self) -> GroupId {
        match self {
            Self::Pinned { id, .. }
            | Self::User { id, .. }
            | Self::Exchange { id, .. }
            | Self::Summary { id, .. }
            | Self::Continuation { id, .. } => *id,
        }
    }

    /// Whether compaction may select this group (Pinned and Continuation are
    /// excluded at the type level — the selection loop iterates these only).
    fn summarizable(&self) -> bool {
        matches!(
            self,
            Self::User { .. } | Self::Exchange { .. } | Self::Summary { .. }
        )
    }
}

/// Append-only ordered history of context groups with a monotonic id source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextHistory {
    groups: Vec<ContextGroup>,
    next_id: u64,
    /// The CURRENT session preamble's group id (unit 5 ship-fix P0-3):
    /// standing directives + recall, replaced per turn — never allowed to
    /// accumulate as stale pins. `default` so pre-fix checkpoints restore.
    #[serde(default)]
    preamble_id: Option<GroupId>,
    /// RC1-N-FIX2/FIX3: this history's checkpoint — the newest
    /// execution-log turn whose outcome it holds, every call that turn
    /// finalized included (set by the seam when a turn's history is kept, and
    /// by a worker after it recovers earlier turns from the log). Crash
    /// recovery reads only turns logged after it. `default` so histories saved
    /// before it restore (recovery then lists every finalized call they lack).
    /// Stored as `log_checkpoint`: RC1-N-FIX2 wrote a `log_turn` key without
    /// placing every finalized call, so such a stamp is not exact and is
    /// ignored on load (FIX3 pre-review P1) — that history counts as unstamped.
    #[serde(
        default,
        rename = "log_checkpoint",
        skip_serializing_if = "Option::is_none"
    )]
    log_turn: Option<String>,
    /// RC2-GEMMA1: this turn's transient memory context (the recalled
    /// notes), set by the caller before a turn and taken by the seam, which
    /// opens the turn's user message with it for the turn's requests only.
    /// Never stored.
    #[serde(skip)]
    turn_context: Option<String>,
}

impl ContextHistory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn groups(&self) -> &[ContextGroup] {
        &self.groups
    }

    /// RC1-N-FIX2: the newest execution-log turn this history covers.
    pub fn log_turn(&self) -> Option<&str> {
        self.log_turn.as_deref()
    }

    /// RC1-N-FIX2: record that this history holds the outcome of `turn_id`
    /// (and of every log turn before it).
    pub fn set_log_turn(&mut self, turn_id: impl Into<String>) {
        self.log_turn = Some(turn_id.into());
    }

    pub fn get(&self, id: GroupId) -> Option<&ContextGroup> {
        self.groups.iter().find(|g| g.id() == id)
    }

    fn alloc(&mut self) -> GroupId {
        let id = GroupId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Replace the session preamble (ship-fix P0-3): the ONE pinned group
    /// that always sits FIRST — the caller supplies the CURRENT standing
    /// directives/recall each turn, and the previous turn's preamble is
    /// removed rather than accumulating. `None` clears it.
    pub fn set_preamble(&mut self, text: Option<String>) {
        if let Some(id) = self.preamble_id.take() {
            self.groups.retain(|g| g.id() != id);
        }
        if let Some(text) = text {
            let id = self.alloc();
            self.groups.insert(0, ContextGroup::Pinned { id, text });
            self.preamble_id = Some(id);
        }
    }

    pub fn push_pinned(&mut self, text: impl Into<String>) -> GroupId {
        let id = self.alloc();
        self.groups.push(ContextGroup::Pinned {
            id,
            text: text.into(),
        });
        id
    }

    pub fn push_user(&mut self, text: impl Into<String>) -> GroupId {
        let id = self.alloc();
        self.groups.push(ContextGroup::User {
            id,
            text: text.into(),
        });
        id
    }

    /// Push one completed tool exchange directly (the mid-turn path: an
    /// engine loop assembles context PER continuation request, before the
    /// turn's event slice is complete). Enforces the same call↔result
    /// bijection as ingest (P0-2): results must match calls 1:1 IN CALL
    /// ORDER, no duplicates. A zero-call exchange carries final assistant
    /// text (calls and results both empty).
    pub fn push_exchange(
        &mut self,
        assistant_text: impl Into<String>,
        calls: Vec<ToolCall>,
        results: Vec<ToolResultRecord>,
    ) -> Result<GroupId, ContextError> {
        if calls.len() != results.len() {
            return Err(ContextError::IncompleteExchange(format!(
                "{} calls, {} results",
                calls.len(),
                results.len()
            )));
        }
        let mut seen = std::collections::HashSet::new();
        for c in &calls {
            if !seen.insert(c.awili_call_id.0.clone()) {
                return Err(ContextError::DuplicateCallId(format!(
                    "{:?} appears twice in one exchange",
                    c.awili_call_id
                )));
            }
        }
        for (c, r) in calls.iter().zip(&results) {
            if c.awili_call_id != r.awili_call_id {
                return Err(ContextError::OrphanResult(format!(
                    "result {:?} does not match call {:?} at its position",
                    r.awili_call_id, c.awili_call_id
                )));
            }
        }
        let id = self.alloc();
        self.groups.push(ContextGroup::Exchange {
            id,
            assistant_text: assistant_text.into(),
            calls,
            results,
        });
        Ok(id)
    }

    /// RC2-GEMMA1: set (or clear) the next turn's transient memory context.
    pub fn set_turn_context(&mut self, context: Option<String>) {
        self.turn_context = context;
    }

    /// RC2-GEMMA1: take the turn's transient memory context (the seam, once
    /// per turn).
    pub fn take_turn_context(&mut self) -> Option<String> {
        self.turn_context.take()
    }

    /// RC2-GEMMA1: replace the text of the `User` group `id`. The seam uses
    /// it to drop a turn's transient memory context from the user message
    /// once the turn is over. Returns `false` (nothing changed) when `id` is
    /// not a `User` group of this history.
    pub fn set_user_text(&mut self, id: GroupId, text: impl Into<String>) -> bool {
        match self.groups.iter_mut().find(|g| g.id() == id) {
            Some(ContextGroup::User { text: t, .. }) => {
                *t = text.into();
                true
            }
            _ => false,
        }
    }

    /// RC1-N: shrink the tool results of the newest exchange of the active
    /// turn (after the last user input) that can still shrink, to half their
    /// size, refitted with [`fit_tool_results`] — used when a model server
    /// refused a request as longer than its context although Awili's count
    /// said it fit. Never below a short marker per result (a cut result
    /// always says so). Returns whether anything shrank; `false` means only
    /// markers are left in this turn. Group ids are unchanged.
    pub fn shrink_latest_results(&mut self, counter: &dyn TokenCounter) -> bool {
        let last_user = self
            .groups
            .iter()
            .rposition(|g| matches!(g, ContextGroup::User { .. }))
            .unwrap_or(0);
        for g in self.groups[last_user..].iter_mut().rev() {
            let ContextGroup::Exchange { calls, results, .. } = g else {
                continue;
            };
            if calls.is_empty() {
                continue;
            }
            // Pre-review P2 (passes 6 and 11): each result shrinks by half,
            // down to its OWN short marker; a result already that small
            // stays as it is.
            let mut progress = false;
            let shrunk: Vec<ToolResultRecord> = results
                .iter()
                .map(|r| {
                    let cost = result_tokens(&r.content, counter);
                    let (body, earlier) = split_cut_marker(&r.content);
                    let total = earlier.unwrap_or_else(|| body.chars().count());
                    let own = result_tokens(&cut_marker_short(total), counter);
                    let target = (cost / 2).max(own);
                    if target >= cost {
                        return r.clone();
                    }
                    let (mut one, _) = fit_tool_results(vec![r.clone()], target, counter);
                    let one = one.remove(0);
                    if result_tokens(&one.content, counter) < cost {
                        progress = true;
                        one
                    } else {
                        r.clone()
                    }
                })
                .collect();
            if progress {
                *results = shrunk;
                return true;
            }
        }
        false
    }

    pub fn push_continuation(&mut self, artifact: ContinuationArtifact) -> GroupId {
        let id = self.alloc();
        self.groups
            .push(ContextGroup::Continuation { id, artifact });
        id
    }

    /// Fold ONE completed turn's canonical event slice into groups.
    ///
    /// Contract (P0-1): the slice is exactly one turn — its LAST element must
    /// be `TurnCompleted`; a failed/cancelled terminal, a missing terminal,
    /// events after the terminal, or `ToolCallInvalid` anywhere are typed
    /// errors. Ingest is ATOMIC: on any error the history is unchanged.
    ///
    /// Segmentation (P0-1): text accumulates; `ToolCallReady` opens/extends
    /// the batch; the exchange closes exactly when every call has exactly one
    /// result (results stored in CALL order — P0-2); continuation text after
    /// a closed batch starts the next exchange; terminal text with no calls
    /// becomes a zero-call exchange. `ContinuationBlobCaptured` arrives at
    /// segment boundaries (possibly with the batch still awaiting results);
    /// its group is emitted right AFTER the exchange it trails.
    pub fn ingest_turn_events(
        &mut self,
        events: &[AgentEvent],
    ) -> Result<Vec<GroupId>, ContextError> {
        // Proto-groups first; ids allocated only on success (atomic ingest).
        enum Proto {
            Exchange {
                assistant_text: String,
                calls: Vec<ToolCall>,
                results: Vec<ToolResultRecord>,
            },
            Continuation(ContinuationArtifact),
        }

        struct Batch {
            calls: Vec<ToolCall>,
            results: Vec<Option<ToolResultRecord>>,
        }
        impl Batch {
            fn filled(&self) -> usize {
                self.results.iter().filter(|r| r.is_some()).count()
            }
            fn complete(&self) -> bool {
                self.filled() == self.calls.len()
            }
        }

        let mut out: Vec<Proto> = Vec::new();
        let mut text = String::new();
        let mut batch: Option<Batch> = None;
        let mut pending_blobs: Vec<ContinuationArtifact> = Vec::new();
        let mut terminal_seen = false;

        // Close the current exchange into `out`, flushing trailing blobs.
        fn close_exchange(
            out: &mut Vec<Proto>,
            text: &mut String,
            batch: Batch,
            pending_blobs: &mut Vec<ContinuationArtifact>,
        ) {
            out.push(Proto::Exchange {
                assistant_text: std::mem::take(text),
                calls: batch.calls,
                results: batch
                    .results
                    .into_iter()
                    .map(|r| r.expect("complete"))
                    .collect(),
            });
            for b in pending_blobs.drain(..) {
                out.push(Proto::Continuation(b));
            }
        }

        for (i, ev) in events.iter().enumerate() {
            if terminal_seen {
                return Err(ContextError::MalformedTurnSlice(format!(
                    "event at index {i} follows the terminal"
                )));
            }
            match ev {
                AgentEvent::TurnStarted
                | AgentEvent::ReasoningDelta(_)
                | AgentEvent::UsageReported(_)
                | AgentEvent::InputRejected { .. } => {}
                AgentEvent::TextDelta(t) => match &batch {
                    None => text.push_str(t),
                    Some(b) if b.filled() == 0 => {
                        // Same assistant segment: content deltas may interleave
                        // with tool-call assembly before any result exists.
                        text.push_str(t);
                    }
                    Some(b) if b.complete() => {
                        // Defensive: eager close at the last result makes this
                        // unreachable, but keep the fold total.
                        let b = batch.take().expect("checked");
                        close_exchange(&mut out, &mut text, b, &mut pending_blobs);
                        text.push_str(t);
                    }
                    Some(b) => {
                        return Err(ContextError::IncompleteExchange(format!(
                            "text arrived with {}/{} results filled",
                            b.filled(),
                            b.calls.len()
                        )));
                    }
                },
                AgentEvent::ToolCallReady(c) => match &mut batch {
                    None => {
                        batch = Some(Batch {
                            calls: vec![c.clone()],
                            results: vec![None],
                        });
                    }
                    Some(b) if b.filled() == 0 => {
                        if b.calls.iter().any(|x| x.awili_call_id == c.awili_call_id) {
                            return Err(ContextError::DuplicateCallId(format!(
                                "{:?} appears twice in one batch",
                                c.awili_call_id
                            )));
                        }
                        b.calls.push(c.clone());
                        b.results.push(None);
                    }
                    Some(b) => {
                        return Err(ContextError::IncompleteExchange(format!(
                            "new call arrived with {}/{} results filled",
                            b.filled(),
                            b.calls.len()
                        )));
                    }
                },
                AgentEvent::ToolResultIngested(r) => {
                    let Some(b) = &mut batch else {
                        return Err(ContextError::OrphanResult(format!(
                            "result for {:?} with no open batch",
                            r.awili_call_id
                        )));
                    };
                    let Some(idx) = b
                        .calls
                        .iter()
                        .position(|c| c.awili_call_id == r.awili_call_id)
                    else {
                        return Err(ContextError::OrphanResult(format!(
                            "result for {:?} not in the open batch",
                            r.awili_call_id
                        )));
                    };
                    if b.results[idx].is_some() {
                        return Err(ContextError::DuplicateResult(format!(
                            "second result for {:?}",
                            r.awili_call_id
                        )));
                    }
                    b.results[idx] = Some(r.clone());
                    if b.complete() {
                        let b = batch.take().expect("checked");
                        close_exchange(&mut out, &mut text, b, &mut pending_blobs);
                    }
                }
                AgentEvent::ContinuationBlobCaptured(a) => {
                    if batch.is_some() || !text.is_empty() {
                        pending_blobs.push(a.clone());
                    } else {
                        out.push(Proto::Continuation(a.clone()));
                    }
                }
                AgentEvent::ToolCallInvalid { reason, .. } => {
                    return Err(ContextError::MalformedTurnSlice(format!(
                        "ToolCallInvalid in slice: {reason}"
                    )));
                }
                AgentEvent::TurnCompleted { .. } => {
                    if let Some(b) = &batch {
                        return Err(ContextError::IncompleteExchange(format!(
                            "terminal with open batch ({}/{} results)",
                            b.filled(),
                            b.calls.len()
                        )));
                    }
                    if !text.is_empty() {
                        out.push(Proto::Exchange {
                            assistant_text: std::mem::take(&mut text),
                            calls: vec![],
                            results: vec![],
                        });
                    }
                    for b in pending_blobs.drain(..) {
                        out.push(Proto::Continuation(b));
                    }
                    terminal_seen = true;
                }
                AgentEvent::TurnCancelled { .. } => {
                    return Err(ContextError::IngestOfFailedTurn("TurnCancelled".into()));
                }
                AgentEvent::TurnFailed(e) => {
                    return Err(ContextError::IngestOfFailedTurn(format!(
                        "TurnFailed: {e:?}"
                    )));
                }
            }
        }
        if !terminal_seen {
            return Err(ContextError::MalformedTurnSlice(
                "slice has no TurnCompleted terminal".into(),
            ));
        }

        // Commit: allocate ids and append (atomicity — nothing above touched self).
        let mut ids = Vec::with_capacity(out.len());
        for proto in out {
            let id = self.alloc();
            ids.push(id);
            self.groups.push(match proto {
                Proto::Exchange {
                    assistant_text,
                    calls,
                    results,
                } => ContextGroup::Exchange {
                    id,
                    assistant_text,
                    calls,
                    results,
                },
                Proto::Continuation(artifact) => ContextGroup::Continuation { id, artifact },
            });
        }
        Ok(ids)
    }
}

// ============================================================================
// Profile + counting
// ============================================================================

/// Explicit, deterministic model context profile. No model-name heuristics:
/// every number is caller-supplied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelContextProfile {
    /// Wire-dialect family — gates continuation replay eligibility together
    /// with `model` ([`Provenance::replayable_to`]).
    pub provider: String,
    pub model: String,
    pub window_tokens: u64,
    pub reserved_output_tokens: u64,
    pub safety_margin_tokens: u64,
    /// Deterministic allowance a future summary is assumed to occupy when
    /// planning compaction (explicit input, not a heuristic).
    pub assumed_summary_tokens: u64,
    /// Whether the target wire dialect can replay continuation artifacts at
    /// all (P1-3). `false` (e.g. Chat Completions): eligible blobs are
    /// excluded from assembly AND counting — preserved in history, recorded
    /// as excluded. `true`: the provenance gate applies per artifact.
    pub replays_continuations: bool,
    /// Fixed structural request overhead (framing, tool schemas, fixed
    /// fields) — explicit conservative constant (P1-4).
    pub fixed_request_overhead_tokens: u64,
    /// Per-encoded-message structural overhead (role, framing) — explicit
    /// conservative constant (P1-4).
    pub per_message_overhead_tokens: u64,
}

impl ModelContextProfile {
    /// Tokens available for the assembled input.
    pub fn input_budget(&self) -> u64 {
        self.window_tokens
            .saturating_sub(self.reserved_output_tokens)
            .saturating_sub(self.safety_margin_tokens)
    }
}

/// Injected deterministic token counting. Both methods are required — the
/// caller owns the bound and its conservatism; nothing here claims a hidden
/// default is safe.
pub trait TokenCounter {
    fn count_text(&self, text: &str) -> u64;
    fn count_bytes(&self, bytes: &[u8]) -> u64;
}

/// A simple explicit-ratio counter. The ratios are the caller's claim; pick
/// them conservatively for the target tokenizer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeuristicCounter {
    pub chars_per_token: f64,
    pub bytes_per_token: f64,
}

impl TokenCounter for HeuristicCounter {
    fn count_text(&self, text: &str) -> u64 {
        (text.chars().count() as f64 / self.chars_per_token).ceil() as u64
    }
    fn count_bytes(&self, bytes: &[u8]) -> u64 {
        (bytes.len() as f64 / self.bytes_per_token).ceil() as u64
    }
}

// ============================================================================
// Assembly
// ============================================================================

/// Why a continuation group was excluded from an assembly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExclusionReason {
    /// The profile's dialect cannot replay continuation artifacts (P1-3).
    DialectCannotReplay,
    /// The artifact's provenance does not match the profile's provider/model.
    ProvenanceMismatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssembledContext {
    /// Included groups in history order (clones; history is untouched).
    pub groups: Vec<ContextGroup>,
    /// Deterministic token accounting for the included set (content +
    /// structural overheads per the profile).
    pub tokens: u64,
    /// Continuation groups excluded by dialect/provenance, with reasons.
    /// They remain in the history — exclusion is per-assembly.
    pub excluded_continuations: Vec<(GroupId, ExclusionReason)>,
}

/// A deterministic compaction plan: summarize these WHOLE groups (oldest
/// first, possibly non-contiguous where pinned/continuation groups
/// interleave — those keep their places). The caller produces the summary
/// text with any model, then calls [`apply_summary`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionPlan {
    /// Selected ids in history order.
    pub summarize_ids: Vec<GroupId>,
    /// The canonical material to summarize (clones, history order). Contains
    /// only User/Exchange/Summary groups — never pinned text, never blob
    /// payloads (P2: a summary cannot import hidden reasoning).
    pub to_summarize: Vec<ContextGroup>,
    /// Projected post-compaction assembly cost (retained + assumed summary),
    /// for the caller's bookkeeping.
    pub projected_tokens: u64,
    /// The excluded continuations of the underlying assembly pass.
    pub excluded_continuations: Vec<(GroupId, ExclusionReason)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AssembleOutcome {
    Fits(AssembledContext),
    NeedsCompaction(CompactionPlan),
}

/// How many wire messages a group encodes to (structural overhead accounting,
/// P1-4).
fn messages_of(group: &ContextGroup) -> u64 {
    match group {
        ContextGroup::Pinned { .. } | ContextGroup::User { .. } | ContextGroup::Summary { .. } => 1,
        ContextGroup::Exchange { results, .. } => 1 + results.len() as u64,
        ContextGroup::Continuation { .. } => 1,
    }
}

/// Content tokens of a group (structural overhead excluded — added by the
/// caller via the profile constants).
fn content_tokens(group: &ContextGroup, counter: &dyn TokenCounter, send_images: bool) -> u64 {
    match group {
        ContextGroup::Pinned { text, .. } | ContextGroup::Summary { text, .. } => {
            counter.count_text(text)
        }
        ContextGroup::User { text, .. } => {
            let content = awili_image_content::split(text);
            counter
                .count_text(&content.text)
                .saturating_add(if send_images {
                    content.images.len() as u64 * awili_image_content::IMAGE_TOKEN_RESERVE
                } else {
                    0
                })
        }
        ContextGroup::Exchange {
            assistant_text,
            calls,
            results,
            ..
        } => {
            let mut t = counter.count_text(assistant_text);
            for c in calls {
                t += counter.count_text(&c.name) + counter.count_text(&c.arguments);
            }
            for r in results {
                t += counter.count_text(&r.content);
            }
            t
        }
        ContextGroup::Continuation { artifact, .. } => counter.count_bytes(&artifact.payload),
    }
}

fn group_cost(
    group: &ContextGroup,
    p: &ModelContextProfile,
    c: &dyn TokenCounter,
    send_images: bool,
) -> u64 {
    content_tokens(group, c, send_images) + messages_of(group) * p.per_message_overhead_tokens
}

/// RC1-N (usability round 4, M3: the owner's 63k-character memory
/// `bootstrap` result overflowed a 32K window and the turn failed with
/// "cannot fit"): how many content tokens the results of a NEW exchange
/// (`assistant_text` + `calls`, `n_results` results) may take so the next
/// request still fits `profile` — counting everything that can never be
/// compacted (pinned groups, the active suffix, non-replayable
/// continuations are excluded anyway) at full cost, and the older
/// summarizable groups at the assumed summary size.
pub fn exchange_result_budget(
    history: &ContextHistory,
    profile: &ModelContextProfile,
    counter: &dyn TokenCounter,
    assistant_text: &str,
    calls: &[ToolCall],
    n_results: usize,
) -> u64 {
    exchange_room(history, profile, counter, assistant_text, calls, n_results).unwrap_or(0)
}

/// RC1-N (pre-review P1): [`exchange_result_budget`], or `None` when even
/// the exchange's calls and empty results cannot fit — then the batch must
/// not run (nothing it produced could reach the model).
pub fn exchange_room(
    history: &ContextHistory,
    profile: &ModelContextProfile,
    counter: &dyn TokenCounter,
    assistant_text: &str,
    calls: &[ToolCall],
    n_results: usize,
) -> Option<u64> {
    let groups = history.groups();
    let active_from = groups
        .iter()
        .rposition(|g| matches!(g, ContextGroup::User { .. }))
        .unwrap_or(0);
    let mut fixed = profile.fixed_request_overhead_tokens;
    let mut any_summarizable = false;
    for (i, g) in groups.iter().enumerate() {
        if let ContextGroup::Continuation { artifact, .. } = g {
            if !profile.replays_continuations
                || !artifact.replayable_to(&profile.provider, &profile.model)
            {
                continue;
            }
        }
        if i < active_from && g.summarizable() {
            any_summarizable = true;
        } else {
            fixed += group_cost(g, profile, counter, i == active_from);
        }
    }
    if any_summarizable {
        fixed += profile.assumed_summary_tokens + profile.per_message_overhead_tokens;
    }
    let probe = ContextGroup::Exchange {
        id: GroupId(u64::MAX),
        assistant_text: assistant_text.to_string(),
        calls: calls.to_vec(),
        results: Vec::new(),
    };
    fixed += group_cost(&probe, profile, counter, false)
        + n_results as u64 * profile.per_message_overhead_tokens;
    profile.input_budget().checked_sub(fixed)
}

/// The plain marker a cut tool result ends with (RC1-N).
pub fn cut_marker(kept_chars: usize, total_chars: usize) -> String {
    format!(
        "\n[Cut by Awili to fit the model's context: {kept_chars} of {total_chars} characters kept. \
         Ask for a narrower result if you need the rest.]"
    )
}

/// RC1-N: the short marker of a result whose share cannot hold even
/// [`cut_marker`].
pub fn cut_marker_short(total_chars: usize) -> String {
    format!("\n[Cut by Awili: {total_chars} characters did not fit the model's context.]")
}

/// RC1-N: a result's text without an earlier cut marker, and the original
/// size that marker states (`None` when the result was never cut).
fn split_cut_marker(content: &str) -> (&str, Option<usize>) {
    let Some(at) = content.rfind("\n[Cut by Awili") else {
        return (content, None);
    };
    let marker = &content[at..];
    // Only Awili's own marker: the last line, ending the result.
    if marker[1..].contains('\n') || !marker.ends_with(']') {
        return (content, None);
    }
    let number = |after: &str| -> Option<usize> {
        let rest = marker.split(after).nth(1)?;
        rest.split_whitespace().next()?.parse().ok()
    };
    let total = number(" of ").or_else(|| number("Cut by Awili: "));
    match total {
        Some(n) => (&content[..at], Some(n)),
        None => (content, None),
    }
}

/// RC1-N (pre-review P2): the room one cut result needs at least — its
/// short marker — so a batch is only run when every result can at least
/// say it was cut.
pub fn marker_floor(counter: &dyn TokenCounter) -> u64 {
    result_tokens(&cut_marker_short(usize::MAX), counter)
}

/// RC1-N: whether a batch may run — the exchange's calls fit and leave
/// every result at least [`marker_floor`].
pub fn batch_has_room(
    history: &ContextHistory,
    profile: &ModelContextProfile,
    counter: &dyn TokenCounter,
    assistant_text: &str,
    calls: &[ToolCall],
) -> bool {
    exchange_room(
        history,
        profile,
        counter,
        assistant_text,
        calls,
        calls.len(),
    )
    .is_some_and(|room| room >= calls.len() as u64 * marker_floor(counter))
}

/// RC1-N: what a tool result costs when results are fitted — the counter's
/// count, but never less than one token per two UTF-8 bytes. The heuristic
/// counter (three characters a token) under-counts dense text such as
/// base64, minified JSON or CJK, and a model's real tokenizer is not known
/// here; the fitted results are measured conservatively instead
/// (pre-review P1).
pub fn result_tokens(text: &str, counter: &dyn TokenCounter) -> u64 {
    counter.count_text(text).max(text.len().div_ceil(2) as u64)
}

/// The longest char-boundary prefix of `text` whose [`result_tokens`] plus
/// `extra` stays within `cap`, as a char count (0 when none fits).
fn longest_prefix(text: &str, cap: u64, extra: u64, counter: &dyn TokenCounter) -> (usize, usize) {
    let bounds: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let fits = |k: usize| result_tokens(&text[..bounds[k]], counter) + extra <= cap;
    let (mut lo, mut hi) = (0usize, bounds.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    (lo, bounds[lo])
}

/// RC1-N: fit a new exchange's tool results into `budget_tokens` (from
/// [`exchange_result_budget`]), measured with [`result_tokens`]. Results that
/// fit are untouched. Otherwise small results stay whole and the rest share
/// what is left equally; each cut result keeps a prefix (on a char boundary)
/// and ends with [`cut_marker`], the marker counted inside its share. When
/// The fitted results always fit the budget: a share too small for the
/// marker gets [`cut_marker_short`] alone, or an empty result when even
/// that does not fit (a result is never dropped). Returns the fitted results
/// and how many were cut.
pub fn fit_tool_results(
    results: Vec<ToolResultRecord>,
    budget_tokens: u64,
    counter: &dyn TokenCounter,
) -> (Vec<ToolResultRecord>, usize) {
    let costs: Vec<u64> = results
        .iter()
        .map(|r| result_tokens(&r.content, counter))
        .collect();
    if costs.iter().sum::<u64>() <= budget_tokens {
        return (results, 0);
    }
    // Water-filling: the smallest results keep everything while an equal
    // share of what is left still covers them.
    let mut order: Vec<usize> = (0..results.len()).collect();
    order.sort_by_key(|&i| costs[i]);
    let mut left = budget_tokens;
    let mut caps: Vec<Option<u64>> = vec![None; results.len()];
    for (k, &i) in order.iter().enumerate() {
        let share = left / (order.len() - k) as u64;
        if costs[i] <= share {
            left -= costs[i];
        } else {
            for &j in &order[k..] {
                caps[j] = Some(left / (order.len() - k) as u64);
            }
            break;
        }
    }
    let mut cut = 0;
    let fitted = results
        .into_iter()
        .zip(caps)
        .map(|(r, cap)| {
            let Some(cap) = cap else {
                return r;
            };
            cut += 1;
            // Pre-review P3 (pass 9): a result cut before (a resend) keeps
            // its original size in the new marker, and its old marker is
            // not part of what is kept.
            let (body, earlier) = split_cut_marker(&r.content);
            let total = earlier.unwrap_or_else(|| body.chars().count());
            let marker_tokens = result_tokens(&cut_marker(total, total), counter);
            if marker_tokens > cap {
                // Pre-review P1: never more than the share — the short
                // marker when it fits, else nothing (the result message
                // itself is already counted).
                let short = cut_marker_short(total);
                let content = if result_tokens(&short, counter) <= cap {
                    short
                } else {
                    String::new()
                };
                return ToolResultRecord { content, ..r };
            }
            let (keep, end) = longest_prefix(body, cap, marker_tokens, counter);
            ToolResultRecord {
                content: format!("{}{}", &body[..end], cut_marker(keep, total)),
                ..r
            }
        })
        .collect();
    (fitted, cut)
}

/// RC1-N (pre-review P1): a compaction summary cut to the room the budget
/// assumed for it (`assumed_summary_tokens`), so a turn whose exchange was
/// fitted still assembles after compaction. The summarizer is asked for at
/// most that many model tokens, but the counter may count its text higher.
pub fn fit_summary(text: String, allowance_tokens: u64, counter: &dyn TokenCounter) -> String {
    if counter.count_text(&text) <= allowance_tokens {
        return text;
    }
    let ellipsis = counter.count_text("\u{2026}");
    let bounds: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let fits = |k: usize| counter.count_text(&text[..bounds[k]]) + ellipsis <= allowance_tokens;
    let (mut lo, mut hi) = (0usize, bounds.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    format!("{}\u{2026}", &text[..bounds[lo]])
}

/// Assemble the history for one request against `profile`. Pure and
/// deterministic: identical inputs yield identical outcomes.
pub fn assemble(
    history: &ContextHistory,
    profile: &ModelContextProfile,
    counter: &dyn TokenCounter,
) -> Result<AssembleOutcome, ContextError> {
    let groups = history.groups();

    // P1-1: the active request boundary must exist.
    let last_user_idx = groups
        .iter()
        .rposition(|g| matches!(g, ContextGroup::User { .. }))
        .ok_or(ContextError::NoActiveRequest)?;

    // Continuation gating (P1-3 + B1 provenance rule).
    let mut included: Vec<&ContextGroup> = Vec::with_capacity(groups.len());
    let mut excluded: Vec<(GroupId, ExclusionReason)> = Vec::new();
    for g in groups {
        if let ContextGroup::Continuation { id, artifact } = g {
            if !profile.replays_continuations {
                excluded.push((*id, ExclusionReason::DialectCannotReplay));
                continue;
            }
            if !artifact.replayable_to(&profile.provider, &profile.model) {
                excluded.push((*id, ExclusionReason::ProvenanceMismatch));
                continue;
            }
        }
        included.push(g);
    }

    let budget = profile.input_budget();
    let total: u64 = profile.fixed_request_overhead_tokens
        + included
            .iter()
            .map(|g| group_cost(g, profile, counter, g.id() == groups[last_user_idx].id()))
            .sum::<u64>();

    if total <= budget {
        return Ok(AssembleOutcome::Fits(AssembledContext {
            groups: included.into_iter().cloned().collect(),
            tokens: total,
            excluded_continuations: excluded,
        }));
    }

    // Over budget: select the OLDEST summarizable groups strictly before the
    // active suffix, until retained + the assumed summary fits.
    let summary_allowance = profile.assumed_summary_tokens + profile.per_message_overhead_tokens;
    let mut savings: u64 = 0;
    let mut selected: Vec<GroupId> = Vec::new();
    let mut material: Vec<ContextGroup> = Vec::new();
    for g in groups.iter().take(last_user_idx) {
        if !g.summarizable() {
            continue;
        }
        savings += group_cost(g, profile, counter, false);
        selected.push(g.id());
        material.push(g.clone());
        if total - savings + summary_allowance <= budget {
            return Ok(AssembleOutcome::NeedsCompaction(CompactionPlan {
                summarize_ids: selected,
                to_summarize: material,
                projected_tokens: total - savings + summary_allowance,
                excluded_continuations: excluded,
            }));
        }
    }
    Err(ContextError::CannotFit {
        needed_tokens: total - savings
            + if selected.is_empty() {
                0
            } else {
                summary_allowance
            },
        budget_tokens: budget,
    })
}

/// Replace the selected groups with ONE canonical summary, returning a NEW
/// history (the input is untouched — gate 7). The summary lands at the first
/// selected position; unselected interleaved groups (pinned, blobs) keep
/// their relative order (P1-2).
pub fn apply_summary(
    history: &ContextHistory,
    ids: &[GroupId],
    summary_text: impl Into<String>,
) -> Result<ContextHistory, ContextError> {
    if ids.is_empty() {
        return Err(ContextError::InvalidSelection("empty selection".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id.0) {
            return Err(ContextError::InvalidSelection(format!(
                "id {} selected twice",
                id.0
            )));
        }
    }

    let groups = history.groups();
    let last_user_idx = groups
        .iter()
        .rposition(|g| matches!(g, ContextGroup::User { .. }))
        .ok_or(ContextError::NoActiveRequest)?;

    for id in ids {
        let Some(pos) = groups.iter().position(|g| g.id() == *id) else {
            return Err(ContextError::UnknownGroupId(id.0));
        };
        match &groups[pos] {
            ContextGroup::Pinned { .. } => {
                return Err(ContextError::PinnedNotSummarizable(id.0));
            }
            ContextGroup::Continuation { .. } => {
                return Err(ContextError::NotSummarizable(id.0));
            }
            _ => {}
        }
        if pos >= last_user_idx {
            return Err(ContextError::ActiveSuffixSelected(id.0));
        }
    }

    let selected: std::collections::HashSet<u64> = ids.iter().map(|i| i.0).collect();
    let first_selected_pos = groups
        .iter()
        .position(|g| selected.contains(&g.id().0))
        .expect("ids verified present");

    let summary_text: String = summary_text.into();
    let mut out = ContextHistory {
        groups: Vec::with_capacity(groups.len()),
        next_id: history.next_id,
        // The preamble group (if any) is carried through the replacement
        // loop below like every other retained group; its id survives.
        preamble_id: history.preamble_id,
        // RC1-N-FIX2: compaction changes no turn's outcome; the mark stays.
        log_turn: history.log_turn.clone(),
        // RC2-GEMMA1: a compaction inside a turn keeps the turn's context.
        turn_context: history.turn_context.clone(),
    };
    let summary_id = out.alloc();
    // Record replacements in HISTORY order (deterministic regardless of the
    // order ids were passed in).
    let replaces: Vec<GroupId> = groups
        .iter()
        .filter(|g| selected.contains(&g.id().0))
        .map(|g| g.id())
        .collect();
    for (i, g) in groups.iter().enumerate() {
        if i == first_selected_pos {
            out.groups.push(ContextGroup::Summary {
                id: summary_id,
                text: summary_text.clone(),
                replaces: replaces.clone(),
            });
            continue;
        }
        if selected.contains(&g.id().0) {
            continue;
        }
        out.groups.push(g.clone());
    }
    Ok(out)
}

#[cfg(test)]
mod rc1n_fit_tests {
    use super::*;
    use crate::events::AwiliCallId;

    fn counter() -> HeuristicCounter {
        HeuristicCounter {
            chars_per_token: 3.0,
            bytes_per_token: 3.0,
        }
    }

    fn result(id: &str, len: usize) -> ToolResultRecord {
        ToolResultRecord {
            awili_call_id: AwiliCallId(id.into()),
            content: "x".repeat(len),
            is_error: false,
        }
    }

    fn profile(window: u64) -> ModelContextProfile {
        ModelContextProfile {
            provider: "chat-completions".into(),
            model: "m".into(),
            window_tokens: window,
            reserved_output_tokens: 2048,
            safety_margin_tokens: 512,
            assumed_summary_tokens: 1024,
            replays_continuations: false,
            fixed_request_overhead_tokens: 64,
            per_message_overhead_tokens: 8,
        }
    }

    /// RC1-N: results that fit are untouched; an oversized one is cut to its
    /// share with the marker; small results beside it stay whole; whenever
    /// every share can hold its marker, the fitted set fits the budget —
    /// measured conservatively ([`result_tokens`]), dense text included.
    #[test]
    fn rc1n_results_are_fitted_to_the_budget() {
        let c = counter();
        let cost = |rs: &[ToolResultRecord]| -> u64 {
            rs.iter().map(|r| result_tokens(&r.content, &c)).sum()
        };
        let (same, cut) = fit_tool_results(vec![result("a", 300)], 1000, &c);
        assert_eq!((same[0].content.len(), cut), (300, 0));
        let (fitted, cut) =
            fit_tool_results(vec![result("small", 300), result("big", 60_000)], 2_000, &c);
        assert_eq!(cut, 1);
        assert_eq!(fitted[0].content.len(), 300, "the small result stays whole");
        assert!(fitted[1]
            .content
            .contains("[Cut by Awili to fit the model's context:"));
        assert!(fitted[1].content.contains("of 60000 characters kept"));
        assert!(cost(&fitted) <= 2_000, "{}", cost(&fitted));
        // Many results, many budgets, dense text: within budget every time
        // the shares can hold their markers.
        let dense = |id: &str, n: usize| ToolResultRecord {
            awili_call_id: AwiliCallId(id.into()),
            content: "\u{6F22}".repeat(n),
            is_error: false,
        };
        for budget in [200u64, 500, 1_000, 3_333, 20_000] {
            for set in [
                vec![result("a", 50_000), result("b", 50_000)],
                vec![result("a", 10), result("b", 900), result("c", 90_000)],
                vec![dense("a", 40_000), result("b", 40_000), dense("c", 5)],
                (0..12).map(|i| result(&format!("r{i}"), 7_000)).collect(),
            ] {
                let n = set.len() as u64;
                let (fitted, _) = fit_tool_results(set, budget, &c);
                assert!(cost(&fitted) <= budget, "{budget}: {}", cost(&fitted));
                assert_eq!(fitted.len() as u64, n, "no result is dropped");
            }
        }
        // CJK text is measured by its bytes, not as three characters a token.
        let (cjk, cut) = fit_tool_results(vec![dense("a", 9_000)], 6_000, &c);
        assert_eq!(cut, 1, "27,000 bytes do not fit 6,000 tokens");
        assert!(cost(&cjk) <= 6_000);
        // A share too small for the marker: the short marker alone; with no
        // room at all, an empty result — never over the budget.
        let (small, _) = fit_tool_results(vec![result("a", 50_000)], 40, &c);
        assert_eq!(small[0].content, cut_marker_short(50_000));
        let (none, _) = fit_tool_results(vec![result("a", 50_000), result("b", 9)], 0, &c);
        assert!(none.iter().all(|r| r.content.is_empty()), "{none:?}");
    }

    /// RC1-N (pre-review P1): a summary longer than its allowance is cut to
    /// it; a short one is untouched.
    #[test]
    fn rc1n_a_summary_is_cut_to_its_allowance() {
        let c = counter();
        assert_eq!(fit_summary("short".into(), 1024, &c), "short");
        let long = fit_summary("word ".repeat(2_000), 1024, &c);
        assert!(c.count_text(&long) <= 1024, "{}", c.count_text(&long));
        assert!(long.ends_with('\u{2026}') && long.starts_with("word word"));
    }

    /// RC1-N: the budget for a new exchange counts what can never be
    /// compacted — the active suffix at full cost — and older history only
    /// at the assumed summary size.
    #[test]
    fn rc1n_the_result_budget_leaves_room_for_the_rest() {
        let c = counter();
        let p = profile(32_768);
        let mut h = ContextHistory::new();
        h.push_user("old ".repeat(20_000));
        h.push_exchange("old answer", vec![], vec![]).unwrap();
        h.push_user("now");
        let budget = exchange_result_budget(&h, &p, &c, "", &[], 1);
        // The old 80k characters count only as the assumed summary.
        assert!(budget > 25_000 && budget < p.input_budget(), "{budget}");
        // A long active request takes its full cost.
        let mut h2 = ContextHistory::new();
        h2.push_user("now ".repeat(20_000));
        let budget2 = exchange_result_budget(&h2, &p, &c, "", &[], 1);
        assert!(budget2 < budget, "{budget2} < {budget}");
        assert_eq!(
            exchange_result_budget(&h2, &profile(1_000), &c, "", &[], 1),
            0
        );
        // No room even for the exchange's own calls: none at all.
        assert_eq!(exchange_room(&h2, &profile(1_000), &c, "", &[], 1), None);
        assert!(exchange_room(&h, &p, &c, "", &[], 1).is_some());
    }

    /// RC1-N (pre-review P2): a batch runs only when every result keeps at
    /// least room for its short marker.
    #[test]
    fn rc1n_a_batch_needs_room_for_every_marker() {
        let c = counter();
        let mut h = ContextHistory::new();
        h.push_user("q");
        let calls: Vec<ToolCall> = (0..3)
            .map(|i| ToolCall {
                awili_call_id: AwiliCallId(format!("c{i}")),
                provider_call_id: Some(format!("c{i}")),
                name: "fs.read".into(),
                arguments: "{}".into(),
            })
            .collect();
        let big = profile(32_768);
        let room = exchange_room(&h, &big, &c, "", &calls, 3).unwrap();
        let need = 3 * marker_floor(&c);
        // A window whose room is exactly `need`, then one token less.
        let exact = profile(32_768 - (room - need));
        assert_eq!(exchange_room(&h, &exact, &c, "", &calls, 3), Some(need));
        assert!(batch_has_room(&h, &exact, &c, "", &calls));
        let short = profile(32_768 - (room - need) - 1);
        assert!(!batch_has_room(&h, &short, &c, "", &calls));
    }

    /// RC1-N: after a model server refused a request as too long, the newest
    /// exchange's results shrink to half (ids unchanged); results already
    /// down to markers do not shrink again.
    #[test]
    fn rc1n_the_newest_results_shrink_by_half() {
        let c = counter();
        let mut h = ContextHistory::new();
        h.push_user("q");
        let call = |id: &str| ToolCall {
            awili_call_id: AwiliCallId(id.into()),
            provider_call_id: Some(id.into()),
            name: "fs.read".into(),
            arguments: "{}".into(),
        };
        h.push_exchange("", vec![call("a")], vec![result("a", 30_000)])
            .unwrap();
        let ids: Vec<GroupId> = h.groups().iter().map(|g| g.id()).collect();
        let size = |h: &ContextHistory| match h.groups().last() {
            Some(ContextGroup::Exchange { results, .. }) => results[0].content.len(),
            _ => 0,
        };
        assert!(h.shrink_latest_results(&c));
        let once = size(&h);
        assert!(once < 16_000 && once > 10_000, "{once}");
        assert_eq!(ids, h.groups().iter().map(|g| g.id()).collect::<Vec<_>>());
        // Pre-review P1: shrinking again and again stops at one short
        // marker per result — never an empty result — and then reports no
        // progress.
        let mut two = ContextHistory::new();
        two.push_user("q");
        two.push_exchange(
            "",
            vec![call("a"), call("b")],
            vec![result("a", 30_000), result("b", 30_000)],
        )
        .unwrap();
        let mut steps = 0;
        while two.shrink_latest_results(&c) {
            steps += 1;
            assert!(steps < 40, "shrinking must end");
        }
        match two.groups().last() {
            Some(ContextGroup::Exchange { results, .. }) => {
                for r in results {
                    assert!(r.content.contains("[Cut by Awili"), "{:?}", r.content);
                }
            }
            other => panic!("{other:?}"),
        }
        // Pre-review P1: with the newest exchange down to its markers, an
        // earlier exchange of the same turn shrinks next.
        let mut turn = ContextHistory::new();
        turn.push_user("q");
        turn.push_exchange("", vec![call("a")], vec![result("a", 30_000)])
            .unwrap();
        turn.push_exchange("", vec![call("b")], vec![result("b", 30_000)])
            .unwrap();
        let sizes = |h: &ContextHistory| -> Vec<usize> {
            h.groups()
                .iter()
                .filter_map(|g| match g {
                    ContextGroup::Exchange { results, .. } => Some(results[0].content.len()),
                    _ => None,
                })
                .collect()
        };
        assert!(turn.shrink_latest_results(&c));
        assert_eq!(sizes(&turn)[0], 30_000, "the newest first");
        while turn.shrink_latest_results(&c) {}
        let end = sizes(&turn);
        assert!(
            end.iter().all(|&n| n < 200),
            "both down to markers: {end:?}"
        );
        // Pre-review P2 (pass 6): an 80-character result (40 tokens) still
        // shrinks to its own short marker (about 32 tokens).
        let mut small = ContextHistory::new();
        small.push_user("q");
        small
            .push_exchange("", vec![call("a")], vec![result("a", 80)])
            .unwrap();
        assert!(small.shrink_latest_results(&c));
        match small.groups().last() {
            Some(ContextGroup::Exchange { results, .. }) => {
                assert_eq!(results[0].content, cut_marker_short(80));
            }
            other => panic!("{other:?}"),
        }
        assert!(!small.shrink_latest_results(&c), "nothing left to shrink");
        // Pre-review P2 (pass 11): beside a large result, a small one still
        // shrinks to its own marker.
        let mut mixed = ContextHistory::new();
        mixed.push_user("q");
        mixed
            .push_exchange(
                "",
                vec![call("a"), call("b")],
                vec![result("a", 100_000), result("b", 80)],
            )
            .unwrap();
        while mixed.shrink_latest_results(&c) {}
        match mixed.groups().last() {
            Some(ContextGroup::Exchange { results, .. }) => {
                assert_eq!(results[1].content, cut_marker_short(80));
                assert_eq!(results[0].content, cut_marker_short(100_000));
            }
            other => panic!("{other:?}"),
        }
        // Pre-review P3 (pass 9): every resend's marker states the ORIGINAL
        // size, and the old marker is not kept as text.
        let mut big = ContextHistory::new();
        big.push_user("q");
        let (first, _) = fit_tool_results(vec![result("a", 150_000)], 20_000, &c);
        big.push_exchange("", vec![call("a")], first).unwrap();
        while big.shrink_latest_results(&c) {
            if let Some(ContextGroup::Exchange { results, .. }) = big.groups().last() {
                let t = &results[0].content;
                assert!(
                    t.contains("of 150000 characters kept")
                        || t.contains("150000 characters did not fit"),
                    "{}",
                    &t[t.len().saturating_sub(120)..]
                );
                assert_eq!(t.matches("[Cut by Awili").count(), 1, "one marker");
            }
        }
        let mut empty = ContextHistory::new();
        empty.push_user("q");
        assert!(!empty.shrink_latest_results(&c));
    }
}

#[cfg(test)]
mod rc1n_fix2_mark_tests {
    use super::*;

    /// RC1-N-FIX2 (pre-review pass 4): the log-turn mark survives compaction
    /// and a stored round trip, and a history stored before it existed
    /// restores without one.
    #[test]
    fn the_log_turn_mark_survives_compaction_and_storage() {
        let mut h = ContextHistory::new();
        let a = h.push_user("one");
        let b = h.push_exchange("ok", vec![], vec![]).unwrap();
        h.push_user("two");
        h.set_log_turn("turn-awili-3");
        let compacted = apply_summary(&h, &[a, b], "earlier: one, ok").unwrap();
        assert_eq!(compacted.log_turn(), Some("turn-awili-3"));
        let stored = serde_json::to_string(&compacted).unwrap();
        let back: ContextHistory = serde_json::from_str(&stored).unwrap();
        assert_eq!(back.log_turn(), Some("turn-awili-3"));
        let unmarked = serde_json::to_string(&ContextHistory::new()).unwrap();
        assert!(!unmarked.contains("log_checkpoint"), "{unmarked}");
        assert!(
            stored.contains(r#""log_checkpoint":"turn-awili-3""#),
            "{stored}"
        );
        // RC1-N-FIX3: a RC1-N-FIX2 stamp (`log_turn`) is not exact: ignored.
        let fix2: ContextHistory =
            serde_json::from_str(r#"{"groups":[],"next_id":0,"log_turn":"turn-awili-3"}"#).unwrap();
        assert_eq!(fix2.log_turn(), None);
        let old: ContextHistory = serde_json::from_str(r#"{"groups":[],"next_id":0}"#).unwrap();
        assert_eq!(old.log_turn(), None);
    }
}

#[cfg(test)]
mod image_content_regressions {
    use super::*;
    #[test]
    fn image_admission_reserves_vision_tokens_without_counting_base64_as_prose() {
        let mut image = b"\x89\x50\x4e\x47\x0d\x0a\x1a\x0a\x00\x00\x00\x0d\x49\x48\x44\x52\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\x0d\x49\x44\x41\x54\x78\x9c\x63\xf8\xcf\xc0\xf0\x1f\x00\x05\x00\x01\xff\x89\x99\x3d\x1d\x00\x00\x00\x00\x49\x45\x4e\x44\xae\x42\x60\x82".to_vec();
        image.extend(vec![0; 1024 * 1024]);
        let source = format!(
            "![s]({})\nDescribe",
            awili_image_content::uri("image/png", &image)
        );
        let counter = HeuristicCounter {
            chars_per_token: 3.,
            bytes_per_token: 3.,
        };
        let cost = content_tokens(
            &ContextGroup::User {
                id: GroupId(1),
                text: source.clone(),
            },
            &counter,
            true,
        );
        assert!(cost >= awili_image_content::IMAGE_TOKEN_RESERVE);
        assert!(cost < 20000, "a binary attachment is not 450k prose tokens");
        assert!(counter.count_text(&source) > 400000);
    }
    #[test]
    fn old_screenshots_use_only_placeholder_budget_on_a_text_followup() {
        let image="![s](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)";
        let mut h = ContextHistory::new();
        h.push_user(image);
        h.push_user(image);
        h.push_user("Continue using the previous answer");
        let p = ModelContextProfile {
            provider: "chat-completions".into(),
            model: "m".into(),
            window_tokens: 32768,
            reserved_output_tokens: 2048,
            safety_margin_tokens: 512,
            assumed_summary_tokens: 1024,
            replays_continuations: false,
            fixed_request_overhead_tokens: 64,
            per_message_overhead_tokens: 8,
        };
        let c = HeuristicCounter {
            chars_per_token: 3.,
            bytes_per_token: 3.,
        };
        let AssembleOutcome::Fits(a) = assemble(&h, &p, &c).unwrap() else {
            panic!("old image reservations must not force compaction");
        };
        assert!(a.tokens < 1000);
        h.push_user(image);
        let AssembleOutcome::Fits(a) = assemble(&h, &p, &c).unwrap() else {
            panic!("one current image fits");
        };
        assert!(a.tokens >= awili_image_content::IMAGE_TOKEN_RESERVE);
        assert!(a.tokens < 20000);
    }
}
