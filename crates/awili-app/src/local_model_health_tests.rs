//! Proof tests for the L1e1 local model health core. The network tests bind
//! only `127.0.0.1:0` stub servers (U2a's pattern) and open no window.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::gateway_health::PROBE_TIMEOUT;

/// Bind `127.0.0.1:0`; the accepted request's first line goes to the
/// returned receiver, then `status` is answered. Returns the base URL.
fn stub(status: &'static str) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut data = [0u8; 4096];
            let n = stream.read(&mut data).unwrap_or(0);
            let text = String::from_utf8_lossy(&data[..n]).to_string();
            let _ = tx.send(text.lines().next().unwrap_or_default().to_string());
            let body = "{\"data\":[]}";
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    (url, rx)
}

fn configured() -> EngineConfig {
    EngineConfig {
        base_url: "http://192.0.2.10:8851".into(),
        model: "/m/gemma-4-12b".into(),
        label: "Gemma 4 12B (Studio)".into(),
        ..Default::default()
    }
}

fn checked(result: Result<(), String>, at: Instant) -> LocalModelHealth {
    let mut h = LocalModelHealth::default();
    assert!(h.health.begin_probe());
    h.health.finish_probe(result, at);
    h
}

#[test]
fn l1e1_probe_hits_v1_models() {
    let (url, rx) = stub("200 OK");
    assert_eq!(probe_model_endpoint(&url, PROBE_TIMEOUT), Ok(()));
    let line = rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(line.starts_with("GET /v1/models "), "{line}");

    let (url, rx) = stub("200 OK");
    assert_eq!(
        probe_model_endpoint(&format!("{url}/"), PROBE_TIMEOUT),
        Ok(())
    );
    let line = rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(line.starts_with("GET /v1/models "), "{line}");
    assert_eq!(
        models_url(" http://h:1/ "),
        "http://h:1/v1/models",
        "trailing slash and spaces trimmed"
    );
}

#[test]
fn l1e1_probe_errors() {
    let (url, _rx) = stub("404 Not Found");
    assert_eq!(
        probe_model_endpoint(&url, PROBE_TIMEOUT),
        Err("HTTP 404".to_string())
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let dead = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let started = Instant::now();
    let err = probe_model_endpoint(&dead, PROBE_TIMEOUT).unwrap_err();
    assert!(!err.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "bounded by the timeout"
    );
}

#[test]
fn l1e1_gate_matrix() {
    let now = Instant::now();
    let cfg = configured();
    let blank = EngineConfig::default();
    assert_eq!(
        checked(Ok(()), now).gate(&blank, now),
        LocalGate::Refuse(DownReason::NotConfigured),
        "not configured wins even over a fresh Up"
    );
    assert_eq!(
        LocalModelHealth::default().gate(&cfg, now),
        LocalGate::Probe
    );
    assert_eq!(checked(Ok(()), now).gate(&cfg, now), LocalGate::Send);
    assert_eq!(
        checked(Err("x".into()), now).gate(&cfg, now),
        LocalGate::Refuse(DownReason::Unreachable("x".into()))
    );
    let old = now.checked_sub(Duration::from_secs(21)).unwrap();
    assert_eq!(checked(Ok(()), old).gate(&cfg, now), LocalGate::Probe);
    let mut probing = checked(Ok(()), now);
    assert!(probing.health.begin_probe());
    assert_eq!(probing.gate(&cfg, now), LocalGate::Probe);
}

#[test]
fn l1e1_gate_applies_only_to_idle_in_app() {
    assert!(gate_applies(false, false, &WorkExecutor::InApp));
    assert!(!gate_applies(true, false, &WorkExecutor::InApp));
    assert!(!gate_applies(false, true, &WorkExecutor::InApp));
    assert!(!gate_applies(false, false, &WorkExecutor::Gateway));
    assert!(!gate_applies(
        false,
        false,
        &WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None
        }
    ));
    assert!(!gate_applies(
        false,
        false,
        &WorkExecutor::LocalBuild {
            target_id: "t".into()
        }
    ));
}

#[test]
fn l1e1_escalation_target() {
    let claude = WorkExecutor::Agent {
        engine: CodeEngine::Claude,
        model: None,
    };
    assert_eq!(escalation_target(None), claude);
    assert_eq!(
        escalation_target(Some("agent:codex:gpt-6-sol")),
        WorkExecutor::Agent {
            engine: CodeEngine::Codex,
            model: Some("gpt-6-sol".into())
        }
    );
    for other in ["gateway", "in-app", "local-build:x", "junk"] {
        assert_eq!(escalation_target(Some(other)), claude, "{other}");
    }
}

#[test]
fn l1e1_after_local_probe() {
    let pending = || PendingSend {
        text: "hello".into(),
        conv_id: "a".into(),
    };
    assert_eq!(
        after_local_probe(pending(), &Reach::Up, true),
        AfterLocalProbe::Submit("hello".into())
    );
    assert_eq!(
        after_local_probe(pending(), &Reach::Down("refused".into()), true),
        AfterLocalProbe::Park(LocalDown {
            reason: DownReason::Unreachable("refused".into()),
            text: Some("hello".into()),
            conv_id: Some("a".into()),
        })
    );
    assert_eq!(
        after_local_probe(pending(), &Reach::Unknown, true),
        AfterLocalProbe::Park(LocalDown {
            reason: DownReason::Unreachable("no answer".into()),
            text: Some("hello".into()),
            conv_id: Some("a".into()),
        })
    );
    assert_eq!(
        after_local_probe(pending(), &Reach::Up, false),
        AfterLocalProbe::MovedOn {
            clipboard: "hello".into(),
            notice: MOVED_ON_NOTICE.into(),
        },
        "moved on: never submitted, never into another composer"
    );
}

#[test]
fn l1e1_down_copy_is_plain_and_honest() {
    let cfg = configured();
    let down = DownReason::Unreachable("connection refused".into());
    let kept = down_body(&down, &cfg, true);
    assert!(kept.contains("Gemma 4 12B (Studio)"), "{kept}");
    assert!(kept.contains("http://192.0.2.10:8851"), "{kept}");
    assert!(kept.contains("connection refused"), "{kept}");
    assert!(kept.contains("Nothing was sent"), "{kept}");
    let after_failure = down_body(&down, &cfg, false);
    assert!(
        !after_failure.contains("Nothing was sent"),
        "{after_failure}"
    );
    assert!(after_failure.contains("didn't switch"), "{after_failure}");
    let unset = down_body(&DownReason::NotConfigured, &cfg, true);
    // R2d: the not-set-up cause is plain words (it used to name [engine]).
    for jargon in ["[engine]", "config.toml", "base_url", "All fields"] {
        assert!(!unset.contains(jargon), "{jargon} in {unset}");
    }
    for text in [
        kept,
        after_failure,
        unset,
        down_title(&down).to_string(),
        down_title(&DownReason::NotConfigured).to_string(),
    ] {
        let lower = text.to_lowercase();
        for banned in ["fallback", "automatically", "switched to"] {
            assert!(!lower.contains(banned), "{banned} in {text}");
        }
    }
}

#[test]
fn l1e1_in_app_status_part() {
    let l = "Gemma";
    assert_eq!(
        in_app_part(l, true, &Reach::Up, false),
        ("\u{25CF} In-app \u{00B7} Gemma".to_string(), ACCENT)
    );
    assert_eq!(
        in_app_part(l, true, &Reach::Unknown, true),
        ("\u{25CB} checking local model\u{2026}".to_string(), MUTED)
    );
    assert_eq!(
        in_app_part(l, true, &Reach::Unknown, false),
        ("\u{25CB} In-app \u{00B7} Gemma".to_string(), MUTED)
    );
    assert_eq!(
        in_app_part(l, true, &Reach::Down("x".into()), false),
        ("\u{2717} Local model not answering".to_string(), CHIP_WARN)
    );
    assert_eq!(
        in_app_part(l, false, &Reach::Up, false),
        ("\u{2717} No local model set up".to_string(), CHIP_WARN)
    );
}

/// RC2-RUNF (review P3): a pending second check belongs to the endpoint that
/// missed; a new endpoint starts with none.
#[test]
fn rc2runf_an_endpoint_change_forgets_the_pending_second_check() {
    let mut h = checked(Err("timed out".into()), Instant::now());
    h.rechecking = true;
    let gen = h.endpoint_gen;
    h.endpoint_changed();
    assert_eq!(h.endpoint_gen, gen.wrapping_add(1));
    assert!(!h.rechecking);
}

/// RC2-GEMMA5: a stub answering each request by its path: `/v1/models`
/// with `models`, `/health` with `health` (`None`: the server has no
/// `/health`, it answers 404). Answers up to two requests.
fn stub_paths(models: &'static str, health: Option<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut data = [0u8; 4096];
            let n = stream.read(&mut data).unwrap_or(0);
            let line = String::from_utf8_lossy(&data[..n])
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            let (status, body) = if line.starts_with("GET /health ") {
                match health {
                    Some("503 Service Unavailable") => {
                        ("503 Service Unavailable", "{\"status\": \"unavailable\"}")
                    }
                    Some(s) => (s, "{\"status\": \"ok\"}"),
                    None => ("404 Not Found", "Not Found"),
                }
            } else {
                (models, "{\"data\":[]}")
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    url
}

/// RC2-GEMMA5 (R7-A3): mlx_lm still answers `/v1/models` after its
/// generation thread died; its `/health` says 503 — then the model is down,
/// in plain words. A healthy server, or one without `/health`, is up.
#[test]
fn rc2gemma5_a_server_whose_health_says_503_is_down() {
    assert_eq!(
        probe_model_endpoint(
            &stub_paths("200 OK", Some("503 Service Unavailable")),
            PROBE_TIMEOUT
        ),
        Err(NOT_WORKING.to_string())
    );
    assert_eq!(
        probe_model_endpoint(&stub_paths("200 OK", Some("200 OK")), PROBE_TIMEOUT),
        Ok(())
    );
    assert_eq!(
        probe_model_endpoint(&stub_paths("200 OK", None), PROBE_TIMEOUT),
        Ok(())
    );
    assert_eq!(
        probe_model_endpoint(&stub_paths("404 Not Found", Some("200 OK")), PROBE_TIMEOUT),
        Err("HTTP 404".to_string())
    );
    assert_eq!(health_url(" http://h:1/ "), "http://h:1/health");
    let card = down_body(
        &DownReason::Unreachable(NOT_WORKING.into()),
        &configured(),
        false,
    );
    assert!(card.contains("it may need a restart"), "{card}");
}
