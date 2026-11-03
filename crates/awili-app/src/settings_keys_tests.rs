//! SXi proof tests: the Settings keyboard, driven with real keystrokes in a
//! GPUI window (sidebar arrows, Tab order, ←/→, Space/Enter, ⌘F, Escape).
use super::*;
use crate::settings_catalog::{self as cat, Page};
use crate::settings_focus::FocusStop;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_SXI_TEST_HOME";

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

fn keys(window: &mut VisualTestContext, keys: &str) {
    window.simulate_keystrokes(&crate::composer_keys::plat_keys(keys));
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

/// Give the Settings page body keyboard focus (as a click on it would).
fn focus_page(view: &Entity<AwiliApp>, window: &mut VisualTestContext) {
    let h = view.update(window, |app, _| app.settings_nav_focus.clone());
    window.update(|w, cx| w.focus(&h, cx));
    window.run_until_parked();
}

fn page(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> Page {
    view.update(window, |app, _| app.settings.page)
}

fn stop(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> Option<FocusStop> {
    view.update(window, |app, _| app.settings_stop)
}

/// The keyboard ring sits on the button `selector`.
fn ring_on(window: &mut VisualTestContext, selector: &str) -> bool {
    match (
        window.debug_bounds("sx-keyboard-focus"),
        window.debug_bounds(sel(selector)),
    ) {
        (Some(mark), Some(button)) => button.contains(&mark.origin),
        _ => false,
    }
}

#[gpui::test]
fn sxi_arrows_move_pages(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::keys_tests::sxi_arrows_move_pages") {
        return;
    }
    let (view, window) = settings_window(cx);
    focus_page(&view, window);
    assert_eq!(page(&view, window), Page::General);
    keys(window, "down");
    assert_eq!(
        page(&view, window),
        Page::Appearance,
        "Appearance follows General"
    );
    keys(window, "down");
    assert_eq!(
        page(&view, window),
        Page::Workshop,
        "Workshop follows Appearance"
    );
    assert!(
        window
            .debug_bounds("sx-row-work.default_executor")
            .is_some(),
        "the page opens at once"
    );
    keys(window, "up");
    assert_eq!(page(&view, window), Page::Appearance);
    keys(window, "down");
    assert_eq!(page(&view, window), Page::Workshop);
    keys(window, "end");
    assert_eq!(page(&view, window), Page::Allowed);
    keys(window, "home");
    assert_eq!(page(&view, window), Page::General);
    keys(window, "up");
    assert_eq!(page(&view, window), Page::General, "the top stays put");
}

#[gpui::test]
fn sxi_tab_then_space_toggles(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::keys_tests::sxi_tab_then_space_toggles") {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-4");
    focus_page(&view, window);
    let n = ix("memory.recall");
    keys(window, "tab");
    assert_eq!(stop(&view, window), Some(FocusStop::On("memory.recall")));
    assert!(
        ring_on(window, &format!("sx-on-{n}")),
        "the ring is on Use my memory's On"
    );
    keys(window, "space");
    assert!(
        config_text(&view, window).contains("recall = true"),
        "Space presses it"
    );
    keys(window, "right");
    assert!(ring_on(window, &format!("sx-off-{n}")), "→ moves to Off");
    // The changed row's Reset joins the order right after Off.
    keys(window, "tab");
    assert_eq!(stop(&view, window), Some(FocusStop::Reset("memory.recall")));
    keys(window, "enter");
    assert!(
        !config_text(&view, window).contains("recall = true"),
        "Enter presses Reset"
    );
    // Shift-Tab walks back and leaves the page at its start.
    keys(
        window,
        "shift-tab shift-tab shift-tab shift-tab shift-tab shift-tab",
    );
    assert_eq!(stop(&view, window), None);
    assert!(view.update(window, |app, _| app.settings_in_sidebar));
}

#[gpui::test]
fn sxi_tab_scrolls_the_focused_row_into_view(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::keys_tests::sxi_tab_scrolls_the_focused_row_into_view") {
        return;
    }
    let (view, window) = settings_window(cx);
    // A short window, so the page's last row starts below the fold.
    window.simulate_resize(gpui::size(px(900.), px(460.)));
    window.run_until_parked();
    // To Notifications by keyboard (its sidebar row may be below the fold).
    focus_page(&view, window);
    keys(window, "down down down down down down");
    assert_eq!(page(&view, window), Page::Notifications);
    let page_box = window.debug_bounds("sx-page").expect("the page");
    let inc = format!("sx-inc-{}", ix("notifications.min_run_secs"));
    // Out of view: not painted at all, or painted outside the page.
    let before = window.debug_bounds(sel(&inc));
    assert!(
        before.is_none_or(|b| !page_box.contains(&b.center())),
        "precondition: the last row is out of view: {before:?} vs {page_box:?}"
    );
    let last = view.update(window, |app, _| {
        crate::settings_focus::order(
            Page::Notifications,
            app.settings.doc.as_ref().unwrap(),
            false,
        )
        .len()
    });
    for _ in 0..last {
        keys(window, "tab");
    }
    assert_eq!(
        stop(&view, window),
        Some(FocusStop::Inc("notifications.min_run_secs")),
        "Tab reached the page's last stop"
    );
    let page_box = window.debug_bounds("sx-page").expect("the page");
    let mark = window
        .debug_bounds("sx-keyboard-focus")
        .expect("the ring is drawn");
    assert!(
        page_box.contains(&mark.origin),
        "the focused row is scrolled into view: {mark:?} in {page_box:?}"
    );
}

#[gpui::test]
fn sxi_cmd_f_focuses_search(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::keys_tests::sxi_cmd_f_focuses_search") {
        return;
    }
    let (view, window) = settings_window(cx);
    focus_page(&view, window);
    keys(window, "cmd-f");
    keys(window, "m i c");
    assert_eq!(view.update(window, |app, cx| app.settings_query(cx)), "mic");
    assert!(
        window.debug_bounds("sx-row-voice.ptt_key").is_some(),
        "the results show"
    );

    // Repeating Cmd-F while the non-empty Settings field owns focus must
    // keep that field active instead of opening conversation search.
    keys(window, "cmd-f");
    assert!(
        !view.read_with(window, |app, _| app.search.open),
        "Cmd-F in Settings does not open conversation search"
    );
    let settings_search_focus = view.read_with(window, |app, _| app.settings_search_focus.clone());
    assert!(window.update(|w, _| settings_search_focus.is_focused(w)));
    keys(window, "d e f");
    assert_eq!(
        view.update(window, |app, cx| app.settings_query(cx)),
        "micdef",
        "typing after repeated Cmd-F still edits Settings search"
    );
    assert!(window.debug_bounds("sx-no-hits").is_some());
}

#[gpui::test]
fn sxi_escape_still_closes(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::keys_tests::sxi_escape_still_closes") {
        return;
    }
    let (view, window) = settings_window(cx);
    focus_page(&view, window);
    keys(window, "tab tab");
    assert!(stop(&view, window).is_some());
    keys(window, "escape");
    assert!(
        !view.update(window, |app, _| app.settings.open),
        "Escape closes Settings"
    );
}

#[gpui::test]
fn v2_settings_done_returns_typing_and_job_shortcut_without_pointer(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::keys_tests::v2_settings_done_returns_typing_and_job_shortcut_without_pointer") {
        return;
    }
    let (view, window) = settings_window(cx);
    view.update(window, |app, cx| {
        app.set_mode(crate::work::Mode::Work, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Retained draft ", cx));
    });
    window.run_until_parked();
    focus_page(&view, window);
    click(window, "sx-done-0");
    assert!(!view.read_with(window, |app, _| app.settings.open));
    window.simulate_input("after Done");
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        "Retained draft after Done"
    );
    keys(window, "cmd-j");
    assert!(
        view.read_with(window, |app, _| app.shell.job_open),
        "Cmd-J must work without a pointer refocus"
    );
}
