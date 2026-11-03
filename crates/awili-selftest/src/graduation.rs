//! P8a: the append-only graduation evidence ledger and its arithmetic.
//!
//! Graduation week (plan 02, item 10) needs seven consecutive UTC days on
//! which every attempted fleet task is logged, every fallback carries its
//! reason, and the median frontier-token cost of successful tasks is at
//! most `target_ratio` of the frozen 2026-09-11 comparator. This module
//! is the ledger (JSON Lines, never truncated) and the summary that turns
//! it into a PASS / NOT YET verdict. Nothing here talks to the app or a
//! runner; P8b will append from those.
//!
//! Source spec: `docs/daily-driver/specs/P8a-graduation-ledger.md`.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Days, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::redact::redact;

/// The only ledger entry schema version this code writes or accepts.
pub const LEDGER_VERSION: u32 = 1;

/// The only policy file version this code accepts.
pub const POLICY_VERSION: u32 = 1;

/// How one attempted task ended.
///
/// Externally tagged: `"Success"`, `{"Failed":{"reason":".."}}`,
/// `{"Fallback":{"reason":".."}}`, `"Stopped"`. A fallback is a task
/// completed by a frontier model after a local attempt, or one that skipped
/// the fleet entirely; its reason is mandatory so no fallback goes
/// unrecorded. `Stopped` (F16) is a user-stopped task: counted as attempted,
/// never as a failure, success or fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    /// The task completed as routed.
    Success,
    /// The task did not complete; `reason` says why.
    Failed {
        /// Why the task failed (non-empty).
        reason: String,
    },
    /// The task fell back to a frontier model; `reason` says why.
    Fallback {
        /// Why the fleet route was abandoned (non-empty).
        reason: String,
    },
    /// The user stopped the task before it finished (F16). Counted as
    /// attempted; never a failure, success or fallback.
    Stopped,
}

/// One attempted task, one line of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Entry schema version; always [`LEDGER_VERSION`].
    pub v: u32,
    /// When the entry was recorded, RFC 3339 in UTC.
    pub recorded_at: String,
    /// Caller-chosen task identifier (non-empty).
    pub task_id: String,
    /// Executor that ran the task (non-empty), e.g. `dsh`, `claude`.
    pub executor: String,
    /// Model that ran the task (non-empty).
    pub model: String,
    /// How the task ended.
    pub outcome: Outcome,
    /// Frontier tokens spent on this task (all frontier calls, in + out).
    pub frontier_tokens: u64,
    /// Local-model tokens spent on this task.
    pub local_tokens: u64,
    /// Free text; redacted before it reaches the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// The frozen comparator the ratio is measured against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparator {
    /// Date the comparator was frozen (informational).
    pub frozen_at: String,
    /// Where the comparator number came from (informational).
    pub source: String,
    /// Frontier tokens the comparator task consumed (> 0).
    pub frontier_tokens: u64,
}

/// The pass rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyRules {
    /// Median successful-task frontier tokens / comparator must be <= this
    /// (0 < ratio <= 1).
    pub target_ratio: f64,
    /// The `target_ratio` literal exactly as written in the TOML (digits and
    /// one optional point), captured at parse time because the f64 alone
    /// cannot preserve it (`0.199999999999999999` parses to `0.2`). The
    /// verdict compares against this text, never the float.
    #[serde(skip)]
    pub target_ratio_literal: String,
    /// Consecutive UTC calendar days that must each carry an entry (>= 1).
    pub window_days: u32,
}

/// The parsed `tests/daily-driver/graduation.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraduationPolicy {
    /// Policy file version; must equal [`POLICY_VERSION`].
    pub version: u32,
    /// The frozen comparator.
    pub comparator: Comparator,
    /// The pass rule.
    pub policy: PolicyRules,
}

impl GraduationPolicy {
    /// Parse and validate a policy from TOML text.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let mut policy: GraduationPolicy =
            toml::from_str(text).context("parse graduation policy")?;
        policy.policy.target_ratio_literal = target_ratio_literal(text)?;
        policy.validate()?;
        Ok(policy)
    }

    /// Read, parse and validate a policy file.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read graduation policy {}", path.display()))?;
        Self::from_toml_str(&text)
    }

    /// Reject a policy whose fields are outside their documented ranges,
    /// naming the offending field.
    pub fn validate(&self) -> Result<()> {
        if self.version != POLICY_VERSION {
            bail!(
                "graduation policy `version` must be {POLICY_VERSION}, got {}",
                self.version
            );
        }
        if self.comparator.frontier_tokens == 0 {
            bail!("graduation policy `comparator.frontier_tokens` must be > 0");
        }
        if self.policy.window_days == 0 {
            bail!("graduation policy `policy.window_days` must be >= 1");
        }
        // The range rule is decided on the typed literal, exactly, because the
        // f64 rounds (`1.0000000000000001` parses to 1.0).
        let ratio = self.policy.target_ratio;
        let literal = &self.policy.target_ratio_literal;
        let Some((numerator, scale)) = decimal_fraction(literal) else {
            bail!(
                "graduation policy `policy.target_ratio` literal `{literal}` is not a plain decimal"
            );
        };
        let one = 10u128.checked_pow(scale).ok_or_else(|| {
            anyhow!("graduation policy `policy.target_ratio` has too many digits")
        })?;
        if numerator == 0 || numerator > one {
            bail!(
                "graduation policy `policy.target_ratio` must satisfy 0 < ratio <= 1, got {literal}"
            );
        }
        if literal.parse::<f64>().ok() != Some(ratio) {
            bail!(
                "graduation policy `policy.target_ratio` literal `{literal}` does not denote {ratio}"
            );
        }
        Ok(())
    }
}

/// The append-only JSON Lines ledger on disk.
pub struct Ledger;

impl Ledger {
    /// Validate `entry`, redact its notes and append it as one JSON line.
    ///
    /// The file is opened create+append and is never truncated or
    /// rewritten. A [`Outcome::Failed`] or [`Outcome::Fallback`] without a
    /// non-empty reason is rejected here, at the door.
    pub fn append(path: &Path, entry: &LedgerEntry) -> Result<()> {
        let entry = validate_entry(entry)?;
        let line = serde_json::to_string(&entry).context("serialize ledger entry")?;
        let mut file: File = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open ledger {} for append", path.display()))?;
        file.write_all(line.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_all())
            .with_context(|| format!("append to ledger {}", path.display()))?;
        Ok(())
    }

    /// Read every entry. A missing file is an empty ledger; any malformed
    /// line fails the whole read, naming its 1-based line number.
    pub fn read(path: &Path) -> Result<Vec<LedgerEntry>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("open ledger {}", path.display()))
            }
        };
        let mut entries = Vec::new();
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let number = index + 1;
            let line = line.with_context(|| format!("read ledger line {number}"))?;
            if line.trim().is_empty() {
                bail!("ledger line {number} is empty");
            }
            let entry: LedgerEntry = serde_json::from_str(&line)
                .with_context(|| format!("ledger line {number} is not a valid entry"))?;
            // The same door rules as `append`: a hand-written or foreign
            // ledger cannot reach a verdict with entries `append` would refuse.
            // S7d (F0828): return the checked entry, so a line written before
            // reasons were redacted (or by hand) is redacted on the way out.
            let entry = validate_entry(&entry).with_context(|| format!("ledger line {number}"))?;
            entries.push(entry);
        }
        Ok(entries)
    }
}

/// Check the door rules and return the entry as it will be written
/// (version pinned, notes and outcome reasons redacted).
pub fn validate_entry(entry: &LedgerEntry) -> Result<LedgerEntry> {
    if entry.v != LEDGER_VERSION {
        bail!("ledger entry `v` must be {LEDGER_VERSION}, got {}", entry.v);
    }
    for (field, value) in [
        ("task_id", &entry.task_id),
        ("executor", &entry.executor),
        ("model", &entry.model),
    ] {
        if value.trim().is_empty() {
            bail!("ledger entry `{field}` must be non-empty");
        }
    }
    match &entry.outcome {
        Outcome::Success | Outcome::Stopped => {}
        Outcome::Failed { reason } if reason.trim().is_empty() => {
            bail!("a `Failed` outcome must carry a non-empty reason")
        }
        Outcome::Fallback { reason } if reason.trim().is_empty() => {
            bail!("a `Fallback` outcome must carry a non-empty reason (no unrecorded fallback)")
        }
        Outcome::Failed { .. } | Outcome::Fallback { .. } => {}
    }
    let recorded_at = parse_rfc3339(&entry.recorded_at)
        .with_context(|| format!("ledger entry `recorded_at` `{}`", entry.recorded_at))?;
    // S7d (F0824): a Failed/Fallback reason is free text exactly like
    // `notes` (often an error message), so it gets the same redaction.
    let outcome = match &entry.outcome {
        Outcome::Failed { reason } => Outcome::Failed {
            reason: redact_notes(reason),
        },
        Outcome::Fallback { reason } => Outcome::Fallback {
            reason: redact_notes(reason),
        },
        other => other.clone(),
    };
    Ok(LedgerEntry {
        // Stored in UTC at second precision whatever offset the caller gave.
        recorded_at: recorded_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        notes: entry.notes.as_deref().map(redact_notes),
        outcome,
        ..entry.clone()
    })
}

/// Bare credential shapes that the shared `redact` (labelled values and
/// `Bearer`) does not cover: vendor-prefixed keys (`sk-…`, `ghp_…`, `AKIA…`,
/// `xox…`, `glpat-…`, `hf_…`) and any run of 32+ token characters, which is
/// what raw API keys, JWT segments and hex secrets look like and what a
/// human reason never does.
const BARE_TOKEN_PATTERN: &str = r"(?i)\b(?:sk|ghp|gho|ghu|glpat|xox[abps]|hf|AKIA)[-_]?[A-Za-z0-9_\-]{8,}\b|[A-Za-z0-9_\-+/=]{32,}";

/// `redact` plus the bare-credential shapes above: the ledger's door rule is
/// that no secret enters it, and `notes` is free text typed by a person.
pub fn redact_notes(notes: &str) -> String {
    let labelled = redact(notes);
    match regex::Regex::new(BARE_TOKEN_PATTERN) {
        Ok(re) => re.replace_all(&labelled, "[REDACTED]").into_owned(),
        Err(_) => labelled,
    }
}

/// Parse an RFC 3339 timestamp into UTC.
pub fn parse_rfc3339(text: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|stamp| stamp.with_timezone(&Utc))
        .map_err(|error| anyhow!("not RFC 3339: {error}"))
}

/// The arithmetic over one window of the ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// First UTC day of the window (ISO date).
    pub window_start: String,
    /// Length of the window in UTC calendar days.
    pub window_days: u32,
    /// Window days that carry at least one entry of any outcome.
    pub days_with_entries: u32,
    /// Entries in the window.
    pub attempted: u64,
    /// `Success` entries in the window.
    pub successes: u64,
    /// `Failed` entries in the window.
    pub failures: u64,
    /// `Fallback` entries in the window.
    pub fallbacks: u64,
    /// `Stopped` entries in the window (F16).
    #[serde(default)]
    pub stopped: u64,
    /// Median frontier tokens over the window's successes (even count:
    /// mean of the two middle values, rounded down); `None` without any.
    pub median_success_frontier_tokens: Option<u64>,
    /// `median / comparator.frontier_tokens`; `None` without a median.
    pub ratio: Option<f64>,
    /// The policy's target ratio (float, for display and JSON consumers).
    pub target_ratio: f64,
    /// The policy's target ratio exactly as typed; what the verdict used.
    pub target_ratio_literal: String,
    /// The comparator the ratio is measured against.
    pub comparator_frontier_tokens: u64,
    /// Every window day has at least one entry.
    pub coverage_complete: bool,
    /// `coverage_complete` and the ratio is at or under target.
    pub passes: bool,
}

/// Summarize `entries` over the `policy.window_days` UTC calendar days
/// ending on `now`'s date, inclusive. Entries whose `recorded_at` does not
/// parse are ignored (the ledger rejects them at the door, so a hand-edited
/// file is the only way to get one).
pub fn summarize(
    entries: &[LedgerEntry],
    policy: &GraduationPolicy,
    now: DateTime<Utc>,
) -> Summary {
    let window_days = policy.policy.window_days;
    let end = now.date_naive();
    let start = end
        .checked_sub_days(Days::new(u64::from(window_days) - 1))
        .unwrap_or(NaiveDate::MIN);

    let mut covered = vec![false; window_days as usize];
    let (mut successes, mut failures, mut fallbacks, mut stopped) = (0u64, 0u64, 0u64, 0u64);
    let mut success_tokens: Vec<u64> = Vec::new();
    for entry in entries {
        let Some(day) = parse_rfc3339(&entry.recorded_at)
            .ok()
            .map(|stamp| stamp.date_naive())
        else {
            continue;
        };
        if day < start || day > end {
            continue;
        }
        let offset = day.signed_duration_since(start).num_days();
        if let Some(slot) = usize::try_from(offset)
            .ok()
            .and_then(|i| covered.get_mut(i))
        {
            *slot = true;
        }
        match entry.outcome {
            Outcome::Success => {
                successes += 1;
                success_tokens.push(entry.frontier_tokens);
            }
            Outcome::Failed { .. } => failures += 1,
            Outcome::Fallback { .. } => fallbacks += 1,
            Outcome::Stopped => stopped += 1,
        }
    }

    let median = median_floor(&mut success_tokens);
    let comparator = policy.comparator.frontier_tokens;
    let ratio = median.map(|m| m as f64 / comparator as f64);
    let days_with_entries = covered.iter().filter(|c| **c).count() as u32;
    let coverage_complete = days_with_entries == window_days;
    let target_ratio = policy.policy.target_ratio;
    let passes = coverage_complete
        && median
            .is_some_and(|m| within_target(m, comparator, &policy.policy.target_ratio_literal));
    Summary {
        window_start: start.to_string(),
        window_days,
        days_with_entries,
        attempted: successes + failures + fallbacks + stopped,
        successes,
        failures,
        fallbacks,
        stopped,
        median_success_frontier_tokens: median,
        ratio,
        target_ratio,
        target_ratio_literal: policy.policy.target_ratio_literal.clone(),
        comparator_frontier_tokens: comparator,
        coverage_complete,
        passes,
    }
}

use crate::graduation_math::{decimal_fraction, median_floor, target_ratio_literal, within_target};

pub use crate::graduation_render::render_summary;

#[cfg(test)]
#[path = "graduation_tests.rs"]
mod graduation_tests;

#[cfg(test)]
#[path = "graduation_policy_tests.rs"]
mod graduation_policy_tests;
