//! R2c / RC1-M proof tests: what a vendor agent CLI really receives from a
//! Work agent turn and from a Code tab turn. RC1-M ("no claude.md just
//! agents.md"): Awili's brief and the user-level AGENTS.md arrive once,
//! through `--append-system-prompt` (never `--system-prompt`), the prompt is
//! exactly the user's words, every CLAUDE.md is excluded, and the owner's own
//! settings stay in force (no `--setting-sources`, no `--strict-mcp-config`,
//! no safe mode). The real `AwiliApp` runs in a private-HOME subprocess and a
//! fake `claude` records its argv.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use awili_code::CodeEngine;
use gpui::AppContext;

use super::{FOLDER_LINE, NO_INSTRUCTION_TEXT};
use crate::work::{Mode, WorkExecutor};
use crate::AwiliApp;

const CHILD: &str = "AWILI_R2C_TEST_HOME";
const CONV: &str = "r2c-work-conv";
const ASK: &str = "What instructions were you given about memory or boot gates?";

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

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn recording_claude(dir: &Path) -> PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` in `dir`: writes its argv (each argument followed by a
/// NUL, so multi-line values survive) to `argv`, answers one Claude
/// stream-json turn, exits 0.
fn recording_claude(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    let script = format!(
        "#!/bin/sh\n\
         cat > {argv}.stdin\n\
         prev=\"\"; for a in \"$@\"; do if [ \"$prev\" = --append-system-prompt-file ]; then printf '%s\\0' \"$(cat \"$a\")\"; elif [ \"$a\" = --append-system-prompt-file ]; then printf '%s\\0' --append-system-prompt; else printf '%s\\0' \"$a\"; fi; prev=\"$a\"; done > {argv}.part && mv {argv}.part {argv}\n\
         printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"ok\"}}]}}}}'\n\
         printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\"}}'\n",
        argv = dir.join("argv").display(),
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Pump the app until the fake CLI wrote its argv; return it.
fn argv_seen(cx: &mut gpui::TestAppContext, dir: &Path) -> Vec<String> {
    let file = dir.join("argv");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if let Ok(p) = std::fs::read_to_string(&file) {
            if !p.is_empty() {
                let argv: Vec<String> = p
                    .split('\0')
                    .filter(|a| !a.is_empty())
                    .map(str::to_string)
                    .collect();
                let stdin = std::fs::read_to_string(dir.join("argv.stdin")).unwrap_or_default();
                return crate::test_stdin::argv_with_prompt(argv, &stdin);
            }
        }
        assert!(Instant::now() < deadline, "the agent CLI never ran");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A user-level AGENTS.md (configured by absolute path), and a folder with a
/// project AGENTS.md and a project CLAUDE.md.
fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let user = root.join("user-AGENTS.md");
    std::fs::write(&user, "USER-AGENTS-RULE: answer like the owner.\n").unwrap();
    let folder = root.join("proj");
    std::fs::create_dir_all(folder.join(".git")).unwrap();
    std::fs::write(folder.join("AGENTS.md"), "PROJECT-AGENTS-FACT\n").unwrap();
    std::fs::write(folder.join("CLAUDE.md"), "PROJECT-CLAUDE-RULE\n").unwrap();
    (user, folder)
}

/// The value after `flag` (the only one), or a failure naming the argv.
fn value_of<'a>(argv: &'a [String], flag: &str) -> &'a str {
    let at: Vec<usize> = argv
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == flag)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(at.len(), 1, "exactly one {flag}: {argv:?}");
    &argv[at[0] + 1]
}

/// What every Claude Work/Code launch must carry (RC1-M).
fn assert_agents_md_launch(argv: &[String]) {
    let prompt = argv.last().expect("a prompt");
    assert!(
        prompt.ends_with(ASK) && !prompt.contains("How to answer in Awili"),
        "the prompt carries the user's words and none of Awili's own text: {argv:?}"
    );
    let appended = value_of(argv, "--append-system-prompt");
    assert!(appended.contains("How to answer in Awili"), "{appended}");
    assert!(appended.contains(FOLDER_LINE), "{appended}");
    assert!(appended.contains(NO_INSTRUCTION_TEXT), "{appended}");
    assert_eq!(
        appended.matches("USER-AGENTS-RULE").count(),
        1,
        "{appended}"
    );
    let settings: serde_json::Value =
        serde_json::from_str(value_of(argv, "--settings")).expect("inline settings JSON");
    let excludes = settings["claudeMdExcludes"]
        .as_array()
        .expect("claudeMdExcludes");
    assert!(excludes.iter().any(|e| e == "**/CLAUDE.md"), "{settings}");
    for gone in [
        "--system-prompt",
        "--setting-sources",
        "--strict-mcp-config",
        "--safe-mode",
    ] {
        assert!(!argv.iter().any(|a| a == gone), "{gone}: {argv:?}");
    }
    let all = argv.join("\n");
    for absent in ["PROJECT-CLAUDE-RULE", "PROJECT-AGENTS-FACT"] {
        // The project AGENTS.md is read by Claude Code itself, never copied.
        assert!(!all.contains(absent), "{absent}: {argv:?}");
    }
    // RC1-M-FIX6: the planning step's text, when the turn is one, rides in
    // this turn's prompt (Claude keeps appended system text across
    // `--resume`), never in the appended system text.
    assert!(
        !appended.contains(awili_code::PLAN_FIRST_SYSTEM),
        "{appended}"
    );
    assert!(
        !prompt.contains("[System context]")
            || prompt.starts_with(&format!(
                // RC2-R4 (R3-F9b, R5-6): then Claude's plan-in-the-reply
                // line (deliberate test change).
                "[System context]\n{}\n{}\n\n",
                awili_code::PLAN_FIRST_SYSTEM,
                crate::agent_launch::CLAUDE_PLAN_IN_REPLY
            )),
        "only the planning text rides in the prompt: {prompt}"
    );
}

/// A Work agent turn (Claude).
#[gpui::test]
fn r2c_work_agent_launch_is_agents_md_only(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("agent_brief::r2c_tests::r2c_work_agent_launch_is_agents_md_only")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (user, folder) = fixture(dir.path());
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.agents.claude_bin = Some(recording_claude(dir.path()).display().to_string());
        app.work.agents.agents_md = Some(user.display().to_string());
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        app.work.folder = Some(folder.clone());
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run(ASK.into(), cx);
    });
    assert_agents_md_launch(&argv_seen(cx, dir.path()));
}

/// A Code tab turn (Claude).
#[gpui::test]
fn r2c_code_tab_launch_is_agents_md_only(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated("agent_brief::r2c_tests::r2c_code_tab_launch_is_agents_md_only")
    else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (user, _folder) = fixture(dir.path());
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.memory.enabled = false;
        app.mode = Mode::Code;
        app.ensure_code_tab(cx);
        app.work.agents.claude_bin = Some(recording_claude(dir.path()).display().to_string());
        app.work.agents.agents_md = Some(user.display().to_string());
        let tab = &mut app.work.code_tabs[app.work.code_active];
        tab.engine = CodeEngine::Claude;
        tab.model = None;
        app.test_choose_native_target(CodeEngine::Claude, None, cx);
        app.submit_code_turn(ASK.into(), cx);
    });
    assert_agents_md_launch(&argv_seen(cx, dir.path()));
}
