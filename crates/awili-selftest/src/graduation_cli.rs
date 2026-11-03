//! `awili-selftest graduation ...` (P8a): append to and report on the
//! graduation ledger. Bin-private module; the arithmetic lives in
//! `awili_selftest::graduation`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use awili_selftest::graduation::{
    parse_rfc3339, render_summary, summarize, GraduationPolicy, Ledger, LedgerEntry, Outcome,
    LEDGER_VERSION,
};
use chrono::{SecondsFormat, Utc};
use clap::Subcommand;

/// Exit status when the ledger does not yet pass the policy.
const EXIT_NOT_YET: u8 = 3;

/// `graduation` subcommands.
#[derive(Subcommand, Debug, Clone)]
pub enum GraduationCommand {
    /// Append one attempted task to the ledger (validated at the door).
    Append {
        /// Ledger path (JSON Lines; created if missing, never truncated).
        #[arg(long)]
        ledger: PathBuf,
        /// Caller-chosen task identifier.
        #[arg(long)]
        task_id: String,
        /// Executor that ran the task, e.g. dsh, claude, codex.
        #[arg(long)]
        executor: String,
        /// Model that ran the task.
        #[arg(long)]
        model: String,
        /// success|failed|fallback|stopped.
        #[arg(long)]
        outcome: String,
        /// Reason; required for failed and fallback (ignored for stopped).
        #[arg(long)]
        reason: Option<String>,
        /// Frontier tokens spent on this task.
        #[arg(long)]
        frontier_tokens: u64,
        /// Local-model tokens spent on this task.
        #[arg(long)]
        local_tokens: u64,
        /// Free-text notes (redacted before write).
        #[arg(long)]
        notes: Option<String>,
        /// RFC 3339 timestamp; default now (UTC).
        #[arg(long)]
        recorded_at: Option<String>,
    },
    /// Print the window summary; exit 0 when it passes, 3 when not yet.
    Report {
        /// Ledger path.
        #[arg(long)]
        ledger: PathBuf,
        /// Policy TOML path.
        #[arg(long)]
        policy: PathBuf,
        /// RFC 3339 "now" for the window end; default now (UTC).
        #[arg(long)]
        now: Option<String>,
    },
}

/// Run a `graduation` subcommand and map its result to an exit code.
pub fn run(command: GraduationCommand) -> ExitCode {
    match command {
        GraduationCommand::Append {
            ledger,
            task_id,
            executor,
            model,
            outcome,
            reason,
            frontier_tokens,
            local_tokens,
            notes,
            recorded_at,
        } => {
            let built = build_entry(
                &task_id,
                &executor,
                &model,
                &outcome,
                reason,
                frontier_tokens,
                local_tokens,
                notes,
                recorded_at,
            );
            match built.and_then(|entry| Ledger::append(&ledger, &entry)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error:#}");
                    ExitCode::FAILURE
                }
            }
        }
        GraduationCommand::Report {
            ledger,
            policy,
            now,
        } => match report(&ledger, &policy, now.as_deref()) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::from(EXIT_NOT_YET),
            Err(error) => {
                eprintln!("error: {error:#}");
                ExitCode::FAILURE
            }
        },
    }
}

#[allow(clippy::too_many_arguments)] // mirrors the CLI flags one-to-one
fn build_entry(
    task_id: &str,
    executor: &str,
    model: &str,
    outcome: &str,
    reason: Option<String>,
    frontier_tokens: u64,
    local_tokens: u64,
    notes: Option<String>,
    recorded_at: Option<String>,
) -> Result<LedgerEntry> {
    let outcome = match outcome {
        "success" => Outcome::Success,
        "failed" => Outcome::Failed {
            reason: reason.unwrap_or_default(),
        },
        "fallback" => Outcome::Fallback {
            reason: reason.unwrap_or_default(),
        },
        // F16: a user-stopped task carries no reason.
        "stopped" => Outcome::Stopped,
        other => bail!("--outcome must be success|failed|fallback|stopped, got `{other}`"),
    };
    let recorded_at = match recorded_at {
        Some(text) => {
            parse_rfc3339(&text).with_context(|| format!("--recorded-at `{text}`"))?;
            text
        }
        None => Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    };
    Ok(LedgerEntry {
        v: LEDGER_VERSION,
        recorded_at,
        task_id: task_id.to_owned(),
        executor: executor.to_owned(),
        model: model.to_owned(),
        outcome,
        frontier_tokens,
        local_tokens,
        notes,
    })
}

/// Print the summary; `Ok(true)` when the policy passes.
fn report(ledger: &Path, policy: &Path, now: Option<&str>) -> Result<bool> {
    let policy = GraduationPolicy::load(policy)?;
    let entries = Ledger::read(ledger)?;
    let now = match now {
        Some(text) => parse_rfc3339(text).with_context(|| format!("--now `{text}`"))?,
        None => Utc::now(),
    };
    let summary = summarize(&entries, &policy, now);
    print!("{}", render_summary(&summary));
    Ok(summary.passes)
}
