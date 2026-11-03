//! Headless runner. Commands accept JSON stdin and emit one JSON result.
use anyhow::{bail, ensure, Result};
use awili_dispatch::{protocol::*, runner, store::Store};
use std::{
    io::{self, Read},
    path::PathBuf,
};
fn input<T: serde::de::DeserializeOwned>() -> Result<T> {
    let mut bytes = vec![];
    io::stdin()
        .take(MAX_REQUEST_JSON_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_REQUEST_JSON_BYTES, "request too large");
    Ok(serde_json::from_slice(&bytes)?)
}
fn emit(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}
/// The runner's command table: every command `--config PATH <command>` runs
/// (`supervise` also takes the request UUID). `awili-build-runner commands`
/// prints it one per line, so an install can refuse a runner that lacks a
/// command its service unit calls (deploy/awili-build-hostb/prepare.sh checks
/// `supervise`). A command outside the table is refused before anything else.
const COMMANDS: &[&str] = &[
    "validate",
    "submit",
    "status",
    "cancel",
    "logs",
    "supervise",
];

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 1 && args[0] == "commands" {
        println!("{}", COMMANDS.join("\n"));
        return Ok(());
    }
    ensure!(
        args.len() >= 3,
        "usage: --config PATH submit|status|logs|cancel|validate, --config PATH supervise UUID, or commands"
    );
    if args[0] == "--root" {
        ensure!(
            args.len() == 4 && args[2] == "worker",
            "invalid internal invocation"
        );
        // The worker reports its own errors, a job-ending one in worker.log
        // before the terminal state is published; printing it here again,
        // after that state, would duplicate it.
        if runner::run_worker(&PathBuf::from(&args[1]), &args[3]).is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
    ensure!(args[0] == "--config", "invalid invocation");
    ensure!(
        COMMANDS.contains(&args[2].as_str()),
        "unknown command `{}`",
        args[2]
    );
    let supervise = args[2] == "supervise";
    ensure!(
        args.len() == if supervise { 4 } else { 3 },
        "invalid invocation"
    );
    if supervise {
        // The awili-build@<UUID>.service main process (replaces supervised-run.py).
        let config = RunnerConfig::load(std::path::Path::new(&args[1]))?;
        let supervisor = awili_dispatch::supervisor::Supervisor {
            root: awili_dispatch::supervisor::ROOT.into(),
            config,
            executable: std::env::current_exe()?,
            poll: awili_dispatch::supervisor::POLL,
            window: awili_dispatch::supervisor::SUPERVISION_WINDOW,
        };
        let code = supervisor.run(&args[3], &mut io::stdout())?;
        std::process::exit(code);
    }
    let config = RunnerConfig::load(std::path::Path::new(&args[1]))?;
    match args[2].as_str() {
        "validate" => {
            let request: BuildRequest = input()?;
            request.validate()?;
            config.target(&request)?;
            emit(&awili_dispatch::supervised::ValidatedRequest {
                request_sha256: request.fingerprint(),
                request,
            })
        }
        "submit" => emit(&runner::submit(
            &config,
            input()?,
            &std::env::current_exe()?,
        )?),
        "status" => emit(&runner::status(
            &config.job_root,
            &input::<JobQuery>()?.request_id,
        )?),
        "cancel" => emit(&runner::cancel(
            &config.job_root,
            &input::<JobQuery>()?.request_id,
        )?),
        "logs" => emit(&Store::open(&config.job_root)?.logs(input()?)?),
        _ => bail!("unknown command"),
    }
}
pub fn main() {
    if let Err(e) = run() {
        eprintln!("{e:#}");
        std::process::exit(1);
    }
}
