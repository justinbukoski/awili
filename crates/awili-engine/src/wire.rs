//! Dialect-neutral wire deltas — the input vocabulary of the reducer.
//!
//! B3 codecs (Chat Completions, Responses, Anthropic Messages) decode raw
//! streams into these. Fixture-grounded: every variant corresponds to
//! behavior observed in crates/awili-engine/fixtures/ (A2) or required by the
//! wire contract. Codecs must tolerate unknown provider fields; by the time
//! data reaches this enum, dialect is gone.

use serde::{Deserialize, Serialize};

/// One decoded increment of a streaming model response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WireDelta {
    /// Assistant text content delta.
    Text(String),
    /// Provider-native reasoning delta (Ollama and vLLM both emit
    /// `delta.reasoning`). Kept distinct from text — never concatenated.
    Reasoning(String),
    /// A fragment of a tool call. Ollama sends the entire call in one
    /// fragment (id + name + full arguments); vLLM sends the id/name first
    /// and streams `args_fragment` across many chunks keyed only by
    /// `tool_index`. Assembly is the reducer's job.
    ToolCallFragment {
        /// choices[].index — 0 for every fixture we serve today, but carried.
        choice: u32,
        /// tool_calls[].index — the only stable key across vLLM fragments.
        tool_index: u32,
        /// Present on the first fragment (both servers).
        id: Option<String>,
        /// Present when the provider names the function in this fragment.
        name: Option<String>,
        /// Possibly-partial JSON argument text; may be empty.
        args_fragment: String,
    },
    /// Provider signalled the end of a choice's output.
    Finish { choice: u32, reason: FinishReason },
    /// Usage accounting chunk (arrives after Finish, with empty choices, on
    /// both observed servers).
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    /// Provider-reported error. Chat Completions servers send these as
    /// non-SSE JSON bodies (fixture 08, real OOM) or as SSE error events.
    ProviderError {
        message: String,
        /// Codec's judgment: is retrying plausibly useful?
        retryable: bool,
    },
    /// Clean end of stream (`[DONE]`).
    StreamEnd,
    /// Stream ended WITHOUT `[DONE]` — connection drop or client cancel
    /// (fixture 05). The reducer decides which, based on whether a Cancel
    /// input was seen.
    StreamTruncated,
}

/// Normalized stop reasons (P10). Dialect-specific values (`pause_turn`,
/// `refusal` from Anthropic) are first-class so downstream never re-parses
/// provider strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    /// Anthropic `pause_turn` — turn may be resumed.
    PauseTurn,
    /// Anthropic `refusal`.
    Refusal,
    /// Unknown value, preserved verbatim (unknown-field tolerance).
    Other(String),
}
