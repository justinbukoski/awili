//! OpenAI-compatible Chat Completions API over SSE (`POST /v1/chat/completions`).
//!
//! This is the wire shape spoken by plain OpenAI-compatible endpoints — vLLM,
//! llama.cpp's server, DeepSeek's cloud API, etc. (e.g. deepseek-flash at
//! `api.deepseek.com`; the local deepseek the fleet once served on Spark no
//! longer runs). It is distinct from the Anthropic `/v1/messages` shape that
//! [`HermesCompat`](crate::HermesCompat) speaks.
//!
//! The stream is SSE: `data: {json}` frames separated by blank lines, terminated
//! by a literal `data: [DONE]`. Text arrives at `choices[0].delta.content`; usage
//! (requested via `stream_options.include_usage`) arrives in a final chunk whose
//! `choices` array is empty.

use serde_json::{json, Value};

use crate::decode::{push_too_large, Decoder, LineBuffer, MAX_LINE_BYTES};
use crate::event::{clamp_u32, ChatRequest, ProviderEvent, Role, Usage};

/// Build the JSON request body for the OpenAI Chat Completions API. Unlike
/// Anthropic, the system preamble is a leading `"system"` message (the API
/// accepts a system role in the array), and `max_tokens` is optional so it is
/// omitted — the endpoint's own default applies.
pub fn build_body(req: &ChatRequest) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = &req.system {
        if !system.is_empty() {
            messages.push(json!({ "role": "system", "content": system }));
        }
    }
    // Historical attachments keep their textual placeholder. Resending them
    // into every future turn poisons a later switch to a text-only model.
    let current_user = req.messages.iter().rposition(|m| m.role == Role::User);
    for (index, m) in req.messages.iter().enumerate() {
        let role = match m.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        messages.push(json!({ "role": role, "content": if m.role == Role::User { if Some(index) == current_user { awili_image_content::openai(&m.content) } else { json!(awili_image_content::split(&m.content).text) } } else { json!(m.content) } }));
    }
    json!({
        "model": req.model,
        "stream": true,
        // Ask the endpoint to emit a final usage chunk so the cost ticker works.
        "stream_options": { "include_usage": true },
        "messages": messages,
    })
}

/// Incremental SSE decoder for the OpenAI chat-completions stream.
#[derive(Default)]
pub struct OpenAiDecoder {
    lines: LineBuffer,
    /// Accumulated `data:` payload for the event currently being assembled.
    data: String,
    done_emitted: bool,
}

impl OpenAiDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    fn on_line(&mut self, line: &str, out: &mut Vec<ProviderEvent>) {
        if self.done_emitted {
            return;
        }
        if line.is_empty() {
            if !self.data.is_empty() {
                let data = std::mem::take(&mut self.data);
                self.dispatch(&data, out);
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            // S7a (F0782): one assembled event is capped like one line.
            if self.data.len() + 1 + rest.len() > MAX_LINE_BYTES {
                self.data = String::new();
                push_too_large(&mut self.done_emitted, out);
                return;
            }
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(rest);
        }
        // `event:`, `id:`, `:`-comments are ignored.
    }

    fn dispatch(&mut self, data: &str, out: &mut Vec<ProviderEvent>) {
        // The terminal sentinel is a literal, not JSON.
        if data.trim() == "[DONE]" {
            self.done_emitted = true;
            out.push(ProviderEvent::Done);
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            out.push(ProviderEvent::Error(format!(
                "malformed OpenAI SSE frame: {}",
                truncate_for_msg(data)
            )));
            self.done_emitted = true;
            out.push(ProviderEvent::Done);
            return;
        };
        // In-stream error object. Surface the structured error `type` ahead of the
        // prose message so automatic fallback classification (classify_error) keys
        // off the authoritative type — across OpenAI-compatible fleet servers
        // (vLLM, Hermes, Ollama's shim) the message text isn't standardized but the
        // type (`insufficient_quota`, `rate_limit_exceeded`, …) is.
        if let Some(msg) = v.pointer("/error/message").and_then(Value::as_str) {
            let surfaced = match v
                .pointer("/error/type")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                Some(t) => format!("{t}: {msg}"),
                None => msg.to_string(),
            };
            out.push(ProviderEvent::Error(surfaced));
            self.done_emitted = true;
            out.push(ProviderEvent::Done);
            return;
        }
        // Text token.
        if let Some(text) = v
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
        {
            if !text.is_empty() {
                out.push(ProviderEvent::Token(text.to_string()));
            }
        }
        // Usage chunk (choices empty, usage populated).
        if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
            let input = clamp_u32(
                usage
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            let output = clamp_u32(
                usage
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            if input > 0 || output > 0 {
                out.push(ProviderEvent::Usage(Usage {
                    cached_input_tokens: None,
                    input_tokens: input,
                    output_tokens: output,
                }));
            }
        }
    }
}

impl Decoder for OpenAiDecoder {
    fn feed(&mut self, chunk: &[u8]) -> Vec<ProviderEvent> {
        let mut out = Vec::new();
        for line in self.lines.feed(chunk) {
            self.on_line(&line, &mut out);
        }
        if self.lines.overflowed() {
            push_too_large(&mut self.done_emitted, &mut out);
        }
        out
    }

    fn finish(&mut self) -> Vec<ProviderEvent> {
        let mut out = Vec::new();
        if let Some(rem) = self.lines.take_remainder() {
            self.on_line(&rem, &mut out);
        }
        if !self.done_emitted && !self.data.is_empty() {
            let data = std::mem::take(&mut self.data);
            self.dispatch(&data, &mut out);
        }
        if !self.done_emitted {
            // EOF before `[DONE]`: the stream was cut. Don't pass a truncated
            // reply off as complete — Error then Done (matches the Anthropic
            // decoder's SF1 contract).
            self.done_emitted = true;
            out.push(ProviderEvent::Error(
                "OpenAI stream ended before [DONE] (truncated)".to_string(),
            ));
            out.push(ProviderEvent::Done);
        }
        out
    }
}

fn truncate_for_msg(s: &str) -> String {
    const MAX: usize = 120;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let mut end = MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = "\
data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}

data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}

data: {\"choices\":[{\"delta\":{\"content\":\", world\"}}]}

data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}

data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":7}}

data: [DONE]

";

    fn drain(d: &mut OpenAiDecoder, bytes: &[u8]) -> Vec<ProviderEvent> {
        let mut out = d.feed(bytes);
        out.extend(d.finish());
        out
    }

    #[test]
    fn decodes_full_stream() {
        let mut d = OpenAiDecoder::new();
        let events = drain(&mut d, STREAM.as_bytes());
        assert_eq!(
            events,
            vec![
                ProviderEvent::Token("Hello".to_string()),
                ProviderEvent::Token(", world".to_string()),
                ProviderEvent::Usage(Usage {
                    cached_input_tokens: None,
                    input_tokens: 12,
                    output_tokens: 7
                }),
                ProviderEvent::Done,
            ]
        );
    }

    #[test]
    fn usage_above_u32_max_clamps_not_wraps() {
        // M5: a usage frame carrying a count above u32::MAX (here 5_000_000_000,
        // ~1.16 * u32::MAX) must saturate to u32::MAX, not wrap to ~705M via a
        // truncating `as u32`. Bad upstream data should under-report at the
        // ceiling, never silently report a tiny wrapped value.
        let frame = "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5000000000,\"completion_tokens\":4294967296}}\n\ndata: [DONE]\n\n";
        let mut d = OpenAiDecoder::new();
        let events = drain(&mut d, frame.as_bytes());
        assert!(events.contains(&ProviderEvent::Usage(Usage {
            cached_input_tokens: None,
            input_tokens: u32::MAX,
            output_tokens: u32::MAX,
        })));
    }

    #[test]
    fn reassembles_across_chunks() {
        let bytes = STREAM.as_bytes();
        let mut d = OpenAiDecoder::new();
        let mut events = Vec::new();
        for chunk in bytes.chunks(5) {
            events.extend(d.feed(chunk));
        }
        events.extend(d.finish());
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::Token(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Hello, world");
        assert!(events.contains(&ProviderEvent::Done));
    }

    #[test]
    fn error_frame_emits_error_then_done() {
        // No `type` field -> the message is surfaced unchanged (no stray prefix).
        let mut d = OpenAiDecoder::new();
        let s = "data: {\"error\":{\"message\":\"model not found\"}}\n\n";
        let events = drain(&mut d, s.as_bytes());
        assert_eq!(
            events,
            vec![
                ProviderEvent::Error("model not found".to_string()),
                ProviderEvent::Done
            ]
        );
    }

    #[test]
    fn error_frame_type_drives_classification() {
        use crate::{classify_error, ProviderErrorKind};
        // An OpenAI-compatible fleet server's quota error: the prose message
        // ("You are out") carries no capacity keyword, so without the prefixed
        // `type` it would classify as Other (no retry). The `insufficient_quota`
        // type is what lets the turn fall back instead of dead-ending.
        let mut d = OpenAiDecoder::new();
        let s =
            "data: {\"error\":{\"type\":\"insufficient_quota\",\"message\":\"You are out\"}}\n\n";
        let events = drain(&mut d, s.as_bytes());
        let ProviderEvent::Error(msg) = &events[0] else {
            panic!("expected Error, got {events:?}");
        };
        assert_eq!(msg, "insufficient_quota: You are out");
        assert_eq!(classify_error(msg), ProviderErrorKind::Quota);
    }

    #[test]
    fn error_frame_overloaded_type_classifies_when_prose_is_opaque() {
        use crate::{classify_error, ProviderErrorKind};
        // vLLM-style: opaque prose, but the type carries the capacity signal.
        let mut d = OpenAiDecoder::new();
        let s = "data: {\"error\":{\"type\":\"overloaded\",\"message\":\"please retry\"}}\n\n";
        let events = drain(&mut d, s.as_bytes());
        let ProviderEvent::Error(msg) = &events[0] else {
            panic!("expected Error, got {events:?}");
        };
        assert_eq!(msg, "overloaded: please retry");
        assert_eq!(classify_error(msg), ProviderErrorKind::Overloaded);
    }

    #[test]
    fn truncated_stream_errors_then_done() {
        // No [DONE] arrives.
        let mut d = OpenAiDecoder::new();
        let s = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
        let events = drain(&mut d, s.as_bytes());
        assert_eq!(events[0], ProviderEvent::Token("hi".to_string()));
        assert!(matches!(events[1], ProviderEvent::Error(_)));
        assert_eq!(events[2], ProviderEvent::Done);
    }

    #[test]
    fn done_sentinel_is_not_parsed_as_json() {
        let mut d = OpenAiDecoder::new();
        let events = drain(&mut d, b"data: [DONE]\n\n");
        assert_eq!(events, vec![ProviderEvent::Done]);
    }

    #[test]
    fn build_body_puts_system_first_and_keeps_turns() {
        let req = ChatRequest {
            model: "deepseek-flash".to_string(),
            system: Some("Be brief.".to_string()),
            messages: vec![
                crate::event::ChatMessage::user("hi"),
                crate::event::ChatMessage::assistant("hey"),
                crate::event::ChatMessage::user("more"),
            ],
        };
        let body = build_body(&req);
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"]["include_usage"], json!(true));
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], json!("system"));
        assert_eq!(msgs[0]["content"], json!("Be brief."));
        assert_eq!(msgs[1]["role"], json!("user"));

        // No system preamble ⇒ no leading system message.
        let no_sys = ChatRequest {
            system: None,
            ..req
        };
        let body = build_body(&no_sys);
        assert_eq!(body["messages"][0]["role"], json!("user"));
    }
}
