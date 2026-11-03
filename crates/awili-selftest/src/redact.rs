//! Secret redaction for evidence (P0b).
//!
//! The runner must never print environment variables or anything matching
//! `(?i)(token|key|secret|bearer)\s*[:=]`. [`redact`] replaces
//! secret-looking fragments with [`REDACTED`] before evidence is written.

/// The replacement used for every redacted fragment.
pub const REDACTED: &str = "[REDACTED]";

/// Spec trigger (P0-selftest.md): `token|key|secret|bearer` followed by
/// `:` or `=`, plus the value after it (the value is the secret).
///
/// S7d (F0849): the keyword may be a quoted JSON key (`"api_key": ...`);
/// the closing quote is kept in group 2. The value is a double-quoted
/// string (backslash escapes allowed, spaces included, running to the end
/// of the line if the quote never closes), a single-quoted string, or a run
/// of non-space characters.
const KEY_VALUE_PATTERN: &str =
    r#"(?i)(token|key|secret|bearer)("?)\s*[:=]\s*("(?:[^"\\\n]|\\.)*"?|'[^'\n]*'?|\S+)"#;

/// `Authorization: Bearer <value>` form: the keyword plus a space-separated
/// token value (no `:`/`=` separator), e.g. `Bearer abc123`. A quoted value
/// is taken whole; an unquoted one stops at whitespace or a `"`.
const BEARER_PATTERN: &str = r#"(?i)\bbearer\s+("(?:[^"\\\n]|\\.)*"?|[^\s"]+)"#;

/// Redact secret-looking fragments from `evidence`.
///
/// `Bearer <value>` becomes `[REDACTED]` FIRST (S7d, F0848: run second, the
/// key/value pass would swallow the word `Bearer` as the value of
/// `token: Bearer abc` and leave `abc` behind). Then
/// `(?i)(token|key|secret|bearer)\s*[:=]` runs (including the value, quoted
/// or not, and a quoted JSON key) become `<keyword>: [REDACTED]`. So
/// `Authorization: Bearer abc123` becomes `Authorization: [REDACTED]` and
/// `{"api_key": "a b"}` becomes `{"api_key": [REDACTED]}`.
///
/// The patterns are compile-time constants; if one ever fails to compile
/// (it cannot), the corresponding redaction is skipped rather than
/// panicking.
pub fn redact(evidence: &str) -> String {
    let mut out = evidence.to_owned();
    if let Ok(re) = regex::Regex::new(BEARER_PATTERN) {
        out = re.replace_all(&out, REDACTED).into_owned();
    }
    if let Ok(re) = regex::Regex::new(KEY_VALUE_PATTERN) {
        out = re.replace_all(&out, "${1}${2}: [REDACTED]").into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn bearer_form_is_redacted() {
        assert_eq!(redact("Bearer abc123"), "[REDACTED]");
    }

    #[test]
    fn bearer_header_keeps_authorization_label() {
        assert_eq!(
            redact("Authorization: Bearer abc123"),
            "Authorization: [REDACTED]"
        );
    }

    #[test]
    fn key_value_runs_are_redacted() {
        assert_eq!(redact("api_key=secret123"), "api_key: [REDACTED]");
        assert_eq!(redact("token: abc123 rest"), "token: [REDACTED] rest");
        // The keyword keeps its original case; only the value is replaced.
        assert_eq!(redact("SECRET = hunter2"), "SECRET: [REDACTED]");
    }

    #[test]
    fn ordinary_text_is_untouched() {
        // No `keyword :=` run and no `Bearer <value>` form -> unchanged.
        assert_eq!(
            redact("the key to success is patience"),
            "the key to success is patience"
        );
        assert_eq!(redact("tokenizer: 123 tokens"), "tokenizer: 123 tokens");
    }

    /// S7d (F0848): a bearer token after a key is fully redacted. Before the
    /// fix the key/value pass took `Bearer` as the value and left the token.
    #[test]
    fn s7d_bearer_after_a_key_is_redacted() {
        assert_eq!(redact("token: Bearer abc123"), "token: [REDACTED]");
        assert_eq!(
            redact("header token=Bearer abc123 end"),
            "header token: [REDACTED] end"
        );
        assert_eq!(
            redact(r#""authorization": "Bearer abc123""#),
            r#""authorization": "[REDACTED]""#
        );
    }

    /// S7d (F0849): quoted JSON keys and quoted values with spaces are
    /// redacted whole.
    #[test]
    fn s7d_quoted_keys_and_values_are_redacted() {
        assert_eq!(
            redact(r#"{"api_key": "sk live 123"}"#),
            r#"{"api_key": [REDACTED]}"#
        );
        assert_eq!(
            redact(r#"{"access_token":"abc","user":"me"}"#),
            r#"{"access_token": [REDACTED],"user":"me"}"#
        );
        assert_eq!(
            redact(r#"{"secret": "a \"b\" c", "n": 1}"#),
            r#"{"secret": [REDACTED], "n": 1}"#
        );
        assert_eq!(redact("token = 'two words' next"), "token: [REDACTED] next");
        assert_eq!(
            redact(r#"token: "never closed and more"#),
            "token: [REDACTED]"
        );
        assert_eq!(redact(r#"Bearer "quoted value""#), "[REDACTED]");
    }
}
