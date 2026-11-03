//! Probe results and run reports (P0a data model; JSON on disk, append-only).
//!
//! Reports are written as
//! `tests/daily-driver/results/<rfc3339>-<executor>-<model>.json` — one file
//! per run, never rewritten. [`write_report_new`] creates the file with
//! `O_EXCL` (`create_new`) and retries on a collision, so concurrent runs
//! can never truncate or replace an existing report.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};

use crate::evaluate::{Outcome, Summary};
use crate::manifest::ExecutorKind;

/// Maximum evidence size: 2 KiB.
pub const EVIDENCE_LIMIT: usize = 2 * 1024;

/// Suffix appended when evidence is truncated.
pub const EVIDENCE_TRUNCATION_SUFFIX: &str = "…[truncated]";

/// Clamp `evidence` to [`EVIDENCE_LIMIT`] bytes.
///
/// Input already within the limit is returned unchanged. Longer input is cut
/// on a char boundary and suffixed with [`EVIDENCE_TRUNCATION_SUFFIX`], so
/// the result is always at most `EVIDENCE_LIMIT` bytes.
pub fn clamp_evidence(evidence: &str) -> String {
    if evidence.len() <= EVIDENCE_LIMIT {
        return evidence.to_owned();
    }
    let budget = EVIDENCE_LIMIT - EVIDENCE_TRUNCATION_SUFFIX.len();
    let mut cut = budget;
    while !evidence.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = String::with_capacity(EVIDENCE_LIMIT);
    out.push_str(&evidence[..cut]);
    out.push_str(EVIDENCE_TRUNCATION_SUFFIX);
    out
}

/// Result of running one probe against one executor + model.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProbeResult {
    /// Probe id from the manifest.
    pub probe_id: String,
    /// Executor kind that produced the result.
    pub executor: ExecutorKind,
    /// Model name.
    pub model: String,
    /// Judged outcome.
    pub outcome: Outcome,
    /// Wall-clock probe duration in milliseconds.
    pub duration_ms: u64,
    /// Input tokens, when the executor reports them.
    pub tokens_in: Option<u64>,
    /// Output tokens, when the executor reports them.
    pub tokens_out: Option<u64>,
    /// Redacted, clamped evidence (the final assistant text).
    pub evidence: String,
    /// Tool the answer came from, when known (e.g. `memory`, `Bash`).
    pub tool_origin: Option<String>,
    /// RFC3339 timestamp at which the probe started.
    pub started_at: String,
}

/// A full run of a manifest against one executor + model.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunReport {
    /// Manifest version the run used.
    pub manifest_version: u32,
    /// Executor kind.
    pub executor: ExecutorKind,
    /// Model name.
    pub model: String,
    /// One result per probe, in manifest order.
    pub results: Vec<ProbeResult>,
    /// Totals for the run.
    pub summary: Summary,
}

/// Write a brand-new report file into `dir`, named
/// `<rfc3339>-<executor>-<model>.json` (second precision, UTC).
///
/// P0-weekend-repair.md item 3: the file is opened with
/// `File::create_new(true)` (`O_EXCL`), so an existing report is never
/// truncated or replaced. Concurrent `run` processes (or two runs in the
/// same second) that compute the same name cannot clobber each other — on a
/// collision the name is retried with the timestamp bumped by one second,
/// which is what makes the retry safe where timestamp precision alone is
/// insufficient. Returns the path actually written; the prior file keeps its
/// original bytes (still valid JSON).
///
/// RC2-SVC9 (TOP43 #28): `executor` and `model` become ONE file-name part —
/// every character outside ASCII letters, digits, `.`, `_` and `-` (a `/`,
/// `\\`, `:`, a space, a control character) is replaced with `_`, so a model
/// id like `org/model` or `../../x` can never place the report outside `dir`.
pub fn write_report_new(
    dir: &Path,
    timestamp: DateTime<Utc>,
    executor: &str,
    model: &str,
    json: &str,
) -> std::io::Result<PathBuf> {
    // One hour of second-bumps bounds the retry: beyond that the directory
    // holds a pathological backlog, so fail instead of spinning.
    const MAX_BUMPS: u32 = 3600;
    let mut at = timestamp;
    for _ in 0..MAX_BUMPS {
        let name = format!(
            "{}-{}-{}.json",
            at.to_rfc3339_opts(SecondsFormat::Secs, true),
            name_part(executor),
            name_part(model)
        );
        let path = dir.join(&name);
        match File::options().create_new(true).write(true).open(&path) {
            Ok(mut file) => {
                file.write_all(json.as_bytes())?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                at += TimeDelta::seconds(1);
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "report name collided on every retry",
    ))
}

/// RC2-SVC9 (TOP43 #28): one safe file-name part (see [`write_report_new`]).
fn name_part(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::write_report_new;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "awili-selftest-report-{}-{tag}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn at_midnight() -> chrono::DateTime<chrono::Utc> {
        "2026-09-12T00:00:00Z"
            .parse()
            .expect("static timestamp parses")
    }

    fn write(which: u8, dir: PathBuf, at: chrono::DateTime<chrono::Utc>) -> (PathBuf, Vec<u8>) {
        let json = format!("{{\"which\":{which}}}");
        let path = write_report_new(&dir, at, "local", "m", &json).expect("write");
        let bytes = std::fs::read(&path).expect("read back");
        (path, bytes)
    }

    #[test]
    fn forced_collision_retries_and_preserves_prior_bytes() {
        let dir = temp_dir("collision");
        let first =
            write_report_new(&dir, at_midnight(), "local", "m", "{\"v\":1}").expect("first write");
        // Same name a second run in the same second would compute.
        let second =
            write_report_new(&dir, at_midnight(), "local", "m", "{\"v\":2}").expect("second write");
        assert_ne!(first, second);
        let name = second
            .file_name()
            .expect("has a name")
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with("2026-09-12T00:00:01Z-"), "{name}");

        // The first report keeps its original bytes and both stay valid JSON.
        let v1 = std::fs::read_to_string(&first).expect("read first");
        let v2 = std::fs::read_to_string(&second).expect("read second");
        assert_eq!(v1, "{\"v\":1}\n");
        assert_eq!(v2, "{\"v\":2}\n");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&v1).expect("v1 json")["v"],
            1
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&v2).expect("v2 json")["v"],
            2
        );
    }

    #[test]
    fn concurrent_writers_neither_clobbers_the_other() {
        let dir = temp_dir("concurrent");
        let at = at_midnight();
        let (dir_a, dir_b) = (dir.clone(), dir.clone());
        let h1 = std::thread::spawn(move || write(1, dir_a, at));
        let h2 = std::thread::spawn(move || write(2, dir_b, at));
        let (p1, b1) = h1.join().expect("thread 1");
        let (p2, b2) = h2.join().expect("thread 2");
        assert_ne!(p1, p2);
        // Each file holds exactly the bytes its own writer wrote.
        assert_eq!(b1, b"{\"which\":1}\n");
        assert_eq!(b2, b"{\"which\":2}\n");
    }

    /// RC2-SVC9 (TOP43 #28): separators in the executor or model never
    /// place the report outside `dir`; the name is one safe component.
    #[test]
    fn rc2svc9_separators_never_leave_the_report_dir() {
        let dir = temp_dir("separators");
        let path = write_report_new(&dir, at_midnight(), "loc/al", "../../escape\\x:y z", "{}")
            .expect("write");
        assert_eq!(path.parent(), Some(dir.as_path()), "{}", path.display());
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, "2026-09-12T00:00:00Z-loc_al-.._.._escape_x_y_z.json");
    }
}
