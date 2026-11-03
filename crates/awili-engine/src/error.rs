//! Typed engine errors (P6/P8): every failure mode is a value, never a panic.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum EngineError {
    /// The provider reported an error (HTTP error body or SSE error event).
    /// Real example on file: Ollama OOM (fixture 08).
    #[error("provider error{}: {message}", if *.retryable { " (retryable)" } else { "" })]
    Provider { message: String, retryable: bool },

    /// The stream ended without `[DONE]` and no cancel was requested.
    #[error("stream truncated unexpectedly after {events_seen} deltas")]
    UnexpectedTruncation { events_seen: u64 },

    /// The provider violated the protocol in a way the reducer cannot
    /// reconcile (e.g. deltas after finish for the same choice).
    #[error("protocol violation: {0}")]
    Protocol(String),

    /// Input arrived in a phase that cannot accept it (caller bug).
    #[error("input rejected in phase {phase}: {reason}")]
    InvalidInput { phase: String, reason: String },
}
