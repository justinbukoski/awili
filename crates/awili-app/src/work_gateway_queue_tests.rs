//! P3c round 3 — a prompt submitted while a gateway Stop is pending is parked
//! and replayed once the Stop is acknowledged (R5), and a repeated Stop keeps
//! the session (R6). End to end: the real `AwiliApp::submit_work_run` /
//! `cancel_work_run` and the production reducer on one side, the real
//! `run_worker_loop_with_drain` over the scripted fake on the other. `pump`
//! does what the production event consumer does (the run_seq gate,
//! `work_event_to_updates`, `apply_work_update`).

use super::gateway_turn_tests::{complete, delta, running, spawn_loop, stopped};
use super::tests::{gw_event, FakeOps};
use super::*;
use serde_json::json;
use std::thread::JoinHandle;
use std::time::Instant;

const CHILD: &str = "AWILI_GATEWAY_QUEUE_TEST_HOME";
pub(super) const CONV: &str = "work-conv";
/// Long enough that hitting it would be obvious: every drain here ends on the
/// interrupted turn's own end signals.
pub(super) const DRAIN: Duration = Duration::from_secs(5);

/// Run `test` in a subprocess with a private HOME (`AwiliApp::new` reads
/// config and opens persistence — same isolation as `stream_turn_tests`).
/// Returns the private home inside the child; `None` in the parent once the
/// child has passed.
pub(super) fn isolated(test: &str) -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os(CHILD) {
        return Some(home.into());
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        // RC1-M-FIX6: the child runs this one test alone, so it may set
        // process-wide test state (`agent_launch::set_test_paths`).
        .env(
            crate::test_child::OWN_PROCESS,
            format!("{test}@{}", std::process::id()),
        )
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "subprocess must actually execute the regression: {stdout}"
    );
    None
}

/// Apply the worker's events to `app` until (and including) the first one
/// matching `done`, dropping any that arrive after run_seq moved off `epoch`.
pub(super) fn pump(
    app: &mut AwiliApp,
    rx: &async_channel::Receiver<WorkEvent>,
    epoch: u64,
    done: impl Fn(&WorkEvent) -> bool,
    cx: &mut Context<AwiliApp>,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.try_recv() {
            Ok(ev) => {
                let stop = done(&ev);
                if app.work.run_seq == epoch {
                    for up in work_event_to_updates(ev) {
                        app.apply_work_update(up, cx);
                    }
                }
                if stop {
                    return;
                }
            }
            Err(_) => {
                assert!(Instant::now() < deadline, "worker event never arrived");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// Make `app` a configured gateway Work session driven by a real worker loop
/// over `fake`, with its `Started` applied. Returns the worker's events, its
/// thread and the run_seq its events are accepted under.
pub(super) fn wire_gateway(
    app: &mut AwiliApp,
    home: &std::path::Path,
    fake: &FakeOps,
    cx: &mut Context<AwiliApp>,
) -> (async_channel::Receiver<WorkEvent>, JoinHandle<()>, u64) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.work.gateway.enabled = true;
    app.work.engine.kind = awili_config::EngineKind::HermesGateway;
    app.work.executor = WorkExecutor::Gateway;
    app.work
        .graduation
        .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
    app.views.insert(
        CONV.into(),
        crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
    );
    crate::exec_fixture::seed_conversation_metadata(app, CONV, awili_core::ConvKind::Work);
    app.set_active_conversation(CONV);
    let (cmd_tx, ev_rx, handle) = spawn_loop(fake, DRAIN, vec![]);
    app.work.gateway_cmd_tx = Some(cmd_tx);
    app.work.worker_identity = Some(WorkRunIdentity::HermesGateway);
    let epoch = app.work.run_seq;
    pump(
        app,
        &ev_rx,
        epoch,
        |e| matches!(e, WorkEvent::Started(_)),
        cx,
    );
    assert_eq!(app.work.session_id.as_deref(), Some("s1"));
    (ev_rx, handle, epoch)
}

/// The Work transcript as `(role, text)` pairs.
pub(super) fn transcript(app: &AwiliApp) -> Vec<(Role, String)> {
    app.views[CONV]
        .convo
        .messages()
        .iter()
        .map(|m| (m.role, m.text.clone()))
        .collect()
}

pub(super) fn denial_event() -> GatewayEvent {
    gw_event(
        "hook.denied",
        json!({ "source": "authorization", "reason": "denied by policy" }),
    )
}

pub(super) const DENIAL_TEXT: &str =
    "Blocked by authorization: denied by policy\nNo automatic recovery supplied\n";

/// R5: Stop → submit → the old worker's drain forwards `hook.denied` →
/// `Terminal { Stopped }` → the parked prompt is submitted exactly once, on
/// the same worker and session, and the denial is in the stopped turn's row
/// before the new turn's text.
#[gpui::test]
fn submit_during_pending_stop_is_queued_then_runs(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::gateway_queue_tests::submit_during_pending_stop_is_queued_then_runs")
    else {
        return;
    };
    let mut fake = FakeOps::new(vec![
        vec![running(true), delta("t1 ")],
        vec![
            running(true),
            delta("new answer"),
            complete(),
            running(false),
        ],
    ]);
    fake.interrupt_events = vec![denial_event(), delta("late"), complete(), running(false)];
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
        app.submit_work_run("second".into(), cx);
        // Parked: nothing reached the worker or the transcript yet.
        assert_eq!(
            app.work.queued_after_stop.as_ref().map(|q| q.text.as_str()),
            Some("second")
        );
        assert_eq!(
            app.work.panel.activity.as_deref(),
            Some(GATEWAY_STOPPING_ACTIVITY)
        );
        assert!(!app.work.running);
        assert_eq!(transcript(app).len(), 2);

        // The acknowledgement finishes the stopped turn, then replays once.
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        assert!(!app.work.gateway_stop_pending);
        assert!(app.work.queued_after_stop.is_none());
        assert!(app.work.running, "the parked prompt is running");
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);

        assert_eq!(
            transcript(app),
            vec![
                (Role::User, "first".to_string()),
                (Role::Assistant, format!("t1 {DENIAL_TEXT}")),
                (Role::User, "second".to_string()),
                (Role::Assistant, "new answer".to_string()),
            ]
        );
        assert_eq!(app.work.run_seq, epoch, "no respawn");
        assert_eq!(app.work.session_id.as_deref(), Some("s1"));
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,first)",
            "session_interrupt(s1)",
            "prompt_submit(s1,second)",
            "close",
        ]
    );
}

/// The queued prompt was accepted under Gateway. Choosing Claude while its
/// Stop is pending is only the next turn: replay must still use Gateway and
/// leave Claude selected after the replay starts.
#[gpui::test]
fn m2_queued_replay_keeps_accepted_executor_and_preserves_next_target(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::gateway_queue_tests::m2_queued_replay_keeps_accepted_executor_and_preserves_next_target",
    ) else {
        return;
    };
    let mut fake = FakeOps::new(vec![
        vec![running(true), delta("first")],
        vec![running(true), delta("queued"), complete(), running(false)],
    ]);
    fake.interrupt_events = vec![complete(), running(false)];
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("first".into(), cx);
        pump(
            app,
            &ev_rx,
            epoch,
            |e| *e == WorkEvent::Token("first".into()),
            cx,
        );
        app.cancel_work_run(cx);
        app.submit_work_run("queued".into(), cx);
        let next = WorkExecutor::Agent {
            engine: awili_code::CodeEngine::Claude,
            model: Some("sonnet".into()),
        };
        app.work
            .conversation_routes
            .get_mut(CONV)
            .expect("wired conversation has a canonical route")
            .set_next_target(next.clone());

        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        assert!(app.work.running, "the accepted queued prompt started");
        assert_eq!(
            app.work.executor,
            WorkExecutor::Gateway,
            "active approval/Stop ownership stays with the accepted target"
        );
        assert_eq!(
            app.work.conversation_routes[CONV].next_target(),
            &next,
            "the later chooser value remains next"
        );
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

/// R6: Stop, Stop (a no-op while the first is pending), a prompt parked
/// behind the Stop, then another prompt: one `Cancel` reaches the worker, the
/// flag is false after the acknowledgement, and both prompts run on the SAME
/// session.
#[gpui::test]
fn double_stop_then_two_prompts_keep_the_session(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::gateway_queue_tests::double_stop_then_two_prompts_keep_the_session")
    else {
        return;
    };
    let mut fake = FakeOps::new(vec![
        vec![running(true), delta("t1")],
        vec![running(true), delta("a"), complete(), running(false)],
        vec![running(true), delta("b"), complete(), running(false)],
    ]);
    fake.interrupt_events = vec![complete(), running(false)];
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("t1".into(), cx);
        pump(
            app,
            &ev_rx,
            epoch,
            |e| *e == WorkEvent::Token("t1".into()),
            cx,
        );

        app.cancel_work_run(cx);
        let cancel = app.work.cancel.clone();
        app.cancel_work_run(cx);
        assert!(
            app.work.gateway_stop_pending,
            "the first Stop is still pending"
        );
        assert!(app.work.gateway_cmd_tx.is_some());
        assert_eq!(app.work.session_id.as_deref(), Some("s1"));
        assert_eq!(app.work.run_seq, epoch);
        assert!(!cancel.load(Ordering::SeqCst));

        app.submit_work_run("pa".into(), cx);
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        assert!(!app.work.gateway_stop_pending);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        app.submit_work_run("pb".into(), cx);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);

        assert!(!app.work.gateway_stop_pending);
        assert_eq!(app.work.run_seq, epoch, "no respawn");
        assert_eq!(app.work.session_id.as_deref(), Some("s1"));
        let texts: Vec<String> = transcript(app).into_iter().map(|(_, t)| t).collect();
        assert_eq!(texts[2..], ["pa", "a", "pb", "b"]);
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
    // One interrupt, no close: a second `Cancel` would have reached the idle
    // worker and closed the session.
    assert_eq!(
        fake.ops(),
        vec![
            "session_create(/wd)",
            "prompt_submit(s1,t1)",
            "session_interrupt(s1)",
            "prompt_submit(s1,pa)",
            "prompt_submit(s1,pb)",
            "close",
        ]
    );
}
