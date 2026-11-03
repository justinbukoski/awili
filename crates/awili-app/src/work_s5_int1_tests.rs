//! S5 INT1 (answers the RVS5 slice review, two P1s): the Work AGENT lane
//! awaits the memory gate before anything is spawned, and both the Work agent
//! lane and Chat pin the gate's validated, ordered policy block into the
//! prompt the agent CLI receives. End to end over the real `AwiliApp`
//! (`submit_work_run`, `start_turn`), the real production gate
//! (`production_client` resolving `$HOME/.config/foreman/memory-backend.conf`
//! to a loopback v6.5 stub) and a fake `claude` binary that records its
//! prompt. Each app test runs in a private-HOME subprocess (`isolated`).

use super::gateway_queue_tests::isolated;
use super::s5a_policy_tests::{accepted_body, stub};
use super::*;
use crate::recall::{open_gated_stream, pin_policy};
use crate::MemoryCfg;
use awili_memory::RulesStatus;
use awili_providers::{ChatProvider, ChatRequest, ProviderEvent};
use futures::stream::{BoxStream, StreamExt};
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const CONV: &str = "s5-int1-conv";

/// The ordered markers `accepted_body()` renders (10 directives, fence tag
/// on item 3 stripped, item 5 a JSON object).
const MARKERS: [&str; 10] = [
    "1. d0",
    "2. d1",
    "3. d2",
    "4. keep",
    "5. d4",
    "6. {\"rule\":\"json-shaped\"}",
    "7. d6",
    "8. d7",
    "9. d8",
    "10. d9",
];

/// Assert every marker appears in `s`, in order.
fn assert_ordered_block(s: &str) {
    let mut prev = 0usize;
    for m in &MARKERS {
        let at = s
            .find(m)
            .unwrap_or_else(|| panic!("policy block missing {m:?}: {s}"));
        assert!(
            at >= prev,
            "policy block reorders: {m:?} at {at} after {prev}"
        );
        prev = at + 1;
    }
    assert!(
        !s.to_ascii_lowercase().contains("past-context"),
        "the fence tag is sanitized: {s}"
    );
}

/// Point the production memory client at a v6.5 stub answering `body`:
/// write `$HOME/.config/foreman/memory-backend.conf` + a reader token, and
/// return the enabled `MemoryCfg` plus the stub's request counter.
fn memory_home(
    home: &std::path::Path,
    body: String,
) -> (MemoryCfg, Arc<std::sync::atomic::AtomicUsize>) {
    let (base, count) = stub(body);
    let foreman = home.join(".config/foreman");
    std::fs::create_dir_all(&foreman).unwrap();
    let token = foreman.join("v6-reader-token");
    std::fs::write(&token, "reader\n").unwrap();
    std::fs::write(
        foreman.join("memory-backend.conf"),
        format!(
            "FOREMAN_MEMORY_BACKEND=v65\nFOREMAN_MEMORY_V65_BASE={base}\nFOREMAN_MEMORY_V65_READ_TOKEN={}\n",
            token.display()
        ),
    )
    .unwrap();
    let mem = MemoryCfg {
        base_url: base,
        token: String::new(),
        min_priority: 90,
        enabled: true,
        recall: true,
        write: false,
    };
    (mem, count)
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn fake_claude(dir: &std::path::Path) -> std::path::PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` in `dir`: touches `ran`, writes its LAST argv (the
/// prompt) to `prompt`, answers one Claude stream-json turn, exits 0.
fn fake_claude(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    let script = format!(
        "#!/bin/sh\n\
         : > {ran}\n\
         prev=\"\"\n\
         cat > {prompt}\n\
         for a in \"$@\"; do \
         if [ \"$prev\" = \"--append-system-prompt\" ]; then printf '%s' \"$a\" > {system}; fi; \
         if [ \"$prev\" = \"--append-system-prompt-file\" ]; then cat \"$a\" > {system}; fi; \
         prev=\"$a\"; done\n\
         printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"agent answer\"}}]}}}}'\n\
         printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"agent answer\"}}'\n",
        ran = dir.join("ran").display(),
        prompt = dir.join("prompt").display(),
        system = dir.join("system").display(),
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Choose `target` for `CONV`'s next turn the way the chooser does. A send
/// follows the conversation's route; writing `work.executor` selects nothing.
fn pick(app: &mut AwiliApp, target: WorkExecutor, cx: &mut Context<AwiliApp>) {
    assert!(
        app.set_composer_target(
            &crate::workshop_composer::ModelOwner::Work(CONV.into()),
            target,
            cx
        ),
        "the chooser accepts a target for the shown Work thread"
    );
}

/// Make `app` a Workshop session on conversation `CONV` whose agent lane is
/// the fake claude in `dir` (also the bound folder), with `mem` as memory.
fn wire_agent(
    app: &mut AwiliApp,
    home: &std::path::Path,
    dir: &std::path::Path,
    mem: MemoryCfg,
    kind: awili_core::ConvKind,
    cx: &mut Context<AwiliApp>,
) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.memory = mem;
    app.work.agents.claude_bin = Some(fake_claude(dir).display().to_string());
    app.work.folder = Some(dir.to_path_buf());
    app.work
        .graduation
        .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
    app.views.insert(
        CONV.into(),
        crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
    );
    crate::exec_fixture::seed_conversation_metadata(app, CONV, kind);
    app.set_active_conversation(CONV);
    app.mode = Mode::Work;
    pick(
        app,
        WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        },
        cx,
    );
}

/// Pump the gpui executor (parking allowed: the gate and the agent reader
/// report from their own threads) until `done`, or fail after 20 s.
fn settle(
    cx: &mut gpui::TestAppContext,
    app: &gpui::Entity<AwiliApp>,
    done: impl Fn(&AwiliApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if app.read_with(cx, |a, _| done(a)) {
            return;
        }
        assert!(Instant::now() < deadline, "the app never settled");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// P1 #1: a malformed required policy (`mandatory_policy: null`) refuses a
/// Work AGENT turn before any spawn: the fake claude never runs, the turn
/// ends Failed with the shared refusal text, and the rules chip shows
/// rejected.
#[gpui::test]
fn work_agent_rejected_policy_never_spawns_agent(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::s5_int1_tests::work_agent_rejected_policy_never_spawns_agent")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (mem, count) = memory_home(
        &home,
        r#"{"mandatory_policy": null, "items": []}"#.to_string(),
    );
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.submit_work_run("task".into(), cx);
    });
    settle(cx, &entity, |a| {
        !a.work.running && a.work.panel.status.is_some()
    });
    assert!(
        !dir.path().join("ran").exists(),
        "a rejected policy must not spawn the agent"
    );
    assert!(
        count.load(AtomicOrdering::SeqCst) >= 1,
        "the memory gate was asked"
    );
    entity.read_with(cx, |app, _| {
        let err = app.work.panel.error.clone().unwrap_or_default();
        assert!(
            err.starts_with("memory policy rejected — turn not sent: "),
            "the agent lane shows the shared refusal: {err:?}"
        );
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        assert!(app.work.agent_abort_tx.is_none(), "no agent run was armed");
        assert_eq!(app.rules, Some(RulesStatus::Rejected));
        let last = app.views[CONV]
            .convo
            .messages()
            .last()
            .map(|m| m.text.clone())
            .unwrap_or_default();
        assert!(
            last.contains("memory policy rejected — turn not sent: "),
            "the transcript shows the refusal: {last:?}"
        );
    });
}

/// P1 #1 (accepted side): the Work agent lane's prompt leads with the
/// gate's validated, ordered policy block, then the task.
#[gpui::test]
fn work_agent_accepted_policy_reaches_agent_prompt(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::s5_int1_tests::work_agent_accepted_policy_reaches_agent_prompt")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (mem, _count) = memory_home(&home, accepted_body());
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.submit_work_run("int1 work task".into(), cx);
    });
    settle(cx, &entity, |a| {
        !a.work.running && a.work.panel.status.is_some()
    });
    // RC1-M: Claude takes Awili's text through its system channel: the
    // policy block leads `--append-system-prompt` (a household install: no
    // `[work.agents.memory]`, so the preamble is kept), and the prompt
    // carries the task alone.
    let system = std::fs::read_to_string(dir.path().join("system"))
        .expect("the agent ran and recorded its appended system text");
    assert_ordered_block(&system);
    assert!(
        system.find("10. d9").unwrap() < system.find("How to answer in Awili").unwrap(),
        "the policy block leads the appended system text: {system}"
    );
    let prompt = crate::test_stdin::read_prompt(&dir.path().join("prompt"))
        .expect("the agent ran and recorded its prompt");
    assert!(
        prompt.contains("int1 work task"),
        "the task reaches the agent: {prompt}"
    );
    assert!(
        !prompt.contains("10. d9"),
        "the policy is not in the prompt: {prompt}"
    );
    // RC1-M-FIX6: the planning step's text rides in the prompt (Claude keeps
    // appended system text across `--resume`); nothing else of Awili's does.
    // RC2-R4 (R3-F9b, R5-6): then Claude's plan-in-the-reply line
    // (deliberate test change).
    assert!(
        !prompt.contains("[System context]")
            || prompt.starts_with(&format!(
                "[System context]\n{}\n{}\n\n",
                awili_code::PLAN_FIRST_SYSTEM,
                crate::agent_launch::CLAUDE_PLAN_IN_REPLY
            )),
        "{prompt}"
    );
    assert!(!system.contains(awili_code::PLAN_FIRST_SYSTEM), "{system}");
    entity.read_with(cx, |app, _| {
        assert_eq!(app.work.panel.error, None);
        assert_eq!(app.rules, Some(RulesStatus::Loaded(10)));
    });
}

/// A provider that captures every request handed to `stream`.
struct Capture(Arc<Mutex<Vec<ChatRequest>>>);

impl ChatProvider for Capture {
    fn stream(&self, request: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        self.0.lock().unwrap().push(request);
        futures::stream::iter(vec![ProviderEvent::Done]).boxed()
    }
}

/// P1 #2 at the routing seam: an accepted gate's block is pinned into the
/// captured request's `system`, ahead of the lane's own system text; the
/// messages are untouched; an empty policy pins nothing.
#[test]
fn gated_request_pins_policy_ahead_of_lane_system() {
    let block = "Standing directives:\n1. first rule\n2. second rule".to_string();
    let capture = Arc::new(Mutex::new(Vec::new()));
    let provider = Capture(Arc::clone(&capture));
    let request = ChatRequest {
        model: "m".into(),
        system: Some("LANE SYSTEM".into()),
        messages: vec![awili_providers::ChatMessage::user("question")],
    };
    let _: Vec<ProviderEvent> = futures::executor::block_on(
        open_gated_stream(&provider, request, &Ok(Some(block.clone()))).collect(),
    );
    let got = capture.lock().unwrap().pop().expect("one request");
    assert_eq!(
        got.system.as_deref(),
        Some(format!("{block}\n\nLANE SYSTEM").as_str()),
        "the policy block leads, the lane's system follows"
    );
    assert_eq!(got.messages.len(), 1);
    assert_eq!(got.messages[0].content, "question");

    let mut bare = ChatRequest {
        model: "m".into(),
        system: None,
        messages: Vec::new(),
    };
    pin_policy(&mut bare, Some("  "));
    assert_eq!(bare.system, None, "a blank policy pins nothing");
    pin_policy(&mut bare, Some(block.as_str()));
    assert_eq!(bare.system.as_deref(), Some(block.as_str()));
}

// ---- S5 INT1-FIX1: Stop during the gate wait; a late gate after Stop ----

/// A v6.5 stub whose first `hold_n` requests wait until [`Held::release`]
/// and then answer `held_body`; every later request answers `later_body`
/// at once. `seen` counts arrivals, `served` counts written answers.
struct Held {
    seen: std::sync::atomic::AtomicUsize,
    served: std::sync::atomic::AtomicUsize,
    open: Mutex<bool>,
    cv: std::sync::Condvar,
}

impl Held {
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

fn held_stub(held_body: String, later_body: String, hold_n: usize) -> (String, Arc<Held>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let held = Arc::new(Held {
        seen: std::sync::atomic::AtomicUsize::new(0),
        served: std::sync::atomic::AtomicUsize::new(0),
        open: Mutex::new(false),
        cv: std::sync::Condvar::new(),
    });
    let owned = Arc::clone(&held);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let held = Arc::clone(&owned);
            let (held_body, later_body) = (held_body.clone(), later_body.clone());
            std::thread::spawn(move || {
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                while data.windows(4).all(|w| w != b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                }
                let header = String::from_utf8_lossy(&data).to_ascii_lowercase();
                let v6 =
                    header.contains("post /v6/bootstrap") || header.contains("post /v6/recall");
                let (status, body) = if v6 {
                    let n = held.seen.fetch_add(1, AtomicOrdering::SeqCst);
                    if n < hold_n {
                        let mut open = held.open.lock().unwrap();
                        while !*open {
                            open = held.cv.wait(open).unwrap();
                        }
                        ("200 OK", held_body)
                    } else {
                        ("200 OK", later_body)
                    }
                } else {
                    ("404 Not Found", String::from("{}"))
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                if v6 {
                    held.served.fetch_add(1, AtomicOrdering::SeqCst);
                }
            });
        }
    });
    (format!("http://{addr}"), held)
}

/// [`memory_home`] over a [`held_stub`].
fn held_memory_home(
    home: &std::path::Path,
    held_body: String,
    later_body: String,
    hold_n: usize,
) -> (MemoryCfg, Arc<Held>) {
    let (base, held) = held_stub(held_body, later_body, hold_n);
    let foreman = home.join(".config/foreman");
    std::fs::create_dir_all(&foreman).unwrap();
    let token = foreman.join("v6-reader-token");
    std::fs::write(&token, "reader\n").unwrap();
    std::fs::write(
        foreman.join("memory-backend.conf"),
        format!(
            "FOREMAN_MEMORY_BACKEND=v65\nFOREMAN_MEMORY_V65_BASE={base}\nFOREMAN_MEMORY_V65_READ_TOKEN={}\n",
            token.display()
        ),
    )
    .unwrap();
    let mem = MemoryCfg {
        base_url: base,
        token: String::new(),
        min_priority: 90,
        enabled: true,
        recall: true,
        write: false,
    };
    (mem, held)
}

/// Pump the gpui executor until `cond` holds, or fail after 20 s.
fn pump_until(cx: &mut gpui::TestAppContext, what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if cond() {
            return;
        }
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// How many agent-lane gate answers the UI thread has processed (the
/// `TEST_AGENT_GATE_ANSWERS` marker, bumped first thing in the gate
/// callback, before its stale check).
fn gate_answers() -> usize {
    TEST_AGENT_GATE_ANSWERS.with(|n| n.get())
}

/// The last Work transcript row's text.
fn last_text(app: &AwiliApp) -> String {
    app.views[CONV]
        .convo
        .messages()
        .last()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

/// P1: Stop while the agent turn waits on its memory gate settles the turn
/// at once — Stopped, the streaming row finished, one ledger line — and the
/// gate's late (accepted) answer starts no agent and touches nothing.
#[gpui::test]
fn work_agent_stop_during_gate_settles_turn_as_stopped(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::s5_int1_tests::work_agent_stop_during_gate_settles_turn_as_stopped")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (mem, held) = held_memory_home(&home, accepted_body(), accepted_body(), 2);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.submit_work_run("int1 stopped task".into(), cx);
        assert!(app.work.running, "the gate wait counts as running");
        assert!(
            app.work.agent_gate_pending,
            "the agent turn waits on its gate"
        );
        assert!(app.work.agent_abort_tx.is_none(), "no agent child yet");
        app.cancel_work_run(cx);
    });
    let before = entity.read_with(cx, |app, _| {
        assert_eq!(
            app.work.stream_row_idx, None,
            "Stop during the gate finishes the streaming row"
        );
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
        assert!(!app.work.running);
        assert!(!app.work.agent_gate_pending, "the pending gate is settled");
        assert_eq!(last_text(app), "*(stopped before any text)*");
        (app.rules.clone(), app.recall.clone())
    });
    let ledger = std::fs::read_to_string(home.join("ledger.jsonl")).unwrap_or_default();
    assert_eq!(ledger.lines().count(), 1, "one ledger line: {ledger}");
    let entry: awili_selftest::graduation::LedgerEntry =
        serde_json::from_str(ledger.lines().next().unwrap_or_default())
            .expect("ledger line parses");
    assert_eq!(
        entry.outcome,
        awili_selftest::graduation::Outcome::Stopped,
        "{ledger}"
    );
    // The gate's requests arrive and are held; release the late answer.
    pump_until(cx, "the gate asked memory", || {
        held.seen.load(AtomicOrdering::SeqCst) >= 2
    });
    assert_eq!(gate_answers(), 0, "the held gate has not answered yet");
    held.release();
    pump_until(cx, "the late gate answer was written", || {
        held.served.load(AtomicOrdering::SeqCst) >= 2
    });
    // Deterministic: the late answer's callback has run on the UI thread.
    pump_until(cx, "the late gate answer was processed", || {
        gate_answers() >= 1
    });
    assert!(
        !dir.path().join("ran").exists(),
        "the late gate answer must not start the agent"
    );
    entity.read_with(cx, |app, _| {
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
        assert!(!app.work.running);
        assert_eq!(
            (app.rules.clone(), app.recall.clone()),
            before,
            "a stale gate touches no chip"
        );
        assert_eq!(last_text(app), "*(stopped before any text)*");
    });
    let ledger = std::fs::read_to_string(home.join("ledger.jsonl")).unwrap_or_default();
    assert_eq!(ledger.lines().count(), 1, "still one ledger line: {ledger}");
}

/// P2: after Stop and a resubmit, the FIRST turn's late gate answer must not
/// overwrite the current turn's rules/recall chips or its panel.
#[gpui::test]
fn late_gate_after_stop_and_resubmit_leaves_current_chips(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::s5_int1_tests::late_gate_after_stop_and_resubmit_leaves_current_chips")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    // Turn 1's two requests are held (accepted body); turn 2's answer at
    // once with a rejected policy.
    let (mem, held) = held_memory_home(
        &home,
        accepted_body(),
        r#"{"mandatory_policy": null, "items": []}"#.to_string(),
        2,
    );
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.submit_work_run("int1 first task".into(), cx);
        app.cancel_work_run(cx);
    });
    pump_until(cx, "turn 1's gate asked memory", || {
        held.seen.load(AtomicOrdering::SeqCst) >= 2
    });
    entity.update(cx, |app, cx| {
        app.submit_work_run("int1 second task".into(), cx);
        assert!(app.work.agent_gate_pending, "turn 2 waits on its own gate");
    });
    settle(cx, &entity, |a| {
        !a.work.running && a.work.panel.status == Some(RunStatus::Failed)
    });
    let current = entity.read_with(cx, |app, _| {
        assert_eq!(app.rules, Some(RulesStatus::Rejected), "turn 2's chip");
        (
            app.rules.clone(),
            app.recall.clone(),
            app.work.panel.error.clone(),
        )
    });
    assert_eq!(gate_answers(), 1, "only turn 2's gate has answered");
    held.release();
    pump_until(cx, "turn 1's late answer was written", || {
        held.served.load(AtomicOrdering::SeqCst) >= 4
    });
    // Deterministic: turn 1's late answer's callback has run on the UI thread.
    pump_until(cx, "turn 1's late answer was processed", || {
        gate_answers() >= 2
    });
    entity.read_with(cx, |app, _| {
        assert_eq!(
            (
                app.rules.clone(),
                app.recall.clone(),
                app.work.panel.error.clone()
            ),
            current,
            "turn 1's late gate must not overwrite turn 2's chips or panel"
        );
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        assert!(!app.work.running);
    });
    assert!(
        !dir.path().join("ran").exists(),
        "neither turn may start the agent"
    );
}

/// R1d (priority 4): the S5 refusal of a Work agent turn also shows a plain,
/// jargon-free notice that stays; the panel and transcript keep S5's text.
#[gpui::test]
fn r1d_policy_refusal_shows_a_plain_notice(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::s5_int1_tests::r1d_policy_refusal_shows_a_plain_notice")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (mem, _count) = memory_home(
        &home,
        r#"{"mandatory_policy": null, "items": []}"#.to_string(),
    );
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.submit_work_run("task".into(), cx);
    });
    settle(cx, &entity, |a| {
        !a.work.running && a.work.panel.status.is_some()
    });
    entity.read_with(cx, |app, _| {
        let toast = app
            .toast
            .msg
            .as_ref()
            .map(|m| m.to_string())
            .unwrap_or_default();
        assert_eq!(toast, crate::submit_guard::POLICY_REFUSED_NOTICE);
        assert!(!toast.contains("policy rejected"), "plain words: {toast}");
        let err = app.work.panel.error.clone().unwrap_or_default();
        assert!(
            err.starts_with("memory policy rejected — turn not sent: "),
            "{err}"
        );
    });
}

/// RC1-M ("prove which one to keep", request dumps 2026-10-03), RC1-M-FIX6/
/// FIX7: on the owner's install the Work agent lane leaves the RECALL to his
/// own Claude Code hook only when he says so (`[work.agents.memory]
/// hooks_provide_recall = true`); RC1-N-FIX1 (`hooks_provide_memory`): then
/// Awili pins nothing — neither the recall nor the standing directives. With
/// the default the whole preamble is pinned, even with a hook configured. The gate runs in both (its chips stay
/// current).
#[gpui::test]
fn rc1m_owner_work_agent_lane_leaves_memory_to_his_hooks(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::s5_int1_tests::rc1m_owner_work_agent_lane_leaves_memory_to_his_hooks")
    else {
        return;
    };
    cx.executor().allow_parking();
    let launch_home = home.join("launch");
    std::fs::create_dir_all(launch_home.join(".claude")).unwrap();
    crate::agent_launch::set_test_paths(Some(crate::agent_launch::LaunchPaths {
        codex_home: launch_home.join(".codex"),
        home: launch_home.clone(),
    }));
    let body = accepted_body().replace(
        r#""items": []"#,
        r#""items": [{"id": "m1", "object": {"content": "RECALLED-NOTE"}}]"#,
    );
    let (mem, _count) = memory_home(&home, body);
    for hook in [false, true] {
        // A configured hook must not matter; only the setting does.
        std::fs::write(
            launch_home.join(".claude/settings.json"),
            serde_json::json!({ "hooks": { "UserPromptSubmit": [
                { "hooks": [ { "type": "command", "command": "memory-hook" } ] } ] } })
            .to_string(),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let entity = cx.new(AwiliApp::new);
        let mem = mem.clone();
        entity.update(cx, |app, cx| {
            wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
            app.work.agents.memory = Some(awili_config::AgentMemoryConfig {
                hooks_provide_memory: hook,
                ..Default::default()
            });
            app.submit_work_run("int1 work task".into(), cx);
        });
        settle(cx, &entity, |a| {
            !a.work.running && a.work.panel.status.is_some()
        });
        let system = std::fs::read_to_string(dir.path().join("system"))
            .expect("the agent ran and recorded its appended system text");
        let prompt = crate::test_stdin::read_prompt(&dir.path().join("prompt")).unwrap();
        // RC1-N-FIX1: with the owner's hooks bringing his memory, Awili
        // sends neither the standing directives nor the recall.
        let mut prev = 0usize;
        for m in MARKERS {
            if hook {
                assert!(
                    !system.contains(m),
                    "hook=true: no directives {m:?}: {system}"
                );
                continue;
            }
            let at = system[prev..]
                .find(m)
                .unwrap_or_else(|| panic!("hook={hook}: the policy keeps {m:?}: {system}"));
            prev += at + 1;
        }
        for m in MARKERS {
            assert!(!prompt.contains(m), "the prompt carries {m:?}: {prompt}");
        }
        assert_eq!(
            system.contains("RECALLED-NOTE"),
            !hook,
            "hooks_provide_memory={hook}: the recall is left out only when the owner says so: {system}"
        );
        assert!(!prompt.contains("RECALLED-NOTE"), "{prompt}");
        assert!(system.contains("How to answer in Awili"), "{system}");
        entity.read_with(cx, |app, _| {
            assert_eq!(app.rules, Some(RulesStatus::Loaded(10)), "the gate ran");
        });
    }
    crate::agent_launch::set_test_paths(None);
}

/// RC1-M: the in-app engine's system prompt is Awili's memory preamble, then
/// the user-level and project AGENTS.md files, then its brief — also when the
/// gate hands the prompt straight to the LIVE worker (before RC1-M that path
/// carried the preamble alone: no brief from the second turn on). No
/// CLAUDE.md.
#[gpui::test]
fn rc1m_live_inapp_worker_gets_agents_md_and_brief(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::s5_int1_tests::rc1m_live_inapp_worker_gets_agents_md_and_brief")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("proj");
    std::fs::create_dir_all(folder.join(".git")).unwrap();
    std::fs::write(folder.join("AGENTS.md"), "PROJECT-AGENTS-FACT\n").unwrap();
    std::fs::write(folder.join("CLAUDE.md"), "PROJECT-CLAUDE-RULE\n").unwrap();
    let user = dir.path().join("user-AGENTS.md");
    std::fs::write(&user, "USER-AGENTS-RULE\n").unwrap();
    let (mem, _count) = memory_home(&home, accepted_body());
    let (tx, rx) = std::sync::mpsc::channel::<crate::work::GatewayCmd>();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, &folder, mem, awili_core::ConvKind::Work, cx);
        pick(app, WorkExecutor::InApp, cx);
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "m".into();
        app.work.agents.agents_md = Some(user.display().to_string());
        app.work.gateway_cmd_tx = Some(tx); // the live worker
        app.work.worker_conv = Some(CONV.into());
        app.submit_work_run("int1 in-app task".into(), cx);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let system = loop {
        cx.run_until_parked();
        if let Ok(crate::work::GatewayCmd::Prompt { system, .. }) = rx.try_recv() {
            break system.expect("a system prompt");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the live worker got no prompt"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_ordered_block(&system);
    let at = |needle: &str| {
        system
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing: {system}"))
    };
    assert!(at("10. d9") < at("USER-AGENTS-RULE"), "{system}");
    assert!(
        at("USER-AGENTS-RULE") < at("PROJECT-AGENTS-FACT"),
        "{system}"
    );
    assert!(
        at("PROJECT-AGENTS-FACT") < at("Your file tools work in"),
        "{system}"
    );
    assert!(!system.contains("PROJECT-CLAUDE-RULE"), "{system}");
    assert_eq!(
        system.matches("How to answer in Awili").count(),
        1,
        "{system}"
    );
}

/// RC1-M-FIX1: on the owner's install the in-app engine gets the memory
/// preamble although `[memory]` is off (his `base_url` is empty): the S5
/// gate runs (one policy recall + one recall) and its block leads the
/// system text the live worker receives.
#[gpui::test]
fn rc1m_fix1_owner_install_in_app_turn_gets_the_memory_preamble(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::s5_int1_tests::rc1m_fix1_owner_install_in_app_turn_gets_the_memory_preamble",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (mem, _count) = memory_home(&home, accepted_body());
    let (tx, rx) = std::sync::mpsc::channel::<crate::work::GatewayCmd>();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.memory.enabled = false;
        app.work.agents.memory = Some(awili_config::AgentMemoryConfig::default());
        pick(app, WorkExecutor::InApp, cx);
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "m".into();
        app.work.gateway_cmd_tx = Some(tx);
        app.work.worker_conv = Some(CONV.into());
        app.submit_work_run("int1 in-app task".into(), cx);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let system = loop {
        cx.run_until_parked();
        if let Ok(crate::work::GatewayCmd::Prompt { system, .. }) = rx.try_recv() {
            break system.expect("a system prompt");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the live worker got no prompt"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_ordered_block(&system);
    assert!(
        system.ends_with(crate::agent_brief::OWNED_MEMORY_LINE),
        "{system}"
    );
    // RC1-M-FIX3 (RC1-M-FIX1 review P2): the indicators follow that gate.
    entity.read_with(cx, |app, _| {
        assert_eq!(app.rules, Some(RulesStatus::Loaded(10)), "the rules chip");
        assert!(app.recall.is_some(), "the recall chip");
    });
}

#[cfg(unix)]
/// RC1-M-FIX8 (whole-slice review P1, the Work path): a DeepSeek Work turn
/// through the real Work lane gets the owner's user AGENTS.md and the
/// project's AGENTS.md exactly once each — from its launch, no longer also
/// from the task prefix — and never a CLAUDE.md.
#[gpui::test]
fn rc1m_fix8_deepseek_work_turn_gets_agents_md_once(cx: &mut gpui::TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    let Some(home) =
        isolated("work::s5_int1_tests::rc1m_fix8_deepseek_work_turn_gets_agents_md_once")
    else {
        return;
    };
    cx.executor().allow_parking();
    let launch_home = home.join("launch");
    std::fs::create_dir_all(launch_home.join(".agents")).unwrap();
    std::fs::write(launch_home.join(".agents/AGENTS.md"), "USER-AGENTS-RULE\n").unwrap();
    crate::agent_launch::set_test_paths(Some(crate::agent_launch::LaunchPaths {
        codex_home: launch_home.join(".codex"),
        home: launch_home.clone(),
    }));
    let dir = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(dir.path()).unwrap().join("proj");
    std::fs::create_dir_all(folder.join(".git")).unwrap();
    std::fs::write(folder.join("AGENTS.md"), "PROJECT-AGENTS-FACT\n").unwrap();
    std::fs::write(folder.join("CLAUDE.md"), "PROJECT-CLAUDE-RULE\n").unwrap();
    let bin = dir.path().join("dsh");
    let prompt = dir.path().join("prompt");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\ncat > {}\necho done\n", prompt.display()),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.agents.dsh_bin = Some(bin.display().to_string());
        app.work.agents.memory = Some(awili_config::AgentMemoryConfig::default());
        app.work.folder = Some(folder.clone());
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        crate::exec_fixture::seed_conversation_metadata(app, CONV, awili_core::ConvKind::Work);
        app.set_active_conversation(CONV);
        app.mode = Mode::Work;
        pick(
            app,
            WorkExecutor::Agent {
                engine: CodeEngine::Dsh,
                model: None,
            },
            cx,
        );
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(CONV.into()),
            awili_work::ApprovalMode::Auto,
        );
        app.submit_work_run("int1 deepseek task".into(), cx);
    });
    settle(cx, &entity, |a| {
        !a.work.running && a.work.panel.status.is_some()
    });
    let p = std::fs::read_to_string(&prompt).expect("dsh ran and recorded its prompt");
    assert_eq!(p.matches("USER-AGENTS-RULE").count(), 1, "{p}");
    assert_eq!(p.matches("PROJECT-AGENTS-FACT").count(), 1, "{p}");
    assert!(!p.contains("PROJECT-CLAUDE-RULE"), "{p}");
    assert!(p.contains("int1 deepseek task"), "{p}");
    crate::agent_launch::set_test_paths(None);
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn streaming_claude(dir: &std::path::Path) -> std::path::PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` in `dir` that writes its pid to `pid`, streams the start
/// of a reply as partial text deltas (the `--include-partial-messages`
/// shape) and then keeps writing for a long time — the reply is never
/// complete unless the run is stopped.
fn streaming_claude(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude-streaming");
    let delta = |text: &str| {
        format!(
            "printf '%s\\n' '{{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{text}\"}}}}}}'\n"
        )
    };
    let script = format!(
        "#!/bin/sh\n\
         echo $$ > {pid}\n\
         printf '%s\\n' '{{\"type\":\"stream_event\",\"parent_tool_use_id\":null,\"event\":{{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}}}'\n\
         {d1}{d2}\
         exec sleep 30\n",
        pid = dir.join("pid").display(),
        d1 = delta("Once upon"),
        d2 = delta(" a time"),
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// RC2-RUN (R3-20, R6-6): a Claude Work reply shows its text while it is
/// being written (not only when complete), and Stop in the middle of it
/// ends the run at once — the agent process is gone, the text already shown
/// stays in the reply, and the run reads Stopped.
#[gpui::test]
fn rc2_run_claude_work_reply_streams_and_stops_mid_stream(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::s5_int1_tests::rc2_run_claude_work_reply_streams_and_stops_mid_stream")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (mem, _count) = memory_home(&home, accepted_body());
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.work.agents.claude_bin = Some(streaming_claude(dir.path()).display().to_string());
        app.submit_work_run("tell me a long story".into(), cx);
    });
    settle(cx, &entity, |a| last_text(a).contains("Once upon a time"));
    let pid: i32 = std::fs::read_to_string(dir.path().join("pid"))
        .expect("the agent ran")
        .trim()
        .parse()
        .unwrap();
    let alive = |pid: i32| {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success())
    };
    entity.update(cx, |app, cx| {
        assert!(app.work.running, "the reply is still being written");
        assert!(alive(pid), "the agent is still writing");
        app.cancel_work_run(cx);
    });
    settle(cx, &entity, |a| !a.work.running);
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) {
        assert!(Instant::now() < deadline, "Stop ends the agent process");
        std::thread::sleep(Duration::from_millis(20));
    }
    entity.read_with(cx, |app, _| {
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
        let text = last_text(app);
        assert!(
            text.starts_with("Once upon a time")
                && !text.contains("no text returned")
                && !text.contains("before any text"),
            "the streamed text stays in the stopped reply: {text:?}"
        );
    });
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn streaming_codex(dir: &std::path::Path) -> std::path::PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// RC2-CXS: a scripted `codex app-server` that answers the handshake,
/// writes "Once upon" and " a time" as two pieces of one message, then
/// keeps writing (never completes).
fn streaming_codex(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("codex-streaming");
    let say = |line: &str| format!("printf '%s\\n' '{line}'\n");
    let delta = |text: &str| {
        say(&format!(
            r#"{{"method":"item/agentMessage/delta","params":{{"threadId":"thr-1","turnId":"turn-1","itemId":"msg-1","delta":"{text}"}}}}"#
        ))
    };
    let script = [
        format!("#!/bin/sh\necho $$ > {}\n", dir.join("pid").display()),
        "read -r l\n".to_string(),
        say(r#"{"id":1,"result":{"userAgent":"fake"}}"#),
        "read -r l\nread -r l\n".to_string(),
        say(r#"{"id":2,"result":{"thread":{"id":"thr-1"}}}"#),
        "read -r l\n".to_string(),
        say(r#"{"id":3,"result":{"turn":{"id":"turn-1","status":"inProgress"}}}"#),
        delta("Once upon"),
        delta(" a time"),
        "exec sleep 30\n".to_string(),
    ]
    .concat();
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// RC2-CXS (R3-20 Codex part): a Codex Work reply shows its text while it
/// is being written (over Codex's app server), and Stop in the middle of it
/// ends the run at once — the server is gone, the text already shown stays
/// in the reply, and the run reads Stopped.
#[gpui::test]
fn rc2_cxs_codex_work_reply_streams_and_stops_mid_stream(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::s5_int1_tests::rc2_cxs_codex_work_reply_streams_and_stops_mid_stream")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (mem, _count) = memory_home(&home, accepted_body());
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.work.agents.codex_bin = Some(streaming_codex(dir.path()).display().to_string());
        pick(
            app,
            WorkExecutor::Agent {
                engine: CodeEngine::Codex,
                model: None,
            },
            cx,
        );
        app.submit_work_run("tell me a long story".into(), cx);
    });
    settle(cx, &entity, |a| last_text(a).contains("Once upon a time"));
    let pid: i32 = std::fs::read_to_string(dir.path().join("pid"))
        .expect("the server ran")
        .trim()
        .parse()
        .unwrap();
    let alive = |pid: i32| {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    entity.update(cx, |app, cx| {
        assert!(app.work.running, "the reply is still being written");
        assert!(alive(pid), "Codex is still writing");
        app.cancel_work_run(cx);
    });
    settle(cx, &entity, |a| !a.work.running);
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) {
        assert!(Instant::now() < deadline, "Stop ends the Codex server");
        std::thread::sleep(Duration::from_millis(20));
    }
    entity.read_with(cx, |app, _| {
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
        let text = last_text(app);
        assert!(
            text.starts_with("Once upon a time")
                && !text.contains("no text returned")
                && !text.contains("before any text"),
            "the streamed text stays in the stopped reply: {text:?}"
        );
    });
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn cut_short_codex(dir: &std::path::Path) -> std::path::PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// RC2-CXSF: a scripted `codex app-server` that answers the handshake,
/// writes "Once upon", then closes its output and exits 0 — before Codex
/// says the turn is over.
fn cut_short_codex(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("codex-cut-short");
    let say = |line: &str| format!("printf '%s\\n' '{line}'\n");
    let script = [
        "#!/bin/sh\nread -r l\n".to_string(),
        say(r#"{"id":1,"result":{"userAgent":"fake"}}"#),
        "read -r l\nread -r l\n".to_string(),
        say(r#"{"id":2,"result":{"thread":{"id":"thr-1"}}}"#),
        "read -r l\n".to_string(),
        say(r#"{"id":3,"result":{"turn":{"id":"turn-1","status":"inProgress"}}}"#),
        say(
            r#"{"method":"item/agentMessage/delta","params":{"threadId":"thr-1","turnId":"turn-1","itemId":"msg-1","delta":"Once upon"}}"#,
        ),
        "exit 0\n".to_string(),
    ]
    .concat();
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// RC2-CXSF (RC2-CXS review P1): a Codex Work reply whose server left
/// before the turn was over is not a finished reply — the run reads Failed
/// with the plain-words reason, and the text already shown stays.
#[gpui::test]
fn rc2cxsf_codex_work_reply_cut_short_is_not_complete(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::s5_int1_tests::rc2cxsf_codex_work_reply_cut_short_is_not_complete")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (mem, _count) = memory_home(&home, accepted_body());
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.work.agents.codex_bin = Some(cut_short_codex(dir.path()).display().to_string());
        pick(
            app,
            WorkExecutor::Agent {
                engine: CodeEngine::Codex,
                model: None,
            },
            cx,
        );
        app.submit_work_run("tell me a story".into(), cx);
    });
    settle(cx, &entity, |a| {
        !a.work.running && a.work.panel.status.is_some()
    });
    entity.read_with(cx, |app, _| {
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        assert_eq!(
            app.work.panel.error.as_deref(),
            Some(crate::codex_app_server::ENDED_EARLY)
        );
        let text = last_text(app);
        assert!(
            text.starts_with("Once upon"),
            "the text already shown stays: {text:?}"
        );
    });
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn no_app_server_codex(dir: &std::path::Path) -> std::path::PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// RC2-CXSF: a Codex with no app server (it exits at once on the
/// subcommand), whose `codex exec --json` turn writes one whole message.
fn no_app_server_codex(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("codex-old");
    let script = "#!/bin/sh\n\
         if [ \"$1\" = app-server ]; then echo \"error: unrecognized subcommand 'app-server'\" >&2; exit 2; fi\n\
         printf '%s\\n' '{\"type\":\"item.completed\",\"item\":{\"id\":\"a\",\"type\":\"agent_message\",\"text\":\"from exec\"}}'\n\
         printf '%s\\n' '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":4}}'\n";
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// RC2-CXSF (RC2-CXS review P3): how a run's silence reads follows that
/// run's own Codex. A run that had to fall back to `codex exec --json`
/// shows whole messages; a later run whose Codex streams over its app
/// server reads as streaming — the earlier fallback does not carry over.
#[gpui::test]
fn rc2cxsf_codex_fallback_run_then_streaming_run_reads_each_its_own_way(
    cx: &mut gpui::TestAppContext,
) {
    use crate::executor_status::Quiet;
    let Some(home) = isolated(
        "work::s5_int1_tests::rc2cxsf_codex_fallback_run_then_streaming_run_reads_each_its_own_way",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let old = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (mem, _count) = memory_home(&home, accepted_body());
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.work.agents.codex_bin = Some(no_app_server_codex(old.path()).display().to_string());
        pick(
            app,
            WorkExecutor::Agent {
                engine: CodeEngine::Codex,
                model: None,
            },
            cx,
        );
        app.submit_work_run("first".into(), cx);
    });
    settle(cx, &entity, |a| {
        !a.work.running && a.work.panel.status.is_some()
    });
    entity.update(cx, |app, cx| {
        assert_eq!(app.work.panel.status, Some(RunStatus::Completed));
        assert!(last_text(app).contains("from exec"), "{:?}", last_text(app));
        assert_eq!(
            app.work_run_quiet(),
            Quiet::WholeMessages(CodeEngine::Codex.label()),
            "the fallback run shows whole messages"
        );
        app.work.agents.codex_bin = Some(streaming_codex(dir.path()).display().to_string());
        app.submit_work_run("second".into(), cx);
    });
    settle(cx, &entity, |a| last_text(a).contains("Once upon a time"));
    entity.update(cx, |app, cx| {
        assert!(app.work.running, "the second reply is still being written");
        assert_eq!(
            app.work_run_quiet(),
            Quiet::Streams,
            "the streaming run does not take the earlier run's wording"
        );
        app.cancel_work_run(cx);
    });
    settle(cx, &entity, |a| !a.work.running);
}

// ---- RC2-FIXC1: a memory-off Work send leaves no older counts ----

/// RC2-FIXC1: the counts an earlier send saved for `CONV` (19 standing
/// rules, 5 notes recalled), written the way the app keeps them, before the
/// app starts.
fn rc2fixc_counts_saved_before_launch() {
    let store = awili_persist::Store::open_default().unwrap();
    store
        .create_conversation_with_kind(CONV, "counts", 1, "work")
        .unwrap();
    store
        .set_app_state(
            &crate::recall::memory_chip_key(CONV),
            r#"{"rules":19,"recall":5}"#,
        )
        .unwrap();
}

/// RC2-FIXC1: the memory counts the status row shows for the conversation
/// on screen.
fn rc2fixc_counts_shown(app: &AwiliApp) -> Vec<String> {
    app.collect_status_parts()
        .into_iter()
        .map(|(t, _)| t)
        .filter(|t| t.contains("standing rule") || t.contains("recalled"))
        .collect()
}

/// RC2-FIXC1: memory switched off (no gate runs).
fn rc2fixc_memory_off() -> MemoryCfg {
    MemoryCfg {
        base_url: "http://127.0.0.1:9".into(),
        token: String::new(),
        min_priority: 90,
        enabled: false,
        recall: true,
        write: false,
    }
}

/// RC2-FIXC1: wait until the app's store has written everything queued so
/// far (its writer keeps order), then start the app again on `CONV` and
/// return the memory counts it shows.
fn rc2fixc_counts_after_relaunch(
    cx: &mut gpui::TestAppContext,
    app: &gpui::Entity<AwiliApp>,
) -> Vec<String> {
    app.read_with(cx, |a, _| {
        a.persist
            .as_ref()
            .expect("the app keeps a store")
            .set_state("rc2fixc-written", "1".into());
    });
    let store = awili_persist::Store::open_default().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while store.get_app_state("rc2fixc-written").unwrap().is_none() {
        assert!(Instant::now() < deadline, "the store never caught up");
        std::thread::sleep(Duration::from_millis(10));
    }
    let relaunched = cx.new(AwiliApp::new);
    relaunched.update(cx, |app, _| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Work;
        assert_eq!(
            app.conversations.get(CONV).unwrap().kind,
            awili_core::ConvKind::Work
        );
        app.set_active_conversation(CONV);
        rc2fixc_counts_shown(app)
    })
}

/// RC2-FIXC1 (RC2-FIXB review P2: a memory-off Work send skipped the gate
/// and left the older counts saved): a real Work send on an agent with
/// memory off clears this conversation's counts — none on screen, none
/// after a relaunch.
#[gpui::test]
fn rc2fixc_a_memory_off_agent_send_leaves_no_older_counts_after_a_relaunch(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::s5_int1_tests::rc2fixc_a_memory_off_agent_send_leaves_no_older_counts_after_a_relaunch",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    rc2fixc_counts_saved_before_launch();
    let dir = tempfile::tempdir().unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(
            app,
            &home,
            dir.path(),
            rc2fixc_memory_off(),
            awili_core::ConvKind::Work,
            cx,
        );
        app.mode = Mode::Work;
        assert_eq!(
            rc2fixc_counts_shown(app),
            vec![
                "19 standing rules".to_string(),
                "5 notes recalled".to_string()
            ],
            "the earlier send's counts show"
        );
        app.submit_work_run("memory off task".into(), cx);
    });
    settle(cx, &entity, |a| {
        !a.work.running && a.work.panel.status.is_some()
    });
    assert!(
        dir.path().join("ran").exists(),
        "the agent ran with memory off"
    );
    entity.read_with(cx, |app, _| {
        let shown = rc2fixc_counts_shown(app);
        assert!(shown.is_empty(), "the memory-off send shows: {shown:?}");
    });
    let shown = rc2fixc_counts_after_relaunch(cx, &entity);
    assert!(shown.is_empty(), "older counts came back: {shown:?}");
}

/// RC2-FIXC1 (RC2-FIXB review P2): the same for a real Work send on the
/// in-app engine with memory off — the engine gets the prompt with no
/// gate, and this conversation's older counts are gone, also after a
/// relaunch.
#[gpui::test]
fn rc2fixc_a_memory_off_in_app_send_leaves_no_older_counts_after_a_relaunch(
    cx: &mut gpui::TestAppContext,
) {
    use std::cell::RefCell;
    use std::rc::Rc;
    let Some(home) = isolated(
        "work::s5_int1_tests::rc2fixc_a_memory_off_in_app_send_leaves_no_older_counts_after_a_relaunch",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    rc2fixc_counts_saved_before_launch();
    let dir = tempfile::tempdir().unwrap();
    let entity = cx.new(AwiliApp::new);
    let worker: Rc<RefCell<Option<WorkerContext>>> = Rc::new(RefCell::new(None));
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory = rc2fixc_memory_off();
        app.mode = Mode::Work;
        app.work.folder = Some(dir.path().to_path_buf());
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "/models/gemma-4-26b".into();
        app.work.engine.label = "Gemma 4 26B (Studio)".into();
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        assert_eq!(
            app.conversations.get(CONV).unwrap().kind,
            awili_core::ConvKind::Work
        );
        app.set_active_conversation(CONV);
        pick(app, WorkExecutor::InApp, cx);
        assert_eq!(
            rc2fixc_counts_shown(app),
            vec![
                "19 standing rules".to_string(),
                "5 notes recalled".to_string()
            ],
            "the earlier send's counts show"
        );
        let keep = worker.clone();
        TEST_WORKER_SPAWN.with(|s| {
            *s.borrow_mut() = Some(Box::new(move |ctx| {
                *keep.borrow_mut() = Some(ctx);
            }));
        });
        app.submit_work_run("memory off task".into(), cx);
    });
    cx.run_until_parked();
    let ctx = worker
        .borrow_mut()
        .take()
        .expect("the in-app engine started");
    assert!(
        matches!(
            ctx.cmd_rx.try_recv(),
            Ok(awili_work::WorkCmd::Prompt { .. })
        ),
        "the prompt went out with no memory gate"
    );
    entity.read_with(cx, |app, _| {
        let shown = rc2fixc_counts_shown(app);
        assert!(shown.is_empty(), "the memory-off send shows: {shown:?}");
    });
    let shown = rc2fixc_counts_after_relaunch(cx, &entity);
    assert!(shown.is_empty(), "older counts came back: {shown:?}");
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    drop(ctx);
}

/// RC2-R1F7 (usability RC2 round 1, N5: a warm Gemma turn took 8-9 s to its
/// first word, most of it reading the recalled notes). Through the real Work
/// lane and the real gate: the in-app engine's turn gets each recalled note
/// cut at 400 bytes (marked "[truncated]"), not 1,500; the directives are
/// whole.
#[gpui::test]
fn rc2r1f7_the_in_app_turn_gets_short_recalled_notes(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::s5_int1_tests::rc2r1f7_the_in_app_turn_gets_short_recalled_notes")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let note = "n".repeat(1_400);
    let body = format!(
        r#"{{"mandatory_policy": [{{"id": "d0", "subject": "t", "authority": "owner_instruction", "object": "d0"}}], "items": [{{"id": "m1", "subject": "s", "authority": "inference", "object": "{note}"}}]}}"#
    );
    let (mem, _count) = memory_home(&home, body);
    let (tx, rx) = std::sync::mpsc::channel::<crate::work::GatewayCmd>();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        app.memory.enabled = false;
        app.work.agents.memory = Some(awili_config::AgentMemoryConfig::default());
        pick(app, WorkExecutor::InApp, cx);
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "m".into();
        app.work.gateway_cmd_tx = Some(tx);
        app.work.worker_conv = Some(CONV.into());
        app.submit_work_run("what is in alpha.txt?".into(), cx);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let system = loop {
        cx.run_until_parked();
        if let Ok(crate::work::GatewayCmd::Prompt { system, .. }) = rx.try_recv() {
            break system.expect("a system prompt");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the live worker got no prompt"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(system.contains("1. d0"), "{system}");
    let line = system
        .lines()
        .find(|l| l.starts_with("- ") && l.contains("nnn"))
        .unwrap_or_else(|| panic!("the recalled note: {system}"));
    let ns = line.trim_end_matches(" [truncated]").matches('n').count();
    assert!(
        ns <= crate::recall::INAPP_RECALL_NOTE_BYTES && ns > 300,
        "the note is cut near 400 bytes, not 1,500: {} n's",
        ns
    );
    assert!(line.ends_with(" [truncated]"), "{line}");
}

/// Stop before a memory/resume check returns must close an in-app row;
/// an idle gateway channel cannot turn this into a wait for a nonexistent run.
#[gpui::test]
fn inapp_stop_during_resume_gate_closes_row_before_late_evidence(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::s5_int1_tests::inapp_stop_during_resume_gate_closes_row_before_late_evidence",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (mem, held) = held_memory_home(&home, accepted_body(), accepted_body(), 2);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, dir.path(), mem, awili_core::ConvKind::Work, cx);
        pick(app, WorkExecutor::InApp, cx);
        app.work.local.health.finish_probe(Ok(()), Instant::now());
        app.work.engine.base_url = "http://127.0.0.1:1".into();
        app.work.engine.model = "fixture".into();
        app.submit_work_run("please continue awili".into(), cx);
        assert!(app.work.resume_gate_pending);
        assert!(app.work.running);
        app.cancel_work_run(cx);
        assert!(!app.work.resume_gate_pending);
        assert!(!app.work.running);
        assert_eq!(app.work.stream_row_idx, None);
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
    });
    held.release();
    cx.run_until_parked();
    entity.read_with(cx, |app, _| {
        assert!(!app.work.running);
        assert!(app.work.agent_abort_tx.is_none());
        assert!(!dir.path().join("ran").exists());
    });
}

/// SAFETY (owner rule: Awili never makes a Lemonade host load a model): the
/// memory-gated follow-up goes straight to a LIVE worker, so it must pass the
/// same residency check as every other send. Turn one found the model
/// resident; it is unloaded before turn two. Turn two (memory on, gate
/// accepted) must send NOTHING to the live worker and no chat request.
#[gpui::test]
fn lemonade_model_unloaded_between_turns_gets_no_memory_gated_prompt(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::s5_int1_tests::lemonade_model_unloaded_between_turns_gets_no_memory_gated_prompt",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("proj");
    std::fs::create_dir_all(folder.join(".git")).unwrap();
    let (mem, _count) = memory_home(&home, accepted_body());
    // Only the voice model is resident: the Coder was unloaded.
    let (url, seen) = crate::strip::tests::serve(
        vec![
            (
                "/api/v1/health",
                200,
                crate::strip::tests::health_body(&["kokoro-v1"]),
            ),
            ("/v1/models", 200, r#"{"data":[]}"#.to_string()),
        ],
        false,
    );
    let (tx, rx) = std::sync::mpsc::channel::<crate::work::GatewayCmd>();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        wire_agent(app, &home, &folder, mem, awili_core::ConvKind::Work, cx);
        pick(app, WorkExecutor::InApp, cx);
        app.work.engine.base_url = url.clone();
        app.work.engine.model = "Qwen3-Coder-Next-GGUF".into();
        app.work.engine.label = "Qwen3 Coder Next (Hosta)".into();
        app.work.gateway_cmd_tx = Some(tx); // the live worker from turn one
        app.work.worker_conv = Some(CONV.into());
        app.submit_work_run("turn two".into(), cx);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if entity.read_with(cx, |app, _| app.work.panel.error.is_some()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the refusal never showed"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    cx.run_until_parked();
    assert!(
        rx.try_recv().is_err(),
        "nothing reached the live worker"
    );
    entity.read_with(cx, |app, _| {
        let card = app.work.panel.error.clone().unwrap();
        assert!(card.contains("isn\u{2019}t loaded on Hosta"), "{card}");
        assert!(!app.work.running);
    });
    for line in seen.lock().unwrap().iter() {
        assert_eq!(line, "GET /api/v1/health HTTP/1.1", "{line}");
    }
}
