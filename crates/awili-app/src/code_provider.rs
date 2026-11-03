//! `CodeProvider` — drives a coding-agent CLI (`claude`/`codex`) for Code mode
//! and bridges its stream onto the [`ChatProvider`] trait.
//!
//! Per-turn lifecycle (the opcode-proven model, verified live 2026-06-24): spawn
//! the engine with [`CodeEngine::build_args`] over **plain pipes — no PTY** (the
//! current `claude`/`codex` authenticate fine without a TTY), read stdout
//! line-by-line, decode each line with the pure [`CodeEngine::parse_line`] +
//! [`CodeEvent::to_provider_event`] in `awili-code`, and forward the result. Each
//! turn is a fresh process; multi-turn continuity is `--resume <session_id>`
//! captured from the first turn — so prompts carry ONLY the new user message,
//! never the replayed history (the engine already holds it).
//!
//! The decoding is tested in `awili-code`; the only logic here is the I/O glue and
//! [`finalize`], which guarantees exactly one terminal [`ProviderEvent::Done`]
//! tied to process exit and synthesises an error if the process dies without one.

use crate::proc_compat::OwnProcessGroup;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use awili_code::{Autonomy, CodeEngine};
use awili_providers::{ChatRequest, ProviderEvent, Role};

/// A shared cell the driver fills with the engine's session/thread id the first
/// time it sees one, so the next turn can resume that conversation. The Code tab
/// owns the `Arc` and reads it between turns.
pub type SessionSlot = Arc<Mutex<Option<String>>>;

/// Make the one native-id cell owned by one accepted provider turn. Reusing a
/// tab's old cell would let a late process write its id into a different
/// harness/fresh turn; the caller installs this cell on the tab before launch.
pub fn fresh_session_slot(resume_id: Option<&str>) -> SessionSlot {
    Arc::new(Mutex::new(resume_id.map(str::to_owned)))
}

/// A [`ChatProvider`] that runs a coding-agent CLI for one Code turn.
pub struct CodeProvider {
    /// Absolute path to the engine binary. A GUI app launched from Finder does
    /// not inherit a shell `PATH`, so the caller resolves the binary up front
    /// (`resolve_agent_bin`); `engine` still decides args and decoding.
    bin: String,
    engine: CodeEngine,
    autonomy: Autonomy,
    cwd: String,
    /// The session id to resume (the tab's prior turn), or `None` on turn one.
    resume_id: Option<String>,
    /// The model this turn should run on (`None` = the engine's own default).
    /// Claude/Codex take it as a CLI flag; GLM takes a `--settings` overlay
    /// (ZCode has no model flag — see [`glm_settings_overlay`]).
    model: Option<String>,
    /// Filled with the session id parsed from this turn (for the next resume).
    session_slot: SessionSlot,
    termination_observed: Option<Arc<AtomicBool>>,
    /// RC2-CXSF (RC2-CXS review P3): set when this turn runs on
    /// `codex exec --json` (a Codex with no app server), for the run that
    /// asked ([`Self::observe_exec`]).
    exec_observed: Option<Arc<AtomicBool>>,
    /// The dsh env file the app reads itself to hand credentials to the child
    /// (P9). Only consulted when `engine` is [`CodeEngine::Dsh`]; `None` for
    /// every other engine and when unset (the spawn then falls back to the
    /// configured default path and surfaces a typed error if it is unreadable).
    dsh_env_file: Option<String>,
    /// Directories the child's `PATH` must contain after the binary's own
    /// directory and before the parent's `PATH` (P9 live-defect fix). Seeded
    /// with [`awili_config::AgentBinsConfig`]'s defaults and extended with the
    /// configured `[work.agents].search_paths` by [`Self::with_search_paths`];
    /// `~` is expanded against `HOME` at spawn time (see [`build_child_path`]).
    search_paths: Vec<String>,
    /// P12: the resolved `awili-connectors` server entry to hand the vendor
    /// CLIs through their own MCP mechanisms (claude `--mcp-config`, codex
    /// `-c`). `None` when no connector family is enabled or the server binary
    /// cannot be resolved (then no flag is added at all).
    connectors_mcp: Option<awili_config::McpServerConfig>,
    /// `[work.agents.memory]` (RC1-M: the owner's Chat memory server and the
    /// Codex model overrides); `None` = none configured.
    agent_memory: Option<awili_config::AgentMemoryConfig>,
    /// RC1-M: `[work.agents] agents_md` (the user-level AGENTS.md).
    agents_md: Option<String>,
    /// RC2-R2I: the council this turn answers in, if any: a ready Codex
    /// server is kept for, and taken by, that council only.
    council: Option<String>,
    /// rc.4 S7: `[work.sandbox]`; default OFF (the launch is then unchanged).
    sandbox: awili_config::SandboxConfig,
}

impl CodeProvider {
    pub fn new(
        bin: String,
        engine: CodeEngine,
        autonomy: Autonomy,
        cwd: String,
        resume_id: Option<String>,
        model: Option<String>,
        session_slot: SessionSlot,
    ) -> Self {
        Self {
            bin,
            engine,
            autonomy,
            cwd,
            resume_id,
            model,
            session_slot,
            termination_observed: None,
            exec_observed: None,
            dsh_env_file: None,
            search_paths: awili_config::AgentBinsConfig::default().search_paths,
            connectors_mcp: None,
            agent_memory: None,
            agents_md: None,
            council: None,
            sandbox: awili_config::SandboxConfig::default(),
        }
    }

    /// Hand the `[work.agents.memory]` section to the launch.
    pub(crate) fn with_agent_memory(
        mut self,
        mem: Option<awili_config::AgentMemoryConfig>,
    ) -> Self {
        self.agent_memory = mem;
        self
    }

    /// rc.4 S7: the opt-in shell sandbox (`[work.sandbox]`) for Autonomous turns.
    pub(crate) fn with_sandbox(mut self, cfg: awili_config::SandboxConfig) -> Self {
        self.sandbox = cfg;
        self
    }

    /// RC1-M: the configured user-level AGENTS.md (`[work.agents] agents_md`).
    pub(crate) fn with_agents_md(mut self, path: Option<String>) -> Self {
        self.agents_md = path;
        self
    }

    /// RC2-R2I (RC2-R2F review P2 on d65da1fc): this turn answers in council
    /// `council`, so it takes only a server made ready for that council.
    pub(crate) fn for_council(mut self, council: &str) -> Self {
        self.council = Some(council.to_string());
        self
    }

    /// Attach the dsh env file (P9) the app reads itself to supply the child's
    /// credentials (`DEEPSEEK_API_KEY`/`DEEPSEEK_BASE_URL`). Only meaningful for
    /// [`CodeEngine::Dsh`]; a no-op intent for every other engine. The `~` is
    /// expanded by the caller (like every other configured path).
    pub(crate) fn with_dsh_env_file(mut self, path: String) -> Self {
        self.dsh_env_file = Some(path);
        self
    }

    /// Append the configured `[work.agents].search_paths` to the built-in
    /// defaults for the child's `PATH` (P9 live-defect fix). The defaults are
    /// always present (a Finder-launched .app has only the system `PATH`, and
    /// the vendor CLI's own `#!/usr/bin/env <interp>` lookup needs them), and
    /// the caller's configured entries are added after them. The final `PATH`
    /// is de-duplicated by [`build_child_path`]; `~` is expanded there.
    pub(crate) fn with_search_paths(mut self, paths: Vec<String>) -> Self {
        self.search_paths.extend(paths);
        self
    }

    /// Attach the P12 `awili-connectors` server entry so the child gets the
    /// connector tools through the vendor CLI's own MCP mechanism
    /// (`None`-free here: the caller resolves the entry and calls this only
    /// when one exists).
    pub(crate) fn with_connectors_mcp(mut self, spec: awili_config::McpServerConfig) -> Self {
        self.connectors_mcp = Some(spec);
        self
    }
}

impl CodeProvider {
    /// Separate from UI completion: true only after reaping, or no child spawned.
    pub(crate) fn observe_termination(mut self, observation: Arc<AtomicBool>) -> Self {
        self.termination_observed = Some(observation);
        self
    }

    /// RC2-CXSF: `observation` is set when this turn runs on
    /// `codex exec --json` instead of streaming over Codex's app server.
    pub(crate) fn observe_exec(mut self, observation: Arc<AtomicBool>) -> Self {
        self.exec_observed = Some(observation);
        self
    }

    /// Run one turn. Returns the event channel the UI drains. `cancel_rx` is the
    /// kill switch: sending on its paired sender (held by the Code tab as
    /// `stream_abort_tx`) KILLS the child process — not just stops reading it
    /// (review HIGH: a dropped reader left `claude`/`codex` running and editing).
    /// Dropping the sender (turn finished) lets the watcher thread exit cleanly.
    pub fn run(
        &self,
        req: ChatRequest,
        cancel_rx: std::sync::mpsc::Receiver<()>,
    ) -> async_channel::Receiver<ProviderEvent> {
        self.spawn_turn(req, cancel_rx, None)
    }

    /// RC2-R2F10: make this launch's Codex server ready for a later turn
    /// with `req`'s system text (its question is not sent): spawned, its
    /// MCP servers starting and its thread open, in the background. Does
    /// nothing for another engine or a resumed conversation. RC2-R2G5: the
    /// server is made for `owner` (a council), which ends it on closing
    /// ([`crate::codex_app_server::close_warm_for`]).
    pub(crate) fn prewarm(&self, req: ChatRequest, owner: &str) {
        let (_cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
        drop(self.spawn_turn(req, cancel_rx, Some(owner.to_string())));
    }

    fn spawn_turn(
        &self,
        req: ChatRequest,
        cancel_rx: std::sync::mpsc::Receiver<()>,
        warm_for: Option<String>,
    ) -> async_channel::Receiver<ProviderEvent> {
        let (tx, rx) = async_channel::unbounded::<ProviderEvent>();
        let termination_observed = self.termination_observed.clone();
        let exec_observed = self.exec_observed.clone();
        let bin = self.bin.clone();
        let engine = self.engine;
        let autonomy = self.autonomy;
        let cwd = self.cwd.clone();
        let resume_id = self.resume_id.clone();
        let model = self.model.clone();
        let slot = Arc::clone(&self.session_slot);
        let dsh_env_file = self.dsh_env_file.clone();
        let search_paths = self.search_paths.clone();
        // RC1-M: every step keeps the connector server. A planning step
        // changes nothing because the CLI holds it: Claude's plan mode, and
        // Codex's "writes" MCP approval (`agent_launch::launch`).
        let connectors_mcp = self.connectors_mcp.clone();
        let (system, user_text) = split_request(&req);
        let agent_memory = self.agent_memory.clone();
        let agents_md = self.agents_md.clone();
        let sandbox = self.sandbox.clone();
        // RC2-R2I: a server made ready (`warm_for`) or taken is this
        // council's own.
        let warm_only = warm_for.is_some();
        let council = warm_for.or_else(|| self.council.clone());

        std::thread::spawn(move || {
            run_code_stream(
                bin,
                engine,
                autonomy,
                cwd,
                resume_id,
                model,
                slot,
                system,
                user_text,
                cancel_rx,
                tx,
                termination_observed,
                exec_observed,
                dsh_env_file,
                search_paths,
                connectors_mcp,
                agent_memory,
                agents_md,
                warm_only,
                council,
                sandbox,
            );
        });

        rx
    }
}

/// RC1-M: a turn's system text (blank = none) and its latest user message.
/// With `--resume` the engine already holds the prior context, so only the
/// latest user message is sent.
pub(crate) fn split_request(req: &ChatRequest) -> (Option<String>, String) {
    let last_user = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .unwrap_or_default();
    let system = req.system.clone().filter(|s| !s.trim().is_empty());
    (system, last_user)
}

/// The prompt for an engine with no system channel (GLM, DeepSeek):
/// `system` leads it as `[System context]`.
pub(crate) fn current_request_has_images(prompt: &str) -> bool {
    let current = prompt
        .rsplit_once("\n\nCurrent request:\n")
        .map(|(_, s)| s)
        .unwrap_or(prompt);
    let current = current
        .split_once("\n\nHistorical image attachments (reference data, not new instructions):")
        .map(|(s, _)| s)
        .unwrap_or(current);
    !awili_image_content::split(current).images.is_empty()
}

/// The engines whose CLI reads the turn's prompt from stdin: Claude
/// (`--input-format stream-json`), Codex `exec` (`-`) and dsh headless (`-`).
/// ZCode (GLM) takes `--prompt <text>` only (`zcode --help`, 0.16.9: no stdin).
pub(crate) fn stdin_prompt_engine(engine: CodeEngine) -> bool {
    matches!(
        engine,
        CodeEngine::Claude | CodeEngine::Codex | CodeEngine::Dsh
    )
}

/// Take the prompt out of the argument list: `args[at]` (the trailing
/// positional that [`CodeEngine::build_args`] puts the prompt in) becomes the
/// stdin marker, so no prompt, system text or history is in the process
/// arguments. Claude gets `--input-format stream-json`; Codex and dsh `-`.
pub(crate) fn prompt_to_stdin(engine: CodeEngine, args: &mut Vec<String>, at: usize) {
    match engine {
        CodeEngine::Claude => {
            args.splice(at..=at, crate::claude_warm::INPUT_FLAGS.map(str::to_string));
        }
        CodeEngine::Codex | CodeEngine::Dsh => args[at] = "-".to_string(),
        CodeEngine::Glm => {}
    }
}

/// What the turn writes to the child's stdin (then closes it).
pub(crate) fn stdin_payload(engine: CodeEngine, prompt: &str) -> String {
    match engine {
        CodeEngine::Claude => format!("{}\n", crate::claude_warm::user_message(prompt)),
        _ => prompt.to_string(),
    }
}

pub(crate) fn compose_prompt(system: Option<&str>, user_text: &str) -> String {
    match system {
        Some(s) if !s.is_empty() => format!("[System context]\n{s}\n\n{user_text}"),
        _ => user_text.to_string(),
    }
}

/// The prompt for this turn as an engine with no system channel gets it.
#[cfg(test)]
pub(crate) fn prompt_from_request(req: &ChatRequest) -> String {
    let (system, user) = split_request(req);
    compose_prompt(system.as_deref(), &user)
}

/// Build the `PATH` handed to a spawned engine child (P9 live-defect fix).
///
/// A Finder-launched `.app` inherits only the system `PATH`, and a vendor CLI
/// may itself be a script whose shebang is `#!/usr/bin/env <interp>` (the real
/// `~/.local/bin/dsh` is `#!/usr/bin/env node`, with `node` in `~/.local/bin`).
/// This ordering gives such a CLI a `PATH` that can find its interpreter:
///
/// 1. the directory containing the resolved binary,
/// 2. every configured agent search path (`~` expanded against `HOME`),
/// 3. the parent process `PATH`, or the system default when absent.
///
/// Entries are de-duplicated with the first occurrence winning, then joined
/// with `:`. Pure over its inputs so the ordering is unit-testable.
fn build_child_path(bin: &str, search_paths: &[String], parent_path: Option<&str>) -> String {
    fn push_entry(entries: &mut Vec<String>, entry: &str) {
        let entry = entry.trim();
        if entry.is_empty() || entries.iter().any(|e| e == entry) {
            return;
        }
        entries.push(entry.to_string());
    }

    let mut entries: Vec<String> = Vec::new();
    if let Some(dir) = std::path::Path::new(bin).parent() {
        push_entry(&mut entries, &dir.to_string_lossy());
    }
    for dir in search_paths {
        let expanded = crate::work::expand_home(dir);
        // Match `resolve_agent_bin`'s trailing-slash handling without turning
        // the root directory `/` into the empty string.
        let trimmed = expanded.trim_end_matches(PATH_TRIM);
        let entry = if trimmed.is_empty() {
            expanded.as_str()
        } else {
            trimmed
        };
        push_entry(&mut entries, entry);
    }
    match parent_path {
        Some(p) if !p.trim().is_empty() => {
            for part in p.split(PATH_SEP) {
                push_entry(&mut entries, part);
            }
        }
        _ => {
            for part in DEFAULT_SYSTEM_PATH.split(PATH_SEP) {
                push_entry(&mut entries, part);
            }
        }
    }
    entries.join(&PATH_SEP.to_string())
}

/// `PATH` entry separator.
#[cfg(windows)]
const PATH_SEP: char = ';';
#[cfg(not(windows))]
const PATH_SEP: char = ':';
/// Trailing separators trimmed off a configured directory.
#[cfg(windows)]
const PATH_TRIM: &[char] = &['/', '\\'];
#[cfg(not(windows))]
const PATH_TRIM: &[char] = &['/'];
/// The system `PATH` used when the parent has none.
#[cfg(windows)]
const DEFAULT_SYSTEM_PATH: &str = "C:\\Windows\\System32;C:\\Windows";
#[cfg(not(windows))]
const DEFAULT_SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// P12: writes the claude `--mcp-config` JSON for one turn and returns its
/// path (a [`McpConfigGuard`] removes it when the turn ends — even a
/// spawn-failure early return). Returns `None` when the data dir cannot be
/// determined or the write fails (the turn then runs without the flag).
/// The document carries ONLY the server binary path and the `--config`
/// path — never a secret (the server reads the config itself at call time).
fn write_claude_mcp_config(spec: &awili_config::McpServerConfig) -> Option<String> {
    let dir = dirs::data_dir()?.join(awili_config::APP_DIR_NAME).join("mcp");
    if awili_config::ensure_private_app_dir(&dir).is_err() {
        return None;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!(
        "awili-connectors-{}-{nanos}.json",
        std::process::id()
    ));
    let doc = serde_json::json!({
        "mcpServers": {
            crate::connectors_mcp::CONNECTORS_SERVER_NAME: {
                "command": spec.command,
                "args": spec.args,
            }
        }
    });
    // A `--mcp-config` file could be read by anything with this account's
    // uid; `0600` is the minimum for a file that names our tooling, even
    // though it holds no secret. `create_new` refuses to clobber.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .ok()?;
        file.write_all(doc.to_string().as_bytes()).ok()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, doc.to_string()).ok()?;
    }
    Some(path.to_string_lossy().into_owned())
}

/// RAII: removes the turn's `--mcp-config` file. Dropped at the end of
/// `run_code_stream` (and on the spawn-failure early return), so a config
/// never outlives the turn that named it.
pub(crate) struct McpConfigGuard(String);

impl Drop for McpConfigGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// RAII: removes the turn's `--append-system-prompt-file` file. Held for the
/// whole turn by `run_code_stream` (every return path, spawn failure and
/// stop included), and by a ready Claude until it ends.
pub(crate) struct BriefFile(std::path::PathBuf);

impl Drop for BriefFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The directory the briefs live in. Test builds outside a private HOME use a
/// temp directory instead, never the owner's real app data (R2h).
fn brief_dir() -> Result<std::path::PathBuf, String> {
    #[cfg(test)]
    if !std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .is_some_and(|h| crate::test_home_guard::inside(&h, &std::env::temp_dir()))
    {
        return Ok(std::env::temp_dir().join("awili-test-brief"));
    }
    dirs::data_dir()
        .map(|d| d.join(awili_config::APP_DIR_NAME).join("brief"))
        .ok_or_else(|| "there is no data folder".to_string())
}

/// Make sure `dir` is a real directory (not a link) owned by this user, mode
/// 0700; create it when missing. Anything else is refused.
#[cfg(windows)]
fn prepare_brief_dir(dir: &std::path::Path) -> Result<(), String> {
    // No uid on Windows: the equivalent of "owned by this user, 0700" is an
    // owner-only protected DACL. A folder belonging to another account cannot
    // be re-protected, so it is refused the same way. Links are refused.
    let protect = || {
        awili_winacl::ensure_owner_only(dir, true)
            .map(|_| ())
            .map_err(|e| format!("cannot protect {}: {e}", dir.display()))
    };
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.file_type().is_dir() {
                return Err(format!("{} is not a plain folder", dir.display()));
            }
            protect()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Parents first, then the folder itself created WITH the
            // owner-only descriptor (never visible with a broader list).
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            awili_winacl::create_dir_owner_only(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))
        }
        Err(e) => Err(format!("cannot open {}: {e}", dir.display())),
    }
}

#[cfg(unix)]
fn prepare_brief_dir(dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.file_type().is_dir() {
                return Err(format!("{} is not a plain folder", dir.display()));
            }
            // SAFETY: geteuid has no preconditions.
            if meta.uid() != unsafe { libc::geteuid() } {
                return Err(format!("{} belongs to another user", dir.display()));
            }
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot protect {}: {e}", dir.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))
            .and_then(|()| {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| format!("cannot protect {}: {e}", dir.display()))
            }),
        Err(e) => Err(format!("cannot open {}: {e}", dir.display())),
    }
}

/// Write `text` to a new private file (0600, unique per call) in `dir`.
fn write_brief_file_in(dir: &std::path::Path, text: &str) -> Result<BriefFile, String> {
    use std::io::Write;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    prepare_brief_dir(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!("brief-{}-{nanos}-{n}.md", std::process::id()));
    // Windows: created WITH the owner-only descriptor and read back; no
    // instant with a broader list. Fail closed: no file, no write.
    #[cfg(windows)]
    let mut file = awili_winacl::create_new_file_owner_only(&path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    #[cfg(not(windows))]
    let mut file = {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&path)
            .map_err(|e| format!("cannot create {}: {e}", path.display()))?
    };
    let guard = BriefFile(path);
    file.write_all(text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", guard.0.display()))?;
    Ok(guard)
}

/// What the owner sees when a run's instructions cannot be prepared.
fn brief_error(reason: &str) -> String {
    format!("Awili could not prepare this run's instructions: {reason}. Nothing was sent.")
}

/// Claude: move Awili's brief (`--append-system-prompt <text>`) out of the
/// argument list into a private file named by `--append-system-prompt-file`.
/// Fails closed: when the file cannot be prepared the brief is NOT left on
/// the argv; the error says so. `Ok(None)` when the launch has no brief.
pub(crate) fn brief_to_file(args: &mut [String]) -> Result<Option<BriefFile>, String> {
    let Some(at) = args.iter().position(|a| a == "--append-system-prompt") else {
        return Ok(None);
    };
    let Some(text) = args.get(at + 1).cloned() else {
        return Ok(None);
    };
    let file = brief_dir()
        .and_then(|dir| write_brief_file_in(&dir, &text))
        .map_err(|reason| brief_error(&reason))?;
    args[at] = "--append-system-prompt-file".to_string();
    args[at + 1] = file.0.display().to_string();
    Ok(Some(file))
}

/// Remove the `brief-*.md` files an earlier crashed run left in `dir`: plain
/// files owned by this user only (never links, never other names). Returns
/// how many were removed.
#[cfg(windows)]
pub(crate) fn sweep_briefs_in(dir: &std::path::Path) -> usize {
    // The owner check: the folder must be owner-only (this user and SYSTEM),
    // which `prepare_brief_dir` guarantees for the real one. If it is not,
    // nothing in it is trusted as ours and nothing is removed.
    if awili_winacl::verify_owner_only(dir).is_err() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("brief-") && name.ends_with(".md")) {
            continue;
        }
        let alive = name
            .strip_prefix("brief-")
            .and_then(|r| r.split('-').next())
            .and_then(|p| p.parse::<i32>().ok())
            .is_some_and(crate::proc_compat::pid_exists);
        if alive {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.file_type().is_file() && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(unix)]
pub(crate) fn sweep_briefs_in(dir: &std::path::Path) -> usize {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("brief-") && name.ends_with(".md")) {
            continue;
        }
        // A brief of a run still going in another Awili stays: its name carries
        // the process id.
        let alive = name
            .strip_prefix("brief-")
            .and_then(|r| r.split('-').next())
            .and_then(|p| p.parse::<i32>().ok())
            .is_some_and(crate::proc_compat::pid_exists);
        if alive {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.file_type().is_file()
            && meta.uid() == me
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// At startup: remove leftover briefs from earlier runs that did not finish.
pub(crate) fn sweep_leftover_briefs() {
    if let Ok(dir) = brief_dir() {
        sweep_briefs_in(&dir);
    }
}

/// RC1-G3-FIX1: end a turn: its `--mcp-config` file (if any) is removed
/// FIRST, then the terminal `events` go out through `send`, so a reader acting
/// on them (`Done` above all) can never still find the file. Every terminal
/// path of [`run_code_stream`] after the file is written ends through here.
fn end_turn(
    mcp_config: Option<McpConfigGuard>,
    events: Vec<ProviderEvent>,
    mut send: impl FnMut(ProviderEvent),
) {
    drop(mcp_config);
    for ev in events {
        send(ev);
    }
}

/// Escape one string for embedding in a TOML value (codex `-c` keys are
/// dot-notation TOML: `mcp_servers.<name>.command="<value>"`).
fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Escape a string array for a TOML value: `["--config", "/abs/path"]`.
fn toml_string_array(v: &[String]) -> String {
    let inner: Vec<String> = v.iter().map(|s| toml_string(s)).collect();
    format!("[{}]", inner.join(", "))
}

/// P12: the codex `-c mcp_servers.<name>.command=…` / `…args=[…]` overrides
/// that hand it the `awili-connectors` server (codex has no config-file
/// flag). RC2-CXS: the app server takes the same overrides as `exec`.
fn codex_connector_overrides(spec: &awili_config::McpServerConfig) -> Vec<String> {
    let name = crate::connectors_mcp::CONNECTORS_SERVER_NAME;
    vec![
        "-c".to_string(),
        format!("mcp_servers.{name}.command={}", toml_string(&spec.command)),
        "-c".to_string(),
        format!("mcp_servers.{name}.args={}", toml_string_array(&spec.args)),
    ]
}

/// Spawn the engine and pump its stdout onto `tx` as [`ProviderEvent`]s. Kills
/// the child if `cancel_rx` fires; reaps it on exit either way (no orphan).
#[allow(clippy::too_many_arguments)]
fn run_code_stream(
    bin: String,
    engine: CodeEngine,
    autonomy: Autonomy,
    cwd: String,
    resume_id: Option<String>,
    model: Option<String>,
    session_slot: SessionSlot,
    system: Option<String>,
    user_text: String,
    cancel_rx: std::sync::mpsc::Receiver<()>,
    tx: async_channel::Sender<ProviderEvent>,
    termination_observed: Option<Arc<AtomicBool>>,
    exec_observed: Option<Arc<AtomicBool>>,
    dsh_env_file: Option<String>,
    search_paths: Vec<String>,
    connectors_mcp: Option<awili_config::McpServerConfig>,
    agent_memory: Option<awili_config::AgentMemoryConfig>,
    agents_md: Option<String>,
    warm_only: bool,
    council: Option<String>,
    sandbox: awili_config::SandboxConfig,
) {
    // RC2-R2F10: only a Codex server can be made ready ahead (RC2-R2G5,
    // RC2-R2I: for `council`, the council it is made for). RC2-R3: and a
    // council's Claude member (`claude_warm`).
    if warm_only
        && (!matches!(engine, CodeEngine::Codex | CodeEngine::Claude) || resume_id.is_some())
    {
        return;
    }
    // RC1-M: the owner's own setup, AGENTS.md only, Awili's text through the
    // CLI's system channel (`agent_launch`). With no home directory known the
    // launch still runs, with no user-level AGENTS.md and no excludes path
    // for the home (the `**/CLAUDE.md` globs still apply).
    let known_paths = crate::agent_launch::LaunchPaths::production();
    let paths = known_paths.clone().unwrap_or_else(|| {
        crate::agent_launch::LaunchPaths {
            home: std::path::PathBuf::from("/nonexistent-home"),
            codex_home: std::path::PathBuf::from("/nonexistent-home/.codex"),
        }
    });
    // rc.4 S7: the opt-in shell sandbox. Off (the default) or a planning step:
    // no profile and every launch below is exactly what it always was. On and
    // Autonomous: the profile (or a plain reason the turn cannot start; the
    // sandbox is never silently dropped).
    let sandbox_profile = if sandbox.enabled {
        crate::agent_sandbox::data_dir().and_then(|dir| {
            crate::agent_sandbox::prepare(
                &sandbox,
                engine,
                autonomy,
                &cwd,
                known_paths.as_ref(),
                &dir,
            )
        })
    } else {
        Ok(None)
    };
    let sandbox_profile = match sandbox_profile {
        Ok(p) => p,
        Err(message) => {
            if warm_only {
                return;
            }
            if let Some(observation) = &termination_observed {
                observation.store(true, Ordering::Release);
            }
            end_turn(
                None,
                vec![ProviderEvent::Error(message), ProviderEvent::Done],
                |ev| {
                    let _ = tx.send_blocking(ev);
                },
            );
            return;
        }
    };
    let connectors_name = connectors_mcp
        .as_ref()
        .filter(|_| matches!(engine, CodeEngine::Claude | CodeEngine::Codex))
        .map(|_| crate::connectors_mcp::CONNECTORS_SERVER_NAME);
    let launch = crate::agent_launch::launch(
        &paths,
        &crate::agent_launch::LaunchSpec {
            engine,
            autonomy,
            cwd: std::path::Path::new(&cwd),
            system: system.as_deref(),
            mem: agent_memory.as_ref(),
            agents_md: agents_md.as_deref(),
            connectors: connectors_name,
        },
    );
    let mut prompt = compose_prompt(launch.prompt_system.as_deref(), &user_text);
    // A text-only harness may continue after an old attachment: only a new
    // image request requires image support. Keep the history placeholder.
    if matches!(engine, CodeEngine::Dsh | CodeEngine::Glm) && !current_request_has_images(&prompt) {
        prompt = awili_image_content::split(&prompt).text;
    }
    // GLM note (verified 2026-08-23): zcode 0.16.3's help documents a
    // `--settings <path>` flag but its parser rejects it ("Unknown option"),
    // so there is NO per-run model mechanism — a --settings overlay made
    // zcode print its help and fail the run. GLM therefore always runs on
    // its own configured main model; `agent_model_options` offers no GLM
    // rows, and `build_args` ignores `model` for Glm by design.
    let image_input = !awili_image_content::split(&prompt).images.is_empty();
    let args = engine.build_args(&prompt, autonomy, resume_id.as_deref(), model.as_deref());
    // Claude, Codex `exec` and dsh are asked on stdin, so the prompt, system
    // text and history never appear in the process arguments (see
    // [`prompt_to_stdin`]). GLM's zcode has no stdin prompt.
    let stdin_turn = !warm_only && stdin_prompt_engine(engine);
    let claude_image_stdin = image_input && stdin_turn && engine == CodeEngine::Claude;
    let mut args = crate::agent_launch::apply_args(args, &launch.args);
    if claude_image_stdin {
        // apply_args inserts settings/system flags before the final prompt.
        // Convert that prompt to stdin only AFTER all such insertions, so
        // --input-format and stream-json remain an adjacent flag/value pair.
        args.pop();
        args.extend(crate::claude_warm::INPUT_FLAGS.map(str::to_string));
    }

    // RC2-CXS (R3-20 Codex part): a Codex turn runs over its app server so
    // the reply streams as it is written — the same launch `-c` overrides
    // and connector server, the posture on the argv and the thread request.
    // A Codex that does not answer `initialize` runs this turn (and the
    // session's later ones) on `codex exec --json` below. Codex never has
    // an `--mcp-config` file, so its turn ends with no guard to drop.
    let cancel_rx =
        if engine == CodeEngine::Codex && !crate::codex_app_server::exec_fallback_for(&bin) {
            let mut server_args = engine.app_server_args(autonomy);
            server_args.extend(launch.args.iter().cloned());
            if let Some(spec) = &connectors_mcp {
                server_args.extend(codex_connector_overrides(spec));
            }
            let child_path =
                build_child_path(&bin, &search_paths, std::env::var("PATH").ok().as_deref());
            let turn = crate::codex_app_server::Turn {
                bin: &bin,
                args: &server_args,
                cwd: &cwd,
                child_path: &child_path,
                owner: council.as_deref().unwrap_or(""),
                sandbox: sandbox_profile.as_deref(),
                spec: awili_code::app_server::TurnSpec {
                    cwd: cwd.clone(),
                    prompt: prompt.clone(),
                    resume_id: resume_id.clone(),
                    model: model.clone(),
                    autonomy,
                },
            };
            // RC2-R2F10: made ready for a later question, not run.
            if warm_only {
                crate::codex_app_server::prewarm(turn);
                return;
            }
            match crate::codex_app_server::run_turn(
                turn,
                cancel_rx,
                &tx,
                &session_slot,
                &termination_observed,
            ) {
                Ok(events) => {
                    end_turn(None, events, |ev| {
                        let _ = tx.send_blocking(ev);
                    });
                    return;
                }
                Err(cancel_rx) => cancel_rx,
            }
        } else {
            cancel_rx
        };
    if image_input && engine != CodeEngine::Claude && current_request_has_images(&prompt) {
        end_turn(None, vec![ProviderEvent::Error("This native harness cannot accept image input here. Use Codex with app-server or Claude, or an in-app vision model.".into()), ProviderEvent::Done], |ev| { let _ = tx.send_blocking(ev); });
        return;
    }
    if image_input && engine == CodeEngine::Codex {
        // exec fallback cannot carry typed images. Historical images need not
        // prevent a text follow-up, but must never be passed as base64 prose.
        prompt = awili_image_content::split(&prompt).text;
        args = engine.build_args(&prompt, autonomy, resume_id.as_deref(), model.as_deref());
        args = crate::agent_launch::apply_args(args, &launch.args);
    }
    // RC2-R3 (usability RC2 round 3, F7): a council's Claude member: the
    // launch a Claude made ready ahead must match (`claude_warm`).
    let claude_key = (engine == CodeEngine::Claude && resume_id.is_none() && !image_input)
        .then(|| council.clone())
        .flatten()
        .filter(|owner| !owner.is_empty())
        .map(|owner| crate::claude_warm::WarmKey {
            bin: bin.clone(),
            args: crate::claude_warm::warm_args(&args),
            cwd: cwd.clone(),
            owner,
            connectors: connectors_mcp
                .as_ref()
                .map(|s| (s.command.clone(), s.args.clone())),
            sandbox: sandbox_profile.clone(),
        });
    if warm_only && claude_key.is_none() {
        return;
    }
    // RC2-R3: a Claude made ready for exactly this launch takes the question
    // (its start-up, the owner's SessionStart hook included, is done).
    let mut warm = if warm_only {
        None
    } else {
        claude_key.as_ref().and_then(crate::claude_warm::take)
    };
    let base_len = args.len();
    // RC2-CXSF (RC2-CXS review P3): this run's Codex turn runs on exec.
    if engine == CodeEngine::Codex {
        if let Some(observation) = &exec_observed {
            observation.store(true, Ordering::Release);
        }
    }

    // P12: the vendor CLIs take the shared `awili-connectors` server through
    // their OWN MCP mechanisms:
    //   claude → `--mcp-config <file>`: a JSON document naming the server.
    //            The file is created 0600 and REMOVED after the run (a
    //            claude-mcp config left on disk would outlive the turn).
    //   codex  → `-c mcp_servers.<name>.command=…` / `…args=[…]`: codex has
    //            no config-file flag, only dot-notation `-c` overrides.
    //   zcode/dsh → NO flag: neither CLI exposes a headless MCP mechanism
    //            (documented in docs/daily-driver/specs/P12-DEVIATIONS.md).
    // The entry carries only the binary path + the config path — never a
    // secret. If the temp file cannot be written, the turn runs WITHOUT the
    // connectors flag (degraded, never a failure of the whole turn). RC1-M:
    // a planning step allows the server by name (Claude) or holds it to
    // "writes" approval (Codex) in `agent_launch::launch`; the household and
    // owner Chats never get it (no provider hands it to them).
    let mcp_config_guard = match &connectors_mcp {
        // RC2-R3: a ready Claude brought its own file (named in its launch).
        _ if warm.is_some() => warm.as_mut().and_then(|w| w.guard.take()),
        Some(spec) if engine == CodeEngine::Claude => {
            let path = write_claude_mcp_config(spec);
            if let Some(p) = &path {
                args.push("--mcp-config".to_string());
                args.push(p.clone());
            }
            path.map(McpConfigGuard)
        }
        Some(spec) if engine == CodeEngine::Codex => {
            args.extend(codex_connector_overrides(spec));
            None
        }
        _ => None, // zcode / dsh / no connectors entry: no MCP flag.
    };
    // RC2-R3: made ready, not run: the Claude waits for its question with its
    // own `--mcp-config` file.
    if warm_only {
        if let Some(key) = claude_key {
            let mut ready_args = key.args.clone();
            ready_args.extend(args[base_len..].iter().cloned());
            // Fails closed: no brief file, no ready Claude (the turn asks for
            // its own, and reports the failure).
            let Ok(brief) = brief_to_file(&mut ready_args) else {
                return;
            };
            let child_path =
                build_child_path(&bin, &search_paths, std::env::var("PATH").ok().as_deref());
            crate::claude_warm::prewarm(key, &ready_args, &child_path, mcp_config_guard, brief);
        }
        return;
    }
    // The prompt (last argument before any `--mcp-config` appended above)
    // moves to stdin (Claude: the stream-json flags take its place, adjacent).
    if stdin_turn && !claude_image_stdin {
        prompt_to_stdin(engine, &mut args, base_len - 1);
    }
    // Claude's brief goes in a private file too, not the argv; the file is
    // removed when this function returns (turn end, spawn failure or stop).
    let _brief_file = if engine == CodeEngine::Claude {
        match brief_to_file(&mut args) {
            Ok(file) => file,
            Err(message) => {
                if let Some(w) = warm.take() {
                    crate::claude_warm::end(w);
                }
                if let Some(observation) = &termination_observed {
                    observation.store(true, Ordering::Release);
                }
                end_turn(
                    mcp_config_guard,
                    vec![ProviderEvent::Error(message), ProviderEvent::Done],
                    |ev| {
                        let _ = tx.send_blocking(ev);
                    },
                );
                return;
            }
        }
    } else {
        None
    };
    // A ready Claude brought its own brief file; it goes with the turn.
    let _warm_brief = warm.as_mut().and_then(|w| w.brief.take());

    // dsh (P9): a macOS .app does not inherit a login shell, so the child's
    // credentials must be read by Awili itself from a file it reads itself.
    // Dsh-only (keyed on engine) — no other engine gets extra env. Values are
    // credentials: NEVER log, persist, or render them; only the PATH may be
    // logged. Telemetry is always off for a child Awili spawns; the chosen
    // model travels in `DSH_MODEL` (the picker's "default" = `None` means NO
    // `DSH_MODEL` for the child at all — an inherited parent value is scrubbed,
    // see `env_remove` below, so a runner that exports its own `DSH_MODEL`
    // cannot leak into a "default" turn). A missing/unreadable file still
    // spawns: dsh's own error surfaces with this reason prepended.
    let mut dsh_env: Vec<(String, String)> = Vec::new();
    let dsh_env_note: Option<String> = if engine == CodeEngine::Dsh {
        let path = dsh_env_file.unwrap_or_else(|| awili_config::DEFAULT_DSH_ENV_FILE.to_string());
        if path.trim().is_empty() {
            // No env file configured: dsh gets only its own environment, and
            // the run says why a missing key may be the reason.
            Some("no DeepSeek key file configured".to_string())
        } else {
            let expanded = crate::work::expand_home(&path);
            match std::fs::read_to_string(&expanded) {
                Ok(text) => {
                    dsh_env = parse_dsh_env(&text);
                    None
                }
                Err(_) => Some(format!("dsh env file {expanded} not readable")),
            }
        }
    } else {
        None
    };
    if engine == CodeEngine::Dsh {
        dsh_env.push(("DSH_TELEMETRY_MODE".to_string(), "DISABLED".to_string()));
        // rc.4 S7: dsh confines its own shell with a nested `sandbox-exec`,
        // which macOS refuses inside ours (live probe 2026-11-01: "sandbox_apply:
        // Operation not permitted", every command fails). Under Awili's sandbox
        // the confinement is ours, so dsh's own layer is told to stand down.
        // Never set when the sandbox is off: dsh's default stands then.
        if sandbox_profile.is_some() {
            dsh_env.push((
                "DSH_PERMISSION_MODE".to_string(),
                "danger-full-access".to_string(),
            ));
        }
        if let Some(m) = &model {
            dsh_env.push(("DSH_MODEL".to_string(), m.clone()));
        }
        // dsh's DeepSeek Messages adapter appends `/v1/messages` itself. Its
        // documented official root therefore includes `/anthropic`; a shared
        // credential file written with the API-origin root otherwise yields a
        // 404. Apply this only to that exact known root, after resolving the
        // effective file-over-inherited value, and only to this child command.
        // Custom gateways and already-complete endpoints stay byte-for-byte as
        // supplied; neither this nor `cmd.env` mutates Awili's parent env.
        if let Some(base_url) = dsh_child_base_url_override(
            engine,
            &dsh_env,
            std::env::var("DEEPSEEK_BASE_URL").ok().as_deref(),
        ) {
            dsh_env.push(("DEEPSEEK_BASE_URL".to_string(), base_url.to_string()));
        }
    }

    let (program, full_args) =
        crate::agent_sandbox::launch_command(sandbox_profile.as_deref(), &bin, &args);
    let mut cmd = Command::new(&program);
    cmd.args(&full_args)
        .current_dir(&cwd)
        .stdin(if stdin_turn {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group (leader pgid == child pid) so cancellation kills the
        // agent and any tool subprocesses that stay in its group. CAVEAT (verified
        // 2026-06-24): claude-code detaches some tool commands into their OWN
        // session (observed PPID reparented to launchd, separate pgid), so a
        // command already executing when cancel/quit lands can survive to
        // completion — the agent is killed, but its detached children are
        // unreachable from here. This stops the agent immediately; it does not
        // guarantee an in-flight detached command dies.
        .own_process_group();
    for (k, v) in &dsh_env {
        cmd.env(k, v);
    }
    // "default" model (P9): the child must end up with NO `DSH_MODEL` so the
    // headless route's own default stands. `Command` inherits the parent's
    // environment, so merely not setting it is not enough — a parent that
    // exports `DSH_MODEL` (a runner, a shell) would leak into the turn.
    // `env_remove` scrubs exactly that one inherited variable; all other
    // inherited variables are untouched.
    if engine == CodeEngine::Dsh && model.is_none() {
        cmd.env_remove("DSH_MODEL");
    }
    // P9 live-defect fix: a Finder-launched .app has only the system `PATH`, so
    // a vendor CLI script whose shebang is `#!/usr/bin/env <interp>` (the real
    // `~/.local/bin/dsh` -> node) could not find its interpreter. Hand every
    // engine's child an explicit `PATH`: the binary's directory, the configured
    // agent search paths, then the parent `PATH`.
    let child_path = build_child_path(&bin, &search_paths, std::env::var("PATH").ok().as_deref());
    cmd.env("PATH", child_path);
    // RC2-R3: the ready Claude is asked on its stdin (then it is closed);
    // one that cannot take it is ended, and the turn starts its own.
    let had_ready = warm.is_some();
    let ready = warm.and_then(|mut w| {
        if crate::claude_warm::ask(&mut w.child, &prompt) {
            Some(w.child)
        } else {
            crate::claude_warm::end(w);
            None
        }
    });
    // RC2-R3M11 (RC2-R39 review P2): that start has the connectors too: its
    // own `--mcp-config` file (the ready one's went with it), placed after
    // the question as on any start.
    let mcp_config_guard = match connectors_mcp
        .as_ref()
        .filter(|_| had_ready && ready.is_none() && engine == CodeEngine::Claude)
    {
        Some(spec) => {
            let path = write_claude_mcp_config(spec);
            if let Some(p) = &path {
                cmd.arg("--mcp-config").arg(p);
            }
            path.map(McpConfigGuard)
        }
        None => mcp_config_guard,
    };
    let ready_taken = ready.is_some();
    let mut child = match ready.map(Ok).unwrap_or_else(|| crate::proc_compat::spawn_tracked(&mut cmd)) {
        Ok(c) => c,
        Err(e) => {
            if let Some(observation) = &termination_observed {
                observation.store(true, Ordering::Release);
            }
            // The single most common failure: the binary is not on PATH.
            // RC1-G3-FIX1: the `--mcp-config` file goes before Error/Done.
            end_turn(
                mcp_config_guard,
                vec![
                    ProviderEvent::Error(crate::agent_bin::start_failure(
                        engine.label(),
                        engine.bin(),
                        &bin,
                        &e,
                    )),
                    ProviderEvent::Done,
                ],
                |ev| {
                    let _ = tx.send_blocking(ev);
                },
            );
            return;
        }
    };

    if stdin_turn && !ready_taken {
        if let Some(mut stdin) = child.stdin.take() {
            let payload = stdin_payload(engine, &prompt);
            std::thread::spawn(move || {
                use std::io::Write;
                let _ = stdin.write_all(payload.as_bytes());
            });
        }
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Drain stderr on its own thread so a full stderr pipe can never deadlock the
    // stdout read; JOINED before we read the buffer so the error tail is complete.
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_handle = stderr.map(|mut err| {
        let buf = Arc::clone(&stderr_buf);
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            if let Ok(mut guard) = buf.lock() {
                *guard = s;
            }
        })
    });

    // The cancel watcher kills by PROCESS GROUP id (a plain integer), so it never
    // touches the `Child` — the main thread owns it outright and `wait()`s with no
    // lock (removes the wait-under-lock race). `done` distinguishes a real
    // cancel/abandonment (child still alive → kill the group) from a clean finish
    // (worker set `done`, leader already reaped → nothing to kill). The watcher
    // fires on BOTH an explicit cancel (recv Ok) AND a silent drop of the sender
    // (recv Err — e.g. a turn abandoned by teardown/tab-close), per review HIGH.
    let pgid = child.id() as i32;
    // Track this group so a graceful app-quit can kill it synchronously.
    register_group(pgid);
    let done = Arc::new(AtomicBool::new(false));
    let watcher_done = Arc::clone(&done);
    let watcher = std::thread::spawn(move || {
        let _ = cancel_rx.recv();
        if !watcher_done.load(Ordering::Acquire) {
            kill_process_group(pgid);
        }
    });

    let mut emitted_error = false;
    let mut parser = awili_code::stream::StreamParser::new(engine).with_model(model.clone());
    // Whole-output engines (ZCode --json) print ONE document at end-of-turn:
    // buffer stdout, parse at EOF, and feed the events through the same pump.
    let mut whole_buf = String::new();
    if let Some(stdout) = stdout {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if engine.buffers_whole_output() {
                // Bounded (ocr/codex review R2): the envelope is small; a
                // runaway child cannot balloon memory. 4MB keeps any sane
                // response with headroom; beyond it we keep the head (the
                // envelope precedes nothing) and stop accumulating.
                const WHOLE_BUF_CAP: usize = 4 * 1024 * 1024;
                if whole_buf.len() < WHOLE_BUF_CAP {
                    whole_buf.push_str(&line);
                    whole_buf.push('\n');
                }
                continue;
            }
            if engine == CodeEngine::Claude {
                trace_claude_terminal(&line);
            }
            for ev in parser.parse_line(&line) {
                // exec JSON does not provide an attributable resume baseline;
                // retain unknown usage rather than replaying thread totals.
                if engine == CodeEngine::Codex
                    && resume_id.is_some()
                    && matches!(&ev, awili_code::CodeEvent::Usage { .. })
                {
                    continue;
                }
                // Capture the session id for the next turn's resume; it carries
                // no ProviderEvent of its own.
                if let awili_code::CodeEvent::SessionStarted { session_id } = &ev {
                    if let Ok(mut slot) = session_slot.lock() {
                        *slot = Some(session_id.clone());
                    }
                    continue;
                }
                match ev.to_provider_event() {
                    // The terminal Done is owned by process exit (below), so the
                    // turn ends when the process does — not on an early result
                    // frame. Drop the mapped Done; forward everything else.
                    Some(ProviderEvent::Done) => {}
                    Some(ProviderEvent::Error(e)) => {
                        emitted_error = true;
                        if tx.send_blocking(ProviderEvent::Error(e)).is_err() {
                            break;
                        }
                    }
                    Some(other) => {
                        if tx.send_blocking(other).is_err() {
                            break;
                        }
                    }
                    None => {}
                }
            }
        }
    }
    // Empty stdout from a whole-output engine is a FAILURE, not a clean empty
    // answer (ocr/codex review R3): exit-zero with nothing written would
    // otherwise leave an empty assistant row marked Completed.
    if engine.buffers_whole_output() && whole_buf.trim().is_empty() {
        emitted_error = true;
        let msg = with_dsh_env_note(
            &dsh_env_note,
            "engine produced no output — the turn failed".to_string(),
        );
        let _ = tx.send_blocking(ProviderEvent::Error(msg));
    }
    if engine.buffers_whole_output() && !whole_buf.trim().is_empty() {
        for ev in engine.parse_whole(&whole_buf) {
            if let awili_code::CodeEvent::SessionStarted { session_id } = &ev {
                if let Ok(mut slot) = session_slot.lock() {
                    *slot = Some(session_id.clone());
                }
                continue;
            }
            match ev.to_provider_event() {
                Some(ProviderEvent::Done) => {}
                Some(ProviderEvent::Error(e)) => {
                    emitted_error = true;
                    let e = with_dsh_env_note(&dsh_env_note, e);
                    let _ = tx.send_blocking(ProviderEvent::Error(e));
                }
                Some(other) => {
                    let _ = tx.send_blocking(other);
                }
                None => {}
            }
        }
    }

    // Reap the leader (whether it exited on its own or the group was killed).
    let status = child.wait().ok();
    if status.is_some() {
        if let Some(observation) = &termination_observed {
            observation.store(true, Ordering::Release);
        }
    }
    // Mark finished BEFORE the watcher can observe it: a later sender-drop must
    // not trigger a stray group kill against a now-reaped (possibly reused) pgid.
    done.store(true, Ordering::Release);
    deregister_group(pgid);
    let _ = watcher; // detached; exits when its sender fires or drops
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }
    let stderr = stderr_buf
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    // RC1-G3: the turn's `--mcp-config` file is removed BEFORE its terminal
    // events go out. It used to be removed when this function returned, after
    // `Done`, so a reader acting on `Done` could still find it (the RC-16
    // `claude_mcp_config_flag_written_0600_and_removed` flake under load).
    let events = finalize(status.and_then(|s| s.code()), emitted_error, &stderr)
        .into_iter()
        .map(|ev| {
            // For dsh with an unreadable env file, surface dsh's own error with the
            // reason prepended (P9) so the user knows to fix the file.
            if engine == CodeEngine::Dsh {
                match ev {
                    ProviderEvent::Error(msg) => {
                        ProviderEvent::Error(with_dsh_env_note(&dsh_env_note, msg))
                    }
                    other => other,
                }
            } else {
                ev
            }
        })
        .collect();
    end_turn(mcp_config_guard, events, |ev| {
        let _ = tx.send_blocking(ev);
    });
}

/// Parse a dsh env file body into `(key, value)` pairs (P9). Lines matching
/// `^(export\s+)?([A-Z_][A-Z0-9_]*)=(.*)$` yield one pair each; comments and
/// blank lines are skipped, and ONE layer of matching quotes is stripped from
/// the value. Returns values in file order. The values are credentials — the
/// caller must never log, persist, or render them (only the file PATH may be
/// logged). Pure over its input so the parse is unit-testable without a file.
fn parse_dsh_env(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `^(export\s+)?`: an optional `export` keyword followed by at least one
        // whitespace. Only strip it when whitespace actually follows (so a
        // literal key named `export…` is not mangled).
        let rest = match line.strip_prefix("export") {
            Some(after) if after.starts_with(char::is_whitespace) => after.trim_start(),
            _ => line,
        };
        // `([A-Z_][A-Z0-9_]*)=`: the `=` directly follows the key (no space).
        let Some((key, value)) = rest.split_once('=') else {
            continue;
        };
        if !is_dsh_env_key(key) {
            continue;
        }
        out.push((key.to_string(), strip_matching_quotes(value.trim())));
    }
    out
}

/// A legal dsh env variable name: `[A-Z_][A-Z0-9_]*` (matches the spec's
/// `([A-Z_][A-Z0-9_]*)`).
fn is_dsh_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The Dsh-only child override for DeepSeek's official Messages root. Values
/// read from its env file win over inherited values because `Command::env`
/// applies them after environment inheritance, and the last duplicate in the
/// file is the value the child would receive. Only the exact historical root
/// (with an optional trailing slash) is normalized; full official paths and
/// custom provider endpoints are deliberately untouched.
fn dsh_child_base_url_override(
    engine: CodeEngine,
    file_env: &[(String, String)],
    inherited: Option<&str>,
) -> Option<&'static str> {
    if engine != CodeEngine::Dsh {
        return None;
    }
    let effective = file_env
        .iter()
        .rev()
        .find_map(|(key, value)| (key == "DEEPSEEK_BASE_URL").then_some(value.as_str()))
        .or(inherited);
    match effective {
        Some("https://api.deepseek.com" | "https://api.deepseek.com/") => {
            Some("https://api.deepseek.com/anthropic")
        }
        _ => None,
    }
}

/// Strip ONE layer of matching single or double quotes from a value. Non-matching
/// or absent quotes are left as-is (the value is otherwise passed through).
fn strip_matching_quotes(value: &str) -> String {
    let bytes = value.as_bytes();
    if value.len() >= 2
        && ((bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// Prepend the dsh env-file note to a dsh error message (P9): when the env file
/// was missing/unreadable, dsh's own error surfaces with the reason FIRST so the
/// user knows to fix the file. A `None` note (a readable file, or any non-Dsh
/// engine) leaves the message untouched.
fn with_dsh_env_note(note: &Option<String>, msg: String) -> String {
    match note {
        Some(n) => format!("{n}: {msg}"),
        None => msg,
    }
}

/// Process-group ids of all in-flight Code turns. The app SIGKILLs these
/// synchronously on graceful quit (see [`kill_all_active_groups`]) — the per-turn
/// watcher thread can't be relied on to run during process teardown. A hard
/// crash (SIGKILL/panic) still can't be covered on macOS; that is inherent.
static ACTIVE_GROUPS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

pub(crate) fn register_group(pgid: i32) {
    if let Ok(mut g) = ACTIVE_GROUPS.lock() {
        g.push(pgid);
    }
}

pub(crate) fn deregister_group(pgid: i32) {
    if let Ok(mut g) = ACTIVE_GROUPS.lock() {
        g.retain(|&p| p != pgid);
    }
}

/// SIGKILL every in-flight Code turn's process group. Wired to app-quit so a
/// graceful quit takes the running agents (and everything they spawned) down
/// with Awili instead of orphaning them.
pub(crate) fn kill_all_active_groups() {
    let pgids: Vec<i32> = ACTIVE_GROUPS
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default();
    for pgid in pgids {
        kill_process_group(pgid);
    }
}

/// SIGKILL a process group by its leader pgid. A negative pid passed to `kill(1)`
/// targets the whole group — the CLI plus any subprocess still in that group.
/// (Tool commands claude-code has detached into their own session are NOT in
/// this group and survive; see the spawn-site caveat.) Killing an already-dead
/// group is a harmless no-op
/// (ESRCH). Uses `/bin/kill` to stay dependency-free (no `libc`); spawning it on
/// the rare cancel path is negligible.
///
/// The leader is ALSO killed by its positive pid: the group signal is the wide
/// net, but a sandboxed process supervisor can accept a group `kill(2)` for a
/// same-session group and still deliver nothing (observed 2026-09-14), which
/// would leave the engine running. Every spawn site uses `process_group(0)`
/// (pgid == leader pid), so the positive-pid signal reliably reaches the direct
/// child; the negative-pid signal still covers the in-group subprocesses.
#[cfg(windows)]
pub(crate) fn kill_process_group(pgid: i32) {
    if pgid <= 1 {
        return;
    }
    // Windows: the "group leader" pid is the child; its Job Object ends it and
    // everything it started (a failure is reported, not ignored).
    crate::proc_compat::kill_pid_reporting(pgid as u32);
}

#[cfg(unix)]
pub(crate) fn kill_process_group(pgid: i32) {
    if pgid <= 1 {
        return; // never target pgid 0 (caller's group) or 1 (init)
    }
    // Leader first (positive pid): the one death the reap below depends on.
    let _ = Command::new("/bin/kill")
        .arg("-KILL")
        .arg(pgid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // Then the rest of the group (negative pid targets the whole group).
    let _ = Command::new("/bin/kill")
        .args(group_kill_args(pgid))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// argv for the group signal. The `--` is load-bearing: procps-ng `kill`
/// (4.0.4, Linux) misparses `-KILL -<pgid>` for a group that no longer exists
/// and issues `kill(-1, SIGKILL)` — every process the user owns. That is the
/// normal case here, because the leader was just killed by its positive pid.
/// Observed 2026-09-30 on hosta (three mass kills traced to these tests).
fn group_kill_args(pgid: i32) -> [String; 3] {
    ["-KILL".to_string(), "--".to_string(), format!("-{pgid}")]
}

// Metadata only: never log terminal prose, tool inputs or arbitrary subtype text.
fn trace_claude_terminal(line: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    if value.get("type").and_then(|v| v.as_str()) != Some("result") {
        return;
    }
    let subtype = match value.get("subtype").and_then(|v| v.as_str()) {
        Some("success") => "success",
        Some("error_during_execution") => "error_during_execution",
        Some("error_max_turns") => "error_max_turns",
        Some(_) => "other",
        None => "missing_or_invalid",
    };
    eprintln!(
        "awili: Claude terminal metadata subtype={subtype} is_error={:?} denial_count={}",
        value.get("is_error").and_then(|v| v.as_bool()),
        value
            .get("permission_denials")
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len),
    );
}

/// Decide the terminal events given the process exit code, whether an error was
/// already streamed, and captured stderr. Always ends the turn with exactly one
/// [`ProviderEvent::Done`]; adds a leading [`ProviderEvent::Error`] only when the
/// process failed without the stream already carrying one (so a crash or a
/// non-zero exit surfaces instead of looking like a clean empty turn).
pub(crate) fn finalize(
    exit_code: Option<i32>,
    emitted_error: bool,
    stderr: &str,
) -> Vec<ProviderEvent> {
    let failed = !matches!(exit_code, Some(0));
    if failed && !emitted_error {
        let detail = stderr.trim();
        let msg = if detail.is_empty() {
            match exit_code {
                Some(code) => format!("the coding engine exited with code {code}"),
                None => "the coding engine was terminated".to_string(),
            }
        } else {
            // Keep the tail — the actionable error is usually the last line.
            let tail: String = detail
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            tail
        };
        vec![ProviderEvent::Error(msg), ProviderEvent::Done]
    } else {
        vec![ProviderEvent::Done]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_providers::ChatMessage;

    #[test]
    fn group_kill_separates_options_before_negative_pgid() {
        // Without `--`, procps kill turns `-KILL -<gone pgid>` into kill(-1).
        assert_eq!(group_kill_args(4242), ["-KILL", "--", "-4242"]);
    }

    fn req(system: Option<&str>, msgs: &[ChatMessage]) -> ChatRequest {
        ChatRequest {
            model: "x".into(),
            system: system.map(str::to_string),
            messages: msgs.to_vec(),
        }
    }

    /// Serializes executable-fixture creation/spawn across the Code-provider
    /// tests: on Linux a fork can inherit a sibling test's still-writable
    /// fixture descriptor, and the sibling's later exec of that fixture then
    /// fails with a transient ETXTBSY ("Text file busy"). The same race is
    /// serialized by the `SCRIPT_FIXTURE` locks in `build_dispatch_tests` and
    /// `crates/awili-dispatch/tests/ssh.rs`.
    static SCRIPT_FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The driver's own spawn failure for that race: the first event is the
    /// "could not start" error carrying the ETXTBSY text. Any other first
    /// event is a real engine outcome and must never be retried away.
    fn is_txtbsy_spawn_failure(events: &[ProviderEvent]) -> bool {
        matches!(
            events.first(),
            Some(ProviderEvent::Error(msg)) if msg.contains("Text file busy")
        )
    }

    #[test]
    fn prompt_is_last_user_message_only() {
        let r = req(
            None,
            &[
                ChatMessage::user("first"),
                ChatMessage::assistant("reply"),
                ChatMessage::user("second"),
            ],
        );
        assert_eq!(prompt_from_request(&r), "second");
    }

    #[test]
    fn prompt_prepends_nonempty_system_context() {
        let r = req(Some("rules here"), &[ChatMessage::user("do it")]);
        let p = prompt_from_request(&r);
        assert!(p.contains("rules here"));
        assert!(p.trim_end().ends_with("do it"));
        // An empty system string is not wrapped.
        let r2 = req(Some(""), &[ChatMessage::user("do it")]);
        assert_eq!(prompt_from_request(&r2), "do it");
    }

    #[test]
    fn system_context_uses_exact_labelled_framing_with_blank_line_separator() {
        // The first-turn prompt frames memory directives as
        // `[System context]\n{system}\n\n{user}` — a labelled header, a single
        // newline after it, then a BLANK LINE (`\n\n`) before the user message.
        // That framing is the boundary the coding engine parses to tell standing
        // rules from the new user turn. The existing
        // `prompt_prepends_nonempty_system_context` only asserts `.contains()` +
        // `.ends_with()`, so it pins NEITHER the `[System context]` label NOR the
        // `\n\n` separator: mutating the label wording, or collapsing `\n\n` to a
        // single `\n`, both survive a substring/suffix check. An exact-equality
        // assertion pins the whole composed string at once.
        let r = req(Some("rules here"), &[ChatMessage::user("do it")]);
        assert_eq!(
            prompt_from_request(&r),
            "[System context]\nrules here\n\ndo it"
        );
    }

    #[test]
    fn prompt_is_empty_when_request_has_no_user_message() {
        // The turn prompt is the LAST user message; with no user-role message at
        // all (`.find` returns None) it falls back to the empty string, not to any
        // assistant text. Pins the `unwrap_or("")` fallback: a mutation to a
        // non-empty default, or to picking the assistant content, is caught.
        let r = req(None, &[ChatMessage::assistant("only assistant text")]);
        assert_eq!(prompt_from_request(&r), "");
    }

    #[cfg(unix)]
    /// P10/F3: cancelling a run must actually REAP the child — the diagnostic
    /// flag flips only after the process dies, so `Done` is never reported
    /// ahead of the reap. Serialized with the shared executable-fixture lock
    /// (the same ETXTBSY spawn race the sibling Codex test documents).
    #[test]
    fn diagnostic_observes_actual_reap_after_cancel() {
        use std::os::unix::fs::PermissionsExt;
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("waiting-cli");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 5\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let observed = Arc::new(AtomicBool::new(false));
        let provider = CodeProvider::new(
            script.display().to_string(),
            CodeEngine::Claude,
            Autonomy::Autonomous,
            temp.path().display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        )
        .observe_termination(observed.clone());
        let (cancel_tx, cancel_rx) = std::sync::mpsc::channel();
        let start = std::time::Instant::now();
        let rx = provider.run(req(None, &[ChatMessage::user("fixture")]), cancel_rx);
        cancel_tx.send(()).unwrap();
        let mut error = false;
        while let Ok(event) = rx.recv_blocking() {
            error |= matches!(event, ProviderEvent::Error(_));
            if event == ProviderEvent::Done {
                break;
            }
        }
        assert!(observed.load(Ordering::Acquire));
        assert!(error);
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn codex_completed_message_boundaries_survive_provider_stream() {
        use std::os::unix::fs::PermissionsExt;
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let mut events = Vec::new();
        // A sibling test's fork can inherit this fixture's still-writable
        // descriptor and make the spawn fail with a transient ETXTBSY; re-run
        // the scenario a bounded number of times when (and only when) the
        // driver reports that exact spawn failure.
        for attempt in 0..8 {
            let temp = tempfile::tempdir().unwrap();
            let script = temp.path().join("message-fixture");
            let body = r#"#!/bin/sh
cat <<'EVENTS'
{"type":"item.started","item":{"id":"first","type":"agent_message","text":"I will"}}
{"type":"item.updated","item":{"id":"first","type":"agent_message","text":"I will not modify"}}
{"type":"item.completed","item":{"id":"first","type":"agent_message","text":"I will not modify anything."}}
{"type":"item.completed","item":{"id":"tool","type":"command_execution","command":"read challenge","exit_code":0}}
{"type":"item.completed","item":{"id":"final","type":"agent_message","text":"fixture-nonce"}}
{"type":"item.completed","item":{"id":"final","type":"agent_message","text":"fixture-nonce"}}
{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}
EVENTS
"#;
            std::fs::write(&script, body).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let provider = CodeProvider::new(
                script.display().to_string(),
                CodeEngine::Codex,
                Autonomy::Autonomous,
                temp.path().display().to_string(),
                None,
                None,
                Arc::new(Mutex::new(None)),
            );
            let (_cancel, cancel_rx) = std::sync::mpsc::channel();
            let rx = provider.run(req(None, &[ChatMessage::user("fixture")]), cancel_rx);
            let mut collected = Vec::new();
            while let Ok(event) = rx.recv_blocking() {
                let done = event == ProviderEvent::Done;
                collected.push(event);
                if done {
                    break;
                }
            }
            if attempt < 7 && is_txtbsy_spawn_failure(&collected) {
                std::thread::sleep(std::time::Duration::from_millis(
                    (100 * (attempt + 1)) as u64,
                ));
                continue;
            }
            events = collected;
            break;
        }
        let mut answer = String::new();
        let mut done_count = 0;
        for event in &events {
            match event {
                ProviderEvent::Token(text) => answer.push_str(text),
                ProviderEvent::Error(error) => panic!("fixture failed: {error}"),
                ProviderEvent::Done => done_count += 1,
                _ => {}
            }
        }
        assert_eq!(answer, "I will not modify anything.\n\nfixture-nonce");
        assert_eq!(done_count, 1);
    }

    #[test]
    fn finalize_clean_exit_is_single_done() {
        assert_eq!(finalize(Some(0), false, ""), vec![ProviderEvent::Done]);
    }

    #[cfg(unix)]
    #[test]
    fn recoverable_tool_stream_continues_but_terminal_denials_still_fail() {
        use std::os::unix::fs::PermissionsExt;
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        for (terminal, fatal_terminal, assistant_text, exit_code) in [
            (
                r#"{"type":"result","subtype":"success","is_error":false}"#,
                false,
                true,
                0,
            ),
            (
                r#"{"type":"result","subtype":"success","permission_denials":[{"tool_name":"Bash"}]}"#,
                true,
                true,
                0,
            ),
            (
                r#"{"type":"result","subtype":"success","permission_denials":[{"tool_name":"Bash"}]}"#,
                true,
                false,
                0,
            ),
            (
                r#"{"type":"result","subtype":"success","is_error":false,"permission_denials":[{"tool_name":"Bash"}]}"#,
                false,
                true,
                0,
            ),
            (
                r#"{"type":"result","subtype":"success","is_error":false,"permission_denials":[{"tool_name":"Bash"}]}"#,
                false,
                true,
                2,
            ),
            (
                r#"{"type":"result","subtype":"success","is_error":true,"permission_denials":[{"tool_name":"Bash"}]}"#,
                true,
                true,
                0,
            ),
            (
                r#"{"type":"result","subtype":"error_during_execution","is_error":false,"permission_denials":[{"tool_name":"Bash"}]}"#,
                true,
                true,
                0,
            ),
            ("", false, true, 2),
            ("", false, true, 0),
        ] {
            // A sibling test's fork can inherit this fixture's still-writable
            // descriptor and make the spawn fail with a transient ETXTBSY;
            // re-run the scenario a bounded number of times when (and only
            // when) the driver reports that exact spawn failure.
            let mut events = Vec::new();
            let mut reaped = false;
            for attempt in 0..8 {
                let temp = tempfile::tempdir().unwrap();
                let script = temp.path().join("fixture-cli");
                // A harmless deterministic subprocess fixture, never a live model.
                let body = format!(
                    "#!/bin/sh\ncat <<'EVENTS'\n{}\n{}\n{}\n{}\nEVENTS\nexit {exit_code}\n",
                    r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":true,"content":"private error payload"}]}}"#,
                    r#"{"type":"user","message":{"content":[{"type":"tool_result","is_error":false,"content":"retry succeeded"}]}}"#,
                    if assistant_text {
                        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"response after retry"}]}}"#
                    } else {
                        ""
                    },
                    terminal,
                );
                std::fs::write(&script, body).unwrap();
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
                let provider = CodeProvider::new(
                    script.display().to_string(),
                    CodeEngine::Claude,
                    Autonomy::Autonomous,
                    temp.path().display().to_string(),
                    None,
                    None,
                    Arc::new(Mutex::new(None)),
                );
                let observation = Arc::new(AtomicBool::new(false));
                let provider = provider.observe_termination(observation.clone());
                let (_cancel_tx, cancel_rx) = std::sync::mpsc::channel();
                let rx = provider.run(req(None, &[ChatMessage::user("fixture")]), cancel_rx);
                let mut collected = Vec::new();
                while let Ok(event) = rx.recv_blocking() {
                    let done = event == ProviderEvent::Done;
                    collected.push(event);
                    if done {
                        break;
                    }
                }
                reaped = observation.load(Ordering::Acquire);
                if attempt < 7 && is_txtbsy_spawn_failure(&collected) {
                    std::thread::sleep(std::time::Duration::from_millis(
                        (100 * (attempt + 1)) as u64,
                    ));
                    continue;
                }
                events = collected;
                break;
            }
            assert!(reaped, "Done follows actual process reap");
            assert_eq!(events.first(), Some(&ProviderEvent::ToolAttemptFailed));
            assert_eq!(
                events.contains(&ProviderEvent::Token("response after retry".into())),
                assistant_text
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, ProviderEvent::Error(_)))
                    .count(),
                usize::from(fatal_terminal || exit_code != 0)
            );
            assert_eq!(
                events.iter().filter(|e| **e == ProviderEvent::Done).count(),
                1
            );
            assert_eq!(events.last(), Some(&ProviderEvent::Done));
            assert!(!format!("{events:?}").contains("private error payload"));
        }
    }

    #[test]
    fn finalize_does_not_double_error_when_stream_already_errored() {
        // A `result`/`turn.failed` already produced an Error in-stream; the
        // non-zero exit must NOT add a second one — just the terminal Done.
        assert_eq!(
            finalize(Some(1), true, "stack trace"),
            vec![ProviderEvent::Done]
        );
    }

    #[test]
    fn finalize_synthesises_error_on_silent_failure() {
        let out = finalize(Some(1), false, "");
        assert!(matches!(out.first(), Some(ProviderEvent::Error(m)) if m.contains("code 1")));
        assert_eq!(out.last(), Some(&ProviderEvent::Done));
    }

    #[test]
    fn finalize_uses_stderr_tail_when_present() {
        let out = finalize(
            Some(2),
            false,
            "noise\nmore noise\nfatal: not a git repository",
        );
        match out.first() {
            Some(ProviderEvent::Error(m)) => assert!(m.contains("fatal: not a git repository")),
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn finalize_terminated_without_code() {
        let out = finalize(None, false, "");
        assert!(matches!(out.first(), Some(ProviderEvent::Error(m)) if m.contains("terminated")));
    }

    #[test]
    fn finalize_stderr_tail_keeps_last_four_lines_in_chronological_order() {
        // A crash with a long stderr: the surfaced error is the LAST FOUR lines,
        // kept in original (chronological) order — the module doc's "keep the
        // tail; the actionable error is usually the last line". The existing
        // `finalize_uses_stderr_tail_when_present` test uses only three lines and
        // a `.contains()` assertion, so neither the 4-line CAP nor the
        // order-preservation was pinned: a mutation to `take(1)` (last line only)
        // or dropping the second `.rev()` (tail emitted reversed) both survive a
        // substring check. Here six distinguishable lines with an exact-equality
        // assertion pin both contracts at once.
        let stderr = "l1\nl2\nl3\nl4\nl5\nl6";
        let out = finalize(Some(2), false, stderr);
        match out.first() {
            // Exactly the last four, in order — not "l6" (over-truncation), not
            // "l6\nl5\nl4\nl3" (reversed), not the whole six (no cap).
            Some(ProviderEvent::Error(m)) => assert_eq!(m, "l3\nl4\nl5\nl6"),
            other => panic!("expected error, got {other:?}"),
        }
        assert_eq!(out.last(), Some(&ProviderEvent::Done));

        // Boundary: exactly four lines pass through whole (the cap is inclusive,
        // and short stderr is never padded or dropped).
        let out4 = finalize(Some(2), false, "a\nb\nc\nd");
        assert!(
            matches!(out4.first(), Some(ProviderEvent::Error(m)) if m == "a\nb\nc\nd"),
            "four-line stderr must pass through in order, got {:?}",
            out4.first()
        );
    }

    // ---- dsh (P9) env-file spawn ------------------------------------------
    //
    // A FAKE dsh binary (a small script) records its argv and the env vars Awili
    // injects, then prints a final answer to stdout. This proves the Dsh spawn
    // contract without a real dsh or a network: argv is `--profile headless
    // <prompt>`, the child gets the env file's credentials + telemetry off,
    // `DSH_MODEL` travels only when a model was chosen, and the credential value
    // NEVER appears in the rendered event stream (so it can never reach the
    // transcript or the SQLite rows that persist that transcript).

    /// The fake dsh script: records argv (one per line) and the env vars Awili
    /// injects, prints a fixed final answer to stdout, exits 0. `$root` is the
    /// fixture dir (embedded so the script needs no knowledge of its own path).
    fn fake_dsh_script(root: &std::path::Path) -> String {
        format!(
            "#!/bin/sh\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\"; done > {argv}\n\
             cat > {stdinf}\n\
             {{\n\
             echo \"DEEPSEEK_API_KEY=$DEEPSEEK_API_KEY\"\n\
             echo \"DEEPSEEK_BASE_URL=$DEEPSEEK_BASE_URL\"\n\
             echo \"DSH_MODEL=$DSH_MODEL\"\n\
             echo \"DSH_TELEMETRY_MODE=$DSH_TELEMETRY_MODE\"\n\
             }} > {envf}\n\
             echo \"final answer from dsh\"\n",
            argv = root.join("argv").display(),
            stdinf = root.join("stdin").display(),
            envf = root.join("env").display(),
        )
    }

    /// The fake credential file the app reads itself (P9): a mode-0600
    /// `KEY=VALUE` file, here with a leading `export ` on one line and a
    /// comment + blank line to exercise the parser. The key is the literal test sentinel
    /// (never a real key).
    fn fake_dsh_env_file() -> String {
        fake_dsh_env_file_with_base_url("https://api.example.test/v1")
    }

    fn fake_dsh_env_file_with_base_url(base_url: &str) -> String {
        // Mode-0600 `KEY=VALUE` lines per the P9 facts: an optional leading
        // `export `, comments and blank lines skipped. The key is the literal
        // test sentinel (never a real key). Note the `=` directly follows the
        // key (the spec grammar has no space before `=`).
        format!(
            "# dsh credentials (test fixture — never a real key)\n\
             \n\
             export DEEPSEEK_API_KEY=\"fake-key-for-tests\"\n\
             DEEPSEEK_BASE_URL={base_url}\n"
        )
    }

    /// Run one Dsh turn through the driver against the fake binary. Returns the
    /// collected events, the fixture root, and the `TempDir` guard (kept alive so
    /// the recorded argv/env files survive until the assertions run).
    fn run_fake_dsh(
        model: Option<&str>,
    ) -> (Vec<ProviderEvent>, std::path::PathBuf, tempfile::TempDir) {
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        run_fake_dsh_locked(model)
    }

    fn run_fake_dsh_with_env_file(
        model: Option<&str>,
        env_body: &str,
    ) -> (Vec<ProviderEvent>, std::path::PathBuf, tempfile::TempDir) {
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        run_fake_dsh_locked_with_env_file(model, env_body)
    }

    /// The spawn core of [`run_fake_dsh`], for callers that ALREADY hold
    /// `SCRIPT_FIXTURE`: the `DSH_MODEL` scrub test mutates a process-global env
    /// var and must hold the lock while the child runs so no sibling spawn test
    /// interleaves.
    fn run_fake_dsh_locked(
        model: Option<&str>,
    ) -> (Vec<ProviderEvent>, std::path::PathBuf, tempfile::TempDir) {
        run_fake_dsh_locked_with_env_file(model, &fake_dsh_env_file())
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, dead_code)]
    fn run_fake_dsh_locked_with_env_file(
        model: Option<&str>,
        env_body: &str,
    ) -> (Vec<ProviderEvent>, std::path::PathBuf, tempfile::TempDir) {
        unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
    }
    #[cfg(unix)]
    fn run_fake_dsh_locked_with_env_file(
        model: Option<&str>,
        env_body: &str,
    ) -> (Vec<ProviderEvent>, std::path::PathBuf, tempfile::TempDir) {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let bin = root.join("fake-dsh");
        // RC1-G2: never `fs::write` an executable a test will exec (ETXTBSY).
        crate::exec_fixture::write_exec_fixture(&bin, &fake_dsh_script(&root), 0o700);
        let env_file = root.join("deepseek.conf");
        std::fs::write(&env_file, env_body).unwrap();
        std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o600)).unwrap();

        let provider = CodeProvider::new(
            bin.display().to_string(),
            CodeEngine::Dsh,
            Autonomy::Autonomous,
            root.display().to_string(),
            None,
            model.map(str::to_string),
            Arc::new(Mutex::new(None)),
        )
        .with_dsh_env_file(env_file.display().to_string());
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(req(None, &[ChatMessage::user("do the thing")]), cancel_rx);
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_blocking() {
            let done = event == ProviderEvent::Done;
            events.push(event);
            if done {
                break;
            }
        }
        (events, root, temp)
    }

    /// (a) argv is exactly `--profile headless <prompt>`; (b) the child env
    /// carries the env file's credentials and telemetry-off; (d) the credential
    /// value appears NOWHERE in the rendered event stream.
    #[test]
    fn dsh_spawn_argv_env_and_no_key_in_events() {
        let (events, root, _keep) = run_fake_dsh(Some("deepseek-v4-pro"));
        let argv_file = root.join("argv");
        let env_file = root.join("env");
        // (a) argv: the headless form, "-" for the task, no model/resume.
        let argv = std::fs::read_to_string(&argv_file).unwrap();
        let argv_lines: Vec<&str> = argv.lines().collect();
        assert_eq!(argv_lines, ["--profile", "headless", "-"]);
        // The task travels on stdin, never in argv.
        assert!(!argv.contains("do the thing"), "{argv}");
        let stdin = std::fs::read_to_string(root.join("stdin")).unwrap();
        assert!(stdin.contains("do the thing"), "{stdin}");
        // (b) env: credentials from the file (quotes stripped) + telemetry off.
        let env = std::fs::read_to_string(&env_file).unwrap();
        assert!(env.contains("DEEPSEEK_API_KEY=fake-key-for-tests"), "{env}");
        assert!(
            env.contains("DEEPSEEK_BASE_URL=https://api.example.test/v1"),
            "{env}"
        );
        assert!(env.contains("DSH_TELEMETRY_MODE=DISABLED"), "{env}");
        // (c) a chosen model travels in DSH_MODEL.
        assert!(env.contains("DSH_MODEL=deepseek-v4-pro"), "{env}");
        // The turn succeeded with the fake answer (trimmed, per the whole-output
        // parse), and the key is nowhere in the rendered stream (so it can never
        // reach the transcript or the SQLite rows that persist it).
        assert!(events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Token(t) if t == "final answer from dsh")));
        let rendered = format!("{events:?}");
        assert!(
            !rendered.contains("fake-key-for-tests"),
            "the credential value must never be rendered: {rendered}"
        );
        assert!(!rendered.contains("api.example.test"), "{rendered}");
    }

    /// S1b: a fake `bin` that records its argv and stdin, answers one line.
    fn argv_stdin_recorder(root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let bin = root.join(name);
        let script = format!(
            "#!/bin/sh\ncat > {stdin}\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > {argv}\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\"}}'\n",
            stdin = root.join(format!("{name}.stdin")).display(),
            argv = root.join(format!("{name}.argv")).display(),
        );
        crate::exec_fixture::write_exec_fixture(&bin, &script, 0o700);
        bin
    }

    fn run_recorded(engine: CodeEngine, name: &str, resume: Option<&str>) -> (String, String) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let bin = argv_stdin_recorder(&root, name);
        let provider = CodeProvider::new(
            bin.display().to_string(),
            engine,
            Autonomy::Autonomous,
            root.display().to_string(),
            resume.map(str::to_string),
            None,
            Arc::new(Mutex::new(None)),
        );
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(
            req(
                Some("SECRET-SYSTEM-TEXT"),
                &[
                    ChatMessage::user("EARLIER-HISTORY-TURN"),
                    ChatMessage::assistant("an earlier answer"),
                    ChatMessage::user("THE-QUESTION"),
                ],
            ),
            cancel_rx,
        );
        while let Ok(event) = rx.recv_blocking() {
            if event == ProviderEvent::Done {
                break;
            }
        }
        (
            std::fs::read_to_string(root.join(format!("{name}.argv"))).unwrap(),
            std::fs::read_to_string(root.join(format!("{name}.stdin"))).unwrap_or_default(),
        )
    }

    /// S1b: a Claude text turn (fresh and resumed) carries no prompt, system
    /// text or history in its argv; the stream-json user line on stdin has it.
    #[test]
    fn s1b_claude_text_turn_prompt_is_on_stdin_not_argv() {
        for resume in [None, Some("sess-1")] {
            let (argv, stdin) = run_recorded(CodeEngine::Claude, "claude", resume);
            for secret in ["THE-QUESTION", "EARLIER-HISTORY"] {
                assert!(!argv.contains(secret), "{secret} in argv: {argv}");
            }
            let lines: Vec<&str> = argv.lines().collect();
            let at = lines.iter().position(|a| *a == "--input-format").unwrap();
            assert_eq!(lines[at + 1], "stream-json", "{argv}");
            let prompt = crate::test_stdin::claude_prompt(&stdin).expect("a user line");
            assert!(prompt.contains("THE-QUESTION"), "{stdin}");
        }
    }

    /// S1b: Codex `exec` and dsh take `-` and read the prompt from stdin.
    #[test]
    fn s1b_codex_exec_prompt_is_on_stdin_not_argv() {
        let (argv, stdin) = run_recorded(CodeEngine::Codex, "codex", None);
        for secret in ["THE-QUESTION", "EARLIER-HISTORY"] {
            assert!(!argv.contains(secret), "{secret} in argv: {argv}");
        }
        assert_eq!(argv.lines().last(), Some("-"), "{argv}");
        assert!(stdin.contains("THE-QUESTION"), "{stdin}");
    }

    /// S1b: Claude's brief (`--append-system-prompt`) goes in a private 0600
    /// file named by `--append-system-prompt-file`; the argv holds no brief
    /// text and the file is removed when the turn ends.
    #[test]
    fn s1b_claude_brief_is_a_private_file_removed_after_the_turn() {
        if !crate::test_child::in_own_process(
            "code_provider::tests::s1b_claude_brief_is_a_private_file_removed_after_the_turn",
        ) {
            return;
        }
        let temp_data = tempfile::tempdir().unwrap();
        let _home = HomeGuard::new(temp_data.path());
        let _xdg = XdgDataGuard::new(temp_data.path());
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let bin = root.join("claude");
        let script = format!(
            "#!/bin/sh\ncat > /dev/null\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > {r}/argv\n\
             prev=\"\"\nfor a in \"$@\"; do if [ \"$prev\" = --append-system-prompt-file ]; then \
             cp \"$a\" {r}/brief.copy; (stat -c %a \"$a\" 2>/dev/null || stat -f %Lp \"$a\") > {r}/brief.mode; printf '%s' \"$a\" > {r}/brief.path; fi; prev=\"$a\"; done\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"ok\"}}'\n",
            r = root.display()
        );
        crate::exec_fixture::write_exec_fixture(&bin, &script, 0o700);
        let provider = CodeProvider::new(
            bin.display().to_string(),
            CodeEngine::Claude,
            Autonomy::Autonomous,
            root.display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        );
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(
            req(
                Some("SECRET-BRIEF-TEXT"),
                &[ChatMessage::user("THE-QUESTION")],
            ),
            cancel_rx,
        );
        while let Ok(event) = rx.recv_blocking() {
            if event == ProviderEvent::Done {
                break;
            }
        }
        let argv = std::fs::read_to_string(root.join("argv")).unwrap();
        assert!(argv.contains("--append-system-prompt-file"), "{argv}");
        assert!(
            !argv.lines().any(|a| a == "--append-system-prompt"),
            "{argv}"
        );
        assert!(!argv.contains("SECRET-BRIEF-TEXT"), "{argv}");
        assert!(!argv.contains("THE-QUESTION"), "{argv}");
        let copy = std::fs::read_to_string(root.join("brief.copy")).unwrap();
        assert!(copy.contains("SECRET-BRIEF-TEXT"), "{copy}");
        let mode = std::fs::read_to_string(root.join("brief.mode")).unwrap();
        assert_eq!(mode.trim(), "600");
        let path = std::fs::read_to_string(root.join("brief.path")).unwrap();
        assert!(
            path.starts_with(&temp_data.path().display().to_string())
                || std::fs::canonicalize(temp_data.path())
                    .is_ok_and(|p| path.starts_with(&p.display().to_string())),
            "under the app's own state dir: {path}"
        );
        assert!(
            !std::path::Path::new(&path).exists(),
            "the brief file is removed after the turn: {path}"
        );
    }

    fn dead_pid() -> u32 {
        let mut c = std::process::Command::new("true").spawn().unwrap();
        let pid = c.id();
        c.wait().unwrap();
        pid
    }

    /// S1b review HIGH 1: with no brief file the turn does not run with the
    /// brief on argv; it ends with a plain error and sends nothing.
    #[test]
    fn s1b_a_brief_that_cannot_be_written_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        // A regular file where the brief folder must be.
        let blocker = temp.path().join("brief");
        std::fs::write(&blocker, "x").unwrap();
        let err = write_brief_file_in(&blocker, "SECRET").err().unwrap();
        assert!(err.contains("not a plain folder"), "{err}");
        let msg = brief_error(&err);
        assert!(msg.starts_with("Awili could not prepare this run's instructions: "));
        assert!(msg.ends_with(". Nothing was sent."), "{msg}");
    }

    /// The error path of a real turn: HOME's `Awili/brief` is a plain file.
    #[test]
    fn s1b_a_turn_whose_brief_cannot_be_written_sends_nothing() {
        if !crate::test_child::in_own_process(
            "code_provider::tests::s1b_a_turn_whose_brief_cannot_be_written_sends_nothing",
        ) {
            return;
        }
        let temp_data = tempfile::tempdir().unwrap();
        let _home = HomeGuard::new(temp_data.path());
        let _xdg = XdgDataGuard::new(temp_data.path());
        let dir = dirs::data_dir().unwrap().join(awili_config::APP_DIR_NAME).join("brief");
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        std::fs::write(&dir, "in the way").unwrap();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let bin = argv_stdin_recorder(&root, "claude");
        let provider = CodeProvider::new(
            bin.display().to_string(),
            CodeEngine::Claude,
            Autonomy::Autonomous,
            root.display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        );
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(
            req(Some("SECRET-BRIEF-TEXT"), &[ChatMessage::user("Q")]),
            cancel_rx,
        );
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_blocking() {
            let done = event == ProviderEvent::Done;
            events.push(event);
            if done {
                break;
            }
        }
        assert!(
            events.iter().any(|e| matches!(e, ProviderEvent::Error(m)
                if m.starts_with("Awili could not prepare this run's instructions: ")
                    && m.ends_with(". Nothing was sent."))),
            "{events:?}"
        );
        assert!(!root.join("claude.argv").exists(), "the CLI never started");
    }

    #[cfg(unix)]
    /// S1b review MEDIUM 4: the brief folder must be a real folder of this
    /// user; it is made 0700; a link or file is refused.
    #[test]
    fn s1b_brief_folder_is_checked_and_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("brief");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        prepare_brief_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let fresh = temp.path().join("a").join("brief");
        prepare_brief_dir(&fresh).unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let target = temp.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = prepare_brief_dir(&link).unwrap_err();
        assert!(err.contains("not a plain folder"), "{err}");
        assert!(write_brief_file_in(&link, "SECRET").is_err());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }

    #[cfg(windows)]
    /// Windows counterpart of the 0700/0600 brief checks: the folder and each
    /// brief file are owner-only (protected DACL, user + SYSTEM), a tree
    /// created under the folder is too, and a folder that is a file is refused.
    #[test]
    fn windows_brief_folder_and_file_are_owner_only() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("brief");
        std::fs::create_dir(&dir).unwrap();
        assert!(awili_winacl::verify_owner_only(&dir).is_err(), "starts inherited");
        prepare_brief_dir(&dir).unwrap();
        awili_winacl::verify_owner_only(&dir).expect("existing folder is tightened");
        let fresh = temp.path().join("a").join("brief");
        prepare_brief_dir(&fresh).unwrap();
        awili_winacl::verify_owner_only(&fresh).expect("new folder is created private");
        let file = write_brief_file_in(&dir, "SECRET").unwrap();
        awili_winacl::verify_owner_only(&file.0).expect("brief file is owner-only");
        assert_eq!(std::fs::read_to_string(&file.0).unwrap(), "SECRET");
        let blocker = temp.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let err = prepare_brief_dir(&blocker).unwrap_err();
        assert!(err.contains("not a plain folder"), "{err}");
    }

    #[cfg(unix)]
    /// S1b review MEDIUM 2: leftover `brief-*.md` files of runs that are gone
    /// are removed; other names, links and live runs' files stay.
    #[test]
    fn s1b_startup_sweep_removes_only_dead_runs_brief_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let dead = dead_pid();
        let old = dir.join(format!("brief-{dead}-1-0.md"));
        let live = dir.join(format!("brief-{}-1-0.md", std::process::id()));
        let other = dir.join("notes.md");
        let outside = temp.path().join("outside.txt");
        std::fs::write(&old, "old").unwrap();
        std::fs::write(&live, "live").unwrap();
        std::fs::write(&other, "keep").unwrap();
        std::fs::write(&outside, "keep").unwrap();
        let link = dir.join(format!("brief-{dead}-2-0.md"));
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_eq!(sweep_briefs_in(dir), 1);
        assert!(!old.exists());
        assert!(live.exists() && other.exists() && outside.exists());
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "a link is left alone"
        );
        assert_eq!(sweep_briefs_in(&dir.join("missing")), 0);
    }

    /// S1b review MEDIUM 3: Codex exec (fresh and resumed) and dsh keep the
    /// question AND the system text off argv, except that Codex's stable
    /// system text rides `-c developer_instructions` (Codex has no file key
    /// for it: only `model_instructions_file`, which replaces the base
    /// prompt). That `-c` value is the ONLY place the system text appears;
    /// the prompt is the trailing `-`, also after `resume <id>`.
    #[test]
    fn s1b_codex_exec_system_text_only_in_developer_instructions() {
        for resume in [None, Some("thr-1")] {
            let (argv, stdin) = run_recorded(CodeEngine::Codex, "codex", resume);
            let lines: Vec<&str> = argv.lines().collect();
            let with_system: Vec<&&str> = lines
                .iter()
                .filter(|l| l.contains("SECRET-SYSTEM-TEXT"))
                .collect();
            assert_eq!(with_system.len(), 1, "{argv}");
            assert!(
                with_system[0].starts_with("developer_instructions="),
                "{argv}"
            );
            assert!(!argv.contains("THE-QUESTION"), "{argv}");
            assert_eq!(lines.last(), Some(&"-"), "{argv}");
            assert_eq!(
                lines.contains(&"resume"),
                resume.is_some(),
                "resume form: {argv}"
            );
            assert!(stdin.contains("THE-QUESTION"), "{stdin}");
            assert!(!stdin.contains("SECRET-SYSTEM-TEXT"), "{stdin}");
        }
    }

    /// dsh has no system channel: its system text rides the prompt, on stdin.
    #[test]
    fn s1b_dsh_system_text_is_on_stdin_not_argv() {
        let (argv, stdin) = run_recorded(CodeEngine::Dsh, "dsh", None);
        assert!(!argv.contains("SECRET-SYSTEM-TEXT"), "{argv}");
        assert!(!argv.contains("THE-QUESTION"), "{argv}");
        assert!(stdin.contains("SECRET-SYSTEM-TEXT"), "{stdin}");
        assert!(stdin.contains("THE-QUESTION"), "{stdin}");
    }
    #[test]
    fn s1b_prompt_to_stdin_swaps_only_the_prompt() {
        let mut claude: Vec<String> = ["-p", "--model", "m", "PROMPT"].map(String::from).to_vec();
        prompt_to_stdin(CodeEngine::Claude, &mut claude, 3);
        assert_eq!(
            claude,
            ["-p", "--model", "m", "--input-format", "stream-json"]
        );
        let mut codex: Vec<String> = ["exec", "--json", "PROMPT", "-c", "x"]
            .map(String::from)
            .to_vec();
        prompt_to_stdin(CodeEngine::Codex, &mut codex, 2);
        assert_eq!(codex, ["exec", "--json", "-", "-c", "x"]);
        let mut glm: Vec<String> = ["--prompt", "PROMPT"].map(String::from).to_vec();
        prompt_to_stdin(CodeEngine::Glm, &mut glm, 1);
        assert_eq!(glm, ["--prompt", "PROMPT"], "zcode has no stdin prompt");
        assert!(!stdin_prompt_engine(CodeEngine::Glm));
        assert_eq!(stdin_payload(CodeEngine::Dsh, "task"), "task");
    }

    #[test]
    fn dsh_child_normalizes_the_documented_official_root_only() {
        let (_events, root, _keep) = run_fake_dsh_with_env_file(
            None,
            &fake_dsh_env_file_with_base_url("https://api.deepseek.com/"),
        );
        let env = std::fs::read_to_string(root.join("env")).unwrap();
        assert!(
            env.lines()
                .any(|line| line == "DEEPSEEK_BASE_URL=https://api.deepseek.com/anthropic"),
            "official root must use the Messages endpoint: {env}"
        );

        // Pure child-boundary policy: explicit file values override inherited
        // values, full/correct endpoints pass through, and other harnesses
        // cannot receive the Dsh-specific override.
        assert_eq!(
            dsh_child_base_url_override(CodeEngine::Dsh, &[], Some("https://api.deepseek.com")),
            Some("https://api.deepseek.com/anthropic")
        );
        assert_eq!(
            dsh_child_base_url_override(
                CodeEngine::Dsh,
                &[],
                Some("https://api.deepseek.com/anthropic"),
            ),
            None
        );
        let custom = vec![(
            "DEEPSEEK_BASE_URL".to_string(),
            "https://gateway.example.test/messages".to_string(),
        )];
        assert_eq!(
            dsh_child_base_url_override(CodeEngine::Dsh, &custom, Some("https://api.deepseek.com"),),
            None,
            "file-sourced custom endpoint must beat inherited official root"
        );
        assert_eq!(
            dsh_child_base_url_override(
                CodeEngine::Claude,
                &[(
                    "DEEPSEEK_BASE_URL".to_string(),
                    "https://api.deepseek.com".to_string(),
                )],
                None,
            ),
            None,
            "other harnesses must not receive Dsh endpoint normalization"
        );
    }

    /// Restores `DSH_MODEL` to its pre-test value on drop — including when the
    /// test panics — so the process-global env mutation in
    /// [`dsh_spawn_model_default_sets_no_dsh_model`] can never leak into a
    /// sibling spawn test.
    struct DshModelGuard(Option<String>);

    impl Drop for DshModelGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("DSH_MODEL", v),
                None => std::env::remove_var("DSH_MODEL"),
            }
        }
    }

    /// (c) the picker's "default" (model = None) sets NO `DSH_MODEL` at all — the
    /// headless route's own default stands. The test sets `DSH_MODEL` in the
    /// PARENT process (under the fixture lock, restored via a drop guard) before
    /// the spawn so the spawn's `env_remove` is actually EXERCISED: on a machine
    /// that exports `DSH_MODEL` (a runner, a shell) an inherited value would
    /// otherwise leak into the "default" turn — precisely the leak
    /// `env_remove` guards against. The assertion that the child sees NO
    /// `DSH_MODEL` is kept.
    #[test]
    fn dsh_spawn_model_default_sets_no_dsh_model() {
        // Hold the fixture lock across the env mutation AND the spawn: a
        // process-global `DSH_MODEL` must not be visible to any sibling spawn
        // test while this one runs. The guard restores the parent's environment
        // exactly as found (set or absent) even if an assertion below panics.
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let _restore = DshModelGuard(std::env::var("DSH_MODEL").ok());
        std::env::set_var("DSH_MODEL", "inherited-model-must-not-leak");
        let (events, root, _keep) = run_fake_dsh_locked(None);
        let env = std::fs::read_to_string(root.join("env")).unwrap();
        // The var is present (the fake script echoes it) but EMPTY — the
        // inherited parent value was scrubbed by `env_remove`, so the
        // headless default model stands.
        assert!(
            env.lines().any(|l| l == "DSH_MODEL="),
            "DSH_MODEL must be unset (the inherited value must be scrubbed): {env}"
        );
        // The turn still runs and the key still never renders.
        assert!(events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Token(t) if t.contains("final answer from dsh"))));
        assert!(!format!("{events:?}").contains("fake-key-for-tests"));
    }

    #[cfg(unix)]
    /// A missing/unreadable env file: the driver still SPAWNS (it does not
    /// pre-fail), dsh's own failure surfaces as a typed Error (never silence),
    /// and that Error carries the reason "dsh env file <path> not readable"
    /// PREPENDED (P9). Here the credential-less dsh exits non-zero with empty
    /// stdout, so the driver's own-output error is the surfaced failure.
    #[test]
    fn dsh_spawn_missing_env_file_prepends_reason() {
        use std::os::unix::fs::PermissionsExt;
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let bin = root.join("failing-dsh");
        // A "dsh" that fails (non-zero exit, empty stdout, error on stderr) —
        // as a real dsh would when it has no credentials.
        std::fs::write(
            &bin,
            "#!/bin/sh\necho 'auth: missing DEEPSEEK_API_KEY' >&2\nexit 7\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Point at a NONEXISTENT env file.
        let missing = root.join("does-not-exist.conf");
        let provider = CodeProvider::new(
            bin.display().to_string(),
            CodeEngine::Dsh,
            Autonomy::Autonomous,
            root.display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        )
        .with_dsh_env_file(missing.display().to_string());
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(req(None, &[ChatMessage::user("do the thing")]), cancel_rx);
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_blocking() {
            let done = event == ProviderEvent::Done;
            events.push(event);
            if done {
                break;
            }
        }
        // The error surfaces, with the env-file reason PREPENDED (the path is
        // logged; the credential value is not involved here).
        let err = events
            .iter()
            .find_map(|e| match e {
                ProviderEvent::Error(m) => Some(m.clone()),
                _ => None,
            })
            .expect("a failed dsh turn must surface an error, never silence");
        let expected_prefix = format!("dsh env file {} not readable", missing.display());
        assert!(
            err.starts_with(&expected_prefix),
            "expected prefix {expected_prefix:?}, got {err:?}"
        );
        // The turn is terminal (exactly one Done), and the credential-less
        // failure is reported as a no-output failure, not a clean answer.
        assert!(err.contains("engine produced no output"), "{err}");
        assert_eq!(
            events.iter().filter(|e| **e == ProviderEvent::Done).count(),
            1
        );
        assert!(!events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Token(t) if !t.is_empty())));
    }

    #[cfg(unix)]
    /// No env file configured at all (a fresh install): a failing dsh turn
    /// says "no DeepSeek key file configured" instead of staying silent.
    #[test]
    fn dsh_spawn_without_env_file_says_none_is_configured() {
        use std::os::unix::fs::PermissionsExt;
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let bin = root.join("failing-dsh");
        std::fs::write(
            &bin,
            "#!/bin/sh\necho 'auth: missing DEEPSEEK_API_KEY' >&2\nexit 7\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let provider = CodeProvider::new(
            bin.display().to_string(),
            CodeEngine::Dsh,
            Autonomy::Autonomous,
            root.display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        )
        .with_dsh_env_file(String::new());
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(req(None, &[ChatMessage::user("do the thing")]), cancel_rx);
        let mut err = None;
        while let Ok(event) = rx.recv_blocking() {
            match event {
                ProviderEvent::Error(m) => err = Some(m),
                ProviderEvent::Done => break,
                _ => {}
            }
        }
        let err = err.expect("a failed dsh turn must surface an error");
        assert!(
            err.starts_with("no DeepSeek key file configured"),
            "{err:?}"
        );
    }

    /// The dsh env-file parser (P9): `KEY=VALUE` lines, an optional leading
    /// `export `, comments and blanks skipped, one layer of matching quotes
    /// stripped, and only `[A-Z_][A-Z0-9_]*` keys accepted.
    #[test]
    fn dsh_env_file_parse_rules() {
        let text = "# comment\n\nexport FOO=\"bar\"\n  export\tTWO=tab\nBAZ=plain\nSPACED = nope\nlowercase=nope\nBAD-KEY=x\n=orphan\nQUOTED='q v'\n";
        let parsed = parse_dsh_env(text);
        let m: std::collections::BTreeMap<_, _> = parsed.into_iter().collect();
        assert_eq!(m.get("FOO").map(String::as_str), Some("bar"));
        // `export` followed by a TAB (any whitespace) also strips.
        assert_eq!(m.get("TWO").map(String::as_str), Some("tab"));
        assert_eq!(m.get("BAZ").map(String::as_str), Some("plain"));
        assert_eq!(m.get("QUOTED").map(String::as_str), Some("q v"));
        assert!(
            !m.contains_key("lowercase"),
            "lowercase is not a legal env key: {m:?}"
        );
        assert!(
            !m.contains_key("BAD-KEY"),
            "a dash is not a legal env key: {m:?}"
        );
        // The spec grammar has NO space before `=`: `SPACED = nope` is skipped.
        assert!(
            !m.contains_key("SPACED"),
            "space before = is not legal: {m:?}"
        );
        assert_eq!(m.len(), 4, "only the four legal keys parse: {m:?}");
        // One layer of MATCHING quotes is stripped (both ends same quote char).
        assert_eq!(strip_matching_quotes("'a\"b'"), "a\"b");
        assert_eq!(strip_matching_quotes("\"wrap\""), "wrap");
        // Non-matching or absent quotes are NOT stripped.
        assert_eq!(strip_matching_quotes("\"unbalanced"), "\"unbalanced");
        assert_eq!(strip_matching_quotes("a\"b"), "a\"b");
        assert_eq!(strip_matching_quotes("x'y"), "x'y");
    }

    // ---- P9 live-defect: the child PATH handed to every engine ------------
    //
    // A Finder-launched .app inherits only the system PATH, so a vendor CLI
    // whose shebang is `#!/usr/bin/env <interp>` (the real ~/.local/bin/dsh is
    // `#!/usr/bin/env node`, with node in ~/.local/bin) could not find its
    // interpreter and Work failed instantly with "engine produced no output".
    // These pin the PATH contract: binary dir, then configured search paths
    // (~ expanded), then the parent PATH, de-duplicated.

    /// RC1-G3: in the shared test process `HomeGuard` and `XdgDataGuard`
    /// refuse before touching the environment, so no sibling reads another
    /// test's HOME (RC-16: `build_child_path_orders_expands_and_dedups` below
    /// expanded `~` with a foreign HOME).
    #[test]
    fn rc1g3_env_guards_refuse_a_shared_process() {
        // RC1-G3-FIX1: no early return: this test never runs as an own-process
        // child, so the guards must refuse wherever it runs (an inherited
        // marker included).
        let (home, xdg) = (std::env::var_os("HOME"), std::env::var_os("XDG_DATA_HOME"));
        let dir = tempfile::tempdir().unwrap();
        assert!(
            std::panic::catch_unwind(|| HomeGuard::new(dir.path())).is_err(),
            "HomeGuard must refuse in the shared test process"
        );
        assert!(
            std::panic::catch_unwind(|| XdgDataGuard::new(dir.path())).is_err(),
            "XdgDataGuard must refuse in the shared test process"
        );
        assert_eq!(std::env::var_os("HOME"), home);
        assert_eq!(std::env::var_os("XDG_DATA_HOME"), xdg);
        println!("RC1G3: HomeGuard and XdgDataGuard refused in a shared process");
    }

    /// The pure ordering/expansion/dedup contract of [`build_child_path`].
    #[test]
    fn build_child_path_orders_expands_and_dedups() {
        let home = std::env::var("HOME").unwrap_or_default();
        let expanded = format!("{home}/agent-tools");
        let path = build_child_path(
            "/opt/tools/dsh",
            &[
                "/custom/bin".to_string(),
                "~/agent-tools".to_string(),
                "/custom/bin".to_string(),
            ],
            Some("/usr/bin:/opt/tools:/bin"),
        );
        let entries: Vec<&str> = path.split(':').collect();
        // 1) the resolved binary's directory leads;
        assert_eq!(entries.first().copied(), Some("/opt/tools"));
        // 2) each configured search path follows, `~` expanded, duplicate
        //    entries collapsed to their first occurrence;
        assert!(entries.contains(&"/custom/bin"), "{path}");
        assert_eq!(
            entries.iter().filter(|e| **e == "/custom/bin").count(),
            1,
            "{path}"
        );
        assert!(entries.contains(&expanded.as_str()), "{path}");
        // 3) parent PATH entries come after the configured search paths (and
        //    the binary dir already seen in the parent PATH is not repeated).
        let custom = entries.iter().position(|e| *e == "/custom/bin");
        let usr = entries.iter().position(|e| *e == "/usr/bin");
        assert!(custom < usr, "{entries:?}");
        assert_eq!(entries.iter().filter(|e| **e == "/opt/tools").count(), 1);

        // Absent parent PATH: the system default stands.
        let fallback = build_child_path("/opt/tools/dsh", &[], None);
        assert_eq!(fallback, "/opt/tools:/usr/bin:/bin:/usr/sbin:/sbin");

        // A bare binary name has no directory and contributes none.
        let bare = build_child_path("dsh", &[], None);
        assert_eq!(bare, "/usr/bin:/bin:/usr/sbin:/sbin");
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, dead_code)]
    fn write_path_recorder(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
    }
    #[cfg(unix)]
    /// A fake engine that records the exact `PATH` Awili handed it, then exits
    /// 0 with no stdout (a line-streamed engine's clean silent turn).
    fn write_path_recorder(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let bin_dir = root.join("engine-bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join("path-recorder");
        let path_file = root.join("child-path");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s' \"$PATH\" > {}\n",
                path_file.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        (bin, path_file)
    }

    /// (a) The child sees a PATH whose FIRST entry is the resolved binary's
    /// directory and which contains each configured search path expanded
    /// against HOME, followed by the parent's PATH.
    #[test]
    fn child_path_leads_with_binary_dir_and_includes_expanded_search_paths() {
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let (bin, path_file) = write_path_recorder(&root);

        let abs = root.join("agent-tools");
        std::fs::create_dir_all(&abs).unwrap();
        let tilde_rel = "awili-path-test-tools";
        let home = std::env::var("HOME").unwrap_or_default();
        let expanded = format!("{home}/{tilde_rel}");

        let provider = CodeProvider::new(
            bin.display().to_string(),
            CodeEngine::Claude,
            Autonomy::Autonomous,
            root.display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        )
        .with_search_paths(vec![abs.display().to_string(), format!("~/{tilde_rel}")]);
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(req(None, &[ChatMessage::user("fixture")]), cancel_rx);
        while let Ok(event) = rx.recv_blocking() {
            if event == ProviderEvent::Done {
                break;
            }
        }

        let child_path = std::fs::read_to_string(&path_file).unwrap();
        let entries: Vec<&str> = child_path.split(':').collect();
        assert_eq!(
            entries.first().copied(),
            bin.parent().and_then(std::path::Path::to_str),
            "the binary's own directory must lead the child PATH: {child_path}"
        );
        assert!(entries.contains(&abs.to_str().unwrap()), "{child_path}");
        assert!(entries.contains(&expanded.as_str()), "{child_path}");
        // The parent PATH survives after the configured entries.
        if let Ok(parent) = std::env::var("PATH") {
            for part in parent.split(':').filter(|p| !p.is_empty()) {
                assert!(
                    entries.contains(&part),
                    "parent PATH entry {part:?} must be preserved: {child_path}"
                );
            }
        }
    }

    #[cfg(unix)]
    /// (b) A fake dsh written as `#!/usr/bin/env fakeinterp`, where `fakeinterp`
    /// exists ONLY in a temp dir listed in the configured search paths, runs
    /// successfully through the shared spawn. This is the live defect in
    /// miniature: without the PATH fix `env` cannot find the interpreter and
    /// the spawn yields "engine produced no output".
    #[test]
    fn env_shebang_interpreter_is_found_through_search_path() {
        use std::os::unix::fs::PermissionsExt;
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let interp_dir = root.join("interp-bin");
        let bin_dir = root.join("dsh-bin");
        std::fs::create_dir_all(&interp_dir).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();

        // The interpreter, found only via PATH: re-run the target script
        // through /bin/sh, exactly as `node` would run dsh's JS.
        let interp = interp_dir.join("fakeinterp");
        std::fs::write(&interp, "#!/bin/sh\nexec /bin/sh \"$@\"\n").unwrap();
        std::fs::set_permissions(&interp, std::fs::Permissions::from_mode(0o755)).unwrap();

        // A dsh whose shebang goes through `/usr/bin/env` (like the real one).
        let bin = bin_dir.join("interp-dsh");
        std::fs::write(
            &bin,
            "#!/usr/bin/env fakeinterp\necho \"answer via env shebang\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        // A deliberately absent env file: the turn must still run (the env-file
        // note only ever decorates an error, and this turn succeeds).
        let missing_env = root.join("missing.conf");
        let provider = CodeProvider::new(
            bin.display().to_string(),
            CodeEngine::Dsh,
            Autonomy::Autonomous,
            root.display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        )
        .with_search_paths(vec![interp_dir.display().to_string()])
        .with_dsh_env_file(missing_env.display().to_string());
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(req(None, &[ChatMessage::user("fixture")]), cancel_rx);
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_blocking() {
            let done = event == ProviderEvent::Done;
            events.push(event);
            if done {
                break;
            }
        }
        assert!(
            events.iter().any(
                |e| matches!(e, ProviderEvent::Token(t) if t.contains("answer via env shebang"))
            ),
            "the env-shebang dsh must run through the spawn: {events:?}"
        );
        assert!(
            !events.iter().any(|e| matches!(e, ProviderEvent::Error(_))),
            "no spawn/engine error expected: {events:?}"
        );
    }

    // ---- P12: the vendor CLIs take the connectors server -------------------
    //
    // A FAKE claude/codex binary records its argv (and, for claude, copies the
    // `--mcp-config` file it is handed + records its mode via `stat -c %a`).
    // This proves the P12 contract without a real vendor CLI: claude gets a
    // 0600 `--mcp-config` file naming the resolved server + `--config` (never a
    // secret), codex gets dot-notation `-c mcp_servers.…` overrides, and the
    // claude file is REMOVED after the run (a config never outlives its turn).

    /// Restore `XDG_DATA_HOME` on drop (so `dirs::data_dir()` — where the
    /// `--mcp-config` temp file lands — is deterministic and un-polluted).
    struct XdgDataGuard(Option<std::ffi::OsString>);

    impl XdgDataGuard {
        fn new(dir: &std::path::Path) -> Self {
            crate::test_child::assert_own_process("XDG_DATA_HOME");
            let prev = std::env::var_os("XDG_DATA_HOME");
            std::env::set_var("XDG_DATA_HOME", dir);
            Self(prev)
        }
    }

    impl Drop for XdgDataGuard {
        fn drop(&mut self) {
            match self.0.as_ref() {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }

    /// Pin `$HOME` to the test temp dir and restore it on drop. macOS's
    /// `dirs::data_dir()` ignores `XDG_DATA_HOME` and derives from `$HOME`
    /// (the `dirs-sys` `home_dir`), so without this the production app-data
    /// path (`~/Library/Application Support/Awili/mcp`) is outside the
    /// workspace-write test sandbox. On Linux `XDG_DATA_HOME` already wins, so
    /// this is harmless there.
    struct HomeGuard(Option<std::ffi::OsString>);

    impl HomeGuard {
        fn new(dir: &std::path::Path) -> Self {
            crate::test_child::assert_own_process("HOME");
            let prev = std::env::var_os("HOME");
            std::env::set_var("HOME", dir);
            Self(prev)
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.0.as_ref() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// The `awili-connectors-*` entries currently present in `dir` (empty when
    /// `dir` does not exist). The claude `--mcp-config` proof compares the set
    /// BEFORE and AFTER a run so it does not depend on `XDG_DATA_HOME` being
    /// honoured: macOS's `dirs::data_dir()` ignores it, so a leftover check
    /// against the redirected temp dir would pass vacuously there.
    fn connector_mcp_entries(dir: &std::path::Path) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("awili-connectors-") {
                    out.insert(name);
                }
            }
        }
        out
    }

    /// The fake vendor-CLI script: records argv (one per line); when it sees
    /// `--mcp-config <file>` it also copies that file + records its mode so the
    /// test can assert on the file the child was actually handed.
    fn fake_vendor_script(root: &std::path::Path) -> String {
        format!(
            "#!/bin/sh\n\
             : > {argv}\n\
             prev=\"\"\n\
             for a in \"$@\"; do\n\
             printf '%s\\n' \"$a\" >> {argv}\n\
             if [ \"$prev\" = \"--mcp-config\" ]; then\n\
             cp \"$a\" {copy} 2>/dev/null\n\
             (stat -c %a \"$a\" 2>/dev/null || stat -f %Lp \"$a\") > {mode}\n\
             fi\n\
             prev=\"$a\"\n\
             done\n\
             exit 0\n",
            argv = root.join("argv").display(),
            copy = root.join("mcpconfig.json").display(),
            mode = root.join("mcpconfig.mode").display(),
        )
    }

    /// The resolved server entry the driver is handed (spec.command = a fake
    /// server path, args = `--config <path>`): carries ONLY paths, no secret.
    fn connectors_spec(root: &std::path::Path) -> awili_config::McpServerConfig {
        let server_bin = root.join("awili-connectors-mcp");
        std::fs::write(&server_bin, "#!/bin/sh\nexit 0\n").unwrap();
        let cfg_file = root.join("config.toml");
        std::fs::write(&cfg_file, "[outline]\nenabled = true\n").unwrap();
        awili_config::McpServerConfig {
            name: crate::connectors_mcp::CONNECTORS_SERVER_NAME.to_string(),
            command: server_bin.display().to_string(),
            args: vec!["--config".to_string(), cfg_file.display().to_string()],
            env: Default::default(),
            trust: awili_config::McpTrust::Prompt,
            modes: vec![awili_config::McpMode::Work, awili_config::McpMode::Code],
            enabled: true,
        }
    }

    /// Run one vendor-CLI turn against the fake binary (holds the fixture
    /// lock). Returns the events, the fixture root, and the `TempDir` guard
    /// (kept alive so the recorded argv survives until the assertions run).
    fn run_fake_vendor(
        engine: CodeEngine,
    ) -> (Vec<ProviderEvent>, std::path::PathBuf, tempfile::TempDir) {
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        run_fake_vendor_locked(engine)
    }

    /// The spawn core, for callers that ALREADY hold `SCRIPT_FIXTURE` (the
    /// claude test sets a process-global env var and must hold the lock while
    /// the child runs, so no sibling fixture test interleaves).
    fn run_fake_vendor_locked(
        engine: CodeEngine,
    ) -> (Vec<ProviderEvent>, std::path::PathBuf, tempfile::TempDir) {
        run_fake_vendor_locked_with(engine, Autonomy::Autonomous)
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, dead_code)]
    fn run_fake_vendor_locked_with(
        engine: CodeEngine,
        autonomy: Autonomy,
    ) -> (Vec<ProviderEvent>, std::path::PathBuf, tempfile::TempDir) {
        unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
    }
    #[cfg(unix)]
    /// Same as `run_fake_vendor_locked` but with a caller-chosen autonomy —
    /// U4d D4 needs the read-only planning step's argv.
    fn run_fake_vendor_locked_with(
        engine: CodeEngine,
        autonomy: Autonomy,
    ) -> (Vec<ProviderEvent>, std::path::PathBuf, tempfile::TempDir) {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let bin = root.join("fake-vendor");
        std::fs::write(&bin, fake_vendor_script(&root)).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let spec = connectors_spec(&root);

        let provider = CodeProvider::new(
            bin.display().to_string(),
            engine,
            autonomy,
            root.display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        )
        .with_connectors_mcp(spec);
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(req(None, &[ChatMessage::user("do the thing")]), cancel_rx);
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_blocking() {
            let done = event == ProviderEvent::Done;
            events.push(event);
            if done {
                break;
            }
        }
        (events, root, temp)
    }

    /// claude: argv carries `--mcp-config <file>`; the file was 0600, named the
    /// resolved server + `--config` (no secret), and is REMOVED after the run.
    #[test]
    fn claude_mcp_config_flag_written_0600_and_removed() {
        // RC1-G3: it points the process-wide HOME and XDG_DATA_HOME at a temp
        // dir, so it runs in its own process.
        if !crate::test_child::in_own_process(
            "code_provider::tests::claude_mcp_config_flag_written_0600_and_removed",
        ) {
            return;
        }
        // Hold the fixture lock across the process-global env mutation (same
        // discipline as the DSH_MODEL scrub test) so no sibling fixture test
        // reads `dirs::data_dir()` while it points at our temp dir.
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let temp_data = tempfile::tempdir().unwrap();
        // `XDG_DATA_HOME` covers Linux; `HOME` covers macOS (whose
        // `dirs::data_dir()` ignores XDG). Both point at the same temp dir, so
        // `dirs::data_dir()` resolves there on either host and the production
        // path never touches the real app-data dir.
        let _home = HomeGuard::new(temp_data.path());
        let _xdg = XdgDataGuard::new(temp_data.path());
        // The directory PRODUCTION writes to (`write_claude_mcp_config` uses
        // `dirs::data_dir().join(awili_config::APP_DIR_NAME).join("mcp")`), computed the same way
        // and therefore under `temp_data` on both hosts (XDG on Linux, HOME on
        // macOS). The before/after set comparison below is the removal proof;
        // the exact-path check is belt-and-braces.
        let mcp_dir = dirs::data_dir()
            .expect("a data dir on the coding host")
            .join(awili_config::APP_DIR_NAME)
            .join("mcp");
        let before = connector_mcp_entries(&mcp_dir);
        let (events, root, _keep) = run_fake_vendor_locked(CodeEngine::Claude);
        assert!(
            !events.iter().any(|e| matches!(e, ProviderEvent::Error(_))),
            "no spawn/engine error expected: {events:?}"
        );
        let argv = std::fs::read_to_string(root.join("argv")).unwrap();
        let lines: Vec<&str> = argv.lines().collect();
        // The flag is appended AFTER the engine's own args (prompt last-ish).
        let flag_idx = lines
            .iter()
            .position(|l| *l == "--mcp-config")
            .expect("argv must contain --mcp-config: {lines:?}");
        let file = lines
            .get(flag_idx + 1)
            .copied()
            .expect("a path follows the flag");
        // (a) the child copied the file it was handed (so it existed mid-run).
        let handed = std::fs::read_to_string(root.join("mcpconfig.json"))
            .expect("the child must have been handed a readable file");
        let doc: serde_json::Value = serde_json::from_str(&handed).unwrap();
        let entry = &doc["mcpServers"]["awili-connectors"];
        // The file named the resolved server + the `--config` arg.
        assert_eq!(
            entry["command"].as_str(),
            Some(
                root.join("awili-connectors-mcp")
                    .display()
                    .to_string()
                    .as_str()
            ),
            "the entry must name the resolved server binary"
        );
        assert_eq!(
            entry["args"][0].as_str(),
            Some("--config"),
            "the entry's args must be --config <path>"
        );
        assert!(
            entry["args"][1]
                .as_str()
                .map(|p| p.ends_with("config.toml"))
                .unwrap_or(false),
            "the --config path must point at the config file: {handed}"
        );
        assert!(handed.contains("awili-connectors-mcp"), "{handed}");
        assert!(handed.contains("--config"), "{handed}");
        // (b) it was 0600.
        let mode = std::fs::read_to_string(root.join("mcpconfig.mode")).unwrap();
        assert_eq!(mode.trim(), "600", "the --mcp-config file must be 0600");
        // (c) it is REMOVED after the run (the turn is over; the guard dropped).
        // The exact path the child recorded must be gone …
        assert!(
            !std::path::Path::new(file).exists(),
            "the --mcp-config file must be removed after the run: {file}"
        );
        // … and no NEW `awili-connectors-*` entry may remain in the production
        // dir (platform-aware: on macOS the redirected `temp_data/Awili/mcp`
        // check would pass vacuously).
        let after = connector_mcp_entries(&mcp_dir);
        let new_entries: Vec<&String> = after.difference(&before).collect();
        assert!(
            new_entries.is_empty(),
            "no new --mcp-config file may remain after the run: {new_entries:?}"
        );
    }

    /// RC1-G3-FIX1: `end_turn` removes the `--mcp-config` file before it sends
    /// any terminal event: at every send the file is already gone.
    #[test]
    fn rc1g3_fix1_a_turn_ends_with_its_mcp_config_already_removed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("awili-connectors-turn.json");
        std::fs::write(&file, "{}").unwrap();
        let mut seen = Vec::new();
        end_turn(
            Some(McpConfigGuard(file.display().to_string())),
            vec![
                ProviderEvent::Error("could not start `claude`".into()),
                ProviderEvent::Done,
            ],
            |ev| seen.push((ev, file.exists())),
        );
        assert_eq!(
            seen,
            vec![
                (
                    ProviderEvent::Error("could not start `claude`".into()),
                    false
                ),
                (ProviderEvent::Done, false),
            ],
            "the file must be gone before each terminal event is sent"
        );
    }

    /// RC1-G3-FIX1: both terminal paths of `run_code_stream` after the
    /// `--mcp-config` file is written (the spawn failure and the normal end)
    /// hand that guard to `end_turn`, and none sends `Done` by itself. (The
    /// ordering inside `end_turn` is the test above; a scheduling race cannot
    /// be caught end to end, so the wiring is pinned here.)
    #[test]
    fn rc1g3_fix1_both_terminal_paths_end_through_end_turn() {
        let src = include_str!("code_provider.rs");
        let start = src
            .find("let mcp_config_guard = match &connectors_mcp")
            .expect("the guard is created in run_code_stream");
        let body = &src[start..start + src[start..].find("\n}\n").expect("end of run_code_stream")];
        let flat: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(
            flat.matches("end_turn(mcp_config_guard,").count(),
            3,
            "the brief failure, the spawn failure and the normal end all go through end_turn"
        );
        assert!(
            !flat.contains("send_blocking(ProviderEvent::Done)"),
            "no terminal Done is sent outside end_turn after the file is written"
        );
    }

    /// RC1-G3-FIX1 (Codex T2 P2 on 0b4d934): a Claude CLI that cannot start
    /// still had its `--mcp-config` file written; the turn ends with
    /// Error + Done and no such file remains (the spawn-failure path goes
    /// through `end_turn` too).
    #[test]
    fn rc1g3_fix1_mcp_config_removed_when_the_cli_cannot_start() {
        // It points the process-wide HOME and XDG_DATA_HOME at a temp dir.
        if !crate::test_child::in_own_process(
            "code_provider::tests::rc1g3_fix1_mcp_config_removed_when_the_cli_cannot_start",
        ) {
            return;
        }
        let temp_data = tempfile::tempdir().unwrap();
        let _home = HomeGuard::new(temp_data.path());
        let _xdg = XdgDataGuard::new(temp_data.path());
        let mcp_dir = dirs::data_dir()
            .expect("a data dir on the coding host")
            .join(awili_config::APP_DIR_NAME)
            .join("mcp");
        let before = connector_mcp_entries(&mcp_dir);
        let root = tempfile::tempdir().unwrap();
        let provider = CodeProvider::new(
            root.path().join("no-such-claude").display().to_string(),
            CodeEngine::Claude,
            Autonomy::Autonomous,
            root.path().display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        )
        .with_connectors_mcp(connectors_spec(root.path()));
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(req(None, &[ChatMessage::user("hello")]), cancel_rx);
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_blocking() {
            let done = event == ProviderEvent::Done;
            events.push(event);
            if done {
                // Checked at Done, before the driver thread can return.
                let after = connector_mcp_entries(&mcp_dir);
                let new_entries: Vec<&String> = after.difference(&before).collect();
                assert!(
                    new_entries.is_empty(),
                    "no --mcp-config file may remain at Done: {new_entries:?}"
                );
                break;
            }
        }
        assert!(
            matches!(events.first(), Some(ProviderEvent::Error(m)) if m.contains("could not start")),
            "{events:?}"
        );
        assert_eq!(events.last(), Some(&ProviderEvent::Done));
    }

    /// codex: argv carries the dot-notation `-c mcp_servers.…` overrides (codex
    /// has no config-file flag).
    #[test]
    fn codex_mcp_config_flag_is_c_overrides() {
        let (events, root, _keep) = run_fake_vendor(CodeEngine::Codex);
        assert!(
            !events.iter().any(|e| matches!(e, ProviderEvent::Error(_))),
            "no spawn/engine error expected: {events:?}"
        );
        let argv = std::fs::read_to_string(root.join("argv")).unwrap();
        let lines: Vec<&str> = argv.lines().collect();
        let joined = lines.join(" ");
        assert!(
            joined.contains("-c mcp_servers.awili-connectors.command="),
            "codex must get a -c mcp_servers.awili-connectors.command override: {joined}"
        );
        assert!(
            joined.contains("-c mcp_servers.awili-connectors.args=["),
            "codex must get a -c mcp_servers.awili-connectors.args override: {joined}"
        );
        // The overrides carry the resolved server path + the `--config` arg.
        assert!(joined.contains("awili-connectors-mcp"), "{joined}");
        assert!(joined.contains("--config"), "{joined}");
    }

    /// zcode/dsh have no CLI MCP mechanism: a connectors spec attached to a
    /// non-claude/codex engine adds NO flag (the turn runs plain).
    #[test]
    fn dsh_gets_no_connectors_flag() {
        let (events, root, _keep) = run_fake_vendor(CodeEngine::Dsh);
        let argv = std::fs::read_to_string(root.join("argv")).unwrap();
        assert!(
            !argv.contains("--mcp-config") && !argv.contains("mcp_servers."),
            "dsh has no CLI MCP mechanism; no flag may be added: {argv:?}"
        );
        let _ = events;
    }

    /// U4 D4 / RC1-M: the read-only planning step (AskFirst) keeps the
    /// connector server and every one of its tools; Claude's plan mode is what
    /// refuses the ones that change things, and the server is allowed by name
    /// so its read-only tools run on every model. No tool allowlist, no
    /// denylist. Autonomous (Auto) keeps the server too, with no allow flag.
    #[test]
    fn u4d_plan_step_keeps_connectors_under_plan_mode() {
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let plan = argv_of(CodeEngine::Claude, Autonomy::AskFirst);
        assert!(
            plan.windows(2).any(|w| w == ["--permission-mode", "plan"]),
            "{plan:?}"
        );
        assert!(plan.iter().any(|a| a == "--mcp-config"), "{plan:?}");
        let allowed = plan
            .iter()
            .find(|a| a.starts_with("--allowedTools="))
            .expect("the connector server is allowed by name");
        assert!(
            allowed
                .split_once('=')
                .unwrap()
                .1
                .split(',')
                .any(|n| n == "mcp__awili-connectors"),
            "{allowed}"
        );
        for gone in ["--disallowedTools=", "--tools="] {
            assert!(
                !plan.iter().any(|a| a.starts_with(gone)),
                "{gone}: {plan:?}"
            );
        }
        assert!(!plan.iter().any(|a| a == "--strict-mcp-config"), "{plan:?}");

        let full = argv_of(CodeEngine::Claude, Autonomy::Autonomous);
        assert!(full.iter().any(|a| a == "--mcp-config"), "{full:?}");
        assert!(
            !full
                .iter()
                .any(|a| a.starts_with("--allowedTools=") || a.starts_with("--disallowedTools=")),
            "the full run needs no allow flag: {full:?}"
        );
    }

    fn argv_of(engine: CodeEngine, autonomy: Autonomy) -> Vec<String> {
        let (_events, root, _keep) = run_fake_vendor_locked_with(engine, autonomy);
        std::fs::read_to_string(root.join("argv"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// RC1-M (RC1-L review P1, Codex gpt-6-sol on 1d90a4f: "MCP calls run
    /// outside the read-only sandbox, so a write can bypass the approval
    /// card"): Codex's planning step keeps the connector server and holds it
    /// to "writes" approval — a tool the server marks read-only runs, any
    /// other one is refused (`codex exec` grants no approval). The full run
    /// has no such hold.
    #[test]
    fn rc1m_codex_plan_step_holds_connectors_to_writes() {
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let plan = argv_of(CodeEngine::Codex, Autonomy::AskFirst);
        let joined = plan.join(" ");
        assert!(
            joined.contains("mcp_servers.awili-connectors.command="),
            "{joined}"
        );
        let hold = "mcp_servers.awili-connectors.default_tools_approval_mode=\"writes\"";
        assert!(
            plan.windows(2).any(|w| w[0] == "-c" && w[1] == hold),
            "{plan:?}"
        );
        assert!(!joined.contains("--ignore-user-config"), "{joined}");
        let full = argv_of(CodeEngine::Codex, Autonomy::Autonomous).join(" ");
        assert!(
            full.contains("mcp_servers.awili-connectors.command="),
            "{full}"
        );
        assert!(!full.contains("approval_mode"), "{full}");
    }

    #[test]
    fn m2_fresh_turn_slot_isolated_from_a_late_previous_harness_write() {
        let claude_turn = fresh_session_slot(Some("claude-native-id"));
        let dsh_fresh_turn = fresh_session_slot(None);

        // This is the exact late-provider race: an old child reports its
        // native id after the tab selected a fresh Dsh turn.
        *claude_turn.lock().unwrap() = Some("late-claude-id".into());

        assert_eq!(
            dsh_fresh_turn.lock().unwrap().as_deref(),
            None,
            "a fresh harness must not inherit an old native resume id"
        );
        assert_eq!(
            claude_turn.lock().unwrap().as_deref(),
            Some("late-claude-id"),
            "the old provider still writes only its own retired slot"
        );
    }

    #[test]
    fn m2_resumed_turn_slot_starts_with_its_paired_resume_id() {
        let resumed = fresh_session_slot(Some("codex-native-id"));
        assert_eq!(resumed.lock().unwrap().as_deref(), Some("codex-native-id"));
    }
    #[cfg(unix)]
    #[test]
    fn claude_image_flags_survive_real_launch_settings_and_connectors() {
        use std::os::unix::fs::PermissionsExt;
        if !crate::test_child::in_own_process(
            "code_provider::tests::claude_image_flags_survive_real_launch_settings_and_connectors",
        ) {
            return;
        }
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let root = tempfile::tempdir().unwrap();
        let _home = HomeGuard::new(root.path());
        let _xdg = XdgDataGuard::new(root.path());
        let script = root.path().join("image-cli");
        std::fs::write(&script,r#"#!/usr/bin/env python3
import sys,json,os
args=sys.argv[1:]
i=args.index('--input-format')
assert args[i+1]=='stream-json', args
assert '--settings' in args, args
assert '--append-system-prompt-file' in args, args
assert '--mcp-config' in args, args
assert not any('data:image/' in arg for arg in args)
assert args[args.index('--model')+1]=='haiku'
assert args[args.index('--resume')+1]=='prior-session'
message=json.loads(sys.stdin.readline())
assert message['message']['content'][1]['type']=='image', message
print(json.dumps({'type':'assistant','message':{'content':[{'type':'text','text':'vision input accepted'}]}}))
print(json.dumps({'type':'result','is_error':False,'session_id':'new-session','result':'vision input accepted'}))
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let provider = CodeProvider::new(
            script.display().to_string(),
            CodeEngine::Claude,
            Autonomy::AskFirst,
            root.path().display().to_string(),
            Some("prior-session".into()),
            Some("haiku".into()),
            Arc::new(Mutex::new(None)),
        )
        .with_connectors_mcp(connectors_spec(root.path()));
        let image="![s](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)\nRead this";
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(
            req(Some("Test system instruction"), &[ChatMessage::user(image)]),
            cancel_rx,
        );
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_blocking() {
            let done = event == ProviderEvent::Done;
            events.push(event);
            if done {
                break;
            }
        }
        assert!(
            !events.iter().any(|e| matches!(e, ProviderEvent::Error(_))),
            "{events:?}"
        );
        assert!(
            events.iter().any(
                |e| matches!(e,ProviderEvent::Token(t) if t.contains("vision input accepted"))
            ),
            "{events:?}"
        );
        assert_eq!(
            events.iter().filter(|e| **e == ProviderEvent::Done).count(),
            1
        );
    }
    // ---- rc.4 S7: the opt-in sandbox at the spawn ---------------------------

    /// What a sandbox probe turn saw, read from the files its fake vendor wrote.
    struct SbxRun {
        argv: String,
        env: std::collections::BTreeMap<String, String>,
        /// Exit status of the fake's write outside its working folder.
        outside_rc: String,
        outside_exists: bool,
        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        inside_rc: String,
        events: Vec<ProviderEvent>,
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, dead_code)]
    fn run_sbx_turn(
        engine: CodeEngine,
        autonomy: Autonomy,
        sandbox: awili_config::SandboxConfig,
    ) -> SbxRun {
        unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
    }
    #[cfg(unix)]
    /// Run one turn of `engine` against a fake vendor that tries to write
    /// inside and outside its working folder. Both folders sit under the crate
    /// folder (a temp folder is writable by the profile, so cannot show a
    /// refusal).
    fn run_sbx_turn(
        engine: CodeEngine,
        autonomy: Autonomy,
        sandbox: awili_config::SandboxConfig,
    ) -> SbxRun {
        use std::os::unix::fs::PermissionsExt;
        let _fixture = crate::exec_fixture::lock(&SCRIPT_FIXTURE);
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let record = tempfile::tempdir().unwrap();
        let work = std::fs::canonicalize(root.path()).unwrap().join("work");
        let outside = std::fs::canonicalize(root.path()).unwrap().join("outside");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let rec = record.path();
        let bin = rec.join("fake-vendor");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {rec}/argv\n\
                 env | sort > {rec}/env\n\
                 echo i > {work}/inside 2>/dev/null; echo $? > {rec}/inside_rc\n\
                 echo o > {outside}/out 2>/dev/null; echo $? > {rec}/outside_rc\n\
                 exit 0\n",
                rec = rec.display(),
                work = work.display(),
                outside = outside.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let provider = CodeProvider::new(
            bin.display().to_string(),
            engine,
            autonomy,
            work.display().to_string(),
            None,
            None,
            Arc::new(Mutex::new(None)),
        )
        .with_sandbox(sandbox);
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(req(None, &[ChatMessage::user("go")]), cancel_rx);
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_blocking() {
            let done = event == ProviderEvent::Done;
            events.push(event);
            if done {
                break;
            }
        }
        let read = |n: &str| std::fs::read_to_string(rec.join(n)).unwrap_or_default();
        let env = read("env")
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        SbxRun {
            argv: read("argv"),
            env,
            outside_rc: read("outside_rc").trim().to_string(),
            outside_exists: outside.join("out").exists(),
            inside_rc: read("inside_rc").trim().to_string(),
            events,
        }
    }

    fn sbx_on() -> awili_config::SandboxConfig {
        awili_config::SandboxConfig {
            enabled: true,
            extra_writable: vec![],
        }
    }

    const SBX_ENGINES: [CodeEngine; 4] = [
        CodeEngine::Claude,
        CodeEngine::Codex,
        CodeEngine::Glm,
        CodeEngine::Dsh,
    ];

    /// Variables a launch changes for a reason other than the sandbox: the
    /// child `PATH`, the shell's own bookkeeping, and dsh's own settings.
    const SBX_NOISE: [&str; 9] = [
        "PATH",
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
        "DSH_TELEMETRY_MODE",
        "DSH_MODEL",
        "DEEPSEEK_BASE_URL",
        "AWILI_NATIVE_RESUME_ID",
    ];

    /// Off (the default): every engine is launched as before: the program is
    /// the vendor binary itself, nothing is confined, and the child's whole
    /// environment is the parent's (no variable added, changed or removed
    /// beyond what the launch always did).
    #[test]
    fn s7_sandbox_off_launches_exactly_as_before() {
        for engine in SBX_ENGINES {
            let r = run_sbx_turn(engine, Autonomy::Autonomous, Default::default());
            assert!(
                !r.argv.contains("sandbox-exec") && !r.argv.contains("sandbox-profiles") && !r.argv.starts_with("-f"),
                "{}",
                r.argv
            );
            assert_eq!(r.outside_rc, "0", "{engine:?} writes anywhere when off");
            assert!(r.outside_exists);
            let parent: std::collections::BTreeMap<String, String> = std::env::vars().collect();
            for (k, v) in &r.env {
                if parent.get(k) != Some(v) {
                    assert!(SBX_NOISE.contains(&k.as_str()), "{engine:?}: env {k} appeared/changed");
                }
            }
            for k in parent.keys() {
                if !r.env.contains_key(k) {
                    assert!(SBX_NOISE.contains(&k.as_str()) || k.starts_with("DYLD_"), "{engine:?}: env {k} vanished");
                }
            }
            assert!(
                !r.env.contains_key("DSH_PERMISSION_MODE") || parent.contains_key("DSH_PERMISSION_MODE")
            );
        }
    }

    /// On + Autonomous: the vendor still runs with its original arguments and
    /// environment, but it cannot write outside its folder; a write inside
    /// works. dsh alone is told to stand down its own (nested) sandbox, and
    /// that is the ONLY environment difference. Codex is checked on its
    /// `exec` run (the fake app server fails `initialize`, so the turn falls
    /// back to exec).
    #[cfg(target_os = "macos")] // Seatbelt; unavailable on Linux
    #[test]
    fn s7_sandbox_on_confines_autonomous_turns_for_each_engine() {
        for engine in SBX_ENGINES {
            let off = run_sbx_turn(engine, Autonomy::Autonomous, Default::default());
            let on = run_sbx_turn(engine, Autonomy::Autonomous, sbx_on());
            assert_eq!(on.argv, off.argv, "{engine:?}: the vendor's own argv is untouched");
            if engine == CodeEngine::Codex {
                assert!(on.argv.starts_with("exec"), "Codex exec ran: {}", on.argv);
            }
            assert_eq!(on.inside_rc, "0", "{engine:?}: the working folder is writable");
            assert_ne!(on.outside_rc, "0", "{engine:?}: outside write must be refused");
            assert!(!on.outside_exists, "{engine:?}");
            assert!(
                !on.events.iter().any(|e| matches!(e, ProviderEvent::Error(m) if m.contains("sandbox"))),
                "{:?}",
                on.events
            );
            let volatile = ["_", "SHLVL", "PWD", "OLDPWD"];
            // The binary's own folder leads `PATH` (a fresh one per run).
            let tail = |e: &mut std::collections::BTreeMap<String, String>| {
                for k in volatile {
                    e.remove(k);
                }
                if let Some(p) = e.get_mut("PATH") {
                    *p = p.split_once(':').map_or("", |(_, rest)| rest).to_string();
                }
            };
            let mut want = off.env.clone();
            let mut got = on.env.clone();
            tail(&mut want);
            tail(&mut got);
            if engine == CodeEngine::Dsh {
                want.insert("DSH_PERMISSION_MODE".into(), "danger-full-access".into());
            }
            let differing: Vec<&String> = want
                .keys()
                .chain(got.keys())
                .filter(|k| want.get(*k) != got.get(*k))
                .collect();
            assert!(
                differing.is_empty(),
                "{engine:?}: environment differs from the unsandboxed launch in {differing:?}"
            );
        }
    }

    /// The read-only planning step is never wrapped, even with the setting on.
    #[test]
    fn s7_sandbox_does_not_wrap_the_planning_step() {
        let r = run_sbx_turn(CodeEngine::Claude, Autonomy::AskFirst, sbx_on());
        assert_eq!(r.outside_rc, "0");
        assert!(r.outside_exists);
    }

    /// Fail closed: the turn does not start, with the S3 "sandbox" sentence,
    /// and the vendor is never run unconfined.
    #[test]
    fn s7_sandbox_that_cannot_start_stops_the_turn() {
        let mut cfg = sbx_on();
        cfg.extra_writable = vec!["/tmp/bad\npath".into()];
        let r = run_sbx_turn(CodeEngine::Claude, Autonomy::Autonomous, cfg);
        assert!(r.argv.is_empty(), "the vendor must not run: {}", r.argv);
        assert!(!r.outside_exists);
        assert!(
            r.events.iter().any(|e| matches!(e, ProviderEvent::Error(m) if m.starts_with("Couldn't start the sandbox:"))),
            "{:?}",
            r.events
        );
        assert_eq!(r.events.last(), Some(&ProviderEvent::Done));
    }

}
