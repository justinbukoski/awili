//! awili-seam — the C2-u5 LIVE acceptance runner.
//!
//! One live turn against an OpenAI-compatible local endpoint, sandbox
//! rooted at --dir, builtin fs tools advertised, mutating calls approved
//! `once` interactively-by-default (--yes auto-approves for the recorded
//! acceptance run). The B2 log is written to a FILE (--log, default
//! ./awili-seam-live.b2.jsonl) so the audit/replay artifact SURVIVES the
//! process — it is part of the recorded evidence.
//!
//! PASS requires (round-1 P1): every intent Clean, zero AuthzInvalid,
//! zero Uncertain, at least one MUTATING builtin (fs.write/fs.edit)
//! Clean ok=true, and — when --expect FILE SUBSTR is given (the
//! acceptance run must give it) — the file under --dir containing the
//! substring, verified on disk by THIS process.
//!
//! Evidence from the successful run is recorded in
//! docs/reviews/c2-u5-design-sketch-2026-08-13.md (design blocker 2:
//! the live run gates the SLICE's merge).

use std::io::Write as _;
use std::process::ExitCode;
use std::sync::OnceLock;

use awili_engine_authz::{Authorizer, UserVerdict};
use awili_engine_log::authz::issue_bundle;
use awili_engine_log::replay::IntentStatus;
use awili_engine_log::storage::AppendOnlyLog;
use awili_engine_log::writer::ExecutionLog;
use awili_engine_log::{LogError, MemLog, RiskClass};
use awili_seam::{run_turn, ApprovalOutcome, HostAuthority, HttpTransport, SeamError, TurnHost};

/// Append-only log backed by a file: the surviving replay artifact.
struct FileLog {
    file: std::fs::File,
}

impl FileLog {
    fn create(path: &str) -> std::io::Result<Self> {
        Ok(Self {
            file: std::fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .append(true)
                .open(path)?,
        })
    }
}

impl AppendOnlyLog for FileLog {
    fn append(&mut self, bytes: &[u8], sync: bool) -> Result<(), LogError> {
        self.file
            .write_all(bytes)
            .and_then(|()| if sync { self.file.sync_all() } else { Ok(()) })
            .map_err(|e| LogError::Storage(e.to_string()))
    }
    fn read_all(&self) -> Result<Vec<u8>, LogError> {
        use std::io::Read;
        let mut f = self
            .file
            .try_clone()
            .map_err(|e| LogError::Storage(e.to_string()))?;
        let mut buf = Vec::new();
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(0))
            .map_err(|e| LogError::Storage(e.to_string()))?;
        f.read_to_end(&mut buf)
            .map_err(|e| LogError::Storage(e.to_string()))?;
        Ok(buf)
    }
}

/// One process bootstrap for the singleton authority bundle (design P2;
/// unit 4 r6/r7: this separate-process acceptance runner is itself a
/// production composition root, so it claims the bundle directly — the
/// `test-authority` fixture is for test processes only). The registry
/// authority rides the bundle into the seam composition, which mints the
/// resolved identity for every registry resolution (unit 4 item 3).
fn host() -> &'static HostAuthority {
    static HOST: OnceLock<HostAuthority> = OnceLock::new();
    HOST.get_or_init(|| {
        let (gate, cap, registry_authority, _install_seed) =
            issue_bundle().expect("sole issue in this process");
        HostAuthority {
            authorizer: Authorizer::new(gate),
            approval_cap: cap,
            registry: registry_authority,
        }
    })
}

struct CliApprover {
    auto_yes: bool,
}

impl TurnHost for CliApprover {
    fn approve(&mut self, _call_id: &str, prompt: &str, risk: RiskClass) -> ApprovalOutcome {
        println!("APPROVAL REQUIRED [{}]: {prompt}", risk.as_str());
        if self.auto_yes {
            println!("  --yes: approved (scope: once)");
            return ApprovalOutcome::Verdict(UserVerdict::Approve {
                scope: "once".into(),
            });
        }
        print!("  approve once? [y/N] ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        ApprovalOutcome::Verdict(if line.trim().eq_ignore_ascii_case("y") {
            UserVerdict::Approve {
                scope: "once".into(),
            }
        } else {
            UserVerdict::Deny {
                reason: "declined at the CLI".into(),
            }
        })
    }
}

struct Args {
    base_url: String,
    model: String,
    dir: String,
    log_path: String,
    auto_yes: bool,
    expect: Option<(String, String)>,
    window_tokens: u64,
    prompt: String,
    /// C4 live gate: run a SECOND turn over the same session history. With a
    /// small --window-tokens this forces a live compaction cycle (summary
    /// produced by the same endpoint) before the second turn completes.
    prompt2: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut base_url = None;
    let mut model = None;
    let mut dir = None;
    let mut log_path = "awili-seam-live.b2.jsonl".to_string();
    let mut auto_yes = false;
    let mut expect = None;
    let mut window_tokens = awili_seam::DEFAULT_WINDOW_TOKENS;
    let mut prompt2 = None;
    let mut prompt: Option<String> = None;
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        match argv.get(*i) {
            Some(v) if v.starts_with("--") => {
                Err(format!("{flag} requires a value, got flag {v:?}"))
            }
            Some(v) => Ok(v.clone()),
            None => Err(format!("{flag} requires a value")),
        }
    };
    while i < argv.len() {
        match argv[i].as_str() {
            "--base-url" => base_url = Some(value(&mut i, "--base-url")?),
            "--model" => model = Some(value(&mut i, "--model")?),
            "--dir" => dir = Some(value(&mut i, "--dir")?),
            "--log" => log_path = value(&mut i, "--log")?,
            "--yes" => auto_yes = true,
            "--prompt2" => prompt2 = Some(value(&mut i, "--prompt2")?),
            "--window-tokens" => {
                let v = value(&mut i, "--window-tokens")?;
                window_tokens = v
                    .parse()
                    .map_err(|e| format!("--window-tokens must be a number: {e}"))?;
            }
            "--expect" => {
                let f = value(&mut i, "--expect")?;
                let s = value(&mut i, "--expect")?;
                expect = Some((f, s));
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown option {flag}"));
            }
            positional => {
                if prompt.is_some() {
                    return Err(format!(
                        "unexpected extra positional argument {positional:?} (prompt already set)"
                    ));
                }
                prompt = Some(positional.to_string());
            }
        }
        i += 1;
    }
    Ok(Args {
        base_url: base_url.ok_or("--base-url is required")?,
        model: model.ok_or("--model is required")?,
        dir: dir.ok_or("--dir is required")?,
        log_path,
        auto_yes,
        expect,
        window_tokens,
        prompt: prompt.ok_or("PROMPT is required")?,
        prompt2,
    })
}

const MUTATING_BUILTINS: [&str; 2] = ["fs.write", "fs.edit"];

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "error: {e}\nusage: awili-seam --base-url URL --model NAME --dir DIR \
                 [--log PATH] [--yes] [--expect FILE SUBSTR] [--window-tokens N] \
                 [--prompt2 TEXT] PROMPT"
            );
            return ExitCode::from(2);
        }
    };

    let host = host();
    let mut transport = HttpTransport::new(&args.base_url, std::time::Duration::from_secs(180));
    let mut approver = CliApprover {
        auto_yes: args.auto_yes,
    };
    let file_log = match FileLog::create(&args.log_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: cannot create B2 log {}: {e}", args.log_path);
            return ExitCode::from(2);
        }
    };
    let mut log = ExecutionLog::open(file_log, "sess-seam-live").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::new()).unwrap();

    let mut history = awili_seam::SessionHistory::new();
    // Unit 4 item 6: this runner composes the builtin executor only —
    // live MCP wiring (runtime instantiation, bring-up) is unit 4b.
    let executors = awili_seam::Executors::builtin_only();
    let roots = awili_seam::RootsAcceptance::default();
    let result = run_turn(
        host,
        &mut transport,
        &args.model,
        &args.dir,
        &args.prompt,
        &mut approver,
        &mut log,
        &mut grants,
        &[],
        &executors,
        &awili_engine_authz::NoInstallAuthority,
        &roots,
        args.window_tokens,
        "turn-seam-0",
        &mut history,
    );

    // C4: optional second turn on the SAME session history (live
    // forced-compaction evidence). Runs before turn-1 evaluation so a
    // failure here fails the whole acceptance.
    let mut turn2_clean = true;
    if let (Some(p2), true) = (&args.prompt2, result.is_ok()) {
        let path2 = format!("{}.turn2", args.log_path);
        let file_log2 = match FileLog::create(&path2) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("error: cannot create turn-2 B2 log {path2}: {e}");
                return ExitCode::from(2);
            }
        };
        let mut log2 = ExecutionLog::open(file_log2, "sess-seam-live-t2").unwrap();
        let mut grants2 = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
        match run_turn(
            host,
            &mut transport,
            &args.model,
            &args.dir,
            p2,
            &mut approver,
            &mut log2,
            &mut grants2,
            &[],
            &executors,
            &awili_engine_authz::NoInstallAuthority,
            &roots,
            args.window_tokens,
            "turn-seam-1",
            &mut history,
        ) {
            Ok(r2) => {
                println!("\n=== TURN 2 ({} requests) ===", r2.requests.len());
                for (i, req) in r2.requests.iter().enumerate() {
                    let summarizer = req.contains("Summarize the following working context");
                    println!(
                        "  request {i}: {} bytes{}",
                        req.len(),
                        if summarizer {
                            " [SUMMARIZER — live compaction]"
                        } else {
                            ""
                        }
                    );
                }
                for ev in &r2.events {
                    println!("  t2 {ev:?}");
                }
                for turn in &r2.replay {
                    for intent in &turn.intents {
                        if !matches!(intent.status, IntentStatus::Clean { .. }) {
                            turn2_clean = false;
                            println!("  t2 !! non-Clean intent: {:?}", intent.status);
                        }
                    }
                }
                println!("  t2 final text: {}", r2.final_text);
            }
            Err(e) => {
                eprintln!("TURN 2 FAILED: {e}");
                turn2_clean = false;
            }
        }
    }

    match result {
        Ok(report) => {
            println!("\n=== EVENT TRACE ({} events) ===", report.events.len());
            for ev in &report.events {
                println!("  {ev:?}");
            }
            println!("\n=== REPLAY ({}) ===", args.log_path);
            let mut all_clean = true;
            let mut intents = 0usize;
            let mut mutating_ok = false;
            for turn in &report.replay {
                for intent in &turn.intents {
                    intents += 1;
                    println!(
                        "  {} {} -> {:?}",
                        intent.intent_id, intent.tool, intent.status
                    );
                    match &intent.status {
                        IntentStatus::Clean { ok } => {
                            if *ok && MUTATING_BUILTINS.contains(&intent.tool.as_str()) {
                                mutating_ok = true;
                            }
                        }
                        other => {
                            all_clean = false;
                            println!("    !! non-Clean status: {other:?}");
                        }
                    }
                }
            }
            let file_ok = match &args.expect {
                None => {
                    println!(
                        "FILE CHECK FAILED: --expect FILE SUBSTR was not given; \
                         PASS requires on-disk verification (round-2 P1)"
                    );
                    false
                }
                // RC2-ENG5 (T26): the file must resolve inside --dir.
                // RC2-ENGG1: and is read in the same step, through
                // descriptors anchored to --dir — never reopened by name.
                Some((file, substr)) => {
                    let shown = std::path::Path::new(&args.dir).join(file);
                    match awili_seam::expect_file_in_dir(std::path::Path::new(&args.dir), file) {
                        Err(e) => {
                            println!("FILE CHECK FAILED: {e}");
                            false
                        }
                        Ok(contents) if contents.contains(substr.as_str()) => {
                            println!("verified: {} contains {substr:?}", shown.display());
                            true
                        }
                        Ok(contents) => {
                            println!(
                                "FILE CHECK FAILED: {} does not contain {substr:?} (got {} bytes)",
                                shown.display(),
                                contents.len()
                            );
                            false
                        }
                    }
                }
            };
            println!("\nfinal text: {}", report.final_text);
            println!("rounds: {}", report.rounds);
            let pass = all_clean && intents > 0 && mutating_ok && file_ok && turn2_clean;
            println!(
                "\nACCEPTANCE: {}",
                if pass {
                    "PASS — all intents Clean, mutating edit landed, zero AuthzInvalid, zero Uncertain"
                } else {
                    "FAIL"
                }
            );
            if !mutating_ok {
                println!(
                    "  (no Clean ok=true fs.write/fs.edit intent — a real file edit is required)"
                );
            }
            if pass {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(SeamError::TurnFailed { reason, report }) => {
            eprintln!("TURN FAILED: {reason}");
            for ev in &report.events {
                eprintln!("  {ev:?}");
            }
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("SEAM ERROR: {e}");
            ExitCode::FAILURE
        }
    }
}
