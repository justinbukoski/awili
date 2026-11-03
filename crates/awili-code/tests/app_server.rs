//! RC2-CXS: the Codex app-server protocol for one turn. Shapes are the ones
//! codex-cli 0.160.1 printed live on 2026-10-07 (`RC2-CXS1.md`).
use awili_code::app_server::{AppServerTurn, Handshake, TurnSpec};
use awili_code::{Autonomy, CodeEngine, CodeEvent};
use serde_json::{json, Value};

fn spec(resume: Option<&str>, autonomy: Autonomy) -> TurnSpec {
    TurnSpec {
        cwd: "/work/folder".into(),
        prompt: "Say hi".into(),
        resume_id: resume.map(str::to_string),
        model: Some("gpt-6.1-sol".into()),
        autonomy,
    }
}

fn parse(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn text(events: &[CodeEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            CodeEvent::AssistantText(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

/// Run the handshake to an open thread `thr-1` and a started turn.
fn open(turn: &mut AppServerTurn) -> Vec<Value> {
    let mut sent = parse(&turn.start());
    let step = turn.on_line(r#"{"id":1,"result":{"userAgent":"awili/0.160.1","codexHome":"/h/.codex","platformFamily":"unix","platformOs":"macos"}}"#);
    assert_eq!(turn.handshake(), Handshake::Ready);
    sent.extend(parse(&step.send));
    let step = turn.on_line(r#"{"id":2,"result":{"thread":{"id":"thr-1","sessionId":"thr-1"},"model":"gpt-6.1-sol","approvalPolicy":"never","sandbox":{"type":"dangerFullAccess"}}}"#);
    assert_eq!(
        step.events,
        [CodeEvent::SessionStarted {
            session_id: "thr-1".into()
        }]
    );
    sent.extend(parse(&step.send));
    let step = turn.on_line(r#"{"id":3,"result":{"turn":{"id":"turn-1","status":"inProgress"}}}"#);
    assert!(step.send.is_empty() && step.events.is_empty());
    sent
}

fn delta(thread: &str, item: &str, text: &str) -> String {
    json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"turnId":"turn-1","itemId":item,"delta":text}}).to_string()
}

/// RC2-CXS (R3-20 Codex part): the reply streams piece by piece as Codex
/// writes it; the complete copy is not shown again; a second message
/// starts after a blank line; tokens are the thread's totals (as `exec
/// --json` reported them); the turn completes once.
#[test]
fn rc2_cxs_codex_reply_streams_piece_by_piece() {
    let mut turn = AppServerTurn::new(spec(None, Autonomy::Autonomous));
    let sent = open(&mut turn);
    let methods: Vec<&str> = sent.iter().filter_map(|v| v["method"].as_str()).collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "thread/start", "turn/start"]
    );
    assert_eq!(sent[0]["params"]["clientInfo"]["name"], "awili");
    assert_eq!(
        sent[2]["params"],
        json!({"cwd":"/work/folder","sandbox":"danger-full-access","approvalPolicy":"never","model":"gpt-6.1-sol"})
    );
    assert_eq!(
        sent[3]["params"],
        // RC2-R615: with a summary of Codex's thinking asked for.
        json!({"threadId":"thr-1","input":[{"type":"text","text":"Say hi"}],"summary":"auto"})
    );

    let mut pieces = Vec::new();
    for line in [
        r#"{"method":"turn/started","params":{"threadId":"thr-1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}}"#.to_string(),
        r#"{"method":"item/started","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"agentMessage","id":"msg-1","text":""}}}"#.to_string(),
        delta("thr-1", "msg-1", "Hi"),
        delta("thr-1", "msg-1", " there"),
        r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","completedAtMs":1,"item":{"type":"agentMessage","id":"msg-1","text":"Hi there"}}}"#.to_string(),
        delta("thr-1", "msg-2", "Bye"),
        r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","completedAtMs":2,"item":{"type":"agentMessage","id":"msg-2","text":"Bye"}}}"#.to_string(),
    ] {
        let step = turn.on_line(&line);
        assert!(step.send.is_empty());
        pieces.push(text(&step.events));
    }
    assert_eq!(
        pieces,
        [
            vec![],
            vec![],
            vec!["Hi".to_string()],
            vec![" there".to_string()],
            vec![],
            vec!["\n\n".to_string(), "Bye".to_string()],
            vec![],
        ]
    );
    assert!(!turn.finished());
    turn.on_line(r#"{"method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","turnId":"turn-1","tokenUsage":{"total":{"totalTokens":9100,"inputTokens":9000,"cachedInputTokens":0,"outputTokens":100,"reasoningOutputTokens":40},"last":{"totalTokens":10,"inputTokens":5,"cachedInputTokens":0,"outputTokens":5,"reasoningOutputTokens":0},"modelContextWindow":258400}}}"#);
    let end = turn.on_line(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","items":[],"status":"completed","error":null}}}"#);
    assert_eq!(
        end.events,
        [
            CodeEvent::ContextWindow(258400),
            CodeEvent::Usage {
                cached_input_tokens: Some(0),
                input_tokens: 9000,
                output_tokens: 100
            },
            CodeEvent::TurnComplete
        ]
    );
    assert!(turn.finished() && turn.completed());
}

/// RC2-CXS: a message Codex sends only complete is shown once; tool items
/// read as the exec path's activity lines (with each changed file named);
/// a subagent's thread is not this reply; a stray approval request or MCP
/// question is declined, an unknown request refused; a retry is a note, a
/// failure one error in Codex's words.
#[test]
fn rc2_cxs_codex_items_requests_and_failures() {
    let mut turn = AppServerTurn::new(spec(None, Autonomy::Autonomous));
    open(&mut turn);
    let step = turn.on_line(r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"agentMessage","id":"m1","text":"Whole"}}}"#);
    assert_eq!(text(&step.events), ["Whole"]);
    let again = turn.on_line(r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"agentMessage","id":"m1","text":"Whole"}}}"#);
    assert!(again.events.is_empty());
    let cmd = turn.on_line(r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"commandExecution","id":"c1","command":"cargo test\nmore","commandActions":[],"cwd":"/","status":"failed","exitCode":101}}}"#);
    assert_eq!(
        cmd.events,
        [CodeEvent::ToolFinished {
            label: "$ cargo test".into(),
            ok: false
        }]
    );
    let edit = turn.on_line(r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"fileChange","id":"f1","status":"completed","changes":[{"path":"/work/folder/a.rs","kind":{"type":"update"},"diff":""},{"path":"/work/folder/b.rs","kind":{"type":"add"},"diff":""}]}}}"#);
    assert_eq!(
        edit.events,
        [
            CodeEvent::ToolFinished {
                label: "edit /work/folder/a.rs".into(),
                ok: true
            },
            CodeEvent::ToolFinished {
                label: "edit /work/folder/b.rs".into(),
                ok: true
            }
        ]
    );
    let sub = turn.on_line(&delta("thr-sub", "s1", "not mine"));
    assert!(sub.events.is_empty());
    let sub_end = turn.on_line(r#"{"method":"turn/completed","params":{"threadId":"thr-sub","turn":{"id":"turn-9","items":[],"status":"completed"}}}"#);
    assert!(sub_end.events.is_empty() && !turn.finished());

    for (request, answer) in [
        (
            r#"{"id":7,"method":"item/commandExecution/requestApproval","params":{}}"#,
            json!({"id":7,"result":{"decision":"decline"}}),
        ),
        (
            r#"{"id":"x","method":"item/fileChange/requestApproval","params":{}}"#,
            json!({"id":"x","result":{"decision":"decline"}}),
        ),
        (
            r#"{"id":8,"method":"mcpServer/elicitation/request","params":{}}"#,
            json!({"id":8,"result":{"action":"decline"}}),
        ),
        (
            r#"{"id":9,"method":"execCommandApproval","params":{}}"#,
            json!({"id":9,"result":{"decision":"denied"}}),
        ),
    ] {
        let step = turn.on_line(request);
        assert_eq!(parse(&step.send), [answer]);
        assert!(step.events.is_empty());
    }
    let unknown = turn.on_line(r#"{"id":10,"method":"item/tool/call","params":{}}"#);
    assert_eq!(parse(&unknown.send)[0]["error"]["code"], -32601);

    let retry = turn.on_line(r#"{"method":"error","params":{"threadId":"thr-1","turnId":"turn-1","willRetry":true,"error":{"message":"Reconnecting... 1/5"}}}"#);
    assert!(matches!(
        retry.events.as_slice(),
        [CodeEvent::Diagnostic(_)]
    ));
    let fatal = turn.on_line(r#"{"method":"error","params":{"threadId":"thr-1","turnId":"turn-1","willRetry":false,"error":{"message":"You've hit your usage limit."}}}"#);
    assert_eq!(
        fatal.events,
        [CodeEvent::Error("You've hit your usage limit.".into())]
    );
    let end = turn.on_line(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","items":[],"status":"failed","error":{"message":"You've hit your usage limit."}}}}"#);
    assert!(
        end.events.is_empty(),
        "one error, not two: {:?}",
        end.events
    );
    assert!(turn.finished() && !turn.completed());
}

/// RC2-CXS: continuing a conversation resumes its Codex session; the
/// planning step of "Ask before changes" (and Chat) is read-only; a refused
/// `initialize` means this Codex has no app server; a thread Codex cannot
/// open is said plainly.
#[test]
fn rc2_cxs_codex_resume_ask_and_unavailable() {
    let mut turn = AppServerTurn::new(spec(Some("thr-old"), Autonomy::AskFirst));
    turn.start();
    let step = turn.on_line(r#"{"id":1,"result":{}}"#);
    let sent = parse(&step.send);
    assert_eq!(sent[1]["method"], "thread/resume");
    assert_eq!(
        sent[1]["params"],
        json!({"threadId":"thr-old","cwd":"/work/folder","sandbox":"read-only","approvalPolicy":"never","model":"gpt-6.1-sol","excludeTurns":true})
    );
    let busy = turn.on_line(r#"{"error":{"code":-32600,"message":"thread thr-old already has an active writer"},"id":2}"#);
    assert!(busy.events.is_empty() && !turn.finished());
    let fork = parse(&busy.send);
    assert_eq!(fork[0]["method"], "thread/fork");
    assert_eq!(fork[0]["params"], sent[1]["params"]);
    // Replayed resume error cannot branch twice.
    assert!(turn
        .on_line(r#"{"id":2,"error":{"message":"already has an active writer"}}"#)
        .send
        .is_empty());
    let opened = turn.on_line(r#"{"id":4,"result":{"thread":{"id":"thr-independent"}}}"#);
    assert_eq!(
        opened.events,
        [
            CodeEvent::Diagnostic("Continuing in an independent copy of this conversation.".into()),
            CodeEvent::SessionStarted {
                session_id: "thr-independent".into()
            }
        ]
    );
    let ask = parse(&opened.send);
    assert_eq!(ask.len(), 1);
    assert_eq!(ask[0]["method"], "turn/start");
    assert_eq!(ask[0]["params"]["threadId"], "thr-independent");
    assert_eq!(ask[0]["params"]["input"][0]["text"], "Say hi");
    assert!(turn
        .on_line(r#"{"id":4,"result":{"thread":{"id":"thr-independent"}}}"#)
        .send
        .is_empty());

    let mut gone = AppServerTurn::new(spec(Some("thr-gone"), Autonomy::Autonomous));
    gone.start();
    gone.on_line(r#"{"id":1,"result":{}}"#);
    let step = gone.on_line(
        r#"{"error":{"code":-32600,"message":"no rollout found for thread id thr-gone"},"id":2}"#,
    );
    assert!(
        matches!(step.events.as_slice(), [CodeEvent::Error(m)] if m.starts_with("Codex can no longer find this conversation's earlier session")),
        "{:?}",
        step.events
    );

    let mut refused = AppServerTurn::new(spec(None, Autonomy::Autonomous));
    refused.start();
    assert_eq!(refused.handshake(), Handshake::Pending);
    assert!(refused.on_line("").send.is_empty());
    assert_eq!(refused.handshake(), Handshake::Pending);
    refused.on_line(r#"{"id":1,"error":{"code":-32601,"message":"method not found"}}"#);
    assert_eq!(refused.handshake(), Handshake::Unavailable);
    // An exec-shaped event (or any text) before the answer: not an app server.
    for line in [
        r#"{"type":"thread.started","thread_id":"t"}"#,
        "Usage: codex [OPTIONS] [PROMPT]",
    ] {
        let mut old = AppServerTurn::new(spec(None, Autonomy::Autonomous));
        old.start();
        let step = old.on_line(line);
        assert!(step.send.is_empty() && step.events.is_empty());
        assert_eq!(old.handshake(), Handshake::Unavailable, "{line}");
    }

    assert_eq!(
        CodeEngine::Codex.app_server_args(Autonomy::AskFirst),
        [
            "app-server",
            "-c",
            "sandbox_mode=\"read-only\"",
            "-c",
            "approval_policy=\"never\""
        ]
    );
    assert_eq!(
        CodeEngine::Codex.app_server_args(Autonomy::Autonomous)[2],
        "sandbox_mode=\"danger-full-access\""
    );
    assert!(CodeEngine::Claude
        .app_server_args(Autonomy::Autonomous)
        .is_empty());
}

/// RC2-R2F9 (usability RC2 round 2, F7: Codex wrote nothing for 19-33 s on
/// a council): a warm turn opens its thread before the question exists and
/// sends no `turn/start`; asked, it sends the question on that thread at
/// once, and its reply then streams as any turn's.
#[test]
fn rc2r2f9_a_warm_turn_opens_its_thread_before_the_question() {
    let mut turn = AppServerTurn::warm(spec(None, Autonomy::AskFirst));
    assert!(turn.ask("too early".into()).is_empty(), "no thread yet");
    let mut sent = parse(&turn.start());
    let step = turn.on_line(r#"{"id":1,"result":{"userAgent":"awili/0.160.1"}}"#);
    sent.extend(parse(&step.send));
    let step = turn.on_line(r#"{"id":2,"result":{"thread":{"id":"thr-1"}}}"#);
    assert!(step.send.is_empty(), "no turn/start before the question");
    assert_eq!(
        step.events,
        [CodeEvent::SessionStarted {
            session_id: "thr-1".into()
        }]
    );
    let methods: Vec<&str> = sent.iter().filter_map(|v| v["method"].as_str()).collect();
    assert_eq!(methods, ["initialize", "initialized", "thread/start"]);
    assert!(turn.thread_ready());
    assert_eq!(turn.thread_id(), Some("thr-1"));
    let ask = parse(&turn.ask("In one sentence: what is RAID 1?".into()));
    assert_eq!(ask.len(), 1);
    assert_eq!(ask[0]["method"], "turn/start");
    assert_eq!(ask[0]["id"], 3);
    assert_eq!(ask[0]["params"]["threadId"], "thr-1");
    assert_eq!(
        ask[0]["params"]["input"][0]["text"],
        "In one sentence: what is RAID 1?"
    );
    assert!(!turn.thread_ready(), "asked once");
    turn.on_line(r#"{"id":3,"result":{"turn":{"id":"turn-1","status":"inProgress"}}}"#);
    let step = turn.on_line(&delta("thr-1", "m1", "Mirroring."));
    assert_eq!(text(&step.events), ["Mirroring."]);
}

/// RC2-R2J (RC2-R2F review P3 on 1366a0c0: a council card could list no
/// tools after a Codex member used MCP tools). An MCP tool call, a tool the
/// app gave Codex, and a web search are each named as a tool start.
#[test]
fn rc2r2j_codex_mcp_tools_and_searches_are_named() {
    let mut turn = AppServerTurn::new(spec(None, Autonomy::AskFirst));
    open(&mut turn);
    let mcp = turn.on_line(r#"{"method":"item/started","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"mcpToolCall","id":"t1","server":"awili-connectors","tool":"gmail_search","status":"inProgress","arguments":{}}}}"#);
    assert_eq!(
        mcp.events,
        [CodeEvent::ToolStarted {
            label: "awili-connectors: gmail_search".into()
        }]
    );
    let dynamic = turn.on_line(r#"{"method":"item/started","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"dynamicToolCall","id":"t2","tool":"lookup","status":"inProgress","arguments":{}}}}"#);
    assert_eq!(
        dynamic.events,
        [CodeEvent::ToolStarted {
            label: "lookup".into()
        }]
    );
    let search = turn.on_line(r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"webSearch","id":"t3","query":"zfs vs btrfs"}}}"#);
    assert_eq!(
        search.events,
        [CodeEvent::ToolStarted {
            label: "web search: zfs vs btrfs".into()
        }]
    );
}

/// RC2-R2K (RC2-R2F review P3 on 55d863a3): handing work to another agent
/// is named as a tool start.
#[test]
fn rc2r2k_codex_agent_calls_are_named() {
    let mut turn = AppServerTurn::new(spec(None, Autonomy::AskFirst));
    open(&mut turn);
    let collab = turn.on_line(r#"{"method":"item/started","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"collabAgentToolCall","id":"t4","tool":"spawn_agent","status":"inProgress","senderThreadId":"thr-1","receiverThreadIds":[],"agentsStates":{}}}}"#);
    assert_eq!(
        collab.events,
        [CodeEvent::ToolStarted {
            label: "agent: spawn_agent".into()
        }]
    );
}

/// RC2-R615 (usability RC2 round 5: Codex's council answer opened with "I'll
/// check the filesystem documentation for the main NAS tradeoffs." and a
/// long Codex turn showed nothing for about 75 s). Shapes as codex-cli
/// 0.162.0-alpha.2 printed them live on 2026-10-09: an agent message Codex
/// starts with `phase: "commentary"` is progress (one line, once), never the
/// answer, and the answer that follows starts clean; each topic of Codex's
/// reasoning summary is said once while it thinks.
#[test]
fn rc2r615_codex_commentary_and_thinking_are_progress_not_the_answer() {
    let mut turn = AppServerTurn::new(spec(None, Autonomy::Autonomous));
    open(&mut turn);
    let notes = |events: &[CodeEvent]| -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                CodeEvent::Diagnostic(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    };
    let started = turn.on_line(r#"{"method":"item/started","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"agentMessage","id":"msg-c","text":"","phase":"commentary","memoryCitation":null,"delivery":null,"questions":null}}}"#);
    assert!(text(&started.events).is_empty());
    let d1 = turn.on_line(&delta("thr-1", "msg-c", "I'll check the filesystem "));
    let d2 = turn.on_line(&delta(
        "thr-1",
        "msg-c",
        "documentation for the main NAS tradeoffs.",
    ));
    assert!(
        text(&d1.events).is_empty() && text(&d2.events).is_empty(),
        "no answer text"
    );
    let done = turn.on_line(r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"agentMessage","id":"msg-c","text":"I'll check the filesystem documentation for the main NAS tradeoffs.","phase":"commentary"}}}"#);
    assert!(text(&done.events).is_empty());
    assert_eq!(
        notes(&done.events),
        ["I'll check the filesystem documentation for the main NAS tradeoffs."]
    );
    // Thinking: each topic once, as it is written.
    let summary = |d: &str| {
        json!({"method":"item/reasoning/summaryTextDelta","params":{"threadId":"thr-1","turnId":"turn-1","itemId":"rs-1","delta":d,"summaryIndex":0}}).to_string()
    };
    let t1 = turn.on_line(&summary("**Considering story "));
    assert!(
        notes(&t1.events).is_empty(),
        "the title is not complete yet"
    );
    let t2 = turn.on_line(&summary("composition**\n\nI need to compose a"));
    assert_eq!(
        notes(&t2.events),
        ["Thinking: Considering story composition"]
    );
    let t3 = turn.on_line(&summary(" story about a lighthouse."));
    assert!(notes(&t3.events).is_empty(), "said once");
    // The answer starts clean, with no blank line from the commentary.
    let a1 = turn.on_line(r#"{"method":"item/started","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"agentMessage","id":"msg-a","text":"","phase":"final_answer"}}}"#);
    assert!(text(&a1.events).is_empty());
    let a2 = turn.on_line(&delta("thr-1", "msg-a", "ZFS is the safer pick."));
    assert_eq!(text(&a2.events), ["ZFS is the safer pick."]);
    // An older Codex without phases: every message is the answer, as before.
    let mut old = AppServerTurn::new(spec(None, Autonomy::Autonomous));
    open(&mut old);
    let o = old.on_line(&delta("thr-1", "m", "Hi"));
    assert_eq!(text(&o.events), ["Hi"]);
}

// A resumed thread's first total includes old work. Only `last` belongs to
// this request; subsequent cumulative differences include every tool round.
fn usage_snapshot(turn: &mut AppServerTurn, id: &str, total: (u64, u64), last: Option<(u64, u64)>) {
    let mut usage = json!({"total":{"inputTokens":total.0,"outputTokens":total.1}});
    if let Some(last) = last {
        usage["last"] = json!({"inputTokens":last.0,"outputTokens":last.1});
    }
    turn.on_line(&json!({"method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","turnId":id,"tokenUsage":usage}}).to_string());
}
fn finish_usage(turn: &mut AppServerTurn) -> Vec<CodeEvent> {
    turn.on_line(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}"#).events
}
#[test]
fn resumed_usage_charges_new_inferences_once_not_history() {
    let mut turn = AppServerTurn::new(spec(Some("thr-1"), Autonomy::Autonomous));
    open(&mut turn);
    usage_snapshot(&mut turn, "old-turn", (28941, 96), Some((28941, 96)));
    usage_snapshot(&mut turn, "turn-1", (59621, 183), Some((30680, 87)));
    usage_snapshot(&mut turn, "turn-1", (59621, 183), Some((30680, 87)));
    usage_snapshot(&mut turn, "turn-1", (92020, 235), Some((32399, 52)));
    assert_eq!(
        finish_usage(&mut turn),
        vec![
            CodeEvent::Usage {
                cached_input_tokens: None,
                input_tokens: 63079,
                output_tokens: 139
            },
            CodeEvent::TurnComplete
        ]
    );
    assert!(finish_usage(&mut turn).is_empty());
}
#[test]
fn missing_usage_is_unknown_and_wrong_turn_is_never_billed() {
    let mut turn = AppServerTurn::new(spec(Some("thr-1"), Autonomy::Autonomous));
    open(&mut turn);
    usage_snapshot(&mut turn, "old-turn", (100000, 999), Some((100000, 999)));
    assert_eq!(finish_usage(&mut turn), vec![CodeEvent::TurnComplete]);
    let mut turn = AppServerTurn::new(spec(Some("thr-1"), Autonomy::Autonomous));
    open(&mut turn);
    usage_snapshot(&mut turn, "turn-1", (100000, 999), None);
    assert_eq!(finish_usage(&mut turn), vec![CodeEvent::TurnComplete]);
}
#[test]
fn image_request_uses_codex_image_part() {
    let mut s = spec(None, Autonomy::Autonomous);
    s.prompt = "![screen](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)\nDescribe this".into();
    let mut turn = AppServerTurn::new(s);
    let sent = open(&mut turn);
    assert_eq!(sent[3]["params"]["input"][1]["type"], "image");
    assert!(!sent[3]["params"]["input"][0]["text"]
        .as_str()
        .unwrap()
        .contains("base64"));
}

#[test]
fn early_turn_started_and_counter_reset_keep_attributable_usage() {
    let mut turn = AppServerTurn::new(spec(Some("thr-1"), Autonomy::Autonomous));
    turn.start();
    turn.on_line(r#"{"id":1,"result":{}}"#);
    turn.on_line(r#"{"id":2,"result":{"thread":{"id":"thr-1"}}}"#);
    turn.on_line(
        r#"{"method":"turn/started","params":{"threadId":"thr-1","turn":{"id":"turn-1"}}}"#,
    );
    usage_snapshot(&mut turn, "turn-1", (1000, 100), Some((100, 10)));
    usage_snapshot(&mut turn, "turn-1", (1200, 120), Some((200, 20)));
    usage_snapshot(&mut turn, "turn-1", (50, 5), Some((50, 5)));
    assert_eq!(
        finish_usage(&mut turn),
        vec![
            CodeEvent::Usage {
                cached_input_tokens: None,
                input_tokens: 350,
                output_tokens: 35
            },
            CodeEvent::TurnComplete
        ]
    );
}

#[test]
fn resumed_usage_counts_all_requests_once_and_keeps_cached_subset() {
    let mut turn = AppServerTurn::new(spec(Some("thr-1"), Autonomy::Autonomous));
    open(&mut turn);
    let usage = |id: &str,
                 input: u64,
                 cached: u64,
                 output: u64,
                 last_in: u64,
                 last_cached: u64,
                 last_out: u64| {
        json!({"method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","turnId":id,"tokenUsage":{"total":{"inputTokens":input,"cachedInputTokens":cached,"outputTokens":output},"last":{"inputTokens":last_in,"cachedInputTokens":last_cached,"outputTokens":last_out}}}}).to_string()
    };
    turn.on_line(&usage("stale-turn", 999999, 900000, 9999, 100, 80, 10));
    let first = usage("turn-1", 1300, 1000, 120, 300, 250, 20);
    turn.on_line(&first);
    turn.on_line(&first); // duplicate notification is not extra spend
    turn.on_line(&usage("turn-1", 1650, 1300, 150, 350, 300, 30));
    let end=turn.on_line(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}"#);
    assert!(end.events.contains(&CodeEvent::Usage {
        input_tokens: 650,
        output_tokens: 50,
        cached_input_tokens: Some(550)
    }));
}

#[test]
fn resumed_usage_without_a_baseline_stays_unknown() {
    let mut turn = AppServerTurn::new(spec(Some("thr-1"), Autonomy::Autonomous));
    open(&mut turn);
    turn.on_line(r#"{"method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","turnId":"turn-1","tokenUsage":{"total":{"inputTokens":9000,"outputTokens":100}}}}"#);
    let end=turn.on_line(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}"#);
    assert!(!end
        .events
        .iter()
        .any(|e| matches!(e, CodeEvent::Usage { .. })));
}

#[test]
fn ownership_recovery_never_retries_unrelated_or_failed_forks() {
    for (resumed, message) in [
        (None, "already has an active writer"),
        (Some("old"), "permission denied"),
    ] {
        let mut t = AppServerTurn::new(spec(resumed, Autonomy::AskFirst));
        t.on_line(r#"{"id":1,"result":{}}"#);
        let step = t.on_line(&json!({"id":2,"error":{"message":message}}).to_string());
        assert!(step.send.is_empty() && t.finished());
    }
    for response in [
        json!({"error":{"message":"fork failed"}}),
        json!({"result":{"thread":{"id":"old"}}}),
    ] {
        let mut t = AppServerTurn::new(spec(Some("old"), Autonomy::AskFirst));
        t.on_line(r#"{"id":1,"result":{}}"#);
        t.on_line(r#"{"id":2,"error":{"message":"already has an active writer"}}"#);
        let mut r = response;
        r["id"] = json!(4);
        let failed = t.on_line(&r.to_string());
        assert!(t.finished() && !t.completed() && failed.send.is_empty());
        assert!(failed
            .events
            .iter()
            .any(|e| matches!(e, CodeEvent::Error(_))));
    }
}
