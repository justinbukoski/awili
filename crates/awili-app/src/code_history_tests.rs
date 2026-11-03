//! U1c required proof (offline, temp dirs): the Code-tab history state, meta,
//! text shaping, command builders and the ledger line for one turn.

use std::path::Path;

use awili_code::CodeEngine;
use awili_core::{ConvKind, ConversationList, ConversationMeta};
use awili_providers::Usage;
use awili_selftest::graduation::{Ledger as GradLedger, Outcome};
use awili_work::run_events::RunStatus;

use super::*;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-29T20:00:00Z")
        .expect("test timestamp")
        .with_timezone(&Utc)
}

/// Seed a fresh Code conversation into the list (returns its id).
fn code_conv(list: &mut ConversationList) -> String {
    list.create_kind(1, ConvKind::Code).id
}

fn agent(engine: CodeEngine, model: Option<&str>) -> WorkRunIdentity {
    WorkRunIdentity::Agent {
        engine,
        model: model.map(str::to_string),
    }
}

/// U1c: Stop wins over an error; an error without a Stop is `Failed`; a clean
/// turn is `Completed`.
#[test]
fn u1c_turn_status_stop_wins() {
    assert_eq!(turn_status(true, Some("x")), RunStatus::Stopped);
    assert_eq!(turn_status(false, Some("x")), RunStatus::Failed);
    assert_eq!(turn_status(false, None), RunStatus::Completed);
}

/// U1c: usage frames sum field-wise and saturate at `u32::MAX`; `note_usage`
/// without a turn is a no-op.
#[test]
fn u1c_usage_frames_sum_and_saturate() {
    let mut h = CodeTabHistory::default();
    h.note_usage(Usage {
        cached_input_tokens: None,
        input_tokens: 7,
        output_tokens: 9,
    });
    assert!(h.turn.is_none());

    h.turn = Some(CodeTurn {
        aidx: 1,
        identity: agent(CodeEngine::Glm, None),
        usage: None,
        error: None,
        stop_requested: false,
        run_id: String::new(),
    });
    h.note_usage(Usage {
        cached_input_tokens: None,
        input_tokens: 10,
        output_tokens: 20,
    });
    h.note_usage(Usage {
        cached_input_tokens: None,
        input_tokens: 5,
        output_tokens: 3,
    });
    let u = h.turn.as_ref().unwrap().usage.expect("summed");
    assert_eq!(
        u,
        Usage {
            cached_input_tokens: None,
            input_tokens: 15,
            output_tokens: 23
        }
    );
    h.note_usage(Usage {
        cached_input_tokens: None,
        input_tokens: u32::MAX,
        output_tokens: u32::MAX,
    });
    let u = h.turn.as_ref().unwrap().usage.expect("saturated");
    assert_eq!(
        u,
        Usage {
            cached_input_tokens: None,
            input_tokens: u32::MAX,
            output_tokens: u32::MAX
        }
    );
}

/// U1c: the FIRST non-empty error is kept; blanks and later errors are dropped.
#[test]
fn u1c_first_error_kept() {
    let mut h = CodeTabHistory::default();
    h.note_error("ignored without a turn");
    h.turn = Some(CodeTurn {
        aidx: 0,
        identity: agent(CodeEngine::Codex, None),
        usage: None,
        error: None,
        stop_requested: false,
        run_id: String::new(),
    });
    h.note_error("");
    h.note_error("a");
    h.note_error("b");
    assert_eq!(h.turn.as_ref().unwrap().error.as_deref(), Some("a"));
}

/// U1c: meta round-trips through JSON and `engine_model` parses only `agent:`
/// executors; garbage JSON and non-agent executors give `None`.
#[test]
fn u1c_meta_round_trips_and_rejects_garbage() {
    let meta = CodeConvMeta::for_turn(CodeEngine::Glm, None, Some("/p".into()), Some("s1".into()));
    let back = CodeConvMeta::from_json(&meta.to_json()).expect("round trip");
    assert_eq!(back, meta);
    assert_eq!(back.engine_model(), Some((CodeEngine::Glm, None)));

    let pinned = CodeConvMeta::for_turn(CodeEngine::Claude, Some("sonnet".into()), None, None);
    assert_eq!(
        pinned.engine_model(),
        Some((CodeEngine::Claude, Some("sonnet".into())))
    );

    assert_eq!(CodeConvMeta::from_json("{"), None);

    let gateway = CodeConvMeta {
        executor: "gateway".into(),
        folder: None,
        session: None,
        route: None,
    };
    assert_eq!(gateway.engine_model(), None);
}

/// U1c: a stopped, partial reply gets exactly one stop marker; an already
/// marked one is unchanged; a blank stopped reply persists nothing; a clean
/// reply is untouched.
#[test]
fn u1c_stopped_text_gets_suffix_once() {
    assert_eq!(
        persisted_assistant_text("partial", RunStatus::Stopped).as_deref(),
        Some(format!("partial{STOPPED_SUFFIX}").as_str())
    );
    let pre_marked = format!("partial{STOPPED_SUFFIX}");
    assert_eq!(
        persisted_assistant_text(&pre_marked, RunStatus::Stopped).as_deref(),
        Some(pre_marked.as_str())
    );
    assert_eq!(persisted_assistant_text("  ", RunStatus::Stopped), None);
    assert_eq!(
        persisted_assistant_text("done", RunStatus::Completed).as_deref(),
        Some("done")
    );
}

/// U1c: only a live Code conversation is reusable — `None`, an unknown id,
/// an archived Code conversation and a Chat conversation all give `None`.
#[test]
fn u1c_reusable_conv_rules() {
    let list = ConversationList::default();
    assert_eq!(reusable_conv(None, &list), None);
    assert_eq!(reusable_conv(Some("ghost"), &list), None);

    let mut list = ConversationList::default();
    let archived_id = code_conv(&mut list);
    list.archive(&archived_id);
    assert_eq!(reusable_conv(Some(&archived_id), &list), None);

    let mut list = ConversationList::default();
    let chat_id = list.create_kind(1, ConvKind::Chat).id;
    assert_eq!(reusable_conv(Some(&chat_id), &list), None);

    let mut list = ConversationList::default();
    let live_id = code_conv(&mut list);
    assert_eq!(reusable_conv(Some(&live_id), &list), Some(live_id));
}

/// U1c: a begin/end pair lands in the store — a `code` conversation with the
/// auto-title, the user and assistant rows, and the `code_conv:<id>` meta.
#[test]
fn u1c_turn_is_listed_and_restorable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("u1c.db");
    {
        let store = awili_persist::Store::open(&path).unwrap();
        let (p, worker) = crate::persistence::Persistence::spawn_worker(store);
        let id = "11111111-2222-4333-8444-555555555555";
        let meta = ConversationMeta {
            id: id.into(),
            title: "New conversation".into(),
            created_ts: 1,
            updated_ts: 1,
            archived: false,
            title_is_custom: false,
            cwd: None,
            profile: None,
            kind: ConvKind::Code,
        };
        let cmds = begin_cmds(
            id,
            Some(&meta),
            "why does median fail?",
            awili_core::MessageKind::Normal,
            1,
            Some("why does median fail?"),
        );
        for c in cmds {
            p.send(c);
        }
        let meta_json =
            CodeConvMeta::for_turn(CodeEngine::Glm, None, Some("/p".into()), None).to_json();
        let cmds = end_cmds(id, Some("because …"), 2, &meta_json, 3);
        for c in cmds {
            p.send(c);
        }
        drop(p);
        worker.join().unwrap();
    }
    let store = awili_persist::Store::open(&path).unwrap();
    let id = "11111111-2222-4333-8444-555555555555";
    let conv = store
        .conversations()
        .unwrap()
        .into_iter()
        .find(|c| c.id == id)
        .expect("the code conversation is listed");
    assert_eq!(conv.kind, "code");
    assert_eq!(ConvKind::from_token(&conv.kind), ConvKind::Code);
    assert_eq!(conv.title, "why does median fail?");

    let rows = store.messages(id).unwrap();
    let roles: Vec<&str> = rows.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["user", "assistant"]);
    assert_eq!(rows[0].text, "why does median fail?");
    assert_eq!(rows[1].text, "because …");

    let raw = store
        .get_app_state(&code_conv_key(id))
        .unwrap()
        .expect("the code_conv meta is stored");
    let back = CodeConvMeta::from_json(&raw).expect("meta parses");
    assert_eq!(
        back,
        CodeConvMeta::for_turn(CodeEngine::Glm, None, Some("/p".into()), None)
    );
}

/// U1c: a Code turn's ledger line carries the executor, the summed frontier
/// tokens and the right outcome (Success; a Stop is `Outcome::Stopped`, as in Work).
#[test]
fn u1c_turn_records_ledger_line() {
    // RC2-APP (K-flake2): a fresh folder under this process's scratch root.
    let ledger = crate::test_tmp::fresh_dir("u1c-ledger");
    let path = ledger.join("ledger.jsonl").to_string_lossy().into_owned();
    let mut rec = Recorder::from_config(&path);

    let turn = CodeTurn {
        aidx: 1,
        identity: agent(CodeEngine::Glm, None),
        usage: Some(Usage {
            cached_input_tokens: None,
            input_tokens: 700,
            output_tokens: 300,
        }),
        error: None,
        stop_requested: false,
        run_id: String::new(),
    };
    let e = turn
        .record(&mut rec, "c9", now())
        .expect("enabled")
        .expect("ok");
    assert_eq!(e.executor.as_str(), "glm");
    assert_eq!(e.frontier_tokens, 1000);
    assert_eq!(e.outcome, Outcome::Success);

    let stopped = CodeTurn {
        usage: None,
        error: Some("killed".into()),
        stop_requested: true,
        run_id: String::new(),
        ..CodeTurn {
            aidx: 1,
            identity: agent(CodeEngine::Glm, None),
            usage: None,
            error: None,
            stop_requested: false,
            run_id: String::new(),
        }
    };
    let e = stopped
        .record(&mut rec, "c9", now())
        .expect("enabled")
        .expect("ok");
    assert_eq!(e.outcome, Outcome::Stopped);

    let lines = GradLedger::read(Path::new(&path)).expect("read");
    assert_eq!(lines.len(), 2);
}

/// U1c-FIX1 (Codex P2): the persisted reply is the bubble exactly as the user
/// saw it. Whitespace is only a blank-detector; it is never trimmed away.
#[test]
fn u1c_fix_persisted_text_is_the_bubble_untrimmed() {
    assert_eq!(
        persisted_assistant_text("  indented reply\n", RunStatus::Completed).as_deref(),
        Some("  indented reply\n")
    );
    assert_eq!(
        persisted_assistant_text("partial\n", RunStatus::Stopped).as_deref(),
        Some(format!("partial\n{STOPPED_SUFFIX}").as_str())
    );
    assert_eq!(
        persisted_assistant_text(" \n\t", RunStatus::Completed),
        None
    );
}

// ---------------------------------------------------------------------------
// U1c-FIX1 (Codex P1): lifecycle proof. Each test builds the real `AwiliApp`
// (`AwiliApp::new` in a subprocess with a private HOME, the
// `stream_turn_tests` isolation) and runs one whole Code turn through the
// production path: `submit_code_turn` spawns a fake `codex` (a shell script
// that prints codex `--json` events), the provider loop's Usage/Error arms
// and `finish_code_turn` run, and the test then reads the store and the
// ledger back from disk. `allow_parking` lets the provider's reader thread
// wake the gpui test scheduler.
// ---------------------------------------------------------------------------

use gpui::AppContext;

const FIX_CHILD: &str = "AWILI_U1C_FIX1_TEST_HOME";
const FIX_PROMPT: &str = "why does median fail?";

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

/// What one finished Code turn left behind.
struct FinishedTurn {
    conv_id: String,
    /// The assistant bubble's text as the tab shows it after the turn.
    bubble: String,
    /// The store, reopened from disk after the persistence worker drained.
    store: awili_persist::Store,
    /// Every graduation ledger line, read back from disk.
    ledger: Vec<LedgerEntry>,
}

/// How long a Code turn may take to end once Stop is pressed (a bound on
/// the Stop, unchanged; the wait for the answer before it is
/// [`crate::exec_fixture::ANSWER_DEADLINE`]).
const STOP_ENDS_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn run_code_turn(cx: &mut gpui::TestAppContext, body: &str, stop_on: Option<&str>) -> FinishedTurn {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// Run one Code turn on a fresh app whose `codex` is a shell script with
/// `body`. With `stop_on` set, the test presses Stop (`cancel_code_turn`)
/// once the bubble contains that text.
fn run_code_turn(cx: &mut gpui::TestAppContext, body: &str, stop_on: Option<&str>) -> FinishedTurn {
    use std::os::unix::fs::PermissionsExt;
    cx.executor().allow_parking();
    let home = std::path::PathBuf::from(std::env::var_os(FIX_CHILD).unwrap());
    // Written before the app exists, so no other thread can hold it open.
    let script = home.join("fake-codex");
    std::fs::write(&script, body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let db = home.join("u1c-fix1.db");
    let ledger = home.join("u1c-fix1-ledger.jsonl");
    let (persist, worker) =
        crate::persistence::Persistence::spawn_worker(awili_persist::Store::open(&db).unwrap());

    let entity = cx.new(crate::AwiliApp::new);
    let tab_id = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = Some(persist);
        // Production writes through one database: the first native input
        // creates the conversation row synchronously on the UI-side
        // connection (`search_store`, WAL) and the worker appends the rest.
        // Point both at the fixture's file, or the create lands in the
        // default store and the reopened one never lists the conversation.
        app.search_store = Some(awili_persist::Store::open(&db).unwrap());
        app.work.graduation = Recorder::from_config(&ledger.to_string_lossy());
        app.work.agents.codex_bin = Some(script.display().to_string());
        app.ensure_code_tab(cx);
        let active = app.work.code_active;
        app.work.code_tabs[active].engine = CodeEngine::Codex;
        app.test_choose_native_target(CodeEngine::Codex, None, cx);
        app.submit_code_turn(FIX_PROMPT.to_string(), cx);
        app.work.code_tabs[active].id
    });

    let mut stopped = false;
    let mut finished = false;
    // The answer (up to the Stop text, or the whole turn) gets the answer
    // budget; once Stop is pressed, the turn must end within
    // [`STOP_ENDS_WITHIN`], the bound it always had.
    let mut deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    let mut stop_bounded = false;
    while std::time::Instant::now() < deadline {
        cx.run_until_parked();
        finished = entity.update(cx, |app, cx| {
            let tab = app.work.code_tabs.iter().find(|s| s.id == tab_id).unwrap();
            if let Some(needle) = stop_on.filter(|_| !stopped) {
                if tab
                    .convo
                    .message(1)
                    .is_some_and(|m| m.text.contains(needle))
                {
                    stopped = true;
                    app.cancel_code_turn(cx);
                }
                return false;
            }
            !tab.streaming
        });
        if finished {
            break;
        }
        if stopped && !stop_bounded {
            stop_bounded = true;
            deadline = std::time::Instant::now() + STOP_ENDS_WITHIN;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(finished, "the Code turn never finished");

    entity.update(cx, |app, _cx| {
        let tab = app.work.code_tabs.iter().find(|s| s.id == tab_id).unwrap();
        assert!(tab.history.turn.is_none(), "the turn record was settled");
        let conv_id = tab
            .history
            .conv_id
            .clone()
            .expect("the tab has a conversation");
        let bubble = tab.convo.message(1).expect("assistant bubble").text.clone();
        // Dropping the only handle lets the worker drain and exit.
        drop(app.persist.take());
        worker.join().unwrap();
        FinishedTurn {
            conv_id,
            bubble,
            store: awili_persist::Store::open(&db).unwrap(),
            ledger: GradLedger::read(&ledger).unwrap_or_default(),
        }
    })
}

/// The persisted (role, text) rows of a conversation.
fn stored_rows(turn: &FinishedTurn) -> Vec<(String, String)> {
    turn.store
        .messages(&turn.conv_id)
        .unwrap()
        .into_iter()
        .map(|m| (m.role, m.text))
        .collect()
}

/// S1-F04 end to end: a real Code turn lands in `conversations` (kind
/// `code`, auto-titled), in `messages` (the user prompt and the reply exactly
/// as the bubble shows it), in `app_state` (`code_conv:<id>`) and in the
/// graduation ledger (codex, summed frontier tokens, Success).
#[gpui::test]
fn u1c_fix_code_turn_reaches_store_and_ledger(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "code_history::code_history_tests::u1c_fix_code_turn_reaches_store_and_ledger",
    ) {
        return;
    }
    let turn = run_code_turn(
        cx,
        r#"#!/bin/sh
cat <<'EVENTS'
{"type":"item.completed","item":{"id":"a","type":"agent_message","text":"because the list is empty\n"}}
{"type":"turn.completed","usage":{"input_tokens":400,"output_tokens":200}}
{"type":"turn.completed","usage":{"input_tokens":300,"output_tokens":100}}
EVENTS
"#,
        None,
    );
    assert!(
        turn.bubble.contains("because the list is empty"),
        "{:?}",
        turn.bubble
    );

    let conv = turn
        .store
        .conversations()
        .unwrap()
        .into_iter()
        .find(|c| c.id == turn.conv_id)
        .expect("the Code conversation is listed");
    assert_eq!(ConvKind::from_token(&conv.kind), ConvKind::Code);
    assert_eq!(conv.title, FIX_PROMPT);
    assert_eq!(
        stored_rows(&turn),
        [
            ("user".to_string(), FIX_PROMPT.to_string()),
            ("assistant".to_string(), turn.bubble.clone()),
        ],
        "the reopened transcript is what the user saw"
    );
    let raw = turn
        .store
        .get_app_state(&code_conv_key(&turn.conv_id))
        .unwrap()
        .expect("the code_conv meta is stored");
    let meta = CodeConvMeta::from_json(&raw).expect("meta parses");
    assert_eq!(meta.engine_model(), Some((CodeEngine::Codex, None)));

    assert_eq!(turn.ledger.len(), 1, "exactly one ledger line per turn");
    let line = &turn.ledger[0];
    assert_eq!(line.executor.as_str(), "codex");
    assert_eq!(line.frontier_tokens, 1000, "both usage frames are summed");
    assert_eq!(line.outcome, Outcome::Success);
}

/// A Stop mid-turn: the ledger line is `Outcome::Stopped` (as in Work), and the
/// persisted reply equals the bubble, which carries the stop marker once.
#[gpui::test]
fn u1c_fix_stopped_code_turn_records_the_stop(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "code_history::code_history_tests::u1c_fix_stopped_code_turn_records_the_stop",
    ) {
        return;
    }
    let turn = run_code_turn(
        cx,
        r#"#!/bin/sh
cat <<'EVENTS'
{"type":"item.completed","item":{"id":"a","type":"agent_message","text":"partial answer"}}
EVENTS
exec sleep 30
"#,
        Some("partial answer"),
    );
    assert!(turn.bubble.ends_with(STOPPED_SUFFIX), "{:?}", turn.bubble);
    assert_eq!(turn.bubble.matches(STOPPED_SUFFIX).count(), 1);
    assert_eq!(
        stored_rows(&turn),
        [
            ("user".to_string(), FIX_PROMPT.to_string()),
            ("assistant".to_string(), turn.bubble.clone()),
        ]
    );
    assert_eq!(turn.ledger.len(), 1);
    assert_eq!(turn.ledger[0].outcome, Outcome::Stopped);
}

/// An engine error: the ledger line is Failed with the engine's first error.
#[gpui::test]
fn u1c_fix_failed_code_turn_records_the_error(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "code_history::code_history_tests::u1c_fix_failed_code_turn_records_the_error",
    ) {
        return;
    }
    let turn = run_code_turn(
        cx,
        r#"#!/bin/sh
cat <<'EVENTS'
{"type":"turn.failed","error":{"message":"model quota exhausted"}}
EVENTS
exit 1
"#,
        None,
    );
    assert_eq!(turn.ledger.len(), 1);
    match &turn.ledger[0].outcome {
        Outcome::Failed { reason } => {
            assert!(reason.contains("model quota exhausted"), "{reason}")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(stored_rows(&turn)[1].1, turn.bubble);
}

/// Run one Code turn with `prompt` on the ACTIVE tab (the fake `codex` at
/// `script`) and wait until it has finished streaming.
fn run_turn_on_active_tab(
    cx: &mut gpui::TestAppContext,
    entity: &gpui::Entity<crate::AwiliApp>,
    prompt: &str,
) {
    let tab_id = entity.update(cx, |app, cx| {
        app.submit_code_turn(prompt.to_string(), cx);
        app.work.code_tabs[app.work.code_active].id
    });
    let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    while std::time::Instant::now() < deadline {
        cx.run_until_parked();
        let streaming = entity.update(cx, |app, _cx| {
            app.work
                .code_tabs
                .iter()
                .find(|s| s.id == tab_id)
                .is_some_and(|s| s.streaming)
        });
        if !streaming {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the Code turn never finished");
}

#[cfg(unix)]
/// U1-INT1 (slice review P2): a new Code turn selects the conversation it
/// creates, and switching Code tabs moves the sidebar selection with the
/// visible tab, so the tab and the sidebar never point at different
/// conversations.
#[gpui::test]
fn u1_int_code_selection_follows_new_turns_and_tab_switches(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "code_history::code_history_tests::u1_int_code_selection_follows_new_turns_and_tab_switches",
    ) {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    cx.executor().allow_parking();
    let home = std::path::PathBuf::from(std::env::var_os(FIX_CHILD).unwrap());
    let script = home.join("fake-codex");
    std::fs::write(
        &script,
        r#"#!/bin/sh
cat <<'EVENTS'
{"type":"item.completed","item":{"id":"a","type":"agent_message","text":"ok"}}
{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}
EVENTS
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

    let entity = cx.new(crate::AwiliApp::new);
    let before = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.work.agents.codex_bin = Some(script.display().to_string());
        app.set_mode(crate::work::Mode::Code, cx);
        let active = app.work.code_active;
        app.work.code_tabs[active].engine = CodeEngine::Codex;
        app.test_choose_native_target(CodeEngine::Codex, None, cx);
        app.sidebar.active_id().to_string()
    });

    // Tab 0's first turn creates a Code conversation and selects it.
    run_turn_on_active_tab(cx, &entity, "first question");
    let first = entity.update(cx, |app, _cx| {
        let conv = app.work.code_tabs[0]
            .history
            .conv_id
            .clone()
            .expect("tab 0 has a conversation");
        assert_ne!(conv, before);
        assert_eq!(
            app.sidebar.active_id(),
            conv,
            "the new turn's conversation is selected"
        );
        conv
    });

    // A second tab's first turn selects its own, different conversation.
    entity.update(cx, |app, _cx| app.new_code_tab(CodeEngine::Codex));
    run_turn_on_active_tab(cx, &entity, "second question");
    let second = entity.update(cx, |app, _cx| {
        assert_eq!(app.work.code_active, 1);
        let conv = app.work.code_tabs[1]
            .history
            .conv_id
            .clone()
            .expect("tab 1 has a conversation");
        assert_ne!(conv, first);
        assert_eq!(
            app.sidebar.active_id(),
            conv,
            "tab 1's turn selects its conversation"
        );
        conv
    });

    // Switching tabs moves the selection with the visible tab, both ways.
    entity.update(cx, |app, _cx| {
        app.select_code_tab(0);
        assert_eq!(app.work.code_active, 0);
        assert_eq!(
            app.sidebar.active_id(),
            first,
            "the selection follows tab 0"
        );
        app.select_code_tab(1);
        assert_eq!(app.work.code_active, 1);
        assert_eq!(
            app.sidebar.active_id(),
            second,
            "the selection follows tab 1"
        );
    });
}

/// RC2-RUNF (review P2): a Code turn started by a plan-card click is stored
/// with its mark and comes back marked after a relaunch; the same words
/// typed by the owner are stored and restored as an ordinary message.
#[test]
fn rc2runf_a_plan_click_keeps_its_mark_after_a_relaunch() {
    use awili_core::MessageKind;
    let path = crate::test_tmp::fresh_dir("runf-code").join("runf.db");
    let id = "11111111-2222-4333-8444-666666666666";
    {
        let store = awili_persist::Store::open(&path).unwrap();
        store.create_conversation(id, "c", 1).unwrap();
        let (p, worker) = crate::persistence::Persistence::spawn_worker(store);
        let words = awili_code::PLAN_APPROVED_TEXT;
        for (ts, kind) in [(1, MessageKind::PlanClick), (2, MessageKind::Normal)] {
            for c in begin_cmds(id, None, words, kind, ts, None) {
                p.send(c);
            }
        }
        drop(p);
        worker.join().unwrap();
    }
    let rows = awili_persist::Store::open(&path)
        .unwrap()
        .messages(id)
        .unwrap();
    let kinds: Vec<&str> = rows.iter().map(|m| m.kind.as_str()).collect();
    assert_eq!(kinds, ["plan_click", "normal"]);
    let back = crate::restore_conversation(&rows);
    let kinds: Vec<MessageKind> = back.messages().iter().map(|m| m.kind).collect();
    assert_eq!(kinds, [MessageKind::PlanClick, MessageKind::Normal]);
}

#[test]
fn native_code_cached_usage_reaches_the_saved_reply_footer() {
    let mut h = CodeTabHistory {
        turn: Some(CodeTurn {
            aidx: 1,
            identity: agent(CodeEngine::Codex, None),
            usage: None,
            error: None,
            stop_requested: false,
            run_id: String::new(),
        }),
        ..Default::default()
    };
    h.note_usage(Usage {
        input_tokens: 10,
        output_tokens: 2,
        cached_input_tokens: Some(8),
    });
    h.note_usage(Usage {
        input_tokens: 5,
        output_tokens: 1,
        cached_input_tokens: Some(3),
    });
    let u = h.turn.as_ref().unwrap().usage.unwrap();
    assert_eq!(u.cached_input_tokens, Some(11));
    let entry = awili_selftest::graduation::LedgerEntry {
        v: 1,
        recorded_at: String::new(),
        task_id: "cache-test".into(),
        executor: "codex".into(),
        model: "subscription".into(),
        outcome: Outcome::Success,
        frontier_tokens: 18,
        local_tokens: 0,
        notes: None,
    };
    let text = crate::run_footer::footer_text(&crate::run_footer::FooterFacts {
        entry: &entry,
        reported_model: None,
        usage: Some(u),
        elapsed: None,
    });
    assert!(text.contains("15 input + 3 output"));
    assert!(text.contains("11 cached (included in input)"));
    let saved = std::collections::BTreeMap::from([("1:2".to_string(), text.clone())]);
    let restored = crate::run_footer::parse_footers(&serde_json::to_string(&saved).unwrap());
    assert_eq!(restored.get("1:2"), Some(&text));
    h.note_usage(Usage {
        input_tokens: 1,
        output_tokens: 1,
        cached_input_tokens: None,
    });
    assert_eq!(
        h.turn.as_ref().unwrap().usage.unwrap().cached_input_tokens,
        None
    );
}
