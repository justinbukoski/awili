use super::*;
use std::time::Instant;

#[test]
fn shell_quote_wraps_and_escapes_embedded_quotes() {
    assert_eq!(shell_quote("plain"), "'plain'");
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
    assert_eq!(shell_quote("a b"), "'a b'");
}

#[test]
fn prompt_is_substituted_shell_quoted() {
    let exec = CommandExecutor::new("echo {prompt}", Duration::from_secs(5));
    let answer = exec
        .ask("it's a probe", Duration::from_secs(5))
        .expect("echo succeeds");
    assert_eq!(answer.text.trim_end(), "it's a probe");
    assert!(answer.stderr.is_empty());
}

#[test]
fn stdout_is_text_and_stderr_is_captured_separately() {
    // Single-command template: the nested sh writes both streams, so the
    // exec'd child is one process and both pipes are captured.
    let exec = CommandExecutor::new(
        "sh -c 'printf OUT:%s {prompt}; printf ERR:%s {prompt} >&2'",
        Duration::from_secs(5),
    );
    let answer = exec
        .ask("x", Duration::from_secs(5))
        .expect("printf succeeds");
    assert_eq!(answer.text, "OUT:x");
    assert_eq!(answer.stderr, "ERR:x");
}

#[test]
fn nonzero_exit_is_an_error_naming_the_code_and_stderr() {
    let exec = CommandExecutor::new(
        "grep selftest /nonexistent-selftest-file",
        Duration::from_secs(5),
    );
    let err = exec
        .ask("x", Duration::from_secs(5))
        .expect_err("grep of a missing file must error");
    let msg = err.to_string();
    assert!(msg.contains("exited 2"), "{msg}");
    assert!(msg.contains("No such file"), "{msg}");
}

#[test]
fn timeout_kills_the_child_and_reports_timeout() {
    let exec = CommandExecutor::new("sleep 5", Duration::from_secs(1));
    let started = Instant::now();
    let err = exec
        .ask("x", Duration::from_secs(1))
        .expect_err("sleep 5 must time out");
    assert_eq!(err.to_string(), "timeout");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "the child must be killed at the deadline, not after sleep ends"
    );
}

#[test]
fn executor_timeout_caps_the_call_timeout() {
    // The executor's own 1 s cap wins over a 30 s per-call timeout.
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

#[cfg(unix)]
#[test]
fn noisy_command_hits_combined_output_limit() {
    let executor = CommandExecutor::new("head -c 1100000 /dev/zero", Duration::from_secs(5));
    let err = executor.ask("unused", Duration::from_secs(5)).unwrap_err();
    assert!(err.to_string().contains("output exceeds size limit"));
}
