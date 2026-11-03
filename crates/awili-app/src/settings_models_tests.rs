//! SXe proof tests: Models & Fleet and safe closing. The old Chat assistant
//! picker is retired: its stored `[chat].executor` key stays readable, but
//! Settings no longer offers it as an alternate execution mode.
use super::*;
use crate::settings_catalog::{self as cat, Page};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_SXE_TEST_HOME";

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

/// Write the config file Awili will load (call before `settings_window`).
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

fn is_open(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> bool {
    view.update(window, |app, _| app.settings.open)
}

/// Open Models & Fleet and add one valid, unsaved model-service card.
fn unsaved_service(view: &Entity<AwiliApp>, window: &mut VisualTestContext) {
    click(window, "sx-nav-3");
    view.update(window, |app, cx| {
        let i = app.settings.add_provider().unwrap();
        let p = &mut app.settings.draft.as_mut().unwrap().providers[i];
        p.name = "lan".into();
        p.base_url = "http://127.0.0.1:9".into();
        cx.notify();
    });
    window.run_until_parked();
}

#[gpui::test]
fn sxe_scrim_click_does_not_close_settings(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_models::tests::sxe_scrim_click_does_not_close_settings") {
        return;
    }
    let (view, window) = settings_window(cx);
    let panel = window.debug_bounds("sx-panel").expect("the panel");
    let beside = point(panel.origin.x / 2., panel.origin.y + px(40.));
    assert!(
        beside.x > px(0.),
        "there is room beside the panel: {panel:?}"
    );
    window.simulate_click(beside, Modifiers::default());
    window.run_until_parked();
    assert!(
        is_open(&view, window),
        "a click beside Settings keeps it open"
    );
}

#[gpui::test]
fn sxe_done_closes_when_nothing_is_pending(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_models::tests::sxe_done_closes_when_nothing_is_pending") {
        return;
    }
    let (view, window) = settings_window(cx);
    let done = window.debug_bounds("sx-done-0").expect("a Done button");
    assert!(done.size.height >= px(crate::settings_pages::HIT));
    click(window, "sx-done-0");
    assert!(!is_open(&view, window));
}

#[gpui::test]
fn sxe_close_with_unsaved_services_asks_first(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_models::tests::sxe_close_with_unsaved_services_asks_first") {
        return;
    }
    let (view, window) = settings_window(cx);
    unsaved_service(&view, window);
    click(window, "sx-done-0");
    assert!(is_open(&view, window), "Done asks first");
    assert!(shown(window, "sx-confirm-close"));
    click(window, "sx-confirm-keep-0");
    assert!(is_open(&view, window) && !shown(window, "sx-confirm-close"));
    let cards = view.update(window, |app, _| {
        app.settings.draft.as_ref().unwrap().providers.len()
    });
    assert_eq!(cards, 1, "Keep editing keeps the card");
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(is_open(&view, window), "Esc asks too");
    assert!(shown(window, "sx-confirm-close"));
    click(window, "sx-confirm-discard-0");
    assert!(!is_open(&view, window), "Throw away closes");
    assert!(
        !config_text(&view, window).contains("lan"),
        "and nothing was written"
    );
}

#[gpui::test]
fn sxe_save_services_button_only_when_changed(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_models::tests::sxe_save_services_button_only_when_changed") {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-3");
    assert!(!shown(window, "sx-save-services-0"), "nothing to save");
    unsaved_service(&view, window);
    assert!(shown(window, "sx-save-services-0") && shown(window, "sx-discard-services-0"));
    let scroll = view.read_with(window, |app, _| app.settings_nav_scroll.clone());
    let (viewport, save) =
        settings_models_reveal_face(window, &scroll, "ui-button-sx-save-services-0", -120.);
    assert!(settings_models_fully_inside(viewport, save));
    click(window, "ui-button-sx-save-services-0");
    assert!(config_text(&view, window).contains("name = \"lan\""));
    assert!(
        !shown(window, "sx-save-services-0"),
        "saved: the button goes away"
    );
    assert!(is_open(&view, window), "saving does not close Settings");
}

#[gpui::test]
fn sxe_keyboard_reaches_scrollable_service_save_and_discard(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_models::tests::sxe_keyboard_reaches_scrollable_service_save_and_discard",
    ) {
        return;
    }
    with_config("");
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-3");
    unsaved_service(&view, window);

    // Establish the existing Settings navigation focus once, then use its
    // real Tab order to reach the appended service-action row.
    let focus = view.update(window, |app, _| app.settings_nav_focus.clone());
    window.update(|window, cx| window.focus(&focus, cx));
    window.run_until_parked();
    assert_eq!(
        view.update(window, |app, _| app.settings_nav.selected()),
        settings_nav::Action::AddProvider
    );
    for _ in 0..32 {
        if view.update(window, |app, _| app.settings_nav.selected()) == settings_nav::Action::Save {
            break;
        }
        window.simulate_keystrokes("tab");
        window.run_until_parked();
    }
    assert_eq!(
        view.update(window, |app, _| app.settings_nav.selected()),
        settings_nav::Action::Save,
        "Tab reaches Save after the existing provider and local-model actions"
    );
    let save = window
        .debug_bounds("ui-button-sx-save-services-0")
        .expect("the focused Save action is rendered");
    assert!(
        settings_models_fully_inside(settings_models_current_viewport(window), save),
        "the selected Save Button is fully revealed in the physical scroll viewport: {save:?}"
    );
    assert!(
        view.update(window, |app, _| {
            app.settings_nav_focused && app.settings_nav.selected() == settings_nav::Action::Save
        }),
        "the actual Settings navigation handle owns focus on Save"
    );
    assert!(
        shown(window, "sx-keyboard-focus"),
        "Save paints the selected focus marker"
    );
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert!(config_text(&view, window).contains("name = \"lan\""));
    assert!(!view.update(window, |app, _| app.settings.services_dirty()));

    // Add another draft provider without moving the actual keyboard focus.
    view.update(window, |app, cx| {
        let ix = app.settings.add_provider().unwrap();
        let provider = &mut app.settings.draft.as_mut().unwrap().providers[ix];
        provider.name = "draft-only".into();
        provider.base_url = "http://127.0.0.1:10".into();
        cx.notify();
    });
    window.run_until_parked();
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    assert_eq!(
        view.update(window, |app, _| app.settings_nav.selected()),
        settings_nav::Action::Discard,
        "Tab advances from Save to Discard"
    );
    let discard = window
        .debug_bounds("ui-button-sx-discard-services-0")
        .expect("the focused Discard action is rendered");
    assert!(
        settings_models_fully_inside(settings_models_current_viewport(window), discard),
        "the selected Discard Button is fully revealed in the physical scroll viewport: {discard:?}"
    );
    assert!(
        shown(window, "sx-keyboard-focus"),
        "Discard paints the selected focus marker"
    );
    window.simulate_keystrokes("space");
    window.run_until_parked();
    assert!(!view.update(window, |app, _| app.settings.services_dirty()));
    assert_eq!(
        view.update(window, |app, _| {
            app.settings.draft.as_ref().unwrap().providers[0]
                .name
                .clone()
        }),
        "lan",
        "Space discards the unsaved second provider while preserving the saved one"
    );
}

#[gpui::test]
fn sxe_default_model_applies_live_and_undoes(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_models::tests::sxe_default_model_applies_live_and_undoes") {
        return;
    }
    with_config(
        "[[providers]]\nname = \"lan\"\nkind = \"ollama\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"a\", \"b\"]\nenabled = true\n",
    );
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-3");
    let before = config_text(&view, window);
    click(window, "ui-button-sx-model-1");
    let after = config_text(&view, window);
    assert!(
        after.contains("default_provider = \"lan\"") && after.contains("default_model = \"b\""),
        "{after}"
    );
    click(window, "sx-undo-0");
    assert_eq!(
        config_text(&view, window),
        before,
        "undo puts the bytes back"
    );
}

#[gpui::test]
fn sxe_models_page_no_longer_shows_memory_or_voice_rows(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_models::tests::sxe_models_page_no_longer_shows_memory_or_voice_rows",
    ) {
        return;
    }
    let (_view, window) = settings_window(cx);
    click(window, "sx-nav-3");
    assert!(shown(window, "sx-section-Model services"));
    assert!(shown(window, "sx-section-Default model"));
    assert!(
        !shown(window, "sx-section-Memory service"),
        "Memory has its own page"
    );
    assert!(!shown(window, "sx-section-Voice"), "Voice has its own page");
}

#[test]
fn the_retired_answer_helper_row_is_not_offered_but_its_key_is_known() {
    const { assert!(!cat::CHAT_PICKER, "the answer helper picker is retired") };
    assert!(!cat::items_on(Page::Workshop, false)
        .iter()
        .any(|i| i.id == "chat.executor"));
    assert!(!cat::items_on(Page::Workshop, true)
        .iter()
        .any(|i| i.id == "chat.executor"));
    assert!(
        cat::find("chat.executor").is_some(),
        "an old config that sets it still reads"
    );
}

/// SXe-FIX1 (Codex P2 on 020d7d5): the question's "Save and close" writes
/// the cards and closes Settings.
#[gpui::test]
fn sxe_fix_save_and_close_saves_then_closes(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_models::tests::sxe_fix_save_and_close_saves_then_closes") {
        return;
    }
    let (view, window) = settings_window(cx);
    unsaved_service(&view, window);
    click(window, "sx-done-0");
    assert!(shown(window, "sx-confirm-close"), "Done asks first");
    click(window, "sx-confirm-save-0");
    assert!(!is_open(&view, window), "a landed save closes Settings");
    assert!(
        config_text(&view, window).contains("name = \"lan\""),
        "and the card reached disk"
    );
}

/// SXe-FIX1 (Codex P1 on 020d7d5, settings_models.rs:195): when the write is
/// refused, "Save and close" keeps Settings open with the cards and the
/// question (and its reason) on screen, and the cards still read as
/// unsaved, so a later Done asks again instead of losing them.
#[gpui::test]
fn sxe_fix_refused_save_keeps_the_draft_and_the_question(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_models::tests::sxe_fix_refused_save_keeps_the_draft_and_the_question",
    ) {
        return;
    }
    with_config("[chat]\nexecutor = \"agent:claude:haiku\"\n");
    let (view, window) = settings_window(cx);
    let path = view.update(window, |app, _| app.settings.path().clone());
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    let bak = std::path::PathBuf::from(bak);
    // A non-empty directory where the backup goes: the save is refused
    // (even for root) before the config file is touched.
    std::fs::create_dir_all(bak.join("keep")).unwrap();
    let before = config_text(&view, window);
    unsaved_service(&view, window);
    click(window, "sx-done-0");
    click(window, "sx-confirm-save-0");
    assert!(
        is_open(&view, window),
        "a refused save never closes Settings"
    );
    assert!(shown(window, "sx-confirm-close"), "the question stays");
    assert!(shown(window, "sx-confirm-error"), "and says why");
    assert_eq!(config_text(&view, window), before, "nothing reached disk");
    let (cards, dirty) = view.update(window, |app, _| {
        (
            app.settings.draft.as_ref().unwrap().providers.len(),
            app.settings.services_dirty(),
        )
    });
    assert_eq!(cards, 1, "the card is kept");
    assert!(dirty, "and still reads as unsaved");
    click(window, "sx-confirm-keep-0");
    click(window, "sx-done-0");
    assert!(
        is_open(&view, window) && shown(window, "sx-confirm-close"),
        "Done asks again"
    );
    assert!(
        !shown(window, "sx-confirm-error"),
        "a fresh question carries no old reason"
    );
    // With the way clear, the same answer saves and closes.
    std::fs::remove_dir_all(&bak).unwrap();
    click(window, "sx-confirm-save-0");
    assert!(!is_open(&view, window), "the retried save closes");
    assert!(config_text(&view, window).contains("name = \"lan\""));
}

/// SXe-FIX1: "Use my memory when answering" Off (and the other memory rows)
/// work on a config with no `[memory]` table: the table is created with the
/// address in use, and nothing is refused.
#[gpui::test]
fn sxe_fix_memory_off_without_a_memory_table(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_models::tests::sxe_fix_memory_off_without_a_memory_table") {
        return;
    }
    with_config("[chat]\nexecutor = \"agent:claude:haiku\"\n");
    let (view, window) = settings_window(cx);
    let default_address = awili_config::Config::default().memory.base_url;
    let error = |view: &Entity<AwiliApp>, window: &mut VisualTestContext| {
        view.update(window, |app, _| app.settings.error.clone())
    };
    click(window, "sx-nav-4");
    click(window, &format!("sx-on-{}", ix("memory.recall")));
    assert_eq!(error(&view, window), None, "Off is not refused");
    let text = config_text(&view, window);
    let cfg = awili_config::Config::parse(&text).unwrap();
    assert!(cfg.memory.recall, "recall is on on disk: {text}");
    assert_eq!(cfg.memory.base_url, default_address, "the address in use");
    assert!(
        shown(window, &format!("sx-reset-{}", ix("memory.recall"))),
        "the row shows as changed"
    );
    let address_changed = view.update(window, |app, _| {
        cat::is_modified(
            app.settings.doc.as_ref().unwrap(),
            cat::find("memory.base_url").unwrap(),
        )
    });
    assert!(
        !address_changed,
        "the written address is the default, not a change"
    );
    click(window, &format!("sx-on-{}", ix("memory.write")));
    assert_eq!(error(&view, window), None, "a second memory row works too");
    assert!(
        awili_config::Config::parse(&config_text(&view, window))
            .unwrap()
            .memory
            .write
    );
    // Undo walks both back to the file without a memory table.
    click(window, "sx-undo-0");
    click(window, "sx-undo-0");
    assert_eq!(
        config_text(&view, window),
        "[chat]\nexecutor = \"agent:claude:haiku\"\n"
    );
}

/// Read text actually laid out inside a current frame and the named face.
fn painted_texts(window: &mut VisualTestContext, selector: &str) -> Vec<String> {
    crate::ui::text::DRAWN_TEXTS.with(|drawn| drawn.borrow_mut().clear());
    window.update(|window, _cx| window.refresh());
    window.run_until_parked();
    let Some(bounds) = window.debug_bounds(sel(selector)) else {
        return Vec::new();
    };
    crate::ui::text::DRAWN_TEXTS.with(|drawn| {
        drawn
            .borrow()
            .iter()
            .filter_map(|layout| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (layout.text().to_string(), layout.bounds())
                }))
                .ok()
                .filter(|(_, text_bounds)| bounds.contains(&text_bounds.center()))
                .map(|(text, _)| text)
            })
            .collect()
    })
}

#[gpui::test]
fn sxe_provider_card_uses_kit_faces_and_masks_the_api_key(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_models::tests::sxe_provider_card_uses_kit_faces_and_masks_the_api_key",
    ) {
        return;
    }
    const SECRET: &str = "test-api-key-do-not-paint";
    with_config(&format!(
        "[[providers]]\nname = \"cloud\"\nkind = \"openai\"\nbase_url = \"https://example.invalid/v1\"\nmodels = [\"model-a\"]\napi_key = \"{SECRET}\"\nenabled = true\n"
    ));
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-3");

    let card = window
        .debug_bounds("ui-card-provider-card-0")
        .expect("the provider is a shared Card");
    for selector in [
        "ui-row-provider-health-0",
        "ui-row-prov-name-0",
        "ui-button-prov-kind-0",
        "ui-button-prov-enabled-0",
        "ui-icon-button-prov-remove-0",
        "ui-row-prov-url-0",
        "ui-row-prov-models-0",
        "ui-row-prov-key-0",
    ] {
        let face = window
            .debug_bounds(sel(selector))
            .unwrap_or_else(|| panic!("the KIT face {selector} is mounted"));
        assert!(face.size.height >= theme::target(), "{selector}: {face:?}");
        assert!(
            card.contains(&face.center()),
            "{selector} is inside {card:?}"
        );
    }
    let key_text = painted_texts(window, "ui-row-prov-key-0");
    assert!(
        key_text.iter().any(|text| text.contains("••••")),
        "{key_text:?}"
    );
    assert!(
        key_text.iter().all(|text| !text.contains(SECRET)),
        "the raw key must never be painted"
    );

    click(window, "ui-row-prov-url-0");
    assert_eq!(
        view.update(window, |app, _| app.settings.field),
        Some(settings::Field::ProviderUrl(0)),
        "the actual Row face enters the existing draft editor"
    );
    assert!(shown(window, "ui-text-field-prov-url-0"));
}

#[gpui::test]
fn sxe_provider_buttons_keep_the_parent_action_model(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_models::tests::sxe_provider_buttons_keep_the_parent_action_model",
    ) {
        return;
    }
    with_config(
        "[[providers]]\nname = \"lan\"\nkind = \"ollama\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"a\"]\nenabled = true\n",
    );
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-3");

    let focus = view.update(window, |app, _| app.settings_nav_focus.clone());
    window.update(|window, cx| window.focus(&focus, cx));
    window.run_until_parked();
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    assert_eq!(
        view.update(window, |app, _| app.settings_nav.selected()),
        settings_nav::Action::LocalDiscovery,
        "the existing parent action order reaches Local Discovery before provider rows"
    );
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    assert_eq!(
        view.update(window, |app, _| app.settings_nav.selected()),
        settings_nav::Action::ProviderName(0),
        "Tab advances the parent action list rather than focusing a child Button"
    );
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert_eq!(
        view.update(window, |app, _| app.settings.field),
        Some(settings::Field::ProviderName(0)),
        "the selected parent action activates the same provider editor"
    );
}

#[gpui::test]
fn sxe_provider_kind_enabled_remove_and_footer_actions_use_real_faces(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_models::tests::sxe_provider_kind_enabled_remove_and_footer_actions_use_real_faces",
    ) {
        return;
    }
    with_config(
        "[[providers]]\nname = \"lan\"\nkind = \"ollama\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [\"a\"]\nenabled = true\n",
    );
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-3");
    assert!(painted_texts(window, "ui-row-provider-health-0")
        .iter()
        .any(|text| text == "Not checked" || text == "Reachable" || text == "Unreachable"));

    click(window, "ui-button-prov-kind-0");
    let kind = view.update(window, |app, _| {
        app.settings.draft.as_ref().unwrap().providers[0].kind
    });
    assert_eq!(kind, awili_config::ProviderKind::Anthropic);
    click(window, "ui-button-prov-enabled-0");
    assert!(!view.update(window, |app, _| {
        app.settings.draft.as_ref().unwrap().providers[0].enabled
    }));
    let health = painted_texts(window, "ui-row-provider-health-0");
    assert!(health.iter().any(|text| text == "Disabled"), "{health:?}");
    assert!(shown(window, "sx-save-services-0"));
    click(window, "ui-icon-button-prov-remove-0");
    assert_eq!(
        view.update(window, |app, _| app
            .settings
            .draft
            .as_ref()
            .unwrap()
            .providers
            .len()),
        0,
        "the real IconButton removes the provider"
    );
    assert!(shown(window, "rc2r614-model-services-empty"));
    assert!(shown(window, "sx-save-services-0"));
    let scroll = view.read_with(window, |app, _| app.settings_nav_scroll.clone());
    let (viewport, discard) =
        settings_models_reveal_face(window, &scroll, "ui-button-sx-discard-services-0", -120.);
    assert!(settings_models_fully_inside(viewport, discard));
    click(window, "ui-button-sx-discard-services-0");
    assert!(!shown(window, "sx-save-services-0"));
}

#[gpui::test]
fn sxe_models_narrow_zoom_keeps_provider_and_footer_faces_reachable(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_models::tests::sxe_models_narrow_zoom_keeps_provider_and_footer_faces_reachable",
    ) {
        return;
    }
    with_config("");
    theme::set_ui_scale(1.25);
    let (view, window) = settings_window(cx);
    window.simulate_resize(gpui::size(px(760.), px(820.)));
    window.run_until_parked();
    click(window, "sx-nav-3");
    click(window, "ui-button-prov-add-0");
    assert!(shown(window, "ui-card-provider-card-0"));
    for selector in [
        "ui-row-prov-name-0",
        "ui-button-prov-kind-0",
        "ui-button-prov-enabled-0",
        "ui-icon-button-prov-remove-0",
    ] {
        let face = window
            .debug_bounds(sel(selector))
            .unwrap_or_else(|| panic!("{selector} remains reachable at 760 px / 1.25×"));
        assert!(face.size.height >= theme::target(), "{selector}: {face:?}");
    }
    let scroll = view.read_with(window, |app, _| app.settings_nav_scroll.clone());
    let (footer_viewport, save) =
        settings_models_reveal_face(window, &scroll, "ui-button-sx-save-services-0", -120.);
    assert!(save.size.height >= theme::target());
    assert!(settings_models_fully_inside(footer_viewport, save));
    let (discard_viewport, discard) =
        settings_models_reveal_face(window, &scroll, "ui-button-sx-discard-services-0", 120.);
    assert!(discard.size.height >= theme::target());
    assert!(settings_models_fully_inside(discard_viewport, discard));
    let card = window.debug_bounds("ui-card-provider-card-0").unwrap();
    let name = window.debug_bounds("ui-row-prov-name-0").unwrap();
    let kind = window.debug_bounds("ui-button-prov-kind-0").unwrap();
    assert!(card.contains(&name.center()) && card.contains(&kind.center()));
    assert!(
        name.origin.y + name.size.height <= kind.origin.y
            || kind.origin.y + kind.size.height <= name.origin.y,
        "wrapped provider controls do not overlap: name={name:?}, kind={kind:?}"
    );
    assert!(view.update(window, |app, _| app.settings.services_dirty()));

    // At 2.5× each natural label is wider than the form column. The service
    // actions switch to full-width KIT Buttons, stack inside the card, and
    // remain reachable as actual physical targets in the same scroll view.
    theme::set_ui_scale(2.5);
    view.update(window, |_app, cx| cx.notify());
    window.run_until_parked();
    for (selector, direction) in [
        ("ui-button-sx-save-services-0", -120.),
        ("ui-button-sx-discard-services-0", 120.),
    ] {
        let (viewport, face) = settings_models_reveal_face(window, &scroll, selector, direction);
        assert!(face.size.height >= theme::target(), "{selector}: {face:?}");
        assert!(
            settings_models_fully_inside(viewport, face),
            "the 2.5× {selector} Button face stays inside the physical viewport: face={face:?}, viewport={viewport:?}"
        );
    }
    view.update(window, |app, cx| {
        let provider = &mut app.settings.draft.as_mut().unwrap().providers[0];
        provider.name = "zoomed".into();
        provider.base_url = "http://127.0.0.1:9".into();
        cx.notify();
    });
    window.run_until_parked();
    let focus = view.read_with(window, |app, _| app.settings_nav_focus.clone());
    window.update(|window, cx| window.focus(&focus, cx));
    window.run_until_parked();
    for _ in 0..32 {
        if view.update(window, |app, _| app.settings_nav.selected()) == settings_nav::Action::Save {
            break;
        }
        window.simulate_keystrokes("tab");
        window.run_until_parked();
    }
    assert_eq!(
        view.update(window, |app, _| app.settings_nav.selected()),
        settings_nav::Action::Save
    );
    let save = window.debug_bounds("ui-button-sx-save-services-0").unwrap();
    assert!(settings_models_fully_inside(
        settings_models_current_viewport(window),
        save
    ));
    assert!(shown(window, "sx-keyboard-focus"));
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert!(config_text(&view, window).contains("name = \"zoomed\""));
    assert!(is_open(&view, window));

    view.update(window, |app, cx| {
        let ix = app.settings.add_provider().unwrap();
        let provider = &mut app.settings.draft.as_mut().unwrap().providers[ix];
        provider.name = "discarded".into();
        provider.base_url = "http://127.0.0.1:10".into();
        cx.notify();
    });
    window.run_until_parked();
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    assert_eq!(
        view.update(window, |app, _| app.settings_nav.selected()),
        settings_nav::Action::Discard
    );
    let discard = window
        .debug_bounds("ui-button-sx-discard-services-0")
        .unwrap();
    assert!(settings_models_fully_inside(
        settings_models_current_viewport(window),
        discard
    ));
    assert!(shown(window, "sx-keyboard-focus"));
    window.simulate_keystrokes("space");
    window.run_until_parked();
    assert!(!view.update(window, |app, _| app.settings.services_dirty()));
    assert!(is_open(&view, window));
    assert!(!config_text(&view, window).contains("name = \"discarded\""));
    theme::set_ui_scale(1.0);
}

#[gpui::test]
fn sxe_models_config_error_uses_a_kit_empty_state(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_models::tests::sxe_models_config_error_uses_a_kit_empty_state")
    {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-3");
    view.update(window, |app, cx| {
        app.settings.draft = None;
        app.settings.error = Some("invalid model configuration".to_string());
        cx.notify();
    });
    window.run_until_parked();
    assert!(shown(
        window,
        "ui-empty-state-settings-models-config-unavailable"
    ));
    let text = painted_texts(window, "ui-empty-state-settings-models-config-unavailable");
    assert!(text
        .iter()
        .any(|line| line.contains("invalid model configuration")));
    assert!(text.iter().any(|line| line.contains("Raw TOML tab")));
}

fn settings_models_intersection(
    a: gpui::Bounds<gpui::Pixels>,
    b: gpui::Bounds<gpui::Pixels>,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    let left = a.left().max(b.left());
    let top = a.top().max(b.top());
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (left < right && top < bottom)
        .then(|| gpui::Bounds::from_corners(gpui::point(left, top), gpui::point(right, bottom)))
}

fn settings_models_wheel(
    window: &mut VisualTestContext,
    position: gpui::Point<gpui::Pixels>,
    delta_y: f32,
) {
    window.simulate_event(gpui::ScrollWheelEvent {
        position,
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(delta_y))),
        touch_phase: gpui::TouchPhase::Moved,
        ..Default::default()
    });
    window.run_until_parked();
}

fn settings_models_fully_inside(
    viewport: gpui::Bounds<gpui::Pixels>,
    face: gpui::Bounds<gpui::Pixels>,
) -> bool {
    face.left() >= viewport.left()
        && face.top() >= viewport.top()
        && face.right() <= viewport.right()
        && face.bottom() <= viewport.bottom()
}

fn settings_models_current_viewport(window: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
    let scroll = window
        .debug_bounds("settings-panel-scroll")
        .expect("the model form has its own scroll face");
    let panel = window.debug_bounds("sx-panel").expect("Settings panel");
    let window_size = window.update(|window, _| window.viewport_size());
    let window_bounds = gpui::Bounds::new(gpui::point(px(0.), px(0.)), window_size);
    settings_models_intersection(scroll, panel)
        .and_then(|bounds| settings_models_intersection(bounds, window_bounds))
        .expect("the model form has a visible physical scroll viewport")
}

fn settings_models_reveal_face(
    window: &mut VisualTestContext,
    scroll: &gpui::ScrollHandle,
    selector: &str,
    delta_y: f32,
) -> (gpui::Bounds<gpui::Pixels>, gpui::Bounds<gpui::Pixels>) {
    for _ in 0..40 {
        let viewport = settings_models_current_viewport(window);
        let face = window
            .debug_bounds(sel(selector))
            .unwrap_or_else(|| panic!("{selector} remains mounted"));
        if settings_models_fully_inside(viewport, face) {
            return (viewport, face);
        }
        let before = scroll.offset().y;
        settings_models_wheel(window, viewport.center(), delta_y);
        let after = scroll.offset().y;
        if after == before {
            break;
        }
    }
    let viewport = settings_models_current_viewport(window);
    let face = window
        .debug_bounds(sel(selector))
        .unwrap_or_else(|| panic!("{selector} remains mounted"));
    assert!(
        settings_models_fully_inside(viewport, face),
        "bounded real-wheel reveal fully contains {selector}: face={face:?}, viewport={viewport:?}"
    );
    (viewport, face)
}

#[gpui::test]
fn sxe_many_saved_models_scroll_with_provider_controls_at_narrow_zoom(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_models::tests::sxe_many_saved_models_scroll_with_provider_controls_at_narrow_zoom",
    ) {
        return;
    }
    let models = (0..18).map(|i| format!("model-{i:02}")).collect::<Vec<_>>();
    let model_list = models
        .iter()
        .map(|model| format!("\"{model}\""))
        .collect::<Vec<_>>()
        .join(", ");
    with_config(&format!(
        "[[providers]]\nname = \"lan\"\nkind = \"ollama\"\nbase_url = \"http://127.0.0.1:9\"\nmodels = [{model_list}]\nenabled = true\n"
    ));

    theme::set_ui_scale(1.5);
    let (view, window) = settings_window(cx);
    window.simulate_resize(gpui::size(px(760.), px(740.)));
    window.run_until_parked();
    click(window, "sx-nav-3");

    let scroll = view.read_with(window, |app, _| app.settings_nav_scroll.clone());
    let viewport = settings_models_current_viewport(window);
    let before = scroll.offset().y;
    settings_models_wheel(window, viewport.center(), -120.);
    assert_ne!(
        scroll.offset().y,
        before,
        "a real wheel over the visible model-form viewport moves its scroll handle"
    );

    let (_viewport, last_model) =
        settings_models_reveal_face(window, &scroll, "ui-button-sx-model-17", -120.);
    assert!(last_model.size.height >= theme::target());
    click(window, "ui-button-sx-model-17");
    assert!(
        config_text(&view, window).contains("default_model = \"model-17\""),
        "the actual visible choice still applies live from saved provider data"
    );

    let (provider_viewport, provider_name) =
        settings_models_reveal_face(window, &scroll, "ui-row-prov-name-0", 120.);
    assert!(provider_name.size.height >= theme::target());
    assert!(settings_models_fully_inside(
        provider_viewport,
        provider_name
    ));

    let (add_viewport, add_face) =
        settings_models_reveal_face(window, &scroll, "ui-button-prov-add-0", -120.);
    assert!(add_face.size.height >= theme::target());
    assert!(settings_models_fully_inside(add_viewport, add_face));
    click(window, "ui-button-prov-add-0");
    assert_eq!(
        view.update(window, |app, _| app
            .settings
            .draft
            .as_ref()
            .unwrap()
            .providers
            .len()),
        2,
        "the real Add action remains reachable after returning through the same scroller"
    );
    theme::set_ui_scale(1.0);
}
