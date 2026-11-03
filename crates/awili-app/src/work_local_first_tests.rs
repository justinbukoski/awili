//! L1b — the selected Work executor, not `[engine].kind`, decides the engine:
//! end to end through the real `AwiliApp::submit_work_run` and
//! `resolve_work_approval`, each test in an isolated HOME (the
//! `gateway_queue_tests::isolated` subprocess pattern).

use super::gateway_queue_tests::{isolated, wire_gateway, CONV};
use super::tests::FakeOps;
use super::*;
use awili_code::CodeEngine;
use std::sync::atomic::AtomicBool as StdAtomicBool;
use std::time::{Duration, Instant};

/// A configured, folder-less Work session in `CONV`, memory off (the owned
/// engine then dispatches straight away instead of awaiting the network
/// memory gate).
fn work_app(app: &mut AwiliApp) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.memory.enabled = false;
    app.mode = Mode::Work;
    app.work.folder = None;
    app.views.insert(
        CONV.into(),
        crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
    );
    app.set_active_conversation(CONV);
}

/// Arm the one-shot worker-spawn seam; the flag flips when a worker would
/// have started.
fn spawn_hook() -> Arc<StdAtomicBool> {
    let fired = Arc::new(StdAtomicBool::new(false));
    let flag = fired.clone();
    TEST_WORKER_SPAWN.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |_ctx| {
            flag.store(true, Ordering::SeqCst);
        }));
    });
    fired
}

/// A fresh install's Work executor is the in-app engine on this Mac.
#[gpui::test]
fn l1b_default_executor_is_in_app(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1b_default_executor_is_in_app").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        assert_eq!(app.work_engine_kind(), awili_config::EngineKind::Awili);
    });
}

/// The fleet gateway runs Hermes even when `[engine].kind` says "awili" and
/// the in-app endpoint is blank (before L1b the config decided, and this
/// submit was refused as "Engine not configured").
#[gpui::test]
fn l1b_gateway_executor_runs_hermes_even_if_config_says_awili(cx: &mut gpui::TestAppContext) {
    if isolated(
        "work::local_first_tests::l1b_gateway_executor_runs_hermes_even_if_config_says_awili",
    )
    .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        app.work.engine.kind = awili_config::EngineKind::Awili;
        app.work.engine.base_url = String::new();
        app.work.executor = WorkExecutor::Gateway;
        app.work.gateway.enabled = true;
        let fired = spawn_hook();
        app.submit_work_run("x".into(), cx);
        assert_eq!(app.work.panel.error, None);
        assert!(fired.load(Ordering::SeqCst), "a worker was spawned");
        assert_eq!(
            app.work.worker_identity,
            Some(WorkRunIdentity::HermesGateway)
        );
    });
}

/// In-app needs its own endpoint even when `[engine].kind` says
/// "hermes-gateway" and the gateway is configured: nothing is spawned.
#[gpui::test]
fn l1b_in_app_needs_its_endpoint_even_if_config_says_gateway(cx: &mut gpui::TestAppContext) {
    if isolated(
        "work::local_first_tests::l1b_in_app_needs_its_endpoint_even_if_config_says_gateway",
    )
    .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        app.work.engine.kind = awili_config::EngineKind::HermesGateway;
        app.work.engine.base_url = String::new();
        app.work.gateway.enabled = true;
        app.work.executor = WorkExecutor::InApp;
        let fired = spawn_hook();
        app.submit_work_run("x".into(), cx);
        assert_eq!(
            app.work.panel.error.as_deref(),
            Some("Engine not configured (set engine.base_url in config.toml)")
        );
        assert!(!fired.load(Ordering::SeqCst), "nothing was spawned");
        assert!(!app.work.running);
    });
}

/// In-app with a configured endpoint builds the owned engine, whatever
/// `[engine].kind` says.
#[gpui::test]
fn l1b_in_app_spawns_owned_engine(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1b_in_app_spawns_owned_engine").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        app.work.engine.kind = awili_config::EngineKind::HermesGateway;
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "m".into();
        app.work.executor = WorkExecutor::InApp;
        let fired = spawn_hook();
        app.submit_work_run("x".into(), cx);
        assert_eq!(app.work.panel.error, None);
        assert!(fired.load(Ordering::SeqCst), "the owned engine was spawned");
        assert_eq!(
            app.work.worker_identity,
            Some(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None })
        );
    });
}

/// D6: a live, idle Hermes worker is shut down when the next prompt is for
/// the in-app lane; the prompt goes to a fresh owned engine, never to the
/// gateway the chip no longer shows.
#[gpui::test]
fn l1b_switch_lane_retires_live_worker(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::local_first_tests::l1b_switch_lane_retires_live_worker")
    else {
        return;
    };
    let fake = FakeOps::new(vec![]);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (_ev_rx, handle, _epoch) = wire_gateway(app, &home, &fake, cx);
        app.memory.enabled = false;
        app.work.executor = WorkExecutor::InApp;
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "m".into();
        let fired = spawn_hook();
        app.submit_work_run("next".into(), cx);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !handle.is_finished() {
            assert!(
                Instant::now() < deadline,
                "the gateway worker was never shut down"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        handle.join().unwrap();
        assert!(fired.load(Ordering::SeqCst), "the owned engine was spawned");
        assert_eq!(
            app.work.worker_identity,
            Some(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None })
        );
    });
}

/// An approval card raised by the in-app engine is answerable: the verdict
/// goes to the worker with the engine's ORIGINAL id and the card clears.
/// Before L1b, `resolve_work_approval` returned early for any executor other
/// than the gateway row.
#[gpui::test]
fn l1b_in_app_approval_card_is_answerable(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1b_in_app_approval_card_is_answerable").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        app.work.executor = WorkExecutor::InApp;
        app.work.engine.kind = awili_config::EngineKind::HermesGateway;
        app.work.worker_identity = Some(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None });
        let (tx, rx) = std::sync::mpsc::channel();
        app.work.gateway_cmd_tx = Some(tx);
        app.work.panel.approval = Some(ApprovalCard {
            approval_id: "abc-1".into(),
            kind: "approval".into(),
            prompt: "write a file".into(),
            risk: ApprovalRisk::decode(Some("mutating")),
            delivery: ApprovalDelivery::default(),
        });
        app.resolve_work_approval(ApprovalDecision::Session, cx);
        match rx.try_recv().expect("the verdict reached the worker") {
            GatewayCmd::Approval { id, choice } => {
                assert_eq!(id.as_deref(), Some("abc-1"));
                assert_eq!(choice, "session");
            }
            _ => panic!("expected Approval"),
        }
        assert!(app.work.panel.approval.is_none(), "the card is cleared");
    });
}

/// The stored `work_executor`, waiting (≤ 5 s) for the persistence worker
/// thread to write `want`.
fn stored_executor_eventually(want: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let store = awili_persist::Store::open_default().unwrap();
        let got = store
            .get_app_state(crate::persistence::WORK_EXECUTOR_KEY)
            .unwrap();
        if got.as_deref() == Some(want) || Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// L1c: a persisted legacy "gateway" launches as in-app with the one-time
/// notice, and the store is rewritten; the next launch shows no notice.
#[gpui::test]
fn l1c_launch_migrates_legacy_gateway_with_one_notice(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1c_launch_migrates_legacy_gateway_with_one_notice")
        .is_none()
    {
        return;
    }
    {
        let store = awili_persist::Store::open_default().unwrap();
        store
            .set_app_state(crate::persistence::WORK_EXECUTOR_KEY, "gateway")
            .unwrap();
    }
    let first = cx.new(AwiliApp::new);
    first.update(cx, |app, _cx| {
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(crate::local_first_migrate::MIGRATION_NOTICE)
        );
    });
    assert_eq!(
        stored_executor_eventually("in-app").as_deref(),
        Some("in-app")
    );
    let second = cx.new(AwiliApp::new);
    second.update(cx, |app, _cx| {
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        assert_eq!(app.toast.msg, None, "the notice shows once per install");
    });
}

/// L1c (S3-F06): a CLI-only launch keeps in-app (and a picked fleet
/// gateway); an uninstalled agent is replaced by the configured harness AND
/// that replacement is persisted, so the chip and `app_state` agree.
#[gpui::test]
fn l1c_cli_only_launch_keeps_in_app_and_persists_the_harness(cx: &mut gpui::TestAppContext) {
    if isolated(
        "work::local_first_tests::l1c_cli_only_launch_keeps_in_app_and_persists_the_harness",
    )
    .is_none()
    {
        return;
    }
    let glm = WorkExecutor::Agent {
        engine: CodeEngine::Glm,
        model: None,
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.choices.clear();
        app.onboarding_harness = Some(glm.clone());

        app.work.executor = WorkExecutor::InApp;
        assert!(app.recover_cli_only_executor());
        assert_eq!(app.work.executor, WorkExecutor::InApp);

        app.work.executor = WorkExecutor::Gateway;
        assert!(app.recover_cli_only_executor());
        assert_eq!(app.work.executor, WorkExecutor::Gateway);

        app.work.agents.codex_bin = Some("/nonexistent/awili-l1c/codex".into());
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Codex,
            model: None,
        };
        assert!(app.recover_cli_only_executor());
        assert_eq!(app.work.executor, glm);
    });
    assert_eq!(
        stored_executor_eventually("agent:glm").as_deref(),
        Some("agent:glm")
    );
}

/// L1c: a pick assigns, persists, closes the picker and runs U4f's
/// Ask-support note (an Ask conversation picking GLM gets the NoAsk toast).
#[gpui::test]
fn l1c_pick_persists_closes_and_notes_ask_support(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1c_pick_persists_closes_and_notes_ask_support").is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        app.work.executor_picker_open = true;
        let glm = WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None,
        };
        app.pick_work_executor(glm.clone(), cx);
        assert_eq!(app.work.executor, glm);
        assert!(!app.work.executor_picker_open);
        let toast = app.toast.msg.clone().expect("the NoAsk toast");
        assert!(toast.contains("GLM"), "{toast}");
    });
    assert_eq!(
        stored_executor_eventually("agent:glm").as_deref(),
        Some("agent:glm")
    );
}

/// The real picker must change the canonical route that Send reads, not only
/// the old ambient executor. While a run owns that ambient field, the same
/// choice is NEXT-only and must leave the captured owner visible.
#[gpui::test]
fn m2_picker_sets_the_canonical_next_target_without_retargeting_a_live_owner(
    cx: &mut gpui::TestAppContext,
) {
    if isolated(
        "work::local_first_tests::m2_picker_sets_the_canonical_next_target_without_retargeting_a_live_owner",
    )
    .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        // The chooser only accepts a target for a conversation the app lists
        // (`work_app` seeds only its view), so list CONV as a real Work thread
        // before it is made the active one.
        if app.conversations.get(CONV).is_none() {
            let id = app
                .conversations
                .create_kind(1, awili_core::convs::ConvKind::Work)
                .id;
            app.conversations.get_mut(&id).unwrap().id = CONV.into();
        }
        work_app(app);
        assert_eq!(app.sidebar.active_id(), CONV);
        let conv = CONV.to_string();
        let owner = crate::workshop_composer::ModelOwner::Work(conv.clone());
        assert!(app.set_composer_target(&owner, WorkExecutor::InApp, cx));

        let claude = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: Some("sonnet".into()),
        };
        app.pick_work_executor(claude.clone(), cx);
        assert_eq!(
            app.work.conversation_routes[&conv].next_target(),
            &claude,
            "Send dispatches this route, not a stale ambient chip"
        );

        app.work.running = true;
        app.work.executor = claude.clone();
        let codex = WorkExecutor::Agent {
            engine: CodeEngine::Codex,
            model: Some("gpt-5.6".into()),
        };
        app.pick_work_executor(codex.clone(), cx);
        assert_eq!(app.work.conversation_routes[&conv].next_target(), &codex);
        assert_eq!(
            app.work.executor, claude,
            "a busy picker change belongs to NEXT, never the active owner"
        );
    });
}

/// L1d: the chip and the Work header read the selected executor and the
/// shown run (S3-F06: the header said gateway while GLM was picked, and
/// nothing said what the next run would use).
#[gpui::test]
fn l1d_chip_and_header_follow_the_executor(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1d_chip_and_header_follow_the_executor").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        work_app(app);
        app.work.engine.label = "Local M".into();
        app.work.engine.base_url = "http://192.0.2.10:8851".into();
        app.work.executor = WorkExecutor::InApp;
        assert_eq!(app.work_executor_chip_label(), "In-app \u{00B7} Local M");

        app.work.panel.requested_identity = Some(WorkRunIdentity::HermesGateway);
        assert_eq!(
            app.work_provenance_lines(true),
            [
                "Requested: Hermes gateway \u{00B7} gateway chooses model",
                "Where: fleet gateway (remote) \u{00B7} can't see this Mac's folders",
                "Next run: In-app \u{00B7} Local M",
            ]
        );
        assert!(
            app.work_provenance_lines(false).is_empty(),
            "nothing for a run the panel does not own"
        );
        app.work.running = true;
        assert_eq!(
            app.work_provenance_lines(true).len(),
            2,
            "no Next run while running"
        );

        app.work.running = false;
        app.work.panel.requested_identity =
            Some(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None });
        assert_eq!(
            app.work_provenance_lines(true),
            [
                "Requested: Awili engine \u{00B7} m",
                "Where: Awili on this Mac \u{00B7} model at 192.0.2.10",
            ]
        );
        app.work.executor = WorkExecutor::Gateway;
        assert_eq!(app.work_executor_chip_label(), "Fleet gateway");
    });
}

/// L1e2: point `[engine]` at a configured (never contacted) endpoint.
fn configure_local(app: &mut AwiliApp) {
    app.work.engine.base_url = "http://127.0.0.1:9".into();
    app.work.engine.model = "m".into();
    app.work.engine.label = "Local M".into();
    app.work.executor = WorkExecutor::InApp;
}

/// L1e2: the next local probe answers `result` on the UI thread.
fn local_probe_answers(result: Result<(), String>) {
    crate::local_model_app::TEST_LOCAL_PROBE_RESULT.with(|slot| {
        *slot.borrow_mut() = Some(result);
    });
}

fn work_rows(app: &AwiliApp) -> Vec<(Role, String)> {
    app.views[CONV]
        .convo
        .messages()
        .iter()
        .map(|m| (m.role, m.text.clone()))
        .collect()
}

fn composer_text(app: &AwiliApp, cx: &mut Context<AwiliApp>) -> String {
    app.composer.read(cx).text().to_string()
}

/// No local model set up: a composer send (the real `submit_composer_draft`
/// → `dispatch_composer_text` path) sends nothing, starts nothing, keeps the
/// executor, and the card keeps the message.
#[gpui::test]
fn l1e2_not_configured_refuses_without_sending(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_not_configured_refuses_without_sending").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        app.work.executor = WorkExecutor::InApp;
        let fired = spawn_hook();
        app.composer.update(cx, |c, cx| c.set_text("hello", cx));
        app.submit_composer_draft(cx);
        assert_eq!(
            app.work.local.down,
            Some(crate::local_model_health::LocalDown {
                reason: crate::local_model_health::DownReason::NotConfigured,
                text: Some("hello".into()),
                conv_id: Some(CONV.into()),
            })
        );
        assert!(!fired.load(Ordering::SeqCst), "nothing was spawned");
        assert!(!app.work.running);
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        assert!(work_rows(app).is_empty(), "no User row: nothing was sent");
    });
}

/// The model does not answer: the text is parked behind the probe, then
/// kept on the card; nothing is sent and the executor never changes.
#[gpui::test]
fn l1e2_unreachable_parks_text_and_never_switches(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_unreachable_parks_text_and_never_switches").is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let fired = entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        let fired = spawn_hook();
        local_probe_answers(Err("connection refused".into()));
        app.submit_work_local_first("hello".into(), cx);
        assert!(
            app.work.local.health.pending_send.is_some(),
            "parked behind the probe"
        );
        fired
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        let down = app.work.local.down.clone().expect("the card");
        assert_eq!(
            down.reason,
            crate::local_model_health::DownReason::Unreachable("connection refused".into())
        );
        assert_eq!(down.text.as_deref(), Some("hello"));
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        assert!(!fired.load(Ordering::SeqCst), "nothing was spawned");
        assert_eq!(app.work.panel.requested_identity, None, "no run started");
        assert!(work_rows(app).is_empty());
    });
}

/// The probe says Up: the parked text is sent once, in-app.
#[gpui::test]
fn l1e2_up_probe_sends_in_app(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_up_probe_sends_in_app").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let fired = entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        let fired = spawn_hook();
        local_probe_answers(Ok(()));
        app.submit_work_local_first("hello".into(), cx);
        assert!(
            !fired.load(Ordering::SeqCst),
            "nothing sent before the probe"
        );
        fired
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(fired.load(Ordering::SeqCst), "sent after the probe");
        assert_eq!(
            app.work.worker_identity,
            Some(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None })
        );
        assert!(app.work.local.down.is_none());
        assert_eq!(
            work_rows(app)
                .iter()
                .filter(|(r, t)| *r == Role::User && t == "hello")
                .count(),
            1
        );
    });
}

/// A fresh Up sends without a probe.
#[gpui::test]
fn l1e2_fresh_up_sends_without_probe(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_fresh_up_sends_without_probe").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        assert!(app.work.local.health.begin_probe());
        app.work.local.health.finish_probe(Ok(()), Instant::now());
        let fired = spawn_hook();
        app.submit_work_local_first("hello".into(), cx);
        assert!(fired.load(Ordering::SeqCst));
        assert!(app.work.local.health.pending_send.is_none());
    });
}

/// A failed in-app turn (the real `Failed` arm) triggers one check; down →
/// the card offers the switch without a text to resend; nothing switches.
#[gpui::test]
fn l1e2_failed_turn_offers_switch_without_text(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_failed_turn_offers_switch_without_text").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        app.work.conv_id = Some(CONV.into());
        local_probe_answers(Err("x".into()));
        // D12: a failure with no owned-engine identity (an S5 policy
        // refusal) never probes: the one-shot probe answer stays unused.
        app.apply_work_update(WorkUpdate::Failed("refused by policy".into()), cx);
        assert!(
            crate::local_model_app::TEST_LOCAL_PROBE_RESULT.with(|s| s.borrow().is_some()),
            "no probe for a failure the in-app engine did not run"
        );
        app.work.panel.requested_identity =
            Some(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None });
        app.apply_work_update(WorkUpdate::Failed("stream ended".into()), cx);
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(
            app.work.local.down,
            Some(crate::local_model_health::LocalDown {
                reason: crate::local_model_health::DownReason::Unreachable("x".into()),
                text: None,
                conv_id: Some(CONV.into()),
            })
        );
        assert_eq!(app.work.executor, WorkExecutor::InApp);
    });
}

/// The user moved to another conversation before the probe finished: the
/// text goes to the clipboard, never into that conversation's composer.
#[gpui::test]
fn l1e2_moved_before_probe_goes_to_clipboard(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_moved_before_probe_goes_to_clipboard").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        local_probe_answers(Err("x".into()));
        app.submit_work_local_first("hello".into(), cx);
        app.set_active_conversation("another-conv");
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(app.work.local.down.is_none());
        assert_eq!(composer_text(app, cx), "");
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(crate::local_model_health::MOVED_ON_NOTICE)
        );
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("hello")
    );
}

/// Another executor was picked while the local model was checked: the
/// parked text is never sent through it; it goes back into the composer.
#[gpui::test]
fn l1e2_executor_changed_while_checking_restores_text(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_executor_changed_while_checking_restores_text")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        local_probe_answers(Ok(()));
        app.submit_work_local_first("hello".into(), cx);
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None,
        };
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert_eq!(composer_text(app, cx), "hello");
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(crate::local_model_app::LOCAL_EXECUTOR_CHANGED)
        );
        assert!(work_rows(app).is_empty(), "nothing was sent");
    });
}

/// Entering Work with in-app selected checks the local model once.
#[gpui::test]
fn l1e2_entering_work_probes_the_local_model(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_entering_work_probes_the_local_model").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        app.mode = Mode::Code;
        local_probe_answers(Ok(()));
        app.set_mode(Mode::Work, cx);
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(
            app.work.local.health.reach(),
            &crate::gateway_health::Reach::Up
        );
    });
}

/// No fleet host, port or model name in the L1 production code (their test
/// modules use sample names, so only the text before `#[cfg(test)] mod
/// tests` is checked).
#[test]
fn l1e2_no_fleet_host_in_code() {
    for (name, src) in [
        ("local_first.rs", include_str!("local_first.rs")),
        (
            "local_first_migrate.rs",
            include_str!("local_first_migrate.rs"),
        ),
        ("executor_labels.rs", include_str!("executor_labels.rs")),
        (
            "local_model_health.rs",
            include_str!("local_model_health.rs"),
        ),
        ("local_model_app.rs", include_str!("local_model_app.rs")),
    ] {
        let prod = &src[..src.find("#[cfg(test)]\nmod tests").unwrap_or(src.len())];
        let lower = prod.to_lowercase();
        for banned in [
            concat!("192", ".168."),
            concat!("88", "51"),
            concat!("gem", "ma"),
        ] {
            assert!(!lower.contains(banned), "{name} names {banned}");
        }
    }
}

/// "Send with <cloud> instead": one click switches (persisted) and sends
/// the kept text exactly once down the normal agent path. With no folder
/// bound, that path's own refusal proves where the text went, and no
/// process is spawned.
#[gpui::test]
fn l1e2_use_cloud_switches_persists_and_sends_once(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_use_cloud_switches_persists_and_sends_once")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        app.work.local.cloud_escalation = Some("agent:codex".into());
        app.work.local.down = Some(crate::local_model_health::LocalDown {
            reason: crate::local_model_health::DownReason::Unreachable("x".into()),
            text: Some("hello".into()),
            conv_id: Some(CONV.into()),
        });
        app.local_use_cloud(cx);
        assert_eq!(
            app.work.executor,
            WorkExecutor::Agent {
                engine: CodeEngine::Codex,
                model: None
            }
        );
        assert!(app.work.local.down.is_none());
        let users: Vec<_> = work_rows(app)
            .into_iter()
            .filter(|(r, _)| *r == Role::User)
            .collect();
        assert_eq!(users, vec![(Role::User, "hello".to_string())]);
        // No folder is chosen: the helper still starts (from the home folder)
        // instead of the run stopping at a "choose a folder" gate.
        assert!(
            app.work
                .panel
                .error
                .as_deref()
                .is_none_or(|e| !e.contains("working folder")),
            "{:?}",
            app.work.panel.error
        );
        assert!(app.work.running, "the cloud helper was started once");
    });
    assert_eq!(
        stored_executor_eventually("agent:codex").as_deref(),
        Some("agent:codex")
    );
}

/// The status run names the in-app model and never claims the gateway.
#[gpui::test]
fn l1e2_status_segment_names_the_local_model(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_status_segment_names_the_local_model").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        work_app(app);
        configure_local(app);
        app.work.hermes_status = Some(Ok("ok".to_string()));
        assert!(app.work.local.health.begin_probe());
        app.work.local.health.finish_probe(Ok(()), Instant::now());
        let parts: Vec<String> = app
            .status_parts_with_executor()
            .into_iter()
            .map(|(t, _)| t)
            .collect();
        assert!(
            parts.contains(&"\u{25CF} In-app \u{00B7} Local M".to_string()),
            "{parts:?}"
        );
        assert!(parts.iter().all(|t| !t.contains("gateway")), "{parts:?}");
    });
}

/// The card renders in the Work panel for its conversation only.
#[gpui::test]
fn l1e2_down_card_renders_in_work_panel(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_down_card_renders_in_work_panel").is_none() {
        return;
    }
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        work_app(app);
        configure_local(app);
        app.work.local.down = Some(crate::local_model_health::LocalDown {
            reason: crate::local_model_health::DownReason::NotConfigured,
            text: Some("hello".into()),
            conv_id: Some(CONV.into()),
        });
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window
            .debug_bounds(crate::local_model_app::LOCAL_DOWN_CARD_SELECTOR)
            .is_some(),
        "the card shows in its conversation"
    );
    view.update(window, |app, cx| {
        app.work.local.down.as_mut().unwrap().conv_id = Some("another-conv".into());
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window
            .debug_bounds(crate::local_model_app::LOCAL_DOWN_CARD_SELECTOR)
            .is_none(),
        "never in another conversation"
    );
}

/// L1e2a-FIX1: a down card in conversation "conv-a" holding a message.
fn held_card_in_conv_a() -> crate::local_model_health::LocalDown {
    crate::local_model_health::LocalDown {
        reason: crate::local_model_health::DownReason::Unreachable("x".into()),
        text: Some("kept in A".into()),
        conv_id: Some("conv-a".into()),
    }
}

/// L1e2a-FIX1: a live config reload that moves `[engine] base_url`.
fn reload_engine_url(app: &mut AwiliApp, base_url: &str) {
    let cfg = awili_config::Config {
        engine: awili_config::EngineConfig {
            base_url: base_url.into(),
            ..app.work.engine.clone()
        },
        ..Default::default()
    };
    AwiliApp::refresh_work_endpoints(&mut app.work, &cfg);
}

/// L1e2a-FIX1: a card holding a message in another conversation (which this
/// one cannot see) does not block a send here, and keeps its message.
#[gpui::test]
fn l1e2_fix1_cross_conversation_send_is_not_blocked(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_fix1_cross_conversation_send_is_not_blocked")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        app.work.local.down = Some(held_card_in_conv_a());
        assert!(app.work.local.health.begin_probe());
        app.work.local.health.finish_probe(Ok(()), Instant::now());
        let fired = spawn_hook();
        app.submit_work_local_first("hello".into(), cx);
        assert!(fired.load(Ordering::SeqCst), "sent in this conversation");
        assert_eq!(composer_text(app, cx), "", "not put back in the composer");
        assert_ne!(
            app.toast.msg.as_deref(),
            Some(crate::local_model_app::CARD_HOLDS_MESSAGE)
        );
        assert_eq!(
            app.work.local.down,
            Some(held_card_in_conv_a()),
            "the other conversation's card keeps its message"
        );
    });
}

/// L1e2a-FIX1: a send here that needs the card while another conversation's
/// card holds a message never replaces that card; the new text goes back to
/// this composer.
#[gpui::test]
fn l1e2_fix1_other_conversation_card_is_never_replaced(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_fix1_other_conversation_card_is_never_replaced")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        app.work.local.down = Some(held_card_in_conv_a());
        app.work
            .local
            .health
            .finish_probe(Err("down".into()), Instant::now());
        let fired = spawn_hook();
        app.submit_work_local_first("hello".into(), cx);
        assert!(!fired.load(Ordering::SeqCst), "nothing was sent");
        assert_eq!(app.work.local.down, Some(held_card_in_conv_a()));
        assert_eq!(composer_text(app, cx), "hello");
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(crate::local_model_app::OTHER_CARD_HOLDS_MESSAGE)
        );
    });
}

/// L1e2a-FIX1: `[engine] base_url` changes while a probe is in flight: the
/// old endpoint's Up is discarded, the gate probes the new endpoint, and its
/// Down keeps the message on the card. Nothing is sent.
#[gpui::test]
fn l1e2_fix1_endpoint_change_discards_inflight_probe(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_fix1_endpoint_change_discards_inflight_probe")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let fired = entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        let fired = spawn_hook();
        // The old endpoint's probe answers Up, but only after the reload.
        local_probe_answers(Ok(()));
        app.submit_work_local_first("hello".into(), cx);
        assert!(app.work.local.health.pending_send.is_some());
        reload_engine_url(app, "http://127.0.0.1:10");
        // The new endpoint does not answer.
        local_probe_answers(Err("new endpoint down".into()));
        fired
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(
            !fired.load(Ordering::SeqCst),
            "never sent on the old endpoint's answer"
        );
        assert!(
            crate::local_model_app::TEST_LOCAL_PROBE_RESULT.with(|s| s.borrow().is_none()),
            "the new endpoint was probed"
        );
        assert_eq!(
            app.work.local.down,
            Some(crate::local_model_health::LocalDown {
                reason: crate::local_model_health::DownReason::Unreachable(
                    "new endpoint down".into()
                ),
                text: Some("hello".into()),
                conv_id: Some(CONV.into()),
            })
        );
        assert!(work_rows(app).is_empty(), "nothing was sent");
    });
}

/// L1e2a-FIX1: a fresh Up stops counting when `[engine] base_url` changes
/// (a reload that keeps the URL keeps it): the next send probes the new
/// endpoint first, and its Down sends nothing.
#[gpui::test]
fn l1e2_fix1_endpoint_change_voids_fresh_up(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_fix1_endpoint_change_voids_fresh_up").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let fired = entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        assert!(app.work.local.health.begin_probe());
        app.work.local.health.finish_probe(Ok(()), Instant::now());
        let same = app.work.engine.base_url.clone();
        reload_engine_url(app, &same);
        assert_eq!(
            app.work.local.health.reach(),
            &crate::gateway_health::Reach::Up,
            "the same URL keeps its result"
        );
        reload_engine_url(app, "http://127.0.0.1:10");
        let fired = spawn_hook();
        local_probe_answers(Err("new endpoint down".into()));
        app.submit_work_local_first("hello".into(), cx);
        assert!(
            !fired.load(Ordering::SeqCst),
            "no send before the new endpoint is checked"
        );
        assert!(app.work.local.health.pending_send.is_some());
        fired
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(!fired.load(Ordering::SeqCst), "nothing was sent");
        assert_eq!(
            app.work.local.down.as_ref().map(|d| &d.reason),
            Some(&crate::local_model_health::DownReason::Unreachable(
                "new endpoint down".into()
            ))
        );
        assert!(work_rows(app).is_empty());
    });
}

/// L1e2a-FIX2: a failed in-app turn starts a check, `[engine] base_url`
/// changes before it answers, and the old answer arrives: a probe of the new
/// endpoint starts, and its Down resolves the check (the text-less card).
#[gpui::test]
fn l1e2_fix2_failed_turn_check_follows_endpoint_change(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1e2_fix2_failed_turn_check_follows_endpoint_change")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        app.work.conv_id = Some(CONV.into());
        app.work.panel.requested_identity =
            Some(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None });
        // The old endpoint's check answers Up, but only after the reload.
        local_probe_answers(Ok(()));
        app.apply_work_update(WorkUpdate::Failed("stream ended".into()), cx);
        assert!(app.work.local.after_failure, "the failed turn's check runs");
        reload_engine_url(app, "http://127.0.0.1:10");
        // The new endpoint does not answer.
        local_probe_answers(Err("new endpoint down".into()));
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(
            crate::local_model_app::TEST_LOCAL_PROBE_RESULT.with(|s| s.borrow().is_none()),
            "the new endpoint was probed"
        );
        assert!(!app.work.local.after_failure, "the check resolved");
        assert_eq!(
            app.work.local.down,
            Some(crate::local_model_health::LocalDown {
                reason: crate::local_model_health::DownReason::Unreachable(
                    "new endpoint down".into()
                ),
                text: None,
                conv_id: Some(CONV.into()),
            })
        );
        assert_eq!(app.work.executor, WorkExecutor::InApp);
    });
}

/// L1-INT1: a gateway turn's Stop is pending (what `gateway_stop` and M1a's
/// deadline arm leave: `gateway_stop_pending`, generation 1, not running),
/// the user switches to In-app and sends "hello": both pre-send gates defer
/// to the Stop barrier, which parks it.
fn park_in_app_behind_stop(app: &mut AwiliApp, cx: &mut Context<AwiliApp>) {
    work_app(app);
    app.work.gateway_stop_pending = true;
    app.work.gateway_stop_gen = 1;
    app.work.running = false;
    configure_local(app);
    app.submit_work_local_first("hello".into(), cx);
    assert_eq!(
        app.work.queued_after_stop.as_ref().map(|q| q.text.as_str()),
        Some("hello"),
        "parked behind the Stop"
    );
}

/// L1-INT1: the worker's Stop acknowledgement (`Terminal { Stopped }`).
fn stop_ack(app: &mut AwiliApp, cx: &mut Context<AwiliApp>) {
    app.apply_work_update(
        WorkUpdate::Terminal {
            status: RunStatus::Stopped,
            output: None,
            model: None,
        },
        cx,
    );
}

fn user_rows(app: &AwiliApp, text: &str) -> usize {
    work_rows(app)
        .iter()
        .filter(|(r, t)| *r == Role::User && t == text)
        .count()
}

/// L1-INT1: Stop → In-app → send (parked) → Stop ack with the local model
/// down: the replay is checked first, nothing runs, and the card holds the
/// text in its conversation.
#[gpui::test]
fn l1_int1_stop_ack_replay_with_model_down_keeps_the_card(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1_int1_stop_ack_replay_with_model_down_keeps_the_card")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let fired = entity.update(cx, |app, cx| {
        park_in_app_behind_stop(app, cx);
        let fired = spawn_hook();
        local_probe_answers(Err("connection refused".into()));
        stop_ack(app, cx);
        assert!(app.work.queued_after_stop.is_none(), "the slot is taken");
        assert!(
            !fired.load(Ordering::SeqCst),
            "nothing runs before the check"
        );
        fired
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(!fired.load(Ordering::SeqCst), "nothing was spawned");
        assert_eq!(
            app.work.local.down,
            Some(crate::local_model_health::LocalDown {
                reason: crate::local_model_health::DownReason::Unreachable(
                    "connection refused".into()
                ),
                text: Some("hello".into()),
                conv_id: Some(CONV.into()),
            })
        );
        assert_eq!(user_rows(app, "hello"), 0, "nothing was sent");
        assert_eq!(app.work.executor, WorkExecutor::InApp);
    });
}

/// L1-INT1: the same sequence with the local model up: the replay is
/// checked, then sent once, in-app.
#[gpui::test]
fn l1_int1_stop_ack_replay_with_model_up_sends_once(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1_int1_stop_ack_replay_with_model_up_sends_once")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let fired = entity.update(cx, |app, cx| {
        park_in_app_behind_stop(app, cx);
        let fired = spawn_hook();
        local_probe_answers(Ok(()));
        stop_ack(app, cx);
        assert!(
            !fired.load(Ordering::SeqCst),
            "nothing runs before the check"
        );
        assert!(app.work.local.health.pending_send.is_some());
        fired
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(fired.load(Ordering::SeqCst), "sent after the check");
        assert_eq!(
            app.work.worker_identity,
            Some(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None })
        );
        assert_eq!(user_rows(app, "hello"), 1, "sent exactly once");
        assert!(app.work.local.down.is_none());
    });
}

/// L1-INT1: M1a's abandoned Stop (`expire_gateway_stop`, the 15 s deadline)
/// replays through the same gate: with the model down nothing runs and the
/// card holds the text.
#[gpui::test]
fn l1_int1_abandoned_stop_replay_passes_the_local_gate(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1_int1_abandoned_stop_replay_passes_the_local_gate")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let fired = entity.update(cx, |app, cx| {
        park_in_app_behind_stop(app, cx);
        let fired = spawn_hook();
        local_probe_answers(Err("connection refused".into()));
        app.expire_gateway_stop(1, cx);
        assert!(!app.work.gateway_stop_pending, "the Stop was abandoned");
        assert!(app.work.queued_after_stop.is_none(), "the slot is taken");
        assert!(
            !fired.load(Ordering::SeqCst),
            "nothing runs before the check"
        );
        fired
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(!fired.load(Ordering::SeqCst), "nothing was spawned");
        assert_eq!(
            app.work.local.down.as_ref().and_then(|d| d.text.as_deref()),
            Some("hello")
        );
        assert_eq!(user_rows(app, "hello"), 0, "nothing was sent");
    });
}

/// L1-INT1: the replay keeps its conversation: the user moved to another
/// Work conversation before the Stop ack, the model is fresh Up, and the
/// prompt runs in the conversation it was typed in.
#[gpui::test]
fn l1_int1_replay_keeps_its_conversation(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::l1_int1_replay_keeps_its_conversation").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        park_in_app_behind_stop(app, cx);
        assert!(app.work.local.health.begin_probe());
        app.work.local.health.finish_probe(Ok(()), Instant::now());
        app.views.insert(
            "another-conv".into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation("another-conv");
        let fired = spawn_hook();
        stop_ack(app, cx);
        assert!(fired.load(Ordering::SeqCst), "fresh Up: sent at once");
        assert_eq!(app.work.conv_id.as_deref(), Some(CONV));
        assert_eq!(
            user_rows(app, "hello"),
            1,
            "in the conversation it was typed in"
        );
    });
}

/// R1-INT2: the R1a fallback notice is not on screen.
fn no_not_started_toast(app: &AwiliApp) {
    assert_ne!(
        app.toast.msg.as_deref(),
        Some(crate::submit_guard::NOT_STARTED_NOTICE),
        "no false \"Not sent\" notice"
    );
}

/// R1-INT2: a composer send (the real `submit_composer_draft`, R1a's guard
/// armed) while the local model is freshly Down: the card holds the text and
/// the composer stays empty (no second copy, no false notice).
#[gpui::test]
fn r1_int2_composer_send_model_down_card_holds_the_only_copy(cx: &mut gpui::TestAppContext) {
    if isolated(
        "work::local_first_tests::r1_int2_composer_send_model_down_card_holds_the_only_copy",
    )
    .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        assert!(app.work.local.health.begin_probe());
        app.work
            .local
            .health
            .finish_probe(Err("connection refused".into()), Instant::now());
        let fired = spawn_hook();
        app.composer.update(cx, |c, cx| c.set_text("hello", cx));
        app.submit_composer_draft(cx);
        assert_eq!(
            app.work.local.down,
            Some(crate::local_model_health::LocalDown {
                reason: crate::local_model_health::DownReason::Unreachable(
                    "connection refused".into()
                ),
                text: Some("hello".into()),
                conv_id: Some(CONV.into()),
            })
        );
        assert_eq!(composer_text(app, cx), "", "the card holds the only copy");
        no_not_started_toast(app);
        assert!(!fired.load(Ordering::SeqCst), "nothing was spawned");
    });
}

/// R1-INT2: a composer send that needs a probe: the text is parked (the
/// composer stays empty, no notice), and on Up it is sent exactly once.
#[gpui::test]
fn r1_int2_composer_send_probe_parks_then_sends_once(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::r1_int2_composer_send_probe_parks_then_sends_once")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let fired = entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        let fired = spawn_hook();
        local_probe_answers(Ok(()));
        app.composer.update(cx, |c, cx| c.set_text("hello", cx));
        app.submit_composer_draft(cx);
        assert!(
            app.work.local.health.pending_send.is_some(),
            "parked behind the probe"
        );
        assert_eq!(composer_text(app, cx), "", "parked, not handed back");
        no_not_started_toast(app);
        fired
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(fired.load(Ordering::SeqCst), "sent after the probe");
        assert_eq!(user_rows(app, "hello"), 1, "sent exactly once");
        assert_eq!(composer_text(app, cx), "", "no copy left to send again");
    });
}

/// R1-INT2: a composer send while this conversation's card already holds a
/// message: exactly one copy of the new text goes back, with
/// CARD_HOLDS_MESSAGE.
#[gpui::test]
fn r1_int2_composer_send_card_holds_message_one_copy_back(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::r1_int2_composer_send_card_holds_message_one_copy_back")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        let held = crate::local_model_health::LocalDown {
            reason: crate::local_model_health::DownReason::Unreachable("x".into()),
            text: Some("kept".into()),
            conv_id: Some(CONV.into()),
        };
        app.work.local.down = Some(held.clone());
        let fired = spawn_hook();
        app.composer.update(cx, |c, cx| c.set_text("hello", cx));
        app.submit_composer_draft(cx);
        assert_eq!(composer_text(app, cx), "hello", "exactly one copy back");
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(crate::local_model_app::CARD_HOLDS_MESSAGE)
        );
        assert_eq!(
            app.work.local.down,
            Some(held),
            "the card keeps its message"
        );
        assert!(!fired.load(Ordering::SeqCst), "nothing was spawned");
    });
}

/// R1-INT2 (M1a × R1a): a gateway Stop is pending and a first composer send
/// is parked behind it; a second composer send is refused back to the
/// composer exactly once, with M1a's toast (no false "Not sent" notice).
#[gpui::test]
fn r1_int2_composer_send_already_parked_one_copy_back(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::r1_int2_composer_send_already_parked_one_copy_back")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        app.work.gateway_stop_pending = true;
        app.work.running = false;
        app.composer.update(cx, |c, cx| c.set_text("first", cx));
        app.submit_composer_draft(cx);
        assert_eq!(
            app.work.queued_after_stop.as_ref().map(|q| q.text.as_str()),
            Some("first"),
            "the first send is parked behind the Stop"
        );
        assert_eq!(composer_text(app, cx), "", "parked, not handed back");
        app.composer.update(cx, |c, cx| c.set_text("second", cx));
        app.submit_composer_draft(cx);
        assert_eq!(
            app.work.queued_after_stop.as_ref().map(|q| q.text.as_str()),
            Some("first"),
            "the first parked send keeps its place"
        );
        assert_eq!(composer_text(app, cx), "second", "exactly one copy back");
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(crate::gateway_stop_deadline::ALREADY_PARKED_TOAST)
        );
        no_not_started_toast(app);
        assert_eq!(user_rows(app, "second"), 0, "nothing was sent");
    });
}

/// One in-app turn on a worker spawned for `spawn_url`; the owner then
/// re-applies a config whose `[engine] base_url` is `config_url` while that
/// worker lives on; the turn ends with 1,000/42 tokens. Returns the ledger
/// line the turn wrote.
fn rc1f4b_fix1_turn(
    cx: &mut gpui::TestAppContext,
    home: &std::path::Path,
    spawn_url: &str,
    config_url: &str,
) -> awili_selftest::graduation::LedgerEntry {
    let ledger = home.join(format!("ledger-{}.jsonl", spawn_url.len()));
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        app.work.graduation.set_ledger(ledger.to_str().unwrap());
        app.work.engine.base_url = spawn_url.into();
        app.work.engine.model = "m".into();
        app.work.executor = WorkExecutor::InApp;
        let fired = spawn_hook();
        app.submit_work_run("one".into(), cx);
        assert!(fired.load(Ordering::SeqCst), "the owned engine was spawned");
        assert_eq!(app.work.panel.error, None);
        let mut cfg = awili_config::Config::networkless();
        cfg.engine.base_url = config_url.into();
        cfg.engine.model = "m".into();
        cfg.graduation.ledger = ledger.to_str().unwrap().into();
        app.apply_runtime_config(&cfg, cx);
        app.work.panel.status = Some(RunStatus::Completed);
        app.work.panel.usage = Some(awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: 1_000,
            output_tokens: 42,
        });
        app.work_record_graduation();
    });
    let text = std::fs::read_to_string(&ledger).unwrap();
    let last = text.lines().last().expect("one ledger line");
    serde_json::from_str(last).unwrap()
}

/// RC1-F4b-FIX1 (Codex T2 P2 on 0925578): the in-app locality is the
/// endpoint of the worker that served the turn. A config re-apply that moves
/// `[engine] base_url` while the worker lives on changes nothing for its
/// turns: home worker → local tokens even after a cloud URL is configured;
/// cloud worker → today's unknown booking even after a home URL.
#[gpui::test]
fn rc1f4b_fix1_locality_follows_the_live_worker_not_the_config(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::local_first_tests::rc1f4b_fix1_locality_follows_the_live_worker_not_the_config",
    ) else {
        return;
    };
    let at_home = rc1f4b_fix1_turn(
        cx,
        &home,
        "http://10.77.68.94:8844",
        "https://api.example.com",
    );
    assert_eq!(at_home.executor, "owned");
    assert_eq!(
        (at_home.frontier_tokens, at_home.local_tokens),
        (0, 1_042),
        "the home worker's turn stays local"
    );
    let away = rc1f4b_fix1_turn(
        cx,
        &home,
        "https://api.example.com/x",
        "http://10.77.68.94:8844",
    );
    assert_eq!(away.executor, "owned");
    assert_eq!(
        (away.frontier_tokens, away.local_tokens, away.notes),
        (0, 0, None),
        "the cloud worker's turn keeps today's unknown booking"
    );
}

/// RC1-N (owner decision 2026-10-04, round 4 Major 2 "b"): with the in-app
/// engine as the Default Work helper (`[work] default_executor = "in-app"`),
/// "+ New conversation" starts on it in "Ask before changes", and every new
/// Work conversation starts in "Ask before changes" whatever the helper —
/// even right after the owner put another conversation in "Runs without
/// asking". The stored mode is the default (`[work] default_approval_mode`,
/// absent = Ask). What each helper can DO with Ask is its own: the in-app
/// engine asks per action, Claude and Codex plan first, GLM and DeepSeek
/// cannot ask (their chip says so).
#[gpui::test]
fn rc1n_a_new_work_conversation_starts_in_ask_on_every_helper(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::local_first_tests::rc1n_a_new_work_conversation_starts_in_ask_on_every_helper",
    ) else {
        return;
    };
    let cfg = home.join(".config/awili");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        "[work]\ndefault_executor = \"in-app\"\n",
    )
    .unwrap();
    assert_eq!(
        awili_work::ApprovalMode::from_config(None),
        awili_work::ApprovalMode::AskBeforeChanges,
        "the default mode, unset, is Ask"
    );
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Work;
        assert_eq!(app.work.default_helper.as_deref(), Some("in-app"));
        // Start elsewhere, with an earlier conversation in Auto.
        app.set_work_executor_persisted(WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None,
        });
        let (earlier, _) = app.fresh_conversation(ConvKind::Work);
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(earlier),
            awili_work::ApprovalMode::Auto,
        );
        let (id, _) = app.fresh_conversation(ConvKind::Work);
        app.set_active_conversation(&id);
        assert_eq!(app.work.executor, WorkExecutor::InApp, "the default helper");
        let key = crate::ask_mode::AskKey::Work(id);
        assert_eq!(
            app.work.ask_modes.mode_for(&key),
            awili_work::ApprovalMode::AskBeforeChanges
        );
        assert_eq!(
            crate::ask_mode::effective(
                app.work.ask_modes.mode_for(&key),
                crate::ask_mode::work_support(&app.work.executor, app.work_engine_kind()),
            ),
            crate::ask_mode::Effective::AskPerAction,
            "Gemma asks before every change"
        );
        for (helper, asks) in [
            ("agent:claude", true),
            ("agent:codex", true),
            ("agent:glm", false),
            ("agent:dsh", false),
        ] {
            app.work.default_helper = Some(helper.into());
            let (id, _) = app.fresh_conversation(ConvKind::Work);
            app.set_active_conversation(&id);
            let key = crate::ask_mode::AskKey::Work(id);
            assert_eq!(
                app.work.ask_modes.mode_for(&key),
                awili_work::ApprovalMode::AskBeforeChanges,
                "{helper}: a new conversation starts in Ask"
            );
            let eff = crate::ask_mode::effective(
                app.work.ask_modes.mode_for(&key),
                crate::ask_mode::work_support(&app.work.executor, app.work_engine_kind()),
            );
            assert_eq!(
                matches!(eff, crate::ask_mode::Effective::AskPlanFirst),
                asks,
                "{helper}: {eff:?}"
            );
        }
    });
}

/// RC1-N: the default mode stays the owner's to set — `[work]
/// default_approval_mode = "auto"` (Settings asks to confirm it) makes new
/// Work conversations start in "Runs without asking"; unset, they start in
/// "Ask before changes" (the test above). Owner principle: his control.
#[gpui::test]
fn rc1n_an_explicit_auto_default_is_the_owners_choice(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::local_first_tests::rc1n_an_explicit_auto_default_is_the_owners_choice")
    else {
        return;
    };
    let cfg = home.join(".config/awili");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        "[work]\ndefault_executor = \"in-app\"\ndefault_approval_mode = \"auto\"\n",
    )
    .unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Work;
        let (id, _) = app.fresh_conversation(ConvKind::Work);
        assert_eq!(
            app.work
                .ask_modes
                .mode_for(&crate::ask_mode::AskKey::Work(id)),
            awili_work::ApprovalMode::Auto
        );
    });
}

/// RC2-RUN (round 3 Minor 12): one missed check when entering Work (a busy
/// or loading model) does not put "✗ Local model not answering" in the
/// status bar; the model is checked again a little later and shows as up.
/// Two misses in a row do say it; a completed in-app turn clears it.
#[gpui::test]
fn rc2_run_one_missed_idle_check_is_not_down(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::rc2_run_one_missed_idle_check_is_not_down").is_none() {
        return;
    }
    let status = |app: &AwiliApp| {
        crate::local_model_health::in_app_part(
            &app.work.engine.display_label(),
            app.work.engine.is_configured(),
            app.work.local.health.reach(),
            app.work.local.health.probing(),
        )
        .0
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        app.mode = Mode::Code;
        local_probe_answers(Err("timed out".into()));
        app.set_mode(Mode::Work, cx);
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(
            app.work.local.health.reach(),
            &crate::gateway_health::Reach::Unknown,
            "one miss is not recorded as down"
        );
        assert!(!status(app).contains("not answering"), "{}", status(app));
        local_probe_answers(Ok(()));
    });
    cx.executor()
        .advance_clock(crate::local_model_health::LOCAL_RECHECK_AFTER);
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert_eq!(
            app.work.local.health.reach(),
            &crate::gateway_health::Reach::Up,
            "checked again, and it answers"
        );
        // Two misses in a row: now it is not answering.
        local_probe_answers(Err("refused".into()));
        app.start_local_probe(cx);
    });
    cx.run_until_parked();
    entity.update(cx, |_app, _cx| local_probe_answers(Err("refused".into())));
    cx.executor()
        .advance_clock(crate::local_model_health::LOCAL_RECHECK_AFTER);
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(status(app).contains("not answering"), "{}", status(app));
        // A completed in-app turn is proof it answers.
        app.work.running = true;
        app.work.panel.requested_identity =
            Some(crate::work_run_identity::WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None });
        app.work.panel.status = Some(awili_gateway::run_events::RunStatus::Completed);
        app.apply_work_update(crate::work::WorkUpdate::TurnComplete, cx);
        assert_eq!(
            app.work.local.health.reach(),
            &crate::gateway_health::Reach::Up
        );
    });
}

/// RC2-RUNF (review P3): the old local endpoint missed one check on its own,
/// then a config reload moves `[engine] base_url` before the second check.
/// The new endpoint's first miss is still only one miss — it is checked
/// again a little later (and answers), never marked "not answering" at once.
#[gpui::test]
fn rc2runf_a_reload_between_checks_keeps_the_second_check(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::rc2runf_a_reload_between_checks_keeps_the_second_check")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_app(app);
        configure_local(app);
        app.mode = Mode::Code;
        local_probe_answers(Err("timed out".into()));
        app.set_mode(Mode::Work, cx);
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(
            app.work.local.rechecking,
            "setup: the old endpoint missed once"
        );
        reload_engine_url(app, "http://127.0.0.1:10");
        assert!(
            !app.work.local.rechecking,
            "the old endpoint's miss is forgotten with it"
        );
        local_probe_answers(Err("refused".into()));
        app.start_local_probe(cx);
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(
            app.work.local.health.reach(),
            &crate::gateway_health::Reach::Unknown,
            "the new endpoint's first miss is not recorded as down"
        );
        local_probe_answers(Ok(()));
    });
    cx.executor()
        .advance_clock(crate::local_model_health::LOCAL_RECHECK_AFTER);
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(
            app.work.local.health.reach(),
            &crate::gateway_health::Reach::Up,
            "checked again, and it answers"
        );
    });
}

/// M2: the chooser changes only a conversation's NEXT target. A send parked
/// for the in-app lane, then a chooser change to another harness while the
/// probe runs: the probe answers "up", yet nothing runs on the new target (nor
/// on the old). The text is back in its own conversation's box, the notice
/// says why, and the new selection is untouched.
#[gpui::test]
fn m2_chooser_change_while_checking_hands_the_text_back(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::m2_chooser_change_while_checking_hands_the_text_back")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let glm = WorkExecutor::Agent {
        engine: CodeEngine::Glm,
        model: None,
    };
    let (conv, fired) = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = None;
        configure_local(app);
        let conv = app.sidebar.active_id().to_string();
        let owner = crate::workshop_composer::ModelOwner::Work(conv.clone());
        assert!(app.set_composer_target(&owner, WorkExecutor::InApp, cx));
        let fired = spawn_hook();
        local_probe_answers(Ok(()));
        app.composer.update(cx, |c, cx| c.set_text("hello", cx));
        app.submit_composer_draft(cx);
        assert!(
            app.work.local.health.pending_send.is_some(),
            "parked for the in-app lane"
        );
        assert!(app.set_composer_target(&owner, glm.clone(), cx));
        (conv, fired)
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        let owner = crate::workshop_composer::ModelOwner::Work(conv.clone());
        assert!(app.work.local.health.pending_send.is_none());
        assert_eq!(composer_text(app, cx), "hello", "back in its own box");
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(crate::local_model_app::LOCAL_EXECUTOR_CHANGED)
        );
        assert!(!fired.load(Ordering::SeqCst), "no worker started");
        assert!(!app.work.running, "no run on the new target either");
        assert!(app.views[&conv].convo.messages().is_empty(), "no row");
        assert_eq!(app.composer_target_for(&owner), Some(glm.clone()));
        assert_eq!(app.sidebar.active_id(), conv);
    });
}

/// The same hand-back when the saved target becomes unavailable while the
/// probe runs: the raw saved value is kept and nothing is sent.
#[gpui::test]
fn m2_unavailable_target_while_checking_hands_the_text_back(cx: &mut gpui::TestAppContext) {
    if isolated("work::local_first_tests::m2_unavailable_target_while_checking_hands_the_text_back")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let (conv, fired) = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = None;
        configure_local(app);
        let conv = app.sidebar.active_id().to_string();
        let owner = crate::workshop_composer::ModelOwner::Work(conv.clone());
        assert!(app.set_composer_target(&owner, WorkExecutor::InApp, cx));
        let fired = spawn_hook();
        local_probe_answers(Ok(()));
        app.composer.update(cx, |c, cx| c.set_text("hello", cx));
        app.submit_composer_draft(cx);
        assert!(app.work.local.health.pending_send.is_some());
        app.work
            .unavailable_conversation_routes
            .insert(conv.clone(), "{\"version\":99}".into());
        (conv, fired)
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        let owner = crate::workshop_composer::ModelOwner::Work(conv.clone());
        assert_eq!(composer_text(app, cx), "hello");
        assert!(!fired.load(Ordering::SeqCst) && !app.work.running);
        assert!(app.views[&conv].convo.messages().is_empty());
        assert_eq!(app.composer_target_for(&owner), None, "still unavailable");
        assert_eq!(
            app.work
                .unavailable_conversation_routes
                .get(&conv)
                .map(String::as_str),
            Some("{\"version\":99}"),
            "the saved value is untouched"
        );
    });
}
