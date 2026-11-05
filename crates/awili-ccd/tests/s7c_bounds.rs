//! S7c proofs (F0343 deadlines, path ids, body cap) for `CcdClient`, driven
//! through real HTTP against localhost stubs.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use awili_ccd::{
    read_capped, CcdClient, CcdError, CcdTimeouts, DispatchReq, CCD_BAD_JOB_ID, CCD_BODY_TOO_LARGE,
    MAX_CCD_BODY_BYTES,
};

/// Short deadlines for the stall tests: each one is distinct, so the
/// elapsed time shows which deadline fired.
fn short() -> CcdTimeouts {
    CcdTimeouts {
        connect: Duration::from_secs(1),
        request: Duration::from_secs(1),
        dispatch: Duration::from_secs(3),
        verify: Duration::from_secs(5),
    }
}

/// A stub that accepts connections, reads the request head, and never
/// answers. Each socket is held open for 60 s so only the client's own
/// deadline can end the call.
fn stalled() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(mut sock) = sock else { continue };
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf);
                thread::sleep(Duration::from_secs(60));
                drop(sock);
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Run `call` on its own thread and return (result, elapsed). A call that
/// is still blocked after `limit` fails the test instead of hanging it.
fn timed<T: Send + 'static>(
    limit: Duration,
    call: impl FnOnce() -> T + Send + 'static,
) -> (T, Duration) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let start = Instant::now();
        let out = call();
        let _ = tx.send((out, start.elapsed()));
    });
    rx.recv_timeout(limit)
        .unwrap_or_else(|_| panic!("the ccd call was still blocked after {limit:?}: no deadline"))
}

fn assert_transport<T: std::fmt::Debug>(r: Result<T, CcdError>, what: &str) {
    assert!(
        matches!(r, Err(CcdError::Transport(_))),
        "{what}: expected a transport timeout, got {r:?}"
    );
}

/// F0343: a stalled ccd can hold a job read for at most `request`.
#[test]
fn s7c_stalled_job_read_ends_at_the_request_deadline() {
    let base = stalled();
    let (r, took) = timed(Duration::from_secs(10), move || {
        CcdClient::with_timeouts(base, "tok", short()).job("j1")
    });
    assert_transport(r, "job");
    assert!(took >= Duration::from_millis(900), "ended early: {took:?}");
    assert!(took < Duration::from_millis(2500), "ended late: {took:?}");
}

/// F0343: the stop request has the same `request` deadline.
#[test]
fn s7c_stalled_stop_ends_at_the_request_deadline() {
    let base = stalled();
    let (r, took) = timed(Duration::from_secs(10), move || {
        CcdClient::with_timeouts(base, "tok", short()).kill("j1")
    });
    assert_transport(r, "stop");
    assert!(took < Duration::from_millis(2500), "ended late: {took:?}");
}

/// F0343: dispatch has its own, longer deadline (not `request`).
#[test]
fn s7c_stalled_dispatch_ends_at_the_dispatch_deadline() {
    let base = stalled();
    let (r, took) = timed(Duration::from_secs(15), move || {
        CcdClient::with_timeouts(base, "tok", short()).dispatch(&DispatchReq::new("r", "b", "s"))
    });
    assert_transport(r, "dispatch");
    assert!(
        took >= Duration::from_millis(2700),
        "used the wrong deadline: {took:?}"
    );
    assert!(took < Duration::from_millis(4500), "ended late: {took:?}");
}

/// F0343: verify has its own, longest deadline (ccd runs the verify
/// command inside the request).
#[test]
fn s7c_stalled_verify_ends_at_the_verify_deadline() {
    let base = stalled();
    let (r, took) = timed(Duration::from_secs(15), move || {
        CcdClient::with_timeouts(base, "tok", short()).verify("j1", "rust_check")
    });
    assert_transport(r, "verify");
    assert!(
        took >= Duration::from_millis(4700),
        "used the wrong deadline: {took:?}"
    );
    assert!(took < Duration::from_millis(6500), "ended late: {took:?}");
}

/// F0343: polling can overshoot its own timeout by at most one request
/// deadline, even when every poll stalls.
#[test]
fn s7c_poll_is_bounded_when_every_request_stalls() {
    let base = stalled();
    let (r, took) = timed(Duration::from_secs(10), move || {
        CcdClient::with_timeouts(base, "tok", short()).poll_until_terminal(
            "j1",
            Duration::from_millis(50),
            Duration::from_millis(100),
            &|| false,
        )
    });
    assert_transport(r, "poll");
    assert!(took < Duration::from_millis(2500), "ended late: {took:?}");
}

/// The production deadlines cover ccd's own server-side bounds.
#[test]
fn s7c_default_deadlines_cover_ccd_server_bounds() {
    let t = CcdTimeouts::default();
    assert_eq!(t.connect, Duration::from_secs(10));
    assert_eq!(t.request, Duration::from_secs(60));
    assert_eq!(t.dispatch, Duration::from_secs(180));
    assert_eq!(t.verify, Duration::from_secs(660));
    // ccd VERIFY_TIMEOUT is 600 s; its preflight cap is 120 s + 2 s.
    assert!(t.verify > Duration::from_secs(600));
    assert!(t.dispatch > Duration::from_secs(122));
}

/// A stub that answers every request with `status` and `body` and sends
/// each request line back over the channel.
fn answering(status: &'static str, body: Vec<u8>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { continue };
            let tx = tx.clone();
            let body = body.clone();
            thread::spawn(move || answer(sock, status, &body, &tx));
        }
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

fn answer(mut sock: TcpStream, status: &str, body: &[u8], tx: &mpsc::Sender<String>) {
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
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    resp.extend_from_slice(body);
    let _ = sock.write_all(&resp);
}

/// A crafted job id stays one path segment on every job route.
#[test]
fn s7c_crafted_job_ids_stay_one_path_segment() {
    let (base, rx) = answering("404 Not Found", br#"{"error":"no such job"}"#.to_vec());
    let client = CcdClient::with_timeouts(base, "tok", short());
    let id = "../dispatch?x=1#f";
    let enc = "..%2Fdispatch%3Fx%3D1%23f";
    let _ = client.job(id);
    assert_eq!(rx.recv().unwrap(), format!("GET /jobs/{enc} HTTP/1.1"));
    let _ = client.kill(id);
    assert_eq!(
        rx.recv().unwrap(),
        format!("POST /jobs/{enc}/kill HTTP/1.1")
    );
    let _ = client.verify(id, "p");
    assert_eq!(
        rx.recv().unwrap(),
        format!("POST /jobs/{enc}/verify HTTP/1.1")
    );
    // The canary's probe id is plain and passes through unchanged.
    let _ = client.job("__canary_probe__");
    assert_eq!(rx.recv().unwrap(), "GET /jobs/__canary_probe__ HTTP/1.1");
}

/// An empty id, `.` or `..` is refused before any connection is made.
#[test]
fn s7c_empty_and_dot_job_ids_are_refused_before_sending() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("addr").port();
    let client = CcdClient::with_timeouts(format!("http://127.0.0.1:{port}"), "tok", short());
    for id in ["", ".", ".."] {
        for r in [
            client.job(id).map(|_| ()),
            client.kill(id),
            client.verify(id, "p").map(|_| ()),
        ] {
            match r {
                Err(CcdError::Transport(msg)) => assert_eq!(msg, CCD_BAD_JOB_ID, "{id:?}"),
                other => panic!("{id:?}: expected refusal, got {other:?}"),
            }
        }
    }
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "no connection may be opened"
    );
}

/// A 2xx body over the cap is refused with the plain error.
#[test]
fn s7c_oversized_body_is_refused() {
    let (base, _rx) = answering("200 OK", vec![b' '; MAX_CCD_BODY_BYTES as usize + 1]);
    let client = CcdClient::with_timeouts(base, "tok", short());
    match client.job("j1") {
        Err(CcdError::Transport(msg)) => assert_eq!(msg, CCD_BODY_TOO_LARGE),
        other => panic!("expected the too-large refusal, got {other:?}"),
    }
}

/// A non-2xx body over the cap keeps its status and suppresses the partial body.
#[test]
fn s7c_oversized_error_body_keeps_status_and_is_cut() {
    let (base, _rx) = answering(
        "500 Internal Server Error",
        vec![b'e'; MAX_CCD_BODY_BYTES as usize + 5],
    );
    let client = CcdClient::with_timeouts(base, "tok", short());
    match client.job("j1") {
        Err(CcdError::Http { status, body }) => {
            assert_eq!(status, 500);
            assert_eq!(body, "response body exceeds size limit");
        }
        other => panic!("expected Http 500, got {other:?}"),
    }
}

/// `read_capped` takes at most `cap + 1` bytes whatever the source offers.
#[test]
fn s7c_reader_stops_at_the_cap() {
    struct Counting {
        left: usize,
        consumed: usize,
    }
    impl Read for Counting {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.left);
            buf[..n].fill(b' ');
            self.left -= n;
            self.consumed += n;
            Ok(n)
        }
    }
    let mut src = Counting {
        left: 4000,
        consumed: 0,
    };
    let (bytes, over) = read_capped(&mut src, 1000).expect("read");
    assert!(over);
    assert_eq!(bytes.len(), 1000);
    assert_eq!(
        src.consumed, 1001,
        "the reader must stop one byte past the cap"
    );
}

#[test]
fn error_diagnostics_redact_synthetic_credentials_on_500_and_404() {
    for status in [500, 404] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0; 4096]; let _ = sock.read(&mut buf);
            let body = "reflected synthetic-token and Bearer\tsecond-credential";
            write!(sock, "HTTP/1.1 {status} test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let error = CcdClient::with_timeouts(base, "synthetic-token", short()).job("j1").unwrap_err();
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains("synthetic-token"));
        assert!(!shown.contains("second-credential"));
    }
}
