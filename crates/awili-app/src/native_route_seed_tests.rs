//! Native route and folder handoff regressions. Worker launch is intercepted
//! at its context boundary; no provider or external model is started.

use crate::{
    consolidation_tests::isolated,
    work::{Mode, WorkExecutor},
    workshop_composer::ModelOwner,
    AwiliApp,
};
use awili_code::CodeEngine;
use gpui::AppContext;

fn agent(engine: CodeEngine, model: &str) -> WorkExecutor {
    WorkExecutor::Agent {
        engine,
        model: Some(model.to_string()),
    }
}

/// A freshly created native helper must carry its explicit model into the
/// canonical route before the first submit asks dispatch for its target.
#[gpui::test]
fn new_native_model_is_the_first_input_route_target(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "code_history::native_route_seed_tests::new_native_model_is_the_first_input_route_target",
    ) {
        return;
    }
    let app = cx.new(AwiliApp::new);
    app.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Work;
        let target = agent(CodeEngine::Codex, "gpt-test");
        app.new_workshop_from_default(target.clone(), cx);
        let tab = app.work.code_tabs[app.work.code_active].id;
        assert_eq!(
            app.composer_target_for(&ModelOwner::Native(tab)),
            Some(target.clone()),
            "the blank tab exposes the explicit New target"
        );

        // `workshop_input_owner` is the first synchronous step of a native
        // send. It creates no provider and must seed the route it will submit.
        let owner = app
            .workshop_input_owner(false, cx)
            .expect("first native input gets a durable owner");
        assert_eq!(
            app.work.conversation_routes[&owner].next_target(),
            &target,
            "the first native send resolves its explicit target canonically"
        );
    });
}

/// A chooser selection belongs to the blank native tab until a file gives it
/// an id. Identity creation must transfer that exact selection, rather than
/// replacing it with the ambient legacy model fields.
#[gpui::test]
fn blank_native_chooser_choice_survives_staged_file_identity_creation(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "code_history::native_route_seed_tests::blank_native_chooser_choice_survives_staged_file_identity_creation",
    ) {
        return;
    }
    let app = cx.new(AwiliApp::new);
    app.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Code;
        app.ensure_code_tab(cx);
        let tab = app.work.code_tabs[app.work.code_active].id;
        let target = agent(CodeEngine::Codex, "gpt-test");
        assert!(app.set_composer_target(&ModelOwner::Native(tab), target.clone(), cx));

        // Attachment staging asks the real input-owner path to create the
        // native identity, but cannot launch a native harness.
        app.attach_bytes("prompt.txt", "text/plain", b"seed the owner", cx);
        let owner = app.work.code_tabs[app.work.code_active]
            .history
            .conv_id
            .clone()
            .expect("staging establishes a native identity");
        assert_eq!(app.staged_attachments.len(), 1);
        assert_eq!(
            app.work.conversation_routes[&owner].next_target(),
            &target,
            "the canonical route retains the chooser target after staging"
        );
        assert_eq!(
            app.composer_target_for(&ModelOwner::Native(tab)),
            Some(target),
            "the chooser still renders the selected target after identity creation"
        );
    });
}

/// A legacy Code metadata row is itself an explicit selection. If its executor
/// cannot be read, startup must keep it unavailable rather than allowing the
/// final ambient-default pass to create a replacement route.
#[gpui::test]
fn invalid_legacy_code_metadata_is_not_reseeded_at_startup(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::invalid_legacy_code_metadata_is_not_reseeded_at_startup")
    {
        return;
    }
    const ID: &str = "invalid-native-meta";
    const RAW: &str = r#"{"executor":"agent:nonesuch:model","folder":null,"session":null}"#;
    let store = awili_persist::Store::open_default().expect("private test store");
    store
        .create_conversation_with_kind(ID, "Invalid native", 1, "code")
        .expect("Code history row");
    store
        .set_app_state(&crate::code_history::code_conv_key(ID), RAW)
        .expect("legacy Code metadata");
    drop(store);

    let app = cx.new(AwiliApp::new);
    app.read_with(cx, |app, _| {
        assert!(
            !app.work.conversation_routes.contains_key(ID),
            "startup must not seed an ambient route over invalid metadata"
        );
        assert_eq!(
            app.work.unavailable_conversation_routes.get(ID),
            Some(&RAW.to_string()),
            "the raw legacy value remains an explicit unavailable choice"
        );
    });
}

fn folder_handoff_app(
    cx: &mut gpui::TestAppContext,
    selected: Option<&std::path::Path>,
    ambient: Option<&std::path::Path>,
) -> gpui::Entity<AwiliApp> {
    assert!(
        awili_config::Config::config_paths()[0]
            .starts_with(std::path::PathBuf::from(std::env::var("HOME").unwrap())),
        "config writes stay under isolated HOME"
    );
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let app = cx.new(AwiliApp::new);
    app.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Code;
        app.ensure_code_tab(cx);
        app.work.projects.clear();
        app.work.workshop_selected_folder = selected.map(|p| p.display().to_string());
        app.work.folder = ambient.map(std::path::Path::to_path_buf);
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "fixture".into();
        // Switching surfaces must not launch a real HTTP health probe from
        // the deterministic GPUI scheduler. The worker itself is intercepted.
        assert!(app.work.local.health.begin_probe());
        app.work
            .local
            .health
            .finish_probe(Ok(()), std::time::Instant::now());
        let id = app.work.code_tabs[app.work.code_active].id;
        app.work.code_tabs[app.work.code_active].project_folder = None;
        assert!(app.set_composer_target(&ModelOwner::Native(id), WorkExecutor::InApp, cx));
    });
    app
}

struct WorkerHookReset;
impl Drop for WorkerHookReset {
    fn drop(&mut self) {
        crate::work::TEST_WORKER_SPAWN.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
fn capture_worker_folder() -> (std::sync::mpsc::Receiver<String>, WorkerHookReset) {
    let (tx, rx) = std::sync::mpsc::channel();
    crate::work::TEST_WORKER_SPAWN.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move |ctx| {
            tx.send(ctx.workdir).unwrap();
        }));
    });
    (rx, WorkerHookReset)
}

#[gpui::test]
fn first_owned_turn_keeps_the_visible_native_project_folder(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::first_owned_turn_keeps_the_visible_native_project_folder") { return; }
    let folder = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, Some(folder.path()), None);
    let (rx, _hook) = capture_worker_folder();
    app.update(cx, |app, cx| {
        app.dispatch_composer_text("Read project-relative part.txt".into(), cx)
    });
    assert_eq!(
        rx.try_recv().unwrap(),
        folder.path().display().to_string(),
        "visible native project must reach the owned worker on its first turn"
    );
    app.read_with(cx, |app, _| {
        assert_eq!(app.work.folder.as_deref(), Some(folder.path()));
        let owner = app.work.conv_id.as_ref().unwrap();
        assert_eq!(
            app.conversations.get(owner).unwrap().cwd.as_deref(),
            Some(folder.path().to_str().unwrap())
        );
        assert_eq!(
            app.work.code_tabs[app.work.code_active]
                .project_folder
                .as_deref(),
            Some(folder.path().to_str().unwrap())
        );
        assert_eq!(
            app.work.code_tabs[app.work.code_active]
                .history
                .conv_id
                .as_ref(),
            Some(owner)
        );
    });
}

#[gpui::test]
fn owned_native_handoff_does_not_inherit_an_unrelated_work_folder(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::owned_native_handoff_does_not_inherit_an_unrelated_work_folder") { return; }
    let ambient = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, None, Some(ambient.path()));
    let (rx, _hook) = capture_worker_folder();
    app.update(cx, |app, cx| {
        // A stored Code identity with no folder has no authority to adopt
        // another conversation's ambient runtime folder. A fresh native tab
        // is different: ensure_code_project visibly adopts its default.
        let tab = app.work.code_tabs[app.work.code_active].id;
        let owner = app.ensure_code_conversation_identity(tab).unwrap();
        assert!(app.code_identity_folder(&owner).is_none());
        app.mode = Mode::Work;
        app.set_active_conversation(&owner);
        assert!(app.submit_work_run_kind(
            "An unscoped question".into(),
            owner,
            awili_core::MessageKind::Normal,
            cx,
        ));
    });
    assert_eq!(
        rx.try_recv().unwrap(),
        "/tmp",
        "an unbound native thread retains the owned engine's scoped default"
    );
    app.read_with(cx, |app, _| assert!(app.work.folder.is_none()));
}

#[gpui::test]
fn owned_native_handoff_retires_only_the_idle_adapter_without_refiling_its_sidebar(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("code_history::native_route_seed_tests::owned_native_handoff_retires_only_the_idle_adapter_without_refiling_its_sidebar") { return; }
    let folder = tempfile::tempdir().unwrap();
    let ambient = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, Some(folder.path()), Some(ambient.path()));
    let (tx, old_rx) = std::sync::mpsc::channel();
    let unrelated = app.update(cx, |app, _| {
        let id = app.sidebar.active_id().to_string();
        app.work.gateway_cmd_tx = Some(tx);
        app.work.session_id = Some("idle-other-session".into());
        app.work.worker_conv = Some(id.clone());
        (id.clone(), app.conversations.get(&id).unwrap().cwd.clone())
    });
    let (rx, _hook) = capture_worker_folder();
    app.update(cx, |app, cx| {
        app.dispatch_composer_text("Read part.txt".into(), cx)
    });
    assert_eq!(rx.try_recv().unwrap(), folder.path().display().to_string());
    assert!(matches!(
        old_rx.try_recv(),
        Ok(crate::work::GatewayCmd::Shutdown)
    ));
    assert!(old_rx.try_recv().is_err());
    app.read_with(cx, |app, _| {
        assert_eq!(
            app.conversations.get(&unrelated.0).unwrap().cwd,
            unrelated.1
        );
        assert_ne!(app.work.conv_id.as_deref(), Some(unrelated.0.as_str()));
    });
}

#[gpui::test]
fn missing_native_project_refuses_before_rebinding_or_launching(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::missing_native_project_refuses_before_rebinding_or_launching") { return; }
    let folder = tempfile::tempdir().unwrap();
    let missing = folder.path().join("removed-project");
    let ambient = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, Some(&missing), Some(ambient.path()));
    let (rx, _hook) = capture_worker_folder();
    app.update(cx, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("Read part.txt", cx));
        app.dispatch_composer_text("Read part.txt".into(), cx);
    });
    assert!(matches!(
        rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    app.read_with(cx, |app, cx| {
        assert_eq!(app.mode, Mode::Code);
        assert!(!app.work.running);
        assert_eq!(app.work.folder.as_deref(), Some(ambient.path()));
        assert_eq!(app.composer.read(cx).text(), "Read part.txt");
    });
}

#[gpui::test]
fn queued_native_handoff_preserves_the_stopping_folder_until_replay(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::queued_native_handoff_preserves_the_stopping_folder_until_replay") { return; }
    let folder = tempfile::tempdir().unwrap();
    let stopping = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, Some(folder.path()), Some(stopping.path()));
    let (tx, old_rx) = std::sync::mpsc::channel();
    app.update(cx, |app, _| {
        app.work.gateway_stop_pending = true;
        app.work.gateway_cmd_tx = Some(tx);
        app.work.session_id = Some("stopping-session".into());
    });
    let (rx, _hook) = capture_worker_folder();
    app.update(cx, |app, cx| {
        app.dispatch_composer_text("Read after the Stop".into(), cx)
    });
    assert!(matches!(
        rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    app.read_with(cx, |app, _| {
        assert_eq!(app.work.folder.as_deref(), Some(stopping.path()));
        assert_eq!(app.work.session_id.as_deref(), Some("stopping-session"));
        assert!(app.work.gateway_cmd_tx.is_some());
        assert!(app.work.queued_after_stop.is_some());
        assert!(app.work.folder_changed_during_stop);
    });
    assert!(matches!(
        old_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    app.update(cx, |app, cx| {
        // The existing gateway drain tests cover acknowledgement. This test
        // checks the captured conversation's folder on the normal replay send.
        app.work.gateway_stop_pending = false;
        app.work.gateway_cmd_tx = None;
        app.work.session_id = None;
        let queued = app.work.queued_after_stop.take().unwrap();
        assert!(app.submit_work_run_kind(
            queued.text,
            queued.conv_id,
            awili_core::MessageKind::Normal,
            cx
        ));
    });
    assert_eq!(rx.try_recv().unwrap(), folder.path().display().to_string());
}

#[gpui::test]
fn native_identity_creates_its_row_before_persisting_the_project_folder(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("code_history::native_route_seed_tests::native_identity_creates_its_row_before_persisting_the_project_folder") { return; }
    let folder = tempfile::tempdir().unwrap();
    let dbdir = tempfile::tempdir().unwrap();
    let path = dbdir.path().join("identity.db");
    let app = folder_handoff_app(cx, Some(folder.path()), None);
    let (persist, worker) =
        crate::persistence::Persistence::spawn_worker(awili_persist::Store::open(&path).unwrap());
    let (owner, barrier) = app.update(cx, |app, _| {
        app.persist = Some(persist);
        let tab = app.work.code_tabs[app.work.code_active].id;
        let owner = app.ensure_code_conversation_identity(tab).unwrap();
        let (tx, rx) = async_channel::bounded(1);
        app.persist
            .as_ref()
            .unwrap()
            .send(crate::PersistCmd::ReadState {
                key: "folder-barrier".into(),
                reply: tx,
            });
        (owner, rx)
    });
    barrier.recv_blocking().unwrap();
    let db = awili_persist::Store::open(&path).unwrap();
    let stored = db
        .conversations()
        .unwrap()
        .into_iter()
        .find(|c| c.id == owner)
        .unwrap();
    assert_eq!(
        stored.cwd.as_deref(),
        Some(folder.path().to_str().unwrap()),
        "Create-before-SetCwd must survive the real SQLite writer"
    );
    assert!(
        db.messages(&owner).unwrap().is_empty(),
        "identity creation is not a sacrificial native turn"
    );
    app.update(cx, |app, _| app.persist = None);
    worker.join().unwrap();
}

#[gpui::test]
fn work_surface_of_a_native_identity_restores_its_folder_and_keeps_folder_change_boundaries(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("code_history::native_route_seed_tests::work_surface_of_a_native_identity_restores_its_folder_and_keeps_folder_change_boundaries") { return; }
    let original = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, Some(original.path()), Some(other.path()));
    let owner = app.update(cx, |app, cx| {
        let tab = app.work.code_tabs[app.work.code_active].id;
        let id = app.ensure_code_conversation_identity(tab).unwrap();
        app.mode = Mode::Work;
        app.set_active_conversation(&id);
        assert!(app.restore_work_folder(cx));
        assert_eq!(app.work.folder.as_deref(), Some(original.path()));
        let view = app.views.get_mut(&id).unwrap();
        let n = view.convo.len();
        view.convo
            .push(crate::Role::User, "Original folder history");
        view.rows
            .push(crate::MessageRow::new(view.convo.message(n).unwrap()));
        app.choose_work_folder(other.path().into(), cx);
        assert_ne!(app.sidebar.active_id(), id);
        id
    });
    app.read_with(cx, |app, _| {
        assert_eq!(
            app.conversations.get(&owner).unwrap().cwd.as_deref(),
            Some(original.path().to_str().unwrap())
        );
        assert_eq!(app.work.folder.as_deref(), Some(other.path()));
    });
}

#[gpui::test]
fn historical_unknown_folder_is_not_rewritten_from_a_global_project(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::historical_unknown_folder_is_not_rewritten_from_a_global_project") { return; }
    let global = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, None, None);
    app.update(cx, |app, cx| {
        let tab = app.work.code_tabs[app.work.code_active].id;
        let id = app.ensure_code_conversation_identity(tab).unwrap();
        let view = app.views.get_mut(&id).unwrap();
        let n = view.convo.len();
        view.convo
            .push(crate::Role::User, "Historical unbound turn");
        view.rows
            .push(crate::MessageRow::new(view.convo.message(n).unwrap()));
        app.work.workshop_selected_folder = Some(global.path().display().to_string());
        app.ensure_code_conversation_identity(tab).unwrap();
        assert!(app.conversations.get(&id).unwrap().cwd.is_none());
        assert!(app.work.code_tabs[app.work.code_active]
            .project_folder
            .is_none());
        assert!(app.code_identity_folder(&id).is_none());
        // A new explicit picker action is tested separately; merely opening
        // or ensuring a historical identity carries no authority to refile it.
        app.composer
            .update(cx, |c, cx| c.set_text("retained draft", cx));
    });
}

#[gpui::test]
fn canonical_folder_wins_over_empty_or_stale_native_projection(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::canonical_folder_wins_over_empty_or_stale_native_projection") { return; }
    let folder = tempfile::tempdir().unwrap();
    let stale = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, Some(folder.path()), None);
    app.update(cx, |app, _| {
        let tab = app.work.code_tabs[app.work.code_active].id;
        let id = app.ensure_code_conversation_identity(tab).unwrap();
        for projection in [String::new(), stale.path().display().to_string()] {
            app.work.code_tabs[app.work.code_active].project_folder = Some(projection);
            app.ensure_code_conversation_identity(tab).unwrap();
            assert_eq!(
                app.conversations.get(&id).unwrap().cwd.as_deref(),
                Some(folder.path().to_str().unwrap())
            );
            assert_eq!(
                app.work.code_tabs[app.work.code_active]
                    .project_folder
                    .as_deref(),
                Some(folder.path().to_str().unwrap())
            );
            assert_eq!(
                app.code_identity_folder(&id).as_deref(),
                Some(folder.path())
            );
        }
    });
}

#[gpui::test]
fn explicit_pickers_on_an_unsent_code_identity_survive_ensure(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::explicit_pickers_on_an_unsent_code_identity_survive_ensure") { return; }
    let first = tempfile::tempdir().unwrap();
    let native_pick = tempfile::tempdir().unwrap();
    let work_pick = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, Some(first.path()), None);
    app.update(cx, |app, cx| {
        let tab = app.work.code_tabs[app.work.code_active].id;
        let id = app.ensure_code_conversation_identity(tab).unwrap();
        app.work.workshop_selected_folder = Some(native_pick.path().display().to_string());
        app.follow_project_in_code_tab();
        assert_eq!(
            app.ensure_code_conversation_identity(tab).as_deref(),
            Some(id.as_str())
        );
        assert_eq!(
            app.code_identity_folder(&id).as_deref(),
            Some(native_pick.path())
        );
        app.mode = Mode::Work;
        app.set_active_conversation(&id);
        app.choose_work_folder(work_pick.path().into(), cx);
        app.ensure_code_conversation_identity(tab).unwrap();
        assert_eq!(
            app.code_identity_folder(&id).as_deref(),
            Some(work_pick.path())
        );
        assert_eq!(
            app.work.code_tabs[app.work.code_active]
                .project_folder
                .as_deref(),
            Some(work_pick.path().to_str().unwrap())
        );
        assert_eq!(
            app.conversations.get(&id).unwrap().cwd.as_deref(),
            Some(work_pick.path().to_str().unwrap())
        );
    });
}

#[gpui::test]
fn unavailable_identity_keeps_folder_metadata_and_native_projection(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::unavailable_identity_keeps_folder_metadata_and_native_projection") { return; }
    let folder = tempfile::tempdir().unwrap();
    let raw_tab = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, Some(folder.path()), None);
    app.update(cx, |app, _| {
        let tab = app.work.code_tabs[app.work.code_active].id;
        let id = app.ensure_code_conversation_identity(tab).unwrap();
        app.work.code_tabs[app.work.code_active].project_folder =
            Some(raw_tab.path().display().to_string());
        app.work.conversation_routes.remove(&id);
        app.work
            .unavailable_conversation_routes
            .insert(id.clone(), "unavailable raw route".into());
        app.ensure_code_conversation_identity(tab).unwrap();
        assert_eq!(
            app.conversations.get(&id).unwrap().cwd.as_deref(),
            Some(folder.path().to_str().unwrap())
        );
        assert_eq!(
            app.work.code_tabs[app.work.code_active]
                .project_folder
                .as_deref(),
            Some(raw_tab.path().to_str().unwrap())
        );
        assert!(!app.work.conversation_routes.contains_key(&id));
        assert_eq!(
            app.work.unavailable_conversation_routes[&id],
            "unavailable raw route"
        );
    });
}

#[gpui::test]
fn unbound_code_restore_waits_for_another_running_work_owner(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::unbound_code_restore_waits_for_another_running_work_owner") { return; }
    let running = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, None, Some(running.path()));
    let (tx, old_rx) = std::sync::mpsc::channel();
    let owner = app.update(cx, |app, cx| {
        let tab = app.work.code_tabs[app.work.code_active].id;
        let id = app.ensure_code_conversation_identity(tab).unwrap();
        app.mode = Mode::Work;
        app.set_active_conversation(&id);
        app.work.running = true;
        app.work.conv_id = Some("other-running-owner".into());
        app.work.gateway_cmd_tx = Some(tx);
        app.work.session_id = Some("live-session".into());
        assert!(!app.restore_work_folder(cx));
        assert_eq!(app.work.folder.as_deref(), Some(running.path()));
        assert_eq!(app.work.session_id.as_deref(), Some("live-session"));
        id
    });
    assert!(matches!(
        old_rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    let (rx, _hook) = capture_worker_folder();
    app.update(cx, |app, cx| {
        // After the other turn has ended, its idle runtime can be rebound.
        app.work.running = false;
        app.work.gateway_cmd_tx = None;
        app.work.session_id = None;
        assert!(app.restore_work_folder(cx));
        assert!(app.work.folder.is_none());
        assert!(app.submit_work_run_kind(
            "Unbound follow-up".into(),
            owner,
            awili_core::MessageKind::Normal,
            cx
        ));
    });
    assert_eq!(rx.try_recv().unwrap(), "/tmp");
}

#[gpui::test]
fn adapter_handoff_preserves_the_persisted_global_folder_default(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::adapter_handoff_preserves_the_persisted_global_folder_default") { return; }
    let selected = tempfile::tempdir().unwrap();
    let ambient = tempfile::tempdir().unwrap();
    let dbdir = tempfile::tempdir().unwrap();
    let path = dbdir.path().join("global-default.db");
    let app = folder_handoff_app(cx, Some(selected.path()), Some(ambient.path()));
    let store = awili_persist::Store::open(&path).unwrap();
    store
        .set_app_state(
            crate::persistence::WORK_FOLDER_KEY,
            &ambient.path().display().to_string(),
        )
        .unwrap();
    let (persist, worker) = crate::persistence::Persistence::spawn_worker(store);
    let (rx, _hook) = capture_worker_folder();
    let barrier = app.update(cx, |app, cx| {
        app.persist = Some(persist);
        app.dispatch_composer_text("Read the selected project".into(), cx);
        // An idle restore of the same Code identity also binds only this
        // runtime. It must not replace the user's persisted global default.
        app.work.running = false;
        app.work.folder = Some(ambient.path().into());
        assert!(app.restore_work_folder(cx));
        assert_eq!(app.work.folder.as_deref(), Some(selected.path()));
        let (tx, rx) = async_channel::bounded(1);
        app.persist
            .as_ref()
            .unwrap()
            .send(crate::PersistCmd::ReadState {
                key: crate::persistence::WORK_FOLDER_KEY.into(),
                reply: tx,
            });
        rx
    });
    assert_eq!(
        rx.try_recv().unwrap(),
        selected.path().display().to_string()
    );
    assert_eq!(
        barrier.recv_blocking().unwrap().as_deref(),
        Some(ambient.path().to_str().unwrap()),
        "a handoff binds this conversation, not the global default"
    );
    app.update(cx, |app, _| app.persist = None);
    worker.join().unwrap();
}

#[gpui::test]
fn native_picker_preserves_history_recorded_only_in_the_shared_view(cx: &mut gpui::TestAppContext) {
    if !isolated("code_history::native_route_seed_tests::native_picker_preserves_history_recorded_only_in_the_shared_view") { return; }
    let original = tempfile::tempdir().unwrap();
    let picked = tempfile::tempdir().unwrap();
    let app = folder_handoff_app(cx, Some(original.path()), None);
    app.update(cx, |app, _| {
        let tab = app.work.code_tabs[app.work.code_active].id;
        let owner = app.ensure_code_conversation_identity(tab).unwrap();
        app.views
            .get_mut(&owner)
            .unwrap()
            .convo
            .push(crate::Role::User, "A turn recorded by the Work adapter");
        assert!(app.work.code_tabs[app.work.code_active].rows.is_empty());
        app.work.workshop_selected_folder = Some(picked.path().display().to_string());
        app.follow_project_in_code_tab();
        assert_eq!(
            app.conversations.get(&owner).unwrap().cwd.as_deref(),
            Some(original.path().to_str().unwrap())
        );
        assert_eq!(
            app.code_identity_folder(&owner).as_deref(),
            Some(original.path())
        );
        let active = &app.work.code_tabs[app.work.code_active];
        assert_ne!(
            active.id, tab,
            "history opens the chosen project in another tab"
        );
        assert_eq!(
            active.project_folder.as_deref(),
            Some(picked.path().to_str().unwrap())
        );
        assert!(active.history.conv_id.is_none());
    });
}
