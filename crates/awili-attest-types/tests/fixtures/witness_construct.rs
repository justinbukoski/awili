//! Unit 5 (ruling 18 + O3): no raw construction — the fields are private
//! and no public constructor exists; only VerifiedLaunch::with_install_witness
//! issues a witness.

fn main() {
    let _w = awili_attest_types::InstallOriginWitness {
        origin: unimplemented!(),
        generation: unimplemented!(),
        setup_id: unimplemented!(),
        pinned_requirement: unimplemented!(),
        _brand: std::marker::PhantomData,
    };
}
