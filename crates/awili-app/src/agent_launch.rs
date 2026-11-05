//! RC1-M "AGENTS.md everywhere" (owner, 2026-10-03: "no claude.md just
//! agents.md"; design principle: "be able to do everything i can do in this
//! app, plus some, with any model i want with my full control"). How Awili
//! starts Claude Code and Codex, and the AGENTS.md chain it reads itself for
//! the engines that cannot.
//!
//! - Claude Code and Codex run with the owner's own setup: his settings, MCP
//!   servers, hooks, skills and plugins, his `CODEX_HOME`. Nothing is copied
//!   and nothing private is written (R1-ISO's isolation is gone).
//! - Instructions are AGENTS.md only. Every CLAUDE.md is excluded
//!   (`claudeMdExcludes`); the user-level AGENTS.md
//!   (`[work.agents] agents_md`, default `~/.agents/AGENTS.md`) is appended to
//!   Claude's system prompt and read by Codex from its own home; project
//!   AGENTS.md files are read natively by both. Awili's own text (the reply
//!   brief, the planning-step text) goes through each CLI's system channel —
//!   `--append-system-prompt` (never `--system-prompt`) and Codex
//!   `developer_instructions` — never into the user's message.
//! - "Ask before changes" stays honest for MCP tools without removing any:
//!   Claude's plan mode refuses tools that change things (each MCP server is
//!   allowed by name so its read-only tools run on every model; a write tool
//!   of an allowed server is still refused — live 2026-10-03). Codex runs MCP
//!   tools outside its sandbox, so a planning step holds every MCP server to
//!   "writes" approval: a tool its server marks read-only runs, any other is
//!   refused (live 2026-10-03, codex 0.159).

use std::path::{Path, PathBuf};

use awili_code::{Autonomy, CodeEngine};
use awili_config::AgentMemoryConfig;

/// Codex's `project_doc_max_bytes` default: the cap on the project chain (and,
/// here, on the user file).
pub(crate) const PROJECT_DOC_MAX_BYTES: usize = 32 * 1024;

/// RC2-R4 (R3-F9b, R5-6): the line added to Claude's planning text — the
/// plan goes in the reply, never into Claude Code's plan file.
pub(crate) const CLAUDE_PLAN_IN_REPLY: &str =
    "Write the plan only in your reply: do not save it to \
a plan file or any other file, and do not call ExitPlanMode.";

/// One AGENTS.md file and its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentsMd {
    pub path: PathBuf,
    pub text: String,
}

/// The owner's home and Codex home for a launch.
#[derive(Clone, Debug)]
pub(crate) struct LaunchPaths {
    pub home: PathBuf,
    /// `$CODEX_HOME` of Awili, else `~/.codex` (only read).
    pub codex_home: PathBuf,
}

impl LaunchPaths {
    /// The production paths, or `None` when the OS reports no home.
    pub(crate) fn production() -> Option<Self> {
        #[cfg(test)]
        {
            // Tests never read the real home: a set fixture, else an empty
            // per-process folder.
            let fixture = TEST_PATHS.lock().ok().and_then(|g| g.clone());
            Some(fixture.unwrap_or_else(|| {
                // RC2-APP (K-flake2): under the test process's scratch root.
                let home = crate::test_tmp::process_dir("launch-home");
                Self {
                    codex_home: home.join(".codex"),
                    home,
                }
            }))
        }
        #[cfg(not(test))]
        {
            let home = dirs::home_dir()?;
            let codex_home = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| home.join(".codex"));
            Some(Self { home, codex_home })
        }
    }
}

/// Tests only: production paths override for spawned runs. Process-wide:
/// write it only through [`set_test_paths`].
#[cfg(test)]
static TEST_PATHS: std::sync::Mutex<Option<LaunchPaths>> = std::sync::Mutex::new(None);

/// Tests only: set or clear [`TEST_PATHS`]. Every launch in the process reads
/// it, so (RC1-G3) only a test running in its own process may change it.
#[cfg(test)]
pub(crate) fn set_test_paths(paths: Option<LaunchPaths>) {
    crate::test_child::assert_own_process("agent_launch::TEST_PATHS");
    *TEST_PATHS.lock().unwrap_or_else(|e| e.into_inner()) = paths;
}

fn expand(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if path == "~" => home.to_path_buf(),
        None => PathBuf::from(path),
    }
}

/// RC3-S4: what became of one rules/doc part Awili assembles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DocStatus {
    /// Read whole.
    Included { bytes: usize },
    /// Read only up to the cap: `kept` of `total` bytes.
    Cut { kept: usize, total: usize },
    /// Left out: the shared limit was already used up by earlier files.
    LimitReached,
    /// Its real location (after following links) is outside the project
    /// folder, so it was not read.
    OutsideFolder,
    /// The named CLI reads (or deliberately skips) this itself.
    LeftToCli { cli: &'static str },
    /// It exists but could not be read.
    Unreadable,
}

impl DocStatus {
    /// A part the model did not get whole because of a problem (a part left
    /// to its CLI is by design, not a problem).
    pub(crate) fn is_incomplete(&self) -> bool {
        !matches!(self, Self::Included { .. } | Self::LeftToCli { .. })
    }
}

/// One rules/doc part and its status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocPart {
    /// What the owner calls it: `AGENTS.md`, `docs/AGENTS.md`, `Your AGENTS.md`.
    pub label: String,
    pub status: DocStatus,
}

/// Bytes as the owner reads them: "32 KB" (rounded, at least 1 KB), under
/// 1 KB "N bytes".
fn size_words(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else {
        format!("{} KB", (bytes + 512) / 1024)
    }
}

impl DocPart {
    /// One plain sentence: "AGENTS.md was cut at 32 KB of 51 KB".
    pub(crate) fn sentence(&self) -> String {
        let l = &self.label;
        match &self.status {
            DocStatus::Included { bytes } => format!("{l} was included ({})", size_words(*bytes)),
            DocStatus::Cut { kept, total } => format!(
                "{l} was cut at {} of {}",
                size_words(*kept),
                size_words(*total)
            ),
            DocStatus::LimitReached => format!(
                "{l} was left out: the {} limit was already used by earlier files",
                size_words(PROJECT_DOC_MAX_BYTES)
            ),
            DocStatus::OutsideFolder => {
                format!("{l} points outside this folder, so it was not read")
            }
            DocStatus::LeftToCli { cli } => format!("{l} is left to {cli}"),
            DocStatus::Unreadable => format!("{l} could not be read"),
        }
    }
}

/// The one line for the parts that were not included whole, or `None`.
pub(crate) fn incomplete_summary(parts: &[DocPart]) -> Option<String> {
    let bad: Vec<String> = parts
        .iter()
        .filter(|p| p.status.is_incomplete())
        .map(DocPart::sentence)
        .collect();
    (!bad.is_empty()).then(|| bad.join("; "))
}

/// The result of reading one instruction file.
enum DocRead {
    /// Nothing there (missing, a folder, or blank): not a part.
    Absent,
    Text(String, DocStatus),
    /// There, but not read.
    Skipped(DocStatus),
}

/// One instruction file: a regular file (a symlink is followed, as the CLIs
/// do), not blank, cut to `cap` bytes at a character boundary. With `fence`
/// (a canonical folder) a file whose real path is outside it is not read.
fn read_doc_status(path: &Path, cap: usize, fence: Option<&Path>) -> DocRead {
    use std::io::Read;
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DocRead::Absent,
        Err(_) => return DocRead::Skipped(DocStatus::Unreadable),
    }
    // Open the very path that was checked, not the link, so a swap between
    // the check and the read cannot point the read elsewhere.
    // A link to nothing is unreadable.
    let Ok(real) = std::fs::canonicalize(path) else {
        return DocRead::Skipped(DocStatus::Unreadable);
    };
    if fence.is_some_and(|f| !real.starts_with(f)) {
        return DocRead::Skipped(DocStatus::OutsideFolder);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let Ok(file) = options.open(&real) else {
        return DocRead::Skipped(DocStatus::Unreadable);
    };
    let Ok(meta) = file.metadata() else {
        return DocRead::Skipped(DocStatus::Unreadable);
    };
    if !meta.is_file() {
        return DocRead::Absent;
    }
    let total = meta.len() as usize;
    if cap == 0 {
        return if total > 0 {
            DocRead::Skipped(DocStatus::LimitReached)
        } else {
            DocRead::Absent
        };
    }
    // RC1-M-FIX2 (RC1-M review P2): read at most `cap` bytes — a huge file
    // is never read whole.
    let mut bytes = Vec::with_capacity(cap.min(64 * 1024));
    if file.take(cap as u64).read_to_end(&mut bytes).is_err() {
        return DocRead::Skipped(DocStatus::Unreadable);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let text = text.trim_end_matches('\u{FFFD}').to_string();
    if text.trim().is_empty() {
        return DocRead::Absent;
    }
    let status = if total > cap {
        DocStatus::Cut {
            kept: text.len(),
            total,
        }
    } else {
        DocStatus::Included { bytes: text.len() }
    };
    DocRead::Text(text, status)
}

/// [`read_doc_status`] without a fence, text only.
fn read_doc(path: &Path, cap: usize) -> Option<String> {
    match read_doc_status(path, cap, None) {
        DocRead::Text(t, _) => Some(t),
        _ => None,
    }
}

/// The user-level AGENTS.md: `configured` (`[work.agents] agents_md`), else
/// [`awili_config::DEFAULT_USER_AGENTS_MD`]; an empty setting turns it off.
pub(crate) fn user_agents_md(configured: Option<&str>, home: &Path) -> Option<AgentsMd> {
    user_agents_md_report(configured, home).0
}

/// [`user_agents_md`] and its part. The owner's own file is his choice, so it
/// is not fenced to a project folder.
pub(crate) fn user_agents_md_report(
    configured: Option<&str>,
    home: &Path,
) -> (Option<AgentsMd>, Option<DocPart>) {
    let raw = configured
        .unwrap_or(awili_config::DEFAULT_USER_AGENTS_MD)
        .trim();
    if raw.is_empty() {
        return (None, None);
    }
    let path = expand(raw, home);
    let label = "Your AGENTS.md".to_string();
    match read_doc_status(&path, PROJECT_DOC_MAX_BYTES, None) {
        DocRead::Absent => (None, None),
        DocRead::Skipped(status) => (None, Some(DocPart { label, status })),
        DocRead::Text(text, status) => (
            Some(AgentsMd { path, text }),
            Some(DocPart { label, status }),
        ),
    }
}

/// The project's AGENTS.md chain with Codex's semantics: from the project
/// root (the nearest directory at or above `folder` holding `.git`; none =
/// `folder` alone) down to `folder`, at most one file per directory
/// (`AGENTS.override.md` when not blank, else `AGENTS.md`), root first, all
/// together cut at [`PROJECT_DOC_MAX_BYTES`].
pub(crate) fn project_chain(folder: &Path) -> Vec<AgentsMd> {
    project_chain_report(folder).0
}

/// The canonical project root and the directories from it down to `folder`.
fn chain_dirs(folder: &Path) -> (PathBuf, Vec<PathBuf>) {
    let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    let root = folder
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(&folder)
        .to_path_buf();
    let mut dirs: Vec<PathBuf> = folder
        .ancestors()
        .take_while(|d| d.starts_with(&root))
        .map(Path::to_path_buf)
        .collect();
    dirs.reverse();
    (root, dirs)
}

/// [`project_chain`] and a part for every file found. RC3-S4: a file whose
/// real path is outside the project root (a link out) is not read; a file
/// cut at the shared cap, or left out because the cap was already used up,
/// says so. An unread override file does not stop `AGENTS.md` being tried.
pub(crate) fn project_chain_report(folder: &Path) -> (Vec<AgentsMd>, Vec<DocPart>) {
    let (root, dirs) = chain_dirs(folder);
    let mut left = PROJECT_DOC_MAX_BYTES;
    let mut out = Vec::new();
    let mut parts = Vec::new();
    for dir in dirs {
        for name in ["AGENTS.override.md", "AGENTS.md"] {
            let path = dir.join(name);
            let label = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .display()
                .to_string();
            match read_doc_status(&path, left, Some(&root)) {
                DocRead::Absent => continue,
                DocRead::Skipped(status) => {
                    let cut = matches!(status, DocStatus::Cut { .. });
                    parts.push(DocPart { label, status });
                    if cut {
                        break;
                    }
                }
                DocRead::Text(text, status) => {
                    left = left.saturating_sub(text.len());
                    parts.push(DocPart { label, status });
                    out.push(AgentsMd { path, text });
                    break;
                }
            }
        }
    }
    (out, parts)
}

/// RC3-S4: every rules/doc part Awili assembles (or deliberately leaves) for
/// a run of `engine` (`None` = the in-app engine or a gateway) in `folder`:
/// the owner's AGENTS.md and the project chain. CLAUDE.md files are not
/// parts: Awili reads AGENTS.md only, and Claude Code is told to skip them too.
pub(crate) fn context_parts(
    paths: &LaunchPaths,
    engine: Option<CodeEngine>,
    folder: Option<&Path>,
    agents_md: Option<&str>,
) -> Vec<DocPart> {
    let mut parts = Vec::new();
    let (user_doc, user_part) = user_agents_md_report(agents_md, &paths.home);
    if let Some(mut part) = user_part {
        let cli = user_doc
            .as_ref()
            .zip(engine.zip(folder))
            .and_then(|(doc, (e, cwd))| match e {
                CodeEngine::Claude => {
                    claude_reads_natively(&doc.path, cwd).then_some("Claude Code")
                }
                CodeEngine::Codex => {
                    codex_reads_natively(&doc.path, &paths.codex_home, cwd).then_some("Codex")
                }
                CodeEngine::Glm => {
                    zcode_reads_natively(&doc.path, &paths.home, cwd).then_some("ZCode")
                }
                _ => None,
            });
        if let Some(cli) = cli {
            part.status = DocStatus::LeftToCli { cli };
        }
        parts.push(part);
    }
    if let Some(folder) = folder {
        parts.extend(project_chain_report(folder).1);
    }
    parts
}

fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `docs` as one block for an engine that does not read AGENTS.md itself
/// (the in-app engine's system prompt; the task text of the lanes with no
/// system channel). A file that is the same file as an earlier one is left
/// out. `None` when there is nothing.
pub(crate) fn instructions_block(docs: &[AgentsMd]) -> Option<String> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for d in docs {
        let r = real(&d.path);
        if seen.contains(&r) {
            continue;
        }
        seen.push(r);
        parts.push(format!(
            "Instructions from {}:\n\n{}",
            d.path.display(),
            d.text.trim_end()
        ));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Every CLAUDE.md Claude Code would load — the user's, the project's (at
/// any depth), the local ones and the rules folders — as `claudeMdExcludes`
/// globs. An excluded CLAUDE.md no longer hides the folder's AGENTS.md (live,
/// claude 2.1.282, research 2026-10-03, Test 1).
pub(crate) fn claude_md_excludes(home: &Path) -> Vec<String> {
    let mut v = vec![
        "**/CLAUDE.md".to_string(),
        "**/CLAUDE.local.md".to_string(),
        "**/.claude/rules/**".to_string(),
    ];
    for rel in [".claude/CLAUDE.md", ".claude/rules/**"] {
        v.push(home.join(rel).display().to_string());
    }
    v
}

/// The `--settings` value (inline JSON, layered on the owner's own settings,
/// which stay in force): only the CLAUDE.md excludes.
pub(crate) fn claude_settings_arg(home: &Path) -> String {
    serde_json::json!({ "claudeMdExcludes": claude_md_excludes(home) }).to_string()
}

/// RC2-R4M4 (slice review P2; live 2026-10-08, claude 2.1.295): Claude Code's
/// plan mode tells the model to write its plan to `~/.claude/plans/<slug>.md`
/// (outside the Work folder, before any approval). [`CLAUDE_PLAN_IN_REPLY`]
/// only asks it not to; these rules make the CLI refuse it. They name only
/// that folder (`~` is the CLI's own home): every tool stays, and the
/// `plansDirectory` setting cannot help (the CLI ignores an absolute path
/// outside the project and would put a relative one in the owner's folder).
pub(crate) const CLAUDE_PLAN_FILE_DENY: [&str; 2] =
    ["Edit(~/.claude/plans/**)", "Write(~/.claude/plans/**)"];

/// The `--settings` value for a Claude planning step: the excludes, and
/// [`CLAUDE_PLAN_FILE_DENY`] (the owner's own rules stay in force; settings
/// layers merge their permission rules).
pub(crate) fn claude_planning_settings_arg(home: &Path) -> String {
    serde_json::json!({
        "claudeMdExcludes": claude_md_excludes(home),
        "permissions": { "deny": CLAUDE_PLAN_FILE_DENY },
    })
    .to_string()
}

/// Claude Code reads `AGENTS.md` and `.claude/AGENTS.md` in the working
/// folder and every folder above it: `user` is one of those files.
fn claude_reads_natively(user: &Path, cwd: &Path) -> bool {
    let user = real(user);
    real(cwd).ancestors().any(|d| {
        [d.join("AGENTS.md"), d.join(".claude").join("AGENTS.md")]
            .iter()
            .any(|f| f.is_file() && real(f) == user)
    })
}

/// Codex reads `$CODEX_HOME/AGENTS.override.md` (when not blank) else
/// `$CODEX_HOME/AGENTS.md`, and the project chain: `user` is one of those.
fn codex_reads_natively(user: &Path, codex_home: &Path, cwd: &Path) -> bool {
    let user = real(user);
    let home_doc = ["AGENTS.override.md", "AGENTS.md"]
        .iter()
        .map(|n| codex_home.join(n))
        .find(|p| read_doc(p, PROJECT_DOC_MAX_BYTES).is_some());
    home_doc.is_some_and(|p| real(&p) == user)
        || project_chain(cwd).iter().any(|d| real(&d.path) == user)
}

/// The MCP server names Claude Code has for a launch in `cwd`, read (never
/// written) from the owner's own configuration: the user scope (the root
/// `mcpServers` of `~/.claude.json`), the local scope (its `projects` entry
/// for `cwd` or a folder above it) and the project scope (`.mcp.json` in
/// `cwd` or a folder above it). Sorted, no repeats.
pub(crate) fn claude_mcp_server_names(home: &Path, cwd: &Path) -> Vec<String> {
    fn keys(v: Option<&serde_json::Value>, out: &mut Vec<String>) {
        if let Some(obj) = v.and_then(|v| v.as_object()) {
            out.extend(obj.keys().cloned());
        }
    }
    let read = |p: &Path| -> Option<serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
    };
    let mut names = Vec::new();
    let cwd = real(cwd);
    if let Some(doc) = read(&home.join(".claude.json")) {
        keys(doc.get("mcpServers"), &mut names);
        if let Some(projects) = doc.get("projects").and_then(|p| p.as_object()) {
            for dir in cwd.ancestors() {
                let key = dir.display().to_string();
                keys(
                    projects.get(&key).and_then(|p| p.get("mcpServers")),
                    &mut names,
                );
            }
        }
    }
    for dir in cwd.ancestors() {
        if let Some(doc) = read(&dir.join(".mcp.json")) {
            keys(doc.get("mcpServers"), &mut names);
        }
    }
    names.sort();
    names.dedup();
    names
}

/// RC1-M-FIX1 (owner, 2026-10-03: "any model … full capability"): the
/// owner's own MCP servers for Awili's in-app engine, which has no config of
/// its own to read — the stdio servers of his Claude Code user scope (the root
/// `mcpServers` of `~/.claude.json`, read, never written; `~` expanded), plus
/// his memory server from `[work.agents.memory]` when that file names no
/// `interlock`. Servers the in-app engine cannot start are left out: a URL
/// server (no stdio transport) and a relative or missing program. Each is an
/// ordinary server (`trust = "prompt"`): its read-only tools run without a
/// card, everything else asks in "Ask before changes".
pub(crate) fn owner_mcp_servers(
    home: &Path,
    mem: &AgentMemoryConfig,
) -> Vec<awili_config::McpServerConfig> {
    let entry = |name: &str, command: &str, args: Vec<String>, env: Vec<(String, String)>| {
        let command = expand(command.trim(), home);
        (command.is_absolute() && command.is_file()).then(|| awili_config::McpServerConfig {
            name: name.to_string(),
            command: command.display().to_string(),
            args,
            env: env
                .into_iter()
                .map(|(k, v)| (k, expand(&v, home).display().to_string()))
                .collect(),
            trust: awili_config::McpTrust::Prompt,
            modes: vec![awili_config::McpMode::Work, awili_config::McpMode::Code],
            enabled: true,
        })
    };
    let mut out = Vec::new();
    let doc: Option<serde_json::Value> = std::fs::read_to_string(home.join(".claude.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let servers = doc
        .as_ref()
        .and_then(|d| d.get("mcpServers"))
        .and_then(|s| s.as_object());
    for (name, server) in servers.into_iter().flatten() {
        let stdio = server
            .get("type")
            .and_then(|t| t.as_str())
            .is_none_or(|t| t == "stdio");
        let Some(command) = server
            .get("command")
            .and_then(|c| c.as_str())
            .filter(|_| stdio)
        else {
            continue;
        };
        let args = server
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let env = server
            .get("env")
            .and_then(|e| e.as_object())
            .map(|e| {
                e.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        out.extend(entry(name, command, args, env));
    }
    if !out.iter().any(|s| s.name == OWNER_CHAT_MEMORY_SERVER) {
        if let Some(cmd) = mem.interlock_command.as_deref() {
            let env = mem
                .interlock_env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            out.extend(entry(OWNER_CHAT_MEMORY_SERVER, cmd, Vec::new(), env));
        }
    }
    out
}

/// One `--allowedTools=` element naming whole MCP servers (`mcp__<name>`):
/// in plan mode it lets their read-only tools run without a prompt; plan
/// mode still refuses their tools that change things.
pub(crate) fn claude_server_allow_arg(names: &[String]) -> Option<String> {
    (!names.is_empty()).then(|| {
        format!(
            "--allowedTools={}",
            names
                .iter()
                .map(|n| format!("mcp__{n}"))
                .collect::<Vec<_>>()
                .join(",")
        )
    })
}

/// A TOML basic string (every character Codex's `-c` parser needs escaped).
pub(crate) fn toml_basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The Codex config files that can name MCP servers for a run in `cwd`: the
/// owner's `$CODEX_HOME/config.toml` and every `.codex/config.toml` from
/// `cwd` up (a trusted project's layer). Only read.
fn codex_config_layers(codex_home: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut files = vec![codex_home.join("config.toml")];
    for dir in real(cwd).ancestors() {
        let f = dir.join(".codex").join("config.toml");
        if f.is_file() && real(&f) != real(&files[0]) {
            files.push(f);
        }
    }
    files
}

/// What one launch is.
pub(crate) struct LaunchSpec<'a> {
    pub engine: CodeEngine,
    pub autonomy: Autonomy,
    pub cwd: &'a Path,
    /// Awili's own system text for this turn (planning step, memory policy
    /// where kept, reply brief).
    pub system: Option<&'a str>,
    pub mem: Option<&'a AgentMemoryConfig>,
    /// `[work.agents] agents_md`.
    pub agents_md: Option<&'a str>,
    /// The name of the connector MCP server Awili adds, when it does.
    pub connectors: Option<&'a str>,
}

/// What one launch adds to the CLI's own arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Launch {
    /// Flags inserted before the trailing prompt.
    pub args: Vec<String>,
    /// Text that leads the prompt as `[System context]` — only for the
    /// engines with no system channel (GLM, DeepSeek).
    pub prompt_system: Option<String>,
}

/// The owner's memory server name (the owner's own name for it), used when
/// `[work.agents.memory]` names a program but his Claude settings do not list it.
pub(crate) const OWNER_CHAT_MEMORY_SERVER: &str = "interlock";

fn join_texts(parts: &[Option<&str>]) -> Option<String> {
    let kept: Vec<&str> = parts
        .iter()
        .flatten()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    (!kept.is_empty()).then(|| kept.join("\n\n"))
}

/// The launch for `spec` (pure over the filesystem it reads: the AGENTS.md
/// files and the owner's MCP configuration).
pub(crate) fn launch(paths: &LaunchPaths, spec: &LaunchSpec) -> Launch {
    let mut out = Launch::default();
    let user = user_agents_md(spec.agents_md, &paths.home);
    match spec.engine {
        CodeEngine::Claude => {
            out.args.push("--settings".into());
            // RC2-R4M4: a Work planning step cannot write a plan file.
            out.args.push(if spec.autonomy.plan_first() {
                claude_planning_settings_arg(&paths.home)
            } else {
                claude_settings_arg(&paths.home)
            });
            let user_text = user
                .as_ref()
                .filter(|u| !claude_reads_natively(&u.path, spec.cwd))
                .map(|u| u.text.as_str());
            // RC1-M-FIX6 (live 2026-10-03, claude 2.1.282, scripted model: the
            // approved step's `--resume` request carried the planning step's
            // system text byte for byte — "You are in a read-only planning
            // step" — though Awili appended none): Claude keeps a session's
            // appended system prompt across `--resume`, as Codex keeps its
            // developer instructions (RC1-M-FIX2), so the planning text goes
            // in THIS turn's prompt, where it ends with the turn.
            let (system, planning) = split_planning_text(spec.system);
            // RC2-R4 (R3-F9b, R5-6): Claude Code's plan mode saves the plan
            // to a file under `~/.claude/plans/` first (outside the Work
            // folder, before any approval) and only then answers: a second
            // model call that re-reads the whole context (the round-3
            // one-line plan: 48,822 + 49,400 input tokens, transcript
            // 5ed5fe46). Awili shows the plan from the reply, so Claude is
            // told to give it there only. No tool is taken away. RC2-R4M4: the
            // launch's settings also refuse a write there
            // ([`CLAUDE_PLAN_FILE_DENY`]); this line saves the refused call.
            out.prompt_system = match planning {
                Some(p) if spec.autonomy.plan_first() => {
                    Some(format!("{p}\n{CLAUDE_PLAN_IN_REPLY}"))
                }
                other => other,
            };
            if let Some(text) = join_texts(&[user_text, system.as_deref()]) {
                out.args.push("--append-system-prompt".into());
                out.args.push(text);
            }
            if spec.autonomy.plan_first() {
                let mut names = claude_mcp_server_names(&paths.home, spec.cwd);
                if let Some(c) = spec.connectors {
                    names.push(c.to_string());
                }
                names.sort();
                names.dedup();
                if let Some(a) = claude_server_allow_arg(&names) {
                    out.args.push(a);
                }
            }
        }
        CodeEngine::Codex => {
            let user_text = user
                .as_ref()
                .filter(|u| !codex_reads_natively(&u.path, &paths.codex_home, spec.cwd))
                .map(|u| u.text.as_str());
            // RC1-M-FIX2 (live 2026-10-03: after Approve on the plan card the
            // resumed Codex session still answered "this turn is still
            // restricted to read-only planning"): Codex keeps a session's
            // developer instructions across `exec resume`, so the planning
            // step's own text goes in THIS turn's prompt, where it ends with
            // the turn; the stable text (AGENTS.md, brief) stays in
            // `developer_instructions`.
            let (system, planning) = split_planning_text(spec.system);
            out.prompt_system = planning;
            if let Some(text) = join_texts(&[user_text, system.as_deref()]) {
                out.args.push("-c".into());
                out.args.push(format!(
                    "developer_instructions={}",
                    toml_basic_string(&text)
                ));
            }
            if let Some(m) = spec.mem {
                for (key, value) in [
                    ("model", m.codex_model.as_deref()),
                    (
                        "model_reasoning_effort",
                        m.codex_reasoning_effort.as_deref(),
                    ),
                ] {
                    if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
                        out.args.push("-c".into());
                        out.args.push(format!("{key}={}", toml_basic_string(v)));
                    }
                }
            }
            if spec.autonomy.plan_first() {
                let extra: Vec<&str> = spec.connectors.into_iter().collect();
                let mut overrides = Vec::new();
                for file in codex_config_layers(&paths.codex_home, spec.cwd) {
                    let text = std::fs::read_to_string(&file).unwrap_or_default();
                    for o in awili_config::codex_plan::approval_overrides(&text, &extra) {
                        if !overrides.contains(&o) {
                            overrides.push(o);
                        }
                    }
                }
                for o in overrides {
                    out.args.push("-c".into());
                    out.args.push(o);
                }
            }
        }
        CodeEngine::Glm | CodeEngine::Dsh => {
            // RC1-M-FIX8 (RC1-M whole-slice review P1): these CLIs take Awili's
            // text only as prompt text, so they get the AGENTS.md files there
            // — the bounded user file and, for a Work or Code turn, the
            // project chain — minus what the CLI reads itself (ZCode reads
            // its own user file and one workspace file; the DeepSeek CLI
            // reads none).
            let native = |d: &AgentsMd| {
                spec.engine == CodeEngine::Glm
                    && zcode_reads_natively(&d.path, &paths.home, spec.cwd)
            };
            let docs: Vec<AgentsMd> = user
                .into_iter()
                .chain(project_chain(spec.cwd))
                .filter(|d| !native(d))
                .collect();
            out.prompt_system = join_texts(&[instructions_block(&docs).as_deref(), spec.system]);
        }
    }
    out
}

/// RC1-M-FIX8: whether ZCode (the GLM CLI) reads `path` itself — its user
/// file `$HOME/.zcode/AGENTS.md`, or the ONE workspace file it picks: the
/// nearest `AGENTS.md` walking up from `cwd` to the project root (it reads
/// no `AGENTS.override.md` and no further ancestor). Read from the ZCode
/// bundle (`resolveUserInstructions`, 2026-10-04).
fn zcode_reads_natively(path: &Path, home: &Path, cwd: &Path) -> bool {
    let p = real(path);
    let user = home.join(".zcode").join("AGENTS.md");
    (user.is_file() && real(&user) == p) || zcode_workspace_file(cwd).is_some_and(|w| real(&w) == p)
}

/// The workspace AGENTS.md ZCode picks for `cwd` (see [`zcode_reads_natively`]).
fn zcode_workspace_file(cwd: &Path) -> Option<PathBuf> {
    let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let root = cwd
        .ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf);
    for dir in cwd.ancestors() {
        let file = dir.join("AGENTS.md");
        if file.is_file() {
            return Some(file);
        }
        if root.as_deref() == Some(dir) {
            break;
        }
    }
    None
}

/// RC1-M: the engines Awili hands its own text through a system channel
/// (`--append-system-prompt`, `developer_instructions`) — every turn, never
/// into the conversation.
pub(crate) fn has_system_channel(engine: CodeEngine) -> bool {
    matches!(engine, CodeEngine::Claude | CodeEngine::Codex)
}

/// RC1-M-FIX2: `system` without the planning-step text
/// ([`awili_code::PLAN_FIRST_SYSTEM`]), and that text when it was there.
fn split_planning_text(system: Option<&str>) -> (Option<String>, Option<String>) {
    let plan = awili_code::PLAN_FIRST_SYSTEM;
    match system {
        Some(s) if s.contains(plan) => {
            let rest = s.replacen(plan, "", 1);
            let rest = rest.trim();
            (
                (!rest.is_empty()).then(|| rest.to_string()),
                Some(plan.to_string()),
            )
        }
        other => (other.map(str::to_string), None),
    }
}

/// Insert a launch's flags before the trailing prompt. The variadic
/// `--mcp-config` must be followed by another flag, never the prompt:
/// [`launch`] always puts one after it.
pub(crate) fn apply_args(mut args: Vec<String>, extra: &[String]) -> Vec<String> {
    if extra.is_empty() {
        return args;
    }
    let at = args.len().saturating_sub(1);
    args.splice(at..at, extra.iter().cloned());
    args
}

#[cfg(test)]
#[path = "agent_launch_tests.rs"]
mod tests;

#[cfg(test)]
mod rc2r4_tests {
    use super::*;

    /// RC2-R4 (R3-F9b, R5-6): a Claude planning step is told to give its
    /// plan in the reply only — no plan file under `~/.claude/plans/`, which
    /// cost the round-3 one-line plan a second full model call — and its
    /// tools are untouched. Codex (no plan file) and the approved follow-up
    /// (no planning text) are unchanged.
    #[test]
    fn rc2r4_claude_plans_in_its_reply_not_in_a_plan_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = tmp.path().join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let paths = LaunchPaths {
            home: home.clone(),
            codex_home: home.join(".codex"),
        };
        let system = format!("{}\n\nBRIEF-TEXT", awili_code::PLAN_FIRST_SYSTEM);
        let spec = |engine, autonomy| LaunchSpec {
            engine,
            autonomy,
            cwd: &cwd,
            system: Some(&system),
            mem: None,
            agents_md: None,
            connectors: None,
        };
        let plan = launch(&paths, &spec(CodeEngine::Claude, Autonomy::AskFirst));
        let prompt =
            crate::code_provider::compose_prompt(plan.prompt_system.as_deref(), "make hello.txt");
        assert!(prompt.contains(awili_code::PLAN_FIRST_SYSTEM), "{prompt}");
        assert!(prompt.contains(CLAUDE_PLAN_IN_REPLY), "{prompt}");
        assert!(prompt.ends_with("make hello.txt"), "{prompt}");
        assert!(
            !plan
                .args
                .iter()
                .any(|a| a.starts_with("--disallowedTools") || a.starts_with("--tools")),
            "no tool is taken away: {:?}",
            plan.args
        );
        // The line is in the turn's prompt, never in the system text that a
        // resumed session keeps.
        assert!(!plan.args.iter().any(|a| a.contains(CLAUDE_PLAN_IN_REPLY)));
        let go = launch(&paths, &spec(CodeEngine::Claude, Autonomy::Autonomous));
        assert!(go
            .prompt_system
            .as_deref()
            .is_none_or(|p| !p.contains(CLAUDE_PLAN_IN_REPLY)));
        let codex = launch(&paths, &spec(CodeEngine::Codex, Autonomy::AskFirst));
        assert!(codex
            .prompt_system
            .as_deref()
            .is_none_or(|p| !p.contains(CLAUDE_PLAN_IN_REPLY)));
    }

    /// RC2-R4M4 (slice review P2: the plan-file property was only a prompt
    /// line). A Claude Work planning step launches with settings that make
    /// the CLI refuse any write under `~/.claude/plans/` (live 2026-10-08,
    /// claude 2.1.295: a planning turn told to save its plan file got "File
    /// is in a directory that is denied by your permission settings" and gave
    /// the plan in its reply; without the rule the same turn wrote the file).
    /// Plan mode, the planning text and every tool stay; the approved
    /// follow-up and the owner's Chat carry no such rule.
    #[test]
    fn rc2r4m4_a_claude_planning_step_cannot_write_a_plan_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cwd = tmp.path().join("work");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let paths = LaunchPaths {
            home: home.clone(),
            codex_home: home.join(".codex"),
        };
        let system = format!("{}\n\nBRIEF-TEXT", awili_code::PLAN_FIRST_SYSTEM);
        let spec = |autonomy| LaunchSpec {
            engine: CodeEngine::Claude,
            autonomy,
            cwd: &cwd,
            system: Some(&system),
            mem: None,
            agents_md: None,
            connectors: None,
        };
        let settings = |l: &Launch| -> serde_json::Value {
            let at = l.args.iter().position(|a| a == "--settings").unwrap();
            serde_json::from_str(&l.args[at + 1]).unwrap()
        };
        let plan = launch(&paths, &spec(Autonomy::AskFirst));
        let s = settings(&plan);
        let deny: Vec<&str> = s["permissions"]["deny"]
            .as_array()
            .unwrap_or_else(|| panic!("no deny rules: {s}"))
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            deny,
            ["Edit(~/.claude/plans/**)", "Write(~/.claude/plans/**)"],
            "{s}"
        );
        assert_eq!(
            s["claudeMdExcludes"],
            serde_json::json!(claude_md_excludes(&home))
        );
        assert!(s["permissions"].get("allow").is_none(), "{s}");
        // Plan mode itself, the planning text and every tool stay.
        let argv = awili_code::CodeEngine::Claude.build_args("p", Autonomy::AskFirst, None, None);
        assert!(argv.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        let prompt =
            crate::code_provider::compose_prompt(plan.prompt_system.as_deref(), "make hello.txt");
        assert!(prompt.contains(CLAUDE_PLAN_IN_REPLY), "{prompt}");
        assert!(
            !plan
                .args
                .iter()
                .any(|a| a.starts_with("--disallowedTools") || a.starts_with("--tools")),
            "{:?}",
            plan.args
        );
        // The approved follow-up: no rule of Awili's.
        let l = launch(&paths, &spec(Autonomy::Autonomous));
        assert!(settings(&l).get("permissions").is_none(), "{:?}", l.args);
    }
}
