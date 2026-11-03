//! rc.4 S7: the opt-in shell sandbox for agent commands.
//!
//! The owner's standing rule (2026-06-24, `CodeEngine::permission_args`) is
//! that an Autonomous turn has full filesystem AND network access. This module
//! is the one exception, and only when the owner turns the setting on
//! (`[work.sandbox] enabled`, default OFF): the vendor agent process is
//! started as `/usr/bin/sandbox-exec -f <profile> <bin> <args…>`, and
//! everything the agent runs inherits the macOS seatbelt profile.
//!
//! The profile limits file WRITES only. Reads and the network stay open (the
//! network is how the agent reaches memory, the web and the fleet). Writes are
//! allowed in: the working folder (and the git directories of its repository),
//! temp, a few devices, the agent's own state, `~/Library/Caches`,
//! `~/.ssh/known_hosts` and the owner's `extra_writable` folders.
//!
//! After the allow rules come DENY rules (in SBPL the later rule wins) for the
//! files that later runs EXECUTE or obey: the agents' settings, hooks,
//! commands, skills, agents, plugins and instruction files, and each
//! repository's `.git/hooks` and `.git/config`. So a sandboxed run cannot
//! plant something that a later, unsandboxed run would execute.
//!
//! This guards against mistakes. It is not a security boundary against a
//! hostile agent (see `docs/v2/rc4/SANDBOX.md`, "Limits").
//!
//! The planning step (`Autonomy::AskFirst`) is never wrapped: the vendor
//! harness holds it read-only already, and Codex's own sandbox is
//! `sandbox-exec`, which cannot nest.
//!
//! [`profile_text`] is a pure function of its inputs and the filesystem
//! (it canonicalises paths); [`write_profile`] stores the text in a
//! write-once, content-addressed file; [`prepare`] ties them to a turn and
//! fails closed.

use std::path::{Path, PathBuf};

use awili_code::{Autonomy, CodeEngine};

/// The macOS seatbelt launcher.
pub(crate) const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Why an enabled sandbox refuses to start off macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) const UNAVAILABLE_OFF_MACOS: &str = "Couldn't start the sandbox: the agent sandbox is only available on macOS. Turn it off in Settings to run without it.";

/// The git directories a working folder's repository needs written: the
/// repository's own `.git` folder (or a linked worktree's private gitdir, or a
/// submodule's gitdir) and the common dir shared by linked worktrees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GitDirs {
    pub dirs: Vec<PathBuf>,
}

/// Everything the profile text depends on.
#[derive(Debug, Clone)]
pub(crate) struct ProfileInputs<'a> {
    /// The turn's working folder, already canonical.
    pub folder: &'a Path,
    pub engine: CodeEngine,
    pub home: &'a Path,
    pub codex_home: &'a Path,
    /// Awili's data folder (`<data>/Awili`).
    pub data_dir: &'a Path,
    /// Temp folders (`/private/tmp`, `/private/var/folders`, `$TMPDIR`).
    pub temp_dirs: &'a [PathBuf],
    /// `[work.sandbox] extra_writable`, `~` already expanded.
    pub extra: &'a [PathBuf],
    pub git: &'a GitDirs,
}

/// A path that cannot be written into a profile is refused (the turn does
/// not start; the sandbox is never silently dropped).
fn check_path(p: &Path) -> Result<&str, String> {
    let s = p
        .to_str()
        .ok_or_else(|| format!("the path {} is not valid text", p.display()))?;
    if s.contains('\0') || s.contains('\n') || s.contains('\r') {
        return Err(format!(
            "a path contains a line break or a hidden character: {}",
            s.escape_debug()
        ));
    }
    if !s.starts_with('/') {
        return Err(format!("{s} is not a full path"));
    }
    Ok(s)
}

/// Escape one SBPL string literal body (`"` and `\`).
pub(crate) fn sbpl_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape regex metacharacters (the profile's `regex` filters).
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Canonical form of `p`, resolving symlinks. A path that does not exist yet
/// is resolved through its nearest existing parent, so a file that does not
/// exist (a deny target an agent might create) still gets its real location.
pub(crate) fn canon_lenient(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) if parent != p => canon_lenient(parent).join(name),
        _ => p.to_path_buf(),
    }
}

/// The agent's own state folders and files, by engine (canonical).
fn engine_state(i: &ProfileInputs<'_>) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let h = i.home;
    let (dirs, prefixes): (Vec<PathBuf>, Vec<PathBuf>) = match i.engine {
        // `~/.claude.json*` also holds the atomic-write temp files.
        CodeEngine::Claude => (
            vec![
                h.join(".claude"),
                h.join(".local/share/claude"),
                h.join(".local/state/claude"),
            ],
            vec![h.join(".claude.json")],
        ),
        CodeEngine::Codex => (vec![i.codex_home.to_path_buf(), h.join(".codex")], vec![]),
        CodeEngine::Glm => (
            vec![
                h.join(".zcode"),
                h.join("Library/Application Support/ZCode"),
            ],
            vec![],
        ),
        CodeEngine::Dsh => (vec![h.join(".dsh")], vec![]),
    };
    (
        dirs.iter().map(|d| canon_lenient(d)).collect(),
        prefixes.iter().map(|d| canon_lenient(d)).collect(),
    )
}

/// One deny rule.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Deny {
    Subpath(PathBuf),
    Literal(PathBuf),
    /// A regex over the full path (already a regex, not escaped again).
    Regex(String),
}

/// Every spelling of a path: where it really is, where the entry itself sits
/// (a symlink's own location), and the path as written, because removing or
/// replacing a link is a write to the link's path, not its target's.
fn spellings(p: &Path) -> Vec<PathBuf> {
    let real = canon_lenient(p);
    let mut v = vec![real.clone()];
    // Where the entry itself sits (a symlink's own location, not its target).
    if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
        let here = canon_lenient(parent).join(name);
        if !v.contains(&here) {
            v.push(here);
        }
    }
    if !v.iter().any(|x| x == p) {
        v.push(p.to_path_buf());
    }
    v
}

/// Files an agent's later (unsandboxed) runs execute or obey, per engine, plus
/// each repository's hooks and config, plus the generated-profile folder.
fn deny_rules(i: &ProfileInputs<'_>) -> Vec<Deny> {
    let h = i.home;
    let mut out: Vec<Deny> = Vec::new();
    let mut subs: Vec<PathBuf> = Vec::new();
    let mut lits: Vec<PathBuf> = Vec::new();
    let mut regexes: Vec<PathBuf> = Vec::new(); // `settings*.json` parents
    let mut profile_regexes: Vec<PathBuf> = Vec::new(); // dsh profile roots
    match i.engine {
        CodeEngine::Claude => {
            let b = h.join(".claude");
            for n in ["hooks", "commands", "skills", "agents", "plugins"] {
                subs.push(b.join(n));
            }
            lits.push(b.join("CLAUDE.md"));
            regexes.push(b);
        }
        CodeEngine::Codex => {
            for b in [i.codex_home.to_path_buf(), h.join(".codex")] {
                for n in [
                    "config.toml",
                    "hooks.json",
                    "AGENTS.md",
                    "AGENTS.override.md",
                ] {
                    lits.push(b.join(n));
                }
                subs.push(b.join("rules"));
            }
        }
        CodeEngine::Glm => {
            let z = h.join(".zcode");
            for n in [
                "cli/config.json",
                "v2/config.json",
                "v2/setting.json",
                "v2/provider_config.json",
                "AGENTS.md",
            ] {
                lits.push(z.join(n));
            }
            for n in ["cli/plugins", "cli/agents", "skills", "plugin-workspace"] {
                subs.push(z.join(n));
            }
        }
        // dsh composes its tools from its profile folders. It regenerates
        // `cordis.yml` itself on every start (live probe 2026-11-01: denying it
        // makes dsh crash with EPERM), so that one file stays writable; what it
        // is generated from, and the packages it loads, do not.
        CodeEngine::Dsh => {
            subs.push(h.join(".dsh/profiles/node_modules"));
            profile_regexes.push(h.join(".dsh/profiles"));
        }
    }
    for p in subs {
        for s in spellings(&p) {
            out.push(Deny::Subpath(s));
        }
    }
    for p in lits {
        for s in spellings(&p) {
            out.push(Deny::Literal(s));
        }
    }
    for base in regexes {
        for b in spellings(&base) {
            if let Some(s) = b.to_str() {
                out.push(Deny::Regex(format!(
                    "^{}/settings[^/]*\\.json$",
                    regex_escape(s)
                )));
            }
        }
    }
    for base in profile_regexes {
        for b in spellings(&base) {
            if let Some(s) = b.to_str() {
                let s = regex_escape(s);
                out.push(Deny::Regex(format!(
                    "^{s}/[^/]+/(cordis\\.patch\\.yml|package\\.json|pnpm-workspace\\.yaml)$"
                )));
                out.push(Deny::Regex(format!("^{s}/[^/]+/node_modules(/.*)?$")));
            }
        }
    }
    for g in &i.git.dirs {
        out.push(Deny::Subpath(g.join("hooks")));
        out.push(Deny::Literal(g.join("config")));
        out.push(Deny::Literal(g.join("config.worktree")));
        // At any depth: modules/<name>/hooks, worktrees/<x>/config.worktree, ...
        if let Some(s) = g.to_str() {
            out.push(Deny::Regex(format!(
                "^{}/(.*/)?(hooks(/|$)|config(\\.worktree)?$)",
                regex_escape(s)
            )));
        }
    }
    // A repository nested anywhere under the working folder, even one created
    // during the turn.
    if let Some(s) = i.folder.to_str() {
        out.push(Deny::Regex(format!(
            "^{}/(.*/)?\\.git/(hooks(/|$)|config(\\.worktree)?$)",
            regex_escape(s)
        )));
    }
    // The generated profiles are never writable, whatever else allows the path.
    out.push(Deny::Subpath(canon_lenient(&profile_dir(i.data_dir))));
    let mut seen = Vec::new();
    out.retain(|d| {
        let dup = seen.contains(d);
        if !dup {
            seen.push(d.clone());
        }
        !dup
    });
    out
}

/// Unix sockets of other agent hosts and command daemons (the Codex desktop
/// app's `ipc.sock`, the mac-shell MCP daemon, Docker, Claude's messaging and
/// browser-bridge sockets, zcode and dsh state sockets) that a sandboxed agent must not talk to: a
/// command sent to a host that is not sandboxed runs outside the profile (a
/// live Codex probe on 2026-11-01 wrote outside the folder once). Only unix
/// sockets: the internet stays open. Returns (subpaths, path regexes).
fn ipc_denies(i: &ProfileInputs<'_>) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let h = i.home;
    let mut subs: Vec<PathBuf> = Vec::new();
    for d in [
        i.codex_home.to_path_buf(),
        h.join(".codex"),
        h.join(".claude"),
        h.join(".zcode"),
        h.join(".dsh"),
        h.join("Library/Application Support/Claude"),
        h.join("Library/Application Support/Codex"),
        h.join("Library/Application Support/ZCode"),
        // The mac-shell MCP daemon: every adapter an agent starts forwards its
        // commands to one long-running unsandboxed daemon over this socket
        // (found 2026-11-01: a sandboxed Codex ran commands outside the profile
        // through the mac-shell tool). Docker: a daemon that runs containers.
        h.join(".mac-shell"),
        h.join(".docker"),
        // Docker-compatible daemons under home (they run containers, so
        // arbitrary commands), terminal multiplexers that forward keystrokes
        // to a live shell, and their sockets in home.
        h.join(".colima"),
        h.join(".lima"),
        h.join(".orbstack"),
        h.join(".rd"),
        h.join(".local/share/containers"),
        h.join("Library/Containers/com.docker.docker"),
        h.join(".screen"),
        h.join(".zellij"),
        PathBuf::from("/private/var/run/screen"),
        PathBuf::from("/private/var/run/docker.sock"),
        PathBuf::from("/var/run/docker.sock"),
    ] {
        for s in spellings(&d) {
            if !subs.contains(&s) {
                subs.push(s);
            }
        }
    }
    // Agent-host folders in temp (Codex daemon, browser-use, app-tools,
    // artifact sessions, Claude `cc-socks` and browser bridge, Chromium-style
    // singleton sockets of the Codex app).
    let names = "(codex|claude|cc-socks|com\\.openai|tmux|zellij|podman|screen)[^/]*";
    let mut res: Vec<String> = Vec::new();
    let mut temps: Vec<PathBuf> = vec![PathBuf::from("/private/tmp")];
    temps.extend(i.temp_dirs.iter().cloned());
    for t in &temps {
        let s = check_path(t)?;
        if s.contains('"') {
            return Err("a temp path contains a quote".into());
        }
        let r = format!("^{}/(.*/)?{names}/", regex_escape(s));
        if !res.contains(&r) {
            res.push(r);
        }
        // VS Code, Cursor and Windsurf IPC and git sockets (files in temp).
        let r = format!("^{}/(.*/)?(vscode|cursor|windsurf)[^/]*", regex_escape(s));
        if !res.contains(&r) {
            res.push(r);
        }
    }
    Ok((subs, res))
}

/// Directories that must not be renamed away or removed. A leaf deny is
/// useless if its parent can be moved aside, edited elsewhere and moved back
/// (`mv ~/.claude /tmp/c; echo x > /tmp/c/settings.json; mv /tmp/c ~/.claude`),
/// so every ancestor directory of a denied path that lies inside an allowed
/// root (up to and including that root) gets a `file-write-unlink` deny, in
/// every spelling. Creating and deleting ordinary files and folders inside
/// them is unaffected. Returns literal directories and regexes.
fn protect_rules(
    i: &ProfileInputs<'_>,
    denies: &[Deny],
    roots: &[PathBuf],
) -> (Vec<PathBuf>, Vec<String>) {
    let mut lits: Vec<PathBuf> = Vec::new();
    let mut add = |p: &Path| {
        if !lits.iter().any(|x| x == p) {
            lits.push(p.to_path_buf());
        }
    };
    for d in denies {
        let (Deny::Subpath(p) | Deny::Literal(p)) = d else {
            continue;
        };
        let mut up = p.parent();
        while let Some(a) = up {
            if a == Path::new("/") || !roots.iter().any(|r| a.starts_with(r)) {
                break;
            }
            add(a);
            up = a.parent();
        }
    }
    // The working folder itself (`mv folder /tmp/x`, plant, move back).
    add(i.folder);
    let mut res: Vec<String> = Vec::new();
    // Submodule and linked-worktree gitdirs below each git dir.
    for g in &i.git.dirs {
        if let Some(s) = g.to_str() {
            res.push(format!(
                "^{}/(modules|worktrees)(/[^/]+)?$",
                regex_escape(s)
            ));
        }
    }
    // A repository's `.git` anywhere under the folder.
    if let Some(s) = i.folder.to_str() {
        res.push(format!("^{}/(.*/)?\\.git$", regex_escape(s)));
    }
    // dsh: each profile folder.
    if i.engine == CodeEngine::Dsh {
        for b in spellings(&i.home.join(".dsh/profiles")) {
            if let Some(s) = b.to_str() {
                res.push(format!("^{}/[^/]+$", regex_escape(s)));
            }
        }
    }
    (lits, res)
}

/// The seatbelt profile text for a turn. Deterministic: the same inputs (and
/// filesystem) give the same bytes (the file name is a hash of them).
pub(crate) fn profile_text(i: &ProfileInputs<'_>) -> Result<String, String> {
    let mut subpaths: Vec<String> = Vec::new();
    let mut prefixes: Vec<String> = Vec::new();
    let mut literals: Vec<String> = Vec::new();
    let add = |v: &mut Vec<String>, p: &Path| -> Result<(), String> {
        let s = check_path(p)?.to_string();
        if !v.contains(&s) {
            v.push(s);
        }
        Ok(())
    };
    add(&mut subpaths, i.folder)?;
    for g in &i.git.dirs {
        add(&mut subpaths, g)?;
    }
    for t in i.temp_dirs {
        add(&mut subpaths, t)?;
    }
    let (state_dirs, state_prefixes) = engine_state(i);
    for d in &state_dirs {
        add(&mut subpaths, d)?;
    }
    for f in &state_prefixes {
        add(&mut prefixes, f)?;
    }
    add(&mut subpaths, &canon_lenient(&i.data_dir.join("agents")))?;
    add(
        &mut subpaths,
        &canon_lenient(&i.home.join("Library/Caches")),
    )?;
    // Only this file, so a first SSH to a new host can record its key.
    add(
        &mut literals,
        &canon_lenient(&i.home.join(".ssh/known_hosts")),
    )?;
    for e in i.extra {
        add(&mut subpaths, e)?;
    }
    let denies = deny_rules(i);
    // Validate every deny path too (a bad one refuses the turn).
    for d in &denies {
        match d {
            Deny::Subpath(p) | Deny::Literal(p) => {
                check_path(p)?;
            }
            Deny::Regex(r) => {
                if r.contains('\n') || r.contains('\0') {
                    return Err("a path contains a line break or a hidden character".into());
                }
            }
        }
    }

    let mut out = String::new();
    out.push_str("(version 1)\n");
    out.push_str(";; Awili rc.4 S7: file writes limited; reads and network stay open.\n");
    out.push_str(";; A guard against mistakes, not a boundary against a hostile agent.\n");
    out.push_str("(allow default)\n");
    out.push_str("(deny file-write*)\n");
    out.push_str("(allow file-write*\n");
    for s in &subpaths {
        out.push_str(&format!("  (subpath \"{}\")\n", sbpl_escape(s)));
    }
    for s in &prefixes {
        // The file itself plus its temp/backup siblings (`.claude.json.tmp…`).
        out.push_str(&format!(
            "  (regex \"^{}\")\n",
            sbpl_escape(&regex_escape(s))
        ));
    }
    for s in &literals {
        out.push_str(&format!("  (literal \"{}\")\n", sbpl_escape(s)));
    }
    for dev in [
        "/dev/null",
        "/dev/zero",
        "/dev/tty",
        "/dev/ptmx",
        "/dev/dtracehelper",
    ] {
        out.push_str(&format!("  (literal \"{dev}\")\n"));
    }
    out.push_str("  (subpath \"/dev/fd\")\n");
    out.push_str("  (regex \"^/dev/ttys[0-9]+$\")\n");
    out.push_str(")\n");
    // Later rules win: these cut holes in the allowed trees above.
    out.push_str(";; Executable config: never writable from a sandboxed run.\n");
    out.push_str("(deny file-write*\n");
    for d in &denies {
        match d {
            Deny::Subpath(p) => out.push_str(&format!(
                "  (subpath \"{}\")\n",
                sbpl_escape(&p.to_string_lossy())
            )),
            Deny::Literal(p) => out.push_str(&format!(
                "  (literal \"{}\")\n",
                sbpl_escape(&p.to_string_lossy())
            )),
            Deny::Regex(r) => out.push_str(&format!("  (regex \"{}\")\n", sbpl_escape(r))),
        }
    }
    out.push_str(")\n");
    // Rename-away / removal of the directories that hold the denied files.
    let roots: Vec<PathBuf> = subpaths.iter().map(PathBuf::from).collect();
    let (dirs, dir_regexes) = protect_rules(i, &denies, &roots);
    out.push_str(";; Directories holding them cannot be moved aside or removed either.\n");
    out.push_str("(deny file-write-unlink\n");
    for d in &dirs {
        out.push_str(&format!(
            "  (literal \"{}\")\n",
            sbpl_escape(check_path(d)?)
        ));
    }
    for r in &dir_regexes {
        out.push_str(&format!("  (regex \"{}\")\n", sbpl_escape(r)));
    }
    out.push_str(")\n");
    // No IPC with other agent hosts over unix sockets (the internet stays open).
    let (sock_dirs, sock_res) = ipc_denies(i)?;
    out.push_str(";; Unix sockets of other agent hosts: no IPC. The internet stays open.\n");
    out.push_str("(deny network-outbound\n");
    for d in &sock_dirs {
        out.push_str(&format!(
            "  (remote unix-socket (subpath \"{}\"))\n",
            sbpl_escape(check_path(d)?)
        ));
    }
    for r in &sock_res {
        out.push_str(&format!("  (remote unix-socket (path-regex #\"{r}\"))\n"));
    }
    out.push_str(")\n");
    Ok(out)
}

/// The Awili-owned folder profiles live in: `<data>/Awili/sandbox-profiles`,
/// outside every allowed path (and denied explicitly in every profile).
pub(crate) fn profile_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("sandbox-profiles")
}

/// SHA-256, first 16 bytes in hex.
fn content_name(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("profile-{hex}.sb")
}

/// Write `text` to `dir` under its content-addressed name and return the
/// path. Write-once: an existing file with that name is the same bytes and is
/// left alone; a new one is written to a unique temp name then renamed, so a
/// concurrent launch never sees a partial file or a rewrite. The folder is
/// mode 0700 whether or not it existed.
#[cfg(not(unix))]
pub(crate) fn write_profile(_dir: &Path, _text: &str) -> Result<PathBuf, String> {
    // The Seatbelt profile is macOS-only (`sandbox-exec`). TODO(windows-phase1):
    // a Windows agent sandbox (AppContainer / restricted token). Until then the
    // sandbox reports unavailable; nothing runs "as if" contained.
    Err("the agent sandbox is not available on Windows yet".to_string())
}

#[cfg(unix)]
pub(crate) fn write_profile(dir: &Path, text: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot protect {}: {e}", dir.display()))?;
    let path = dir.join(content_name(text));
    if std::fs::read_to_string(&path).is_ok_and(|have| have == text) {
        return Ok(path);
    }
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".tmp-{}-{n}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let wrote = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()));
    drop(file);
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot publish {}: {e}", path.display())
    })?;
    Ok(path)
}

/// `gitdir: <path>` of a `.git` file, resolved against `base`.
fn read_gitdir_file(dot_git: &Path, base: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let gitdir = if Path::new(gitdir).is_absolute() {
        PathBuf::from(gitdir)
    } else {
        base.join(gitdir)
    };
    std::fs::canonicalize(gitdir).ok()
}

/// The git directories a turn in `folder` needs. Walks up from `folder` to the
/// nearest `.git`:
/// - a `.git` folder: that folder;
/// - a `.git` file (linked worktree): its private gitdir AND the common dir
///   named by its `commondir`;
/// - a `.git` file whose gitdir has no `commondir` (a submodule): that gitdir.
///
/// Empty when `folder` is not inside a repository.
pub(crate) fn git_dirs(folder: &Path) -> GitDirs {
    for dir in folder.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = std::fs::symlink_metadata(&dot_git) else {
            continue;
        };
        let mut dirs = Vec::new();
        if meta.is_dir() {
            dirs.extend(std::fs::canonicalize(&dot_git).ok());
        } else if let Some(gitdir) = read_gitdir_file(&dot_git, dir) {
            let common = std::fs::read_to_string(gitdir.join("commondir"))
                .ok()
                .map(|c| {
                    let c = c.trim();
                    if Path::new(c).is_absolute() {
                        PathBuf::from(c)
                    } else {
                        gitdir.join(c)
                    }
                })
                .and_then(|c| std::fs::canonicalize(c).ok());
            dirs.push(gitdir);
            dirs.extend(common);
        }
        dirs.dedup();
        return GitDirs { dirs };
    }
    GitDirs::default()
}

/// Repositories already nested below `folder` (a few levels, bounded): their
/// `.git` folders get the same hook/config denies and rename protection as
/// the top one. A repository created during the turn is covered by the
/// folder-wide patterns in the profile, but not by the parent protection.
pub(crate) fn nested_git_dirs(folder: &Path) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 4;
    const MAX_ENTRIES: usize = 4000;
    let mut found = Vec::new();
    let mut seen = 0usize;
    let mut stack = vec![(folder.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return found;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            let Ok(ft) = e.file_type() else { continue };
            if !ft.is_dir() {
                continue;
            }
            if name == ".git" {
                if dir != folder {
                    found.extend(std::fs::canonicalize(e.path()).ok());
                }
                continue;
            }
            if depth + 1 < MAX_DEPTH && name != "node_modules" && name != "target" {
                stack.push((e.path(), depth + 1));
            }
        }
    }
    found.sort();
    found
}

/// `~` and `~/…` expanded against `home`.
fn expand_tilde(p: &str, home: &Path) -> PathBuf {
    if p == "~" {
        home.to_path_buf()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(p)
    }
}

/// `/`, the home folder or any folder above it: a sandbox that allows writes
/// there protects nothing.
fn too_broad(p: &Path, home: &Path) -> bool {
    p == Path::new("/") || home.starts_with(p)
}

/// What a turn needs to start sandboxed, or `Ok(None)` when it starts as it
/// always did (setting off, or the read-only planning step). `Err` is a plain
/// sentence for the owner; the turn then does not start. `data_dir` is
/// `<data>/Awili`; `paths` is `None` when no home folder is known (refused).
pub(crate) fn prepare(
    cfg: &awili_config::SandboxConfig,
    engine: CodeEngine,
    autonomy: Autonomy,
    cwd: &str,
    paths: Option<&crate::agent_launch::LaunchPaths>,
    data_dir: &Path,
) -> Result<Option<PathBuf>, String> {
    // The agent sandbox is macOS Seatbelt (`sandbox-exec`). Anywhere else it
    // reports itself unavailable: an enabled sandbox never starts a turn
    // "as if" contained. TODO(linux): bubblewrap/Landlock for Autonomous runs.
    #[cfg(not(target_os = "macos"))]
    if cfg.enabled && autonomy == Autonomy::Autonomous {
        return Err(UNAVAILABLE_OFF_MACOS.to_string());
    }
    prepare_with(
        Path::new(SANDBOX_EXEC),
        cfg,
        engine,
        autonomy,
        cwd,
        paths,
        data_dir,
    )
}

pub(crate) fn prepare_with(
    exec: &Path,
    cfg: &awili_config::SandboxConfig,
    engine: CodeEngine,
    autonomy: Autonomy,
    cwd: &str,
    paths: Option<&crate::agent_launch::LaunchPaths>,
    data_dir: &Path,
) -> Result<Option<PathBuf>, String> {
    if !cfg.enabled || autonomy != Autonomy::Autonomous {
        return Ok(None);
    }
    let fail = |why: String| format!("Couldn't start the sandbox: {why}.");
    let Some(paths) = paths else {
        return Err(fail("this Mac reports no home folder".into()));
    };
    if !exec.is_file() {
        return Err(fail(format!("{} is missing on this Mac", exec.display())));
    }
    let home = canon_lenient(&paths.home);
    let folder = std::fs::canonicalize(cwd)
        .map_err(|e| fail(format!("the working folder {cwd} can't be opened ({e})")))?;
    if too_broad(&folder, &home) {
        return Err(fail(format!(
            "{} is your home folder or above it, so limiting writes there would protect nothing. \
             Choose a project folder",
            folder.display()
        )));
    }
    let mut temp_dirs = vec![
        PathBuf::from("/private/tmp"),
        PathBuf::from("/private/var/folders"),
    ];
    temp_dirs.push(canon_lenient(&std::env::temp_dir()));
    let mut extra: Vec<PathBuf> = Vec::new();
    for raw in cfg.extra_writable.iter().filter(|p| !p.trim().is_empty()) {
        let p = canon_lenient(&expand_tilde(raw.trim(), &paths.home));
        if too_broad(&p, &home) {
            return Err(fail(format!(
                "the extra writable folder {} is your home folder or above it",
                p.display()
            )));
        }
        extra.push(p);
    }
    let mut git = git_dirs(&folder);
    for n in nested_git_dirs(&folder) {
        if !git.dirs.contains(&n) {
            git.dirs.push(n);
        }
    }
    let text = profile_text(&ProfileInputs {
        folder: &folder,
        engine,
        home: &paths.home,
        codex_home: &paths.codex_home,
        data_dir,
        temp_dirs: &temp_dirs,
        extra: &extra,
        git: &git,
    })
    .map_err(fail)?;
    write_profile(&profile_dir(data_dir), &text)
        .map(Some)
        .map_err(fail)
}

/// The program and arguments a turn runs: unchanged without a profile,
/// otherwise `/usr/bin/sandbox-exec -f <profile> <bin> <args…>`.
pub(crate) fn launch_command(
    profile: Option<&Path>,
    bin: &str,
    args: &[String],
) -> (String, Vec<String>) {
    match profile {
        None => {
            // Windows: an npm `.cmd` shim is replaced by the node script or
            // native program it forwards to, so no `cmd.exe` (and none of its
            // argument-escaping limits) sits between Awili and the CLI.
            #[cfg(windows)]
            if let Some((program, mut lead)) = crate::agent_bin::resolve_shim(bin) {
                lead.extend(args.iter().cloned());
                return (program, lead);
            }
            (bin.to_string(), args.to_vec())
        }
        Some(p) => {
            let mut wrapped = vec!["-f".to_string(), p.display().to_string(), bin.to_string()];
            wrapped.extend(args.iter().cloned());
            (SANDBOX_EXEC.to_string(), wrapped)
        }
    }
}

/// `Awili` in the app data folder. Test builds use a temp folder, never the
/// owner's real data.
pub(crate) fn data_dir() -> Result<PathBuf, String> {
    #[cfg(test)]
    if !std::env::var_os("HOME")
        .map(PathBuf::from)
        .is_some_and(|h| crate::test_home_guard::inside(&h, &std::env::temp_dir()))
    {
        return Ok(std::env::temp_dir().join("awili-test-data"));
    }
    dirs::data_dir()
        .map(|d| d.join(awili_config::APP_DIR_NAME))
        .ok_or_else(|| "there is no data folder".to_string())
}

/// The Job panel / Inspect line for a run: "Sandbox: on — writes limited to
/// <folder name>" or "Sandbox: off".
pub(crate) fn status_line(on: bool, folder: &str) -> String {
    if !on {
        return "Sandbox: off".to_string();
    }
    let name = Path::new(folder.trim_end_matches('/'))
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("the working folder");
    format!("Sandbox: on \u{2014} writes limited to {name}")
}

#[cfg(test)]
#[path = "agent_sandbox_tests.rs"]
mod tests;
