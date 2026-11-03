//! SXh proof tests: "Ask before changes" (U4's `[work] default_approval_mode`)
//! in Settings: two clicks for Auto, one for Ask, live and undoable.
use super::*;
use crate::settings_catalog as cat;
use awili_work::ApprovalMode;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_SXH_TEST_HOME";

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

fn with_config(text: &str) {
    let path = awili_config::Config::config_paths()[0].clone();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
}

fn settings_window(cx: &mut TestAppContext) -> (Entity<AwiliApp>, &mut VisualTestContext) {
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.open_settings(cx);
    });
    window.run_until_parked();
    (view, window)
}

fn sel(selector: &str) -> &'static str {
    Box::leak(selector.to_string().into_boxed_str())
}

fn click(window: &mut VisualTestContext, selector: &str) {
    let b = window
        .debug_bounds(sel(selector))
        .unwrap_or_else(|| panic!("{selector} is on screen"));
    window.simulate_click(b.center(), Modifiers::default());
    window.run_until_parked();
}

/// Scroll a choice into the real Settings viewport before clicking it.
fn click_revealed(window: &mut VisualTestContext, selector: &str) {
    let mut target = window
        .debug_bounds(sel(selector))
        .unwrap_or_else(|| panic!("{selector} is rendered"));
    let mut viewport = window
        .debug_bounds("sx-page")
        .expect("the Settings page has a scroll viewport");
    for _ in 0..16 {
        if target.top() >= viewport.top() && target.bottom() <= viewport.bottom() {
            break;
        }
        let delta = if target.bottom() > viewport.bottom() {
            viewport.bottom() - target.bottom()
        } else {
            viewport.top() - target.top()
        };
        window.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), delta)),
            touch_phase: gpui::TouchPhase::Moved,
            ..Default::default()
        });
        window.run_until_parked();
        target = window
            .debug_bounds(sel(selector))
            .expect("choice remains rendered");
        viewport = window
            .debug_bounds("sx-page")
            .expect("Settings viewport remains rendered");
    }
    assert!(
        target.top() >= viewport.top() && target.bottom() <= viewport.bottom(),
        "real wheel scrolling reveals {selector}: {target:?} in {viewport:?}"
    );
    window.simulate_click(target.center(), Modifiers::default());
    window.run_until_parked();
}

fn ix(id: &str) -> usize {
    cat::ITEMS.iter().position(|i| i.id == id).unwrap()
}

fn config_text(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> String {
    view.update(window, |app, _| {
        std::fs::read_to_string(app.settings.path()).unwrap_or_default()
    })
}

/// The live global default Work uses for a conversation with no choice.
fn live_mode(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> ApprovalMode {
    view.update(window, |app, _| app.work.ask_modes.default_mode())
}

fn ask_button() -> String {
    format!("sx-choice-{}", ix("work.default_approval_mode") * 16)
}

fn auto_button() -> String {
    format!("sx-choice-{}", ix("work.default_approval_mode") * 16 + 1)
}

#[gpui::test]
fn sxh_auto_needs_two_clicks(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::approval_tests::sxh_auto_needs_two_clicks") {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-2");
    let before = config_text(&view, window);
    click_revealed(window, &auto_button());
    assert_eq!(
        config_text(&view, window),
        before,
        "one click never turns on Auto"
    );
    assert!(!before.contains("default_approval_mode"));
    assert_eq!(live_mode(&view, window), ApprovalMode::AskBeforeChanges);
    assert_eq!(
        view.update(window, |app, _| app.settings.pending_choice),
        Some(("work.default_approval_mode", "auto"))
    );
    click_revealed(window, &auto_button());
    assert!(
        config_text(&view, window).contains("default_approval_mode = \"auto\""),
        "the second click writes it"
    );
    assert_eq!(
        live_mode(&view, window),
        ApprovalMode::Auto,
        "Work uses it at once"
    );
}

#[gpui::test]
fn sxh_ask_applies_at_once_and_is_undoable(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::approval_tests::sxh_ask_applies_at_once_and_is_undoable")
    {
        return;
    }
    with_config("[work]\ndefault_approval_mode = \"auto\"\n");
    let (view, window) = settings_window(cx);
    assert_eq!(live_mode(&view, window), ApprovalMode::Auto);
    click(window, "sx-nav-2");
    click_revealed(window, &ask_button());
    assert!(
        config_text(&view, window).contains("default_approval_mode = \"ask\""),
        "one click is enough for the safer choice"
    );
    assert_eq!(live_mode(&view, window), ApprovalMode::AskBeforeChanges);
    click(window, "sx-undo-0");
    assert!(config_text(&view, window).contains("default_approval_mode = \"auto\""));
    assert_eq!(
        live_mode(&view, window),
        ApprovalMode::Auto,
        "Undo puts Work back too"
    );
}

#[gpui::test]
fn sxh_permission_default_is_on_workshop(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::approval_tests::sxh_permission_default_is_on_workshop") {
        return;
    }
    let (_view, window) = settings_window(cx);
    assert!(
        window
            .debug_bounds("sx-row-work.default_approval_mode")
            .is_none(),
        "the shared approval default is not on General"
    );
    for item in cat::items_on(cat::Page::General, false) {
        assert_ne!(item.id, "work.default_approval_mode");
        assert!(
            window
                .debug_bounds(sel(&format!("sx-row-{}", item.id)))
                .is_some(),
            "{} remains on General",
            item.id
        );
    }
    click(window, "sx-nav-2");
    let permission = window
        .debug_bounds("sx-row-work.default_approval_mode")
        .expect("the shared approval default is on Workshop");
    // Workshop defaults stay task-oriented: both helper defaults precede
    // permissions, and the safer choice remains a full-size target.
    {
        let id = "work.default_executor";
        let row = window
            .debug_bounds(sel(&format!("sx-row-{id}")))
            .expect("helper default is on Workshop");
        assert!(
            row.origin.y < permission.origin.y,
            "{id} precedes permissions"
        );
    }
    click_revealed(window, &ask_button());
    let ask = window.debug_bounds(sel(&ask_button())).expect("Ask button");
    assert!(ask.size.height >= px(crate::settings_pages::HIT));
}
