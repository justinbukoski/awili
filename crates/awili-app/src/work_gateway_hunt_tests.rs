//! P3c race hunt: gateway Work-lane interleavings rounds 1-6 did not pin. A
//! test with a `P3c-HUNT:` note reproduced a defect the hunt found; P3g fixed
//! all four, so none is `#[ignore]`d any more.

use super::gateway_queue_tests::{
    denial_event, isolated, pump, transcript, wire_gateway, CONV, DRAIN,
};
use super::gateway_turn_tests::{
    collect_until, complete, delta, prompt, running, spawn_loop, spawn_loop_during_submit, stopped,
    tokens,
};
use super::tests::{gw_event, FakeOps};
use super::*;
use async_channel::{Receiver, Sender};
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

fn answer(text: &str) -> Vec<GatewayEvent> {
    vec![running(true), delta(text), complete(), running(false)]
}

/// Wire `app` to a worker over `fake`, submit "first", let it stream, Stop.
fn submit_and_stop(
    app: &mut AwiliApp,
    home: &std::path::Path,
    fake: &FakeOps,
    cx: &mut Context<AwiliApp>,
) -> (Receiver<WorkEvent>, std::thread::JoinHandle<()>, u64) {
    let (ev_rx, handle, epoch) = wire_gateway(app, home, fake, cx);
    app.submit_work_run("first".into(), cx);
    pump(app, &ev_rx, epoch, |e| matches!(e, WorkEvent::Token(_)), cx);
    app.cancel_work_run(cx);
    assert!(app.work.gateway_stop_pending);
    (ev_rx, handle, epoch)
}

/// Worker event channels [`settle`] relays into the production consumer on
/// the test thread (the gpui test scheduler rejects foreign-thread wake-ups).
type Relays = Rc<RefCell<Vec<(Receiver<WorkEvent>, Option<Sender<WorkEvent>>)>>>;

/// Take over the next worker spawn: run `body` on its own thread behind a relay.
fn on_spawn(relays: &Relays, body: impl FnOnce(WorkerContext) + Send + 'static) {
    let relays = relays.clone();
    TEST_WORKER_SPAWN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |ctx: WorkerContext| {
            let (tx, rx) = async_channel::unbounded();
            relays.borrow_mut().push((rx, Some(ctx.events.clone())));
            let ctx = WorkerContext { events: tx, ..ctx };
            std::thread::spawn(move || body(ctx));
        }));
    });
}

/// The next spawn runs the real loop over `fake` (session `s2`) behind a relay.
fn respawn_s2(relays: &Relays, text: &str) -> FakeOps {
    let mut fake = FakeOps::new(vec![answer(text)]);
    fake.session_name = "s2".into();
    let worker = fake.clone();
    on_spawn(relays, move |ctx| {
        run_worker_loop_with_drain(worker, ctx, DRAIN)
    });
    fake
}

/// Relay events and run the production consumer (with its dead-worker
/// fallback) until `done`; a relay closes once its worker's sender is gone.
fn settle(
    cx: &mut gpui::TestAppContext,
    app: &gpui::Entity<AwiliApp>,
    relays: &Relays,
    done: impl Fn(&AwiliApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for (rx, tx) in relays.borrow_mut().iter_mut() {
            while let Ok(ev) = rx.try_recv() {
                let _ = tx.as_ref().map(|tx| tx.try_send(ev));
            }
            if rx.is_closed() && rx.is_empty() {
                *tx = None;
            }
        }
        cx.run_until_parked();
        if app.read_with(cx, |a, _| done(a)) {
            return;
        }
        assert!(Instant::now() < deadline, "the app never settled");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Wait until a worker over `fake` has exited (closed its transport).
fn wait_closed(fake: &FakeOps) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while fake.ops().last().map(String::as_str) != Some("close") {
        assert!(Instant::now() < deadline, "{:?}", fake.ops());
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// [`isolated`] for `name`; an `ignored` repro's child gets `--include-ignored`.
fn iso(name: &str, ignored: bool) -> Option<std::path::PathBuf> {
    let test = format!("work::gateway_hunt_tests::{name}");
    if !ignored || std::env::var_os("AWILI_GATEWAY_QUEUE_TEST_HOME").is_some() {
        return isolated(&test);
    }
    let home = tempfile::tempdir().unwrap();
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", &test, "--nocapture", "--include-ignored"]);
    for (key, sub) in [
        ("AWILI_GATEWAY_QUEUE_TEST_HOME", ""),
        ("HOME", ""),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CACHE_HOME", "cache"),
    ] {
        cmd.env(key, home.path().join(sub));
    }
    let out = cmd.output().unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), &out.stderr);
    let ok = out.status.success() && stdout.contains("1 passed");
    assert!(ok, "{stdout}\n{}", String::from_utf8_lossy(stderr));
    None
}

/// Busy, a tool-call `message.complete`, then the tool's run and result.
/// P3g: the tool's `tool.start` is on the wire (as from a real backend) — it is
/// what tells the turn-end fold that the completion before it was the
/// tool-call message's, not the turn's.
fn tool_call_opening() -> Vec<GatewayEvent> {
    let start = gw_event("tool.start", json!({ "name": "search" }));
    let tool = gw_event("tool.complete", json!({ "name": "search" }));
    vec![running(true), complete(), start, tool]
}

// P3c-HUNT (fixed in P3g, scope 2): Stop drain ends early on a pre-Stop tool-call completion; the rest leaks into turn 2.
// The mid-turn Cancel arm hands its TurnState to the drain (work.rs:9468), so
// an interrupt answered [running:false, message.complete] ends at running:false.
#[test]
fn stop_drain_waits_for_the_interrupted_turns_own_completion() {
    let mut fake = FakeOps::new(vec![tool_call_opening(), answer("x")]);
    fake.interrupt_events = vec![
        running(false),
        gw_event("message.complete", json!({ "status": "interrupted" })),
    ];
    let (cmd_tx, ev_rx, handle) = spawn_loop(&fake, DRAIN, vec![prompt("t1")]);
    collect_until(&ev_rx, |e| matches!(e, WorkEvent::ToolFinished { .. }));
    cmd_tx.send(GatewayCmd::Cancel).unwrap();
    collect_until(&ev_rx, |e| *e == stopped());
    cmd_tx.send(prompt("t2")).unwrap();
    let turn2 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    drop(cmd_tx);
    handle.join().unwrap();
    assert_eq!(tokens(&turn2), "x");
    let completions = format!("{turn2:?}").matches("TurnCompleted").count();
    assert_eq!(
        completions, 1,
        "the stopped turn's completion leaked: {turn2:?}"
    );
}

/// P3g round 2 (Codex P2): pins scope 2 on its own. A pre-Stop
/// `message.complete` with NO later `message.start` / `tool.start` is still
/// live in the interrupted turn's [`TurnState`] (scope 1 never clears it), so
/// a drain that reused that state would end at the interrupt's `running:false`
/// and leak the interrupted completion into turn 2. Only a FRESH drain state
/// (`stop_turn_in_flight`) waits for the post-interrupt completion.
#[test]
fn stop_drain_ignores_pre_stop_completion_without_later_start() {
    let opening = vec![running(true), delta("p"), complete()];
    let mut fake = FakeOps::new(vec![opening, answer("x")]);
    fake.interrupt_events = vec![
        running(false),
        gw_event("message.complete", json!({ "status": "interrupted" })),
    ];
    let (cmd_tx, ev_rx, handle) = spawn_loop(&fake, DRAIN, vec![prompt("t1")]);
    // The loop folds an event before its next command check, so once the
    // completion is seen here it is in the interrupted turn's state.
    collect_until(&ev_rx, |e| matches!(e, WorkEvent::TurnCompleted { .. }));
    cmd_tx.send(GatewayCmd::Cancel).unwrap();
    collect_until(&ev_rx, |e| *e == stopped());
    cmd_tx.send(prompt("t2")).unwrap();
    let turn2 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    drop(cmd_tx);
    handle.join().unwrap();
    assert_eq!(tokens(&turn2), "x");
    let completions = format!("{turn2:?}").matches("TurnCompleted").count();
    assert_eq!(
        completions, 1,
        "the stopped turn's completion leaked: {turn2:?}"
    );
}

// P3c-HUNT (fixed in P3g, scope 1): a tool-call turn whose running:false precedes the final completion ends early (D3).
// The tool-call `message.complete` stays set in TurnState (events.rs:499), and
// the spec allows either order, so the final text leaks into the next turn.
#[test]
fn tool_call_turn_with_running_false_first_ends_on_its_own_completion() {
    let mut turn1 = tool_call_opening();
    turn1.extend([delta("a"), running(false), delta("b"), complete()]);
    let fake = FakeOps::new(vec![turn1, answer("x")]);
    let (cmd_tx, ev_rx, handle) = spawn_loop(&fake, DRAIN, vec![prompt("t1")]);
    let turn1 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    cmd_tx.send(prompt("t2")).unwrap();
    let turn2 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    drop(cmd_tx);
    handle.join().unwrap();
    assert_eq!(tokens(&turn1), "ab", "turn 1 ended early: {turn1:?}");
    assert_eq!(tokens(&turn2), "x", "turn 2: {turn2:?}");
}

/// Stop, Stop, prompt queued mid-turn: one `Stopped` per Stop (an `Ended`
/// drain, then the idle Stop interrupts and retires); the prompt queued behind
/// the retiring Stop is never submitted.
#[test]
fn two_stops_then_a_prompt_mid_turn() {
    let mut fake = FakeOps::new(vec![vec![running(true), delta("p")]]);
    fake.interrupt_events = vec![complete(), running(false)];
    let cmds = vec![GatewayCmd::Cancel, GatewayCmd::Cancel, prompt("late")];
    let (cmd_tx, ev_rx, handle) = spawn_loop_during_submit(&fake, DRAIN, vec![prompt("t1")], cmds);
    handle.join().unwrap();
    assert!(cmd_tx.send(prompt("t3")).is_err(), "the worker retired");
    let events: Vec<_> = std::iter::from_fn(|| ev_rx.try_recv().ok()).collect();
    let want = [WorkEvent::Started("s1".into()), stopped(), stopped()];
    assert_eq!(events, want);
    let ops = fake.ops().join(" ");
    let want = "prompt_submit(s1,t1) session_interrupt(s1) session_interrupt(s1) session_close(s1)";
    assert!(ops.ends_with(&format!("{want} close")), "{ops}");
}

/// The stopped turn's conversation is deleted during the drain (its raced
/// denial has no row left): a prompt parked from ANOTHER conversation still
/// replays once, on the kept session, into its own conversation.
#[gpui::test]
fn deleted_stopped_conv_parked_elsewhere_replays(cx: &mut gpui::TestAppContext) {
    let Some(home) = iso("deleted_stopped_conv_parked_elsewhere_replays", false) else {
        return;
    };
    let mut fake = FakeOps::new(vec![vec![running(true), delta("t1 ")], answer("ans")]);
    fake.interrupt_events = vec![denial_event(), complete(), running(false)];
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = submit_and_stop(app, &home, &fake, cx);
        let other = "other-conv".to_string();
        app.views.insert(
            other.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(&other);
        app.submit_work_run("second".into(), cx);
        app.conversations.remove(CONV); // what `delete_conversation` does
        app.views.remove(CONV);
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        assert!(!app.work.gateway_stop_pending);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        let msgs = app.views[&other].convo.messages();
        let texts: Vec<&str> = msgs.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["second", "ans"], "nothing misrouted here");
        assert_eq!(app.work.conv_id.as_deref(), Some(other.as_str()));
        assert_eq!(app.work.session_id.as_deref(), Some("s1"));
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

// P3c-HUNT (fixed in P3g, scope 3): a prompt parked in a conversation deleted before the ack still runs, invisibly.
// replay_queued_after_stop (work.rs:3891) -> submit_work_run_in (work.rs:2505)
// never checks that the conversation still exists.
#[gpui::test]
fn parked_prompt_of_deleted_conv_is_not_run(cx: &mut gpui::TestAppContext) {
    let Some(home) = iso("parked_prompt_of_deleted_conv_is_not_run", false) else {
        return;
    };
    let mut fake = FakeOps::new(vec![vec![running(true), delta("t1 ")], answer("ghost")]);
    fake.interrupt_events = vec![complete(), running(false)];
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = submit_and_stop(app, &home, &fake, cx);
        app.submit_work_run("second".into(), cx);
        app.conversations.remove(CONV);
        app.views.remove(CONV);
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        assert!(!app.work.gateway_stop_pending);
        assert!(!app.work.running, "a turn runs for a deleted conversation");
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
    assert!(!fake.ops().contains(&"prompt_submit(s1,second)".to_string()));
}

// P3c-HUNT (fixed in P3g, scope 4): a buffered approval.request reopens the stopped turn's card; a click answers it.
// The Event reducer (work.rs:2930 -> apply_event, work.rs:692) ignores the stop
// barrier; the verdict reaches the backend after the drain (R7a says never).
#[gpui::test]
fn buffered_approval_does_not_reopen_stopped_gate(cx: &mut gpui::TestAppContext) {
    let Some(home) = iso("buffered_approval_does_not_reopen_stopped_gate", false) else {
        return;
    };
    let ask = json!({ "command": "rm", "approval_id": "a1" });
    let mut fake = FakeOps::new(vec![vec![running(true), gw_event("approval.request", ask)]]);
    fake.interrupt_events = vec![complete(), running(false)];
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("first".into(), cx);
        let held = collect_until(&ev_rx, |e| matches!(e, WorkEvent::ApprovalRequested { .. }));
        app.cancel_work_run(cx);
        assert!(app.work.panel.approval.is_none());
        for ev in held {
            for up in work_event_to_updates(ev) {
                app.apply_work_update(up, cx);
            }
        }
        app.resolve_work_approval(ApprovalDecision::Approve, cx);
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
    let ops = fake.ops().join(" ");
    assert!(!ops.contains("approval_respond"), "{ops}");
}

/// A worker dies with a Stop pending and a prompt parked: its `session.create`
/// fails (`Failed`, then return — emulated in the spawn hook) or it panics.
/// Only the consumer's dead-worker fallback acknowledges: the `Failed` is
/// evidence (R13), the barrier ends, the prompt respawns exactly once.
#[gpui::test]
fn dead_worker_create_failed_replays_parked_once(cx: &mut gpui::TestAppContext) {
    if let Some(home) = iso("dead_worker_create_failed_replays_parked_once", false) {
        dead_worker_case(cx, &home, false);
    }
}

/// [`dead_worker_create_failed_replays_parked_once`], panicking.
#[gpui::test]
fn dead_worker_panic_replays_parked_once(cx: &mut gpui::TestAppContext) {
    if let Some(home) = iso("dead_worker_panic_replays_parked_once", false) {
        dead_worker_case(cx, &home, true);
    }
}

fn dead_worker_case(cx: &mut gpui::TestAppContext, home: &std::path::Path, panics: bool) {
    let relays = Relays::default();
    let entity = cx.new(AwiliApp::new);
    let fresh = entity.update(cx, |app, cx| {
        let (_, handle, _) = wire_gateway(app, home, &FakeOps::new(vec![]), cx);
        app.work.gateway_cmd_tx = None; // start with no worker
        handle.join().unwrap();
        app.work.session_id = None;
        on_spawn(&relays, move |ctx| {
            if panics {
                let _ = ctx.events.send_blocking(WorkEvent::Started("s1".into()));
            }
            // Die once the UI's Stop has reached the channel.
            while !matches!(ctx.cmd_rx.recv(), Ok(GatewayCmd::Cancel) | Err(_)) {}
            assert!(!panics, "P3c hunt: the gateway worker died mid-turn");
            let failed = WorkEvent::Failed("create failed".into());
            let _ = ctx.events.send_blocking(failed);
        });
        app.submit_work_run("first".into(), cx);
        app.cancel_work_run(cx);
        assert!(app.work.gateway_stop_pending, "the Stop reached the worker");
        app.submit_work_run("second".into(), cx);
        respawn_s2(&relays, "ans")
    });
    settle(cx, &entity, &relays, |a| {
        !a.work.gateway_stop_pending && !a.work.running && a.work.session_id.is_some()
    });
    entity.update(cx, |app, _| {
        assert!(app.work.queued_after_stop.is_none());
        assert_eq!(app.work.session_id.as_deref(), Some("s2"));
        let t = transcript(app);
        assert_eq!(t.len(), 4, "{t:?}");
        assert!(panics || t[1].1.contains("create failed"), "{t:?}");
        assert_eq!(t[2].1, "second");
        assert_eq!(t[3].1, "ans");
        app.work.gateway_cmd_tx = None;
    });
    assert_eq!(relays.borrow().len(), 2, "exactly one respawn");
    wait_closed(&fresh);
    assert_eq!(fresh.ops()[1..], ["prompt_submit(s2,second)", "close"]);
}

/// Folder change during a Stop whose drain TIMES OUT: the old worker retires
/// itself, so the ack's deferred `Shutdown` meets a dead channel; the parked
/// prompt respawns once in the new folder; the old session closes once.
#[gpui::test]
fn folder_change_timeout_respawns_in_new_folder(cx: &mut gpui::TestAppContext) {
    let Some(home) = iso("folder_change_timeout_respawns_in_new_folder", false) else {
        return;
    };
    let fake = FakeOps::new(vec![vec![running(true), delta("t1 ")]]);
    let new_dir = home.join("new-folder");
    std::fs::create_dir_all(&new_dir).unwrap();
    let relays = Relays::default();
    let entity = cx.new(AwiliApp::new);
    let fresh = entity.update(cx, |app, cx| {
        // A short drain: this Stop's end signals never come.
        let (cmd_tx, ev_rx, handle) = spawn_loop(&fake, Duration::from_millis(50), vec![]);
        let (_, old_handle, epoch) = wire_gateway(app, &home, &FakeOps::new(vec![]), cx);
        app.work.gateway_cmd_tx = Some(cmd_tx);
        old_handle.join().unwrap();
        let started = |e: &WorkEvent| matches!(e, WorkEvent::Started(_));
        pump(app, &ev_rx, epoch, started, cx);
        app.submit_work_run("first".into(), cx);
        pump(app, &ev_rx, epoch, |e| matches!(e, WorkEvent::Token(_)), cx);
        app.cancel_work_run(cx);
        app.set_work_folder(new_dir.clone(), cx);
        app.submit_work_run("second".into(), cx);
        let fresh = respawn_s2(&relays, "n");
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        assert!(!app.work.gateway_stop_pending);
        assert!(!app.work.folder_changed_during_stop);
        handle.join().unwrap();
        assert_eq!(relays.borrow().len(), 1, "the parked prompt respawned once");
        app.work.gateway_cmd_tx = None;
        fresh
    });
    wait_closed(&fresh);
    let ops = fake.ops().join(" ");
    assert_eq!(ops.matches("session_close").count(), 1, "{ops}");
    let create = format!("session_create({})", new_dir.display());
    assert_eq!(fresh.ops()[0], create);
    assert_eq!(fresh.ops()[1], "prompt_submit(s2,second)");
}
