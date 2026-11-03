//! Unit 5 slice s3 tests: the atomic InstallTransition seed — tombstone
//! rule (any approval, active/revoked/superseded, blocks auto-reseed;
//! A→B→A never reseeds A), supersession bounding, the append-nothing
//! invariant, torn-transition recovery, and load/matching semantics.

use std::sync::OnceLock;

use crate::authz::{HostApprovalCapability, InstallSeedAuthority, InstallSeedCandidate, RiskClass};
use crate::grant_service::GrantStoreService;
use crate::schema::LogRecordV1;
use crate::writer::ExecutionLog;
use awili_attest_types::{BoundLaunch, LaunchKind, LaunchToken, SetupRecordFacts};
use awili_engine::generation::ConnectionGeneration;
use awili_engine::ToolOrigin;

/// The in-crate suite shares one process: the INSTALL bit is claimable
/// once, so every s3 test borrows the same authority.
fn auth() -> &'static InstallSeedAuthority {
    static A: OnceLock<InstallSeedAuthority> = OnceLock::new();
    A.get_or_init(|| InstallSeedAuthority::issue().expect("install bit free in this process"))
}

fn cap() -> HostApprovalCapability {
    HostApprovalCapability::test_only()
}

fn origin() -> ToolOrigin {
    ToolOrigin::new("mcp:mac-shell")
}

fn generation(skip: u64) -> ConnectionGeneration {
    let mut a = awili_engine::generation::GenerationAllocator::new();
    for _ in 0..skip {
        let _ = a.next();
    }
    a.next()
}

/// A candidate for `setup` identity at `gen`, with the given floors.
fn candidate(
    setup: &str,
    generation: ConnectionGeneration,
    tools: &[(&str, RiskClass)],
) -> InstallSeedCandidate {
    let facts = SetupRecordFacts {
        origin: origin(),
        setup_id: awili_attest_types::SetupIdentityId::parse(setup).unwrap(),
        pinned_requirement: format!("cdhash H\"{setup}\""),
        expected_protocol_name: "mac-shell".into(),
        launch_kind: LaunchKind::Direct,
        program: "/usr/local/lib/awili/mac-shell".into(),
    };
    let token = LaunchToken::mint_verified(&facts, generation);
    let BoundLaunch::Verified(v) = token.bind_scoped(&origin(), generation).unwrap() else {
        panic!("verified bind");
    };
    let floors = auth().mint_floors(
        origin(),
        generation,
        tools.iter().map(|(n, r)| (n.to_string(), *r)).collect(),
    );
    InstallSeedCandidate::mint(v, floors).expect("origin+generation match")
}

fn store(tag: &str) -> &'static GrantStoreService {
    let d = std::env::temp_dir().join(format!(
        "awili-u5-s3-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    GrantStoreService::init(d.join("grants.log"), None).unwrap()
}

fn records(svc: &GrantStoreService) -> Vec<LogRecordV1> {
    svc.with_store(|log| log.read_records()).unwrap().0
}

/// Happy path + the append-nothing invariant: one transition per tool,
/// irreversible skipped and named, a repeat seed appends ZERO records and
/// reports everything tombstoned.
#[test]
fn seed_mints_one_transition_per_tool_and_repeats_append_nothing() {
    let svc = store("happy");
    let g = generation(0);
    let out = svc
        .seed_install_grants(
            &cap(),
            candidate(
                "setup-a",
                g,
                &[
                    ("run_command", RiskClass::Privileged),
                    ("read_file", RiskClass::ReadOnly),
                    ("wipe_disk", RiskClass::Irreversible),
                ],
            ),
        )
        .unwrap();
    assert_eq!(out.seeded, vec!["run_command", "read_file"]);
    assert_eq!(out.skipped_irreversible, vec!["wipe_disk"]);
    assert!(out.tombstoned.is_empty() && out.superseded.is_empty());
    let n = records(svc).len();
    assert_eq!(
        records(svc)
            .iter()
            .filter(|r| matches!(r, LogRecordV1::InstallTransition { .. }))
            .count(),
        2
    );
    // Append-nothing: same identity, same catalog.
    let out2 = svc
        .seed_install_grants(
            &cap(),
            candidate(
                "setup-a",
                g,
                &[
                    ("run_command", RiskClass::Privileged),
                    ("read_file", RiskClass::ReadOnly),
                ],
            ),
        )
        .unwrap();
    assert!(out2.seeded.is_empty());
    assert_eq!(out2.tombstoned, vec!["run_command", "read_file"]);
    assert_eq!(records(svc).len(), n, "repeat seed appends nothing");
}

/// The tombstone rule across identities (rulings 5): a NEW identity seeds
/// and machine-supersedes the old ACTIVE approvals; returning to the old
/// identity (A→B→A) does NOT reseed A — its superseded approvals are
/// tombstones. User revocation likewise tombstones forever.
#[test]
fn a_b_a_never_reseeds_and_revocation_tombstones_forever() {
    let svc = store("aba");
    let tools = [("run_command", RiskClass::Privileged)];
    let out_a = svc
        .seed_install_grants(&cap(), candidate("setup-a", generation(0), &tools))
        .unwrap();
    assert_eq!(out_a.seeded, vec!["run_command"]);
    let ref_a = {
        let recs = records(svc);
        recs.iter()
            .find_map(|r| match r {
                LogRecordV1::InstallTransition { approval_ref, .. } => Some(approval_ref.clone()),
                _ => None,
            })
            .unwrap()
    };

    // B supersedes A's active approval, inside B's own transition.
    let out_b = svc
        .seed_install_grants(&cap(), candidate("setup-b", generation(1), &tools))
        .unwrap();
    assert_eq!(out_b.seeded, vec!["run_command"]);
    assert_eq!(out_b.superseded, vec![ref_a.clone()]);

    // A again: its key is tombstoned by its own (now superseded) approval.
    let n = records(svc).len();
    let out_a2 = svc
        .seed_install_grants(&cap(), candidate("setup-a", generation(2), &tools))
        .unwrap();
    assert!(out_a2.seeded.is_empty());
    assert_eq!(out_a2.tombstoned, vec!["run_command"]);
    assert_eq!(records(svc).len(), n, "A->B->A appends nothing");

    // Revoke B's grant; C seeds fresh (new key), but B can never reseed.
    let ref_b = records(svc)
        .iter()
        .find_map(|r| match r {
            LogRecordV1::InstallTransition {
                approval_ref,
                supersedes,
                ..
            } if !supersedes.is_empty() => Some(approval_ref.clone()),
            _ => None,
        })
        .unwrap();
    svc.with_store(|log| log.append_grant_revoked(&ref_b))
        .unwrap();
    let out_b2 = svc
        .seed_install_grants(&cap(), candidate("setup-b", generation(3), &tools))
        .unwrap();
    assert!(out_b2.seeded.is_empty(), "revoked key never auto-reseeds");
    assert_eq!(out_b2.tombstoned, vec!["run_command"]);
}

/// Load/matching semantics: the embedded approval is an install grant
/// matching ONLY through a witness with the same identity; a superseded
/// ref contributes nothing; the active set is bounded to one per
/// (origin, tool).
#[test]
fn transition_grants_load_and_match_only_with_the_identity_witness() {
    let svc = store("load");
    let tools = [("run_command", RiskClass::Privileged)];
    svc.seed_install_grants(&cap(), candidate("setup-a", generation(0), &tools))
        .unwrap();
    svc.seed_install_grants(&cap(), candidate("setup-b", generation(1), &tools))
        .unwrap();
    let recs = records(svc);

    let grants = awili_engine_authz_shim::load(&recs);
    // Exactly ONE active install grant for the tool (B's); A's is
    // superseded.
    assert_eq!(grants.install_count("run_command"), 1);

    // Matching: the identity witness for B matches; A's identity does not.
    let facts_b = SetupRecordFacts {
        origin: origin(),
        setup_id: awili_attest_types::SetupIdentityId::parse("setup-b").unwrap(),
        pinned_requirement: "cdhash H\"setup-b\"".into(),
        expected_protocol_name: "mac-shell".into(),
        launch_kind: LaunchKind::Direct,
        program: "/usr/local/lib/awili/mac-shell".into(),
    };
    let g = generation(5);
    let token = LaunchToken::mint_verified(&facts_b, g);
    let BoundLaunch::Verified(vb) = token.bind_scoped(&origin(), g).unwrap() else {
        panic!()
    };
    assert!(vb.with_install_witness(|w| grants.matches_install("run_command", Some(&w))));

    let facts_a = SetupRecordFacts {
        setup_id: awili_attest_types::SetupIdentityId::parse("setup-a").unwrap(),
        pinned_requirement: "cdhash H\"setup-a\"".into(),
        ..facts_b
    };
    let token = LaunchToken::mint_verified(&facts_a, generation(6));
    let BoundLaunch::Verified(va) = token.bind_scoped(&origin(), generation(6)).unwrap() else {
        panic!()
    };
    assert!(
        !va.with_install_witness(|w| grants.matches_install("run_command", Some(&w))),
        "a different identity's witness matches nothing"
    );
    assert!(
        !grants.matches_install("run_command", None),
        "no witness, no match — fail closed"
    );
}

/// A torn transition at the tail is repaired before the next append and
/// the store reconstructs the complete OLD state (ruling 12): the half-
/// written transition never contributes a grant or a supersession.
#[test]
fn torn_transition_recovers_to_the_complete_old_state() {
    let svc = store("torn");
    let tools = [("run_command", RiskClass::Privileged)];
    svc.seed_install_grants(&cap(), candidate("setup-a", generation(0), &tools))
        .unwrap();
    let n = records(svc).len();
    // Tear: half of a would-be transition frame, as a crash would leave.
    let fake = crate::frame::encode(
        serde_json::to_vec(&LogRecordV1::InstallTransition {
            approval_ref: "appr-grants-9999".into(),
            session_id: "grants".into(),
            tool: "run_command".into(),
            risk_class: "privileged".into(),
            constraints: "{}".into(),
            supersedes: vec!["appr-grants-0000".into()],
        })
        .unwrap()
        .as_slice(),
    );
    let path = svc_store_path(svc);
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        // RVS5C-FIX1: tear at 5 bytes — a PROVABLE torn tail (an incomplete
        // header). A complete header plus a partial payload is now UNCERTAIN
        // and refused (spec RVS5C-FIX1, D2), so a half-frame tear no longer
        // recovers: it is refused, and the file is left byte-identical.
        f.write_all(&fake[..5]).unwrap();
    }
    // The next section repairs, and the old state is intact: the original
    // approval is still ACTIVE (the torn supersession never happened).
    let recs = records(svc);
    assert_eq!(recs.len(), n, "torn frame truncated, old state complete");
    assert!(recs.iter().any(
        |r| matches!(r, LogRecordV1::InstallTransition { supersedes, .. } if supersedes.is_empty())
    ));
}

fn svc_store_path(svc: &GrantStoreService) -> std::path::PathBuf {
    svc.store_path_for_tests()
}

/// Minimal shim over awili-engine-authz's GrantSet for in-crate assertions
/// (the real matching lives a crate above; here we verify the RECORD
/// semantics: active-set bounding and identity binding).
/// RC2-ENG3 (F0558): the shim no longer has its own, looser admission rule
/// (it retired every `supersedes` of any transition with an object
/// binding): it takes the admitted set from THE rule the production loader
/// uses, `crate::replay::install_state`; the engine-authz test
/// `rc2_eng3_install` checks the real `GrantSet` too.
mod awili_engine_authz_shim {
    use super::*;

    pub struct Loaded {
        grants: Vec<(String, serde_json::Map<String, serde_json::Value>)>,
    }

    pub fn load(recs: &[LogRecordV1]) -> Loaded {
        let state = crate::replay::install_state(recs);
        let revoked: std::collections::BTreeSet<&String> = recs
            .iter()
            .filter_map(|r| match r {
                LogRecordV1::GrantRevoked { approval_ref } => Some(approval_ref),
                _ => None,
            })
            .collect();
        let grants = state
            .admitted
            .iter()
            .filter(|(r, _)| !state.superseded.contains(*r) && !revoked.contains(r))
            .map(|(_, a)| {
                let c = serde_json::json!({
                    "__origin": a.origin,
                    "__setup_id": a.setup_id,
                    "__pinned_requirement": a.pinned_requirement,
                });
                (a.tool.clone(), c.as_object().cloned().unwrap_or_default())
            })
            .collect();
        Loaded { grants }
    }

    impl Loaded {
        pub fn install_count(&self, tool: &str) -> usize {
            self.grants.iter().filter(|(t, _)| t == tool).count()
        }
        pub fn matches_install(
            &self,
            tool: &str,
            witness: Option<&awili_attest_types::InstallOriginWitness<'_>>,
        ) -> bool {
            let Some(w) = witness else { return false };
            self.grants.iter().any(|(t, c)| {
                t == tool
                    && c.len() == 3
                    && c.get("__origin").and_then(|v| v.as_str()) == Some(&w.origin().origin)
                    && c.get("__setup_id").and_then(|v| v.as_str()) == Some(w.setup_id().as_str())
                    && c.get("__pinned_requirement").and_then(|v| v.as_str())
                        == Some(w.pinned_requirement())
            })
        }
    }
}

/// Impl review r1 P1-1: the general approval path REFUSES "install" — the
/// seed-only scope is unreachable through record_approval (and therefore
/// through the seam's GrantStoreAccess), no matter who holds the
/// capability. Only record_install_grants mints it.
#[test]
fn general_approval_path_refuses_the_install_scope() {
    let mut log = ExecutionLog::open_grant_store(crate::storage::MemLog::new()).unwrap();
    let call = awili_engine::events::ToolCall {
        awili_call_id: awili_engine::events::AwiliCallId("c1".into()),
        provider_call_id: None,
        name: "run".into(),
        arguments: "{}".into(),
    };
    let out = log.record_approval(
        &cap(),
        "install",
        "t1",
        &call,
        RiskClass::Privileged,
        None,
        r#"{"__origin":"mcp:mac-shell","__setup_id":"s","__pinned_requirement":"p"}"#,
    );
    assert!(
        matches!(out, Err(crate::LogError::EvidenceMismatch(ref m)) if m.contains("invalid scope")),
        "{out:?}"
    );
}

/// Impl review r1 P1-2 (replay half): a transition whose supersedes names
/// anything but an OLDER install approval of the same tool with a
/// DIFFERENT identity is admitted NOWHERE — it contributes no approval and
/// retires nothing, so the innocent grant it pointed at keeps authorizing.
#[test]
fn malformed_transition_retires_nothing_and_contributes_nothing() {
    use crate::replay::classify_with_grants;
    let good = LogRecordV1::InstallTransition {
        approval_ref: "appr-grants-0000".into(),
        session_id: "grants".into(),
        tool: "run".into(),
        risk_class: "privileged".into(),
        constraints: serde_json::json!({
            "__origin": "mcp:mac-shell",
            "__setup_id": "setup-a",
            "__pinned_requirement": "pin-a",
        })
        .to_string(),
        supersedes: vec![],
    };
    // Malicious: same identity as its target (not a legal supersession).
    let bad_same_identity = LogRecordV1::InstallTransition {
        approval_ref: "appr-grants-0001".into(),
        session_id: "grants".into(),
        tool: "run".into(),
        risk_class: "privileged".into(),
        constraints: serde_json::json!({
            "__origin": "mcp:mac-shell",
            "__setup_id": "setup-a",
            "__pinned_requirement": "pin-a",
        })
        .to_string(),
        supersedes: vec!["appr-grants-0000".into()],
    };
    // Malicious: supersedes a ref that is not an install approval at all.
    let bad_foreign_ref = LogRecordV1::InstallTransition {
        approval_ref: "appr-grants-0002".into(),
        session_id: "grants".into(),
        tool: "run".into(),
        risk_class: "privileged".into(),
        constraints: serde_json::json!({
            "__origin": "mcp:mac-shell",
            "__setup_id": "setup-b",
            "__pinned_requirement": "pin-b",
        })
        .to_string(),
        supersedes: vec!["appr-folder-9999".into()],
    };
    // r2 P1 strictness: missing key, non-string value, extra key — each
    // shape admits nothing and retires nothing, even with an EMPTY
    // supersedes list, and even when pointed at the good grant.
    let bad_missing_key = LogRecordV1::InstallTransition {
        approval_ref: "appr-grants-0003".into(),
        session_id: "grants".into(),
        tool: "run".into(),
        risk_class: "privileged".into(),
        constraints: serde_json::json!({"__origin": "mcp:mac-shell"}).to_string(),
        supersedes: vec!["appr-grants-0000".into()],
    };
    let bad_non_string = LogRecordV1::InstallTransition {
        approval_ref: "appr-grants-0004".into(),
        session_id: "grants".into(),
        tool: "run".into(),
        risk_class: "privileged".into(),
        constraints: serde_json::json!({
            "__origin": "mcp:mac-shell",
            "__setup_id": 7,
            "__pinned_requirement": "pin-x",
        })
        .to_string(),
        supersedes: vec![],
    };
    let bad_extra_key = LogRecordV1::InstallTransition {
        approval_ref: "appr-grants-0005".into(),
        session_id: "grants".into(),
        tool: "run".into(),
        risk_class: "privileged".into(),
        constraints: serde_json::json!({
            "__origin": "mcp:mac-shell",
            "__setup_id": "setup-x",
            "__pinned_requirement": "pin-x",
            "path": "anything",
        })
        .to_string(),
        supersedes: vec![],
    };
    let grant_records = vec![
        good,
        bad_same_identity,
        bad_foreign_ref,
        bad_missing_key,
        bad_non_string,
        bad_extra_key,
    ];

    // Evidence referencing the GOOD approval still verifies (nothing
    // retired it); evidence referencing either malformed transition's
    // embedded ref dangles (they were never admitted).
    let mk = |intent, aref: &str| {
        vec![
            LogRecordV1::TurnOpened {
                turn_id: "t1".into(),
                provider: String::new(),
                model: String::new(),
            },
            LogRecordV1::Intent {
                turn_id: "t1".into(),
                intent_id: format!("intent-{intent}"),
                awili_call_id: "c1".into(),
                tool: "run".into(),
                raw_arguments: "{}".into(),
                canonical_args: None,
                call_fingerprint: None,
                argument_validation: crate::schema::ArgumentValidationV1::Valid,
                risk_class: "privileged".into(),
                decision_basis: "grant".into(),
                evidence_call_hash: String::new(),
                identity_digest: String::new(),
                approval_ref: aref.into(),
            },
        ]
    };
    let ok = classify_with_grants(&mk(1, "appr-grants-0000"), &grant_records);
    assert!(
        !matches!(
            ok[0].intents[0].status,
            crate::replay::IntentStatus::AuthzInvalid { .. }
        ),
        "the innocent grant keeps authorizing: {:?}",
        ok[0].intents[0].status
    );
    for bad in [
        "appr-grants-0001",
        "appr-grants-0002",
        "appr-grants-0003",
        "appr-grants-0004",
        "appr-grants-0005",
    ] {
        let out = classify_with_grants(&mk(2, bad), &grant_records);
        assert!(
            matches!(
                out[0].intents[0].status,
                crate::replay::IntentStatus::AuthzInvalid { .. }
            ),
            "a malformed transition's ref must dangle: {bad}"
        );
    }
}
