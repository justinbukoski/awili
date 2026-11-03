//! The canonical agent-event vocabulary — the output of the reducer and the
//! single cross-model context carrier (B1). The transcript, the B2 execution
//! log, C4 compaction, and the UI all consume THESE, never provider frames.

use serde::{Deserialize, Serialize};

use crate::error::EngineError;
use crate::provenance::ContinuationArtifact;

/// Stable Awili-side identifier for a tool call within a session.
/// Provider call ids are correlated but never load-bearing (B2 maps both).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AwiliCallId(pub String);

/// A fully-assembled, validated tool call ready for authorization (C1) and
/// dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub awili_call_id: AwiliCallId,
    pub provider_call_id: Option<String>,
    pub name: String,
    /// Argument JSON, validated well-formed (object) at assembly time.
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// Canonical tool result (Codex B1 review P0): the payload IS part of the
/// canonical transcript — B2 logs it durably and B3 encodes it into
/// continuation requests. Never dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultRecord {
    pub awili_call_id: AwiliCallId,
    /// Result content as produced by the tool (text or serialized JSON).
    pub content: String,
    pub is_error: bool,
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopKind {
    /// Natural end of assistant output.
    EndTurn,
    /// Model asked for tools; the turn continues after results.
    ToolUse,
    /// Output truncated by token limit.
    Length,
    /// Provider refused / filtered.
    Refusal,
    /// Provider paused the turn (resumable).
    Paused,
    /// Unrecognized provider stop value, preserved.
    Other(String),
}

/// Canonical events. Append-only vocabulary: extend, never repurpose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentEvent {
    /// First event of every turn.
    TurnStarted,
    /// Assistant text increment (render-ready).
    TextDelta(String),
    /// Provider reasoning increment. Rendered separately; accumulated
    /// reasoning becomes a ContinuationBlob at turn end.
    ReasoningDelta(String),
    /// A tool call finished assembling and validated well-formed.
    ToolCallReady(ToolCall),
    /// A tool call finished assembling but its arguments are not valid JSON
    /// (P6 — typed failure, never a panic, never silent).
    ToolCallInvalid {
        awili_call_id: AwiliCallId,
        provider_call_id: Option<String>,
        name: Option<String>,
        raw_arguments: String,
        reason: String,
    },
    /// A tool result was fed back into the turn (full canonical record —
    /// content preserved for the durable log and continuation encoding).
    ToolResultIngested(ToolResultRecord),
    /// Per-segment usage report (multi-segment turns report each segment;
    /// terminal events carry the aggregated total).
    UsageReported(Usage),
    /// A host control input (tool result / duplicate) arrived when it could
    /// not be accepted. Auditable, does not change phase (Codex review P1:
    /// silent absorption would hide replay bugs from B2).
    InputRejected { reason: String },
    /// Opaque provider-native continuation artifact captured at a segment
    /// boundary (Set A model, arbitration 2026-08-06: provenance-tagged,
    /// kind-labeled, payload never interpreted; replay eligibility is a pure
    /// function of the provenance tag).
    ContinuationBlobCaptured(ContinuationArtifact),
    /// Turn ended (all terminal variants carry what is known).
    TurnCompleted {
        stop: StopKind,
        usage: Option<Usage>,
    },
    /// Turn ended because the user cancelled (stream truncation after a
    /// Cancel input).
    TurnCancelled { usage: Option<Usage> },
    /// Turn ended in error (provider error, unexpected truncation, protocol
    /// violation).
    TurnFailed(EngineError),
}
