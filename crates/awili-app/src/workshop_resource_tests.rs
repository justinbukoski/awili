//! Actual menu navigation and ownership in private, networkless app windows.
use super::*;
use crate::consolidation_tests::isolated;
use gpui::{px, size};

fn click(window: &mut gpui::VisualTestContext, index: usize) {
    let selector = Box::leak(format!("ui-row-awili-workshop-menu-item-{index}").into_boxed_str());
    let bounds = window.debug_bounds(selector).expect(selector);
    let panel = window
        .debug_bounds("ui-popover-awili-workshop-menu-panel")
        .unwrap();
    assert!(
        bounds.origin.x >= panel.origin.x
            && bounds.origin.x + bounds.size.width <= panel.origin.x + panel.size.width,
        "menu row {index} must stay inside its visible popover: {bounds:?} / {panel:?}"
    );
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
}

fn session_request(listener: &std::net::TcpListener) -> (std::net::TcpStream, String) {
    use std::io::{Read, Write};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0; 4096];
            let n = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]).to_string();
            if request.starts_with("GET /api/sessions/") {
                return (stream, request);
            }
            assert!(
                request.starts_with("GET / "),
                "unexpected fixture route: {request}"
            );
            let body = "window.__HERMES_SESSION_TOKEN__=\"fixture\"";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session transcript was never requested"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn setup<'a>(
    cx: &'a mut gpui::TestAppContext,
    folder: &std::path::Path,
) -> (gpui::Entity<AwiliApp>, &'a mut gpui::VisualTestContext) {
    awili_config::write_config(
        &awili_config::Config::networkless(),
        &awili_config::Config::config_paths()[0],
    )
    .unwrap();
    cx.executor().allow_parking();
    cx.update(|cx| cx.bind_keys(crate::app_lifecycle::standard_bindings()));
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = None;
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.set_mode(Mode::Work, cx);
        app.work.folder = Some(folder.to_owned());
        let owner = app.sidebar.active_id().to_owned();
        app.conversations.get_mut(&owner).unwrap().cwd = Some(folder.display().to_string());
        app.work.projects.clear();
    });
    window.simulate_resize(size(px(1400.), px(900.)));
    window.run_until_parked();
    (view, window)
}

fn open(view: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext, kind: WorkshopMenu) {
    // Drive the visible project trigger and root menu entry, not the helper.
    let bounds = window
        .debug_bounds("ui-chip-awili-workshop-project")
        .unwrap();
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
    let index = match kind {
        WorkshopMenu::Files => 4,
        WorkshopMenu::Outputs => 5,
        WorkshopMenu::RecentSessions => 6,
        _ => unreachable!(),
    };
    // Enlarged compact menus scroll: bring the root action into view
    // through production keyboard navigation before clicking its real row.
    for _ in 0..index {
        window.simulate_keystrokes("down");
    }
    window.run_until_parked();
    click(window, index);
    assert!(view.read_with(window, |app, _| app.shell.tabs.menu == Some(kind)));
}

#[gpui::test]
fn files_menu_descends_refreshes_and_preserves_running_workspace(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_resources::tests::files_menu_descends_refreshes_and_preserves_running_workspace",
    ) {
        return;
    }
    let fixtures = tempfile::tempdir().unwrap();
    let a = fixtures.path().join("running-a");
    let b = fixtures.path().join("selected-b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(b.join("nested")).unwrap();
    std::fs::write(a.join("a.txt"), "a").unwrap();
    std::fs::write(b.join("b.txt"), "b").unwrap();
    std::fs::write(b.join(".hidden"), "h").unwrap();
    std::fs::write(b.join(".gitignore"), "ignored.txt\n").unwrap();
    std::fs::write(b.join("ignored.txt"), "i").unwrap();
    let (view, window) = setup(cx, &b);
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        let owner = app
            .conversations
            .create_kind(44, awili_core::ConvKind::Work)
            .id;
        app.conversations.get_mut(&owner).unwrap().cwd = Some(a.display().to_string());
        app.work.conv_id = Some(owner);
        app.work.folder = Some(a.clone());
        app.work.workspace_tree = Some(FileTreeState::new(a.clone()));
        app.work.running = true;
        app.work.agent_abort_tx = Some(tx);
        cx.notify();
    });
    window.run_until_parked();
    open(&view, window, WorkshopMenu::Files);
    assert_eq!(
        view.read_with(window, |app, _| app.shell.tabs.resources.root.clone()),
        Some(b.clone())
    );
    assert!(
        !view.read_with(window, |app, _| app
            .shell
            .tabs
            .resources
            .tree
            .as_ref()
            .unwrap()
            .is_watching()),
        "a menu reader must not retain an unconsumed filesystem-event queue"
    );
    assert_eq!(
        view.read_with(window, |app, _| app
            .work
            .workspace_tree
            .as_ref()
            .unwrap()
            .root()
            .to_owned()),
        a
    );
    let visible = view.read_with(window, |app, _| {
        let tree = app.shell.tabs.resources.tree.as_ref().unwrap();
        tree.root_entries()
            .iter()
            .filter(|e| tree.is_visible(e))
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(visible, ["nested", "b.txt"]);
    click(window, 4); // nested directory, after Back, heading, Refresh, Finder.
    assert_eq!(
        view.read_with(window, |app, _| app.shell.tabs.resources.directory.clone()),
        Some(b.join("nested"))
    );
    std::fs::write(b.join("nested/late.txt"), "late").unwrap();
    click(window, 3); // Refresh within nested directory.
    assert!(view.read_with(window, |app, _| app
        .shell
        .tabs
        .resources
        .tree
        .as_ref()
        .unwrap()
        .loaded_entries(&b.join("nested"))
        .iter()
        .any(|e| e.name == "late.txt")));
    click(window, 2); // Parent folder.
    assert_eq!(
        view.read_with(window, |app, _| app.shell.tabs.resources.directory.clone()),
        Some(b.clone())
    );
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(view.read_with(window, |app, _| app.work.running));
    assert!(
        rx.try_recv().is_err(),
        "menu Escape must not stop the running job"
    );
    assert_eq!(
        view.read_with(window, |app, _| app.work.folder.clone()),
        Some(a)
    );
    assert!(view.read_with(window, |app, _| app.shell.tabs.menu.is_none()));
}

#[gpui::test]
fn outputs_menu_resolves_owned_paths_and_hides_other_threads(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_resources::tests::outputs_menu_resolves_owned_paths_and_hides_other_threads",
    ) {
        return;
    }
    let fixtures = tempfile::tempdir().unwrap();
    let a = fixtures.path().join("a");
    let b = fixtures.path().join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let (view, window) = setup(cx, &a);
    let owner = view.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_owned();
        app.work.conv_id = Some(owner.clone());
        app.work.outputs = vec!["report.txt".into()];
        cx.notify();
        owner
    });
    window.run_until_parked();
    open(&view, window, WorkshopMenu::Outputs);
    assert_eq!(
        view.read_with(window, |app, _| app.shell.tabs.resources.outputs.clone()),
        vec![("report.txt".into(), a.join("report.txt"))]
    );
    view.update(window, |app, cx| {
        app.work.outputs.push("late.txt".into());
        cx.notify();
    });
    window.run_until_parked();
    click(window, 1); // Refresh outputs recaptures this owner's files.
    assert_eq!(
        view.read_with(window, |app, _| app.shell.tabs.resources.outputs.len()),
        2
    );
    view.update(window, |app, cx| {
        let other = app
            .conversations
            .create_kind(55, awili_core::ConvKind::Work)
            .id;
        app.conversations.get_mut(&other).unwrap().cwd = Some(b.display().to_string());
        app.set_active_conversation(&other);
        app.work.running = true;
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window
            .debug_bounds("ui-row-awili-workshop-menu-item-3")
            .is_none(),
        "a selected thread cannot see the previous owner's output rows"
    );
    click(window, 0); // Back to selected B's folder menu.
    click(window, 5); // Outputs for B.
    assert!(view.read_with(window, |app, _| app.shell.tabs.resources.outputs.is_empty()));
    assert_eq!(
        view.read_with(window, |app, _| app.shell.tabs.resources.root.clone()),
        Some(b)
    );
    assert_eq!(
        view.read_with(window, |app, _| app.work.conv_id.clone()),
        Some(owner)
    );
    assert_eq!(
        view.read_with(window, |app, _| app.work.folder.clone()),
        Some(a)
    );
}

#[gpui::test]
fn files_menu_reports_removed_directory_and_keeps_back_action(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_resources::tests::files_menu_reports_removed_directory_and_keeps_back_action",
    ) {
        return;
    }
    let fixtures = tempfile::tempdir().unwrap();
    let folder = fixtures.path().join("missing");
    std::fs::create_dir(&folder).unwrap();
    let (view, window) = setup(cx, &folder);
    std::fs::remove_dir(&folder).unwrap();
    open(&view, window, WorkshopMenu::Files);
    assert!(view.read_with(window, |app, _| app.shell.tabs.resources.error.is_some()));
    click(window, 0);
    assert!(view.read_with(window, |app, _| app.shell.tabs.menu
        == Some(WorkshopMenu::Project)));
    assert!(!folder.exists());
}

// Stubs `open` on PATH, which is the macOS opener. Linux opens through an
// absolute `/usr/bin/xdg-open` that a PATH stub cannot intercept.
// TODO(linux): give `open_path` a test seam and run this on Linux too.
#[cfg(target_os = "macos")]
#[gpui::test]
fn files_and_outputs_open_complete_captured_paths(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_resources::tests::files_and_outputs_open_complete_captured_paths") {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let fixtures = tempfile::tempdir().unwrap();
    let folder = fixtures.path().join("folder with spaces");
    let bin = fixtures.path().join("bin");
    let log = fixtures.path().join("opened.log");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let name = "Résumé \u{1f9ed}.txt";
    let path = folder.join(name);
    std::fs::write(&path, "fixture").unwrap();
    std::fs::write(
        bin.join("open"),
        "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$AWILI_RESOURCE_OPEN_LOG\"\n",
    )
    .unwrap();
    std::fs::set_permissions(bin.join("open"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&original));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
    std::env::set_var("AWILI_RESOURCE_OPEN_LOG", &log);
    let (view, window) = setup(cx, &folder);
    open(&view, window, WorkshopMenu::Files);
    click(window, 4);
    for _ in 0..100 {
        if log.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [path.to_str().unwrap()]
    );
    view.update(window, |app, cx| {
        app.work.outputs = vec![name.into()];
        cx.notify();
    });
    window.run_until_parked();
    open(&view, window, WorkshopMenu::Outputs);
    click(window, 3);
    for _ in 0..100 {
        if std::fs::read_to_string(&log).unwrap().lines().count() == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [path.to_str().unwrap(), path.to_str().unwrap()]
    );
    assert!(view.read_with(window, |app, _| app.shell.tabs.menu.is_none()));
}

#[gpui::test]
fn files_menu_keyboard_scrolls_compact_and_restores_title_focus(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "workshop_resources::tests::files_menu_keyboard_scrolls_compact_and_restores_title_focus",
    ) {
        return;
    }
    let fixtures = tempfile::tempdir().unwrap();
    for i in 0..35 {
        std::fs::write(fixtures.path().join(format!("file-{i:02}.txt")), "fixture").unwrap();
    }
    let (view, window) = setup(cx, fixtures.path());
    for scale in [1.0_f32, 1.5] {
        crate::theme::set_ui_scale(scale);
        window.simulate_resize(size(px(800.), px(600.)));
        window.run_until_parked();
        open(&view, window, WorkshopMenu::Files);
        // Initial Back (0), skip disabled heading (1), Refresh (2), Finder (3),
        // then every file row (4..38). Enter on Back restores the root menu.
        for _ in 0..37 {
            window.simulate_keystrokes("down");
        }
        window.run_until_parked();
        let last = window
            .debug_bounds("ui-row-awili-workshop-menu-item-38")
            .unwrap();
        assert!(
            last.top() >= px(0.) && last.bottom() <= px(600.),
            "keyboard must scroll the final file into the compact viewport"
        );
        window.simulate_keystrokes("down enter");
        window.run_until_parked();
        assert!(view.read_with(window, |app, _| app.shell.tabs.menu
            == Some(WorkshopMenu::Project)));
        window.simulate_keystrokes("escape");
        window.run_until_parked();
        assert!(view.read_with(window, |app, _| app.shell.tabs.menu.is_none()));
        window.simulate_keystrokes("enter");
        window.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        window.run_until_parked();
        assert!(
            view.read_with(window, |app, _| app.shell.tabs.menu
                == Some(WorkshopMenu::Project)),
            "Escape must restore the title trigger's keyboard focus"
        );
        window.simulate_keystrokes("escape");
        window.run_until_parked();
    }
    crate::theme::set_ui_scale(1.0);
}

#[gpui::test]
fn recent_sessions_menu_opens_existing_session_reader(cx: &mut gpui::TestAppContext) {
    if !isolated("workshop_resources::tests::recent_sessions_menu_opens_existing_session_reader") {
        return;
    }
    use std::io::Write;
    let fixtures = tempfile::tempdir().unwrap();
    let (view, window) = setup(cx, fixtures.path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut stream, request) = session_request(&listener);
        let body = r#"{"messages":[{"role":"assistant","content":"Saved session answer"}]}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        tx.send(request).unwrap();
    });
    view.update(window, |app, cx| {
        app.work.gateway.dashboard_url = format!("http://{address}");
        app.work.hermes_sessions = vec![serde_json::from_value(serde_json::json!({"id":"s1","title":"Résumé \u{1f9ed}","model":"fixture","source":"api","message_count":3,"tool_call_count":2})).unwrap()];
        app.sessions_state.filter = "old filter".into();
        app.sessions_state.view = SessionsView::Work;
        cx.notify();
    });
    window.run_until_parked();
    open(&view, window, WorkshopMenu::RecentSessions);
    click(window, 3); // Back, Open Sessions, Refresh, then the session row.
    assert!(view.read_with(window, |app, _| app.mode == Mode::Sessions));
    assert!(view.read_with(window, |app, _| app.sessions_state.view
        == SessionsView::Hermes));
    assert!(view.read_with(window, |app, _| app.sessions_state.filter.is_empty()));
    assert_eq!(
        view.read_with(window, |app, _| app.sessions_state.selected_id.clone()),
        Some("s1".into())
    );
    let request = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert!(request.starts_with("GET /api/sessions/s1/messages "));
    worker.join().unwrap();
    for _ in 0..100 {
        window.run_until_parked();
        if view.read_with(window, |app, _| app.sessions_state.transcript.is_some()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        view.read_with(window, |app, _| app.sessions_state.transcript.clone()),
        Some((
            "s1".into(),
            vec![("assistant".into(), "Saved session answer".into())]
        ))
    );
}

#[gpui::test]
fn recent_session_switch_starts_new_fetch_and_ignores_older_completion(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("workshop_resources::tests::recent_session_switch_starts_new_fetch_and_ignores_older_completion") { return; }
    use std::io::Write;
    let fixtures = tempfile::tempdir().unwrap();
    let (view, window) = setup(cx, fixtures.path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
    let (release_a, held_a) = std::sync::mpsc::channel();
    let (release_b, held_b) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut replies = Vec::new();
        for hold in [held_a, held_b] {
            let (mut stream, request) = session_request(&listener);
            let sid = if request.starts_with("GET /api/sessions/a/messages ") {
                "a"
            } else {
                assert!(request.starts_with("GET /api/sessions/b/messages "));
                "b"
            };
            accepted_tx.send(sid).unwrap();
            replies.push(std::thread::spawn(move || {
                hold.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
                let body = format!(r#"{{"messages":[{{"role":"assistant","content":"Answer {sid}"}}]}}"#);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            }));
        }
        for reply in replies {
            reply.join().unwrap();
        }
    });
    view.update(window, |app, cx| {
        app.work.gateway.dashboard_url = format!("http://{address}");
        app.work.hermes_sessions = ["a", "b"]
            .into_iter()
            .map(|id| {
                serde_json::from_value(serde_json::json!({"id":id,"title":format!("Session {id}")}))
                    .unwrap()
            })
            .collect();
        cx.notify();
    });
    window.run_until_parked();
    open(&view, window, WorkshopMenu::RecentSessions);
    click(window, 3); // Open A in the existing Sessions browser.
    assert_eq!(
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap(),
        "a"
    );
    view.update(window, |app, cx| app.set_mode(Mode::Work, cx));
    window.run_until_parked();
    open(&view, window, WorkshopMenu::RecentSessions);
    click(window, 4); // Open B while A is held.
    assert_eq!(
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap(),
        "b"
    );
    let generation_b = view.read_with(window, |app, _| app.sessions_state.transcript_generation);
    assert_eq!(
        generation_b, 2,
        "both reader selections must start an owned request"
    );
    release_a.send(()).unwrap();
    // A late response must not settle B's loading state or install A's reply.
    for _ in 0..30 {
        window.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(view.read_with(window, |app, _| app.sessions_state.transcript_loading));
    assert_eq!(
        view.read_with(window, |app, _| app
            .sessions_state
            .transcript_request
            .clone()),
        Some("b".into())
    );
    assert!(view.read_with(window, |app, _| app.sessions_state.transcript.is_none()));
    release_b.send(()).unwrap();
    worker.join().unwrap();
    for _ in 0..100 {
        window.run_until_parked();
        if view.read_with(window, |app, _| app.sessions_state.transcript.is_some()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!view.read_with(window, |app, _| app.sessions_state.transcript_loading));
    assert_eq!(
        view.read_with(window, |app, _| app.sessions_state.transcript.clone()),
        Some(("b".into(), vec![("assistant".into(), "Answer b".into())]))
    );
}
