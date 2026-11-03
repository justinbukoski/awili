//! awili-engine-sandbox — C3: filesystem operations that CANNOT escape the
//! authorized session root.
//!
//! The implementation (descriptor-relative, no-follow `openat` resolution) is
//! Unix-only and lives in `unix.rs`. On other platforms `unsupported.rs`
//! provides the same API, but [`Sandbox::open_root`] always fails: the
//! sandbox reports itself UNAVAILABLE rather than pretending to contain
//! anything. TODO(windows-phase1): a Windows implementation (handle-relative
//! `NtCreateFile` with reparse-point refusal) or an AppContainer boundary.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(not(unix))]
mod unsupported;
#[cfg(not(unix))]
pub use unsupported::*;
