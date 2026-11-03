//! B2 acceptance (the P0): kill the flow at EVERY append boundary and at
//! torn-write points; replay must never make an unsafe classification.
//! "Unsafe" = a dispatch that started but did not finalize being anything
//! other than Uncertain, or a mutating never-dispatched intent marked
//! auto-resume.

use crate::schema::LogRecordV1;
use crate::{classify, AppendOnlyLog, ExecutionLog, IntentOutcome, IntentStatus, MemLog};
use awili_engine::events::{AwiliCallId, ToolCall};

fn call(id: &str, args: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: Some(format!("prov-{id}")),
        name: "write_file".into(),
        arguments: args.into(),
    }
}

/// Drive the canonical flow, stopping at the first storage error (crash
/// semantics: a real host would be dead). Returns how far it got.
fn drive_flow(log: &mut ExecutionLog<MemLog>) -> &'static str {
    if log.turn_opened("t1", "ollama", "qwen3:8b").is_err() {
        return "turn_opened";
    }
    let ticket = match log.intent_unchecked(
        "t1",
        &call("t1-call-0000", r#"{"path":"a.txt","content":"hi"}"#),
        "",
        "mutating",
        "",
        "",
        "",
    ) {
        Ok(IntentOutcome::Dispatchable(t)) => t,
        Ok(IntentOutcome::NotDispatchable { .. }) => return "intent-ineligible",
        Err(_) => return "intent",
    };
    if log.dispatch_started(&ticket).is_err() {
        return "dispatch_started";
    }
    if log.dispatch_finalized(&ticket, true, "written").is_err() {
        return "dispatch_finalized";
    }
    if log.turn_closed("t1", "completed").is_err() {
        return "turn_closed";
    }
    "complete"
}

/// Returns (turns, intents) classified — RC2-ENG2 (F0584): callers check
/// them against the step the flow died at, so the loops below are never
/// vacuously empty.
fn assert_safe(records: &[LogRecordV1], died_at: &str) -> (usize, usize) {
    let recovery = classify(records);
    for turn in &recovery {
        for i in &turn.intents {
            match &i.status {
                IntentStatus::Clean { .. } => {
                    // Clean requires a DispatchFinalized record — verify.
                    assert!(
                        records.iter().any(|r| matches!(
                            r,
                            LogRecordV1::DispatchFinalized { intent_id, .. } if *intent_id == i.intent_id
                        )),
                        "Clean without finalized record (died at {died_at})"
                    );
                }
                IntentStatus::NeverDispatched { auto_resume } => {
                    // Mutating tools must never auto-resume.
                    assert!(
                        !(*auto_resume) || i.risk_class == "read",
                        "mutating intent auto-resumable (died at {died_at})"
                    );
                    // And there must be no DispatchStarted record.
                    assert!(
                        !records.iter().any(|r| matches!(
                            r,
                            LogRecordV1::DispatchStarted { intent_id } if *intent_id == i.intent_id
                        )),
                        "NeverDispatched but start record exists (died at {died_at})"
                    );
                }
                IntentStatus::Uncertain => {
                    // Uncertain is always safe — but verify it is accurate:
                    // started, not finalized.
                    assert!(records.iter().any(|r| matches!(
                        r,
                        LogRecordV1::DispatchStarted { intent_id } if *intent_id == i.intent_id
                    )));
                }
                IntentStatus::AuthzInvalid { .. } => {
                    // Never resumable — inherently safe; kill-point flows
                    // use legacy (pre-C1) intents so this should not occur.
                    panic!("kill-point flow produced AuthzInvalid (died at {died_at})");
                }
            }
        }
    }
    (
        recovery.len(),
        recovery.iter().map(|t| t.intents.len()).sum(),
    )
}

/// RC2-ENG2 (F0584): what the surviving prefix must hold when the flow died
/// at `died_at` — (turns, intents).
fn expected_shape(died_at: &str) -> (usize, usize) {
    match died_at {
        "turn_opened" => (0, 0),
        "intent" | "intent-ineligible" => (1, 0),
        _ => (1, 1),
    }
}

/// Kill cleanly at every append boundary (0..=6 covers header, turn_opened,
/// intent, dispatch_started, dispatch_finalized, turn_closed, and beyond).
#[test]
fn clean_kill_at_every_append_boundary_is_safe() {
    for kill_at in 0..=6u64 {
        let mut mem = MemLog::new();
        mem.fail_on_append = Some(kill_at);
        // Header append may itself fail -> open() errors; that log is empty
        // and trivially safe.
        let Ok(mut log) = ExecutionLog::open(mem, "s1") else {
            continue;
        };
        let died_at = drive_flow(&mut log);
        let (records, torn) = log.read_records().expect("clean kill leaves whole frames");
        assert_eq!(torn, 0);
        assert_eq!(
            assert_safe(&records, died_at),
            expected_shape(died_at),
            "the surviving prefix (died at {died_at})"
        );

        // Specific invariant: if we died at `intent`, NO ticket was issued,
        // so the host cannot have dispatched — and the log must contain no
        // dispatch records at all.
        if died_at == "intent" {
            assert!(!records
                .iter()
                .any(|r| matches!(r, LogRecordV1::DispatchStarted { .. })));
        }
        // If we died right after dispatch_started, replay MUST say Uncertain.
        if died_at == "dispatch_finalized" {
            let rec = classify(&records);
            assert!(rec
                .iter()
                .flat_map(|t| &t.intents)
                .any(|i| i.status == IntentStatus::Uncertain));
        }
    }
}

/// Power-cut mid-write: every append boundary, torn at several byte offsets.
/// The torn tail must be tolerated and the surviving prefix classified safely.
#[test]
fn torn_write_at_every_boundary_is_safe() {
    for kill_at in 1..=5u64 {
        for tear in [1usize, 4, 8, 9, 20] {
            let mut mem = MemLog::new();
            mem.fail_on_append = Some(kill_at);
            mem.tear_bytes = Some(tear);
            let Ok(mut log) = ExecutionLog::open(mem, "s1") else {
                continue;
            };
            let died_at = drive_flow(&mut log);
            let (records, torn) = log
                .read_records()
                .expect("torn tail must be recovered, not errored");
            // RC2-ENG2 (F0584): every torn frame here is longer than the
            // tear, so exactly the torn bytes are reported...
            assert_eq!(torn, tear, "torn bytes (died at {died_at}, tear {tear})");
            // ...and the classified prefix is the one the flow wrote.
            assert_eq!(
                assert_safe(&records, died_at),
                expected_shape(died_at),
                "the surviving prefix (died at {died_at}, tear {tear})"
            );
        }
    }
}

/// Happy path: full flow round-trips and classifies Clean.
#[test]
fn complete_flow_round_trips_clean() {
    let mut log = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    assert_eq!(drive_flow(&mut log), "complete");
    let (records, torn) = log.read_records().unwrap();
    assert_eq!(torn, 0);
    let rec = classify(&records);
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].closed.as_deref(), Some("completed"));
    assert_eq!(rec[0].intents.len(), 1);
    assert_eq!(rec[0].intents[0].status, IntentStatus::Clean { ok: true });
}

/// Read-only intents that never dispatched may auto-resume; mutating and
/// unknown ones may not.
#[test]
fn never_dispatched_policy_is_conservative() {
    let mut log = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    log.turn_opened("t1", "ollama", "qwen3:8b").unwrap();
    for (cid, rc) in [
        ("t1-call-0000", "read"),
        ("t1-call-0001", "mutating"),
        ("t1-call-0002", ""),
    ] {
        let out = log
            .intent_unchecked("t1", &call(cid, "{}"), "", rc, "", "", "")
            .unwrap();
        assert!(matches!(out, IntentOutcome::Dispatchable(_)));
    }
    let (records, _) = log.read_records().unwrap();
    let rec = classify(&records);
    let statuses: Vec<_> = rec[0].intents.iter().map(|i| i.status.clone()).collect();
    assert_eq!(
        statuses,
        vec![
            IntentStatus::NeverDispatched { auto_resume: true },
            IntentStatus::NeverDispatched { auto_resume: false },
            IntentStatus::NeverDispatched { auto_resume: false },
        ]
    );
}

/// Fingerprints are stable under argument key reordering, and advisory only.
#[test]
fn fingerprint_ignores_json_key_order() {
    use crate::{call_fingerprint, canonicalize_args};
    let a = canonicalize_args(r#"{"path":"a.txt","content":"hi"}"#).unwrap();
    let b = canonicalize_args(r#"{"content":"hi","path":"a.txt"}"#).unwrap();
    assert_eq!(a, b);
    assert_eq!(
        call_fingerprint("write_file", &a),
        call_fingerprint("write_file", &b)
    );
}

/// Set A doctrine: malformed argument JSON is persisted verbatim for audit,
/// yields NO ticket, and is never auto-resumable on replay.
#[test]
fn malformed_intent_is_audited_but_not_dispatchable() {
    let mut log = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    log.turn_opened("t1", "ollama", "qwen3:8b").unwrap();
    let out = log
        .intent_unchecked(
            "t1",
            &call("t1-call-0000", r#"{"path":"trunc"#),
            "",
            "read",
            "",
            "",
            "",
        )
        .unwrap();
    assert!(matches!(out, IntentOutcome::NotDispatchable { .. }));
    let (records, _) = log.read_records().unwrap();
    let rec = classify(&records);
    let i = &rec[0].intents[0];
    assert_eq!(i.raw_arguments, r#"{"path":"trunc"#); // verbatim audit
    assert_eq!(i.canonical_args, None);
    assert_eq!(i.call_fingerprint, None);
    assert_eq!(
        i.status,
        IntentStatus::NeverDispatched { auto_resume: false }
    );
}

/// Empty arguments are legal zero-arg calls: canonicalize to {} and dispatch.
#[test]
fn empty_args_are_dispatchable() {
    let mut log = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    log.turn_opened("t1", "ollama", "qwen3:8b").unwrap();
    let out = log
        .intent_unchecked("t1", &call("t1-call-0000", "  "), "", "read", "", "", "")
        .unwrap();
    assert!(matches!(out, IntentOutcome::Dispatchable(_)));
}

/// Corruption in the MIDDLE of a log (not a torn tail) is an error, never
/// silently skipped.
#[test]
fn mid_log_corruption_errors() {
    let mut log = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    assert_eq!(drive_flow(&mut log), "complete");
    let mut raw = log.into_storage().read_all().unwrap();
    let mid = raw.len() / 2;
    raw[mid] ^= 0xFF;
    let mut corrupted = MemLog::new();
    corrupted.append(&raw, false).unwrap();
    // Mid-log corruption now surfaces at open() (the reopen sequence scan
    // decodes frames) — or at read_records() if open tolerated it. Either
    // way it must ERROR, never silently skip.
    match ExecutionLog::open(corrupted, "s1") {
        Err(_) => {}
        Ok(log2) => assert!(log2.read_records().is_err()),
    }
}

/// C2-u4 pin (Codex round-1 P1): malformed JSON arguments are
/// NotDispatchable at the INTENT boundary — no ticket exists, so a
/// malformed call can never reach dispatch at all, and replay never
/// sees a dispatchable intent for it. (One layer further out, C1's
/// `policy_evidence` refuses to mint evidence for malformed arguments,
/// pinned in awili-builtins; this test pins B2's own boundary.)
#[test]
fn malformed_arguments_are_not_dispatchable_at_intent() {
    let mut log = ExecutionLog::open(MemLog::default(), "sess-malformed").unwrap();
    log.turn_opened("t1", "test", "test-model").unwrap();
    let outcome = log
        .intent_unchecked(
            "t1",
            &call("t1-call-0000", "{nope"),
            "",
            "read_only",
            "",
            "",
            "",
        )
        .unwrap();
    match outcome {
        IntentOutcome::NotDispatchable { reason, .. } => {
            assert!(!reason.is_empty(), "bounded diagnostic expected");
        }
        IntentOutcome::Dispatchable(_) => panic!("malformed JSON must never yield a ticket"),
    }
    // The intent is durably recorded for audit, and replay classifies it
    // without inventing a dispatchable state.
    let (records, _) = log.read_records().unwrap();
    assert!(records
        .iter()
        .any(|r| matches!(r, LogRecordV1::Intent { .. })));
    let turns = classify(&records);
    let statuses: Vec<_> = turns.iter().flat_map(|t| t.intents.iter()).collect();
    assert_eq!(statuses.len(), 1);
    assert!(
        statuses[0].canonical_args.is_none(),
        "replay must see the malformed marker (no canonical args)"
    );
    assert!(
        !matches!(statuses[0].status, IntentStatus::Uncertain),
        "malformed intent must not classify Uncertain"
    );
}

/// RVS5C-FIX1 (review RVS5C-sp P1 writer.rs:284): after an append that
/// failed AFTER writing bytes (a torn write), the SAME live handle refuses
/// every later append with `LogError::Poisoned` — nothing lands behind the
/// torn bytes, and the storage is unchanged by the refused retries. Reopen
/// runs the normal tail validation; a session log has no repair path, so
/// it refuses.
#[test]
fn same_handle_retry_after_torn_append_is_refused() {
    let mut mem = MemLog::new();
    // Append 0 = Header, 1 = TurnOpened, 2 = the Intent: torn after 9 bytes.
    mem.fail_on_append = Some(2);
    mem.tear_bytes = Some(9);
    let mut log = ExecutionLog::open(mem, "s1").unwrap();
    log.turn_opened("t1", "ollama", "qwen3:8b").unwrap();
    let first = log.intent_unchecked(
        "t1",
        &call("t1-call-0000", "{}"),
        "",
        "mutating",
        "",
        "",
        "",
    );
    assert!(
        matches!(first, Err(crate::LogError::Storage(_))),
        "{first:?}"
    );
    assert!(log.is_poisoned(), "a torn append must poison the handle");
    // Same-handle retries on three different append paths: all refused.
    let offset = match log.turn_closed("t1", "failed") {
        Err(crate::LogError::Poisoned { offset }) => offset,
        other => panic!("retry on a poisoned handle must be refused, got {other:?}"),
    };
    assert!(matches!(
        log.append_grant_revoked("appr-s1-0000"),
        Err(crate::LogError::Poisoned { .. })
    ));
    assert!(matches!(
        log.intent_unchecked("t1", &call("t1-call-0001", "{}"), "", "read", "", "", ""),
        Err(crate::LogError::Poisoned { .. })
    ));
    // Storage = the clean prefix + exactly the 9 torn bytes; no retry frame.
    let raw = log.into_storage().read_all().unwrap();
    assert_eq!(
        raw.len() as u64,
        offset + 9,
        "no retry bytes landed behind the tear"
    );
    match crate::frame::decode_all(&raw[..offset as usize]).unwrap() {
        crate::frame::DecodeResult::Clean(frames) => assert_eq!(frames.len(), 2),
        other => panic!("the prefix before the failed append must be clean: {other:?}"),
    }
    // Reopen re-validates the tail and refuses (no session-log repair).
    let mut reopened = MemLog::new();
    reopened.append(&raw, false).unwrap();
    match ExecutionLog::open(reopened, "s1") {
        Err(crate::LogError::Corrupt { seq: 0, reason }) => {
            assert!(reason.contains("torn tail at open"), "{reason}")
        }
        Ok(_) => panic!("reopen over torn bytes must refuse"),
        Err(e) => panic!("reopen must refuse with Corrupt, got {e:?}"),
    }
}

/// RVS5C-FIX1: an append that failed BEFORE any byte landed (length
/// unchanged) leaves the framing intact and the handle usable — the seam's
/// audited close (awili-seam `storage_failure_matrix_aborts_honestly_and_closes`)
/// appends TurnClosed on the same handle after exactly this failure.
#[test]
fn clean_append_failure_keeps_the_handle_usable() {
    let mut mem = MemLog::new();
    mem.fail_on_append = Some(2);
    let mut log = ExecutionLog::open(mem, "s1").unwrap();
    log.turn_opened("t1", "ollama", "qwen3:8b").unwrap();
    assert!(log
        .intent_unchecked(
            "t1",
            &call("t1-call-0000", "{}"),
            "",
            "mutating",
            "",
            "",
            ""
        )
        .is_err());
    assert!(
        !log.is_poisoned(),
        "no byte landed: the handle stays usable"
    );
    log.turn_closed("t1", "failed").unwrap();
    let (records, torn) = log.read_records().unwrap();
    assert_eq!(torn, 0);
    assert_eq!(records.len(), 3, "{records:?}");
    assert!(matches!(
        records.last(),
        Some(LogRecordV1::TurnClosed { .. })
    ));
}
