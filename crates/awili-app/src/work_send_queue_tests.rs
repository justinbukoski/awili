//! RC3-S5: messages typed while a turn runs queue under the composer, drain
//! in order when it ends, and obey Edit / Cancel / Send now / Resume. End to
//! end on the real submit path over the scripted gateway worker; the queue's
//! own observer (`send_queue_tick`) does the draining between updates.

use super::gateway_queue_tests::{isolated, pump, wire_gateway, CONV};
use super::gateway_turn_tests::{complete, delta, running, stopped};
use super::tests::{gw_event, FakeOps};
use super::*;
use crate::send_queue::{PauseReason, QueueState};
use serde_json::json;

/// A wired gateway session whose health check already passed, so a drained
/// send goes straight to the worker instead of waiting for a probe.
fn wire(
    app: &mut AwiliApp,
    home: &std::path::Path,
    fake: &FakeOps,
    cx: &mut Context<AwiliApp>,
) -> (
    async_channel::Receiver<WorkEvent>,
    std::thread::JoinHandle<()>,
    u64,
) {
    let wired = wire_gateway(app, home, fake, cx);
    app.work
        .health
        .gateway
        .finish_probe(Ok(()), std::time::Instant::now());
    wired
}

fn done_turn(text: &str) -> Vec<GatewayEvent> {
    vec![running(true), delta(text), complete(), running(false)]
}

fn user_rows(app: &AwiliApp, conv: &str) -> Vec<String> {
    app.views[conv]
        .convo
        .messages()
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.text.clone())
        .collect()
}

/// Type `text` into the composer and press Enter while the turn is busy.
fn enter_while_busy(app: &mut AwiliApp, text: &str, cx: &mut Context<AwiliApp>) {
    assert!(app.composer_busy(), "the turn must be running");
    app.composer.update(cx, |c, cx| c.set_text(text, cx));
    app.queue_busy_draft(cx);
}

fn states(app: &AwiliApp, conv: &str) -> Vec<(String, QueueState)> {
    app.work
        .send_queue
        .items(conv)
        .iter()
        .map(|i| (i.text.clone(), i.state))
        .collect()
}

fn finish(
    entity: &gpui::Entity<AwiliApp>,
    cx: &mut gpui::TestAppContext,
    ev_rx: &async_channel::Receiver<WorkEvent>,
    epoch: u64,
) {
    entity.update(cx, |app, cx| {
        pump(app, ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx)
    });
}

/// Enter during a run queues (the box empties, the row says Queued); when the
/// turn ends the observer sends the messages in order, one turn at a time.
#[gpui::test]
fn s5_enter_while_running_queues_then_drains_in_order(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::send_queue_tests::s5_enter_while_running_queues_then_drains_in_order")
    else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1"), done_turn("a2"), done_turn("a3")]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        enter_while_busy(app, "second", cx);
        enter_while_busy(app, "third", cx);
        assert_eq!(app.composer.read(cx).text(), "", "the box is free again");
        assert_eq!(
            states(app, CONV),
            vec![
                ("second".into(), QueueState::Queued),
                ("third".into(), QueueState::Queued)
            ]
        );
        assert_eq!(user_rows(app, CONV), vec!["first"], "nothing sent yet");
        assert!(app.send_queue_panel(cx).is_some(), "the queue is drawn");
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert!(app.work.running, "the observer sent the next message");
        assert_eq!(user_rows(app, CONV), vec!["first", "second"]);
        assert_eq!(
            states(app, CONV),
            vec![
                ("second".into(), QueueState::Sent),
                ("third".into(), QueueState::Queued)
            ]
        );
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert_eq!(user_rows(app, CONV), vec!["first", "second", "third"]);
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert!(!app.work.running);
        assert!(states(app, CONV)
            .iter()
            .all(|(_, s)| *s == QueueState::Sent));
        app.work.gateway_cmd_tx = None;
    });
    handle.join().unwrap();
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,first)",
            "prompt_submit(s1,second)",
            "prompt_submit(s1,third)",
            "close",
        ]
    );
}

/// Edit puts the text back in the composer and takes it out of the queue;
/// Cancel keeps the row (as Cancelled) but never sends it.
#[gpui::test]
fn s5_edit_and_cancel_keep_their_messages_out_of_the_run(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::send_queue_tests::s5_edit_and_cancel_keep_their_messages_out_of_the_run")
    else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1"), done_turn("a2")]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        enter_while_busy(app, "edit me", cx);
        enter_while_busy(app, "cancel me", cx);
        enter_while_busy(app, "keep me", cx);
        let ids: Vec<u64> = app
            .work
            .send_queue
            .items(CONV)
            .iter()
            .map(|i| i.id)
            .collect();
        app.send_queue_edit(CONV, ids[0], cx);
        assert_eq!(app.composer.read(cx).text(), "edit me");
        app.send_queue_cancel(CONV, ids[1], cx);
        assert_eq!(
            states(app, CONV),
            vec![
                ("cancel me".into(), QueueState::Cancelled),
                ("keep me".into(), QueueState::Queued)
            ]
        );
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert_eq!(user_rows(app, CONV), vec!["first", "keep me"]);
        app.work.gateway_cmd_tx = None;
    });
    handle.join().unwrap();
}

/// Send now stops the running turn with the ordinary Stop and sends that
/// message next, ahead of messages queued before it.
#[gpui::test]
fn s5_send_now_stops_the_turn_and_goes_first(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::send_queue_tests::s5_send_now_stops_the_turn_and_goes_first")
    else {
        return;
    };
    let mut fake = FakeOps::new(vec![
        vec![running(true), delta("t1 ")],
        done_turn("urgent answer"),
        done_turn("early answer"),
    ]);
    fake.interrupt_events = vec![complete(), running(false)];
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        pump(
            app,
            &wired.0,
            wired.2,
            |e| *e == WorkEvent::Token("t1 ".into()),
            cx,
        );
        enter_while_busy(app, "early", cx);
        enter_while_busy(app, "urgent", cx);
        let urgent = app.work.send_queue.items(CONV)[1].id;
        app.send_queue_send_now(CONV, urgent, cx);
        assert!(app.work.gateway_stop_pending, "the ordinary Stop ran");
        assert_eq!(
            user_rows(app, CONV),
            vec!["first"],
            "not sent before the ack"
        );
        wired
    });
    entity.update(cx, |app, cx| {
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx)
    });
    entity.update(cx, |app, _| {
        assert!(app.work.running);
        assert_eq!(user_rows(app, CONV), vec!["first", "urgent"]);
    });
    finish(&entity, cx, &ev_rx, epoch);
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert_eq!(user_rows(app, CONV), vec!["first", "urgent", "early"]);
        app.work.gateway_cmd_tx = None;
    });
    handle.join().unwrap();
}

/// A failed turn pauses the queue (nothing fires); Resume sends the next one.
#[gpui::test]
fn s5_a_failed_turn_pauses_the_queue_until_resume(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::send_queue_tests::s5_a_failed_turn_pauses_the_queue_until_resume")
    else {
        return;
    };
    let fake = FakeOps::new(vec![
        vec![
            running(true),
            gw_event("error", json!({ "message": "boom" })),
        ],
        done_turn("recovered"),
    ]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        enter_while_busy(app, "after the failure", cx);
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, cx| {
        assert!(!app.work.running, "nothing fired");
        assert_eq!(user_rows(app, CONV), vec!["first"]);
        assert_eq!(app.work.send_queue.paused(CONV), Some(&PauseReason::Failed));
        assert_eq!(
            states(app, CONV),
            vec![("after the failure".into(), QueueState::Queued)]
        );
        app.send_queue_resume(CONV, cx);
    });
    entity.update(cx, |app, cx| {
        assert!(app.work.running, "Resume sent the next message");
        assert_eq!(user_rows(app, CONV), vec!["first", "after the failure"]);
        assert_eq!(
            states(app, CONV),
            vec![("after the failure".into(), QueueState::Sent)]
        );
        // The failed turn retired its worker, so the resumed turn spawned its
        // own (not the scripted one): stop it rather than wait on it.
        app.cancel_work_run(cx);
    });
    handle.join().unwrap();
}

/// A plain Stop (not Send now) also pauses: the owner asked the run to stop,
/// so the rest does not start on its own.
#[gpui::test]
fn s5_a_plain_stop_pauses_the_queue(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::send_queue_tests::s5_a_plain_stop_pauses_the_queue") else {
        return;
    };
    let mut fake = FakeOps::new(vec![vec![running(true), delta("t1 ")]]);
    fake.interrupt_events = vec![complete(), running(false)];
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        pump(
            app,
            &wired.0,
            wired.2,
            |e| *e == WorkEvent::Token("t1 ".into()),
            cx,
        );
        enter_while_busy(app, "later", cx);
        wired
    });
    entity.update(cx, |app, cx| app.cancel_work_run(cx));
    entity.update(cx, |app, cx| {
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx)
    });
    entity.update(cx, |app, _| {
        assert!(!app.work.running);
        assert_eq!(
            app.work.send_queue.paused(CONV),
            Some(&PauseReason::Stopped)
        );
        assert_eq!(user_rows(app, CONV), vec!["first"]);
        app.work.gateway_cmd_tx = None;
    });
    handle.join().unwrap();
}

/// The queue belongs to its conversation: after switching to another one,
/// the first conversation's message still goes to the first conversation,
/// and the other conversation neither shows nor receives it.
#[gpui::test]
fn s5_the_queue_stays_with_its_conversation(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::send_queue_tests::s5_the_queue_stays_with_its_conversation")
    else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1"), done_turn("a2")]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        enter_while_busy(app, "for the first thread", cx);
        app.views.insert(
            "other".into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        crate::exec_fixture::seed_conversation_metadata(app, "other", awili_core::ConvKind::Work);
        app.set_active_conversation("other");
        assert!(app.work.send_queue.items("other").is_empty());
        assert!(
            app.send_queue_panel(cx).is_none(),
            "the other thread shows no queue"
        );
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert_eq!(user_rows(app, CONV), vec!["first", "for the first thread"]);
        assert!(user_rows(app, "other").is_empty());
        assert_eq!(app.work.send_queue.queued_count("other"), 0);
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
}

// ---- review fixes -----------------------------------------------------------

/// A plan the owner has not answered holds the queue: the next message would
/// supersede it. Denying the plan lets the queue go on.
#[gpui::test]
fn s5_an_open_plan_holds_the_queue_until_answered(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::send_queue_tests::s5_an_open_plan_holds_the_queue_until_answered")
    else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1"), done_turn("a2")]);
    let entity = cx.new(AwiliApp::new);
    let key = crate::ask_mode::AskKey::Work(CONV.into());
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        enter_while_busy(app, "second", cx);
        app.work
            .plan_gates
            .arm(key.clone(), awili_code::CodeEngine::Claude, None);
        let plan = format!("the plan\n{}", awili_code::PLAN_READY_MARKER);
        assert!(app.work.plan_gates.settle(&key, &plan, true).1);
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, cx| {
        assert!(!app.work.running, "nothing was sent over the open plan");
        assert!(app.work.plan_gates.open_for(&key).is_some(), "plan kept");
        assert_eq!(user_rows(app, CONV), vec!["first"]);
        assert!(app.send_queue_hold(CONV).is_some());
        assert_eq!(app.work.send_queue.queued_count(CONV), 1);
        // The owner answers: Deny. The queue goes on by itself.
        app.work.plan_gates.deny(&key);
        cx.notify();
    });
    entity.update(cx, |app, _| {
        assert!(app.work.running, "the queue resumed after the answer");
        assert_eq!(user_rows(app, CONV), vec!["first", "second"]);
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
}

/// A message goes out with the helper it was queued for, even when the next
/// turn's choice was changed meanwhile; the choice itself is left alone.
#[gpui::test]
fn s5_a_queued_message_keeps_the_helper_it_was_queued_for(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::send_queue_tests::s5_a_queued_message_keeps_the_helper_it_was_queued_for")
    else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1"), done_turn("a2")]);
    let entity = cx.new(AwiliApp::new);
    let other = WorkExecutor::Agent {
        engine: awili_code::CodeEngine::Claude,
        model: Some("sonnet".into()),
    };
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        enter_while_busy(app, "second", cx);
        assert_eq!(
            app.work.send_queue.items(CONV)[0].snap.target,
            WorkExecutor::Gateway
        );
        // The owner picks Claude for the NEXT turn while the first runs.
        app.work
            .conversation_routes
            .get_mut(CONV)
            .unwrap()
            .set_next_target(other.clone());
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert!(app.work.running);
        assert_eq!(app.work.executor, WorkExecutor::Gateway, "sent via Gateway");
        assert_eq!(
            app.work.conversation_routes[CONV].next_target(),
            &other,
            "the owner's next choice is untouched"
        );
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,first)",
            "prompt_submit(s1,second)",
            "close",
        ]
    );
}

/// A helper that is no longer available, or a changed Ask/Auto choice,
/// pauses the queue with a plain reason instead of sending under other terms;
/// Resume then takes the choices now in force.
#[gpui::test]
fn s5_changed_choices_pause_the_queue_with_a_reason(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::send_queue_tests::s5_changed_choices_pause_the_queue_with_a_reason")
    else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1"), done_turn("a2")]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        enter_while_busy(app, "second", cx);
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(CONV.into()),
            awili_work::ApprovalMode::Auto,
        );
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, cx| {
        assert!(!app.work.running, "not sent under the other setting");
        let Some(PauseReason::Refused(why)) = app.work.send_queue.paused(CONV) else {
            panic!("expected a pause with a reason");
        };
        assert!(why.contains("ask-before-changes"), "{why}");
        app.send_queue_resume(CONV, cx);
    });
    entity.update(cx, |app, _| {
        assert!(app.work.running, "Resume sent it under the current choice");
        assert_eq!(user_rows(app, CONV), vec!["first", "second"]);
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
}

/// Files on the tray when Enter is pressed travel with THAT message, and a
/// file added afterwards for the next message does not ride on it.
#[gpui::test]
fn s5_staged_files_travel_with_the_message_they_were_staged_for(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::send_queue_tests::s5_staged_files_travel_with_the_message_they_were_staged_for",
    ) else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1"), done_turn("a2")]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        let store = app.search_store.as_ref().expect("a store in the test home");
        store
            .create_conversation_with_kind(CONV, "files", 1, "work")
            .unwrap();
        let one = store
            .store_attachment("one.txt", "text/plain", b"FIRSTFILE")
            .unwrap();
        app.submit_work_run("first".into(), cx);
        app.search_store
            .as_ref()
            .unwrap()
            .stage_attachment(CONV, one)
            .unwrap();
        app.refresh_staged_attachments();
        enter_while_busy(app, "second", cx);
        assert!(app.staged_attachments.is_empty(), "the tray was emptied");
        assert_eq!(
            app.work.send_queue.items(CONV)[0].snap.attachment_ids,
            vec![one]
        );
        // A file for a LATER message goes on the tray afterwards.
        let store = app.search_store.as_ref().unwrap();
        let two = store
            .store_attachment("two.txt", "text/plain", b"SECONDFILE")
            .unwrap();
        store.stage_attachment(CONV, two).unwrap();
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        let rows = user_rows(app, CONV);
        assert!(
            rows[1].contains("FIRSTFILE") && rows[1].ends_with("second"),
            "{rows:?}"
        );
        assert!(
            !rows[1].contains("SECONDFILE"),
            "the later file stayed behind"
        );
        let still = app
            .search_store
            .as_ref()
            .unwrap()
            .get_staged_attachments(CONV)
            .unwrap();
        assert_eq!(still.len(), 1, "the later file is still staged");
        assert_eq!(still[0].filename, "two.txt");
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
}

/// Edit gives the files back with the text.
#[gpui::test]
fn s5_edit_puts_the_files_back_on_the_tray(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::send_queue_tests::s5_edit_puts_the_files_back_on_the_tray")
    else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1")]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        let store = app.search_store.as_ref().expect("a store in the test home");
        store
            .create_conversation_with_kind(CONV, "files", 1, "work")
            .unwrap();
        app.submit_work_run("first".into(), cx);
        let store = app.search_store.as_ref().unwrap();
        let one = store
            .store_attachment("one.txt", "text/plain", b"X")
            .unwrap();
        store.stage_attachment(CONV, one).unwrap();
        app.refresh_staged_attachments();
        enter_while_busy(app, "second", cx);
        let id = app.work.send_queue.items(CONV)[0].id;
        app.send_queue_edit(CONV, id, cx);
        assert_eq!(app.staged_attachments.len(), 1, "the file is back");
        assert_eq!(app.composer.read(cx).text(), "second");
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
}

/// After a Stop paused the queue, Resume sends the next message; when THAT
/// turn finishes normally the queue goes on (no stale Stop mark pauses it).
#[gpui::test]
fn s5_a_normal_finish_after_an_earlier_stop_does_not_pause(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::send_queue_tests::s5_a_normal_finish_after_an_earlier_stop_does_not_pause")
    else {
        return;
    };
    let mut fake = FakeOps::new(vec![
        vec![running(true), delta("t1 ")],
        done_turn("a2"),
        done_turn("a3"),
    ]);
    fake.interrupt_events = vec![complete(), running(false)];
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        pump(
            app,
            &wired.0,
            wired.2,
            |e| *e == WorkEvent::Token("t1 ".into()),
            cx,
        );
        enter_while_busy(app, "second", cx);
        enter_while_busy(app, "third", cx);
        wired
    });
    entity.update(cx, |app, cx| app.cancel_work_run(cx));
    entity.update(cx, |app, cx| {
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx)
    });
    entity.update(cx, |app, cx| {
        assert_eq!(
            app.work.send_queue.paused(CONV),
            Some(&PauseReason::Stopped)
        );
        // The Stop left its mark; submit resets it (work.rs owner_stopped = false
        // in submit_work_run_kind, and the RunPanel reset just below it).
        assert!(
            app.work.owner_stopped,
            "the Stop mark is set before the new turn"
        );
        app.send_queue_resume(CONV, cx);
    });
    entity.update(cx, |app, _| {
        assert_eq!(user_rows(app, CONV), vec!["first", "second"]);
        assert!(
            !app.work.owner_stopped,
            "the new turn cleared the Stop mark"
        );
        assert!(app.work.panel.status.is_none(), "and the panel was reset");
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert!(app.work.send_queue.paused(CONV).is_none(), "not paused");
        assert!(app.work.running, "the third message went");
        assert_eq!(user_rows(app, CONV), vec!["first", "second", "third"]);
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
}

/// Send now for a message in a thread that is NOT the one running stops
/// nothing and leaves no "send next" mark behind.
#[gpui::test]
fn s5_send_now_in_an_idle_thread_leaves_no_stale_mark(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::send_queue_tests::s5_send_now_in_an_idle_thread_leaves_no_stale_mark")
    else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1")]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        app.work.send_queue.enqueue(
            "elsewhere",
            "later".into(),
            crate::send_queue::QueueSnapshot {
                body: "later".into(),
                target: WorkExecutor::Gateway,
                folder: None,
                mode: Default::default(),
                attachment_ids: Vec::new(),
            },
        );
        let id = app.work.send_queue.items("elsewhere")[0].id;
        app.send_queue_send_now("elsewhere", id, cx);
        assert!(app.work.running, "the running thread was not stopped");
        assert!(app.work.send_queue.send_now_for.is_none(), "no stale mark");
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
}

/// A helper that stopped being available, or a folder that is gone, pauses
/// the queue with a plain reason instead of sending somewhere else.
#[gpui::test]
fn s5_an_unavailable_helper_or_missing_folder_pauses_the_queue(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::send_queue_tests::s5_an_unavailable_helper_or_missing_folder_pauses_the_queue",
    ) else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1")]);
    let entity = cx.new(AwiliApp::new);
    let gone = home.join("will-be-deleted");
    std::fs::create_dir_all(&gone).unwrap();
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        app.work.folder = Some(gone.clone());
        enter_while_busy(app, "second", cx);
        app.work.folder = None;
        std::fs::remove_dir_all(&gone).unwrap();
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert!(!app.work.running);
        let Some(PauseReason::Refused(why)) = app.work.send_queue.paused(CONV) else {
            panic!("expected a pause with a reason");
        };
        assert!(why.contains("folder"), "{why}");
        assert_eq!(user_rows(app, CONV), vec!["first"]);
        // Resume takes the choices now in force, then the helper vanishes.
        app.work.gateway.enabled = false;
        app.work.hermes.enabled = false;
    });
    entity.update(cx, |app, cx| {
        app.send_queue_resume(CONV, cx);
    });
    entity.update(cx, |app, _| {
        let Some(PauseReason::Refused(why)) = app.work.send_queue.paused(CONV) else {
            panic!("expected a pause for the missing helper");
        };
        assert!(why.contains("helper or model"), "{why}");
        assert_eq!(user_rows(app, CONV), vec!["first"]);
        app.work.gateway_cmd_tx = None;
    });
    handle.join().unwrap();
}

/// The message runs in the folder it was queued under, not the one shown
/// when its turn comes.
#[gpui::test]
fn s5_a_queued_message_runs_in_the_folder_it_was_queued_under(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::send_queue_tests::s5_a_queued_message_runs_in_the_folder_it_was_queued_under",
    ) else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1")]);
    let entity = cx.new(AwiliApp::new);
    let (a, b) = (home.join("folder-a"), home.join("folder-b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        app.work.folder = Some(a.clone());
        enter_while_busy(app, "second", cx);
        app.work.folder = Some(b.clone());
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, cx| {
        assert_eq!(
            app.work.folder.as_ref(),
            Some(&a),
            "bound to the queued folder"
        );
        assert_eq!(user_rows(app, CONV), vec!["first", "second"]);
        app.work.gateway_cmd_tx = None;
        app.cancel_work_run(cx);
    });
    handle.join().unwrap();
}

fn background_setup(
    entity: &gpui::Entity<AwiliApp>,
    cx: &mut gpui::TestAppContext,
    home: &std::path::Path,
    fake: &FakeOps,
    queued_in: &std::path::Path,
    shown_after: &std::path::Path,
) -> (
    async_channel::Receiver<WorkEvent>,
    std::thread::JoinHandle<()>,
    u64,
) {
    entity.update(cx, |app, cx| {
        let wired = wire(app, home, fake, cx);
        app.mode = Mode::Work;
        app.submit_work_run("first".into(), cx);
        app.work.folder = Some(queued_in.to_path_buf());
        enter_while_busy(app, "second", cx);
        app.views.insert(
            "other".into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        crate::exec_fixture::seed_conversation_metadata(app, "other", awili_core::ConvKind::Work);
        app.set_active_conversation("other");
        app.work.folder = Some(shown_after.to_path_buf());
        wired
    })
}

/// Draining a BACKGROUND thread's queue never rebinds the folder of the
/// thread on screen: with the same folder it just goes; with another folder
/// it pauses and says why.
#[gpui::test]
fn s5_a_background_drain_leaves_the_open_threads_folder_alone(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::send_queue_tests::s5_a_background_drain_leaves_the_open_threads_folder_alone",
    ) else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1"), done_turn("a2")]);
    let entity = cx.new(AwiliApp::new);
    let (a, b) = (home.join("folder-a"), home.join("folder-b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    // Same folder: the queue goes, the folder stays.
    let (ev_rx, handle, epoch) = background_setup(&entity, cx, &home, &fake, &a, &a);
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert_eq!(user_rows(app, CONV), vec!["first", "second"]);
        assert_eq!(app.work.folder.as_ref(), Some(&a));
        assert_eq!(app.sidebar.active_id(), "other");
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
}

#[gpui::test]
fn s5_a_background_queue_for_another_folder_waits_instead_of_rebinding(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::send_queue_tests::s5_a_background_queue_for_another_folder_waits_instead_of_rebinding",
    ) else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1")]);
    let entity = cx.new(AwiliApp::new);
    let (a, b) = (home.join("folder-a"), home.join("folder-b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (ev_rx, handle, epoch) = background_setup(&entity, cx, &home, &fake, &a, &b);
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| {
        assert!(!app.work.running);
        assert_eq!(
            app.work.folder.as_ref(),
            Some(&b),
            "the open thread's folder is untouched"
        );
        let Some(PauseReason::Refused(why)) = app.work.send_queue.paused(CONV) else {
            panic!("expected a pause");
        };
        assert!(why.contains("different folder"), "{why}");
        assert_eq!(user_rows(app, CONV), vec!["first"]);
        app.work.gateway_cmd_tx = None;
    });
    handle.join().unwrap();
}

/// Cancel gives the row's files back to the tray.
#[gpui::test]
fn s5_cancel_puts_the_files_back_on_the_tray(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::send_queue_tests::s5_cancel_puts_the_files_back_on_the_tray")
    else {
        return;
    };
    let fake = FakeOps::new(vec![done_turn("a1")]);
    let entity = cx.new(AwiliApp::new);
    let (ev_rx, handle, epoch) = entity.update(cx, |app, cx| {
        let wired = wire(app, &home, &fake, cx);
        app.mode = Mode::Work;
        let store = app.search_store.as_ref().expect("a store in the test home");
        store
            .create_conversation_with_kind(CONV, "files", 1, "work")
            .unwrap();
        app.submit_work_run("first".into(), cx);
        let store = app.search_store.as_ref().unwrap();
        let one = store
            .store_attachment("one.txt", "text/plain", b"X")
            .unwrap();
        store.stage_attachment(CONV, one).unwrap();
        app.refresh_staged_attachments();
        enter_while_busy(app, "second", cx);
        assert!(app.staged_attachments.is_empty());
        let id = app.work.send_queue.items(CONV)[0].id;
        app.send_queue_cancel(CONV, id, cx);
        assert_eq!(app.staged_attachments.len(), 1, "the file is back");
        wired
    });
    finish(&entity, cx, &ev_rx, epoch);
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    handle.join().unwrap();
}

#[gpui::test]
fn attachment_read_failure_keeps_busy_draft_and_tray(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("work::send_queue_tests::attachment_read_failure_keeps_busy_draft_and_tray")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.mode = Mode::Work;
        app.work.running = true;
        let conv = app.sidebar.active_id().to_string();
        let store = app.search_store.as_ref().unwrap();
        // The startup conversation may already be persisted; create only if absent.
        if !store.conversations().unwrap().iter().any(|c| c.id == conv) {
            store
                .create_conversation_with_kind(&conv, "files", 1, "work")
                .unwrap();
        }
        let id = store
            .store_attachment("fixture.txt", "text/plain", b"fixture")
            .unwrap();
        store.stage_attachment(&conv, id).unwrap();
        app.composer
            .update(cx, |c, cx| c.set_text("keep this draft", cx));
        crate::send_queue_app::FAIL_STAGED_READ.with(|flag| flag.set(true));
        app.queue_busy_draft(cx);
        crate::send_queue_app::FAIL_STAGED_READ.with(|flag| flag.set(false));
        assert_eq!(app.composer.read(cx).text(), "keep this draft");
        assert!(app.work.send_queue.items(&conv).is_empty());
        assert_eq!(
            app.search_store
                .as_ref()
                .unwrap()
                .get_staged_attachments(&conv)
                .unwrap()
                .iter()
                .map(|a| a.id)
                .collect::<Vec<_>>(),
            vec![id]
        );
    });
}
