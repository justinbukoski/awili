//! SXc proof tests: the real `AwiliApp` in a test window, driven by mouse
//! clicks on the rendered Settings pages. Each test runs in a subprocess with
//! a private, empty HOME (the `stream_turn_tests` isolation), so the first
//! click also proves the first-run create-only save.
use super::*;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_SXC_TEST_HOME";

/// Run `test` in a subprocess with a private HOME. `true` inside the child
/// (run the body); `false` in the parent once the child passed.
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

/// The real app in a window with Settings open.
fn settings_window(cx: &mut TestAppContext) -> (Entity<AwiliApp>, &mut VisualTestContext) {
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.open_settings(cx);
    });
    window.run_until_parked();
    (view, window)
}

/// gpui looks debug selectors up by `&'static str`.
fn sel(selector: &str) -> &'static str {
    Box::leak(selector.to_string().into_boxed_str())
}

/// Click the centre of the element with this debug selector.
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

fn bounds_intersection(
    a: gpui::Bounds<gpui::Pixels>,
    b: gpui::Bounds<gpui::Pixels>,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    let left = a.origin.x.max(b.origin.x);
    let top = a.origin.y.max(b.origin.y);
    let right = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let bottom = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    (right > left && bottom > top).then(|| gpui::Bounds {
        origin: gpui::point(left, top),
        size: gpui::size(right - left, bottom - top),
    })
}

/// Reveal an actual KIT Button root inside the rendered Settings-page
/// viewport. A too-short viewport is reported with the parent/layout bounds
/// captured before the first wheel event.
fn reveal_settings_button(
    window: &mut VisualTestContext,
    selector: &str,
    window_bounds: gpui::Bounds<gpui::Pixels>,
    layout: &str,
) -> (gpui::Bounds<gpui::Pixels>, gpui::Bounds<gpui::Pixels>, bool) {
    let mut initial = None;
    for _ in 0..32 {
        let page = window
            .debug_bounds("sx-page")
            .expect("the Settings page viewport is rendered");
        let visible = bounds_intersection(page, window_bounds)
            .expect("the Settings page has a physical visible area");
        let face = window
            .debug_bounds(sel(selector))
            .unwrap_or_else(|| panic!("the actual KIT Button {selector} is rendered"));
        assert!(
            visible.size.height >= face.size.height,
            "the viewport must be tall enough for the actual Button face: {selector} {face:?}, viewport {visible:?}; layout: {layout}"
        );
        if face.origin.x >= visible.origin.x
            && face.origin.y >= visible.origin.y
            && face.origin.x + face.size.width <= visible.origin.x + visible.size.width
            && face.origin.y + face.size.height <= visible.origin.y + visible.size.height
        {
            let did_scroll = initial.is_some();
            return (initial.unwrap_or(face), face, did_scroll);
        }
        initial.get_or_insert(face);
        let face_bottom = face.origin.y + face.size.height;
        let visible_bottom = visible.origin.y + visible.size.height;
        let delta_y = if face_bottom > visible_bottom {
            -((face_bottom - visible_bottom + px(2.)).min(px(120.)))
        } else {
            (visible.origin.y - face.origin.y + px(2.)).min(px(120.))
        };
        window.simulate_event(gpui::ScrollWheelEvent {
            position: visible.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), delta_y)),
            ..Default::default()
        });
        window.run_until_parked();
    }
    let page = window
        .debug_bounds("sx-page")
        .expect("the Settings page viewport");
    let visible = bounds_intersection(page, window_bounds).unwrap();
    let face = window.debug_bounds(sel(selector)).unwrap();
    panic!("wheel did not reveal actual KIT Button {selector}: {face:?} / {visible:?}; layout: {layout}");
}

/// Reveal a real Settings sidebar Row through its scroll viewport. In
/// particular, the higher zoom matrix must not click a clipped page label.
fn reveal_settings_nav(
    window: &mut VisualTestContext,
    selector: &str,
    window_bounds: gpui::Bounds<gpui::Pixels>,
) -> (gpui::Bounds<gpui::Pixels>, gpui::Bounds<gpui::Pixels>, bool) {
    let mut initial = None;
    for _ in 0..32 {
        let scroll = window
            .debug_bounds("sx-debug-nav-scroll")
            .expect("the sidebar page-list scroll viewport");
        let visible = bounds_intersection(scroll, window_bounds)
            .expect("the sidebar page-list has a physical visible area");
        let face = window
            .debug_bounds(sel(selector))
            .unwrap_or_else(|| panic!("the actual sidebar Row {selector} is rendered"));
        assert!(
            face.size.height <= visible.size.height,
            "the sidebar viewport can contain the actual Row face: {face:?} / {visible:?}"
        );
        if face.origin.x >= visible.origin.x
            && face.origin.y >= visible.origin.y
            && face.origin.x + face.size.width <= visible.origin.x + visible.size.width
            && face.origin.y + face.size.height <= visible.origin.y + visible.size.height
        {
            let did_scroll = initial.is_some();
            return (initial.unwrap_or(face), face, did_scroll);
        }
        initial.get_or_insert(face);
        let face_bottom = face.origin.y + face.size.height;
        let visible_bottom = visible.origin.y + visible.size.height;
        let delta_y = if face_bottom > visible_bottom {
            -((face_bottom - visible_bottom + px(2.)).min(px(120.)))
        } else {
            (visible.origin.y - face.origin.y + px(2.)).min(px(120.))
        };
        window.simulate_event(gpui::ScrollWheelEvent {
            position: visible.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), delta_y)),
            ..Default::default()
        });
        window.run_until_parked();
    }
    let face = window.debug_bounds(sel(selector)).unwrap();
    let scroll = window.debug_bounds("sx-debug-nav-scroll").unwrap();
    let visible = bounds_intersection(scroll, window_bounds).unwrap();
    panic!("wheel did not reveal actual sidebar Row {selector}: {face:?} / {visible:?}");
}

/// The catalog index an item's element ids use.
fn ix(id: &str) -> usize {
    cat::ITEMS.iter().position(|i| i.id == id).unwrap()
}

/// The settings file on disk ("" before the first save).
fn config_text(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> String {
    view.update(window, |app, _| {
        std::fs::read_to_string(app.settings.path()).unwrap_or_default()
    })
}

fn page(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> Page {
    view.update(window, |app, _| app.settings.page)
}

#[gpui::test]
fn sxc_sidebar_has_nine_pages_and_opens_on_general(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::tests::sxc_sidebar_has_nine_pages_and_opens_on_general")
    {
        return;
    }
    let (view, window) = settings_window(cx);
    assert_eq!(page(&view, window), Page::General);
    for n in 0..Page::ALL.len() {
        let b = window
            .debug_bounds(sel(&format!("sx-nav-{n}")))
            .expect("every page is listed");
        assert!(b.size.height >= px(HIT), "page {n} is a big target: {b:?}");
    }
    assert!(
        !shown(window, "sx-row-work.default_executor"),
        "Workshop defaults are not duplicated on General"
    );
    assert!(shown(window, "sx-row-voice.auto_speak"));
    assert!(!shown(window, "sx-row-memory.write"), "but not the rest");
    assert!(
        !shown(window, "sx-row-memory.token"),
        "and never an advanced one"
    );
    // SXc-FIX1: every open lands on General, not only the first one. Visit
    // Voice, close with Done, reopen through the ⌘, handler.
    click(window, "sx-nav-7");
    assert_eq!(page(&view, window), Page::Voice);
    click(window, "sx-done-0");
    assert!(
        !view.update(window, |app, _| app.settings.open),
        "Done closed Settings"
    );
    view.update(window, |app, cx| app.toggle_settings(cx));
    window.run_until_parked();
    assert!(view.update(window, |app, _| app.settings.open), "reopened");
    assert_eq!(
        page(&view, window),
        Page::General,
        "a reopen lands on General"
    );
    assert!(
        !shown(window, "sx-row-work.default_executor"),
        "General keeps its own rows"
    );
}

#[gpui::test]
fn sxc_clicking_a_page_shows_only_its_settings(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::tests::sxc_clicking_a_page_shows_only_its_settings") {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-7");
    assert_eq!(page(&view, window), Page::Voice);
    assert!(shown(window, "sx-row-voice.vad_silence_windows"));
    assert!(!shown(window, "sx-row-memory.recall"));
    assert!(
        !shown(window, "sx-row-voice.model"),
        "advanced stays on Advanced"
    );
    click(window, "sx-nav-4");
    assert_eq!(page(&view, window), Page::Memory);
    assert!(shown(window, "sx-row-memory.write"));
    click(window, "sx-nav-8");
    assert!(shown(window, "sx-row-memory.token") && shown(window, "sx-row-voice.model"));
    click(window, "sx-every-0");
    let tab = view.update(window, |app, _| app.settings.tab);
    assert_eq!(
        tab,
        settings::Tab::Fields,
        "the old every-field view is one click away"
    );
    click(window, "sx-nav-2");
    let (tab, now) = view.update(window, |app, _| (app.settings.tab, app.settings.page));
    assert_eq!((tab, now), (settings::Tab::Form, Page::Workshop));
    for id in ["work.default_executor", "work.default_approval_mode"] {
        assert!(
            shown(window, &format!("sx-row-{id}")),
            "Workshop includes {id}"
        );
    }
}

#[gpui::test]
fn sxc_toggle_saves_at_once_then_reset_and_undo(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::tests::sxc_toggle_saves_at_once_then_reset_and_undo") {
        return;
    }
    let (view, window) = settings_window(cx);
    let recall = ix("memory.recall");
    click(window, "sx-nav-4");
    assert!(
        !shown(window, &format!("sx-reset-{recall}")),
        "default: no Reset"
    );
    click(window, &format!("sx-on-{recall}"));
    assert!(
        config_text(&view, window).contains("recall = true"),
        "saved at once, no Save button"
    );
    let status = view.update(window, |app, _| app.settings.status.clone());
    assert_eq!(
        status.as_deref(),
        Some("Saved: Use my memory when answering")
    );
    let reset = window
        .debug_bounds(sel(&format!("sx-reset-{recall}")))
        .expect("changed: Reset shows");
    assert!(reset.size.height >= px(HIT));
    click(window, &format!("sx-reset-{recall}"));
    assert!(
        !config_text(&view, window).contains("recall ="),
        "reset removes the key"
    );
    assert!(!shown(window, &format!("sx-reset-{recall}")));
    click(window, "sx-undo-0");
    assert!(
        config_text(&view, window).contains("recall = true"),
        "undo brings the change back"
    );
    let status = view.update(window, |app, _| app.settings.status.clone());
    assert_eq!(
        status.as_deref(),
        Some("Undone: Use my memory when answering")
    );
}

#[gpui::test]
fn sxc_steppers_and_choices_save_with_one_click(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::tests::sxc_steppers_and_choices_save_with_one_click") {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-7");
    click(
        window,
        &format!("sx-inc-{}", ix("voice.vad_silence_windows")),
    );
    assert!(config_text(&view, window).contains("vad_silence_windows = 35"));
    click(
        window,
        &format!("sx-choice-{}", ix("voice.ptt_key") * 16 + 4),
    );
    assert!(config_text(&view, window).contains("ptt_key = \"off\""));
    let undo = view.update(window, |app, _| app.settings.undo.len());
    assert_eq!(undo, 2, "each click is one undoable change");
}

/// RC2-UX (R2-21, round 2 Minor 11): a change says "✓ Saved" right on its
/// row, next to the control (the line at the bottom was small and far
/// away), a Reset says "✓ Back to default", and nothing moves under the
/// pointer: the Reset slot is kept while the row is at its default.
#[gpui::test]
fn rc2ux_a_change_says_saved_on_its_row_and_nothing_moves(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_pages::tests::rc2ux_a_change_says_saved_on_its_row_and_nothing_moves",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    let recall = ix("memory.recall");
    click(window, "sx-nav-4");
    let off = sel(&format!("sx-on-{recall}"));
    let before = window.debug_bounds(off).expect("the On button");
    // Within a pixel (text rounding): the pointer stays on the button.
    let still = |w: &mut VisualTestContext| {
        let now = w.debug_bounds(off).expect("the On button");
        (now.left() - before.left()).abs() <= px(2.)
            && (now.right() - before.right()).abs() <= px(2.)
            && now.top() == before.top()
    };
    assert!(
        shown(window, &format!("sx-reset-slot-{recall}")),
        "the slot is kept"
    );
    assert!(!shown(window, "sx-saved-here-memory.recall"));
    click(window, &format!("sx-on-{recall}"));
    assert!(still(window), "On did not move");
    assert!(
        shown(window, "sx-saved-here-memory.recall"),
        "said on the row"
    );
    let mark = view.update(window, |app, _| app.settings.saved_here);
    assert_eq!(mark, Some(("memory.recall", "\u{2713} Saved")));
    let reset = window
        .debug_bounds(sel(&format!("sx-reset-{recall}")))
        .expect("Reset shows");
    let slot_was = before.right() < reset.left();
    assert!(
        slot_was,
        "Reset sits where its slot was: {before:?} {reset:?}"
    );
    click(window, &format!("sx-reset-{recall}"));
    assert!(still(window), "still did not move");
    let mark = view.update(window, |app, _| app.settings.saved_here);
    assert_eq!(mark, Some(("memory.recall", "\u{2713} Back to default")));
    click(window, "sx-nav-7");
    assert!(view.update(window, |app, _| app.settings.saved_here.is_none()));
}

/// RC2-UX (R2-21, round 2 Minor 12): after a Reset by keyboard the focus
/// stays on that row (its first button), not on the page's first control.
#[gpui::test]
fn rc2ux_the_keyboard_stays_on_the_row_after_a_reset(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_pages::tests::rc2ux_the_keyboard_stays_on_the_row_after_a_reset",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    let write = ix("memory.write");
    click(window, "sx-nav-4");
    click(window, &format!("sx-off-{write}"));
    let stop = view.update_in(window, |app, w, cx| {
        app.settings_in_sidebar = false;
        app.settings_stop = Some(crate::settings_focus::FocusStop::Reset("memory.write"));
        app.settings_key(&gpui::Keystroke::parse("space").unwrap(), w, cx);
        app.settings_stop
    });
    window.run_until_parked();
    assert!(!config_text(&view, window).contains("write ="), "reset");
    assert_eq!(
        stop,
        Some(crate::settings_focus::FocusStop::On("memory.write")),
        "the keyboard stays on the row"
    );
}

/// RC2-UX (R2-21, round 2 Minor 11): the text size buttons say what they
/// did on the change line (it kept "Last change: Chat assistant"); and
/// (S-keys) the keyboard ring is on the open page when Settings opens.
#[gpui::test]
fn rc2ux_text_size_says_so_and_the_ring_shows_on_open(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_pages::tests::rc2ux_text_size_says_so_and_the_ring_shows_on_open",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    let ring = view.update(window, |app, _| {
        app.settings_in_sidebar && app.settings_nav_focused
    });
    assert!(ring, "the open page carries the keyboard ring on open");
    click(window, "sx-zoom-in-0");
    let pct = (crate::theme::ui_scale() * 100.0).round() as i32;
    let status = view.update(window, |app, _| app.settings.status.clone());
    assert_eq!(status, Some(format!("Text size: {pct}%")));
    click(window, "sx-zoom-reset-0");
}

/// RC2-UX (R2-23): "Send a test notification" says in Awili what happened
/// (only the macOS banner was posted).
#[gpui::test]
fn rc2ux_a_test_notification_says_so_in_awili(cx: &mut TestAppContext) {
    if !in_isolated_child("settings_pages::tests::rc2ux_a_test_notification_says_so_in_awili") {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-6");
    click(window, "sx-notify-test-0");
    // RC2-FIXB4: macOS is being asked (the recording backend starts with
    // nothing asked), and the toast says so.
    let (toast, want) = view.update(window, |app, _| {
        (
            app.toast.visible().map(|s| s.to_string()),
            crate::settings_pages::test_notice(crate::notify_hooks::TestOutcome::Asking),
        )
    });
    assert_eq!(toast.as_deref(), Some(want));
    for o in [
        crate::notify_hooks::TestOutcome::Refused,
        crate::notify_hooks::TestOutcome::Shown,
    ] {
        assert!(!crate::settings_pages::test_notice(o).is_empty());
    }
}

/// The denied-permission actions stay fully reachable when the Settings page
/// is narrow or the interface is zoomed. Exercise the rendered buttons and
/// their recording-backend effects, not just the Card's layout metadata.
#[gpui::test]
fn rc2ux_notification_card_actions_wrap_and_remain_usable(cx: &mut TestAppContext) {
    use crate::notify_hooks::TestOutcome;
    use crate::notify_platform::{BackendSignal, Recorded};

    if !in_isolated_child(
        "settings_pages::tests::rc2ux_notification_card_actions_wrap_and_remain_usable",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    for (width, height, scale) in [(640.0, 720.0, 1.0), (900.0, 712.0, 2.5)] {
        window.simulate_resize(size(px(width), px(height)));
        window.update(|win, cx| {
            view.update(cx, |app, cx| app.apply_ui_scale(scale, win, cx));
        });
        view.update(window, |app, cx| {
            app.on_notify_signal(
                BackendSignal::Permission(notifications::Permission::Denied),
                cx,
            );
        });
        window.run_until_parked();

        // Make the footer's real Undo action present at each tested scale.
        // The usual `sx-undo-off` placeholder would not prove the action's
        // actual KIT Button face fits inside the Settings window.
        let recall = ix("memory.recall");
        let window_bounds = gpui::Bounds {
            origin: gpui::point(px(0.), px(0.)),
            size: size(px(width), px(height)),
        };
        if page(&view, window) != Page::Notifications {
            let (initial_notifications_nav, notifications_nav, notifications_nav_scrolled) =
                reveal_settings_nav(window, "ui-row-sx-nav-6", window_bounds);
            if scale > 1.0 {
                assert!(
                    notifications_nav_scrolled,
                    "the Notifications page Row starts clipped at high zoom and is revealed by sidebar wheel input: {initial_notifications_nav:?} -> {notifications_nav:?}"
                );
            }
            click(window, "ui-row-sx-nav-6");
            assert_eq!(page(&view, window), Page::Notifications);
        }
        let (initial_memory_nav, memory_nav, memory_nav_scrolled) =
            reveal_settings_nav(window, "ui-row-sx-nav-4", window_bounds);
        if scale > 1.0 {
            assert!(
                memory_nav_scrolled,
                "the memory page Row starts clipped at high zoom and is revealed by sidebar wheel input: {initial_memory_nav:?} -> {memory_nav:?}"
            );
        }
        click(window, "ui-row-sx-nav-4");
        assert_eq!(page(&view, window), Page::Memory);
        // Check the real KIT Button roots for both values. At large zoom the
        // Boolean pair stacks inside the SettingRow's right slot, but each
        // face keeps its normal target size and remains independently usable.
        let on_selector = format!("ui-button-sx-on-{recall}");
        let (initial_on, on, _) = reveal_settings_button(
            window,
            &on_selector,
            window_bounds,
            "checking the actual On Button face",
        );
        let off_selector = format!("ui-button-sx-off-{recall}");
        let (initial_off, off, _) = reveal_settings_button(
            window,
            &off_selector,
            window_bounds,
            "preparing a real Undo action",
        );
        click(window, &on_selector);
        assert!(
            config_text(&view, window).contains("recall = true"),
            "the visible On control creates a real live setting change before checking Undo: On={on:?} initial={initial_on:?}; Off={off:?} initial={initial_off:?}"
        );
        let (initial_notifications_nav, notifications_nav, notifications_nav_scrolled) =
            reveal_settings_nav(window, "ui-row-sx-nav-6", window_bounds);
        if scale > 1.0 {
            assert!(
                notifications_nav_scrolled,
                "the Notifications page Row starts clipped at high zoom and is revealed by sidebar wheel input: {initial_notifications_nav:?} -> {notifications_nav:?}"
            );
        }
        click(window, "ui-row-sx-nav-6");
        assert_eq!(page(&view, window), Page::Notifications);

        let fully_inside = |outer: gpui::Bounds<gpui::Pixels>,
                            inner: gpui::Bounds<gpui::Pixels>| {
            inner.origin.x >= outer.origin.x
                && inner.origin.y >= outer.origin.y
                && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
                && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
        };
        let layout = format!(
            "panel={:?}; sidebar={:?}; header={:?}; content={:?}; footer={:?}; footer_row={:?}; status={:?}; undo_wrapper={:?}; undo_face={:?}; page={:?}",
            window.debug_bounds("sx-panel"),
            window.debug_bounds("sx-debug-sidebar"),
            window.debug_bounds("sx-debug-header"),
            window.debug_bounds("sx-debug-content"),
            window.debug_bounds("sx-debug-footer"),
            window.debug_bounds("sx-debug-footer-row"),
            window.debug_bounds("sx-status"),
            window.debug_bounds("sx-undo-0"),
            window.debug_bounds("ui-button-sx-undo-0"),
            window.debug_bounds("sx-page"),
        );
        let status = window.debug_bounds("sx-status").expect("the change status");
        let min_status_width = theme::z(theme::TARGET * 2.0);
        assert!(
            status.size.width >= min_status_width,
            "the status keeps at least two target-widths beside a wrapping Undo action: {status:?}; {layout}"
        );
        let undo = window
            .debug_bounds("ui-button-sx-undo-0")
            .expect("a live change renders the real Undo Button");
        let footer = window
            .debug_bounds("sx-debug-footer")
            .expect("the Settings change footer");
        assert!(
            fully_inside(footer, undo),
            "the actual Undo Button remains inside its footer: {undo:?} / {footer:?}; {layout}"
        );
        assert!(
            fully_inside(window_bounds, undo),
            "the actual Undo Button remains inside the physical window: {undo:?}; {layout}"
        );
        let page_before_undo = window.debug_bounds("sx-page").expect("the Settings page");
        assert!(
            page_before_undo.size.height >= undo.size.height,
            "the actual Undo face has a useful page viewport beside it: {undo:?} / {page_before_undo:?}; {layout}"
        );
        click(window, "ui-button-sx-undo-0");
        assert!(
            !config_text(&view, window).contains("recall = true"),
            "the rendered Undo Button restores the setting at {scale}×"
        );
        let undo_status = view.update(window, |app, _| app.settings.status.clone());
        assert_eq!(
            undo_status.as_deref(),
            Some("Undone: Use my memory when answering"),
            "the real Undo Button callback ran at {scale}×"
        );
        let status_tooltip =
            crate::ui::interaction::test_util::tooltip_at(window, "ui-text-settings-change-status");
        let full_status = format!("{} ✓", undo_status.as_deref().unwrap());
        assert_eq!(
            status_tooltip.as_deref(),
            Some(full_status.as_str()),
            "the compact status keeps its full message available in the KIT tooltip at {scale}×"
        );
        let (initial_send, send, send_scrolled) =
            reveal_settings_button(window, "ui-button-sx-notify-test-0", window_bounds, &layout);
        if send_scrolled {
            assert_ne!(
                send, initial_send,
                "real wheel input changes the actual button bounds at {scale}×; {layout}"
            );
        }
        let page = window.debug_bounds("sx-page").expect("the page viewport");
        let visible = bounds_intersection(page, window_bounds)
            .expect("the Settings page has a physical visible area");
        let card = window
            .debug_bounds("sx-notify-status")
            .expect("the notification Card is visible");
        assert!(
            fully_inside(card, send),
            "test action is inside the Card: {send:?} / {card:?}"
        );
        assert!(
            fully_inside(visible, send),
            "test action is in the physical page and window intersection: {send:?} / {visible:?}; initial={initial_send:?}; {layout}"
        );

        click(window, "ui-button-sx-notify-test-0");
        let toast = view.update(window, |app, _| {
            app.toast.visible().map(|message| message.to_string())
        });
        let expected = crate::settings_pages::test_notice(TestOutcome::Asking);
        assert_eq!(
            toast.as_deref(),
            Some(expected),
            "the test button re-asks for permission at {scale}×"
        );
        let events = view.update(window, |app, _| app.notifier.test_log());
        assert!(
            events.contains(&Recorded::RequestPermission),
            "the test button requests permission at {scale}×"
        );
        view.update(window, |app, cx| {
            app.on_notify_signal(
                BackendSignal::Permission(notifications::Permission::Denied),
                cx,
            );
        });
        window.run_until_parked();
        let toast = view.update(window, |app, _| app.toast.visible().map(|s| s.to_string()));
        let expected = crate::settings_pages::test_notice(TestOutcome::Refused);
        assert_eq!(
            toast.as_deref(),
            Some(expected),
            "the real denied response is reported at {scale}×"
        );
        let card = window
            .debug_bounds("sx-notify-status")
            .expect("the notification Card remains visible");
        let (initial_open, open, open_scrolled) =
            reveal_settings_button(window, "ui-button-sx-notify-sys-0", window_bounds, &layout);
        if open_scrolled {
            assert_ne!(
                open, initial_open,
                "real wheel input changes the actual System Settings button bounds at {scale}×; {layout}"
            );
        }
        let page = window.debug_bounds("sx-page").expect("the page viewport");
        let visible = bounds_intersection(page, window_bounds)
            .expect("the Settings page has a physical visible area");
        let open = window
            .debug_bounds("ui-button-sx-notify-sys-0")
            .expect("the actual Open System Settings Button returns after denial");
        assert!(
            fully_inside(card, open),
            "System Settings is inside the Card after denial: {open:?} / {card:?}"
        );
        assert!(
            fully_inside(visible, open),
            "System Settings is in the physical page and window intersection after denial: {open:?} / {visible:?}"
        );
        click(window, "ui-button-sx-notify-sys-0");
        let events = view.update(window, |app, _| app.notifier.test_log());
        assert!(
            events.contains(&Recorded::OpenSettings),
            "the real System Settings button reaches the recording backend at {scale}×"
        );
    }
}

/// RC2-FIXB4 (RC2-UX review P2: Settings said a test notification was sent
/// while macOS was still being asked, and the send path dropped what was
/// held back): the toast says what happened — macOS being asked (not
/// "sent"), then, when macOS answers, refused or sent; a post macOS could
/// not show goes through Script Editor, and with that off says it was not
/// shown.
#[gpui::test]
fn rc2fixb_a_test_notification_says_what_macos_did(cx: &mut TestAppContext) {
    use crate::notify_hooks::TestOutcome;
    use crate::notify_platform::{BackendSignal, Recorded};
    if !in_isolated_child("settings_pages::tests::rc2fixb_a_test_notification_says_what_macos_did")
    {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-6");
    let toast = |view: &Entity<AwiliApp>, window: &mut VisualTestContext| {
        view.update(window, |app, _| {
            app.toast
                .visible()
                .map(|s| s.to_string())
                .unwrap_or_default()
        })
    };
    let notice = crate::settings_pages::test_notice;
    let answer = |view: &Entity<AwiliApp>, window: &mut VisualTestContext, sig: BackendSignal| {
        view.update(window, |app, cx| app.on_notify_signal(sig, cx));
        window.run_until_parked();
    };

    // Asked, then refused.
    click(window, "sx-notify-test-0");
    let shown = toast(&view, window);
    assert!(!shown.contains("sent"), "nothing was sent yet: {shown}");
    assert_eq!(shown, notice(TestOutcome::Asking));
    answer(
        &view,
        window,
        BackendSignal::Permission(notifications::Permission::Denied),
    );
    assert_eq!(toast(&view, window), notice(TestOutcome::Refused));

    // Asked again, then allowed: the test posts and the toast says sent.
    click(window, "sx-notify-test-0");
    assert_eq!(toast(&view, window), notice(TestOutcome::Asking));
    answer(
        &view,
        window,
        BackendSignal::Permission(notifications::Permission::Authorized),
    );
    assert_eq!(toast(&view, window), notice(TestOutcome::Shown));
    let posted = view.update(window, |app, _| app.notifier.test_log());
    let Some(Recorded::Native(n)) = posted.last().cloned() else {
        panic!("the test was posted: {posted:?}");
    };

    // macOS could not show it: Script Editor shows it instead.
    answer(&view, window, BackendSignal::PostFailed(n));
    assert_eq!(
        toast(&view, window),
        notice(TestOutcome::ShownByScriptEditor)
    );
    let log = view.update(window, |app, _| app.notifier.test_log());
    assert!(matches!(log.last(), Some(Recorded::Fallback(_))), "{log:?}");

    // With Script Editor off, nothing can show it: the press says so.
    view.update(window, |app, _| {
        let cfg = awili_config::NotificationsConfig {
            fallback_osascript: false,
            ..app.notifier.core.config().clone()
        };
        app.notifier.set_config(cfg);
    });
    click(window, "sx-notify-test-0");
    assert_eq!(toast(&view, window), notice(TestOutcome::NotShown));
    let after = view.update(window, |app, _| app.notifier.test_log());
    assert_eq!(after.len(), log.len(), "nothing was posted");
}

/// RC2-FIXC2 (RC2-FIXB review P2: a late native denial left the toast
/// saying "sent"): a test notification Awili said was sent, which macOS
/// then refuses because Awili's notifications are not allowed, says it was
/// refused — right after the owner's Allow, and when notifications were
/// already allowed at the press. The refusal is read through the native
/// post's own error, as the macOS adapter reads it.
#[gpui::test]
fn rc2fixc_a_test_macos_refuses_after_sent_says_refused(cx: &mut TestAppContext) {
    use crate::notify_hooks::TestOutcome;
    use crate::notify_platform::{
        post_error_signal, BackendSignal, Recorded, UN_ERROR_DOMAIN, UN_NOT_ALLOWED,
    };
    if !in_isolated_child(
        "settings_pages::tests::rc2fixc_a_test_macos_refuses_after_sent_says_refused",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    click(window, "sx-nav-6");
    let toast = |view: &Entity<AwiliApp>, window: &mut VisualTestContext| {
        view.update(window, |app, _| {
            app.toast
                .visible()
                .map(|s| s.to_string())
                .unwrap_or_default()
        })
    };
    let notice = crate::settings_pages::test_notice;
    let answer = |view: &Entity<AwiliApp>, window: &mut VisualTestContext, sig: BackendSignal| {
        view.update(window, |app, cx| app.on_notify_signal(sig, cx));
        window.run_until_parked();
    };
    // macOS refuses the test it was handed: the native post's error.
    let refuse_last_test = |view: &Entity<AwiliApp>, window: &mut VisualTestContext| {
        let posted = view.update(window, |app, _| app.notifier.test_log());
        let Some(Recorded::Native(n)) = posted.last().cloned() else {
            panic!("the test was posted: {posted:?}");
        };
        post_error_signal(n, UN_ERROR_DOMAIN, UN_NOT_ALLOWED)
    };

    // Asked, allowed: sent. Then macOS refuses the post itself.
    click(window, "sx-notify-test-0");
    assert_eq!(toast(&view, window), notice(TestOutcome::Asking));
    answer(
        &view,
        window,
        BackendSignal::Permission(notifications::Permission::Authorized),
    );
    assert_eq!(toast(&view, window), notice(TestOutcome::Shown));
    let refused = refuse_last_test(&view, window);
    answer(&view, window, refused);
    assert_eq!(toast(&view, window), notice(TestOutcome::Refused));
    view.update(window, |app, _| {
        assert_eq!(
            app.notifier.core.permission(),
            &notifications::Permission::Denied
        );
    });

    // Allowed again; a press with notifications already allowed says sent
    // at once, and macOS refusing that post says refused.
    click(window, "sx-notify-test-0");
    answer(
        &view,
        window,
        BackendSignal::Permission(notifications::Permission::Authorized),
    );
    click(window, "sx-notify-test-0");
    assert_eq!(toast(&view, window), notice(TestOutcome::Shown));
    let refused = refuse_last_test(&view, window);
    answer(&view, window, refused);
    assert_eq!(toast(&view, window), notice(TestOutcome::Refused));
}
