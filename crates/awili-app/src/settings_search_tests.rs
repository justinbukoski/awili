//! SXd proof tests: the real `AwiliApp` in a test window (private HOME per
//! test, the `stream_turn_tests` isolation), driven by clicks, typed input
//! and key presses.
use super::*;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_SXD_TEST_HOME";

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

fn shown(window: &mut VisualTestContext, selector: &str) -> bool {
    window.debug_bounds(sel(selector)).is_some()
}

fn ix(id: &str) -> usize {
    cat::ITEMS.iter().position(|i| i.id == id).unwrap()
}

fn config_text(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> String {
    view.update(window, |app, _| {
        std::fs::read_to_string(app.settings.path()).unwrap_or_default()
    })
}

/// Click into the search box and type `text`.
fn search_for(window: &mut VisualTestContext, text: &str) {
    click(window, "sx-search");
    let keys = text
        .chars()
        .map(|ch| {
            if ch == ' ' {
                "space".to_string()
            } else {
                ch.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    window.simulate_keystrokes(&keys);
    window.run_until_parked();
}

#[gpui::test]
fn sxd_typing_filters_across_pages_and_enter_opens_the_first_hit(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_search::tests::sxd_typing_filters_across_pages_and_enter_opens_the_first_hit",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    assert!(
        !shown(window, "sx-nav-count-6"),
        "no counts while not searching"
    );
    search_for(window, "mic");
    assert_eq!(view.update(window, |app, cx| app.settings_query(cx)), "mic");
    assert!(
        shown(window, "sx-row-voice.vad_threshold"),
        "hits from Voice"
    );
    assert!(shown(window, "sx-row-voice.ptt_key"));
    assert!(!shown(window, "sx-row-memory.recall"), "and nothing else");
    assert!(
        shown(window, "sx-nav-count-6"),
        "the Voice page shows its count"
    );
    let hits = view.update(window, |app, _| app.settings_hits_on(Page::Voice, "mic"));
    assert_eq!(hits, 3);
    window.simulate_keystrokes("return");
    window.run_until_parked();
    let (page, query) = view.update(window, |app, cx| {
        (app.settings.page, app.settings_query(cx))
    });
    assert_eq!(
        (page, query.as_str()),
        (Page::Voice, ""),
        "Enter opens the first hit's page"
    );
}

#[gpui::test]
fn sxd_searchfield_keyboard_edits_query_and_enter_opens_first_hit(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_search::tests::sxd_searchfield_keyboard_edits_query_and_enter_opens_first_hit",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    let nav_focus = view.update(window, |app, _| app.settings_nav_focus.clone());
    window.update(|w, cx| w.focus(&nav_focus, cx));
    window.simulate_keystrokes("/");
    window.run_until_parked();
    window.simulate_keystrokes("m i c");
    window.run_until_parked();

    assert_eq!(view.update(window, |app, cx| app.settings_query(cx)), "mic");
    assert!(shown(window, "sx-row-voice.ptt_key"));
    window.simulate_keystrokes("backspace");
    window.run_until_parked();
    assert_eq!(view.update(window, |app, cx| app.settings_query(cx)), "mi");
    window.simulate_keystrokes("c");
    window.run_until_parked();
    assert_eq!(view.update(window, |app, cx| app.settings_query(cx)), "mic");

    window.simulate_keystrokes("enter");
    window.run_until_parked();
    let (page, query) = view.update(window, |app, cx| {
        (app.settings.page, app.settings_query(cx))
    });
    assert_eq!((page, query.as_str()), (Page::Voice, ""));
}

#[gpui::test]
fn sxd_searchfield_clear_button_updates_parent_query(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_search::tests::sxd_searchfield_clear_button_updates_parent_query",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    search_for(window, "mic");
    assert_eq!(view.update(window, |app, cx| app.settings_query(cx)), "mic");
    click(window, "ui-icon-button-settings-search-clear");
    assert_eq!(view.update(window, |app, cx| app.settings_query(cx)), "");
    assert!(!shown(window, "sx-no-hits"));
}

#[gpui::test]
fn sxd_escape_clears_the_query_before_closing(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_search::tests::sxd_escape_clears_the_query_before_closing") {
        return;
    }
    let (view, window) = settings_window(cx);
    search_for(window, "zzz");
    assert!(shown(window, "sx-no-hits"));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    let (open, query) = view.update(window, |app, cx| {
        (app.settings.open, app.settings_query(cx))
    });
    assert!(open, "the first Escape only clears the search");
    assert_eq!(query, "");
    assert!(!shown(window, "sx-no-hits"));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(
        !view.update(window, |app, _| app.settings.open),
        "the second one closes"
    );
}

#[gpui::test]
fn sxd_change_button_edits_a_text_setting(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_search::tests::sxd_change_button_edits_a_text_setting") {
        return;
    }
    let (view, window) = settings_window(cx);
    let n = ix("gateway.dashboard_url");
    click(window, "sx-nav-8");
    let b = window
        .debug_bounds(sel(&format!("sx-edit-{n}")))
        .expect("Change button");
    assert!(b.size.height >= px(crate::settings_pages::HIT));
    click(window, &format!("sx-edit-{n}"));
    let editing = view.update(window, |app, _| app.settings.editing_item);
    assert_eq!(editing, Some("gateway.dashboard_url"));
    window.simulate_input("http://127.0.0.1:9");
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert!(
        config_text(&view, window).contains("dashboard_url = \"http://127.0.0.1:9\""),
        "{}",
        config_text(&view, window)
    );
    assert!(
        shown(window, &format!("sx-reset-{n}")),
        "the row now shows Reset"
    );
    assert_eq!(
        view.update(window, |app, _| app.settings.editing_item),
        None
    );
}

#[gpui::test]
fn sxd_secret_editor_starts_empty_and_never_shows_the_key(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_search::tests::sxd_secret_editor_starts_empty_and_never_shows_the_key",
    ) {
        return;
    }
    let path = awili_config::Config::config_paths()[0].clone();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "[memory]\nbase_url = \"\"\ntoken = \"s3cret\"\n").unwrap();
    let (view, window) = settings_window(cx);
    let n = ix("memory.token");
    click(window, "sx-nav-8");
    let shown_value = view.update(window, |app, _| {
        let doc = app.settings.doc.clone().unwrap();
        let item = cat::find("memory.token").unwrap();
        cat::display(item, &cat::current(&doc, item))
    });
    assert_eq!(shown_value, "Set (hidden)");
    click(window, &format!("sx-edit-{n}"));
    let text = view.update(window, |app, cx| {
        app.settings_field.read(cx).text().to_string()
    });
    assert_eq!(text, "", "the key is never put into the editor");
    window.simulate_input("n3w-key");
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    let (err, status) = view.update(window, |app, _| {
        (app.settings.error.clone(), app.settings.status.clone())
    });
    assert!(
        config_text(&view, window).contains("token = \"secret:memory\""),
        "{}\n{err:?} {status:?}",
        config_text(&view, window)
    );
    assert!(
        !config_text(&view, window).contains("n3w-key"),
        "the typed key is filed in the secret store, not the config"
    );
    assert_eq!(
        crate::secret_store::store().get("memory").unwrap().as_deref(),
        Some("n3w-key")
    );
    let text = view.update(window, |app, cx| {
        app.settings_field.read(cx).text().to_string()
    });
    assert_eq!(text, "", "and the new key does not stay in the editor");
}

#[gpui::test]
fn sxd_text_size_buttons_change_the_scale(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_search::tests::sxd_text_size_buttons_change_the_scale") {
        return;
    }
    let (_view, window) = settings_window(cx);
    assert!(shown(window, "sx-row-text-size"), "Text size is on General");
    assert_eq!(theme::ui_scale(), 1.0);
    click(window, "sx-zoom-in-0");
    assert!(theme::ui_scale() > 1.0, "{}", theme::ui_scale());
    click(window, "sx-zoom-reset-0");
    assert_eq!(theme::ui_scale(), 1.0);
    click(window, "sx-zoom-out-0");
    assert!(theme::ui_scale() < 1.0);
}

#[gpui::test]
fn sxd_the_change_bar_never_moves_the_page(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_search::tests::sxd_the_change_bar_never_moves_the_page") {
        return;
    }
    let (_view, window) = settings_window(cx);
    click(window, "sx-nav-4");
    let bar = window
        .debug_bounds("sx-status")
        .expect("the bar is there before any change");
    assert!(
        shown(window, "sx-undo-off"),
        "with a disabled Undo in its place"
    );
    let row = window.debug_bounds("sx-row-memory.write").unwrap();
    click(window, &format!("sx-on-{}", ix("memory.recall")));
    assert!(shown(window, "sx-undo-0"));
    assert_eq!(
        window.debug_bounds("sx-status").unwrap(),
        bar,
        "the bar did not move"
    );
    assert_eq!(
        window.debug_bounds("sx-row-memory.write").unwrap().origin,
        row.origin,
        "nor did the rows"
    );
}

/// SXd-FIX1 (Codex P1 on dc9790e): the first Escape while a typed value is
/// being edited only cancels the edit (nothing is saved, Settings stays
/// open); the second Escape closes Settings. Both from a page and from the
/// search results.
#[gpui::test]
fn sxd_fix_escape_cancels_an_edit_before_closing(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_search::tests::sxd_fix_escape_cancels_an_edit_before_closing") {
        return;
    }
    let (view, window) = settings_window(cx);
    let n = ix("gateway.dashboard_url");
    let state = |view: &Entity<AwiliApp>, window: &mut VisualTestContext| {
        view.update(window, |app, cx| {
            (
                app.settings.open,
                app.settings.editing_item,
                app.settings_query(cx),
            )
        })
    };
    // From the Advanced page.
    click(window, "sx-nav-8");
    let before = config_text(&view, window);
    click(window, &format!("sx-edit-{n}"));
    window.simulate_input("http://127.0.0.1:9");
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert_eq!(
        state(&view, window),
        (true, None, String::new()),
        "the first Escape only cancels the edit"
    );
    assert_eq!(config_text(&view, window), before, "nothing was saved");
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!state(&view, window).0, "the second Escape closes Settings");
    // From the search results.
    view.update(window, |app, cx| app.open_settings(cx));
    window.run_until_parked();
    search_for(window, "gateway address");
    click(window, &format!("sx-edit-{n}"));
    window.simulate_input("http://127.0.0.1:9");
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert_eq!(
        state(&view, window),
        (true, None, String::new()),
        "from a search result the first Escape ends the edit and the search"
    );
    assert_eq!(config_text(&view, window), before, "nothing was saved");
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!state(&view, window).0, "the second Escape closes Settings");
    // After a saved edit (Return), Escape still reaches Settings.
    view.update(window, |app, cx| app.open_settings(cx));
    window.run_until_parked();
    click(window, "sx-nav-8");
    click(window, &format!("sx-edit-{n}"));
    window.simulate_input("http://127.0.0.1:9");
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert!(config_text(&view, window).contains("dashboard_url = \"http://127.0.0.1:9\""));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(
        !state(&view, window).0,
        "Escape after Return closes Settings"
    );
}
