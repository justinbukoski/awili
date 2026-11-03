//! End-to-end exercise of [`MemoryClient`] against an in-process loopback stub
//! HTTP server. This is NOT a live-network test — the stub binds `127.0.0.1:0`
//! and serves canned responses, so the reqwest transport, URL building, status
//! handling, and JSON parsing are all covered without any external dependency.
//!
//! The stub routes on **method + exact path** matching the real Foreman Memory
//! API contract (`GET /v5/health`, `POST /v5/notes`, `POST /chat`) and serves
//! response bodies captured verbatim from the live service
//! (`tests/fixtures/*.json`). Any other method/path 404s — so a client that
//! regresses to a wrong verb or path (the original `GET /search`, `POST /store`,
//! `POST /chat_log` bug, the dead v4 `POST /memories` write, or the dead v4
//! `POST /memories/search` recall removed at the v5 cutover 2026-06-19) fails
//! these tests loudly. `POST /v5/notes` additionally 401s without a writer
//! bearer, mirroring the live service — so a write that forgets the token fails.
//!
//! The per-turn read path (standing directives + past-context recall) is the
//! authed v5 `POST /v5/recall` API. Its pure decode/mapping is covered by the
//! in-crate unit tests (`RecallResponse` + `directives_from_recall`/
//! `recall_from_recall`), but the wire contract — method, path, bearer auth, and
//! the `{query, top_k}` body — is guarded here through `directives_v5`/
//! `recall_past_v5`, so a regression in the live read path fails loudly.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use awili_memory::{ChatLogEntry, MemoryClient};

/// Captured live from `GET /health` (still served at `/v5/health`).
const FIXTURE_HEALTH: &str = include_str!("fixtures/health.json");
/// Captured live from `POST /chat` (201).
const FIXTURE_CHAT: &str = include_str!("fixtures/chat.json");
/// Captured live from the note write path (200): the stored note object whose
/// `id` the client surfaces (`POST /v5/notes` echoes a numeric `id`).
const FIXTURE_STORE: &str = include_str!("fixtures/store.json");
/// A real-shaped `POST /v5/recall` response: the five buckets Awili's per-turn
/// read path folds into directives (constraints+directives) and past context
/// (notes+facts).
const FIXTURE_V5_RECALL: &str = include_str!("fixtures/v5_recall.json");

/// The reader/boot token the recall tests pin so the request is deterministic
/// (the real `~/.config/foreman/v5-boot-token` fallback must not leak in).
const TEST_READER_TOKEN: &str = "test-reader-token";

/// The writer token the write tests pin so the request is deterministic (the
/// real `~/.config/foreman/v5-writer-token` fallback must not leak into tests).
const TEST_WRITER_TOKEN: &str = "test-writer-token";

/// How the stub answers `POST /v5/notes` (the v5 write path): the live-captured
/// stored object, or a 500 to exercise the write_memory error path.
#[derive(Clone, Copy)]
enum StoreMode {
    Ok,
    Fail,
}

/// Spawn a stub server on an ephemeral port; returns its base URL. The server
/// loops for the lifetime of the test process, answering a small fixed routing
/// table. Each connection handles one request then closes.
fn spawn_stub() -> String {
    spawn_stub_capturing().0
}

/// [`spawn_stub`], also returning the captured `(path, JSON body)` of every
/// POST — so tests can assert the wire shape the client actually sent, not just
/// route on it.
/// Captured `(path, JSON body)` pairs, shared with the stub thread.
type Captured = Arc<Mutex<Vec<(String, String)>>>;

fn spawn_stub_capturing() -> (String, Captured) {
    spawn_stub_with(StoreMode::Ok)
}

/// The fully-parameterised spawn: store mode.
fn spawn_stub_with(store: StoreMode) -> (String, Captured) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let capture = Arc::clone(&captured);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // Each connection in its own thread so a slow client can't wedge the
            // accept loop.
            let capture = Arc::clone(&capture);
            thread::spawn(move || handle(stream, store, capture));
        }
    });
    (format!("http://{addr}"), captured)
}

/// Read one full HTTP request (headers + Content-Length body). The body may
/// arrive in a second segment, so read until the declared Content-Length is
/// satisfied before routing.
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

fn handle(mut stream: TcpStream, store: StoreMode, capture: Arc<Mutex<Vec<(String, String)>>>) {
    let req = read_request(&mut stream);
    let mut parts = req.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");

    // Record POST bodies (with their path) for wire-shape assertions.
    if method == "POST" {
        if let Some((_, body)) = req.split_once("\r\n\r\n") {
            capture
                .lock()
                .unwrap()
                .push((path.to_string(), body.to_string()));
        }
    }

    // Whether the request carried a writer bearer (the v5 write path requires it,
    // like the live service). Header names are case-insensitive on the wire.
    let has_bearer = req.to_lowercase().contains("authorization: bearer ");

    // Match the real contract exactly: method AND path. A wrong verb/path 404s.
    let (status, body) = match (method, path) {
        ("GET", "/v5/health") => ("200 OK", FIXTURE_HEALTH),
        // v5 read path: the authed recall API. A missing bearer is a 401 (matches
        // the live service), so a client that forgets the reader token fails loudly.
        ("POST", "/v5/recall") if !has_bearer => ("401 Unauthorized", ""),
        ("POST", "/v5/recall") => ("200 OK", FIXTURE_V5_RECALL),
        // v5 write path: a missing writer bearer is a 401 (matches the live
        // service), so a client that forgets the token fails loudly.
        ("POST", "/v5/notes") if !has_bearer => ("401 Unauthorized", ""),
        ("POST", "/v5/notes") => match store {
            StoreMode::Ok => ("200 OK", FIXTURE_STORE),
            StoreMode::Fail => ("500 Internal Server Error", ""),
        },
        ("POST", "/chat") => ("201 Created", FIXTURE_CHAT),
        _ => ("404 Not Found", ""),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

#[test]
fn health_reports_reachable() {
    let base = spawn_stub();
    let client = MemoryClient::new(base, 90);
    assert!(client.health());
}

#[test]
fn health_false_when_unreachable() {
    // Port 1 is not listening; health must degrade to false quickly, not hang.
    let client = MemoryClient::new("http://127.0.0.1:1", 90);
    assert!(!client.health());
}

// ---- per-turn read path: v5 recall wire contract ---------------------------

#[test]
fn directives_and_recall_ride_the_v5_recall_api() {
    // The live per-turn read path: `directives_v5`/`recall_past_v5` must POST to
    // `/v5/recall` with a bearer and the `{query, top_k}` body, then fold the
    // five buckets into directives (constraints+directives, priority-sorted) and
    // past context (notes+facts). The stub 404s any invented path and 401s
    // without a bearer, so this proves target, auth, body, AND mapping together.
    let (base, captured) = spawn_stub_capturing();
    let client = MemoryClient::new(base, 90).with_token(TEST_READER_TOKEN);

    let directives = client.directives_v5("what are the rules", 8).unwrap();
    assert_eq!(directives.len(), 2, "constraints + directives buckets");
    assert_eq!(directives[0].text, "one app one database"); // importance 1.0 -> 100, first
    assert_eq!(directives[0].priority, 100);
    assert_eq!(directives[1].priority, 98);

    let past = client.recall_past_v5("what did we decide", 5).unwrap();
    assert_eq!(past.len(), 2, "notes + facts buckets");
    let texts: Vec<&str> = past.iter().map(|e| e.text.as_str()).collect();
    assert!(texts.contains(&"awili pivot to hermes surface"));
    assert!(texts.contains(&"HOSTC is 10.77.68.86"));
    // Directive text must not leak into past context.
    assert!(!texts.iter().any(|t| t.contains("deep think")));

    // Wire shape: both calls hit `/v5/recall` with `{query, top_k}` (top_k is
    // floored at 10 by the client so a small k still surfaces enough buckets).
    let bodies = captured.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0].0, "/v5/recall");
    let body: serde_json::Value = serde_json::from_str(&bodies[0].1).unwrap();
    assert_eq!(body["query"], "what are the rules");
    assert_eq!(body["top_k"], 10);
    // The recall_past_v5 call hits the same endpoint with its own query.
    assert_eq!(bodies[1].0, "/v5/recall");
    let body: serde_json::Value = serde_json::from_str(&bodies[1].1).unwrap();
    assert_eq!(body["query"], "what did we decide");
    assert_eq!(body["top_k"], 10);
}

/// RC2-SVC1 (F0735): the configured priority floor applies on the v5 path —
/// a client built with `min_priority` 99 drops the priority-98 directive.
#[test]
fn rc2svc1_the_priority_floor_applies_to_v5_directives() {
    let (base, _captured) = spawn_stub_capturing();
    let client = MemoryClient::new(base, 99).with_token(TEST_READER_TOKEN);
    let directives = client.directives_v5("what are the rules", 8).unwrap();
    assert_eq!(directives.len(), 1, "only the priority-100 directive stays");
    assert_eq!(directives[0].priority, 100);
}

// ---- slice 10: write_memory + distillation wire shape ----------------------

#[test]
fn write_memory_posts_v5_notes_and_returns_id() {
    // Happy path on the v5 write path: the real `POST /v5/notes` route, the v5
    // body shape `{content, category, importance}` (priority 60 -> importance
    // 0.6, NO agent_id — v5 notes carry no agent field), and the stored object's
    // id surfaced to the caller.
    let (base, captured) = spawn_stub_capturing();
    let client = MemoryClient::new(base, 90).with_writer_token(TEST_WRITER_TOKEN);
    let id = client
        .write_memory(
            "Awili learned the feed service is built on axum.",
            "conversation",
            60,
        )
        .unwrap();
    assert_eq!(id, 2350, "the fixture's stored id must be surfaced");
    let bodies = captured.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    let (path, body) = &bodies[0];
    assert_eq!(path, "/v5/notes");
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        body["content"],
        "Awili learned the feed service is built on axum."
    );
    assert_eq!(body["category"], "conversation");
    assert_eq!(body["importance"], 0.6);
    assert!(
        body.get("agent_id").is_none(),
        "v5 notes carry no agent field"
    );
    assert!(
        body.get("priority").is_none(),
        "v5 notes use importance, not priority"
    );
}

#[test]
fn write_memory_error_paths() {
    // A 5xx from a reachable service is a Status error...
    let (base, _) = spawn_stub_with(StoreMode::Fail);
    let client = MemoryClient::new(base, 90).with_writer_token(TEST_WRITER_TOKEN);
    assert!(matches!(
        client.write_memory("x", "conversation", 60),
        Err(awili_memory::MemoryError::Status(500))
    ));
    // ...and a dead endpoint is an Http error. Neither panics, neither fakes an id.
    let dead = MemoryClient::new("http://127.0.0.1:1", 90).with_writer_token(TEST_WRITER_TOKEN);
    assert!(matches!(
        dead.write_memory("x", "conversation", 60),
        Err(awili_memory::MemoryError::Http(_))
    ));
}

#[test]
fn distillation_writes_conversation_category_at_importance_point6() {
    // End-to-end below the provider: a model answer passes the distillation gate,
    // then lands on the v5 wire with EXACTLY the distill constants — category
    // "conversation", priority 60 mapped to importance 0.6.
    let (base, captured) = spawn_stub_capturing();
    let client = MemoryClient::new(base, 90).with_writer_token(TEST_WRITER_TOKEN);
    let answer = "Awili and the boss decided the feed service will use axum.";
    let content = awili_memory::distilled_content(answer).expect("a real answer passes the gate");
    let id = client
        .write_memory(
            &content,
            awili_memory::DISTILL_CATEGORY,
            awili_memory::DISTILL_PRIORITY,
        )
        .unwrap();
    assert!(id > 0);
    let bodies = captured.lock().unwrap();
    let (path, body) = &bodies[0];
    assert_eq!(path, "/v5/notes");
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["category"], "conversation");
    assert_eq!(body["importance"], 0.6);
    assert_eq!(body["content"], answer);
}

#[test]
fn store_and_chat_log_succeed() {
    let base = spawn_stub();
    let client = MemoryClient::new(base, 90).with_writer_token(TEST_WRITER_TOKEN);
    assert!(client.store("a fact", "note", 80).is_ok());
    let entry = ChatLogEntry {
        session_id: "c1".to_string(),
        role: "user".to_string(),
        content: "hi".to_string(),
    };
    assert!(client.chat_log(&entry).is_ok());
}

#[test]
fn record_usage_posts_to_v5_notes() {
    // The usage note rides the v5 `POST /v5/notes` write path — the stub 404s any
    // invented path and 401s without a writer bearer, so this proves both the
    // wire target and that the writer token is sent.
    let (base, captured) = spawn_stub_capturing();
    let client = MemoryClient::new(base, 90).with_writer_token(TEST_WRITER_TOKEN);
    let note = awili_memory::UsageNote {
        session_cost_usd: 0.0031,
        provider: "anthropic".to_string(),
        model: "claude-haiku-4-5".to_string(),
        input_tokens: 1200,
        output_tokens: 340,
    };
    assert!(client.record_usage(&note).is_ok());
    let bodies = captured.lock().unwrap();
    assert_eq!(bodies[0].0, "/v5/notes");
}

/// Send a raw request line to the stub and return its HTTP status line. Uses
/// std only (the test target has no reqwest), so it can probe arbitrary
/// method/path combinations the typed client no longer emits.
fn raw_status(base: &str, request_line: &str) -> String {
    let addr = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(format!("{request_line}\r\nHost: stub\r\nConnection: close\r\n\r\n").as_bytes())
        .unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    resp.lines().next().unwrap_or("").to_string()
}

#[test]
fn old_invented_routes_are_dead() {
    // Guard against regressing to a wrong contract: the stub mirrors the real
    // service and answers ONLY the real routes. The original bug's routes
    // (`GET /search`, `POST /store`, `POST /chat_log`), the dead v4 write route
    // (`POST /memories`), and the dead v4 recall route (`POST /memories/search`,
    // both removed server-side at the v5 cutover) must 404 here, while the real
    // ones succeed.
    let base = spawn_stub();
    assert!(raw_status(&base, "GET /search HTTP/1.1").contains("404"));
    assert!(raw_status(&base, "POST /store HTTP/1.1").contains("404"));
    assert!(raw_status(&base, "POST /chat_log HTTP/1.1").contains("404"));
    assert!(
        raw_status(&base, "POST /memories HTTP/1.1").contains("404"),
        "dead v4 write route"
    );
    assert!(
        raw_status(&base, "POST /memories/search HTTP/1.1").contains("404"),
        "dead v4 recall route"
    );
    // Real routes the typed client actually uses:
    assert!(raw_status(&base, "POST /chat HTTP/1.1").contains("201"));
    assert!(raw_status(&base, "GET /v5/health HTTP/1.1").contains("200"));
    assert!(
        raw_status(&base, "GET /health HTTP/1.1").contains("404"),
        "dead v4 health route"
    );
    // Both authed v5 paths require a bearer (matches the live service): a raw POST
    // with no auth 401s on the read (`/v5/recall`) and the write (`/v5/notes`); a
    // typed write (which sends the token) succeeds.
    assert!(raw_status(&base, "POST /v5/recall HTTP/1.1").contains("401"));
    assert!(raw_status(&base, "POST /v5/notes HTTP/1.1").contains("401"));
    let client = MemoryClient::new(base, 90).with_writer_token(TEST_WRITER_TOKEN);
    assert!(client.write_memory("real path", "note", 80).is_ok());
}
