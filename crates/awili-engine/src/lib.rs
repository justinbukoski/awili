//! awili-engine — the pure core of Awili's owned, model-agnostic agent loop.
//!
//! Slice B1 of ENGINE-PIVOT-PLAN.md r3.1: the canonical agent-event model and
//! the turn state machine as a pure reducer. Decoded wire deltas and tool
//! results go in; typed [`AgentEvent`]s and a [`NextAction`] come out.
//!
//! Purity contract (PS-ENG-02): no IO, no gpui, no async runtime. Everything
//! here is deterministic: the same input sequence always yields the same
//! events and final state. Dialect quirks (Ollama's whole-chunk tool calls,
//! vLLM's fragmented arguments — see docs/engine-wire-contract.md) are
//! normalized by the B3 codecs into [`wire::WireDelta`] before they reach
//! this crate; provider-native reasoning artifacts stay opaque per the
//! continuation-blob rule (PS-ENG-04).

pub mod codec;
pub mod context;
pub mod error;
pub mod events;
pub mod generation;
pub mod identity;
pub mod provenance;
pub mod reducer;
pub mod risk;
pub mod state;
pub mod wire;

pub use context::{
    apply_summary, assemble, AssembleOutcome, AssembledContext, CompactionPlan, ContextError,
    ContextGroup, ContextHistory, ExclusionReason, GroupId, HeuristicCounter, ModelContextProfile,
    TokenCounter,
};
pub use error::EngineError;
pub use events::{AgentEvent, AwiliCallId, StopKind, ToolCall, ToolResultRecord, Usage};
pub use identity::{DescriptorDigest, ToolIdentity, ToolOrigin};
pub use provenance::{ContinuationArtifact, ContinuationKind, Provenance};
pub use reducer::{reduce, Input, NextAction};
pub use risk::RiskClass;
pub use state::{TurnPhase, TurnState};
pub use wire::{FinishReason, WireDelta};
