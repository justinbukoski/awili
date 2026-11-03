//! S7b proofs (F0714 path ids, F0716 body cap) for [`RestClient`], driven
//! through real HTTP against a one-shot localhost stub.

use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

/// One-shot HTTP stub on 127.0.0.1. It reads one whole request (head plus a
/// `Content-Length` body), sends the request line back over the channel,
/// then answers with `status` and `body`.
fn serve_once(status: &'static str, body: Vec<u8>) -> (RestClient, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let mut req = Vec::new();
        let mut buf = [0u8; 4096];
        let head_end = loop {
            if let Some(i) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => req.extend_from_slice(&buf[..n]),
            }
        };
        let head = String::from_utf8_lossy(&req[..head_end]).into_owned();
        let content_length = head
            .lines()
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while req.len() < head_end + content_length {
            match sock.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => req.extend_from_slice(&buf[..n]),
            }
        }
        let _ = tx.send(head.lines().next().unwrap_or("").to_string());
        let mut resp = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        resp.extend_from_slice(&body);
        let _ = sock.write_all(&resp);
    });
    let client = RestClient::with_timeouts(
        "127.0.0.1",
        port,
        "tok",
        Duration::from_secs(2),
        Duration::from_secs(20),
    );
    (client, rx)
}

type Call = fn(&RestClient, &str) -> Result<(), GatewayError>;

/// Every RestClient verb that puts a dynamic id in its path: method, path
/// template (`{}` = the id segment) and the call.
fn id_routes() -> Vec<(&'static str, &'static str, Call)> {
    vec![
        ("GET", "/api/sessions/{}/messages", |c, id| {
            c.session_messages(id).map(|_| ())
        }),
        ("PATCH", "/api/sessions/{}", |c, id| {
            c.set_session_archived(id, true)
        }),
        ("PATCH", "/api/sessions/{}", |c, id| {
            c.set_session_title(id, "t")
        }),
        ("DELETE", "/api/sessions/{}", |c, id| c.delete_session(id)),
        ("PUT", "/api/tools/toolsets/{}", |c, id| {
            c.set_toolset(id, &serde_json::json!({}))
        }),
        ("POST", "/api/cron/jobs/{}/pause", |c, id| c.pause_job(id)),
        ("POST", "/api/cron/jobs/{}/resume", |c, id| c.resume_job(id)),
        ("POST", "/api/cron/jobs/{}/trigger", |c, id| {
            c.trigger_job(id)
        }),
        ("PUT", "/api/cron/jobs/{}", |c, id| {
            c.update_job(id, &serde_json::json!({}))
        }),
        ("PUT", "/api/messaging/platforms/{}", |c, id| {
            c.update_messaging(id, &serde_json::json!({}))
        }),
        ("POST", "/api/messaging/platforms/{}/test", |c, id| {
            c.test_messaging(id)
        }),
    ]
}

/// F0714: a crafted id can never leave its one path segment. Dot segments,
/// `/`, `?`, `#`, `%`, spaces and non-ASCII are all encoded, on every route.
#[test]
fn s7b_crafted_ids_stay_one_path_segment() {
    let cases = [
        ("../../api/env?x=1#f", "..%2F..%2Fapi%2Fenv%3Fx%3D1%23f"),
        ("%2e%2e", "%252e%252e"),
        ("a b/\u{e9}", "a%20b%2F%C3%A9"),
        ("plain-ID_1.2~x", "plain-ID_1.2~x"),
    ];
    for (method, template, call) in id_routes() {
        for (id, encoded) in cases {
            let (client, rx) = serve_once("200 OK", b"{}".to_vec());
            call(&client, id).unwrap_or_else(|e| panic!("{method} {template} {id:?}: {e}"));
            let line = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the stub saw a request");
            let want = format!("{method} {} HTTP/1.1", template.replace("{}", encoded));
            assert_eq!(line, want, "{method} {template} with id {id:?}");
        }
    }
}

/// F0714: an empty id, `.` or `..` is refused before any connection is made.
#[test]
fn s7b_empty_and_dot_ids_are_refused_before_sending() {
    for (method, template, call) in id_routes() {
        for id in ["", ".", ".."] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.set_nonblocking(true).expect("nonblocking");
            let port = listener.local_addr().expect("addr").port();
            let client = RestClient::with_timeouts(
                "127.0.0.1",
                port,
                "tok",
                Duration::from_secs(1),
                Duration::from_secs(2),
            );
            match call(&client, id) {
                Err(GatewayError::Transport(msg)) => {
                    assert_eq!(msg, BAD_PATH_ID, "{method} {template} {id:?}")
                }
                other => panic!("{method} {template} {id:?}: expected refusal, got {other:?}"),
            }
            assert!(
                matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
                "{method} {template} {id:?}: no connection may be opened"
            );
        }
    }
}

/// F0716: a 2xx body one byte over the cap is refused with the plain error.
#[test]
fn s7b_oversized_success_body_is_refused() {
    let body = vec![b' '; MAX_REST_BODY_BYTES as usize + 1];
    let (client, _rx) = serve_once("200 OK", body);
    match client.model_info() {
        Err(GatewayError::Transport(msg)) => assert_eq!(msg, BODY_TOO_LARGE),
        other => panic!("expected the too-large refusal, got {other:?}"),
    }
}

/// F0716: a body of exactly the cap is still read and parsed.
#[test]
fn s7b_body_at_the_cap_is_read() {
    let mut body = vec![b'"'];
    body.extend(vec![b'a'; MAX_REST_BODY_BYTES as usize - 2]);
    body.push(b'"');
    let (client, _rx) = serve_once("200 OK", body);
    let value = client.model_info().expect("a body at the cap is accepted");
    assert_eq!(
        value.as_str().map(str::len),
        Some(MAX_REST_BODY_BYTES as usize - 2)
    );
}

/// F0716: an oversized non-2xx body keeps its status; only the first
/// `MAX_REST_BODY_BYTES` are kept.
#[test]
fn s7b_oversized_error_body_keeps_status_and_is_cut() {
    let body = vec![b'e'; MAX_REST_BODY_BYTES as usize + 5];
    let (client, _rx) = serve_once("500 Internal Server Error", body);
    match client.model_info() {
        Err(GatewayError::Http { status, body }) => {
            assert_eq!(status, 500);
            assert_eq!(body.len() as u64, MAX_REST_BODY_BYTES);
        }
        other => panic!("expected Http 500, got {other:?}"),
    }
}

/// A reader that serves `left` bytes, then EOF, and counts what it gave out.
struct CountingReader {
    left: usize,
    consumed: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Read for CountingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.left);
        buf[..n].fill(b' ');
        self.left -= n;
        self.consumed
            .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
        Ok(n)
    }
}

/// F0716: `send` reads through `read_capped`, which takes at most `cap + 1`
/// bytes from the wire whatever the server offers, and reports the overflow.
#[test]
fn s7b_reader_stops_at_the_cap() {
    let consumed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reader = CountingReader {
        left: 4000,
        consumed: consumed.clone(),
    };
    let (bytes, over) = read_capped(reader, 1000).expect("read");
    assert!(over, "4000 bytes is over a 1000-byte cap");
    assert_eq!(bytes.len(), 1000, "only the first cap bytes are kept");
    assert_eq!(
        consumed.load(std::sync::atomic::Ordering::SeqCst),
        1001,
        "the reader must stop one byte past the cap"
    );
    let (bytes, over) = read_capped(&b"short"[..], 1000).expect("read");
    assert_eq!((bytes, over), (b"short".to_vec(), false));
}
