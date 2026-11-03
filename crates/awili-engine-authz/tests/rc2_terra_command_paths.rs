//! Command/path role regression tests: path-shaped payload is not a path,
//! while actual path arguments retain traversal, secret, and grant bounds.

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_authz::{Authorizer, AuthzContext, Decision, GrantSet, UserVerdict};
use awili_engine_log::authz::RiskClass;
use awili_engine_log::{test_bundle, ExecutionLog, MemLog};

const FOLDER: &str = "/tmp";

fn call(id: &str, tool: &str, arguments: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: None,
        name: tool.into(),
        arguments: arguments.into(),
    }
}

fn context<'a>(
    grants: &'a GrantSet,
    resolved: &'a awili_engine_log::ResolvedTool,
    floor: Option<RiskClass>,
) -> AuthzContext<'a, 'static> {
    AuthzContext {
        turn_id: "t1",
        session_id: "s1",
        folder_root: Some(FOLDER),
        grants,
        registry_floor: floor,
        resolved_tool: resolved,
        install_witness: None,
    }
}

#[test]
fn rc2_terra_command_and_path_roles() {
    let bundle = test_bundle();
    let cap = bundle.1.clone();
    let gate = bundle.0.clone();
    let authorizer = Authorizer::new(gate.clone());
    let empty = GrantSet::load(&gate, &[], &[]);
    let shell = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("mcp:mac-shell"),
        "run_command",
        DescriptorDigest([1; 32]),
    ));
    let search = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("mcp:search"),
        "search",
        DescriptorDigest([2; 32]),
    ));
    let fs = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("builtin:fs"),
        "fs",
        DescriptorDigest([3; 32]),
    ));

    // A whole shell command stays an opaque command argument. It reaches the
    // usual privileged approval path, rather than a false folder denial.
    let open_chrome = call(
        "t1-command-0000",
        "mac-shell.run_command",
        r#"{"command":"/usr/bin/open -a \"Google Chrome\" \"https://example.com\""}"#,
    );
    assert!(matches!(
        authorizer.authorize(
            &open_chrome,
            context(&empty, &shell, Some(RiskClass::Privileged))
        ),
        Decision::Ask(_)
    ));

    // Slash and double-dot text in URL/search/content roles is not local
    // filesystem input and cannot trigger a folder-boundary decision.
    for arguments in [
        r#"{"targetURL":"https://example.com/a/../b"}"#,
        r#"{"query":"why does a/b contain .. in prose?"}"#,
        r#"{"content":"see /tmp/../ordinary text"}"#,
    ] {
        assert!(
            matches!(
                authorizer.authorize(
                    &call("t1-payload-0000", "search", arguments),
                    context(&empty, &search, Some(RiskClass::ReadOnly)),
                ),
                Decision::Allow { .. }
            ),
            "payload was treated as a filesystem path: {arguments}"
        );
    }

    // Genuine path fields remain subject to outside-root and traversal
    // screening. draft..final.txt is a filename, not a .. component.
    assert!(matches!(
        authorizer.authorize(
            &call(
                "t1-path-0000",
                "fs.write",
                r#"{"path":"/outside/file","content":"x"}"#,
            ),
            context(&empty, &fs, None),
        ),
        Decision::Deny(_)
    ));
    assert!(matches!(
        authorizer.authorize(
            &call("t1-path-0001", "fs.read", r#"{"path":"../secret"}"#),
            context(&empty, &fs, None),
        ),
        Decision::Deny(_)
    ));
    assert!(matches!(
        authorizer.authorize(
            &call("t1-path-0002", "fs.read", r#"{"path":"draft..final.txt"}"#,),
            context(&empty, &fs, None),
        ),
        Decision::Allow { .. }
    ));
    for arguments in [
        r#"{"script_path":"../secret"}"#,
        r#"{"source_code_path":"%252e%252e%252fetc"}"#,
    ] {
        assert!(
            matches!(
                authorizer.authorize(
                    &call("t1-compound-path-0000", "fs.read", arguments),
                    context(&empty, &fs, None),
                ),
                Decision::Deny(_)
            ),
            "compound path key escaped screening: {arguments}"
        );
    }
    // Read-only access outside the folder is still screened, but the existing
    // policy presents its non-overridable approval floor rather than denying.
    assert!(matches!(
        authorizer.authorize(
            &call(
                "t1-compound-path-0001",
                "fs.read",
                r#"{"content_file":"/outside/file"}"#,
            ),
            context(&empty, &fs, None),
        ),
        Decision::Ask(pending) if pending.floor_required()
    ));
    // The same outside-root compound path remains a deny for a mutation.
    assert!(matches!(
        authorizer.authorize(
            &call(
                "t1-compound-path-0002",
                "fs.write",
                r#"{"content_file":"/outside/file","content":"x"}"#,
            ),
            context(&empty, &fs, None),
        ),
        Decision::Deny(_)
    ));

    // Decoding remains part of genuine path screening: encoded traversal is
    // denied and an encoded secret path reaches the non-overridable Ask floor.
    assert!(matches!(
        authorizer.authorize(
            &call(
                "t1-path-0003",
                "fs.read",
                r#"{"path":"%252e%252e%252fetc"}"#,
            ),
            context(&empty, &fs, None),
        ),
        Decision::Deny(_)
    ));
    assert!(matches!(
        authorizer.authorize(
            &call(
                "t1-path-0004",
                "fs.read",
                r#"{"path":"config%2f%2eenv"}"#,
            ),
            context(&empty, &fs, None),
        ),
        Decision::Ask(pending) if pending.floor_required()
    ));
    assert!(matches!(
        authorizer.authorize(
            &call("t1-compound-path-0003", "fs.read", r#"{"text_file":"config%2f%2eenv"}"#),
            context(&empty, &fs, None),
        ),
        Decision::Ask(pending) if pending.floor_required()
    ));

    // Session scope expressly permits this tool+risk with any arguments for
    // the rest of the session. It must therefore allow both the approved and
    // changed commands; constrained scopes below are intentionally narrower.
    let Decision::Ask(pending) = authorizer.authorize(
        &open_chrome,
        context(&empty, &shell, Some(RiskClass::Privileged)),
    ) else {
        panic!("the privileged shell command must ask");
    };
    let mut session = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    let mut grant_store = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
    session.turn_opened("t1", "test", "model").unwrap();
    authorizer
        .resolve(
            pending,
            UserVerdict::Approve {
                scope: "session".into(),
            },
            &cap,
            &mut session,
            &mut grant_store,
        )
        .unwrap()
        .unwrap();
    let (session_records, _) = session.read_records().unwrap();
    let (grant_records, _) = grant_store.read_records().unwrap();
    let granted = authorizer.load_grants(&session_records, &grant_records);
    let changed_command = call(
        "t1-command-0001",
        "mac-shell.run_command",
        r#"{"command":"/usr/bin/open -a \"Google Chrome\" \"https://other.example\""}"#,
    );
    assert!(matches!(
        authorizer.authorize(
            &open_chrome,
            context(&granted, &shell, Some(RiskClass::Privileged)),
        ),
        Decision::Allow { .. }
    ));
    assert!(matches!(
        authorizer.authorize(
            &changed_command,
            context(&granted, &shell, Some(RiskClass::Privileged)),
        ),
        Decision::Allow { .. }
    ));

    let Decision::Ask(folder_pending) = authorizer.authorize(
        &open_chrome,
        context(&empty, &shell, Some(RiskClass::Privileged)),
    ) else {
        panic!("the privileged shell command must ask");
    };
    let mut folder_session = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    let mut folder_store = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
    folder_session.turn_opened("t1", "test", "model").unwrap();
    authorizer
        .resolve(
            folder_pending,
            UserVerdict::Approve {
                scope: "folder".into(),
            },
            &cap,
            &mut folder_session,
            &mut folder_store,
        )
        .unwrap()
        .unwrap();
    let (folder_session_records, _) = folder_session.read_records().unwrap();
    let (folder_grant_records, _) = folder_store.read_records().unwrap();
    let folder_granted = authorizer.load_grants(&folder_session_records, &folder_grant_records);
    assert!(matches!(
        authorizer.authorize(
            &open_chrome,
            context(&folder_granted, &shell, Some(RiskClass::Privileged)),
        ),
        Decision::Allow { .. }
    ));
    assert!(matches!(
        authorizer.authorize(
            &changed_command,
            context(&folder_granted, &shell, Some(RiskClass::Privileged)),
        ),
        Decision::Ask(_)
    ));
}
