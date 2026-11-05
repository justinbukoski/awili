//! N1 proof tests 29–37 plus the N1a-FIX1 review proofs: the real
//! `AwiliApp` (built by `AwiliApp::new` in a subprocess with a private HOME)
//! driven through the production hook paths — `apply_work_update`,
//! `cancel_work_run`, the agent-executor and local-build run ends,
//! `finish_code_turn`, `finish_turn`, backend signals and clicks. The backend
//! is always the recording one: nothing reaches Notification Center.

use std::time::{Duration, Instant};

use awili_code::CodeEngine;
use awili_core::{ConvKind, ConversationState, Role};
use awili_gateway::run_events::{ApprovalEvent, RunEvent, RunStatus};
use gpui::{AppContext, Context};

use crate::notifications::{route_id, Permission, Target};
use crate::notify_platform::{BackendSignal, Recorded};
use crate::work::{Mode, WorkExecutor, WorkUpdate};
use crate::work_run_identity::WorkRunIdentity;
use crate::AwiliApp;

const CHILD: &str = "AWILI_NOTIFY_TEST_HOME";
const CONV: &str = "n1-work-conv";

/// Run `test` in a subprocess with a private HOME. `Some(home)` inside the
/// child (run the body); `None` in the parent once the child passed.
fn isolated(test: &str) -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os(CHILD) {
        return Some(home.into());
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
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
    None
}

/// A configured, non-gateway app whose Work output goes to `CONV`, with the
/// owner away for 30 s and native notifications allowed.
fn away_authorized(app: &mut AwiliApp, cx: &mut Context<AwiliApp>) {
    setup(app);
    app.on_notify_signal(BackendSignal::Permission(Permission::Authorized), cx);
    assert!(app.notifier.test_log().is_empty(), "nothing was pending");
}

/// Like [`away_authorized`] but permission still `Unknown`.
fn setup(app: &mut AwiliApp) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.work.executor = WorkExecutor::Agent {
        engine: CodeEngine::Dsh,
        model: None,
    };
    app.work.folder = None;
    app.views.insert(
        CONV.into(),
        crate::conv_view::ConvView::from_state(ConversationState::new()),
    );
    app.work.conv_id = Some(CONV.into());
    app.notifier
        .observe_window(false, Instant::now() - Duration::from_secs(30));
}

/// Every native post as `(id, title, body)`.
fn natives(app: &AwiliApp) -> Vec<(String, String, String)> {
    app.notifier
        .test_log()
        .into_iter()
        .filter_map(|r| match r {
            Recorded::Native(n) => Some((n.id, n.title, n.body)),
            _ => None,
        })
        .collect()
}

fn titles(app: &AwiliApp) -> Vec<String> {
    natives(app).into_iter().map(|(_, t, _)| t).collect()
}

/// What an accepted submit leaves behind: the user's row and the empty
/// assistant row the reply streams into. A turn's end is latched on that row
/// (`stream_row_idx.take()`), so a synthetic terminal update only completes a
/// turn that really began; every fixture arms one before it ends a run.
fn arm_turn(app: &mut AwiliApp) {
    app.work_push(Role::User, "do the work".into());
    app.work_push(Role::Assistant, String::new());
    let last = app.views[&app.work_conv_id()].rows.len() - 1;
    app.work.stream_row_idx = Some(last);
}

fn work_route() -> String {
    route_id(&Target::Work(CONV.into()))
}

fn approval(id: &str, kind: &str, prompt: &str) -> WorkUpdate {
    WorkUpdate::Event(RunEvent::Approval(ApprovalEvent {
        approval_id: Some(id.into()),
        kind: Some(kind.into()),
        prompt: Some(prompt.into()),
        risk: awili_gateway::events::ApprovalRisk::Privileged,
    }))
}

/// 29: an approval raised by `apply_work_update` posts exactly once, to the
/// Work conversation, without the prompt text; the same event again adds
/// nothing.
#[gpui::test]
fn work_approval_update_posts_once_without_prompt_text(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::work_approval_update_posts_once_without_prompt_text")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        let ev = || approval("ap-1", "dangerous_command", "rm -rf ~/x TOKEN=abc");
        app.apply_work_update(ev(), cx);
        let posted = natives(app);
        assert_eq!(posted.len(), 1, "{posted:?}");
        let (id, title, body) = &posted[0];
        assert_eq!(id, &work_route());
        assert_eq!(title, "Approval needed");
        assert!(
            !body.contains("rm -rf") && !body.contains("TOKEN"),
            "{body}"
        );
        app.apply_work_update(ev(), cx);
        assert_eq!(natives(app).len(), 1, "the same card posts once");
        let clarify = approval("ap-2", "clarify", "What is your API key?");
        app.apply_work_update(clarify, cx);
        assert_eq!(titles(app), ["Approval needed", "Question waiting"]);
    });
}

/// 30: sudo and secret prompts post while away, never naming the env var.
#[gpui::test]
fn work_credential_prompt_posts_when_away(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::work_credential_prompt_posts_when_away").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.apply_work_update(
            WorkUpdate::SudoRequest {
                request_id: "sr-1".into(),
            },
            cx,
        );
        app.apply_work_update(
            WorkUpdate::SecretRequest {
                request_id: "sr-2".into(),
                env_var: "GITEA_TOKEN".into(),
                prompt: "paste the GITEA_TOKEN".into(),
            },
            cx,
        );
        assert_eq!(titles(app), ["Password needed", "Secret needed"]);
        assert!(natives(app)
            .iter()
            .all(|(_, _, body)| !body.contains("GITEA_TOKEN")));
    });
}

/// 31: a gateway turn that ends with `TurnComplete` then `Terminal` posts one
/// `Work finished`.
#[gpui::test]
fn work_end_posts_once_across_terminal_and_turn_complete(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::work_end_posts_once_across_terminal_and_turn_complete")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(20));
        arm_turn(app);
        app.apply_work_update(WorkUpdate::TurnComplete, cx);
        app.apply_work_update(
            WorkUpdate::Terminal {
                status: RunStatus::Completed,
                output: None,
                model: None,
            },
            cx,
        );
        assert_eq!(titles(app), ["Work finished"]);
        // The next turn on the same session (same run_seq) ends too.
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(15));
        arm_turn(app);
        app.apply_work_update(WorkUpdate::TurnComplete, cx);
        assert_eq!(titles(app), ["Work finished", "Work finished"]);
    });
}

/// 32: the owner's Stop is silent — an agent run's Stop ends it `Stopped`
/// through the shared finish, and nothing posts.
#[gpui::test]
fn cancel_work_run_is_silent(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::cancel_work_run_is_silent").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(20));
        arm_turn(app);
        let (tx, _rx) = std::sync::mpsc::channel();
        app.work.agent_abort_tx = Some(tx);
        app.cancel_work_run(cx);
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
        assert!(natives(app).is_empty(), "{:?}", natives(app));
    });
}

/// 37: the first eligible event asks macOS once and parks; a second one
/// while the prompt is up asks nothing (same conversation: it replaces the
/// parked one); `Authorized` posts it.
#[gpui::test]
fn first_eligible_event_requests_permission_once(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::first_eligible_event_requests_permission_once").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        setup(app);
        assert_eq!(
            app.notifier.status_line(),
            "macOS will ask the first time something needs you"
        );
        assert!(app.notifier.test_log().is_empty(), "nothing at launch");
        app.apply_work_update(approval("ap-1", "deletion", "x"), cx);
        assert_eq!(app.notifier.test_log(), [Recorded::RequestPermission]);
        app.apply_work_update(approval("ap-2", "clarify", "which branch?"), cx);
        assert_eq!(
            app.notifier.test_log(),
            [Recorded::RequestPermission],
            "no second request while the prompt is up"
        );
        app.on_notify_signal(BackendSignal::Permission(Permission::Authorized), cx);
        let log = app.notifier.test_log();
        assert_eq!(log.len(), 2, "{log:?}");
        assert_eq!(log[0], Recorded::RequestPermission);
        // Both cards are on one conversation: the newer replaced the parked one.
        assert_eq!(titles(app), ["Question waiting"]);
    });
}

/// Review P1-3: the owner's denial drops what was parked, never falls back to
/// Script Editor, and Settings offers System Settings; "send test" asks again.
#[gpui::test]
fn denied_permission_drops_parked_and_never_falls_back(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::denied_permission_drops_parked_and_never_falls_back")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        setup(app);
        app.apply_work_update(approval("ap-1", "deletion", "x"), cx);
        app.on_notify_signal(BackendSignal::Permission(Permission::Denied), cx);
        app.apply_work_update(approval("ap-2", "deletion", "x"), cx);
        assert_eq!(app.notifier.test_log(), [Recorded::RequestPermission]);
        assert_eq!(
            app.notifier.status_line(),
            "off for Awili in System Settings"
        );
        app.notify_open_system_settings();
        app.notify_send_test();
        assert_eq!(
            app.notifier.test_log(),
            [
                Recorded::RequestPermission,
                Recorded::OpenSettings,
                Recorded::RequestPermission
            ]
        );
    });
}

/// Review P1-1: an agent-executor run (claude/codex/dsh) ends through its own
/// task, not the gateway arms, and still posts exactly one event per run —
/// finished, then failed without the error text.
#[gpui::test]
fn agent_executor_run_end_posts_once(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::agent_executor_run_end_posts_once").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        let folder = tempfile::tempdir().unwrap();
        let project = folder.path().join("lisbon-app");
        std::fs::create_dir_all(&project).unwrap();
        app.work.folder = Some(project.clone());
        // The state `run_work_via_agent` leaves while its task runs.
        let begin = |app: &mut AwiliApp, secs: u64| {
            app.work.run_seq += 1;
            app.work.running = true;
            app.work.run_started = Some(Instant::now() - Duration::from_secs(secs));
            app.work.panel = Default::default();
            app.work.panel.requested_identity = Some(WorkRunIdentity::Agent {
                engine: CodeEngine::Dsh,
                model: None,
            });
            let (tx, _rx) = std::sync::mpsc::channel();
            app.work.agent_abort_tx = Some(tx);
            arm_turn(app);
            app.work.run_seq
        };
        let run = begin(app, 20);
        app.work_agent_run_done(run, &project, cx);
        app.work_agent_run_done(run, &project, cx);
        assert_eq!(
            natives(app),
            [(
                work_route(),
                "Work finished".to_string(),
                // RC2-R69: with the helper that did the work.
                "lisbon-app \u{b7} DeepSeek".to_string()
            )]
        );
        let run = begin(app, 1);
        app.work.panel.error = Some("401 key sk-live-123".into());
        app.work_agent_run_done(run, &project, cx);
        assert_eq!(titles(app), ["Work finished", "Work failed"]);
        assert!(natives(app).iter().all(|(_, _, b)| !b.contains("sk-live")));
    });
}

/// Review P1-1: a local build (the real submit-and-drive task against the
/// fake `ssh` runner) that finishes posts `Work finished` once.
#[gpui::test]
fn local_build_run_end_posts_once(cx: &mut gpui::TestAppContext) {
    use crate::build_dispatch::{build_request, ResolvedBuild};
    use crate::build_dispatch_tests::{fake_ssh, input, snapshot, targets, SCRIPT_FIXTURE};
    use crate::build_staleness::LocalBuildHandle;
    use awili_dispatch::protocol::BuildState;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let Some(home) = isolated("notify_hooks::tests::local_build_run_end_posts_once") else {
        return;
    };
    let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
    let runner = home.join("runner");
    std::fs::create_dir_all(&runner).unwrap();
    let resolved = ResolvedBuild::resolve_with(&targets(), "hosta", fake_ssh(&runner, 0)).unwrap();
    let request = build_request(&resolved, &input("turn-1", "task".into())).unwrap();
    let mut done = snapshot(&request, BuildState::AwaitingReview);
    done.revision = 4;
    done.result_sha = Some("b".repeat(40));
    std::fs::write(
        runner.join("snapshot.json"),
        serde_json::to_vec(&done).unwrap(),
    )
    .unwrap();

    let entity = cx.new(AwiliApp::new);
    let (persist, rx) = crate::build_dispatch_p10_tests::recording_persist();
    entity.update(cx, |app, cx| {
        app.persist = Some(persist);
        away_authorized(app, cx);
        // The state `run_work_via_local_build` leaves before its drive.
        app.work.run_seq += 1;
        let run_id = app.work.run_seq;
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(20));
        arm_turn(app);
        app.work.panel.requested_identity = Some(WorkRunIdentity::LocalBuild {
            model: "qwen3".into(),
            host: "configured-host".into(),
        });
        let stop = Arc::new(AtomicBool::new(false));
        app.work.cancel = stop.clone();
        let key = LocalBuildHandle {
            turn_id: "turn-1".into(),
            request_id: request.request_id.clone(),
            target_id: "hosta".into(),
            revision: 0,
            stop,
        };
        app.work.local_build = Some(key.clone());
        app.drive_local_build(run_id, resolved, request, key, cx);
    });
    cx.run_until_parked();
    let reply = rx
        .try_iter()
        .find_map(|cmd| match cmd {
            crate::PersistCmd::CommitBuildJob { reply, .. } => Some(reply),
            _ => None,
        })
        .expect("the build requested a durable tracking row");
    assert!(
        !runner.join("calls").exists(),
        "the runner waits for acknowledgement"
    );
    reply.send_blocking(Ok(())).unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, _| {
        assert!(!app.work.running, "the drive finished the run");
        assert_eq!(app.work.panel.status, Some(RunStatus::Completed));
        assert_eq!(titles(app), ["Work finished"]);
    });
}

/// Review P1-2: a failure posts however short the run was; a finish needs
/// the run to be long enough.
#[gpui::test]
fn quick_failure_posts_but_quick_finish_does_not(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::quick_failure_posts_but_quick_finish_does_not").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(1));
        arm_turn(app);
        app.apply_work_update(WorkUpdate::TurnComplete, cx);
        assert!(natives(app).is_empty(), "a 1 s finish is too quick");
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(1));
        arm_turn(app);
        app.apply_work_update(WorkUpdate::Failed("boom: sk-live-123".into()), cx);
        assert_eq!(titles(app), ["Work failed"]);
        assert!(natives(app).iter().all(|(_, _, b)| !b.contains("sk-live")));
    });
}

/// Review P1-4: a saved `enabled = false` holds from launch — the app built
/// from that config posts nothing and asks nothing.
#[gpui::test]
fn saved_notifications_off_survives_restart(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("notify_hooks::tests::saved_notifications_off_survives_restart")
    else {
        return;
    };
    let dir = home.join(".config").join("awili");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[notifications]\nenabled = false\n",
    )
    .unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        assert!(!app.notifier.core.config().enabled, "loaded from the file");
        setup(app);
        app.apply_work_update(approval("ap-1", "deletion", "x"), cx);
        app.apply_work_update(WorkUpdate::Failed("boom".into()), cx);
        assert!(app.notifier.test_log().is_empty());
    });
}

/// A native post the adapter reports failed goes out through the fallback,
/// and native stays off for the rest of the launch.
#[gpui::test]
fn native_post_failure_falls_back_to_script_editor(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::native_post_failure_falls_back_to_script_editor").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.apply_work_update(approval("ap-1", "deletion", "x"), cx);
        let first = app.notifier.test_log();
        let [Recorded::Native(n)] = first.as_slice() else {
            panic!("one native post: {first:?}");
        };
        let n = n.clone();
        app.on_notify_signal(BackendSignal::PostFailed(n.clone()), cx);
        app.apply_work_update(approval("ap-2", "deletion", "x"), cx);
        let log = app.notifier.test_log();
        assert_eq!(log[1], Recorded::Fallback(n));
        assert!(matches!(&log[2], Recorded::Fallback(m) if m.title == "Approval needed"));
        assert_eq!(log.len(), 3);
        assert_eq!(
            app.notifier.status_line(),
            "native notifications unavailable \u{2014} using Script Editor fallback"
        );
    });
}

/// 33: a Code turn that ends by itself posts once; one the owner cancelled
/// does not.
#[gpui::test]
fn code_turn_end_posts_and_user_cancel_does_not(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::code_turn_end_posts_and_user_cancel_does_not").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.ensure_code_tab(cx);
        let tab_id = app.work.code_tabs[app.work.code_active].id;
        let start_turn = |app: &mut AwiliApp| {
            let s = &mut app.work.code_tabs[app.work.code_active];
            let idx = s.rows.len();
            s.convo.push(Role::Assistant, "done");
            s.rows
                .push(crate::MessageRow::new(s.convo.message(idx).unwrap()));
            s.streaming = true;
            let (tx, _rx) = std::sync::mpsc::channel();
            s.stream_abort_tx = Some(tx);
        };
        start_turn(app);
        app.finish_code_turn(tab_id, cx);
        let posted = natives(app);
        assert_eq!(posted.len(), 1, "{posted:?}");
        assert_eq!(posted[0].0, route_id(&Target::Code(tab_id)));
        assert_eq!(posted[0].1, "Code turn finished");
        start_turn(app);
        app.cancel_code_turn(cx);
        app.finish_code_turn(tab_id, cx);
        assert_eq!(natives(app).len(), 1, "a cancelled turn is silent");
    });
}

/// 35: a click on a Work notification opens that Work conversation — not the
/// newest one, which is what switching to Work alone would show.
#[gpui::test]
fn click_routes_to_work_conversation(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::click_routes_to_work_conversation").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.set_mode(Mode::Work, cx);
        let first = app.sidebar.active_id().to_string();
        assert_eq!(
            app.conversations.get(&first).map(|c| c.kind),
            Some(ConvKind::Work)
        );
        // A newer Work conversation becomes the remembered Work thread (what
        // `new_conversation_kind` does, minus focusing the window).
        let newer = app.conversations.create_kind(1, ConvKind::Work).id;
        app.views
            .insert(newer.clone(), crate::conv_view::ConvView::new());
        app.set_active_conversation(&newer);
        app.remember_active_kind();
        app.set_mode(Mode::Code, cx);
        assert_ne!(app.sidebar.active_id(), first);
        let route = route_id(&Target::Work(first.clone()));
        app.on_notify_signal(BackendSignal::Clicked(route), cx);
        assert_eq!(app.mode, Mode::Work);
        assert_eq!(app.sidebar.active_id(), first, "the clicked conversation");
    });
}

/// 36: a click on a conversation that is gone switches mode only; garbage
/// changes nothing; neither answers the open approval nor starts a run.
#[gpui::test]
fn click_with_missing_target_or_garbage_only_navigates_safely(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::click_with_missing_target_or_garbage_only_navigates_safely")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.apply_work_update(approval("ap-9", "deletion", "delete it"), cx);
        let chat = app.sidebar.active_id().to_string();
        assert_eq!(app.mode, Mode::Work);
        for garbage in ["awili:v1:work:../x", "awili:v2:work:a", "", "hello"] {
            app.on_notify_signal(BackendSignal::Clicked(garbage.into()), cx);
            assert_eq!(
                (app.mode, app.sidebar.active_id().to_string()),
                (Mode::Work, chat.clone())
            );
        }
        let gone = route_id(&Target::Work("no-such-conversation".into()));
        app.on_notify_signal(BackendSignal::Clicked(gone), cx);
        assert_eq!(app.mode, Mode::Work);
        assert_ne!(app.sidebar.active_id(), "no-such-conversation");
        let card = app.work.panel.approval.as_ref().expect("card still open");
        assert_eq!(card.approval_id, "ap-9");
        assert_eq!(card.delivery, Default::default(), "no verdict was sent");
        assert!(!app.work.running, "no run started");
    });
}

/// FIX1f (review of b5a7d6a, P1): the owner turns notifications off in
/// Settings (the live re-apply) while the macOS prompt is up; the Allow that
/// follows posts nothing that was parked.
#[gpui::test]
fn settings_off_during_prompt_drops_parked_event(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::settings_off_during_prompt_drops_parked_event").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        setup(app);
        app.apply_work_update(approval("ap-1", "deletion", "x"), cx);
        assert_eq!(app.notifier.test_log(), [Recorded::RequestPermission]);
        let mut cfg = awili_config::Config::networkless();
        cfg.notifications.enabled = false;
        app.apply_runtime_config(&cfg, cx);
        app.on_notify_signal(BackendSignal::Permission(Permission::Authorized), cx);
        assert_eq!(
            app.notifier.test_log(),
            [Recorded::RequestPermission],
            "the parked approval is dropped, not posted"
        );
    });
}

/// FIX1f (review of b5a7d6a, P1): a native post the adapter reports failed
/// after the owner denied Awili is dropped — never a Script Editor fallback.
#[gpui::test]
fn late_post_failure_after_denial_never_falls_back(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::late_post_failure_after_denial_never_falls_back").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.apply_work_update(approval("ap-1", "deletion", "x"), cx);
        let first = app.notifier.test_log();
        let [Recorded::Native(n)] = first.as_slice() else {
            panic!("one native post: {first:?}");
        };
        let n = n.clone();
        app.on_notify_signal(BackendSignal::Permission(Permission::Denied), cx);
        app.on_notify_signal(BackendSignal::PostFailed(n.clone()), cx);
        assert_eq!(app.notifier.test_log(), [Recorded::Native(n)]);
        assert_eq!(
            app.notifier.status_line(),
            "off for Awili in System Settings"
        );
    });
}

/// N1a-FIX1h (review of e5c7c98, P2): stopping a local build is silent and
/// leaves nothing behind — a NEW run on the same conversation that fails a
/// moment later still notifies.
#[gpui::test]
fn local_build_stop_does_not_silence_the_next_failure(cx: &mut gpui::TestAppContext) {
    use crate::build_staleness::LocalBuildHandle;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    if isolated("notify_hooks::tests::local_build_stop_does_not_silence_the_next_failure").is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        // A live, panel-driving local build (the state its drive leaves).
        app.work.run_seq += 1;
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(20));
        arm_turn(app);
        app.work.panel.requested_identity = Some(WorkRunIdentity::LocalBuild {
            model: "qwen3".into(),
            host: "configured-host".into(),
        });
        let stop = Arc::new(AtomicBool::new(false));
        app.work.cancel = stop.clone();
        app.work.local_build = Some(LocalBuildHandle {
            turn_id: "turn-1".into(),
            request_id: "req-1".into(),
            target_id: "hosta".into(),
            revision: 3,
            stop,
        });
        app.cancel_work_run(cx);
        assert!(app.work.local_build.is_none(), "the Stop closed the build");
        assert!(natives(app).is_empty(), "the owner's Stop is silent");
        // The next run on the same conversation fails at once.
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(1));
        arm_turn(app);
        app.apply_work_update(WorkUpdate::Failed("boom".into()), cx);
        assert_eq!(titles(app), ["Work failed"]);
    });
}

/// N1a-FIX1i (slice review, P1): a native post queued before the owner turns
/// notifications off in Settings, and reported failed after, produces no
/// Script Editor notification.
#[gpui::test]
fn post_failure_after_settings_off_never_falls_back(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::post_failure_after_settings_off_never_falls_back").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.apply_work_update(approval("ap-1", "deletion", "x"), cx);
        let first = app.notifier.test_log();
        let [Recorded::Native(n)] = first.as_slice() else {
            panic!("one native post: {first:?}");
        };
        let n = n.clone();
        let mut cfg = awili_config::Config::networkless();
        cfg.notifications.enabled = false;
        app.apply_runtime_config(&cfg, cx);
        app.on_notify_signal(BackendSignal::PostFailed(n.clone()), cx);
        assert_eq!(app.notifier.test_log(), [Recorded::Native(n)]);
    });
}

/// N1a-FIX1j (review of cd77d4e, P1): an approval and a failure on one Work
/// conversation share a notification id; when `approvals` is switched off in
/// Settings and both native posts are then reported failed (the later one
/// first), only the failure falls back — each report is matched to its own
/// post, not to the id they share.
#[gpui::test]
fn same_conversation_failures_follow_their_own_kind(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::same_conversation_failures_follow_their_own_kind").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.apply_work_update(approval("ap-1", "deletion", "x"), cx);
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(1));
        arm_turn(app);
        app.apply_work_update(WorkUpdate::Failed("boom".into()), cx);
        let log = app.notifier.test_log();
        let [Recorded::Native(approval_note), Recorded::Native(failed_note)] = log.as_slice()
        else {
            panic!("two native posts: {log:?}");
        };
        assert_eq!(approval_note.id, failed_note.id);
        let (approval_note, failed_note) = (approval_note.clone(), failed_note.clone());
        let mut cfg = awili_config::Config::networkless();
        cfg.notifications.approvals = false;
        app.apply_runtime_config(&cfg, cx);
        // The later post's failure is reported first, then the earlier one's.
        app.on_notify_signal(BackendSignal::PostFailed(failed_note.clone()), cx);
        app.on_notify_signal(BackendSignal::PostFailed(approval_note), cx);
        let log = app.notifier.test_log();
        assert_eq!(log.len(), 3, "{log:?}");
        assert_eq!(log[2], Recorded::Fallback(failed_note));
    });
}

/// N1a-FIX1k (slice re-review, P1): the owner revokes permission after an
/// earlier Allow; the next native post fails with UserNotifications' "not
/// allowed". That reads as Denied: nothing posts, no Script Editor fallback,
/// and later events stay refused.
#[gpui::test]
fn revoked_permission_after_authorization_is_denied(cx: &mut gpui::TestAppContext) {
    use crate::notify_platform::{post_error_signal, UN_ERROR_DOMAIN, UN_NOT_ALLOWED};
    if isolated("notify_hooks::tests::revoked_permission_after_authorization_is_denied").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        app.apply_work_update(approval("ap-1", "deletion", "x"), cx);
        let first = app.notifier.test_log();
        let [Recorded::Native(n)] = first.as_slice() else {
            panic!("one native post: {first:?}");
        };
        let n = n.clone();
        let signal = post_error_signal(n.clone(), UN_ERROR_DOMAIN, UN_NOT_ALLOWED);
        app.on_notify_signal(signal, cx);
        assert_eq!(app.notifier.core.permission(), &Permission::Denied);
        assert_eq!(
            app.notifier.status_line(),
            "off for Awili in System Settings"
        );
        app.apply_work_update(approval("ap-2", "deletion", "x"), cx);
        assert_eq!(app.notifier.test_log(), [Recorded::Native(n)]);
    });
}

/// RC2-R64 (usability RC2 round 5, Minor 7): Settings › Notifications said
/// "macOS will ask the first time something needs you" until a test was
/// sent, though macOS already allowed Awili. Awili asks macOS for the real
/// permission at launch and when Settings and its Notifications page open
/// (asking, never prompting), and the page says what macOS answered.
#[gpui::test]
fn rc2r64_settings_reports_what_macos_allows(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::rc2r64_settings_reports_what_macos_allows").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        setup(app);
        assert_eq!(app.notifier.queries, 1, "asked at launch");
        assert!(app.notifier.test_log().is_empty(), "never prompted");
        app.on_notify_signal(BackendSignal::Current(Permission::Authorized), cx);
        assert_eq!(app.notifier.status_line(), "allowed");
        app.select_settings_page(crate::settings_catalog::Page::Notifications, cx);
        assert_eq!(app.notifier.queries, 2, "asked when the page opens");
        app.on_notify_signal(BackendSignal::Current(Permission::Denied), cx);
        assert_eq!(
            app.notifier.status_line(),
            "off for Awili in System Settings"
        );
        assert!(
            app.notifier.test_log().is_empty(),
            "nothing posted or asked"
        );
    });
}

/// RC2-R69 (usability RC2 round 5, Minor 6: "Work finished" said only "x1",
/// "Approval needed" only "w1 · command"): a notification says what was
/// asked (the conversation's title), which helper and where — in Awili's
/// own labels only, never the agent's words.
#[gpui::test]
fn rc2r69_notifications_say_what_who_and_where(cx: &mut gpui::TestAppContext) {
    if isolated("notify_hooks::tests::rc2r69_notifications_say_what_who_and_where").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        away_authorized(app, cx);
        let folder = tempfile::tempdir().unwrap();
        let x1 = folder.path().join("x1");
        std::fs::create_dir_all(&x1).unwrap();
        app.work.folder = Some(x1.clone());
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Work);
        app.conversations
            .rename(&meta.id, "write a 200-word story about a lighthouse");
        app.views.insert(
            meta.id.clone(),
            crate::conv_view::ConvView::from_state(ConversationState::new()),
        );
        app.work.conv_id = Some(meta.id.clone());
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Codex,
            model: None,
        };
        app.work.run_seq += 1;
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(20));
        arm_turn(app);
        let (tx, _rx) = std::sync::mpsc::channel();
        app.work.agent_abort_tx = Some(tx);
        let run = app.work.run_seq;
        app.work_agent_run_done(run, &x1, cx);
        let posted = natives(app);
        let (_, title, body) = posted.last().expect("a post").clone();
        assert_eq!(title, "Work finished");
        assert_eq!(
            body,
            "write a 200-word story about a lighthouse \u{b7} Codex in x1"
        );

        // An approval from the in-app model: who wants to do what, where.
        app.work.executor = WorkExecutor::InApp;
        app.work.engine.label = "Gemma 4 26B (Studio)".into();
        app.work.running = true;
        app.apply_work_update(
            WorkUpdate::Event(RunEvent::Approval(ApprovalEvent {
                approval_id: Some("ap-r69".into()),
                kind: Some("command".into()),
                prompt: Some("Write the file notes.txt (2 characters) TOKEN=abc".into()),
                risk: awili_work::run_events::ApprovalRisk::Mutating,
            })),
            cx,
        );
        let (_, title, body) = natives(app).last().expect("an approval post").clone();
        assert_eq!(title, "Approval needed");
        assert_eq!(
            body,
            "write a 200-word story about a lighthouse \u{b7} Gemma 4 26B (Studio) wants to \
             make a change in x1"
        );
        assert!(
            !body.contains("TOKEN") && !body.contains("notes.txt"),
            "{body}"
        );
    });
}
