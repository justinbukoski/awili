//! Thin entry point. The runner host is Unix-only: its modules (job store,
//! worker, process groups, flock/openat) are gated to `cfg(unix)`.
//! TODO(windows-phase1): a Windows runner host is not planned; the app only
//! needs the client side, which builds everywhere.
#[cfg(unix)]
#[path = "../host_bins/runner.rs"]
mod imp;

#[cfg(unix)]
fn main() {
    imp::main()
}

#[cfg(not(unix))]
fn main() {
    eprintln!("awili-build-runner is not supported on Windows yet; the runner host runs on Linux");
    std::process::exit(2);
}
