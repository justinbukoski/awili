//! P3c — gateway turn end and Stop-as-interrupt, pinned through the scripted
//! [`GatewayOps`] fake (`work::tests::FakeOps`).
//!
//! A turn ends only on the gateway crate's `is_turn_end` conjunction
//! (`message.complete` AND `session.info running:false`, either order; `error`
//! alone). A mid-turn Stop sends `session.interrupt`, drains the interrupted
//! turn (bounded), reports `Terminal Stopped`, and keeps the session for the
//! next prompt; only a failed interrupt falls back to `session.close`. The
//! round-2 Stop cases (timeout, drain evidence, the app's own Stop) live in
//! `work_gateway_stop_tests.rs`, which reuses the helpers here.

use super::tests::{drive_loop, gw_event, FakeOps};
use super::*;
use serde_json::json;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Instant;

/// A drain bound long enough that hitting it would be obvious in a test that
/// expects the end signals to arrive first.
const LONG_DRAIN: Duration = Duration::from_secs(5);

/// A `session.info` event with `running: on`.
pub(super) fn running(on: bool) -> GatewayEvent {
    gw_event("session.info", json!({ "running": on }))
}

/// A `message.delta` carrying `text`.
pub(super) fn delta(text: &str) -> GatewayEvent {
    gw_event("message.delta", json!({ "text": text }))
}

/// A final `message.complete`.
pub(super) fn complete() -> GatewayEvent {
    gw_event("message.complete", json!({ "status": "complete" }))
}

/// A `Prompt` command with no system preamble.
pub(super) fn prompt(text: &str) -> GatewayCmd {
    GatewayCmd::Prompt {
        text: text.into(),
        system: None,
    }
}

/// The `Terminal { Stopped }` every gateway Stop reports.
pub(super) fn stopped() -> WorkEvent {
    WorkEvent::Terminal {
        status: RunStatus::Stopped,
        output: None,
        model: None,
    }
}

/// The forwarded text of `events`, concatenated.
pub(super) fn tokens(events: &[WorkEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::Token(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn count_turn_complete(events: &[WorkEvent]) -> usize {
    events
        .iter()
        .filter(|e| **e == WorkEvent::TurnComplete)
        .count()
}

/// A running worker loop: its command sender, event receiver and thread.
pub(super) type Spawned = (
    Sender<GatewayCmd>,
    async_channel::Receiver<WorkEvent>,
    JoinHandle<()>,
);

/// Run the loop on a thread with an injected Stop drain. `queued` commands are
/// in the channel before the loop starts; later commands are sent live so a
/// second prompt is only submitted after the first turn ends.
pub(super) fn spawn_loop(fake: &FakeOps, drain: Duration, queued: Vec<GatewayCmd>) -> Spawned {
    spawn_loop_during_submit(fake, drain, queued, Vec::new())
}

/// A mid-turn Stop, deterministically: `first` is queued and the UI's
/// `Cancel` lands while its `prompt_submit` RPC is in flight (a Stop queued
/// BEFORE the submission never submits — round 5, R12).
pub(super) fn spawn_stopped_turn(fake: &FakeOps, drain: Duration, first: &str) -> Spawned {
    spawn_loop_during_submit(fake, drain, vec![prompt(first)], vec![GatewayCmd::Cancel])
}

/// [`spawn_loop`], plus `during_submit` sent from inside the first
/// `prompt_submit` (see `FakeOps::send_during`).
pub(super) fn spawn_loop_during_submit(
    fake: &FakeOps,
    drain: Duration,
    queued: Vec<GatewayCmd>,
    during_submit: Vec<GatewayCmd>,
) -> Spawned {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    if !during_submit.is_empty() {
        fake.send_during("prompt_submit", &cmd_tx, during_submit);
    }
    for c in queued {
        cmd_tx.send(c).unwrap();
    }
    let (ev_tx, ev_rx) = async_channel::unbounded();
    let fake = fake.clone();
    let handle = std::thread::spawn(move || {
        run_worker_loop_with_drain(
            fake,
            WorkerContext {
                workdir: "/wd".into(),
                cmd_rx,
                cancel: Arc::new(AtomicBool::new(false)),
                events: ev_tx,
            },
            drain,
        )
    });
    (cmd_tx, ev_rx, handle)
}

/// Collect events until (and including) the first one matching `done`.
pub(super) fn collect_until(
    rx: &async_channel::Receiver<WorkEvent>,
    done: impl Fn(&WorkEvent) -> bool,
) -> Vec<WorkEvent> {
    let mut out = Vec::new();
    loop {
        match rx.recv_blocking() {
            Ok(ev) => {
                let stop = done(&ev);
                out.push(ev);
                if stop {
                    return out;
                }
            }
            Err(e) => panic!("worker exited early: {e}; got {out:?}"),
        }
    }
}

/// D3: a tool-call `message.complete` does NOT end the turn; the one
/// `TurnComplete` comes after the final `running:false`, and every scripted
/// event was consumed by this turn.
#[test]
fn gateway_turn_ends_only_on_complete_and_running_false() {
    let fake = FakeOps::new(vec![vec![
        running(true),
        gw_event("message.complete", json!({ "status": "complete" })),
        gw_event("tool.complete", json!({ "name": "read_file" })),
        delta("final"),
        complete(),
        running(false),
    ]]);
    let events = drive_loop(&fake, vec![prompt("t1")]);
    assert_eq!(count_turn_complete(&events), 1, "{events:?}");
    assert_eq!(events.last(), Some(&WorkEvent::TurnComplete), "{events:?}");
    // The final answer and both completions were forwarded BEFORE the end.
    assert_eq!(tokens(&events), "final");
    let completions = events
        .iter()
        .filter(|e| matches!(e, WorkEvent::TurnCompleted { .. }))
        .count();
    assert_eq!(completions, 2, "{events:?}");
    assert!(events
        .iter()
        .any(|e| matches!(e, WorkEvent::ToolFinished { .. })));
    assert!(
        fake.live.lock().unwrap().is_empty(),
        "turn left events queued"
    );
    assert_eq!(
        fake.ops(),
        vec!["session_create(/wd)", "prompt_submit(s1,t1)", "close"]
    );
}

/// The D3 reproduction: under the old first-`message.complete` rule, turn 1's
/// tool result + final answer stayed queued and ended turn 2 at its start.
/// Now turn 1 consumes them and turn 2 streams exactly its own text.
#[test]
fn gateway_second_turn_does_not_consume_previous_turn_events() {
    let fake = FakeOps::new(vec![
        vec![
            running(true),
            complete(), // tool-call message: old rule ended turn 1 here
            gw_event("tool.complete", json!({ "name": "search" })),
            delta("turn-one-answer"),
            complete(),
            running(false),
        ],
        vec![running(true), delta("x"), complete(), running(false)],
    ]);
    let (cmd_tx, ev_rx, handle) = spawn_loop(&fake, LONG_DRAIN, vec![prompt("first")]);
    let turn1 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    assert_eq!(tokens(&turn1), "turn-one-answer");
    cmd_tx.send(prompt("second")).unwrap();
    let turn2 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    drop(cmd_tx);
    handle.join().unwrap();
    assert_eq!(tokens(&turn2), "x", "turn 2 text: {turn2:?}");
    assert_eq!(count_turn_complete(&turn2), 1, "{turn2:?}");
    assert!(
        matches!(
            turn2.iter().rev().nth(1),
            Some(WorkEvent::TurnCompleted { .. })
        ),
        "turn 2 ends on its own completion: {turn2:?}"
    );
    let mut rest = Vec::new();
    while let Ok(ev) = ev_rx.try_recv() {
        rest.push(ev);
    }
    assert!(rest.is_empty(), "nothing after turn 2: {rest:?}");
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

/// An `error` event is terminal on its own (no `running:false` needed): its
/// `Failed` precedes the single `TurnComplete`, and the session survives for
/// the next prompt.
#[test]
fn gateway_error_event_ends_turn() {
    let fake = FakeOps::new(vec![
        vec![
            running(true),
            gw_event("error", json!({ "message": "boom" })),
        ],
        vec![running(true), delta("ok"), complete(), running(false)],
    ]);
    let (cmd_tx, ev_rx, handle) = spawn_loop(&fake, LONG_DRAIN, vec![prompt("t1")]);
    let turn1 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    assert_eq!(
        turn1,
        vec![
            WorkEvent::Started("s1".into()),
            WorkEvent::Failed("boom".into()),
            WorkEvent::TurnComplete,
        ]
    );
    cmd_tx.send(prompt("t2")).unwrap();
    let turn2 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    drop(cmd_tx);
    handle.join().unwrap();
    assert_eq!(tokens(&turn2), "ok");
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,t1)",
            "prompt_submit(s1,t2)",
            "close",
        ]
    );
}

/// D4: Stop mid-turn interrupts (never closes), waits for the backend's
/// [`message.complete`, `running:false`] drain, reports `Terminal Stopped`,
/// forwards nothing from the interrupted turn, and the SAME session runs the
/// next prompt to a full turn.
#[test]
fn gateway_stop_interrupts_and_keeps_session() {
    let mut fake = FakeOps::new(vec![
        vec![running(true), delta("partial")],
        vec![running(true), delta("again"), complete(), running(false)],
    ]);
    fake.interrupt_events = vec![
        gw_event("message.complete", json!({ "status": "interrupted" })),
        running(false),
    ];
    // The Cancel lands while `prompt_submit` is in flight: it is seen
    // mid-turn, before any event of the turn is read.
    let t0 = Instant::now();
    let (cmd_tx, ev_rx, handle) = spawn_stopped_turn(&fake, LONG_DRAIN, "t1");
    let stop = collect_until(&ev_rx, |e| *e == stopped());
    // `Stopped` came from the end signals, not from the drain bound...
    assert!(
        t0.elapsed() < LONG_DRAIN / 2,
        "drain waited {:?}",
        t0.elapsed()
    );
    // ...and only after the backend's drain was fully consumed.
    assert!(
        fake.live.lock().unwrap().is_empty(),
        "Stopped sent before the interrupted turn drained"
    );
    // Nothing from the interrupted turn reached the UI.
    assert_eq!(
        stop,
        vec![WorkEvent::Started("s1".into()), stopped()],
        "{stop:?}"
    );
    // The worker is still alive: a following prompt runs a full turn.
    cmd_tx.send(prompt("t2")).unwrap();
    let turn2 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    assert_eq!(tokens(&turn2), "again");
    assert_eq!(count_turn_complete(&turn2), 1);
    drop(cmd_tx);
    handle.join().unwrap();
    let ops = fake.ops();
    assert_eq!(
        ops,
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,t1)",
            "session_interrupt(s1)",
            "prompt_submit(s1,t2)",
            "close",
        ]
    );
    assert_eq!(
        ops.iter()
            .filter(|o| o.starts_with("session_interrupt"))
            .count(),
        1
    );
    assert!(!ops.iter().any(|o| o.starts_with("session_close")));
}

/// `session.interrupt` fails: fall back to exactly the pre-P3c Stop —
/// `session_close`, `Terminal Stopped`, client closed, worker returns (a
/// queued follow-up prompt is never submitted).
#[test]
fn gateway_stop_falls_back_to_close_when_interrupt_fails() {
    let mut fake = FakeOps::new(vec![vec![running(true), delta("partial")]]);
    fake.fail_interrupt = true;
    let (cmd_tx, ev_rx, handle) = spawn_loop_during_submit(
        &fake,
        LONG_DRAIN,
        vec![prompt("t1")],
        vec![GatewayCmd::Cancel, prompt("t2-never")],
    );
    handle.join().unwrap();
    assert!(cmd_tx.send(prompt("t3")).is_err(), "the worker is gone");
    let events: Vec<_> = std::iter::from_fn(|| ev_rx.try_recv().ok()).collect();
    assert_eq!(events, vec![WorkEvent::Started("s1".into()), stopped()]);
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,t1)",
            "session_interrupt(s1)",
            "session_close(s1)",
            "close",
        ]
    );
}

/// Either order of the two completion signals ends the turn once, and a
/// `running:false` that arrives before the final `message.complete` does not
/// end the UI turn early (the loop owns turn end).
#[test]
fn gateway_turn_end_running_false_first_ends_once() {
    let fake = FakeOps::new(vec![vec![
        running(true),
        delta("a"),
        running(false),
        delta("b"),
        complete(),
    ]]);
    let events = drive_loop(&fake, vec![prompt("t1")]);
    assert_eq!(count_turn_complete(&events), 1, "{events:?}");
    assert_eq!(events.last(), Some(&WorkEvent::TurnComplete));
    assert_eq!(tokens(&events), "ab");
}
