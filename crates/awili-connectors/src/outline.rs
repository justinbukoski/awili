//! Typed blocking client for the Outline (notes/knowledge base) HTTP API.
//!
//! Contract (per `skills/fleet/outline/SKILL.md`): base `<host>/api`, every
//! verb is a JSON `POST` to a dotted path (`/documents.info`, `/documents.search`,
//! `/documents.create`, `/documents.update`), authenticated with
//! `Authorization: Bearer <key>`.
//!
//! The decoders are pure and fixture-tested (offline); the socket wiring is a
//! thin blocking-reqwest layer proven against in-process loopback stubs in
//! `tests/loopback.rs`, never a live network.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{redact_and_truncate, ConnectorError};
use crate::secret::SecretRef;
use crate::Timeouts;

/// Outline's `{"ok":bool,"data":…}` response envelope. `documents.info`,
/// `documents.create`, and `documents.update` all wrap the document in this
/// envelope; the client unwraps `data`. A missing/`false` `ok` is a decode
/// error (the HTTP status was already 2xx). `documents.search` returns a bare
/// `{"data":[…]}` body with no `ok` flag — it is decoded by
/// [`decode_search_hits`], not by this envelope.
#[derive(Deserialize)]
struct OutlineEnvelope<T> {
    ok: bool,
    data: T,
}

/// A resolved Outline document, as returned by `documents.info`,
/// `documents.create`, and `documents.update`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Document {
    /// The document's id.
    pub id: String,
    /// The document's title.
    pub title: String,
    /// The document's Markdown body (empty for a bare `documents.info`).
    #[serde(default)]
    pub text: String,
    /// The document's current revision number.
    #[serde(default)]
    pub revision: Option<u64>,
    /// The document's URL.
    #[serde(default)]
    pub url: Option<String>,
}

/// One hit from `documents.search`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SearchHit {
    /// The document id.
    pub id: String,
    /// The document title.
    pub title: String,
    /// The document URL (often relative to the Outline host).
    #[serde(default)]
    pub url: Option<String>,
}

/// A blocking client for the Outline API. One per configured base URL.
pub struct OutlineClient {
    base_url: String,
    secret: String,
    http: reqwest::blocking::Client,
}

impl fmt::Debug for OutlineClient {
    /// The bearer token never prints. `base_url` is safe.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutlineClient")
            .field("base_url", &self.base_url)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl OutlineClient {
    /// Create a client for the given base URL, resolving `secret` up-front.
    ///
    /// The returned error is [`ConnectorError::Secret`] (a failed secret
    /// ref) or [`ConnectorError::Builder`] (a failed client construction);
    /// neither ever names a secret value.
    pub fn new(
        base_url: impl Into<String>,
        secret: SecretRef,
        timeouts: Timeouts,
    ) -> Result<Self, ConnectorError> {
        let token = secret.resolve()?.as_str().to_string();
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.request)
            .build()
            .map_err(ConnectorError::Builder)?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            secret: token,
            http,
        })
    }

    /// The base URL this client targets (no trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `POST /documents.info` `{"id"}` → the document.
    ///
    /// Unwraps the `{"ok":true,"data":{…}}` envelope; a missing/`false` `ok`
    /// is a decode error.
    pub fn document_info(&self, id: &str) -> Result<Document, ConnectorError> {
        let body = self.post("/documents.info", &serde_json::json!({ "id": id }))?;
        let env: OutlineEnvelope<Document> =
            serde_json::from_str(&body).map_err(|e| ConnectorError::Decode(e.to_string()))?;
        if !env.ok {
            return Err(ConnectorError::Decode(
                "outline documents.info returned ok:false".to_string(),
            ));
        }
        Ok(env.data)
    }

    /// `POST /documents.search` `{"query","limit"}` → the hits.
    ///
    /// Decodes `.data[].document.{id,title,url}` exactly — NOT `.data[].id`
    /// (a classic regression this client must not repeat).
    pub fn document_search(
        &self,
        query: &str,
        limit: u32,
    ) -> Result<Vec<SearchHit>, ConnectorError> {
        let body = self.post(
            "/documents.search",
            &serde_json::json!({ "query": query, "limit": limit }),
        )?;
        decode_search_hits(&body)
    }

    /// `POST /documents.create` `{"title","text","collectionId","publish"}` →
    /// the created document.
    ///
    /// Unwraps the `{"ok":true,"data":{…}}` envelope.
    pub fn document_create(
        &self,
        title: &str,
        text: &str,
        collection_id: &str,
        publish: bool,
    ) -> Result<Document, ConnectorError> {
        let body = self.post(
            "/documents.create",
            &serde_json::json!({
                "title": title,
                "text": text,
                "collectionId": collection_id,
                "publish": publish,
            }),
        )?;
        let env: OutlineEnvelope<Document> =
            serde_json::from_str(&body).map_err(|e| ConnectorError::Decode(e.to_string()))?;
        if !env.ok {
            return Err(ConnectorError::Decode(
                "outline documents.create returned ok:false".to_string(),
            ));
        }
        Ok(env.data)
    }

    /// `POST /documents.update` `{"id","text","append": true}` → the updated
    /// document, including its new `revision`.
    ///
    /// Unwraps the `{"ok":true,"data":{…}}` envelope.
    pub fn document_append(&self, id: &str, text: &str) -> Result<Document, ConnectorError> {
        let body = self.post(
            "/documents.update",
            &serde_json::json!({ "id": id, "text": text, "append": true }),
        )?;
        let env: OutlineEnvelope<Document> =
            serde_json::from_str(&body).map_err(|e| ConnectorError::Decode(e.to_string()))?;
        if !env.ok {
            return Err(ConnectorError::Decode(
                "outline documents.update returned ok:false".to_string(),
            ));
        }
        Ok(env.data)
    }

    /// Send a JSON POST to the given path with the bearer header. Returns the
    /// 2xx response body verbatim. Non-2xx becomes [`ConnectorError::Status`].
    fn post(&self, path: &str, body: &serde_json::Value) -> Result<String, ConnectorError> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.secret))
            .json(body)
            .send()
            .map_err(ConnectorError::Http)?;
        let status = resp.status();
        let text = resp.text().map_err(ConnectorError::Http)?;
        if !status.is_success() {
            return Err(ConnectorError::Status {
                code: status.as_u16(),
                body_excerpt: redact_and_truncate(&text, &self.secret),
            });
        }
        Ok(text)
    }
}

/// Decode a `documents.search` body into hits. Pure; fixture-tested.
///
/// Outline nests the document inside each `.data` item: the shape is
/// `.data[].document.{id,title,url}`, NOT `.data[].id`.
pub fn decode_search_hits(body: &str) -> Result<Vec<SearchHit>, ConnectorError> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| ConnectorError::Decode(e.to_string()))?;
    let data = v.get("data").and_then(|d| d.as_array());
    let Some(data) = data else {
        return Err(ConnectorError::Decode(
            "missing `data` array in search response".to_string(),
        ));
    };
    let mut hits = Vec::with_capacity(data.len());
    for (i, item) in data.iter().enumerate() {
        let doc = item.get("document").ok_or_else(|| {
            ConnectorError::Decode(format!("data[{i}] missing `document` object"))
        })?;
        let id = doc
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConnectorError::Decode(format!("data[{i}].document.id missing")))?
            .to_string();
        let title = doc
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ConnectorError::Decode(format!("data[{i}].document.title missing")))?
            .to_string();
        let url = doc.get("url").and_then(|v| v.as_str()).map(str::to_string);
        hits.push(SearchHit { id, title, url });
    }
    Ok(hits)
}
