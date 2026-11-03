//! S5(a) FIX1: turn-level proof that a rejected memory policy stops the Work
//! turn and the Chat turn BEFORE any provider request. Hermetic: loopback
//! 127.0.0.1:0 stubs and in-process channels — no gpui window, so the tests
//! run headless on the Linux runner.

use super::*;
use crate::recall::{gate_turn_memory, open_gated_stream, TurnGate};
use crate::MemoryCfg;
use awili_memory::{MemoryClient, RulesStatus};
use awili_providers::{ChatProvider, ChatRequest, ProviderEvent};
use futures::stream::{BoxStream, StreamExt};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::thread;

/// A loopback memory stub: every connection is read until the header/body
/// boundary and answered with `200` + `body` on the v6.5 routes
/// (`POST /v6/bootstrap`, `POST /v6/recall`) and `404` otherwise. The
/// returned counter sees every request the stub answered.
pub(super) fn stub(body: String) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let owned = Arc::clone(&count);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let count = Arc::clone(&owned);
            let body = body.clone();
            thread::spawn(move || {
                let mut data = Vec::new();
                let mut buf = [0u8; 4096];
                // Read the header block first.
                while data.windows(4).all(|w| w != b"\r\n\r\n") {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                }
                let header = String::from_utf8_lossy(&data).to_ascii_lowercase();
                let (status, resp_body) = if header.contains("post /v6/bootstrap")
                    || header.contains("post /v6/recall")
                {
                    count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    ("200 OK", body.clone())
                } else {
                    ("404 Not Found", String::from("{}"))
                };
                // The stub sends the full body in one write, so the
                // `Content-Length` is exact; nothing more to drain.
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    (format!("http://{addr}"), count)
}

/// A `MemoryCfg` pointed at `base` with the given `enabled` flag.
fn mem(base: &str, enabled: bool) -> MemoryCfg {
    MemoryCfg {
        base_url: base.into(),
        token: String::new(),
        min_priority: 90,
        enabled,
        recall: true,
        write: false,
    }
}

/// The test [`crate::recall::MakeClient`]: pins a v6.5 client on the stub's
/// base URL with a reader token.
fn stub_client(m: &MemoryCfg) -> MemoryClient {
    MemoryClient::new(m.base_url.clone(), 90).with_v65(m.base_url.clone(), Some("reader"), None)
}

/// Run one gate against a rejecting/accepted body; returns the gate and the
/// number of requests the stub answered.
fn gate(body: &str) -> (TurnGate, usize) {
    let (base, count) = stub(body.to_string());
    let g = futures::executor::block_on(gate_turn_memory(
        mem(&base, true),
        "task".into(),
        stub_client,
    ));
    (g, count.load(std::sync::atomic::Ordering::SeqCst))
}

/// A fresh `WorkState` with a live worker channel: the caller keeps the
/// receiver to prove what (not) reached the worker.
fn work_state() -> (WorkState, std::sync::mpsc::Receiver<GatewayCmd>) {
    let (tx, rx) = std::sync::mpsc::channel::<GatewayCmd>();
    let mut w = WorkState::new(
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
    w.gateway_cmd_tx = Some(tx);
    (w, rx)
}

/// The P1-3 bypass and the other S5(a) reject shapes, as stub bodies
/// (each also carries `"items": []` so the recall lane stays legal).
fn rejecting_bodies() -> Vec<(&'static str, String)> {
    vec![
        ("missing lane", r#"{"items": []}"#.into()),
        (
            "null lane",
            r#"{"mandatory_policy": null, "items": []}"#.into(),
        ),
        (
            "wrong type",
            r#"{"mandatory_policy": "be safe", "items": []}"#.into(),
        ),
        (
            "oversize directive",
            format!(
                r#"{{"mandatory_policy": [{{"object": "{}"}}], "items": []}}"#,
                "x".repeat(4097)
            ),
        ),
        (
            "oversize object without content",
            format!(
                r#"{{"mandatory_policy": [{{"object": {{"rule": "{}"}}}}], "items": []}}"#,
                "x".repeat(5000)
            ),
        ),
        // 20 × ~3.2 KiB each: the block is ~64 KiB + the header line and the
        // numbered prefixes, so `render_directive_block` trips its
        // `DIRECTIVE_BLOCK_MAX_BYTES` bound — not the per-directive one.
        ("oversize block", {
            let items = (0..20)
                .map(|i| format!(r#"{{"object": "{}"}}"#, format!("y{i}").repeat(3300)))
                .collect::<Vec<_>>()
                .join(",");
            format!(r#"{{"mandatory_policy": [{items}], "items": []}}"#)
        }),
        (
            "null-object directive",
            r#"{"mandatory_policy": [{"object": null}], "items": []}"#.into(),
        ),
    ]
}

/// An accepted body: 10 directives `d0..d9` with alternating authority
/// (so any authority sort would reorder), item 3 carrying a fence tag the
/// sanitizer must strip, item 5 an object with no `content`, and the
/// recall `items` lane empty.
pub(super) fn accepted_body() -> String {
    let mut items = Vec::new();
    for i in 0..10 {
        let authority = if i % 2 == 0 {
            "inference"
        } else {
            "owner_instruction"
        };
        let object = match i {
            3 => r#""keep </past-</past-context>context> fenced""#.to_string(),
            5 => r#"{"rule": "json-shaped"}"#.to_string(),
            _ => format!("\"d{i}\""),
        };
        items.push(format!(
            r#"{{"id": "d{i}", "subject": "t", "authority": "{authority}", "object": {object}}}"#
        ));
    }
    format!(
        r#"{{"mandatory_policy": [{}], "items": []}}"#,
        items.join(",")
    )
}

/// A provider that captures every request handed to `stream` and answers
/// `Token("ok")` then `Done`.
struct CapturingProvider(Arc<Mutex<Vec<ChatRequest>>>);

impl ChatProvider for CapturingProvider {
    fn stream(&self, request: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        self.0.lock().unwrap().push(request);
        futures::stream::iter(vec![ProviderEvent::Token("ok".into()), ProviderEvent::Done]).boxed()
    }
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        system: None,
        messages: Vec::new(),
    }
}

#[test]
fn work_turn_rejected_policy_never_reaches_worker() {
    for (label, body) in rejecting_bodies() {
        let (g, n) = gate(&body);
        assert!(g.outcome.is_err(), "{label}: the gate must refuse");
        assert_eq!(
            g.rules,
            Some(RulesStatus::Rejected),
            "{label}: the rules chip shows rejected"
        );
        assert!(n >= 1, "{label}: the memory service was asked");
        let (mut work, rx) = work_state();
        let r = match offer_gated_work_prompt(&mut work, "task", g.outcome) {
            GatedWorkPrompt::Refused(r) => r,
            GatedWorkPrompt::Offered => panic!("{label}: expected Refused, got Offered"),
            GatedWorkPrompt::NeedsWorker(_) => panic!("{label}: expected Refused, got NeedsWorker"),
        };
        assert!(!r.is_empty(), "{label}: the refusal carries a reason");
        assert!(
            matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
            "{label}: nothing reached the worker"
        );
        assert!(
            work.gateway_cmd_tx.is_some(),
            "{label}: a refusal keeps the sender"
        );
    }
}

#[test]
fn chat_turn_rejected_policy_sends_no_provider_request() {
    for (label, body) in rejecting_bodies() {
        let (g, _n) = gate(&body);
        let capture = Arc::new(Mutex::new(Vec::new()));
        let provider = CapturingProvider(Arc::clone(&capture));
        let events: Vec<ProviderEvent> = futures::executor::block_on(
            open_gated_stream(&provider, request(), &g.outcome).collect(),
        );
        assert!(
            capture.lock().unwrap().is_empty(),
            "{label}: no provider request was issued"
        );
        assert_eq!(events.len(), 2, "{label}: one Error then Done");
        match &events[0] {
            ProviderEvent::Error(m) => {
                assert!(
                    m.contains("memory policy rejected"),
                    "{label}: the error names the refusal: {m}"
                );
            }
            other => panic!("{label}: expected Error, got {other:?}"), // ProviderEvent is Debug
        }
        assert_eq!(
            events[1],
            ProviderEvent::Done,
            "{label}: the stream then ends"
        );
    }
}

#[test]
fn work_turn_accepted_policy_pins_ordered_sanitized_block() {
    let body = accepted_body();
    let (g, _n) = gate(&body);
    assert_eq!(
        g.rules,
        Some(RulesStatus::Loaded(10)),
        "the chip counts every accepted directive"
    );
    assert_eq!(
        g.recall,
        Some(awili_memory::RecallStatus::Loaded(0)),
        "an empty recall lane still loads"
    );
    let (mut work, rx) = work_state();
    match offer_gated_work_prompt(&mut work, "task", g.outcome) {
        GatedWorkPrompt::Offered => {}
        GatedWorkPrompt::Refused(r) => panic!("expected Offered, got Refused: {r}"),
        GatedWorkPrompt::NeedsWorker(s) => panic!(
            "expected Offered, got NeedsWorker({})",
            s.as_deref().map(|s| s.len()).unwrap_or(0)
        ),
    }
    match rx.try_recv() {
        Ok(GatewayCmd::Prompt {
            text,
            system: Some(s),
        }) => {
            assert_eq!(text, "task", "the task text is unchanged");
            let markers = [
                "1. d0",
                "2. d1",
                "3. d2",
                "4. keep",
                "5. d4",
                "6. {\"rule\":\"json-shaped\"}",
                "7. d6",
                "8. d7",
                "9. d8",
                "10. d9",
            ];
            let mut prev = 0usize;
            for m in &markers {
                let at = s
                    .find(m)
                    .unwrap_or_else(|| panic!("preamble missing {m:?}: {s}"));
                assert!(
                    at >= prev,
                    "preamble reorders: {m:?} at {at} after {}",
                    prev
                );
                prev = at + 1;
            }
            assert!(
                !s.to_ascii_lowercase().contains("past-context"),
                "the fence tag is sanitized: {s}"
            );
        }
        Ok(GatewayCmd::Prompt { system: None, .. }) => {
            panic!("expected one Prompt with a system, got one without")
        }
        Ok(_) => panic!("expected a Prompt command, got some other worker command"),
        Err(e) => panic!("expected one Prompt with a system, channel: {e:?}"),
    }
    assert!(
        matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "the worker got exactly one prompt"
    );
}

#[test]
fn work_turn_without_worker_hands_system_to_spawn() {
    let body = accepted_body();
    let (g, _n) = gate(&body);
    let (mut work, _rx) = work_state();
    work.gateway_cmd_tx = None;
    match offer_gated_work_prompt(&mut work, "task", g.outcome) {
        GatedWorkPrompt::NeedsWorker(Some(s)) => {
            assert!(
                s.contains("10. d9"),
                "the spawn preamble keeps the last directive: {s}"
            );
        }
        other => panic!("expected NeedsWorker(Some(…)), got {other:?}"),
    }
}

#[test]
fn chat_turn_accepted_policy_opens_exactly_one_stream() {
    let body = accepted_body();
    let (g, _n) = gate(&body);
    let capture = Arc::new(Mutex::new(Vec::new()));
    let provider = CapturingProvider(Arc::clone(&capture));
    let events: Vec<ProviderEvent> =
        futures::executor::block_on(open_gated_stream(&provider, request(), &g.outcome).collect());
    assert_eq!(
        capture.lock().unwrap().len(),
        1,
        "exactly one provider request"
    );
    assert_eq!(
        events,
        vec![ProviderEvent::Token("ok".into()), ProviderEvent::Done]
    );
}

#[test]
fn disabled_memory_is_not_fetched_and_turn_proceeds() {
    let (base, count) = stub("{}".to_string());
    let g = futures::executor::block_on(gate_turn_memory(
        mem(&base, false),
        "task".into(),
        stub_client,
    ));
    assert!(g.rules.is_none(), "disabled: the rules chip is hidden");
    assert!(g.recall.is_none(), "disabled: the recall chip is hidden");
    assert_eq!(
        g.outcome,
        Ok(None),
        "disabled: the turn proceeds with no pinned preamble"
    );
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "disabled: the memory service was never asked"
    );
    let capture = Arc::new(Mutex::new(Vec::new()));
    let provider = CapturingProvider(Arc::clone(&capture));
    let events: Vec<ProviderEvent> =
        futures::executor::block_on(open_gated_stream(&provider, request(), &g.outcome).collect());
    assert_eq!(
        capture.lock().unwrap().len(),
        1,
        "the provider stream opened once"
    );
    assert_eq!(
        events,
        vec![ProviderEvent::Token("ok".into()), ProviderEvent::Done]
    );
}

/// RVS5A-FIX2: the "oversize block" fixture in `rejecting_bodies`
/// (6,600–9,900-byte directives) proves the per-directive bound at turn
/// level, because the 4 KiB per-directive check fires first; this body
/// proves the 64 KiB RENDERED-block bound instead — 20 directives of
/// 3,300 bytes each (each under 4 KiB; the rendered block totals
/// 66,146 bytes > 64 KiB) — so only the block bound can fire.
fn block_oversize_body() -> String {
    let items = (0..20)
        .map(|_| format!(r#"{{"object": "{}"}}"#, "x".repeat(3300)))
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"mandatory_policy": [{items}], "items": []}}"#)
}

/// RVS5A-FIX2 boundary companion: 19 directives of 3,399 bytes each —
/// each under 4 KiB, and the rendered block totals 64,722 bytes, just
/// UNDER the 64 KiB bound — so the turn must be accepted and every
/// directive must reach the prompt in order.
fn block_under_body() -> String {
    let items = (0..19)
        .map(|i| format!(r#"{{"object": "{}"}}"#, under_directive_text(i)))
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"mandatory_policy": [{items}], "items": []}}"#)
}

/// Directive `i` of the under-limit fixture: a unique `DIRECTIVE-NN:` marker
/// padded with 'z' to exactly 3,399 bytes (the same size as before, so the
/// rendered block still totals 64,722 bytes).
fn under_directive_text(i: usize) -> String {
    let marker = format!("DIRECTIVE-{:02}:", i + 1);
    format!("{marker}{}", "z".repeat(3399 - marker.len()))
}

/// RVS5A-FIX2: a rendered standing-directives block over 64 KiB is
/// refused at turn level — the WORK and CHAT paths both refuse before
/// any provider request or worker prompt, and the refusal names the
/// BLOCK bound (`the bound is 65536`), not the per-directive one
/// (`the per-directive bound is 4096`).
#[test]
fn oversize_block_under_per_directive_limit_refuses_turn() {
    let body = block_oversize_body();
    let (g, n) = gate(&body);
    assert!(
        g.outcome.is_err(),
        "the gate must refuse an over-64 KiB block"
    );
    assert_eq!(
        g.rules,
        Some(RulesStatus::Rejected),
        "the rules chip shows rejected"
    );
    assert!(n >= 1, "the memory service was asked");
    let reason = g.outcome.clone().unwrap_err();
    assert!(
        reason.contains("the bound is 65536"),
        "the refusal names the BLOCK bound, not the per-directive one: {reason}"
    );
    assert!(
        !reason.contains("per-directive bound is 4096"),
        "the 4 KiB per-directive check must NOT have fired first: {reason}"
    );
    // Work path: the refusal carries the reason and nothing reaches the
    // worker.
    let (mut work, rx) = work_state();
    let r = match offer_gated_work_prompt(&mut work, "task", g.outcome.clone()) {
        GatedWorkPrompt::Refused(r) => r,
        GatedWorkPrompt::Offered => panic!("expected Refused, got Offered"),
        GatedWorkPrompt::NeedsWorker(_) => panic!("expected Refused, got NeedsWorker"),
    };
    assert!(
        r.contains("the bound is 65536"),
        "the work refusal names the BLOCK bound: {r}"
    );
    assert!(
        !r.contains("per-directive bound is 4096"),
        "the work refusal does not name the per-directive bound: {r}"
    );
    assert!(
        matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "nothing reached the worker"
    );
    // Chat path: no provider request, the error names the BLOCK bound.
    let capture = Arc::new(Mutex::new(Vec::new()));
    let provider = CapturingProvider(Arc::clone(&capture));
    let events: Vec<ProviderEvent> =
        futures::executor::block_on(open_gated_stream(&provider, request(), &g.outcome).collect());
    assert!(
        capture.lock().unwrap().is_empty(),
        "no provider request was issued"
    );
    assert_eq!(events.len(), 2);
    match &events[0] {
        ProviderEvent::Error(m) => {
            assert!(
                m.contains("memory policy rejected"),
                "the error names the refusal: {m}"
            );
            assert!(
                m.contains("the bound is 65536"),
                "the chat error names the BLOCK bound: {m}"
            );
            assert!(
                !m.contains("per-directive bound is 4096"),
                "the chat error does not name the per-directive bound: {m}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
    assert_eq!(events[1], ProviderEvent::Done, "the stream then ends");
}

/// RVS5A-FIX2 boundary: directives totalling just UNDER the 64 KiB
/// rendered-block bound (19 × 3,399 bytes = 64,722 B) are accepted, and
/// EVERY one reaches the pinned prompt, in order.
#[test]
fn block_just_under_bound_is_accepted_in_order() {
    let body = block_under_body();
    let (g, _n) = gate(&body);
    assert!(
        g.outcome.is_ok(),
        "a just-under-64 KiB block must be accepted: {:?}",
        g.outcome
    );
    assert_eq!(
        g.rules,
        Some(RulesStatus::Loaded(19)),
        "the chip counts all 19 accepted directives"
    );
    let (mut work, rx) = work_state();
    match offer_gated_work_prompt(&mut work, "task", g.outcome) {
        GatedWorkPrompt::Offered => {}
        GatedWorkPrompt::Refused(r) => panic!("expected Offered, got Refused: {r}"),
        GatedWorkPrompt::NeedsWorker(_) => panic!("expected Offered, got NeedsWorker"),
    }
    let GatewayCmd::Prompt {
        text,
        system: Some(s),
    } = rx
        .try_recv()
        .unwrap_or_else(|e| panic!("expected one Prompt with a system: {e:?}"))
    else {
        panic!("expected a Prompt command, got some other worker command");
    };
    assert_eq!(text, "task", "the task text is unchanged");
    // Every directive reaches the prompt, in order: each directive's
    // CONTENT (unique `DIRECTIVE-NN:` marker padded with 'z') must sit
    // under its own number, in source order.
    let mut prev = 0usize;
    for i in 0..19 {
        let num = i + 1;
        let marker = format!("{num}. {}", under_directive_text(i));
        let at = s
            .find(&marker)
            .unwrap_or_else(|| panic!("preamble missing directive {num} with its own content"));
        assert!(
            at >= prev,
            "preamble reorders: directive {num} at {at} after {prev}"
        );
        prev = at + 1;
    }
    // Content order, independent of numbering: DIRECTIVE-01 … DIRECTIVE-19
    // appear strictly in source order.
    let positions: Vec<usize> = (1..=19)
        .map(|n| {
            s.find(&format!("DIRECTIVE-{n:02}:"))
                .expect("marker present")
        })
        .collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "directive contents must appear in source order: {positions:?}"
    );
    assert!(
        matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "the worker got exactly one prompt"
    );
}

#[test]
fn unreachable_memory_keeps_offline_degrade() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let g = futures::executor::block_on(gate_turn_memory(
        mem(&base, true),
        "task".into(),
        stub_client,
    ));
    assert_eq!(
        g.rules,
        Some(RulesStatus::Offline),
        "unreachable: the historical degrade keeps its chip"
    );
    assert_eq!(
        g.outcome,
        Ok(None),
        "unreachable: the turn proceeds, as before S5(a)"
    );
    let capture = Arc::new(Mutex::new(Vec::new()));
    let provider = CapturingProvider(Arc::clone(&capture));
    let events: Vec<ProviderEvent> =
        futures::executor::block_on(open_gated_stream(&provider, request(), &g.outcome).collect());
    assert_eq!(
        capture.lock().unwrap().len(),
        1,
        "the provider stream opened once"
    );
    assert_eq!(
        events,
        vec![ProviderEvent::Token("ok".into()), ProviderEvent::Done]
    );
}
