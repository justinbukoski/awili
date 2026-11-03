//! U1d required proof (offline): the sidebar kind, the tab lookups, the blank-
//! tab reuse rule, the reopen plan and the `apply_reopen` restore.

use std::sync::{Arc, Mutex};

use awili_code::CodeEngine;
use awili_core::{ConvKind, ConversationState, Message, MessageKind, Role};

use super::*;

/// Build a `CodeSession` with the given conversation id, row count and
/// streaming flag. The `MessageRow` values are placeholders; `apply_reopen`
/// rebuilds them from the conversation.
fn session(conv_id: Option<&str>, rows: usize, streaming: bool) -> CodeSession {
    let dummy = Message {
        role: Role::User,
        text: String::new(),
        kind: MessageKind::Normal,
        ts: 0,
        provenance: None,
        superseded_by: None,
    };
    let mut s = CodeSession {
        id: 0,
        engine: CodeEngine::Claude,
        route: crate::conversation_route::ConversationRoute::new(WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        }),
        convo: ConversationState::default(),
        reply_presentation_token: crate::conv_view::TurnToken::fresh(),
        rows: (0..rows).map(|_| crate::MessageRow::new(&dummy)).collect(),
        errored: std::collections::HashSet::new(),
        streaming,
        context_gap: false,
        session_slot: Arc::new(Mutex::new(None)),
        project_folder: None,
        model: None,
        scroll: gpui::ScrollHandle::new(),
        retry_focus: None,
        retry_reveal_key: std::rc::Rc::default(),
        retry_bounds: std::rc::Rc::new(std::cell::Cell::new(None)),
        stream_abort_tx: None,
        history: Default::default(),
    };
    s.history.conv_id = conv_id.map(str::to_string);
    s
}

/// U1d: the sidebar kind follows the mode — Code mode gets the Code list,
/// Work mode the Work list, and every other variant the Work list.
#[test]
fn u1d_sidebar_kind_follows_mode() {
    assert_eq!(sidebar_kind_for(Mode::Code), ConvKind::Code);
    assert_eq!(sidebar_kind_for(Mode::Work), ConvKind::Work);
    assert_eq!(sidebar_kind_for(Mode::Skills), ConvKind::Work);
    assert_eq!(sidebar_kind_for(Mode::Cron), ConvKind::Work);
    assert_eq!(sidebar_kind_for(Mode::Sessions), ConvKind::Work);
    assert_eq!(sidebar_kind_for(Mode::CommandCenter), ConvKind::Work);
    assert_eq!(sidebar_kind_for(Mode::Messaging), ConvKind::Work);
}

/// U1d: `reopen_plan` reads the engine, model, folder and session out of the
/// recorded meta.
#[test]
fn u1d_reopen_plan_reads_meta() {
    let meta = CodeConvMeta::for_turn(
        CodeEngine::Glm,
        Some("glm-5".into()),
        Some("/p".into()),
        Some("s1".into()),
    );
    let plan = reopen_plan(Some(&meta.to_json()));
    assert_eq!(
        plan,
        ReopenPlan {
            engine: CodeEngine::Glm,
            model: Some("glm-5".into()),
            folder: Some("/p".into()),
            session: Some("s1".into()),
            engine_known: true,
            route: ConversationRoute::from_legacy(
                WorkExecutor::Agent {
                    engine: CodeEngine::Glm,
                    model: Some("glm-5".into()),
                },
                Some("/p".into()),
                Some("s1".into()),
                awili_code::continuity::TranscriptCursor(0),
            ),
        }
    );
}

/// U1d: missing or unusable meta (absent value, malformed JSON, a gateway
/// executor) gives the Claude fallback with nothing else and
/// `engine_known: false`.
#[test]
fn u1d_reopen_plan_defaults_when_missing() {
    let fallback = ReopenPlan {
        engine: CodeEngine::Claude,
        model: None,
        folder: None,
        session: None,
        engine_known: false,
        route: ConversationRoute::new(WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        }),
    };
    assert_eq!(reopen_plan(None), fallback);
    assert_eq!(reopen_plan(Some("{")), fallback);
    let gateway = serde_json::json!({ "executor": "gateway", "folder": null, "session": null });
    assert_eq!(reopen_plan(Some(&gateway.to_string())), fallback);
}

/// U1d: `apply_reopen` restores the transcript, the rebuilt rows, the engine,
/// the model pin, the project folder and the resume slot, binds the history
/// and clears the streaming and error state.
#[test]
fn u1d_apply_reopen_restores_transcript_and_resume() {
    let convo = ConversationState::seeded([
        (Role::User, "q".to_string()),
        (Role::Assistant, "a".to_string()),
    ]);
    let plan_glm = reopen_plan(Some(
        &CodeConvMeta::for_turn(
            CodeEngine::Glm,
            Some("glm-5".into()),
            Some("/p".into()),
            Some("s1".into()),
        )
        .to_json(),
    ));
    let mut s = session(None, 0, false);
    s.engine = CodeEngine::Codex; // a different engine the reopen must override
    s.streaming = true;
    // U1d-FIX1 (Codex P2): a prior error mark the reopen must clear.
    s.errored.insert(1);
    apply_reopen(&mut s, "c1", convo, &plan_glm);
    assert_eq!(s.rows.len(), 2, "rows rebuilt from the transcript");
    let texts: Vec<_> = s.convo.messages().iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, ["q", "a"], "transcript restored");
    assert_eq!(s.engine, CodeEngine::Glm);
    assert_eq!(s.model.as_deref(), Some("glm-5"), "model pin restored");
    assert!(s.errored.is_empty(), "error marks cleared");
    assert_eq!(s.project_folder.as_deref(), Some("/p"));
    assert_eq!(
        s.session_slot.lock().unwrap().as_deref(),
        Some("s1"),
        "resume session restored"
    );
    assert_eq!(s.history.conv_id.as_deref(), Some("c1"));
    assert!(s.history.turn.is_none());
    assert!(!s.streaming);
}

/// U1d: `tab_for_conv` finds the tab carrying `c1` and not others.
/// `reusable_blank_tab` returns the active index only when that tab has no
/// rows, is not streaming and has no conversation.
#[test]
fn u1d_tab_lookup_and_blank_reuse() {
    let a = session(None, 0, false);
    let b = session(Some("c1"), 2, false);
    let c = session(Some("c2"), 0, true); // streaming: not reusable
    let tabs = vec![a, b, c];
    assert_eq!(tab_for_conv(&tabs, "c1"), Some(1));
    assert_eq!(tab_for_conv(&tabs, "c2"), Some(2));
    assert_eq!(tab_for_conv(&tabs, "zz"), None);

    // The blank tab is reusable when it is the active one...
    assert_eq!(reusable_blank_tab(&tabs, 0), Some(0));
    // ...but a streaming or already-bound tab at the active index is not, and
    // so is an index out of range.
    assert_eq!(reusable_blank_tab(&tabs, 2), None);
    assert_eq!(reusable_blank_tab(&tabs, 1), None);
    assert_eq!(reusable_blank_tab(&tabs, 7), None);
}

/// U1d-FIX1 (Codex P2): a `code_conv` reply applies only while the app is in
/// Code mode and the requested conversation is still the selected one.
#[test]
fn u1d_fix_reply_is_current_only_for_the_selection_in_code_mode() {
    assert!(reopen_reply_is_current("a", Mode::Code, "a"));
    assert!(!reopen_reply_is_current("a", Mode::Code, "b"));
    assert!(!reopen_reply_is_current("a", Mode::Work, "a"));
}

// ---------------------------------------------------------------------------
// U1d-FIX1 (Codex P2): app-level proof of the stale-reply guard. Each test
// builds the real `AwiliApp` (`AwiliApp::new` in a subprocess with a private
// HOME, the `stream_turn_tests` isolation) with its own persistence worker,
// so the `code_conv` meta read really goes through the worker thread and
// comes back through `cx.spawn`. `allow_parking` lets that thread wake the
// gpui test scheduler.
// ---------------------------------------------------------------------------

use gpui::AppContext;

const FIX_CHILD: &str = "AWILI_U1D_FIX1_TEST_HOME";

/// Run `test` in a subprocess with a private HOME. `true` inside the child
/// (run the body); `false` in the parent once the child passed.
fn in_isolated_child(test: &str) -> bool {
    if std::env::var_os(FIX_CHILD).is_some() {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(FIX_CHILD, home.path())
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

/// A fresh app in Chat mode whose persistence is a worker on a temp store,
/// with two Code conversations `(a, b)` in its list.
fn reopen_app(cx: &mut gpui::TestAppContext) -> (gpui::Entity<crate::AwiliApp>, String, String) {
    cx.executor().allow_parking();
    let home = std::path::PathBuf::from(std::env::var_os(FIX_CHILD).unwrap());
    let store = awili_persist::Store::open(&home.join("u1d-fix1.db")).unwrap();
    let (persist, _worker) = crate::persistence::Persistence::spawn_worker(store);
    let entity = cx.new(crate::AwiliApp::new);
    let (a, b) = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = Some(persist);
        app.set_mode(Mode::Code, cx);
        let a = app.conversations.create_kind(1, ConvKind::Code).id;
        let b = app.conversations.create_kind(2, ConvKind::Code).id;
        (a, b)
    });
    (entity, a, b)
}

/// Wait until every `code_conv` read queued so far has been answered and its
/// reply task has run. The worker answers in order, so once this probe's
/// reply is back, every earlier reply was already sent (and its task woken);
/// `run_until_parked` then runs those tasks.
fn drain_reopen_replies(cx: &mut gpui::TestAppContext, entity: &gpui::Entity<crate::AwiliApp>) {
    let (tx, rx) = async_channel::bounded(1);
    entity.update(cx, |app, _cx| {
        app.persist.as_ref().expect("the test's worker").send(
            crate::persistence::PersistCmd::ReadState {
                key: "u1d-fix1-probe".into(),
                reply: tx,
            },
        );
    });
    assert_eq!(rx.recv_blocking(), Ok(None));
    cx.run_until_parked();
}

/// Opening A and then B from the Sessions browser before A's meta arrives:
/// A's late reply is dropped, and only B gets a tab, focused and selected.
#[gpui::test]
fn u1d_fix_stale_reply_after_selecting_another_is_dropped(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "code_reopen::code_reopen_tests::u1d_fix_stale_reply_after_selecting_another_is_dropped",
    ) {
        return;
    }
    let (entity, a, b) = reopen_app(cx);
    entity.update(cx, |app, cx| {
        app.open_conversation_from_history(&a, cx);
        app.open_conversation_from_history(&b, cx);
        assert!(
            tab_for_conv(&app.work.code_tabs, &a).is_none(),
            "no tab before the reply"
        );
    });
    drain_reopen_replies(cx, &entity);
    entity.update(cx, |app, _cx| {
        assert_eq!(app.mode, Mode::Code);
        assert_eq!(app.sidebar.active_id(), b);
        assert_eq!(
            tab_for_conv(&app.work.code_tabs, &a),
            None,
            "the stale reply for A opened no tab"
        );
        let ix = tab_for_conv(&app.work.code_tabs, &b).expect("B's reply opened its tab");
        assert_eq!(app.work.code_active, ix, "B is the focused tab");
    });
}

/// Opening A and then leaving Code mode before A's meta arrives: the late
/// reply is dropped (no tab, the new mode kept). Opening A again while in Code
/// mode then works (positive control).
#[gpui::test]
fn u1d_fix_stale_reply_after_leaving_code_mode_is_dropped(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "code_reopen::code_reopen_tests::u1d_fix_stale_reply_after_leaving_code_mode_is_dropped",
    ) {
        return;
    }
    let (entity, a, _b) = reopen_app(cx);
    entity.update(cx, |app, cx| {
        app.open_conversation_from_history(&a, cx);
        assert_eq!(app.mode, Mode::Code);
        // A utility mode keeps the selection, so only the mode check can
        // drop this reply.
        app.set_mode(Mode::Skills, cx);
        assert_eq!(app.sidebar.active_id(), a, "the selection is still A");
    });
    drain_reopen_replies(cx, &entity);
    entity.update(cx, |app, cx| {
        assert_eq!(
            app.mode,
            Mode::Skills,
            "the late reply did not pull the user back"
        );
        assert_eq!(
            tab_for_conv(&app.work.code_tabs, &a),
            None,
            "the stale reply for A opened no tab"
        );
        app.open_conversation_from_history(&a, cx);
    });
    drain_reopen_replies(cx, &entity);
    entity.update(cx, |app, _cx| {
        assert_eq!(app.mode, Mode::Code);
        let ix = tab_for_conv(&app.work.code_tabs, &a).expect("a current reply opens the tab");
        assert_eq!(app.work.code_active, ix);
    });
}

/// A Code-mode app with acknowledged project saves and two real folders
/// `(x, y)` and one tab that has already run in `x`.
fn code_tabs_app(cx: &mut gpui::TestAppContext) -> (gpui::Entity<crate::AwiliApp>, String, String) {
    cx.executor().allow_parking();
    let home = std::path::PathBuf::from(std::env::var_os(FIX_CHILD).unwrap());
    let (x, y) = (home.join("proj-x"), home.join("proj-y"));
    std::fs::create_dir_all(&x).unwrap();
    std::fs::create_dir_all(&y).unwrap();
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Code, cx);
        app.add_code_project(x.clone(), cx);
        let tab = &mut app.work.code_tabs[app.work.code_active];
        assert_eq!(tab.project_folder.as_deref(), Some(x.to_str().unwrap()));
        tab.convo.push(Role::User, "fix the build");
        let row = crate::MessageRow::new(tab.convo.message(0).unwrap());
        tab.rows.push(row);
    });
    let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    loop {
        cx.run_until_parked();
        if entity.read_with(cx, |app, _| {
            app.work
                .projects
                .iter()
                .any(|p| p.folder == x.display().to_string())
        }) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "project save was not acknowledged"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    (entity, x.display().to_string(), y.display().to_string())
}

fn tab_folders(app: &crate::AwiliApp) -> Vec<Option<String>> {
    app.work
        .code_tabs
        .iter()
        .map(|t| t.project_folder.clone())
        .collect()
}

/// R2-17 (round 2 Minor 8): ⌘O in Code opens the folder in the tab on
/// screen, and a tab that has not run yet keeps the project picked in it —
/// switching to another tab and back no longer returns to that tab's project.
#[gpui::test]
fn rc2ux_a_blank_code_tab_keeps_the_project_picked_in_it(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "code_reopen::code_reopen_tests::rc2ux_a_blank_code_tab_keeps_the_project_picked_in_it",
    ) {
        return;
    }
    let (entity, x, y) = code_tabs_app(cx);
    entity.update(cx, |app, cx| {
        app.new_code_tab_like_active(cx); // "+ New conversation" in Code
        assert_eq!(app.work.code_tabs.len(), 2);
        assert_eq!(app.work.code_active, 1);
        assert_eq!(
            tab_folders(app)[1].as_deref(),
            Some(x.as_str()),
            "a new tab starts in the project"
        );
        app.use_folder(&y, cx); // ⌘O
        assert_eq!(app.work.code_tabs.len(), 2, "no extra tab");
        assert_eq!(tab_folders(app), vec![Some(x.clone()), Some(y.clone())]);
        assert_eq!(app.code_cwd().display().to_string(), y);
        app.activate_code_tab(0);
        assert_eq!(app.code_cwd().display().to_string(), x);
        app.activate_code_tab(1);
        assert_eq!(
            app.code_cwd().display().to_string(),
            y,
            "the blank tab kept y"
        );
    });
}

/// R2-25 / D-R1 M9c: "+ New conversation" on a tab that has not run yet
/// keeps it (no pile of empty tabs); a new project picked on a tab that has
/// already worked in another folder opens in a fresh tab, so the old
/// transcript never shows under the new folder name.
#[gpui::test]
fn rc2ux_new_code_tabs_only_when_they_are_needed(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "code_reopen::code_reopen_tests::rc2ux_new_code_tabs_only_when_they_are_needed",
    ) {
        return;
    }
    let (entity, x, y) = code_tabs_app(cx);
    entity.update(cx, |app, cx| {
        app.new_code_tab_like_active(cx); // "+ New conversation" in Code
        app.new_code_tab_like_active(cx); // "+ New conversation" in Code
        app.new_code_tab_like_active(cx); // "+ New conversation" in Code
        assert_eq!(
            app.work.code_tabs.len(),
            2,
            "one new tab, however often + New is clicked"
        );
        app.activate_code_tab(0);
        app.use_folder(&y, cx);
        assert_eq!(app.work.code_tabs.len(), 2, "the blank tab is reused");
        assert_eq!(app.work.code_active, 1);
        assert_eq!(tab_folders(app), vec![Some(x.clone()), Some(y.clone())]);
        assert_eq!(
            app.work.code_tabs[0].convo.messages()[0].text,
            "fix the build",
            "the first tab keeps its transcript and folder"
        );
        // From a tab that has worked in y, picking x opens a fresh tab.
        let tab = &mut app.work.code_tabs[1];
        tab.convo.push(Role::User, "and the tests");
        let row = crate::MessageRow::new(tab.convo.message(0).unwrap());
        tab.rows.push(row);
        let x_ix = app
            .work
            .projects
            .iter()
            .position(|p| p.folder == x)
            .unwrap();
        app.switch_code_project(x_ix);
        assert_eq!(app.work.code_tabs.len(), 3);
        assert_eq!(app.work.code_active, 2);
        assert_eq!(tab_folders(app)[2].as_deref(), Some(x.as_str()));
        assert!(app.work.code_tabs[2].rows.is_empty());
    });
}

/// R2-17 (round 2 Minor 8: "Code tabs are not restored after relaunch"):
/// the tabs open at the last close come back after a relaunch — a tab that
/// ran with its transcript, helper and project, a tab that had not run yet
/// on its project — with the same tab on screen.
#[gpui::test]
fn rc2ux_code_tabs_come_back_after_a_relaunch(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "code_reopen::code_reopen_tests::rc2ux_code_tabs_come_back_after_a_relaunch",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let home = std::path::PathBuf::from(std::env::var_os(FIX_CHILD).unwrap());
    let (x, y) = (home.join("proj-x"), home.join("proj-y"));
    std::fs::create_dir_all(&x).unwrap();
    std::fs::create_dir_all(&y).unwrap();
    let (view, window) = cx.add_window_view(|_w, cx| crate::AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
        app.set_mode(Mode::Code, cx);
        app.add_code_project(x.clone(), cx);
        app.submit_code_turn("fix the build".into(), cx);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while view.read_with(window, |app, _| app.work.code_tabs[0].streaming) {
        window.run_until_parked();
        assert!(std::time::Instant::now() < deadline, "the turn never ended");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (conv, want) = view.update(window, |app, cx| {
        app.new_code_tab_like_active(cx);
        app.use_folder(&y.display().to_string(), cx);
        cx.notify();
        let conv = app.work.code_tabs[0].history.conv_id.clone().unwrap();
        (conv, app.code_tabs_snapshot())
    });
    window.run_until_parked();
    assert_eq!(want.tabs.len(), 2);
    assert_eq!(want.active, 1);
    let store = awili_persist::Store::open_default().unwrap();
    let want_json = view.read_with(window, |app, _| {
        serde_json::to_string(&app.workshop_snapshot()).unwrap()
    });
    while store
        .get_app_state(crate::workshop::TABS_KEY)
        .unwrap()
        .as_deref()
        != Some(want_json.as_str())
        || store
            .get_app_state(&crate::code_history::code_conv_key(&conv))
            .unwrap()
            .is_none()
    {
        window.run_until_parked();
        assert!(
            std::time::Instant::now() < deadline,
            "the tabs were never saved"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let relaunched = cx.new(crate::AwiliApp::new);
    relaunched.read_with(cx, |app, _| {
        let tabs = &app.work.code_tabs;
        assert_eq!(tabs.len(), 2, "both tabs are back");
        assert_eq!(tabs[0].history.conv_id.as_deref(), Some(conv.as_str()));
        assert_eq!(tabs[0].convo.messages()[0].text, "fix the build");
        assert_eq!(tabs[0].engine, CodeEngine::Claude);
        assert_eq!(tabs[0].project_folder.as_deref(), Some(x.to_str().unwrap()));
        assert!(tabs[1].rows.is_empty());
        assert_eq!(tabs[1].project_folder.as_deref(), Some(y.to_str().unwrap()));
        assert_eq!(app.work.code_active, 1, "the same tab is on screen");
    });
}

/// RC2-FIXB2 (RC2-UX review P2: a Code tab lost its helper and folder when
/// Awili relaunched during its first turn): the conversation's helper and
/// folder are saved when a turn ends, so a relaunch in the middle of the
/// first turn has none; the tab comes back on the helper, model and project
/// it was saved with, with its conversation.
#[gpui::test]
fn rc2fixb_a_code_tab_relaunched_mid_turn_keeps_its_helper_and_folder(
    cx: &mut gpui::TestAppContext,
) {
    if !in_isolated_child(
        "code_reopen::code_reopen_tests::rc2fixb_a_code_tab_relaunched_mid_turn_keeps_its_helper_and_folder",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let home = std::path::PathBuf::from(std::env::var_os(FIX_CHILD).unwrap());
    let x = home.join("proj-x");
    std::fs::create_dir_all(&x).unwrap();
    let entity = cx.new(crate::AwiliApp::new);
    let conv = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Code, cx);
        app.add_code_project(x.clone(), cx);
        let ix = app.work.code_active;
        let tab = &mut app.work.code_tabs[ix];
        tab.engine = CodeEngine::Codex;
        tab.model = Some("gpt-test".into());
        // The turn has started (the conversation exists, the question is
        // saved) and has not ended: no helper and folder saved for it yet.
        tab.convo.push(Role::User, "fix the build");
        tab.convo.push(Role::Assistant, "");
        let id = tab.id;
        app.code_turn_begin(id, "fix the build", 1);
        app.save_code_tabs_if_changed();
        app.work.code_tabs[ix].history.conv_id.clone().unwrap()
    });
    let store = awili_persist::Store::open_default().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !store
        .get_app_state(CODE_OPEN_TABS_KEY)
        .unwrap()
        .is_some_and(|j| j.contains(&conv))
    {
        cx.run_until_parked();
        assert!(
            std::time::Instant::now() < deadline,
            "the tabs were never saved"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        store
            .get_app_state(&crate::code_history::code_conv_key(&conv))
            .unwrap()
            .is_none(),
        "mid-turn: the conversation's helper and folder are not saved yet"
    );

    let relaunched = cx.new(crate::AwiliApp::new);
    relaunched.read_with(cx, |app, _| {
        let tabs = &app.work.code_tabs;
        assert_eq!(tabs.len(), 1, "the tab is back");
        assert_eq!(tabs[0].history.conv_id.as_deref(), Some(conv.as_str()));
        assert_eq!(tabs[0].convo.messages()[0].text, "fix the build");
        assert_eq!(tabs[0].engine, CodeEngine::Codex, "on its helper");
        assert_eq!(tabs[0].model.as_deref(), Some("gpt-test"));
        assert_eq!(
            tabs[0].project_folder.as_deref(),
            Some(x.to_str().unwrap()),
            "in its project"
        );
    });
}

#[gpui::test]
fn workshop_delayed_code_reopen_cannot_replace_selected_blank_tab(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("code_reopen::code_reopen_tests::workshop_delayed_code_reopen_cannot_replace_selected_blank_tab") { return; }
    let (entity, requested, _) = reopen_app(cx);
    let selected = entity.update(cx, |app, cx| {
        app.open_conversation_from_history(&requested, cx);
        app.new_workshop_with(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Codex,
                model: None,
            },
            cx,
        );
        assert_eq!(
            app.sidebar.active_id(),
            requested,
            "blank tab has no durable ID to replace sidebar selection"
        );
        app.work.code_tabs[app.work.code_active].id
    });
    drain_reopen_replies(cx, &entity);
    entity.read_with(cx, |app, _| {
        assert!(
            tab_for_conv(&app.work.code_tabs, &requested).is_none(),
            "stale requested history did not open"
        );
        let active = &app.work.code_tabs[app.work.code_active];
        assert_eq!(active.id, selected);
        assert_eq!(active.engine, awili_code::CodeEngine::Codex);
        assert!(active.history.conv_id.is_none());
        assert!(active.convo.messages().is_empty());
    });
}

#[gpui::test]
fn workshop_new_action_invalidates_reopen_even_when_reusing_blank(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("code_reopen::code_reopen_tests::workshop_new_action_invalidates_reopen_even_when_reusing_blank") {
        return;
    }
    cx.executor().allow_parking();
    let home = std::path::PathBuf::from(std::env::var_os(FIX_CHILD).unwrap());
    let store = awili_persist::Store::open(&home.join("new-action.db")).unwrap();
    let (persist, _worker) = crate::persistence::Persistence::spawn_worker(store);
    let (view, window) = cx.add_window_view(|_, cx| crate::AwiliApp::new(cx));
    let (requested, selected, rx) = view.update_in(window, |app, window, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = Some(persist);
        app.set_mode(Mode::Code, cx);
        let selected = app.work.code_tabs[app.work.code_active].id;
        let requested = app.conversations.create_kind(1, ConvKind::Code).id;
        app.open_conversation_from_history(&requested, cx);
        app.new_conversation(window, cx);
        assert_eq!(app.work.code_tabs.len(), 1, "New reuses the blank adapter");
        let (tx, rx) = async_channel::bounded(1);
        app.persist
            .as_ref()
            .unwrap()
            .send(crate::PersistCmd::ReadState {
                key: "new-action-barrier".into(),
                reply: tx,
            });
        (requested, selected, rx)
    });
    assert_eq!(rx.recv_blocking(), Ok(None));
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(tab_for_conv(&app.work.code_tabs, &requested).is_none());
        let active = &app.work.code_tabs[app.work.code_active];
        assert_eq!(active.id, selected);
        assert!(active.history.conv_id.is_none());
        assert!(active.convo.messages().is_empty());
    });
}
