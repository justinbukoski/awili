//! `awili-selftest` CLI (P0b): run probes and render the result matrix.
//!
//! Subcommands:
//! - `run --manifest <toml> --executor <gateway|claude|codex|glm|local>
//!   --model <name> --command "<template>" [--only a,b] [--out dir]
//!   [--min-pass <n> | --min-pass-rate <f>]`
//!   executes the probes through one [`CommandExecutor`] and writes the
//!   append-only JSON report `<rfc3339>-<executor>-<model>.json` (created
//!   with `O_EXCL`; on a name collision the timestamp is bumped and
//!   retried, so an existing report is never truncated or replaced).
//!   The report is always written before the threshold decision: with
//!   `--min-pass <n>`, a pass count below `n` prints
//!   `threshold: <pass> < <n> -> NOT MET` and exits 3 (met prints
//!   `threshold: <pass> >= <n> -> MET` and exits 0); `--min-pass-rate
//!   <f>` (0 < f <= 1) is the same on `pass / total` and conflicts with
//!   `--min-pass` (a clap usage error, exit 2). Without either flag a
//!   completed run exits 0.
//!   A probe with a record check (`records`, RC2-OPSF5) gets a fresh run
//!   marker in its prompt (`{nonce}`; `AWILI_SELFTEST_NONCE` fixes it for
//!   a replay of recorded runs) and passes only when, besides its answer
//!   text, the tools' own records show the work (`records::verify`; read
//!   from `CLAUDE_CONFIG_DIR` / `CODEX_HOME`, else `~/.claude` / `~/.codex`).
//! - `report --dir <results>` prints the cross-report matrix as markdown.
//! - `validate --manifest <toml>` exits 0 when valid, 2 otherwise (the
//!   error names the offending probe id).
//! - `graduation append|report ...` (P8a): the append-only graduation
//!   ledger and its PASS / NOT YET summary; see `graduation_cli.rs`.
//!
//! The binary never prints environment variables: progress lines carry
//! only the probe id and outcome, evidence is redacted (`redact`) and
//! clamped before it is written, and error outcomes (executor and
//! evaluator errors) are sanitized the same way (P0-weekend-repair.md
//! item 1).

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use awili_selftest::evaluate::{evaluate, reduce, Outcome};
use awili_selftest::executor::CommandExecutor;
use awili_selftest::manifest::{ExecutorKind, Manifest, Probe};
use awili_selftest::matrix::Matrix;
use awili_selftest::records::{verify, RecordRoots};
use awili_selftest::redact::redact;
use awili_selftest::report::{clamp_evidence, write_report_new, ProbeResult, RunReport};
use chrono::SecondsFormat;
use clap::{Parser, Subcommand};

mod graduation_cli;
mod threshold;

/// Command-line arguments.
#[derive(Parser, Debug)]
#[command(
    name = "awili-selftest",
    version,
    about = "Daily-driver selftest: run probes per executor+model and render the matrix."
)]
struct Cli {
    /// Subcommand.
    #[command(subcommand)]
    command: Command,
}

/// Subcommands.
#[derive(Subcommand, Debug)]
enum Command {
    /// Run probes against one executor+model and write a JSON report.
    Run {
        /// Manifest TOML path.
        #[arg(long)]
        manifest: PathBuf,
        /// Executor kind: gateway|claude|codex|glm|local.
        #[arg(long)]
        executor: String,
        /// Model name (e.g. fable).
        #[arg(long)]
        model: String,
        /// Shell command template with a `{prompt}` placeholder
        /// (CommandExecutor), e.g. `claude -p {prompt}`.
        #[arg(long)]
        command: String,
        /// Only run these probe ids, comma separated.
        #[arg(long)]
        only: Option<String>,
        /// Output directory for the JSON report.
        #[arg(long, default_value = "tests/daily-driver/results")]
        out: PathBuf,
        /// Fail (exit 3) when the pass count is below this. Conflicts
        /// with `--min-pass-rate`.
        #[arg(long, value_name = "n", conflicts_with = "min_pass_rate")]
        min_pass: Option<u32>,
        /// Fail (exit 3) when `pass / total` is below this fraction
        /// (0 < f <= 1). Conflicts with `--min-pass`.
        #[arg(long, value_name = "f", conflicts_with = "min_pass")]
        min_pass_rate: Option<f64>,
    },
    /// Print the matrix over all reports in a directory as markdown.
    Report {
        /// Results directory.
        #[arg(long, default_value = "tests/daily-driver/results")]
        dir: PathBuf,
    },
    /// Validate a manifest; exit 2 (naming the offending id) when invalid.
    Validate {
        /// Manifest TOML path.
        #[arg(long)]
        manifest: PathBuf,
    },
    /// Graduation ledger (P8a): append entries and report the window.
    Graduation {
        /// Subcommand.
        #[command(subcommand)]
        command: graduation_cli::GraduationCommand,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match &cli.command {
        // `validate` exits 2 (naming the offending id) on an invalid manifest.
        Command::Validate { manifest } => match load_manifest(manifest) {
            Ok(manifest) => match manifest.validate() {
                Ok(()) => {
                    println!("valid: {} probes", manifest.probes.len());
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("awili-selftest: invalid manifest: {}", redact(&error));
                    ExitCode::from(2)
                }
            },
            Err(error) => {
                eprintln!("awili-selftest: {}", redact(&format!("{error:#}")));
                ExitCode::FAILURE
            }
        },
        Command::Run {
            manifest,
            executor,
            model,
            command,
            only,
            out,
            min_pass,
            min_pass_rate,
        } => {
            // The threshold is validated before any probe runs: an
            // out-of-range rate exits 1 (a run error), while giving
            // both flags is already a clap usage error (exit 2).
            let threshold = match threshold::Threshold::new(*min_pass, *min_pass_rate) {
                Ok(threshold) => threshold,
                Err(error) => {
                    eprintln!("awili-selftest: {}", redact(&format!("{error:#}")));
                    return ExitCode::FAILURE;
                }
            };
            match run_probes(
                manifest,
                executor,
                model,
                command,
                only.as_deref(),
                out,
                &threshold,
            ) {
                // The report is written inside `run_probes` before the
                // threshold decides the exit code (0 met / no flag,
                // 3 NOT MET).
                Ok(threshold_exit) => threshold_exit,
                Err(error) => {
                    eprintln!("awili-selftest: {}", redact(&format!("{error:#}")));
                    ExitCode::FAILURE
                }
            }
        }
        Command::Report { dir } => match report_dir(dir) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("awili-selftest: {}", redact(&format!("{error:#}")));
                ExitCode::FAILURE
            }
        },
        Command::Graduation { command } => graduation_cli::run(command.clone()),
    }
}

fn load_manifest(path: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read manifest {}", path.display()))?;
    let manifest = Manifest::from_toml_str(&text).map_err(anyhow::Error::msg)?;
    Ok(manifest)
}

fn run_probes(
    manifest_path: &Path,
    executor_str: &str,
    model: &str,
    command: &str,
    only: Option<&str>,
    out_dir: &Path,
    threshold: &threshold::Threshold,
) -> Result<ExitCode> {
    let kind = ExecutorKind::parse(executor_str).with_context(|| {
        format!("unknown executor kind `{executor_str}` (expected gateway|claude|codex|glm|local)")
    })?;
    let manifest = load_manifest(manifest_path)?;
    manifest.validate().map_err(anyhow::Error::msg)?;

    let probes = select_probes(&manifest, only)?;
    // The per-probe manifest `timeout_s` governs (passed per ask); the
    // executor itself imposes no additional cap.
    let executor = CommandExecutor::new(command, Duration::MAX);
    let roots = RecordRoots::from_env();

    let run_started = chrono::Utc::now();
    let mut results: Vec<ProbeResult> = Vec::with_capacity(probes.len());
    for probe in probes.iter() {
        let started_at = now_rfc3339();
        // F1098: a probe that builds on an earlier probe's result (the
        // document it created, the repo it cloned) is skipped, with the
        // reason, when that probe did not pass in this run.
        let unmet = probe.depends_on.iter().find(|dependency| {
            !results
                .iter()
                .any(|r| &r.probe_id == *dependency && r.outcome == Outcome::Pass)
        });
        if let Some(dependency) = unmet {
            let reason = if probes.iter().any(|p| &p.id == dependency) {
                format!("depends on `{dependency}`, which did not pass")
            } else {
                format!("depends on `{dependency}`, which this run did not include")
            };
            let outcome = Outcome::Skipped { reason };
            results.push(ProbeResult {
                probe_id: probe.id.clone(),
                executor: kind,
                model: model.to_owned(),
                outcome: outcome.clone(),
                duration_ms: 0,
                tokens_in: None,
                tokens_out: None,
                evidence: String::new(),
                tool_origin: None,
                started_at,
            });
            println!(
                "[{}/{}] {} {outcome:?}",
                results.len(),
                probes.len(),
                probe.id
            );
            continue;
        }
        let started = Instant::now();
        // RC2-OPSF5: this probe's run marker, so its records can be told
        // from any other run's.
        let nonce = run_marker()?;
        let prompt = probe.prompt.replace("{nonce}", &nonce);
        // P0-weekend-repair.md item 5: the CLI honors `{prompt_id}` —
        // shell-quoted from `probe.id`, independently of the prompt.
        let (outcome, evidence, tokens_in, tokens_out, tool_origin) = match executor.ask_probe(
            &prompt,
            &probe.id,
            Duration::from_secs(probe.timeout_s as u64),
        ) {
            Ok(answer) => {
                let mut outcome = evaluate(&probe.expect, &answer.text);
                // The text passed; a record check then asks the tools'
                // own records whether the work was really done.
                if let (Outcome::Pass, Some(check)) = (&outcome, &probe.records) {
                    if let Err(why) = verify(check, &roots, &nonce, &answer.text) {
                        outcome = Outcome::Fail {
                            reason: format!("unverified: {why}"),
                        };
                    }
                }
                // Evaluator errors (e.g. an invalid regex) are sanitized
                // like executor errors before they reach the report.
                let outcome = sanitize_outcome(outcome);
                // stdout is the evidence; stderr is appended to it (both
                // redacted and clamped) so diagnostic output is never lost.
                let evidence = if answer.stderr.is_empty() {
                    clamp_evidence(&redact(&answer.text))
                } else {
                    clamp_evidence(&redact(&format!("{}\n{}", answer.text, answer.stderr)))
                };
                (
                    outcome,
                    evidence,
                    answer.tokens_in,
                    answer.tokens_out,
                    answer.tool_origin,
                )
            }
            Err(error) => (
                // Executor failure diagnostics (spawn/wait/pipe/exit)
                // are sanitized before they become an Outcome.
                sanitize_outcome(Outcome::Error {
                    message: error.to_string(),
                }),
                String::new(),
                None,
                None,
                None,
            ),
        };
        let probe_id = probe.id.clone();
        let duration_ms = started.elapsed().as_millis() as u64;
        results.push(ProbeResult {
            probe_id: probe_id.clone(),
            executor: kind,
            model: model.to_owned(),
            outcome: outcome.clone(),
            duration_ms,
            tokens_in,
            tokens_out,
            evidence,
            tool_origin,
            started_at,
        });
        // Progress line: id + outcome only (never the answer text or env).
        println!(
            "[{}/{}] {probe_id} {outcome:?}",
            results.len(),
            probes.len()
        );
    }

    let summary = reduce(&results);
    let report = RunReport {
        manifest_version: manifest.version,
        executor: kind,
        model: model.to_owned(),
        results,
        summary,
    };

    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("create results dir {}", out_dir.display()))?;
    let json = serde_json::to_string_pretty(&report)?;
    // P0-weekend-repair.md item 3: `O_EXCL` create + collision retry — an
    // existing report (including one written by a concurrent CLI process in
    // the same second) is never truncated or replaced.
    let path = write_report_new(
        out_dir,
        run_started,
        kind.as_str(),
        &sanitize_label(model),
        &json,
    )
    .with_context(|| format!("write report in {}", out_dir.display()))?;

    println!("wrote {}", path.display());
    println!(
        "summary: {} pass / {} fail / {} error / {} skipped (total {}, pass_rate {:.3})",
        summary.pass,
        summary.fail,
        summary.error,
        summary.skipped,
        summary.total,
        summary.pass_rate
    );
    // The report is already written: the threshold decision (P0d) is
    // made after it, so a NOT MET run still leaves the report on disk.
    Ok(threshold.exit_code(&summary))
}

/// Redact and clamp an error outcome's message before it reaches the report
/// (P0-weekend-repair.md item 1): executor errors carry spawn/wait/pipe/exit
/// diagnostics and evaluator errors carry the offending pattern — neither
/// may leak secrets, and truncation happens only after redaction.
fn sanitize_outcome(outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Error { message } => Outcome::Error {
            message: clamp_evidence(&redact(&message)),
        },
        Outcome::Fail { reason } => Outcome::Fail {
            reason: clamp_evidence(&redact(&reason)),
        },
        Outcome::Skipped { reason } => Outcome::Skipped {
            reason: clamp_evidence(&redact(&reason)),
        },
        Outcome::Pass => Outcome::Pass,
    }
}

/// Keep the probes selected by `--only` (comma separated); unknown ids error.
fn select_probes<'m>(manifest: &'m Manifest, only: Option<&str>) -> Result<Vec<&'m Probe>> {
    let Some(list) = only else {
        return Ok(manifest.probes.iter().collect());
    };
    let wanted: Vec<String> = list
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect();
    if wanted.is_empty() {
        return Err(anyhow!("--only lists no probe ids"));
    }
    let known: Vec<&str> = manifest.probes.iter().map(|p| p.id.as_str()).collect();
    for id in &wanted {
        if !known.contains(&id.as_str()) {
            return Err(anyhow!("unknown probe id in --only: {id}"));
        }
    }
    Ok(manifest
        .probes
        .iter()
        .filter(|probe| wanted.contains(&probe.id))
        .collect())
}

/// Load every `<...>.json` report in `dir` and print the matrix as markdown.
fn report_dir(dir: &Path) -> Result<()> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read results dir {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();

    let mut reports: Vec<RunReport> = Vec::new();
    for path in &files {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read report {}", path.display()))?;
        match serde_json::from_str::<RunReport>(&text) {
            Ok(report) => reports.push(report),
            Err(e) => eprintln!("warning: skipping {}: {e}", path.display()),
        }
    }
    if reports.is_empty() {
        return Err(anyhow!("no reports found in {}", dir.display()));
    }
    let matrix = Matrix::from_reports(&reports);
    print!("{}", matrix.to_markdown());
    Ok(())
}

/// RFC3339 UTC timestamp (second precision) for `started_at` and filenames.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Make a label safe for a filename (keep alphanumerics, `.`, `-`, `_`).
fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// A fresh run marker for one probe (`selftest-` and 16 random hex digits),
/// or `AWILI_SELFTEST_NONCE` when set (replaying recorded runs).
fn run_marker() -> Result<String> {
    if let Some(fixed) = std::env::var_os("AWILI_SELFTEST_NONCE").filter(|v| !v.is_empty()) {
        return Ok(fixed.to_string_lossy().into_owned());
    }
    let mut bytes = [0u8; 8];
    std::io::Read::read_exact(
        &mut std::fs::File::open("/dev/urandom").context("open /dev/urandom")?,
        &mut bytes,
    )
    .context("read /dev/urandom")?;
    Ok(format!(
        "selftest-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}
