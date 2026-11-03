//! Ollama `/api/chat`: a stream of newline-delimited JSON objects, one per
//! generated chunk, ending with an object whose `done` is `true` that also
//! carries the token counts.

use serde_json::{json, Value};

use crate::decode::{push_too_large, Decoder, LineBuffer};
use crate::event::{clamp_u32, ChatRequest, ProviderEvent, Role, Usage};

/// Build the JSON request body for Ollama `/api/chat`. The system preamble is the
/// leading message with role `system` (Ollama's convention), followed by the
/// conversation turns. `stream: true` yields the NDJSON token stream.
pub fn build_body(req: &ChatRequest) -> Value {
    let mut messages: Vec<Value> = Vec::with_capacity(req.messages.len() + 1);
    if let Some(system) = &req.system {
        messages.push(json!({ "role": "system", "content": system }));
    }
    // Historical attachments keep their textual placeholder. Resending them
    // into every future turn poisons a later switch to a text-only model.
    let current_user = req.messages.iter().rposition(|m| m.role == Role::User);
    for (index, m) in req.messages.iter().enumerate() {
        let c = if m.role == Role::User {
            let mut content = awili_image_content::split(&m.content);
            if Some(index) != current_user {
                content.images.clear();
            }
            content
        } else {
            awili_image_content::Content {
                text: m.content.clone(),
                images: Vec::new(),
            }
        };
        let mut message = json!({"role":match m.role {Role::System=>"system",Role::User=>"user",Role::Assistant=>"assistant"},"content":c.text});
        if !c.images.is_empty() {
            use base64::Engine;
            message["images"] = json!(c
                .images
                .iter()
                .map(|(_, b)| base64::engine::general_purpose::STANDARD.encode(b))
                .collect::<Vec<_>>());
        }
        messages.push(message);
    }
    json!({
        "model": req.model,
        "stream": true,
        "messages": messages,
    })
}

/// Incremental NDJSON decoder for the Ollama chat stream.
#[derive(Default)]
pub struct OllamaDecoder {
    lines: LineBuffer,
    done_emitted: bool,
}

impl OllamaDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    fn on_line(&mut self, line: &str, out: &mut Vec<ProviderEvent>) {
        if self.done_emitted {
            return; // a terminal (done or error) object already closed the stream
        }
        if line.trim().is_empty() {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            // A malformed NDJSON line is a protocol failure: surface it as Error +
            // Done rather than silently dropping the chunk (review SF1).
            out.push(ProviderEvent::Error(format!(
                "malformed Ollama NDJSON line: {}",
                truncate_for_msg(line)
            )));
            self.done_emitted = true;
            out.push(ProviderEvent::Done);
            return;
        };
        // An object with an `error` field is a fatal upstream error.
        if let Some(err) = v.get("error").and_then(Value::as_str) {
            out.push(ProviderEvent::Error(err.to_string()));
            self.done_emitted = true;
            out.push(ProviderEvent::Done);
            return;
        }
        if let Some(content) = v.pointer("/message/content").and_then(Value::as_str) {
            if !content.is_empty() {
                out.push(ProviderEvent::Token(content.to_string()));
            }
        }
        if v.get("done").and_then(Value::as_bool) == Some(true) {
            let input_tokens = clamp_u32(
                v.get("prompt_eval_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
            let output_tokens = clamp_u32(v.get("eval_count").and_then(Value::as_u64).unwrap_or(0));
            out.push(ProviderEvent::Usage(Usage {
                cached_input_tokens: None,
                input_tokens,
                output_tokens,
            }));
            self.done_emitted = true;
            out.push(ProviderEvent::Done);
        }
    }
}

impl Decoder for OllamaDecoder {
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
        if !self.done_emitted {
            // EOF before the `done: true` object: Ollama always sends one, so its
            // absence means the stream was cut. Report Error then Done so a partial
            // reply is not persisted as complete (review SF1).
            self.done_emitted = true;
            out.push(ProviderEvent::Error(
                "Ollama stream ended before a done:true object (truncated)".to_string(),
            ));
            out.push(ProviderEvent::Done);
        }
        out
    }
}

/// Clip an offending line so an error message stays readable in the UI/log.
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
    use crate::event::ChatMessage;

    const STREAM: &str = concat!(
        "{\"model\":\"llama3.1\",\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"done\":false}\n",
        "{\"model\":\"llama3.1\",\"message\":{\"role\":\"assistant\",\"content\":\"lo\"},\"done\":false}\n",
        "{\"model\":\"llama3.1\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":26,\"eval_count\":8}\n",
    );

    fn drain(d: &mut OllamaDecoder, bytes: &[u8]) -> Vec<ProviderEvent> {
        let mut out = d.feed(bytes);
        out.extend(d.finish());
        out
    }

    #[test]
    fn decodes_ndjson_stream() {
        let mut d = OllamaDecoder::new();
        let events = drain(&mut d, STREAM.as_bytes());
        assert_eq!(
            events,
            vec![
                ProviderEvent::Token("Hel".to_string()),
                ProviderEvent::Token("lo".to_string()),
                ProviderEvent::Usage(Usage {
                    cached_input_tokens: None,
                    input_tokens: 26,
                    output_tokens: 8
                }),
                ProviderEvent::Done,
            ]
        );
    }

    #[test]
    fn reassembles_across_chunks() {
        let mut d = OllamaDecoder::new();
        let mut events = Vec::new();
        for chunk in STREAM.as_bytes().chunks(5) {
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
        assert_eq!(text, "Hello");
    }

    #[test]
    fn error_object_emits_error_then_done() {
        let mut d = OllamaDecoder::new();
        let events = drain(&mut d, b"{\"error\":\"model 'x' not found\"}\n");
        assert_eq!(
            events,
            vec![
                ProviderEvent::Error("model 'x' not found".to_string()),
                ProviderEvent::Done,
            ]
        );
    }

    #[test]
    fn truncated_stream_errors_then_done() {
        // No `done:true` object arrives (connection cut). The decoder must report
        // the truncation rather than synthesise a clean completion (review SF1).
        let mut d = OllamaDecoder::new();
        let events = drain(
            &mut d,
            b"{\"message\":{\"content\":\"hi\"},\"done\":false}\n",
        );
        assert_eq!(events[0], ProviderEvent::Token("hi".to_string()));
        assert!(
            matches!(events[1], ProviderEvent::Error(_)),
            "got {events:?}"
        );
        assert_eq!(events[2], ProviderEvent::Done);
    }

    #[test]
    fn clean_done_object_has_no_error() {
        let mut d = OllamaDecoder::new();
        let events = drain(&mut d, STREAM.as_bytes());
        assert!(!events.iter().any(|e| matches!(e, ProviderEvent::Error(_))));
        assert_eq!(events.last(), Some(&ProviderEvent::Done));
    }

    #[test]
    fn malformed_line_errors_then_done() {
        let mut d = OllamaDecoder::new();
        let events = drain(&mut d, b"{not valid json}\n{\"done\":true}\n");
        assert!(matches!(events[0], ProviderEvent::Error(_)));
        assert_eq!(events[1], ProviderEvent::Done);
        assert_eq!(events.len(), 2, "no events after terminal Done: {events:?}");
    }

    #[test]
    fn unterminated_json_line_errors() {
        // A final line that is an unclosed JSON object (EOF mid-object) -> Error.
        let mut d = OllamaDecoder::new();
        let events = drain(&mut d, b"{\"message\":{\"content\":\"hi\"}");
        assert!(
            matches!(events[0], ProviderEvent::Error(_)),
            "got {events:?}"
        );
        assert_eq!(events.last(), Some(&ProviderEvent::Done));
    }

    #[test]
    fn multibyte_content_split_across_chunks_is_intact() {
        // N1/SF1: a multibyte token split between transport chunks reassembles.
        let line = "{\"message\":{\"content\":\"日本語\"},\"done\":false}\n{\"done\":true}\n";
        let bytes = line.as_bytes();
        let split = line.find('日').unwrap() + 1; // mid-codepoint
        let mut d = OllamaDecoder::new();
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
        assert!(!text.contains('\u{FFFD}'));
    }

    #[test]
    fn invalid_utf8_line_errors_not_panics() {
        let mut d = OllamaDecoder::new();
        let mut bytes = b"{\"message\": ".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE, 0x80]); // invalid UTF-8, not in a string
        bytes.extend_from_slice(b"}\n");
        let events = drain(&mut d, &bytes);
        assert!(
            matches!(events[0], ProviderEvent::Error(_)),
            "got {events:?}"
        );
        assert_eq!(events.last(), Some(&ProviderEvent::Done));
    }

    #[test]
    fn build_body_maps_system_role_message_in_place() {
        // Slice 18: an applied template arrives as a leading `Role::System`
        // message; Ollama's format takes it verbatim as a "system"-role entry.
        let req = ChatRequest {
            model: "llama3.1".to_string(),
            system: None,
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: "you are the foreman".to_string(),
                },
                ChatMessage::user("hi"),
            ],
        };
        let body = build_body(&req);
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert_eq!(body["messages"][0]["content"], json!("you are the foreman"));
        assert_eq!(body["messages"][1]["role"], json!("user"));
    }

    #[test]
    fn build_body_prepends_system_message() {
        let req = ChatRequest {
            model: "llama3.1".to_string(),
            system: Some("rules".to_string()),
            messages: vec![ChatMessage::user("hi")],
        };
        let body = build_body(&req);
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert_eq!(body["messages"][0]["content"], json!("rules"));
        assert_eq!(body["messages"][1]["role"], json!("user"));
        assert_eq!(body["stream"], json!(true));
    }
}
