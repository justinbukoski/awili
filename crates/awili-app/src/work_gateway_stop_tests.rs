//! P3c round 2 — the gateway Stop lane end to end.
//!
//! R1: the app's own Stop (`cancel_work_run`) on a live gateway turn sends the
//! worker `Cancel` and keeps the sender, `session_id`, run_seq and cancel flag;
//! the worker's later `Terminal { Stopped }` lands under that run_seq without a
//! second ledger line. R2: a drain that does not see the turn's own end closes
//! the session and ends the worker. R3: the drain forwards only `hook.denied`.
//! Round 3 (R7): the drain answers no approval (no RPC can stretch its bound),
//! and every Stop exit that retires the session reports `Stopped` before the
//! close RPC. The app-level queue (R5/R6) lives in
//! `work_gateway_queue_tests.rs`.

use super::gateway_turn_tests::{
    collect_until, complete, delta, prompt, running, spawn_stopped_turn, stopped, tokens,
};
use super::tests::{gw_event, FakeOps};
use super::*;
use serde_json::json;
use std::time::Instant;

/// Long enough that hitting it would be obvious in a test whose end signals
/// arrive first.
const LONG_DRAIN: Duration = Duration::from_secs(5);

/// R1 through the real app: build `AwiliApp`, put a gateway turn in flight on
/// a live worker channel, press Stop, then deliver the worker's
/// acknowledgement. Isolated in a subprocess because `AwiliApp::new` reads
/// config and opens persistence (same pattern as `stream_turn_tests`).
#[gpui::test]
fn ui_stop_on_gateway_lane_sends_cancel_and_keeps_session(cx: &mut gpui::TestAppContext) {
    const CHILD: &str = "AWILI_GATEWAY_STOP_TEST_HOME";
    const TEST: &str =
        "work::gateway_stop_tests::ui_stop_on_gateway_lane_sends_cancel_and_keeps_session";
    if std::env::var_os(CHILD).is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, home.path())
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated Stop test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(TEST) && stdout.contains("1 passed"),
            "subprocess must actually execute the regression: {stdout}"
        );
        return;
    }

    let ledger = std::path::PathBuf::from(std::env::var_os(CHILD).unwrap()).join("ledger.jsonl");
    let ledger_lines = || {
        std::fs::read_to_string(&ledger)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        app.work.gateway_cmd_tx = Some(cmd_tx);
        app.work.session_id = Some("s1".into());
        app.work.worker_identity = Some(WorkRunIdentity::HermesGateway);
        app.work.panel.requested_identity = Some(WorkRunIdentity::HermesGateway);
        app.work.running = true;
        app.work
            .graduation
            .set_ledger(ledger.to_str().expect("utf-8 temp path"));
        let conv = app.work_conv_id();
        app.work.graduation.arm(&conv);
        let seq = app.work.run_seq;
        let cancel = app.work.cancel.clone();

        app.cancel_work_run(cx);

        assert!(matches!(cmd_rx.try_recv(), Ok(GatewayCmd::Cancel)));
        assert!(
            matches!(cmd_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
            "exactly one command, and the sender is still alive"
        );
        assert!(app.work.gateway_cmd_tx.is_some());
        assert_eq!(app.work.session_id.as_deref(), Some("s1"));
        assert_eq!(app.work.run_seq, seq);
        assert!(!cancel.load(Ordering::SeqCst));
        assert!(!app.work.cancel.load(Ordering::SeqCst));
        assert!(!app.work.running);
        assert!(app.work.gateway_stop_pending);
        // P8b: the Stop itself wrote the turn's one ledger line.
        assert!(!app.work.graduation.is_armed());
        assert_eq!(ledger_lines(), 1);

        // The worker's acknowledgement arrives under the unchanged run_seq
        // and goes through the production reducer: no second ledger line.
        app.apply_work_update(
            WorkUpdate::Terminal {
                status: RunStatus::Stopped,
                output: None,
                model: None,
            },
            cx,
        );
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
        assert!(!app.work.gateway_stop_pending);
        assert!(!app.work.running);
        assert!(app.work.gateway_cmd_tx.is_some());
        assert_eq!(app.work.session_id.as_deref(), Some("s1"));
        assert_eq!(
            ledger_lines(),
            1,
            "the acknowledgement must not record again"
        );
    });
}

/// R2: the backend never sends the end signals after the interrupt. The
/// bounded drain closes the session, reports `Stopped`, and the worker
/// returns; its command receiver is already gone when `Stopped` arrives, so a
/// next prompt cannot reach this session (the UI respawns instead).
#[test]
fn gateway_stop_timeout_closes_session_and_worker_exits() {
    let fake = FakeOps::new(vec![vec![running(true), delta("stuck")]]);
    let drain = Duration::from_millis(60);
    let t0 = Instant::now();
    let (cmd_tx, ev_rx, handle) = spawn_stopped_turn(&fake, drain, "t1");
    let events = collect_until(&ev_rx, |e| *e == stopped());
    let waited = t0.elapsed();
    assert!(
        waited >= drain,
        "Stopped before the drain bound: {waited:?}"
    );
    assert!(waited < LONG_DRAIN, "drain overran: {waited:?}");
    assert_eq!(events, vec![WorkEvent::Started("s1".into()), stopped()]);
    assert!(
        cmd_tx.send(prompt("t2")).is_err(),
        "a prompt after Stopped must fail, not queue on a stale session"
    );
    handle.join().unwrap();
    assert!(ev_rx.recv_blocking().is_err(), "nothing after Stopped");
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

/// R2: the transport drops during the drain — `Stopped`, no RPC on the dead
/// link, the worker returns.
#[test]
fn gateway_stop_drain_disconnect_reports_stopped_and_exits() {
    let fake = FakeOps::new(vec![vec![running(true)]]);
    fake.connected.store(false, Ordering::SeqCst);
    let (cmd_tx, ev_rx, handle) = spawn_stopped_turn(&fake, LONG_DRAIN, "t1");
    let events = collect_until(&ev_rx, |e| *e == stopped());
    assert_eq!(events, vec![WorkEvent::Started("s1".into()), stopped()]);
    assert!(cmd_tx.send(prompt("t2")).is_err());
    handle.join().unwrap();
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,t1)",
            "session_interrupt(s1)",
            "close",
        ]
    );
}

/// R3 + round 3 (R7a): during the drain a raced `hook.denied` still reaches
/// the UI (once); a pending `approval.request` is neither answered (no
/// `approval_respond` RPC) nor shown; late text is consumed; the turn's own
/// end keeps the session for the next prompt.
#[test]
fn gateway_stop_drain_does_not_answer_approvals() {
    let mut fake = FakeOps::new(vec![
        vec![running(true)],
        vec![running(true), delta("next"), complete(), running(false)],
    ]);
    fake.interrupt_events = vec![
        gw_event(
            "hook.denied",
            json!({ "source": "authorization", "reason": "denied by policy" }),
        ),
        gw_event(
            "approval.request",
            json!({ "command": "rm -rf build", "approval_id": "a1" }),
        ),
        delta("late"),
        complete(),
        running(false),
    ];
    let (cmd_tx, ev_rx, handle) = spawn_stopped_turn(&fake, LONG_DRAIN, "t1");
    let stop = collect_until(&ev_rx, |e| *e == stopped());
    // Exactly these: no approval card, no text.
    assert_eq!(
        stop,
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
    // Ended: the same session runs the next prompt.
    cmd_tx.send(prompt("t2")).unwrap();
    let turn2 = collect_until(&ev_rx, |e| *e == WorkEvent::TurnComplete);
    assert_eq!(tokens(&turn2), "next");
    drop(cmd_tx);
    handle.join().unwrap();
    let ops = fake.ops();
    assert!(
        !ops.iter().any(|o| o.starts_with("approval_respond")),
        "{ops:?}"
    );
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
}

/// Round 3 (R7b): when a Stop retires the session — the drain timed out, or
/// the interrupt RPC failed — the worker has already dropped its command
/// receiver and sent `Terminal { Stopped }` when `session_close` starts, so a
/// slow close RPC is invisible to the UI.
#[test]
fn gateway_stop_reports_stopped_before_close_rpc() {
    for interrupt_fails in [false, true] {
        let mut fake = FakeOps::new(vec![vec![running(true), delta("stuck")]]);
        fake.fail_interrupt = interrupt_fails;
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        cmd_tx.send(prompt("t1")).unwrap();
        // The Stop lands while `prompt_submit` is in flight (mid-turn).
        fake.send_during("prompt_submit", &cmd_tx, vec![GatewayCmd::Cancel]);
        let (ev_tx, ev_rx) = async_channel::unbounded();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (probe_tx, probe_rx, probe_seen) = (cmd_tx.clone(), ev_rx.clone(), seen.clone());
        fake.close_probe = Some(Arc::new(move || {
            let receiver_dropped = probe_tx.send(prompt("probe")).is_err();
            let mut seen = probe_seen.lock().unwrap();
            while let Ok(ev) = probe_rx.try_recv() {
                seen.push(ev);
            }
            let stopped_sent = seen.last() == Some(&stopped());
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
                Duration::from_millis(40),
            )
        });
        handle.join().unwrap();
        assert!(ev_rx.try_recv().is_err(), "nothing after the close RPC");
        assert_eq!(
            *seen.lock().unwrap(),
            vec![WorkEvent::Started("s1".into()), stopped()]
        );
        assert_eq!(
            fake.ops(),
            vec![
                "session_create(/wd)",
                "prompt_submit(s1,t1)",
                "session_interrupt(s1)",
                "at_close(receiver_dropped=true,stopped_sent=true)",
                "session_close(s1)",
                "close",
            ],
            "interrupt_fails={interrupt_fails}"
        );
    }
}

/// Round 4 (R10): Stop pressed while the worker is blocked in an outstanding
/// `prompt_submit` RPC. The `Cancel` waits behind that RPC (the part of the
/// Stop bound the drain cannot enforce); once it returns, the worker handles
/// the `Cancel` before forwarding anything of the stopped turn, interrupts,
/// drains and reports `Stopped` — keeping the session when the drain ends,
/// and with the R7 ordering (receiver dropped, `Stopped`, then the close RPC)
/// when it times out. Nothing of the stopped turn is forwarded, before or
/// after `Stopped`.
#[test]
fn stop_during_outstanding_prompt_submit_still_reports_stopped() {
    for drain_ends in [true, false] {
        let mut fake = FakeOps::new(vec![vec![running(true), delta("stale")]]);
        fake.submit_delay = Duration::from_millis(300);
        if drain_ends {
            fake.interrupt_events = vec![delta("late"), complete(), running(false)];
        }
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        cmd_tx.send(prompt("t1")).unwrap();
        let (ev_tx, ev_rx) = async_channel::unbounded();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        if !drain_ends {
            // Only a retired session closes; the probe's sender clone would
            // otherwise keep an idle worker's command channel open.
            let (probe_tx, probe_rx, probe_seen) = (cmd_tx.clone(), ev_rx.clone(), seen.clone());
            fake.close_probe = Some(Arc::new(move || {
                let receiver_dropped = probe_tx.send(prompt("probe")).is_err();
                let mut seen = probe_seen.lock().unwrap();
                seen.extend(std::iter::from_fn(|| probe_rx.try_recv().ok()));
                let stopped_sent = seen.last() == Some(&stopped());
                format!("at_close(receiver_dropped={receiver_dropped},stopped_sent={stopped_sent})")
            }));
        }
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
                Duration::from_millis(if drain_ends { 5_000 } else { 40 }),
            )
        });
        // Press Stop while `prompt_submit` is still outstanding.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fake.ops().iter().any(|op| op == "prompt_submit(s1,t1)") {
            assert!(Instant::now() < deadline, "prompt_submit never started");
            std::thread::sleep(Duration::from_millis(1));
        }
        cmd_tx.send(GatewayCmd::Cancel).unwrap();
        assert_eq!(fake.ops().len(), 2, "the Stop queued behind the RPC");

        if drain_ends {
            // The session survives: the worker idles until the UI lets go.
            let got = collect_until(&ev_rx, |e| *e == stopped());
            seen.lock().unwrap().extend(got);
            drop(cmd_tx);
        }
        handle.join().unwrap();
        let mut events = seen.lock().unwrap().clone();
        events.extend(std::iter::from_fn(|| ev_rx.try_recv().ok()));
        assert_eq!(
            events,
            vec![WorkEvent::Started("s1".into()), stopped()],
            "drain_ends={drain_ends}"
        );
        assert_eq!(tokens(&events), "");
        let mut ops = vec![
            "session_create(/wd)",
            "prompt_submit(s1,t1)",
            "session_interrupt(s1)",
        ];
        if !drain_ends {
            ops.extend([
                "at_close(receiver_dropped=true,stopped_sent=true)",
                "session_close(s1)",
            ]);
        }
        ops.push("close");
        assert_eq!(fake.ops(), ops, "drain_ends={drain_ends}");
    }
}
