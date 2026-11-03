//! `awili-config` — the on-disk configuration for Awili: provider endpoints, the
//! cycle-able model list, and the memory base URL.
//!
//! Loaded from `~/.config/awili/config.toml` (slice-2 spec §4). The file is
//! optional: [`Config::load`] falls back to [`Config::default`] when it is absent
//! or unreadable, so a fresh checkout runs against localhost Ollama + the Foreman
//! memory service with no setup. Parsing is pure and lives here (no GPUI, no
//! network) so the defaults and the TOML shape are unit-testable in isolation.

use std::collections::BTreeMap;
#[cfg(not(windows))]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

pub mod appearance;
pub use appearance::{AppearanceConfig, ColorMode};
pub mod codex_plan;
pub mod doc;
pub use doc::{save_validated, ConfigDoc, SettingEntry, Target, ValueKind};
pub mod secrets;
pub use secrets::{OsSecretStore, SecretStore};

/// Name of Awili's folder under the platform data directory: `Awili` on macOS
/// and Windows (unchanged), `awili` on Linux (XDG convention, so
/// `$XDG_DATA_HOME/awili`).
pub const APP_DIR_NAME: &str = if cfg!(target_os = "linux") {
    "awili"
} else {
    "Awili"
};

/// Awili's data folder (`<platform data dir>/<APP_DIR_NAME>`), if the platform
/// has a data directory.
pub fn app_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(APP_DIR_NAME))
}

/// `create_dir_all` that keeps Awili's own folders private on Linux: folders it
/// creates under the data or config directory are mode 0700, and an existing app folder
/// (`awili` directly under the data or config directory, anywhere along `path`) that is group- or world-accessible is
/// tightened to 0700. Other folders along the way (`$HOME`, `/tmp`, ...) are
/// never touched. Elsewhere this is plain `create_dir_all`.
pub fn ensure_private_app_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let roots: Vec<PathBuf> = [dirs::data_dir(), dirs::config_dir()]
            .into_iter()
            .flatten()
            .collect();
        private_dir_under(path, &roots)
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::fs::create_dir_all(path)
    }
}

/// [`ensure_private_app_dir`] with the roots explicit. A path outside every root
/// (a folder the person chose, such as an export target) is plain `create_dir_all`.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn private_dir_under(path: &Path, roots: &[PathBuf]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        // A `..` component could escape a root while still matching it by prefix.
        let has_parent_dir = path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        if has_parent_dir || !roots.iter().any(|r| path.starts_with(r)) {
            return std::fs::create_dir_all(path);
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        for dir in path.ancestors() {
            let ours = dir.file_name().is_some_and(|n| n == APP_DIR_NAME)
                && dir.parent().is_some_and(|p| roots.iter().any(|r| r == p));
            if ours {
                let meta = std::fs::metadata(dir)?;
                if meta.is_dir() && meta.permissions().mode() & 0o077 != 0 {
                    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
                }
                break;
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = roots;
        std::fs::create_dir_all(path)
    }
}

/// Secret-file permission bits: owner read/write only (S6(a)).
pub(crate) const SECRET_FILE_MODE: u32 = 0o600;

/// The mode a config write must settle on: the STRICTER of `SECRET_FILE_MODE`
/// and whatever the existing file already is. A pre-existing 0400 (read-only)
/// or 0440 file is never loosened by a save; a looser one (e.g. 0644) is
/// tightened to 0600. Returns `None` when there is no existing file, in which
/// case the write uses `SECRET_FILE_MODE`.
#[cfg(unix)]
pub(crate) fn effective_config_mode(path: &Path) -> Option<u32> {
    let meta = std::fs::metadata(path).ok()?;
    Some(SECRET_FILE_MODE & meta.mode())
}

/// Non-Unix twin: there is no permission mode to reason about, so the write
/// falls back to `SECRET_FILE_MODE` (`let _ = mode;` at the call site).
#[cfg(not(unix))]
pub(crate) fn effective_config_mode(_path: &Path) -> Option<u32> {
    None
}

/// Per-process counter for `.tmp-` temp names (S6(a)): the PID alone would let
/// two writers racing inside the SAME process pick the same name; the atomic
/// counter keeps every name in the process unique.
static PUBLISH_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Why the last loaded config file could not be made private to the account, if
/// it could not (Windows). `None` on every other platform and when it could.
static FILE_PROTECTION_PROBLEM: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The banner line for the config file the app is running on not being private
/// to the account, when that is so. While it is set, plaintext API keys read
/// from that file are not used.
pub fn file_protection_problem() -> Option<String> {
    FILE_PROTECTION_PROBLEM
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Record (or clear) the problem [`file_protection_problem`] reports.
#[doc(hidden)]
pub fn set_file_protection_problem(problem: Option<String>) {
    *FILE_PROTECTION_PROBLEM
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = problem;
}

/// Windows: make a loaded config owner-only, or say why not. Elsewhere there is
/// nothing to do (Unix files are created 0600 and are not re-checked here).
#[cfg(windows)]
fn protect_loaded(path: &Path) -> Option<String> {
    #[cfg(test)]
    if protect_seam::armed() {
        return Some(protection_line(path, "forced failure (test seam)"));
    }
    awili_winacl::ensure_owner_only(path, false)
        .err()
        .map(|e| protection_line(path, &e.to_string()))
}

#[cfg(not(windows))]
fn protect_loaded(_path: &Path) -> Option<String> {
    None
}

#[cfg(windows)]
fn protection_line(path: &Path, why: &str) -> String {
    format!(
        "{} could not be made private to your account ({why}). API keys written directly in it are not used until that is fixed; keys saved in the Windows credential store still work.",
        path.display()
    )
}

/// Test seam: force [`protect_loaded`] to fail on this thread.
#[cfg(all(test, windows))]
mod protect_seam {
    use std::cell::Cell;
    thread_local! { static ON: Cell<bool> = const { Cell::new(false) }; }
    pub(super) fn set(v: bool) {
        ON.with(|c| c.set(v));
    }
    pub(super) fn armed() -> bool {
        ON.with(|c| c.get())
    }
}

/// The error for a save that could not be made private to the account (Windows).
/// A volume without file permissions already carries the plain NTFS message.
#[cfg(windows)]
fn private_save_error(path: &Path, e: std::io::Error) -> std::io::Error {
    let msg = if e.kind() == std::io::ErrorKind::Unsupported {
        format!("{e}. Nothing was saved.")
    } else {
        format!(
            "could not make {} private to your Windows account, so nothing was saved: {e}",
            path.display()
        )
    };
    std::io::Error::new(e.kind(), msg)
}

/// Write `bytes` to a UNIQUE temp file next to `path` created at `mode`,
/// fsync it, then rename it over `path` (atomic on POSIX). On every failure
/// the temp is removed and the original `path` is left untouched (S6(a)).
pub(crate) fn write_bytes_and_publish(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), ConfigError> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    loop {
        let n = PUBLISH_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut tmp_name = name.clone();
        tmp_name.push(format!(".tmp-{}-{}", std::process::id(), n));
        let tmp = dir.join(&tmp_name);
        // Windows: the equivalent of mode 0600. The file is CREATED with the
        // owner-only protected descriptor (no instant with a broader list) and
        // read back, before any secret is written; the rename below carries
        // it to the published file. Fail closed: if that cannot be done,
        // nothing is written and the save errors with the plain reason.
        #[cfg(windows)]
        let created = awili_winacl::create_new_file_owner_only(&tmp);
        #[cfg(not(windows))]
        let created = {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(mode);
            }
            options.open(&tmp)
        };
        #[cfg(windows)]
        let _ = mode;
        let mut file = match created {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // The name collided with a foreign temp; pick another. Never
                // delete a foreign file.
                continue;
            }
            #[cfg(windows)]
            Err(e) => return Err(ConfigError::Io(private_save_error(path, e))),
            #[cfg(not(windows))]
            Err(e) => return Err(ConfigError::Io(e)),
        };
        let result = (|| -> std::io::Result<()> {
            use std::io::Write;
            file.write_all(bytes)?;
            file.sync_all()
        })();
        drop(file);
        if let Err(e) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(ConfigError::Io(e));
        }
        return match publish_rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(ConfigError::Io(e))
            }
        };
    }
}

/// Write `text` to a UNIQUE temp file next to `path`, fsync it, then rename it
/// over `path`. The temp is created 0600 (or the stricter existing mode, when
/// `path` already exists with one) so the config's secret fields never exist
/// on disk under broader permissions, even for a split second (S6(a)).
pub(crate) fn write_temp_and_publish(path: &Path, text: &str) -> Result<(), ConfigError> {
    write_bytes_and_publish(
        path,
        text.as_bytes(),
        effective_config_mode(path).unwrap_or(SECRET_FILE_MODE),
    )
}

/// The rename that publishes a fully-written temp over its target (S6(a)).
/// In tests the seam can force this one step to fail while the original file
/// stays in place — the ONLY supported way to prove a failed publish leaves
/// the previous bytes and mode intact.
fn publish_rename(tmp: &Path, path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if publish_seam::armed() {
        return Err(std::io::Error::other("forced publish failure (test seam)"));
    }
    std::fs::rename(tmp, path)
}

/// Test seam (S6(a)): while `FailPublishes` is held, [`publish_rename`]
/// fails. Thread-local, so parallel tests on other threads are unaffected;
/// dropping the guard disarms the seam.
#[cfg(test)]
pub(crate) mod publish_seam {
    use std::cell::Cell;
    use std::thread_local;

    thread_local! {
        static ARMED: Cell<bool> = const { Cell::new(false) };
    }

    /// Armed while held; automatically disarmed when dropped.
    pub(crate) struct FailPublishes;

    impl FailPublishes {
        /// Arm the seam for the current thread until the returned guard
        /// is dropped.
        pub(crate) fn arm() -> Self {
            ARMED.with(|a| a.set(true));
            Self
        }
    }

    impl Drop for FailPublishes {
        fn drop(&mut self) {
            ARMED.with(|a| a.set(false));
        }
    }

    /// Whether the seam is armed on the current thread.
    pub(crate) fn armed() -> bool {
        ARMED.with(|a| a.get())
    }
}

/// Which wire protocol a provider speaks. Mirrors the `kind = "..."` field in the
/// TOML and selects the [`awili_providers`](../awili_providers) impl at wire-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Local Ollama (`/api/chat`, newline-delimited JSON).
    Ollama,
    /// Anthropic Messages API (SSE).
    Anthropic,
    /// An Anthropic-compatible endpoint hosted elsewhere (e.g. Hermes), same SSE
    /// shape as Anthropic but a configurable `base_url`.
    Hermes,
    /// A plain OpenAI-compatible Chat Completions endpoint (`/v1/chat/completions`)
    /// — vLLM, llama.cpp, DeepSeek, etc. A bare OpenAI-shaped endpoint, NOT a
    /// Hermes gateway.
    #[serde(rename = "openai", alias = "openai-compatible")]
    OpenAi,
}

impl ProviderKind {
    /// Every kind, in the order the settings panel's kind chip cycles them.
    pub const ALL: [ProviderKind; 4] = [
        ProviderKind::Ollama,
        ProviderKind::Anthropic,
        ProviderKind::Hermes,
        ProviderKind::OpenAi,
    ];

    /// The TOML spelling of this kind (`kind = "..."`).
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Ollama => "ollama",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Hermes => "hermes",
            ProviderKind::OpenAi => "openai",
        }
    }
}

fn default_true() -> bool {
    true
}

/// Render a secret for `Debug`: presence stays observable, the value never
/// prints. H4 — a debug-logged config must not disclose keys/tokens.
fn redact_opt_secret(v: &Option<String>) -> &'static str {
    match v {
        Some(s) if !s.is_empty() => "Some(<redacted>)",
        Some(_) => "Some(\"\")",
        None => "None",
    }
}

fn redact_str_secret(v: &str) -> &'static str {
    if v.is_empty() {
        "<unset>"
    } else {
        "<redacted>"
    }
}

/// One configured provider: a name (shown in the chip), the protocol it speaks,
/// its base URL, and the models it offers.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: Option<String>,
}

// Custom `Debug` so a debug-logged config never leaks the provider `api_key`
// (H4); presence stays observable.
impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("models", &self.models)
            .field("enabled", &self.enabled)
            .field("api_key", &redact_opt_secret(&self.api_key))
            .finish()
    }
}

/// The Foreman Memory API connection (slice-2 spec §2).
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct MemoryConfig {
    /// Empty (the default) = no memory service: nothing is contacted.
    #[serde(default)]
    pub base_url: String,
    /// Explicit v5 reader bearer token. When empty (the default), the client
    /// falls back to reading `~/.config/foreman/v5-boot-token`. Set this only to
    /// override the standard token location.
    #[serde(default)]
    pub token: String,
    /// Only standing directives at or above this priority are injected each turn.
    /// The spec fixes the bar at 90; configurable so a deployment can tighten it.
    #[serde(default = "default_min_priority")]
    pub min_priority: i64,
    /// Whether per-turn recall of Awili's own memories runs (slice 9). Off ⇒ no
    /// recall fetch, no recall chip; rules injection is unaffected.
    #[serde(default)]
    pub recall: bool,
    /// Whether Awili writes back to the memory service (slice 10 distillation and
    /// the usage notes). Off ⇒ Awili is read-only toward memory.
    #[serde(default)]
    pub write: bool,
}

// Custom `Debug` so a debug-logged config never leaks the Foreman bearer token
// (H4); presence stays observable.
impl std::fmt::Debug for MemoryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryConfig")
            .field("base_url", &self.base_url)
            .field("token", &redact_str_secret(&self.token))
            .field("min_priority", &self.min_priority)
            .field("recall", &self.recall)
            .field("write", &self.write)
            .finish()
    }
}

/// The Claude-Code Dispatcher (`ccd`) connection (slice-5 spec §D). ccd owns the
/// lifecycle of heavy `claude` coding jobs on a fleet host; Awili is a read-only
/// console over it. Mirrors [`MemoryConfig`]: always present with a runnable
/// default, `enabled` derived from a non-empty `base_url`.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CcdConfig {
    /// Base URL of the ccd HTTP API (default `http://127.0.0.1:8870`, the local
    /// bind; point at an ssh-tunnel or tailscale address to reach a remote host).
    pub base_url: String,
    /// Bearer token sent on every request. Empty when ccd needs no credential.
    #[serde(default)]
    pub token: String,
    /// Repo the dispatched job runs against (must be inside ccd's `CCD_REPO_ROOTS`).
    #[serde(default = "default_ccd_repo")]
    pub repo: String,
    /// Branch the job works on.
    #[serde(default = "default_ccd_branch")]
    pub branch: String,
    /// Optional server-side verify profile (e.g. `rust_check`) attached to each
    /// dispatch; `None` dispatches with no verification.
    #[serde(default)]
    pub verify_profile: Option<String>,
}

// Custom `Debug` so a debug-logged config never leaks the ccd bearer token
// (H4); presence stays observable.
impl std::fmt::Debug for CcdConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CcdConfig")
            .field("base_url", &self.base_url)
            .field("token", &redact_str_secret(&self.token))
            .field("repo", &self.repo)
            .field("branch", &self.branch)
            .field("verify_profile", &self.verify_profile)
            .finish()
    }
}

/// The Hermes Agent connection (the agentic "Cowork" engine, slice "awili-hermes").
/// Hermes is an OpenAI-compatible server that runs a server-side AIAgent loop;
/// Awili drives it via the gateway (`awili-gateway`). Mirrors [`CcdConfig`]:
/// always present with a runnable loopback default, `enabled` derived from a
/// non-empty `base_url`.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct HermesConfig {
    /// Base URL of the Hermes API server, including the `/v1` prefix the server
    /// publishes (default `http://127.0.0.1:8642/v1`, the local bind). Point at an
    /// ssh-tunnel or tailscale address to reach a remote host.
    #[serde(default = "default_hermes_base_url")]
    pub base_url: String,
    /// Bearer key sent as `Authorization: Bearer <key>` on every request. Carried
    /// here (not derived from a port heuristic), mirroring the per-provider
    /// `api_key` pattern: the credential is bound to *this* endpoint. Empty when
    /// the server needs none (it will then answer 401, surfaced to the user).
    #[serde(default)]
    pub api_key: String,
}

// Custom `Debug` so a debug-logged config never leaks the bearer key: `api_key`
// is redacted regardless of length, while presence is still observable.
impl std::fmt::Debug for HermesConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HermesConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

fn default_hermes_base_url() -> String {
    "http://127.0.0.1:8642/v1".to_string()
}

impl Default for HermesConfig {
    fn default() -> Self {
        Self {
            base_url: default_hermes_base_url(),
            api_key: String::new(),
        }
    }
}

/// The Hermes gateway WebSocket connection (folder-scoped Work mode).
///
/// The gateway `/api/ws` endpoint lives on the dashboard web server, which runs
/// on a dynamic port. `dashboard_url` is the HTTP base URL of the running
/// dashboard; the token is scraped from its index page at connect time.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
pub struct GatewayConfig {
    /// HTTP base URL of the Hermes dashboard (e.g. `http://127.0.0.1:60142`).
    /// When empty, gateway mode is disabled and Work mode falls back to the
    /// REST `/v1/runs` path.
    #[serde(default)]
    pub dashboard_url: String,
}

impl std::fmt::Debug for GatewayConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayConfig")
            .field("dashboard_url", &self.dashboard_url)
            .finish()
    }
}

/// The ONE server name allowed to carry [`McpTrust::Install`].
///
/// A name, not a capability: see [`McpTrust::Install`] for why the seed site
/// must still check executable identity.
pub const INSTALL_TRUSTED_SERVER: &str = "mac-shell";

/// When a configured MCP server's tools may be used without a fresh approval.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTrust {
    /// Approve on first use; the user's choice then persists (the normal C1
    /// grant flow). The default, and the right answer for anything Awili did
    /// not ship with.
    #[default]
    Prompt,
    /// Trusted at install time — a grant is seeded once during setup and no
    /// card is ever raised.
    ///
    /// Reserved for [`INSTALL_TRUSTED_SERVER`], which is how Awili reaches the
    /// Foreman memory service: prompting per call would break the very thing
    /// every session depends on (Owner, 2026-08-13). A deliberate, narrow
    /// exception — NOT a convenience switch. Setting it on another server
    /// means that server can act on this machine with no human in the loop,
    /// ever.
    ///
    /// **Enforcement is two-part, and only the first part lives here.**
    /// [`Config::parse`] rejects this variant on any other name, and
    /// [`McpServerConfig::is_install_trusted`] re-checks the name so a
    /// programmatically built config cannot slip past the parser. That is
    /// necessary but NOT sufficient: a config can name a server `mac-shell`
    /// and point `command` at any executable on the machine. The grant-seed
    /// site (unit 5) MUST independently verify setup-controlled executable
    /// identity before seeding — configuration text alone must never confer
    /// permanent trust (Codex P1, 2026-08-13).
    Install,
}

/// An Awili mode a server's tools are offered in.
///
/// `Chat` remains only to deserialize pre-consolidation config. It is not a
/// live runtime surface; Work and Code are the available tool surfaces.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpMode {
    /// Legacy serialized value. It is never offered at runtime.
    Chat,
    Work,
    Code,
}

/// One MCP server Awili launches and offers tools from.
///
/// RC2-SVC2 (F0384): `Debug` is written by hand — `env` routinely carries
/// credentials (API keys, tokens), so a debug-logged config shows the env
/// NAMES with every value redacted.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpServerConfig {
    /// Short name; the registry origin is `mcp:<name>`, which cannot collide
    /// with the reserved `builtin:*` space.
    pub name: String,
    /// Absolute path to the server executable. Relative paths are rejected at
    /// launch: what one resolves to depends on how Awili was started, and
    /// silently launching a different binary than intended is not an ambiguity
    /// a tool that can act on the machine should carry.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the child process.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub trust: McpTrust,
    /// Modes this server's tools appear in. Defaults to Work + Code. Legacy
    /// `chat` entries deserialize but are never offered.
    #[serde(default = "default_mcp_modes")]
    pub modes: Vec<McpMode>,
    /// Set false to keep a server configured but not launched.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_mcp_modes() -> Vec<McpMode> {
    vec![McpMode::Work, McpMode::Code]
}

/// RC2-SVC2 (F0384): an environment map for `Debug` — every name kept, every
/// value shown only as `<redacted>` (set) or `<unset>` (empty).
fn redact_env(
    env: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<&str, &'static str> {
    env.iter()
        .map(|(k, v)| (k.as_str(), redact_str_secret(v)))
        .collect()
}

impl std::fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpServerConfig")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env", &redact_env(&self.env))
            .field("trust", &self.trust)
            .field("modes", &self.modes)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl McpServerConfig {
    /// The registry origin string for this server.
    pub fn origin(&self) -> String {
        format!("mcp:{}", self.name)
    }

    pub fn offers_in(&self, mode: McpMode) -> bool {
        mode != McpMode::Chat && self.modes.contains(&mode)
    }

    /// Trusted at install: no approval card, ever.
    ///
    /// The name is re-checked here, not just at parse time, so a config built
    /// in code (a test, a future migration) cannot confer install trust on a
    /// server the parser would have rejected.
    pub fn is_install_trusted(&self) -> bool {
        self.trust == McpTrust::Install && self.name == INSTALL_TRUSTED_SERVER
    }
}

/// The `[mcp]` section.
#[derive(Clone, PartialEq, Eq, Debug, Default, Deserialize, Serialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

impl McpConfig {
    /// Servers that should actually be launched.
    pub fn active(&self) -> impl Iterator<Item = &McpServerConfig> {
        self.servers.iter().filter(|s| s.enabled)
    }

    /// Active servers offering tools in `mode`.
    pub fn active_in(&self, mode: McpMode) -> impl Iterator<Item = &McpServerConfig> {
        self.active().filter(move |s| s.offers_in(mode))
    }
}

/// Which engine backs the Work surface (ENGINE-PIVOT-PLAN r3.1, D2-u2a).
///
/// The variants map to the two [`awili_work::EngineWorker`] implementations.
/// An absent `[engine]` section, or an absent `kind`, means
/// [`EngineKind::HermesGateway`] — the D1 path — so adding this section is
/// never a behavior change on its own.
///
/// Since L1 the selected Work executor decides the engine; `kind` is parsed so
/// existing files load and is not read by the app. Unknown values stay a hard
/// error.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineKind {
    /// The Hermes gateway adapter (`HermesGatewayWorker`).
    #[default]
    HermesGateway,
    /// `awili-engine` in-process (`AwiliEngineWorker`).
    Awili,
}

/// The `[engine]` section: which engine runs Work, and — for the `awili` kind —
/// the OpenAI-compatible endpoint it drives.
///
/// `base_url`/`model` are only read when `kind = "awili"`; the gateway path
/// takes its endpoint from [`GatewayConfig`] as before. An `awili` kind with an
/// empty `base_url` or `model` is rejected at the spawn site rather than at
/// parse time, so an engine stanza left half-filled cannot break launch for
/// someone still on the gateway.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct EngineConfig {
    #[serde(default)]
    pub kind: EngineKind,
    /// OpenAI-compatible server root; the Chat Completions path is appended.
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    /// Bearer credential for an authenticated OpenAI-compatible engine endpoint.
    /// Bound to THIS endpoint only; never sent anywhere else. Empty = send no
    /// Authorization header (a self-hosted vLLM box needs none).
    #[serde(default)]
    pub api_key: String,
    /// Display name for the in-app model on the executor chip and picker,
    /// e.g. "Gemma 4 12B (Studio)". Empty = derived from `model`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Wire dialect. u2a speaks `chat-completions` only; u2b adds the rest.
    #[serde(default = "default_engine_dialect")]
    pub dialect: String,
    /// C4 context window for the session profile.
    #[serde(default = "default_engine_window_tokens")]
    pub window_tokens: u64,
    /// How long, in seconds, the engine waits for the model server to send
    /// anything (the reply starting, or the next part of it). RC2-GEMMA3: a
    /// limit on silence, not on the whole reply — a long answer, or a server
    /// still reading a long request (it sends a keepalive as it goes), is
    /// never cut off while it is alive.
    #[serde(default = "default_engine_request_timeout_s")]
    pub request_timeout_s: u64,
}

// Custom `Debug` so a debug-logged config never leaks the engine `api_key`
// (H4); presence stays observable, following the other secret-bearing
// sub-structs.
impl std::fmt::Debug for EngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineConfig")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &redact_str_secret(&self.api_key))
            .field("label", &self.label)
            .field("dialect", &self.dialect)
            .field("window_tokens", &self.window_tokens)
            .field("request_timeout_s", &self.request_timeout_s)
            .finish()
    }
}

fn default_engine_dialect() -> String {
    "chat-completions".to_string()
}

/// Matches `awili_seam::DEFAULT_WINDOW_TOKENS` (laguna serves 131072, verified
/// live 2026-08-13). Duplicated as a literal so `awili-config` stays free of a
/// dependency on the seam.
fn default_engine_window_tokens() -> u64 {
    131_072
}

fn default_engine_request_timeout_s() -> u64 {
    180
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            kind: EngineKind::default(),
            base_url: String::new(),
            model: String::new(),
            api_key: String::new(),
            label: String::new(),
            dialect: default_engine_dialect(),
            window_tokens: default_engine_window_tokens(),
            request_timeout_s: default_engine_request_timeout_s(),
        }
    }
}

impl EngineConfig {
    /// The in-app endpoint is filled in: `base_url` and `model` are both
    /// non-blank.
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.model.trim().is_empty()
    }

    /// Display name for the in-app model: `label` when non-blank, else the
    /// last `/`-separated segment of `model` when non-blank, else
    /// `"no model set"`.
    pub fn display_label(&self) -> String {
        let label = self.label.trim();
        if !label.is_empty() {
            return label.to_string();
        }
        let model = self.model.trim();
        if !model.is_empty() {
            return model.rsplit('/').next().unwrap_or(model).to_string();
        }
        "no model set".to_string()
    }

    /// The host of `base_url`: the text after `"://"` up to the first `':'`
    /// or `'/'`. `None` when `base_url` is blank, has no `"://"`, or the host
    /// part is empty.
    pub fn endpoint_host(&self) -> Option<String> {
        let rest = self.base_url.split("://").nth(1)?;
        let host = rest.split([':', '/']).next()?;
        if host.is_empty() {
            None
        } else {
            Some(host.to_string())
        }
    }
}

/// One entry of the optional `[[engines]]` list: an additional in-app model
/// next to the primary `[engine]`. Same endpoint fields as [`EngineConfig`]
/// plus a required stable `id`, the key the executor string `in-app:<id>`
/// refers to. Unknown keys are a load error (a typo'd `base_ulr` must not
/// leave an engine silently half-configured).
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraEngineConfig {
    /// Stable id: 1..=64 chars of `[A-Za-z0-9._-]`, unique across the list.
    /// Persisted in conversations as `in-app:<id>`; never shown to the user.
    pub id: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    /// Bearer credential, bound to THIS endpoint only. Empty = none.
    #[serde(default)]
    pub api_key: String,
    /// Picker/chip name, e.g. "Qwen 3.8 27B (hostd)". Empty = from `model`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default = "default_engine_dialect")]
    pub dialect: String,
    #[serde(default = "default_engine_window_tokens")]
    pub window_tokens: u64,
    #[serde(default = "default_engine_request_timeout_s")]
    pub request_timeout_s: u64,
}

// Redacting `Debug`, as for [`EngineConfig`]: the `api_key` is never logged.
impl std::fmt::Debug for ExtraEngineConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtraEngineConfig")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &redact_str_secret(&self.api_key))
            .field("label", &self.label)
            .field("dialect", &self.dialect)
            .field("window_tokens", &self.window_tokens)
            .field("request_timeout_s", &self.request_timeout_s)
            .finish()
    }
}

impl ExtraEngineConfig {
    /// The runtime engine for this entry: an in-app (`Awili`) [`EngineConfig`]
    /// carrying this entry's endpoint, key, limits and label.
    pub fn to_engine_config(&self) -> EngineConfig {
        EngineConfig {
            kind: EngineKind::Awili,
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            api_key: self.api_key.clone(),
            label: self.label.clone(),
            dialect: self.dialect.clone(),
            window_tokens: self.window_tokens,
            request_timeout_s: self.request_timeout_s,
        }
    }
}

/// Whether `id` is a valid `[[engines]]` id.
pub fn is_valid_engine_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn default_ccd_repo() -> String {
    String::new()
}

fn default_ccd_branch() -> String {
    "main".to_string()
}

impl Default for CcdConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8870".to_string(),
            token: String::new(),
            repo: default_ccd_repo(),
            branch: default_ccd_branch(),
            verify_profile: None,
        }
    }
}

/// The hard floor for injected standing directives, fixed by the slice-2 spec:
/// every injected directive must be priority ≥ 90. A deployment may set a *higher*
/// `min_priority` to tighten the bar, but never a lower one.
pub const MIN_PRIORITY_FLOOR: i64 = 90;

fn default_min_priority() -> i64 {
    MIN_PRIORITY_FLOOR
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            token: String::new(),
            min_priority: 90,
            recall: false,
            write: false,
        }
    }
}

/// The attention-feed service connection (slice-4 spec §8). Disabled — no
/// panel, no polling — when the table is absent or `base_url` is empty: the
/// feed is fleet infrastructure a fresh checkout may not have. The bearer
/// token comes from the `AWILI_FEED_TOKEN` environment variable, never config.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct FeedConfig {
    #[serde(default)]
    pub base_url: String,
}

/// The `[outline]` connector (daily-driver P12): the fleet Outline
/// notes/knowledge base, offered as the `outline_*` MCP tools of the
/// `awili-connectors` server. The credential is NEVER stored here — only the
/// file path (plus the key line to take from it). The value is resolved
/// through `awili_connectors::SecretRef` at call time, so nothing secret is
/// ever held or rendered: `Debug` can be derived as-is.
/// RC2-SVC2 (F0382): an unknown key (a typo such as `enabeld`) is a load
/// error naming the key, never a silent fall-back to the defaults.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutlineConfig {
    /// Whether the Outline connector family is active at all. Off ⇒ the
    /// `outline_*` tools are omitted and (with a disabled `[gitea]`) no
    /// synthesised `awili-connectors` server entry is produced.
    #[serde(default)]
    pub enabled: bool,
    /// The Outline API base; every verb is a JSON `POST` to a dotted path
    /// under it (the P11 wire contract).
    #[serde(default = "default_outline_base_url")]
    pub base_url: String,
    /// Path to the file holding the Outline API key (a `KEY=VALUE` config
    /// file). `~` is expanded at resolution time. Only the PATH may be
    /// logged — never a value read from the file.
    #[serde(default = "default_outline_secret_file")]
    pub secret_file: String,
    /// The `KEY=VALUE` line whose value is the key, taken from `secret_file`.
    #[serde(default = "default_outline_secret_key")]
    pub secret_key: String,
}

fn default_outline_base_url() -> String {
    String::new()
}

fn default_outline_secret_file() -> String {
    String::new()
}

fn default_outline_secret_key() -> String {
    "OUTLINE_API_KEY".to_string()
}

impl Default for OutlineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_outline_base_url(),
            secret_file: default_outline_secret_file(),
            secret_key: default_outline_secret_key(),
        }
    }
}

/// The `[gitea]` connector (daily-driver P12): the fleet Gitea code host,
/// offered as the `gitea_*` MCP tools of the `awili-connectors` server. As
/// with [`OutlineConfig`], no credential value is ever stored — only the file
/// path (and optionally the key line): the token is resolved through
/// `awili_connectors::SecretRef` at call time.
/// RC2-SVC2 (F0382): unknown keys are refused, as for [`OutlineConfig`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GiteaConfig {
    /// Whether the Gitea connector family is active at all (same gating as
    /// [`OutlineConfig::enabled`]).
    #[serde(default)]
    pub enabled: bool,
    /// The Gitea API base (the P11 wire contract is `…/api/v1`).
    #[serde(default = "default_gitea_base_url")]
    pub base_url: String,
    /// Path to the token file. `~` is expanded at resolution time; only the
    /// PATH may be logged.
    #[serde(default = "default_gitea_secret_file")]
    pub secret_file: String,
    /// `None` = the WHOLE file is the token (trimmed); `Some(key)` = the
    /// `KEY=VALUE` line of `key` in the file.
    #[serde(default)]
    pub secret_key: Option<String>,
    /// The SSH host for `ssh_clone_url` — separate from the HTTP API host,
    /// because the fleet's Gitea sshd listens on a different port than the
    /// HTTP API.
    #[serde(default = "default_gitea_ssh_host")]
    pub ssh_host: String,
    /// The SSH port (the fleet Gitea sshd listens on 2222, never the HTTP
    /// API port).
    #[serde(default = "default_gitea_ssh_port")]
    pub ssh_port: u16,
}

fn default_gitea_base_url() -> String {
    String::new()
}

fn default_gitea_secret_file() -> String {
    String::new()
}

fn default_gitea_ssh_host() -> String {
    String::new()
}

fn default_gitea_ssh_port() -> u16 {
    awili_connectors_ssh_port_default()
}

/// 0 = not set; a clone URL needs `ssh_host` too, so the port alone does
/// nothing.
fn awili_connectors_ssh_port_default() -> u16 {
    0
}

impl Default for GiteaConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_gitea_base_url(),
            secret_file: default_gitea_secret_file(),
            secret_key: None,
            ssh_host: default_gitea_ssh_host(),
            ssh_port: default_gitea_ssh_port(),
        }
    }
}

/// Vendor agent-harness binaries. The executor-selection design of record
/// (2026-08-22 §1) specified `[work.agents]`; this is it. An explicit path
/// wins; otherwise the binary is looked up by name in `search_paths` (a GUI
/// app launched from Finder inherits no shell `PATH`, which is why discovery
/// exists at all), and finally by bare name for `PATH` resolution.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentBinsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_bin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_bin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zcode_bin: Option<String>,
    /// The DeepSeek `dsh` harness binary (daily-driver P9); mirrors
    /// `claude_bin`/`codex_bin`/`zcode_bin`. When absent, `dsh` is looked up
    /// in `search_paths` (which include `~/.local/bin`) and finally by bare
    /// name for `PATH` resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsh_bin: Option<String>,
    /// The env file the app reads itself to hand credentials to the dsh child
    /// (P9): a macOS .app does not inherit a login shell, so the child's
    /// `DEEPSEEK_API_KEY`/`DEEPSEEK_BASE_URL` must come from this file. When
    /// absent, [`DEFAULT_DSH_ENV_FILE`] stands. `~` is expanded by the caller
    /// exactly like every other configured path in this crate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsh_env_file: Option<String>,
    #[serde(default = "default_agent_search_paths")]
    pub search_paths: Vec<String>,
    /// RC1-M: the owner's memory setup. Its presence marks the owner's own
    /// install: his Claude/Codex hooks deliver memory to the agents Awili
    /// launches (so Awili's own memory preamble is not added for them), and
    /// his Chat gets his setup. `None` = a household install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<AgentMemoryConfig>,
    /// RC1-M: the user-level AGENTS.md every agent Awili starts is given
    /// (`~` expanded by the caller). Absent = [`DEFAULT_USER_AGENTS_MD`];
    /// an empty string = none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents_md: Option<String>,
}

/// RC1-M: the tool-neutral user-level AGENTS.md (the owner's is a symlink to
/// his fleet instructions).
pub const DEFAULT_USER_AGENTS_MD: &str = "~/.agents/AGENTS.md";

/// `[work.agents.memory]`. Every field is optional; `~` is expanded by the
/// caller. RC1-M: the agents run with the owner's own settings, so only
/// `interlock_command` / `interlock_env` (the owner's Chat memory server) and
/// `codex_model` / `codex_reasoning_effort` are used; `precompact_hook` and
/// `codex_hook_trust` are read for compatibility and ignored (his own hooks
/// run).
/// RC2-SVC2 (F0384): `Debug` is written by hand so `interlock_env` values
/// never print (only the names do).
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentMemoryConfig {
    /// The interlock memory MCP server program. Absent = no memory server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interlock_command: Option<String>,
    /// Its environment (URL and token FILE paths — never token values).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub interlock_env: std::collections::BTreeMap<String, String>,
    /// The PreCompact memory hook script (run as `bash <path>`). Absent = the
    /// fleet default `~/.claude/foreman-gate/precompact-breadcrumb.sh` when
    /// it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precompact_hook: Option<String>,
    /// Codex's trust hash for that hook (`sha256:…`, the value Codex stored
    /// for the same hook in the owner's own config). Absent = Codex skips it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_hook_trust: Option<String>,
    /// Codex model and reasoning effort for the Codex runs Awili starts
    /// (passed as `-c` overrides on top of the owner's own config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_reasoning_effort: Option<String>,
    /// RC1-N-FIX1 (lead ruling, 2026-10-04, owner's direction to cut memory
    /// waste; was RC1-M-FIX7's `hooks_provide_recall`, still accepted as an
    /// alias): the owner says his own Claude Code and Codex hooks bring his
    /// memory — the standing directives at session start and recall on each
    /// prompt. `true`: Awili sends neither its standing directives nor its
    /// recall to those agents (Work and Chat); the hooks deliver them once.
    /// `false`, the default: Awili sends both. Awili does not inspect hook
    /// files. Other engines (the in-app engine, GLM, DeepSeek) keep Awili's
    /// memory either way.
    #[serde(
        default,
        alias = "hooks_provide_recall",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub hooks_provide_memory: bool,
}

impl std::fmt::Debug for AgentMemoryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMemoryConfig")
            .field("interlock_command", &self.interlock_command)
            .field("interlock_env", &redact_env(&self.interlock_env))
            .field("precompact_hook", &self.precompact_hook)
            .field("codex_hook_trust", &self.codex_hook_trust)
            .field("codex_model", &self.codex_model)
            .field("codex_reasoning_effort", &self.codex_reasoning_effort)
            .field("hooks_provide_memory", &self.hooks_provide_memory)
            .finish()
    }
}

/// The default dsh env-file location (P9 facts): mode-0600 `KEY=VALUE` lines,
/// possibly with a leading `export `. Only the PATH may be logged — never a
/// value read from the file.
pub const DEFAULT_DSH_ENV_FILE: &str = "";

/// Windows: where npm and per-user installers put vendor CLIs. `~/.local/bin`
/// and `~/.npm-global/bin` keep `~` (expanded to `%USERPROFILE%` at use).
#[cfg(windows)]
fn default_agent_search_paths() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(d) = dirs::config_dir() {
        v.push(d.join("npm").to_string_lossy().into_owned());
    }
    if let Some(d) = dirs::data_local_dir() {
        v.push(d.join("Programs").to_string_lossy().into_owned());
    }
    v.push("~/.local/bin".to_string());
    v.push("~/.npm-global/bin".to_string());
    v
}

/// Linux: `~/.local/bin` and `~/.npm-global/bin`, then the system dirs, then the
/// npm prefix from `NPM_CONFIG_PREFIX` or `~/.npmrc` (last: see below). A GUI
/// launched from a desktop entry inherits no shell `PATH`, which is why these
/// are listed. No `npm` process is run to ask.
#[cfg(target_os = "linux")]
fn default_agent_search_paths() -> Vec<String> {
    let mut v: Vec<String> = vec!["~/.local/bin".to_string(), "~/.npm-global/bin".to_string()];
    for d in [
        "/usr/local/bin",
        "/usr/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ] {
        v.push(d.to_string());
    }
    // The npm prefix comes AFTER the system dirs: nothing here vets the owner or
    // mode of a resolved binary, so a user-writable prefix must not shadow
    // /usr/bin or /usr/local/bin.
    let home = dirs::home_dir();
    if let Some(bin) = npm_prefix_bin(home.as_deref()) {
        if !v.contains(&bin) {
            v.push(bin);
        }
    }
    v
}

/// `<npm prefix>/bin`, from `NPM_CONFIG_PREFIX` or the `prefix=` line of
/// `~/.npmrc`.
#[cfg(target_os = "linux")]
fn npm_prefix_bin(home: Option<&Path>) -> Option<String> {
    let env = std::env::var("NPM_CONFIG_PREFIX").ok();
    let from_rc = || {
        let home = home?;
        let text = std::fs::read_to_string(home.join(".npmrc")).ok()?;
        parse_npmrc_prefix(&text, home)
    };
    pick_npm_prefix(env.as_deref(), from_rc)
}

/// The env value when it is an absolute path, else the `.npmrc` one. A relative
/// `NPM_CONFIG_PREFIX` is ignored: a search path must be absolute, or a binary
/// in the launch folder could be picked up.
#[cfg(any(target_os = "linux", test))]
fn pick_npm_prefix(env: Option<&str>, from_rc: impl FnOnce() -> Option<String>) -> Option<String> {
    let prefix = env
        .map(str::trim)
        .filter(|e| e.starts_with('/'))
        .map(str::to_string)
        .or_else(from_rc)?;
    Some(format!("{}/bin", prefix.trim_end_matches('/')))
}

/// The `prefix` value of an `.npmrc` (last one wins), `~` and `$HOME` expanded.
/// Relative values are ignored: a search path must be absolute.
#[cfg(any(target_os = "linux", test))]
fn parse_npmrc_prefix(text: &str, home: &Path) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "prefix" {
            continue;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        let h = home.to_string_lossy();
        let value = if let Some(rest) = value.strip_prefix("~/") {
            format!("{h}/{rest}")
        } else {
            value.replace("${HOME}", &h).replace("$HOME", &h)
        };
        if value.starts_with('/') {
            found = Some(value);
        }
    }
    found
}

#[cfg(not(any(windows, target_os = "linux")))]
fn default_agent_search_paths() -> Vec<String> {
    [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "~/.npm-global/bin",
        "~/.local/bin",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl AgentBinsConfig {
    /// The EFFECTIVE dsh env-file path: the configured `dsh_env_file` when
    /// set and non-blank, else [`DEFAULT_DSH_ENV_FILE`] (P9). The `~` is
    /// expanded by the caller like every other configured path.
    pub fn dsh_env_file(&self) -> String {
        self.dsh_env_file
            .clone()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| DEFAULT_DSH_ENV_FILE.to_string())
    }
}

impl Default for AgentBinsConfig {
    fn default() -> Self {
        Self {
            claude_bin: None,
            codex_bin: None,
            zcode_bin: None,
            dsh_bin: None,
            dsh_env_file: None,
            search_paths: default_agent_search_paths(),
            memory: None,
            agents_md: None,
        }
    }
}

/// The `[work]` section — Work-surface policy that is not provider config.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkConfig {
    #[serde(default)]
    pub agents: AgentBinsConfig,
    /// What executes Work runs when nothing has been chosen yet — the
    /// executor-selection design of record (2026-08-22 §1) calls this
    /// `default_executor` and says it SEEDS only: a persisted per-app choice
    /// always wins over it. Grammar is the persisted one — `"in-app"` (the
    /// default, the in-app engine on this Mac), `"gateway"` (the fleet
    /// gateway, used only when picked), or
    /// `"agent:<claude|codex|glm|dsh>[:<model>]"`. Unrecognised values are
    /// ignored (the InApp default stands), never an error: a household
    /// install must not brick Work over a typo here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_executor: Option<String>,
    /// The executor the one-click cloud buttons switch Work to, in the
    /// `default_executor` grammar. Only `agent:…` values are honoured;
    /// absent or anything else means `agent:claude` (Claude's own CLI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_escalation: Option<String>,
    /// U4 (spec D2): the global default "Ask before changes" posture —
    /// `"ask"` | `"auto"`; anything else, or absent, means ask, and a bad
    /// value is never an error (a typo must not brick Work). Per-conversation
    /// choices are session-only and override this at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_approval_mode: Option<String>,
    /// rc.4 S7: `[work.sandbox]` — the opt-in shell sandbox for agent commands.
    #[serde(default)]
    pub sandbox: SandboxConfig,
}

/// `[work.sandbox]` (rc.4 S7). Default OFF: the owner's 2026-06-24 rule is that
/// Autonomous turns have full filesystem and network access unless he turns
/// this on. On: every Autonomous vendor-agent process starts under a macOS
/// seatbelt profile that limits file WRITES (never reads, never network).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SandboxConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Extra folders the agent may write to (`~` expanded by the caller).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_writable: Vec<String>,
}

/// The `[chat]` section (daily-driver P9): Chat is PINNED to one executor —
/// the owner decision of record (2026-09-18): "in fact there should be no
/// picker for chat at all"; "just peg chat to haiku or luna and move on".
/// There is deliberately NO chat model chip; the pin is config-only.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChatConfig {
    /// The executor that runs every Chat turn, parsed with the SAME grammar
    /// as `[work].default_executor` (`"agent:<claude|codex|glm|dsh>[:<model>]"`),
    /// so `"agent:dsh:deepseek-flash"` — or a future local Luna entry — is
    /// legal without code changes. The default is Claude · haiku through the
    /// vendor's own `claude` binary. The value is a vendor/model NAME, never
    /// a secret: no redacting `Debug` is needed.
    #[serde(default = "default_chat_executor")]
    pub executor: String,
}

/// The default Chat pin (P9): Claude · haiku, through the `claude` binary —
/// a cheap, routine lane, exactly as the standing vendor-binary rule wants.
pub const DEFAULT_CHAT_EXECUTOR: &str = "agent:claude:haiku";

fn default_chat_executor() -> String {
    DEFAULT_CHAT_EXECUTOR.to_string()
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            executor: default_chat_executor(),
        }
    }
}

/// One `[build_targets.<id>]` entry — the app-side binding for a local Build
/// lane (daily-driver P2d): which dispatcher host the durable SSH runner
/// lives on and what it runs. Every field is required within a target; a
/// missing or invalid field is a config error naming the field, surfaced at
/// load. The runner's own repository/command bindings stay in the
/// trusted-administrator runner config on the host (P2c) — prompt text can
/// never choose executables, repositories, or models.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildTargetConfig {
    /// Dispatcher host the `ssh` client connects to. Option-looking or
    /// shell-metacharacter values are rejected at load (the SSH client
    /// re-validates before any call).
    pub ssh_host: String,
    /// Absolute path of the `awili-build-runner` executable on the host.
    pub runner_path: String,
    /// The repository this target owns — must equal the runner-side binding,
    /// so a mismatched request is rejected at the runner, not in flight.
    pub repo_id: String,
    /// The local model the runner executes for this target (displayed
    /// verbatim in the Work executor picker).
    pub model: String,
    /// The verification profile the runner applies after the model.
    pub verify_profile: String,
}

impl BuildTargetConfig {
    /// The picker/chip label: `Build · <model> on <ssh_host>`.
    pub fn label(&self) -> String {
        format!("Build \u{00B7} {} on {}", self.model, self.ssh_host)
    }
}

/// Model vocabularies that used to be compiled in. They are DATA about
/// third-party products — vendors rename and add models on their own
/// schedule — so a new name must never require rebuilding Awili. The
/// compiled lists remain the defaults, so an absent section behaves exactly
/// as before.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelsConfig {
    /// Selectable aliases for the Claude Code harness (`claude --model <x>`).
    #[serde(default = "default_claude_aliases")]
    pub claude_aliases: Vec<String>,
    /// Substrings that mark a DISCOVERED model as non-chat (speech, image,
    /// embedding-adjacent, reranking): matched case-insensitively, these are
    /// never offered as chat targets.
    #[serde(default = "default_non_chat_filters")]
    pub non_chat_filters: Vec<String>,
    /// Selectable models for the DeepSeek `dsh` harness (daily-driver P9):
    /// the picker's rows are these ids; the chosen one travels to the child
    /// as `DSH_MODEL` (the headless profile has no model flag). VENDOR NAMES
    /// ARE DATA — a new DeepSeek model must not require rebuilding Awili.
    #[serde(default = "default_dsh_models")]
    pub dsh_models: Vec<String>,
}

fn default_claude_aliases() -> Vec<String> {
    ["fable", "opus", "sonnet", "haiku"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The default selectable `dsh` models (P9): the headless route's default
/// (`deepseek-flash`) and the pro route.
fn default_dsh_models() -> Vec<String> {
    ["deepseek-flash", "deepseek-v4-pro"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_non_chat_filters() -> Vec<String> {
    [
        "whisper",
        "kokoro",
        "flux",
        "tts",
        "vae",
        "sdxl",
        "stable-diffusion",
        "rerank",
        "clip",
        "guard",
        "moderation",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            claude_aliases: default_claude_aliases(),
            non_chat_filters: default_non_chat_filters(),
            dsh_models: default_dsh_models(),
        }
    }
}

/// The `[skills]` section (daily-driver P1): the fleet skills pack the app
/// loads once at startup and on config reload.
///
/// The pack is a directory of `awili-skills` skills (`<dir>/<name>/SKILL.md`)
/// whose selected recipes are injected ahead of the executor's prompt on Work
/// submission. It is OFF by default (`None`): existing configs that never
/// mention `[skills]` parse unchanged and no recipes are injected. A bad
/// `fleet_pack` path never breaks the config or the send — the app logs a
/// clear diagnostic and runs without the pack.
///
/// Lifecycle: the pack is loaded from the app config at startup and re-loaded
/// whenever the config is re-applied (live save or startup re-check). File
/// edits to the pack therefore become active on the next config reload or app
/// restart; the app runs NO background watcher over the pack directory.
/// The `[graduation]` section (daily-driver P8b): where the app appends one
/// line per terminal Work run for graduation week. The ledger is the
/// append-only JSON Lines file `awili-selftest graduation report` reads;
/// `ledger = ""` disables recording. `~` is expanded at use.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct GraduationConfig {
    /// Ledger path; empty disables. Default `~/.local/share/awili/graduation.jsonl`.
    #[serde(default = "default_graduation_ledger")]
    pub ledger: String,
}

fn default_graduation_ledger() -> String {
    "~/.local/share/awili/graduation.jsonl".to_string()
}

impl Default for GraduationConfig {
    fn default() -> Self {
        Self {
            ledger: default_graduation_ledger(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct SkillsConfig {
    /// Directory holding the fleet skills pack. Absent (the default) means the
    /// pack is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet_pack: Option<PathBuf>,
}

/// The `[notifications]` section (N1: `docs/daily-driver/specs/
/// N1-macos-notifications.md` §3). An absent or partial section equals the
/// defaults: everything on, 8 s, 120 s. `Config::parse` does not validate the
/// numbers; they clamp at use through the `effective_*` methods.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct NotificationsConfig {
    /// Master switch.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Approval, question, sudo and secret prompts (Work).
    #[serde(default = "default_true")]
    pub approvals: bool,
    /// A Work run or Code turn finished.
    #[serde(default = "default_true")]
    pub finished: bool,
    /// A Work run or Code turn failed, or a run stopped by something other
    /// than the owner.
    #[serde(default = "default_true")]
    pub failed: bool,
    /// Legacy deserialization only. Chat replies no longer have a runtime
    /// notification path, and newly serialized configs omit this key.
    #[serde(default = "default_true", skip_serializing)]
    pub chat_replies: bool,
    /// Play the default sound.
    #[serde(default = "default_true")]
    pub sound: bool,
    /// When native notifications are UNAVAILABLE (never when the owner
    /// denied them), post through the Script Editor fallback.
    #[serde(default = "default_true")]
    pub fallback_osascript: bool,
    /// Finished run: the run length (or time away) worth a notification.
    #[serde(default = "default_min_run_secs")]
    pub min_run_secs: u64,
    /// Frontmost-but-idle for this long counts as away; 0 disables.
    #[serde(default = "default_idle_away_secs")]
    pub idle_away_secs: u64,
}

fn default_min_run_secs() -> u64 {
    8
}

fn default_idle_away_secs() -> u64 {
    120
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            approvals: true,
            finished: true,
            failed: true,
            chat_replies: true,
            sound: true,
            fallback_osascript: true,
            min_run_secs: default_min_run_secs(),
            idle_away_secs: default_idle_away_secs(),
        }
    }
}

impl NotificationsConfig {
    /// `min_run_secs` clamped to 0..=3600.
    pub fn effective_min_run_secs(&self) -> u64 {
        self.min_run_secs.min(3600)
    }

    /// `idle_away_secs`: 0 stays 0 (idle detection off); anything else is
    /// clamped to 30..=3600.
    pub fn effective_idle_away_secs(&self) -> u64 {
        match self.idle_away_secs {
            0 => 0,
            n => n.clamp(30, 3600),
        }
    }
}

/// The whole configuration.
///
/// `Debug` is derived deliberately: every secret-bearing field is a sub-struct
/// (`memory`, `ccd`, `providers`, `hermes`, `gateway`) with its own redacting
/// `Debug`, and a derived `Debug` calls each field's impl — so the whole-config
/// dump is transitively redacted (H4). The redaction test asserts on this full
/// `Config` dump to prove that guarantee, not just the leaves.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub appearance: AppearanceConfig,
    /// Name of the provider used on launch; falls back to the first provider when
    /// absent or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    /// Model used on launch; falls back to that provider's first model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default)]
    pub memory: MemoryConfig,
    #[serde(default)]
    pub feed: FeedConfig,
    /// The `[outline]` connector (daily-driver P12). Absent section = the
    /// family is disabled and the `outline_*` tools are omitted.
    #[serde(default)]
    pub outline: OutlineConfig,
    /// The `[gitea]` connector (daily-driver P12). Same gating as
    /// [`Config::outline`].
    #[serde(default)]
    pub gitea: GiteaConfig,
    #[serde(default)]
    pub ccd: CcdConfig,
    #[serde(default)]
    pub hermes: HermesConfig,
    #[serde(default)]
    pub gateway: GatewayConfig,
    #[serde(default)]
    pub engine: EngineConfig,
    /// Additional in-app engines (`[[engines]]`), in picker order after the
    /// primary `[engine]`. Absent = none: behaviour is exactly the single-engine
    /// one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<ExtraEngineConfig>,
    #[serde(default)]
    pub mcp: McpConfig,
    #[serde(default)]
    pub work: WorkConfig,
    /// The pinned Chat executor (daily-driver P9): Chat has no picker; every
    /// Chat turn runs through this agent, parsed with the `default_executor`
    /// grammar. Absent section = `agent:claude:haiku`.
    #[serde(default)]
    pub chat: ChatConfig,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    /// The `[notifications]` section (N1). Absent = every kind enabled.
    #[serde(default)]
    pub notifications: NotificationsConfig,
    /// The `[graduation]` ledger (daily-driver P8b). Absent section = the
    /// default ledger path; `ledger = ""` disables recording.
    #[serde(default)]
    pub graduation: GraduationConfig,
    /// Local Build targets — optional `[build_targets.<id>]` bindings
    /// (daily-driver P2d). An absent section means no build targets, and the
    /// app behaves exactly as before.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub build_targets: BTreeMap<String, BuildTargetConfig>,
    /// Ordered — the provider/model chip cycles models in this order.
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
}

/// One selectable entry in the model chip's cycle: a provider paired with one of
/// its models, flattened across all providers in config order.
#[derive(Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub provider: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    /// Provider-configured static key (see [`ProviderConfig::api_key`]); carried
    /// through so wire-up attaches it without re-reading the provider list.
    pub api_key: Option<String>,
}

// Custom `Debug` so a debug-logged model choice never leaks the provider
// `api_key` copied out of config (H4); presence stays observable.
impl std::fmt::Debug for ModelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelChoice")
            .field("provider", &self.provider)
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &redact_opt_secret(&self.api_key))
            .finish()
    }
}

/// Where a loaded [`Config`] came from (config provenance, FIRST-RUN-SPEC task 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigSource {
    /// No candidate config path existed; the loaded config is [`Config::builtin`].
    Missing,
    /// The config was read and parsed from this path — the first existing
    /// candidate, including an existing but empty (fully default) file.
    Loaded(PathBuf),
}

impl Config {
    /// Built-in defaults used when no config file is present. Awili is a thin
    /// surface over the Hermes Agent runtime (2026-06-19 architecture pivot), so
    /// the coherent default is provider `hermes` / model `hermes-agent`; Anthropic
    /// stays as a direct-API alternative. The local-Ollama default was dropped
    /// per the pivot (no in-app local model serving). Keeps a checkout runnable.
    pub fn builtin() -> Self {
        Config {
            appearance: AppearanceConfig::default(),
            work: WorkConfig::default(),
            chat: ChatConfig::default(),
            models: ModelsConfig::default(),
            skills: SkillsConfig::default(),
            notifications: NotificationsConfig::default(),
            graduation: GraduationConfig::default(),
            build_targets: BTreeMap::new(),
            default_provider: Some("hermes".to_string()),
            default_model: Some("hermes-agent".to_string()),
            memory: MemoryConfig::default(),
            feed: FeedConfig::default(),
            outline: OutlineConfig::default(),
            gitea: GiteaConfig::default(),
            ccd: CcdConfig::default(),
            hermes: HermesConfig::default(),
            gateway: GatewayConfig::default(),
            engine: EngineConfig::default(),
            engines: Vec::new(),
            mcp: McpConfig::default(),
            providers: vec![
                ProviderConfig {
                    name: "hermes".to_string(),
                    kind: ProviderKind::Hermes,
                    base_url: default_hermes_base_url(),
                    models: vec!["hermes-agent".to_string()],
                    enabled: true,
                    api_key: None,
                },
                ProviderConfig {
                    name: "anthropic".to_string(),
                    kind: ProviderKind::Anthropic,
                    base_url: "https://api.anthropic.com".to_string(),
                    models: vec!["claude-opus-4-8".to_string()],
                    enabled: true,
                    api_key: None,
                },
            ],
        }
    }

    /// A configuration with no providers and no memory URL — every network path is
    /// inert. Used by the app when an existing config file is malformed: rather
    /// than silently falling back to built-in endpoints (and possibly sending
    /// traffic somewhere the user never selected), the app disables networking and
    /// surfaces the error (review SF7).
    pub fn networkless() -> Config {
        Config {
            appearance: AppearanceConfig::default(),
            work: WorkConfig::default(),
            chat: ChatConfig::default(),
            models: ModelsConfig::default(),
            skills: SkillsConfig::default(),
            notifications: NotificationsConfig::default(),
            graduation: GraduationConfig::default(),
            build_targets: BTreeMap::new(),
            default_provider: None,
            default_model: None,
            memory: MemoryConfig {
                base_url: String::new(),
                token: String::new(),
                min_priority: MIN_PRIORITY_FLOOR,
                recall: false,
                write: false,
            },
            feed: FeedConfig::default(),
            // Connectors are network paths too: networkless means disabled.
            outline: OutlineConfig {
                enabled: false,
                ..OutlineConfig::default()
            },
            gitea: GiteaConfig {
                enabled: false,
                ..GiteaConfig::default()
            },
            ccd: CcdConfig {
                base_url: String::new(),
                ..CcdConfig::default()
            },
            hermes: HermesConfig {
                base_url: String::new(),
                api_key: String::new(),
            },
            gateway: GatewayConfig {
                dashboard_url: String::new(),
            },
            engine: EngineConfig::default(),
            engines: Vec::new(),
            mcp: McpConfig::default(),
            providers: Vec::new(),
        }
    }

    /// Candidate config paths, in priority order:
    /// 1. `~/.config/awili/config.toml` — the XDG-style path, stable across every
    ///    platform and the one the README documents.
    /// 2. the platform config dir via `dirs::config_dir()` — on macOS this is
    ///    `~/Library/Application Support/awili/config.toml`.
    ///
    /// The first path that exists wins. On Linux the two coincide (both `~/.config`)
    /// so the second is de-duplicated (review finding A: the app read the platform
    /// dir while the README promised `~/.config`, so a Mac config was ignored).
    pub fn config_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        // Windows has one profile location: `%APPDATA%\Awili\config.toml` (the
        // roaming config folder). No `~/.config` lookup there.
        #[cfg(windows)]
        {
            if let Some(cfg) = dirs::config_dir() {
                paths.push(cfg.join("Awili").join("config.toml"));
            }
            return paths;
        }
        #[cfg(not(windows))]
        {
            let xdg_home =
                dirs::home_dir().map(|h| h.join(".config").join("awili").join("config.toml"));
            let platform = dirs::config_dir().map(|c| c.join("awili").join("config.toml"));
            // Linux: `$XDG_CONFIG_HOME/awili/config.toml` ranks first (it is
            // `~/.config` unless the person moved it); `~/.config` stays a
            // fallback. macOS keeps its order: `~/.config` first.
            let ordered = if cfg!(target_os = "linux") {
                [platform, xdg_home]
            } else {
                [xdg_home, platform]
            };
            for p in ordered.into_iter().flatten() {
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
            paths
        }
    }

    /// The primary config path (`~/.config/awili/config.toml`), if a home dir is
    /// known. Kept for callers that want to report *where* config should live.
    pub fn default_path() -> Option<PathBuf> {
        Self::config_paths().into_iter().next()
    }

    /// Load from the first existing [candidate path](Self::config_paths), or fall
    /// back to [`Config::builtin`] when no file is present. A malformed file is a
    /// hard error (returned to the caller), so a typo is surfaced rather than
    /// silently ignored.
    ///
    /// Provenance is discarded here; use [`Self::load_with_source`] for it.
    pub fn load() -> Result<Config, ConfigError> {
        Self::load_with_source().map(|(config, _source)| config)
    }

    /// [`Self::load`] plus provenance: where the config came from.
    ///
    /// The first existing [candidate path](Self::config_paths) wins — including
    /// an existing but empty file, which is fully valid TOML and loads with all
    /// defaults ([`ConfigSource::Loaded`], not `Missing`). An existing but
    /// invalid or unreadable candidate is a hard error even when a later
    /// candidate is valid: a typo is surfaced, never masked by a fallback.
    /// When no candidate exists, [`Config::builtin`] is returned with
    /// [`ConfigSource::Missing`].
    pub fn load_with_source() -> Result<(Config, ConfigSource), ConfigError> {
        Self::load_from_paths(&Self::config_paths())
    }

    /// Load from the first existing path of an explicit ordered candidate list,
    /// with the same first-existing-wins and hard-error semantics as
    /// [`Self::load_with_source`] (which calls this with [`Self::config_paths`]).
    /// "Existing" means the filesystem entry exists ([`std::fs::symlink_metadata`]),
    /// so a dangling symlink counts as existing and is loaded: it is unreadable,
    /// hence an authoritative [`ConfigError::Io`], never skipped in favour of a
    /// later candidate.
    /// Tests use this to exercise controlled candidates without HOME or
    /// environment mutation.
    pub fn load_from_paths(paths: &[PathBuf]) -> Result<(Config, ConfigSource), ConfigError> {
        for path in paths {
            match std::fs::symlink_metadata(path) {
                Ok(_) => {
                    // Windows: tighten a config an older build or another tool
                    // left with a broader access list (the Unix mode check's
                    // counterpart). If that cannot be done the file is NOT
                    // trusted with secrets: plaintext keys are dropped from
                    // what is returned (`secret:` references are kept) and the
                    // reason is published for the UI banner.
                    let problem = protect_loaded(path);
                    let mut config = Self::load_from(path)?;
                    set_file_protection_problem(problem.clone());
                    if problem.is_some() {
                        config.scrub_plaintext_secrets();
                    }
                    return Ok((config, ConfigSource::Loaded(path.clone())));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(ConfigError::Io(e)),
            }
        }
        set_file_protection_problem(None);
        Ok((Self::builtin(), ConfigSource::Missing))
    }

    /// Load and parse a specific file (used by `load` and by tests).
    pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        Self::parse(&text)
    }

    /// Parse TOML text into a [`Config`]. Pure — the unit-testable core. Rejects a
    /// `memory.min_priority` below [`MIN_PRIORITY_FLOOR`]: the spec requires every
    /// injected directive to be priority ≥ 90, so a lower floor is a config error
    /// surfaced at load rather than silently honoured (review slice 2, blocker 2).
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text).map_err(ConfigError::Toml)?;
        if config.memory.min_priority < MIN_PRIORITY_FLOOR {
            return Err(ConfigError::MinPriorityTooLow(config.memory.min_priority));
        }
        // Install trust is a permanent, card-free grant. A typo'd or hostile
        // name must not acquire it silently — fail the whole load, matching
        // this module's "a malformed file is surfaced, never ignored" rule.
        if let Some(s) = config
            .mcp
            .servers
            .iter()
            .find(|s| s.trust == McpTrust::Install && s.name != INSTALL_TRUSTED_SERVER)
        {
            return Err(ConfigError::InstallTrustNotReserved(s.name.clone()));
        }
        config.validate_build_targets()?;
        config.validate_engines()?;
        Ok(config)
    }

    /// Load-time validation of `[[engines]]`: every id valid and unique. A bad
    /// list fails the load naming the id; nothing is dropped or merged.
    fn validate_engines(&self) -> Result<(), ConfigError> {
        let mut seen = std::collections::BTreeSet::new();
        for e in &self.engines {
            if !is_valid_engine_id(&e.id) {
                return Err(ConfigError::InvalidEngine {
                    id: e.id.clone(),
                    message: "id must be 1..=64 characters of A-Z a-z 0-9 . _ -".into(),
                });
            }
            if !seen.insert(e.id.as_str()) {
                return Err(ConfigError::InvalidEngine {
                    id: e.id.clone(),
                    message: "duplicate id: each [[engines]] entry needs its own".into(),
                });
            }
        }
        Ok(())
    }

    /// Load-time validation of `[build_targets.<id>]` (daily-driver P2d):
    /// every field of every target must be present, non-empty, and
    /// dispatch-safe. An invalid target is a config error naming the field —
    /// the load fails closed; nothing is silently dropped or re-routed.
    fn validate_build_targets(&self) -> Result<(), ConfigError> {
        for (id, target) in &self.build_targets {
            if id.is_empty() || id.len() > 256 || id.contains('\0') || id.starts_with('-') {
                return Err(ConfigError::InvalidBuildTarget {
                    target: id.clone(),
                    field: "(target id)".into(),
                    message: "must be a 1..=256 char id without NUL or a leading '-'".into(),
                });
            }
            check_build_target_field(
                id,
                "ssh_host",
                &target.ssh_host,
                "must be a non-empty host without a leading '-' or shell metacharacters",
                |v| {
                    !v.is_empty()
                        && !v.starts_with('-')
                        && v.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._@:-".contains(&b))
                },
            )?;
            check_build_target_field(
                id,
                "runner_path",
                &target.runner_path,
                "must be an absolute, shell-safe executable path",
                |v| {
                    v.starts_with('/')
                        && v.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
                },
            )?;
            for (field, value) in [
                ("repo_id", &target.repo_id),
                ("model", &target.model),
                ("verify_profile", &target.verify_profile),
            ] {
                check_build_target_field(
                    id,
                    field,
                    value,
                    "must be a 1..=256 char value without NUL",
                    |v| !v.is_empty() && v.len() <= 256 && !v.contains('\0'),
                )?;
            }
        }
        Ok(())
    }

    /// The flattened model cycle for the chip: every ENABLED provider's models
    /// in order. A disabled provider stays configured but offers nothing
    /// (slice-11 spec §1).
    pub fn model_choices(&self) -> Vec<ModelChoice> {
        self.providers
            .iter()
            .filter(|p| p.enabled)
            .flat_map(|p| {
                p.models.iter().map(move |m| ModelChoice {
                    provider: p.name.clone(),
                    kind: p.kind,
                    base_url: p.base_url.clone(),
                    model: m.clone(),
                    api_key: p.api_key.clone(),
                })
            })
            // An embedding model can't chat; never offer it as a chat target
            // (P1.2 — e.g. ollama's nomic-embed-text leaking into the picker).
            .filter(|c| !is_embedding_model(&c.model))
            .collect()
    }

    /// Index into [`model_choices`](Self::model_choices) the app should start on:
    /// the configured `default_provider` + `default_model` if both match, else the
    /// first model of `default_provider`, else 0.
    pub fn default_choice_index(&self) -> usize {
        let choices = self.model_choices();
        if choices.is_empty() {
            return 0;
        }
        if let Some(provider) = &self.default_provider {
            // Exact provider+model match wins.
            if let Some(model) = &self.default_model {
                if let Some(i) = choices
                    .iter()
                    .position(|c| &c.provider == provider && &c.model == model)
                {
                    return i;
                }
            }
            // Else first model of the named provider.
            if let Some(i) = choices.iter().position(|c| &c.provider == provider) {
                return i;
            }
        }
        0
    }

    /// Non-fatal coherence warnings about the default provider/model wiring
    /// (P1.1). Returned for the caller to log — a misconfigured default should
    /// never crash the app, only warn and fall back to a usable choice.
    pub fn default_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let Some(provider) = &self.default_provider else {
            return warnings;
        };
        let Some(p) = self.providers.iter().find(|p| &p.name == provider) else {
            warnings.push(format!(
                "default_provider \"{provider}\" is not a configured provider"
            ));
            return warnings;
        };
        if let Some(model) = &self.default_model {
            if !p.models.iter().any(|m| m == model) {
                warnings.push(format!(
                    "default_model \"{model}\" is not offered by default_provider \"{provider}\" \
                     (its models: {}); the app will start on that provider's first model instead",
                    if p.models.is_empty() {
                        "none".to_string()
                    } else {
                        p.models.join(", ")
                    }
                ));
            }
        }
        warnings
    }
}

/// Whether a model name denotes an embedding model (which cannot do chat).
/// Heuristic by name — embedding endpoints are never valid chat targets, so a
/// stray one in the provider list must not appear in the model picker (P1.2).
pub fn is_embedding_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("embed") // nomic-embed-text, text-embedding-3, *-embedding-*
        || m.starts_with("bge-")
        || m.starts_with("gte-")
        || m.starts_with("e5-")
}

impl Default for Config {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Serialize `config` to TOML and write it to `path` ATOMICALLY: the text lands
/// in a unique 0600 temp file (fsync'd) first and is renamed over `path` only
/// once fully written, so a crash mid-save can never leave a torn config
/// (slice-11 spec §2). The temp's mode is the stricter of 0600 and any existing
/// file's mode, so a save never widens the config's permissions (S6(a)).
///
/// The serialized text is re-validated through [`Config::parse`] before any
/// file is touched — a semantically invalid `Config` (e.g. a programmatic
/// `min_priority` below the floor) is an error, not a write.
///
/// This is the plain serde write path: it emits exactly what [`Config`] models.
/// Comments and foreign tables (`[voice]`) do not survive it — the settings
/// panel edits an existing file through [`ConfigDoc`] instead, which shares the
/// same atomic write gate ([`save_validated`]).
pub fn write_config(config: &Config, path: &Path) -> Result<(), ConfigError> {
    let text = toml::to_string(config).map_err(|e| ConfigError::Syntax(e.to_string()))?;
    Config::parse(&text)?;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            crate::ensure_private_app_dir(parent).map_err(ConfigError::Io)?;
        }
    }
    write_temp_and_publish(path, &text)
}

/// One field of a `[build_targets.<id>]` entry failed load-time validation;
/// the error names the target AND the field (daily-driver P2d).
fn check_build_target_field(
    target: &str,
    field: &str,
    value: &str,
    expected: &str,
    ok: impl Fn(&str) -> bool,
) -> Result<(), ConfigError> {
    if ok(value) {
        Ok(())
    } else {
        Err(ConfigError::InvalidBuildTarget {
            target: target.to_string(),
            field: field.to_string(),
            message: expected.to_string(),
        })
    }
}

/// Why a config failed to load.
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Toml(toml::de::Error),
    /// `memory.min_priority` was below [`MIN_PRIORITY_FLOOR`].
    MinPriorityTooLow(i64),
    /// Text that is not even well-formed TOML (from the editing layer, which
    /// parses with `toml_edit` before semantic validation).
    Syntax(String),
    /// `trust = "install"` on a server other than [`INSTALL_TRUSTED_SERVER`].
    InstallTrustNotReserved(String),
    /// A `[build_targets.<id>]` entry failed load-time validation; `field`
    /// names the offending field (or `(target id)`) and `message` why.
    InvalidBuildTarget {
        target: String,
        field: String,
        message: String,
    },
    /// An `[[engines]]` entry failed load-time validation (bad or duplicate id).
    InvalidEngine {
        id: String,
        message: String,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "reading config: {e}"),
            ConfigError::Toml(e) => write!(f, "parsing config: {e}"),
            ConfigError::MinPriorityTooLow(n) => write!(
                f,
                "memory.min_priority = {n} is below the required floor of {MIN_PRIORITY_FLOOR}"
            ),
            ConfigError::Syntax(e) => write!(f, "TOML syntax: {e}"),
            ConfigError::InstallTrustNotReserved(name) => write!(
                f,
                "mcp server {name:?}: trust = \"install\" is reserved for \
                 {INSTALL_TRUSTED_SERVER:?} — every other server must use \
                 trust = \"prompt\""
            ),
            ConfigError::InvalidBuildTarget {
                target,
                field,
                message,
            } => write!(f, "build_targets.{target}.{field}: {message}"),
            ConfigError::InvalidEngine { id, message } => {
                write!(f, "engines (id {id:?}): {message}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// H4 regression: `format!("{:?}", config)` must not disclose ANY secret —
    /// provider api_key, memory bearer token, ccd bearer token, or the engine
    /// api_key — and must mark them redacted. Asserts on the whole-`Config`
    /// dump (proves the derived `Config` Debug transitively redacts via its
    /// sub-structs).
    #[test]
    fn config_debug_redacts_all_secrets() {
        const PROVIDER_KEY: &str = "sk-provider-SECRET-aaa";
        const MEMORY_TOKEN: &str = "mem-bearer-SECRET-bbb";
        const CCD_TOKEN: &str = "ccd-bearer-SECRET-ccc";
        const ENGINE_KEY: &str = "sk-engine-SECRET-ddd";

        let mut cfg = Config::builtin();
        cfg.memory.token = MEMORY_TOKEN.to_string();
        cfg.ccd.token = CCD_TOKEN.to_string();
        cfg.engine.api_key = ENGINE_KEY.to_string();
        cfg.providers[0].api_key = Some(PROVIDER_KEY.to_string());

        let dump = format!("{cfg:?}");

        for secret in [PROVIDER_KEY, MEMORY_TOKEN, CCD_TOKEN, ENGINE_KEY] {
            assert!(
                !dump.contains(secret),
                "Debug output leaked a secret ({secret}): {dump}"
            );
        }
        assert!(
            dump.contains("redacted"),
            "redaction marker must be present: {dump}"
        );

        // The leaf structs redact in isolation too (not only inside Config).
        assert!(!format!("{:?}", cfg.providers[0]).contains(PROVIDER_KEY));
        assert!(!format!("{:?}", cfg.memory).contains(MEMORY_TOKEN));
        assert!(!format!("{:?}", cfg.ccd).contains(CCD_TOKEN));
        assert!(!format!("{:?}", cfg.engine).contains(ENGINE_KEY));
        // Presence stays observable: an unset key prints as such, a set one
        // is marked redacted — never the value.
        let unset = format!("{:?}", EngineConfig::default());
        assert!(
            unset.contains("<unset>"),
            "unset key must show as unset: {unset}"
        );
        assert!(
            !unset.contains("<redacted>"),
            "an unset key must not claim redaction: {unset}"
        );
    }

    /// RC2-SVC2 (F0384): the MCP server `env`, the agents' `interlock_env` and
    /// a credential setting row never print their values through `Debug`;
    /// the names stay visible.
    #[test]
    fn rc2svc2_env_maps_and_setting_rows_redact_values_in_debug() {
        const MCP_SECRET: &str = "mcp-env-SECRET-eee";
        const INTERLOCK_SECRET: &str = "interlock-env-SECRET-fff";
        const ROW_SECRET: &str = "row-api-key-SECRET-ggg";
        let mut cfg = Config::builtin();
        let mut server = McpServerConfig {
            name: "probe".into(),
            command: "/bin/probe".into(),
            args: vec!["--flag".into()],
            env: BTreeMap::new(),
            trust: McpTrust::default(),
            modes: default_mcp_modes(),
            enabled: true,
        };
        server.env.insert("PROBE_API_KEY".into(), MCP_SECRET.into());
        server.env.insert("EMPTY".into(), String::new());
        cfg.mcp.servers.push(server.clone());
        let mut memory = AgentMemoryConfig::default();
        memory
            .interlock_env
            .insert("FOREMAN_TOKEN".into(), INTERLOCK_SECRET.into());
        cfg.work.agents.memory = Some(memory.clone());

        let dumps = [
            format!("{server:?}"),
            format!("{memory:?}"),
            format!("{cfg:?}"),
        ];
        for dump in &dumps {
            for secret in [MCP_SECRET, INTERLOCK_SECRET] {
                assert!(!dump.contains(secret), "Debug leaked {secret}: {dump}");
            }
        }
        assert!(dumps[0].contains("PROBE_API_KEY") && dumps[0].contains("<redacted>"));
        assert!(
            dumps[0].contains("<unset>"),
            "an empty value shows as unset"
        );
        assert!(dumps[1].contains("FOREMAN_TOKEN"));

        let row = doc::SettingEntry {
            section: "providers[0]".into(),
            key: "api_key".into(),
            target: doc::Target::TopLevel {
                key: "api_key".into(),
            },
            value: ROW_SECRET.into(),
            kind: doc::ValueKind::Str,
        };
        let dump = format!("{row:?}");
        assert!(!dump.contains(ROW_SECRET), "setting row leaked: {dump}");
    }

    /// RC2-SVC2 (F0382): a misspelled key in `[outline]` or `[gitea]` is a
    /// load error naming the key — never a silent fall-back to the defaults.
    #[test]
    fn rc2svc2_unknown_connector_keys_are_refused() {
        for text in [
            "[outline]\nenabeld = true\n",
            "[gitea]\nenabled = true\nssh_prot = 22\n",
        ] {
            let err = Config::parse(text).expect_err("a typo must not load");
            let shown = err.to_string();
            assert!(
                shown.contains("enabeld") || shown.contains("ssh_prot"),
                "the error names the key: {shown}"
            );
        }
        let ok = Config::parse("[outline]\nenabled = true\n[gitea]\nssh_port = 22\n").unwrap();
        assert!(ok.outline.enabled);
        assert_eq!(ok.gitea.ssh_port, 22);
    }

    /// H4 completion: `ModelChoice` carries the provider `api_key` copied out of
    /// config, so its `Debug` must redact it too. The choice is built through the
    /// real `model_choices()` flatten, not by hand, so this isn't a tautology.
    #[test]
    fn model_choice_debug_redacts_api_key() {
        const SECRET: &str = "sk-SECRET123";

        let mut cfg = Config::builtin();
        cfg.providers[0].api_key = Some(SECRET.to_string());
        let choice = cfg
            .model_choices()
            .into_iter()
            .find(|c| c.provider == cfg.providers[0].name)
            .expect("the keyed provider offers a model choice");
        assert_eq!(
            choice.api_key.as_deref(),
            Some(SECRET),
            "the key is carried through"
        );

        let dump = format!("{choice:?}");
        assert!(
            !dump.contains(SECRET),
            "ModelChoice Debug leaked the api_key: {dump}"
        );
        assert!(
            dump.contains("redacted"),
            "redaction marker must be present: {dump}"
        );
    }

    const SAMPLE: &str = r#"
default_provider = "anthropic"
default_model = "claude-opus-4-8"

[memory]
base_url = "http://10.0.0.5:9000"
min_priority = 95

[ccd]
base_url = "http://127.0.0.1:8870"
token = "sekret"
repo = "/home/example/Projects/awili"
branch = "feat/x"
verify_profile = "rust_check"

[hermes]
base_url = "http://127.0.0.1:8642/v1"
api_key = "bearer-xyz"

[[providers]]
name = "ollama"
kind = "ollama"
base_url = "http://localhost:11434"
models = ["llama3.1:8b", "qwen2.5:14b"]

[[providers]]
name = "anthropic"
kind = "anthropic"
base_url = "https://api.anthropic.com"
models = ["claude-opus-4-8"]

[[providers]]
name = "hermes"
kind = "hermes"
base_url = "http://10.77.68.86:8080"
models = ["hermes-4"]
api_key = "local"
"#;

    #[test]
    fn parses_full_config() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.default_provider.as_deref(), Some("anthropic"));
        assert_eq!(c.memory.base_url, "http://10.0.0.5:9000");
        assert_eq!(c.memory.min_priority, 95);
        assert_eq!(c.providers.len(), 3);
        assert_eq!(c.providers[2].kind, ProviderKind::Hermes);
    }

    #[test]
    fn parses_ccd_table() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.ccd.base_url, "http://127.0.0.1:8870");
        assert_eq!(c.ccd.token, "sekret");
        assert_eq!(c.ccd.repo, "/home/example/Projects/awili");
        assert_eq!(c.ccd.branch, "feat/x");
        assert_eq!(c.ccd.verify_profile.as_deref(), Some("rust_check"));
    }

    #[test]
    fn parses_hermes_table() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.hermes.base_url, "http://127.0.0.1:8642/v1");
        assert_eq!(c.hermes.api_key, "bearer-xyz");
    }

    #[test]
    fn hermes_defaults_when_table_absent() {
        // No [hermes] table -> runnable default pointing at the local /v1 bind,
        // no key (the server will 401, surfaced rather than silently mis-routed).
        let c = Config::parse("").unwrap();
        assert_eq!(c.hermes.base_url, "http://127.0.0.1:8642/v1");
        assert!(c.hermes.api_key.is_empty());
        // networkless disables hermes by emptying the base_url.
        assert!(Config::networkless().hermes.base_url.is_empty());
    }

    #[test]
    fn ccd_defaults_when_table_absent() {
        // No [ccd] table -> runnable default pointing at the local bind, no token.
        let c = Config::parse("").unwrap();
        assert_eq!(c.ccd.base_url, "http://127.0.0.1:8870");
        assert!(c.ccd.token.is_empty());
        assert!(c.ccd.verify_profile.is_none());
        // networkless disables ccd by emptying the base_url.
        assert!(Config::networkless().ccd.base_url.is_empty());
    }

    // ---- P12 connector sections ------------------------------------------

    #[test]
    fn empty_config_names_no_fleet_host() {
        // An empty config.toml must not carry anyone's addresses: memory,
        // outline and gitea are all unset, and memory is off.
        let c = Config::parse("").unwrap();
        assert!(c.memory.base_url.is_empty());
        assert!(!c.memory.recall && !c.memory.write);
        let text = format!("{:?}{:?}{:?}", c.memory, c.outline, c.gitea);
        assert!(!text.contains("192.168."), "{text}");
        assert!(!text.contains("ts.net"), "{text}");
        assert!(!text.contains("foreman"), "{text}");
        assert_eq!(c.work.agents.dsh_env_file(), "");
    }

    #[test]
    fn connector_sections_default_when_absent() {
        // No [outline]/[gitea] tables -> both families disabled, and no
        // host, URL or credential path at all: a fresh install names no one's
        // fleet.
        let c = Config::parse("").unwrap();
        assert!(!c.outline.enabled);
        assert_eq!(c.outline.base_url, "");
        assert_eq!(c.outline.secret_file, "");
        assert_eq!(c.outline.secret_key, "OUTLINE_API_KEY");
        assert!(!c.gitea.enabled);
        assert_eq!(c.gitea.base_url, "");
        assert_eq!(c.gitea.secret_file, "");
        assert!(c.gitea.secret_key.is_none(), "whole-file token by default");
        assert_eq!(c.gitea.ssh_host, "");
        assert_eq!(c.gitea.ssh_port, 0);
        // networkless keeps both families disabled.
        let net = Config::networkless();
        assert!(!net.outline.enabled && !net.gitea.enabled);
    }

    #[test]
    fn connector_sections_parse_partial_tables() {
        let c = Config::parse(
            r#"
[outline]
enabled = true
secret_key = "OUTLINE_API_KEY_ROTATED"

[gitea]
enabled = true
secret_key = "GITEA_TOKEN"
ssh_port = 22
"#,
        )
        .unwrap();
        assert!(c.outline.enabled);
        // Unset outline keys keep the (empty) defaults.
        assert_eq!(c.outline.base_url, "");
        assert_eq!(c.outline.secret_file, "");
        assert_eq!(c.outline.secret_key, "OUTLINE_API_KEY_ROTATED");
        assert!(c.gitea.enabled);
        assert_eq!(c.gitea.secret_key.as_deref(), Some("GITEA_TOKEN"));
        assert_eq!(c.gitea.ssh_port, 22);
        // Unset gitea keys keep the defaults.
        assert_eq!(c.gitea.base_url, "");
        assert_eq!(c.gitea.ssh_host, "");
        // A table with only `enabled = false` still parses (explicit off).
        let off: Config =
            toml::from_str("[outline]\nenabled = false\n[gitea]\nenabled = false").unwrap();
        assert!(!off.outline.enabled && !off.gitea.enabled);
    }

    #[test]
    fn api_key_parses_and_carries_onto_model_choice() {
        let c = Config::parse(SAMPLE).unwrap();
        // The hermes/spark provider block declares api_key = "local".
        assert_eq!(c.providers[2].api_key.as_deref(), Some("local"));
        let choices = c.model_choices();
        // It is threaded onto the flattened choice (hermes is the last entry).
        assert_eq!(choices[3].provider, "hermes");
        assert_eq!(choices[3].api_key.as_deref(), Some("local"));
    }

    #[test]
    fn absent_api_key_defaults_to_none() {
        let c = Config::parse(SAMPLE).unwrap();
        // ollama and anthropic declare no api_key.
        assert!(c.providers[0].api_key.is_none());
        assert!(c.providers[1].api_key.is_none());
        let choices = c.model_choices();
        assert!(choices[0].api_key.is_none());
        assert!(choices[2].api_key.is_none());
    }

    #[test]
    fn model_choices_flatten_in_order() {
        let c = Config::parse(SAMPLE).unwrap();
        let choices = c.model_choices();
        assert_eq!(choices.len(), 4); // 2 + 1 + 1
        assert_eq!(choices[0].provider, "ollama");
        assert_eq!(choices[0].model, "llama3.1:8b");
        assert_eq!(choices[2].provider, "anthropic");
        assert_eq!(choices[3].provider, "hermes");
    }

    /// DeepSeek's cloud API reaches the pickers as an ordinary provider: a
    /// `[[providers]]` row with `kind = "openai"` flattens into one choice per
    /// declared model, each carrying the provider's own base_url and static
    /// key — exactly what `provider_build` routes to `OpenAiCompat`. This is
    /// the choice-construction seam; the wire-level routing is pinned in
    /// `awili-app`'s provider_route tests. Key is synthetic, never real.
    #[test]
    fn deepseek_openai_provider_yields_picker_choices() {
        let cfg = r#"
[[providers]]
name = "deepseek"
kind = "openai"
base_url = "https://api.deepseek.com"
models = ["deepseek-flash", "deepseek-v4-pro"]
api_key = "sk-test-000"
"#;
        let c = Config::parse(cfg).unwrap();
        let choices = c.model_choices();
        assert_eq!(choices.len(), 2, "{choices:?}");
        for c in &choices {
            assert_eq!(c.provider, "deepseek");
            assert_eq!(c.kind, ProviderKind::OpenAi);
            assert_eq!(c.base_url, "https://api.deepseek.com");
            assert_eq!(c.api_key.as_deref(), Some("sk-test-000"));
        }
        assert_eq!(choices[0].model, "deepseek-flash");
        assert_eq!(choices[1].model, "deepseek-v4-pro");
        // `deepseek` is NOT treated as an embedding/non-chat model — both
        // entries survive the chat-target filter above, but say it plainly.
        assert_eq!(
            choices
                .iter()
                .filter(|c| !is_embedding_model(&c.model))
                .count(),
            2
        );
    }

    #[test]
    fn default_choice_prefers_exact_provider_and_model() {
        let c = Config::parse(SAMPLE).unwrap();
        // anthropic + claude-opus-4-8 is index 2 in the flattened list.
        assert_eq!(c.default_choice_index(), 2);
    }

    #[test]
    fn default_choice_falls_back_to_first_model_of_provider() {
        let mut c = Config::parse(SAMPLE).unwrap();
        c.default_model = Some("nonexistent".to_string());
        // Unknown model -> first anthropic model, still index 2.
        assert_eq!(c.default_choice_index(), 2);
        c.default_provider = Some("ollama".to_string());
        assert_eq!(c.default_choice_index(), 0);
    }

    #[test]
    fn default_choice_falls_back_to_zero_when_provider_unknown() {
        let mut c = Config::parse(SAMPLE).unwrap();
        c.default_provider = Some("missing".to_string());
        c.default_model = None;
        assert_eq!(c.default_choice_index(), 0);
    }

    #[test]
    fn memory_min_priority_defaults_to_90() {
        let c = Config::parse(
            r#"
[[providers]]
name = "ollama"
kind = "ollama"
base_url = "http://localhost:11434"
models = ["x"]

[memory]
base_url = "http://h:1"
"#,
        )
        .unwrap();
        assert_eq!(c.memory.min_priority, 90);
    }

    #[test]
    fn empty_config_uses_serde_defaults() {
        // No providers, no memory table -> defaults fill in, no panic.
        let c = Config::parse("").unwrap();
        assert!(c.providers.is_empty());
        assert_eq!(c.memory.min_priority, 90);
        assert_eq!(c.default_choice_index(), 0);
        assert!(c.model_choices().is_empty());
    }

    #[test]
    fn builtin_is_runnable() {
        let c = Config::builtin();
        assert!(!c.model_choices().is_empty());
        assert_eq!(c.model_choices()[0].provider, "hermes");
    }

    #[test]
    fn malformed_toml_is_an_error() {
        assert!(Config::parse("this is not = = toml").is_err());
    }

    #[test]
    fn embedding_models_excluded_from_choices() {
        // P1.2: an embedding model in the provider list must never be a chat
        // target.
        let cfg = r#"
[[providers]]
name = "ollama"
kind = "ollama"
base_url = "http://localhost:11434"
models = ["nomic-embed-text", "llama3.1:8b", "bge-large", "gte-base"]
"#;
        let c = Config::parse(cfg).unwrap();
        let choices = c.model_choices();
        let models: Vec<&str> = choices.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(models, vec!["llama3.1:8b"]);
        assert!(is_embedding_model("nomic-embed-text"));
        assert!(is_embedding_model("text-embedding-3-large"));
        assert!(!is_embedding_model("llama3.1:8b"));
        assert!(!is_embedding_model("hermes-agent"));
    }

    #[test]
    fn embedding_e5_prefix_and_case_fold_excluded() {
        // Guards the two `is_embedding_model` behaviours the flagship P1.2 test
        // above never exercises: the `e5-` prefix arm (e5-mistral is a real
        // embedding family) and the leading `to_ascii_lowercase()` — every
        // existing assertion passes a name that is already lowercase and never
        // an `e5-*` one, so dropping either the `e5-` branch OR the case-fold
        // would let an uppercase e5 embedding model leak straight into the
        // model picker and the slice-15 fallback chain as a dead retry target.
        assert!(is_embedding_model("e5-large-v2"));
        assert!(is_embedding_model("E5-Mistral-7B-Instruct")); // case-fold + e5-
        assert!(is_embedding_model("BGE-Large")); // case-fold + bge-
        assert!(!is_embedding_model("gemma3:12b"));
        // Prove exclusion through the runtime chain, not just the predicate: an
        // e5 entry must not survive into model_choices() (the fallback list).
        let cfg = r#"
[[providers]]
name = "ollama"
kind = "ollama"
base_url = "http://localhost:11434"
models = ["e5-large-v2", "qwen2.5:7b"]
"#;
        let c = Config::parse(cfg).unwrap();
        let choices = c.model_choices();
        let models: Vec<&str> = choices.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(models, vec!["qwen2.5:7b"]);
    }

    #[test]
    fn default_warnings_flag_out_of_provider_model() {
        // Coherent pair → no warning.
        let mut c = Config::builtin();
        assert!(
            c.default_warnings().is_empty(),
            "{:?}",
            c.default_warnings()
        );
        // Model not in the default provider → exactly one warning, no crash.
        c.default_model = Some("deepseek-flash".to_string());
        let w = c.default_warnings();
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("deepseek-flash"));
        // Unknown default provider → its own warning.
        c.default_provider = Some("ghost".to_string());
        assert!(c.default_warnings()[0].contains("ghost"));
    }

    #[test]
    fn min_priority_below_floor_is_rejected() {
        let bad = r#"
[memory]
base_url = "http://h:1"
min_priority = 0
"#;
        match Config::parse(bad) {
            Err(ConfigError::MinPriorityTooLow(0)) => {}
            other => panic!("expected MinPriorityTooLow(0), got {other:?}"),
        }
        // Exactly at the floor is accepted.
        assert!(Config::parse("[memory]\nbase_url=\"http://h:1\"\nmin_priority=90").is_ok());
        // Above the floor (tighter) is accepted.
        assert!(Config::parse("[memory]\nbase_url=\"http://h:1\"\nmin_priority=95").is_ok());
    }

    #[test]
    fn networkless_has_no_endpoints() {
        let c = Config::networkless();
        assert!(c.providers.is_empty());
        assert!(c.model_choices().is_empty());
        assert!(c.memory.base_url.is_empty());
    }

    /// The exact macOS app-data folder (what `main` used before the Linux port).
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_app_data_dir_is_pinned() {
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(APP_DIR_NAME, "Awili");
        assert_eq!(
            app_data_dir(),
            Some(home.join("Library/Application Support/Awili"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_config_paths_are_pinned() {
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(
            Config::config_paths(),
            vec![
                home.join(".config/awili/config.toml"),
                home.join("Library/Application Support/awili/config.toml"),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_config_is_under_roaming_appdata() {
        let appdata = std::env::var_os("APPDATA").expect("APPDATA is set");
        let want = std::path::PathBuf::from(appdata)
            .join("Awili")
            .join("config.toml");
        assert_eq!(Config::config_paths(), vec![want.clone()]);
        assert_eq!(Config::default_path(), Some(want));
    }

    #[cfg(unix)]
    #[test]
    fn private_app_dir_is_0700_under_a_root_and_plain_elsewhere() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let base = std::env::temp_dir().join(format!("awili-priv-{}", std::process::id()));
        let root = base.join("data");
        // A new app folder and everything made under the root is 0700.
        let leaf = root.join(APP_DIR_NAME).join("engine").join("logs");
        private_dir_under(&leaf, std::slice::from_ref(&root)).unwrap();
        for d in [&root, &root.join(APP_DIR_NAME), &leaf] {
            assert_eq!(mode(d), 0o700, "{}", d.display());
        }
        // An existing 0755 app folder is tightened.
        let old = base.join("old");
        std::fs::create_dir_all(old.join(APP_DIR_NAME)).unwrap();
        std::fs::set_permissions(old.join(APP_DIR_NAME), std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir_under(&old.join(APP_DIR_NAME).join("x"), std::slice::from_ref(&old)).unwrap();
        assert_eq!(mode(&old.join(APP_DIR_NAME)), 0o700);
        // Outside every root (a chosen export folder): ordinary create_dir_all.
        let chosen = base.join("export").join("here");
        private_dir_under(&chosen, std::slice::from_ref(&root)).unwrap();
        assert_ne!(mode(&chosen), 0o700);
        // A `..` path is never treated as under a root.
        let sneaky = root.join("..").join("sneaky-dir");
        private_dir_under(&sneaky, std::slice::from_ref(&root)).unwrap();
        assert_ne!(mode(&base.join("sneaky-dir")), 0o700);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn npm_config_prefix_must_be_absolute() {
        let rc = || Some("/home/u/rc".to_string());
        assert_eq!(pick_npm_prefix(Some("/opt/npm/"), rc).as_deref(), Some("/opt/npm/bin"));
        // Relative, blank or unset: the env value is ignored, `.npmrc` is used.
        assert_eq!(pick_npm_prefix(Some("rel/dir"), rc).as_deref(), Some("/home/u/rc/bin"));
        assert_eq!(pick_npm_prefix(Some("  "), rc).as_deref(), Some("/home/u/rc/bin"));
        assert_eq!(pick_npm_prefix(None, || None), None);
        assert_eq!(pick_npm_prefix(Some("."), || None), None);
    }

    #[test]
    fn npmrc_prefix_is_read_and_expanded() {
        let home = Path::new("/home/u");
        assert_eq!(
            parse_npmrc_prefix("; c\nprefix = ${HOME}/.npm-pkgs\n", home).as_deref(),
            Some("/home/u/.npm-pkgs")
        );
        assert_eq!(
            parse_npmrc_prefix("prefix=\"~/n\"", home).as_deref(),
            Some("/home/u/n")
        );
        assert_eq!(parse_npmrc_prefix("prefix=rel/dir", home), None);
        assert_eq!(parse_npmrc_prefix("registry=x", home), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_config_is_under_xdg_config_home() {
        assert_eq!(
            Config::default_path(),
            dirs::config_dir().map(|c| c.join("awili").join("config.toml"))
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn config_paths_prefers_dot_config_first() {
        // The XDG-style path must rank first regardless of platform. (On Linux it
        // is also the platform dir, so the list de-dupes to a single entry.)
        let paths = Config::config_paths();
        if let Some(first) = paths.first() {
            assert!(
                first.ends_with("awili/config.toml"),
                "unexpected primary config path: {first:?}"
            );
            assert!(
                first.to_string_lossy().contains(".config"),
                "primary path should be the ~/.config XDG path: {first:?}"
            );
        }
    }

    #[test]
    fn unknown_provider_kind_is_an_error() {
        let bad = r#"
[[providers]]
name = "x"
kind = "frobnicate"
base_url = "http://h"
models = []
"#;
        assert!(Config::parse(bad).is_err());
    }

    #[test]
    fn openai_provider_kind_parses() {
        // P1.3: a bare OpenAI-compatible endpoint is a first-class kind, and its
        // alias spelling is accepted too. (The fixtures use the DeepSeek cloud
        // shape — the local deepseek box the fleet once served no longer runs.)
        let ok = r#"
[[providers]]
name = "deepseek"
kind = "openai"
base_url = "https://api.deepseek.com"
models = ["deepseek-flash"]

[[providers]]
name = "deepseek-alt"
kind = "openai-compatible"
base_url = "https://api.deepseek.com"
models = ["deepseek-v4-pro"]
"#;
        let c = Config::parse(ok).expect("openai kinds parse");
        assert_eq!(c.providers[0].kind, ProviderKind::OpenAi);
        assert_eq!(c.providers[1].kind, ProviderKind::OpenAi);
    }

    // ---- enabled / recall / write flags (slice 11) --------------------------

    #[test]
    fn provider_enabled_defaults_to_true_and_memory_toggles_default_off() {
        let c = Config::parse(SAMPLE).unwrap();
        assert!(
            c.providers.iter().all(|p| p.enabled),
            "absent `enabled` means enabled"
        );
        assert!(!c.memory.recall);
        assert!(!c.memory.write);
    }

    #[test]
    fn disabled_provider_contributes_no_model_choices() {
        let text = format!("{SAMPLE}\n[[providers]]\nname = \"off\"\nkind = \"ollama\"\nbase_url = \"http://h:1\"\nmodels = [\"m1\", \"m2\"]\nenabled = false\n");
        let c = Config::parse(&text).unwrap();
        assert_eq!(c.providers.len(), 4);
        assert!(!c.providers[3].enabled);
        // The flattened cycle still has only the 4 enabled models — the chip
        // (and the default-model dropdown) never offer a disabled provider.
        let choices = c.model_choices();
        assert_eq!(choices.len(), 4);
        assert!(choices.iter().all(|ch| ch.provider != "off"));
    }

    // ---- write_config (slice-11 spec §2) -------------------------------------

    /// A unique temp config path per test.
    fn temp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "awili-write-config-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    /// A config exercising every modelled field, defaults avoided everywhere.
    fn full_config() -> Config {
        Config {
            appearance: AppearanceConfig::default(),
            work: WorkConfig::default(),
            // A non-default pin so the round-trip test exercises the field.
            chat: ChatConfig {
                executor: "agent:dsh:deepseek-v4-pro".to_string(),
            },
            build_targets: BTreeMap::from([(
                "hosta".to_string(),
                BuildTargetConfig {
                    ssh_host: "build-host".to_string(),
                    runner_path: "/opt/awili/runner".to_string(),
                    repo_id: "repo-a".to_string(),
                    model: "qwen3".to_string(),
                    verify_profile: "standard".to_string(),
                },
            )]),
            models: ModelsConfig::default(),
            skills: SkillsConfig {
                fleet_pack: Some(PathBuf::from("/srv/awili/skills/fleet")),
            },
            // N1: every notifications field off its default, so the round-trip
            // proves each one survives a write/read.
            notifications: NotificationsConfig {
                enabled: false,
                approvals: false,
                finished: false,
                failed: false,
                chat_replies: true,
                sound: false,
                fallback_osascript: false,
                min_run_secs: 30,
                idle_away_secs: 0,
            },
            graduation: GraduationConfig::default(),
            default_provider: Some("hermes".to_string()),
            default_model: Some("hermes-4".to_string()),
            memory: MemoryConfig {
                base_url: "http://10.1.2.3:8841".to_string(),
                token: String::new(),
                min_priority: 95,
                recall: false,
                write: false,
            },
            feed: FeedConfig {
                base_url: "http://10.1.2.3:8787".to_string(),
            },
            // Non-default enabled flag; the rest ride the (allowed) defaults.
            outline: OutlineConfig {
                enabled: true,
                ..OutlineConfig::default()
            },
            gitea: GiteaConfig {
                enabled: true,
                ..GiteaConfig::default()
            },
            ccd: CcdConfig::default(),
            hermes: HermesConfig::default(),
            gateway: GatewayConfig::default(),
            engine: EngineConfig::default(),
            engines: Vec::new(),
            mcp: McpConfig::default(),
            providers: vec![
                ProviderConfig {
                    name: "ollama".to_string(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://localhost:11434".to_string(),
                    models: vec!["llama3.1:8b".to_string(), "qwen2.5:14b".to_string()],
                    enabled: false,
                    api_key: None,
                },
                ProviderConfig {
                    name: "hermes".to_string(),
                    kind: ProviderKind::Hermes,
                    base_url: "http://10.1.2.3:8080".to_string(),
                    models: vec!["hermes-4".to_string()],
                    enabled: true,
                    api_key: None,
                },
            ],
        }
    }

    // ---- N1: [notifications] section -----------------------------------------

    /// N1 test 1: an absent `[notifications]` section gives every bool true,
    /// 8 and 120.
    #[test]
    fn notifications_defaults_all_on() {
        let n = Config::parse("default_provider = \"hermes\"\n")
            .unwrap()
            .notifications;
        assert!(n.enabled && n.approvals && n.finished && n.failed);
        assert!(n.chat_replies && n.sound && n.fallback_osascript);
        assert_eq!((n.min_run_secs, n.idle_away_secs), (8, 120));
        assert_eq!(n, NotificationsConfig::default());
    }

    #[test]
    fn legacy_chat_replies_deserializes_but_is_not_serialized() {
        let cfg = Config::parse("[notifications]\nchat_replies = false\n").unwrap();
        assert!(!cfg.notifications.chat_replies);

        let rendered = toml::to_string(&cfg).unwrap();
        assert!(
            !rendered.contains("chat_replies"),
            "legacy notification switch must not be offered again: {rendered}"
        );
    }

    /// N1 test 3: write to a temp path, parse back, and the result is equal
    /// (every field off its default, see `full_config`).
    #[test]
    fn notifications_round_trip_through_write_config() {
        let path = temp_path("notifications");
        let cfg = full_config();
        write_config(&cfg, &path).unwrap();
        let reread = Config::load_from(&path).unwrap();
        assert_eq!(reread.notifications, cfg.notifications);
        assert!(!reread.notifications.enabled);
        assert_eq!(reread.notifications.min_run_secs, 30);
        assert_eq!(reread.notifications.idle_away_secs, 0);
    }

    /// N1 test 5: the built-in and networkless configs carry the default
    /// `[notifications]` section.
    #[test]
    fn builtin_and_networkless_carry_default_notifications() {
        assert_eq!(
            Config::builtin().notifications,
            NotificationsConfig::default()
        );
        assert_eq!(
            Config::networkless().notifications,
            NotificationsConfig::default()
        );
    }

    /// N1 test 2: `enabled = false` alone leaves everything else at its
    /// default.
    #[test]
    fn notifications_partial_section_keeps_defaults() {
        let n = Config::parse("[notifications]\nenabled = false\n")
            .unwrap()
            .notifications;
        assert!(!n.enabled, "explicit enabled = false wins");
        assert_eq!(
            n,
            NotificationsConfig {
                enabled: false,
                ..NotificationsConfig::default()
            }
        );
    }

    /// N1 test 4: out-of-range numbers clamp at use: 99999 → 3600, idle 5 →
    /// 30, idle 0 → 0 (off). `Config::parse` accepts them unchanged.
    #[test]
    fn notifications_effective_values_clamp() {
        let n = Config::parse("[notifications]\nmin_run_secs = 99999\nidle_away_secs = 99999\n")
            .unwrap()
            .notifications;
        assert_eq!(n.min_run_secs, 99999, "parse keeps the raw value");
        assert_eq!(n.effective_min_run_secs(), 3600);
        assert_eq!(n.effective_idle_away_secs(), 3600);
        let low = NotificationsConfig {
            min_run_secs: 0,
            idle_away_secs: 5,
            ..NotificationsConfig::default()
        };
        assert_eq!(low.effective_min_run_secs(), 0);
        assert_eq!(low.effective_idle_away_secs(), 30);
        let off = NotificationsConfig {
            idle_away_secs: 0,
            ..NotificationsConfig::default()
        };
        assert_eq!(off.effective_idle_away_secs(), 0);
        assert_eq!(
            NotificationsConfig::default().effective_idle_away_secs(),
            120
        );
    }

    #[test]
    fn write_config_round_trip_preserves_all_fields() {
        let path = temp_path("roundtrip");
        let original = full_config();
        write_config(&original, &path).unwrap();
        let reread = Config::load_from(&path).unwrap();
        assert_eq!(
            reread, original,
            "write then read must preserve every field"
        );

        // None defaults round-trip too (absent keys, not empty strings).
        let mut bare = full_config();
        bare.default_provider = None;
        bare.default_model = None;
        write_config(&bare, &path).unwrap();
        let reread = Config::load_from(&path).unwrap();
        assert_eq!(reread, bare);
    }

    #[test]
    fn write_config_partial_write_leaves_old_file_intact() {
        // Atomicity: the new text lands in a unique temp file next to `path`
        // first. Force THAT publish to fail (the test seam) — the previous
        // config must survive byte-for-byte, with its mode, and the error
        // must surface. The original file is never deleted in this test.
        let path = temp_path("atomic");
        write_config(&full_config(), &path).unwrap();
        let before = std::fs::read(&path).unwrap();
        #[cfg(unix)]
        let before_mode = file_mode(&path);

        let mut changed = full_config();
        changed.default_model = Some("other".to_string());
        let _f = crate::publish_seam::FailPublishes::arm();
        assert!(
            write_config(&changed, &path).is_err(),
            "interrupted write must error"
        );
        drop(_f);

        // The original file is intact: same bytes, same mode, still a file.
        assert!(path.is_file(), "the original file must still exist");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "a failed publish must leave the original bytes untouched"
        );
        #[cfg(unix)]
        assert_eq!(
            file_mode(&path),
            before_mode,
            "a failed publish must leave the original mode untouched"
        );
        let dir = path.parent().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed publish must not leave a temp file behind: {leftovers:?}"
        );

        // And a clean save afterwards succeeds and leaves no temp files.
        write_config(&changed, &path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), changed);
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file may outlive a successful save"
        );
    }

    #[test]
    fn write_config_rejects_semantically_invalid_config_without_touching_disk() {
        let path = temp_path("invalid");
        write_config(&full_config(), &path).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let mut bad = full_config();
        bad.memory.min_priority = 10; // below the floor — constructible in code,
                                      // but must never reach disk
        match write_config(&bad, &path) {
            Err(ConfigError::MinPriorityTooLow(10)) => {}
            other => panic!("expected MinPriorityTooLow, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    // ---- Windows owner-only ACL proof (counterpart of the mode proof) ------

    /// Windows: every config write lands owner-only (protected DACL, current
    /// user and SYSTEM only), also over a file that was world-readable, and the
    /// `.bak` is owner-only too. Read back with the real ACL API.
    #[cfg(windows)]
    #[test]
    fn windows_config_saves_are_owner_only() {
        let path = temp_path("win-acl");
        let dir = path.parent().unwrap().to_path_buf();
        write_config(&full_config(), &path).unwrap();
        awili_winacl::verify_owner_only(&path).expect("first save must be owner-only");

        // A file an older build left with the inherited (broader) list is
        // replaced by an owner-only one on the next save.
        let loose = dir.join("loose.toml");
        std::fs::write(&loose, "default_provider = \"ollama\"\n").unwrap();
        assert!(awili_winacl::verify_owner_only(&loose).is_err());
        write_config(&full_config(), &loose).unwrap();
        awili_winacl::verify_owner_only(&loose).expect("overwrite must tighten");

        // Document save: new file and the .bak are both owner-only.
        doc::save_validated(&path, "default_provider = \"ollama\"\n").unwrap();
        awili_winacl::verify_owner_only(&path).expect("doc save must be owner-only");
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        assert!(bak.exists(), "an overwrite must leave a .bak");
        awili_winacl::verify_owner_only(&bak).expect(".bak must be owner-only");

        // Loading tightens a config that arrived with a broad list.
        let broad = dir.join("broad.toml");
        std::fs::write(&broad, "default_provider = \"ollama\"\n").unwrap();
        assert!(awili_winacl::verify_owner_only(&broad).is_err());
        Config::load_from_paths(&[broad.clone()]).unwrap();
        awili_winacl::verify_owner_only(&broad).expect("load must tighten");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Windows: a config that cannot be made private is not trusted with
    /// secrets. Plaintext keys are dropped from what load returns, `secret:`
    /// references stay, and a file that CAN be protected loads unchanged.
    #[cfg(windows)]
    #[test]
    fn windows_load_drops_plaintext_keys_when_the_file_cannot_be_protected() {
        let path = temp_path("win-load-insecure");
        let dir = path.parent().unwrap().to_path_buf();
        std::fs::write(
            &path,
            "[hermes]\napi_key = \"hermes-plain\"\n\n[engine]\napi_key = \"secret:engine\"\n",
        )
        .unwrap();
        protect_seam::set(true);
        let (cfg, _) = Config::load_from_paths(std::slice::from_ref(&path)).unwrap();
        protect_seam::set(false);
        assert_eq!(cfg.hermes.api_key, "", "plaintext key must not be used");
        assert_eq!(cfg.engine.api_key, "secret:engine", "references are fine");
        // Protectable: loads as written.
        let (cfg, _) = Config::load_from_paths(std::slice::from_ref(&path)).unwrap();
        assert_eq!(cfg.hermes.api_key, "hermes-plain");
        awili_winacl::verify_owner_only(&path).expect("and the file is now owner-only");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Windows: if the file cannot be created owner-only nothing is written and
    /// no secret reaches the disk (the post-create read-back fails here by the
    /// seam, the way a volume that ignores descriptors would).
    #[cfg(windows)]
    #[test]
    fn windows_save_that_cannot_be_made_private_leaves_no_secret_on_disk() {
        let path = temp_path("win-save-fail");
        let dir = path.parent().unwrap().to_path_buf();
        let mut cfg = full_config();
        cfg.hermes.api_key = "SECRET-KEY-MUST-NOT-LAND".to_string();
        awili_winacl::seam::force_verify_failure(true);
        let err = write_config(&cfg, &path).unwrap_err();
        awili_winacl::seam::force_verify_failure(false);
        assert!(err.to_string().contains("nothing was saved"), "{err}");
        assert!(!path.exists(), "no config was published");
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            assert!(
                !text.contains("SECRET-KEY"),
                "{:?} holds the key",
                entry.path()
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Windows, REAL volume without file permissions (FAT32/exFAT). Run by hand
    /// with `AWILI_TEST_FAT_DIR=<a folder on such a volume>` and `--ignored`
    /// (the build VM mounts a FAT32 VHD as R: for this). A save fails closed with
    /// the plain NTFS message, nothing is published and no secret reaches the
    /// disk; a folder cannot be created owner-only either; and a config that
    /// already sits there loads with its plaintext keys dropped.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs a FAT volume: set AWILI_TEST_FAT_DIR"]
    fn windows_fat_volume_fails_closed_with_the_ntfs_message() {
        let dir =
            PathBuf::from(std::env::var_os("AWILI_TEST_FAT_DIR").expect("AWILI_TEST_FAT_DIR"));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(awili_winacl::volume_supports_acls(&dir), Some(false));
        let path = dir.join("config.toml");
        let mut cfg = full_config();
        cfg.hermes.api_key = "SECRET-KEY-MUST-NOT-LAND".to_string();
        let err = write_config(&cfg, &path).unwrap_err().to_string();
        assert!(err.contains(awili_winacl::NTFS_NEEDED), "{err}");
        assert!(err.contains("Nothing was saved"), "{err}");
        assert!(!path.exists());
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            assert!(
                !text.contains("SECRET-KEY"),
                "{:?} holds the key",
                entry.path()
            );
        }
        let sub = dir.join("private");
        let err = awili_winacl::create_dir_owner_only(&sub).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported, "{err}");
        assert!(err.to_string().contains(awili_winacl::NTFS_NEEDED), "{err}");
        assert!(!sub.exists());
        // Load: a plaintext key in a file that cannot be protected is not used.
        let loose = dir.join("loose.toml");
        std::fs::write(&loose, "[hermes]\napi_key = \"hermes-plain\"\n").unwrap();
        let (loaded, _) = Config::load_from_paths(std::slice::from_ref(&loose)).unwrap();
        assert_eq!(loaded.hermes.api_key, "");
        let problem = file_protection_problem().expect("a banner line is published");
        assert!(problem.contains(awili_winacl::NTFS_NEEDED), "{problem}");
        set_file_protection_problem(None);
        let _ = std::fs::remove_file(loose);
    }

    // ---- S6(a) secret-file mode proof ---------------------------------------

    /// A minimal valid config document for the doc-save assertions.
    #[cfg(unix)]
    const SAMPLE_DOC_TEXT: &str = "default_provider = \"ollama\"\n";

    #[cfg(unix)]
    fn file_mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o777
    }

    /// S6(a) proof: both `write_config` and `ConfigDoc::save_validated` create
    /// the config at mode 0600 (owner read/write only), preserve a STRICTER
    /// existing mode instead of loosening it, and never let the `.bak` backup
    /// become broader than the source. A failed save leaves the original file
    /// intact with its original mode.
    #[cfg(unix)]
    #[test]
    fn config_saves_keep_secret_mode() {
        let mut saved_dirs: Vec<PathBuf> = Vec::new();

        // 1. First save: a fresh config lands at exactly 0600.
        let path = temp_path("secret-mode");
        saved_dirs.push(path.parent().unwrap().to_path_buf());
        write_config(&full_config(), &path).unwrap();
        assert_eq!(file_mode(&path), 0o600, "first save must be 0600");

        // 2. Overwrite with a different config: still exactly 0600.
        let mut changed = full_config();
        changed.default_model = Some("other".to_string());
        write_config(&changed, &path).unwrap();
        assert_eq!(file_mode(&path), 0o600, "overwrite must stay 0600");

        // 3. A stricter existing mode (0400) is preserved, never loosened.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let mut stricter = full_config();
        stricter.default_model = Some("read-only".to_string());
        write_config(&stricter, &path).unwrap();
        assert_eq!(
            file_mode(&path),
            0o400,
            "a stricter mode must not be loosened"
        );

        // 4. A BROADER existing mode (0644) is tightened back to 0600 — the
        //    temp file's mode wins on publish, so a world-readable config can
        //    never survive a save.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_config(&stricter, &path).unwrap();
        assert_eq!(
            file_mode(&path),
            0o600,
            "a broader mode must be tightened to 0600"
        );

        // 5. Document save: the new file is 0600 and the .bak backup is no
        //    broader than the source (source here is 0600 → backup 0600).
        let path = temp_path("secret-mode-doc");
        saved_dirs.push(path.parent().unwrap().to_path_buf());
        write_config(&full_config(), &path).unwrap();
        doc::save_validated(&path, SAMPLE_DOC_TEXT).unwrap();
        assert_eq!(file_mode(&path), 0o600, "doc save must be 0600");
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        assert!(bak.exists(), "an overwrite must leave a .bak");
        assert_eq!(
            file_mode(&bak),
            0o600,
            "backup must not be broader than the source"
        );

        // 6. A first-time document save (no existing file) also lands at 0600
        //    and writes no backup.
        let fresh = temp_path("secret-mode-doc-fresh");
        saved_dirs.push(fresh.parent().unwrap().to_path_buf());
        doc::save_validated(&fresh, SAMPLE_DOC_TEXT).unwrap();
        assert_eq!(file_mode(&fresh), 0o600, "first doc save must be 0600");
        let bak = PathBuf::from(format!("{}.bak", fresh.display()));
        assert!(!bak.exists(), "a first create must never write a .bak");

        // 7. A failed publish (forced through the seam) leaves the original
        //    bytes and mode untouched, for BOTH save paths.
        let fail_path = temp_path("secret-mode-fail");
        saved_dirs.push(fail_path.parent().unwrap().to_path_buf());
        write_config(&full_config(), &fail_path).unwrap();
        std::fs::set_permissions(&fail_path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let before = std::fs::read(&fail_path).unwrap();
        let _f = crate::publish_seam::FailPublishes::arm();
        assert!(
            write_config(&full_config(), &fail_path).is_err(),
            "the seam must make write_config fail at publish"
        );
        assert!(
            doc::save_validated(&fail_path, SAMPLE_DOC_TEXT).is_err(),
            "the seam must make save_validated fail at publish"
        );
        drop(_f);
        assert_eq!(
            std::fs::read(&fail_path).unwrap(),
            before,
            "a failed save must leave the original bytes untouched"
        );
        assert_eq!(
            file_mode(&fail_path),
            0o400,
            "a failed save must leave the original mode untouched"
        );
        let fail_dir = fail_path.parent().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(fail_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed save must not leave a temp file behind: {leftovers:?}"
        );

        // 8. No temp files outlive any successful save in these directories.
        for dir in saved_dirs {
            let leftovers: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".tmp-"))
                .collect();
            assert!(
                leftovers.is_empty(),
                "{dir:?}: temp leftovers: {leftovers:?}"
            );
        }
    }

    /// S6(a) F2 proof: a `.bak` is never written THROUGH an existing inode.
    /// A stale 0644 `.bak` (a regular file) is REPLACED by a new inode created
    /// at the source's stricter-or-0600 mode — the secret is never held under
    /// the old, broader permissions for a single write.
    #[cfg(unix)]
    #[test]
    fn doc_backup_replaces_broader_bak_inode() {
        use std::os::unix::fs::MetadataExt;

        let path = temp_path("bak-inode");
        write_config(&full_config(), &path).unwrap();
        let src_before = std::fs::read(&path).unwrap();

        // A stale, world-readable backup squatting at the backup path.
        let bak = PathBuf::from(format!("{}.bak", path.display()));
        std::fs::write(&bak, b"stale").unwrap();
        std::fs::set_permissions(&bak, std::fs::Permissions::from_mode(0o644)).unwrap();
        let bak_ino_before = std::fs::metadata(&bak).unwrap().ino();

        // The source is 0600 here: the backup must land 0600 on a NEW inode.
        doc::save_validated(&path, SAMPLE_DOC_TEXT).unwrap();
        assert_eq!(
            file_mode(&bak),
            0o600,
            "the backup must be created at the source's mode"
        );
        assert_eq!(
            std::fs::read(&bak).unwrap(),
            src_before,
            "the backup must carry the previous source bytes"
        );
        assert_ne!(
            std::fs::metadata(&bak).unwrap().ino(),
            bak_ino_before,
            "the stale .bak inode must be replaced, not written through"
        );

        // A stricter source (0400): the second backup is created at 0400,
        // even though the existing (first) backup is back at 0644.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let src_read_only = std::fs::read(&path).unwrap();
        std::fs::set_permissions(&bak, std::fs::Permissions::from_mode(0o644)).unwrap();
        doc::save_validated(&path, SAMPLE_DOC_TEXT).unwrap();
        assert_eq!(
            file_mode(&bak),
            0o400,
            "the backup must match the source's stricter mode"
        );
        assert_eq!(
            std::fs::read(&bak).unwrap(),
            src_read_only,
            "the backup must carry the recorded source bytes"
        );

        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file may outlive the save: {leftovers:?}"
        );
    }

    // -- [engine] section (D2-u2a) --------------------------------------------

    /// An absent `[engine]` section is the D1 status quo: the gateway engine,
    /// with the seam's own defaults filled in. Adding the section to the config
    /// struct must not move any existing deployment onto the new engine.
    #[test]
    fn absent_engine_section_defaults_to_the_gateway_engine() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.engine.kind, EngineKind::HermesGateway);
        assert_eq!(cfg.engine.dialect, "chat-completions");
        assert_eq!(cfg.engine.window_tokens, 131_072);
        assert_eq!(cfg.engine.request_timeout_s, 180);
        assert!(cfg.engine.base_url.is_empty());
    }

    /// A partial `[engine]` stanza selects the engine and inherits the rest —
    /// the shape a user actually types.
    #[test]
    fn engine_kind_awili_parses_and_inherits_defaults() {
        let cfg = Config::parse(
            r#"
            [engine]
            kind = "awili"
            base_url = "http://localhost:18000"
            model = "laguna-s-2.1-nvfp4"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.engine.kind, EngineKind::Awili);
        assert_eq!(cfg.engine.base_url, "http://localhost:18000");
        assert_eq!(cfg.engine.model, "laguna-s-2.1-nvfp4");
        assert_eq!(cfg.engine.dialect, "chat-completions");
        assert_eq!(cfg.engine.window_tokens, 131_072);
    }

    /// A misspelled `kind` is a hard parse error, not a silent fall back to the
    /// default engine: per this module's contract a malformed file is surfaced
    /// rather than ignored, and silently running a different engine than the one
    /// written down is exactly the drift that would hide.
    #[test]
    fn unknown_engine_kind_is_a_hard_error() {
        let err = Config::parse("[engine]\nkind = \"awilii\"\n");
        assert!(
            err.is_err(),
            "a typo'd engine kind must not silently resolve to a default"
        );
    }

    /// `kind` is kebab-case on the wire, matching the plan's spelling.
    #[test]
    fn engine_kind_hermes_gateway_is_kebab_case_on_the_wire() {
        let cfg = Config::parse("[engine]\nkind = \"hermes-gateway\"\n").unwrap();
        assert_eq!(cfg.engine.kind, EngineKind::HermesGateway);
    }

    // -- [[engines]] (additional in-app models) -------------------------------

    const QWEN_STANZA: &str = r#"
        [[engines]]
        id = "qwen-hostd"
        label = "Qwen 3.8 27B (hostd)"
        base_url = "http://qwen-host.example:8000"
        model = "Qwen/Qwen3.8-27B"
        api_key = "sk-qwen-SECRET"
        window_tokens = 65536
        request_timeout_s = 300
    "#;

    #[test]
    fn absent_engines_list_is_empty_and_not_serialized() {
        let cfg = Config::parse("").unwrap();
        assert!(cfg.engines.is_empty());
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains("engines"), "no [[engines]] in output");
    }

    #[test]
    fn one_extra_engine_parses_and_round_trips() {
        let cfg = Config::parse(QWEN_STANZA).unwrap();
        assert_eq!(cfg.engines.len(), 1);
        let e = &cfg.engines[0];
        assert_eq!(e.id, "qwen-hostd");
        assert_eq!(e.dialect, "chat-completions");
        assert_eq!(e.window_tokens, 65_536);
        let rt = Config::parse(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(rt.engines, cfg.engines);
        let eng = e.to_engine_config();
        assert_eq!(eng.kind, EngineKind::Awili);
        assert_eq!(eng.display_label(), "Qwen 3.8 27B (hostd)");
        assert_eq!(eng.base_url, "http://qwen-host.example:8000");
    }

    #[test]
    fn extra_engines_keep_their_order() {
        let cfg = Config::parse(
            "[[engines]]\nid = \"b\"\n[[engines]]\nid = \"a\"\n[[engines]]\nid = \"c\"\n",
        )
        .unwrap();
        let ids: Vec<_> = cfg.engines.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["b", "a", "c"]);
    }

    #[test]
    fn duplicate_engine_ids_are_rejected_with_a_clear_error() {
        let err = Config::parse("[[engines]]\nid = \"q\"\n[[engines]]\nid = \"q\"\n")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("duplicate id") && err.contains("\"q\""),
            "{err}"
        );
    }

    #[test]
    fn missing_or_invalid_engine_id_is_rejected() {
        let err = Config::parse("[[engines]]\nmodel = \"m\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing field `id`"), "{err}");
        for bad in ["", "has space", "a:b", "x/y"] {
            let text = format!("[[engines]]\nid = \"{bad}\"\n");
            assert!(Config::parse(&text).is_err(), "id {bad:?} must be rejected");
        }
        assert!(Config::parse("[[engines]]\nid = \"ok.1_x-2\"\n").is_ok());
    }

    #[test]
    fn extra_engine_typo_key_is_a_load_error() {
        assert!(Config::parse("[[engines]]\nid = \"q\"\nbase_ulr = \"x\"\n").is_err());
    }

    #[test]
    fn extra_engine_debug_never_prints_the_api_key() {
        let cfg = Config::parse(QWEN_STANZA).unwrap();
        assert!(!format!("{:?}", cfg.engines[0]).contains("sk-qwen-SECRET"));
        assert!(!format!("{cfg:?}").contains("sk-qwen-SECRET"));
    }

    // -- [skills] section (daily-driver P1) -----------------------------------

    /// A configured `fleet_pack` path parses and is carried through.
    /// P8b: an absent `[graduation]` section keeps the default ledger path;
    /// an explicit empty string disables recording; a custom path is kept.
    #[test]
    fn graduation_default_and_empty_disables() {
        let bare = Config::parse("").unwrap();
        assert_eq!(
            bare.graduation.ledger,
            "~/.local/share/awili/graduation.jsonl"
        );
        assert_eq!(bare.graduation, GraduationConfig::default());
        let off = Config::parse("[graduation]\nledger = \"\"\n").unwrap();
        assert!(off.graduation.ledger.is_empty());
        let custom = Config::parse("[graduation]\nledger = \"/tmp/g.jsonl\"\n").unwrap();
        assert_eq!(custom.graduation.ledger, "/tmp/g.jsonl");
        let sample = Config::parse(SAMPLE).unwrap();
        assert_eq!(sample.graduation, GraduationConfig::default());
    }

    #[test]
    fn skills_section_parses_fleet_pack() {
        let cfg = Config::parse("[skills]\nfleet_pack = \"/srv/awili/skills/fleet\"\n").unwrap();
        assert_eq!(
            cfg.skills.fleet_pack.as_deref(),
            Some(Path::new("/srv/awili/skills/fleet"))
        );
    }

    /// An absent `[skills]` section — every existing config shape — parses
    /// unchanged: the pack is disabled by default, and a config without the
    /// section parses exactly as it did before the field existed.
    #[test]
    fn absent_skills_section_leaves_the_pack_disabled() {
        let bare = Config::parse("").unwrap();
        assert_eq!(bare.skills, SkillsConfig::default());
        assert!(bare.skills.fleet_pack.is_none());

        // The full SAMPLE config (no [skills] stanza) keeps the default too.
        let c = Config::parse(SAMPLE).unwrap();
        assert!(c.skills.fleet_pack.is_none());
    }

    /// The error-inert config used when an existing file is malformed must not
    /// retain a pack either: a disabled config keeps no recipes.
    #[test]
    fn networkless_carries_no_skills_pack() {
        assert!(Config::networkless().skills.fleet_pack.is_none());
    }

    // -- [mcp] servers (mac-shell slice, unit 2) -----------------------------

    /// The shape a user actually writes for mac-shell, and the defaults it
    /// leans on.
    #[test]
    fn mcp_server_parses_with_install_trust_and_default_modes() {
        let cfg = Config::parse(
            r#"
            [[mcp.servers]]
            name = "mac-shell"
            command = "/Users/j/.local/bin/mac-shell-mcp"
            trust = "install"
            "#,
        )
        .unwrap();
        let s = &cfg.mcp.servers[0];
        assert_eq!(s.name, "mac-shell");
        assert_eq!(s.origin(), "mcp:mac-shell");
        assert!(s.is_install_trusted());
        assert!(s.enabled, "a configured server is launched unless disabled");
        assert!(s.args.is_empty());
        assert_eq!(s.modes, vec![McpMode::Work, McpMode::Code]);
    }

    /// `trust = "install"` is a permanent, card-free grant. Any server other
    /// than the reserved one asking for it fails the whole load — the doc
    /// comment on `McpTrust::Install` used to be the ONLY thing standing
    /// between an arbitrary config entry and unattended host access
    /// (Codex P1, 2026-08-13).
    #[test]
    fn install_trust_on_an_unreserved_name_is_a_hard_error() {
        let err = Config::parse(
            r#"
            [[mcp.servers]]
            name = "totally-legit"
            command = "/tmp/evil"
            trust = "install"
            "#,
        );
        match err {
            Err(ConfigError::InstallTrustNotReserved(name)) => {
                assert_eq!(name, "totally-legit");
            }
            other => panic!("install trust must be refused off the reserved name, got {other:?}"),
        }
    }

    /// Parse-time validation does not cover a config built in code, so the
    /// accessor re-checks the name. Belt and braces on the same invariant.
    #[test]
    fn install_trust_needs_the_reserved_name_even_when_built_in_code() {
        let imposter = McpServerConfig {
            name: "not-mac-shell".into(),
            command: "/tmp/evil".into(),
            args: Vec::new(),
            env: Default::default(),
            trust: McpTrust::Install,
            modes: default_mcp_modes(),
            enabled: true,
        };
        assert!(
            !imposter.is_install_trusted(),
            "only {INSTALL_TRUSTED_SERVER:?} may carry install trust"
        );
    }

    /// The default trust is PROMPT. A server that forgets to say gets the
    /// approval card, never the install exemption — the safe direction to fail.
    #[test]
    fn omitted_trust_defaults_to_prompt_not_install() {
        let cfg = Config::parse(
            r#"
            [[mcp.servers]]
            name = "somebody-elses-server"
            command = "/opt/x/server"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.mcp.servers[0].trust, McpTrust::Prompt);
        assert!(!cfg.mcp.servers[0].is_install_trusted());
    }

    /// Legacy `chat` mode entries still parse, but never offer a removed
    /// runtime surface.
    #[test]
    fn chat_mode_is_deserialized_but_never_offered() {
        let cfg = Config::parse(
            r#"
            [[mcp.servers]]
            name = "mac-shell"
            command = "/x/y"
            modes = ["work", "chat"]
            "#,
        )
        .unwrap();
        let s = &cfg.mcp.servers[0];
        assert!(s.offers_in(McpMode::Work));
        assert!(s.modes.contains(&McpMode::Chat));
        assert!(
            !s.offers_in(McpMode::Chat),
            "legacy Chat config must not revive a removed runtime"
        );
        assert_eq!(cfg.mcp.active_in(McpMode::Chat).count(), 0);
        assert_eq!(cfg.mcp.active_in(McpMode::Work).count(), 1);
    }

    /// A disabled server stays configured but is never launched or offered.
    #[test]
    fn disabled_server_is_not_active_in_any_mode() {
        let cfg = Config::parse(
            r#"
            [[mcp.servers]]
            name = "parked"
            command = "/x/y"
            enabled = false
            modes = ["work", "code", "chat"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.mcp.active().count(), 0);
        for m in [McpMode::Work, McpMode::Code] {
            assert_eq!(cfg.mcp.active_in(m).count(), 0);
        }
    }

    /// An absent `[mcp]` section is simply no servers — adding the section to
    /// the Config struct changes nothing for an existing config.
    #[test]
    fn absent_mcp_section_is_no_servers() {
        assert!(Config::parse("").unwrap().mcp.servers.is_empty());
    }

    /// A misspelled trust or mode is a hard parse error, per this module's
    /// contract. Silently downgrading an unknown trust to the default would be
    /// the safe direction; silently UPGRADING would not, and neither should be
    /// guessed at when the fix is one visible error message.
    #[test]
    fn unknown_trust_or_mode_is_a_hard_error() {
        assert!(Config::parse(
            "[[mcp.servers]]\nname = \"x\"\ncommand = \"/x\"\ntrust = \"installl\"\n"
        )
        .is_err());
        assert!(Config::parse(
            "[[mcp.servers]]\nname = \"x\"\ncommand = \"/x\"\nmodes = [\"wrok\"]\n"
        )
        .is_err());
    }

    /// `[work].default_executor` is optional, round-trips, and its absence
    /// leaves the section identical to before it existed (household installs
    /// seed it; every existing config omits it).
    #[test]
    fn work_default_executor_is_optional_and_parses() {
        let cfg = Config::parse("[work]\ndefault_executor = \"agent:glm\"\n").unwrap();
        assert_eq!(cfg.work.default_executor.as_deref(), Some("agent:glm"));
        assert_eq!(cfg.work.agents, AgentBinsConfig::default());

        let cfg = Config::parse("[work.agents]\nsearch_paths = [\"/x\"]\n").unwrap();
        assert_eq!(cfg.work.default_executor, None);
        assert_eq!(cfg.work.agents.search_paths, vec!["/x".to_string()]);

        let bare = Config::parse("").unwrap();
        assert_eq!(bare.work, WorkConfig::default());
    }

    /// `[work].default_approval_mode` (U4, spec D2) is optional, parses
    /// `"auto"`, and round-trips; absence means `None` (the caller falls
    /// back to Ask — a value is never an error).
    #[test]
    fn u4c_default_approval_mode_is_optional_and_round_trips() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.work.default_approval_mode, None);

        let cfg = Config::parse("[work]\ndefault_approval_mode = \"auto\"\n").unwrap();
        assert_eq!(cfg.work.default_approval_mode.as_deref(), Some("auto"));

        let cfg = Config::parse("[work]\ndefault_approval_mode = \"auto\"\n").unwrap();
        let toml = toml::to_string(&cfg).unwrap();
        assert!(toml.contains("default_approval_mode = \"auto\""));
        let back = Config::parse(&toml).unwrap();
        assert_eq!(back.work.default_approval_mode.as_deref(), Some("auto"));
    }

    /// rc.4 S7: `[work.sandbox]` is optional, OFF by default, parses its two
    /// keys and round-trips; an old config without it is unchanged.
    #[test]
    fn s7_work_sandbox_defaults_off_and_round_trips() {
        let cfg = Config::parse("").unwrap();
        assert!(!cfg.work.sandbox.enabled);
        assert!(cfg.work.sandbox.extra_writable.is_empty());
        assert_eq!(cfg.work.sandbox, SandboxConfig::default());

        let text =
            "[work.sandbox]\nenabled = true\nextra_writable = [\"~/Scratch\", \"/Volumes/Out\"]\n";
        let cfg = Config::parse(text).unwrap();
        assert!(cfg.work.sandbox.enabled);
        assert_eq!(
            cfg.work.sandbox.extra_writable,
            ["~/Scratch", "/Volumes/Out"]
        );
        let back = Config::parse(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.work.sandbox, cfg.work.sandbox);

        // A typo in the section is a plain config error, not a silent default.
        assert!(Config::parse("[work.sandbox]\nenabled = \"yes please\"\n").is_err());
    }

    // ---- [chat] (daily-driver P9) + dsh agent fields -----------------------
    //
    // Chat is pinned to one executor with NO picker: the pin is config-only,
    // parsed with the SAME grammar as `[work].default_executor` so a dsh (or a
    // future local Luna) entry is legal without code changes.

    /// An absent `[chat]` section pins Chat to Claude · haiku through the
    /// vendor's own `claude` binary — the owner decision of record.
    #[test]
    fn chat_config_default_is_claude_haiku() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.chat.executor, "agent:claude:haiku");
        let cfg = Config::parse("[work]\ndefault_executor = \"agent:glm\"\n").unwrap();
        assert_eq!(cfg.chat.executor, "agent:claude:haiku");
        assert_eq!(cfg.chat, ChatConfig::default());
    }

    /// `[chat] executor` parses a dsh pin (and, by the same grammar, any
    /// agent:<engine>:<model> entry) verbatim; an empty executor string keeps
    /// the default.
    #[test]
    fn chat_config_parses_agent_dsh_deepseek_flash() {
        let cfg = Config::parse("[chat]\nexecutor = \"agent:dsh:deepseek-flash\"\n").unwrap();
        assert_eq!(cfg.chat.executor, "agent:dsh:deepseek-flash");
        // A model-less dsh pin and a claude pin parse too (same grammar).
        let cfg = Config::parse("[chat]\nexecutor = \"agent:dsh\"\n").unwrap();
        assert_eq!(cfg.chat.executor, "agent:dsh");
        let cfg = Config::parse("[chat]\nexecutor = \"agent:claude:haiku\"\n").unwrap();
        assert_eq!(cfg.chat.executor, "agent:claude:haiku");
        // The field survives a TOML round-trip.
        let cfg = Config::parse("[chat]\nexecutor = \"agent:dsh:deepseek-v4-pro\"\n").unwrap();
        assert_eq!(cfg.chat.executor, "agent:dsh:deepseek-v4-pro");
    }

    /// `[work.agents]` dsh fields: absent = default (no explicit bin, default
    /// env file), explicit values are honoured, and the effective env file
    /// falls back to the default when unset or blank.
    #[test]
    fn dsh_agent_fields_default_and_parse() {
        let d = AgentBinsConfig::default();
        assert_eq!(d.dsh_bin, None);
        assert_eq!(d.dsh_env_file, None);
        assert_eq!(d.dsh_env_file(), DEFAULT_DSH_ENV_FILE);
        // A blank configured env file is treated as unset (never a blank path).
        let blank = AgentBinsConfig {
            dsh_env_file: Some("   ".into()),
            ..Default::default()
        };
        assert_eq!(blank.dsh_env_file(), DEFAULT_DSH_ENV_FILE);

        let cfg = Config::parse(
            "[work.agents]\n\
             dsh_bin = \"/opt/homebrew/bin/dsh\"\n\
             dsh_env_file = \"~/keys/deepseek.toml\"\n",
        )
        .unwrap();
        assert_eq!(
            cfg.work.agents.dsh_bin.as_deref(),
            Some("/opt/homebrew/bin/dsh")
        );
        assert_eq!(
            cfg.work.agents.dsh_env_file.as_deref(),
            Some("~/keys/deepseek.toml")
        );
        assert_eq!(cfg.work.agents.dsh_env_file(), "~/keys/deepseek.toml");
    }

    /// `[models].dsh_models` defaults to the two headless-route ids; a
    /// configured list replaces them (vendor names are data, not code).
    #[test]
    fn dsh_models_default_and_parse() {
        let d = ModelsConfig::default();
        assert_eq!(
            d.dsh_models,
            vec!["deepseek-flash".to_string(), "deepseek-v4-pro".to_string()]
        );
        let cfg = Config::parse("[models]\ndsh_models = [\"deepseek-flash\", \"deepseek-r1\"]\n")
            .unwrap();
        assert_eq!(
            cfg.models.dsh_models,
            vec!["deepseek-flash".to_string(), "deepseek-r1".to_string()]
        );
        // An absent `[models]` section keeps the defaults.
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.models.dsh_models, d.dsh_models);
    }

    // ---- config provenance (FIRST-RUN-SPEC task 1) ----------------------------
    //
    // `load_from_paths` is exercised with explicit primary + fallback paths in
    // unique temp dirs — no HOME or environment mutation, and never the live
    // user config. The pre-existing `temp_path` helper only builds paths and
    // leaves dirs behind, so these tests use an RAII dir that cleans up.

    /// Distinguishable config values: `default_provider` / `default_model`.
    const PROVENANCE_PRIMARY_TOML: &str =
        "default_provider = \"prov-primary\"\ndefault_model = \"model-primary\"\n";
    const PROVENANCE_FALLBACK_TOML: &str =
        "default_provider = \"prov-fallback\"\ndefault_model = \"model-fallback\"\n";

    /// RAII unique temp dir: isolated candidates, removed on drop.
    struct ProvenanceTempDir(PathBuf);

    impl ProvenanceTempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static PROVENANCE_N: AtomicU64 = AtomicU64::new(0);
            let n = PROVENANCE_N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("awili-provenance-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn dir(&self) -> &Path {
            &self.0
        }

        /// Create `name` inside the dir with `contents`; returns the path.
        fn write_file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for ProvenanceTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn provenance_missing_when_no_candidate_exists() {
        let dir = ProvenanceTempDir::new("missing");
        let (config, source) = Config::load_from_paths(&[
            dir.dir().join("primary.toml"),
            dir.dir().join("fallback.toml"),
        ])
        .unwrap();
        assert_eq!(source, ConfigSource::Missing);
        assert_eq!(config, Config::builtin());
    }

    #[test]
    fn provenance_primary_only() {
        let dir = ProvenanceTempDir::new("primary-only");
        let primary = dir.write_file("primary.toml", PROVENANCE_PRIMARY_TOML);
        let (config, source) =
            Config::load_from_paths(&[primary.clone(), dir.dir().join("fallback.toml")]).unwrap();
        assert_eq!(source, ConfigSource::Loaded(primary));
        assert_eq!(config.default_provider.as_deref(), Some("prov-primary"));
        assert_eq!(config.default_model.as_deref(), Some("model-primary"));
    }

    #[test]
    fn provenance_fallback_only() {
        let dir = ProvenanceTempDir::new("fallback-only");
        let fallback = dir.write_file("fallback.toml", PROVENANCE_FALLBACK_TOML);
        let (config, source) =
            Config::load_from_paths(&[dir.dir().join("primary.toml"), fallback.clone()]).unwrap();
        assert_eq!(source, ConfigSource::Loaded(fallback));
        assert_eq!(config.default_provider.as_deref(), Some("prov-fallback"));
        assert_eq!(config.default_model.as_deref(), Some("model-fallback"));
    }

    #[test]
    fn provenance_primary_wins_over_fallback() {
        let dir = ProvenanceTempDir::new("both-valid");
        let primary = dir.write_file("primary.toml", PROVENANCE_PRIMARY_TOML);
        let fallback = dir.write_file("fallback.toml", PROVENANCE_FALLBACK_TOML);
        let (config, source) = Config::load_from_paths(&[primary.clone(), fallback]).unwrap();
        assert_eq!(source, ConfigSource::Loaded(primary));
        assert_eq!(config.default_provider.as_deref(), Some("prov-primary"));
        assert_eq!(config.default_model.as_deref(), Some("model-primary"));
    }

    #[test]
    fn provenance_invalid_primary_errors_despite_valid_fallback() {
        let dir = ProvenanceTempDir::new("invalid-primary");
        let primary = dir.write_file("primary.toml", "this is not = = toml");
        let fallback = dir.write_file("fallback.toml", PROVENANCE_FALLBACK_TOML);
        let err = Config::load_from_paths(&[primary, fallback]).unwrap_err();
        assert!(
            matches!(err, ConfigError::Toml(_)),
            "an invalid primary must fail the load, not fall through to the fallback: {err:?}"
        );
    }

    #[test]
    fn provenance_empty_existing_file_is_loaded_not_missing() {
        let dir = ProvenanceTempDir::new("empty-file");
        let primary = dir.write_file("primary.toml", "");
        let fallback = dir.write_file("fallback.toml", PROVENANCE_FALLBACK_TOML);
        let (config, source) = Config::load_from_paths(&[primary.clone(), fallback]).unwrap();
        assert_eq!(source, ConfigSource::Loaded(primary));
        // An empty file is valid TOML: every field at its serde default — not
        // the builtin (which sets provider/model and two providers) and not the
        // fallback's values.
        assert_eq!(config, Config::parse("").unwrap());
        assert_eq!(config.default_provider, None);
        assert!(config.providers.is_empty());
    }

    #[test]
    fn provenance_directory_at_primary_is_io_error_fallback_not_chosen() {
        let dir = ProvenanceTempDir::new("dir-primary");
        let primary = dir.dir().join("primary.toml");
        std::fs::create_dir(&primary).unwrap();
        let fallback = dir.write_file("fallback.toml", PROVENANCE_FALLBACK_TOML);
        let err = Config::load_from_paths(&[primary, fallback]).unwrap_err();
        assert!(
            matches!(err, ConfigError::Io(_)),
            "a directory at the primary path is unreadable: Io error, fallback not chosen: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn provenance_dangling_primary_symlink_is_io_error_fallback_not_chosen() {
        let dir = ProvenanceTempDir::new("dangling-primary");
        let primary = dir.dir().join("primary.toml");
        std::os::unix::fs::symlink(dir.dir().join("no-such-target.toml"), &primary).unwrap();
        let fallback = dir.write_file("fallback.toml", PROVENANCE_FALLBACK_TOML);
        let err = Config::load_from_paths(&[primary, fallback]).unwrap_err();
        assert!(
            matches!(err, ConfigError::Io(_)),
            "a dangling primary symlink is unreadable: Io error, never skipped to the fallback: {err:?}"
        );
    }

    #[test]
    fn provenance_directory_primary_errors_despite_valid_fallback() {
        let dir = ProvenanceTempDir::new("directory-primary");
        let primary = dir.dir().join("primary.toml");
        std::fs::create_dir(&primary).unwrap();
        let fallback = dir.write_file("fallback.toml", PROVENANCE_FALLBACK_TOML);
        let err = Config::load_from_paths(&[primary, fallback]).unwrap_err();
        assert!(
            matches!(err, ConfigError::Io(_)),
            "a directory at the primary path is unreadable: Io error, fallback not chosen: {err:?}"
        );
    }
    // ---- [build_targets.<id>] (daily-driver P2d) ---------------------------

    fn build_targets_text(key: &str, line: &str) -> String {
        let mut lines = [
            "ssh_host = \"build-host\"",
            "runner_path = \"/opt/awili/runner\"",
            "repo_id = \"repo-a\"",
            "model = \"qwen3\"",
            "verify_profile = \"standard\"",
        ];
        let marker = format!("{key} = ");
        *lines.iter_mut().find(|l| l.starts_with(&marker)).unwrap() = line;
        format!("[build_targets.hosta]\n{}\n", lines.join("\n"))
    }

    /// An absent `[build_targets]` section parses to NO targets — today's
    /// behavior, preserved exactly.
    #[test]
    fn build_targets_absent_preserves_behavior() {
        let cfg = Config::parse("").unwrap();
        assert!(cfg.build_targets.is_empty());
        let cfg = Config::parse("[work]\ndefault_executor = \"gateway\"\n").unwrap();
        assert!(cfg.build_targets.is_empty());
    }

    /// Valid entries parse with their exact values.
    #[test]
    fn build_targets_valid_entries_parse() {
        let text = build_targets_text("model", "model = \"qwen3\"");
        let cfg = Config::parse(&text).unwrap();
        assert_eq!(cfg.build_targets.len(), 1);
        let t = cfg.build_targets.get("hosta").unwrap();
        assert_eq!(t.ssh_host, "build-host");
        assert_eq!(t.runner_path, "/opt/awili/runner");
        assert_eq!(t.repo_id, "repo-a");
        assert_eq!(t.model, "qwen3");
        assert_eq!(t.verify_profile, "standard");
        assert_eq!(t.label(), "Build \u{00B7} qwen3 on build-host");
    }

    /// An invalid field is a config error naming the target AND the field.
    #[test]
    fn build_targets_invalid_field_is_a_naming_error() {
        let cases = [
            ("ssh_host = \"host;rm -rf /\"", "ssh_host"),
            ("ssh_host = \"-oProxyCommand=x\"", "ssh_host"),
            ("runner_path = \"relative/runner\"", "runner_path"),
            ("repo_id = \"\"", "repo_id"),
            ("model = \"\"", "model"),
            ("verify_profile = \"\"", "verify_profile"),
        ];
        for (bad_line, field) in cases {
            let text = build_targets_text(field, bad_line);
            match Config::parse(&text) {
                Err(ConfigError::InvalidBuildTarget {
                    target, field: f, ..
                }) => {
                    assert_eq!(target, "hosta");
                    assert_eq!(f, field, "the offending field must be named");
                }
                other => panic!("expected an InvalidBuildTarget error, got {other:?}"),
            }
        }
    }

    /// A missing field or an unknown key is a config error too: every field
    /// is required and no extras are accepted (the target is all-or-nothing).
    #[test]
    fn build_targets_missing_or_unknown_field_is_an_error() {
        let missing = "ssh_host = \"build-host\"\n\
                       runner_path = \"/opt/awili/runner\"\n\
                       repo_id = \"repo-a\"\n";
        assert!(Config::parse(&format!("[build_targets.hosta]\n{missing}\n")).is_err());
        let unknown = build_targets_text("model", "model = \"qwen3\"\nextra = \"x\"");
        assert!(Config::parse(&unknown).is_err());
    }

    /// R1-ISO (C-lite): `[work.agents.memory]` parses; absent by default.
    #[test]
    fn r1_iso_agent_memory_section_parses() {
        let none = Config::parse("[work.agents]\nsearch_paths = [\"/x\"]\n").unwrap();
        assert_eq!(none.work.agents.memory, None);
        let cfg = Config::parse(
            "[work.agents.memory]\ninterlock_command = \"~/.local/bin/interlock-mcp\"\n\
             precompact_hook = \"~/.claude/foreman-gate/precompact-breadcrumb.sh\"\n\
             codex_hook_trust = \"sha256:abc\"\ncodex_model = \"gpt-6.1-sol\"\n\
             codex_reasoning_effort = \"low\"\n\
             [work.agents.memory.interlock_env]\nINTERLOCK_URL = \"http://127.0.0.1:18861\"\n",
        )
        .unwrap();
        let m = cfg.work.agents.memory.expect("parsed");
        assert_eq!(
            m.interlock_command.as_deref(),
            Some("~/.local/bin/interlock-mcp")
        );
        assert_eq!(m.interlock_env["INTERLOCK_URL"], "http://127.0.0.1:18861");
        assert_eq!(m.codex_hook_trust.as_deref(), Some("sha256:abc"));
        assert_eq!(m.codex_model.as_deref(), Some("gpt-6.1-sol"));
    }
}
