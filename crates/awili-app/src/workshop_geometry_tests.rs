//! Actual Workshop allocation, painted text and bounded live terminal actions.
use crate::{consolidation_tests::isolated, job_panel::JobTab, theme, work::Mode, AwiliApp};
use gpui::{px, size, Focusable};

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

#[gpui::test]
fn workshop_geometry_retained_draft_has_room_for_its_painted_rows_after_zoom(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_geometry_tests::workshop_geometry_retained_draft_has_room_for_its_painted_rows_after_zoom") { return; }
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.25);
    window.simulate_resize(size(px(860.), px(1100.)));
    window.run_until_parked();
    window.update(|win, cx| win.focus(&app.read(cx).composer.read(cx).focus_handle().clone(), cx));
    let draft =
        "Keep this owned draft\nwhile I change interface zoom.\nThe last line is still here.";
    window.simulate_input(draft);
    for (scale, width) in [
        (1.25, 860.),
        (1., 860.),
        (0.9, 860.),
        (0.9, 998.),
        (1.25, 1400.),
    ] {
        theme::set_ui_scale(scale);
        window.simulate_resize(size(px(width), px(1100.)));
        crate::composer::measure_probe::begin();
        app.update(window, |_, cx| cx.notify());
        window.update(|win, cx| win.draw(cx).clear());
        window.run_until_parked();
        let id = app.read_with(window, |app, _| app.composer.entity_id());
        let samples = crate::composer::measure_probe::take();
        let measured = samples
            .iter()
            .rev()
            .find(|s| s.composer == id && s.measuring)
            .expect("actual editor measurement");
        let painted = samples
            .iter()
            .rev()
            .find(|s| s.composer == id && !s.measuring)
            .expect("actual editor prepaint");
        assert_eq!(
            measured.font, painted.font,
            "measured font must match the owned editor font"
        );
        assert_eq!(
            measured.font_size, painted.font_size,
            "measured font size must match the owned editor paint"
        );
        assert_eq!(
            measured.line_height, painted.line_height,
            "measured row height must match the owned editor paint"
        );
        app.read_with(window, |app, cx| {
            let composer = app.composer.read(cx);
            let (field, text, rows) = composer.painted_geometry();
            assert_eq!(text, draft, "zoom preserves the complete owned draft");
            assert!((3..=theme::COMPOSER_MAX_ROWS).contains(&rows));
            for (top, bottom) in composer.painted_vertical_bounds() {
                assert!(
                    top >= field.top() - px(0.1) && bottom <= field.bottom() + px(0.1),
                    "uncapped draft rows must remain completely visible after {scale}/{width}: {top:?}..{bottom:?} inside {field:?}"
                );
            }
        });
    }
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_soft_wrapped_draft_keeps_every_uncapped_row_after_zoom(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_geometry_tests::workshop_geometry_soft_wrapped_draft_keeps_every_uncapped_row_after_zoom") { return; }
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.25);
    window.simulate_resize(size(px(860.), px(750.)));
    window.run_until_parked();
    window.update(|win, cx| win.focus(&app.read(cx).composer.read(cx).focus_handle().clone(), cx));
    let draft = "Keep this owned draft during the native Conversations check.";
    window.simulate_input(draft);
    for (scale, width) in [
        (1.25, 860.),
        (1., 860.),
        (0.9, 860.),
        (0.9, 998.),
        (1.25, 1400.),
    ] {
        window.update(|win, cx| app.update(cx, |app, cx| app.apply_ui_scale(scale, win, cx)));
        window.simulate_resize(size(px(width), px(750.)));
        crate::composer::measure_probe::begin();
        app.update(window, |_, cx| cx.notify());
        window.update(|win, cx| win.draw(cx).clear());
        window.run_until_parked();
        let id = app.read_with(window, |app, _| app.composer.entity_id());
        let samples = crate::composer::measure_probe::take();
        for sample in &samples {
            eprintln!("TRACE {scale}/{width} {} known={:?} available_width={:?} width={:?} height={:?} rows={}", sample.measuring, sample.known, sample.available_width, sample.width, sample.height, sample.rows);
        }
        let measured = samples
            .iter()
            .rev()
            .find(|s| s.composer == id && s.measuring)
            .expect("actual editor measurement");
        let painted = samples
            .iter()
            .rev()
            .find(|s| s.composer == id && !s.measuring)
            .expect("actual editor prepaint");
        assert_eq!(
            measured.font, painted.font,
            "measured font must match the owned editor font"
        );
        assert_eq!(
            measured.font_size, painted.font_size,
            "measured font size must match the owned editor paint"
        );
        assert_eq!(
            measured.line_height, painted.line_height,
            "measured row height must match the owned editor paint"
        );
        eprintln!(
            "ZOOM {scale}/{width} measured={:?}/{:?}/{} painted={:?}/{:?}/{}",
            measured.width,
            measured.height,
            measured.rows,
            painted.width,
            painted.height,
            painted.rows
        );
        app.read_with(window, |app, cx| {
            let composer = app.composer.read(cx);
            let (field, text, rows) = composer.painted_geometry();
            assert_eq!(text, draft, "zoom preserves the complete owned draft");
            assert!((1..=theme::COMPOSER_MAX_ROWS).contains(&rows));
            for (top, bottom) in composer.painted_vertical_bounds() {
                assert!(
                    top >= field.top() - px(0.1) && bottom <= field.bottom() + px(0.1),
                    "uncapped draft rows must remain completely visible after {scale}/{width}: {top:?}..{bottom:?} inside {field:?}"
                );
            }
        });
    }
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_compact_conversations_blocks_the_navigation_rail(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_geometry_tests::workshop_geometry_compact_conversations_blocks_the_navigation_rail") { return; }
    let (app, window) = setup(cx);
    for mode in [Mode::Work, Mode::Council] {
        for (scale, width) in [(0.9, 780.), (1., 800.), (1.25, 860.)] {
            theme::set_ui_scale(scale);
            window.simulate_resize(size(px(width), px(900.)));
            app.update(window, |app, cx| {
                app.set_mode(mode, cx);
                app.shell.conversations = true;
                cx.notify();
            });
            window.run_until_parked();
            assert_eq!(
                window.debug_bounds("ui-sheet-scrim").unwrap(),
                gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(width), px(900.))),
                "compact Conversations scrim must cover the whole window"
            );
            let design = window.debug_bounds("awili-rail-item-2").unwrap();
            window.simulate_click(design.center(), gpui::Modifiers::default());
            window.run_until_parked();
            assert_eq!(
                app.read_with(window, |app, _| app.mode),
                mode,
                "compact Conversations must block background Design navigation"
            );
            window.simulate_keystrokes("escape");
            window.run_until_parked();
        }
    }
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_forced_sheet_covers_canvas_and_keeps_its_keyboard(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_geometry_tests::workshop_geometry_forced_sheet_covers_canvas_and_keeps_its_keyboard") { return; }
    let (app, window) = setup(cx);
    theme::set_ui_scale(0.9);
    window.simulate_resize(size(px(800.), px(900.)));
    app.update(window, |app, cx| {
        app.shell.conversations = true;
        cx.notify();
    });
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-j"));
    window.run_until_parked();
    assert!(app.read_with(window, |app, _| app.shell.job_open
        && !app.shell.conversations));
    let screen = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(800.), px(900.)));
    for selector in ["awili-work-panel", "ui-sheet-scrim"] {
        assert_eq!(
            window.debug_bounds(selector).unwrap(),
            screen,
            "forced sheet covers the whole canvas"
        );
    }
    for chord in ["tab", "shift-tab"] {
        let mut visited = Vec::new();
        for _ in 0..16 {
            window.simulate_keystrokes(chord);
            window.run_until_parked();
            assert!(
                window.update(|win, cx| app.read(cx).shell.job_focus.contains_focused(win, cx)),
                "Job sheet keyboard must stay inside the modal face"
            );
            let focus = window.update(|win, cx| win.focused(cx).unwrap());
            if !visited.contains(&focus) {
                visited.push(focus);
            }
        }
        assert!(
            visited.len() >= 2,
            "Job sheet Tab must move through its controls"
        );
    }
    let rail = window.debug_bounds("awili-rail-item-0").unwrap();
    window.simulate_click(rail.center(), gpui::Modifiers::default());
    window.run_until_parked();
    assert!(
        !app.read_with(window, |app, _| app.shell.conversations),
        "sheet blocks the Workshop rail behind it"
    );
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!app.read_with(window, |app, _| app.shell.job_open));
    assert!(window.update(|win, cx| app
        .read(cx)
        .composer
        .read(cx)
        .focus_handle()
        .is_focused(win)));
    window.simulate_input("After Job closes");
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        "After Job closes"
    );
    theme::set_ui_scale(1.25);
    app.update(window, |app, cx| {
        app.shell.conversations = true;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("ui-sheet-scrim").is_some());
    let conversations_focus = window.update(|win, cx| win.focused(cx).unwrap());
    for chord in ["tab", "shift-tab"] {
        let mut visited = Vec::new();
        for _ in 0..16 {
            window.simulate_keystrokes(chord);
            window.run_until_parked();
            assert!(
                window.update(|win, cx| conversations_focus.contains_focused(win, cx)),
                "Conversations sheet keyboard must stay inside the modal face"
            );
            let focus = window.update(|win, cx| win.focused(cx).unwrap());
            if !visited.contains(&focus) {
                visited.push(focus);
            }
        }
        assert!(
            visited.len() >= 2,
            "Conversations sheet Tab must move through its controls"
        );
    }
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!app.read_with(window, |app, _| app.shell.conversations));
    assert!(window.update(|win, cx| app
        .read(cx)
        .composer
        .read(cx)
        .focus_handle()
        .is_focused(win)));
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        "After Job closes"
    );
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_all_job_tabs_keep_full_owned_text_and_overlay_allocation(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_geometry_tests::workshop_geometry_all_job_tabs_keep_full_owned_text_and_overlay_allocation") { return; }
    let (app, window) = setup(cx);
    let step = "A complete long progress label including the final step. ".repeat(5);
    let file = "A complete long file name including spaces and its final suffix.txt";
    let url = "https://example.invalid/a/complete/long/preview/path/that/must/remain/readable?tail=RETAINED";
    let note = "A complete recalled Context note with its final sentence. ".repeat(5);
    app.update(window, |app, cx| {
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.work.panel.steps = vec![crate::work::ProgressStep {
            label: step.clone(),
            state: crate::work::StepState::Done,
        }];
        app.work.touched_files = vec![file.into()];
        app.work.referenced_urls = vec![url.into()];
        app.work.memory_open = true;
        app.work.memory_results = Some(crate::work::MemoryInspectorResults::V65 {
            policy: vec![],
            items: vec![
                serde_json::from_value(serde_json::json!({"object":{"content":note}})).unwrap(),
            ],
        });
        app.shell.job_open = true;
        cx.notify();
    });
    for (scale, width) in [(0.9, 800.), (0.9, 998.), (1.25, 1400.)] {
        theme::set_ui_scale(scale);
        window.simulate_resize(size(px(width), px(1100.)));
        for tab in JobTab::ALL {
            crate::ui::text::DRAWN_TEXTS.with(|d| d.borrow_mut().clear());
            app.update(window, |app, cx| {
                app.shell.job_tab = tab;
                app.work.scroll.set_offset(Default::default());
                cx.notify();
            });
            window.run_until_parked();
            let body = window.debug_bounds("awili-job-body").unwrap();
            let panel = window.debug_bounds("ui-panel-awili-job-panel").unwrap();
            assert!(body.left() >= panel.left() && body.right() <= panel.right());
            let rows: Vec<(&str, &str)> = match tab {
                JobTab::History => vec![("ui-row-job-step-0", &step)],
                JobTab::Terminal => vec![(
                    "ui-card-job-terminal-empty",
                    "The terminal appears here when this conversation has a working folder.",
                )],
                JobTab::Previews => vec![
                    ("ui-row-job-preview-file-0", file),
                    ("ui-row-job-preview-link-0", url),
                ],
                JobTab::Context => vec![("ui-row-memory-result-0", &note)],
            };
            for (selector, text) in rows {
                let row = window.debug_bounds(selector).expect(selector);
                assert!(
                    row.left() >= body.left() && row.right() <= body.right(),
                    "{selector} stays inside the bounded Job body"
                );
                let painted = crate::ui::text::DRAWN_TEXTS
                    .with(|d| {
                        d.borrow().iter().find_map(|l| {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                (l.text(), l.bounds())
                            }))
                            .ok()
                            .filter(|(t, b)| t == text && row.contains(&b.center()))
                            .map(|(_, b)| b)
                        })
                    })
                    .unwrap_or_else(|| {
                        panic!("complete owned {text:?} not painted inside {selector}")
                    });
                contains(row, painted);
            }
        }
    }
    theme::set_ui_scale(1.);
    window.simulate_resize(size(px(1400.), px(1100.)));
    app.update(window, |app, cx| {
        app.shell.conversations = false;
        app.voice.speaking = true;
        cx.notify();
    });
    window.run_until_parked();
    let before = app.read_with(window, |app, cx| app.composer.read(cx).painted_geometry().0);
    app.update(window, |app, cx| {
        app.shell.conversations = true;
        cx.notify();
    });
    window.run_until_parked();
    let after = app.read_with(window, |app, cx| app.composer.read(cx).painted_geometry().0);
    assert_eq!(
        before, after,
        "Conversations is an overlay and allocates no extra dock width"
    );
    assert!(after.size.width >= theme::z(theme::JOB_EDITOR_MIN_W));
}

#[gpui::test]
fn workshop_geometry_job_sheet_reserves_a_readable_composer(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_geometry_tests::workshop_geometry_job_sheet_reserves_a_readable_composer",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    for (scale, width, sheet) in [
        (0.9, 800., true),
        (0.9, 994., true),
        (0.9, 998., false),
        (1., 1078., true),
        (1., 1082., false),
        (1.25, 1346., true),
        (1.25, 1400., false),
    ] {
        theme::set_ui_scale(scale);
        window.simulate_resize(size(px(width), px(900.)));
        for speaking in [false, true] {
            app.update(window, |app, cx| {
                app.shell.job_open = true;
                app.voice.speaking = speaking;
                cx.notify();
            });
            window.run_until_parked();
            assert_eq!(
                window.debug_bounds("ui-sheet-scrim").is_some(),
                sheet,
                "{width}/{scale}"
            );
            if sheet {
                window.simulate_keystrokes("escape");
                window.run_until_parked();
                assert!(!app.read_with(window, |app, _| app.shell.job_open));
            }
            window.update(|win, cx| app.update(cx, |app, cx| app.focus_composer(win, cx)));
            window.simulate_input("A readable draft stays here.");
            window.run_until_parked();
            let frame = window
                .debug_bounds("ui-composer-frame-awili-composer")
                .unwrap();
            app.read_with(window, |app, cx| {
                let (field, text, rows) = app.composer.read(cx).painted_geometry();
                assert!(
                    field.size.width >= theme::z(theme::JOB_EDITOR_MIN_W) - px(1.),
                    "editor at {width}/{scale}/{speaking}: {field:?}"
                );
                assert_eq!(text, "A readable draft stays here.");
                assert!(rows <= 3, "draft must remain readable: {rows} rows");
                contains(frame, field);
            });
            for selector in [
                "ui-icon-button-attach",
                "ui-icon-button-mic",
                "ui-icon-button-voice-mode",
                crate::SEND_OR_STOP_SELECTOR,
            ] {
                let b = window.debug_bounds(selector).expect(selector);
                contains(frame, b);
                assert!(b.size.width >= px(44.) && b.size.height >= px(44.));
            }
            if speaking {
                contains(
                    frame,
                    window.debug_bounds("ui-icon-button-stop-speak").unwrap(),
                );
            }
            app.update(window, |app, cx| {
                app.composer.update(cx, |c, cx| c.set_text("", cx))
            });
        }
    }
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_user_text_wraps_inside_its_available_transcript(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_geometry_tests::workshop_geometry_user_text_wraps_inside_its_available_transcript") { return; }
    let (app, window) = setup(cx);
    const LONG: &str = "Beginning of the complete request. Please explain the folder changes and keep the entire message readable even when Job details are open on the right. End of the request.";
    app.update(window, |app, cx| {
        let active = app.views.get_mut(app.sidebar.active_id()).unwrap();
        for (role, text) in [
            (crate::Role::User, LONG),
            (
                crate::Role::Assistant,
                "The next reply stays below the request.",
            ),
            (crate::Role::User, "OK"),
        ] {
            let idx = active.convo.len();
            active.convo.push(role, text);
            active
                .rows
                .push(crate::MessageRow::new(active.convo.message(idx).unwrap()));
        }
        cx.notify();
    });
    for (scale, width, job) in [
        (0.9, 800., false),
        (1.25, 800., false),
        (0.9, 800., true),
        (0.9, 988., true),
        (1., 1092., true),
        (1.25, 1400., true),
        (1., 1400., false),
    ] {
        theme::set_ui_scale(scale);
        window.simulate_resize(size(px(width), px(1100.)));
        app.update(window, |app, cx| {
            app.shell.job_open = job;
            cx.notify();
        });
        crate::selectable_message::geometry_probe::begin();
        window.update(|win, cx| win.draw(cx).clear());
        window.run_until_parked();
        let samples = crate::selectable_message::geometry_probe::take();
        let viewport = window.debug_bounds("awili-transcript-viewport").unwrap();
        let bubble = window.debug_bounds("awili-user-bubble-0").unwrap();
        contains(viewport, bubble);
        let sample = samples
            .iter()
            .rev()
            .find(|s| s.source == LONG)
            .expect("complete request shaped");
        assert!(!sample.text_bounds.is_empty());
        for text in &sample.text_bounds {
            contains(bubble, *text);
        }
        let next = window.debug_bounds("awili-message-row-1").unwrap();
        assert!(
            next.top() >= bubble.bottom(),
            "wrapped request overlaps the next reply"
        );
        let short = window.debug_bounds("awili-user-bubble-2").unwrap();
        contains(viewport, short);
        assert!(
            (short.right() - bubble.right()).abs() < px(1.),
            "short message stays right-aligned"
        );
        assert!(
            short.size.width < bubble.size.width,
            "short message keeps a compact bubble"
        );
    }
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_terminal_actions_fit_the_dock_and_reach_the_owned_child(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_geometry_tests::workshop_geometry_terminal_actions_fit_the_dock_and_reach_the_owned_child") { return; }
    let (app, window) = setup(cx);
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir
        .path()
        .join("a-very-long-working-folder-name-that-must-truncate-without-hiding-terminal-actions");
    std::fs::create_dir(&cwd).unwrap();
    let latch = dir.path().join("input");
    let script = format!(
        "stty raw -echo; printf READY; dd bs=1 count=2 of='{}' 2>/dev/null; stty sane",
        latch.display()
    );
    let terminal = app.update(window, |app, cx| {
        let terminal = crate::terminal_view::TerminalView::try_new_command(
            crate::terminal::TerminalId(83),
            cwd,
            "/bin/sh".into(),
            vec!["-c".into(), script],
            vec![],
            cx,
        )
        .unwrap();
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.work.terminal_view = Some(terminal.clone());
        app.shell.job_open = true;
        app.shell.job_tab = JobTab::Terminal;
        cx.notify();
        terminal
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !terminal.update(window, |v, _| {
        v.terminal.poll();
        v.terminal.screen_text().contains("READY")
    }) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for (scale, width) in [(0.9, 800.), (0.9, 988.), (1., 1400.), (1.25, 1400.)] {
        theme::set_ui_scale(scale);
        window.simulate_resize(size(px(width), px(1100.)));
        crate::terminal_element::font_probe::begin();
        terminal.update(window, |_, cx| cx.notify());
        window.update(|win, cx| win.draw(cx).clear());
        window.run_until_parked();
        let body = window.debug_bounds("awili-job-body").unwrap();
        let panel = window.debug_bounds("ui-panel-awili-job-panel").unwrap();
        assert!(
            body.left() >= panel.left() && body.right() <= panel.right(),
            "Job body stays inside its visible panel"
        );
        let frame = window
            .debug_bounds("ui-terminal-frame-job-terminal")
            .unwrap();
        assert!(
            frame.left() >= body.left() && frame.right() <= body.right(),
            "terminal stays within the dock width"
        );
        contains(
            body,
            window
                .debug_bounds("ui-terminal-frame-job-terminal-header")
                .unwrap(),
        );
        for selector in [
            "ui-icon-button-job-terminal-stop",
            "ui-icon-button-job-terminal-clear",
        ] {
            let action = window.debug_bounds(selector).expect(selector);
            contains(body, action);
            contains(frame, action);
            assert!(action.size.width >= px(44.) && action.size.height >= px(44.));
        }
        let grid = window.debug_bounds("awili-terminal-grid-viewport").unwrap();
        assert!(
            grid.left() >= body.left() && grid.right() <= body.right(),
            "grid fits the dock width; tall content remains scrollable"
        );
        let samples = crate::terminal_element::font_probe::take();
        let sample = samples
            .iter()
            .rev()
            .find(|s| s.bounds.is_some())
            .expect("actual fitted grid");
        let fitted = sample.bounds.unwrap();
        assert!(fitted.left() >= grid.left() && fitted.right() <= grid.right());
        use alacritty_terminal::grid::Dimensions;
        assert_eq!(
            terminal.read_with(window, |v, _| v
                .terminal
                .term
                .lock()
                .unwrap()
                .grid()
                .columns()),
            usize::from(crate::terminal::fit_cols(
                f32::from(fitted.size.width),
                f32::from(sample.cell_width)
            ))
        );
        if scale == 0.9 && width == 800. {
            for chord in ["tab", "shift-tab"] {
                window.update(|win, cx| win.focus(&terminal.read(cx).focus_handle(cx), cx));
                window.simulate_keystrokes(chord);
                window.run_until_parked();
                assert!(window.update(|win, cx| app
                    .read(cx)
                    .shell
                    .job_focus
                    .contains_focused(win, cx)));
                std::thread::sleep(std::time::Duration::from_millis(50));
                assert_eq!(
                    std::fs::metadata(&latch).unwrap().len(),
                    0,
                    "UI Tab navigation must not reach the PTY"
                );
            }
        }
        assert_eq!(std::fs::metadata(&latch).unwrap().len(), 0);
    }
    let clear = window
        .debug_bounds("ui-icon-button-job-terminal-clear")
        .unwrap();
    window.simulate_click(clear.center(), gpui::Modifiers::default());
    window.run_until_parked();
    assert!(terminal.read_with(window, |v, _| v.terminal.screen_text().trim().is_empty()));
    assert_eq!(
        std::fs::metadata(&latch).unwrap().len(),
        0,
        "Clear sends no input"
    );
    window.update(|win, cx| win.focus(&terminal.read(cx).focus_handle(cx), cx));
    window.simulate_keystrokes("x");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::fs::read(&latch).unwrap().is_empty() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read(&latch).unwrap(),
        [b'x'],
        "ordinary terminal input still reaches the child"
    );
    let stop = window
        .debug_bounds("ui-icon-button-job-terminal-stop")
        .unwrap();
    window.simulate_click(stop.center(), gpui::Modifiers::default());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::fs::read(&latch).unwrap().len() < 2 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        std::fs::read(latch).unwrap(),
        [b'x', 3],
        "actual Stop delivered ETX to the owned raw child"
    );
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_native_short_max_zoom_keeps_reply_and_editor(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_geometry_tests::workshop_geometry_native_short_max_zoom_keeps_reply_and_editor",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let draft = "Native G2: typing after Settings Done. Still owned.";
    app.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        app.conversations.get_mut(&owner).unwrap().title =
            "Native Inspector first reply: answer with the owned fixture.".into();
        let view = app.views.get_mut(&owner).unwrap();
        for (role, text) in [
            (crate::Role::User, "Original owned request"),
            (crate::Role::Assistant, "Complete retained native reply."),
        ] {
            let index = view.convo.len();
            view.convo.push(role, text);
            view.rows
                .push(crate::MessageRow::new(view.convo.message(index).unwrap()));
        }
        app.composer
            .update(cx, |composer, cx| composer.set_text(draft, cx));
        cx.notify();
    });
    window.simulate_resize(size(px(900.), px(712.)));
    app.update_in(window, |app, win, cx| app.apply_ui_scale(2.5, win, cx));
    window.run_until_parked();
    let viewport = window.debug_bounds("awili-transcript-viewport").unwrap();
    for selector in [
        "ui-composer-frame-awili-composer",
        "ui-composer-frame-awili-composer-row",
        "ui-composer-frame-awili-composer-footer",
        crate::plan_gate::COMPOSER_ROW_SELECTOR,
    ] {
        eprintln!(
            "max allocation {selector}: {:?}",
            window.debug_bounds(selector)
        );
    }
    assert!(
        viewport.size.height >= theme::target(),
        "Replies retain a useful viewport: {viewport:?}"
    );
    let canvas = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(900.), px(712.)));
    let frame = window
        .debug_bounds("ui-composer-frame-awili-composer")
        .unwrap();
    contains(canvas, frame);
    let editor = window
        .debug_bounds(crate::plan_gate::COMPOSER_ROW_SELECTOR)
        .unwrap();
    assert!(
        editor.size.width >= theme::target() * 2. && editor.size.height >= theme::target(),
        "{editor:?}"
    );
    contains(frame, editor);
    for selector in [
        "ui-icon-button-attach",
        "ui-icon-button-mic",
        "ui-icon-button-voice-mode",
        crate::SEND_OR_STOP_SELECTOR,
    ] {
        let control = window
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("Native control {selector} is rendered"));
        contains(frame, control);
        contains(canvas, control);
    }
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        draft
    );
}

#[gpui::test]
fn workshop_geometry_compact_header_keyboard_reveal(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_geometry_tests::workshop_geometry_compact_header_keyboard_reveal") {
        return;
    }
    let (app, window) = setup(cx);
    let draft = "Header navigation keeps this draft.";
    app.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        let view = app.views.get_mut(&owner).unwrap();
        for _ in 0..12 {
            let index = view.convo.len();
            view.convo.push(
                crate::Role::Assistant,
                "Retained answer with enough rows to scroll.",
            );
            view.rows
                .push(crate::MessageRow::new(view.convo.message(index).unwrap()));
        }
        app.composer.update(cx, |c, cx| c.set_text(draft, cx));
        cx.notify();
    });
    window.simulate_resize(size(px(900.), px(712.)));
    app.update_in(window, |app, win, cx| {
        app.apply_ui_scale(2.5, win, cx);
        app.focus_composer(win, cx);
    });
    window.run_until_parked();
    let scroll = app.read_with(window, |app, _| app.active_view().unwrap().scroll.clone());
    scroll.scroll_to_bottom();
    window.update(|win, _| win.refresh());
    window.run_until_parked();
    assert!(
        scroll.offset().y < px(-200.),
        "actually scrolled away from header"
    );
    let before = scroll.offset();
    app.update(window, |_, cx| cx.notify());
    window.run_until_parked();
    assert_eq!(
        scroll.offset(),
        before,
        "ordinary repaint preserves the reading position"
    );

    for (index, selector) in [
        (1, "ui-chip-awili-workshop-helper"),
        (2, "ui-chip-awili-workshop-project"),
        (10, "ui-button-awili-workshop-history-toggle"),
    ] {
        app.update_in(window, |app, win, cx| app.focus_composer(win, cx));
        scroll.scroll_to_bottom();
        window.update(|win, _| win.refresh());
        window.run_until_parked();
        let focus = app.read_with(window, |app, _| app.workshop_header_focus(index));
        for _ in 0..30 {
            window.simulate_keystrokes("shift-tab");
            window.run_until_parked();
            if window.update(|win, _| focus.is_focused(win)) {
                break;
            }
        }
        assert!(
            window.update(|win, _| focus.is_focused(win)),
            "real Tab traversal reaches {selector}"
        );
        // The model chip now lives in the composer footer (always on screen), not
        // in the scrolling header: it must stay inside the window instead.
        let viewport = if index == 1 {
            gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(900.), px(712.)))
        } else {
            window.debug_bounds("awili-transcript-viewport").unwrap()
        };
        let trigger = window.debug_bounds(selector).unwrap();
        assert!(
            trigger.top() >= viewport.top() - px(0.1)
                && trigger.bottom() <= viewport.bottom() + px(0.1),
            "focused {selector} remains fully visible vertically: {trigger:?} inside {viewport:?}"
        );
        if index == 1 {
            for scale in [1.25, 2.5] {
                app.update_in(window, |app, win, cx| app.apply_ui_scale(scale, win, cx));
                window.run_until_parked();
                let viewport = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(900.), px(712.)));
                let trigger = window.debug_bounds(selector).unwrap();
                assert!(trigger.top() >= viewport.top() - px(0.1) && trigger.bottom() <= viewport.bottom() + px(0.1),
                    "retained focused helper remains visible after {scale}: {trigger:?} inside {viewport:?}");
            }
        }
        if index == 10 {
            // The toggle opens the Conversations sheet, not a menu.
            continue;
        }
        window.simulate_keystrokes("enter");
        window.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        window.run_until_parked();
        window.update(|win, _| win.refresh());
        window.run_until_parked();
        // The helper chip opens the composer's chooser; the folder/history
        // chevron opens the Workshop project menu.
        let menu = window
            .debug_bounds(if index == 1 {
                "ui-popover-composer-model-menu-panel"
            } else {
                "ui-popover-awili-workshop-menu-panel"
            })
            .unwrap();
        contains(
            gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(900.), px(712.))),
            menu,
        );
        window.simulate_keystrokes("escape");
        window.run_until_parked();
    }
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        draft
    );
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_shared_header_all_modes_and_zoom(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_geometry_tests::workshop_geometry_shared_header_all_modes_and_zoom") {
        return;
    }
    let (app, window) = setup(cx);
    let draft = "Native G2: typing after Settings Done. Still owned.";
    for mode in [Mode::Work, Mode::Code] {
        app.update(window, |app, cx| {
            app.set_mode(mode, cx);
            if mode == Mode::Code {
                if app.work.code_tabs.is_empty() {
                    app.new_code_tab(awili_code::CodeEngine::Claude);
                }
                let session = &mut app.work.code_tabs[app.work.code_active];
                session
                    .convo
                    .push(crate::Role::Assistant, "Complete retained Code reply.");
                let index = session.convo.len() - 1;
                session.rows.push(crate::MessageRow::new(
                    session.convo.message(index).unwrap(),
                ));
            } else {
                let owner = app.sidebar.active_id().to_string();
                let view = app.views.get_mut(&owner).unwrap();
                view.convo
                    .push(crate::Role::Assistant, "Complete retained Workshop reply.");
                let index = view.convo.len() - 1;
                view.rows
                    .push(crate::MessageRow::new(view.convo.message(index).unwrap()));
            }
            app.composer.update(cx, |c, cx| c.set_text(draft, cx));
            cx.notify();
        });
        for scale in [0.9, 1., 1.25, 2.5] {
            window.simulate_resize(size(px(900.), px(712.)));
            app.update_in(window, |app, win, cx| app.apply_ui_scale(scale, win, cx));
            window.run_until_parked();
            let viewport = window
                .debug_bounds(if mode == Mode::Code {
                    "awili-code-transcript-viewport"
                } else {
                    "awili-transcript-viewport"
                })
                .unwrap();
            assert!(
                viewport.size.height >= theme::target(),
                "{mode:?}/{scale}: {viewport:?}"
            );
            assert!(
                window.debug_bounds("awili-workshop-header").is_some(),
                "the shared header remains rendered"
            );
            let canvas = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(900.), px(712.)));
            let frame = window
                .debug_bounds("ui-composer-frame-awili-composer")
                .unwrap();
            contains(canvas, frame);
            let editor = window
                .debug_bounds(crate::plan_gate::COMPOSER_ROW_SELECTOR)
                .unwrap();
            assert!(
                editor.size.height >= theme::target() && editor.size.width >= theme::target() * 2.,
                "{mode:?}/{scale}: {editor:?}"
            );
            contains(frame, editor);
            for selector in [
                "ui-icon-button-attach",
                "ui-icon-button-mic",
                "ui-icon-button-voice-mode",
                crate::SEND_OR_STOP_SELECTOR,
            ] {
                let bounds = window.debug_bounds(selector).unwrap();
                contains(frame, bounds);
                assert!(
                    bounds.size.width >= theme::target() && bounds.size.height >= theme::target(),
                    "{mode:?}/{scale}: {selector} {bounds:?}"
                );
            }
            // The model, mode and run state live in the top bar now.
            for gone in ["ui-chip-model-chip", "ui-chip-composer-status"] {
                assert!(window.debug_bounds(gone).is_none(), "{gone} left the composer");
            }
            // The mode chip is in the bar, which scrolls with the transcript here.
            assert!(window
                .debug_bounds(crate::ask_mode_ui::ASK_MODE_CHIP_SELECTOR)
                .is_some());
            assert_eq!(
                app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
                draft
            );
        }
    }
    theme::set_ui_scale(1.);
}

fn header_action_seed(
    app: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
    mode: Mode,
    notice: usize,
) {
    app.update(window, |app, cx| {
        app.set_mode(mode, cx);
        app.workshop_header_notice_fixture(notice);
        if mode == Mode::Code {
            if app.work.code_tabs.is_empty() {
                app.new_code_tab(awili_code::CodeEngine::Claude);
            }
            let session = &mut app.work.code_tabs[app.work.code_active];
            for _ in 0..12 {
                let index = session.convo.len();
                session
                    .convo
                    .push(crate::Role::Assistant, "Retained owned Code answer.");
                session.rows.push(crate::MessageRow::new(
                    session.convo.message(index).unwrap(),
                ));
            }
        } else {
            let owner = app.sidebar.active_id().to_string();
            let view = app.views.get_mut(&owner).unwrap();
            for _ in 0..12 {
                let index = view.convo.len();
                view.convo
                    .push(crate::Role::Assistant, "Retained owned answer.");
                view.rows
                    .push(crate::MessageRow::new(view.convo.message(index).unwrap()));
            }
        }
        app.composer.update(cx, |c, cx| {
            c.set_text("Header controls keep this owned draft.", cx)
        });
        cx.notify();
    });
    window.simulate_resize(size(px(900.), px(712.)));
    app.update_in(window, |app, win, cx| {
        app.apply_ui_scale(2.5, win, cx);
        app.focus_composer(win, cx);
    });
    window.run_until_parked();
}

fn traverse_header_action(
    app: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
    mode: Mode,
    index: usize,
    selector: &'static str,
) {
    let (scroll, rows) = app.read_with(window, |app, _| {
        if mode == Mode::Code {
            let session = &app.work.code_tabs[app.work.code_active];
            (session.scroll.clone(), session.rows.len())
        } else {
            let view = app.active_view().unwrap();
            (view.scroll.clone(), view.rows.len())
        }
    });
    app.update_in(window, |app, win, cx| app.focus_composer(win, cx));
    scroll.scroll_to_bottom();
    window.update(|win, _| win.refresh());
    window.run_until_parked();
    assert!(scroll.offset().y < px(-200.), "actually scrolled away");
    let before = scroll.offset();
    app.update(window, |_, cx| cx.notify());
    window.run_until_parked();
    assert_eq!(
        scroll.offset(),
        before,
        "unrelated paint keeps reading position"
    );
    let focus = app.read_with(window, |app, _| app.workshop_header_focus(index));
    assert!(!window.update(|win, _| focus.is_focused(win)));
    let initial_focus = window
        .update(|win, cx| win.focused(cx))
        .expect("composer focus before complete Tab traversal");
    let mut visited = vec![initial_focus];
    let mut reached = false;
    for _ in 0..4096 {
        window.simulate_keystrokes("shift-tab");
        window.run_until_parked();
        let current = window.update(|win, cx| win.focused(cx));
        if current.as_ref() == Some(&focus) {
            reached = true;
            break;
        }
        let current = current.expect("actual focus remains present during Tab traversal");
        assert!(
            !visited.contains(&current),
            "complete Shift-Tab focus cycle ended before reaching {selector}"
        );
        visited.push(current);
    }
    assert!(
        reached,
        "bounded complete traversal reaches {selector} before its focus cycle repeats"
    );
    let viewport = window
        .debug_bounds(if mode == Mode::Code {
            "awili-code-transcript-viewport"
        } else {
            "awili-transcript-viewport"
        })
        .unwrap();
    let target = window.debug_bounds(selector).expect(selector);
    assert!(
        target.top() >= viewport.top() - px(0.1) && target.bottom() <= viewport.bottom() + px(0.1),
        "focused {selector} must be visible: {target:?} inside {viewport:?}"
    );
    app.read_with(window, |app, cx| {
        assert_eq!(
            app.composer.read(cx).text().to_string(),
            "Header controls keep this owned draft."
        );
        let after = if mode == Mode::Code {
            app.work.code_tabs[app.work.code_active].rows.len()
        } else {
            app.active_view().unwrap().rows.len()
        };
        assert_eq!(rows, after, "navigation does not rewrite transcript");
    });
}

#[gpui::test]
fn workshop_geometry_code_history_keyboard_reveal(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_geometry_tests::workshop_geometry_code_history_keyboard_reveal") {
        return;
    }
    let (app, window) = setup(cx);
    // The title is the conversation switcher in both modes.
    {
        let mode = Mode::Code;
        header_action_seed(&app, window, mode, 0);
        traverse_header_action(
            &app,
            window,
            mode,
            3,
            "ui-chip-awili-workshop-title",
        );
        window.simulate_keystrokes("enter");
        window.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        window.run_until_parked();
        assert!(
            app.read_with(window, |app, _| app.shell.tabs.menu
                == Some(crate::workshop::WorkshopMenu::Switcher)),
            "the title opens the conversation switcher"
        );
        window.simulate_keystrokes("escape");
        window.run_until_parked();
    }
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_notice_actions_keyboard_reveal(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_geometry_tests::workshop_geometry_notice_actions_keyboard_reveal") {
        return;
    }
    for (index, selector) in [
        (4, "ui-button-awili-workshop-dismiss-project-import"),
        (5, "ui-button-awili-workshop-dismiss-restore"),
        (6, "ui-button-awili-workshop-recover"),
        (7, "ui-button-awili-workshop-retry-save"),
    ] {
        // Keep each case's traversal bounded by one seeded transcript. Reusing
        // the app here appended another twelve rows on every iteration.
        let (app, window) = setup(cx);
        header_action_seed(&app, window, Mode::Work, index);
        traverse_header_action(&app, window, Mode::Work, index, selector);
        let (tx, rx) = std::sync::mpsc::channel();
        if index == 6 {
            app.update(window, |app, _| {
                app.persist = Some(crate::Persistence {
                    tx,
                    failed: Default::default(),
            recovery: Default::default(),
                })
            });
        }
        window.simulate_keystrokes("enter");
        window.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        window.run_until_parked();
        if index == 6 {
            assert!(
                app.read_with(window, |app, _| app.shell.tabs.unreadable.is_some()),
                "cause stays until successful recovery ack"
            );
            let reply = rx
                .try_iter()
                .find_map(|cmd| match cmd {
                    crate::PersistCmd::SaveWorkshopTabs {
                        recovery: Some(_),
                        reply,
                        ..
                    } => Some(reply),
                    _ => None,
                })
                .expect("actual Enter queued owned recovery");
            reply.try_send(Ok(())).unwrap();
            window.run_until_parked();
            app.update(window, |app, _| app.persist = None);
        }
        assert!(
            window.debug_bounds(selector).is_none(),
            "actual {selector} activation resolves its notice"
        );
        window.simulate_input(" Immediate typing.");
        assert_eq!(
            app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
            "Header controls keep this owned draft. Immediate typing."
        );
        window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-j"));
        window.run_until_parked();
        assert!(
            app.read_with(window, |app, _| app.shell.job_open),
            "immediate shortcut after notice removal"
        );
        window.simulate_keystrokes("escape");
        window.run_until_parked();
    }
    theme::set_ui_scale(1.);
}

#[gpui::test]
fn workshop_geometry_history_toggle_keyboard_reveal_and_sheet(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_geometry_tests::workshop_geometry_history_toggle_keyboard_reveal_and_sheet",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    header_action_seed(&app, window, Mode::Work, 0);
    traverse_header_action(
        &app,
        window,
        Mode::Work,
        10,
        "ui-button-awili-workshop-history-toggle",
    );
    // Compact: the panel cannot dock, so the toggle opens it as a sheet over
    // the same thread, and the owned draft is untouched.
    let owner = app.read_with(window, |app, _| app.sidebar.active_id().to_string());
    window.simulate_keystrokes("enter");
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();
    assert!(app.read_with(window, |app, _| app.shell.conversations));
    assert!(window
        .debug_bounds("ui-panel-awili-conversations-panel")
        .is_some());
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    app.read_with(window, |app, cx| {
        assert!(!app.shell.conversations);
        assert_eq!(app.sidebar.active_id(), owner);
        assert_eq!(
            app.composer.read(cx).text(),
            "Header controls keep this owned draft."
        );
    });
    theme::set_ui_scale(1.);
}

const HISTORY_PANEL: &str = "ui-panel-awili-conversations-panel";
const HISTORY_TOGGLE: &str = "ui-button-awili-workshop-history-toggle";

fn composer_controls_fit(window: &mut gpui::VisualTestContext, canvas: gpui::Bounds<gpui::Pixels>) {
    let frame = window
        .debug_bounds("ui-composer-frame-awili-composer")
        .unwrap();
    contains(canvas, frame);
    let editor = window
        .debug_bounds(crate::plan_gate::COMPOSER_ROW_SELECTOR)
        .unwrap();
    assert!(
        editor.size.width >= theme::target() * 2. && editor.size.height >= theme::target(),
        "usable editor: {editor:?}"
    );
    contains(frame, editor);
    for selector in [
        "ui-icon-button-attach",
        "ui-icon-button-mic",
        "ui-icon-button-voice-mode",
        crate::SEND_OR_STOP_SELECTOR,
    ] {
        let control = window
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is rendered"));
        contains(frame, control);
        assert!(
            control.size.width >= theme::target() && control.size.height >= theme::target(),
            "{selector} keeps its hit target: {control:?}"
        );
    }
}

/// The old horizontal strip is gone: the Conversations panel docks on the
/// right of the transcript and composer, under the bar, and lists the open thread.
#[gpui::test]
fn workshop_conversations_dock_right_and_replace_the_top_strip(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_geometry_tests::workshop_conversations_dock_right_and_replace_the_top_strip",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.);
    window.simulate_resize(size(px(1400.), px(800.)));
    window.run_until_parked();

    assert!(window.debug_bounds("ui-tabs-awili-workshop-tabs").is_none());
    let panel = window.debug_bounds(HISTORY_PANEL).expect("docked panel");
    let header = window.debug_bounds("awili-workshop-header").unwrap();
    let frame = window
        .debug_bounds("ui-composer-frame-awili-composer")
        .unwrap();
    let canvas = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(1400.), px(800.)));
    contains(canvas, panel);
    assert!(
        panel.top() >= header.bottom() - px(0.1) && panel.left() >= frame.right() - px(0.1),
        "the panel sits under the bar and right of the composer: {panel:?} / {header:?} / {frame:?}"
    );
    assert!(panel.left() > px(700.), "right half of the window");
    let active = app.read_with(window, |app, _| app.shell.tabs.active.clone().unwrap());
    let row = window
        .debug_bounds(Box::leak(
            format!("ui-row-workshop-{active}").into_boxed_str(),
        ))
        .expect("the open thread is listed");
    contains(panel, row);
    composer_controls_fit(window, canvas);
}

/// Collapsing and reopening is presentation only: the thread, tab identities,
/// unsent draft and a running turn are untouched, the choice is saved, and a
/// saved collapse is restored on the next launch.
#[gpui::test]
fn workshop_conversations_collapse_and_reopen_without_touching_thread_or_run(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_geometry_tests::workshop_conversations_collapse_and_reopen_without_touching_thread_or_run",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.);
    window.simulate_resize(size(px(1400.), px(800.)));
    let (tx, rx) = std::sync::mpsc::channel();
    app.update(window, |app, _| {
        app.persist = Some(crate::Persistence {
            tx,
            failed: Default::default(),
            recovery: Default::default(),
        })
    });
    let (abort_tx, abort_rx) = std::sync::mpsc::channel();
    let (owner, tab_ids) = app.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        app.work.conv_id = Some(owner.clone());
        app.work.running = true;
        app.work.agent_abort_tx = Some(abort_tx);
        app.composer
            .update(cx, |c, cx| c.set_text("Keep this draft.", cx));
        cx.notify();
        (
            owner,
            app.shell
                .tabs
                .tabs
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>(),
        )
    });
    window.run_until_parked();
    let canvas = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(1400.), px(800.)));
    let docked_frame = window
        .debug_bounds("ui-composer-frame-awili-composer")
        .unwrap();
    assert!(window.debug_bounds(HISTORY_PANEL).is_some());

    // Collapse.
    let toggle = window.debug_bounds(HISTORY_TOGGLE).unwrap().center();
    window.simulate_click(toggle, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds(HISTORY_PANEL).is_none(), "collapsed");
    assert!(
        window.debug_bounds(HISTORY_TOGGLE).is_some(),
        "the reopen control stays in the header"
    );
    let wide_frame = window
        .debug_bounds("ui-composer-frame-awili-composer")
        .unwrap();
    assert!(
        wide_frame.right() > docked_frame.right(),
        "the composer reclaims the panel's room: {wide_frame:?} vs {docked_frame:?}"
    );
    composer_controls_fit(window, canvas);
    app.read_with(window, |app, cx| {
        assert_eq!(app.sidebar.active_id(), owner);
        assert_eq!(app.composer.read(cx).text(), "Keep this draft.");
        assert!(app.work.running, "the run keeps going");
        assert_eq!(app.work.conv_id.as_deref(), Some(owner.as_str()));
        assert!(app.shell.tabs.history_collapsed);
    });
    assert_eq!(
        abort_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "collapse never stops the run"
    );
    let saved = rx.try_iter().find_map(|cmd| match cmd {
        crate::PersistCmd::SetState { key, value } if key == crate::workshop::HISTORY_PANEL_KEY => {
            Some(value)
        }
        _ => None,
    });
    assert_eq!(saved.as_deref(), Some("collapsed"));

    // Reopen.
    let toggle = window.debug_bounds(HISTORY_TOGGLE).unwrap().center();
    window.simulate_click(toggle, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds(HISTORY_PANEL).is_some(), "reopened");
    app.read_with(window, |app, cx| {
        assert_eq!(app.sidebar.active_id(), owner);
        assert_eq!(app.composer.read(cx).text(), "Keep this draft.");
        assert!(app.work.running);
        let ids: Vec<_> = app.shell.tabs.tabs.iter().map(|t| t.id.clone()).collect();
        assert_eq!(ids, tab_ids, "stable tab identities");
        assert!(ids.iter().all(|id| uuid::Uuid::parse_str(id).is_ok()));
    });
    assert_eq!(
        abort_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    );
    app.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn workshop_conversations_saved_collapse_is_restored_on_launch(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_geometry_tests::workshop_conversations_saved_collapse_is_restored_on_launch",
    ) {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    cx.executor().allow_parking();
    let (tx, rx) = std::sync::mpsc::channel();
    let (app, window) = cx.add_window_view(|_, cx| {
        let mut app = AwiliApp::new(cx);
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = Some(crate::Persistence {
            tx,
            failed: Default::default(),
            recovery: Default::default(),
        });
        app.fleet_pack = None;
        app.memory.enabled = false;
        app
    });
    theme::set_ui_scale(1.);
    app.update(window, |app, cx| {
        app.set_mode(Mode::Work, cx);
        cx.notify();
    });
    window.simulate_resize(size(px(1400.), px(800.)));
    window.run_until_parked();
    let reply = rx
        .try_iter()
        .find_map(|cmd| match cmd {
            crate::PersistCmd::ReadState { key, reply }
                if key == crate::workshop::HISTORY_PANEL_KEY =>
            {
                Some(reply)
            }
            _ => None,
        })
        .expect("the panel asks for its saved choice once");
    reply.try_send(Some("collapsed".to_string())).unwrap();
    window.run_until_parked();
    app.update(window, |app, _| app.persist = None);
    assert!(app.read_with(window, |app, _| app.shell.tabs.history_collapsed));
    assert!(window.debug_bounds(HISTORY_PANEL).is_none());
    assert!(window.debug_bounds(HISTORY_TOGGLE).is_some());
}

/// Selecting rows, and closing one, keep each thread's unsent draft and the
/// stable conversation and tab identities.
#[gpui::test]
fn workshop_conversations_rows_select_and_close_keeping_drafts_and_identities(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_geometry_tests::workshop_conversations_rows_select_and_close_keeping_drafts_and_identities",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.);
    window.simulate_resize(size(px(1400.), px(800.)));
    let (a, b, c) = app.update(window, |app, cx| {
        let mut ids = Vec::new();
        for _ in 0..3 {
            let id = app.create_conversation(awili_core::ConvKind::Work);
            app.open_conversation_from_history(&id, cx);
            ids.push(id);
        }
        (ids[0].clone(), ids[1].clone(), ids[2].clone())
    });
    window.run_until_parked();
    let tab_of = |app: &AwiliApp, conv: &str| {
        app.shell
            .tabs
            .tabs
            .iter()
            .find(
                |t| matches!(&t.backing, crate::workshop::Backing::Conversation(id) if id == conv),
            )
            .map(|t| t.id.clone())
            .unwrap()
    };
    let (tab_a, tab_b, tab_c) = app.read_with(window, |app, _| {
        (tab_of(app, &a), tab_of(app, &b), tab_of(app, &c))
    });
    let row = |tab: &str| -> &'static str {
        Box::leak(format!("ui-row-workshop-{tab}").into_boxed_str())
    };
    let click = |window: &mut gpui::VisualTestContext, selector: &'static str| {
        let center = window.debug_bounds(selector).unwrap().center();
        window.simulate_click(center, gpui::Modifiers::default());
        window.run_until_parked();
    };
    let text = |app: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext| {
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string())
    };

    click(window, row(&tab_a));
    window.simulate_input("Draft in A");
    click(window, row(&tab_b));
    assert_eq!(text(&app, window), "", "B starts with an empty box");
    window.simulate_input("Draft in B");
    click(window, row(&tab_a));
    assert_eq!(text(&app, window), "Draft in A", "A's draft returned");
    app.read_with(window, |app, _| assert_eq!(app.sidebar.active_id(), a));
    click(window, row(&tab_b));
    assert_eq!(text(&app, window), "Draft in B");

    // Close the idle third thread with its own control.
    let before = app.read_with(window, |app, _| {
        app.shell
            .tabs
            .tabs
            .iter()
            .map(|t| t.id.clone())
            .collect::<Vec<_>>()
    });
    click(
        window,
        Box::leak(format!("ui-icon-button-workshop-{tab_c}-close").into_boxed_str()),
    );
    app.read_with(window, |app, _| {
        let after: Vec<_> = app.shell.tabs.tabs.iter().map(|t| t.id.clone()).collect();
        let expected: Vec<_> = before.iter().filter(|id| **id != tab_c).cloned().collect();
        assert_eq!(
            after, expected,
            "only the closed tab left; the others keep their ids"
        );
        assert!(
            app.conversations.get(&c).is_some(),
            "closing never deletes the conversation"
        );
    });
    click(window, row(&tab_a));
    assert_eq!(text(&app, window), "Draft in A");
    click(window, row(&tab_b));
    assert_eq!(text(&app, window), "Draft in B");
}

/// Where the panel cannot dock it is a sheet on demand, never a squeeze: the
/// composer keeps its controls at a compact width, and a docked Job takes the
/// right edge while the panel yields to the header toggle.
#[gpui::test]
fn workshop_conversations_yield_to_compact_widths_and_job_without_squeezing_composer(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_geometry_tests::workshop_conversations_yield_to_compact_widths_and_job_without_squeezing_composer",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.);

    // Compact: no dock; the toggle opens a sheet over the same thread.
    window.simulate_resize(size(px(700.), px(700.)));
    app.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("Compact draft.", cx));
        cx.notify();
    });
    window.run_until_parked();
    let canvas = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(700.), px(700.)));
    assert!(window.debug_bounds("awili-history-dock").is_none());
    composer_controls_fit(window, canvas);
    let owner = app.read_with(window, |app, _| app.sidebar.active_id().to_string());
    let toggle = window.debug_bounds(HISTORY_TOGGLE).unwrap().center();
    window.simulate_click(toggle, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(window.debug_bounds(HISTORY_PANEL).is_some(), "sheet opened");
    let active = app.read_with(window, |app, _| app.shell.tabs.active.clone().unwrap());
    let row = window
        .debug_bounds(Box::leak(
            format!("ui-row-workshop-{active}").into_boxed_str(),
        ))
        .unwrap();
    window.simulate_click(row.center(), gpui::Modifiers::default());
    window.run_until_parked();
    app.read_with(window, |app, cx| {
        assert!(
            !app.shell.conversations,
            "choosing a thread closes the sheet"
        );
        assert_eq!(app.sidebar.active_id(), owner);
        assert_eq!(app.composer.read(cx).text(), "Compact draft.");
    });
    composer_controls_fit(window, canvas);

    // Wide enough for the panel alone, not for the panel beside a Job dock.
    window.simulate_resize(size(px(1200.), px(800.)));
    window.run_until_parked();
    assert!(window.debug_bounds("awili-history-dock").is_some());
    app.update(window, |app, cx| {
        app.shell.job_open = true;
        cx.notify();
    });
    window.run_until_parked();
    let canvas = gpui::Bounds::new(gpui::point(px(0.), px(0.)), size(px(1200.), px(800.)));
    let job = window.debug_bounds("awili-work-panel").expect("Job docks");
    assert!(
        window.debug_bounds("awili-history-dock").is_none(),
        "the panel yields instead of overlapping Job"
    );
    let frame = window
        .debug_bounds("ui-composer-frame-awili-composer")
        .unwrap();
    assert!(frame.right() <= job.left() + px(0.1), "{frame:?} / {job:?}");
    composer_controls_fit(window, canvas);
    app.update(window, |app, cx| {
        app.shell.job_open = false;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-history-dock").is_some());
}

/// The chooser lists configured targets in the one conversation; picking one,
/// even while a turn runs, changes only the next turn: nothing is sent and the
/// running turn keeps its identity.
#[gpui::test]
#[cfg(unix)]
fn workshop_chooser_selects_next_target_without_touching_the_running_turn(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_geometry_tests::workshop_chooser_selects_next_target_without_touching_the_running_turn",
    ) {
        return;
    }
    use awili_code::CodeEngine;
    use std::os::unix::fs::PermissionsExt;
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.);
    window.simulate_resize(size(px(1200.), px(800.)));
    let dir = tempfile::tempdir().unwrap();
    let cli = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path.display().to_string()
    };
    let codex = work_executor(CodeEngine::Codex);
    let (abort_tx, abort_rx) = std::sync::mpsc::channel();
    let (owner, rows_before) = app.update(window, |app, cx| {
        app.work.agents.claude_bin = Some(cli("claude"));
        app.work.agents.codex_bin = Some(cli("codex"));
        app.work.agents.dsh_bin = Some(dir.path().join("absent-dsh").display().to_string());
        let owner = app.sidebar.active_id().to_string();
        // A turn is running under the in-app model.
        app.work.conv_id = Some(owner.clone());
        app.work.running = true;
        app.work.agent_abort_tx = Some(abort_tx);
        app.work.panel.requested_identity =
            Some(crate::work_run_identity::WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None });
        app.composer.update(cx, |c, cx| c.set_text("Unsent.", cx));
        cx.notify();
        let rows = app.views[&owner].convo.messages().len();
        (owner, rows)
    });
    window.run_until_parked();

    let chip = window
        .debug_bounds("ui-chip-awili-workshop-helper")
        .unwrap()
        .center();
    window.simulate_click(chip, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("ui-popover-composer-model-menu-panel")
            .is_some(),
        "the chooser opens while a turn runs"
    );
    // Rows are numbered in the order of the sections the chooser is built from.
    let rows: Vec<_> = app.read_with(window, |app, _| {
        crate::executor_labels::target_sections(&crate::executor_labels::TargetReach {
            engine: &app.work.engine,
            extras: &[],
            models: &app.work.models_cfg,
            agents: &app.work.agents,
            build_targets: &app.work.build_targets,
            gateway_set_up: app.work.hermes.enabled || app.work.gateway.enabled,
        })
        .into_iter()
        .flat_map(|s| s.rows)
        .collect()
    });
    for index in 0..rows.len() {
        let selector =
            Box::leak(format!("ui-row-composer-model-menu-item-{index}").into_boxed_str());
        assert!(
            window.debug_bounds(selector).is_some(),
            "row {index} is offered"
        );
    }
    let position = |target: &crate::work::WorkExecutor| {
        rows.iter().position(|row| &row.target == target).unwrap()
    };
    let dsh = work_executor(CodeEngine::Dsh);
    assert!(
        rows[position(&dsh)].unavailable.is_some(),
        "missing DeepSeek names its setup"
    );

    // An unavailable target selects nothing.
    let before = app.read_with(window, |app, _| {
        app.composer_target_for(&crate::workshop_composer::ModelOwner::Work(owner.clone()))
    });
    let unavailable =
        Box::leak(format!("ui-row-composer-model-menu-item-{}", position(&dsh)).into_boxed_str());
    let center = window.debug_bounds(unavailable).unwrap().center();
    window.simulate_click(center, gpui::Modifiers::default());
    window.run_until_parked();
    assert_eq!(
        app.read_with(window, |app, _| app.composer_target_for(
            &crate::workshop_composer::ModelOwner::Work(owner.clone())
        )),
        before
    );

    // A configured harness becomes the next target.
    let selector =
        Box::leak(format!("ui-row-composer-model-menu-item-{}", position(&codex)).into_boxed_str());
    let center = window.debug_bounds(selector).unwrap().center();
    window.simulate_click(center, gpui::Modifiers::default());
    window.run_until_parked();
    app.read_with(window, |app, cx| {
        assert_eq!(
            app.composer_target_for(&crate::workshop_composer::ModelOwner::Work(owner.clone())),
            Some(codex.clone())
        );
        assert!(app.work.running, "the turn keeps running");
        assert_eq!(app.work.conv_id.as_deref(), Some(owner.as_str()));
        assert!(
            matches!(
                app.work.panel.requested_identity,
                Some(crate::work_run_identity::WorkRunIdentity::OwnedEngine { .. })
            ),
            "the running turn keeps the identity it started with"
        );
        assert_eq!(
            app.views[&owner].convo.messages().len(),
            rows_before,
            "selecting sends nothing"
        );
        assert_eq!(app.composer.read(cx).text(), "Unsent.");
        assert_eq!(app.sidebar.active_id(), owner, "same conversation");
    });
    assert_eq!(
        abort_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "selecting never stops the turn"
    );
    app.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
        app.work.panel.requested_identity = None;
    });
}

fn work_executor(engine: awili_code::CodeEngine) -> crate::work::WorkExecutor {
    crate::work::WorkExecutor::Agent {
        engine,
        model: None,
    }
}

/// The header's helper chip and the composer's model chip are one chooser for
/// one owner: choosing through either sets that conversation's next target,
/// sends nothing, opens no new thread, and Escape returns focus to its chip.
#[gpui::test]
#[cfg(unix)]
fn workshop_header_and_composer_choosers_share_one_owner_and_next_target(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_geometry_tests::workshop_header_and_composer_choosers_share_one_owner_and_next_target",
    ) {
        return;
    }
    use crate::workshop_composer::ModelOwner;
    use awili_code::CodeEngine;
    use std::os::unix::fs::PermissionsExt;
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.);
    window.simulate_resize(size(px(1200.), px(800.)));
    let dir = tempfile::tempdir().unwrap();
    let cli = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path.display().to_string()
    };
    let (owner, tabs_before, rows_before) = app.update(window, |app, cx| {
        app.work.agents.claude_bin = Some(cli("claude"));
        app.work.agents.codex_bin = Some(cli("codex"));
        app.composer.update(cx, |c, cx| c.set_text("Unsent.", cx));
        cx.notify();
        let owner = app.sidebar.active_id().to_string();
        let tabs: Vec<_> = app.shell.tabs.tabs.iter().map(|t| t.id.clone()).collect();
        let rows = app.views[&owner].convo.messages().len();
        (owner, tabs, rows)
    });
    window.run_until_parked();
    let owner_key = ModelOwner::Work(owner.clone());
    let targets: Vec<_> = app.read_with(window, |app, _| {
        crate::executor_labels::target_sections(&crate::executor_labels::TargetReach {
            engine: &app.work.engine,
            extras: &[],
            models: &app.work.models_cfg,
            agents: &app.work.agents,
            build_targets: &app.work.build_targets,
            gateway_set_up: app.work.hermes.enabled || app.work.gateway.enabled,
        })
        .into_iter()
        .flat_map(|s| s.rows)
        .collect()
    });
    let row_for = |target: &crate::work::WorkExecutor| -> &'static str {
        let index = targets.iter().position(|r| &r.target == target).unwrap();
        Box::leak(format!("ui-row-composer-model-menu-item-{index}").into_boxed_str())
    };
    let pick = |window: &mut gpui::VisualTestContext,
                chip: &'static str,
                target: crate::work::WorkExecutor| {
        let center = window.debug_bounds(chip).unwrap().center();
        window.simulate_click(center, gpui::Modifiers::default());
        window.run_until_parked();
        app.read_with(window, |app, _| {
            assert_eq!(app.shell.composer_menus.model, Some(owner_key.clone()));
            assert!(app.shell.tabs.menu.is_none(), "not a New-thread menu");
        });
        assert!(window
            .debug_bounds("ui-popover-composer-model-menu-panel")
            .is_some());
        let center = window.debug_bounds(row_for(&target)).unwrap().center();
        window.simulate_click(center, gpui::Modifiers::default());
        window.run_until_parked();
        app.read_with(window, |app, cx| {
            assert_eq!(app.composer_target_for(&owner_key), Some(target.clone()));
            assert_eq!(app.sidebar.active_id(), owner, "same conversation");
            assert_eq!(
                app.shell
                    .tabs
                    .tabs
                    .iter()
                    .map(|t| t.id.clone())
                    .collect::<Vec<_>>(),
                tabs_before,
                "no thread was created"
            );
            assert_eq!(app.views[&owner].convo.messages().len(), rows_before);
            assert_eq!(app.composer.read(cx).text(), "Unsent.");
            assert_eq!(app.mode, Mode::Work);
        });
    };
    pick(
        window,
        "ui-chip-awili-workshop-helper",
        work_executor(CodeEngine::Codex),
    );
    // The one chooser is the composer footer's chip (the bar no longer carries it).
    assert!(window.debug_bounds("ui-chip-model-chip").is_none());
    // Escape from the header's chooser lands back on the header chip.
    let header = window
        .debug_bounds("ui-chip-awili-workshop-helper")
        .unwrap()
        .center();
    window.simulate_click(header, gpui::Modifiers::default());
    window.run_until_parked();
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    window.update(|win, cx| {
        app.read_with(cx, |app, _| {
            assert!(app.shell.composer_menus.model.is_none());
            assert!(app.shell.tabs.engine_focus.is_focused(win));
        })
    });
}

fn resize(app: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext, width: f32) {
    window.simulate_resize(size(px(width), px(800.)));
    window.run_until_parked();
    // The width the click handlers use is the one the frame rendered with.
    assert_eq!(
        app.read_with(window, |app, _| app.window_w),
        width,
        "window_w follows the rendered viewport"
    );
}

fn click_selector(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let center = window.debug_bounds(selector).unwrap().center();
    window.simulate_click(center, gpui::Modifiers::default());
    window.run_until_parked();
}

/// One collapse intent serves the docked panel and the sheet: Show and Hide
/// carry across the breakpoint, a sheet closed by picking a thread does not
/// reopen the panel after a later resize, and widening with a sheet open
/// keeps the panel open.
#[gpui::test]
fn workshop_conversations_collapse_intent_survives_dock_sheet_and_resize(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_geometry_tests::workshop_conversations_collapse_intent_survives_dock_sheet_and_resize",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.);
    let collapsed = |app: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext| {
        app.read_with(window, |app, _| app.shell.tabs.history_collapsed)
    };
    let sheet = |app: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext| {
        app.read_with(window, |app, _| app.shell.conversations)
    };

    // Hide while docked.
    resize(&app, window, 1400.);
    assert!(window.debug_bounds(HISTORY_PANEL).is_some());
    click_selector(window, HISTORY_TOGGLE);
    assert!(collapsed(&app, window) && window.debug_bounds(HISTORY_PANEL).is_none());

    // Narrow: still hidden. Show opens the sheet and records the intent.
    resize(&app, window, 700.);
    assert!(!sheet(&app, window) && window.debug_bounds(HISTORY_PANEL).is_none());
    click_selector(window, HISTORY_TOGGLE);
    assert!(sheet(&app, window) && !collapsed(&app, window));
    assert!(window.debug_bounds(HISTORY_PANEL).is_some());

    // Picking a thread closes the sheet and puts the collapse back, so a later
    // resize does not reopen the panel on its own.
    let active = app.read_with(window, |app, _| app.shell.tabs.active.clone().unwrap());
    click_selector(
        window,
        Box::leak(format!("ui-row-workshop-{active}").into_boxed_str()),
    );
    assert!(!sheet(&app, window));
    assert!(
        collapsed(&app, window),
        "the pick was not a request to keep it open"
    );
    resize(&app, window, 1400.);
    assert!(
        window.debug_bounds(HISTORY_PANEL).is_none(),
        "no spurious reopen"
    );

    // Escape on a sheet opened from a collapsed panel behaves the same way.
    resize(&app, window, 700.);
    click_selector(window, HISTORY_TOGGLE);
    assert!(sheet(&app, window));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!sheet(&app, window) && collapsed(&app, window));

    // Widening with a sheet open keeps the panel open, docked.
    click_selector(window, HISTORY_TOGGLE);
    assert!(sheet(&app, window));
    resize(&app, window, 1400.);
    assert!(window.debug_bounds(HISTORY_PANEL).is_some());
    app.read_with(window, |app, _| {
        assert!(!app.shell.conversations && !app.shell.tabs.history_collapsed)
    });

    // Hide from the sheet form is a real Hide: widening does not bring it back.
    resize(&app, window, 700.);
    click_selector(window, HISTORY_TOGGLE);
    assert!(sheet(&app, window));
    app.update(window, |app, cx| app.toggle_workshop_history(cx));
    window.run_until_parked();
    assert!(!sheet(&app, window), "toggling the shown sheet hides it");
    assert!(collapsed(&app, window));
    resize(&app, window, 1400.);
    assert!(window.debug_bounds(HISTORY_PANEL).is_none());
}

/// Activating the selected open-thread row from the keyboard leaves focus on a
/// control that exists afterwards: the composer when a sheet closed, the
/// panel's own selected row when it stays docked.
#[gpui::test]
fn workshop_conversations_keyboard_pick_keeps_a_focus_destination(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_geometry_tests::workshop_conversations_keyboard_pick_keeps_a_focus_destination",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.);
    let press_enter = |window: &mut gpui::VisualTestContext| {
        window.simulate_keystrokes("enter");
        window.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        window.run_until_parked();
    };
    let composer = app.read_with(window, |app, cx| {
        app.composer.read(cx).focus_handle().clone()
    });
    let row_focus = app.read_with(window, |app, _| app.shell.tabs.focus.clone());

    // Sheet.
    resize(&app, window, 700.);
    click_selector(window, HISTORY_TOGGLE);
    window.update(|win, cx| win.focus(&row_focus, cx));
    window.run_until_parked();
    assert!(window.update(|win, _| row_focus.is_focused(win)));
    press_enter(window);
    assert!(!app.read_with(window, |app, _| app.shell.conversations));
    assert!(
        window.update(|win, _| composer.is_focused(win)),
        "the sheet closed; focus moved to the composer, not to nothing"
    );

    // Docked.
    resize(&app, window, 1400.);
    app.update(window, |app, cx| app.set_workshop_history_open(true, cx));
    window.run_until_parked();
    window.update(|win, cx| win.focus(&row_focus, cx));
    window.run_until_parked();
    press_enter(window);
    assert!(
        window.update(|win, _| row_focus.is_focused(win)),
        "docked, the keyboard stays on the selected row"
    );
    assert!(window.debug_bounds(HISTORY_PANEL).is_some());
}

/// With more open threads than the list shows, the selected one is scrolled
/// into the list, as the old strip did.
#[gpui::test]
fn workshop_conversations_selected_open_row_scrolls_into_the_list(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_geometry_tests::workshop_conversations_selected_open_row_scrolls_into_the_list",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    theme::set_ui_scale(1.);
    resize(&app, window, 1400.);
    let tabs = app.update(window, |app, cx| {
        for _ in 0..14 {
            let id = app.create_conversation(awili_core::ConvKind::Work);
            app.open_conversation_from_history(&id, cx);
        }
        app.shell
            .tabs
            .tabs
            .iter()
            .map(|t| t.id.clone())
            .collect::<Vec<_>>()
    });
    for _ in 0..3 {
        window.update(|win, _| win.refresh());
        window.run_until_parked();
    }
    let list = window.debug_bounds("awili-workshop-open").unwrap();
    let visible = |window: &mut gpui::VisualTestContext, tab: &str| {
        let row = window
            .debug_bounds(Box::leak(format!("ui-row-workshop-{tab}").into_boxed_str()))
            .unwrap_or_else(|| panic!("row for {tab} is rendered"));
        row.top() >= list.top() - px(1.) && row.bottom() <= list.bottom() + px(1.)
    };
    assert!(
        list.size.height < px((tabs.len() as f32) * 44.),
        "the measured open-thread list viewport is shorter than its hit-target rows"
    );
    for tab in [&tabs[0], &tabs[tabs.len() - 1], &tabs[3]] {
        app.update(window, |app, cx| app.select_workshop_tab(tab, cx));
        for _ in 0..3 {
            window.update(|win, _| win.refresh());
            window.run_until_parked();
        }
        assert!(
            visible(window, tab),
            "the selected open thread {tab} is in view"
        );
    }
}
