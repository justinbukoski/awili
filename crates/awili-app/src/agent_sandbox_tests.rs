//! rc.4 S7 proof tests: the seatbelt profile text, its deny rules, its
//! write-once file, the fail-closed `prepare`, the launch command, git, and
//! (macOS) real enforcement with `sandbox-exec`.

use super::*;
use awili_code::{Autonomy, CodeEngine};

const ENGINES: [CodeEngine; 4] = [
    CodeEngine::Claude,
    CodeEngine::Codex,
    CodeEngine::Glm,
    CodeEngine::Dsh,
];

static NO_GIT: GitDirs = GitDirs { dirs: Vec::new() };

fn inputs<'a>(
    folder: &'a Path,
    engine: CodeEngine,
    home: &'a Path,
    temp: &'a [PathBuf],
    extra: &'a [PathBuf],
) -> ProfileInputs<'a> {
    ProfileInputs {
        folder,
        engine,
        home,
        codex_home: home,
        data_dir: Path::new("/data/Awili"),
        temp_dirs: temp,
        extra,
        git: &NO_GIT,
    }
}

fn temp_dirs() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/private/tmp"),
        PathBuf::from("/private/var/folders"),
    ]
}

/// The allow part of a profile (before the deny section).
fn allow_part(text: &str) -> &str {
    text.split(";; Executable config").next().unwrap()
}

/// The deny part of a profile.
fn deny_part(text: &str) -> &str {
    text.split(";; Executable config").nth(1).unwrap()
}

#[test]
fn profile_allows_folder_temp_devices_library_caches_known_hosts_and_extras() {
    let temp = temp_dirs();
    let extra = [PathBuf::from("/Volumes/Scratch/out")];
    let text = profile_text(&inputs(
        Path::new("/Users/me/proj"),
        CodeEngine::Claude,
        Path::new("/Users/me"),
        &temp,
        &extra,
    ))
    .unwrap();
    assert!(text.starts_with("(version 1)\n"));
    assert!(text.contains("(allow default)\n(deny file-write*)\n"));
    let allow = allow_part(&text);
    for want in [
        "(subpath \"/Users/me/proj\")",
        "(subpath \"/private/tmp\")",
        "(subpath \"/private/var/folders\")",
        "(subpath \"/Volumes/Scratch/out\")",
        "(subpath \"/data/Awili/agents\")",
        "(subpath \"/Users/me/Library/Caches\")",
        "(literal \"/Users/me/.ssh/known_hosts\")",
        "(literal \"/dev/null\")",
        "(literal \"/dev/zero\")",
        "(literal \"/dev/tty\")",
        "(literal \"/dev/dtracehelper\")",
        "(subpath \"/dev/fd\")",
        "(regex \"^/dev/ttys[0-9]+$\")",
    ] {
        assert!(allow.contains(want), "missing {want} in\n{text}");
    }
    // Shared caches whose contents later run unsandboxed are NOT default.
    for gone in [".npm", ".cache", ".cargo"] {
        assert!(
            !text.contains(gone),
            "{gone} must not be writable by default"
        );
    }
    // Only that one ssh file, never the folder.
    assert!(!text.contains("(subpath \"/Users/me/.ssh"));
    // The network is never mentioned: only file writes are restricted.
    // Only unix sockets of other agent hosts; never the internet.
    for l in text.lines().filter(|l| l.contains("remote")) {
        assert!(l.contains("unix-socket"), "{l}");
    }
    assert!(!text.contains("tcp") && !text.contains("udp") && !text.contains("(remote ip"));
    assert!(!text.contains("deny file-read"));
    // The generated-profile folder is outside every allow rule and denied.
    assert!(!allow.contains("sandbox-profiles"));
    assert!(deny_part(&text).contains("(subpath \"/data/Awili/sandbox-profiles\")"));
}

#[test]
fn profile_names_each_engines_own_state() {
    let temp = temp_dirs();
    let home = Path::new("/Users/me");
    let text = |e| profile_text(&inputs(Path::new("/Users/me/p"), e, home, &temp, &[])).unwrap();
    let claude = text(CodeEngine::Claude);
    assert!(claude.contains("(subpath \"/Users/me/.claude\")"));
    assert!(claude.contains("(regex \"^/Users/me/\\\\.claude\\\\.json\")"));
    assert!(!allow_part(&claude).contains("/.zcode") && !allow_part(&claude).contains("/.dsh"));
    assert!(text(CodeEngine::Codex).contains("(subpath \"/Users/me/.codex\")"));
    assert!(text(CodeEngine::Glm).contains("(subpath \"/Users/me/.zcode\")"));
    assert!(text(CodeEngine::Dsh).contains("(subpath \"/Users/me/.dsh\")"));
    let mut i = inputs(
        Path::new("/Users/me/p"),
        CodeEngine::Codex,
        home,
        &temp,
        &[],
    );
    let ch = PathBuf::from("/Users/me/awili-codex-home");
    i.codex_home = &ch;
    let t = profile_text(&i).unwrap();
    assert!(t.contains("(subpath \"/Users/me/awili-codex-home\")"));
    assert!(t.contains("(subpath \"/Users/me/.codex\")"));
}

#[test]
fn profile_denies_executable_config_after_the_allow_rules() {
    let temp = temp_dirs();
    let home = Path::new("/Users/me");
    let deny = |e| {
        let t = profile_text(&inputs(Path::new("/Users/me/p"), e, home, &temp, &[])).unwrap();
        let (allow, deny) = (allow_part(&t).len(), deny_part(&t).to_string());
        assert!(
            t.find("(deny file-write*\n  (").unwrap() > allow - 5,
            "deny comes last"
        );
        deny
    };
    let c = deny(CodeEngine::Claude);
    for want in [
        "(regex \"^/Users/me/\\\\.claude/settings[^/]*\\\\.json$\")",
        "(subpath \"/Users/me/.claude/hooks\")",
        "(subpath \"/Users/me/.claude/commands\")",
        "(subpath \"/Users/me/.claude/skills\")",
        "(subpath \"/Users/me/.claude/agents\")",
        "(subpath \"/Users/me/.claude/plugins\")",
        "(literal \"/Users/me/.claude/CLAUDE.md\")",
    ] {
        assert!(c.contains(want), "claude: missing {want}\n{c}");
    }
    let x = deny(CodeEngine::Codex);
    for want in [
        "(literal \"/Users/me/.codex/config.toml\")",
        "(literal \"/Users/me/.codex/hooks.json\")",
        "(literal \"/Users/me/.codex/AGENTS.md\")",
        "(subpath \"/Users/me/.codex/rules\")",
    ] {
        assert!(x.contains(want), "codex: missing {want}\n{x}");
    }
    let z = deny(CodeEngine::Glm);
    for want in [
        "(literal \"/Users/me/.zcode/cli/config.json\")",
        "(literal \"/Users/me/.zcode/v2/config.json\")",
        "(subpath \"/Users/me/.zcode/cli/plugins\")",
        "(subpath \"/Users/me/.zcode/skills\")",
    ] {
        assert!(z.contains(want), "zcode: missing {want}\n{z}");
    }
    let d = deny(CodeEngine::Dsh);
    assert!(d.contains("(subpath \"/Users/me/.dsh/profiles/node_modules\")"));
    assert!(d.contains("cordis\\\\.patch\\\\.yml"), "{d}");
    assert!(
        !d.contains("cordis.yml"),
        "dsh regenerates cordis.yml itself"
    );
}

#[test]
fn git_hooks_and_config_are_denied_for_every_git_dir() {
    let temp = temp_dirs();
    let git = GitDirs {
        dirs: vec![
            PathBuf::from("/r/main/.git/worktrees/wt"),
            PathBuf::from("/r/main/.git"),
        ],
    };
    let mut i = inputs(
        Path::new("/r/wt"),
        CodeEngine::Claude,
        Path::new("/Users/me"),
        &temp,
        &[],
    );
    i.git = &git;
    let t = profile_text(&i).unwrap();
    for d in ["/r/main/.git/worktrees/wt", "/r/main/.git"] {
        assert!(allow_part(&t).contains(&format!("(subpath \"{d}\")")));
        let deny = deny_part(&t);
        assert!(deny.contains(&format!("(subpath \"{d}/hooks\")")), "{deny}");
        assert!(
            deny.contains(&format!("(literal \"{d}/config\")")),
            "{deny}"
        );
    }
}

#[test]
fn profile_escapes_quotes_and_backslashes() {
    let temp = temp_dirs();
    let folder = PathBuf::from("/Users/me/we\"ird\\dir");
    let text = profile_text(&inputs(
        &folder,
        CodeEngine::Dsh,
        Path::new("/Users/me"),
        &temp,
        &[],
    ))
    .unwrap();
    assert!(
        text.contains("(subpath \"/Users/me/we\\\"ird\\\\dir\")"),
        "{text}"
    );
    assert_eq!(sbpl_escape("a\"b\\c"), "a\\\"b\\\\c");
}

#[test]
fn profile_refuses_nul_newline_and_relative_paths() {
    let temp = temp_dirs();
    let home = Path::new("/Users/me");
    for bad in [
        "/Users/me/a\nb",
        "/Users/me/a\0b",
        "/Users/me/a\rb",
        "relative/x",
    ] {
        let folder = PathBuf::from(bad);
        let r = profile_text(&inputs(&folder, CodeEngine::Claude, home, &temp, &[]));
        assert!(r.is_err(), "{bad:?} must be refused");
        let extra = [PathBuf::from(bad)];
        let r = profile_text(&inputs(
            Path::new("/Users/me/p"),
            CodeEngine::Claude,
            home,
            &temp,
            &extra,
        ));
        assert!(r.is_err(), "{bad:?} as an extra path must be refused");
    }
}

#[test]
fn profile_is_deterministic_and_folder_specific() {
    let temp = temp_dirs();
    let home = Path::new("/Users/me");
    let a = |f: &str, e| profile_text(&inputs(Path::new(f), e, home, &temp, &[])).unwrap();
    assert_eq!(a("/x/a", CodeEngine::Claude), a("/x/a", CodeEngine::Claude));
    assert_ne!(a("/x/a", CodeEngine::Claude), a("/x/b", CodeEngine::Claude));
    for e in ENGINES {
        assert!(a("/x/a", e).contains("(subpath \"/x/a\")"));
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // only the Seatbelt tests use it
fn sh(dir: &Path, script: &str) {
    let ok = std::process::Command::new("/bin/sh")
        .current_dir(dir)
        .args(["-c", script])
        .status()
        .unwrap()
        .success();
    assert!(ok, "setup failed: {script}");
}

#[test]
fn git_dirs_for_a_plain_repo_a_subfolder_a_worktree_and_a_submodule() {
    let root = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    // Plain repository, and a folder below its root.
    let plain = base.join("plain");
    std::fs::create_dir_all(plain.join(".git")).unwrap();
    std::fs::create_dir_all(plain.join("a/b")).unwrap();
    assert_eq!(git_dirs(&plain).dirs, [plain.join(".git")]);
    assert_eq!(git_dirs(&plain.join("a/b")).dirs, [plain.join(".git")]);
    // Not in a repository at all.
    let loose = base.join("loose");
    std::fs::create_dir_all(&loose).unwrap();
    assert_eq!(git_dirs(&loose), GitDirs::default());
    // Linked worktree: its private gitdir AND the common dir; from a subfolder too.
    let gitdir = plain.join(".git/worktrees/wt");
    let wt = base.join("wt");
    std::fs::create_dir_all(&gitdir).unwrap();
    std::fs::create_dir_all(wt.join("deep/er")).unwrap();
    std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    let want = vec![gitdir.clone(), plain.join(".git")];
    assert_eq!(git_dirs(&wt).dirs, want);
    assert_eq!(git_dirs(&wt.join("deep/er")).dirs, want);
    // Submodule: a gitdir with no commondir is allowed as itself.
    let sub_git = plain.join(".git/modules/sub");
    let sub = base.join("sub");
    std::fs::create_dir_all(&sub_git).unwrap();
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join(".git"), "gitdir: ../plain/.git/modules/sub\n").unwrap();
    assert_eq!(git_dirs(&sub).dirs, [sub_git]);
}

#[test]
fn launch_command_is_unchanged_off_and_wrapped_on() {
    let args: Vec<String> = ["-p", "--dangerously-skip-permissions", "x"]
        .map(String::from)
        .to_vec();
    let (prog, out) = launch_command(None, "/bin/claude", &args);
    assert_eq!((prog.as_str(), &out), ("/bin/claude", &args));
    let (prog, out) = launch_command(Some(Path::new("/p/profile-1.sb")), "/bin/claude", &args);
    assert_eq!(prog, "/usr/bin/sandbox-exec");
    assert_eq!(&out[..3], ["-f", "/p/profile-1.sb", "/bin/claude"]);
    assert_eq!(&out[3..], &args[..]);
}

fn on() -> awili_config::SandboxConfig {
    awili_config::SandboxConfig {
        enabled: true,
        extra_writable: vec![],
    }
}

fn launch_paths(home: &Path) -> crate::agent_launch::LaunchPaths {
    crate::agent_launch::LaunchPaths {
        home: home.to_path_buf(),
        codex_home: home.join(".codex"),
    }
}

/// A scratch area: a fake home, a project folder and a data dir, all siblings
/// under one temp folder that is NOT inside any temp allow rule's reach
/// (tests that show a refusal build theirs under the crate folder).
struct Scratch {
    _root: tempfile::TempDir,
    home: PathBuf,
    work: PathBuf,
    data: PathBuf,
}

fn scratch_in(parent: Option<&str>) -> Scratch {
    let root = match parent {
        Some(p) => tempfile::tempdir_in(p).unwrap(),
        None => tempfile::tempdir().unwrap(),
    };
    let base = std::fs::canonicalize(root.path()).unwrap();
    let s = Scratch {
        home: base.join("home"),
        work: base.join("work"),
        data: base.join("data/Awili"),
        _root: root,
    };
    for d in [&s.home, &s.work] {
        std::fs::create_dir_all(d).unwrap();
    }
    s
}

#[test]
fn prepare_is_off_by_default_and_skips_the_planning_step() {
    let s = scratch_in(None);
    let paths = launch_paths(&s.home);
    let cwd = s.work.to_str().unwrap();
    let off = awili_config::SandboxConfig::default();
    assert!(!off.enabled, "default OFF");
    for e in ENGINES {
        assert_eq!(
            prepare(&off, e, Autonomy::Autonomous, cwd, Some(&paths), &s.data),
            Ok(None)
        );
        assert_eq!(
            prepare(&on(), e, Autonomy::AskFirst, cwd, Some(&paths), &s.data),
            Ok(None),
            "the read-only planning step is never wrapped"
        );
    }
    // Off needs no home at all.
    assert_eq!(
        prepare(
            &off,
            CodeEngine::Claude,
            Autonomy::Autonomous,
            cwd,
            None,
            &s.data
        ),
        Ok(None)
    );
    assert!(
        !s.data.exists(),
        "nothing is written when nothing is wrapped"
    );
}

#[cfg(target_os = "macos")] // Seatbelt profile; unavailable on Linux
#[test]
fn prepare_writes_a_profile_when_on_and_autonomous() {
    let s = scratch_in(None);
    let paths = launch_paths(&s.home);
    let mut cfg = on();
    cfg.extra_writable = vec!["~/extra-out".into(), "  ".into()];
    let p = prepare(
        &cfg,
        CodeEngine::Claude,
        Autonomy::Autonomous,
        s.work.to_str().unwrap(),
        Some(&paths),
        &s.data,
    )
    .unwrap()
    .expect("a profile");
    // Outside the writable agents/ tree.
    assert!(p.starts_with(s.data.join("sandbox-profiles")));
    assert!(!p.starts_with(s.data.join("agents")));
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains(&format!("(subpath \"{}\")", s.work.display())));
    assert!(text.contains(&format!("{}/extra-out", s.home.display())));
}

#[test]
fn prepare_fails_closed() {
    let s = scratch_in(None);
    let paths = launch_paths(&s.home);
    let cwd = s.work.to_str().unwrap();
    let go = |cfg: &awili_config::SandboxConfig, cwd: &str, p: Option<&_>, d: &Path| {
        prepare(cfg, CodeEngine::Claude, Autonomy::Autonomous, cwd, p, d)
    };
    let sorry = |r: Result<Option<PathBuf>, String>, why: &str| {
        let e = r.unwrap_err();
        assert!(e.starts_with("Couldn't start the sandbox:"), "{why}: {e}");
    };
    // sandbox-exec missing.
    sorry(
        prepare_with(
            &s.work.join("no-such-sandbox-exec"),
            &on(),
            CodeEngine::Claude,
            Autonomy::Autonomous,
            cwd,
            Some(&paths),
            &s.data,
        ),
        "exec missing",
    );
    // Working folder gone.
    sorry(
        go(
            &on(),
            s.work.join("gone").to_str().unwrap(),
            Some(&paths),
            &s.data,
        ),
        "folder gone",
    );
    // No home folder known.
    sorry(go(&on(), cwd, None, &s.data), "no home");
    // Filesystem root, the home folder, and a folder above home.
    sorry(go(&on(), "/", Some(&paths), &s.data), "root");
    sorry(
        go(&on(), s.home.to_str().unwrap(), Some(&paths), &s.data),
        "home",
    );
    let above = s.home.parent().unwrap();
    sorry(
        go(&on(), above.to_str().unwrap(), Some(&paths), &s.data),
        "above home",
    );
    // The same for extra_writable entries.
    for bad in [
        "/".to_string(),
        s.home.display().to_string(),
        "~".to_string(),
        above.display().to_string(),
    ] {
        let mut cfg = on();
        cfg.extra_writable = vec![bad.clone()];
        sorry(go(&cfg, cwd, Some(&paths), &s.data), &bad);
    }
    // A newline in an extra path refuses the turn (never drops the sandbox).
    let mut cfg = on();
    cfg.extra_writable = vec!["/tmp/a\nb".into()];
    sorry(go(&cfg, cwd, Some(&paths), &s.data), "newline");
    // The profile folder cannot be created (a file is in the way).
    let blocker = s.work.join("blocker");
    std::fs::write(&blocker, "x").unwrap();
    sorry(
        go(&on(), cwd, Some(&paths), &blocker.join("Awili")),
        "blocked",
    );
}

#[cfg(unix)]
#[test]
fn concurrent_writes_of_one_profile_are_safe_and_content_addressed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("sandbox-profiles");
    // The folder already exists with loose permissions: it is tightened.
    std::fs::create_dir(&out).unwrap();
    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755)).unwrap();
    let text = "(version 1)\n(allow default)\n";
    let paths: Vec<PathBuf> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..16)
            .map(|_| s.spawn(|| write_profile(&out, text).unwrap()))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(
        std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(paths.iter().all(|p| *p == paths[0]));
    assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), text);
    let names: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    let other = write_profile(&out, "(version 1)\n(allow default)\n(deny file-write*)\n").unwrap();
    assert_ne!(other, paths[0]);
    assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), text);
}

#[cfg(target_os = "macos")] // Seatbelt profile; unavailable on Linux
#[test]
fn different_folders_give_different_profile_files() {
    let s = scratch_in(None);
    let paths = launch_paths(&s.home);
    let a = s.work.join("a");
    let b = s.work.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let go = |f: &Path| {
        prepare(
            &on(),
            CodeEngine::Dsh,
            Autonomy::Autonomous,
            f.to_str().unwrap(),
            Some(&paths),
            &s.data,
        )
        .unwrap()
        .unwrap()
    };
    assert_ne!(go(&a), go(&b));
    assert_eq!(go(&a), go(&a), "the same turn reuses its file");
}

#[test]
fn status_line_reads_both_ways() {
    assert_eq!(status_line(false, "/x/proj"), "Sandbox: off");
    assert_eq!(
        status_line(true, "/x/proj/"),
        "Sandbox: on \u{2014} writes limited to proj"
    );
    assert_eq!(
        status_line(true, "/"),
        "Sandbox: on \u{2014} writes limited to the working folder"
    );
}

// ---- real enforcement (macOS, no network, no model) ------------------------------

/// Run `script` under `profile` with `/bin/sh`; true when it exited 0.
#[cfg(target_os = "macos")]
fn sandboxed_ok(profile: &Path, dir: &Path, script: &str) -> bool {
    std::process::Command::new(SANDBOX_EXEC)
        .arg("-f")
        .arg(profile)
        .args(["/bin/sh", "-c", script])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap()
        .status
        .success()
}

/// Scratch under the crate folder, which no profile opens (a temp folder is
/// writable, so it cannot show a refusal).
#[cfg(target_os = "macos")]
fn scratch_enforce() -> Scratch {
    scratch_in(Some(env!("CARGO_MANIFEST_DIR")))
}

#[cfg(target_os = "macos")]
fn prepare_for(s: &Scratch, e: CodeEngine, cwd: &Path, extra: &[&str]) -> PathBuf {
    let mut cfg = on();
    cfg.extra_writable = extra.iter().map(|x| x.to_string()).collect();
    prepare(
        &cfg,
        e,
        Autonomy::Autonomous,
        cwd.to_str().unwrap(),
        Some(&launch_paths(&s.home)),
        &s.data,
    )
    .unwrap()
    .unwrap()
}

#[cfg(target_os = "macos")]
#[test]
fn seatbelt_lets_the_folder_be_written_and_refuses_outside() {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return;
    }
    let s = scratch_enforce();
    let outside = s.work.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let profile = prepare_for(&s, CodeEngine::Claude, &s.work, &[]);
    let w = |p: &Path| format!("echo x > '{}'", p.display());
    assert!(sandboxed_ok(&profile, &s.work, &w(&s.work.join("in"))));
    assert!(!sandboxed_ok(&profile, &s.work, &w(&outside.join("out"))));
    assert!(!outside.join("out").exists());
    assert_eq!(std::fs::read_to_string(s.work.join("in")).unwrap(), "x\n");
    // Temp is writable; reads of outside files still work.
    assert!(sandboxed_ok(
        &profile,
        &s.work,
        "echo x > /private/tmp/awili-s7-probe && rm /private/tmp/awili-s7-probe"
    ));
    std::fs::write(outside.join("r"), "seen").unwrap();
    assert!(sandboxed_ok(
        &profile,
        &s.work,
        &format!("test \"$(cat '{}/r')\" = seen", outside.display())
    ));
}

/// The generated profile folder is not writable under its own profile, even
/// when the working folder is the folder that contains it.
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_generated_profiles_are_not_writable_under_their_own_profile() {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return;
    }
    let s = scratch_enforce();
    // Worst case: the data dir is inside the working folder.
    let inside = Scratch {
        data: s.work.join("Awili"),
        ..scratch_in(None)
    };
    let profile = prepare_for(&inside, CodeEngine::Dsh, &s.work, &[]);
    let dir = profile.parent().unwrap();
    assert!(dir.starts_with(&s.work) || dir.starts_with(&inside.data));
    assert!(!sandboxed_ok(
        &profile,
        &s.work,
        &format!("echo x >> '{}'", profile.display())
    ));
    assert!(!sandboxed_ok(
        &profile,
        &s.work,
        &format!("echo x > '{}/new.sb'", dir.display())
    ));
    assert!(!sandboxed_ok(
        &profile,
        &s.work,
        &format!("rm -f '{}'", profile.display())
    ));
    assert!(profile.exists(), "the profile is still there");
    // The folder around it stays writable.
    assert!(sandboxed_ok(&profile, &s.work, "echo x > ok"));
}

/// Deny rules beat the allow rules for the executable-config files of every
/// engine, using a fake home.
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_denies_executable_config_but_not_the_rest_of_the_state() {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return;
    }
    let s = scratch_enforce();
    // (engine, paths that must be writable, paths that must be refused)
    let cases: [(CodeEngine, &[&str], &[&str]); 4] = [
        (
            CodeEngine::Claude,
            &[
                ".claude/projects/p",
                ".claude/todo.json",
                ".claude.json",
                ".claude.json.tmp.1",
                "Library/Caches/c",
                ".ssh/known_hosts",
            ],
            &[
                ".claude/settings.json",
                ".claude/settings.local.json",
                ".claude/hooks/x",
                ".claude/commands/x",
                ".claude/skills/x",
                ".claude/agents/x",
                ".claude/plugins/x",
                ".claude/CLAUDE.md",
                ".ssh/id_ed25519",
                ".ssh/config",
                ".npm/x",
                ".cache/x",
                ".cargo/registry/x",
                ".zshrc",
            ],
        ),
        (
            CodeEngine::Codex,
            &[
                ".codex/sessions/s",
                ".codex/history.jsonl",
                ".codex/auth.json",
            ],
            &[
                ".codex/config.toml",
                ".codex/hooks.json",
                ".codex/AGENTS.md",
                ".codex/rules/x",
            ],
        ),
        (
            CodeEngine::Glm,
            &[".zcode/cli/exec/x", ".zcode/v2/logs/x", ".zcode/cli/db/x"],
            &[
                ".zcode/cli/config.json",
                ".zcode/v2/config.json",
                ".zcode/cli/plugins/x",
                ".zcode/skills/x",
                ".zcode/AGENTS.md",
            ],
        ),
        (
            CodeEngine::Dsh,
            &[
                ".dsh/sessions/s",
                ".dsh/.anonymous-user-id",
                ".dsh/profiles/headless/cordis.yml",
            ],
            &[
                ".dsh/profiles/headless/cordis.patch.yml",
                ".dsh/profiles/headless/package.json",
                ".dsh/profiles/headless/pnpm-workspace.yaml",
                ".dsh/profiles/headless/node_modules/m",
                ".dsh/profiles/node_modules/m",
            ],
        ),
    ];
    for (engine, ok, no) in cases {
        for rel in ok.iter().chain(no.iter()) {
            std::fs::create_dir_all(s.home.join(rel).parent().unwrap()).unwrap();
        }
        let profile = prepare_for(&s, engine, &s.work, &[]);
        for rel in ok {
            let p = s.home.join(rel);
            assert!(
                sandboxed_ok(&profile, &s.work, &format!("echo x > '{}'", p.display())),
                "{engine:?}: {rel} must stay writable"
            );
        }
        for rel in no {
            let p = s.home.join(rel);
            assert!(
                !sandboxed_ok(&profile, &s.work, &format!("echo x > '{}'", p.display())),
                "{engine:?}: {rel} must be refused"
            );
            assert!(
                !p.exists() || std::fs::read_to_string(&p).unwrap().is_empty(),
                "{rel}"
            );
        }
    }
}

/// A symlinked state dir is followed (writes work, denies hold), and a
/// symlinked instruction file can be neither written through nor replaced.
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_follows_symlinked_state_dirs_and_guards_symlinked_files() {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return;
    }
    let s = scratch_enforce();
    let base = s.home.parent().unwrap().to_path_buf();
    let real = base.join("real-claude");
    std::fs::create_dir_all(real.join("hooks")).unwrap();
    std::os::unix::fs::symlink(&real, s.home.join(".claude")).unwrap();
    let target_md = base.join("shared-CLAUDE.md");
    std::fs::write(&target_md, "rules\n").unwrap();
    std::os::unix::fs::symlink(&target_md, real.join("CLAUDE.md")).unwrap();
    let profile = prepare_for(&s, CodeEngine::Claude, &s.work, &[]);
    let text = std::fs::read_to_string(&profile).unwrap();
    assert!(
        text.contains(&format!("(subpath \"{}\")", real.display())),
        "{text}"
    );
    let c = s.home.join(".claude");
    assert!(sandboxed_ok(
        &profile,
        &s.work,
        &format!("echo x > '{}/projects.json'", c.display())
    ));
    assert!(!sandboxed_ok(
        &profile,
        &s.work,
        &format!("echo x > '{}/settings.json'", c.display())
    ));
    assert!(!sandboxed_ok(
        &profile,
        &s.work,
        &format!("echo x > '{}/hooks/h'", c.display())
    ));
    assert!(
        !sandboxed_ok(
            &profile,
            &s.work,
            &format!("echo x > '{}/CLAUDE.md'", c.display())
        ),
        "through the link"
    );
    assert!(
        !sandboxed_ok(
            &profile,
            &s.work,
            &format!("echo x > '{}'", target_md.display())
        ),
        "the target itself"
    );
    assert!(
        !sandboxed_ok(
            &profile,
            &s.work,
            &format!("rm -f '{}/CLAUDE.md'", c.display())
        ),
        "replacing the link"
    );
    assert_eq!(std::fs::read_to_string(&target_md).unwrap(), "rules\n");
    assert!(std::fs::symlink_metadata(real.join("CLAUDE.md"))
        .unwrap()
        .file_type()
        .is_symlink());
}

/// A working folder given as a symlink is canonicalised: the real folder is
/// writable (through either spelling), nothing else is.
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_follows_a_symlinked_working_folder() {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return;
    }
    let s = scratch_enforce();
    let base = s.home.parent().unwrap().to_path_buf();
    let link = base.join("work-link");
    std::os::unix::fs::symlink(&s.work, &link).unwrap();
    let other = base.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let profile = prepare_for(&s, CodeEngine::Glm, &link, &[]);
    let text = std::fs::read_to_string(&profile).unwrap();
    assert!(text.contains(&format!("(subpath \"{}\")", s.work.display())));
    assert!(!text.contains("work-link"));
    assert!(sandboxed_ok(&profile, &link, "echo x > via-link"));
    assert!(sandboxed_ok(
        &profile,
        &s.work,
        &format!("echo x > '{}/via-link2'", link.display())
    ));
    assert!(s.work.join("via-link").exists() && s.work.join("via-link2").exists());
    assert!(!sandboxed_ok(
        &profile,
        &link,
        &format!("echo x > '{}/no'", other.display())
    ));
}

/// A real repository with a linked worktree: `git commit` inside a subfolder
/// of the worktree works under the generated profile; planting a hook or
/// editing the config does not, in the worktree's repo or the main one.
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_git_commit_in_a_linked_worktree_works_but_hooks_and_config_do_not() {
    if !Path::new(SANDBOX_EXEC).is_file() || !Path::new("/usr/bin/git").is_file() {
        return;
    }
    let s = scratch_enforce();
    let base = s.home.parent().unwrap().to_path_buf();
    let main = base.join("main");
    let wt = base.join("wt");
    std::fs::create_dir_all(&main).unwrap();
    let id = "-c user.name=t -c user.email=t@example.com";
    sh(&main, &format!("git init -q . && echo a > a && git add a && git {id} commit -q -m first && git worktree add -q -b side '{}'", wt.display()));
    let sub = wt.join("deep");
    std::fs::create_dir_all(&sub).unwrap();
    let profile = prepare_for(&s, CodeEngine::Claude, &sub, &[]);
    let text = std::fs::read_to_string(&profile).unwrap();
    assert!(
        text.contains(&format!("{}/.git/worktrees/wt", main.display())),
        "{text}"
    );
    assert!(
        sandboxed_ok(&profile, &sub, &format!("echo c > c && git add c && git {id} commit -q -m second && git {id} commit -q --allow-empty -m third")),
        "commit under the profile"
    );
    let log = std::process::Command::new("git")
        .args(["-C"])
        .arg(&wt)
        .args(["log", "--oneline"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&log.stdout).lines().count(),
        3,
        "first + two new"
    );
    let common = main.join(".git");
    for p in [
        common.join("hooks/pre-commit"),
        common.join("hooks/x"),
        common.join("worktrees/wt/hooks/x"),
    ] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        assert!(
            !sandboxed_ok(
                &profile,
                &sub,
                &format!("echo '#!/bin/sh' > '{}'", p.display())
            ),
            "{}",
            p.display()
        );
        assert!(!p.exists() || std::fs::read_to_string(&p).unwrap().is_empty());
    }
    let before = std::fs::read_to_string(common.join("config")).unwrap();
    assert!(!sandboxed_ok(
        &profile,
        &sub,
        &format!("echo '[alias]' >> '{}/config'", common.display())
    ));
    assert!(!sandboxed_ok(
        &profile,
        &sub,
        "git config core.hooksPath /tmp/evil"
    ));
    assert_eq!(
        std::fs::read_to_string(common.join("config")).unwrap(),
        before
    );
    // The plain (non-worktree) repo: its own hooks/config are refused too.
    let p2 = prepare_for(&s, CodeEngine::Claude, &main, &[]);
    assert!(sandboxed_ok(
        &p2,
        &main,
        &format!("echo d > d && git add d && git {id} commit -q -m fourth")
    ));
    assert!(!sandboxed_ok(&p2, &main, "echo x > .git/hooks/post-commit"));
    assert!(!sandboxed_ok(&p2, &main, "echo x >> .git/config"));
}

/// The extra-writable folder really opens up (and only that one).
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_extra_writable_opens_exactly_that_folder() {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return;
    }
    let s = scratch_enforce();
    let base = s.home.parent().unwrap().to_path_buf();
    let (extra, other) = (base.join("x"), base.join("o"));
    std::fs::create_dir_all(&extra).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let profile = prepare_for(&s, CodeEngine::Dsh, &s.work, &[extra.to_str().unwrap()]);
    assert!(sandboxed_ok(
        &profile,
        &s.work,
        &format!("echo 1 > '{}/ok'", extra.display())
    ));
    assert!(!sandboxed_ok(
        &profile,
        &s.work,
        &format!("echo 2 > '{}/no'", other.display())
    ));
    // Extra folders are not an exception to the profile folder or the denies.
    assert!(!sandboxed_ok(
        &profile,
        &s.work,
        &format!("echo 3 > '{}/.claude/settings.json'", s.home.display())
    ));
}

/// Leaf denies are not enough: moving the parent aside, editing there and
/// moving it back must fail too, for a state dir, a `.git` and the working
/// folder itself, while ordinary work in them still works.
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_refuses_the_rename_round_trip_but_not_normal_work() {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return;
    }
    let s = scratch_enforce();
    let base = s.home.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(s.home.join(".claude/hooks")).unwrap();
    std::fs::write(s.home.join(".claude/settings.json"), "{}").unwrap();
    std::fs::create_dir_all(s.work.join(".git/hooks")).unwrap();
    std::fs::write(s.work.join(".git/config"), "[core]\n").unwrap();
    let profile = prepare_for(&s, CodeEngine::Claude, &s.work, &[]);
    let parked = s.work.join("parked");
    let tmp = format!("/private/tmp/awili-s7-rt-{}", std::process::id());
    let ok = |script: &str| sandboxed_ok(&profile, &s.work, script);
    let claude = s.home.join(".claude");
    // The state dir cannot be moved into the folder or temp.
    assert!(!ok(&format!(
        "mv '{}' '{}'",
        claude.display(),
        parked.display()
    )));
    assert!(!ok(&format!("mv '{}' '{}'", claude.display(), tmp)));
    assert!(!ok(&format!("rm -rf '{}'", claude.display())));
    // `mv` falls back to copy + delete: the copy is harmless (reads are open);
    // what matters is that the original stays where it is.
    let clean = || {
        let _ = std::fs::remove_dir_all(&parked);
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(base.join("gone"));
    };
    assert!(claude.join("settings.json").exists() && claude.join("hooks").is_dir());
    clean();
    // Same for .git, and for a swap: move another dir over it.
    assert!(!ok(&format!("mv .git '{}'", parked.display())));
    assert!(!ok(&format!("mv .git '{}'", tmp)));
    assert!(!ok("rm -rf .git"));
    assert!(!ok("mkdir other && mv other .git2 && mv .git .git3"));
    assert!(s.work.join(".git/hooks").is_dir() && s.work.join(".git/config").exists());
    clean();
    // And the folder root itself.
    assert!(!ok(&format!("mv '{}' '{}'", s.work.display(), tmp)));
    assert!(!ok(&format!(
        "mv '{}' '{}/gone'",
        s.work.display(),
        base.display()
    )));
    assert!(s.work.join(".git/config").exists() && s.work.is_dir());
    clean();
    // Normal work is fine: create, edit, rename, delete files and folders;
    // state files and subfolders of the state dir too.
    assert!(ok(
        "mkdir -p a/b && echo x > a/b/f && mv a/b/f a/g && mv a c && rm -rf c && echo y > top"
    ));
    assert!(ok("mkdir sub && rm -rf sub"));
    assert!(sandboxed_ok(
        &profile,
        &s.work,
        &format!("mkdir '{c}/projects' && echo x > '{c}/projects/p' && mv '{c}/projects/p' '{c}/projects/q' && rm -rf '{c}/projects'", c = claude.display())
    ));
    assert!(ok("echo x > .git/index.lock && mv .git/index.lock .git/index && mkdir -p .git/refs/heads && echo h > .git/refs/heads/x"));
}

/// Hooks and config below the top of a git dir (submodule gitdirs, linked
/// worktree gitdirs) and in repositories nested under the folder are denied
/// at any depth, whether or not the repository existed when the turn began.
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_denies_git_hooks_and_config_at_any_depth() {
    if !Path::new(SANDBOX_EXEC).is_file() {
        return;
    }
    let s = scratch_enforce();
    let g = s.work.join(".git");
    for d in [
        "hooks",
        "modules/sub/hooks",
        "worktrees/wt",
        "modules/sub/modules/deep/hooks",
    ] {
        std::fs::create_dir_all(g.join(d)).unwrap();
    }
    let nested = s.work.join("vendor/lib");
    std::fs::create_dir_all(nested.join(".git/hooks")).unwrap();
    let profile = prepare_for(&s, CodeEngine::Claude, &s.work, &[]);
    let text = std::fs::read_to_string(&profile).unwrap();
    assert!(
        text.contains(&format!("{}/vendor/lib/.git", s.work.display())),
        "a nested repo found at start is a git dir"
    );
    let ok = |script: &str| sandboxed_ok(&profile, &s.work, script);
    for refused in [
        ".git/hooks/pre-commit",
        ".git/modules/sub/hooks/pre-commit",
        ".git/modules/sub/config",
        ".git/modules/sub/modules/deep/hooks/post-commit",
        ".git/worktrees/wt/config.worktree",
        ".git/worktrees/wt/hooks/x",
        "vendor/lib/.git/hooks/pre-commit",
        "vendor/lib/.git/config",
    ] {
        assert!(!ok(&format!("echo x > '{refused}'")), "{refused}");
        assert!(!s.work.join(refused).exists(), "{refused}");
    }
    // A repository created during the turn is covered by the folder-wide patterns.
    assert!(
        ok("mkdir -p fresh/.git/refs"),
        "creating a repo is normal work"
    );
    std::fs::create_dir_all(s.work.join("fresh/.git/hooks")).unwrap();
    assert!(!ok("echo x > fresh/.git/hooks/pre-commit"));
    assert!(!ok("echo x >> fresh/.git/config"));
    // Nested .git folders cannot be moved aside; their parents (known at start) cannot either.
    assert!(!ok("mv vendor/lib/.git vendor/lib/.gitx"));
    assert!(!ok("mv vendor/lib vendor/lib2"));
    assert!(!ok(".git/modules/sub"));
    assert!(!ok("mv .git/modules/sub .git/modules/sub2"));
    // Ordinary files next to them are fine.
    assert!(ok(
        "echo x > vendor/lib/code.rs && echo y > .git/HEAD2 && echo z > .git/modules/sub/HEAD"
    ));
}

/// Other agent hosts' unix sockets cannot be connected to from inside the
/// sandbox (the Codex desktop app's `ipc.sock` is how a sandboxed Codex
/// reached an unsandboxed host), while an ordinary socket still can. Uses a
/// real listener and `/usr/bin/nc -U`; no network.
#[cfg(target_os = "macos")]
#[test]
fn seatbelt_refuses_unix_sockets_of_other_agent_hosts() {
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    if !Path::new(SANDBOX_EXEC).is_file() || !Path::new("/usr/bin/nc").is_file() {
        return;
    }
    let s = scratch_enforce();
    // Short names: a unix socket path is limited to about 100 bytes.
    let short = tempfile::tempdir_in("/private/tmp").unwrap();
    let st = std::fs::canonicalize(short.path()).unwrap();
    let temp_agent = std::env::temp_dir().join(format!("codex-s7-{}", std::process::id()));
    std::fs::create_dir_all(&temp_agent).unwrap();
    let ok_sock = st.join("ok.sock");
    let denied: Vec<(CodeEngine, PathBuf)> = vec![
        (CodeEngine::Codex, s.home.join(".codex/ipc/ipc.sock")),
        (CodeEngine::Claude, s.home.join(".claude/h.sock")),
        (CodeEngine::Codex, st.join("tmux-501/default")),
        (CodeEngine::Claude, st.join("vscode-ipc-1234.sock")),
        (CodeEngine::Glm, st.join("windsurf-git-1.sock")),
        (CodeEngine::Dsh, s.home.join(".colima/docker.sock")),
        (CodeEngine::Codex, s.home.join(".mac-shell/daemon.sock")),
        (CodeEngine::Claude, s.home.join(".mac-shell/daemon.sock")),
        (CodeEngine::Glm, s.home.join(".zcode/z.sock")),
        (CodeEngine::Dsh, s.home.join(".dsh/d.sock")),
        (CodeEngine::Claude, st.join("cc-socks/1.sock")),
        (CodeEngine::Codex, st.join("codex-daemon-501/c.sock")),
        (
            CodeEngine::Glm,
            canon_lenient(&temp_agent).join("host.sock"),
        ),
    ];
    for (_, p) in &denied {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    }
    let accepted = std::sync::Arc::new(AtomicUsize::new(0));
    let serve = |p: &Path| {
        let l = UnixListener::bind(p).unwrap();
        let n = accepted.clone();
        std::thread::spawn(move || {
            while l.accept().is_ok() {
                n.fetch_add(1, Ordering::SeqCst);
            }
        });
    };
    serve(&ok_sock);
    let mut served: Vec<&PathBuf> = Vec::new();
    for (_, p) in &denied {
        if !served.contains(&p) {
            served.push(p);
            serve(p);
        }
    }
    let connect = |profile: &Path, sock: &Path| {
        sandboxed_ok(
            profile,
            &s.work,
            &format!("/usr/bin/nc -U -w 1 '{}' < /dev/null", sock.display()),
        )
    };
    for (engine, sock) in &denied {
        let profile = prepare_for(&s, *engine, &s.work, &[]);
        let before = accepted.load(Ordering::SeqCst);
        assert!(
            !connect(&profile, sock),
            "{engine:?}: {} must be refused",
            sock.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            before,
            "nothing reached the listener"
        );
        // An ordinary unix socket and the loopback network still work.
        let before = accepted.load(Ordering::SeqCst);
        assert!(
            connect(&profile, &ok_sock),
            "{engine:?}: ordinary socket stays open"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(accepted.load(Ordering::SeqCst), before + 1);
    }
    let _ = std::fs::remove_dir_all(&temp_agent);
}

/// Live-probe helper (not run by default): writes the real generated profile
/// for every engine into `$AWILI_SBX_PROBE_OUT`, for folder
/// `$AWILI_SBX_PROBE_FOLDER` and the real home, so a hand-run
/// `sandbox-exec -f <profile> <agent>` uses exactly what Awili would.
#[test]
#[ignore = "live probe helper: needs AWILI_SBX_PROBE_OUT and AWILI_SBX_PROBE_FOLDER"]
fn probe_dump_profiles() {
    let out = PathBuf::from(std::env::var("AWILI_SBX_PROBE_OUT").unwrap());
    let folder = std::env::var("AWILI_SBX_PROBE_FOLDER").unwrap();
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let paths = launch_paths(&home);
    let data = out.join("Awili");
    for (name, e) in [
        ("claude", CodeEngine::Claude),
        ("codex", CodeEngine::Codex),
        ("glm", CodeEngine::Glm),
        ("dsh", CodeEngine::Dsh),
    ] {
        let p = prepare(&on(), e, Autonomy::Autonomous, &folder, Some(&paths), &data)
            .unwrap()
            .unwrap();
        std::fs::copy(&p, out.join(format!("{name}.sb"))).unwrap();
    }
}

/// Live probe (not run by default): one real Codex app-server turn through
/// Awili's own `codex_app_server::run_turn`, started under the generated
/// profile in `$AWILI_SBX_PROBE_PROFILE`. The caller checks the files.
#[test]
#[ignore = "live probe: spends a Codex luna turn"]
fn probe_codex_app_server_under_sandbox() {
    let profile = PathBuf::from(std::env::var("AWILI_SBX_PROBE_PROFILE").unwrap());
    let folder = std::env::var("AWILI_SBX_PROBE_FOLDER").unwrap();
    let prompt = std::fs::read_to_string(std::env::var("AWILI_SBX_PROBE_PROMPT").unwrap()).unwrap();
    let bin = std::env::var("AWILI_SBX_PROBE_BIN").unwrap();
    let args = CodeEngine::Codex.app_server_args(Autonomy::Autonomous);
    let path = std::env::var("PATH").unwrap_or_default();
    let (tx, rx) = async_channel::unbounded();
    let (_cancel, cancel_rx) = std::sync::mpsc::channel::<()>();
    let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
    let turn = crate::codex_app_server::Turn {
        bin: &bin,
        args: &args,
        cwd: &folder,
        child_path: &path,
        owner: "",
        sandbox: Some(&profile),
        spec: awili_code::app_server::TurnSpec {
            cwd: folder.clone(),
            prompt,
            resume_id: std::env::var("AWILI_SBX_PROBE_RESUME")
                .ok()
                .filter(|s| !s.is_empty()),
            model: Some("gpt-6-luna".into()),
            autonomy: Autonomy::Autonomous,
            host_approvals: false,
        },
    };
    let events = crate::codex_app_server::run_turn(turn, cancel_rx, &tx, &slot, &None, Default::default())
        .map_err(|_| "app server unavailable")
        .unwrap();
    drop(tx);
    while let Ok(ev) = rx.try_recv() {
        eprintln!("LIVE-EVENT {ev:?}");
    }
    for ev in events {
        eprintln!("LIVE-END {ev:?}");
    }
}

// ---- the Codex app server and the ready Claude --------------------------------

#[cfg(target_os = "macos")]
static FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fake vendor that tries a write inside and one outside, and records the
/// results. Returns (script path, record dir, work dir, outside dir) — work and
/// outside sit under the crate folder, which a profile does not open.
#[cfg(target_os = "macos")]
fn writer_fixture(
    name: &str,
    tail: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    PathBuf,
) {
    let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let rec = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let (work, outside) = (base.join("work"), base.join("outside"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let bin = rec.path().join(name);
    crate::exec_fixture::write_exec_fixture(
        &bin,
        &format!(
            "#!/bin/sh\necho i > {w}/inside 2>/dev/null; echo $? > {r}/inside_rc\n\
             echo o > {o}/out 2>/dev/null; echo $? > {r}/outside_rc\n{tail}\n",
            w = work.display(),
            o = outside.display(),
            r = rec.path().display(),
        ),
        0o700,
    );
    (root, rec, bin, work, outside)
}

#[cfg(target_os = "macos")]
fn wait_for(path: &Path) -> String {
    for _ in 0..100 {
        if let Ok(s) = std::fs::read_to_string(path) {
            if !s.trim().is_empty() {
                return s.trim().to_string();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    String::new()
}

/// Codex's app server is started under the profile when one is given, and
/// as itself when not (nothing else about the launch differs).
#[cfg(target_os = "macos")]
#[test]
fn codex_app_server_starts_under_the_profile_only_when_given() {
    let _fixture = crate::exec_fixture::lock(&FIXTURE);
    for with_profile in [false, true] {
        let (root, rec, bin, work, outside) = writer_fixture("codex", "exit 0");
        let home = tempfile::tempdir().unwrap();
        let temp = temp_dirs();
        let text =
            profile_text(&inputs(&work, CodeEngine::Codex, home.path(), &temp, &[])).unwrap();
        let profile = write_profile(&rec.path().join("sb"), &text).unwrap();
        let bin = bin.display().to_string();
        let args = CodeEngine::Codex.app_server_args(Autonomy::Autonomous);
        let folder = work.display().to_string();
        let (tx, _rx) = async_channel::unbounded();
        let (_cancel, cancel_rx) = std::sync::mpsc::channel::<()>();
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let turn = crate::codex_app_server::Turn {
            bin: &bin,
            args: &args,
            cwd: &folder,
            child_path: "/usr/bin:/bin",
            owner: "",
            sandbox: with_profile.then_some(profile.as_path()),
            spec: awili_code::app_server::TurnSpec {
                cwd: folder.clone(),
                prompt: "hi".into(),
                resume_id: None,
                model: None,
                autonomy: Autonomy::Autonomous,
                host_approvals: false,
            },
        };
        let _ = crate::codex_app_server::run_turn(turn, cancel_rx, &tx, &slot, &None, Default::default());
        assert_eq!(wait_for(&rec.path().join("inside_rc")), "0");
        let rc = wait_for(&rec.path().join("outside_rc"));
        if with_profile {
            assert_ne!(rc, "0", "confined");
            assert!(!outside.join("out").exists());
        } else {
            assert_eq!(rc, "0", "unconfined when off");
            assert!(outside.join("out").exists());
        }
        drop(root);
    }
}

/// A Claude made ready ahead starts under the profile too, and a turn only
/// takes a ready Claude started the same way.
#[cfg(target_os = "macos")]
#[test]
fn ready_claude_starts_under_the_profile() {
    let _fixture = crate::exec_fixture::lock(&FIXTURE);
    let (root, rec, bin, work, outside) = writer_fixture("claude", "sleep 1");
    let home = tempfile::tempdir().unwrap();
    let temp = temp_dirs();
    let text = profile_text(&inputs(&work, CodeEngine::Claude, home.path(), &temp, &[])).unwrap();
    let profile = write_profile(&rec.path().join("sb"), &text).unwrap();
    let key = |sandbox: Option<PathBuf>| crate::claude_warm::WarmKey {
        bin: bin.display().to_string(),
        args: vec!["-p".into()],
        cwd: work.display().to_string(),
        owner: "s7-warm-council".into(),
        connectors: None,
        sandbox,
    };
    assert_ne!(
        key(None),
        key(Some(profile.clone())),
        "the profile is part of the key"
    );
    crate::claude_warm::prewarm(
        key(Some(profile.clone())),
        &["-p".into()],
        "/usr/bin:/bin",
        None,
        None,
    );
    assert_eq!(wait_for(&rec.path().join("inside_rc")), "0");
    assert_ne!(wait_for(&rec.path().join("outside_rc")), "0");
    assert!(!outside.join("out").exists());
    assert!(
        crate::claude_warm::take(&key(None)).is_none(),
        "an unsandboxed turn cannot take it"
    );
    let taken = crate::claude_warm::take(&key(Some(profile))).expect("the same launch takes it");
    crate::claude_warm::end(taken);
    drop(root);
}

/// Off macOS the Seatbelt sandbox reports itself unavailable: an enabled
/// sandbox refuses to start an Autonomous turn, and never runs it uncontained.
#[cfg(not(target_os = "macos"))]
#[test]
fn sandbox_reports_itself_unavailable_off_macos() {
    let s = scratch_in(None);
    let paths = launch_paths(&s.home);
    let cwd = s.work.to_str().unwrap();
    let err = prepare(
        &on(),
        CodeEngine::Claude,
        Autonomy::Autonomous,
        cwd,
        Some(&paths),
        &s.data,
    )
    .unwrap_err();
    assert_eq!(err, UNAVAILABLE_OFF_MACOS);
    // Setting off: the turn starts as it always did.
    let off = awili_config::SandboxConfig::default();
    assert!(!off.enabled);
    assert_eq!(
        prepare(
            &off,
            CodeEngine::Claude,
            Autonomy::Autonomous,
            cwd,
            Some(&paths),
            &s.data
        ),
        Ok(None)
    );
}
