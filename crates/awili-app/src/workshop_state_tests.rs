use super::*;
use gpui::AppContext;

#[gpui::test]
fn snapshot_write_cycle_rejects_old_acknowledgements(cx: &mut gpui::TestAppContext) {
    if !crate::consolidation_tests::isolated(
        "workshop::tests::snapshot_write_cycle_rejects_old_acknowledgements",
    ) {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let (tx, rx) = std::sync::mpsc::channel();
    entity.update(cx, |app, cx| {
        app.persist = Some(crate::Persistence {
            tx,
            failed: Default::default(),
        });
        for raw in ["A", "B", "A"] {
            app.queue_workshop_save(raw.into(), None, cx);
        }
    });
    let replies: Vec<_> = rx
        .try_iter()
        .filter_map(|cmd| match cmd {
            crate::PersistCmd::SaveWorkshopTabs { reply, .. } => Some(reply),
            _ => None,
        })
        .collect();
    assert_eq!(replies.len(), 3);
    replies[0].send_blocking(Ok(())).unwrap();
    replies[1].send_blocking(Ok(())).unwrap();
    replies[2]
        .send_blocking(Err("latest write refused".into()))
        .unwrap();
    cx.run_until_parked();
    entity.read_with(cx, |app, _| {
        assert!(
            app.shell.tabs.saved.is_none(),
            "old identical A acknowledgement cannot claim latest A was saved"
        );
        assert_eq!(
            app.shell.tabs.save_error.as_deref(),
            Some("latest write refused")
        );
        assert!(app.shell.tabs.queued.is_none());
    });
}

#[gpui::test]
fn snapshot_recovers_only_after_worker_ack_and_keeps_raw_backup(cx: &mut gpui::TestAppContext) {
    if !crate::consolidation_tests::isolated(
        "workshop::tests::snapshot_recovers_only_after_worker_ack_and_keeps_raw_backup",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let store = awili_persist::Store::open_default().unwrap();
    store.set_app_state(TABS_KEY, "{unreadable").unwrap();
    drop(store);
    let entity = cx.new(AwiliApp::new);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = entity.update(cx, |app, cx| {
        let worker = app.persist.take().unwrap();
        app.persist = Some(crate::Persistence {
            tx,
            failed: Default::default(),
        });
        app.recover_workshop_tabs(cx);
        assert_eq!(
            app.shell.tabs.unreadable.as_deref(),
            Some("{unreadable"),
            "still blocked before acknowledgement"
        );
        worker
    });
    let command = rx.try_recv().unwrap();
    let backup = match &command {
        crate::PersistCmd::SaveWorkshopTabs {
            recovery: Some((expected, backup)),
            ..
        } => {
            assert_eq!(expected, "{unreadable");
            backup.clone()
        }
        _ => panic!("expected atomic recovery command"),
    };
    worker.send(command);
    let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    loop {
        cx.run_until_parked();
        if entity.read_with(cx, |app, _| app.shell.tabs.unreadable.is_none()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "recovery was not acknowledged"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let raw = entity.read_with(cx, |app, _| app.shell.tabs.saved.clone().unwrap());
    let store = awili_persist::Store::open_default().unwrap();
    assert_eq!(
        store.get_app_state(TABS_KEY).unwrap().as_deref(),
        Some(raw.as_str())
    );
    assert!(SavedWorkshop::parse(&raw).is_ok());
    assert_eq!(
        store.get_app_state(&backup).unwrap().as_deref(),
        Some("{unreadable")
    );
}

#[test]
fn snapshot_rejects_duplicate_identity_and_newer_version() {
    let id = uuid::Uuid::new_v4().to_string();
    let mut saved = SavedWorkshop {
        version: 1,
        tabs: vec![SavedTab {
            id: id.clone(),
            backing: SavedBacking::Conversation("conv".into()),
        }],
        active: Some(id),
        work_helpers: BTreeMap::new(),
    };
    assert!(SavedWorkshop::parse(&serde_json::to_string(&saved).unwrap()).is_ok());
    saved.tabs.push(saved.tabs[0].clone());
    assert!(SavedWorkshop::parse(&serde_json::to_string(&saved).unwrap()).is_err());
    saved.tabs.pop();
    saved.version = 2;
    assert!(SavedWorkshop::parse(&serde_json::to_string(&saved).unwrap()).is_err());
}

#[test]
fn snapshot_collects_only_agent_metas_and_rejects_duplicate_backing() {
    let mut saved = SavedWorkshop {
        work_helpers: BTreeMap::new(),
        version: 1,
        tabs: vec![
            SavedTab {
                id: uuid::Uuid::new_v4().to_string(),
                backing: SavedBacking::Conversation("chat".into()),
            },
            SavedTab {
                id: uuid::Uuid::new_v4().to_string(),
                backing: SavedBacking::Agent(SavedCodeTab {
                    conv: Some("agent".into()),
                    executor: "agent:codex".into(),
                    folder: Some("/repo".into()),
                }),
            },
        ],
        active: None,
    };
    assert_eq!(saved.agent_conversations(), ["agent"]);
    assert!(SavedWorkshop::parse(&serde_json::to_string(&saved).unwrap()).is_ok());
    saved.tabs.push(SavedTab {
        id: uuid::Uuid::new_v4().to_string(),
        backing: SavedBacking::Conversation("agent".into()),
    });
    assert!(SavedWorkshop::parse(&serde_json::to_string(&saved).unwrap()).is_err());
}

#[gpui::test]
fn snapshot_read_failure_blocks_automatic_writes(cx: &mut gpui::TestAppContext) {
    if !crate::consolidation_tests::isolated(
        "workshop::tests::snapshot_read_failure_blocks_automatic_writes",
    ) {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let (tx, rx) = std::sync::mpsc::channel();
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = Some(crate::Persistence {
            tx,
            failed: Default::default(),
        });
        assert!(!app.load_workshop_tabs(&Err("database read refused".into()), &HashMap::new()));
        app.sync_workshop_tabs();
        assert!(!app.shell.tabs.tabs.is_empty());
        app.save_workshop_tabs_if_changed(cx);
        assert!(app.shell.tabs.read_failed);
    });
    assert!(!rx
        .try_iter()
        .any(|cmd| matches!(cmd, crate::PersistCmd::SaveWorkshopTabs { .. })));
}

#[gpui::test]
fn workshop_tabs_compact_keyboard_and_title_geometry(cx: &mut gpui::TestAppContext) {
    if !crate::consolidation_tests::isolated(
        "workshop::tests::workshop_tabs_compact_keyboard_and_title_geometry",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.persist = None;
        for i in 0..8 {
            let id = app.create_conversation(awili_core::ConvKind::Chat);
            app.conversations.get_mut(&id).unwrap().title =
                format!("Thread {i}: {}", "A long conversation title ".repeat(8));
            app.open_conversation_from_history(&id, cx);
        }
    });
    for scale in [1.0, 1.5] {
        crate::theme::set_ui_scale(scale);
        window.simulate_resize(gpui::size(gpui::px(800.), gpui::px(480.)));
        window.run_until_parked();
        let title = window
            .debug_bounds("ui-text-awili-workshop-title-text")
            .unwrap();
        let history = window
            .debug_bounds("ui-chip-awili-workshop-title")
            .unwrap();
        assert!(
            title.left() >= gpui::px(0.) && title.right() <= gpui::px(800.),
            "title fits: {title:?}"
        );
        assert!(
            history.left() >= gpui::px(0.) && history.right() <= gpui::px(800.),
            "the title switcher remains visible: {history:?}"
        );
        assert!(title.size.height >= crate::theme::z(crate::theme::type_::LABEL.line));
        // The Conversations toggle replaces the strip's history entry point
        // and stays on screen beside the title at this compact width.
        let toggle = window
            .debug_bounds("ui-button-awili-workshop-history-toggle")
            .unwrap();
        assert!(
            toggle.left() >= gpui::px(0.) && toggle.right() <= gpui::px(800.),
            "conversations toggle remains visible: {toggle:?}"
        );
        let selected_owner =
            view.read_with(window, |app, _| app.shell.tabs.active.clone().unwrap());
        view.update_in(window, |app, _, cx| {
            let owner = app.sidebar.active_id().to_string();
            let active = app.views.get_mut(&owner).unwrap();
            for _ in 0..12 {
                let index = active.convo.len();
                active.convo.push(
                    crate::Role::Assistant,
                    "Retained answer with enough rows to scroll.",
                );
                active
                    .rows
                    .push(crate::MessageRow::new(active.convo.message(index).unwrap()));
            }
            cx.notify();
        });
        window.run_until_parked();
        view.update_in(window, |app, window, cx| app.focus_composer(window, cx));
        let scroll = view.read_with(window, |app, _| app.active_view().unwrap().scroll.clone());
        scroll.scroll_to_bottom();
        window.update(|window, _| window.refresh());
        window.run_until_parked();
        // The model chip lives in the composer footer now: always on screen, never
        // clipped by the scrolling transcript, so there is nothing to reveal.
        let helper = window
            .debug_bounds("ui-chip-awili-workshop-helper")
            .unwrap();
        let footer = window
            .debug_bounds("ui-composer-frame-awili-composer-footer")
            .unwrap();
        assert!(
            helper.top() >= footer.top() - gpui::px(0.5)
                && helper.bottom() <= footer.bottom() + gpui::px(0.5),
            "the chip sits in the composer footer: {helper:?} in {footer:?}"
        );
        view.read_with(window, |app, _| {
            assert_eq!(
                app.shell.tabs.active.as_deref(),
                Some(selected_owner.as_str())
            );
        });
        window.simulate_click(helper.center(), gpui::Modifiers::default());
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert_eq!(
                app.shell.tabs.active.as_deref(),
                Some(selected_owner.as_str())
            );
            assert!(
                app.shell.composer_menus.model.is_some() && app.shell.tabs.menu.is_none(),
                "pointer click opens the current conversation's chooser"
            );
        });
        window.simulate_keystrokes("escape");
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert!(app.shell.composer_menus.model.is_none());
        });
        window.update(|window, cx| {
            view.read_with(cx, |app, _| {
                assert!(app.shell.tabs.engine_focus.is_focused(window));
            })
        });
        window.simulate_keystrokes("space");
        window.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
        });
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert!(
                app.shell.composer_menus.model.is_some(),
                "Escape restores its trigger focus"
            )
        });
        window.simulate_keystrokes("escape");
        window.run_until_parked();
    }
    crate::theme::set_ui_scale(1.0);
}

#[gpui::test]
fn snapshot_production_dedup_restores_a_after_queued_b_and_retries_failure(
    cx: &mut gpui::TestAppContext,
) {
    if !crate::consolidation_tests::isolated(
        "workshop::tests::snapshot_production_dedup_restores_a_after_queued_b_and_retries_failure",
    ) {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    let (tx, rx) = std::sync::mpsc::channel();
    let a = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = Some(crate::Persistence {
            tx,
            failed: Default::default(),
        });
        let a = app.create_conversation(ConvKind::Chat);
        let b = app.create_conversation(ConvKind::Chat);
        app.open_conversation_from_history(&a, cx);
        app.open_conversation_from_history(&b, cx);
        app.open_conversation_from_history(&a, cx);
        app.save_workshop_tabs_if_changed(cx);
        app.shell.tabs.active.clone().unwrap()
    });
    let replies = rx
        .try_iter()
        .filter_map(|cmd| match cmd {
            crate::PersistCmd::SaveWorkshopTabs { raw, reply, .. } => Some((raw, reply)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), 1);
    replies[0].1.send_blocking(Ok(())).unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        let b = app
            .shell
            .tabs
            .tabs
            .iter()
            .find(|t| t.id != a)
            .unwrap()
            .id
            .clone();
        app.select_workshop_tab(&b, cx);
        app.save_workshop_tabs_if_changed(cx);
        app.select_workshop_tab(&a, cx);
        app.save_workshop_tabs_if_changed(cx);
    });
    let writes = rx
        .try_iter()
        .filter_map(|cmd| match cmd {
            crate::PersistCmd::SaveWorkshopTabs { raw, reply, .. } => Some((raw, reply)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        writes.len(),
        2,
        "queued B cannot suppress restoring A even though A was previously acknowledged"
    );
    assert_eq!(
        SavedWorkshop::parse(&writes[1].0)
            .unwrap()
            .active
            .as_deref(),
        Some(a.as_str())
    );
    writes[0].1.send_blocking(Ok(())).unwrap();
    writes[1]
        .1
        .send_blocking(Err("latest A refused".into()))
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(
            app.shell.tabs.saved.is_none(),
            "earlier acknowledged A is not proof after B was queued"
        );
        assert!(app.shell.tabs.save_error.is_some());
        app.shell.tabs.save_error = None; // the visible Retry action
        app.save_workshop_tabs_if_changed(cx);
    });
    assert!(
        rx.try_iter()
            .any(|cmd| matches!(cmd, crate::PersistCmd::SaveWorkshopTabs { .. })),
        "Retry must actually write A again"
    );
}

/// The right-hand panel lists the open threads with the old strip's live state:
/// the selected row follows a changing title, and a busy thread wears its dot.
#[gpui::test]
fn workshop_live_tab_geometry_reveals_changed_title_and_busy_dot(cx: &mut gpui::TestAppContext) {
    if !crate::consolidation_tests::isolated(
        "workshop::tests::workshop_live_tab_geometry_reveals_changed_title_and_busy_dot",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    let selected = view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.persist = None;
        for _ in 0..8 {
            let id = app.create_conversation(ConvKind::Chat);
            app.open_conversation_from_history(&id, cx);
        }
        let selected = app.sidebar.active_id().to_string();
        app.conversations.get_mut(&selected).unwrap().title = "Short".into();
        selected
    });
    window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(800.)));
    window.run_until_parked();
    let row = Box::leak(
        view.read_with(window, |app, _| {
            format!(
                "ui-row-workshop-{}",
                app.shell.tabs.active.as_ref().unwrap()
            )
        })
        .into_boxed_str(),
    );
    let texts = Box::leak(format!("{row}-texts").into_boxed_str());
    let mut idle_left = None;
    for (title, busy) in [
        (
            "A considerably longer title after the first submitted prompt",
            false,
        ),
        ("Short", false),
        ("Short", true),
    ] {
        view.update(window, |app, cx| {
            app.conversations.get_mut(&selected).unwrap().title = title.into();
            app.work.running = busy;
            app.work.conv_id = busy.then(|| selected.clone());
            cx.notify();
        });
        window.run_until_parked();
        let list = window.debug_bounds("awili-workshop-open").unwrap();
        let bounds = window.debug_bounds(row).unwrap();
        assert!(
            bounds.left() >= list.left() && bounds.right() <= list.right(),
            "the selected row stays inside the open-thread list: {bounds:?} / {list:?}"
        );
        view.read_with(window, |app, _| {
            let backing = app
                .shell
                .tabs
                .tabs
                .iter()
                .find(|tab| Some(&tab.id) == app.shell.tabs.active.as_ref())
                .map(|tab| tab.backing.clone())
                .unwrap();
            assert_eq!(app.workshop_tab_busy(&backing), busy);
        });
        let text_left = window.debug_bounds(texts).unwrap().left();
        if busy {
            assert!(
                text_left > idle_left.unwrap(),
                "a busy thread's row makes room for its dot"
            );
        } else {
            idle_left = Some(text_left);
        }
    }
    view.update(window, |app, cx| {
        app.work.running = false;
        cx.notify();
    });
}
