//! Actual conversation placement, owner Review focus, kit bounds and actions.
use crate::{
    consolidation_tests::isolated,
    work::{ApprovalCard, Mode},
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
        app.work.conv_id = Some(app.sidebar.active_id().into());
        // A live blocked run retains its owner while another Work thread opens.
        app.work.running = true;
        app.work.executor = crate::work::WorkExecutor::InApp;
        app.work.worker_identity = Some(crate::work_run_identity::WorkRunIdentity::OwnedEngine {
            model: "fixture".into(), engine_id: None,
        });
        let active = app.views.get_mut(app.sidebar.active_id()).unwrap();
        let idx = active.convo.len();
        active
            .convo
            .push(crate::Role::Assistant, "This reply owns the gate.");
        active
            .rows
            .push(crate::MessageRow::new(active.convo.message(idx).unwrap()));
        app.work.stream_row_idx = Some(idx);
        cx.notify();
    });
    window.simulate_resize(size(px(860.), px(700.)));
    window.run_until_parked();
    (view, window)
}
fn card(prompt: &str) -> ApprovalCard {
    ApprovalCard {
        approval_id: "owned-fixture".into(),
        kind: "fixture".into(),
        prompt: prompt.into(),
        risk: awili_gateway::events::ApprovalRisk::Privileged,
        delivery: Default::default(),
    }
}
fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let b = window.debug_bounds(selector).expect(selector);
    window.simulate_click(b.center(), gpui::Modifiers::default());
    window.run_until_parked();
}
#[gpui::test]
fn inline_blocking_review_returns_to_owner_and_enter_answers_the_actual_card(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("inline_blocking_tests::inline_blocking_review_returns_to_owner_and_enter_answers_the_actual_card") { return; }
    let (view, window) = setup(cx);
    let (tx, rx) = std::sync::mpsc::channel();
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    // The wide Job sidebar keeps the island reachable while open. Compact
    // sheets own their scrim, so a background click first dismisses the sheet.
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        app.work.gateway_cmd_tx = Some(tx);
        app.work.panel.approval = Some(card("Complete owning approval"));
        let other = app.create_conversation(awili_core::ConvKind::Work);
        app.set_active_conversation(&other);
        app.set_mode(Mode::Work, cx);
        app.shell.job_open = true;
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("work-allow-once").is_none(),
        "another conversation cannot show the gate"
    );
    click(window, "ui-button-job-island-open");
    assert_eq!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_string()),
        owner
    );
    assert!(!view.read_with(window, |app, _| app.shell.job_open));
    assert!(
        window.update(|win, cx| view.read(cx).shell.approval_focus.is_focused(win)),
        "Review focuses the actual first action"
    );
    let message = window
        .debug_bounds("awili-message-row-0")
        .expect("owning reply");
    let gate = window
        .debug_bounds("ui-card-work-approval-card")
        .expect("kit gate");
    assert!(
        message.contains(&gate.center()),
        "gate belongs inside its reply, not the composer"
    );
    window.simulate_keystrokes("enter");
    // The pinned GPUI activates a focused button on the platform key-up.
    // simulate_keystrokes dispatches only key-down/action matching.
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();
    assert!(
        matches!(rx.try_recv(), Ok(crate::work::GatewayCmd::Approval { id, choice }) if id.as_deref() == Some("owned-fixture") && choice == "once"),
        "keyboard activates production verdict handler"
    );
    assert!(rx.try_recv().is_err(), "only one verdict");
    assert!(window.debug_bounds("work-allow-once").is_none());
    let record = window
        .debug_bounds("work-decided-card")
        .expect("decided record");
    let message = window.debug_bounds("awili-message-row-0").unwrap();
    assert!(
        message.contains(&record.center()),
        "record retains the owning reply"
    );
    view.update(window, |app, cx| {
        app.work.running = false;
        app.work_finish_stream(cx);
        cx.notify();
    });
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.work.stream_row_idx.is_none()));
    assert!(
        window.debug_bounds("work-decided-card").is_none(),
        "completed approval controls collapse out of the reading surface"
    );
    click(window, "reply-activity-toggle");
    let record = window.debug_bounds("work-decided-card").unwrap();
    assert!(
        window
            .debug_bounds("awili-message-row-0")
            .unwrap()
            .contains(&record.center()),
        "completion preserves the decided card inside its owning reply"
    );
    let other = window.update(|win, cx| {
        view.update(cx, |app, cx| {
            let other = app.create_conversation(awili_core::ConvKind::Work);
            app.select_conversation(&other, win, cx);
            other
        })
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("work-decided-card").is_none(),
        "finished record remains owner-only in {other}"
    );
    window.update(|win, cx| view.update(cx, |app, cx| app.select_conversation(&owner, win, cx)));
    window.run_until_parked();
    let record = window.debug_bounds("work-decided-card").unwrap();
    assert!(
        window
            .debug_bounds("awili-message-row-0")
            .unwrap()
            .contains(&record.center()),
        "return preserves the original decided reply"
    );
    view.update(window, |app, cx| {
        app.work.panel.decided.as_mut().unwrap().choice = "deny";
        let row = &mut app.views.get_mut(&owner).unwrap().rows[0];
        row.activity.clear();
        row.presentation_activity.clear();
        row.activity_expanded = false;
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("work-decided-card").is_some(),
        "a denied record with no Activity toggle remains reachable"
    );
}

#[gpui::test]
fn inline_blocking_each_scope_button_sends_its_exact_existing_verdict(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "inline_blocking_tests::inline_blocking_each_scope_button_sends_its_exact_existing_verdict",
    ) {
        return;
    }
    let (view, window) = setup(cx);
    let folder = tempfile::tempdir().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, _| {
        app.work.gateway_cmd_tx = Some(tx);
        app.work.folder = Some(folder.path().into());
    });
    for (selector, expected) in [
        ("work-allow-once", "once"),
        ("work-allow-session", "session"),
        ("work-allow-folder", "folder"),
        ("work-allow-always", "always"),
        ("work-deny", "deny-stop"),
    ] {
        view.update(window, |app, cx| {
            let mut gate = card("A fixture action with every offered scope.");
            gate.risk = awili_gateway::events::ApprovalRisk::Mutating;
            app.work.panel.approval = Some(gate);
            app.work.panel.decided = None;
            cx.notify();
        });
        window.run_until_parked();
        click(window, "ui-button-job-island-open");
        click(window, selector);
        assert!(
            matches!(rx.try_recv(), Ok(crate::work::GatewayCmd::Approval { id, choice }) if id.as_deref() == Some("owned-fixture") && choice == expected),
            "{selector} sends its exact offered scope and original ID"
        );
        assert!(rx.try_recv().is_err(), "one verdict per button");
    }
}

#[gpui::test]
fn inline_blocking_credential_send_uses_and_drains_the_existing_masked_editor(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("inline_blocking_tests::inline_blocking_credential_send_uses_and_drains_the_existing_masked_editor") { return; }
    let (view, window) = setup(cx);
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.work.gateway_cmd_tx = Some(tx);
        app.work.panel.credential = Some(crate::work::CredentialPrompt {
            request_id: "dummy-send".into(),
            kind: crate::work::CredentialKind::Secret,
            env_var: "FIXTURE_VALUE".into(),
            prompt: "A dummy fixture value only.".into(),
        });
        cx.notify();
    });
    window.run_until_parked();
    click(window, "ui-button-job-island-open");
    window.write_to_clipboard(gpui::ClipboardItem::new_string("fixture-only-value".into()));
    window.simulate_keystrokes("cmd-v");
    window.run_until_parked();
    crate::ui::text::DRAWN_TEXTS.with(|lines| {
        assert!(
            !lines.borrow().iter().any(|line| {
                crate::ui::text::measured_text(line)
                    .is_some_and(|text| text.contains("fixture-only-value"))
            }),
            "dummy value is never painted as readable text"
        )
    });
    click(window, "work-credential-submit");
    assert!(
        matches!(rx.try_recv(), Ok(crate::work::GatewayCmd::SecretRespond { request_id, value }) if request_id == "dummy-send" && value == "fixture-only-value"),
        "Send routes the original dummy request and editor value"
    );
    assert!(rx.try_recv().is_err());
    assert!(
        view.read_with(window, |app, cx| app.work.panel.credential.is_none()
            && app.credential_input.read(cx).text().is_empty())
    );
}

#[gpui::test]
fn inline_blocking_review_reveals_actions_and_editor_after_tall_activity(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("inline_blocking_tests::inline_blocking_review_reveals_actions_and_editor_after_tall_activity") { return; }
    let (view, window) = setup(cx);
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    view.update(window, |app, cx| {
        for n in 0..90 {
            app.work_on_activity(format!("Completed real Work activity step {n}"), cx);
        }
        app.toggle_row_activity(&owner, 0, cx);
        app.work.panel.approval = Some(card("A live gate after a tall activity history."));
        cx.notify();
    });
    for scale in [0.9, 1.25] {
        crate::theme::set_ui_scale(scale);
        for credential in [false, true] {
            view.update(window, |app, cx| {
                app.work.panel.approval =
                    (!credential).then(|| card("A live gate after a tall activity history."));
                app.work.panel.credential = credential.then(|| crate::work::CredentialPrompt {
                    request_id: "tall-history-credential".into(),
                    kind: crate::work::CredentialKind::Secret,
                    env_var: "FIXTURE_VALUE".into(),
                    prompt: "An owned dummy value after tall history.".into(),
                });
                cx.notify();
            });
            window.run_until_parked();
            click(window, "ui-button-job-island-open");
            let viewport = window.debug_bounds("awili-transcript-viewport").unwrap();
            let selector = if credential {
                "ui-text-field-work-credential-input-frame"
            } else {
                "work-allow-once"
            };
            let target = window.debug_bounds(selector).unwrap();
            assert!(target.origin.y >= viewport.origin.y && target.origin.y + target.size.height <= viewport.origin.y + viewport.size.height,
                "Review reveals the actual control after tall history: {selector} at {scale}: {target:?} / {viewport:?}");
        }
    }
    crate::theme::set_ui_scale(1.0);
}
#[gpui::test]
fn inline_blocking_full_prompt_and_disabled_scopes_keep_kit_targets(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "inline_blocking_tests::inline_blocking_full_prompt_and_disabled_scopes_keep_kit_targets",
    ) {
        return;
    }
    let (view, window) = setup(cx);
    let (tx, rx) = std::sync::mpsc::channel();
    let prompt =
        "Full Unicode café approval prompt with a complete final sentence RETAINED. ".repeat(5);
    view.update(window, |app, cx| {
        app.work.gateway_cmd_tx = Some(tx);
        app.work.panel.approval = Some(card(&prompt));
        cx.notify();
    });
    for scale in [0.9, 1.25] {
        crate::theme::set_ui_scale(scale);
        window.update(|win, cx| {
            view.update(cx, |app, cx| {
                app.focus_work_blocking_card(win, cx);
                cx.notify();
            })
        });
        window.run_until_parked();
        for selector in [
            "work-allow-once",
            "work-deny",
            "work-allow-session",
            "work-allow-folder",
            "work-allow-always",
        ] {
            let b = window.debug_bounds(selector).expect(selector);
            assert!(
                b.size.width >= px(44.) && b.size.height >= px(44.),
                "{selector} at {scale}: {b:?}"
            );
        }
        crate::ui::text::DRAWN_TEXTS.with(|lines| {
            assert!(
                lines
                    .borrow()
                    .iter()
                    .any(|line| crate::ui::text::measured_text(line).as_deref()
                        == Some(prompt.as_str())),
                "the complete owned prompt was painted"
            )
        });
        click(window, "work-allow-always");
        click(window, "work-allow-folder");
        assert!(rx.try_recv().is_err(), "unavailable scopes remain inert");
    }
    crate::theme::set_ui_scale(1.0);
}
#[gpui::test]
fn inline_blocking_credential_review_focuses_the_masked_editor_and_decline_unblocks(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("inline_blocking_tests::inline_blocking_credential_review_focuses_the_masked_editor_and_decline_unblocks") {return;}
    let (view, window) = setup(cx);
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.work.gateway_cmd_tx = Some(tx);
        app.work.panel.credential = Some(crate::work::CredentialPrompt {
            request_id: "dummy-credential".into(),
            kind: crate::work::CredentialKind::Secret,
            env_var: "FIXTURE_VALUE".into(),
            prompt: "Owned dummy credential request".into(),
        });
        cx.notify();
    });
    window.run_until_parked();
    click(window, "ui-button-job-island-open");
    assert!(
        window.update(|win, cx| view
            .read(cx)
            .credential_input
            .read(cx)
            .focus_handle()
            .is_focused(win)),
        "Review focuses existing editor"
    );
    let gate = window.debug_bounds("ui-card-work-credential-card").unwrap();
    assert!(window
        .debug_bounds("awili-message-row-0")
        .unwrap()
        .contains(&gate.center()));
    assert!(window
        .debug_bounds("ui-text-field-work-credential-input-frame")
        .is_some());
    click(window, "work-credential-refuse");
    assert!(
        matches!(
            rx.try_recv(),
            Ok(crate::work::GatewayCmd::SecretRespond { request_id, value })
                if request_id == "dummy-credential" && value.is_empty()
        ),
        "existing refusal goes to waiting engine"
    );
    assert!(view.read_with(window, |app, _| app.work.panel.credential.is_none()));
}

#[gpui::test]
fn inline_blocking_clarify_review_returns_to_composer_without_verdict_controls(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("inline_blocking_tests::inline_blocking_clarify_review_returns_to_composer_without_verdict_controls") {return;}
    let (view, window) = setup(cx);
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.work.gateway_cmd_tx = Some(tx);
        let mut question = card("Which owned fixture should be used?");
        question.kind = crate::work::CLARIFY_KIND.into();
        app.work.panel.approval = Some(question);
        cx.notify();
    });
    window.run_until_parked();
    click(window, "ui-button-job-island-open");
    assert!(window.update(|win, cx| view
        .read(cx)
        .composer
        .read(cx)
        .focus_handle()
        .is_focused(win)));
    assert!(window.debug_bounds("work-allow-once").is_none());
    assert!(window.debug_bounds("work-deny").is_none());
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("The owned dummy fixture", cx));
        app.submit_composer_draft(cx);
    });
    window.run_until_parked();
    assert!(
        matches!(rx.try_recv(),Ok(crate::work::GatewayCmd::Clarify{answer,..}) if answer=="The owned dummy fixture"),
        "production composer answers the clarification"
    );
    assert!(rx.try_recv().is_err());
    assert!(view.read_with(window, |app, _| app.work.panel.approval.is_none()));
}

#[gpui::test]
fn inline_blocking_completed_reply_has_no_blank_scroll_tail(cx: &mut gpui::TestAppContext) {
    if !isolated("inline_blocking_tests::inline_blocking_completed_reply_has_no_blank_scroll_tail")
    {
        return;
    }
    let (view, window) = setup(cx);
    crate::theme::set_ui_scale(1.4);
    window.simulate_resize(size(px(1250.), px(777.)));
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.work.gateway_cmd_tx = Some(tx);
        app.work.panel.approval = Some(card("Run mac-shell.run_command: printf AWILI_RC_SHELL_OK"));
        let active = app.views.get_mut(app.sidebar.active_id()).unwrap();
        active.convo = crate::ConversationState::new();
        active.rows.clear();
        for (role, text) in [
            (crate::Role::User, "RC acceptance check: run the harmless shell command printf AWILI_RC_SHELL_OK and report its actual output. Do not change files or save memory notes."),
            (crate::Role::Assistant, "\n> **⚠ Approval needed:** Run \"mac-shell.run_command\" — \"command\": \"printf AWILI_RC_SHELL_OK\"\n"),
        ] {
            let idx = active.convo.len();
            active.convo.push(role, text);
            active.rows.push(crate::MessageRow::new(active.convo.message(idx).unwrap()));
        }
        app.work.stream_row_idx = Some(1);
        cx.notify();
    });
    window.run_until_parked();
    click(window, "ui-button-job-island-open");
    click(window, "work-allow-once");
    assert!(
        matches!(rx.try_recv(), Ok(crate::work::GatewayCmd::Approval { id, choice }) if id.as_deref() == Some("owned-fixture") && choice == "once")
    );
    view.update(window, |app, cx| {
        app.work_stream_token("AWILI_RC_SHELL_OK", cx);
        app.work.running = false;
        app.work_finish_stream(cx);
        app.views.get_mut(app.sidebar.active_id()).unwrap().rows[1].footer = Some("Codex · gpt-6.1-sol · 1m 06s · 13,023 tokens · this turn: 12,345 input + 678 output · 10,000 cached (included in input) · no files changed".into());
        crate::transcript_keys::pin_to_bottom(&app.active_view().unwrap().scroll);
        cx.notify();
    });
    window.run_until_parked();
    // Geometry is exercised with production-shaped footer text. Attachment
    // policy and persistence have their own run_footer tests.
    let base = view.read_with(window, |app, _| {
        app.active_view().unwrap().rows[1].footer.clone().unwrap()
    });
    let with_folder = crate::work_folder_moves::footer_with_folder(
        base.clone(),
        Some("/Projects/Fixture Work/Released Copy"),
    );
    for footer in [base, with_folder] {
        for expanded in [false, true] {
            // Exercise the same handler as the real Activity button, including
            // its notification and the list's subsequent remeasurement.
            view.update(window, |app, cx| {
                let owner = app.sidebar.active_id().to_string();
                if app.views[&owner].rows[1].activity_expanded != expanded {
                    app.toggle_row_activity(&owner, 1, cx);
                }
            });
            for (scale, width, height) in [
                (1.4, 1250., 777.),
                (1., 900., 900.),
                (1.25, 1400., 900.),
                (1.4, 600., 1000.),
            ] {
                crate::theme::set_ui_scale(scale);
                window.simulate_resize(size(px(width), px(height)));
                view.update(window, |app, cx| {
                    app.views.get_mut(app.sidebar.active_id()).unwrap().rows[1].footer =
                        Some(footer.clone());
                    let scroll = &app.active_view().unwrap().scroll;
                    crate::transcript_keys::pin_to_bottom(scroll);
                    scroll.scroll_to_bottom();
                    cx.notify();
                });
                crate::run_footer::DRAWN_FOOTERS.with(|d| d.borrow_mut().clear());
                window.update(|win, cx| win.draw(cx).clear());
                window.run_until_parked();
                let viewport = window.debug_bounds("awili-transcript-viewport").unwrap();
                let scroll =
                    view.read_with(window, |app, _| app.active_view().unwrap().scroll.clone());
                let measured = scroll.bounds_for_item(0).unwrap();
                let last = if expanded {
                    window.debug_bounds("work-decided-card").unwrap()
                } else {
                    assert!(window.debug_bounds("work-decided-card").is_none());
                    window.debug_bounds("inspect-reply-1").unwrap()
                };
                let reply = window.debug_bounds("awili-message-row-1").unwrap();
                let internal_tail = reply.bottom() - last.bottom();
                assert!(internal_tail >= px(-1.) && internal_tail <= px(1.), "row must end at its last child (no measured blank tail) at {scale}/{width}: {reply:?} / {last:?}");
                // The reading clamp includes both SP_24 gutters. This directly
                // rejects the discarded row-w_full overflow, without a selector
                // that could report the wrapper while its row escaped it.
                assert!(
                    reply.size.width
                        <= crate::theme::z(crate::theme::READING_MAX - 2. * crate::theme::SP_24)
                            + px(1.),
                    "reply exceeds reading width at {scale}/{width}: {reply:?}"
                );
                assert!(
                    reply.left() >= viewport.left() && reply.right() <= viewport.right() + px(1.),
                    "reply must fit viewport horizontally: {reply:?} / {viewport:?}"
                );
                let left = reply.left() - viewport.left();
                let right = viewport.right() - reply.right();
                assert!(
                    (left - right).abs() <= px(1.),
                    "reading column stays centered at {scale}/{width}: {left:?} / {right:?}"
                );
                assert!(
                last.bottom() > viewport.top() && last.bottom() <= viewport.bottom(),
                "last control must remain visible at {scale}/{width}/{expanded}: {last:?} / {viewport:?}; content={measured:?}; scroll={:?}/{:?}", scroll.offset(), scroll.max_offset()
            );
                let gap = viewport.bottom() - reply.bottom();
                assert!(
                    gap >= px(-1.),
                    "the latest control remains inside the viewport"
                );
                if scroll.max_offset().y > px(1.) {
                    assert!(gap <= crate::theme::z(60.),
                        "overflowing content ends at the real last child at {scale}/{width}/{expanded}: {reply:?} / {viewport:?}");
                } else {
                    // A short completed answer underfills the viewport. Empty
                    // canvas is expected; a stale negative offset is not.
                    assert!(
                        scroll.offset().y.abs() <= px(1.),
                        "underfilled transcript has no stale scroll offset: {:?}",
                        scroll.offset()
                    );
                }
                let drawn =
                    crate::run_footer::DRAWN_FOOTERS.with(|d| d.borrow().last().unwrap().clone());
                assert_eq!(
                    drawn.iter().map(|layout| layout.text()).collect::<Vec<_>>().join(" · "),
                    footer.split(" · ").flat_map(crate::run_footer::compact_metric).collect::<Vec<_>>().join(" · "),
                    "complete footer, including folder suffix"
                );
                let actual_metrics: Vec<_> = drawn.iter().map(|layout| layout.text()).collect();
                for expected in ["In 12,345", "Out 678", "Cached 10,000"] {
                    assert!(actual_metrics.iter().any(|metric| metric == expected), "{actual_metrics:?}");
                }
                assert!(!actual_metrics.iter().any(|metric| metric == ")"));
                let bounds = drawn.iter().map(|layout| layout.bounds()).reduce(|a, b| a.union(&b)).unwrap();
                assert!(
                    bounds.left() >= reply.left() && bounds.right() <= reply.right() + px(1.),
                    "footer must fit horizontally: {bounds:?} / {reply:?}"
                );
                if width == 600. {
                    assert!(
                        bounds.size.height > crate::theme::z(crate::theme::LABEL_SIZE * 2.),
                        "narrow footer wraps, rather than clipping: {bounds:?}"
                    );
                }
            }
        }
    }
    crate::theme::set_ui_scale(1.);
}
