//! Production completion, ownership, picker reducer, and actual rendered controls.
use crate::{consolidation_tests::isolated, work::Mode, AwiliApp};
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
        app.work.conv_id = Some(app.sidebar.active_id().into());
        cx.notify();
    });
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();
    (app, window)
}
fn fail(
    app: &mut AwiliApp,
    cx: &mut gpui::Context<AwiliApp>,
    cause: &str,
    partial: &str,
) -> String {
    let owner = app.sidebar.active_id().to_string();
    app.work.conv_id = Some(owner.clone());
    let view = app.views.get_mut(&owner).unwrap();
    let idx = view.convo.len();
    view.convo.push(crate::Role::User, "Original owned request");
    view.rows
        .push(crate::MessageRow::new(view.convo.message(idx).unwrap()));
    view.convo.push(crate::Role::Assistant, partial);
    view.rows
        .push(crate::MessageRow::new(view.convo.message(idx + 1).unwrap()));
    app.work.stream_row_idx = Some(idx + 1);
    app.work.panel.error = Some(cause.into());
    app.work.panel.status = Some(awili_gateway::run_events::RunStatus::Failed);
    app.work.running = false;
    app.work_finish_stream(cx);
    owner
}
fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = window.debug_bounds(selector).expect(selector);
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
}

#[gpui::test]
fn inline_error_completion_is_owned_inline_once_and_review_focuses_real_action(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_completion_is_owned_inline_once_and_review_focuses_real_action",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.update(window, |app, cx| {
        fail(
            app,
            cx,
            "The fixture helper failed after partial output.",
            "Partial output remains.",
        )
    });
    window.run_until_parked();
    let card = window
        .debug_bounds("ui-card-work-error-card")
        .expect("inline kit ErrorCard");
    let row = window.debug_bounds("awili-message-row-1").unwrap();
    assert!(
        row.contains(&card.center()),
        "completed error stays within owning reply"
    );
    app.update(window, |app, cx| {
        assert!(app.work.stream_row_idx.is_none());
        assert_eq!(
            app.work.panel.error.as_deref(),
            Some("The fixture helper failed after partial output.")
        );
        assert_eq!(
            app.views[&owner].convo.message(1).unwrap().text,
            "Partial output remains."
        );
        assert_eq!(
            app.work_error_tone(app.current_work_error().unwrap()),
            crate::theme::Tone::ActNow
        );
        assert_eq!(
            app.composer_status_chip(cx).display_name().as_ref(),
            "Failed"
        );
        app.work.running = true;
        assert_eq!(
            app.composer_status_chip(cx).display_name().as_ref(),
            "Running"
        );
        app.work.running = false;
        app.shell.job_open = true;
        let other = app.create_conversation(awili_core::ConvKind::Work);
        app.work_last_run.insert(
            other.clone(),
            crate::work_last_run::LastRun {
                identity: crate::work_run_identity::WorkRunIdentity::OwnedEngine {
                    model: "other-owned-model".into(), engine_id: None,
                },
                model: Some("other-owned-model".into()),
                status: awili_gateway::run_events::RunStatus::Completed,
                error: None,
                finished_at: "2026-10-13T00:00:00Z".into(),
            }
            .to_json(),
        );
        app.open_conversation_from_history(&other, cx);
        assert_eq!(
            app.work.conv_id.as_deref(),
            Some(other.as_str()),
            "production idle History restore takes its own panel"
        );
        assert!(app.work.panel.error.is_none());
        assert_eq!(
            app.composer_status_chip(cx).display_name().as_ref(),
            "Ready"
        );
        assert_eq!(
            app.current_work_error().unwrap().owner,
            owner,
            "live ErrorCard presentation survives another thread's stored History"
        );
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("ui-card-work-error-card").is_none(),
        "no other-owner or History duplicate"
    );
    click(window, "ui-button-job-island-open");
    assert_eq!(
        app.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner
    );
    assert!(!app.read_with(window, |app, _| app.shell.job_open));
    assert!(window.update(|win, cx| app.read(cx).shell.error_focus.is_focused(win)));
    let button = window.debug_bounds("error-retry").expect("actual Retry");
    assert!(button.size.height >= px(44.));
    let viewport = window.debug_bounds("awili-transcript-viewport").unwrap();
    assert!(button.top() >= viewport.top() && button.bottom() <= viewport.bottom());
    app.update(window, |app, cx| {
        app.shell.job_open = true;
        cx.notify();
    });
    window.run_until_parked();
    let card = window.debug_bounds("ui-card-work-error-card").unwrap();
    assert!(window
        .debug_bounds("awili-message-row-1")
        .unwrap()
        .contains(&card.center()));
    assert_eq!(
        app.read_with(window, |app, _| app.workshop_error_row()),
        Some(1)
    );
}

/// Failed updates share the normal terminal path. The stream-row take is the
/// once-only latch: a duplicate terminal event cannot append another error
/// reply or re-arm completion ownership for the same accepted turn.
#[gpui::test]
fn failed_terminal_completion_is_latched_once(cx: &mut gpui::TestAppContext) {
    if !isolated("inline_error_tests::failed_terminal_completion_is_latched_once") {
        return;
    }
    let (app, window) = setup(cx);
    app.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        app.work.conv_id = Some(owner.clone());
        let view = app.views.get_mut(&owner).unwrap();
        view.convo.push(crate::Role::User, "request");
        view.rows
            .push(crate::MessageRow::new(view.convo.message(0).unwrap()));
        view.convo.push(crate::Role::Assistant, "");
        view.rows
            .push(crate::MessageRow::new(view.convo.message(1).unwrap()));
        app.work.stream_row_idx = Some(1);
        app.work.running = true;
        app.apply_work_update(
            crate::work::WorkUpdate::Failed("fixture failure".into()),
            cx,
        );
        let once_len = app.views[&owner].convo.len();
        let once_text = app.views[&owner]
            .convo
            .message(1)
            .map(|message| message.text.clone());
        assert!(app.work.stream_row_idx.is_none());
        app.apply_work_update(crate::work::WorkUpdate::Failed("late failure".into()), cx);
        assert_eq!(app.views[&owner].convo.len(), once_len);
        assert_eq!(
            app.views[&owner]
                .convo
                .message(1)
                .map(|message| message.text.clone()),
            once_text
        );
        assert!(app.work.stream_row_idx.is_none());
    });
}

#[gpui::test]
fn inline_error_missing_folder_pins_requested_owner_and_cancel_keeps_cause(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_missing_folder_pins_requested_owner_and_cancel_keeps_cause",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("moved-folder");
    let owner = app.update(window, |app, cx| {
        let old = fail(app, cx, "Old failure", "Old reply");
        let owner = app.create_conversation(awili_core::ConvKind::Work);
        app.set_active_conversation(&owner);
        app.set_mode(Mode::Work, cx);
        app.work.folder = Some(missing.clone());
        app.work.stream_row_idx = Some(1); // prior owner's stale live index
        app.guard_submit("Unsent requested draft");
        app.submit_work_run_kind(
            "Unsent requested draft".into(),
            owner.clone(),
            awili_core::MessageKind::Normal,
            cx,
        );
        app.settle_submit_guard(cx);
        assert_ne!(old, owner);
        assert_eq!(app.work.conv_id.as_deref(), Some(owner.as_str()));
        assert!(app.work.stream_row_idx.is_none());
        assert!(app.views[&owner].convo.messages().is_empty());
        assert!(!app.work.running);
        assert_eq!(app.composer.read(cx).text(), "Unsent requested draft");
        let error = app.current_work_error().unwrap().clone();
        assert_eq!(app.work_error_tone(&error), crate::theme::Tone::ActNow);
        assert!(!app.apply_error_folder_pick(&error, None, cx));
        assert!(app.work_error_matches(&error));
        assert_eq!(app.work.panel.error.as_ref(), Some(&error.cause));
        owner
    });
    window.run_until_parked();
    assert!(window.debug_bounds("error-fix-folder").is_some());
    app.update(window, |app, cx| {
        let old_error = app.current_work_error().unwrap().clone();
        let other = app.create_conversation(awili_core::ConvKind::Work);
        app.set_active_conversation(&other);
        let folder_before = app.work.folder.clone();
        assert!(!app.apply_error_folder_pick(&old_error, Some(parent.path().into()), cx));
        assert_eq!(app.work.folder, folder_before);
        assert!(app.work_error_matches(&old_error));
        app.set_active_conversation(&owner);
        assert!(app.apply_error_folder_pick(&old_error, Some(parent.path().into()), cx));
        assert!(app.work.panel.error.is_none());
        assert_eq!(app.work.folder.as_deref(), Some(parent.path()));
    });
}

#[gpui::test]
fn inline_error_same_index_replacement_and_view_recreation_fall_back_without_false_retry(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_same_index_replacement_and_view_recreation_fall_back_without_false_retry",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.update(window, |app, cx| {
        fail(app, cx, "Retained cause", "Original failed reply")
    });
    app.update(window, |app, cx| {
        let view = app.views.get_mut(&owner).unwrap();
        let ts = view.convo.message(1).unwrap().ts;
        view.truncate_to(1);
        view.convo
            .push(crate::Role::Assistant, "Original failed reply");
        view.rows
            .push(crate::MessageRow::new(view.convo.message(1).unwrap()));
        assert_eq!(
            view.convo.message(1).unwrap().ts,
            ts,
            "strong same-ts/same-text replacement control"
        );
        assert_eq!(
            app.workshop_error_row(),
            None,
            "removed row never regains presentation"
        );
        cx.notify();
    });
    window.run_until_parked();
    let card = window.debug_bounds("ui-card-work-error-card").unwrap();
    let row = window.debug_bounds("awili-message-row-1").unwrap();
    assert!(
        !row.contains(&card.center()),
        "fallback outside unrelated replacement reply"
    );
    assert!(window.debug_bounds("error-retry").is_none());
    app.update(window, |app, cx| {
        let state = app.views[&owner].convo.clone();
        app.views
            .insert(owner.clone(), crate::conv_view::ConvView::from_state(state));
        assert_eq!(app.workshop_error_row(), None);
        app.work.panel = Default::default();
        app.work.error_presentation = None;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("ui-card-work-error-card").is_none());
    assert!(window.debug_bounds("ui-button-job-island-open").is_none());
}

#[gpui::test]
fn inline_error_review_reveals_action_after_tall_owned_activity_at_live_zoom(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_review_reveals_action_after_tall_owned_activity_at_live_zoom",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.update(window, |app, cx| {
        let owner = fail(
            app,
            cx,
            "Retained helper failure with a next step",
            "An owned partial reply",
        );
        app.work.stream_row_idx = Some(1);
        for n in 0..90 {
            app.work_on_activity(format!("Completed owned Work step {n}"), cx);
        }
        app.toggle_row_activity(&owner, 1, cx);
        app.work.stream_row_idx = None;
        owner
    });
    for scale in [0.9, 1.25] {
        window.update(|win, cx| app.update(cx, |app, cx| app.apply_ui_scale(scale, win, cx)));
        app.update(window, |app, cx| {
            app.views[&owner]
                .scroll
                .set_offset(gpui::point(px(0.), px(0.)));
            app.shell.more = true;
            cx.notify();
        });
        window.run_until_parked();
        // More legitimately owns its scrim; dismiss it before the real island.
        window.simulate_keystrokes("escape");
        window.run_until_parked();
        click(window, "ui-button-job-island-open");
        let b = window.debug_bounds("error-retry").unwrap();
        let v = window.debug_bounds("awili-transcript-viewport").unwrap();
        assert!(
            b.top() >= v.top() && b.bottom() <= v.bottom(),
            "{scale}: actual action {b:?} outside {v:?}"
        );
        assert!(window.update(|win, cx| app.read(cx).shell.error_focus.is_focused(win)));
        assert!(app.read_with(window, |app, _| app.workshop_error_row() == Some(1)));
    }
    window.update(|win, cx| app.update(cx, |app, cx| app.apply_ui_scale(1., win, cx)));
}

#[gpui::test]
fn inline_error_keyboard_retry_dispatches_original_owned_prompt_once_and_keeps_unsent_draft(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_keyboard_retry_dispatches_original_owned_prompt_once_and_keeps_unsent_draft",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let owner = app.update(window, |app, cx| {
        let cause = crate::agent_signin::signed_out_line(
            Some(awili_code::CodeEngine::Claude),
            "Claude: OAuth session expired",
        )
        .unwrap();
        let owner = fail(app, cx, &cause, "Partial written reply");
        assert_eq!(
            app.work_error_tone(app.current_work_error().unwrap()),
            crate::theme::Tone::ActNow
        );
        app.work.folder = Some(dir.path().into());
        // Chosen as the chooser does: the retry follows the conversation's
        // next-turn target, not the ambient executor field.
        assert!(app.set_composer_target(
            &crate::workshop_composer::ModelOwner::Work(owner.clone()),
            crate::work::WorkExecutor::Gateway,
            cx,
        ));
        app.work.gateway.enabled = true;
        app.work.worker_identity = Some(crate::work_run_identity::WorkRunIdentity::HermesGateway);
        app.work.gateway_cmd_tx = Some(tx);
        app.composer
            .update(cx, |c, cx| c.set_text("Unsent staging remains", cx));
        cx.notify();
        owner
    });
    window.run_until_parked();
    click(window, "ui-button-job-island-open");
    window.simulate_keystrokes("enter");
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();
    assert!(
        matches!(rx.try_recv(), Ok(crate::work::GatewayCmd::Prompt {text, ..}) if text == "Original owned request")
    );
    assert!(
        rx.try_recv().is_err(),
        "single captured original retry dispatch"
    );
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-j"));
    window.run_until_parked();
    assert!(
        app.read_with(window, |app, _| app.shell.job_open),
        "Job shortcut remains usable after Retry removes its focused ErrorCard"
    );
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    app.update(window, |app, cx| {
        assert_eq!(app.work.conv_id.as_deref(), Some(owner.as_str()));
        assert_eq!(app.composer.read(cx).text(), "Unsent staging remains");
        assert!(app.work.panel.error.is_none());
        assert!(app.work.error_presentation.is_none());
        assert!(app.work.running);
        // Drain the owned in-memory fixture through its actual finish path.
        app.work.panel.status = Some(awili_gateway::run_events::RunStatus::Stopped);
        app.work.running = false;
        app.work_finish_stream(cx);
        app.work.gateway_cmd_tx = None;
        // The send follows the conversation's route (still the Gateway from
        // the retry above), so switch it the way the chooser does.
        assert!(app.set_composer_target(
            &crate::workshop_composer::ModelOwner::Work(owner.clone()),
            crate::work::WorkExecutor::InApp,
            cx,
        ));
        app.work.engine.base_url.clear();
        app.submit_work_run_kind(
            "Request awaiting engine setup".into(),
            owner,
            awili_core::MessageKind::Normal,
            cx,
        );
        assert!(!app.work.running);
        assert_eq!(
            app.work.panel.error.as_deref(),
            Some(crate::work::ENGINE_ENDPOINT_NOTICE)
        );
        assert_eq!(
            app.work_error_tone(app.current_work_error().unwrap()),
            crate::theme::Tone::ActNow
        );
    });
}

#[gpui::test]
fn inline_error_late_same_owner_picker_cannot_clear_replacement_and_used_thread_keeps_folder(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_late_same_owner_picker_cannot_clear_replacement_and_used_thread_keeps_folder",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original-folder");
    let destination = dir.path().join("chosen-folder");
    std::fs::create_dir(&destination).unwrap();
    let owner = app.update(window, |app, cx| {
        let cause = crate::submit_guard::missing_folder_notice("original-folder");
        let owner = fail(app, cx, &cause, "Original failed reply");
        app.conversations.get_mut(&owner).unwrap().cwd = Some(original.display().to_string());
        app.work.folder = Some(original.clone());
        app.composer.update(cx, |c, cx| {
            c.set_text("Returned request remains with its original owner", cx)
        });
        let old = app.current_work_error().unwrap().clone();
        app.submit_work_run_kind(
            "Replacement refused prompt".into(),
            owner.clone(),
            awili_core::MessageKind::Normal,
            cx,
        );
        let replacement = app.current_work_error().unwrap().clone();
        assert!(!app.apply_error_folder_pick(&old, Some(destination.clone()), cx));
        assert!(app.work_error_matches(&replacement));
        assert_eq!(app.work.folder.as_deref(), Some(original.as_path()));
        app.work.gateway_stop_pending = true;
        assert!(!app.apply_error_folder_pick(&replacement, Some(destination.clone()), cx));
        assert!(app.work_error_matches(&replacement));
        app.work.gateway_stop_pending = false;
        assert!(app.apply_error_folder_pick(&replacement, Some(destination.clone()), cx));
        assert_ne!(
            app.sidebar.active_id(),
            owner,
            "changing a used thread's folder opens the existing fresh-thread path"
        );
        assert_eq!(
            app.conversations.get(&owner).unwrap().cwd.as_deref(),
            Some(original.to_str().unwrap())
        );
        assert_eq!(
            app.views[&owner].convo.message(1).unwrap().text,
            "Original failed reply"
        );
        app.sync_composer_draft(cx);
        owner
    });
    window.run_until_parked();
    window.update(|win, cx| app.update(cx, |app, cx| app.select_conversation(&owner, win, cx)));
    window.run_until_parked();
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        "Replacement refused prompt\n\nReturned request remains with its original owner"
    );
}

#[gpui::test]
fn inline_error_missing_folder_during_stop_keeps_draining_owner_and_parked_request(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_missing_folder_during_stop_keeps_draining_owner_and_parked_request",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let root = tempfile::tempdir().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    app.update(window, |app, cx| {
        let draining_owner = fail(
            app,
            cx,
            "Evidence on draining turn",
            "Existing stopped reply",
        );
        app.work.stream_row_idx = Some(1);
        app.work.gateway_stop_pending = true;
        app.work.gateway_cmd_tx = Some(tx);
        app.work.folder = Some(root.path().join("gone-while-stopping"));
        let original_error = app.current_work_error().unwrap().clone();
        let original_epoch = app.work.run_seq;
        let requesting = app.create_conversation(awili_core::ConvKind::Work);
        app.set_active_conversation(&requesting);
        app.guard_submit("Parked owner request");
        app.submit_work_run_kind(
            "Parked owner request".into(),
            requesting.clone(),
            awili_core::MessageKind::Normal,
            cx,
        );
        app.settle_submit_guard(cx);
        assert_eq!(app.work.conv_id.as_deref(), Some(draining_owner.as_str()));
        assert_eq!(app.work.stream_row_idx, Some(1));
        assert_eq!(app.work.run_seq, original_epoch);
        assert!(app.work.gateway_stop_pending);
        assert!(app.work_error_matches(&original_error));
        let queued = app
            .work
            .queued_after_stop
            .as_ref()
            .expect("existing Stop admission parks request");
        assert_eq!(queued.conv_id, requesting);
        assert_eq!(queued.text, "Parked owner request");
        assert!(app.views[&requesting].convo.messages().is_empty());
        assert!(
            rx.try_recv().is_err(),
            "no dispatch or premature worker teardown"
        );
        app.work.queued_after_stop = None;
        app.work.gateway_stop_pending = false;
        app.work.gateway_cmd_tx = None;
        app.work.stream_row_idx = None;
    });
}

#[gpui::test]
fn inline_error_failed_completion_event_stays_inline_through_live_drain_and_finalization(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_failed_completion_event_stays_inline_through_live_drain_and_finalization",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let owner = app.update(window, |app, cx| {
        let owner = fail(
            app,
            cx,
            "Earlier fixture outcome",
            "Partial output remains.",
        );
        app.work.panel = Default::default();
        app.work.error_presentation = None;
        app.work.running = true;
        app.work.stream_row_idx = Some(1);
        app.apply_work_update(
            crate::work::WorkUpdate::Event(awili_gateway::run_events::RunEvent::Completion(
                awili_gateway::run_events::CompletionEvent {
                    status: Some(awili_gateway::run_events::RunStatus::Failed),
                    output: Some("The backend failed before its terminal poll settled".into()),
                },
            )),
            cx,
        );
        assert!(
            app.current_work_error().is_some(),
            "real Completion error is visible before Terminal"
        );
        app.apply_work_update(
            crate::work::WorkUpdate::Token(" Final owned drain text.".into()),
            cx,
        );
        assert_eq!(
            app.workshop_error_row(),
            Some(1),
            "live drain may extend the same explicitly owned reply"
        );
        owner
    });
    window.run_until_parked();
    let card = window.debug_bounds("ui-card-work-error-card").unwrap();
    assert!(window
        .debug_bounds("awili-message-row-1")
        .unwrap()
        .contains(&card.center()));
    app.update(window, |app, cx| {
        app.work.running = false;
        app.work_finish_stream(cx);
        assert!(app.work.stream_row_idx.is_none());
        assert_eq!(
            app.workshop_error_row(),
            Some(1),
            "finalized fingerprint retains the settled owning reply"
        );
        assert!(app.views[&owner]
            .convo
            .message(1)
            .unwrap()
            .text
            .contains("Final owned drain text."));
    });
}

#[gpui::test]
fn inline_error_retry_does_not_replace_a_waiting_approval_with_a_new_job(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_retry_does_not_replace_a_waiting_approval_with_a_new_job",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let owner = app.update(window, |app, cx| {
        let owner = fail(
            app,
            cx,
            "Approval delivery was not confirmed",
            "Partial owning reply",
        );
        app.work.folder = Some(dir.path().into());
        app.work.executor = crate::work::WorkExecutor::Gateway;
        app.work.gateway.enabled = true;
        app.work.worker_identity = Some(crate::work_run_identity::WorkRunIdentity::HermesGateway);
        app.work.gateway_cmd_tx = Some(tx);
        app.work.panel.approval = Some(crate::work::ApprovalCard {
            approval_id: "unconfirmed-owned-approval".into(),
            kind: "fixture".into(),
            prompt: "Check the original decision before proceeding".into(),
            risk: awili_gateway::events::ApprovalRisk::Privileged,
            delivery: Default::default(),
        });
        app.composer
            .update(cx, |c, cx| c.set_text("Unsent staging", cx));
        cx.notify();
        owner
    });
    window.run_until_parked();
    click(window, "error-retry");
    window.update(|win, cx| app.update(cx, |app, cx| win.focus(&app.shell.error_focus, cx)));
    window.simulate_keystrokes("enter");
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();
    assert!(
        rx.try_recv().is_err(),
        "actual disabled pointer/key actions dispatch neither Retry nor a verdict"
    );
    app.update(window, |app, cx| {
        assert_eq!(app.work.conv_id.as_deref(), Some(owner.as_str()));
        assert_eq!(
            app.work.panel.approval.as_ref().unwrap().approval_id,
            "unconfirmed-owned-approval"
        );
        assert_eq!(app.views[&owner].convo.len(), 2);
        assert_eq!(app.composer.read(cx).text(), "Unsent staging");
        assert!(app.current_work_error().is_some());
        app.work.gateway_cmd_tx = None;
        app.work.panel.approval = None;
    });
}

#[gpui::test]
fn inline_error_restored_last_run_keeps_saved_cause_without_fabricating_an_action(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_restored_last_run_keeps_saved_cause_without_fabricating_an_action",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let cause =
        "The previous helper failed after partial output; its saved cause remains readable.";
    app.update(window, |app, cx| {
        let owner = app.create_conversation(awili_core::ConvKind::Work);
        let v = app.views.get_mut(&owner).unwrap();
        v.convo.push(crate::Role::User, "Saved original request");
        v.convo.push(crate::Role::Assistant, "Saved partial reply");
        v.rows = v
            .convo
            .messages()
            .iter()
            .map(crate::MessageRow::new)
            .collect();
        app.work_last_run.insert(
            owner.clone(),
            crate::work_last_run::LastRun {
                identity: crate::work_run_identity::WorkRunIdentity::OwnedEngine {
                    model: "saved-model".into(), engine_id: None,
                },
                model: Some("saved-model".into()),
                status: awili_gateway::run_events::RunStatus::Failed,
                error: Some(cause.into()),
                finished_at: "2026-10-13T00:00:00Z".into(),
            }
            .to_json(),
        );
        app.open_conversation_from_history(&owner, cx);
        assert!(
            app.current_work_error().is_none(),
            "restore does not manufacture a live request identity"
        );
        assert_eq!(app.work.panel.error.as_deref(), Some(cause));
        cx.notify();
    });
    window.run_until_parked();
    let card = window
        .debug_bounds("ui-card-work-error-card")
        .expect("saved cause retained in transcript");
    let row = window.debug_bounds("awili-message-row-1").unwrap();
    assert!(
        !row.contains(&card.center()),
        "no invented historical reply anchor"
    );
    assert!(window.debug_bounds("error-retry").is_none());
    assert!(window.debug_bounds("error-fix-folder").is_none());
    assert!(window.debug_bounds("ui-button-job-island-open").is_none());
    let full_cause = crate::ui::text::DRAWN_TEXTS.with(|texts| {
        texts.borrow().iter().any(|text| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (text.text(), text.bounds())
            }))
            .ok()
            .is_some_and(|(shown, bounds)| shown == cause && card.contains(&bounds.center()))
        })
    });
    assert!(
        full_cause,
        "complete saved cause painted in its owning fallback"
    );
}

#[gpui::test]
fn inline_error_retry_preserves_newer_request_held_by_local_notice(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "inline_error_tests::inline_error_retry_preserves_newer_request_held_by_local_notice",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let owner = app.update(window, |app, cx| {
        let owner = fail(app, cx, "Previous helper failed", "Partial reply");
        app.work.folder = Some(dir.path().into());
        app.work.executor = crate::work::WorkExecutor::InApp;
        app.work.engine.base_url.clear();
        app.guard_submit("Newer held request");
        app.local_gate_in("Newer held request".into(), owner.clone(), cx);
        app.settle_submit_guard(cx);
        assert_eq!(
            app.work.local.down.as_ref().unwrap().text.as_deref(),
            Some("Newer held request")
        );
        // Observe the existing direct Job retry without a network worker.
        app.work.executor = crate::work::WorkExecutor::Gateway;
        app.work.gateway.enabled = true;
        app.work.worker_identity = Some(crate::work_run_identity::WorkRunIdentity::HermesGateway);
        app.work.gateway_cmd_tx = Some(tx);
        app.composer
            .update(cx, |c, cx| c.set_text("Unsent staging", cx));
        cx.notify();
        owner
    });
    window.run_until_parked();
    click(window, "error-retry");
    window.update(|win, cx| app.update(cx, |app, cx| win.focus(&app.shell.error_focus, cx)));
    window.simulate_keystrokes("enter");
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();
    assert!(
        rx.try_recv().is_err(),
        "Retry cannot dispatch over the held newer request"
    );
    app.update(window, |app, cx| {
        assert_eq!(app.views[&owner].convo.len(), 2);
        assert_eq!(app.composer.read(cx).text(), "Unsent staging");
        let down = app.work.local.down.as_ref().unwrap().clone();
        assert_eq!(down.conv_id.as_deref(), Some(owner.as_str()));
        assert_eq!(down.text.as_deref(), Some("Newer held request"));
        assert!(app.current_work_error().is_some());
        assert!(app.work_error_action_busy());
        app.work.local.down.as_mut().unwrap().conv_id = None;
        assert!(
            app.work_error_action_busy(),
            "unspecified held owner is protected"
        );
        app.work.local.down.as_mut().unwrap().conv_id = Some("unrelated-owner".into());
        assert!(
            !app.work_error_action_busy(),
            "unrelated held owner does not block this action"
        );
        app.work.local.down = Some(crate::local_model_health::LocalDown { text: None, ..down });
        assert!(
            !app.work_error_action_busy(),
            "textless failure notice does not hold an intent"
        );
        app.work.gateway_cmd_tx = None;
        app.work.local.down = None;
    });
}

#[gpui::test]
fn inline_error_stop_replay_missing_folder_returns_prompt_to_its_own_draft(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_stop_replay_missing_folder_returns_prompt_to_its_own_draft",
    ) {
        return;
    }
    for folder_was_missing in [true, false] {
        for move_away in [false, true] {
            stop_replay_missing_folder_case(cx, folder_was_missing, move_away);
        }
    }
}

fn stop_replay_missing_folder_case(
    cx: &mut gpui::TestAppContext,
    folder_was_missing: bool,
    move_away: bool,
) {
    let (app, window) = setup(cx);
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("gone-before-replay");
    if !folder_was_missing {
        std::fs::create_dir(&folder).unwrap();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let owner = app.update(window, |app, cx| {
        fail(app, cx, "Draining failure", "Draining reply");
        app.work.gateway_stop_pending = true;
        app.work.stream_row_idx = Some(1);
        app.work.executor = crate::work::WorkExecutor::Gateway;
        app.work.gateway_cmd_tx = Some(tx);
        app.work.folder = Some(folder.clone());
        let requesting = app.create_conversation(awili_core::ConvKind::Work);
        app.set_active_conversation(&requesting);
        app.guard_submit("Parked request survives");
        app.submit_work_run_kind(
            "Parked request survives".into(),
            requesting.clone(),
            awili_core::MessageKind::Normal,
            cx,
        );
        app.settle_submit_guard(cx);
        assert!(app.submit_guard.is_none());
        assert_eq!(
            app.work.queued_after_stop.as_ref().unwrap().text,
            "Parked request survives"
        );
        requesting
    });
    if !folder_was_missing {
        std::fs::remove_dir(&folder).unwrap();
    }
    if move_away {
        let other = app.update(window, |app, _| {
            app.create_conversation(awili_core::ConvKind::Work)
        });
        window.update(|win, cx| app.update(cx, |app, cx| app.select_conversation(&other, win, cx)));
        window.run_until_parked();
    }
    app.update(window, |app, cx| {
        if move_away {
            app.composer
                .update(cx, |c, cx| c.set_text("Other owner's unsent draft", cx));
        }
        app.apply_work_update(
            crate::work::WorkUpdate::Terminal {
                status: awili_gateway::run_events::RunStatus::Stopped,
                output: None,
                model: None,
            },
            cx,
        );
        assert!(app.work.queued_after_stop.is_none());
        assert!(!app.work.gateway_stop_pending);
        assert!(app.views[&owner].convo.messages().is_empty());
        assert_eq!(
            app.composer.read(cx).text(),
            if move_away {
                "Other owner's unsent draft"
            } else {
                "Parked request survives"
            }
        );
        assert!(
            rx.try_recv().is_err(),
            "missing folder dispatches no new prompt"
        );
        app.work.gateway_cmd_tx = None;
    });
    window.update(|win, cx| app.update(cx, |app, cx| app.select_conversation(&owner, win, cx)));
    window.run_until_parked();
    assert_eq!(
        app.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        "Parked request survives"
    );
}

#[gpui::test]
fn inline_error_review_reveals_header_of_tall_fallback_without_an_action(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_review_reveals_header_of_tall_fallback_without_an_action",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let cause = "A complete owned failure explanation with retained detail.\n".repeat(100);
    let owner = app.update(window, |app, cx| {
        let owner = fail(app, cx, &cause, "Original failed reply");
        let v = app.views.get_mut(&owner).unwrap();
        v.truncate_to(1);
        v.convo.push(crate::Role::Assistant, "Replacement reply");
        v.rows
            .push(crate::MessageRow::new(v.convo.message(1).unwrap()));
        v.scroll.set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
        owner
    });
    window.run_until_parked();
    click(window, "ui-button-job-island-open");
    let card = window.debug_bounds("ui-card-work-error-card").unwrap();
    let viewport = window.debug_bounds("awili-transcript-viewport").unwrap();
    assert!(
        card.size.height > viewport.size.height,
        "genuine taller-than-viewport fallback"
    );
    assert!(
        card.top() >= viewport.top() - px(1.) && card.top() < viewport.bottom(),
        "Review reveals beginning of full scrollable cause: {card:?} / {viewport:?}"
    );
    assert!(window.debug_bounds("error-retry").is_none());
    assert!(window.update(|win, cx| app.read(cx).shell.error_focus.is_focused(win)));
    assert_eq!(
        app.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner
    );
}

#[gpui::test]
fn inline_error_admitted_folder_pick_returns_keyboard_to_the_new_composer(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::inline_error_admitted_folder_pick_returns_keyboard_to_the_new_composer",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    let dir = tempfile::tempdir().unwrap();
    let expected = app.update(window, |app, cx| {
        fail(
            app,
            cx,
            &crate::submit_guard::missing_folder_notice("gone"),
            "Owned old reply",
        );
        app.work.folder = Some(dir.path().join("gone"));
        app.composer.update(cx, |c, cx| {
            c.set_text("Keep the original owner's staging", cx)
        });
        cx.notify();
        app.current_work_error().unwrap().clone()
    });
    window.run_until_parked();
    click(window, "ui-button-job-island-open");
    assert!(window.update(|win, cx| app.read(cx).shell.error_focus.is_focused(win)));
    window.update(|win, cx| {
        app.update(cx, |app, cx| {
            assert!(!app.complete_error_folder_pick(&expected, None, win, cx));
            assert!(
                app.shell.error_focus.is_focused(win),
                "Cancel keeps the visible action focused"
            );
            app.work.gateway_stop_pending = true;
            assert!(!app.complete_error_folder_pick(&expected, Some(dir.path().into()), win, cx));
            assert!(
                app.shell.error_focus.is_focused(win),
                "Busy refusal preserves focus"
            );
            app.work.gateway_stop_pending = false;
            assert_eq!(
                app.composer.read(cx).text(),
                "Keep the original owner's staging"
            );
        })
    });
    let admitted = window.update(|win, cx| {
        app.update(cx, |app, cx| {
            app.complete_error_folder_pick(&expected, Some(dir.path().into()), win, cx)
        })
    });
    assert!(admitted);
    window.run_until_parked();
    assert!(app.read_with(window, |app, _| app.current_work_error().is_none()));
    window.simulate_keystrokes(&crate::composer_keys::plat_keys("cmd-j"));
    window.run_until_parked();
    assert!(
        app.read_with(window, |app, _| app.shell.job_open),
        "Job shortcut remains usable after admitted folder pick removes its focused ErrorCard"
    );
    assert!(window.debug_bounds("ui-card-work-error-card").is_none());
}

fn clipboard(window: &mut gpui::VisualTestContext) -> Option<String> {
    window.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

/// S3: the failing run's id is in the error card (Copy puts it on the
/// clipboard) and in Inspect reply for the same reply (with its own Copy).
#[gpui::test]
fn run_id_is_in_the_error_card_and_the_inspector(cx: &mut gpui::TestAppContext) {
    if !isolated("inline_error_tests::run_id_is_in_the_error_card_and_the_inspector") {
        return;
    }
    let (app, window) = setup(cx);
    app.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        app.work.graduation.arm(&owner);
        fail(app, cx, "Something unexpected broke.", "Partial output.");
    });
    window.run_until_parked();
    let id = app.read_with(window, |app, _| app.current_work_error().unwrap().run_id.clone());
    assert_eq!(id.len(), 8);
    assert_eq!(id, app.read_with(window, |app, _| app.work.graduation.run_id()));
    click(window, "error-run-id-copy");
    assert_eq!(clipboard(window).as_deref(), Some(id.as_str()));
    click(window, "inspect-reply-1");
    assert!(window.debug_bounds("ui-row-reply-inspector-run-id").is_some());
    app.update(window, |app, _| {
        let owner = app.sidebar.active_id().to_string();
        let ts = app.views[&owner].convo.message(1).unwrap().ts;
        assert_eq!(app.run_ids.get(&owner, ts), Some(id.as_str()));
    });
    window.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("x".into())));
    click(window, "ui-button-reply-copy-run-id");
    assert_eq!(clipboard(window).as_deref(), Some(id.as_str()));
}

/// S3: the primary action of each category does its thing.
#[gpui::test]
fn primary_action_dispatches_for_signed_out_local_setup_and_rate_limit(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_error_tests::primary_action_dispatches_for_signed_out_local_setup_and_rate_limit",
    ) {
        return;
    }
    let (app, window) = setup(cx);
    // Signed out: Sign in copies the command and tells the steps.
    app.update(window, |app, cx| {
        app.work.panel.requested_identity = Some(crate::work_run_identity::WorkRunIdentity::Agent {
            engine: awili_code::CodeEngine::Codex,
            model: None,
        });
        // The text names Claude; the run was on Codex, so Codex's command wins.
        fail(app, cx, "Claude: not logged in \u{b7} Please run /login", "Partial.");
    });
    window.run_until_parked();
    let title = app.read_with(window, |app, _| {
        crate::run_error::classify(&app.current_work_error().unwrap().cause).category
    });
    assert_eq!(title, crate::run_error::RunErrorCategory::SignedOut);
    click(window, "error-primary-action");
    assert_eq!(clipboard(window).as_deref(), Some("codex login"));
    // Rate limit: Try another model opens the helper menu.
    app.update(window, |app, cx| {
        app.work.panel.error = Some("rate_limit_error (429): slow down".into());
        app.work.error_presentation = None;
        app.capture_work_error(None);
        cx.notify();
    });
    window.run_until_parked();
    assert!(app.read_with(window, |app, _| app.shell.tabs.menu.is_none()));
    click(window, "error-primary-action");
    assert!(app.read_with(window, |app, _| {
        app.shell.tabs.menu == Some(crate::workshop::WorkshopMenu::Engine)
    }));
    app.update(window, |app, cx| {
        app.shell.tabs.menu = None;
        // Local setup: a missing helper program opens Settings.
        app.work.panel.error = Some("could not start `claude`".into());
        app.work.error_presentation = None;
        app.capture_work_error(None);
        cx.notify();
    });
    window.run_until_parked();
    assert!(!app.read_with(window, |app, _| app.settings.open));
    click(window, "error-primary-action");
    assert!(app.read_with(window, |app, _| app.settings.open));
}

/// S3: a failed Code reply gets a card with the category title, the cause,
/// the category's action and the run id with Copy; Sign in follows the
/// helper the tab runs, not words in the failure.
#[gpui::test]
fn code_tab_error_card_shows_title_cause_action_and_run_id(cx: &mut gpui::TestAppContext) {
    if !isolated("inline_error_tests::code_tab_error_card_shows_title_cause_action_and_run_id") {
        return;
    }
    let (app, window) = setup(cx);
    let tab_id = app.update(window, |app, cx| {
        app.set_mode(Mode::Code, cx);
        app.ensure_code_tab(cx);
        let active = app.work.code_active;
        let tab = &mut app.work.code_tabs[active];
        tab.engine = awili_code::CodeEngine::Codex;
        tab.convo.push(crate::Role::User, "Fix it");
        tab.rows.push(crate::MessageRow::new(tab.convo.message(0).unwrap()));
        tab.convo.push(crate::Role::Assistant, "\n\n\u{26a0} Not logged in \u{b7} Please run /login");
        tab.rows.push(crate::MessageRow::new(tab.convo.message(1).unwrap()));
        tab.errored.insert(1);
        let id = tab.id;
        app.code_run_errors.insert(
            (id, 1),
            ("Not logged in \u{b7} Please run /login".into(), "abcd1234".into()),
        );
        cx.notify();
        id
    });
    let _ = tab_id;
    window.run_until_parked();
    assert!(window.debug_bounds("ui-card-code-error-card-1").is_some(), "card");
    click(window, "code-run-id-copy");
    assert_eq!(clipboard(window).as_deref(), Some("abcd1234"));
    // The failure text says "login" and nothing about Claude; the tab runs
    // Codex, so Sign in copies Codex's command.
    click(window, "code-error-action");
    assert_eq!(clipboard(window).as_deref(), Some("codex login"));
}
