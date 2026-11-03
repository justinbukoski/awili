//! Token selection and the platform attestor (design r9 §2.4 Phase A,
//! §2.5; probes recorded in `docs/reviews/unit4b-attestation-probe.c`).
//!
//! Phase A runs after spawn, with stdio piped and **no MCP byte written**:
//!
//! | record state | token |
//! |---|---|
//! | absent | `Unverified{NoSetupRecord}` |
//! | `launch_kind = "interpreted"` | `Unverified{InterpreterOnly}` — attestation not attempted |
//! | `launch_kind = "direct"`, basename denylisted | `Unverified{InterpreterOnly}` |
//! | `record.program != spawn_program` | `Unverified{ProgramMismatch}` |
//! | `launch_kind = "direct"`, program agrees | attest → verified or failure |
//!
//! Attestation FAILURE is not an unverified token: the caller kills and
//! reaps the child and emits `ServerDied{AttestationFailed, selected_class:
//! None}` — no handshake with a process that failed attestation.

use awili_attest_types::{LaunchToken, UnverifiedReason};
use awili_engine::generation::ConnectionGeneration;
use awili_engine::identity::ToolOrigin;

use crate::setup_record::InstalledSetupRecord;

/// Interpreter basenames refused for `launch_kind = "direct"` records —
/// DEFENSE IN DEPTH against an inconsistent record, not an independent
/// guard and not a complete classifier (design r9 §2.5, ruling B lineage).
const INTERPRETER_DENYLIST: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "env",
    "python",
    "python2",
    "python3",
    "node",
    "nodejs",
    "bun",
    "deno",
    "ruby",
    "perl",
    "php",
    "osascript",
];

/// RC2-SVC6 (F0325): whether a spawned basename is a denylisted interpreter.
/// Matching ignores ASCII case (macOS file systems do, so `/bin/BASH` runs
/// bash) and a trailing version (`python3.12`, `node22`, `perl5.34`,
/// `ruby-3.3`), so a renamed or versioned interpreter cannot slip past as a
/// `direct` program.
fn is_interpreter(basename: &str) -> bool {
    let lower = basename.to_ascii_lowercase();
    let stem =
        lower.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-' || c == '_');
    INTERPRETER_DENYLIST.contains(&lower.as_str()) || INTERPRETER_DENYLIST.contains(&stem)
}

/// Phase A's outcome: a selected token (verified or unverified — both
/// proceed to the one common Phase B), or an attestation failure that must
/// kill the child before any MCP byte.
#[derive(Debug)]
pub enum TokenSelection {
    Selected(LaunchToken),
    AttestationFailed(AttestationFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("attestation failed against {pinned_requirement:?}: {detail}")]
pub struct AttestationFailure {
    pub pinned_requirement: String,
    pub detail: String,
}

/// Select the launch token for one spawned child (design r9 §2.4 Phase A).
///
/// `spawn_program` is the program the runtime ACTUALLY spawned;
/// `child_pid` is the live child the platform attestor checks. The
/// verified mint copies its origin from the record (`awili-attest-types`,
/// mint-time invariant); the unverified mint carries the runtime's
/// requested origin.
pub fn select_token(
    record: Option<&InstalledSetupRecord>,
    origin: &ToolOrigin,
    generation: ConnectionGeneration,
    spawn_program: &str,
    child_pid: Option<u32>,
) -> TokenSelection {
    let Some(record) = record else {
        return TokenSelection::Selected(LaunchToken::mint_unverified(
            origin.clone(),
            generation,
            UnverifiedReason::NoSetupRecord,
        ));
    };
    let facts = record.facts();

    if facts.launch_kind == awili_attest_types::LaunchKind::Interpreted {
        // Attestation NOT attempted (row 27): an interpreted launch's
        // semantics live in content the requirement cannot pin.
        return TokenSelection::Selected(LaunchToken::mint_unverified(
            origin.clone(),
            generation,
            UnverifiedReason::InterpreterOnly,
        ));
    }

    let basename = std::path::Path::new(spawn_program)
        .file_name()
        .map(|b| b.to_string_lossy().to_string())
        .unwrap_or_default();
    if is_interpreter(&basename) {
        return TokenSelection::Selected(LaunchToken::mint_unverified(
            origin.clone(),
            generation,
            UnverifiedReason::InterpreterOnly,
        ));
    }

    if facts.program != spawn_program {
        return TokenSelection::Selected(LaunchToken::mint_unverified(
            origin.clone(),
            generation,
            UnverifiedReason::ProgramMismatch,
        ));
    }

    match attest_pid(child_pid, &facts.pinned_requirement) {
        Ok(()) => TokenSelection::Selected(LaunchToken::mint_verified(facts, generation)),
        Err(detail) => TokenSelection::AttestationFailed(AttestationFailure {
            pinned_requirement: facts.pinned_requirement.clone(),
            detail,
        }),
    }
}

/// Check the LIVE child against the pinned code requirement. macOS-only:
/// on any other platform — and on any platform error — attestation fails
/// closed; it never silently degrades to unverified, because a record that
/// promised `direct` semantics must be either proven or refused.
#[cfg(target_os = "macos")]
fn attest_pid(child_pid: Option<u32>, requirement: &str) -> Result<(), String> {
    use security_framework::os::macos::code_signing::{
        Flags, GuestAttributes, SecCode, SecRequirement,
    };

    let Some(pid) = child_pid else {
        return Err("child pid unavailable; cannot attest".into());
    };
    let req: SecRequirement = requirement
        .parse()
        .map_err(|e| format!("pinned requirement did not parse: {e:?}"))?;
    let mut attrs = GuestAttributes::new();
    attrs.set_pid(pid as i32);
    // Host None = the system root: guests of anything, matched by pid.
    let code = SecCode::copy_guest_with_attribues(None, &attrs, Flags::NONE)
        .map_err(|e| format!("no guest code object for pid {pid}: {e:?}"))?;
    code.check_validity(Flags::NONE, &req)
        .map_err(|e| format!("code requirement check refused pid {pid}: {e:?}"))
}

#[cfg(not(target_os = "macos"))]
fn attest_pid(_child_pid: Option<u32>, _requirement: &str) -> Result<(), String> {
    Err("platform attestation unavailable on this OS; failing closed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_record::load_setup_records;
    use std::io::Write;

    fn record_with(launch_kind: &str, program: &str) -> crate::setup_record::SetupRecords {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attested-servers.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"
[[server]]
origin        = "mcp:mac-shell"
program       = "{program}"
launch_kind   = "{launch_kind}"
requirement   = 'cdhash H"4dc1e6f6ab3f835b521f1e9da8fa5ef2a484f7d9"'
protocol_name = "mac-shell"
trust         = "install"
"#
        )
        .unwrap();
        let records = load_setup_records(&path);
        assert!(records.problems().is_empty(), "{:?}", records.problems());
        records
    }

    fn g1() -> ConnectionGeneration {
        awili_engine::generation::GenerationAllocator::new().next()
    }

    /// Gate row 27.
    #[test]
    fn an_interpreted_launch_kind_never_mints_verified() {
        let records = record_with("interpreted", "/usr/bin/python3");
        let origin = ToolOrigin::new("mcp:mac-shell");
        let rec = records.record(&origin);
        match select_token(rec, &origin, g1(), "/usr/bin/python3", Some(1)) {
            TokenSelection::Selected(token) => {
                assert_eq!(
                    token.class(),
                    awili_attest_types::AttestationClass::Unverified
                );
                match token.bind_scoped(&origin, token.generation()).unwrap() {
                    awili_attest_types::BoundLaunch::Unverified(u) => {
                        assert_eq!(u.reason(), UnverifiedReason::InterpreterOnly)
                    }
                    awili_attest_types::BoundLaunch::Verified(_) => {
                        panic!("interpreted launch minted verified")
                    }
                }
            }
            TokenSelection::AttestationFailed(f) => {
                panic!("attestation must not be ATTEMPTED for interpreted: {f}")
            }
        }
    }

    #[test]
    fn an_absent_record_selects_unverified_no_setup_record() {
        let origin = ToolOrigin::new("mcp:anything");
        match select_token(None, &origin, g1(), "/bin/anything", Some(1)) {
            TokenSelection::Selected(t) => match t.bind_scoped(&origin, t.generation()).unwrap() {
                awili_attest_types::BoundLaunch::Unverified(u) => {
                    assert_eq!(u.reason(), UnverifiedReason::NoSetupRecord)
                }
                _ => panic!("expected unverified"),
            },
            _ => panic!("absent record is not an attestation failure"),
        }
    }

    /// RC2-SVC6 (F0325): case and version variants of a denylisted
    /// interpreter are caught; ordinary program names are not.
    #[test]
    fn rc2svc6_interpreter_variants_are_caught() {
        for name in [
            "BASH",
            "Python3.12",
            "python3.11",
            "node22",
            "perl5.34",
            "ruby-3.3",
            "ZSH",
        ] {
            assert!(is_interpreter(name), "{name} must count as an interpreter");
        }
        for name in [
            "mac-shell",
            "awili-connectors-mcp",
            "nodejs-server",
            "bashful",
            "envoy",
        ] {
            assert!(!is_interpreter(name), "{name} is not an interpreter");
        }
        let records = record_with("direct", "/usr/local/bin/Python3.12");
        let origin = ToolOrigin::new("mcp:mac-shell");
        match select_token(
            records.record(&origin),
            &origin,
            g1(),
            "/usr/local/bin/Python3.12",
            Some(1),
        ) {
            TokenSelection::Selected(t) => match t.bind_scoped(&origin, t.generation()).unwrap() {
                awili_attest_types::BoundLaunch::Unverified(u) => {
                    assert_eq!(u.reason(), UnverifiedReason::InterpreterOnly)
                }
                _ => panic!("a versioned interpreter must not mint verified"),
            },
            TokenSelection::AttestationFailed(f) => {
                panic!("attestation must not be attempted for an interpreter: {f}")
            }
        }
    }

    #[test]
    fn a_denylisted_basename_on_a_direct_record_is_interpreter_only() {
        let records = record_with("direct", "/usr/bin/python3");
        let origin = ToolOrigin::new("mcp:mac-shell");
        match select_token(
            records.record(&origin),
            &origin,
            g1(),
            "/usr/bin/python3",
            Some(1),
        ) {
            TokenSelection::Selected(t) => match t.bind_scoped(&origin, t.generation()).unwrap() {
                awili_attest_types::BoundLaunch::Unverified(u) => {
                    assert_eq!(u.reason(), UnverifiedReason::InterpreterOnly)
                }
                _ => panic!("denylisted basename minted verified"),
            },
            _ => panic!("denylist is a selection outcome, not a failure"),
        }
    }

    #[test]
    fn a_program_mismatch_selects_unverified_program_mismatch() {
        let records = record_with("direct", "/usr/local/lib/awili/mac-shell");
        let origin = ToolOrigin::new("mcp:mac-shell");
        match select_token(
            records.record(&origin),
            &origin,
            g1(),
            "/tmp/impostor",
            Some(1),
        ) {
            TokenSelection::Selected(t) => match t.bind_scoped(&origin, t.generation()).unwrap() {
                awili_attest_types::BoundLaunch::Unverified(u) => {
                    assert_eq!(u.reason(), UnverifiedReason::ProgramMismatch)
                }
                _ => panic!("mismatched program minted verified"),
            },
            _ => panic!("program mismatch is a selection outcome"),
        }
    }

    #[test]
    fn a_direct_agreeing_program_reaches_the_attestor_and_fails_closed_without_a_real_child() {
        // pid None: the attestor must FAIL, never degrade to unverified.
        let records = record_with("direct", "/usr/local/lib/awili/mac-shell");
        let origin = ToolOrigin::new("mcp:mac-shell");
        match select_token(
            records.record(&origin),
            &origin,
            g1(),
            "/usr/local/lib/awili/mac-shell",
            None,
        ) {
            TokenSelection::AttestationFailed(f) => {
                assert!(f.pinned_requirement.contains("cdhash"));
            }
            TokenSelection::Selected(t) => panic!("no child, yet selected {t:?}"),
        }
    }
}
