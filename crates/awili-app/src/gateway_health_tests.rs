//! Proof tests for the U2a gateway health core.
//!
//! The network tests bind only `127.0.0.1:0` stub servers (the same pattern
//! the awili-gateway `rest.rs` stall tests use) and open no window.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::work::{ProgressStep, RunPanel, StepState};

/// The stub server helper from the spec: bind `127.0.0.1:0`, hand the
/// accepted socket to a handler thread, return the URL.
fn stub<F>(handler: F) -> String
where
    F: FnOnce(std::net::TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handler(stream);
        }
    });
    url
}

/// A stub that reads the request, then writes one raw HTTP status line +
/// `content-length: <body>` body and closes.
fn stub_http(status: &str, body: &str) -> String {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stub(move |mut stream| {
        let mut data = [0u8; 4096];
        let _ = stream.read(&mut data);
        let _ = stream.write_all(response.as_bytes());
    })
}

/// A stub like [`stub_http`] that also counts every accepted connection:
/// returns `(url, hits)`. A 3xx stub can point its redirect at the *other*
/// stub, so the test can assert the redirect was never followed.
fn stub_http_counting(
    status: &str,
    body: &str,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    thread::spawn(move || loop {
        let (stream, _) = match listener.accept() {
            Ok(v) => v,
            Err(_) => break,
        };
        let mut stream = stream;
        counter.fetch_add(1, Ordering::SeqCst);
        let mut data = [0u8; 4096];
        let _ = stream.read(&mut data);
        let _ = stream.write_all(response.as_bytes());
    });
    (url, hits)
}

#[test]
fn u2a_probe_ok_on_http_200() {
    let url = stub_http("200 OK", "ok");
    assert_eq!(probe_dashboard(&url, PROBE_TIMEOUT), Ok(()));
}

#[test]
fn u2a_fix_3xx_not_followed() {
    // The redirect target: a second, independent stub. If the probe ever
    // follows the 302, it lands here and its counter moves.
    let (target_url, target_hits) = stub_http_counting("200 OK", "targeted");
    // The probe target answers 302 with a Location at the second stub.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let url = format!("http://{}", listener.local_addr().expect("local addr"));
    let response =
        format!("HTTP/1.1 302 Found\r\nlocation: {target_url}\r\ncontent-length: 0\r\n\r\n");
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut data = [0u8; 4096];
            let _ = stream.read(&mut data);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    // Per U2a a 3xx IS "up": the probe succeeds on the 302 itself…
    assert_eq!(
        probe_dashboard(&url, PROBE_TIMEOUT),
        Ok(()),
        "a 3xx from the dashboard must count as reachable"
    );
    // …and it must NEVER follow the Location: the second stub sees zero requests.
    assert_eq!(
        target_hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the redirect target must receive zero requests — redirects are never followed"
    );
}

#[test]
fn u2a_probe_http_500_is_down() {
    let url = stub_http("500 Internal Server Error", "");
    let err = probe_dashboard(&url, PROBE_TIMEOUT).expect_err("500 must fail");
    assert!(
        err.contains("500"),
        "the error names the HTTP status: {err}"
    );
}

#[test]
fn u2a_probe_refused_fails_fast() {
    // Bind, take the port, drop the listener: nothing listens on it.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    let url = format!("http://{addr}");
    let start = Instant::now();
    let err = probe_dashboard(&url, PROBE_TIMEOUT).expect_err("refused must fail");
    let elapsed = start.elapsed();
    assert!(
        elapsed < PROBE_TIMEOUT,
        "connection refused must fail fast, took {elapsed:?}: {err}"
    );
}

#[test]
fn u2a_probe_silent_server_times_out() {
    // Accept and hold the socket without ever writing a response.
    let url = stub(|mut stream| {
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        thread::sleep(Duration::from_secs(5));
    });
    let start = Instant::now();
    let err = probe_dashboard(&url, Duration::from_millis(300))
        .expect_err("a silent server must time out");
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "the probe must give up on the 300 ms bound, took {elapsed:?}: {err}"
    );
}

#[test]
fn u2a_send_gate_rules() {
    // Default: never checked, so nothing is fresh.
    let h = GatewayHealth::default();
    let t0 = Instant::now();
    assert_eq!(h.send_gate(t0), SendGate::Probe);

    // Fresh Up sends while fresh.
    let mut up = GatewayHealth::default();
    up.finish_probe(Ok(()), t0);
    assert_eq!(up.send_gate(t0 + Duration::from_secs(19)), SendGate::Send);
    // A stale Up falls back to probing.
    assert_eq!(up.send_gate(t0 + Duration::from_secs(21)), SendGate::Probe);

    // A fresh Down refuses with the unreachable message and the detail.
    let mut down = GatewayHealth::default();
    down.finish_probe(Err("refused".into()), t0);
    match down.send_gate(t0 + Duration::from_secs(5)) {
        SendGate::Refuse(message) => {
            assert!(message.starts_with(GATEWAY_UNREACHABLE), "{message}");
            assert!(message.contains("refused"), "{message}");
        }
        other => panic!("fresh Down must refuse, got {other:?}"),
    }

    // While a probe is in flight no result — fresh or stale — decides the
    // send: the gate stays on Probe.
    let mut probing = GatewayHealth::default();
    probing.finish_probe(Ok(()), t0);
    assert!(probing.begin_probe());
    assert_eq!(probing.send_gate(t0), SendGate::Probe);
    assert_eq!(
        probing.send_gate(t0 + Duration::from_secs(19)),
        SendGate::Probe
    );
    // A fresh Down never flips to Refuse while a probe is in flight either.
    let mut probing_down = GatewayHealth::default();
    probing_down.finish_probe(Err("refused".into()), t0);
    assert!(probing_down.begin_probe());
    assert_eq!(
        probing_down.send_gate(t0 + Duration::from_secs(5)),
        SendGate::Probe
    );
}

#[test]
fn u2a_begin_probe_is_exclusive() {
    let mut h = GatewayHealth::default();
    assert!(h.begin_probe(), "first claim wins");
    assert!(
        !h.begin_probe(),
        "a second claim is refused while in flight"
    );
    assert!(h.probing());
    h.finish_probe(Ok(()), Instant::now());
    assert!(!h.probing(), "finish_probe clears the flag");
    assert!(
        h.begin_probe(),
        "a probe can be claimed again after a finish"
    );
}

#[test]
fn u2a_idle_probe_cadence() {
    let t0 = Instant::now();
    // Never checked: due.
    let h = GatewayHealth::default();
    assert!(h.idle_probe_due(t0));

    // Checked at t0: not due at t0+14 s, due at t0+15 s.
    let mut h = GatewayHealth::default();
    h.finish_probe(Ok(()), t0);
    assert!(!h.idle_probe_due(t0 + Duration::from_secs(14)));
    assert!(h.idle_probe_due(t0 + Duration::from_secs(15)));

    // Never due while a probe is in flight.
    let mut h = GatewayHealth::default();
    assert!(h.begin_probe());
    assert!(!h.idle_probe_due(t0 + Duration::from_secs(60)));
}

#[test]
fn u2a_dead_link_fails_turn_within_30s() {
    // Up at turn start, then the link dies: every probe after that fails its
    // full timeout. The first Fail must arrive at some s ≤ 30.
    let t0 = Instant::now();
    let mut h = GatewayHealth::default();
    h.finish_probe(Ok(()), t0);
    let mut failed_at: Option<u32> = None;
    for s in 1..=60u32 {
        let now = t0 + Duration::from_secs(u64::from(s));
        let decision = h.run_watch(now, Duration::from_secs(u64::from(s)));
        if let RunWatch::Fail(message) = decision {
            failed_at = Some(s);
            assert!(
                message.starts_with(GATEWAY_UNREACHABLE),
                "the failure message is the unreachable one: {message}"
            );
            break;
        }
        if decision == RunWatch::Probe {
            assert!(h.begin_probe());
            // The probe runs to its full timeout: it finishes 3 s later.
            h.finish_probe(Err("refused".into()), now + Duration::from_secs(3));
        }
    }
    let s = failed_at.expect("a dead link fails the turn within the simulated minute");
    assert!(s <= 30, "worst case is ~27 s, got s = {s}");
}

#[test]
fn u2a_slow_but_reachable_is_never_failed() {
    // The same loop over 600 s, with every probe succeeding (3 s later):
    // never Fail, and every 15 s window from s = 10 onward probes at least once.
    let t0 = Instant::now();
    let mut h = GatewayHealth::default();
    h.finish_probe(Ok(()), t0);
    let mut probes: Vec<u32> = Vec::new();
    let mut failed = false;
    for s in 1..=600u32 {
        let now = t0 + Duration::from_secs(u64::from(s));
        let decision = h.run_watch(now, Duration::from_secs(u64::from(s)));
        if let RunWatch::Fail(_) = decision {
            failed = true;
            break;
        }
        if decision == RunWatch::Probe {
            assert!(h.begin_probe());
            probes.push(s);
            h.finish_probe(Ok(()), now + Duration::from_secs(3));
        }
    }
    assert!(!failed, "a slow but reachable gateway is never failed");
    for start in (10..=600).step_by(15) {
        let end = start + 15;
        assert!(
            probes.iter().any(|&p| (start..end).contains(&p)),
            "no probe in window {start}..{end}: {probes:?}"
        );
    }
}

#[test]
fn u2a_after_probe_outcomes() {
    let t0 = Instant::now();
    let pending = || PendingSend {
        text: "ship it".into(),
        conv_id: "c1".into(),
    };

    // Up in the same place: submit the original text.
    let mut up = GatewayHealth::default();
    up.finish_probe(Ok(()), t0);
    match after_probe(pending(), up.reach(), true) {
        AfterProbe::Submit(text) => assert_eq!(text, "ship it"),
        other => panic!("expected Submit, got {other:?}"),
    }

    // Up, but the user moved on: nothing for the current composer; the
    // text goes to the clipboard (U2a-FIX2).
    match after_probe(pending(), up.reach(), false) {
        AfterProbe::MovedOn { clipboard, notice } => {
            assert_eq!(clipboard, "ship it");
            assert!(notice.contains("copied to the clipboard"), "{notice}");
        }
        other => panic!("expected MovedOn, got {other:?}"),
    }

    // Down: refuse, with the unreachable message and the original text.
    let mut down = GatewayHealth::default();
    down.finish_probe(Err("refused".into()), t0);
    match after_probe(pending(), down.reach(), true) {
        AfterProbe::Refuse { text, message } => {
            assert_eq!(text, "ship it");
            assert!(message.starts_with(GATEWAY_UNREACHABLE), "{message}");
            assert!(message.contains("refused"), "{message}");
        }
        other => panic!("expected Refuse, got {other:?}"),
    }

    // Unknown: restore.
    let unknown = GatewayHealth::default();
    match after_probe(pending(), unknown.reach(), true) {
        AfterProbe::Restore(text) => assert_eq!(text, "ship it"),
        other => panic!("expected Restore, got {other:?}"),
    }
}

#[test]
fn u2a_fix_down_after_moving_on_restores_nothing() {
    // Probe failed (Down) but the user changed conversation since the send
    // started (same_place = false): the outcome is the "moved on" restore —
    // no Refuse with draft restoration into the current composer, for any
    // parked text.
    let t0 = Instant::now();
    let mut down = GatewayHealth::default();
    down.finish_probe(Err("refused".into()), t0);
    let pending = PendingSend {
        text: "moved-on draft".into(),
        conv_id: "c1".into(),
    };
    match after_probe(pending, down.reach(), false) {
        AfterProbe::MovedOn { clipboard, notice } => {
            assert_eq!(clipboard, "moved-on draft");
            assert!(notice.contains(GATEWAY_UNREACHABLE), "{notice}");
            assert!(notice.contains("copied to the clipboard"), "{notice}");
        }
        AfterProbe::Restore(_) => panic!(
            "a Down probe failure after the user moved on must NOT restore into \
             the current composer (U2a-FIX2)"
        ),
        AfterProbe::Refuse { .. } => panic!(
            "a Down probe failure after the user moved on must NOT refuse into \
             the current composer"
        ),
        AfterProbe::Submit(_) => panic!("a Down result must never submit"),
    }
}

/// U2a-FIX2: every moved-on outcome (Up, Down or Unknown) puts nothing in
/// the current composer; the FULL unsent text, byte for byte, is handed to
/// the clipboard and the notice says so.
#[test]
fn u2a_fix2_moved_on_goes_to_the_clipboard() {
    let t0 = Instant::now();
    let mut up = GatewayHealth::default();
    up.finish_probe(Ok(()), t0);
    let mut down = GatewayHealth::default();
    down.finish_probe(Err("refused".into()), t0);
    let unknown = GatewayHealth::default();
    let text = "  first line of a long message that goes well past forty characters\n\nsecond  ";
    for health in [&up, &down, &unknown] {
        let pending = PendingSend {
            text: text.into(),
            conv_id: "c1".into(),
        };
        match after_probe(pending, health.reach(), false) {
            AfterProbe::MovedOn { clipboard, notice } => {
                assert_eq!(clipboard, text, "the exact full text goes to the clipboard");
                assert!(notice.starts_with("Not sent"), "{notice}");
                assert!(notice.contains("copied to the clipboard"), "{notice}");
            }
            other => panic!("moved on must be MovedOn, got {other:?}"),
        }
    }
    let notice = moved_on_notice(down.reach());
    assert!(notice.contains(GATEWAY_UNREACHABLE), "{notice}");
    assert!(notice.contains("refused"), "{notice}");
}

#[test]
fn u2a_activity_watch_idles_and_resets() {
    let t0 = Instant::now();
    let mut watch = ActivityWatch::default();
    let first = watch.observe(7, t0);
    assert_eq!(
        first,
        Duration::ZERO,
        "the first observation restarts the clock"
    );
    let idle = watch.observe(7, t0 + Duration::from_secs(25));
    assert_eq!(idle, Duration::from_secs(25));
    // A new fingerprint restarts the clock to zero.
    let moved = watch.observe(8, t0 + Duration::from_secs(30));
    assert_eq!(moved, Duration::ZERO);
    // reset restarts from scratch: the next observation is fresh.
    watch.reset();
    let after_reset = watch.observe(8, t0 + Duration::from_secs(60));
    assert_eq!(after_reset, Duration::ZERO);
}

#[test]
fn u2a_fingerprint_moves_with_visible_progress() {
    let panel = RunPanel::default();
    let fp = |p: &RunPanel, text: usize, act: usize| activity_fingerprint(p, text, act);
    let start = fp(&panel, 0, 0);

    // Identical inputs hash equal.
    assert_eq!(fp(&panel, 0, 0), start);

    // Each visible change moves the hash.
    let mut changed = [start; 5];
    let mut p = RunPanel::default();
    p.streamed.push('x');
    changed[0] = fp(&p, 0, 0);
    p.steps.push(ProgressStep {
        label: "step".into(),
        state: StepState::Running,
    });
    changed[1] = fp(&p, 0, 0);
    p.activity = Some("working…".into());
    changed[2] = fp(&p, 0, 0);
    p.approval = Some(crate::work::ApprovalCard {
        approval_id: "a1".into(),
        kind: "command".into(),
        prompt: "ok?".into(),
        risk: awili_work::run_events::ApprovalRisk::Privileged,
        delivery: crate::work::ApprovalDelivery::default(),
    });
    changed[3] = fp(&p, 0, 0);
    changed[4] = fp(&p, 1, 0);

    // A fresh default panel has the same fingerprint as `start` again, so
    // comparing the mutated chain against each other is what matters:
    assert_eq!(fp(&RunPanel::default(), 0, 0), start);
    for i in 0..changed.len() {
        for j in 0..changed.len() {
            if i != j {
                assert_ne!(
                    changed[i], changed[j],
                    "visible change {i} → {j} must move the fingerprint"
                );
            }
        }
    }
}
