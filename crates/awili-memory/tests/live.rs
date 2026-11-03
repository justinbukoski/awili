//! Live contract test against the real Foreman Memory API. Ignored by default —
//! it needs LAN reachability to the service — so it does not run in CI or on a
//! disconnected host. Run it explicitly when on the LAN:
//!
//! ```text
//! cargo test -p awili-memory --test live -- --ignored
//! ```
//!
//! Override the endpoint with `AWILI_MEMORY_URL` if the service moves.

use awili_memory::{ChatLogEntry, MemoryClient, MIN_DIRECTIVE_PRIORITY};

/// Default live endpoint (the Foreman Memory API on the LAN).
const DEFAULT_URL: &str = "http://10.77.68.86:8841";

fn base_url() -> String {
    std::env::var("AWILI_MEMORY_URL").unwrap_or_else(|_| DEFAULT_URL.to_string())
}

#[test]
#[ignore = "hits the live LAN memory service; run with --ignored"]
fn live_health_is_reachable() {
    let client = MemoryClient::new(base_url(), MIN_DIRECTIVE_PRIORITY);
    assert!(client.health(), "GET /health did not report success");
}

#[test]
#[ignore = "hits the live LAN memory service; run with --ignored"]
fn live_directives_inject_standing_rules() {
    let client = MemoryClient::new(base_url(), MIN_DIRECTIVE_PRIORITY);
    // The every-turn injection path: POST /v5/recall (authed with the boot token),
    // map the constraints+directives buckets, filter to the >=90 floor, sort by
    // priority. The v4 /memories/search path this used to hit was removed at the
    // v5 cutover (2026-06-19).
    assert!(
        client.has_v5_token(),
        "no v5 boot token on this host — cannot run the live recall"
    );
    let directives = client
        .directives_v5("standing directives rules", 10)
        .expect("live recall/parse failed — the contract regressed");
    assert!(
        !directives.is_empty(),
        "expected at least one standing directive"
    );
    assert!(
        directives
            .iter()
            .all(|d| d.priority >= MIN_DIRECTIVE_PRIORITY),
        "a directive below the priority-90 floor leaked through"
    );
    // Sorted high-to-low.
    assert!(directives
        .windows(2)
        .all(|w| w[0].priority >= w[1].priority));
    assert!(directives.iter().all(|d| !d.text.trim().is_empty()));
}

#[test]
#[ignore = "hits the live LAN memory service; writes test data; run with --ignored"]
fn live_chat_and_store_round_trip() {
    let client = MemoryClient::new(base_url(), MIN_DIRECTIVE_PRIORITY);
    // POST /chat — transcript line. Fire-and-forget in the app; here we assert it.
    let entry = ChatLogEntry {
        session_id: "awili-live-contract-test".to_string(),
        role: "user".to_string(),
        content: "awili live contract test transcript line — safe to ignore".to_string(),
    };
    client.chat_log(&entry).expect("POST /chat failed");
    // POST /v5/notes — store a clearly-marked, low-priority test note. Needs the
    // writer token (auto-loaded from ~/.config/foreman/v5-writer-token); the v4
    // POST /memories path this used to hit now 404s.
    assert!(
        client.has_v5_writer_token(),
        "no v5 writer token on this host — cannot run the live write"
    );
    client
        .store(
            "awili live contract test memory — safe to delete",
            "test",
            10,
        )
        .expect("POST /v5/notes failed");
}
