//! The fixture's OWN tests. A fault-injecting gate is only useful if each
//! fault provably fires — otherwise a C2 test could pass because the
//! server never misbehaved, which is the worst possible failure mode for
//! an acceptance harness.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_awili-mcp-testserver")
}

struct Server {
    child: Child,
    out: Option<BufReader<ChildStdout>>,
}

/// RC2-SVC8 (F0722): every variable the fixture reads. A test process that
/// happens to carry one (an exported `MCP_FAULT`, say) must not change the
/// fixture's behaviour unless the test sets it on purpose.
const FIXTURE_VARS: &[&str] = &[
    "MCP_FAULT",
    "MCP_DELAY_MS",
    "MCP_FLOOD_N",
    "MCP_ROOTS_EPOCH",
    "MCP_ROOTS_EPOCH_SEQ",
    "MCP_LAUNCH_COUNTER",
    "MCP_HANDSHAKE_LOG",
];

/// The fixture command: every [`FIXTURE_VARS`] entry removed from the
/// inherited environment, then exactly the test's own `env` set.
fn fixture_command(env: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(bin());
    for var in FIXTURE_VARS {
        cmd.env_remove(var);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd
}

impl Server {
    fn spawn(env: &[(&str, &str)]) -> Self {
        let mut cmd = fixture_command(env);
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn fixture");
        let out = Some(BufReader::new(child.stdout.take().unwrap()));
        Self { child, out }
    }

    fn send(&mut self, msg: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    /// Read with a deadline on a worker thread — a fixture regression must
    /// FAIL the suite, never wedge CI (Codex P1).
    fn recv_deadline(&mut self, ms: u64) -> Option<serde_json::Value> {
        let mut reader = self.out.take().expect("reader");
        let (tx, rx) = std::sync::mpsc::channel();
        let h = std::thread::spawn(move || {
            let mut line = String::new();
            let n = reader.read_line(&mut line).unwrap_or(0);
            let _ = tx.send((n, line));
            reader
        });
        let got = rx.recv_timeout(Duration::from_millis(ms)).ok();
        match got {
            Some((n, line)) => {
                self.out = Some(h.join().expect("reader thread"));
                if n == 0 {
                    None
                } else {
                    serde_json::from_str(line.trim()).ok()
                }
            }
            None => None, // reader thread is detached; the Drop kill releases it
        }
    }

    fn recv(&mut self) -> Option<serde_json::Value> {
        let mut line = String::new();
        match self.out.as_mut().unwrap().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => serde_json::from_str(line.trim()).ok(),
        }
    }

    fn recv_raw(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.out.as_mut().unwrap().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }

    fn call_echo(&mut self, id: u32, text: &str) {
        self.send(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"echo","arguments":{{"text":"{text}"}}}}}}"#
        ));
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn happy_path_initialize_list_call() {
    let mut s = Server::spawn(&[]);
    s.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
    let init = s.recv().unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    // The initialized NOTIFICATION must draw NO response — the next thing
    // we read has to be the tools/list result (Codex P0).
    s.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    s.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    let list = s.recv().unwrap();
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    // Real MCP annotations, plus Awili's namespaced hint that the C2-u1
    // registry consumes for its risk floor.
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
    assert_eq!(tools[0]["_meta"]["awili/sideEffect"], "read_only");
    assert_eq!(tools[1]["annotations"]["readOnlyHint"], false);
    assert_eq!(tools[1]["_meta"]["awili/sideEffect"], "mutating");

    s.call_echo(3, "gate");
    let r = s.recv().unwrap();
    assert_eq!(r["id"], 3);
    assert_eq!(r["result"]["content"][0]["text"], "gate");
}

#[test]
fn unknown_tool_and_method_produce_typed_errors() {
    let mut s = Server::spawn(&[]);
    s.send(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope"}}"#);
    // Unknown TOOL is invalid params, not method-not-found.
    assert_eq!(s.recv().unwrap()["error"]["code"], -32602);
    s.send(r#"{"jsonrpc":"2.0","id":2,"method":"whatever"}"#);
    assert_eq!(s.recv().unwrap()["error"]["code"], -32601);
    s.send("not json at all");
    assert_eq!(s.recv().unwrap()["error"]["code"], -32700);
}

#[test]
fn fault_slow_actually_delays() {
    let mut s = Server::spawn(&[("MCP_FAULT", "slow"), ("MCP_DELAY_MS", "400")]);
    s.call_echo(1, "x");
    let t0 = Instant::now();
    let r = s.recv().unwrap();
    assert!(
        t0.elapsed() >= Duration::from_millis(350),
        "delay did not fire"
    );
    assert_eq!(r["id"], 1);
}

#[test]
fn fault_crash_on_call_kills_mid_request() {
    let mut s = Server::spawn(&[("MCP_FAULT", "crash_on_call")]);
    s.call_echo(1, "x");
    // No reply, stream closes: exactly the "dispatch started, never
    // finalized" shape B2 classifies Uncertain.
    assert!(s.recv().is_none(), "server must die without replying");
    let status = s.child.wait().unwrap();
    assert!(!status.success());
}

#[test]
fn fault_crash_after_n_serves_then_dies() {
    let mut s = Server::spawn(&[("MCP_FAULT", "crash_after=2")]);
    for i in 1..=2 {
        s.call_echo(i, "ok");
        assert_eq!(s.recv().unwrap()["id"], i);
    }
    s.call_echo(3, "boom");
    assert!(s.recv().is_none(), "third call must kill the server");
}

#[test]
fn fault_malformed_emits_invalid_json() {
    let mut s = Server::spawn(&[("MCP_FAULT", "malformed")]);
    s.call_echo(1, "x");
    let raw = s.recv_raw().expect("some bytes");
    assert!(
        serde_json::from_str::<serde_json::Value>(raw.trim()).is_err(),
        "malformed fault must emit UNPARSEABLE json, got: {raw}"
    );
}

#[test]
fn fault_wrong_id_breaks_correlation() {
    let mut s = Server::spawn(&[("MCP_FAULT", "wrong_id")]);
    s.call_echo(7, "x");
    let r = s.recv().unwrap();
    assert_ne!(r["id"], 7, "wrong_id fault must not echo the request id");
}

#[test]
fn fault_flood_emits_many_unsolicited_messages() {
    let mut s = Server::spawn(&[("MCP_FAULT", "flood"), ("MCP_FLOOD_N", "50")]);
    s.call_echo(1, "x");
    let mut notifications = 0;
    let mut saw_result = false;
    for i in 0..60 {
        // The first message waits out the server's start (spawn and first
        // exec can stall for seconds under load — this flaked the Mac
        // gate with 0 of 50); the test checks the counts, not the timing.
        let wait = if i == 0 { 15_000 } else { 2000 };
        let Some(v) = s.recv_deadline(wait) else {
            break;
        };
        if v.get("method").and_then(|m| m.as_str()) == Some("notifications/message") {
            // Valid MCP logging shape, or a real client would drop them.
            assert_eq!(v["params"]["level"], "info");
            assert!(v["params"]["data"]["seq"].is_number());
            notifications += 1;
        } else if v.get("result").is_some() {
            saw_result = true;
            break;
        }
    }
    assert_eq!(
        notifications, 50,
        "flood must emit MCP_FLOOD_N notifications"
    );
    assert!(
        saw_result,
        "the real result must still arrive after the flood"
    );
}

#[test]
fn fault_hang_never_replies() {
    let mut s = Server::spawn(&[("MCP_FAULT", "hang")]);
    s.call_echo(1, "x");
    // Read the SERVER's stdout on a worker thread: nothing may arrive.
    let mut reader = s.out.take().expect("reader");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let n = reader.read_line(&mut line).unwrap_or(0);
        let _ = tx.send(n);
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "hang fault must produce no reply within the window"
    );
    // ALIVE-but-stuck, not silently exited (Codex P1): a dead server would
    // also produce no reply, and would prove nothing about timeout paths.
    assert!(
        s.child.try_wait().unwrap().is_none(),
        "hang fault must leave the server RUNNING, not exited"
    );
}

#[test]
fn roots_epoch_is_echoed_for_restart_tests() {
    // C2-u3 wire correction: numeric, namespaced under _meta — typed rmcp
    // clients silently dropped the old top-level string field.
    let mut s = Server::spawn(&[("MCP_ROOTS_EPOCH", "42")]);
    s.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
    assert_eq!(s.recv().unwrap()["result"]["_meta"]["awili/rootsEpoch"], 42);
}

/// MCP stdio shutdown = close the transport; there is no shutdown request.
#[test]
fn closing_stdin_exits_cleanly() {
    let mut s = Server::spawn(&[]);
    s.call_echo(1, "x");
    assert_eq!(s.recv().unwrap()["id"], 1);
    drop(s.child.stdin.take()); // EOF on stdin
    let status = s.child.wait().unwrap();
    assert!(status.success(), "EOF must exit 0, got {status:?}");
}

/// Config typos FAIL CLOSED rather than silently disabling a fault.
#[test]
fn unknown_fault_config_refuses_to_start() {
    let mut s = Server::spawn(&[("MCP_FAULT", "slwo")]);
    let status = s.child.wait().unwrap();
    assert_eq!(status.code(), Some(2), "typo'd fault must abort startup");
    let mut s2 = Server::spawn(&[("MCP_FAULT", "slow"), ("MCP_DELAY_MS", "fast")]);
    assert_eq!(s2.child.wait().unwrap().code(), Some(2));
}

/// String and null ids round-trip (real clients use string ids).
#[test]
fn string_ids_round_trip() {
    let mut s = Server::spawn(&[]);
    s.send(r#"{"jsonrpc":"2.0","id":"call-abc","method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}"#);
    let r = s.recv().unwrap();
    assert_eq!(r["id"], "call-abc");
}

/// touch_file performs a REAL side effect — a reply-only fixture would let
/// sandbox/authz tests pass while nothing happened.
#[test]
fn touch_file_actually_writes() {
    let dir = std::env::temp_dir().join(format!("awili-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("made.txt");
    let mut s = Server::spawn(&[]);
    s.send(&format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"touch_file","arguments":{{"path":"{}"}}}}}}"#,
        target.display()
    ));
    let r = s.recv().unwrap();
    assert_eq!(r["result"]["isError"], false);
    assert!(target.exists(), "fixture must actually create the file");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Required arguments are enforced: a missing or ill-typed argument is
/// -32602, NOT a cheerful isError:false (Codex final P0).
#[test]
fn missing_or_mistyped_arguments_are_refused() {
    let mut s = Server::spawn(&[]);
    s.send(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{}}}"#,
    );
    assert_eq!(s.recv().unwrap()["error"]["code"], -32602);
    s.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":42}}}"#);
    assert_eq!(s.recv().unwrap()["error"]["code"], -32602);
    s.send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"touch_file","arguments":{}}}"#);
    assert_eq!(s.recv().unwrap()["error"]["code"], -32602);
}

/// JSON-RPC ids must be string/number/null; anything else is -32600.
#[test]
fn invalid_id_types_are_refused() {
    let mut s = Server::spawn(&[]);
    s.send(r#"{"jsonrpc":"2.0","id":true,"method":"tools/list"}"#);
    assert_eq!(s.recv().unwrap()["error"]["code"], -32600);
    s.send(r#"{"jsonrpc":"2.0","id":{"a":1},"method":"tools/list"}"#);
    assert_eq!(s.recv().unwrap()["error"]["code"], -32600);
}

/// slow_initialize delays the HANDSHAKE (not tools/call): the C2-u2
/// shutdown-during-connect test needs a server that is provably slow to
/// initialize, and a fault that silently didn't fire would let that test
/// pass against an instant connect.
#[test]
fn slow_initialize_delays_the_handshake() {
    let mut s = Server::spawn(&[("MCP_FAULT", "slow_initialize"), ("MCP_DELAY_MS", "700")]);
    let t0 = std::time::Instant::now();
    s.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#);
    let r = s.recv().unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
    assert!(
        elapsed >= std::time::Duration::from_millis(700),
        "initialize answered in {elapsed:?}; the fault did not fire"
    );
    // And tools/call is NOT delayed by this fault.
    s.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"quick"}}}"#);
    let t1 = std::time::Instant::now();
    let r = s.recv().unwrap();
    assert!(t1.elapsed() < std::time::Duration::from_millis(500));
    assert_eq!(r["result"]["content"][0]["text"], "quick");
}

/// C2-u3: the advertised roots epoch is a JSON NUMBER in
/// `_meta["awili/rootsEpoch"]` — present exactly when MCP_ROOTS_EPOCH is
/// set, ABSENT otherwise (no proposal is not epoch 0), and a malformed
/// value kills startup (fail-closed config).
#[test]
fn roots_epoch_rides_meta_as_a_number_or_not_at_all() {
    let mut s = Server::spawn(&[("MCP_ROOTS_EPOCH", "42")]);
    s.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#);
    let r = s.recv().unwrap();
    assert_eq!(r["result"]["_meta"]["awili/rootsEpoch"], 42);
    assert!(r["result"]["_meta"]["awili/rootsEpoch"].is_u64());

    let mut s = Server::spawn(&[]);
    s.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#);
    let r = s.recv().unwrap();
    assert!(
        r["result"].get("_meta").is_none(),
        "no env => no proposal, not epoch 0: {r}"
    );
}

/// Malformed MCP_ROOTS_EPOCH aborts startup, same fail-closed rule as
/// every other config knob.
#[test]
fn malformed_roots_epoch_aborts_startup() {
    let out = fixture_command(&[("MCP_ROOTS_EPOCH", "not-a-number")])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "must exit 2 on malformed config"
    );
}

/// RC2-SVC8 (F0722): the fixture never inherits a fault variable from the
/// test process — every variable it reads is removed before the test's own
/// settings are applied.
#[test]
fn rc2svc8_fixture_variables_are_not_inherited() {
    let cmd = fixture_command(&[("MCP_FAULT", "slow")]);
    let envs: Vec<(String, Option<String>)> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    for var in FIXTURE_VARS.iter().filter(|v| **v != "MCP_FAULT") {
        assert!(
            envs.contains(&(var.to_string(), None)),
            "{var} must be removed from the inherited environment: {envs:?}"
        );
    }
    assert!(envs.contains(&("MCP_FAULT".to_string(), Some("slow".to_string()))));
}

/// RC2-SVC8 (F0729): the handshake log is written on the FIRST stdin byte —
/// a partial line with no newline is already enough.
#[test]
fn rc2svc8_the_handshake_log_fires_on_the_first_byte() {
    let dir = std::env::temp_dir().join(format!("rc2svc8-handshake-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("handshake.log");
    let _ = std::fs::remove_file(&log);
    let mut s = Server::spawn(&[("MCP_HANDSHAKE_LOG", log.to_str().unwrap())]);
    {
        let stdin = s.child.stdin.as_mut().unwrap();
        stdin.write_all(b"{").unwrap();
        stdin.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(&log)
        .map(|t| t.contains("byte received"))
        .unwrap_or(false)
    {
        assert!(
            Instant::now() < deadline,
            "no handshake log after a partial line"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
