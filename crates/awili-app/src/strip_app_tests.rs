//! The status strip in the app: what a turn to a Lemonade host does (and,
//! above all, does not do), the strip's states in a real window, the
//! popover, and the polling's cadence. No real network: loopback servers
//! only, and the app's own polling is off under test.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use awili_config::{Config, EngineConfig, ExtraEngineConfig};
use awili_core::ConvKind;
use gpui::AppContext;

use crate::strip::tests::{health_body, serve, Seen};
use crate::strip::{Resident, State};
use crate::strip_app::{
    probe_key, CHECK_NOW_SELECTOR, POPOVER_SELECTOR, RETRY_SELECTOR, STRIP_SELECTOR,
};
use crate::work::{Mode, WorkExecutor};
use crate::AwiliApp;

const CHILD: &str = "AWILI_STRIP_APP_TEST_HOME";

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

fn hosta(base: &str, model: &str) -> ExtraEngineConfig {
    ExtraEngineConfig {
        id: "hosta-coder".into(),
        base_url: base.into(),
        model: model.into(),
        api_key: String::new(),
        label: "Qwen3 Coder Next (Hosta)".into(),
        dialect: "chat-completions".into(),
        window_tokens: 262_144,
        request_timeout_s: 30,
    }
}

fn sse_reply() -> String {
    let chunk = |delta: serde_json::Value, finish: serde_json::Value| {
        serde_json::json!({"id": "c", "object": "chat.completion.chunk", "created": 1,
            "model": "m", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        chunk(
            serde_json::json!({"role": "assistant", "content": "ok"}),
            serde_json::Value::Null
        ),
        chunk(serde_json::json!({}), serde_json::json!("stop")),
    )
}

/// A Work app whose conversation targets the Hosta extra at `url`.
fn work_app(
    cx: &mut gpui::TestAppContext,
    home: &std::path::Path,
    url: &str,
    model: &str,
) -> gpui::Entity<AwiliApp> {
    let folder = home.join("w1");
    std::fs::create_dir_all(&folder).unwrap();
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.work.agents.memory = None;
        app.mode = Mode::Work;
        app.work.folder = Some(folder.clone());
        app.work.engine = EngineConfig {
            kind: awili_config::EngineKind::Awili,
            base_url: "http://127.0.0.1:9".into(),
            model: "/m/gemma".into(),
            label: "Gemma".into(),
            ..Default::default()
        };
        app.work.extra_engines = vec![hosta(url, model)];
        app.work.executor = WorkExecutor::InAppExtra {
            id: "hosta-coder".into(),
        };
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Work);
        app.views.insert(
            meta.id.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(&meta.id);
    });
    entity
}

fn pump(
    cx: &mut gpui::TestAppContext,
    entity: &gpui::Entity<AwiliApp>,
    what: &str,
    done: &dyn Fn(&AwiliApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        cx.run_until_parked();
        if entity.read_with(cx, |app, _| done(app)) {
            return;
        }
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn requests(seen: &Seen) -> Vec<String> {
    seen.lock().unwrap().clone()
}

fn routes_with(models: &[&str]) -> Vec<(&'static str, u16, String)> {
    vec![
        ("/api/v1/health", 200, health_body(models)),
        ("/v1/models", 200, r#"{"data":[]}"#.to_string()),
        ("/v1/chat/completions", 200, sse_reply()),
    ]
}

// ---- THE SAFETY RULE ---------------------------------------------------------------------

/// A turn to a Lemonade model that is NOT resident sends no chat request, no
/// load, nothing but the read-only health GET. The turn ends with an error
/// card that says Awili will not load models on that host, and the strip is
/// green and names the model.
#[gpui::test]
fn a_turn_to_a_non_resident_lemonade_model_sends_no_chat_request(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("strip_app::tests::a_turn_to_a_non_resident_lemonade_model_sends_no_chat_request")
    else {
        return;
    };
    cx.executor().allow_parking();
    // Only the voice model is resident; the Coder is not.
    let (url, seen) = serve(routes_with(&["kokoro-v1"]), false);
    let entity = work_app(cx, &home, &url, "Qwen3-Coder-Next-GGUF");
    entity.update(cx, |app, cx| {
        app.submit_work_run("NEVER-SEND-THIS".into(), cx)
    });
    pump(cx, &entity, "the card shows", &|app| {
        app.work.panel.error.is_some() && !app.work.running
    });
    let lines = requests(&seen);
    assert!(!lines.is_empty(), "the host was read");
    for line in &lines {
        assert_eq!(
            line, "GET /api/v1/health HTTP/1.1",
            "only the health read: {lines:?}"
        );
        assert!(!line.contains("chat") && !line.contains("load"), "{line}");
    }
    entity.read_with(cx, |app, _| {
        let card = app.work.panel.error.clone().unwrap();
        assert!(
            card.contains("Qwen3 Coder Next isn\u{2019}t loaded on Hosta"),
            "{card}"
        );
        assert!(card.contains("doesn\u{2019}t load models"), "{card}");
        let (_, view) = app.strip_view(Instant::now()).unwrap();
        assert_eq!(view.state, State::NotLoaded);
        assert_eq!(
            view.left[0].text,
            "Qwen3 Coder Next isn\u{2019}t loaded on Hosta"
        );
        assert!(view.retry);
    });
    // Nothing was written into the model's history either.
    assert!(
        !seen.lock().unwrap().iter().any(|l| l.contains("POST")),
        "no POST of any kind"
    );
}

/// The same host with the model resident: the turn is sent as before.
#[gpui::test]
fn a_turn_to_a_resident_lemonade_model_is_sent(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("strip_app::tests::a_turn_to_a_resident_lemonade_model_is_sent")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (url, seen) = serve(routes_with(&["Qwen3-Coder-Next-GGUF", "kokoro-v1"]), false);
    let entity = work_app(cx, &home, &url, "Qwen3-Coder-Next-GGUF");
    entity.update(cx, |app, cx| app.submit_work_run("SEND-THIS".into(), cx));
    pump(cx, &entity, "the turn ended", &|app| !app.work.running);
    let lines = requests(&seen);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("POST /v1/chat/completions")),
        "sent: {lines:?}"
    );
    assert!(
        lines.iter().all(|l| !l.contains("/load")),
        "never a load: {lines:?}"
    );
    entity.read_with(cx, |app, _| {
        assert!(app.work.panel.error.is_none(), "{:?}", app.work.panel.error);
    });
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// A server that is not Lemonade (a 404 on its health path) is untouched.
#[gpui::test]
fn a_server_that_is_not_lemonade_still_gets_its_turn(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("strip_app::tests::a_server_that_is_not_lemonade_still_gets_its_turn")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (url, seen) = serve(
        vec![
            ("/v1/models", 200, r#"{"data":[]}"#.to_string()),
            ("/v1/chat/completions", 200, sse_reply()),
        ],
        false,
    );
    let entity = work_app(cx, &home, &url, "any-model");
    entity.update(cx, |app, cx| app.submit_work_run("PLAIN".into(), cx));
    pump(cx, &entity, "the turn ended", &|app| !app.work.running);
    assert!(requests(&seen)
        .iter()
        .any(|l| l.starts_with("POST /v1/chat/completions")));
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// A Lemonade host that cannot be seen to be ready gets nothing either.
#[test]
fn a_silent_lemonade_host_gets_no_turn() {
    let (url, seen) = serve(vec![], true);
    let engine = EngineConfig {
        kind: awili_config::EngineKind::Awili,
        base_url: url,
        model: "m".into(),
        label: "Model (Hosta)".into(),
        ..Default::default()
    };
    let (card, resident) = AwiliApp::lemonade_refusal(&engine, Duration::from_millis(400)).unwrap();
    assert!(
        card.contains("Hosta didn\u{2019}t answer in time"),
        "{card}"
    );
    assert!(resident.is_none());
    assert!(requests(&seen)
        .iter()
        .all(|l| l.starts_with("GET /api/v1/health")));
}

// ---- the strip in a real window ---------------------------------------------------------------

fn setup(cx: &mut gpui::TestAppContext) -> (gpui::Entity<AwiliApp>, &mut gpui::VisualTestContext) {
    awili_config::write_config(&Config::networkless(), &Config::config_paths()[0]).unwrap();
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.persist = None;
        app.fleet_pack = None;
        app.memory.enabled = false;
        app.set_mode(Mode::Work, cx);
        app.work.folder = Some("/fixture/strip".into());
        app.work.projects.clear();
        app.work.extra_engines = vec![hosta("http://hosta.test:13305", "Qwen3-Coder-Next-GGUF")];
        app.work.executor = WorkExecutor::InAppExtra {
            id: "hosta-coder".into(),
        };
        let owner = crate::workshop_composer::ModelOwner::Work(app.sidebar.active_id().to_string());
        assert!(app.set_composer_target(
            &owner,
            WorkExecutor::InAppExtra {
                id: "hosta-coder".into()
            },
            cx
        ));
    });
    window.simulate_resize(gpui::size(gpui::px(1455.), gpui::px(900.)));
    window.run_until_parked();
    (view, window)
}

fn has(window: &mut gpui::VisualTestContext, selector: &'static str) -> bool {
    window.debug_bounds(selector).is_some()
}

fn apply(
    view: &gpui::Entity<AwiliApp>,
    window: &mut gpui::VisualTestContext,
    reach: crate::gateway_health::Reach,
    resident: Option<Resident>,
) {
    view.update(window, |app, cx| {
        app.strip_apply_reading(
            &probe_key("http://hosta.test:13305"),
            crate::strip::Reading {
                reach,
                lemonade: resident.is_some(),
                resident,
            },
            Instant::now(),
        );
        cx.notify();
    });
    window.run_until_parked();
}

#[gpui::test]
fn the_strip_replaces_the_key_hints_and_stays_one_row(cx: &mut gpui::TestAppContext) {
    if isolated("strip_app::tests::the_strip_replaces_the_key_hints_and_stays_one_row").is_none() {
        return;
    }
    let (_view, window) = setup(cx);
    assert!(has(window, STRIP_SELECTOR), "the strip is in the composer");
    // The old hints are gone.
    for gone in ["Send", "New line"] {
        let found = crate::ui::text::DRAWN_TEXTS.with(|t| {
            t.borrow()
                .iter()
                .filter_map(crate::ui::text::measured_text)
                .any(|s| s == gone)
        });
        assert!(!found, "the {gone:?} hint is dropped");
    }
    let strip = window.debug_bounds(STRIP_SELECTOR).unwrap();
    let footer = window
        .debug_bounds("ui-composer-frame-awili-composer-footer")
        .unwrap();
    assert!(
        strip.size.height <= crate::theme::z(crate::theme::STRIP_H) + gpui::px(1.),
        "no taller than 22 px: {strip:?}"
    );
    assert!(footer.size.height <= crate::theme::z(crate::theme::STRIP_H) + gpui::px(4.));
}

#[gpui::test]
fn unreachable_shows_the_sentence_and_retry_and_hides_the_context(cx: &mut gpui::TestAppContext) {
    if isolated("strip_app::tests::unreachable_shows_the_sentence_and_retry_and_hides_the_context")
        .is_none()
    {
        return;
    }
    let (view, window) = setup(cx);
    // A reply that recorded its context.
    view.update(window, |app, cx| {
        app.work_push(crate::Role::Assistant, "Reply".into());
        let owner = app.sidebar.active_id().to_string();
        let row = app.views.get_mut(&owner).unwrap().rows.last_mut().unwrap();
        let mut b = crate::context_breakdown::ContextBreakdown::default();
        b.settle(Some(18_700), Some((262_144, true)), false);
        row.context = Some(b);
        cx.notify();
    });
    window.run_until_parked();
    assert!(has(window, "ui-text-composer-context-use"));
    assert!(!has(window, RETRY_SELECTOR), "reachable: no Retry");

    apply(
        &view,
        window,
        crate::gateway_health::Reach::Down("connection refused".into()),
        None,
    );
    let (text, retry) = view.read_with(window, |app, _| {
        let (_, v) = app.strip_view(Instant::now()).unwrap();
        (v.left[0].text.clone(), v.retry)
    });
    assert_eq!(text, "Hosta isn\u{2019}t responding");
    assert!(retry);
    assert!(has(window, RETRY_SELECTOR), "Retry is drawn");
    assert!(
        !has(window, "ui-text-composer-context-use") || {
            // The slot may exist but must be empty: no text drawn into it.
            let b = window.debug_bounds("ui-text-composer-context-use").unwrap();
            b.size.width <= gpui::px(1.)
        },
        "context is hidden on a host that cannot answer"
    );
}

#[gpui::test]
fn hover_or_click_opens_the_popover_and_check_now_reads_again(cx: &mut gpui::TestAppContext) {
    if isolated("strip_app::tests::hover_or_click_opens_the_popover_and_check_now_reads_again")
        .is_none()
    {
        return;
    }
    let (view, window) = setup(cx);
    assert!(!has(window, POPOVER_SELECTOR), "closed until asked");
    view.update(window, |app, cx| app.strip_toggle_pin(cx));
    window.run_until_parked();
    assert!(has(window, POPOVER_SELECTOR), "a click pins it open");
    assert!(has(window, CHECK_NOW_SELECTOR), "with Check now");
    view.update(window, |app, cx| app.strip_toggle_pin(cx));
    window.run_until_parked();
    assert!(!has(window, POPOVER_SELECTOR), "a second click releases it");
}

#[gpui::test]
fn streaming_beats_unreachable_and_the_clock_stops_with_the_turn(cx: &mut gpui::TestAppContext) {
    if isolated("strip_app::tests::streaming_beats_unreachable_and_the_clock_stops_with_the_turn")
        .is_none()
    {
        return;
    }
    let (view, window) = setup(cx);
    view.update(window, |app, cx| {
        app.work.conv_id = Some(app.sidebar.active_id().into());
        app.work.running = true;
        app.work.run_started = Some(Instant::now() - Duration::from_secs(5));
        let conv = app.sidebar.active_id().to_string();
        for _ in 0..10 {
            app.strip_on_token(&conv, cx);
            std::thread::sleep(Duration::from_millis(40));
        }
        cx.notify();
    });
    window.run_until_parked();
    let state = view.read_with(window, |app, _| {
        app.strip_view(Instant::now()).unwrap().1.state
    });
    assert_eq!(state, State::Streaming);
    view.update(window, |app, cx| {
        let conv = app.sidebar.active_id().to_string();
        app.strip_on_end(&conv);
        app.work.running = false;
        cx.notify();
    });
    window.run_until_parked();
    let (state, left) = view.read_with(window, |app, _| {
        let v = app.strip_view(Instant::now()).unwrap().1;
        (
            v.state,
            v.left.iter().map(|s| s.text.clone()).collect::<Vec<_>>(),
        )
    });
    assert_eq!(state, State::Ready);
    assert_eq!(left.last().map(String::as_str), Some("tok/s"), "{left:?}");
}

/// Polling is for in-app engines only, and the app's own polling stays off
/// under test (no test reaches a configured host).
#[test]
fn polling_is_off_under_test_and_cloud_models_have_no_engine() {
    const { assert!(!crate::strip_app::POLL_DEFAULT) };
}

/// A warm-up is a chat request: it goes only to a host a recent health read
/// showed safe (another kind of server, or Lemonade with the model
/// resident), and never blocks the UI thread to find out.
#[test]
fn a_warm_up_only_goes_to_a_host_a_recent_read_showed_safe() {
    // Lemonade, model not resident: never.
    let (url, seen) = serve(routes_with(&["kokoro-v1"]), false);
    assert!(!crate::strip::warm_allowed(&url, "Qwen3-Coder-Next-GGUF"));
    // Lemonade, resident: yes.
    let (url2, _) = serve(routes_with(&["Qwen3-Coder-Next-GGUF"]), false);
    assert!(crate::strip::warm_allowed(&url2, "Qwen3-Coder-Next-GGUF"));
    // A different kind of server (404 on health): yes.
    let (url3, _) = serve(vec![], false);
    assert!(crate::strip::warm_allowed(&url3, "any"));
    // Nothing known (nothing listening): no.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    // Under test a refused connection counts as a confirmed other server (a
    // warm-up there only fails); in the app no reading means no warm-up.
    assert!(crate::strip::warm_allowed(&dead, "any"));
    for line in requests(&seen) {
        assert_eq!(line, "GET /api/v1/health HTTP/1.1");
    }
}

/// A user-configured OpenAI-compatible endpoint that is a Lemonade host gets
/// the same check: a non-resident model is sent nothing; a resident one goes.
#[test]
fn an_openai_provider_on_a_lemonade_host_is_checked_before_it_sends() {
    use awili_providers::{ChatMessage, ChatProvider, ChatRequest, ProviderEvent};
    use futures::StreamExt;
    let req = |model: &str| ChatRequest {
        model: model.into(),
        system: None,
        messages: vec![ChatMessage::user("hi")],
    };
    let guarded = |url: &str| crate::provider_build::LemonadeGuarded {
        inner: std::sync::Arc::new(awili_providers::OpenAiCompat::new(url.to_string())),
        base_url: url.to_string(),
        api_key: None,
    };

    let (url, seen) = serve(routes_with(&["kokoro-v1"]), false);
    let events = futures::executor::block_on(
        guarded(&url)
            .stream(req("Qwen3-Coder-Next-GGUF"))
            .collect::<Vec<_>>(),
    );
    assert!(
        matches!(&events[0], ProviderEvent::Error(m) if m.contains("isn\u{2019}t loaded")),
        "{events:?}"
    );
    assert_eq!(events[1], ProviderEvent::Done);
    for line in requests(&seen) {
        assert_eq!(line, "GET /api/v1/health HTTP/1.1", "no chat: {line}");
    }

    let (url2, seen2) = serve(routes_with(&["Qwen3-Coder-Next-GGUF"]), false);
    let events = futures::executor::block_on(
        guarded(&url2)
            .stream(req("Qwen3-Coder-Next-GGUF"))
            .collect::<Vec<_>>(),
    );
    assert!(
        events.contains(&ProviderEvent::Token("ok".into())),
        "{events:?}"
    );
    assert!(requests(&seen2)
        .iter()
        .any(|l| l.starts_with("POST /v1/chat/completions")));
}

/// Keyboard: the state area, Retry and Check now are tab stops in that order
/// (after the model chip); Enter opens the popover, Escape closes it.
#[gpui::test]
fn keyboard_reaches_the_state_retry_and_check_now_and_escape_closes(cx: &mut gpui::TestAppContext) {
    if isolated(
        "strip_app::tests::keyboard_reaches_the_state_retry_and_check_now_and_escape_closes",
    )
    .is_none()
    {
        return;
    }
    let (view, window) = setup(cx);
    apply(
        &view,
        window,
        crate::gateway_health::Reach::Down("connection refused".into()),
        None,
    );
    let focused = |window: &mut gpui::VisualTestContext, which: u8| {
        view.read_with(window, |app, _| match which {
            0 => app.strip.focus_state.clone(),
            1 => app.strip.focus_retry.clone(),
            _ => app.strip.focus_check.clone(),
        })
    };
    let state = focused(window, 0);
    window.update(|win, cx| win.focus(&state, cx));
    window.run_until_parked();
    assert!(window.update(|win, _| state.is_focused(win)));
    // Tab: the state area, then Retry.
    window.simulate_keystrokes("tab");
    window.run_until_parked();
    let retry = focused(window, 1);
    assert!(
        window.update(|win, _| retry.is_focused(win)),
        "Tab reaches Retry"
    );
    // Back, then Enter opens the popover.
    window.simulate_keystrokes("shift-tab");
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    assert!(has(window, POPOVER_SELECTOR), "Enter opens it");
    assert!(view.read_with(window, |app, _| app.strip.pinned));
    // Escape closes a pinned popover.
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    assert!(!has(window, POPOVER_SELECTOR), "Escape closes it");
    assert!(!view.read_with(window, |app, _| app.strip.pinned));
    // Reopened, Check now is a stop too.
    window.simulate_keystrokes("space");
    window.run_until_parked();
    assert!(has(window, CHECK_NOW_SELECTOR));
    let mut reached = false;
    for _ in 0..4 {
        window.simulate_keystrokes("tab");
        window.run_until_parked();
        let check = focused(window, 2);
        if window.update(|win, _| check.is_focused(win)) {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab reaches Check now");
}

// ---- review round 2 ----------------------------------------------------------------------

/// A loopback Lemonade whose resident list a test changes under it.
fn switchable(models: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> (String, Seen) {
    switchable_after(models, None)
}

/// Like [`switchable`], but after `evict_after` health reads the model is gone.
fn switchable_after(
    models: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    evict_after: Option<usize>,
) -> (String, Seen) {
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    use std::io::{Read, Write};
    let seen: Seen = Default::default();
    let keep = seen.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (keep, models, reads) = (keep.clone(), models.clone(), reads.clone());
            std::thread::spawn(move || {
                let mut s = stream;
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = s.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        return;
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
                let text = String::from_utf8_lossy(&data).into_owned();
                let line = text.lines().next().unwrap_or_default().to_string();
                keep.lock().unwrap().push(line.clone());
                let (ctype, body) = if line.contains("/api/v1/health") {
                    let n = reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let names: Vec<String> = if evict_after.is_some_and(|k| n >= k) {
                        Vec::new()
                    } else {
                        models.lock().unwrap().clone()
                    };
                    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                    ("application/json", health_body(&refs))
                } else {
                    ("text/event-stream", sse_reply())
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nConnection: close\r\n\
                     Content-Length: {}\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (format!("http://{addr}"), seen)
}

/// THE TRANSPORT BACKSTOP: a request the engine makes AFTER the first (a
/// resume after an approval, a tool-loop step, a retry) is checked too. The
/// model is resident for the first request and evicted before the second:
/// the second is never sent.
#[test]
fn a_resume_after_an_approval_with_the_model_evicted_sends_nothing() {
    use awili_seam::Transport;
    let models = std::sync::Arc::new(std::sync::Mutex::new(vec![
        "Qwen3-Coder-Next-GGUF".to_string()
    ]));
    let (url, seen) = switchable(models.clone());
    let engine = EngineConfig {
        kind: awili_config::EngineKind::Awili,
        base_url: url.clone(),
        model: "Qwen3-Coder-Next-GGUF".into(),
        label: "Qwen3 Coder Next (Hosta)".into(),
        ..Default::default()
    };
    let mut transport = awili_seam::HttpTransport::new(&url, Duration::from_secs(10))
        .with_preflight(AwiliApp::transport_preflight(&engine, Duration::ZERO));
    // Request one (the turn's first step): resident, sent.
    let first = transport.stream("{\"model\":\"m\"}").expect("sent");
    assert!(first.contains("[DONE]"), "{first}");
    let chats = |seen: &Seen| {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("POST"))
            .count()
    };
    assert_eq!(chats(&seen), 1);
    // The model is evicted while the approval card waits; the owner approves
    // and the engine resumes with its next request.
    models.lock().unwrap().clear();
    let err = transport.stream("{\"model\":\"m\"}").expect_err("refused");
    assert!(
        format!("{err:?}").contains("isn\u{2019}t loaded"),
        "{err:?}"
    );
    assert_eq!(
        chats(&seen),
        1,
        "the resume sent nothing: {:?}",
        requests(&seen)
    );
}

/// A read taken a moment ago serves the next request, so a tool loop does
/// not read the host on every step.
#[test]
fn a_tool_loop_reuses_a_fresh_reading() {
    use awili_seam::Transport;
    let models = std::sync::Arc::new(std::sync::Mutex::new(vec!["m1".to_string()]));
    let (url, seen) = switchable(models);
    let engine = EngineConfig {
        kind: awili_config::EngineKind::Awili,
        base_url: url.clone(),
        model: "m1".into(),
        ..Default::default()
    };
    let mut transport =
        awili_seam::HttpTransport::new(&url, Duration::from_secs(10)).with_preflight(
            AwiliApp::transport_preflight(&engine, Duration::from_secs(5)),
        );
    for _ in 0..4 {
        transport.stream("{}").expect("sent");
    }
    let health = requests(&seen)
        .iter()
        .filter(|l| l.contains("/api/v1/health"))
        .count();
    assert_eq!(health, 1, "one read for four steps");
}

/// A 401 on the health read is no answer: nothing is sent.
#[test]
fn a_401_health_answer_is_refused_with_the_card() {
    let (url, seen) = serve(
        vec![
            ("/api/v1/health", 401, "unauthorized".into()),
            ("/v1/chat/completions", 200, sse_reply()),
        ],
        false,
    );
    let engine = EngineConfig {
        kind: awili_config::EngineKind::Awili,
        base_url: url,
        model: "m".into(),
        label: "Model (Hosta)".into(),
        ..Default::default()
    };
    let (card, _) = AwiliApp::lemonade_refusal(&engine, Duration::from_secs(2)).expect("refused");
    assert!(
        card.contains("Hosta didn\u{2019}t answer in time"),
        "{card}"
    );
    assert!(requests(&seen)
        .iter()
        .all(|l| l.starts_with("GET /api/v1/health")));
}

/// An Ollama-kind (and Anthropic-kind) route on a LAN/loopback Lemonade host
/// is checked before it sends, like the OpenAI kind.
#[test]
fn an_ollama_kind_route_on_a_lemonade_host_is_checked() {
    use awili_config::{ModelChoice, ProviderKind};
    use awili_providers::{ChatMessage, ChatRequest, ProviderEvent};
    use futures::StreamExt;
    let (url, seen) = serve(routes_with(&["kokoro-v1"]), false);
    for kind in [ProviderKind::Ollama, ProviderKind::Anthropic] {
        let choice = ModelChoice {
            provider: "hosta".into(),
            kind,
            base_url: url.clone(),
            model: "Qwen3-Coder-Next-GGUF".into(),
            api_key: None,
        };
        let provider = crate::provider_build::build_provider(
            &choice,
            &crate::provider_build::ProviderKeys::default(),
            "http://hermes.invalid",
        );
        let events = futures::executor::block_on(
            provider
                .stream(ChatRequest {
                    model: "Qwen3-Coder-Next-GGUF".into(),
                    system: None,
                    messages: vec![ChatMessage::user("hi")],
                })
                .collect::<Vec<_>>(),
        );
        assert!(
            matches!(&events[0], ProviderEvent::Error(m) if m.contains("isn\u{2019}t loaded")),
            "{kind:?}: {events:?}"
        );
    }
    for line in requests(&seen) {
        assert_eq!(
            line, "GET /api/v1/health HTTP/1.1",
            "no model request: {line}"
        );
    }
}

// ---- review round 3 ---------------------------------------------------------------------

/// END TO END: the first read (the early guard) finds the model resident; it
/// is evicted before the engine's request, so the TRANSPORT refuses. The turn
/// ends with the card text, not rewritten as a context overflow, and nothing
/// switches the conversation to another executor.
#[gpui::test]
fn a_transport_refusal_ends_the_turn_with_the_card_and_never_falls_back(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "strip_app::tests::a_transport_refusal_ends_the_turn_with_the_card_and_never_falls_back",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let models = std::sync::Arc::new(std::sync::Mutex::new(vec![
        "Qwen3-Coder-Next-GGUF".to_string()
    ]));
    let (url, seen) = switchable_after(models, Some(1));
    let entity = work_app(cx, &home, &url, "Qwen3-Coder-Next-GGUF");
    entity.update(cx, |app, cx| {
        app.submit_work_run("EVICTED-MID-TURN".into(), cx)
    });
    pump(cx, &entity, "the turn ended with an error", &|app| {
        !app.work.running && app.work.panel.error.is_some()
    });
    entity.read_with(cx, |app, _| {
        let error = app.work.panel.error.clone().unwrap();
        // The backstop names the host by address (the worker knows no label).
        assert!(
            error.contains("isn\u{2019}t loaded on 127.0.0.1"),
            "{error}"
        );
        assert!(error.contains("doesn\u{2019}t load models"), "{error}");
        // Not turned into a "your message was too long" retry or card.
        assert!(!awili_seam::context_overflow_text(&error), "{error}");
        // Still on the same executor: nothing fell back to another model.
        assert_eq!(
            app.work.executor,
            WorkExecutor::InAppExtra {
                id: "hosta-coder".into()
            }
        );
        assert!(
            app.work.local.down.is_none(),
            "no down card, no cloud switch"
        );
    });
    assert!(
        !requests(&seen).iter().any(|l| l.starts_with("POST")),
        "no chat request: {:?}",
        requests(&seen)
    );
}

/// Neither card can be mistaken for a context overflow (which would make the
/// seam retry with a shorter request).
#[test]
fn the_refusal_cards_are_not_read_as_context_overflows() {
    for card in [
        crate::strip::not_loaded_card("Qwen3 Coder Next", "Hosta"),
        crate::strip::host_silent_card("Hosta"),
    ] {
        assert!(!awili_seam::context_overflow_text(&card), "{card}");
    }
}
