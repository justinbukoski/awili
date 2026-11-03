//! P3d round 2 — the Stop's deny keeps its place in the in-order command
//! queue (P3c R11). A command already ahead of the Stop runs first, so each
//! one adds an RPC bound to the Stop's latency (Codex review 1, F1). A deny
//! whose reply completes an already half-met turn end lets the turn end
//! normally, and the queued `Cancel` then takes the idle path (F2). End to
//! end over the real `AwiliApp` and the real worker loop over the scripted
//! fake, with the P3d harness in `work_gateway_approval_stop_tests.rs`.

use super::gateway_approval_stop_tests::{
    approval_request, fake_turns, finish_stop_and_continue, kept_session_tail,
};
use super::gateway_queue_tests::{isolated, pump, wire_gateway};
use super::gateway_turn_tests::{complete, delta, running, stopped};
use super::tests::{gw_event, FakeOps, Latch};
use super::*;
use serde_json::json;
use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Instant;

/// Wait until the fake has recorded `op` (the worker is inside that RPC).
fn wait_for_op(fake: &FakeOps, op: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fake.ops().iter().any(|o| o == op) {
        assert!(Instant::now() < deadline, "{op} never started");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Round 2, R1 (Codex review 1, F1): a command already ahead of the Stop in
/// the FIFO runs first. The card is showing; the backend's `terminal.read`
/// makes the UI send its `TerminalReadRespond`, and that RPC is still
/// outstanding (latched) when Stop is pressed. The deny and the `Cancel`
/// queue behind it: nothing is denied until it returns, then the fake
/// records terminal_read_respond → approval_respond(deny) →
/// session_interrupt, and the drain acknowledges with `Terminal { Stopped }`.
/// Each command ahead of the Stop adds one RPC bound to the Stop's latency.
#[gpui::test]
fn stop_with_queued_terminal_read_ahead_of_deny_keeps_order(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gateway_approval_order_tests::stop_with_queued_terminal_read_ahead_of_deny_keeps_order",
    ) else {
        return;
    };
    let read = gw_event("terminal.read", json!({ "request_id": "tr-1" }));
    let mut fake = fake_turns(Some(None), vec![read]);
    let latch = Latch::default();
    fake.terminal_read_latch = Some(latch.clone());
    let entity = cx.new(AwiliApp::new);
    let seen = entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("first".into(), cx);
        pump(
            app,
            &ev_rx,
            epoch,
            |e| matches!(e, WorkEvent::ApprovalRequested { .. }),
            cx,
        );
        // The production reducer answers the read on the same channel.
        pump(
            app,
            &ev_rx,
            epoch,
            |e| matches!(e, WorkEvent::TerminalReadRequest { .. }),
            cx,
        );
        wait_for_op(&fake, "terminal_read_respond(s1,tr-1,)");
        assert!(app.work.panel.approval.is_some(), "the card is showing");

        app.cancel_work_run(cx);
        // The worker is blocked inside the read: the deny and the Cancel
        // wait behind it on the channel.
        let ops = fake.ops();
        assert!(
            !ops.iter()
                .any(|o| o.starts_with("approval_respond") || o.starts_with("session_interrupt")),
            "{ops:?}"
        );
        latch.open();
        finish_stop_and_continue(app, &fake, &ev_rx, handle, epoch, cx)
    });
    assert_eq!(
        seen,
        vec![
            WorkEvent::ApprovalDelivery {
                id: None,
                delivered: true,
            },
            stopped(),
        ]
    );
    let mut expected = vec![
        "session_create(/wd)",
        "prompt_submit(s1,first)",
        "terminal_read_respond(s1,tr-1,)",
        "approval_respond(s1,deny)",
    ];
    expected.extend(kept_session_tail());
    assert_eq!(fake.ops(), expected);
}

/// Round 2, R2 (Codex review 1, F2): the turn's `running:false` was already
/// consumed when Stop is pressed, and the backend answers the deny with the
/// `message.complete` that completes the conjunction. The worker's one poll
/// between the deny and the `Cancel` ends the turn normally (one
/// `TurnComplete`), so the queued `Cancel` reaches the idle outer loop:
/// interrupt best-effort, receiver dropped, `Terminal { Stopped }`, then the
/// close RPC — the session is retired, not kept.
#[gpui::test]
fn deny_completing_the_turn_routes_cancel_to_idle_path(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gateway_approval_order_tests::deny_completing_the_turn_routes_cancel_to_idle_path",
    ) else {
        return;
    };
    let mut fake = FakeOps::new(vec![vec![
        running(true),
        delta("t1 "),
        running(false),
        approval_request(None),
    ]]);
    fake.approval_events = vec![complete()];
    // Filled with the UI's sender once the worker is wired; the close probe
    // sends on it to see whether the worker dropped its receiver first.
    type Slot = Arc<Mutex<Option<std::sync::mpsc::Sender<GatewayCmd>>>>;
    let slot: Slot = Arc::default();
    let probe = slot.clone();
    fake.close_probe = Some(Arc::new(move || {
        let dropped = probe
            .lock()
            .unwrap()
            .take()
            .is_some_and(|tx| tx.send(GatewayCmd::Shutdown).is_err());
        format!("at_close(receiver_dropped={dropped})")
    }));
    let entity = cx.new(AwiliApp::new);
    let seen = entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        *slot.lock().unwrap() = app.work.gateway_cmd_tx.clone();
        app.submit_work_run("first".into(), cx);
        pump(
            app,
            &ev_rx,
            epoch,
            |e| matches!(e, WorkEvent::ApprovalRequested { .. }),
            cx,
        );
        app.cancel_work_run(cx);
        assert!(app.work.gateway_stop_pending);
        let seen = RefCell::new(Vec::new());
        pump(
            app,
            &ev_rx,
            epoch,
            |e| {
                seen.borrow_mut().push(e.clone());
                *e == stopped()
            },
            cx,
        );
        handle.join().unwrap(); // the worker retired
        assert!(ev_rx.try_recv().is_err(), "Stopped was the last event");
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
        assert!(!app.work.gateway_stop_pending);
        assert!(app.work.panel.approval.is_none());
        seen.into_inner()
    });
    assert_eq!(
        seen.iter()
            .filter(|e| **e == WorkEvent::TurnComplete)
            .count(),
        1,
        "{seen:?}"
    );
    assert_eq!(
        seen,
        vec![
            WorkEvent::ApprovalDelivery {
                id: None,
                delivered: true,
            },
            // The post-deny `message.complete`, forwarded, then the loop's
            // own turn end.
            WorkEvent::TurnCompleted {
                status: RunStatus::Completed,
                output: None,
                cost_usd: None,
                context_percent: None,
            },
            WorkEvent::TurnComplete,
            stopped(),
        ]
    );
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,first)",
            "approval_respond(s1,deny)",
            "session_interrupt(s1)",
            "at_close(receiver_dropped=true)",
            "session_close(s1)",
            "close",
        ]
    );
}
