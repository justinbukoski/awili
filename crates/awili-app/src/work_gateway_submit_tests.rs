//! P3c round 5 — a Stop around the prompt submission.
//!
//! R11: a failed or timed-out `prompt_submit` is uncertain (the backend may
//! have accepted the work), so a Stop queued behind it still interrupts and
//! drains instead of reporting `Failed`; and a Stop that reaches an idle
//! worker interrupts best-effort before retiring. R12: a Stop queued before a
//! prompt is submitted — e.g. while `session_create` is in flight — means the
//! prompt is never submitted.

use super::gateway_turn_tests::{
    collect_until, complete, delta, prompt, running, spawn_loop_during_submit, stopped,
};
use super::tests::{gw_event, FakeOps};
use super::*;
use serde_json::json;

/// Long enough that hitting it would be obvious in a test whose end signals
/// arrive first.
const LONG_DRAIN: Duration = Duration::from_secs(5);

/// R11: `prompt_submit` "accepts" the work (its turn's events are queued) and
/// then times out. With a Stop queued behind the RPC the worker interrupts,
/// drains the late events (forwarding only the raced denial), reports
/// `Stopped` and NO `Failed`, and keeps the session; without a Stop it is
/// `Failed` exactly as before and nothing is interrupted.
#[test]
fn stop_during_failed_prompt_submit_still_interrupts_and_drains() {
    for stop_queued in [true, false] {
        let mut fake = FakeOps::new(vec![vec![running(true), delta("accepted anyway")]]);
        fake.fail_submit = true;
        fake.interrupt_events = vec![
            gw_event(
                "hook.denied",
                json!({ "source": "authorization", "reason": "denied by policy" }),
            ),
            delta("late"),
            complete(),
            running(false),
        ];
        let during = if stop_queued {
            vec![GatewayCmd::Cancel]
        } else {
            Vec::new()
        };
        let (cmd_tx, ev_rx, handle) =
            spawn_loop_during_submit(&fake, LONG_DRAIN, vec![prompt("t1")], during);
        let events = collect_until(&ev_rx, |e| {
            *e == stopped() || matches!(e, WorkEvent::Failed(_))
        });
        // The idle worker ends when the UI lets go.
        drop(cmd_tx);
        handle.join().unwrap();
        let rest: Vec<_> = std::iter::from_fn(|| ev_rx.try_recv().ok()).collect();
        assert!(rest.is_empty(), "nothing after the report: {rest:?}");
        if stop_queued {
            assert_eq!(
                events,
                vec![
                    WorkEvent::Started("s1".into()),
                    WorkEvent::HookDenial {
                        source: "authorization".into(),
                        reason: "denied by policy".into(),
                        recovery: None,
                    },
                    stopped(),
                ]
            );
            assert!(
                fake.live.lock().unwrap().is_empty(),
                "the drain consumed the accepted turn's late events"
            );
            assert_eq!(
                fake.ops(),
                vec![
                    "session_create(/wd)",
                    "prompt_submit(s1,t1)",
                    "session_interrupt(s1)",
                    "close",
                ]
            );
        } else {
            assert_eq!(events.len(), 2, "{events:?}");
            assert!(
                matches!(&events[1], WorkEvent::Failed(m) if m.starts_with("prompt.submit failed")),
                "{events:?}"
            );
            assert_eq!(
                fake.ops(),
                vec!["session_create(/wd)", "prompt_submit(s1,t1)", "close"]
            );
        }
    }
}

/// R11: a Stop that reaches the worker after its turn already ended (the
/// outer loop's idle `Cancel`) calls `session_interrupt` first; then the
/// worker retires as before — receiver dropped and `Stopped` reported before
/// the close RPC starts.
#[test]
fn idle_cancel_interrupts_before_retiring() {
    let mut fake = FakeOps::new(vec![vec![
        running(true),
        delta("done"),
        complete(),
        running(false),
    ]]);
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, ev_rx) = async_channel::unbounded();
    let (probe_tx, probe_rx) = (cmd_tx.clone(), ev_rx.clone());
    fake.close_probe = Some(Arc::new(move || {
        let receiver_dropped = probe_tx.send(prompt("probe")).is_err();
        let stopped_sent = probe_rx.try_recv().ok() == Some(stopped());
        format!("at_close(receiver_dropped={receiver_dropped},stopped_sent={stopped_sent})")
    }));
    let worker = fake.clone();
    let handle = std::thread::spawn(move || {
        run_worker_loop_with_drain(
            worker,
            WorkerContext {
                workdir: "/wd".into(),
                cmd_rx,
                cancel: Arc::new(AtomicBool::new(false)),
                events: ev_tx,
            },
            LONG_DRAIN,
        )
    });
    cmd_tx.send(prompt("t1")).unwrap();
    collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    // The turn is over: this Stop reaches the idle worker.
    cmd_tx.send(GatewayCmd::Cancel).unwrap();
    handle.join().unwrap();
    assert!(ev_rx.try_recv().is_err(), "Stopped was the last event");
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,t1)",
            "session_interrupt(s1)",
            "at_close(receiver_dropped=true,stopped_sent=true)",
            "session_close(s1)",
            "close",
        ]
    );
}

/// R12: the UI sends the first prompt and then Stop while `session_create`
/// is still in flight. The worker never submits the prompt and interrupts
/// nothing (nothing reached the backend); it drops its command receiver,
/// reports `Stopped`, then closes the session.
#[test]
fn stop_queued_before_prompt_is_submitted_never_submits() {
    let fake = FakeOps::new(vec![vec![running(true), delta("never")]]);
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    fake.send_during(
        "session_create",
        &cmd_tx,
        vec![prompt("t1"), GatewayCmd::Cancel],
    );
    let (ev_tx, ev_rx) = async_channel::unbounded();
    let worker = fake.clone();
    let handle = std::thread::spawn(move || {
        run_worker_loop_with_drain(
            worker,
            WorkerContext {
                workdir: "/wd".into(),
                cmd_rx,
                cancel: Arc::new(AtomicBool::new(false)),
                events: ev_tx,
            },
            LONG_DRAIN,
        )
    });
    let events = collect_until(&ev_rx, |e| *e == stopped());
    // Checked the moment `Stopped` is seen: the receiver was dropped first.
    assert!(
        cmd_tx.send(prompt("t2")).is_err(),
        "the receiver is gone before Stopped is reported"
    );
    handle.join().unwrap();
    assert_eq!(events, vec![WorkEvent::Started("s1".into()), stopped()]);
    assert!(ev_rx.try_recv().is_err(), "nothing after Stopped");
    let ops = fake.ops();
    assert!(
        !ops.iter().any(|o| o.starts_with("prompt_submit")),
        "{ops:?}"
    );
    assert_eq!(
        ops,
        vec!["session_create(/wd)", "session_close(s1)", "close"]
    );
}
