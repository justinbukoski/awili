//! RC2-CXS (R3-20 Codex part): one Codex turn over `codex app-server`, so
//! the reply text streams into Work, Chat and Code as Codex writes it
//! (`codex exec --json` prints a message only once it is complete).
//!
//! The protocol is the pure [`awili_code::app_server::AppServerTurn`]; this
//! is the process around it, in the shape of the exec driver
//! (`code_provider::run_code_stream`): the owner's own Codex (his
//! `CODEX_HOME`, config and MCP servers — nothing is copied), the same `-c`
//! overrides, its own process group so Stop kills it and everything it
//! started, stderr drained on its own thread, and one terminal `Done` from
//! [`crate::code_provider::finalize`].
//!
//! Decided at the start of each turn: when the server does not answer
//! `initialize` (a Codex with no app server exits at once with "unrecognized
//! subcommand"; one that hangs is given [`HANDSHAKE_TIMEOUT`]), nothing has
//! been shown, the child is reaped and [`run_turn`] hands the Stop receiver
//! back so the caller runs the turn with `codex exec --json` instead. That
//! binary is remembered for the rest of the session
//! ([`exec_fallback_for`]). RC2-CXSF: the run that took the fallback is
//! told so (`CodeProvider::observe_exec`), so only ITS quiet wording says
//! Codex shows each message when it is written.

use crate::proc_compat::OwnProcessGroup;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use awili_code::app_server::{AppServerTurn, Handshake, TurnSpec};
use awili_code::CodeEvent;
use awili_providers::ProviderEvent;

/// How long the server has to answer `initialize` before the turn runs on
/// `codex exec --json` instead. The answer comes at once (the MCP servers
/// start later, with the thread); this only catches a server that hangs.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the server has to exit after its turn completed and its stdin
/// closed, before its group is killed.
const EXIT_GRACE: Duration = Duration::from_secs(3);

/// RC2-CXSF (RC2-CXS review P1): the server's output ended before Codex
/// said the turn was over, so the reply is not finished. Text already shown
/// stays as it was.
pub(crate) const ENDED_EARLY: &str = "Codex ended before the reply was finished.";

/// How often the handshake looks for a Stop while it waits.
const HANDSHAKE_POLL: Duration = Duration::from_millis(50);

/// The Codex binaries that answered no `initialize` this session.
static EXEC_FALLBACK: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Whether `bin` is known to have no app server (its turns run on exec).
pub(crate) fn exec_fallback_for(bin: &str) -> bool {
    EXEC_FALLBACK
        .lock()
        .map(|g| g.iter().any(|b| b == bin))
        .unwrap_or(false)
}

fn remember_fallback(bin: &str) {
    if let Ok(mut g) = EXEC_FALLBACK.lock() {
        if !g.iter().any(|b| b == bin) {
            g.push(bin.to_string());
        }
    }
}

/// Everything one app-server turn needs.
pub(crate) struct Turn<'a> {
    pub bin: &'a str,
    /// The full argv after the binary (`app-server`, then every `-c`).
    pub args: &'a [String],
    pub cwd: &'a str,
    /// The child's `PATH` (`code_provider::build_child_path`).
    pub child_path: &'a str,
    /// rc.4 S7: the seatbelt profile to start under (`None` = unsandboxed).
    pub sandbox: Option<&'a std::path::Path>,
    /// RC2-R2I: the council the turn answers in ("" = none).
    pub owner: &'a str,
    pub spec: TurnSpec,
}

// ---- RC2-R2F10: a server made ready before the question ----------------------
//
// Measured live 2026-10-07 (codex-cli 0.160.1, `RC2-R2F10.md`): from the spawn,
// Codex answers `initialize` in 0.05 s and opens a thread in 0.2 s, but after
// `turn/start` it first starts the owner's MCP servers (~1.7 s) before his
// hooks run (SessionStart ~7.5 s, UserPromptSubmit ~1.4 s) and the model
// writes. Awili owns the spawn and the MCP start-up: a council member's
// server is started, and its thread opened, while the owner reads; his next
// message is asked on it at once (the hooks are his own and still run).

/// RC2-R2F10: servers are made ready ahead (off under test: a test turns it
/// on to prove it).
pub(crate) static PREWARM: AtomicBool = AtomicBool::new(!cfg!(test));

/// RC2-R2F10: how long a ready server waits for its question before it is
/// ended.
pub(crate) const WARM_TTL: Duration = Duration::from_secs(600);

/// RC2-R2G4: a test's shorter wait for the servers of one binary.
#[cfg(test)]
pub(crate) static TEST_TTL: Mutex<Vec<(String, Duration)>> = Mutex::new(Vec::new());

/// How long a ready server of `bin` waits for its question.
fn warm_ttl(bin: &str) -> Duration {
    #[cfg(test)]
    if let Some(ttl) = TEST_TTL
        .lock()
        .ok()
        .and_then(|t| t.iter().find(|(b, _)| b == bin).map(|(_, d)| *d))
    {
        return ttl;
    }
    let _ = bin;
    WARM_TTL
}

/// RC2-R2F10: at most this many ready servers at once.
const WARM_CAP: usize = 4;

/// What a ready server must match to take a turn: the same launch and the
/// same thread posture (the question is the only thing left).
#[derive(Clone, Debug, PartialEq, Eq)]
struct WarmKey {
    bin: String,
    args: Vec<String>,
    cwd: String,
    child_path: String,
    thread_cwd: String,
    autonomy: awili_code::Autonomy,
    model: Option<String>,
    /// RC2-R2I (RC2-R2F review P2 on d65da1fc: two councils with the same
    /// launch shared one ready server): the council it is kept for.
    owner: String,
    /// rc.4 S7: the profile it was started under.
    sandbox: Option<std::path::PathBuf>,
}

impl WarmKey {
    fn of(turn: &Turn<'_>) -> Self {
        Self {
            bin: turn.bin.to_string(),
            args: turn.args.to_vec(),
            cwd: turn.cwd.to_string(),
            child_path: turn.child_path.to_string(),
            thread_cwd: turn.spec.cwd.clone(),
            autonomy: turn.spec.autonomy,
            model: turn.spec.model.clone(),
            owner: turn.owner.to_string(),
            sandbox: turn.sandbox.map(std::path::Path::to_path_buf),
        }
    }
}

/// A running server and its pipes.
struct Server {
    child: Child,
    pgid: i32,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr_buf: Arc<Mutex<String>>,
    stderr_handle: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// End a server that will not (or no longer) run a turn.
    fn abandon(mut self) {
        drop(self.stdin.take());
        abandon(&mut self.child);
        crate::code_provider::deregister_group(self.pgid);
        if let Some(h) = self.stderr_handle.take() {
            crate::code_provider::join_stderr_with_deadline(h);
        }
    }
}

/// A server whose thread is open, waiting for its question.
struct Warm {
    key: WarmKey,
    server: Server,
    protocol: AppServerTurn,
    born: Instant,
    /// RC2-R2G4: how long it waits for its question.
    ttl: Duration,
}

/// RC2-R2F10: the ready servers.
static WARM: Mutex<Vec<Warm>> = Mutex::new(Vec::new());

/// RC2-R2H: councils archived or deleted (owners passed to
/// [`close_warm_for`]); a prewarm for one of them keeps nothing ready until
/// [`reopen_warm_for`] (unarchive).
static CLOSED: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(crate) fn is_closed(owner: &str) -> bool {
    CLOSED
        .lock()
        .map(|c| c.iter().any(|o| o == owner))
        .unwrap_or(false)
}

/// RC2-R2H: council `owner` is back (unarchived): it may be made ready again.
pub(crate) fn reopen_warm_for(owner: &str) {
    if let Ok(mut closed) = CLOSED.lock() {
        closed.retain(|o| o != owner);
    }
}

/// Spawn the server with its pipes and line reader (in its own process
/// group, registered so a quit ends it).
fn start_server(turn: &Turn<'_>) -> Option<Server> {
    let (program, full_args) =
        crate::agent_sandbox::launch_command(turn.sandbox, turn.bin, turn.args);
    let mut cmd = Command::new(&program);
    cmd.args(&full_args)
        .current_dir(turn.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, as the exec driver: Stop kills the server and
        // everything it started (MCP servers, commands) in one signal.
        .own_process_group()
        .env("PATH", turn.child_path)
        .env(
            "AWILI_NATIVE_RESUME_ID",
            turn.spec.resume_id.as_deref().unwrap_or(""),
        );
    let mut child = spawn_retrying_busy(&mut cmd)?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        abandon(&mut child);
        return None;
    };
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let stderr_handle = child
        .stderr
        .take()
        .map(|err| crate::code_provider::spawn_stderr_reader(err, Arc::clone(&stderr_buf)));
    // Lines arrive on a channel so the handshake can time out.
    let (line_tx, lines) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let pgid = child.id() as i32;
    crate::code_provider::register_group(pgid);
    Some(Server {
        child,
        pgid,
        stdin: Some(stdin),
        lines,
        stderr_buf,
        stderr_handle,
    })
}

/// Poll without releasing the Unix leader's pid reservation. Approval polling
/// must leave reaping to the synchronized group-cleanup path.
fn server_has_exited(child: &mut Child) -> bool {
    #[cfg(unix)]
    {
        crate::proc_compat::child_has_exited(child.id())
    }
    #[cfg(windows)]
    {
        child.try_wait().ok().flatten().is_some()
    }
}

/// End every ready server that has waited too long or has gone.
fn expire_warm(pool: &mut Vec<Warm>) {
    let mut keep = Vec::with_capacity(pool.len());
    for mut w in pool.drain(..) {
        #[cfg(unix)]
        let alive = !server_has_exited(&mut w.server.child);
        #[cfg(windows)]
        let alive = matches!(w.server.child.try_wait(), Ok(None));
        if alive && w.born.elapsed() < w.ttl {
            keep.push(w);
        } else {
            w.server.abandon();
        }
    }
    *pool = keep;
}

/// RC2-R2F10: make a server ready for `turn`'s launch (its question,
/// `turn.spec.prompt`, is not sent): spawned, `initialize` answered and its
/// thread opened. Blocks while it starts (call it off the UI thread). Does
/// nothing when one is ready already, when turning ahead is off, for a
/// resumed conversation, or for a Codex with no app server. RC2-R2G5,
/// RC2-R2I: it is `turn.owner`'s (a council's), taken only by that council's
/// turns and ended when it closes.
pub(crate) fn prewarm(turn: Turn<'_>) {
    let owner = turn.owner;
    if !PREWARM.load(Ordering::Acquire)
        || turn.spec.resume_id.is_some()
        || exec_fallback_for(turn.bin)
        || is_closed(owner)
    {
        return;
    }
    let key = WarmKey::of(&turn);
    if let Ok(mut pool) = WARM.lock() {
        expire_warm(&mut pool);
        if pool.iter().any(|w| w.key == key) {
            return;
        }
    }
    let Some(mut server) = start_server(&turn) else {
        return;
    };
    let mut protocol = AppServerTurn::warm(turn.spec);
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut ok = send_lines(&mut server.stdin, &protocol.start());
    while ok && !protocol.thread_ready() && !protocol.finished() {
        let left = deadline.saturating_duration_since(Instant::now());
        match server.lines.recv_timeout(left) {
            Ok(line) => {
                let step = protocol.on_line(&line);
                ok = send_lines(&mut server.stdin, &step.send);
            }
            Err(_) => ok = false,
        }
        if protocol.handshake() == Handshake::Unavailable {
            remember_fallback(turn.bin);
            ok = false;
        }
    }
    if !ok || !protocol.thread_ready() {
        server.abandon();
        return;
    }
    let Ok(mut pool) = WARM.lock() else {
        server.abandon();
        return;
    };
    // RC2-R2H: a council archived or deleted while this server was starting
    // keeps nothing ready (checked under the pool lock, after the close).
    if pool.iter().any(|w| w.key == key) || is_closed(owner) {
        drop(pool);
        server.abandon();
        return;
    }
    while pool.len() >= WARM_CAP {
        pool.remove(0).server.abandon();
    }
    let ttl = warm_ttl(turn.bin);
    pool.push(Warm {
        key,
        server,
        protocol,
        born: Instant::now(),
        ttl,
    });
    drop(pool);
    // RC2-R2G4 (RC2-R2F review P2: a ready server outlived its ten minutes
    // while no one asked anything): when its time is up it is ended, with
    // or without a later request.
    std::thread::spawn(move || {
        std::thread::sleep(ttl);
        if let Ok(mut pool) = WARM.lock() {
            expire_warm(&mut pool);
        }
    });
}

/// RC2-R2G5 (RC2-R2F review P2): end the ready servers made for council
/// `owner`, which was deleted or archived: taken off the list at once, their
/// processes ended off the caller's thread.
pub(crate) fn close_warm_for(owner: &str) {
    // RC2-R2H (RC2-R2F review P2 on d5cbd4ca): remember the close first, so a
    // prewarm already starting, or one asked for after this, keeps nothing.
    if let Ok(mut closed) = CLOSED.lock() {
        if !closed.iter().any(|o| o == owner) {
            closed.push(owner.to_string());
        }
    }
    end_ready_for(owner);
}

/// RC2-R3M4 (RC2-R3 review P3): end every process made ready for council
/// `owner` \u{2014} its Codex servers and its Claude members \u{2014} without closing
/// the council: it may be made ready again at once (its members changed,
/// so each brief, which names the others, is not the one they were started
/// with).
pub(crate) fn end_ready_for(owner: &str) {
    // RC2-R3: its ready Claude members end too.
    crate::claude_warm::close_warm_for(owner);
    let gone: Vec<Warm> = match WARM.lock() {
        Ok(mut pool) => {
            let (gone, keep) = pool.drain(..).partition(|w| w.key.owner == owner);
            *pool = keep;
            gone
        }
        Err(_) => return,
    };
    if !gone.is_empty() {
        std::thread::spawn(move || {
            for w in gone {
                w.server.abandon();
            }
        });
    }
}

/// RC2-R2F10: the ready server for this launch, if one is still alive.
fn take_warm(key: &WarmKey) -> Option<Warm> {
    let mut pool = WARM.lock().ok()?;
    expire_warm(&mut pool);
    let at = pool.iter().position(|w| &w.key == key)?;
    Some(pool.remove(at))
}

/// RC2-R2F10: how many servers are ready now (tests).
#[cfg(test)]
pub(crate) fn warm_count() -> usize {
    WARM.lock().map(|p| p.len()).unwrap_or(0)
}

/// RC2-R2G4: how many servers of `bin` are ready now (tests).
#[cfg(test)]
pub(crate) fn warm_count_of(bin: &str) -> usize {
    WARM.lock()
        .map(|p| p.iter().filter(|w| w.key.bin == bin).count())
        .unwrap_or(0)
}

/// Run one turn. `Ok(events)`: the turn ran here; its streamed events went
/// out on `tx` and `events` are the terminal ones (an error the process
/// left, then exactly one `Done`) for the caller to send. `Err(cancel_rx)`:
/// this Codex has no app server — nothing was sent, the child is reaped,
/// and the caller runs the turn with `codex exec --json`, with the Stop
/// receiver it gets back.
pub(crate) fn run_turn(
    turn: Turn<'_>,
    cancel_rx: Receiver<()>,
    tx: &async_channel::Sender<ProviderEvent>,
    session_slot: &crate::code_provider::SessionSlot,
    termination_observed: &Option<Arc<AtomicBool>>,
    approval_session: crate::codex_approval::Session,
) -> Result<Vec<ProviderEvent>, Receiver<()>> {
    // RC2-R2F10: a server made ready for this launch takes the question at
    // once, on the thread it already opened.
    if turn.spec.resume_id.is_none() {
        if let Some(mut warm) = take_warm(&WarmKey::of(&turn)) {
            let ask = warm.protocol.ask(turn.spec.prompt.clone());
            if !ask.is_empty() && send_lines(&mut warm.server.stdin, &ask) {
                if let (Some(id), Ok(mut slot)) = (warm.protocol.thread_id(), session_slot.lock()) {
                    *slot = Some(id.to_string());
                }
                return Ok(drive(
                    warm.server,
                    warm.protocol,
                    cancel_rx,
                    tx,
                    session_slot,
                    termination_observed,
                    approval_session,
                    turn.spec.host_approvals
                        && turn.spec.autonomy == awili_code::Autonomy::Autonomous,
                ));
            }
            warm.server.abandon();
        }
    }
    let Some(mut server) = start_server(&turn) else {
        // The exec path spawns the same binary and reports why it cannot.
        return Err(cancel_rx);
    };
    let allow_approvals =
        turn.spec.host_approvals && turn.spec.autonomy == awili_code::Autonomy::Autonomous;
    let mut protocol = AppServerTurn::new(turn.spec);

    // The handshake: `initialize` answered, or this Codex has no app server.
    // Stop is watched here too (a hung server is waited on for up to
    // HANDSHAKE_TIMEOUT): it ends the turn as stopped, with no fallback.
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut ready = send_lines(&mut server.stdin, &protocol.start());
    let mut stopped = false;
    while ready && protocol.handshake() == Handshake::Pending {
        if !matches!(cancel_rx.try_recv(), Err(TryRecvError::Empty)) {
            stopped = true;
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match server.lines.recv_timeout(left.min(HANDSHAKE_POLL)) {
            Ok(line) => {
                // Nothing is shown before the handshake ends (`initialize`
                // answers carry no events).
                let step = protocol.on_line(&line);
                ready = send_lines(&mut server.stdin, &step.send);
            }
            Err(RecvTimeoutError::Timeout) => ready = Instant::now() < deadline,
            Err(RecvTimeoutError::Disconnected) => ready = false,
        }
    }
    if stopped || !ready || protocol.handshake() != Handshake::Ready {
        server.abandon();
        if stopped {
            if let Some(observation) = termination_observed {
                observation.store(true, Ordering::Release);
            }
            return Ok(crate::code_provider::finalize(None, false, ""));
        }
        remember_fallback(turn.bin);
        return Err(cancel_rx);
    }
    Ok(drive(
        server,
        protocol,
        cancel_rx,
        tx,
        session_slot,
        termination_observed,
        approval_session,
        allow_approvals,
    ))
}

/// The turn runs on `server` (its handshake done): streamed events out on
/// `tx`, the terminal ones returned. Stop kills the group, as on the exec
/// path.
#[allow(clippy::too_many_arguments)] // Keep process, cancellation and approval authority explicit.
fn drive(
    mut server: Server,
    mut protocol: AppServerTurn,
    cancel_rx: Receiver<()>,
    tx: &async_channel::Sender<ProviderEvent>,
    session_slot: &crate::code_provider::SessionSlot,
    termination_observed: &Option<Arc<AtomicBool>>,
    approval_session: crate::codex_approval::Session,
    allow_approvals: bool,
) -> Vec<ProviderEvent> {
    let mut emitted_error = false;
    let forward = |events: Vec<CodeEvent>, emitted_error: &mut bool| -> bool {
        for ev in events {
            if let CodeEvent::SessionStarted { session_id } = &ev {
                if let Ok(mut slot) = session_slot.lock() {
                    *slot = Some(session_id.clone());
                }
                continue;
            }
            match ev.to_provider_event() {
                // The terminal Done comes from `finalize`, after the reap.
                Some(ProviderEvent::Done) | None => {}
                Some(other) => {
                    *emitted_error |= matches!(other, ProviderEvent::Error(_));
                    if tx.send_blocking(other).is_err() {
                        return false;
                    }
                }
            }
        }
        true
    };
    let pgid = server.pgid;
    let done = Arc::new(AtomicBool::new(false));
    let watcher_done = Arc::clone(&done);
    let reap_guard = Arc::new(Mutex::new(()));
    let watcher_guard = Arc::clone(&reap_guard);
    std::thread::spawn(move || {
        let _ = cancel_rx.recv();
        let _guard = watcher_guard.lock().unwrap_or_else(|e| e.into_inner());
        if !watcher_done.load(Ordering::Acquire) {
            crate::code_provider::kill_process_group(pgid);
        }
    });
    let mut deferred = std::collections::VecDeque::new();
    while !protocol.finished() {
        // A killed server (Stop) ends its stdout: the loop ends with it.
        let Some(line) = deferred.pop_front().or_else(|| server.lines.recv().ok()) else {
            break;
        };
        if allow_approvals {
            if let Some(request) = crate::codex_approval::Request::parse(
                &line,
                protocol.thread_id(),
                protocol.turn_id(),
            ) {
                let mut cancelled = false;
                let reply = crate::codex_approval::resolve(&request, &approval_session, tx, || {
                    while let Ok(line) = server.lines.try_recv() {
                        cancelled |= request.cancelled_by(&line);
                        deferred.push_back(line);
                    }
                    cancelled || server_has_exited(&mut server.child)
                });
                send_lines(&mut server.stdin, &[reply]);
                continue;
            }
        }
        let step = protocol.on_line(&line);
        send_lines(&mut server.stdin, &step.send);
        if !forward(step.events, &mut emitted_error) {
            break;
        }
    }
    // Closing stdin ends the server; one that lingers is killed.
    drop(server.stdin.take());
    let status = wait_with_grace(&mut server.child, pgid, EXIT_GRACE, &done, &reap_guard);
    if status.is_some() {
        if let Some(observation) = termination_observed {
            observation.store(true, Ordering::Release);
        }
    }
    done.store(true, Ordering::Release);
    crate::code_provider::deregister_group(pgid);
    if let Some(h) = server.stderr_handle.take() {
        crate::code_provider::join_stderr_with_deadline(h);
    }
    let stderr = server
        .stderr_buf
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    // A completed turn is a clean end however the server then left.
    let exit_code = if protocol.completed() {
        Some(0)
    } else {
        status.and_then(|s| s.code())
    };
    // RC2-CXSF (RC2-CXS review P1): only Codex's own "completed" is a
    // finished reply. A server that left cleanly before it said so (and
    // said nothing else went wrong) left the reply unfinished.
    if !protocol.completed() && exit_code == Some(0) && !emitted_error {
        return vec![
            ProviderEvent::Error(ENDED_EARLY.to_string()),
            ProviderEvent::Done,
        ];
    }
    crate::code_provider::finalize(exit_code, emitted_error, &stderr)
}

/// Spawn, retrying a few times while the binary is busy (ETXTBSY: it is
/// being replaced — a Codex update — or another thread's fork still holds
/// it open for writing). Any other error: `None` at once.
fn spawn_retrying_busy(cmd: &mut Command) -> Option<Child> {
    const ETXTBSY: i32 = 26; // the same number on macOS and Linux
    for attempt in 0..5u64 {
        match crate::proc_compat::spawn_tracked(cmd) {
            Ok(child) => return Some(child),
            Err(e) if e.raw_os_error() == Some(ETXTBSY) => {
                std::thread::sleep(Duration::from_millis(50 * (attempt + 1)));
            }
            Err(_) => return None,
        }
    }
    None
}

/// Write `lines` to the server; false once its stdin is gone.
fn send_lines(stdin: &mut Option<ChildStdin>, lines: &[String]) -> bool {
    let Some(pipe) = stdin.as_mut() else {
        return false;
    };
    for line in lines {
        if pipe.write_all(line.as_bytes()).is_err()
            || pipe.write_all(b"\n").is_err()
            || pipe.flush().is_err()
        {
            *stdin = None;
            return false;
        }
    }
    true
}

/// Kill and reap a child that will not run the turn.
fn abandon(child: &mut Child) {
    crate::code_provider::kill_process_group(child.id() as i32);
    let _ = child.wait();
}

/// Reap the server, giving it `grace` to exit on its own first.
fn wait_with_grace(
    child: &mut Child,
    pgid: i32,
    grace: Duration,
    done: &AtomicBool,
    reap_guard: &Mutex<()>,
) -> Option<std::process::ExitStatus> {
    let until = Instant::now() + grace;
    loop {
        #[cfg(unix)]
        {
            if crate::proc_compat::child_has_exited(child.id()) {
                return crate::code_provider::reap_native_child(child, done, reap_guard);
            }
            if Instant::now() >= until {
                crate::code_provider::kill_process_group(pgid);
                return crate::code_provider::reap_native_child(child, done, reap_guard);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        #[cfg(windows)]
        match child.try_wait() {
            Ok(Some(_)) => return crate::code_provider::reap_native_child(child, done, reap_guard),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                crate::code_provider::kill_process_group(pgid);
                return crate::code_provider::reap_native_child(child, done, reap_guard);
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
#[path = "codex_app_server_tests.rs"]
mod tests;
