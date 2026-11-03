//! Render and drive the actual app: panel routes, tab selection, compact
//! bounds, and Escape before cancellation of a real worker channel.
use crate::{consolidation_tests::isolated, job_panel::JobTab, work::Mode, AwiliApp};
use gpui::{px, size};

fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = window.debug_bounds(selector).expect(selector);
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
}

fn setup(cx: &mut gpui::TestAppContext) -> (gpui::Entity<AwiliApp>, &mut gpui::VisualTestContext) {
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = None;
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.set_mode(Mode::Work, cx);
    });
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();
    (view, window)
}

#[gpui::test]
fn job_panel_routes_open_and_close_without_stopping(cx: &mut gpui::TestAppContext) {
    if !isolated("job_panel_tests::job_panel_routes_open_and_close_without_stopping") {
        return;
    }
    let (view, window) = setup(cx);
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.work.running = true;
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("awili-work-panel").is_none(),
        "starting a job must not open details"
    );
    assert!(window.debug_bounds("ui-pill-job-activity-island").is_some());
    // The capsule's Job and the rail open it; the composer no longer repeats
    // the run state.
    assert!(window.debug_bounds("ui-chip-composer-status").is_none());
    for selector in ["ui-button-job-island-open", "awili-rail-item-0"] {
        click(window, selector);
        assert!(view.read_with(window, |app, _| app.shell.job_open));
        window.update(|win, cx| view.update(cx, |app, cx| app.focus_composer(win, cx)));
        window.simulate_keystrokes("escape");
        window.run_until_parked();
        assert!(!view.read_with(window, |app, _| app.shell.job_open));
        assert!(view.read_with(window, |app, _| app.work.running));
        assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        assert!(window.update(|win, cx| view
            .read(cx)
            .composer
            .read(cx)
            .focus_handle()
            .is_focused(win)));
    }
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-j"));
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.shell.job_open));
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-j"));
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.shell.job_open));
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn job_panel_tabs_and_footer_fit_compact_zoom(cx: &mut gpui::TestAppContext) {
    if !isolated("job_panel_tests::job_panel_tabs_and_footer_fit_compact_zoom") {
        return;
    }
    let (view, window) = setup(cx);
    for scale in [0.9_f32, 1.0, 1.25, 1.5] {
        crate::theme::set_ui_scale(scale);
        for (w, h) in [(1400., 900.), (860., 600.), (800., 480.)] {
            window.simulate_resize(size(px(w), px(h)));
            view.update(window, |app, cx| {
                app.shell.job_open = true;
                app.shell.job_tab = JobTab::History;
                cx.notify();
            });
            window.run_until_parked();
            for (selector, tab) in [
                ("ui-tab-job-tab-0", JobTab::History),
                ("ui-tab-job-tab-1", JobTab::Terminal),
                ("ui-tab-job-tab-2", JobTab::Previews),
                ("ui-tab-job-tab-3", JobTab::Context),
            ] {
                if tab == JobTab::History {
                    click(window, selector);
                } else {
                    window.simulate_keystrokes("right");
                    window.run_until_parked();
                }
                let strip = window
                    .debug_bounds("ui-tabs-job-panel-tabs")
                    .unwrap_or_else(|| panic!("strip disappeared {tab:?} {w}/{h}/{scale}"));
                let b = window.debug_bounds(selector).unwrap();
                assert!(
                    b.left() >= strip.left() && b.right() <= strip.right(),
                    "selected {selector} must be visible at {w}/{h}/{scale}: {b:?} in {strip:?}"
                );
                assert!(
                    b.size.width >= px(44.) && b.size.height >= px(44.),
                    "{selector}: {b:?}"
                );
                assert_eq!(view.read_with(window, |app, _| app.shell.job_tab), tab);
                let footer = window.debug_bounds("ui-button-work-pick").unwrap();
                assert!(
                    footer.bottom() <= px(h) && footer.top() >= px(0.),
                    "footer stays on screen at {w}/{h}/{scale}: {footer:?}"
                );
                assert!(footer.size.height >= px(44.));
                assert_eq!(
                    window.debug_bounds("ui-card-job-terminal-empty").is_some(),
                    tab == JobTab::Terminal
                );
                assert_eq!(
                    window.debug_bounds("ui-card-job-previews-empty").is_some(),
                    tab == JobTab::Previews
                );
            }
            view.update(window, |app, cx| {
                app.shell.job_open = false;
                cx.notify();
            });
            window.run_until_parked();
        }
    }
    crate::theme::set_ui_scale(1.0);
}

#[gpui::test]
fn job_panel_escape_menu_precedes_panel_and_stop(cx: &mut gpui::TestAppContext) {
    if !isolated("job_panel_tests::job_panel_escape_menu_precedes_panel_and_stop") {
        return;
    }
    let (view, window) = setup(cx);
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.shell.job_open = true;
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.work.running = true;
        app.work.agent_abort_tx = Some(tx);
        app.shell.more = true;

        cx.notify();
    });
    window.run_until_parked();
    window.update(|win, cx| {
        let handle = view.read(cx).shell.menu_focus.clone();
        win.focus(&handle, cx);
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.shell.more));
    assert!(view.read_with(window, |app, _| app.shell.job_open));
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.shell.job_open));
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn job_panel_unfocused_rename_does_not_turn_escape_into_stop(cx: &mut gpui::TestAppContext) {
    if !isolated("job_panel_tests::job_panel_unfocused_rename_does_not_turn_escape_into_stop") {
        return;
    }
    let (view, window) = setup(cx);
    // Too narrow to dock the Conversations panel or the Job beside the composer:
    // both are sheets, and a Job sheet replaces the Conversations sheet.
    window.simulate_resize(gpui::size(gpui::px(1000.), gpui::px(800.)));
    window.run_until_parked();
    let (tx, rx) = std::sync::mpsc::channel();
    window.update(|win, cx| {
        view.update(cx, |app, cx| {
            app.work.conv_id = Some(app.sidebar.active_id().into());
            app.work.running = true;
            app.work.agent_abort_tx = Some(tx);
            app.shell.conversations = true;
            let id = app.sidebar.active_id().to_string();
            app.begin_rename(&id, win, cx);
        })
    });
    window.run_until_parked();
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-j"));
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.shell.job_open));
    assert!(view.read_with(window, |app, _| app.sidebar.renaming.is_some()));
    // Opening the Job sheet already replaced the Conversations sheet, so Escape
    // closes the Job: it ends the sheet, never the run.
    assert!(!view.read_with(window, |app, _| app.shell.conversations));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.shell.job_open));
    assert!(view.read_with(window, |app, _| app.work.running));
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn job_panel_first_job_does_not_open_the_retained_panel(cx: &mut gpui::TestAppContext) {
    if !isolated("job_panel_tests::job_panel_first_job_does_not_open_the_retained_panel") {
        return;
    }
    let mut config = awili_config::Config::networkless();
    config.work.default_executor = Some("agent:codex".into());
    awili_config::write_config(&config, &awili_config::Config::config_paths()[0]).unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    let id = view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        // A new Work thread starts on the configured project helper.
        let (id, _) = app.fresh_conversation(awili_core::convs::ConvKind::Work);
        app.set_active_conversation(&id);
        app.set_mode(Mode::Work, cx);
        app.sync_composer_draft(cx);
        app.sync_workshop_tabs();
        app.work.agents.claude_bin = Some("/no-such-helper".into());
        app.work.agents.codex_bin = Some("/no-such-helper".into());
        app.conversations.get_mut(&id).unwrap().cwd = Some(std::env::var("HOME").unwrap());
        id
    });
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("Fix JOB_OPEN_UNIQUE", cx));
        app.submit_composer_draft(cx);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        window.run_until_parked();
        if view.read_with(window, |app, _| {
            app.mode == Mode::Work && app.sidebar.active_id() == id
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "job promotion did not pass save acknowledgement"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        view.read_with(window, |app, _| app.work.chip_picked.get(&id).cloned()),
        Some(crate::work::WorkExecutor::Agent {
            engine: awili_code::CodeEngine::Codex,
            model: None,
        }),
        "a first task uses the configured project helper"
    );
    assert!(!view.read_with(window, |app, _| app.shell.job_open));
    assert!(
        window.debug_bounds("awili-work-panel").is_none(),
        "first job must not open panel implicitly"
    );
    assert!(window.debug_bounds("ui-card-composer-details").is_none());
}

#[gpui::test]
fn job_panel_owner_go_and_all_stop_routes_use_the_existing_worker(cx: &mut gpui::TestAppContext) {
    if !isolated("job_panel_tests::job_panel_owner_go_and_all_stop_routes_use_the_existing_worker")
    {
        return;
    }
    let (view, window) = setup(cx);
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        let other = app.create_conversation(awili_core::convs::ConvKind::Work);
        app.set_active_conversation(&other);
        app.set_mode(Mode::Work, cx);
        app.work.conv_id = Some(owner.clone());
        app.work.running = true;
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("ui-button-job-island-stop").is_none(),
        "another thread shows Go, not Stop"
    );
    click(window, "ui-button-job-island-open");
    assert_eq!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner
    );
    assert!(view.read_with(window, |app, _| app.shell.job_open));
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    let stop = window.debug_bounds("ui-button-work-cancel").unwrap();
    assert!(stop.size.width >= px(44.) && stop.size.height >= px(44.));
    click(window, "ui-button-work-cancel");
    assert_eq!(rx.try_recv(), Ok(()));
    assert!(!view.read_with(window, |app, _| app.work.running));
    for route in ["island", "escape"] {
        let (tx, rx) = std::sync::mpsc::channel();
        view.update(window, |app, cx| {
            app.shell.job_open = false;
            app.work.running = true;
            app.work.agent_abort_tx = Some(tx);
            app.work
                .cancel
                .store(false, std::sync::atomic::Ordering::SeqCst);
            cx.notify();
        });
        window.run_until_parked();
        if route == "island" {
            click(window, "ui-button-job-island-stop");
        } else {
            window.update(|win, cx| view.update(cx, |app, cx| app.focus_composer(win, cx)));
            window.simulate_keystrokes("escape");
            window.run_until_parked();
        }
        assert_eq!(rx.try_recv(), Ok(()), "{route} must reach worker Stop");
        assert!(!view.read_with(window, |app, _| app.work.running));
    }
}

#[gpui::test]
fn job_panel_more_dismiss_layer_prevents_clicking_stop_behind_menu(cx: &mut gpui::TestAppContext) {
    if !isolated("job_panel_tests::job_panel_more_dismiss_layer_prevents_clicking_stop_behind_menu")
    {
        return;
    }
    let (view, window) = setup(cx);
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.shell.job_open = true;
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.work.running = true;
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
    click(window, "awili-rail-item-4");
    assert!(view.read_with(window, |app, _| app.shell.more));
    click(window, "ui-button-work-cancel");
    assert!(
        !view.read_with(window, |app, _| app.shell.more),
        "outside click dismisses More before acting on job controls"
    );
    assert!(view.read_with(window, |app, _| app.work.running));
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn job_panel_review_reveals_blocked_owner_from_another_thread(cx: &mut gpui::TestAppContext) {
    if !isolated("job_panel_tests::job_panel_review_reveals_blocked_owner_from_another_thread") {
        return;
    }
    let (view, window) = setup(cx);
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    view.update(window, |app, cx| {
        let other = app.create_conversation(awili_core::convs::ConvKind::Work);
        app.set_active_conversation(&other);
        app.set_mode(Mode::Work, cx);
        app.work.conv_id = Some(owner.clone());
        app.work.running = true;
        // The compact sheet covers the whole window, island included, so it
        // is closed here: Review is reached from the island on another thread.
        app.shell.job_open = false;
        app.work.panel.approval = Some(crate::work::ApprovalCard {
            approval_id: "job-review-fixture".into(),
            kind: "fixture".into(),
            prompt: "Review the fixture change".into(),
            risk: awili_gateway::events::ApprovalRisk::Privileged,
            delivery: Default::default(),
        });
        cx.notify();
    });
    window.simulate_resize(size(px(860.), px(600.)));
    window.run_until_parked();
    click(window, "ui-button-job-island-open");
    assert_eq!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner
    );
    assert!(!view.read_with(window, |app, _| app.shell.job_open));
    assert!(
        window.debug_bounds("awili-work-panel").is_none(),
        "Review must remove the modal covering the blocking card"
    );
    assert!(
        window.debug_bounds("work-allow-once").is_some(),
        "the owner approval remains inline"
    );
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.panel.approval = None;
    });
}
