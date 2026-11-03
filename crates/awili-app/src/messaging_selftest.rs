//! Messaging-connector **self-test** — the pure, [LIVE]-free half of the "Test
//! connection" action.
//!
//! [`crate::messaging_pane`] reads a decoded snapshot and exposes
//! [`MessagingPaneState::test_target`](crate::messaging_pane::MessagingPaneState::test_target):
//! the id a "Test" button would probe, but *only* the target — it marks the call
//! itself (`RestClient::test_messaging`) as **[LIVE]** / Owner-gated, so nothing
//! records or classifies the outcome. [`crate::messaging_controls`] built the
//! *mutating* half (pending edits → ordered actions). This module builds the
//! remaining pure half: a window-free engine that tracks each connector's
//! self-test lifecycle — *untested → running → passed / failed* — and, crucially,
//! **classifies a failure** so the eventual GPUI host can render a specific,
//! honest verdict ("credentials refused" vs "couldn't reach the gateway" vs "your
//! backend is too old for this probe") instead of a bare red ✗.
//!
//! The engine performs no I/O and holds no time: the host fires the real
//! `test_messaging` call, then feeds its `Result<(), GatewayError>` back in
//! through [`SelfTestLog::record`]. Everything here is `#[test]`-covered.
//!
//! ## Two properties worth a state machine (why this isn't a bool per connector)
//!
//! 1. **A verdict goes stale when the config changes.** A green ✓ means "these
//!    credentials answered the probe". The moment the user applies a
//!    [`ConnectorDraft`](crate::messaging_controls::ConnectorDraft) — new token,
//!    toggled enable — that ✓ is meaningless. [`SelfTestLog::invalidate`] drops
//!    the connector back to [`Untested`](SelfTest::Untested) so the UI can't show
//!    a pass for a config that was never tested.
//!
//! 2. **A late response can't paint a stale verdict.** If the user changes the
//!    config (invalidating) *while* a probe is in flight, the probe's eventual
//!    result must not repaint a ✓/✗ for the config that's already gone — even in
//!    the A→B→A race where the user invalidates *and re-tests* before the first
//!    probe answers. Each [`begin`](SelfTestLog::begin) mints a monotonic
//!    [`ProbeToken`]; [`record`](SelfTestLog::record) applies a result only when
//!    its token still matches the connector's live probe, dropping it (returning
//!    `false`) otherwise. A bare "is it Running?" check can't tell the first
//!    probe's late answer from the second probe it would wrongly settle; the
//!    token can.
//!
//! ## Detail strings never carry a secret
//!
//! A [`Failed`](SelfTest::Failed) outcome keeps a short `detail` for a tooltip,
//! derived by [`safe_detail`] — a fixed, secret-free label per [`GatewayError`]
//! variant (plus the body-free HTTP status). It deliberately does **not** store
//! `err.to_string()`: `Connect`/`Transport`/`Rpc`/`Decode` displays can echo an
//! arbitrary backend/transport string (a URL with a `?ticket=` query param, a
//! parse fragment), and only `Http`'s own `Display` is guaranteed body-free.
#![allow(dead_code)]

use std::collections::BTreeMap;

use awili_gateway::GatewayError;

/// Why a self-test probe failed, classified from the [`GatewayError`] the live
/// `test_messaging` call returned. The host maps each kind to an icon, colour and
/// a short line of copy; the distinction is the whole point of this engine —
/// "your token is wrong" and "the gateway is down" are different problems the
/// user fixes in different places, and a bare ✗ hides which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelfTestFailure {
    /// The backend ran the probe and the connector/credentials were **refused** —
    /// a 4xx (other than the "verb too new" codes) from `POST …/test`. The user's
    /// config is the problem; point them at the credential fields.
    Rejected,
    /// The backend itself **errored** running the probe — a 5xx (other than a
    /// `501 Not Implemented`). Not the user's fault; a retry may succeed.
    ServerError,
    /// The request **never reached a live backend** — a transport/connect/timeout/
    /// closed error. The gateway is down or unreachable, not the connector.
    Unreachable,
    /// The running backend **doesn't implement the probe** yet — the verb is newer
    /// than the backend (`404`/`405`/`501`, or an "unknown method" RPC frame). The
    /// fix is to update the backend, not the connector.
    Unsupported,
    /// The caller's **session is dead** and a full reauth is required before any
    /// probe can run (a single-use ticket could not be minted).
    ReauthRequired,
    /// Anything else — an unparseable body, or an RPC error frame that doesn't
    /// classify. Shown as a generic failure with the raw detail.
    Other,
}

/// One connector's self-test lifecycle. Absence of a connector from a
/// [`SelfTestLog`] is equivalent to [`Untested`](SelfTest::Untested) — the third
/// state (never run) is modelled by *not being running, passed or failed*.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelfTest {
    /// Never probed, or invalidated by a config change since the last probe.
    Untested,
    /// A `test_messaging` call is in flight for this connector.
    Running,
    /// The probe answered `2xx` — the connector's credentials work.
    Passed,
    /// The probe failed. `kind` drives the UI treatment; `detail` is a short,
    /// **secret-free** tooltip label from [`safe_detail`] — never the raw
    /// `GatewayError` display, which for some variants can echo a backend string
    /// (see the module docs).
    Failed {
        kind: SelfTestFailure,
        detail: String,
    },
}

impl SelfTest {
    /// True while a probe is in flight — the host shows a spinner.
    pub fn is_running(&self) -> bool {
        matches!(self, SelfTest::Running)
    }

    /// True when the last probe passed and hasn't been invalidated.
    pub fn is_passed(&self) -> bool {
        matches!(self, SelfTest::Passed)
    }

    /// The failure classification, if the last probe failed.
    pub fn failure(&self) -> Option<SelfTestFailure> {
        match self {
            SelfTest::Failed { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

/// Classify a live `test_messaging` error into a [`SelfTestFailure`]. Kept private
/// and driven only through [`SelfTestLog::record`]; pinned by the `classifies_*`
/// tests.
///
/// The `404`/`405`/`501` special-case is deliberate: the codebase treats a `404`
/// from a REST *verb* path as "the endpoint is newer than the running backend"
/// (see [`GatewayError::Http`] docs), so those map to
/// [`Unsupported`](SelfTestFailure::Unsupported) — "update your backend" — rather
/// than [`Rejected`](SelfTestFailure::Rejected) — "fix your credentials".
fn classify(err: &GatewayError) -> SelfTestFailure {
    // A dead session and a too-new verb are about the deployment, not the 4xx/5xx
    // band, so they take precedence over the raw status.
    if err.is_reauth_required() {
        return SelfTestFailure::ReauthRequired;
    }
    if err.is_unknown_method() {
        return SelfTestFailure::Unsupported;
    }
    match err {
        GatewayError::Http { status, .. } => match status {
            404 | 405 | 501 => SelfTestFailure::Unsupported,
            400..=499 => SelfTestFailure::Rejected,
            500..=599 => SelfTestFailure::ServerError,
            // A non-error status on the error path — treat as unclassified.
            _ => SelfTestFailure::Other,
        },
        GatewayError::Connect(_)
        | GatewayError::NotConnected
        | GatewayError::Closed
        | GatewayError::Transport(_)
        | GatewayError::RequestTimeout { .. } => SelfTestFailure::Unreachable,
        GatewayError::ReauthRequired => SelfTestFailure::ReauthRequired, // handled above; exhaustive
        GatewayError::Decode(_) | GatewayError::Rpc { .. } => SelfTestFailure::Other,
    }
}

/// A short, **secret-free** tooltip label for a failed probe. Never
/// `err.to_string()`: the `Connect`/`Transport`/`Rpc`/`Decode` displays can carry
/// arbitrary backend/transport text (a URL with a `?ticket=` param, an echoed
/// request fragment), and only `Http`'s own `Display` is body-free. So each
/// variant lowers to a fixed phrase, with the safe scalars (HTTP status, RPC
/// numeric code, timeout method verb) folded in for a little more signal.
fn safe_detail(err: &GatewayError) -> String {
    match err {
        // Http's Display is "gateway HTTP {status}" — body-free, safe to surface.
        GatewayError::Http { status, .. } => format!("gateway HTTP {status}"),
        // `method` is a caller-supplied `String`; keep to a fixed label so no
        // variant surfaces free text (no-free-text guarantee).
        GatewayError::RequestTimeout { .. } => "request timed out".to_string(),
        GatewayError::Connect(_) => "connection failed".to_string(),
        GatewayError::Transport(_) => "transport error".to_string(),
        GatewayError::NotConnected => "gateway not connected".to_string(),
        GatewayError::Closed => "gateway connection closed".to_string(),
        // The numeric code is safe; the free-text `message` is not, so it's dropped.
        GatewayError::Rpc { code: Some(c), .. } => format!("gateway RPC error (code {c})"),
        GatewayError::Rpc { code: None, .. } => "gateway RPC error".to_string(),
        GatewayError::Decode(_) => "response did not parse".to_string(),
        GatewayError::ReauthRequired => "reauthentication required".to_string(),
    }
}

/// A one-shot handle for a single [`SelfTestLog::begin`] dispatch. The host holds
/// it across the async `test_messaging` call and hands it back to
/// [`SelfTestLog::record`]; a token from a superseded probe no longer matches the
/// connector's live generation, so its late result is dropped (module docs,
/// property 2). Opaque and monotonic — the host never constructs one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeToken(u64);

/// The live outcome for one connector: its state plus the generation of the probe
/// that produced it, so a stale [`record`](SelfTestLog::record) can be told from
/// the live one.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Slot {
    gen: u64,
    state: SelfTest,
}

/// The self-test outcomes for every connector the pane has probed, keyed by id.
/// Purely models the *untested → running → passed / failed* lifecycle plus the
/// two staleness properties (see the module docs). Time-free and I/O-free.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelfTestLog {
    by_id: BTreeMap<String, Slot>,
    /// Monotonic source of [`ProbeToken`]s. Never reset — a fresh generation for
    /// every `begin` across all connectors guarantees a superseded probe's token
    /// can never collide with a later one.
    next_gen: u64,
}

impl SelfTestLog {
    /// An empty log — every connector reads as [`Untested`](SelfTest::Untested).
    pub fn new() -> SelfTestLog {
        SelfTestLog::default()
    }

    /// The current self-test state for `id`. An id never probed (or one dropped by
    /// [`invalidate`](Self::invalidate)) reads as [`Untested`](SelfTest::Untested).
    pub fn state(&self, id: &str) -> SelfTest {
        self.by_id
            .get(id)
            .map(|s| s.state.clone())
            .unwrap_or(SelfTest::Untested)
    }

    /// Mark a probe as **in flight** for `id` and mint the [`ProbeToken`] the host
    /// carries across the async `test_messaging` call. Call this the instant the
    /// call is dispatched: the UI shows a spinner and [`record`](Self::record) has
    /// a live generation to match against. Replaces any prior outcome — re-testing
    /// a passed connector shows the spinner again — and supersedes any earlier
    /// in-flight probe for the same id, whose token no longer matches.
    pub fn begin(&mut self, id: impl Into<String>) -> ProbeToken {
        let gen = self.next_gen;
        self.next_gen += 1;
        self.by_id.insert(
            id.into(),
            Slot {
                gen,
                state: SelfTest::Running,
            },
        );
        ProbeToken(gen)
    }

    /// Feed back the live `test_messaging` result for the probe `token` identifies,
    /// transitioning its [`Running`](SelfTest::Running) slot to
    /// [`Passed`](SelfTest::Passed) or a classified [`Failed`](SelfTest::Failed).
    ///
    /// Returns `true` when the result was applied. Returns `false` — and changes
    /// nothing — when `token` is **not the connector's live probe**: the slot is no
    /// longer running (never begun, invalidated, or already settled), or a newer
    /// `begin` has superseded it (the A→B→A race). A late response must never
    /// repaint a verdict for a config or probe that's already gone.
    pub fn record(
        &mut self,
        id: &str,
        token: ProbeToken,
        result: Result<(), GatewayError>,
    ) -> bool {
        match self.by_id.get(id) {
            // Live iff still running AND this is the generation that started it.
            Some(Slot {
                gen,
                state: SelfTest::Running,
            }) if *gen == token.0 => {}
            _ => return false,
        }
        let state = match result {
            Ok(()) => SelfTest::Passed,
            Err(err) => SelfTest::Failed {
                kind: classify(&err),
                detail: safe_detail(&err),
            },
        };
        // Keep the same generation: the settled outcome belongs to this probe, so a
        // duplicate late `record` with the same token still finds a non-Running slot
        // and is dropped.
        self.by_id.insert(
            id.to_string(),
            Slot {
                gen: token.0,
                state,
            },
        );
        true
    }

    /// Invalidate `id`'s outcome — return it to [`Untested`](SelfTest::Untested).
    /// The host calls this after applying a
    /// [`ConnectorDraft`](crate::messaging_controls::ConnectorDraft) for the
    /// connector: a new token or a toggled enable makes any prior ✓/✗ stale, and
    /// an in-flight probe's result is then dropped by [`record`](Self::record)
    /// (its token no longer matches a live slot).
    pub fn invalidate(&mut self, id: &str) {
        // Remove rather than insert Untested: absence *is* Untested, and this keeps
        // the map to only connectors with a live/settled outcome.
        self.by_id.remove(id);
    }

    /// Forget every outcome — e.g. after a fresh snapshot fetch replaces the
    /// connector set. Every connector reads as [`Untested`](SelfTest::Untested).
    /// Generations keep advancing, so tokens from before a clear stay stale.
    pub fn clear(&mut self) {
        self.by_id.clear();
    }

    /// How many connectors currently have a probe in flight (drives a header
    /// "testing…" affordance).
    pub fn running_count(&self) -> usize {
        self.by_id.values().filter(|s| s.state.is_running()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Probe `id` to completion with `result` in one go, asserting it applied.
    fn probe(log: &mut SelfTestLog, id: &str, result: Result<(), GatewayError>) {
        let tok = log.begin(id);
        assert!(log.record(id, tok, result), "one-shot probe should apply");
    }

    // --- classification (the reason a bare ✗ is not enough) ----------------

    #[test]
    fn classifies_4xx_as_rejected_credentials() {
        let mut log = SelfTestLog::new();
        probe(
            &mut log,
            "smtp",
            Err(GatewayError::Http {
                status: 401,
                body: String::new(),
            }),
        );
        assert_eq!(log.state("smtp").failure(), Some(SelfTestFailure::Rejected));
    }

    #[test]
    fn classifies_5xx_as_server_error() {
        let mut log = SelfTestLog::new();
        probe(
            &mut log,
            "tg",
            Err(GatewayError::Http {
                status: 502,
                body: String::new(),
            }),
        );
        assert_eq!(
            log.state("tg").failure(),
            Some(SelfTestFailure::ServerError)
        );
    }

    #[test]
    fn classifies_verb_too_new_as_unsupported() {
        // A 404/405/501 on a verb path, and an "unknown method" RPC frame, all mean
        // "update the backend", not "fix the connector".
        for status in [404u16, 405, 501] {
            let mut log = SelfTestLog::new();
            probe(
                &mut log,
                "x",
                Err(GatewayError::Http {
                    status,
                    body: String::new(),
                }),
            );
            assert_eq!(
                log.state("x").failure(),
                Some(SelfTestFailure::Unsupported),
                "HTTP {status} should be Unsupported"
            );
        }
        let mut log = SelfTestLog::new();
        probe(
            &mut log,
            "x",
            Err(GatewayError::Rpc {
                code: Some(-32601),
                message: "method not found".into(),
            }),
        );
        assert_eq!(log.state("x").failure(), Some(SelfTestFailure::Unsupported));
    }

    #[test]
    fn classifies_transport_family_as_unreachable() {
        let cases = [
            GatewayError::Connect("refused".into()),
            GatewayError::NotConnected,
            GatewayError::Closed,
            GatewayError::Transport("reset".into()),
            GatewayError::RequestTimeout {
                method: "test".into(),
            },
        ];
        for err in cases {
            let mut log = SelfTestLog::new();
            probe(&mut log, "g", Err(err));
            assert_eq!(
                log.state("g").failure(),
                Some(SelfTestFailure::Unreachable),
                "transport-family error should be Unreachable"
            );
        }
    }

    #[test]
    fn classifies_reauth_required() {
        let mut log = SelfTestLog::new();
        probe(&mut log, "g", Err(GatewayError::ReauthRequired));
        assert_eq!(
            log.state("g").failure(),
            Some(SelfTestFailure::ReauthRequired)
        );
    }

    #[test]
    fn classifies_decode_as_other() {
        let mut log = SelfTestLog::new();
        probe(&mut log, "g", Err(GatewayError::Decode("bad json".into())));
        assert_eq!(log.state("g").failure(), Some(SelfTestFailure::Other));
    }

    // --- detail strings never carry a secret ------------------------------

    #[test]
    fn http_detail_names_status_but_never_the_body() {
        let mut log = SelfTestLog::new();
        probe(
            &mut log,
            "g",
            Err(GatewayError::Http {
                status: 403,
                body: "secret-token=abc".into(),
            }),
        );
        match log.state("g") {
            SelfTest::Failed { detail, .. } => {
                assert!(
                    detail.contains("403"),
                    "detail {detail:?} should name the status"
                );
                assert!(
                    !detail.contains("secret-token"),
                    "detail must not echo the body"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn free_text_variants_are_replaced_not_echoed() {
        // Connect/Transport/Rpc/Decode Displays can carry an arbitrary backend or
        // transport string (a URL with a ?ticket= param, an echoed fragment). The
        // detail must be a fixed label — the raw payload must never appear.
        let leaky = "https://host/ws?ticket=SECRET-TICKET-XYZ";
        let cases = [
            GatewayError::Connect(leaky.into()),
            GatewayError::Transport(leaky.into()),
            GatewayError::Rpc {
                code: Some(7),
                message: leaky.into(),
            },
            GatewayError::Decode(leaky.into()),
            GatewayError::RequestTimeout {
                method: leaky.into(),
            },
        ];
        for err in cases {
            let mut log = SelfTestLog::new();
            probe(&mut log, "g", Err(err));
            match log.state("g") {
                SelfTest::Failed { detail, .. } => assert!(
                    !detail.contains("SECRET-TICKET-XYZ"),
                    "detail {detail:?} leaked the raw error payload"
                ),
                other => panic!("expected Failed, got {other:?}"),
            }
        }
    }

    // --- lifecycle ---------------------------------------------------------

    #[test]
    fn unknown_id_is_untested() {
        let log = SelfTestLog::new();
        assert_eq!(log.state("never"), SelfTest::Untested);
        assert_eq!(log.running_count(), 0);
    }

    #[test]
    fn begin_then_pass() {
        let mut log = SelfTestLog::new();
        let tok = log.begin("tg");
        assert_eq!(log.state("tg"), SelfTest::Running);
        assert!(log.state("tg").is_running());
        assert_eq!(log.running_count(), 1);
        assert!(log.record("tg", tok, Ok(())));
        assert_eq!(log.state("tg"), SelfTest::Passed);
        assert!(log.state("tg").is_passed());
        assert_eq!(log.running_count(), 0);
    }

    // --- staleness property 1: a config change invalidates a verdict -------

    #[test]
    fn invalidate_drops_a_passed_verdict_to_untested() {
        let mut log = SelfTestLog::new();
        probe(&mut log, "tg", Ok(()));
        assert!(log.state("tg").is_passed());
        // User edits the token and saves → the ✓ is meaningless now.
        log.invalidate("tg");
        assert_eq!(log.state("tg"), SelfTest::Untested);
    }

    // --- staleness property 2: a late result can't paint a stale verdict ---

    #[test]
    fn record_with_no_live_probe_is_dropped() {
        let mut log = SelfTestLog::new();
        let tok = log.begin("tg");
        // Settle it, then a duplicate record with the same token must be dropped —
        // the slot is no longer Running.
        assert!(log.record("tg", tok, Ok(())));
        assert!(!log.record("tg", tok, Err(GatewayError::Closed)));
        assert_eq!(
            log.state("tg"),
            SelfTest::Passed,
            "duplicate late record must not overwrite"
        );
    }

    #[test]
    fn late_result_after_invalidation_is_discarded() {
        let mut log = SelfTestLog::new();
        let tok = log.begin("tg"); // probe dispatched
        log.invalidate("tg"); // user changes config mid-flight
                              // The probe finally answers — but for a config that's already gone.
        assert!(
            !log.record("tg", tok, Ok(())),
            "stale result must be dropped"
        );
        assert_eq!(
            log.state("tg"),
            SelfTest::Untested,
            "a discarded stale pass must not show a ✓"
        );
    }

    #[test]
    fn aba_race_first_probe_cannot_settle_the_second() {
        // The race a bare Running-check misses: begin(A) → invalidate → begin(B),
        // then A's late result arrives. B's slot IS Running, so a token-blind guard
        // would let A settle it. The generation token rejects A.
        let mut log = SelfTestLog::new();
        let a = log.begin("tg"); // first probe (old creds)
        log.invalidate("tg"); // user swaps the token
        let b = log.begin("tg"); // second probe (new creds), now Running
                                 // A finally answers — with a PASS for the old, wrong creds.
        assert!(
            !log.record("tg", a, Ok(())),
            "the superseded first probe must not settle the live slot"
        );
        assert_eq!(
            log.state("tg"),
            SelfTest::Running,
            "B's spinner must remain"
        );
        // B answers correctly and lands.
        assert!(log.record(
            "tg",
            b,
            Err(GatewayError::Http {
                status: 401,
                body: String::new()
            })
        ));
        assert_eq!(log.state("tg").failure(), Some(SelfTestFailure::Rejected));
    }

    #[test]
    fn re_test_replaces_a_prior_outcome_with_the_spinner() {
        let mut log = SelfTestLog::new();
        probe(
            &mut log,
            "tg",
            Err(GatewayError::Http {
                status: 401,
                body: String::new(),
            }),
        );
        assert_eq!(log.state("tg").failure(), Some(SelfTestFailure::Rejected));
        // Fix creds, test again: begin() shows the spinner over the old ✗.
        let tok = log.begin("tg");
        assert_eq!(log.state("tg"), SelfTest::Running);
        assert!(log.record("tg", tok, Ok(())));
        assert_eq!(log.state("tg"), SelfTest::Passed);
    }

    #[test]
    fn independent_connectors_track_independently() {
        let mut log = SelfTestLog::new();
        let a = log.begin("a");
        let b = log.begin("b");
        assert_eq!(log.running_count(), 2);
        log.record("a", a, Ok(()));
        log.record("b", b, Err(GatewayError::Connect("down".into())));
        assert!(log.state("a").is_passed());
        assert_eq!(log.state("b").failure(), Some(SelfTestFailure::Unreachable));
        assert_eq!(log.running_count(), 0);
        // Invalidating one leaves the other untouched.
        log.invalidate("a");
        assert_eq!(log.state("a"), SelfTest::Untested);
        assert_eq!(log.state("b").failure(), Some(SelfTestFailure::Unreachable));
    }

    #[test]
    fn clear_forgets_everything_and_stales_prior_tokens() {
        let mut log = SelfTestLog::new();
        let a = log.begin("a");
        log.record("a", a, Ok(()));
        let b = log.begin("b"); // still in flight
        log.clear();
        assert_eq!(log.state("a"), SelfTest::Untested);
        assert_eq!(log.state("b"), SelfTest::Untested);
        assert_eq!(log.running_count(), 0);
        // b's pre-clear token is now stale — its late result is dropped.
        assert!(!log.record("b", b, Ok(())));
        assert_eq!(log.state("b"), SelfTest::Untested);
    }
}
