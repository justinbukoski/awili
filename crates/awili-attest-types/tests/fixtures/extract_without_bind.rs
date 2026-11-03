// Row 23 fixture: every route to the token's inner launch value other than
// `bind_scoped` must fail to compile. The tuple field is private and the
// inner enum type is not even nameable outside the crate.

use awili_attest_types::{LaunchToken, SetupIdentityId, SetupRecordFacts, LaunchKind};
use awili_engine::generation::GenerationAllocator;
use awili_engine::identity::ToolOrigin;

fn main() {
    let record = SetupRecordFacts {
        origin: ToolOrigin::new("mcp:mac-shell"),
        setup_id: SetupIdentityId::parse("setup-1").unwrap(),
        pinned_requirement: "req".into(),
        expected_protocol_name: "mac-shell".into(),
        launch_kind: LaunchKind::Direct,
        program: "/bin/mac-shell".into(),
    };
    let mut alloc = GenerationAllocator::new();
    let token = LaunchToken::mint_verified(&record, alloc.next());

    // Private tuple field: E0616.
    let inner = token.0;

    // And the inner enum's variants are unnameable, so destructuring is
    // impossible even in a pattern.
    let LaunchToken(contents) = token;
    let _ = (inner, contents);
}
