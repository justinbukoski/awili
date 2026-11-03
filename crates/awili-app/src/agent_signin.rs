//! RC2-R56 and RC2-R57 (usability RC2 round 4, signed-out Claude): whether a helper's
//! CLI is signed in, and the one plain line Awili shows when a turn failed
//! because it is not.
//!
//! - RC2-R56, a failed turn: Claude's own words ("Failed to authenticate: OAuth
//!   session expired and could not be refreshed") never say what to do, and
//!   Work printed them twice. Awili shows one line that does
//!   ([`signed_out_line`]).
//! - RC2-R57, Settings > Code said "✓ Installed" for a Claude Code that was signed
//!   out. The CLIs report their sign-in cheaply, with no model call:
//!   `claude auth status` (JSON `loggedIn`, exit 1 when signed out) and
//!   `codex login status` (exit 0 "Logged in using …", exit 1 "Not logged
//!   in") ([`sign_in_state`]).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use awili_code::CodeEngine;

/// Whether a helper's CLI is signed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignIn {
    SignedIn,
    SignedOut,
    /// The CLI could not say (it would not start, or it has no such
    /// command): Awili claims neither.
    Unknown,
}

/// How to sign `engine` in again, in one sentence (`None`: Awili knows no
/// command for it).
pub(crate) fn sign_in_steps(engine: CodeEngine) -> Option<&'static str> {
    match engine {
        CodeEngine::Claude => Some("Open Terminal, run claude, then type /login."),
        CodeEngine::Codex => Some("Open Terminal and run codex login."),
        _ => None,
    }
}

/// RC2-R610 (usability RC2 round 5, Minor 9: ZCode and DeepSeek Harness said
/// only "✓ Installed"): these two work with a key kept in a file, not a
/// sign-in. Whether that key is there (never what it is): ZCode's in
/// `~/.zcode/cli/config.json` (any provider's `options.apiKey`), the
/// DeepSeek Harness's `DEEPSEEK_API_KEY=` in its env file (`dsh_env_file`).
/// `None` for a helper that signs in instead. Reading a small file, it
/// needs no deadline.
pub(crate) fn key_state(
    engine: CodeEngine,
    home: &std::path::Path,
    dsh_env_file: &str,
) -> Option<SignIn> {
    match engine {
        CodeEngine::Glm => Some(zcode_key(&home.join(".zcode/cli/config.json"))),
        CodeEngine::Dsh => {
            if dsh_env_file.trim().is_empty() {
                return Some(SignIn::SignedOut);
            }
            let path = match dsh_env_file.strip_prefix("~/") {
                Some(rest) => home.join(rest),
                None => std::path::PathBuf::from(dsh_env_file),
            };
            Some(dsh_key(&path))
        }
        _ => None,
    }
}

/// RC2-R610: how to set a key-based helper's key, in one sentence.
pub(crate) fn key_steps(engine: CodeEngine, dsh_env_file: &str) -> Option<String> {
    match engine {
        CodeEngine::Glm => Some("Open ZCode and set your coding-plan key there.".to_string()),
        CodeEngine::Dsh if dsh_env_file.trim().is_empty() => Some(
            "Set DEEPSEEK_API_KEY for the DeepSeek Harness, or name a key file in Settings."
                .to_string(),
        ),
        CodeEngine::Dsh => Some(format!(
            "Put your DeepSeek key in {dsh_env_file} as DEEPSEEK_API_KEY=\u{2026}"
        )),
        _ => None,
    }
}

fn zcode_key(path: &std::path::Path) -> SignIn {
    let Ok(text) = std::fs::read_to_string(path) else {
        return SignIn::SignedOut;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return SignIn::Unknown;
    };
    let set = v
        .get("provider")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|providers| {
            providers.values().any(|p| {
                p.pointer("/options/apiKey")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|k| !k.trim().is_empty())
            })
        });
    if set {
        SignIn::SignedIn
    } else {
        SignIn::SignedOut
    }
}

fn dsh_key(path: &std::path::Path) -> SignIn {
    let Ok(text) = std::fs::read_to_string(path) else {
        return SignIn::SignedOut;
    };
    let set = text.lines().any(|line| {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        line.strip_prefix("DEEPSEEK_API_KEY=")
            .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\''))
            .is_some_and(|v| !v.is_empty())
    });
    if set {
        SignIn::SignedIn
    } else {
        SignIn::SignedOut
    }
}

/// The helper a sign-in failure names, from its own words.
fn engine_named(text: &str) -> Option<CodeEngine> {
    let l = text.to_ascii_lowercase();
    if l.contains("/login") || l.contains("oauth") || l.contains("claude") {
        Some(CodeEngine::Claude)
    } else if l.contains("codex login") || l.contains("chatgpt") || l.contains("codex") {
        Some(CodeEngine::Codex)
    } else {
        None
    }
}

/// Whether `text` (a CLI's failure) says it is not signed in.
pub(crate) fn is_signed_out_failure(text: &str) -> bool {
    let l = text.to_ascii_lowercase();
    [
        "failed to authenticate",
        "oauth session expired",
        "not logged in",
        "please run /login",
        "run /login",
        "please log in",
        "codex login",
        "invalid api key",
        "authentication_error",
    ]
    .iter()
    .any(|w| l.contains(w))
}

/// The one plain line for a turn that failed because the helper is signed
/// out ("Claude is signed out. Open Terminal, run claude, then type /login.
/// Then press Retry."), or `None` for any other failure. `engine`: the
/// helper that ran, when known; else it is read from the failure's words.
pub(crate) fn signed_out_line(engine: Option<CodeEngine>, text: &str) -> Option<String> {
    if !is_signed_out_failure(text) {
        return None;
    }
    let engine = engine
        .filter(|e| sign_in_steps(*e).is_some())
        .or_else(|| engine_named(text));
    Some(
        match engine.and_then(|e| sign_in_steps(e).map(|s| (e, s))) {
            Some((e, steps)) => format!(
                "{} is signed out. {steps} Then press Retry.",
                helper_name(e)
            ),
            None => "The helper is signed out. Sign it in again in Terminal, then press Retry."
                .to_string(),
        },
    )
}

/// Recognize already-normalized required-action text as well as raw failures.
pub(crate) fn is_signed_out_notice(text: &str) -> bool {
    is_signed_out_failure(text)
        || [CodeEngine::Claude, CodeEngine::Codex]
            .into_iter()
            .any(|engine| {
                sign_in_steps(engine).is_some_and(|steps| {
                    text == format!(
                        "{} is signed out. {steps} Then press Retry.",
                        helper_name(engine)
                    )
                })
            })
        || text == "The helper is signed out. Sign it in again in Terminal, then press Retry."
}

fn helper_name(engine: CodeEngine) -> &'static str {
    match engine {
        CodeEngine::Claude => "Claude",
        CodeEngine::Codex => "Codex",
        _ => "The helper",
    }
}

/// Sign-in states already asked, by CLI path (cleared by "Check again").
///
/// RC2-R5M7 (slice review 3775ed08, P2): with how many times "Check again"
/// has forgotten them, under the SAME lock: a check stores its answer only
/// when no "Check again" came since it started, tested and stored in one
/// step ([`remember_sign_in`]), so a "Check again" can never land between
/// the test and the store and leave an obsolete answer behind.
static ASKED: Mutex<Option<Asked>> = Mutex::new(None);

/// What [`ASKED`] holds.
#[derive(Default)]
struct Asked {
    answers: HashMap<String, SignIn>,
    /// Bumped by every [`forget_sign_in`].
    generation: u64,
}

fn asked() -> std::sync::MutexGuard<'static, Option<Asked>> {
    ASKED.lock().unwrap_or_else(|e| e.into_inner())
}

/// RC2-R5M4 (slice review befb867d, P2): how long a status command may run
/// before Awili stops waiting for it (and stops it): a CLI that hangs never
/// holds a check, and the state stays `Unknown` (claimed neither way).
pub(crate) const STATUS_DEADLINE: Duration = Duration::from_secs(3);

/// RC2-R5M4: the checks under way (by the [`ASKED`] key).
static CHECKING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// The status command of `engine`'s CLI (`None`: it has none).
pub(crate) fn status_command(engine: CodeEngine) -> Option<&'static [&'static str]> {
    match engine {
        CodeEngine::Claude => Some(&["auth", "status", "--json"]),
        CodeEngine::Codex => Some(&["login", "status"]),
        _ => None,
    }
}

fn asked_key(engine: CodeEngine, bin: &str) -> String {
    format!("{}\u{0}{bin}", engine.bin())
}

/// Whether the CLI at `bin` (for `engine`) is signed in. Asked once per
/// path (a fraction of a second, no model call) and remembered until
/// [`forget_sign_in`]; `Unknown` for a helper with no status command.
/// Blocking, for at most [`STATUS_DEADLINE`]: never on the UI thread
/// ([`known_sign_in`] and [`begin_sign_in_check`] are the UI's side).
pub(crate) fn sign_in_state(engine: CodeEngine, bin: &str) -> SignIn {
    sign_in_state_within(engine, bin, status_deadline())
}

/// The deadline [`sign_in_state`] uses: [`STATUS_DEADLINE`] in the app. In
/// test builds a stand-in CLI that must answer gets
/// [`crate::exec_fixture::ANSWER_DEADLINE`]: on macOS the first exec of a
/// freshly written stand-in can stall past 3 s under load (the system scans
/// the new executable), which flaked the Mac gate (K3b, K4b) by reading a
/// quick answer as `Unknown` (it took 3.18 s). A test whose point is a hung
/// CLI calls `use_hung_cli_deadline` first and gets
/// `TEST_STATUS_DEADLINE`, so its check (which may ask twice: 2 × 8 s)
/// still ends well before the 30 s hang; the production deadline itself is
/// covered directly by `sign_in_state_within` with [`STATUS_DEADLINE`].
#[cfg(not(test))]
fn status_deadline() -> Duration {
    STATUS_DEADLINE
}

/// See the app build's `status_deadline`.
#[cfg(test)]
fn status_deadline() -> Duration {
    if HUNG_CLI_TEST.load(std::sync::atomic::Ordering::Acquire) {
        TEST_STATUS_DEADLINE
    } else {
        crate::exec_fixture::ANSWER_DEADLINE
    }
}

/// Set by [`use_hung_cli_deadline`]; process-wide, so only for tests that
/// run alone in their own child process.
#[cfg(test)]
static HUNG_CLI_TEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// For a test whose point is a hung status command (run alone in its own
/// child process): from now on [`sign_in_state`] gives up at
/// [`TEST_STATUS_DEADLINE`] instead of waiting the answer budget.
#[cfg(test)]
pub(crate) fn use_hung_cli_deadline() {
    HUNG_CLI_TEST.store(true, std::sync::atomic::Ordering::Release);
}

/// The hung-CLI tests' status deadline (see [`use_hung_cli_deadline`]).
#[cfg(test)]
pub(crate) const TEST_STATUS_DEADLINE: Duration = Duration::from_secs(8);

/// [`sign_in_state`] with its own deadline.
pub(crate) fn sign_in_state_within(engine: CodeEngine, bin: &str, deadline: Duration) -> SignIn {
    let Some(args) = status_command(engine) else {
        return SignIn::Unknown;
    };
    let key = asked_key(engine, bin);
    let generation = {
        let mut asked = asked();
        let asked = asked.get_or_insert_with(Asked::default);
        if let Some(known) = asked.answers.get(&key) {
            return *known;
        }
        asked.generation
    };
    let state = match ask_cli(bin, args, deadline) {
        Some((success, stdout)) => read_status(engine, success, &stdout),
        None => SignIn::Unknown,
    };
    remember_sign_in(key, state, generation);
    state
}

/// RC2-R5M7: keep `state` for `key` when no "Check again" came since the
/// check began (`generation`); a "Check again" while it ran means the
/// answer may be stale, and the next check asks anew. The test and the
/// store are one step under [`ASKED`]'s lock.
fn remember_sign_in(key: String, state: SignIn, generation: u64) {
    let mut asked = asked();
    let asked = asked.get_or_insert_with(Asked::default);
    if asked.generation == generation {
        #[cfg(test)]
        race_hooks::between_test_and_store(&key);
        asked.answers.insert(key, state);
    }
}

/// RC2-R5M4: the remembered state, without asking anything: `None` when the
/// CLI has not been asked yet (or since "Check again").
pub(crate) fn known_sign_in(engine: CodeEngine, bin: &str) -> Option<SignIn> {
    status_command(engine)?;
    asked()
        .as_ref()
        .and_then(|a| a.answers.get(&asked_key(engine, bin)).copied())
}

/// RC2-R5M4: mark a check of `bin` as under way. `false` when one already
/// is (the caller starts none).
pub(crate) fn begin_sign_in_check(engine: CodeEngine, bin: &str) -> bool {
    checking(|c| c.insert(asked_key(engine, bin)))
}

/// RC2-R5M4: the check of `bin` is over.
pub(crate) fn end_sign_in_check(engine: CodeEngine, bin: &str) {
    checking(|c| c.remove(&asked_key(engine, bin)));
}

fn checking<T>(f: impl FnOnce(&mut HashSet<String>) -> T) -> T {
    f(CHECKING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashSet::new))
}

/// Run `bin args` and wait at most `deadline` for it to finish: its exit
/// success and what it printed, or `None` when it could not start or did
/// not finish in time (then it is stopped).
fn ask_cli(bin: &str, args: &[&str], deadline: Duration) -> Option<(bool, Vec<u8>)> {
    use std::io::Read;
    let started = Instant::now();
    // Windows: never a bare name (see agent_bin::resolve_program); elsewhere as is.
    let program = crate::agent_bin::resolve_program(std::ffi::OsStr::new(bin)).ok()?;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    // RC2-R610 (usability RC2 round 5, Minor 9: Codex showed only
    // "✓ Installed"): `codex login status` says "Logged in using ChatGPT" on
    // stderr, not stdout.
    // Each pipe is drained on its own thread, so a CLI filling one never
    // blocks on the other.
    let err_reader = std::thread::spawn(move || {
        let mut err = Vec::new();
        let _ = stderr.read_to_end(&mut err);
        err
    });
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let err = err_reader.join().unwrap_or_default();
        let _ = tx.send((out, err));
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let left = deadline.saturating_sub(started.elapsed());
                return rx.recv_timeout(left).ok().map(|(out, err)| {
                    // A CLI that answers on stdout (Claude's JSON) is read
                    // there alone; one that printed nothing there, on stderr.
                    let said = if out.iter().all(u8::is_ascii_whitespace) {
                        err
                    } else {
                        out
                    };
                    (status.success(), said)
                });
            }
            Ok(None) if started.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// What a status command's answer says.
pub(crate) fn read_status(engine: CodeEngine, success: bool, stdout: &[u8]) -> SignIn {
    let text = String::from_utf8_lossy(stdout);
    match engine {
        CodeEngine::Claude => match serde_json::from_str::<serde_json::Value>(text.trim())
            .ok()
            .and_then(|v| v.get("loggedIn").and_then(|b| b.as_bool()))
        {
            Some(true) => SignIn::SignedIn,
            Some(false) => SignIn::SignedOut,
            None => SignIn::Unknown,
        },
        CodeEngine::Codex => {
            let l = text.to_ascii_lowercase();
            if l.contains("not logged in") {
                SignIn::SignedOut
            } else if success && l.contains("logged in") {
                SignIn::SignedIn
            } else {
                SignIn::Unknown
            }
        }
        _ => SignIn::Unknown,
    }
}

/// "Check again": ask every CLI anew next time.
pub(crate) fn forget_sign_in() {
    // RC2-R5M7: forgotten, and the generation bumped, in one step under the
    // lock a check stores under: a check under way keeps its answer to
    // itself.
    let mut asked = match ASKED.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            #[cfg(test)]
            race_hooks::forget_waits();
            asked()
        }
    };
    let asked = asked.get_or_insert_with(Asked::default);
    asked.generation += 1;
    asked.answers.clear();
}

/// RC2-R5M7: test hooks that drive a "Check again" into a check's store,
/// step by step (no timing).
#[cfg(test)]
pub(crate) mod race_hooks {
    use std::sync::Mutex;

    /// Run inside a check's store, between its generation test and its
    /// insertion, for the key it names.
    type Between = (String, Box<dyn FnMut() + Send>);
    static BETWEEN: Mutex<Option<Between>> = Mutex::new(None);
    /// Told when [`super::forget_sign_in`] on the named thread finds the
    /// answers held by a check's store and waits for it.
    type Waits = (std::thread::ThreadId, std::sync::mpsc::Sender<()>);
    static WAITS: Mutex<Option<Waits>> = Mutex::new(None);

    pub(crate) fn on_between_test_and_store(key: String, f: Box<dyn FnMut() + Send>) {
        *BETWEEN.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, f));
    }

    pub(crate) fn on_forget_waits(thread: std::thread::ThreadId, tx: std::sync::mpsc::Sender<()>) {
        *WAITS.lock().unwrap_or_else(|e| e.into_inner()) = Some((thread, tx));
    }

    pub(super) fn between_test_and_store(key: &str) {
        let hook = {
            let mut slot = BETWEEN.lock().unwrap_or_else(|e| e.into_inner());
            match slot.as_ref() {
                Some((k, _)) if k == key => slot.take(),
                _ => None,
            }
        };
        if let Some((_, mut f)) = hook {
            f();
        }
    }

    pub(super) fn forget_waits() {
        let me = std::thread::current().id();
        if let Some((t, tx)) = WAITS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if *t == me {
                let _ = tx.send(());
            }
        }
    }
}

#[cfg(test)]
#[path = "agent_signin_tests.rs"]
mod tests;
