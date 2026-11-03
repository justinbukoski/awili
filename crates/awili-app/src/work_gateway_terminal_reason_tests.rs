//! P3i — the `WorkUpdate::Terminal` arm's reason plumbing, through the real
//! production reducer (`apply_work_update`) on the real `AwiliApp`: a stopped
//! run reads `stopped` (no `run ended: stopped` error card), while a failed
//! run with no worker output still synthesizes its reason.

use super::*;

/// A `Terminal { Stopped }` on a running panel leaves `panel.error == None`
/// and sets `panel.status` to `Stopped` — the header renders `stopped` from
/// the status, not `failed` from a synthesized error card.
#[gpui::test]
fn stopped_terminal_does_not_set_an_error(cx: &mut gpui::TestAppContext) {
    const CHILD: &str = "AWILI_TERMINAL_REASON_TEST_HOME";
    const TEST: &str =
        "work::gateway_terminal_reason_tests::stopped_terminal_does_not_set_an_error";
    let Some(home) = isolated_test(CHILD, TEST) else {
        return;
    };

    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        app.work.gateway_cmd_tx = Some(cmd_tx);
        app.work.running = true;
        let conv = app.work_conv_id();
        app.work.graduation.arm(&conv);
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        // A mid-turn Completion already put the run's text on the panel.
        app.work.panel.output = Some("partial run text".into());

        app.apply_work_update(
            WorkUpdate::Terminal {
                status: RunStatus::Stopped,
                output: None,
                model: None,
            },
            cx,
        );

        assert_eq!(app.work.panel.error, None);
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
        // A stop with no worker output keeps the text already on the panel.
        assert_eq!(app.work.panel.output.as_deref(), Some("partial run text"));
        assert!(!app.work.running);
        let _ = cmd_rx;
    });
}

/// A `Terminal { Failed }` with no worker output still synthesizes its
/// reason — today's behaviour, guarded.
#[gpui::test]
fn failed_terminal_still_synthesizes_its_reason(cx: &mut gpui::TestAppContext) {
    const CHILD: &str = "AWILI_TERMINAL_REASON_TEST_HOME";
    const TEST: &str =
        "work::gateway_terminal_reason_tests::failed_terminal_still_synthesizes_its_reason";
    let Some(home) = isolated_test(CHILD, TEST) else {
        return;
    };

    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        app.work.gateway_cmd_tx = Some(cmd_tx);
        app.work.running = true;
        let conv = app.work_conv_id();
        app.work.graduation.arm(&conv);
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());

        app.apply_work_update(
            WorkUpdate::Terminal {
                status: RunStatus::Failed,
                output: None,
                model: None,
            },
            cx,
        );

        assert_eq!(app.work.panel.error.as_deref(), Some("run ended: failed"));
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        assert!(!app.work.running);
        let _ = cmd_rx;
    });
}

/// The isolated-subprocess harness, mirroring `isolated` (which asserts its
/// own exact test name) — but here the test is named after the lane it
/// proves, not the harness.
fn isolated_test(child: &str, test: &str) -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os(child) {
        return Some(home.into());
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(child, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "isolated terminal-reason test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "subprocess must actually execute the regression: {stdout}"
    );
    None
}
