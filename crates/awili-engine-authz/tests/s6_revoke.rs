//! S6: a revoked remembered approval stops applying — the next matching
//! action asks again (Folder/Always through the durable grant store,
//! Session through the in-memory revoke).

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_authz::{Authorizer, AuthzContext, Decision, GrantSet, UserVerdict};
use awili_engine_log::grant_service::{GrantStoreAccess, GrantStoreService, ServiceHandle};
use awili_engine_log::test_bundle;
use awili_engine_log::{ExecutionLog, MemLog};

fn write_call(id: &str, path: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: None,
        name: "fs.write".into(),
        arguments: format!("{{\"path\":\"{path}\",\"content\":\"x\"}}"),
    }
}

#[test]
fn s6_revoked_grants_ask_again() {
    let bundle = test_bundle();
    let cap = bundle.1.clone();
    let gate = bundle.0.clone();
    let authorizer = Authorizer::new(gate.clone());
    let dir = std::env::temp_dir().join(format!(
        "awili-s6-authz-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let svc = GrantStoreService::init(dir.join("grants.log"), None).unwrap();
    let mut handle = ServiceHandle(svc);
    let resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("builtin:fs"),
        "fs",
        DescriptorDigest([0u8; 32]),
    ));
    let mut session = ExecutionLog::open(MemLog::new(), "s6-authz-a").unwrap();
    session.turn_opened("t1", "ollama", "m").unwrap();

    let decide = |grants: &GrantSet, session_id: &str, id: &str, path: &str| {
        authorizer.authorize(
            &write_call(id, path),
            AuthzContext {
                turn_id: "t1",
                session_id,
                folder_root: Some("/Users/example/work"),
                grants,
                registry_floor: None,
                resolved_tool: &resolved,
                install_witness: None,
            },
        )
    };
    let load = |session: &ExecutionLog<MemLog>, handle: &mut ServiceHandle| {
        let (s, _) = session.read_records().unwrap();
        let (g, _) = handle.read_grant_records().unwrap();
        GrantSet::load(&gate, &s, &g)
    };
    let approve = |scope: &str,
                   id: &str,
                   path: &str,
                   session: &mut ExecutionLog<MemLog>,
                   handle: &mut ServiceHandle| {
        let none = GrantSet::load(&gate, &[], &[]);
        let Decision::Ask(p) = decide(&none, "s6-authz-a", id, path) else {
            panic!("a write asks");
        };
        authorizer
            .resolve(
                p,
                UserVerdict::Approve {
                    scope: scope.into(),
                },
                &cap,
                session,
                handle,
            )
            .unwrap()
            .unwrap();
    };

    // Folder grant: applies, is listed, revoke -> asks again.
    approve("folder", "t1-c0", "a.txt", &mut session, &mut handle);
    let grants = load(&session, &mut handle);
    assert!(matches!(
        decide(&grants, "other", "t1-c1", "a.txt"),
        Decision::Allow { .. }
    ));
    let listed = svc.list_remembered().unwrap();
    let mine: Vec<_> = listed.iter().filter(|g| g.tool == "fs.write").collect();
    assert_eq!(mine.len(), 1);
    assert!(svc.revoke_remembered(&mine[0].approval_ref).unwrap());
    let grants = load(&session, &mut handle);
    assert!(
        matches!(decide(&grants, "other", "t1-c2", "a.txt"), Decision::Ask(_)),
        "after Revoke the same action asks again"
    );

    // Session grant: applies in its session, revoke -> asks again.
    approve("session", "t1-c3", "b.txt", &mut session, &mut handle);
    let grants = load(&session, &mut handle);
    assert!(matches!(
        decide(&grants, "s6-authz-a", "t1-c4", "b.txt"),
        Decision::Allow { .. }
    ));
    let sref = svc
        .list_remembered()
        .unwrap()
        .into_iter()
        .find(|g| g.session_id == "s6-authz-a")
        .expect("the live session grant is listed")
        .approval_ref;
    assert!(svc.revoke_remembered(&sref).unwrap());
    let grants = load(&session, &mut handle);
    assert!(
        matches!(
            decide(&grants, "s6-authz-a", "t1-c5", "b.txt"),
            Decision::Ask(_)
        ),
        "a revoked session grant asks again in the same session"
    );
}
