//! Exercise the saved preference and existing app state, with a private profile.
use crate::{consolidation_tests::isolated, theme, work::Mode, AwiliApp, MessageRow, Role};
use awili_config::ColorMode;
use gpui::{px, size};

fn setup(cx: &mut gpui::TestAppContext) -> (gpui::Entity<AwiliApp>, &mut gpui::VisualTestContext) {
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    cx.executor().allow_parking();
    let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    app.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = None;
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.set_mode(Mode::Work, cx);
        cx.notify();
    });
    window.simulate_resize(size(px(1400.), px(1100.)));
    window.run_until_parked();
    (app, window)
}

fn click(window: &mut gpui::VisualTestContext, selector: &str) {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    window.update(|win, _| win.refresh());
    window.run_until_parked();
    let bounds = window.debug_bounds(selector).expect(selector);
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
}

#[gpui::test]
fn appearance_settings_reset_undo_preserve_owned_run_and_staging(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "runtime_palette_tests::appearance_settings_reset_undo_preserve_owned_run_and_staging",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let (owner, tab, probe) = app.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        app.work.running = true;
        app.work.conv_id = Some(owner.clone());
        app.work.run_seq = 42;
        app.work.agent_abort_tx = Some(stop_tx);
        app.composer.update(cx, |c, cx| {
            c.set_text("Owned unfinished appearance draft", cx)
        });
        app.open_settings(cx);
        app.select_settings_page(crate::settings_catalog::Page::Appearance, cx);
        (
            owner,
            app.shell.tabs.active.clone(),
            app.provider_probe_generation,
        )
    });
    window.run_until_parked();
    let index = crate::settings_catalog::ITEMS
        .iter()
        .position(|i| i.id == "appearance.color_mode")
        .unwrap();
    click(window, &format!("sx-choice-{}", index * 16 + 1));
    assert!(theme::is_light_mode());
    assert_eq!(
        app.read_with(window, |app, _| app.appearance.preference),
        ColorMode::Light
    );
    click(window, &format!("sx-reset-{index}"));
    assert!(!theme::is_light_mode());
    click(window, "sx-undo-0");
    assert!(theme::is_light_mode());
    let native_light = window.update(|win, _| {
        matches!(
            win.appearance(),
            gpui::WindowAppearance::Light | gpui::WindowAppearance::VibrantLight
        )
    });
    click(
        window,
        &format!(
            "sx-choice-{}",
            index * 16 + if native_light { 0 } else { 1 }
        ),
    );
    assert_ne!(
        theme::is_light_mode(),
        native_light,
        "Explicit mode starts opposite the actual window"
    );
    click(window, &format!("sx-choice-{}", index * 16 + 2));
    app.read_with(window, |app, cx| {
        assert_eq!(app.appearance.preference, ColorMode::System);
        assert_eq!(
            app.saved_config().unwrap().appearance.color_mode,
            ColorMode::System
        );
        assert_eq!(app.sidebar.active_id(), owner);
        assert_eq!(app.shell.tabs.active, tab);
        assert_eq!(app.provider_probe_generation, probe);
        assert!(app.work.running && app.work.agent_abort_tx.is_some());
        assert_eq!(app.work.run_seq, 42);
        assert_eq!(app.work.conv_id.as_deref(), Some(owner.as_str()));
        assert_eq!(
            app.composer.read(cx).text().to_string(),
            "Owned unfinished appearance draft"
        );
    });
    assert!(matches!(
        stop_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    // The test platform is Light; this proves startup/current appearance resolution,
    // not delivery of a real macOS appearance-change notification.
    assert_eq!(
        theme::is_light_mode(),
        native_light,
        "System changes effective paint to the actual window appearance"
    );
    app.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn appearance_commands_repaint_cached_filtered_replies_and_keep_metadata(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("runtime_palette_tests::appearance_commands_repaint_cached_filtered_replies_and_keep_metadata") { return; }
    let (app, window) = setup(cx);
    let owner = app.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        let view = app.views.get_mut(&owner).unwrap();
        view.convo.push(Role::User, "Recorded question");
        view.convo.push(
            Role::Assistant,
            "[Link](https://example.com/recorded) and `inline`.\n\n[answered by recorded-model]",
        );
        view.rows = view.convo.messages().iter().map(MessageRow::new).collect();
        view.rows[1].activity = vec!["Recorded activity".into()];
        app.composer
            .update(cx, |c, cx| c.set_text("Unsent repaint draft", cx));
        cx.notify();
        owner
    });
    window.run_until_parked();
    let before = app.read_with(window, |app, _| {
        let view = &app.views[&owner];
        let row = &view.rows[1];
        assert!(row.plan_palette.canvas == theme::DARK.canvas);
        (
            view.convo.message(1).unwrap().text.clone(),
            row.raw.clone(),
            row.footer.clone(),
            row.activity.clone(),
            row.parsed_len,
            view.reply_presentation_token.clone(),
        )
    });
    let cached = app.read_with(window, |app, _| {
        app.views[&owner].rows[1].plan.as_ptr() as usize
    });
    for _ in 0..3 {
        app.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        assert_eq!(
            app.read_with(window, |app, _| app.views[&owner].rows[1].plan.as_ptr()
                as usize),
            cached,
            "Ordinary repaint keeps its cached plan allocation"
        );
    }
    window.update(|win, cx| {
        app.update(cx, |app, cx| {
            app.run_command_palette_action(
                crate::command_palette::PaletteAction::SetColorMode(
                    crate::command_palette::ColorModeChoice::Light,
                ),
                win,
                cx,
            )
        })
    });
    window.run_until_parked();
    app.read_with(window, |app, cx| {
        let view = &app.views[&owner];
        let row = &view.rows[1];
        assert!(row.plan_palette.canvas == theme::LIGHT.canvas);
        assert_eq!(view.convo.message(1).unwrap().text, before.0);
        assert_eq!(row.raw, before.1);
        assert_eq!(row.footer, before.2);
        assert_eq!(row.activity, before.3);
        assert_eq!(row.parsed_len, before.4);
        assert!(view.reply_presentation_token.matches(&before.5));
        assert!(!row.raw.contains("answered by"));
        let crate::markdown_plan::PlanBlock::Text { text, .. } = &row.plan[0] else {
            panic!("text plan");
        };
        assert!(text
            .highlights
            .iter()
            .any(|(_, h)| h.color == Some(theme::LIGHT.link.into())));
        assert!(text
            .highlights
            .iter()
            .any(|(_, h)| h.background_color == Some(theme::LIGHT.inline_code_bg.into())));
        assert_eq!(
            app.composer.read(cx).text().to_string(),
            "Unsent repaint draft"
        );
        assert!(!app.settings.open);
        assert_eq!(
            awili_config::Config::load().unwrap().appearance.color_mode,
            ColorMode::Light
        );
    });
    window.update(|win, cx| {
        app.update(cx, |app, cx| {
            app.run_command_palette_action(
                crate::command_palette::PaletteAction::SetColorMode(
                    crate::command_palette::ColorModeChoice::Dark,
                ),
                win,
                cx,
            )
        })
    });
    window.run_until_parked();
    assert!(app.read_with(window, |app, _| app.views[&owner].rows[1]
        .plan_palette
        .canvas
        == theme::DARK.canvas));
    window.update(|win, cx| {
        app.update(cx, |app, cx| {
            app.run_command_palette_action(
                crate::command_palette::PaletteAction::RunCatalog("appearance.theme"),
                win,
                cx,
            )
        })
    });
    window.run_until_parked();
    assert!(app.read_with(window, |app, _| app.settings.open
        && app.settings.page == crate::settings_catalog::Page::Appearance));
}

#[gpui::test]
fn appearance_failed_save_keeps_effective_palette_and_preference(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "runtime_palette_tests::appearance_failed_save_keeps_effective_palette_and_preference",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    window.update(|win, cx| {
        app.update(cx, |app, cx| {
            app.choose_color_mode(ColorMode::Light, win, cx)
        })
    });
    window.run_until_parked();
    std::fs::write(awili_config::Config::config_paths()[0].clone(), "[broken").unwrap();
    window.update(|win, cx| {
        app.update(cx, |app, cx| {
            app.choose_color_mode(ColorMode::Dark, win, cx)
        })
    });
    window.run_until_parked();
    assert!(theme::is_light_mode());
    assert!(app.read_with(window, |app, _| app.appearance.preference
        == ColorMode::Light
        && app.settings.error.is_some()));
}

#[test]
fn appearance_resolves_all_native_variants_without_overwriting_system_preference() {
    use gpui::WindowAppearance::*;
    for native in [Light, VibrantLight, Dark, VibrantDark] {
        assert!(crate::appearance::effective_light(ColorMode::Light, native));
        assert!(!crate::appearance::effective_light(ColorMode::Dark, native));
        assert_eq!(
            crate::appearance::effective_light(ColorMode::System, native),
            matches!(native, Light | VibrantLight)
        );
    }
    assert_eq!(
        awili_config::ConfigDoc::parse("")
            .unwrap()
            .config()
            .appearance
            .color_mode,
        ColorMode::Dark
    );
    assert_eq!(
        awili_config::ConfigDoc::parse("[appearance]\ncolor_mode = 'unknown'\n")
            .unwrap()
            .config()
            .appearance
            .color_mode,
        ColorMode::Dark
    );
}

#[gpui::test]
fn appearance_repaints_background_and_native_code_without_rebinding(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "runtime_palette_tests::appearance_repaints_background_and_native_code_without_rebinding",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let (abort_tx, abort_rx) = std::sync::mpsc::channel();
    let (owner, native_id, native_epoch) = app.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        let view = app.views.get_mut(&owner).unwrap();
        view.convo.push(
            Role::Assistant,
            "[Background](https://example.com/background) and `code`.",
        );
        view.rows = view.convo.messages().iter().map(MessageRow::new).collect();
        app.new_workshop_with(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None,
            },
            cx,
        );
        let tab = &mut app.work.code_tabs[app.work.code_active];
        tab.history.conv_id = Some("palette-owned-native-history".into());
        *tab.session_slot.lock().unwrap() = Some("palette-owned-native-resume".into());
        tab.project_folder = Some("/Projects/palette-owned-fixture".into());
        tab.convo.push(
            Role::Assistant,
            "[Native](https://example.com/native) and `code`.\n\n[answered by native-model]",
        );
        tab.rows = tab.convo.messages().iter().map(MessageRow::new).collect();
        tab.rows[0].activity = vec!["Native recorded activity".into()];
        tab.stream_abort_tx = Some(abort_tx);
        tab.streaming = true;
        let id = tab.id;
        let epoch = tab.reply_presentation_token.clone();
        app.composer
            .update(cx, |c, cx| c.set_text("Native unsent staging", cx));
        cx.notify();
        (owner, id, epoch)
    });
    window.run_until_parked();
    window.update(|win, cx| {
        app.update(cx, |app, cx| {
            app.choose_color_mode(ColorMode::Light, win, cx)
        })
    });
    window.run_until_parked();
    app.read_with(window, |app, cx| {
        let background = &app.views[&owner].rows[0];
        let tab = &app.work.code_tabs[app.work.code_active];
        assert_eq!(background.plan_palette.canvas, theme::LIGHT.canvas);
        assert_eq!(tab.rows[0].plan_palette.canvas, theme::LIGHT.canvas);
        assert_eq!(tab.id, native_id);
        assert!(tab.reply_presentation_token.matches(&native_epoch));
        assert_eq!(
            tab.history.conv_id.as_deref(),
            Some("palette-owned-native-history")
        );
        assert_eq!(
            tab.session_slot.lock().unwrap().as_deref(),
            Some("palette-owned-native-resume")
        );
        assert_eq!(
            tab.project_folder.as_deref(),
            Some("/Projects/palette-owned-fixture")
        );
        assert!(tab.streaming && tab.stream_abort_tx.is_some());
        assert!(!tab.rows[0].raw.contains("answered by"));
        assert_eq!(
            tab.rows[0].footer.as_deref(),
            Some("Answered by native-model")
        );
        assert_eq!(tab.rows[0].activity[0].as_ref(), "Native recorded activity");
        assert!(tab
            .convo
            .message(0)
            .unwrap()
            .text
            .contains("answered by native-model"));
        assert_eq!(
            app.composer.read(cx).text().to_string(),
            "Native unsent staging"
        );
    });
    assert!(matches!(
        abort_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    app.update(window, |app, cx| {
        let tab = &mut app.work.code_tabs[app.work.code_active];
        tab.streaming = false;
        tab.stream_abort_tx = None;
        cx.notify();
    });
}
