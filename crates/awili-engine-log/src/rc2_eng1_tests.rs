//! RC2-ENG1 regression tests: unknown record tags are skipped and counted
//! (F0570), a failed text flush keeps its text and sequence (F0599), and an
//! intent whose turn was never opened is still reported (F0593).

use crate::authz::{HostApprovalCapability, MintedProof, RiskClass};
use crate::schema::{LogRecordV1, KNOWN_RECORD_TAGS};
use crate::{classify, frame, AppendOnlyLog, ExecutionLog, IntentOutcome, IntentStatus, MemLog};
use awili_engine::events::{AgentEvent, AwiliCallId, ToolCall};

fn call(id: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: None,
        name: "write_file".into(),
        arguments: r#"{"path":"a.txt"}"#.into(),
    }
}

fn append_raw(store: &mut MemLog, value: &serde_json::Value) {
    store
        .append(&frame::encode(&serde_json::to_vec(value).unwrap()), true)
        .unwrap();
}

/// F0570: a record of a newer build (a tag this build does not know) is
/// skipped and counted by the reader and tolerated at open; the ids it
/// carries are never minted again. A KNOWN tag that does not decode is
/// still corrupt.
#[test]
fn rc2eng1_unknown_record_tags_are_skipped_and_counted() {
    let mut log = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    log.turn_opened("t1", "ollama", "qwen3:8b").unwrap();
    let mut store = log.into_storage();
    append_raw(
        &mut store,
        &serde_json::json!({
            "t": "future_record",
            "intent_id": "t1-intent-0007",
            "approval_ref": "appr-s1-0009",
            "detail": "written by a newer build",
        }),
    );
    let mut log = ExecutionLog::open(store, "s1").expect("open tolerates an unknown tag");
    let (recs, torn, unknown) = log.read_records_counted().unwrap();
    assert_eq!((torn, unknown), (0, 1));
    assert_eq!(recs.len(), 2, "header and turn_opened only: {recs:?}");
    assert_eq!(log.read_records().unwrap().0, recs);

    // The skipped record's ids are reserved.
    let IntentOutcome::Dispatchable(ticket) = log
        .intent_unchecked("t1", &call("t1-call-0000"), "", "mutating", "", "", "")
        .unwrap()
    else {
        panic!("dispatchable");
    };
    assert_eq!(ticket.intent_id(), "t1-intent-0008");
    let proof = log
        .record_approval(
            &HostApprovalCapability::test_only(),
            "once",
            "t1",
            &call("t1-call-0001"),
            RiskClass::ReadOnly,
            None,
            "{}",
        )
        .unwrap();
    let MintedProof::Once(p) = proof else {
        panic!("once proof");
    };
    assert_eq!(p.approval_ref, "appr-s1-0010");

    // A known tag that does not decode is corrupt, as before.
    let mut store = log.into_storage();
    append_raw(&mut store, &serde_json::json!({ "t": "intent" }));
    assert!(matches!(
        ExecutionLog::open(store, "s1"),
        Err(crate::LogError::Corrupt { .. })
    ));
}

/// F0570: the reader's list of known tags is exactly the tags of
/// `LogRecordV1` — a new variant that is not added to the list would be
/// skipped as unknown.
#[test]
fn rc2eng1_known_record_tags_match_the_schema() {
    let src = include_str!("schema.rs");
    let start = src.find("pub enum LogRecordV1 {").expect("the record enum");
    let end = start + src[start..].find("\n}\n").expect("the enum's end");
    let mut tags: Vec<&str> = src[start..end]
        .split("#[serde(rename = \"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap())
        .collect();
    tags.sort_unstable();
    let mut known = KNOWN_RECORD_TAGS.to_vec();
    known.sort_unstable();
    assert_eq!(tags, known);
}

/// F0599: a text flush whose append fails cleanly keeps the text for the
/// next flush and does not consume a sequence number.
#[test]
fn rc2eng1_a_failed_text_flush_keeps_the_text_and_its_sequence() {
    let mut mem = MemLog::new();
    // Appends: 0 header, 1 turn_opened, 2 the text flush (fails cleanly).
    mem.fail_on_append = Some(2);
    let mut log = ExecutionLog::open(mem, "s1").unwrap();
    log.turn_opened("t1", "ollama", "qwen3:8b").unwrap();
    log.record_event("t1", &AgentEvent::TextDelta("hello ".into()))
        .unwrap();
    log.record_event("t1", &AgentEvent::TextDelta("world".into()))
        .unwrap();
    assert!(
        log.record_event("t1", &AgentEvent::TurnStarted).is_err(),
        "the flush append fails"
    );
    assert!(
        !log.is_poisoned(),
        "a clean failure leaves the handle usable"
    );
    log.record_event("t1", &AgentEvent::TurnStarted).unwrap();
    let (recs, _) = log.read_records().unwrap();
    let events: Vec<(u64, String)> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::EventAppended {
                seq_in_turn, event, ..
            } => Some((*seq_in_turn, format!("{event:?}"))),
            _ => None,
        })
        .collect();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].0, 0);
    assert!(events[0].1.contains("hello world"), "{events:?}");
    assert_eq!(events[1].0, 1, "no sequence gap: {events:?}");
}

/// F0593: an intent whose `TurnOpened` is missing (a malformed log) is
/// still in the recovery output — an uncertain dispatch is never dropped.
#[test]
fn rc2eng1_an_intent_without_its_turn_is_still_reported() {
    let mut log = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    log.turn_opened("t1", "ollama", "qwen3:8b").unwrap();
    log.turn_closed("t1", "completed").unwrap();
    // No turn_opened for t9.
    let IntentOutcome::Dispatchable(ticket) = log
        .intent_unchecked("t9", &call("t9-call-0000"), "", "mutating", "", "", "")
        .unwrap()
    else {
        panic!("dispatchable");
    };
    log.dispatch_started(&ticket).unwrap();
    let (recs, _) = log.read_records().unwrap();
    let recovery = classify(&recs);
    let turns: Vec<&str> = recovery.iter().map(|t| t.turn_id.as_str()).collect();
    assert_eq!(turns, ["t1", "t9"], "opened turns first, then the orphan");
    let orphan = &recovery[1];
    assert_eq!(orphan.closed, None);
    assert_eq!(orphan.intents.len(), 1);
    assert_eq!(orphan.intents[0].status, IntentStatus::Uncertain);
}
