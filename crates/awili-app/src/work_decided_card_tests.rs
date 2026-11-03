//! RC2-R4 (D-R1 M10c, RC1 round 1: "the layout under the card jumps after
//! approval (the Save as project button moves to where Make the changes
//! was)"). The plan card left the Work panel in R1g, but an approval card
//! for a step still sits above the Projects card there: answering it took it
//! away and moved everything below up by its height, so "Save as project"
//! landed under the pointer. Now the answered card keeps its place, inert,
//! with the answer as its heading, until the next card or run.

use super::*;
use awili_gateway::run_events::ApprovalDecision;
use gpui::{px, size, TestAppContext};
use std::sync::mpsc::channel;

const CHILD: &str = "AWILI_RC2R4_DECIDED_HOME";

/// Run `test` in a subprocess with a private HOME. `true` inside the child.
fn in_isolated_child(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

#[gpui::test]
fn rc2r4_answering_a_card_moves_nothing_under_the_pointer(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "work::decided_card_tests::rc2r4_answering_a_card_moves_nothing_under_the_pointer",
    ) {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(1000.)));
    let (tx, rx) = channel();
    app.update(window, |a, cx| {
        a.onboarding = crate::onboarding::Onboarding::existing();
        a.set_mode(Mode::Work, cx);
        a.shell.job_open = true;
        a.shell.job_tab = crate::job_panel::JobTab::Context;
        a.work.executor = WorkExecutor::InApp;
        a.work.running = true; // answered card remains pinned only during a live run
        a.work.worker_identity =
            Some(crate::work_run_identity::WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None });
        a.work.gateway_cmd_tx = Some(tx);
        a.work.conv_id = Some(a.sidebar.active_id().to_string());
        // The Job footer stays pinned while the inline approval is answered.
        a.work.folder = Some(folder.path().to_path_buf());
        a.work.panel.approval = Some(ApprovalCard {
            approval_id: "a1".into(),
            kind: "shell".into(),
            prompt: "rm build/".into(),
            risk: awili_gateway::events::ApprovalRisk::Mutating,
            delivery: Default::default(),
        });
        cx.notify();
    });
    window.run_until_parked();
    let folder_control = |window: &mut gpui::VisualTestContext| {
        window
            .debug_bounds("ui-button-work-pick")
            .expect("the pinned Choose folder button")
    };
    let before = folder_control(window);
    assert!(
        window.debug_bounds("work-allow-once").is_some(),
        "the card is up"
    );

    app.update(window, |a, cx| {
        a.resolve_work_approval(ApprovalDecision::Approve, cx)
    });
    window.run_until_parked();
    assert!(
        matches!(rx.try_recv(), Ok(GatewayCmd::Approval { .. })),
        "the verdict went to the engine"
    );
    app.read_with(window, |a, _| {
        assert!(a.work.panel.approval.is_none(), "the card is answered");
        assert_eq!(
            a.work.panel.decided.as_ref().map(|d| d.choice),
            Some("once")
        );
    });
    assert!(window.debug_bounds("work-allow-once").is_none());
    assert!(
        window.debug_bounds("work-decided-card").is_some(),
        "the answered card keeps its place"
    );
    assert_eq!(
        folder_control(window).origin,
        before.origin,
        "Choose folder did not move under the pointer"
    );
}
