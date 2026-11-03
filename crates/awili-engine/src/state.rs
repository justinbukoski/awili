//! Turn state — everything the reducer needs to be deterministic.
//!
//! Fields are private (Codex B1 review P2): downstream code cannot construct
//! impossible states. Serde derives exist for B2 checkpointing, but the
//! durable log MUST wrap these in versioned persistence DTOs — the runtime
//! representation is not a wire/storage contract.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::events::{AwiliCallId, ToolCall, ToolResultRecord, Usage};
use crate::provenance::Provenance;

/// Phase of the turn state machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnPhase {
    /// Created, no deltas yet.
    Idle,
    /// Receiving wire deltas.
    Streaming,
    /// Provider finished; awaiting the trailing usage chunk / StreamEnd.
    Finishing,
    /// Tool calls dispatched; awaiting results.
    AwaitingToolResults,
    /// All results in; caller should request continuation from the model.
    ReadyToContinue,
    /// Terminal.
    Completed,
    Cancelled,
    Failed,
}

/// A tool call mid-assembly. Keyed by (choice, tool_index) — the only key
/// vLLM keeps stable across argument fragments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct PartialCall {
    pub(crate) provider_call_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) args_buf: String,
}

/// Full state of one agent turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnState {
    pub(crate) phase: TurnPhase,
    /// Durable turn identifier, supplied by the caller (B2 owns generation).
    /// Stamped into every AwiliCallId so call ids never collide across turns.
    pub(crate) turn_id: String,
    /// Who is producing this turn — stamped on continuation blobs.
    pub(crate) provenance: Provenance,
    /// Monotonic counter for diagnostics.
    pub(crate) deltas_seen: u64,
    /// Sequence number for AwiliCallId generation within this turn.
    pub(crate) next_call_seq: u32,
    /// In-flight tool-call assembly buffers.
    pub(crate) assembling: BTreeMap<(u32, u32), PartialCall>,
    /// Calls assembled + validated this turn, in provider order.
    pub(crate) ready_calls: Vec<ToolCall>,
    /// Calls dispatched and awaiting results.
    pub(crate) awaiting_results: Vec<AwiliCallId>,
    /// Canonical tool results ingested this turn (content preserved — the
    /// canonical transcript is the sole cross-model context carrier).
    pub(crate) results: Vec<ToolResultRecord>,
    /// Accumulated assistant text.
    pub(crate) text: String,
    /// Accumulated provider reasoning for the CURRENT segment (drained into
    /// a ContinuationBlob at each segment boundary).
    pub(crate) reasoning: String,
    /// Stop reason recorded at Finish, resolved at StreamEnd.
    pub(crate) pending_finish: Option<crate::wire::FinishReason>,
    /// Aggregated usage across ALL segments of this turn.
    pub(crate) usage_total: Option<Usage>,
    /// A Cancel input was received; truncation is now expected.
    pub(crate) cancel_requested: bool,
}

impl TurnState {
    /// `turn_id` must be unique per turn within the session — the caller
    /// (ultimately the B2 durable log) owns generating it deterministically.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Self {
        Self::with_turn_index(provider, model, turn_id, 0)
    }

    /// `turn_index` is the zero-based turn number within the session; it is
    /// stamped into continuation-artifact provenance.
    pub fn with_turn_index(
        provider: impl Into<String>,
        model: impl Into<String>,
        turn_id: impl Into<String>,
        turn_index: u32,
    ) -> Self {
        Self {
            phase: TurnPhase::Idle,
            turn_id: turn_id.into(),
            provenance: Provenance::new(provider, model, turn_index),
            deltas_seen: 0,
            next_call_seq: 0,
            assembling: BTreeMap::new(),
            ready_calls: Vec::new(),
            awaiting_results: Vec::new(),
            results: Vec::new(),
            text: String::new(),
            reasoning: String::new(),
            pending_finish: None,
            usage_total: None,
            cancel_requested: false,
        }
    }

    pub fn phase(&self) -> &TurnPhase {
        &self.phase
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    pub fn usage_total(&self) -> Option<Usage> {
        self.usage_total
    }

    pub fn results(&self) -> &[ToolResultRecord] {
        &self.results
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.phase,
            TurnPhase::Completed | TurnPhase::Cancelled | TurnPhase::Failed
        )
    }

    pub(crate) fn mint_call_id(&mut self) -> AwiliCallId {
        let id = AwiliCallId(format!("{}-call-{:04}", self.turn_id, self.next_call_seq));
        self.next_call_seq += 1;
        id
    }

    pub(crate) fn add_usage(&mut self, u: Usage) {
        let t = self.usage_total.get_or_insert(Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
        });
        t.prompt_tokens += u.prompt_tokens;
        t.completion_tokens += u.completion_tokens;
    }
}
