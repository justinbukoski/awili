//! Gate row 23 driver (design r9 §6.3): a `LaunchToken` cannot yield a
//! `&VerifiedLaunch` without the bind. The fixture tries to pattern-match
//! the token's private inner value; rustc refuses.

#[test]
fn a_launch_token_cannot_yield_a_verified_launch_without_bind() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/fixtures/extract_without_bind.rs");
}

/// Unit 5 (rulings 18-20): witness escape and raw construction are
/// compile errors — the brand pins the callback scope, the private fields
/// pin construction.
#[test]
fn an_install_witness_cannot_escape_or_be_constructed_raw() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/fixtures/witness_escape.rs");
    t.compile_fail("tests/fixtures/witness_construct.rs");
}
