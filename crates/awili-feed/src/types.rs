//! The feed wire contract: every JSON body the service speaks, as serde types.
//!
//! The service (`services/awili-feed-service`) depends on this module for its
//! request/response types, so a contract change is a single edit visible to both
//! sides — the client cannot regress to an invented route shape unnoticed.

use serde::{Deserialize, Serialize};

/// Event severity — the council's flat, voice-friendly taxonomy (synthesis,
/// unanimous): no hierarchy, no numeric scores. Declaration order gives the
/// derived `Ord` the meaning "how loudly this wants a human":
/// `routine < attention < critical`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Ambient record: completions, self-healed failures, state transitions.
    Routine,
    /// Needs a human eventually — surfaces in "Needs You", never phones.
    Attention,
    /// Needs a human now — the only class (plus needs-input) that may escalate.
    Critical,
}

impl Severity {
    /// The wire/storage string. Stable: sqlite rows and JSON share it.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Routine => "routine",
            Severity::Attention => "attention",
            Severity::Critical => "critical",
        }
    }

    /// Parse a stored/wire string. `None` for anything unknown — a corrupt or
    /// future severity must surface, not coerce to a quieter class.
    pub fn parse(s: &str) -> Option<Severity> {
        match s {
            "routine" => Some(Severity::Routine),
            "attention" => Some(Severity::Attention),
            "critical" => Some(Severity::Critical),
            _ => None,
        }
    }
}

/// A new event as a producer posts it (`POST /events`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewEvent {
    /// Producer-generated idempotency key: re-posting the same `event_id` (a
    /// retry after a dropped response) is acknowledged, never duplicated.
    /// Grammar: 1..=128 chars of `[A-Za-z0-9._-]` — the service rejects
    /// anything else at post time (422), because the ack route addresses an
    /// event by URL path segment. [`generate_event_id`] emits within this.
    pub event_id: String,
    /// Which daemon/agent produced this (`ccd-hosta`, `awili`, …).
    pub source: String,
    pub severity: Severity,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// The underlying condition, for storm grouping ("4 jobs blocked: gitea auth
    /// expired"). Absent = the event is its own group.
    #[serde(default)]
    pub dedup_key: Option<String>,
    /// Unix seconds. Absent = the server stamps receipt time.
    #[serde(default)]
    pub created_ts: Option<i64>,
}

/// What `POST /events` returns.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostReceipt {
    /// The server-assigned, monotonically increasing read cursor position.
    pub seq: i64,
    pub event_id: String,
    /// `true` when this `event_id` had already been appended — the original row
    /// (and its original `seq`) stands; nothing was written.
    pub duplicate: bool,
}

/// A stored event, as the service returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    pub event_id: String,
    pub source: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub dedup_key: Option<String>,
    pub created_ts: i64,
    /// Unix seconds of the binary ack (synthesis: no read-vs-seen distinction).
    /// Acked events are hidden from "Needs You", never deleted.
    pub acked_ts: Option<i64>,
}

/// One page of `GET /events?after=<seq>` — oldest first.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventsPage {
    pub events: Vec<Event>,
    /// The cursor to pass as `after` next time: the last seq in `events`, or the
    /// `after` the caller sent when the page is empty.
    pub cursor: i64,
}

/// What `POST /events/{event_id}/ack` returns.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AckReceipt {
    pub event_id: String,
    /// Unix seconds the event was acked — the original stamp when the ack was
    /// already in place (acking twice is a no-op, not an error).
    pub acked_ts: i64,
}

/// One explicit acknowledgement fact from the service's append-only ack log
/// (S5(b) FIX1). `ack_seq` is the ack cursor; it is independent of event `seq`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckFact {
    pub ack_seq: i64,
    pub event_id: String,
    pub acked_ts: i64,
}

/// One page of `GET /acks?after=<ack_seq>` — oldest first. `cursor` is the last
/// `ack_seq` in `acks`, or the caller's `after` when the page is empty.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcksPage {
    pub acks: Vec<AckFact>,
    pub cursor: i64,
}

/// One "Needs You" group: every unacked attention/critical event sharing a
/// `dedup_key` collapses to one row (storm control), keyless events stand alone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NeedsGroup {
    pub dedup_key: Option<String>,
    /// Unacked events in this group.
    pub count: usize,
    /// The worst severity present in the group.
    pub severity: Severity,
    /// The newest event in the group — what a one-line row displays.
    pub latest: Event,
    /// Every unacked `event_id` in the group, for bulk ack.
    pub event_ids: Vec<String>,
}

/// `GET /needs-you`: the primary surface — unresolved items, one action each.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NeedsYou {
    /// Newest-first by the group's latest event.
    pub groups: Vec<NeedsGroup>,
    /// Total unacked attention+critical EVENTS (not groups) — the badge number.
    pub total: usize,
}

/// `POST /escalation/lease` request: ask to hold (or renew) the single
/// escalation lease for `ttl_seconds`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaseRequest {
    /// Who is asking (e.g. `hermes-neo`) — observability only; identity is the
    /// fencing `token`, never the name (two processes configured with the same
    /// name must NOT both pass as the holder).
    pub holder: String,
    pub ttl_seconds: i64,
    /// The fencing token from a previous grant. `None` asks for a fresh
    /// acquisition (refused while another hold is live); `Some` asks to renew
    /// the hold that token belongs to.
    #[serde(default)]
    pub token: Option<String>,
}

/// The lease outcome — returned for both grants (200) and refusals (409), so a
/// refused caller learns who actually holds it and until when.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeaseState {
    pub granted: bool,
    /// The CURRENT holder after this call (the caller iff `granted`).
    pub holder: String,
    /// Unix seconds the current hold expires.
    pub expires_ts: i64,
    /// The opaque fencing token, present iff `granted`. It is REQUIRED to renew
    /// and to claim events for escalation; a refusal never reveals the live
    /// holder's token. A fresh acquisition (including taking over an expired
    /// hold) mints a NEW token, so a holder paused across its TTL wakes up
    /// fenced out: its old token can no longer renew or claim.
    #[serde(default)]
    pub token: Option<String>,
}

/// `POST /escalation/claim` request: atomically claim ONE event for escalation
/// under a live lease token. The claim — not the lease alone — is what makes
/// double-texting structurally impossible: the side effect (the send) may only
/// happen after the claim is granted, and an event is claimable exactly once
/// per lifetime (a later lease generation is refused, because the earlier
/// claimant may already have sent).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub event_id: String,
    /// Who is claiming — observability only, like [`LeaseRequest::holder`].
    pub holder: String,
    /// The fencing token of the live lease.
    pub token: String,
}

/// The claim outcome — 200 when the caller holds the claim (first time or an
/// idempotent retry under the same token), 409 otherwise.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaimState {
    /// `true` iff the CALLER holds the claim after this call.
    pub claimed: bool,
    pub event_id: String,
    /// Who holds the claim, if anyone does.
    #[serde(default)]
    pub holder: Option<String>,
    /// Unix seconds the claim was recorded, if it exists.
    #[serde(default)]
    pub claimed_ts: Option<i64>,
}

/// Generate a producer-side `event_id`: URL-safe, unique across processes and
/// retries (wall-clock nanos + pid + an in-process counter). Cheap and
/// dependency-free; collisions would need two events in the same nanosecond in
/// the same pid at the same counter value.
pub fn generate_event_id(source: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Sanitize the source into the id (it is also sent as its own field).
    let safe: String = source
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{safe}-{nanos:x}-{:x}-{n:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_by_loudness() {
        assert!(Severity::Routine < Severity::Attention);
        assert!(Severity::Attention < Severity::Critical);
    }

    #[test]
    fn severity_wire_strings_roundtrip() {
        for s in [Severity::Routine, Severity::Attention, Severity::Critical] {
            assert_eq!(Severity::parse(s.as_str()), Some(s));
            // JSON serialization matches the storage string.
            assert_eq!(
                serde_json::to_string(&s).unwrap(),
                format!("\"{}\"", s.as_str())
            );
        }
        assert_eq!(Severity::parse("urgent"), None);
        assert_eq!(Severity::parse(""), None);
    }

    #[test]
    fn unknown_severity_fails_to_deserialize() {
        // A future/typo'd class must error loudly, not coerce.
        assert!(serde_json::from_str::<Severity>("\"panic\"").is_err());
    }

    #[test]
    fn new_event_optional_fields_default() {
        let e: NewEvent = serde_json::from_str(
            r#"{"event_id":"e1","source":"ccd","severity":"attention","title":"t"}"#,
        )
        .unwrap();
        assert_eq!(e.body, "");
        assert_eq!(e.dedup_key, None);
        assert_eq!(e.created_ts, None);
    }

    #[test]
    fn event_ids_are_unique_and_url_safe() {
        let a = generate_event_id("awili");
        let b = generate_event_id("awili");
        assert_ne!(a, b);
        let c = generate_event_id("weird source/with spaces");
        for id in [&a, &b, &c] {
            assert!(
                id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-'),
                "id not URL-safe: {id}"
            );
        }
    }
}
