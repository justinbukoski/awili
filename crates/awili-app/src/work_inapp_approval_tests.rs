//! RC1-H-SLICE-FIX1b proof (whole-slice Codex T2 P2 on a2521489): a REAL
//! in-app worker turn — the app's own engine worker over HTTP to a scripted
//! model server — that asks to write a file in "Ask before changes": the
//! approval card is up before anything is written; Allow once writes the file
//! on disk and the footer says "changed hello.txt"; Deny writes nothing and
//! the footer says "no files changed".

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::gateway_queue_tests::{isolated, CONV};
use super::*;

fn chunk(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

/// One model round that calls `fs_write` (the wire name) with `path`/`contents`.
fn write_round(id: &str, path: &str, contents: &str) -> String {
    let args = serde_json::json!({ "path": path, "contents": contents }).to_string();
    let args = serde_json::to_string(&args).unwrap();
    let mut out = chunk(&format!(
        r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":"","tool_calls":[{{"id":"{id}","index":0,"type":"function","function":{{"name":"fs_write","arguments":{args}}}}}]}},"finish_reason":null}}]}}"#
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"tool_calls"}]}"#,
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":10,"total_tokens":20}}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

/// One final text round.
fn text_round(text: &str) -> String {
    let text = serde_json::to_string(text).unwrap();
    let mut out = chunk(&format!(
        r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":{text}}},"finish_reason":null}}]}}"#
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}]}"#,
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

/// A loopback model server that answers each request with the next scripted
/// SSE body (and an error body once the script is used up). Returns its base
/// URL.
fn scripted_model(bodies: Vec<String>) -> String {
    scripted_model_capturing(bodies).0
}

/// RC1-J: [`scripted_model`] that also keeps every request body it was sent
/// (oldest first), so a test can read exactly what the model was shown.
fn scripted_model_capturing(bodies: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let keep = seen.clone();
    let script = Arc::new(Mutex::new(VecDeque::from(bodies)));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut data = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                if let Some(h) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&data[..h]).to_ascii_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if data.len() >= h + 4 + len {
                        break;
                    }
                }
            }
            // RC1-J1-FIX2: a local-model health probe (GET …/models) is
            // answered "up" and takes no scripted reply.
            if data.starts_with(b"GET ") {
                let models = r#"{"object":"list","data":[{"id":"m","object":"model"}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\
                     Content-Length: {}\r\n\r\n{models}",
                    models.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                continue;
            }
            if let Some(h) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                keep.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&data[h + 4..]).into_owned());
            }
            let body = script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| "{\"error\":{\"message\":\"script exhausted\"}}".into());
            // RC1-N-FIX2: `DELAY_MS:<n>\n<body>` answers after n ms (a model
            // still generating when the owner presses Stop).
            let body = match body
                .strip_prefix("DELAY_MS:")
                .and_then(|r| r.split_once('\n'))
            {
                Some((ms, rest)) => {
                    std::thread::sleep(Duration::from_millis(ms.parse().unwrap_or(0)));
                    rest.to_string()
                }
                None => body,
            };
            let (status, body) = body
                .strip_prefix("HTTP_STATUS:")
                .and_then(|s| s.split_once('\n'))
                .map(|(status, body)| (status.to_string(), body.to_string()))
                .unwrap_or(("200 OK".to_string(), body));
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}"), seen)
}

/// Pump the app (and the worker thread) until `done` holds, at most 20 s.
fn wait_for(
    cx: &mut gpui::TestAppContext,
    entity: &gpui::Entity<AwiliApp>,
    what: &str,
    done: impl Fn(&AwiliApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if entity.update(cx, |app, _| done(app)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn last_footer(app: &AwiliApp) -> String {
    app.views[CONV]
        .rows
        .last()
        .and_then(|r| r.footer.as_deref().map(str::to_string))
        .expect("the reply has a footer")
}

/// The real in-app path in "Ask before changes": approve one write, deny one.
#[gpui::test]
fn rc1h_slice_fix1_in_app_write_goes_through_the_approval_card(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::inapp_approval_tests::rc1h_slice_fix1_in_app_write_goes_through_the_approval_card",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let base = scripted_model(vec![
        write_round("w1", "hello.txt", "hi\n"),
        text_round("Done."),
        write_round("w2", "second.txt", "ok\n"),
        text_round("I did not write second.txt."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(CONV.into()),
            awili_work::ApprovalMode::AskBeforeChanges,
        );
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run("Create a file named hello.txt containing hi.".into(), cx);
    });

    // 1. The card is up before anything is written; Allow once writes it.
    wait_for(cx, &entity, "the first approval card", |app| {
        app.work.panel.approval.is_some()
    });
    entity.update(cx, |app, cx| {
        let card = app.work.panel.approval.as_ref().unwrap();
        // RC1-I3 (amended): the card says it in plain words; RC2-R1F1: it
        // says it is a file and names the whole path.
        let want = format!(
            "Write the file {}/hello.txt (3 characters)",
            folder.display()
        );
        assert!(card.prompt.contains(&want), "{}", card.prompt);
        assert!(
            !folder.join("hello.txt").exists(),
            "nothing written before the OK"
        );
        app.resolve_work_approval(ApprovalDecision::Approve, cx);
    });
    wait_for(cx, &entity, "the first turn to end", |app| {
        !app.work.running
    });
    assert_eq!(
        std::fs::read_to_string(folder.join("hello.txt")).unwrap(),
        "hi\n",
        "Allow once wrote the file"
    );
    entity.update(cx, |app, _cx| {
        let footer = last_footer(app);
        assert!(footer.ends_with(" \u{b7} changed hello.txt"), "{footer}");
    });

    // 2. Deny: no file, and the footer says nothing changed.
    entity.update(cx, |app, cx| {
        app.submit_work_run("Create a file named second.txt containing ok.".into(), cx);
    });
    wait_for(cx, &entity, "the second approval card", |app| {
        app.work.panel.approval.is_some()
    });
    entity.update(cx, |app, cx| {
        assert!(!folder.join("second.txt").exists());
        app.resolve_work_approval(ApprovalDecision::Deny, cx);
    });
    wait_for(cx, &entity, "the second turn to end", |app| {
        !app.work.running
    });
    assert!(!folder.join("second.txt").exists(), "Deny wrote nothing");
    entity.update(cx, |app, cx| {
        let footer = last_footer(app);
        assert!(footer.ends_with(" \u{b7} no files changed"), "{footer}");
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// RC1-I2 (RC1 round 3b, E2-5b): "Allow for this session" on one in-app write
/// covers the next write in the SAME conversation (no card; the file is
/// written; the footer says so) and never reaches ANOTHER conversation in the
/// same folder (a card again).
#[gpui::test]
fn rc1i2_allow_for_session_covers_this_conversation_only(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::inapp_approval_tests::rc1i2_allow_for_session_covers_this_conversation_only",
    ) else {
        return;
    };
    const OTHER: &str = "work-conv-other";
    cx.executor().allow_parking();
    let base = scripted_model(vec![
        write_round("s1", "session1.txt", "ok\n"),
        text_round("Wrote session1.txt."),
        write_round("s2", "session2.txt", "ok\n"),
        text_round("Wrote session2.txt."),
        write_round("s3", "session3.txt", "ok\n"),
        text_round("I did not write session3.txt."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        for conv in [CONV, OTHER] {
            app.work.ask_modes.set(
                crate::ask_mode::AskKey::Work(conv.into()),
                awili_work::ApprovalMode::AskBeforeChanges,
            );
            app.views.insert(
                conv.into(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
        }
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        app.set_active_conversation(CONV);
        app.submit_work_run("Create session1.txt containing ok.".into(), cx);
    });

    // 1. Allow for this session.
    wait_for(cx, &entity, "the first card", |app| {
        app.work.panel.approval.is_some()
    });
    entity.update(cx, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Session, cx);
    });
    wait_for(cx, &entity, "the first turn to end", |app| {
        !app.work.running
    });
    assert!(folder.join("session1.txt").exists());

    // 2. Same conversation: no card, the file is written.
    let asked = std::cell::Cell::new(false);
    entity.update(cx, |app, cx| {
        app.submit_work_run("Create session2.txt containing ok.".into(), cx);
    });
    wait_for(cx, &entity, "the second turn to end", |app| {
        if app.work.panel.approval.is_some() {
            asked.set(true);
        }
        !app.work.running
    });
    assert!(!asked.get(), "the session grant covers the second write");
    assert_eq!(
        std::fs::read_to_string(folder.join("session2.txt")).unwrap(),
        "ok\n"
    );
    entity.update(cx, |app, _cx| {
        let footer = last_footer(app);
        assert!(footer.ends_with(" \u{b7} changed session2.txt"), "{footer}");
    });

    // 3. Another conversation in the same folder: a card again.
    entity.update(cx, |app, cx| {
        app.set_active_conversation(OTHER);
        app.submit_work_run("Create session3.txt containing ok.".into(), cx);
    });
    wait_for(cx, &entity, "the card in the other conversation", |app| {
        app.work.panel.approval.is_some()
    });
    assert!(!folder.join("session3.txt").exists());
    entity.update(cx, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Deny, cx);
    });
    wait_for(cx, &entity, "the third turn to end", |app| {
        !app.work.running
    });
    assert!(!folder.join("session3.txt").exists());
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// RC1-I2-FIX1 (Codex T2 P1 on 237537a): a session ends with its worker. In
/// conversation A "Allow for this session"; switch to B (A's worker is
/// retired); back in A a NEW worker reopens A's log — and the next write asks
/// again (the old grant belongs to the ended session).
#[gpui::test]
fn rc1i2_fix1_a_session_ends_with_its_worker(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_approval_tests::rc1i2_fix1_a_session_ends_with_its_worker")
    else {
        return;
    };
    const OTHER: &str = "work-conv-b";
    cx.executor().allow_parking();
    let base = scripted_model(vec![
        write_round("a1", "a1.txt", "ok\n"),
        text_round("Wrote a1.txt."),
        // RC1-N: an in-app Deny ends the turn, so no text round follows a
        // denied write.
        write_round("b1", "b1.txt", "ok\n"),
        write_round("a2", "a2.txt", "ok\n"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        for conv in [CONV, OTHER] {
            app.work.ask_modes.set(
                crate::ask_mode::AskKey::Work(conv.into()),
                awili_work::ApprovalMode::AskBeforeChanges,
            );
            app.views.insert(
                conv.into(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
        }
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        app.set_active_conversation(CONV);
        app.submit_work_run("Create a1.txt containing ok.".into(), cx);
    });
    wait_for(cx, &entity, "A's card", |app| {
        app.work.panel.approval.is_some()
    });
    entity.update(cx, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Session, cx);
    });
    wait_for(cx, &entity, "A's first turn", |app| !app.work.running);
    assert!(folder.join("a1.txt").exists());
    // The chat line says, in plain words, how long the permission lasts.
    entity.update(cx, |app, _cx| {
        let said = app.views[CONV].convo.messages().iter().any(|m| {
            m.text
                .contains("until you switch to another conversation or quit Awili")
        });
        assert!(said, "the outcome line names the session's end");
    });

    for (conv, file, what) in [
        (OTHER, "b1.txt", "B's card"),
        (CONV, "a2.txt", "A's card again"),
    ] {
        entity.update(cx, |app, cx| {
            app.set_active_conversation(conv);
            app.submit_work_run(format!("Create {file} containing ok."), cx);
        });
        wait_for(cx, &entity, what, |app| {
            app.work.panel.approval.is_some() || !app.work.running
        });
        let asked = entity.update(cx, |app, _| app.work.panel.approval.is_some());
        assert!(asked, "{what}: a new worker is a new session, so it asks");
        entity.update(cx, |app, cx| {
            app.resolve_work_approval(ApprovalDecision::Deny, cx);
        });
        wait_for(cx, &entity, "the turn to end", |app| !app.work.running);
        assert!(!folder.join(file).exists());
    }
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// RC1-I2-FIX1 (Codex T2 P2 on 237537a): a memory gate that REFUSES a prompt
/// for conversation Y leaves the live worker of conversation X alone — no
/// shutdown, no prompt, its session kept.
#[test]
fn rc1i2_fix1_a_refused_gate_keeps_the_live_worker() {
    let (tx, rx) = std::sync::mpsc::channel::<GatewayCmd>();
    let mut work = WorkState::new(
        HermesCfg {
            base_url: String::new(),
            api_key: String::new(),
            enabled: false,
            dashboard_url: String::new(),
        },
        GatewayCfg {
            dashboard_url: String::new(),
            enabled: false,
        },
        Default::default(),
    );
    work.gateway_cmd_tx = Some(tx);
    work.worker_conv = Some("conv-x".into());
    work.session_id = Some("sess-x".into());
    let r = hand_off_gated_work_prompt(&mut work, "conv-y", "task", Err("rejected".into()));
    assert!(matches!(r, GatedWorkPrompt::Refused(_)));
    assert!(rx.try_recv().is_err(), "nothing reached the live worker");
    assert!(work.gateway_cmd_tx.is_some(), "the live worker is kept");
    assert_eq!(work.worker_conv.as_deref(), Some("conv-x"));
    assert_eq!(work.session_id.as_deref(), Some("sess-x"));
    // An ACCEPTED gate for another conversation does retire it.
    let r = hand_off_gated_work_prompt(&mut work, "conv-y", "task", Ok(None));
    assert!(matches!(r, GatedWorkPrompt::NeedsWorker(_)), "{r:?}");
    assert!(
        matches!(rx.try_recv(), Ok(GatewayCmd::Shutdown)),
        "an accepted prompt for another conversation retires the worker"
    );
    assert!(work.gateway_cmd_tx.is_none());
}

/// RC1-I2-FIX2: a Work app in a real GPUI window (conversation switches go
/// through `select_conversation`, the sidebar's own path), in-app engine at
/// `base`, Ask mode for `convs`.
fn windowed_in_app<'a>(
    cx: &'a mut gpui::TestAppContext,
    home: &std::path::Path,
    folder: &std::path::Path,
    base: &str,
    convs: &[&str],
) -> (gpui::Entity<AwiliApp>, &'a mut gpui::VisualTestContext) {
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, _cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.to_path_buf());
        app.work.engine.base_url = base.to_string();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        for conv in convs {
            let mut metas = app.conversations.all().to_vec();
            metas.push(awili_core::ConversationMeta {
                id: (*conv).into(),
                title: "Permission fixture".into(),
                created_ts: 1,
                updated_ts: 1,
                archived: false,
                title_is_custom: false,
                cwd: Some(folder.display().to_string()),
                profile: None,
                kind: awili_core::ConvKind::Work,
            });
            app.conversations = awili_core::ConversationList::from_items(metas);
            app.search_store
                .as_ref()
                .unwrap()
                .create_conversation_with_kind(conv, "Permission fixture", 1, "work")
                .unwrap();
            app.work.ask_modes.set(
                crate::ask_mode::AskKey::Work((*conv).into()),
                awili_work::ApprovalMode::AskBeforeChanges,
            );
            app.views.insert(
                (*conv).into(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
        }
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        app.set_active_conversation(convs[0]);
    });
    (view, window)
}

fn wait_in_window(
    view: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
    what: &str,
    done: impl Fn(&AwiliApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        window.run_until_parked();
        if view.update(window, |app, _| done(app)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn switch_to(view: &gpui::Entity<AwiliApp>, window: &mut gpui::VisualTestContext, conv: &str) {
    window.update(|win, cx| {
        view.update(cx, |app, cx| app.select_conversation(conv, win, cx));
    });
    window.run_until_parked();
}

/// RC1-I2-FIX2 (Codex T2 P1 on 81cf808): A → B → A with NO prompt in B ends
/// A's session — the next write in A asks again.
#[gpui::test]
fn rc1i2_fix2_switching_away_ends_the_session(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_approval_tests::rc1i2_fix2_switching_away_ends_the_session")
    else {
        return;
    };
    cx.executor().allow_parking();
    let base = scripted_model(vec![
        write_round("a1", "a1.txt", "ok\n"),
        text_round("Wrote a1.txt."),
        write_round("a2", "a2.txt", "ok\n"),
        text_round("I did not write a2.txt."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let (view, window) = windowed_in_app(cx, &home, &folder, &base, &[CONV, "work-conv-b"]);
    view.update(window, |app, cx| {
        app.submit_work_run("Create a1.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "A's card", |app| {
        app.work.panel.approval.is_some()
    });
    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Session, cx)
    });
    wait_in_window(&view, window, "A's first turn", |app| !app.work.running);
    assert!(folder.join("a1.txt").exists());

    switch_to(&view, window, "work-conv-b");
    switch_to(&view, window, CONV);
    view.update(window, |app, cx| {
        app.submit_work_run("Create a2.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "A's next turn", |app| {
        app.work.panel.approval.is_some() || !app.work.running
    });
    assert!(
        view.update(window, |app, _| app.work.panel.approval.is_some()),
        "switching away ended the session, so the next write asks"
    );
    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Deny, cx)
    });
    wait_in_window(&view, window, "the turn to end", |app| !app.work.running);
    assert!(!folder.join("a2.txt").exists());
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
}

/// RC1-I2-FIX2: switching away never stops a running run. A's run waits on its
/// card; the user switches to B — the run and its worker stay; back in A the
/// card is answered and the run finishes (the file is written); THEN the
/// session ends, and A's next write asks again.
#[gpui::test]
fn rc1i2_fix2_a_running_run_keeps_going_after_a_switch(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_approval_tests::rc1i2_fix2_a_running_run_keeps_going_after_a_switch")
    else {
        return;
    };
    cx.executor().allow_parking();
    let base = scripted_model(vec![
        write_round("r1", "r1.txt", "ok\n"),
        text_round("Wrote r1.txt."),
        write_round("r2", "r2.txt", "ok\n"),
        text_round("I did not write r2.txt."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let (view, window) = windowed_in_app(cx, &home, &folder, &base, &[CONV, "work-conv-b"]);
    view.update(window, |app, cx| {
        app.submit_work_run("Create r1.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "A's card", |app| {
        app.work.panel.approval.is_some()
    });
    // Switch away while the run waits on its card.
    switch_to(&view, window, "work-conv-b");
    view.update(window, |app, _| {
        assert!(app.work.running, "the run keeps going");
        assert!(
            app.work.gateway_cmd_tx.is_some(),
            "its worker is not stopped"
        );
    });
    switch_to(&view, window, CONV);
    // "Allow for this session": a kept session would skip the next card.
    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Session, cx)
    });
    wait_in_window(&view, window, "A's run to finish", |app| !app.work.running);
    assert_eq!(
        std::fs::read_to_string(folder.join("r1.txt")).unwrap(),
        "ok\n",
        "the run that was going finished its write"
    );
    view.update(window, |app, _| {
        let said = app.views[CONV].convo.messages().iter().any(|m| {
            m.text
                .contains("or quit Awili (a run that is still going finishes first)")
        });
        assert!(said, "the chat line says a running run finishes first");
    });
    view.update(window, |app, cx| {
        app.submit_work_run("Create r2.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "A's next turn", |app| {
        app.work.panel.approval.is_some() || !app.work.running
    });
    assert!(
        view.update(window, |app, _| app.work.panel.approval.is_some()),
        "after the run that was going, the session ended: the next write asks"
    );
    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Deny, cx)
    });
    wait_in_window(&view, window, "the turn to end", |app| !app.work.running);
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
}

/// RC1-I2-FIX3: the conversations a switch-path test moves between — real
/// rows in the conversation list, so mode switches and history find them.
pub(super) struct SwitchConvs {
    a: String,
    b: String,
    code: String,
}

/// RC1-I2-FIX3: a Work app in a real GPUI window on in-app engine `base`, in
/// Work conversation A (Ask mode), with a second Work conversation B, a Chat
/// conversation, and a Code conversation that a Code tab shows.
pub(super) fn windowed_switch_app<'a>(
    cx: &'a mut gpui::TestAppContext,
    home: &std::path::Path,
    folder: &std::path::Path,
    base: &str,
) -> (
    gpui::Entity<AwiliApp>,
    &'a mut gpui::VisualTestContext,
    SwitchConvs,
) {
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    let convs = view.update(window, |app, _cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.to_path_buf());
        app.work.engine.base_url = base.to_string();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        app.work.default_helper = None;
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        let mut make = |kind: ConvKind| {
            let id = app.conversations.create_kind(1_000, kind).id;
            app.views.insert(
                id.clone(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
            id
        };
        let a = make(ConvKind::Work);
        let b = make(ConvKind::Work);
        let chat = make(ConvKind::Chat);
        let code = make(ConvKind::Code);
        for conv in [&a, &b] {
            app.work.ask_modes.set(
                crate::ask_mode::AskKey::Work(conv.clone()),
                awili_work::ApprovalMode::AskBeforeChanges,
            );
        }
        app.new_code_tab(CodeEngine::Claude);
        let tab = app.work.code_tabs.len() - 1;
        app.work.code_tabs[tab].history.conv_id = Some(code.clone());
        app.set_active_conversation(&a);
        app.sidebar.last_work_id = Some(a.clone());
        app.sidebar.last_chat_id = Some(chat);
        SwitchConvs { a, b, code }
    });
    (view, window, convs)
}

/// RC1-I2-FIX3: every known way of changing the active conversation, as the
/// UI does it: (name, leave A, come back to A).
type SwitchStep = fn(&mut AwiliApp, &SwitchConvs, &mut gpui::Window, &mut Context<AwiliApp>);
fn switch_paths() -> Vec<(&'static str, SwitchStep, SwitchStep)> {
    vec![
        (
            "Work sidebar",
            |app, c, w, cx| app.select_conversation(&c.b, w, cx),
            |app, c, w, cx| app.select_conversation(&c.a, w, cx),
        ),
        (
            "Work history",
            |app, c, _w, cx| app.open_conversation_from_history(&c.b, cx),
            |app, c, _w, cx| app.open_conversation_from_history(&c.a, cx),
        ),
        (
            "new conversation",
            |app, _c, w, cx| app.new_conversation(w, cx),
            |app, c, w, cx| app.select_conversation(&c.a, w, cx),
        ),
        (
            "Code sidebar",
            |app, c, w, cx| app.select_conversation(&c.code, w, cx),
            |app, _c, _w, cx| app.set_mode(Mode::Work, cx),
        ),
        (
            "Code history reopen",
            |app, c, _w, cx| app.open_conversation_from_history(&c.code, cx),
            |app, _c, _w, cx| app.set_mode(Mode::Work, cx),
        ),
        (
            "Code tab",
            |app, c, _w, cx| {
                app.set_mode(Mode::Code, cx);
                let tab = crate::code_reopen::tab_for_conv(&app.work.code_tabs, &c.code)
                    .expect("a Code tab shows the Code conversation");
                app.select_code_tab(tab);
            },
            |app, _c, _w, cx| app.set_mode(Mode::Work, cx),
        ),
    ]
}

fn take_step(
    view: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
    convs: &SwitchConvs,
    step: SwitchStep,
) {
    window.update(|win, cx| {
        view.update(cx, |app, cx| {
            // The local model was just seen up, so entering Work does not
            // probe it (a probe would take a scripted reply).
            app.work.local.health.finish_probe(Ok(()), Instant::now());
            step(app, convs, win, cx)
        })
    });
    window.run_until_parked();
}

/// RC1-I2-FIX3 (Codex T2 P1 on 7273fff): EVERY way of leaving conversation A
/// ends A's session — Work sidebar, Work history, new
/// conversation, Code sidebar, Code history reopen and Code tab selection. For
/// each: A's write is allowed for the session, A → elsewhere → A, and A's next
/// write shows a card again.
#[gpui::test]
fn rc1i2_fix3_every_switch_path_ends_the_session(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_approval_tests::rc1i2_fix3_every_switch_path_ends_the_session")
    else {
        return;
    };
    cx.executor().allow_parking();
    let paths = switch_paths();
    let mut script = Vec::new();
    for i in 0..paths.len() {
        script.push(write_round(&format!("s{i}"), &format!("s{i}.txt"), "ok\n"));
        script.push(text_round("Wrote it."));
        // RC1-N: an in-app Deny ends the turn, so the model is not asked
        // again after it (no text round follows the denied write).
        script.push(write_round(&format!("t{i}"), &format!("t{i}.txt"), "ok\n"));
    }
    let base = scripted_model(script);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let (view, window, convs) = windowed_switch_app(cx, &home, &folder, &base);
    for (i, (name, leave, back)) in paths.into_iter().enumerate() {
        view.update(window, |app, cx| {
            app.submit_work_run(format!("Create s{i}.txt containing ok."), cx)
        });
        wait_in_window(&view, window, &format!("{name}: A's card"), |app| {
            app.work.panel.approval.is_some()
        });
        view.update(window, |app, cx| {
            app.resolve_work_approval(ApprovalDecision::Session, cx)
        });
        wait_in_window(&view, window, &format!("{name}: A's turn"), |app| {
            !app.work.running
        });
        assert!(folder.join(format!("s{i}.txt")).exists(), "{name}");

        take_step(&view, window, &convs, leave);
        view.update(window, |app, _| {
            assert_ne!(app.sidebar.active_id(), convs.a, "{name}: A was left");
        });
        take_step(&view, window, &convs, back);
        view.update(window, |app, _| {
            assert_eq!(app.sidebar.active_id(), convs.a, "{name}: back in A");
            assert_eq!(app.mode, Mode::Work, "{name}: back in Work");
        });

        view.update(window, |app, cx| {
            app.submit_work_run(format!("Create t{i}.txt containing ok."), cx)
        });
        wait_in_window(&view, window, &format!("{name}: A's next turn"), |app| {
            app.work.panel.approval.is_some() || !app.work.running
        });
        assert!(
            view.update(window, |app, _| app.work.panel.approval.is_some()),
            "{name}: leaving A ended its session, so the next write asks"
        );
        view.update(window, |app, cx| {
            app.resolve_work_approval(ApprovalDecision::Deny, cx)
        });
        wait_in_window(&view, window, &format!("{name}: the turn to end"), |app| {
            !app.work.running
        });
        assert!(!folder.join(format!("t{i}.txt")).exists(), "{name}");
    }
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
}

/// RC1-I2-FIX3: no way of switching stops a running run. A's run waits on its
/// card while the user takes every switch path away and back; the run and its
/// worker stay. The card is then answered "Allow for this session", the run
/// finishes its write, and only THEN does the session end: A's next write
/// asks again.
#[gpui::test]
fn rc1i2_fix3_no_switch_path_stops_a_running_run(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_approval_tests::rc1i2_fix3_no_switch_path_stops_a_running_run")
    else {
        return;
    };
    cx.executor().allow_parking();
    let base = scripted_model(vec![
        write_round("r1", "r1.txt", "ok\n"),
        text_round("Wrote r1.txt."),
        write_round("r2", "r2.txt", "ok\n"),
        text_round("I did not write r2.txt."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let (view, window, convs) = windowed_switch_app(cx, &home, &folder, &base);
    view.update(window, |app, cx| {
        app.submit_work_run("Create r1.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "A's card", |app| {
        app.work.panel.approval.is_some()
    });
    for (name, leave, back) in switch_paths() {
        take_step(&view, window, &convs, leave);
        view.update(window, |app, _| {
            assert_ne!(app.sidebar.active_id(), convs.a, "{name}: A was left");
            assert!(app.work.running, "{name}: the run keeps going");
            assert!(
                app.work.gateway_cmd_tx.is_some(),
                "{name}: its worker is not stopped"
            );
        });
        take_step(&view, window, &convs, back);
        view.update(window, |app, _| {
            assert_eq!(app.sidebar.active_id(), convs.a, "{name}: back in A");
            assert!(app.work.running, "{name}: the run keeps going");
        });
    }
    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Session, cx)
    });
    wait_in_window(&view, window, "A's run to finish", |app| !app.work.running);
    assert_eq!(
        std::fs::read_to_string(folder.join("r1.txt")).unwrap(),
        "ok\n",
        "the run that was going finished its write"
    );
    view.update(window, |app, cx| {
        app.submit_work_run("Create r2.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "A's next turn", |app| {
        app.work.panel.approval.is_some() || !app.work.running
    });
    assert!(
        view.update(window, |app, _| app.work.panel.approval.is_some()),
        "after the run that was going, the session ended: the next write asks"
    );
    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Deny, cx)
    });
    wait_in_window(&view, window, "the turn to end", |app| !app.work.running);
    assert!(!folder.join("r2.txt").exists());
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
}

/// A live-gate line: to stderr and, when AWILI_LIVE_LOG names a file, there.
pub(super) fn live_log(line: &str) {
    eprintln!("{line}");
    if let Ok(path) = std::env::var("AWILI_LIVE_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Pump the window until `done` holds, at most `secs` (live model turns).
pub(super) fn wait_long(
    view: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
    secs: u64,
    what: &str,
    done: impl Fn(&AwiliApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        window.run_until_parked();
        if view.update(window, |app, _| done(app)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// RC1-J [live gate]: repeat file writes in ONE Work conversation against the
/// real in-app model, the way round 3c used it — each write is followed by a
/// switch away and back (the conversation's worker is retired and the next
/// one is built from the conversation). Every write must reach the file tool
/// (a card, then the file on disk). Runs only with AWILI_LIVE_MODEL_URL (e.g.
/// http://10.77.68.94:8844) and AWILI_LIVE_MODEL (the served model id) set;
/// otherwise it returns at once.
#[gpui::test]
fn rc1j_live_repeat_writes_call_the_tool(cx: &mut gpui::TestAppContext) {
    live_repeat_writes(
        cx,
        "work::inapp_approval_tests::rc1j_live_repeat_writes_call_the_tool",
        LiveMode::Normal,
    );
}

/// RC1-J [live gate]: the same, as after a relaunch before every write — the
/// in-memory history is gone, so each new worker reads the kept history back
/// from its file.
#[gpui::test]
fn rc1j_live_repeat_writes_after_relaunch_call_the_tool(cx: &mut gpui::TestAppContext) {
    live_repeat_writes(
        cx,
        "work::inapp_approval_tests::rc1j_live_repeat_writes_after_relaunch_call_the_tool",
        LiveMode::Relaunched,
    );
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum LiveMode {
    Normal,
    Relaunched,
}

fn live_repeat_writes(cx: &mut gpui::TestAppContext, test: &str, mode: LiveMode) {
    let (Ok(url), Ok(model)) = (
        std::env::var("AWILI_LIVE_MODEL_URL"),
        std::env::var("AWILI_LIVE_MODEL"),
    ) else {
        return;
    };
    let Some(home) = isolated(test) else {
        return;
    };
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let (view, window, convs) = windowed_switch_app(cx, &home, &folder, &url);
    view.update(window, |app, _| {
        app.work.engine.model = model.clone();
        app.work.engine.label = "Gemma (live)".into();
        app.work.engine.request_timeout_s = 300;
    });
    let rounds = 7;
    let mut called = Vec::new();
    for i in 1..=rounds {
        let name = format!("session{i}.txt");
        if mode != LiveMode::Normal {
            view.update(window, |app, _| app.work.kept_histories.clear());
        }
        view.update(window, |app, cx| {
            app.submit_work_run(
                format!("Create a file named {name} with the content: ok"),
                cx,
            )
        });
        wait_long(&view, window, 600, &format!("{name}: card or end"), |app| {
            app.work.panel.approval.is_some() || !app.work.running
        });
        if view.update(window, |app, _| app.work.panel.approval.is_some()) {
            view.update(window, |app, cx| {
                app.resolve_work_approval(ApprovalDecision::Session, cx)
            });
            wait_long(&view, window, 600, &format!("{name}: end"), |app| {
                !app.work.running
            });
        }
        let reply = view.update(window, |app, _| {
            app.views[&convs.a]
                .convo
                .messages()
                .last()
                .map(|m| m.text.clone())
                .unwrap_or_default()
        });
        let wrote = folder.join(&name).exists();
        live_log(&format!(
            "RC1J-LIVE {mode:?} {i}/{rounds} {name}: tool={wrote} reply={reply:?}"
        ));
        called.push(wrote);
        let (_, leave, back) = switch_paths().remove(0);
        take_step(&view, window, &convs, leave);
        take_step(&view, window, &convs, back);
    }
    let n = called.iter().filter(|c| **c).count();
    live_log(&format!(
        "RC1J-LIVE RESULT {mode:?} {n}/{rounds} writes reached the file tool"
    ));
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
    assert_eq!(
        n, rounds,
        "every repeat write reaches the file tool: {called:?}"
    );
}

/// RC1-J: the `messages` of one captured model request.
fn request_messages(body: &str) -> Vec<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(body).expect("a JSON request");
    v["messages"].as_array().cloned().unwrap_or_default()
}

/// RC1-J: no Awili line (card notice, outcome line) in any assistant message.
fn assert_no_awili_lines(msgs: &[serde_json::Value]) {
    for m in msgs.iter().filter(|m| m["role"] == "assistant") {
        let text = m["content"].as_str().unwrap_or_default();
        assert!(
            !text.contains("Approval needed") && !text.contains("Allowed for this session"),
            "Awili's words shown to the model as its own: {text:?}"
        );
    }
}

/// RC1-J: does the request show the model its own earlier `s1` write call
/// and that call's result?
fn shows_the_s1_tool_call(msgs: &[serde_json::Value]) -> bool {
    let call = msgs.iter().any(|m| {
        m["role"] == "assistant"
            && m["tool_calls"]
                .as_array()
                .is_some_and(|c| c.iter().any(|c| c["id"] == "s1"))
    });
    let result = msgs
        .iter()
        .any(|m| m["role"] == "tool" && m["tool_call_id"] == "s1");
    call && result
}

/// RC1-J: in conversation A, write s1.txt ("Allow for this session"), switch
/// away and back (A's worker is retired), let `between` run, then ask for
/// s2.txt and return what the NEW worker sent the model.
fn second_worker_request(
    cx: &mut gpui::TestAppContext,
    test: &str,
    between: impl Fn(&mut AwiliApp, &SwitchConvs),
) -> Option<Vec<serde_json::Value>> {
    let home = isolated(test)?;
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        write_round("s1", "s1.txt", "ok\n"),
        text_round("Wrote s1.txt."),
        text_round("Not writing s2.txt."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let (view, window, convs) = windowed_switch_app(cx, &home, &folder, &base);
    view.update(window, |app, cx| {
        app.submit_work_run("Create s1.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "s1's card", |app| {
        app.work.panel.approval.is_some()
    });
    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Session, cx)
    });
    wait_in_window(&view, window, "the s1 turn", |app| !app.work.running);
    assert!(folder.join("s1.txt").exists());
    let (_, leave, back) = switch_paths().remove(0);
    take_step(&view, window, &convs, leave);
    take_step(&view, window, &convs, back);
    view.update(window, |app, _| between(app, &convs));
    let before = seen.lock().unwrap().len();
    view.update(window, |app, cx| {
        app.submit_work_run("What is in s1.txt?".into(), cx)
    });
    wait_in_window(&view, window, "the next turn", |app| !app.work.running);
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), before + 1, "one request for the next turn");
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
    Some(request_messages(&bodies[before]))
}

/// RC1-J (round 3c Major 1): after a switch retired A's worker, A's NEW
/// worker shows the model exactly its own past — its s1 write call and that
/// call's result — and never Awili's card or outcome lines as its words.
#[gpui::test]
fn rc1j_a_new_worker_is_shown_the_models_own_history(cx: &mut gpui::TestAppContext) {
    let Some(msgs) = second_worker_request(
        cx,
        "work::inapp_approval_tests::rc1j_a_new_worker_is_shown_the_models_own_history",
        |_, _| {},
    ) else {
        return;
    };
    assert!(shows_the_s1_tool_call(&msgs), "{msgs:#?}");
    assert_no_awili_lines(&msgs);
}

/// RC1-J: a relaunch loses the in-memory history; the new worker reads the
/// kept history back from its file. Deleting the conversation deletes it.
#[gpui::test]
fn rc1j_after_a_relaunch_the_kept_history_is_read_back(cx: &mut gpui::TestAppContext) {
    let Some(msgs) = second_worker_request(
        cx,
        "work::inapp_approval_tests::rc1j_after_a_relaunch_the_kept_history_is_read_back",
        |app, c| {
            assert!(
                super::inapp_history::load(&c.a).is_some(),
                "the history is kept on disk"
            );
            app.work.kept_histories.clear();
        },
    ) else {
        return;
    };
    assert!(shows_the_s1_tool_call(&msgs), "{msgs:#?}");
    assert_no_awili_lines(&msgs);
}

/// RC1-J: deleting a conversation deletes the model history kept for it.
#[gpui::test]
fn rc1j_deleting_a_conversation_forgets_its_history(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_approval_tests::rc1j_deleting_a_conversation_forgets_its_history")
    else {
        return;
    };
    cx.executor().allow_parking();
    let base = scripted_model(vec![text_round("Hello.")]);
    let dir = tempfile::tempdir().unwrap();
    let (view, window, convs) = windowed_switch_app(cx, &home, dir.path(), &base);
    view.update(window, |app, cx| {
        app.submit_work_run("Say hello.".into(), cx)
    });
    wait_in_window(&view, window, "the turn", |app| !app.work.running);
    assert!(super::inapp_history::load(&convs.a).is_some(), "kept");
    window.update(|win, cx| view.update(cx, |app, cx| app.delete_conversation(&convs.a, win, cx)));
    window.run_until_parked();
    assert!(super::inapp_history::load(&convs.a).is_none(), "forgotten");
    view.update(window, |app, _| {
        assert!(!app.work.kept_histories.contains_key(&convs.a));
        app.work.gateway_cmd_tx = None;
    });
}

/// RC1-J: with NO kept history (a conversation from before RC1-J), the new
/// worker starts from the transcript — the model's own words only, never
/// Awili's lines.
#[gpui::test]
fn rc1j_without_kept_history_awili_lines_never_reach_the_model(cx: &mut gpui::TestAppContext) {
    let Some(msgs) = second_worker_request(
        cx,
        "work::inapp_approval_tests::rc1j_without_kept_history_awili_lines_never_reach_the_model",
        |app, c| {
            app.work.kept_histories.clear();
            super::inapp_history::forget(&c.a);
        },
    ) else {
        return;
    };
    assert_no_awili_lines(&msgs);
    // RC1-J1-FIX2: with nothing kept, a transcript reply is context on the
    // user side — never an assistant message of the model's own.
    assert!(msgs.iter().all(|m| m["role"] != "assistant"), "{msgs:#?}");
    assert!(
        msgs.iter().any(|m| m["role"] == "user"
            && m["content"] == format!("{}\nWrote s1.txt.", super::inapp_history::CONTEXT_LABEL)),
        "{msgs:#?}"
    );
}

/// RC1-J (round 3c Major 2): while a write waits on its card, Progress and
/// the Tools list read the card's plain words, never the JSON arguments.
#[gpui::test]
fn rc1j_progress_and_tools_show_plain_words(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_approval_tests::rc1j_progress_and_tools_show_plain_words")
    else {
        return;
    };
    cx.executor().allow_parking();
    let base = scripted_model(vec![
        write_round("w1", "session1.txt", "ok"),
        text_round("Not written."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let (view, window, _convs) = windowed_switch_app(cx, &home, dir.path(), &base);
    view.update(window, |app, cx| {
        app.submit_work_run("Create session1.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "the card", |app| {
        app.work.panel.approval.is_some()
    });
    view.update(window, |app, _| {
        let words = "Write session1.txt (2 characters)";
        assert_eq!(app.work.panel.activity.as_deref(), Some(words));
        let rows: Vec<Option<String>> = app
            .work
            .panel
            .tool_calls
            .iter()
            .map(|t| t.message.clone())
            .collect();
        assert_eq!(rows, vec![Some(words.to_string())]);
    });
    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Deny, cx)
    });
    wait_in_window(&view, window, "the turn to end", |app| !app.work.running);
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
}

/// RC1-J1-FIX1 (Codex T2 P1 on f6c4a55): a conversation deleted WHILE its
/// in-app turn runs keeps no model history — the turn's end does not write
/// the file back, nor the in-memory entry.
#[gpui::test]
fn rc1j1_fix1_deleting_during_a_turn_keeps_no_history(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_approval_tests::rc1j1_fix1_deleting_during_a_turn_keeps_no_history")
    else {
        return;
    };
    cx.executor().allow_parking();
    let base = scripted_model(vec![
        text_round("Hello."),
        write_round("d1", "s2.txt", "ok"),
        text_round("Not written."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let (view, window, convs) = windowed_switch_app(cx, &home, dir.path(), &base);
    view.update(window, |app, cx| {
        app.submit_work_run("Say hello.".into(), cx)
    });
    wait_in_window(&view, window, "the first turn", |app| !app.work.running);
    assert!(
        super::inapp_history::load(&convs.a).is_some(),
        "a finished turn keeps its history"
    );
    view.update(window, |app, cx| {
        app.submit_work_run("Create s2.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "the card", |app| {
        app.work.panel.approval.is_some()
    });
    // Delete A while its turn waits on the card.
    window.update(|win, cx| view.update(cx, |app, cx| app.delete_conversation(&convs.a, win, cx)));
    window.run_until_parked();
    assert!(super::inapp_history::load(&convs.a).is_none(), "deleted");
    view.update(window, |app, cx| {
        assert!(app.work.running, "the turn is still going");
        app.resolve_work_approval(ApprovalDecision::Deny, cx)
    });
    wait_in_window(&view, window, "the turn to end", |app| !app.work.running);
    assert!(
        super::inapp_history::load(&convs.a).is_none(),
        "the turn's end did not write the file back"
    );
    view.update(window, |app, _| {
        assert!(!app.work.kept_histories.contains_key(&convs.a));
        app.work.gateway_cmd_tx = None;
    });
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn fake_claude_answering(dir: &std::path::Path, answer: &str) -> std::path::PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// RC1-J1-FIX2: a fake `claude` in `dir` that answers one Work turn with
/// `answer` (Claude stream-json), whatever it is asked.
fn fake_claude_answering(dir: &std::path::Path, answer: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{answer}\"}}]}}}}'\n\
         printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"{answer}\"}}'\n"
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Choose `target` for the shown conversation's next turn the way the chooser
/// does. A send follows the conversation's route; writing `work.executor`
/// directly no longer selects anything.
fn choose_target(app: &mut AwiliApp, target: WorkExecutor, cx: &mut Context<AwiliApp>) {
    let id = app.sidebar.active_id().to_string();
    assert!(app.set_composer_target(&crate::workshop_composer::ModelOwner::Work(id), target, cx));
}

/// RC1-J1-FIX2 (Codex T2 P1 on 91299095): in-app → Claude → in-app in ONE
/// conversation. The new in-app worker is shown its own past exactly (the s1
/// write call and result) and Claude's turn only as labelled context — the
/// other helper's reply is never the in-app model's own assistant message.
#[gpui::test]
fn rc1j1_fix2_another_helpers_reply_is_never_the_models_own(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::inapp_approval_tests::rc1j1_fix2_another_helpers_reply_is_never_the_models_own",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        write_round("s1", "s1.txt", "ok\n"),
        text_round("Wrote s1.txt."),
        text_round("It holds ok."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let bins = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let (view, window, convs) = windowed_switch_app(cx, &home, &folder, &base);
    // 1. In-app: write s1.txt.
    view.update(window, |app, cx| {
        app.submit_work_run("Create s1.txt containing ok.".into(), cx)
    });
    wait_in_window(&view, window, "s1's card", |app| {
        app.work.panel.approval.is_some()
    });
    view.update(window, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Approve, cx)
    });
    wait_in_window(&view, window, "the s1 turn", |app| !app.work.running);
    assert!(folder.join("s1.txt").exists());
    // 2. Claude (the agent lane), same conversation.
    let claude = fake_claude_answering(bins.path(), "Hi from Claude.");
    view.update(window, |app, cx| {
        app.work.agents.claude_bin = Some(claude.display().to_string());
        choose_target(
            app,
            WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            },
            cx,
        );
        app.submit_work_run("Say hi.".into(), cx)
    });
    wait_in_window(&view, window, "Claude's turn", |app| !app.work.running);
    view.update(window, |app, cx| {
        let last = app.views[&convs.a]
            .convo
            .messages()
            .last()
            .unwrap()
            .text
            .clone();
        assert!(last.contains("Hi from Claude."), "{last}");
        choose_target(app, WorkExecutor::InApp, cx);
    });
    view.update(window, |app, _| {
        assert!(
            app.work.gateway_cmd_tx.is_none(),
            "the agent turn retired the in-app worker"
        );
    });
    // 3. In-app again: a NEW worker, built from the conversation.
    let before = seen.lock().unwrap().len();
    view.update(window, |app, cx| {
        app.work.local.health.finish_probe(Ok(()), Instant::now());
        app.submit_work_run("What is in s1.txt?".into(), cx)
    });
    wait_in_window(&view, window, "the in-app turn", |app| !app.work.running);
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), before + 1);
    let msgs = request_messages(&bodies[before]);
    assert!(shows_the_s1_tool_call(&msgs), "own past kept: {msgs:#?}");
    assert_no_awili_lines(&msgs);
    for m in msgs.iter().filter(|m| m["role"] == "assistant") {
        let text = m["content"].as_str().unwrap_or_default();
        assert!(
            !text.contains("Hi from Claude"),
            "Claude's reply as the model's own: {msgs:#?}"
        );
    }
    let users: Vec<&str> = msgs
        .iter()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str())
        .collect();
    let note = format!("{}\nHi from Claude.", super::inapp_history::CONTEXT_LABEL);
    let at = users
        .iter()
        .position(|u| *u == "Say hi.")
        .expect("Claude's prompt, as context");
    assert_eq!(users.get(at + 1), Some(&note.as_str()), "{msgs:#?}");
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
}

/// RC1-J1-FIX3 (Codex T2 P2 on 935a600): a workerless-lane turn that FAILS
/// its preflight (local Build with an unknown target) still retires the live
/// in-app worker — its user message is in the transcript — so the next in-app
/// turn is built from the conversation and sees that turn as context.
#[gpui::test]
fn rc1j1_fix3_a_failed_workerless_turn_still_retires_the_worker(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::inapp_approval_tests::rc1j1_fix3_a_failed_workerless_turn_still_retires_the_worker",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        text_round("Hello."),
        text_round("The build did not start."),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let (view, window, convs) = windowed_switch_app(cx, &home, dir.path(), &base);
    view.update(window, |app, cx| {
        app.submit_work_run("Say hello.".into(), cx)
    });
    wait_in_window(&view, window, "the in-app turn", |app| !app.work.running);
    assert!(view.update(window, |app, _| app.work.gateway_cmd_tx.is_some()));
    // A local Build turn whose target does not exist: it fails in preflight.
    view.update(window, |app, cx| {
        choose_target(
            app,
            WorkExecutor::LocalBuild {
                target_id: "no-such-target".into(),
            },
            cx,
        );
        app.submit_work_run("Build the parser.".into(), cx)
    });
    wait_in_window(&view, window, "the failed build", |app| !app.work.running);
    view.update(window, |app, cx| {
        let texts: Vec<String> = app.views[&convs.a]
            .convo
            .messages()
            .iter()
            .map(|m| m.text.clone())
            .collect();
        assert!(texts.iter().any(|t| t == "Build the parser."), "{texts:?}");
        assert!(
            app.work.gateway_cmd_tx.is_none(),
            "the accepted workerless turn retired the in-app worker"
        );
        choose_target(app, WorkExecutor::InApp, cx);
    });
    let before = seen.lock().unwrap().len();
    view.update(window, |app, cx| {
        app.submit_work_run("Did the build start?".into(), cx)
    });
    wait_in_window(&view, window, "the next in-app turn", |app| {
        !app.work.running
    });
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), before + 1);
    let msgs = request_messages(&bodies[before]);
    assert!(
        msgs.iter()
            .any(|m| m["role"] == "user" && m["content"] == "Build the parser."),
        "the failed turn is context: {msgs:#?}"
    );
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
}

/// RC1-N item 3 (round 4 K3), the real in-app path: Deny on the write card
/// ends the turn — the model is NOT asked again about the refusal, nothing
/// is written, the reply is one plain line, the request is back in the box
/// with the plan card's toast — and the same worker answers the next prompt.
#[gpui::test]
fn rc1n_in_app_deny_ends_the_turn_in_one_plain_line(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("work::inapp_approval_tests::rc1n_in_app_deny_ends_the_turn_in_one_plain_line")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        write_round("w1", "hello3.txt", "hi"),
        text_round("NEXT PROMPT ANSWER"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let entity = cx.new(AwiliApp::new);
    let request = "Create a file named hello3.txt containing hi.";
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(CONV.into()),
            awili_work::ApprovalMode::AskBeforeChanges,
        );
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run(request.into(), cx);
    });
    wait_for(cx, &entity, "the approval card", |app| {
        app.work.panel.approval.is_some()
    });
    entity.update(cx, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Deny, cx);
    });
    wait_for(cx, &entity, "the denied turn to end", |app| {
        !app.work.running
    });
    assert!(!folder.join("hello3.txt").exists(), "Deny wrote nothing");
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the model is not asked again after the Deny"
    );
    entity.update(cx, |app, cx| {
        let reply = app.views[CONV]
            .convo
            .messages()
            .last()
            .map(|m| m.text.clone())
            .unwrap();
        // The single-line denial quotes at most 120 characters, including the
        // ellipsis for long isolated test paths.
        let prompt = format!(
            "Write the file {}/hello3.txt (2 characters)",
            folder.display()
        );
        let shown = if prompt.chars().count() > 120 {
            format!("{}…", prompt.chars().take(119).collect::<String>())
        } else {
            prompt
        };
        assert_eq!(
            reply,
            format!("Okay — you declined “{shown}”, so it was not done.")
        );
        assert_eq!(app.composer.read(cx).text(), request, "the request is back");
        assert_eq!(
            app.toast.msg.as_ref().map(|m| m.to_string()).as_deref(),
            Some(crate::plan_gate::DENY_STOPPED_TOAST)
        );
        assert_eq!(app.work.panel.status, Some(RunStatus::Stopped));
        assert!(app.work.panel.error.is_none(), "a Deny is not an error");
        assert!(app.work.gateway_cmd_tx.is_some(), "the worker stays up");
        let footer = last_footer(app);
        assert!(footer.ends_with(" \u{b7} no files changed"), "{footer}");
        app.composer.update(cx, |c, cx| c.set_text("", cx));
        app.submit_work_run("Say something.".into(), cx);
    });
    wait_for(cx, &entity, "the next turn to end", |app| !app.work.running);
    entity.update(cx, |app, cx| {
        let reply = app.views[CONV]
            .convo
            .messages()
            .last()
            .map(|m| m.text.clone())
            .unwrap();
        assert!(reply.contains("NEXT PROMPT ANSWER"), "{reply}");
        // The denied turn was rolled back: the next request carries no
        // trace of the refused call.
        let next = seen.lock().unwrap()[1].clone();
        assert!(!next.contains("hello3.txt\\\""), "{next}");
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// One model round that calls `fs_read` (the wire name) on `path`.
fn read_round(id: &str, path: &str) -> String {
    write_round(id, path, "")
        .replace("fs_write", "fs_read")
        .replace(
            &serde_json::to_string(
                &serde_json::json!({ "path": path, "contents": "" }).to_string(),
            )
            .unwrap(),
            &serde_json::to_string(&serde_json::json!({ "path": path }).to_string()).unwrap(),
        )
}

/// RC1-N item 1 (round 4 M3), the real in-app path: a tool result far
/// larger than the model's window (a 150,000-character file on a 32K window
/// — the owner's 63k-character memory answer did this to Gemma) is cut to
/// fit with Awili's plain marker, and the turn completes with the model's
/// answer — never "context: cannot fit" as the reply.
#[gpui::test]
fn rc1n_in_app_oversized_result_is_cut_and_the_turn_completes(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated(
        "work::inapp_approval_tests::rc1n_in_app_oversized_result_is_cut_and_the_turn_completes",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        read_round("r1", "big.txt"),
        text_round("SUMMARY OF THE BIG FILE"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let big: String = (0..150_000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    std::fs::write(folder.join("big.txt"), &big).unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.engine.window_tokens = 32_768;
        app.work.executor = WorkExecutor::InApp;
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(CONV.into()),
            awili_work::ApprovalMode::AskBeforeChanges,
        );
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run("Read big.txt and summarise it.".into(), cx);
    });
    wait_for(cx, &entity, "the turn to end", |app| !app.work.running);
    entity.update(cx, |app, cx| {
        let reply = app.views[CONV]
            .convo
            .messages()
            .last()
            .map(|m| m.text.clone())
            .unwrap();
        assert!(reply.contains("SUMMARY OF THE BIG FILE"), "{reply}");
        assert!(!reply.contains("cannot fit"), "{reply}");
        assert!(app.work.panel.error.is_none(), "{:?}", app.work.panel.error);
        assert_eq!(app.work.panel.status, Some(RunStatus::Completed));
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "one read, one answer");
    assert!(
        requests[1].contains("[Cut by Awili to fit the model's context:"),
        "the model saw the plain marker"
    );
    assert!(requests[1].len() < big.len(), "the result was cut");
}

/// RC1-N-FIX2 (whole-slice R2 P1), the real in-app path: a turn whose
/// approved write ran, then whose continuation failed (the model server's
/// error), keeps that write in the model's history — the next turn's request
/// shows the call and its result, so the model does not repeat it.
#[gpui::test]
fn rc1n_fix2_in_app_failed_turn_keeps_its_completed_write(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated(
        "work::inapp_approval_tests::rc1n_fix2_in_app_failed_turn_keeps_its_completed_write",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        write_round("w1", "kept.txt", "hi"),
        r#"{"error":{"message":"upstream model crashed","type":"server_error"}}"#.into(),
        text_round("NEXT"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(CONV.into()),
            awili_work::ApprovalMode::AskBeforeChanges,
        );
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run("Create kept.txt containing hi.".into(), cx);
    });
    wait_for(cx, &entity, "the card", |app| {
        app.work.panel.approval.is_some()
    });
    entity.update(cx, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Approve, cx)
    });
    wait_for(cx, &entity, "the failed turn to end", |app| {
        !app.work.running
    });
    assert!(folder.join("kept.txt").exists(), "the write ran");
    entity.update(cx, |app, cx| {
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
        app.submit_work_run("What did you do?".into(), cx)
    });
    wait_for(cx, &entity, "the next turn to end", |app| !app.work.running);
    let next = seen.lock().unwrap()[2].clone();
    assert!(
        next.contains("kept.txt") && next.contains("\"role\":\"tool\""),
        "{next}"
    );
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// One model round that calls `fs_write` twice (the wire name).
fn write_round2(a: (&str, &str), b: (&str, &str)) -> String {
    let call = |i: usize, id: &str, path: &str| {
        let args = serde_json::json!({ "path": path, "contents": "x" }).to_string();
        let args = serde_json::to_string(&args).unwrap();
        chunk(&format!(
            r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":"","tool_calls":[{{"id":"{id}","index":{i},"type":"function","function":{{"name":"fs_write","arguments":{args}}}}}]}},"finish_reason":null}}]}}"#
        ))
    };
    let mut out = call(0, a.0, a.1);
    out.push_str(&call(1, b.0, b.1));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"tool_calls"}]}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

/// RC1-N-FIX2 (pre-review P1), the Stop path through the app: a turn whose
/// first write was approved and ran is stopped at the second write's card,
/// and the owner resubmits AT ONCE. The app discarded that worker; the next
/// dispatch waits for it to publish its history (its completed write kept)
/// and starts the new worker from it — the next request shows the completed
/// write as the model's own call, and never the second one.
#[gpui::test]
fn rc1n_fix2_in_app_stop_then_resubmit_keeps_the_completed_write(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated(
        "work::inapp_approval_tests::rc1n_fix2_in_app_stop_then_resubmit_keeps_the_completed_write",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        write_round2(("w1", "first.txt"), ("w2", "second.txt")),
        text_round("NEXT"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(CONV.into()),
            awili_work::ApprovalMode::AskBeforeChanges,
        );
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run("Create first.txt and second.txt.".into(), cx);
    });
    wait_for(cx, &entity, "the first card", |app| {
        app.work.panel.approval.is_some()
    });
    entity.update(cx, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Approve, cx)
    });
    wait_for(cx, &entity, "the second card", |app| {
        app.work
            .panel
            .approval
            .as_ref()
            .is_some_and(|c| c.prompt.contains("second.txt"))
    });
    assert!(folder.join("first.txt").exists(), "the first write ran");
    // Stop at the second card, and resubmit at once.
    entity.update(cx, |app, cx| {
        app.cancel_work_run(cx);
        app.submit_work_run("What did you do?".into(), cx);
    });
    wait_for(cx, &entity, "the next turn", |app| !app.work.running);
    assert!(
        !folder.join("second.txt").exists(),
        "the second write never ran"
    );
    let next = seen.lock().unwrap()[1].clone();
    let msgs = request_messages(&next);
    assert!(
        msgs.iter()
            .any(|m| m["role"] == "assistant" && m["tool_calls"].to_string().contains("first.txt")),
        "the completed write is the model's own call: {msgs:#?}"
    );
    assert!(!next.contains("second.txt\\\""), "{next}");
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// RC1-N-FIX2 (pre-review pass 2), the Stop barrier: Stop arrives while the
/// stopped worker is still waiting on a slow model answer (after its write
/// ran), and the owner resubmits at once. The new turn waits for that worker
/// to publish the stopped turn's history — however long its answer takes,
/// no timeout — and starts from it: the next request shows the completed
/// write as the model's own call.
#[gpui::test]
fn rc1n_fix2_in_app_resubmit_waits_for_the_stopped_worker(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated(
        "work::inapp_approval_tests::rc1n_fix2_in_app_resubmit_waits_for_the_stopped_worker",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        write_round("w1", "slow.txt", "hi"),
        format!("DELAY_MS:4000\n{}", text_round("LATE")),
        text_round("NEXT"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(CONV.into()),
            awili_work::ApprovalMode::Auto,
        );
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run("Create slow.txt containing hi.".into(), cx);
    });
    // The write ran and the continuation is out (the model is "thinking").
    let deadline = Instant::now() + Duration::from_secs(20);
    while seen.lock().unwrap().len() < 2 {
        cx.run_until_parked();
        assert!(Instant::now() < deadline, "the continuation never went out");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(folder.join("slow.txt").exists());
    let stopped_at = Instant::now();
    entity.update(cx, |app, cx| {
        app.cancel_work_run(cx);
        app.submit_work_run("What did you do?".into(), cx);
    });
    wait_for(cx, &entity, "the next turn", |app| {
        !app.work.running && seen.lock().unwrap().len() >= 3
    });
    assert!(
        stopped_at.elapsed() >= Duration::from_millis(3500),
        "the new turn waited for the stopped worker"
    );
    let msgs = request_messages(&seen.lock().unwrap()[2]);
    assert!(
        msgs.iter()
            .any(|m| m["role"] == "assistant" && m["tool_calls"].to_string().contains("slow.txt")),
        "{msgs:#?}"
    );
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// RC1-N-FIX2 (pre-review pass 3): a Stop arms the "wait for the stopped
/// worker" barrier only for a run of this conversation's live in-app worker
/// — a stopped Agent turn after an in-app turn never does (its next in-app
/// submit would otherwise wait on an empty slot). And the stopped history
/// covers the transcript only as it was at the Stop: a later helper's turn
/// is restored as context after it.
#[gpui::test]
fn rc1n_fix2_the_stop_barrier_is_only_for_in_app_runs(cx: &mut gpui::TestAppContext) {
    if isolated("work::inapp_approval_tests::rc1n_fix2_the_stop_barrier_is_only_for_in_app_runs")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Work;
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.work.conv_id = Some(CONV.into());
        // A finished in-app turn left its (emptied) slot behind; an Agent
        // turn is running and is stopped.
        let slot = awili_engine_worker::HistorySlot::default();
        app.work.inapp_history_slot = Some((CONV.into(), slot));
        app.work.executor = WorkExecutor::Agent {
            engine: awili_code::CodeEngine::Claude,
            model: None,
        };
        app.work.running = true;
        app.cancel_work_run(cx);
        assert!(
            app.work.stopped_histories.is_empty(),
            "an Agent Stop arms nothing"
        );
        assert!(super::stopped_history_pending(&app.work, CONV).is_none());
    });
    // The boundary: history published by a stopped worker covers the first
    // two messages (the stopped turn); a later helper's turn follows.
    let mut convo = awili_core::ConversationState::new();
    convo.push(awili_core::Role::User, "write it");
    convo.push(awili_core::Role::Assistant, "Stopped.");
    convo.push(awili_core::Role::User, "Claude: what changed?");
    convo.push(awili_core::Role::Assistant, "Claude says: nothing yet.");
    let before = convo.messages().to_vec();
    entity.update(cx, |app, _| {
        let slot = awili_engine_worker::HistorySlot::default();
        let mut h = awili_seam::ContextHistory::new();
        h.push_user("write it");
        *slot.lock().unwrap() = Some(
            awili_engine_worker::ModelHistory::from_json(&serde_json::to_string(&h).unwrap())
                .unwrap(),
        );
        app.work.stopped_histories.insert(CONV.into(), (slot, 2));
        let kept = super::take_stopped_history(&mut app.work, CONV, &before).expect("kept");
        let (own, restored) = super::inapp_history::resume(Some(&kept), &before);
        assert!(own.is_some(), "the stopped history is the model's own past");
        let texts: Vec<String> = restored.iter().map(|r| r.user.clone()).collect();
        assert!(
            texts.iter().any(|t| t == "Claude: what changed?"),
            "the later helper's turn is restored: {texts:?}"
        );
    });
}

/// RC1-N-FIX2 (pre-review pass 5): a Stop in conversation A stays pending
/// while the owner works in conversation B — B's turn never takes or drops
/// A's stopped worker — and A's next turn still starts from the stopped
/// turn's history (the completed write as the model's own call).
#[gpui::test]
fn rc1n_fix2_a_stop_in_one_conversation_waits_for_its_own_next_turn(cx: &mut gpui::TestAppContext) {
    let Some(_home) = isolated(
        "work::inapp_approval_tests::rc1n_fix2_a_stop_in_one_conversation_waits_for_its_own_next_turn",
    ) else {
        return;
    };
    const B: &str = "rc1n-fix2-other";
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        write_round("w1", "slow.txt", "hi"),
        format!("DELAY_MS:3000\n{}", text_round("LATE")),
        text_round("B ANSWER"),
        text_round("A NEXT"),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        for conv in [CONV, B] {
            app.work.ask_modes.set(
                crate::ask_mode::AskKey::Work(conv.into()),
                awili_work::ApprovalMode::Auto,
            );
            app.views.insert(
                conv.into(),
                crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
            );
        }
        app.set_active_conversation(CONV);
        app.submit_work_run("Create slow.txt containing hi.".into(), cx);
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while seen.lock().unwrap().len() < 2 {
        cx.run_until_parked();
        assert!(Instant::now() < deadline, "the continuation never went out");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(folder.join("slow.txt").exists());
    entity.update(cx, |app, cx| {
        app.cancel_work_run(cx);
        assert!(
            app.work.stopped_histories.contains_key(CONV),
            "A's Stop is pending"
        );
        app.set_active_conversation(B);
        app.submit_work_run("Hello from B.".into(), cx);
        assert!(
            app.work.stopped_histories.contains_key(CONV),
            "B's turn leaves A's stopped worker pending"
        );
    });
    wait_for(cx, &entity, "B's turn", |app| {
        !app.work.running && seen.lock().unwrap().len() >= 3
    });
    entity.update(cx, |app, cx| {
        assert!(app.work.stopped_histories.contains_key(CONV));
        app.set_active_conversation(CONV);
        app.submit_work_run("What did you do?".into(), cx);
    });
    wait_for(cx, &entity, "A's next turn", |app| {
        !app.work.running && seen.lock().unwrap().len() >= 4
    });
    let msgs = request_messages(&seen.lock().unwrap()[3]);
    assert!(
        msgs.iter()
            .any(|m| m["role"] == "assistant" && m["tool_calls"].to_string().contains("slow.txt")),
        "{msgs:#?}"
    );
    entity.update(cx, |app, cx| {
        assert!(
            !app.work.stopped_histories.contains_key(CONV),
            "kept by A's turn"
        );
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// RC2-RUN (round 3c Minor 7): after the owner's Deny the in-app turn can
/// take a while to wind down — Progress says so in plain words, the reply
/// keeps its "working" pulse, and no "nothing new for …" line appears
/// meanwhile. When the turn has ended, all of that clears.
#[gpui::test]
fn rc2_run_after_a_deny_the_run_says_it_is_finishing(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("work::inapp_approval_tests::rc2_run_after_a_deny_the_run_says_it_is_finishing")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (base, _seen) = scripted_model_capturing(vec![write_round("w1", "hello3.txt", "hi")]);
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        app.work.ask_modes.set(
            crate::ask_mode::AskKey::Work(CONV.into()),
            awili_work::ApprovalMode::AskBeforeChanges,
        );
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run("Create a file named hello3.txt containing hi.".into(), cx);
    });
    wait_for(cx, &entity, "the approval card", |app| {
        app.work.panel.approval.is_some()
    });
    entity.update(cx, |app, cx| {
        app.resolve_work_approval(ApprovalDecision::Deny, cx);
        assert!(app.work.running, "the turn is winding down");
        assert_eq!(
            app.work.panel.activity.as_deref(),
            Some(crate::work::DENY_WINDING_DOWN)
        );
        assert!(
            app.work.waiting_for_stream_output,
            "the reply keeps its working pulse"
        );
        let quiet = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(60))
            .unwrap();
        app.work.health.activity.observe(u64::MAX, quiet);
        let parts = app.executor_status_parts();
        assert!(
            parts.iter().all(|(t, _)| !t.contains("nothing new")),
            "no silence line while it finishes: {parts:?}"
        );
    });
    wait_for(cx, &entity, "the denied turn to end", |app| {
        !app.work.running
    });
    entity.update(cx, |app, cx| {
        assert_eq!(app.work.panel.activity, None);
        assert!(!app.work.waiting_for_stream_output);
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// RC2-R56 (usability RC2 round 4: a signed-out Claude's Work card printed
/// "Failed to authenticate: OAuth session expired and could not be
/// refreshed" twice and never said how to sign in): a fake `claude` that
/// fails exactly as the signed-out CLI does (the line as text, then as an
/// error result) leaves one plain line with the fix, and the run failed.
#[gpui::test]
fn rc2r56_a_signed_out_claude_is_one_plain_line(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::inapp_approval_tests::rc2r56_a_signed_out_claude_is_one_plain_line")
    else {
        return;
    };
    cx.executor().allow_parking();
    let raw = "Failed to authenticate: OAuth session expired and could not be refreshed";
    let bins = tempfile::tempdir().unwrap();
    let claude = bins.path().join("claude");
    // Never `fs::write` an executable a test will exec (ETXTBSY, RC1-G2).
    crate::exec_fixture::write_exec_fixture(
        &claude,
        &format!(
            "#!/bin/sh\n\
             printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{raw}\"}}]}}}}'\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\"result\":\"{raw}\"}}'\n\
             exit 1\n"
        ),
        0o755,
    );
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().to_path_buf();
    let (view, window, convs) = windowed_switch_app(cx, &home, &folder, "http://127.0.0.1:9");
    view.update(window, |app, cx| {
        app.work.agents.claude_bin = Some(claude.display().to_string());
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        app.submit_work_run("rename alpha.txt to beta.txt".into(), cx)
    });
    wait_in_window(&view, window, "Claude's turn", |app| !app.work.running);
    view.update(window, |app, _| {
        let last = app.views[&convs.a]
            .convo
            .messages()
            .last()
            .unwrap()
            .text
            .clone();
        assert_eq!(
            last.trim(),
            "\u{26A0} Claude is signed out. Open Terminal, run claude, then type /login. Then \
             press Retry.",
            "one plain line, no raw CLI words"
        );
        assert!(!last.contains("OAuth"), "{last}");
        assert_eq!(app.work.panel.status, Some(RunStatus::Failed));
    });
}

/// The configured Studio's live rejection on 2026-10-16 was HTTP404 with a
/// string error, not a dead server. An image failure must not poison the next
/// ordinary text request, lose the attachment, or silently change provider.
#[gpui::test]
fn text_only_endpoint_rejects_image_clearly_and_next_text_turn_still_works(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home)=isolated("work::inapp_approval_tests::text_only_endpoint_rejects_image_clearly_and_next_text_turn_still_works") else {return;};
    cx.executor().allow_parking();
    let (base, seen) = scripted_model_capturing(vec![
        "HTTP_STATUS:404 Not Found\n{\"error\":\"Only 'text' content type is supported.\"}".into(),
        text_round("Text followup works."),
    ]);
    let folder = tempfile::tempdir().unwrap();
    let entity = cx.new(AwiliApp::new);
    let image="![s](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)\nDescribe the image";
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.path().to_path_buf());
        app.work.engine.base_url = base.clone();
        app.work.engine.model = "m".into();
        app.work.executor = WorkExecutor::InApp;
        app.work
            .graduation
            .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
        app.views.insert(
            CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(CONV);
        app.submit_work_run(image.into(), cx);
    });
    wait_for(
        cx,
        &entity,
        "the explicit image-capability rejection",
        |app| !app.work.running,
    );
    entity.update(cx, |app, cx| {
        let error = app.work.panel.error.as_deref().unwrap_or("");
        assert!(error.contains("accepts text only"), "{error}");
        assert!(app.views[CONV]
            .convo
            .messages()
            .iter()
            .any(|m| m.text == image));
        assert!(matches!(app.work.executor, WorkExecutor::InApp));
        app.work.local.health.finish_probe(Ok(()), Instant::now());
        app.submit_work_run("Continue with text only".into(), cx);
    });
    wait_for(
        cx,
        &entity,
        "a text followup after the image rejection",
        |app| !app.work.running,
    );
    entity.read_with(cx, |app, _| {
        assert!(app.work.panel.error.is_none(), "{:?}", app.work.panel.error);
        assert!(app.views[CONV]
            .convo
            .messages()
            .iter()
            .any(|m| m.text.contains("Text followup works.")));
    });
    let bodies = seen.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0].contains("image_url"));
    assert!(!bodies[1].contains("base64"));
    assert!(image_endpoint_error("HTTP404 missing resource").is_none());
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn fake_claude_with_usage(dir: &std::path::Path, answer: &str, input: u32) -> std::path::PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// rc.3 S2: a fake `claude` whose result reports `input` input tokens.
fn fake_claude_with_usage(dir: &std::path::Path, answer: &str, input: u32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\n' '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{answer}\"}}]}}}}'\n\
         printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"{answer}\",\"usage\":{{\"input_tokens\":{input},\"output_tokens\":5}},\"modelUsage\":{{\"claude-haiku-4-5\":{{\"contextWindow\":1000000,\"maxOutputTokens\":128000}}}}}}'\n"
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// rc.3 S2: an in-app turn and a Claude CLI turn each keep a breakdown on
/// their reply. The in-app one lists Awili's parts and no CLI remainder; the
/// Claude one adds "Claude's own instructions and tools" = provider input
/// minus Awili's parts, and knows Claude's 200K window.
#[gpui::test]
fn s2_turns_record_their_prompt_breakdown(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::inapp_approval_tests::s2_turns_record_their_prompt_breakdown")
    else {
        return;
    };
    use crate::context_breakdown::PartCategory;
    cx.executor().allow_parking();
    let base = scripted_model(vec![text_round("Hello there.")]);
    let dir = tempfile::tempdir().unwrap();
    let bins = tempfile::tempdir().unwrap();
    let (view, window, convs) = windowed_switch_app(cx, &home, dir.path(), &base);
    view.update(window, |app, cx| {
        app.submit_work_run("Say hello to me.".into(), cx)
    });
    wait_in_window(&view, window, "the in-app turn", |app| !app.work.running);
    let inapp = view.update(window, |app, _| {
        app.views[&convs.a].rows.last().unwrap().context.clone()
    });
    let inapp = inapp.expect("in-app reply keeps a breakdown");
    let msg = inapp
        .parts
        .iter()
        .find(|p| p.label == "Your message")
        .unwrap();
    assert_eq!(msg.chars, "Say hello to me.".chars().count());
    assert!(inapp
        .parts
        .iter()
        .any(|p| p.category == PartCategory::Instructions));
    assert!(inapp.cli.is_none());
    assert!(inapp
        .parts
        .iter()
        .all(|p| p.category != PartCategory::Provider));

    let claude = fake_claude_with_usage(bins.path(), "Hi from Claude.", 12_000);
    view.update(window, |app, cx| {
        app.work.agents.claude_bin = Some(claude.display().to_string());
        choose_target(
            app,
            WorkExecutor::Agent {
                engine: CodeEngine::Claude,
                model: None,
            },
            cx,
        );
        app.submit_work_run("Say hi.".into(), cx)
    });
    wait_in_window(&view, window, "Claude's turn", |app| !app.work.running);
    let ctx = view
        .update(window, |app, _| {
            app.views[&convs.a].rows.last().unwrap().context.clone()
        })
        .expect("Claude reply keeps a breakdown");
    assert_eq!(ctx.cli.as_deref(), Some("Claude"));
    assert_eq!(ctx.provider_input, Some(12_000));
    assert_eq!(ctx.limit, Some(1_000_000), "the window Claude reported");
    assert!(ctx.limit_reported);
    let rest = ctx
        .parts
        .iter()
        .find(|p| p.category == PartCategory::Provider)
        .expect("remainder part");
    assert_eq!(rest.label, "Claude's own instructions and tools");
    let awili: u32 = ctx
        .parts
        .iter()
        .filter(|p| p.category != PartCategory::Provider)
        .map(|p| p.tokens)
        .sum();
    assert_eq!(rest.tokens, 12_000 - awili);
    view.update(window, |app, _| app.work.gateway_cmd_tx = None);
}
