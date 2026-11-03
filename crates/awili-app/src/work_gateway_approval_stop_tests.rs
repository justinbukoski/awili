//! P3d — a Stop on the gateway lane answers the approval card that was
//! showing with `deny` BEFORE the interrupt, so the backend can end the turn
//! and the drain sees the real completion (the session survives). Without a
//! card nothing is answered. End to end over the real `AwiliApp`
//! (`submit_work_run`, `cancel_work_run`, the production reducer) and the real
//! worker loop over the scripted fake, with the P3c round-3 harness.

use super::gateway_queue_tests::{isolated, pump, wire_gateway};
use super::gateway_turn_tests::{complete, delta, running, stopped};
use super::tests::{gw_event, FakeOps};
use super::*;
use serde_json::json;
use std::cell::RefCell;
use std::thread::JoinHandle;

/// An `approval.request` as the gateway sends it; `id` is the backend's
/// approval id when it reports one (Hermes does not).
pub(super) fn approval_request(id: Option<&str>) -> GatewayEvent {
    let mut payload = json!({ "command": "rm -rf build", "description": "delete build" });
    if let Some(id) = id {
        payload["approval_id"] = json!(id);
    }
    gw_event("approval.request", payload)
}

/// A fake whose first turn stops on an approval request (`Some(id)`, the
/// card's backend id if any) or, with `None`, on a plain token, followed by
/// `extra`; the interrupt yields the turn's own end signals, and a second
/// turn runs to completion.
pub(super) fn fake_turns(card: Option<Option<&str>>, extra: Vec<GatewayEvent>) -> FakeOps {
    let mut first = match card {
        Some(id) => vec![running(true), delta("t1 "), approval_request(id)],
        None => vec![running(true), delta("t1 ")],
    };
    first.extend(extra);
    let mut fake = FakeOps::new(vec![
        first,
        vec![running(true), delta("next"), complete(), running(false)],
    ]);
    fake.interrupt_events = vec![
        gw_event("message.complete", json!({ "status": "interrupted" })),
        running(false),
    ];
    fake
}

/// Drive `first` → (card or token visible) → Stop → the acknowledgement →
/// `second` on the same session. Returns the worker events the UI applied
/// from the Stop to the acknowledgement, in order.
fn stop_then_continue(
    app: &mut AwiliApp,
    home: &std::path::Path,
    fake: &FakeOps,
    card_id: Option<Option<&str>>,
    cx: &mut Context<AwiliApp>,
) -> Vec<WorkEvent> {
    let (ev_rx, handle, epoch) = wire_gateway(app, home, fake, cx);
    app.submit_work_run("first".into(), cx);
    let showing = |e: &WorkEvent| match card_id {
        Some(_) => matches!(e, WorkEvent::ApprovalRequested { .. }),
        None => *e == WorkEvent::Token("t1 ".into()),
    };
    pump(app, &ev_rx, epoch, showing, cx);
    match card_id {
        Some(id) => assert_eq!(
            app.work
                .panel
                .approval
                .as_ref()
                .map(|c| c.approval_id.as_str()),
            Some(id.unwrap_or("")),
            "the card is showing"
        ),
        None => assert!(app.work.panel.approval.is_none()),
    }

    app.cancel_work_run(cx);
    finish_stop_and_continue(app, fake, &ev_rx, handle, epoch, cx)
}

/// From just after the Stop press: the card is cleared, the Stop is pending;
/// apply the worker's events up to the acknowledgement (session kept), then
/// run `second` on the same session. Returns the events from the Stop to the
/// acknowledgement, in order.
pub(super) fn finish_stop_and_continue(
    app: &mut AwiliApp,
    fake: &FakeOps,
    ev_rx: &async_channel::Receiver<WorkEvent>,
    handle: JoinHandle<()>,
    epoch: u64,
    cx: &mut Context<AwiliApp>,
) -> Vec<WorkEvent> {
    assert!(app.work.panel.approval.is_none(), "Stop clears the card");
    assert!(app.work.gateway_stop_pending);
    assert!(!app.work.running);

    let seen = RefCell::new(Vec::new());
    pump(
        app,
        ev_rx,
        epoch,
        |e| {
            seen.borrow_mut().push(e.clone());
            *e == stopped()
        },
        cx,
    );
    // The drain saw the turn's own end: the session and worker are kept.
    assert!(
        fake.live.lock().unwrap().is_empty(),
        "the interrupted turn drained"
    );
    assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
    assert!(app.work.panel.approval.is_none());
    assert!(!app.work.gateway_stop_pending);
    assert!(app.work.gateway_cmd_tx.is_some(), "the worker is kept");
    assert_eq!(app.work.session_id.as_deref(), Some("s1"));
    assert_eq!(app.work.run_seq, epoch, "no respawn");

    app.submit_work_run("second".into(), cx);
    pump(app, ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
    assert_eq!(app.work.session_id.as_deref(), Some("s1"));
    app.work.gateway_cmd_tx = None;
    handle.join().unwrap();
    seen.into_inner()
}

/// The ops every case shares after the Stop: the interrupt, then the second
/// prompt on the SAME session (the drain `Ended`), never a close RPC.
pub(super) fn kept_session_tail() -> Vec<&'static str> {
    vec!["session_interrupt(s1)", "prompt_submit(s1,second)", "close"]
}

/// P3d scope 1: a card is showing → Stop → `approval_respond(s1,deny)` is
/// recorded BEFORE `session_interrupt(s1)`; the card is cleared; the drain
/// sees [`message.complete`, `running:false`] → `Terminal { Stopped }`, and
/// the session is kept.
#[gpui::test]
fn stop_with_pending_approval_denies_it_before_interrupt(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gateway_approval_stop_tests::stop_with_pending_approval_denies_it_before_interrupt",
    ) else {
        return;
    };
    let fake = fake_turns(Some(Some("appr-7")), vec![]);
    let entity = cx.new(AwiliApp::new);
    let seen = entity.update(cx, |app, cx| {
        stop_then_continue(app, &home, &fake, Some(Some("appr-7")), cx)
    });
    assert_eq!(
        seen,
        vec![
            WorkEvent::ApprovalDelivery {
                id: Some("appr-7".into()),
                delivered: true,
            },
            stopped(),
        ],
        "the deny's delivery report, then the acknowledgement; nothing of the stopped turn"
    );
    let mut expected = vec![
        "session_create(/wd)",
        "prompt_submit(s1,first)",
        "approval_respond(s1,deny)",
    ];
    expected.extend(kept_session_tail());
    assert_eq!(fake.ops(), expected);
}

/// Regression guard: a Stop with no card showing answers no approval — the
/// P3c Stop path exactly (interrupt, drain, session kept).
#[gpui::test]
fn stop_without_pending_approval_sends_no_approval_response(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gateway_approval_stop_tests::stop_without_pending_approval_sends_no_approval_response",
    ) else {
        return;
    };
    let fake = fake_turns(None, vec![]);
    let entity = cx.new(AwiliApp::new);
    let seen = entity.update(cx, |app, cx| {
        stop_then_continue(app, &home, &fake, None, cx)
    });
    assert_eq!(seen, vec![stopped()]);
    let ops = fake.ops();
    assert!(
        !ops.iter().any(|o| o.starts_with("approval_respond")),
        "{ops:?}"
    );
    let mut expected = vec!["session_create(/wd)", "prompt_submit(s1,first)"];
    expected.extend(kept_session_tail());
    assert_eq!(ops, expected);
}

/// The deny RPC fails (and the card is id-less, as Hermes sends it): the
/// worker reports the failed delivery (the UI has already cleared the card,
/// so the report changes nothing) and the `Cancel` still runs the interrupt
/// path → `Terminal { Stopped }`, session kept.
#[gpui::test]
fn stop_with_pending_approval_and_failed_respond_still_interrupts(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gateway_approval_stop_tests::stop_with_pending_approval_and_failed_respond_still_interrupts",
    ) else {
        return;
    };
    let mut fake = fake_turns(Some(None), vec![]);
    fake.fail_approval = true;
    let entity = cx.new(AwiliApp::new);
    let seen = entity.update(cx, |app, cx| {
        stop_then_continue(app, &home, &fake, Some(None), cx)
    });
    assert_eq!(
        seen,
        vec![
            WorkEvent::ApprovalDelivery {
                id: None,
                delivered: false,
            },
            stopped(),
        ]
    );
    let mut expected = vec![
        "session_create(/wd)",
        "prompt_submit(s1,first)",
        "approval_respond(s1,deny)",
    ];
    expected.extend(kept_session_tail());
    assert_eq!(fake.ops(), expected);
}
