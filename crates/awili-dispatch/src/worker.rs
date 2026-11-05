//! One owned checkout, independent verification, and create-only branch publication.
use crate::protocol::*;
use crate::store::Store;
use anyhow::{anyhow, bail, ensure, Context, Result};
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

/// The worker's single source of truth: the record captured under the lock
/// at claim time, before the model launches.
struct Owned<'a> {
    store: &'a Store,
    expected: JobRecord,
}
/// Outcome of comparing the on-disk record with the owned in-memory record.
enum Drift {
    /// Identical: the record is exactly what the worker expects.
    Same,
    /// Changed only the way `runner::cancel` (or `runner::status` after a
    /// lost worker) legitimately changes it.
    Cancelled,
    /// Any other difference, or an unreadable record.
    Tampered,
}
impl<'a> Owned<'a> {
    /// Compare the on-disk record with the expected one. The caller holds
    /// the store lock.
    fn drift(&self) -> Drift {
        let Ok(fresh) = self.store.read(&self.expected.request.request_id) else {
            return Drift::Tampered;
        };
        if fresh == self.expected {
            return Drift::Same;
        }
        let e = &self.expected.snapshot;
        let f = &fresh.snapshot;
        let cancelled = fresh.request == self.expected.request
            && fresh.target == self.expected.target
            && f.revision > e.revision
            && matches!(
                f.state,
                BuildState::CancelRequested | BuildState::NeedsAttention
            )
            && f.request_id == e.request_id
            && f.request_sha256 == e.request_sha256
            && f.executor == e.executor
            && f.requested_model == e.requested_model
            && f.baseline_sha == e.baseline_sha
            && f.result_sha == e.result_sha
            && f.remote_sha == e.remote_sha
            && f.model_exit == e.model_exit
            && f.verify_exit == e.verify_exit
            && f.push_exit == e.push_exit
            && f.worker == e.worker
            && f.no_op == e.no_op;
        if cancelled {
            Drift::Cancelled
        } else {
            Drift::Tampered
        }
    }
    /// Under the lock, classify the on-disk record and restore it if it was
    /// tampered with. Returns an error that aborts the calling transition.
    fn guard(&mut self) -> Result<()> {
        let _lock = self.store.lock()?;
        self.apply_drift()?;
        Ok(())
    }
    /// Apply the snapshot mutation, guarded against foreign writers; one lock
    /// acquisition covers both the drift check and the write.
    fn transition(&mut self, f: impl FnOnce(&mut BuildSnapshot)) -> Result<()> {
        let _lock = self.store.lock()?;
        if self.apply_drift()? {
            ensure!(
                matches!(
                    self.expected.snapshot.state,
                    BuildState::Running | BuildState::Verifying
                ),
                "worker no longer owns active state"
            );
        }
        let mut next = self.expected.clone();
        f(&mut next.snapshot);
        self.store.write(&mut next)?;
        self.expected = next;
        Ok(())
    }
    /// Classify the drift and repair a tampered record. Returns `true` when
    /// the record was untouched. The caller holds the store lock.
    fn apply_drift(&mut self) -> Result<bool> {
        match self.drift() {
            Drift::Same => Ok(true),
            Drift::Cancelled => Err(anyhow::anyhow!("cancel requested")),
            Drift::Tampered => {
                let fresh_revision = self
                    .store
                    .read(&self.expected.request.request_id)
                    .map(|fresh| fresh.snapshot.revision)
                    .unwrap_or(self.expected.snapshot.revision);
                let mut restored = self.expected.clone();
                restored.snapshot.state = BuildState::NeedsAttention;
                restored.snapshot.failure =
                    Some("control record changed outside the worker".into());
                restored.snapshot.revision = restored.snapshot.revision.max(fresh_revision);
                // The reason reaches worker.log before the state is published.
                log_reason(
                    self.store,
                    &self.expected.request.request_id,
                    "control record changed outside the worker",
                );
                // Ignore a write error; the caller still reports the drift.
                let _ = self.store.write(&mut restored);
                self.expected = restored;
                Err(anyhow::anyhow!("control record changed outside the worker"))
            }
        }
    }
}
/// Append `reason` as a line to the job's worker.log. Called before a
/// terminal state is published, so whoever reads that state finds its reason
/// in the log already. If the log cannot be opened the reason still goes to
/// stderr (the same log when the runner spawned this worker).
fn log_reason(store: &Store, id: &str, reason: &str) {
    use std::io::Write;
    let logged = store
        .log_for_append(id)
        .and_then(|mut log| Ok(log.write_all(format!("{reason}\n").as_bytes())?));
    if logged.is_err() {
        eprintln!("{reason}");
    }
}
/// Run job `id` as its worker. Every error is reported by this function (the
/// caller must not print it again): one that ends the job is written to
/// worker.log BEFORE the terminal state is published, so a reader who sees
/// the job Failed (or NeedsAttention) also finds the reason in its log.
pub fn run(root: &Path, id: &str) -> Result<()> {
    let store = Store::open(root).inspect_err(|e| eprintln!("{e:#}"))?;
    let record = claim(&store, id).inspect_err(|e| eprintln!("{e:#}"))?;
    let mut owned = Owned {
        store: &store,
        expected: record,
    };
    if let Err(err) = execute(&mut owned) {
        // If the drift repair already restored the record to NeedsAttention,
        // keep that outcome (it logged its reason first); otherwise log the
        // failure, then persist it on the expected record.
        let restored = owned.expected.snapshot.state == BuildState::NeedsAttention
            && owned.expected.snapshot.failure.as_deref()
                == Some("control record changed outside the worker");
        if !restored {
            let reason = format!("{err:#}");
            log_reason(&store, id, &reason);
            let _ = owned.transition(|s| {
                s.state = BuildState::Failed;
                s.failure = Some(reason);
            });
        }
        return Err(err);
    }
    Ok(())
}
/// Under the lock, take a Queued job whose recorded worker is this process,
/// and mark it Running.
fn claim(store: &Store, id: &str) -> Result<JobRecord> {
    let _lock = store.lock()?;
    let mut record = store.read(id)?;
    ensure!(
        record.snapshot.state == BuildState::Queued,
        "job is not queued"
    );
    ensure!(
        record
            .snapshot
            .worker
            .as_ref()
            .is_some_and(|w| w.pid == std::process::id() && crate::process::alive(w)),
        "worker identity mismatch"
    );
    record.snapshot.state = BuildState::Running;
    store.write(&mut record)?;
    Ok(record)
}
/// RVS3C3: the job's private, empty HOME for every worker Git invocation.
static GIT_HOME: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
fn init_git_home(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::remove_dir_all(path).ok();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .context("git home")?;
    *GIT_HOME.lock().map_err(|_| anyhow!("git home lock"))? = Some(path.into());
    Ok(())
}
fn git(repo: Option<&Path>, args: &[&str]) -> Result<Output> {
    git_input(repo, args, None, &[])
}
/// RVS3C3 defense in depth, in addition to never running Git in the model's
/// checkout: every worker Git starts from an EMPTY environment (no inherited
/// GIT_* variable) with no system or global config, HOME = the job's private
/// empty directory (no ~/.gitconfig or ~/.config/git/{attributes,ignore}),
/// hooks and fsmonitor off. Only PATH and SSH_AUTH_SOCK (the push) pass
/// through. A helper thread writes `input` to stdin (no pipe deadlock);
/// `index` selects a worker-owned GIT_INDEX_FILE.
fn git_input(
    repo: Option<&Path>,
    args: &[&str],
    index: Option<&Path>,
    input: &[u8],
) -> Result<Output> {
    use std::io::Write;
    let home = GIT_HOME
        .lock()
        .map_err(|_| anyhow!("git home lock"))?
        .clone();
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
    ]);
    cmd.args(["-c", "protocol.file.allow=always"]);
    if let Some(repo) = repo {
        cmd.arg("-C").arg(repo);
    }
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    cmd.args(args).env_clear().env("PATH", path);
    cmd.env("HOME", home.context("worker git home is not initialised")?);
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    // RC2-R4 (TOP43 #13): Git itself refuses every transport but the
    // configured ones (no `ext::` helper, no `fd::`, no plain http or git).
    cmd.env("GIT_ALLOW_PROTOCOL", crate::protocol::GIT_ALLOWED_PROTOCOLS);
    if let Some(sock) = std::env::var_os("SSH_AUTH_SOCK") {
        cmd.env("SSH_AUTH_SOCK", sock);
    }
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let (mut stdin, input) = (child.stdin.take().context("git stdin")?, input.to_vec());
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output()?;
    let wrote = writer.join().map_err(|_| anyhow!("git stdin writer"))?;
    if output.status.success() {
        wrote.context("write git stdin")?;
    }
    Ok(output)
}
fn text(output: Output, operation: &str) -> Result<String> {
    // Do not echo credential-bearing Git remote/error strings in snapshot failures.
    ensure!(
        output.status.success(),
        "git {operation} failed (exit {:?})",
        output.status.code()
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
fn git_text(repo: &Path, args: &[&str], operation: &str) -> Result<String> {
    text(git(Some(repo), args)?, operation)
}
type Found = Vec<(PathBuf, fs::Metadata)>;
/// RVS3C3: every non-directory entry below `root` as (relative path, lstat
/// metadata). Never follows a symlink; skips every `.git` entry at any depth
/// (the model's repository metadata or a harness's nested repository).
fn walk_work_tree(root: &Path, rel: &Path, out: &mut Found) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    for entry in fs::read_dir(root.join(rel)).context("read the work tree")? {
        let name = entry?.file_name();
        if !name.as_bytes().eq_ignore_ascii_case(b".git") {
            let rel = rel.join(&name);
            let meta = fs::symlink_metadata(root.join(&rel))?;
            if meta.is_dir() {
                walk_work_tree(root, &rel, out)?;
            } else {
                out.push((rel, meta));
            }
        }
    }
    Ok(())
}
type Links = std::collections::BTreeMap<Vec<u8>, Vec<u8>>;
/// RVS3C3: true when the symlink at `link` resolves inside the published
/// tree: lexically from the link's directory, following the tree's other
/// symlinks (at most 40 hops), so `sub/up -> ..` plus `sub/esc -> up/../x`
/// is caught. An absolute target, a step above the root, a loop, or a step
/// into `.git` is an escape.
fn symlink_stays_inside(links: &Links, link: &[u8]) -> bool {
    let mut resolved: Vec<&[u8]> = link.split(|b| *b == b'/').collect();
    resolved.pop();
    let (mut todo, mut hops, mut next) = (Vec::new(), 0, Some(&links[link]));
    loop {
        if let Some(target) = next.take() {
            hops += 1;
            if hops > 40 || target.first() == Some(&b'/') {
                return false;
            }
            todo.extend(target.split(|b| *b == b'/').rev());
        }
        match todo.pop() {
            None => return true,
            Some(b"" | b".") => {}
            Some(b"..") => {
                if resolved.pop().is_none() {
                    return false;
                }
            }
            Some(name) if name.eq_ignore_ascii_case(b".git") => return false,
            Some(name) => {
                resolved.push(name);
                if let Some(target) = links.get(&resolved.join(&b'/')) {
                    resolved.pop();
                    next = Some(target);
                }
            }
        }
    }
}
/// RVS3C3: build the publishable commit from the model's final WORK TREE
/// FILES in the worker-owned trusted view; no Git command ever runs with its
/// cwd, GIT_DIR or work tree in the checkout (whose .git is untrusted).
/// Ignored paths follow the TRUSTED BASELINE's .gitignore files, copied into
/// the worker's own `<stage>/rules` repository (never the model's .gitignore
/// or info/exclude); a path the baseline tracks is always kept (as `git add
/// -A`). Bytes are hashed exactly (`--no-filters`: no attributes, filters or
/// eol conversion); the owner execute bit gives 100755; symlinks keep their
/// target and must stay inside the tree; other file types and baseline
/// submodules fail the job; empty directories are not published. The model's
/// commits, branch, remote, config and hooks are never read. Returns the new
/// commit (sole parent: the frozen baseline) or, for an unchanged tree, the
/// baseline SHA (no-op).
fn publishable_commit(view: &Path, stage: &Path, checkout: &Path, base: &str) -> Result<String> {
    use std::collections::{BTreeMap, BTreeSet};
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let run = |args: &[&str], index: Option<&Path>, input: &[u8]| -> Result<String> {
        text(git_input(Some(view), args, index, input)?, args[0])
    };
    let checkout = checkout.canonicalize().context("resolve checkout path")?;
    let (rules, index) = (stage.join("rules"), stage.join("index"));
    fs::remove_dir_all(stage).ok();
    fs::create_dir_all(&rules)?;
    text(
        git(Some(&rules), &["init", "--quiet"])?,
        "init ignore rules",
    )?;
    let listing = git(Some(view), &["ls-tree", "-r", "-z", "--full-tree", base])?;
    ensure!(listing.status.success(), "git list baseline tree failed");
    let mut tracked = BTreeSet::new();
    for record in listing.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = record.iter().position(|b| *b == b'\t').context("ls-tree")?;
        let (head, path) = (std::str::from_utf8(&record[..tab])?, &record[tab + 1..]);
        let f: Vec<&str> = head.split(' ').collect();
        ensure!(
            f.len() == 3 && f[0] != "160000",
            "unsupported baseline entry"
        );
        tracked.insert(path);
        if path.rsplit(|b| *b == b'/').next() == Some(b".gitignore") && f[0] != "120000" {
            let dest = rules.join(std::ffi::OsStr::from_bytes(path));
            fs::create_dir_all(dest.parent().context("rules path")?)?;
            // RVS3C3-FIX1: the blob's exact bytes (no trimming: a trailing
            // escaped space is part of an ignore pattern).
            let blob = git(Some(view), &["cat-file", "blob", f[2]])?;
            ensure!(blob.status.success(), "git read baseline .gitignore failed");
            fs::write(dest, blob.stdout)?;
        }
    }
    let mut found = Vec::new();
    walk_work_tree(&checkout, Path::new(""), &mut found)?;
    let mut query = Vec::new();
    for (rel, _) in &found {
        query.extend(rel.as_os_str().as_bytes().iter().chain(b"\0"));
    }
    let check = ["check-ignore", "--no-index", "-z", "--stdin"];
    let answer = git_input(Some(&rules), &check, None, &query)?;
    ensure!(
        matches!(answer.status.code(), Some(0 | 1)),
        "check-ignore failed"
    );
    let ignored: BTreeSet<&[u8]> = answer.stdout.split(|b| *b == 0).collect();
    let (mut paths, mut modes, mut links) = (Vec::new(), Vec::new(), BTreeMap::new());
    for (rel, meta) in &found {
        let key = rel.as_os_str().as_bytes();
        // RVS3C3-FIX1: fail closed on a newline in ANY work tree path, before
        // ignore rules or the file type are considered.
        ensure!(!key.contains(&b'\n'), "work tree path contains a newline");
        let exec = meta.mode() & 0o100 != 0;
        if ignored.contains(key) && !tracked.contains(key) {
            continue;
        } else if meta.file_type().is_symlink() {
            let target = fs::read_link(checkout.join(rel))?;
            links.insert(key.to_vec(), target.as_os_str().as_bytes().to_vec());
        } else if meta.file_type().is_file() {
            modes.push((if exec { "100755" } else { "100644" }, key));
            paths.extend(
                checkout
                    .join(rel)
                    .as_os_str()
                    .as_bytes()
                    .iter()
                    .chain(b"\n"),
            );
        } else {
            bail!("unsupported work tree entry: {}", rel.display());
        }
    }
    let hash = ["hash-object", "-w", "--no-filters", "--stdin-paths"];
    let oids = run(&hash, None, &paths)?;
    ensure!(
        oids.lines().count() == modes.len(),
        "hash-object count mismatch"
    );
    let mut entries: Vec<(&str, String, &[u8])> = Vec::new();
    for ((mode, path), oid) in modes.into_iter().zip(oids.lines()) {
        entries.push((mode, oid.to_string(), path));
    }
    for (path, target) in &links {
        let why = format!(
            "symlink escapes the checkout: {}",
            String::from_utf8_lossy(path)
        );
        ensure!(symlink_stays_inside(&links, path), why);
        let hash = ["hash-object", "-w", "--no-filters", "--stdin"];
        entries.push(("120000", run(&hash, None, target)?, path));
    }
    let mut info = Vec::new();
    for (mode, oid, path) in entries {
        info.extend(
            format!("{mode} {oid}\t")
                .bytes()
                .chain(path.iter().copied()),
        );
        info.push(0);
    }
    run(
        &["update-index", "--add", "-z", "--index-info"],
        Some(&index),
        &info,
    )?;
    let tree = run(&["write-tree"], Some(&index), &[])?;
    if tree == run(&["rev-parse", &format!("{base}^{{tree}}")], None, &[])? {
        return Ok(base.to_string());
    }
    let msg = "build: checkpoint isolated local implementation";
    let who = [
        "-c",
        "user.name=Awili local build",
        "-c",
        "user.email=awili-build@localhost",
    ];
    let args = [&who[..], &["commit-tree", &tree, "-p", base, "-m", msg][..]].concat();
    let sha = text(git(Some(view), &args)?, "commit")?;
    run(&["update-ref", "refs/awili/result", &sha], None, &[])?;
    Ok(sha)
}
/// S3(b) preflight, before any model command is built or started. On Linux
/// the configured bwrap executable is run (outside any sandbox, with an
/// empty environment) on the exact fixed arguments and binds the launch
/// will use. A missing bwrap, an unavailable user or PID namespace, or an
/// unlisted/missing bind source errors here, before model execution; there
/// is no fallback to a direct command. On other targets isolation is
/// refused outright (fail closed).
fn ensure_isolation_available(profile: &BwrapProfile) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let probe = Command::new(&profile.bwrap)
            .env_clear()
            .args(isolation_args(profile))
            .arg("/usr/bin/true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status();
        let Ok(status) = probe else {
            return Err(anyhow!(
                "isolation unavailable: bwrap {} cannot start",
                profile.bwrap.display()
            ));
        };
        ensure!(
            status.success(),
            "isolation unavailable: bwrap probe failed (exit {:?}) — the job fails before model execution",
            status.code()
        );
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = profile;
        bail!("model isolation requires the Linux bwrap profile; refusing to launch the model")
    }
}
/// The fixed bwrap argument list for one profile. No root bind is used: the
/// model's view is exactly the profile's listed paths plus what the caller
/// adds (the owned checkout and the job's private scratch).
#[cfg(target_os = "linux")]
fn isolation_args(profile: &BwrapProfile) -> Vec<String> {
    let mut args = vec![
        "--die-with-parent".to_string(),
        // Own PID namespace: the model cannot see or open the worker's
        // /proc/<pid>/fd links (control leaves), and every process in the
        // sandbox is killed when the model's init exits, so the output pipe
        // always reaches EOF once the model is done.
        "--unshare-pid".to_string(),
        // New session: no terminal injection (TIOCSTI) into the worker's tty.
        "--new-session".to_string(),
    ];
    for src in &profile.readonly {
        args.push("--ro-bind".into());
        args.push(src.to_string_lossy().into_owned());
        args.push(src.to_string_lossy().into_owned());
    }
    for src in &profile.writable {
        args.push("--bind".into());
        args.push(src.to_string_lossy().into_owned());
        args.push(src.to_string_lossy().into_owned());
    }
    args
}
/// PATH the model gets when neither the profile nor the target declares one.
#[cfg(target_os = "linux")]
const MODEL_DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// Largest number of stdout/stderr bytes of one sandboxed run (the model's,
/// or the verifier's) the worker copies into the control log. Output beyond
/// it is drained and counted, never written.
#[cfg(target_os = "linux")]
const MODEL_OUTPUT_LIMIT: u64 = 1 << 20;
/// S3(b): the model's complete environment. The caller clears the inherited
/// environment first; a later layer overrides an earlier one:
/// fixed defaults < profile.env < target-declared spec.env < worker job
/// values < pinned sandbox HOME/TMPDIR.
#[cfg(target_os = "linux")]
fn model_env(
    profile: &BwrapProfile,
    spec: &CommandSpec,
    job: &[(&str, &str)],
) -> std::collections::BTreeMap<String, String> {
    let mut env = std::collections::BTreeMap::new();
    env.insert("PATH".to_string(), MODEL_DEFAULT_PATH.to_string());
    env.insert("LANG".to_string(), "C.UTF-8".to_string());
    env.extend(profile.env.clone());
    env.extend(spec.env.clone());
    for (k, v) in job {
        env.insert((*k).to_string(), (*v).to_string());
    }
    env.insert("HOME".to_string(), format!("{SANDBOX_SCRATCH}/home"));
    env.insert("TMPDIR".to_string(), format!("{SANDBOX_SCRATCH}/tmp"));
    env
}
/// Copy a sandboxed `role`'s (model or verifier) output pipe into the control
/// log, at most MODEL_OUTPUT_LIMIT bytes. Always drains to EOF (even after the
/// limit or a log write error) so the process never blocks on a full pipe;
/// then appends the end or truncation marker. Only this worker thread writes
/// the log.
#[cfg(target_os = "linux")]
fn pump_output(
    mut reader: std::io::PipeReader,
    mut log: File,
    role: &'static str,
) -> std::io::Result<()> {
    use std::io::{ErrorKind, Read, Write};
    let mut buf = [0u8; 8192];
    let mut total: u64 = 0;
    let mut write_error: Option<std::io::Error> = None;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let room = MODEL_OUTPUT_LIMIT.saturating_sub(total);
        let keep = (n as u64).min(room) as usize;
        if keep > 0 && write_error.is_none() {
            write_error = log.write_all(&buf[..keep]).err();
        }
        total += n as u64;
    }
    if let Some(e) = write_error {
        return Err(e);
    }
    let kept = total.min(MODEL_OUTPUT_LIMIT);
    let footer = if total > kept {
        format!("\n[awili-worker] {role} output truncated: kept {kept} of {total} bytes\n")
    } else {
        format!("\n[awili-worker] {role} output end: {total} bytes\n")
    };
    log.write_all(footer.as_bytes())
}
/// S3(b)/RVS3-INT1: run a bwrap launch with stdout and stderr on ONE pipe
/// that a worker thread drains into the control log (`pump_output`), framed
/// by `[awili-worker] {role} output begin` and the end or truncation marker.
/// The sandboxed process (the model, or the verifier running model-authored
/// fixture code) never holds a descriptor for any control leaf, so it can
/// neither reopen nor truncate worker.log through /proc/self/fd.
#[cfg(target_os = "linux")]
fn run_with_output_pipe(mut cmd: Command, mut log: File, role: &'static str) -> Result<i32> {
    use std::io::Write;
    let (reader, writer) = std::io::pipe().context("create sandbox output pipe")?;
    // Written before the process starts, so it always precedes its output.
    let begin = format!("[awili-worker] {role} output begin\n");
    log.write_all(begin.as_bytes())
        .context("write sandbox output header")?;
    cmd.stdin(Stdio::null())
        .stdout(writer.try_clone().context("clone sandbox output pipe")?)
        .stderr(writer);
    let spawned = cmd.spawn();
    // The Command still owns the parent's copies of the pipe's write end;
    // drop it now or the reader never sees EOF.
    drop(cmd);
    let mut child = spawned.context("start bwrap")?;
    let pump = std::thread::spawn(move || pump_output(reader, log, role));
    let waited = child.wait();
    let pumped = pump
        .join()
        .map_err(|_| anyhow!("{role} output reader panicked"))?;
    let status = waited.with_context(|| format!("wait for sandboxed {role}"))?;
    pumped.with_context(|| format!("copy {role} output to the control log"))?;
    Ok(status.code().unwrap_or(-1))
}
/// S3(b): launch the model through the fixed bwrap profile with an empty
/// inherited environment, the owned checkout bound writable at its own
/// path, the job's private scratch bound writable at SANDBOX_SCRATCH, and
/// stdout/stderr on a pipe the worker drains into the control log. The
/// model never holds a descriptor for any control leaf.
#[cfg(target_os = "linux")]
fn command_sandboxed(
    profile: &BwrapProfile,
    spec: &CommandSpec,
    repo: &Path,
    scratch: &Path,
    log: File,
    env: &[(&str, &str)],
    prompt: Option<&str>,
) -> Result<i32> {
    let checkout = repo.canonicalize().context("resolve checkout path")?;
    let scratch = scratch.canonicalize().context("resolve scratch path")?;
    ensure!(
        std::fs::metadata(&profile.bwrap)?.permissions().mode() & 0o111 != 0,
        "isolation unavailable: bwrap is not executable"
    );
    let mut args = isolation_args(profile);
    args.extend([
        "--bind".into(),
        checkout.to_string_lossy().into_owned(),
        checkout.to_string_lossy().into_owned(),
        "--bind".into(),
        scratch.to_string_lossy().into_owned(),
        SANDBOX_SCRATCH.to_string(),
        "--dev".into(),
        "/dev".into(),
        "--proc".into(),
        "/proc".into(),
        "--chdir".into(),
        checkout.to_string_lossy().into_owned(),
    ]);
    args.push(spec.program.to_string_lossy().into_owned());
    args.extend(spec.args.iter().cloned());
    if let Some(prompt) = prompt {
        args.push(prompt.to_string());
    }
    let mut cmd = Command::new(&profile.bwrap);
    cmd.args(&args)
        .env_clear()
        .envs(model_env(profile, spec, env));
    run_with_output_pipe(cmd, log, "model")
}
/// Non-Linux model launch path: isolation cannot be provided, so the model
/// is never started (fail closed).
#[cfg(not(target_os = "linux"))]
fn command_sandboxed(
    profile: &BwrapProfile,
    spec: &CommandSpec,
    repo: &Path,
    scratch: &Path,
    log: File,
    env: &[(&str, &str)],
    prompt: Option<&str>,
) -> Result<i32> {
    let _ = (profile, spec, repo, scratch, log, env, prompt);
    bail!("model isolation requires the Linux bwrap profile; refusing to launch the model")
}
/// S3(b): the job's private scratch, `<job_dir>/scratch` (0700), bound
/// writable at SANDBOX_SCRATCH for one model run. Created fresh (an existing
/// entry, including a symlink, is an error, never reused) and removed when
/// dropped, right after the model exits. `remove_dir_all` never follows
/// symlinks. Anything the model makes undeletable stays inside this job's
/// 0700 job directory and is never bound into another job.
struct JobScratch {
    path: PathBuf,
}
impl JobScratch {
    fn create(job_dir: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        let path = job_dir.join("scratch");
        builder
            .create(&path)
            .context("create job scratch; refusing reuse")?;
        let scratch = Self { path };
        builder
            .create(scratch.path.join("home"))
            .context("create scratch home")?;
        builder
            .create(scratch.path.join("tmp"))
            .context("create scratch tmp")?;
        Ok(scratch)
    }
}
impl Drop for JobScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
/// Materialize committed blob bytes directly; archive attributes and filters never run.
#[cfg(target_os = "linux")]
fn export_committed_tree(repo: &Path, sha: &str, dest: &Path) -> Result<()> {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let listing = git(Some(repo), &["ls-tree", "-rz", "--full-tree", sha])?;
    ensure!(listing.status.success(), "cannot list committed tree");
    for entry in listing.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let tab = entry.iter().position(|b| *b == b'\t').ok_or_else(|| anyhow!("invalid tree entry"))?;
        let header = std::str::from_utf8(&entry[..tab])?;
        let mut parts = header.split_whitespace();
        let mode = parts.next().ok_or_else(|| anyhow!("missing tree mode"))?;
        let kind = parts.next().ok_or_else(|| anyhow!("missing tree kind"))?;
        let oid = parts.next().ok_or_else(|| anyhow!("missing object id"))?;
        ensure!(parts.next().is_none() && kind == "blob", "unsupported committed tree entry");
        let relative = PathBuf::from(std::ffi::OsString::from_vec(entry[tab + 1..].to_vec()));
        ensure!(relative.components().all(|c| matches!(c, std::path::Component::Normal(_))), "invalid committed path");
        let path = dest.join(relative);
        fs::create_dir_all(path.parent().ok_or_else(|| anyhow!("missing parent"))?)?;
        let blob = git(Some(repo), &["cat-file", "blob", oid])?;
        ensure!(blob.status.success(), "cannot read committed blob");
        match mode {
            "100644" | "100755" => {
                fs::write(&path, &blob.stdout)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(if mode == "100755" { 0o755 } else { 0o644 }))?;
            }
            "120000" => symlink(std::ffi::OsString::from_vec(blob.stdout), &path)?,
            _ => bail!("unsupported committed file mode {mode}"),
        }
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn export_committed_tree(_repo: &Path, _sha: &str, _dest: &Path) -> Result<()> {
    bail!("verifier isolation requires Linux")
}

/// Non-Linux verifier launch path: isolation cannot be provided, so the
/// verifier is never started (fail closed).
#[cfg(not(target_os = "linux"))]
fn verify_sandboxed(
    profile: &BwrapProfile,
    spec: &CommandSpec,
    export: &Path,
    scratch: &Path,
    git_view: &Path,
    log: File,
    env: &[(&str, &str)],
) -> Result<i32> {
    let _ = (profile, spec, export, scratch, git_view, log, env);
    bail!("verifier isolation requires the Linux bwrap profile; refusing to launch the verifier")
}
/// PATH the verifier gets when neither its profile nor the verify command
/// declares one.
#[cfg(target_os = "linux")]
const VERIFIER_DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
/// S3(c): the verifier's complete environment. The caller clears the
/// inherited environment first, so no worker variable (push credentials,
/// agent sockets, GIT_*, CARGO_*) reaches the verifier. A later layer
/// overrides an earlier one: fixed defaults < profile.env < the verify
/// command's declared env < the frozen SHAs < the pinned trusted Git view
/// (GIT_DIR) and this job's own scratch HOME/TMPDIR. `scratch` and
/// `git_view` are the canonical paths bound into the sandbox.
#[cfg(target_os = "linux")]
fn verifier_env(
    profile: &BwrapProfile,
    spec: &CommandSpec,
    scratch: &Path,
    git_view: &Path,
    job: &[(&str, &str)],
) -> std::collections::BTreeMap<String, String> {
    let mut env = std::collections::BTreeMap::new();
    env.insert("PATH".to_string(), VERIFIER_DEFAULT_PATH.to_string());
    env.insert("LANG".to_string(), "C.UTF-8".to_string());
    env.extend(profile.env.clone());
    env.extend(spec.env.clone());
    for (k, v) in job {
        env.insert((*k).to_string(), (*v).to_string());
    }
    env.insert(
        "GIT_DIR".to_string(),
        git_view.to_string_lossy().into_owned(),
    );
    env.insert(
        "HOME".to_string(),
        scratch.join("home").to_string_lossy().into_owned(),
    );
    env.insert(
        "TMPDIR".to_string(),
        scratch.join("tmp").to_string_lossy().into_owned(),
    );
    env
}
/// S3(c): the verifier role's bwrap view, built the same way as the model's
/// but around the committed source export instead of the live checkout. The
/// export and the trusted Git view are bound read-only (the verifier can
/// neither edit them nor plant uncommitted inputs), the verifier's own
/// scratch directory is the only writable scratch beyond the profile's
/// fixed list, and nothing else — no control directory, no checkout, no
/// remote configuration — is listed.
#[cfg(target_os = "linux")]
fn verify_sandbox_args(
    profile: &BwrapProfile,
    export: &Path,
    scratch: &Path,
    git_view: &Path,
    spec: &CommandSpec,
) -> Result<Vec<String>> {
    let export = export
        .canonicalize()
        .context("resolve source export path")?;
    let scratch = scratch
        .canonicalize()
        .context("resolve verifier scratch path")?;
    let git_view = git_view
        .canonicalize()
        .context("resolve trusted git view path")?;
    ensure!(
        std::fs::metadata(&profile.bwrap)?.permissions().mode() & 0o111 != 0,
        "isolation unavailable: bwrap is not executable"
    );
    let mut args = isolation_args(profile);
    args.extend([
        "--ro-bind".into(),
        export.to_string_lossy().into_owned(),
        export.to_string_lossy().into_owned(),
    ]);
    args.extend([
        "--bind".into(),
        scratch.to_string_lossy().into_owned(),
        scratch.to_string_lossy().into_owned(),
    ]);
    args.extend([
        "--ro-bind".into(),
        git_view.to_string_lossy().into_owned(),
        git_view.to_string_lossy().into_owned(),
    ]);
    args.extend([
        "--dev".into(),
        "/dev".into(),
        "--proc".into(),
        "/proc".into(),
        "--chdir".into(),
        export.to_string_lossy().into_owned(),
    ]);
    args.push(spec.program.to_string_lossy().into_owned());
    args.extend(spec.args.iter().cloned());
    Ok(args)
}
/// S3(c): launch the verifier through its fixed bwrap profile over the
/// source export. No fallback to a direct command exists on this path.
/// The trusted launcher materializes the committed tree BEFORE the isolated
/// process is built, so the verifier sees the committed source bytes rather
/// than the model's mutable checkout. The verifier
/// starts from an EMPTY environment (`env_clear`) plus `verifier_env`.
#[cfg(target_os = "linux")]
fn verify_sandboxed(
    profile: &BwrapProfile,
    spec: &CommandSpec,
    export: &Path,
    scratch: &Path,
    git_view: &Path,
    log: File,
    env: &[(&str, &str)],
) -> Result<i32> {
    // HOME/TMPDIR/GIT_DIR must name the paths as bound inside the sandbox.
    let scratch = scratch
        .canonicalize()
        .context("resolve verifier scratch path")?;
    let git_view = git_view
        .canonicalize()
        .context("resolve trusted git view path")?;
    let args = verify_sandbox_args(profile, export, &scratch, &git_view, spec)?;
    let mut cmd = Command::new(&profile.bwrap);
    cmd.args(&args)
        .env_clear()
        .envs(verifier_env(profile, spec, &scratch, &git_view, env));
    // RVS3-INT1: the verifier runs model-authored fixture code, so its
    // output reaches worker.log only through the worker's bounded pipe.
    run_with_output_pipe(cmd, log, "verifier")
}
fn execute(owned: &mut Owned) -> Result<()> {
    // Every decision input comes from the owned in-memory record, cloned out
    // of it here, never from a re-read of the control record.
    let request = owned.expected.request.clone();
    let target = owned.expected.target.clone();
    let store = owned.store;
    let r = &request;
    let job_dir = store.dir(&r.request_id)?;
    let id = &r.request_id;
    init_git_home(&store.root.join(format!("{id}.git-home")))?;
    let repo = job_dir.join("repo");
    ensure!(!repo.exists(), "checkout already exists; refusing reuse");
    let remote_ref = format!("refs/heads/{}", r.branch);
    let existing = text(
        git(
            None,
            &["ls-remote", "--heads", "--", &target.repo_url, &remote_ref],
        )?,
        "remote ownership check",
    )?;
    ensure!(
        existing.is_empty(),
        "remote branch already exists; refusing overwrite"
    );
    text(
        git(
            None,
            &[
                "clone",
                "--no-local",
                "--no-checkout",
                "--",
                &target.repo_url,
                repo.to_str().context("non-UTF8 repo path")?,
            ],
        )?,
        "clone",
    )?;

    git_text(
        &repo,
        &["checkout", "-b", &r.branch, &r.baseline_sha],
        "checkout",
    )?;
    ensure!(
        git_text(&repo, &["rev-parse", "HEAD"], "baseline")?.eq_ignore_ascii_case(&r.baseline_sha),
        "baseline mismatch"
    );
    // S3(c)/RVS3C3: the trusted Git object view, a fresh worker-owned bare
    // repository filled with the frozen baseline NOW, from the worker's own
    // clone, BEFORE the model runs. After the model exits no Git command runs
    // in the checkout: the result commit is built in this view from the work
    // tree files (`publishable_commit`); the export, the verifier's read-only
    // GIT_DIR (HEAD = the result) and the push all use this view.
    let git_view = store.root.join(format!("{id}.git-view"));
    fs::remove_dir_all(&git_view).ok();
    let view = git_view.to_str().context("non-UTF8 git view path")?;
    text(
        git(None, &["init", "--quiet", "--bare", "--", view])?,
        "create git view",
    )?;
    let src = repo.to_str().context("non-UTF8 repo path")?;
    let want = format!("+{}:refs/awili/baseline", r.baseline_sha);
    let fetch = ["fetch", "--quiet", "--no-tags", "--", src, &want];
    text(git(Some(&git_view), &fetch)?, "fill trusted git view")?;
    // S3(b): the model runs only through the fixed bwrap profile. Preflight
    // proves the isolation facility before the model log is even created; a
    // missing bwrap, an unavailable user namespace, or an unlisted path
    // fails the job here, before model execution. No fallback to direct
    // Command::status exists on this path.
    let isolation = target
        .isolation
        .as_ref()
        .ok_or_else(|| anyhow!("model isolation profile is required"))?;
    ensure_isolation_available(&isolation.bwrap)?;
    // Provenance file lives outside the model's checkout, never committed.
    store.write_spec(&r.request_id, &r.spec_text)?;
    owned.guard()?;
    let model_log = store.log_for_append(&r.request_id)?;
    // S3(b): a fresh private scratch for this model run only; bound at
    // SANDBOX_SCRATCH and removed as soon as the model exits, before any
    // later step (commit, verification, push) runs.
    let scratch = JobScratch::create(&job_dir)?;
    let rc = command_sandboxed(
        &isolation.bwrap,
        &target.command,
        &repo,
        &scratch.path,
        model_log,
        &[
            ("DSH_MODEL", &target.model),
            ("DSH_TELEMETRY_MODE", "DISABLED"),
        ],
        Some(&r.spec_text),
    );
    drop(scratch);
    let rc = rc.context("model launch failed")?;
    owned.transition(|s| s.model_exit = Some(rc))?;
    ensure!(rc == 0, "model exited {rc}");
    // RVS3C3: the checkout and its .git (config, hooks, attributes, commits,
    // branch, remote) are untrusted output. The result is built from the work
    // tree files in the trusted view on the frozen baseline; the push below
    // uses only the frozen branch and remote.
    let stage = store.root.join(format!("{id}.git-stage"));
    let sha = publishable_commit(&git_view, &stage, &repo, &r.baseline_sha)?;
    ensure!(
        git(
            Some(&git_view),
            &["merge-base", "--is-ancestor", &r.baseline_sha, &sha]
        )?
        .status
        .success(),
        "result does not descend from submitted baseline"
    );
    owned.guard()?;
    if sha == r.baseline_sha {
        owned.transition(|s| {
            s.no_op = true;
            s.result_sha = Some(sha.clone());
        })?;
        bail!("no implementation changes");
    }
    owned.transition(|s| {
        s.result_sha = Some(sha.clone());
        s.state = BuildState::Verifying;
    })?;
    owned.guard()?;
    // S3(c): verify the committed result, not the model's live checkout.
    // The export contains exactly the objects reachable from the committed
    // SHA, so any file the model planted but left uncommitted (ignored or
    // otherwise) cannot exist in it. The frozen baseline and result SHAs
    // are passed as validated environment arguments only: the verifier
    // never rereads the mutable record, the checkout, or the remote.
    // The verifier's fixed bwrap profile is required; there is no
    // direct-command fallback on this path. Preflight proves the facility
    // before any verifier command is started.
    let verifier_profile = &target
        .isolation
        .as_ref()
        .ok_or_else(|| anyhow!("verifier isolation profile is required"))?
        .verifier;
    ensure_isolation_available(verifier_profile)?;
    let verify_scratch = store.root.join(format!("{id}.verify-scratch"));
    fs::remove_dir_all(&verify_scratch).ok();
    fs::create_dir_all(verify_scratch.join("home"))?;
    fs::create_dir_all(verify_scratch.join("tmp"))?;
    text(
        git(Some(&git_view), &["update-ref", "--no-deref", "HEAD", &sha])?,
        "pin trusted git view HEAD",
    )?;
    // Materialize only the result commit's blobs in the source export view.
    // Anything else under the job root stays out of the verifier's view.
    let export_view = store.root.join(format!("{id}.export-view"));
    fs::remove_dir_all(&export_view).ok();
    fs::create_dir_all(&export_view)?;
    export_committed_tree(&git_view, &sha, &export_view)?;
    let verify_log = store.log_for_append(id)?;
    let rc = verify_sandboxed(
        verifier_profile,
        &target.verify,
        &export_view,
        &verify_scratch,
        &git_view,
        verify_log,
        &[
            ("AWILI_BASELINE_SHA", &r.baseline_sha),
            ("AWILI_RESULT_SHA", &sha),
        ],
    )
    .context("verifier launch failed")?;
    owned.transition(|s| s.verify_exit = Some(rc))?;
    ensure!(rc == 0, "verification exited {rc}");
    // The push publishes the exact verified SHA: recheck the trusted view's
    // result ref (never the model's checkout).
    let verified = git_text(&git_view, &["rev-parse", "refs/awili/result"], "verified")?;
    ensure!(
        verified == sha,
        "trusted view result differs from the verified commit"
    );
    owned.guard()?;
    // Use the frozen destination, not model-editable remote configuration. Lease permits creation only.
    // S3(c): push the verified SHA from the trusted Git view, never from the
    // checkout, whose .git config (pushInsteadOf, insteadOf) the model wrote.
    let lease = format!("--force-with-lease={remote_ref}:");
    let refspec = format!("{sha}:{remote_ref}");
    let push = git(
        Some(&git_view),
        &["push", &lease, "--", &target.repo_url, &refspec],
    )?;
    let rc = push.status.code().unwrap_or(-1);
    owned.transition(|s| s.push_exit = Some(rc))?;
    ensure!(rc == 0, "push exited {rc}");
    let remote = text(
        git(
            None,
            &["ls-remote", "--heads", "--", &target.repo_url, &remote_ref],
        )?,
        "remote SHA verification",
    )?;
    let remote_sha = remote.split_whitespace().next().unwrap_or("").to_string();
    owned.transition(|s| s.remote_sha = Some(remote_sha.clone()))?;
    ensure!(remote_sha == sha, "remote SHA differs from verified result");
    owned.transition(|s| s.state = BuildState::AwaitingReview)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worker's Git home is process-global: tests that set it run one
    /// at a time.
    static GIT_HOME_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(target_os = "linux")]
    #[test]
    fn committed_export_ignores_archive_attributes() {
        let _guard = GIT_HOME_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        init_git_home(&tmp.path().join("home")).unwrap();
        let repo = tmp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        assert!(git(Some(&repo), &["init"]).unwrap().status.success());
        fs::write(repo.join(".gitattributes"), "hidden export-ignore\nsubst export-subst\n").unwrap();
        fs::write(repo.join("hidden"), b"committed bytes\n").unwrap();
        fs::write(repo.join("subst"), b"$Format:%H$\n").unwrap();
        use std::os::unix::ffi::OsStringExt;
        let odd = std::ffi::OsString::from_vec(b"nonutf8-\xff".to_vec());
        fs::write(repo.join(&odd), b"odd bytes").unwrap();
        fs::write(repo.join("executable"), b"script").unwrap();
        fs::set_permissions(repo.join("executable"), fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("hidden", repo.join("link")).unwrap();
        assert!(git(Some(&repo), &["add", "."]).unwrap().status.success());
        assert!(git(Some(&repo), &["-c", "user.name=test", "-c", "user.email=test@example.invalid", "commit", "-m", "fixture"]).unwrap().status.success());
        let sha = git_text(&repo, &["rev-parse", "HEAD"], "fixture sha").unwrap();
        let export = tmp.path().join("export"); fs::create_dir(&export).unwrap();
        export_committed_tree(&repo, &sha, &export).unwrap();
        assert_eq!(fs::read(export.join("hidden")).unwrap(), b"committed bytes\n");
        assert_eq!(fs::read(export.join("subst")).unwrap(), b"$Format:%H$\n");
        assert_eq!(fs::read(export.join(".gitattributes")).unwrap(), fs::read(repo.join(".gitattributes")).unwrap());
        assert_eq!(fs::read(export.join(odd)).unwrap(), b"odd bytes");
        assert_eq!(fs::read_link(export.join("link")).unwrap(), PathBuf::from("hidden"));
        assert_eq!(fs::metadata(export.join("executable")).unwrap().permissions().mode() & 0o777, 0o755);
    }

    /// A job that ends Failed has its reason in worker.log, written by the
    /// worker itself before it publishes the state. (The reason used to be
    /// printed by the binary only after `run` returned, i.e. after Failed was
    /// visible, so a reader polling status could find the log without it; on
    /// hosta under load, isolation_unavailable_fails_closed did.) This test is
    /// the worker: the job names this process, and it fails at its first
    /// check because its checkout already exists.
    #[test]
    fn a_failure_is_in_the_log_before_the_state_is_published() {
        let _git_home = GIT_HOME_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("jobs");
        let store = Store::open(&root).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let command = |program: &str| CommandSpec {
            program: program.into(),
            args: vec![],
            env: Default::default(),
        };
        let mut record = JobRecord {
            request: BuildRequest {
                request_id: id.clone(),
                turn_id: "turn".into(),
                conversation_id: "conv".into(),
                repo_id: "repo".into(),
                baseline_sha: "a".repeat(40),
                branch: "fix/log-before-state".into(),
                spec_text: "spec".into(),
                target_id: "local".into(),
                verify_profile: "test".into(),
            },
            target: BuildTarget {
                repo_id: "repo".into(),
                repo_url: "/nonexistent/remote.git".into(),
                model: "fixture".into(),
                command: command("/usr/bin/true"),
                verify_profile: "test".into(),
                verify: command("/usr/bin/true"),
                isolation: None,
            },
            snapshot: BuildSnapshot {
                request_id: id.clone(),
                request_sha256: String::new(),
                revision: 0,
                state: BuildState::Queued,
                executor: "/usr/bin/true".into(),
                requested_model: "fixture".into(),
                baseline_sha: "a".repeat(40),
                result_sha: None,
                remote_sha: None,
                model_exit: None,
                verify_exit: None,
                push_exit: None,
                worker: crate::process::identity(std::process::id()),
                failure: None,
                no_op: false,
            },
        };
        record.snapshot.request_sha256 = record.request.fingerprint();
        assert!(record.snapshot.worker.is_some(), "own identity unavailable");
        store.create_job(&id).unwrap();
        store.write(&mut record).unwrap();
        fs::create_dir(store.dir(&id).unwrap().join("repo")).unwrap();

        assert!(run(&root, &id).is_err());
        let end = store.read(&id).unwrap().snapshot;
        assert_eq!(end.state, BuildState::Failed, "{:?}", end.failure);
        let failure = end.failure.unwrap();
        assert!(failure.contains("checkout already exists"), "{failure}");
        let log = fs::read_to_string(root.join(&id).join("control/worker.log")).unwrap_or_default();
        assert!(
            log.contains(&failure),
            "{failure:?} not in worker.log {log:?}"
        );
    }

    /// RC2-R4 (TOP43 #13): the worker's Git refuses every transport but
    /// ssh, https and file, before any helper or connection runs. (The only
    /// test in this binary that runs Git; GIT_HOME_TESTS orders it with the
    /// other one that sets the worker's Git home.)
    #[test]
    fn rc2r4_the_workers_git_refuses_other_transports() {
        let _git_home = GIT_HOME_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let temp = tempfile::tempdir().unwrap();
        init_git_home(&temp.path().join("git-home")).unwrap();
        for url in ["http://127.0.0.1:9/x.git", "git://127.0.0.1:9/x.git"] {
            let out = git(None, &["ls-remote", "--", url]).unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(!out.status.success(), "{url}: {err}");
            assert!(err.contains("not allowed"), "{url}: {err}");
        }
        // A local bare repository (file) still works.
        let bare = temp.path().join("r.git");
        let made = git(None, &["init", "--bare", "-q", bare.to_str().unwrap()]).unwrap();
        assert!(made.status.success());
        let url = format!("file://{}", bare.display());
        let out = git(None, &["ls-remote", "--", &url]).unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
