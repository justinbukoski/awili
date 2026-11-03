//! The blocking HTTP client for the feed service.
//!
//! Same transport posture as [`awili_memory`]'s client: `reqwest::blocking`
//! behind tight timeouts, constructed cheaply on whatever worker thread needs
//! it. An unreachable HOSTC degrades in about a second — the UI thread never
//! sees this client at all.

use std::time::Duration;

use crate::types::{
    AckReceipt, AcksPage, ClaimRequest, ClaimState, EventsPage, LeaseRequest, LeaseState, NeedsYou,
    NewEvent, PostReceipt,
};

/// Why a feed call failed.
#[derive(Debug)]
pub enum FeedError {
    /// Transport-level failure (unreachable, timeout, TLS…).
    Http(reqwest::Error),
    /// The service answered with a non-success status.
    Status(u16),
    /// The service answered 2xx but the body did not parse as the contract.
    Parse(String),
    /// A local mirror (sqlite) operation failed.
    Db(String),
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedError::Http(e) => write!(f, "feed transport: {e}"),
            FeedError::Status(s) => write!(f, "feed service answered HTTP {s}"),
            FeedError::Parse(e) => write!(f, "feed response did not parse: {e}"),
            FeedError::Db(e) => write!(f, "feed mirror: {e}"),
        }
    }
}

impl std::error::Error for FeedError {}

impl From<rusqlite::Error> for FeedError {
    fn from(e: rusqlite::Error) -> Self {
        FeedError::Db(e.to_string())
    }
}

/// Client for the awili-feed service.
pub struct FeedClient {
    base_url: String,
    /// Bearer token (`AWILI_FEED_TOKEN` for Awili; `FEED_TOKEN` service-side).
    /// `None` sends no Authorization header — the service will 401.
    token: Option<String>,
    client: reqwest::blocking::Client,
}

impl FeedClient {
    /// Construct a client. Timeouts are deliberately short — feed sync is a
    /// background nicety; nothing may stall behind an unreachable HOSTC.
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_millis(800))
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            base_url: base_url.into(),
            token,
            client,
        }
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// Attach the bearer token, if configured.
    fn auth(&self, req: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    /// Send, gate on success status, parse the JSON body as `T`.
    fn parse_success<T: serde::de::DeserializeOwned>(
        response: reqwest::blocking::Response,
    ) -> Result<T, FeedError> {
        if !response.status().is_success() {
            return Err(FeedError::Status(response.status().as_u16()));
        }
        let text = response.text().map_err(FeedError::Http)?;
        serde_json::from_str(&text).map_err(|e| FeedError::Parse(e.to_string()))
    }

    /// `true` if the service answers `GET /health` (unauthenticated liveness).
    pub fn health(&self) -> bool {
        self.client
            .get(self.url("health"))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Append an event. Idempotent on `event_id`: a retry of a post whose
    /// response was lost returns the original row's receipt with
    /// `duplicate: true`.
    pub fn post_event(&self, event: &NewEvent) -> Result<PostReceipt, FeedError> {
        let response = self
            .auth(self.client.post(self.url("events")))
            .json(event)
            .send()
            .map_err(FeedError::Http)?;
        Self::parse_success(response)
    }

    /// Cursor read: every event with `seq > after`, oldest first, at most
    /// `limit` of them. Pass the returned page's `cursor` as the next `after`.
    pub fn events_after(&self, after: i64, limit: usize) -> Result<EventsPage, FeedError> {
        let response = self
            .auth(
                self.client
                    .get(self.url("events"))
                    .query(&[("after", after.to_string()), ("limit", limit.to_string())]),
            )
            .send()
            .map_err(FeedError::Http)?;
        Self::parse_success(response)
    }

    /// Ack-fact cursor read (S5(b) FIX1): every ack with `ack_seq > after`, oldest first.
    pub fn acks_after(&self, after: i64, limit: usize) -> Result<AcksPage, FeedError> {
        let response = self
            .auth(
                self.client
                    .get(self.url("acks"))
                    .query(&[("after", after.to_string()), ("limit", limit.to_string())]),
            )
            .send()
            .map_err(FeedError::Http)?;
        Self::parse_success(response)
    }

    /// Binary ack. Idempotent — acking an already-acked event returns the
    /// original stamp. Unknown `event_id` is `FeedError::Status(404)`.
    pub fn ack(&self, event_id: &str) -> Result<AckReceipt, FeedError> {
        let response = self
            .auth(
                self.client
                    .post(self.url(&format!("events/{event_id}/ack"))),
            )
            .send()
            .map_err(FeedError::Http)?;
        Self::parse_success(response)
    }

    /// The primary surface: unacked attention+critical, grouped by `dedup_key`.
    pub fn needs_you(&self) -> Result<NeedsYou, FeedError> {
        let response = self
            .auth(self.client.get(self.url("needs-you")))
            .send()
            .map_err(FeedError::Http)?;
        Self::parse_success(response)
    }

    /// Ask for (or renew) the single escalation lease. Pass `token: None` for a
    /// fresh acquisition; renewal MUST present the fencing token of the grant
    /// being renewed — the holder name alone never proves identity. BOTH
    /// outcomes are `Ok`: a 409 refusal parses into
    /// `LeaseState { granted: false, .. }` carrying the actual holder — being
    /// refused the lease is an answer, not an error.
    pub fn lease(
        &self,
        holder: &str,
        ttl_seconds: i64,
        token: Option<&str>,
    ) -> Result<LeaseState, FeedError> {
        let request = LeaseRequest {
            holder: holder.to_string(),
            ttl_seconds,
            token: token.map(str::to_string),
        };
        let response = self
            .auth(self.client.post(self.url("escalation/lease")))
            .json(&request)
            .send()
            .map_err(FeedError::Http)?;
        Self::parse_answer(response)
    }

    /// Atomically claim one event for escalation under a live lease `token`.
    /// The send may happen ONLY after `claimed: true` — this is the per-event
    /// fence that keeps a stale holder (paused across its TTL) or a same-named
    /// twin process from double-texting. Idempotent under the same token; a
    /// claim from a different lease generation is refused (`claimed: false`),
    /// also as `Ok` — refusal is an answer. Unknown events are `Status(404)`.
    pub fn claim_escalation(
        &self,
        event_id: &str,
        holder: &str,
        token: &str,
    ) -> Result<ClaimState, FeedError> {
        let request = ClaimRequest {
            event_id: event_id.to_string(),
            holder: holder.to_string(),
            token: token.to_string(),
        };
        let response = self
            .auth(self.client.post(self.url("escalation/claim")))
            .json(&request)
            .send()
            .map_err(FeedError::Http)?;
        Self::parse_answer(response)
    }

    /// Like [`parse_success`](Self::parse_success), but 409 ("you were refused,
    /// here is who holds it") parses as a valid answer rather than an error.
    fn parse_answer<T: serde::de::DeserializeOwned>(
        response: reqwest::blocking::Response,
    ) -> Result<T, FeedError> {
        let status = response.status();
        if !status.is_success() && status.as_u16() != 409 {
            return Err(FeedError::Status(status.as_u16()));
        }
        let text = response.text().map_err(FeedError::Http)?;
        serde_json::from_str(&text).map_err(|e| FeedError::Parse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Token / http client are irrelevant to path composition; `None` avoids any
    // network. Only `url()`'s pure string normalization is under test here.
    fn client(base: &str) -> FeedClient {
        FeedClient::new(base, None)
    }

    #[test]
    fn url_joins_with_exactly_one_slash_across_all_boundary_slash_shapes() {
        // The plain case every current caller hits: clean base, no leading slash.
        assert_eq!(
            client("http://h:8899").url("events"),
            "http://h:8899/events"
        );
        // A base_url configured WITH a trailing slash (e.g. `AWILI_FEED_URL=.../`)
        // must be trimmed — otherwise the join yields `http://h:8899//events`,
        // which some routers 404, silently killing background feed sync. Guards
        // a dropped `trim_end_matches('/')`.
        assert_eq!(
            client("http://h:8899/").url("events"),
            "http://h:8899/events"
        );
        // A path arriving WITH a leading slash is trimmed too, so a future caller
        // passing `/events` still composes one slash. Guards a dropped
        // `trim_start_matches('/')`.
        assert_eq!(
            client("http://h:8899").url("/events"),
            "http://h:8899/events"
        );
        // Both sides carrying a slash still collapse to exactly one — neither
        // trim can be removed without failing at least one of these arms.
        assert_eq!(
            client("http://h:8899/").url("/events"),
            "http://h:8899/events"
        );
    }

    // A tiny DeserializeOwned target: the two parse helpers are generic over `T`,
    // and their status-gate / parse-error behavior is `T`-independent, so a
    // one-field struct exercises every arm without pulling in a real feed type.
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Answer {
        ok: bool,
    }

    // Build an in-memory `reqwest::blocking::Response` at `status` carrying `body`,
    // with no socket — reqwest 0.12's `blocking::Response: From<http::Response<_>>`.
    fn response(status: u16, body: &str) -> reqwest::blocking::Response {
        let raw = http::Response::builder()
            .status(status)
            .body(body.to_string())
            .expect("valid http::Response");
        reqwest::blocking::Response::from(raw)
    }

    #[test]
    fn parse_success_rejects_409_but_parse_answer_accepts_it() {
        // This is the ENTIRE reason the two helpers are distinct: a 409 is a
        // hard error for `parse_success` (the ordinary "gate on success" path)
        // but a VALID answer for `parse_answer` (a refused lease/claim replies
        // 409 with the current holder). Collapsing `parse_answer` back into
        // `parse_success` — an inviting "dedup" — would silently turn every
        // lease/claim refusal into an error instead of `Ok(granted:false)`,
        // so the two must be pinned against the same 409 body in one test.
        let body = r#"{"ok":false}"#;

        // parse_success: 409 is NOT is_success() => hard Status(409), body unread.
        match FeedClient::parse_success::<Answer>(response(409, body)) {
            Err(FeedError::Status(409)) => {}
            other => panic!("parse_success must reject 409 as Status(409), got {other:?}"),
        }

        // parse_answer: 409 is tolerated => the body parses through to the value.
        assert_eq!(
            FeedClient::parse_answer::<Answer>(response(409, body)).unwrap(),
            Answer { ok: false },
        );
    }

    #[test]
    fn parse_answer_still_rejects_non_409_errors() {
        // The 409 tolerance is surgical: any OTHER non-2xx (a real backend
        // failure) must still surface as Status, not be swallowed as a parseable
        // answer. Guards a `!= 409` predicate accidentally widened to `>= 400`
        // "close enough" tolerance, or dropped entirely.
        match FeedClient::parse_answer::<Answer>(response(500, r#"{"ok":true}"#)) {
            Err(FeedError::Status(500)) => {}
            other => panic!("parse_answer must reject 500 as Status(500), got {other:?}"),
        }
    }

    #[test]
    fn parse_success_maps_malformed_2xx_body_to_parse_error() {
        // A 200 that is not the expected JSON is a `Parse` error, not a `Status`
        // one and not a panic — pins the `serde_json::from_str(..).map_err(Parse)`
        // arm that a `.unwrap()` "cleanup" would turn into a crash on a
        // schema-drifted feed service.
        match FeedClient::parse_success::<Answer>(response(200, "not json at all")) {
            Err(FeedError::Parse(_)) => {}
            other => panic!("malformed 2xx body must map to Parse, got {other:?}"),
        }
    }

    #[test]
    fn url_only_touches_boundary_slashes_not_interior_segments() {
        // Only the join boundary is normalized: the `events/<id>/ack` shape real
        // callers build must pass through intact. Guards against over-eager
        // normalization (e.g. a `.replace("//", "/")` or all-slash strip) that
        // would corrupt multi-segment paths.
        // Dirty on both boundaries so the trims fire, yet the interior `/` in
        // `events/<id>/ack` survives — proving only the join boundary is touched.
        assert_eq!(
            client("http://h:8899/").url("/events/evt-1/ack"),
            "http://h:8899/events/evt-1/ack"
        );
    }
}
