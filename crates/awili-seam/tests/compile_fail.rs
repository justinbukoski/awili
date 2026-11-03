//! Gate row 25 driver (design r9 §6.3): `AcceptedRoots` cannot be
//! constructed outside validation — its fields are private and its sole
//! producer is `validate_roots`.

#[test]
fn accepted_roots_cannot_be_constructed_outside_validation() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/fixtures/construct_accepted_roots.rs");
}
