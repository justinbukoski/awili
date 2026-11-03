//! S7a (F0782) proof: every streaming decoder stops at `MAX_LINE_BYTES`.
//!
//! An endpoint that never sends a newline (or, for SSE, never ends an
//! event) must not grow memory without limit. Each decoder reports the
//! owner-visible `STREAM_TOO_LARGE` error and `Done` exactly once, then
//! ignores the rest of the body.

use awili_providers::{
    AnthropicDecoder, Decoder, OllamaDecoder, OpenAiDecoder, ProviderEvent, MAX_LINE_BYTES,
    STREAM_TOO_LARGE,
};

fn too_large() -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::Error(STREAM_TOO_LARGE.to_string()),
        ProviderEvent::Done,
    ]
}

/// Feed `prefix` then filler bytes with no newline until one byte past the
/// cap, in 64 KiB chunks. Returns every event the feeds produced.
fn feed_unterminated(d: &mut dyn Decoder, prefix: &[u8]) -> Vec<ProviderEvent> {
    let mut out = d.feed(prefix);
    let chunk = vec![b'x'; 64 * 1024];
    let mut sent = prefix.len();
    while sent <= MAX_LINE_BYTES {
        out.extend(d.feed(&chunk));
        sent += chunk.len();
    }
    out
}

fn decoders() -> Vec<(&'static str, Box<dyn Decoder>, &'static [u8])> {
    vec![
        ("anthropic", Box::new(AnthropicDecoder::new()), b"data: "),
        ("openai", Box::new(OpenAiDecoder::new()), b"data: "),
        (
            "ollama",
            Box::new(OllamaDecoder::new()),
            b"{\"message\":{\"content\":\"",
        ),
    ]
}

#[test]
fn s7a_unterminated_line_stops_every_decoder_once() {
    for (name, mut d, prefix) in decoders() {
        let out = feed_unterminated(d.as_mut(), prefix);
        assert_eq!(out, too_large(), "{name}: one Error + Done at the cap");
        // Later complete lines are ignored, and finish adds nothing.
        assert!(
            d.feed(b"\n\ndata: [DONE]\n\n").is_empty(),
            "{name}: input after the cap is dropped"
        );
        assert!(
            d.finish().is_empty(),
            "{name}: finish after the cap adds nothing"
        );
    }
}

#[test]
fn s7a_sse_event_assembled_from_many_lines_is_capped() {
    // Many `data:` lines, each under the line cap, never closed by a blank
    // line: the joined event passes the cap and the decoder stops.
    let line = {
        let mut l = b"data: ".to_vec();
        l.extend(vec![b'y'; 512 * 1024]);
        l.push(b'\n');
        l
    };
    let sse: Vec<(&str, Box<dyn Decoder>)> = vec![
        ("anthropic", Box::new(AnthropicDecoder::new())),
        ("openai", Box::new(OpenAiDecoder::new())),
    ];
    for (name, mut d) in sse {
        let mut out = Vec::new();
        for _ in 0..9 {
            out.extend(d.feed(&line));
        }
        assert_eq!(out, too_large(), "{name}: the assembled event is capped");
        assert!(
            d.feed(b"\n").is_empty(),
            "{name}: the blank line after the cap is ignored"
        );
        assert!(
            d.finish().is_empty(),
            "{name}: finish after the cap adds nothing"
        );
    }
}

#[test]
fn s7a_large_reply_under_the_cap_still_decodes() {
    // A 3 MiB token in one line is legitimate and must arrive intact.
    let text = "z".repeat(3 * 1024 * 1024);
    let frame = format!(
        "data: {{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}\n\n"
    );
    let mut d = AnthropicDecoder::new();
    let out = d.feed(frame.as_bytes());
    assert_eq!(out, vec![ProviderEvent::Token(text.clone())]);

    let line = format!("{{\"message\":{{\"content\":\"{text}\"}}}}\n");
    let mut d = OllamaDecoder::new();
    assert_eq!(d.feed(line.as_bytes()), vec![ProviderEvent::Token(text)]);
}
