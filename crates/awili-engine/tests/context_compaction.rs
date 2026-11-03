//! C4 merge-gate suite (Codex consult j1786631635 + design opinion
//! j1786631898): boundary budgets, atomicity under every compaction outcome,
//! pinned retention across repeated compactions, provenance/dialect gating,
//! cross-model summaries, determinism, immutability, and the ingest
//! bijection/segmentation contract.

use awili_engine::{
    apply_summary, assemble, AgentEvent, AssembleOutcome, AwiliCallId, ContextError, ContextGroup,
    ContextHistory, ContinuationArtifact, ContinuationKind, ExclusionReason, GroupId,
    HeuristicCounter, ModelContextProfile, Provenance, StopKind, ToolCall, ToolResultRecord,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// 1 char = 1 token, 1 byte = 1 token: exact arithmetic in tests.
fn counter() -> HeuristicCounter {
    HeuristicCounter {
        chars_per_token: 1.0,
        bytes_per_token: 1.0,
    }
}

/// Zero-overhead profile: budget arithmetic in tests is pure content tokens.
fn profile(window: u64) -> ModelContextProfile {
    ModelContextProfile {
        provider: "chat-completions".into(),
        model: "laguna-s-2.1-nvfp4".into(),
        window_tokens: window,
        reserved_output_tokens: 0,
        safety_margin_tokens: 0,
        assumed_summary_tokens: 10,
        replays_continuations: false,
        fixed_request_overhead_tokens: 0,
        per_message_overhead_tokens: 0,
    }
}

fn call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: Some(format!("prov_{id}")),
        name: name.into(),
        arguments: args.into(),
    }
}

fn result(id: &str, content: &str) -> ToolResultRecord {
    ToolResultRecord {
        awili_call_id: AwiliCallId(id.into()),
        content: content.into(),
        is_error: false,
    }
}

fn completed() -> AgentEvent {
    AgentEvent::TurnCompleted {
        stop: StopKind::EndTurn,
        usage: None,
    }
}

fn artifact(provider: &str, model: &str, payload: &[u8]) -> ContinuationArtifact {
    ContinuationArtifact::new(
        Provenance::new(provider, model, 0),
        ContinuationKind::Reasoning,
        payload.to_vec(),
    )
}

/// A standard history: pinned policy, old user, one tool exchange, new user.
fn standard_history() -> ContextHistory {
    let mut h = ContextHistory::new();
    h.push_pinned("POLICY: never rm -rf"); // 20 chars
    h.push_user("old question................"); // 28 chars
    h.ingest_turn_events(&[
        AgentEvent::TextDelta("old answer".into()), // 10
        AgentEvent::ToolCallReady(call("c1", "fs.read", "{\"path\":\"a\"}")), // 7 + 12
        AgentEvent::ToolResultIngested(result("c1", "FILE-CONTENT")), // 12
        completed(),
    ])
    .unwrap();
    h.push_user("current question"); // 16 chars
    h
}

// ---------------------------------------------------------------------------
// Gate 1 — boundary budgets (just-under / exact / just-over)
// ---------------------------------------------------------------------------

#[test]
fn boundary_budgets_pin_the_fits_edge() {
    let h = standard_history();
    // Total content: pinned 20 + user 28 + exchange (10+7+12+12=41) + user 16 = 105.
    let c = counter();
    for (window, expect_fits) in [(106, true), (105, true), (104, false)] {
        match assemble(&h, &profile(window), &c).unwrap() {
            AssembleOutcome::Fits(a) => {
                assert!(expect_fits, "window {window} should NOT fit");
                assert_eq!(a.tokens, 105);
            }
            AssembleOutcome::NeedsCompaction(plan) => {
                assert!(!expect_fits, "window {window} should fit");
                // Oldest summarizable first: the old user (id order preserved).
                assert!(!plan.summarize_ids.is_empty());
                assert!(plan.projected_tokens <= profile(window).input_budget());
            }
        }
    }
}

#[test]
fn cannot_fit_is_typed_never_truncated() {
    let h = standard_history();
    // Budget smaller than pinned + active suffix + assumed summary: nothing
    // selectable can save enough.
    let err = assemble(&h, &profile(30), &counter()).unwrap_err();
    assert!(matches!(err, ContextError::CannotFit { .. }), "{err:?}");
}

// ---------------------------------------------------------------------------
// Gate 2 — no orphaned call/result under every compaction boundary
// ---------------------------------------------------------------------------

#[test]
fn compaction_selects_whole_exchanges_only() {
    let mut h = ContextHistory::new();
    h.push_user("u1".repeat(10)); // 20
    for i in 0..5 {
        h.ingest_turn_events(&[
            AgentEvent::TextDelta(format!("answer-{i}-{}", "x".repeat(30))),
            AgentEvent::ToolCallReady(call(&format!("c{i}"), "fs.read", "{}")),
            AgentEvent::ToolResultIngested(result(&format!("c{i}"), &"r".repeat(20))),
            completed(),
        ])
        .unwrap();
    }
    h.push_user("current");
    // Force compaction at several window sizes; whatever is selected, every
    // Exchange in the plan (and every Exchange retained) carries its calls
    // AND results together.
    for window in [60, 120, 200, 260] {
        if let Ok(AssembleOutcome::NeedsCompaction(plan)) =
            assemble(&h, &profile(window), &counter())
        {
            for g in &plan.to_summarize {
                if let ContextGroup::Exchange { calls, results, .. } = g {
                    assert_eq!(calls.len(), results.len(), "orphaned call/result in plan");
                }
            }
            let after = apply_summary(&h, &plan.summarize_ids, "S").unwrap();
            for g in after.groups() {
                if let ContextGroup::Exchange { calls, results, .. } = g {
                    assert_eq!(calls.len(), results.len(), "orphan after apply");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Gate 3 — pinned retention across REPEATED compactions
// ---------------------------------------------------------------------------

#[test]
fn pinned_survives_two_compaction_rounds_and_summaries_nest() {
    let mut h = ContextHistory::new();
    h.push_pinned("POLICY-TEXT");
    h.push_user("a".repeat(100));
    h.ingest_turn_events(&[AgentEvent::TextDelta("b".repeat(100)), completed()])
        .unwrap();
    h.push_user("c".repeat(100));
    h.ingest_turn_events(&[AgentEvent::TextDelta("d".repeat(100)), completed()])
        .unwrap();
    h.push_user("now");

    // Round 1.
    let AssembleOutcome::NeedsCompaction(p1) = assemble(&h, &profile(150), &counter()).unwrap()
    else {
        panic!("expected compaction")
    };
    assert!(p1
        .to_summarize
        .iter()
        .all(|g| !matches!(g, ContextGroup::Pinned { .. })));
    let h2 = apply_summary(&h, &p1.summarize_ids, "SUMMARY-ONE".repeat(3)).unwrap();
    assert!(h2
        .groups()
        .iter()
        .any(|g| matches!(g, ContextGroup::Pinned { text, .. } if text == "POLICY-TEXT")));

    // Grow again, then round 2 — the previous summary itself is summarizable.
    let mut h3 = h2.clone();
    h3.ingest_turn_events(&[AgentEvent::TextDelta("e".repeat(120)), completed()])
        .unwrap();
    h3.push_user("newest");
    let AssembleOutcome::NeedsCompaction(p2) = assemble(&h3, &profile(100), &counter()).unwrap()
    else {
        panic!("expected second compaction")
    };
    let h4 = apply_summary(&h3, &p2.summarize_ids, "SUMMARY-TWO").unwrap();
    // Pinned still present after two rounds; a summary replacing a summary
    // records the nesting in `replaces`.
    assert!(h4
        .groups()
        .iter()
        .any(|g| matches!(g, ContextGroup::Pinned { text, .. } if text == "POLICY-TEXT")));
    let summaries: Vec<_> = h4
        .groups()
        .iter()
        .filter(|g| matches!(g, ContextGroup::Summary { .. }))
        .collect();
    assert_eq!(
        summaries.len(),
        1,
        "second summary consumed the first: {summaries:?}"
    );
}

#[test]
fn apply_summary_rejects_pinned_and_active_suffix() {
    let h = standard_history();
    let pinned_id = h.groups()[0].id();
    let last_user_id = h.groups().last().unwrap().id();
    assert!(matches!(
        apply_summary(&h, &[pinned_id], "S"),
        Err(ContextError::PinnedNotSummarizable(_))
    ));
    assert!(matches!(
        apply_summary(&h, &[last_user_id], "S"),
        Err(ContextError::ActiveSuffixSelected(_))
    ));
    assert!(matches!(
        apply_summary(&h, &[GroupId(999)], "S"),
        Err(ContextError::UnknownGroupId(999))
    ));
    assert!(matches!(
        apply_summary(&h, &[], "S"),
        Err(ContextError::InvalidSelection(_))
    ));
}

// ---------------------------------------------------------------------------
// Gate 4 — provenance + dialect gating of continuation blobs
// ---------------------------------------------------------------------------

#[test]
fn continuation_gating_dialect_and_provenance() {
    let mut h = ContextHistory::new();
    h.push_user("q");
    let same = h.push_continuation(artifact("openai-responses", "o-mega", b"OPAQUE"));
    let other = h.push_continuation(artifact("anthropic", "claude-opus-4-8", b"OPAQUE"));
    h.push_user("current");
    let c = counter();

    // Dialect cannot replay: both excluded, neither counted.
    let mut p = profile(1000);
    p.provider = "openai-responses".into();
    p.model = "o-mega".into();
    p.replays_continuations = false;
    let AssembleOutcome::Fits(a) = assemble(&h, &p, &c).unwrap() else {
        panic!()
    };
    assert_eq!(a.excluded_continuations.len(), 2);
    assert!(a
        .excluded_continuations
        .iter()
        .all(|(_, r)| *r == ExclusionReason::DialectCannotReplay));
    assert_eq!(a.tokens, 8); // "q" + "current" only

    // Dialect replays: same-model blob included AND counted; other excluded
    // by provenance.
    p.replays_continuations = true;
    let AssembleOutcome::Fits(a) = assemble(&h, &p, &c).unwrap() else {
        panic!()
    };
    assert_eq!(
        a.excluded_continuations,
        vec![(other, ExclusionReason::ProvenanceMismatch)]
    );
    assert!(a.groups.iter().any(|g| g.id() == same));
    assert_eq!(a.tokens, 8 + 6); // + OPAQUE payload bytes

    // Model switch: BOTH excluded by provenance; blobs stay in history.
    p.model = "different-model".into();
    let AssembleOutcome::Fits(a) = assemble(&h, &p, &c).unwrap() else {
        panic!()
    };
    assert_eq!(a.excluded_continuations.len(), 2);
    assert_eq!(
        h.groups().len(),
        4,
        "exclusion is per-assembly; history keeps blobs"
    );
}

#[test]
fn blob_payload_survives_byte_for_byte_and_is_never_summarizable() {
    let mut h = ContextHistory::new();
    h.push_user("x".repeat(50));
    let payload = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
    let blob_id = h.push_continuation(ContinuationArtifact::new(
        Provenance::new("p", "m", 0),
        ContinuationKind::Thinking,
        payload.clone(),
    ));
    h.push_user("current");
    // Byte-for-byte through assembly.
    let mut p = profile(1000);
    p.provider = "p".into();
    p.model = "m".into();
    p.replays_continuations = true;
    let AssembleOutcome::Fits(a) = assemble(&h, &p, &counter()).unwrap() else {
        panic!()
    };
    let Some(ContextGroup::Continuation { artifact, .. }) =
        a.groups.iter().find(|g| g.id() == blob_id)
    else {
        panic!("blob missing")
    };
    assert_eq!(artifact.payload, payload);
    // Never summarizable.
    assert!(matches!(
        apply_summary(&h, &[blob_id], "S"),
        Err(ContextError::NotSummarizable(_))
    ));
}

// ---------------------------------------------------------------------------
// Gate 5 — summaries are canonical cross-model context
// ---------------------------------------------------------------------------

#[test]
fn summary_carries_across_model_switch_without_blobs() {
    let mut h = ContextHistory::new();
    h.push_user("old-material ".repeat(20)); // 260
    h.push_continuation(artifact("openai-responses", "o-mega", b"HIDDEN-REASONING"));
    h.ingest_turn_events(&[AgentEvent::TextDelta("old answer ".repeat(10)), completed()])
        .unwrap(); // 110
    h.push_user("current");
    let mut p = profile(200);
    p.provider = "openai-responses".into();
    p.model = "o-mega".into();
    p.replays_continuations = true;
    let AssembleOutcome::NeedsCompaction(plan) = assemble(&h, &p, &counter()).unwrap() else {
        panic!("expected compaction")
    };
    // The plan material never contains blob payloads (P2 / no hidden
    // reasoning import).
    assert!(plan
        .to_summarize
        .iter()
        .all(|g| !matches!(g, ContextGroup::Continuation { .. })));
    let h2 = apply_summary(&h, &plan.summarize_ids, "CANONICAL-SUMMARY").unwrap();

    // Switch models entirely: the summary is included; the blob is excluded.
    let mut other = profile(1000);
    other.provider = "anthropic".into();
    other.model = "claude".into();
    other.replays_continuations = true;
    let AssembleOutcome::Fits(a) = assemble(&h2, &other, &counter()).unwrap() else {
        panic!()
    };
    assert!(a
        .groups
        .iter()
        .any(|g| matches!(g, ContextGroup::Summary { text, .. } if text == "CANONICAL-SUMMARY")));
    assert!(!a
        .groups
        .iter()
        .any(|g| matches!(g, ContextGroup::Continuation { .. })));
}

// ---------------------------------------------------------------------------
// Gate 6 — determinism / idempotence
// ---------------------------------------------------------------------------

#[test]
fn assemble_is_deterministic_and_idempotent() {
    let h = standard_history();
    let c = counter();
    for window in [104, 105, 300, 50] {
        let a = assemble(&h, &profile(window), &c);
        let b = assemble(&h, &profile(window), &c);
        assert_eq!(a, b, "window {window}");
    }
}

// ---------------------------------------------------------------------------
// Gate 7 — apply_summary never mutates its input
// ---------------------------------------------------------------------------

#[test]
fn apply_summary_leaves_original_intact() {
    let h = standard_history();
    let before = h.clone();
    let old_user = h.groups()[1].id();
    let _h2 = apply_summary(&h, &[old_user], "S").unwrap();
    assert_eq!(h, before, "input history mutated");
    // And the failure path trivially leaves it intact too.
    let _ = apply_summary(&h, &[GroupId(999)], "S");
    assert_eq!(h, before);
}

#[test]
fn interleaved_pinned_and_blob_survive_compaction_in_place() {
    // P1-2: pinned + blob INTERLEAVED with old context; selection is
    // non-contiguous; unselected groups keep their relative order.
    let mut h = ContextHistory::new();
    let u1 = h.push_user("one ".repeat(30)); // 120
    let pin = h.push_pinned("PIN");
    h.ingest_turn_events(&[AgentEvent::TextDelta("two ".repeat(30)), completed()])
        .unwrap();
    let blob = h.push_continuation(artifact("p", "m", b"B"));
    h.ingest_turn_events(&[AgentEvent::TextDelta("three ".repeat(30)), completed()])
        .unwrap();
    h.push_user("current");

    let AssembleOutcome::NeedsCompaction(plan) = assemble(&h, &profile(100), &counter()).unwrap()
    else {
        panic!("expected compaction")
    };
    assert!(plan.summarize_ids.contains(&u1));
    let h2 = apply_summary(&h, &plan.summarize_ids, "S").unwrap();
    let kinds: Vec<&str> = h2
        .groups()
        .iter()
        .map(|g| match g {
            ContextGroup::Pinned { .. } => "pin",
            ContextGroup::User { .. } => "user",
            ContextGroup::Exchange { .. } => "ex",
            ContextGroup::Summary { .. } => "sum",
            ContextGroup::Continuation { .. } => "blob",
        })
        .collect();
    // Summary at the first selected position; pin and blob in place.
    assert_eq!(kinds.first(), Some(&"sum"));
    assert!(h2.groups().iter().any(|g| g.id() == pin));
    assert!(h2.groups().iter().any(|g| g.id() == blob));
    let pin_pos = h2.groups().iter().position(|g| g.id() == pin).unwrap();
    let blob_pos = h2.groups().iter().position(|g| g.id() == blob).unwrap();
    assert!(pin_pos < blob_pos, "relative order preserved");
}

// ---------------------------------------------------------------------------
// Ingest — segmentation + bijection (P0-1 / P0-2)
// ---------------------------------------------------------------------------

#[test]
fn ingest_multi_round_turn_segments_into_exchanges() {
    let mut h = ContextHistory::new();
    h.push_user("q");
    let ids = h
        .ingest_turn_events(&[
            AgentEvent::TurnStarted,
            AgentEvent::TextDelta("round one".into()),
            AgentEvent::ToolCallReady(call("c1", "fs.read", "{}")),
            AgentEvent::ToolCallReady(call("c2", "fs.read", "{}")),
            // Results arrive OUT of call order — stored in call order.
            AgentEvent::ToolResultIngested(result("c2", "R2")),
            AgentEvent::ToolResultIngested(result("c1", "R1")),
            AgentEvent::TextDelta("round two".into()),
            AgentEvent::ToolCallReady(call("c3", "fs.edit", "{}")),
            AgentEvent::ToolResultIngested(result("c3", "R3")),
            AgentEvent::TextDelta("final text".into()),
            completed(),
        ])
        .unwrap();
    assert_eq!(ids.len(), 3);
    let g = h.groups();
    let ContextGroup::Exchange {
        assistant_text,
        calls,
        results,
        ..
    } = &g[1]
    else {
        panic!()
    };
    assert_eq!(assistant_text, "round one");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        results
            .iter()
            .map(|r| r.content.as_str())
            .collect::<Vec<_>>(),
        vec!["R1", "R2"],
        "results in CALL order, not arrival order"
    );
    let ContextGroup::Exchange {
        assistant_text,
        calls,
        ..
    } = &g[2]
    else {
        panic!()
    };
    assert_eq!(assistant_text, "round two");
    assert_eq!(calls.len(), 1);
    let ContextGroup::Exchange {
        assistant_text,
        calls,
        ..
    } = &g[3]
    else {
        panic!()
    };
    assert_eq!(assistant_text, "final text");
    assert!(calls.is_empty());
}

#[test]
fn ingest_blob_at_segment_boundary_lands_after_its_exchange() {
    let mut h = ContextHistory::new();
    h.push_user("q");
    h.ingest_turn_events(&[
        AgentEvent::TextDelta("text".into()),
        AgentEvent::ToolCallReady(call("c1", "t", "{}")),
        // Reducer emits the blob at StreamEnd — BEFORE results are fed.
        AgentEvent::ContinuationBlobCaptured(artifact("p", "m", b"B")),
        AgentEvent::ToolResultIngested(result("c1", "R")),
        completed(),
    ])
    .unwrap();
    let kinds: Vec<bool> = h
        .groups()
        .iter()
        .map(|g| matches!(g, ContextGroup::Continuation { .. }))
        .collect();
    assert_eq!(
        kinds,
        vec![false, false, true],
        "blob after the exchange it trails"
    );
}

#[test]
fn ingest_bijection_violations_are_typed_errors_and_atomic() {
    let cases: Vec<(&str, Vec<AgentEvent>)> = vec![
        (
            "duplicate call id",
            vec![
                AgentEvent::ToolCallReady(call("c1", "t", "{}")),
                AgentEvent::ToolCallReady(call("c1", "t", "{}")),
                completed(),
            ],
        ),
        (
            "orphan result (no batch)",
            vec![
                AgentEvent::ToolResultIngested(result("c9", "R")),
                completed(),
            ],
        ),
        (
            "orphan result (unknown id)",
            vec![
                AgentEvent::ToolCallReady(call("c1", "t", "{}")),
                AgentEvent::ToolResultIngested(result("c9", "R")),
                completed(),
            ],
        ),
        (
            "duplicate result",
            vec![
                AgentEvent::ToolCallReady(call("c1", "t", "{}")),
                AgentEvent::ToolCallReady(call("c2", "t", "{}")),
                AgentEvent::ToolResultIngested(result("c1", "R")),
                AgentEvent::ToolResultIngested(result("c1", "R")),
                completed(),
            ],
        ),
        (
            "terminal with open batch",
            vec![
                AgentEvent::ToolCallReady(call("c1", "t", "{}")),
                completed(),
            ],
        ),
        (
            "text over partial results",
            vec![
                AgentEvent::ToolCallReady(call("c1", "t", "{}")),
                AgentEvent::ToolCallReady(call("c2", "t", "{}")),
                AgentEvent::ToolResultIngested(result("c1", "R")),
                AgentEvent::TextDelta("nope".into()),
                completed(),
            ],
        ),
        (
            "new call over partial results",
            vec![
                AgentEvent::ToolCallReady(call("c1", "t", "{}")),
                AgentEvent::ToolCallReady(call("c2", "t", "{}")),
                AgentEvent::ToolResultIngested(result("c1", "R")),
                AgentEvent::ToolCallReady(call("c3", "t", "{}")),
                completed(),
            ],
        ),
        ("no terminal", vec![AgentEvent::TextDelta("x".into())]),
        (
            "event after terminal",
            vec![completed(), AgentEvent::TextDelta("x".into())],
        ),
        (
            "failed turn",
            vec![AgentEvent::TurnCancelled { usage: None }],
        ),
        (
            "tool call invalid",
            vec![
                AgentEvent::ToolCallInvalid {
                    awili_call_id: AwiliCallId("c1".into()),
                    provider_call_id: None,
                    name: None,
                    raw_arguments: "{".into(),
                    reason: "not valid JSON".into(),
                },
                completed(),
            ],
        ),
    ];
    for (label, events) in cases {
        let mut h = ContextHistory::new();
        h.push_user("q");
        let before = h.clone();
        let err = h.ingest_turn_events(&events);
        assert!(err.is_err(), "case '{label}' should fail");
        assert_eq!(h, before, "case '{label}' mutated history on error");
    }
}

#[test]
fn ingest_cumulative_trace_is_rejected() {
    // A cumulative trace contains the PREVIOUS turn's terminal — the
    // events-after-terminal rule catches the overlap.
    let mut h = ContextHistory::new();
    h.push_user("q");
    let turn1 = vec![AgentEvent::TextDelta("one".into()), completed()];
    h.ingest_turn_events(&turn1).unwrap();
    let mut cumulative = turn1;
    cumulative.push(AgentEvent::TextDelta("two".into()));
    cumulative.push(completed());
    assert!(matches!(
        h.ingest_turn_events(&cumulative),
        Err(ContextError::MalformedTurnSlice(_))
    ));
}

#[test]
fn no_active_request_is_typed() {
    let mut h = ContextHistory::new();
    h.push_pinned("P");
    assert_eq!(
        assemble(&h, &profile(100), &counter()).unwrap_err(),
        ContextError::NoActiveRequest
    );
}

// ---------------------------------------------------------------------------
// Structural overhead accounting (P1-4)
// ---------------------------------------------------------------------------

#[test]
fn overheads_are_explicit_and_counted() {
    let mut h = ContextHistory::new();
    h.push_user("1234"); // 4 content tokens, 1 message
    h.ingest_turn_events(&[
        AgentEvent::TextDelta("ab".into()),                   // 2
        AgentEvent::ToolCallReady(call("c1", "tt", "{}")),    // 2 + 2
        AgentEvent::ToolResultIngested(result("c1", "rrrr")), // 4
        completed(),
    ])
    .unwrap(); // exchange: 10 content, 2 messages (assistant + 1 tool)
    let mut p = profile(1000);
    p.fixed_request_overhead_tokens = 100;
    p.per_message_overhead_tokens = 7;
    let AssembleOutcome::Fits(a) = assemble(&h, &p, &counter()).unwrap() else {
        panic!()
    };
    // 100 fixed + user(4 + 1*7) + exchange(10 + 2*7) = 100 + 11 + 24 = 135.
    assert_eq!(a.tokens, 135);
}
