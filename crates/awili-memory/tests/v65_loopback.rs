//! P4b acceptance 1/2/3/5/6: the typed v6.5 interlock verbs against an
//! in-process loopback stub (127.0.0.1:0, answering on method + exact path):
//! exact method/path/body/bearer per verb, the service-stated budget floor,
//! one-shot transport retries (writes reuse identical idempotency arguments),
//! the non-retryable 4xx `predicate_not_allowed`, sentinel tokens absent.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use awili_memory::v65::{
    archive_search, bootstrap, recall, remember, Authority, EpistemicStatus, Predicate,
    RememberRequest, ServiceCode, TokenLane, V65Error,
};
use awili_memory::MemoryClient;

/// Sentinel reader token — must never leak into an error string (acceptance 6).
const READER: &str = "test-v65-reader-token";
/// Sentinel writer token — must never leak into an error string (acceptance 6).
const WRITER: &str = "test-v65-writer-token";

/// A 422 whose stated floor OUTGROWS the 16,000 start budget — the only
/// case in which a budget retry is legal (acceptance 2).
const BUDGET_FLOOR_422: &str = r#"{"error":{"code":"budget_too_small","message":"mandatory policy floor grew","minimum_token_budget":16500}}"#;
/// RC2-SVC1 (F0731): a stated floor far above any sane budget.
const HUGE_FLOOR_422: &str =
    r#"{"error":{"code":"budget_too_small","message":"floor","minimum_token_budget":50000000}}"#;
const PREDICATE_400: &str =
    r#"{"error":{"code":"predicate_not_allowed","message":"legacy.note only"}}"#;

/// The boot/recall envelope fixture: one mandatory-policy item and two
/// ordinary items, in separate lanes.
const BOOTSTRAP_FIXTURE: &str = r#"{"mandatory_policy":[
  {"id":"pol-1","kind":"directive","subject":"policy","authority":"owner_instruction","object":"NEVER shut down the fleet"}
],"items":[
  {"id":"it-1","subject":"decision","object":{"content":"we ship P4b","importance":0.9}},
  {"id":"it-2","subject":"note","object":"the gate is cargo test"}
]}"#;

/// The archive evidence fixture: RAW hits, a distinct lane.
const ARCHIVE_FIXTURE: &str = r#"{"results":[
  {"id":"arch-1","subject":"chat","object":{"raw":"log line one"},"timestamp":"2026-09-01T10:00:00Z"},
  {"id":"arch-2","subject":"chat","object":{"raw":"log line two"},"timestamp":"2026-09-01T10:01:00Z"}
]}"#;

/// How the stub behaves for the verb under test.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StubMode {
    /// Every verb 200s with its fixture.
    Ok,
    /// The FIRST read request 422s with the named budget floor.
    BudgetFloorFirst,
    /// The FIRST read connection is dropped (transport retry).
    DropFirstRead,
    /// The FIRST write connection is dropped (transport retry).
    DropFirstWrite,
    /// `/v6/memories` always 400s `predicate_not_allowed`.
    PredicateReject,
    /// RC2-SVC1 (F0731): the FIRST read request 422s with an absurd floor.
    HugeFloorFirst,
    /// RC2-SVC1 (T23): every read 401s with a message that reflects the
    /// request's `Authorization` header back.
    EchoBearer,
}

/// A `(path, body, authorization)` capture of every request.
type Captures = Arc<Mutex<Vec<(String, String, String)>>>;

/// Per-lane request counters (0-based: `n` is this request's index).
struct Counts {
    read: Mutex<u32>,
    write: Mutex<u32>,
}

/// Spawn the stub on an ephemeral port; returns the base URL and captures.
fn spawn(mode: StubMode) -> (String, Captures) {
    let captured: Captures = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let capture = Arc::clone(&captured);
    let counts = Arc::new(Counts {
        read: Mutex::new(0),
        write: Mutex::new(0),
    });
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let capture = Arc::clone(&capture);
            let counts = Arc::clone(&counts);
            thread::spawn(move || handle(stream, mode, capture, counts));
        }
    });
    (format!("http://{addr}"), captured)
}

/// Read one full HTTP request (headers + Content-Length body).
fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(header_end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&data[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if data.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

fn handle(mut stream: TcpStream, mode: StubMode, capture: Captures, counts: Arc<Counts>) {
    let req = read_request(&mut stream);
    let mut parts = req.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");
    let (head, body) = req.split_once("\r\n\r\n").unwrap_or((req.as_str(), ""));
    let auth = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("authorization")
                .then(|| v.trim().to_string())
        })
        .unwrap_or_default();
    capture
        .lock()
        .unwrap()
        .push((path.to_string(), body.to_string(), auth.clone()));

    let is_read = path == "/v6/bootstrap" || path == "/v6/recall";
    let counter = if is_read { &counts.read } else { &counts.write };
    let n = *counter.lock().unwrap();
    *counter.lock().unwrap() = n + 1;

    // A dropped connection is the transport failure the retry discipline
    // must survive exactly once.
    let drop = match (mode, is_read) {
        (StubMode::DropFirstRead, true) if n == 0 => true,
        (StubMode::DropFirstWrite, false) if n == 0 => true,
        _ => false,
    };
    if drop {
        return;
    }

    let echoed = format!(
        r#"{{"error":{{"code":"unauthorized","message":"rejected {auth} for this route"}}}}"#
    );
    let (status, resp): (&str, &str) = match (method, path) {
        ("POST", "/v6/bootstrap") | ("POST", "/v6/recall") if mode == StubMode::EchoBearer => {
            ("401 Unauthorized", echoed.as_str())
        }
        ("POST", "/v6/bootstrap") | ("POST", "/v6/recall")
            if mode == StubMode::HugeFloorFirst && n == 0 =>
        {
            ("422 Unprocessable Entity", HUGE_FLOOR_422)
        }
        ("POST", "/v6/bootstrap") | ("POST", "/v6/recall")
            if mode == StubMode::BudgetFloorFirst && n == 0 =>
        {
            ("422 Unprocessable Entity", BUDGET_FLOOR_422)
        }
        ("POST", "/v6/bootstrap") | ("POST", "/v6/recall") => ("200 OK", BOOTSTRAP_FIXTURE),
        ("POST", "/v6/memories") if mode == StubMode::PredicateReject => {
            ("400 Bad Request", PREDICATE_400)
        }
        ("POST", "/v6/memories") => ("200 OK", r#"{"id":"1f3c-9a"}"#),
        ("POST", "/v6/archive/search") => ("200 OK", ARCHIVE_FIXTURE),
        _ => ("404 Not Found", ""),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
        resp.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// A client pinned to the stub with both sentinel tokens (hermetic: the
/// seam overrides whatever `~/.config/foreman` holds).
fn client(base: &str) -> MemoryClient {
    MemoryClient::new(base, 90).with_v65(base, Some(READER), Some(WRITER))
}

fn remember_request() -> RememberRequest {
    RememberRequest {
        request_id: "req-uuid-1".to_string(),
        subject: "awili.p4b".to_string(),
        predicate: Predicate::LegacyNote,
        object: serde_json::json!({"content": "P4b shipped: interlock verbs"}),
        authority: Authority::TrustedAgentReport,
        epistemic_status: EpistemicStatus::Asserted,
        source_type: "awili-app".to_string(),
        source_ref: "awili-desktop".to_string(),
        reason: "P4b acceptance write".to_string(),
    }
}

// ---- acceptance 1: exact method / path / body / auth per verb --------------

#[test]
fn bootstrap_posts_exact_wire_contract() {
    let (base, caps) = spawn(StubMode::Ok);
    let c = client(&base);
    let boot = bootstrap(&c).unwrap();
    // Lane discipline: policy and items stay separate typed fields.
    assert_eq!(boot.mandatory_policy.len(), 1);
    assert_eq!(boot.items.len(), 2);
    assert_eq!(boot.mandatory_policy[0].subject.as_deref(), Some("policy"));
    assert_eq!(boot.items[0].id.as_deref(), Some("it-1"));
    let caps = caps.lock().unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].0, "/v6/bootstrap");
    assert_eq!(caps[0].2, format!("Bearer {READER}"));
    let v: serde_json::Value = serde_json::from_str(&caps[0].1).unwrap();
    assert_eq!(v, serde_json::json!({ "token_budget": 16000 }));
}

#[test]
fn recall_posts_exact_wire_contract() {
    let (base, caps) = spawn(StubMode::Ok);
    let c = client(&base);
    let rec = recall(&c, "what did we decide").unwrap();
    assert_eq!(rec.mandatory_policy.len(), 1);
    assert_eq!(rec.items.len(), 2);
    let caps = caps.lock().unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].0, "/v6/recall");
    assert_eq!(caps[0].2, format!("Bearer {READER}"));
    let v: serde_json::Value = serde_json::from_str(&caps[0].1).unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "query": "what did we decide", "token_budget": 16000 })
    );
}

#[test]
fn remember_posts_full_provenance_body() {
    let (base, caps) = spawn(StubMode::Ok);
    let c = client(&base);
    let res = remember(&c, &remember_request()).unwrap();
    assert_eq!(res.id.as_deref(), Some("1f3c-9a"));
    let caps = caps.lock().unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].0, "/v6/memories");
    assert_eq!(caps[0].2, format!("Bearer {WRITER}"));
    let v: serde_json::Value = serde_json::from_str(&caps[0].1).unwrap();
    // Every provenance field serializes, with the exact wire vocabulary.
    assert_eq!(v["request_id"], "req-uuid-1");
    assert_eq!(v["subject"], "awili.p4b");
    assert_eq!(v["predicate"], "legacy.note");
    assert_eq!(v["object"]["content"], "P4b shipped: interlock verbs");
    assert_eq!(v["authority"], "trusted_agent_report");
    assert_eq!(v["epistemic_status"], "asserted");
    assert_eq!(v["source_type"], "awili-app");
    assert_eq!(v["source_ref"], "awili-desktop");
    assert_eq!(v["reason"], "P4b acceptance write");
    assert_eq!(v.as_object().unwrap().len(), 9, "exactly the nine fields");
}

#[test]
fn archive_search_posts_exact_wire_contract() {
    let (base, caps) = spawn(StubMode::Ok);
    let c = client(&base);
    // Acceptance 5: the results stay RAW EVIDENCE — a distinct type, never
    // folded into canonical recall/policy collections.
    let res = archive_search(&c, "hostc", 25).unwrap();
    assert_eq!(res.results.len(), 2);
    assert_eq!(res.results[0].id.as_deref(), Some("arch-1"));
    assert_eq!(
        res.results[1].timestamp.as_deref(),
        Some("2026-09-01T10:01:00Z")
    );
    let caps = caps.lock().unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].0, "/v6/archive/search");
    assert_eq!(caps[0].2, format!("Bearer {READER}"));
    let v: serde_json::Value = serde_json::from_str(&caps[0].1).unwrap();
    assert_eq!(v, serde_json::json!({ "query": "hostc", "limit": 25 }));
}

// ---- acceptance 2: only the service-stated budget floor is enforced --------

#[test]
fn budget_floor_retry_uses_service_minimum() {
    let (base, caps) = spawn(StubMode::BudgetFloorFirst);
    let c = client(&base);
    let boot = bootstrap(&c).unwrap();
    assert_eq!(boot.items.len(), 2, "the floored retry must succeed");
    let caps = caps.lock().unwrap();
    assert_eq!(caps.len(), 2, "one retry, at the stated floor");
    let first: serde_json::Value = serde_json::from_str(&caps[0].1).unwrap();
    assert_eq!(first["token_budget"], 16000, "start at the shared floor");
    let second: serde_json::Value = serde_json::from_str(&caps[1].1).unwrap();
    assert_eq!(
        second["token_budget"], 16500,
        "retry at exactly the service-stated minimum"
    );
    assert!(
        caps.iter().all(|c| c.0 == "/v6/bootstrap"),
        "both requests hit the bootstrap route"
    );
}

/// RC2-SVC1 (F0731): a stated floor above `V65_MAX_TOKEN_BUDGET` is never
/// adopted — one request, and the read fails with the service's own answer.
#[test]
fn rc2svc1_a_huge_stated_floor_is_not_adopted() {
    let (base, caps) = spawn(StubMode::HugeFloorFirst);
    let c = client(&base);
    let err = bootstrap(&c).expect_err("an absurd floor must fail the read");
    assert_eq!(err.code(), Some(ServiceCode::BudgetTooSmall));
    assert_eq!(caps.lock().unwrap().len(), 1, "no retry at the huge floor");
    // The legacy recall path keeps the same cap.
    let (base, caps) = spawn(StubMode::HugeFloorFirst);
    let c = client(&base);
    assert!(c.recall_v65("q", 16_000).is_err());
    assert_eq!(caps.lock().unwrap().len(), 1, "no retry at the huge floor");
}

/// RC2-SVC1 (T23): a service that reflects the request's bearer in its
/// error message must not put the token into the error text.
#[test]
fn rc2svc1_a_reflected_bearer_is_redacted() {
    let (base, _caps) = spawn(StubMode::EchoBearer);
    let c = client(&base);
    let err = recall(&c, "anything").expect_err("401 must fail");
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(READER), "token leaked: {shown}");
    assert!(shown.contains("[redacted]"), "redaction marker: {shown}");
    assert_eq!(err.status(), Some(401));
}

// ---- retry discipline: reads and writes retry once -------------------------

#[test]
fn read_transport_failure_retries_once() {
    let (base, caps) = spawn(StubMode::DropFirstRead);
    let c = client(&base);
    assert!(recall(&c, "again").is_ok(), "the retry must land");
    let caps = caps.lock().unwrap();
    assert_eq!(caps.len(), 2, "exactly one retry");
    assert!(
        caps.iter().all(|c| c.0 == "/v6/recall"),
        "both hit the recall route"
    );
}

#[test]
fn remember_retries_identical_body() {
    // Acceptance 3: a transport retry reuses the same request id — the
    // captured bodies must be byte-identical idempotency arguments.
    let (base, caps) = spawn(StubMode::DropFirstWrite);
    let c = client(&base);
    assert!(remember(&c, &remember_request()).is_ok());
    let caps = caps.lock().unwrap();
    assert_eq!(caps.len(), 2, "exactly one retry");
    assert!(
        caps.iter().all(|c| c.0 == "/v6/memories"),
        "both hit the write route"
    );
    assert_eq!(caps[0].1, caps[1].1, "identical idempotency arguments");
}

#[test]
fn missing_writer_token_fails_closed() {
    let (base, caps) = spawn(StubMode::Ok);
    let c = MemoryClient::new(base.as_str(), 90).with_v65(base.as_str(), Some(READER), None);
    assert!(
        matches!(
            remember(&c, &remember_request()),
            Err(V65Error::MissingToken(TokenLane::Writer))
        ),
        "writer-lane absence must fail closed before any request"
    );
    assert!(caps.lock().unwrap().is_empty(), "no request may be sent");
}

// ---- acceptance 4: the freeform-predicate rejection is final and typed -----

#[test]
fn predicate_not_allowed_is_final_and_typed() {
    let (base, caps) = spawn(StubMode::PredicateReject);
    let c = client(&base);
    let err = remember(&c, &remember_request()).unwrap_err();
    assert!(
        matches!(
            &err,
            V65Error::Service {
                status: 400,
                code: ServiceCode::PredicateNotAllowed,
                ..
            }
        ),
        "expected 400 + PredicateNotAllowed, got {err:?}"
    );
    assert!(err.is_predicate_not_allowed(), "typed non-retryable flag");
    assert!(!err.retryable(), "a 4xx rejection is never retried");
    assert_eq!(caps.lock().unwrap().len(), 1, "no retry, no fallback");
}

// ---- acceptance 6: sentinel tokens never render -----------------------------

#[test]
fn errors_carry_no_bearer_token() {
    let (base, _) = spawn(StubMode::PredicateReject);
    let c = client(&base);
    let pred = remember(&c, &remember_request()).unwrap_err();
    let missing = MemoryClient::new(base.as_str(), 90)
        .with_v65(base.as_str(), None, None)
        .remember(&remember_request())
        .unwrap_err();
    let dead = MemoryClient::new("http://127.0.0.1:1", 90).with_v65(
        "http://127.0.0.1:1",
        Some(READER),
        Some(WRITER),
    );
    let transport = recall(&dead, "x").unwrap_err();
    for err in [&pred, &missing, &transport] {
        for rendered in [err.to_string(), format!("{err:?}")] {
            assert!(
                !rendered.contains(READER) && !rendered.contains(WRITER),
                "sentinel token leaked into: {rendered}"
            );
        }
    }
}

#[test]
fn continuation_browse_uses_normalized_chronological_lane() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert!(request.starts_with("POST /v6.5/archive/search "));
        let body: serde_json::Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"query":null,"project_key":null,"thread_id":"known-thread","limit":8})
        );
        let data = r#"[{"event_id":"recent","source_timestamp":"2026-10-17T00:40:41Z"}]"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            data.len(),
            data
        )
        .unwrap();
    });
    let rows =
        awili_memory::v65::continuation_archive(&client(&base), None, None, Some("known-thread"))
            .unwrap();
    assert_eq!(rows[0]["event_id"], "recent");
    server.join().unwrap();
}
#[test]
fn continuation_refuses_account_wide_browse_before_network() {
    assert!(awili_memory::v65::continuation_archive(
        &client("http://127.0.0.1:1"),
        None,
        None,
        None
    )
    .is_err());
}

#[test]
#[ignore = "explicit read-only live Foreman contract probe"]
fn continuation_live_contract_probe() {
    let client = awili_memory::MemoryClient::resolved("", 90);
    let notes =
        awili_memory::v65::continuation_notes(&client, "awili").expect("live notes contract");
    let rows = awili_memory::v65::continuation_archive(&client, Some("awili polish"), None, None)
        .expect("live normalized archive contract");
    assert!(!rows.is_empty(), "known Awili history must be discoverable");
    let thread = rows[0]["thread_id"].as_str().unwrap();
    let latest = awili_memory::v65::continuation_archive(&client, None, None, Some(thread))
        .expect("live chronological thread browse");
    assert!(!latest.is_empty());
    assert!(latest
        .windows(2)
        .all(|w| w[0]["source_timestamp"].as_str() >= w[1]["source_timestamp"].as_str()));
    let key = rows[0]["project_key"].as_str().unwrap();
    let page = awili_memory::v65::continuation_project_page(&client, key, None)
        .expect("live project page");
    assert!(!page.is_empty());
    let bound = page.last().unwrap()["source_timestamp"].as_str().unwrap();
    let older = awili_memory::v65::continuation_project_page(&client, key, Some(bound))
        .expect("live timestamp pagination");
    assert!(older
        .iter()
        .all(|r| r["source_timestamp"].as_str().unwrap() <= bound));
    println!(
        "live contract: notes={} discovery={} chronological={} (content omitted)",
        notes.len(),
        rows.len(),
        latest.len()
    );
}
