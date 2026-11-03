//! Unit 4 verdict r7, commit 2: a bundle claim that loses on ONE bit claims
//! NONE. A separate test target — its own process — so the sole-issue statics
//! start clean and this file can build the deliberately partial state the
//! in-crate sequential suite reaches only mid-sequence (see
//! src/authz_tests.rs).

#![cfg(feature = "test-authority")]

use awili_engine_log::authz::{
    issue_bundle, AuthzGate, HostApprovalCapability, InstallSeedAuthority,
};

#[test]
fn failed_bundle_claim_consumes_no_free_bit() {
    // Controlled partial claim: take exactly the capability bit, leaving the
    // gate and registry bits free.
    let _cap = HostApprovalCapability::issue().expect("the free capability bit is claimable");

    // The bundle now loses on the capability bit — and must consume nothing.
    assert!(
        issue_bundle().is_err(),
        "a bundle claim must fail once the capability bit is taken"
    );

    // The gate bit the failed bundle attempt would have stranded in the
    // pre-atomic implementation is still issuable.
    AuthzGate::issue()
        .expect("the failed bundle claim consumed no free bit — the gate must still be issuable");

    // Unit 5 (ruling 24): the new fourth bit is not stranded either.
    InstallSeedAuthority::issue().expect(
        "the failed bundle claim consumed no free bit — install-seed must still be issuable",
    );
}
