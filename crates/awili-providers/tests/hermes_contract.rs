//! Hermes contract tests (slice-5; see `docs/hermes-contract.md`).
//!
//! Hermes-the-runtime does not exist yet, so these tests assert the property the
//! whole "Hermes leg" depends on: **Hermes is Anthropic-compatible**, therefore
//! Awili's existing Anthropic SSE decoder already handles a Hermes-shaped stream —
//! including the toolful events Hermes adds (`tool_use`, `tool_result`,
//! `permission_request`), which Awili v1 must tolerate (ignore) without breaking
//! the text/usage/done it does render, and a mid-stream `error` frame.
//!
//! When the Hermes runtime is built, point it at these same shapes to prove the
//! contract holds end-to-end. If Hermes ever drifts from Anthropic-compat, one of
//! these fails before it reaches Owner.

use awili_providers::{AnthropicDecoder, Decoder, ProviderEvent, Usage};

fn drain(stream: &str) -> Vec<ProviderEvent> {
    let mut d = AnthropicDecoder::new();
    let mut out = d.feed(stream.as_bytes());
    out.extend(d.finish());
    out
}

/// A plain Anthropic-shaped reply served by Hermes decodes to text + usage + done.
#[test]
fn hermes_plain_text_stream_is_anthropic_compatible() {
    let stream = "\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20}}}

data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi from \"}}

data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hermes\"}}

data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}

data: {\"type\":\"message_stop\"}

";
    let events = drain(stream);
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Token(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hi from Hermes");
    assert!(events.contains(&ProviderEvent::Usage(Usage {
        cached_input_tokens: None,
        input_tokens: 20,
        output_tokens: 5
    })));
    assert!(events.contains(&ProviderEvent::Done));
}

/// Hermes's toolful events are interleaved with text. Awili v1 ignores them and
/// still renders the surrounding text cleanly, ending in Done. This is the
/// property that lets the Hermes leg ship before the permission UI exists.
#[test]
fn hermes_tool_events_are_tolerated_not_fatal() {
    let stream = "\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me check. \"}}

data: {\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read_file\",\"input\":{\"path\":\"/x\"}}

data: {\"type\":\"permission_request\",\"id\":\"pr_1\",\"action\":\"fs_read\",\"scope\":\"/x\"}

data: {\"type\":\"tool_result\",\"tool_use_id\":\"tu_1\",\"ok\":true,\"summary\":\"42 lines\"}

data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Done.\"}}

data: {\"type\":\"message_stop\"}

";
    let events = drain(stream);
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Token(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "Let me check. Done.",
        "tool/permission events must not corrupt text"
    );
    assert!(
        events.contains(&ProviderEvent::Done),
        "stream must still terminate cleanly"
    );
    // No event should be an Error — unknown frame types are ignored, not errors.
    assert!(
        !events.iter().any(|e| matches!(e, ProviderEvent::Error(_))),
        "unknown Hermes event types must be ignored, not raised as errors"
    );
}

/// A mid-stream Hermes `error` frame surfaces as an Error event followed by Done,
/// exactly like any Anthropic provider error.
#[test]
fn hermes_mid_stream_error_surfaces_then_done() {
    let stream = "\
data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Partial\"}}

data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Hermes overloaded\"}}

";
    let events = drain(stream);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Error(m) if m.contains("overloaded"))),
        "mid-stream error must surface: {events:?}"
    );
    assert!(
        events.contains(&ProviderEvent::Done),
        "error must be followed by Done"
    );
}
