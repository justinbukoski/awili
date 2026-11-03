//! Unit 4 verdict r7, item 3: the authority-minted `ResolvedTool` bound
//! through C1 evidence, B2 intent persistence, and replay. A separate test
//! target — its own process — sharing the `test-authority` fixture with the
//! rest of the workspace's test processes.

#![cfg(feature = "test-authority")]

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_log::authz::issue_bundle;
use awili_engine_log::schema::LogRecordV1;
use awili_engine_log::{classify, test_bundle, ExecutionLog, MemLog, RiskClass};

fn tool_identity(origin: &str, name: &str) -> ToolIdentity {
    ToolIdentity::new(ToolOrigin::new(origin), name, DescriptorDigest([0u8; 32]))
}

fn call(id: &str, tool: &str, args: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId(id.into()),
        provider_call_id: None,
        name: tool.into(),
        arguments: args.into(),
    }
}

/// Outside this crate the ONLY way to a `ResolvedTool` is
/// `RegistryAuthority::mint` with a held authority — the field is private
/// (pinned by the compile-fail doctest on the type) and there is no public
/// constructor. The runtime half of the proof: the authority itself is only
/// obtainable from the process's one `issue_bundle` claim, which the
/// fixture has already made, and minting binds exactly the identity handed
/// to it.
#[test]
fn resolved_tool_cannot_be_constructed_without_registry_authority() {
    let bundle = test_bundle();

    // The fixture performed this process's sole bundle claim: no second
    // authority — of any kind — can be created, so every `ResolvedTool`
    // in this process traces back to `bundle.2`.
    assert!(
        issue_bundle().is_err(),
        "the fixture must hold the sole issuance; no second authority can exist"
    );

    let identity = tool_identity("mcp:server-1", "probe.ping");
    let resolved = bundle.2.mint(identity.clone());
    assert_eq!(resolved.identity(), &identity);

    // Minting is transparent binding, not fabrication: a different
    // identity mints a different token, and the token exposes nothing
    // beyond `identity()`.
    let other = bundle.2.mint(tool_identity("builtin:fs", "fs.read"));
    assert_ne!(resolved.identity(), other.identity());
}

/// `intent()` requires a bound tool identity — evidence-borne only. The
/// binding is IMPOSSIBLE TO BYPASS BY CONSTRUCTION (r4 acceptance 16,
/// asserted as the type-level discipline): `AuthzEvidence` has a private
/// identity field, every constructor takes `&ResolvedTool`, and no public
/// API accepts a free caller identity — so an evidence value lacking an
/// identity cannot exist and `intent()` cannot be handed one. The runtime
/// half: whatever token the constructors consumed is EXACTLY what the
/// durable record persists, for every basis (policy and ask shown).
#[test]
fn intent_requires_bound_tool_identity() {
    let bundle = test_bundle();
    let (gate, cap, registry) = (bundle.0.clone(), bundle.1.clone(), bundle.2.clone());
    let mut log = ExecutionLog::open(MemLog::new(), "s-identity").unwrap();
    log.turn_opened("t1", "test", "test-model").unwrap();

    // Policy basis: the gate binds the token's identity into the evidence.
    let resolved = registry.mint(tool_identity("builtin:fs", "fs.read"));
    let c1 = call("t1-call-0000", "fs.read", r#"{"path":"a.txt"}"#);
    let ev = gate
        .policy_evidence(&resolved, "t1", &c1, RiskClass::ReadOnly, None)
        .unwrap();
    assert_eq!(ev.tool_identity(), resolved.identity());
    log.intent("t1", &c1, ev).unwrap();

    // Ask basis: the approval flow's evidence binds the SAME token the
    // dialog was raised for (PendingApproval captured it at Ask time).
    let resolved2 = registry.mint(tool_identity("mcp:server-1", "probe.ping"));
    let c2 = call("t1-call-0001", "probe.ping", r#"{"q":"x"}"#);
    let proof = log
        .record_approval(&cap, "once", "t1", &c2, RiskClass::Mutating, None, "{}")
        .unwrap();
    let ev2 = awili_engine_log::AuthzEvidence::from_ask(proof, &resolved2, None);
    assert_eq!(ev2.tool_identity(), resolved2.identity());
    log.intent("t1", &c2, ev2).unwrap();

    // The durable records persist exactly the bound identities' digests —
    // derived from the evidence, never a caller argument.
    let (records, _) = log.read_records().unwrap();
    let digests: Vec<&str> = records
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::Intent {
                identity_digest, ..
            } => Some(identity_digest.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        digests,
        vec![
            resolved.identity().digest_hex().as_str(),
            resolved2.identity().digest_hex().as_str()
        ],
        "intent() must persist the evidence-bound identity digest"
    );
    assert_ne!(digests[0], digests[1]);
}

/// Replay exposes the persisted identity binding for BOTH legs — the
/// builtin leg (`builtin:fs`) and the mcp leg (`mcp:*`) share one record
/// shape (r3 Delta 2), and `classify` surfaces each intent's digest.
#[test]
fn replay_exposes_persisted_identity_digest_for_builtin_and_mcp() {
    let bundle = test_bundle();
    let gate = &bundle.0;
    let registry = &bundle.2;
    let mut log = ExecutionLog::open(MemLog::new(), "s-replay").unwrap();
    log.turn_opened("t1", "test", "test-model").unwrap();

    let builtin = registry.mint(tool_identity("builtin:fs", "fs.read"));
    let mcp = registry.mint(tool_identity("mcp:server-1", "probe.ping"));

    let cb = call("t1-call-0000", "fs.read", r#"{"path":"a.txt"}"#);
    let evb = gate
        .policy_evidence(&builtin, "t1", &cb, RiskClass::ReadOnly, None)
        .unwrap();
    log.intent("t1", &cb, evb).unwrap();

    let cm = call("t1-call-0001", "probe.ping", r#"{"q":"x"}"#);
    let evm = gate
        .policy_evidence(&mcp, "t1", &cm, RiskClass::ReadOnly, None)
        .unwrap();
    log.intent("t1", &cm, evm).unwrap();

    let (records, _) = log.read_records().unwrap();
    let intents: Vec<_> = classify(&records)
        .into_iter()
        .flat_map(|t| t.intents)
        .collect();
    assert_eq!(intents.len(), 2);
    for i in &intents {
        let expected = if i.tool == "fs.read" {
            builtin.identity()
        } else {
            assert_eq!(i.tool, "probe.ping");
            mcp.identity()
        };
        assert_eq!(
            i.identity_digest,
            expected.digest_hex(),
            "replay must expose the persisted identity digest for {}",
            i.tool
        );
    }
    assert_ne!(intents[0].identity_digest, intents[1].identity_digest);
}
