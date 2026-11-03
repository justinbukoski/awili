//! R2i proof tests: the activity lines a Code tab and a Work run show do not
//! warn "⚠ outside project" for the program an agent runs (RC1 round 2,
//! Minor 13: "⚠ outside project" next to "$ /bin/zsh -lc 'cat README.md'"),
//! while a real read outside the project is still marked. Drives the app's
//! own activity handlers on the real `AwiliApp` (private-HOME subprocess).

use std::path::PathBuf;

use awili_core::Role;
use gpui::AppContext;

use crate::AwiliApp;

const CHILD: &str = "AWILI_R2I_TEST_HOME";
const CONV: &str = "r2i-work-conv";
const SHELL_CAT: &str = "$ /bin/zsh -lc 'cat README.md'";
const OUTSIDE: &str = "$ /bin/zsh -lc 'cat /etc/hosts'";
const MARK: &str = "\u{26A0} outside project";

/// Run `test` in a subprocess with a private HOME. `Some(home)` inside the
/// child; `None` in the parent once the child passed.
fn isolated(test: &str) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os(CHILD) {
        return Some(home.into());
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    None
}

/// A Code tab on a project folder: the running command line from Codex is
/// not marked; a command that reads outside the project still is.
#[gpui::test]
fn r2i_code_activity_does_not_flag_the_shell(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("agent_brief::r2i_tests::r2i_code_activity_does_not_flag_the_shell")
    else {
        return;
    };
    let project = home.join("f1c");
    std::fs::create_dir_all(&project).unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = crate::work::Mode::Code;
        app.ensure_code_tab(cx);
        let tab = &mut app.work.code_tabs[app.work.code_active];
        tab.project_folder = Some(project.display().to_string());
        tab.convo.push(Role::Assistant, "");
        let last = tab.convo.messages().last().unwrap().clone();
        tab.rows.push(crate::MessageRow::new(&last));
        let (id, idx) = (tab.id, tab.rows.len() - 1);
        app.code_on_activity(id, idx, SHELL_CAT.into(), cx);
        app.code_on_activity(id, idx, OUTSIDE.into(), cx);
        let tab = &app.work.code_tabs[app.work.code_active];
        let lines: Vec<String> = tab.rows[idx]
            .activity
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(lines[0], SHELL_CAT, "the shell is not an outside read");
        assert!(lines[1].starts_with(MARK), "a real outside read: {lines:?}");
    });
}

/// The Work run's activity follows the same rule.
#[gpui::test]
fn r2i_work_activity_does_not_flag_the_shell(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("agent_brief::r2i_tests::r2i_work_activity_does_not_flag_the_shell")
    else {
        return;
    };
    let folder = home.join("w1");
    std::fs::create_dir_all(&folder).unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = crate::work::Mode::Work;
        app.work.folder = Some(folder.clone());
        let mut state = awili_core::ConversationState::new();
        state.push(Role::User, "Rename every .txt to .md");
        state.push(Role::Assistant, "");
        app.views
            .insert(CONV.into(), crate::conv_view::ConvView::from_state(state));
        app.set_active_conversation(CONV);
        app.work.conv_id = Some(CONV.into());
        let idx = app.views[CONV].rows.len() - 1;
        app.work.stream_row_idx = Some(idx);
        app.work_on_activity(SHELL_CAT.into(), cx);
        app.work_on_activity(OUTSIDE.into(), cx);
        let lines: Vec<String> = app.views[CONV].rows[idx]
            .activity
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(lines[0], SHELL_CAT, "the shell is not an outside read");
        assert!(lines[1].starts_with(MARK), "a real outside read: {lines:?}");
    });
}
