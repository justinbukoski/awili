//! The Workshop top bar (docs/v2/topbar/SPEC.md), driven in real app windows:
//! the bar in both states, its reserved run slot, the more menu, the shortcuts,
//! the composer footer, the panel search and the narrow collapse.
use crate::{consolidation_tests::isolated, work::Mode, AwiliApp};
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
        app.work.folder = Some("/fixture/topbar-project".into());
        let owner = app.sidebar.active_id().to_owned();
        app.conversations.get_mut(&owner).unwrap().cwd = Some("/fixture/topbar-project".into());
        app.work.projects.clear();
    });
    window.simulate_resize(size(px(1600.), px(900.)));
    window.run_until_parked();
    (view, window)
}

fn bounds(window: &mut gpui::VisualTestContext, selector: &str) -> gpui::Bounds<gpui::Pixels> {
    window
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("{selector} is rendered"))
}

fn click(window: &mut gpui::VisualTestContext, selector: &str) {
    let center = bounds(window, selector).center();
    window.simulate_click(center, gpui::Modifiers::default());
    window.run_until_parked();
}

fn start_run(
    view: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
) -> std::sync::mpsc::Receiver<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.work.running = true;
        app.work.run_started = Some(std::time::Instant::now());
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
    rx
}

const ORDER: [&str; 6] = [
    "ui-chip-awili-workshop-project",
    "ui-chip-awili-workshop-title",
    crate::ask_mode_ui::ASK_MODE_CHIP_SELECTOR,
    "ui-button-awili-new-conversation",
    "ui-button-awili-workshop-history-toggle",
    "ui-icon-button-awili-workshop-more",
];

#[gpui::test]
fn bar_renders_idle_and_running_with_the_elements_in_order(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::bar_renders_idle_and_running_with_the_elements_in_order") {
        return;
    }
    let (view, window) = setup(cx);
    let bar = bounds(window, "awili-workshop-bar");
    assert!(bar.size.height <= crate::theme::bar_h() + px(1.), "{bar:?}");
    let slot_idle = bounds(window, "awili-run-slot");
    assert!(
        window.debug_bounds("ui-pill-job-activity-island").is_none(),
        "idle: the slot is empty"
    );
    let mut last = px(-1.);
    for selector in ORDER {
        let b = bounds(window, selector);
        assert!(b.left() >= last - px(0.5), "{selector} is out of order: {b:?}");
        assert!(
            b.top() >= bar.top() - px(0.5) && b.bottom() <= bar.bottom() + px(0.5),
            "{selector} sits inside the bar: {b:?} / {bar:?}"
        );
        last = b.right();
    }
    // The model chip left the bar: it is the composer footer's left end,
    // in front of the status strip, and the capsule's slot sits left of the
    // mode chip.
    let chip = bounds(window, "ui-chip-awili-workshop-helper");
    let footer = bounds(window, "ui-composer-frame-awili-composer-footer");
    assert!(chip.top() >= footer.top() - px(0.5) && chip.bottom() <= footer.bottom() + px(0.5));
    assert!(chip.left() <= bounds(window, crate::strip_app::STRIP_SELECTOR).left());
    assert!(chip.top() > bar.bottom(), "not in the top bar");
    assert!(slot_idle.right() <= bounds(window, ORDER[2]).left() + px(0.5));

    let _rx = start_run(&view, window);
    let slot_running = bounds(window, "awili-run-slot");
    let capsule = bounds(window, "ui-pill-job-activity-island");
    assert_eq!(slot_idle.size.width, slot_running.size.width, "the slot never resizes");
    assert_eq!(slot_idle.origin.x, slot_running.origin.x, "nothing shifts");
    assert!(capsule.left() >= slot_running.left() - px(0.5));
    assert!(capsule.right() <= slot_running.right() + px(0.5));
    // Nothing else moved when the run started.
    let mut last = px(-1.);
    for selector in ORDER {
        let b = bounds(window, selector);
        assert!(b.left() >= last - px(0.5), "{selector} moved out of order: {b:?}");
        last = b.right();
    }
    for selector in ["ui-button-job-island-open", "ui-button-job-island-stop"] {
        assert!(window.debug_bounds(selector).is_some(), "{selector} in the capsule");
    }
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn stop_in_the_capsule_stops_the_run(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::stop_in_the_capsule_stops_the_run") {
        return;
    }
    let (view, window) = setup(cx);
    let rx = start_run(&view, window);
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    click(window, "ui-button-job-island-stop");
    assert!(rx.try_recv().is_ok(), "Stop in the capsule reaches the worker");
}

#[gpui::test]
fn run_capsule_shows_the_plan_step_when_the_bar_is_wide(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::run_capsule_shows_the_plan_step_when_the_bar_is_wide") {
        return;
    }
    let (view, window) = setup(cx);
    window.simulate_resize(size(px(2000.), px(900.)));
    let _rx = start_run(&view, window);
    view.update(window, |app, cx| {
        app.work.panel.steps = vec![
            crate::work::ProgressStep {
                label: "Read".into(),
                state: crate::work::StepState::Done,
            },
            crate::work::ProgressStep {
                label: "Edit".into(),
                state: crate::work::StepState::Running,
            },
            crate::work::ProgressStep {
                label: "Test".into(),
                state: crate::work::StepState::Pending,
            },
        ];
        cx.notify();
    });
    window.run_until_parked();
    let step = bounds(window, "ui-text-job-island-step");
    let slot = bounds(window, "awili-run-slot");
    assert!(step.right() <= slot.right() + px(0.5), "{step:?} in {slot:?}");
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn more_menu_carries_every_conversation_action(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::more_menu_carries_every_conversation_action") {
        return;
    }
    let (view, window) = setup(cx);
    view.update(window, |app, cx| {
        app.work_push(crate::Role::User, "Keep this thread".into());
        app.work_push(crate::Role::Assistant, "Kept.".into());
        cx.notify();
    });
    window.run_until_parked();
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    let item = |window: &mut gpui::VisualTestContext, i: usize| {
        click(window, "ui-icon-button-awili-workshop-more");
        click(window, &format!("ui-row-awili-workshop-menu-item-{i}"));
    };
    // 0 Rename: the panel opens and the row is in rename.
    item(window, 0);
    view.read_with(window, |app, _| {
        assert_eq!(app.sidebar.renaming.as_deref(), Some(owner.as_str()));
        assert!(app.workshop_history_open(px(1600.)));
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    // 1 Copy conversation: the one export path answers (copied, or why not: the
    // fixture has no store, so it says so in a toast).
    view.update(window, |app, cx| app.dismiss_toast(cx));
    item(window, 1);
    assert!(
        view.read_with(window, |app, _| app.toast.visible().is_some()),
        "Copy conversation runs the export path"
    );
    // 4 Delete…: the panel row asks first.
    item(window, 4);
    view.read_with(window, |app, _| {
        assert_eq!(app.pending_delete.as_deref(), Some(owner.as_str()));
        assert!(app.conversations.get(&owner).is_some(), "asked, not deleted");
    });
    view.update(window, |app, _| app.pending_delete = None);
    // 5 Keyboard shortcuts.
    item(window, 5);
    view.read_with(window, |app, _| assert!(app.shortcuts.open));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    // 3 Archive.
    item(window, 3);
    view.read_with(window, |app, _| {
        assert!(
            app.conversations.get(&owner).is_none_or(|c| c.archived),
            "Archive archives the thread"
        );
    });
}

#[gpui::test]
fn more_menu_open_as_code_tab_starts_a_code_thread(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::more_menu_open_as_code_tab_starts_a_code_thread") {
        return;
    }
    let (view, window) = setup(cx);
    click(window, "ui-icon-button-awili-workshop-more");
    click(window, "ui-row-awili-workshop-menu-item-2");
    view.read_with(window, |app, _| {
        assert_eq!(app.mode, Mode::Code, "Open as Code tab lands in a Code tab");
        assert!(!app.work.code_tabs.is_empty());
    });
}

#[gpui::test]
fn shortcuts_dispatch_their_actions(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::shortcuts_dispatch_their_actions") {
        return;
    }
    let (view, window) = setup(cx);
    view.update(window, |app, cx| {
        app.work_push(crate::Role::User, "Shortcut transcript".into());
        cx.notify();
    });
    window.run_until_parked();
    // ⌘\ toggles the Conversations panel.
    let open = |view: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext| {
        view.read_with(window, |app, _| app.workshop_history_open(px(1600.)))
    };
    let before = open(&view, window);
    window.simulate_keystrokes("cmd-\\");
    window.run_until_parked();
    assert_ne!(open(&view, window), before, "⌘\\ toggles the panel");
    window.simulate_keystrokes("cmd-\\");
    window.run_until_parked();
    assert_eq!(open(&view, window), before);
    // ⇧⌘K opens the conversation switcher under the title.
    window.simulate_keystrokes("cmd-shift-k");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.shell.tabs.menu == Some(crate::workshop::WorkshopMenu::Switcher));
        assert!(!app.palette.open, "⌘K's palette is untouched");
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    // ⌥⌘C copies the conversation (the one export path answers in a toast).
    view.update(window, |app, cx| app.dismiss_toast(cx));
    window.simulate_keystrokes("cmd-alt-c");
    window.run_until_parked();
    assert!(
        view.read_with(window, |app, _| app.toast.visible().is_some()),
        "⌥⌘C runs the export path"
    );
    // ⌘N starts a new conversation.
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    window.simulate_keystrokes("cmd-n");
    window.run_until_parked();
    assert_ne!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner
    );
    // ⌘. stops the running reply.
    let rx = start_run(&view, window);
    window.simulate_keystrokes("cmd-.");
    window.run_until_parked();
    assert!(rx.try_recv().is_ok(), "⌘. stops the run");
    // Existing chords keep their meaning: ⇧⌘C saves, ⌘K is the palette.
    window.simulate_keystrokes("cmd-k");
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.palette.open));
}

#[gpui::test]
fn composer_footer_is_the_status_strip_and_has_no_model_mode_or_hints(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::composer_footer_is_the_status_strip_and_has_no_model_mode_or_hints") {
        return;
    }
    let (view, window) = setup(cx);
    let footer = bounds(window, "ui-composer-frame-awili-composer-footer");
    for gone in [
        "ui-chip-model-chip",
        "ui-chip-composer-status",
        "ui-chip-ask-mode",
        crate::ask_mode_ui::ASK_MODE_CHIP_SELECTOR,
    ] {
        let b = window.debug_bounds(gone);
        assert!(
            b.is_none_or(|b| b.top() < footer.top()),
            "{gone} is not in the composer"
        );
    }
    // The context figure shows once a reply has recorded how its prompt was made.
    assert_eq!(view.read_with(window, |app, _| app.composer_context_parts()), None);
    view.update(window, |app, cx| {
        app.work_push(crate::Role::Assistant, "Reply".into());
        let owner = app.sidebar.active_id().to_string();
        let row = app.views.get_mut(&owner).unwrap().rows.last_mut().unwrap();
        let mut breakdown = crate::context_breakdown::ContextBreakdown::default();
        breakdown.settle(Some(12_400), Some((1_000_000, true)), false);
        row.context = Some(breakdown);
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, _| app.composer_context_parts()),
        Some((12_400, 1_000_000, true))
    );
    assert!(window.debug_bounds("ui-text-composer-context-use").is_some());
    // The strip says it as "12.4K of 1M" (no trailing word), and the key hints
    // are gone from the footer.
    assert!(window.debug_bounds(crate::strip_app::STRIP_SELECTOR).is_some());
    let right = view.read_with(window, |app, _| {
        let (_, v) = app.strip_view(std::time::Instant::now()).unwrap();
        v.right.iter().map(|s| s.text.clone()).collect::<Vec<_>>().join(" ")
    });
    assert_eq!(right, "12.4K of 1M");
    assert!(window.debug_bounds("ui-text-composer-hint-send").is_none());
}

#[gpui::test]
fn panel_search_filters_the_list_by_title(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::panel_search_filters_the_list_by_title") {
        return;
    }
    let (view, window) = setup(cx);
    let (alpha, beta) = view.update(window, |app, cx| {
        let alpha = app.create_conversation(awili_core::ConvKind::Work);
        app.conversations.get_mut(&alpha).unwrap().title = "Alpha release notes".into();
        let beta = app.create_conversation(awili_core::ConvKind::Work);
        app.conversations.get_mut(&beta).unwrap().title = "Beta migration".into();
        cx.notify();
        (alpha, beta)
    });
    window.run_until_parked();
    let row = |id: &str| format!("awili-history-row-{id}");
    assert!(window.debug_bounds(Box::leak(row(&alpha).into_boxed_str())).is_some());
    assert!(window.debug_bounds(Box::leak(row(&beta).into_boxed_str())).is_some());
    assert!(
        window.debug_bounds("ui-button-sidebar-new").is_none(),
        "the panel's duplicate New row is gone"
    );
    click(window, "awili-history-search");
    window.simulate_input("alph");
    window.run_until_parked();
    assert!(window.debug_bounds(Box::leak(row(&alpha).into_boxed_str())).is_some());
    assert!(
        window.debug_bounds(Box::leak(row(&beta).into_boxed_str())).is_none(),
        "a title that does not match is filtered out"
    );
    view.read_with(window, |app, _| assert_eq!(app.shell.tabs.history_query, "alph"));
}

#[gpui::test]
fn bar_collapses_to_short_forms_at_980(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::bar_collapses_to_short_forms_at_980") {
        return;
    }
    let (view, window) = setup(cx);
    let wide_project = bounds(window, "ui-chip-awili-workshop-project").size.width;
    let wide_new = bounds(window, "ui-button-awili-new-conversation").size.width;
    let wide_slot = bounds(window, "awili-run-slot").size.width;
    let wide_mode = bounds(window, crate::ask_mode_ui::ASK_MODE_CHIP_SELECTOR).size.width;
    let bar_idle_h = bounds(window, "awili-workshop-bar").size.height;
    window.simulate_resize(size(px(980.), px(800.)));
    window.run_until_parked();
    let narrow_project = bounds(window, "ui-chip-awili-workshop-project").size.width;
    let narrow_new = bounds(window, "ui-button-awili-new-conversation").size.width;
    assert!(narrow_project < wide_project, "project collapses to its icon {narrow_project:?} {wide_project:?}");
    assert!(narrow_new < wide_new, "New collapses to +");
    assert!(bounds(window, "awili-run-slot").size.width < wide_slot, "the slot narrows");
    // Everything is still there and still inside the window, the title included.
    let canvas = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(980.), px(800.)));
    for selector in ORDER {
        let b = bounds(window, selector);
        assert!(
            b.left() >= canvas.left() && b.right() <= canvas.right(),
            "{selector} stays on screen: {b:?}"
        );
    }
    // The mode folds to its short form (the full words stay in its name).
    assert!(
        bounds(window, crate::ask_mode_ui::ASK_MODE_CHIP_SELECTOR).size.width < wide_mode,
        "the mode chip folds"
    );
    // A running job in another thread, with a long title, still fits the slot.
    long_title_run(&view, window);
    let slot = bounds(window, "awili-run-slot");
    let capsule = bounds(window, "ui-pill-job-activity-island");
    assert!(capsule.left() >= slot.left() - px(0.5) && capsule.right() <= slot.right() + px(0.5));
    assert_eq!(bounds(window, "awili-workshop-bar").size.height, bar_idle_h);
    for selector in ORDER {
        let b = bounds(window, selector);
        assert!(b.right() <= canvas.right(), "{selector} stays on screen with a run: {b:?}");
    }
}

/// Another conversation owns a running job and its title is far too long.
fn long_title_run(view: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext) {
    view.update(window, |app, cx| {
        let other = app.create_conversation(awili_core::ConvKind::Work);
        app.conversations.get_mut(&other).unwrap().title =
            "A very long conversation title that cannot possibly fit inside the run capsule slot".into();
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        app.work.conv_id = Some(other);
        app.work.running = true;
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
}

#[gpui::test]
fn capsule_fits_its_slot_with_plan_first_at_wide_and_with_a_long_title(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "topbar_tests::capsule_fits_its_slot_with_plan_first_at_wide_and_with_a_long_title",
    ) {
        return;
    }
    let (view, window) = setup(cx);
    window.simulate_resize(size(px(2000.), px(900.)));
    let bar_idle_h = bounds(window, "awili-workshop-bar").size.height;
    view.update(window, |app, cx| {
        app.pick_work_executor(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None,
            },
            cx,
        );
    });
    let _rx = start_run(&view, window);
    view.update(window, |app, cx| {
        app.work.panel.steps = vec![
            crate::work::ProgressStep {
                label: "a".into(),
                state: crate::work::StepState::Running,
            },
            crate::work::ProgressStep {
                label: "b".into(),
                state: crate::work::StepState::Pending,
            },
        ];
        cx.notify();
    });
    window.run_until_parked();
    let slot = bounds(window, "awili-run-slot");
    let capsule = bounds(window, "ui-pill-job-activity-island");
    let step = bounds(window, "ui-text-job-island-step");
    assert!(capsule.right() <= slot.right() + px(0.5), "{capsule:?} in {slot:?}");
    assert!(step.right() <= capsule.right(), "step and posture stay inside the capsule");
    for selector in ["ui-button-job-island-open", "ui-button-job-island-stop"] {
        let b = bounds(window, selector);
        assert!(b.right() <= capsule.right() + px(0.5), "{selector} inside the capsule");
    }
    assert_eq!(bounds(window, "awili-workshop-bar").size.height, bar_idle_h);
    // Cross-conversation, long title: the label gives way, the actions stay.
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
    long_title_run(&view, window);
    let slot = bounds(window, "awili-run-slot");
    let capsule = bounds(window, "ui-pill-job-activity-island");
    assert!(capsule.left() >= slot.left() - px(0.5) && capsule.right() <= slot.right() + px(0.5));
    assert!(bounds(window, "ui-button-job-island-open").right() <= capsule.right() + px(0.5));
    assert_eq!(bounds(window, "awili-workshop-bar").size.height, bar_idle_h);
}

#[gpui::test]
fn bar_controls_stay_inside_the_bar_at_max_zoom(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::bar_controls_stay_inside_the_bar_at_max_zoom") {
        return;
    }
    let (view, window) = setup(cx);
    let _rx = start_run(&view, window);
    window.simulate_resize(size(px(900.), px(712.)));
    view.update_in(window, |app, win, cx| app.apply_ui_scale(2.5, win, cx));
    window.run_until_parked();
    let bar = bounds(window, "awili-workshop-bar");
    let canvas = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(900.), px(712.)));
    for selector in ORDER
        .into_iter()
        .chain(["ui-pill-job-activity-island", "ui-button-job-island-stop"])
    {
        let b = bounds(window, selector);
        assert!(
            b.left() >= bar.left() - px(0.5)
                && b.right() <= bar.right() + px(0.5)
                && b.top() >= bar.top() - px(0.5)
                && b.bottom() <= bar.bottom() + px(0.5),
            "{selector} inside the bar at 2.5x: {b:?} / {bar:?}"
        );
        assert!(b.right() <= canvas.right() + px(0.5), "{selector} on screen");
    }
    crate::theme::set_ui_scale(1.);
}

#[gpui::test]
fn panel_filter_never_hides_the_thread_the_menu_acts_on(cx: &mut gpui::TestAppContext) {
    if !isolated("topbar_tests::panel_filter_never_hides_the_thread_the_menu_acts_on") {
        return;
    }
    let (view, window) = setup(cx);
    // A thread with something in it, so ⌘N starts a new one rather than reusing it.
    view.update(window, |app, cx| {
        app.work_push(crate::Role::User, "Something to keep".into());
        cx.notify();
    });
    window.run_until_parked();
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    let row = |id: &str| -> &'static str {
        Box::leak(format!("awili-history-row-{id}").into_boxed_str())
    };
    click(window, "awili-history-search");
    window.simulate_input("zzzz");
    window.run_until_parked();
    assert!(window.debug_bounds(row(&owner)).is_some(), "the open thread stays");
    // Rename and Delete from the more menu act on a visible row.
    click(window, "ui-icon-button-awili-workshop-more");
    click(window, "ui-row-awili-workshop-menu-item-0");
    assert!(window.debug_bounds(row(&owner)).is_some());
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    click(window, "ui-icon-button-awili-workshop-more");
    click(window, "ui-row-awili-workshop-menu-item-4");
    assert!(window.debug_bounds(row(&owner)).is_some(), "the confirm row shows");
    view.update(window, |app, _| app.pending_delete = None);
    // ⌘N's new conversation is visible too.
    window.simulate_keystrokes("cmd-n");
    window.run_until_parked();
    let fresh = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    assert_ne!(fresh, owner);
    assert!(window.debug_bounds(row(&fresh)).is_some(), "the new thread is listed");
    // Hiding the panel forgets the filter.
    window.simulate_keystrokes("cmd-\\");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.shell.tabs.history_query, ""));
    // ⌘\ from the search field lands the keyboard in the composer.
    assert!(view.update_in(window, |app, win, cx| app
        .composer
        .read(cx)
        .focus_handle()
        .is_focused(win)));
}

#[gpui::test]
fn panel_filter_drops_empty_groups_and_the_switcher_lists_the_open_thread(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "topbar_tests::panel_filter_drops_empty_groups_and_the_switcher_lists_the_open_thread",
    ) {
        return;
    }
    let (view, window) = setup(cx);
    view.update(window, |app, cx| {
        for (title, cwd) in [("Gamma", "/fixture/gc"), ("Delta", "/fixture/gd")] {
            let id = app.create_conversation(awili_core::ConvKind::Work);
            let meta = app.conversations.get_mut(&id).unwrap();
            meta.title = title.into();
            meta.cwd = Some(cwd.into());
        }
        cx.notify();
    });
    window.run_until_parked();
    assert!(window_heading(window, "gc").is_some() && window_heading(window, "gd").is_some());
    click(window, "awili-history-search");
    window.simulate_input("gamm");
    window.run_until_parked();
    assert!(window_heading(window, "gc").is_some());
    assert!(window_heading(window, "gd").is_none(), "a group with no match has no heading");
    // The switcher lists what the panel does, and the open thread is in it.
    window.simulate_keystrokes("cmd-shift-k");
    window.run_until_parked();
    let open = view.read_with(window, |app, _| app.workshop_active_conversation());
    assert!(open.is_some());
    assert!(window.debug_bounds("ui-row-awili-workshop-menu-item-0").is_some());
}

fn window_heading(
    window: &mut gpui::VisualTestContext,
    label: &str,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    window.debug_bounds(Box::leak(
        format!("{}{label}", crate::sidebar::GROUP_SELECTOR).into_boxed_str(),
    ))
}
