//! Pure expectation evaluation and result reduction (P0a).
//!
//! [`evaluate`] judges a final assistant text against an
//! [`crate::manifest::Expect`]; [`reduce`] folds probe results into a
//! [`Summary`]. Both are pure functions — no I/O.

use regex::RegexBuilder;

use crate::manifest::Expect;
use crate::report::ProbeResult;

/// Outcome of one probe.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Outcome {
    /// The expectation was met.
    Pass,
    /// The model answered but the expectation was not met.
    Fail {
        /// Why it failed (e.g. `missing: <string>`, `declared-missing`).
        reason: String,
    },
    /// The probe could not be executed (timeout, spawn failure, ...).
    Error {
        /// What went wrong (e.g. `timeout`).
        message: String,
    },
    /// The probe was not run (absent from a run, matrix gap, ...).
    Skipped {
        /// Why it was skipped.
        reason: String,
    },
}

/// Per-run totals.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    /// Probes that passed.
    pub pass: u32,
    /// Probes that failed (answered, expectation not met).
    pub fail: u32,
    /// Probes that errored.
    pub error: u32,
    /// Probes that were skipped.
    pub skipped: u32,
    /// All probes (`pass + fail + error + skipped`).
    pub total: u32,
    /// `pass / total` as a fraction (0.0 when `total` is 0).
    pub pass_rate: f32,
}

/// Judge `text` (the final assistant text) against `expect`.
///
/// Pure. All expectations are case-insensitive by default
/// (P0-weekend-repair.md item 4): `ContainsAll` matches ignoring case,
/// `Regex` is built case-insensitive with `RegexBuilder` (an explicit
/// `(?-i:...)` group in the pattern opts out for that match), and
/// `Sentinel` is an exact token compared ignoring case. A `Sentinel` that
/// is absent simply does not trigger; it never passes on its own.
pub fn evaluate(expect: &Expect, text: &str) -> Outcome {
    match expect {
        Expect::ContainsAll(needles) => {
            // F1080: each needle must appear as a whole word (a needle that
            // starts or ends with a letter or digit may not run into one), and
            // a needle listed N times must appear N times — "17" is not found
            // inside "2017", and ["READY", "READY"] needs two READYs.
            let lower = text.to_lowercase();
            for needle in needles {
                let wanted = needle.to_lowercase();
                let required = needles
                    .iter()
                    .filter(|other| other.to_lowercase() == wanted)
                    .count();
                if whole_word_count(&lower, &wanted) < required {
                    return Outcome::Fail {
                        reason: format!("missing: {needle}"),
                    };
                }
            }
            Outcome::Pass
        }
        Expect::Regex(pattern) => {
            // P0-weekend-repair.md item 4: case-insensitive by default; the
            // pattern can opt out with an explicit `(?-i:...)` group.
            let re = match RegexBuilder::new(pattern).case_insensitive(true).build() {
                Ok(re) => re,
                Err(e) => {
                    return Outcome::Error {
                        message: format!("invalid regex `{pattern}`: {e}"),
                    }
                }
            };
            if re.is_match(text) {
                Outcome::Pass
            } else {
                Outcome::Fail {
                    reason: "no-regex-match".to_owned(),
                }
            }
        }
        Expect::Sentinel(token) => {
            // Exact token, case-insensitive (P0-weekend-repair.md item 4).
            let lower = text.to_lowercase();
            if lower.contains(&token.to_lowercase()) {
                Outcome::Fail {
                    reason: "declared-missing".to_owned(),
                }
            } else {
                Outcome::Error {
                    message: format!("sentinel `{token}` not present"),
                }
            }
        }
        Expect::AllOf(parts) => {
            for part in parts {
                let outcome = evaluate(part, text);
                if outcome != Outcome::Pass {
                    return outcome;
                }
            }
            Outcome::Pass
        }
        Expect::Forbid(pattern) => {
            let re = match RegexBuilder::new(pattern).case_insensitive(true).build() {
                Ok(re) => re,
                Err(e) => {
                    return Outcome::Error {
                        message: format!("invalid regex `{pattern}`: {e}"),
                    }
                }
            };
            match re.find(text) {
                Some(m) => Outcome::Fail {
                    reason: format!("forbidden: {}", m.as_str()),
                },
                None => Outcome::Pass,
            }
        }
        Expect::AnyOf(branches) => {
            let mut declared_missing = false;
            for branch in branches {
                match evaluate(branch, text) {
                    Outcome::Pass => return Outcome::Pass,
                    // A broken pattern is the probe's fault, not the answer's:
                    // say so instead of folding it into "no branch matched".
                    Outcome::Error { message } if message.starts_with("invalid regex") => {
                        return Outcome::Error { message };
                    }
                    Outcome::Fail { reason } if reason == "declared-missing" => {
                        declared_missing = true;
                    }
                    _ => {}
                }
            }
            if declared_missing {
                Outcome::Fail {
                    reason: "declared-missing".to_owned(),
                }
            } else {
                Outcome::Fail {
                    reason: format!("no branch of {} matched", branches.len()),
                }
            }
        }
    }
}

/// Non-overlapping occurrences of `needle` in `hay` that do not run into a
/// neighbouring word character at a word-character edge of the needle.
fn whole_word_count(hay: &str, needle: &str) -> usize {
    fn word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
    if needle.is_empty() {
        return 0;
    }
    let starts_word = needle.chars().next().is_some_and(word);
    let ends_word = needle.chars().last().is_some_and(word);
    hay.match_indices(needle)
        .filter(|(at, _)| {
            let before = hay[..*at].chars().next_back();
            let after = hay[at + needle.len()..].chars().next();
            let runs_in = starts_word && before.is_some_and(word);
            let runs_out = ends_word && after.is_some_and(word);
            !(runs_in || runs_out)
        })
        .count()
}

/// Fold probe results into per-run totals.
///
/// Pure. `pass_rate` is `pass / total` (0.0 for an empty slice).
pub fn reduce(results: &[ProbeResult]) -> Summary {
    let mut summary = Summary {
        pass: 0,
        fail: 0,
        error: 0,
        skipped: 0,
        total: results.len() as u32,
        pass_rate: 0.0,
    };
    for result in results {
        match result.outcome {
            Outcome::Pass => summary.pass += 1,
            Outcome::Fail { .. } => summary.fail += 1,
            Outcome::Error { .. } => summary.error += 1,
            Outcome::Skipped { .. } => summary.skipped += 1,
        }
    }
    if summary.total > 0 {
        summary.pass_rate = summary.pass as f32 / summary.total as f32;
    }
    summary
}
