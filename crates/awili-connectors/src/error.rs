//! [`ConnectorError`] — the single error type for both connector clients.
//!
//! Invariant (checked by the loopback tests): a secret value can appear in
//! NONE of these renderings. `Secret` names only the path/key or variable
//! name that failed; `Status` carries a body excerpt that has every resolved
//! secret value replaced with `<redacted>` *before* being capped at 200 chars,
//! so a server that echoes an `Authorization` header back in an error body
//! still cannot leak the token; `Http`, `Builder` and `Decode` are
//! transport/parser text that never see a secret.

use thiserror::Error;

/// The literal that replaces every occurrence of a resolved secret value in a
/// server body before it is stored in [`ConnectorError::Status`].
pub(crate) const REDACTED: &str = "<redacted>";

/// An error from either connector client.
#[derive(Debug, Error)]
pub enum ConnectorError {
    /// The named secret could not be resolved. `path` and `key` are file
    /// details (a `SecretRef::Env` failure has `path == None` and the variable
    /// name in `key`); `reason` is secret-free by construction.
    #[error("secret unavailable (path={path:?}, key={key:?}): {reason}")]
    Secret {
        /// The file that failed to read/parse; `None` for `Env` refs.
        path: Option<std::path::PathBuf>,
        /// The key line that was missing/empty (or the env var name); `None`
        /// for whole-file refs.
        key: Option<String>,
        /// Why resolution failed. Never contains a secret value.
        reason: String,
    },
    /// The request never completed (connect failure, timeout, TLS).
    #[error("http transport error: {0}")]
    Http(#[source] reqwest::Error),
    /// The HTTP client could not even be constructed. The underlying
    /// `reqwest::Error` text is config-level and never contains a secret.
    #[error("could not build http client: {0}")]
    Builder(#[source] reqwest::Error),
    /// The server answered with a non-2xx status. `body_excerpt` is the
    /// response body with the resolved secret value redacted, then truncated
    /// to 200 chars.
    #[error("http {code} from server: {body_excerpt}")]
    Status {
        /// The HTTP status code (e.g. `401`).
        code: u16,
        /// Response body, secret-redacted and truncated to 200 chars.
        body_excerpt: String,
    },
    /// A 2xx response body did not parse as the expected JSON shape.
    #[error("decode error: {0}")]
    Decode(String),
}

/// Cap a server body before it is stored in a [`ConnectorError::Status`].
/// The cap keeps error strings bounded for logging.
pub(crate) const MAX_BODY_EXCERPT: usize = 200;

/// Redact every occurrence of `secret` in `body` with [`REDACTED`], then
/// truncate the result to [`MAX_BODY_EXCERPT`] chars.
///
/// The redaction happens **before** the cap so that even a server that echoes
/// the resolved `Authorization` header value back in an error body cannot leak
/// it. When `secret` is empty (the value should never be empty in practice) the
/// body is passed through untouched and only the cap applies.
pub(crate) fn redact_and_truncate(body: &str, secret: &str) -> String {
    if secret.is_empty() {
        return truncate(body);
    }
    let redacted = body.split(secret).collect::<Vec<_>>().join(REDACTED);
    truncate(&redacted)
}

/// Truncate `body` to [`MAX_BODY_EXCERPT`] chars.
pub(crate) fn truncate(body: &str) -> String {
    let chars: Vec<char> = body.chars().take(MAX_BODY_EXCERPT).collect();
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_and_truncate_replaces_secret_before_cap() {
        let secret = "super-secret-token";
        // Secret in the middle; body longer than 200 chars.
        let body = format!("{}{}{}", "a".repeat(100), secret, "b".repeat(100));
        let redacted = redact_and_truncate(&body, secret);
        assert!(!redacted.contains(secret));
        assert!(redacted.contains("<redacted>"));
        // 100 a's + 10 redacted + 90 b's (truncated to 200) = 200 chars.
        assert_eq!(redacted.len(), 200);
    }

    #[test]
    fn redact_empty_secret_only_caps() {
        let body = format!("x{}", "y".repeat(300));
        let out = redact_and_truncate(&body, "");
        assert_eq!(out.len(), MAX_BODY_EXCERPT);
    }
}
