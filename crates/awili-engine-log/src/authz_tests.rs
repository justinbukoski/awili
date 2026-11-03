//! C1 contract §10 acceptance — the parts that live at the B2 layer.
//! Single test fn: the issue-once capability statics are process-wide, so
//! all capability-dependent flows run sequentially here (parallel test
//! threads would race the AtomicBools).

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};

use crate::authz::{
    issue_bundle, AuthzEvidence, AuthzGate, GrantRef, HostApprovalCapability, ResolvedTool,
    RiskClass,
};
use crate::schema::LogRecordV1;
use crate::{
    classify, classify_with_grants, ExecutionLog, IntentOutcome, IntentStatus, LogError, MemLog,
};

/// In-crate token construction for the sequential suite (unit 4): the
/// field is crate-visible here, so these tests bypass the mint — the
/// unforgeability guarantee is against EXTERNAL code and is pinned by the
/// compile-fail doctest on `ResolvedTool` plus
/// tests/resolved_identity.rs.
fn resolved(origin: &str, name: &str) -> ResolvedTool {
    ResolvedTool {
        identity: ToolIdentity::new(ToolOrigin::new(origin), name, DescriptorDigest([0u8; 32])),
    }
}

fn call(id: &str, tool: &str, args: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: None,
        name: tool.into(),
        arguments: args.into(),
    }
}

fn statuses(recs: &[LogRecordV1]) -> Vec<IntentStatus> {
    classify(recs)
        .into_iter()
        .flat_map(|t| t.intents)
        .map(|i| i.status)
        .collect()
}

/// In-sequence coverage (unit 4 verdict r7, commit 2): a bundle claim that
/// loses on ONE bit claims NONE. The issuance statics are process-global
/// (module doc) and this runs at the one point where exactly one bit is
/// taken and another is free to observe, so it cannot be a parallel
/// `#[test]` fn here. The independently addressable named test — same
/// scenario, driven from a deliberately partial state in a fresh process —
/// is `tests/failed_bundle_claim.rs`.
///
/// Proves the Codex P1 stranded-capability regression guard, now covering
/// all three bits: if `issue_bundle` claimed the gate bit before
/// discovering the capability bit was gone, the gate would be consumed
/// with no holder and the `expect` below would fail.
fn failed_bundle_claim_consumes_no_free_bit() -> AuthzGate {
    assert!(
        issue_bundle().is_err(),
        "a bundle claim must fail once the capability bit is taken"
    );
    AuthzGate::issue()
        .expect("the failed bundle claim consumed no free bit — the gate must still be issuable")
}

#[test]
fn c1_b2_machinery_end_to_end() {
    // --- capabilities: exclusive issuance -------------------------------
    let cap = HostApprovalCapability::issue().expect("first issue succeeds");
    assert!(
        HostApprovalCapability::issue().is_err(),
        "second issue must fail"
    );

    let gate = failed_bundle_claim_consumes_no_free_bit();
    assert!(AuthzGate::issue().is_err(), "second gate issue must fail");
    assert!(
        issue_bundle().is_err(),
        "a bundle claim must fail once any of its bits is taken"
    );

    // --- policy evidence: call-bound, cross-call rejected ---------------
    let mut log = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    log.turn_opened("t1", "ollama", "qwen3:8b").unwrap();

    let read_call = call("t1-call-0000", "fs.read", r#"{"path":"a.txt"}"#);
    let ev = gate
        .policy_evidence(
            &resolved("builtin:fs", "fs.read"),
            "t1",
            &read_call,
            RiskClass::ReadOnly,
            None,
        )
        .unwrap();
    let out = log.intent("t1", &read_call, ev).unwrap();
    assert!(matches!(out, IntentOutcome::Dispatchable(_)));

    // Evidence minted for the harmless call must not authorize another.
    let evil = call("t1-call-0001", "shell.exec", r#"{"cmd":"rm -rf /"}"#);
    let ev2 = gate
        .policy_evidence(
            &resolved("builtin:fs", "fs.read"),
            "t1",
            &read_call,
            RiskClass::ReadOnly,
            None,
        )
        .unwrap();
    match log.intent("t1", &evil, ev2) {
        Err(LogError::EvidenceMismatch(_)) => {}
        other => panic!("cross-call evidence must be rejected, got {other:?}"),
    }

    // --- ask flow: record_approval -> proof -> evidence -> intent -------
    let ask_call = call(
        "t1-call-0002",
        "fs.write",
        r#"{"path":"b.txt","content":"x"}"#,
    );
    let proof = log
        .record_approval(
            &cap,
            "once",
            "t1",
            &ask_call,
            RiskClass::Mutating,
            None,
            "{}",
        )
        .unwrap();
    let ev3 = AuthzEvidence::from_ask(proof, &resolved("builtin:fs", "fs.write"), None);
    let out = log.intent("t1", &ask_call, ev3).unwrap();
    assert!(matches!(out, IntentOutcome::Dispatchable(_)));

    // --- denial audit ---------------------------------------------------
    log.record_denial("t1", &evil, RiskClass::Irreversible, "outside folder root")
        .unwrap();

    // --- clean classification: nothing AuthzInvalid so far --------------
    let (recs, _) = log.read_records().unwrap();
    assert!(
        !statuses(&recs)
            .iter()
            .any(|s| matches!(s, IntentStatus::AuthzInvalid { .. })),
        "well-formed flows must not classify AuthzInvalid"
    );
    assert!(recs.iter().any(
        |r| matches!(r, LogRecordV1::AuthzDenied { reason, .. } if reason.contains("folder"))
    ));

    // --- once-reuse: a second intent citing the same once ref -----------
    let reuse = call(
        "t1-call-0003",
        "fs.write",
        r#"{"path":"b.txt","content":"x"}"#,
    );
    let once_ref = recs
        .iter()
        .find_map(|r| match r {
            LogRecordV1::ApprovalRecorded {
                approval_ref,
                scope,
                ..
            } if scope == "once" => Some(approval_ref.clone()),
            _ => None,
        })
        .unwrap();
    // Forged via the raw path (storage-layer; simulates log tampering).
    let hash = {
        let canonical = crate::writer::canonicalize_args(&reuse.arguments).unwrap();
        crate::writer::call_fingerprint(&reuse.name, &canonical)
    };
    log.intent_unchecked("t1", &reuse, &once_ref, "mutating", "ask", &hash, "")
        .unwrap();
    let (recs, _) = log.read_records().unwrap();
    let st = statuses(&recs);
    assert!(
        st.iter().any(|s| matches!(s, IntentStatus::AuthzInvalid { reason } if reason.contains("different call") || reason.contains("reused"))),
        "once-ref reuse must classify AuthzInvalid, got {st:?}"
    );

    // --- tampered evidence hash -----------------------------------------
    let t2 = call("t2-call-0000", "fs.read", r#"{"path":"c.txt"}"#);
    log.turn_opened("t2", "ollama", "qwen3:8b").unwrap();
    log.intent_unchecked("t2", &t2, "", "read_only", "policy", "deadbeef", "")
        .unwrap();
    let (recs, _) = log.read_records().unwrap();
    assert!(
        statuses(&recs).iter().any(|s| matches!(s, IntentStatus::AuthzInvalid { reason } if reason.contains("does not match"))),
        "tampered evidence_call_hash must classify AuthzInvalid"
    );

    // --- record_approval input validation (Codex C1p1 P1) ---------------
    let v = call("t2-call-0009", "fs.rm", "{}");
    assert!(
        log.record_approval(
            &cap,
            "always",
            "t2",
            &v,
            RiskClass::Irreversible,
            None,
            "{}"
        )
        .is_err(),
        "irreversible must not be grantable 'always'"
    );
    assert!(
        log.record_approval(&cap, "sometimes", "t2", &v, RiskClass::ReadOnly, None, "{}")
            .is_err(),
        "invalid scope must be rejected"
    );
    assert!(
        log.record_approval(
            &cap,
            "once",
            "t2",
            &v,
            RiskClass::ReadOnly,
            None,
            "not-json"
        )
        .is_err(),
        "constraints must be valid JSON"
    );

    // --- reopen continues sequences (Codex C1p1 P0) ---------------------
    {
        let (store, first_ref) = {
            let mut l2 = ExecutionLog::open(MemLog::new(), "sX").unwrap();
            l2.turn_opened("tA", "ollama", "qwen3:8b").unwrap();
            let c = call("tA-call-0000", "fs.read", "{}");
            let e = gate
                .policy_evidence(
                    &resolved("builtin:fs", "fs.read"),
                    "tA",
                    &c,
                    RiskClass::ReadOnly,
                    None,
                )
                .unwrap();
            l2.intent("tA", &c, e).unwrap();
            let first = l2
                .record_approval(&cap, "once", "tA", &c, RiskClass::ReadOnly, None, "{}")
                .unwrap();
            let crate::authz::MintedProof::Once(first) = first else {
                panic!("once proof");
            };
            (l2.into_storage(), first.approval_ref)
        };
        let mut l3 = ExecutionLog::open(store, "sX").unwrap();
        let c2 = call("tA-call-0001", "fs.read", "{}");
        let p2 = l3
            .record_approval(&cap, "once", "tA", &c2, RiskClass::ReadOnly, None, "{}")
            .unwrap();
        let crate::authz::MintedProof::Once(second) = &p2 else {
            panic!("once proof");
        };
        let (recs, _) = l3.read_records().unwrap();
        let refs: Vec<_> = recs
            .iter()
            .filter_map(|r| match r {
                LogRecordV1::ApprovalRecorded { approval_ref, .. } => Some(approval_ref.clone()),
                _ => None,
            })
            .collect();
        // RC2-ENG2 (F0543): pairwise uniqueness alone would pass if reopening
        // re-recorded the first approval under a NEW ref. The log holds
        // exactly the first approval, under the ref it was minted with, and
        // the reopened handle's approval under a different one.
        assert_eq!(
            refs,
            vec![first_ref.clone(), second.approval_ref.clone()],
            "reopen keeps the existing ref and never reissues it: {refs:?}"
        );
        assert_ne!(first_ref, second.approval_ref);
        drop(p2);
    }

    // --- dangling + revoked grant refs ----------------------------------
    let g = call("t2-call-0001", "fs.read", r#"{"path":"d.txt"}"#);
    let gref = GrantRef::from_grant_set(&gate, "grants-ref-404", RiskClass::ReadOnly);
    let ev4 = AuthzEvidence::from_grant(&gref, &resolved("builtin:fs", "fs.read"), "t2", &g, None)
        .unwrap();
    log.intent("t2", &g, ev4).unwrap();
    let (recs, _) = log.read_records().unwrap();
    assert!(
        statuses(&recs).iter().any(
            |s| matches!(s, IntentStatus::AuthzInvalid { reason } if reason.contains("dangling"))
        ),
        "dangling grant ref must classify AuthzInvalid"
    );

    // A real reusable grant in a grant store, then revoked.
    let grant_store_records = {
        let mut gs = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
        let gcall = call("t9-call-0000", "fs.read", r#"{"path":"e.txt"}"#);
        let _proof = gs
            .record_approval(
                &cap,
                "always",
                "t9",
                &gcall,
                RiskClass::ReadOnly,
                None,
                "{}",
            )
            .unwrap();
        let (grecs, _) = gs.read_records().unwrap();
        let aref = grecs
            .iter()
            .find_map(|r| match r {
                LogRecordV1::ApprovalRecorded { approval_ref, .. } => Some(approval_ref.clone()),
                _ => None,
            })
            .unwrap();
        // Use it from the session log.
        let use_call = call("t2-call-0002", "fs.read", r#"{"path":"e.txt"}"#);
        let gref = GrantRef::from_grant_set(&gate, &aref, RiskClass::ReadOnly);
        let ev = AuthzEvidence::from_grant(
            &gref,
            &resolved("builtin:fs", "fs.read"),
            "t2",
            &use_call,
            None,
        )
        .unwrap();
        log.intent("t2", &use_call, ev).unwrap();
        (grecs, aref, gs)
    };
    let (grecs, aref, mut gs) = grant_store_records;
    let (srecs, _) = log.read_records().unwrap();
    // Valid grant use classifies clean (NeverDispatched, auto-resume ok).
    let st = classify_with_grants(&srecs, &grecs)
        .into_iter()
        .flat_map(|t| t.intents)
        .filter(|i| i.intent_id.contains("t2-intent"))
        .map(|i| i.status)
        .collect::<Vec<_>>();
    assert!(
        st.iter()
            .any(|s| matches!(s, IntentStatus::NeverDispatched { auto_resume: true })),
        "valid always-grant use must classify NeverDispatched(auto), got {st:?}"
    );
    // Revoke, reclassify: now invalid.
    gs.append_grant_revoked(&aref).unwrap();
    let (grecs2, _) = gs.read_records().unwrap();
    let st2 = classify_with_grants(&srecs, &grecs2)
        .into_iter()
        .flat_map(|t| t.intents)
        .map(|i| i.status)
        .collect::<Vec<_>>();
    assert!(
        st2.iter().any(
            |s| matches!(s, IntentStatus::AuthzInvalid { reason } if reason.contains("revoked"))
        ),
        "revoked grant use must classify AuthzInvalid"
    );
}
