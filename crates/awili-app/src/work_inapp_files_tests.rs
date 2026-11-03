//! RC1-H1 proofs (RC1 round 3, Major 1): the in-app engine gets a brief that
//! keeps the local model calling its tools, and every in-app reply's footer
//! says which files Awili saw change — from the engine's tool results, never
//! from the model's words.

use std::cell::RefCell;
use std::rc::Rc;

use super::gateway_queue_tests::{isolated, CONV};
use super::inapp_files::InAppFiles;
use super::*;
use crate::agent_brief::OWNED_FILES_RULE;
use awili_work::WorkCmd;

/// A configured in-app Work session in `CONV`, bound to `folder`, memory off
/// (the owned engine then dispatches at once), ledger in `home`.
fn in_app_session(app: &mut AwiliApp, home: &std::path::Path, folder: &std::path::Path) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.memory.enabled = false;
    app.mode = Mode::Work;
    app.shell.job_open = true;
    app.work.folder = Some(folder.to_path_buf());
    app.work.engine.base_url = "http://127.0.0.1:9".into();
    app.work.engine.model = "/models/gemma-4-26b".into();
    app.work.engine.label = "Gemma 4 26B (Studio)".into();
    app.work.executor = WorkExecutor::InApp;
    app.work
        .graduation
        .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
    app.views.insert(
        CONV.into(),
        crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
    );
    app.set_active_conversation(CONV);
}

/// Take over the next worker spawn: the test holds the worker's context and
/// plays the engine.
fn capture_worker() -> Rc<RefCell<Option<WorkerContext>>> {
    let slot: Rc<RefCell<Option<WorkerContext>>> = Rc::new(RefCell::new(None));
    let keep = slot.clone();
    TEST_WORKER_SPAWN.with(|s| {
        *s.borrow_mut() = Some(Box::new(move |ctx| {
            *keep.borrow_mut() = Some(ctx);
        }));
    });
    slot
}

/// The system preamble of the next prompt the worker receives.
fn next_system(ctx: &WorkerContext) -> String {
    match ctx.cmd_rx.try_recv().expect("a prompt reached the worker") {
        WorkCmd::Prompt { system, .. } => system.expect("the in-app prompt has a preamble"),
        _ => panic!("expected a prompt"),
    }
}

fn started(name: &str, path: &str) -> WorkEvent {
    WorkEvent::ToolStarted {
        name: Some(name.into()),
        detail: Some(format!("{{\"path\": \"{path}\"}}")),
        file_path: Some(path.into()),
    }
}

fn finished(name: &str, ok: bool) -> WorkEvent {
    WorkEvent::ToolFinished {
        name: Some(name.into()),
        detail: Some(if ok {
            "done".into()
        } else {
            "user denied the request".into()
        }),
        duration_s: None,
        ok: Some(ok),
    }
}

/// One whole in-app turn: the model, the given tool events, the reply text,
/// usage, then the turn boundary.
fn play_turn(ctx: &WorkerContext, tools: Vec<WorkEvent>, reply: &str) {
    let mut events = vec![WorkEvent::TurnStarted {
        model: Some("/models/gemma-4-26b".into()),
    }];
    events.extend(tools);
    events.push(WorkEvent::Token(reply.into()));
    events.push(WorkEvent::UsageReported {
        input_tokens: 700,
        output_tokens: 40,
    });
    events.push(WorkEvent::TurnCompleted {
        status: RunStatus::Completed,
        output: Some(reply.into()),
        cost_usd: None,
        context_percent: None,
    });
    events.push(WorkEvent::TurnComplete);
    for ev in events {
        ctx.events.send_blocking(ev).unwrap();
    }
}

fn last_footer(app: &AwiliApp) -> String {
    app.views[CONV]
        .rows
        .last()
        .and_then(|r| r.footer.as_deref().map(str::to_string))
        .expect("the reply has a footer")
}

/// The real Work submit path with the in-app engine: the prompt carries the
/// in-app brief, and each reply's footer names the files Awili saw change —
/// a real write, a write the model only claimed, and a denied write.
#[gpui::test]
fn rc1h1_in_app_footer_says_what_changed(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::inapp_files_tests::rc1h1_in_app_footer_says_what_changed")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let entity = cx.new(AwiliApp::new);
    let slot = entity.update(cx, |app, cx| {
        in_app_session(app, &home, dir.path());
        let slot = capture_worker();
        app.submit_work_run(
            "Read notes.txt, then write the secret word into answer.txt.".into(),
            cx,
        );
        slot
    });
    let ctx = slot
        .borrow_mut()
        .take()
        .expect("the in-app worker was spawned");

    // 1. The brief that keeps the local model on its tools.
    let system = next_system(&ctx);
    assert!(system.contains(OWNED_FILES_RULE), "{system}");
    assert!(!system.contains("Start with the answer"), "{system}");
    assert!(
        !system.contains("end with a short list of the files"),
        "{system}"
    );
    assert!(system.contains("How to answer in Awili"), "{system}");

    // 2. A real write: the footer names it, and the model by its label.
    play_turn(
        &ctx,
        vec![
            started("fs.read", "notes.txt"),
            finished("fs.read", true),
            started("fs.write", "answer.txt"),
            finished("fs.write", true),
        ],
        "The secret word is heliotrope.",
    );
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        let footer = last_footer(app);
        assert!(footer.ends_with(" \u{b7} changed answer.txt"), "{footer}");
        assert!(
            footer.starts_with("In-app \u{b7} Gemma 4 26B (Studio) \u{b7} "),
            "{footer}"
        );
        assert!(!footer.contains("/models"), "no model path: {footer}");
    });

    // 3. A write the model only claims: Awili says nothing changed.
    entity.update(cx, |app, cx| {
        app.submit_work_run("Create a file named utf8.txt containing hi.".into(), cx);
    });
    let _ = next_system(&ctx);
    play_turn(
        &ctx,
        vec![],
        "I have created the file utf8.txt.\n\nFiles changed:\nutf8.txt",
    );
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        let footer = last_footer(app);
        assert!(footer.ends_with(" \u{b7} no files changed"), "{footer}");
    });

    // 4. A denied write changes nothing.
    entity.update(cx, |app, cx| {
        app.submit_work_run("Create a file named second.txt containing ok.".into(), cx);
    });
    let _ = next_system(&ctx);
    play_turn(
        &ctx,
        vec![
            started("fs.write", "second.txt"),
            finished("fs.write", false),
        ],
        "I could not write second.txt.",
    );
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        let footer = last_footer(app);
        assert!(footer.ends_with(" \u{b7} no files changed"), "{footer}");
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
    drop(ctx);
}

/// The record pairs each file-changing call with its own result.
#[test]
fn rc1h1_record_counts_only_successful_changes() {
    let mut r = InAppFiles::default();
    for ev in [
        started("fs.write", "a.txt"),
        started("fs.edit", "b.txt"),
        started("fs.write", "c.txt"),
        finished("fs.write", true),
        finished("fs.edit", false),
        finished("fs.write", true),
        started("fs.read", "d.txt"),
        finished("fs.read", true),
        started("fs.write", "a.txt"),
        finished("fs.write", true),
    ] {
        r.observe(&ev);
    }
    assert_eq!(r.footer_part(), "changed a.txt, c.txt");
    assert_eq!(InAppFiles::default().footer_part(), "no files changed");
    let mut many = InAppFiles::default();
    for name in ["1", "2", "3", "4"] {
        many.observe(&started("fs.write", name));
        many.observe(&finished("fs.write", true));
    }
    assert_eq!(many.footer_part(), "changed 4 files");
}

/// One in-app session in `home`, submitted once; returns the captured worker
/// after its first prompt was taken.
fn started_session(
    cx: &mut gpui::TestAppContext,
    home: &std::path::Path,
    folder: &std::path::Path,
    setup: impl FnOnce(&mut AwiliApp),
) -> (gpui::Entity<AwiliApp>, WorkerContext) {
    let entity = cx.new(AwiliApp::new);
    let slot = entity.update(cx, |app, cx| {
        in_app_session(app, home, folder);
        setup(app);
        let slot = capture_worker();
        app.submit_work_run("Create a file named utf8.txt containing hi.".into(), cx);
        slot
    });
    let ctx = slot
        .borrow_mut()
        .take()
        .expect("the in-app worker was spawned");
    let _ = next_system(&ctx);
    (entity, ctx)
}

/// RC1-H1-FIX1 (Codex T2 P1 on 49856d6): the footer does not depend on the
/// graduation ledger. With recording off, a reply that only claims a write
/// still shows "no files changed"; when the ledger append fails, a real write
/// still shows "changed answer.txt".
#[gpui::test]
fn rc1h1_fix1_footer_does_not_need_the_ledger(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_files_tests::rc1h1_fix1_footer_does_not_need_the_ledger")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();

    // Recording off.
    let (entity, ctx) = started_session(cx, &home, dir.path(), |app| {
        app.work.graduation.set_ledger("");
    });
    play_turn(
        &ctx,
        vec![],
        "I have created the file utf8.txt.\n\nFiles changed:\nutf8.txt",
    );
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        let footer = last_footer(app);
        assert!(footer.ends_with(" \u{b7} no files changed"), "{footer}");
    });
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    drop(ctx);

    // The append fails: the ledger's folder is a regular file.
    let blocker = home.join("not-a-folder");
    std::fs::write(&blocker, "x").unwrap();
    let ledger = blocker.join("ledger.jsonl");
    let (entity, ctx) = started_session(cx, &home, dir.path(), |app| {
        app.work.graduation.set_ledger(ledger.to_str().unwrap());
    });
    play_turn(
        &ctx,
        vec![
            started("fs.write", "answer.txt"),
            finished("fs.write", true),
        ],
        "Done.",
    );
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(
            app.work.graduation.last_error.is_some(),
            "the append really failed"
        );
        let footer = last_footer(app);
        assert!(footer.ends_with(" \u{b7} changed answer.txt"), "{footer}");
        assert!(footer.starts_with("In-app \u{b7} "), "{footer}");
    });
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    drop(ctx);
}

/// RC1-H1-FIX1 (Codex T2 P2 on 49856d6): a blank `[engine].label` names the
/// model by the config's display label (its last path segment), never the
/// model server's path.
#[gpui::test]
fn rc1h1_fix1_blank_label_names_the_model_not_its_path(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_files_tests::rc1h1_fix1_blank_label_names_the_model_not_its_path")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (entity, ctx) = started_session(cx, &home, dir.path(), |app| {
        app.work.engine.label = "  ".into();
    });
    play_turn(&ctx, vec![], "Hi there.");
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        let footer = last_footer(app);
        assert!(
            footer.starts_with("In-app \u{b7} gemma-4-26b \u{b7} "),
            "{footer}"
        );
        assert!(!footer.contains("/models"), "no model path: {footer}");
        assert!(footer.ends_with(" \u{b7} no files changed"), "{footer}");
    });
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    drop(ctx);
}

/// RC1-O (round 5 K3): an owner MCP `write_file` changed a file outside
/// Awili's sight; the footer and the Deny line must not say the turn changed
/// nothing.
#[test]
fn rc1o_other_tools_that_may_change_files_are_named() {
    let may = |n: &str| WorkEvent::ToolMayHaveChangedFiles { name: n.into() };
    let mut r = InAppFiles::default();
    r.observe(&may("write_file"));
    assert!(r.changed_any());
    assert_eq!(
        r.footer_part(),
        "ran write_file (Awili can't tell whether it changed any files)"
    );
    r.observe(&may("write_file"));
    r.observe(&started("fs.write", "a.txt"));
    r.observe(&finished("fs.write", true));
    assert_eq!(
        r.footer_part(),
        "changed a.txt; ran write_file (Awili can't tell whether it changed any files)"
    );
    r.observe(&may("run_command"));
    assert_eq!(
        r.footer_part(),
        "changed a.txt; ran write_file, run_command (Awili can't tell whether they changed any files)"
    );
    let mut many = InAppFiles::default();
    for n in ["a", "b", "c", "d"] {
        many.observe(&may(n));
    }
    assert_eq!(
        many.footer_part(),
        "ran 4 other tools (Awili can't tell whether they changed any files)"
    );
    assert_eq!(
        crate::work::deny_stop_line(
            "Run \"write_file\" \u{2014} \"content\": \"hi\", \"path\": \"b.txt\"",
            &r.footer_part(),
            r.changed_any()
        ),
        "Stopped \u{2014} you declined \u{201C}Run \"write_file\" \u{2014} \"content\": \"hi\", \"path\": \"b.txt\"\u{201D}, \
         so it was not done. Before that, this turn changed a.txt; ran write_file, run_command \
         (Awili can't tell whether they changed any files)."
    );
}

/// RC1-O (round 5 Major 4): opening a Work conversation puts the chip on the
/// lane its last run used, so a follow-up never silently goes to another
/// model; a local Build (one-shot) is never restored.
#[gpui::test]
fn rc1o_the_chip_follows_the_conversation(cx: &mut gpui::TestAppContext) {
    use crate::work_last_run::{restore_into, LastRun};
    use crate::work_run_identity::WorkRunIdentity;
    let Some(_home) = isolated("work::inapp_files_tests::rc1o_the_chip_follows_the_conversation")
    else {
        return;
    };
    let last = |identity: WorkRunIdentity| {
        LastRun {
            identity,
            model: None,
            status: RunStatus::Completed,
            error: None,
            finished_at: "2026-10-06T10:00:00+00:00".into(),
        }
        .to_json()
    };
    let claude = WorkExecutor::Agent {
        engine: awili_code::CodeEngine::Claude,
        model: Some("sonnet".into()),
    };
    let mut map = std::collections::HashMap::new();
    map.insert(
        "gemma-conv".to_string(),
        last(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None }),
    );
    map.insert(
        "claude-conv".to_string(),
        last(WorkRunIdentity::Agent {
            engine: awili_code::CodeEngine::Claude,
            model: Some("sonnet".into()),
        }),
    );
    map.insert(
        "build-conv".to_string(),
        last(WorkRunIdentity::LocalBuild {
            model: "b".into(),
            host: "h".into(),
        }),
    );
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _| {
        app.work.default_helper = Some("in-app".into());
        app.work.executor = claude.clone();
        assert!(restore_into(&mut app.work, "gemma-conv", &map));
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        assert!(restore_into(&mut app.work, "claude-conv", &map));
        assert_eq!(app.work.executor, claude);
        // A one-shot Build is not brought back: the default helper instead
        // (pre-review 4), never the previous conversation's chip.
        assert!(restore_into(&mut app.work, "build-conv", &map));
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        app.work.executor = claude.clone();
        // No last run: nothing changes.
        assert!(!restore_into(&mut app.work, "new-conv", &map));
        assert_eq!(app.work.executor, claude);
        // A running panel is never touched.
        app.work.running = true;
        assert!(!restore_into(&mut app.work, "gemma-conv", &map));
        assert_eq!(app.work.executor, claude);
        app.work.running = false;
    });

    // Pre-review P1: a Gemma run in A while the owner opens Claude
    // conversation B — B gets its own lane when A's run ends, so B's next
    // send never goes to Gemma; an explicit pick in B wins.
    entity.update(cx, |app, cx| {
        let a = app.conversations.create_kind(1, ConvKind::Work).id;
        let b = app.conversations.create_kind(2, ConvKind::Work).id;
        app.work_last_run.insert(
            a.clone(),
            last(WorkRunIdentity::OwnedEngine { model: "m".into(), engine_id: None }),
        );
        app.work_last_run.insert(
            b.clone(),
            last(WorkRunIdentity::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: Some("sonnet".into()),
            }),
        );
        app.work.chip_picked.clear();
        app.set_active_conversation(&a);
        app.restore_work_last_run();
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        app.work.running = true; // A's Gemma run
        app.set_active_conversation(&b);
        app.restore_work_last_run();
        assert_eq!(
            app.work.executor,
            WorkExecutor::InApp,
            "untouched while A runs"
        );
        app.work.running = false;
        app.settle_work_chip();
        assert_eq!(app.work.executor, claude, "B's own lane once A ends");
        // The owner's own pick in B is kept over B's last lane, and B goes
        // back to it after a visit to A (pre-review 2 P1: B -> A -> B).
        let codex = WorkExecutor::Agent {
            engine: awili_code::CodeEngine::Codex,
            model: None,
        };
        app.pick_work_executor(codex.clone(), cx);
        assert_eq!(app.work.chip_picked.get(&b), Some(&codex));
        app.settle_work_chip();
        app.restore_work_last_run();
        assert_eq!(app.work.executor, codex);
        app.set_active_conversation(&a);
        app.restore_work_last_run();
        assert_eq!(app.work.executor, WorkExecutor::InApp, "not carried into A");
        app.set_active_conversation(&b);
        app.restore_work_last_run();
        assert_eq!(app.work.executor, codex, "B is on its pick again");
        // While a run is live nothing moves, picked or not.
        app.work.running = true;
        app.set_active_conversation(&a);
        app.restore_work_last_run();
        assert_eq!(app.work.executor, codex);
        app.work.running = false;
        app.settle_work_chip();
        assert_eq!(app.work.executor, WorkExecutor::InApp);

        // Pre-review 3 P1: a pick in a conversation with no last run is kept
        // across a visit elsewhere.
        let c = app.conversations.create_kind(3, ConvKind::Work).id;
        app.set_active_conversation(&c);
        app.restore_work_last_run();
        app.pick_work_executor(claude.clone(), cx);
        app.set_active_conversation(&a);
        app.restore_work_last_run();
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        app.set_active_conversation(&c);
        app.restore_work_last_run();
        assert_eq!(app.work.executor, claude, "C's pick, though C never ran");

        // Pre-review 3 P1: a one-shot local Build pick is never kept; once it
        // is consumed, B goes back to its last lane, not to Build.
        app.work.chip_picked.remove(&b);
        app.set_active_conversation(&b);
        app.restore_work_last_run();
        app.pick_work_executor(
            WorkExecutor::LocalBuild {
                target_id: "t".into(),
            },
            cx,
        );
        assert!(!app.work.chip_picked.contains_key(&b));
        app.work.executor = WorkExecutor::InApp; // the Build was consumed
        app.set_active_conversation(&a);
        app.restore_work_last_run();
        app.set_active_conversation(&b);
        app.restore_work_last_run();
        assert_eq!(
            app.work.executor, claude,
            "B's last lane, never Build again"
        );

        // Pre-review 3 P1: a run ending in the active conversation leaves its
        // chip alone (its last-run row may not be written yet).
        app.work.conv_id = Some(b.clone());
        app.work.executor = codex.clone();
        app.settle_work_chip();
        assert_eq!(
            app.work.executor, codex,
            "the run's own conversation is left as it ran"
        );

        // Pre-review 4: a conversation with no pick and no last run takes the
        // default helper, not the chip the previous conversation left.
        // RC2-R1F2 (round 1 N2): the helper picked last (Claude, in C) comes
        // before the default helper.
        let d = app.conversations.create_kind(4, ConvKind::Work).id;
        app.work.conv_id = None;
        app.work.executor = codex.clone();
        app.set_active_conversation(&d);
        app.restore_work_last_run();
        assert_eq!(
            app.work.executor, claude,
            "the last pick, not the chip left"
        );
        // Pre-review 4: "+ New conversation" during a live run leaves the
        // live run's chip; the new conversation gets the last pick once the
        // run ends.
        app.work.executor = codex.clone();
        app.work.conv_id = Some(b.clone());
        app.work.running = true;
        app.start_on_default_helper();
        assert_eq!(app.work.executor, codex, "the live run's chip stays");
        app.set_active_conversation(&d);
        app.restore_work_last_run();
        assert_eq!(app.work.executor, codex);
        app.work.running = false;
        app.settle_work_chip();
        assert_eq!(app.work.executor, claude);
    });
}

/// RC1-O-FIX3 (pre-review 6 P2): a name holding a direction control or an
/// invisible character never reaches the footer or the Deny line as such —
/// the footer escapes it, the Deny line turns it into a space.
#[test]
fn rc1o_fix3_hidden_characters_are_shown_or_dropped() {
    let mut r = InAppFiles::default();
    r.observe(&WorkEvent::ToolMayHaveChangedFiles {
        name: "write\u{202E}elif_\u{200B}x\\y\u{AD}".into(),
    });
    r.observe(&started("fs.write", "a\u{2066}b.txt"));
    r.observe(&finished("fs.write", true));
    let footer = r.footer_part();
    assert_eq!(
        footer,
        "changed a\\u{2066}b.txt; ran write\\u{202e}elif_\\u{200b}x\\\\y\\u{ad} \
         (Awili can't tell whether it changed any files)"
    );
    assert!(
        !footer.chars().any(super::inapp_files::is_hidden_char),
        "{footer}"
    );
    let line = crate::work::deny_stop_line(
        "Run \"t\u{202E}x\" \u{2014} \"a\": \"b\"",
        &footer,
        r.changed_any(),
    );
    assert!(
        !line.chars().any(super::inapp_files::is_hidden_char),
        "{line}"
    );
    assert!(line.contains("Run \"t x\""), "{line}");
}

/// RC1-O-FIX4 (slice review P1): a prompt parked behind a Stop goes out on
/// the helper it was typed under, not on the chip shown when the Stop is
/// acknowledged (the owner may have opened or picked something else since).
#[gpui::test]
fn rc1o_fix4_a_parked_prompt_replays_on_its_own_helper(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("work::inapp_files_tests::rc1o_fix4_a_parked_prompt_replays_on_its_own_helper")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        // RC2-APP (S-O1, deliberate change): setup is finished, so the replay
        // really sends (a submit during setup returns early — the test used
        // to pass without sending anything).
        app.onboarding = crate::onboarding::Onboarding::existing();
        let b = app.conversations.create_kind(1, ConvKind::Work).id;
        app.views.insert(
            b.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.work.queued_after_stop = Some(QueuedPrompt {
            text: "say ok".into(),
            conv_id: b.clone(),
            executor: WorkExecutor::Gateway,
        });
        // A live gateway worker is wired to a channel the test owns, so the
        // dispatch is observable. Without a worker the Gateway send is refused,
        // the replay is not active, and the ambient field goes back to the
        // user's own posture (InApp) by design — which proves nothing about
        // which helper the prompt reached.
        let (tx, rx) = std::sync::mpsc::channel();
        app.work.gateway.enabled = true;
        app.work.worker_identity = Some(crate::work_run_identity::WorkRunIdentity::HermesGateway);
        app.work.gateway_cmd_tx = Some(tx);
        // Since parking, the chip moved (another conversation, another pick).
        app.work.executor = WorkExecutor::InApp;
        app.replay_queued_after_stop(cx);
        assert!(
            app.work.queued_after_stop.is_none(),
            "the parked prompt went out"
        );
        assert!(
            matches!(
                rx.try_recv(),
                Ok(crate::work::GatewayCmd::Prompt { text, .. }) if text == "say ok"
            ),
            "the prompt reached the gateway worker it was parked under"
        );
        assert!(rx.try_recv().is_err(), "exactly one dispatch");
        assert!(app.work.running, "the replayed run is the active owner");
        assert_eq!(app.work.executor, WorkExecutor::Gateway);
        // It reached the helper it was parked under, in its conversation.
        assert_eq!(app.work.conv_id.as_deref(), Some(b.as_str()));
        let sent: Vec<&str> = app.views[&b]
            .convo
            .messages()
            .iter()
            .filter(|m| m.role == awili_core::Role::User)
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(sent, ["say ok"], "the prompt was sent");
        app.work.gateway_cmd_tx = None;
    });
}

/// RC1-O-FIX5 (whole-slice review P1): while a prompt is parked — behind a
/// Stop, or behind the local model's health probe — the chip does not move,
/// so the parked prompt goes out on the helper it was parked under.
#[gpui::test]
fn rc1o_fix5_the_chip_stays_while_a_prompt_is_parked(cx: &mut gpui::TestAppContext) {
    use crate::work_last_run::LastRun;
    use crate::work_run_identity::WorkRunIdentity;
    let Some(_home) =
        isolated("work::inapp_files_tests::rc1o_fix5_the_chip_stays_while_a_prompt_is_parked")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _| {
        let a = app.conversations.create_kind(1, ConvKind::Work).id;
        let b = app.conversations.create_kind(2, ConvKind::Work).id;
        app.work_last_run.insert(
            b.clone(),
            LastRun {
                identity: WorkRunIdentity::Agent {
                    engine: awili_code::CodeEngine::Claude,
                    model: None,
                },
                model: None,
                status: RunStatus::Completed,
                error: None,
                finished_at: "2026-10-06T10:00:00+00:00".into(),
            }
            .to_json(),
        );
        app.set_active_conversation(&b);
        app.work.conv_id = Some(a.clone());
        app.work.executor = WorkExecutor::InApp;
        // Parked behind a Stop: no settle.
        app.work.queued_after_stop = Some(QueuedPrompt {
            text: "go".into(),
            conv_id: a.clone(),
            executor: WorkExecutor::InApp,
        });
        app.settle_work_chip();
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        app.work.queued_after_stop = None;
        // Parked behind the local model's probe: no settle.
        app.work.local.health.pending_send = Some(crate::gateway_health::PendingSend {
            text: "go".into(),
            conv_id: a.clone(),
        });
        app.settle_work_chip();
        assert_eq!(app.work.executor, WorkExecutor::InApp);
        app.work.local.health.pending_send = None;
        // Nothing parked: B gets its own lane.
        app.settle_work_chip();
        assert_eq!(
            app.work.executor,
            WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None
            }
        );
    });
}

/// RC1-Q-FIX1 (slice review P2/P3): a Stop (`Terminal`) and a failure
/// (`Failed`) close every running row, through the app's own update path,
/// and the Progress card then reads as ended, never "No run yet".
#[gpui::test]
fn rc1q_fix1_stop_and_failure_close_rows_and_progress_says_ended(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated(
        "work::inapp_files_tests::rc1q_fix1_stop_and_failure_close_rows_and_progress_says_ended",
    ) else {
        return;
    };
    let start = |tool: &str| {
        RunEvent::ToolProgress(awili_work::run_events::ToolProgressEvent {
            tool: Some(tool.into()),
            status: Some("running".into()),
            message: Some("…".into()),
        })
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.work.panel = RunPanel::default();
        // RC2-RUN (S-Q1, deliberate change): the thinking row is typed.
        app.work.panel.apply_reasoning(false);
        app.work.panel.apply_event(&start("recall"));
        assert_eq!(app.work.panel.tool_calls.len(), 2);
        assert!(!app.work.panel.has_ended());
        app.work.running = true;
        app.apply_work_update(
            WorkUpdate::Terminal {
                status: RunStatus::Stopped,
                output: None,
                model: None,
            },
            cx,
        );
        // RC1-Q-FIX2: the reasoning row completes; the tool row that never
        // reported an outcome ends neutral, never ✓.
        let st: Vec<ToolCallStatus> = app.work.panel.tool_calls.iter().map(|t| t.status).collect();
        assert_eq!(st, [ToolCallStatus::Completed, ToolCallStatus::Ended]);
        assert!(app.work.panel.has_ended());
        assert_eq!(
            progress_empty_text(false, false, app.work.panel.has_ended()),
            "This run has ended. Its reply is in the chat."
        );

        app.work.panel = RunPanel::default();
        app.work.panel.apply_reasoning(false);
        app.work.panel.apply_event(&start("recall"));
        app.work.running = true;
        app.apply_work_update(WorkUpdate::Failed("the model went away".into()), cx);
        let st: Vec<ToolCallStatus> = app.work.panel.tool_calls.iter().map(|t| t.status).collect();
        assert_eq!(st, [ToolCallStatus::Failed, ToolCallStatus::Ended]);
        // A failure now records the turn as Failed (it used to leave the
        // status unset and rest on the error alone); both end the run.
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        assert!(app.work.panel.has_ended(), "a failure ends the run");
    });
}

/// RC1-Q-FIX2 (slice review P1/P2/P3): from the worker's own events to the
/// Tools card's row — a failed call whose result is not shown reads
/// "<name> failed" (never "done"), and a call the owner denied (no result,
/// the turn stopped) ends neutral, never with a success tick.
#[gpui::test]
fn rc1q_fix2_worker_events_reach_the_tools_card_truthfully(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated(
        "work::inapp_files_tests::rc1q_fix2_worker_events_reach_the_tools_card_truthfully",
    ) else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.work.panel = RunPanel::default();
        app.work.running = true;
        let feed = |app: &mut AwiliApp, ev: WorkEvent, cx: &mut gpui::Context<AwiliApp>| {
            for up in work_event_to_updates(ev) {
                app.apply_work_update(up, cx);
            }
        };
        feed(
            app,
            WorkEvent::ToolStarted {
                name: Some("recall".into()),
                detail: Some("Run \"recall\"".into()),
                file_path: None,
            },
            cx,
        );
        // The worker hides a JSON error result (`plain_result` gives None).
        feed(
            app,
            WorkEvent::ToolFinished {
                name: Some("recall".into()),
                detail: None,
                duration_s: None,
                ok: Some(false),
            },
            cx,
        );
        let row = &app.work.panel.tool_calls[0];
        assert_eq!(row.status, ToolCallStatus::Failed);
        assert_eq!(row.message.as_deref(), Some("recall failed"));

        // A write waiting at its card, then the owner's Deny stops the turn:
        // no ToolFinished ever comes for it.
        feed(
            app,
            WorkEvent::ToolStarted {
                name: Some("fs.write".into()),
                detail: Some("Write a.txt (2 characters)".into()),
                file_path: Some("a.txt".into()),
            },
            cx,
        );
        app.apply_work_update(
            WorkUpdate::Terminal {
                status: RunStatus::Stopped,
                output: None,
                model: None,
            },
            cx,
        );
        let write = app
            .work
            .panel
            .tool_calls
            .iter()
            .find(|t| t.name == "fs.write")
            .expect("the write's row");
        assert_eq!(write.status, ToolCallStatus::Ended, "never a success tick");
        assert_eq!(app.work.panel.tool_calls[0].status, ToolCallStatus::Failed);
    });
}

/// RC2-APP (S-Q2): the cards as DRAWN — from the worker's own events through
/// the app's update path to the rendered Tools rows (glyph, title, subtitle)
/// and the Progress text: a failed call reads ✗ "… failed", a denied write
/// that never finished ends neutral (–), and Progress says the run ended.
#[gpui::test]
fn rc2app_the_drawn_cards_show_what_the_worker_reported(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_files_tests::rc2app_the_drawn_cards_show_what_the_worker_reported")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    view.update(window, |app, cx| {
        in_app_session(app, &home, dir.path());
        app.work.conv_id = Some(CONV.into());
        app.work.panel = RunPanel::default();
        app.work.running = true;
        for ev in [
            WorkEvent::ToolStarted {
                name: Some("recall".into()),
                detail: Some("Run \"recall\"".into()),
                file_path: None,
            },
            WorkEvent::ToolFinished {
                name: Some("recall".into()),
                detail: None,
                duration_s: None,
                ok: Some(false),
            },
            WorkEvent::ToolStarted {
                name: Some("fs.write".into()),
                detail: Some("Write a.txt (2 characters)".into()),
                file_path: Some("a.txt".into()),
            },
        ] {
            for up in work_event_to_updates(ev) {
                app.apply_work_update(up, cx);
            }
        }
        app.apply_work_update(
            WorkUpdate::Terminal {
                status: RunStatus::Stopped,
                output: None,
                model: None,
            },
            cx,
        );
    });
    window.run_until_parked();
    let rows: Vec<String> = view.read_with(window, |app, _| {
        app.work
            .panel
            .tool_calls
            .iter()
            .map(|t| {
                let glyph = match t.status {
                    ToolCallStatus::Failed => "\u{2717}",
                    ToolCallStatus::Ended => "\u{2013}",
                    other => panic!("unexpected row status {other:?}"),
                };
                let sub = t.message.clone().unwrap_or_default();
                format!(
                    "{}{glyph} {} | {}",
                    AwiliApp::TOOL_ROW_SELECTOR,
                    t.title(),
                    crate::work::text_clip::clip(&sub, 60, 57)
                )
            })
            .collect()
    });
    assert_eq!(rows.len(), 2);
    assert!(rows[0].ends_with(" | recall failed"), "{}", rows[0]);
    for row in &rows {
        let key: &'static str = Box::leak(row.clone().into_boxed_str());
        assert!(window.debug_bounds(key).is_some(), "drawn: {row}");
    }
    // RC2-FIXA4 (RC2-RUNAPP review P3): each text of each row is itself
    // on screen, inside its row: the glyph, then the title, the subtitle
    // under the title.
    let texts: Vec<(String, [String; 3])> = view.read_with(window, |app, _| {
        app.work
            .panel
            .tool_calls
            .iter()
            .zip(&rows)
            .map(|(t, row)| {
                let glyph = if t.status == ToolCallStatus::Failed {
                    "\u{2717}"
                } else {
                    "\u{2013}"
                };
                let sub = t.message.clone().unwrap_or_default();
                (row.clone(), [glyph.to_string(), t.title().to_string(), sub])
            })
            .collect()
    });
    assert_eq!(texts[0].1[2], "recall failed");
    // RC2-FIXC4 (RC2-FIXA review P3, twice: the lookup key was a copy of
    // the text, so it was found with the text missing): each text is read
    // back from the element that drew it — the text it laid out and where
    // it was painted — on a fresh frame. A text element that was never
    // laid out drew nothing.
    crate::ui::text::DRAWN_TEXTS.with(|d| d.borrow_mut().clear());
    view.update(window, |_, cx| cx.notify());
    window.run_until_parked();
    let drawn: Vec<(String, gpui::Bounds<gpui::Pixels>)> = crate::ui::text::DRAWN_TEXTS.with(|d| {
        d.borrow()
            .iter()
            .filter_map(|layout| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (layout.text(), layout.bounds())
                }))
                .ok()
            })
            .collect()
    });
    for (row, [glyph, title, sub]) in &texts {
        let row_at = window
            .debug_bounds(Box::leak(row.clone().into_boxed_str()))
            .unwrap();
        let at = |text: &str| -> gpui::Bounds<gpui::Pixels> {
            drawn
                .iter()
                .find(|(t, b)| t == text && row_at.contains(&b.center()))
                .map(|(_, b)| *b)
                .unwrap_or_else(|| panic!("{text:?} is not drawn in its row ({row}): {drawn:?}"))
        };
        let (g, t) = (at(glyph), at(title));
        let s = (!sub.is_empty()).then(|| at(sub));
        assert!(
            g.right() <= t.left(),
            "the glyph is left of the title: {row}"
        );
        if let Some(s) = s {
            assert!(row_at.contains(&s.center()), "{row}");
            assert!(
                s.top() >= t.bottom(),
                "the subtitle is under the title: {row}"
            );
        }
    }
    let progress = format!(
        "{}This run has ended. Its reply is in the chat.",
        AwiliApp::PROGRESS_TEXT_SELECTOR
    );
    let key: &'static str = Box::leak(progress.clone().into_boxed_str());
    assert!(window.debug_bounds(key).is_some(), "drawn: {progress}");
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
}

/// RC2-APP (S-N1): the app's own restart path — an in-app worker that ends
/// with `Failed` (as a log storage error ends it, so the startup recovery
/// gate runs) is let go, and the next prompt starts a NEW worker that gets
/// the prompt.
#[gpui::test]
fn rc2app_a_failed_worker_is_replaced_on_the_next_prompt(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_files_tests::rc2app_a_failed_worker_is_replaced_on_the_next_prompt")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let entity = cx.new(AwiliApp::new);
    let first = entity.update(cx, |app, cx| {
        in_app_session(app, &home, dir.path());
        let slot = capture_worker();
        app.submit_work_run("Say hello.".into(), cx);
        slot
    });
    let ctx1 = first.borrow_mut().take().expect("the first worker");
    let _ = next_system(&ctx1);
    // A failed in-app turn checks the local model once: answer that check
    // here instead of a probe thread.
    crate::local_model_app::TEST_LOCAL_PROBE_RESULT.with(|slot| *slot.borrow_mut() = Some(Ok(())));
    ctx1.events
        .send_blocking(WorkEvent::Failed(
            "the engine log could not be written".into(),
        ))
        .unwrap();
    cx.run_until_parked();
    let second = entity.update(cx, |app, cx| {
        assert!(!app.work.running, "the failed turn ended");
        assert!(
            app.work.gateway_cmd_tx.is_none(),
            "the failed worker is let go"
        );
        let slot = capture_worker();
        app.submit_work_run("Say hello again.".into(), cx);
        slot
    });
    let ctx2 = second
        .borrow_mut()
        .take()
        .expect("the next prompt starts a new worker");
    let system = next_system(&ctx2);
    assert!(!system.is_empty(), "the new worker got the prompt");
    entity.update(cx, |app, _| app.work.gateway_cmd_tx = None);
    drop((ctx1, ctx2));
}

/// RC1-O-FIX6 (whole-slice review P1): a prompt parked behind the local
/// model's probe is handed back because the owner moved to B; B's chip then
/// follows B's own lane (it stayed put only while the prompt was parked).
#[gpui::test]
fn rc1o_fix6_the_chip_settles_after_a_parked_prompt_is_handed_back(cx: &mut gpui::TestAppContext) {
    use crate::work_last_run::LastRun;
    use crate::work_run_identity::WorkRunIdentity;
    let Some(_home) = isolated(
        "work::inapp_files_tests::rc1o_fix6_the_chip_settles_after_a_parked_prompt_is_handed_back",
    ) else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let a = app.conversations.create_kind(1, ConvKind::Work).id;
        let b = app.conversations.create_kind(2, ConvKind::Work).id;
        app.work_last_run.insert(
            b.clone(),
            LastRun {
                identity: WorkRunIdentity::Agent {
                    engine: awili_code::CodeEngine::Claude,
                    model: None,
                },
                model: None,
                status: RunStatus::Completed,
                error: None,
                finished_at: "2026-10-06T10:00:00+00:00".into(),
            }
            .to_json(),
        );
        app.mode = Mode::Work;
        app.set_active_conversation(&b);
        app.work.conv_id = Some(a.clone());
        app.work.executor = WorkExecutor::InApp;
        app.work.local.health.pending_send = Some(crate::gateway_health::PendingSend {
            text: "go".into(),
            conv_id: a.clone(),
        });
        app.settle_work_chip();
        assert_eq!(
            app.work.executor,
            WorkExecutor::InApp,
            "parked: the chip stays"
        );
        let gen = app.work.local.endpoint_gen;
        app.on_local_probe(gen, Ok(()), cx);
        assert!(app.work.local.health.pending_send.is_none(), "handed back");
        assert_eq!(
            app.work.executor,
            WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None
            },
            "B's own lane once the prompt was handed back"
        );
    });
}

/// RC2-APP (S-O2): the footer the owner SEES under a stopped in-app reply —
/// attached through the app's own Stop path from the worker's own events —
/// is the stopped wording, not the ordinary one.
#[gpui::test]
fn rc2app_the_visible_footer_of_a_stopped_turn_says_so(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_files_tests::rc2app_the_visible_footer_of_a_stopped_turn_says_so")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let entity = cx.new(AwiliApp::new);
    let slot = entity.update(cx, |app, cx| {
        in_app_session(app, &home, dir.path());
        let slot = capture_worker();
        app.submit_work_run("Write a.txt, then keep going.".into(), cx);
        slot
    });
    let ctx = slot
        .borrow_mut()
        .take()
        .expect("the in-app worker was spawned");
    let _ = next_system(&ctx);
    for ev in [
        WorkEvent::TurnStarted {
            model: Some("/models/gemma-4-26b".into()),
        },
        started("fs.write", "a.txt"),
        finished("fs.write", true),
        WorkEvent::Token("Wrote a.txt, now".into()),
    ] {
        ctx.events.send_blocking(ev).unwrap();
    }
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(app.work.running, "the turn is still going");
        app.cancel_work_run(cx);
        let footer = last_footer(app);
        assert!(
            footer.ends_with(" \u{b7} changed a.txt \u{2014} seen before the Stop"),
            "{footer}"
        );
        app.work.gateway_cmd_tx = None;
    });
    drop(ctx);
}

/// RC1-O-FIX8 (whole-slice reviews of FIX6/FIX7, P1): a stopped turn's
/// footer claims only what was seen before the Stop — never "no files
/// changed" (a call may still have been in flight, and the worker's last
/// events may not have arrived).
#[test]
fn rc1o_fix8_a_stopped_turn_claims_only_what_was_seen() {
    let mut r = InAppFiles::default();
    assert_eq!(
        r.stopped_footer_part(),
        "no file changes seen before the Stop"
    );
    r.observe(&WorkEvent::ToolStarted {
        name: Some("run_command".into()),
        detail: None,
        file_path: None,
    });
    assert_eq!(
        r.stopped_footer_part(),
        "no file changes seen before the Stop"
    );
    r.observe(&started("fs.write", "a.txt"));
    r.observe(&finished("fs.write", true));
    assert_eq!(
        r.stopped_footer_part(),
        "changed a.txt \u{2014} seen before the Stop"
    );
    r.observe(&WorkEvent::ToolMayHaveChangedFiles {
        name: "run_command".into(),
    });
    assert_eq!(
        r.stopped_footer_part(),
        "changed a.txt; ran run_command (Awili can't tell whether it changed any files) \u{2014} seen before the Stop"
    );
    // An ordinary (not stopped) footer is unchanged.
    assert_eq!(
        r.footer_part(),
        "changed a.txt; ran run_command (Awili can't tell whether it changed any files)"
    );
}

/// RC1-O-FIX7 (whole-slice review P1): Stopping a run while another Work
/// conversation is on screen gives that conversation its own lane.
#[gpui::test]
fn rc1o_fix7_stop_settles_the_chip(cx: &mut gpui::TestAppContext) {
    use crate::work_last_run::LastRun;
    use crate::work_run_identity::WorkRunIdentity;
    let Some(_home) = isolated("work::inapp_files_tests::rc1o_fix7_stop_settles_the_chip") else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let a = app.conversations.create_kind(1, ConvKind::Work).id;
        let b = app.conversations.create_kind(2, ConvKind::Work).id;
        app.work_last_run.insert(
            b.clone(),
            LastRun {
                identity: WorkRunIdentity::Agent {
                    engine: awili_code::CodeEngine::Claude,
                    model: None,
                },
                model: None,
                status: RunStatus::Completed,
                error: None,
                finished_at: "2026-10-06T10:00:00+00:00".into(),
            }
            .to_json(),
        );
        app.set_active_conversation(&b);
        app.work.conv_id = Some(a.clone());
        app.work.executor = WorkExecutor::InApp;
        app.work.running = true;
        app.cancel_work_run(cx);
        assert!(!app.work.running);
        // RC1-O-FIX8: the owner's Stop marks the run, so its footer claims
        // only what was seen before the Stop.
        assert!(app.work.owner_stopped);
        assert_eq!(
            app.work.executor,
            WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None
            }
        );
    });
}

/// RC1-S (round 6 Major 1): opening a Work conversation puts the Work folder
/// on the conversation's own (the owner's pick there, else its folder); never
/// while a run is live; the run's end catches up; a folder that is gone is
/// left alone.
#[gpui::test]
fn rc1s_the_folder_follows_the_conversation(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated("work::inapp_files_tests::rc1s_the_folder_follows_the_conversation")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (fa, fb, fc) = (
        dir.path().join("a"),
        dir.path().join("b"),
        dir.path().join("c"),
    );
    for f in [&fa, &fb, &fc] {
        std::fs::create_dir_all(f).unwrap();
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let a = app.conversations.create_kind(1, ConvKind::Work).id;
        let b = app.conversations.create_kind(2, ConvKind::Work).id;
        let gone = app.conversations.create_kind(3, ConvKind::Work).id;
        app.conversations.get_mut(&a).unwrap().cwd = Some(fa.display().to_string());
        app.conversations.get_mut(&b).unwrap().cwd = Some(fb.display().to_string());
        app.conversations.get_mut(&gone).unwrap().cwd =
            Some(dir.path().join("deleted").display().to_string());

        app.set_active_conversation(&a);
        assert!(app.restore_work_folder(cx));
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));
        app.set_active_conversation(&b);
        app.restore_work_folder(cx);
        assert_eq!(app.work.folder.as_deref(), Some(fb.as_path()));

        // The owner's pick in B is kept over B's own folder.
        app.choose_work_folder(fc.clone(), cx);
        app.set_active_conversation(&a);
        app.restore_work_folder(cx);
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));
        app.set_active_conversation(&b);
        app.restore_work_folder(cx);
        assert_eq!(app.work.folder.as_deref(), Some(fc.as_path()));

        // A live run in B: opening A changes nothing (RC1-S-FIX2: no catch-up
        // when the run ends — the next opening of A applies its folder).
        app.work.running = true;
        app.work.conv_id = Some(b.clone());
        app.set_active_conversation(&a);
        assert!(!app.restore_work_folder(cx));
        assert_eq!(app.work.folder.as_deref(), Some(fc.as_path()));
        app.work.running = false;
        assert!(app.restore_work_folder(cx));
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));

        // A folder that no longer exists is left alone.
        app.set_active_conversation(&gone);
        assert!(!app.restore_work_folder(cx));
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));
    });
}

/// RC1-S-FIX2: through the app's own entry points — opening a conversation
/// from history moves the folder to its own (or the owner's pick there);
/// choosing a folder applies at once (as before RC1-S) and is remembered;
/// nothing moves while a run is live.
#[gpui::test]
fn rc1s_fix2_the_folder_follows_through_the_real_paths(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("work::inapp_files_tests::rc1s_fix2_the_folder_follows_through_the_real_paths")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (fa, fb, fc) = (
        dir.path().join("a"),
        dir.path().join("b"),
        dir.path().join("c"),
    );
    for f in [&fa, &fb, &fc] {
        std::fs::create_dir_all(f).unwrap();
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        let a = app.conversations.create_kind(1, ConvKind::Work).id;
        let b = app.conversations.create_kind(2, ConvKind::Work).id;
        for (id, f) in [(&a, &fa), (&b, &fb)] {
            app.conversations.get_mut(id).unwrap().cwd = Some(f.display().to_string());
            app.views.insert(
                id.clone(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
        }
        app.open_conversation_from_history(&a, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));
        app.open_conversation_from_history(&b, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fb.as_path()));
        app.choose_work_folder(fc.clone(), cx);
        assert_eq!(app.work.folder.as_deref(), Some(fc.as_path()));
        app.open_conversation_from_history(&a, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));
        app.open_conversation_from_history(&b, cx);
        assert_eq!(
            app.work.folder.as_deref(),
            Some(fc.as_path()),
            "B keeps the pick"
        );
        // Nothing moves under a live run.
        app.work.running = true;
        app.open_conversation_from_history(&a, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fc.as_path()));
        app.work.running = false;
    });
}

/// RC2-APP (S-S1): the folder follows the conversation through the two
/// entry points the RC1-S tests did not drive — a sidebar selection and a
/// switch into Work mode — with the owner's pick kept per conversation.
#[gpui::test]
fn rc2app_the_folder_follows_the_sidebar_and_the_mode_switch(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated(
        "work::inapp_files_tests::rc2app_the_folder_follows_the_sidebar_and_the_mode_switch",
    ) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (fa, fb, fc) = (
        dir.path().join("a"),
        dir.path().join("b"),
        dir.path().join("c"),
    );
    for f in [&fa, &fb, &fc] {
        std::fs::create_dir_all(f).unwrap();
    }
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        let chat = app.conversations.create_kind(1, ConvKind::Chat).id;
        let a = app.conversations.create_kind(2, ConvKind::Work).id;
        let b = app.conversations.create_kind(3, ConvKind::Work).id;
        for id in [&chat, &a, &b] {
            app.views.insert(
                id.clone(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
        }
        for (id, f) in [(&a, &fa), (&b, &fb)] {
            app.conversations.get_mut(id).unwrap().cwd = Some(f.display().to_string());
        }
        // In Chat; Work last had A open.
        app.mode = Mode::Code;
        app.set_active_conversation(&chat);
        app.sidebar.last_work_id = Some(a.clone());
        app.work.folder = None;
        // The mode switch into Work opens A — and A's folder.
        app.set_mode(Mode::Work, cx);
        assert_eq!(app.sidebar.active_id(), a);
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));
        // A sidebar selection of B — B's folder.
        app.select_conversation(&b, window, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fb.as_path()));
        // The owner's pick in B is kept across a visit to A.
        app.choose_work_folder(fc.clone(), cx);
        app.select_conversation(&a, window, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));
        app.select_conversation(&b, window, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fc.as_path()));
    });
}

/// RC2-RUN (S-S2): the owner opens conversation A while a run goes on in B;
/// the run's real end (`TurnComplete`, and a Stop) puts the folder on A's at
/// once — no second opening needed. When the ended run's own conversation is
/// on screen, nothing moves.
#[gpui::test]
fn rc2_run_the_folder_catches_up_when_the_run_ends(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("work::inapp_files_tests::rc2_run_the_folder_catches_up_when_the_run_ends")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let (fa, fb) = (dir.path().join("a"), dir.path().join("b"));
    for f in [&fa, &fb] {
        std::fs::create_dir_all(f).unwrap();
    }
    let entity = cx.new(AwiliApp::new);
    let (a, b) = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        let a = app.conversations.create_kind(1, ConvKind::Work).id;
        let b = app.conversations.create_kind(2, ConvKind::Work).id;
        for (id, f) in [(&a, &fa), (&b, &fb)] {
            app.conversations.get_mut(id).unwrap().cwd = Some(f.display().to_string());
            app.views.insert(
                id.clone(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
        }
        app.open_conversation_from_history(&b, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fb.as_path()));
        // A run in B; the owner opens A meanwhile: nothing moves yet.
        app.work.running = true;
        app.work.conv_id = Some(b.clone());
        app.open_conversation_from_history(&a, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fb.as_path()));
        // The run's real end.
        app.apply_work_update(WorkUpdate::TurnComplete, cx);
        assert!(!app.work.running);
        (a, b)
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert_eq!(
            app.work.folder.as_deref(),
            Some(fa.as_path()),
            "A's folder applies when B's run ends"
        );
        // A Stop is a run's end too.
        app.work.running = true;
        app.work.conv_id = Some(a.clone());
        app.open_conversation_from_history(&b, cx);
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));
        app.cancel_work_run(cx);
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert_eq!(
            app.work.folder.as_deref(),
            Some(fb.as_path()),
            "B's folder applies when A's run is stopped"
        );
        // The ended run's own conversation on screen: nothing moves.
        app.work.running = true;
        app.work.conv_id = Some(b.clone());
        app.work.folder = Some(fa.clone());
        app.apply_work_update(WorkUpdate::TurnComplete, cx);
    });
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(app.work.folder.as_deref(), Some(fa.as_path()));
    });
}

/// RC2-RUN (S-Q1): a tool the owner named "reasoning" is a tool — it never
/// merges into Awili's own thinking row (a typed row kind), its outcome is
/// its own, and a Stop that cuts it off leaves it neutral, never ✓.
#[gpui::test]
fn rc2_run_an_owner_tool_named_reasoning_is_a_tool(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("work::inapp_files_tests::rc2_run_an_owner_tool_named_reasoning_is_a_tool")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.work.panel = RunPanel::default();
        app.work.running = true;
        let feed = |app: &mut AwiliApp, ev: WorkEvent, cx: &mut gpui::Context<AwiliApp>| {
            for up in work_event_to_updates(ev) {
                app.apply_work_update(up, cx);
            }
        };
        feed(app, WorkEvent::ReasoningDelta { text: "hmm".into() }, cx);
        feed(
            app,
            WorkEvent::ToolStarted {
                name: Some("reasoning".into()),
                detail: None,
                file_path: None,
            },
            cx,
        );
        assert_eq!(app.work.panel.tool_calls.len(), 2, "two rows, not one");
        feed(
            app,
            WorkEvent::ReasoningDelta {
                text: "more".into(),
            },
            cx,
        );
        assert_eq!(app.work.panel.tool_calls.len(), 2);
        let kinds: Vec<RowKind> = app.work.panel.tool_calls.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, [RowKind::Reasoning, RowKind::Tool]);
        assert_eq!(app.work.panel.tool_calls[1].title(), "reasoning");
        app.apply_work_update(
            WorkUpdate::Terminal {
                status: RunStatus::Stopped,
                output: None,
                model: None,
            },
            cx,
        );
        let st: Vec<ToolCallStatus> = app.work.panel.tool_calls.iter().map(|t| t.status).collect();
        assert_eq!(
            st,
            [ToolCallStatus::Completed, ToolCallStatus::Ended],
            "the owner's tool cut off by the Stop ends neutral"
        );
    });
}

/// RC2-RUN (round 6 Minors 1 and 2): what a Gemma run shows while it starts
/// and works is plain words — no MCP bookkeeping ("reconciled: 3 live, 0
/// pending"), no "Waiting for the agent's plan…", no engine tool names
/// ("fs.glob / fs.glob done", "reasoning / thinking…"). A real connector
/// problem still shows, in one plain line.
#[gpui::test]
fn rc2_run_progress_and_tools_speak_plain_words(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("work::inapp_files_tests::rc2_run_progress_and_tools_speak_plain_words")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.work.panel = RunPanel::default();
        app.work.running = true;
        let feed = |app: &mut AwiliApp, ev: WorkEvent, cx: &mut gpui::Context<AwiliApp>| {
            for up in work_event_to_updates(ev) {
                app.apply_work_update(up, cx);
            }
        };
        let mut seen: Vec<String> = vec![progress_empty_text(true, false, false).to_string()];
        for line in [
            "mcp-shell: live (g1, epoch 3, Attested), 4 tools",
            "reconciled: 3 live, 0 pending",
            "mcp: install grants seeded for mac-shell",
        ] {
            feed(
                app,
                WorkEvent::StatusUpdate {
                    kind: Some("mcp".into()),
                    message: Some(line.into()),
                },
                cx,
            );
            assert_eq!(app.work.panel.activity, None, "{line} is not progress");
        }
        feed(app, WorkEvent::ReasoningDelta { text: "hmm".into() }, cx);
        seen.extend(app.work.panel.activity.clone());
        feed(
            app,
            WorkEvent::ToolStarted {
                name: Some("fs.glob".into()),
                detail: None,
                file_path: None,
            },
            cx,
        );
        seen.extend(app.work.panel.activity.clone());
        feed(
            app,
            WorkEvent::ToolFinished {
                name: Some("fs.glob".into()),
                detail: None,
                duration_s: None,
                ok: Some(true),
            },
            cx,
        );
        seen.extend(app.work.panel.activity.clone());
        for t in &app.work.panel.tool_calls {
            seen.push(t.title().to_string());
            seen.extend(t.message.clone());
        }
        assert_eq!(
            seen,
            [
                "Working on it…",
                "Thinking…",
                "Listing files…",
                "Listed the files",
                "Thinking",
                "List files",
                "Listed the files",
            ]
        );
        feed(
            app,
            WorkEvent::StatusUpdate {
                kind: Some("mcp".into()),
                message: Some("mac-shell: tool \"x\" dropped: BadSchema".into()),
            },
            cx,
        );
        assert_eq!(app.work.panel.activity.as_deref(), Some(MCP_PROBLEM_LINE));
    });
}

/// RC2-FIXA1 (RC2-GEMMA review P1): the model server cuts a reply off and
/// the engine asks again. The reply row ON SCREEN drops the cut text at once
/// (the stream cadence redraws a row only when its text grows), and the
/// answer asked again shows in its place.
#[gpui::test]
fn rc2fixa_a_cut_reply_leaves_the_drawn_row_at_once(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_files_tests::rc2fixa_a_cut_reply_leaves_the_drawn_row_at_once")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let entity = cx.new(AwiliApp::new);
    let slot = entity.update(cx, |app, cx| {
        in_app_session(app, &home, dir.path());
        let slot = capture_worker();
        app.submit_work_run("Tell me about herons.".into(), cx);
        slot
    });
    let ctx = slot
        .borrow_mut()
        .take()
        .expect("the in-app worker was spawned");
    let _ = next_system(&ctx);
    // What the row draws: its text and its rendered blocks.
    let drawn = |app: &AwiliApp| {
        let row = &app.views[CONV].rows[app.work.stream_row_idx.expect("the reply row")];
        (row.raw.to_string(), format!("{:?}", row.plan))
    };
    for ev in [
        WorkEvent::TurnStarted {
            model: Some("/models/gemma-4-26b".into()),
        },
        WorkEvent::Token("Herons are birds.\n\n".into()),
        WorkEvent::Token("They wade in CUT-OFF".into()),
    ] {
        ctx.events.send_blocking(ev).unwrap();
    }
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        let (raw, plan) = drawn(app);
        assert_eq!(raw, "Herons are birds.\n\nThey wade in CUT-OFF", "setup");
        assert!(plan.contains("CUT-OFF"), "setup: {plan}");
    });
    ctx.events
        .send_blocking(WorkEvent::StatusUpdate {
            kind: Some(awili_engine_worker::REPLY_RESTART.into()),
            message: Some("They wade in CUT-OFF".into()),
        })
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        let (raw, plan) = drawn(app);
        assert_eq!(raw, "Herons are birds.\n\n", "the cut text is gone at once");
        assert!(!plan.contains("CUT-OFF"), "nor drawn: {plan}");
        assert!(plan.contains("Herons are birds."), "{plan}");
        assert_eq!(app.work.panel.activity.as_deref(), Some(REPLY_RESTART_LINE));
    });
    ctx.events
        .send_blocking(WorkEvent::Token("They wade in shallow water.".into()))
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        let (raw, plan) = drawn(app);
        assert_eq!(raw, "Herons are birds.\n\nThey wade in shallow water.");
        assert!(!plan.contains("CUT-OFF"), "{plan}");
        app.work.gateway_cmd_tx = None;
    });
}

/// RC2-R616 (usability RC2 round 5, Major 1): a cold first Gemma turn sat on
/// "Reading your request… 85%" for about 40 s with nothing else, and a
/// Codex turn showed nothing for about 75 s. A waiting Work turn's Progress
/// line says how long it has waited (so it never looks frozen) and, when the
/// model server has sent nothing new while reading, says that too.
#[test]
fn rc2r616_a_wait_says_how_long() {
    use std::time::Duration;
    let reading = "Reading your request\u{2026} 85%";
    assert_eq!(
        crate::work::alive_line(reading, None, Duration::ZERO),
        reading,
        "no run: as it is"
    );
    assert_eq!(
        crate::work::alive_line(reading, Some(Duration::from_secs(2)), Duration::ZERO),
        reading,
        "a short wait: as it is"
    );
    assert_eq!(
        crate::work::alive_line(
            reading,
            Some(Duration::from_secs(41)),
            Duration::from_secs(4)
        ),
        "Reading your request\u{2026} 85% \u{b7} 41s"
    );
    assert_eq!(
        crate::work::alive_line(reading, Some(Duration::from_secs(41)), Duration::from_secs(18)),
        "Reading your request\u{2026} 85% \u{b7} 41s \u{b7} nothing new from the model server for 18s"
    );
    assert_eq!(
        crate::work::alive_line(
            "Codex is working\u{2026}",
            Some(Duration::from_secs(75)),
            Duration::from_secs(60)
        ),
        "Codex is working\u{2026} \u{b7} 75s",
        "only the reading line talks about the model server"
    );
    // A council member waiting for its first word says how long too.
    let m = awili_council::Member::new("m3", "in-app", "Gemma 4 26B (Studio)", 2);
    let t = awili_council::SeatTurn::running("t3", &m, 1_000);
    assert_eq!(
        crate::council_view::card_wait_shown(&t, 1_002),
        "Answering\u{2026}"
    );
    assert_eq!(
        crate::council_view::card_wait_shown(&t, 1_024),
        "Answering\u{2026} \u{b7} 24s"
    );
}

/// RC2-R616: the Progress card draws the running turn's line with its time.
#[gpui::test]
fn rc2r616_the_progress_line_shows_the_wait(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated("work::inapp_files_tests::rc2r616_the_progress_line_shows_the_wait")
    else {
        return;
    };
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(900.)));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(crate::work::Mode::Work, cx);
        app.shell.job_open = true;
        app.work.running = true;
        app.work.run_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(41));
        app.work.panel.activity = Some("Reading your request\u{2026} 85%".into());
        cx.notify();
    });
    window.run_until_parked();
    let drawn = (41..50).any(|s| {
        let key = format!(
            "{}Reading your request\u{2026} 85% \u{b7} {s}s",
            AwiliApp::PROGRESS_ACTIVITY_SELECTOR
        );
        window
            .debug_bounds(Box::leak(key.into_boxed_str()))
            .is_some()
    });
    assert!(drawn, "the line says how long it has waited");
    view.update(window, |app, _| app.work.running = false);
}
