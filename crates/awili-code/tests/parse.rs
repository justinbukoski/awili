//! Fixture tests for the two engine parsers, against streams captured verbatim
//! from the real `claude` and `codex` CLIs (2026-06-24). These pin the wire
//! shapes the parsers depend on — if a CLI changes its event format, the
//! captured fixture stops matching and these fail loudly rather than the live
//! Code session silently rendering nothing.

use awili_code::{Autonomy, CodeEngine, CodeEvent};
use awili_providers::ProviderEvent;

/// Decode every line of a fixture into a flat event list.
fn decode(engine: CodeEngine, fixture: &str) -> Vec<CodeEvent> {
    fixture.lines().flat_map(|l| engine.parse_line(l)).collect()
}

const CLAUDE: &str = include_str!("fixtures/claude_stream.jsonl");
const CODEX: &str = include_str!("fixtures/codex_stream.jsonl");

#[test]
fn claude_stream_normalises_to_expected_events() {
    let events = decode(CodeEngine::Claude, CLAUDE);
    assert_eq!(
        events,
        vec![
            CodeEvent::SessionStarted {
                session_id: "d0fa077a".to_string()
            },
            CodeEvent::AssistantText("On it — running the tests.".to_string()),
            CodeEvent::ToolStarted {
                label: "Bash: cargo test -p awili-code".to_string()
            },
            CodeEvent::ToolFinished {
                label: "done".to_string(),
                ok: true
            },
            CodeEvent::AssistantText("All green.".to_string()),
            CodeEvent::Usage {
                cached_input_tokens: None,
                input_tokens: 4530,
                output_tokens: 16
            },
            CodeEvent::TurnComplete,
        ]
    );
}

#[test]
fn claude_bash_label_is_first_line_only() {
    // The tool_use input had a two-line command; the label must collapse to the
    // first line so the transcript bullet stays one line.
    let events = decode(CodeEngine::Claude, CLAUDE);
    let label = events.iter().find_map(|e| match e {
        CodeEvent::ToolStarted { label } => Some(label.clone()),
        _ => None,
    });
    assert_eq!(label.as_deref(), Some("Bash: cargo test -p awili-code"));
}

#[test]
fn claude_hook_and_rate_limit_lines_are_ignored() {
    // The hook_started system line and the rate_limit_event must produce nothing.
    assert!(CodeEngine::Claude
        .parse_line(r#"{"type":"system","subtype":"hook_started","hook_id":"h1"}"#)
        .is_empty());
    assert!(CodeEngine::Claude
        .parse_line(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#)
        .is_empty());
}

#[test]
fn claude_result_usage_folds_cache_tokens() {
    let events = CodeEngine::Claude.parse_line(
        r#"{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":10,"output_tokens":7,"cache_read_input_tokens":1000,"cache_creation_input_tokens":50}}"#,
    );
    let usages: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, CodeEvent::Usage { .. }))
        .collect();
    assert_eq!(usages.len(), 1);
    assert_eq!(
        usages[0],
        &CodeEvent::Usage {
            cached_input_tokens: Some(1000),
            input_tokens: 1060,
            output_tokens: 7
        }
    );
}

#[test]
fn claude_result_usage_without_cache_fields_is_unchanged() {
    let events = CodeEngine::Claude.parse_line(
        r#"{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":10,"output_tokens":7}}"#,
    );
    let usages: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, CodeEvent::Usage { .. }))
        .collect();
    assert_eq!(usages.len(), 1);
    assert_eq!(
        usages[0],
        &CodeEvent::Usage {
            cached_input_tokens: None,
            input_tokens: 10,
            output_tokens: 7
        }
    );
}

#[test]
fn claude_result_usage_cache_fields_saturate() {
    let events = CodeEngine::Claude.parse_line(
        r#"{"type":"result","subtype":"success","is_error":false,"usage":{"input_tokens":18446744073709551615,"output_tokens":1,"cache_read_input_tokens":5}}"#,
    );
    let usages: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, CodeEvent::Usage { .. }))
        .collect();
    assert_eq!(usages.len(), 1);
    assert_eq!(
        usages[0],
        &CodeEvent::Usage {
            cached_input_tokens: Some(5),
            input_tokens: u64::MAX,
            output_tokens: 1
        }
    );
    // The provider mapping clamps the saturated count to u32::MAX.
    let ev = CodeEvent::Usage {
        cached_input_tokens: None,
        input_tokens: u64::MAX,
        output_tokens: 1,
    };
    assert_eq!(
        ev.to_provider_event(),
        Some(ProviderEvent::Usage(awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: u32::MAX,
            output_tokens: 1
        }))
    );
}

#[test]
fn codex_turn_completed_usage_is_unchanged() {
    let events = CodeEngine::Codex
        .parse_line(r#"{"type":"turn.completed","usage":{"input_tokens":3,"output_tokens":4}}"#);
    let usages: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, CodeEvent::Usage { .. }))
        .collect();
    assert_eq!(usages.len(), 1);
    assert_eq!(
        usages[0],
        &CodeEvent::Usage {
            cached_input_tokens: None,
            input_tokens: 3,
            output_tokens: 4
        }
    );
}

#[test]
fn claude_result_error_subtype_is_fatal() {
    let events = CodeEngine::Claude.parse_line(
        r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"hit the turn limit"}"#,
    );
    assert_eq!(
        events,
        vec![CodeEvent::Error("hit the turn limit".to_string())]
    );
}

#[test]
fn claude_failed_tool_attempt_is_not_a_terminal_error() {
    let events = CodeEngine::Claude.parse_line(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"Claude requested permissions to read this file, but you haven't granted it yet."}]}}"#,
    );
    assert_eq!(
        events,
        vec![CodeEvent::ToolFinished {
            label: "done".into(),
            ok: false
        },]
    );
}

#[test]
fn claude_structured_tool_failure_and_result_denial_fail_closed() {
    let tool_events = CodeEngine::Claude.parse_line(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":[{"type":"text","text":"custom denial"}]}]}}"#,
    );
    assert!(matches!(
        tool_events.as_slice(),
        [CodeEvent::ToolFinished { ok: false, .. }]
    ));

    let result_events = CodeEngine::Claude.parse_line(
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"permission_denials":[{"tool":"Bash"}]}"#,
    );
    assert_eq!(
        result_events,
        vec![CodeEvent::Error(
            "Blocked by Claude CLI permission policy: permission denied for Bash. The terminal result did not confirm success.".into()
        )]
    );
}

#[test]
fn codex_stream_normalises_to_expected_events() {
    let events = decode(CodeEngine::Codex, CODEX);
    assert_eq!(
        events,
        vec![
            CodeEvent::SessionStarted {
                session_id: "019efa68-28dc-76f0".to_string()
            },
            CodeEvent::Reasoning("The user wants the tests run.".to_string()),
            CodeEvent::ToolFinished {
                label: "$ cargo test -p awili-code".to_string(),
                ok: true
            },
            // RC2-R2J: a change is named as it starts, then as done.
            CodeEvent::ToolStarted {
                label: "edit src/lib.rs".to_string()
            },
            CodeEvent::ToolFinished {
                label: "edit src/lib.rs".to_string(),
                ok: true
            },
            CodeEvent::AssistantText("Done — all tests pass.".to_string()),
            CodeEvent::Usage {
                cached_input_tokens: Some(10624),
                input_tokens: 12197,
                output_tokens: 5
            },
            CodeEvent::TurnComplete,
        ]
    );
}

#[test]
fn codex_nonzero_exit_marks_tool_failed() {
    let events = CodeEngine::Codex.parse_line(
        r#"{"type":"item.completed","item":{"type":"command_execution","command":"cargo test","exit_code":101}}"#,
    );
    assert_eq!(
        events,
        vec![CodeEvent::ToolFinished {
            label: "$ cargo test".to_string(),
            ok: false
        }]
    );
}

#[test]
fn codex_turn_failed_is_an_error() {
    let events = CodeEngine::Codex
        .parse_line(r#"{"type":"turn.failed","error":{"message":"sandbox denied"}}"#);
    assert_eq!(events, vec![CodeEvent::Error("sandbox denied".to_string())]);
}

#[test]
fn malformed_or_unknown_lines_never_panic() {
    for engine in [CodeEngine::Claude, CodeEngine::Codex] {
        assert!(engine.parse_line("not json at all").is_empty());
        assert!(engine.parse_line("").is_empty());
        assert!(engine.parse_line("   ").is_empty());
        assert!(engine.parse_line(r#"{"type":"something_new"}"#).is_empty());
        assert!(engine.parse_line("{}").is_empty());
    }
}

#[test]
fn provider_mapping_flattens_to_chat_stream() {
    // The end-to-end shape the native transcript consumes: assistant text as
    // tokens, tool activity as readable bullet lines, usage + a terminal Done.
    let provider: Vec<ProviderEvent> = decode(CodeEngine::Claude, CLAUDE)
        .iter()
        .filter_map(CodeEvent::to_provider_event)
        .collect();
    assert_eq!(
        provider,
        vec![
            ProviderEvent::Token("On it — running the tests.".to_string()),
            ProviderEvent::ToolStarted("Bash: cargo test -p awili-code".to_string()),
            ProviderEvent::Activity("✓ done".to_string()),
            ProviderEvent::Token("All green.".to_string()),
            ProviderEvent::Usage(awili_providers::Usage {
                cached_input_tokens: None,
                input_tokens: 4530,
                output_tokens: 16
            }),
            ProviderEvent::Done,
        ]
    );
}

#[test]
fn session_started_and_reasoning_carry_no_provider_event() {
    assert_eq!(
        CodeEvent::SessionStarted {
            session_id: "x".into()
        }
        .to_provider_event(),
        None
    );
    assert_eq!(
        CodeEvent::Reasoning("thinking".into()).to_provider_event(),
        None
    );
}

#[test]
fn failed_tool_error_and_oversized_usage_map_correctly() {
    // The captured fixtures only exercise ok=true tool finishes, a clean small
    // usage frame, and no engine Error — so three `to_provider_event` arms carry
    // untested user-facing behaviour. Each is a distinct silent-regression path:
    //  - a FAILED tool must render the ✗ mark, never the ✓ of the success arm
    //    (the fixture's `provider_mapping_flattens_to_chat_stream` only pins ✓);
    //  - an engine Error must surface as ProviderEvent::Error, not vanish;
    //  - an oversized token count must SATURATE to u32::MAX, not wrap to a tiny
    //    number and silently under-report cost (mirrors the awili-providers M5
    //    clamp — 5_000_000_000 as u32 would wrap to ~705M).
    assert_eq!(
        CodeEvent::ToolFinished {
            label: "cargo test".into(),
            ok: false
        }
        .to_provider_event(),
        Some(ProviderEvent::ToolAttemptFailed)
    );
    assert_eq!(
        CodeEvent::Error("sandbox denied".into()).to_provider_event(),
        Some(ProviderEvent::Error("sandbox denied".to_string()))
    );
    assert_eq!(
        CodeEvent::Usage {
            cached_input_tokens: None,
            input_tokens: 5_000_000_000,
            output_tokens: u64::MAX
        }
        .to_provider_event(),
        Some(ProviderEvent::Usage(awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: u32::MAX,
            output_tokens: u32::MAX,
        }))
    );
}

#[test]
fn build_args_first_turn_per_engine() {
    assert_eq!(CodeEngine::Claude.bin(), "claude");
    assert_eq!(CodeEngine::Codex.bin(), "codex");
    // First turn (no resume id), AskFirst = the read-only planning step
    // (U4): the harness flags make the step read-only; prompt is the trailing
    // arg.
    assert_eq!(
        CodeEngine::Claude.build_args("fix the bug", Autonomy::AskFirst, None, None),
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
            "fix the bug"
        ]
    );
    assert_eq!(
        CodeEngine::Codex.build_args("fix the bug", Autonomy::AskFirst, None, None),
        vec![
            "exec",
            // U4a-FIX1: permission flags precede any `resume` subcommand
            // (codex-cli 0.155 rejects `--sandbox` after `resume`).
            "--sandbox",
            "read-only",
            "--json",
            "--skip-git-repo-check",
            "fix the bug"
        ]
    );
}

#[test]
fn permission_args_never_sandbox_and_keep_network() {
    // Owner 2026-06-24: NEVER sandbox — every turn that can change anything
    // must reach memory/web. Autonomous is asserted exactly as before (the
    // never-sandbox rule is unchanged).
    assert_eq!(
        CodeEngine::Claude.permission_args(Autonomy::Autonomous),
        vec!["--dangerously-skip-permissions"]
    );
    // U4: AskFirst is the read-only planning step — the vendor harness holds it
    // read-only (the one D5 exception to the never-sandbox rule).
    assert_eq!(
        CodeEngine::Claude.permission_args(Autonomy::AskFirst),
        vec!["--permission-mode", "plan", "--permission-prompts", "none"]
    );
    assert_eq!(
        CodeEngine::Codex.permission_args(Autonomy::Autonomous),
        vec!["--dangerously-bypass-approvals-and-sandbox"]
    );
    assert_eq!(
        CodeEngine::Codex.permission_args(Autonomy::AskFirst),
        vec!["--sandbox", "read-only"]
    );
    // Regression guard: the never-sandbox rule holds for every posture that can
    // change anything; AskFirst is the read-only planning step, U4 D5.
    for engine in [CodeEngine::Claude, CodeEngine::Codex] {
        let args = engine.permission_args(Autonomy::Autonomous);
        assert!(
            !args
                .iter()
                .any(|f| f == "workspace-write" || f == "read-only"),
            "{engine:?} must not sandbox the network"
        );
    }
}

#[test]
fn plan_first_only_for_ask_first() {
    assert!(Autonomy::AskFirst.plan_first());
    assert!(!Autonomy::Autonomous.plan_first());
    assert!(!awili_code::PLAN_FIRST_SYSTEM.is_empty());
}

#[test]
fn default_autonomy_is_autonomous() {
    // The owner's app on the owner's fleet: full access, no friction by default.
    assert_eq!(Autonomy::default(), Autonomy::Autonomous);
}

#[test]
fn claude_tool_labels_are_tailored_per_tool_with_name_fallback() {
    // The tailored per-tool labels (`claude_tool_label`) are the whole reason Code
    // mode renders a legible bullet instead of dumping raw terminal output: Read/
    // Edit/Write surface their file path, Grep/Glob their pattern. The captured
    // fixture only exercises a `Bash` tool_use, so every non-Bash arm — the
    // file_path extraction, the pattern extraction, the unknown-tool fallback, and
    // a known tool missing its signature field — was unpinned. A refactor dropping
    // the tailored arms would silently degrade every Read/Edit/Grep bullet to a
    // bare tool name (or, without the field guards, panic) with no test failing.
    let label = |name: &str, input: &str| -> String {
        let line = format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"{name}","input":{input}}}]}}}}"#
        );
        match CodeEngine::Claude.parse_line(&line).into_iter().next() {
            Some(CodeEvent::ToolStarted { label }) => label,
            other => panic!("expected exactly one ToolStarted for {name}, got {other:?}"),
        }
    };

    // Read/Edit/Write surface the file path.
    assert_eq!(
        label("Read", r#"{"file_path":"src/main.rs"}"#),
        "Read src/main.rs"
    );
    assert_eq!(
        label("Edit", r#"{"file_path":"src/lib.rs"}"#),
        "Edit src/lib.rs"
    );
    assert_eq!(
        label("Write", r#"{"file_path":"notes.md"}"#),
        "Write notes.md"
    );
    // Grep/Glob surface the search pattern.
    assert_eq!(label("Grep", r#"{"pattern":"TODO"}"#), "Grep TODO");
    assert_eq!(label("Glob", r#"{"pattern":"**/*.rs"}"#), "Glob **/*.rs");
    // An unknown tool shows its bare name — still far more legible than raw output.
    assert_eq!(
        label("WebFetch", r#"{"url":"https://example.com"}"#),
        "WebFetch"
    );
    // A known tool missing its signature field falls back to the bare name, never
    // panicking on the absent key.
    assert_eq!(label("Read", r#"{}"#), "Read");
    assert_eq!(label("Grep", r#"{"path":"src"}"#), "Grep");
}

#[test]
fn build_args_resume_turn_places_id_correctly_per_engine() {
    // Claude: --resume is a plain option among the flags. Autonomous posture.
    assert_eq!(
        CodeEngine::Claude.build_args("more", Autonomy::Autonomous, Some("sess-1"), None),
        vec![
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--resume",
            "sess-1",
            "--dangerously-skip-permissions",
            "more"
        ]
    );
    // Codex: resume is the `exec resume <id>` subcommand form. The permission
    // flag precedes `resume` (U4a-FIX1: codex-cli 0.155 rejects `--sandbox`
    // after `resume`), then the remaining flags and prompt follow.
    assert_eq!(
        CodeEngine::Codex.build_args("more", Autonomy::Autonomous, Some("thr-9"), None),
        vec![
            "exec",
            "--dangerously-bypass-approvals-and-sandbox",
            "resume",
            "thr-9",
            "--json",
            "--skip-git-repo-check",
            "more"
        ]
    );
}

#[test]
fn long_command_label_is_char_safely_truncated_at_the_cap() {
    // `first_line` caps every command/Bash label at 80 chars with a `…` so one
    // over-long single-line command can't blow out a transcript bullet — and it
    // does so CHAR-safely (`chars().take(79)`, never a byte slice), so a multibyte
    // command (a Japanese path/prompt) truncates instead of panicking mid-char.
    // Every other label test uses a short command (`cargo test -p awili-code`, 24
    // chars < cap), so both the cap threshold AND its Unicode-safety are unpinned:
    // dropping the truncation, weakening `> 80`, or swapping `chars().take(79)` for
    // `&line[..79]` all survive the existing suite. We drive it through the public
    // `parse_line` on a Codex `command_execution`, whose label is `$ <first_line>`.
    let cmd_label = |cmd: &str| -> String {
        let line = format!(
            r#"{{"type":"item.completed","item":{{"type":"command_execution","command":"{cmd}","exit_code":0}}}}"#
        );
        match CodeEngine::Codex.parse_line(&line).as_slice() {
            [CodeEvent::ToolFinished { label, ok: true }] => label.clone(),
            other => panic!("expected one ok ToolFinished, got {other:?}"),
        }
    };

    // Over-cap ASCII single line: truncated to the first 79 chars + `…` (80 chars),
    // prefixed by the `$ ` marker → 82 chars total. Dropping the truncation (or a
    // `> 800` threshold) leaves the full 95-char command and fails this exactly.
    let ascii = format!("echo {}", "x".repeat(90));
    let expected_ascii = format!("$ {}…", ascii.chars().take(79).collect::<String>());
    assert_eq!(cmd_label(&ascii), expected_ascii);
    assert_eq!(cmd_label(&ascii).chars().count(), 82);
    assert!(cmd_label(&ascii).ends_with('…'));

    // Over-cap MULTIBYTE line: char 79 lands inside a 3-byte `あ`. The char-based
    // truncation yields `echo ` + 74×`あ` + `…`; a `&line[..79]` byte slice would
    // panic on the char boundary. Reaching this assertion at all proves no panic.
    let multibyte = format!("echo {}", "あ".repeat(90));
    let expected_mb = format!("$ {}…", multibyte.chars().take(79).collect::<String>());
    assert_eq!(cmd_label(&multibyte), expected_mb);
    assert_eq!(cmd_label(&multibyte).chars().count(), 82);

    // Boundary: a line of EXACTLY 80 chars is passed through whole (no `…`); 81
    // truncates. Pins `> 80` precisely — `>= 80` would clip the at-cap case.
    let at_cap = "x".repeat(80);
    assert_eq!(cmd_label(&at_cap), format!("$ {at_cap}"));
    assert!(!cmd_label(&at_cap).ends_with('…'));
    let over_cap = "x".repeat(81);
    assert!(cmd_label(&over_cap).ends_with('…'));
    assert_eq!(cmd_label(&over_cap).chars().count(), 82);
}

// ---- GLM / ZCode (executor-selection design of record, 2026-08-22 + 23) ----

#[test]
fn glm_args_first_turn_and_resume() {
    assert_eq!(
        CodeEngine::Glm.build_args("fix the bug", Autonomy::Autonomous, None, None),
        ["--prompt", "fix the bug", "--json", "--mode", "yolo"]
    );
    // U4: the GLM AskFirst posture is `--mode plan` — a fallback only, since
    // the app never dispatches a planning step to an engine where
    // `enforces_ask` is false.
    assert_eq!(
        CodeEngine::Glm.build_args("more", Autonomy::AskFirst, Some("sess_abc"), None),
        ["--prompt", "more", "--json", "--resume", "sess_abc", "--mode", "plan"]
    );
}

#[test]
fn model_flag_lands_per_engine() {
    let c = CodeEngine::Claude.build_args("p", Autonomy::Autonomous, None, Some("fable"));
    assert!(c.windows(2).any(|w| w == ["--model", "fable"]));
    let x = CodeEngine::Codex.build_args("p", Autonomy::Autonomous, None, Some("gpt-5.6-sol"));
    assert!(x.windows(2).any(|w| w == ["-m", "gpt-5.6-sol"]));
    // GLM has no model CLI flag — the driver handles it via a settings overlay.
    let g = CodeEngine::Glm.build_args("p", Autonomy::Autonomous, None, Some("glm-5.1"));
    assert!(!g.iter().any(|a| a.contains("glm-5.1")));
}

#[test]
fn glm_parse_whole_real_envelope() {
    // Captured live from `zcode --prompt … --json`, 2026-08-23.
    let out = r#"{
  "sessionId": "sess_3cc80215-9137-4263-895b-6e68915bfd93",
  "traceId": "763a9525-7f21-4663-9f9c-f021c2372708",
  "turnId": "turn_94811d35-5fc0-4209-9191-3aefbcb36575",
  "response": "OK",
  "usage": {
    "source": "provider",
    "modelRequestCount": 1,
    "inputTokens": 27340,
    "outputTokens": 3,
    "totalTokens": 27343
  },
  "eventCount": 11,
  "projection": { "status": "idle", "turnCount": 1 }
}"#;
    let events = CodeEngine::Glm.parse_whole(out);
    assert_eq!(
        events,
        vec![
            CodeEvent::SessionStarted {
                session_id: "sess_3cc80215-9137-4263-895b-6e68915bfd93".into()
            },
            CodeEvent::AssistantText("OK".into()),
            CodeEvent::Usage {
                cached_input_tokens: None,
                input_tokens: 27340,
                output_tokens: 3
            },
            CodeEvent::TurnComplete,
        ]
    );
}

#[test]
fn glm_parse_whole_skips_leading_noise_and_fails_closed() {
    let noisy = "some electron warning\n{\"sessionId\":\"sess_1\",\"response\":\"hi\"}";
    let events = CodeEngine::Glm.parse_whole(noisy);
    assert!(
        matches!(&events[0], CodeEvent::SessionStarted { session_id } if session_id == "sess_1")
    );
    assert!(events.contains(&CodeEvent::AssistantText("hi".into())));
    // Garbage output surfaces as an Error event, never silence.
    let bad = CodeEngine::Glm.parse_whole("segfault lol");
    assert!(matches!(&bad[0], CodeEvent::Error(e) if e.contains("no parseable result")));
}

#[test]
fn glm_per_line_parsing_is_inert() {
    assert!(CodeEngine::Glm.buffers_whole_output());
    assert!(CodeEngine::Glm
        .parse_line("{\"response\":\"x\"}")
        .is_empty());
}

#[test]
fn glm_envelope_without_response_fails_closed() {
    // ocr/codex review R1: a schema change or error envelope must never
    // masquerade as a clean empty run.
    let no_resp = r#"{"sessionId":"sess_9","eventCount":3}"#;
    let events = CodeEngine::Glm.parse_whole(no_resp);
    assert!(
        matches!(&events[0], CodeEvent::SessionStarted { session_id } if session_id == "sess_9")
    );
    assert!(matches!(&events[1], CodeEvent::Error(e) if e.contains("no response field")));
    let err_env = r#"{"error":"quota exceeded"}"#;
    let events = CodeEngine::Glm.parse_whole(err_env);
    assert!(matches!(&events[0], CodeEvent::Error(e) if e.contains("quota exceeded")));
}

// ---- dsh / DeepSeek Harness (daily-driver P9) ----
//
// dsh headless has NO model flag and NO resume/session flag on the argv
// (verified 2026-09-18, dsh 0.1.5-rc.1): the model travels in the `DSH_MODEL`
// environment variable and continuity travels in the prompt (the bounded
// context pack). These pin the argv contract and the plain-text whole-output
// parse, mirroring the GLM tests above.

#[test]
fn dsh_args_first_turn_and_resume_are_identical() {
    // First turn: exactly `--profile headless <prompt>` — no model, no resume,
    // no permission flags (headless has none).
    assert_eq!(
        CodeEngine::Dsh.build_args("fix the bug", Autonomy::Autonomous, None, None),
        ["--profile", "headless", "fix the bug"]
    );
    // Resume: the resume id is IGNORED — a first turn and a "resume" turn
    // build byte-identical argv, because headless has no session to resume.
    // `model` is ignored on the argv too (it travels in DSH_MODEL).
    let first = CodeEngine::Dsh.build_args("fix the bug", Autonomy::Autonomous, None, None);
    let resumed = CodeEngine::Dsh.build_args(
        "fix the bug",
        Autonomy::AskFirst,
        Some("should-not-appear"),
        Some("deepseek-v4-pro"),
    );
    assert_eq!(first, resumed);
    assert!(
        !resumed.iter().any(|a| a.contains("should-not-appear")),
        "a dsh argv must never carry a session/resume id: {resumed:?}"
    );
    assert!(
        !resumed.iter().any(|a| a.contains("deepseek-v4-pro")),
        "a dsh argv must never carry a model (it travels in DSH_MODEL): {resumed:?}"
    );
    assert!(!resumed.contains(&"--resume".to_string()));
}

#[test]
fn dsh_parse_whole_plain_text_is_single_answer() {
    // stdout is the final assistant message ONLY — plain text, no envelope.
    let out = "I fixed the flaky test by pinning the clock.\nRun it again to confirm.";
    let events = CodeEngine::Dsh.parse_whole(out);
    assert_eq!(
        events,
        vec![
            CodeEvent::AssistantText(
                "I fixed the flaky test by pinning the clock.\nRun it again to confirm.".into()
            ),
            CodeEvent::TurnComplete,
        ]
    );
    // No session id exists in dsh — never a SessionStarted event.
    assert!(!events
        .iter()
        .any(|e| matches!(e, CodeEvent::SessionStarted { .. })));
}

#[test]
fn dsh_parse_whole_empty_fails_closed() {
    // Empty and all-whitespace stdout is a FAILED turn — exactly one Error,
    // never silence and never a clean empty answer.
    for empty in ["", "   ", "\n\n\t\n"] {
        let events = CodeEngine::Dsh.parse_whole(empty);
        assert_eq!(
            events,
            vec![CodeEvent::Error("dsh printed no final message".into())],
            "empty/whitespace dsh stdout must fail closed: {empty:?}"
        );
    }
}

#[test]
fn dsh_parse_whole_trims_but_preserves_inner_newlines() {
    // Outer whitespace is trimmed; INNER newlines and indentation are part of
    // the answer and survive verbatim.
    let out = "\n\n  line one\nline two   \n\n";
    let events = CodeEngine::Dsh.parse_whole(out);
    match events.as_slice() {
        [CodeEvent::AssistantText(text), CodeEvent::TurnComplete] => {
            assert_eq!(text, "line one\nline two");
        }
        other => panic!("expected one answer + TurnComplete, got {other:?}"),
    }
}

#[test]
fn dsh_per_line_parsing_is_inert() {
    // Whole-output engine: the driver buffers stdout and calls parse_whole at
    // EOF; per-line parsing yields nothing (like Glm).
    assert!(CodeEngine::Dsh.buffers_whole_output());
    assert!(CodeEngine::Dsh.parse_line("not json at all").is_empty());
    assert!(CodeEngine::Dsh.parse_line("{\"text\":\"x\"}").is_empty());
    assert!(CodeEngine::Dsh.parse_line("").is_empty());
}

#[test]
fn dsh_permission_args_empty() {
    // Headless has no sandbox/permission flag; the posture is the profile
    // default (governed by DSH_PERMISSION_MODE in env, which Awili does not
    // set) — so both postures contribute zero argv.
    assert_eq!(
        CodeEngine::Dsh.permission_args(Autonomy::Autonomous),
        Vec::<String>::new()
    );
    assert_eq!(
        CodeEngine::Dsh.permission_args(Autonomy::AskFirst),
        Vec::<String>::new()
    );
    // And the bin/label the chip and the driver key off.
    assert_eq!(CodeEngine::Dsh.bin(), "dsh");
    assert_eq!(CodeEngine::Dsh.label(), "DeepSeek");
}

/// RC2-SVC5 (F0376): a valid envelope followed by log text still parses, and
/// a JSON log object around it does not replace the envelope.
#[test]
fn rc2svc5_glm_envelope_survives_trailing_and_surrounding_logs() {
    let out =
        "warn: starting\n{\"sessionId\":\"sess_7\",\"response\":\"done\"}\n[exit] cleanup ok\n";
    let events = CodeEngine::Glm.parse_whole(out);
    assert!(
        matches!(&events[0], CodeEvent::SessionStarted { session_id } if session_id == "sess_7"),
        "{events:?}"
    );
    assert!(events.contains(&CodeEvent::AssistantText("done".into())));
    assert_eq!(events.last(), Some(&CodeEvent::TurnComplete));

    let noisy = "{\"level\":\"info\",\"msg\":\"boot\"}\n{\"sessionId\":\"s2\",\"response\":\"ok\",\"usage\":{\"inputTokens\":1}}\n{\"level\":\"info\",\"msg\":\"bye\"}";
    let events = CodeEngine::Glm.parse_whole(noisy);
    assert!(
        events.contains(&CodeEvent::AssistantText("ok".into())),
        "{events:?}"
    );
}

/// RC2-SVC5 (F0375): a Claude `result` with neither `is_error` nor `subtype`
/// is a failed turn, never a success.
#[test]
fn rc2svc5_a_result_without_terminal_fields_is_a_failure() {
    let events = CodeEngine::Claude.parse_line(r#"{"type":"result","result":"maybe fine"}"#);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CodeEvent::Error(m) if m.contains("without saying"))),
        "{events:?}"
    );
    assert!(!events.contains(&CodeEvent::TurnComplete), "{events:?}");
    // The affirmative shape is still a success.
    let ok = CodeEngine::Claude
        .parse_line(r#"{"type":"result","subtype":"success","is_error":false,"result":"x"}"#);
    assert_eq!(ok.last(), Some(&CodeEvent::TurnComplete));
}

/// RC2-SVCF1 (Codex review of 1b59feee, P2): a trailing JSON log record that
/// carries an `error` key never replaces a valid result envelope, and output
/// made only of log records is a failure that shows what was printed.
#[test]
fn rc2svcf1_a_trailing_error_log_never_replaces_the_envelope() {
    // The exact case from the review.
    let out = "{\"sessionId\":\"s3\",\"response\":\"done\"}\n{\"level\":\"error\",\"error\":\"cleanup\"}\n";
    let events = CodeEngine::Glm.parse_whole(out);
    assert_eq!(
        events,
        vec![
            CodeEvent::SessionStarted {
                session_id: "s3".into()
            },
            CodeEvent::AssistantText("done".into()),
            CodeEvent::TurnComplete,
        ]
    );
    // Numeric (pino-style) levels and `severity` are log records too.
    let out = "{\"level\":30,\"msg\":\"boot\"}\n{\"sessionId\":\"s4\",\"response\":\"ok\"}\n{\"level\":50,\"error\":\"socket closed\"}\n{\"severity\":\"ERROR\",\"error\":\"flush\"}";
    let events = CodeEngine::Glm.parse_whole(out);
    assert!(
        events.contains(&CodeEvent::AssistantText("ok".into())),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, CodeEvent::Error(_))),
        "{events:?}"
    );
    // A real error envelope (no log level) still fails the turn.
    let events = CodeEngine::Glm
        .parse_whole("{\"level\":\"info\",\"msg\":\"boot\"}\n{\"error\":\"quota exceeded\"}");
    assert!(
        matches!(&events[0], CodeEvent::Error(e) if e.contains("quota exceeded")),
        "{events:?}"
    );
    // Only log records: a failure naming what the engine printed.
    let events = CodeEngine::Glm.parse_whole("{\"level\":\"error\",\"error\":\"cleanup\"}");
    assert!(
        matches!(&events[0], CodeEvent::Error(e) if e.contains("no parseable result") && e.contains("cleanup")),
        "{events:?}"
    );
}

/// RC2-SVCG (Codex review of 0d6fc200): a `level` that is not a string or a
/// number does not make a log line, so a trailing error envelope with
/// `"level":null` (or an object/bool level) still fails the turn.
#[test]
fn rc2svcg_a_null_level_does_not_hide_a_trailing_error() {
    for level in ["null", "true", "{}", "[]"] {
        let out = format!(
            "{{\"sessionId\":\"s5\",\"response\":\"done\"}}\n{{\"level\":{level},\"error\":\"quota\"}}\n"
        );
        let events = CodeEngine::Glm.parse_whole(&out);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, CodeEvent::Error(m) if m.contains("quota"))),
            "level {level}: {events:?}"
        );
    }
    // A string or numeric level is still a log line.
    let events = CodeEngine::Glm.parse_whole(
        "{\"sessionId\":\"s6\",\"response\":\"done\"}\n{\"level\":\"warn\",\"error\":\"x\"}",
    );
    assert!(
        events.contains(&CodeEvent::AssistantText("done".into())),
        "{events:?}"
    );
}
