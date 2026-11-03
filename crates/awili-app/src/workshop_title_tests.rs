//! Drive the title menu and its confirmation in actual app windows.
use crate::{consolidation_tests::isolated, work::Mode, AwiliApp};
use gpui::{px, size};

fn click(window: &mut gpui::VisualTestContext, selector: &str) {
    let bounds = window
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .expect(selector);
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
}

fn setup(cx: &mut gpui::TestAppContext) -> (gpui::Entity<AwiliApp>, &mut gpui::VisualTestContext) {
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    cx.executor().allow_parking();
    cx.update(|cx| {
        cx.bind_keys(crate::app_lifecycle::standard_bindings());
        crate::install_native_menu_actions(cx);
    });
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    window.update(|window, cx| {
        cx.set_global(crate::AwiliWindow(
            window.window_handle().downcast::<AwiliApp>().unwrap(),
        ));
    });
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = None;
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.set_mode(Mode::Work, cx);
        app.work.folder = Some("/fixture/title-project".into());
        let owner = app.sidebar.active_id().to_owned();
        app.conversations.get_mut(&owner).unwrap().cwd = Some("/fixture/title-project".into());
        app.work.projects.clear();
    });
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();
    (view, window)
}

fn item(window: &mut gpui::VisualTestContext, index: usize) {
    click(window, &format!("ui-row-awili-workshop-menu-item-{index}"));
}

#[gpui::test]
fn title_projects_save_edit_cancel_and_remove_preserve_folder(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_title_tests::title_projects_save_edit_cancel_and_remove_preserve_folder")
    {
        return;
    }
    let (view, window) = setup(cx);
    assert!(window
        .debug_bounds("ui-chip-awili-workshop-project")
        .is_some());
    view.update(window, |app, cx| {
        app.shell.job_open = true;
        app.shell.job_tab = crate::job_panel::JobTab::Context;
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds("work-save-project").is_none());
    view.update_in(window, |app, window, cx| app.close_job_panel(window, cx));
    window.run_until_parked();
    click(window, "ui-chip-awili-workshop-project");
    item(window, 2); // Save folder as project, after Conversations and Choose.
    assert!(view.read_with(window, |app, _| app.work.projects.len() == 1));
    click(window, "ui-chip-awili-workshop-project");
    item(window, 3); // Edit current saved project.
    assert!(view.read_with(window, |app, _| app
        .project_editor
        .as_ref()
        .is_some_and(|(_, folder)| folder == "/fixture/title-project")));
    view.update(window, |app, cx| app.close_project_editor(cx));
    window.run_until_parked();
    click(window, "ui-chip-awili-workshop-project");
    item(window, 4); // Removing requires a dialog.
    assert!(window
        .debug_bounds("ui-dialog-awili-remove-saved-project-panel")
        .is_some());
    assert_eq!(view.read_with(window, |app, _| app.work.projects.len()), 1);
    let scrim = window.debug_bounds("ui-dialog-scrim").unwrap();
    assert_eq!(scrim.origin, gpui::point(px(0.), px(0.)));
    assert_eq!(scrim.size, size(px(1400.), px(900.)));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.shell.tabs.delete_project.is_none()));
    assert_eq!(view.read_with(window, |app, _| app.work.projects.len()), 1);
    click(window, "ui-chip-awili-workshop-project");
    item(window, 4);
    click(window, "ui-button-awili-remove-saved-project-primary");
    assert_eq!(view.read_with(window, |app, _| app.work.projects.len()), 0);
    assert_eq!(
        view.read_with(window, |app, _| app.work.folder.clone()),
        Some("/fixture/title-project".into())
    );
    assert!(view.read_with(window, |app, _| app.shell.tabs.delete_project.is_none()));
}

#[gpui::test]
fn title_confirmation_owns_shortcuts_and_does_not_stop_job(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_title_tests::title_confirmation_owns_shortcuts_and_does_not_stop_job") {
        return;
    }
    let (view, window) = setup(cx);
    click(window, "ui-chip-awili-workshop-project");
    item(window, 2);
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        app.work.running = true;
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.views
            .get_mut(app.sidebar.active_id())
            .unwrap()
            .convo
            .push(awili_core::Role::User, "Fixture job prompt");
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
    let behind_stop = window
        .debug_bounds("ui-button-job-island-stop")
        .unwrap()
        .center();
    click(window, "ui-chip-awili-workshop-project");
    item(window, 4);
    let owner = view.read_with(window, |app, _| app.sidebar.active_id().to_owned());
    window.simulate_keystrokes("cmd-n cmd-k cmd-, cmd-j");
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_owned()),
        owner
    );
    assert!(!view.read_with(window, |app, _| app.shell.job_open));
    assert!(!view.read_with(window, |app, _| app.command_palette.open));
    assert!(!view.read_with(window, |app, _| app.settings.open));
    window.dispatch_action(crate::native_menu::ShowShortcuts);
    window.run_until_parked();
    assert!(!view.read_with(window, |app, _| app.shortcuts.open));
    view.update_in(window, |app, window, cx| app.toggle_search(window, cx));
    assert!(!view.read_with(window, |app, _| app.search.open));
    assert!(view.read_with(window, |app, _| app.shell.tabs.delete_project.is_some()));
    let generation = view.update(window, |app, cx| {
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
        let arm = app.voice.ptt_guard.modifier(true);
        app.voice.ptt_guard.key_down(); // disarm before any timer can fire
        assert!(
            matches!(arm, crate::ptt_guard::PttAction::ArmTimer(_)),
            "confirmation must not arm held PTT"
        );
        match app.voice.ptt_guard.modifier(true) {
            crate::ptt_guard::PttAction::ArmTimer(generation) => generation,
            _ => panic!("fixture must arm"),
        }
    });
    window.simulate_keystrokes("alt-x");
    window.run_until_parked();
    view.update(window, |app, _| {
        assert_eq!(
            app.voice.ptt_guard.timer(generation),
            crate::ptt_guard::PttAction::None,
            "typing must disarm even inside confirmation"
        )
    });
    window.simulate_click(behind_stop, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.shell.tabs.delete_project.is_some()));
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.work.running));
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    assert_eq!(view.read_with(window, |app, _| app.work.projects.len()), 1);
    // The actual app-level binding must work once confirmation is closed.
    window.simulate_keystrokes("cmd-n");
    window.run_until_parked();
    assert_ne!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_owned()),
        owner
    );
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}

#[gpui::test]
fn title_actions_follow_selected_thread_while_other_job_owns_folder(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_title_tests::title_actions_follow_selected_thread_while_other_job_owns_folder",
    ) {
        return;
    }
    let (view, window) = setup(cx);
    click(window, "ui-chip-awili-workshop-project");
    item(window, 2); // Save A.
    view.update(window, |app, cx| {
        app.work.running = true;
        app.work.conv_id = Some(app.sidebar.active_id().into());
        let other = app.create_conversation(awili_core::ConvKind::Work);
        app.conversations.get_mut(&other).unwrap().cwd = Some("/fixture/thread-b".into());
        app.open_conversation_from_history(&other, cx);
    });
    window.run_until_parked();
    assert_eq!(
        view.read_with(window, |app, _| app.work.folder.clone()),
        Some("/fixture/title-project".into())
    );
    click(window, "ui-chip-awili-workshop-project");
    item(window, 3); // Save B, after Conversations, A and Choose.
    assert!(view.read_with(window, |app, _| app
        .work
        .projects
        .iter()
        .any(|p| p.folder == "/fixture/thread-b")));
    assert_eq!(view.read_with(window, |app, _| app.work.projects.len()), 2);
    click(window, "ui-chip-awili-workshop-project");
    item(window, 4); // Edit B, after two projects.
    assert!(view.read_with(window, |app, _| app
        .project_editor
        .as_ref()
        .is_some_and(|(_, f)| f == "/fixture/thread-b")));
    view.update(window, |app, cx| app.close_project_editor(cx));
    window.run_until_parked();
    click(window, "ui-chip-awili-workshop-project");
    item(window, 5);
    click(window, "ui-button-awili-remove-saved-project-primary");
    assert_eq!(view.read_with(window, |app, _| app.work.projects.len()), 1);
    assert_eq!(
        view.read_with(window, |app, _| app.work.projects[0].folder.clone()),
        "/fixture/title-project"
    );
    assert_eq!(
        view.read_with(window, |app, _| app.work.folder.clone()),
        Some("/fixture/title-project".into())
    );
    assert!(view.read_with(window, |app, _| app.work.running));
    view.update(window, |app, _| app.work.running = false);
}

#[gpui::test]
fn title_manages_stale_saved_project_without_switching_running_folder(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated(
        "workshop_title_tests::title_manages_stale_saved_project_without_switching_running_folder",
    ) {
        return;
    }
    let (view, window) = setup(cx);
    let missing = tempfile::tempdir().unwrap();
    let missing_folder = missing.path().display().to_string();
    drop(missing);
    assert!(!std::path::Path::new(&missing_folder).exists());
    let (tx, rx) = std::sync::mpsc::channel();
    let owner = view.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_owned();
        app.work.running = true;
        app.work.conv_id = Some(owner.clone());
        app.work.agent_abort_tx = Some(tx);
        app.work.projects = vec![awili_persist::StoredProject {
            id: 55,
            name: "Unavailable folder".into(),
            folder: missing_folder.clone(),
            instructions: "Keep these standing instructions".into(),
            created_ts: 1,
        }];
        cx.notify();
        owner
    });
    window.run_until_parked();
    click(window, "ui-chip-awili-workshop-project");
    item(window, 5); // Manage saved projects, after Finder.
    item(window, 1); // The stale row, without selecting its folder.
    item(window, 2); // Edit its captured metadata.
    assert!(view.read_with(window, |app, _| app
        .project_editor
        .as_ref()
        .is_some_and(|(id, folder)| *id == 55 && folder == &missing_folder)));
    view.update(window, |app, cx| app.close_project_editor(cx));
    window.run_until_parked();
    click(window, "ui-chip-awili-workshop-project");
    item(window, 5);
    item(window, 1);
    item(window, 3); // Remove the same saved row, with confirmation.
    assert_eq!(
        view.read_with(window, |app, _| app
            .shell
            .tabs
            .delete_project
            .as_ref()
            .map(|(id, folder, _)| (*id, folder.clone()))),
        Some((55, missing_folder.clone()))
    );
    click(window, "ui-button-awili-remove-saved-project-primary");
    assert!(view.read_with(window, |app, _| app.work.projects.is_empty()));
    assert_eq!(
        view.read_with(window, |app, _| app.sidebar.active_id().to_owned()),
        owner
    );
    assert_eq!(
        view.read_with(window, |app, _| app.work.folder.clone()),
        Some("/fixture/title-project".into())
    );
    assert!(view.read_with(window, |app, _| app.work.running));
    assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    assert!(!std::path::Path::new(&missing_folder).exists());
    view.update(window, |app, _| {
        app.work.running = false;
        app.work.agent_abort_tx = None;
    });
}
