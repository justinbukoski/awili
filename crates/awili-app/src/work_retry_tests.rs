//! M2g proof: Work-mode Retry (F09/F12) through the real Work path — a
//! gateway turn gets re-sent on the executor the chip showed, and Up recalls
//! the last sent prompt in the main composer. The harness is
//! `gateway_queue_tests`'s isolated-app plus fake-gateway helpers, driven
//! exactly like `submit_during_pending_stop_is_queued_then_runs`.

use super::gateway_queue_tests::{isolated, pump, transcript, wire_gateway, CONV};
use super::gateway_turn_tests::{complete, delta, running};
use super::tests::FakeOps;
use super::*;

/// M2g: the Work-mode Retry chip re-sends the user row to the executor the
/// chip was showing at click time. The new user row is appended (history is
/// never rewritten) and the run goes to `self.work.executor` at click time —
/// so a mid-run executor change is honored by the next click.
#[gpui::test]
fn m2g_work_retry_resends_on_the_current_executor(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::retry_tests::m2g_work_retry_resends_on_the_current_executor")
    else {
        return;
    };
    let fake = FakeOps::new(vec![
        vec![
            running(true),
            delta("first answer"),
            complete(),
            running(false),
        ],
        vec![
            running(true),
            delta("retried answer"),
            complete(),
            running(false),
        ],
    ]);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Work;
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        // First turn: user asks "q", gateway answers.
        app.submit_work_run("q".into(), cx);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        // The first turn leaves at least a user row and an assistant row.
        let rows_before_retry = app.views[CONV].convo.messages().len();
        assert_eq!(
            fake.ops()
                .iter()
                .filter(|s| s.starts_with("prompt_submit"))
                .count(),
            1,
            "exactly one gateway prompt_submit before the retry"
        );

        // The user row is at index 0, the assistant row at index 1. The
        // Retry chip (rendered for idx=1) re-sends the user row at 0.
        app.retry_work(rows_before_retry - 1, cx);
        let rows_after_retry = app.views[CONV].convo.messages().len();
        // `retry_text` reads the user row immediately preceding the *last*
        // assistant row, so the last row must be an assistant row with a user
        // row before it. The scripted turn leaves the transcript in exactly
        // that shape after the turn completes.
        assert!(
            rows_after_retry > rows_before_retry,
            "retry appends a fresh user row; history is never rewritten \
             (before={rows_before_retry}, after={rows_after_retry})"
        );
        // The new user row carries the original prompt text. The user rows
        // are indexed by role (not by the trailing user row), so the
        // iteration is a forward scan — not the `filter(...).last()` that
        // `clippy::double_ended_iterator_last` would flag.
        let user_rows: Vec<_> = app.views[CONV]
            .convo
            .messages()
            .iter()
            .filter(|m| m.role == crate::Role::User)
            .collect();
        let last_user = user_rows
            .last()
            .expect("at least the original user row and the retry's user row");
        assert_eq!(
            last_user.text.trim(),
            "q",
            "the retried user row carries the original prompt text"
        );
        // A second `prompt_submit` reaches the gateway. The retry's dispatch
        // runs on the test thread while the worker thread is busy pumping the
        // first turn's scripted events; the cmd_tx hand-off is synchronous,
        // but `ops()` only records once the worker's `prompt_submit` call
        // lands. Pump the second turn (the retry's) to completion so the
        // worker processes the `Prompt` cmd and the `prompt_submit` record
        // lands; the assertion follows.
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        assert_eq!(
            fake.ops()
                .iter()
                .filter(|s| s.starts_with("prompt_submit"))
                .count(),
            2,
            "the retry reaches the gateway as a second prompt_submit"
        );
        // M2g-FIX1 (T2 P2): the exact four-row transcript, and the prompt
        // text in BOTH gateway submits.
        assert_eq!(
            transcript(app),
            vec![
                (crate::Role::User, "q".to_string()),
                (crate::Role::Assistant, "first answer".to_string()),
                (crate::Role::User, "q".to_string()),
                (crate::Role::Assistant, "retried answer".to_string()),
            ],
            "retry appends one user row and one reply; nothing is rewritten"
        );
        assert_eq!(
            submits(&fake),
            vec!["prompt_submit(s1,q)", "prompt_submit(s1,q)"],
            "both gateway submits carry the user's prompt text"
        );

        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

/// M2-SLICE (T2 P2): a Retry click carries the conversation it was painted
/// in. Handled after the user moved to another conversation, or for a row
/// that is not the last assistant row, it sends nothing; the same click in
/// its own conversation still re-sends.
#[gpui::test]
fn m2_slice_retry_click_is_pinned_to_its_conversation(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::retry_tests::m2_slice_retry_click_is_pinned_to_its_conversation")
    else {
        return;
    };
    let fake = FakeOps::new(vec![
        vec![
            running(true),
            delta("first answer"),
            complete(),
            running(false),
        ],
        vec![
            running(true),
            delta("retried answer"),
            complete(),
            running(false),
        ],
    ]);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Work;
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.views.insert(
            "other-conv".into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("q".into(), cx);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        let last = app.views[CONV].convo.messages().len() - 1;
        assert_eq!(submits(&fake).len(), 1);

        // The chip was painted in CONV; the user then moved to another
        // conversation before the click was handled.
        app.set_active_conversation("other-conv");
        app.retry_work_in(CONV.into(), last, cx);
        assert_eq!(
            app.views[CONV].convo.messages().len(),
            last + 1,
            "nothing appended"
        );
        assert!(
            app.views["other-conv"].convo.messages().is_empty(),
            "nothing sent there"
        );
        assert!(!app.work.running, "no run started");

        // Back in CONV: a click for a row that is not the last assistant row
        // is a no-op too.
        app.set_active_conversation(CONV);
        app.retry_work_in(CONV.into(), 0, cx);
        assert_eq!(
            app.views[CONV].convo.messages().len(),
            last + 1,
            "stale row: nothing appended"
        );
        assert!(!app.work.running, "stale row: no run started");

        // The real click in its own conversation still re-sends once.
        app.retry_work_in(CONV.into(), last, cx);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        assert_eq!(
            submits(&fake),
            vec!["prompt_submit(s1,q)", "prompt_submit(s1,q)"],
            "exactly one re-send, from the conversation the chip was painted in"
        );

        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

/// M2g: Up in the main composer recalls the last sent prompt — the walk is
/// live over the session `sent` list and the active conversation's user
/// messages, de-duplicated. A diverged draft (an edit that breaks the
/// equality with `recalled`) ends the walk: the next Up moves the caret
/// instead of stepping the cursor.
#[gpui::test]
fn m2g_up_recalls_the_last_sent_prompt(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::retry_tests::m2g_up_recalls_the_last_sent_prompt") else {
        return;
    };
    let _ = home; // isolation only; no gateway needed for this test
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        // Onboarding owns the turn pipeline — never start a walk mid-onboarding.
        app.onboarding = crate::onboarding::Onboarding::existing();

        // Seed the active Work view with one user + one assistant turn so the
        // conversation tail is non-empty.
        let mut state = awili_core::ConversationState::new();
        state.push(crate::Role::User, "original prompt");
        state.push(crate::Role::Assistant, "first answer");
        app.views
            .insert(CONV.into(), crate::conv_view::ConvView::from_state(state));
        app.set_active_conversation(CONV);
        app.mode = Mode::Work;

        // The composer is already history-enabled by `AwiliApp::new` (the
        // `enable_history()` call right after `cx.new(Composer::new)`).
        // Record two prompts in the session walk: the first is in the convo,
        // the second is a fresh prompt (not yet sent through the convo).
        app.prompt_history.record("original prompt");
        app.prompt_history.record("second prompt");

        // Empty draft: Up fires HistoryPrev. The recalled entry is the
        // newest `sent` entry ("second prompt").
        let got = app.composer.update(_cx, |c, _cx| {
            crate::composer::composer_history_key_direction(c, "up")
        });
        assert!(
            matches!(got, Some(true)),
            "Up with an empty draft must fire HistoryPrev, got {got:?}"
        );
        // The owning view writes the recalled entry into the composer.
        app.composer_history_prev(_cx);
        let draft = app.composer.update(_cx, |c, _cx| c.draft());
        assert_eq!(
            draft, "second prompt",
            "the recalled entry lands in the draft (the session's newest sent prompt)"
        );
        // A second Up steps older to the conversation tail ("original
        // prompt").
        let got2 = app.composer.update(_cx, |c, _cx| {
            crate::composer::composer_history_key_direction(c, "up")
        });
        assert!(
            matches!(got2, Some(true)),
            "Up on a matching draft must fire HistoryPrev again, got {got2:?}"
        );
        app.composer_history_prev(_cx);
        let draft2 = app.composer.update(_cx, |c, _cx| c.draft());
        assert_eq!(
            draft2, "original prompt",
            "the second Up recalls the conversation's user message"
        );

        // A diverged draft (an edit) ends the walk: the next Up moves the
        // caret (no emit) instead of stepping the cursor.
        app.composer.update(_cx, |c, cx| c.set_text("edited", cx));
        let got3 = app.composer.update(_cx, |c, _cx| {
            crate::composer::composer_history_key_direction(c, "up")
        });
        assert!(
            got3.is_none(),
            "a diverged draft ends the walk — Up moves the caret, got {got3:?}"
        );
    });
}

// ---- M2g-FIX1 (T2 review of 30f18a2) -----------------------------------------

/// The gateway's `prompt_submit` calls, in order.
fn submits(fake: &FakeOps) -> Vec<String> {
    fake.ops()
        .into_iter()
        .filter(|s| s.starts_with("prompt_submit"))
        .collect()
}

/// Paints the real transcript (`message_list`) into a test window, so a test
/// clicks the Retry chip that was actually painted. Same pattern as
/// `work::tests::readonly_connector_detail_renders_label_without_plan`.
struct TranscriptHarness {
    app: gpui::Entity<AwiliApp>,
}

impl gpui::Render for TranscriptHarness {
    fn render(
        &mut self,
        _w: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let list = self
            .app
            .update(cx, |app, cx| app.message_list(cx).into_any_element());
        div().size_full().flex().flex_col().child(list)
    }
}

/// Paints the app's MAIN composer into a test window, so keys reach it
/// through the window's real key dispatch (see `press_enter`).
struct ComposerHarness {
    app: gpui::Entity<AwiliApp>,
}

impl gpui::Render for ComposerHarness {
    fn render(
        &mut self,
        _w: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        div().size_full().child(self.app.read(cx).composer.clone())
    }
}

/// Click the centre of the painted element tagged `selector`.
fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = window
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not painted"));
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
}

/// Press Enter through the window's key dispatch to the focused composer.
/// Not `simulate_keystrokes("enter")`: the test platform then also types its
/// `key_char` "\n" into the input handler for any Enter the element does not
/// stop, and the composer never stops Enter (the idle Enter-to-send does not
/// either, and the macOS app leaves no "\n" behind after a send).
fn press_enter(window: &mut gpui::VisualTestContext) {
    window.simulate_event(gpui::KeyDownEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    window.run_until_parked();
}

/// M2g-FIX1 (T2 P1 + P2): the Retry chip of the LAST assistant row is painted
/// and clickable with no row hovered. A click re-sends the user's prompt on
/// the executor the chip shows at click time: first the gateway, then, after
/// the executor changes, an agent harness.
#[gpui::test]
fn m2g_fix1_rendered_retry_needs_no_hover_and_follows_the_executor(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::retry_tests::m2g_fix1_rendered_retry_needs_no_hover_and_follows_the_executor",
    ) else {
        return;
    };
    // The agent retry starts a CLI thread (a missing binary) that may report.
    cx.executor().allow_parking();
    let fake = FakeOps::new(vec![
        vec![
            running(true),
            delta("first answer"),
            complete(),
            running(false),
        ],
        vec![
            running(true),
            delta("retried answer"),
            complete(),
            running(false),
        ],
    ]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        app.mode = Mode::Work;
        let wired = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("q".into(), cx);
        pump(
            app,
            &wired.0,
            wired.2,
            |e| *e == WorkEvent::TurnComplete,
            cx,
        );
        app.hovered_row = None;
        wired
    });
    let (_v, window) = cx.add_window_view(|_, cx| {
        cx.observe(&entity, |_, _, cx| cx.notify()).detach();
        TranscriptHarness {
            app: entity.clone(),
        }
    });
    window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(1000.)));
    window.run_until_parked();

    // (1) Gateway: click the painted Retry under row 1. No row is hovered.
    click(window, "act-retry-1");
    entity.update(window, |app, cx| {
        assert_eq!(
            transcript(app).len(),
            4,
            "the click on the painted Retry (no hover) started a retry"
        );
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        assert_eq!(
            transcript(app),
            vec![
                (crate::Role::User, "q".to_string()),
                (crate::Role::Assistant, "first answer".to_string()),
                (crate::Role::User, "q".to_string()),
                (crate::Role::Assistant, "retried answer".to_string()),
            ],
            "the painted Retry appended one user row and one reply"
        );
        assert_eq!(
            submits(&fake),
            vec!["prompt_submit(s1,q)", "prompt_submit(s1,q)"],
            "both gateway submits carry the user's prompt text"
        );
        // (2) The executor shown in the chip changes to an agent harness.
        // Its binary is a path that does not exist, so no real CLI runs.
        // (chosen as the chooser does: a retry follows the thread's route)
        assert!(app.set_composer_target(
            &crate::workshop_composer::ModelOwner::Work(CONV.into()),
            WorkExecutor::Agent {
                engine: CodeEngine::Codex,
                model: None,
            },
            cx,
        ));
        app.work.agents.codex_bin = Some(home.join("no-such-codex").display().to_string());
        app.work.folder = Some(home.clone());
        app.hovered_row = None;
    });
    window.run_until_parked();

    // Click the painted Retry under the NEW last row (3). No row is hovered.
    click(window, "act-retry-3");
    entity.update(window, |app, cx| {
        let rows = transcript(app);
        assert_eq!(
            rows.len(),
            6,
            "the retry appended a user row and a reply row"
        );
        assert_eq!(
            rows[4],
            (crate::Role::User, "q".to_string()),
            "the retry re-sends the user's prompt text"
        );
        assert_eq!(
            app.work.panel.requested_identity,
            Some(WorkRunIdentity::Agent {
                engine: CodeEngine::Codex,
                model: None,
            }),
            "the retry runs on the executor the chip shows now"
        );
        assert_eq!(
            submits(&fake).len(),
            2,
            "the gateway got no third submit: the retry left the old executor"
        );
        app.cancel_work_run(cx);
        app.work.gateway_cmd_tx = None;
    });
    handle.join().unwrap();
}

/// M2g-FIX1 (T2 P1), updated for RC3-S5: Enter in the main composer during a
/// live Work run emits no Submit; the draft no longer stays in the box but
/// joins the visible queue exactly as typed, and once the run ends it is sent
/// once. Keys go through the window's real key dispatch.
#[gpui::test]
fn m2g_fix1_enter_during_a_live_run_keeps_the_draft(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::retry_tests::m2g_fix1_enter_during_a_live_run_keeps_the_draft")
    else {
        return;
    };
    let fake = FakeOps::new(vec![
        vec![
            running(true),
            delta("first answer"),
            complete(),
            running(false),
        ],
        vec![
            running(true),
            delta("second answer"),
            complete(),
            running(false),
        ],
    ]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        app.mode = Mode::Work;
        let wired = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("q".into(), cx);
        assert!(app.work.running, "the first run is live");
        wired
    });
    let composer = entity.read_with(cx, |app, _| app.composer.clone());
    let seen_submits = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let seen = seen_submits.clone();
    cx.update(|cx| {
        cx.subscribe(
            &composer,
            move |_, ev: &crate::composer::ComposerEvent, _| {
                if matches!(ev, crate::composer::ComposerEvent::Submit) {
                    seen.set(seen.get() + 1);
                }
            },
        )
        .detach();
    });
    let (_v, window) = cx.add_window_view(|_, _| ComposerHarness {
        app: entity.clone(),
    });
    window.update(|window, cx| {
        let focus = gpui::Focusable::focus_handle(composer.read(cx), cx);
        window.focus(&focus, cx);
    });
    composer.update(window, |c, cx| c.set_text("next prompt", cx));
    window.run_until_parked();

    // Enter while the run is live: no Submit; the draft moves to the queue.
    press_enter(window);
    assert_eq!(
        seen_submits.get(),
        0,
        "Enter during a live run emits no Submit"
    );
    assert_eq!(
        composer.read_with(window, |c, _| c.text().to_string()),
        "",
        "the typed message left the box for the queue"
    );
    entity.read_with(window, |app, _| {
        let items = app.work.send_queue.items(CONV);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "next prompt");
        assert_eq!(items[0].state, crate::send_queue::QueueState::Queued);
    });

    // The run ends; the queue sends the message once.
    entity.update(window, |app, cx| {
        // M2-INT: main's U2c gateway health gate probes before a send on the
        // gateway lane; a fresh Up probe lets the queued send go straight out.
        assert!(app.work.health.gateway.begin_probe());
        app.work
            .health
            .gateway
            .finish_probe(Ok(()), std::time::Instant::now());
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        assert!(!app.work.running, "the first run ended");
    });
    entity.update(window, |app, cx| {
        assert!(app.work.running, "the queued message started");
        assert!(
            app.composer.read(cx).text().is_empty(),
            "the sent draft left the composer"
        );
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        assert_eq!(
            submits(&fake),
            vec!["prompt_submit(s1,q)", "prompt_submit(s1,next prompt)"],
            "the kept draft reached the gateway once, after the first run"
        );
        app.work.gateway_cmd_tx = None;
    });
    handle.join().unwrap();
}

/// RC2-UX (R2-14, round 2 Minor 6): Retry put a second copy of the question
/// in the transcript. Clicking the painted Retry still re-sends the question
/// (history is never rewritten), but its row is marked as a Retry: drawn as
/// one short note, saved with its mark, and still a note after a relaunch.
#[gpui::test]
fn rc2ux_a_retry_is_a_note_not_a_second_question(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::retry_tests::rc2ux_a_retry_is_a_note_not_a_second_question")
    else {
        return;
    };
    cx.executor().allow_parking();
    let db = home.join("rc2ux-retry.db");
    let store = awili_persist::Store::open(&db).unwrap();
    store.create_conversation(CONV, "retry", 0).unwrap();
    let (persist, worker) = crate::persistence::Persistence::spawn_worker(store);
    let fake = FakeOps::new(vec![
        vec![
            running(true),
            delta("first answer"),
            complete(),
            running(false),
        ],
        vec![
            running(true),
            delta("retried answer"),
            complete(),
            running(false),
        ],
    ]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        app.mode = Mode::Work;
        let wired = wire_gateway(app, &home, &fake, cx);
        app.persist = Some(persist);
        app.submit_work_run("q".into(), cx);
        pump(
            app,
            &wired.0,
            wired.2,
            |e| *e == WorkEvent::TurnComplete,
            cx,
        );
        app.hovered_row = None;
        wired
    });
    let (_v, window) = cx.add_window_view(|_, cx| {
        cx.observe(&entity, |_, _, cx| cx.notify()).detach();
        TranscriptHarness {
            app: entity.clone(),
        }
    });
    window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(1000.)));
    window.run_until_parked();
    click(window, "act-retry-1");
    entity.update(window, |app, cx| {
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        let kinds: Vec<_> = app.views[CONV]
            .convo
            .messages()
            .iter()
            .map(|m| (m.role, m.text.clone(), m.kind))
            .collect();
        let normal = awili_core::MessageKind::Normal;
        assert_eq!(
            kinds,
            vec![
                (crate::Role::User, "q".to_string(), normal),
                (crate::Role::Assistant, "first answer".to_string(), normal),
                (
                    crate::Role::User,
                    "q".to_string(),
                    awili_core::MessageKind::Retry
                ),
                (crate::Role::Assistant, "retried answer".to_string(), normal),
            ],
            "the question is sent again, marked as a Retry"
        );
        app.hovered_row = None;
        cx.notify();
    });
    window.run_until_parked();
    let note = |i: usize| -> &'static str {
        Box::leak(format!("{}-{i}", crate::prompt_history::RETRY_NOTE_SELECTOR).into_boxed_str())
    };
    assert_eq!(
        crate::prompt_history::RETRY_NOTE,
        "\u{21BB} You asked again."
    );
    assert!(
        window.debug_bounds(note(2)).is_some(),
        "the Retry row is a note"
    );
    assert!(
        window.debug_bounds(note(0)).is_none(),
        "the question is a bubble"
    );

    // Saved with its mark; after a relaunch the row is still a note.
    let persist = entity.update(window, |app, _| app.persist.take()).unwrap();
    drop(persist);
    worker.join().unwrap();
    let rows = awili_persist::Store::open(&db)
        .unwrap()
        .messages(CONV)
        .unwrap();
    let stored: Vec<&str> = rows
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.kind.as_str())
        .collect();
    assert_eq!(stored, ["normal", "retry"]);
    entity.update(window, |app, cx| {
        let restored = crate::restore_conversation(&rows);
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(restored),
        );
        assert!(app.views[CONV].rows[2].retry);
        assert!(!app.views[CONV].rows[0].retry);
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(note(2)).is_some(),
        "still a note after a relaunch"
    );
    handle.join().unwrap();
}

/// RC2-UX (R2-14): the Code tab's Retry marks its row the same way, on the
/// tab and in the tab's saved conversation.
#[gpui::test]
fn rc2ux_a_code_retry_is_marked_too(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::retry_tests::rc2ux_a_code_retry_is_marked_too") else {
        return;
    };
    cx.executor().allow_parking();
    let entity = cx.new(AwiliApp::new);
    let wait_idle = |cx: &mut gpui::TestAppContext| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let streaming = entity.read_with(cx, |app, _| {
                app.work
                    .code_tabs
                    .get(app.work.code_active)
                    .is_some_and(|t| t.streaming)
            });
            if !streaming {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the Code turn never finished"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Code;
        app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
        app.ensure_code_tab(cx);
        // Claude through the chooser's setter (its binary is a missing path, so
        // no installed CLI can run).
        app.test_choose_native_target(CodeEngine::Claude, None, cx);
        app.submit_code_turn("q".into(), cx);
    });
    wait_idle(cx);
    entity.update(cx, |app, cx| app.retry_code(cx));
    wait_idle(cx);
    entity.read_with(cx, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        let users: Vec<_> = tab
            .convo
            .messages()
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == crate::Role::User)
            .map(|(i, m)| (m.text.clone(), m.kind, tab.rows[i].retry))
            .collect();
        assert_eq!(
            users,
            vec![
                ("q".to_string(), awili_core::MessageKind::Normal, false),
                ("q".to_string(), awili_core::MessageKind::Retry, true),
            ]
        );
        let saved = tab.history.conv_id.as_ref().unwrap();
        let mirror: Vec<_> = app.views[saved]
            .convo
            .messages()
            .iter()
            .filter(|m| m.role == crate::Role::User)
            .map(|m| m.kind)
            .collect();
        assert_eq!(
            mirror,
            [
                awili_core::MessageKind::Normal,
                awili_core::MessageKind::Retry
            ]
        );
    });
}
