//! Exercise the shared catalog against actual app selection and worker replies.
use crate::{consolidation_tests::isolated, work::Mode, AwiliApp};
use awili_persist::StoredProject;
use gpui::AppContext;

#[gpui::test]
fn workshop_tabs_restore_uuid_order_and_recorded_session_identity(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_tabs_restore_uuid_order_and_recorded_session_identity") {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    let chat = "saved-chat";
    let code = "saved-code";
    store
        .create_conversation_with_kind(chat, "Question", 1, "chat")
        .unwrap();
    store
        .create_conversation_with_kind(code, "Keep transcript", 2, "code")
        .unwrap();
    store
        .append_message(code, "user", "original prompt", 2)
        .unwrap();
    let meta = crate::code_history::CodeConvMeta::for_turn(
        awili_code::CodeEngine::Claude,
        None,
        Some("/original".into()),
        Some("resume-saved".into()),
    );
    store
        .set_app_state(&crate::code_history::code_conv_key(code), &meta.to_json())
        .unwrap();
    let chat_id = uuid::Uuid::new_v4().to_string();
    let code_id = uuid::Uuid::new_v4().to_string();
    let raw = serde_json::json!({"version":1,"tabs":[
        {"id":chat_id,"backing":{"Conversation":chat}},
        {"id":code_id,"backing":{"Agent":{"conv":code,"executor":"agent:codex","folder":"/requested"}}}
    ],"active":code_id}).to_string();
    store
        .set_app_state(crate::workshop::TABS_KEY, &raw)
        .unwrap();
    store
        .set_app_state(crate::persistence::LAST_MODE_KEY, "code")
        .unwrap();
    store
        .set_app_state(
            crate::code_reopen::CODE_OPEN_TABS_KEY,
            r#"{"tabs":[{"conv":null,"executor":"agent:claude","folder":"/legacy"}],"active":0}"#,
        )
        .unwrap();
    drop(store);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        assert_eq!(app.shell.tabs.tabs[0].id, chat_id);
        assert_eq!(app.shell.tabs.tabs[1].id, code_id);
        assert_eq!(app.shell.tabs.active.as_deref(), Some(code_id.as_str()));
        assert_eq!(app.mode, Mode::Code, "canonical active tab wins at startup");
        assert_eq!(
            app.shell.tabs.tabs.len(),
            3,
            "mismatched requested tuple opens one fresh thread"
        );
        assert_eq!(app.work.code_tabs.len(), 2, "no legacy resurrection");
        app.select_workshop_tab(&code_id, cx);
        let original = &app.work.code_tabs[app.work.code_active];
        assert_eq!(original.engine, awili_code::CodeEngine::Claude);
        assert_eq!(original.project_folder.as_deref(), Some("/original"));
        assert_eq!(original.convo.message(0).unwrap().text, "original prompt");
        assert_eq!(
            original.session_slot.lock().unwrap().as_deref(),
            Some("resume-saved")
        );
        let fresh = app
            .work
            .code_tabs
            .iter()
            .find(|s| s.engine == awili_code::CodeEngine::Codex)
            .unwrap();
        assert_eq!(fresh.project_folder.as_deref(), Some("/requested"));
        assert!(fresh.convo.messages().is_empty());
        assert!(fresh.session_slot.lock().unwrap().is_none());
    });
}

fn project(id: i64, folder: &str) -> StoredProject {
    StoredProject {
        id,
        name: folder.into(),
        folder: folder.into(),
        instructions: "Keep project instructions".into(),
        created_ts: id,
    }
}

fn bind_used_tab(app: &mut AwiliApp, folder: &str) -> usize {
    app.new_code_tab(awili_code::CodeEngine::Claude);
    let ix = app.work.code_active;
    let tab = &mut app.work.code_tabs[ix];
    tab.project_folder = Some(folder.into());
    tab.convo
        .push(awili_core::Role::User, "Keep this transcript");
    tab.rows
        .push(crate::MessageRow::new(tab.convo.message(0).unwrap()));
    *tab.session_slot.lock().unwrap() = Some("resume-original".into());
    ix
}

#[gpui::test]
fn project_catalog_edits_keep_used_tab_folder_and_resume(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::project_catalog_edits_keep_used_tab_folder_and_resume") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.mode = Mode::Code;
        app.work.projects = vec![project(1, "/repo/a"), project(2, "/repo/b")];
        app.work.code_active_project = 1; // compatibility seed, no explicit folder yet
        let used_b = bind_used_tab(app, "/repo/b");
        app.work.folder = Some("/repo/c".into());
        app.save_project_folder(app.work.folder.clone().unwrap(), cx); // inserts before B in the catalog
        assert_eq!(app.code_cwd().to_str(), Some("/repo/b"));
        app.remove_project_by_id(1, "/repo/a".into(), cx); // removes before B
        assert_eq!(app.code_cwd().to_str(), Some("/repo/b"));
        app.work.running = true;
        app.work.folder = Some("/repo/b".into());
        app.remove_project_by_id(2, "/repo/b".into(), cx);
        assert!(!app.work.projects.iter().any(|p| p.folder == "/repo/b"));
        assert_eq!(
            app.code_cwd().to_str(),
            Some("/repo/b"),
            "deleting saved row does not retarget a used tab"
        );
        assert!(
            app.active_code_project().is_none(),
            "deleted selection must not point at another saved row"
        );
        assert!(app.work.running);
        assert_eq!(
            app.work.folder.as_deref(),
            Some(std::path::Path::new("/repo/b"))
        );
        let tab = &app.work.code_tabs[used_b];
        assert_eq!(tab.project_folder.as_deref(), Some("/repo/b"));
        assert_eq!(tab.convo.message(0).unwrap().text, "Keep this transcript");
        assert_eq!(
            tab.session_slot.lock().unwrap().as_deref(),
            Some("resume-original")
        );
        let used_c = bind_used_tab(app, "/repo/c");
        app.activate_code_tab(used_c);
        app.activate_code_tab(used_b);
        assert_eq!(
            app.code_cwd().to_str(),
            Some("/repo/b"),
            "unsaved tab restores its own folder"
        );
    });
}

#[gpui::test]
fn project_failed_save_keeps_unsaved_tab_identity(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::project_failed_save_keeps_unsaved_tab_identity") {
        return;
    }
    cx.executor().allow_parking();
    let entity = cx.new(AwiliApp::new);
    let (tx, rx) = std::sync::mpsc::channel();
    entity.update(cx, |app, cx| {
        app.persist = Some(crate::Persistence {
            tx,
            failed: Default::default(),
        });
        app.mode = Mode::Code;
        app.work.projects = vec![project(1, "/repo/a")];
        app.new_code_tab(awili_code::CodeEngine::Claude);
        app.add_code_project("/repo/unsaved".into(), cx);
        assert_eq!(
            app.work.projects.len(),
            1,
            "no false saved row before acknowledgement"
        );
    });
    let mut selection_seen = false;
    let mut reply = None;
    for cmd in rx.try_iter() {
        match cmd {
            crate::PersistCmd::SetState { key, value }
                if key == crate::persistence::WORKSHOP_ACTIVE_PROJECT_KEY =>
            {
                assert_eq!(value, "/repo/unsaved");
                selection_seen = true;
            }
            crate::PersistCmd::SaveProject { reply: channel, .. } => {
                assert!(
                    selection_seen,
                    "folder selection is persisted before saving its row"
                );
                reply = Some(channel);
            }
            _ => {}
        }
    }
    let reply = reply.expect("actual save command");
    reply
        .send_blocking(Err("fixture write refused".into()))
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(
            app.work.projects.len(),
            1,
            "failed save keeps canonical rows"
        );
        assert_eq!(app.code_cwd().to_str(), Some("/repo/unsaved"));
        let used_b = bind_used_tab(app, "/repo/unsaved");
        let used_a = bind_used_tab(app, "/repo/a");
        app.activate_code_tab(used_a);
        assert_eq!(app.code_cwd().to_str(), Some("/repo/a"));
        app.activate_code_tab(used_b);
        assert_eq!(app.code_cwd().to_str(), Some("/repo/unsaved"));
        assert_eq!(
            app.work.code_tabs[used_b]
                .session_slot
                .lock()
                .unwrap()
                .as_deref(),
            Some("resume-original")
        );
    });
}

#[gpui::test]
fn project_first_native_tab_keeps_fallback_unsaved(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::project_first_native_tab_keeps_fallback_unsaved") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.work.projects = vec![project(1, "/repo/other")];
        app.work.recent_folders.clear();
        app.work.workshop_selected_folder = None;
        app.work.code_tabs.clear();
        app.work.folder = Some("/repo/unsaved".into());
        app.ensure_code_tab(cx);
        assert_eq!(app.code_cwd().to_str(), Some("/repo/unsaved"));
        assert_eq!(
            app.work.projects.len(),
            1,
            "opening a tab adds no saved row"
        );
        assert!(app.work.recent_folders.is_empty());
        app.replace_workshop_projects(vec![project(1, "/repo/other")]);
        assert_eq!(app.code_cwd().to_str(), Some("/repo/unsaved"));
        assert!(app.active_code_project().is_none());
    });
}

#[gpui::test]
fn project_malformed_import_is_visible_and_preserved(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::project_malformed_import_is_visible_and_preserved") {
        return;
    }
    let store = awili_persist::Store::open_default().unwrap();
    store
        .add_project("Existing", "/repo/existing", "", 1)
        .unwrap();
    store.set_app_state("code_projects", "{malformed").unwrap();
    drop(store);
    let entity = cx.new(AwiliApp::new);
    entity.read_with(cx, |app, _| {
        assert_eq!(app.work.projects.len(), 1);
        assert_eq!(app.work.projects[0].folder, "/repo/existing");
        assert!(app
            .toast
            .msg
            .as_ref()
            .unwrap()
            .contains("could not be imported"));
    });
    let store = awili_persist::Store::open_default().unwrap();
    assert_eq!(
        store.get_app_state("code_projects").unwrap().as_deref(),
        Some("{malformed")
    );
    assert!(store
        .get_app_state("workshop_projects_imported")
        .unwrap()
        .is_none());
}

#[gpui::test]
fn project_worker_snapshots_cannot_rewind_catalog(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::project_worker_snapshots_cannot_rewind_catalog") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        app.persist = None;
        app.work.workshop_selected_folder = Some("/repo/b".into());
        app.apply_project_snapshot(crate::persistence::ProjectSnapshot {
            revision: 2,
            projects: vec![project(2, "/repo/b"), project(1, "/repo/a")],
        });
        app.apply_project_snapshot(crate::persistence::ProjectSnapshot {
            revision: 1,
            projects: vec![project(1, "/repo/a")],
        });
        assert_eq!(app.work.projects.len(), 2);
        assert_eq!(app.active_code_project().unwrap().folder, "/repo/b");
        assert_eq!(
            app.active_code_project().unwrap().instructions,
            "Keep project instructions"
        );
        assert_eq!(app.work.project_revision, 2);
    });
}

#[gpui::test]
fn workshop_tabs_select_exact_threads_and_keep_agent_identity(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_tabs_select_exact_threads_and_keep_agent_identity") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let chat = app.create_conversation(awili_core::ConvKind::Work);
        let work = app.create_conversation(awili_core::ConvKind::Work);
        app.open_conversation_from_history(&chat, cx);
        app.sync_workshop_tabs();
        let chat_tab = app.shell.tabs.active.clone().unwrap();
        app.open_conversation_from_history(&work, cx);
        app.sync_workshop_tabs();
        let work_tab = app.shell.tabs.active.clone().unwrap();
        app.set_mode(Mode::Code, cx);
        let b = bind_used_tab(app, "/repo/b");
        app.activate_code_tab(b);
        let agent_id = app.work.code_tabs[b].id;
        app.sync_workshop_tabs();
        let agent_tab = app.shell.tabs.active.clone().unwrap();
        app.select_workshop_tab(&chat_tab, cx);
        assert_eq!(app.sidebar.active_id(), chat);
        assert_eq!(app.mode, Mode::Work);
        app.select_workshop_tab(&work_tab, cx);
        assert_eq!(app.sidebar.active_id(), work);
        assert_eq!(app.mode, Mode::Work);
        app.select_workshop_tab(&agent_tab, cx);
        assert_eq!(app.work.code_tabs[app.work.code_active].id, agent_id);
        assert_eq!(app.code_cwd().to_str(), Some("/repo/b"));
        assert_eq!(
            app.work.code_tabs[app.work.code_active]
                .session_slot
                .lock()
                .unwrap()
                .as_deref(),
            Some("resume-original")
        );
        let earlier = app
            .shell
            .tabs
            .tabs
            .iter()
            .find(|t| matches!(t.backing, crate::workshop::Backing::Agent(id) if id != agent_id))
            .unwrap()
            .id
            .clone();
        app.close_workshop_tab(&earlier, cx);
        assert_eq!(
            app.work.code_tabs[app.work.code_active].id, agent_id,
            "removing an earlier adapter keeps stable active identity"
        );
        assert_eq!(app.shell.tabs.active.as_deref(), Some(agent_tab.as_str()));
    });
}

#[gpui::test]
fn workshop_tabs_keep_stopping_and_parked_owners_open(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_tabs_keep_stopping_and_parked_owners_open") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let owner = app.create_conversation(awili_core::ConvKind::Work);
        let idle = app.create_conversation(awili_core::ConvKind::Work);
        app.open_conversation_from_history(&owner, cx);
        app.sync_workshop_tabs();
        let owner_tab = app.shell.tabs.active.clone().unwrap();
        app.open_conversation_from_history(&idle, cx);
        app.sync_workshop_tabs();
        let idle_tab = app.shell.tabs.active.clone().unwrap();
        app.work.conv_id = Some(owner.clone());
        app.work.gateway_stop_pending = true;
        app.close_workshop_tab(&owner_tab, cx);
        assert!(app.shell.tabs.tabs.iter().any(|t| t.id == owner_tab));
        app.close_workshop_tab(&idle_tab, cx);
        assert!(!app.shell.tabs.tabs.iter().any(|t| t.id == idle_tab));
        assert!(
            app.conversations.get(&idle).is_some(),
            "tab closure preserves history"
        );
        app.work.gateway_stop_pending = false;
        app.work.local.health.pending_send = Some(crate::gateway_health::PendingSend {
            text: "parked".into(),
            conv_id: owner.clone(),
        });
        app.close_workshop_tab(&owner_tab, cx);
        assert!(app.shell.tabs.tabs.iter().any(|t| t.id == owner_tab));
        app.work.local.health.pending_send = None;
        let key = crate::ask_mode::AskKey::Work(owner.clone());
        app.work
            .plan_gates
            .arm(key.clone(), awili_code::CodeEngine::Claude, None);
        assert!(
            app.work
                .plan_gates
                .settle(&key, awili_code::PLAN_READY_MARKER, true)
                .1
        );
        app.close_workshop_tab(&owner_tab, cx);
        assert!(
            app.shell.tabs.tabs.iter().any(|t| t.id == owner_tab),
            "idle owner keeps its waiting plan"
        );
        app.work.plan_gates.clear(&key);
        app.close_workshop_tab(&owner_tab, cx);
        assert!(!app.shell.tabs.tabs.iter().any(|t| t.id == owner_tab));
        assert!(
            !app.shell.tabs.tabs.is_empty(),
            "last tab closure leaves a usable thread"
        );
    });
}

#[gpui::test]
fn workshop_tabs_malformed_state_never_resurrects_legacy_or_overwrites(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_tabs_malformed_state_never_resurrects_legacy_or_overwrites",
    ) {
        return;
    }
    let store = awili_persist::Store::open_default().unwrap();
    store
        .set_app_state(crate::workshop::TABS_KEY, "{bad-state")
        .unwrap();
    store
        .set_app_state(
            crate::code_reopen::CODE_OPEN_TABS_KEY,
            r#"{"tabs":[{"conv":null,"executor":"agent:claude","folder":"/legacy"}],"active":0}"#,
        )
        .unwrap();
    drop(store);
    let entity = cx.new(AwiliApp::new);
    let (tx, rx) = std::sync::mpsc::channel();
    entity.update(cx, |app, cx| {
        assert!(
            app.work.code_tabs.is_empty(),
            "invalid canonical state is not absent"
        );
        assert_eq!(app.shell.tabs.unreadable.as_deref(), Some("{bad-state"));
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = Some(crate::Persistence {
            tx,
            failed: Default::default(),
        });
        app.sync_workshop_tabs();
        assert!(
            !app.shell.tabs.tabs.is_empty(),
            "exercise a writable current snapshot"
        );
        app.save_workshop_tabs_if_changed(cx);
    });
    assert!(!rx
        .try_iter()
        .any(|cmd| matches!(cmd, crate::PersistCmd::SaveWorkshopTabs { .. })));
    let store = awili_persist::Store::open_default().unwrap();
    assert_eq!(
        store
            .get_app_state(crate::workshop::TABS_KEY)
            .unwrap()
            .as_deref(),
        Some("{bad-state")
    );
}

#[gpui::test]
fn workshop_tabs_history_opens_only_the_requested_thread(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_tabs_history_opens_only_the_requested_thread") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = None;
        app.set_mode(Mode::Work, cx);
        app.sync_workshop_tabs();
        let remembered = app.sidebar.active_id().to_string();
        let requested = app.create_conversation(awili_core::ConvKind::Work);
        app.shell.tabs.tabs.clear();
        app.shell.tabs.active = None;
        app.sidebar.last_chat_id = Some(remembered);
        app.mode = Mode::Design;
        app.open_conversation_from_history(&requested, cx);
        assert_eq!(app.sidebar.active_id(), requested);
        assert_eq!(app.shell.tabs.tabs.len(), 1);
        assert_eq!(
            app.shell.tabs.tabs[0].backing,
            crate::workshop::Backing::Conversation(requested)
        );
    });
}

#[gpui::test]
fn workshop_tabs_valid_empty_snapshot_never_resurrects_legacy(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_tabs_valid_empty_snapshot_never_resurrects_legacy") {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    store
        .set_app_state(
            crate::workshop::TABS_KEY,
            r#"{"version":1,"tabs":[],"active":null}"#,
        )
        .unwrap();
    store
        .set_app_state(
            crate::code_reopen::CODE_OPEN_TABS_KEY,
            r#"{"tabs":[{"conv":null,"executor":"agent:claude","folder":"/legacy"}],"active":0}"#,
        )
        .unwrap();
    drop(store);
    let entity = cx.new(AwiliApp::new);
    entity.read_with(cx, |app, _| {
        assert!(app.work.code_tabs.is_empty());
        assert_eq!(
            app.shell.tabs.tabs.len(),
            1,
            "one ordinary usable thread after empty canonical restore"
        );
        assert!(matches!(
            app.shell.tabs.tabs[0].backing,
            crate::workshop::Backing::Conversation(_)
        ));
        assert_eq!(app.mode, Mode::Work);
    });
}

#[gpui::test]
fn workshop_work_helpers_keep_identity_across_selection_and_restart(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_work_helpers_keep_identity_across_selection_and_restart")
    {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let first = cx.new(AwiliApp::new);
    let (a, b, raw) = first.update(cx, |app, cx| {
        app.new_workshop_with(crate::work::WorkExecutor::InApp, cx);
        let a = app.sidebar.active_id().to_string();
        app.new_workshop_with(crate::work::WorkExecutor::Gateway, cx);
        let b = app.sidebar.active_id().to_string();
        assert_ne!(a, b, "distinct open blank threads keep distinct helpers");
        app.open_conversation_from_history(&a, cx);
        assert_eq!(app.work.executor, crate::work::WorkExecutor::InApp);
        let raw = serde_json::to_string(&app.workshop_snapshot()).unwrap();
        app.work.conv_id = Some(a.clone());
        app.work.running = true;
        let tab_count = app.shell.tabs.tabs.len();
        app.new_workshop_with(crate::work::WorkExecutor::Gateway, cx);
        assert_eq!(
            app.work.executor,
            crate::work::WorkExecutor::InApp,
            "new helper cannot retarget a live runner"
        );
        assert_eq!(app.shell.tabs.tabs.len(), tab_count);
        app.work.running = false;
        let key = crate::ask_mode::AskKey::Work(a.clone());
        app.work
            .plan_gates
            .arm(key.clone(), awili_code::CodeEngine::Claude, None);
        assert!(
            app.work
                .plan_gates
                .settle(&key, awili_code::PLAN_READY_MARKER, true)
                .1
        );
        app.new_workshop_with(crate::work::WorkExecutor::Gateway, cx);
        assert_eq!(
            app.shell.tabs.tabs.len(),
            tab_count,
            "waiting Work plan blocks helper retargeting"
        );
        assert_eq!(app.work.executor, crate::work::WorkExecutor::InApp);
        app.work.plan_gates.clear(&key);
        (a, b, raw)
    });
    // Ordered read confirms the preceding conversation creates were stored.
    let (tx, rx) = async_channel::bounded(1);
    first.update(cx, |app, _| {
        app.persist
            .as_ref()
            .unwrap()
            .send(crate::PersistCmd::ReadState {
                key: "workshop_helper_fixture_barrier".into(),
                reply: tx,
            })
    });
    rx.recv_blocking().unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    store
        .set_app_state(crate::workshop::TABS_KEY, &raw)
        .unwrap();
    drop(store);
    let restored = cx.new(AwiliApp::new);
    restored.update(cx, |app, cx| {
        assert_eq!(app.sidebar.active_id(), a);
        assert_eq!(app.work.executor, crate::work::WorkExecutor::InApp);
        app.open_conversation_from_history(&b, cx);
        assert_eq!(app.work.executor, crate::work::WorkExecutor::Gateway);
        app.open_conversation_from_history(&a, cx);
        assert_eq!(app.work.executor, crate::work::WorkExecutor::InApp);
    });
}

#[gpui::test]
fn workshop_last_blank_close_reuses_history_row(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_last_blank_close_reuses_history_row") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = None;
        app.set_mode(Mode::Work, cx);
        // No window renders in this fixture, so the open thread is given its
        // Workshop tab the way the first frame does.
        app.sync_workshop_tabs();
        let work = app.sidebar.active_id().to_string();
        app.shell
            .tabs
            .tabs
            .retain(|t| t.backing == crate::workshop::Backing::Conversation(work.clone()));
        for _ in 0..5 {
            let active = app.shell.tabs.active.clone().unwrap();
            app.close_workshop_tab(&active, cx);
            assert_eq!(app.shell.tabs.tabs.len(), 1);
            assert_ne!(app.shell.tabs.active.as_deref(), Some(active.as_str()));
        }
        assert_eq!(
            app.conversations
                .all()
                .iter()
                .filter(|c| c.kind == awili_core::ConvKind::Work)
                .count(),
            1,
            "blank close does not pile up history rows"
        );
    });
}

#[gpui::test]
fn workshop_project_import_dismissal_preserves_and_rechecks_data(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_project_import_dismissal_preserves_and_rechecks_data") {
        return;
    }
    let store = awili_persist::Store::open_default().unwrap();
    store
        .add_project("Existing", "/existing", "keep", 1)
        .unwrap();
    store.set_app_state("code_projects", "{broken-a").unwrap();
    drop(store);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        assert!(app.shell.tabs.project_import_notice.is_some());
        app.dismiss_workshop_project_import(cx);
        assert!(app.shell.tabs.project_import_notice.is_none());
    });
    let (tx, rx) = async_channel::bounded(1);
    entity.update(cx, |app, _| {
        app.persist
            .as_ref()
            .unwrap()
            .send(crate::PersistCmd::ReadState {
                key: crate::persistence::PROJECT_IMPORT_DISMISSED_KEY.into(),
                reply: tx,
            })
    });
    assert_eq!(
        rx.recv_blocking().unwrap().as_deref(),
        Some("legacy:{broken-a")
    );
    let restored = cx.new(AwiliApp::new);
    restored.read_with(cx, |app, _| {
        assert!(app.shell.tabs.project_import_notice.is_none())
    });
    let store = awili_persist::Store::open_default().unwrap();
    assert_eq!(
        store.get_app_state("code_projects").unwrap().as_deref(),
        Some("{broken-a")
    );
    assert!(store
        .get_app_state("workshop_projects_imported")
        .unwrap()
        .is_none());
    store.set_app_state("code_projects", "{broken-b").unwrap();
    let changed = cx.new(AwiliApp::new);
    changed.read_with(cx, |app, _| {
        assert!(app.shell.tabs.project_import_notice.is_some())
    });
    store
        .set_app_state(
            "code_projects",
            r#"[{"name":"Recovered","folder":"/recovered"}]"#,
        )
        .unwrap();
    let repaired = cx.new(AwiliApp::new);
    repaired.read_with(cx, |app, _| {
        assert!(app.shell.tabs.project_import_notice.is_none());
        assert!(app.work.projects.iter().any(|p| p.folder == "/recovered"));
        assert!(app
            .work
            .projects
            .iter()
            .any(|p| p.folder == "/existing" && p.instructions == "keep"));
    });
    assert!(store
        .get_app_state("workshop_projects_imported")
        .unwrap()
        .is_some());
}

#[gpui::test]
fn workshop_legacy_duplicate_tabs_write_a_valid_canonical_snapshot(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_legacy_duplicate_tabs_write_a_valid_canonical_snapshot")
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.code_tabs.clear();
        app.shell.tabs.tabs.clear();
        let id = app.create_conversation(awili_core::ConvKind::Code);
        let raw = serde_json::json!({"tabs":[
            {"conv":id,"executor":"agent:claude","folder":"/original"},
            {"conv":id,"executor":"agent:codex","folder":"/other"}
        ],"active":1})
        .to_string();
        app.restore_code_tabs(Some(&raw), &Default::default());
        assert_eq!(app.work.code_tabs.len(), 1);
        assert_eq!(
            app.work.code_tabs[app.work.code_active]
                .project_folder
                .as_deref(),
            Some("/original")
        );
        app.mode = Mode::Code;
        app.sync_workshop_tabs();
        let canonical = serde_json::to_string(&app.workshop_snapshot()).unwrap();
        assert!(crate::workshop::SavedWorkshop::parse(&canonical).is_ok());
    });
}

#[gpui::test]
fn workshop_composer_shortcuts_keep_closed_transcripts_in_history(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_composer_shortcuts_keep_closed_transcripts_in_history") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    {
        let (kind, event, mode) = (
            awili_core::ConvKind::Work,
            crate::ComposerEvent::SwitchWork,
            Mode::Work,
        );
        let (closed, composer) = entity.update(cx, |app, cx| {
            app.persist = None;
            app.onboarding = crate::onboarding::Onboarding::existing();
            let id = app.create_conversation(kind);
            app.views.get_mut(&id).unwrap().convo.push(awili_core::Role::User, "Keep closed transcript");
            app.open_conversation_from_history(&id, cx);
            let tab = app.shell.tabs.active.clone().unwrap();
            app.close_workshop_tab(&tab, cx);
            app.shell.tabs.tabs.retain(|tab| !matches!(&tab.backing,
                crate::workshop::Backing::Conversation(id) if app.conversations.get(id).is_some_and(|c| c.kind == kind)));
            (id, app.composer.clone())
        });
        composer.update(cx, |_, cx| cx.emit(event));
        cx.run_until_parked();
        entity.read_with(cx, |app, _| {
            assert_eq!(app.mode, mode);
            assert_ne!(app.sidebar.active_id(), closed);
            assert!(app.conversations.get(&closed).is_some());
            assert!(!app
                .shell
                .tabs
                .tabs
                .iter()
                .any(|t| t.backing == crate::workshop::Backing::Conversation(closed.clone())));
        });
    }
}

#[gpui::test]
fn workshop_work_stop_only_cancels_the_selected_owner(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_work_stop_only_cancels_the_selected_owner") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let owner = app.create_conversation(awili_core::ConvKind::Work);
        let idle = app.create_conversation(awili_core::ConvKind::Work);
        app.open_conversation_from_history(&owner, cx);
        app.work.conv_id = Some(owner.clone());
        app.work.executor = crate::work::WorkExecutor::InApp;
        app.work.running = true;
        app.work
            .cancel
            .store(false, std::sync::atomic::Ordering::SeqCst);
        app.open_conversation_from_history(&idle, cx);
        app.abort_active_stream(cx);
        assert!(!app.work.cancel.load(std::sync::atomic::Ordering::SeqCst));
        assert!(app.work.running);
        assert!(!app.work.owner_stopped);
        app.open_conversation_from_history(&owner, cx);
        app.abort_active_stream(cx);
        assert!(app.work.cancel.load(std::sync::atomic::Ordering::SeqCst));
        assert!(app.work.owner_stopped);
    });
}

#[gpui::test]
fn workshop_last_close_preserves_typed_draft_and_staged_file(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_last_close_preserves_typed_draft_and_staged_file") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Work, cx);
        app.sync_workshop_tabs();
        let owner = app.sidebar.active_id().to_string();
        app.shell
            .tabs
            .tabs
            .retain(|t| t.backing == crate::workshop::Backing::Conversation(owner.clone()));
        app.sync_composer_draft(cx);
        app.composer
            .update(cx, |c, cx| c.set_text("My unfinished draft", cx));
        let tab = app.shell.tabs.active.clone().unwrap();
        app.close_workshop_tab(&tab, cx);
        assert_ne!(app.sidebar.active_id(), owner);
        assert!(app.composer.read(cx).text().is_empty());
        assert!(!app
            .shell
            .tabs
            .tabs
            .iter()
            .any(|t| t.backing == crate::workshop::Backing::Conversation(owner.clone())));
        app.open_conversation_from_history(&owner, cx);
        assert_eq!(app.composer.read(cx).text(), "My unfinished draft");
        app.composer.update(cx, |c, cx| c.set_text("", cx));
        // The staged file's conversation must be a stored row. The startup
        // conversation may already be saved by now (inserting its id again
        // is a UNIQUE violation), so create it only when it is missing and
        // prove it is there either way.
        let store = app.search_store.as_ref().unwrap();
        if !store.conversations().unwrap().iter().any(|c| c.id == owner) {
            store
                .create_conversation_with_kind(&owner, "Draft", 1, "work")
                .unwrap();
        }
        assert!(
            store.conversations().unwrap().iter().any(|c| c.id == owner),
            "the conversation holding the staged file is a stored row"
        );
        app.attach_bytes("keep.txt", "text/plain", b"keep file", cx);
        assert_eq!(app.staged_attachments.len(), 1);
        app.shell
            .tabs
            .tabs
            .retain(|t| t.backing == crate::workshop::Backing::Conversation(owner.clone()));
        let tab = app.shell.tabs.active.clone().unwrap();
        app.close_workshop_tab(&tab, cx);
        assert_ne!(app.sidebar.active_id(), owner);
        assert!(app.composer.read(cx).text().is_empty());
        assert!(app.staged_attachments.is_empty());
        app.open_conversation_from_history(&owner, cx);
        assert_eq!(app.staged_attachments.len(), 1);
        assert_eq!(app.staged_attachments[0].filename, "keep.txt");
    });
}

#[gpui::test]
fn workshop_idle_cancel_keeps_plan_and_failure_notification_state(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_idle_cancel_keeps_plan_and_failure_notification_state") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let (owner, key, seq, composer) = entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Work, cx);
        let owner = app.sidebar.active_id().to_string();
        let key = crate::ask_mode::AskKey::Work(owner.clone());
        app.work.conv_id = Some(owner.clone());
        app.work
            .plan_gates
            .arm(key.clone(), awili_code::CodeEngine::Claude, None);
        assert!(
            app.work
                .plan_gates
                .settle(&key, awili_code::PLAN_READY_MARKER, true)
                .1
        );
        app.work
            .cancel
            .store(false, std::sync::atomic::Ordering::SeqCst);
        (owner, key, app.work.run_seq, app.composer.clone())
    });
    composer.update(cx, |_, cx| cx.emit(crate::ComposerEvent::Cancel));
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(app.work.plan_gates.open_for(&key).is_some());
        assert!(!app.work.owner_stopped);
        assert!(!app.work.cancel.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(app.work.run_seq, seq);
        let idle = app.create_conversation(awili_core::ConvKind::Work);
        app.open_conversation_from_history(&idle, cx);
        app.toast.msg = None;
        app.abort_active_stream(cx);
        assert!(
            app.toast.msg.is_none(),
            "idle cancellation makes no false running claim"
        );
        assert_eq!(app.work.conv_id.as_deref(), Some(owner.as_str()));
    });
}

#[gpui::test]
fn workshop_snapshot_persists_only_explicit_work_helpers(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_snapshot_persists_only_explicit_work_helpers") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Work, cx);
        app.sync_workshop_tabs();
        let id = app.sidebar.active_id().to_string();
        app.work.chip_picked.remove(&id);
        let raw = serde_json::to_value(app.workshop_snapshot()).unwrap();
        assert!(raw["work_helpers"].get(&id).is_none());
        app.pick_work_executor(crate::work::WorkExecutor::Gateway, cx);
        let raw = serde_json::to_value(app.workshop_snapshot()).unwrap();
        assert_eq!(raw["work_helpers"][&id], "gateway");
    });
}

#[gpui::test]
fn workshop_agent_model_change_restores_in_the_same_thread(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_agent_model_change_restores_in_the_same_thread") {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    store
        .create_conversation_with_kind("model-thread", "Keep this", 1, "code")
        .unwrap();
    store
        .append_message("model-thread", "user", "original", 1)
        .unwrap();
    let meta = crate::code_history::CodeConvMeta::for_turn(
        awili_code::CodeEngine::Codex,
        None,
        Some("/repo".into()),
        Some("resume-original".into()),
    );
    store
        .set_app_state(
            &crate::code_history::code_conv_key("model-thread"),
            &meta.to_json(),
        )
        .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let raw = serde_json::json!({"version":1,"tabs":[{"id":id,"backing":{"Agent":{"conv":"model-thread","executor":"agent:codex:next-model","folder":"/repo"}}}],"active":id}).to_string();
    store
        .set_app_state(crate::workshop::TABS_KEY, &raw)
        .unwrap();
    drop(store);
    let entity = cx.new(AwiliApp::new);
    entity.read_with(cx, |app, _| {
        assert_eq!(app.shell.tabs.tabs.len(), 1);
        assert_eq!(app.shell.tabs.active.as_deref(), Some(id.as_str()));
        let tab = &app.work.code_tabs[app.work.code_active];
        assert_eq!(tab.model.as_deref(), Some("next-model"));
        assert_eq!(tab.history.conv_id.as_deref(), Some("model-thread"));
        assert_eq!(tab.convo.message(0).unwrap().text, "original");
        assert_eq!(
            tab.session_slot.lock().unwrap().as_deref(),
            Some("resume-original")
        );
    });
}

#[gpui::test]
fn workshop_stop_removes_selected_probe_intent_before_late_success(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_stop_removes_selected_probe_intent_before_late_success")
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.memory.enabled = false;
        app.set_mode(Mode::Work, cx);
        let old = app.sidebar.active_id().to_string();
        app.work.conv_id = Some(old.clone());
        app.work.worker_conv = Some(old.clone());
        let pending = app.create_conversation(awili_core::ConvKind::Work);
        app.work.local.health.pending_send = Some(crate::gateway_health::PendingSend {
            text: "cancel this pending request".into(),
            conv_id: pending.clone(),
        });
        app.work
            .cancel
            .store(false, std::sync::atomic::Ordering::SeqCst);
        app.abort_active_stream(cx);
        assert!(
            app.work.local.health.pending_send.is_some(),
            "another thread cannot cancel it"
        );
        app.open_conversation_from_history(&pending, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("draft already here", cx));
        let seq = app.work.run_seq;
        app.abort_active_stream(cx);
        assert!(app.work.local.health.pending_send.is_none());
        assert_eq!(app.work.run_seq, seq, "old runner epoch stays intact");
        assert!(!app.work.cancel.load(std::sync::atomic::Ordering::SeqCst));
        let draft = app.composer.read(cx).text().to_string();
        assert!(
            draft.contains("draft already here") && draft.contains("cancel this pending request")
        );
        let spawned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = spawned.clone();
        crate::work::TEST_WORKER_SPAWN.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |_| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }))
        });
        app.on_local_probe(app.work.local.endpoint_gen, Ok(()), cx);
        assert!(!app.work.running);
        assert_eq!(app.work.run_seq, seq);
        assert!(!spawned.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(app.composer.read(cx).text(), draft);
        crate::work::TEST_WORKER_SPAWN.with(|slot| slot.borrow_mut().take());
    });
}

#[gpui::test]
fn workshop_stop_removes_selected_queued_prompt_without_interrupting_prior_stop(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_tests::workshop_stop_removes_selected_queued_prompt_without_interrupting_prior_stop") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Work, cx);
        let old = app.sidebar.active_id().to_string();
        let pending = app.create_conversation(awili_core::ConvKind::Work);
        app.work.conv_id = Some(old.clone());
        app.work.worker_conv = Some(old.clone());
        app.work.gateway_stop_pending = true;
        app.work.queued_after_stop = Some(crate::work::QueuedPrompt {
            text: "queued in B".into(),
            conv_id: pending.clone(),
            executor: crate::work::WorkExecutor::Gateway,
        });
        let (tx, rx) = std::sync::mpsc::channel();
        app.work.gateway_cmd_tx = Some(tx);
        let seq = app.work.run_seq;
        app.abort_active_stream(cx);
        assert!(
            app.work.queued_after_stop.is_some(),
            "A's repeated Stop preserves B's prompt"
        );
        app.open_conversation_from_history(&pending, cx);
        app.abort_active_stream(cx);
        assert!(app.work.queued_after_stop.is_none());
        assert!(app.work.gateway_stop_pending);
        assert_eq!(app.work.conv_id.as_deref(), Some(old.as_str()));
        assert_eq!(app.work.run_seq, seq);
        assert!(app.work.gateway_cmd_tx.is_some());
        assert!(rx.try_recv().is_err());
        assert!(app.composer.read(cx).text().contains("queued in B"));
        app.apply_work_update(
            crate::work::WorkUpdate::Terminal {
                status: awili_work::run_events::RunStatus::Stopped,
                output: None,
                model: None,
            },
            cx,
        );
        assert!(!app.work.gateway_stop_pending);
        assert!(!app.work.running);
        assert!(app.work.queued_after_stop.is_none());
        assert!(!rx
            .try_iter()
            .any(|c| matches!(c, crate::work::GatewayCmd::Prompt { .. })));
    });
}

#[gpui::test]
fn workshop_agent_drafts_keep_their_tabs_reachable_until_cleared(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_agent_drafts_keep_their_tabs_reachable_until_cleared") {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.new_workshop_with(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None,
            },
            cx,
        );
        let blank = app.shell.tabs.active.clone().unwrap();
        app.composer
            .update(cx, |c, cx| c.set_text("Keep my agent draft", cx));
        app.close_workshop_tab(&blank, cx);
        assert_eq!(app.shell.tabs.active.as_deref(), Some(blank.as_str()));
        app.new_workshop_with(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Codex,
                model: None,
            },
            cx,
        );
        app.close_workshop_tab(&blank, cx);
        assert!(app.shell.tabs.tabs.iter().any(|t| t.id == blank));
        app.select_workshop_tab(&blank, cx);
        assert_eq!(app.composer.read(cx).text(), "Keep my agent draft");
        app.composer.update(cx, |c, cx| c.set_text("", cx));
        app.close_workshop_tab(&blank, cx);
        assert!(!app.shell.tabs.tabs.iter().any(|t| t.id == blank));
    });
}

#[gpui::test]
fn workshop_choose_folder_refuses_unsettled_work_before_opening_box(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_choose_folder_refuses_unsettled_work_before_opening_box")
    {
        return;
    }
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.running = true;
        app.open_workshop_folder_box(window, cx);
        assert!(!app.palette.open);
        assert_eq!(app.mode, Mode::Work);
        app.work.running = false;
        app.set_mode(Mode::Work, cx);
        let key = crate::ask_mode::AskKey::Work(app.sidebar.active_id().to_string());
        app.work
            .plan_gates
            .arm(key.clone(), awili_code::CodeEngine::Claude, None);
        assert!(
            app.work
                .plan_gates
                .settle(&key, awili_code::PLAN_READY_MARKER, true)
                .1
        );
        app.open_workshop_folder_box(window, cx);
        assert!(!app.palette.open);
        app.work.plan_gates.clear(&key);
        app.open_workshop_folder_box(window, cx);
        assert!(app.palette.open && app.palette.folder_box);
    });
}

/// The Workshop composer's placeholder with no folder bound: every Workshop
/// thread is a Work thread.
const WORKSHOP_PLACEHOLDER: &str = "Tell Awili what to do\u{2026}";

fn used_archive_thread(app: &mut AwiliApp, kind: awili_core::ConvKind) -> String {
    let id = app.create_conversation(kind);
    let mut state = awili_core::ConversationState::new();
    state.push(awili_core::Role::User, "Keep this question");
    state.push(awili_core::Role::Assistant, "Keep this answer");
    app.views
        .insert(id.clone(), crate::conv_view::ConvView::from_state(state));
    id
}

#[gpui::test]
fn workshop_history_removal_preserves_other_native_owner(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_history_removal_preserves_other_native_owner") {
        return;
    }
    cx.executor().allow_parking();
    for delete in [false, true] {
        let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
        view.update_in(window, |app, window, cx| {
            app.persist = None;
            app.onboarding = crate::onboarding::Onboarding::existing();
            let stored = used_archive_thread(app, awili_core::ConvKind::Work);
            app.open_conversation_from_history(&stored, cx);
            app.new_workshop_with(
                crate::work::WorkExecutor::Agent {
                    engine: awili_code::CodeEngine::Claude,
                    model: None,
                },
                cx,
            );
            app.composer.update(cx, |composer, cx| {
                composer.set_text("Keep native draft", cx)
            });
            let tab = app.shell.tabs.active.clone();
            let runtime = app.work.code_tabs[app.work.code_active].id;
            assert_eq!(app.sidebar.active_id(), stored);
            if delete {
                app.delete_conversation(&stored, window, cx);
                assert!(app.conversations.get(&stored).is_none());
            } else {
                app.archive_conversation(&stored, window, cx);
                assert!(app.conversations.get(&stored).unwrap().archived);
            }
            assert_eq!(app.mode, Mode::Code);
            assert_eq!(app.shell.tabs.active, tab);
            assert_eq!(app.work.code_tabs[app.work.code_active].id, runtime);
            assert_eq!(app.composer.read(cx).text(), "Keep native draft");
            assert_ne!(app.sidebar.active_id(), stored);
            assert!(app
                .conversations
                .get(app.sidebar.active_id())
                .is_some_and(|c| !c.archived));
            let fallback = app.sidebar.active_id().to_string();
            let store = app.search_store.as_ref().unwrap();
            let file = store
                .store_attachment("fallback.txt", "text/plain", b"Other thread's file")
                .unwrap();
            store.stage_attachment(&fallback, file).unwrap();
            let expected_files: Vec<_> = store
                .get_staged_attachments(&fallback)
                .unwrap()
                .into_iter()
                .map(|file| file.id)
                .collect();
            app.select_workshop_tab(tab.as_deref().unwrap(), cx);
            assert!(
                app.staged_attachments.is_empty(),
                "blank native tray borrowed fallback files"
            );
            assert_eq!(
                app.search_store
                    .as_ref()
                    .unwrap()
                    .get_staged_attachments(&fallback)
                    .unwrap()
                    .into_iter()
                    .map(|file| file.id)
                    .collect::<Vec<_>>(),
                expected_files
            );
            if !delete {
                app.open_conversation_from_history(&stored, cx);
                assert!(app.shell.archived_reader.is_some());
                app.close_workshop_archive(cx);
                assert_eq!(app.mode, Mode::Code);
                assert_eq!(app.shell.tabs.active, tab);
                assert_eq!(app.work.code_tabs[app.work.code_active].id, runtime);
                assert_eq!(app.composer.read(cx).text(), "Keep native draft");
            }
            // Exercise the real input-owner admission without starting a CLI.
            let owner = app
                .workshop_input_owner(false, cx)
                .expect("native input remains admissible");
            assert_ne!(owner, stored);
            assert_eq!(app.sidebar.active_id(), owner);
            assert_eq!(
                app.work.code_tabs[app.work.code_active]
                    .history
                    .conv_id
                    .as_deref(),
                Some(owner.as_str())
            );
            assert!(app.conversations.get(&owner).is_some_and(|c| !c.archived));
        });
    }
}

#[gpui::test]
fn workshop_archives_close_idle_backings_and_read_without_runtime_or_save(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_archives_close_idle_backings_and_read_without_runtime_or_save",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    for kind in [awili_core::ConvKind::Work, awili_core::ConvKind::Code] {
        let id = view.update_in(window, |app, window, cx| {
            app.persist = None;
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.fleet_pack = None;
            app.memory.enabled = false;
            let id = used_archive_thread(app, kind);
            app.open_conversation_from_history(&id, cx);
            let other = used_archive_thread(app, awili_core::ConvKind::Work);
            app.open_conversation_from_history(&other, cx);
            app.open_conversation_from_history(&id, cx);
            app.archive_conversation(&id, window, cx);
            assert!(app.conversations.get(&id).unwrap().archived);
            assert!(
                !app.conversations
                    .get(app.sidebar.active_id())
                    .unwrap()
                    .archived
            );
            assert!(!app
                .work
                .code_tabs
                .iter()
                .any(|s| s.history.conv_id.as_deref() == Some(&id)));
            for _ in 0..3 {
                app.sync_workshop_tabs();
            }
            assert!(!serde_json::to_string(&app.workshop_snapshot())
                .unwrap()
                .contains(&id));
            assert_eq!(app.views[&id].convo.messages().len(), 2);
            app.composer
                .update(cx, |c, cx| c.set_text("Previous owner's draft", cx));
            let saved = serde_json::to_string(&app.workshop_snapshot()).unwrap();
            let runtime_ids = app.work.code_tabs.iter().map(|s| s.id).collect::<Vec<_>>();
            let helper = app.work.executor.clone();
            let folder = app.work.folder.clone();
            let seq = app.work.run_seq;
            app.open_conversation_from_history(&id, cx);
            assert!(app.shell.archived_reader.is_some());
            assert_eq!(app.sidebar.active_id(), id);
            assert!(app.composer_draft_key().is_none());
            app.sync_workshop_tabs();
            assert_eq!(
                serde_json::to_string(&app.workshop_snapshot()).unwrap(),
                saved
            );
            assert_eq!(
                app.work.code_tabs.iter().map(|s| s.id).collect::<Vec<_>>(),
                runtime_ids
            );
            assert_eq!(app.work.executor, helper);
            assert_eq!(app.work.folder, folder);
            app.dispatch_composer_text("Do not send".into(), cx);
            app.submit_composer_draft(cx);
            app.retry_work_in(id.clone(), 1, cx);
            app.submit_code_turn_kind("Do not run".into(), awili_core::MessageKind::Normal, cx);
            app.submit_work_run_kind(
                "Do not work".into(),
                id.clone(),
                awili_core::MessageKind::Normal,
                cx,
            );
            assert_eq!(app.views[&id].convo.messages().len(), 2);
            assert_eq!(app.work.run_seq, seq);
            assert!(!app.work.running);
            assert_eq!(app.composer.read(cx).text(), "Previous owner's draft");
            id
        });
        window.run_until_parked();
        assert!(window.debug_bounds("awili-archive-readonly").is_some());
        assert!(
            window
                .debug_bounds("ui-button-awili-workshop-history-toggle")
                .is_none(),
            "the stored-transcript reader has no Conversations toggle or panel"
        );
        view.update(window, |app, cx| {
            app.close_workshop_archive(cx);
            assert!(app.shell.archived_reader.is_none());
            assert_eq!(app.composer.read(cx).text(), "Previous owner's draft");
            app.unarchive_conversation(&id, cx);
            app.open_conversation_from_history(&id, cx);
            assert_eq!(app.views[&id].convo.messages().len(), 2);
            assert!(!app.conversations.get(&id).unwrap().archived);
            assert!(!app.shell.tabs.tabs.is_empty());
        });
    }
}

#[gpui::test]
fn workshop_archive_refuses_busy_pending_plan_and_native_draft_owners(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_archive_refuses_busy_pending_plan_and_native_draft_owners",
    ) {
        return;
    }
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let id = used_archive_thread(app, awili_core::ConvKind::Work);
        app.open_conversation_from_history(&id, cx);
        app.work.conv_id = Some(id.clone());
        let key = crate::ask_mode::AskKey::Work(id.clone());
        for state in 0..10 {
            match state {
                0 => app.work.running = true,
                1 => app.work.gateway_stop_pending = true,
                2 => app.work.agent_gate_pending = true,
                3 => {
                    app.work.local.health.pending_send = Some(crate::gateway_health::PendingSend {
                        text: "Waiting local".into(),
                        conv_id: id.clone(),
                    })
                }
                4 => {
                    app.work.queued_after_stop = Some(crate::work::QueuedPrompt {
                        text: "Waiting Stop".into(),
                        conv_id: id.clone(),
                        executor: crate::work::WorkExecutor::InApp,
                    })
                }
                5 => {
                    app.work
                        .plan_gates
                        .arm(key.clone(), awili_code::CodeEngine::Claude, None);
                    app.work
                        .plan_gates
                        .settle(&key, awili_code::PLAN_READY_MARKER, true);
                }
                6 => {
                    app.work.health.gateway.pending_send =
                        Some(crate::gateway_health::PendingSend {
                            text: "Waiting gateway".into(),
                            conv_id: id.clone(),
                        })
                }
                7 => {
                    app.work.panel.approval = Some(crate::work::ApprovalCard {
                        approval_id: "archive-approval".into(),
                        kind: "read".into(),
                        prompt: "Read?".into(),
                        risk: awili_gateway::events::ApprovalRisk::ReadOnly,
                        delivery: Default::default(),
                    })
                }
                8 => {
                    app.work.panel.credential = Some(crate::work::CredentialPrompt {
                        request_id: "archive-credential".into(),
                        kind: crate::work::CredentialKind::Secret,
                        env_var: "TEST_SECRET".into(),
                        prompt: "Supply?".into(),
                    })
                }
                _ => {
                    app.work.local.down = Some(crate::local_model_health::LocalDown {
                        reason: crate::local_model_health::DownReason::Unreachable(
                            "test model down".into(),
                        ),
                        text: Some("Held message".into()),
                        conv_id: Some(id.clone()),
                    })
                }
            }
            app.archive_conversation(&id, window, cx);
            assert!(
                !app.conversations.get(&id).unwrap().archived,
                "pending state {state} retains its owner"
            );
            if state == 9 {
                app.delete_conversation(&id, window, cx);
                assert!(app.conversations.get(&id).is_some());
                let tab = app
                    .shell
                    .tabs
                    .tabs
                    .iter()
                    .find(|t| t.backing == crate::workshop::Backing::Conversation(id.clone()))
                    .unwrap()
                    .id
                    .clone();
                app.close_workshop_tab(&tab, cx);
                assert!(app.shell.tabs.tabs.iter().any(|t| t.id == tab));
                assert_eq!(
                    app.work.local.down.as_ref().unwrap().text.as_deref(),
                    Some("Held message")
                );
            }
            app.work.running = false;
            app.work.gateway_stop_pending = false;
            app.work.agent_gate_pending = false;
            app.work.local.health.pending_send = None;
            app.work.health.gateway.pending_send = None;
            app.work.panel.approval = None;
            app.work.panel.credential = None;
            app.work.local.down = None;
            app.work.queued_after_stop = None;
            app.work.plan_gates.clear(&key);
        }
        let code = used_archive_thread(app, awili_core::ConvKind::Code);
        app.open_conversation_from_history(&code, cx);
        app.work.code_tabs[app.work.code_active].streaming = true;
        app.archive_conversation(&code, window, cx);
        assert!(!app.conversations.get(&code).unwrap().archived);
        app.delete_conversation(&code, window, cx);
        assert!(app.conversations.get(&code).is_some());
        app.work.code_tabs[app.work.code_active].streaming = false;
        app.composer
            .update(cx, |c, cx| c.set_text("Native draft stays reachable", cx));
        app.archive_conversation(&code, window, cx);
        assert!(!app.conversations.get(&code).unwrap().archived);
        app.open_conversation_from_history(&id, cx);
        app.archive_conversation(&code, window, cx);
        assert!(
            !app.conversations.get(&code).unwrap().archived,
            "background runtime draft also stays reachable"
        );
        app.open_conversation_from_history(&code, cx);
        assert_eq!(app.composer.read(cx).text(), "Native draft stays reachable");
        app.composer.update(cx, |c, cx| c.set_text("", cx));
        app.archive_conversation(&code, window, cx);
        assert!(app.conversations.get(&code).unwrap().archived);
    });
}

#[gpui::test]
fn workshop_restart_skips_archived_shared_and_legacy_backings(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_restart_skips_archived_shared_and_legacy_backings") {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    for (id, kind) in [
        ("archived-chat", "chat"),
        ("archived-work", "work"),
        ("archived-code", "code"),
        ("live-chat", "chat"),
    ] {
        store
            .create_conversation_with_kind(id, id, 1, kind)
            .unwrap();
        store
            .append_message(id, "user", "Saved question", 2)
            .unwrap();
        if id != "live-chat" {
            store.set_archived(id, true).unwrap();
        }
    }
    let live = uuid::Uuid::new_v4().to_string();
    let raw = serde_json::json!({"version":1,"tabs":[
        {"id":uuid::Uuid::new_v4(),"backing":{"Conversation":"archived-chat"}},
        {"id":uuid::Uuid::new_v4(),"backing":{"Conversation":"archived-work"}},
        {"id":uuid::Uuid::new_v4(),"backing":{"Agent":{"conv":"archived-code","executor":"agent:claude","folder":null}}},
        {"id":live,"backing":{"Conversation":"live-chat"}}
    ],"active":live,"work_helpers":{"archived-work":"in-app"}}).to_string();
    store
        .set_app_state(crate::workshop::TABS_KEY, &raw)
        .unwrap();
    store.set_app_state(crate::code_reopen::CODE_OPEN_TABS_KEY, &serde_json::json!({"tabs":[{"conv":"archived-code","executor":"agent:claude","folder":null}],"active":0}).to_string()).unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.read_with(cx, |app, _| {
        assert_eq!(app.shell.tabs.tabs.len(), 1);
        assert_eq!(app.shell.tabs.active.as_deref(), Some(live.as_str()));
        assert!(app.work.code_tabs.is_empty());
        assert!(!app.work.chip_picked.contains_key("archived-work"));
        assert_eq!(app.views["archived-code"].convo.messages().len(), 1);
    });
    entity.update(cx, |app, _| app.persist = None);
    store
        .delete_app_states(&[crate::workshop::TABS_KEY.into()])
        .unwrap();
    let legacy = cx.new(AwiliApp::new);
    legacy.read_with(cx, |app, _| {
        assert!(!app
            .work
            .code_tabs
            .iter()
            .any(|s| s.history.conv_id.as_deref() == Some("archived-code")));
        assert!(!serde_json::to_string(&app.workshop_snapshot())
            .unwrap()
            .contains("archived-"));
    });
}

#[gpui::test]
fn workshop_history_escape_precedes_focused_composer_cancel(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_history_escape_precedes_focused_composer_cancel") {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    // Narrower than a docked Conversations panel needs beside the composer: it
    // opens as a sheet, which is what Escape dismisses.
    window.simulate_resize(gpui::size(gpui::px(1000.), gpui::px(800.)));
    window.run_until_parked();
    for kind in [
        awili_core::ConvKind::Work,
        awili_core::ConvKind::Work,
        awili_core::ConvKind::Code,
    ] {
        view.update_in(window, |app, window, cx| {
            app.persist = None;
            app.onboarding = crate::onboarding::Onboarding::existing();
            let id = used_archive_thread(app, kind);
            app.open_conversation_from_history(&id, cx);
            match kind {
                awili_core::ConvKind::Work => {
                    app.work.running = true;
                    app.work.conv_id = Some(id.clone());
                    app.work
                        .cancel
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                }
                _ => app.work.code_tabs[app.work.code_active].streaming = true,
            }
            app.toggle_conversations(cx);
            assert!(app.shell.conversations);
            app.work.ask_menu =
                crate::ask_mode_ui::AskMenu::Open(if kind == awili_core::ConvKind::Code {
                    crate::ask_mode::AskKey::CodeTab(app.work.code_tabs[app.work.code_active].id)
                } else {
                    crate::ask_mode::AskKey::Work(id)
                });
            app.focus_composer(window, cx);
        });
        window.run_until_parked();
        window.simulate_keystrokes("escape");
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert!(matches!(
                app.work.ask_menu,
                crate::ask_mode_ui::AskMenu::Closed
            ));
            assert!(
                app.shell.conversations,
                "permission menu owns the first Escape"
            );
        });
        window.simulate_keystrokes("escape");
        window.run_until_parked();
        view.update(window, |app, _| {
            assert!(!app.shell.conversations);
            match kind {
                awili_core::ConvKind::Work => {
                    assert!(app.work.running);
                    assert!(!app.work.cancel.load(std::sync::atomic::Ordering::SeqCst));
                    app.work.running = false;
                }
                _ => {
                    assert!(app.work.code_tabs[app.work.code_active].streaming);
                    app.work.code_tabs[app.work.code_active].streaming = false;
                }
            }
        });
    }
}

#[gpui::test]
fn workshop_archive_reader_keeps_background_completion_on_its_owner(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_archive_reader_keeps_background_completion_on_its_owner")
    {
        return;
    }
    cx.executor().allow_parking();
    let folder_a = tempfile::tempdir().unwrap();
    let folder_b = tempfile::tempdir().unwrap();
    std::fs::write(folder_a.path().join("owned-output.txt"), "A's result").unwrap();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    let (a, b, rx, terminal_counter) = view.update(window, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        let a = used_archive_thread(app, awili_core::ConvKind::Work);
        let b = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.get_mut(&a).unwrap().cwd = Some(folder_a.path().display().to_string());
        app.conversations.get_mut(&b).unwrap().cwd = Some(folder_b.path().display().to_string());
        app.conversations.archive(&b);
        app.work.chip_picked.insert(
            b.clone(),
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Codex,
                model: None,
            },
        );
        app.open_conversation_from_history(&a, cx);
        app.work.executor = crate::work::WorkExecutor::InApp;
        app.work.folder = Some(folder_a.path().to_path_buf());
        app.work.folder_snapshot.clear();
        app.work.conv_id = Some(a.clone());
        app.work.worker_conv = Some(a.clone());
        app.work.worker_identity = Some(crate::work_run_identity::WorkRunIdentity::OwnedEngine {
            model: "test-owned".into(), engine_id: None,
        });
        let (tx, rx) = std::sync::mpsc::channel();
        app.work.gateway_cmd_tx = Some(tx);
        app.work.session_id = Some("keep-owner-session".into());
        app.work.running = true;
        app.work.panel.output = Some("A's completed answer".into());
        app.work.panel.status = Some(awili_gateway::run_events::RunStatus::Completed);
        app.work.touched_files = vec!["owned-output.txt".into()];
        let counter = app.work.next_terminal_id;
        app.open_conversation_from_history(&b, cx);
        assert!(
            !app.work.retire_after_run,
            "reading an archive retains the live owner's session"
        );
        app.apply_work_update(crate::work::WorkUpdate::TurnComplete, cx);
        (a, b, rx, counter)
    });
    window.run_until_parked();
    view.update(window, |app, cx| {
        assert!(app.shell.archived_reader.is_some());
        assert_eq!(app.sidebar.active_id(), b);
        assert!(!app.work.running, "actual completion still settles A");
        assert_eq!(app.work.conv_id.as_deref(), Some(a.as_str()));
        assert_eq!(app.work.executor, crate::work::WorkExecutor::InApp);
        assert_eq!(app.work.folder.as_deref(), Some(folder_a.path()));
        assert_eq!(app.work.session_id.as_deref(), Some("keep-owner-session"));
        assert!(app.work.gateway_cmd_tx.is_some());
        assert!(matches!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        assert_eq!(
            app.work.panel.output.as_deref(),
            Some("A's completed answer")
        );
        assert_eq!(app.work.outputs, ["owned-output.txt"]);
        assert_eq!(app.work.touched_files, ["owned-output.txt"]);
        assert_eq!(
            app.work.next_terminal_id, terminal_counter,
            "reader never constructs B's terminal"
        );
        assert_eq!(app.views[&b].convo.messages().len(), 2);
        app.close_workshop_archive(cx);
        assert_eq!(app.sidebar.active_id(), a);
        assert_eq!(app.work.folder.as_deref(), Some(folder_a.path()));
        assert_eq!(app.work.session_id.as_deref(), Some("keep-owner-session"));
    });
    // Also exercise a folder settle scheduled before the reader opens: the
    // deferred callback must check the reader at execution time.
    view.update(window, |app, cx| {
        let c = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.get_mut(&c).unwrap().cwd = Some(folder_b.path().display().to_string());
        app.set_active_conversation(&c);
        app.settle_work_folder(cx);
        app.open_conversation_from_history(&b, cx);
    });
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(app.work.folder.as_deref(), Some(folder_a.path()));
        assert_eq!(app.work.next_terminal_id, terminal_counter);
        assert_eq!(app.work.outputs, ["owned-output.txt"]);
    });
}

#[gpui::test]
fn workshop_archived_search_and_image_paste_preserve_the_prior_draft(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_archived_search_and_image_paste_preserve_the_prior_draft",
    ) {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    for (id, kind) in [("reader-live", "chat"), ("reader-archived", "code")] {
        store
            .create_conversation_with_kind(id, id, 1, kind)
            .unwrap();
        store.append_message(id, "user", "Stored text", 2).unwrap();
    }
    store.set_archived("reader-archived", true).unwrap();
    cx.executor().allow_parking();
    cx.update(|cx| cx.bind_keys(crate::app_lifecycle::standard_bindings()));
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.open_conversation_from_history("reader-live", cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Keep this draft", cx));
        app.focus_composer(window, cx);
        app.toggle_search(window, cx);
        app.search.query = "Stored".into();
        app.open_search_result("reader-archived", window, cx);
        assert!(!app.search.open);
        assert!(app.shell.archived_reader.is_some());
        assert!(!app.composer.read(cx).focus_handle().is_focused(window));
    });
    window.run_until_parked();
    assert!(window.debug_bounds("awili-archive-readonly").is_some());
    let png = gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        b"\x89PNG\r\n\x1a\narchive-test".to_vec(),
    );
    view.update_in(window, |_, _, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_image(&png))
    });
    window.dispatch_action(crate::native_menu::Paste);
    window.simulate_keystrokes("cmd-v");
    window.run_until_parked();
    view.update_in(window, |app, window, cx| {
        assert!(!app.composer.read(cx).focus_handle().is_focused(window));
        // An already queued composer paste callback cannot stage either.
        assert!(!app.attach_from_clipboard(cx));
        app.attach_bytes("queued.png", "image/png", b"queued", cx);
        assert!(app.staged_attachments.is_empty());
        assert!(store
            .get_staged_attachments("reader-archived")
            .unwrap()
            .is_empty());
        assert_eq!(app.composer.read(cx).text(), "Keep this draft");
        assert_eq!(app.views["reader-archived"].convo.messages().len(), 1);
        app.close_workshop_archive(cx);
        assert_eq!(app.sidebar.active_id(), "reader-live");
        assert_eq!(app.composer.read(cx).text(), "Keep this draft");
    });
}

#[gpui::test]
fn workshop_delete_prior_native_owner_while_reading_never_restores_an_orphan(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_delete_prior_native_owner_while_reading_never_restores_an_orphan",
    ) {
        return;
    }
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let a = used_archive_thread(app, awili_core::ConvKind::Code);
        let b = used_archive_thread(app, awili_core::ConvKind::Work);
        let c = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.archive(&b);
        app.open_conversation_from_history(&c, cx);
        app.open_conversation_from_history(&a, cx);
        let original = app.shell.tabs.active.clone().unwrap();
        let runtime = app.work.code_tabs[app.work.code_active].id;
        app.open_conversation_from_history(&b, cx);
        // Opening another archive preserves the first return owner.
        let d = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.archive(&d);
        app.open_conversation_from_history(&d, cx);
        app.close_workshop_archive(cx);
        assert_eq!(app.shell.tabs.active.as_deref(), Some(original.as_str()));
        assert_eq!(app.sidebar.active_id(), a);
        app.open_conversation_from_history(&d, cx);
        app.delete_conversation(&a, window, cx);
        assert!(
            app.shell.archived_reader.is_some(),
            "removing a Back target preserves the current reader"
        );
        assert_eq!(app.sidebar.active_id(), d);
        app.close_workshop_archive(cx);
        assert!(app.conversations.get(&a).is_none());
        assert!(!app.work.code_tabs.iter().any(|s| s.id == runtime));
        assert!(!app.shell.tabs.tabs.iter().any(|tab| tab.id == original));
        for _ in 0..3 {
            app.sync_workshop_tabs();
        }
        assert!(!serde_json::to_string(&app.workshop_snapshot())
            .unwrap()
            .contains(&a));
        let selected = app.conversations.get(app.sidebar.active_id()).unwrap();
        assert!(!selected.archived);
        assert!(app.shell.tabs.active.as_ref().is_some_and(|id| app
            .shell
            .tabs
            .tabs
            .iter()
            .any(|t| &t.id == id)));
        if app.mode == Mode::Code {
            let selected = &app.work.code_tabs[app.work.code_active];
            assert!(selected
                .history
                .conv_id
                .as_ref()
                .is_none_or(|id| app.conversations.get(id).is_some_and(|c| !c.archived)));
        }
        app.composer
            .update(cx, |c, cx| c.set_text("Usable surviving draft", cx));
        assert_eq!(app.composer.read(cx).text(), "Usable surviving draft");
    });
}

#[gpui::test]
fn workshop_removing_the_sole_native_owner_from_a_reader_creates_one_fallback(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_tests::workshop_removing_the_sole_native_owner_from_a_reader_creates_one_fallback") {
        return;
    }
    for delete in [false, true] {
        let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
        view.update_in(window, |app, window, cx| {
            app.persist = None;
            app.onboarding = crate::onboarding::Onboarding::existing();
            let a = used_archive_thread(app, awili_core::ConvKind::Code);
            let b = used_archive_thread(app, awili_core::ConvKind::Work);
            app.conversations.archive(&b);
            app.open_conversation_from_history(&a, cx);
            let selected = app.shell.tabs.active.clone().unwrap();
            let others = app
                .shell
                .tabs
                .tabs
                .iter()
                .filter(|t| t.id != selected)
                .map(|t| t.id.clone())
                .collect::<Vec<_>>();
            for id in others {
                app.close_workshop_tab(&id, cx);
            }
            assert_eq!(app.shell.tabs.tabs.len(), 1);
            app.open_conversation_from_history(&b, cx);
            if delete {
                app.delete_conversation(&a, window, cx);
            } else {
                app.archive_conversation(&a, window, cx);
            }
            assert!(app.shell.archived_reader.is_some());
            assert_eq!(app.sidebar.active_id(), b);
            app.close_workshop_archive(cx);
            assert_eq!(
                app.shell.tabs.tabs.len(),
                1,
                "one fallback when delete={delete}"
            );
            assert!(app.work.code_tabs.is_empty());
            assert!(app.shell.archived_reader.is_none());
            let active = app.conversations.get(app.sidebar.active_id()).unwrap();
            assert!(!active.archived);
            assert!(app.active_view().unwrap().convo.messages().is_empty());
            assert!(!serde_json::to_string(&app.workshop_snapshot())
                .unwrap()
                .contains(&a));
        });
    }
}

#[gpui::test]
fn workshop_archive_reader_retires_voice_results_and_disarms_hidden_input(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_archive_reader_retires_voice_results_and_disarms_hidden_input",
    ) {
        return;
    }
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    for hands_free in [false, true] {
        view.update(window, |app, cx| {
            app.persist = None;
            app.onboarding = crate::onboarding::Onboarding::existing();
            let a = used_archive_thread(app, awili_core::ConvKind::Work);
            let b = used_archive_thread(app, awili_core::ConvKind::Work);
            app.conversations.archive(&b);
            app.open_conversation_from_history(&a, cx);
            app.composer
                .update(cx, |c, cx| c.set_text("Typed draft stays", cx));
            app.voice.dictating = !hands_free;
            app.voice.conversation_mode = hands_free;
            app.voice.first_send_confirmed = hands_free;
            app.voice.ptt_session = !hands_free;
            app.voice.ptt_held = !hands_free;
            app.voice
                .stop_partials
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let session = app.voice.session.begin();
            let generation = match app.voice.ptt_guard.modifier(true) {
                crate::ptt_guard::PttAction::ArmTimer(g) => g,
                action => panic!("{action:?}"),
            };
            app.open_conversation_from_history(&b, cx);
            assert!(!app.voice.dictating && !app.voice.conversation_mode);
            assert!(!app.voice.ptt_session && !app.voice.ptt_held);
            assert!(app.voice.capture.is_none() && app.voice.conv_mic.is_none());
            assert!(app.voice.conv_capture.is_none());
            assert!(!app.voice.session.is_current(session));
            if !hands_free {
                assert!(app
                    .voice
                    .stop_partials
                    .load(std::sync::atomic::Ordering::SeqCst));
            }
            app.apply_dictation_final(Some("Late dictation".into()), session, cx);
            app.on_voice_transcript(Some("Late hands-free send".into()), session, cx);
            app.on_voice_utterance(vec![0.0; 10_000], session, cx);
            app.voice.cfg.ptt_key = awili_voice::PttKey::Option;
            app.on_modifiers(
                &gpui::ModifiersChangedEvent {
                    modifiers: gpui::Modifiers {
                        alt: true,
                        ..Default::default()
                    },
                    capslock: gpui::Capslock { on: false },
                },
                cx,
            );
            app.ptt_timer(generation, cx);
            app.start_dictation(cx);
            app.toggle_voice_mode(cx);
            app.start_voice_listening(cx);
            assert!(!app.voice.dictating && !app.voice.conversation_mode);
            assert!(!app.voice.ptt_session && !app.voice.ptt_held);
            assert!(!app.voice.mic_permission_pending);
            assert_eq!(app.composer.read(cx).text(), "Typed draft stays");
            assert_eq!(app.views[&b].convo.messages().len(), 2);
            app.close_workshop_archive(cx);
            // Queued callbacks remain stale after Back; a genuinely new final
            // still applies through the production worker-result reducer.
            app.apply_dictation_final(Some("Still late".into()), session, cx);
            app.ptt_timer(generation, cx);
            assert!(!app.voice.ptt_held);
            assert_eq!(app.composer.read(cx).text(), "Typed draft stays");
            let fresh = app.voice.session.begin();
            app.apply_dictation_final(Some("fresh dictation".into()), fresh, cx);
            assert!(app.composer.read(cx).text().contains("fresh dictation"));
        });
    }
}

#[gpui::test]
fn workshop_reader_buttons_return_focus_to_the_visible_composer(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_reader_buttons_return_focus_to_the_visible_composer") {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    let archived = view.update(window, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        let a = used_archive_thread(app, awili_core::ConvKind::Work);
        let b = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.archive(&b);
        app.open_conversation_from_history(&a, cx);
        app.open_conversation_from_history(&b, cx);
        b
    });
    for (selector, text) in [
        ("ui-button-awili-archive-back", "After Back"),
        ("ui-button-awili-archive-restore", "After Restore"),
    ] {
        window.run_until_parked();
        let bounds = window.debug_bounds(selector).unwrap();
        window.simulate_click(bounds.center(), gpui::Modifiers::default());
        window.run_until_parked();
        view.update_in(window, |app, window, cx| {
            assert!(app.shell.archived_reader.is_none());
            assert!(app.composer.read(cx).focus_handle().is_focused(window));
        });
        window.simulate_input(text);
        window.run_until_parked();
        view.update(window, |app, cx| {
            assert_eq!(app.composer.read(cx).text(), text);
            if text == "After Back" {
                app.open_conversation_from_history(&archived, cx);
            } else {
                assert_eq!(app.sidebar.active_id(), archived);
                assert!(!app.conversations.get(&archived).unwrap().archived);
            }
        });
    }
}

#[gpui::test]
fn workshop_reader_retires_pending_microphone_permission_after_back(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_reader_retires_pending_microphone_permission_after_back")
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let a = used_archive_thread(app, awili_core::ConvKind::Work);
        let b = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.archive(&b);
        app.open_conversation_from_history(&a, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Keep permission-era draft", cx));
        let pending = app.voice.session.current();
        app.voice.mic_permission_pending = true;
        app.open_conversation_from_history(&b, cx);
        app.close_workshop_archive(cx);
        app.voice.status = Some("Newer status".into());
        app.apply_mic_permission_result(
            Ok(()),
            pending,
            |app, _| app.voice.dictating = true,
            |app, _| app.voice.ptt_held = true,
            cx,
        );
        assert!(!app.voice.mic_permission_pending);
        assert!(!app.voice.dictating && !app.voice.ptt_held);
        assert_eq!(app.voice.status.as_deref(), Some("Newer status"));
        app.voice.mic_permission_pending = true;
        app.apply_mic_permission_result(
            Err("Old denial".into()),
            pending,
            |app, _| app.voice.dictating = true,
            |app, _| app.voice.ptt_held = true,
            cx,
        );
        assert!(!app.voice.mic_permission_pending && !app.voice.ptt_held);
        assert_eq!(app.voice.status.as_deref(), Some("Newer status"));
        assert_eq!(app.composer.read(cx).text(), "Keep permission-era draft");
        // A current result still reaches the same production retry callback.
        let current = app.voice.session.current();
        app.voice.mic_permission_pending = true;
        app.apply_mic_permission_result(
            Ok(()),
            current,
            |app, _| app.voice.status = Some("Current retry applied".into()),
            |_, _| panic!("success is not denial"),
            cx,
        );
        assert!(!app.voice.mic_permission_pending);
        assert_eq!(app.voice.status.as_deref(), Some("Current retry applied"));
    });
}

#[gpui::test]
fn workshop_history_escape_preserves_inline_rename_and_edit_cancellation(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_history_escape_preserves_inline_rename_and_edit_cancellation",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    // Narrower than a docked Conversations panel needs beside the composer: it
    // opens as a sheet, which is what Escape dismisses.
    window.simulate_resize(gpui::size(gpui::px(1000.), gpui::px(800.)));
    window.run_until_parked();
    let (id, title) = view.update_in(window, |app, _window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let id = used_archive_thread(app, awili_core::ConvKind::Work);
        app.open_conversation_from_history(&id, cx);
        app.toggle_conversations(cx);
        let title = app.conversations.get(&id).unwrap().title.clone();
        (id, title)
    });
    window.run_until_parked();
    view.update_in(window, |app, window, cx| {
        app.begin_rename(&id, window, cx);
        app.rename_editor
            .update(cx, |c, cx| c.set_text("Do not rename", cx));
    });
    window.run_until_parked();
    view.update_in(window, |app, window, cx| {
        assert!(app.rename_editor.read(cx).focus_handle().is_focused(window));
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.update_in(window, |app, _, _| {
        assert!(app.sidebar.renaming.is_none());
        assert!(app.shell.conversations);
        assert_eq!(app.conversations.get(&id).unwrap().title, title);
    });
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.shell.conversations);
        assert_eq!(app.views[&id].convo.messages().len(), 2);
        assert_eq!(
            app.views[&id].convo.message(0).unwrap().text,
            "Keep this question"
        );
    });
}

#[gpui::test]
fn workshop_reader_back_to_blank_native_restores_a_live_history_selection(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_reader_back_to_blank_native_restores_a_live_history_selection",
    ) {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    for id in ["blank-return-live", "blank-return-archive"] {
        store
            .create_conversation_with_kind(id, id, 1, "chat")
            .unwrap();
    }
    store.set_archived("blank-return-archive", true).unwrap();
    store
        .create_conversation_with_kind("blank-return-council", "Council", 100, "council")
        .unwrap();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.open_conversation_from_history("blank-return-live", cx);
        app.new_workshop_with(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None,
            },
            cx,
        );
        let selected = app.shell.tabs.active.clone();
        let runtime = app.work.code_tabs[app.work.code_active].id;
        assert!(app.work.code_tabs[app.work.code_active]
            .history
            .conv_id
            .is_none());
        app.open_conversation_from_history("blank-return-archive", cx);
        app.close_workshop_archive(cx);
        assert_eq!(app.mode, Mode::Code);
        assert_eq!(app.shell.tabs.active, selected);
        assert_eq!(app.work.code_tabs[app.work.code_active].id, runtime);
        assert_eq!(app.sidebar.active_id(), "blank-return-live");
        app.attach_bytes(
            "after-back.txt",
            "text/plain",
            b"Preserve the archived row",
            cx,
        );
        assert!(store
            .get_staged_attachments("blank-return-archive")
            .unwrap()
            .is_empty());
        let input_owner = app.sidebar.active_id().to_string();
        assert_ne!(input_owner, "blank-return-live");
        assert_eq!(store.get_staged_attachments(&input_owner).unwrap().len(), 1);
        assert!(store
            .get_staged_attachments("blank-return-live")
            .unwrap()
            .is_empty());
        assert_eq!(
            app.conversations.get(&input_owner).unwrap().kind,
            awili_core::ConvKind::Code
        );
        assert_eq!(
            app.work.code_tabs[app.work.code_active]
                .history
                .conv_id
                .as_deref(),
            Some(input_owner.as_str())
        );
        assert_eq!(app.shell.tabs.active, selected);
        assert_eq!(app.work.code_tabs[app.work.code_active].id, runtime);
        app.open_conversation_from_history("blank-return-archive", cx);
        app.close_workshop_archive(cx);
        assert_eq!(app.sidebar.active_id(), input_owner);
        assert_eq!(app.staged_attachments.len(), 1);
        assert_eq!(app.shell.tabs.active, selected);
        // Start another unbound runtime for the deleted-return-target case.
        app.new_workshop_with(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Codex,
                model: None,
            },
            cx,
        );
        let selected = app.shell.tabs.active.clone();
        let runtime = app.work.code_tabs[app.work.code_active].id;
        // Remove every live General row while the stored reader is open.
        // The blank native runtime survives and Back must mint one valid
        // selection rather than retain the archived row as a staging owner.
        app.open_conversation_from_history("blank-return-archive", cx);
        let live_ids: Vec<_> = app
            .conversations
            .active()
            .iter()
            .filter(|c| c.kind != awili_core::ConvKind::Council)
            .map(|c| c.id.clone())
            .collect();
        for id in live_ids {
            app.delete_conversation(&id, window, cx);
        }
        assert!(app
            .conversations
            .active()
            .iter()
            .all(|c| c.kind == awili_core::ConvKind::Council));
        assert!(app.shell.archived_reader.is_some());
        assert_eq!(app.sidebar.active_id(), "blank-return-archive");
        app.close_workshop_archive(cx);
        assert_eq!(app.mode, Mode::Code);
        assert_eq!(app.shell.tabs.active, selected);
        assert_eq!(app.work.code_tabs[app.work.code_active].id, runtime);
        assert_eq!(app.conversations.active().len(), 2);
        let fallback = app.sidebar.active_id().to_string();
        assert_ne!(fallback, "blank-return-archive");
        assert!(!app.conversations.get(&fallback).unwrap().archived);
        assert!(
            app.staged_attachments.is_empty(),
            "a fresh fallback cannot retain the removed owner's tray"
        );
        assert_eq!(
            app.conversations.get(&fallback).unwrap().kind,
            awili_core::ConvKind::Work
        );
        assert!(app.views.contains_key(&fallback));
        // Persistence is intentionally disabled in this reducer fixture; save
        // the minted metadata explicitly before exercising the real staging DB.
        let meta = app.conversations.get(&fallback).unwrap();
        store
            .create_conversation_with_kind(
                &meta.id,
                &meta.title,
                meta.created_ts,
                meta.kind.as_str(),
            )
            .unwrap();
        app.attach_bytes("fallback.txt", "text/plain", b"Live fallback", cx);
        let native_owner = app.sidebar.active_id().to_string();
        assert_ne!(native_owner, fallback);
        assert!(store.get_staged_attachments(&fallback).unwrap().is_empty());
        assert_eq!(
            store.get_staged_attachments(&native_owner).unwrap().len(),
            1
        );
        assert_eq!(
            app.conversations.get(&native_owner).unwrap().kind,
            awili_core::ConvKind::Code
        );
        assert!(store
            .get_staged_attachments("blank-return-archive")
            .unwrap()
            .is_empty());
        app.delete_conversation("blank-return-archive", window, cx);
        assert_eq!(app.mode, Mode::Code);
        assert_eq!(app.shell.tabs.active, selected);
        assert_eq!(app.work.code_tabs[app.work.code_active].id, runtime);
    });
}

#[gpui::test]
fn workshop_archiving_the_return_target_preserves_the_current_reader(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_archiving_the_return_target_preserves_the_current_reader",
    ) {
        return;
    }
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let c = used_archive_thread(app, awili_core::ConvKind::Work);
        let a = used_archive_thread(app, awili_core::ConvKind::Code);
        let b = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.archive(&b);
        app.open_conversation_from_history(&c, cx);
        app.open_conversation_from_history(&a, cx);
        app.open_conversation_from_history(&b, cx);
        app.archive_conversation(&a, window, cx);
        assert!(app.conversations.get(&a).unwrap().archived);
        assert_eq!(app.sidebar.active_id(), b);
        assert!(app.shell.archived_reader.is_some());
        assert!(!app
            .work
            .code_tabs
            .iter()
            .any(|s| s.history.conv_id.as_deref() == Some(&a)));
        app.close_workshop_archive(cx);
        assert_eq!(app.sidebar.active_id(), c);
    });
}

#[gpui::test]
fn workshop_reader_back_refreshes_attachments_after_return_owner_deletion(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_reader_back_refreshes_attachments_after_return_owner_deletion",
    ) {
        return;
    }
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    for id in ["tray-a", "tray-b", "tray-reader"] {
        store
            .create_conversation_with_kind(id, id, 1, "chat")
            .unwrap();
    }
    store.set_archived("tray-reader", true).unwrap();
    let (persist, worker) = crate::persistence::Persistence::spawn_worker(
        awili_persist::Store::open_default().unwrap(),
    );
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let other: Vec<_> = app
            .conversations
            .active()
            .iter()
            .filter(|c| c.id != "tray-a" && c.id != "tray-b")
            .map(|c| c.id.clone())
            .collect();
        for id in other {
            app.conversations.remove(&id);
        }
        app.sync_workshop_tabs();
        app.open_conversation_from_history("tray-b", cx);
        app.open_conversation_from_history("tray-a", cx);
        app.attach_bytes(
            "private-a.txt",
            "text/plain",
            b"A's file must not move to B",
            cx,
        );
        assert_eq!(app.staged_attachments.len(), 1);
        app.persist = Some(persist);
        app.save_active_conversation();
        app.open_conversation_from_history("tray-reader", cx);
        app.delete_conversation("tray-a", window, cx);
        assert!(app.shell.archived_reader.is_some());
        app.close_workshop_archive(cx);
        assert_eq!(app.sidebar.active_id(), "tray-b");
        assert!(
            app.staged_attachments.is_empty(),
            "B cannot inherit A's tray"
        );
        app.attach_bytes("own-b.txt", "text/plain", b"B owns this file", cx);
        assert_eq!(app.staged_attachments.len(), 1);
        assert_eq!(store.get_staged_attachments("tray-b").unwrap().len(), 1);
        assert!(store
            .get_staged_attachments("tray-reader")
            .unwrap()
            .is_empty());
        app.persist
            .as_ref()
            .unwrap()
            .set_state("tray-back-written", "1".into());
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while store.get_app_state("tray-back-written").unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "Back persistence did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        store
            .get_app_state(crate::persistence::ACTIVE_CONVERSATION_KEY)
            .unwrap()
            .as_deref(),
        Some("tray-b")
    );
    view.update(window, |app, _| app.persist = None);
    worker.join().unwrap();
}

#[gpui::test]
fn workshop_archive_delete_keeps_the_selected_blank_native_neighbor(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_archive_delete_keeps_the_selected_blank_native_neighbor")
    {
        return;
    }
    for delete in [false, true] {
        let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
        view.update_in(window, |app, window, cx| {
            app.persist = None;
            app.onboarding = crate::onboarding::Onboarding::existing();
            let z = used_archive_thread(app, awili_core::ConvKind::Work);
            let a = used_archive_thread(app, awili_core::ConvKind::Code);
            app.open_conversation_from_history(&a, cx);
            let a_tab = app.shell.tabs.active.clone().unwrap();
            app.new_workshop_with(
                crate::work::WorkExecutor::Agent {
                    engine: awili_code::CodeEngine::Codex,
                    model: None,
                },
                cx,
            );
            let blank = app.shell.tabs.active.clone().unwrap();
            let runtime = app.work.code_tabs[app.work.code_active].id;
            app.shell
                .tabs
                .tabs
                .retain(|t| t.id == a_tab || t.id == blank);
            app.shell.tabs.tabs.sort_by_key(|t| t.id == a_tab);
            app.select_workshop_tab(&a_tab, cx);
            if delete {
                app.delete_conversation(&a, window, cx);
            } else {
                app.archive_conversation(&a, window, cx);
            }
            assert_eq!(app.mode, Mode::Code);
            assert_eq!(app.shell.tabs.active.as_deref(), Some(blank.as_str()));
            assert_eq!(app.work.code_tabs[app.work.code_active].id, runtime);
            assert!(app.work.code_tabs[app.work.code_active]
                .history
                .conv_id
                .is_none());
            assert_eq!(
                app.shell.tabs.tabs.len(),
                1,
                "no closed history row opens implicitly"
            );
            assert!(!app
                .shell
                .tabs
                .tabs
                .iter()
                .any(|t| t.backing == crate::workshop::Backing::Conversation(z.clone())));
            assert!(
                !app.conversations
                    .get(app.sidebar.active_id())
                    .unwrap()
                    .archived
            );
        });
    }
}

#[gpui::test]
fn workshop_restore_with_no_surviving_tab_opens_only_the_requested_thread(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_restore_with_no_surviving_tab_opens_only_the_requested_thread",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    let b = view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let a = used_archive_thread(app, awili_core::ConvKind::Code);
        let b = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.archive(&b);
        app.open_conversation_from_history(&a, cx);
        let selected = app.shell.tabs.active.clone().unwrap();
        let others: Vec<_> = app
            .shell
            .tabs
            .tabs
            .iter()
            .filter(|t| t.id != selected)
            .map(|t| t.id.clone())
            .collect();
        for id in others {
            app.close_workshop_tab(&id, cx);
        }
        app.open_conversation_from_history(&b, cx);
        app.delete_conversation(&a, window, cx);
        assert!(app.shell.archived_reader.is_some());
        assert!(app.shell.tabs.tabs.is_empty());
        b
    });
    window.run_until_parked();
    let restore = window
        .debug_bounds("ui-button-awili-archive-restore")
        .unwrap();
    window.simulate_click(restore.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.update_in(window, |app, window, cx| {
        assert!(app.shell.archived_reader.is_none());
        assert_eq!(app.shell.tabs.tabs.len(), 1);
        assert_eq!(
            app.shell.tabs.tabs[0].backing,
            crate::workshop::Backing::Conversation(b.clone())
        );
        assert_eq!(app.sidebar.active_id(), b);
        assert!(app.composer.read(cx).focus_handle().is_focused(window));
    });
}

#[gpui::test]
fn workshop_confirmed_general_delete_does_not_show_a_close_refusal(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_confirmed_general_delete_does_not_show_a_close_refusal")
    {
        return;
    }
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let a = used_archive_thread(app, awili_core::ConvKind::Work);
        app.open_conversation_from_history(&a, cx);
        app.work.running = true;
        app.work.conv_id = Some(a.clone());
        app.toast.msg = None;
        app.delete_conversation(&a, window, cx);
        assert!(app.conversations.get(&a).is_none());
        assert!(app
            .toast
            .msg
            .as_ref()
            .is_none_or(|msg| !msg.contains("before closing")));
        assert!(
            app.work.running,
            "confirmed delete preserves pinned completion lifecycle"
        );
    });
}

#[gpui::test]
fn workshop_restore_refreshes_the_requested_owners_files_and_saved_selection(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_restore_refreshes_the_requested_owners_files_and_saved_selection",
    ) {
        return;
    }
    cx.executor().allow_parking();
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    let store = awili_persist::Store::open_default().unwrap();
    // A thread the removed Chat wrote (stored kind `chat`), a Work thread and
    // a native one: each is its own stored conversation.
    for kind in [
        awili_core::ConvKind::Chat,
        awili_core::ConvKind::Work,
        awili_core::ConvKind::Code,
    ] {
        let a = format!("restore-files-a-{}", kind.as_str());
        let b = format!("restore-files-b-{}", kind.as_str());
        store
            .create_conversation_with_kind(&a, &a, 1, "chat")
            .unwrap();
        store
            .create_conversation_with_kind(&b, &b, 2, kind.as_str())
            .unwrap();
        store
            .append_message(&b, "user", "Keep B's transcript", 2)
            .unwrap();
        store
            .append_message(&b, "assistant", "Keep B's answer", 3)
            .unwrap();
        store.set_archived(&b, true).unwrap();
        let own_file = store
            .store_attachment("own-b.txt", "text/plain", b"B's own file")
            .unwrap();
        store.stage_attachment(&b, own_file).unwrap();
        let (persist, worker) = crate::persistence::Persistence::spawn_worker(
            awili_persist::Store::open_default().unwrap(),
        );
        let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
        view.update(window, |app, cx| {
            app.persist = None;
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.open_conversation_from_history(&a, cx);
            app.attach_bytes(
                "private-a.txt",
                "text/plain",
                b"Do not carry A's file into B",
                cx,
            );
            assert_eq!(app.staged_attachments.len(), 1);
            app.persist = Some(persist);
            app.save_active_conversation();
            app.open_conversation_from_history(&b, cx);
            assert_eq!(app.staged_attachments[0].filename, "private-a.txt");
        });
        window.run_until_parked();
        let restore = window
            .debug_bounds("ui-button-awili-archive-restore")
            .unwrap();
        window.simulate_click(restore.center(), gpui::Modifiers::default());
        window.run_until_parked();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            window.run_until_parked();
            let ready = view.read_with(window, |app, _| {
                app.staged_attachments.len() == 1 && app.staged_attachments[0].id == own_file
            });
            if ready
                && store
                    .get_app_state(crate::persistence::ACTIVE_CONVERSATION_KEY)
                    .unwrap()
                    .as_deref()
                    == Some(b.as_str())
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Restore did not commit {kind:?}'s owner"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        view.update_in(window, |app, window, cx| {
            assert!(app.shell.archived_reader.is_none());
            assert_eq!(app.sidebar.active_id(), b);
            assert_eq!(app.staged_attachments[0].filename, "own-b.txt");
            assert!(app.composer.read(cx).focus_handle().is_focused(window));
            assert_eq!(app.views[&b].convo.messages().len(), 2);
            app.persist = None;
        });
        window.run_until_parked();
        worker.join().unwrap();
    }
}

#[gpui::test]
fn workshop_history_reopen_preserves_a_blank_native_helpers_typed_draft(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_history_reopen_preserves_a_blank_native_helpers_typed_draft",
    ) {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let saved = used_archive_thread(app, awili_core::ConvKind::Code);
        app.new_workshop_with(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Codex,
                model: None,
            },
            cx,
        );
        let blank = app.shell.tabs.active.clone().unwrap();
        let runtime = app.work.code_tabs[app.work.code_active].id;
        app.composer
            .update(cx, |c, cx| c.set_text("Keep my Codex draft", cx));
        app.open_conversation_from_history(&saved, cx);
        // The real render reconciles native activation into the shared UUID
        // registry before painting. This entity fixture drives that same step.
        app.sync_workshop_tabs();
        assert_ne!(app.shell.tabs.active.as_deref(), Some(blank.as_str()));
        assert_eq!(
            app.work.code_tabs[app.work.code_active].engine,
            awili_code::CodeEngine::Claude
        );
        assert_eq!(
            app.work.code_tabs[app.work.code_active]
                .history
                .conv_id
                .as_deref(),
            Some(saved.as_str())
        );
        let old = app.work.code_tabs.iter().find(|s| s.id == runtime).unwrap();
        assert_eq!(old.engine, awili_code::CodeEngine::Codex);
        assert!(old.history.conv_id.is_none());
        assert!(old.session_slot.lock().unwrap().is_none());
        app.select_workshop_tab(&blank, cx);
        assert_eq!(app.composer.read(cx).text(), "Keep my Codex draft");
        app.new_code_tab_like_active(cx);
        app.sync_workshop_tabs();
        assert_ne!(app.work.code_tabs[app.work.code_active].id, runtime);
        app.select_workshop_tab(&blank, cx);
        assert_eq!(app.composer.read(cx).text(), "Keep my Codex draft");
    });
}

#[gpui::test]
fn workshop_restore_from_work_or_council_runs_the_normal_chat_mode_setup(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_tests::workshop_restore_from_work_or_council_runs_the_normal_chat_mode_setup",
    ) {
        return;
    }
    cx.executor().allow_parking();
    for mode in [Mode::Work, Mode::Council] {
        let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
        view.update(window, |app, cx| {
            app.persist = None;
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.set_mode(mode, cx);
            assert_ne!(
                app.composer.read(cx).placeholder(),
                crate::theme::COMPOSER_PLACEHOLDER
            );
            let b = used_archive_thread(app, awili_core::ConvKind::Work);
            app.conversations.archive(&b);
            app.open_conversation_from_history(&b, cx);
            assert!(app.shell.archived_reader.is_some());
        });
        window.run_until_parked();
        let restore = window
            .debug_bounds("ui-button-awili-archive-restore")
            .unwrap();
        window.simulate_click(restore.center(), gpui::Modifiers::default());
        window.run_until_parked();
        view.read_with(window, |app, cx| {
            assert!(app.shell.archived_reader.is_none());
            assert_eq!(app.mode, Mode::Work);
            assert_eq!(app.shell.workshop, Mode::Work);
            assert_eq!(app.composer.read(cx).placeholder(), WORKSHOP_PLACEHOLDER);
            assert_eq!(
                app.conversations.get(app.sidebar.active_id()).unwrap().kind,
                awili_core::ConvKind::Work
            );
        });
    }
}

#[gpui::test]
fn workshop_back_from_deleted_work_owner_sets_up_the_surviving_chat(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_tests::workshop_back_from_deleted_work_owner_sets_up_the_surviving_chat")
    {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    let b = view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let b = used_archive_thread(app, awili_core::ConvKind::Work);
        let a = used_archive_thread(app, awili_core::ConvKind::Work);
        let c = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.archive(&c);
        app.open_conversation_from_history(&b, cx);
        app.open_conversation_from_history(&a, cx);
        assert_ne!(
            app.composer.read(cx).placeholder(),
            crate::theme::COMPOSER_PLACEHOLDER
        );
        app.open_conversation_from_history(&c, cx);
        app.delete_conversation(&a, window, cx);
        assert!(app.shell.archived_reader.is_some());
        b
    });
    window.run_until_parked();
    let back = window.debug_bounds("ui-button-awili-archive-back").unwrap();
    window.simulate_click(back.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, cx| {
        assert_eq!(app.mode, Mode::Work);
        assert_eq!(app.sidebar.active_id(), b);
        assert_eq!(app.shell.workshop, Mode::Work);
        assert_eq!(app.composer.read(cx).placeholder(), WORKSHOP_PLACEHOLDER);
    });
}

#[gpui::test]
fn workshop_reader_back_after_council_return_owner_deletion_selects_a_council(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_tests::workshop_reader_back_after_council_return_owner_deletion_selects_a_council") { return; }
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.persist = None;
        app.onboarding = crate::onboarding::Onboarding::existing();
        let survivor = used_archive_thread(app, awili_core::ConvKind::Council);
        let a = used_archive_thread(app, awili_core::ConvKind::Council);
        let b = used_archive_thread(app, awili_core::ConvKind::Work);
        app.conversations.archive(&b);
        app.open_conversation_from_history(&a, cx);
        app.open_conversation_from_history(&b, cx);
        app.delete_conversation(&a, window, cx);
        assert!(app.shell.archived_reader.is_some());
        app.close_workshop_archive(cx);
        assert_eq!(app.mode, Mode::Council);
        assert_eq!(app.sidebar.active_id(), survivor);
        assert_eq!(
            app.conversations.get(app.sidebar.active_id()).unwrap().kind,
            awili_core::ConvKind::Council
        );
        assert_eq!(
            app.composer.read(cx).placeholder(),
            crate::council_view::COMPOSER_PLACEHOLDER
        );
    });
}
