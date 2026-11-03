//! Drive the real shell with a private HOME and no configured services.
use crate::{consolidation::BACKSTAGE, work::Mode, AwiliApp};
use gpui::{px, size};

fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let center = window.debug_bounds(selector).unwrap().center();
    window.simulate_click(center, gpui::Modifiers::default());
}

fn hermes_session(
    id: &str,
    title: &str,
    model: &str,
    source: &str,
) -> awili_gateway::rest::HermesSession {
    awili_gateway::rest::HermesSession {
        id: id.to_string(),
        source: Some(source.to_string()),
        model: Some(model.to_string()),
        title: Some(title.to_string()),
        started_at: Some(1_000.0),
        ended_at: Some(2_000.0),
        end_reason: None,
        message_count: Some(3),
        tool_call_count: Some(1),
        input_tokens: Some(20),
        output_tokens: Some(30),
        preview: None,
        last_active: None,
    }
}

const CHILD: &str = "AWILI_CONSOLIDATION_TEST_HOME";

pub(crate) fn isolated(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

#[gpui::test]
fn compact_backstage_preserves_header_lines(cx: &mut gpui::TestAppContext) {
    if !isolated("consolidation_tests::compact_backstage_preserves_header_lines") {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        for i in 0..60 {
            app.conversations
                .create_kind(1000 + i, awili_core::convs::ConvKind::Work);
        }
        cx.notify();
    });
    for scale in [1.0_f32, 1.25, 1.5] {
        crate::theme::set_ui_scale(scale);
        for (width, height) in [(863., 512.), (800., 480.), (1400., 900.)] {
            window.simulate_resize(size(px(width), px(height)));
            for (route, _) in BACKSTAGE {
                view.update(window, |app, cx| {
                    app.set_mode(route, cx);
                    for handle in [
                        &app.shell.backstage_scroll,
                        &app.shell.skills_scroll,
                        &app.shell.cron_scroll,
                        &app.shell.sessions_scroll,
                    ] {
                        handle.set_offset(Default::default());
                    }
                    cx.notify();
                });
                window.run_until_parked();
                let strip = window
                    .debug_bounds("ui-tabs-awili-backstage-sections")
                    .unwrap();
                let selector = match route {
                    Mode::Skills => "ui-tab-backstage-Skills",
                    Mode::Cron => "ui-tab-backstage-Automations",
                    Mode::Sessions => "ui-tab-backstage-Sessions",
                    Mode::Messaging => "ui-tab-backstage-Connections",
                    Mode::CommandCenter => "ui-tab-backstage-Health",
                    _ => unreachable!(),
                };
                let selected_tab = window.debug_bounds(selector).unwrap();
                assert!(
                    selected_tab.left() >= strip.left() && selected_tab.right() <= strip.right(),
                    "{route:?} selected tab must be visible at {width}x{height}/{scale}: {selected_tab:?} / {strip:?}"
                );
                let title = window
                    .debug_bounds("ui-text-awili-backstage-title")
                    .unwrap();
                let hint = window.debug_bounds("ui-text-awili-backstage-hint").unwrap();
                let body = window.debug_bounds("awili-backstage-body").unwrap();
                assert!(
                    title.size.height >= crate::theme::z(crate::theme::type_::TITLE.line),
                    "{route:?} title must retain its full line at {width}x{height}/{scale}: {title:?}"
                );
                assert!(hint.size.height >= crate::theme::z(crate::theme::type_::BODY.line));
                assert!(
                    body.top() >= hint.bottom(),
                    "section must not overlap the header"
                );
                assert!(
                    body.bottom() <= px(height),
                    "section owns only the remaining stage"
                );
                assert!(body.size.height > px(0.));
                let section = window.debug_bounds("awili-backstage-section").unwrap();
                let section_floor = crate::theme::z(crate::theme::BACKSTAGE_SECTION_MIN_H);
                assert!(section.size.height >= section_floor);
                assert!((section.top() - body.top()).abs() <= px(1.));
                if route == Mode::Messaging {
                    let minimum = body.size.height.max(section_floor);
                    assert!(
                        section.size.height + px(1.) >= minimum,
                        "Messaging keeps at least the stage or minimum section height: {section:?} / {body:?}"
                    );
                    let outer_max =
                        view.read_with(window, |app, _| app.shell.backstage_scroll.max_offset().y);
                    let content_overflow = (section.size.height - body.size.height).max(px(0.));
                    assert!(
                        (outer_max - content_overflow).abs() <= px(1.),
                        "Messaging growth is represented by the real outer scroll extent: {outer_max:?} / {section:?} / {body:?}"
                    );
                    if outer_max > px(0.) {
                        let before = section;
                        window.simulate_event(gpui::ScrollWheelEvent {
                            position: gpui::point(body.left() + px(1.), body.top() + px(1.)),
                            delta: gpui::ScrollDelta::Pixels(gpui::point(
                                px(0.),
                                -outer_max.min(px(80.)),
                            )),
                            ..Default::default()
                        });
                        window.run_until_parked();
                        let after = window.debug_bounds("awili-backstage-section").unwrap();
                        assert!(
                            after.top() < before.top(),
                            "a real wheel moves the growable Messaging section: {before:?} -> {after:?}"
                        );
                        assert_eq!(
                            window.debug_bounds("awili-backstage-body").unwrap(),
                            body,
                            "the outer viewport stays fixed while its content scrolls"
                        );
                        view.update(window, |app, cx| {
                            app.shell.backstage_scroll.set_offset(Default::default());
                            cx.notify();
                        });
                        window.run_until_parked();
                    }
                } else {
                    assert!(
                        (section.size.height - body.size.height.max(section_floor)).abs() <= px(1.),
                        "{route:?} section fills the stage or its minimum viewport"
                    );
                }
                let selector = match route {
                    Mode::Skills => Some("awili-skills-list"),
                    Mode::Cron => Some("awili-cron-list"),
                    Mode::Sessions => Some("awili-sessions-list"),
                    _ => None,
                };
                if let Some(selector) = selector {
                    let list = window.debug_bounds(selector).unwrap();
                    assert!(
                        list.size.height >= crate::theme::z(crate::theme::TARGET),
                        "{route:?} list must retain a usable viewport at {width}x{height}/{scale}: {list:?}"
                    );
                    assert!(
                        list.bottom() <= section.bottom() + px(1.),
                        "{route:?} list must stay inside its section: {list:?} / {section:?}"
                    );
                    window.simulate_event(gpui::ScrollWheelEvent {
                        position: gpui::point(body.center().x, body.top() + px(1.)),
                        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-400.))),
                        ..Default::default()
                    });
                    window.run_until_parked();
                    let revealed = window.debug_bounds(selector).unwrap();
                    let visible =
                        revealed.bottom().min(body.bottom()) - revealed.top().max(body.top());
                    assert!(
                        visible >= crate::theme::z(crate::theme::TARGET),
                        "{route:?} list must be visibly reachable: {revealed:?} / {body:?}"
                    );
                    view.update(window, |app, cx| {
                        app.shell.backstage_scroll.set_offset(Default::default());
                        cx.notify();
                    });
                    window.run_until_parked();
                }
                if route == Mode::Sessions {
                    let initial_list = window.debug_bounds("awili-sessions-list").unwrap();
                    if scale == 1.5 && height <= 512. {
                        assert!(
                            initial_list.top()
                                > body.bottom() - crate::theme::z(crate::theme::TARGET)
                        );
                    }
                    // Reveal the inner list through the bounded outer section.
                    window.simulate_event(gpui::ScrollWheelEvent {
                        position: gpui::point(body.center().x, body.top() + px(1.)),
                        // Leave the outer scroller partway through its range:
                        // a saturated outer scroller would conceal double scrolling.
                        delta: gpui::ScrollDelta::Pixels(gpui::point(
                            px(0.),
                            -(section.size.height - body.size.height) * 0.75,
                        )),
                        ..Default::default()
                    });
                    window.run_until_parked();
                    let list = window.debug_bounds("awili-sessions-list").unwrap();
                    if section.size.height > body.size.height {
                        assert!(
                            list.top() < initial_list.top(),
                            "the outer section must move to reveal history"
                        );
                    }
                    let row = window.debug_bounds("awili-session-row-0").unwrap();
                    view.read_with(window, |app, _| {
                        assert_eq!(app.shell.sessions_scroll.offset().y, px(0.))
                    });
                    let scrolled_section = window.debug_bounds("awili-backstage-section").unwrap();
                    assert!(list.top() < body.bottom() && list.bottom() > body.top());
                    let visible_top = list.top().max(body.top());
                    let visible_bottom = list.bottom().min(body.bottom());
                    assert!(visible_bottom - visible_top >= crate::theme::z(crate::theme::TARGET));
                    let pointer = gpui::point(list.center().x, (visible_top + visible_bottom) / 2.);
                    window.simulate_event(gpui::ScrollWheelEvent {
                        position: pointer,
                        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-80.))),
                        ..Default::default()
                    });
                    window.run_until_parked();
                    assert_eq!(
                        window.debug_bounds("awili-sessions-list").unwrap(),
                        list,
                        "the inner wheel must move rows within a stationary viewport"
                    );
                    view.read_with(window, |app, _| {
                        assert_eq!(app.shell.sessions_scroll.offset().y, px(-80.))
                    });
                    assert!(
                        window.debug_bounds("awili-session-row-0").unwrap().top() < row.top(),
                        "populated Sessions history must actually scroll"
                    );
                    assert_eq!(
                        window.debug_bounds("awili-backstage-section").unwrap(),
                        scrolled_section
                    );
                    let down_row = window.debug_bounds("awili-session-row-0").unwrap();
                    window.simulate_event(gpui::ScrollWheelEvent {
                        position: pointer,
                        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(40.))),
                        ..Default::default()
                    });
                    window.run_until_parked();
                    assert_eq!(window.debug_bounds("awili-sessions-list").unwrap(), list);
                    view.read_with(window, |app, _| {
                        assert_eq!(app.shell.sessions_scroll.offset().y, px(-40.))
                    });
                    assert_eq!(
                        window.debug_bounds("awili-backstage-section").unwrap(),
                        scrolled_section
                    );
                    assert!(
                        window.debug_bounds("awili-session-row-0").unwrap().top() > down_row.top()
                    );
                    // At the inner limits a wheel can reveal the section
                    // header again. Away from those limits it cannot move it.
                    if section.size.height > body.size.height {
                        view.update(window, |app, cx| {
                            app.shell.sessions_scroll.set_offset(Default::default());
                            cx.notify();
                        });
                        window.run_until_parked();
                        window.simulate_event(gpui::ScrollWheelEvent {
                            position: pointer,
                            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(20.))),
                            ..Default::default()
                        });
                        window.run_until_parked();
                        let at_top = window.debug_bounds("awili-sessions-list").unwrap();
                        assert!(
                            at_top.top() > list.top(),
                            "top limit must let the section scroll up"
                        );
                        view.update(window, |app, cx| {
                            let max = app.shell.sessions_scroll.max_offset();
                            app.shell
                                .sessions_scroll
                                .set_offset(gpui::point(px(0.), -max.y));
                            cx.notify();
                        });
                        window.run_until_parked();
                        window.simulate_event(gpui::ScrollWheelEvent {
                            position: pointer,
                            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-20.))),
                            ..Default::default()
                        });
                        window.run_until_parked();
                        assert!(
                            window.debug_bounds("awili-sessions-list").unwrap().top()
                                < at_top.top(),
                            "bottom limit must let the section scroll down"
                        );
                    }
                    assert_eq!(
                        window
                            .debug_bounds("ui-text-awili-backstage-title")
                            .unwrap(),
                        title
                    );
                    assert_eq!(
                        window.debug_bounds("ui-text-awili-backstage-hint").unwrap(),
                        hint
                    );
                }
            }
        }
    }
    crate::theme::set_ui_scale(1.0);
}

#[gpui::test]
fn backstage_section_switch_preserves_history_but_resets_outer_scroll(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "consolidation_tests::backstage_section_switch_preserves_history_but_resets_outer_scroll",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        for i in 0..60 {
            app.conversations
                .create_kind(1000 + i, awili_core::convs::ConvKind::Work);
        }
        app.set_mode(Mode::Sessions, cx);
    });
    crate::theme::set_ui_scale(1.5);
    window.simulate_resize(size(px(800.), px(480.)));
    window.run_until_parked();
    view.update(window, |app, cx| {
        app.shell
            .backstage_scroll
            .set_offset(gpui::point(px(0.), px(-100.)));
        app.shell
            .sessions_scroll
            .set_offset(gpui::point(px(0.), px(-80.)));
        cx.notify();
    });
    window.run_until_parked();
    let body = window.debug_bounds("awili-backstage-body").unwrap();
    assert!(
        window
            .debug_bounds("awili-backstage-section")
            .unwrap()
            .top()
            < body.top()
    );
    window.update(|window, cx| {
        let focus = view.read_with(cx, |app, _| app.shell.backstage_focus.clone());
        window.focus(&focus, cx);
    });
    // Keyboard reaches the overflowed tabs in a compact strip too.
    for _ in 0..2 {
        window.simulate_keystrokes("right");
        window.run_until_parked();
    }
    view.read_with(window, |app, _| {
        assert_eq!(app.mode, Mode::CommandCenter);
        assert_eq!(app.shell.backstage_scroll.offset().y, px(0.));
        assert_eq!(app.shell.sessions_scroll.offset().y, px(-80.));
    });
    assert_eq!(
        window
            .debug_bounds("awili-backstage-section")
            .unwrap()
            .top(),
        window.debug_bounds("awili-backstage-body").unwrap().top()
    );
    // Keep the same selection while resizing and changing zoom. Reveal uses
    // the fresh painted geometry, not only the previous strip's child bounds.
    for width in [1400., 800.] {
        window.simulate_resize(size(px(width), px(480.)));
        window.run_until_parked();
        let selected = window.debug_bounds("ui-tab-backstage-Health").unwrap();
        let track = view.read_with(window, |app, _| app.shell.backstage_tabs_scroll.bounds());
        assert!(
            selected.left() >= track.left() && selected.right() <= track.right(),
            "resize {width}: {selected:?} / {track:?}"
        );
    }
    for scale in [1.0, 1.5] {
        crate::theme::set_ui_scale(scale);
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        let selected = window.debug_bounds("ui-tab-backstage-Health").unwrap();
        let track = view.read_with(window, |app, _| app.shell.backstage_tabs_scroll.bounds());
        assert!(
            selected.left() >= track.left() && selected.right() <= track.right(),
            "zoom {scale}: {selected:?} / {track:?}"
        );
    }
    let (track, before) = view.read_with(window, |app, _| {
        (
            app.shell.backstage_tabs_scroll.bounds(),
            app.shell.backstage_tabs_scroll.offset(),
        )
    });
    assert!(
        before.x < px(-60.),
        "the selected last tab requires horizontal reveal"
    );
    window.simulate_event(gpui::ScrollWheelEvent {
        position: track.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(60.), px(0.))),
        ..Default::default()
    });
    window.run_until_parked();
    let after = view.read_with(window, |app, _| app.shell.backstage_tabs_scroll.offset());
    assert_eq!(
        after.x,
        before.x + px(60.),
        "manual horizontal scroll is retained"
    );
    view.update(window, |_, cx| cx.notify());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(app.shell.backstage_tabs_scroll.offset(), after)
    });
    for _ in 0..2 {
        window.simulate_keystrokes("left");
        window.run_until_parked();
    }
    view.read_with(window, |app, _| {
        assert_eq!(app.mode, Mode::Sessions);
        assert_eq!(app.shell.backstage_scroll.offset().y, px(0.));
        assert_eq!(app.shell.sessions_scroll.offset().y, px(-80.));
    });
    crate::theme::set_ui_scale(1.0);
}

#[gpui::test]
fn real_rail_backstage_design_and_more_routes(cx: &mut gpui::TestAppContext) {
    if !isolated("consolidation_tests::real_rail_backstage_design_and_more_routes") {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        cx.notify();
    });
    for scale in [0.9_f32, 1.0, 1.25] {
        crate::theme::set_ui_scale(scale);
        for width in [860., 1400.] {
            window.simulate_resize(size(px(width), px(900.)));
            window.run_until_parked();
            for selector in [
                "awili-rail-item-0",
                "awili-rail-item-1",
                "awili-rail-item-2",
                "awili-rail-item-3",
                "awili-rail-item-4",
            ] {
                let bounds = window.debug_bounds(selector).unwrap();
                assert!(bounds.size.width >= px(44.) && bounds.size.height >= px(44.));
                assert!(bounds.left() >= px(0.) && bounds.right() <= px(width));
            }
            // All old utility links go through the shared page, without a
            // composer, and select exactly their original service section.
            for (route, _) in BACKSTAGE {
                view.update(window, |app, cx| app.set_mode(route, cx));
                window.run_until_parked();
                assert!(window.debug_bounds("awili-backstage").is_some());
                assert!(window.debug_bounds("awili-center").is_none());
                view.read_with(window, |app, _| assert_eq!(app.mode, route));
            }
            if width == 860. && scale == 1.25 {
                view.read_with(window, |app, _| {
                    assert!(
                        matches!(
                            crate::council_view::card_layout(app.council_width(), scale, 2),
                            crate::council_view::CardLayout::Columns { per_row: 2, .. }
                        ),
                        "two Council cards fit the real stage after removing inline history"
                    );
                });
            }
            let design = window.debug_bounds("awili-rail-item-2").unwrap();
            window.simulate_click(design.center(), gpui::Modifiers::default());
            window.run_until_parked();
            assert!(window.debug_bounds("awili-design-placeholder").is_some());
            assert!(window.debug_bounds("awili-backstage").is_none());
            view.read_with(window, |app, _| assert_eq!(app.mode, Mode::Design));

            let more = window.debug_bounds("awili-rail-item-4").unwrap();
            window.simulate_click(more.center(), gpui::Modifiers::default());
            window.run_until_parked();
            assert!(window
                .debug_bounds("ui-menu-awili-more-menu-panel")
                .is_some());
            window.simulate_keystrokes("escape");
            window.run_until_parked();
            view.read_with(window, |app, _| assert!(!app.shell.more));
            window.update(|window, cx| {
                view.read_with(cx, |app, _| {
                    assert!(app.shell.more_trigger_focus.is_focused(window))
                });
            });
            window.simulate_keystrokes("space");
            window.run_until_parked();
            window.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("space").unwrap(),
            });
            window.run_until_parked();
            view.read_with(window, |app, _| assert!(app.shell.more));
            // Settings is the first real menu action. Keyboard choice closes
            // More before opening the existing editable Settings form.
            window.simulate_keystrokes("enter");
            window.run_until_parked();
            window.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("enter").unwrap(),
            });
            window.run_until_parked();
            view.update(window, |app, cx| {
                assert!(!app.shell.more);
                assert!(app.settings.open);
                app.close_settings(cx);
                cx.notify();
            });
            window.run_until_parked();
        }
    }
    crate::theme::set_ui_scale(1.0);
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();

    // Tab and global shortcuts dismiss More before another control takes
    // focus. It cannot remain painted while Escape belongs to the page.
    click(window, "awili-rail-item-4");
    window.run_until_parked();
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(!app.shell.more));
    click(window, "awili-rail-item-4");
    window.run_until_parked();
    window.simulate_keystrokes("cmd-n");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(!app.shell.more));

    // Code uses the same real text input; returning to a remembered Code
    // lane through Workshop must make it ready to type too.
    view.update(window, |app, cx| {
        app.set_mode(Mode::Code, cx);
        app.set_mode(Mode::Design, cx);
    });
    window.run_until_parked();
    click(window, "awili-rail-item-0");
    window.run_until_parked();
    window.simulate_keystrokes("c");
    window.run_until_parked();
    view.read_with(window, |app, cx| {
        assert_eq!(app.composer.read(cx).text(), "c")
    });

    // Close restores the trigger before an action moves focus into its own
    // overlay. The shortcuts action remains reachable through More.
    click(window, "awili-rail-item-4");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(app.shell.more));
    for key in ["down", "down", "enter"] {
        window.simulate_keystrokes(key);
        window.run_until_parked();
    }
    window.run_until_parked();
    window.update(|window, cx| {
        view.read_with(cx, |app, _cx| {
            assert!(app.shortcuts.open);
            assert!(app.overlay_focus.handle().is_focused(window));
            assert!(!app.shell.rail_focus.is_focused(window));
        });
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(!app.shortcuts.open));
    // The header's helper chip is the same current-conversation chooser as the
    // composer's: it opens that menu (not a New-thread menu), and Escape
    // returns focus to the chip without leaving the conversation.
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    click(window, "ui-chip-awili-workshop-helper");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.shell.composer_menus.model.is_some());
        assert!(app.shell.tabs.menu.is_none());
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    window.update(|window, cx| {
        view.read_with(cx, |app, _| {
            assert!(app.shell.composer_menus.model.is_none());
            assert!(app.shell.tabs.engine_focus.is_focused(window));
            assert_eq!(app.sidebar.active_id(), owner);
            assert!(!app.shell.more);
        });
    });

    // Click the page and tab callbacks, including the Connections keyboard
    // capture that must yield to the newly focused shell controls.
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();
    click(window, "awili-rail-item-3");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.mode, Mode::CommandCenter));
    click(window, "ui-tab-backstage-Connections");
    window.run_until_parked();
    view.read_with(window, |app, _cx| {
        assert_eq!(app.mode, Mode::Messaging);
    });
    window.update(|window, cx| {
        view.update(cx, |app, cx| window.focus(&app.shell.backstage_focus, cx))
    });
    window.simulate_keystrokes("shift-a");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.messaging_data.state.query().is_empty())
    });
    window.update(|window, cx| {
        view.update(cx, |app, cx| window.focus(&app.shell.backstage_focus, cx))
    });
    window.simulate_keystrokes("up");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(app.mode, Mode::Sessions);
        assert!(app.messaging_data.state.query().is_empty())
    });
    view.update(window, |app, cx| app.set_mode(Mode::Messaging, cx));
    window.run_until_parked();
    window.update(|window, cx| view.update(cx, |app, cx| window.focus(&app.shell.rail_focus, cx)));
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    window.update(|window, cx| {
        view.read_with(cx, |app, cx| {
            assert!(
                app.shell.navigation_focus.contains_focused(window, cx),
                "Tab stays in the rail: {:?}",
                window.focused(cx)
            );
            assert!(
                !app.shell.rail_focus.is_focused(window),
                "Tab moves to an inactive item"
            );
        });
    });
    window.simulate_keystrokes("space");
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("space").unwrap(),
    });
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(
            app.shell.more,
            "Space on the inactive More item opens its menu: mode={:?}, query={:?}",
            app.mode,
            app.messaging_data.state.query()
        );
        assert!(app.messaging_data.state.query().is_empty());
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |app, cx| window.focus(&app.shell.backstage_focus, cx))
    });
    window.simulate_keystrokes("left");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.mode, Mode::Sessions));

    // The remembered Workshop lane survives visits to another page.
    view.update(window, |app, cx| {
        app.set_mode(Mode::Work, cx);
        app.set_mode(Mode::Design, cx);
    });
    window.run_until_parked();
    click(window, "awili-rail-item-0");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.mode, Mode::Work));
    view.update(window, |app, cx| app.set_mode(Mode::Design, cx));
    window.run_until_parked();
    window.update(|window, cx| {
        view.update(cx, |app, cx| window.focus(&app.shell.rail_focus, cx));
    });
    for key in ["shift-tab", "shift-tab", "space"] {
        window.simulate_keystrokes(key);
        window.run_until_parked();
    }
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("space").unwrap(),
    });
    window.run_until_parked();
    window.update(|window, cx| {
        view.read_with(cx, |app, _| {
            assert_eq!(app.mode, Mode::Work);
            assert!(app.shell.rail_focus.is_focused(window));
        });
    });
    // Already-active Work now opens Job. Closing it returns typing to the composer.
    click(window, "awili-rail-item-0");
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.shell.job_open));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.shell.job_open));
    window.simulate_keystrokes("a");
    window.run_until_parked();
    view.read_with(window, |app, cx| {
        assert_eq!(app.composer.read(cx).text(), "a")
    });

    // Council keeps its history browser: More > Conversations must not
    // navigate to Workshop or shrink the center column.
    view.update(window, |app, cx| app.set_mode(Mode::Council, cx));
    window.run_until_parked();
    click(window, "awili-rail-item-4");
    window.run_until_parked();
    window.simulate_keystrokes("down enter");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(app.mode, Mode::Council);
        assert!(app.shell.conversations);
    });
    assert!(window.debug_bounds("awili-sidebar").is_some());

    // The existing history panel is below More's scrim too.
    let archived_before = view.read_with(window, |app, _| app.sidebar.archived_open);
    click(window, "awili-rail-item-4");
    window.run_until_parked();
    click(window, "awili-sidebar-archive");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(!app.shell.more);
        assert_eq!(app.sidebar.archived_open, archived_before);
    });

    // A hidden-but-remembered history panel must open when requested from a
    // non-conversation page, instead of toggling its stale flag closed. At a
    // width too narrow to dock beside the composer it opens as a sheet, which
    // is what Escape dismisses (a docked panel is not modal).
    window.simulate_resize(size(px(1000.), px(800.)));
    window.run_until_parked();
    view.update(window, |app, cx| app.set_mode(Mode::Skills, cx));
    window.run_until_parked();
    click(window, "awili-rail-item-4");
    window.run_until_parked();
    window.simulate_keystrokes("down enter");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(app.mode, Mode::Work);
        assert!(app.shell.conversations);
    });
    assert!(window.debug_bounds("awili-sidebar").is_some());
    view.update(window, |app, _| app.work.running = true);
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.update(window, |app, _| {
        assert!(!app.shell.conversations);
        assert!(app.work.running, "closing history must not stop the job");
        app.work.running = false;
    });
    window.simulate_keystrokes("b");
    window.run_until_parked();
    view.read_with(window, |app, cx| {
        assert_eq!(app.composer.read(cx).text(), "ab")
    });

    // More owns Escape even while history and a running job are present.
    // (Open through the panel's own setter: docked at this width, a raw
    // sheet flag is reconciled away, which is not what is being tested.)
    // More sits on the rail, so the history it shares the window with is the
    // DOCKED panel: at a width too narrow to dock, history is a modal sheet
    // above the rail, the rail cannot be clicked (the scrim takes the click and
    // dismisses the sheet), and More cannot open over it at all.
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();
    view.update(window, |app, cx| {
        app.work.running = true;
        app.set_workshop_history_open(true, cx);
        cx.notify();
    });
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(
            app.workshop_history_docked(px(app.window_w)),
            "setup: the panel is docked beside the running job"
        );
        assert!(
            app.workshop_history_open(px(app.window_w)),
            "setup: showing"
        );
    });
    click(window, "awili-rail-item-4");
    window.run_until_parked();
    // The click really opened More (state and rendered menu) before Escape.
    view.read_with(window, |app, _| {
        assert!(app.shell.more, "the click opened More")
    });
    assert!(
        window
            .debug_bounds("ui-menu-awili-more-menu-panel")
            .is_some(),
        "the More menu is rendered before Escape"
    );
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.update(window, |app, cx| {
        assert!(!app.shell.more, "Escape closed More");
        assert!(
            app.workshop_history_open(px(app.window_w)),
            "Escape closed More, not the history panel"
        );
        assert!(app.work.running);
        app.work.running = false;
        app.set_workshop_history_open(false, cx);
        cx.notify();
    });
    window.simulate_resize(size(px(1000.), px(800.)));
    window.run_until_parked();

    // Global overlays dismiss More before taking their keys and focus.
    click(window, "awili-rail-item-4");
    window.run_until_parked();
    window.simulate_keystrokes("cmd-shift-p");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.command_palette.open);
        assert!(!app.shell.more);
    });
    window.simulate_keystrokes("s");
    window.run_until_parked();
    view.read_with(window, |app, _| assert_eq!(app.command_palette.query, "s"));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(!app.command_palette.open));

    // More's scrim must block the live Connections tab underneath it.
    view.update(window, |app, cx| {
        app.shell.conversations = false;
        app.set_mode(Mode::Skills, cx);
    });
    window.run_until_parked();
    click(window, "awili-rail-item-4");
    window.run_until_parked();
    click(window, "ui-tab-backstage-Connections");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(!app.shell.more);
        assert_eq!(app.mode, Mode::Skills);
    });

    // Supported minimum height at large zoom: scroll to More, then prove both
    // the rail face and the actual menu remain inside the viewport.
    crate::theme::set_ui_scale(2.5);
    window.simulate_resize(size(px(800.), px(480.)));
    window.run_until_parked();
    view.update(window, |app, cx| {
        app.set_mode(Mode::Council, cx);
        app.shell.conversations = true;
        cx.notify();
    });
    window.run_until_parked();
    let history = window
        .debug_bounds("ui-panel-awili-conversations-panel")
        .unwrap();
    assert!(history.left() >= px(0.) && history.right() <= px(800.));
    let rail = window.debug_bounds("awili-rail").unwrap();
    let history_body = window.debug_bounds("awili-sidebar").unwrap();
    // At this compact width, Conversations is a full-window Sheet. Its
    // body belongs inside that Sheet; navigation resumes after dismissal.
    assert!(
        history_body.left() >= history.left() && history_body.right() <= history.right(),
        "{history_body:?}"
    );
    assert_eq!(history.left(), px(0.));
    assert!(rail.top() >= px(0.) && rail.bottom() <= px(480.));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.read_with(window, |app, _| assert!(!app.shell.conversations));
    window.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.shell.rail_scroll.set_offset(gpui::Point::default());
            window.focus(&app.shell.rail_focus, cx);
        });
    });
    window.run_until_parked();
    for _ in 0..3 {
        window.simulate_keystrokes("tab");
        window.run_until_parked();
    }
    let more = window.debug_bounds("awili-rail-item-4").unwrap();
    assert!(more.top() >= px(0.) && more.bottom() <= px(480.));
    window.simulate_click(more.center(), gpui::Modifiers::default());
    window.run_until_parked();
    let menu = window
        .debug_bounds("ui-menu-awili-more-menu-panel")
        .unwrap();
    assert!(
        menu.left() >= px(0.)
            && menu.right() <= px(800.)
            && menu.top() >= px(0.)
            && menu.bottom() <= px(480.),
        "{menu:?}"
    );
    for _ in 0..5 {
        window.simulate_keystrokes("down");
        window.run_until_parked();
    }
    // Five Downs land on item 2 only in the three-row macOS menu; off macOS the
    // menu carries the File/Edit/View/Help rows, so look at the row actually selected.
    let selected_ix = view.read_with(window, |app, _| app.shell.more_selected);
    #[cfg(target_os = "macos")]
    assert_eq!(selected_ix, 2, "macOS: five Downs wrap the 3-row menu to row 2");
    let selector: &'static str = Box::leak(format!("ui-row-awili-more-menu-item-{selected_ix}").into_boxed_str());
    let selected = window.debug_bounds(selector).unwrap();
    assert!(
        selected.top() >= menu.top() && selected.bottom() <= menu.bottom(),
        "{selected:?}"
    );
    // Menu keys still disarm a pending hold, and an open menu cannot arm a
    // fresh hold. Only the guard is probed; no microphone is opened.
    let generation = view.update(window, |app, _| {
        app.voice.ptt_guard = crate::ptt_guard::PttGuard::default();
        match app.voice.ptt_guard.modifier(true) {
            crate::ptt_guard::PttAction::ArmTimer(generation) => generation,
            action => panic!("{action:?}"),
        }
    });
    window.simulate_keystrokes("up");
    window.run_until_parked();
    view.update(window, |app, cx| {
        assert_eq!(
            app.voice.ptt_guard.timer(generation),
            crate::ptt_guard::PttAction::None
        );
        app.voice.cfg.ptt_key = awili_voice::PttKey::Option;
        app.on_modifiers(
            &gpui::ModifiersChangedEvent {
                modifiers: gpui::Modifiers {
                    alt: true,
                    ..Default::default()
                },
                capslock: gpui::Capslock { on: false },
            },
            cx,
        );
        assert!(matches!(
            app.voice.ptt_guard.clone().modifier(true),
            crate::ptt_guard::PttAction::ArmTimer(_)
        ));
    });
    crate::theme::set_ui_scale(1.0);
}

#[gpui::test]
fn populated_history_remains_bounded_and_scrolls(cx: &mut gpui::TestAppContext) {
    if !isolated("consolidation_tests::populated_history_remains_bounded_and_scrolls") {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    for archived in [false, true] {
        let oldest = view.update(window, |app, cx| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.fleet_pack = None;
            app.memory.enabled = false;
            app.set_mode(Mode::Work, cx);
            app.sidebar.archived_open = archived;
            let mut oldest = String::new();
            for i in 0..60 {
                let meta = app
                    .conversations
                    .create_kind(1000 + i, awili_core::convs::ConvKind::Work);
                if archived {
                    app.conversations.get_mut(&meta.id).unwrap().archived = true;
                }
                if i == 0 {
                    oldest = meta.id;
                }
            }
            cx.notify();
            oldest
        });
        let oldest_selector = Box::leak(format!("awili-history-row-{oldest}").into_boxed_str());
        for width in [1400., 800.] {
            window.simulate_resize(size(px(width), px(900.)));
            window.run_until_parked();
            // Open through the panel's setter at this width: docked at 1400,
            // a sheet at 800. (A raw sheet flag set at one width is
            // reconciled away by the first docked frame.)
            view.update(window, |app, cx| app.set_workshop_history_open(true, cx));
            window.run_until_parked();
            let panel = window
                .debug_bounds("ui-panel-awili-conversations-panel")
                .unwrap();
            let sidebar = window.debug_bounds("awili-sidebar").unwrap();
            let list = window.debug_bounds("awili-history-list").unwrap();
            assert!(
                sidebar.bottom() <= panel.bottom(),
                "{sidebar:?} / {panel:?}"
            );
            assert!(
                sidebar.size.width > px(300.),
                "history must fill the panel: {sidebar:?}"
            );
            window.simulate_event(gpui::ScrollWheelEvent {
                position: list.center(),
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-10000.))),
                ..Default::default()
            });
            window.run_until_parked();
            window.update(|window, _| window.refresh());
            window.run_until_parked();
            let last = window.debug_bounds(oldest_selector).unwrap();
            assert!(
                last.top() >= list.top() && last.bottom() <= list.bottom(),
                "{last:?} / {list:?}"
            );
            let title_prefix = if archived {
                "rc2ux-sidebar-title-a"
            } else {
                "rc2ux-sidebar-title-c"
            };
            let title = (0..60).find_map(|ix| {
                let selector: &'static str =
                    Box::leak(format!("{title_prefix}-{ix}").into_boxed_str());
                window
                    .debug_bounds(selector)
                    .filter(|bounds| last.contains(&bounds.center()))
            });
            let pointer = last.center();
            let before_hover = view.read_with(window, |app, _| {
                (
                    app.shell.history_scroll.offset().y,
                    app.shell.history_scroll.max_offset().y,
                    app.sidebar.hovered_id.clone(),
                )
            });
            // The earlier wheel targeted list.center(), not this row. Put the
            // pointer on the last row before checking its actions at the limit.
            window.simulate_mouse_move(pointer, None, gpui::Modifiers::default());
            window.run_until_parked();
            let after_hover = view.read_with(window, |app, _| {
                (
                    app.shell.history_scroll.offset().y,
                    app.shell.history_scroll.max_offset().y,
                    app.sidebar.hovered_id.clone(),
                )
            });
            assert_eq!(
                after_hover.2.as_deref(),
                Some(oldest.as_str()),
                "pointer hover must select the last row; archived={archived}, width={width}, row={last:?}, title={title:?}, list={list:?}, pointer={pointer:?}, point_in_row={}, point_in_title={}, scroll_before={:?}, scroll_after_hover={:?}",
                last.contains(&pointer),
                title.as_ref().is_some_and(|bounds| bounds.contains(&pointer)),
                (&before_hover.0, &before_hover.1, &before_hover.2),
                (&after_hover.0, &after_hover.1, &after_hover.2),
            );
            window.simulate_event(gpui::ScrollWheelEvent {
                position: pointer,
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-100.))),
                ..Default::default()
            });
            window.run_until_parked();
            let after_wheel = view.read_with(window, |app, _| {
                (
                    app.shell.history_scroll.offset().y,
                    app.shell.history_scroll.max_offset().y,
                    app.sidebar.hovered_id.clone(),
                )
            });
            assert_eq!(
                after_wheel.2.as_deref(),
                Some(oldest.as_str()),
                "a wheel at the bottom must retain row actions; archived={archived}, width={width}, row={last:?}, title={title:?}, list={list:?}, pointer={pointer:?}, point_in_row={}, point_in_title={}, scroll_before={:?}, scroll_after_hover={:?}, scroll_after_wheel={:?}",
                last.contains(&pointer),
                title.as_ref().is_some_and(|bounds| bounds.contains(&pointer)),
                (&before_hover.0, &before_hover.1, &before_hover.2),
                (&after_hover.0, &after_hover.1, &after_hover.2),
                (&after_wheel.0, &after_wheel.1, &after_wheel.2),
            );
            window.simulate_event(gpui::ScrollWheelEvent {
                position: pointer,
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(100.))),
                ..Default::default()
            });
            window.run_until_parked();
            view.read_with(window, |app, _| {
                assert!(
                    app.sidebar.hovered_id.is_none(),
                    "moving history clears actions for the row that was under the pointer"
                );
            });
        }
    }
}

#[gpui::test]
fn retained_conversations_settings_done_keeps_modal_keys(cx: &mut gpui::TestAppContext) {
    if !isolated("consolidation_tests::retained_conversations_settings_done_keeps_modal_keys") {
        return;
    }
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
        cx.notify();
    });
    window.simulate_resize(size(px(800.), px(712.)));
    window.run_until_parked();
    assert!(
        window.update(|win, _| crate::ui::is_compact(win)),
        "Fixture must exercise a compact Sheet"
    );
    // Opened after the resize: a sheet flag set while the window was still
    // wide is reconciled away by the first docked frame.
    view.update(window, |app, cx| app.set_workshop_history_open(true, cx));
    window.run_until_parked();
    assert!(
        window.debug_bounds("ui-sheet-scrim").is_some(),
        "Conversations has its actual modal scrim"
    );
    assert!(window
        .debug_bounds("ui-panel-awili-conversations-panel")
        .is_some());
    window.simulate_keystrokes("cmd-,");
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.settings.open));
    click(window, "sx-done-0");
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.settings.open));
    assert!(
        !window.update(|win, cx| view.read(cx).shell.rail_focus.contains_focused(win, cx)),
        "Done must return keys to the retained Conversations modal, not the background rail"
    );
    assert!(window.update(|win, cx| view
        .read(cx)
        .shell
        .conversations_focus
        .contains_focused(win, cx)));
    for key in ["tab", "shift-tab", "tab", "space"] {
        window.simulate_keystrokes(key);
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert!(
                app.shell.conversations,
                "Conversations stays open after {key}"
            );
            assert_eq!(
                app.mode,
                Mode::Work,
                "Background rail cannot activate after {key}"
            );
        });
        assert!(!window.update(|win, cx| view.read(cx).shell.rail_focus.contains_focused(win, cx)));
        assert!(window.update(|win, cx| view
            .read(cx)
            .shell
            .conversations_focus
            .contains_focused(win, cx)));
    }
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.shell.conversations));
}

#[gpui::test]
fn sessions_kit_filters_search_and_controls_keep_target_size(cx: &mut gpui::TestAppContext) {
    if !isolated("consolidation_tests::sessions_kit_filters_search_and_controls_keep_target_size") {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        let preexisting_ids: Vec<_> = app
            .conversations
            .all()
            .iter()
            .map(|conversation| conversation.id.clone())
            .collect();
        for id in preexisting_ids {
            app.conversations.get_mut(&id).unwrap().archived = true;
        }
        for (timestamp, kind, title) in [
            (
                1_001,
                awili_core::convs::ConvKind::Chat,
                "Stored chat history",
            ),
            (1_002, awili_core::convs::ConvKind::Work, "Work history"),
            (1_003, awili_core::convs::ConvKind::Chat, "Archived history"),
        ] {
            let meta = app.conversations.create_kind(timestamp, kind);
            app.conversations.get_mut(&meta.id).unwrap().title = title.to_string();
            if title == "Archived history" {
                app.conversations.get_mut(&meta.id).unwrap().archived = true;
            }
        }
        app.sessions_state.sessions = vec![
            hermes_session("remote-apollo", "Apollo research", "haiku", "desktop"),
            hermes_session("remote-notes", "Meeting notes", "sonnet", "mobile"),
        ];
        app.set_mode(Mode::Sessions, cx);
    });

    for (scale, width) in [(1.0_f32, 1400.0_f32), (1.0, 863.0), (1.5, 863.0)] {
        crate::theme::set_ui_scale(scale);
        window.simulate_resize(size(px(width), px(900.)));
        window.run_until_parked();
        let search = window
            .debug_bounds("awili-sessions-search")
            .expect("shared Sessions SearchField");
        assert!(search.size.height >= crate::theme::target(), "{search:?}");
        let tabs = window
            .debug_bounds("awili-sessions-tabs")
            .expect("shared Sessions Tabs");
        for selector in [
            "ui-tab-sess-tab-All",
            "ui-tab-sess-tab-Work",
            "ui-tab-sess-tab-Hermes",
        ] {
            let tab = window
                .debug_bounds(selector)
                .expect("all three filter tabs");
            assert!(
                tab.size.height >= crate::theme::target(),
                "{selector}: {tab:?}"
            );
            assert!(
                tab.top() >= tabs.top() && tab.bottom() <= tabs.bottom(),
                "{selector} stays inside the filter strip: {tab:?} / {tabs:?}"
            );
        }
        let list = window.debug_bounds("awili-sessions-list").unwrap();
        let local = window
            .debug_bounds("awili-session-row-0")
            .expect("local history Row");
        assert!(local.size.height >= crate::theme::target(), "{local:?}");
        assert!(
            local.top() >= list.top() && local.bottom() <= list.bottom(),
            "the first local target stays in the list viewport: {local:?} / {list:?}"
        );
        assert!(window.debug_bounds("awili-session-row-1").is_some());
        assert!(window.debug_bounds("awili-session-row-2").is_none());
        assert!(window.debug_bounds("ui-row-hist-hermes-0").is_some());
        assert!(window.debug_bounds("ui-row-hist-hermes-1").is_some());
    }

    crate::theme::set_ui_scale(1.0);
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();
    for (selector, expected) in [
        ("ui-tab-sess-tab-Work", crate::sessions::SessionsView::Work),
        (
            "ui-tab-sess-tab-Hermes",
            crate::sessions::SessionsView::Hermes,
        ),
        ("ui-tab-sess-tab-All", crate::sessions::SessionsView::All),
    ] {
        click(window, selector);
        assert_eq!(
            view.read_with(window, |app, _| app.sessions_state.view),
            expected
        );
    }
    // Work lists every local thread, including ones stored as "chat" before
    // the Chat split was removed.
    click(window, "ui-tab-sess-tab-Work");
    assert!(window.debug_bounds("awili-session-row-0").is_some());
    assert!(window.debug_bounds("awili-session-row-1").is_some());
    assert!(window.debug_bounds("awili-session-row-2").is_none());
    assert!(window.debug_bounds("ui-row-hist-hermes-0").is_none());

    click(window, "ui-tab-sess-tab-Hermes");
    click(window, "awili-sessions-search");
    window.simulate_keystrokes("mobile");
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, _| app.sessions_state.filter.clone()),
        "mobile"
    );
    assert!(window.debug_bounds("ui-row-hist-hermes-0").is_some());
    assert!(window.debug_bounds("ui-row-hist-hermes-1").is_none());
    click(window, "ui-icon-button-sessions-search-clear");
    assert!(view.read_with(window, |app, _| app.sessions_state.filter.is_empty()));
    click(window, "awili-sessions-search");
    window.simulate_keystrokes("apollo");
    window.run_until_parked();
    assert!(window.debug_bounds("ui-row-hist-hermes-0").is_some());
    assert!(window.debug_bounds("ui-row-hist-hermes-1").is_none());
    crate::theme::set_ui_scale(1.0);
}

#[gpui::test]
fn sessions_local_keyboard_row_routes_and_returns_typing_to_owner(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "consolidation_tests::sessions_local_keyboard_row_routes_and_returns_typing_to_owner",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    let (work_id, archived_id) = view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        let preexisting_ids: Vec<_> = app
            .conversations
            .all()
            .iter()
            .map(|conversation| conversation.id.clone())
            .collect();
        for id in preexisting_ids {
            app.conversations.get_mut(&id).unwrap().archived = true;
        }
        let live = app
            .conversations
            .create_kind(1_010, awili_core::convs::ConvKind::Work);
        app.conversations.get_mut(&live.id).unwrap().title = "Live Work destination".into();
        let archived = app
            .conversations
            .create_kind(1_020, awili_core::convs::ConvKind::Work);
        app.conversations.get_mut(&archived.id).unwrap().archived = true;
        let archived_id = archived.id.clone();
        app.set_active_conversation(&archived_id);
        app.composer
            .update(cx, |composer, cx| composer.set_text("", cx));
        app.set_mode(Mode::Sessions, cx);
        cx.notify();
        (live.id, archived_id)
    });
    window.run_until_parked();
    let search_focus = view.read_with(window, |app, _| app.sessions_search_focus.clone());
    let row = window.debug_bounds("awili-session-row-0").unwrap();
    let list = window.debug_bounds("awili-sessions-list").unwrap();
    assert!(row.size.height >= crate::theme::target(), "{row:?}");
    assert!(
        row.top() >= list.top() && row.bottom() <= list.bottom(),
        "the keyboard destination is fully visible in its scroll viewport: {row:?} / {list:?}"
    );
    // Identify the row's real GPUI focus handle through a canceled pointer
    // activation. Mouse down focuses the row; releasing outside cancels its
    // click, so the route still has to be reached by keyboard below.
    let outside = gpui::point(px(0.), px(0.));
    assert!(
        !row.contains(&outside),
        "the cancel point lies outside the row"
    );
    window.simulate_mouse_down(
        row.center(),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    window.run_until_parked();
    let row_focus = window
        .update(|window, cx| window.focused(cx))
        .expect("mouse down on the visible Sessions row focuses a control");
    assert_ne!(row_focus, search_focus);
    window.simulate_mouse_up(outside, gpui::MouseButton::Left, gpui::Modifiers::default());
    window.run_until_parked();
    assert_eq!(
        window.update(|window, cx| window.focused(cx)),
        Some(row_focus.clone()),
        "the canceled pointer probe leaves the row focused"
    );
    assert_eq!(view.read_with(window, |app, _| app.mode), Mode::Sessions);
    assert_eq!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        archived_id,
        "the canceled pointer probe neither routes nor changes the active owner"
    );

    window.update(|window, cx| window.focus(&search_focus, cx));
    let mut visited_focus = vec![search_focus.clone()];
    let mut reached_row = false;
    for _ in 0..4096 {
        window.simulate_keystrokes("tab");
        window.run_until_parked();
        let focused = window
            .update(|window, cx| window.focused(cx))
            .expect("Tab traversal retains a focused control");
        if focused == row_focus {
            reached_row = true;
            break;
        }
        assert!(
            !visited_focus.contains(&focused),
            "Tab traversal completed a focus cycle without reaching the pointer-verified Row"
        );
        visited_focus.push(focused);
    }
    assert!(
        reached_row,
        "Tab traversal reached the pointer-verified Row before the catastrophic guard"
    );
    window.simulate_keystrokes("enter");
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();

    assert_eq!(view.read_with(window, |app, _| app.mode), Mode::Work);
    assert_eq!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        work_id
    );
    let composer_focus = view.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });
    assert!(
        window.update(|window, _| composer_focus.is_focused(window)),
        "the disappearing history Row hands focus to the routed composer"
    );
    window.simulate_keystrokes("x");
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        "x",
        "typing immediately after row activation reaches the selected owner"
    );
}

#[gpui::test]
fn sessions_palette_open_clears_stale_transcript_and_guards_mismatched_owner(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "consolidation_tests::sessions_palette_open_clears_stale_transcript_and_guards_mismatched_owner",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.sessions_state.transcript_generation = 40;
        app.sessions_state.selected_id = Some("session-a".into());
        app.sessions_state.transcript = Some((
            "session-a".into(),
            vec![("assistant".into(), "stale A transcript".into())],
        ));
        app.sessions_state.transcript_error = Some("stale A error".into());
        app.sessions_state.transcript_loading = false;
        app.sessions_state.transcript_request = None;
        cx.notify();
    });

    // Exercise the dynamic action dispatcher used by palette Enter. Its
    // synchronous ownership transition must clear A and schedule B before a
    // network response can be applied.
    window.update(|window, cx| {
        view.update(cx, |app, cx| {
            app.run_command_palette_action(
                crate::command_palette::PaletteAction::RunDynamic(
                    crate::command_catalog::ProviderKind::RecentSessions,
                    "session-b".into(),
                ),
                window,
                cx,
            );
            assert_eq!(app.sessions_state.selected_id.as_deref(), Some("session-b"));
            assert!(app.sessions_state.transcript.is_none());
            assert!(app.sessions_state.transcript_error.is_none());
            assert!(app.sessions_state.transcript_loading);
            assert_eq!(
                app.sessions_state.transcript_request.as_deref(),
                Some("session-b")
            );
            assert_eq!(app.sessions_state.transcript_generation, 41);
        });
    });

    // A mismatched cached owner must never paint under B, even if stale state
    // is injected after the route transition. Invalidate B's real fetch so
    // this rendering check does not depend on the gateway.
    view.update(window, |app, cx| {
        app.sessions_state.transcript_generation = 42;
        app.sessions_state.transcript_request = None;
        app.sessions_state.transcript_loading = false;
        app.sessions_state.transcript_error = None;
        app.sessions_state.transcript = Some((
            "session-a".into(),
            vec![("assistant".into(), "stale A transcript".into())],
        ));
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("sess-transcript-empty").is_some());
    assert!(window.debug_bounds("sess-transcript-error").is_none());
    assert!(view.read_with(window, |app, _| {
        app.sessions_state.selected_id.as_deref() == Some("session-b")
    }));
}

#[gpui::test]
fn sessions_hermes_transcript_uses_shared_parts_and_back_hands_off_focus(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "consolidation_tests::sessions_hermes_transcript_uses_shared_parts_and_back_hands_off_focus",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.sessions_state.sessions = vec![hermes_session(
            "remote-transcript",
            "Transcript target",
            "haiku",
            "desktop",
        )];
        // Model an already-owned same-session request so selecting this record
        // exercises the real click/focus path without starting a network task.
        app.sessions_state.transcript_loading = true;
        app.sessions_state.transcript_request = Some("remote-transcript".into());
        app.set_mode(Mode::Sessions, cx);
    });
    window.run_until_parked();
    click(window, "ui-row-hist-hermes-0");
    assert_eq!(
        view.read_with(window, |app, _| app.sessions_state.selected_id.clone()),
        Some("remote-transcript".to_string())
    );
    let back_focus = view.read_with(window, |app, _| app.sessions_back_focus.clone());
    assert!(window.debug_bounds("sess-back").is_some());
    assert!(window.update(|window, _| back_focus.is_focused(window)));
    assert!(view.read_with(window, |app, _| app.sessions_state.transcript_loading));

    view.update(window, |app, cx| {
        app.sessions_state.transcript_generation =
            app.sessions_state.transcript_generation.wrapping_add(1);
        app.sessions_state.transcript_request = None;
        app.sessions_state.transcript_loading = false;
        app.sessions_state.transcript_error = Some("gateway unavailable".into());
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("sess-transcript-error").is_some());
    assert!(window.debug_bounds("sess-transcript-empty").is_none());

    view.update(window, |app, cx| {
        app.sessions_state.transcript_error = None;
        app.sessions_state.transcript = Some(("remote-transcript".into(), Vec::new()));
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("sess-transcript-empty").is_some());

    view.update(window, |app, cx| {
        app.sessions_state.transcript = Some((
            "remote-transcript".into(),
            vec![
                ("user".into(), "Question from Hermes".into()),
                ("assistant".into(), "Answer from Hermes".into()),
            ],
        ));
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("ui-card-sess-msg-0").is_some());
    assert!(window.debug_bounds("ui-card-sess-msg-1").is_some());
    let back = window.debug_bounds("sess-back").unwrap();
    assert!(back.size.height >= crate::theme::target(), "{back:?}");
    click(window, "sess-back");
    assert!(view.read_with(window, |app, _| app.sessions_state.selected_id.is_none()));
    let search_focus = view.read_with(window, |app, _| app.sessions_search_focus.clone());
    assert!(
        window.update(|window, _| search_focus.is_focused(window)),
        "Back hands focus to the live browser search field"
    );
}
