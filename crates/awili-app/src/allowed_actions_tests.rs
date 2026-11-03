//! S6 proof tests: the Allowed actions page lists every remembered approval
//! in plain words and Revoke removes it, durably.
use super::*;
use crate::settings_catalog::Page;
use awili_engine::events::{AwiliCallId, ToolCall};
use awili_engine_log::{test_bundle, ExecutionLog, MemLog, RiskClass};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_S6_TEST_HOME";

/// Run `test` in a subprocess with a private HOME. `true` inside the child.
fn in_isolated_child(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
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
    false
}

fn call(name: &str, args: &str) -> ToolCall {
    ToolCall {
        awili_call_id: AwiliCallId("c1".into()),
        provider_call_id: None,
        name: name.into(),
        arguments: args.into(),
    }
}

fn sel(selector: &str) -> &'static str {
    Box::leak(selector.to_string().into_boxed_str())
}

fn shown(window: &mut VisualTestContext, selector: &str) -> bool {
    window.debug_bounds(sel(selector)).is_some()
}

fn click(window: &mut VisualTestContext, selector: &str) {
    let b = window
        .debug_bounds(sel(selector))
        .unwrap_or_else(|| panic!("{selector} is on screen"));
    window.simulate_click(b.center(), Modifiers::default());
    window.run_until_parked();
}

fn settings_window(cx: &mut TestAppContext) -> (Entity<AwiliApp>, &mut VisualTestContext) {
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.open_settings(cx);
    });
    window.run_until_parked();
    (view, window)
}

fn private_store(tag: &str) -> (&'static GrantStoreService, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "awili-s6-app-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("grants.log");
    (GrantStoreService::init(&path, None).unwrap(), path)
}

fn grant(svc: &GrantStoreService, scope: &str, tool: &str, args: &str, folder: Option<&str>) {
    let cap = test_bundle().1.clone();
    let constraints = if scope == "always" || scope == "folder" {
        let v: serde_json::Value = serde_json::from_str(args).unwrap();
        serde_json::json!({"path": v["path"], "__paths": [v["path"]]}).to_string()
    } else {
        "{}".to_string()
    };
    svc.with_store(|log| {
        log.record_approval(
            &cap,
            scope,
            "t1",
            &call(tool, args),
            RiskClass::Mutating,
            folder,
            &constraints,
        )
    })
    .unwrap();
}

fn open_page(
    view: &Entity<AwiliApp>,
    window: &mut VisualTestContext,
    svc: &'static GrantStoreService,
) {
    view.update(window, |app, cx| {
        app.allowed_actions.store_override = Some(svc);
        app.select_settings_page(Page::Allowed, cx);
    });
    window.run_until_parked();
}

#[test]
fn s6_describe_says_what_where_and_when_in_plain_words() {
    let mut c = serde_json::Map::new();
    c.insert("path".into(), "notes.txt".into());
    c.insert("__paths".into(), serde_json::json!(["notes.txt"]));
    let always = RememberedGrant {
        approval_ref: "a".into(),
        all_refs: vec!["a".into()],
        scope: GrantScopeKind::Always,
        tool: "fs.write".into(),
        risk_class: "mutating".into(),
        folder_root: None,
        constraints: c,
        granted_at_ms: Some(1_700_000_000_000),
        session_id: "grants".into(),
    };
    let t = describe(&always);
    assert_eq!(t.what, "Write a file");
    assert_eq!(t.limit, "Only for this path: notes.txt");
    assert_eq!(t.scope, "In all projects");
    assert!(t.when.starts_with("Allowed Nov 1"), "{}", t.when);
    assert!(t.when.contains(", 2023 at"), "{}", t.when);

    let folder = RememberedGrant {
        scope: GrantScopeKind::Folder,
        folder_root: Some("/Users/j/proj".into()),
        constraints: serde_json::Map::new(),
        granted_at_ms: None,
        tool: "run_command".into(),
        ..always.clone()
    };
    let t = describe(&folder);
    assert_eq!(t.what, "run_command");
    assert_eq!(t.limit, "Any use of it, with any details");
    assert_eq!(t.scope, "In the folder /Users/j/proj");
    assert_eq!(t.when, "Allowed earlier (the time was not kept)");

    let mut c = serde_json::Map::new();
    c.insert("__commands".into(), serde_json::json!(["ls -la"]));
    let session = RememberedGrant {
        scope: GrantScopeKind::Session,
        constraints: c,
        ..always
    };
    let t = describe(&session);
    assert_eq!(t.scope, "This conversation only");
    assert_eq!(t.limit, "Only this command: ls -la");
}

#[gpui::test]
fn s6_empty_state_says_nothing_is_allowed_yet(cx: &mut TestAppContext) {
    if !in_isolated_child("allowed_actions::tests::s6_empty_state_says_nothing_is_allowed_yet") {
        return;
    }
    let (view, window) = settings_window(cx);
    let (svc, _) = private_store("empty");
    open_page(&view, window, svc);
    assert!(shown(window, "allowed-empty"));
    assert!(!shown(window, "allowed-row-0"));
    let text = view.update(window, |app, _| app.allowed_actions.grants.len());
    assert_eq!(text, 0);
}

#[gpui::test]
fn s6_page_lists_grants_newest_first_and_revoke_removes_the_row(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "allowed_actions::tests::s6_page_lists_grants_newest_first_and_revoke_removes_the_row",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    let (svc, path) = private_store("list");
    grant(svc, "always", "fs.write", r#"{"path":"old.txt"}"#, None);
    std::thread::sleep(std::time::Duration::from_millis(5));
    grant(
        svc,
        "folder",
        "fs.edit",
        r#"{"path":"new.txt"}"#,
        Some("/work/p"),
    );
    // The nav row for the page exists and opens it.
    open_page(&view, window, svc);
    assert!(shown(window, "sx-nav-9"), "the page is in the sidebar");
    assert!(!shown(window, "allowed-empty"));
    assert!(shown(window, "allowed-row-0"));
    assert!(shown(window, "allowed-row-1"));
    assert!(!shown(window, "allowed-row-2"));
    let first = view.update(window, |app, _| {
        describe(&app.allowed_actions.grants[0]).what
    });
    assert_eq!(first, "Edit a file", "newest first");

    // Revoke the newest: its row goes, the older one stays, and a fresh read
    // of the file agrees.
    click(window, "sx-revoke-0");
    assert!(!shown(window, "allowed-row-1"));
    let (what, n) = view.update(window, |app, _| {
        (
            describe(&app.allowed_actions.grants[0]).what,
            app.allowed_actions.grants.len(),
        )
    });
    assert_eq!((what.as_str(), n), ("Write a file", 1));
    let (records, _) =
        ExecutionLog::open_grant_store(awili_engine_log::storage::FileLog::open(&path).unwrap())
            .unwrap()
            .read_records()
            .unwrap();
    let left = awili_engine_log::remembered::stored_grants_from_records(&records);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].tool, "fs.write");

    // Revoke the last one: the empty state returns.
    click(window, "sx-revoke-0");
    assert!(shown(window, "allowed-empty"));
}

#[gpui::test]
fn s6_session_grant_is_listed_and_revoked_in_memory(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "allowed_actions::tests::s6_session_grant_is_listed_and_revoked_in_memory",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    let (svc, _) = private_store("session");
    let cap = test_bundle().1.clone();
    let mut session = ExecutionLog::open(MemLog::new(), "s6-app-session").unwrap();
    session
        .record_approval(
            &cap,
            "session",
            "t1",
            &call("fs.write", r#"{"path":"x"}"#),
            RiskClass::Mutating,
            None,
            "{}",
        )
        .unwrap();
    open_page(&view, window, svc);
    assert!(shown(window, "allowed-row-0"));
    let scope = view.update(window, |app, _| {
        describe(&app.allowed_actions.grants[0]).scope
    });
    assert_eq!(scope, "This conversation only");
    click(window, "sx-revoke-0");
    assert!(shown(window, "allowed-empty"));
    awili_engine_log::remembered::forget_session("s6-app-session");
}

#[gpui::test]
fn s6_revoke_is_reachable_from_the_keyboard(cx: &mut TestAppContext) {
    if !in_isolated_child("allowed_actions::tests::s6_revoke_is_reachable_from_the_keyboard") {
        return;
    }
    let (view, window) = settings_window(cx);
    let (svc, _) = private_store("keys");
    grant(svc, "always", "fs.write", r#"{"path":"k.txt"}"#, None);
    open_page(&view, window, svc);
    // Tab from the sidebar lands on the first Revoke; Enter presses it.
    let used = view.update_in(window, |app, window, cx| {
        let tab = gpui::Keystroke::parse("tab").unwrap();
        let enter = gpui::Keystroke::parse("enter").unwrap();
        let a = app.settings_key(&tab, window, cx);
        let at = app.settings_stop;
        let b = app.settings_key(&enter, window, cx);
        (a, at, b)
    });
    window.run_until_parked();
    assert_eq!(
        used,
        (
            true,
            Some(crate::settings_focus::FocusStop::Revoke(0)),
            true
        )
    );
    assert!(shown(window, "allowed-empty"), "Enter on Revoke removed it");
}

#[gpui::test]
fn s6_identical_grants_are_one_row_and_one_revoke_takes_all(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "allowed_actions::tests::s6_identical_grants_are_one_row_and_one_revoke_takes_all",
    ) {
        return;
    }
    let (view, window) = settings_window(cx);
    let (svc, path) = private_store("dupes");
    grant(svc, "always", "fs.write", r#"{"path":"same.txt"}"#, None);
    grant(svc, "always", "fs.write", r#"{"path":"same.txt"}"#, None);
    grant(svc, "always", "fs.write", r#"{"path":"other.txt"}"#, None);
    open_page(&view, window, svc);
    assert!(shown(window, "allowed-row-1"));
    assert!(
        !shown(window, "allowed-row-2"),
        "the two identical ones are one row"
    );
    // Revoke the collapsed row (the same.txt one is older: row 1).
    let ix = view.update(window, |app, _| {
        app.allowed_actions
            .grants
            .iter()
            .position(|g| g.all_refs.len() == 2)
            .unwrap()
    });
    click(window, &format!("sx-revoke-{ix}"));
    let (records, _) =
        ExecutionLog::open_grant_store(awili_engine_log::storage::FileLog::open(&path).unwrap())
            .unwrap()
            .read_records()
            .unwrap();
    let left = awili_engine_log::remembered::stored_grants_from_records(&records);
    assert_eq!(left.len(), 1, "no identical older grant stays active");
    assert_eq!(left[0].constraints.get("path").unwrap(), "other.txt");
}
