//! Unit 4 verdict r7, commit 2: the `test-authority` fixture. Own process
//! (each test target is its own binary), so the sole-issue statics start
//! clean and the fixture performs this process's one and only issuance.

#![cfg(feature = "test-authority")]

use awili_engine::{DescriptorDigest, ToolIdentity, ToolOrigin};
use awili_engine_log::authz::issue_bundle;
use awili_engine_log::test_bundle;

#[test]
fn test_bundle_returns_the_same_process_global_bundle() {
    let first = test_bundle();
    let second = test_bundle();
    assert!(
        std::ptr::eq(first, second),
        "repeated calls must return the SAME process-global bundle"
    );

    // The fixture performed the process's sole issuance: no caller can now
    // claim anything, so no test can even attempt a second issuance.
    assert!(
        issue_bundle().is_err(),
        "the fixture must have claimed the sole bundle issuance"
    );

    // The third element is the real registry authority, not a placeholder.
    let resolved = first.2.mint(ToolIdentity::new(
        ToolOrigin::new("mcp:probe"),
        "probe.ping",
        DescriptorDigest([0u8; 32]),
    ));
    assert_eq!(resolved.identity().name, "probe.ping");
}
