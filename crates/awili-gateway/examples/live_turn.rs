//! Live gateway smoke — run ONE full turn through Awili's own GatewayClient against
//! a running Hermes dashboard WS, and confirm turn-end (message.complete AND
//! session.info running:false). This is the [LIVE] confirmation the strangler
//! milestone needs before removing the /v1/runs fallback (plan unit 16).
//!
//! Run: GW_PORT=9119 cargo run -p awili-gateway --example live_turn
//!
//! `GW_TOKEN` is OPTIONAL: if unset (or empty) the example self-scrapes the live
//! session token from the dashboard index page at `http://127.0.0.1:$GW_PORT/`
//! (the injected `window.__HERMES_SESSION_TOKEN__="..."` global). This makes the
//! [LIVE] gate a headless pushbutton — the driver never has to know the rotating
//! token, which changes on every dashboard restart (reboot/KeepAlive). Pass
//! `GW_TOKEN` only to override the scrape (e.g. a remote gateway). `GW_PORT`
//! defaults to 9119, the fixed durable dashboard LaunchAgent port.
//!
//! S4(b) (RVS4-FIX1a, B-L1): the smoke's verdict is decided by
//! [`SmokeJudge::observe`], which `main` calls verbatim for every decoded
//! event, and `main` exits with the verdict's code — 0 (a clean turn end),
//! 13 (a gateway `error` event), 14 (a non-success `message.complete`
//! status) — plus the unchanged pre-loop codes 2 (timeout), 9 (token scrape),
//! 10 (connect), 11 (session), 12 (submit).
use awili_gateway::events::{decode_event, is_turn_end, GatewayEventKind, TurnState};
use awili_gateway::{gateway_url, GatewayClient};
use std::time::{Duration, Instant};

/// Scrape the dashboard session token from the index page's injected JS global.
/// Mirrors `awili_app::work::scrape_dashboard_token` (kept in sync by hand — the
/// gateway crate does not depend on awili-app). Returns the token or a human
/// error string naming what went wrong.
fn scrape_dashboard_token(dashboard_url: &str) -> Result<String, String> {
    scrape_dashboard_token_within(dashboard_url, DASHBOARD_FETCH_TIMEOUT)
}

/// RC2-SVC4 (F0708): the whole dashboard fetch (connect, headers and body)
/// is bounded — a dashboard that accepts the connection and never answers
/// fails the smoke with exit 9 instead of hanging it.
const DASHBOARD_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

fn scrape_dashboard_token_within(dashboard_url: &str, limit: Duration) -> Result<String, String> {
    let url = format!("{}/", dashboard_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(limit)
        .timeout(limit)
        .build()
        .map_err(|e| format!("dashboard client could not be built: {e}"))?;
    let html = client
        .get(&url)
        .send()
        .map_err(|e| format!("dashboard unreachable at {url}: {e}"))?
        .text()
        .map_err(|e| format!("dashboard response unreadable: {e}"))?;
    // window.__HERMES_SESSION_TOKEN__="<token>"
    let start = html
        .find("__HERMES_SESSION_TOKEN__=\"")
        .ok_or_else(|| "dashboard token not found in index HTML".to_string())?;
    let after_prefix = start + "__HERMES_SESSION_TOKEN__=\"".len();
    let end = html[after_prefix..]
        .find('"')
        .ok_or_else(|| "malformed dashboard token injection".to_string())?;
    Ok(html[after_prefix..after_prefix + end].to_string())
}

/// The smoke's verdict for one turn. `main` prints it via [`report`] and exits
/// with [`exit_code`]: 0 for a clean pass, 13 for a gateway `error` event, 14
/// for a non-success `message.complete` status.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SmokeVerdict {
    Pass { reply: String },
    Fail { code: i32, reason: String },
}

impl SmokeVerdict {
    /// The process exit code this verdict maps to.
    fn exit_code(&self) -> i32 {
        match self {
            SmokeVerdict::Pass { .. } => 0,
            SmokeVerdict::Fail { code, .. } => *code,
        }
    }
}

/// `None` or a success-family word → true; anything else → false (fail closed).
/// Mirrors the app's Work `parse_complete_status` success family
/// (`complete|completed|success|ok|done`, case-insensitive).
fn completion_status_ok(status: Option<&str>) -> bool {
    match status {
        None => true,
        Some(s) => matches!(
            s.to_ascii_lowercase().as_str(),
            "complete" | "completed" | "success" | "ok" | "done"
        ),
    }
}

/// Folds the smoke's event stream into a single verdict. `main` calls
/// [`observe`] verbatim for every decoded event; the tests drive the same code
/// to pin the exit-code contract.
#[derive(Default)]
struct SmokeJudge {
    ts: TurnState,
    reply: String,
    decided: bool,
}

impl SmokeJudge {
    /// Fold one decoded event. Returns `Some(verdict)` exactly once (the first
    /// decisive event); `None` before that and on every call after it.
    ///
    /// Step order (S4(b), RVS4-FIX1a B-L1), in order:
    /// 1. `decided` → `None`.
    /// 2. `MessageDelta { text }` → append to the reply.
    /// 3. `MessageComplete { text, status, .. }` → if
    ///    `!completion_status_ok(status)`: decided with
    ///    `Fail { code: 14, reason: "message.complete status {status:?} is not a success" }`;
    ///    else `text` (when present, non-empty) becomes the reply.
    /// 4. `Error { message }` → decided with
    ///    `Fail { code: 13, reason: "gateway error event: {message:?}" }` —
    ///    checked AFTER the reply/status handling, so a non-success
    ///    `message.complete` fails with 14 even when a later `error` would
    ///    otherwise win, but before the terminal check: an `error` event
    ///    makes the turn fail regardless of turn-end.
    /// 5. `is_turn_end(kind, &mut ts)` → decided with `Pass { reply: reply.trim() }`.
    fn observe(&mut self, kind: &GatewayEventKind) -> Option<SmokeVerdict> {
        if self.decided {
            return None;
        }
        match kind {
            GatewayEventKind::MessageDelta { text } => self.reply.push_str(text),
            GatewayEventKind::MessageComplete { text, status, .. } => {
                if !completion_status_ok(status.as_deref()) {
                    self.decided = true;
                    return Some(SmokeVerdict::Fail {
                        code: 14,
                        reason: format!("message.complete status {status:?} is not a success"),
                    });
                }
                if let Some(t) = text {
                    if !t.is_empty() {
                        self.reply = t.clone();
                    }
                }
            }
            GatewayEventKind::Error { message } => {
                self.decided = true;
                return Some(SmokeVerdict::Fail {
                    code: 13,
                    reason: format!("gateway error event: {message:?}"),
                });
            }
            _ => {}
        }
        if is_turn_end(kind, &mut self.ts) {
            self.decided = true;
            return Some(SmokeVerdict::Pass {
                reply: self.reply.trim().to_string(),
            });
        }
        None
    }
}

/// Print the verdict: `Pass` writes the PASS + REPLY lines to stdout (the
/// driver greps both), `Fail` writes one FAIL line to stderr.
fn report(v: &SmokeVerdict, elapsed: Duration) {
    match v {
        SmokeVerdict::Pass { reply } => {
            println!("PASS: full turn completed over the gateway in {elapsed:?}");
            println!("REPLY: {reply}");
        }
        SmokeVerdict::Fail { reason, .. } => {
            eprintln!("[live_turn] FAIL: {reason}");
        }
    }
}

fn main() {
    let port: u16 = std::env::var("GW_PORT")
        .unwrap_or_else(|_| "9119".into())
        .parse()
        .unwrap();
    // GW_TOKEN is optional: prefer it if explicitly set & non-empty, else scrape
    // the live token off the dashboard so the gate runs headless.
    let token = match std::env::var("GW_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            let dash = format!("http://127.0.0.1:{port}");
            eprintln!("[live_turn] GW_TOKEN unset; scraping live token from {dash}/ ...");
            match scrape_dashboard_token(&dash) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[live_turn] TOKEN SCRAPE FAILED: {e}");
                    eprintln!("[live_turn] (is the Hermes dashboard up on :{port}? set GW_TOKEN to override)");
                    std::process::exit(9);
                }
            }
        }
    };
    let url = gateway_url("127.0.0.1", port, &token);
    eprintln!("[live_turn] connecting 127.0.0.1:{port}/api/ws ...");
    let client = match GatewayClient::connect(&url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[live_turn] CONNECT FAILED: {e:?}");
            std::process::exit(10);
        }
    };
    eprintln!(
        "[live_turn] connected; state={:?}",
        client.connection_state()
    );
    let cwd = std::env::var("HOME").unwrap();
    let sess = match client.session_create(&cwd) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[live_turn] session_create FAILED: {e:?}");
            std::process::exit(11);
        }
    };
    eprintln!("[live_turn] session_id={}", sess.session_id);
    if let Err(e) = client.prompt_submit(
        &sess.session_id,
        "Reply with exactly three words: gateway is live",
    ) {
        eprintln!("[live_turn] prompt_submit FAILED: {e:?}");
        std::process::exit(12);
    }

    let start = Instant::now();
    let timeout = Duration::from_secs(120);
    let mut judge = SmokeJudge::default();
    // The loop body is the whole decision: recv, decode, and hand the event to
    // the judge; a verdict exits with its code. (S4(b) RVS4-FIX1a B-L1: a
    // gateway `error` event and a non-success `message.complete` status both
    // fail the smoke — previously the error was only eprintln'd and the loop
    // fell through to the turn-end check, which a clean conjunction could still
    // satisfy and print PASS/exit 0 for a broken turn.)
    loop {
        if start.elapsed() > timeout {
            eprintln!(
                "[live_turn] TIMEOUT 120s (reply so far: {})",
                judge.reply.trim()
            );
            std::process::exit(2);
        }
        let Some(line) = client.recv_event(Duration::from_secs(15)) else {
            continue;
        };
        let kind = decode_event(&line);
        if let Some(v) = judge.observe(&kind) {
            report(&v, start.elapsed());
            std::process::exit(v.exit_code());
        }
    }
}

#[cfg(test)]
mod tests {
    //! The B-L1 contract, unit-driven: a gateway `error` event and a
    //! non-success `message.complete` status must each turn the smoke FAIL
    //! (exit 13 / exit 14) rather than PASS — the old loop fell through to the
    //! turn-end check on both, and a clean message.complete + running:false
    //! conjunction after an error printed PASS and exited 0. The judge owns the
    //! decision; `main` calls [`observe`] verbatim.
    use super::*;
    use awili_gateway::GatewayEvent;
    use serde_json::{json, Value};

    fn ev(event_type: &str, payload: Value) -> GatewayEvent {
        GatewayEvent {
            event_type: event_type.to_string(),
            session_id: Some("s1".to_string()),
            payload: Some(payload),
        }
    }

    /// The judge's outcome at one event position in [`judge_per_event`]:
    /// undecided so far, decided with `v` at this position, or already locked
    /// at `v` by an earlier position.
    #[derive(Debug)]
    enum JudgeStep {
        Undecided,
        Decided(SmokeVerdict),
        Locked(SmokeVerdict),
    }

    /// Drive the judge event by event and record the judge's outcome at every
    /// position: `Undecided` (observe returned None), `Decided(v)` (observe
    /// returned that verdict), or `Locked(v)` (a verdict was already decided at
    /// an earlier position and observe returned None here). Proves WHERE a
    /// verdict was decided — the final verdict alone cannot.
    fn judge_per_event(events: &[GatewayEvent]) -> Vec<JudgeStep> {
        let mut judge = SmokeJudge::default();
        let mut out = Vec::new();
        let mut decided: Option<SmokeVerdict> = None;
        for e in events {
            let step = match judge.observe(&decode_event(e)) {
                Some(v) => {
                    decided = Some(v.clone());
                    JudgeStep::Decided(v)
                }
                None => match decided.as_ref().cloned() {
                    Some(v) => JudgeStep::Locked(v),
                    None => JudgeStep::Undecided,
                },
            };
            out.push(step);
        }
        out
    }

    /// RC2-SVC4 (F0708): a dashboard that accepts the connection and never
    /// answers fails the token scrape within the bound instead of hanging.
    #[test]
    fn rc2svc4_a_silent_dashboard_fails_the_scrape_within_the_bound() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            while let Ok((s, _)) = listener.accept() {
                held.push(s);
            }
        });
        let start = Instant::now();
        let res =
            scrape_dashboard_token_within(&format!("http://{addr}"), Duration::from_millis(300));
        assert!(res.is_err(), "a silent dashboard must fail: {res:?}");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn gateway_error_never_passes_live_smoke() {
        // (a) An `error` event from the wire decodes to a terminal Error kind:
        // the smoke's match arm fires and exits nonzero before any PASS can be
        // printed — even if a terminal-looking event follows it.
        let err = decode_event(&ev("error", json!({ "message": "backend exploded" })));
        assert_eq!(
            err,
            GatewayEventKind::Error {
                message: Some("backend exploded".to_string())
            }
        );
        let mut st = TurnState::new();
        assert!(is_turn_end(&err, &mut st)); // terminal — but the smoke exits 13 first
        assert!(st.ended()); // the error event left the state terminally ended
                             // A late `message.complete` after the error can't resurrect a PASS.
        let complete = decode_event(&ev(
            "message.complete",
            json!({ "text": "late", "status": "complete" }),
        ));
        assert!(!is_turn_end(&complete, &mut st));

        // (b) The clean path is untouched: no error event, and the turn ends
        // exactly once on the message.complete + running:false conjunction —
        // this is the only route to the smoke's PASS/exit-0.
        let mut ok = TurnState::new();
        let start = decode_event(&ev("message.start", json!({})));
        assert!(!is_turn_end(&start, &mut ok));
        let done = decode_event(&ev(
            "message.complete",
            json!({ "text": "gateway is live", "status": "complete" }),
        ));
        assert!(!is_turn_end(&done, &mut ok)); // complete alone: not yet
        let idle = decode_event(&ev("session.info", json!({ "running": false })));
        assert!(is_turn_end(&idle, &mut ok)); // both signals -> end (PASS)
        assert!(ok.ended());

        // (c) Judge: a gateway error event decides the turn FAIL (exit 13) at
        // the error, and the decision is final — no later event (not even a
        // clean turn end) can turn it into a PASS.
        let err = decode_event(&ev("error", json!({ "message": "upstream 500" })));
        let complete = decode_event(&ev(
            "message.complete",
            json!({ "text": "late", "status": "complete" }),
        ));
        let idle = decode_event(&ev("session.info", json!({ "running": false })));
        let mut judge = SmokeJudge::default();
        let first = judge.observe(&err);
        assert_eq!(
            first,
            Some(SmokeVerdict::Fail {
                code: 13,
                reason: "gateway error event: Some(\"upstream 500\")".to_string(),
            })
        );
        assert_eq!(first.as_ref().unwrap().exit_code(), 13);
        assert!(judge.observe(&complete).is_none());
        assert!(judge.observe(&idle).is_none());

        // (d) Judge: the clean path is still the only route to PASS/exit 0 —
        // the decision lands exactly on the running:false event.
        let start = decode_event(&ev("message.start", json!({})));
        let done = decode_event(&ev(
            "message.complete",
            json!({ "text": "gateway is live", "status": "complete" }),
        ));
        let idle = decode_event(&ev("session.info", json!({ "running": false })));
        let mut ok_judge = SmokeJudge::default();
        assert!(ok_judge.observe(&start).is_none());
        assert!(ok_judge.observe(&done).is_none());
        let last = ok_judge.observe(&idle);
        assert_eq!(
            last,
            Some(SmokeVerdict::Pass {
                reply: "gateway is live".to_string()
            })
        );
        assert_eq!(last.unwrap().exit_code(), 0);
    }

    #[test]
    fn smoke_failed_completion_status_exits_nonzero() {
        // Per-event proof: every event BEFORE the non-success message.complete
        // leaves the judge undecided; the message.complete with the bad status
        // itself decides Fail{code: 14} (the exit-14 path) — and nothing after
        // it (even a clean turn-end conjunction) can turn it into a Pass.
        // Checked in both stream orders (session.info running:false before or
        // after the message.complete).
        for status in [
            "error",
            "failed",
            "failure",
            "ERROR",
            "stopped",
            "cancelled",
            "interrupted",
            "weird",
        ] {
            for info_first in [false, true] {
                let start = ev("message.start", json!({}));
                let complete = ev(
                    "message.complete",
                    json!({ "text": "done-ish", "status": status }),
                );
                let idle = ev("session.info", json!({ "running": false }));
                let events = if info_first {
                    vec![start.clone(), idle.clone(), complete.clone()]
                } else {
                    vec![start.clone(), complete.clone(), idle.clone()]
                };
                let complete_idx = if info_first { 2 } else { 1 };
                let steps = judge_per_event(&events);
                assert_eq!(
                    steps.len(),
                    3,
                    "status {status:?}, info_first={info_first}: one step per event"
                );
                for (i, step) in steps.iter().enumerate() {
                    if i < complete_idx {
                        assert!(
                            matches!(step, JudgeStep::Undecided),
                            "status {status:?}, info_first={info_first}: event {i} must be undecided, got {step:?}"
                        );
                    } else if i == complete_idx {
                        match step {
                            JudgeStep::Decided(v) => {
                                assert!(
                                    matches!(v, SmokeVerdict::Fail { code: 14, .. }),
                                    "status {status:?} must decide Fail{{code:14}} at message.complete, got {v:?}"
                                );
                                assert_eq!(v.exit_code(), 14, "status {status:?} must exit 14");
                                assert!(
                                    !matches!(v, SmokeVerdict::Pass { .. }),
                                    "status {status:?} must never pass"
                                );
                            }
                            other => panic!(
                                "status {status:?}, info_first={info_first}: event {i} must DECIDE the verdict, got {other:?}"
                            ),
                        }
                    } else {
                        match step {
                            JudgeStep::Locked(v) => {
                                assert!(
                                    matches!(v, SmokeVerdict::Fail { code: 14, .. }),
                                    "status {status:?}: event {i} after decision must stay locked at Fail{{code:14}}, got {v:?}"
                                );
                            }
                            other => panic!(
                                "status {status:?}, info_first={info_first}: event {i} after decision must be Locked, got {other:?}"
                            ),
                        }
                    }
                }
                // (The decision-finality half of the contract — a later event
                // in the SAME stream cannot turn the failure into a pass — is
                // proven by the Locked arm above: the step after the
                // message.complete is Locked, not a second Decided.)
            }
        }
    }

    #[test]
    fn smoke_clean_stream_exits_zero() {
        // (i) Deltas accumulate the reply; a message.complete with no status
        // and no text keeps them (status None is a success); the turn end
        // passes with the accumulated reply, exit 0.
        let events = vec![
            ev("message.start", json!({})),
            ev("message.delta", json!({ "text": "gateway " })),
            ev("message.delta", json!({ "text": "is live" })),
            ev("message.complete", json!({})),
            ev("session.info", json!({ "running": false })),
        ];
        let steps = judge_per_event(&events);
        for (i, step) in steps.iter().enumerate() {
            if i < 4 {
                assert!(
                    matches!(step, JudgeStep::Undecided),
                    "clean stream: event {i} must be undecided, got {step:?}"
                );
            } else {
                match step {
                    JudgeStep::Decided(v) => {
                        assert_eq!(
                            *v,
                            SmokeVerdict::Pass {
                                reply: "gateway is live".to_string()
                            }
                        );
                        assert_eq!(v.exit_code(), 0);
                    }
                    other => panic!("clean stream: event 4 must DECIDE the pass, got {other:?}"),
                }
            }
        }

        // (ii) The P3g tool turn: the mid-turn tool.start clears the pending
        // completion, so the turn ends exactly once — on the LAST event — with
        // the final message's text as the reply. Per-event: every event before
        // the final turn-end is undecided; only the last event decides the pass.
        let events = vec![
            ev("message.start", json!({})),
            ev(
                "message.complete",
                json!({ "status": "complete", "text": "calling tool" }),
            ),
            ev("tool.start", json!({ "name": "bash" })),
            ev("message.start", json!({})),
            ev(
                "message.complete",
                json!({ "status": "complete", "text": "gateway is live" }),
            ),
            ev("session.info", json!({ "running": false })),
        ];
        let steps = judge_per_event(&events);
        assert_eq!(steps.len(), 6, "tool turn: one step per event");
        for (i, step) in steps.iter().enumerate() {
            if i < 5 {
                assert!(
                    matches!(step, JudgeStep::Undecided),
                    "tool turn: event {i} must be undecided, got {step:?}"
                );
            } else {
                match step {
                    JudgeStep::Decided(v) => {
                        assert_eq!(
                            *v,
                            SmokeVerdict::Pass {
                                reply: "gateway is live".to_string()
                            }
                        );
                        assert_eq!(v.exit_code(), 0);
                    }
                    other => panic!("tool turn: event 5 must DECIDE the pass, got {other:?}"),
                }
            }
        }
    }
}
