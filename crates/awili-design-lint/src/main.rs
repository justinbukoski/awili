//! `awili-design-lint` CLI — the design-token ratchet (slice 2).
//!
//! ```text
//! awili-design-lint check <src-dir> --baseline <file> [--reference <file>]
//!     # exit 1 on any NEW finding, or on any baseline entry not present in
//!     # --reference (the baseline can only shrink against the committed one)
//! awili-design-lint check <src-dir> --baseline <file> --update-baseline
//!     # rewrites the baseline with only the findings that still exist; exit 1
//!     # and writes nothing if any finding is NOT in the baseline
//! awili-design-lint check <src-dir> --baseline <file> --init
//!     # one-shot: writes the baseline from the current tree, only while the
//!     # file does not exist yet
//! awili-design-lint list <src-dir>
//!     # print all findings
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use awili_design_lint::{check, init_baseline, reference_violations, scan, update_baseline};

const USAGE: &str = "\
usage:
  awili-design-lint check <src-dir> --baseline <file> [--reference <file>]
      [--init | --update-baseline]
  awili-design-lint list <src-dir>

check  compares the scan against the baseline: prints each new finding as
       'path:line  kind  text', then 'N new finding(s)', then exits 1 if any
       are new. Stale baseline entries are reported without affecting the
       exit code. --update-baseline rewrites the baseline keeping only the
       findings that still exist, and refuses (exit 1, no write) if any
       finding is not in the baseline. --init is one-shot and writes the
       baseline from the current tree only while the file does not exist.
       --reference <file> additionally FAILS (exit 1) if the baseline holds
       any fingerprint not present in the reference file: the working
       baseline may only SHRINK against the committed one.
list   prints every finding as 'path:line  kind  text'.
";

struct CheckArgs {
    src_dir: PathBuf,
    baseline: PathBuf,
    /// Shrink-only reference (02b item 9): the baseline must not hold any
    /// fingerprint absent from this file.
    reference: Option<PathBuf>,
    init: bool,
    update: bool,
}

enum Command {
    Check(CheckArgs),
    List(PathBuf),
}

fn parse(args: &[String]) -> Result<Command, String> {
    let mut src_dir = None;
    let mut baseline = None;
    let mut reference = None;
    let mut init = false;
    let mut update = false;

    let mut pos = 0;
    let cmd = args
        .get(pos)
        .cloned()
        .ok_or("missing subcommand: check | list (see --help)")?;
    pos += 1;
    match cmd.as_str() {
        "check" => {
            while pos < args.len() {
                let a = args[pos].clone();
                pos += 1;
                match a.as_str() {
                    "--baseline" => {
                        let v = args
                            .get(pos)
                            .cloned()
                            .ok_or("missing value for --baseline <file>")?;
                        pos += 1;
                        baseline = Some(PathBuf::from(v));
                    }
                    "--reference" => {
                        let v = args
                            .get(pos)
                            .cloned()
                            .ok_or("missing value for --reference <file>")?;
                        pos += 1;
                        reference = Some(PathBuf::from(v));
                    }
                    "--init" => init = true,
                    "--update-baseline" => update = true,
                    s if s == "-h" || s == "--help" => return Err(USAGE.to_string()),
                    s if s.starts_with("--") => {
                        return Err(format!("unknown flag {s} (see --help)"))
                    }
                    s => {
                        if src_dir.is_some() {
                            return Err(format!("unexpected extra argument {s:?}"));
                        }
                        src_dir = Some(PathBuf::from(s));
                    }
                }
            }
            let src_dir = src_dir.ok_or("missing <src-dir> (see --help)")?;
            if init && update {
                return Err("--init and --update-baseline are mutually exclusive".to_string());
            }
            Ok(Command::Check(CheckArgs {
                src_dir,
                baseline: baseline.ok_or("missing --baseline <file> (see --help)")?,
                reference,
                init,
                update,
            }))
        }
        "list" => {
            let mut dirs = Vec::new();
            while pos < args.len() {
                let a = args[pos].clone();
                pos += 1;
                if a == "-h" || a == "--help" {
                    return Err(USAGE.to_string());
                }
                dirs.push(a);
            }
            match dirs.as_slice() {
                [d] => Ok(Command::List(PathBuf::from(d))),
                [] => Err("missing <src-dir> (see --help)".to_string()),
                _ => Err(format!("unexpected extra argument {:?}", &dirs[1..])),
            }
        }
        "-h" | "--help" => Err(USAGE.to_string()),
        other => Err(format!("unknown subcommand {other:?} (see --help)")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("awili-design-lint: {err}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    use std::io::Write;
    let out = std::io::stdout();
    let mut out = out.lock();

    match parse(args)? {
        Command::List(src_dir) => {
            let findings = scan(&src_dir).map_err(|e| e.to_string())?;
            for f in &findings {
                writeln!(out, "{}", f.display()).map_err(|e| e.to_string())?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Check(a) => {
            if a.init {
                let n = init_baseline(&a.src_dir, &a.baseline).map_err(|e| e.to_string())?;
                writeln!(
                    out,
                    "baseline written: {n} finding(s) in {}",
                    a.baseline.display()
                )
                .map_err(|e| e.to_string())?;
                return Ok(ExitCode::SUCCESS);
            }
            if !a.baseline.exists() {
                return Err(format!(
                    "no baseline file at {} — write it first: awili-design-lint check {} --baseline {} --init",
                    a.baseline.display(),
                    a.src_dir.display(),
                    a.baseline.display()
                ));
            }

            if a.update {
                // Never add: refuse (exit 1, no write) if any finding is not
                // in the baseline; otherwise rewrite keeping only the
                // findings that still exist.
                let old = awili_design_lint::read_baseline(&a.baseline)
                    .map_err(|e| e.to_string())?
                    .findings
                    .len();
                let outcome = check(&a.src_dir, &a.baseline).map_err(|e| e.to_string())?;
                if !outcome.new.is_empty() {
                    for f in &outcome.new {
                        writeln!(out, "{}", f.display()).map_err(|e| e.to_string())?;
                    }
                    writeln!(out, "{} new finding(s)", outcome.new.len())
                        .map_err(|e| e.to_string())?;
                    writeln!(
                        out,
                        "refusing to update baseline: {} finding(s) not in baseline (the baseline can only shrink)",
                        outcome.new.len()
                    )
                    .map_err(|e| e.to_string())?;
                    return Ok(ExitCode::FAILURE);
                }
                let kept = update_baseline(&a.src_dir, &a.baseline).map_err(|e| e.to_string())?;
                writeln!(
                    out,
                    "baseline updated: kept {kept} of {old} entry(ies) (dropped {})",
                    old - kept
                )
                .map_err(|e| e.to_string())?;
                return Ok(ExitCode::SUCCESS);
            }

            let outcome = check(&a.src_dir, &a.baseline).map_err(|e| e.to_string())?;
            for f in &outcome.new {
                writeln!(out, "{}", f.display()).map_err(|e| e.to_string())?;
            }
            writeln!(out, "{} new finding(s)", outcome.new.len()).map_err(|e| e.to_string())?;
            if !outcome.stale.is_empty() {
                writeln!(
                    out,
                    "{} baseline entries no longer present — run --update-baseline",
                    outcome.stale.len()
                )
                .map_err(|e| e.to_string())?;
            }
            // Shrink-only against the committed reference (02b item 9): the
            // working baseline may drop entries (debt paid down) but must
            // never hold a fingerprint the reference does not have.
            if let Some(ref_path) = &a.reference {
                let baseline =
                    awili_design_lint::read_baseline(&a.baseline).map_err(|e| e.to_string())?;
                let reference =
                    awili_design_lint::read_baseline(ref_path).map_err(|e| e.to_string())?;
                let extra = reference_violations(&baseline, &reference);
                if !extra.is_empty() {
                    for fp in &extra {
                        writeln!(out, "{}", fp).map_err(|e| e.to_string())?;
                    }
                    writeln!(
                        out,
                        "{} baseline entry(ies) not in reference {} — the baseline can only shrink",
                        extra.len(),
                        ref_path.display()
                    )
                    .map_err(|e| e.to_string())?;
                    return Ok(ExitCode::FAILURE);
                }
            }
            if outcome.new.is_empty() {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::FAILURE)
            }
        }
    }
}
