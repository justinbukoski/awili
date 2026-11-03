//! U4b1 (D7): `resolve_auto` — Auto mode's non-dialog approval path. Single
//! test fn: the issue-once statics are process-wide (same pattern as
//! `authorize.rs`).

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_authz::{Authorizer, AuthzContext, Decision, GrantSet};
use awili_engine_log::test_bundle;
use awili_engine_log::{ExecutionLog, IntentOutcome, MemLog};

fn call(id: &str, tool: &str, args: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: None,
        name: tool.into(),
        arguments: args.into(),
    }
}

/// Auto mode (D7): a non-floor Ask resolves WITHOUT a dialog and WITHOUT
/// the grant store — a scope-`"once"` session record carrying the
/// auto-mode constraints — while every floor-escalated call (secret file,
/// read outside the folder) fails closed, and a pending can never mint
/// into another session's log.
#[test]
fn u4b1_auto_resolution_respects_floors() {
    let bundle = test_bundle();
    let cap = bundle.1.clone();
    let gate = bundle.0.clone();
    let empty = GrantSet::load(&gate, &[], &[]);
    let authorizer = Authorizer::new(gate);
    let mut session = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    session.turn_opened("t1", "ollama", "qwen3:8b").unwrap();

    let resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("builtin:fs"),
        "fs",
        DescriptorDigest([0u8; 32]),
    ));

    // --- non-floor `fs.write`: Ask, floor_required() == false ----------
    let write = call(
        "t1-u4b1-0000",
        "fs.write",
        r#"{"path":"a.txt","contents":"x"}"#,
    );
    let d = authorizer.authorize(
        &write,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    );
    let Decision::Ask(pending) = d else {
        panic!("in-folder write must ask (no grant, not read_only)")
    };
    assert!(!pending.floor_required(), "plain write is not a floor");
    let ev = authorizer
        .resolve_auto(pending, &cap, &mut session)
        .expect("non-floor ask auto-resolution must succeed");
    assert!(matches!(
        session.intent("t1", &write, ev).unwrap(),
        IntentOutcome::Dispatchable(_)
    ));
    let (records, _) = session.read_records().unwrap();
    let approvals: Vec<_> = records
        .iter()
        .filter_map(|r| match r {
            awili_engine_log::LogRecordV1::ApprovalRecorded {
                scope, constraints, ..
            } => Some((scope.clone(), constraints.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(approvals.len(), 1, "exactly one approval record");
    let (scope, constraints) = approvals.into_iter().next().unwrap();
    assert_eq!(scope, "once", "auto mode mints scope-\"once\" only");
    assert!(
        constraints.contains("auto_mode"),
        "auto-mode record carries the auto signature: {constraints}"
    );

    // --- `.env`: floor; `resolve_auto` fails closed ---------------------
    let env = call(
        "t1-u4b1-0001",
        "fs.write",
        r#"{"path":".env","contents":"x"}"#,
    );
    let d = authorizer.authorize(
        &env,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    );
    let Decision::Ask(pending) = d else {
        panic!("secret write must ask")
    };
    assert!(pending.floor_required(), "secret file is a floor");
    assert!(
        authorizer
            .resolve_auto(pending, &cap, &mut session)
            .is_err(),
        "floor call must fail closed in auto mode"
    );

    // --- read outside the folder: floor ----------------------------------
    let outside = call("t1-u4b1-0002", "fs.read", r#"{"path":"/etc/hosts"}"#);
    let d = authorizer.authorize(
        &outside,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    );
    let Decision::Ask(pending) = d else {
        panic!("read outside the folder must ask (read_only floor)")
    };
    assert!(pending.floor_required(), "outside-folder read is a floor");
    let approvals_before = session
        .read_records()
        .unwrap()
        .0
        .iter()
        .filter(|r| matches!(r, awili_engine_log::LogRecordV1::ApprovalRecorded { .. }))
        .count();
    assert!(
        authorizer
            .resolve_auto(pending, &cap, &mut session)
            .is_err(),
        "floor call must fail closed in auto mode"
    );
    let approvals_after = session
        .read_records()
        .unwrap()
        .0
        .iter()
        .filter(|r| matches!(r, awili_engine_log::LogRecordV1::ApprovalRecorded { .. }))
        .count();
    assert_eq!(
        approvals_after, approvals_before,
        "a floor refusal must not record an approval"
    );

    // --- session binding: s1 pending into s2's log ----------------------
    let stray = call(
        "t1-u4b1-0003",
        "fs.write",
        r#"{"path":"b.txt","contents":"x"}"#,
    );
    let d = authorizer.authorize(
        &stray,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    );
    let Decision::Ask(pending) = d else {
        panic!("plain write must ask")
    };
    let mut other = ExecutionLog::open(MemLog::new(), "s2").unwrap();
    other.turn_opened("t1", "ollama", "qwen3:8b").unwrap();
    assert!(
        authorizer.resolve_auto(pending, &cap, &mut other).is_err(),
        "a pending bound to s1 must not mint into s2's log"
    );
}
