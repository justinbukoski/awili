//! rc.3: the first send from an empty page renders without navigating away.

use gpui::{px, size};

use crate::council_run::stub;
use crate::work::Mode;
use crate::AwiliApp;

const CHILD: &str = "AWILI_FIRST_SEND_TEST_HOME";

fn isolated(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

fn quiet(app: &mut AwiliApp, base: &str) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.memory.enabled = false;
    app.work.engine.label = "Gemma 4 26B".into();
    app.work.engine.base_url = format!("{base}/v1");
    app.work.engine.model = "gemma-4-26b-a4b-it-qat-mlx-4bit".into();
}

fn enter(window: &mut gpui::VisualTestContext, text: &str) {
    window.simulate_input(text);
    window.simulate_event(gpui::KeyDownEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    window.run_until_parked();
}

/// Council with no council yet: Enter on the empty page shows the round.
#[gpui::test]
fn first_council_send_renders_without_navigating(cx: &mut gpui::TestAppContext) {
    if !isolated("first_send_render_tests::first_council_send_renders_without_navigating") {
        return;
    }
    cx.executor().allow_parking();
    let (a, _seen) = stub::spawn(|_| {
        let mut r = stub::Reply::text(&["Hello ", "there."]);
        r.gap_ms = 100;
        r
    });
    let (view, window) = cx.add_window_view(|_w, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update_in(window, |app, win, cx| {
        quiet(app, &a);
        app.council_last_group = Some(vec![("in-app".into(), "Gemma 4 26B".into())]);
        app.set_mode(Mode::Council, cx);
        app.focus_composer(win, cx);
    });
    window.run_until_parked();
    assert!(window
        .debug_bounds(crate::council_view::EMPTY_STATE_SELECTOR)
        .is_some());
    enter(window, "First question");
    assert!(
        window
            .debug_bounds(crate::council_view::EMPTY_STATE_SELECTOR)
            .is_none(),
        "still the empty page"
    );
    assert!(
        window
            .debug_bounds(crate::council_view::round_selector(1).leak() as &str)
            .is_some(),
        "no Round 1"
    );
}

/// Work: Enter on the empty page shows the user's message.
#[gpui::test]
fn first_work_send_renders_without_navigating(cx: &mut gpui::TestAppContext) {
    if !isolated("first_send_render_tests::first_work_send_renders_without_navigating") {
        return;
    }
    cx.executor().allow_parking();
    let (a, _seen) = stub::spawn(|_| {
        let mut r = stub::Reply::text(&["Hello ", "there."]);
        r.gap_ms = 100;
        r
    });
    let (view, window) = cx.add_window_view(|_w, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update_in(window, |app, win, cx| {
        quiet(app, &a);
        app.set_mode(Mode::Work, cx);
        app.focus_composer(win, cx);
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-user-bubble-0").is_none());
    enter(window, "First question");
    // A first send probes the real stub's /v1/models and /health off-thread.
    // Parking the deterministic executor does not wait for that HTTP work.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while window.debug_bounds("awili-user-bubble-0").is_none()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
        window.run_until_parked();
    }
    assert!(
        window.debug_bounds("awili-user-bubble-0").is_some(),
        "the message is not on the page"
    );
}
