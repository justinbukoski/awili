//! Actual selection controls, immutable reply metadata, and modal allocation.
use crate::{consolidation_tests::isolated, work::Mode, AwiliApp, MessageRow, Role};
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

#[gpui::test]
fn v2_settings_done_returns_to_retained_inspector(cx: &mut gpui::TestAppContext) {
    if !isolated("reply_inspector_tests::v2_settings_done_returns_to_retained_inspector") {
        return;
    }
    let (app, window) = setup(cx);
    app.update(window, add_reply);
    window.run_until_parked();
    click(window, "inspect-reply-1");
    app.update(window, |app, cx| app.open_settings(cx));
    window.run_until_parked();
    assert!(window.update(|win, cx| app.read(cx).settings_nav_focus.is_focused(win)));
    click(window, "sx-done-0");
    assert!(!app.read_with(window, |app, _| app.settings.open));
    assert!(
        window.update(|win, cx| app.read(cx).shell.inspector.focus.contains_focused(win, cx)),
        "The retained Inspector must regain its own keys"
    );
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(app.read_with(window, |app, _| app.shell.inspector.selection.is_none()));
}
fn add_reply(app: &mut AwiliApp, cx: &mut gpui::Context<AwiliApp>) -> String {
    let owner = app.sidebar.active_id().to_string();
    let view = app.views.get_mut(&owner).unwrap();
    for (role, text) in [
        (Role::User, "Original question to inspect"),
        (
            Role::Assistant,
            "Earlier owned reply with [a link](https://example.com/recorded?tail=COMPLETE).",
        ),
        (Role::User, "Later question"),
        (
            Role::Assistant,
            "Later reply must not supply the selected metadata",
        ),
    ] {
        let index = view.convo.len();
        view.convo.push(role, text);
        view.rows
            .push(MessageRow::new(view.convo.message(index).unwrap()));
    }
    let mut p = awili_core::Provenance::chat(
        "recorded-route",
        "recorded-provider",
        "claude-recorded-model",
        awili_core::Locality::Cloud,
        "Recorded reason for the selected older reply",
    );
    p.est_cost_usd = Some(0.1234);
    p.usage = Some(awili_core::Usage {
        cached_input_tokens: None,
        input_tokens: 31,
        output_tokens: 47,
    });
    view.convo.set_provenance(1, p);
    view.rows[1].footer = Some("Recorded footer · 13s · unchanged complete final summary".into());
    view.rows[1].activity = vec!["Actual owned tool result COMPLETE".into()];
    cx.notify();
    owner
}
fn click(window: &mut gpui::VisualTestContext, selector: &str) {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    if selector.starts_with("inspect-reply-") {
        for _ in 0..4 {
            let Some(viewport) = window.debug_bounds("awili-transcript-viewport") else {
                break;
            };
            let target = window.debug_bounds(selector).expect(selector);
            if viewport.contains(&target.center()) {
                break;
            }
            window.simulate_keystrokes(if target.top() < viewport.top() {
                "pageup"
            } else {
                "pagedown"
            });
            window.run_until_parked();
        }
    }
    let initial = window.debug_bounds(selector).expect(selector);
    if selector.starts_with("inspect-reply-") {
        if let Some(viewport) = window.debug_bounds("awili-transcript-viewport") {
            assert!(
                viewport.contains(&initial.center()),
                "Inspect pointer must be visible: {initial:?} / {viewport:?}"
            );
        }
    }
    window.simulate_event(gpui::MouseMoveEvent {
        position: initial.center(),
        pressed_button: None,
        modifiers: gpui::Modifiers::default(),
    });
    window.run_until_parked();
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
fn drawn(window: &mut gpui::VisualTestContext, row: &'static str, text: &str) {
    let bounds = window.debug_bounds(row).expect(row);
    assert!(
        crate::ui::text::DRAWN_TEXTS.with(|d| d.borrow().iter().any(|l| std::panic::catch_unwind(
            std::panic::AssertUnwindSafe(|| (l.text(), l.bounds()))
        )
        .ok()
        .is_some_and(|(t, b)| t == text && bounds.contains(&b.center())))),
        "Full recorded text {text:?} must be painted inside {row}"
    );
}

#[gpui::test]
fn inspector_older_reply_uses_its_record_and_copy(cx: &mut gpui::TestAppContext) {
    if !isolated("reply_inspector_tests::inspector_older_reply_uses_its_record_and_copy") {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.update(window, add_reply);
    window.run_until_parked();
    click(window, "inspect-reply-1");
    assert!(!app.read_with(window, |app, _| app.shell.inspector.details));
    drawn(window, "ui-row-reply-inspector-model", "Claude · cloud");
    drawn(window, "ui-row-reply-inspector-cost", "Estimated $0.1234");
    drawn(
        window,
        "ui-row-reply-inspector-time",
        "Recorded summary: Recorded footer · 13s · unchanged complete final summary",
    );
    drawn(
        window,
        "ui-row-reply-inspector-sources",
        "No sources were recorded",
    );
    drawn(
        window,
        "ui-row-reply-inspector-link-0",
        "https://example.com/recorded?tail=COMPLETE",
    );
    click(window, "ui-button-reply-copy");
    let copied = window.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()).unwrap());
    assert_eq!(
        copied,
        app.read_with(window, |app, _| app.views[&owner]
            .convo
            .message(1)
            .unwrap()
            .text
            .clone())
    );
    click(window, "ui-section-reply-inspector-details");
    assert!(app.read_with(window, |app, _| app.shell.inspector.details));
    assert!(
        window.debug_bounds("ui-button-reply-report").is_some(),
        "Absent report stays visibly disabled"
    );
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(app.read_with(window, |app, _| app.shell.inspector.selection.is_none()));
    assert!(window.update(|win, cx| app
        .read(cx)
        .composer
        .read(cx)
        .focus_handle()
        .is_focused(win)));
}

#[gpui::test]
fn inspector_retains_append_but_rejects_same_index_rewrite(cx: &mut gpui::TestAppContext) {
    if !isolated("reply_inspector_tests::inspector_retains_append_but_rejects_same_index_rewrite") {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.update(window, add_reply);
    window.run_until_parked();
    click(window, "inspect-reply-1");
    let selected = app.read_with(window, |app, _| {
        app.shell.inspector.selection.clone().unwrap()
    });
    app.update(window, |app, cx| {
        let view = app.views.get_mut(&owner).unwrap();
        view.convo.push(Role::User, "Another appended question");
        view.rows
            .push(MessageRow::new(view.convo.message(4).unwrap()));
        assert!(
            selected.matches(app),
            "Appending a later turn must not alias or erase an unchanged selected reply"
        );
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-reply-inspector").is_some());
    app.update(window, |app, cx| {
        let view = app.views.get_mut(&owner).unwrap();
        let old = view.convo.message(1).unwrap().clone();
        view.truncate_to(1);
        view.convo.push(Role::Assistant, &old.text);
        assert_eq!(view.convo.message(1).unwrap().ts, old.ts);
        view.rows
            .push(MessageRow::new(view.convo.message(1).unwrap()));
        assert!(
            !selected.matches(app),
            "Same index, timestamp and text after a rewrite are a different selection"
        );
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-reply-inspector").is_none());
    assert!(
        window.update(|win, cx| app
            .read(cx)
            .composer
            .read(cx)
            .focus_handle()
            .is_focused(win)),
        "Removing a stale Inspector returns its owned focus"
    );
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-j"));
    window.run_until_parked();
    assert!(app.read_with(window, |app, _| app.shell.job_open));
}

#[gpui::test]
fn inspector_check_survives_menu_close_and_prepares_new_thread_without_send(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("reply_inspector_tests::inspector_check_survives_menu_close_and_prepares_new_thread_without_send") { return; }
    let (app, window) = setup(cx);
    let owner = app.update(window, add_reply);
    let old_tab = app.read_with(window, |app, _| app.shell.tabs.active.clone());
    app.update(window, |app, cx| {
        app.composer.update(cx, |c, cx| {
            c.set_text("Original owner's unsent staging", cx)
        });
        cx.notify();
    });
    window.run_until_parked();
    click(window, "inspect-reply-1");
    click(window, "ui-button-reply-check");
    // The real kit Menu closes/restores its trigger BEFORE invoking the item.
    window.update(|win, _| win.refresh());
    window.run_until_parked();
    click(window, "ui-row-awili-workshop-menu-item-0");
    app.read_with(window, |app, cx| {
        assert_ne!(app.shell.tabs.active, old_tab);
        assert_ne!(app.sidebar.active_id(), owner);
        assert_eq!(app.work.executor, crate::work::WorkExecutor::InApp);
        let request = app.composer.read(cx).text().to_string();
        assert!(
            request.contains("Original question to inspect")
                && request.contains("Earlier owned reply with")
                && !request.contains("Later reply must not")
        );
        assert!(app.views[app.sidebar.active_id()].convo.is_empty());
        assert!(
            !app.work.running
                && app.work.local.health.pending_send.is_none()
                && app.work.health.gateway.pending_send.is_none(),
            "Helper choice prepares a draft and never dispatches"
        );
        assert!(app.shell.inspector.check.is_none());
    });
    window.update(|win, cx| app.update(cx, |app, cx| app.select_conversation(&owner, win, cx)));
    window.run_until_parked();
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        "Original owner's unsent staging"
    );
}

#[gpui::test]
fn inspector_native_session_uses_record_and_reopen_identity(cx: &mut gpui::TestAppContext) {
    if !isolated("reply_inspector_tests::inspector_native_session_uses_record_and_reopen_identity")
    {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.update(window, |app, cx| {
        app.new_workshop_with(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None,
            },
            cx,
        );
        let tab = &mut app.work.code_tabs[app.work.code_active];
        let owner = "inspector-native-owned-history".to_string();
        tab.history.conv_id = Some(owner.clone());
        *tab.session_slot.lock().unwrap() = Some("original-native-resume".into());
        tab.convo.push(Role::User, "Native owned question");
        tab.convo.push(Role::Assistant, "Native owned reply");
        tab.convo.set_provenance(
            1,
            awili_core::Provenance::chat(
                "native-recorded",
                "native-provider",
                "codex-recorded-model",
                awili_core::Locality::Local,
                "Native recorded reason",
            ),
        );
        tab.rows = tab.convo.messages().iter().map(MessageRow::new).collect();
        cx.notify();
        owner
    });
    window.run_until_parked();
    click(window, "inspect-reply-1");
    drawn(window, "ui-row-reply-inspector-model", "Codex · local");
    click(window, "ui-button-reply-copy");
    assert_eq!(
        window.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()).unwrap()),
        "Native owned reply"
    );
    let selected = app.read_with(window, |app, _| {
        app.shell.inspector.selection.clone().unwrap()
    });
    app.update(window, |app, cx| {
        let tab = &mut app.work.code_tabs[app.work.code_active];
        assert_eq!(tab.history.conv_id.as_deref(), Some(owner.as_str()));
        assert_eq!(
            tab.session_slot.lock().unwrap().as_deref(),
            Some("original-native-resume")
        );
        let same_transcript = tab.convo.clone();
        let plan = crate::code_reopen::ReopenPlan {
            engine: tab.engine,
            model: tab.model.clone(),
            folder: tab.project_folder.clone(),
            session: tab.session_slot.lock().unwrap().clone(),
            engine_known: true,
            route: tab.route.clone(),
        };
        crate::code_reopen::apply_reopen(tab, &owner, same_transcript, &plan);
        assert!(
            !selected.matches(app),
            "Reopened same-text native transcript has a different selection identity"
        );
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-reply-inspector").is_none());
    assert!(window.update(|win, cx| app
        .read(cx)
        .composer
        .read(cx)
        .focus_handle()
        .is_focused(win)));
}

#[gpui::test]
fn inspector_modal_bounds_wrap_actions_and_escape_does_not_stop_background_job(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("reply_inspector_tests::inspector_modal_bounds_wrap_actions_and_escape_does_not_stop_background_job") { return; }
    let (app, window) = setup(cx);
    let owner = app.update(window, add_reply);
    for scale in [0.9, 1.25] {
        window.update(|win, cx| app.update(cx, |app, cx| app.apply_ui_scale(scale, win, cx)));
        window.simulate_resize(size(px(800.), px(700.)));
        window.run_until_parked();
        click(window, "inspect-reply-1");
        let panel = window.debug_bounds("awili-reply-inspector").unwrap();
        let rail = window.debug_bounds("awili-rail").unwrap();
        let scrim = window.debug_bounds("ui-sheet-scrim").unwrap();
        assert!(
            scrim.contains(&rail.center()),
            "Inspector Sheet covers the rail at{scale}"
        );
        for id in [
            "ui-button-reply-copy",
            "ui-button-reply-check",
            "ui-button-reply-report",
        ] {
            let b = window.debug_bounds(id).unwrap();
            assert!(
                b.left() >= panel.left()
                    && b.right() <= panel.right()
                    && b.top() >= panel.top()
                    && b.bottom() <= panel.bottom(),
                "{scale}:{id} outside Inspector {b:?}/{panel:?}"
            );
            assert!(
                b.size.height >= crate::theme::target(),
                "44 logical point control floor"
            );
        }
        for _ in 0..18 {
            window.simulate_keystrokes("tab");
            window.run_until_parked();
            assert!(
                window.update(|win, cx| app
                    .read(cx)
                    .shell
                    .inspector
                    .focus
                    .contains_focused(win, cx)),
                "Modal Tab stays in Inspector"
            );
        }
        app.update(window, |app, cx| {
            app.work.running = true;
            app.work.conv_id = Some("background-owner".into());
            cx.notify();
        });
        window.simulate_keystrokes("escape");
        window.run_until_parked();
        assert!(
            app.read_with(window, |app, _| app.shell.inspector.selection.is_none()
                && app.work.running),
            "Escape closes Inspector before stopping background work"
        );
        app.update(window, |app, cx| {
            app.work.running = false;
            app.work.conv_id = Some(owner.clone());
            cx.notify();
        });
    }
    window.update(|win, cx| app.update(cx, |app, cx| app.apply_ui_scale(1., win, cx)));
}

#[gpui::test]
fn inspector_consolidation_owned_error_replaces_only_identical_refusal_toast(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("reply_inspector_tests::inspector_consolidation_owned_error_replaces_only_identical_refusal_toast") { return; }
    let (app, window) = setup(cx);
    let dir = tempfile::tempdir().unwrap();
    app.update(window, |app, cx| {
        app.work.folder = Some(dir.path().join("missing-owned-folder"));
        let owner = app.sidebar.active_id().to_string();
        app.submit_work_run_kind(
            "Keep the refused request".into(),
            owner,
            awili_core::MessageKind::Normal,
            cx,
        );
        let cause = &app.current_work_error().unwrap().cause;
        assert_eq!(
            app.toast.visible().unwrap().as_ref(),
            cause.as_str(),
            "Genuine duplicate cause produced by the existing refusal path"
        );
    });
    window.run_until_parked();
    assert!(window.debug_bounds("ui-card-work-error-card").is_some());
    assert!(
        window.debug_bounds("awili-toast").is_none(),
        "Owned ErrorCard removes duplicate toast over the draft"
    );
    app.update(window, |app, cx| {
        app.show_sticky_toast("A different notice remains visible", cx)
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("awili-toast").is_some(),
        "Unrelated notices retain their existing presentation"
    );
}

#[gpui::test]
fn inspector_absent_metadata_and_navigation_do_not_borrow_latest_job(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "reply_inspector_tests::inspector_absent_metadata_and_navigation_do_not_borrow_latest_job",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.update(window, add_reply);
    app.update(window, |app, cx| {
        app.work.panel.model = Some("Unrelated latest job model".into());
        app.apply_work_update(crate::work::WorkUpdate::ReportedCost(77.), cx);
        cx.notify();
    });
    window.run_until_parked();
    click(window, "inspect-reply-3");
    for row in [
        "ui-row-reply-inspector-model",
        "ui-row-reply-inspector-time",
        "ui-row-reply-inspector-cost",
    ] {
        drawn(window, row, "Not recorded for this reply");
    }
    let selected = app.read_with(window, |app, _| {
        app.shell.inspector.selection.clone().unwrap()
    });
    window.update(|win, cx| {
        app.update(cx, |app, cx| {
            app.new_workshop_with(crate::work::WorkExecutor::InApp, cx);
            app.focus_composer(win, cx);
            assert!(
                !selected.matches(app),
                "Navigation invalidates captured selection commands"
            );
        })
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-reply-inspector").is_none());
    assert_ne!(
        app.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner
    );
}

#[gpui::test]
fn inspector_display_annotations_do_not_change_record_identity(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "reply_inspector_tests::inspector_display_annotations_do_not_change_record_identity",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.update(window, add_reply);
    for (text, plan_safe) in [
        ("Visible retained reply.\n\n[answered by Claude]", false),
        ("Visible retained reply.\n[[awili:plan-ready]]", true),
    ] {
        app.update(window, |app, cx| {
            let view = app.views.get_mut(&owner).unwrap();
            view.truncate_to(1);
            view.convo.push(Role::Assistant, text);
            let msg = view.convo.message(1).unwrap();
            let mut row = MessageRow::new(msg);
            if plan_safe {
                row.stream_taken_back(msg);
            }
            assert!(!row.raw.contains("[answered by") && !row.raw.contains("[[awili"));
            view.rows.push(row);
            cx.notify();
        });
        window.run_until_parked();
        click(window, "inspect-reply-1");
        assert!(
            window.debug_bounds("awili-reply-inspector").is_some(),
            "A recorded annotation must not make the visible reply uninspectable"
        );
        click(window, "ui-button-reply-copy");
        let copied =
            window.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()).unwrap());
        assert_eq!(copied.trim_end(), "Visible retained reply.");
        app.read_with(window, |app, _| {
            assert_eq!(
                app.views[&owner].convo.message(1).unwrap().text,
                text,
                "Inspection does not rewrite the actual stored record"
            );
        });
        window.simulate_keystrokes("escape");
        window.run_until_parked();
    }
}

#[gpui::test]
fn inspector_review_job_island_replaces_old_inspection_without_stopping(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("reply_inspector_tests::inspector_review_job_island_replaces_old_inspection_without_stopping") { return; }
    let (app, window) = setup(cx);
    let owner = app.update(window, add_reply);
    window.run_until_parked();
    click(window, "inspect-reply-1");
    let (tx, rx) = std::sync::mpsc::channel();
    app.update(window, |app, cx| {
        let view = app.views.get_mut(&owner).unwrap();
        view.convo.push(Role::User, "Next job in the same thread");
        view.rows
            .push(MessageRow::new(view.convo.message(4).unwrap()));
        app.work.running = true;
        app.work.conv_id = Some(owner.clone());
        app.work.run_seq = 42;
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-reply-inspector").is_some());
    click(window, "ui-button-job-island-open");
    assert!(window.debug_bounds("awili-reply-inspector").is_none());
    assert!(window.debug_bounds("awili-work-panel").is_some());
    assert!(window.update(|win, cx| app.read(cx).shell.job_focus.contains_focused(win, cx)));
    app.read_with(window, |app, _| {
        assert!(app.work.running);
        assert_eq!(app.work.run_seq, 42);
        assert_eq!(app.work.conv_id.as_deref(), Some(owner.as_str()));
    });
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
}

/// rc.3 S2: Inspect reply lists the prompt's parts largest first, each with
/// its size and a share bar, and the total against the model's window.
#[gpui::test]
fn inspector_lists_the_context_parts_with_share_bars(cx: &mut gpui::TestAppContext) {
    if !isolated("reply_inspector_tests::inspector_lists_the_context_parts_with_share_bars") {
        return;
    }
    use crate::context_breakdown::{ContextBreakdown, ContextPart, PartCategory};
    let (app, window) = setup(cx);
    app.update(window, |app, cx| {
        let owner = add_reply(app, cx);
        let mut b = ContextBreakdown {
            cli: Some("Claude".into()),
            ..Default::default()
        };
        b.push(ContextPart::estimate(
            "Your message",
            PartCategory::Message,
            400,
        ));
        b.push(ContextPart::estimate(
            "AGENTS.md in proj",
            PartCategory::Rules,
            8_000,
        ));
        b.settle(Some(12_100), Some((200_000, true)), false);
        app.views.get_mut(&owner).unwrap().rows[1].context = Some(b);
        cx.notify();
    });
    window.run_until_parked();
    click(window, "inspect-reply-1");
    drawn(
        window,
        "ui-row-reply-inspector-context",
        "Total: 12.1K of 200K",
    );
    // Largest first: the CLI's own 10K remainder, then the rules file, then the message.
    drawn(
        window,
        "ui-row-reply-inspector-context-part-0",
        "10K tokens \u{b7} 83%",
    );
    drawn(
        window,
        "ui-row-reply-inspector-context-part-1",
        "about 2K tokens \u{b7} 17%",
    );
    drawn(
        window,
        "ui-row-reply-inspector-context-part-2",
        "about 100 tokens \u{b7} 1%",
    );
    let bar = window.debug_bounds("context-bar-0").expect("bar 0");
    let bar2 = window.debug_bounds("context-bar-2").expect("bar 2");
    assert!(bar.size.width > bar2.size.width * 10.);
}

/// rc.3 S2: a window the provider did not report (the table's fallback) is
/// shown as "about", and a CLI that summed tool steps says so.
#[gpui::test]
fn inspector_marks_a_fallback_window_and_summed_tool_steps(cx: &mut gpui::TestAppContext) {
    if !isolated("reply_inspector_tests::inspector_marks_a_fallback_window_and_summed_tool_steps") {
        return;
    }
    use crate::context_breakdown::{ContextBreakdown, ContextPart, PartCategory};
    let (app, window) = setup(cx);
    app.update(window, |app, cx| {
        let owner = add_reply(app, cx);
        let mut b = ContextBreakdown {
            cli: Some("GLM".into()),
            ..Default::default()
        };
        b.push(ContextPart::estimate(
            "Your message",
            PartCategory::Message,
            400,
        ));
        b.settle(Some(5_100), Some((200_000, false)), true);
        app.views.get_mut(&owner).unwrap().rows[1].context = Some(b);
        cx.notify();
    });
    window.run_until_parked();
    click(window, "inspect-reply-1");
    drawn(
        window,
        "ui-row-reply-inspector-context",
        "Total: 5.1K across tool steps \u{b7} window about 200K",
    );
    drawn(
        window,
        "ui-row-reply-inspector-context-part-0",
        "5K tokens \u{b7} 98%",
    );
    let row = app.read_with(window, |app, _| {
        let owner = app.sidebar.active_id().to_string();
        app.views[&owner].rows[1].context.clone().unwrap()
    });
    assert_eq!(
        row.parts.last().unwrap().label,
        "GLM's own instructions and tools, plus tool steps"
    );
}
