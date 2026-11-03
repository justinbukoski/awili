//! Rendered regressions for the Backstage Skills page.
use crate::{work::Mode, AwiliApp};
use gpui::{px, size};

const LAST_SKILL: &str = "zz-last-skill";
const MAX_FOCUS_STEPS: usize = 512;

fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let center = window.debug_bounds(selector).unwrap().center();
    window.simulate_click(center, gpui::Modifiers::default());
    window.run_until_parked();
}

fn assert_contained(
    viewport: gpui::Bounds<gpui::Pixels>,
    face: gpui::Bounds<gpui::Pixels>,
    label: &str,
) {
    let tolerance = px(0.1);
    assert!(
        face.left() >= viewport.left() - tolerance
            && face.right() <= viewport.right() + tolerance
            && face.top() >= viewport.top() - tolerance
            && face.bottom() <= viewport.bottom() + tolerance,
        "the complete {label} face is inside the Skills viewport: {face:?} / {viewport:?}"
    );
}

fn intersection(
    a: gpui::Bounds<gpui::Pixels>,
    b: gpui::Bounds<gpui::Pixels>,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    let left = a.left().max(b.left());
    let top = a.top().max(b.top());
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (right > left && bottom > top)
        .then(|| gpui::Bounds::new(gpui::point(left, top), size(right - left, bottom - top)))
}

fn wheel(window: &mut gpui::VisualTestContext, at: gpui::Point<gpui::Pixels>, delta_y: f32) {
    window.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(delta_y))),
        touch_phase: gpui::TouchPhase::Moved,
        ..Default::default()
    });
    window.run_until_parked();
}

fn pointer_probe_focus(
    window: &mut gpui::VisualTestContext,
    face: gpui::Bounds<gpui::Pixels>,
    outside: gpui::Point<gpui::Pixels>,
) -> gpui::FocusHandle {
    assert!(
        !face.contains(&outside),
        "cancel point is outside the target: {face:?}"
    );
    let modifiers = gpui::Modifiers::default();
    window.simulate_mouse_down(face.center(), gpui::MouseButton::Left, modifiers);
    window.run_until_parked();
    let pressed = window
        .update(|window, cx| window.focused(cx))
        .expect("pointer down focuses the actual KIT control");
    window.simulate_mouse_up(outside, gpui::MouseButton::Left, modifiers);
    window.run_until_parked();
    assert_eq!(
        window.update(|window, cx| window.focused(cx)),
        Some(pressed.clone()),
        "mouse up outside cancels activation but leaves the target focused"
    );
    pressed
}

fn tab_to_focus(window: &mut gpui::VisualTestContext, target: &gpui::FocusHandle, label: &str) {
    let first = window
        .update(|window, cx| window.focused(cx))
        .expect("Tab traversal begins with a focused control");
    let mut visited = vec![first];
    for _ in 0..MAX_FOCUS_STEPS {
        if window.update(|window, _| target.is_focused(window)) {
            return;
        }
        window.simulate_keystrokes("tab");
        window.run_until_parked();
        let focused = window
            .update(|window, cx| window.focused(cx))
            .expect("Tab traversal retains an actual focused control");
        if focused == *target {
            return;
        }
        assert!(
            !visited.contains(&focused),
            "one complete Tab cycle ends without reaching the {label}"
        );
        visited.push(focused);
    }
    panic!("Tab traversal exceeded {MAX_FOCUS_STEPS} stops before the {label}");
}

#[gpui::test]
fn skills_panel_kit_search_rows_states_and_busy_action(cx: &mut gpui::TestAppContext) {
    const TEST: &str = "skills_panel_tests::skills_panel_kit_search_rows_states_and_busy_action";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));

    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.gateway.enabled = true;
        app.set_mode(Mode::Skills, cx);
    });
    // Let the test-build refresh settle, then seed a healthy, deterministic
    // page model. The composer slash palette remains an independent owner.
    window.run_until_parked();
    view.update(window, |app, cx| {
        app.skills_state.loading = false;
        app.skills_state.error = None;
        app.skills_state.filter.clear();
        app.skills_state.selected = None;
        app.skills_state.toggling = false;
        app.skills_state.skills = vec![
            awili_gateway::rest::Skill {
                name: "python-support".into(),
                category: Some("development".into()),
                description: Some("Assistant workflow guidance".into()),
                enabled: true,
                version: Some("2.1".into()),
            },
            awili_gateway::rest::Skill {
                name: "rust-format".into(),
                category: Some("development".into()),
                description: Some("Format Rust source".into()),
                enabled: false,
                version: Some("1.4".into()),
            },
        ];
        app.skills_state.toolsets = vec![
            awili_gateway::rest::Toolset {
                name: "python-tools".into(),
                label: Some("Python tools".into()),
                description: Some("Database adapters".into()),
                enabled: true,
                configured: true,
                tools: vec!["query".into(), "migrate".into()],
            },
            awili_gateway::rest::Toolset {
                name: "database-tools".into(),
                label: Some("Database tools".into()),
                description: Some("Python database helpers".into()),
                enabled: false,
                configured: false,
                tools: vec!["inspect".into()],
            },
        ];
        app.skill_palette.open = true;
        app.skill_palette.query = "composer-owned".into();
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("rc2r58-hermes-notice-skills").is_none());

    for (scale, width) in [(1.0_f32, 1400.0_f32), (1.0, 863.0), (1.5, 863.0)] {
        crate::theme::set_ui_scale(scale);
        window.simulate_resize(size(px(width), px(900.)));
        window.run_until_parked();
        let search = window.debug_bounds("awili-skills-search").unwrap();
        let list = window.debug_bounds("awili-skills-list").unwrap();
        let skill = window
            .debug_bounds("ui-row-skill-row-python-support")
            .unwrap();
        let toolset = window
            .debug_bounds("ui-row-toolset-row-python-tools")
            .unwrap();
        for (selector, face) in [
            ("SearchField", search),
            ("Skill Row", skill),
            ("Toolset Row", toolset),
        ] {
            assert!(
                face.size.height >= crate::theme::target(),
                "{selector} keeps its target at scale {scale}: {face:?}"
            );
        }
        for (selector, face) in [("Skill Row", skill), ("Toolset Row", toolset)] {
            assert!(
                face.left() >= list.left() && face.right() <= list.right(),
                "{selector} stays inside the live scroll viewport horizontally at scale {scale}: {face:?} / {list:?}"
            );
        }
        if scale == 1.5 {
            let scroll = view.read_with(window, |app, _| app.shell.skills_scroll.clone());
            let before = scroll.offset();
            assert!(
                toolset.top() >= list.bottom() || toolset.bottom() > list.bottom(),
                "the high-zoom fixture begins with a clipped toolset row: {toolset:?} / {list:?}"
            );
            wheel(window, list.center(), -2000.);
            assert!(
                scroll.offset().y < before.y,
                "an actual wheel event moves the Skills list toward its last row: {before:?} -> {:?}",
                scroll.offset()
            );
            let revealed = window
                .debug_bounds("ui-row-toolset-row-python-tools")
                .unwrap();
            let viewport = window.debug_bounds("awili-skills-list").unwrap();
            assert_contained(viewport, revealed, "high-zoom Toolset Row");
        }
    }

    crate::theme::set_ui_scale(1.0);
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();
    click(window, "awili-skills-search");
    window.simulate_input("python");
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, _| app.skills_state.filter.clone()),
        "python"
    );
    assert!(window
        .debug_bounds("ui-row-skill-row-python-support")
        .is_some());
    assert!(window
        .debug_bounds("ui-row-toolset-row-database-tools")
        .is_some());
    assert!(window
        .debug_bounds("ui-row-skill-row-rust-format")
        .is_none());
    assert!(window
        .debug_bounds("ui-row-toolset-row-python-tools")
        .is_some());
    assert_eq!(
        view.read_with(window, |app, _| app.skill_palette.query.clone()),
        "composer-owned",
        "page search does not repurpose the composer's slash-palette query"
    );

    click(window, "ui-icon-button-skills-search-clear");
    assert!(view.read_with(window, |app, _| app.skills_state.filter.is_empty()));
    click(window, "awili-skills-search");
    window.simulate_input("format");
    window.run_until_parked();
    assert!(window
        .debug_bounds("ui-row-skill-row-rust-format")
        .is_some());
    assert!(window
        .debug_bounds("ui-row-skill-row-python-support")
        .is_none());
    assert!(window
        .debug_bounds("ui-row-toolset-row-python-tools")
        .is_none());
    click(window, "ui-icon-button-skills-search-clear");
    click(window, "awili-skills-search");
    window.simulate_input("assistant");
    window.run_until_parked();
    assert!(window
        .debug_bounds("ui-row-skill-row-python-support")
        .is_some());
    assert!(window
        .debug_bounds("ui-row-toolset-row-database-tools")
        .is_none());
    assert!(window
        .debug_bounds("ui-row-skill-row-rust-format")
        .is_none());
    click(window, "ui-icon-button-skills-search-clear");
    assert!(window
        .debug_bounds("ui-row-skill-row-rust-format")
        .is_some());
    assert!(window
        .debug_bounds("ui-row-toolset-row-database-tools")
        .is_some());

    click(window, "ui-row-skill-row-python-support");
    assert_eq!(
        view.read_with(window, |app, _| app.skills_state.selected.clone()),
        Some("python-support".into())
    );
    assert!(window
        .debug_bounds("ui-card-skill-details-python-support")
        .is_some());
    let list = window.debug_bounds("awili-skills-list").unwrap();
    let row = window
        .debug_bounds("ui-row-skill-row-python-support")
        .unwrap();
    let action = window
        .debug_bounds("ui-button-skill-toggle-python-support")
        .unwrap();
    assert!(row.size.height >= crate::theme::target(), "{row:?}");
    assert!(action.size.height >= crate::theme::target(), "{action:?}");
    assert!(
        row.top() >= list.top()
            && row.bottom() <= list.bottom()
            && action.top() >= list.top()
            && action.bottom() <= list.bottom(),
        "expanded skill and its action remain in the scroll viewport: {row:?} / {action:?} / {list:?}"
    );

    // A busy update keeps the action visible but disabled. Clicking it must
    // neither start another request nor collapse the selected skill.
    view.update(window, |app, cx| {
        app.skills_state.toggling = true;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window
        .debug_bounds("ui-button-skill-toggle-python-support")
        .is_some());
    click(window, "ui-button-skill-toggle-python-support");
    assert!(view.read_with(window, |app, _| app.skills_state.toggling));
    assert_eq!(
        view.read_with(window, |app, _| app.skills_state.selected.clone()),
        Some("python-support".to_string())
    );

    view.update(window, |app, cx| {
        app.skills_state.toggling = false;
        app.skills_state.filter = "no-such-capability".into();
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-skills-empty").is_some());
    view.update(window, |app, cx| {
        app.skills_state.filter.clear();
        app.skills_state.skills.clear();
        app.skills_state.toolsets.clear();
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-skills-empty").is_some());

    view.update(window, |app, cx| {
        app.skills_state.loading = true;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-skills-loading").is_some());
    crate::theme::set_ui_scale(1.0);
}

#[gpui::test]
fn skills_panel_tab_reaches_clipped_row_and_action(cx: &mut gpui::TestAppContext) {
    const TEST: &str = "skills_panel_tests::skills_panel_tab_reaches_clipped_row_and_action";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.gateway.enabled = true;
        app.set_mode(Mode::Skills, cx);
    });
    window.run_until_parked();
    view.update(window, |app, cx| {
        app.skills_state.loading = false;
        app.skills_state.error = None;
        app.skills_state.filter.clear();
        app.skills_state.toggling = false;
        app.skills_state.skills = (0..12)
            .map(|index| awili_gateway::rest::Skill {
                name: if index == 11 {
                    LAST_SKILL.to_string()
                } else {
                    format!("skill-{index:02}")
                },
                category: Some("testing".into()),
                description: Some(format!("Capability number {index}")),
                enabled: true,
                version: Some("1".into()),
            })
            .collect();
        app.skills_state.toolsets.clear();
        app.skills_state.selected = Some(LAST_SKILL.to_string());
        cx.notify();
    });
    crate::theme::set_ui_scale(1.5);
    window.simulate_resize(size(px(863.), px(900.)));
    window.run_until_parked();

    let scroll = view.read_with(window, |app, _| app.shell.skills_scroll.clone());
    let list = window.debug_bounds("awili-skills-list").unwrap();
    wheel(window, list.center(), -10000.);
    assert!(scroll.offset().y < px(0.), "wheel reveals the final skill");
    let row_selector = "ui-row-skill-row-zz-last-skill";
    let action_selector = "ui-button-skill-toggle-zz-last-skill";
    let row = window.debug_bounds(row_selector).unwrap();
    let action = window.debug_bounds(action_selector).unwrap();
    let list = window.debug_bounds("awili-skills-list").unwrap();
    assert_contained(list, row, "pointer-probed final Skill Row");
    assert_contained(list, action, "pointer-probed final skill action");

    let outside = gpui::point(px(0.), px(0.));
    let row_focus = pointer_probe_focus(window, row, outside);
    view.read_with(window, |app, _| {
        assert_eq!(app.skills_state.selected.as_deref(), Some(LAST_SKILL));
        assert!(!app.skills_state.toggling);
    });
    let action_focus = pointer_probe_focus(window, action, outside);
    assert_ne!(row_focus, action_focus, "Row and action are distinct stops");
    view.read_with(window, |app, _| {
        assert_eq!(app.skills_state.selected.as_deref(), Some(LAST_SKILL));
        assert!(
            !app.skills_state.toggling,
            "the canceled probe ran no update"
        );
    });

    let search_focus = view.read_with(window, |app, _| app.skills_search_focus.clone());
    window.update(|window, cx| window.focus(&search_focus, cx));
    window.run_until_parked();
    let list = window.debug_bounds("awili-skills-list").unwrap();
    wheel(window, list.center(), 10000.);
    assert_eq!(
        scroll.offset().y,
        px(0.),
        "actual wheel returns the list to its top"
    );
    let before_row = window.debug_bounds(row_selector).unwrap();
    let before_action = window.debug_bounds(action_selector).unwrap();
    let viewport = window.debug_bounds("awili-skills-list").unwrap();
    assert!(
        before_row.top() >= viewport.bottom() || before_row.bottom() > viewport.bottom(),
        "the exact pointer-probed Row starts clipped before keyboard traversal: {before_row:?} / {viewport:?}"
    );
    assert!(
        before_action.top() >= viewport.bottom() || before_action.bottom() > viewport.bottom(),
        "the exact pointer-probed action starts clipped before keyboard traversal: {before_action:?} / {viewport:?}"
    );

    window.update(|window, cx| window.focus(&search_focus, cx));
    window.run_until_parked();
    tab_to_focus(window, &row_focus, "final Skill Row");
    let focused_row = window.debug_bounds(row_selector).unwrap();
    let viewport = window.debug_bounds("awili-skills-list").unwrap();
    let row_visible =
        focused_row.top() >= viewport.top() && focused_row.bottom() <= viewport.bottom();

    tab_to_focus(window, &action_focus, "final skill action");
    let focused_action = window.debug_bounds(action_selector).unwrap();
    let viewport = window.debug_bounds("awili-skills-list").unwrap();
    let action_visible =
        focused_action.top() >= viewport.top() && focused_action.bottom() <= viewport.bottom();
    assert!(
        row_visible && action_visible,
        "real Tab focus reveals the complete Row and action without programmatic target focus; Row visible={row_visible} {focused_row:?}, action visible={action_visible} {focused_action:?}, viewport={viewport:?}"
    );
    crate::theme::set_ui_scale(1.0);
}

#[gpui::test]
fn skills_panel_live_zoom_reveals_focus_and_preserves_wheel_choice(cx: &mut gpui::TestAppContext) {
    const TEST: &str =
        "skills_panel_tests::skills_panel_live_zoom_reveals_focus_and_preserves_wheel_choice";
    if !crate::consolidation_tests::isolated(TEST) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.gateway.enabled = true;
        app.set_mode(Mode::Skills, cx);
    });
    window.run_until_parked();
    view.update(window, |app, cx| {
        app.skills_state.loading = false;
        app.skills_state.error = None;
        app.skills_state.filter.clear();
        app.skills_state.toggling = false;
        app.skills_state.skills = (0..12)
            .map(|index| awili_gateway::rest::Skill {
                name: if index == 11 {
                    LAST_SKILL.to_string()
                } else {
                    format!("skill-{index:02}")
                },
                category: Some("testing".into()),
                description: Some(format!("Capability number {index}")),
                enabled: true,
                version: Some("1".into()),
            })
            .collect();
        app.skills_state.toolsets.clear();
        app.skills_state.selected = Some(LAST_SKILL.to_string());
        cx.notify();
    });
    window.update(|win, cx| view.update(cx, |app, cx| app.apply_ui_scale(1.0, win, cx)));
    window.simulate_resize(size(px(900.), px(712.)));
    window.run_until_parked();

    let scroll = view.read_with(window, |app, _| app.shell.skills_scroll.clone());
    let backstage_scroll = view.read_with(window, |app, _| app.shell.backstage_scroll.clone());
    let search_focus = view.read_with(window, |app, _| app.skills_search_focus.clone());
    let list = window.debug_bounds("awili-skills-list").unwrap();
    wheel(window, list.center(), -10000.);
    let row_selector = "ui-row-skill-row-zz-last-skill";
    let action_selector = "ui-button-skill-toggle-zz-last-skill";
    let row = window.debug_bounds(row_selector).unwrap();
    let action = window.debug_bounds(action_selector).unwrap();
    let viewport = window.debug_bounds("awili-skills-list").unwrap();
    let body = window.debug_bounds("awili-backstage-body").unwrap();
    let window_size = window.update(|window, _| window.viewport_size());
    let window_bounds = gpui::Bounds::new(gpui::point(px(0.), px(0.)), window_size);
    let physical_viewport = [body, window_bounds]
        .into_iter()
        .try_fold(viewport, intersection)
        .expect("the Skills list has a visible physical viewport at scale 1");
    assert_contained(
        physical_viewport,
        row,
        "pointer-probed final Skill Row at actual size",
    );
    assert_contained(
        physical_viewport,
        action,
        "pointer-probed final action at actual size",
    );

    let outside = gpui::point(px(0.), px(0.));
    let row_focus = pointer_probe_focus(window, row, outside);
    let action_focus = pointer_probe_focus(window, action, outside);
    assert_ne!(
        row_focus, action_focus,
        "Row and action are separate focus stops"
    );
    view.read_with(window, |app, _| {
        assert_eq!(app.skills_state.selected.as_deref(), Some(LAST_SKILL));
        assert!(!app.skills_state.toggling, "cancelled probes run no update");
    });

    for scale in [0.9_f32, 1.0, 1.25, 2.5] {
        window.update(|win, cx| view.update(cx, |app, cx| app.apply_ui_scale(scale, win, cx)));
        window.simulate_resize(size(px(900.), px(712.)));
        view.update(window, |app, cx| {
            app.shell
                .skills_scroll
                .set_offset(gpui::point(px(0.), px(0.)));
            cx.notify();
        });
        window.update(|win, cx| win.focus(&search_focus, cx));
        window.run_until_parked();

        tab_to_focus(window, &row_focus, "last Skill Row at live zoom");
        let focused_row = window.debug_bounds(row_selector).unwrap();
        let list_bounds = window.debug_bounds("awili-skills-list").unwrap();
        let body_bounds = window.debug_bounds("awili-backstage-body").unwrap();
        let window_size = window.update(|window, _| window.viewport_size());
        let window_bounds = gpui::Bounds::new(gpui::point(px(0.), px(0.)), window_size);
        let physical_viewport = [body_bounds, window_bounds]
            .into_iter()
            .try_fold(list_bounds, intersection)
            .unwrap_or_else(|| {
                panic!(
                    "Skills list has no physical viewport after Tab focus at zoom {scale}: list={list_bounds:?}, body={body_bounds:?}, window={window_bounds:?}"
                )
            });
        assert_contained(
            physical_viewport,
            focused_row,
            "Tab-focused Skill Row at live zoom within body/window",
        );

        tab_to_focus(window, &action_focus, "last skill action at live zoom");
        let focused_action = window.debug_bounds(action_selector).unwrap();
        let list_bounds = window.debug_bounds("awili-skills-list").unwrap();
        let body_bounds = window.debug_bounds("awili-backstage-body").unwrap();
        let window_size = window.update(|window, _| window.viewport_size());
        let window_bounds = gpui::Bounds::new(gpui::point(px(0.), px(0.)), window_size);
        let physical_viewport = [body_bounds, window_bounds]
            .into_iter()
            .try_fold(list_bounds, intersection)
            .unwrap_or_else(|| {
                panic!(
                    "Skills list has no physical viewport at zoom {scale}: list={list_bounds:?}, body={body_bounds:?}, window={window_bounds:?}"
                )
            });
        let action_visible_before_wheel = focused_action.left() >= physical_viewport.left()
            && focused_action.right() <= physical_viewport.right()
            && focused_action.top() >= physical_viewport.top()
            && focused_action.bottom() <= physical_viewport.bottom();
        let before_body_scroll = backstage_scroll.offset();
        let before_wheel = scroll.offset().y;
        window.simulate_event(gpui::ScrollWheelEvent {
            position: physical_viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(12.))),
            touch_phase: gpui::TouchPhase::Moved,
            ..Default::default()
        });
        let after_wheel = scroll.offset().y;
        assert!(
            after_wheel > before_wheel,
            "real wheel at the visible Skills/body/window intersection moves the inner list toward the top at zoom {scale}: inner={before_wheel:?}->{after_wheel:?}, outer={before_body_scroll:?}->{:?}, list={list_bounds:?}, body={body_bounds:?}, window={window_bounds:?}, focused-face={focused_action:?}, physical-viewport={physical_viewport:?}",
            backstage_scroll.offset()
        );
        window.run_until_parked();
        assert_eq!(
            scroll.offset().y,
            after_wheel,
            "the same focused action preserves the reader's wheel choice at zoom {scale}"
        );
        assert!(
            action_visible_before_wheel,
            "Tab focus must reveal the complete Button face inside the actually allocated list/body/window intersection at zoom {scale}: face={focused_action:?}, list={list_bounds:?}, body={body_bounds:?}, window={window_bounds:?}, physical-viewport={physical_viewport:?}, inner-scroll={before_wheel:?}->{after_wheel:?}, outer-scroll={before_body_scroll:?}->{:?}",
            backstage_scroll.offset()
        );
        assert!(window.update(|window, _| action_focus.is_focused(window)));
        let wheel_face = window.debug_bounds(action_selector).unwrap();
        let list_bounds = window.debug_bounds("awili-skills-list").unwrap();
        let body_bounds = window.debug_bounds("awili-backstage-body").unwrap();
        let window_size = window.update(|window, _| window.viewport_size());
        let window_bounds = gpui::Bounds::new(gpui::point(px(0.), px(0.)), window_size);
        let physical_viewport = [body_bounds, window_bounds]
            .into_iter()
            .try_fold(list_bounds, intersection)
            .expect("the Skills list remains physically reachable after wheel input");
        let face_intersection = intersection(wheel_face, physical_viewport);
        assert!(
            face_intersection.is_some(),
            "the focused action retains an actual screen-visible intersection after wheel input at zoom {scale}: face={wheel_face:?}, list={list_bounds:?}, body={body_bounds:?}, window={window_bounds:?}, physical-viewport={physical_viewport:?}"
        );

        window.update(|win, cx| win.focus(&search_focus, cx));
        window.run_until_parked();
        tab_to_focus(
            window,
            &action_focus,
            "same skill action after returning from Search",
        );
        let revisited_action = window.debug_bounds(action_selector).unwrap();
        let list_bounds = window.debug_bounds("awili-skills-list").unwrap();
        let body_bounds = window.debug_bounds("awili-backstage-body").unwrap();
        let window_size = window.update(|window, _| window.viewport_size());
        let window_bounds = gpui::Bounds::new(gpui::point(px(0.), px(0.)), window_size);
        let physical_viewport = [body_bounds, window_bounds]
            .into_iter()
            .try_fold(list_bounds, intersection)
            .expect("the Skills list remains physically reachable after returning from Search");
        assert_contained(
            physical_viewport,
            revisited_action,
            "revisited Button face after manual wheel movement",
        );
    }

    window.update(|win, cx| view.update(cx, |app, cx| app.apply_ui_scale(1.0, win, cx)));
    window.simulate_resize(size(px(900.), px(712.)));
    window.run_until_parked();
}
