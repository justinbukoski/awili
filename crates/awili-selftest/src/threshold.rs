//! P0d pass-threshold for `awili-selftest run`: the report is always
//! written first; only then does the threshold decide the exit code
//! (0 met or no flag, 3 not met; errors keep 1; a clap flag conflict
//! is a usage error and exits 2 before any probe runs).

use std::process::ExitCode;

use anyhow::{anyhow, Result};
use awili_selftest::evaluate::Summary;

/// A `run` pass-threshold (P0d): `--min-pass <n>` compares the pass
/// count to `n`; `--min-pass-rate <f>` (0 < f <= 1) compares `pass /
/// total` to `f`. `None` means no threshold was given (a completed
/// run exits 0, unchanged behaviour). The clap `conflicts_with`
/// already rejects giving both flags (a usage error, exit 2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Threshold {
    /// No `--min-pass` or `--min-pass-rate` flag.
    None,
    /// `--min-pass <n>` was given.
    Count { min_pass: u32 },
    /// `--min-pass-rate <f>` was given.
    Rate { rate: f64 },
}

impl Threshold {
    /// Build the threshold from the parsed flags, rejecting an
    /// out-of-range rate (0 < f <= 1).
    pub(crate) fn new(min_pass: Option<u32>, min_pass_rate: Option<f64>) -> Result<Threshold> {
        let Some(min_pass) = min_pass else {
            if let Some(rate) = min_pass_rate {
                if !(0.0 < rate && rate <= 1.0) {
                    return Err(anyhow!(
                        "--min-pass-rate must be a fraction in (0, 1]; got {rate}"
                    ));
                }
                return Ok(Threshold::Rate { rate });
            }
            return Ok(Threshold::None);
        };
        if min_pass_rate.is_some() {
            // Unreachable once the clap conflict is enforced; kept so
            // the threshold logic is self-contained if called directly.
            return Err(anyhow!(
                "--min-pass and --min-pass-rate cannot both be given"
            ));
        }
        Ok(Threshold::Count { min_pass })
    }

    /// Decide the exit code from the run summary, printing the
    /// `threshold:` line after the summary when a threshold was given:
    /// `threshold: <pass> < <n> -> NOT MET` (exit 3) below it,
    /// `threshold: <pass> >= <n> -> MET` (exit 0) at or above it.
    /// Without a flag a completed run exits 0, unchanged.
    pub(crate) fn exit_code(&self, summary: &Summary) -> ExitCode {
        match self {
            Threshold::None => ExitCode::SUCCESS,
            Threshold::Count { min_pass } => {
                let met = summary.pass >= *min_pass;
                println!(
                    "threshold: {} {} {} -> {}",
                    summary.pass,
                    if met { ">=" } else { "<" },
                    min_pass,
                    if met { "MET" } else { "NOT MET" }
                );
                if met {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(3)
                }
            }
            Threshold::Rate { rate } => {
                let pass_rate = summary.pass as f64 / summary.total as f64;
                let met = pass_rate >= *rate;
                println!(
                    "threshold: {}/{} ({:.3}) {} {:.3} -> {}",
                    summary.pass,
                    summary.total,
                    pass_rate,
                    if met { ">=" } else { "<" },
                    rate,
                    if met { "MET" } else { "NOT MET" }
                );
                if met {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(3)
                }
            }
        }
    }
}
