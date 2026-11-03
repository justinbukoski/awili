//! B1 acceptance: reducer behavior on sequences hand-derived from the A2
//! live captures (crates/awili-engine/fixtures/). Each test names its fixture.
//! The driver records the FULL transition trace (Codex review P2): every
//! (events, action) step is asserted, not just the last.

use awili_engine::events::{AwiliCallId, ToolResultRecord};
use awili_engine::{
    reduce, AgentEvent, EngineError, FinishReason, Input, NextAction, StopKind, TurnPhase,
    TurnState, Usage, WireDelta,
};

fn d(w: WireDelta) -> Input {
    Input::Delta(w)
}

fn result(id: &AwiliCallId, content: &str) -> Input {
    Input::ToolResult(ToolResultRecord {
        awili_call_id: id.clone(),
        content: content.into(),
        is_error: false,
    })
}

/// Full trace: one (events, action) pair per input.
fn drive(state: &mut TurnState, inputs: Vec<Input>) -> Vec<(Vec<AgentEvent>, NextAction)> {
    inputs.into_iter().map(|i| reduce(state, i)).collect()
}

fn all_events(trace: &[(Vec<AgentEvent>, NextAction)]) -> Vec<AgentEvent> {
    trace.iter().flat_map(|(e, _)| e.clone()).collect()
}

/// Every non-final step must be side-effect-free (NextAction::None) — catches
/// premature Done/Dispatch the old driver concealed.
fn assert_quiet_until_last(trace: &[(Vec<AgentEvent>, NextAction)]) {
    for (i, (_, na)) in trace.iter().enumerate().take(trace.len() - 1) {
        assert_eq!(
            *na,
            NextAction::None,
            "unexpected action at step {i}: {na:?}"
        );
    }
}

/// Fixture hostc-qwen3-8b/01: reasoning deltas, then content, finish=stop,
/// trailing usage chunk, [DONE].
#[test]
fn plain_turn_with_reasoning_and_usage() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t1");
    let trace = drive(
        &mut s,
        vec![
            d(WireDelta::Reasoning("Okay".into())),
            d(WireDelta::Reasoning(", the user asks".into())),
            d(WireDelta::Text("Paris".into())),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::Stop,
            }),
            d(WireDelta::Usage {
                prompt_tokens: 22,
                completion_tokens: 177,
            }),
            d(WireDelta::StreamEnd),
        ],
    );
    assert_quiet_until_last(&trace);
    assert_eq!(trace.last().unwrap().1, NextAction::Done);
    assert_eq!(*s.phase(), TurnPhase::Completed);

    let events = all_events(&trace);
    assert_eq!(events[0], AgentEvent::TurnStarted);
    assert!(matches!(events[1], AgentEvent::ReasoningDelta(_)));
    assert!(events
        .iter()
        .any(|e| *e == AgentEvent::TextDelta("Paris".into())));
    // Per-segment usage event emitted.
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::UsageReported(_))));
    // Reasoning became an opaque blob with correct provenance.
    let blob = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ContinuationBlobCaptured(b) => Some(b),
            _ => None,
        })
        .expect("reasoning must be captured as continuation blob");
    assert_eq!(blob.provenance.model, "qwen3:8b");
    assert_eq!(blob.payload, b"Okay, the user asks".to_vec());
    assert_eq!(blob.kind, awili_engine::ContinuationKind::Reasoning);
    assert!(blob.replayable_to("ollama", "qwen3:8b"));
    assert!(!blob.replayable_to("ollama", "some-other-model"));
    match events.last().unwrap() {
        AgentEvent::TurnCompleted { stop, usage } => {
            assert_eq!(*stop, StopKind::EndTurn);
            assert_eq!(usage.unwrap().completion_tokens, 177);
        }
        other => panic!("unexpected terminal event {other:?}"),
    }
    assert_eq!(s.text(), "Paris");
}

/// Fixture hostc-qwen3-8b/02: Ollama sends the ENTIRE call in one fragment.
#[test]
fn ollama_whole_chunk_tool_call_dispatches() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t2");
    let trace = drive(
        &mut s,
        vec![
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 0,
                id: Some("call_e0k4yozm".into()),
                name: Some("read_file".into()),
                args_fragment: "{\"path\":\"notes.md\"}".into(),
            }),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::ToolCalls,
            }),
            d(WireDelta::Usage {
                prompt_tokens: 100,
                completion_tokens: 20,
            }),
            d(WireDelta::StreamEnd),
        ],
    );
    assert_quiet_until_last(&trace);
    let NextAction::DispatchTools(calls) = &trace.last().unwrap().1 else {
        panic!("expected DispatchTools");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].provider_call_id.as_deref(), Some("call_e0k4yozm"));
    // Turn-scoped call ids (Codex P1: no cross-turn collisions).
    assert!(calls[0].awili_call_id.0.starts_with("t2-call-"));
    assert_eq!(*s.phase(), TurnPhase::AwaitingToolResults);
}

/// Fixture hostf-vllm-laguna/02 + Codex P2: TWO calls with INTERLEAVED
/// argument shards, out-of-order completion, sparse indices, conflicting
/// repeated metadata (first-wins).
#[test]
fn interleaved_fragmented_multi_call_assembles() {
    let mut s = TurnState::new("vllm", "laguna-s-2.1-nvfp4", "t3");
    let trace = drive(
        &mut s,
        vec![
            // Call at sparse index 2 opens first.
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 2,
                id: Some("prov-b".into()),
                name: Some("get_weather".into()),
                args_fragment: "{\"ci".into(),
            }),
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 0,
                id: Some("prov-a".into()),
                name: Some("read_file".into()),
                args_fragment: String::new(),
            }),
            // Interleaved shards, alternating indices.
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 0,
                id: None,
                name: None,
                args_fragment: "{\"pa".into(),
            }),
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 2,
                id: None,
                // Conflicting repeated name — first wins.
                name: Some("EVIL_RENAME".into()),
                args_fragment: "ty\":\"Tokyo\"}".into(),
            }),
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 0,
                id: None,
                name: None,
                args_fragment: "th\":\"notes.md\"}".into(),
            }),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::ToolCalls,
            }),
            d(WireDelta::StreamEnd),
        ],
    );
    assert_quiet_until_last(&trace);
    let NextAction::DispatchTools(calls) = &trace.last().unwrap().1 else {
        panic!("expected DispatchTools");
    };
    assert_eq!(calls.len(), 2);
    // BTreeMap ordering: index 0 first, sparse index 2 second.
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments, "{\"path\":\"notes.md\"}");
    assert_eq!(calls[1].name, "get_weather"); // first-wins, not EVIL_RENAME
    assert_eq!(calls[1].arguments, "{\"city\":\"Tokyo\"}");
    assert_eq!(calls[1].provider_call_id.as_deref(), Some("prov-b"));
}

/// Fixtures 03+04: parallel calls, results (content preserved), continuation
/// segment, aggregated usage across segments.
#[test]
fn parallel_calls_results_continuation_and_usage_aggregation() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t4");
    let trace1 = drive(
        &mut s,
        vec![
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 0,
                id: Some("a".into()),
                name: Some("get_weather".into()),
                args_fragment: "{\"city\":\"Tokyo\"}".into(),
            }),
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 1,
                id: Some("b".into()),
                name: Some("get_weather".into()),
                args_fragment: "{\"city\":\"Paris\"}".into(),
            }),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::ToolCalls,
            }),
            d(WireDelta::Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
            }),
            d(WireDelta::StreamEnd),
        ],
    );
    let NextAction::DispatchTools(calls) = &trace1.last().unwrap().1 else {
        panic!("expected DispatchTools");
    };
    let ids: Vec<AwiliCallId> = calls.iter().map(|c| c.awili_call_id.clone()).collect();

    // First result: still waiting; content is preserved in state + event.
    let (ev1, na1) = reduce(&mut s, result(&ids[0], "22C sunny"));
    assert_eq!(na1, NextAction::None);
    match &ev1[0] {
        AgentEvent::ToolResultIngested(r) => assert_eq!(r.content, "22C sunny"),
        other => panic!("unexpected {other:?}"),
    }

    // Second result: continuation requested.
    let (_ev2, na2) = reduce(&mut s, result(&ids[1], "18C rain"));
    assert_eq!(na2, NextAction::RequestContinuation);
    assert_eq!(*s.phase(), TurnPhase::ReadyToContinue);
    assert_eq!(s.results().len(), 2);

    // Continuation segment with its own usage chunk.
    let trace2 = drive(
        &mut s,
        vec![
            d(WireDelta::Text("Sunny in Tokyo, rainy in Paris.".into())),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::Stop,
            }),
            d(WireDelta::Usage {
                prompt_tokens: 40,
                completion_tokens: 12,
            }),
            d(WireDelta::StreamEnd),
        ],
    );
    assert_eq!(trace2.last().unwrap().1, NextAction::Done);
    assert_eq!(*s.phase(), TurnPhase::Completed);
    // TurnStarted fires exactly once per turn.
    let events2 = all_events(&trace2);
    assert!(!events2.iter().any(|e| matches!(e, AgentEvent::TurnStarted)));
    // Usage aggregated across both segments (Codex P2).
    assert_eq!(
        s.usage_total(),
        Some(Usage {
            prompt_tokens: 50,
            completion_tokens: 17
        })
    );
}

/// Fixture hostc-qwen3-8b/06 (SYNTHETIC): truncated argument JSON must fail
/// typed (P6) — an invalid-call event, never a panic.
#[test]
fn malformed_tool_args_fail_typed() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t5");
    let trace = drive(
        &mut s,
        vec![
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 0,
                id: Some("call_x".into()),
                name: Some("read_file".into()),
                args_fragment: "{\"path\":\"no".into(), // truncated
            }),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::ToolCalls,
            }),
            d(WireDelta::StreamEnd),
        ],
    );
    let events = all_events(&trace);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCallInvalid { reason, .. } if reason.contains("not valid JSON")
    )));
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnFailed(EngineError::Protocol(_))
    ));
    assert_eq!(trace.last().unwrap().1, NextAction::Done);
}

/// Fixture hostc-qwen3-8b/05: client cancel = truncation after Cancel input.
#[test]
fn cancellation_yields_cancelled_not_failed() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t6");
    drive(
        &mut s,
        vec![
            d(WireDelta::Reasoning("Okay".into())),
            Input::Cancel,
            d(WireDelta::StreamTruncated),
        ],
    );
    assert_eq!(*s.phase(), TurnPhase::Cancelled);
}

/// Truncation WITHOUT a cancel is a typed failure (P5/P8).
#[test]
fn unexpected_truncation_fails_typed() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t7");
    let trace = drive(
        &mut s,
        vec![
            d(WireDelta::Text("par".into())),
            d(WireDelta::StreamTruncated),
        ],
    );
    assert!(matches!(
        all_events(&trace).last().unwrap(),
        AgentEvent::TurnFailed(EngineError::UnexpectedTruncation { .. })
    ));
}

/// Fixture qwen3-coder-30b/08: real provider OOM error body.
#[test]
fn provider_error_fails_typed() {
    let mut s = TurnState::new("ollama", "qwen3-coder:30b", "t8");
    let trace = drive(
        &mut s,
        vec![d(WireDelta::ProviderError {
            message: "model requires more system memory (34.3 GiB) than is available (8.4 GiB)"
                .into(),
            retryable: false,
        })],
    );
    assert!(matches!(
        all_events(&trace).last().unwrap(),
        AgentEvent::TurnFailed(EngineError::Provider {
            retryable: false,
            ..
        })
    ));
}

/// Codex P1: a second Finish is a protocol failure, never a silent overwrite.
#[test]
fn double_finish_fails_typed() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t9");
    let trace = drive(
        &mut s,
        vec![
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 0,
                id: Some("a".into()),
                name: Some("read_file".into()),
                args_fragment: "{}".into(),
            }),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::ToolCalls,
            }),
            // Conflicting second finish trying to flip tool_calls -> stop.
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::Stop,
            }),
        ],
    );
    assert!(matches!(
        all_events(&trace).last().unwrap(),
        AgentEvent::TurnFailed(EngineError::Protocol(_))
    ));
    assert_eq!(*s.phase(), TurnPhase::Failed);
}

/// Codex P2: a Usage chunk as the FIRST frame must not fabricate a turn.
#[test]
fn phantom_usage_before_content_fails_typed() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t10");
    let trace = drive(
        &mut s,
        vec![d(WireDelta::Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
        })],
    );
    let events = all_events(&trace);
    assert!(!events.iter().any(|e| matches!(e, AgentEvent::TurnStarted)));
    assert!(matches!(
        events.last().unwrap(),
        AgentEvent::TurnFailed(EngineError::Protocol(_))
    ));
}

/// Codex P1: duplicate/unknown tool results are auditable rejections that do
/// NOT torch a live turn, and results after terminal are rejected visibly.
#[test]
fn duplicate_and_late_tool_results_are_rejected_auditably() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t11");
    let trace = drive(
        &mut s,
        vec![
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 0,
                id: Some("a".into()),
                name: Some("read_file".into()),
                args_fragment: "{}".into(),
            }),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::ToolCalls,
            }),
            d(WireDelta::StreamEnd),
        ],
    );
    let NextAction::DispatchTools(calls) = &trace.last().unwrap().1 else {
        panic!("expected DispatchTools");
    };
    let id = calls[0].awili_call_id.clone();

    // Real result: accepted.
    let (_, na) = reduce(&mut s, result(&id, "contents"));
    assert_eq!(na, NextAction::RequestContinuation);

    // Duplicate: rejected, turn NOT failed.
    let (ev, na) = reduce(&mut s, result(&id, "contents again"));
    assert!(matches!(ev[0], AgentEvent::InputRejected { .. }));
    assert_eq!(na, NextAction::None);
    assert_eq!(*s.phase(), TurnPhase::ReadyToContinue);

    // Finish the turn, then send another late result: rejected visibly.
    drive(
        &mut s,
        vec![
            d(WireDelta::Text("done".into())),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::Stop,
            }),
            d(WireDelta::StreamEnd),
        ],
    );
    assert_eq!(*s.phase(), TurnPhase::Completed);
    let (ev, _) = reduce(&mut s, result(&id, "zombie"));
    assert!(matches!(ev[0], AgentEvent::InputRejected { .. }));
    // But late WIRE tails stay silent.
    let (ev, _) = reduce(&mut s, d(WireDelta::Text("tail".into())));
    assert!(ev.is_empty());
}

/// PS-ENG-02: determinism — same inputs, fresh state ⇒ identical trace and
/// identical final state.
#[test]
fn reducer_is_deterministic() {
    let script = || {
        vec![
            d(WireDelta::Reasoning("hm".into())),
            d(WireDelta::ToolCallFragment {
                choice: 0,
                tool_index: 0,
                id: Some("a".into()),
                name: Some("read_file".into()),
                args_fragment: "{}".into(),
            }),
            d(WireDelta::Finish {
                choice: 0,
                reason: FinishReason::ToolCalls,
            }),
            d(WireDelta::StreamEnd),
        ]
    };
    let mut s1 = TurnState::new("ollama", "qwen3:8b", "t12");
    let mut s2 = TurnState::new("ollama", "qwen3:8b", "t12");
    let r1 = drive(&mut s1, script());
    let r2 = drive(&mut s2, script());
    assert_eq!(r1, r2);
    assert_eq!(s1, s2);
}

/// RC2-SVC7 (F0668): a stream that ends without a finish reason fails the
/// turn — but the reasoning blob captured at that stream end, already taken
/// out of the state, is still emitted before the failure, not dropped.
#[test]
fn rc2svc7_a_failure_keeps_the_events_already_produced() {
    let mut s = TurnState::new("ollama", "qwen3:8b", "t1");
    let trace = drive(
        &mut s,
        vec![
            d(WireDelta::Reasoning("thinking".into())),
            d(WireDelta::StreamEnd),
        ],
    );
    let (events, action) = trace.last().unwrap();
    assert_eq!(*action, NextAction::Done);
    assert!(
        matches!(
            events.first(),
            Some(AgentEvent::ContinuationBlobCaptured(_))
        ),
        "the reasoning blob must not be dropped: {events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::TurnFailed(EngineError::Protocol(_)))
        ),
        "{events:?}"
    );
    assert_eq!(*s.phase(), TurnPhase::Failed);
}
