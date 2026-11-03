//! The v6.5 transport and error model: bearer-lane resolution, one bounded
//! POST per attempt, sanitized service errors, and the shared helpers the
//! typed verbs ([`super::bootstrap`], [`super::recall`], [`super::remember`],
//! [`super::archive_search`]) retry-loop over.
//!
//! Error discipline: [`V65Error`] carries the HTTP status plus a bounded,
//! sanitized service code/message (the body's `error.code`/`error.message`,
//! truncated to [`MAX_SERVICE_MESSAGE_CHARS`]). The bearer travels ONLY in
//! the `Authorization` header and never appears in any variant, its
//! `Display`, or its `Debug` (acceptance 6).
//!
//! Retry discipline: reads retry once on a transport failure; a 422
//! `budget_too_small` retries once at exactly the service-stated floor;
//! writes retry once with IDENTICAL idempotency arguments (same body, same
//! caller-supplied `request_id`). Any other answer is final — no retry, no
//! fallback from a rejected write.

use serde_json::Value;

use crate::MemoryClient;

/// Char bound on the sanitized service message in [`V65Error::Service`]: a
/// hostile or broken body must not bloat the error surface.
pub const MAX_SERVICE_MESSAGE_CHARS: usize = 200;

/// Sanitized v6.5 service error codes; unrecognized strings classify as
/// [`ServiceCode::Unknown`] rather than being echoed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceCode {
    /// 422: `token_budget` below the service-stated floor.
    BudgetTooSmall,
    /// 4xx: predicate not server-supported — non-retryable.
    PredicateNotAllowed,
    /// 4xx: the object tripped the service PII guard.
    PiiGuard,
    /// Any other service code.
    Unknown,
}

impl ServiceCode {
    /// Classify a wire code string.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "budget_too_small" => Self::BudgetTooSmall,
            "predicate_not_allowed" => Self::PredicateNotAllowed,
            "pii_guard" => Self::PiiGuard,
            _ => Self::Unknown,
        }
    }
}

/// Which bearer token a verb needs — resolved at client construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenLane {
    /// Reader/boot token: bootstrap, recall, archive_search.
    Reader,
    /// Owner/writer token: remember.
    Writer,
}

/// Why a typed v6.5 verb failed. `Service` carries the HTTP status plus a
/// bounded sanitized code/message; the bearer travels ONLY in the
/// `Authorization` header — never in an error (acceptance 6).
#[derive(Debug)]
pub enum V65Error {
    /// No token resolved for the verb's lane (fails closed, no I/O).
    MissingToken(TokenLane),
    /// Transport failure; detail-free so nothing can leak.
    Transport,
    /// Non-2xx answer: status + sanitized code + bounded message.
    Service {
        status: u16,
        code: ServiceCode,
        message: String,
    },
    /// A 2xx body that did not parse into the verb's response.
    Parse(String),
}

impl V65Error {
    /// The HTTP status of a service answer, if any.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Service { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The sanitized service code of a service answer, if any.
    pub fn code(&self) -> Option<ServiceCode> {
        match self {
            Self::Service { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The typed non-retryable predicate rejection (acceptance 4): a 4xx
    /// `predicate_not_allowed`.
    pub fn is_predicate_not_allowed(&self) -> bool {
        match self {
            Self::Service {
                status,
                code: ServiceCode::PredicateNotAllowed,
                ..
            } => (400..500).contains(status),
            _ => false,
        }
    }

    /// Whether a bounded transport retry is allowed: only `Transport`.
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Transport)
    }
}

impl std::fmt::Display for V65Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingToken(TokenLane::Reader) => write!(f, "no v6.5 reader token resolved"),
            Self::MissingToken(TokenLane::Writer) => write!(f, "no v6.5 writer token resolved"),
            Self::Transport => write!(f, "v6.5 transport failure (details withheld)"),
            Self::Service {
                status,
                code,
                message,
            } => {
                let c = match code {
                    ServiceCode::BudgetTooSmall => "budget_too_small",
                    ServiceCode::PredicateNotAllowed => "predicate_not_allowed",
                    ServiceCode::PiiGuard => "pii_guard",
                    ServiceCode::Unknown => "unknown",
                };
                write!(
                    f,
                    "v6.5 service error: status={status} code={c} message={message}"
                )
            }
            Self::Parse(m) => write!(f, "v6.5 response parse error: {m}"),
        }
    }
}

impl std::error::Error for V65Error {}

/// Pull `minimum_token_budget` out of a v6.5 error body
/// (`{"error":{"code":"budget_too_small","minimum_token_budget":N,…}}`).
pub fn minimum_budget(body: &str) -> Option<usize> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("error")?
        .get("minimum_token_budget")?
        .as_u64()
        .map(|n| n as usize)
}

/// Char-safe truncation to [`MAX_SERVICE_MESSAGE_CHARS`].
pub(super) fn bound(s: &str) -> String {
    s.chars().take(MAX_SERVICE_MESSAGE_CHARS).collect()
}

/// The sanitized `(code, message, minimum)` triple from a non-2xx body:
/// `{"error":{…}}` (top-level `{code,…}` tolerated). Unparseable input
/// degrades to `(Unknown, "", None)` — never a raw echo.
///
/// RC2-SVC1 (T23): the message is REDACTED before it is bounded — the bearer
/// this request sent, and any `Bearer <credential>` text, become
/// `[redacted]` — so a service that reflects the request's credential in its
/// error text cannot put it into an error, a log or the screen.
pub(super) fn sanitize(body: &str, token: &str) -> (ServiceCode, String, Option<usize>) {
    let Some(v) = serde_json::from_str::<Value>(body).ok() else {
        return (ServiceCode::Unknown, String::new(), None);
    };
    let e = v.get("error").unwrap_or(&v);
    let code = e
        .get("code")
        .and_then(Value::as_str)
        .map(ServiceCode::from_wire)
        .unwrap_or(ServiceCode::Unknown);
    let message = e
        .get("message")
        .and_then(Value::as_str)
        .map(|m| bound(&redact_credentials(m, token)))
        .unwrap_or_default();
    let minimum = e
        .get("minimum_token_budget")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    (code, message, minimum)
}

/// Replace the request's own bearer `token` (when non-empty) and every
/// `Bearer <credential>` run (case-insensitive scheme word, the credential is
/// the following non-whitespace run) with `[redacted]`.
pub(super) fn redact_credentials(message: &str, token: &str) -> String {
    let mut out = if token.is_empty() {
        message.to_string()
    } else {
        message.replace(token, "[redacted]")
    };
    const SCHEME: &str = "bearer";
    let mut from = 0;
    loop {
        let lower = out.to_ascii_lowercase();
        let Some(rel) = lower[from..].find(SCHEME) else {
            break;
        };
        let start = from + rel;
        let after = start + SCHEME.len();
        let gap = out[after..].len() - out[after..].trim_start().len();
        let cred_start = after + gap;
        let cred_len = out[cred_start..]
            .find(char::is_whitespace)
            .unwrap_or(out.len() - cred_start);
        if gap == 0 || cred_len == 0 || out[cred_start..].starts_with("[redacted]") {
            from = after;
            continue;
        }
        out.replace_range(cred_start..cred_start + cred_len, "[redacted]");
        from = cred_start + "[redacted]".len();
    }
    out
}

/// A non-2xx service answer, pre-sanitised for the verb's retry decision.
#[derive(Debug)]
pub(super) struct Rejected {
    pub(super) status: u16,
    pub(super) code: ServiceCode,
    pub(super) message: String,
    /// The `minimum_token_budget` named by a `budget_too_small` answer.
    pub(super) minimum: Option<usize>,
}

impl Rejected {
    pub(super) fn into_error(self) -> V65Error {
        V65Error::Service {
            status: self.status,
            code: self.code,
            message: self.message,
        }
    }
}

/// What a bounded `POST` can return: the 2xx body, a transport failure, a
/// 2xx body that failed JSON parsing, or a sanitized non-2xx answer.
pub(super) enum PostOutcome {
    Ok(Value),
    Transport,
    BadBody(String),
    Rejected(Rejected),
}

/// The verb's bearer token, or a fail-closed [`V65Error::MissingToken`].
pub(super) fn require_token(client: &MemoryClient, lane: TokenLane) -> Result<&str, V65Error> {
    match lane {
        TokenLane::Reader => client.v65_reader.as_deref(),
        TokenLane::Writer => client.v65_writer.as_deref(),
    }
    .ok_or(V65Error::MissingToken(lane))
}

/// RC1-M-FIX1: the whole-request timeout of a v6.5 call. The client's 3 s
/// (sized for v5) never fit v6.5: measured 2026-10-03 on the owner's Mac over
/// the 127.0.0.1:18861 tunnel at the 16,000 budget, `bootstrap` took 6.0-6.3 s
/// (n=5), `recall` 1.1-1.9 s alone (n=40) and 6.5-6.7 s while a `bootstrap`
/// runs (n=5, the S5 gate's two workers), so every S5 gate failed ("v6.5
/// transport failure") and the turn ran without memory. 15 s covers the worst
/// measured call more than twice over; the connect timeout (800 ms) still
/// fails fast when the service is down.
pub const V65_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// One bounded POST with the verb's bearer token, bounded by
/// [`V65_REQUEST_TIMEOUT`] (and the client's connect timeout).
pub(super) fn post(client: &MemoryClient, token: &str, path: &str, body: &Value) -> PostOutcome {
    let Some(resp) = client
        .client
        .post(client.url(path))
        .timeout(V65_REQUEST_TIMEOUT)
        .bearer_auth(token)
        .json(body)
        .send()
        .ok()
    else {
        return PostOutcome::Transport;
    };
    let status = resp.status().as_u16();
    let Ok(text) = resp.text() else {
        return PostOutcome::Transport;
    };
    if (200..300).contains(&status) {
        return match serde_json::from_str::<Value>(&text) {
            Ok(v) => PostOutcome::Ok(v),
            Err(e) => PostOutcome::BadBody(bound(&e.to_string())),
        };
    }
    let (code, message, minimum) = sanitize(&text, token);
    PostOutcome::Rejected(Rejected {
        status,
        code,
        message,
        minimum,
    })
}

/// Parse a 2xx body into `T`, bounded.
pub(super) fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, V65Error> {
    serde_json::from_value(v).map_err(|e| V65Error::Parse(bound(&e.to_string())))
}

#[cfg(test)]
mod rc2svc1_tests {
    use super::*;

    /// RC2-SVC1 (T23): the request's own token and any `Bearer <x>` run are
    /// redacted, case-insensitively, and BEFORE the length bound — a token
    /// straddling the bound leaves no prefix behind.
    #[test]
    fn rc2svc1_service_messages_are_redacted_before_the_bound() {
        let token = "sekret-reader-token-123";
        let body = format!(
            r#"{{"error":{{"code":"x","message":"saw {token} and BEARER other-cred ok"}}}}"#
        );
        let (_, msg, _) = sanitize(&body, token);
        assert!(!msg.contains(token) && !msg.contains("other-cred"), "{msg}");
        assert_eq!(msg, "saw [redacted] and BEARER [redacted] ok");

        let filler = "x".repeat(MAX_SERVICE_MESSAGE_CHARS - 5);
        let body = format!(r#"{{"error":{{"message":"{filler}{token}"}}}}"#);
        let (_, msg, _) = sanitize(&body, token);
        assert!(
            !msg.contains("sekr"),
            "a token prefix survived the bound: {msg}"
        );
        assert_eq!(msg.chars().count(), MAX_SERVICE_MESSAGE_CHARS);

        // An ordinary word is untouched; an empty token redacts nothing extra.
        assert_eq!(redact_credentials("bearerless text", ""), "bearerless text");
    }
}
