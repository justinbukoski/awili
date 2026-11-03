//! M1a (S2-F01) — a gateway Stop the worker never acknowledges must not park
//! Work sends forever. Over the real `AwiliApp` and the real worker loop,
//! with the round-3 harness of `work_gateway_queue_tests.rs` and the
//! `TEST_WORKER_SPAWN` respawn hook of `work_gateway_barrier_tests.rs`: the
//! interrupt RPC hangs behind a `Latch`, the deadline fires
//! (`expire_gateway_stop`, called directly with the generation the Stop
//! produced — under `#[gpui::test]` the clock never advances) and the
//! abandoned worker is replaced by a fresh scripted session that replays the
//! parked prompt exactly once.

use super::gateway_queue_tests::{isolated, pump, transcript, wire_gateway, DRAIN};
use super::gateway_turn_tests::{complete, delta, running};
use super::tests::{FakeOps, Latch};
use super::*;
use crate::gateway_stop_deadline::{ABANDONED_TOAST, ALREADY_PARKED_TOAST, PARKED_TOAST};
use std::cell::RefCell;
use std::rc::Rc;
use std::thread::JoinHandle;

/// The respawned worker's events and thread, stashed by the spawn hook.
type Spawned = (async_channel::Receiver<WorkEvent>, JoinHandle<()>);

/// One cut-off turn whose interrupt RPC hangs until the test opens `latch`;
/// the drain then sees the turn's own end signals.
fn hanging_interrupt_fake(latch: &Latch) -> FakeOps {
    let mut fake = FakeOps::new(vec![vec![running(true), delta("t1 ")]]);
    fake.interrupt_latch = Some(latch.clone());
    fake.interrupt_events = vec![complete(), running(false)];
    fake
}

/// A fresh session's scripted turn: one answer and the turn's end.
fn fresh_fake(session: &str) -> FakeOps {
    let mut fresh = FakeOps::new(vec![vec![
        running(true),
        delta("fresh answer"),
        complete(),
        running(false),
    ]]);
    fresh.session_name = session.into();
    fresh
}

/// The one-shot replacement for the next `spawn_engine_worker`: a real
/// `run_worker_loop_with_drain` over `fresh`, whose receiver and thread are
/// stashed in `spawned` (the barrier-test pattern).
fn install_spawn_hook(fresh: FakeOps, spawned: &Rc<RefCell<Option<Spawned>>>) {
    let (slot, worker) = (spawned.clone(), fresh.clone());
    TEST_WORKER_SPAWN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |ctx: WorkerContext| {
            let (ev_tx, ev_rx) = async_channel::unbounded();
            let ctx = WorkerContext {
                events: ev_tx,
                ..ctx
            };
            let handle = std::thread::spawn(move || run_worker_loop_with_drain(worker, ctx, DRAIN));
            *slot.borrow_mut() = Some((ev_rx, handle));
        }));
    });
}

/// The parked slot's text, for readability.
fn parked(app: &AwiliApp) -> Option<String> {
    app.work.queued_after_stop.as_ref().map(|q| q.text.clone())
}

/// S2-F01 (1): the interrupt RPC hangs. The armed deadline abandons the old
/// worker (Shutdown, sender dropped, session forgotten, run_seq bumped,
/// `Terminal { Stopped }` through the reducer) and the parked prompt replays
/// exactly once on a fresh session — the old worker never sees it, even after
/// its hung RPC finally returns.
#[gpui::test]
fn m1a_unacked_stop_expires_and_replays_the_parked_prompt(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gateway_stop_deadline_tests::m1a_unacked_stop_expires_and_replays_the_parked_prompt",
    ) else {
        return;
    };
    let latch = Latch::default();
    let fake = hanging_interrupt_fake(&latch);
    let fresh = fresh_fake("s2");
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
        assert_eq!(app.work.gateway_stop_gen, 1);

        app.submit_work_run("second".into(), cx);
        assert_eq!(parked(app).as_deref(), Some("second"));
        assert_eq!(
            app.toast.visible().map(ToString::to_string),
            Some(PARKED_TOAST.to_string())
        );

        // The deadline fires for the Stop it was armed for.
        let spawned: Rc<RefCell<Option<Spawned>>> = Rc::default();
        install_spawn_hook(fresh.clone(), &spawned);
        app.expire_gateway_stop(1, cx);

        assert!(!app.work.gateway_stop_pending);
        assert!(app.work.queued_after_stop.is_none());
        assert!(app.work.running, "the parked prompt is running");
        assert_ne!(app.work.run_seq, epoch, "the old worker's epoch is dead");
        assert!(app.work.gateway_cmd_tx.is_some());
        assert_eq!(
            app.toast.visible().map(ToString::to_string),
            Some(ABANDONED_TOAST.to_string())
        );
        let Some((new_rx, new_handle)) = spawned.borrow_mut().take() else {
            panic!("abandonment did not respawn a worker");
        };

        pump(
            app,
            &new_rx,
            app.work.run_seq,
            |e| *e == WorkEvent::TurnComplete,
            cx,
        );
        assert_eq!(
            transcript(app),
            vec![
                (Role::User, "first".to_string()),
                (Role::Assistant, "t1 ".to_string()),
                (Role::User, "second".to_string()),
                (Role::Assistant, "fresh answer".to_string()),
            ]
        );
        assert_eq!(app.work.session_id.as_deref(), Some("s2"));
        // The Stop once, the fresh turn once — the replay is not double-recorded.
        let ledger = std::fs::read_to_string(home.join("ledger.jsonl")).unwrap();
        assert_eq!(
            ledger.lines().count(),
            2,
            "ledger must hold exactly the abandoned Stop and the fresh turn:\n{ledger}"
        );

        // Now the hung interrupt returns: the old worker's ack must not reach
        // the new turn (run_seq moved) and the old worker must close itself.
        latch.open();
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
        new_handle.join().unwrap();
    });
    assert!(
        fresh
            .ops()
            .contains(&"prompt_submit(s2,second)".to_string()),
        "the parked prompt ran on the fresh session: {:?}",
        fresh.ops()
    );
    assert!(
        !fake.ops().contains(&"prompt_submit(s1,second)".to_string()),
        "the old session never saw the parked prompt: {:?}",
        fake.ops()
    );
}

/// S2-F01 (2): the Stop is acknowledged before the deadline. The deadline
/// callback is a no-op — the session survives and the same session runs the
/// replayed prompt (the R5 path).
#[gpui::test]
fn m1a_ack_before_deadline_makes_expiry_a_noop(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::gateway_stop_deadline_tests::m1a_ack_before_deadline_makes_expiry_a_noop")
    else {
        return;
    };
    // The interrupt RPC hangs behind the latch until the test opens it, then
    // the drain sees the turn's own end signals (the R5 path: the session
    // survives and the same worker runs the replayed prompt).
    let latch = Latch::default();
    let mut fake = FakeOps::new(vec![
        vec![running(true), delta("t1 ")],
        vec![
            running(true),
            delta("new answer"),
            complete(),
            running(false),
        ],
    ]);
    fake.interrupt_latch = Some(latch.clone());
    fake.interrupt_events = vec![complete(), running(false)];
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
        // The interrupt RPC returns as soon as the test opens the latch, so
        // the drain sees the turn's end signals and the session survives.
        latch.open();
        app.cancel_work_run(cx);
        assert!(app.work.gateway_stop_pending);
        app.submit_work_run("second".into(), cx);
        assert_eq!(parked(app).as_deref(), Some("second"));

        // The acknowledgement arrives first: the barrier lifts and the parked
        // prompt replays on the same worker.
        pump(
            app,
            &ev_rx,
            epoch,
            |e| matches!(e, WorkEvent::Terminal { .. }),
            cx,
        );
        // The replayed prompt is a `Prompt` command sent to the live worker;
        // the worker's `prompt_submit` records the op before streaming the
        // next turn's events. The first `Token` proves the worker has
        // processed the prompt.
        pump(app, &ev_rx, epoch, |e| matches!(e, WorkEvent::Token(_)), cx);
        assert!(!app.work.gateway_stop_pending);
        assert!(app.work.queued_after_stop.is_none());
        assert!(app.work.running);
        assert_eq!(
            app.work.run_seq,
            epoch,
            "no respawn (ops: {:?})",
            fake.ops()
        );
        assert_eq!(
            app.work.session_id.as_deref(),
            Some("s1"),
            "the acked Stop keeps the session"
        );
        assert!(app.work.gateway_cmd_tx.is_some());
        // The replay is a prompt into the SAME live worker, not a respawn.
        assert!(
            fake.ops().contains(&"prompt_submit(s1,second)".to_string()),
            "ops: {:?}",
            fake.ops()
        );

        // The deadline fires late: everything the ack already settled.
        app.expire_gateway_stop(1, cx);
        assert_eq!(app.work.run_seq, epoch, "the late deadline must be a no-op");
        assert_eq!(app.work.session_id.as_deref(), Some("s1"));
        assert!(app.work.gateway_cmd_tx.is_some(), "the live worker is kept");
        assert_ne!(
            app.toast.visible().map(ToString::to_string),
            Some(ABANDONED_TOAST.to_string())
        );

        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
    // One session for both prompts, closed at the end.
    assert!(
        fake.ops()
            .ends_with(&["prompt_submit(s1,second)".to_string(), "close".to_string()]),
        "the replay ran on the same session: {:?}",
        fake.ops()
    );
}

/// S2-F01 (3): one prompt parked at a time. The second submit while one is
/// parked goes back to the composer with a toast; the first keeps its place
/// and is the one replayed.
#[gpui::test]
fn m1a_second_submit_while_parked_returns_to_composer(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gateway_stop_deadline_tests::m1a_second_submit_while_parked_returns_to_composer",
    ) else {
        return;
    };
    let latch = Latch::default();
    let fake = hanging_interrupt_fake(&latch);
    let fresh = fresh_fake("s2");
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, _handle, epoch) = wire_gateway(app, &home, &fake, cx);
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

        app.submit_work_run("a".into(), cx);
        assert_eq!(
            app.toast.visible().map(ToString::to_string),
            Some(PARKED_TOAST.to_string())
        );
        app.submit_work_run("b".into(), cx);
        assert_eq!(
            parked(app).as_deref(),
            Some("a"),
            "the first parked prompt keeps its place"
        );
        assert_eq!(
            app.composer.read(cx).text(),
            "b",
            "the refused submit goes back to the composer"
        );
        assert_eq!(
            app.toast.visible().map(ToString::to_string),
            Some(ALREADY_PARKED_TOAST.to_string())
        );
        assert_eq!(
            transcript(app).len(),
            2,
            "neither parked prompt is in the transcript yet"
        );

        // The deadline fires; the FIRST parked prompt ("a") is the one replayed.
        let spawned: Rc<RefCell<Option<Spawned>>> = Rc::default();
        install_spawn_hook(fresh.clone(), &spawned);
        app.expire_gateway_stop(1, cx);
        assert!(app.work.queued_after_stop.is_none());
        let Some((new_rx, new_handle)) = spawned.borrow_mut().take() else {
            panic!("abandonment did not respawn a worker");
        };
        pump(
            app,
            &new_rx,
            app.work.run_seq,
            |e| *e == WorkEvent::TurnComplete,
            cx,
        );
        assert_eq!(transcript(app)[2], (Role::User, "a".to_string()));

        latch.open();
        app.work.gateway_cmd_tx = None;
        _handle.join().unwrap();
        new_handle.join().unwrap();
    });
}

/// S2-F01 (4): the deadline is generation-guarded. A stale timer (from a Stop
/// that is no longer the pending one) is a no-op; only the current Stop's own
/// timer can act.
#[gpui::test]
fn m1a_stale_generation_is_ignored(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::gateway_stop_deadline_tests::m1a_stale_generation_is_ignored")
    else {
        return;
    };
    let latch = Latch::default();
    let fake = hanging_interrupt_fake(&latch);
    let fresh = fresh_fake("s2");
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
        assert_eq!(app.work.gateway_stop_gen, 1);

        // A stale generation (a newer Stop would have bumped it, say) must not
        // act even though a Stop is pending.
        app.expire_gateway_stop(0, cx);
        assert!(
            app.work.gateway_stop_pending,
            "the stale timer must be a no-op"
        );
        assert!(app.work.gateway_cmd_tx.is_some(), "the live worker is kept");
        assert_eq!(app.work.run_seq, epoch, "no respawn from the stale timer");

        // The Stop's own timer fires and abandons it; the parked prompt
        // replays on a fresh worker.
        app.submit_work_run("second".into(), cx);
        let spawned: Rc<RefCell<Option<Spawned>>> = Rc::default();
        install_spawn_hook(fresh.clone(), &spawned);
        app.expire_gateway_stop(1, cx);
        assert!(!app.work.gateway_stop_pending);
        assert_ne!(app.work.run_seq, epoch);
        let Some((_new_rx, new_handle)) = spawned.borrow_mut().take() else {
            panic!("the current generation did not respawn a worker");
        };
        assert!(app.work.running);

        latch.open();
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
        new_handle.join().unwrap();
    });
}
