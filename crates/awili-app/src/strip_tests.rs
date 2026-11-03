//! The status strip's pure half: words, parsing, telemetry, the derived view,
//! and the two read-only Lemonade reads against loopback servers.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

pub(crate) type Seen = Arc<Mutex<Vec<String>>>;

/// A loopback HTTP server: `routes` maps a request path to (status, body);
/// any other path is a 404. Every request line is kept. With `silent` the
/// server accepts and never answers.
pub(crate) fn serve(routes: Vec<(&'static str, u16, String)>, silent: bool) -> (String, Seen) {
    let seen: Seen = Default::default();
    let keep = seen.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let keep = keep.clone();
            let routes = routes.clone();
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
                        // Read the whole body too, or the close resets the client.
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
                if let Some(auth) = text
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                {
                    keep.lock().unwrap().push(auth.to_ascii_lowercase());
                }
                if silent {
                    std::thread::sleep(Duration::from_secs(30));
                    return;
                }
                let path = line.split_whitespace().nth(1).unwrap_or_default();
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| *p == path)
                    .map(|(_, st, b)| (*st, b.clone()))
                    .unwrap_or((404, "not found".to_string()));
                let ctype = if body.starts_with("data:") {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nConnection: close\r\n\
                     Content-Length: {}\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    (format!("http://{addr}"), seen)
}

pub(crate) fn health_body(models: &[&str]) -> String {
    let all: Vec<serde_json::Value> = models
        .iter()
        .map(|m| serde_json::json!({"model_name": m, "type": "llm", "device": "gpu"}))
        .collect();
    serde_json::json!({"status": "ok", "model_loaded": models.first(), "all_models_loaded": all})
        .to_string()
}

// ---- words and numbers -----------------------------------------------------------

#[test]
fn tokens_and_speeds_read_the_way_the_design_says() {
    assert_eq!(tokens_short(412), "412");
    assert_eq!(tokens_short(1_300), "1.3K");
    assert_eq!(tokens_short(18_700), "18.7K");
    assert_eq!(tokens_short(262_000), "262K");
    assert_eq!(speed_short(41.2), "41");
    assert_eq!(speed_long(47.24), "47.2");
    assert_eq!(first_token_text(0.376), "0.38");
    assert_eq!(group(1_912), "1,912");
    assert_eq!(group(604), "604");
    assert_eq!(group(1_234_567), "1,234,567");
}

#[test]
fn ages_are_plain() {
    assert_eq!(age_text(Duration::from_secs(1)), "just now");
    assert_eq!(age_text(Duration::from_secs(6)), "6 s ago");
    assert_eq!(age_text(Duration::from_secs(125)), "2 min ago");
    assert_eq!(age_text(Duration::from_secs(7300)), "2 h ago");
}

#[test]
fn the_server_is_named_from_the_label_then_the_address() {
    assert_eq!(host_name("Qwen3 Coder Next (Hosta)", "http://x:1"), "Hosta");
    assert_eq!(
        host_name("Gemma 4 26B", "http://studio.local:8080/v1"),
        "studio.local:8080"
    );
    assert_eq!(host_name("", ""), "The server");
    assert_eq!(
        host_of("http://10.0.0.5:13305/"),
        Some("10.0.0.5:13305".into())
    );
}

#[test]
fn context_text_splits_into_typed_runs() {
    let segs = context_segs("18.7K of 262K");
    let words: Vec<_> = segs.iter().map(|s| (s.text.as_str(), s.weight)).collect();
    assert_eq!(
        words,
        vec![
            ("18.7K", Weight::Value),
            ("of", Weight::Of),
            ("262K", Weight::Window)
        ]
    );
    // The fallback window reads "about 200K": still two quiet words, then the window.
    let about = context_segs("43.6K of about 200K");
    assert_eq!(about.last().unwrap().weight, Weight::Window);
    assert_eq!(about.len(), 4);
}

// ---- urls and parsing --------------------------------------------------------------

#[test]
fn lemonade_urls_hang_off_the_server_root() {
    for base in [
        "http://h:13305",
        "http://h:13305/",
        "http://h:13305/v1",
        "http://h:13305/api/v1/",
    ] {
        assert_eq!(health_url(base), "http://h:13305/api/v1/health", "{base}");
        assert_eq!(
            stats_url(base),
            "http://h:13305/api/v1/system-stats",
            "{base}"
        );
    }
}

#[test]
fn health_parses_objects_and_names_and_rejects_other_servers() {
    let r = parse_health(&health_body(&["Qwen3-Coder-Next-GGUF", "kokoro-v1"])).unwrap();
    assert!(r.has("Qwen3-Coder-Next-GGUF"));
    assert!(r.has("qwen3-coder-next-gguf"), "case does not matter");
    assert!(r.has("kokoro-v1"));
    assert!(!r.has("Fleet-Ornith-Omni-Planner"));
    assert!(!r.has(""), "a blank model is never resident");

    let plain = parse_health(r#"{"status":"ok","all_models_loaded":["user.My-Model"]}"#).unwrap();
    assert!(plain.has("My-Model"), "the user. prefix is the same model");

    let empty = parse_health(r#"{"status":"ok","all_models_loaded":[]}"#).unwrap();
    assert!(empty.names.is_empty() && !empty.has("anything"));

    // Not Lemonade: no all_models_loaded, no status, not JSON.
    assert_eq!(parse_health(r#"{"status":"ok"}"#), None);
    assert_eq!(parse_health(r#"{"all_models_loaded":[]}"#), None);
    assert_eq!(parse_health("data: {\"choices\":[]}"), None);
    assert_eq!(parse_health("[]"), None);
}

#[test]
fn system_stats_follow_the_lemonade_status_bar() {
    let body = r#"{"cpu_percent": 12.5, "memory_gb": 41.2, "gpu_percent": 62.0,
                   "vram_gb": 3.1, "npu_percent": 18.0}"#;
    let s = parse_system_stats(body).unwrap();
    assert_eq!(s.gpu_percent, Some(62.0));
    assert_eq!(s.npu_percent, Some(18.0));
    assert_eq!(s.memory_used_gb, Some(41.2));
    assert_eq!(
        s.memory_total_gb, None,
        "no total unless the server says one"
    );

    let with_total = parse_system_stats(r#"{"memory_gb": 41.2, "memory_total_gb": 64.0}"#).unwrap();
    assert_eq!(with_total.memory_total_gb, Some(64.0));

    // The status bar's "unknown" is a zero memory and null percents.
    assert_eq!(
        parse_system_stats(r#"{"cpu_percent": 3, "memory_gb": 0, "gpu_percent": null}"#),
        None
    );
    assert_eq!(parse_system_stats("nope"), None);
}

// ---- telemetry ------------------------------------------------------------------------

fn at(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

#[test]
fn a_streaming_reply_has_a_live_speed_once_there_is_enough_to_say() {
    let t0 = Instant::now();
    let mut t = Telemetry::default();
    t.turn_started("c", t0);
    let live = t.live("c", at(t0, 100)).unwrap();
    assert!(!live.has_first_token && live.tokens_per_s.is_none());

    t.token("c", Some(t0), at(t0, 400));
    t.token("c", Some(t0), at(t0, 500));
    assert!(
        t.live("c", at(t0, 500)).unwrap().tokens_per_s.is_none(),
        "too early"
    );
    // 11 chunks over one second after the first: 10 intervals -> 10 tok/s.
    for i in 2..=10 {
        t.token("c", Some(t0), at(t0, 400 + i * 100));
    }
    let live = t.live("c", at(t0, 1400)).unwrap();
    assert!(live.has_first_token);
    let tps = live.tokens_per_s.unwrap();
    assert!((tps - 10.0).abs() < 0.01, "{tps}");
}

#[test]
fn a_finished_reply_keeps_speed_first_token_and_the_engines_counts() {
    let t0 = Instant::now();
    let mut t = Telemetry::default();
    t.turn_started("c", t0);
    for i in 0..21 {
        t.token("c", Some(t0), at(t0, 380 + i * 50));
    }
    t.usage("c", Some(t0), 1912, 21);
    t.turn_ended("c");
    assert!(t.live("c", at(t0, 5000)).is_none());
    let last = t.last("c").unwrap();
    assert_eq!(last.input, Some(1912));
    assert_eq!(last.output, Some(21));
    assert!((last.first_token_s.unwrap() - 0.38).abs() < 0.001);
    // 20 intervals over one second.
    assert!((last.tokens_per_s.unwrap() - 20.0).abs() < 0.01);
}

#[test]
fn a_failed_turn_leaves_the_last_replys_numbers_alone() {
    let t0 = Instant::now();
    let mut t = Telemetry::default();
    for i in 0..10 {
        t.token("c", Some(t0), at(t0, 100 + i * 100));
    }
    t.turn_ended("c");
    let before = t.last("c").unwrap();
    t.turn_started("c", at(t0, 5000));
    t.turn_ended("c");
    assert_eq!(t.last("c"), Some(before));
    assert!(t.last("other").is_none(), "another thread has nothing");
}

#[test]
fn a_turn_whose_start_is_unknown_never_invents_a_first_token_time() {
    let t0 = Instant::now();
    let mut t = Telemetry::default();
    for i in 0..5 {
        t.token("c", None, at(t0, i * 100));
    }
    t.turn_ended("c");
    assert_eq!(t.last("c").unwrap().first_token_s, None);
}

// ---- the derived view -----------------------------------------------------------------------

fn local() -> Where {
    Where::Local {
        host: "Hosta".into(),
        addr: "hosta.internal:13305".into(),
        model: "Qwen3 Coder Next".into(),
    }
}

fn base(place: Where) -> Inputs {
    Inputs {
        place,
        running: false,
        live: None,
        last: None,
        reach: Reach::Up,
        reach_age: Some(Duration::from_secs(2)),
        lemonade: false,
        resident: None,
        host: None,
        host_age: None,
        context_text: Some("18.7K of 262K".into()),
        context_used: Some(18_700),
        context_window: Some(262_144),
        spend_today: None,
        loading_after: Duration::from_millis(1500),
    }
}

fn say(segs: &[Seg]) -> String {
    segs.iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn nothing_is_shown_until_a_value_exists() {
    let v = derive(&base(local()));
    assert_eq!(v.state, State::Ready);
    assert_eq!(v.dot, DotKind::Idle);
    assert!(v.left.is_empty(), "a dot and nothing else");
    assert_eq!(say(&v.right), "18.7K of 262K");
    assert!(!v.retry);

    let mut i = base(local());
    i.context_text = None;
    let v = derive(&i);
    assert!(v.left.is_empty() && v.right.is_empty());
}

#[test]
fn local_ready_shows_the_last_replys_speed() {
    let mut i = base(local());
    i.last = Some(ReplyStats {
        tokens_per_s: Some(41.2),
        first_token_s: Some(0.38),
        input: Some(1912),
        output: Some(604),
    });
    let v = derive(&i);
    assert_eq!(say(&v.left), "41 tok/s");
    assert_eq!(v.dot, DotKind::Idle);
    // The popover has every number.
    let rows: Vec<_> = v
        .detail
        .reply
        .iter()
        .map(|(k, s)| (k.as_str(), say(s)))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("Speed", "41.2 tok/s".to_string()),
            ("First token", "0.38 s".to_string()),
            ("Tokens", "1,912 in \u{b7} 604 out".to_string()),
        ]
    );
    let ctx = v.detail.context.unwrap();
    assert_eq!(ctx.percent, "7.1%");
    assert!((ctx.fraction - 0.0713).abs() < 0.001);
}

#[test]
fn streaming_breathes_and_ticks() {
    let mut i = base(local());
    i.running = true;
    i.live = Some(LiveSnapshot {
        tokens_per_s: Some(46.8),
        has_first_token: true,
        waiting: Duration::from_secs(3),
    });
    let v = derive(&i);
    assert_eq!(v.state, State::Streaming);
    assert_eq!(v.dot, DotKind::Live);
    assert_eq!(say(&v.left), "47 tok/s");
    assert_eq!(say(&v.right), "18.7K of 262K");
    assert_eq!(v.detail.state.word(), "streaming");
}

#[test]
fn cloud_after_a_reply_says_tokens_in_and_out() {
    let mut i = base(Where::Cloud {
        model: "Claude Haiku".into(),
        plan: Plan::Subscription,
    });
    i.last = Some(ReplyStats {
        tokens_per_s: None,
        first_token_s: None,
        input: Some(1_300),
        output: Some(412),
    });
    i.reach = Reach::Unknown;
    i.reach_age = None;
    let v = derive(&i);
    assert_eq!(say(&v.left), "1.3K in \u{b7} 412 out");
    assert_eq!(v.dot, DotKind::Idle);
    assert_eq!(v.detail.cloud[0].0, "Plan");
    assert_eq!(say(&v.detail.cloud[0].1), "Included");
    assert!(v.detail.host.is_none());
    assert!(!v.detail.check_now, "no check for a cloud model");
    assert!(v.detail.age.is_none());
    // Cloud models are never "unreachable": nothing is probed for them.
    let mut down = i.clone();
    down.reach = Reach::Down("x".into());
    assert_eq!(derive(&down).state, State::Ready);
}

#[test]
fn a_metered_cloud_model_shows_todays_spend() {
    let mut i = base(Where::Cloud {
        model: "DeepSeek".into(),
        plan: Plan::Metered,
    });
    i.spend_today = Some("$0.0124".into());
    let v = derive(&i);
    let rows: Vec<_> = v
        .detail
        .cloud
        .iter()
        .map(|(k, s)| (k.as_str(), say(s)))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("Plan", "Pay as you go".to_string()),
            ("Today", "$0.0124".to_string())
        ]
    );
}

#[test]
fn unreachable_is_red_says_one_sentence_offers_retry_and_hides_context() {
    let mut i = base(local());
    i.reach = Reach::Down("connection refused".into());
    i.reach_age = Some(Duration::from_secs(12));
    let v = derive(&i);
    assert_eq!(v.state, State::Unreachable);
    assert_eq!(v.dot, DotKind::Alert);
    assert_eq!(v.left_tone, LeftTone::Alert);
    assert_eq!(say(&v.left), "Hosta isn\u{2019}t responding");
    assert!(v.retry);
    assert!(
        v.right.is_empty(),
        "context is useless on a host that cannot answer"
    );
    assert!(v.detail.context.is_none());
    assert_eq!(v.detail.age.as_deref(), Some("last checked 12 s ago"));
}

#[test]
fn a_reply_already_streaming_is_not_called_unreachable() {
    let mut i = base(local());
    i.reach = Reach::Down("old".into());
    i.running = true;
    i.live = Some(LiveSnapshot {
        tokens_per_s: None,
        has_first_token: true,
        waiting: Duration::from_secs(1),
    });
    assert_eq!(derive(&i).state, State::Streaming);
}

#[test]
fn loading_is_green_and_waits_a_beat_before_it_speaks() {
    let mut i = base(local());
    i.running = true;
    i.live = Some(LiveSnapshot {
        tokens_per_s: None,
        has_first_token: false,
        waiting: Duration::from_millis(400),
    });
    assert_eq!(derive(&i).state, State::Ready, "a quick start says nothing");
    i.live = Some(LiveSnapshot {
        tokens_per_s: None,
        has_first_token: false,
        waiting: Duration::from_millis(1600),
    });
    let v = derive(&i);
    assert_eq!(v.state, State::Loading);
    assert_eq!(v.dot, DotKind::Attention);
    assert_eq!(v.left_tone, LeftTone::Attention);
    assert_eq!(say(&v.left), "Loading Qwen3 Coder Next\u{2026}");
}

#[test]
fn a_lemonade_model_that_is_not_resident_is_green_and_named() {
    let mut i = base(local());
    i.lemonade = true;
    i.resident = Some(Resident {
        names: vec!["kokoro-v1".into()],
    });
    let v = derive(&i);
    assert_eq!(v.state, State::NotLoaded);
    assert_eq!(v.dot, DotKind::Attention);
    assert_eq!(
        say(&v.left),
        "Qwen3 Coder Next isn\u{2019}t loaded on Hosta"
    );
    assert!(v.retry && v.right.is_empty());

    // Resident: quiet again.
    i.resident = Some(Resident {
        names: vec!["Qwen3 Coder Next".into()],
    });
    assert_eq!(derive(&i).state, State::Ready);
    // Not Lemonade: the list means nothing.
    i.lemonade = false;
    i.resident = Some(Resident::default());
    assert_eq!(derive(&i).state, State::Ready);
}

#[test]
fn the_host_block_appears_only_when_the_server_reports_it() {
    let mut i = base(local());
    i.lemonade = true;
    i.resident = Some(Resident {
        names: vec!["Qwen3 Coder Next".into()],
    });
    assert!(
        derive(&i).detail.host.is_none(),
        "nothing reported, no block"
    );

    i.host = Some(HostStats {
        gpu_percent: Some(62.0),
        npu_percent: Some(18.0),
        memory_used_gb: Some(41.2),
        memory_total_gb: Some(64.0),
    });
    i.host_age = Some(Duration::from_secs(6));
    let v = derive(&i);
    let (name, rows) = v.detail.host.unwrap();
    assert_eq!(name, "Hosta");
    let rows: Vec<_> = rows.iter().map(|(k, s)| (k.as_str(), say(s))).collect();
    assert_eq!(
        rows,
        vec![
            ("Graphics", "62 %".to_string()),
            ("Neural processor", "18 %".to_string()),
            ("Memory", "41.2 of 64 GB".to_string()),
        ]
    );
    assert_eq!(v.detail.age.as_deref(), Some("as of 6 s ago"));

    // Without a reported total, memory is just the used figure.
    i.host = Some(HostStats {
        memory_used_gb: Some(41.2),
        ..Default::default()
    });
    let rows = derive(&i).detail.host.unwrap().1;
    assert_eq!(say(&rows[0].1), "41.2 GB");

    // A server that is not Lemonade never gets a host block.
    i.lemonade = false;
    assert!(derive(&i).detail.host.is_none());
}

// ---- probe bookkeeping ---------------------------------------------------------------------------

#[test]
fn reachability_is_read_every_fifteen_seconds_and_host_stats_every_five_when_wanted() {
    let t0 = Instant::now();
    let mut p = EngineProbe::default();
    assert!(p.reach_due(t0), "never read: due");
    p.reach_in_flight = true;
    assert!(!p.reach_due(t0), "one read at a time");
    p.apply(
        Reading {
            reach: Reach::Up,
            resident: Some(Resident::default()),
            lemonade: true,
        },
        t0,
    );
    assert!(!p.reach_due(at(t0, 14_000)));
    assert!(p.reach_due(at(t0, 15_000)));

    assert!(
        !p.host_due(at(t0, 1_000), false),
        "only when a turn runs or the popover is open"
    );
    assert!(p.host_due(at(t0, 1_000), true));
    p.host_at = Some(t0);
    assert!(!p.host_due(at(t0, 4_999), true));
    assert!(p.host_due(at(t0, 5_000), true));

    // A host that is not Lemonade is never asked for stats.
    p.apply(
        Reading {
            reach: Reach::Up,
            resident: None,
            lemonade: false,
        },
        at(t0, 20_000),
    );
    assert!(!p.host_due(at(t0, 99_000), true));
    assert!(p.host.is_none());
}

// ---- the two reads, against loopback servers -------------------------------------------------------

#[test]
fn residency_reads_health_and_nothing_else() {
    let (url, seen) = serve(
        vec![(
            "/api/v1/health",
            200,
            health_body(&["Qwen3-Coder-Next-GGUF"]),
        )],
        false,
    );
    let t = Duration::from_secs(2);
    match check_residency(&url, "Qwen3-Coder-Next-GGUF", None, t) {
        Residency::Resident(r) => assert!(r.has("Qwen3-Coder-Next-GGUF")),
        other => panic!("{other:?}"),
    }
    match check_residency(&url, "Fleet-Ornith-Omni-Planner", None, t) {
        Residency::NotResident(r) => assert_eq!(r.names, vec!["Qwen3-Coder-Next-GGUF".to_string()]),
        other => panic!("{other:?}"),
    }
    let lines = seen.lock().unwrap().clone();
    assert_eq!(lines.len(), 2);
    assert!(
        lines.iter().all(|l| l == "GET /api/v1/health HTTP/1.1"),
        "only the health read: {lines:?}"
    );
}

#[test]
fn a_server_that_is_not_lemonade_is_left_alone() {
    // mlx_lm / llama.cpp style: 404 on /api/v1/health.
    let (url, _) = serve(vec![], false);
    assert_eq!(
        check_residency(&url, "m", None, Duration::from_secs(2)),
        Residency::NotLemonade
    );
    // Refused connection: the turn will report it as it always did.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    assert_eq!(
        check_residency(&dead, "m", None, Duration::from_secs(2)),
        Residency::NotLemonade
    );
}

#[test]
fn a_first_seen_host_that_never_answers_is_silent_nothing_is_sent() {
    let (url, _) = serve(vec![], true);
    assert_eq!(
        check_residency(&url, "m", None, Duration::from_millis(300)),
        Residency::Silent
    );
}

#[test]
fn a_5xx_or_a_redirect_is_no_answer_but_a_404_is_a_definitive_no() {
    let t = Duration::from_secs(2);
    for status in [500u16, 502, 503, 504] {
        let (url, _) = serve(vec![("/api/v1/health", status, "busy".into())], false);
        assert_eq!(
            check_residency(&url, "m", None, t),
            Residency::Silent,
            "{status}"
        );
    }
    let (url, _) = serve(vec![("/api/v1/health", 404, "no".into())], false);
    assert_eq!(check_residency(&url, "m", None, t), Residency::NotLemonade);
    // 2xx with another server's shape is a definitive no too.
    let (url, _) = serve(
        vec![("/api/v1/health", 200, r#"{"ok":true}"#.into())],
        false,
    );
    assert_eq!(check_residency(&url, "m", None, t), Residency::NotLemonade);
}

#[test]
fn the_known_lemonade_set_is_persisted_and_reloads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known.json");
    forget_known_for_test();
    init_known_store(path.clone());
    remember_lemonade("http://hosta-persist.test:13305/v1");
    assert!(is_known_lemonade("http://hosta-persist.test:13305"));
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("http://hosta-persist.test:13305"), "{saved}");
    // A restart: memory gone, the file remains.
    forget_known_for_test();
    assert!(!is_known_lemonade("http://hosta-persist.test:13305"));
    init_known_store(path);
    assert!(is_known_lemonade("http://hosta-persist.test:13305"));
    forget_known_for_test();
}

#[test]
fn a_refused_connection_stays_as_before_unless_the_host_is_known() {
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let t = Duration::from_secs(1);
    assert_eq!(check_residency(&dead, "m", None, t), Residency::NotLemonade);
    remember_lemonade(&dead);
    assert_eq!(check_residency(&dead, "m", None, t), Residency::Silent);
}

#[test]
fn engine_reads_learn_reachability_and_whether_it_is_lemonade() {
    let (url, seen) = serve(
        vec![
            ("/v1/models", 200, r#"{"data":[]}"#.to_string()),
            ("/health", 404, String::new()),
            ("/api/v1/health", 200, health_body(&["kokoro-v1"])),
            (
                "/api/v1/system-stats",
                200,
                r#"{"gpu_percent": 5, "memory_gb": 9.5, "npu_percent": null}"#.to_string(),
            ),
        ],
        false,
    );
    let r = read_engine(&url, None, Duration::from_secs(2));
    assert_eq!(r.reach, Reach::Up);
    assert!(r.lemonade);
    assert!(r.resident.unwrap().has("kokoro-v1"));
    let stats = fetch_host_stats(&url, None, Duration::from_secs(2)).unwrap();
    assert_eq!(stats.gpu_percent, Some(5.0));
    assert_eq!(stats.memory_used_gb, Some(9.5));
    for line in seen.lock().unwrap().iter() {
        assert!(line.starts_with("GET "), "read-only: {line}");
        assert!(
            !line.contains("/load") && !line.contains("chat"),
            "never a load or a chat: {line}"
        );
    }

    // No /v1/models answer: down, and health is never asked.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    let r = read_engine(&dead, None, Duration::from_secs(1));
    assert!(matches!(r.reach, Reach::Down(_)));
    assert!(!r.lemonade);
}

// ---- review round 2 --------------------------------------------------------------------

#[test]
fn only_a_404_is_a_definitive_no_a_401_or_403_is_refused() {
    let t = Duration::from_secs(2);
    for status in [401u16, 403, 405] {
        let (url, _) = serve(vec![("/api/v1/health", status, "no".into())], false);
        assert_eq!(
            check_residency(&url, "m", None, t),
            Residency::Silent,
            "{status}"
        );
    }
    let (url, _) = serve(vec![], false);
    assert_eq!(check_residency(&url, "m", None, t), Residency::NotLemonade);
}

#[test]
fn the_engines_bearer_is_sent_on_the_health_read() {
    let (url, seen) = serve(vec![("/api/v1/health", 200, health_body(&["m"]))], false);
    let r = check_residency(&url, "m", Some("sk-secret"), Duration::from_secs(2));
    assert!(matches!(r, Residency::Resident(_)), "{r:?}");
    let lines = seen.lock().unwrap().clone();
    assert!(
        lines.contains(&"authorization: bearer sk-secret".to_string()),
        "{lines:?}"
    );
    // And none when no key is configured.
    let (url2, seen2) = serve(vec![("/api/v1/health", 200, health_body(&["m"]))], false);
    let _ = check_residency(&url2, "m", None, Duration::from_secs(2));
    assert!(seen2
        .lock()
        .unwrap()
        .iter()
        .all(|l| !l.starts_with("authorization")));
}

#[test]
fn model_loaded_alone_is_lemonade_and_a_garbled_body_fails_closed() {
    let old = r#"{"status":"ok","model_loaded":"Qwen3-Coder-Next-GGUF"}"#;
    let r = parse_health(old).unwrap();
    assert!(r.has("Qwen3-Coder-Next-GGUF") && !r.has("other"));
    let none = parse_health(r#"{"status":"ok","model_loaded":null}"#).unwrap();
    assert!(none.names.is_empty());
    assert_eq!(
        health_shape(r#"{"status":"ok","all_models_loaded":"nope"}"#),
        HealthShape::Unparsable
    );
    assert_eq!(
        health_shape(r#"{"status":"ok","model_loaded":7}"#),
        HealthShape::Unparsable
    );
    assert_eq!(health_shape(r#"{"status":"ok"}"#), HealthShape::Other);
    assert_eq!(health_shape("<html>"), HealthShape::Other);
    // Over the wire: Lemonade-looking but unreadable is no answer.
    let (url, _) = serve(
        vec![(
            "/api/v1/health",
            200,
            r#"{"status":"ok","all_models_loaded":"x"}"#.into(),
        )],
        false,
    );
    assert_eq!(
        check_residency(&url, "m", None, Duration::from_secs(2)),
        Residency::Silent
    );
    // An old build with model_loaded only, model resident / not.
    let (url2, _) = serve(vec![("/api/v1/health", 200, old.into())], false);
    assert!(matches!(
        check_residency(&url2, "Qwen3-Coder-Next-GGUF", None, Duration::from_secs(2)),
        Residency::Resident(_)
    ));
    assert!(matches!(
        check_residency(&url2, "Other", None, Duration::from_secs(2)),
        Residency::NotResident(_)
    ));
}

#[test]
fn the_guard_is_scoped_by_host_public_clouds_are_never_probed() {
    for yes in [
        "http://127.0.0.1:8080",
        "http://localhost:11434/v1",
        "http://10.0.0.5:13305",
        "http://172.16.1.1",
        "http://172.31.255.1",
        "http://10.77.68.105:13305",
        "http://100.64.0.1",
        "http://100.127.255.254",
        "http://169.254.10.10",
        "http://[::1]:8000",
        "http://[fd12:3456::1]:8000",
        "http://hosta:13305",
        "https://hosta.tail-example.ts.net",
        "http://models.example.internal:8842",
        "http://box.local",
        "http://nas.lan",
    ] {
        assert!(guarded_host(yes), "{yes} is watched");
    }
    for no in [
        "https://api.deepseek.com",
        "https://api.anthropic.com/v1",
        "https://api.openai.com",
        "http://8.8.8.8",
        "http://172.32.0.1",
        "http://100.128.0.1",
        "http://100.63.255.255",
        "https://open.bigmodel.cn",
        "",
    ] {
        assert!(!guarded_host(no), "{no:?} is never probed");
    }
    // Known-Lemonade hosts are watched wherever they are.
    remember_lemonade("https://lemonade.example.org");
    assert!(guarded_host("https://lemonade.example.org/v1"));
}

#[test]
fn a_cached_reading_serves_the_transport_until_it_ages_out() {
    let (url, seen) = serve(
        vec![("/api/v1/health", 200, health_body(&["kokoro-v1"]))],
        false,
    );
    assert!(
        cached_residency(&url, "m", Duration::from_secs(5)).is_none(),
        "nothing yet"
    );
    let _ = check_residency(&url, "m", None, Duration::from_secs(2));
    assert!(matches!(
        cached_residency(&url, "m", Duration::from_secs(5)),
        Some(Residency::NotResident(_))
    ));
    assert!(matches!(
        cached_residency(&url, "kokoro-v1", Duration::from_secs(5)),
        Some(Residency::Resident(_))
    ));
    assert!(
        cached_residency(&url, "m", Duration::ZERO).is_none(),
        "too old for a zero age"
    );
    assert_eq!(seen.lock().unwrap().len(), 1, "the cache read nothing");
}

// ---- review round 3 --------------------------------------------------------------------

#[test]
fn the_host_is_parsed_the_way_the_request_is() {
    // Guarded: userinfo, backslashes, short and numeric IPv4 forms, the
    // unspecified address, IPv4-mapped IPv6, IPv6 link-local.
    for yes in [
        "http://user:pw@10.77.1.5:80",
        "http://evil.com@10.77.1.5",
        "http:\\\\10.77.1.5\\v1",
        "http://127.1",
        "http://0x7f.1",
        "http://0x7f000001",
        "http://2130706433",
        "http://017700000001",
        "http://0.0.0.0:8080",
        "http://[::]:8080",
        "http://[::ffff:10.77.1.5]",
        "http://[::ffff:127.0.0.1]:9",
        "http://[::ffff:100.64.0.1]",
        "http://[fe80::1]:8000",
        "http://[febf::1]",
        "http://[fc00::1]",
    ] {
        assert!(guarded_host(yes), "{yes} is watched");
    }
    // Not guarded: public addresses, however they are written.
    for no in [
        "http://[::ffff:8.8.8.8]",
        "http://[2001:db8::1]",
        "http://[fec0::1]",
        "http://user@api.deepseek.com",
        "http://134744072",
        "http://0x08080808",
    ] {
        assert!(!guarded_host(no), "{no} is not");
    }
    assert_eq!(
        host_of("http://user:pw@10.77.1.5:8080/v1"),
        Some("10.77.1.5:8080".into())
    );
    assert_eq!(host_of("http://127.1/x"), Some("127.0.0.1".into()));
}

#[test]
fn a_provider_name_that_resolves_to_the_lan_is_guarded_a_public_one_is_not() {
    let t = Duration::from_millis(50);
    // A public-style name (a tunnelled name, split-horizon DNS) on the LAN.
    set_test_resolution("hosta.example.org", &["10.77.68.105"]);
    set_test_resolution("tunnel.example.net", &["100.101.102.103"]);
    set_test_resolution("fe80ed.example.net", &["fe80::1"]);
    set_test_resolution("mapped.example.net", &["::ffff:10.0.0.9"]);
    // A cloud API: public name, public addresses only.
    set_test_resolution("api.deepseek.com", &["104.18.1.1", "104.18.2.2"]);
    // A name with one private and one public address is guarded.
    set_test_resolution("both.example.net", &["8.8.8.8", "10.1.2.3"]);
    for yes in [
        "https://hosta.example.org/v1",
        "https://tunnel.example.net",
        "http://fe80ed.example.net:8000",
        "http://mapped.example.net",
        "http://both.example.net",
    ] {
        assert!(provider_host_guarded(yes, t), "{yes} resolves to home");
    }
    assert!(
        !provider_host_guarded("https://api.deepseek.com", t),
        "public stays unwatched"
    );
    // A resolution failure is "not guarded" on a provider route.
    assert!(!provider_host_guarded(
        "https://never-resolves.example.net",
        t
    ));
    // Names that are guarded by their shape need no lookup.
    assert!(provider_host_guarded("http://hosta.internal", t));
    assert!(provider_host_guarded("http://10.77.1.2", t));
}
