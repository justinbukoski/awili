//! Extra regression coverage added with the P0-weekend-repair implementation
//! (on top of the committed `weekend_repairs` suite): `{prompt_id}` CLI
//! coverage, non-recursive substitution, background-child early-exit /
//! process-group kill, bounded repeated timeouts, and the executor behaviors
//! previously covered by the inline unit tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use awili_selftest::evaluate::Outcome;
use awili_selftest::executor::{render_template, shell_quote, CommandExecutor, ProbeExecutor};
use awili_selftest::report::RunReport;

fn cli(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_awili-selftest"))
        .args(args)
        .output()
        .expect("spawn awili-selftest")
}

fn utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

static TEMP_SEQ: AtomicUsize = AtomicUsize::new(0);

/// A fresh temp dir for this test's artifacts (unique per test).
fn temp_dir(tag: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "awili-selftest-repairs-{}-{tag}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Load the single JSON report a `run` wrote into `dir`.
fn load_single_report(dir: &Path) -> (RunReport, PathBuf) {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read out dir")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(paths.len(), 1, "exactly one report must be written");
    let text = std::fs::read_to_string(&paths[0]).expect("read report");
    (
        serde_json::from_str(&text).expect("report parses"),
        paths.pop().expect("one report"),
    )
}

// --- {prompt_id} / substitution semantics (P0-weekend-repair.md item 5) ---

#[test]
fn render_template_substitutes_both_placeholders_shell_quoted() {
    let rendered = render_template(
        "printf '%s %s' {prompt_id} {prompt}",
        Some("it's a prompt"),
        Some("alpha-x"),
    );
    assert_eq!(rendered, r#"printf '%s %s' 'alpha-x' 'it'\''s a prompt'"#);
}

#[test]
fn render_template_values_are_opaque_no_recursive_substitution() {
    // A prompt that LOOKS like a placeholder must not be re-substituted.
    let rendered = render_template("cat {prompt}", Some("literal {prompt_id} text"), None);
    assert_eq!(rendered, r#"cat 'literal {prompt_id} text'"#);
    // A probe id that LOOKS like a placeholder must not be re-substituted.
    let rendered = render_template("cat {prompt_id}", None, Some("literal {prompt} id"));
    assert_eq!(rendered, r#"cat 'literal {prompt} id'"#);
    // An unbound placeholder is left untouched (alongside a bound one).
    let rendered = render_template("cat {prompt_id} {prompt}", Some("p"), None);
    assert_eq!(rendered, r#"cat {prompt_id} 'p'"#);
}

#[test]
fn cli_run_substitutes_prompt_id_independently_of_prompt() {
    let dir = temp_dir("prompt-id");
    let manifest = dir.join("probes.toml");
    std::fs::write(
        &manifest,
        r#"
version = 1

[[probes]]
id = "alpha-x"
title = "Prompt id substitution"
capability = "Hygiene"
prompt = "say {prompt_id} please"
expect = { ContainsAll = ["alpha-x | say {prompt_id} please"] }
timeout_s = 10
tags = ["weekend-repair"]

[[probes]]
id = "weird {prompt}"
title = "Probe id with a prompt-looking token"
capability = "Hygiene"
prompt = "q1"
expect = { ContainsAll = ["weird {prompt} | q1"] }
timeout_s = 10
tags = ["weekend-repair"]
"#,
    )
    .expect("write manifest");

    let out_dir = dir.join("results");
    let out = cli(&[
        "run",
        "--manifest",
        manifest.to_str().expect("manifest path is utf8"),
        "--executor",
        "local",
        "--model",
        "probeid",
        "--command",
        "printf '%s | %s\n' {prompt_id} {prompt}",
        "--out",
        out_dir.to_str().expect("out dir is utf8"),
    ]);
    assert!(out.status.success(), "run failed: {}", utf8(&out.stderr));

    let (report, path) = load_single_report(&out_dir);
    assert_eq!(report.summary.pass, 2);
    assert_eq!(report.summary.error, 0);
    // The prompt's `{prompt_id}` text arrived verbatim (shell-quoted, not
    // re-substituted), and the probe id's `{prompt}` text did too.
    assert_eq!(report.results[0].probe_id, "alpha-x");
    assert_eq!(
        report.results[0].evidence.trim(),
        "alpha-x | say {prompt_id} please"
    );
    assert_eq!(report.results[1].probe_id, "weird {prompt}");
    assert_eq!(report.results[1].evidence.trim(), "weird {prompt} | q1");
    // The report itself is a valid, well-named JSON file.
    assert!(path
        .file_name()
        .expect("has a name")
        .to_string_lossy()
        .ends_with("-local-probeid.json"));
    assert!(matches!(report.results[0].outcome, Outcome::Pass));
}

#[test]
fn shell_quote_escapes_embedded_single_quotes() {
    assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
    assert_eq!(shell_quote("no-quotes"), "'no-quotes'");
}

// --- Executor behaviors (previously the inline unit tests) ---

#[test]
fn prompt_is_substituted_shell_quoted() {
    let exec = CommandExecutor::new("echo {prompt}", Duration::from_secs(2));
    let answer = exec
        .ask("it's a probe", Duration::from_secs(2))
        .expect("echo must succeed");
    assert_eq!(answer.text.trim_end(), "it's a probe");
}

#[test]
fn stdout_is_text_and_stderr_is_captured_separately() {
    let exec = CommandExecutor::new(
        "sh -c 'printf OUT:%s {prompt}; printf ERR:%s {prompt} >&2'",
        Duration::from_secs(2),
    );
    let answer = exec
        .ask("x", Duration::from_secs(2))
        .expect("command must succeed");
    assert_eq!(answer.text, "OUT:x");
    assert_eq!(answer.stderr, "ERR:x");
}

#[test]
fn nonzero_exit_is_an_error_naming_the_code_and_stderr() {
    let exec = CommandExecutor::new("grep needle {prompt}", Duration::from_secs(5));
    let err = exec
        .ask("/nonexistent-selftest-file", Duration::from_secs(5))
        .expect_err("grep must fail on a missing file");
    let msg = err.to_string();
    assert!(msg.contains("exited 2"), "{msg}");
    assert!(msg.contains("No such file"), "{msg}");
}

#[test]
fn timeout_kills_the_child_and_reports_timeout() {
    let exec = CommandExecutor::new("sleep 5", Duration::from_secs(1));
    let started = Instant::now();
    let err = exec
        .ask("x", Duration::from_secs(5))
        .expect_err("the timeout must fire");
    assert_eq!(err.to_string(), "timeout");
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn executor_timeout_caps_the_call_timeout() {
    let exec = CommandExecutor::new("sleep 30", Duration::from_secs(1));
    let err = exec
        .ask("x", Duration::from_secs(30))
        .expect_err("the executor cap must apply");
    assert_eq!(err.to_string(), "timeout");
}

#[test]
fn call_timeout_wins_over_a_larger_executor_timeout() {
    let exec = CommandExecutor::new("sleep 5", Duration::from_secs(60));
    let started = Instant::now();
    let err = exec
        .ask("x", Duration::from_secs(1))
        .expect_err("the per-call timeout must apply");
    assert_eq!(err.to_string(), "timeout");
    assert!(started.elapsed() < Duration::from_secs(4));
}

// --- Process-group kill: background children holding the pipes (item 2) ---

#[cfg(unix)]
#[test]
fn background_child_early_exit_holder_is_killed_at_deadline() {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let dir = temp_dir("bg-kill");
    let pid_file = dir.join("pid");
    // The parent exits immediately; the background `sleep 5` inherits the
    // pipes and the process group, and is the deadline's real victim.
    let template = format!(
        "sh -c 'sleep 5 & echo $! > {pid}; exit 0'",
        pid = pid_file.display()
    );
    let exec = CommandExecutor::new(&template, Duration::from_millis(150));
    let started = Instant::now();
    let err = exec
        .ask("test", Duration::from_secs(2))
        .expect_err("the pipe deadline must fail the ask");
    assert_eq!(err.to_string(), "timeout");
    assert!(started.elapsed() < Duration::from_secs(2));

    // The background child must be dead: the group kill, not the parent's
    // early exit, is what removed it. SIGKILL delivery and reaping are not
    // instantaneous, so poll for death with a bounded wait. Signal 0 checks
    // liveness without delivering anything.
    let raw = std::fs::read_to_string(&pid_file).expect("pid file written");
    let holder = Pid::from_raw(raw.trim().parse::<i32>().expect("pid parses"));
    let mut alive = true;
    let mut waited = Duration::ZERO;
    while waited < Duration::from_secs(1) {
        match kill(holder, None) {
            Ok(()) => {
                std::thread::sleep(Duration::from_millis(20));
                waited += Duration::from_millis(20);
            }
            Err(Errno::ESRCH) | Err(Errno::EPERM) => {
                alive = false;
                break;
            }
            Err(e) => panic!("unexpected kill-check error: {e}"),
        }
    }
    assert!(
        !alive,
        "the background child must be dead after the deadline"
    );
}

#[cfg(unix)]
#[test]
fn repeated_timeouts_stay_bounded() {
    // The reader architecture spawns no threads: repeated deadline kills
    // must each stay inside their own deadline, every time.
    let exec = CommandExecutor::new("sh -c 'sleep 5 & wait'", Duration::from_millis(100));
    for _ in 0..3 {
        let started = Instant::now();
        let err = exec
            .ask("x", Duration::from_secs(1))
            .expect_err("timeout expected");
        assert_eq!(err.to_string(), "timeout");
        assert!(started.elapsed() < Duration::from_millis(1500));
    }
}

#[test]
fn cli_redacts_failed_commands_and_failed_expectations_in_reports_and_progress() {
    for (name, command, expectation) in [
        (
            "command",
            "sh -c 'printf \"Authorization: Bearer synthetic-private-value\\n\" >&2; exit 7'",
            "expected",
        ),
        (
            "expectation",
            "printf ordinary",
            "token=synthetic-private-value",
        ),
    ] {
        let dir = temp_dir(name);
        let manifest = dir.join("probes.toml");
        std::fs::write(&manifest, format!("version = 1\n[[probes]]\nid = \"redaction\"\ntitle = \"Redaction\"\ncapability = \"Hygiene\"\nprompt = \"test\"\nexpect = {{ ContainsAll = [\"{expectation}\"] }}\ntimeout_s = 2\ntags = []\n")).unwrap();
        let out = dir.join("reports");
        let result = cli(&[
            "run",
            "--manifest",
            manifest.to_str().unwrap(),
            "--executor",
            "local",
            "--model",
            "fixture",
            "--command",
            command,
            "--out",
            out.to_str().unwrap(),
        ]);
        assert!(result.status.success());
        assert!(!utf8(&result.stdout).contains("synthetic-private-value"));
        assert!(!utf8(&result.stderr).contains("synthetic-private-value"));
        let (_, report_path) = load_single_report(&out);
        assert!(!std::fs::read_to_string(report_path)
            .unwrap()
            .contains("synthetic-private-value"));
    }
}
