//! R2a proof tests: a long toast wraps inside the window and its close mark
//! stays on screen (RC1 round 2, Major 1: "the toast is wider than the
//! window; the dismiss X is also off screen"). Each test renders the real
//! `AwiliApp` in a GPUI test window (private HOME subprocess, the U2c-FIX1
//! isolation) and measures the painted toast.

use gpui::{px, size, Modifiers, TestAppContext, VisualTestContext};

use super::*;

const CHILD: &str = "AWILI_R2A_TEST_HOME";

/// The four clipped messages the tester quoted, at full length.
const LONG: [&str; 4] = [
    "Not sent \u{2014} this folder\u{2019}s CLAUDE.md asks to include a file from outside the \
     folder, and Awili only reads files inside it. Your message is still in the box. Remove \
     that line from CLAUDE.md or choose another folder, then press Enter.",
    "Saved \"What instructions were you given about...\" to \
     ~/Downloads/what-instructions-were-you-given-about-2026-10-01.md \u{2014} also copied to \
     the clipboard",
    "GLM's plan mode doesn't stop its plugins from changing files, so Awili can't ask before \
     changes with GLM. Use Claude or Codex when you want to be asked first.",
    "Nothing was changed. Your request is back in the box \u{2014} edit it or press Enter to \
     ask again.",
];

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
        out.status.success(),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "the subprocess must actually run the test: {stdout}"
    );
    false
}

fn app_window(cx: &mut TestAppContext) -> (gpui::Entity<AwiliApp>, &mut VisualTestContext) {
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, _cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
    });
    window.run_until_parked();
    (view, window)
}

/// Every long toast, at a laptop-sized window and at a narrow one: the box,
/// its text and its close mark all sit inside the window, and the text wraps
/// to the box (it is never wider than the box).
#[gpui::test]
fn r2a_long_toast_wraps_inside_the_window(cx: &mut TestAppContext) {
    if !in_isolated_child("toast_tests::r2a_long_toast_wraps_inside_the_window") {
        return;
    }
    let (view, window) = app_window(cx);
    for width in [1280.0_f32, 700.0] {
        window.simulate_resize(size(px(width), px(800.)));
        window.run_until_parked();
        // One line of toast text, for the wrap check below.
        view.update(window, |app, cx| app.show_sticky_toast("Copied", cx));
        window.run_until_parked();
        let line = window
            .debug_bounds("awili-toast-text")
            .expect("a short toast")
            .size
            .height;
        for msg in LONG {
            view.update(window, |app, cx| app.show_sticky_toast(msg, cx));
            window.run_until_parked();
            let viewport = window.update(|w, _| w.viewport_size());
            let toast = window.debug_bounds("awili-toast").expect("the toast");
            let text = window.debug_bounds("awili-toast-text").expect("its text");
            let close = window
                .debug_bounds("awili-toast-close")
                .expect("its close mark");
            let right = |b: gpui::Bounds<gpui::Pixels>| b.origin.x + b.size.width;
            assert!(
                toast.origin.x >= px(0.) && right(toast) <= viewport.width,
                "the toast fits the {width} pt window: {toast:?} in {viewport:?}"
            );
            assert!(
                right(text) <= right(toast) && text.origin.x >= toast.origin.x,
                "the text wraps inside the toast: {text:?} in {toast:?} ({msg})"
            );
            assert!(
                right(close) <= right(toast) && right(close) <= viewport.width,
                "the close mark is on screen: {close:?} in {toast:?}"
            );
            assert!(
                text.size.height > line,
                "a long message wraps onto more than one line: {text:?} ({msg})"
            );
        }
    }
}

/// The close mark is a real target: a click on it closes a sticky toast.
#[gpui::test]
fn r2a_click_on_the_close_mark_closes_the_toast(cx: &mut TestAppContext) {
    if !in_isolated_child("toast_tests::r2a_click_on_the_close_mark_closes_the_toast") {
        return;
    }
    let (view, window) = app_window(cx);
    window.simulate_resize(size(px(700.), px(800.)));
    view.update(window, |app, cx| app.show_sticky_toast(LONG[0], cx));
    window.run_until_parked();
    let close = window
        .debug_bounds("awili-toast-close")
        .expect("its close mark");
    window.simulate_click(close.center(), Modifiers::default());
    window.run_until_parked();
    assert!(
        view.update(window, |app, _| app.toast.msg.is_none()),
        "the click on the close mark closed the toast"
    );
}
