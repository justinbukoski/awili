//! `awili-mcp-testserver` — a minimal, deliberately-abusable MCP server over
//! stdio. THIS IS THE GATE for C2-u2/u3 (see docs/contracts/c2-build-plan.md):
//! the runtime-thread acceptance criteria — only canonical values cross the
//! channel, cancellation certainty, server restart, backpressure — are not
//! provable without a server that can be made to stall, crash, and lie.
//!
//! Protocol: JSON-RPC 2.0, newline-delimited, over stdin/stdout (the same
//! framing `rmcp`'s stdio transport speaks). Implemented by hand with zero
//! async dependencies so the fixture cannot fail for the same reasons the
//! code under test might.
//!
//! Methods: `initialize`, `tools/list`, `tools/call`. There is deliberately
//! NO bespoke `shutdown` request (Codex P1): MCP stdio shutdown is closing
//! the transport and waiting for exit, and a fake method here would teach
//! C2-u3 to implement a protocol that does not exist. Close stdin instead.
//!
//! Fault injection via env vars, so a test spawns a differently-behaved
//! server without a different binary:
//!
//! | env | effect |
//! |---|---|
//! | `MCP_FAULT=slow` | every `tools/call` sleeps `MCP_DELAY_MS` (default 5000) — backpressure/cancellation |
//! | `MCP_FAULT=crash_on_call` | process exits mid-request — restart/uncertain-dispatch |
//! | `MCP_FAULT=crash_after=N` | exits after N successful calls — restart with state |
//! | `MCP_FAULT=malformed` | replies with invalid JSON — decoder robustness |
//! | `MCP_FAULT=wrong_id` | replies with a mismatched request id — correlation |
//! | `MCP_FAULT=flood` | emits `MCP_FLOOD_N` unsolicited notifications per call — backpressure |
//! | `MCP_FAULT=hang` | never replies — timeout paths |
//! | `MCP_ROOTS_EPOCH=x` | value echoed in `initialize` for root-epoch tests |
//! | `MCP_ROOTS_EPOCH_SEQ=a,b,c` | per-LAUNCH epoch sequence (unit 4b): launch N advertises the Nth value (last repeats) |
//! | `MCP_LAUNCH_COUNTER=path` | unit 4b: append one line per process launch — row-10 proof that a reconnect is a fresh child |
//! | `MCP_HANDSHAKE_LOG=path` | unit 4b: append one line the moment ANY stdin byte arrives — row-5 proof that attestation kill precedes every handshake byte |

use std::io::{BufRead, Write};

fn main() {
    let fault = std::env::var("MCP_FAULT").unwrap_or_default();
    // FAIL CLOSED on config (Codex P1): a typo'd fault name must not
    // silently become "behave normally" — that would let a C2 acceptance
    // test pass with the fault disabled.
    const KNOWN: &[&str] = &[
        "",
        "slow",
        "slow_initialize",
        "crash_on_call",
        "malformed",
        "wrong_id",
        "flood",
        "hang",
    ];
    if !KNOWN.contains(&fault.as_str()) && !fault.starts_with("crash_after=") {
        eprintln!("awili-mcp-testserver: unknown MCP_FAULT={fault:?}");
        std::process::exit(2);
    }
    if fault.starts_with("crash_after=")
        && fault
            .strip_prefix("crash_after=")
            .and_then(|v| v.parse::<u64>().ok())
            .is_none()
    {
        eprintln!("awili-mcp-testserver: malformed crash_after");
        std::process::exit(2);
    }
    for (var, val) in [
        ("MCP_DELAY_MS", std::env::var("MCP_DELAY_MS")),
        ("MCP_FLOOD_N", std::env::var("MCP_FLOOD_N")),
        ("MCP_ROOTS_EPOCH", std::env::var("MCP_ROOTS_EPOCH")),
    ] {
        if let Ok(v) = val {
            if v.parse::<u64>().is_err() {
                eprintln!("awili-mcp-testserver: malformed {var}={v:?}");
                std::process::exit(2);
            }
        }
    }
    let delay_ms: u64 = std::env::var("MCP_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000);
    let crash_after: Option<u64> = fault
        .strip_prefix("crash_after=")
        .and_then(|v| v.parse().ok());
    let flood_n: usize = std::env::var("MCP_FLOOD_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);

    // Unit 4b levers. The launch counter appends BEFORE any I/O: it is the
    // per-process launch proof. The handshake log appends on the FIRST
    // stdin byte only: its absence proves no MCP byte ever reached this
    // process. The epoch sequence indexes by the launch count.
    let launch_index: u64 = if let Ok(path) = std::env::var("MCP_LAUNCH_COUNTER") {
        use std::io::Write as _;
        let prior = std::fs::read_to_string(&path)
            .map(|t| t.lines().count() as u64)
            .unwrap_or(0);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .expect("launch counter path");
        writeln!(f, "launch {}", prior + 1).expect("launch counter write");
        prior
    } else {
        0
    };
    let roots_epoch_seq: Option<Vec<u64>> = std::env::var("MCP_ROOTS_EPOCH_SEQ").ok().map(|v| {
        v.split(',')
            .map(|p| {
                p.trim().parse::<u64>().unwrap_or_else(|_| {
                    eprintln!("awili-mcp-testserver: malformed MCP_ROOTS_EPOCH_SEQ={v:?}");
                    std::process::exit(2);
                })
            })
            .collect()
    });
    let handshake_log = std::env::var("MCP_HANDSHAKE_LOG").ok();

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    let mut calls_served: u64 = 0;

    // RC2-SVC8 (F0729): the handshake log is written the moment the FIRST
    // stdin byte arrives (`fill_buf` returns as soon as any byte is
    // buffered), not after a whole line — a partial line is already a byte
    // that reached this process.
    if let Some(path) = &handshake_log {
        if matches!(input.fill_buf(), Ok(buf) if !buf.is_empty()) {
            use std::io::Write as _;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(f, "byte received (launch {})", launch_index + 1);
            }
        }
    }

    for line in input.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<serde_json::Value>(line) else {
            // A client sending us garbage gets a parse error, per JSON-RPC.
            let _ = writeln!(
                out,
                r#"{{"jsonrpc":"2.0","id":null,"error":{{"code":-32700,"message":"parse error"}}}}"#
            );
            let _ = out.flush();
            continue;
        };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        // A JSON-RPC NOTIFICATION has no `id` and MUST NOT be answered
        // (Codex P0: replying to notifications/initialized with an error
        // is illegal and would teach the C2 client the wrong handshake).
        if req.get("id").is_none() {
            match method {
                "notifications/initialized" | "notifications/cancelled" => {}
                _ => {} // unknown notifications are ignored, never answered
            }
            continue;
        }
        let id = req.get("id").cloned().unwrap();
        // JSON-RPC 2.0: an id MUST be a string, number, or null. Accepting
        // booleans/objects would teach the client that anything goes
        // (Codex final P1).
        if !(id.is_string() || id.is_number() || id.is_null()) {
            error(
                &mut out,
                &serde_json::Value::Null,
                -32600,
                "invalid request: id must be a string, number, or null",
            );
            continue;
        }

        match method {
            "initialize" => {
                if fault == "slow_initialize" {
                    // Delay the handshake itself (shutdown-during-connect
                    // coverage in C2-u2). Same knob as `slow`.
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
                let mut result = serde_json::json!({
                    // 2025-03-26: the version that actually defines tool
                    // annotations, which this server advertises.
                    "protocolVersion": "2025-03-26",
                    "serverInfo": {"name": "awili-mcp-testserver", "version": "1"},
                    "capabilities": {"tools": {}, "logging": {}},
                });
                // The advertised roots epoch rides _meta as a JSON NUMBER
                // (C2-u3: typed rmcp clients drop unknown top-level
                // fields; the old top-level "rootsEpoch" string was
                // invisible to them). Absent env = NO proposal — the
                // field is omitted entirely, never defaulted to 0. A
                // malformed value already aborted startup above.
                if let Some(seq) = &roots_epoch_seq {
                    // Unit 4b: the Nth LAUNCH advertises the Nth value
                    // (the last repeats) — reconnect-epoch rows drive this.
                    let idx = (launch_index as usize).min(seq.len().saturating_sub(1));
                    result["_meta"] = serde_json::json!({"awili/rootsEpoch": seq[idx]});
                } else if let Ok(v) = std::env::var("MCP_ROOTS_EPOCH") {
                    let n: u64 = v.parse().expect("validated at startup");
                    result["_meta"] = serde_json::json!({"awili/rootsEpoch": n});
                }
                reply(&mut out, &id, result);
            }
            "tools/list" => {
                // RC1-N: `MCP_EXTRA_TOOL=<name>` also advertises a read-only
                // tool of that name (answered like `echo`), so a test can
                // see a named tool offered or withheld.
                let extra = std::env::var("MCP_EXTRA_TOOL").ok().map(|name| {
                    serde_json::json!({
                        "name": name,
                        "description": "An extra read-only test tool.",
                        "inputSchema": {"type": "object", "properties": {}},
                        "annotations": {"readOnlyHint": true, "destructiveHint": false},
                        "_meta": {"awili/sideEffect": "read_only"}
                    })
                });
                let mut list = serde_json::json!({"tools": [
                    {
                        "name": "echo",
                        "description": "Echo the text argument back.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"text": {"type": "string"}},
                            "required": ["text"]
                        },
                        // MCP `annotations` (2025-03-26+) uses readOnlyHint /
                        // destructiveHint — NOT a "sideEffect" string. Awili's
                        // own classification rides a NAMESPACED _meta field so
                        // the registry can consume it without pretending it is
                        // standard MCP (Codex P0).
                        "annotations": {"readOnlyHint": true, "destructiveHint": false},
                        "_meta": {"awili/sideEffect": "read_only"}
                    },
                    {
                        "name": "touch_file",
                        "description": "Create a file (mutating; used for authz/sandbox paths).",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"path": {"type": "string"}},
                            "required": ["path"]
                        },
                        "annotations": {"readOnlyHint": false, "destructiveHint": false},
                        "_meta": {"awili/sideEffect": "mutating"}
                    }
                ]});
                if let (Some(extra), Some(tools)) = (extra, list["tools"].as_array_mut()) {
                    tools.push(extra);
                }
                reply(&mut out, &id, list);
            }
            "tools/call" => {
                match fault.as_str() {
                    "hang" => {
                        // Never reply; the client must time out on its own.
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(3600));
                        }
                    }
                    "crash_on_call" => {
                        // Die MID-REQUEST: the client saw the write succeed but
                        // will never see a result — the "uncertain dispatch"
                        // case B2's replay classifier exists for.
                        std::process::exit(9);
                    }
                    "malformed" => {
                        let _ = writeln!(
                            out,
                            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"content\":["
                        );
                        let _ = out.flush();
                        continue;
                    }
                    "wrong_id" => {
                        reply(
                            &mut out,
                            &serde_json::Value::String("not-your-id".into()),
                            serde_json::json!({"content": [{"type": "text", "text": "mismatched"}]}),
                        );
                        continue;
                    }
                    "slow" => std::thread::sleep(std::time::Duration::from_millis(delay_ms)),
                    "flood" => {
                        for i in 0..flood_n {
                            // Valid MCP logging notification shape: level + data
                            // (Codex P1) — a client may legitimately drop
                            // malformed ones, which would neuter the fault.
                            let _ = writeln!(
                                out,
                                r#"{{"jsonrpc":"2.0","method":"notifications/message","params":{{"level":"info","logger":"fixture","data":{{"seq":{i}}}}}}}"#
                            );
                        }
                        let _ = out.flush();
                    }
                    _ => {}
                }
                if let Some(n) = crash_after {
                    if calls_served >= n {
                        std::process::exit(9);
                    }
                }
                calls_served += 1;

                let params = req
                    .get("params")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                match name {
                    "echo" => {
                        // Required-argument validation (Codex final P0): a
                        // fixture that accepts a missing/ill-typed argument
                        // and returns isError:false would let a C2 argument-
                        // projection bug pass unnoticed.
                        let Some(text) = args.get("text").and_then(|t| t.as_str()) else {
                            error(&mut out, &id, -32602, "echo requires a string 'text'");
                            continue;
                        };
                        reply(
                            &mut out,
                            &id,
                            serde_json::json!({"content": [{"type": "text", "text": text}], "isError": false}),
                        );
                    }
                    "touch_file" => {
                        let Some(path) = args.get("path").and_then(|t| t.as_str()) else {
                            error(&mut out, &id, -32602, "touch_file requires a string 'path'");
                            continue;
                        };
                        // REAL side effect (Codex P0): a fixture that reports
                        // success without doing anything lets an authz/sandbox
                        // test pass while nothing happened — the precise
                        // false-positive this harness exists to prevent. Tests
                        // assert the file's existence, not the reply text.
                        match std::fs::write(path, b"awili-mcp-testserver") {
                            Ok(()) => reply(
                                &mut out,
                                &id,
                                serde_json::json!({"content": [{"type": "text", "text": format!("touched {path}")}], "isError": false}),
                            ),
                            Err(e) => reply(
                                &mut out,
                                &id,
                                serde_json::json!({"content": [{"type": "text", "text": format!("failed: {e}")}], "isError": true}),
                            ),
                        }
                    }
                    other => {
                        // -32602 invalid params: `tools/call` EXISTS; it is the
                        // tool NAME that is wrong (Codex P1).
                        error(&mut out, &id, -32602, &format!("unknown tool {other}"));
                    }
                }
            }
            other => error(&mut out, &id, -32601, &format!("unknown method {other}")),
        }
    }
}

fn reply(out: &mut impl Write, id: &serde_json::Value, result: serde_json::Value) {
    let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

fn error(out: &mut impl Write, id: &serde_json::Value, code: i64, message: &str) {
    let msg = serde_json::json!({
        "jsonrpc": "2.0", "id": id,
        "error": {"code": code, "message": message}
    });
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}
