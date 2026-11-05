//! OpenAI Chat Completions streaming codec (B3, dialect 1 of 3).
//!
//! Grounded in live captures from BOTH observed server families:
//! Ollama (whole tool call in one chunk, `delta.reasoning`, trailing usage
//! chunk with empty `choices`) and vLLM 0.25.1 (fragmented tool-call
//! arguments keyed by `tool_calls[].index`, nonstandard `stop_reason` /
//! `token_ids` fields — ignored). See docs/engine-wire-contract.md.

use serde::Deserialize;

use crate::wire::{FinishReason, WireDelta};

/// Decode a complete captured/received stream body into wire deltas.
///
/// Accepts both SSE streams (`data: {...}` lines ending with `data: [DONE]`)
/// and bare JSON error bodies (real behavior: Ollama returns a non-SSE
/// `{"error": {...}}` body — fixture 08). A stream that ends without the
/// `[DONE]` sentinel yields [`WireDelta::StreamTruncated`]; the reducer
/// decides cancellation vs failure.
///
/// Malformed frames fail typed (P6): decoding stops with a
/// [`WireDelta::ProviderError`] describing the frame — never a panic, never
/// a silent skip.
pub fn decode_stream(raw: &str) -> Vec<WireDelta> {
    // SSE if ANY line is a data: field (streams may open with comment
    // keepalives or `event:` fields — SSE permits both).
    let is_sse = raw.lines().any(|l| l.trim_start().starts_with("data:"));
    if !is_sse {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return vec![WireDelta::StreamTruncated];
        }
        // Whole body first (pretty-printed bodies), then first-JSON-line
        // fallback (Ollama repeats the error object on multiple lines).
        let parsed = serde_json::from_str::<ErrorBody>(trimmed)
            .or_else(|_| serde_json::from_str::<ErrorBody>(extract_first_json_line(trimmed)));
        return match parsed {
            Ok(body) => vec![body.error.delta()],
            Err(_) => vec![WireDelta::ProviderError {
                message: format!(
                    "unrecognized non-SSE response body ({} bytes)",
                    trimmed.len()
                ),
                retryable: false,
            }],
        };
    }

    // RC2-GEMMA3: one decoder for both paths — the whole body here, line by
    // line as it arrives in the live path ([`SseDecoder`]).
    let mut decoder = SseDecoder::new();
    let mut out = Vec::new();
    for line in raw.lines() {
        out.extend(decoder.push_line(line));
        if decoder.is_finished() {
            return out;
        }
    }
    out.extend(decoder.finish());
    out
}

/// RC2-GEMMA3 (R7-speed): [`decode_stream`] for an SSE body read line by line
/// as it arrives, so the reply shows while the model writes it. Fed the
/// body's lines in order and then [`finish`](Self::finish)ed, it yields
/// exactly the deltas `decode_stream` yields for the whole body.
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// The `data:` lines of the event being assembled (SSE: consecutive
    /// data lines of one event, ended by a blank line, join with `\n`).
    event_data: Vec<String>,
    event_bytes: usize,
    /// `[DONE]`, a typed error or the end of the body ended decoding.
    finished: bool,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decoding ended: nothing more is decoded.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// One line of the body, without its line ending. Returns the deltas of
    /// the event this line completes (a blank line ends an event); comments
    /// (`:`) and other fields (`event:`, `id:`, `retry:`) are ignored.
    pub fn push_line(&mut self, line: &str) -> Vec<WireDelta> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        if let Some(d) = line.strip_prefix("data:") {
            let d = d.strip_prefix(' ').unwrap_or(d);
            self.event_bytes = self.event_bytes.saturating_add(d.len()).saturating_add(1);
            if self.event_bytes > 1024 * 1024 {
                self.event_data.clear();
                self.finished = true;
                out.push(WireDelta::ProviderError { message: "SSE event exceeds size limit".into(), retryable: false });
                return out;
            }
            self.event_data.push(d.to_string());
        }
        if line.trim().is_empty() && !self.event_data.is_empty() {
            // A real blank-line boundary: the pending event arrived COMPLETE.
            self.event(true, &mut out);
        }
        out
    }

    /// The body ended. An event still pending was cut off unterminated
    /// mid-frame; then, unless `[DONE]` or a typed error already ended
    /// decoding, the stream ended without `[DONE]`: truncation (client
    /// cancel or connection drop).
    pub fn finish(&mut self) -> Vec<WireDelta> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        if !self.event_data.is_empty() {
            self.event(false, &mut out);
            if self.finished {
                return out;
            }
        }
        self.finished = true;
        out.push(WireDelta::StreamTruncated);
        out
    }

    fn event(&mut self, terminated_by_boundary: bool, out: &mut Vec<WireDelta>) {
        let data = self.event_data.join("\n");
        self.event_data.clear();
        self.event_bytes = 0;
        let data = data.trim();
        if data == "[DONE]" {
            out.push(WireDelta::StreamEnd);
            self.finished = true;
            return;
        }
        match serde_json::from_str::<Chunk>(data) {
            Ok(chunk) => {
                if decode_chunk(chunk, out).is_err() {
                    self.finished = true; // typed error already pushed
                }
            }
            Err(e) => {
                // Two distinct final-frame cases (Codex B4 round 2, §6):
                // an event TERMINATED by a blank-line SSE boundary is a
                // complete frame the provider chose to send — malformed =
                // provider garbage, typed failure (P6). An UNTERMINATED
                // tail at EOF (no closing boundary) is the torn end of a
                // client cancel / connection drop (REAL lightning
                // `cancellation` capture, 2026-08-13) — truncation, so the
                // reducer doesn't blame the server for a client hangup.
                if terminated_by_boundary {
                    out.push(WireDelta::ProviderError {
                        message: format!("malformed stream frame: {e}"),
                        retryable: false,
                    });
                } else {
                    out.push(WireDelta::StreamTruncated);
                }
                self.finished = true;
            }
        }
    }
}

fn decode_chunk(chunk: Chunk, out: &mut Vec<WireDelta>) -> Result<(), ()> {
    // In-stream SSE error event (wire contract: errors arrive BOTH as
    // non-SSE bodies and as SSE error events). Without this explicit field,
    // unknown-field tolerance would silently swallow the error.
    if let Some(err) = chunk.error {
        out.push(err.delta());
        return Err(());
    }
    // Multi-choice (n > 1) responses are unsupported: Text/Reasoning carry
    // no choice index in the canonical model, so merging alternatives would
    // corrupt the transcript. Awili never requests n > 1; defend anyway.
    if chunk.choices.iter().any(|c| c.index != 0) {
        out.push(WireDelta::ProviderError {
            message: "multi-choice (n>1) streams are unsupported".into(),
            retryable: false,
        });
        return Err(());
    }
    for choice in &chunk.choices {
        decode_choice(choice, out);
    }
    if let Some(u) = chunk.usage {
        out.push(WireDelta::Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
        });
    }
    Ok(())
}

fn decode_choice(choice: &Choice, out: &mut Vec<WireDelta>) {
    let idx = choice.index;
    if let Some(d) = &choice.delta {
        if let Some(r) = &d.reasoning {
            if !r.is_empty() {
                out.push(WireDelta::Reasoning(r.clone()));
            }
        }
        if let Some(c) = &d.content {
            if !c.is_empty() {
                out.push(WireDelta::Text(c.clone()));
            }
        }
        for tc in d.tool_calls.iter().flatten() {
            out.push(WireDelta::ToolCallFragment {
                choice: idx,
                tool_index: tc.index,
                id: tc.id.clone(),
                name: tc.function.as_ref().and_then(|f| f.name.clone()),
                args_fragment: tc
                    .function
                    .as_ref()
                    .and_then(|f| f.arguments.clone())
                    .unwrap_or_default(),
            });
        }
    }
    if let Some(fr) = &choice.finish_reason {
        out.push(WireDelta::Finish {
            choice: idx,
            reason: map_finish(fr),
        });
    }
}

fn map_finish(raw: &str) -> FinishReason {
    match raw {
        "stop" => FinishReason::Stop,
        "tool_calls" => FinishReason::ToolCalls,
        "length" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        // Anthropic-compatible gateways proxying these through:
        "pause_turn" => FinishReason::PauseTurn,
        "refusal" => FinishReason::Refusal,
        other => FinishReason::Other(other.to_string()),
    }
}

fn is_retryable(error_type: Option<&str>) -> bool {
    // Conservative: only classes that are plausibly transient.
    matches!(
        error_type,
        Some("rate_limit_error" | "overloaded_error" | "server_error")
    )
}

/// First line only — Ollama repeats the error body on multiple lines.
fn extract_first_json_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

// ---- serde views (unknown fields ignored by construction) ----

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageView>,
    #[serde(default)]
    error: Option<ErrorField>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    delta: Option<Delta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallView>>,
}

#[derive(Deserialize)]
struct ToolCallView {
    #[serde(default)]
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionView>,
}

#[derive(Deserialize)]
struct FunctionView {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct UsageView {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorField,
}

/// RC2-GEMMA4 (R7-A3): an error is an object (`{"message": …, "type": …}`,
/// OpenAI, Ollama, vLLM) or a bare string — mlx_lm's server answers
/// `{"error": "generation thread died"}` (HTTP 404) once its generation
/// thread has died, which read as an "unrecognized non-SSE response body".
#[derive(Deserialize)]
#[serde(untagged)]
enum ErrorField {
    Object(ErrorView),
    Text(String),
}

impl ErrorField {
    fn delta(self) -> WireDelta {
        match self {
            Self::Object(e) => WireDelta::ProviderError {
                retryable: is_retryable(e.r#type.as_deref()),
                message: e.message,
            },
            Self::Text(message) => WireDelta::ProviderError {
                message,
                retryable: false,
            },
        }
    }
}

#[derive(Deserialize)]
struct ErrorView {
    message: String,
    #[serde(default)]
    r#type: Option<String>,
}

#[cfg(test)]
mod size_limit_tests {
    use super::*;

    #[test]
    fn oversized_event_fails_without_accumulating_more() {
        let mut decoder = SseDecoder::new();
        assert!(decoder.push_line(&format!("data: {}", "a".repeat(1024 * 1024))).iter().any(|d| matches!(d, WireDelta::ProviderError { .. })));
        assert!(decoder.is_finished());
        assert!(decoder.event_data.is_empty());
    }
}
