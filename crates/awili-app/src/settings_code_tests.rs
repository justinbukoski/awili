//! SXf proof tests: the Workshop page's helper cards, the live Work helper
//! choice, and the old keyboard model kept to the Models & Fleet form.
use super::*;
use crate::settings_catalog::{self as cat, Page};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_SXF_TEST_HOME";

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

fn bounds_inside(inner: gpui::Bounds<gpui::Pixels>, outer: gpui::Bounds<gpui::Pixels>) -> bool {
    inner.left() >= outer.left()
        && inner.right() <= outer.right()
        && inner.top() >= outer.top()
        && inner.bottom() <= outer.bottom()
}

/// Reveal a real settings face with the page's wheel scroller.
/// `debug_bounds` keeps returning layout bounds for clipped children.
fn reveal_settings_face(
    window: &mut VisualTestContext,
    selector: &str,
) -> gpui::Bounds<gpui::Pixels> {
    let mut target = window
        .debug_bounds(sel(selector))
        .unwrap_or_else(|| panic!("{selector} is on the Workshop page"));
    let mut viewport = window
        .debug_bounds("sx-page")
        .expect("the settings page has a scroll viewport");
    assert!(
        target.left() >= viewport.left() && target.right() <= viewport.right(),
        "the action face stays within the settings page width: {target:?} in {viewport:?}"
    );

    for _ in 0..12 {
        if bounds_inside(target, viewport) {
            break;
        }
        let delta = if target.bottom() > viewport.bottom() {
            viewport.bottom() - target.bottom()
        } else if target.top() < viewport.top() {
            viewport.top() - target.top()
        } else {
            panic!("cannot vertically reveal {selector}: {target:?} in {viewport:?}");
        };
        window.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), delta)),
            touch_phase: gpui::TouchPhase::Moved,
            ..Default::default()
        });
        window.run_until_parked();
        target = window
            .debug_bounds(sel(selector))
            .expect("action remains rendered");
        viewport = window
            .debug_bounds("sx-page")
            .expect("page viewport remains rendered");
    }
    assert!(
        bounds_inside(target, viewport),
        "wheel reveal must fully expose {selector}: {target:?} in {viewport:?}"
    );
    target
}

/// Scrolls the page to the real action face before clicking its visible center.
fn click_revealed_settings_action(window: &mut VisualTestContext, selector: &str) {
    let target = reveal_settings_face(window, selector);
    window.simulate_click(target.center(), Modifiers::default());
    window.run_until_parked();
}

fn shown(window: &mut VisualTestContext, selector: &str) -> bool {
    window.debug_bounds(sel(selector)).is_some()
}

/// Read words laid out in this isolated test window's current frame. The
/// test-only collector is thread-local and append-only, so clear it before
/// forcing this window to redraw, then query the refreshed wrapper bounds.
fn painted_text_matching(
    window: &mut VisualTestContext,
    selector: &str,
    matches: impl Fn(&str) -> bool,
) -> bool {
    crate::ui::text::DRAWN_TEXTS.with(|drawn| drawn.borrow_mut().clear());
    window.update(|window, _cx| window.refresh());
    window.run_until_parked();
    let Some(bounds) = window.debug_bounds(sel(selector)) else {
        return false;
    };
    crate::ui::text::DRAWN_TEXTS.with(|drawn| {
        drawn.borrow().iter().any(|layout| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (layout.text(), layout.bounds())
            }))
            .ok()
            .is_some_and(|(text, text_bounds)| {
                matches(&text) && bounds.contains(&text_bounds.center())
            })
        })
    })
}

fn painted_text(window: &mut VisualTestContext, selector: &str, expected: &str) -> bool {
    painted_text_matching(window, selector, |text| text == expected)
}

fn painted_text_contains(window: &mut VisualTestContext, selector: &str, fragment: &str) -> bool {
    painted_text_matching(window, selector, |text| text.contains(fragment))
}

fn ix(id: &str) -> usize {
    cat::ITEMS.iter().position(|i| i.id == id).unwrap()
}

/// Give the Settings page body keyboard focus.
fn focus_page(view: &Entity<AwiliApp>, window: &mut VisualTestContext) {
    let h = view.update(window, |app, _| app.settings_nav_focus.clone());
    window.update(|w, cx| w.focus(&h, cx));
    window.run_until_parked();
}

#[test]
fn sxf_catalog_lists_workshop_defaults_once() {
    let on = |p| {
        cat::items_on(p, false)
            .iter()
            .any(|i| i.id == "work.default_executor")
    };
    assert!(on(Page::Workshop));
    assert!(!on(Page::General));
    assert_eq!(
        cat::home_page(cat::find("work.default_executor").unwrap()),
        Page::Workshop
    );
}

#[gpui::test]
fn sxf_code_page_lists_four_helpers_with_status(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::sxf_code_page_lists_four_helpers_with_status") {
        return;
    }
    let (_view, window) = settings_window(cx);
    click(window, "sx-nav-2");
    for i in 0..harness_setup::RECIPES.len() {
        for id in ["sx-guide", "sx-check"] {
            let b = window
                .debug_bounds(sel(&format!("{id}-{i}")))
                .expect("a button per helper");
            assert!(b.size.height >= px(crate::settings_pages::HIT));
        }
        let ok = shown(window, &format!("sx-helper-ok-{i}"));
        let missing = shown(window, &format!("sx-helper-missing-{i}"));
        assert!(
            ok != missing,
            "helper {i} says exactly one of Installed / Not found"
        );
    }
    assert!(
        shown(window, "sx-row-work.default_executor"),
        "the Work helper choice is here too"
    );
}

#[gpui::test]
fn sxf_helper_cards_and_actions_stay_inside_code_page(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::tests::sxf_helper_cards_and_actions_stay_inside_code_page",
    ) {
        return;
    }
    let (_view, window) = settings_window(cx);
    click(window, "sx-nav-2");

    for (scale, width, height) in [(1.0_f32, 1280.0_f32, 800.0_f32), (1.5, 863.0, 512.0)] {
        crate::theme::set_ui_scale(scale);
        window.simulate_resize(gpui::size(px(width), px(height)));
        window.run_until_parked();

        let page = window
            .debug_bounds("sx-page")
            .expect("Workshop page viewport");
        for i in 0..harness_setup::RECIPES.len() {
            let card = window
                .debug_bounds(sel(&format!("ui-card-sx-helper-card-{i}")))
                .unwrap_or_else(|| panic!("helper card {i} is rendered"));
            assert!(
                card.left() >= page.left() && card.right() <= page.right(),
                "helper card {i} stays within page width at {width}×{height}/{scale}: {card:?} / {page:?}"
            );

            for action in ["sx-guide", "sx-check"] {
                let face = window
                    .debug_bounds(sel(&format!("{action}-{i}")))
                    .unwrap_or_else(|| panic!("{action} for helper {i} is rendered"));
                assert!(
                    face.size.height >= crate::theme::target(),
                    "{action} for helper {i} keeps the shared target at {scale}: {face:?}"
                );
                assert!(
                    face.left() >= card.left()
                        && face.right() <= card.right()
                        && face.top() >= card.top()
                        && face.bottom() <= card.bottom(),
                    "{action} for helper {i} stays inside its card at {width}×{height}/{scale}: {face:?} / {card:?}"
                );
            }
        }
    }
    crate::theme::set_ui_scale(1.0);
}

#[cfg(unix)]
#[gpui::test]
fn sxf_check_again_sees_a_new_install(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::sxf_check_again_sees_a_new_install") {
        return;
    }
    let bin = std::path::PathBuf::from(std::env::var_os(CHILD).unwrap()).join("bin/claude");
    with_config(&format!(
        "[work.agents]\nclaude_bin = \"{}\"\n",
        bin.display()
    ));
    let (_view, window) = settings_window(cx);
    click(window, "sx-nav-2");
    assert!(shown(window, "sx-helper-missing-0"));
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, "#!/bin/sh\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    click_revealed_settings_action(window, "sx-check-0");
    assert!(shown(window, "sx-helper-ok-0"), "Check again finds it");
}

#[gpui::test]
fn sxf_code_page_sets_the_work_helper_live(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::sxf_code_page_sets_the_work_helper_live") {
        return;
    }
    let (view, window) = settings_window(cx);
    let before = view.update(window, |app, _| {
        (app.work.executor.clone(), app.work.last_pick.clone())
    });
    click(window, "sx-nav-2");
    click(
        window,
        &format!("sx-choice-{}", ix("work.default_executor") * 16 + 1),
    );
    let (text, executor, last_pick, preference) = view.update(window, |app, _| {
        (
            std::fs::read_to_string(app.settings.path()).unwrap_or_default(),
            app.work.executor.clone(),
            app.work.last_pick.clone(),
            app.work.default_helper.clone(),
        )
    });
    assert!(
        text.contains("default_executor = \"agent:codex\""),
        "{text}"
    );
    assert_eq!(
        executor, before.0,
        "saving a default leaves the active thread alone"
    );
    assert_eq!(
        last_pick, before.1,
        "saving a default leaves the last pick alone"
    );
    assert_eq!(preference.as_deref(), Some("agent:codex"));
}

#[gpui::test]
fn sxf_clicking_selected_claude_family_preserves_pinned_model(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::tests::sxf_clicking_selected_claude_family_preserves_pinned_model",
    ) {
        return;
    }
    with_config("[work]\ndefault_executor = \"agent:claude:haiku\"\n");
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-2");
    click(
        window,
        &format!("sx-choice-{}", ix("work.default_executor") * 16),
    );
    let text = view.update(window, |app, _| {
        std::fs::read_to_string(app.settings.path()).unwrap_or_default()
    });
    assert!(
        text.contains("default_executor = \"agent:claude:haiku\""),
        "clicking the selected Claude family preserves the pinned model: {text}"
    );
}

#[gpui::test]
fn sxf_workshop_defaults_precede_helper_setup(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::sxf_workshop_defaults_precede_helper_setup") {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-2");
    assert_eq!(
        view.update(window, |app, _| app.settings.page),
        Page::Workshop
    );
    let defaults = window
        .debug_bounds("sx-row-work.default_executor")
        .expect("new project helper default");
    let helper = window
        .debug_bounds(sel("ui-card-sx-helper-card-0"))
        .expect("helper setup card follows defaults");
    assert!(
        defaults.top() < helper.top(),
        "new-thread choices precede helper setup: {defaults:?} / {helper:?}"
    );
    assert!(
        !shown(window, "sx-to-code-0"),
        "the obsolete cross-page link is gone"
    );
}

#[gpui::test]
fn sxf_old_keyboard_model_stays_on_the_models_form(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::sxf_old_keyboard_model_stays_on_the_models_form") {
        return;
    }
    let (view, window) = settings_window(cx);
    // Another page: Space must not run the old form's (invisible) Add.
    click(window, "sx-nav-4");
    focus_page(&view, window);
    window.simulate_keystrokes("space");
    window.run_until_parked();
    let cards = view.update(window, |app, _| {
        app.settings.draft.as_ref().unwrap().providers.len()
    });
    assert_eq!(cards, 0, "Space on Memory added nothing");
    // The Models form: Tab never lands on the moved helper cards.
    click(window, "sx-nav-3");
    focus_page(&view, window);
    for _ in 0..12 {
        window.simulate_keystrokes("tab");
        window.run_until_parked();
        let at = view.update(window, |app, _| app.settings_nav.selected());
        assert!(
            !matches!(
                at,
                settings_nav::Action::HarnessRecipe(_) | settings_nav::Action::HarnessDocs(_)
            ),
            "{at:?}"
        );
    }
}

/// Choosing Codex changes only the preference for new project threads; Undo
/// restores the configured Claude preference without changing the active thread.
#[gpui::test]
fn sxf_fix_undo_puts_the_work_helper_back(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::sxf_fix_undo_puts_the_work_helper_back") {
        return;
    }
    with_config("[work]\ndefault_executor = \"agent:claude\"\n");
    let (view, window) = settings_window(cx);
    let (executor_before, pick_before) = view.update(window, |app, _| {
        (app.work.executor.clone(), app.work.last_pick.clone())
    });
    click(window, "sx-nav-2");
    click(
        window,
        &format!("sx-choice-{}", ix("work.default_executor") * 16 + 1),
    );
    assert_eq!(
        view.update(window, |app, _| app.work.executor.clone()),
        executor_before
    );
    assert_eq!(
        view.update(window, |app, _| app.work.last_pick.clone()),
        pick_before
    );
    assert_eq!(
        view.update(window, |app, _| app.work.default_helper.clone())
            .as_deref(),
        Some("agent:codex")
    );
    click(window, "sx-undo-0");
    let text = view.update(window, |app, _| {
        std::fs::read_to_string(app.settings.path()).unwrap_or_default()
    });
    assert!(
        text.contains("default_executor = \"agent:claude\""),
        "{text}"
    );
    assert_eq!(
        view.update(window, |app, _| app.work.executor.clone()),
        executor_before
    );
    assert_eq!(
        view.update(window, |app, _| app.work.last_pick.clone()),
        pick_before
    );
    assert_eq!(
        view.update(window, |app, _| app.work.default_helper.clone())
            .as_deref(),
        Some("agent:claude")
    );
}

/// Reset clears only the starting-helper preference; it leaves the current
/// thread helper and remembered pick intact.
#[gpui::test]
fn sxf_fix_reset_puts_the_work_helper_back(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::sxf_fix_reset_puts_the_work_helper_back") {
        return;
    }
    with_config("[work]\ndefault_executor = \"agent:codex\"\n");
    let (view, window) = settings_window(cx);
    let (executor_before, pick_before) = view.update(window, |app, _| {
        (app.work.executor.clone(), app.work.last_pick.clone())
    });
    click(window, "sx-nav-2");
    click(window, &format!("sx-reset-{}", ix("work.default_executor")));
    let text = view.update(window, |app, _| {
        std::fs::read_to_string(app.settings.path()).unwrap_or_default()
    });
    assert!(!text.contains("default_executor"), "{text}");
    assert_eq!(
        view.update(window, |app, _| app.work.executor.clone()),
        executor_before
    );
    assert_eq!(
        view.update(window, |app, _| app.work.last_pick.clone()),
        pick_before
    );
    assert_eq!(
        view.update(window, |app, _| app.work.default_helper.clone()),
        None
    );
}

/// RC2-R57 (usability RC2 round 4: Settings > Code showed Claude Code
/// "✓ Installed" while its sign-in had expired): an installed CLI that says
/// it is signed out (`claude auth status`, here a stand-in) reads "Installed,
/// but signed out" with how to sign in; after signing in, Check again says
/// so.
#[gpui::test]
fn rc2r57_the_code_page_says_when_claude_is_signed_out(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::tests::rc2r57_the_code_page_says_when_claude_is_signed_out",
    ) {
        return;
    }
    let bin = std::path::PathBuf::from(std::env::var_os(CHILD).unwrap()).join("bin/claude");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    // Never `fs::write` an executable a test will exec (ETXTBSY, RC1-G2).
    let write = |logged: bool| {
        crate::exec_fixture::write_exec_fixture(
            &bin,
            &format!(
                "#!/bin/sh\n[ \"$1 $2\" = \"auth status\" ] || exit 2\necho '{{\"loggedIn\": {logged}}}'\nexit {}\n",
                if logged { 0 } else { 1 }
            ),
            0o755,
        );
    };
    write(false);
    with_config(&format!(
        "[work.agents]\nclaude_bin = \"{}\"\n",
        bin.display()
    ));
    let (_view, window) = settings_window(cx);
    click(window, "sx-nav-2");
    assert!(shown(window, "sx-helper-ok-0"), "it is installed");
    assert!(
        shown(window, "sx-helper-signed-out-0"),
        "and it says it is signed out"
    );
    write(true);
    click_revealed_settings_action(window, "sx-check-0");
    assert!(shown(window, "sx-helper-ok-0"));
    assert!(
        !shown(window, "sx-helper-signed-out-0"),
        "Check again sees the sign-in"
    );
}

/// RC2-R511 (usability RC2 round 4, Minor: the Voice settings said "Applies
/// after you quit Awili and open it again" four times): a page whose every
/// row needs a restart says so once, at its top, and on no row.
#[gpui::test]
fn rc2r511_the_voice_page_says_restart_once(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::rc2r511_the_voice_page_says_restart_once") {
        return;
    }
    let (_view, window) = settings_window(cx);
    click(window, "sx-nav-7");
    assert!(
        shown(window, "rc2r511-restart-note"),
        "said once, at the top"
    );
    for id in [
        "voice.auto_speak",
        "voice.ptt_key",
        "voice.vad_threshold",
        "voice.vad_silence_windows",
    ] {
        assert!(shown(window, &format!("sx-row-{id}")), "{id} is listed");
        assert!(
            !shown(window, &format!("sx-restart-{id}")),
            "{id} repeats it"
        );
    }
}

/// RC2-R511 (usability RC2 round 4, Minor: "Default Work helper" read "On this
/// Mac" while new Work conversations opened with Codex, picked last): the
/// row scopes the choice to new project threads and explains the configured
/// preference/last-helper fallback.
#[test]
fn rc2r511_the_work_helper_row_says_the_last_pick_carries_on() {
    let item = cat::find("work.default_executor").unwrap();
    assert!(
        item.help.contains("new project threads")
            && item.help.contains("saved preference")
            && item.help.contains("last helper is used"),
        "{}",
        item.help
    );
    assert_eq!(item.group, "New project threads");
}

/// RC2-R511 (usability RC2 round 4, Minor: the memory service's address and
/// key read "Not set" while Work said "5 notes recalled"): an empty address
/// says what is used instead, and an empty key says the standard file is.
#[test]
fn rc2r511_empty_memory_rows_say_what_is_used() {
    let blank = serde_json::Value::String(String::new());
    let url = cat::find("memory.base_url").unwrap();
    assert_eq!(
        cat::display(url, &blank),
        "Off \u{2014} your agents' memory (~/.config/foreman) is used"
    );
    let key = cat::find("memory.token").unwrap();
    assert_eq!(cat::display(key, &blank), "The standard key file");
    let other = cat::find("gateway.dashboard_url").unwrap();
    assert_eq!(cat::display(other, &blank), "Not set");
}

/// RC2-R5M4 (slice review befb867d, P2): Settings > Code with a Claude Code
/// whose status command hangs. The page is drawn at once, saying the
/// sign-in is being checked; the check runs off the UI thread and is given
/// up on at its deadline, and the card then claims neither way.
#[gpui::test]
fn rc2r5m4_a_hung_status_command_never_holds_the_code_page(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::tests::rc2r5m4_a_hung_status_command_never_holds_the_code_page",
    ) {
        return;
    }
    // Its point is the hang: the short test deadline, not the answer budget.
    crate::agent_signin::use_hung_cli_deadline();
    let bin = std::path::PathBuf::from(std::env::var_os(CHILD).unwrap()).join("bin/claude");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    // Never `fs::write` an executable a test will exec (ETXTBSY, RC1-G2).
    crate::exec_fixture::write_exec_fixture(&bin, "#!/bin/sh\nexec sleep 30\n", 0o755);
    with_config(&format!(
        "[work.agents]\nclaude_bin = \"{}\"\n",
        bin.display()
    ));
    let path = bin.display().to_string();
    let engine = awili_code::CodeEngine::Claude;
    // A check already on its way: the page says so and starts no other.
    assert!(crate::agent_signin::begin_sign_in_check(engine, &path));
    let (_view, window) = settings_window(cx);
    let started = std::time::Instant::now();
    click(window, "sx-nav-2");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the page is drawn without waiting on the CLI"
    );
    assert!(shown(window, "sx-helper-ok-0"), "it is installed");
    assert!(
        shown(window, "sx-helper-checking-0"),
        "and its sign-in is being checked"
    );
    assert!(!shown(window, "sx-helper-signed-out-0"));
    // That check ends; Check again asks the hung CLI itself.
    crate::agent_signin::end_sign_in_check(engine, &path);
    let started = std::time::Instant::now();
    click_revealed_settings_action(window, "sx-check-0");
    let took = started.elapsed();
    assert!(
        // Timely, not just eventual: at most two checks of the test
        // deadline each (the click may ask twice), with room for the kill.
        took < crate::agent_signin::TEST_STATUS_DEADLINE * 2 + std::time::Duration::from_secs(4),
        "given up at the deadline, not after the 30 s hang: {took:?}"
    );
    assert!(shown(window, "sx-helper-ok-0"));
    assert!(
        !shown(window, "sx-helper-checking-0"),
        "the check is over: the card claims neither way"
    );
    assert!(!shown(window, "sx-helper-signed-out-0"));
    assert_eq!(
        crate::agent_signin::known_sign_in(engine, &path),
        Some(crate::agent_signin::SignIn::Unknown)
    );
}

/// RC2-R65 (usability RC2 round 5, Minor 8: "Check again" on Settings ›
/// Code showed no feedback): pressing it shows when it checked, under that
/// helper, and a second press moves the time on.
#[gpui::test]
fn rc2r65_check_again_says_when_it_checked(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::rc2r65_check_again_says_when_it_checked") {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-2");
    assert!(
        !shown(window, "ui-text-rc2r65-code-checked-1"),
        "nothing before a press"
    );
    click_revealed_settings_action(window, "sx-check-1");
    let at = view.update(window, |app, _| {
        app.checked_at
            .get(&crate::settings_code::code_check_key(1))
            .cloned()
    });
    let at = at.expect("the time it checked");
    let line = crate::hermes_pages::checked_line(&at);
    assert!(
        painted_text(window, "ui-text-rc2r65-code-checked-1", &line),
        "the checked time is painted in the pressed helper card"
    );

    // The sibling's negative assertion is meaningful only when its actual
    // card and control are present and visible. Reveal with wheel input only;
    // clicking a sibling action would mutate the state under test.
    let check0 = reveal_settings_face(window, "sx-check-0");
    let page = window
        .debug_bounds("sx-page")
        .expect("Workshop page viewport");
    let card0 = window
        .debug_bounds("ui-card-sx-helper-card-0")
        .expect("the sibling helper card is rendered");
    assert!(
        bounds_inside(card0, page),
        "sibling card is fully visible: {card0:?} / {page:?}"
    );
    assert!(
        bounds_inside(check0, card0),
        "sibling control belongs to its card: {check0:?} / {card0:?}"
    );
    let status0 = window
        .debug_bounds("sx-helper-ok-0")
        .or_else(|| window.debug_bounds("sx-helper-missing-0"))
        .expect("the sibling helper status face is rendered");
    assert!(
        bounds_inside(status0, card0),
        "sibling status is inside its card: {status0:?} / {card0:?}"
    );
    let sibling_checked_at = view.update(window, |app, _| {
        app.checked_at
            .get(&crate::settings_code::code_check_key(0))
            .cloned()
    });
    assert!(
        sibling_checked_at.is_none(),
        "row 0 has no check timestamp: {sibling_checked_at:?}"
    );
    assert!(
        !shown(window, "ui-text-rc2r65-code-checked-0"),
        "the other helper has no checked time"
    );
    assert_eq!(line, format!("Checked again at {at}."));
}

/// RC2-R610 (usability RC2 round 5, Minor 9: ZCode and DeepSeek Harness
/// said only "✓ Installed"): Settings › Code says whether their key is set,
/// and how to set it when it is not.
#[gpui::test]
fn rc2r610_the_code_page_says_whether_a_key_is_set(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_code::tests::rc2r610_the_code_page_says_whether_a_key_is_set") {
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os(CHILD).unwrap());
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["zcode", "dsh"] {
        crate::exec_fixture::write_exec_fixture(&bin.join(name), "#!/bin/sh\n", 0o755);
    }
    let env = home.join("deepseek.conf");
    with_config(&format!(
        "[work.agents]\nzcode_bin = \"{}\"\ndsh_bin = \"{}\"\ndsh_env_file = \"{}\"\n",
        bin.join("zcode").display(),
        bin.join("dsh").display(),
        env.display()
    ));
    let (_view, window) = settings_window(cx);
    click(window, "sx-nav-2");
    // ZCode is row 2, the DeepSeek Harness row 3: no key yet.
    assert!(
        shown(window, "sx-helper-signed-out-2"),
        "ZCode says its key is not set"
    );
    assert!(shown(window, "sx-helper-signed-out-3"), "so does DeepSeek");
    let signed_out = "Installed, but its key is not set.";
    reveal_settings_face(window, "ui-text-sx-helper-status-2");
    assert!(
        painted_text_contains(window, "ui-text-sx-helper-status-2", signed_out),
        "ZCode's no-key status is actually painted before its key is created"
    );
    reveal_settings_face(window, "ui-text-sx-helper-status-3");
    assert!(
        painted_text_contains(window, "ui-text-sx-helper-status-3", signed_out),
        "DeepSeek's no-key status is actually painted before its key is created"
    );
    let zcode = home.join(".zcode/cli/config.json");
    std::fs::create_dir_all(zcode.parent().unwrap()).unwrap();
    std::fs::write(
        &zcode,
        r#"{"provider":{"zai":{"options":{"apiKey":"k-1"}}}}"#,
    )
    .unwrap();
    std::fs::write(&env, "DEEPSEEK_API_KEY=sk-1\n").unwrap();
    click_revealed_settings_action(window, "sx-check-2");
    let signed_in = "✓ Installed, and its key is set";
    reveal_settings_face(window, "ui-text-sx-helper-status-2");
    assert!(
        painted_text(window, "ui-text-sx-helper-status-2", signed_in),
        "ZCode's key is set in the painted status"
    );
    reveal_settings_face(window, "ui-text-sx-helper-status-3");
    assert!(
        painted_text(window, "ui-text-sx-helper-status-3", signed_in),
        "DeepSeek's key is set in the painted status"
    );
}

/// RC2-R614 (usability RC2 round 5, Settings: Models & Fleet said "No model
/// services yet. Most people do not need one." while Gemma ran on the Mac
/// Studio): with no extra service, the line names the model Work uses and
/// where it runs; with no in-app model set, it reads as before.
#[test]
fn rc2r614_models_and_fleet_names_the_in_app_model() {
    let gemma = awili_config::EngineConfig {
        base_url: "http://10.77.68.94:8844".into(),
        label: "Gemma 4 26B (Studio)".into(),
        ..Default::default()
    };
    let line = crate::no_model_services_line(&gemma);
    assert!(
        line.contains("Gemma 4 26B (Studio), runs at http://10.77.68.94:8844"),
        "{line}"
    );
    assert!(!line.contains("No model services yet"), "{line}");
    let none = awili_config::EngineConfig {
        base_url: String::new(),
        ..Default::default()
    };
    assert_eq!(
        crate::no_model_services_line(&none),
        "No model services yet. Most people do not need one."
    );
}
