//! `awili-connectors` — typed blocking clients for the two fleet services the
//! Awili daily driver talks to over plain HTTP:
//!
//! - **Outline** (notes/knowledge base): [`OutlineClient`], bearer-token auth,
//!   JSON POST verbs (`documents.info`, `documents.search`, `documents.create`,
//!   `documents.update` with `append: true`).
//! - **Gitea** (code hosting): [`GiteaClient`], `token`-scheme auth, repo
//!   search, branch head commit, raw file fetch, deploy-key install, and the
//!   pure `ssh_clone_url` helper (SSH runs on the separate ssh port, never the
//!   HTTP port).
//!
//! Secrets are held as [`SecretRef`]s (a file path with an optional key line,
//! or an environment variable name) and resolved lazily by each client's
//! `new()`. The resolved [`Secret`] never renders its value in `Debug`, and no
//! error variant of [`ConnectorError`] can echo a secret value — only the path
//! or key name that failed, plus a capped body excerpt for non-2xx responses.
//!
//! The decoders are pure and fixture-tested (offline); the socket wiring is a
//! thin blocking-reqwest layer proven against in-process loopback stubs in
//! `tests/loopback.rs`, never a live network.

#![forbid(unsafe_code)]

mod error;
mod gitea;
mod outline;
mod secret;
mod url;

pub use error::ConnectorError;
pub use gitea::{ssh_clone_url, Branch, DeployKey, GiteaClient, GiteaSsh, RepoSummary};
pub use outline::{Document, OutlineClient, SearchHit};
pub use secret::{Secret, SecretRef};

/// Per-connector request timeouts: connect (TCP) and the whole request.
/// Defaults: 800 ms connect, 10 s request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Max time to establish the TCP connection.
    pub connect: std::time::Duration,
    /// Max time for the whole request (connect + send + full response body).
    pub request: std::time::Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: std::time::Duration::from_millis(800),
            request: std::time::Duration::from_secs(10),
        }
    }
}
