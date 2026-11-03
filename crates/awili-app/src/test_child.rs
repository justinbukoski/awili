//! RC1-G3: run one test alone in its own process, for the tests that change
//! process-wide state (`HOME`, `XDG_DATA_HOME`, `agent_launch`'s
//! `TEST_PATHS`). Every test of the shared test process sees that state, so a
//! sibling running at the same moment read the change (RC-16):
//! `build_child_path_orders_expands_and_dedups` expanded `~` with another
//! test's `HOME`, and `recoverable_tool_stream_continues_but_terminal_denials_
//! still_fail` launched with another test's blocked `TEST_PATHS` and got the
//! "could not prepare the agent's private settings" note first.

/// Marks a child started by [`in_own_process`]: the one test in its process.
/// Its value is `<test>@<pid of the process that started the child>`.
pub(crate) const OWN_PROCESS: &str = "AWILI_TEST_OWN_PROCESS";

/// RC1-G3-FIX1: the test this process was started for, when the marker is
/// genuinely this process's: it must name this process's parent. A marker
/// inherited from anywhere else (exported into a shared `cargo test` run, say)
/// names another process and switches nothing off.
fn own_test() -> Option<String> {
    let value = std::env::var(OWN_PROCESS).ok()?;
    let (test, parent) = value.rsplit_once('@')?;
    (parent.parse::<u32>().ok()? == parent_id()).then(|| test.to_string())
}

#[cfg(unix)]
fn parent_id() -> u32 {
    std::os::unix::process::parent_id()
}

/// TODO(windows-phase1): std has no parent-pid on Windows, so a marker is
/// never accepted as genuine and the own-process tests run (and skip) as
/// they would in a shared run.
#[cfg(not(unix))]
fn parent_id() -> u32 {
    0
}

/// `true` inside the child started for exactly `test`. Anywhere else: run
/// `test` (its full path, as `--exact` takes it) alone in a child process,
/// require that it ran and passed, and return `false`.
pub(crate) fn in_own_process(test: &str) -> bool {
    if own_test().as_deref() == Some(test) {
        return true;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(OWN_PROCESS, format!("{test}@{}", std::process::id()))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{test} failed in its own process:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "the child must actually run {test}: {stdout}"
    );
    false
}

/// Panic unless this test runs in its own process: every change to
/// process-wide test state calls this first.
pub(crate) fn assert_own_process(what: &str) {
    assert!(
        own_test().is_some(),
        "{what} is process-wide: change it only in a test that runs in its own process \
         (crate::test_child::in_own_process)"
    );
}

/// RC1-G3-FIX1 proof: a marker inherited by a shared run (it names a process
/// that is not the parent) switches no guard off. Each proof test is run in a
/// subprocess carrying such a marker; it must run its assertions (it prints
/// its line only after them) and pass.
#[test]
fn rc1g3_fix1_an_inherited_marker_switches_no_guard_off() {
    for (test, line) in [
        (
            "code_provider::tests::rc1g3_env_guards_refuse_a_shared_process",
            "RC1G3: HomeGuard and XdgDataGuard refused in a shared process",
        ),
        (
            "agent_launch::tests::rc1g3_test_paths_refuse_a_shared_process",
            "RC1G3: set_test_paths refused in a shared process",
        ),
    ] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(OWN_PROCESS, format!("{test}@{}", u32::MAX))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed") && stdout.contains(line),
            "{test} with an inherited marker must refuse, not skip:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
