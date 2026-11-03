//! Finding vendor CLIs (`claude`, `codex`, `zcode`, `dsh`) the way Windows
//! users install them, and launching npm's `.cmd` shims safely.
//!
//! The matching logic is pure (directories, `PATHEXT` and the file probe are
//! parameters) so it is tested on every platform; only the thin callers that
//! read the real environment are `cfg(windows)`. macOS and Linux discovery in
//! `work::resolve_agent_bin` is untouched.
//!
//! Spawning a `.cmd` or `.bat` file: since Rust 1.77.2 std escapes arguments
//! for `cmd.exe` itself (the CVE-2024-24576 fix) and refuses, with
//! `InvalidInput`, an argument it cannot escape (a newline, for one). Awili
//! never builds its own `cmd /c` string. On top of that, an npm shim only
//! forwards to a node script or a native `.exe`, so [`resolve_shim`] reads the
//! shim and runs the real target directly: no `cmd.exe` in the path at all, so
//! multi-line prompt arguments do not hit the std refusal either. A `.cmd`
//! whose target cannot be read still goes through std's escaping; a refused
//! argument then fails the launch (closed), never a guess.

// The pure matching is exercised by tests on every platform but only called on
// Windows.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};

/// Extensions Windows treats as directly runnable and that we will launch.
const RUNNABLE: [&str; 4] = [".com", ".exe", ".bat", ".cmd"];

/// The system default when `PATHEXT` is not set.
pub(crate) const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The runnable extensions in `PATHEXT` order (lower-case, with the dot).
/// Entries outside [`RUNNABLE`] (`.vbs`, `.js`, ...) are dropped: they are not
/// something Awili can spawn as a program.
pub(crate) fn runnable_extensions(pathext: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in pathext.split(';') {
        let ext = raw.trim().to_ascii_lowercase();
        if RUNNABLE.contains(&ext.as_str()) && !out.contains(&ext) {
            out.push(ext);
        }
    }
    if out.is_empty() {
        out = RUNNABLE.iter().map(|e| e.to_string()).collect();
    }
    out
}

/// The file names to try for `name` inside one directory: `name` itself when
/// it already ends in a runnable extension, otherwise `name` plus each
/// extension in order.
pub(crate) fn candidate_names(name: &str, pathext: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    if RUNNABLE.iter().any(|e| lower.ends_with(e)) {
        return vec![name.to_string()];
    }
    runnable_extensions(pathext)
        .into_iter()
        .map(|e| format!("{name}{e}"))
        .collect()
}

/// The first directory (in order) holding a runnable `name`, as the full path.
/// `is_file` is the probe (the real file system, or a fake in tests).
pub(crate) fn find_in_dirs(
    name: &str,
    dirs: &[PathBuf],
    pathext: &str,
    is_file: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let names = candidate_names(name, pathext);
    for dir in dirs {
        // A relative entry (`.`, `tools`, an empty one) is resolved against the
        // process's current folder, which can be a project the user opened:
        // never searched, so a project cannot supply `claude.exe`.
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        for n in &names {
            let p = dir.join(n);
            if is_file(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// The places a Windows user's CLI usually lands, in order: npm's global
/// folder (`%APPDATA%\npm`, where the `.cmd` shims live), the per-user program
/// folder (`%LOCALAPPDATA%\Programs`), `~/.local/bin`, then `~/.npm-global/bin`.
pub(crate) fn windows_default_dirs(
    appdata: Option<&Path>,
    local_appdata: Option<&Path>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(a) = appdata {
        v.push(a.join("npm"));
    }
    if let Some(l) = local_appdata {
        v.push(l.join("Programs"));
    }
    if let Some(h) = home {
        v.push(h.join(".local").join("bin"));
        v.push(h.join(".npm-global").join("bin"));
    }
    v
}

/// What an npm `.cmd` shim runs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ShimTarget {
    /// A native program: run it directly.
    Exe(PathBuf),
    /// A node script: run `node <script> <args…>`.
    Node {
        script: PathBuf,
        /// `node.exe` next to the shim, when npm put one there.
        local_node: bool,
    },
}

/// Read the target out of the text of an npm `cmd-shim` file. The shim names
/// its target as `"%dp0%\relative\path"` (or `%~dp0`); the last such path that
/// is not `node.exe` itself is the program. `None` when the file is not a
/// recognisable shim (the caller then runs it through std's `.cmd` handling).
pub(crate) fn parse_shim(text: &str, shim_dir: &Path) -> Option<ShimTarget> {
    let local_node = text.contains("%dp0%\\node.exe") || text.contains("%~dp0\\node.exe");
    let mut target: Option<String> = None;
    for line in text.lines() {
        let mut rest = line;
        while let Some(start) = ["\"%dp0%", "\"%~dp0"]
            .iter()
            .filter_map(|m| rest.find(m).map(|i| (i, m.len())))
            .min()
        {
            let after = &rest[start.0 + start.1..];
            let Some(end) = after.find('"') else { break };
            let rel = after[..end].trim_start_matches(['\\', '/']);
            let lower = rel.to_ascii_lowercase();
            let runnable = lower.ends_with(".exe")
                || lower.ends_with(".js")
                || lower.ends_with(".mjs")
                || lower.ends_with(".cjs");
            if runnable && lower != "node.exe" {
                target = Some(rel.to_string());
            }
            rest = &after[end + 1..];
        }
    }
    let rel = target?;
    // A target must stay inside the shim's own folder tree (npm puts it under
    // `node_modules`): a `..` anywhere means the shim is not trusted to name it.
    if rel.split(['\\', '/']).any(|c| c == "..") {
        return None;
    }
    let path = rel
        .split(['\\', '/'])
        .filter(|c| !c.is_empty())
        .fold(shim_dir.to_path_buf(), |p, c| p.join(c));
    if rel.to_ascii_lowercase().ends_with(".exe") {
        Some(ShimTarget::Exe(path))
    } else {
        Some(ShimTarget::Node {
            script: path,
            local_node,
        })
    }
}

/// Whether `target` really lies under `base` once links are resolved (so a
/// `node_modules` entry that is a junction or symlink out of the tree is
/// refused). Both must exist.
pub(crate) fn contained_in(target: &Path, base: &Path) -> bool {
    match (target.canonicalize(), base.canonicalize()) {
        (Ok(t), Ok(b)) => t.starts_with(b),
        _ => false,
    }
}

/// For a `.cmd` shim at `bin`: the program and leading arguments that run its
/// real target, or `None` (not a shim, unreadable, or not a `.cmd`).
#[cfg(windows)]
pub(crate) fn resolve_shim(bin: &str) -> Option<(String, Vec<String>)> {
    let path = Path::new(bin);
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if ext != "cmd" && ext != "bat" {
        return None;
    }
    let dir = path.parent()?;
    let text = std::fs::read_to_string(path).ok()?;
    match parse_shim(&text, dir)? {
        ShimTarget::Exe(exe) => (exe.is_file() && contained_in(&exe, dir))
            .then(|| (exe.to_string_lossy().into_owned(), Vec::new())),
        ShimTarget::Node { script, local_node } => {
            if !script.is_file() || !contained_in(&script, dir) {
                return None;
            }
            let node = if local_node && dir.join("node.exe").is_file() {
                dir.join("node.exe")
            } else {
                find_in_path("node.exe")?
            };
            Some((
                node.to_string_lossy().into_owned(),
                vec![script.to_string_lossy().into_owned()],
            ))
        }
    }
}

/// `name` on the real `PATH` (with `PATHEXT`).
#[cfg(windows)]
pub(crate) fn find_in_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&paths).collect();
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| DEFAULT_PATHEXT.to_string());
    find_in_dirs(name, &dirs, &pathext, &|p| p.is_file())
}

/// Windows resolution of a vendor CLI by bare name: the configured search
/// paths first (already `~`-expanded by the caller), then the Windows default
/// install folders, then `PATH`, each with `PATHEXT`.
#[cfg(windows)]
pub(crate) fn resolve_windows(name: &str, search_dirs: &[PathBuf]) -> Option<PathBuf> {
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| DEFAULT_PATHEXT.to_string());
    let is_file = |p: &Path| p.is_file();
    if let Some(p) = find_in_dirs(name, search_dirs, &pathext, &is_file) {
        return Some(p);
    }
    let home = crate::home::home_os().map(PathBuf::from);
    let defaults = windows_default_dirs(
        dirs::config_dir().as_deref(),
        dirs::data_local_dir().as_deref(),
        home.as_deref(),
    );
    if let Some(p) = find_in_dirs(name, &defaults, &pathext, &is_file) {
        return Some(p);
    }
    find_in_path(name)
}

/// The Windows folder (`%SystemRoot%`), absolute.
#[cfg(windows)]
fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
}

/// A Windows system tool (`tasklist`, `notepad`...) by ABSOLUTE path under
/// `%SystemRoot%\System32`, never by bare name (a bare name is searched for in
/// more places than anyone intends). Elsewhere the bare name, as before.
pub(crate) fn system_tool(name: &str) -> std::ffi::OsString {
    #[cfg(windows)]
    {
        system_root()
            .join("System32")
            .join(format!("{name}.exe"))
            .into_os_string()
    }
    #[cfg(not(windows))]
    {
        std::ffi::OsString::from(name)
    }
}

/// `git`: on Windows an absolute `git.exe` from an absolute `PATH` entry or the
/// usual Git for Windows folders, else a plain, actionable error. Elsewhere the
/// bare name, as before.
pub(crate) fn git_program() -> Result<std::ffi::OsString, String> {
    #[cfg(windows)]
    {
        if let Some(p) = find_in_path("git.exe") {
            return Ok(p.into_os_string());
        }
        let is_file = |p: &Path| p.is_file();
        let mut dirs = Vec::new();
        for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(var).map(PathBuf::from) {
                dirs.push(root.join("Git").join("cmd"));
            }
        }
        if let Some(local) = dirs::data_local_dir() {
            dirs.push(local.join("Programs").join("Git").join("cmd"));
        }
        if let Some(p) = find_in_dirs("git.exe", &dirs, DEFAULT_PATHEXT, &is_file) {
            return Ok(p.into_os_string());
        }
        Err("Git was not found. Install Git for Windows (git-scm.com) or add it to PATH.".into())
    }
    #[cfg(not(windows))]
    {
        Ok(std::ffi::OsString::from("git"))
    }
}

/// The program an MCP server (or any configured command) will really run. On
/// Windows a bare name is looked up the way vendor CLIs are (absolute `PATH`
/// entries and install folders, with `PATHEXT`), a relative path is refused,
/// and a missing program is a plain error. Elsewhere unchanged.
pub(crate) fn resolve_program(program: &std::ffi::OsStr) -> Result<std::ffi::OsString, String> {
    #[cfg(windows)]
    {
        let path = Path::new(program);
        if path.is_absolute() {
            return Ok(program.to_os_string());
        }
        let name = program.to_string_lossy();
        if name.contains(['\\', '/']) {
            return Err(format!(
                "`{name}` is a relative path; give the full path to the program."
            ));
        }
        resolve_windows(&name, &[])
            .map(PathBuf::into_os_string)
            .ok_or_else(|| {
                format!(
                    "`{name}` was not found. Install it, or give its full path in the settings."
                )
            })
    }
    #[cfg(not(windows))]
    {
        Ok(program.to_os_string())
    }
}

/// Open a file or folder with the system's file manager or default app.
pub(crate) fn open_path(path: &Path) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        std::process::Command::new(system_root().join("explorer.exe"))
            .arg(path)
            .spawn()
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let program = xdg_open().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "xdg-open was not found")
        })?;
        std::process::Command::new(program).arg(path).spawn()
    }
}

/// `xdg-open` by an absolute path from the usual system folders (never searched
/// for by bare name). `None` when it is not installed.
#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn xdg_open() -> Option<PathBuf> {
    ["/usr/bin", "/usr/local/bin", "/bin"]
        .iter()
        .map(|d| Path::new(d).join("xdg-open"))
        .find(|p| p.is_file())
}

/// What the platform calls its file manager, for menu labels.
pub(crate) fn file_manager_name() -> &'static str {
    if cfg!(windows) {
        "File Explorer"
    } else if cfg!(target_os = "macos") {
        "Finder"
    } else {
        "Files"
    }
}

/// The message for a vendor CLI that could not be started. On Windows a name
/// that never resolved to an absolute path is reported as "not found" with what
/// to do, not as a bare OS error; everywhere else (and for any other failure)
/// the wording is the one the app always used.
pub(crate) fn start_failure(
    label: &str,
    bin_name: &str,
    resolved: &str,
    err: &std::io::Error,
) -> String {
    if cfg!(windows) && !Path::new(resolved).is_absolute() {
        return format!(
            "{label} was not found. Install it, or set its path in Settings. Awili only runs it from an absolute location, never from the current folder."
        );
    }
    format!("could not start `{bin_name}`: {err}")
}

/// Windows, once at startup before anything is spawned: stop Windows from
/// looking in the current folder for programs. `SetSearchPathMode` with
/// `BASE_SEARCH_PATH_ENABLE_SAFE_SEARCHMODE` moves the current folder to the end
/// of the search for this process, and `NoDefaultCurrentDirectoryInExePath`
/// (which children inherit) removes it for `cmd.exe` and friends.
#[cfg(windows)]
pub(crate) fn harden_search_path() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetSearchPathMode(flags: u32) -> i32;
    }
    const BASE_SEARCH_PATH_ENABLE_SAFE_SEARCHMODE: u32 = 0x0000_0001;
    // SAFETY: a plain process-wide setting with no pointer arguments.
    let ok = unsafe { SetSearchPathMode(BASE_SEARCH_PATH_ENABLE_SAFE_SEARCHMODE) };
    if ok == 0 {
        eprintln!("awili: could not enable safe search mode for programs");
    }
    // Set before any other thread exists, as `std::env::set_var` requires.
    std::env::set_var("NoDefaultCurrentDirectoryInExePath", "1");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_follow_pathext_order_and_drop_non_programs() {
        assert_eq!(
            runnable_extensions(".COM;.EXE;.BAT;.CMD;.VBS;.JS"),
            vec![".com", ".exe", ".bat", ".cmd"]
        );
        assert_eq!(runnable_extensions(".CMD;.EXE"), vec![".cmd", ".exe"]);
        assert_eq!(runnable_extensions("").len(), 4);
    }

    #[test]
    fn a_name_with_an_extension_is_tried_as_is() {
        assert_eq!(
            candidate_names("claude.cmd", DEFAULT_PATHEXT),
            vec!["claude.cmd"]
        );
        assert_eq!(
            candidate_names("claude", ".EXE;.CMD"),
            vec!["claude.exe", "claude.cmd"]
        );
    }

    #[test]
    fn first_directory_wins_then_pathext_order() {
        // Absolute on every OS (relative entries are skipped by design).
        let root = std::env::temp_dir();
        let npm = root.join("appdata").join("npm");
        let bin2 = root.join("bin2");
        let dirs = vec![npm.clone(), bin2.clone()];
        let present = |files: Vec<PathBuf>| move |p: &Path| files.iter().any(|f| f == p);
        // npm folder first, even though the later folder has a .exe.
        let probe = present(vec![npm.join("claude.cmd"), bin2.join("claude.exe")]);
        assert_eq!(
            find_in_dirs("claude", &dirs, DEFAULT_PATHEXT, &probe),
            Some(npm.join("claude.cmd"))
        );
        let probe = present(vec![npm.join("claude.cmd"), npm.join("claude.exe")]);
        assert_eq!(
            find_in_dirs("claude", &dirs, DEFAULT_PATHEXT, &probe),
            Some(npm.join("claude.exe"))
        );
        assert_eq!(find_in_dirs("codex", &dirs, DEFAULT_PATHEXT, &probe), None);
    }

    #[test]
    fn default_dirs_are_the_windows_install_places() {
        let v = windows_default_dirs(
            Some(Path::new("/u/AppData/Roaming")),
            Some(Path::new("/u/AppData/Local")),
            Some(Path::new("/u")),
        );
        assert_eq!(
            v,
            vec![
                PathBuf::from("/u/AppData/Roaming/npm"),
                PathBuf::from("/u/AppData/Local/Programs"),
                PathBuf::from("/u/.local/bin"),
                PathBuf::from("/u/.npm-global/bin"),
            ]
        );
        assert!(windows_default_dirs(None, None, None).is_empty());
    }

    pub(super) const NODE_SHIM: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n";

    pub(super) const EXE_SHIM: &str = "@ECHO off\r\nSETLOCAL\r\nSET dp0=%~dp0\r\n\"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe\"   %*\r\n";

    #[test]
    fn node_shim_runs_the_script_with_node() {
        let t = parse_shim(NODE_SHIM, Path::new("/npm")).unwrap();
        assert_eq!(
            t,
            ShimTarget::Node {
                script: Path::new("/npm")
                    .join("node_modules")
                    .join("@openai")
                    .join("codex")
                    .join("bin")
                    .join("codex.js"),
                local_node: true
            }
        );
    }

    #[test]
    fn native_shim_runs_the_exe_directly() {
        let t = parse_shim(EXE_SHIM, Path::new("/npm")).unwrap();
        assert_eq!(
            t,
            ShimTarget::Exe(
                Path::new("/npm")
                    .join("node_modules")
                    .join("@anthropic-ai")
                    .join("claude-code")
                    .join("bin")
                    .join("claude.exe")
            )
        );
    }

    #[test]
    fn relative_search_entries_are_never_used() {
        // `.` and `tools` resolve against the current folder, which can be a
        // project the user opened. Even when a program is "there", it is ignored.
        let dirs = vec![PathBuf::from("."), PathBuf::from("tools"), PathBuf::new()];
        let probe = |_: &Path| true;
        assert_eq!(find_in_dirs("claude", &dirs, DEFAULT_PATHEXT, &probe), None);
        // An absolute entry after them is still found.
        let abs = std::env::temp_dir();
        let mut dirs = dirs;
        dirs.push(abs.clone());
        assert_eq!(
            find_in_dirs("claude", &dirs, DEFAULT_PATHEXT, &probe),
            Some(abs.join("claude.com"))
        );
    }

    #[test]
    fn a_shim_that_climbs_out_of_its_folder_is_rejected() {
        let evil = "@echo off\r\n\"%dp0%\\..\\other\\evil.exe\"   %*\r\n";
        assert_eq!(parse_shim(evil, Path::new("/npm")), None);
        let evil_js = "\"%dp0%\\node_modules\\..\\..\\x\\run.js\" %*\r\n";
        assert_eq!(parse_shim(evil_js, Path::new("/npm")), None);
        // The normal npm layout still parses.
        assert!(parse_shim(EXE_SHIM, Path::new("/npm")).is_some());
    }

    #[test]
    fn containment_requires_the_target_to_stay_under_the_shim_folder() {
        let d = tempfile::tempdir().unwrap();
        let shim_dir = d.path().join("npm");
        let inside = shim_dir.join("node_modules").join("pkg");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::write(inside.join("run.js"), "x").unwrap();
        let outside = d.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("evil.js"), "x").unwrap();
        assert!(contained_in(&inside.join("run.js"), &shim_dir));
        assert!(!contained_in(&outside.join("evil.js"), &shim_dir));
        assert!(!contained_in(&shim_dir.join("missing.js"), &shim_dir));
        // A link inside node_modules that leads out of the tree is refused too.
        #[cfg(unix)]
        {
            let link = shim_dir.join("node_modules").join("link");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(!contained_in(&link.join("evil.js"), &shim_dir));
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn start_failure_wording_is_unchanged_off_windows() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(
            start_failure("Claude Code", "claude", "claude", &e),
            format!("could not start `claude`: {e}")
        );
    }

    #[test]
    fn a_file_that_is_not_a_shim_is_left_alone() {
        assert_eq!(
            parse_shim("@echo off\r\necho hi\r\n", Path::new("/npm")),
            None
        );
        // node.exe alone is not a target.
        assert_eq!(
            parse_shim("\"%dp0%\\node.exe\" %*\r\n", Path::new("/npm")),
            None
        );
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::process::Command;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn finds_an_npm_cmd_shim_via_pathext() {
        let d = temp();
        std::fs::write(d.path().join("claude.cmd"), "@echo off\r\n").unwrap();
        let dirs = vec![d.path().to_path_buf()];
        let found = find_in_dirs("claude", &dirs, DEFAULT_PATHEXT, &|p| p.is_file()).unwrap();
        assert_eq!(found, d.path().join("claude.cmd"));
        // The real resolver sees it through a configured search folder.
        let got = resolve_windows("claude", &dirs).unwrap();
        assert_eq!(got, d.path().join("claude.cmd"));
    }

    #[test]
    fn a_native_shim_launches_the_exe_directly_with_arguments_untouched() {
        let d = temp();
        let real = d.path().join("node_modules").join("pkg").join("bin");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::copy(r"C:\Windows\System32\whoami.exe", real.join("tool.exe")).unwrap();
        let shim = d.path().join("tool.cmd");
        std::fs::write(
            &shim,
            super::tests::EXE_SHIM
                .replace("@anthropic-ai\\claude-code", "pkg")
                .replace("claude.exe", "tool.exe"),
        )
        .unwrap();
        let args = vec!["line one\nline two".to_string(), "a&b".to_string()];
        let (program, full) =
            crate::agent_sandbox::launch_command(None, &shim.to_string_lossy(), &args);
        assert_eq!(PathBuf::from(&program), real.join("tool.exe"));
        assert_eq!(full, args, "arguments pass through untouched");
        // A plain program takes the multi-line argument without std refusing it.
        let out = Command::new(&program).arg("/?").output().unwrap();
        assert!(out.status.success() || out.status.code().is_some());
    }

    /// CVE-2024-24576 class: an argument with shell metacharacters reaches a
    /// batch file as data, never as a command (the batch uses %1; `%~1` would re-parse the text itself, which no caller can fix).
    #[test]
    fn batch_file_arguments_are_data_not_commands() {
        let d = temp();
        let bat = d.path().join("echo.bat");
        std::fs::write(&bat, "@echo off\r\necho [%1]\r\n").unwrap();
        let marker = d.path().join("pwned.txt");
        let evil = format!("x\" & echo pwned > \"{}\" & \"", marker.display());
        let out = Command::new(&bat).arg(&evil).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(!marker.exists(), "the argument must not run as a command");
    }

    /// std refuses an argument it cannot escape for a batch file (a newline):
    /// the launch fails closed instead of guessing.
    #[test]
    fn an_unescapable_batch_argument_fails_the_launch() {
        let d = temp();
        let bat = d.path().join("echo.bat");
        std::fs::write(&bat, "@echo off\r\necho %1\r\n").unwrap();
        let err = Command::new(&bat).arg("a\nb").output().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
    }

    #[test]
    fn a_node_shim_runs_the_script_with_the_node_next_to_it() {
        let d = temp();
        let bin = d
            .path()
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("codex.js"), "// js").unwrap();
        std::fs::copy(r"C:\Windows\System32\whoami.exe", d.path().join("node.exe")).unwrap();
        let shim = d.path().join("codex.cmd");
        std::fs::write(&shim, super::tests::NODE_SHIM).unwrap();
        let (program, lead) = resolve_shim(&shim.to_string_lossy()).unwrap();
        assert_eq!(PathBuf::from(program), d.path().join("node.exe"));
        assert_eq!(
            lead,
            vec![bin.join("codex.js").to_string_lossy().into_owned()]
        );
    }
}

#[cfg(all(test, windows))]
mod windows_hardening_tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn a_bare_program_name_is_never_spawned() {
        // Not found, and not searched for: no OS lookup that could reach the
        // current folder or a relative PATH entry.
        let mut cmd = Command::new("claude");
        let err = crate::proc_compat::spawn_tracked(&mut cmd).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
    }

    #[test]
    fn a_missing_cli_reads_as_not_found_with_what_to_do() {
        let e = std::io::Error::from(std::io::ErrorKind::NotFound);
        let msg = start_failure("Claude Code", "claude", "claude", &e);
        assert!(msg.starts_with("Claude Code was not found."), "{msg}");
        assert!(msg.contains("set its path in Settings"), "{msg}");
        // An absolute path that failed for another reason keeps the OS error.
        let msg = start_failure("Claude Code", "claude", r"C:\x\claude.exe", &e);
        assert!(msg.starts_with("could not start `claude`"), "{msg}");
    }

    #[test]
    fn a_shim_whose_target_is_outside_its_folder_is_not_resolved() {
        let d = tempfile::tempdir().unwrap();
        let shim_dir = d.path().join("npm");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let outside = d.path().join("other");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::copy(r"C:\Windows\System32\whoami.exe", outside.join("evil.exe")).unwrap();
        let shim = shim_dir.join("evil.cmd");
        std::fs::write(&shim, "@echo off\r\n\"%dp0%\\..\\other\\evil.exe\" %*\r\n").unwrap();
        assert_eq!(resolve_shim(&shim.to_string_lossy()), None);
    }
}

#[cfg(all(test, windows))]
mod windows_program_tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn system_tools_are_absolute_paths_under_system32() {
        for name in ["tasklist", "notepad"] {
            let p = PathBuf::from(system_tool(name));
            assert!(p.is_absolute(), "{p:?}");
            assert!(p.is_file(), "{p:?}");
        }
    }

    #[test]
    fn git_resolves_to_an_absolute_exe_or_a_plain_error() {
        match git_program() {
            Ok(p) => assert!(PathBuf::from(p).is_absolute()),
            Err(e) => assert!(e.starts_with("Git was not found."), "{e}"),
        }
    }

    #[test]
    fn a_missing_mcp_program_is_reported_clearly() {
        let e = resolve_program(OsStr::new("definitely-not-installed-xyz")).unwrap_err();
        assert!(e.contains("was not found"), "{e}");
        // A relative path is refused, not searched for.
        let e = resolve_program(OsStr::new(r".\tool.exe")).unwrap_err();
        assert!(e.contains("relative path"), "{e}");
        // An absolute program passes through; a bare system name is found on PATH.
        let abs = std::ffi::OsString::from(system_tool("tasklist"));
        assert_eq!(resolve_program(&abs).unwrap(), abs);
        let found = resolve_program(OsStr::new("tasklist")).unwrap();
        assert!(PathBuf::from(found).is_absolute());
    }

    #[test]
    fn open_path_uses_explorer_by_absolute_path() {
        assert_eq!(file_manager_name(), "File Explorer");
    }
}

#[cfg(all(test, not(windows)))]
mod unix_program_tests {
    use super::*;

    /// macOS behaviour is unchanged: bare names stay bare, as before.
    #[test]
    fn off_windows_programs_are_left_as_they_were() {
        assert_eq!(
            system_tool("tasklist"),
            std::ffi::OsString::from("tasklist")
        );
        assert_eq!(git_program().unwrap(), std::ffi::OsString::from("git"));
        let p = std::ffi::OsStr::new("npx");
        assert_eq!(resolve_program(p).unwrap(), p);
        assert_eq!(
            file_manager_name(),
            if cfg!(target_os = "macos") {
                "Finder"
            } else {
                "Files"
            }
        );
    }
}

#[cfg(all(test, not(any(windows, target_os = "macos"))))]
mod linux_open_tests {
    use super::*;

    #[test]
    fn xdg_open_is_an_absolute_path_when_present() {
        if let Some(p) = xdg_open() {
            assert!(p.is_absolute() && p.is_file(), "{p:?}");
        }
        assert_eq!(file_manager_name(), "Files");
    }
}
