//! Unit 4 verdict r7, commit 2: the PUBLIC issuance surface is one atomic
//! bundle. A separate test target — its own process — so the sole-issue
//! statics start clean and this file can make the fresh-process claims the
//! in-crate sequential suite cannot (see src/authz_tests.rs: one process,
//! one linear consumption sequence).

use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_log::authz::issue_bundle;
use awili_engine_log::RiskClass;

#[test]
fn issue_bundle_claims_all_four_bits_atomically() {
    let (gate, _approval_cap, registry, install_seed) =
        issue_bundle().expect("a fresh process claims the whole bundle");

    // Every element of the bundle is the real, usable capability. The gate
    // mints call-bound policy evidence; the registry authority mints
    // resolution tokens; the approval capability is held (its minting flow
    // is exercised end-to-end in the in-crate suite).
    let identity = ToolIdentity::new(
        ToolOrigin::new("builtin:fs"),
        "fs.read",
        DescriptorDigest([0u8; 32]),
    );
    let resolved = registry.mint(identity.clone());

    let call = ToolCall {
        awili_call_id: AwiliCallId("c1".into()),
        provider_call_id: None,
        name: "fs.read".into(),
        arguments: r#"{"path":"a.txt"}"#.into(),
    };
    let evidence = gate
        .policy_evidence(&resolved, "t1", &call, RiskClass::ReadOnly, None)
        .unwrap();
    assert!(evidence.authorizes("t1", &call));

    assert_eq!(resolved.identity(), &identity);

    // The FOURTH member is usable in this fresh process (unit 5 ruling
    // 24): a floors mint through the confined-shape constructor works.
    let floors = install_seed.mint_floors(
        ToolOrigin::new("mcp:mac-shell"),
        awili_engine::generation::GenerationAllocator::new().next(),
        vec![("run_command".into(), RiskClass::Privileged)],
    );
    assert_eq!(floors.origin().origin, "mcp:mac-shell");
    assert_eq!(floors.floors().count(), 1);

    // And one bundle exhausted the process's ENTIRE issuance surface: no
    // later issuance of any kind exists, so all four bits were claimed by
    // the single call above. (The no-partial-claim half of atomicity is
    // proven in-crate from a deliberately partial state:
    // `failed_bundle_claim_consumes_no_free_bit`.)
    assert!(
        issue_bundle().is_err(),
        "the second bundle claim must fail in the same process"
    );
}
