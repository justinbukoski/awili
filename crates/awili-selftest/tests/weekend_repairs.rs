//! Regression requirements committed before implementation.
use awili_selftest::evaluate::{evaluate, Outcome};
use awili_selftest::executor::{CommandExecutor, ProbeExecutor};
use awili_selftest::manifest::Expect;
use std::time::{Duration, Instant};

#[test]
fn regex_default_ignores_case_but_explicit_override_works() {
    assert_eq!(
        evaluate(&Expect::Regex("load average".into()), "LOAD AVERAGE"),
        Outcome::Pass
    );
    assert_ne!(
        evaluate(&Expect::Regex("(?-i:load average)".into()), "LOAD AVERAGE"),
        Outcome::Pass
    );
}

#[test]
fn sentinel_default_ignores_case() {
    assert_eq!(
        evaluate(&Expect::Sentinel("NO IMAGE INPUT".into()), "no image input"),
        Outcome::Fail {
            reason: "declared-missing".into()
        }
    );
}

#[test]
fn failure_diagnostics_redact_before_truncation() {
    let exec = CommandExecutor::new(
        "sh -c 'printf \"Authorization: Bearer synthetic-test-secret\\n\" >&2; exit 7'",
        Duration::from_secs(2),
    );
    let error = exec
        .ask("test", Duration::from_secs(2))
        .expect_err("must fail")
        .to_string();
    assert!(error.contains('7'));
    assert!(
        !error.contains("synthetic-test-secret"),
        "unredacted diagnostic"
    );
}

#[test]
fn timeout_bounds_descendants_holding_pipes() {
    let exec = CommandExecutor::new("sh -c 'sleep 5 & wait'", Duration::from_millis(200));
    let start = Instant::now();
    assert_eq!(
        exec.ask("test", Duration::from_secs(2))
            .expect_err("timeout")
            .to_string(),
        "timeout"
    );
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn successful_parent_cannot_escape_pipe_deadline() {
    let exec = CommandExecutor::new("sh -c 'sleep 5 & exit 0'", Duration::from_millis(200));
    let start = Instant::now();
    assert!(exec.ask("test", Duration::from_secs(2)).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[test]
fn overflowing_deadline_fails_before_spawning() {
    let marker = std::env::temp_dir().join(format!("awili-overflow-{}", std::process::id()));
    let template = format!(
        "touch {}",
        awili_selftest::executor::shell_quote(&marker.to_string_lossy())
    );
    let exec = CommandExecutor::new(template, Duration::MAX);
    let error = exec
        .ask("test", Duration::MAX)
        .expect_err("overflow must fail");
    assert!(error.to_string().contains("out of range"));
    assert!(
        !marker.exists(),
        "must not spawn before validating deadline"
    );
}

#[cfg(unix)]
#[test]
fn timeout_kills_owned_child_after_it_moves_to_parent_process_group() {
    use awili_selftest::executor::shell_quote;
    let marker = std::env::temp_dir().join(format!("awili-group-moved-{}", std::process::id()));
    // The child is this test binary re-run as `group_move_child` (below): it
    // joins its parent's (this process's) group, writes the marker and stays
    // alive for at most 3 seconds.
    let exe = std::env::current_exe().unwrap();
    let executor = CommandExecutor::new(
        format!(
            "env {GROUP_MOVE_MARKER}={} {} --exact group_move_child --nocapture",
            shell_quote(&marker.to_string_lossy()),
            shell_quote(&exe.to_string_lossy())
        ),
        Duration::from_millis(300),
    );
    let start = Instant::now();
    let error = executor
        .ask("test", Duration::from_secs(2))
        .expect_err("must time out");
    assert_eq!(error.to_string(), "timeout");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("child moved before timeout"),
        "moved"
    );
    std::fs::remove_file(marker).unwrap();
    // The child's own 3-second bound limits this test even on the old bug;
    // correct cleanup returns at the 300-ms deadline, never signals our group.
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[cfg(unix)]
const GROUP_MOVE_MARKER: &str = "AWILI_GROUP_MOVE_MARKER";

/// The child of the test above (a no-op in an ordinary test run): move into
/// the parent's process group, write "moved" to the marker, then live at
/// most 3 seconds.
#[cfg(unix)]
#[test]
fn group_move_child() {
    use nix::unistd::{getpgid, getppid, setpgid, Pid};
    let Some(marker) = std::env::var_os(GROUP_MOVE_MARKER) else {
        return;
    };
    let parent_group = getpgid(Some(getppid())).unwrap();
    setpgid(Pid::from_raw(0), parent_group).unwrap();
    std::fs::write(marker, "moved").unwrap();
    std::thread::sleep(Duration::from_secs(3));
    std::process::exit(0);
}
