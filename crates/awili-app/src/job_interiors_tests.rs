//! Real app windows: owned Job content, preview actions, and live motion changes.
use crate::{
    consolidation_tests::isolated,
    job_panel::JobTab,
    work::{Mode, ProgressStep, RowKind, StepState, ToolCall, ToolCallStatus},
    AwiliApp,
};
use gpui::{px, size};

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
        app.shell.job_open = true;
        cx.notify();
    });
    window.simulate_resize(size(px(1400.), px(1100.)));
    window.run_until_parked();
    (view, window)
}
fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = window.debug_bounds(selector).expect(selector);
    let point = if selector.starts_with("ui-section-") {
        gpui::point(
            bounds.center().x,
            bounds.top() + crate::ui::Section::header_height() / 2.,
        )
    } else {
        bounds.center()
    };
    window.simulate_click(point, gpui::Modifiers::default());
    window.run_until_parked();
}
fn drawn(
    window: &mut gpui::VisualTestContext,
    row: &'static str,
    text: &str,
) -> gpui::Bounds<gpui::Pixels> {
    let bounds = window.debug_bounds(row).expect(row);
    crate::ui::text::DRAWN_TEXTS
        .with(|d| {
            d.borrow().iter().find_map(|l| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (l.text(), l.bounds())))
                    .ok()
                    .filter(|(t, b)| t == text && bounds.contains(&b.center()))
                    .map(|(_, b)| b)
            })
        })
        .unwrap_or_else(|| panic!("Complete owned text {text:?} was not painted inside {row}"))
}
#[gpui::test]
fn job_interiors_cold_folder_restore_does_not_wait_for_watcher_start(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "job_interiors_tests::job_interiors_cold_folder_restore_does_not_wait_for_watcher_start",
    ) {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("restored.txt"), "file").unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    store
        .set_app_state(
            crate::persistence::WORK_FOLDER_KEY,
            dir.path().to_str().unwrap(),
        )
        .unwrap();
    drop(store);
    let (started_tx, started) = std::sync::mpsc::channel();
    let (release_tx, release) = std::sync::mpsc::channel();
    let (finished_tx, finished) = std::sync::mpsc::channel();
    crate::workspace::override_watcher_start(
        dir.path().into(),
        Box::new(move |_| {
            started_tx.send(()).unwrap();
            release
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            finished_tx.send(()).unwrap();
        }),
    );
    cx.executor().allow_parking();
    let before = std::time::Instant::now();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    assert!(before.elapsed() < std::time::Duration::from_secs(1));
    started
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = None;
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.set_mode(Mode::Work, cx);
        app.shell.job_open = true;
        cx.notify();
    });
    window.simulate_resize(size(px(1400.), px(1100.)));
    window.run_until_parked();
    click(window, "ui-tab-job-tab-2");
    assert_eq!(
        view.read_with(window, |app, _| app.shell.job_tab),
        JobTab::Previews
    );
    view.read_with(window, |app, _| {
        assert_eq!(app.work.folder.as_deref(), Some(dir.path()));
        assert!(app
            .work
            .folder_files
            .iter()
            .any(|name| name == "restored.txt"));
    });
    release_tx.send(()).unwrap();
    finished
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
}
#[gpui::test]
fn job_interiors_folder_change_keeps_window_responsive_during_watcher_start(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("job_interiors_tests::job_interiors_folder_change_keeps_window_responsive_during_watcher_start") {
        return;
    }
    let (view, window) = setup(cx);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("visible.txt"), "file").unwrap();
    let (started_tx, started) = std::sync::mpsc::channel();
    let (release_tx, release) = std::sync::mpsc::channel();
    let (finished_tx, finished) = std::sync::mpsc::channel();
    crate::workspace::override_watcher_start(
        dir.path().into(),
        Box::new(move |_| {
            started_tx.send(()).unwrap();
            release
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            finished_tx.send(()).unwrap();
        }),
    );
    let before = std::time::Instant::now();
    view.update(window, |app, cx| app.set_work_folder(dir.path().into(), cx));
    assert!(before.elapsed() < std::time::Duration::from_secs(1));
    started
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    window.run_until_parked();
    click(window, "ui-tab-job-tab-1");
    assert_eq!(
        view.read_with(window, |app, _| app.shell.job_tab),
        JobTab::Terminal
    );
    assert!(window.debug_bounds("r1h-terminal-open").is_some());
    view.read_with(window, |app, _| {
        assert_eq!(app.work.folder.as_deref(), Some(dir.path()));
        assert!(app
            .work
            .folder_files
            .iter()
            .any(|name| name == "visible.txt"));
    });
    release_tx.send(()).unwrap();
    finished
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
}
#[gpui::test]
fn job_interiors_history_keeps_full_steps_and_tool_results(cx: &mut gpui::TestAppContext) {
    if !isolated("job_interiors_tests::job_interiors_history_keeps_full_steps_and_tool_results") {
        return;
    }
    let (view, window) = setup(cx);
    let title = "A complete long progress label including café \u{1f30a} ".repeat(4);
    let detail = "Full tool result including all output before FINAL TOOL RESULT.".repeat(4);
    crate::ui::text::DRAWN_TEXTS.with(|d| d.borrow_mut().clear());
    view.update(window, |app, cx| {
        app.work.conv_id = Some(app.sidebar.active_id().to_string());
        app.work.panel.steps = vec![ProgressStep {
            label: title.clone(),
            state: StepState::Done,
        }];
        app.work.panel.tool_calls = vec![ToolCall {
            name: "read_file".into(),
            message: Some(detail.clone()),
            status: ToolCallStatus::Failed,
            kind: RowKind::Tool,
        }];
        cx.notify();
    });
    window.run_until_parked();
    let step = drawn(window, "ui-row-job-step-0", &title);
    assert!(step.size.height > crate::theme::z(crate::theme::type_::UI.line));
    let sub = drawn(window, "ui-row-job-tool-0", &detail);
    assert!(sub.size.height > crate::theme::z(crate::theme::type_::LABEL.line));
    drawn(window, "ui-row-job-step-0", "Finished");
    drawn(window, "ui-row-job-tool-0", "\u{2717}");
}
#[gpui::test]
fn job_interiors_preview_opens_the_captured_root_and_full_url(cx: &mut gpui::TestAppContext) {
    if !isolated("job_interiors_tests::job_interiors_preview_opens_the_captured_root_and_full_url")
    {
        return;
    }
    let (view, window) = setup(cx);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("original café folder");
    std::fs::create_dir(&root).unwrap();
    let other = dir.path().join("later folder");
    std::fs::create_dir(&other).unwrap();
    let file = "résumé with spaces.txt";
    let url = "https://example.org/a-complete-long-preview-path?value=caf%C3%A9&tail=RETAINED";
    view.update(window, |app, cx| {
        app.work.conv_id = Some(app.sidebar.active_id().to_string());
        app.work.preview_root = Some(root.clone());
        app.work.touched_files = vec![file.into()];
        app.work.referenced_urls = vec![url.into()];
        app.work.folder = Some(other.clone());
        app.shell.job_tab = JobTab::Previews;
        cx.notify();
    });
    std::fs::remove_dir(&root).unwrap();
    window.run_until_parked();
    crate::work::OPENED_JOB_PREVIEWS.with(|o| o.borrow_mut().clear());
    click(window, "ui-row-job-preview-file-0");
    click(window, "ui-row-job-preview-link-0");
    crate::work::OPENED_JOB_PREVIEWS.with(|o| {
        assert_eq!(
            &*o.borrow(),
            &[root.join(file).display().to_string(), url.to_string()]
        )
    });
    assert_eq!(
        view.read_with(window, |app, _| app.work.folder.clone()),
        Some(other)
    );
}
#[gpui::test]
fn job_interiors_preview_has_a_reason_without_a_captured_root(cx: &mut gpui::TestAppContext) {
    if !isolated("job_interiors_tests::job_interiors_preview_has_a_reason_without_a_captured_root")
    {
        return;
    }
    let (view, window) = setup(cx);
    view.update(window, |app, cx| {
        app.work.conv_id = Some(app.sidebar.active_id().to_string());
        app.work.touched_files = vec!["relative.txt".into()];
        app.work.preview_root = None;
        app.shell.job_tab = JobTab::Previews;
        cx.notify();
    });
    window.run_until_parked();
    crate::work::OPENED_JOB_PREVIEWS.with(|o| o.borrow_mut().clear());
    click(window, "ui-row-job-preview-file-0");
    crate::work::OPENED_JOB_PREVIEWS.with(|o| assert!(o.borrow().is_empty()));
    assert_eq!(
        crate::ui::interaction::test_util::tooltip_at(window, "ui-row-job-preview-file-0")
            .as_deref(),
        Some("This job has no working folder for this relative path")
    );
}
#[gpui::test]
fn job_interiors_motion_notification_reconciles_the_live_clock(cx: &mut gpui::TestAppContext) {
    if !isolated("job_interiors_tests::job_interiors_motion_notification_reconciles_the_live_clock")
    {
        return;
    }
    let (view, window) = setup(cx);
    crate::ui::motion::set_test_override(true);
    view.update(window, |app, cx| {
        app.shell.job_open = false;
        app.work.running = true;
        app.work.waiting_for_stream_output = true;
        app.work.conv_id = Some(app.sidebar.active_id().to_string());
        let active = app.views.get_mut(app.sidebar.active_id()).unwrap();
        let index = active.convo.len();
        active.convo.push(crate::Role::Assistant, "");
        active
            .rows
            .push(crate::MessageRow::new(active.convo.message(index).unwrap()));
        app.work.stream_row_idx = Some(index);
        app.ensure_thinking_timer(cx);
        cx.notify();
    });
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.thinking_timer_running));
    let halo = window
        .debug_bounds("ui-thinking-dot")
        .expect("real reply paints the kit dot");
    assert_eq!(halo.size.width, crate::theme::z(crate::theme::THINK_HALO));
    crate::ui::motion::publish_test_preference(false);
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.thinking_timer_running));
    window
        .executor()
        .advance_clock(std::time::Duration::from_millis(
            crate::ui::thinking_dot::THINK_STEP_MS,
        ));
    window.run_until_parked();
    assert_eq!(view.read_with(window, |app, _| app.thinking_phase), 1);
    let live_generation = view.read_with(window, |app, _| app.thinking_timer_generation);
    // A delayed "on" event arrives after the current preference is already
    // off. Its stale payload must not stop or replace the active clock.
    crate::ui::motion::publish_test_preference(true);
    crate::ui::motion::set_test_override(false);
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.thinking_timer_running));
    assert_eq!(
        view.read_with(window, |app, _| app.thinking_timer_generation),
        live_generation
    );
    let old = view.read_with(window, |app, _| app.thinking_timer_generation);
    crate::ui::motion::publish_test_preference(true);
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.thinking_timer_running));
    assert!(view.read_with(window, |app, _| app.thinking_timer_generation) > old);
    assert_eq!(crate::activity_pulse(1).size(), crate::theme::THINK_MAX);
    crate::ui::motion::publish_test_preference(false);
    window.run_until_parked();
    window
        .executor()
        .advance_clock(std::time::Duration::from_millis(
            crate::ui::thinking_dot::THINK_STEP_MS,
        ));
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, _| app.thinking_phase),
        2,
        "a superseded tick cannot advance a new clock"
    );
    view.update(window, |app, cx| {
        app.work.running = false;
        cx.notify();
    });
    crate::ui::motion::clear_test_override();
}
#[gpui::test]
fn job_interiors_helpers_keep_the_tree_and_automations_route(cx: &mut gpui::TestAppContext) {
    if !isolated("job_interiors_tests::job_interiors_helpers_keep_the_tree_and_automations_route") {
        return;
    }
    let (view, window) = setup(cx);
    let helper =
        |id: &str, parent: Option<&str>, goal: &str| awili_gateway::subagents::SubagentProgress {
            id: id.into(),
            parent_id: parent.map(str::to_string),
            goal: Some(goal.into()),
            session_id: None,
            model: Some("reported fixture model".into()),
            status: "running".into(),
            task_count: None,
            task_index: None,
            started_at: 1,
            updated_at: 2,
            duration_seconds: Some(1.),
            cost_usd: Some(0.1),
            input_tokens: Some(2),
            output_tokens: Some(3),
            tool_count: Some(4),
            files_read: vec!["a".into()],
            files_written: vec!["b".into()],
            stream: vec![],
            summary: None,
            current_tool: Some("Complete live helper activity".into()),
        };
    view.update(window, |app, cx| {
        app.work.conv_id = Some(app.sidebar.active_id().to_string());
        app.work.subagents = vec![
            helper("parent", None, "Parent task"),
            helper("child", Some("parent"), "Child task"),
        ];
        app.work.agents_open = false;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("ui-row-job-helper-parent").is_none());
    click(window, "ui-section-job-helpers");
    let parent = window.debug_bounds("ui-row-job-helper-parent").unwrap();
    let child = window.debug_bounds("ui-row-job-helper-child").unwrap();
    assert!(child.left() > parent.left() && child.top() >= parent.bottom());
    drawn(window,"ui-row-job-helper-parent","Running · reported fixture model · 4 tools · 1R/1W · 1s · 5 tok · $0.1000 · Complete live helper activity");
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    click(window, "ui-button-job-open-automations");
    assert_eq!(view.read_with(window, |app, _| app.mode), Mode::Cron);
    assert_eq!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner
    );
}
#[gpui::test]
fn job_interiors_workshop_reply_uses_the_same_blue_pulse(cx: &mut gpui::TestAppContext) {
    if !isolated("job_interiors_tests::job_interiors_workshop_reply_uses_the_same_blue_pulse") {
        return;
    }
    let (view, window) = setup(cx);
    crate::ui::motion::set_test_override(false);
    view.update(window, |app, cx| {
        app.set_mode(Mode::Work, cx);
        app.shell.job_open = false;
        let active = app.views.get_mut(app.sidebar.active_id()).unwrap();
        let index = active.convo.len();
        active.convo.push(crate::Role::Assistant, "");
        active
            .rows
            .push(crate::MessageRow::new(active.convo.message(index).unwrap()));
        let conv = app.sidebar.active_id().to_string();
        app.work.running = true;
        app.work.waiting_for_stream_output = true;
        app.work.conv_id = Some(conv);
        app.work.stream_row_idx = Some(index);
        app.ensure_thinking_timer(cx);
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(
        window.debug_bounds("ui-thinking-dot").unwrap().size.width,
        crate::theme::z(crate::theme::THINK_HALO)
    );
    crate::ui::motion::publish_test_preference(true);
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.thinking_timer_running));
    crate::ui::motion::publish_test_preference(false);
    window.run_until_parked();
    window
        .executor()
        .advance_clock(std::time::Duration::from_millis(
            crate::ui::thinking_dot::THINK_STEP_MS,
        ));
    window.run_until_parked();
    assert_eq!(view.read_with(window, |app, _| app.thinking_phase), 1);
    view.update(window, |app, cx| {
        app.work.running = false;
        cx.notify();
    });
    crate::ui::motion::clear_test_override();
}
#[gpui::test]
fn job_interiors_preview_root_is_captured_by_actual_submit(cx: &mut gpui::TestAppContext) {
    if !isolated("job_interiors_tests::job_interiors_preview_root_is_captured_by_actual_submit") {
        return;
    }
    let (view, window) = setup(cx);
    let root = tempfile::tempdir().unwrap();
    view.update(window, |app, cx| {
        app.work.folder = Some(root.path().to_path_buf());
        app.work.preview_root = None;
        // This accepted turn fails at the missing-target preflight, before any
        // dispatcher or model call. It still traverses the real submit reset.
        app.work.executor = crate::work::WorkExecutor::LocalBuild {
            target_id: "missing-fixture-target".into(),
        };
        app.submit_work_run("Check captured preview ownership".into(), cx);
        assert_eq!(app.work.preview_root.as_deref(), Some(root.path()));
        assert!(app.work.panel.error.is_some());
        assert!(!app.work.running);
    });
    window.run_until_parked();
}

#[gpui::test]
fn job_interiors_cold_same_folder_restores_terminal_without_navigation(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "job_interiors_tests::job_interiors_cold_same_folder_restores_terminal_without_navigation",
    ) {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let owner = uuid::Uuid::new_v4().to_string();
    let tab = uuid::Uuid::new_v4().to_string();
    let store = awili_persist::Store::open_default().unwrap();
    store
        .create_conversation_with_kind(&owner, "Restored terminal", 1, "work")
        .unwrap();
    store
        .set_conversation_cwd(&owner, dir.path().to_str())
        .unwrap();
    store
        .set_app_state(
            crate::persistence::WORK_FOLDER_KEY,
            dir.path().to_str().unwrap(),
        )
        .unwrap();
    store
        .set_app_state(crate::persistence::LAST_MODE_KEY, "work")
        .unwrap();
    store.set_app_state(crate::workshop::TABS_KEY, &serde_json::json!({"version":1,"tabs":[{"id":tab,"backing":{"Conversation":owner}}],"active":tab}).to_string()).unwrap();
    drop(store);
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    let terminal = view.read_with(window, |app, cx| {
        assert_eq!(app.mode, Mode::Work);
        assert_eq!(app.sidebar.active_id(), owner);
        assert_eq!(app.work.folder.as_deref(), Some(dir.path()));
        assert!(!app.work.running);
        let terminal = app
            .work
            .terminal_view
            .as_ref()
            .expect("cold same-folder restore must initialize the missing terminal");
        assert_eq!(terminal.read(cx).terminal.cwd, dir.path());
        terminal.entity_id()
    });
    window.simulate_resize(size(px(1400.), px(1100.)));
    window.run_until_parked();
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-j"));
    window.run_until_parked();
    click(window, "ui-tab-job-tab-1");
    assert!(window.debug_bounds("ui-card-job-terminal-empty").is_none());
    assert!(window
        .debug_bounds("ui-terminal-frame-job-terminal")
        .is_some());
    view.update(window, |app, cx| {
        app.restore_work_folder(cx);
        assert_eq!(
            app.work.terminal_view.as_ref().unwrap().entity_id(),
            terminal,
            "same-folder restore preserves the existing terminal"
        );
        app.work.conv_id = Some("different-background-owner".into());
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("ui-card-job-terminal-empty").is_some(),
        "another run's terminal stays private to its owning conversation"
    );
    view.read_with(window, |app, cx| {
        let t = app.work.terminal_view.as_ref().unwrap();
        assert_eq!(t.entity_id(), terminal);
        assert_eq!(t.read(cx).terminal.cwd, dir.path());
    });
}
