//! RC1-H6 proofs: the middle column keeps its width at the narrowest window
//! (RC1 round 3, Major 6: window 843×843 pt at 110 % text, middle column
//! about 100 px, "T e l l  A w i l i" one letter per line).

use gpui::{px, size, TestAppContext, VisualTestContext};

use super::*;
use crate::AwiliApp;

const CHILD: &str = "AWILI_RC1H6_TEST_HOME";

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

fn app_window(cx: &mut TestAppContext) -> (gpui::Entity<AwiliApp>, &mut VisualTestContext) {
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, _cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
    });
    window.run_until_parked();
    (view, window)
}

/// The arithmetic: full widths when there is room; the panel gives way
/// first, then the sidebar; floors are kept.
#[test]
fn rc1h6_side_columns_yield_to_the_middle() {
    let center = |w: f32, scale: f32, panel: f32| {
        let c = columns(w, scale, panel);
        w - c.sidebar - c.panel
    };
    assert_eq!(
        columns(1600., 1.0, 340.),
        Columns {
            sidebar: 240.,
            panel: 340.
        }
    );
    // The panel yields first.
    assert_eq!(
        columns(900., 1.0, 340.),
        Columns {
            sidebar: 240.,
            panel: 300.
        }
    );
    // Then the sidebar; at the minimum window the middle keeps its 360.
    let min = crate::app_lifecycle::MIN_WINDOW_SIZE.0;
    assert_eq!(
        columns(min, 1.0, 340.),
        Columns {
            sidebar: 200.,
            panel: 240.
        }
    );
    assert!(center(min, 1.0, 340.) >= CENTER_MIN_W);
    // The round-3 window: 843 pt at 110 %.
    assert!(
        center(843., 1.1, 340.) >= CENTER_MIN_W * 1.1 - 2.,
        "{:?}",
        columns(843., 1.1, 340.)
    );
    // Floors hold even when the window is too narrow for everything.
    let tight = columns(500., 1.0, 340.);
    assert_eq!((tight.sidebar, tight.panel), (SIDEBAR_MIN_W, PANEL_MIN_W));
    // A narrower panel than the floor keeps its own width; no panel is 0.
    assert_eq!(columns(700., 1.0, 200.).panel, 200.);
    assert_eq!(columns(700., 1.0, 0.).panel, 0.);
}

/// The rendered Work layout at the round-3 window (843×843 at 110 %) and at
/// the minimum window (100 %): the middle column keeps its width and every
/// column is on screen.
#[gpui::test]
fn rc1h6_work_middle_column_keeps_its_width(cx: &mut TestAppContext) {
    if !in_isolated_child("layout::tests::rc1h6_work_middle_column_keeps_its_width") {
        return;
    }
    let (view, window) = app_window(cx);
    let min_w = crate::app_lifecycle::MIN_WINDOW_SIZE.0;
    for (w, h, scale) in [(843., 843., 1.1_f32), (min_w, 600., 1.0)] {
        crate::theme::set_ui_scale(scale);
        view.update(window, |app, cx| {
            app.mode = crate::work::Mode::Work;
            app.shell.job_open = true;
            cx.notify();
        });
        window.simulate_resize(size(px(w), px(h)));
        window.run_until_parked();
        let viewport = window.update(|win, _| win.viewport_size());
        assert!(
            window.debug_bounds("awili-sidebar").is_none(),
            "conversations start closed beside the new rail"
        );
        let center = window
            .debug_bounds("awili-center")
            .expect("the middle column");
        let panel = window
            .debug_bounds("awili-work-panel")
            .expect("the Work panel");
        let right = |b: gpui::Bounds<gpui::Pixels>| b.origin.x + b.size.width;
        assert!(
            center.size.width >= px(CENTER_MIN_W * scale - 2.),
            "the middle column keeps its width at {w} pt, {scale}x: {center:?} \
             (panel {panel:?})"
        );
        assert!(
            right(panel) <= viewport.width + px(1.),
            "the Work panel is on screen: {panel:?} in {viewport:?}"
        );
        assert!(panel.size.width >= px(PANEL_MIN_W * scale - 1.));
    }
    crate::theme::set_ui_scale(1.0);
}
