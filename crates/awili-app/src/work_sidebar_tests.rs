//! R1f proof: the real `AwiliApp` (built by `AwiliApp::new` in a subprocess
//! with a private HOME) drives "+ New conversation", a Work submit and a folder
//! switch; the sidebar is rendered in a test window and read back through its
//! `debug_selector` bounds.

use std::path::{Path, PathBuf};

use awili_code::CodeEngine;
use awili_core::{ConvKind, Role};
use awili_work::ApprovalMode;
use gpui::{px, size};

use crate::sidebar::{ACTIVE_ROW_SELECTOR, GROUP_SELECTOR};
use crate::work::{Mode, WorkExecutor};
use crate::AwiliApp;

const CHILD: &str = "AWILI_R1F_TEST_HOME";

/// Run `test` in a subprocess with a private HOME. `Some(home)` inside the
/// child (run the body); `None` in the parent once the child passed.
fn isolated(test: &str) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os(CHILD) {
        return Some(home.into());
    }
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

/// Work mode on Claude (a binary path that does not exist, so no CLI ever
/// runs). Returns a real folder `home/<name>` for the caller to bind.
fn work_app(app: &mut AwiliApp, home: &Path, name: &str) -> PathBuf {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.work
        .graduation
        .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
    app.shell.conversations = true;
    app.mode = Mode::Work;
    app.work.executor = WorkExecutor::Agent {
        engine: CodeEngine::Claude,
        model: None,
    };
    app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
    let folder = home.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    folder
}

/// The shown thread's next turn is Claude, chosen the way the chooser does.
/// A send follows the conversation's route, not `work.executor`.
fn choose_claude(app: &mut AwiliApp, cx: &mut gpui::Context<AwiliApp>) {
    let id = app.sidebar.active_id().to_string();
    assert!(app.set_composer_target(
        &crate::workshop_composer::ModelOwner::Work(id),
        WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        },
        cx,
    ));
}

/// `n` old Work conversations with one turn each and no folder: months of
/// "General" history.
fn old_history(app: &mut AwiliApp, n: usize) {
    for i in 0..n {
        let meta = app
            .conversations
            .create_kind(1_000_000 + i as i64, ConvKind::Work);
        let mut state = awili_core::ConversationState::new();
        state.push(Role::User, format!("old task {i}"));
        app.views
            .insert(meta.id, crate::conv_view::ConvView::from_state(state));
    }
}

fn cwd_of(app: &AwiliApp, id: &str) -> Option<String> {
    app.conversations.get(id).and_then(|c| c.cwd.clone())
}

fn work_ids(app: &AwiliApp) -> Vec<String> {
    app.conversations
        .active()
        .into_iter()
        .filter(|c| c.kind == ConvKind::Work)
        .map(|c| c.id.clone())
        .collect()
}

/// Major 4: after a send, the conversation is filed under the folder the
/// turn runs in (memory and database), and the rendered sidebar shows it at
/// the top — above the long "General" history and inside the window, with
/// no scrolling. A folder group holding only Chat conversations draws no
/// heading in Work.
#[gpui::test]
fn r1f_sent_conversation_is_filed_and_shown_first(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work_sidebar::tests::r1f_sent_conversation_is_filed_and_shown_first")
    else {
        return;
    };
    let store = awili_persist::Store::open_default().unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    let id = view.update_in(window, |app, window, cx| {
        let folder = work_app(app, &home, "b1");
        old_history(app, 30);
        let chat = app.conversations.create_kind(2_000_000, ConvKind::Chat);
        app.conversations.get_mut(&chat.id).unwrap().cwd = Some("/x/chat-only".into());
        // "+ New conversation" with no folder bound yet, then the folder the
        // composer shows is restored without touching the conversation (the
        // launch path).
        app.work.folder = None;
        app.new_conversation(window, cx);
        let id = app.sidebar.active_id().to_string();
        assert_eq!(cwd_of(app, &id), None);
        app.work.folder = Some(folder.clone());

        app.submit_work_run("Summarise these files into SUMMARY.md".into(), cx);
        assert_eq!(
            cwd_of(app, &id).as_deref(),
            Some(folder.display().to_string().as_str()),
            "the first turn files the conversation under its folder"
        );
        cx.notify();
        id
    });
    window.run_until_parked();

    let active = window
        .debug_bounds(ACTIVE_ROW_SELECTOR)
        .expect("the active row is drawn");
    let general = window
        .debug_bounds("r1f-group-General")
        .expect("the General heading is drawn");
    let b1 = window
        .debug_bounds("r1f-group-b1")
        .expect("the b1 heading is drawn");
    assert!(
        b1.top() < active.top() && active.bottom() <= general.top(),
        "the sent conversation's group is first: b1 {b1:?}, row {active:?}, General {general:?}"
    );
    assert!(
        active.bottom() <= px(800.),
        "the sent conversation is visible without scrolling: {active:?}"
    );
    assert!(
        window.debug_bounds("r1f-group-chat-only").is_some(),
        "shared Workshop history includes Chat threads too"
    );
    assert_eq!(format!("{GROUP_SELECTOR}b1"), "r1f-group-b1");

    let folder = home.join("b1").display().to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let saved = store
            .conversations()
            .unwrap()
            .into_iter()
            .find(|c| c.id == id)
            .and_then(|c| c.cwd);
        if saved.as_deref() == Some(folder.as_str()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the database records the folder: {saved:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Major 4: "+ New conversation" files the new Work conversation under the
/// folder the composer shows, and a second click reuses the never-sent one
/// (no empty husk) — back in the default Ask mode. After a send, the next
/// click makes a fresh conversation.
#[gpui::test]
fn r1f_new_conversation_reuses_the_unsent_one(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work_sidebar::tests::r1f_new_conversation_reuses_the_unsent_one")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        let folder = work_app(app, &home, "b1");
        app.work.folder = Some(folder.clone());
        app.new_conversation(window, cx);
        let first = app.sidebar.active_id().to_string();
        // The thread the app opened with is itself a never-sent Work thread,
        // so the first click reuses it; the count is the baseline.
        let before = work_ids(app).len();
        assert_eq!(
            cwd_of(app, &first).as_deref(),
            Some(folder.display().to_string().as_str()),
            "a new conversation is filed under the folder the composer shows"
        );
        let key = crate::ask_mode::AskKey::Work(first.clone());
        app.work.ask_modes.set(key.clone(), ApprovalMode::Auto);

        app.new_conversation(window, cx);
        assert_eq!(app.sidebar.active_id(), first, "the unsent one is reused");
        assert_eq!(work_ids(app).len(), before, "no empty husk is left");
        assert_eq!(
            app.work.ask_modes.mode_for(&key),
            app.work.ask_modes.default_mode(),
            "a reused conversation starts in the default mode"
        );

        app.submit_work_run("tidy the notes".into(), cx);
        app.cancel_work_run(cx);
        app.new_conversation(window, cx);
        assert_ne!(
            app.sidebar.active_id(),
            first,
            "a sent conversation is kept"
        );
        assert_eq!(work_ids(app).len(), before + 1);
    });
}

/// Major 6 tail: switching the folder never moves a conversation that has
/// turns to the new folder's group; a conversation with no turns yet follows
/// the folder the composer shows.
#[gpui::test]
fn r1f_folder_switch_keeps_a_conversation_with_turns(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work_sidebar::tests::r1f_folder_switch_keeps_a_conversation_with_turns")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        let a = work_app(app, &home, "a");
        let b = home.join("b");
        let c = home.join("c");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir_all(&c).unwrap();
        app.work.folder = Some(a.clone());
        app.new_conversation(window, cx);
        let sent = app.sidebar.active_id().to_string();
        app.submit_work_run("plan the trip".into(), cx);
        app.cancel_work_run(cx);

        app.set_work_folder(b.clone(), cx);
        assert_eq!(
            cwd_of(app, &sent).as_deref(),
            Some(a.display().to_string().as_str()),
            "a conversation with turns stays in its folder group"
        );

        app.new_conversation(window, cx);
        let fresh = app.sidebar.active_id().to_string();
        assert_ne!(fresh, sent);
        assert_eq!(
            cwd_of(app, &fresh).as_deref(),
            Some(b.display().to_string().as_str())
        );
        app.set_work_folder(c.clone(), cx);
        assert_eq!(
            cwd_of(app, &fresh).as_deref(),
            Some(c.display().to_string().as_str()),
            "a conversation with no turns follows the folder"
        );
    });
}

/// The profile `id` is tagged with (`None` is the default profile).
fn profile_of(app: &AwiliApp, id: &str) -> Option<String> {
    app.conversations.get(id).and_then(|c| c.profile.clone())
}

/// R1f-FIX1 (T2 P1): "+ New conversation" in profile A never reuses an empty,
/// never-sent Work conversation of profile B — a fresh one is created in A and
/// B's keeps its profile. Not even with "show all" on, where B's row is listed
/// and is the newest. A's own unsent conversation is still reused.
#[gpui::test]
fn r1f_fix1_new_conversation_never_reuses_another_profiles(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work_sidebar::tests::r1f_fix1_new_conversation_never_reuses_another_profiles")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        let folder = work_app(app, &home, "b1");
        app.work.folder = Some(folder);
        for name in ["a", "b"] {
            assert!(app
                .profile_store
                .create(name, crate::profile::ProfileColor::Green, 0));
        }
        let before = work_ids(app).len();

        assert!(app.profile_store.set_active("b"));
        app.new_conversation(window, cx);
        let hidden = app.sidebar.active_id().to_string();
        assert_eq!(profile_of(app, &hidden).as_deref(), Some("b"));

        assert!(app.profile_store.set_active("a"));
        app.new_conversation(window, cx);
        let fresh = app.sidebar.active_id().to_string();
        assert_ne!(
            fresh, hidden,
            "another profile's unsent conversation is never reused"
        );
        assert_eq!(
            profile_of(app, &fresh).as_deref(),
            Some("a"),
            "a fresh conversation is created in the active profile"
        );
        assert_eq!(
            profile_of(app, &hidden).as_deref(),
            Some("b"),
            "the other profile's conversation keeps its profile"
        );
        assert_eq!(work_ids(app).len(), before + 1);

        app.new_conversation(window, cx);
        assert_eq!(
            app.sidebar.active_id(),
            fresh,
            "the active profile's own unsent one is reused"
        );
        assert_eq!(work_ids(app).len(), before + 1, "no empty husk is left");

        // "Show all" lists B's conversation, and it is the newest.
        app.conversations
            .touch(&hidden, crate::feed::unix_now() + 60);
        app.profile_store.toggle_show_all();
        assert!(app.profile_store.show_all());
        app.new_conversation(window, cx);
        assert_eq!(
            app.sidebar.active_id(),
            fresh,
            "with show all on, another profile's conversation is still never reused"
        );
        assert_eq!(profile_of(app, &hidden).as_deref(), Some("b"));
        assert_eq!(work_ids(app).len(), before + 1);
    });
}

/// R1f-FIX1 (T2 P1): attachments staged in profile B's never-sent Work
/// conversation are never shown in profile A's "+ New conversation" tray, and
/// they stay staged on B's conversation only.
#[gpui::test]
fn r1f_fix1_other_profiles_staged_attachments_never_exposed(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work_sidebar::tests::r1f_fix1_other_profiles_staged_attachments_never_exposed")
    else {
        return;
    };
    let store = awili_persist::Store::open_default().unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        let folder = work_app(app, &home, "b1");
        app.work.folder = Some(folder);
        for name in ["a", "b"] {
            assert!(app
                .profile_store
                .create(name, crate::profile::ProfileColor::Green, 0));
        }

        assert!(app.profile_store.set_active("b"));
        app.new_conversation(window, cx);
        let hidden = app.sidebar.active_id().to_string();
        // Staging needs the conversation's database row (foreign key).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !store
            .conversations()
            .unwrap()
            .iter()
            .any(|c| c.id == hidden)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the conversation row is saved"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.attach_bytes("b-only.txt", "text/plain", b"profile b only", cx);
        assert_eq!(app.staged_attachments.len(), 1, "staged in profile B");

        assert!(app.profile_store.set_active("a"));
        app.new_conversation(window, cx);
        let fresh = app.sidebar.active_id().to_string();
        assert!(
            app.staged_attachments.is_empty(),
            "another profile's staged attachments are never exposed: {:?}",
            app.staged_attachments
                .iter()
                .map(|a| a.filename.clone())
                .collect::<Vec<_>>()
        );
        assert_ne!(fresh, hidden);
        assert_eq!(profile_of(app, &fresh).as_deref(), Some("a"));
        assert_eq!(store.get_staged_attachments(&hidden).unwrap().len(), 1);
        assert!(store.get_staged_attachments(&fresh).unwrap().is_empty());
    });
}

/// RC2-RUN (round 7 Minor 1 / F-3): a folder the owner chooses is the folder
/// of the conversation on screen from now on — the sidebar files it under
/// the new folder and the database row records it, so it is still there
/// after a restart (the session-only pick is gone then).
/// RC2-R67 (usability RC2 round 5, Minor 4): chosen in a conversation that
/// already has turns, it opens a new conversation in that folder (the one
/// on screen); the conversation with turns keeps the folder they ran in.
#[gpui::test]
fn rc2_run_a_chosen_folder_refiles_the_conversation(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work_sidebar::tests::rc2_run_a_chosen_folder_refiles_the_conversation")
    else {
        return;
    };
    let store = awili_persist::Store::open_default().unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    let (id, b) = view.update_in(window, |app, window, cx| {
        let a = work_app(app, &home, "fa");
        let b = home.join("fb");
        std::fs::create_dir_all(&b).unwrap();
        app.work.folder = Some(a.clone());
        app.new_conversation(window, cx);
        let id = app.sidebar.active_id().to_string();
        app.submit_work_run("plan the trip".into(), cx);
        app.cancel_work_run(cx);
        assert_eq!(
            cwd_of(app, &id).as_deref(),
            Some(a.display().to_string().as_str())
        );

        app.choose_work_folder(b.clone(), cx);
        // RC2-R67: the turns keep the folder they ran in; the choice opens
        // a new conversation, on screen, filed under fb.
        let turns = id;
        let id = app.sidebar.active_id().to_string();
        assert_ne!(id, turns, "a new conversation is on screen");
        assert_eq!(
            cwd_of(app, &turns).as_deref(),
            Some(a.display().to_string().as_str()),
            "the conversation with turns keeps fa"
        );
        assert_eq!(
            cwd_of(app, &id).as_deref(),
            Some(b.display().to_string().as_str()),
            "the owner's choice files the conversation on screen"
        );
        let group = crate::sidebar::workspace_groups_scoped(
            &app.conversations,
            Some(app.profile_store.active()),
            false,
        )
        .into_iter()
        .find(|g| g.session_ids.contains(&id))
        .expect("the conversation is listed");
        assert_eq!(group.label, "fb", "the sidebar shows it under fb");

        // "After a restart": the session-only pick is gone; opening the
        // conversation again still puts the Work folder on fb.
        app.work.folder_picked.clear();
        app.work.folder = Some(a.clone());
        assert!(app.restore_work_folder(cx));
        assert_eq!(app.work.folder.as_deref(), Some(b.as_path()));
        (id, b)
    });
    window.run_until_parked();

    let folder = b.display().to_string();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let saved = store
            .conversations()
            .unwrap()
            .into_iter()
            .find(|c| c.id == id)
            .and_then(|c| c.cwd);
        if saved.as_deref() == Some(folder.as_str()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the database records the chosen folder: {saved:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// RC2-RUN (UX-FOOTER; round 3 Minor 11, round 5 Major 3): a Work reply's
/// footer ("Stopped · Claude · … · tokens not reported") is saved with the
/// reply and is back under it when the conversation is restored after a
/// relaunch — not only for the session it was written in.
#[gpui::test]
fn rc2_run_reply_footers_survive_a_relaunch(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work_sidebar::tests::rc2_run_reply_footers_survive_a_relaunch")
    else {
        return;
    };
    let store = awili_persist::Store::open_default().unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    let (id, footer) = view.update_in(window, |app, window, cx| {
        let folder = work_app(app, &home, "ft");
        app.work.folder = Some(folder);
        app.new_conversation(window, cx);
        let id = app.sidebar.active_id().to_string();
        app.submit_work_run("plan the trip".into(), cx);
        app.cancel_work_run(cx);
        let footer = app.views[&id]
            .rows
            .last()
            .and_then(|r| r.footer.clone())
            .expect("the stopped reply has its footer")
            .to_string();
        (id, footer)
    });
    window.run_until_parked();
    let key = crate::run_footer::footers_key(&id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while store.get_app_state(&key).unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "the footer is saved");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // A relaunch: a fresh app restored from the same database.
    let (again, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    again.update_in(window2, |app, _window, _cx| {
        let rows = &app
            .views
            .get(&id)
            .expect("the conversation is restored")
            .rows;
        let last = rows.last().expect("the reply is restored");
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(
            last.footer.as_ref().map(|f| f.to_string()),
            Some(footer.clone()),
            "the footer is back under the reply"
        );
        assert!(
            rows.iter().filter(|r| r.footer.is_some()).count() == 1,
            "no other row takes it"
        );
    });
}

/// RC2-APP (F0300 and the RUN follow-up): deleting a Work conversation drops
/// its run's owner once the run has ended (the next conversation shows its
/// own panel) and removes the conversation's saved last run and reply
/// footers — in memory and on disk.
#[gpui::test]
fn rc2app_deleting_a_conversation_forgets_its_run(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work_sidebar::tests::rc2app_deleting_a_conversation_forgets_its_run")
    else {
        return;
    };
    let store = awili_persist::Store::open_default().unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    // A finished, stopped run: its footer and last run are saved.
    let id = view.update_in(window, |app, window, cx| {
        let folder = work_app(app, &home, "del");
        app.work.folder = Some(folder);
        app.new_conversation(window, cx);
        choose_claude(app, cx);
        let id = app.sidebar.active_id().to_string();
        app.submit_work_run("plan the trip".into(), cx);
        app.cancel_work_run(cx);
        id
    });
    window.run_until_parked();
    let keys = crate::persistence::conversation_state_keys(&id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while keys
        .iter()
        .any(|k| store.get_app_state(k).unwrap().is_none())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the last run and the footers are saved"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    view.update_in(window, |app, window, cx| {
        assert_eq!(app.work.conv_id.as_deref(), Some(id.as_str()));
        assert!(app.work_last_run.contains_key(&id) && app.work_footers.contains_key(&id));
        app.delete_conversation(&id, window, cx);
        assert_eq!(app.work.conv_id, None, "the deleted run owner is dropped");
        assert!(!app.work_last_run.contains_key(&id));
        assert!(!app.work_footers.contains_key(&id));
        assert!(
            app.work.panel_is_visible(app.sidebar.active_id()),
            "the next conversation shows its own panel"
        );
    });
    window.run_until_parked();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while keys
        .iter()
        .any(|k| store.get_app_state(k).unwrap().is_some())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the rows are removed from disk"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // A run still going when its conversation is deleted keeps its owner
    // (RC1-J1-FIX1: the turn goes on; its late events find no conversation)
    // until it ends; then the owner is dropped.
    view.update_in(window, |app, window, cx| {
        app.new_conversation(window, cx);
        let live = app.sidebar.active_id().to_string();
        app.submit_work_run("plan the trip again".into(), cx);
        assert!(app.work.running, "the run is live");
        app.delete_conversation(&live, window, cx);
        assert!(app.work.running, "deleting does not stop the turn");
        assert_eq!(app.work.conv_id.as_deref(), Some(live.as_str()));
        app.cancel_work_run(cx);
        assert!(!app.work.running);
        assert_eq!(app.work.conv_id, None, "dropped at the run's end");
    });
}

/// RC2-R1F5 (usability RC2 round 1, N1: ⌘O in a conversation with turns
/// "silently moves that whole conversation to the new folder"). A moved
/// conversation (RC2-R67: as an earlier build moved it; ⌘O now opens a new
/// conversation instead) is filed under the new folder, its next turn runs
/// there, and the move shows — a note in the transcript at that point ("Now working
/// in k3 — earlier turns were in g1"), each earlier reply's footer naming
/// g1, and the sidebar row saying "Moved from g1" — also after a relaunch.
#[gpui::test]
fn rc2r1f5_a_folder_chosen_in_a_conversation_with_turns_shows_the_move(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work_sidebar::tests::rc2r1f5_a_folder_chosen_in_a_conversation_with_turns_shows_the_move",
    ) else {
        return;
    };
    let store = awili_persist::Store::open_default().unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let (id, g1, k3) = view.update_in(window, |app, window, cx| {
        let g1 = work_app(app, &home, "g1");
        let k3 = home.join("k3");
        std::fs::create_dir_all(&k3).unwrap();
        app.work.folder = Some(g1.clone());
        app.new_conversation(window, cx);
        let id = app.sidebar.active_id().to_string();
        for ask in ["list the files in this folder", "what is in alpha.txt?"] {
            app.submit_work_run(ask.into(), cx);
            app.cancel_work_run(cx);
        }
        assert_eq!(app.views[&id].convo.messages().len(), 4);
        // RC2-R67: ⌘O no longer moves a conversation with turns (it opens a
        // new one); a move an earlier build made still shows. Made here as
        // that build made it.
        let k3s = k3.display().to_string();
        app.note_folder_move(&id, Some(g1.display().to_string()), &k3s);
        app.work.folder_picked.insert(id.clone(), k3.clone());
        app.set_work_folder(k3.clone(), cx);
        app.file_work_conversation(&id);
        (id, g1, k3)
    });
    window.run_until_parked();
    let g1s = g1.display().to_string();
    let k3s = k3.display().to_string();
    view.update(window, |app, _| {
        assert_eq!(app.work.folder.as_deref(), Some(k3.as_path()));
        assert_eq!(
            cwd_of(app, &id).as_deref(),
            Some(k3s.as_str()),
            "filed under k3"
        );
        let moves = app.folder_moves(&id).to_vec();
        assert_eq!(
            moves,
            vec![crate::work_folder_moves::FolderMove {
                at: 4,
                from: g1s.clone(),
                to: k3s.clone(),
            }]
        );
        assert_eq!(
            crate::work_folder_moves::note_text(&moves[0]),
            "Now working in k3 \u{2014} earlier turns were in g1"
        );
        // Each earlier reply keeps the folder it ran in.
        // RC2-R1G3 (RC2-R1F review P3): both earlier replies have a footer
        // (the check below cannot pass on none).
        let footed: Vec<usize> = app.views[&id]
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.footer.is_some())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(footed, vec![1, 3], "each earlier reply has its footer");
    });
    // RC2-R1G3: the footers as the transcript draws them name g1.
    let drawn = drawn_footers(&view, window);
    assert_eq!(drawn.len(), 2, "two footers drawn: {drawn:?}");
    for text in &drawn {
        assert!(text.ends_with(" \u{b7} in g1"), "{text}");
    }
    // The note is drawn where the move happened; the sidebar row says so.
    assert!(
        window
            .debug_bounds(Box::leak(
                format!("{}-4", crate::work_folder_moves::MOVE_NOTE_SELECTOR).into_boxed_str()
            ))
            .is_some(),
        "the transcript shows the note"
    );
    assert!(
        (0..20).any(|ix| window
            .debug_bounds(Box::leak(
                format!("{}-{ix}", crate::work_folder_moves::SIDEBAR_MOVED_SELECTOR)
                    .into_boxed_str()
            ))
            .is_some()),
        "the sidebar row shows the move"
    );
    // The next turn runs in k3; its reply needs no folder word.
    view.update(window, |app, cx| {
        app.submit_work_run("say ok".into(), cx);
        assert_eq!(app.work.folder.as_deref(), Some(k3.as_path()));
        app.cancel_work_run(cx);
        let moves = app.folder_moves(&id);
        assert_eq!(crate::work_folder_moves::ran_in(moves, 5), None);
        assert_eq!(
            crate::work_folder_moves::sidebar_line(moves).as_deref(),
            Some("Moved from g1")
        );
    });
    window.run_until_parked();
    // A relaunch: the move is still there.
    let key = crate::work_folder_moves::moves_key(&id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while store.get_app_state(&key).unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "the move is saved");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let footers_key = crate::run_footer::footers_key(&id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while store
        .get_app_state(&footers_key)
        .unwrap()
        .is_none_or(|raw| crate::run_footer::parse_footers(&raw).len() < 3)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the three footers are saved"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    window.update(|window, _| window.remove_window());
    window.run_until_parked();
    let (again, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1400.), px(900.)));
    again.update_in(window2, |app, window, cx| {
        let moves = app.folder_moves(&id);
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].from, g1s);
        assert_eq!(
            crate::work_folder_moves::ran_in(moves, 1),
            Some(g1s.as_str())
        );
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Work, cx);
        app.select_conversation(&id, window, cx);
        assert_eq!(app.sidebar.active_id(), id);
    });
    window2.run_until_parked();
    // RC2-R1G3: after the relaunch the two earlier replies' footers still
    // name g1 as drawn; the reply that ran in k3 names no folder.
    let drawn = drawn_footers(&again, window2);
    assert_eq!(drawn.len(), 3, "three footers drawn: {drawn:?}");
    assert!(drawn[0].ends_with(" \u{b7} in g1"), "{}", drawn[0]);
    assert!(drawn[1].ends_with(" \u{b7} in g1"), "{}", drawn[1]);
    assert!(
        !drawn[2].contains(" in g1") && !drawn[2].contains(" in k3"),
        "{}",
        drawn[2]
    );
}

/// RC2-R1G3: the reply footers one fresh frame of `view` lays out.
fn drawn_footers(
    view: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
) -> Vec<String> {
    crate::run_footer::DRAWN_FOOTERS.with(|d| d.borrow_mut().clear());
    view.update(window, |_, cx| cx.notify());
    window.run_until_parked();
    crate::run_footer::drawn_footers()
}
/// RC2-R67 (usability RC2 round 5, Minor 4: ⌘O while the w1 conversation was
/// open moved it, with its w1 approvals, under c1 — "Moved from w1"). Through
/// ⌘O's own path (`use_folder`): a conversation with turns keeps its folder
/// and its rows, and a new conversation opens in the chosen folder, on the
/// same helper; a conversation with no turns yet simply takes the folder.
#[gpui::test]
fn rc2r67_open_folder_in_a_conversation_with_turns_opens_a_new_one(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work_sidebar::tests::rc2r67_open_folder_in_a_conversation_with_turns_opens_a_new_one",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        let w1 = work_app(app, &home, "w1");
        let c1 = home.join("c1");
        std::fs::create_dir_all(&c1).unwrap();
        app.work.folder = Some(w1.clone());
        app.new_conversation(window, cx);
        choose_claude(app, cx);
        let id = app.sidebar.active_id().to_string();
        app.submit_work_run("create notes.txt containing hi".into(), cx);
        app.cancel_work_run(cx);
        let rows = app.views[&id].convo.messages().len();
        assert!(rows > 0);

        app.use_folder(c1.to_str().unwrap(), cx);
        let fresh = app.sidebar.active_id().to_string();
        assert_ne!(fresh, id, "a new conversation opens");
        assert_eq!(
            cwd_of(app, &id).as_deref(),
            Some(w1.display().to_string().as_str()),
            "the w1 conversation stays under w1"
        );
        assert!(app.folder_moves(&id).is_empty(), "nothing was moved");
        assert_eq!(app.views[&id].convo.messages().len(), rows, "its rows stay");
        assert_eq!(
            cwd_of(app, &fresh).as_deref(),
            Some(c1.display().to_string().as_str())
        );
        assert_eq!(app.work.folder.as_deref(), Some(c1.as_path()));
        assert!(app.views[&fresh].convo.messages().is_empty());
        assert_eq!(
            app.composer_target_for(&crate::workshop_composer::ModelOwner::Work(fresh.clone())),
            Some(WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            }),
            "the helper carries on: the new thread starts on the one just used"
        );

        // No turns yet: the conversation on screen takes the folder.
        app.use_folder(w1.to_str().unwrap(), cx);
        assert_eq!(app.sidebar.active_id(), fresh, "the same conversation");
        assert_eq!(
            cwd_of(app, &fresh).as_deref(),
            Some(w1.display().to_string().as_str())
        );
    });
}

/// RC2-R612 (usability RC2 round 5, Minor 12: Gemma's footer said "changed
/// notes.txt"; Claude's and Codex's said nothing although Claude renamed
/// alpha.txt to beta.txt with `mv`). An agent's footer says what the folder
/// shows it changed (and what it removed); one that changed nothing says so.
#[gpui::test]
fn rc2r612_an_agent_footer_says_what_changed(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work_sidebar::tests::rc2r612_an_agent_footer_says_what_changed")
    else {
        return;
    };
    use gpui::AppContext as _;
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let folder = work_app(app, &home, "c1");
        std::fs::write(folder.join("alpha.txt"), "one\n").unwrap();
        std::fs::write(folder.join("keep.txt"), "same\n").unwrap();
        app.work.folder = Some(folder.clone());
        let conv = "r612-conv";
        let footer_of = |app: &mut AwiliApp,
                         cx: &mut gpui::Context<AwiliApp>,
                         change: &dyn Fn()| {
            app.views.insert(
                conv.into(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::seeded([
                    (Role::User, "rename alpha.txt to beta.txt".to_string()),
                    (Role::Assistant, "Done.".to_string()),
                ])),
            );
            app.work.conv_id = Some(conv.into());
            app.work.stream_row_idx = Some(1);
            app.work.graduation.arm(conv);
            app.work.run_seq += 1;
            app.work.running = true;
            app.work.run_started = Some(std::time::Instant::now());
            app.work.panel = Default::default();
            app.work.panel.requested_identity =
                Some(crate::work_run_identity::WorkRunIdentity::Agent {
                    engine: CodeEngine::Claude,
                    model: None,
                });
            // What the run start records.
            app.work.folder_stamps = crate::run_footer::dir_stamps(&folder);
            change();
            let run = app.work.run_seq;
            app.work_agent_run_done(run, &folder, cx);
            app.views[conv].rows[1]
                .footer
                .clone()
                .map(|f| f.to_string())
                .unwrap_or_default()
        };
        let renamed = footer_of(app, cx, &|| {
            std::fs::rename(folder.join("alpha.txt"), folder.join("beta.txt")).unwrap();
        });
        assert!(renamed.starts_with("Claude"), "{renamed}");
        assert!(
            renamed.ends_with(" \u{b7} changed beta.txt \u{b7} removed alpha.txt"),
            "{renamed}"
        );
        let nothing = footer_of(app, cx, &|| {});
        assert!(nothing.ends_with(" \u{b7} no files changed"), "{nothing}");
        let edited = footer_of(app, cx, &|| {
            std::fs::write(folder.join("keep.txt"), "same\nand two\n").unwrap();
        });
        assert!(edited.ends_with(" \u{b7} changed keep.txt"), "{edited}");
    });
}

/// RC2-R6M3 (RC2-R6 review P2 on 0fc8434b: an agent footer recorded only
/// the folder's top level, so a shell command that edited `src/file.rs`
/// read "no files changed"). A Claude run whose command edits a nested file
/// (no write step of its own names it) says so in its footer.
#[gpui::test]
fn rc2r6m3_an_agent_footer_names_a_nested_edit_made_by_a_command(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work_sidebar::tests::rc2r6m3_an_agent_footer_names_a_nested_edit_made_by_a_command",
    ) else {
        return;
    };
    use gpui::AppContext as _;
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let folder = work_app(app, &home, "c1");
        std::fs::create_dir_all(folder.join("src")).unwrap();
        std::fs::write(folder.join("src/file.rs"), "fn one() {}\n").unwrap();
        app.work.folder = Some(folder.clone());
        let conv = "r6m3-conv";
        app.views.insert(
            conv.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::seeded([
                (Role::User, "add fn two to src/file.rs".to_string()),
                (Role::Assistant, "Added it.".to_string()),
            ])),
        );
        let step = "printf 'fn two() {}\\n' >> src/file.rs";
        app.views.get_mut(conv).unwrap().rows[1].activity =
            vec![format!("\u{2713} Bash {step}").into()];
        app.work.conv_id = Some(conv.into());
        app.work.stream_row_idx = Some(1);
        app.work.graduation.arm(conv);
        app.work.run_seq += 1;
        app.work.running = true;
        app.work.run_started = Some(std::time::Instant::now());
        app.work.panel = Default::default();
        app.work.panel.requested_identity =
            Some(crate::work_run_identity::WorkRunIdentity::Agent {
                engine: CodeEngine::Claude,
                model: None,
            });
        // What the run start records.
        app.work.folder_stamps = crate::run_footer::dir_stamps(&folder);
        let ran = std::process::Command::new("sh")
            .args(["-c", step])
            .current_dir(&folder)
            .status()
            .unwrap();
        assert!(ran.success());
        let run = app.work.run_seq;
        app.work_agent_run_done(run, &folder, cx);
        let footer = app.views[conv].rows[1]
            .footer
            .clone()
            .map(|f| f.to_string())
            .unwrap_or_default();
        assert!(footer.starts_with("Claude"), "{footer}");
        assert!(!footer.contains("no files changed"), "{footer}");
        assert!(footer.ends_with(" \u{b7} changed src/file.rs"), "{footer}");
    });
}

/// RC2-R6M6 (RC2-R6 review P3 on 9c0fa36d: a command that changed only a
/// file inside `.git` got "no files changed", though `.git` is not looked
/// inside). The footer says only what it saw: "no files changed outside
/// .git".
#[gpui::test]
fn rc2r6m6_an_agent_footer_claims_no_more_than_it_saw(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work_sidebar::tests::rc2r6m6_an_agent_footer_claims_no_more_than_it_saw")
    else {
        return;
    };
    use gpui::AppContext as _;
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let folder = work_app(app, &home, "c1");
        std::fs::create_dir_all(folder.join(".git")).unwrap();
        std::fs::write(folder.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(folder.join("notes.txt"), "one\n").unwrap();
        app.work.folder = Some(folder.clone());
        let conv = "r6m6-conv";
        app.views.insert(
            conv.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::seeded([
                (Role::User, "switch to a new branch".to_string()),
                (Role::Assistant, "Switched.".to_string()),
            ])),
        );
        let step = "printf 'ref: refs/heads/feature-x\\n' > .git/HEAD";
        app.views.get_mut(conv).unwrap().rows[1].activity =
            vec![format!("\u{2713} Bash {step}").into()];
        app.work.conv_id = Some(conv.into());
        app.work.stream_row_idx = Some(1);
        app.work.graduation.arm(conv);
        app.work.run_seq += 1;
        app.work.running = true;
        app.work.run_started = Some(std::time::Instant::now());
        app.work.panel = Default::default();
        app.work.panel.requested_identity =
            Some(crate::work_run_identity::WorkRunIdentity::Agent {
                engine: CodeEngine::Claude,
                model: None,
            });
        // What the run start records.
        app.work.folder_stamps = crate::run_footer::dir_stamps(&folder);
        let ran = std::process::Command::new("sh")
            .args(["-c", step])
            .current_dir(&folder)
            .status()
            .unwrap();
        assert!(ran.success());
        let run = app.work.run_seq;
        app.work_agent_run_done(run, &folder, cx);
        let footer = app.views[conv].rows[1]
            .footer
            .clone()
            .map(|f| f.to_string())
            .unwrap_or_default();
        assert!(
            footer.ends_with(" \u{b7} no files changed outside .git"),
            "{footer}"
        );
    });
}

#[cfg(unix)]
/// RC2-R6M8 (RC2-R6 review P3 on 0dd1a343: a directory reached through a
/// symbolic link is not walked, yet a command that edited a file in it got
/// "no files changed"). The footer makes no such claim for a folder it did
/// not see whole.
#[gpui::test]
fn rc2r6m8_an_agent_footer_makes_no_claim_past_a_linked_directory(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work_sidebar::tests::rc2r6m8_an_agent_footer_makes_no_claim_past_a_linked_directory",
    ) else {
        return;
    };
    use gpui::AppContext as _;
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let folder = work_app(app, &home, "c1");
        let shared = home.join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("lib.rs"), "fn one() {}\n").unwrap();
        std::os::unix::fs::symlink(&shared, folder.join("shared")).unwrap();
        app.work.folder = Some(folder.clone());
        let conv = "r6m8-conv";
        app.views.insert(
            conv.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::seeded([
                (Role::User, "add fn two to shared/lib.rs".to_string()),
                (Role::Assistant, "Added it.".to_string()),
            ])),
        );
        let step = "printf 'fn two() {}\\n' >> shared/lib.rs";
        app.views.get_mut(conv).unwrap().rows[1].activity =
            vec![format!("\u{2713} Bash {step}").into()];
        app.work.conv_id = Some(conv.into());
        app.work.stream_row_idx = Some(1);
        app.work.graduation.arm(conv);
        app.work.run_seq += 1;
        app.work.running = true;
        app.work.run_started = Some(std::time::Instant::now());
        app.work.panel = Default::default();
        app.work.panel.requested_identity =
            Some(crate::work_run_identity::WorkRunIdentity::Agent {
                engine: CodeEngine::Claude,
                model: None,
            });
        // What the run start records.
        app.work.folder_stamps = crate::run_footer::dir_stamps(&folder);
        let ran = std::process::Command::new("sh")
            .args(["-c", step])
            .current_dir(&folder)
            .status()
            .unwrap();
        assert!(ran.success());
        let run = app.work.run_seq;
        app.work_agent_run_done(run, &folder, cx);
        let footer = app.views[conv].rows[1]
            .footer
            .clone()
            .map(|f| f.to_string())
            .unwrap_or_default();
        assert!(footer.starts_with("Claude"), "{footer}");
        assert!(!footer.contains("no files changed"), "{footer}");
    });
}

/// RC2-R6M12 (RC2-R6 review P2 on 4a02953f: only a text draft kept a
/// never-sent conversation from being the "new" one, so one with a file
/// staged was reused by "+ New conversation", or re-filed with its file by
/// RC2-R67's new conversation when another folder was chosen). A
/// never-sent Work conversation with a file staged is never reused or
/// re-filed: "+ New conversation" and choosing another folder from a
/// conversation with turns both open another one, and the file stays where
/// it was staged.
#[gpui::test]
fn rc2r6m12_a_conversation_with_a_staged_file_is_never_the_new_one(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work_sidebar::tests::rc2r6m12_a_conversation_with_a_staged_file_is_never_the_new_one",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        let w1 = work_app(app, &home, "w1");
        let c1 = home.join("c1");
        std::fs::create_dir_all(&c1).unwrap();
        app.work.folder = Some(w1.clone());
        // A never-sent Work conversation, its box empty, a file staged.
        app.new_conversation(window, cx);
        let staged = app.sidebar.active_id().to_string();
        // Staging needs the conversation's database row (foreign key).
        let store = awili_persist::Store::open_default().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !store
            .conversations()
            .unwrap()
            .iter()
            .any(|c| c.id == staged)
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the conversation row is saved"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.attach_bytes("taxes.txt", "text/plain", b"keep\n", cx);
        assert_eq!(app.staged_attachments.len(), 1, "the file is in the tray");
        app.sync_composer_draft(cx);
        let file_kept = |app: &AwiliApp| {
            app.search_store
                .as_ref()
                .unwrap()
                .get_staged_attachments(&staged)
                .unwrap()
                .len()
                == 1
        };

        // "+ New conversation": another one.
        app.new_conversation(window, cx);
        let fresh = app.sidebar.active_id().to_string();
        assert_ne!(fresh, staged, "+ New conversation does not reuse it");
        assert!(app.staged_attachments.is_empty(), "the new one's tray");
        assert!(file_kept(app));

        // Another folder chosen from a conversation with turns: another one,
        // and the staged one stays under w1.
        app.submit_work_run("create notes.txt containing hi".into(), cx);
        app.cancel_work_run(cx);
        assert!(!app.views[&fresh].convo.messages().is_empty());
        app.use_folder(c1.to_str().unwrap(), cx);
        let opened = app.sidebar.active_id().to_string();
        assert_ne!(opened, fresh, "a new conversation opens");
        assert_ne!(opened, staged, "not the one with a file staged");
        assert_eq!(
            cwd_of(app, &staged).as_deref(),
            Some(w1.display().to_string().as_str()),
            "the staged one is not re-filed"
        );
        assert_eq!(
            cwd_of(app, &opened).as_deref(),
            Some(c1.display().to_string().as_str())
        );
        assert!(app.staged_attachments.is_empty());
        assert!(file_kept(app));
    });
}

/// S1b: deleting a conversation removes its engine log (`logs/<id>.log`
/// under the engine data directory); a conversation with no log deletes the
/// same, and a log still held by a worker goes once the worker lets go.
#[gpui::test]
fn s1b_deleting_a_conversation_removes_its_engine_log(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work_sidebar::tests::s1b_deleting_a_conversation_removes_its_engine_log")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    let root = dirs::data_dir()
        .unwrap()
        .join(awili_config::APP_DIR_NAME)
        .join("engine");
    assert!(
        root.starts_with(&home),
        "the engine data is under the private HOME: {}",
        root.display()
    );
    let logs = root.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let (id, other) = view.update_in(window, |app, window, cx| {
        let folder = work_app(app, &home, "s1b");
        app.work.folder = Some(folder);
        app.new_conversation(window, cx);
        (app.sidebar.active_id().to_string(), "bystander".to_string())
    });
    let log = crate::work::engine_log_path(&root, &id);
    let kept = crate::work::engine_log_path(&root, &other);
    std::fs::write(&log, "{}\n").unwrap();
    std::fs::write(&kept, "{}\n").unwrap();
    // Held like a live worker holds it: the delete waits for the lock.
    let holder = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    holder.lock().unwrap();
    view.update_in(window, |app, window, cx| {
        app.delete_conversation(&id, window, cx)
    });
    window.run_until_parked();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(log.exists(), "a log a worker still holds stays");
    drop(holder);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while log.exists() {
        assert!(std::time::Instant::now() < deadline, "the log is removed");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(kept.exists(), "another conversation's log is untouched");
    assert!(crate::work::remove_engine_log(
        &log,
        std::time::Duration::from_millis(10)
    ));
}

/// S1b review LOW 5: conversation ids are minted as UUID v4
/// (`awili_core::convs::new_conversation_id`), every character of which the
/// log file name keeps, so two ids never share an engine log path.
#[test]
fn s1b_minted_conversation_ids_map_to_distinct_log_paths() {
    let root = std::path::Path::new("/engine");
    let a = awili_core::convs::new_conversation_id();
    let b = awili_core::convs::new_conversation_id();
    assert_ne!(a, b);
    for id in [&a, &b] {
        assert_eq!(
            crate::work::engine_log_path(root, id),
            root.join("logs").join(format!("{id}.log")),
            "a minted id is kept whole in its file name"
        );
    }
    assert_ne!(
        crate::work::engine_log_path(root, &a),
        crate::work::engine_log_path(root, &b)
    );
}
