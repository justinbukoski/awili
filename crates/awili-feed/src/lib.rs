//! `awili-feed` — the attention-feed wire contract, the blocking HTTP client, and
//! the local sqlite mirror (slice-4 spec, Part B; design authority
//! docs/council/r9-attention-feed/synthesis.md).
//!
//! The feed is CENTRAL-AUTHORITATIVE (Foreman ruling): the service on HOSTC owns
//! events, ack state, and the escalation lease; every consumer keeps a local
//! sqlite mirror it fills by cursor polling. The mirror is what the UI reads —
//! it answers instantly and keeps answering when HOSTC is unreachable (the panel
//! then shows the last-sync age instead of lying about freshness).
//!
//! Three deliberate echoes of the rest of the workspace:
//! - the HTTP transport is `reqwest::blocking` behind tight timeouts, called only
//!   from worker threads (the GPUI loop owns no tokio runtime);
//! - the wire types live here and the service depends on THIS crate for them, so
//!   client and server cannot drift apart silently;
//! - parsing/grouping logic is pure and unit-tested; the full client is exercised
//!   against the real service in `services/awili-feed-service/tests/contract.rs`.

mod client;
mod mirror;
mod types;

pub use client::{FeedClient, FeedError};
pub use mirror::{FeedMirror, NeedsItem, SyncOutcome};
pub use types::{
    generate_event_id, AckFact, AckReceipt, AcksPage, ClaimRequest, ClaimState, Event, EventsPage,
    LeaseRequest, LeaseState, NeedsGroup, NeedsYou, NewEvent, PostReceipt, Severity,
};
