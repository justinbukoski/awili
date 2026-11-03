//! R1g proof: the real `AwiliApp` (built by `AwiliApp::new` in a subprocess
//! with a private HOME) in a test window. The Work plan card is read back
//! through its `debug_selector` bounds: it sits right above the composer, in
//! the composer's column, inside the window; a plan that replaced an earlier,
//! undecided one carries a visible note.

use std::path::{Path, PathBuf};

use awili_code::{CodeEngine, PLAN_READY_MARKER};
use gpui::{px, size, Bounds, Pixels};

use super::*;
use crate::AwiliApp;

const CHILD: &str = "AWILI_R1G_TEST_HOME";
const CONV: &str = "r1g-conv";

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

/// A Work conversation on Claude in a real folder; `claude_bin` is the CLI.
fn work_plan_app(app: &mut AwiliApp, home: &Path, claude_bin: String) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.work
        .graduation
        .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
    app.views.insert(
        CONV.into(),
        crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
    );
    app.set_active_conversation(CONV);
    app.mode = crate::work::Mode::Work;
    app.work.executor = WorkExecutor::Agent {
        engine: CodeEngine::Claude,
        model: None,
    };
    app.work.agents.claude_bin = Some(claude_bin);
    let folder = home.join("f1");
    std::fs::create_dir_all(&folder).unwrap();
    app.work.folder = Some(folder);
}

/// Choose Claude as the thread's next-turn target the way the chooser does: a
/// run follows the conversation's route, not the ambient executor field.
fn choose_claude(app: &mut AwiliApp, cx: &mut gpui::Context<AwiliApp>) {
    assert!(app.set_composer_target(
        &crate::workshop_composer::ModelOwner::Work(CONV.into()),
        WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        },
        cx,
    ));
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn planning_claude(home: &Path) -> String {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` that answers every run with a plan ending in the marker.
fn planning_claude(home: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let script = r#"#!/bin/sh
printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-r1g"}'
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"1. rename the files\n[[awili:plan-ready]]"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"ok","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
    let dir = home.join("fake");
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("claude");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    bin.display().to_string()
}

/// Drive the window until the Work run has finished.
fn wait_idle(window: &mut gpui::VisualTestContext, view: &gpui::Entity<AwiliApp>) {
    let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    loop {
        window.run_until_parked();
        if !view.read_with(window, |app, _| app.work.running) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the agent turn never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// The card's and the composer row's bounds, asserting the card is right
/// above the composer, in its column, and inside an 800 px window.
fn card_above_composer(window: &mut gpui::VisualTestContext) -> Bounds<Pixels> {
    let card = window
        .debug_bounds(WORK_PLAN_CARD_SELECTOR)
        .expect("the plan card is drawn");
    let composer = window
        .debug_bounds(COMPOSER_ROW_SELECTOR)
        .expect("the composer row is drawn");
    assert!(
        card.bottom() <= composer.top() && composer.top() - card.bottom() <= px(40.),
        "the card sits right above the composer: card {card:?}, composer {composer:?}; wrapper={:?}; frame={:?}; footer={:?}",
        window.debug_bounds("awili-composer-plan-wrapper"),
        window.debug_bounds("ui-composer-frame-awili-composer"),
        window.debug_bounds("ui-composer-frame-awili-composer-footer")
    );
    assert!(
        card.left() < composer.right() && card.right() > composer.left(),
        "the card is in the composer's column, not the side panel: card {card:?}, composer {composer:?}"
    );
    assert!(
        card.top() >= px(0.) && card.bottom() <= px(800.),
        "the whole card is on screen: {card:?}"
    );
    card
}

/// Major 5: an open Work plan renders its card right above the composer
/// (the Code placement), whatever the side panel's scroll position; no
/// replaced-plan note on a first plan.
#[gpui::test]
fn r1g_work_plan_card_sits_above_the_composer(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("plan_gate::r1g_tests::r1g_work_plan_card_sits_above_the_composer")
    else {
        return;
    };
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    view.update(window, |app, cx| {
        work_plan_app(
            app,
            &home,
            home.join("no-such-claude").display().to_string(),
        );
        let key = AskKey::Work(CONV.into());
        app.work
            .plan_gates
            .arm(key.clone(), CodeEngine::Claude, None);
        let (_, opened) =
            app.work
                .plan_gates
                .settle(&key, &format!("1. edit\n{PLAN_READY_MARKER}"), true);
        assert!(opened && app.plan_gate_pending_here());
        cx.notify();
    });
    window.run_until_parked();
    card_above_composer(window);
    assert!(
        window.debug_bounds(PLAN_REPLACED_NOTE_SELECTOR).is_none(),
        "a first plan has no replaced-plan note"
    );

    view.update(window, |app, cx| {
        app.work.plan_gates.deny(&AskKey::Work(CONV.into()));
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(WORK_PLAN_CARD_SELECTOR).is_none(),
        "no card once the plan is decided"
    );
}

/// Major 5: a second request while a card is open runs a new planning step
/// over the real (fake-CLI) agent lane; its card says the earlier plan was
/// set aside, above the composer. Declining it clears the note.
#[gpui::test]
fn r1g_second_request_card_says_the_first_plan_was_set_aside(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("plan_gate::r1g_tests::r1g_second_request_card_says_the_first_plan_was_set_aside")
    else {
        return;
    };
    let store = awili_persist::Store::open_default().unwrap();
    store.create_conversation(CONV, "plan", 0).unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    let bin = planning_claude(&home);
    view.update(window, |app, cx| {
        work_plan_app(app, &home, bin);
        choose_claude(app, cx);
        app.submit_work_run("Rename every .txt to .md".into(), cx);
        assert!(app.work.running);
    });
    wait_idle(window, &view);
    assert!(view.read_with(window, |app, _| app.plan_gate_pending_here()));
    card_above_composer(window);
    assert!(window.debug_bounds(PLAN_REPLACED_NOTE_SELECTOR).is_none());

    view.update(window, |app, cx| {
        app.submit_work_run("Also put them in a notes folder".into(), cx);
        assert!(app.work.running, "the second request runs");
    });
    wait_idle(window, &view);
    assert!(view.read_with(window, |app, _| app.plan_gate_pending_here()));
    let card = card_above_composer(window);
    let note = window
        .debug_bounds(PLAN_REPLACED_NOTE_SELECTOR)
        .expect("the card says the earlier plan was set aside");
    assert!(
        card.top() <= note.top() && note.bottom() <= card.bottom(),
        "the note is inside the card: note {note:?}, card {card:?}"
    );

    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Deny, cx);
        assert!(
            !app.work
                .plan_gates
                .replaced_earlier(&AskKey::Work(CONV.into())),
            "a decided plan clears the note"
        );
    });
}

/// RC1-N item 4: the hostd plan from round 4 (M1), which ends with two
/// questions, and the M2 plan, which does not.
const M1_PLAN_END: &str = "**Plan:**\n1. **Pre-checks (read-only):** `zpool status vault`.\n\
4. **Shut down:** `sudo systemctl poweroff` on hostd, then confirm it's off with ping and iDRAC.\n\n\
Two things to confirm:\n- A) Shut down now, or B) at a set time?\n- Is this for the `eno1` cable \
or the P100 power cable, or something else? It decides which hardware checks I add to step 5.";
const M2_PLAN_END: &str = "I can prepare hostd for a graceful shutdown. See the [outage \
record](https://hostb.example/doc/x?id=1).\n\n1. Check hostd's current workloads.\n2. Cordon hostd \
and drain only workloads that can safely move.\n3. Record the planned outage, then power off hostd.";

/// RC1-N item 4: which plan endings count as open questions.
#[test]
fn rc1n_plans_that_end_with_questions_are_recognised() {
    assert!(plan_ends_with_questions(M1_PLAN_END));
    assert!(!plan_ends_with_questions(M2_PLAN_END));
    // Only asking whether to start is not an open question.
    for go in [
        "1. edit a.txt\n\nShall I go ahead?",
        "1. edit a.txt\n\nWant me to start?",
        "1. edit a.txt\n\n**Ready to make the changes?**",
        "1. edit a.txt\n\nProceed?",
        "1. edit a.txt\n\nShall I start now?",
        "1. edit a.txt\n\nShall we proceed?",
        "1. edit a.txt\n\nShould we go ahead?",
        "1. edit a.txt\n\nWant me to proceed with the plan?",
    ] {
        assert!(!plan_ends_with_questions(go), "{go}");
    }
    for open in [
        "1. edit a.txt\n\nShould it be a.txt or b.txt?",
        "1. edit a.txt\n\nWhich port should it use? I assumed 8080.",
        "1. edit a.txt\n\n- What name do you want?",
        // Pre-review P2: timing, choice and yes/no questions about the work.
        "1. edit a.txt\n\nWhen should we start?",
        "1. edit a.txt\n\nShould I start now or after the backup?",
        "1. edit a.txt\n\nShould I use port 8080?",
        "1. edit a.txt\n\nHow soon do you want me to proceed?",
        "1. edit a.txt\n\nCan I start Tuesday?",
        "1. edit a.txt\n\nShall I start with the backup?",
        "1. edit a.txt\n\nShould I proceed with port 8080?",
        "1. edit a.txt\n\nShall we start on Monday?",
        "1. edit a.txt\n\nWould you like me to proceed with these checks and the shutdown?",
    ] {
        assert!(plan_ends_with_questions(open), "{open}");
    }
    // A question earlier in the plan, then a plain last paragraph: no.
    assert!(!plan_ends_with_questions(
        "Is hostd up? Yes.\n\n1. edit a.txt\n2. restart it."
    ));
    // Pre-review P2 (pass 8): question marks in code are not questions.
    for code in [
        "1. edit a.txt\n\nThe port falls back with config.port ?? 8080 then.",
        "1. edit a.txt\n\nIt reads `let p = x? ;` and `a ? b : c` as before.",
        "1. edit a.txt\n\n```\nlet n = parse(s)? + 1;\n```",
        "1. edit a.txt\n\nUse condition ? a : b for the fallback.",
    ] {
        assert!(!plan_ends_with_questions(code), "{code}");
    }
    assert!(plan_ends_with_questions(
        "1. edit a.txt\n\nKeep `x ?? y`, or use the default? Tell me."
    ));
    // Pre-review P2 (pass 10): code later on the line does not hide it.
    assert!(plan_ends_with_questions(
        "1. edit a.txt\n\nShould I use port 8080? Example: `cond ? a : b`."
    ));
    assert!(plan_ends_with_questions(
        "1. edit a.txt\n\nWhich mapping should I use? old : new"
    ));
    // A spaced `?` is weighed only against its own expression, not code later
    // on the line.
    assert!(plan_ends_with_questions(
        "1. edit a.txt\n\nIs that the port you want ? Example: `cond ? a : b`."
    ));
    // A URL's query is not a question.
    assert!(!plan_ends_with_questions(
        "1. edit a.txt\n\nSee https://x.example/a?b=c for details."
    ));
}

/// RC1-N item 4 (round 4 M1/M2): a plan that ends with questions gets a
/// card that says so plainly, with "Answer the questions" as its first
/// (filled) pill — pressing it puts the cursor in the box — while "Make the
/// changes" stays on the card and still works (the owner stays in control).
/// A plan without questions keeps the plain card.
#[gpui::test]
fn rc1n_a_plan_with_questions_makes_answering_the_first_action(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "plan_gate::r1g_tests::rc1n_a_plan_with_questions_makes_answering_the_first_action",
    ) else {
        return;
    };
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    let key = AskKey::Work(CONV.into());
    let open_plan = |view: &gpui::Entity<AwiliApp>,
                     window: &mut gpui::VisualTestContext,
                     text: &'static str| {
        let key = key.clone();
        view.update(window, |app, cx| {
            app.work
                .plan_gates
                .arm(key.clone(), CodeEngine::Claude, None);
            let (_, opened) =
                app.work
                    .plan_gates
                    .settle(&key, &format!("{text}\n{PLAN_READY_MARKER}"), true);
            assert!(opened && app.plan_gate_pending_here());
            cx.notify();
        });
        window.run_until_parked();
    };
    view.update(window, |app, _| {
        work_plan_app(
            app,
            &home,
            home.join("no-such-claude").display().to_string(),
        )
    });

    // No questions: the plain card, no answer pill.
    open_plan(&view, window, M2_PLAN_END);
    card_above_composer(window);
    assert!(window.debug_bounds(PLAN_QUESTIONS_SELECTOR).is_none());
    assert!(window.debug_bounds(PLAN_ANSWER_SELECTOR).is_none());
    assert!(!view.read_with(window, |app, _| app.work.plan_gates.has_questions(&key)));

    // Questions: the card says so, and answering comes first.
    open_plan(&view, window, M1_PLAN_END);
    let card = card_above_composer(window);
    let note = window
        .debug_bounds(PLAN_QUESTIONS_SELECTOR)
        .expect("the card says the plan ends with questions");
    assert!(card.top() <= note.top() && note.bottom() <= card.bottom());
    let answer = window
        .debug_bounds(PLAN_ANSWER_SELECTOR)
        .expect("the answer pill is drawn");
    let approve = window
        .debug_bounds(PLAN_APPROVE_SELECTOR)
        .expect("Make the changes stays on the card");
    assert!(
        answer.right() <= approve.left(),
        "answering is the first pill: answer {answer:?}, approve {approve:?}"
    );
    // The cursor is elsewhere; the answer pill puts it in the box.
    let other = view.read_with(window, |a, cx| {
        a.search_input.read(cx).focus_handle().clone()
    });
    window.update(|window, cx| window.focus(&other, cx));
    window.run_until_parked();
    window.simulate_click(answer.center(), gpui::Modifiers::none());
    window.run_until_parked();
    let composer = view.read_with(window, |a, cx| a.composer.read(cx).focus_handle().clone());
    assert!(window.update(|window, _| composer.is_focused(window)));
    assert!(
        view.read_with(window, |app, _| app.plan_gate_pending_here()),
        "answering decides nothing; the plan stays open"
    );

    // "Make the changes" still approves the plan.
    window.simulate_click(approve.center(), gpui::Modifiers::none());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert!(app.work.plan_gates.open_for(&key).is_none());
        assert!(!app.work.plan_gates.has_questions(&key));
        assert!(app.views[CONV]
            .convo
            .messages()
            .iter()
            .any(|m| m.text == awili_code::PLAN_APPROVED_TEXT));
    });
}

/// RC2-RUN (round 3 Minor 14, R1d): once the conversation points at another
/// folder, the open plan's card no longer says "Ready to make changes" with
/// a live "Make the changes": it says the plan cannot run here and why (the
/// refusal is not only a toast). Choosing the plan's folder again brings
/// the ready card back.
#[gpui::test]
fn rc2_run_a_plan_that_cannot_run_here_says_so_on_its_card(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("plan_gate::r1g_tests::rc2_run_a_plan_that_cannot_run_here_says_so_on_its_card")
    else {
        return;
    };
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    let other = home.join("f2");
    std::fs::create_dir_all(&other).unwrap();
    let planned = view.update(window, |app, cx| {
        work_plan_app(
            app,
            &home,
            home.join("no-such-claude").display().to_string(),
        );
        let key = AskKey::Work(CONV.into());
        let planned = app.work.folder.clone().unwrap();
        app.work
            .plan_gates
            .arm(key.clone(), CodeEngine::Claude, None);
        app.work
            .plan_gates
            .bind_folder(&key, Some(planned.display().to_string()));
        let (_, opened) =
            app.work
                .plan_gates
                .settle(&key, &format!("1. edit\n{PLAN_READY_MARKER}"), true);
        assert!(opened && app.plan_gate_pending_here());
        cx.notify();
        planned
    });
    window.run_until_parked();
    assert!(window.debug_bounds(PLAN_APPROVE_SELECTOR).is_some());
    assert!(window.debug_bounds(PLAN_BLOCKED_SELECTOR).is_none());

    view.update(window, |app, cx| {
        app.set_work_folder(other.clone(), cx);
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(WORK_PLAN_CARD_SELECTOR).is_some(),
        "the plan is kept"
    );
    // The plan owns the folder it was made in; the conversation's current
    // folder is next-turn state, so Approve stays live and nothing says
    // "blocked".
    assert!(
        window.debug_bounds(PLAN_APPROVE_SELECTOR).is_some(),
        "Make the changes stays available after a folder change"
    );
    assert!(
        window.debug_bounds(PLAN_BLOCKED_SELECTOR).is_none(),
        "a folder change blocks nothing"
    );
    assert!(window.debug_bounds(PLAN_DENY_SELECTOR).is_some());
    view.update(window, |app, _cx| {
        let key = AskKey::Work(CONV.into());
        assert_eq!(app.work_plan_refusal(&key), None);
    });

    view.update(window, |app, cx| {
        app.set_work_folder(planned, cx);
        cx.notify();
    });
    window.run_until_parked();
    assert!(window.debug_bounds(PLAN_APPROVE_SELECTOR).is_some());
    assert!(window.debug_bounds(PLAN_BLOCKED_SELECTOR).is_none());
}

/// RC2-RUN (round 4 Minor 10): the rows Awili writes for a plan-card click
/// ("Approved — go ahead…", "Declined the plan — …") are drawn as a muted
/// note about the click ("You chose “Make the changes”."), never as a "You"
/// bubble the owner seems to have typed; a real message keeps its bubble.
/// RC2-RUNF (review P2): the rows are marked as clicks; the same words typed
/// by the owner (rows 3 and 4) keep their bubble.
#[gpui::test]
fn rc2_run_plan_clicks_are_notes_not_you_bubbles(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("plan_gate::r1g_tests::rc2_run_plan_clicks_are_notes_not_you_bubbles")
    else {
        return;
    };
    assert_eq!(
        plan_click_note(awili_code::PLAN_APPROVED_TEXT).as_deref(),
        Some("You chose \u{201C}Make the changes\u{201D}.")
    );
    assert_eq!(
        plan_click_note(awili_code::PLAN_DECLINED_TEXT).as_deref(),
        Some("You chose \u{201C}Don't change anything\u{201D} \u{2014} nothing was changed.")
    );
    assert_eq!(plan_click_note("Approved"), None);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    view.update(window, |app, cx| {
        work_plan_app(
            app,
            &home,
            home.join("no-such-claude").display().to_string(),
        );
        let mut state = awili_core::ConversationState::new();
        state.push(awili_core::Role::User, "Rename the files");
        let click = awili_core::MessageKind::PlanClick;
        state.push_kind(
            awili_core::Role::User,
            awili_code::PLAN_APPROVED_TEXT,
            click,
        );
        state.push_kind(
            awili_core::Role::User,
            awili_code::PLAN_DECLINED_TEXT,
            click,
        );
        state.push(awili_core::Role::User, awili_code::PLAN_APPROVED_TEXT);
        state.push(awili_core::Role::User, awili_code::PLAN_DECLINED_TEXT);
        app.views
            .insert(CONV.into(), crate::conv_view::ConvView::from_state(state));
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(PLAN_CLICK_NOTE_SELECTOR, "rc2-plan-click-note");
    assert!(
        window.debug_bounds("rc2-plan-click-note-0").is_none(),
        "a typed message"
    );
    assert!(
        window.debug_bounds("rc2-plan-click-note-1").is_some(),
        "the Approve click"
    );
    assert!(
        window.debug_bounds("rc2-plan-click-note-2").is_some(),
        "the Decline click"
    );
    for typed in ["rc2-plan-click-note-3", "rc2-plan-click-note-4"] {
        assert!(
            window.debug_bounds(typed).is_none(),
            "the same words typed by the owner keep their bubble ({typed})"
        );
    }
}

/// RC2-RUN (round 7 Minor 3): picking another helper in the chip menu never
/// moves the message box — at any window width the composer row stays where
/// it was, whatever the chip labels and the status text now say.
#[gpui::test]
fn rc2_run_picking_a_helper_never_moves_the_message_box(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("plan_gate::r1g_tests::rc2_run_picking_a_helper_never_moves_the_message_box")
    else {
        return;
    };
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        work_plan_app(
            app,
            &home,
            home.join("no-such-claude").display().to_string(),
        );
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Gemma 4 26B (Studio)".into();
        cx.notify();
    });
    let helpers = [
        WorkExecutor::InApp,
        WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: Some("claude-opus-4-1-20250805".into()),
        },
        WorkExecutor::Agent {
            engine: CodeEngine::Codex,
            model: None,
        },
        WorkExecutor::Gateway,
        WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None,
        },
    ];
    for width in [843., 900., 1000., 1100., 1280.] {
        window.simulate_resize(size(px(width), px(800.)));
        let mut tops = Vec::new();
        for helper in &helpers {
            view.update(window, |app, cx| {
                app.pick_work_executor(helper.clone(), cx);
                cx.notify();
            });
            window.run_until_parked();
            let row = window
                .debug_bounds(COMPOSER_ROW_SELECTOR)
                .expect("the composer row is drawn");
            tops.push(row.top());
        }
        assert!(
            tops.windows(2).all(|w| w[0] == w[1]),
            "the message box moved at width {width}: {tops:?}"
        );
    }
}

/// RC2-RUNF (review P2): the owner types the plan card's exact sentences
/// himself ("Declined the plan — …", "Approved — go ahead…"): they stay
/// normal "You" bubbles. Only the rows the card's clicks write are notes —
/// live, in storage, and after a relaunch.
#[gpui::test]
fn rc2runf_a_typed_plan_sentence_stays_a_bubble(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("plan_gate::r1g_tests::rc2runf_a_typed_plan_sentence_stays_a_bubble")
    else {
        return;
    };
    // The thread is listed at startup (the chooser accepts a target only for a
    // conversation the app lists), as well as in the file this test reads back.
    awili_persist::Store::open_default()
        .unwrap()
        .create_conversation(CONV, "plan", 0)
        .unwrap();
    let db = home.join("runf.db");
    let store = awili_persist::Store::open(&db).unwrap();
    store.create_conversation(CONV, "plan", 0).unwrap();
    let (persist, worker) = crate::persistence::Persistence::spawn_worker(store);
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    let bin = planning_claude(&home);
    view.update(window, |app, cx| {
        work_plan_app(app, &home, bin);
        choose_claude(app, cx);
        app.persist = Some(persist);
    });
    for (typed, decision) in [
        (awili_code::PLAN_DECLINED_TEXT, ApprovalDecision::Deny),
        (awili_code::PLAN_APPROVED_TEXT, ApprovalDecision::Approve),
    ] {
        view.update(window, |app, cx| app.submit_work_run(typed.into(), cx));
        wait_idle(window, &view);
        assert!(view.read_with(window, |app, _| app.plan_gate_pending_here()));
        view.update(window, |app, cx| app.resolve_work_approval(decision, cx));
        wait_idle(window, &view);
    }
    let users = |app: &AwiliApp| -> Vec<(String, bool)> {
        app.views[CONV]
            .rows
            .iter()
            .filter(|r| r.role == awili_core::Role::User)
            .map(|r| (r.raw.to_string(), r.plan_click))
            .collect()
    };
    let expected = vec![
        (awili_code::PLAN_DECLINED_TEXT.to_string(), false),
        (awili_code::PLAN_DECLINED_TEXT.to_string(), true),
        (awili_code::PLAN_APPROVED_TEXT.to_string(), false),
        (awili_code::PLAN_APPROVED_TEXT.to_string(), true),
    ];
    assert_eq!(view.read_with(window, |app, _| users(app)), expected);

    // Stored with the mark: only the clicks.
    let persist = view.update(window, |app, _| app.persist.take()).unwrap();
    drop(persist);
    worker.join().unwrap();
    let rows = awili_persist::Store::open(&db)
        .unwrap()
        .messages(CONV)
        .unwrap();
    let stored: Vec<&str> = rows
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.kind.as_str())
        .collect();
    assert_eq!(stored, ["normal", "plan_click", "normal", "plan_click"]);

    // After a relaunch: the clicks are notes, the typed sentences bubbles.
    let restored = crate::restore_conversation(&rows);
    let state = view.update(window, |app, cx| {
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(restored),
        );
        cx.notify();
        assert_eq!(users(app), expected);
        app.views[CONV]
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.role == awili_core::Role::User)
            .map(|(i, r)| (i, r.plan_click))
            .collect::<Vec<_>>()
    });
    window.run_until_parked();
    for (idx, click) in state {
        let note: &'static str =
            Box::leak(format!("{PLAN_CLICK_NOTE_SELECTOR}-{idx}").into_boxed_str());
        assert_eq!(
            window.debug_bounds(note).is_some(),
            click,
            "row {idx}: a note only for a click"
        );
    }
}
