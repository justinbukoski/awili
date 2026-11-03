//! The installer-authored setup-record loader (design r4 §2.3, carried
//! through r9 §2.5).
//!
//! `~/.config/awili/attested-servers.toml` is control-plane state: written by
//! unit-5 install, never by the settings UI, and a separate file from the
//! editable MCP config. Every load rule fails closed:
//!
//! - file absent → every origin Unverified (no records, no error);
//! - no entry for an origin → that origin Unverified;
//! - malformed requirement → hard error, origin unavailable;
//! - `launch_kind` absent or unrecognized → hard error, not a default;
//! - `trust = "install"` on any origin other than the reserved
//!   `INSTALL_TRUSTED_SERVER` (`crates/awili-config/src/lib.rs:261`) →
//!   hard error, the same reservation the editable config enforces at
//!   `:699-701`, so the two files cannot disagree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use awili_attest_types::{LaunchKind, SetupIdentityId, SetupRecordFacts};
use awili_config::INSTALL_TRUSTED_SERVER;
use awili_engine::identity::ToolOrigin;
use sha2::{Digest, Sha256};

/// One loaded record: the pure facts plus file provenance. Minted only by
/// [`load_setup_records`]/[`SetupRecords`] — possession of the type is the
/// loader's provenance statement, which is why `InstallAuthorityView`
/// (grant clause 2) takes it rather than raw facts.
#[derive(Debug, Clone)]
pub struct InstalledSetupRecord {
    facts: SetupRecordFacts,
    source: PathBuf,
}

impl InstalledSetupRecord {
    pub fn facts(&self) -> &SetupRecordFacts {
        &self.facts
    }
    pub fn source(&self) -> &Path {
        &self.source
    }
}

/// A per-origin hard error. The origin it names is UNAVAILABLE — the
/// runtime must not launch it — while other origins load independently.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetupRecordProblem {
    #[error("setup record {origin:?}: malformed or empty field {field:?}")]
    MalformedField { origin: String, field: &'static str },
    #[error("setup record {origin:?}: launch_kind {value:?} is not \"direct\" or \"interpreted\"")]
    UnrecognizedLaunchKind { origin: String, value: String },
    #[error(
        "setup record {origin:?}: trust = \"install\" is reserved for \
         {INSTALL_TRUSTED_SERVER:?} (origin \"mcp:{INSTALL_TRUSTED_SERVER}\") — \
         the setup file may not disagree with the editable config's reservation"
    )]
    InstallTrustOffReservedOrigin { origin: String },
    #[error("setup record {origin:?}: unrecognized trust {value:?}")]
    UnrecognizedTrust { origin: String, value: String },
    #[error("setup file {path:?}: {detail}")]
    UnreadableFile { path: PathBuf, detail: String },
    #[error("setup record entry {index}: duplicate origin {origin:?}")]
    DuplicateOrigin { index: usize, origin: String },
}

/// The load result: per-origin records, per-origin hard errors, and the
/// POISON sets that make hard errors terminal (impl review r1 P1-3): an
/// origin whose control-plane entry hard-errored — malformed field,
/// unrecognized kind/trust, reservation violation, duplicate — is
/// UNAVAILABLE, distinguishable from "no entry", and stays that way for
/// this load. An unreadable/unparsable FILE poisons every origin.
#[derive(Debug, Default)]
pub struct SetupRecords {
    by_origin: HashMap<String, InstalledSetupRecord>,
    problems: Vec<SetupRecordProblem>,
    poisoned: std::collections::HashSet<String>,
    file_invalid: bool,
}

/// What the control plane says about one origin (impl review r1 P1-3).
#[derive(Debug)]
pub enum RecordLookup<'a> {
    /// No entry: the launch may proceed and selects `Unverified`.
    Absent,
    /// A well-formed installer record.
    Valid(&'a InstalledSetupRecord),
    /// The entry (or the whole file) hard-errored: the origin is
    /// UNAVAILABLE — the runtime must not launch it at all.
    Invalid,
}

impl SetupRecords {
    /// The fail-closed gate: `Invalid` for a hard-errored entry or an
    /// unreadable file, `Valid` for a well-formed record, `Absent`
    /// otherwise.
    pub fn lookup(&self, origin: &ToolOrigin) -> RecordLookup<'_> {
        if self.file_invalid || self.poisoned.contains(&origin.origin) {
            return RecordLookup::Invalid;
        }
        match self.by_origin.get(&origin.origin) {
            Some(r) => RecordLookup::Valid(r),
            None => RecordLookup::Absent,
        }
    }

    /// The VALID record for an origin, if any. Callers deciding
    /// availability must use [`Self::lookup`] — this accessor cannot
    /// distinguish Absent from Invalid, on purpose (it feeds comparisons,
    /// never gates).
    pub fn record(&self, origin: &ToolOrigin) -> Option<&InstalledSetupRecord> {
        if self.file_invalid || self.poisoned.contains(&origin.origin) {
            return None;
        }
        self.by_origin.get(&origin.origin)
    }

    pub fn problems(&self) -> &[SetupRecordProblem] {
        &self.problems
    }
}

/// The default control-plane path (design r4 §2.3).
pub fn default_setup_path() -> Option<PathBuf> {
    dirs_home().map(|h| h.join(".config/awili/attested-servers.toml"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// RC2-SVC6 (F0336): unknown keys are refused at both levels — a
/// misspelled table (`[[sever]]`) or field (`trsut`) makes the whole file
/// invalid (every origin fails closed) instead of loading with no records
/// and no problem.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    server: Vec<RawServer>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServer {
    origin: Option<String>,
    program: Option<String>,
    launch_kind: Option<String>,
    requirement: Option<String>,
    protocol_name: Option<String>,
    trust: Option<String>,
}

/// Load and validate the setup file. An ABSENT file is not an error: it
/// returns an empty record set (every origin Unverified). Every other
/// irregularity is a recorded per-origin (or per-file) problem.
pub fn load_setup_records(path: &Path) -> SetupRecords {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SetupRecords::default(),
        Err(e) => {
            let mut out = SetupRecords::default();
            out.problems.push(SetupRecordProblem::UnreadableFile {
                path: path.to_path_buf(),
                detail: e.to_string(),
            });
            out.file_invalid = true;
            return out;
        }
    };
    load_setup_records_from(&text, path)
}

/// Unit 5 (r5 §4, one read / one parse / one hash): parse EXACT bytes the
/// caller read and hashed — the sandwich must never re-read on its own.
// Three-way branch on one key (poisoned / duplicate / first sight): the entry
// API would fold the poison case into the insert and hide the control flow.
#[allow(clippy::map_entry)]
pub fn load_setup_records_from(text: &str, path: &Path) -> SetupRecords {
    let mut out = SetupRecords::default();
    let raw: RawFile = match toml::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            out.problems.push(SetupRecordProblem::UnreadableFile {
                path: path.to_path_buf(),
                detail: format!("not parseable as TOML: {e}"),
            });
            out.file_invalid = true;
            return out;
        }
    };

    for (index, entry) in raw.server.into_iter().enumerate() {
        let origin_key = entry.origin.clone().unwrap_or_default();
        match validate_entry(entry, path) {
            Ok(record) => {
                let key = record.facts.origin.origin.clone();
                // Entry view (rather than `contains_key`): the occupancy check
                // and the insertion below share one map probe.
                let dup = matches!(
                    out.by_origin.entry(key.clone()),
                    std::collections::hash_map::Entry::Occupied(_)
                );
                if out.poisoned.contains(&key) {
                    // Already poisoned (duplicate or prior hard error):
                    // TERMINAL — a later occurrence never resurrects it
                    // (impl review r1 P1-3).
                    out.problems
                        .push(SetupRecordProblem::DuplicateOrigin { index, origin: key });
                } else if dup {
                    // A duplicated origin is ambiguous control-plane state:
                    // BOTH entries are refused and the origin is POISONED
                    // for this load.
                    out.by_origin.remove(&key);
                    out.poisoned.insert(key.clone());
                    out.problems
                        .push(SetupRecordProblem::DuplicateOrigin { index, origin: key });
                } else {
                    out.by_origin.insert(key, record);
                }
            }
            Err(problem) => {
                // The named origin is UNAVAILABLE, terminally: a hard
                // error is distinguishable from absence.
                if !origin_key.is_empty() {
                    out.by_origin.remove(&origin_key);
                    out.poisoned.insert(origin_key);
                }
                out.problems.push(problem);
            }
        }
    }
    out
}

fn validate_entry(
    entry: RawServer,
    path: &Path,
) -> Result<InstalledSetupRecord, SetupRecordProblem> {
    let origin = non_empty(entry.origin, "origin", "")?;
    let program = non_empty(entry.program, "program", &origin)?;
    let requirement = non_empty(entry.requirement, "requirement", &origin)?;
    let protocol_name = non_empty(entry.protocol_name, "protocol_name", &origin)?;

    // Requirement well-formedness (syntactic floor; the attestor is the
    // semantic check): non-empty, printable, and plausibly a code
    // requirement. A requirement the platform later refuses to parse fails
    // the launch at attestation time — also closed.
    if requirement.chars().any(|c| c.is_control()) {
        return Err(SetupRecordProblem::MalformedField {
            origin,
            field: "requirement",
        });
    }

    let launch_kind = match entry.launch_kind.as_deref() {
        Some("direct") => LaunchKind::Direct,
        Some("interpreted") => LaunchKind::Interpreted,
        Some(other) => {
            return Err(SetupRecordProblem::UnrecognizedLaunchKind {
                origin,
                value: other.to_string(),
            })
        }
        None => {
            return Err(SetupRecordProblem::MalformedField {
                origin,
                field: "launch_kind",
            })
        }
    };

    // The reservation: trust = "install" only for the reserved server's
    // origin. (Row 7 pins this exact refusal.)
    match entry.trust.as_deref() {
        Some("install") => {
            let reserved = format!("mcp:{INSTALL_TRUSTED_SERVER}");
            if origin != reserved {
                return Err(SetupRecordProblem::InstallTrustOffReservedOrigin { origin });
            }
        }
        Some("prompt") | None => {}
        Some(other) => {
            return Err(SetupRecordProblem::UnrecognizedTrust {
                origin,
                value: other.to_string(),
            })
        }
    }

    // The setup identity is CONTENT-DERIVED: a digest over every
    // identity-bearing field. Replacing the installed record — new program,
    // new requirement, new protocol claim, new kind — necessarily changes
    // the id, which is exactly the comparison grant clause 2 makes
    // (design r9 §2.6, row 38).
    let mut h = Sha256::new();
    for part in [&origin, &program, &requirement, &protocol_name] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    h.update(match launch_kind {
        LaunchKind::Direct => b"direct".as_slice(),
        LaunchKind::Interpreted => b"interpreted".as_slice(),
    });
    let digest = h.finalize();
    let setup_id = SetupIdentityId::parse(&format!("{digest:x}")).expect("hex digest is parseable");

    Ok(InstalledSetupRecord {
        facts: SetupRecordFacts {
            origin: ToolOrigin::new(origin),
            setup_id,
            pinned_requirement: requirement,
            expected_protocol_name: protocol_name,
            launch_kind,
            program,
        },
        source: path.to_path_buf(),
    })
}

fn non_empty(
    v: Option<String>,
    field: &'static str,
    origin: &str,
) -> Result<String, SetupRecordProblem> {
    match v {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(SetupRecordProblem::MalformedField {
            origin: origin.to_string(),
            field,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(content: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("attested-servers.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, path)
    }

    const GOOD: &str = r#"
[[server]]
origin        = "mcp:mac-shell"
program       = "/usr/local/lib/awili/mac-shell"
launch_kind   = "direct"
requirement   = 'cdhash H"4dc1e6f6ab3f835b521f1e9da8fa5ef2a484f7d9"'
protocol_name = "mac-shell"
trust         = "install"
"#;

    /// RC2-SVC6 (F0336): a misspelled table or field makes the file invalid
    /// (every origin fails closed, one problem names the file) instead of
    /// loading silently with no records.
    #[test]
    fn rc2svc6_a_misspelled_table_or_field_fails_closed() {
        let typo_table = GOOD.replace("[[server]]", "[[sever]]");
        let typo_field = GOOD.replace("trust         =", "trsut         =");
        for text in [typo_table, typo_field] {
            let (_d, path) = write_file(&text);
            let records = load_setup_records(&path);
            assert!(
                matches!(
                    records.lookup(&ToolOrigin::new("mcp:mac-shell")),
                    RecordLookup::Invalid
                ),
                "{text}"
            );
            assert!(
                matches!(
                    records.problems(),
                    [SetupRecordProblem::UnreadableFile { .. }]
                ),
                "{:?}",
                records.problems()
            );
        }
        let (_d, path) = write_file(GOOD);
        assert!(matches!(
            load_setup_records(&path).lookup(&ToolOrigin::new("mcp:mac-shell")),
            RecordLookup::Valid(_)
        ));
    }

    /// Gate row 7.
    #[test]
    fn install_trust_off_the_reserved_origin_is_a_hard_error_in_the_setup_record() {
        let (_d, path) = write_file(
            r#"
[[server]]
origin        = "mcp:other"
program       = "/usr/local/lib/awili/other"
launch_kind   = "direct"
requirement   = 'cdhash H"4dc1e6f6ab3f835b521f1e9da8fa5ef2a484f7d9"'
protocol_name = "other"
trust         = "install"
"#,
        );
        let records = load_setup_records(&path);
        assert!(records.record(&ToolOrigin::new("mcp:other")).is_none());
        assert!(matches!(
            records.problems(),
            [SetupRecordProblem::InstallTrustOffReservedOrigin { origin }] if origin == "mcp:other"
        ));
    }

    #[test]
    fn an_absent_file_yields_no_records_and_no_problems() {
        let dir = tempfile::tempdir().unwrap();
        let records = load_setup_records(&dir.path().join("missing.toml"));
        assert!(records.record(&ToolOrigin::new("mcp:mac-shell")).is_none());
        assert!(records.problems().is_empty());
    }

    #[test]
    fn a_wellformed_record_loads_with_a_content_derived_identity() {
        let (_d, path) = write_file(GOOD);
        let records = load_setup_records(&path);
        let rec = records
            .record(&ToolOrigin::new("mcp:mac-shell"))
            .expect("record");
        assert_eq!(rec.facts().expected_protocol_name, "mac-shell");
        assert_eq!(rec.facts().launch_kind, LaunchKind::Direct);
        // Replacing any identity-bearing field changes the setup id.
        let (_d2, path2) = write_file(&GOOD.replace("/usr/local/lib/awili/mac-shell", "/tmp/evil"));
        let rec2 = load_setup_records(&path2);
        let rec2 = rec2.record(&ToolOrigin::new("mcp:mac-shell")).unwrap();
        assert_ne!(rec.facts().setup_id, rec2.facts().setup_id);
    }

    #[test]
    fn an_absent_or_unrecognized_launch_kind_is_a_hard_error_not_a_default() {
        let (_d, path) = write_file(&GOOD.replace("launch_kind   = \"direct\"\n", ""));
        let records = load_setup_records(&path);
        assert!(records.record(&ToolOrigin::new("mcp:mac-shell")).is_none());
        assert!(matches!(
            records.problems(),
            [SetupRecordProblem::MalformedField {
                field: "launch_kind",
                ..
            }]
        ));

        let (_d2, path2) = write_file(&GOOD.replace("\"direct\"", "\"maybe\""));
        let records2 = load_setup_records(&path2);
        assert!(records2.record(&ToolOrigin::new("mcp:mac-shell")).is_none());
        assert!(matches!(
            records2.problems(),
            [SetupRecordProblem::UnrecognizedLaunchKind { value, .. }] if value == "maybe"
        ));
    }

    #[test]
    fn a_duplicate_origin_refuses_both_entries() {
        let (_d, path) = write_file(&format!("{GOOD}\n{GOOD}"));
        let records = load_setup_records(&path);
        assert!(records.record(&ToolOrigin::new("mcp:mac-shell")).is_none());
        assert!(records
            .problems()
            .iter()
            .any(|p| matches!(p, SetupRecordProblem::DuplicateOrigin { .. })));
    }
}
