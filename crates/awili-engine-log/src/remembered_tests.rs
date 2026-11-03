//! S6: the remembered-approvals read API and revocation.

use std::path::PathBuf;

use crate::authz::{HostApprovalCapability, RiskClass};
use crate::grant_service::GrantStoreService;
use crate::remembered::{
    is_session_revoked, session_grants, stored_grants_from_records, GrantScopeKind,
};
use crate::storage::MemLog;
use crate::writer::ExecutionLog;

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "awili-s6-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn call(name: &str, args: &str) -> awili_engine::events::ToolCall {
    awili_engine::events::ToolCall {
        awili_call_id: awili_engine::events::AwiliCallId("c1".into()),
        provider_call_id: None,
        name: name.into(),
        arguments: args.into(),
    }
}

fn grant_ref(p: crate::authz::MintedProof) -> String {
    match p {
        crate::authz::MintedProof::Grant(g) => g.approval_ref,
        crate::authz::MintedProof::Once(o) => o.approval_ref,
    }
}

#[test]
fn s6_list_returns_each_kind_with_scope_and_time_newest_first() {
    let cap = HostApprovalCapability::test_only();
    let svc = GrantStoreService::init(tmpdir("list").join("grants.log"), None).unwrap();
    let a = svc
        .with_store(|log| {
            log.record_approval(
                &cap,
                "always",
                "t1",
                &call("fs.write", r#"{"path":"a.txt"}"#),
                RiskClass::Mutating,
                None,
                r#"{"path":"a.txt"}"#,
            )
        })
        .map(grant_ref)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let f = svc
        .with_store(|log| {
            log.record_approval(
                &cap,
                "folder",
                "t1",
                &call("fs.edit", r#"{"path":"b.txt"}"#),
                RiskClass::Mutating,
                Some("/work/proj"),
                "{}",
            )
        })
        .map(grant_ref)
        .unwrap();
    // A session grant from a live session log, and a "once" that must not list.
    let mut session = ExecutionLog::open(MemLog::new(), "s6-list-session").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let s = grant_ref(
        session
            .record_approval(
                &cap,
                "session",
                "t1",
                &call("fs.write", r#"{"path":"c.txt"}"#),
                RiskClass::Mutating,
                None,
                "{}",
            )
            .unwrap(),
    );
    session
        .record_approval(
            &cap,
            "once",
            "t1",
            &call("fs.write", r#"{"path":"d.txt"}"#),
            RiskClass::Mutating,
            None,
            "{}",
        )
        .unwrap();

    let listed = svc.list_remembered().unwrap();
    let mine: Vec<_> = listed
        .iter()
        .filter(|g| [&a, &f, &s].contains(&&g.approval_ref))
        .collect();
    let refs: Vec<&str> = mine.iter().map(|g| g.approval_ref.as_str()).collect();
    assert_eq!(refs, [s.as_str(), f.as_str(), a.as_str()], "newest first");
    assert_eq!(mine[0].scope, GrantScopeKind::Session);
    assert_eq!(mine[1].scope, GrantScopeKind::Folder);
    assert_eq!(mine[1].folder_root.as_deref(), Some("/work/proj"));
    assert_eq!(mine[2].scope, GrantScopeKind::Always);
    assert_eq!(mine[2].constraints.get("path").unwrap(), "a.txt");
    assert!(mine.iter().all(|g| g.granted_at_ms.is_some_and(|t| t > 0)));
    assert!(mine[0].granted_at_ms >= mine[1].granted_at_ms);
    assert!(mine[1].granted_at_ms >= mine[2].granted_at_ms);
    crate::remembered::forget_session("s6-list-session");
}

#[test]
fn s6_revoke_persists_across_reload_and_is_idempotent() {
    let cap = HostApprovalCapability::test_only();
    let dir = tmpdir("revoke");
    let path = dir.join("grants.log");
    let svc = GrantStoreService::init(&path, None).unwrap();
    let keep = svc
        .with_store(|log| {
            log.record_approval(
                &cap,
                "always",
                "t1",
                &call("fs.read", r#"{"path":"k"}"#),
                RiskClass::ReadOnly,
                None,
                "{}",
            )
        })
        .map(grant_ref)
        .unwrap();
    let gone = svc
        .with_store(|log| {
            log.record_approval(
                &cap,
                "folder",
                "t1",
                &call("fs.write", r#"{"path":"g"}"#),
                RiskClass::Mutating,
                Some("/w"),
                "{}",
            )
        })
        .map(grant_ref)
        .unwrap();
    assert!(svc.revoke_remembered(&gone).unwrap());
    assert!(!svc.revoke_remembered(&gone).unwrap(), "already revoked");
    assert!(!svc.revoke_remembered("appr-nope-0000").unwrap());

    // Reload from the bytes on disk, as a fresh process would.
    let (records, _) =
        ExecutionLog::open_grant_store(crate::storage::FileLog::open(&path).unwrap())
            .unwrap()
            .read_records()
            .unwrap();
    let left: Vec<_> = stored_grants_from_records(&records)
        .into_iter()
        .map(|g| g.approval_ref)
        .collect();
    assert_eq!(left, [keep]);
}

#[test]
fn s6_session_revoke_is_in_memory_and_ends_with_the_session() {
    let cap = HostApprovalCapability::test_only();
    let svc = GrantStoreService::init(tmpdir("sess").join("grants.log"), None).unwrap();
    let mut session = ExecutionLog::open(MemLog::new(), "s6-sess-revoke").unwrap();
    let r = grant_ref(
        session
            .record_approval(
                &cap,
                "session",
                "t1",
                &call("fs.write", r#"{"path":"c"}"#),
                RiskClass::Mutating,
                None,
                "{}",
            )
            .unwrap(),
    );
    assert!(session_grants().iter().any(|g| g.approval_ref == r));
    assert!(svc.revoke_remembered(&r).unwrap());
    assert!(is_session_revoked(&r));
    assert!(!session_grants().iter().any(|g| g.approval_ref == r));
    crate::remembered::forget_session("s6-sess-revoke");
    assert!(
        !is_session_revoked(&r),
        "the revoke note ends with the session"
    );
}

#[test]
fn s6_identical_grants_collapse_and_revoke_together() {
    let cap = HostApprovalCapability::test_only();
    let svc = GrantStoreService::init(tmpdir("dupe").join("grants.log"), None).unwrap();
    let mut refs = Vec::new();
    for _ in 0..2 {
        refs.push(
            svc.with_store(|log| {
                log.record_approval(
                    &cap,
                    "always",
                    "t1",
                    &call("fs.write", r#"{"path":"a"}"#),
                    RiskClass::Mutating,
                    None,
                    r#"{"path":"a"}"#,
                )
            })
            .map(grant_ref)
            .unwrap(),
        );
    }
    let listed = svc.list_remembered().unwrap();
    assert_eq!(listed.len(), 1);
    let mut got = listed[0].all_refs.clone();
    got.sort();
    refs.sort();
    assert_eq!(got, refs);
    assert!(svc.revoke_grant(&listed[0]).unwrap());
    assert!(svc.list_remembered().unwrap().is_empty());
}

#[test]
fn s6_forget_session_clears_revoke_notes_that_never_registered() {
    let r = "appr-s6-never-registered-0000";
    crate::remembered::mark_revoked_for_tests(r);
    assert!(is_session_revoked(r));
    crate::remembered::forget_session("s6-never-registered");
    assert!(!is_session_revoked(r));
}
