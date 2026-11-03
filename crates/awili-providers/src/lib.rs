//! `awili-providers` — streaming chat backends behind one [`ChatProvider`] trait.
//!
//! Three impls (slice-2 spec §1):
//! - [`OllamaChat`] — local Ollama `/api/chat`, newline-delimited JSON.
//! - [`AnthropicMessages`] — the Anthropic Messages API over SSE; the API key
//!   comes from `AWILI_ANTHROPIC_KEY` for now.
//! - [`HermesCompat`] — an Anthropic-compatible endpoint at a configurable
//!   `base_url` (e.g. a self-hosted Hermes), reusing the Anthropic wire format.
//!
//! Every impl normalises its wire format onto the four-variant [`ProviderEvent`]
//! vocabulary. The *decoders* (the part that can be wrong) are pure and fully
//! fixture-tested in [`anthropic`] and [`ollama`]; the [`transport`] wiring that
//! actually opens a socket is intentionally untested by `cargo test` (no live
//! network) and validated by the Mac smoke pass instead.

mod anthropic;
mod decode;
mod event;
mod ollama;
mod openai;
mod transport;

pub use decode::{Decoder, LineBuffer, MAX_LINE_BYTES, STREAM_TOO_LARGE};
pub use event::{ChatMessage, ChatProvider, ChatRequest, ProviderEvent, Role, Usage};

// Re-export the pure pieces so they are reachable (and the body builders are
// usable by callers that want to inspect a request).
pub use anthropic::{build_body as build_anthropic_body, AnthropicDecoder};
pub use ollama::{build_body as build_ollama_body, OllamaDecoder};
pub use openai::{build_body as build_openai_body, OpenAiDecoder};

use futures::stream::BoxStream;

/// Coarse classification of a provider error message (slice 15 §3), driving the
/// app's automatic fallback: `Overloaded`/`RateLimit` retry on the next model of
/// any provider, `Quota` retries while skipping models of the same provider kind
/// (the whole account is out, not one endpoint), `Other` never retries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// The provider is at capacity (`overloaded_error`, HTTP 529).
    Overloaded,
    /// Request rate exceeded (`rate_limit_error`, HTTP 429).
    RateLimit,
    /// Out of quota/credits (`quota`, `billing`, `credit balance`,
    /// `insufficient credit`, `out of credit`, `usage limit`).
    Quota,
    /// Anything else (auth, network, malformed request) — not retryable.
    Other,
}

/// Classify a [`ProviderEvent::Error`] message by substring, case-insensitively.
/// Quota wins over the capacity classes: a "quota exceeded (429)" message must
/// ban the provider, not just rotate past the model.
///
/// Credit-depletion / subscription-usage-limit messages from Anthropic/OpenAI
/// (e.g. "credit balance is too low", "usage limit reached") also map to
/// [`ProviderErrorKind::Quota`] so the app falls back to a local/fleet model
/// instead of hard-failing.
pub fn classify_error(msg: &str) -> ProviderErrorKind {
    let m = msg.to_lowercase();
    if m.contains("quota")
        || m.contains("billing")
        || m.contains("credit balance")
        || m.contains("insufficient credit")
        || m.contains("out of credit")
        || m.contains("usage limit")
    {
        ProviderErrorKind::Quota
    } else if m.contains("overloaded") || m.contains("529") {
        ProviderErrorKind::Overloaded
    } else if m.contains("rate_limit") || m.contains("rate limit") || m.contains("429") {
        ProviderErrorKind::RateLimit
    } else {
        ProviderErrorKind::Other
    }
}

/// Default upper bound on generated tokens for the Anthropic-family providers
/// (the API requires `max_tokens`). Generous enough for chat replies.
const DEFAULT_MAX_TOKENS: u32 = 2048;

/// Local Ollama. `base_url` defaults to `http://localhost:11434`; the model is
/// chosen per request.
pub struct OllamaChat {
    base_url: String,
}

impl OllamaChat {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl ChatProvider for OllamaChat {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        let url = format!("{}/api/chat", self.base_url.trim_end_matches('/'));
        let body = ollama::build_body(&req);
        transport::spawn_stream(
            move || {
                transport::shared_client()
                    .post(&url)
                    .json(&body)
                    .send()
                    .map_err(|e| format!("ollama request failed: {e}"))
            },
            Box::new(OllamaDecoder::new()),
        )
    }
}

/// The Anthropic Messages API. The `api_key` is supplied by the caller (the app
/// reads `AWILI_ANTHROPIC_KEY`).
pub struct AnthropicMessages {
    base_url: String,
    api_key: Option<String>,
    max_tokens: u32,
}

impl AnthropicMessages {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    /// Set the API key (from `AWILI_ANTHROPIC_KEY`).
    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        self.api_key = key;
        self
    }
}

impl ChatProvider for AnthropicMessages {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        anthropic_stream(
            &self.base_url,
            self.api_key.clone(),
            self.max_tokens,
            req,
            true,
        )
    }
}

/// An Anthropic-compatible endpoint at a configurable base URL (Hermes). Same
/// wire format and decoder as Anthropic; the API key is optional because a
/// self-hosted endpoint may not require one.
pub struct HermesCompat {
    base_url: String,
    api_key: Option<String>,
    max_tokens: u32,
}

impl HermesCompat {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        self.api_key = key;
        self
    }
}

impl ChatProvider for HermesCompat {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        // `require_key = false`: a missing key is allowed for a self-hosted box.
        anthropic_stream(
            &self.base_url,
            self.api_key.clone(),
            self.max_tokens,
            req,
            false,
        )
    }
}

/// An OpenAI-compatible Chat Completions endpoint (`/v1/chat/completions`) at a
/// configurable base URL — vLLM, llama.cpp, DeepSeek, etc. Distinct from the
/// Anthropic-wire [`HermesCompat`]: different request shape, different SSE
/// decoder. The API key is optional (a self-hosted box may not require one) and
/// sent as a Bearer token when present.
pub struct OpenAiCompat {
    base_url: String,
    api_key: Option<String>,
}

impl OpenAiCompat {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: None,
        }
    }

    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        self.api_key = key;
        self
    }
}

impl ChatProvider for OpenAiCompat {
    fn stream(&self, req: ChatRequest) -> BoxStream<'static, ProviderEvent> {
        let url = openai_chat_url(&self.base_url);
        let body = openai::build_body(&req);
        let api_key = self.api_key.clone();
        transport::spawn_stream(
            move || {
                refuse_unresolved_key(api_key.as_deref())?;
                build_openai_request(transport::shared_client(), &url, api_key.as_deref(), &body)
                    .send()
                    .map_err(|e| format!("openai request failed: {e}"))
            },
            Box::new(OpenAiDecoder::new()),
        )
    }
}

/// Normalise an OpenAI-compatible base URL to the full chat-completions endpoint.
/// The `/v1` segment is part of the OpenAI convention, but a configured
/// `base_url` may or may not already include it (the Hermes `[hermes]` default is
/// `http://127.0.0.1:8642/v1`). Strip a trailing `/v1` then re-append the full
/// path, so both forms collapse to a single `/v1/chat/completions` — guarding the
/// double-`/v1` 404 that routing Chat-mode Hermes through here originally caused.
fn openai_chat_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/v1/chat/completions")
}

/// RC2-SVC5 (F0790): normalise an Anthropic-wire base URL to the full
/// messages endpoint, the same way [`openai_chat_url`] does: a configured
/// `base_url` with or without a trailing `/v1` both give one
/// `/v1/messages` (never `/v1/v1/messages`).
fn anthropic_messages_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/v1/messages")
}

/// A key still of the form `secret:<account>` was never resolved from the
/// system keychain; sending it would put the reference text on the wire as the
/// credential. Both request builders drop it and both streams refuse it.
fn is_unresolved_secret(key: &str) -> bool {
    key.trim_start().starts_with("secret:")
}

const UNRESOLVED_KEY_ERROR: &str =
    "the API key is an unresolved secret: reference; enter it again in Settings";

fn refuse_unresolved_key(key: Option<&str>) -> Result<(), String> {
    match key {
        Some(k) if is_unresolved_secret(k) => Err(UNRESOLVED_KEY_ERROR.to_string()),
        _ => Ok(()),
    }
}

/// Build the OpenAI-wire HTTP request, attaching `Authorization: Bearer` **only**
/// when a non-empty credential is present (a self-hosted box may need none).
/// Extracted from [`OpenAiCompat::stream`] so the URL normalisation and the
/// auth-header rule are unit-testable without a live socket — mirrors
/// [`build_anthropic_request`].
fn build_openai_request(
    client: &reqwest::blocking::Client,
    url: &str,
    api_key: Option<&str>,
    body: &serde_json::Value,
) -> reqwest::blocking::RequestBuilder {
    let mut rb = client.post(url).header("content-type", "application/json");
    if let Some(key) = api_key.filter(|k| !k.is_empty() && !is_unresolved_secret(k)) {
        rb = rb.header("authorization", format!("Bearer {key}"));
    }
    rb.json(body)
}

/// Shared launcher for the Anthropic-wire providers. `require_key` makes a missing
/// key a hard error for hosted Anthropic, but tolerated for Hermes.
///
/// The `api_key` passed here is whatever credential *that specific provider* owns:
/// the hosted Anthropic key for [`AnthropicMessages`], or Hermes's own (usually
/// absent) credential for [`HermesCompat`]. The two never share a key — see
/// `build_anthropic_request` for the attachment rule, regression-tested below.
fn anthropic_stream(
    base_url: &str,
    api_key: Option<String>,
    max_tokens: u32,
    req: ChatRequest,
    require_key: bool,
) -> BoxStream<'static, ProviderEvent> {
    let url = anthropic_messages_url(base_url);
    let body = anthropic::build_body(&req, max_tokens);
    transport::spawn_stream(
        move || {
            refuse_unresolved_key(api_key.as_deref())?;
            if require_key && api_key.as_deref().unwrap_or("").is_empty() {
                return Err("no Anthropic API key (set AWILI_ANTHROPIC_KEY)".to_string());
            }
            build_anthropic_request(transport::shared_client(), &url, api_key.as_deref(), &body)
                .send()
                .map_err(|e| format!("anthropic request failed: {e}"))
        },
        Box::new(AnthropicDecoder::new()),
    )
}

/// Build the Anthropic-wire HTTP request, attaching `x-api-key` **only** when a
/// non-empty credential is present. Extracted from [`anthropic_stream`] so the
/// credential-attachment rule is unit-testable without a live socket: a Hermes
/// provider built with no key must produce a request carrying no `x-api-key`.
fn build_anthropic_request(
    client: &reqwest::blocking::Client,
    url: &str,
    api_key: Option<&str>,
    body: &serde_json::Value,
) -> reqwest::blocking::RequestBuilder {
    let mut rb = client
        .post(url)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");
    if let Some(key) = api_key.filter(|k| !k.is_empty() && !is_unresolved_secret(k)) {
        rb = rb.header("x-api-key", key);
    }
    rb.json(body)
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    #[test]
    fn classifies_overloaded() {
        // Anthropic's SSE error type and the bare HTTP status both count.
        assert_eq!(
            classify_error("overloaded_error: try later"),
            ProviderErrorKind::Overloaded
        );
        assert_eq!(
            classify_error("HTTP 529 from upstream"),
            ProviderErrorKind::Overloaded
        );
        assert_eq!(classify_error("Overloaded"), ProviderErrorKind::Overloaded);
    }

    #[test]
    fn classifies_rate_limit() {
        assert_eq!(
            classify_error("rate_limit_error"),
            ProviderErrorKind::RateLimit
        );
        assert_eq!(
            classify_error("HTTP 429 Too Many Requests"),
            ProviderErrorKind::RateLimit
        );
        assert_eq!(
            classify_error("Rate limit exceeded"),
            ProviderErrorKind::RateLimit
        );
        // REGRESSION guard: a rate-limit message that even mentions "usage" must stay
        // RateLimit — "usage tier" must not collide with the new "usage limit" quota substring.
        assert_eq!(
            classify_error("API rate limit exceeded for your current usage tier"),
            ProviderErrorKind::RateLimit
        );
    }

    #[test]
    fn classifies_quota() {
        assert_eq!(
            classify_error("monthly quota exceeded"),
            ProviderErrorKind::Quota
        );
        assert_eq!(
            classify_error("billing hard limit reached"),
            ProviderErrorKind::Quota
        );
        // Quota outranks the capacity classes even when both appear.
        assert_eq!(
            classify_error("quota exceeded (429)"),
            ProviderErrorKind::Quota
        );

        // Credit-depletion / usage-limit messages (Anthropic/OpenAI June 2026 billing change)
        assert_eq!(
            classify_error("Your credit balance is too low to make this request"),
            ProviderErrorKind::Quota
        );
        assert_eq!(
            classify_error("credit balance is too low"),
            ProviderErrorKind::Quota
        );
        assert_eq!(
            classify_error("You have reached your usage limit"),
            ProviderErrorKind::Quota
        );
        assert_eq!(
            classify_error("usage limit reached"),
            ProviderErrorKind::Quota
        );
        assert_eq!(
            classify_error("insufficient credits"),
            ProviderErrorKind::Quota
        );
        assert_eq!(classify_error("out of credits"), ProviderErrorKind::Quota);
    }

    #[test]
    fn classifies_other() {
        assert_eq!(
            classify_error("no Anthropic API key (set AWILI_ANTHROPIC_KEY)"),
            ProviderErrorKind::Other
        );
        assert_eq!(
            classify_error("connection refused"),
            ProviderErrorKind::Other
        );
        assert_eq!(classify_error(""), ProviderErrorKind::Other);
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;
    use serde_json::json;

    fn header_value(rb: reqwest::blocking::RequestBuilder, name: &str) -> Option<String> {
        let req = rb.build().expect("request builds");
        req.headers()
            .get(name)
            .map(|v| v.to_str().unwrap().to_string())
    }

    /// A `secret:<account>` reference that was never resolved is not a
    /// credential: neither builder puts it on the wire, and the streams
    /// refuse it with a plain error.
    #[test]
    fn unresolved_secret_reference_is_never_sent() {
        let client = transport::shared_client();
        let body = json!({"model": "m"});
        let a = build_anthropic_request(
            client,
            "http://h/v1/messages",
            Some("secret:provider:x"),
            &body,
        );
        assert_eq!(header_value(a, "x-api-key"), None);
        let o = build_openai_request(
            client,
            "http://h/v1/chat/completions",
            Some("secret:provider:x"),
            &body,
        );
        assert_eq!(header_value(o, "authorization"), None);
        assert!(refuse_unresolved_key(Some("secret:provider:x")).is_err());
        assert!(refuse_unresolved_key(Some("sk-real")).is_ok());
        assert!(refuse_unresolved_key(None).is_ok());
    }

    /// BLOCKER regression: a Hermes-shaped request (no credential of its own) must
    /// NOT carry an `x-api-key` header. This is the guard against the hosted
    /// Anthropic key leaking to a user-controlled Hermes `base_url`.
    #[test]
    fn hermes_request_has_no_api_key_header() {
        let client = transport::shared_client();
        let rb = build_anthropic_request(
            client,
            "http://hermes.local/v1/messages",
            None,
            &json!({"model": "hermes-4"}),
        );
        assert_eq!(header_value(rb, "x-api-key"), None);
    }

    /// An empty-string key is treated as "no key" — still no header attached.
    #[test]
    fn empty_key_attaches_no_header() {
        let client = transport::shared_client();
        let rb = build_anthropic_request(
            client,
            "http://hermes.local/v1/messages",
            Some(""),
            &json!({}),
        );
        assert_eq!(header_value(rb, "x-api-key"), None);
    }

    /// The Anthropic provider DOES attach its key.
    #[test]
    fn anthropic_request_attaches_its_key() {
        let client = transport::shared_client();
        let rb = build_anthropic_request(
            client,
            "https://api.anthropic.com/v1/messages",
            Some("sk-ant-test"),
            &json!({}),
        );
        assert_eq!(
            header_value(rb, "x-api-key"),
            Some("sk-ant-test".to_string())
        );
    }
}

#[cfg(test)]
mod openai_request_tests {
    use super::*;
    use serde_json::json;

    fn header_value(rb: reqwest::blocking::RequestBuilder, name: &str) -> Option<String> {
        let req = rb.build().expect("request builds");
        req.headers()
            .get(name)
            .map(|v| v.to_str().unwrap().to_string())
    }

    fn request_url(rb: reqwest::blocking::RequestBuilder) -> String {
        rb.build().expect("request builds").url().to_string()
    }

    /// The Hermes `[hermes]` default base_url already ends in `/v1`; it must
    /// normalise to a SINGLE `/v1/chat/completions`, not `/v1/v1/...`. This is
    /// the exact endpoint the owner's working curl hit — the regression that the
    /// Chat-mode 404 came from routing Hermes to the Anthropic-wire path instead.
    #[test]
    fn hermes_default_base_normalises_to_single_v1() {
        assert_eq!(
            openai_chat_url("http://127.0.0.1:8642/v1"),
            "http://127.0.0.1:8642/v1/chat/completions"
        );
    }

    /// A base_url WITHOUT a trailing `/v1` yields the same single-`/v1` endpoint.
    #[test]
    fn base_without_v1_normalises_the_same() {
        assert_eq!(
            openai_chat_url("http://127.0.0.1:8642"),
            "http://127.0.0.1:8642/v1/chat/completions"
        );
    }

    /// RC2-SVC5 (F0790): the Anthropic-wire endpoint normalises a `/v1`
    /// base exactly like the OpenAI one — never `/v1/v1/messages`.
    #[test]
    fn rc2svc5_anthropic_base_with_v1_normalises_to_single_v1() {
        for base in [
            "https://api.anthropic.com",
            "https://api.anthropic.com/",
            "https://api.anthropic.com/v1",
            "https://api.anthropic.com/v1/",
        ] {
            assert_eq!(
                anthropic_messages_url(base),
                "https://api.anthropic.com/v1/messages",
                "{base}"
            );
        }
    }

    /// A trailing slash (with or without `/v1`) is tolerated.
    #[test]
    fn trailing_slash_is_tolerated() {
        assert_eq!(
            openai_chat_url("http://127.0.0.1:8642/v1/"),
            "http://127.0.0.1:8642/v1/chat/completions"
        );
        assert_eq!(
            openai_chat_url("http://127.0.0.1:8642/"),
            "http://127.0.0.1:8642/v1/chat/completions"
        );
    }

    /// The Chat-mode Hermes request carries `Authorization: Bearer <key>` — the
    /// `[hermes]` credential — and posts to the normalised endpoint. No
    /// `x-api-key`, no double `/v1`.
    #[test]
    fn hermes_chat_request_is_bearer_to_single_v1() {
        let client = transport::shared_client();
        let url = openai_chat_url("http://127.0.0.1:8642/v1");
        let rb = build_openai_request(
            client,
            &url,
            Some("hermes-secret"),
            &json!({"model": "hermes-agent"}),
        );
        let built = rb.build().expect("request builds");
        assert_eq!(
            built.url().to_string(),
            "http://127.0.0.1:8642/v1/chat/completions"
        );
        assert_eq!(
            built
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_string()),
            Some("Bearer hermes-secret".to_string())
        );
        assert_eq!(built.headers().get("x-api-key"), None);
    }

    /// A self-hosted box that needs no credential attaches no auth header.
    #[test]
    fn empty_key_attaches_no_auth_header() {
        let client = transport::shared_client();
        let rb = build_openai_request(client, "http://h/v1/chat/completions", Some(""), &json!({}));
        assert_eq!(header_value(rb, "authorization"), None);
    }

    #[test]
    fn no_key_attaches_no_auth_header() {
        let client = transport::shared_client();
        let rb = build_openai_request(client, "http://h/v1/chat/completions", None, &json!({}));
        assert_eq!(request_url(rb), "http://h/v1/chat/completions");
    }
}

#[cfg(test)]
mod image_history_regressions {
    use super::*;
    #[test]
    fn text_followup_does_not_resend_a_previous_attachment_to_a_text_model() {
        let image = "![s](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)";
        let mut request = ChatRequest {
            model: "server-selected-model".into(),
            system: None,
            messages: vec![ChatMessage::user(image)],
        };
        assert_eq!(
            build_openai_body(&request)["messages"][0]["content"][1]["type"],
            "image_url"
        );
        request
            .messages
            .push(ChatMessage::assistant("A red pixel."));
        request
            .messages
            .push(ChatMessage::user("Continue using that answer."));
        for wire in [
            build_openai_body(&request),
            build_ollama_body(&request),
            build_anthropic_body(&request, 128),
        ] {
            let serialized = wire.to_string();
            assert!(!serialized.contains("base64"));
            assert!(!serialized.contains("image_url"));
            assert!(!serialized.contains("\"images\""));
            assert!(serialized.contains("embedded-image"));
            assert!(serialized.contains("A red pixel."));
        }
    }
}
