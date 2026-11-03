//! Typed v6.5 (interlock) verbs: `bootstrap`, `recall`, `remember`,
//! `archive_search` against the Foreman memory service's v6.5 lane.
//!
//! P4b: the typed adapter that keeps the three memory lanes distinct —
//! mandatory policy, canonical memory, and raw archive evidence — while
//! reusing [`crate::MemoryClient`]'s backend/token resolution and the
//! 16,000 bootstrap/recall budget floor ([`crate::V65_RECALL_TOKEN_BUDGET`]).
//!
//! Lane discipline: [`V65Bootstrap`] and [`V65Recall`] expose
//! `mandatory_policy` and `items` as separate typed fields, and
//! [`V65ArchiveResults`] is a distinct type with no conversion into the
//! canonical [`crate::Directive`]/[`crate::RecallEntry`] mappers — archive
//! evidence can never enter a canonical recall/policy collection, by
//! construction (acceptance 5).
//!
//! The transport, error model, and budget-floor parsing live in
//! [`v65::transport`]; the verbs here are thin, bounded retry loops over
//! them.

#![forbid(unsafe_code)]

pub mod transport;

pub use transport::{
    minimum_budget, ServiceCode, TokenLane, V65Error, MAX_SERVICE_MESSAGE_CHARS,
    V65_REQUEST_TIMEOUT,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{MemoryClient, V65_MAX_TOKEN_BUDGET, V65_RECALL_TOKEN_BUDGET};
use transport::{parse, post, require_token, PostOutcome};

/// The v6.5 `POST` path for the bootstrap verb (bearer-authenticated).
pub const PATH_BOOTSTRAP: &str = "v6/bootstrap";
/// The v6.5 `POST` path for the recall verb.
pub const PATH_RECALL: &str = "v6/recall";
/// The v6.5 `POST` path for the remember (write) verb.
pub const PATH_MEMORIES: &str = "v6/memories";
/// The v6.5 `POST` path for the archive search verb.
pub const PATH_ARCHIVE_SEARCH: &str = "v6/archive/search";

/// The provenance authority a `remember` write may claim — the strict
/// server-supported vocabulary. There is no `Unknown` arm, so unknown
/// strings are rejected at the type level (acceptance 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    OwnerInstruction,
    MechanicallyVerified,
    CanonicalDocumentation,
    RepositoryState,
    TrustedAgentReport,
    Inference,
    RawHistory,
}

impl Authority {
    /// The accepted wire strings (P1a memory-provenance contract order).
    pub const WIRES: [&str; 7] = [
        "owner_instruction",
        "mechanically_verified",
        "canonical_documentation",
        "repository_state",
        "trusted_agent_report",
        "inference",
        "raw_history",
    ];

    /// Strict parse; `None` outside the vocabulary — no freeform fallback.
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "owner_instruction" => Self::OwnerInstruction,
            "mechanically_verified" => Self::MechanicallyVerified,
            "canonical_documentation" => Self::CanonicalDocumentation,
            "repository_state" => Self::RepositoryState,
            "trusted_agent_report" => Self::TrustedAgentReport,
            "inference" => Self::Inference,
            "raw_history" => Self::RawHistory,
            _ => return None,
        })
    }
}

/// The epistemic status a `remember` write may claim. The only status the
/// interlock service's contract has confirmed is `asserted`; variants are
/// added only with server-confirmed evidence — the freeform-predicate
/// rejection (2026-09-11) is the failure class an unconfirmed vocabulary
/// produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpistemicStatus {
    /// Stated as a fact by the source.
    #[serde(rename = "asserted")]
    Asserted,
}

impl EpistemicStatus {
    /// The accepted wire strings.
    pub const WIRES: [&str; 1] = ["asserted"];

    /// Strict parse: `None` for anything outside the vocabulary.
    pub fn from_wire(s: &str) -> Option<Self> {
        (s == Self::WIRES[0]).then_some(Self::Asserted)
    }
}

/// The predicate a `remember` write may use. Only server-supported
/// variants ship — including `legacy.note`. A freeform predicate is a
/// server-side 4xx `predicate_not_allowed` rejection, preserved as the
/// typed non-retryable [`V65Error::Service`] with
/// [`ServiceCode::PredicateNotAllowed`] (acceptance 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Predicate {
    /// The v5-imported note lane (`{content}` objects).
    #[serde(rename = "legacy.note")]
    LegacyNote,
}

impl Predicate {
    /// The accepted wire strings.
    pub const WIRES: [&str; 1] = ["legacy.note"];

    /// Strict parse: `None` for anything outside the vocabulary (the
    /// freeform-predicate guard).
    pub fn from_wire(s: &str) -> Option<Self> {
        (s == Self::WIRES[0]).then_some(Self::LegacyNote)
    }
}

/// One v6.5 item (proposition or note), shared by the `mandatory_policy`
/// and `items` lanes. Only the fields Awili maps are declared; everything
/// else in the envelope is ignored. `Serialize` is derived so the typed
/// lane labels in the tool results (P4b) can carry items verbatim.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct V65Item {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub authority: Option<String>,
    /// A plain string for propositions, or an object (v5-imported notes
    /// carry `{content, category, importance, …}`).
    #[serde(default)]
    pub object: Value,
    /// The recorded-at instant the service stamped on the item (e.g.
    /// `2026-08-25T16:05:55Z`), when the envelope carries it. M2c: recall
    /// entries carry their date so the model can tell a stale note from a
    /// fresh one (F10).
    #[serde(default)]
    pub recorded_at: Option<String>,
    /// The validity start instant the service stamped on the item, when the
    /// envelope carries it and no `recorded_at` is present. M2c: fallback
    /// date source for recall entries.
    #[serde(default)]
    pub valid_from: Option<String>,
}

/// The `POST /v6/recall` response envelope (the fields Awili consumes).
/// S5(a): `mandatory_policy` is REQUIRED — a missing, null, or wrong-typed lane fails to deserialize; an explicit `[]` is valid.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct V65Recall {
    pub mandatory_policy: Vec<V65Item>,
    #[serde(default)]
    pub items: Vec<V65Item>,
}

/// The `POST /v6/bootstrap` response envelope: the same two lanes as
/// recall, without a query. Consumers map `mandatory_policy` BEFORE
/// `items` and never flatten the lanes into one collection.
/// S5(a): `mandatory_policy` is REQUIRED — a missing, null, or wrong-typed lane fails to deserialize; an explicit `[]` is valid.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct V65Bootstrap {
    pub mandatory_policy: Vec<V65Item>,
    #[serde(default)]
    pub items: Vec<V65Item>,
}

/// S5(a): the hard byte limit for ONE directive's text. A policy response
/// that carries more is rejected whole (fail closed) — never truncated,
/// because truncation can drop the constraint that matters.
pub const DIRECTIVE_MAX_BYTES: usize = 4 * 1024;

/// S5(a): the hard byte limit for the RENDERED standing-directives block —
/// everything from the `"Standing directives"` header through the last
/// numbered directive line, as measured by `crate::render_directive_block`.
pub const DIRECTIVE_BLOCK_MAX_BYTES: usize = 64 * 1024;

/// A provenance-complete `remember` write. Every field serializes into the
/// `POST /v6/memories` body (acceptance 3); the caller-supplied idempotent
/// `request_id` is what makes a transport retry safe — the retry resends
/// the identical body, never a fresh id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RememberRequest {
    /// Caller-supplied idempotency id (a fresh UUID per logical write).
    pub request_id: String,
    /// The subject the proposition is about.
    pub subject: String,
    /// A server-supported predicate — never a freeform string.
    pub predicate: Predicate,
    /// The JSON object to store (for `legacy.note`: `{"content": …}`).
    pub object: Value,
    /// The provenance authority (typed vocabulary).
    pub authority: Authority,
    /// The epistemic status (typed vocabulary).
    pub epistemic_status: EpistemicStatus,
    /// The kind of source (e.g. `awili-app`).
    pub source_type: String,
    /// The source's stable reference (e.g. `awili-desktop`).
    pub source_ref: String,
    /// Why this write is happening.
    pub reason: String,
}

/// The `remember` success body: the stored object's id (a v6.5 UUID
/// string). Tolerant: unknown extra fields are ignored and a missing id is
/// `None`, not a parse failure — a 2xx is a 2xx.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct V65RememberResult {
    #[serde(default)]
    pub id: Option<String>,
}

/// One raw archive hit — EVIDENCE, not memory. A distinct type with no
/// mapper into [`crate::Directive`]/[`crate::RecallEntry`], so archive
/// results can never appear in canonical recall/policy collections
/// (acceptance 5). `Serialize` is derived so the archive-evidence lane of
/// the tool results can carry hits verbatim.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct V65ArchiveHit {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// The raw hit payload, carried verbatim (never reinterpreted).
    #[serde(default)]
    pub object: Value,
    #[serde(default)]
    pub timestamp: Option<String>,
}

/// The `POST /v6/archive/search` response envelope.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct V65ArchiveResults {
    #[serde(default)]
    pub results: Vec<V65ArchiveHit>,
}

/// The shared read-verb loop: up to two attempts — one at the 16,000
/// budget, one at a service-stated `minimum_token_budget` or after a
/// transport failure. Only the service-stated floor is ever enforced or
/// retried (acceptance 2).
fn budgeted_read<T: serde::de::DeserializeOwned>(
    client: &MemoryClient,
    path: &str,
    make_body: impl Fn(usize) -> Value,
) -> Result<T, V65Error> {
    let token = require_token(client, TokenLane::Reader)?;
    let mut budget = V65_RECALL_TOKEN_BUDGET;
    for attempt in 0..2 {
        match post(client, token, path, &make_body(budget)) {
            PostOutcome::Ok(v) => return parse(v),
            PostOutcome::BadBody(m) => return Err(V65Error::Parse(m)),
            PostOutcome::Transport if attempt == 0 => continue,
            PostOutcome::Transport => return Err(V65Error::Transport),
            PostOutcome::Rejected(r) => {
                if r.code == ServiceCode::BudgetTooSmall && attempt == 0 {
                    if let Some(min) = r.minimum {
                        // F0731: a stated floor above the cap is not adopted;
                        // the read fails with the service's own answer.
                        if min > budget && min <= V65_MAX_TOKEN_BUDGET {
                            budget = min;
                            continue;
                        }
                    }
                }
                return Err(r.into_error());
            }
        }
    }
    unreachable!("budgeted_read returns on every path")
}

/// `bootstrap`: the no-query v6.5 boot fetch (`POST /v6/bootstrap`, reader
/// token). The response's mandatory policy lane precedes its ordinary
/// items lane, and only the service-stated budget floor is enforced.
///
/// S5(a): the lane is required by serde and validated by
/// `crate::policy_directives` (per-directive 4 KiB, rendered block 64 KiB,
/// no empty/null directive) before any caller sees it.
pub fn bootstrap(client: &MemoryClient) -> Result<V65Bootstrap, V65Error> {
    let env: V65Bootstrap = budgeted_read(
        client,
        PATH_BOOTSTRAP,
        |budget| serde_json::json!({ "token_budget": budget }),
    )?;
    crate::policy_directives(&env.mandatory_policy).map_err(V65Error::Parse)?;
    Ok(env)
}

/// `recall`: one query-scoped v6.5 recall round-trip
/// (`POST /v6/recall`, reader token). Same budget-floor discipline as
/// [`bootstrap`], plus the same S5(a) policy-lane validation.
///
/// S5(a): the lane is required by serde and validated by
/// `crate::policy_directives` (per-directive 4 KiB, rendered block 64 KiB,
/// no empty/null directive) before any caller sees it.
pub fn recall(client: &MemoryClient, query: &str) -> Result<V65Recall, V65Error> {
    let env: V65Recall = budgeted_read(
        client,
        PATH_RECALL,
        |budget| serde_json::json!({ "query": query, "token_budget": budget }),
    )?;
    crate::policy_directives(&env.mandatory_policy).map_err(V65Error::Parse)?;
    Ok(env)
}

/// RC1-M-FIX1: [`recall`] that also asks for the mandatory-policy lane
/// (`include_policy: true`) — the S5 gate's ONE source of standing
/// directives on v6.5. The service's `/v6/bootstrap` no longer names that
/// lane `mandatory_policy` (live 2026-10-03 it answers `directives`, 28
/// items), and it took 6.0-6.3 s; `/v6/recall` with `include_policy` returns
/// the same 28 items as `mandatory_policy` in 1.4-1.9 s (n=6, beside a plain
/// recall). Same budget-floor discipline and lane validation as [`recall`].
pub fn recall_policy(client: &MemoryClient, query: &str) -> Result<V65Recall, V65Error> {
    let env: V65Recall = budgeted_read(
        client,
        PATH_RECALL,
        |budget| serde_json::json!({ "query": query, "token_budget": budget, "include_policy": true }),
    )?;
    crate::policy_directives(&env.mandatory_policy).map_err(V65Error::Parse)?;
    Ok(env)
}

/// `remember`: a provenance-complete write (`POST /v6/memories`, writer
/// token). Every field of [`RememberRequest`] serializes (acceptance 3); a
/// transport retry resends the identical body — the same `request_id` —
/// and a rejected write is never retried or fallen back from.
pub fn remember(
    client: &MemoryClient,
    req: &RememberRequest,
) -> Result<V65RememberResult, V65Error> {
    let token = require_token(client, TokenLane::Writer)?;
    let body =
        serde_json::to_value(req).map_err(|e| V65Error::Parse(transport::bound(&e.to_string())))?;
    for attempt in 0..2 {
        match post(client, token, PATH_MEMORIES, &body) {
            PostOutcome::Ok(v) => return parse(v),
            PostOutcome::BadBody(m) => return Err(V65Error::Parse(m)),
            // Identical idempotency arguments on the retry: same body, same id.
            PostOutcome::Transport if attempt == 0 => continue,
            PostOutcome::Rejected(r) => return Err(r.into_error()),
            PostOutcome::Transport => return Err(V65Error::Transport),
        }
    }
    unreachable!("remember returns on every path")
}

/// `archive_search`: a raw archive query (`POST /v6/archive/search`,
/// reader token). The results are RAW EVIDENCE — a distinct type, never
/// merged into canonical recall/policy collections (acceptance 5). Reads
/// retry once on a transport failure.
pub fn archive_search(
    client: &MemoryClient,
    query: &str,
    limit: usize,
) -> Result<V65ArchiveResults, V65Error> {
    let token = require_token(client, TokenLane::Reader)?;
    let body = serde_json::json!({ "query": query, "limit": limit });
    for attempt in 0..2 {
        match post(client, token, PATH_ARCHIVE_SEARCH, &body) {
            PostOutcome::Ok(v) => return parse(v),
            PostOutcome::BadBody(m) => return Err(V65Error::Parse(m)),
            PostOutcome::Transport if attempt == 0 => continue,
            PostOutcome::Transport => return Err(V65Error::Transport),
            PostOutcome::Rejected(r) => return Err(r.into_error()),
        }
    }
    unreachable!("archive_search returns on every path")
}

/// Normalized archive browse/search, distinct from semantic recall and the
/// legacy /v6/archive/search envelope. A query-less project/thread browse is
/// ordered newest-first by source timestamp and ingestion sequence.
/// Never use an unscoped browse: continuation must not collect unrelated mail
/// or conversations. Returned values are evidence, never policy.
pub fn continuation_archive(
    client: &MemoryClient,
    query: Option<&str>,
    project_key: Option<&str>,
    thread_id: Option<&str>,
) -> Result<Vec<Value>, V65Error> {
    if query.is_none_or(|s| s.trim().is_empty()) && project_key.is_none() && thread_id.is_none() {
        return Err(V65Error::Parse("unscoped archive browse refused".into()));
    }
    let body = serde_json::json!({
        "query": query, "project_key": project_key, "thread_id": thread_id,
        "limit": 8
    });
    serde_json::from_value(continuation_read(client, "v6.5/archive/search", &body)?)
        .map_err(|_| V65Error::Parse("invalid normalized archive response".into()))
}

/// A bounded chronological project page; `before` is an RFC3339 upper bound.
pub fn continuation_project_page(
    client: &MemoryClient,
    project_key: &str,
    before: Option<&str>,
) -> Result<Vec<Value>, V65Error> {
    if project_key.trim().is_empty() {
        return Err(V65Error::Parse("empty project browse refused".into()));
    }
    serde_json::from_value(continuation_read(
        client,
        "v6.5/archive/search",
        &serde_json::json!({"project_key":project_key,"to":before,"limit":64}),
    )?)
    .map_err(|_| V65Error::Parse("invalid chronological project page".into()))
}

/// Bounded relevance-only note sample, separate from the authoritative policy
/// gate. No policy is requested, truncated or interpreted here.
pub fn continuation_notes(client: &MemoryClient, query: &str) -> Result<Vec<Value>, V65Error> {
    let value = continuation_read(
        client,
        "v6/recall",
        &serde_json::json!({
            "query":query, "token_budget":4000, "include_policy":false, "limit":5
        }),
    )?;
    value
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| V65Error::Parse("invalid continuation notes".into()))
}

fn continuation_read(client: &MemoryClient, path: &str, body: &Value) -> Result<Value, V65Error> {
    use std::io::Read;
    let token = require_token(client, TokenLane::Reader)?;
    let response = client
        .client
        .post(client.url(path))
        .timeout(std::time::Duration::from_secs(3))
        .bearer_auth(token)
        .json(body)
        .send()
        .map_err(|_| V65Error::Transport)?;
    if !response.status().is_success() {
        return Err(V65Error::Service {
            status: response.status().as_u16(),
            code: ServiceCode::Unknown,
            message: "continuation read unavailable".into(),
        });
    }
    let mut bytes = Vec::new();
    response
        .take(1048577)
        .read_to_end(&mut bytes)
        .map_err(|_| V65Error::Transport)?;
    if bytes.len() > 1048576 {
        return Err(V65Error::Parse(
            "continuation response exceeds bound".into(),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| V65Error::Parse("invalid continuation response".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// A `(path, body, authorization)` capture of every request the stub saw.
    type Captures = Arc<Mutex<Vec<(String, String, String)>>>;

    /// S5(a) proof: the `mandatory_policy` lane is REQUIRED at the serde
    /// level — a missing, null, or wrong-typed lane fails to deserialize —
    /// so the read verbs can never return an envelope with an empty policy,
    /// and they REJECT an over-limit lane or a text-less item (`Parse`).
    /// An explicit `[]` stays valid (no directives is a legitimate answer).
    #[test]
    fn v65_policy_field_presence_is_required() {
        // Serde-level presence on BOTH envelope types: missing / null /
        // wrong-type rejected, explicit `[]` and a well-typed item accepted.
        fn parses(body: &str) -> (bool, bool) {
            (
                serde_json::from_str::<V65Recall>(body).is_ok(),
                serde_json::from_str::<V65Bootstrap>(body).is_ok(),
            )
        }
        assert_eq!(parses("{}"), (false, false));
        assert_eq!(parses(r#"{"items": []}"#), (false, false));
        assert_eq!(parses(r#"{"mandatory_policy": null}"#), (false, false));
        assert_eq!(parses(r#"{"mandatory_policy": "be safe"}"#), (false, false));
        assert_eq!(parses(r#"{"mandatory_policy": 7}"#), (false, false));
        assert_eq!(parses(r#"{"mandatory_policy": [7]}"#), (false, false));
        assert_eq!(parses(r#"{"mandatory_policy": []}"#), (true, true));
        assert_eq!(
            parses(r#"{"mandatory_policy": [{"object": "rule"}]}"#),
            (true, true)
        );

        // The verbs own the contract end-to-end: each malformed lane shape is
        // rejected by BOTH verbs with a `Parse` error; an explicit `[]` passes
        // both.
        for body in [
            r#"{"items": []}"#,
            r#"{"mandatory_policy": null, "items": []}"#,
            r#"{"mandatory_policy": "be safe", "items": []}"#,
            r#"{"mandatory_policy": 7, "items": []}"#,
            r#"{"mandatory_policy": [{"object": null}], "items": []}"#,
        ] {
            let (base, _caps) = stub_with(body);
            let c = client(&base);
            assert!(
                matches!(bootstrap(&c), Err(V65Error::Parse(_))),
                "bootstrap: lane shape {body} must be a parse failure"
            );
            assert!(
                matches!(recall(&c, "q"), Err(V65Error::Parse(_))),
                "recall: lane shape {body} must be a parse failure"
            );
        }
        // An explicit `[]` is a legitimate "no directives" answer.
        let (base, _caps) = stub_with(r#"{"mandatory_policy": [], "items": []}"#);
        let c = client(&base);
        assert!(bootstrap(&c).unwrap().mandatory_policy.is_empty());
        assert!(recall(&c, "q").unwrap().mandatory_policy.is_empty());
    }

    /// S5(a) proof: an oversized directive or an over-limit rendered block is
    /// REJECTED (fail closed) by the verbs rather than truncated, and
    /// `past-context` fence tags embedded in directive text are stripped so a
    /// policy item can never open or close a fence the preamble owns.
    #[test]
    fn oversize_or_fence_policy_blocks_submit() {
        fn dir(t: &str) -> crate::Directive {
            crate::Directive {
                id: None,
                text: t.to_string(),
                priority: 95,
            }
        }

        // An oversized single directive (> 4 KiB) fails the lane check.
        let big = "x".repeat(DIRECTIVE_MAX_BYTES + 1);
        let lane = vec![V65Item {
            id: None,
            recorded_at: None,
            valid_from: None,
            kind: None,
            subject: None,
            authority: None,
            object: serde_json::Value::String(big),
        }];
        assert!(crate::policy_directives(&lane).is_err());

        // A rendered block over 64 KiB fails even if each directive is
        // individually within the 4 KiB bound.
        let many = vec![dir(&"x".repeat(4096))];
        assert!(crate::render_directive_block(&many).is_ok());
        let many_more: Vec<crate::Directive> =
            std::iter::repeat_n(dir(&"y".repeat(4096)), 20).collect();
        assert!(crate::render_directive_block(&many_more).is_err());

        // The verb REJECTS an oversized lane end-to-end (fail closed — no
        // truncated policy ever reaches the caller).
        let (base, caps) = stub_with(&oversize_body());
        let c = client(&base);
        assert!(matches!(recall(&c, "q"), Err(V65Error::Parse(_))));
        assert!(matches!(bootstrap(&c), Err(V65Error::Parse(_))));
        // The rejection happens at the client boundary: the provider is never
        // handed a prompt built from a rejected policy (the only wire traffic
        // is the memory fetch itself, which the stub counted).
        let caps = caps.lock().unwrap();
        assert_eq!(caps.len(), 2, "one rejected fetch per verb, no retry");

        // A `past-context` fence tag embedded in a directive is stripped:
        // the policy text must never carry fence markup the preamble owns.
        let fenced = "do <past-context>not</ past-context> this";
        let lane = vec![V65Item {
            id: None,
            recorded_at: None,
            valid_from: None,
            kind: None,
            subject: None,
            authority: None,
            object: serde_json::Value::String(fenced.to_string()),
        }];
        let dirs = crate::policy_directives(&lane).expect("fenced but in-bound policy is accepted");
        let block = crate::render_directive_block(&dirs).unwrap();
        assert!(!block.contains("<past-context"));
        assert!(!block.contains("</past-context"));
        // The text itself survives — only the tags are dropped.
        assert!(block.contains("do not this"));

        // Tag-SPLICE attacks: a single-pass strip would leave a working
        // fence behind; the fixpoint sanitizer in
        // `crate::render_directive_block` removes every remaining tag, so a
        // directive can never open or close a `<past-context>` fence.
        for t in [
            "a </past-</past-context>context> b",
            "<past-<past-context>context>open",
            "< / PAST-context >x",
            "</past-con<past-context>text>y",
        ] {
            let block = crate::render_directive_block(&[dir(t)]).unwrap();
            assert!(!block.to_ascii_lowercase().contains("past-context"));
        }

        // An object `object` with NO `content` key: the mapper serializes its
        // JSON (5000 bytes > the 4 KiB bound), so BOTH verbs reject it as a
        // `Parse` failure — the lane is never truncated or silently dropped.
        // (The mapper's JSON measurement, not a `content` string, is what the
        // per-directive bound applies to.)
        let rule = "x".repeat(5000);
        let body =
            format!(r#"{{"mandatory_policy": [{{"object": {{"rule": "{rule}"}}}}], "items": []}}"#);
        let (base, _caps) = stub_with(&body);
        let c = client(&base);
        assert!(matches!(recall(&c, "q"), Err(V65Error::Parse(_))));
        assert!(matches!(bootstrap(&c), Err(V65Error::Parse(_))));

        // Boundary: a string `content` of exactly 4096 bytes is IN bound;
        // a `null` object has no text at all.
        let at_bound = vec![V65Item {
            id: None,
            recorded_at: None,
            valid_from: None,
            kind: None,
            subject: None,
            authority: None,
            object: serde_json::json!({"content": "x".repeat(4096)}),
        }];
        assert!(crate::policy_directives(&at_bound).is_ok());
        let nulled = vec![V65Item {
            id: None,
            recorded_at: None,
            valid_from: None,
            kind: None,
            subject: None,
            authority: None,
            object: serde_json::Value::Null,
        }];
        assert!(crate::policy_directives(&nulled).is_err());
    }

    /// A policy body with one over-limit directive (4 KiB + 1).
    fn oversize_body() -> String {
        let big = "x".repeat(DIRECTIVE_MAX_BYTES + 1);
        format!(r#"{{"mandatory_policy": [{{"object": "{big}"}}], "items": []}}"#)
    }

    /// A client pinned to the stub with a reader token (hermetic).
    fn client(base: &str) -> MemoryClient {
        MemoryClient::new(base, 90).with_v65(base, Some("test-v65-reader"), None)
    }

    /// Spawn a loopback stub that answers `POST /v6/bootstrap` and
    /// `POST /v6/recall` with `body` (200), everything else 404s. Returns the
    /// base URL and a capture of `(path, body, auth)` per request.
    fn stub_with(body: &str) -> (String, Captures) {
        let captures: Captures = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let cap = Arc::clone(&captures);
        let body = body.to_string();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let cap = Arc::clone(&cap);
                let body = body.clone();
                thread::spawn(move || {
                    // Read the full request (headers + Content-Length body).
                    let mut data = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        data.extend_from_slice(&buf[..n]);
                        if let Some(header_end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&data[..header_end]);
                            let content_length = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.trim()
                                        .eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            if data.len() >= header_end + 4 + content_length {
                                break;
                            }
                        }
                    }
                    let req = String::from_utf8_lossy(&data).into_owned();
                    let mut parts = req.split_whitespace();
                    let method = parts.next().unwrap_or("");
                    let path = parts.next().unwrap_or("/");
                    let (head, rbody) = req.split_once("\r\n\r\n").unwrap_or((req.as_str(), ""));
                    let auth = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.trim()
                                .eq_ignore_ascii_case("authorization")
                                .then(|| v.trim().to_string())
                        })
                        .unwrap_or_default();
                    cap.lock()
                        .unwrap()
                        .push((path.to_string(), rbody.to_string(), auth));
                    let (status, resp) =
                        if method == "POST" && (path == "/v6/bootstrap" || path == "/v6/recall") {
                            ("200 OK", body.as_str())
                        } else {
                            ("404 Not Found", "")
                        };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                        resp.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        (format!("http://{addr}"), captures)
    }

    /// RC1-M-FIX1: a v6.5 read that answers like the owner's live service —
    /// `bootstrap` at 6.0-6.3 s, `recall` beside it at 6.5-6.7 s (measured
    /// 2026-10-03) — is answered, once, not dropped as a transport failure.
    /// With the old 3 s client timeout both verbs failed after two requests
    /// each ("v6.5 transport failure"), so every S5 gate ran without memory.
    #[test]
    fn rc1m_fix1_reads_wait_for_a_service_as_slow_as_the_owners() {
        let hits = Arc::new(Mutex::new(0u32));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::clone(&hits);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let seen = Arc::clone(&seen);
                thread::spawn(move || {
                    let mut data = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !data.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        data.extend_from_slice(&buf[..n]);
                    }
                    *seen.lock().unwrap() += 1;
                    thread::sleep(std::time::Duration::from_millis(6_700));
                    let body = r#"{"mandatory_policy": [], "items": []}"#;
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                });
            }
        });
        let c = client(&format!("http://{addr}"));
        let started = std::time::Instant::now();
        let (boot, rec) = thread::scope(|s| {
            let b = s.spawn(|| bootstrap(&c));
            let r = s.spawn(|| recall(&c, "I need to take an outage on hostd"));
            (b.join().unwrap(), r.join().unwrap())
        });
        assert!(boot.is_ok(), "bootstrap: {boot:?}");
        assert!(rec.is_ok(), "recall: {rec:?}");
        assert_eq!(*hits.lock().unwrap(), 2, "one request each, no retry");
        assert!(
            started.elapsed() < V65_REQUEST_TIMEOUT,
            "{:?}",
            started.elapsed()
        );
    }

    /// RC1-M-FIX1: the S5 policy read asks the recall verb for the policy
    /// lane (`include_policy: true`) and returns it.
    #[test]
    fn rc1m_fix1_recall_policy_asks_for_the_policy_lane() {
        let (base, caps) =
            stub_with(r#"{"mandatory_policy": [{"object": "Search memory first."}], "items": []}"#);
        let c = client(&base);
        let rec = recall_policy(&c, "hostd").expect("policy recall");
        assert_eq!(rec.mandatory_policy.len(), 1);
        let caps = caps.lock().unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].0, "/v6/recall");
        let body: Value = serde_json::from_str(&caps[0].1).unwrap();
        assert_eq!(body["include_policy"], true);
        assert_eq!(body["query"], "hostd");
    }
}
