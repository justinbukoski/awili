//! Anthropic Messages API over SSE, and the Anthropic-compatible Hermes variant.
//!
//! The wire format is Server-Sent Events: `event: <type>` / `data: <json>` frames
//! separated by blank lines. We ignore the `event:` line and dispatch on the JSON
//! `type` field, which carries the same information. The decoder is stateful only
//! to remember `input_tokens` (reported at `message_start`) so it can pair them
//! with the `output_tokens` reported at `message_delta` into one [`Usage`].

use serde_json::{json, Value};

use crate::decode::{push_too_large, Decoder, LineBuffer, MAX_LINE_BYTES};
use crate::event::{clamp_u32, ChatRequest, ProviderEvent, Role, Usage};

/// Build the JSON request body for the Anthropic Messages API. `stream: true` and
/// a `max_tokens` ceiling are required by the API; the system preamble is a
/// top-level field (not a message), per the Anthropic schema. `Role::System`
/// messages (slice 18: an applied prompt template) are therefore HOISTED out of
/// the array and appended to that field — the API rejects a `"system"` role in
/// `messages`.
pub fn build_body(req: &ChatRequest, max_tokens: u32) -> Value {
    let mut system_parts: Vec<&str> = Vec::new();
    if let Some(system) = &req.system {
        system_parts.push(system);
    }
    let current_user = req.messages.iter().rposition(|m| m.role == Role::User);
    let messages: Vec<Value> = req
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, m)| match m.role {
            Role::System => {
                system_parts.push(&m.content);
                None
            }
            Role::User => Some(
                json!({ "role": "user", "content": if Some(index) == current_user { awili_image_content::anthropic(&m.content) } else { json!(awili_image_content::split(&m.content).text) } }),
            ),
            Role::Assistant => Some(json!({ "role": "assistant", "content": m.content })),
        })
        .collect();
    let mut body = json!({
        "model": req.model,
        "max_tokens": max_tokens,
        "stream": true,
        "messages": messages,
    });
    if !system_parts.is_empty() {
        body["system"] = json!(system_parts.join("\n\n"));
    }
    body
}

/// Incremental SSE decoder for the Anthropic event stream.
#[derive(Default)]
pub struct AnthropicDecoder {
    lines: LineBuffer,
    /// Accumulated `data:` payload(s) for the event currently being assembled.
    data: String,
    /// `input_tokens` from `message_start`, held until `message_delta` reports
    /// `output_tokens` so a single [`Usage`] carries both.
    input_tokens: Option<u32>,
    done_emitted: bool,
}

impl AnthropicDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle one decoded SSE line, dispatching a completed event on a blank line.
    fn on_line(&mut self, line: &str, out: &mut Vec<ProviderEvent>) {
        if self.done_emitted {
            return; // a terminal (stop or error) frame already closed the stream
        }
        if line.is_empty() {
            // Blank line dispatches the assembled event.
            if !self.data.is_empty() {
                let data = std::mem::take(&mut self.data);
                self.dispatch(&data, out);
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            // One optional leading space after the colon, per the SSE spec.
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
        // `event:`, `id:`, `:`-comments and anything else are ignored — the JSON
        // `type` field is authoritative.
    }

    /// Parse one event's JSON payload and map it onto provider events.
    fn dispatch(&mut self, data: &str, out: &mut Vec<ProviderEvent>) {
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            // A malformed SSE frame is a protocol failure, not something to skip:
            // skipping silently drops tokens and lets a corrupt stream persist as a
            // "complete" reply (review SF1). Surface it as Error then close.
            out.push(ProviderEvent::Error(format!(
                "malformed Anthropic SSE frame: {}",
                truncate_for_msg(data)
            )));
            self.done_emitted = true;
            out.push(ProviderEvent::Done);
            return;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("content_block_delta") => {
                // Text generation: delta.type == "text_delta", delta.text.
                if let Some(text) = v.pointer("/delta/text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        out.push(ProviderEvent::Token(text.to_string()));
                    }
                }
            }
            Some("message_start") => {
                if let Some(n) = v
                    .pointer("/message/usage/input_tokens")
                    .and_then(Value::as_u64)
                {
                    self.input_tokens = Some(clamp_u32(n));
                }
            }
            Some("message_delta") => {
                if let Some(out_tok) = v.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                    out.push(ProviderEvent::Usage(Usage {
                        cached_input_tokens: None,
                        input_tokens: self.input_tokens.unwrap_or(0),
                        output_tokens: clamp_u32(out_tok),
                    }));
                }
            }
            Some("message_stop") => {
                self.done_emitted = true;
                out.push(ProviderEvent::Done);
            }
            Some("error") => {
                let msg = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown upstream error");
                // Surface the structured error `type` ahead of the prose message so
                // automatic fallback classification (classify_error) keys off the
                // authoritative type, not the message — message text isn't
                // standardized across the fleet, but the type is. A mid-stream
                // `overloaded_error` whose message omitted the word "overloaded"
                // would otherwise classify as Other (no retry) and strand the turn.
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
            }
            // ping, content_block_start, content_block_stop: nothing to emit.
            _ => {}
        }
    }
}

impl Decoder for AnthropicDecoder {
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
        // Flush a final line with no trailing newline, then a dangling event.
        if let Some(rem) = self.lines.take_remainder() {
            self.on_line(&rem, &mut out);
        }
        if !self.done_emitted && !self.data.is_empty() {
            let data = std::mem::take(&mut self.data);
            self.dispatch(&data, &mut out);
        }
        if !self.done_emitted {
            // EOF arrived before `message_stop`. Anthropic ALWAYS terminates with
            // `message_stop`, so a missing one means the connection was cut mid
            // stream — report it as an error rather than synthesising a clean Done
            // that would persist a truncated reply as complete (review SF1).
            self.done_emitted = true;
            out.push(ProviderEvent::Error(
                "Anthropic stream ended before message_stop (truncated)".to_string(),
            ));
            out.push(ProviderEvent::Done);
        }
        out
    }
}

/// Clip an offending frame so an error message stays readable in the UI/log.
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

    /// A representative Anthropic SSE stream (abridged from the real wire shape).
    const STREAM: &str = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: ping
data: {\"type\":\"ping\"}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\", world\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

    fn drain(decoder: &mut AnthropicDecoder, bytes: &[u8]) -> Vec<ProviderEvent> {
        let mut out = decoder.feed(bytes);
        out.extend(decoder.finish());
        out
    }

    #[test]
    fn decodes_full_stream_in_one_chunk() {
        let mut d = AnthropicDecoder::new();
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
    fn reassembles_tokens_across_byte_chunks() {
        // Split the stream mid-frame to prove the line/SSE buffering is robust.
        let bytes = STREAM.as_bytes();
        let mut d = AnthropicDecoder::new();
        let mut events = Vec::new();
        for chunk in bytes.chunks(7) {
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
        let mut d = AnthropicDecoder::new();
        let s = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let events = drain(&mut d, s.as_bytes());
        // The structured `type` is prefixed so classify_error keys off it.
        assert_eq!(
            events,
            vec![
                ProviderEvent::Error("overloaded_error: Overloaded".to_string()),
                ProviderEvent::Done
            ]
        );
    }

    #[test]
    fn error_frame_type_drives_classification() {
        use crate::{classify_error, ProviderErrorKind};
        // A fleet/mid-stream error whose prose message lacks any capacity keyword
        // must still be classified by its `type`, not stranded as Other (no retry).
        let mut d = AnthropicDecoder::new();
        let s =
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"server busy, try again\"}}\n\n";
        let events = drain(&mut d, s.as_bytes());
        let ProviderEvent::Error(msg) = &events[0] else {
            panic!("expected Error, got {events:?}");
        };
        assert_eq!(msg, "overloaded_error: server busy, try again");
        assert_eq!(classify_error(msg), ProviderErrorKind::Overloaded);
    }

    #[test]
    fn error_frame_without_type_keeps_bare_message() {
        // No `type` field -> the message is surfaced unchanged (no stray prefix).
        let mut d = AnthropicDecoder::new();
        let s = "event: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"boom\"}}\n\n";
        let events = drain(&mut d, s.as_bytes());
        assert_eq!(events[0], ProviderEvent::Error("boom".to_string()));
    }

    #[test]
    fn truncated_stream_errors_then_done() {
        // No message_stop arrives (connection cut mid-stream). The decoder must
        // NOT pretend the reply completed cleanly — it emits Error then Done so the
        // partial reply is not persisted as complete (review SF1).
        let mut d = AnthropicDecoder::new();
        let s = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n";
        let events = drain(&mut d, s.as_bytes());
        assert_eq!(events[0], ProviderEvent::Token("hi".to_string()));
        assert!(
            matches!(events[1], ProviderEvent::Error(_)),
            "expected Error, got {:?}",
            events[1]
        );
        assert_eq!(events[2], ProviderEvent::Done);
    }

    #[test]
    fn clean_eof_after_message_stop_is_just_done() {
        // The happy path: a proper message_stop means finish() adds NO error.
        let mut d = AnthropicDecoder::new();
        let events = drain(&mut d, STREAM.as_bytes());
        assert!(!events.iter().any(|e| matches!(e, ProviderEvent::Error(_))));
        assert_eq!(events.last(), Some(&ProviderEvent::Done));
    }

    #[test]
    fn malformed_json_frame_errors_then_done() {
        // A `data:` line carrying invalid JSON is a protocol failure: Error + Done,
        // and no further frames are processed.
        let mut d = AnthropicDecoder::new();
        let s = "data: {\"type\":\"content_block_delta\",  BROKEN }\n\ndata: {\"type\":\"message_stop\"}\n\n";
        let events = drain(&mut d, s.as_bytes());
        assert!(matches!(events[0], ProviderEvent::Error(_)));
        assert_eq!(events[1], ProviderEvent::Done);
        assert_eq!(
            events.len(),
            2,
            "no events after the terminal Done: {events:?}"
        );
    }

    #[test]
    fn unterminated_json_frame_errors() {
        // A frame whose JSON object is never closed (EOF mid-object) -> Error.
        let mut d = AnthropicDecoder::new();
        let s = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"\n\n";
        let events = drain(&mut d, s.as_bytes());
        assert!(
            matches!(events[0], ProviderEvent::Error(_)),
            "got {events:?}"
        );
        assert_eq!(events.last(), Some(&ProviderEvent::Done));
    }

    #[test]
    fn multibyte_token_split_across_chunks_is_intact() {
        // N1/SF1: a non-ASCII token whose UTF-8 bytes are split exactly between two
        // transport chunks must reassemble intact (no replacement char).
        let frame = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"日本語\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n";
        let bytes = frame.as_bytes();
        // Find a byte index in the middle of the first multibyte char (日 = 3 bytes).
        let split = frame.find('日').unwrap() + 1; // mid-codepoint
        let mut d = AnthropicDecoder::new();
        let mut events = d.feed(&bytes[..split]);
        events.extend(d.feed(&bytes[split..]));
        events.extend(d.finish());
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::Token(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "日本語");
        assert!(!text.contains('\u{FFFD}'), "no replacement char");
    }

    #[test]
    fn invalid_utf8_in_frame_errors_not_panics() {
        // Raw invalid UTF-8 bytes in the structural part of a frame become U+FFFD
        // via lossy decode, which breaks the JSON -> Error. Must never panic.
        let mut d = AnthropicDecoder::new();
        let mut bytes = b"data: {\"type\": ".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE, 0x80]); // invalid UTF-8, not in a string
        bytes.extend_from_slice(b"}\n\n");
        let events = drain(&mut d, &bytes);
        assert!(
            matches!(events[0], ProviderEvent::Error(_)),
            "got {events:?}"
        );
        assert_eq!(events.last(), Some(&ProviderEvent::Done));
    }

    #[test]
    fn build_body_places_system_top_level_and_omits_when_absent() {
        let req = ChatRequest {
            model: "claude-opus-4-8".to_string(),
            system: Some("Be brief.".to_string()),
            messages: vec![crate::event::ChatMessage::user("hi")],
        };
        let body = build_body(&req, 2048);
        assert_eq!(body["system"], json!("Be brief."));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["max_tokens"], json!(2048));
        assert_eq!(body["messages"][0]["role"], json!("user"));

        let no_sys = ChatRequest {
            system: None,
            ..req
        };
        let body = build_body(&no_sys, 2048);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn build_body_hoists_system_messages_out_of_the_array() {
        // Slice 18: a prepended `Role::System` message (an applied prompt
        // template) must reach the top-level `system` field — the Messages API
        // rejects a "system" role inside `messages`. With a base preamble
        // present the two concatenate, preamble first.
        let req = ChatRequest {
            model: "claude-opus-4-8".to_string(),
            system: Some("Standing directives.".to_string()),
            messages: vec![
                crate::event::ChatMessage {
                    role: Role::System,
                    content: "You are the foreman assistant.".to_string(),
                },
                crate::event::ChatMessage::user("hi"),
            ],
        };
        let body = build_body(&req, 2048);
        assert_eq!(
            body["system"],
            json!("Standing directives.\n\nYou are the foreman assistant.")
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "the system message must not stay in the array"
        );
        assert_eq!(msgs[0]["role"], json!("user"));

        // Without a base preamble the template alone becomes the system field.
        let only_template = ChatRequest {
            system: None,
            ..req
        };
        let body = build_body(&only_template, 2048);
        assert_eq!(body["system"], json!("You are the foreman assistant."));
    }

    #[test]
    fn build_body_never_emits_system_role_in_messages() {
        // Whatever mix of roles arrives, the payload's array holds only
        // user/assistant entries (the API rejects anything else).
        let req = ChatRequest {
            model: "claude-opus-4-8".to_string(),
            system: Some("base".to_string()),
            messages: vec![
                crate::event::ChatMessage {
                    role: Role::System,
                    content: "tpl".to_string(),
                },
                crate::event::ChatMessage::user("q"),
                crate::event::ChatMessage::assistant("a"),
                crate::event::ChatMessage::user("q2"),
            ],
        };
        let body = build_body(&req, 1024);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        for m in msgs {
            assert_ne!(m["role"], json!("system"));
        }
    }
}
