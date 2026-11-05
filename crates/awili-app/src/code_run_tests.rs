//! M2e proof: a real Code turn in an isolated app, painted in a test window.
//! Enter goes through the window's key dispatch to the main composer, the
//! turn spawns a fake `claude` that only sleeps (no real engine), and Stop is
//! a click on the painted strip button.

use crate::work::Mode;
use gpui::prelude::*;
use gpui::AppContext;

/// Run `test` in a subprocess with a private HOME (`AwiliApp::new` reads
/// config and opens persistence). Same pattern as `s1a_proof_tests::isolated`.
/// Returns the private home inside the child; `None` in the parent once the
/// child has passed.
fn isolated(test: &str) -> Option<std::path::PathBuf> {
    const CHILD: &str = "AWILI_M2E_HOME";
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
        output.status.success(),
        "isolated M2e test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "subprocess must actually execute the regression: {stdout}"
    );
    None
}

/// Paints the real Code center (`code_center`: transcript, strip, composer).
struct CodeHarness {
    app: gpui::Entity<crate::AwiliApp>,
}

impl gpui::Render for CodeHarness {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let center = self.app.update(cx, |app, cx| {
            app.sync_workshop_tabs();
            let header = crate::ui::is_compact(window).then(|| app.workshop_header(window, cx));
            let body = app.code_center(header, cx).into_any_element();
            app.workshop_frame(body, window, cx)
        });
        // The app's root key hook (`handle_focus_keys`: Tab / ⇧Tab and the
        // approval chords), as main.rs wires it on the real root.
        gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                let used = this
                    .app
                    .update(cx, |app, cx| app.handle_focus_keys(ev, window, cx));
                if used {
                    cx.stop_propagation();
                }
            }))
            .child(center)
    }
}

/// A fake `claude` that only sleeps (Stop kills it). RC1-K-FIX1: 600 s, so it
/// outlives every wait in these tests; each test stops it itself.
const SLEEPS: &str = "#!/bin/sh\nexec sleep 600\n";

/// A fake `claude` that replies "late" once the file `GATE` exists and keeps
/// running (Stop kills it and its reply job: they share its process group).
/// RC1-K: the reply waits for a file the test creates after Stop, not for a
/// clock, so a turn can never reply before its Stop however slow the machine.
/// RC1-K-FIX1: it then runs 600 s, past `wait_until`'s 60 s bound: the live
/// turn ends only when the test stops it.
const REPLIES_LATE: &str =
    "#!/bin/sh\n( while [ ! -e 'GATE' ]; do sleep 0.01; done; echo '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"late\"}]}}' ) &\nexec sleep 600\n";

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn code_app<'a>(
    cx: &'a mut gpui::TestAppContext,
    home: &std::path::Path,
    script_body: &str,
) -> (
    gpui::Entity<crate::AwiliApp>,
    &'a mut gpui::VisualTestContext,
    u64,
) {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// An isolated app in Code mode whose active tab runs a fake `claude`
/// (`script_body`), painted in a focused test window. Returns the app entity,
/// the window and the tab id.
fn code_app<'a>(
    cx: &'a mut gpui::TestAppContext,
    home: &std::path::Path,
    script_body: &str,
) -> (
    gpui::Entity<crate::AwiliApp>,
    &'a mut gpui::VisualTestContext,
    u64,
) {
    use std::os::unix::fs::PermissionsExt;
    // The fake engine's thread reports after its kill.
    cx.executor().allow_parking();
    let script = home.join("fake-claude");
    std::fs::write(&script, script_body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let entity = cx.new(crate::AwiliApp::new);
    let tab_id = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Code;
        app.ensure_code_tab(cx);
        app.work.agents.claude_bin = Some(script.display().to_string());
        let tab = &mut app.work.code_tabs[app.work.code_active];
        tab.engine = awili_code::CodeEngine::Claude;
        tab.model = None;
        let id = tab.id;
        app.test_choose_native_target(awili_code::CodeEngine::Claude, None, cx);
        id
    });
    let (_v, window) = cx.add_window_view(|_, cx| {
        cx.observe(&entity, |_, _, cx| cx.notify()).detach();
        CodeHarness {
            app: entity.clone(),
        }
    });
    window.simulate_resize(gpui::size(gpui::px(1400.), gpui::px(1000.)));
    window.update(|window, cx| {
        let composer = entity.read(cx).composer.clone();
        let focus = gpui::Focusable::focus_handle(composer.read(cx), cx);
        window.focus(&focus, cx);
    });
    window.run_until_parked();
    (entity, window, tab_id)
}

/// Type `text` into the main composer and press Enter through the window's
/// key dispatch (`simulate_event`: the test platform would also type "\n"
/// for `simulate_keystrokes("enter")`, see work_retry_tests::press_enter).
fn send(entity: &gpui::Entity<crate::AwiliApp>, window: &mut gpui::VisualTestContext, text: &str) {
    let composer = entity.read_with(window, |app, _| app.composer.clone());
    composer.update(window, |c, cx| c.set_text(text, cx));
    window.run_until_parked();
    window.simulate_event(gpui::KeyDownEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    window.run_until_parked();
}

/// Click the centre of the painted element tagged `selector`.
fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = window
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not painted"));
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
}

/// RC1-K: drive the window until `done` holds. The fake engine's events come
/// from its own threads, so this waits for them, not for a clock; 60 s of real
/// time is only the bound after which `what` fails the test.
fn wait_until(
    entity: &gpui::Entity<crate::AwiliApp>,
    window: &mut gpui::VisualTestContext,
    what: &str,
    done: impl Fn(&crate::AwiliApp) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        window.run_until_parked();
        if entity.read_with(window, |app, _| done(app)) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{what} within 60 s");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// The Code tab's transcript as `(role, text)` pairs.
fn rows(app: &crate::AwiliApp) -> Vec<(crate::Role, String)> {
    app.work.code_tabs[app.work.code_active]
        .convo
        .messages()
        .iter()
        .map(|m| (m.role, m.text.clone()))
        .collect()
}

/// M2e (S3-F04): a running Code turn paints the strip with Stop; a click on
/// Stop settles the tab at once (not when the stream closes), the strip goes
/// away, and the composer is free.
#[gpui::test]
fn m2e_stop_click_settles_the_tab_immediately(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("code_run::code_run_tests::m2e_stop_click_settles_the_tab_immediately")
    else {
        return;
    };
    let (entity, window, tab_id) = code_app(cx, &home, SLEEPS);

    send(&entity, window, "q");
    entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        assert!(tab.streaming, "the turn is running");
        assert!(app.work.code_runs.started(tab_id).is_some(), "a live run");
        assert!(app.composer_busy());
    });
    assert!(
        window.debug_bounds("code-stop").is_some(),
        "the running strip with Stop is painted"
    );

    click(window, "code-stop");
    entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        assert!(!tab.streaming, "Stop settled the tab at once");
        assert!(tab.stream_abort_tx.is_none(), "the kill was sent");
        assert!(app.work.code_runs.started(tab_id).is_none(), "no live run");
        assert!(!app.composer_busy(), "the composer is free");
    });
    assert!(
        window.debug_bounds("code-stop").is_none(),
        "the strip is gone after Stop"
    );
}

/// M2e (S3-F04): the stopped turn's late events (the killed engine's error
/// and its end) arrive after Stop has settled the tab. They touch nothing: the
/// stopped bubble stays empty, and the next turn keeps running and gets its
/// own reply. The stopped task's last step is also run once more with the
/// next turn live, so the order of that race cannot hide a missing guard.
#[gpui::test]
fn m2e_late_events_of_a_stopped_turn_touch_nothing(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("code_run::code_run_tests::m2e_late_events_of_a_stopped_turn_touch_nothing")
    else {
        return;
    };
    let gate = home.join("reply-gate");
    let script = REPLIES_LATE.replace("GATE", &gate.display().to_string());
    let (entity, window, tab_id) = code_app(cx, &home, &script);

    send(&entity, window, "q");
    let first = entity.read_with(window, |app, _| app.work.code_runs.seq(tab_id).unwrap());
    click(window, "code-stop");
    // RC1-K: replies are allowed only now, after the Stop.
    std::fs::write(&gate, "").unwrap();
    send(&entity, window, "second");
    entity.read_with(window, |app, _| {
        assert!(
            app.work.code_tabs[app.work.code_active].streaming,
            "the next Enter after Stop sends"
        );
    });

    // RC1-K: wait for the events themselves, not for a clock: the stopped
    // turn's task has received a late event, dropped it and run its last
    // step; the live turn's reply has landed.
    wait_until(
        &entity,
        window,
        "the stopped turn's task ran its last step",
        |app| {
            app.work
                .code_runs
                .last_ended()
                .is_some_and(|(seq, _)| seq == first)
        },
    );
    wait_until(&entity, window, "the live turn's reply landed", |app| {
        rows(app).get(3).is_some_and(|(_, text)| !text.is_empty())
    });
    entity.update(window, |app, cx| {
        assert_eq!(
            rows(app),
            vec![
                (crate::Role::User, "q".to_string()),
                (crate::Role::Assistant, String::new()),
                (crate::Role::User, "second".to_string()),
                (crate::Role::Assistant, "late".to_string()),
            ],
            "the stopped turn's late reply and error did not reach its bubble; \
             the live turn's own reply did"
        );
        assert!(
            app.work.code_tabs[app.work.code_active].streaming,
            "the stopped turn's late end did not end the next turn"
        );
        assert_eq!(
            app.work.code_runs.last_ended(),
            Some((first, true)),
            "the stopped turn's task received a late event and dropped it"
        );
        assert!(app.work.code_runs.started(tab_id).is_some());
        // The engine usually ends before the next turn starts, so also run the
        // stopped turn task's last step (its exact call) now, with the next
        // turn live: it must not end that turn.
        app.finish_code_turn_if_live(tab_id, first, cx);
        assert!(
            app.work.code_tabs[app.work.code_active].streaming,
            "the stopped turn's finish did not end the next turn"
        );
        assert!(app.work.code_runs.started(tab_id).is_some());
        app.cancel_code_turn(cx);
    });
}

/// M2e: Enter while the Code tab runs keeps the draft and says why, in a
/// toast; nothing is sent.
#[gpui::test]
fn m2e_busy_enter_explains_and_keeps_the_draft(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("code_run::code_run_tests::m2e_busy_enter_explains_and_keeps_the_draft")
    else {
        return;
    };
    let (entity, window, _tab_id) = code_app(cx, &home, SLEEPS);

    send(&entity, window, "q");
    send(&entity, window, "second");
    entity.update(window, |app, cx| {
        // The toast shows the platform's own chord (Ctrl+. off macOS).
        let want = crate::shortcuts::localize(crate::code_run::busy_hint(Mode::Code));
        assert_eq!(
            app.toast.visible().map(|m| m.to_string()).as_deref(),
            Some(want.as_ref()),
            "the toast says why nothing was sent"
        );
        assert_eq!(
            app.composer.read(cx).text(),
            "second",
            "the draft is still in the box"
        );
        assert_eq!(rows(app).len(), 2, "nothing was sent");
        app.cancel_code_turn(cx);
    });
}

// ---- M2f: the run footer under Code-tab replies -------------------------------

/// A fake `claude` that replies "done" and reports 1000 in / 500 out tokens.
const REPLIES_WITH_USAGE: &str = "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"usage\":{\"input_tokens\":1000,\"output_tokens\":500}}'\n";

/// M2f (F02/F03 for Code): a finished Code turn shows its footer, built from
/// the turn's ledger line: engine, model, time, tokens. It is painted under
/// the reply, and running the turn's end again changes nothing.
#[gpui::test]
fn m2f_finished_code_turn_shows_its_footer(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("code_run::code_run_tests::m2f_finished_code_turn_shows_its_footer")
    else {
        return;
    };
    let (entity, window, tab_id) = code_app(cx, &home, REPLIES_WITH_USAGE);
    let ledger = home.join("m2f-ledger.jsonl");
    entity.update(window, |app, _| {
        app.work.graduation =
            crate::graduation_record::Recorder::from_config(&ledger.to_string_lossy());
    });

    send(&entity, window, "q");
    let mut done = false;
    let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    while std::time::Instant::now() < deadline {
        window.run_until_parked();
        done = entity.read_with(window, |app, _| {
            !app.work.code_tabs[app.work.code_active].streaming
        });
        if done {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(done, "the Code turn never finished");
    window.run_until_parked();

    let footer = entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        assert_eq!(tab.convo.message(1).map(|m| m.text.as_str()), Some("done"));
        tab.rows[1].footer.clone().map(|f| f.to_string())
    });
    let footer = footer.expect("the finished reply has a footer");
    let parts: Vec<&str> = footer.split(" · ").collect();
    assert_eq!(
        parts.len(),
        4,
        "engine · model · time · tokens; Claude's window is reported only once the CLI says it: {footer}"
    );
    assert_eq!(parts[0], "Claude");
    assert_eq!(parts[1], "default model");
    assert!(parts[2].ends_with('s'), "the turn's time: {footer}");
    assert_eq!(parts[3], "1,500 tokens", "the ledger's tokens");
    assert!(
        window.debug_bounds("code-footer-1").is_some(),
        "the footer is painted under the reply"
    );
    let lines = awili_selftest::graduation::Ledger::read(&ledger).unwrap();
    assert_eq!(lines.len(), 1, "one ledger line");
    assert_eq!(
        lines[0].frontier_tokens, 1500,
        "the footer's figure is the ledger's"
    );

    // The turn's end again (a late duplicate) changes nothing.
    entity.update(window, |app, cx| app.code_turn_end(tab_id, cx));
    entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        assert_eq!(
            tab.rows[1].footer.as_ref().map(|f| f.to_string()),
            Some(footer.clone())
        );
    });
    assert_eq!(
        awili_selftest::graduation::Ledger::read(&ledger)
            .unwrap()
            .len(),
        1
    );
}

/// RC2-UX (D-PUNCH F08: the Code tab had no list of the files a turn
/// changed): the footer names the files the turn edited or wrote, relative
/// to the project, once each; Claude's plan notes are not listed.
#[gpui::test]
fn rc2ux_a_code_footer_names_the_files_it_changed(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("code_run::code_run_tests::rc2ux_a_code_footer_names_the_files_it_changed")
    else {
        return;
    };
    // RC2-FIXB1 (RC2-UX review P2): each step also reports that it finished,
    // as Claude's tool results do.
    let h = home.display().to_string();
    let lines = [
        claude_tool("t1", "Edit", &format!("{h}/notes.md")),
        claude_result("t1", false),
        claude_tool("t2", "Write", &format!("{h}/plan/todo.md")),
        claude_result("t2", false),
        claude_tool("t3", "Edit", &format!("{h}/notes.md")),
        claude_result("t3", false),
        claude_tool("t4", "Read", &format!("{h}/readme.md")),
        claude_result("t4", false),
    ];
    let footer = code_footer_after(cx, &home, &lines);
    assert!(
        footer.ends_with(" \u{b7} changed notes.md, plan/todo.md"),
        "{footer}"
    );
    let notes = [
        gpui::SharedString::from(format!(
            "{}\u{2713} Write {h}/.claude/plans/p.md",
            crate::agent_brief::PLAN_NOTES_MARK
        )),
        gpui::SharedString::from("\u{26A0} outside project \u{b7} \u{2713} Edit /etc/hosts"),
        gpui::SharedString::from("Bash: ls"),
    ];
    assert_eq!(
        crate::run_footer::changed_files_from_activity(&notes, &h),
        vec!["/etc/hosts".to_string()]
    );
    assert_eq!(
        crate::run_footer::changed_part(&["a".into(), "b".into(), "c".into(), "d".into()])
            .as_deref(),
        Some("changed 4 files")
    );
}

/// RC2-FIXB1: one Claude `tool_use` line (`id`, tool `name`, `file_path`).
fn claude_tool(id: &str, name: &str, path: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {"content": [{
            "type": "tool_use", "id": id, "name": name,
            "input": {"file_path": path}
        }]}
    })
    .to_string()
}

/// RC2-FIXB1: the Claude tool result for the call `id`.
fn claude_result(id: &str, is_error: bool) -> String {
    serde_json::json!({
        "type": "user",
        "message": {"content": [{
            "type": "tool_result", "tool_use_id": id, "content": "ok", "is_error": is_error
        }]}
    })
    .to_string()
}

/// RC2-FIXB1: run one Code turn on a fake `claude` that prints `lines`, then
/// a reply and a good result, and return the reply's footer.
fn code_footer_after(
    cx: &mut gpui::TestAppContext,
    home: &std::path::Path,
    lines: &[String],
) -> String {
    let mut lines = lines.to_vec();
    lines.push(
        serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}})
            .to_string(),
    );
    lines.push(
        serde_json::json!({"type":"result","subtype":"success","is_error":false,"result":"done",
            "usage":{"input_tokens":10,"output_tokens":5}})
        .to_string(),
    );
    footer_of_turn(cx, home, &lines, awili_code::CodeEngine::Claude)
}

/// RC2-FIXC3: run one Code turn on a fake `codex` (`exec --json`) that
/// prints `lines`, then a reply and the end of the turn, and return the
/// reply's footer.
fn codex_footer_after(
    cx: &mut gpui::TestAppContext,
    home: &std::path::Path,
    lines: &[String],
) -> String {
    let mut lines = lines.to_vec();
    lines.push(
        serde_json::json!({"type":"item.completed",
            "item":{"id":"m1","type":"agent_message","text":"done"}})
        .to_string(),
    );
    lines.push(
        serde_json::json!({"type":"turn.completed",
            "usage":{"input_tokens":10,"output_tokens":5}})
        .to_string(),
    );
    footer_of_turn(cx, home, &lines, awili_code::CodeEngine::Codex)
}

/// RC2-FIXC3: run one Code turn on `engine`, faked by a script that prints
/// `lines`, and return the reply's footer.
fn footer_of_turn(
    cx: &mut gpui::TestAppContext,
    home: &std::path::Path,
    lines: &[String],
    engine: awili_code::CodeEngine,
) -> String {
    let out = home.join("events.jsonl");
    std::fs::write(&out, lines.join("\n") + "\n").unwrap();
    let script = format!("#!/bin/sh\ncat '{}'\n", out.display());
    let (entity, window, _tab_id) = code_app(cx, home, &script);
    let ledger = home.join("rc2ux-ledger.jsonl");
    entity.update(window, |app, cx| {
        // These footer cases exercise project-relative activity paths. Bind
        // the private fixture as the selected project instead of relying on
        // code_app's HOME launch cwd, which intentionally is not a project.
        app.add_code_project(home.to_path_buf(), cx);
        app.work.graduation =
            crate::graduation_record::Recorder::from_config(&ledger.to_string_lossy());
        // RC2-FIXC3: the same fake answers as Codex.
        app.work.agents.codex_bin = app.work.agents.claude_bin.clone();
        app.work.code_tabs[app.work.code_active].engine = engine;
        app.test_choose_native_target(engine, None, cx);
    });
    send(&entity, window, "tidy the notes");
    let deadline = std::time::Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    while std::time::Instant::now() < deadline {
        window.run_until_parked();
        if entity.read_with(window, |app, _| {
            !app.work.code_tabs[app.work.code_active].streaming
        }) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    window.run_until_parked();
    let footer = entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        tab.rows[1].footer.clone().map(|f| f.to_string())
    });
    footer.expect("a footer")
}

/// RC2-FIXB1 (RC2-UX review P2: the footer said a file changed when its edit
/// had only started): an edit that started and never finished (the turn
/// went on without its result) and a write that failed are not "changed";
/// the footer says the turn tried to edit them. Only the edit that
/// finished well is "changed".
#[gpui::test]
fn rc2fixb_a_code_footer_says_tried_for_an_edit_that_never_finished(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "code_run::code_run_tests::rc2fixb_a_code_footer_says_tried_for_an_edit_that_never_finished",
    ) else {
        return;
    };
    let h = home.display().to_string();
    let lines = [
        claude_tool("t1", "Edit", &format!("{h}/notes.md")),
        claude_tool("t2", "Write", &format!("{h}/a.md")),
        claude_result("t2", true),
        claude_tool("t3", "MultiEdit", &format!("{h}/b.md")),
        claude_result("t3", false),
    ];
    let footer = code_footer_after(cx, &home, &lines);
    assert!(
        footer.ends_with(" \u{b7} changed b.md \u{b7} tried to edit notes.md, a.md"),
        "{footer}"
    );
    assert!(!footer.contains("changed notes.md"), "{footer}");
    assert_eq!(
        crate::run_footer::tried_part(&["a".into(), "b".into(), "c".into(), "d".into()]).as_deref(),
        Some("tried to edit 4 files")
    );
}

/// RC2-FIXC3 (RC2-FIXB review P3: a failed Codex file change named no file,
/// so the footer could not say "tried to edit"): a Codex turn whose change to
/// one file failed and whose change to another was applied says it changed
/// the one and tried to edit the other.
#[gpui::test]
fn rc2fixc_a_codex_footer_says_tried_for_a_change_that_failed(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "code_run::code_run_tests::rc2fixc_a_codex_footer_says_tried_for_a_change_that_failed",
    ) else {
        return;
    };
    let h = home.display().to_string();
    let change = |id: &str, path: &str, status: &str| {
        serde_json::json!({"type":"item.completed","item":{"id":id,"type":"file_change",
            "changes":[{"path":format!("{h}/{path}"),"kind":"update"}],"status":status}})
        .to_string()
    };
    let lines = [
        change("c1", "notes.md", "failed"),
        change("c2", "b.md", "completed"),
    ];
    let footer = codex_footer_after(cx, &home, &lines);
    assert!(
        footer.ends_with(" \u{b7} changed b.md \u{b7} tried to edit notes.md"),
        "{footer}"
    );
    assert!(!footer.contains("changed notes.md"), "{footer}");
}

#[gpui::test]
fn workshop_parallel_tabs_close_and_stop_keep_run_ownership(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "code_run::code_run_tests::workshop_parallel_tabs_close_and_stop_keep_run_ownership",
    ) else {
        return;
    };
    let (entity, window, first) = code_app(cx, &home, SLEEPS);
    send(&entity, window, "first private fixture");
    let first_tab = entity.read_with(window, |app, _| app.shell.tabs.active.clone().unwrap());
    let second = entity.update(window, |app, cx| {
        app.new_workshop_with(
            crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None,
            },
            cx,
        );
        app.work.code_tabs[app.work.code_active].id
    });
    window.run_until_parked();
    send(&entity, window, "second private fixture");
    entity.update(window, |app, cx| {
        assert_eq!(app.work.code_tabs.iter().filter(|s| s.streaming).count(), 2);
        app.close_workshop_tab(&first_tab, cx);
        assert!(
            app.shell.tabs.tabs.iter().any(|t| t.id == first_tab),
            "running owner cannot close"
        );
        app.select_workshop_tab(&first_tab, cx);
    });
    window.run_until_parked();
    click(window, "code-stop");
    entity.read_with(window, |app, _| {
        assert!(
            !app.work
                .code_tabs
                .iter()
                .find(|s| s.id == first)
                .unwrap()
                .streaming
        );
        assert!(
            app.work
                .code_tabs
                .iter()
                .find(|s| s.id == second)
                .unwrap()
                .streaming,
            "Stop targets only the selected run"
        );
    });
    entity.update(window, |app, cx| app.close_workshop_tab(&first_tab, cx));
    window.run_until_parked();
    entity.read_with(window, |app, _| {
        assert_eq!(app.work.code_tabs[app.work.code_active].id, second);
        assert!(app.work.code_tabs[app.work.code_active].streaming);
    });
    click(window, "code-stop");
    entity.read_with(window, |app, _| {
        assert!(!app.work.code_tabs.iter().any(|s| s.streaming))
    });
}

/// Keyboard reach of the Code Retry (the kit Button under the last reply). The
/// Retry chip has no handle of its own: its focus handle is created by GPUI
/// (`focusable()`), so the test takes it from a real pointer probe. A mouse
/// down on Retry focuses it through GPUI's focus listener; the mouse up lands
/// outside Retry, so the click is canceled and Retry's callback does not run.
/// The focused handle is then the real one. From the composer the test walks
/// actual Shift-Tab presses until that handle is focused, stopping once the
/// walk has cycled through every control it has seen. Enter KeyDown/KeyUp then
/// runs Retry. The fixture is a fake `claude` that answers at once on its first
/// turn, then holds its second and third turns until the test creates their
/// gate files. Each argv file is published by rename, so its existence means
/// the complete argument list is present. So the test checks the callback's
/// effects (a Retry row, a second CLI turn, a second answer), then a pointer
/// Retry's third turn, and proves typing during that turn stays out of its
/// request. No plan, run gate or staged attachment is set.
#[gpui::test]
fn code_retry_shift_tab_then_enter_runs_the_retry(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("code_run::code_run_tests::code_retry_shift_tab_then_enter_runs_the_retry")
    else {
        return;
    };
    let count = home.join("retry-count");
    let gate = home.join("retry-gate");
    let third_gate = home.join("retry-gate-third");
    let argv_prefix = home.join("retry-argv-");
    struct ReleaseRetryGates(Vec<std::path::PathBuf>);
    impl Drop for ReleaseRetryGates {
        fn drop(&mut self) {
            for gate in &self.0 {
                let _ = std::fs::write(gate, "release");
            }
        }
    }
    let _release_retry_gates = ReleaseRetryGates(vec![gate.clone(), third_gate.clone()]);
    let script = format!(
        "#!/bin/sh\n\
         n=0\n\
         [ ! -e '{count}' ] || n=$(cat '{count}')\n\
         n=$((n+1))\n\
         printf '%s' \"$n\" > '{count}'\n\
         {{ printf '%s\\n' \"$@\"; cat; }} > '{argv_prefix}'\"$n\"'.tmp'\n\
         mv '{argv_prefix}'\"$n\"'.tmp' '{argv_prefix}'\"$n\"\n\
         if [ \"$n\" = 2 ]; then while [ ! -e '{gate}' ]; do sleep 0.01; done; fi\n\
         if [ \"$n\" = 3 ]; then while [ ! -e '{third_gate}' ]; do sleep 0.01; done; fi\n\
         printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"CODE_RETRY_ANSWER\"}}]}}}}'\n\
         printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"CODE_RETRY_ANSWER\"}}'\n",
        count = count.display(),
        gate = gate.display(),
        third_gate = third_gate.display(),
        argv_prefix = argv_prefix.display(),
    );
    let (entity, window, tab_id) = code_app(cx, &home, &script);

    // The Retry is only clipped when the transcript is longer than its
    // viewport (the right Conversations panel left a short one fitting
    // whole). Give the tab earlier history so the settled answer sits below
    // the fold when the transcript is read from the top. The renderer walks
    // `tab.rows` and the next submit indexes `convo` by `rows.len()`, so every
    // earlier message is seeded in BOTH, one row per message, in step.
    entity.update(window, |app, _| {
        let tab = &mut app.work.code_tabs[app.work.code_active];
        assert_eq!(tab.rows.len(), tab.convo.messages().len());
        for i in 0..30 {
            for (role, text) in [
                (crate::Role::User, format!("EARLIER_QUESTION_{i}")),
                (crate::Role::Assistant, format!("EARLIER_ANSWER_{i}")),
            ] {
                let idx = tab.convo.len();
                tab.convo.push(role, text);
                tab.rows
                    .push(crate::MessageRow::new(tab.convo.message(idx).unwrap()));
            }
        }
        assert_eq!(tab.rows.len(), tab.convo.messages().len());
    });
    let top_of_transcript = gpui::point(gpui::px(0.), gpui::px(0.));

    send(&entity, window, "CODE_RETRY_PROMPT_UNIQUE");
    wait_until(&entity, window, "first code turn", |app| {
        !app.work.code_tabs[app.work.code_active].streaming
    });
    window.run_until_parked();
    let transcript_scroll = entity.read_with(window, |app, _| {
        app.work.code_tabs[app.work.code_active].scroll.clone()
    });
    transcript_scroll.scroll_to_bottom();
    window.update(|window, _| window.refresh());
    window.run_until_parked();
    let retry = window
        .debug_bounds("awili-code-retry")
        .expect("the Code Retry is painted once the answer settles");
    assert!(
        f32::from(retry.size.width) >= 44.0 && f32::from(retry.size.height) >= 44.0,
        "the Code Retry keeps a 44 px hit box: {retry:?}"
    );
    let viewport = window
        .debug_bounds("awili-code-transcript-viewport")
        .expect("the Code transcript viewport is painted");
    assert!(
        retry.left() >= viewport.left()
            && retry.right() <= viewport.right()
            && retry.top() >= viewport.top()
            && retry.bottom() <= viewport.bottom(),
        "the keyboard target starts fully visible inside the transcript: {retry:?} / {viewport:?}"
    );
    let composer = entity.read_with(window, |app, cx| {
        gpui::Focusable::focus_handle(&app.composer, cx)
    });
    // The owned session's Retry handle, created by the transcript render. The
    // probe below must focus this exact handle, not a copy of its own result.
    let owned_retry_focus = entity
        .read_with(window, |app, _| {
            app.work.code_tabs[app.work.code_active].retry_focus.clone()
        })
        .expect("the owned Code session holds its Retry focus handle");

    // Canceled pointer probe: mouse down on Retry focuses its GPUI-owned
    // handle; the mouse up outside Retry cancels the click.
    let outside = gpui::point(gpui::px(0.), gpui::px(0.));
    assert!(
        !retry.contains(&outside),
        "the cancel point lies outside Retry"
    );
    let none = gpui::Modifiers::default();
    window.simulate_mouse_down(retry.center(), gpui::MouseButton::Left, none);
    window.run_until_parked();
    let pointer_focus = window
        .update(|window, cx| window.focused(cx))
        .expect("mouse down on the Retry face focuses a control");
    assert_ne!(
        pointer_focus, composer,
        "the Retry probe does not capture the composer handle"
    );
    assert_eq!(
        pointer_focus, owned_retry_focus,
        "mouse down focuses the owned Code Retry handle"
    );
    window.simulate_mouse_up(outside, gpui::MouseButton::Left, none);
    window.run_until_parked();
    let retry_handle = window
        .update(|window, cx| window.focused(cx))
        .expect("the canceled pointer probe leaves a focused control");
    assert_eq!(
        retry_handle, owned_retry_focus,
        "mouse up outside preserves focus on the owned Code Retry handle"
    );
    entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        assert_eq!(
            tab.id, tab_id,
            "the canceled probe keeps the owned Code tab"
        );
        assert!(!tab.streaming, "the canceled probe leaves the tab idle");
        assert!(
            tab.convo
                .messages()
                .iter()
                .all(|m| m.kind != awili_core::MessageKind::Retry),
            "the canceled probe ran no Retry"
        );
    });
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "1",
        "the canceled probe ran no second fake claude turn"
    );

    // The composer is focused through its own handle, as `code_app` does.
    let composer_entity = entity.read_with(window, |app, _| app.composer.clone());
    window.update(|window, cx| {
        let focus = gpui::Focusable::focus_handle(composer_entity.read(cx), cx);
        window.focus(&focus, cx);
    });
    transcript_scroll.set_offset(top_of_transcript);
    window.update(|window, _| window.refresh());
    window.run_until_parked();
    let before_walk = window.debug_bounds("awili-code-retry").unwrap();
    let viewport = window
        .debug_bounds("awili-code-transcript-viewport")
        .unwrap();
    assert!(
        before_walk.top() < viewport.top() || before_walk.bottom() > viewport.bottom(),
        "the Retry is genuinely clipped before keyboard focus: {before_walk:?} / {viewport:?}"
    );

    // Real Shift-Tab presses: stop on Retry's handle, or once the walk cycles
    // back to a control it already saw. 4096 is only a catastrophic bound.
    let mut seen: Vec<gpui::FocusHandle> = Vec::new();
    for _ in 0..4096 {
        if window.update(|window, _| retry_handle.is_focused(window)) {
            break;
        }
        let focused = window
            .update(|window, cx| window.focused(cx))
            .expect("the focus walk keeps some control focused");
        assert!(
            !seen.contains(&focused),
            "Shift-Tab cycled through {} controls without reaching the Code Retry",
            seen.len()
        );
        seen.push(focused);
        window.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("shift-tab").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        window.run_until_parked();
    }
    assert!(
        window.update(|window, _| retry_handle.is_focused(window)),
        "the Code Retry is reached by Shift-Tab"
    );
    assert_ne!(
        retry_handle, composer,
        "the traversed Retry stop is distinct from the composer"
    );
    let focused_retry = window.debug_bounds("awili-code-retry").unwrap();
    let viewport = window
        .debug_bounds("awili-code-transcript-viewport")
        .unwrap();
    assert!(
        focused_retry.left() >= viewport.left()
            && focused_retry.right() <= viewport.right()
            && focused_retry.top() >= viewport.top()
            && focused_retry.bottom() <= viewport.bottom(),
        "keyboard focus reveals the entire Retry face: {focused_retry:?} / {viewport:?}"
    );

    // Wheel movement while Retry remains focused is still user-controlled.
    let revealed_offset = transcript_scroll.offset();
    window.simulate_event(gpui::ScrollWheelEvent {
        position: viewport.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(80.))),
        touch_phase: gpui::TouchPhase::Moved,
        ..Default::default()
    });
    window.run_until_parked();
    let wheeled_offset = transcript_scroll.offset();
    assert_ne!(
        wheeled_offset, revealed_offset,
        "wheel scrolls while Retry stays focused"
    );
    entity.update(window, |_, cx| cx.notify());
    window.run_until_parked();
    assert_eq!(
        transcript_scroll.offset(),
        wheeled_offset,
        "ordinary repaint after wheel keeps the reader's position"
    );

    // Leaving and returning to this exact focus identity clears its old layout
    // key, so a newly clipped face is revealed again.
    window.update(|window, cx| {
        let focus = gpui::Focusable::focus_handle(composer_entity.read(cx), cx);
        window.focus(&focus, cx);
    });
    transcript_scroll.set_offset(top_of_transcript);
    window.update(|window, _| window.refresh());
    window.run_until_parked();
    let before_return_walk = window.debug_bounds("awili-code-retry").unwrap();
    let viewport = window
        .debug_bounds("awili-code-transcript-viewport")
        .unwrap();
    assert!(
        before_return_walk.top() < viewport.top() || before_return_walk.bottom() > viewport.bottom(),
        "same Retry is clipped before returning to its focus stop: {before_return_walk:?} / {viewport:?}"
    );
    let mut seen: Vec<gpui::FocusHandle> = Vec::new();
    for _ in 0..4096 {
        if window.update(|window, _| retry_handle.is_focused(window)) {
            break;
        }
        let focused = window
            .update(|window, cx| window.focused(cx))
            .expect("the return focus walk keeps some control focused");
        assert!(
            !seen.contains(&focused),
            "return Shift-Tab cycled through {} controls without reaching Retry",
            seen.len()
        );
        seen.push(focused);
        window.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("shift-tab").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        window.run_until_parked();
    }
    assert!(window.update(|window, _| retry_handle.is_focused(window)));
    let returned_retry = window.debug_bounds("awili-code-retry").unwrap();
    let viewport = window
        .debug_bounds("awili-code-transcript-viewport")
        .unwrap();
    assert!(
        returned_retry.left() >= viewport.left() && returned_retry.right() <= viewport.right()
            && returned_retry.top() >= viewport.top() && returned_retry.bottom() <= viewport.bottom(),
        "returning to the same keyboard target reveals its full face: {returned_retry:?} / {viewport:?}"
    );

    window.simulate_event(gpui::KeyDownEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
        is_held: false,
        prefer_character_input: false,
    });
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();

    entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        assert_eq!(tab.id, tab_id, "the owned Code tab stays active");
        assert!(tab.streaming, "Enter on Retry started the second turn");
        let retries = tab
            .convo
            .messages()
            .iter()
            .filter(|m| m.kind == awili_core::MessageKind::Retry)
            .collect::<Vec<_>>();
        assert_eq!(retries.len(), 1, "one new Retry row");
        assert_eq!(retries[0].role, crate::Role::User);
        assert_eq!(retries[0].text, "CODE_RETRY_PROMPT_UNIQUE");
    });
    assert!(
        window.debug_bounds("awili-code-retry").is_none(),
        "no Retry is offered while the turn streams"
    );
    let retry_argv = home.join("retry-argv-2");
    wait_until(&entity, window, "second CLI argument capture", |_| {
        retry_argv.exists()
    });
    let unsent = "CODE_RETRY_UNSENT_AFTER_ACTIVATION";
    window.simulate_input(unsent);
    window.run_until_parked();
    assert_eq!(
        entity.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        unsent,
        "keyboard Retry returns typing to the owned composer without test refocus"
    );
    let retry_argv = std::fs::read_to_string(&retry_argv).unwrap();
    assert!(retry_argv.contains("CODE_RETRY_PROMPT_UNIQUE"));
    assert!(
        !retry_argv.contains(unsent),
        "post-activation draft stays out of the already-started Retry request"
    );

    std::fs::write(&gate, "go").unwrap();
    wait_until(&entity, window, "retried code turn", |app| {
        !app.work.code_tabs[app.work.code_active].streaming
    });
    window.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "2",
        "the Retry ran a second fake claude turn"
    );
    entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        assert_eq!(tab.id, tab_id);
        let answers = tab
            .convo
            .messages()
            .iter()
            .filter(|m| m.role == crate::Role::Assistant && m.text == "CODE_RETRY_ANSWER")
            .count();
        assert_eq!(answers, 2, "the retried turn painted its own answer");
    });

    // A pointer Retry also hands typing back to the composer after the button
    // disappears, preserving the pre-button behavior for mouse users. Focus
    // first moves to the owned Retry handle, a non-composer control, so the
    // click's return to the composer is observable.
    composer_entity.update(window, |composer, cx| composer.set_text("", cx));
    transcript_scroll.scroll_to_bottom();
    window.update(|window, _| window.refresh());
    window.update(|window, cx| window.focus(&retry_handle, cx));
    window.run_until_parked();
    assert!(
        window.update(|window, _| retry_handle.is_focused(window)),
        "the owned Retry handle holds focus before the pointer click"
    );
    assert!(
        !window.update(|window, _| composer.is_focused(window)),
        "the composer is not focused before the pointer click"
    );
    let retry = window
        .debug_bounds("awili-code-retry")
        .expect("the Code Retry is painted for the pointer click");
    let viewport = window
        .debug_bounds("awili-code-transcript-viewport")
        .expect("the Code transcript viewport is painted");
    assert!(
        retry.left() >= viewport.left() && retry.right() <= viewport.right()
            && retry.top() >= viewport.top() && retry.bottom() <= viewport.bottom(),
        "the pointer target's complete Retry face lies inside the transcript: {retry:?} / {viewport:?}"
    );
    window.simulate_click(retry.center(), gpui::Modifiers::default());
    window.run_until_parked();
    assert!(
        window.update(|window, _| composer.is_focused(window)),
        "pointer Retry returns focus to the composer"
    );

    // The third fake turn is gated: observe exactly one new invocation and
    // Retry row while it runs, then type and prove the draft stays out of its
    // already-started request.
    let third_argv = home.join("retry-argv-3");
    wait_until(&entity, window, "third CLI argument capture", |_| {
        third_argv.exists()
    });
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "3",
        "the pointer Retry started exactly one third fake claude turn"
    );
    entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        assert_eq!(tab.id, tab_id, "the owned Code tab stays active");
        assert!(tab.streaming, "the pointer Retry is running");
        let retries = tab
            .convo
            .messages()
            .iter()
            .filter(|m| m.kind == awili_core::MessageKind::Retry)
            .count();
        assert_eq!(retries, 2, "one Retry row per keyboard and pointer Retry");
    });
    let pointer_unsent = "CODE_RETRY_POINTER_UNSENT";
    window.simulate_input(pointer_unsent);
    window.run_until_parked();
    assert_eq!(
        entity.read_with(window, |app, cx| app.composer.read(cx).text().to_string()),
        pointer_unsent,
        "pointer Retry preserves immediate composer typing"
    );
    let third_argv = std::fs::read_to_string(&third_argv).unwrap();
    assert!(third_argv.contains("CODE_RETRY_PROMPT_UNIQUE"));
    assert!(
        !third_argv.contains(pointer_unsent),
        "typing during the pointer Retry stays out of its already-started request"
    );

    std::fs::write(&third_gate, "go").unwrap();
    wait_until(&entity, window, "pointer retried code turn", |app| {
        !app.work.code_tabs[app.work.code_active].streaming
    });
    window.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "3",
        "the pointer Retry ran no more than its one third fake claude turn"
    );
    entity.read_with(window, |app, _| {
        let tab = &app.work.code_tabs[app.work.code_active];
        assert_eq!(tab.id, tab_id);
        let answers = tab
            .convo
            .messages()
            .iter()
            .filter(|m| m.role == crate::Role::Assistant && m.text == "CODE_RETRY_ANSWER")
            .count();
        assert_eq!(
            answers, 3,
            "the pointer retried turn painted its own answer"
        );
    });
}

#[gpui::test]
fn native_host_approval_buttons_answer_only_their_live_request(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("code_run::code_run_tests::native_host_approval_buttons_answer_only_their_live_request") else { return; };
    let (app, window, _) = code_app(cx, &home, SLEEPS);
    for (selector, choice) in [
        ("host-approve-once", awili_providers::ApprovalChoice::Once),
        ("host-approve-session", awili_providers::ApprovalChoice::Session),
        ("host-approve-deny", awili_providers::ApprovalChoice::Deny),
    ] {
        let (prompt, rx) = awili_providers::HostApproval::channel("Capture the whole screen?".into(), "desktop.screenshot · allScreens: true".into(), true);
        app.update(window, |app, cx| {
            let active = app.work.code_active;
            app.work.code_tabs[active].host_approval = Some(prompt.clone());
            cx.notify();
        });
        window.run_until_parked();
        let bounds = window.debug_bounds(selector).expect("visible native approval control");
        assert!(bounds.size.height >= crate::theme::target());
        click(window, selector);
        assert_eq!(rx.try_recv().unwrap(), choice);
        assert!(!prompt.active());
        assert!(window.debug_bounds("host-approve-once").is_none(), "resolved card disappears");
    }
}
