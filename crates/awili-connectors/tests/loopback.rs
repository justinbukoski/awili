//! Loopback stub-server proof tests: assert exact wire shape and decode
//! behavior for every Outline and Gitea client method against a local stub.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use awili_connectors::{GiteaClient, OutlineClient, SecretRef, Timeouts};

const OUTLINE_INFO: &str = include_str!("fixtures/outline_info.json");
const OUTLINE_SEARCH: &str = include_str!("fixtures/outline_search.json");
const OUTLINE_CREATE: &str = include_str!("fixtures/outline_create.json");
const OUTLINE_APPEND: &str = include_str!("fixtures/outline_append.json");
const OUTLINE_ERROR: &str = include_str!("fixtures/outline_error.json");
const GITEA_SEARCH: &str = include_str!("fixtures/gitea_search.json");
const GITEA_BRANCH: &str = include_str!("fixtures/gitea_branch.json");
const GITEA_RAW: &str = include_str!("fixtures/gitea_raw.txt");
const GITEA_DEPLOY_KEY: &str = include_str!("fixtures/gitea_deploy_key.json");

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    body: String,
    auth: String,
}

type Capture = Arc<Mutex<Vec<Captured>>>;

fn header<'a>(req: &'a str, name: &str) -> &'a str {
    req.lines()
        .find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
        .unwrap_or("")
}

fn spawn_stub() -> (String, Capture) {
    let capture: Capture = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let cap = Arc::clone(&capture);
    thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let c = Arc::clone(&cap);
            thread::spawn(move || handle(s, c));
        }
    });
    (format!("http://{addr}"), capture)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(h) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let cl = header(&String::from_utf8_lossy(&data[..h]), "content-length")
                .parse::<usize>()
                .unwrap_or(0);
            if data.len() >= h + 4 + cl {
                break;
            }
        }
    }
    String::from_utf8_lossy(&data).into_owned()
}

fn handle(mut stream: TcpStream, capture: Capture) {
    let req = read_request(&mut stream);
    let mut parts = req.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let auth = header(&req, "authorization").to_string();
    let body = req
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    let (status, resp, ctype) = route(&method, &path, &auth);
    capture.lock().unwrap().push(Captured {
        method,
        path,
        body,
        auth,
    });
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
        resp.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn route(method: &str, path: &str, auth: &str) -> (&'static str, String, &'static str) {
    let (json, text) = ("application/json", "text/plain");
    if auth.contains("unauthorized-sentinel") {
        return ("401 Unauthorized", OUTLINE_ERROR.to_string(), json);
    }
    if method == "POST" {
        if let Some(rest) = path.strip_prefix("/documents.") {
            return match rest {
                "info" => ("200 OK", OUTLINE_INFO.to_string(), json),
                "search" => ("200 OK", OUTLINE_SEARCH.to_string(), json),
                "create" => ("200 OK", OUTLINE_CREATE.to_string(), json),
                "update" => ("200 OK", OUTLINE_APPEND.to_string(), json),
                _ => ("404 Not Found", String::new(), json),
            };
        }
        if path.starts_with("/repos/") && path.ends_with("/keys") {
            return ("201 Created", GITEA_DEPLOY_KEY.to_string(), json);
        }
    }
    if method == "GET" {
        if path.starts_with("/repos/search") {
            return ("200 OK", GITEA_SEARCH.to_string(), json);
        }
        if path.contains("/branches/") {
            return ("200 OK", GITEA_BRANCH.to_string(), json);
        }
        if path.contains("/raw/") {
            return ("200 OK", GITEA_RAW.to_string(), text);
        }
    }
    ("404 Not Found", String::new(), json)
}

static FILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// A temp token file whose backing file is removed on drop. Clients resolve
/// the secret into a `String` during `new()`, so the file is only needed for
/// that construction call and can be deleted immediately after.
struct TempFile(std::path::PathBuf);

impl std::ops::Deref for TempFile {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        // This fixture owns its directory. Another fixture may be between
        // directory creation and its first write, so sharing a pid-only
        // directory would let this drop remove that fixture's empty parent.
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

fn temp_file_dir() -> std::path::PathBuf {
    let seq = FILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "awili-connectors-test-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_temp_file(name: &str, content: &str) -> TempFile {
    let path = temp_file_dir().join(name);
    std::fs::write(&path, content).expect("write temp file");
    TempFile(path)
}

#[test]
fn dropping_one_fixture_preserves_another_before_its_first_write() {
    let first = write_temp_file("first-token.txt", "dummy-first");
    // Deterministically pause the second fixture between mkdir and write.
    let second_dir = temp_file_dir();
    drop(first);
    assert!(
        second_dir.is_dir(),
        "another fixture's empty parent survives drop"
    );
    let second = TempFile(second_dir.join("second-token.txt"));
    std::fs::write(&second.0, "dummy-second").expect("second fixture still writable");
    assert_eq!(std::fs::read_to_string(&second.0).unwrap(), "dummy-second");
    drop(second);
    assert!(
        !second_dir.exists(),
        "own fixture cleans its private directory"
    );
}

fn outline_client(base: &str) -> OutlineClient {
    let path = write_temp_file("outline-token.txt", "test-outline-token\n");
    OutlineClient::new(
        base,
        SecretRef::File {
            path: path.to_path_buf(),
            key: None,
        },
        Timeouts::default(),
    )
    .expect("outline")
}

fn gitea_client(base: &str) -> GiteaClient {
    let path = write_temp_file("gitea-token.txt", "test-gitea-token");
    GiteaClient::new(
        base,
        SecretRef::File {
            path: path.to_path_buf(),
            key: None,
        },
        Timeouts::default(),
    )
    .expect("gitea")
}

/// `document_info` POSTs to `/documents.info` with `Authorization: Bearer …`.
#[test]
fn info_posts_bearer_and_decodes_document() {
    let (base, capture) = spawn_stub();
    let client = outline_client(&base);
    let doc = client.document_info("doc-abc-123").expect("document_info");
    assert_eq!(doc.id, "doc-abc-123");
    assert_eq!(doc.title, "Infrastructure Runbook");
    let reqs = capture.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/documents.info");
    assert!(r.auth.starts_with("Bearer "), "Bearer scheme");
    assert!(r.body.contains("doc-abc-123"), "body carries id");
}

/// `document_search` decodes `.data[].document.{id,title,url}`.
#[test]
fn search_decodes_data_document_hits() {
    let (base, capture) = spawn_stub();
    let client = outline_client(&base);
    let hits = client
        .document_search("infrastructure", 10)
        .expect("search");
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "doc-hit-001");
    assert_eq!(hits[0].title, "First Hit");
    assert_eq!(hits[0].url.as_deref(), Some("/document/doc-hit-001"));
    assert_eq!(hits[1].url, None, "null url");
    assert_eq!(hits[2].id, "doc-hit-003");
    let reqs = capture.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/documents.search");
    let body: serde_json::Value = serde_json::from_str(&reqs[0].body).expect("json");
    assert_eq!(body["query"], "infrastructure");
    assert_eq!(body["limit"], 10);
}

/// `document_append` POSTs `/documents.update` with `append: true`.
#[test]
fn append_sends_append_true_and_returns_revision() {
    let (base, capture) = spawn_stub();
    let client = outline_client(&base);
    let doc = client
        .document_append("doc-abc-123", "\nAppended line.")
        .expect("append");
    assert_eq!(doc.id, "doc-abc-123");
    assert_eq!(doc.revision, Some(4), "revision 3->4");
    let reqs = capture.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/documents.update");
    let body: serde_json::Value = serde_json::from_str(&r.body).expect("json");
    assert_eq!(body["id"], "doc-abc-123");
    assert_eq!(body["append"], true);
}

/// `document_create` sends `collectionId` and `publish` in the body.
#[test]
fn create_sends_collection_and_publish() {
    let (base, capture) = spawn_stub();
    let client = outline_client(&base);
    let doc = client
        .document_create("New Doc", "Fresh body.", "coll-synthetic-0001", true)
        .expect("create");
    assert_eq!(doc.id, "doc-new-456");
    assert_eq!(doc.title, "New Doc");
    let reqs = capture.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/documents.create");
    let body: serde_json::Value = serde_json::from_str(&r.body).expect("json");
    assert_eq!(body["title"], "New Doc");
    assert_eq!(body["collectionId"], "coll-synthetic-0001");
    assert_eq!(body["publish"], true);
}

/// A non-2xx response maps to `ConnectorError::Status`; token never leaks.
#[test]
fn non_2xx_maps_to_status_with_excerpt() {
    let (base, _capture) = spawn_stub();
    let path = write_temp_file("outline-token-401.txt", "unauthorized-sentinel-token\n");
    let client = OutlineClient::new(
        &base,
        SecretRef::File {
            path: path.to_path_buf(),
            key: None,
        },
        Timeouts::default(),
    )
    .expect("client");
    let err = client.document_info("doc-abc-123").expect_err("must fail");
    let msg = err.to_string();
    assert!(msg.contains("401"), "status");
    assert!(msg.contains("Invalid API token"), "excerpt");
    assert!(!msg.contains("unauthorized-sentinel"), "no leak");
    assert!(
        !format!("{err:?}").contains("unauthorized-sentinel"),
        "no leak"
    );
}

/// A minimal stub that, for any request, replies 500 with a body echoing the
/// request's `Authorization` header verbatim. Used to prove the secret-safety
/// invariant when a server reflects the token back in an error body.
fn spawn_echo_stub() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        for s in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut stream = s;
                let req = read_request(&mut stream);
                let auth = header(&req, "authorization").to_string();
                let body =
                    format!(r#"{{"ok":false,"data":null,"debug":"got Authorization: {auth}"}}"#);
                let resp = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    format!("http://{addr}")
}

/// When the server echoes the `Authorization` header back in an error body,
/// the token value is redacted from the error rendering (never leaks).
#[test]
fn auth_echo_in_error_body_is_redacted() {
    let base = spawn_echo_stub();
    let secret = "leaky-auth-secret-value";
    let path = write_temp_file("outline-token-echo500.txt", &format!("{secret}\n"));
    let client = OutlineClient::new(
        &base,
        SecretRef::File {
            path: path.to_path_buf(),
            key: None,
        },
        Timeouts::default(),
    )
    .expect("client");
    let err = client.document_info("doc-abc-123").expect_err("must fail");
    let msg = err.to_string();
    // The raw secret must never appear anywhere in the rendering, and the
    // redaction marker must be present where the echoed token was.
    assert!(!msg.contains(secret), "no secret leak in Display: {msg}");
    assert!(
        !format!("{err:?}").contains(secret),
        "no secret leak in Debug"
    );
    assert!(
        msg.contains("<redacted>"),
        "redaction marker present: {msg}"
    );
}

/// `repo_search` sends `GET /repos/search?q=…` with `Authorization: token …`.
#[test]
fn repo_search_decodes() {
    let (base, capture) = spawn_stub();
    let client = gitea_client(&base);
    let repos = client.repo_search("awili").expect("repo_search");
    assert_eq!(repos.len(), 2);
    assert_eq!(repos[0].full_name, "testuser/awili");
    assert_eq!(repos[0].owner.login, "testuser");
    assert_eq!(repos[0].name, "awili");
    assert!(!repos[0].fork);
    assert_eq!(repos[0].default_branch.as_deref(), Some("main"));
    assert_eq!(repos[1].full_name, "testuser/awili-deploy");
    let reqs = capture.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "GET");
    assert!(r.path.starts_with("/repos/search?q="), "path");
    assert!(r.auth.starts_with("token "), "token scheme");
    assert!(!r.auth.to_lowercase().starts_with("bearer"), "not bearer");
}

/// The auth header uses the `token` scheme, never `Bearer`.
#[test]
fn token_header_is_token_scheme_not_bearer() {
    let (base, capture) = spawn_stub();
    let client = gitea_client(&base);
    let _ = client.repo_search("awili").expect("repo_search");
    let reqs = capture.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let auth = &reqs[0].auth;
    assert!(auth.starts_with("token "), "token scheme");
    assert!(!auth.to_lowercase().starts_with("bearer"), "not bearer");
    assert!(auth.ends_with("test-gitea-token"), "token value");
}

/// `branch` exposes `commit.id` via `head_commit_id()`.
#[test]
fn branch_exposes_commit_id() {
    let (base, capture) = spawn_stub();
    let client = gitea_client(&base);
    let br = client.branch("testuser", "awili", "main").expect("branch");
    assert_eq!(br.name, "main");
    assert_eq!(
        br.head_commit_id(),
        "deadbeefcafe0123456789abcdef0123456789ab"
    );
    let reqs = capture.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "GET");
    assert_eq!(r.path, "/repos/testuser/awili/branches/main");
    assert!(r.auth.starts_with("token "), "auth");
}

/// `raw_file` sends `GET /repos/{o}/{r}/raw/{path}` (no double `/raw/`).
#[test]
fn raw_file_returns_body_verbatim() {
    let (base, capture) = spawn_stub();
    let client = gitea_client(&base);
    let body = client
        .raw_file("testuser", "awili", "/deploy.sh", None)
        .expect("raw_file");
    assert!(body.contains("hello from the raw file endpoint"), "body");
    let reqs = capture.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "GET");
    assert_eq!(r.path, "/repos/testuser/awili/raw/deploy.sh", "path");
    assert!(!r.path.contains("/raw/raw/"), "no double raw");
}

/// `add_deploy_key` POSTs `/repos/{o}/{r}/keys` with `{"title","key"}`.
#[test]
fn deploy_key_posts_title_and_key() {
    let (base, capture) = spawn_stub();
    let client = gitea_client(&base);
    let key = client
        .add_deploy_key(
            "testuser",
            "awili",
            "awili-deploy-key",
            "ssh-ed25519 AAAAsynthetickeydata deploy@awili",
        )
        .expect("add_deploy_key");
    assert_eq!(key.id, 7);
    assert_eq!(key.title, "awili-deploy-key");
    assert!(key.read_only);
    let reqs = capture.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/repos/testuser/awili/keys");
    let body: serde_json::Value = serde_json::from_str(&r.body).expect("json");
    assert_eq!(body["title"], "awili-deploy-key");
    assert_eq!(body["key"], "ssh-ed25519 AAAAsynthetickeydata deploy@awili");
    assert!(r.auth.starts_with("token "), "auth");
}

/// `Debug` on both clients shows `<redacted>`, never the token value.
#[test]
fn client_debug_is_redacted() {
    let (base, _capture) = spawn_stub();
    let path_o = write_temp_file("outline-token-dbg.txt", "test-outline-secret-dbg\n");
    let outline = OutlineClient::new(
        &base,
        SecretRef::File {
            path: path_o.to_path_buf(),
            key: None,
        },
        Timeouts::default(),
    )
    .expect("outline");
    let o_dbg = format!("{outline:?}");
    assert!(o_dbg.contains("<redacted>"), "redacted");
    assert!(!o_dbg.contains("test-outline-secret-dbg"), "no leak");
    let path_g = write_temp_file("gitea-token-dbg.txt", "test-gitea-secret-dbg");
    let gitea = GiteaClient::new(
        &base,
        SecretRef::File {
            path: path_g.to_path_buf(),
            key: None,
        },
        Timeouts::default(),
    )
    .expect("gitea");
    let g_dbg = format!("{gitea:?}");
    assert!(g_dbg.contains("<redacted>"), "redacted");
    assert!(!g_dbg.contains("test-gitea-secret-dbg"), "no leak");
}
