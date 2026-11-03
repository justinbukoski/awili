//! RC2-R4 (TOP43 #10): the app gets only the dispatch client. The runner
//! host's modules (job store, submission and cancellation, worker, service
//! endpoint, supervisor) sit behind the `host` feature, the binaries require
//! it, and Awili's dependency turns it off, so the app's own build cannot
//! reach them (`cargo check -p awili-dispatch --no-default-features --lib`
//! in the slice's VERIFY proves the client builds alone).
#![cfg(unix)] // TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
use std::{fs, path::Path};

fn read(rel: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

#[test]
fn rc2r4_the_app_gets_only_the_dispatch_client() {
    // Awili's dependency line turns the host modules off.
    let app = read("../awili-app/Cargo.toml");
    let line = app
        .lines()
        .find(|l| l.trim_start().starts_with("awili-dispatch"))
        .expect("awili-app depends on awili-dispatch");
    assert!(line.contains("default-features = false"), "{line}");

    // The host modules are compiled only with `host`; the client always.
    let lib = read("src/lib.rs");
    let lines: Vec<&str> = lib.lines().map(str::trim).collect();
    let gated = |decl: &str| {
        let at = lines
            .iter()
            .position(|l| *l == decl)
            .unwrap_or_else(|| panic!("{decl} missing from lib.rs"));
        at > 0 && lines[at - 1] == "#[cfg(feature = \"host\")]"
    };
    for host in [
        "pub mod endpoint;",
        "pub mod runner;",
        "pub mod store;",
        "pub mod supervisor;",
        "mod process;",
        "mod worker;",
    ] {
        assert!(gated(host), "{host} is not behind the host feature");
    }
    for client in ["pub mod protocol;", "pub mod ssh;", "pub mod supervised;"] {
        assert!(!gated(client), "{client} must stay in the client");
    }

    // The binaries need the host modules; the default build keeps them.
    let manifest = read("Cargo.toml");
    assert!(manifest.contains("default = [\"host\"]"), "{manifest}");
    for bin in ["awili-build-runner", "awili-build-endpoint"] {
        let at = manifest
            .find(&format!("name = \"{bin}\""))
            .unwrap_or_else(|| panic!("[[bin]] {bin}"));
        let section = &manifest[at..];
        let end = section[1..].find("[[").map_or(section.len(), |e| e + 1);
        assert!(
            section[..end].contains("required-features = [\"host\"]"),
            "{bin}: {}",
            &section[..end]
        );
    }
}
