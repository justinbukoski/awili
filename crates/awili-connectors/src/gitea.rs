//! Typed blocking client for the Gitea (code hosting) HTTP API.
//!
//! Contract (per `skills/fleet/gitea/SKILL.md`): API base `<host>/api/v1`,
//! authenticated with `Authorization: token <TOKEN>` (the `token` scheme, NOT
//! `Bearer`). Repos are addressed as `{owner}/{repo}`. SSH clone URLs use a
//! *separate* ssh host/port (default 2222), never the HTTP API port.
//!
//! The decoders are pure and fixture-tested (offline); the socket wiring is a
//! thin blocking-reqwest layer proven against in-process loopback stubs in
//! `tests/loopback.rs`, never a live network.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{redact_and_truncate, ConnectorError};
use crate::secret::SecretRef;
use crate::url;
use crate::Timeouts;

/// A repo match from `GET /repos/search?q=`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct RepoSummary {
    /// The full repo path, e.g. `owner/repo`.
    pub full_name: String,
    /// The owner, decoded from Gitea's `User`/`Organization` object.
    pub owner: RepoOwner,
    /// The repo name.
    pub name: String,
    /// The repo description (may be empty).
    #[serde(default)]
    pub description: String,
    /// Whether the repo is a fork.
    #[serde(default)]
    pub fork: bool,
    /// The default branch name.
    #[serde(default)]
    pub default_branch: Option<String>,
}

/// The owner of a repo. Gitea's search results carry an `User` (or
/// `Organization`) object, not a bare string, so we decode the login from it.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct RepoOwner {
    /// The owner's login (username or org name).
    pub login: String,
}

/// A Gitea branch object (as returned by `GET /repos/{o}/{r}/branches/{name}`).
/// The head commit's id is exposed via [`Branch::head_commit_id`].
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Branch {
    /// The branch name.
    pub name: String,
    /// The head commit on this branch.
    pub commit: BranchCommit,
    /// The default branch for the repo.
    #[serde(default)]
    pub default_branch: Option<String>,
}

/// The head commit of a [`Branch`].
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct BranchCommit {
    /// The full commit SHA.
    pub id: String,
    /// The commit message (best-effort).
    #[serde(default)]
    pub message: Option<String>,
}

impl Branch {
    /// The head commit's full SHA (`.commit.id` on the wire).
    pub fn head_commit_id(&self) -> &str {
        &self.commit.id
    }
}

/// A Gitea deploy key (as returned by `POST /repos/{o}/{r}/keys`).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct DeployKey {
    /// The key's id.
    pub id: u64,
    /// The title the key was registered under.
    pub title: String,
    /// Whether the key is read-only.
    #[serde(default)]
    pub read_only: bool,
}

/// The SSH host/port pair used to build `ssh_clone_url`. Kept separate from
/// the HTTP API base because the fleet's Gitea sshd listens on a different
/// port (2222) than the HTTP API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GiteaSsh {
    /// The SSH host. Defaults to the API base's host.
    pub host: String,
    /// The SSH port. Defaults to 2222.
    pub port: u16,
}

impl GiteaSsh {
    /// The default SSH port the fleet Gitea sshd listens on.
    pub const DEFAULT_PORT: u16 = 2222;

    /// Derive an [`GiteaSsh`] from the API base URL's host, using
    /// [`Self::DEFAULT_PORT`].
    pub fn from_base_url(base_url: &str) -> Self {
        let host = url::host(base_url).unwrap_or_default();
        Self {
            host,
            port: Self::DEFAULT_PORT,
        }
    }
}

/// A blocking client for the Gitea API. One per configured base URL.
pub struct GiteaClient {
    base_url: String,
    secret: String,
    http: reqwest::blocking::Client,
    ssh: GiteaSsh,
}

impl fmt::Debug for GiteaClient {
    /// The token never prints. `base_url` and `ssh` are safe.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GiteaClient")
            .field("base_url", &self.base_url)
            .field("secret", &"<redacted>")
            .field("ssh", &self.ssh)
            .finish()
    }
}

/// The SSH clone URL `ssh://git@{host}:{port}/{owner}/{repo}.git`. Pure; no
/// network, no secret. RC2-SVC3 (F0413): `owner` and `repo` must each be one
/// safe path component (ASCII letters, digits, `.`, `_`, `-`; not `.`/`..`),
/// the same rule every authenticated Gitea call applies — a value carrying
/// `/`, `@`, `:` or a space would otherwise reshape the URL.
pub fn ssh_clone_url(ssh: &GiteaSsh, owner: &str, repo: &str) -> Result<String, ConnectorError> {
    let owner = url::repo_component(owner, "owner")?;
    let repo = url::repo_component(repo, "repo")?;
    Ok(format!(
        "ssh://git@{}:{}/{}/{}.git",
        ssh.host, ssh.port, owner, repo
    ))
}

/// Gitea's `{"ok":bool,"data":…}` response envelope. The client unwraps
/// `data`; a `false` `ok` is a decode error (the HTTP status was already 2xx).
#[derive(Deserialize)]
struct GiteaEnvelope<T> {
    ok: bool,
    data: T,
}

impl GiteaClient {
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
        let base = base_url.into().trim_end_matches('/').to_string();
        let token = secret.resolve()?.as_str().to_string();
        let http = reqwest::blocking::Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.request)
            .build()
            .map_err(ConnectorError::Builder)?;
        Ok(Self {
            ssh: GiteaSsh::from_base_url(&base),
            base_url: base,
            secret: token,
            http,
        })
    }

    /// Override the SSH host/port used by [`GiteaClient::ssh_clone_url`].
    pub fn with_ssh(mut self, ssh: GiteaSsh) -> Self {
        self.ssh = ssh;
        self
    }

    /// The base URL this client targets (no trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The SSH host/port pair used by [`GiteaClient::ssh_clone_url`].
    pub fn ssh(&self) -> &GiteaSsh {
        &self.ssh
    }

    /// `GET /repos/search?q=` → the matching repos.
    ///
    /// Gitea wraps the response in `{"ok":true,"data":[...]}`; the client
    /// unwraps `data`. A missing/`false` `ok` is a decode error, not a status
    /// error (the HTTP status was already 2xx).
    pub fn repo_search(&self, q: &str) -> Result<Vec<RepoSummary>, ConnectorError> {
        let url = format!("{}/repos/search?q={}", self.base_url, url::segment(q));
        let body = self.get(&url)?;
        let env: GiteaEnvelope<Vec<RepoSummary>> =
            serde_json::from_str(&body).map_err(|e| ConnectorError::Decode(e.to_string()))?;
        if !env.ok {
            return Err(ConnectorError::Decode(
                "gitea repo_search returned ok:false".to_string(),
            ));
        }
        Ok(env.data)
    }

    /// `GET /repos/{o}/{r}/branches/{name}` → the branch with its head commit.
    pub fn branch(&self, owner: &str, repo: &str, name: &str) -> Result<Branch, ConnectorError> {
        let owner = url::repo_component(owner, "owner")?;
        let repo = url::repo_component(repo, "repo")?;
        let name = url::repo_path(name)?;
        let url = format!(
            "{}/repos/{}/{}/branches/{}",
            self.base_url, owner, repo, name
        );
        let body = self.get(&url)?;
        serde_json::from_str(&body).map_err(|e| ConnectorError::Decode(e.to_string()))
    }

    /// `GET /repos/{o}/{r}/raw/{path}` → the file body verbatim. `path` starts
    /// at the repo root (no leading `/`, no double `/raw/`). `ref` is optional.
    /// The path is validated BEFORE any request is built: dot segments
    /// (`.`/`..`, empty, or their percent-encoded forms) are refused locally so
    /// an authenticated request can never escape the repo root on the server.
    pub fn raw_file(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        r#ref: Option<&str>,
    ) -> Result<String, ConnectorError> {
        let owner = url::repo_component(owner, "owner")?;
        let repo = url::repo_component(repo, "repo")?;
        let p = url::repo_path(path)?;
        let mut url = format!("{}/repos/{}/{}/raw/{}", self.base_url, owner, repo, p);
        if let Some(ref_name) = r#ref {
            url.push_str(&format!("?ref={}", url::segment(ref_name)));
        }
        self.get(&url)
    }

    /// `POST /repos/{o}/{r}/keys` → the created deploy key. The request always
    /// sends `"read_only": true` explicitly — a deploy key is never created
    /// with write access by default. If the server reports the key's
    /// `read_only` field and it is `false`, the result is rejected: we must
    /// never treat a writable key as ours to use for pulls.
    pub fn add_deploy_key(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        key: &str,
    ) -> Result<DeployKey, ConnectorError> {
        let owner = url::repo_component(owner, "owner")?;
        let repo = url::repo_component(repo, "repo")?;
        let url = format!("{}/repos/{}/{}/keys", self.base_url, owner, repo);
        let body = self.post(
            &url,
            &serde_json::json!({ "title": title, "key": key, "read_only": true }),
        )?;
        let dk: DeployKey =
            serde_json::from_str(&body).map_err(|e| ConnectorError::Decode(e.to_string()))?;
        if !dk.read_only {
            return Err(ConnectorError::Decode(format!(
                "deploy key {} was created writable (read_only=false); refusing to use it",
                dk.id
            )));
        }
        Ok(dk)
    }

    /// Build the SSH clone URL for `{owner}/{repo}` using this client's
    /// configured SSH host/port. Pure; no network. RC2-SVC3 (F0413): owner
    /// and repo must each be one safe path component (see [`ssh_clone_url`]).
    pub fn ssh_clone_url(&self, owner: &str, repo: &str) -> Result<String, ConnectorError> {
        ssh_clone_url(&self.ssh, owner, repo)
    }

    /// Send an authenticated GET. Returns the 2xx response body verbatim.
    fn get(&self, url: &str) -> Result<String, ConnectorError> {
        let resp = self
            .http
            .get(url)
            .header("Authorization", format!("token {}", self.secret))
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

    /// Send an authenticated POST with a JSON body. Returns the 2xx response
    /// body verbatim.
    fn post(&self, url: &str, body: &serde_json::Value) -> Result<String, ConnectorError> {
        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("token {}", self.secret))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_clone_url_shape() {
        // 198.51.100.7 is RFC 5737 TEST-NET-2 — reserved for documentation,
        // unambiguously synthetic.
        let ssh = GiteaSsh {
            host: "198.51.100.7".to_string(),
            port: 2222,
        };
        let client = GiteaClient {
            base_url: "http://198.51.100.7:3000/api/v1".to_string(),
            secret: "synthetic-token-for-unit-test".to_string(),
            http: reqwest::blocking::Client::new(),
            ssh,
        };
        assert_eq!(
            client.ssh_clone_url("testuser", "awili").unwrap(),
            "ssh://git@198.51.100.7:2222/testuser/awili.git"
        );
    }

    /// RC2-SVC3 (F0413): an owner or repo that is not one safe path
    /// component is refused; nothing is formatted into the URL.
    #[test]
    fn rc2svc3_ssh_clone_url_refuses_unsafe_components() {
        let ssh = GiteaSsh {
            host: "198.51.100.7".to_string(),
            port: 2222,
        };
        for (owner, repo) in [
            ("evil.example@x", "r"),
            ("o", "r/../../x"),
            ("o", "r x"),
            ("..", "r"),
            ("", "r"),
            ("o:1", "r"),
        ] {
            assert!(
                ssh_clone_url(&ssh, owner, repo).is_err(),
                "{owner:?}/{repo:?} must be refused"
            );
        }
        assert_eq!(
            ssh_clone_url(&ssh, "owner", "awili.rs-2").unwrap(),
            "ssh://git@198.51.100.7:2222/owner/awili.rs-2.git"
        );
    }

    #[test]
    fn repo_owner_decodes_from_user_object() {
        let env: GiteaEnvelope<Vec<RepoSummary>> = serde_json::from_str(
            r#"{"ok":true,"data":[{"full_name":"testuser/awili","owner":{"login":"testuser"},"name":"awili","description":"the awili app","fork":false,"default_branch":"main"}]}"#,
        )
        .expect("valid gitea envelope");
        assert!(env.ok);
        assert_eq!(env.data[0].owner.login, "testuser");
        assert_eq!(env.data[0].full_name, "testuser/awili");
    }

    #[test]
    fn repo_search_env_ok_false_is_decode_error() {
        let body = r#"{"ok":false,"data":[]}"#;
        let env: Result<GiteaEnvelope<Vec<RepoSummary>>, _> = serde_json::from_str(body);
        match env {
            Ok(e) => assert!(!e.ok),
            Err(_) => panic!("ok:false body should still deserialize"),
        }
    }

    // --- S6(a) loopback proof tests ---------------------------------------
    //
    // The socket wiring for `raw_file` and `add_deploy_key` is proven against
    // in-process loopback stubs (a bare TCP listener answering one canned
    // response per request), mirroring the pattern in `tests/loopback.rs`.

    /// One captured HTTP request as seen by the stub. `method`, `path` and
    /// `auth` are recorded for wire-shape assertions; `body` is the field the
    /// S6(a) proofs check (the explicit `"read_only": true`).
    #[derive(Debug, Clone)]
    #[allow(dead_code)] // fields asserted on as a set by the proof tests
    struct Captured {
        method: String,
        path: String,
        body: String,
        auth: String,
    }

    type Capture = std::sync::Arc<std::sync::Mutex<Vec<Captured>>>;

    /// Read the first HTTP request off `stream` (headers + Content-Length body).
    fn read_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
            if let Some(h) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&data[..h]).into_owned();
                let cl = head
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')?
                            .0
                            .trim()
                            .eq_ignore_ascii_case("content-length")
                            .then(|| l.split_once(':').unwrap().1.trim())
                    })
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                if data.len() >= h + 4 + cl {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    /// A loopback stub that answers every request with the same canned
    /// `(status, content_type, body)`, capturing each request for assertions.
    fn spawn_stub(status: &str, ctype: &str, resp_body: &str) -> (String, Capture) {
        use std::io::Write;
        let capture: Capture = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let cap = std::sync::Arc::clone(&capture);
        let status = status.to_string();
        let ctype = ctype.to_string();
        let resp_body = resp_body.to_string();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let c = std::sync::Arc::clone(&cap);
                // Clone the canned response per connection: the inner closure
                // is `move`, so each one needs its own copy of the strings.
                let (status, ctype, resp_body) = (status.clone(), ctype.clone(), resp_body.clone());
                std::thread::spawn(move || {
                    let mut stream = s;
                    let req = read_request(&mut stream);
                    let mut parts = req.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("/").to_string();
                    let auth = req
                        .lines()
                        .find_map(|l| {
                            l.split_once(':')?
                                .0
                                .trim()
                                .eq_ignore_ascii_case("authorization")
                                .then(|| l.split_once(':').unwrap().1.trim())
                        })
                        .unwrap_or("")
                        .to_string();
                    let body = req
                        .split_once("\r\n\r\n")
                        .map(|(_, b)| b.to_string())
                        .unwrap_or_default();
                    c.lock().unwrap().push(Captured {
                        method,
                        path,
                        body,
                        auth,
                    });
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                        resp_body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        (format!("http://{addr}"), capture)
    }

    /// A `GiteaClient` pointed at `base`, using a synthetic token written to a
    /// temp file (deleted on drop). `SecretRef::File` is the only variant that
    /// does not touch the process environment.
    fn gitea_client(base: &str) -> GiteaClient {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("awili-gitea-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        // A UNIQUE token file per client: parallel tests never truncate each
        // other's file mid-read (the old per-PID name collided).
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(format!("token-{}-{n}.txt", std::process::id()));
        {
            let mut f = std::fs::File::create_new(&path).expect("create token file");
            f.write_all(b"test-gitea-secret-dbg").expect("write token");
        }
        GiteaClient::new(
            base,
            SecretRef::File {
                path: path.clone(),
                key: None,
            },
            Timeouts::default(),
        )
        .expect("gitea client")
    }

    /// `raw_file` with a dot-segment path fails BEFORE any HTTP request is
    /// sent: the stub never sees a request, and the error names the cause.
    #[test]
    fn raw_file_rejects_dot_segments_without_http() {
        let (base, capture) = spawn_stub("200 OK", "text/plain", "file body");
        let client = gitea_client(&base);

        // Every dot/empty-segment path is refused locally, no request sent.
        for bad in [
            ".",
            "..",
            "a/../b",
            "a/./b",
            "/../x",
            "%2e",
            "%2E%2E",
            "a/%2e%2e/b",
            "%2e%2e/x",
            "",
            "/",
            "//",
            "///",
            "a//b",
            "a/",
        ] {
            let err = client
                .raw_file("testuser", "awili", bad, None)
                .expect_err("dot/empty segment must be rejected locally");
            assert!(!err.to_string().is_empty(), "error names the cause: {err}");
        }
        let reqs = capture.lock().unwrap().clone();
        assert_eq!(
            reqs.len(),
            0,
            "no authenticated request may leave the process"
        );

        // Valid paths go through `raw_file` onto the wire exactly once each,
        // percent-encoded and authenticated with the `token` scheme.
        assert_eq!(
            client
                .raw_file("testuser", "awili", "src/lib.rs", None)
                .unwrap(),
            "file body"
        );
        assert_eq!(
            client
                .raw_file("testuser", "awili", "a/b c.txt", None)
                .unwrap(),
            "file body"
        );
        let reqs = capture.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2, "exactly one request per valid path");
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].path, "/repos/testuser/awili/raw/src/lib.rs");
        assert!(
            reqs[0].auth.starts_with("token "),
            "authenticated with the token scheme: {:?}",
            reqs[0].auth
        );
        assert_eq!(reqs[1].method, "GET");
        assert_eq!(reqs[1].path, "/repos/testuser/awili/raw/a/b%20c.txt");
        assert!(reqs[1].auth.starts_with("token "));
    }

    /// S6(a) F1 proof: a model-supplied `owner` or `repo` is validated as ONE
    /// safe path component BEFORE any request is built. Every bad value is
    /// refused on every client method with ZERO requests on the wire; the
    /// allowed name set is used verbatim on the wire.
    #[test]
    fn raw_file_rejects_unsafe_owner_and_repo_without_http() {
        let (base, capture) = spawn_stub("200 OK", "text/plain", "file body");
        let client = gitea_client(&base);

        for bad in [
            "", ".", "..", "a/b", "../admin", "%2e%2e", "a%2Fb", "a\\b", "a b", "a?b", "a#b", "ünï",
        ] {
            for result in [
                client.raw_file(bad, "awili", "src/lib.rs", None).is_err(),
                client
                    .raw_file("testuser", bad, "src/lib.rs", None)
                    .is_err(),
                client.add_deploy_key(bad, "awili", "t", "k").is_err(),
                client.add_deploy_key("testuser", bad, "t", "k").is_err(),
                client.branch(bad, "awili", "main").is_err(),
                client.branch("testuser", bad, "main").is_err(),
            ] {
                assert!(result, "{bad:?} must be refused locally");
            }
        }
        // Branch names may contain `/` but never dot/empty segments.
        assert!(client.branch("testuser", "awili", "../x").is_err());
        assert!(client.branch("testuser", "awili", "a//b").is_err());

        let reqs = capture.lock().unwrap().clone();
        assert_eq!(
            reqs.len(),
            0,
            "no authenticated request may leave the process"
        );

        // Valid names go on the wire verbatim — no re-encoding needed because
        // the allowlist is percent-safe.
        assert_eq!(
            client
                .raw_file("test-user.name_1", "awili", "src/lib.rs", None)
                .unwrap(),
            "file body"
        );
        let _ = client.branch("testuser", "awili", "feat/x");
        let reqs = capture.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2, "exactly the two valid requests were sent");
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].path, "/repos/test-user.name_1/awili/raw/src/lib.rs");
        assert_eq!(reqs[1].method, "GET");
        assert_eq!(reqs[1].path, "/repos/testuser/awili/branches/feat/x");
    }

    /// `add_deploy_key` explicitly sends `"read_only": true`, and a server that
    /// reports the key as writable is refused.
    #[test]
    fn deploy_key_request_is_read_only() {
        let (base, capture) = spawn_stub(
            "201 Created",
            "application/json",
            r#"{"id":7,"title":"awili-deploy-key","key":"ssh-ed25519 AAAAsynthetickeydata deploy@awili","read_only":true}"#,
        );
        let client = gitea_client(&base);
        let key = client
            .add_deploy_key(
                "testuser",
                "awili",
                "awili-deploy-key",
                "ssh-ed25519 AAAAsynthetickeydata deploy@awili",
            )
            .expect("add_deploy_key");
        assert!(key.read_only, "the stub reports read_only=true; accepted");
        let reqs = capture.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&reqs[0].body).expect("json");
        assert_eq!(body["read_only"], true, "read_only must be sent explicitly");
        assert_eq!(body["title"], "awili-deploy-key");
        assert_eq!(body["key"], "ssh-ed25519 AAAAsynthetickeydata deploy@awili");

        // A server that reports the key as writable must be refused.
        let (base2, _cap2) = spawn_stub(
            "201 Created",
            "application/json",
            r#"{"id":8,"title":"writable-attempt","key":"ssh-ed25519 AAAAsynthetickeydata deploy@awili","read_only":false}"#,
        );
        let client2 = gitea_client(&base2);
        let err = client2
            .add_deploy_key(
                "testuser",
                "awili",
                "writable-attempt",
                "ssh-ed25519 AAAAsynthetickeydata deploy@awili",
            )
            .expect_err("a writable reported key must be rejected");
        assert!(
            err.to_string().contains("read_only=false"),
            "rejection names the cause: {err}"
        );
    }
}
