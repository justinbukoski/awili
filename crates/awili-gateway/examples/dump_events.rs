//! Temporary diagnostic: connect to the live Hermes gateway, submit one prompt,
//! and dump every event (type + payload) so we can see exactly what Hermes
//! emits for a simple turn. Used to root-cause the Work-mode doubling bug.
//!
//! Run:
//!   HERMES_HOST=127.0.0.1 HERMES_PORT=9119 \
//!   HERMES_TOKEN=$(curl -fsS http://127.0.0.1:9119/ | grep -o '__HERMES_SESSION_TOKEN__="[^"]*"' | cut -d'"' -f2) \
//!   PROMPT='Reply with exactly the single word: PONG' \
//!   cargo run -p awili-gateway --example dump_events

use std::time::{Duration, Instant};

use awili_gateway::{gateway_url, GatewayClient};

fn main() {
    let host = std::env::var("HERMES_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("HERMES_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9119);
    let token = std::env::var("HERMES_TOKEN").expect("set HERMES_TOKEN");
    let prompt = std::env::var("PROMPT")
        .unwrap_or_else(|_| "Reply with exactly the single word: PONG".into());

    let ws_url = gateway_url(&host, port, &token);
    eprintln!("connecting to {host}:{port} …");
    let mut client = GatewayClient::connect(&ws_url).expect("connect");

    let session = client.session_create("/tmp").expect("session.create");
    eprintln!("session: {}", session.session_id);

    client
        .prompt_submit(&session.session_id, &prompt)
        .expect("prompt.submit");
    eprintln!("submitted prompt: {prompt:?}\n--- events ---");

    let deadline = Instant::now() + Duration::from_secs(45);
    let mut saw_complete = false;
    let mut linger = Instant::now();
    loop {
        if Instant::now() > deadline {
            eprintln!("(deadline)");
            break;
        }
        if saw_complete && Instant::now() > linger {
            break;
        }
        match client.recv_event(Duration::from_millis(200)) {
            Some(ev) => {
                let payload = ev
                    .payload
                    .as_ref()
                    .map(|p| serde_json::to_string(p).unwrap_or_default())
                    .unwrap_or_default();
                // Truncate huge payloads but keep text fields visible.
                let shown = if payload.len() > 600 {
                    format!("{}…", &payload[..600])
                } else {
                    payload
                };
                println!("EVENT type={:<20} payload={}", ev.event_type, shown);
                if ev.event_type == "message.complete" {
                    saw_complete = true;
                    // Linger 1.5s to catch any trailing events after complete.
                    linger = Instant::now() + Duration::from_millis(1500);
                }
            }
            None => {
                if !client.is_connected() {
                    eprintln!("(disconnected)");
                    break;
                }
            }
        }
    }

    let _ = client.session_close(&session.session_id);
    client.close();
    eprintln!("--- done ---");
}
