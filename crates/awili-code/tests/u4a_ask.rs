//! U4a proof tests: the read-only planning-step argv (enforced by the vendor
//! harness) and the plan-ready marker. See docs/daily-driver/specs/
//! U4-ask-first-enforced.md, part U4a.

use awili_code::{take_plan_marker, Autonomy, CodeEngine, PLAN_FIRST_SYSTEM, PLAN_READY_MARKER};

#[test]
fn u4a_claude_plan_step_is_plan_mode_with_every_tool() {
    // RC1-M: the planning step is read-only because the harness ENFORCES it:
    // Claude Code's own plan mode with no prompts. No tool allowlist and no
    // MCP cut (owner design principle, 2026-10-03): plan mode refuses
    // whatever changes things.
    let args = CodeEngine::Claude.build_args("fix the bug", Autonomy::AskFirst, None, None);
    assert_eq!(
        args,
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-mode",
            "plan",
            "--permission-prompts",
            "none",
            "fix the bug",
        ]
    );
    for gone in ["--strict-mcp-config", "--dangerously-skip-permissions"] {
        assert!(!args.iter().any(|a| a == gone), "{gone}: {args:?}");
    }
    assert!(!args.iter().any(|a| a.starts_with("--tools")), "{args:?}");
    assert_eq!(args.last().map(String::as_str), Some("fix the bug"));
}

#[test]
fn u4a_codex_plan_step_reads_the_owner_config_and_is_read_only() {
    // RC1-M: the owner's own Codex config (no `--ignore-user-config`) in the
    // read-only sandbox; the launch holds MCP tools to "writes" approval.
    let args = CodeEngine::Codex.build_args("fix the bug", Autonomy::AskFirst, None, None);
    assert_eq!(
        args,
        vec![
            "exec",
            // U4a-FIX1: permission flags precede any `resume` subcommand
            // (codex-cli 0.155 rejects `--sandbox` after `resume`).
            "--sandbox",
            "read-only",
            "--json",
            "--skip-git-repo-check",
            "fix the bug",
        ]
    );
    // Resume: the `exec` + permission flags + `resume t1` order still holds.
    let args = CodeEngine::Codex.build_args("fix the bug", Autonomy::AskFirst, Some("t1"), None);
    assert_eq!(
        args,
        vec![
            "exec",
            "--sandbox",
            "read-only",
            "resume",
            "t1",
            "--json",
            "--skip-git-repo-check",
            "fix the bug",
        ]
    );
    for gone in [
        "--dangerously-bypass-approvals-and-sandbox",
        "--ignore-user-config",
    ] {
        assert!(!args.iter().any(|a| a == gone), "{gone}: {args:?}");
    }
}

#[test]
fn u4a_enforces_ask_only_where_probed() {
    // Probes of record (2026-09-29): Claude and Codex enforce the planning
    // step; ZCode's plan mode let an MCP tool write a file; dsh was not
    // probed, so it is treated as not enforceable.
    assert!(CodeEngine::Claude.enforces_ask());
    assert!(CodeEngine::Codex.enforces_ask());
    assert!(!CodeEngine::Glm.enforces_ask());
    assert!(!CodeEngine::Dsh.enforces_ask());
}

#[test]
fn u4a_autonomous_argv_unchanged() {
    // D5: every turn that can change anything keeps its full-access argv
    // byte-for-byte (Autonomous is the approved follow-up's posture too).
    assert_eq!(
        CodeEngine::Claude.build_args("fix the bug", Autonomy::Autonomous, None, None),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--dangerously-skip-permissions",
            "fix the bug"
        ]
    );
    // D5 note for Codex: the full-access flag moves right after `exec` (before
    // `resume` on resume turns) — U4a-FIX1. The flag set is otherwise
    // byte-for-byte unchanged.
    assert_eq!(
        CodeEngine::Codex.build_args("fix the bug", Autonomy::Autonomous, None, None),
        vec![
            "exec",
            "--dangerously-bypass-approvals-and-sandbox",
            "--json",
            "--skip-git-repo-check",
            "fix the bug"
        ]
    );
    assert_eq!(
        CodeEngine::Glm.build_args("fix the bug", Autonomy::Autonomous, None, None),
        vec!["--prompt", "fix the bug", "--json", "--mode", "yolo"]
    );
    assert_eq!(
        CodeEngine::Dsh.build_args("fix the bug", Autonomy::Autonomous, None, None),
        vec!["--profile", "headless", "fix the bug"]
    );
}

#[test]
fn u4a_plan_marker_is_stripped_and_detected() {
    // The marker on its own line.
    let (clean, found) =
        take_plan_marker("Plan:\n1. edit src/a.rs\n2. run the tests\n[[awili:plan-ready]]");
    assert!(found);
    assert!(!clean.contains(PLAN_READY_MARKER));
    assert_eq!(clean, "Plan:\n1. edit src/a.rs\n2. run the tests");
    // Glued to the end of the preceding sentence, with no newline.
    let (clean, found) = take_plan_marker("All done.[[awili:plan-ready]]");
    assert!(found);
    assert_eq!(clean, "All done.");
    // CRLF line endings.
    let (clean, found) = take_plan_marker("Plan:\r\n1. edit\r\n[[awili:plan-ready]]\r\n");
    assert!(found);
    assert!(!clean.contains(PLAN_READY_MARKER));
    assert_eq!(clean, "Plan:\r\n1. edit");
    // Absent: unchanged except for a trailing-whitespace trim.
    let (clean, found) = take_plan_marker("Just an answer.\n   \n");
    assert!(!found);
    assert_eq!(clean, "Just an answer.");
    // Appearing twice.
    let (clean, found) =
        take_plan_marker("[[awili:plan-ready]]\nPlan:\n1. edit\n[[awili:plan-ready]]");
    assert!(found);
    assert!(!clean.contains(PLAN_READY_MARKER));
    assert_eq!(clean, "Plan:\n1. edit");
    // Glued to the end of the text with a trailing newline: the marker must
    // still be stripped and detected.
    let (clean, found) = take_plan_marker(&format!("Here is the plan.{PLAN_READY_MARKER}\n"));
    assert!(found);
    assert_eq!(clean, "Here is the plan.");
    let (clean, found) = take_plan_marker(&format!("Here is the plan.{PLAN_READY_MARKER}\n\n  "));
    assert!(found);
    assert_eq!(clean, "Here is the plan.");
    let (clean, found) = take_plan_marker(&format!("Here is the plan.{PLAN_READY_MARKER}\r\n"));
    assert!(found);
    assert_eq!(clean, "Here is the plan.");
}

#[test]
fn u4a_plan_first_system_names_the_marker() {
    assert!(
        PLAN_FIRST_SYSTEM.contains(PLAN_READY_MARKER),
        "PLAN_FIRST_SYSTEM must name the marker: {PLAN_FIRST_SYSTEM}"
    );
    assert!(!PLAN_FIRST_SYSTEM.contains("ADVISORY"));
}
