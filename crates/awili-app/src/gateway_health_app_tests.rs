//! Proof tests for the U2c app wiring (pure decisions only: no windows, no
//! network, no timers).

use std::time::{Duration, Instant};

use awili_code::CodeEngine;
use awili_config::EngineKind;

use super::*;
use crate::gateway_health::{GATEWAY_UNREACHABLE, IDLE_PROBE_EVERY};
use crate::work::WorkExecutor;

#[test]
fn u2c_gate_applies_only_to_the_gateway_lane() {
    let kind = EngineKind::HermesGateway;
    let gw = WorkExecutor::Gateway;
    // The fresh gateway lane is the ONLY case the gate fires on.
    assert!(gate_applies(false, false, &gw, true, kind));
    // Running: a live run is in charge.
    assert!(!gate_applies(true, false, &gw, true, kind));
    // A Stop in flight: the parking path is in charge.
    assert!(!gate_applies(false, true, &gw, true, kind));
    // Another executor: the agent/build lanes keep their own path.
    assert!(!gate_applies(
        false,
        false,
        &WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None
        },
        true,
        kind
    ));
    assert!(!gate_applies(
        false,
        false,
        &WorkExecutor::LocalBuild {
            target_id: "spark".to_string()
        },
        true,
        kind
    ));
    // Gateway disabled: nothing to gate on.
    assert!(!gate_applies(false, false, &gw, false, kind));
    // The configured engine is NOT the gateway worker.
    assert!(!gate_applies(false, false, &gw, true, EngineKind::Awili));
}

#[test]
fn u2c_tick_plan_probes_idle_gateway() {
    let t0 = Instant::now();
    let mut watch = HealthWatch::default();
    // Never probed + selected at rest → probe now.
    let plan = tick_plan(&watch, t0, false, true, Duration::ZERO);
    assert!(
        plan.probe,
        "a never-checked selected gateway probes on the first tick"
    );
    // Freshly probed Up: within the idle cadence → no probe.
    watch.gateway.finish_probe(Ok(()), t0);
    let plan = tick_plan(
        &watch,
        t0 + Duration::from_secs(2),
        false,
        true,
        Duration::ZERO,
    );
    assert!(!plan.probe, "a fresh Up stays quiet between idle probes");
    // Cadence elapsed → probe again.
    let plan = tick_plan(&watch, t0 + IDLE_PROBE_EVERY, false, true, Duration::ZERO);
    assert!(plan.probe, "the idle re-probe fires once the cadence is up");
    // Gateway not selected and no live turn → never probes.
    let plan = tick_plan(&watch, t0 + IDLE_PROBE_EVERY, false, false, Duration::ZERO);
    assert!(!plan.probe && plan.fail.is_none());
}

#[test]
fn u2c_tick_plan_fails_dead_turn() {
    let t0 = Instant::now();
    let mut watch = HealthWatch::default();
    // Two consecutive failed probes — the run_watch threshold.
    watch
        .gateway
        .finish_probe(Err("no route to host".to_string()), t0);
    watch.gateway.finish_probe(
        Err("no route to host".to_string()),
        t0 + Duration::from_secs(1),
    );
    // A live gateway turn that has been silent 25 s is failed…
    let plan = tick_plan(
        &watch,
        t0 + Duration::from_secs(25),
        true,
        true,
        Duration::from_secs(25),
    );
    let msg = plan
        .fail
        .expect("a dead gateway turn is failed while it is in flight");
    assert!(
        msg.starts_with(GATEWAY_UNREACHABLE),
        "the failure carries the U2a unreachable message, got: {msg}"
    );
    // …and only while it is in flight: the same watch with no live turn
    // fails nothing.
    let plan = tick_plan(
        &watch,
        t0 + Duration::from_secs(25),
        false,
        true,
        Duration::ZERO,
    );
    assert!(
        plan.fail.is_none(),
        "only a live gateway turn is ever failed"
    );
}

#[test]
fn u2c_tick_plan_refreshes_installed_every_15s() {
    let t0 = Instant::now();
    let mut watch = HealthWatch::default();
    // Never checked → refresh on the first tick.
    assert!(tick_plan(&watch, t0, false, false, Duration::ZERO).refresh_installed);
    // Freshly checked → quiet inside the cadence.
    watch.installed_checked_at = Some(t0);
    assert!(
        !tick_plan(
            &watch,
            t0 + Duration::from_secs(5),
            false,
            false,
            Duration::ZERO
        )
        .refresh_installed
    );
    // Cadence elapsed → refresh again.
    assert!(
        tick_plan(&watch, t0 + IDLE_PROBE_EVERY, false, false, Duration::ZERO).refresh_installed
    );
}

#[test]
fn u2c_engine_slots_are_distinct() {
    let engines = [
        CodeEngine::Claude,
        CodeEngine::Codex,
        CodeEngine::Glm,
        CodeEngine::Dsh,
    ];
    let mut slots = engines.iter().map(|e| engine_slot(*e)).collect::<Vec<_>>();
    slots.sort();
    slots.dedup();
    assert_eq!(
        slots.len(),
        4,
        "the four engines map to four distinct slots"
    );
    assert!(
        slots.iter().all(|s| *s < 4),
        "slots index a [Option<bool>; 4] array"
    );
}

#[test]
fn u2c_activity_silence_accessor() {
    let t0 = Instant::now();
    let mut watch = ActivityWatch::default();
    // Never observed: silence is ZERO.
    assert_eq!(watch.silence(t0 + Duration::from_secs(10)), Duration::ZERO);
    // Observed at t0: the clock runs from t0.
    assert_eq!(watch.observe(1, t0), Duration::ZERO);
    assert_eq!(
        watch.silence(t0 + Duration::from_secs(30)),
        Duration::from_secs(30)
    );
    // Reset: the clock is forgotten.
    watch.reset();
    assert_eq!(watch.silence(t0 + Duration::from_secs(30)), Duration::ZERO);
}

#[test]
fn u2c_health_watch_defaults_are_inert() {
    // A brand-new app must not probe or fail anything on its first tick:
    // reach is Unknown (no segment), no parked send, no timer yet.
    let t0 = Instant::now();
    let watch = HealthWatch::default();
    assert!(!watch.gateway.probing());
    assert!(matches!(
        *watch.gateway.reach(),
        crate::gateway_health::Reach::Unknown
    ));
    assert!(watch.gateway.pending_send.is_none());
    assert!(!watch.ticker_running);
    assert!(watch.installed.iter().all(|s| s.is_none()));
    assert!(watch.installed_checked_at.is_none());
    // And the default watch's tick plan fails nothing on any lane.
    for (turn, selected) in [(true, true), (true, false), (false, true), (false, false)] {
        let plan = tick_plan(&watch, t0, turn, selected, Duration::ZERO);
        assert!(
            plan.fail.is_none(),
            "a fresh watch never fails a turn (turn={turn}, selected={selected})"
        );
    }
}

// ---------------------------------------------------------------------------
// U2c-FIX1: app-level proof. Each test runs the real `AwiliApp` (built by
// `AwiliApp::new` in a subprocess with a private HOME, the
// `stream_turn_tests` isolation) and drives the production paths: the
// composer submit, the probe completion, the 1 s ticker, the status run and
// the rendered Progress card. The gateway "worker" is a plain channel whose
// receiver the test holds, so "sent" and "not sent" are observable exactly.
// ---------------------------------------------------------------------------

use crate::work::GatewayCmd;
use crate::work_run_identity::WorkRunIdentity;
use gpui::AppContext;

const FIX_CHILD: &str = "AWILI_U2C_FIX1_TEST_HOME";
const FIX_CONV: &str = "u2c-fix1-work-conv";
const DRAFT: &str = "deploy the fix";

/// Run `test` in a subprocess with a private HOME. `true` inside the child
/// (run the body); `false` in the parent once the child passed.
fn in_isolated_child(test: &str) -> bool {
    if std::env::var_os(FIX_CHILD).is_some() {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(FIX_CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "the subprocess must actually run the test: {stdout}"
    );
    false
}

/// A dashboard URL nothing listens on (bind port 0, then drop it).
fn closed_dashboard_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// Make `app` a configured gateway-lane Work session whose persistent worker
/// is a plain channel. Returns the receiving end: every prompt the app sends
/// to the gateway lands there.
fn gateway_lane(app: &mut crate::AwiliApp) -> std::sync::mpsc::Receiver<GatewayCmd> {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.mode = Mode::Work;
    app.shell.job_open = true;
    app.work.folder = None;
    app.work.gateway.enabled = true;
    app.work.gateway.dashboard_url = closed_dashboard_url();
    app.work.engine.kind = EngineKind::HermesGateway;
    app.work.executor = WorkExecutor::Gateway;
    app.views.insert(
        FIX_CONV.into(),
        crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
    );
    app.set_active_conversation(FIX_CONV);
    let (tx, rx) = std::sync::mpsc::channel();
    app.work.gateway_cmd_tx = Some(tx);
    app.work.worker_identity = Some(WorkRunIdentity::HermesGateway);
    rx
}

/// The Work transcript's message texts.
fn transcript(app: &crate::AwiliApp) -> Vec<String> {
    app.views[FIX_CONV]
        .convo
        .messages()
        .iter()
        .map(|m| m.text.clone())
        .collect()
}

/// The status run's segment texts, exactly as `chips_row` gets them.
fn status_texts(app: &crate::AwiliApp) -> Vec<String> {
    app.status_parts_with_executor()
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// Record `n` failed probes on the gateway watch.
fn fail_probes(app: &mut crate::AwiliApp, n: usize) {
    for _ in 0..n {
        assert!(app.work.health.gateway.begin_probe());
        app.work
            .health
            .gateway
            .finish_probe(Err("connection refused".to_string()), Instant::now());
    }
}

/// F01 outcome 1: the composer submit on a gateway known to be down is
/// refused: the text is back in the composer, nothing reaches the gateway
/// worker, no transcript row, the executor picker opens, and the toast and
/// the status run say the gateway is unreachable.
#[gpui::test]
fn u2c_fix_refused_submit_restores_text_and_sends_nothing(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "gateway_health_app::tests::u2c_fix_refused_submit_restores_text_and_sends_nothing",
    ) {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        let cmd_rx = gateway_lane(app);
        fail_probes(app, 1);
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert_eq!(
            app.composer.read(cx).text(),
            DRAFT,
            "the refused text is back in the composer"
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "nothing was sent to the gateway"
        );
        assert!(transcript(app).is_empty(), "no transcript row");
        assert!(!app.work.running);
        assert!(
            app.work.executor_picker_open,
            "another executor is one click away"
        );
        let toast = app.toast.msg.clone().expect("the refusal toast");
        assert!(
            toast.starts_with(GATEWAY_UNREACHABLE) && toast.contains("connection refused"),
            "{toast}"
        );
        let status = status_texts(app);
        assert!(
            status.contains(&format!("\u{2717} {GATEWAY_UNREACHABLE}")),
            "{status:?}"
        );
    });
}

/// F01 outcome 1, probe path: with the gateway's state not fresh the submit
/// parks behind the in-flight probe; the probe completes Down and the parked
/// send is refused: text restored, nothing sent.
#[gpui::test]
fn u2c_fix_probe_down_refuses_the_parked_send(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("gateway_health_app::tests::u2c_fix_probe_down_refuses_the_parked_send") {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        let cmd_rx = gateway_lane(app);
        // An idle probe is already in flight, so the submit parks behind it
        // and starts no second probe thread.
        assert!(app.work.health.gateway.begin_probe());
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert_eq!(
            app.work
                .health
                .gateway
                .pending_send
                .as_ref()
                .map(|p| p.text.as_str()),
            Some(DRAFT),
            "the send is parked behind the probe"
        );
        assert!(app.composer.read(cx).text().is_empty());
        assert!(cmd_rx.try_recv().is_err(), "nothing sent while probing");
        assert!(
            status_texts(app).contains(&"\u{25CB} checking gateway\u{2026}".to_string()),
            "{:?}",
            status_texts(app)
        );

        app.on_gateway_probe(Err("connection refused".to_string()), cx);
        assert!(app.work.health.gateway.pending_send.is_none());
        assert_eq!(
            app.composer.read(cx).text(),
            DRAFT,
            "the refused text is back in the composer"
        );
        assert!(
            cmd_rx.try_recv().is_err(),
            "nothing was sent to the gateway"
        );
        assert!(transcript(app).is_empty(), "no transcript row");
        assert!(app.work.executor_picker_open);
        let toast = app.toast.msg.clone().expect("the refusal toast");
        assert!(toast.starts_with(GATEWAY_UNREACHABLE), "{toast}");
    });
}

/// F01 outcome 1, probe path: the probe completes Up and the parked send is
/// submitted exactly once to the gateway worker.
#[gpui::test]
fn u2c_fix_probe_up_submits_the_parked_send(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("gateway_health_app::tests::u2c_fix_probe_up_submits_the_parked_send") {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        let cmd_rx = gateway_lane(app);
        assert!(app.work.health.gateway.begin_probe());
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert!(cmd_rx.try_recv().is_err(), "nothing sent while probing");

        app.on_gateway_probe(Ok(()), cx);
        match cmd_rx.try_recv() {
            Ok(GatewayCmd::Prompt { text, .. }) => {
                assert!(text.contains(DRAFT), "the parked text is sent: {text}")
            }
            Ok(_) => panic!("expected the parked prompt, got another command"),
            Err(e) => panic!("the parked prompt was never sent: {e:?}"),
        }
        assert!(cmd_rx.try_recv().is_err(), "sent exactly once");
        assert!(app.work.running);
        assert_eq!(transcript(app).first().map(String::as_str), Some(DRAFT));
        assert!(app.composer.read(cx).text().is_empty());
        assert!(app.work.health.gateway.pending_send.is_none());
    });
}

#[gpui::test]
fn workshop_stop_cancels_gateway_probe_before_late_success(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "gateway_health_app::tests::workshop_stop_cancels_gateway_probe_before_late_success",
    ) {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        let cmd_rx = gateway_lane(app);
        app.work.conv_id = Some("previous-finished-run".into());
        app.work.worker_conv = Some("previous-finished-run".into());
        app.sync_composer_draft(cx);
        assert!(app.work.health.gateway.begin_probe());
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert!(app.work.health.gateway.pending_send.is_some());
        app.abort_active_stream(cx);
        assert!(app.work.health.gateway.pending_send.is_none());
        assert!(app.composer.read(cx).text().contains(DRAFT));
        app.on_gateway_probe(Ok(()), cx);
        assert!(!cmd_rx
            .try_iter()
            .any(|c| matches!(c, GatewayCmd::Prompt { .. })));
        assert!(!app.work.running);
        assert!(transcript(app).is_empty());
    });
}

/// F01 outcome 2: the 1 s ticker probes the selected idle gateway, the
/// probe's result comes back through the app's own channel + `cx.spawn`
/// delivery, and the status run follows it: "○ gateway" before the tick,
/// "✗ Fleet gateway unreachable…" after it. (The blocking HTTP GET is the
/// one thing replaced, by the `TEST_PROBE_RESULT` seam: the gpui test
/// scheduler rejects wake-ups from the probe thread.)
#[gpui::test]
fn u2c_fix_ticker_probes_idle_gateway_and_status_follows(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "gateway_health_app::tests::u2c_fix_ticker_probes_idle_gateway_and_status_follows",
    ) {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        let _cmd_rx = gateway_lane(app);
        app.work.health.installed_checked_at = Some(Instant::now());
        assert!(status_texts(app).contains(&"\u{25CB} gateway".to_string()));
        TEST_PROBE_RESULT.with(|slot| *slot.borrow_mut() = Some(Err("connection refused".into())));
        app.start_health_ticker(cx);
    });
    cx.run_until_parked();
    assert!(
        TEST_PROBE_RESULT.with(|slot| slot.borrow().is_some()),
        "no probe before the first tick"
    );
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(
        TEST_PROBE_RESULT.with(|slot| slot.borrow().is_none()),
        "the tick started the idle probe"
    );
    entity.update(cx, |app, _cx| {
        assert!(!app.work.health.gateway.probing(), "the probe completed");
        assert!(matches!(
            app.work.health.gateway.reach(),
            crate::gateway_health::Reach::Down(_)
        ));
        let status = status_texts(app);
        assert!(
            status.contains(&format!("\u{2717} {GATEWAY_UNREACHABLE}")),
            "{status:?}"
        );
    });
}

/// F14: with an agent executor selected, the status run names that agent and
/// never says "gateway", even when the launch probe left `hermes_status` Up.
#[gpui::test]
fn u2c_fix_status_run_names_the_selected_executor(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "gateway_health_app::tests::u2c_fix_status_run_names_the_selected_executor",
    ) {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, _cx| {
        let _cmd_rx = gateway_lane(app);
        app.work.hermes_status = Some(Ok("ok".to_string()));
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None,
        };
        app.work.health.installed[engine_slot(CodeEngine::Glm)] = Some(true);
        let status = status_texts(app);
        assert!(
            status.contains(&"\u{25CF} GLM \u{00B7} default".to_string()),
            "{status:?}"
        );
        assert!(
            status.iter().all(|t| !t.contains("gateway")),
            "GLM selected: no segment may claim the gateway: {status:?}"
        );
    });
}

/// P1 (Codex T2 review of effcf88): a gateway turn under a pending Stop is
/// never failed by the watchdog — no failure line, no cancel flag, the
/// barrier and the sender stay — however many ticks run. Today
/// `cancel_work_run` clears `running` with the barrier, so the state is
/// forced here; the watchdog must not depend on that. Without the Stop, the
/// same dead turn IS failed, once, through the existing `Failed` arm.
#[gpui::test]
fn u2c_fix_watchdog_never_fails_a_stopping_turn(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("gateway_health_app::tests::u2c_fix_watchdog_never_fails_a_stopping_turn")
    {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        let cmd_rx = gateway_lane(app);
        app.submit_work_run("long task".to_string(), cx);
        assert!(matches!(cmd_rx.try_recv(), Ok(GatewayCmd::Prompt { .. })));
        assert!(gateway_turn_in_flight(&app.work));
        app.work.gateway_stop_pending = true;
        app.work.health.installed_checked_at = Some(Instant::now());
        fail_probes(app, 2);
        let before = transcript(app);

        app.health_tick(cx);
        app.health_tick(cx);
        assert_eq!(
            transcript(app),
            before,
            "no failure line on the stopping turn"
        );
        assert!(app.work.gateway_stop_pending, "the Stop barrier stays");
        assert!(app.work.running);
        assert!(app.work.panel.error.is_none());
        assert!(app.work.gateway_cmd_tx.is_some(), "the sender is kept");
        assert!(
            !app.work.cancel.load(std::sync::atomic::Ordering::SeqCst),
            "the Stop never trips the cancel flag"
        );

        // Positive control: no Stop pending → the dead turn is failed once.
        app.work.gateway_stop_pending = false;
        app.health_tick(cx);
        assert!(!app.work.running);
        let error = app.work.panel.error.clone().expect("the turn failed");
        assert!(error.starts_with(GATEWAY_UNREACHABLE), "{error}");
        assert!(app.work.gateway_cmd_tx.is_none(), "the worker is retired");
    });
}

/// F09 outcome 5: a Work run silent for 45 s shows the idle label in the
/// rendered Progress card and in the status run; a run with fresh activity
/// shows neither.
#[gpui::test]
fn u2c_fix_silent_run_shows_idle_label_in_progress_card(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "gateway_health_app::tests::u2c_fix_silent_run_shows_idle_label_in_progress_card",
    ) {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
    let _cmd_rx = view.update(window, |app, cx| {
        let rx = gateway_lane(app);
        app.submit_work_run("long task".to_string(), cx);
        assert!(app.work.running);
        app.work.health.activity.observe(1, Instant::now());
        cx.notify();
        rx
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(IDLE_LINE_SELECTOR).is_none(),
        "no idle line while the run is active"
    );

    view.update(window, |app, cx| {
        let t = Instant::now().checked_sub(Duration::from_secs(45)).unwrap();
        app.work.health.activity.observe(2, t);
        let status = status_texts(app);
        assert!(
            status
                .iter()
                .any(|s| s.starts_with("still working — nothing new for 4")),
            "{status:?}"
        );
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(IDLE_LINE_SELECTOR).is_some(),
        "the Progress card shows the idle line"
    );
}

/// U2a-FIX2 (Codex T2 review of 55a5fab; lead decision: never lose typed
/// text): a send parked behind a probe, then the user moves to another
/// conversation and starts typing there. The probe completes Down, and then
/// (second round) Up: neither touches the current composer, nothing is sent,
/// the picker stays shut, the FULL unsent text is on the clipboard, and the
/// toast says so.
#[gpui::test]
fn u2a_fix2_moved_on_probe_copies_to_clipboard(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("gateway_health_app::tests::u2a_fix2_moved_on_probe_copies_to_clipboard")
    {
        return;
    }
    const OTHER: &str = "u2a-fix2-other-conv";
    const TYPING: &str = "new conversation text";
    const LONG: &str =
        "deploy the fix\nthen restart the gateway worker on the Studio and read the logs twice";
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        let cmd_rx = gateway_lane(app);
        app.views.insert(
            OTHER.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        for result in [Err("connection refused".to_string()), Ok(())] {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("stale".to_string()));
            assert!(app.work.health.gateway.begin_probe());
            app.set_active_conversation(FIX_CONV);
            app.composer.update(cx, |c, cx| c.set_text(LONG, cx));
            app.submit_composer_draft(cx);
            assert!(app.work.health.gateway.pending_send.is_some(), "parked");
            // The user moves on and starts typing in the other conversation.
            app.set_active_conversation(OTHER);
            app.composer.update(cx, |c, cx| c.set_text(TYPING, cx));

            app.on_gateway_probe(result, cx);
            assert!(app.work.health.gateway.pending_send.is_none());
            assert_eq!(
                app.composer.read(cx).text(),
                TYPING,
                "the old text must not land in the current composer"
            );
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some(LONG.to_string()),
                "the full unsent text is on the clipboard"
            );
            assert!(cmd_rx.try_recv().is_err(), "nothing was sent");
            assert!(transcript(app).is_empty(), "no transcript row");
            assert!(
                !app.work.executor_picker_open,
                "no picker in the wrong place"
            );
            let toast = app.toast.msg.clone().expect("the moved-on notice");
            assert!(toast.contains("copied to the clipboard"), "{toast}");
        }
    });
}

/// U2-INT1 (Codex slice review of 0eae92e, P1): a send parked for the
/// gateway probe, then the user switches the Work executor to an agent
/// before the probe returns Up. Nothing is sent, not to the gateway worker
/// and not through the agent: in the same conversation the text is back in
/// the composer; after moving on as well, the full text is on the clipboard
/// and the current composer is untouched.
#[gpui::test]
fn u2_int1_executor_switch_during_probe_never_sends_elsewhere(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "gateway_health_app::tests::u2_int1_executor_switch_during_probe_never_sends_elsewhere",
    ) {
        return;
    }
    const OTHER: &str = "u2-int1-other-conv";
    const TYPING: &str = "new conversation text";
    let glm = WorkExecutor::Agent {
        engine: CodeEngine::Glm,
        model: None,
    };
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        let cmd_rx = gateway_lane(app);
        app.views.insert(
            OTHER.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );

        // Round 1: same conversation, executor switched → text restored.
        assert!(app.work.health.gateway.begin_probe());
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert!(app.work.health.gateway.pending_send.is_some(), "parked");
        app.work.executor = glm.clone();
        app.on_gateway_probe(Ok(()), cx);
        assert!(app.work.health.gateway.pending_send.is_none());
        assert_eq!(
            app.composer.read(cx).text(),
            DRAFT,
            "the text is back in the composer"
        );
        assert!(cmd_rx.try_recv().is_err(), "nothing sent to the gateway");
        assert!(
            transcript(app).is_empty(),
            "no run through the agent either"
        );
        assert!(!app.work.running);
        assert_eq!(app.work.executor, glm, "the user's choice is kept");
        let toast = app.toast.msg.clone().expect("the notice");
        assert_eq!(toast.as_ref(), EXECUTOR_CHANGED_NOTICE);

        // Round 2: executor switched AND moved on → clipboard, composer untouched.
        app.work.executor = WorkExecutor::Gateway;
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("stale".to_string()));
        assert!(app.work.health.gateway.begin_probe());
        app.submit_composer_draft(cx);
        assert!(app.work.health.gateway.pending_send.is_some(), "parked");
        app.work.executor = glm.clone();
        app.set_active_conversation(OTHER);
        app.composer.update(cx, |c, cx| c.set_text(TYPING, cx));
        app.on_gateway_probe(Ok(()), cx);
        assert_eq!(app.composer.read(cx).text(), TYPING);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(DRAFT.to_string()),
            "the full unsent text is on the clipboard"
        );
        assert!(cmd_rx.try_recv().is_err(), "nothing sent to the gateway");
        assert!(
            transcript(app).is_empty(),
            "no run through the agent either"
        );
        assert!(!app.work.running);
    });
}

// ---------------------------------------------------------------------------
// R1e: ONE gateway truth across the Work Connection card, the System tab and
// the Test button (RC1 round-1 finding Major 8 / D2).
// ---------------------------------------------------------------------------

use crate::gateway_health::Reach;

/// What the launch / Test token scrape stores in `hermes_status` (the
/// `gateway_connection_test` success text). It must never reach the user.
const SCRAPE_OK: &str = "Gateway reachable: session token found";

/// The pure line: three plain states plus "checking", each with its own
/// colour family; the System notice keeps "Connect a gateway" only when no
/// gateway is set up.
#[test]
fn r1e_gateway_line_words_and_tones() {
    let down = Reach::Down("connection refused".to_string());
    assert_eq!(
        gateway_line(&Reach::Up, false),
        (GATEWAY_CONNECTED, GatewayTone::Good)
    );
    assert_eq!(
        gateway_line(&down, false),
        (GATEWAY_UNREACHABLE, GatewayTone::Bad)
    );
    assert_eq!(
        gateway_line(&Reach::Unknown, false),
        (GATEWAY_NOT_CHECKED, GatewayTone::Unknown)
    );
    for reach in [Reach::Up, down, Reach::Unknown] {
        assert_eq!(
            gateway_line(&reach, true),
            (GATEWAY_CHECKING, GatewayTone::Unknown)
        );
    }
    let colors = [
        GatewayTone::Good.color(),
        GatewayTone::Bad.color(),
        GatewayTone::Unknown.color(),
    ];
    assert_eq!(colors[0], colors[2], "connected and checking are neutral");
    assert_eq!(colors[0], crate::theme::palette().text_dim);
    assert_eq!(
        colors[1],
        crate::theme::tone_text(crate::theme::Tone::ActNow)
    );
    assert_ne!(colors[0], colors[1], "unreachable still calls for action");
    assert_ne!(
        GatewayTone::Good.color(),
        crate::theme::ACCENT,
        "good is not the plain blue"
    );

    assert_eq!(
        command_center_unavailable_text(false, GATEWAY_CONNECTED),
        CC_NO_GATEWAY
    );
    for line in [
        GATEWAY_CONNECTED,
        GATEWAY_UNREACHABLE,
        GATEWAY_NOT_CHECKED,
        GATEWAY_CHECKING,
    ] {
        let text = command_center_unavailable_text(true, line);
        assert!(text.starts_with(line), "{text}");
        assert!(text.ends_with(CC_NEEDS_HERMES), "{text}");
        assert!(!text.contains("Connect a gateway"), "{text}");
        assert!(!text.contains("session token"), "{text}");
    }
}

/// The app drives one truth: with the gateway set up (and, as in RC1 B6,
/// Claude selected and the launch scrape's "session token" text stored),
/// the Connection card line, the System notice and the Test button's toasts
/// say the same words for Unknown → (Test) checking → Up → (Test) checking →
/// Down, and "session token" never shows.
#[gpui::test]
fn r1e_card_system_and_test_say_the_same_words(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("gateway_health_app::tests::r1e_card_system_and_test_say_the_same_words")
    {
        return;
    }
    fn surfaces(app: &crate::AwiliApp) -> (String, GatewayTone, String, Vec<String>) {
        let (line, tone) = app.gateway_status_line();
        (
            line.to_string(),
            tone,
            app.command_center_unavailable_text(),
            status_texts(app),
        )
    }
    fn check(app: &crate::AwiliApp, want: &str, want_tone: GatewayTone, toast: Option<&str>) {
        let (line, tone, system, status) = surfaces(app);
        assert_eq!(line, want, "the Connection card line");
        assert_eq!(tone, want_tone, "{want}");
        assert!(
            system.starts_with(want) && !system.contains("Connect a gateway"),
            "the System notice: {system}"
        );
        assert_eq!(app.toast.msg.as_deref(), toast, "the Test toast");
        for text in [line.as_str(), system.as_str()]
            .into_iter()
            .chain(status.iter().map(String::as_str))
            .chain(app.toast.msg.as_deref())
        {
            assert!(!text.contains("session token"), "jargon shown: {text}");
        }
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        let _cmd_rx = gateway_lane(app);
        app.work.hermes.enabled = false;
        app.work.hermes_status = Some(Ok(SCRAPE_OK.to_string()));
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        check(app, GATEWAY_NOT_CHECKED, GatewayTone::Unknown, None);

        TEST_PROBE_RESULT.with(|slot| *slot.borrow_mut() = Some(Ok(())));
        app.test_connection_pressed(cx);
        check(
            app,
            GATEWAY_CHECKING,
            GatewayTone::Unknown,
            Some(GATEWAY_CHECKING),
        );
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        check(
            app,
            GATEWAY_CONNECTED,
            GatewayTone::Good,
            Some(GATEWAY_CONNECTED),
        );

        TEST_PROBE_RESULT.with(|slot| *slot.borrow_mut() = Some(Err("connection refused".into())));
        app.test_connection_pressed(cx);
        check(
            app,
            GATEWAY_CHECKING,
            GatewayTone::Unknown,
            Some(GATEWAY_CHECKING),
        );
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        check(
            app,
            GATEWAY_UNREACHABLE,
            GatewayTone::Bad,
            Some(GATEWAY_UNREACHABLE),
        );
        assert!(!app.work.health.test_requested);
    });
}

/// The rendered app uses the line: the Work Connection card shows the
/// gateway line (not the stored scrape text), a click on its Test button
/// toasts the probe's result in the same words, and the System tab shows the
/// gateway-aware notice instead of "Connect a gateway".
#[gpui::test]
fn r1e_card_and_system_render_the_gateway_line(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("gateway_health_app::tests::r1e_card_and_system_render_the_gateway_line")
    {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
    view.update(window, |app, cx| {
        let _cmd_rx = gateway_lane(app);
        app.shell.job_tab = crate::job_panel::JobTab::Context;
        app.shell.job_details = true;
        app.work.hermes.enabled = false;
        app.work.hermes_status = Some(Ok(SCRAPE_OK.to_string()));
        app.work.health.gateway.begin_probe();
        app.work.health.gateway.finish_probe(Ok(()), Instant::now());
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(GATEWAY_LINE_SELECTOR).is_some(),
        "the Connection card shows the gateway line"
    );

    // Context is a bounded scrolling panel now. Reveal the real connection
    // control through its scroll viewport before clicking it.
    for _ in 0..20 {
        let panel = window.debug_bounds("ui-panel-awili-job-panel").unwrap();
        let button = window.debug_bounds(TEST_BUTTON_SELECTOR).unwrap();
        if button.top() >= panel.top() && button.bottom() <= panel.bottom() - crate::theme::target()
        {
            break;
        }
        window.simulate_event(gpui::ScrollWheelEvent {
            position: panel.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-200.))),
            ..Default::default()
        });
        window.run_until_parked();
    }
    // The real Test button: a click runs the U2 probe and toasts its result.
    let button = window
        .debug_bounds(TEST_BUTTON_SELECTOR)
        .expect("the Test button is rendered");
    TEST_PROBE_RESULT.with(|slot| *slot.borrow_mut() = Some(Err("connection refused".into())));
    window.simulate_click(button.center(), gpui::Modifiers::none());
    window.run_until_parked();
    view.update(window, |app, _cx| {
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(GATEWAY_UNREACHABLE),
            "the click's result is toasted"
        );
        assert_eq!(app.gateway_status_line().0, GATEWAY_UNREACHABLE);
    });

    view.update(window, |app, cx| {
        app.mode = Mode::CommandCenter;
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(CC_UNAVAILABLE_SELECTOR).is_some(),
        "the System tab shows the gateway-aware notice"
    );
}

/// RC2-RUN (R3-20): Codex prints each message only when it is complete
/// (codex-cli 0.160.1 `exec --json`), so its silence reads as such, not as
/// "no activity"; Claude streams; GLM still replies at the end. RC2-CXS:
/// Codex now streams over its app server; only a session whose Codex had
/// to run on `exec --json` keeps the whole-message wording.
#[test]
fn rc2_run_codex_silence_reads_plainly() {
    use crate::executor_status::Quiet;
    use crate::work_run_identity::WorkRunIdentity;
    let quiet = |engine: awili_code::CodeEngine| {
        let panel = crate::work::RunPanel {
            requested_identity: Some(WorkRunIdentity::Agent {
                engine,
                model: None,
            }),
            ..Default::default()
        };
        run_quiet(&panel, false)
    };
    assert_eq!(quiet(awili_code::CodeEngine::Codex), Quiet::Streams);
    let codex = crate::work::RunPanel {
        requested_identity: Some(WorkRunIdentity::Agent {
            engine: awili_code::CodeEngine::Codex,
            model: None,
        }),
        ..Default::default()
    };
    assert_eq!(
        run_quiet(&codex, true),
        Quiet::WholeMessages(awili_code::CodeEngine::Codex.label())
    );
    assert_eq!(quiet(awili_code::CodeEngine::Claude), Quiet::Streams);
    assert_eq!(
        quiet(awili_code::CodeEngine::Glm),
        Quiet::WholeReply(awili_code::CodeEngine::Glm.label())
    );
    assert_eq!(
        run_quiet(&crate::work::RunPanel::default(), false),
        Quiet::Streams
    );
}

/// RC2-R66 (usability RC2 round 5, Minor 8: the amber "Fleet gateway
/// unreachable" sat under Connection on every Work page): the owner's
/// fleet gateway (a local gateway) is used by Work only when the
/// Fleet gateway is the helper. With the in-app model or an agent the card
/// shows no gateway warning; with the Fleet gateway it does, as before.
#[gpui::test]
fn rc2r66_the_gateway_warning_shows_where_it_matters(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "gateway_health_app::tests::rc2r66_the_gateway_warning_shows_where_it_matters",
    ) {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
    view.update(window, |app, cx| {
        let _cmd_rx = gateway_lane(app);
        app.shell.job_tab = crate::job_panel::JobTab::Context;
        app.shell.job_details = true;
        app.work.gateway_cmd_tx = None;
        app.work.hermes.enabled = false;
        app.work.health.gateway.begin_probe();
        app.work
            .health
            .gateway
            .finish_probe(Err("connection refused".into()), Instant::now());
        app.work.executor = WorkExecutor::InApp;
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(
        view.update(window, |app, _| app.gateway_status_line().0),
        GATEWAY_UNREACHABLE
    );
    assert!(
        window.debug_bounds(GATEWAY_LINE_SELECTOR).is_none(),
        "no gateway warning while the in-app model is the helper"
    );
    view.update(window, |app, cx| {
        app.work.executor = WorkExecutor::Agent {
            engine: awili_code::CodeEngine::Claude,
            model: None,
        };
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(GATEWAY_LINE_SELECTOR).is_none(),
        "nor with Claude"
    );
    view.update(window, |app, cx| {
        app.work.executor = WorkExecutor::Gateway;
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(GATEWAY_LINE_SELECTOR).is_some(),
        "the Fleet gateway helper shows its state"
    );
}

/// M2: a send parked for the gateway lane, then a chooser change (to another
/// harness, and then to an unavailable saved target) before the probe returns
/// Up. The text goes back to its own conversation's box; nothing reaches the
/// gateway worker or any run on the new target; the selection is not undone.
#[gpui::test]
fn m2_chooser_change_during_probe_never_sends_on_the_new_target(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "gateway_health_app::tests::m2_chooser_change_during_probe_never_sends_on_the_new_target",
    ) {
        return;
    }
    let glm = WorkExecutor::Agent {
        engine: CodeEngine::Glm,
        model: None,
    };
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        // A real thread of the app (it has a route), on the gateway lane.
        let conv = app.sidebar.active_id().to_string();
        let owner = crate::workshop_composer::ModelOwner::Work(conv.clone());
        let cmd_rx = gateway_lane(app);
        app.set_active_conversation(&conv);
        app.mode = Mode::Work;
        assert!(app.set_composer_target(&owner, WorkExecutor::Gateway, cx));
        let rows = |app: &crate::AwiliApp| app.views[&conv].convo.messages().len();
        let before = rows(app);

        for round in 0..2 {
            assert!(app.set_composer_target(&owner, WorkExecutor::Gateway, cx));
            assert!(app.work.health.gateway.begin_probe());
            app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
            app.submit_composer_draft(cx);
            assert!(app.work.health.gateway.pending_send.is_some(), "parked");
            if round == 0 {
                assert!(app.set_composer_target(&owner, glm.clone(), cx));
            } else {
                app.work
                    .unavailable_conversation_routes
                    .insert(conv.clone(), "{\"version\":99}".into());
            }
            app.toast.msg = None;
            app.on_gateway_probe(Ok(()), cx);
            assert!(app.work.health.gateway.pending_send.is_none());
            assert_eq!(app.composer.read(cx).text(), DRAFT, "round {round}");
            assert!(cmd_rx.try_recv().is_err(), "nothing sent to the gateway");
            assert_eq!(rows(app), before, "no transcript row");
            assert!(!app.work.running, "no run on the new target");
            assert_eq!(app.sidebar.active_id(), conv);
            let toast = app.toast.msg.clone().expect("the notice");
            assert_eq!(toast.as_ref(), EXECUTOR_CHANGED_NOTICE);
            app.composer.update(cx, |c, cx| c.set_text("", cx));
        }
        assert_eq!(
            app.composer_target_for(&owner),
            None,
            "the unavailable saved target stays unavailable"
        );
    });
}
