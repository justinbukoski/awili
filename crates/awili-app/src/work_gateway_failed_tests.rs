//! P3c round 6 — R13: while a gateway Stop is pending, only the worker's
//! `Terminal { Stopped }` (or the dead-worker fallback) acknowledges it. A
//! `Failed` the worker emitted BEFORE it read the Cancel — here a timed-out
//! `prompt_submit`, still buffered in the event channel when Stop is pressed —
//! is recorded on the stopped turn and releases nothing, while the worker's
//! idle-Cancel interrupt is still outstanding. End to end over the real
//! `AwiliApp` (`submit_work_run`, `cancel_work_run`, the production reducer)
//! and the real worker loop, with the round-3/4 harness.

use super::gateway_queue_tests::{isolated, pump, transcript, wire_gateway, DRAIN};
use super::gateway_turn_tests::{complete, delta, running, stopped};
use super::tests::{FakeOps, Latch};
use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::thread::JoinHandle;
use std::time::Instant;

/// Take the next worker event off `rx` WITHOUT applying it (the UI has not
/// consumed it yet).
fn hold_next(rx: &async_channel::Receiver<WorkEvent>) -> WorkEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(ev) = rx.try_recv() {
            return ev;
        }
        assert!(Instant::now() < deadline, "worker event never arrived");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Wait until the fake has recorded `op` (the worker is inside that RPC).
fn wait_for_op(fake: &FakeOps, op: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fake.ops().iter().any(|o| o == op) {
        assert!(Instant::now() < deadline, "{op} never started");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// R13 (Codex review 5, F1): submit → the submission times out after the
/// backend accepted it and the worker emits `Failed` (held in the channel) →
/// Stop (the real `cancel_work_run`: flag set, Cancel sent) → a second prompt
/// is parked → the worker's idle-Cancel `session_interrupt` blocks on a latch
/// → the held `Failed` is delivered: the barrier is intact (flag, sender,
/// session, epoch), nothing respawned, the prompt still parked, and the
/// failure text is in the stopped turn's row. Opening the latch lets the
/// worker retire with `Terminal { Stopped }`, which ends the barrier and
/// replays the parked prompt exactly once on a fresh worker.
#[gpui::test]
fn buffered_failed_does_not_release_the_stop_barrier(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::gateway_failed_tests::buffered_failed_does_not_release_the_stop_barrier")
    else {
        return;
    };
    let mut fake = FakeOps::new(vec![vec![running(true), delta("accepted anyway")]]);
    fake.fail_submit = true;
    let latch = Latch::default();
    fake.interrupt_latch = Some(latch.clone());
    let mut fresh = FakeOps::new(vec![vec![
        running(true),
        delta("second answer"),
        complete(),
        running(false),
    ]]);
    fresh.session_name = "s2".into();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("first".into(), cx);

        // The worker's `Failed` is buffered; the UI has not consumed it.
        let held = hold_next(&ev_rx);
        let WorkEvent::Failed(msg) = &held else {
            panic!("expected the submission's Failed, got {held:?}");
        };
        let msg = msg.clone();
        assert!(msg.starts_with("prompt.submit failed"), "{msg}");
        assert!(app.work.running, "the UI still sees the turn running");

        app.cancel_work_run(cx);
        assert!(app.work.gateway_stop_pending);
        app.submit_work_run("second".into(), cx);
        assert_eq!(
            app.work.queued_after_stop.as_ref().map(|q| q.text.as_str()),
            Some("second")
        );
        // The idle-Cancel interrupt is now outstanding.
        wait_for_op(&fake, "session_interrupt(s1)");

        // Any respawn would run the real loop over `fresh`.
        type Spawned = (async_channel::Receiver<WorkEvent>, JoinHandle<()>);
        let spawned: Rc<RefCell<Option<Spawned>>> = Rc::default();
        let spawns = Rc::new(Cell::new(0usize));
        let (slot, count, worker) = (spawned.clone(), spawns.clone(), fresh.clone());
        TEST_WORKER_SPAWN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |ctx: WorkerContext| {
                count.set(count.get() + 1);
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

        // Deliver the held `Failed` exactly as the production consumer does.
        assert_eq!(app.work.run_seq, epoch);
        for up in work_event_to_updates(held) {
            app.apply_work_update(up, cx);
        }
        assert!(app.work.gateway_stop_pending, "the barrier stays up");
        assert!(app.work.gateway_cmd_tx.is_some(), "the sender is kept");
        assert_eq!(app.work.session_id.as_deref(), Some("s1"));
        assert_eq!(app.work.run_seq, epoch, "no epoch bump");
        assert_eq!(spawns.get(), 0, "no respawn");
        assert_eq!(
            app.work.queued_after_stop.as_ref().map(|q| q.text.as_str()),
            Some("second"),
            "the parked prompt is still parked"
        );
        assert_eq!(
            transcript(app),
            vec![
                (Role::User, "first".to_string()),
                (Role::Assistant, failure_during_stop_line(&msg)),
            ],
            "the failure text is evidence on the stopped turn"
        );
        assert!(ev_rx.is_empty(), "the worker is still in the interrupt");

        // The interrupt completes: the worker retires with `Stopped`.
        latch.open();
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        assert!(!app.work.gateway_stop_pending);
        assert!(app.work.queued_after_stop.is_none());
        assert_eq!(spawns.get(), 1, "the parked prompt respawned once");
        let Some((new_rx, new_handle)) = spawned.borrow_mut().take() else {
            panic!("the parked prompt did not respawn a worker");
        };
        handle.join().unwrap(); // the old worker retired
        let new_epoch = app.work.run_seq;
        assert_ne!(new_epoch, epoch, "a fresh worker epoch");
        pump(
            app,
            &new_rx,
            new_epoch,
            |e| *e == WorkEvent::TurnComplete,
            cx,
        );
        assert_eq!(app.work.run_seq, new_epoch, "no second respawn");
        assert_eq!(app.work.session_id.as_deref(), Some("s2"));
        assert_eq!(
            transcript(app),
            vec![
                (Role::User, "first".to_string()),
                (Role::Assistant, failure_during_stop_line(&msg)),
                (Role::User, "second".to_string()),
                (Role::Assistant, "second answer".to_string()),
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
            "session_close(s1)",
            "close",
        ]
    );
    let new_ops = fresh.ops();
    assert_eq!(new_ops.len(), 3, "{new_ops:?}");
    assert!(new_ops[0].starts_with("session_create("), "{new_ops:?}");
    assert_eq!(new_ops[1..], ["prompt_submit(s2,second)", "close"]);
}
