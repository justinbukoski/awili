//! Header action focus in the real Workshop: keyboard reach of Job Open/Stop,
//! the speaking composer action at maximum zoom, conditional reflow under a
//! still-focused action, and retired action focus recovery.
use crate::{consolidation_tests::isolated, theme, work::Mode, AwiliApp};
use gpui::{px, size, InputEvent};

const DRAFT: &str = "Header actions keep this owned draft.";
const TRANSCRIPT: &str = "awili-transcript-viewport";
const JOB_OPEN: &str = "ui-button-job-island-open";
const JOB_STOP: &str = "ui-button-job-island-stop";
const TOGGLE: &str = "ui-button-awili-workshop-history-toggle";
const RETRY: &str = "ui-button-awili-workshop-retry-save";
/// Failure guard for one real Shift-Tab cycle; reaching the target or returning
/// to the composer ends the walk before this bound matters.
const MAX_FOCUS_STEPS: usize = 400;
const PANEL: &str = "ui-panel-awili-conversations-panel";

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
    (app, window)
}

fn contains(outer: gpui::Bounds<gpui::Pixels>, inner: gpui::Bounds<gpui::Pixels>) {
    assert!(
        inner.left() >= outer.left() - px(0.1)
            && inner.right() <= outer.right() + px(0.1)
            && inner.top() >= outer.top() - px(0.1)
            && inner.bottom() <= outer.bottom() + px(0.1),
        "{inner:?} escapes {outer:?}"
    );
}

/// Owned transcript long enough to scroll the header away, at 900x712 and 2.50.
fn open_at_max_zoom(app: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext) {
    app.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        let view = app.views.get_mut(&owner).unwrap();
        for _ in 0..12 {
            let index = view.convo.len();
            view.convo.push(
                crate::Role::Assistant,
                "Retained owned answer with enough rows to scroll.",
            );
            view.rows
                .push(crate::MessageRow::new(view.convo.message(index).unwrap()));
        }
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        cx.notify();
    });
    window.simulate_resize(size(px(900.), px(712.)));
    app.update_in(window, |app, win, cx| {
        app.apply_ui_scale(2.5, win, cx);
        app.focus_composer(win, cx);
    });
    window.run_until_parked();
}

/// Synthetic owned run: the owner's Job island offers Job and Stop. The receiver
/// observes exactly what the owned worker is sent.
fn seed_run(
    app: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
) -> std::sync::mpsc::Receiver<()> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.update(window, |app, cx| {
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.work.running = true;
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
    rx
}

fn press_enter(window: &mut gpui::VisualTestContext) {
    window.simulate_keystrokes("enter");
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();
}

/// A window wide enough to dock the Conversations panel beside the composer.
fn dock_history_panel(app: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext) {
    theme::set_ui_scale(1.);
    window.simulate_resize(size(px(1400.), px(800.)));
    window.run_until_parked();
    assert!(
        app.read_with(window, |app, _| app.workshop_history_docked(px(1400.))),
        "the panel docks at this width"
    );
    assert!(
        window.debug_bounds(PANEL).is_some(),
        "the panel is rendered"
    );
}

/// Reach the requested header handle by real Shift-Tab traversal, stopping if
/// the complete current cycle returns to a previously focused identity.
fn reach_header_focus(
    app: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
    index: usize,
    selector: &'static str,
) {
    let composer = app.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });
    let target = app.read_with(window, |app, _| app.workshop_header_focus(index));
    window.update(|win, cx| win.focus(&composer, cx));
    window.run_until_parked();
    let mut visited = vec![composer];
    let mut reached = false;
    for _ in 0..MAX_FOCUS_STEPS {
        window.simulate_keystrokes("shift-tab");
        window.run_until_parked();
        let current = window.update(|win, cx| win.focused(cx));
        if current.as_ref() == Some(&target) {
            reached = true;
            break;
        }
        let current = current.expect("actual focus remains present during Shift-Tab traversal");
        assert!(
            !visited.contains(&current),
            "complete Shift-Tab cycle ended before reaching {selector}"
        );
        visited.push(current);
    }
    assert!(reached, "one complete Shift-Tab cycle reaches {selector}");
}

/// Scrolls the transcript away, then walks real Shift-Tab presses from the
/// composer until `index` takes focus. Asserts the complete visible face.
fn reach(
    app: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
    index: usize,
    selector: &'static str,
) {
    let scroll = app.read_with(window, |app, _| app.active_view().unwrap().scroll.clone());
    app.update_in(window, |app, win, cx| app.focus_composer(win, cx));
    scroll.scroll_to_bottom();
    window.update(|win, _| win.refresh());
    window.run_until_parked();
    assert!(
        scroll.offset().y < px(-200.),
        "transcript is genuinely scrolled away before {selector}"
    );
    let composer = app.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });
    let target = app.read_with(window, |app, _| app.workshop_header_focus(index));
    let mut reached = false;
    for _ in 0..MAX_FOCUS_STEPS {
        window.simulate_keystrokes("shift-tab");
        window.run_until_parked();
        if window.update(|win, _| target.is_focused(win)) {
            reached = true;
            break;
        }
        if window.update(|win, _| composer.is_focused(win)) {
            break;
        }
    }
    assert!(
        reached,
        "one Shift-Tab cycle from the composer reaches {selector}"
    );
    let viewport = window.debug_bounds(TRANSCRIPT).unwrap();
    let face = window
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is rendered"));
    assert!(
        face.top() >= viewport.top() - px(0.1) && face.bottom() <= viewport.bottom() + px(0.1),
        "focused {selector} shows its complete face: {face:?} inside {viewport:?}"
    );
}

#[gpui::test]
fn job_open_and_stop_reach_full_faces_and_act_on_the_owned_job(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_header_action_tests::job_open_and_stop_reach_full_faces_and_act_on_the_owned_job",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.read_with(window, |app, _| app.sidebar.active_id().to_string());
    let rx = seed_run(&app, window);
    open_at_max_zoom(&app, window);
    assert!(
        window.debug_bounds(JOB_OPEN).is_some() && window.debug_bounds(JOB_STOP).is_some(),
        "the owned run offers Job and Stop"
    );

    reach(&app, window, 8, JOB_OPEN);
    press_enter(window);
    assert!(
        app.read_with(window, |app, _| app.shell.job_open),
        "Enter on the reached Job action opens the owned Job"
    );
    assert_eq!(
        app.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner
    );
    assert_eq!(
        rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "opening the Job never stops the run"
    );
    app.update_in(window, |app, win, cx| app.close_job_panel(win, cx));
    window.run_until_parked();

    reach(&app, window, 9, JOB_STOP);
    press_enter(window);
    assert_eq!(
        rx.try_recv(),
        Ok(()),
        "Enter on the reached Stop reaches the owned run's worker"
    );
    assert!(!app.read_with(window, |app, _| app.work.running));
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn speaking_fifth_action_keeps_editor_reply_and_header_reveal_at_max_zoom(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_header_action_tests::speaking_fifth_action_keeps_editor_reply_and_header_reveal_at_max_zoom") {
        return;
    }
    let (app, window) = setup(cx);
    open_at_max_zoom(&app, window);
    app.update(window, |app, cx| {
        app.voice.speaking = true;
        cx.notify();
    });
    window.run_until_parked();

    let canvas = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(900.), px(712.)));
    let frame = window
        .debug_bounds("ui-composer-frame-awili-composer")
        .unwrap();
    contains(canvas, frame);
    let speak = window
        .debug_bounds("ui-icon-button-stop-speak")
        .expect("the fifth speaking action is rendered");
    let reply = window
        .debug_bounds(crate::SEND_OR_STOP_SELECTOR)
        .expect("the reply control is rendered");
    for control in [speak, reply] {
        contains(frame, control);
        contains(canvas, control);
        assert!(control.size.width >= px(44.) && control.size.height >= px(44.));
    }
    let editor = window
        .debug_bounds(crate::plan_gate::COMPOSER_ROW_SELECTOR)
        .unwrap();
    assert!(
        editor.size.width >= theme::target() * 2. && editor.size.height >= theme::target(),
        "usable editor beside the fifth action: {editor:?}"
    );
    contains(frame, editor);

    app.update_in(window, |app, win, cx| app.focus_composer(win, cx));
    window.simulate_input(" Speaking keeps the draft.");
    window.run_until_parked();
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        format!("{} Speaking keeps the draft.", DRAFT)
    );

    // The Conversations toggle is reachable and fully revealed (`reach` checks
    // its face against the transcript viewport) beside the speaking action.
    reach(&app, window, 10, TOGGLE);
    assert!(app.read_with(window, |app, _| app.voice.speaking));
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn conditional_banners_reflow_a_focused_notice_action_into_view_and_wheel_keeps_position(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_header_action_tests::conditional_banners_reflow_a_focused_notice_action_into_view_and_wheel_keeps_position") {
        return;
    }
    let (app, window) = setup(cx);
    let _rx = seed_run(&app, window);
    // A save-failure notice is up, with its Retry focused.
    app.update(window, |app, cx| {
        app.workshop_header_notice_fixture(7);
        cx.notify();
    });
    open_at_max_zoom(&app, window);
    reach(&app, window, 7, RETRY);
    let focused = app.read_with(window, |app, _| app.workshop_header_focus(7));
    let scroll = app.read_with(window, |app, _| app.active_view().unwrap().scroll.clone());
    let (before_face, before_offset) = (window.debug_bounds(RETRY).unwrap(), scroll.offset());

    // Two more conditional notices render above it while Retry stays focused.
    app.update(window, |app, cx| {
        app.shell.tabs.restore_notice = true;
        app.shell.tabs.project_import_notice = Some("owned fixture".into());
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.update(|win, _| focused.is_focused(win)),
        "conditional insertion keeps focus on the existing notice action"
    );
    let moved = window.debug_bounds(RETRY).unwrap();
    assert!(
        moved.top() != before_face.top() || scroll.offset() != before_offset,
        "the notices must actually reflow the focused action: {before_face:?} -> {moved:?}"
    );
    // The bar itself holds still while notices come and go below it.
    assert!(window.debug_bounds(JOB_OPEN).is_some());
    let viewport = window.debug_bounds(TRANSCRIPT).unwrap();
    assert!(
        moved.top() >= viewport.top() - px(0.1) && moved.bottom() <= viewport.bottom() + px(0.1),
        "reflowed focused notice action reveals its new actual face: {moved:?} inside {viewport:?}"
    );

    // Ordinary repaint keeps the reading position; so does a wheel scroll.
    let revealed = scroll.offset();
    app.update(window, |_, cx| cx.notify());
    window.run_until_parked();
    assert_eq!(
        scroll.offset(),
        revealed,
        "ordinary repaint keeps the reading position"
    );
    window.simulate_mouse_move(viewport.center(), None, gpui::Modifiers::default());
    window.run_until_parked();
    let wheel = gpui::ScrollWheelEvent {
        position: viewport.center(),
        // GPUI adds the wheel delta to its negative scroll offset; a positive
        // delta moves back toward the top from the bottom reveal offset.
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(300.))),
        touch_phase: gpui::TouchPhase::Moved,
        ..Default::default()
    };
    let immediate_wheel = window.update(|window, cx| {
        window.dispatch_event(wheel.to_platform_input(), cx);
        scroll.offset()
    });
    window.run_until_parked();
    let wheeled = scroll.offset();
    let island = window.debug_bounds("ui-pill-job-activity-island");
    // The positive delta moves the offset up toward the transcript's top, so
    // the wheel's documented direction is an increase from the bottom reveal.
    assert!(
        wheeled.y > revealed.y,
        "wheel scrolls the transcript toward its top (immediate={immediate_wheel:?}, after paint={wheeled:?}, viewport={viewport:?}, mouse={}, island={island:?}, max={:?})",
        viewport.center(),
        scroll.max_offset()
    );
    app.update(window, |_, cx| cx.notify());
    window.run_until_parked();
    assert_eq!(
        scroll.offset(),
        wheeled,
        "a repaint after wheel keeps the reader's position, not the focused action"
    );
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn returning_to_the_same_header_action_after_scrolling_reveals_it_again(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_header_action_tests::returning_to_the_same_header_action_after_scrolling_reveals_it_again") {
        return;
    }
    let (app, window) = setup(cx);
    let _rx = seed_run(&app, window);
    open_at_max_zoom(&app, window);
    reach(&app, window, 8, JOB_OPEN);
    let scroll = app.read_with(window, |app, _| app.active_view().unwrap().scroll.clone());
    let composer = app.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });
    app.update_in(window, |app, win, cx| app.focus_composer(win, cx));
    scroll.scroll_to_bottom();
    window.update(|window, _| window.refresh());
    window.run_until_parked();
    let viewport = window.debug_bounds(TRANSCRIPT).unwrap();
    let hidden = window.debug_bounds(JOB_OPEN).unwrap();
    assert!(
        hidden.top() < viewport.top() || hidden.bottom() > viewport.bottom(),
        "composer focus and transcript scrolling hide the same action: {hidden:?} / {viewport:?}"
    );
    assert!(window.update(|window, _| composer.is_focused(window)));

    reach_header_focus(&app, window, 8, JOB_OPEN);
    let face = window.debug_bounds(JOB_OPEN).unwrap();
    let viewport = window.debug_bounds(TRANSCRIPT).unwrap();
    assert!(
        face.top() >= viewport.top() - px(0.1) && face.bottom() <= viewport.bottom() + px(0.1),
        "returning to the same action reveals its complete face: {face:?} / {viewport:?}"
    );
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn retired_header_action_focus_lands_on_owned_composer_without_refocus(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_header_action_tests::retired_header_action_focus_lands_on_owned_composer_without_refocus") {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.read_with(window, |app, _| app.sidebar.active_id().to_string());
    let rx = seed_run(&app, window);
    open_at_max_zoom(&app, window);
    let composer = app.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });

    // Stop is activated from the keyboard, so its focused face is what retires.
    reach(&app, window, 9, JOB_STOP);
    press_enter(window);
    assert_eq!(rx.try_recv(), Ok(()), "Stop reaches the owned worker");
    assert!(
        window.debug_bounds(JOB_STOP).is_none(),
        "Stop leaves the header once its run stops"
    );
    assert!(
        window.update(|win, _| composer.is_focused(win)),
        "focus returns to the owned composer with no test refocus"
    );
    window.simulate_input(" After Stop.");
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        format!("{} After Stop.", DRAFT)
    );

    // The owned run finishes elsewhere while its Job action holds focus. This
    // is a simulated state-transition fixture: it clears the run flags directly
    // and does not drive the engine's real completion route.
    let _rx = seed_run(&app, window);
    reach(&app, window, 8, JOB_OPEN);
    app.update(window, |app, cx| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(JOB_OPEN).is_none(),
        "Job leaves the header once the simulated run state clears"
    );
    assert!(
        window.update(|win, _| composer.is_focused(win)),
        "retired Job focus returns to the owned composer after the simulated transition"
    );
    window.simulate_input(" After finish.");
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        format!("{} After Stop. After finish.", DRAFT)
    );
    assert_eq!(
        app.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner,
        "retirement never moves focus to another conversation"
    );
    assert!(!app.read_with(window, |app, _| app.shell.job_open));
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn keyboard_close_restores_selected_row_focus_only_after_a_successful_close(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_header_action_tests::keyboard_close_restores_selected_row_focus_only_after_a_successful_close") {
        return;
    }
    let (app, window) = setup(cx);
    dock_history_panel(&app, window);
    let id = app.read_with(window, |app, _| app.shell.tabs.active.clone().unwrap());
    let close = app.read_with(window, |app, _| app.workshop_header_focus(11));
    window.update(|window, cx| window.focus(&close, cx));
    window.run_until_parked();
    assert!(window.update(|window, _| close.is_focused(window)));
    press_enter(window);
    assert!(
        app.read_with(window, |app, _| !app
            .shell
            .tabs
            .tabs
            .iter()
            .any(|tab| tab.id == id)),
        "Enter closes the focused tab"
    );
    let row_focus = app.read_with(window, |app, _| app.shell.tabs.focus.clone());
    assert!(
        window.update(|window, _| row_focus.is_focused(window)),
        "successful keyboard close transfers focus to the surviving selected row"
    );
}

#[gpui::test]
fn keyboard_close_refusal_keeps_the_owned_tab_and_its_focus(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_header_action_tests::keyboard_close_refusal_keeps_the_owned_tab_and_its_focus",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    dock_history_panel(&app, window);
    let _rx = seed_run(&app, window);
    let id = app.read_with(window, |app, _| app.shell.tabs.active.clone().unwrap());
    let close = app.read_with(window, |app, _| app.workshop_header_focus(11));
    window.update(|window, cx| window.focus(&close, cx));
    window.run_until_parked();
    press_enter(window);
    assert!(
        app.read_with(window, |app, _| app
            .shell
            .tabs
            .tabs
            .iter()
            .any(|tab| tab.id == id)),
        "the busy tab remains open"
    );
    assert!(
        window.update(|window, _| close.is_focused(window)),
        "a refused keyboard close retains focus on its action"
    );
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn history_focus_stays_on_visible_action_across_modes(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_header_action_tests::history_focus_stays_on_visible_action_across_modes")
    {
        return;
    }
    let (app, window) = setup(cx);
    app.update(window, |app, cx| app.set_mode(Mode::Code, cx));
    window.run_until_parked();
    let history = app.read_with(window, |app, _| app.workshop_header_focus(3));
    window.update(|window, cx| window.focus(&history, cx));
    window.run_until_parked();
    assert!(window.update(|window, _| history.is_focused(window)));

    app.update(window, |app, cx| app.set_mode(Mode::Work, cx));
    window.run_until_parked();
    assert!(
        window.update(|window, _| history.is_focused(window)),
        "the Conversations action remains visible and retains its own focus across modes"
    );
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn pointer_close_restores_composer_focus_for_the_surviving_tab(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_header_action_tests::pointer_close_restores_composer_focus_for_the_surviving_tab",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    dock_history_panel(&app, window);
    let (closed_id, next_id) = app.update(window, |app, cx| {
        for index in 0..2 {
            let id = app.create_conversation(awili_core::ConvKind::Work);
            app.conversations.get_mut(&id).unwrap().title = format!("Idle tab {index}");
            app.open_conversation_from_history(&id, cx);
        }
        let active = app.shell.tabs.active.clone().unwrap();
        let index = app
            .shell
            .tabs
            .tabs
            .iter()
            .position(|tab| tab.id == active)
            .unwrap();
        let next = app.shell.tabs.tabs[index - 1].id.clone();
        (active, next)
    });
    window.run_until_parked();
    app.update_in(window, |app, win, cx| app.focus_composer(win, cx));
    window.run_until_parked();
    let composer = app.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });
    assert!(window.update(|window, _| composer.is_focused(window)));
    assert!(app.read_with(window, |app, cx| app.composer.read(cx).text().is_empty()));

    let selector = Box::leak(format!("ui-icon-button-workshop-{closed_id}-close").into_boxed_str());
    let panel = window.debug_bounds(PANEL).unwrap();
    let close = window.debug_bounds(selector).unwrap();
    contains(panel, close);
    window.simulate_click(close.center(), gpui::Modifiers::default());
    window.run_until_parked();

    app.read_with(window, |app, cx| {
        assert!(!app.shell.tabs.tabs.iter().any(|tab| tab.id == closed_id));
        assert_eq!(app.shell.tabs.active.as_deref(), Some(next_id.as_str()));
        assert_eq!(app.composer.read(cx).text(), "");
    });
    window.simulate_input("Typed immediately after pointer Close");
    window.run_until_parked();
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        "Typed immediately after pointer Close",
        "typing after pointer Close goes to the surviving tab's composer without refocusing"
    );
    assert!(
        window.update(|window, _| composer.is_focused(window)),
        "successful pointer Close restores focus to the owned composer"
    );
}

#[gpui::test]
fn new_conversation_and_project_folder_are_visible_with_history_closed(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_header_action_tests::new_conversation_and_project_folder_are_visible_with_history_closed") { return; }
    let (app, window) = setup(cx);
    window.simulate_resize(size(px(1100.), px(800.)));
    let previous = app.update(window, |app, cx| {
        let id = app.sidebar.active_id().to_string();
        app.work_push(crate::Role::User, "Keep this conversation".into());
        cx.notify();
        id
    });
    window.run_until_parked();
    assert!(window
        .debug_bounds("ui-chip-awili-workshop-project")
        .is_some());
    assert!(window
        .debug_bounds("ui-button-awili-choose-project-folder")
        .is_none());
    let new = window
        .debug_bounds("ui-button-awili-new-conversation")
        .expect("visible New conversation");
    window.simulate_click(new.center(), gpui::Modifiers::default());
    window.run_until_parked();
    app.read_with(window, |app, _| {
        assert_ne!(app.sidebar.active_id(), previous);
        assert!(app
            .views
            .get(&previous)
            .unwrap()
            .convo
            .messages()
            .iter()
            .any(|m| m.text.contains("Keep this conversation")));
        assert!(app
            .work
            .conversation_routes
            .get(app.sidebar.active_id())
            .is_none_or(|r| r.active().is_none()));
    });
}

#[gpui::test]
fn auto_explains_home_folder_and_offers_a_visible_chooser(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_header_action_tests::auto_explains_home_folder_and_offers_a_visible_chooser",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    window.simulate_resize(size(px(1100.), px(800.)));
    let folder = std::env::temp_dir().join(format!("awili-project-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    app.update(window, |app, cx| {
        let target = crate::work::WorkExecutor::Agent {
            engine: awili_code::CodeEngine::Codex,
            model: None,
        };
        app.pick_work_executor(target, cx);
        app.work.folder = Some(dirs::home_dir().unwrap());
        let key = app.work_chip_key();
        app.work.ask_menu = crate::ask_mode_ui::AskMenu::Open(key);
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("ui-button-ask-auto-choose-folder")
            .is_some(),
        "Auto explains the refusal and offers an action"
    );
    app.update(window, |app, cx| {
        app.work.ask_menu = crate::ask_mode_ui::AskMenu::Closed;
        app.choose_workshop_folder(folder.to_string_lossy().into_owned(), cx);
        let key = app.work_chip_key();
        app.set_ask_mode(key.clone(), awili_work::ApprovalMode::Auto, cx);
        assert_eq!(
            app.work.ask_modes.mode_for(&key),
            awili_work::ApprovalMode::Auto
        );
    });
    std::fs::remove_dir_all(folder).unwrap();
}
