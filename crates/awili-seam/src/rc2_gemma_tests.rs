//! RC2-GEMMA: the in-app engine against a local model server (the Studio's
//! mlx_lm serving Gemma): a system message that stays the same bytes per
//! folder, so the server answers it from its cache.

use super::*;
use awili_engine_log::{test_bundle, MemLog};
use std::collections::VecDeque;

/// The process's one authority bundle, possession cloned (as in `tests`).
fn host() -> HostAuthority {
    let bundle = test_bundle();
    HostAuthority {
        authorizer: Authorizer::new(bundle.0.clone()),
        approval_cap: bundle.1.clone(),
        registry: bundle.2.clone(),
    }
}

/// Answers scripted bodies in order and keeps every request body.
struct Recorder {
    bodies: VecDeque<String>,
    sent: Vec<String>,
}

impl Transport for Recorder {
    fn stream(&mut self, request_body: &str) -> Result<String, SeamError> {
        self.sent.push(request_body.to_string());
        self.bodies
            .pop_front()
            .ok_or_else(|| SeamError::Transport("script exhausted".into()))
    }
}

fn chunk(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

fn sse_tool(id: &str, name: &str, args: &str) -> String {
    let args = serde_json::to_string(args).unwrap();
    let mut out = chunk(&format!(
        r#"{{"choices":[{{"index":0,"delta":{{"content":"","tool_calls":[{{"id":"{id}","index":0,"type":"function","function":{{"name":"{name}","arguments":{args}}}}}]}},"finish_reason":null}}]}}"#
    ));
    out.push_str(&chunk(
        r#"{"choices":[{"index":0,"delta":{"content":""},"finish_reason":"tool_calls"}]}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

fn sse_text(text: &str) -> String {
    let text = serde_json::to_string(text).unwrap();
    let mut out = chunk(&format!(
        r#"{{"choices":[{{"index":0,"delta":{{"content":{text}}},"finish_reason":null}}]}}"#
    ));
    out.push_str(&chunk(
        r#"{"choices":[{"index":0,"delta":{"content":""},"finish_reason":"stop"}]}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

const BLOCK: &str = "<past-context>\nRelevant context from your own memory (background only; \
     standing directives always take precedence):\n- the hostd runs k3s\n</past-context>";

#[test]
fn rc2gemma1_split_past_context_takes_the_recall_block_out() {
    let rules = "Standing directives (always apply, in priority order):\n1. Rule.";
    let brief = "How to answer in Awili:\n- Be brief.";
    // As the app composes it: rules, the block, then the rest.
    let full = format!("{rules}\n\n{BLOCK}\n\n{brief}");
    let (rest, block) = split_past_context(&full);
    assert_eq!(block.as_deref(), Some(BLOCK));
    // The same bytes the app sends when nothing was recalled.
    assert_eq!(rest, format!("{rules}\n\n{brief}"));
    // Rules offline: the block first. Nothing after it: the block last.
    assert_eq!(
        split_past_context(&format!("{BLOCK}\n\n{brief}")),
        (brief.to_string(), Some(BLOCK.to_string()))
    );
    assert_eq!(
        split_past_context(&format!("{rules}\n{BLOCK}")),
        (rules.to_string(), Some(BLOCK.to_string()))
    );
    // No recall, or a fence that is not the preamble's: untouched.
    assert_eq!(split_past_context(brief), (brief.to_string(), None));
    let quoted = "Docs say: <past-context>\nRelevant context from your own memory\n</past-context>";
    assert_eq!(split_past_context(quoted), (quoted.to_string(), None));
    let unclosed = format!("{rules}\n\n<past-context>\nRelevant context from your own memory\n- x");
    assert_eq!(split_past_context(&unclosed), (unclosed.clone(), None));
}

/// The turn's context opens its user message in EVERY request of the turn
/// (a tool round still extends the prompt the server has cached), and the
/// kept history holds the user's words alone once the turn is over.
#[test]
fn rc2gemma1_the_turn_context_opens_every_request_then_leaves_the_history() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "hello").unwrap();
    let host = host();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-gemma").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut transport = Recorder {
        bodies: vec![
            sse_tool("p1", "fs_read", r#"{"path":"a.txt"}"#),
            sse_text("done"),
        ]
        .into(),
        sent: Vec::new(),
    };
    let mut history = ContextHistory::new();
    history.set_preamble(Some("rules".into()));
    history.set_turn_context(Some(BLOCK.into()));
    let mut approver =
        |prompt: &str, _r: RiskClass| -> UserVerdict { panic!("a read asks nothing: {prompt}") };
    let report = run_turn(
        &host,
        &mut transport,
        "test-model",
        root.path().to_str().unwrap(),
        "what is in a.txt?",
        &mut approver,
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        DEFAULT_WINDOW_TOKENS,
        "turn-gemma-0",
        &mut history,
    )
    .expect("the turn completes");
    assert_eq!(report.final_text, "done");
    assert_eq!(transport.sent.len(), 2, "a tool round, then the answer");
    let opened = format!("{BLOCK}\n\nwhat is in a.txt?");
    for (i, body) in transport.sent.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        let m = v["messages"].as_array().unwrap();
        assert_eq!(m[0]["content"], "rules", "request {i}");
        assert_eq!(m[1]["role"], "user", "request {i}");
        assert_eq!(m[1]["content"], opened.as_str(), "request {i}");
    }
    let users: Vec<&str> = history
        .groups()
        .iter()
        .filter_map(|g| match g {
            ContextGroup::User { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(users, vec!["what is in a.txt?"]);
    assert_eq!(history.take_turn_context(), None, "taken once, never kept");
}

// -- RC2-GEMMA3: the reply shows while it arrives ---------------------------

/// Bodies of every shape the decoder meets.
fn shapes() -> Vec<String> {
    let mut v = vec![
        sse_text("hello"),
        sse_tool("p1", "fs_read", r#"{"path":"a.txt"}"#),
        format!(": keepalive 2048/4096\n\n: keepalive 4096/4096\n\n{}", sse_text("ok")),
        // Cut off mid-frame, cut off after a whole event, no [DONE].
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"HERON\"}}]}\n\ndata: {\"cho".into(),
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"HERON\"}}]}\n\n".into(),
        // A malformed complete frame; one event over two data lines; CRLF.
        "data: {not json}\n\ndata: [DONE]\n\n".into(),
        "data: {\"choices\":[{\"index\":0,\ndata: \"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\n".into(),
        sse_text("crlf").replace('\n', "\r\n"),
        // Text after [DONE] is never read.
        format!("{}data: {{\"choices\":[]}}\n\n", sse_text("done")),
    ];
    v.push(String::new());
    v
}

#[test]
fn rc2gemma3_the_decoder_line_by_line_matches_the_whole_body() {
    for raw in shapes() {
        let mut d = SseDecoder::new();
        let mut got = Vec::new();
        for line in raw.lines() {
            got.extend(d.push_line(line));
        }
        got.extend(d.finish());
        if raw.lines().any(|l| l.trim_start().starts_with("data:")) {
            assert_eq!(got, decode_stream(&raw), "{raw:?}");
        }
    }
}

/// A transport that hands the body over line by line, recording at each
/// line how many text events the host had seen by then.
struct Lines {
    body: String,
    seen: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    seen_at_line: Vec<usize>,
}

impl Transport for Lines {
    fn stream(&mut self, _request_body: &str) -> Result<String, SeamError> {
        Ok(self.body.clone())
    }
    fn stream_lines(
        &mut self,
        _request_body: &str,
        on_line: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, SeamError> {
        let mut read = String::new();
        for line in self.body.lines() {
            self.seen_at_line.push(self.seen.borrow().len());
            read.push_str(line);
            read.push('\n');
            if !on_line(line) {
                break;
            }
        }
        Ok(read)
    }
}

/// Records the text the host was shown, its progress lines, and a stop.
struct Watch {
    seen: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    progress: Vec<(u64, u64)>,
    stop_after_text: bool,
}

impl TurnHost for Watch {
    fn approve(&mut self, _id: &str, prompt: &str, _r: RiskClass) -> ApprovalOutcome {
        panic!("no approval expected: {prompt}")
    }
    fn observe(&mut self, event: &AgentEvent) -> Result<(), ObserverGone> {
        if let AgentEvent::TextDelta(t) = event {
            self.seen.borrow_mut().push(t.clone());
        }
        Ok(())
    }
    fn cancelled(&self) -> bool {
        self.stop_after_text && !self.seen.borrow().is_empty()
    }
    fn prompt_progress(&mut self, done: u64, total: u64) {
        self.progress.push((done, total));
    }
}

fn two_part_reply() -> String {
    let part = |t: &str| {
        chunk(&format!(
            r#"{{"choices":[{{"index":0,"delta":{{"content":"{t}"}},"finish_reason":null}}]}}"#
        ))
    };
    let mut body = String::from(": keepalive 2048/4096\n\n");
    body.push_str(&part("first "));
    body.push_str(&part("second"));
    body.push_str(&chunk(
        r#"{"choices":[{"index":0,"delta":{"content":""},"finish_reason":"stop"}]}"#,
    ));
    body.push_str("data: [DONE]\n\n");
    body
}

fn run_lines(
    transport: &mut dyn Transport,
    host_io: &mut dyn TurnHost,
) -> Result<TurnReport, SeamError> {
    let root = tempfile::tempdir().unwrap();
    let host = host();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-gemma3").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut history = ContextHistory::new();
    run_turn(
        &host,
        transport,
        "test-model",
        root.path().to_str().unwrap(),
        "hi",
        host_io,
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        DEFAULT_WINDOW_TOKENS,
        "turn-gemma3-0",
        &mut history,
    )
}

/// The first words reach the host before the rest of the body is read, the
/// server's keepalive becomes progress, and the turn ends as before.
#[test]
fn rc2gemma3_the_reply_shows_while_it_arrives() {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut transport = Lines {
        body: two_part_reply(),
        seen: seen.clone(),
        seen_at_line: Vec::new(),
    };
    let mut watch = Watch {
        seen: seen.clone(),
        progress: Vec::new(),
        stop_after_text: false,
    };
    let report = run_lines(&mut transport, &mut watch).expect("completes");
    assert_eq!(report.final_text, "first second");
    assert_eq!(watch.progress, vec![(2048, 4096)]);
    // Lines: keepalive, blank, part 1, blank, part 2, blank, … The second
    // part's line arrives after the first part was shown.
    assert_eq!(transport.seen_at_line[4], 1, "{:?}", transport.seen_at_line);
    assert_eq!(
        *seen.borrow(),
        vec!["first ".to_string(), "second".to_string()]
    );
}

/// A Stop while the reply arrives ends the read at once: the rest of the
/// body is never read and the turn ends stopped.
#[test]
fn rc2gemma3_a_stop_ends_the_read_mid_reply() {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut transport = Lines {
        body: two_part_reply(),
        seen: seen.clone(),
        seen_at_line: Vec::new(),
    };
    let mut watch = Watch {
        seen: seen.clone(),
        progress: Vec::new(),
        stop_after_text: true,
    };
    let result = run_lines(&mut transport, &mut watch);
    assert!(
        matches!(result, Err(SeamError::Cancelled(StopCause::Cancelled))),
        "{result:?}"
    );
    assert_eq!(*seen.borrow(), vec!["first ".to_string()]);
    assert!(
        transport.seen_at_line.len() < 6,
        "{:?}",
        transport.seen_at_line
    );
}

/// A one-request HTTP server: answers with `head` then the `parts`, waiting
/// `gap` before each part. Returns its base URL.
fn slow_server(parts: Vec<String>, gap: std::time::Duration) -> String {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let (mut c, _) = listener.accept().unwrap();
        let mut buf = [0u8; 65536];
        let mut req = Vec::new();
        // Read the head, then the body by its Content-Length.
        loop {
            let n = c.read(&mut buf).unwrap();
            req.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&req).to_string();
            if let Some(end) = text.find("\r\n\r\n") {
                let len = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if req.len() >= end + 4 + len {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        let _ = c.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        );
        for p in parts {
            std::thread::sleep(gap);
            if c.write_all(p.as_bytes()).is_err() {
                return;
            }
            let _ = c.flush();
        }
    });
    url
}

/// The client's timeout bounds each wait for the next bytes, never the
/// whole reply: a server still reading a long request (a keepalive every
/// gap) answers well after the timeout and the reply is read whole.
#[test]
fn rc2gemma3_a_live_server_is_never_cut_off_by_the_timeout() {
    let mut parts: Vec<String> = (1..=8)
        .map(|i| format!(": keepalive {}/8192\n\n", i * 1024))
        .collect();
    parts.push(sse_text("answered"));
    let url = slow_server(parts, std::time::Duration::from_millis(400));
    let mut t = HttpTransport::new(&url, std::time::Duration::from_secs(2));
    let started = std::time::Instant::now();
    let raw = t.stream("{}").expect("a live server is read to the end");
    assert!(started.elapsed() > std::time::Duration::from_millis(3200));
    assert_eq!(
        decode_stream(&raw).first(),
        Some(&awili_engine::wire::WireDelta::Text("answered".into()))
    );
}

/// Over HTTP the lines are handed over as they arrive: the first text line
/// is seen while the server has not sent the rest yet.
#[test]
fn rc2gemma3_http_lines_arrive_before_the_body_ends() {
    let url = slow_server(
        vec![
            chunk(r#"{"choices":[{"index":0,"delta":{"content":"early"}}]}"#),
            sse_text("late"),
        ],
        std::time::Duration::from_millis(300),
    );
    let mut t = HttpTransport::new(&url, std::time::Duration::from_secs(5));
    let started = std::time::Instant::now();
    let mut first_at = None;
    let raw = t
        .stream_lines("{}", &mut |line| {
            if line.contains("early") && first_at.is_none() {
                first_at = Some(started.elapsed());
            }
            true
        })
        .unwrap();
    let first = first_at.expect("the early line was handed over");
    assert!(
        started.elapsed() >= first + std::time::Duration::from_millis(200),
        "{first:?}"
    );
    assert!(raw.contains("late"));
}

// -- RC2-GEMMA4: a reply the server cut off is asked for again, once --------

/// A text reply whose body ends without its end marker (the R7-A3 shape:
/// the whole answer, then the server closed the connection).
fn sse_cut(text: &str) -> String {
    let text = serde_json::to_string(text).unwrap();
    chunk(&format!(
        r#"{{"choices":[{{"index":0,"delta":{{"content":{text}}},"finish_reason":null}}]}}"#
    ))
}

#[derive(Default)]
struct Cuts {
    cut: Vec<String>,
}

impl TurnHost for Cuts {
    fn approve(&mut self, _id: &str, prompt: &str, _r: RiskClass) -> ApprovalOutcome {
        panic!("no approval expected: {prompt}")
    }
    fn reply_cut(&mut self, shown: &str) {
        self.cut.push(shown.to_string());
    }
}

/// Scripted bodies handed over line by line, as the HTTP transport does.
struct LiveScript {
    bodies: VecDeque<String>,
    sent: Vec<String>,
}

impl Transport for LiveScript {
    fn stream(&mut self, request_body: &str) -> Result<String, SeamError> {
        self.stream_lines(request_body, &mut |_| true)
    }
    fn stream_lines(
        &mut self,
        request_body: &str,
        on_line: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, SeamError> {
        self.sent.push(request_body.to_string());
        let body = self
            .bodies
            .pop_front()
            .ok_or_else(|| SeamError::Transport("script exhausted".into()))?;
        for line in body.lines() {
            if !on_line(line) {
                break;
            }
        }
        Ok(body)
    }
}

fn run_bodies(
    bodies: Vec<String>,
    host_io: &mut Cuts,
) -> (Result<TurnReport, SeamError>, Vec<String>) {
    let mut transport = LiveScript {
        bodies: bodies.into(),
        sent: Vec::new(),
    };
    let result = run_lines(&mut transport, host_io);
    (result, transport.sent)
}

#[test]
fn rc2gemma4_a_cut_text_reply_is_asked_again_once() {
    let mut cuts = Cuts::default();
    let (result, sent) = run_bodies(vec![sse_cut("HERON-77"), sse_text("HERON-77")], &mut cuts);
    let report = result.expect("the second answer completes the turn");
    assert_eq!(
        report.final_text, "HERON-77",
        "the cut text is not kept twice"
    );
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0], sent[1],
        "the same request, answered from the server's cache"
    );
    assert_eq!(cuts.cut, vec!["HERON-77".to_string()]);
}

#[test]
fn rc2gemma4_a_reply_cut_twice_ends_the_turn() {
    let mut cuts = Cuts::default();
    let (result, sent) = run_bodies(vec![sse_cut("HER"), sse_cut("HERON")], &mut cuts);
    match result {
        Err(SeamError::TurnFailed { report, .. }) => {
            assert!(report.events.iter().any(|e| matches!(
                e,
                AgentEvent::TurnFailed(awili_engine::EngineError::UnexpectedTruncation { .. })
            )))
        }
        other => panic!("a second cut ends the turn: {other:?}"),
    }
    assert_eq!(sent.len(), 2, "asked again once, not more");
    assert_eq!(cuts.cut.len(), 1);
}

#[test]
fn rc2gemma4_a_cut_tool_call_is_never_asked_again() {
    let mut cuts = Cuts::default();
    let tool = chunk(
        r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"id":"p1","index":0,"type":"function","function":{"name":"fs_read","arguments":"{\"path\""}}]}}]}"#,
    );
    let (result, sent) = run_bodies(vec![tool, sse_text("never")], &mut cuts);
    assert!(
        matches!(result, Err(SeamError::TurnFailed { .. })),
        "{result:?}"
    );
    assert_eq!(sent.len(), 1);
    assert!(cuts.cut.is_empty());
}

/// mlx_lm's server answers `{"error": "<text>"}` (a string, not an object)
/// once its generation thread died: read as that provider error.
#[test]
fn rc2gemma4_a_string_error_body_is_read() {
    use awili_engine::wire::WireDelta;
    let dead = WireDelta::ProviderError {
        message: "generation thread died".into(),
        retryable: false,
    };
    assert_eq!(
        decode_stream(r#"{"error": "generation thread died"}"#),
        vec![dead.clone()]
    );
    assert_eq!(
        decode_stream("data: {\"error\": \"generation thread died\"}\n\ndata: [DONE]\n\n"),
        vec![dead]
    );
    // The object form reads as before.
    assert_eq!(
        decode_stream(r#"{"error": {"message": "out of memory", "type": "server_error"}}"#),
        vec![WireDelta::ProviderError {
            message: "out of memory".into(),
            retryable: true
        }]
    );
}

// -- RC2-GEMMA6: max_tokens on every round; a refused summary shrinks ---------

/// Every round carries `max_tokens` (a continuation without it got the
/// server's own default: 1024 on the Studio).
#[test]
fn rc2gemma6_every_round_carries_max_tokens() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "hello").unwrap();
    let mut transport = Recorder {
        bodies: vec![
            sse_tool("p1", "fs_read", r#"{"path":"a.txt"}"#),
            sse_text("done"),
        ]
        .into(),
        sent: Vec::new(),
    };
    let mut cuts = Cuts::default();
    run_lines(&mut transport, &mut cuts).expect("completes");
    assert_eq!(transport.sent.len(), 2);
    for body in &transport.sent {
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["max_tokens"], 2048, "{body}");
    }
}

/// A summarizer request the server refuses as too long is sent again with
/// the older half of its material left out (marked), and the summary of
/// what is left is used.
#[test]
fn rc2gemma6_a_refused_summary_request_is_shrunk() {
    let old = "OLD-FACT ".repeat(600);
    let new = "NEW-FACT ".repeat(600);
    let plan = CompactionPlan {
        summarize_ids: vec![],
        to_summarize: vec![ContextGroup::User {
            id: GroupId(1),
            text: format!("{old}{new}"),
        }],
        projected_tokens: 0,
        excluded_continuations: vec![],
    };
    let refusal = r#"{"error": {"message": "This model's maximum context length is 32768 tokens", "type": "invalid_request_error"}}"#;
    let mut transport = Recorder {
        bodies: vec![refusal.to_string(), sse_text("the summary")].into(),
        sent: Vec::new(),
    };
    let mut requests = Vec::new();
    let summary = summarize_plan(&mut transport, "m", &plan, &mut requests).expect("summarized");
    assert_eq!(summary, "the summary");
    assert_eq!(transport.sent.len(), 2);
    let content = |i: usize| -> String {
        let v: serde_json::Value = serde_json::from_str(&transport.sent[i]).unwrap();
        v["messages"][0]["content"].as_str().unwrap().to_string()
    };
    assert!(content(0).contains("OLD-FACT") && content(0).contains("NEW-FACT"));
    assert!(
        !content(1).contains("OLD-FACT"),
        "the older half is left out"
    );
    assert!(content(1).contains("NEW-FACT"));
    assert!(content(1).contains("left out: too long for the model"));
    // Little left: no further shrink, the refusal ends it as before.
    assert_eq!(newer_half("short"), None);
}

// -- RC2-R617: the warm-up reads what the first request begins with ----------

/// RC2-R617 (usability RC2 round 5, Major 1: a cold first Gemma turn took
/// 54 s). The warm-up request begins exactly as the turn's first request
/// does — the same system text, kept turns and tools, byte for byte, with
/// the same wire names — so the model server's cache holds for the turn;
/// only its one-word message and its one-token answer differ. The server's
/// reading progress reaches the host; the history is left as it was.
#[test]
fn rc2r617_the_warm_up_begins_as_the_first_request_does() {
    let root = tempfile::tempdir().unwrap();
    let mut history = ContextHistory::new();
    history.set_preamble(Some("rules\n\nAGENTS.md text\n\nthe brief".into()));
    history.push_user("an earlier question");
    let _ = history.push_exchange("an earlier answer".to_string(), Vec::new(), Vec::new());
    let before = history.clone();

    let mut warm = Lines {
        body: ": keepalive 2048/4096\n\n: keepalive 4096/4096\n\n".to_string() + &sse_text("o"),
        seen: Default::default(),
        seen_at_line: Vec::new(),
    };
    let mut watch = Watch {
        seen: Default::default(),
        progress: Vec::new(),
        stop_after_text: false,
    };
    let mut sent = Recorder {
        bodies: Default::default(),
        sent: Vec::new(),
    };
    struct Both<'a> {
        lines: &'a mut Lines,
        rec: &'a mut Recorder,
    }
    impl Transport for Both<'_> {
        fn stream(&mut self, body: &str) -> Result<String, SeamError> {
            self.rec.sent.push(body.to_string());
            self.lines.stream(body)
        }
        fn stream_lines(
            &mut self,
            body: &str,
            on_line: &mut dyn FnMut(&str) -> bool,
        ) -> Result<String, SeamError> {
            self.rec.sent.push(body.to_string());
            self.lines.stream_lines(body, on_line)
        }
    }
    let warmed = warm_prompt_cache(
        &mut Both {
            lines: &mut warm,
            rec: &mut sent,
        },
        "test-model",
        &[],
        DEFAULT_WINDOW_TOKENS,
        &history,
        &mut watch,
    )
    .expect("the warm-up is sent");
    assert!(warmed);
    assert_eq!(watch.progress, vec![(2048, 4096), (4096, 4096)]);
    assert_eq!(
        history.groups().len(),
        before.groups().len(),
        "the history is unchanged"
    );

    // The turn's first request, from the same history.
    let host = host();
    let mut log = ExecutionLog::open(MemLog::default(), "sess-r617").unwrap();
    let mut grants = ExecutionLog::open_grant_store(MemLog::default()).unwrap();
    let mut turn = Recorder {
        bodies: vec![sse_text("done")].into(),
        sent: Vec::new(),
    };
    let mut approver =
        |prompt: &str, _r: RiskClass| -> UserVerdict { panic!("nothing to approve: {prompt}") };
    run_turn(
        &host,
        &mut turn,
        "test-model",
        root.path().to_str().unwrap(),
        "what is in alpha.txt?",
        &mut approver,
        &mut log,
        &mut grants,
        &[],
        &Executors::builtin_only(),
        &awili_engine_authz::NoInstallAuthority,
        &RootsAcceptance::default(),
        DEFAULT_WINDOW_TOKENS,
        "turn-r617-0",
        &mut history,
    )
    .expect("the turn completes");

    let w: serde_json::Value = serde_json::from_str(&sent.sent[0]).unwrap();
    let t: serde_json::Value = serde_json::from_str(&turn.sent[0]).unwrap();
    assert_eq!(w["tools"], t["tools"], "the same tools, byte for byte");
    assert_eq!(w["model"], t["model"]);
    assert_eq!(w["tool_choice"], t["tool_choice"]);
    let (wm, tm) = (
        w["messages"].as_array().unwrap(),
        t["messages"].as_array().unwrap(),
    );
    assert_eq!(wm.len(), tm.len());
    assert_eq!(
        wm[..wm.len() - 1],
        tm[..tm.len() - 1],
        "the same system text and kept turns"
    );
    assert_eq!(wm.last().unwrap()["content"], WARM_USER);
    assert_eq!(tm.last().unwrap()["content"], "what is in alpha.txt?");
    assert_eq!(w["max_tokens"], 1);
    assert_eq!(t["max_tokens"], 2048);
}

/// RC2-R6M5 (RC2-R6 review P2 on 9c0fa36d: a cut-off answer counted as a
/// warm-up read through). An HTTP 200 whose stream stops before `[DONE]`,
/// or is empty, is no warm-up read: an `Err`, so the app does not count
/// the server warm. A clean answer still is.
#[test]
fn rc2r6m5_a_cut_off_warm_up_answer_is_not_a_read_warm_up() {
    let mut history = ContextHistory::new();
    history.set_preamble(Some("rules\n\nthe brief".into()));
    let full = sse_text("o");
    let cut = full.split("data: [DONE]").next().unwrap().to_string();
    let first_chunk_only = full.split("\n\n").next().unwrap().to_string() + "\n\n";
    let warm = |body: String| {
        let mut transport = Recorder {
            bodies: vec![body].into(),
            sent: Vec::new(),
        };
        let mut watch = Watch {
            seen: Default::default(),
            progress: Vec::new(),
            stop_after_text: false,
        };
        let out = warm_prompt_cache(
            &mut transport,
            "test-model",
            &[],
            DEFAULT_WINDOW_TOKENS,
            &history,
            &mut watch,
        );
        assert_eq!(transport.sent.len(), 1, "the warm-up was sent");
        out
    };
    for (what, body) in [
        ("no [DONE]", cut),
        ("one chunk, then nothing", first_chunk_only),
        ("an empty 200", String::new()),
    ] {
        assert!(
            matches!(warm(body), Err(SeamError::Transport(_))),
            "{what}: not a read warm-up"
        );
    }
    assert!(
        warm(full).expect("a clean answer"),
        "a clean answer is read"
    );
}
