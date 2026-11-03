//! B3 acceptance: decode the REAL captured streams (fixtures/) and drive the
//! reducer end-to-end — raw bytes to terminal state. This is the round-trip
//! the whole Phase A fixture effort exists to enable.

use std::fs;
use std::path::PathBuf;

use awili_engine::codec::chat_completions::decode_stream;
use awili_engine::{
    reduce, AgentEvent, EngineError, Input, NextAction, StopKind, TurnPhase, TurnState, WireDelta,
};

fn fixture(server: &str, name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/chat-completions")
        .join(server)
        .join(name);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("missing fixture {p:?}: {e}"))
}

fn drive_raw(state: &mut TurnState, raw: &str) -> (Vec<AgentEvent>, NextAction) {
    let mut events = Vec::new();
    let mut last = NextAction::None;
    for d in decode_stream(raw) {
        let (ev, na) = reduce(state, Input::Delta(d));
        events.extend(ev);
        last = na;
    }
    (events, last)
}

/// Real Ollama plain turn: reasoning + text + stop + usage + [DONE].
#[test]
fn ollama_plain_turn_end_to_end() {
    let raw = fixture("hostc-qwen3-8b", "01-plain-turn.response.sse");
    let mut s = TurnState::new("ollama", "qwen3:8b", "t1");
    let (events, next) = drive_raw(&mut s, &raw);
    assert_eq!(next, NextAction::Done);
    assert_eq!(*s.phase(), TurnPhase::Completed);
    assert_eq!(s.text(), "Paris");
    // Live capture facts: 22 prompt tokens, 177 completion tokens.
    match events.last().unwrap() {
        AgentEvent::TurnCompleted { stop, usage } => {
            assert_eq!(*stop, StopKind::EndTurn);
            let u = usage.unwrap();
            assert_eq!((u.prompt_tokens, u.completion_tokens), (22, 177));
        }
        other => panic!("unexpected {other:?}"),
    }
    // Reasoning was captured opaquely, not folded into text.
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::ContinuationBlobCaptured(_))));
}

/// Real Ollama single tool call (whole-in-one-chunk framing).
#[test]
fn ollama_tool_call_end_to_end() {
    let raw = fixture("hostc-qwen3-8b", "02-single-tool-call.response.sse");
    let mut s = TurnState::new("ollama", "qwen3:8b", "t2");
    let (_events, next) = drive_raw(&mut s, &raw);
    let NextAction::DispatchTools(calls) = next else {
        panic!("expected DispatchTools, got {next:?}");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
    let args: serde_json::Value = serde_json::from_str(&calls[0].arguments).unwrap();
    assert_eq!(args["path"], "notes.md");
    assert_eq!(*s.phase(), TurnPhase::AwaitingToolResults);
}

/// Real vLLM (Laguna) tool call — fragmented arguments reassemble.
#[test]
fn vllm_fragmented_tool_call_end_to_end() {
    let raw = fixture("hostf-vllm-laguna", "02-single-tool-call.response.sse");
    let mut s = TurnState::new("vllm", "laguna-s-2.1-nvfp4", "t3");
    let (_events, next) = drive_raw(&mut s, &raw);
    let NextAction::DispatchTools(calls) = next else {
        panic!("expected DispatchTools, got {next:?}");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
    let args: serde_json::Value = serde_json::from_str(&calls[0].arguments)
        .expect("fragmented args must reassemble into valid JSON");
    assert_eq!(args["path"], "notes.md");
    assert!(calls[0]
        .provider_call_id
        .as_deref()
        .unwrap()
        .starts_with("chatcmpl-tool-"));
}

/// Real Ollama parallel calls fixture (two get_weather calls).
#[test]
fn ollama_parallel_calls_end_to_end() {
    let raw = fixture("hostc-qwen3-8b", "03-parallel-tool-calls.response.sse");
    let mut s = TurnState::new("ollama", "qwen3:8b", "t4");
    let (_events, next) = drive_raw(&mut s, &raw);
    let NextAction::DispatchTools(calls) = next else {
        panic!("expected DispatchTools, got {next:?}");
    };
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.name == "get_weather"));
}

/// Real truncated stream (client cancel capture): with a Cancel input the
/// turn ends Cancelled; without one it fails typed.
#[test]
fn truncated_stream_end_to_end() {
    let raw = fixture("hostc-qwen3-8b", "05-cancellation.response.sse");

    let mut s = TurnState::new("ollama", "qwen3:8b", "t5");
    let _ = reduce(&mut s, Input::Cancel);
    let (_e, next) = drive_raw(&mut s, &raw);
    assert_eq!(next, NextAction::Done);
    assert_eq!(*s.phase(), TurnPhase::Cancelled);

    let mut s2 = TurnState::new("ollama", "qwen3:8b", "t6");
    let (events, _) = drive_raw(&mut s2, &raw);
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnFailed(EngineError::UnexpectedTruncation { .. })
    ));
}

/// SYNTHETIC malformed frame MID-stream (valid frames + [DONE] after it):
/// decoder fails typed, reducer fails the turn. Fixture 06b, authored
/// 2026-08-13 — the original 06 turned out to be truncation-shaped (below).
#[test]
fn malformed_frame_end_to_end() {
    let raw = fixture(
        "hostc-qwen3-8b",
        "06b-malformed-midstream.SYNTHETIC.response.sse",
    );
    let mut s = TurnState::new("ollama", "qwen3:8b", "t7");
    let (events, next) = drive_raw(&mut s, &raw);
    assert_eq!(next, NextAction::Done);
    assert_eq!(*s.phase(), TurnPhase::Failed);
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnFailed(EngineError::Provider {
            retryable: false,
            ..
        })
    ));
}

/// RECLASSIFIED 2026-08-13: fixture 06 ends in an unterminated JSON string
/// with no frames and no [DONE] after it — byte-identical in shape to the
/// REAL lightning `cancellation` capture (client cancel torn at the 4KB
/// boundary). A torn tail is truncation, not provider garbage: blaming the
/// server for the client's hangup would send the reducer down the failed
/// path instead of the cancelled/truncated path. 06 stays immutable as the
/// torn-tail regression fixture; 06b (above) carries the true malformed case.
#[test]
fn torn_tail_is_truncation_not_provider_error() {
    let raw = fixture(
        "hostc-qwen3-8b",
        "06-malformed-tool-json.SYNTHETIC.response.sse",
    );
    let mut s = TurnState::new("ollama", "qwen3:8b", "t7b");
    let (events, _) = drive_raw(&mut s, &raw);
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnFailed(EngineError::UnexpectedTruncation { .. })
    ));
}

/// REAL provider error body (Ollama OOM, non-SSE JSON).
#[test]
fn provider_error_body_end_to_end() {
    let raw = fixture("qwen3-coder-30b", "08-provider-error-oom.response.json");
    let deltas = decode_stream(&raw);
    assert_eq!(deltas.len(), 1);
    match &deltas[0] {
        WireDelta::ProviderError { message, retryable } => {
            assert!(message.contains("more system memory"));
            assert!(!retryable);
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// vLLM plain turn: reasoning deltas + finish=length + usage; nonstandard
/// fields (stop_reason, token_ids, system_fingerprint) must be ignored.
#[test]
fn vllm_plain_turn_end_to_end() {
    let raw = fixture("hostf-vllm-laguna", "01-plain-turn.response.sse");
    let mut s = TurnState::new("vllm", "laguna-s-2.1-nvfp4", "t8");
    let (events, next) = drive_raw(&mut s, &raw);
    assert_eq!(next, NextAction::Done);
    assert_eq!(*s.phase(), TurnPhase::Completed);
    match events.last().unwrap() {
        AgentEvent::TurnCompleted { stop, usage } => {
            assert_eq!(*stop, StopKind::Length); // capture hit max_tokens mid-reasoning
            assert_eq!(usage.unwrap().prompt_tokens, 53);
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// Real continuation capture (fixture 04): full two-segment turn — tool call
/// dispatched from fixture 02's decode, result fed back, then fixture 04's
/// captured continuation stream completes the turn (Codex B3 review P2).
#[test]
fn tool_result_continuation_end_to_end() {
    use awili_engine::events::ToolResultRecord;
    let mut s = TurnState::new("ollama", "qwen3:8b", "t9");
    let (_e, next) = drive_raw(
        &mut s,
        &fixture("hostc-qwen3-8b", "02-single-tool-call.response.sse"),
    );
    let NextAction::DispatchTools(calls) = next else {
        panic!("expected DispatchTools");
    };
    let (_ev, na) = reduce(
        &mut s,
        Input::ToolResult(ToolResultRecord {
            awili_call_id: calls[0].awili_call_id.clone(),
            content: "Buy milk. Ship the engine. Call mom.".into(),
            is_error: false,
        }),
    );
    assert_eq!(na, NextAction::RequestContinuation);
    let (_e2, next2) = drive_raw(
        &mut s,
        &fixture("hostc-qwen3-8b", "04-tool-continuation.response.sse"),
    );
    assert_eq!(next2, NextAction::Done);
    assert_eq!(*s.phase(), TurnPhase::Completed);
    assert!(
        !s.text().is_empty(),
        "continuation text must land in transcript"
    );
}

/// Codex B3 review P1: an in-stream SSE error event must not vanish into
/// unknown-field tolerance.
#[test]
fn sse_error_event_is_surfaced() {
    let raw = "data: {\"error\":{\"message\":\"overloaded\",\"type\":\"server_error\"}}\n\ndata: [DONE]\n";
    let deltas = decode_stream(raw);
    assert_eq!(deltas.len(), 1);
    assert!(matches!(
        &deltas[0],
        WireDelta::ProviderError { message, retryable: true } if message == "overloaded"
    ));
}

/// Codex B3 review P1: multi-choice (n>1) is rejected typed, not merged.
#[test]
fn multi_choice_is_rejected() {
    let raw = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}},{\"index\":1,\"delta\":{\"content\":\"b\"}}]}\n";
    let deltas = decode_stream(raw);
    assert!(deltas.iter().any(|d| matches!(
        d,
        WireDelta::ProviderError { message, .. } if message.contains("multi-choice")
    )));
}

/// Codex B3 review P1: comment keepalives / event: fields before the first
/// data: line must not trip non-SSE detection.
#[test]
fn comment_prefixed_stream_is_sse() {
    let raw = ": ping\n\nevent: message\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n";
    let deltas = decode_stream(raw);
    assert!(deltas.contains(&WireDelta::Text("hi".into())));
    assert_eq!(*deltas.last().unwrap(), WireDelta::StreamEnd);
}

/// Codex B3 review P2: multi-line data fields concatenate per SSE spec.
#[test]
fn multiline_data_field_assembles() {
    // One event, JSON split across two data: lines (legal SSE).
    let raw = "data: {\"choices\":[{\"index\":0,\"delta\":\ndata: {\"content\":\"joined\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n";
    let deltas = decode_stream(raw);
    assert!(
        deltas.contains(&WireDelta::Text("joined".into())),
        "got {deltas:?}"
    );
}

/// Codex B3 review P2: pretty-printed non-SSE error bodies parse.
#[test]
fn pretty_printed_error_body_parses() {
    let raw = "{\n  \"error\": {\n    \"message\": \"boom\",\n    \"type\": \"api_error\"\n  }\n}";
    let deltas = decode_stream(raw);
    assert!(matches!(
        &deltas[0],
        WireDelta::ProviderError { message, retryable: false } if message == "boom"
    ));
}

/// P7 REAL capture: context overflow = empty length-stop, exactly-at-ceiling
/// usage. Pins the wire fact C4's compaction trigger depends on.
#[test]
fn context_limit_end_to_end() {
    let raw = fixture("hostc-qwen3-8b", "07-context-limit.response.sse");
    let mut s = TurnState::new("ollama", "qwen3:8b", "t10");
    let (events, next) = drive_raw(&mut s, &raw);
    assert_eq!(next, NextAction::Done);
    assert_eq!(*s.phase(), TurnPhase::Completed);
    assert_eq!(s.text(), "", "overflow produces zero content");
    match events.last().unwrap() {
        AgentEvent::TurnCompleted { stop, usage } => {
            assert_eq!(*stop, StopKind::Length);
            let u = usage.unwrap();
            assert_eq!(u.prompt_tokens + u.completion_tokens, 40960); // the ceiling
        }
        other => panic!("unexpected {other:?}"),
    }
}
