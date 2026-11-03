//! Offline integration tests for the recovered Workshop: one conversation, a
//! per-conversation next-turn target, and native-session continuity across
//! harnesses. Each test runs the real `AwiliApp` (startup migration, getter,
//! chooser setter and the composer's own submit path) in a private-HOME
//! subprocess against fake `claude` / `dsh` scripts that record their exact
//! argv. Nothing here reads the Store/route types in isolation: every
//! assertion is on what the app showed, persisted or handed a provider.
//!
//! Not covered here: a real Codex app-server (dsh stands in as the second
//! harness, since both are headless CLIs behind the same provider adapter), a
//! late provider event after Stop (the fake is killed with its process group),
//! and a blocked memory gate.

use crate::{
    consolidation_tests::isolated,
    work::{Mode, WorkExecutor},
    workshop_composer::ModelOwner,
    AwiliApp,
};
use awili_code::CodeEngine;
use gpui::AppContext;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ---- fake harnesses ----------------------------------------------------------

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn fake_cli(dir: &Path, name: &str, body: &str) -> PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake CLI named `name` in `dir`. Every invocation records its argv
/// (NUL-separated) and cwd under `dir/calls/`, then runs `body`.
fn fake_cli(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = r#"#!/bin/sh
d=@DIR@
mkdir -p "$d/calls"
in=$(cat)
i=$(( $(ls "$d/calls" | grep -c '^@NAME@\.[0-9]*$') + 1 ))
: > "$d/calls/@NAME@.$i"
for a in "$@"; do printf '%s\0' "$a" >> "$d/calls/@NAME@.$i"; done
printf '%s\0' "$in" >> "$d/calls/@NAME@.$i"
pwd > "$d/calls/@NAME@.$i.cwd"
@BODY@
"#
    .replace("@DIR@", &dir.display().to_string())
    .replace("@NAME@", name)
    .replace("@BODY@", body);
    let bin = dir.join(name);
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Answers one stream-json turn with a session id (the `--resume` id when it
/// was given, else a fresh one), optionally after a tool step (`$d/tools`),
/// or stays busy until killed (`$d/hold`).
const CLAUDE_BODY: &str = r#"sid="sess-claude-$i"
prev=""
for a in "$@"; do [ "$prev" = "--resume" ] && sid="$a"; prev="$a"; done
printf '%s\n' "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$sid\"}"
if [ -f "$d/hold" ]; then sleep 60; exit 0; fi
if [ -f "$d/tools" ]; then
  printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}'
  printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}'
fi
printf '%s\n' "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"claude answer $i\"}]}}"
printf '%s\n' "{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"claude answer $i\"}"
"#;

/// dsh headless prints its final message as plain text and has no resume.
/// The chosen model reaches it in the `DSH_MODEL` environment (no flag).
const DSH_BODY: &str = r#"printf '%s' "$DSH_MODEL" > "$d/calls/dsh.$i.model"
echo "dsh answer $i""#;

/// Everything one run of the fixture needs: the private HOME, a bin dir with
/// the fakes (and their call log), and a project folder.
struct Fx {
    home: PathBuf,
    bins: tempfile::TempDir,
    folder: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let bins = tempfile::tempdir().unwrap();
        fake_cli(bins.path(), "claude", CLAUDE_BODY);
        fake_cli(bins.path(), "dsh", DSH_BODY);
        Self {
            home,
            bins,
            folder: tempfile::tempdir().unwrap(),
        }
    }

    /// Wire the app to the fakes, with no memory, fleet or real ledger.
    fn configure(&self, app: &mut AwiliApp) {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.memory.enabled = false;
        app.fleet_pack = None;
        app.work.agents.claude_bin = Some(self.bins.path().join("claude").display().to_string());
        app.work.agents.dsh_bin = Some(self.bins.path().join("dsh").display().to_string());
        // The other harnesses point at paths that do not exist: a fixture that
        // reached for one would fail, never run an installed CLI.
        for bin in [
            &mut app.work.agents.codex_bin,
            &mut app.work.agents.zcode_bin,
        ] {
            *bin = Some(self.bins.path().join("not-installed").display().to_string());
        }
        app.work.folder = Some(self.folder.path().to_path_buf());
        app.work
            .graduation
            .set_ledger(self.home.join("ledger.jsonl").to_str().unwrap());
    }

    fn flag(&self, name: &str) {
        std::fs::write(self.bins.path().join(name), "").unwrap();
    }

    fn unflag(&self, name: &str) {
        let _ = std::fs::remove_file(self.bins.path().join(name));
    }

    /// Every recorded invocation of `name`, in order, as argv.
    fn calls(&self, name: &str) -> Vec<Vec<String>> {
        (1..)
            .map(|i| self.bins.path().join("calls").join(format!("{name}.{i}")))
            .take_while(|p| p.exists())
            .map(|p| {
                let bytes = std::fs::read(p).unwrap();
                let mut args: Vec<String> = bytes
                    .split(|b| *b == 0)
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .collect();
                args.pop(); // the empty tail after the last NUL
                args
            })
            .collect()
    }

    /// The `DSH_MODEL` a dsh call was launched with.
    fn dsh_model(&self, call: usize) -> String {
        std::fs::read_to_string(
            self.bins
                .path()
                .join("calls")
                .join(format!("dsh.{call}.model")),
        )
        .unwrap()
    }

    fn cwd(&self, name: &str, call: usize) -> String {
        std::fs::read_to_string(
            self.bins
                .path()
                .join("calls")
                .join(format!("{name}.{call}.cwd")),
        )
        .unwrap()
        .trim()
        .to_string()
    }
}

/// The task text a provider was handed: the argv entry carrying `task`. Claude,
/// Codex and GLM take it as an argument (the launch also carries long system
/// text, so "the longest argument" is not the task); none of them is read from
/// stdin in this driver. A miss prints the whole argv.
fn task_arg<'a>(call: &'a [String], task: &str) -> &'a str {
    call.iter()
        .find(|arg| arg.contains(task))
        .map(String::as_str)
        .unwrap_or_else(|| panic!("no argv entry carried {task:?}: {call:?}"))
}

fn has_pair(call: &[String], flag: &str, value: &str) -> bool {
    call.windows(2).any(|w| w[0] == flag && w[1] == value)
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

// ---- driving the app ----------------------------------------------------------

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

fn rows(app: &AwiliApp, conv: &str) -> usize {
    app.views[conv].convo.messages().len()
}

/// Send `text` through the composer's own submit and wait for the reply row.
fn turn(cx: &mut gpui::TestAppContext, app: &gpui::Entity<AwiliApp>, conv: &str, text: &str) {
    let before = app.read_with(cx, |a, _| rows(a, conv));
    app.update(cx, |a, cx| {
        a.composer.update(cx, |c, cx| c.set_text(text, cx));
        a.submit_composer_draft(cx);
    });
    settle(cx, app, text, |a| {
        !a.work.running
            && rows(a, conv) == before + 2
            && a.views[conv]
                .convo
                .messages()
                .last()
                .is_some_and(|m| m.role == awili_core::Role::Assistant && !m.text.is_empty())
    });
}

fn work_owner(conv: &str) -> ModelOwner {
    ModelOwner::Work(conv.to_string())
}

fn agent(engine: CodeEngine, model: Option<&str>) -> WorkExecutor {
    WorkExecutor::Agent {
        engine,
        model: model.map(str::to_string),
    }
}

fn choose(
    cx: &mut gpui::TestAppContext,
    app: &gpui::Entity<AwiliApp>,
    conv: &str,
    target: WorkExecutor,
) {
    let accepted = app.update(cx, |a, cx| {
        a.set_composer_target(&work_owner(conv), target, cx)
    });
    assert!(
        accepted,
        "the chooser accepts a target for the shown thread"
    );
    cx.run_until_parked();
}

fn next_target(app: &AwiliApp, conv: &str) -> Option<WorkExecutor> {
    app.composer_target_for(&work_owner(conv))
}

/// A launch of the app over the private HOME, set up as the user's shown
/// Workshop thread; returns it with that thread's stable id.
fn launch(cx: &mut gpui::TestAppContext, fx: &Fx) -> (gpui::Entity<AwiliApp>, String) {
    let app = cx.new(AwiliApp::new);
    let conv = app.update(cx, |a, _| {
        fx.configure(a);
        a.mode = Mode::Work;
        a.sidebar.active_id().to_string()
    });
    (app, conv)
}

/// The raw stored value, read through a second handle once the app's writer
/// has had time to commit.
fn stored(key: &str) -> Option<String> {
    awili_persist::Store::open_default()
        .unwrap()
        .get_app_state(key)
        .unwrap()
}

fn wait_stored(key: &str, want: impl Fn(&Option<String>) -> bool) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let value = stored(key);
        if want(&value) {
            return value;
        }
        assert!(Instant::now() < deadline, "{key} never reached the store");
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---- 1. startup migration, unavailable routes, recovery --------------------------

const LEGACY: &str = "legacy-chat";
const CANON: &str = "canon-valid";
const BAD_CANON: &str = "canon-unsupported";
const MALFORMED: &str = "canon-malformed";
const BAD_LEGACY: &str = "legacy-unsupported";
const UNSUPPORTED_ROUTE: &str = r#"{"version":99,"next_target":"in-app","native_sessions":[]}"#;

fn seed_history() {
    let store = awili_persist::Store::open_default().unwrap();
    for (id, kind) in [
        (LEGACY, "chat"),
        (CANON, "work"),
        (BAD_CANON, "work"),
        (MALFORMED, "work"),
        (BAD_LEGACY, "chat"),
    ] {
        store
            .create_conversation_with_kind(id, id, 1, kind)
            .unwrap();
        store
            .append_message(id, "user", &format!("earlier question in {id}"), 2)
            .unwrap();
        store
            .append_message(id, "assistant", &format!("earlier answer in {id}"), 3)
            .unwrap();
    }
    let canonical = crate::conversation_route::ConversationRoute::new(agent(CodeEngine::Dsh, None))
        .to_persisted_json()
        .unwrap();
    store
        .set_app_state(&format!("conversation_route:{CANON}"), &canonical)
        .unwrap();
    store
        .set_app_state(
            &format!("conversation_route:{BAD_CANON}"),
            UNSUPPORTED_ROUTE,
        )
        .unwrap();
    store
        .set_app_state(&format!("conversation_route:{MALFORMED}"), "{not json")
        .unwrap();
    // The removed Chat's per-thread choices. The canonical rows above have a
    // perfectly good legacy Claude entry too: none of them may win or fill in.
    store
        .set_app_state(
            "workshop:general_models",
            &serde_json::json!({
                LEGACY: "agent:claude:sonnet",
                CANON: "agent:claude:haiku",
                BAD_CANON: "agent:claude:haiku",
                MALFORMED: "agent:claude:haiku",
                BAD_LEGACY: "agent:nonesuch:model",
            })
            .to_string(),
        )
        .unwrap();
}

/// The real startup reads the removed Chat's `workshop:general_models` and the
/// canonical per-conversation rows. The getter then answers per thread:
/// a valid canonical route beats the legacy value, a legacy Claude+model
/// choice survives, and an unsupported or malformed saved choice is explicit
/// ("no target"), never the ambient default and never a legacy fallback.
#[gpui::test]
fn startup_resolves_legacy_and_canonical_targets_without_inventing_one(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("recovery_integration_tests::startup_resolves_legacy_and_canonical_targets_without_inventing_one") {
        return;
    }
    cx.executor().allow_parking();
    seed_history();
    let fx = Fx::new();
    let (app, _) = launch(cx, &fx);
    app.read_with(cx, |a, _| {
        assert_eq!(
            next_target(a, LEGACY),
            Some(agent(CodeEngine::Claude, Some("sonnet"))),
            "the old Chat thread keeps its Claude model"
        );
        assert_eq!(
            next_target(a, CANON),
            Some(agent(CodeEngine::Dsh, None)),
            "a valid canonical route beats the legacy entry"
        );
        for id in [BAD_CANON, MALFORMED, BAD_LEGACY] {
            assert_eq!(next_target(a, id), None, "{id} is explicitly unavailable");
        }
    });
    // Reading is not repairing: the raw values are exactly what was stored.
    assert_eq!(
        stored(&format!("conversation_route:{BAD_CANON}")).as_deref(),
        Some(UNSUPPORTED_ROUTE)
    );
    assert_eq!(
        stored(&format!("conversation_route:{MALFORMED}")).as_deref(),
        Some("{not json")
    );
}

/// An old Chat thread, sent from, runs on its legacy Claude model through the
/// ordinary composer path, with its stored history, not the global helper.
#[gpui::test]
fn a_restored_legacy_thread_sends_on_its_saved_claude_model(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "recovery_integration_tests::a_restored_legacy_thread_sends_on_its_saved_claude_model",
    ) {
        return;
    }
    cx.executor().allow_parking();
    seed_history();
    let fx = Fx::new();
    let (app, _) = launch(cx, &fx);
    app.update(cx, |a, cx| {
        // The ambient default is something else entirely.
        a.work.executor = agent(CodeEngine::Dsh, None);
        a.open_conversation_from_history(LEGACY, cx);
    });
    turn(cx, &app, LEGACY, "carry on please");
    let claude = fx.calls("claude");
    assert_eq!(claude.len(), 1, "the saved Claude target ran");
    assert!(has_pair(&claude[0], "--model", "sonnet"), "{:?}", claude[0]);
    let sent = task_arg(&claude[0], "carry on please");
    assert!(sent.contains("carry on please"));
    assert!(
        sent.contains(&format!("earlier answer in {LEGACY}")),
        "{sent}"
    );
    assert!(fx.calls("dsh").is_empty(), "the ambient helper did not run");
}

/// An unsupported canonical route refuses a send, keeps the typed draft and
/// the staged file, and stays exactly as stored — also after a restart and a
/// reopen of the previously closed thread — until the owner picks a target in
/// the chooser. Then the same thread sends, once, with the file.
#[gpui::test]
fn unsupported_canonical_route_stays_unavailable_until_the_chooser_repairs_it(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("recovery_integration_tests::unsupported_canonical_route_stays_unavailable_until_the_chooser_repairs_it") {
        return;
    }
    cx.executor().allow_parking();
    seed_history();
    let fx = Fx::new();
    let (app, _) = launch(cx, &fx);
    let route_key = format!("conversation_route:{BAD_CANON}");

    app.update(cx, |a, cx| {
        a.open_conversation_from_history(BAD_CANON, cx);
        a.attach_bytes("notes.txt", "text/plain", b"ATTACH-MARKER-1", cx);
        a.composer
            .update(cx, |c, cx| c.set_text("please continue", cx));
        assert_eq!(a.staged_attachments.len(), 1);
        a.submit_composer_draft(cx);
    });
    cx.run_until_parked();
    app.read_with(cx, |a, cx| {
        assert!(!a.work.running);
        assert_eq!(rows(a, BAD_CANON), 2, "nothing was added to the transcript");
        assert_eq!(
            a.composer.read(cx).text(),
            "please continue",
            "the draft came back"
        );
        assert_eq!(a.staged_attachments.len(), 1, "the file is still staged");
        assert!(
            a.work
                .panel
                .error
                .as_deref()
                .is_some_and(|e| e.contains("unavailable")),
            "{:?}",
            a.work.panel.error
        );
        assert_eq!(next_target(a, BAD_CANON), None);
    });
    assert!(fx.calls("claude").is_empty() && fx.calls("dsh").is_empty());

    // A restart does not repair it, and a thread closed and reopened from
    // history is the same stored, still-unavailable choice.
    let (again, _) = launch(cx, &fx);
    again.update(cx, |a, cx| {
        assert_eq!(next_target(a, BAD_CANON), None);
        a.open_conversation_from_history(BAD_CANON, cx);
        a.open_conversation_from_history(CANON, cx);
        a.open_conversation_from_history(BAD_CANON, cx);
        assert_eq!(next_target(a, BAD_CANON), None);
    });
    assert_eq!(stored(&route_key).as_deref(), Some(UNSUPPORTED_ROUTE));

    // The explicit recovery is the chooser. It replaces the saved choice.
    choose(cx, &app, BAD_CANON, agent(CodeEngine::Claude, Some("opus")));
    app.read_with(cx, |a, _| {
        assert_eq!(
            next_target(a, BAD_CANON),
            Some(agent(CodeEngine::Claude, Some("opus")))
        );
    });
    let repaired = wait_stored(&route_key, |v| v.as_deref() != Some(UNSUPPORTED_ROUTE)).unwrap();
    assert!(
        crate::conversation_route::ConversationRoute::from_persisted_json(&repaired).is_ok(),
        "{repaired}"
    );
    app.update(cx, |a, cx| a.submit_composer_draft(cx));
    settle(cx, &app, "the repaired thread ran", |a| {
        !a.work.running && rows(a, BAD_CANON) == 4
    });
    let claude = fx.calls("claude");
    assert_eq!(claude.len(), 1);
    assert!(has_pair(&claude[0], "--model", "opus"));
    let sent = task_arg(&claude[0], "please continue");
    assert!(sent.contains("please continue"));
    assert_eq!(
        count(sent, "ATTACH-MARKER-1"),
        1,
        "the file reached the provider once: {sent}"
    );
    app.read_with(cx, |a, _| {
        assert!(
            a.staged_attachments.is_empty(),
            "the tray cleared once accepted"
        )
    });
}

// ---- 2. Claude -> another harness -> Claude in one conversation ---------------------

/// One conversation, Claude -> dsh -> Claude -> Claude(other model), captured
/// at the process boundary. The conversation, folder and a typed draft are
/// untouched by every switch; the staged file reaches exactly one request; the
/// returning Claude resumes its own session and is handed exactly the
/// intervening pair, once; the dsh turn (no resume) is handed the history it
/// needs; a model change on the same harness still resumes.
#[gpui::test]
fn claude_to_dsh_and_back_resumes_with_exactly_the_intervening_pair(cx: &mut gpui::TestAppContext) {
    if !isolated("recovery_integration_tests::claude_to_dsh_and_back_resumes_with_exactly_the_intervening_pair") {
        return;
    }
    cx.executor().allow_parking();
    let fx = Fx::new();
    let (app, conv) = launch(cx, &fx);
    let (conversations, tabs) = app.read_with(cx, |a, _| {
        (
            a.conversations.all().len(),
            a.shell
                .tabs
                .tabs
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>(),
        )
    });

    // Turn 1: Claude.
    choose(cx, &app, &conv, agent(CodeEngine::Claude, None));
    turn(cx, &app, &conv, "first-A question");
    let claude = fx.calls("claude");
    assert_eq!(claude.len(), 1);
    assert!(
        !claude[0].iter().any(|a| a == "--resume"),
        "a new thread starts fresh"
    );
    assert!(task_arg(&claude[0], "first-A question").contains("first-A question"));

    // Switch to dsh with a typed draft and a staged file waiting: selecting
    // sends nothing and touches neither.
    app.update(cx, |a, cx| {
        a.attach_bytes("notes.txt", "text/plain", b"ATTACH-MARKER-2", cx);
        a.composer.update(cx, |c, cx| c.set_text("typed draft", cx));
    });
    choose(cx, &app, &conv, agent(CodeEngine::Dsh, None));
    app.read_with(cx, |a, cx| {
        assert_eq!(rows(a, &conv), 2, "selecting a target sent nothing");
        assert_eq!(a.composer.read(cx).text(), "typed draft");
        assert_eq!(a.staged_attachments.len(), 1);
        assert_eq!(a.work.folder.as_deref(), Some(fx.folder.path()));
    });
    assert_eq!(
        fx.calls("claude").len() + fx.calls("dsh").len(),
        1,
        "no provider ran for a selection"
    );
    turn(cx, &app, &conv, "second-B question");
    app.read_with(cx, |a, _| {
        assert!(
            a.staged_attachments.is_empty(),
            "cleared once the turn was accepted"
        )
    });
    let dsh = fx.calls("dsh");
    assert_eq!(dsh.len(), 1);
    assert_eq!(
        &dsh[0][..2],
        &["--profile".to_string(), "headless".to_string()],
        "no resume or model on dsh's argv: {:?}",
        dsh[0]
    );
    let to_dsh = task_arg(&dsh[0], "second-B question");
    assert_eq!(
        count(to_dsh, "first-A question"),
        1,
        "dsh gets the history it cannot resume: {to_dsh}"
    );
    assert_eq!(count(to_dsh, "claude answer 1"), 1, "{to_dsh}");
    assert_eq!(
        count(to_dsh, "ATTACH-MARKER-2"),
        1,
        "the file reached dsh once: {to_dsh}"
    );

    // Back to Claude: it resumes its own session and sees only what it missed.
    choose(cx, &app, &conv, agent(CodeEngine::Claude, None));
    turn(cx, &app, &conv, "third-A question");
    let claude = fx.calls("claude");
    assert_eq!(claude.len(), 2);
    assert!(
        has_pair(&claude[1], "--resume", "sess-claude-1"),
        "{:?}",
        claude[1]
    );
    let back = task_arg(&claude[1], "third-A question");
    assert_eq!(count(back, "second-B question"), 1, "{back}");
    assert_eq!(count(back, "dsh answer 1"), 1, "{back}");
    assert_eq!(
        count(back, "ATTACH-MARKER-2"),
        1,
        "the intervening user row carries its file once: {back}"
    );
    assert!(
        !back.contains("first-A question"),
        "Claude already has this turn: {back}"
    );
    assert_eq!(count(back, "third-A question"), 1, "{back}");

    // Same harness, another model: the session is still resumed, nothing
    // already seen is repeated.
    choose(cx, &app, &conv, agent(CodeEngine::Claude, Some("opus")));
    turn(cx, &app, &conv, "fourth question");
    let claude = fx.calls("claude");
    assert_eq!(claude.len(), 3);
    assert!(
        has_pair(&claude[2], "--resume", "sess-claude-1"),
        "{:?}",
        claude[2]
    );
    assert!(has_pair(&claude[2], "--model", "opus"), "{:?}", claude[2]);
    let model_change = task_arg(&claude[2], "fourth question");
    assert_eq!(count(model_change, "fourth question"), 1);
    assert!(
        !model_change.contains("second-B question") && !model_change.contains("dsh answer"),
        "{model_change}"
    );

    // Throughout: one conversation, the same tabs, one folder.
    app.read_with(cx, |a, _| {
        assert_eq!(a.sidebar.active_id(), conv);
        assert_eq!(
            a.conversations.all().len(),
            conversations,
            "no thread was created"
        );
        assert_eq!(
            a.shell
                .tabs
                .tabs
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>(),
            tabs
        );
        assert_eq!(rows(a, &conv), 8);
    });
    let cwds: Vec<String> = [("claude", 1), ("claude", 2), ("claude", 3), ("dsh", 1)]
        .iter()
        .map(|(n, i)| fx.cwd(n, *i))
        .collect();
    assert!(
        cwds.windows(2).all(|w| w[0] == w[1]),
        "one folder for every harness: {cwds:?}"
    );
}

// ---- 3. a next-target selection while a native turn holds -----------------------------

/// While Claude holds its response, choosing dsh is accepted but changes only
/// the next turn: the running turn keeps its target, a second send neither
/// overlaps nor converts the thread, Stop ends the original, and nothing that
/// finishes afterwards moves the selection. The next accepted send is dsh's.
#[gpui::test]
fn selecting_while_a_native_turn_holds_cannot_retarget_or_overlap_it(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("recovery_integration_tests::selecting_while_a_native_turn_holds_cannot_retarget_or_overlap_it") {
        return;
    }
    cx.executor().allow_parking();
    let fx = Fx::new();
    fx.flag("hold");
    let (app, conv) = launch(cx, &fx);
    choose(cx, &app, &conv, agent(CodeEngine::Claude, None));
    app.update(cx, |a, cx| {
        a.composer
            .update(cx, |c, cx| c.set_text("long first question", cx));
        a.submit_composer_draft(cx);
    });
    settle(cx, &app, "claude is holding", |a| {
        a.work.running && fx.calls("claude").len() == 1
    });
    let held_rows = app.read_with(cx, |a, _| rows(a, &conv));

    choose(cx, &app, &conv, agent(CodeEngine::Dsh, None));
    app.update(cx, |a, cx| {
        a.composer
            .update(cx, |c, cx| c.set_text("overlapping question", cx));
        a.submit_composer_draft(cx);
    });
    cx.run_until_parked();
    app.read_with(cx, |a, cx| {
        assert!(a.work.running, "the original turn is still the running one");
        assert_eq!(
            rows(a, &conv),
            held_rows,
            "the second send was not accepted"
        );
        assert_eq!(
            next_target(a, &conv),
            Some(agent(CodeEngine::Dsh, None)),
            "the selection is the next turn's"
        );
        assert!(
            matches!(
                a.work.conversation_routes[&conv].active(),
                Some(turn) if turn.target == agent(CodeEngine::Claude, None)
            ),
            "the running turn kept the target it started under"
        );
        assert_eq!(
            a.composer.read(cx).text(),
            "overlapping question",
            "the unsent text stays in the box"
        );
    });
    assert!(fx.calls("dsh").is_empty(), "dsh did not overlap Claude");

    // Stop belongs to the original turn, and the selection survives it.
    app.update(cx, |a, cx| a.abort_active_stream(cx));
    settle(cx, &app, "stop settled", |a| !a.work.running);
    std::thread::sleep(Duration::from_millis(300));
    cx.run_until_parked();
    app.read_with(cx, |a, _| {
        assert_eq!(
            next_target(a, &conv),
            Some(agent(CodeEngine::Dsh, None)),
            "a late finish moved nothing"
        );
        assert!(a.work.conversation_routes[&conv].active().is_none());
    });
    assert!(fx.calls("dsh").is_empty());

    // The next accepted send is the selected target's.
    fx.unflag("hold");
    app.update(cx, |a, cx| a.submit_composer_draft(cx));
    settle(cx, &app, "dsh ran", |a| {
        !a.work.running && !a.composer_busy() && fx.calls("dsh").len() == 1
    });
    cx.run_until_parked();
    assert_eq!(fx.calls("dsh").len(), 1, "exactly one dsh turn");
    assert_eq!(fx.calls("claude").len(), 1, "Claude was not run again");
}

// ---- 4. the canonical cursor across activity and restart ------------------------------

/// A turn that used a tool, a restart, an intervening dsh turn, and back to
/// Claude. The transcript Claude has seen is positional, so what the app
/// rebuilds from storage must be the same transcript the cursor was counted
/// on; otherwise the returning Claude skips the new pair. Exposes the
/// activity-record projection (`bridge_records_from_view`) if it does not
/// round-trip a relaunch.
#[gpui::test]
fn the_cursor_still_fits_after_activity_is_lost_across_a_restart(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "recovery_integration_tests::the_cursor_still_fits_after_activity_is_lost_across_a_restart",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let fx = Fx::new();
    fx.flag("tools");
    let (app, conv) = launch(cx, &fx);
    choose(cx, &app, &conv, agent(CodeEngine::Claude, None));
    turn(cx, &app, &conv, "first-A question");
    let live = app.read_with(cx, |a, _| {
        let view = &a.views[&conv];
        assert!(
            view.rows.iter().any(|r| !r.activity.is_empty()),
            "fixture: the Claude turn must carry activity"
        );
        crate::work::bridge_records_from_view(view)
    });
    // The Claude session this turn started must be in the saved route before a
    // relaunch can resume it.
    wait_stored(&format!("conversation_route:{conv}"), |v| {
        v.as_deref()
            .is_some_and(|raw| raw.contains("sess-claude-1"))
    });
    // The stored transcript has to be complete before the relaunch.
    let deadline = Instant::now() + Duration::from_secs(5);
    while awili_persist::Store::open_default()
        .unwrap()
        .messages(&conv)
        .unwrap()
        .len()
        < 2
    {
        assert!(
            Instant::now() < deadline,
            "the turn never reached the store"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let (relaunched, _) = launch(cx, &fx);
    relaunched.update(cx, |a, cx| a.open_conversation_from_history(&conv, cx));
    let restored = relaunched.read_with(cx, |a, _| {
        crate::work::bridge_records_from_view(&a.views[&conv])
    });
    assert_eq!(
        awili_code::continuity::canonical_cursor(&restored),
        awili_code::continuity::canonical_cursor(&live),
        "the same conversation counts the same after a relaunch ({} live records, {} restored)",
        live.len(),
        restored.len()
    );

    fx.unflag("tools");
    choose(cx, &relaunched, &conv, agent(CodeEngine::Dsh, None));
    turn(cx, &relaunched, &conv, "second-B question");
    choose(cx, &relaunched, &conv, agent(CodeEngine::Claude, None));
    turn(cx, &relaunched, &conv, "third-A question");
    let claude = fx.calls("claude");
    assert_eq!(claude.len(), 2);
    assert!(
        has_pair(&claude[1], "--resume", "sess-claude-1"),
        "{:?}",
        claude[1]
    );
    let back = task_arg(&claude[1], "third-A question");
    assert_eq!(
        count(back, "second-B question"),
        1,
        "the intervening pair arrives once: {back}"
    );
    assert_eq!(count(back, "dsh answer 1"), 1, "{back}");
    assert!(!back.contains("first-A question"), "{back}");
}

// ---- 5. older per-thread helpers and closed native history ----------------------------

/// A Workshop thread saved with an explicit helper (`work_helpers`) keeps it:
/// the startup seed of a default route for every thread must not win over that
/// older, explicit per-thread choice.
#[gpui::test]
fn a_saved_work_helper_beats_the_default_seed_at_startup(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "recovery_integration_tests::a_saved_work_helper_beats_the_default_seed_at_startup",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let store = awili_persist::Store::open_default().unwrap();
    store
        .create_conversation_with_kind("helper-thread", "Helper", 1, "work")
        .unwrap();
    store
        .append_message("helper-thread", "user", "an earlier question", 2)
        .unwrap();
    store
        .append_message("helper-thread", "assistant", "an earlier answer", 3)
        .unwrap();
    let tab = uuid::Uuid::new_v4().to_string();
    store
        .set_app_state(
            crate::workshop::TABS_KEY,
            &serde_json::json!({
                "version": 1,
                "tabs": [{"id": tab, "backing": {"Conversation": "helper-thread"}}],
                "active": tab,
                "work_helpers": {"helper-thread": "agent:dsh"},
            })
            .to_string(),
        )
        .unwrap();
    drop(store);
    let fx = Fx::new();
    let (app, _) = launch(cx, &fx);
    app.update(cx, |a, _| a.work.executor = agent(CodeEngine::Claude, None));
    app.read_with(cx, |a, _| {
        assert_eq!(
            next_target(a, "helper-thread"),
            Some(agent(CodeEngine::Dsh, None)),
            "the thread's saved helper, not the ambient default"
        );
    });
}

const CLOSED_CODE: &str = "closed-code-history";
const UNKNOWN_CODE: &str = "closed-code-unknown-route";

fn seed_closed_code_history(fx: &Fx) {
    let store = awili_persist::Store::open_default().unwrap();
    for (id, meta) in [
        (
            CLOSED_CODE,
            crate::code_history::CodeConvMeta::for_turn(
                CodeEngine::Codex,
                Some("gpt-test".into()),
                Some(fx.folder.path().display().to_string()),
                Some("codex-session-1".into()),
            ),
        ),
        (UNKNOWN_CODE, {
            let mut meta = crate::code_history::CodeConvMeta::for_turn(
                CodeEngine::Claude,
                None,
                Some(fx.folder.path().display().to_string()),
                None,
            );
            meta.route = Some(UNSUPPORTED_ROUTE.to_string());
            meta
        }),
    ] {
        store
            .create_conversation_with_kind(id, id, 1, "code")
            .unwrap();
        store
            .append_message(id, "user", &format!("native question in {id}"), 2)
            .unwrap();
        store
            .append_message(id, "assistant", &format!("native answer in {id}"), 3)
            .unwrap();
        store
            .set_app_state(&crate::code_history::code_conv_key(id), &meta.to_json())
            .unwrap();
    }
}

fn native_tab_for(a: &AwiliApp, conv: &str) -> Option<u64> {
    a.work
        .code_tabs
        .iter()
        .find(|t| t.history.conv_id.as_deref() == Some(conv))
        .map(|t| t.id)
}

/// A native thread that was closed (it is in history, not in the open tabs)
/// reopens on its own recorded harness and model, with its transcript — never
/// on the startup default the app seeded for threads it knew nothing about.
#[gpui::test]
fn closed_native_history_reopens_on_its_own_harness_and_model(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "recovery_integration_tests::closed_native_history_reopens_on_its_own_harness_and_model",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let fx = Fx::new();
    seed_closed_code_history(&fx);
    let (app, _) = launch(cx, &fx);
    app.update(cx, |a, cx| {
        a.work.executor = agent(CodeEngine::Dsh, None);
        a.open_conversation_from_history(CLOSED_CODE, cx);
    });
    settle(cx, &app, "the closed native thread reopened", |a| {
        native_tab_for(a, CLOSED_CODE).is_some()
    });
    app.read_with(cx, |a, _| {
        let tab = native_tab_for(a, CLOSED_CODE).unwrap();
        assert_eq!(
            next_target(a, CLOSED_CODE),
            Some(agent(CodeEngine::Codex, Some("gpt-test"))),
            "the thread's own route"
        );
        assert_eq!(
            a.composer_target_for(&ModelOwner::Native(tab)),
            Some(agent(CodeEngine::Codex, Some("gpt-test"))),
            "and the reopened tab shows it, not the ambient helper"
        );
        let session = a.work.code_tabs.iter().find(|t| t.id == tab).unwrap();
        assert_eq!(
            session.convo.messages().len(),
            2,
            "the whole transcript came back"
        );
        assert_eq!(a.sidebar.active_id(), CLOSED_CODE);
    });
}

/// A closed native thread whose saved route this build cannot read still
/// opens, on the SAME conversation, with no target. Choosing one in the
/// chooser repairs that same thread (no new conversation, transcript intact).
#[gpui::test]
fn closed_native_history_with_an_unknown_route_opens_and_repairs_the_same_thread(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("recovery_integration_tests::closed_native_history_with_an_unknown_route_opens_and_repairs_the_same_thread") {
        return;
    }
    cx.executor().allow_parking();
    let fx = Fx::new();
    seed_closed_code_history(&fx);
    let (app, _) = launch(cx, &fx);
    let conversations = app.read_with(cx, |a, _| a.conversations.all().len());
    app.update(cx, |a, cx| {
        a.open_conversation_from_history(UNKNOWN_CODE, cx)
    });
    settle(cx, &app, "the thread opened on its own id", |a| {
        a.sidebar.active_id() == UNKNOWN_CODE && native_tab_for(a, UNKNOWN_CODE).is_some()
    });
    let tab = app.read_with(cx, |a, _| {
        let tab = native_tab_for(a, UNKNOWN_CODE).unwrap();
        assert_eq!(
            a.composer_target_for(&ModelOwner::Native(tab)),
            None,
            "no target is shown for a route that cannot be read"
        );
        let session = a.work.code_tabs.iter().find(|t| t.id == tab).unwrap();
        assert_eq!(session.convo.messages().len(), 2);
        tab
    });
    let accepted = app.update(cx, |a, cx| {
        a.set_composer_target(
            &ModelOwner::Native(tab),
            agent(CodeEngine::Claude, Some("opus")),
            cx,
        )
    });
    assert!(accepted, "a valid choice repairs it");
    app.read_with(cx, |a, _| {
        assert_eq!(
            a.composer_target_for(&ModelOwner::Native(tab)),
            Some(agent(CodeEngine::Claude, Some("opus")))
        );
        assert_eq!(
            native_tab_for(a, UNKNOWN_CODE),
            Some(tab),
            "same tab, same conversation id"
        );
        assert_eq!(
            a.conversations.all().len(),
            conversations,
            "no thread was created"
        );
    });
    let saved = wait_stored(&format!("conversation_route:{UNKNOWN_CODE}"), |v| {
        v.is_some()
    })
    .unwrap();
    assert!(crate::conversation_route::ConversationRoute::from_persisted_json(&saved).is_ok());
}

// ---- 6. native -> in-app -> native in one conversation ----------------------------------

/// A loopback OpenAI-compatible server that answers each request with the next
/// scripted reply (the first line of a body, wrapped as one SSE completion),
/// and a health probe with "up".
fn scripted_model(replies: Vec<&'static str>) -> String {
    use std::io::{Read, Write};
    let script = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
        replies,
    )));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut data = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                if let Some(h) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&data[..h]).to_ascii_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if data.len() >= h + 4 + len {
                        break;
                    }
                }
            }
            let body = if data.starts_with(b"GET ") {
                r#"{"object":"list","data":[{"id":"m","object":"model"}]}"#.to_string()
            } else {
                let text = serde_json::to_string(
                    &script
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or("script exhausted"),
                )
                .unwrap();
                format!(
                    "data: {{\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":{text}}},\"finish_reason\":null}}]}}\n\n\
                     data: {{\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"\"}},\"finish_reason\":\"stop\"}}]}}\n\n\
                     data: [DONE]\n\n"
                )
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
        }
    });
    format!("http://{addr}")
}

/// The messages a viewer of the active Workshop surface would read.
fn visible_transcript(a: &AwiliApp) -> Vec<String> {
    match a.mode {
        Mode::Code => a.work.code_tabs[a.work.code_active]
            .convo
            .messages()
            .iter()
            .map(|m| m.text.clone())
            .collect(),
        _ => a.views[a.sidebar.active_id()]
            .convo
            .messages()
            .iter()
            .map(|m| m.text.clone())
            .collect(),
    }
}

fn workshop_tabs_of(a: &AwiliApp, conv: &str) -> usize {
    a.shell
        .tabs
        .tabs
        .iter()
        .filter(|t| match &t.backing {
            crate::workshop::Backing::Conversation(id) => id == conv,
            crate::workshop::Backing::Agent(session) => a
                .work
                .code_tabs
                .iter()
                .any(|s| s.id == *session && s.history.conv_id.as_deref() == Some(conv)),
        })
        .count()
}

/// A native (Code-backed) thread chooses the in-app model for one turn and
/// then Claude again. Throughout: the same visible Workshop tab, the same
/// conversation id, exactly one tab for it, a complete transcript wherever it
/// is read (live and reopened from history), and Claude resumes with exactly
/// the in-app pair it missed. Stop still ends a busy native turn afterwards.
#[gpui::test]
fn a_native_thread_goes_in_app_and_back_without_a_second_tab_or_a_stale_transcript(
    cx: &mut gpui::TestAppContext,
) {
    if !isolated("recovery_integration_tests::a_native_thread_goes_in_app_and_back_without_a_second_tab_or_a_stale_transcript") {
        return;
    }
    cx.executor().allow_parking();
    let fx = Fx::new();
    let base = scripted_model(vec!["in-app answer"]);
    let (app, _) = launch(cx, &fx);
    app.update(cx, |a, cx| {
        a.work.engine.base_url = base.clone();
        a.work.engine.model = "m".into();
        a.work.engine.label = "Local M".into();
        a.set_mode(Mode::Code, cx);
        a.new_code_tab(CodeEngine::Claude);
        a.work.code_active = a.work.code_tabs.len() - 1;
        a.work.code_tabs[a.work.code_active].project_folder =
            Some(fx.folder.path().display().to_string());
        a.sync_workshop_tabs();
    });
    let code_turn = |cx: &mut gpui::TestAppContext, text: &str, rows_after: usize| {
        app.update(cx, |a, cx| {
            a.composer.update(cx, |c, cx| c.set_text(text, cx));
            a.submit_composer_draft(cx);
        });
        settle(cx, &app, text, |a| {
            !a.work.running
                && a.work.code_tabs.iter().all(|t| !t.streaming)
                && visible_len(a) == rows_after
                && last_visible_is_reply(a)
        });
    };
    code_turn(cx, "first native question", 2);
    let (conv, workshop_tab, tab) = app.read_with(cx, |a, _| {
        let session = &a.work.code_tabs[a.work.code_active];
        (
            session
                .history
                .conv_id
                .clone()
                .expect("the turn stored a conversation"),
            a.shell.tabs.active.clone().expect("a visible Workshop tab"),
            session.id,
        )
    });

    // Choose the in-app model for the next turn: nothing is sent, nothing moves.
    let accepted = app.update(cx, |a, cx| {
        a.set_composer_target(&ModelOwner::Native(tab), WorkExecutor::InApp, cx)
    });
    assert!(accepted);
    app.read_with(cx, |a, _| {
        assert_eq!(a.shell.tabs.active.as_deref(), Some(workshop_tab.as_str()));
        assert_eq!(visible_len(a), 2);
    });
    code_turn(cx, "in-app question", 4);
    app.read_with(cx, |a, _| {
        assert_eq!(a.sidebar.active_id(), conv, "same conversation");
        assert_eq!(
            a.shell.tabs.active.as_deref(),
            Some(workshop_tab.as_str()),
            "same visible tab"
        );
        assert_eq!(
            workshop_tabs_of(a, &conv),
            1,
            "one tab for the conversation"
        );
        let seen = visible_transcript(a);
        assert_eq!(seen.len(), 4, "{seen:?}");
        assert!(
            seen[0].contains("first native question") && seen[1].contains("claude answer 1"),
            "{seen:?}"
        );
        assert!(
            seen[2].contains("in-app question") && seen[3].contains("in-app answer"),
            "{seen:?}"
        );
    });

    // And back to Claude: it resumes its own session and is told only what it missed.
    let accepted = app.update(cx, |a, cx| {
        let owner = if a.mode == Mode::Code {
            ModelOwner::Native(a.work.code_tabs[a.work.code_active].id)
        } else {
            ModelOwner::Work(a.sidebar.active_id().to_string())
        };
        a.set_composer_target(&owner, agent(CodeEngine::Claude, None), cx)
    });
    assert!(
        accepted,
        "the chooser still works after the thread moved to the Work adapter"
    );
    code_turn(cx, "back to claude", 6);
    let claude = fx.calls("claude");
    assert_eq!(claude.len(), 2);
    assert!(
        has_pair(&claude[1], "--resume", "sess-claude-1"),
        "{:?}",
        claude[1]
    );
    let back = task_arg(&claude[1], "back to claude");
    assert_eq!(count(back, "in-app question"), 1, "{back}");
    assert_eq!(count(back, "in-app answer"), 1, "{back}");
    assert!(!back.contains("first native question"), "{back}");

    // Reopened from history, the thread shows all six rows, in one tab.
    app.update(cx, |a, cx| a.open_conversation_from_history(&conv, cx));
    cx.run_until_parked();
    app.read_with(cx, |a, _| {
        assert_eq!(
            visible_transcript(a).len(),
            6,
            "{:?}",
            visible_transcript(a)
        );
        assert_eq!(workshop_tabs_of(a, &conv), 1);
        assert_eq!(a.shell.tabs.active.as_deref(), Some(workshop_tab.as_str()));
    });

    // Stop ends a busy turn of the same thread.
    fx.flag("hold");
    app.update(cx, |a, cx| {
        a.composer
            .update(cx, |c, cx| c.set_text("this one is held", cx));
        a.submit_composer_draft(cx);
    });
    settle(cx, &app, "the held turn started", |_a| {
        fx.calls("claude").len() == 3
    });
    app.update(cx, |a, cx| a.abort_active_stream(cx));
    settle(cx, &app, "Stop ended it", |a| {
        !a.work.running && a.work.code_tabs.iter().all(|t| !t.streaming)
    });
    app.read_with(cx, |a, _| assert_eq!(workshop_tabs_of(a, &conv), 1));
}

fn visible_len(a: &AwiliApp) -> usize {
    visible_transcript(a).len()
}

fn last_visible_is_reply(a: &AwiliApp) -> bool {
    visible_transcript(a)
        .last()
        .is_some_and(|text| !text.is_empty())
}

/// "New" with an explicit model: each fresh thread is chosen onto a named
/// model before its first send, and the process that starts carries that model
/// exactly (`--model` on Claude's argv; `DSH_MODEL` for dsh, which has no
/// flag). Every harness binary is a stub or a path that does not exist, so
/// nothing installed can run.
#[gpui::test]
fn a_new_thread_with_an_explicit_model_launches_with_that_model(cx: &mut gpui::TestAppContext) {
    if !isolated(
        "recovery_integration_tests::a_new_thread_with_an_explicit_model_launches_with_that_model",
    ) {
        return;
    }
    cx.executor().allow_parking();
    let fx = Fx::new();
    let (app, first) = launch(cx, &fx);
    let mut threads = vec![first];
    for (n, (engine, model, ask)) in [
        (
            CodeEngine::Claude,
            "claude-opus-explicit",
            "ask the named claude",
        ),
        (CodeEngine::Dsh, "deepseek-v4-explicit", "ask the named dsh"),
    ]
    .into_iter()
    .enumerate()
    {
        let conv = app.update(cx, |a, cx| {
            let (id, _) = a.fresh_conversation_with(awili_core::ConvKind::Work, None);
            a.open_conversation_from_history(&id, cx);
            id
        });
        if n == 0 {
            // The shown first thread was never sent, so New reuses that blank
            // thread by design (no empty "New conversation" rows pile up).
            assert_eq!(
                threads.as_slice(),
                std::slice::from_ref(&conv),
                "New on a blank thread reuses it"
            );
        } else {
            // The first thread has now been sent, so New is a thread of its own.
            assert!(!threads.contains(&conv), "New is a thread of its own");
            threads.push(conv.clone());
        }
        choose(cx, &app, &conv, agent(engine, Some(model)));
        turn(cx, &app, &conv, ask);
    }
    let claude = fx.calls("claude");
    assert_eq!(claude.len(), 1, "only the chosen harness ran");
    assert!(
        has_pair(&claude[0], "--model", "claude-opus-explicit"),
        "{:?}",
        claude[0]
    );
    let dsh = fx.calls("dsh");
    assert_eq!(dsh.len(), 1);
    assert_eq!(fx.dsh_model(1), "deepseek-v4-explicit");
    assert!(
        !dsh[0].iter().any(|a| a.contains("deepseek-v4-explicit")),
        "dsh takes its model from the environment, never argv: {:?}",
        dsh[0]
    );
}

/// New on a native harness with an explicit model (own file).
#[path = "recovery_native_new_tests.rs"]
mod native_new;
