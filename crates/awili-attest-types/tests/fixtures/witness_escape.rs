//! Unit 5 (rulings 18-20): the branded witness cannot escape its
//! synchronous callback — moving it into any binding that outlives the
//! closure is a lifetime error.

fn main() {
    let launch: &awili_attest_types::VerifiedLaunch = unimplemented!();
    let mut stash = None;
    launch.with_install_witness(|w| {
        stash = Some(w); // ERROR: `'s` cannot escape the closure
    });
    let _ = stash;
}
