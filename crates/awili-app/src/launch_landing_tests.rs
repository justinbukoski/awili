//! R2g proof tests: real cold launches. Each test builds `AwiliApp::new`
//! more than once against the same private HOME (a subprocess per test, the
//! U2c-FIX1 isolation), so the second build reads exactly what the first one
//! saved, as a relaunch does.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use awili_code::CodeEngine;
use awili_core::{ConvKind, Role};
use gpui::AppContext;

use super::*;
use crate::work::WorkExecutor;
use crate::AwiliApp;

const CHILD: &str = "AWILI_R2G_TEST_HOME";

/// Run `test` in a subprocess with a private HOME. `Some(home)` inside the
/// child; `None` in the parent once the child passed.
fn isolated(test: &str) -> Option<PathBuf> {
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
        out.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    None
}

/// Write the config file Awili will load (a config file means onboarding is
/// done).
fn with_config(text: &str) {
    let path = awili_config::Config::config_paths()[0].clone();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
}

/// Wait until the persistence worker has written what `done` looks for.
fn saved(done: impl Fn(&awili_persist::Store) -> bool) {
    let store = awili_persist::Store::open_default().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(&store) {
        assert!(
            Instant::now() < deadline,
            "the launch never saved its state"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn state(store: &awili_persist::Store, key: &str) -> Option<String> {
    store.get_app_state(key).ok().flatten()
}

fn work_ids(app: &AwiliApp) -> Vec<String> {
    app.conversations
        .active()
        .into_iter()
        .filter(|c| c.kind == ConvKind::Work)
        .map(|c| c.id.clone())
        .collect()
}

fn launch(cx: &mut gpui::TestAppContext) -> gpui::Entity<AwiliApp> {
    let app = cx.new(AwiliApp::new);
    cx.run_until_parked();
    app
}

/// Owner's case: Work was the last mode and its conversation has a turn.
/// The next cold launch opens Work on a fresh, empty conversation (the old
/// one is untouched and still listed), with no "another conversation" card;
/// a third launch reuses that empty conversation (no empty rows pile up).
#[gpui::test]
fn r2g_cold_launch_returns_to_work_on_a_fresh_conversation(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("launch_landing::tests::r2g_cold_launch_returns_to_work_on_a_fresh_conversation")
    else {
        return;
    };
    cx.executor().allow_parking();
    with_config("[work]\n");
    let folder = home.join("f2b");
    std::fs::create_dir_all(&folder).unwrap();
    let first = launch(cx);
    let old = first.update(cx, |app, cx| {
        assert_eq!(app.mode, Mode::Work, "no saved mode: Workshop");
        app.set_mode(Mode::Work, cx);
        app.work.folder = Some(folder.clone());
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
        app.submit_work_run("Summarise these files into SUMMARY.md".into(), cx);
        app.sidebar.active_id().to_string()
    });
    saved(|s| {
        state(s, "last_mode").as_deref() == Some("work")
            && s.messages(&old).map(|m| !m.is_empty()).unwrap_or(false)
    });
    // The old conversation's last run, as a finished run leaves it (P2e):
    // entering Work restores it into the panel on the way in.
    let last = crate::work_last_run::LastRun {
        identity: crate::work_run_identity::WorkRunIdentity::Agent {
            engine: CodeEngine::Claude,
            model: None,
        },
        model: None,
        status: awili_gateway::run_events::RunStatus::Failed,
        error: Some("agent CLI not found".into()),
        finished_at: "2026-10-01T07:40:00Z".into(),
    };
    awili_persist::Store::open_default()
        .unwrap()
        .set_app_state(
            &format!("{}{old}", crate::persistence::WORK_LAST_RUN_KEY_PREFIX),
            &last.to_json(),
        )
        .unwrap();

    let second = launch(cx);
    let fresh = second.update(cx, |app, _cx| {
        assert_eq!(app.mode, Mode::Work, "back to the mode that was left");
        let active = app.sidebar.active_id().to_string();
        assert_ne!(active, old, "never an old conversation");
        assert_eq!(
            app.conversations.get(&active).map(|c| c.kind),
            Some(ConvKind::Work)
        );
        assert!(
            app.views
                .get(&active)
                .is_none_or(|v| v.convo.messages().is_empty()),
            "the fresh conversation is empty"
        );
        let kept: Vec<String> = app.views[&old]
            .convo
            .messages()
            .iter()
            .filter(|m| m.role == Role::User)
            .map(|m| m.text.clone())
            .collect();
        assert_eq!(
            kept,
            vec!["Summarise these files into SUMMARY.md".to_string()]
        );
        assert!(
            app.work.panel_is_visible(&active),
            "no 'previous run belongs to another conversation' card"
        );
        active
    });
    saved(|s| state(s, crate::persistence::ACTIVE_CONVERSATION_KEY).as_deref() == Some(&fresh));
    let before = second.read_with(cx, |app, _| work_ids(app).len());

    let third = launch(cx);
    third.read_with(cx, |app, _| {
        assert_eq!(
            app.sidebar.active_id(),
            fresh,
            "the empty conversation is reused"
        );
        assert_eq!(work_ids(app).len(), before, "no new empty row");
    });
}

/// The household case: a CLI-only install (no model services, an installed
/// Default Work helper) used to open Work on every launch. Chat was the last
/// mode, so the next launch opens Chat.
#[gpui::test]
fn r2g_cold_launch_returns_to_chat_when_chat_was_last(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("launch_landing::tests::r2g_cold_launch_returns_to_chat_when_chat_was_last")
    else {
        return;
    };
    cx.executor().allow_parking();
    let claude = installed_claude(&home);
    with_config(&format!(
        "[work]\ndefault_executor = \"agent:claude\"\n\n[work.agents]\nclaude_bin = \"{}\"\n",
        claude.display()
    ));
    let first = launch(cx);
    first.update(cx, |app, cx| {
        assert_eq!(
            app.mode,
            Mode::Work,
            "no saved mode yet: the CLI-only rule still opens Work"
        );
        app.set_mode(Mode::Code, cx);
    });
    saved(|s| state(s, "last_mode").as_deref() == Some("code"));
    let second = launch(cx);
    second.read_with(cx, |app, _| {
        assert_eq!(app.mode, Mode::Code, "back to Code, where the user was");
    });
}

/// A conversation restored at launch opens at its latest message, not
/// scrolled to the top (round 2 Minor 16).
#[gpui::test]
fn r2g_restored_transcript_opens_at_its_latest_message(cx: &mut gpui::TestAppContext) {
    if isolated("launch_landing::tests::r2g_restored_transcript_opens_at_its_latest_message")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    with_config("[work]\n");
    let first = launch(cx);
    let id = first.update(cx, |app, _cx| {
        let id = app.sidebar.active_id().to_string();
        for i in 0..80u64 {
            let (role, text) = if i % 2 == 0 {
                ("user", format!("question {i}"))
            } else {
                (
                    "assistant",
                    format!("answer {i}\n\nwith a second paragraph"),
                )
            };
            app.persist_message(&id, role, &text, 1_000 + i);
        }
        app.save_active_conversation();
        id
    });
    saved(|s| s.messages(&id).map(|m| m.len() == 80).unwrap_or(false));

    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    window.run_until_parked();
    let offset = view.update(window, |app, _| {
        assert_eq!(app.sidebar.active_id(), id, "the long conversation is open");
        app.views[&id].scroll.offset()
    });
    assert!(
        offset.y < gpui::px(0.),
        "the transcript is scrolled to its end: {offset:?}"
    );
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn installed_claude(home: &Path) -> PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// An executable stand-in for the `claude` binary (never run here).
fn installed_claude(home: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("fake-claude");
    std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin
}
