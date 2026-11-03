use super::*;
use gpui::{Entity, FocusHandle, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_SETTINGS_ERROR_DETAILS_TEST_HOME";

fn isolated(test: &str) -> bool {
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
        "isolated Settings error test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "the isolated child must execute this test: {stdout}"
    );
    false
}

fn settings_window(cx: &mut TestAppContext) -> (Entity<AwiliApp>, &mut VisualTestContext) {
    let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    app.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.open_settings(cx);
    });
    window.run_until_parked();
    (app, window)
}

fn selector(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

fn inside(outer: gpui::Bounds<gpui::Pixels>, inner: gpui::Bounds<gpui::Pixels>) -> bool {
    inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
}

fn intersection(
    a: gpui::Bounds<gpui::Pixels>,
    b: gpui::Bounds<gpui::Pixels>,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    let left = a.left().max(b.left());
    let top = a.top().max(b.top());
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (left < right && top < bottom)
        .then(|| gpui::Bounds::from_corners(point(left, top), point(right, bottom)))
}

fn visible_body(
    window: &mut VisualTestContext,
    scroll: &gpui::ScrollHandle,
) -> gpui::Bounds<gpui::Pixels> {
    let size = window.update(|window, _| window.viewport_size());
    let physical_window = gpui::Bounds::new(point(px(0.), px(0.)), size);
    intersection(scroll.bounds(), physical_window)
        .expect("the error Sheet body has a visible physical viewport")
}

fn painted_line(
    window: &mut VisualTestContext,
    line: usize,
    fragment: &str,
    visible: gpui::Bounds<gpui::Pixels>,
) -> bool {
    crate::ui::text::DRAWN_TEXTS.with(|drawn| drawn.borrow_mut().clear());
    window.update(|window, _| window.refresh());
    window.run_until_parked();
    let name = selector(&format!("ui-text-settings-full-error-line-{line}"));
    let Some(bounds) = window.debug_bounds(name) else {
        return false;
    };
    crate::ui::text::DRAWN_TEXTS.with(|drawn| {
        drawn.borrow().iter().any(|layout| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (layout.text().to_string(), layout.bounds())
            }))
            .ok()
            .is_some_and(|(text, text_bounds)| {
                text.contains(fragment)
                    && inside(bounds, text_bounds)
                    && inside(visible, text_bounds)
            })
        })
    })
}

fn is_focused(window: &mut VisualTestContext, handle: &FocusHandle) -> bool {
    window.update(|window, _| handle.is_focused(window))
}

/// Drive the real pointer-down focus transition, capture the native focus
/// target immediately before pointer-up dispatches the opener, and finish the
/// same click a user makes.
fn click_and_capture_parent(
    window: &mut VisualTestContext,
    at: gpui::Point<gpui::Pixels>,
) -> FocusHandle {
    window.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: at,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    window.run_until_parked();
    let parent = window
        .update(|window, cx| window.focused(cx))
        .expect("the real Settings opener has native focus before pointer-up");
    window.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position: at,
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    window.run_until_parked();
    parent
}

#[gpui::test]
fn error_details_sheet_is_keyboard_scrollable_and_restores_settings_focus(cx: &mut TestAppContext) {
    const TEST: &str =
        "settings_pages::error_details_tests::error_details_sheet_is_keyboard_scrollable_and_restores_settings_focus";
    if !isolated(TEST) {
        return;
    }

    let (app, window) = settings_window(cx);
    window.simulate_resize(size(px(900.), px(712.)));
    window.update(|window, cx| {
        app.update(cx, |app, cx| app.apply_ui_scale(2.5, window, cx));
    });

    let mut lines = vec!["UNIQUE ERROR PREFIX: settings validation failed".to_string()];
    lines.extend(
        (0..48).map(|i| format!("Detail line {i}: {}", "configuration context ".repeat(8))),
    );
    lines.push("UNIQUE ERROR SUFFIX: the original provider value remains unchanged".to_string());
    let original = lines.join("\n");
    let (parent_focus, previous_stop) = app.update(window, |app, cx| {
        app.settings.error = Some(original.clone());
        app.settings.status = None;
        app.settings.tab = settings::Tab::Form;
        app.settings.page = settings_catalog::Page::General;
        app.settings_in_sidebar = false;
        app.settings_nav_focus_pending = false;
        let mut stops = settings_focus::order(
            app.settings.page,
            app.settings
                .doc
                .as_ref()
                .expect("Settings loaded its document"),
            app.settings.last_change().is_some(),
        );
        stops.push(settings_focus::FocusStop::ErrorDetails);
        assert_eq!(stops.last(), Some(&settings_focus::FocusStop::ErrorDetails));
        let previous = stops[stops.len() - 2];
        app.settings_stop = Some(previous);
        cx.notify();
        (app.settings_nav_focus.clone(), previous)
    });
    window.update(|window, cx| window.focus(&parent_focus, cx));
    window.run_until_parked();

    let physical_window = gpui::Bounds::new(
        point(px(0.), px(0.)),
        window.update(|window, _| window.viewport_size()),
    );
    let trigger = window
        .debug_bounds("ui-button-sx-error-details-0")
        .expect("the real KIT Read full error button is rendered");
    let footer = window
        .debug_bounds("sx-debug-footer")
        .expect("the Settings footer remains rendered");
    let page = window
        .debug_bounds("sx-page")
        .expect("the Settings page viewport remains rendered");
    assert!(
        inside(physical_window, footer),
        "footer is contained: {footer:?}"
    );
    assert!(
        inside(footer, trigger),
        "the real action face is contained: {trigger:?}"
    );
    assert!(
        inside(physical_window, trigger),
        "the real error-detail Button face is fully inside the wide physical window: {trigger:?}"
    );
    assert!(inside(physical_window, page) && page.size.height > px(0.));
    assert!(page.origin.y + page.size.height <= footer.origin.y);

    // Tab to the new final Pages stop, then activate it through the app-owned
    // keyboard dispatcher; this must open without changing the original error.
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("ui-button-sx-error-details-0")
            .is_some(),
        "the real error-details action remains mounted after Tab"
    );
    assert_eq!(
        app.read_with(window, |app, _| app.settings_stop),
        Some(settings_focus::FocusStop::ErrorDetails),
        "Tab selects the real error-details action"
    );
    let parent_focus = window
        .update(|window, cx| window.focused(cx))
        .expect("the real Settings action has native focus before Enter");
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("settings-error-details-sheet-panel")
            .is_some(),
        "real Tab then Enter opens the bounded KIT Sheet"
    );
    assert_eq!(
        app.read_with(window, |app, _| app.settings.error.clone()),
        Some(original.clone()),
        "opening details does not alter the original error"
    );
    window.simulate_keystrokes("cmd-f");
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("settings-error-details-sheet-panel")
            .is_some(),
        "the Sheet remains open while app-wide chords are pressed"
    );
    assert!(
        !app.read_with(window, |app, _| app.search.open),
        "⌘F cannot open conversation search behind the Sheet"
    );
    let sheet = window
        .debug_bounds("settings-error-details-sheet-panel")
        .expect("the Sheet face is painted");
    assert!(
        inside(physical_window, sheet),
        "Sheet stays in the physical window: {sheet:?}"
    );
    let body = window
        .debug_bounds("settings-error-details-body")
        .expect("the scroll body is painted");
    let scroll = app.read_with(window, |app, _| app.settings_error_scroll.clone());
    let visible = visible_body(window, &scroll);
    assert!(
        body.size.height > visible.size.height,
        "the complete detail content exceeds its clipped viewport"
    );
    assert!(
        inside(physical_window, visible),
        "the ScrollHandle viewport remains inside the physical window: {visible:?}"
    );
    assert!(visible.size.height > px(0.));
    let max = scroll.max_offset().y;
    let viewport_height = f32::from(scroll.bounds().size.height);
    assert!(
        f32::from(max) > 0. && viewport_height > 0.,
        "the detail body has a real scroll range"
    );
    assert!(
        painted_line(window, 0, "UNIQUE ERROR PREFIX", visible),
        "fresh-frame KIT text paints the complete unique prefix inside the body and window"
    );

    let sheet_focus = app.read_with(window, |app, _| app.settings_error_sheet_focus.clone());
    let body_focus = app.read_with(window, |app, _| app.settings_error_body_focus.clone());
    for _ in 0..6 {
        if is_focused(window, &body_focus) {
            break;
        }
        window.simulate_keystrokes("tab");
        window.run_until_parked();
        assert!(
            window.update(|window, cx| { sheet_focus.contains_focused(window, cx) }),
            "Tab stays inside the error Sheet"
        );
    }
    assert!(
        is_focused(window, &body_focus),
        "Tab reaches the real scroll body"
    );

    let before = scroll.offset().y;
    let max_steps = (f32::from(max) / (viewport_height * 0.9)).ceil() as usize + 2;
    let mut moved = false;
    let mut prior = before;
    for _ in 0..max_steps {
        window.simulate_keystrokes("pagedown");
        window.run_until_parked();
        let next = scroll.offset().y;
        let unchanged = next == prior;
        moved |= next < prior;
        prior = next;
        if unchanged {
            break;
        }
        if f32::from(next) <= -f32::from(max) + 0.5 {
            prior = next;
            break;
        }
    }
    assert!(
        moved && prior < before,
        "real PageDown moves the shared ScrollHandle"
    );
    assert!(
        (f32::from(prior) + f32::from(max)).abs() <= 1.0,
        "PageDown reaches the measured maximum offset: {prior:?} / {max:?}"
    );
    let bottom_visible = visible_body(window, &scroll);
    assert!(
        painted_line(
            window,
            lines.len() - 1,
            "UNIQUE ERROR SUFFIX",
            bottom_visible
        ),
        "fresh-frame paint reveals the complete unique suffix inside the body and window"
    );

    let at_suffix = prior;
    let mut after_up = at_suffix;
    for _ in 0..max_steps {
        window.simulate_keystrokes("pageup");
        window.run_until_parked();
        let next = scroll.offset().y;
        let unchanged = next == after_up;
        after_up = next;
        if unchanged || next == px(0.) {
            break;
        }
    }
    assert!(
        (f32::from(after_up)).abs() <= 1.0,
        "PageUp reaches the measured top offset: {after_up:?}"
    );
    assert!(
        after_up > at_suffix,
        "real PageUp moves back toward the prefix"
    );
    let top_visible = visible_body(window, &scroll);
    assert!(
        painted_line(window, 0, "UNIQUE ERROR PREFIX", top_visible),
        "fresh-frame paint shows the complete prefix after returning to the top"
    );

    // A further Tab remains within the Sheet, and Escape restores the exact
    // parent focus handle and logical stop.
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    assert!(
        window.update(|window, cx| { sheet_focus.contains_focused(window, cx) }),
        "Tab cannot escape to the Settings page"
    );
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("settings-error-details-sheet-panel")
            .is_none(),
        "Escape closes the Sheet"
    );
    assert!(
        is_focused(window, &parent_focus),
        "Escape restores the exact parent focus handle"
    );
    assert_eq!(
        app.read_with(window, |app, _| app.settings_stop),
        Some(settings_focus::FocusStop::ErrorDetails)
    );
    assert_eq!(
        app.read_with(window, |app, _| app.settings.error.clone()),
        Some(original.clone()),
        "closing the Sheet leaves the original error untouched"
    );
    assert_eq!(
        app.read_with(window, |app, _| app.settings_stop),
        Some(settings_focus::FocusStop::ErrorDetails),
        "the prior focused Settings action is restored"
    );
    assert_ne!(previous_stop, settings_focus::FocusStop::ErrorDetails);

    // The close Button is also a real action: reopen from the actual footer
    // face, then click the KIT close control.
    let trigger = window
        .debug_bounds("ui-button-sx-error-details-0")
        .expect("the footer action remains visible after close");
    let pointer_parent_focus = click_and_capture_parent(window, trigger.center());
    assert!(
        window
            .debug_bounds("settings-error-details-sheet-panel")
            .is_some(),
        "a real pointer click reopens the error Sheet"
    );
    let close = window
        .debug_bounds("ui-icon-button-settings-error-details-sheet-close")
        .expect("the Sheet's real close Button is visible");
    window.simulate_click(close.center(), Modifiers::default());
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("settings-error-details-sheet-panel")
            .is_none(),
        "the Sheet close Button closes the detail view"
    );
    assert_eq!(
        app.read_with(window, |app, _| app.settings.error.clone()),
        Some(original.clone()),
        "closing by Button preserves the same error"
    );
    assert!(
        is_focused(window, &pointer_parent_focus),
        "the close Button restores the native pointer-opener focus"
    );

    // The Models & Fleet provider form has its own app-owned action list.
    // Read full error follows its existing Save/Discard actions without
    // changing any pre-existing selection order.
    app.update(window, |app, cx| {
        app.settings.page = settings_catalog::Page::Models;
        app.settings_nav.select(settings_nav::Action::Discard);
        app.settings_nav_focus_pending = false;
        cx.notify();
    });
    window.update(|window, cx| window.focus(&parent_focus, cx));
    window.run_until_parked();
    window.simulate_keystrokes("tab enter");
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("settings-error-details-sheet-panel")
            .is_some(),
        "the Models action list reaches the real error details Sheet"
    );
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(is_focused(window, &parent_focus));

    // The forced compact Sheet remains bounded at a conventional narrow
    // window as well as the wide 900px window above.
    window.simulate_resize(size(px(760.), px(712.)));
    app.update(window, |app, cx| {
        app.settings.page = settings_catalog::Page::General;
        app.settings_stop = Some(previous_stop);
        app.settings_in_sidebar = false;
        app.settings_nav_focus_pending = false;
        cx.notify();
    });
    window.update(|window, cx| window.focus(&parent_focus, cx));
    window.run_until_parked();
    let narrow_window = gpui::Bounds::new(
        point(px(0.), px(0.)),
        window.update(|window, _| window.viewport_size()),
    );
    let narrow_trigger = window
        .debug_bounds("ui-button-sx-error-details-0")
        .expect("the Read full error action is visible at the narrow size");
    assert!(
        inside(narrow_window, narrow_trigger),
        "the real error-detail Button face is fully inside the narrow physical window: {narrow_trigger:?}"
    );
    window.simulate_click(narrow_trigger.center(), Modifiers::default());
    window.run_until_parked();
    let narrow_sheet = window
        .debug_bounds("settings-error-details-sheet-panel")
        .expect("the Sheet opens at the narrow window size");
    assert!(inside(narrow_window, narrow_sheet));

    // Open the real body and reach its measured bottom before replacing the
    // error. The new live message must reset its offset to reveal its prefix.
    let sheet_focus = app.read_with(window, |app, _| app.settings_error_sheet_focus.clone());
    let body_focus = app.read_with(window, |app, _| app.settings_error_body_focus.clone());
    for _ in 0..6 {
        if is_focused(window, &body_focus) {
            break;
        }
        window.simulate_keystrokes("tab");
        window.run_until_parked();
        assert!(window.update(|window, cx| sheet_focus.contains_focused(window, cx)));
    }
    assert!(is_focused(window, &body_focus));
    let scroll = app.read_with(window, |app, _| app.settings_error_scroll.clone());
    let max = scroll.max_offset().y;
    let viewport_height = f32::from(scroll.bounds().size.height);
    let max_steps = (f32::from(max) / (viewport_height * 0.9)).ceil() as usize + 2;
    let mut current = scroll.offset().y;
    for _ in 0..max_steps {
        window.simulate_keystrokes("pagedown");
        window.run_until_parked();
        let next = scroll.offset().y;
        let unchanged = next == current;
        current = next;
        if unchanged || f32::from(next) <= -f32::from(max) + 0.5 {
            break;
        }
    }
    assert!(
        (f32::from(current) + f32::from(max)).abs() <= 1.0,
        "the pointer-open Sheet is scrolled to its measured end before replacement"
    );

    // The detail text follows live error changes; clearing the error closes
    // the view and restores a valid Settings focus target.
    let replacement = format!(
        "LIVE ERROR REPLACEMENT PREFIX\n{}\nLIVE ERROR REPLACEMENT SUFFIX",
        "replacement details ".repeat(80)
    );
    app.update(window, |app, cx| {
        app.settings.error = Some(replacement.clone());
        cx.notify();
    });
    window.run_until_parked();
    let scroll = app.read_with(window, |app, _| app.settings_error_scroll.clone());
    assert!(
        f32::from(scroll.offset().y).abs() <= 1.0,
        "a replacement error resets the old end-of-message offset"
    );
    let replacement_visible = visible_body(window, &scroll);
    assert!(
        painted_line(
            window,
            0,
            "LIVE ERROR REPLACEMENT PREFIX",
            replacement_visible,
        ),
        "the Sheet paints the current replacement error"
    );
    let replacement_visible = visible_body(window, &scroll);
    assert!(
        !painted_line(window, 0, "UNIQUE ERROR PREFIX", replacement_visible,),
        "the Sheet does not retain a stale copy of the previous error"
    );
    app.update(window, |app, cx| {
        app.settings.error = None;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window
        .debug_bounds("settings-error-details-sheet-panel")
        .is_none());
    assert!(!app.read_with(window, |app, _| app.settings_error_details_open));
    assert_ne!(
        app.read_with(window, |app, _| app.settings_stop),
        Some(settings_focus::FocusStop::ErrorDetails),
        "clearing the error removes the now-invalid keyboard stop"
    );
    assert!(is_focused(window, &parent_focus));

    // Closing Settings while its Sheet is open leaves focus restoration to
    // Settings' normal close path; it must not refocus the hidden trigger.
    app.update(window, |app, cx| {
        app.settings.error = Some("CLOSE SETTINGS WHILE DETAILS ARE OPEN".into());
        app.settings.page = settings_catalog::Page::General;
        app.settings_stop = Some(previous_stop);
        app.settings_in_sidebar = false;
        cx.notify();
    });
    window.update(|window, cx| window.focus(&parent_focus, cx));
    window.run_until_parked();
    window.simulate_keystrokes("tab enter");
    window.run_until_parked();
    assert!(window
        .debug_bounds("settings-error-details-sheet-panel")
        .is_some());
    app.update(window, |app, cx| app.close_settings(cx));
    window.run_until_parked();
    assert!(!app.read_with(window, |app, _| app.settings.open));
    assert!(!app.read_with(window, |app, _| app.settings_error_details_open));
    assert!(window
        .debug_bounds("settings-error-details-sheet-panel")
        .is_none());
    assert!(
        !is_focused(window, &parent_focus),
        "closing Settings does not restore focus to its hidden trigger"
    );

    // Raw and Fields errors can also disappear while their footer action is
    // open. The persistent active-tab target remains valid after the button
    // itself is removed.
    let active_tab_focus = app.read_with(window, |app, _| app.settings_active_tab_focus.clone());
    for tab in [settings::Tab::Raw, settings::Tab::Fields] {
        app.update(window, |app, cx| {
            app.settings.open();
            app.settings.tab = tab;
            app.settings.error = Some(format!("{tab:?} TAB ERROR TO CLEAR"));
            app.settings_error_details_open = false;
            app.settings_error_parent_focus = None;
            cx.notify();
        });
        window.run_until_parked();
        let trigger = window
            .debug_bounds("ui-button-sx-error-details-0")
            .expect("the active tab renders its real error-details action");
        let pointer_parent_focus = click_and_capture_parent(window, trigger.center());
        assert!(
            window
                .debug_bounds("settings-error-details-sheet-panel")
                .is_some(),
            "pointer opens the detail Sheet on {tab:?}"
        );
        let close = window
            .debug_bounds("ui-icon-button-settings-error-details-sheet-close")
            .expect("the Sheet close Button is visible");
        window.simulate_click(close.center(), Modifiers::default());
        window.run_until_parked();
        assert!(
            window
                .debug_bounds("settings-error-details-sheet-panel")
                .is_none(),
            "the close Button closes a {tab:?} Sheet"
        );
        assert!(
            is_focused(window, &pointer_parent_focus),
            "normal {tab:?} close restores the native pointer-opener focus"
        );

        let trigger = window
            .debug_bounds("ui-button-sx-error-details-0")
            .expect("the active tab's error action remains available after close");
        let _clear_parent_focus = click_and_capture_parent(window, trigger.center());
        assert!(
            window
                .debug_bounds("settings-error-details-sheet-panel")
                .is_some(),
            "pointer reopens the detail Sheet before clearing {tab:?}"
        );
        app.update(window, |app, cx| {
            app.settings.error = None;
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window
                .debug_bounds("settings-error-details-sheet-panel")
                .is_none(),
            "clearing a {tab:?} error closes its Sheet"
        );
        assert!(
            is_focused(window, &active_tab_focus),
            "clearing a {tab:?} error focuses the persistent active-tab target"
        );
    }
}
