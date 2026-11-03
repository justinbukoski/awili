//! RC2-ENG3: the live grant loader and replay judge install supersession by
//! ONE rule (F0588), and seeding keeps at most one active install grant per
//! (origin, tool) — checked with the production `GrantSet`, not a test shim
//! (F0558, F0560).

use awili_attest_types::{BoundLaunch, LaunchKind, LaunchToken, SetupIdentityId, SetupRecordFacts};
use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::generation::GenerationAllocator;
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_authz::GrantSet;
use awili_engine_log::authz::{AuthzEvidence, GrantRef, InstallSeedCandidate, RiskClass};
use awili_engine_log::grant_service::GrantStoreService;
use awili_engine_log::schema::LogRecordV1;
use awili_engine_log::{classify_with_grants, test_bundle, ExecutionLog, IntentStatus, MemLog};

fn facts(origin: &str, setup: &str, pin: &str) -> SetupRecordFacts {
    SetupRecordFacts {
        origin: ToolOrigin::new(origin),
        setup_id: SetupIdentityId::parse(setup).unwrap(),
        pinned_requirement: pin.into(),
        expected_protocol_name: "srv".into(),
        launch_kind: LaunchKind::Direct,
        program: "/usr/local/lib/awili/srv".into(),
    }
}

fn call(tool: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId("t1-call-0000".into()),
        provider_call_id: None,
        name: tool.into(),
        arguments: "{}".into(),
    }
}

fn binding(origin: &str, setup: &str, pin: &str) -> String {
    serde_json::json!({
        "__origin": origin,
        "__setup_id": setup,
        "__pinned_requirement": pin,
    })
    .to_string()
}

/// The approval ref the LIVE grant set matches for a launch of this
/// identity, if any.
fn live_match(
    grants: &GrantSet,
    origin: &str,
    setup: &str,
    pin: &str,
    tool: &str,
    risk: RiskClass,
) -> Option<String> {
    let f = facts(origin, setup, pin);
    let g = GenerationAllocator::new().next();
    let token = LaunchToken::mint_verified(&f, g);
    let BoundLaunch::Verified(v) = token.bind_scoped(&f.origin, g).unwrap() else {
        panic!("verified launch");
    };
    v.with_install_witness(|w| {
        grants
            .matching(&call(tool), risk, "s1", None, Some(&w))
            .map(|m| m.approval_ref.clone())
    })
}

/// What REPLAY says about an intent authorized by the grant `approval_ref`.
fn replay_status(store: &[LogRecordV1], approval_ref: &str) -> IntentStatus {
    let bundle = test_bundle();
    let resolved = bundle.2.mint(ToolIdentity::new(
        ToolOrigin::new("mcp:srv"),
        "run",
        DescriptorDigest([0u8; 32]),
    ));
    let gref = GrantRef::from_grant_set(&bundle.0, approval_ref, RiskClass::Privileged);
    let c = call("run");
    let ev = AuthzEvidence::from_grant(&gref, &resolved, "t1", &c, None).unwrap();
    let mut session = ExecutionLog::open(MemLog::new(), "s1").unwrap();
    session.turn_opened("t1", "ollama", "m").unwrap();
    session.intent("t1", &c, ev).unwrap();
    let (recs, _) = session.read_records().unwrap();
    let turns = classify_with_grants(&recs, store);
    turns[0].intents[0].status.clone()
}

fn standalone(aref: &str, constraints: String) -> LogRecordV1 {
    LogRecordV1::ApprovalRecorded {
        approval_ref: aref.into(),
        decision: "approved".into(),
        scope: "install".into(),
        turn_id: String::new(),
        awili_call_id: String::new(),
        normalized_call_hash: String::new(),
        session_id: "grants".into(),
        tool: "run".into(),
        risk_class: "privileged".into(),
        folder_root: None,
        constraints,
        granted_at_ms: None,
    }
}

fn transition(aref: &str, constraints: String, supersedes: &[&str]) -> LogRecordV1 {
    LogRecordV1::InstallTransition {
        approval_ref: aref.into(),
        session_id: "grants".into(),
        tool: "run".into(),
        risk_class: "privileged".into(),
        constraints,
        supersedes: supersedes.iter().map(|s| s.to_string()).collect(),
    }
}

/// F0588: a transition superseding a standalone install-scope approval is
/// admitted by the live loader AND by replay (before: live admitted it,
/// replay called its grant dangling); one superseding a target whose
/// binding is not strict is admitted by neither (before: live admitted it).
#[test]
fn rc2eng3_live_and_replay_judge_install_supersession_alike() {
    let gate = &test_bundle().0;

    // A strict standalone target: admitted everywhere.
    let store = vec![
        standalone("appr-grants-0000", binding("mcp:srv", "setup-a", "pin-a")),
        transition(
            "appr-grants-0001",
            binding("mcp:srv", "setup-b", "pin-b"),
            &["appr-grants-0000"],
        ),
    ];
    let grants = GrantSet::load(gate, &[], &store);
    assert_eq!(
        live_match(
            &grants,
            "mcp:srv",
            "setup-b",
            "pin-b",
            "run",
            RiskClass::Privileged
        ),
        Some("appr-grants-0001".into())
    );
    assert_eq!(
        live_match(
            &grants,
            "mcp:srv",
            "setup-a",
            "pin-a",
            "run",
            RiskClass::Privileged
        ),
        None,
        "the superseded approval is retired live"
    );
    assert!(
        !matches!(
            replay_status(&store, "appr-grants-0001"),
            IntentStatus::AuthzInvalid { .. }
        ),
        "replay admits the same transition"
    );

    // An over-keyed standalone target: the transition is admitted nowhere.
    let mut loose =
        serde_json::from_str::<serde_json::Value>(&binding("mcp:srv", "setup-a", "pin-a")).unwrap();
    loose["extra"] = serde_json::json!("key");
    let store = vec![
        standalone("appr-grants-0000", loose.to_string()),
        transition(
            "appr-grants-0001",
            binding("mcp:srv", "setup-b", "pin-b"),
            &["appr-grants-0000"],
        ),
    ];
    let grants = GrantSet::load(gate, &[], &store);
    assert_eq!(
        live_match(
            &grants,
            "mcp:srv",
            "setup-b",
            "pin-b",
            "run",
            RiskClass::Privileged
        ),
        None,
        "live refuses the transition"
    );
    assert!(
        matches!(
            replay_status(&store, "appr-grants-0001"),
            IntentStatus::AuthzInvalid { .. }
        ),
        "replay refuses it too"
    );
}

/// F0560 / F0558: seeding new identities for two origins and two tools, the
/// PRODUCTION loader holds exactly one active install grant per (origin,
/// tool) after every seed — the newest identity's — and no older identity
/// matches.
#[test]
fn rc2eng3_seeding_keeps_one_install_grant_per_origin_and_tool() {
    let bundle = test_bundle();
    let dir = std::env::temp_dir().join(format!(
        "awili-rc2eng3-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let svc = GrantStoreService::init(dir.join("grants.log"), None).unwrap();
    let tools = [
        ("run_command", RiskClass::Privileged),
        ("read_file", RiskClass::ReadOnly),
    ];
    let seeds = [
        ("mcp:one", "setup-a", "pin-a"),
        ("mcp:one", "setup-b", "pin-b"),
        ("mcp:two", "setup-x", "pin-x"),
        ("mcp:one", "setup-c", "pin-c"),
        ("mcp:one", "setup-c", "pin-c2"),
        ("mcp:two", "setup-y", "pin-y"),
    ];
    let mut seen: Vec<(&str, &str, &str)> = Vec::new();
    for (origin, setup, pin) in seeds {
        let f = facts(origin, setup, pin);
        let g = GenerationAllocator::new().next();
        let token = LaunchToken::mint_verified(&f, g);
        let BoundLaunch::Verified(v) = token.bind_scoped(&f.origin, g).unwrap() else {
            panic!("verified launch");
        };
        let floors = bundle.3.mint_floors(
            f.origin.clone(),
            g,
            tools.iter().map(|(n, r)| (n.to_string(), *r)).collect(),
        );
        let candidate = InstallSeedCandidate::mint(v, floors).unwrap();
        let out = svc.seed_install_grants(&bundle.1, candidate).unwrap();
        assert_eq!(out.seeded.len(), 2, "{origin} {setup} {pin}: {out:?}");
        seen.push((origin, setup, pin));

        let store = svc.with_store(|log| log.read_records()).unwrap().0;
        let grants = GrantSet::load(&bundle.0, &[], &store);
        let origins: std::collections::BTreeSet<&str> = seen.iter().map(|s| s.0).collect();
        assert_eq!(
            grants.len(),
            origins.len() * tools.len(),
            "one active install grant per (origin, tool) after {seen:?}"
        );
        for o in &origins {
            let newest = seen.iter().rev().find(|s| s.0 == *o).unwrap();
            for (tool, risk) in tools {
                for s in seen.iter().filter(|s| s.0 == *o) {
                    let m = live_match(&grants, s.0, s.1, s.2, tool, risk);
                    if s == newest {
                        assert!(m.is_some(), "{s:?} {tool} is the active grant");
                    } else {
                        assert_eq!(m, None, "{s:?} {tool} was superseded");
                    }
                }
            }
        }
    }
}

/// RC2-ENGF2 (RC2-ENG slice review P2): a standalone install approval the
/// shared rule ([`awili_engine_log::replay::install_state`]) does not admit
/// — missing, extra or non-string binding keys — authorizes nothing: the
/// live loader does not hold it, and replay calls a step that names ITS OWN
/// ref AuthzInvalid. Before RC2-ENGF2 both took any approved install-scope
/// record (replay then trusted the step; live held a grant that no launch
/// could match). A strict one still loads and still replays.
#[test]
fn rc2engf2_a_malformed_standalone_install_approval_authorizes_nothing() {
    let gate = &test_bundle().0;
    let strict = binding("mcp:srv", "setup-a", "pin-a");
    let mut over_keyed = serde_json::from_str::<serde_json::Value>(&strict).unwrap();
    over_keyed["extra"] = serde_json::json!("key");
    let mut non_string = serde_json::from_str::<serde_json::Value>(&strict).unwrap();
    non_string["__setup_id"] = serde_json::json!(7);
    let malformed = [
        over_keyed.to_string(),
        non_string.to_string(),
        serde_json::json!({ "__origin": "mcp:srv", "__setup_id": "setup-a" }).to_string(),
        "{}".to_string(),
    ];
    for constraints in malformed {
        let store = vec![standalone("appr-grants-0000", constraints.clone())];
        assert_eq!(
            GrantSet::load(gate, &[], &store).len(),
            0,
            "live holds no grant for {constraints}"
        );
        assert!(
            matches!(
                replay_status(&store, "appr-grants-0000"),
                IntentStatus::AuthzInvalid { .. }
            ),
            "replay refuses the approval's own ref for {constraints}"
        );
    }
    let store = vec![standalone("appr-grants-0000", strict)];
    assert_eq!(GrantSet::load(gate, &[], &store).len(), 1);
    assert_eq!(
        live_match(
            &GrantSet::load(gate, &[], &store),
            "mcp:srv",
            "setup-a",
            "pin-a",
            "run",
            RiskClass::Privileged
        ),
        Some("appr-grants-0000".into())
    );
    assert!(!matches!(
        replay_status(&store, "appr-grants-0000"),
        IntentStatus::AuthzInvalid { .. }
    ));
}
