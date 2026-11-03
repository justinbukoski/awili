//! R1h proof: the real `AwiliApp` (built by `AwiliApp::new` in a subprocess
//! with a private HOME) in a test window, over a fake Claude CLI: a planning
//! step, then the approved run renames the folder's files. The Files card
//! must list what is on disk after the run, and the rendered status must say
//! the plan waits for an OK (not "completed") while the card is open.

use std::path::{Path, PathBuf};

use awili_code::CodeEngine;
use awili_gateway::run_events::ApprovalDecision;
use gpui::{px, size};

use super::*;
use crate::terminal_view::TERMINAL_STATUS_SELECTOR;
use crate::AwiliApp;

const CHILD: &str = "AWILI_R1H_TEST_HOME";
const CONV: &str = "r1h-conv";

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

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn renaming_claude(home: &Path, folder: &Path) -> String {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude`: run 1 answers with a plan (marker), run 2 renames every
/// `.txt` in `folder` to `.md` and answers "Done.".
fn renaming_claude(home: &Path, folder: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let script = r#"#!/bin/sh
n=$(cat "FAKE/count" 2>/dev/null || echo 0)
n=$((n+1))
echo "$n" > "FAKE/count"
printf '%s\n' '{"type":"system","subtype":"init","session_id":"s-r1h"}'
if [ "$n" = 1 ]; then
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"1. rename the notes\n[[awili:plan-ready]]"}]}}'
else
for f in "FOLDER"/*.txt; do mv "$f" "${f%.txt}.md"; done
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}'
fi
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"ok","usage":{"input_tokens":1,"output_tokens":1}}'
"#;
    let dir = home.join("fake");
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("claude");
    let script = script
        .replace("FAKE", &dir.display().to_string())
        .replace("FOLDER", &folder.display().to_string());
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

/// The folder's top-level names on disk, sorted.
fn on_disk(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// The Files card's tree (top level), sorted.
fn tree_names(app: &AwiliApp) -> Vec<String> {
    let mut names: Vec<String> = app.work.file_tree.iter().map(|n| n.name.clone()).collect();
    names.sort();
    names
}

/// Major 10: plan, approve, the approved run renames the files. Before the
/// approval the rendered status says "waiting for your OK" (never
/// "completed") and the terminal reads "open"; after the approved run the
/// Files card (flat list and tree) lists exactly what is on disk.
#[gpui::test]
fn r1h_files_card_matches_disk_after_the_approved_run(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::r1h_tests::r1h_files_card_matches_disk_after_the_approved_run")
    else {
        return;
    };
    let store = awili_persist::Store::open_default().unwrap();
    store.create_conversation(CONV, "notes", 0).unwrap();
    cx.executor().allow_parking();
    let folder = home.join("b2");
    std::fs::create_dir_all(&folder).unwrap();
    for name in ["meeting.txt", "shopping.txt", "trip.txt"] {
        std::fs::write(folder.join(name), "note\n").unwrap();
    }
    let bin = renaming_claude(&home, &folder);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    view.update(window, |app, cx| {
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
        app.mode = Mode::Work;
        app.shell.job_open = true;
        // The thread's next turn is Claude, chosen the way the chooser does;
        // the route, not `work.executor`, decides what a send runs on.
        assert!(app.set_composer_target(
            &crate::workshop_composer::ModelOwner::Work(CONV.into()),
            WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            },
            cx,
        ));
        app.work.agents.claude_bin = Some(bin);
        app.set_work_folder(folder.clone(), cx);
        assert_eq!(app.work.folder_files, on_disk(&folder));
        app.submit_work_run("Rename every .txt to .md".into(), cx);
    });
    wait_idle(window, &view);
    assert!(view.read_with(window, |app, _| app.plan_gate_pending_here()));
    assert!(
        window
            .debug_bounds("r1h-work-status-waiting for your OK")
            .is_some(),
        "a plan waiting for approval reads as waiting"
    );
    assert!(
        window.debug_bounds("r1h-work-status-completed").is_none(),
        "a plan waiting for approval is not completed"
    );
    let terminal = window.debug_bounds("ui-tab-job-tab-1").unwrap().center();
    window.simulate_click(terminal, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(
        window.debug_bounds("r1h-terminal-open").is_some()
            && window.debug_bounds("r1h-terminal-running").is_none(),
        "the terminal reads open, not running"
    );
    assert_eq!(
        format!("{TERMINAL_STATUS_SELECTOR}open"),
        "r1h-terminal-open"
    );
    assert_eq!(
        format!(
            "{}{}",
            AwiliApp::WORK_STATUS_SELECTOR,
            AwiliApp::PLAN_WAITING_LABEL
        ),
        "r1h-work-status-waiting for your OK"
    );

    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Approve, cx);
        assert!(app.work.running, "the approved run starts");
    });
    wait_idle(window, &view);
    let disk = on_disk(&folder);
    assert_eq!(disk, ["meeting.md", "shopping.md", "trip.md"]);
    view.read_with(window, |app, _| {
        assert_eq!(
            app.work.folder_files, disk,
            "the flat Files list is the disk"
        );
        assert_eq!(tree_names(app), disk, "the Files tree is the disk");
    });
    let history = window.debug_bounds("ui-tab-job-tab-0").unwrap().center();
    window.simulate_click(history, gpui::Modifiers::default());
    window.run_until_parked();
    assert!(
        window.debug_bounds("r1h-work-status-completed").is_some(),
        "the approved run reads completed"
    );
}
