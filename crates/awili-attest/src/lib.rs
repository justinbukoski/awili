//! `awili-attest` — the IO half of unit 4b's attestation boundary
//! (design r9 §2.1, §2.3–§2.6).
//!
//! Owns the installer-authored setup-record **loader**
//! (`~/.config/awili/attested-servers.toml`, fail-closed on every malformed
//! input), the **platform attestor** (macOS `security-framework`,
//! `cfg(target_os = "macos")`), the interpreter classifier's denylist
//! (defense in depth), and the [`grant::InstallAuthorityView`] the unit-5
//! seed site constructs at grant time.
//!
//! The pure vocabulary — `LaunchToken`, `VerifiedLaunch`, the scoped bind,
//! the record-sourced mint — lives below, in `awili-attest-types`; this crate
//! is the only production caller of the mint, and `awili-app` is this
//! crate's only consumer.

pub mod attestor;
pub mod grant;
pub mod setup_record;

pub use attestor::{select_token, AttestationFailure, TokenSelection};
pub use grant::{AuthorityRefusal, InstallAuthorityView};
pub use setup_record::{InstalledSetupRecord, SetupRecordProblem, SetupRecords};
