//! Durable local-build jobs over a fixed SSH runner protocol.
//!
//! RC2-R4 (TOP43 #10): two surfaces. The client — the typed protocol, the
//! fixed SSH client and the supervised-endpoint answers — is always built;
//! it is all the app needs and all it can reach. The runner host's own
//! modules (the job store, submission and cancellation, the worker, the
//! service endpoint and supervisor) are built only with the `host` feature,
//! which the runner and endpoint binaries require. Awili depends on this
//! crate with `default-features = false`, so its build cannot call them.
#![forbid(unsafe_code)]
//! The host modules are additionally Unix-only: they use openat/flock/process
//! groups. On Windows only the client builds (TODO(windows-phase1) if a
//! Windows runner host is ever wanted).
#[cfg(unix)]
#[cfg(feature = "host")]
pub mod endpoint;
#[cfg(unix)]
#[cfg(feature = "host")]
mod process;
pub mod protocol;
#[cfg(unix)]
#[cfg(feature = "host")]
pub mod runner;
pub mod ssh;
#[cfg(unix)]
#[cfg(feature = "host")]
pub mod store;
pub mod supervised;
#[cfg(unix)]
#[cfg(feature = "host")]
pub mod supervisor;
#[cfg(unix)]
#[cfg(feature = "host")]
mod worker;

mod transport;
