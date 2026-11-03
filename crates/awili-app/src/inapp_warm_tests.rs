//! RC2-R619: a Work conversation with the in-app model warms its model
//! server's cache before the owner's first question.

use std::path::PathBuf;

use awili_code::CodeEngine;
use awili_core::ConvKind;
use gpui::AppContext;

use crate::work::{GatewayCmd, Mode, WorkExecutor, TEST_WORKER_SPAWN};
use crate::AwiliApp;
use awili_work::WorkerContext;

const CHILD: &str = "AWILI_RC2R619_TEST_HOME";

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

/// RC2-R619 (usability RC2 round 5, Major 1: a cold first Gemma turn took
/// 54 s). With a Work conversation on screen and the in-app model as its
/// helper, Awili starts that conversation's worker with a warm-up of the
/// system text a turn would pin (rules, AGENTS.md, the brief for its
/// folder) — no run starts and the panel shows none. Once per folder; the
/// owner's question then goes to that same worker; another helper warms
/// nothing.
#[gpui::test]
fn rc2r619_a_gemma_work_conversation_warms_its_model_ahead(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("inapp_warm::tests::rc2r619_a_gemma_work_conversation_warms_its_model_ahead")
    else {
        return;
    };
    let folder = home.join("w1");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("alpha.txt"), "one\n").unwrap();
    let slot: std::rc::Rc<std::cell::RefCell<Option<WorkerContext>>> = Default::default();
    let entity = cx.new(AwiliApp::new);
    let conv = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.agents.memory = None;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.kind = awili_config::EngineKind::Awili;
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "/m/gemma".into();
        app.work.engine.label = "Gemma 4 26B (Studio)".into();
        app.work.executor = WorkExecutor::InApp;
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Work);
        app.views.insert(
            meta.id.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(&meta.id);
        app.work.warm.enabled = true;
        // Another helper warms nothing.
        app.work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        assert_eq!(app.warm_due(std::time::Instant::now()), None);
        app.work.executor = WorkExecutor::InApp;
        assert!(app.warm_due(std::time::Instant::now()).is_some());
        let keep = slot.clone();
        TEST_WORKER_SPAWN.with(|s| {
            *s.borrow_mut() = Some(Box::new(move |ctx| {
                *keep.borrow_mut() = Some(ctx);
            }));
        });
        app.maybe_warm_inapp(cx);
        meta.id
    });
    let ctx = slot.borrow_mut().take().expect("the worker was started");
    match ctx.cmd_rx.try_recv() {
        Ok(GatewayCmd::Warm {
            system: Some(system),
        }) => {
            assert!(
                system.contains(&folder.display().to_string()),
                "the brief for this folder: {system}"
            );
        }
        Ok(_) => panic!("the first command is a warm-up"),
        Err(e) => panic!("no first command: {e}"),
    }
    entity.update(cx, |app, cx| {
        assert!(!app.work.running, "a warm-up is no run");
        assert_eq!(app.work.panel.requested_identity, None);
        assert_eq!(app.work.worker_conv.as_deref(), Some(conv.as_str()));
        assert!(app.warm_due(std::time::Instant::now()).is_none(), "once");
        // The owner's question goes to the same worker.
        app.submit_work_run("what is in alpha.txt?".into(), cx);
    });
    match ctx.cmd_rx.try_recv() {
        Ok(GatewayCmd::Prompt { text, .. }) => assert_eq!(text, "what is in alpha.txt?"),
        Ok(_) => panic!("the question is a prompt"),
        Err(e) => panic!("the question did not reach the warmed worker: {e}"),
    }
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        app.work.running = false;
        cx.notify();
    });
    drop(ctx);
}

/// RC2-R620 (usability RC2 round 5, Major 1: in a council's first round
/// Gemma's first words came after 24-42 s). A council on screen warms the
/// members that answer on Awili's tool engine (Gemma here; Claude is made
/// ready its own way), with the system text their first turn pins; once,
/// and never while the council answers.
#[gpui::test]
fn rc2r620_a_council_warms_its_gemma_member_ahead(cx: &mut gpui::TestAppContext) {
    if isolated("inapp_warm::tests::rc2r620_a_council_warms_its_gemma_member_ahead").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.memory.enabled = false;
        app.work.engine.kind = awili_config::EngineKind::Awili;
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "/m/gemma".into();
        app.work.engine.label = "Gemma 4 26B (Studio)".into();
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Council);
        app.councils.insert(
            meta.id.clone(),
            crate::council_view::CouncilPage::with_group(&[
                ("in-app".to_string(), "Gemma 4 26B (Studio)".to_string()),
                (
                    "agent:claude:sonnet".to_string(),
                    "Claude Sonnet".to_string(),
                ),
            ]),
        );
        app.set_active_conversation(&meta.id);
        app.mode = Mode::Council;
        let now = std::time::Instant::now();
        assert!(app.council_warm_due(now).is_empty(), "off until turned on");
        app.work.warm.enabled = true;
        let due = app.council_warm_due(now);
        assert_eq!(due.len(), 1, "only the member on Awili's engine");
        let d = &due[0];
        assert_eq!(d.target.base_url, "http://127.0.0.1:9");
        assert_eq!(d.target.model, "/m/gemma");
        let system = crate::council_run::engine_member_system(None, None, &d.seat);
        assert!(system.contains(&d.seat.brief), "{system}");
        // Once warmed, not again while it is fresh.
        app.work.warm.warmed.insert(d.key.clone(), now);
        assert!(app.council_warm_due(now).is_empty());
        assert_eq!(
            app.council_warm_due(now + crate::inapp_warm::WARM_FRESH)
                .len(),
            1,
            "again once the server may have let it go"
        );
        // Another page: nothing.
        app.mode = Mode::Work;
        assert!(app
            .council_warm_due(now + crate::inapp_warm::WARM_FRESH)
            .is_empty());
    });
}

/// A loopback model server that keeps every request body and answers each
/// with one short streamed reply.
fn recording_model() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let keep = seen.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let keep = keep.clone();
            std::thread::spawn(move || {
                let mut s = s;
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                let body = loop {
                    let n = s.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break None;
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
                            break Some(String::from_utf8_lossy(&data[h + 4..]).into_owned());
                        }
                    }
                };
                let Some(body) = body else {
                    return;
                };
                // The status strip's read-only Lemonade check is no request: a
                // 404 (not Lemonade), not counted.
                if data.starts_with(b"GET /api/v1/health") {
                    let _ = write!(
                        s,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    return;
                }
                keep.lock().unwrap().push(body);
                let out = format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    serde_json::json!({"id": "c", "object": "chat.completion.chunk",
                        "created": 1, "model": "m", "choices": [{"index": 0,
                        "delta": {"role": "assistant", "content": "o"}, "finish_reason": null}]}),
                    serde_json::json!({"id": "c", "object": "chat.completion.chunk",
                        "created": 1, "model": "m", "choices": [{"index": 0, "delta": {},
                        "finish_reason": "stop"}]}),
                );
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\
                     Content-Length: {}\r\n\r\n{out}",
                    out.len()
                );
            });
        }
    });
    (format!("http://{addr}"), seen)
}

/// Wait (bounded) for a oneshot to resolve.
fn wait_done<T>(mut rx: futures::channel::oneshot::Receiver<T>, what: &str) -> Option<T> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match rx.try_recv() {
            Ok(Some(v)) => return Some(v),
            Ok(None) => {}
            Err(_) => return None,
        }
        assert!(std::time::Instant::now() < deadline, "{what} did not end");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// RC2-R6M1 (RC2-R6 review P1 on 0fc8434b: the council warm-up queued
/// `Shutdown` right after `Warm`, the warm-up's host read it as a stop, and
/// no request was ever sent). A council member's warm-up worker sends its
/// model server the warm-up request — the member's system text first, one
/// token asked for — and then ends.
#[test]
fn rc2r6m1_a_council_warm_up_reaches_its_model_server() {
    let Some(home) =
        isolated("inapp_warm::tests::rc2r6m1_a_council_warm_up_reaches_its_model_server")
    else {
        return;
    };
    let (url, seen) = recording_model();
    let target = crate::council_exec::EngineTarget {
        base_url: url,
        model: "/m/gemma".into(),
        api_key: String::new(),
    };
    let base = awili_config::EngineConfig {
        kind: awili_config::EngineKind::Awili,
        ..Default::default()
    };
    let system = "You are Gemma, one member of the owner's council. R6M1-BRIEF.".to_string();
    let done = crate::council_exec::warm_engine_member(
        &target,
        &base,
        32_768,
        &home.display().to_string(),
        "council-warm-r6m1",
        Some(system),
    )
    .expect("the warm-up worker starts");
    assert_eq!(
        wait_done(done, "the warm-up worker"),
        Some(true),
        "the worker reports the warm-up read"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one warm-up request was sent");
    let body: serde_json::Value = serde_json::from_str(&seen[0]).unwrap();
    assert_eq!(body["max_tokens"], 1, "{body}");
    assert_eq!(body["model"], "/m/gemma");
    let first = body["messages"][0]["content"].as_str().unwrap_or_default();
    assert!(
        first.contains("R6M1-BRIEF"),
        "the member's system text: {first}"
    );
    assert_eq!(
        body["messages"].as_array().unwrap().last().unwrap()["content"],
        awili_seam::WARM_USER
    );
}

/// A loopback model server that answers every request with an error.
fn broken_model() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut s = s;
                let mut buf = [0u8; 65536];
                let _ = s.read(&mut buf);
                let out = "{\"error\":{\"message\":\"model not loaded\"}}";
                let _ = write!(
                    s,
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\n\
                     Connection: close\r\nContent-Length: {}\r\n\r\n{out}",
                    out.len()
                );
            });
        }
    });
    format!("http://{addr}")
}

/// RC2-R6M2 (RC2-R6 review P2 on 0fc8434b: a council member was counted
/// warm for ten minutes the moment its warm-up started, even when the
/// server never read it). While its warm-up runs the member is not due
/// again; it is counted warm only once the server has read the warm-up;
/// a warm-up the server failed is not counted warm and is tried again
/// after `WARM_RETRY`, not on every tick.
#[gpui::test]
fn rc2r6m2_a_council_member_counts_warm_only_once_read(cx: &mut gpui::TestAppContext) {
    if isolated("inapp_warm::tests::rc2r6m2_a_council_member_counts_warm_only_once_read").is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (good, seen) = recording_model();
    let bad = broken_model();
    let entity = cx.new(AwiliApp::new);
    let pump = |cx: &mut gpui::TestAppContext, what: &str, done: &dyn Fn(&AwiliApp) -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            cx.run_until_parked();
            if entity.read_with(cx, |app, _| done(app)) {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    let key = entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.memory.enabled = false;
        app.work.engine.kind = awili_config::EngineKind::Awili;
        app.work.engine.base_url = good.clone();
        app.work.engine.model = "/m/gemma".into();
        app.work.engine.label = "Gemma 4 26B (Studio)".into();
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Council);
        app.councils.insert(
            meta.id.clone(),
            crate::council_view::CouncilPage::with_group(&[(
                "in-app".to_string(),
                "Gemma 4 26B (Studio)".to_string(),
            )]),
        );
        app.set_active_conversation(&meta.id);
        app.mode = Mode::Council;
        app.work.warm.enabled = true;
        let due = app.council_warm_due(std::time::Instant::now());
        assert_eq!(due.len(), 1);
        let key = due[0].key.clone();
        app.maybe_warm_council(cx);
        assert!(
            !app.work.warm.warmed.contains_key(&key),
            "not counted warm when only started"
        );
        assert!(
            app.council_warm_due(std::time::Instant::now()).is_empty(),
            "not started again while it runs"
        );
        key
    });
    pump(cx, "the warm-up read", &|app| {
        app.work.warm.in_flight.is_empty() && !app.work.warm.preparing
    });
    entity.update(cx, |app, _| {
        assert!(
            app.work.warm.warmed.contains_key(&key),
            "counted warm once read"
        );
        assert!(!app.work.warm.failed.contains_key(&key));
    });
    assert_eq!(seen.lock().unwrap().len(), 1, "one warm-up request");

    // A server that fails the warm-up: not counted warm, tried again later.
    let key = entity.update(cx, |app, cx| {
        app.work.warm.warmed.clear();
        app.work.engine.base_url = bad.clone();
        let due = app.council_warm_due(std::time::Instant::now());
        assert_eq!(due.len(), 1);
        let key = due[0].key.clone();
        app.maybe_warm_council(cx);
        key
    });
    pump(cx, "the failed warm-up ended", &|app| {
        app.work.warm.in_flight.is_empty() && !app.work.warm.preparing
    });
    entity.update(cx, |app, _| {
        assert!(
            !app.work.warm.warmed.contains_key(&key),
            "a warm-up the server failed is not counted warm"
        );
        let failed_at = *app.work.warm.failed.get(&key).expect("recorded as failed");
        assert!(
            app.council_warm_due(failed_at).is_empty(),
            "not retried on every tick"
        );
        assert_eq!(
            app.council_warm_due(failed_at + crate::inapp_warm::WARM_RETRY)
                .len(),
            1,
            "tried again later"
        );
    });
}

/// RC2-R6M2: a Work conversation's folder too is counted warm only once its
/// worker says the server read the warm-up — not when the warm-up starts;
/// and a worker that ends without saying so leaves it unread.
#[gpui::test]
fn rc2r6m2_a_work_folder_counts_warm_only_once_read(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("inapp_warm::tests::rc2r6m2_a_work_folder_counts_warm_only_once_read")
    else {
        return;
    };
    let folder = home.join("w1");
    std::fs::create_dir_all(&folder).unwrap();
    // RC2-R6M7: warmth is kept by model server, model and folder.
    let key = gemma_key(&folder);
    let slot: std::rc::Rc<std::cell::RefCell<Option<WorkerContext>>> = Default::default();
    let entity = cx.new(AwiliApp::new);
    let start = |cx: &mut gpui::TestAppContext| {
        entity.update(cx, |app, cx| {
            let keep = slot.clone();
            TEST_WORKER_SPAWN.with(|s| {
                *s.borrow_mut() = Some(Box::new(move |ctx| {
                    *keep.borrow_mut() = Some(ctx);
                }));
            });
            app.work.gateway_cmd_tx = None;
            app.work.worker_conv = None;
            assert!(app.warm_due(std::time::Instant::now()).is_some());
            app.maybe_warm_inapp(cx);
            assert!(
                !app.work.warm.warmed.contains_key(&key),
                "not counted warm when only started"
            );
            assert!(app.work.warm.in_flight.contains(&key));
        });
        slot.borrow_mut().take().expect("the worker was started")
    };
    entity.update(cx, |app, _| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.agents.memory = None;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.kind = awili_config::EngineKind::Awili;
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "/m/gemma".into();
        app.work.executor = WorkExecutor::InApp;
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Work);
        app.views.insert(
            meta.id.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(&meta.id);
        app.work.warm.enabled = true;
    });
    // Read through: counted warm.
    let ctx = start(cx);
    ctx.events
        .send_blocking(awili_work::WorkEvent::Warmed { ok: true })
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, _| {
        assert!(
            app.work.warm.warmed.contains_key(&key),
            "counted warm once read"
        );
        assert!(app.work.warm.in_flight.is_empty());
    });
    drop(ctx);
    cx.run_until_parked();
    entity.update(cx, |app, _| {
        assert!(app.work.warm.warmed.contains_key(&key), "still warm after");
        app.work.warm.warmed.clear();
    });
    // The worker ends without a word: not warm, tried again later.
    let ctx = start(cx);
    drop(ctx);
    cx.run_until_parked();
    entity.update(cx, |app, _| {
        assert!(!app.work.warm.warmed.contains_key(&key));
        assert!(app.work.warm.in_flight.is_empty());
        assert!(app.work.warm.failed.contains_key(&key));
    });
}

/// RC2-R6M4 (RC2-R6 review P2 on 9c0fa36d: a Work conversation whose
/// worker stayed alive was never warmed again after `WARM_FRESH`, so a
/// question after a long idle spell read everything cold). Once the
/// folder's warmth has expired, the live, idle worker of the conversation
/// on screen is sent another `Warm` (no new worker), and the folder counts
/// warm again once that one is read. A turn the worker answers counts as
/// reading the folder.
#[gpui::test]
fn rc2r6m4_a_live_work_worker_warms_again_once_expired(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("inapp_warm::tests::rc2r6m4_a_live_work_worker_warms_again_once_expired")
    else {
        return;
    };
    let folder = home.join("w1");
    std::fs::create_dir_all(&folder).unwrap();
    // RC2-R6M7: warmth is kept by model server, model and folder.
    let key = gemma_key(&folder);
    let slot: std::rc::Rc<std::cell::RefCell<Option<WorkerContext>>> = Default::default();
    let entity = cx.new(AwiliApp::new);
    let keep = slot.clone();
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.agents.memory = None;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.kind = awili_config::EngineKind::Awili;
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "/m/gemma".into();
        app.work.executor = WorkExecutor::InApp;
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Work);
        app.views.insert(
            meta.id.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(&meta.id);
        app.work.warm.enabled = true;
        TEST_WORKER_SPAWN.with(|s| {
            *s.borrow_mut() = Some(Box::new(move |ctx| {
                *keep.borrow_mut() = Some(ctx);
            }));
        });
        app.maybe_warm_inapp(cx);
    });
    let ctx = slot.borrow_mut().take().expect("the worker was started");
    assert!(matches!(ctx.cmd_rx.try_recv(), Ok(GatewayCmd::Warm { .. })));
    ctx.events
        .send_blocking(awili_work::WorkEvent::Warmed { ok: true })
        .unwrap();
    cx.run_until_parked();
    let now = std::time::Instant::now();
    entity.update(cx, |app, cx| {
        assert!(app.work.warm.warmed.contains_key(&key));
        assert!(app.warm_due(now).is_none(), "fresh: not again");
        // Ten minutes and more later, the worker still alive and idle.
        let later = now + crate::inapp_warm::WARM_FRESH;
        assert!(app.work.gateway_cmd_tx.is_some(), "the worker is alive");
        assert!(
            app.warm_due(later).is_some(),
            "due again once the server may have let it go"
        );
        // Make the stamp expired for the real clock (or gone, on a clock
        // younger than `WARM_FRESH`), and warm.
        match now.checked_sub(crate::inapp_warm::WARM_FRESH + std::time::Duration::from_secs(1)) {
            Some(expired) => app.work.warm.warmed.insert(key.clone(), expired),
            None => app.work.warm.warmed.remove(&key),
        };
        app.maybe_warm_inapp(cx);
        assert!(
            app.work.warm.warmed.get(&key).is_none_or(|at| *at < now),
            "not counted warm when only started"
        );
        assert!(
            app.warm_due(std::time::Instant::now()).is_none(),
            "under way"
        );
    });
    assert!(slot.borrow().is_none(), "no new worker");
    match ctx.cmd_rx.try_recv() {
        Ok(GatewayCmd::Warm { system: Some(s) }) => {
            assert!(s.contains(&folder.display().to_string()), "{s}")
        }
        Ok(_) => panic!("the live worker is sent a warm-up"),
        Err(e) => panic!("the live worker got nothing: {e}"),
    }
    ctx.events
        .send_blocking(awili_work::WorkEvent::Warmed { ok: true })
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, _| {
        let at = *app.work.warm.warmed.get(&key).expect("warm again");
        assert!(at > now, "counted warm again once read");
        assert!(app.work.warm.in_flight.is_empty());
        // A finished turn counts as reading the folder.
        app.work.warm.warmed.clear();
    });
    ctx.events
        .send_blocking(awili_work::WorkEvent::TurnCompleted {
            status: awili_work::run_events::RunStatus::Completed,
            output: None,
            cost_usd: None,
            context_percent: None,
        })
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(app.work.warm.warmed.contains_key(&key), "a turn read it");
        app.work.gateway_cmd_tx = None;
        app.work.running = false;
        cx.notify();
    });
    drop(ctx);
}

/// RC2-R6M7: the warmth key of `folder` on the test Gemma.
fn gemma_key(folder: &std::path::Path) -> String {
    crate::inapp_warm::work_warm_key(
        &awili_config::EngineConfig {
            kind: awili_config::EngineKind::Awili,
            base_url: "http://127.0.0.1:9".into(),
            model: "/m/gemma".into(),
            ..Default::default()
        },
        &folder.display().to_string(),
    )
}

/// RC2-R6M7 (RC2-R6 review P2 on 0dd1a343: warmth was kept by folder alone
/// (Work) and without the model (council), so after the model or its server
/// changed, a recently warmed folder was not warmed for the new one). With
/// the old stamp still fresh, another model or another server is due a
/// warm-up of its own; the Work conversation's live worker of the old model
/// is retired and a new one warms the new model. A council member is due
/// again for another model too.
#[gpui::test]
fn rc2r6m7_a_new_model_or_server_is_warmed_while_the_old_is_fresh(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "inapp_warm::tests::rc2r6m7_a_new_model_or_server_is_warmed_while_the_old_is_fresh",
    ) else {
        return;
    };
    let folder = home.join("w1");
    std::fs::create_dir_all(&folder).unwrap();
    let slot: std::rc::Rc<std::cell::RefCell<Option<WorkerContext>>> = Default::default();
    let entity = cx.new(AwiliApp::new);
    let hook = |slot: &std::rc::Rc<std::cell::RefCell<Option<WorkerContext>>>| {
        let keep = slot.clone();
        TEST_WORKER_SPAWN.with(|s| {
            *s.borrow_mut() = Some(Box::new(move |ctx| {
                *keep.borrow_mut() = Some(ctx);
            }));
        });
    };
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.agents.memory = None;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.kind = awili_config::EngineKind::Awili;
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "/m/gemma".into();
        app.work.executor = WorkExecutor::InApp;
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Work);
        app.views.insert(
            meta.id.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(&meta.id);
        app.work.warm.enabled = true;
        hook(&slot);
        app.maybe_warm_inapp(cx);
    });
    let old = slot.borrow_mut().take().expect("the worker was started");
    assert!(matches!(old.cmd_rx.try_recv(), Ok(GatewayCmd::Warm { .. })));
    old.events
        .send_blocking(awili_work::WorkEvent::Warmed { ok: true })
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        let now = std::time::Instant::now();
        assert!(app.work.warm.warmed.contains_key(&gemma_key(&folder)));
        assert!(app.warm_due(now).is_none(), "fresh for Gemma");
        // Another server, same model: due.
        app.work.engine.base_url = "http://127.0.0.1:10".into();
        assert!(app.warm_due(now).is_some(), "another server is not warm");
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        assert!(app.warm_due(now).is_none());
        // Another model: due, and warmed on a worker of that model.
        app.work.engine.model = "/m/qwen".into();
        assert!(app.warm_due(now).is_some(), "another model is not warm");
        hook(&slot);
        app.maybe_warm_inapp(cx);
    });
    assert!(
        matches!(old.cmd_rx.try_recv(), Ok(GatewayCmd::Shutdown)),
        "the old model's worker is retired"
    );
    let new = slot.borrow_mut().take().expect("a worker of the new model");
    assert!(matches!(new.cmd_rx.try_recv(), Ok(GatewayCmd::Warm { .. })));
    new.events
        .send_blocking(awili_work::WorkEvent::Warmed { ok: true })
        .unwrap();
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        let qwen = crate::inapp_warm::work_warm_key(
            &app.work.effective_engine(),
            &folder.display().to_string(),
        );
        assert!(
            app.work.warm.warmed.contains_key(&qwen),
            "the new model is warm"
        );
        assert!(app.warm_due(std::time::Instant::now()).is_none());

        // A council member: due again for another model.
        app.work.gateway_cmd_tx = None;
        app.work.engine.model = "/m/gemma".into();
        let meta = app.conversations.create_kind(1_000_001, ConvKind::Council);
        app.councils.insert(
            meta.id.clone(),
            crate::council_view::CouncilPage::with_group(&[(
                "in-app".to_string(),
                "Gemma 4 26B (Studio)".to_string(),
            )]),
        );
        app.set_active_conversation(&meta.id);
        app.mode = Mode::Council;
        let now = std::time::Instant::now();
        let due = app.council_warm_due(now);
        assert_eq!(due.len(), 1);
        app.work.warm.warmed.insert(due[0].key.clone(), now);
        assert!(app.council_warm_due(now).is_empty(), "fresh for Gemma");
        app.work.engine.model = "/m/qwen".into();
        assert_eq!(
            app.council_warm_due(now).len(),
            1,
            "another model is not warm"
        );
        cx.notify();
    });
    drop((old, new));
}

/// RC2-R6M9 (RC2-R6 review P1 on 8197ec08: the dispatch check compared a
/// worker identity that holds the model but not its server). The in-app
/// model's server is A, then B, then A again, each warmed (both stamps
/// fresh): the owner's question goes to A's server, not to B's live
/// worker.
#[gpui::test]
fn rc2r6m9_a_question_goes_to_the_selected_server_after_a_to_b_to_a(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "inapp_warm::tests::rc2r6m9_a_question_goes_to_the_selected_server_after_a_to_b_to_a",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let folder = home.join("w1");
    std::fs::create_dir_all(&folder).unwrap();
    let (a, seen_a) = recording_model();
    let (b, seen_b) = recording_model();
    let entity = cx.new(AwiliApp::new);
    let pump = |cx: &mut gpui::TestAppContext, what: &str, done: &dyn Fn(&AwiliApp) -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            cx.run_until_parked();
            if entity.read_with(cx, |app, _| done(app)) {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    let key = |app: &AwiliApp| {
        crate::inapp_warm::work_warm_key(
            &app.work.effective_engine(),
            &folder.display().to_string(),
        )
    };
    entity.update(cx, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.agents.memory = None;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine.kind = awili_config::EngineKind::Awili;
        app.work.engine.base_url = a.clone();
        app.work.engine.model = "/m/gemma".into();
        app.work.executor = WorkExecutor::InApp;
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Work);
        app.views.insert(
            meta.id.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(&meta.id);
        app.work.warm.enabled = true;
        app.maybe_warm_inapp(cx);
    });
    pump(cx, "A warmed", &|app| {
        app.work.warm.warmed.contains_key(&key(app))
    });
    entity.update(cx, |app, cx| {
        app.work.engine.base_url = b.clone();
        assert!(app.warm_due(std::time::Instant::now()).is_some());
        app.maybe_warm_inapp(cx);
    });
    pump(cx, "B warmed", &|app| {
        app.work.warm.warmed.contains_key(&key(app))
    });
    assert_eq!(seen_a.lock().unwrap().len(), 1, "A's warm-up");
    assert_eq!(seen_b.lock().unwrap().len(), 1, "B's warm-up");
    entity.update(cx, |app, cx| {
        app.work.engine.base_url = a.clone();
        assert!(
            app.warm_due(std::time::Instant::now()).is_none(),
            "A's stamp is still fresh"
        );
        app.submit_work_run("R6M9-QUESTION".into(), cx);
    });
    let asked = |seen: &std::sync::Arc<std::sync::Mutex<Vec<String>>>| {
        seen.lock()
            .unwrap()
            .iter()
            .any(|body| body.contains("R6M9-QUESTION"))
    };
    pump(cx, "the question answered", &|app| !app.work.running);
    assert!(!asked(&seen_b), "the question did not go to B");
    assert!(asked(&seen_a), "the question went to A");
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}
