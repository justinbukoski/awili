//! C1 policy-engine acceptance (contract §10 at this layer). Single test
//! fn: the issue-once statics are process-wide.

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_authz::{Authorizer, AuthzContext, Decision, GrantSet, UserVerdict};
use awili_engine_log::test_bundle;
use awili_engine_log::{classify_with_grants, ExecutionLog, IntentOutcome, IntentStatus, MemLog};

fn call(id: &str, tool: &str, args: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: None,
        name: tool.into(),
        arguments: args.into(),
    }
}

#[test]
fn c1_policy_engine_end_to_end() {
    // An integration test cannot see the crate-private issuers; the shared
    // fixture (unit 4 r7) performed this process's sole issuance. The
    // authorizer consumes the gate by value, so possession is cloned from
    // the fixture's shared reference (feature-gated `Clone`), never
    // re-issued.
    let bundle = test_bundle();
    let cap = bundle.1.clone();
    let gate = bundle.0.clone();
    let empty = GrantSet::load(&gate, &[], &[]);
    let authorizer = Authorizer::new(gate);
    let mut session = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    let mut grants_log = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
    session.turn_opened("t1", "ollama", "qwen3:8b").unwrap();

    // Unit 4 item 3: every AuthzContext carries the authority-minted
    // resolution token for the call being authorized — minted HERE from the
    // fixture's registry authority (in production, the seam mints it at
    // registry-resolution time, the only site that can).
    let resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("builtin:fs"),
        "fs",
        DescriptorDigest([0u8; 32]),
    ));

    let ctx = |g: &'static str| g; // labels only
    let _ = ctx;

    // --- read_only inside folder: policy allow, dispatchable ------------
    let read = call("t1-call-0000", "fs.read", r#"{"path":"notes.md"}"#);
    let d = authorizer.authorize(
        &read,
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
    let Decision::Allow { evidence } = d else {
        panic!("in-folder read must policy-allow");
    };
    assert!(matches!(
        session.intent("t1", &read, evidence).unwrap(),
        IntentOutcome::Dispatchable(_)
    ));

    // --- mutating: asks; approve once; evidence dispatches THIS call ----
    let write = call(
        "t1-call-0001",
        "fs.write",
        r#"{"path":"out.txt","content":"x"}"#,
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
        panic!("mutating must ask");
    };
    assert!(pending.prompt().contains("fs.write"));
    let ev = authorizer
        .resolve(
            pending,
            UserVerdict::Approve {
                scope: "once".into(),
            },
            &cap,
            &mut session,
            &mut grants_log,
        )
        .unwrap()
        .expect("approved");
    assert!(matches!(
        session.intent("t1", &write, ev).unwrap(),
        IntentOutcome::Dispatchable(_)
    ));

    // --- secrets path: ask-minimum even in-folder -----------------------
    let sneaky = call(
        "t1-call-0002",
        "fs.read",
        r#"{"path":"/Users/example/work/.ssh/id_ed25519"}"#,
    );
    match authorizer.authorize(
        &sneaky,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    ) {
        Decision::Ask(_) => {}
        other => panic!("secrets read must ask, got {}", kind(&other)),
    }

    // --- traversal: denied outright + audited ---------------------------
    let esc = call("t1-call-0003", "fs.read", r#"{"path":"../../etc/passwd"}"#);
    match authorizer.authorize(
        &esc,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    ) {
        Decision::Deny(d) => {
            let reason = d.record(&mut session).unwrap();
            assert!(reason.contains("traversal"));
        }
        other => panic!("traversal must deny, got {}", kind(&other)),
    }

    // --- mutating write outside folder: denied --------------------------
    let out = call(
        "t1-call-0004",
        "fs.write",
        r#"{"path":"/tmp/evil","content":"x"}"#,
    );
    match authorizer.authorize(
        &out,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    ) {
        Decision::Deny(d) => {
            let _ = d.record(&mut session).unwrap();
        }
        other => panic!("out-of-folder write must deny, got {}", kind(&other)),
    }

    // --- session-scope grant: approve once, reuse via GrantSet ----------
    let w2 = call(
        "t1-call-0005",
        "fs.write",
        r#"{"path":"a.txt","content":"1"}"#,
    );
    let Decision::Ask(p2) = authorizer.authorize(
        &w2,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    ) else {
        panic!("must ask");
    };
    let ev2 = authorizer
        .resolve(
            p2,
            UserVerdict::Approve {
                scope: "session".into(),
            },
            &cap,
            &mut session,
            &mut grants_log,
        )
        .unwrap()
        .unwrap();
    session.intent("t1", &w2, ev2).unwrap(); // basis "ask" (fresh)

    // Reload grants from the logs: the session grant now matches a LATER
    // equivalent call and allows without asking (basis "grant").
    let (srecs, _) = session.read_records().unwrap();
    let (grecs, _) = grants_log.read_records().unwrap();
    let grants = authorizer_gate_load(&authorizer, &srecs, &grecs);
    assert_eq!(grants.len(), 1);
    // Constraint auto-capture default: the grant is bound to path a.txt.
    // Same path, different content -> allowed without asking.
    let w3 = call(
        "t1-call-0006",
        "fs.write",
        r#"{"path":"a.txt","content":"2"}"#,
    );
    let d = authorizer.authorize(
        &w3,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &grants,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    );
    let Decision::Allow { evidence } = d else {
        panic!("session grant must allow the same-path write without asking");
    };
    assert_eq!(
        evidence.scope_root(),
        Some("/Users/example/work"),
        "scope_root handshake"
    );
    session.intent("t1", &w3, evidence).unwrap();

    // RC1-I2 (amended): a SESSION grant covers the tool at this risk class
    // for the rest of the session — a different path in the folder is allowed
    // without asking (round 3b E2-5b). The conservative path constraint stays
    // on folder/always grants (`rc1i2_folder_grant_keeps_its_path`).
    let w3b = call(
        "t1-call-0007b",
        "fs.write",
        r#"{"path":"b.txt","content":"2"}"#,
    );
    assert!(
        matches!(
            authorizer.authorize(
                &w3b,
                AuthzContext {
                    turn_id: "t1",
                    session_id: "s1",
                    folder_root: Some("/Users/example/work"),
                    grants: &grants,
                    registry_floor: None,
                    resolved_tool: &resolved,
                    install_witness: None,
                }
            ),
            Decision::Allow { .. }
        ),
        "a session grant covers another path in the same session"
    );

    // --- deny for a DIFFERENT session id: grant must not apply ----------
    let d = authorizer.authorize(
        &w3,
        AuthzContext {
            turn_id: "t1",
            session_id: "OTHER",
            folder_root: Some("/Users/example/work"),
            grants: &grants,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    );
    assert!(
        matches!(d, Decision::Ask(_)),
        "session grant leaked across sessions"
    );

    // --- REGRESSIONS for the patch-grade review fixes -------------------
    // work-evil: sibling dir sharing the root as string prefix = OUTSIDE.
    let evil_sib = call(
        "t1-call-0007",
        "fs.write",
        r#"{"path":"/Users/example/work-evil/x","content":"y"}"#,
    );
    match authorizer.authorize(
        &evil_sib,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    ) {
        Decision::Deny(d) => {
            let _ = d.record(&mut session).unwrap();
        }
        other => panic!(
            "prefix-sibling path must be outside the folder, got {}",
            kind(&other)
        ),
    }

    // RequireAsk beats grants: session grant exists for fs.write, but a
    // secrets-touching write must STILL ask.
    let secret_write = call(
        "t1-call-0008",
        "fs.write",
        r#"{"path":"tokens/api_token.txt","content":"z"}"#,
    );
    assert!(
        matches!(
            authorizer.authorize(
                &secret_write,
                AuthzContext {
                    turn_id: "t1",
                    session_id: "s1",
                    folder_root: Some("/Users/example/work"),
                    grants: &grants,
                    registry_floor: None,
                    resolved_tool: &resolved,
                    install_witness: None,
                }
            ),
            Decision::Ask(_)
        ),
        "secrets floor must not be overridable by grants"
    );

    // Recursive alias/whitespace forms escalate via parsed JSON.
    let sneaky_rm = call(
        "t1-call-0009",
        "fs.edit",
        r#"{ "recursive" : "yes", "path": "d" }"#,
    );
    let d = authorizer.authorize(
        &sneaky_rm,
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
    match d {
        Decision::Ask(p) => assert_eq!(p.risk(), awili_engine_log::authz::RiskClass::Irreversible),
        other => panic!(
            "truthy recursive alias must escalate+ask, got {}",
            kind(&other)
        ),
    }

    // Registry floor is an input and only escalates.
    let floored = authorizer.authorize(
        &read,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: Some(awili_engine_log::authz::RiskClass::Mutating),
            resolved_tool: &resolved,
            install_witness: None,
        },
    );
    assert!(
        matches!(floored, Decision::Ask(_)),
        "registry floor must escalate fs.read to mutating->ask"
    );

    // --- 12x-encoded traversal reaches the fixed point and is denied ----
    let mut enc = "../".to_string();
    for _ in 0..12 {
        enc = enc
            .replace('%', "%25")
            .replace('.', "%2e")
            .replace('/', "%2f");
    }
    let deep_enc = call("t1-call-0010", "fs.read", &format!(r#"{{"path":"{enc}"}}"#));
    match authorizer.authorize(
        &deep_enc,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    ) {
        Decision::Deny(d) => {
            let _ = d.record(&mut session).unwrap();
        }
        other => panic!(
            "deeply-encoded traversal under a path key must deny, got {}",
            kind(&other)
        ),
    }

    // --- invalid UTF-8 escape (%ff) must not panic and still screens ----
    let weird = call(
        "t1-call-0011",
        "fs.read",
        r#"{"path":"%ff%2e%2e%2fsecret"}"#,
    );
    match authorizer.authorize(
        &weird,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: None,
            resolved_tool: &resolved,
            install_witness: None,
        },
    ) {
        Decision::Deny(d) => {
            let _ = d.record(&mut session).unwrap();
        }
        Decision::Ask(_) => {} // traversal after decode may deny or the
        // secrets word may force ask — either is safe; panicking is not.
        Decision::Allow { .. } => panic!("%ff-prefixed traversal must not slip through"),
    }

    // --- RC1-M-FIX1: an MCP tool by its declared class -------------------
    // A read-only MCP tool (its server's annotations, the registry floor)
    // runs without a card; one its server marks as writing still asks.
    let mcp_resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("mcp:interlock"),
        "recall",
        DescriptorDigest([1u8; 32]),
    ));
    let recall = call("t1-call-0090", "recall", r#"{"query":"hostd outage"}"#);
    let d = authorizer.authorize(
        &recall,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: Some(awili_engine_log::authz::RiskClass::ReadOnly),
            resolved_tool: &mcp_resolved,
            install_witness: None,
        },
    );
    assert!(
        matches!(d, Decision::Allow { .. }),
        "a read-only MCP tool runs without a card: {}",
        kind(&d)
    );
    let remember = call("t1-call-0091", "remember", r#"{"subject":"x"}"#);
    let d = authorizer.authorize(
        &remember,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/Users/example/work"),
            grants: &empty,
            registry_floor: Some(awili_engine_log::authz::RiskClass::Mutating),
            resolved_tool: &mcp_resolved,
            install_witness: None,
        },
    );
    assert!(
        matches!(d, Decision::Ask(_)),
        "a writing MCP tool still asks: {}",
        kind(&d)
    );

    // --- full-log replay: nothing AuthzInvalid --------------------------
    let (srecs, _) = session.read_records().unwrap();
    let (grecs, _) = grants_log.read_records().unwrap();
    let bad: Vec<IntentStatus> = classify_with_grants(&srecs, &grecs)
        .into_iter()
        .flat_map(|t| t.intents)
        .map(|i| i.status)
        .filter(|s| matches!(s, IntentStatus::AuthzInvalid { .. }))
        .collect();
    assert!(
        bad.is_empty(),
        "well-formed C1 flows must replay clean: {bad:?}"
    );
}

fn authorizer_gate_load(
    a: &Authorizer,
    s: &[awili_engine_log::schema::LogRecordV1],
    g: &[awili_engine_log::schema::LogRecordV1],
) -> GrantSet {
    a.load_grants(s, g)
}

fn kind(d: &Decision) -> &'static str {
    match d {
        Decision::Allow { .. } => "Allow",
        Decision::Ask(_) => "Ask",
        Decision::Deny { .. } => "Deny",
    }
}

/// Unit 5 §5: the install-arm matching discipline at the authorize level —
/// the right identity's witness allows a privileged call on the stored
/// install grant; a DIFFERENT identity's witness matches nothing; a
/// witness for a different origin than the RESOLVED tool is discarded
/// before matching (fail closed on every arm).
#[test]
fn install_witness_matching_is_identity_and_origin_bound() {
    use awili_engine_log::schema::LogRecordV1;
    use awili_tools_shim::*;

    let bundle = test_bundle();
    let gate = bundle.0.clone();
    let authorizer = Authorizer::new(bundle.0.clone());
    let grant_records = vec![LogRecordV1::InstallTransition {
        approval_ref: "appr-grants-0000".into(),
        session_id: "grants".into(),
        tool: "run".into(),
        risk_class: "privileged".into(),
        constraints: serde_json::json!({
            "__origin": "mcp:mac-shell",
            "__setup_id": "setup-1",
            "__pinned_requirement": "cdhash H\"00\"",
        })
        .to_string(),
        supersedes: vec![],
    }];
    let grants = GrantSet::load(&gate, &[], &grant_records);
    let resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("mcp:mac-shell"),
        "run",
        DescriptorDigest([0u8; 32]),
    ));
    // Install scope is bound to the attested tool identity, not an argument
    // profile, so a command-bearing call still matches its valid witness.
    let run = call(
        "t1-call-0000",
        "run",
        r#"{"command":"safe-command --flag"}"#,
    );
    let authorize_with = |facts: &awili_attest_types::SetupRecordFacts,
                          resolved: &awili_engine_log::ResolvedTool|
     -> &'static str {
        let generation = awili_engine::generation::GenerationAllocator::new().next();
        let token = awili_attest_types::LaunchToken::mint_verified(facts, generation);
        let awili_attest_types::BoundLaunch::Verified(v) =
            token.bind_scoped(&facts.origin, generation).unwrap()
        else {
            panic!("verified");
        };
        v.with_install_witness(|w| {
            kind(&authorizer.authorize(
                &run,
                AuthzContext {
                    turn_id: "t1",
                    session_id: "s1",
                    folder_root: Some("/w"),
                    grants: &grants,
                    registry_floor: Some(awili_engine_log::RiskClass::Privileged),
                    resolved_tool: resolved,
                    install_witness: Some(w),
                },
            ))
        })
    };

    let right = facts("mcp:mac-shell", "setup-1", "cdhash H\"00\"");
    assert_eq!(authorize_with(&right, &resolved), "Allow");

    // Wrong identity: same origin, different setup — matches nothing.
    let wrong = facts("mcp:mac-shell", "setup-2", "cdhash H\"ff\"");
    assert_eq!(authorize_with(&wrong, &resolved), "Ask");

    // Witness origin != the RESOLVED tool's origin: discarded pre-match.
    let foreign = facts("mcp:other", "setup-1", "cdhash H\"00\"");
    assert_eq!(authorize_with(&foreign, &resolved), "Ask");

    // No witness at all: the install arm contributes nothing.
    let d = authorizer.authorize(
        &run,
        AuthzContext {
            turn_id: "t1",
            session_id: "s1",
            folder_root: Some("/w"),
            grants: &grants,
            registry_floor: Some(awili_engine_log::RiskClass::Privileged),
            resolved_tool: &resolved,
            install_witness: None,
        },
    );
    assert_eq!(kind(&d), "Ask");
}

mod awili_tools_shim {
    pub fn facts(origin: &str, setup: &str, pin: &str) -> awili_attest_types::SetupRecordFacts {
        awili_attest_types::SetupRecordFacts {
            origin: awili_engine::ToolOrigin::new(origin),
            setup_id: awili_attest_types::SetupIdentityId::parse(setup).unwrap(),
            pinned_requirement: pin.into(),
            expected_protocol_name: "srv".into(),
            launch_kind: awili_attest_types::LaunchKind::Direct,
            program: "/usr/local/lib/awili/srv".into(),
        }
    }
}

/// Impl review r1 P1-2 (matching half): a malformed transition retires
/// nothing in the LIVE grant set — the innocent install grant it targeted
/// still matches its witness; the malformed one contributes no grant.
#[test]
fn malformed_transition_leaves_the_grant_set_intact() {
    use awili_engine_log::schema::LogRecordV1;
    use awili_tools_shim::facts;

    let bundle = test_bundle();
    let gate = bundle.0.clone();
    let mk = |aref: &str, setup: &str, pin: &str, supersedes: Vec<String>| {
        LogRecordV1::InstallTransition {
            approval_ref: aref.into(),
            session_id: "grants".into(),
            tool: "run".into(),
            risk_class: "privileged".into(),
            constraints: serde_json::json!({
                "__origin": "mcp:mac-shell",
                "__setup_id": setup,
                "__pinned_requirement": pin,
            })
            .to_string(),
            supersedes,
        }
    };
    // r2 P1 strictness at the LIVE loader: missing-key, non-string, and
    // extra-key bindings admit nothing and retire nothing — even when
    // their supersedes points at the good grant.
    let raw = |aref: &str, constraints: serde_json::Value, supersedes: Vec<String>| {
        LogRecordV1::InstallTransition {
            approval_ref: aref.into(),
            session_id: "grants".into(),
            tool: "run".into(),
            risk_class: "privileged".into(),
            constraints: constraints.to_string(),
            supersedes,
        }
    };
    let records = vec![
        mk("appr-grants-0000", "setup-a", "cdhash H\"00\"", vec![]),
        // Same identity as its target: illegal supersession, whole record out.
        mk(
            "appr-grants-0001",
            "setup-a",
            "cdhash H\"00\"",
            vec!["appr-grants-0000".into()],
        ),
        raw(
            "appr-grants-0002",
            serde_json::json!({"__origin": "mcp:mac-shell"}),
            vec!["appr-grants-0000".into()],
        ),
        raw(
            "appr-grants-0003",
            serde_json::json!({
                "__origin": "mcp:mac-shell",
                "__setup_id": 7,
                "__pinned_requirement": "p",
            }),
            vec![],
        ),
        raw(
            "appr-grants-0004",
            serde_json::json!({
                "__origin": "mcp:mac-shell",
                "__setup_id": "setup-x",
                "__pinned_requirement": "p",
                "extra": "key",
            }),
            vec![],
        ),
    ];
    let grants = GrantSet::load(&gate, &[], &records);
    let authorizer = Authorizer::new(bundle.0.clone());
    let resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("mcp:mac-shell"),
        "run",
        DescriptorDigest([0u8; 32]),
    ));
    let run = call("t1-call-0000", "run", "{}");
    let right = facts("mcp:mac-shell", "setup-a", "cdhash H\"00\"");
    let generation = awili_engine::generation::GenerationAllocator::new().next();
    let token = awili_attest_types::LaunchToken::mint_verified(&right, generation);
    let awili_attest_types::BoundLaunch::Verified(v) =
        token.bind_scoped(&right.origin, generation).unwrap()
    else {
        panic!("verified");
    };
    let k = v.with_install_witness(|w| {
        kind(&authorizer.authorize(
            &run,
            AuthzContext {
                turn_id: "t1",
                session_id: "s1",
                folder_root: Some("/w"),
                grants: &grants,
                registry_floor: Some(awili_engine_log::RiskClass::Privileged),
                resolved_tool: &resolved,
                install_witness: Some(w),
            },
        ))
    });
    assert_eq!(
        k, "Allow",
        "the innocent grant survives the malformed record"
    );
}

/// RC1-I2: the grants that outlive the session keep the conservative exact-path
/// constraint — a FOLDER grant for c.txt does not cover d.txt — and a session
/// grant never reaches another session.
#[test]
fn rc1i2_folder_grant_keeps_its_path() {
    let bundle = test_bundle();
    let cap = bundle.1.clone();
    let gate = bundle.0.clone();
    let empty = GrantSet::load(&gate, &[], &[]);
    let authorizer = Authorizer::new(gate);
    let mut session = ExecutionLog::open(MemLog::new(), "s9").unwrap();
    let mut grants_log = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
    session.turn_opened("t9", "ollama", "m").unwrap();
    let resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("builtin:fs"),
        "fs",
        DescriptorDigest([0u8; 32]),
    ));
    let ask = |grants: &GrantSet, session_id: &str, id: &str, path: &str| {
        authorizer.authorize(
            &call(
                id,
                "fs.write",
                &format!("{{\"path\":\"{path}\",\"content\":\"x\"}}"),
            ),
            AuthzContext {
                turn_id: "t9",
                session_id,
                folder_root: Some("/Users/example/work"),
                grants,
                registry_floor: None,
                resolved_tool: &resolved,
                install_witness: None,
            },
        )
    };
    let Decision::Ask(pending) = ask(&empty, "s9", "t9-call-0", "c.txt") else {
        panic!("mutating must ask");
    };
    authorizer
        .resolve(
            pending,
            UserVerdict::Approve {
                scope: "folder".into(),
            },
            &cap,
            &mut session,
            &mut grants_log,
        )
        .unwrap()
        .unwrap();
    let (srecs, _) = session.read_records().unwrap();
    let (grecs, _) = grants_log.read_records().unwrap();
    let grants = authorizer_gate_load(&authorizer, &srecs, &grecs);
    // Another session: only the folder grant can apply.
    assert!(
        matches!(
            ask(&grants, "s10", "t9-call-1", "c.txt"),
            Decision::Allow { .. }
        ),
        "the folder grant covers its own path"
    );
    assert!(
        matches!(ask(&grants, "s10", "t9-call-2", "d.txt"), Decision::Ask(_)),
        "a folder grant does not cover another path"
    );
}
