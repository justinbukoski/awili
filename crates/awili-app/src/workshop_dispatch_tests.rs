//! Workshop has ONE tool-capable path per thread. These tests send real,
//! context-dependent utterances through the composer's own dispatch
//! (`submit_composer_draft` → `dispatch_composer_text`) to a fake `claude`
//! that records its prompt, with no folder chosen. There is no classifier to
//! satisfy: a bare "continue" or a plain question takes the same route as a
//! task, and nothing answers "answer-only turn" or "Run as a job".
//!
//! History compatibility is proven from a real database row: a thread the
//! removed Chat wrote (stored kind `chat`) restores as a Workshop thread, its
//! rows intact, and its old refusal sentence is not replayed to the helper.
//! Each test runs in a private-HOME subprocess (`isolated`).

use super::gateway_queue_tests::isolated;
use super::*;
use std::time::Instant;

const LEGACY: &str = "legacy-chat";

/// Real utterances whose meaning depends on the thread before them, plus a
/// plain question about the app itself.
const UTTERANCES: [&str; 5] = [
    "continue",
    "can you fix it please?",
    "take a look at where we are at now",
    "ask for authorization",
    "why isn't codex or claude code or deepseek listed?",
];

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn fake_claude(dir: &std::path::Path, hold: bool) -> std::path::PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` in `dir`: touches `ran`, writes its LAST argv (the prompt)
/// to `prompt`, then either answers one stream-json turn or (`hold`) stays
/// busy until it is killed.
fn fake_claude(dir: &std::path::Path, hold: bool) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    let tail = if hold {
        "sleep 60\n".to_string()
    } else {
        "printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"agent answer\"}]}}'\n\
         printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"agent answer\"}'\n"
            .to_string()
    };
    let script = format!(
        "#!/bin/sh\n: > {ran}\ncat > {prompt}\n{tail}",
        ran = dir.join("ran").display(),
        prompt = dir.join("prompt").display(),
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Pump the executor until `done`, or fail after 20 s.
fn settle(
    cx: &mut gpui::TestAppContext,
    app: &gpui::Entity<AwiliApp>,
    what: &str,
    done: impl Fn(&AwiliApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if app.read_with(cx, |a, _| done(a)) {
            return;
        }
        assert!(Instant::now() < deadline, "never settled: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Write the stored shape the removed Chat left behind: a `chat` thread with
/// one real answer and one refusal sentence (with its provenance line).
fn write_legacy_thread() {
    let store = awili_persist::Store::open_default().unwrap();
    store
        .create_conversation_with_kind(LEGACY, "Old chat", 1, "chat")
        .unwrap();
    for (role, text, ts) in [
        ("user", "What plants grow in shade?", 2),
        ("assistant", "Hostas and ferns do well.\n\n[answered by Claude \u{b7} haiku]", 3),
        ("user", "run whoami", 4),
        (
            "assistant",
            "I can't do that in an answer-only turn. Select Run as a job in this Workshop thread, and choose a folder if it needs project files.\n\n[answered by Awili]",
            5,
        ),
    ] {
        store.append_message(LEGACY, role, text, ts).unwrap();
    }
}

/// Point the app at the fake helper with NO folder chosen, and open the
/// restored legacy thread.
fn open_legacy_with_fake_helper(
    app: &mut AwiliApp,
    home: &std::path::Path,
    dir: &std::path::Path,
    hold: bool,
    cx: &mut gpui::Context<AwiliApp>,
) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.memory.enabled = false;
    app.fleet_pack = None;
    app.work.agents.claude_bin = Some(fake_claude(dir, hold).display().to_string());
    app.work.folder = None;
    app.work
        .graduation
        .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
    app.open_conversation_from_history(LEGACY, cx);
    // The thread's next-turn target is Claude, chosen the way the chooser does
    // (the route decides the send, not the ambient executor).
    assert!(app.set_composer_target(
        &crate::workshop_composer::ModelOwner::Work(LEGACY.to_string()),
        WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        },
        cx,
    ));
}

fn texts(app: &AwiliApp) -> Vec<(awili_core::Role, String)> {
    app.views[LEGACY]
        .convo
        .messages()
        .iter()
        .map(|m| (m.role, m.text.clone()))
        .collect()
}

/// A legacy thread restores as a Workshop thread with its rows intact, and
/// every real, context-dependent utterance then runs the helper: no refusal,
/// no "Run as a job", no folder demanded, no old refusal replayed.
#[gpui::test]
fn contextual_utterances_run_the_helper_on_a_restored_legacy_thread(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::workshop_dispatch_tests::contextual_utterances_run_the_helper_on_a_restored_legacy_thread",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    write_legacy_thread();
    let dir = tempfile::tempdir().unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        // The stored kind is untouched on disk; in the app it is a Workshop thread.
        assert_eq!(
            app.conversations.get(LEGACY).map(|c| c.kind),
            Some(awili_core::ConvKind::Work)
        );
        assert_eq!(texts(app).len(), 4, "every stored row restored");
        open_legacy_with_fake_helper(app, &home, dir.path(), false, cx);
        assert_eq!(app.mode, Mode::Work);
        assert_eq!(app.sidebar.active_id(), LEGACY);
    });
    for (i, utterance) in UTTERANCES.iter().enumerate() {
        let _ = std::fs::remove_file(dir.path().join("ran"));
        entity.update(cx, |app, cx| {
            app.composer.update(cx, |c, cx| c.set_text(utterance, cx));
            app.submit_composer_draft(cx);
        });
        let want = 4 + 2 * (i + 1);
        settle(cx, &entity, utterance, |a| {
            !a.work.running
                && a.views[LEGACY].convo.len() == want
                && a.views[LEGACY]
                    .convo
                    .messages()
                    .last()
                    .is_some_and(|m| m.text == "agent answer")
        });
        assert!(
            dir.path().join("ran").exists(),
            "{utterance:?} reached the helper"
        );
        let prompt = crate::test_stdin::read_prompt(&dir.path().join("prompt")).unwrap();
        assert!(prompt.contains(utterance), "{utterance:?}: {prompt}");
        // The legacy answer is carried as context; the old refusal and the
        // provenance line are not.
        assert!(prompt.contains("Hostas and ferns do well."), "{prompt}");
        assert!(!prompt.contains("answer-only"), "{prompt}");
        assert!(!prompt.contains("[answered by"), "{prompt}");
        entity.read_with(cx, |app, _| {
            assert_eq!(app.work.panel.error, None, "{utterance:?}");
            let rows = texts(app);
            assert_eq!(
                rows[want - 2],
                (awili_core::Role::User, utterance.to_string())
            );
            for (_, text) in &rows[4..] {
                assert!(
                    !text.contains("Run as a job") && !text.contains("answer-only"),
                    "{text}"
                );
            }
        });
    }
}

/// Stop ends an ordinary turn, the thread stays usable, and the old stored
/// rows are still there.
#[gpui::test]
fn stop_ends_an_ordinary_turn_on_a_legacy_thread(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::workshop_dispatch_tests::stop_ends_an_ordinary_turn_on_a_legacy_thread")
    else {
        return;
    };
    cx.executor().allow_parking();
    write_legacy_thread();
    let dir = tempfile::tempdir().unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        open_legacy_with_fake_helper(app, &home, dir.path(), true, cx);
        app.composer.update(cx, |c, cx| {
            c.set_text("take a look at where we are at now", cx)
        });
        app.submit_composer_draft(cx);
    });
    settle(cx, &entity, "the helper started", |a| {
        a.work.running && dir.path().join("ran").exists()
    });
    entity.update(cx, |app, cx| {
        assert!(app.composer_busy(), "the box is busy while it runs");
        app.abort_active_stream(cx);
    });
    settle(cx, &entity, "Stop settled", |a| !a.work.running);
    entity.read_with(cx, |app, _| {
        assert!(!app.composer_busy(), "Send is back after Stop");
        assert!(
            texts(app)[..4]
                .iter()
                .any(|(_, t)| t.contains("Hostas and ferns")),
            "the stored rows are still there"
        );
    });
}
