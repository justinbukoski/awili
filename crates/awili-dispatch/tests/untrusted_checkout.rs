//! RVS3C3 proof tests: after the sandboxed model exits, the worker never runs
//! Git in the model's checkout. It builds the publishable commit from the
//! checkout's work tree files in its own trusted Git view (baseline ignore
//! rules, exact bytes, frozen baseline parent), with a clean Git environment.
//! Every test runs the real /usr/bin/bwrap (Linux only).
#![cfg(unix)] // TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
#![cfg(target_os = "linux")]
use awili_dispatch::{protocol::*, runner};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use tempfile::TempDir;
/// Test-side Git, only ever on trusted repositories (seed, remote, the
/// worker's view) and with no system/global config.
fn git_ok(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(["-c", "user.name=F", "-c", "user.email=f@x", "-C"])
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    out.status.success().then(|| text.trim().to_string())
}
fn git(dir: &Path, args: &[&str]) -> String {
    git_ok(dir, args).unwrap_or_else(|| panic!("git {args:?} failed in {}", dir.display()))
}
struct Fx {
    temp: TempDir,
    config: RunnerConfig,
    request: BuildRequest,
}
impl Fx {
    /// Baseline repo with `files` (force-added, so a tracked file may match
    /// .gitignore), a bare remote, a model running `model_sh` in its bwrap
    /// profile and a verifier running `verify_sh` over the committed export.
    /// `@MARK@` in `model_sh` becomes a directory only the test (never the
    /// sandbox) can write: a file there proves code ran outside the sandbox.
    fn new(files: &[(&str, &str)], model_sh: &str, verify_sh: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let seed = root.join("seed");
        for (path, body) in files {
            let path = seed.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        fs::create_dir_all(root.join("markers")).unwrap();
        git(&seed, &["init", "-q"]);
        git(&seed, &["add", "-f", "."]);
        git(&seed, &["commit", "-qm", "baseline"]);
        let baseline = git(&seed, &["rev-parse", "HEAD"]);
        let bare = root.join("remote.git");
        let (s, b) = (seed.to_str().unwrap(), bare.to_str().unwrap());
        git(root, &["clone", "--bare", "--quiet", s, b]);
        let (model, verify) = (root.join("model.sh"), root.join("verify.sh"));
        let marks = root.join("markers").to_str().unwrap().to_string();
        assert!(!marks.contains('\''), "{marks}");
        fs::write(&model, model_sh.replace("@MARK@", &format!("'{marks}'"))).unwrap();
        fs::write(&verify, verify_sh).unwrap();
        let profile = |extra: &Path| BwrapProfile {
            bwrap: "/usr/bin/bwrap".into(),
            readonly: vec!["/usr".into(), "/lib".into(), "/lib64".into(), extra.into()],
            writable: vec![],
            env: BTreeMap::new(),
        };
        let command = |script: &Path| CommandSpec {
            program: "/usr/bin/sh".into(),
            args: vec![script.to_str().unwrap().into()],
            env: BTreeMap::new(),
        };
        let target = BuildTarget {
            repo_id: "fixture".into(),
            repo_url: b.into(),
            model: "fixture-local-model".into(),
            command: command(&model),
            verify_profile: "tests".into(),
            verify: command(&verify),
            isolation: Some(IsolationSpec {
                bwrap: profile(&model),
                verifier: profile(&verify),
            }),
        };
        let config = RunnerConfig {
            job_root: root.join("jobs"),
            targets: BTreeMap::from([("local".into(), target)]),
        };
        let request = BuildRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            turn_id: "turn-1".into(),
            conversation_id: "conv-1".into(),
            repo_id: "fixture".into(),
            baseline_sha: baseline,
            branch: "fix/fixture-build".into(),
            spec_text: "rvs3c3".into(),
            target_id: "local".into(),
            verify_profile: "tests".into(),
        };
        Self {
            temp,
            config,
            request,
        }
    }
    fn remote(&self) -> PathBuf {
        PathBuf::from(&self.config.targets["local"].repo_url)
    }
    fn view(&self) -> PathBuf {
        let id = &self.request.request_id;
        self.config.job_root.join(format!("{id}.git-view"))
    }
    fn job(&self) -> PathBuf {
        self.config.job_root.join(&self.request.request_id)
    }
    fn markers(&self) -> Vec<std::ffi::OsString> {
        let dir = fs::read_dir(self.temp.path().join("markers")).unwrap();
        dir.map(|e| e.unwrap().file_name()).collect()
    }
    fn log(&self) -> String {
        fs::read_to_string(self.job().join("control/worker.log")).unwrap_or_default()
    }
    /// The published branch tip on the frozen remote, if any.
    fn published(&self) -> Option<String> {
        git_ok(&self.remote(), &["rev-parse", "--verify", "-q", BRANCH])
    }
    /// Submit through the runner CLI with `extra` in the CLI's (and so the
    /// worker's) environment; wait until the job is terminal AND its worker
    /// process has exited.
    fn run(&self, extra: &[(&str, &str)]) -> BuildSnapshot {
        use std::{io::Write, os::unix::fs::PermissionsExt};
        let path = self.temp.path().join("config.json");
        fs::write(&path, serde_json::to_vec(&self.config).unwrap()).unwrap();
        // Installed as the deploy does (`install -m 0644`), not left to the
        // umask: under umask 0002 the runner rightly refuses a group-writable
        // configuration (RC2-R4M1).
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_awili-build-runner"))
            .arg("--config")
            .arg(&path)
            .arg("submit")
            .envs(extra.iter().copied())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let req = serde_json::to_vec(&self.request).unwrap();
        child.stdin.take().unwrap().write_all(&req).unwrap();
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{err}");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let s = runner::status(&self.config.job_root, &self.request.request_id).unwrap();
            // Exited by the runner's identity check: a zombie or a reused
            // PID is not the worker.
            let gone = s
                .worker
                .as_ref()
                .is_some_and(|w| !runner::worker_running(w));
            if !s.state.active() && gone {
                return s;
            }
            assert!(Instant::now() < deadline, "worker did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    /// The job published: verified, pushed, and the remote holds result_sha.
    fn assert_published(&self, end: &BuildSnapshot) -> String {
        let log = self.log();
        let state = (end.state, end.verify_exit, end.push_exit);
        let want = (BuildState::AwaitingReview, Some(0), Some(0));
        assert_eq!(state, want, "{:?} {log}", end.failure);
        let sha = end.result_sha.clone().unwrap();
        assert_eq!(end.remote_sha.as_deref(), Some(sha.as_str()));
        assert_eq!(self.published().as_deref(), Some(sha.as_str()));
        sha
    }
}
const BRANCH: &str = "refs/heads/fix/fixture-build";
const BASE: &[(&str, &str)] = &[("README.md", "fixture\n")];
const VERIFY_OK: &str = "exit 0\n";
/// RVS3C3 proof: the model plants a core.fsmonitor hook, a filter driver
/// (clean/smudge, selected by .gitattributes AND .git/info/attributes), an
/// include, and hooks (hooksPath and .git/hooks) in its checkout. The job
/// runs through commit, verification and push, and none of it executes on the
/// worker: each payload would create a marker the sandbox cannot write.
#[test]
fn model_git_config_and_hooks_never_run_on_worker() {
    let model = r#"m=@MARK@
g=$PWD/.git
payload() { printf '#!/bin/sh\ntouch %s/%s\ncat\n' "$m" "$1" > "$g/$1"; chmod +x "$g/$1"; }
hooks="pre-commit post-commit pre-push reference-transaction post-index-change post-checkout"
for p in fsmonitor clean smudge $hooks; do payload "$p"; done
for h in $hooks; do cp "$g/$h" "$g/hooks/$h"; done
printf '[core]\n\tfsmonitor = %s\n' "$g/fsmonitor" > "$g/included.cfg"
git config core.fsmonitor "$g/fsmonitor"
git config core.hooksPath "$g/hooks"
git config include.path "$g/included.cfg"
git config filter.awili.clean "$g/clean"
git config filter.awili.smudge "$g/smudge"
printf '* filter=awili\n' > "$g/info/attributes"
printf '* filter=awili\n' > .gitattributes
printf 'implementation\n' > result.rs
echo PLANTED
"#;
    let f = Fx::new(BASE, model, "test -f result.rs\n");
    let end = f.run(&[]);
    assert!(f.log().contains("PLANTED"), "{}", f.log());
    let sha = f.assert_published(&end);
    let config = fs::read_to_string(f.job().join("repo/.git/config")).unwrap();
    assert!(config.contains("fsmonitor = /") && config.contains("clean = /"));
    assert_eq!(
        f.markers(),
        Vec::<std::ffi::OsString>::new(),
        "ran on the worker"
    );
    // The model's .gitattributes is published as plain content.
    let attrs = format!("{sha}:.gitattributes");
    assert_eq!(
        git(&f.view(), &["cat-file", "blob", &attrs]),
        "* filter=awili"
    );
}
/// RVS3C3 proof: a harness can use Git inside the sandbox (commit in the
/// checkout, init/add/commit a nested repository), and none of its commits
/// is published: the result's only parent is the frozen baseline, and its
/// tree is the final work tree (the nested .git is not published).
#[test]
fn harness_git_runs_in_sandbox_and_its_commits_are_not_published() {
    let model = r#"id() { git -c user.name=H -c user.email=h@x "$@"; }
printf 'feature\n' > feature.txt
git add feature.txt && id commit -qm harness-commit && echo "HARNESS-COMMIT $(git rev-parse HEAD)"
mkdir tool && cd tool && git init -q && printf 'x\n' > x.txt && git add x.txt && id commit -qm nested && echo HARNESS-NESTED-OK && cd ..
printf 'after\n' > after.txt
"#;
    let f = Fx::new(BASE, model, VERIFY_OK);
    let end = f.run(&[]);
    let log = f.log();
    assert!(log.contains("HARNESS-NESTED-OK"), "{log}");
    let harness = log.lines().find_map(|l| l.strip_prefix("HARNESS-COMMIT "));
    let harness = harness.unwrap_or_else(|| panic!("no harness commit: {log}"));
    let sha = f.assert_published(&end);
    assert_ne!(sha, harness);
    let parents = git(&f.remote(), &["rev-list", "--parents", "-n", "1", &sha]);
    assert_eq!(parents, format!("{sha} {}", f.request.baseline_sha));
    let harness_commit = format!("{harness}^{{commit}}");
    let leaked = git_ok(&f.remote(), &["cat-file", "-e", &harness_commit]);
    assert!(leaked.is_none(), "the harness commit reached the remote");
    let message = git(&f.remote(), &["log", "-1", "--format=%s", &sha]);
    assert_eq!(message, "build: checkpoint isolated local implementation");
    let tree = git(&f.remote(), &["ls-tree", "-r", "--name-only", &sha]);
    assert_eq!(tree, "README.md\nafter.txt\nfeature.txt\ntool/x.txt");
}
/// RVS3C3 proof: the published tree is exactly the final work tree: added,
/// modified and deleted files, a tracked file matching .gitignore, the
/// execute bit, symlinks inside the tree; ignored paths follow the BASELINE's
/// .gitignore (the model's edits to .gitignore are content, never rules);
/// empty directories are not published.
#[test]
fn published_tree_is_exactly_the_final_work_tree() {
    let base = &[
        (".gitignore", "*.log\nbuild/\n"),
        ("keep.txt", "keep\n"),
        ("modify.txt", "old\n"),
        ("delete.txt", "gone\n"),
        ("tool.sh", "echo tool\n"),
        ("tracked.log", "tracked\n"),
    ];
    let model = r#"printf 'new\n' > modify.txt
rm delete.txt
printf 'added\n' > new.txt
chmod +x tool.sh
ln -s keep.txt link-in
mkdir sub && ln -s ../keep.txt sub/link-up
printf 'more\n' >> tracked.log
printf 'debug\n' > debug.log
mkdir build && printf 'o\n' > build/out.o
printf '!debug.log\nsecret.txt\n' >> .gitignore
printf 'secret\n' > secret.txt
mkdir empty-dir
"#;
    let f = Fx::new(base, model, "test -f new.txt\n");
    let end = f.run(&[]);
    let sha = f.assert_published(&end);
    let listing = git(&f.remote(), &["ls-tree", "-r", &sha]);
    let mut got = Vec::new();
    for line in listing.lines() {
        let (head, path) = line.split_once('\t').unwrap();
        let fields: Vec<&str> = head.split(' ').collect();
        let body = git(&f.remote(), &["cat-file", "blob", fields[2]]);
        got.push(format!("{} {path} {body:?}", fields[0]));
    }
    let want = [
        r#"100644 .gitignore "*.log\nbuild/\n!debug.log\nsecret.txt""#,
        r#"100644 keep.txt "keep""#,
        r#"120000 link-in "keep.txt""#,
        r#"100644 modify.txt "new""#,
        r#"100644 new.txt "added""#,
        r#"100644 secret.txt "secret""#,
        r#"120000 sub/link-up "../keep.txt""#,
        r#"100755 tool.sh "echo tool""#,
        r#"100644 tracked.log "tracked\nmore""#,
    ];
    assert_eq!(got, want, "{listing}");
}
/// RVS3C3 proof: a symlink that leaves the tree (absolute, above the root,
/// through another in-tree symlink, or into .git) fails the job before
/// verification, and nothing is pushed.
#[test]
fn escaping_symlinks_are_rejected() {
    let cases = [
        "ln -s /etc/passwd abs",
        "ln -s ../outside up",
        "mkdir sub && ln -s .. sub/up && ln -s up/../outside sub/esc",
        "ln -s .git/config cfg",
    ];
    for case in cases {
        let model = format!("printf 'x\\n' > new.txt\n{case}\n");
        let f = Fx::new(BASE, &model, VERIFY_OK);
        let end = f.run(&[]);
        assert_eq!(end.state, BuildState::Failed, "{case}: {:?}", end.failure);
        let failure = end.failure.as_deref().unwrap_or_default();
        assert!(failure.contains("symlink escapes the"), "{case}: {failure}");
        assert_eq!((end.verify_exit, end.push_exit), (None, None), "{case}");
        assert_eq!(f.published(), None, "{case}");
    }
}
/// RVS3C3 proof: the pushed SHA is exactly the SHA the verifier tested, both
/// as AWILI_RESULT_SHA and as HEAD of its trusted Git view, and not the
/// model's own checkout HEAD.
#[test]
fn pushed_sha_is_the_sha_the_verifier_tested() {
    let model = r#"printf 'new\n' > new.txt
git add new.txt && git -c user.name=H -c user.email=h@x commit -qm model-head
"#;
    let verify = "printf 'VERIFIED %s %s\\n' \"$AWILI_RESULT_SHA\" \"$(git rev-parse HEAD)\"\n";
    let f = Fx::new(BASE, model, verify);
    let end = f.run(&[]);
    let sha = f.assert_published(&end);
    let log = f.log();
    assert!(log.contains(&format!("VERIFIED {sha} {sha}\n")), "{log}");
    assert_eq!(log.matches("VERIFIED ").count(), 1, "{log}");
    let model_head = git(&f.job().join("repo"), &["rev-parse", "HEAD"]);
    assert_ne!(model_head, sha, "the model's own commit was published");
}
/// RVS3C3 defense in depth: worker Git never inherits Git configuration
/// from its environment (GIT_CONFIG_COUNT, GIT_CONFIG_GLOBAL, HOME). An
/// insteadOf rewrite of the frozen remote to a decoy in each of them is
/// ignored: the job clones from and pushes to the frozen remote.
#[test]
fn worker_git_uses_a_clean_environment() {
    let f = Fx::new(BASE, "printf 'x\\n' > new.txt\n", VERIFY_OK);
    let decoy = f.temp.path().join("decoy.git");
    let (d, r) = (decoy.to_str().unwrap(), f.remote());
    git(f.temp.path(), &["init", "-q", "--bare", d]);
    let rewrite = format!("[url \"{d}\"]\n\tinsteadOf = {}\n", r.display());
    let home = f.temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join(".gitconfig"), &rewrite).unwrap();
    let global = f.temp.path().join("global.cfg");
    fs::write(&global, &rewrite).unwrap();
    let key = format!("url.{d}.insteadOf");
    let end = f.run(&[
        ("HOME", home.to_str().unwrap()),
        ("GIT_CONFIG_GLOBAL", global.to_str().unwrap()),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", &key),
        ("GIT_CONFIG_VALUE_0", r.to_str().unwrap()),
    ]);
    f.assert_published(&end);
    assert!(git(&decoy, &["for-each-ref"]).is_empty(), "used the decoy");
}

/// RVS3C3-FIX1 proof: the baseline's .gitignore is used byte for byte. A
/// pattern ending in an escaped trailing space (`space\ `, the last line)
/// still ignores the file `space `, so the model cannot publish it.
#[test]
fn baseline_gitignore_bytes_are_kept_exactly() {
    let base = &[(".gitignore", "*.log\nspace\\ \n"), ("keep.txt", "keep\n")];
    let model = "printf 'x\\n' > 'space '\nprintf 'added\\n' > new.txt\n";
    let f = Fx::new(base, model, VERIFY_OK);
    let end = f.run(&[]);
    let sha = f.assert_published(&end);
    let tree = git(&f.remote(), &["ls-tree", "-r", "--name-only", &sha]);
    assert_eq!(
        tree, ".gitignore\nkeep.txt\nnew.txt",
        "ignored file published"
    );
}
/// RVS3C3-FIX1 proof: a newline in any work tree path fails the job before
/// verification, for a regular file and for a symlink alike.
#[test]
fn newline_paths_fail_closed() {
    for case in ["printf x > 'a\nb'", "ln -s keep.txt 'l\nk'"] {
        let base = &[("keep.txt", "keep\n")];
        let model = format!("printf 'x\\n' > new.txt\n{case}\n");
        let f = Fx::new(base, &model, VERIFY_OK);
        let end = f.run(&[]);
        assert_eq!(end.state, BuildState::Failed, "{case}: {:?}", end.failure);
        let failure = end.failure.as_deref().unwrap_or_default();
        assert!(failure.contains("contains a newline"), "{case}: {failure}");
        assert_eq!((end.verify_exit, end.push_exit), (None, None), "{case}");
        assert_eq!(f.published(), None, "{case}");
    }
}
