//! P3c round 4 — the stop barrier. While a gateway Stop is pending the
//! stopping turn owns the Work panel, its streaming row and the worker epoch
//! until the acknowledgement arrives: activating another Work conversation
//! cannot re-own the panel (R8), and a folder change is deferred to the ack
//! (R9). End to end over the real `AwiliApp` and the real worker loop, with
//! the round-3 harness of `work_gateway_queue_tests.rs`.

use super::gateway_queue_tests::{
    denial_event, isolated, pump, transcript, wire_gateway, CONV, DENIAL_TEXT, DRAIN,
};
use super::gateway_turn_tests::{complete, delta, running, stopped};
use super::tests::FakeOps;
use super::*;
use crate::work_last_run::LastRun;
use awili_core::ConvKind;
use std::cell::RefCell;
use std::rc::Rc;
use std::thread::JoinHandle;

/// The stopped turn's scripted backend: one turn cut off mid-stream, and an
/// interrupt answered by a raced `hook.denied`, a late delta and the turn's
/// own end signals (an `Ended` drain).
fn stopping_fake() -> FakeOps {
    let mut fake = FakeOps::new(vec![vec![running(true), delta("t1 ")]]);
    fake.interrupt_events = vec![denial_event(), delta("late"), complete(), running(false)];
    fake
}

/// R8: Stop → activate another Work conversation that has a stored last run
/// (the production `open_conversation_from_history` → `restore_work_last_run`
/// → `restore_into` path) → the panel, its owner and the streaming row are
/// untouched, the drain's `HookDenial` lands in the stopped row, the ack
/// clears the flag, and only THEN does the next activation restore.
#[gpui::test]
fn stop_barrier_holds_panel_ownership_until_ack(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::gateway_barrier_tests::stop_barrier_holds_panel_ownership_until_ack")
    else {
        return;
    };
    let fake = stopping_fake();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        // Already in Work mode, so activation is only the conversation switch.
        app.mode = Mode::Work;
        let other = app
            .conversations
            .create_kind(crate::feed::unix_now(), ConvKind::Work)
            .id;
        app.views.insert(
            other.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        let stored = LastRun {
            identity: WorkRunIdentity::HermesGateway,
            model: Some("stored-model".into()),
            status: RunStatus::Failed,
            error: Some("stored error".into()),
            finished_at: "2026-09-24T00:00:00Z".into(),
        };
        app.work_last_run.insert(other.clone(), stored.to_json());

        app.submit_work_run("first".into(), cx);
        pump(
            app,
            &ev_rx,
            epoch,
            |e| *e == WorkEvent::Token("t1 ".into()),
            cx,
        );
        app.cancel_work_run(cx);
        assert!(app.work.gateway_stop_pending);
        let panel = format!("{:?}", app.work.panel);
        let row = app.work.stream_row_idx;
        assert!(row.is_some());

        // Navigation during the Stop: the conversation switches, the panel
        // does not change hands.
        app.open_conversation_from_history(&other, cx);
        assert_eq!(app.sidebar.active_id(), other);
        assert_eq!(app.work.conv_id.as_deref(), Some(CONV));
        assert_eq!(app.work.stream_row_idx, row);
        assert_eq!(format!("{:?}", app.work.panel), panel);

        // The drain's denial and the ack land in the stopped turn's row.
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        assert!(!app.work.gateway_stop_pending);
        assert_eq!(
            transcript(app),
            vec![
                (Role::User, "first".to_string()),
                (Role::Assistant, format!("t1 {DENIAL_TEXT}")),
            ]
        );
        assert!(app.views[&other].convo.messages().is_empty());

        // Barrier lifted: the next activation restores the stored run.
        app.open_conversation_from_history(&other, cx);
        assert_eq!(app.work.conv_id.as_deref(), Some(other.as_str()));
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        assert_eq!(app.work.panel.error.as_deref(), Some("stored error"));
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

/// R9: Stop → change folder → submit (parked) → the old drain forwards
/// `hook.denied` → ack. The folder change touches neither the worker, its
/// session nor the flag; the denial is in the stopped row; the old worker
/// gets `Shutdown` only after the ack (its call log carries the UI's ack
/// marker before `session_close`); the parked prompt runs once on a NEW
/// session in the new folder.
#[gpui::test]
fn folder_change_during_stop_is_deferred_to_ack(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::gateway_barrier_tests::folder_change_during_stop_is_deferred_to_ack")
    else {
        return;
    };
    let fake = stopping_fake();
    let mut fresh = FakeOps::new(vec![vec![
        running(true),
        delta("new answer"),
        complete(),
        running(false),
    ]]);
    fresh.session_name = "s2".into();
    let new_dir = home.join("new-folder");
    std::fs::create_dir_all(&new_dir).unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("first".into(), cx);
        pump(
            app,
            &ev_rx,
            epoch,
            |e| *e == WorkEvent::Token("t1 ".into()),
            cx,
        );
        app.cancel_work_run(cx);
        assert!(app.work.gateway_stop_pending);

        app.set_work_folder(new_dir.clone(), cx);
        assert_eq!(app.work.folder.as_deref(), Some(new_dir.as_path()));
        assert!(app.work.gateway_stop_pending, "the barrier stays up");
        assert!(app.work.folder_changed_during_stop);
        assert!(app.work.gateway_cmd_tx.is_some(), "no Shutdown yet");
        assert_eq!(app.work.session_id.as_deref(), Some("s1"));
        assert_eq!(app.work.run_seq, epoch);

        app.submit_work_run("second".into(), cx);
        assert_eq!(
            app.work.queued_after_stop.as_ref().map(|q| q.text.as_str()),
            Some("second")
        );

        // The respawn after the ack runs the real loop over `fresh`.
        type Spawned = (async_channel::Receiver<WorkEvent>, JoinHandle<()>);
        let spawned: Rc<RefCell<Option<Spawned>>> = Rc::default();
        let (slot, worker) = (spawned.clone(), fresh.clone());
        TEST_WORKER_SPAWN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |ctx: WorkerContext| {
                let (ev_tx, ev_rx) = async_channel::unbounded();
                let ctx = WorkerContext {
                    events: ev_tx,
                    ..ctx
                };
                let handle =
                    std::thread::spawn(move || run_worker_loop_with_drain(worker, ctx, DRAIN));
                *slot.borrow_mut() = Some((ev_rx, handle));
            }));
        });

        let marker = fake.clone();
        pump(
            app,
            &ev_rx,
            epoch,
            |e| {
                let ack = *e == stopped();
                if ack {
                    marker.record("ui_ack".into());
                }
                ack
            },
            cx,
        );
        assert!(!app.work.gateway_stop_pending);
        assert!(!app.work.folder_changed_during_stop);
        assert!(app.work.queued_after_stop.is_none());
        let Some((new_rx, new_handle)) = spawned.borrow_mut().take() else {
            panic!("the parked prompt did not respawn a worker");
        };
        handle.join().unwrap(); // the old worker ended on its Shutdown
        let new_epoch = app.work.run_seq;
        assert_ne!(new_epoch, epoch, "a fresh worker epoch");
        pump(
            app,
            &new_rx,
            new_epoch,
            |e| *e == WorkEvent::TurnComplete,
            cx,
        );
        assert_eq!(app.work.session_id.as_deref(), Some("s2"));
        assert_eq!(
            transcript(app),
            vec![
                (Role::User, "first".to_string()),
                (Role::Assistant, format!("t1 {DENIAL_TEXT}")),
                (Role::User, "second".to_string()),
                (Role::Assistant, "new answer".to_string()),
            ]
        );
        app.work.gateway_cmd_tx = None;
        new_handle.join().unwrap();
    });
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,first)",
            "session_interrupt(s1)",
            "ui_ack",
            "session_close(s1)",
            "close",
        ]
    );
    assert_eq!(
        fresh.ops(),
        vec![
            format!("session_create({})", new_dir.display()),
            "prompt_submit(s2,second)".to_string(),
            "close".to_string(),
        ]
    );
}
