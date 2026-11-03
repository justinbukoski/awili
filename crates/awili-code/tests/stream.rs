use awili_code::{stream::StreamParser, CodeEngine, CodeEvent};
use serde_json::json;

fn message(event: &str, id: &str, text: &str) -> String {
    json!({"type":event,"item":{"id":id,"type":"agent_message","text":text}}).to_string()
}
fn text(events: Vec<CodeEvent>) -> String {
    events
        .into_iter()
        .filter_map(|e| {
            if let CodeEvent::AssistantText(s) = e {
                Some(s)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn separate_messages_keep_boundaries_across_tools() {
    let mut parser = StreamParser::new(CodeEngine::Codex);
    let mut output = text(parser.parse_line(&message(
        "item.completed",
        "commentary",
        "I will not modify anything.",
    )));
    let tool = parser.parse_line(r#"{"type":"item.completed","item":{"id":"tool","type":"command_execution","command":"cat challenge","exit_code":0}}"#);
    assert!(matches!(
        tool.as_slice(),
        [CodeEvent::ToolFinished { ok: true, .. }]
    ));
    output += &text(parser.parse_line(&message("item.completed", "final", "opaque-nonce")));
    assert_eq!(output, "I will not modify anything.\n\nopaque-nonce");
    assert!(output.lines().any(|line| line == "opaque-nonce"));
}

#[test]
fn partial_snapshots_and_unsupported_deltas_do_not_duplicate_final_text() {
    let mut parser = StreamParser::new(CodeEngine::Codex);
    assert!(parser
        .parse_line(&message("item.started", "one", "part"))
        .is_empty());
    assert!(parser
        .parse_line(&message("item.updated", "one", "partial"))
        .is_empty());
    assert!(parser
        .parse_line(r#"{"type":"item.delta","item_id":"one","delta":" text"}"#)
        .is_empty());
    assert_eq!(
        text(parser.parse_line(&message(
            "item.completed",
            "one",
            "partial text\nsecond line"
        ))),
        "partial text\nsecond line"
    );
    assert!(parser
        .parse_line(&message(
            "item.completed",
            "one",
            "partial text\nsecond line"
        ))
        .is_empty());
    assert_eq!(
        text(parser.parse_line(&message("item.completed", "two", "next"))),
        "\n\nnext"
    );
    assert!(parser
        .parse_line(&message(
            "item.completed",
            "one",
            "partial text\nsecond line"
        ))
        .is_empty());
}

#[test]
fn empty_and_identical_messages_do_not_invent_or_erase_boundaries() {
    let mut parser = StreamParser::new(CodeEngine::Codex);
    assert!(parser
        .parse_line(&message("item.completed", "empty", ""))
        .is_empty());
    assert_eq!(
        text(parser.parse_line(&message("item.completed", "a", "same"))),
        "same"
    );
    assert_eq!(
        text(parser.parse_line(&message("item.completed", "b", "same"))),
        "\n\nsame"
    );
    let mut fresh = StreamParser::new(CodeEngine::Codex);
    assert_eq!(
        text(fresh.parse_line(&message("item.completed", "a", "same"))),
        "same"
    );
}

#[test]
fn other_engines_keep_existing_chunk_semantics() {
    // RC2-RUN (R6-3, deliberate change): a Claude text block that follows
    // shown text starts after a blank line; the text itself is unchanged.
    let mut parser = StreamParser::new(CodeEngine::Claude);
    let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"chunk"}]}}"#;
    assert_eq!(
        parser.parse_line(line),
        vec![CodeEvent::AssistantText("chunk".into())]
    );
    assert_eq!(
        parser.parse_line(line),
        vec![
            CodeEvent::AssistantText("\n\n".into()),
            CodeEvent::AssistantText("chunk".into())
        ]
    );
}

fn claude_event(event: serde_json::Value) -> String {
    json!({"type":"stream_event","event":event,"parent_tool_use_id":null}).to_string()
}
fn claude_block_start(index: u64, kind: &str) -> String {
    claude_event(json!({"type":"content_block_start","index":index,"content_block":{"type":kind}}))
}
fn claude_text_delta(index: u64, text: &str) -> String {
    claude_event(
        json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}),
    )
}
fn claude_assistant(block: serde_json::Value) -> String {
    json!({"type":"assistant","message":{"content":[block]},"parent_tool_use_id":null}).to_string()
}

/// RC2-RUN (R3-20, R6-3): with `--include-partial-messages` (the shape
/// claude 2.1.282 prints, 2026-10-06: thinking, a text block, a tool call,
/// the tool result, a second text block — each block also arriving complete
/// on its own `assistant` event) the reply text is shown delta by delta as
/// it arrives, exactly once, and the second block starts after a blank line.
#[test]
fn rc2_run_claude_text_streams_once_with_a_break_between_blocks() {
    let mut parser = StreamParser::new(CodeEngine::Claude);
    let mut shown = Vec::new();
    let mut feed = |parser: &mut StreamParser, line: String| {
        let t = text(parser.parse_line(&line));
        shown.push(t.clone());
        t
    };
    feed(&mut parser, claude_event(json!({"type":"message_start"})));
    feed(&mut parser, claude_block_start(0, "thinking"));
    feed(
        &mut parser,
        claude_event(
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}),
        ),
    );
    feed(
        &mut parser,
        claude_assistant(json!({"type":"thinking","thinking":"","signature":"s"})),
    );
    feed(&mut parser, claude_block_start(1, "text"));
    // Each delta is shown as soon as it arrives.
    assert_eq!(feed(&mut parser, claude_text_delta(1, "Let me")), "Let me");
    assert_eq!(
        feed(&mut parser, claude_text_delta(1, " plan the write.")),
        " plan the write."
    );
    // The complete copy of the same block adds nothing.
    assert_eq!(
        feed(
            &mut parser,
            claude_assistant(json!({"type":"text","text":"Let me plan the write."}))
        ),
        ""
    );
    feed(
        &mut parser,
        claude_event(json!({"type":"content_block_stop","index":1})),
    );
    feed(&mut parser, claude_block_start(2, "tool_use"));
    let tool = parser.parse_line(&claude_assistant(
        json!({"type":"tool_use","name":"Bash","input":{"command":"ls"}}),
    ));
    assert!(matches!(tool.as_slice(), [CodeEvent::ToolStarted { .. }]));
    feed(
        &mut parser,
        json!({"type":"user","message":{"content":[{"type":"tool_result","content":"a.txt"}]}})
            .to_string(),
    );
    feed(&mut parser, claude_block_start(0, "text"));
    assert_eq!(
        feed(&mut parser, claude_text_delta(0, "The folder has")),
        "\n\nThe folder has"
    );
    feed(&mut parser, claude_text_delta(0, " no hello.txt yet."));
    feed(
        &mut parser,
        claude_assistant(json!({"type":"text","text":"The folder has no hello.txt yet."})),
    );
    assert_eq!(
        shown.concat(),
        "Let me plan the write.\n\nThe folder has no hello.txt yet."
    );
}

/// RC2-RUN (R6-3): a Claude CLI that sends no partial deltas still gets the
/// blank line between text blocks (round 6/7: "…the write.The folder has no
/// hello.txt yet"); a subagent's partial text is never streamed.
#[test]
fn rc2_run_claude_blocks_without_deltas_are_separated() {
    let mut parser = StreamParser::new(CodeEngine::Claude);
    let first = text(parser.parse_line(&claude_assistant(
        json!({"type":"text","text":"Creating the file now."}),
    )));
    let sub = json!({"type":"stream_event","parent_tool_use_id":"toolu_1","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"inner"}}}).to_string();
    assert_eq!(text(parser.parse_line(&sub)), "");
    let second = text(parser.parse_line(&claude_assistant(
        json!({"type":"text","text":"Done, hello.txt now exists."}),
    )));
    assert_eq!(
        format!("{first}{second}"),
        "Creating the file now.\n\nDone, hello.txt now exists."
    );
}

/// RC2-FIXB1 (RC2-UX review P2): a Claude tool result names the step it
/// finished (by its `tool_use_id`), so the app can tell an edit that
/// finished well from one that only started; a result for an unknown call
/// still reads "done". A Codex file change lists its files in `changes` and
/// a failed patch is not a finished change.
#[test]
fn rc2fixb_a_finished_step_says_which_step_it_was() {
    let mut parser = StreamParser::new(CodeEngine::Claude);
    let started = parser.parse_line(&claude_assistant(
        json!({"type":"tool_use","id":"toolu_9","name":"MultiEdit","input":{"file_path":"/p/a.rs"}}),
    ));
    assert_eq!(
        started,
        [CodeEvent::ToolStarted {
            label: "MultiEdit /p/a.rs".into()
        }]
    );
    let result = |id: &str, is_error: bool| {
        json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":id,"content":"ok","is_error":is_error}
        ]}})
        .to_string()
    };
    assert_eq!(
        parser.parse_line(&result("toolu_9", false)),
        [CodeEvent::ToolFinished {
            label: "MultiEdit /p/a.rs".into(),
            ok: true
        }]
    );
    assert_eq!(
        parser.parse_line(&result("toolu_unknown", true)),
        [CodeEvent::ToolFinished {
            label: "done".into(),
            ok: false
        }]
    );
    let notebook = parser.parse_line(&claude_assistant(
        json!({"type":"tool_use","id":"toolu_n","name":"NotebookEdit","input":{"notebook_path":"/p/n.ipynb"}}),
    ));
    assert_eq!(
        notebook,
        [CodeEvent::ToolStarted {
            label: "NotebookEdit /p/n.ipynb".into()
        }]
    );

    let mut codex = StreamParser::new(CodeEngine::Codex);
    let change = |status: &str| {
        json!({"type":"item.completed","item":{"id":"c1","type":"file_change",
            "changes":[{"path":"src/a.rs","kind":"update"},{"path":"src/b.rs","kind":"add"}],
            "status":status}})
        .to_string()
    };
    // RC2-R2J (RC2-R2F review P3 on 1366a0c0): a change that worked is
    // named as it starts too, as over the app server.
    assert_eq!(
        codex.parse_line(&change("completed")),
        [
            CodeEvent::ToolStarted {
                label: "edit src/a.rs".into()
            },
            CodeEvent::ToolFinished {
                label: "edit src/a.rs".into(),
                ok: true
            },
            CodeEvent::ToolStarted {
                label: "edit src/b.rs".into()
            },
            CodeEvent::ToolFinished {
                label: "edit src/b.rs".into(),
                ok: true
            }
        ]
    );
    // RC2-FIXC3 (RC2-FIXB review P3): a failed change names each file it
    // tried before it reports the failure.
    assert_eq!(
        codex.parse_line(&change("failed")),
        [
            CodeEvent::ToolStarted {
                label: "edit src/a.rs".into()
            },
            CodeEvent::ToolFinished {
                label: "edit src/a.rs".into(),
                ok: false
            },
            CodeEvent::ToolStarted {
                label: "edit src/b.rs".into()
            },
            CodeEvent::ToolFinished {
                label: "edit src/b.rs".into(),
                ok: false
            }
        ]
    );
}

/// RC2-R2J (RC2-R2F review P3 on 1366a0c0: on `codex exec --json` a
/// council card listed no tools). A command and an MCP tool are named as
/// they start, a web search when it ends; an agent message starting is no
/// tool.
#[test]
fn rc2r2j_codex_exec_names_its_tools() {
    let mut codex = StreamParser::new(CodeEngine::Codex);
    let started = |item: serde_json::Value| json!({"type":"item.started","item":item}).to_string();
    assert_eq!(
        codex.parse_line(&started(json!({"id":"c1","type":"command_execution",
            "command":"ls -la\nmore","status":"in_progress"}))),
        [CodeEvent::ToolStarted {
            label: "$ ls -la".into()
        }]
    );
    assert_eq!(
        codex.parse_line(&started(json!({"id":"m1","type":"mcp_tool_call",
            "server":"awili-connectors","tool":"gmail_search","arguments":{},
            "status":"in_progress"}))),
        [CodeEvent::ToolStarted {
            label: "awili-connectors: gmail_search".into()
        }]
    );
    assert_eq!(
        codex.parse_line(
            &json!({"type":"item.completed","item":{"id":"w1","type":"web_search",
                "query":"zfs vs btrfs"}})
            .to_string()
        ),
        [CodeEvent::ToolStarted {
            label: "web search: zfs vs btrfs".into()
        }]
    );
    assert!(codex
        .parse_line(&started(
            json!({"id":"a1","type":"agent_message","text":""})
        ))
        .is_empty());
}

/// RC2-R2K (RC2-R2F review P3 on 55d863a3): on `codex exec --json` too, a
/// call to another agent is named as it starts.
#[test]
fn rc2r2k_codex_exec_names_agent_calls() {
    let mut codex = StreamParser::new(CodeEngine::Codex);
    let line = json!({"type":"item.started","item":{"id":"a2","type":"collab_tool_call",
        "tool":"spawn_agent","sender_thread_id":"t","receiver_thread_ids":[],
        "status":"in_progress"}})
    .to_string();
    assert_eq!(
        codex.parse_line(&line),
        [CodeEvent::ToolStarted {
            label: "agent: spawn_agent".into()
        }]
    );
}
