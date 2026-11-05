//! `[[engines]]`: additional in-app models next to the primary `[engine]`.
//! Picker order and labels, the `in-app:<id>` executor string, per-turn
//! routing to the selected engine's own endpoint and key, the unknown-id
//! error, and the guarantee that no `[[engines]]` leaves today's single-engine
//! behaviour untouched.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use awili_config::{Config, EngineConfig, ExtraEngineConfig, ModelsConfig};
use awili_core::ConvKind;
use gpui::AppContext;

use crate::executor_labels::{self, in_app_executors, picker_sections};
use crate::run_error::{classify, RunErrorCategory};
use crate::work::{unknown_engine_notice, Mode, WorkExecutor};
use crate::AwiliApp;

const CHILD: &str = "AWILI_MULTI_ENGINE_TEST_HOME";

fn extra(id: &str, label: &str, base: &str, model: &str, key: &str) -> ExtraEngineConfig {
    ExtraEngineConfig {
        id: id.into(),
        base_url: base.into(),
        model: model.into(),
        api_key: key.into(),
        label: label.into(),
        dialect: "chat-completions".into(),
        window_tokens: 65_536,
        request_timeout_s: 30,
        max_output_tokens: None,
    }
}

fn primary(base: &str) -> EngineConfig {
    EngineConfig {
        kind: awili_config::EngineKind::Awili,
        base_url: base.into(),
        model: "/m/gemma".into(),
        api_key: "sk-primary-KEY".into(),
        label: "Gemma 4 26B (Studio)".into(),
        ..Default::default()
    }
}

fn in_app(id: &str) -> WorkExecutor {
    WorkExecutor::InAppExtra { id: id.into() }
}

// ---- picker -------------------------------------------------------------------

#[test]
fn picker_lists_extras_after_the_primary_in_config_order() {
    let extras = [
        extra(
            "qwen-hostd",
            "Qwen 3.8 27B (hostd)",
            "http://q:8000",
            "qwen",
            "",
        ),
        extra("llama-box", "Llama 4 (box)", "http://l:8000", "llama", ""),
    ];
    let sections = picker_sections(
        &primary("http://p:1"),
        &extras,
        &ModelsConfig::default(),
        &BTreeMap::new(),
    );
    assert_eq!(sections[0].header, executor_labels::THIS_MAC_HEADER);
    assert_eq!(
        sections[0].rows,
        vec![
            (
                "In-app \u{00B7} Gemma 4 26B (Studio)".to_string(),
                WorkExecutor::InApp
            ),
            (
                "In-app \u{00B7} Qwen 3.8 27B (hostd)".to_string(),
                in_app("qwen-hostd")
            ),
            (
                "In-app \u{00B7} Llama 4 (box)".to_string(),
                in_app("llama-box")
            ),
        ]
    );
    // The other sections are untouched.
    assert_eq!(sections.len(), 3);
}

#[test]
fn picker_without_extras_is_the_single_in_app_row() {
    let sections = picker_sections(
        &primary("http://p:1"),
        &[],
        &ModelsConfig::default(),
        &BTreeMap::new(),
    );
    assert_eq!(
        sections[0].rows,
        vec![(
            "In-app \u{00B7} Gemma 4 26B (Studio)".to_string(),
            WorkExecutor::InApp
        )]
    );
    assert_eq!(in_app_executors(&[]), vec![WorkExecutor::InApp]);
}

#[test]
fn the_composer_chooser_lists_extras_and_flags_an_unfinished_one() {
    let extras = [
        extra("ok", "Qwen", "http://q:8000", "qwen", ""),
        extra("half", "Half done", "", "m", ""),
    ];
    let models = ModelsConfig::default();
    let agents = awili_config::AgentBinsConfig::default();
    let engine = primary("http://p:1");
    let sections = executor_labels::target_sections(&executor_labels::TargetReach {
        engine: &engine,
        extras: &extras,
        models: &models,
        agents: &agents,
        build_targets: &BTreeMap::new(),
        gateway_set_up: true,
    });
    let rows = &sections[0].rows;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].target, WorkExecutor::InApp);
    assert_eq!(rows[1].target, in_app("ok"));
    assert_eq!(rows[1].label, "In-app \u{00B7} Qwen");
    assert_eq!(rows[1].unavailable, None);
    assert_eq!(rows[2].target, in_app("half"));
    assert!(rows[2].unavailable.is_some(), "no address: not runnable");
    // Entries are shown even when their server may be down: nothing is probed.
}

#[test]
fn chip_and_short_labels_follow_the_selected_extra() {
    let extras = [extra("q", "Qwen", "http://q:8000", "qwen", "")];
    let none = BTreeMap::new();
    let p = primary("http://p:1");
    assert_eq!(
        executor_labels::chip_label(&in_app("q"), &p, &extras, &none),
        "In-app \u{00B7} Qwen"
    );
    assert_eq!(
        executor_labels::short_target(&in_app("q"), &extras, &none),
        "In-app \u{00B7} Qwen"
    );
    // A removed entry is named as not set up, never as the primary.
    assert_eq!(
        executor_labels::chip_label(&in_app("gone"), &p, &extras, &none),
        "In-app \u{00B7} gone (not set up)"
    );
}

// ---- executor string ----------------------------------------------------------

#[test]
fn executor_string_round_trips() {
    let e = in_app("qwen-hostd");
    assert_eq!(e.to_persist(), "in-app:qwen-hostd");
    assert_eq!(
        WorkExecutor::from_persist("in-app:qwen-hostd"),
        Some(e.clone())
    );
    // The primary keeps its plain string.
    assert_eq!(WorkExecutor::InApp.to_persist(), "in-app");
    assert_eq!(
        WorkExecutor::from_persist("in-app"),
        Some(WorkExecutor::InApp)
    );
    // A malformed id is unrecognizable, like any other bad value.
    assert_eq!(WorkExecutor::from_persist("in-app:"), None);
    assert_eq!(WorkExecutor::from_persist("in-app:has space"), None);
    // `[work] default_executor` seeds from it too.
    assert_eq!(
        crate::work::seed_executor(None, Some("in-app:qwen-hostd")),
        Some(e.clone())
    );
    // The per-conversation route persists it and reads it back.
    let route = crate::conversation_route::ConversationRoute::new(e.clone());
    let raw = route.to_persisted_json().unwrap();
    let back = crate::conversation_route::ConversationRoute::from_persisted_json(&raw).unwrap();
    assert_eq!(back.next_target(), &e);
}

// ---- resolution and the unknown id --------------------------------------------

#[test]
fn an_extra_resolves_to_its_own_engine_and_the_primary_is_unchanged() {
    let p = primary("http://p:1");
    let extras = [extra("q", "Qwen", "http://q:8000", "qwen", "sk-qwen-KEY")];
    let eff = crate::local_first::effective_engine(&in_app("q"), &p, &extras);
    assert_eq!(eff.kind, awili_config::EngineKind::Awili);
    assert_eq!(eff.base_url, "http://q:8000");
    assert_eq!(eff.model, "qwen");
    assert_eq!(eff.api_key, "sk-qwen-KEY");
    assert_eq!(eff.window_tokens, 65_536);
    assert_eq!(eff.request_timeout_s, 30);
    let prim = crate::local_first::effective_engine(&WorkExecutor::InApp, &p, &extras);
    assert_eq!(prim.base_url, "http://p:1");
    assert_eq!(prim.api_key, "sk-primary-KEY");
}

#[test]
fn an_unknown_id_is_a_setup_error_and_never_the_primary() {
    let p = primary("http://p:1");
    let eff = crate::local_first::effective_engine(&in_app("gone"), &p, &[]);
    assert!(!eff.is_configured(), "blank, not the primary: {eff:?}");
    let msg = unknown_engine_notice("gone");
    assert!(msg.contains("\"gone\""), "{msg}");
    let class = classify(&msg);
    assert_eq!(class.category, RunErrorCategory::LocalSetup, "{msg}");
    assert!(crate::work::is_engine_setup_notice(&msg));
}

// ---- turns --------------------------------------------------------------------

/// Run `test` in a subprocess with a private HOME (as the in-app warm tests).
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

type Seen = Arc<Mutex<Vec<String>>>;

/// A loopback chat-completions server keeping every request (head and body)
/// and answering each with one short streamed reply.
fn recording_server() -> (String, Seen) {
    use std::io::{Read, Write};
    let seen: Seen = Default::default();
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
                let req = loop {
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
                            break Some(String::from_utf8_lossy(&data).into_owned());
                        }
                    }
                };
                let Some(req) = req else {
                    return;
                };
                keep.lock().unwrap().push(req);
                let chunk = |delta: serde_json::Value, finish: serde_json::Value| {
                    serde_json::json!({"id": "c", "object": "chat.completion.chunk",
                        "created": 1, "model": "m", "choices": [{"index": 0,
                        "delta": delta, "finish_reason": finish}]})
                };
                let out = format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    chunk(
                        serde_json::json!({"role": "assistant", "content": "ok"}),
                        serde_json::Value::Null
                    ),
                    chunk(serde_json::json!({}), serde_json::json!("stop")),
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

/// Pump the GPUI test scheduler until `done` holds (bounded).
fn pump(
    cx: &mut gpui::TestAppContext,
    entity: &gpui::Entity<AwiliApp>,
    what: &str,
    done: &dyn Fn(&AwiliApp) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        cx.run_until_parked();
        if entity.read_with(cx, |app, _| done(app)) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// An app in a Work conversation with a folder, the primary `[engine]` at
/// `primary_url`, and `extras`; `executor` selected.
fn work_app(
    cx: &mut gpui::TestAppContext,
    home: &std::path::Path,
    primary_url: &str,
    extras: Vec<ExtraEngineConfig>,
    executor: WorkExecutor,
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
        app.work.engine = primary(primary_url);
        app.keys.engine = Some("sk-primary-KEY".into());
        app.work.extra_engines = extras;
        app.work.executor = executor;
        let meta = app.conversations.create_kind(1_000_000, ConvKind::Work);
        app.views.insert(
            meta.id.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(&meta.id);
    });
    entity
}

fn asked(seen: &Seen, needle: &str) -> Option<String> {
    seen.lock()
        .unwrap()
        .iter()
        .find(|r| r.contains(needle))
        .cloned()
}

fn finish(cx: &mut gpui::TestAppContext, entity: &gpui::Entity<AwiliApp>) {
    entity.update(cx, |app, cx| {
        app.work.gateway_cmd_tx = None;
        cx.notify();
    });
}

/// A turn on an extra engine goes to THAT engine's server, with its model and
/// its own key: the primary's server hears nothing, and the primary's key is
/// never sent to the extra's server.
#[gpui::test]
fn a_turn_on_an_extra_engine_hits_that_engines_base_url(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::multi_engine_tests::a_turn_on_an_extra_engine_hits_that_engines_base_url")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (primary_url, seen_primary) = recording_server();
    let (extra_url, seen_extra) = recording_server();
    let entity = work_app(
        cx,
        &home,
        &primary_url,
        vec![extra(
            "qwen-hostd",
            "Qwen 3.8 27B (hostd)",
            &extra_url,
            "qwen-served-name",
            "sk-qwen-KEY",
        )],
        in_app("qwen-hostd"),
    );
    entity.update(cx, |app, cx| {
        app.submit_work_run("EXTRA-QUESTION".into(), cx)
    });
    pump(cx, &entity, "the turn ended", &|app| !app.work.running);
    let req = asked(&seen_extra, "EXTRA-QUESTION").expect("the extra's server got the question");
    assert!(req.contains("qwen-served-name"), "its model: {req}");
    let lower = req.to_ascii_lowercase();
    assert!(lower.contains("authorization: bearer sk-qwen-key"), "{req}");
    assert!(!lower.contains("sk-primary-key"), "never the primary's key");
    assert!(
        seen_primary.lock().unwrap().is_empty(),
        "the primary's server heard nothing"
    );
    finish(cx, &entity);
}

/// The primary is unchanged when extras exist: plain `in-app` still runs on
/// `[engine]` with its own key, model and server.
#[gpui::test]
fn the_primary_still_runs_on_engine_when_extras_exist(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::multi_engine_tests::the_primary_still_runs_on_engine_when_extras_exist")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (primary_url, seen_primary) = recording_server();
    let (extra_url, seen_extra) = recording_server();
    let entity = work_app(
        cx,
        &home,
        &primary_url,
        vec![extra("q", "Qwen", &extra_url, "qwen", "sk-qwen-KEY")],
        WorkExecutor::InApp,
    );
    entity.update(cx, |app, cx| {
        app.submit_work_run("PRIMARY-QUESTION".into(), cx)
    });
    pump(cx, &entity, "the turn ended", &|app| !app.work.running);
    let req = asked(&seen_primary, "PRIMARY-QUESTION").expect("the primary got the question");
    assert!(req.contains("/m/gemma"), "{req}");
    assert!(req.to_ascii_lowercase().contains("bearer sk-primary-key"));
    assert!(seen_extra.lock().unwrap().is_empty());
    finish(cx, &entity);
}

/// A restored conversation naming a removed engine ends in a plain setup
/// error, with nothing sent to the primary or any other server.
#[gpui::test]
fn an_unknown_engine_id_errors_without_falling_back(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::multi_engine_tests::an_unknown_engine_id_errors_without_falling_back")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (primary_url, seen_primary) = recording_server();
    let entity = work_app(cx, &home, &primary_url, Vec::new(), in_app("removed-one"));
    entity.update(cx, |app, cx| {
        app.submit_work_run("ORPHAN-QUESTION".into(), cx)
    });
    pump(cx, &entity, "the error shows", &|app| {
        app.work.panel.error.is_some()
    });
    entity.read_with(cx, |app, _| {
        let err = app.work.panel.error.clone().unwrap();
        assert!(err.contains("removed-one"), "{err}");
        assert_eq!(classify(&err).category, RunErrorCategory::LocalSetup);
        assert!(!app.work.running);
    });
    assert!(
        seen_primary.lock().unwrap().is_empty(),
        "no silent fallback to the primary model"
    );
    finish(cx, &entity);
}

/// An engine whose server does not answer still has its picker entry, and
/// its turn fails with the connection card -- not a setup error.
#[gpui::test]
fn an_extra_whose_server_is_down_fails_the_turn_as_a_connection_error(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::multi_engine_tests::an_extra_whose_server_is_down_fails_the_turn_as_a_connection_error",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    // A port nothing listens on.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let extras = vec![extra("down", "Qwen (down)", &dead, "qwen", "")];
    let entity = work_app(cx, &home, "http://127.0.0.1:9", extras, in_app("down"));
    // Listed whether or not it answers.
    entity.read_with(cx, |app, _| {
        let rows = picker_sections(
            &app.work.engine,
            &app.work.extra_engines,
            &app.work.models_cfg,
            &app.work.build_targets,
        );
        assert!(rows[0].rows.iter().any(|(_, e)| *e == in_app("down")));
    });
    entity.update(cx, |app, cx| {
        app.submit_work_run("DOWN-QUESTION".into(), cx)
    });
    pump(cx, &entity, "the turn failed", &|app| {
        !app.work.running && app.work.panel.error.is_some()
    });
    entity.read_with(cx, |app, _| {
        let err = app.work.panel.error.clone().unwrap();
        let class = classify(&err);
        assert_eq!(class.category, RunErrorCategory::Connection, "{err}");
    });
    finish(cx, &entity);
}

// ---- no [[engines]] ------------------------------------------------------------

#[test]
fn without_engines_the_config_loads_exactly_as_before() {
    let cfg =
        Config::parse("[engine]\nkind = \"awili\"\nbase_url = \"http://h:1\"\nmodel = \"m\"\n")
            .unwrap();
    assert!(cfg.engines.is_empty());
    let mut work = crate::work::WorkState::new(
        crate::work::HermesCfg {
            base_url: String::new(),
            api_key: String::new(),
            enabled: false,
            dashboard_url: String::new(),
        },
        crate::work::GatewayCfg {
            dashboard_url: String::new(),
            enabled: false,
        },
        cfg.engine.clone(),
    );
    AwiliApp::apply_startup_work_config(&mut work, &cfg);
    assert!(work.extra_engines.is_empty());
    assert_eq!(work.effective_engine().base_url, "http://h:1");
    assert_eq!(
        executor_labels::chip_label(
            &work.executor,
            &work.engine,
            &work.extra_engines,
            &BTreeMap::new()
        ),
        "In-app \u{00B7} m"
    );
    // A reload picks extras up and drops them again.
    let with =
        Config::parse("[[engines]]\nid = \"q\"\nbase_url = \"http://q:1\"\nmodel = \"qm\"\n")
            .unwrap();
    AwiliApp::refresh_work_endpoints(&mut work, &with);
    assert_eq!(work.extra_engines.len(), 1);
    AwiliApp::refresh_work_endpoints(&mut work, &cfg);
    assert!(work.extra_engines.is_empty());
}

// ---- council ------------------------------------------------------------------

#[test]
fn council_members_on_an_extra_use_its_endpoint_key_and_window() {
    use crate::council_run::{self, MemberRoute};
    let p = primary("http://p:1");
    let extras = [extra(
        "q",
        "Qwen",
        "http://q:8000/v1",
        "qwen",
        "sk-qwen-KEY",
    )];
    let fleet = crate::agent_chat::ChatRoute::Unavailable("none".into());
    assert!(matches!(
        council_run::member_route("in-app:q", &fleet),
        MemberRoute::InAppExtra(id) if id == "q"
    ));
    assert!(matches!(
        council_run::member_route("in-app", &fleet),
        MemberRoute::InApp
    ));
    assert_eq!(council_run::window_tokens("in-app:q", &p, &extras), 65_536);
    assert_eq!(
        council_run::window_tokens("in-app", &p, &extras),
        131_072,
        "the primary keeps its own window"
    );
    assert_eq!(
        council_run::usage_identity("in-app:q", &p, &extras, &fleet),
        ("in-app".to_string(), "qwen".to_string())
    );
    assert_eq!(council_run::work_helper_for("in-app:q"), Some(in_app("q")));
    assert_eq!(
        crate::council_view::route_label("in-app:q", &p, &extras),
        "Qwen"
    );
    assert_eq!(
        crate::council_view::route_label("in-app:gone", &p, &extras),
        "gone (not set up)"
    );
    // The tool-engine target: its own server and key, never the env/primary key.
    let agents = awili_config::AgentBinsConfig::default();
    let keys = crate::provider_build::ProviderKeys {
        anthropic: None,
        hermes: None,
        engine: Some("sk-primary-KEY".into()),
    };
    let setup = council_run::MemberSetup {
        agents: &agents,
        engine: &p,
        extras: &extras,
        fleet: &fleet,
        cwd: "/".into(),
        choices: &[],
        keys: &keys,
        hermes_base_url: "",
        connectors: None,
        council: String::new(),
        approved: false,
        sandbox: Default::default(),
    };
    let t = crate::council_exec::engine_target("in-app:q", &setup).expect("configured");
    assert_eq!(t.base_url, "http://q:8000");
    assert_eq!(t.model, "qwen");
    assert_eq!(t.api_key, "sk-qwen-KEY");
    assert!(
        crate::council_exec::engine_target("in-app:gone", &setup).is_none(),
        "an unknown id answers nothing (its card says why)"
    );
    let primary_target = crate::council_exec::engine_target("in-app", &setup).unwrap();
    assert_eq!(primary_target.api_key, "sk-primary-KEY");
}

// ---- run identity carries the engine id ---------------------------------------

fn last_run(identity: crate::work_run_identity::WorkRunIdentity) -> crate::work_last_run::LastRun {
    crate::work_last_run::LastRun {
        identity,
        model: None,
        status: awili_gateway::run_events::RunStatus::Completed,
        error: None,
        finished_at: "2026-11-01T00:00:00Z".into(),
    }
}

/// Two hosts serving the SAME model name (the Qwen-on-two-hosts case): the
/// run's id, not its model name, decides where a restored conversation goes.
#[test]
fn identity_carries_the_engine_id_and_older_records_mean_the_primary() {
    use crate::work_run_identity::WorkRunIdentity;
    let p = EngineConfig {
        model: "qwen".into(),
        ..primary("http://p:1")
    };
    let extras = [
        extra("a", "Qwen A", "http://a:1", "qwen", ""),
        extra("b", "Qwen B", "http://b:1", "qwen", ""),
    ];
    let on_b = crate::local_first::lane_identity(&in_app("b"), &p, &extras).unwrap();
    let on_primary = crate::local_first::lane_identity(&WorkExecutor::InApp, &p, &extras).unwrap();
    assert_ne!(on_b, on_primary, "same model, different engines");
    // Persisted and restored by id.
    let raw = last_run(on_b.clone()).to_json();
    let back = crate::work_last_run::LastRun::from_json(&raw).unwrap();
    assert_eq!(back.identity, on_b);
    assert_eq!(
        executor_labels::executor_for(&back.identity),
        Some(in_app("b"))
    );
    assert_eq!(
        executor_labels::executor_for(&on_primary),
        Some(WorkExecutor::InApp)
    );
    // A record written before [[engines]] has no id: the primary.
    let old = r#"{"identity":"owned_engine","engine":null,"agent_model":null,
        "owned_model":"qwen","build_model":null,"build_host":null,"model":null,
        "status":"completed","error":null,"finished_at":"2026-01-01T00:00:00Z"}"#;
    let old = crate::work_last_run::LastRun::from_json(old).unwrap();
    assert_eq!(
        old.identity,
        WorkRunIdentity::OwnedEngine {
            model: "qwen".into(),
            engine_id: None
        }
    );
    assert_eq!(
        executor_labels::executor_for(&old.identity),
        Some(WorkExecutor::InApp)
    );
}

/// "Where" names the engine the run used; "Next run" shows whenever the next
/// executor differs, in both directions.
#[test]
fn where_and_next_run_follow_the_run_not_the_next_executor() {
    let p = primary("http://primary-host:1");
    let extras = [extra("q", "Qwen", "http://qwen-host:8000", "qwen", "")];
    let none = BTreeMap::new();
    let on_extra = crate::local_first::lane_identity(&in_app("q"), &p, &extras).unwrap();
    let on_primary = crate::local_first::lane_identity(&WorkExecutor::InApp, &p, &extras).unwrap();
    // Ran on the extra, next is the primary.
    let l = executor_labels::run_lines(
        Some(&on_extra),
        &WorkExecutor::InApp,
        &p,
        &extras,
        &none,
        false,
    );
    assert_eq!(l[0], "Where: Awili on this Mac \u{00B7} model at qwen-host");
    assert_eq!(l[1], "Next run: In-app \u{00B7} Gemma 4 26B (Studio)");
    // Ran on the primary, next is the extra.
    let l = executor_labels::run_lines(Some(&on_primary), &in_app("q"), &p, &extras, &none, false);
    assert_eq!(
        l[0],
        "Where: Awili on this Mac \u{00B7} model at primary-host"
    );
    assert_eq!(l[1], "Next run: In-app \u{00B7} Qwen");
    // Same engine: no Next run line.
    let l = executor_labels::run_lines(Some(&on_extra), &in_app("q"), &p, &extras, &none, false);
    assert_eq!(l.len(), 1);
}

/// Switching between two entries on the same server and model retires the
/// live worker.
#[test]
fn switching_between_entries_on_one_server_retires_the_worker() {
    let mut work = crate::work::WorkState::new(
        crate::work::HermesCfg {
            base_url: String::new(),
            api_key: String::new(),
            enabled: false,
            dashboard_url: String::new(),
        },
        crate::work::GatewayCfg {
            dashboard_url: String::new(),
            enabled: false,
        },
        primary("http://p:1"),
    );
    work.extra_engines = vec![
        extra("a", "A", "http://same:1", "qwen", ""),
        extra("b", "B", "http://same:1", "qwen", ""),
    ];
    work.executor = in_app("a");
    let (tx, rx) = std::sync::mpsc::channel();
    work.gateway_cmd_tx = Some(tx);
    work.worker_identity =
        crate::local_first::lane_identity(&work.executor, &work.engine, &work.extra_engines);
    assert!(!crate::local_first::retire_other_lane_worker(&mut work));
    work.executor = in_app("b");
    assert!(crate::local_first::retire_other_lane_worker(&mut work));
    assert!(matches!(
        rx.try_recv(),
        Ok(crate::work::GatewayCmd::Shutdown)
    ));
}

/// A conversation whose last run was on the extra (same model name as the
/// primary) restores onto the extra, and its next turn hits the extra's server.
#[gpui::test]
fn a_restored_conversation_goes_back_to_the_extra_it_ran_on(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::multi_engine_tests::a_restored_conversation_goes_back_to_the_extra_it_ran_on",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let (primary_url, seen_primary) = recording_server();
    let (extra_url, seen_extra) = recording_server();
    // The extra serves the same model name as the primary.
    let entity = work_app(
        cx,
        &home,
        &primary_url,
        vec![extra("q", "Qwen", &extra_url, "/m/gemma", "sk-qwen-KEY")],
        WorkExecutor::InApp,
    );
    entity.update(cx, |app, cx| {
        let conv = app.sidebar.active_id().to_string();
        let id = crate::local_first::lane_identity(
            &in_app("q"),
            &app.work.engine,
            &app.work.extra_engines,
        )
        .unwrap();
        app.work_last_run
            .insert(conv.clone(), last_run(id).to_json());
        let helper =
            crate::work_last_run::conversation_helper(&app.work, &conv, &app.work_last_run);
        assert_eq!(helper, Some(in_app("q")), "not the primary");
        app.work.executor = helper.unwrap();
        app.submit_work_run("RESTORED-QUESTION".into(), cx);
    });
    pump(cx, &entity, "the turn ended", &|app| !app.work.running);
    assert!(asked(&seen_extra, "RESTORED-QUESTION").is_some());
    assert!(seen_primary.lock().unwrap().is_empty());
    finish(cx, &entity);
}

/// An extra with no `api_key` sends no Authorization header, even with
/// AWILI_ENGINE_KEY set and the primary holding a key.
#[gpui::test]
fn a_keyless_extra_sends_no_authorization_even_with_the_env_key(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::multi_engine_tests::a_keyless_extra_sends_no_authorization_even_with_the_env_key",
    ) else {
        return;
    };
    // Child process only (the parent returned above).
    std::env::set_var("AWILI_ENGINE_KEY", "sk-env-LEAK");
    cx.executor().allow_parking();
    let (primary_url, _seen_primary) = recording_server();
    let (extra_url, seen_extra) = recording_server();
    let entity = work_app(
        cx,
        &home,
        &primary_url,
        vec![extra("open", "Open box", &extra_url, "open-model", "")],
        in_app("open"),
    );
    entity.update(cx, |app, cx| {
        app.keys.engine = Some("sk-env-LEAK".into());
        app.submit_work_run("KEYLESS-QUESTION".into(), cx);
    });
    pump(cx, &entity, "the turn ended", &|app| !app.work.running);
    let req = asked(&seen_extra, "KEYLESS-QUESTION").expect("reached the extra");
    let lower = req.to_ascii_lowercase();
    assert!(!lower.contains("authorization"), "{req}");
    assert!(!lower.contains("sk-env-leak"), "{req}");
    finish(cx, &entity);
}
