//! RC1-I3 proofs (RC1 round 3/3b Minors 8 and 10): the Work panel never
//! names the in-app model by the model server's file path, and the in-app
//! engine's connector-server internals ("mcp:awili-connectors: not prepared:
//! NoAdvertisedEpoch") never reach the Progress panel.

use super::gateway_queue_tests::isolated;
use super::*;
use crate::work_run_identity::WorkRunIdentity;

/// The connector-server lines of the in-app engine are not progress; other
/// MCP lines still are.
#[test]
fn rc1i3_connector_internals_are_not_progress() {
    for line in [
        "mcp:awili-connectors: not prepared: NoAdvertisedEpoch",
        "mcp:awili-connectors: disconnected",
    ] {
        let ups = work_event_to_updates(WorkEvent::StatusUpdate {
            kind: Some("mcp".into()),
            message: Some(line.into()),
        });
        assert!(ups.is_empty(), "{line}: {ups:?}");
    }
    // RC2-RUN (round 6 Minor 1, deliberate change): a healthy server's
    // status is an internal too; only a real problem shows, in plain words.
    let healthy = work_event_to_updates(WorkEvent::StatusUpdate {
        kind: Some("mcp".into()),
        message: Some("mcp:mac-shell: ready (3 tools)".into()),
    });
    assert!(healthy.is_empty(), "{healthy:?}");
    let kept = work_event_to_updates(WorkEvent::StatusUpdate {
        kind: Some("mcp".into()),
        message: Some("mac-shell: tool \"x\" dropped: BadSchema".into()),
    });
    assert_eq!(kept.len(), 1, "{kept:?}");
}

/// The right panel's "Requested" and "Backend-reported" lines for an in-app
/// run name the model by its display label, never its path.
#[gpui::test]
fn rc1i3_in_app_identity_lines_name_the_model(cx: &mut gpui::TestAppContext) {
    if isolated("work::rc1i3_tests::rc1i3_in_app_identity_lines_name_the_model").is_none() {
        return;
    }
    let path = "/Users/someone/models/gemma-4-26b-a4b-it-qat-mlx-4bit";
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        app.work.executor = WorkExecutor::InApp;
        app.work.engine.model = path.into();
        app.work.engine.base_url = "http://192.0.2.10:8844".into();
        app.work.engine.label = "Gemma 4 26B (Studio)".into();
        app.work.running = true;
        app.work.panel.requested_identity =
            Some(WorkRunIdentity::OwnedEngine { model: path.into(), engine_id: None });
        app.work.panel.model = Some(path.into());
        let lines = app.work_provenance_lines(true);
        assert_eq!(
            lines.first().map(String::as_str),
            Some("Requested: Awili engine \u{b7} Gemma 4 26B (Studio)"),
            "{lines:?}"
        );
        assert!(
            lines.iter().all(|l| !l.contains("/Users/")),
            "no model path: {lines:?}"
        );
        // A blank label: the path's last part.
        app.work.engine.label = String::new();
        let lines = app.work_provenance_lines(true);
        assert_eq!(
            lines.first().map(String::as_str),
            Some("Requested: Awili engine \u{b7} gemma-4-26b-a4b-it-qat-mlx-4bit"),
            "{lines:?}"
        );
        assert!(lines.iter().all(|l| !l.contains("/Users/")), "{lines:?}");
    });
}

/// RC1-I3-FIX1 (Codex T2 P2 on afd9ac1): the in-app run's Progress line
/// ("Agent is thinking… (<model>)") names the model by its display label,
/// through the real event pump, never by the model server's file path.
#[gpui::test]
fn rc1i3_fix1_progress_names_the_model(cx: &mut gpui::TestAppContext) {
    if isolated("work::rc1i3_tests::rc1i3_fix1_progress_names_the_model").is_none() {
        return;
    }
    let path = "/Users/someone/models/gemma-4-26b-a4b-it-qat-mlx-4bit";
    let slot: std::rc::Rc<std::cell::RefCell<Option<WorkerContext>>> = Default::default();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(std::env::temp_dir());
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = path.into();
        app.work.engine.label = "Gemma 4 26B (Studio)".into();
        app.work.executor = WorkExecutor::InApp;
        app.views.insert(
            super::gateway_queue_tests::CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(super::gateway_queue_tests::CONV);
        let keep = slot.clone();
        TEST_WORKER_SPAWN.with(|s| {
            *s.borrow_mut() = Some(Box::new(move |ctx| {
                *keep.borrow_mut() = Some(ctx);
            }));
        });
        app.submit_work_run("Say hi.".into(), cx);
    });
    let ctx = slot
        .borrow_mut()
        .take()
        .expect("the in-app worker was spawned");
    ctx.events
        .send_blocking(WorkEvent::TurnStarted {
            model: Some(path.into()),
        })
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(
            app.work.panel.activity.as_deref(),
            Some("Agent is thinking\u{2026} (Gemma 4 26B (Studio))")
        );
    });
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
    drop(ctx);
}

/// The relabel touches only a Progress line that names the model path.
#[test]
fn rc1i3_fix1_name_model_by_label_only_rewrites_the_path() {
    let path = "/m/gemma";
    let mut up = WorkUpdate::Event(RunEvent::ToolProgress(
        awili_gateway::run_events::ToolProgressEvent {
            tool: None,
            status: Some("running".into()),
            message: Some(format!("Agent is thinking\u{2026} ({path})")),
        },
    ));
    name_model_by_label(&mut up, path, "Gemma");
    assert!(matches!(&up, WorkUpdate::Event(RunEvent::ToolProgress(p))
        if p.message.as_deref() == Some("Agent is thinking\u{2026} (Gemma)")));
    let mut other = WorkUpdate::Token("about /m/gemma".into());
    name_model_by_label(&mut other, path, "Gemma");
    assert!(matches!(&other, WorkUpdate::Token(t) if t == "about /m/gemma"));
    let mut plain = WorkUpdate::Event(RunEvent::ToolProgress(
        awili_gateway::run_events::ToolProgressEvent {
            tool: None,
            status: None,
            message: Some("Agent is thinking\u{2026} (m)".into()),
        },
    ));
    name_model_by_label(&mut plain, "m", "Gemma");
    assert!(
        matches!(&plain, WorkUpdate::Event(RunEvent::ToolProgress(p))
        if p.message.as_deref() == Some("Agent is thinking\u{2026} (m)"))
    );
}

/// RC2-R55 (usability RC2 round 4, Major 3: the reply showed "⚠ run_command
/// FAILED" for a step Awili's rules refused): a refused step reads as what
/// was not done and why, with no tool name and no "FAILED"; any other failed
/// step keeps its line.
#[test]
fn rc2r55_a_refused_step_reads_plainly_in_the_reply() {
    let tokens = |detail: &str| -> Vec<String> {
        work_event_to_updates(WorkEvent::ToolFinished {
            name: Some("write_file".into()),
            detail: Some(detail.into()),
            duration_s: Some(0.2),
            ok: Some(false),
        })
        .into_iter()
        .filter_map(|u| match u {
            WorkUpdate::Token(t) => Some(t),
            _ => None,
        })
        .collect()
    };
    let refused = tokens(
        "Not done: /tmp/x.txt is outside this conversation's folder, /w, and Awili changes files \
         only inside it.",
    );
    assert_eq!(refused.len(), 1);
    assert!(
        refused[0].contains("\u{26A0} Not done: /tmp/x.txt is outside"),
        "{refused:?}"
    );
    assert!(!refused[0].contains("write_file") && !refused[0].contains("FAILED"));
    let other = tokens("connection reset");
    assert!(other[0].contains("write_file FAILED"), "{other:?}");
}
