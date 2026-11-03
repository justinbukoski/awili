//! D2-u1 offline gate (Codex's autonomous-merge list, j1786634032): the
//! REAL worker driven through the REAL `WorkerContext` channels with a
//! scripted transport. The live laguna gate is the slice's second gate.

use super::*;
use awili_work::run_events::ApprovalRisk;
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ONE process bootstrap owns the singleton gates; the worker never issues
// them (Codex P1 — authority is injected). The tests go through the SAME
// public bootstrap the app uses, so the sole-issue path is the one under test
// rather than a parallel copy of it.
fn authority() -> Arc<HostAuthority> {
    super::authority().expect("sole issue")
}

/// Serializes tests that share the process-global authority's grant state.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// -- scripted transport ------------------------------------------------------

#[derive(Clone, Default)]
struct ScriptState {
    bodies: VecDeque<String>,
    /// Every request body the worker sent, in order.
    sent: Vec<String>,
}

#[derive(Clone)]
struct Script(Arc<Mutex<ScriptState>>);

impl Script {
    fn new(bodies: Vec<String>) -> Self {
        Self(Arc::new(Mutex::new(ScriptState {
            bodies: bodies.into(),
            sent: Vec::new(),
        })))
    }
    fn sent(&self) -> Vec<String> {
        self.0.lock().unwrap().sent.clone()
    }
}

impl Transport for Script {
    fn stream(&mut self, request_body: &str) -> Result<String, SeamError> {
        let mut st = self.0.lock().unwrap();
        st.sent.push(request_body.to_string());
        st.bodies
            .pop_front()
            .ok_or_else(|| SeamError::Transport("script exhausted".into()))
    }
}

fn chunk(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

/// One assistant round emitting the given tool calls, then finish/usage.
fn sse_tool_round(calls: &[(&str, &str, &str)]) -> String {
    let mut out = String::new();
    for (i, (id, name, args)) in calls.iter().enumerate() {
        let args_esc = serde_json::to_string(args).unwrap();
        out.push_str(&chunk(&format!(
            r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":"","tool_calls":[{{"id":"{id}","index":{i},"type":"function","function":{{"name":"{name}","arguments":{args_esc}}}}}]}},"finish_reason":null}}]}}"#
        )));
    }
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"tool_calls"}]}"#,
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":10,"total_tokens":20}}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

fn sse_text_final(text: &str) -> String {
    let text_esc = serde_json::to_string(text).unwrap();
    let mut out = chunk(&format!(
        r#"{{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{{"index":0,"delta":{{"role":"assistant","content":{text_esc}}},"finish_reason":null}}]}}"#
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}]}"#,
    ));
    out.push_str(&chunk(
        r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

// -- harness -----------------------------------------------------------------

/// One fence budget for every deadline-bounded wait (design r2, Codex P1:
/// a wedged worker must FAIL the test with diagnostics at the deadline,
/// never hang it — `Rig::until` is an unbounded `recv_blocking` and
/// `Rig::finish` an unbounded `join`, so neither can fence a regression).
const FENCE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the test-only hooks (design r2) recorded: the rejected id form of
/// each dropped approval verdict. A `Condvar` pairs with the mutex so a
/// fence can block until a record lands — deadline-bounded, with no
/// polling and no sleeps.
#[derive(Default)]
struct HookJournal {
    records: Mutex<Vec<Option<String>>>,
    told: Condvar,
}

impl HookJournal {
    fn record(&self, entry: Option<String>) {
        self.records.lock().unwrap().push(entry);
        self.told.notify_all();
    }

    fn snapshot(&self) -> Vec<Option<String>> {
        self.records.lock().unwrap().clone()
    }

    /// Block until `pred` holds over the records, or `deadline` passes —
    /// then fail with the records collected so far instead of hanging.
    fn wait_until(
        &self,
        what: &str,
        deadline: Instant,
        mut pred: impl FnMut(&[Option<String>]) -> bool,
    ) {
        let mut records = self.records.lock().unwrap();
        while !pred(&records) {
            let Some(remain) = deadline.checked_duration_since(Instant::now()) else {
                let seen = records.clone();
                panic!("{what}: hook fence deadline passed; records so far: {seen:?}");
            };
            let (guard, timeout) = self.told.wait_timeout(records, remain).unwrap();
            records = guard;
            if timeout.timed_out() && !pred(&records) {
                let seen = records.clone();
                panic!("{what}: hook fence deadline passed; records so far: {seen:?}");
            }
        }
    }
}

struct Rig {
    cmd_tx: Option<std::sync::mpsc::Sender<WorkCmd>>,
    events: async_channel::Receiver<WorkEvent>,
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    script: Script,
    dir: tempfile::TempDir,
    log_path: PathBuf,
    /// What the test-only hooks (design r2) recorded; fences print it on
    /// timeout so a wedged worker fails with its hook activity visible.
    journal: Arc<HookJournal>,
}

impl Rig {
    fn start(bodies: Vec<String>) -> Self {
        Self::start_with(bodies, |_, _| HostHooks::default())
    }

    /// Test construction with instrumentation hooks (design r2): the
    /// closure receives the SAME cancel flag and hook journal the rig will
    /// run with, so a hook can trip the flag on the worker thread or
    /// record its firings for a fence to wait on. Normal `start` rigs
    /// carry no hooks at all.
    fn start_with(
        bodies: Vec<String>,
        build_hooks: impl FnOnce(&Arc<AtomicBool>, &Arc<HookJournal>) -> HostHooks,
    ) -> Self {
        Self::start_full(
            bodies,
            build_hooks,
            awili_work::SharedApprovalMode::default(),
            false,
        )
    }

    /// U4b2: a rig whose worker carries the given shared mode cell. The
    /// test holds the other clone, so a mid-test `mode.set(..)` applies at
    /// the worker's next approval gate.
    fn start_mode(bodies: Vec<String>, mode: awili_work::SharedApprovalMode) -> Self {
        Self::start_full(bodies, |_, _| HostHooks::default(), mode, false)
    }

    /// RC2-R2F7: a rig whose worker reaches any folder by a full path.
    fn start_any_folder(bodies: Vec<String>) -> Self {
        Self::start_full(
            bodies,
            |_, _| HostHooks::default(),
            awili_work::SharedApprovalMode::default(),
            true,
        )
    }

    fn start_full(
        bodies: Vec<String>,
        build_hooks: impl FnOnce(&Arc<AtomicBool>, &Arc<HookJournal>) -> HostHooks,
        approval_mode: awili_work::SharedApprovalMode,
        any_folder: bool,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = Script::new(bodies);
        let log_path = dir.path().join("b2.jsonl");
        let cfg = AwiliEngineConfig::new(
            "http://unused",
            "test-model",
            log_path.clone(),
            dir.path().join("grants.jsonl"),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let journal = Arc::new(HookJournal::default());
        let hooks = build_hooks(&cancel, &journal);
        let worker = AwiliEngineWorker::with_transport_and_hooks(
            cfg,
            authority(),
            Box::new(script.clone()),
            hooks,
        )
        .unwrap()
        .with_approval_mode(approval_mode)
        .with_any_folder(any_folder);
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (ev_tx, events) = async_channel::unbounded();
        let handle = awili_work::spawn_worker(
            Box::new(worker),
            WorkerContext {
                workdir: dir.path().display().to_string(),
                cmd_rx,
                cancel: cancel.clone(),
                events: ev_tx,
            },
        );
        Self {
            cmd_tx: Some(cmd_tx),
            events,
            cancel,
            handle: Some(handle),
            script,
            dir,
            log_path,
            journal,
        }
    }

    fn send(&self, cmd: WorkCmd) {
        self.cmd_tx
            .as_ref()
            .expect("still running")
            .send(cmd)
            .expect("worker alive");
    }

    /// Collect events until (and including) the predicate matches.
    fn until(&self, mut pred: impl FnMut(&WorkEvent) -> bool) -> Vec<WorkEvent> {
        let mut got = Vec::new();
        loop {
            match self.events.recv_blocking() {
                Ok(ev) => {
                    let stop = pred(&ev);
                    got.push(ev);
                    if stop {
                        return got;
                    }
                }
                Err(_) => panic!("worker exited early; events so far: {got:#?}"),
            }
        }
    }

    /// ONE deadline-bounded receive from the event channel, against the
    /// remaining budget. `async-channel` has no `recv_timeout`, so the
    /// design's "or equivalent" is a one-shot std-mpsc bridge: a helper
    /// performs a single `recv_blocking` and forwards into a channel that
    /// DOES have `recv_timeout`. The helper forwards exactly the event it
    /// delivers and exits, so it can never steal from a later fence; on
    /// deadline it is left blocked until the rig tears down (the fence has
    /// already failed the test by then).
    fn recv_bounded(
        &self,
        remain: Duration,
    ) -> Result<Result<WorkEvent, async_channel::RecvError>, std::sync::mpsc::RecvTimeoutError>
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let events = self.events.clone();
        std::thread::spawn(move || {
            let _ = tx.send(events.recv_blocking());
        });
        match rx.recv_timeout(remain) {
            Ok(forwarded) => Ok(forwarded),
            // The helper died without forwarding (cannot happen today);
            // treat it as the surface being gone.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Ok(Err(async_channel::RecvError))
            }
            Err(timeout @ std::sync::mpsc::RecvTimeoutError::Timeout) => Err(timeout),
        }
    }

    /// Deadline-bounded event fence (design r2, Codex P1): repeated
    /// receives against ONE absolute deadline, collecting events until
    /// `pred` matches. At the deadline it fails with the events collected
    /// so far plus the hook journal rather than hanging; if the worker
    /// exits first it returns what it had, leaving the caller's assertions
    /// to fail with diagnostics.
    fn fence_until(
        &self,
        what: &str,
        deadline: Instant,
        mut pred: impl FnMut(&WorkEvent) -> bool,
    ) -> Vec<WorkEvent> {
        let mut got = Vec::new();
        loop {
            let Some(remain) = deadline.checked_duration_since(Instant::now()) else {
                panic!(
                    "{what}: fence deadline passed; events so far: {got:#?}; \
                     hook records: {:?}",
                    self.journal.snapshot()
                );
            };
            match self.recv_bounded(remain) {
                Ok(Ok(ev)) => {
                    let stop = pred(&ev);
                    got.push(ev);
                    if stop {
                        return got;
                    }
                }
                Ok(Err(async_channel::RecvError)) => return got,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
                    "{what}: fence deadline passed; events so far: {got:#?}; \
                     hook records: {:?}",
                    self.journal.snapshot()
                ),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return got,
            }
        }
    }

    /// Deadline-bounded worker completion (design r2, Codex P1): close the
    /// command channel, drain events against the deadline until the turn
    /// boundary (`Terminal` or `TurnComplete`), and only THEN join — the
    /// worker is past its turn, so the join cannot hang. If the boundary
    /// never arrives, the fence fails at the deadline with diagnostics
    /// instead (`std::thread::join` itself has no timeout).
    fn finish_bounded(&mut self, deadline: Instant) -> Vec<WorkEvent> {
        self.cmd_tx.take();
        let mut rest = self.fence_until("worker completion", deadline, |e| {
            matches!(e, WorkEvent::Terminal { .. } | WorkEvent::TurnComplete)
        });
        if let Some(h) = self.handle.take() {
            h.join().expect("worker exits without panicking");
        }
        while let Ok(ev) = self.events.try_recv() {
            rest.push(ev);
        }
        rest
    }

    /// Deadline-bounded hook fence (design r2, Codex P1): block until `pred`
    /// holds over the hook journal, or fail at the deadline with the
    /// records collected so far. No polling, no sleeps — the hook's record
    /// notifies the journal's Condvar.
    fn fence_hooks(
        &self,
        what: &str,
        deadline: Instant,
        pred: impl FnMut(&[Option<String>]) -> bool,
    ) {
        self.journal.wait_until(what, deadline, pred);
    }

    /// Close the command channel, join the worker, and return whatever it
    /// emitted before exiting. Takes `&mut self` so the test can keep
    /// inspecting the rig's script/log/dir afterwards.
    fn finish(&mut self) -> Vec<WorkEvent> {
        self.cmd_tx.take();
        if let Some(h) = self.handle.take() {
            h.join().expect("worker exits without panicking");
        }
        let mut rest = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            rest.push(ev);
        }
        rest
    }
}

fn is_turn_complete(ev: &WorkEvent) -> bool {
    matches!(ev, WorkEvent::TurnComplete)
}

// ============================================================================
// Gate 1 — tool correlation across multiple calls, repeated names, failures
// ============================================================================

#[test]
fn multi_call_tools_correlate_by_name_and_carry_outcomes() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[
            ("c1", "fs.read", r#"{"path":"a.txt"}"#),
            ("c2", "fs.read", r#"{"path":"missing.txt"}"#),
            ("c3", "fs.glob", r#"{"pattern":"*.txt"}"#),
        ]),
        sse_text_final("done"),
    ]);
    std::fs::write(rig.dir.path().join("a.txt"), b"hello").unwrap();
    rig.send(WorkCmd::Prompt {
        text: "go".into(),
        system: None,
    });
    let evs = rig.until(is_turn_complete);

    let started: Vec<(Option<String>, Option<String>)> = evs
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ToolStarted {
                name, file_path, ..
            } => Some((name.clone(), file_path.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        started,
        vec![
            (Some("fs.read".into()), Some("a.txt".into())),
            (Some("fs.read".into()), Some("missing.txt".into())),
            // fs.glob takes `pattern`, not `path` — no file to attribute.
            (Some("fs.glob".into()), None),
        ],
        "ToolStarted names + builtin file paths"
    );

    let finished: Vec<(Option<String>, Option<bool>)> = evs
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ToolFinished { name, ok, .. } => Some((name.clone(), *ok)),
            _ => None,
        })
        .collect();
    // Every completion carries the NAME of the call it closes (Codex P0-3),
    // including the repeated fs.read, and the factual outcome.
    assert_eq!(finished.len(), 3, "{finished:?}");
    assert_eq!(finished[0], (Some("fs.read".into()), Some(true)));
    assert_eq!(
        finished[1],
        (Some("fs.read".into()), Some(false)),
        "a failed tool reports ok=false"
    );
    assert_eq!(finished[2].0, Some("fs.glob".into()));
    rig.finish();
}

// ============================================================================
// Gate 2 — cancel at the approval dialog: nothing runs, nothing is recorded
// ============================================================================

#[test]
fn cancel_at_approval_leaves_no_record_no_effect_no_next_request() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
        // A SECOND body exists on purpose: if the worker kept going after the
        // cancel it would consume it, and `sent` would show two requests.
        sse_text_final("must not be reached"),
    ]);
    let target = rig.dir.path().join("new.txt");
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    // Wait for the dialog, then answer with Cancel.
    let _ = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    rig.send(WorkCmd::Cancel);
    let rest = rig.finish();

    assert!(
        rest.iter().any(|e| matches!(
            e,
            WorkEvent::Terminal {
                status: RunStatus::Stopped,
                ..
            }
        )),
        "exactly one Terminal{{Stopped}} expected: {rest:#?}"
    );
    assert_eq!(
        rest.iter()
            .filter(|e| matches!(e, WorkEvent::Terminal { .. }))
            .count(),
        1
    );
    assert!(!target.exists(), "the tool must NOT have run");
    assert_eq!(
        rig.script.sent().len(),
        1,
        "no transport call after the cancel"
    );
    // No grant AND no denial recorded: authorization never resolved. (The B2
    // log is framed binary; scan the raw bytes.)
    let raw = std::fs::read(&rig.log_path).unwrap();
    let log = String::from_utf8_lossy(&raw);
    assert!(
        !log.contains("GrantRecorded") && !log.contains("grant_recorded"),
        "no grant may be recorded on cancel:\n{log}"
    );
    assert!(
        !log.contains("Denied") && !log.contains("denied"),
        "no denial may be recorded on cancel:\n{log}"
    );
    assert!(
        log.contains("cancelled"),
        "the turn is audited as cancelled:\n{log}"
    );
}

#[test]
fn cancel_flag_at_approval_resolves_without_a_command() {
    let _g = lock();
    let mut rig = Rig::start(vec![sse_tool_round(&[(
        "c1",
        "fs.write",
        r#"{"path":"new.txt","contents":"x"}"#,
    )])]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let _ = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    // No WorkCmd at all — just the flag (Codex P0-1: recv_timeout polls it).
    rig.cancel.store(true, Ordering::SeqCst);
    let rest = rig.finish();
    assert!(rest.iter().any(|e| matches!(
        e,
        WorkEvent::Terminal {
            status: RunStatus::Stopped,
            ..
        }
    )));
    assert!(!rig.dir.path().join("new.txt").exists());
}

// ============================================================================
// Gate 3 — cancel between rounds
// ============================================================================

#[test]
fn cancel_between_rounds_stops_before_the_next_request() {
    let _g = lock();
    // The observe-path completion hook (design r2, Codex P0) runs ON THE
    // WORKER THREAD at the canonical completed-tool event — synchronously
    // between round 1's tool finishing and run_turn's next cancellation
    // poll, which precedes every transport request. The OLD shape (test
    // thread waits for ToolFinished, then sets the flag) raced: the worker
    // could pass its between-rounds check before the store landed.
    let mut rig = Rig::start_with(
        vec![
            // Round 1 needs no approval (fs.read is read-only) so the turn
            // proceeds to a continuation — the flag trips in between.
            sse_tool_round(&[("c1", "fs.read", r#"{"path":"a.txt"}"#)]),
            sse_text_final("must not be reached"),
        ],
        |cancel, _journal| HostHooks {
            on_tool_completed: Some(Arc::new({
                let cancel = cancel.clone();
                move || cancel.store(true, Ordering::SeqCst)
            })),
            on_approval_rejected: None,
            on_log_wait: None,
            log_try_lock: None,
        },
    );
    std::fs::write(rig.dir.path().join("a.txt"), b"hi").unwrap();
    rig.send(WorkCmd::Prompt {
        text: "read it".into(),
        system: None,
    });
    // No sleep, no hope: the flag is set before ToolFinished even reaches
    // this thread, so the stop is deterministic. All waits are
    // deadline-bounded (Codex P1) — a regression fails with diagnostics
    // instead of hanging.
    let rest = rig.finish_bounded(Instant::now() + FENCE_TIMEOUT);
    assert!(
        rest.iter().any(|e| matches!(
            e,
            WorkEvent::Terminal {
                status: RunStatus::Stopped,
                ..
            }
        )),
        "expected one Terminal{{Stopped}}: {rest:#?}"
    );
    assert_eq!(
        rest.iter()
            .filter(|e| matches!(e, WorkEvent::Terminal { .. }))
            .count(),
        1,
        "exactly one Terminal: {rest:#?}"
    );
    assert_eq!(
        rig.script.sent().len(),
        1,
        "the continuation request must not go out"
    );
}

// ============================================================================
// Gate 4 — the event surface disappearing stops the work
// ============================================================================

#[test]
fn dropped_event_receiver_stops_the_worker() {
    let _g = lock();
    let rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.read", r#"{"path":"a.txt"}"#)]),
        sse_text_final("unreachable"),
    ]);
    std::fs::write(rig.dir.path().join("a.txt"), b"hi").unwrap();
    // Drop the receiver BEFORE prompting: the first observed event fails to
    // deliver and the turn must abort before any further transport call.
    drop(rig.events.clone());
    let Rig {
        cmd_tx,
        events,
        handle,
        script,
        dir,
        ..
    } = rig;
    let cmd_tx = cmd_tx.expect("running");
    drop(events);
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "go".into(),
            system: None,
        })
        .unwrap();
    handle
        .expect("running")
        .join()
        .expect("worker exits, no panic");
    assert!(
        script.sent().len() <= 1,
        "no further requests after the surface vanished: {:?}",
        script.sent().len()
    );
    drop(cmd_tx);
    drop(dir);
}

// ============================================================================
// Gate 6/7 — terminal sequences, and a second prompt after each outcome
// ============================================================================

#[test]
fn successful_turn_emits_status_then_boundary_and_survives_for_a_second_prompt() {
    let _g = lock();
    let mut rig = Rig::start(vec![sse_text_final("first"), sse_text_final("second")]);
    rig.send(WorkCmd::Prompt {
        text: "one".into(),
        system: None,
    });
    let t1 = rig.until(is_turn_complete);
    // Started once, then Token(s), then TurnCompleted{Completed}, then the
    // boundary — exactly one turn-boundary event.
    assert!(matches!(t1[0], WorkEvent::Started(_)));
    let boundary: Vec<&WorkEvent> = t1
        .iter()
        .filter(|e| matches!(e, WorkEvent::TurnComplete))
        .collect();
    assert_eq!(boundary.len(), 1);
    let completed = t1.iter().find_map(|e| match e {
        WorkEvent::TurnCompleted { status, output, .. } => Some((*status, output.clone())),
        _ => None,
    });
    assert_eq!(
        completed,
        Some((RunStatus::Completed, Some("first".to_string())))
    );

    rig.send(WorkCmd::Prompt {
        text: "two".into(),
        system: None,
    });
    let t2 = rig.until(is_turn_complete);
    assert!(
        !t2.iter().any(|e| matches!(e, WorkEvent::Started(_))),
        "session reuse: no second Started"
    );
    assert!(t2.iter().any(|e| matches!(
        e,
        WorkEvent::TurnCompleted {
            status: RunStatus::Completed,
            ..
        }
    )));
    rig.finish();
}

#[test]
fn recoverable_failure_is_a_failed_turn_not_a_dead_worker() {
    let _g = lock();
    // First turn's transport is exhausted → SeamError::Transport (recoverable).
    let mut rig = Rig::start(vec![]);
    rig.send(WorkCmd::Prompt {
        text: "one".into(),
        system: None,
    });
    let t1 = rig.until(is_turn_complete);
    let status = t1.iter().find_map(|e| match e {
        WorkEvent::TurnCompleted { status, .. } => Some(*status),
        _ => None,
    });
    assert_eq!(status, Some(RunStatus::Failed), "{t1:#?}");
    assert!(
        !t1.iter().any(|e| matches!(e, WorkEvent::Failed(_))),
        "WorkEvent::Failed is worker-FATAL in the app; a failed turn must not use it"
    );
    // The worker is alive: it answers a second prompt (which also fails,
    // the script being empty — the point is that it was accepted at all).
    rig.send(WorkCmd::Prompt {
        text: "two".into(),
        system: None,
    });
    let t2 = rig.until(is_turn_complete);
    assert!(t2.iter().any(|e| matches!(
        e,
        WorkEvent::TurnCompleted {
            status: RunStatus::Failed,
            ..
        }
    )));
    rig.finish();
}

// ============================================================================
// Gate 8 — compaction is announced before the summarizer request
// ============================================================================

#[test]
fn compaction_announces_itself_before_the_summarizer_call() {
    let _g = lock();
    let long = "the first answer was very long ".repeat(300);
    let dir = tempfile::tempdir().unwrap();
    let script = Script::new(vec![
        sse_text_final(&long),
        sse_text_final("SUMMARY"),
        sse_text_final("second answer"),
    ]);
    // A window that fits turn 1 but forces compaction for turn 2.
    let mut cfg = AwiliEngineConfig::new(
        "http://unused",
        "test-model",
        dir.path().join("b2.jsonl"),
        dir.path().join("grants.jsonl"),
    );
    cfg.window_tokens = 6000;
    let worker =
        AwiliEngineWorker::with_transport(cfg, authority(), Box::new(script.clone())).unwrap();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    let until = |pred: fn(&WorkEvent) -> bool| {
        let mut got = Vec::new();
        loop {
            let ev = events.recv_blocking().expect("worker alive");
            let stop = pred(&ev);
            got.push(ev);
            if stop {
                return got;
            }
        }
    };
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "one".into(),
            system: None,
        })
        .unwrap();
    let _ = until(is_turn_complete);
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "two".into(),
            system: None,
        })
        .unwrap();
    let t2 = until(is_turn_complete);
    drop(cmd_tx);
    handle.join().unwrap();

    let compacting = t2.iter().position(|e| {
        matches!(e, WorkEvent::StatusUpdate { kind, .. } if kind.as_deref() == Some("compacting"))
    });
    assert!(compacting.is_some(), "compaction was silent: {t2:#?}");
    // The summarizer request went out AFTER the announcement: three requests
    // total (turn 1, summarizer, turn 2) and the summary is on the wire.
    let sent = script.sent();
    assert_eq!(sent.len(), 3, "turn1 + summarizer + turn2");
    assert!(sent[1].contains("Summarize the following working context"));
    assert!(sent[2].contains("[context summary] SUMMARY"));
}

// ============================================================================
// Config
// ============================================================================

#[test]
fn non_chat_completions_dialect_is_refused_typed() {
    let _g = lock();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = AwiliEngineConfig::new(
        "http://unused",
        "m",
        dir.path().join("b2.jsonl"),
        dir.path().join("g.jsonl"),
    );
    cfg.dialect = "openai-responses".into();
    match AwiliEngineWorker::with_transport(cfg, authority(), Box::new(Script::new(vec![]))) {
        Err(e) => assert_eq!(
            e,
            ConfigError::UnsupportedDialect("openai-responses".into())
        ),
        Ok(_) => panic!("a non-chat-completions dialect must be refused"),
    }
}

// ============================================================================
// LIVE gate (Codex gate 9). Requires laguna reachable at the tunnel:
//   cargo test -p awili-engine-worker -- --ignored --nocapture live_
// ============================================================================

#[test]
#[ignore = "live gate: requires an OpenAI-compatible endpoint at 127.0.0.1:18000"]
fn live_two_turn_read_edit_approve_through_the_channels() {
    use awili_engine_log::replay::{classify, IntentStatus};
    use awili_engine_log::schema::LogRecordV1;

    let _g = lock();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), b"hello awili\n").unwrap();
    let log_path = dir.path().join("live.b2.jsonl");
    let cfg = AwiliEngineConfig::new(
        "http://127.0.0.1:18000",
        "laguna-s-2.1-nvfp4",
        log_path.clone(),
        dir.path().join("live-grants.jsonl"),
    );
    let worker = AwiliEngineWorker::new(cfg, authority()).unwrap();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );

    let deadline = std::time::Duration::from_secs(240);
    let mut transcript: Vec<String> = Vec::new();
    // Drive one turn: answer any approval over the CHANNEL (the whole point
    // — the app's existing card flow, no engine-specific plumbing), and stop
    // at the turn boundary.
    let run_turn_live = |transcript: &mut Vec<String>, prompt: &str, turn: usize| {
        cmd_tx
            .send(WorkCmd::Prompt {
                text: prompt.into(),
                system: None,
            })
            .unwrap();
        let start = std::time::Instant::now();
        let mut tokens = 0usize;
        let mut approvals = 0usize;
        loop {
            assert!(
                start.elapsed() < deadline,
                "turn {turn} timed out; transcript: {transcript:#?}"
            );
            match events.try_recv() {
                Ok(ev) => {
                    let line = format!("turn{turn}: {ev:?}");
                    eprintln!("{line}");
                    transcript.push(line);
                    match &ev {
                        WorkEvent::Token(_) => tokens += 1,
                        WorkEvent::ApprovalRequested { approval_id, .. } => {
                            approvals += 1;
                            // Echo the gate's own id — the engine drops any
                            // verdict that does not correlate (D2-u2a), so a
                            // live turn only completes if the surface plumbs
                            // it through.
                            cmd_tx
                                .send(WorkCmd::Approval {
                                    id: approval_id.clone(),
                                    choice: "once".into(),
                                })
                                .unwrap();
                        }
                        WorkEvent::Failed(msg) => {
                            panic!("turn {turn} worker-fatal: {msg}; {transcript:#?}")
                        }
                        WorkEvent::TurnComplete => return (tokens, approvals),
                        _ => {}
                    }
                }
                Err(async_channel::TryRecvError::Empty) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(async_channel::TryRecvError::Closed) => {
                    panic!("worker exited mid-turn {turn}; {transcript:#?}")
                }
            }
        }
    };

    let (t1_tokens, t1_approvals) = run_turn_live(
        &mut transcript,
        "Read notes.txt, then use fs.edit to replace the word awili with world, \
         then read it again to confirm.",
        1,
    );
    assert!(t1_tokens > 0, "turn 1 streamed nothing: {transcript:#?}");
    assert!(
        t1_approvals > 0,
        "the mutating edit must have gone through an approval card: {transcript:#?}"
    );
    // The turn actually completed (status carried on the boundary).
    assert!(
        transcript
            .iter()
            .any(|l| l.contains("TurnCompleted") && l.contains("Completed")),
        "turn 1 did not report Completed: {transcript:#?}"
    );
    // On-disk verification by THIS process, inside the scoped root.
    let contents = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
    assert!(
        contents.contains("hello world"),
        "the file was not really edited: {contents:?}"
    );

    // Turn 2 on the SAME worker: session reuse (exactly one Started).
    let (t2_tokens, _) = run_turn_live(
        &mut transcript,
        "What word did you replace in notes.txt? Answer from context.",
        2,
    );
    assert!(t2_tokens > 0, "turn 2 streamed nothing: {transcript:#?}");
    assert_eq!(
        transcript.iter().filter(|l| l.contains("Started(")).count(),
        1,
        "session was recreated between turns: {transcript:#?}"
    );

    cmd_tx.send(WorkCmd::Shutdown).unwrap();
    handle.join().unwrap();

    // B2 replay: every intent Clean, at least one mutating tool ok.
    let raw = std::fs::read(&log_path).unwrap();
    let frames = match awili_engine_log::frame::decode_all(&raw).expect("the B2 log frames decode")
    {
        awili_engine_log::frame::DecodeResult::Clean(frames) => frames,
        other => panic!("the B2 log must decode Clean after a clean worker run, got {other:?}"),
    };
    let records: Vec<LogRecordV1> = frames
        .iter()
        .map(|f| serde_json::from_slice(f).expect("record decodes"))
        .collect();
    let turns = classify(&records);
    let mut intents = 0usize;
    let mut mutating_ok = false;
    for t in &turns {
        for i in &t.intents {
            intents += 1;
            match &i.status {
                IntentStatus::Clean { ok } => {
                    if *ok && (i.tool == "fs.edit" || i.tool == "fs.write") {
                        mutating_ok = true;
                    }
                }
                other => panic!("non-Clean intent {} {}: {other:?}", i.intent_id, i.tool),
            }
        }
    }
    assert!(intents > 0, "no intents recorded");
    assert!(
        mutating_ok,
        "no Clean mutating intent — the edit is the point"
    );
    eprintln!("LIVE GATE PASS: {intents} intents, all Clean, mutating edit landed");
}

// ============================================================================
// D2-u2a — approval correlation
// ============================================================================

/// The gate's id is echoed back and the verdict applies: the ordinary path,
/// pinned so the correlation check cannot regress into rejecting everything.
#[test]
fn approval_carrying_the_matching_id_resolves_the_gate() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => approval_id.clone(),
            _ => None,
        })
        .expect("the engine must raise a correlated gate");
    rig.send(WorkCmd::Approval {
        id: Some(id),
        choice: "once".into(),
    });
    let rest = rig.finish();
    assert!(
        rig.dir.path().join("new.txt").exists(),
        "a correlated approval must let the tool run: {rest:#?}"
    );
}

/// A verdict for some OTHER gate must not resolve the gate that is actually
/// pending. This is the security property of the slice: before correlation, a
/// stale "approve" left over from a dialog the user already answered would
/// silently authorize whatever call happened to be blocked next.
///
/// The stale verdict is DROPPED, not treated as a denial — so the gate is still
/// open afterwards and the real answer still lands.
#[test]
fn approval_for_a_different_gate_is_dropped_and_the_real_one_still_lands() {
    let _g = lock();
    // The approve-path rejection hook (design r2, Codex P1) records each
    // dropped verdict's id form after it is classified and before the wait
    // loop resumes — a fence over the records replaces the old blind
    // 300 ms sleep. No product WorkEvent carries this (Codex P1: the
    // worker already drops and logs the verdict; an ignored-approval
    // notice would be a separate product change).
    let mut rig = Rig::start_with(
        vec![
            sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
            sse_text_final("done"),
        ],
        |_cancel, journal| HostHooks {
            on_tool_completed: None,
            on_approval_rejected: Some(Arc::new({
                let journal = journal.clone();
                move |id| journal.record(id.map(str::to_string))
            })),
            on_log_wait: None,
            log_try_lock: None,
        },
    );
    let target = rig.dir.path().join("new.txt");
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let seen = rig.fence_until("approval dialog", Instant::now() + FENCE_TIMEOUT, |e| {
        matches!(e, WorkEvent::ApprovalRequested { .. })
    });
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => approval_id.clone(),
            _ => None,
        })
        .expect("the engine must raise a correlated gate");

    // A verdict from an earlier, unrelated dialog.
    let stale = format!("{id}-stale");
    rig.send(WorkCmd::Approval {
        id: Some(stale.clone()),
        choice: "once".into(),
    });
    // And an uncorrelated one from a surface that never plumbed the id.
    rig.send(WorkCmd::Approval {
        id: None,
        choice: "once".into(),
    });

    // Fence on BOTH rejections before asserting anything negative (Codex
    // P2): channel FIFO plus these two acknowledged drops make the
    // assertions below deterministic — and the two rejected forms are
    // asserted IN ORDER, not merely counted.
    let expected = vec![Some(stale.clone()), None];
    rig.fence_hooks(
        "both stale verdicts rejected",
        Instant::now() + FENCE_TIMEOUT,
        |records| records.starts_with(&expected),
    );
    assert_eq!(
        rig.journal.snapshot(),
        expected,
        "the rejections arrive in order: Some(wrong_id), then None"
    );
    assert!(
        !target.exists(),
        "an uncorrelated approval must never authorize the pending call"
    );
    assert_eq!(
        rig.script.sent().len(),
        1,
        "the turn must still be blocked at the gate, not moved on"
    );

    // The gate is still open, so the genuine answer resolves it.
    rig.send(WorkCmd::Approval {
        id: Some(id),
        choice: "once".into(),
    });
    let rest = rig.finish_bounded(Instant::now() + FENCE_TIMEOUT);
    assert!(
        target.exists(),
        "dropping a stale verdict must leave the gate answerable: {rest:#?}"
    );
    // The hook must NOT have fired for the correlated verdict (Codex P2):
    // the journal still holds exactly the two stale forms.
    assert_eq!(
        rig.journal.snapshot(),
        expected,
        "the rejection hook fired for the correlated verdict"
    );
}

// ============================================================================
// P3a — scoped approval: exact scopes, typed denial facts, grant-store lanes
// ============================================================================

/// P3a: `session` is a real scope the hook passes through verbatim — the gate
/// resolves, the tool runs, and the grant stays in the SESSION lane (B2 log)
/// without reaching the per-user grant store: a session grant stops at
/// session end. The gate itself carries the risk class the surface offers
/// scopes from.
#[test]
fn session_scope_grant_stays_in_the_session_not_the_store() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let (risk, id) = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested {
                approval_id, risk, ..
            } => Some((*risk, approval_id.clone())),
            _ => None,
        })
        .expect("the engine must raise a correlated gate");
    assert_eq!(
        risk,
        ApprovalRisk::Mutating,
        "the gate carries the risk class for scope offering"
    );
    rig.send(WorkCmd::Approval {
        id,
        choice: "session".into(),
    });
    let rest = rig.finish();
    assert!(
        rig.dir.path().join("new.txt").exists(),
        "a session-scope approval must let the tool run: {rest:#?}"
    );
    assert!(
        rest.iter()
            .all(|e| !matches!(e, WorkEvent::HookDenial { .. })),
        "an accepted scope must not raise a denial fact: {rest:#?}"
    );
    // Session lane: the grant is recorded in the B2 log…
    let log_bytes = std::fs::read(&rig.log_path).unwrap();
    let log = String::from_utf8_lossy(&log_bytes);
    assert!(
        log.contains("\"scope\":\"session\""),
        "the session grant is recorded in the session lane:\n{log}"
    );
    // …and NOT in the per-user grant store — it stops at session end.
    let store = std::fs::read(rig.dir.path().join("grants.jsonl")).unwrap_or_default();
    let store = String::from_utf8_lossy(&store);
    assert!(
        !store.contains("\"scope\":\"session\""),
        "a session grant must not reach the grant store:\n{store}"
    );
}

/// P3a: `folder` and `always` grants persist to the per-user grant store (the
/// folder grant is keyed to the bound folder's canonical root by the store's
/// own matching; `always` survives a restart by the store's construction).
/// Neither may surface a denial fact.
#[test]
fn folder_and_always_scopes_persist_to_the_grant_store() {
    let _g = lock();
    for (scope, file) in [("folder", "a.txt"), ("always", "b.txt")] {
        let args = format!("{{\"path\":\"{file}\",\"contents\":\"x\"}}");
        let mut rig = Rig::start(vec![
            sse_tool_round(&[("c1", "fs.write", args.as_str())]),
            sse_text_final("done"),
        ]);
        rig.send(WorkCmd::Prompt {
            text: "write it".into(),
            system: None,
        });
        let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
        let id = seen
            .iter()
            .find_map(|e| match e {
                WorkEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
                _ => None,
            })
            .expect("the engine must raise a correlated gate");
        rig.send(WorkCmd::Approval {
            id,
            choice: scope.into(),
        });
        let rest = rig.finish();
        assert!(
            rig.dir.path().join(file).exists(),
            "a {scope}-scope approval must let the tool run: {rest:#?}"
        );
        assert!(
            rest.iter()
                .all(|e| !matches!(e, WorkEvent::HookDenial { .. })),
            "an accepted scope must not raise a denial fact: {rest:#?}"
        );
        let store = std::fs::read(rig.dir.path().join("grants.jsonl")).unwrap_or_default();
        let store = String::from_utf8_lossy(&store);
        assert!(
            store.contains(&format!("\"scope\":\"{scope}\"")),
            "the {scope} grant must persist to the grant store:\n{store}"
        );
    }
}

/// P3a (contract §8 floor, authz 315-325): `always` is refused for a
/// PRIVILEGED risk. A strayed `always` verdict on a secret-touching call is a
/// TYPED denial fact (`source`/`reason`/`recovery`), the tool never runs, and
/// no grant of any scope is minted.
#[test]
fn always_scope_is_refused_for_privileged_risk_with_typed_denial() {
    let _g = lock();
    // A write whose path touches the secrets deny-list escalates to privileged.
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"token.txt","contents":"x"}"#)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let (risk, id) = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested {
                approval_id, risk, ..
            } => Some((*risk, approval_id.clone())),
            _ => None,
        })
        .expect("the engine must raise the correlated gate");
    assert_eq!(risk, ApprovalRisk::Privileged);
    rig.send(WorkCmd::Approval {
        id,
        choice: "always".into(),
    });
    let rest = rig.finish();
    let (source, reason, recovery) = rest
        .iter()
        .find_map(|e| match e {
            WorkEvent::HookDenial {
                source,
                reason,
                recovery,
            } => Some((source.clone(), reason.clone(), recovery.clone())),
            _ => None,
        })
        .expect("a strayed `always` must yield the typed denial fact: {rest:#?}");
    assert_eq!(source, "authorization");
    assert!(
        reason.contains("cannot be granted 'always'"),
        "the denier's own reason, verbatim: {reason}"
    );
    assert!(
        recovery.is_some(),
        "the denier supplies the permitted recovery"
    );
    assert!(
        !rig.dir.path().join("token.txt").exists(),
        "the privileged tool must NOT have run"
    );
    let store = std::fs::read(rig.dir.path().join("grants.jsonl")).unwrap_or_default();
    let store = String::from_utf8_lossy(&store);
    assert!(
        !store.contains("\"scope\":\"always\""),
        "no `always` grant may be minted for privileged risk:\n{store}"
    );
}

/// P3a (contract §8 floor): the refusal holds for an IRREVERSIBLE risk too —
/// a force-flagged fs.edit escalates to irreversible, and `always` there is a
/// typed denial.
#[test]
fn always_scope_is_refused_for_irreversible_risk_with_typed_denial() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[(
            "c1",
            "fs.edit",
            r#"{"path":"a.txt","find":"x","replace":"y","force":true}"#,
        )]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "edit it".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let (risk, id) = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested {
                approval_id, risk, ..
            } => Some((*risk, approval_id.clone())),
            _ => None,
        })
        .expect("the engine must raise the correlated gate");
    assert_eq!(risk, ApprovalRisk::Irreversible);
    rig.send(WorkCmd::Approval {
        id,
        choice: "always".into(),
    });
    let rest = rig.finish();
    let (source, reason, _) = rest
        .iter()
        .find_map(|e| match e {
            WorkEvent::HookDenial {
                source,
                reason,
                recovery,
            } => Some((source.clone(), reason.clone(), recovery.clone())),
            _ => None,
        })
        .expect("a strayed `always` must yield the typed denial fact: {rest:#?}");
    assert_eq!(source, "authorization");
    assert!(
        reason.contains("cannot be granted 'always'"),
        "the denier's own reason, verbatim: {reason}"
    );
}

/// S2(a) correction (RVS2A-FIX1): the cross-crate string alignment — for every
/// engine `RiskClass` variant, the mapped `ApprovalRisk` serializes to the
/// SAME frozen wire string the engine uses (`RiskClass::as_str`).
#[test]
fn approval_risk_of_matches_engine_wire_strings() {
    for r in [
        RiskClass::ReadOnly,
        RiskClass::Mutating,
        RiskClass::Irreversible,
        RiskClass::Privileged,
    ] {
        assert_eq!(
            approval_risk_of(r).to_wire(),
            r.as_str(),
            "wire alignment for {r:?}"
        );
    }
}

/// P3a: a user `deny` emits the TYPED denial fact — `source:
/// "authorization"`, the denier's reason verbatim, no invented recovery —
/// and the tool never runs.
#[test]
fn deny_choice_emits_the_typed_denial_fact() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .expect("the engine must raise a correlated gate");
    rig.send(WorkCmd::Approval {
        id,
        choice: "deny".into(),
    });
    let rest = rig.finish();
    let (source, reason, recovery) = rest
        .iter()
        .find_map(|e| match e {
            WorkEvent::HookDenial {
                source,
                reason,
                recovery,
            } => Some((source.clone(), reason.clone(), recovery.clone())),
            _ => None,
        })
        .expect("a deny verdict must yield the typed denial fact: {rest:#?}");
    assert_eq!(source, "authorization");
    assert_eq!(reason, "denied by the user");
    assert!(
        recovery.is_none(),
        "the surface must not invent a recovery for a plain deny"
    );
    assert!(
        !rig.dir.path().join("new.txt").exists(),
        "the denied tool must NOT have run"
    );
}

/// P3a: a scope the surface never offers is refused as a TYPED denial (fail
/// closed, never silently accepted), naming the scopes that are offered.
#[test]
fn unsupported_scope_is_refused_with_typed_denial() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .expect("the engine must raise a correlated gate");
    rig.send(WorkCmd::Approval {
        id,
        choice: "acknowledge".into(),
    });
    let rest = rig.finish();
    let (source, reason, recovery) = rest
        .iter()
        .find_map(|e| match e {
            WorkEvent::HookDenial {
                source,
                reason,
                recovery,
            } => Some((source.clone(), reason.clone(), recovery.clone())),
            _ => None,
        })
        .expect("an unsupported scope must yield the typed denial fact: {rest:#?}");
    assert_eq!(source, "authorization");
    assert!(reason.contains("unsupported approval scope"), "{reason}");
    assert!(
        recovery
            .as_ref()
            // RC1-N: `deny-stop` is offered too.
            .is_some_and(|r| r.contains("once, session, folder, always, deny, and deny-stop")),
        "the recovery names the offered scopes: {recovery:?}"
    );
    assert!(
        !rig.dir.path().join("new.txt").exists(),
        "the tool must NOT have run"
    );
}

/// The respawn property. The Work surface tears down and rebuilds its worker on
/// every folder change, and the underlying gates are sole-issue-per-process —
/// so if issuance sat on the spawn path, the SECOND folder bind would fail.
/// Repeated calls must hand back the same shared authority, not try to reissue.
///
/// This is the failure the offline suite would otherwise miss entirely: every
/// other test here spawns exactly one worker.
#[test]
fn authority_is_issued_once_and_shared_across_respawns() {
    let first = super::authority().expect("first bootstrap issues the gates");
    for _ in 0..4 {
        let again = super::authority().expect("a respawn must not reissue");
        assert!(
            Arc::ptr_eq(&first, &again),
            "every session must share one authority"
        );
    }
}

// ============================================================================
// Ship-fix P0-3 — the pinned system preamble (directives/recall) on the wire
// ============================================================================

/// The prompt's `system` context lands as the FIRST request message with
/// role "system", and the NEXT turn's preamble REPLACES it — exactly one
/// pinned preamble, always current, never accumulating.
#[test]
fn prompt_system_context_is_pinned_first_and_replaced_per_turn() {
    let _g = lock();
    let rig = Rig::start(vec![
        sse_text_final("one"),
        sse_text_final("two"),
        sse_text_final("three"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "first".into(),
        system: Some("RULES v1".into()),
    });
    let _ = rig.until(is_turn_complete);
    rig.send(WorkCmd::Prompt {
        text: "second".into(),
        system: Some("RULES v2".into()),
    });
    let _ = rig.until(is_turn_complete);
    // A turn with NO preamble clears it.
    rig.send(WorkCmd::Prompt {
        text: "third".into(),
        system: None,
    });
    let _ = rig.until(is_turn_complete);

    let sent = rig.script.sent();
    assert_eq!(sent.len(), 3);
    let msgs = |body: &str| -> Vec<(String, String)> {
        serde_json::from_str::<serde_json::Value>(body).unwrap()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["role"].as_str().unwrap_or_default().to_string(),
                    m["content"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    };
    let t1 = msgs(&sent[0]);
    assert_eq!(t1[0], ("system".into(), "RULES v1".into()), "{t1:?}");
    let t2 = msgs(&sent[1]);
    assert_eq!(
        t2[0],
        ("system".into(), "RULES v2".into()),
        "the preamble is REPLACED, not stacked: {t2:?}"
    );
    assert_eq!(
        t2.iter().filter(|(r, _)| r == "system").count(),
        1,
        "exactly one system preamble: {t2:?}"
    );
    let t3 = msgs(&sent[2]);
    assert!(
        !t3.iter().any(|(r, _)| r == "system"),
        "a turn without a preamble clears it: {t3:?}"
    );
}

/// Ship-fix P0-2: a worker built with restored turns presents them to the
/// MODEL — the first request contains the prior user/assistant turns, in
/// order, before the new user message.
#[test]
fn restored_turns_hydrate_the_model_context() {
    let _g = lock();
    let dir = tempfile::tempdir().unwrap();
    let script = Script::new(vec![sse_text_final("hello again")]);
    let mut cfg = AwiliEngineConfig::new(
        "http://unused",
        "test-model",
        dir.path().join("b2.jsonl"),
        dir.path().join("grants.jsonl"),
    );
    cfg.restored = vec![
        crate::RestoredTurn {
            user: "what is the plan?".into(),
            assistant: "we ship unit 5".into(),
        },
        crate::RestoredTurn {
            user: "and after that?".into(),
            assistant: "the ship-readiness queue".into(),
        },
    ];
    let worker =
        AwiliEngineWorker::with_transport(cfg, authority(), Box::new(script.clone())).unwrap();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let _handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "resume".into(),
            system: None,
        })
        .unwrap();
    loop {
        match events.recv_blocking().expect("worker alive") {
            WorkEvent::TurnComplete => break,
            WorkEvent::Failed(m) => panic!("worker failed: {m}"),
            _ => {}
        }
    }
    drop(cmd_tx);
    let sent = script.sent();
    assert_eq!(sent.len(), 1);
    let msgs: Vec<(String, String)> = serde_json::from_str::<serde_json::Value>(&sent[0]).unwrap()
        ["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["role"].as_str().unwrap_or_default().to_string(),
                m["content"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let texts: Vec<&str> = msgs.iter().map(|(_, c)| c.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "what is the plan?",
            "we ship unit 5",
            "and after that?",
            "the ship-readiness queue",
            "resume",
        ],
        "restored turns precede the new user message: {msgs:?}"
    );
    assert_eq!(msgs[0].0, "user");
    assert_eq!(msgs[1].0, "assistant");
    assert_eq!(msgs[4].0, "user");
}

/// Ship-fix P0-1: a crashed prior session's log surfaces recovery at the
/// NEXT worker's startup — the uncertain dispatch is named, never
/// silently continued past — and turn numbering continues past every id
/// the log already holds (no aliasing after respawn).
#[test]
fn crashed_log_surfaces_recovery_and_turn_ids_never_alias() {
    let _g = lock();
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("b2.jsonl");
    // Simulate the crash: turn-awili-0 with a dispatch that STARTED and
    // never finalized (the mid-effect power cut).
    {
        let store = awili_engine_log::storage::FileLog::open(&log_path).unwrap();
        let mut log = ExecutionLog::open(store, "sess-crashed").unwrap();
        log.turn_opened("turn-awili-0", "test", "test-model")
            .unwrap();
        let call = awili_engine::events::ToolCall {
            awili_call_id: AwiliCallId("c1".into()),
            provider_call_id: Some("p1".into()),
            name: "fs.read".into(),
            arguments: r#"{"path":"a.txt"}"#.into(),
        };
        let gate_evidence = {
            // Evidence minted through the PROCESS authority (the same
            // bundle the worker under test shares): a read_only authorize
            // yields policy evidence — the uncertainty classification
            // keys on start-without-finalize, not on the risk class.
            let host = authority();
            let resolved = host.registry.mint(awili_engine::ToolIdentity::new(
                awili_engine::ToolOrigin::new("builtin:fs"),
                "fs.write",
                awili_engine::DescriptorDigest([0u8; 32]),
            ));
            let empty = host.authorizer.load_grants(&[], &[]);
            match host.authorizer.authorize(
                &awili_engine::events::ToolCall {
                    awili_call_id: AwiliCallId("c1".into()),
                    provider_call_id: Some("p1".into()),
                    name: "fs.read".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
                awili_engine_authz::AuthzContext {
                    turn_id: "turn-awili-0",
                    session_id: "sess-crashed",
                    folder_root: None,
                    grants: &empty,
                    registry_floor: None,
                    resolved_tool: &resolved,
                    install_witness: None,
                },
            ) {
                awili_engine_authz::Decision::Allow { evidence } => evidence,
                other => panic!(
                    "read_only policy allow expected, got {}",
                    match other {
                        awili_engine_authz::Decision::Ask(_) => "Ask",
                        awili_engine_authz::Decision::Deny(_) => "Deny",
                        _ => "?",
                    }
                ),
            }
        };
        let outcome = log.intent("turn-awili-0", &call, gate_evidence).unwrap();
        let awili_engine_log::IntentOutcome::Dispatchable(ticket) = outcome else {
            panic!("dispatchable");
        };
        log.dispatch_started(&ticket).unwrap();
        // CRASH: no finalize, no turn_closed.
    }

    // The next worker over the SAME log: recovery surfaces before turns.
    let script = Script::new(vec![sse_text_final("ok")]);
    let cfg = AwiliEngineConfig::new(
        "http://unused",
        "test-model",
        log_path.clone(),
        dir.path().join("grants.jsonl"),
    );
    let worker =
        AwiliEngineWorker::with_transport(cfg, authority(), Box::new(script.clone())).unwrap();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let _handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "continue".into(),
            system: None,
        })
        .unwrap();
    // The GATE (r2 bar): the prompt is HELD behind an acknowledgement
    // card — nothing runs until the user answers.
    let mut recovery_lines = Vec::new();
    let gate_prompt = loop {
        match events.recv_blocking().expect("worker alive") {
            WorkEvent::StatusUpdate {
                kind: Some(k),
                message: Some(m),
            } if k == "recovery" => recovery_lines.push(m),
            WorkEvent::ApprovalRequested {
                approval_id: Some(id),
                description,
                ..
            } if id == "recovery-gate" => {
                break description;
            }
            WorkEvent::TurnStarted { .. } | WorkEvent::TurnComplete => {
                panic!("nothing may run before the gate is answered")
            }
            WorkEvent::Failed(m) => panic!("worker failed: {m}"),
            _ => {}
        }
    };
    let gate_text = gate_prompt.expect("the gate names the effects");
    assert!(gate_text.contains("fs.read"), "{gate_text}");
    // DENY: the prompt is dropped as a failed turn; the gate stands.
    cmd_tx
        .send(WorkCmd::Approval {
            id: Some("recovery-gate".into()),
            choice: "deny".into(),
        })
        .unwrap();
    loop {
        match events.recv_blocking().expect("worker alive") {
            WorkEvent::TurnCompleted { status, output, .. } => {
                assert_eq!(status, awili_work::run_events::RunStatus::Failed);
                assert!(output.unwrap_or_default().contains("recovery gate"));
            }
            WorkEvent::TurnComplete => break,
            _ => {}
        }
    }
    // RC1-N (pre-review P1/P2): the Work surface's Deny (`deny-stop`) and
    // an unknown choice hold the gate too — neither acknowledges.
    for held in ["deny-stop", "bogus"] {
        cmd_tx
            .send(WorkCmd::Prompt {
                text: "continue".into(),
                system: None,
            })
            .unwrap();
        loop {
            match events.recv_blocking().expect("worker alive") {
                WorkEvent::ApprovalRequested {
                    approval_id: Some(id),
                    ..
                } if id == "recovery-gate" => {
                    cmd_tx
                        .send(WorkCmd::Approval {
                            id: Some("recovery-gate".into()),
                            choice: held.into(),
                        })
                        .unwrap();
                }
                WorkEvent::TurnStarted { .. } => panic!("{held} must hold the gate"),
                WorkEvent::TurnCompleted { status, output, .. } => {
                    assert_eq!(status, awili_work::run_events::RunStatus::Failed);
                    assert!(output.unwrap_or_default().contains("recovery gate"));
                }
                WorkEvent::TurnComplete => break,
                _ => {}
            }
        }
    }
    // Retry + APPROVE: the gate clears and the turn actually runs.
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "continue".into(),
            system: None,
        })
        .unwrap();
    loop {
        match events.recv_blocking().expect("worker alive") {
            WorkEvent::ApprovalRequested {
                approval_id: Some(id),
                ..
            } if id == "recovery-gate" => {
                cmd_tx
                    .send(WorkCmd::Approval {
                        id: Some("recovery-gate".into()),
                        choice: "acknowledge".into(),
                    })
                    .unwrap();
            }
            WorkEvent::TurnComplete => break,
            WorkEvent::Failed(m) => panic!("worker failed: {m}"),
            _ => {}
        }
    }
    drop(cmd_tx);
    assert!(
        recovery_lines
            .iter()
            .any(|l| l.contains("turn-awili-0") && l.contains("fs.read")),
        "the uncertain dispatch is surfaced by name: {recovery_lines:?}"
    );
    // And the new turn did NOT alias the crashed one.
    let store = awili_engine_log::storage::FileLog::open(&log_path).unwrap();
    let (recs, _) = ExecutionLog::open(store, "sess-reader")
        .unwrap()
        .read_records()
        .unwrap();
    let turn_ids: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            awili_engine_log::schema::LogRecordV1::TurnOpened { turn_id, .. } => {
                Some(turn_id.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        turn_ids.contains(&"turn-awili-1".to_string()),
        "numbering continues past the crashed turn: {turn_ids:?}"
    );
    assert_eq!(
        turn_ids.iter().filter(|t| *t == "turn-awili-0").count(),
        1,
        "the crashed id is never reused: {turn_ids:?}"
    );
}

// ============================================================================
// U4b2 — the in-app engine worker honours the shared mode
// ============================================================================

/// U4b2: in Ask (the default), a write stops at the approval card; the
/// file is absent while the dialog is open and exists once the verdict is
/// "once".
#[test]
fn u4b2_ask_mode_write_waits_for_the_card_then_runs_on_allow_once() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let target = rig.dir.path().join("new.txt");
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    assert!(
        !target.exists(),
        "the write must NOT have run while the card is open: {seen:#?}"
    );
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .expect("the engine must raise a correlated gate");
    rig.send(WorkCmd::Approval {
        id,
        choice: "once".into(),
    });
    let rest = rig.finish();
    assert!(
        target.exists(),
        "the allow-once verdict must let the tool run: {rest:#?}"
    );
}

/// U4b2: in Ask, a "deny" verdict leaves the file untouched and the turn
/// ends with the typed denial fact.
#[test]
fn u4b2_ask_mode_deny_leaves_the_file_untouched() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .expect("the engine must raise a correlated gate");
    rig.send(WorkCmd::Approval {
        id,
        choice: "deny".into(),
    });
    let rest = rig.finish();
    assert!(
        !rig.dir.path().join("new.txt").exists(),
        "the denied tool must NOT have run"
    );
    assert!(
        rest.iter()
            .any(|e| matches!(e, WorkEvent::HookDenial { .. })),
        "a deny must surface the typed denial fact: {rest:#?}"
    );
}

/// U4b2: in Auto, a non-floor write runs WITHOUT a card, and the
/// auto-approval is audited with the auto-mode constraints (`auto_mode`).
#[test]
fn u4b2_auto_mode_writes_without_a_card() {
    let _g = lock();
    let mut rig = Rig::start_mode(
        vec![
            sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
            sse_text_final("done"),
        ],
        awili_work::SharedApprovalMode::new(awili_work::ApprovalMode::Auto),
    );
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let rest = rig.finish();
    assert!(
        !rest
            .iter()
            .any(|e| matches!(e, WorkEvent::ApprovalRequested { .. })),
        "Auto must not raise a card for a non-floor write: {rest:#?}"
    );
    assert!(
        rig.dir.path().join("new.txt").exists(),
        "the auto-approved write must have run: {rest:#?}"
    );
    let log_bytes = std::fs::read(&rig.log_path).unwrap();
    let log = String::from_utf8_lossy(&log_bytes);
    assert!(
        log.contains("auto_mode"),
        "the auto-approval is audited with the auto-mode constraints:\n{log}"
    );
}

/// U4b2 (D7): Auto still shows the card for a floor-escalated call — a
/// write to a secret file. Denying it leaves the file untouched.
#[test]
fn u4b2_auto_mode_still_cards_a_secret_file() {
    let _g = lock();
    let mut rig = Rig::start_mode(
        vec![
            sse_tool_round(&[("c1", "fs.write", r#"{"path":".env","contents":"x"}"#)]),
            sse_text_final("done"),
        ],
        awili_work::SharedApprovalMode::new(awili_work::ApprovalMode::Auto),
    );
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .expect("the floor-escalated call must still raise a card");
    rig.send(WorkCmd::Approval {
        id,
        choice: "deny".into(),
    });
    let rest = rig.finish();
    assert!(
        !rig.dir.path().join(".env").exists(),
        "the denied secret write must NOT have run"
    );
    assert!(
        rest.iter()
            .any(|e| matches!(e, WorkEvent::HookDenial { .. })),
        "the deny must surface the typed denial fact: {rest:#?}"
    );
}

/// U4b2: the mode is read at each gate — a switch to Auto before the
/// first prompt suppresses the card; a switch back to Ask before the
/// second prompt raises it again.
#[test]
fn u4b2_mode_switch_applies_at_the_next_gate() {
    let _g = lock();
    let mode = awili_work::SharedApprovalMode::default();
    let mut rig = Rig::start_mode(
        vec![
            sse_tool_round(&[("c1", "fs.write", r#"{"path":"a.txt","contents":"x"}"#)]),
            sse_text_final("one"),
            sse_tool_round(&[("c2", "fs.write", r#"{"path":"b.txt","contents":"x"}"#)]),
            sse_text_final("two"),
        ],
        mode.clone(),
    );
    // Switch to Auto BEFORE the first prompt: no card on the first write.
    mode.set(awili_work::ApprovalMode::Auto);
    rig.send(WorkCmd::Prompt {
        text: "first".into(),
        system: None,
    });
    let first = rig.until(is_turn_complete);
    assert!(
        !first
            .iter()
            .any(|e| matches!(e, WorkEvent::ApprovalRequested { .. })),
        "Auto must suppress the first card: {first:#?}"
    );
    assert!(
        rig.dir.path().join("a.txt").exists(),
        "the first auto-approved write must have run"
    );
    // Switch back to Ask BEFORE the second prompt: the card is back.
    mode.set(awili_work::ApprovalMode::AskBeforeChanges);
    rig.send(WorkCmd::Prompt {
        text: "second".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .expect("the switch back to Ask must raise the card again");
    rig.send(WorkCmd::Approval {
        id,
        choice: "once".into(),
    });
    rig.finish();
    assert!(
        rig.dir.path().join("b.txt").exists(),
        "the allow-once on the second write must let it run"
    );
}

/// RC1-F4b: the in-app engine's usage reaches the Work channel, one
/// `UsageReported` per model request (the app sums them like the agent lane's
/// frames): a tool round (10/10) then the final text (10/5).
#[test]
fn rc1f4b_engine_usage_reaches_the_work_channel() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("call_r", "fs.read", r#"{"path":"missing.txt"}"#)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "read it".into(),
        system: None,
    });
    let events = rig.until(is_turn_complete);
    let usage: Vec<(u64, u64)> = events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::UsageReported {
                input_tokens,
                output_tokens,
            } => Some((*input_tokens, *output_tokens)),
            _ => None,
        })
        .collect();
    assert_eq!(usage, [(10, 10), (10, 5)], "{events:#?}");
    let first_usage = events
        .iter()
        .position(|e| matches!(e, WorkEvent::UsageReported { .. }))
        .unwrap();
    let completed = events
        .iter()
        .position(|e| matches!(e, WorkEvent::TurnCompleted { .. }))
        .unwrap();
    assert!(
        first_usage < completed,
        "usage arrives before the turn ends"
    );
    rig.finish();
}

/// RC1-I3 (RC1 round 3/3b Minor 9: the card read `[mutating] fs.write
/// [mutating] {"contents": …, "path": …}`): the in-app approval card says
/// what will happen in plain words — the file and how many characters —
/// counted in characters (accents and emoji), never showing the text.
/// RC2-R1F1 (round 1 N4): it says it is a file and names the whole path,
/// folder and file; a long path is never cut (its file name always shows).
#[test]
fn rc1i3_approval_card_says_it_in_plain_words() {
    let _g = lock();
    let text = "\u{e9}\u{20ac}\u{1f600}".repeat(40);
    let long_path = format!("{}.txt", "\u{e9}\u{1f600}".repeat(50));
    let args1 = serde_json::json!({ "path": "eighth.txt", "contents": text }).to_string();
    let args2 = serde_json::json!({ "path": long_path, "contents": "x" }).to_string();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", args1.as_str())]),
        sse_tool_round(&[("c2", "fs.write", args2.as_str())]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let card = |seen: &[WorkEvent]| {
        seen.iter()
            .find_map(|e| match e {
                WorkEvent::ApprovalRequested {
                    approval_id,
                    description,
                    ..
                } => Some((approval_id.clone(), description.clone().unwrap_or_default())),
                _ => None,
            })
            .expect("a gate")
    };
    let dir = rig.dir.path().display().to_string();
    let (id, words) = card(&rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. })));
    assert_eq!(
        words,
        format!("Write the file {dir}/eighth.txt (120 characters)")
    );
    rig.send(WorkCmd::Approval {
        id,
        choice: "deny".into(),
    });
    let (id, words) = card(&rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. })));
    assert_eq!(
        words,
        format!("Write the file {dir}/{long_path} (1 character)")
    );
    rig.send(WorkCmd::Approval {
        id,
        choice: "deny".into(),
    });
    let _ = rig.finish();
    assert!(
        !rig.dir.path().join("eighth.txt").exists(),
        "nothing written"
    );
}

/// RC1-I3-FIX1 (Codex T2 P2 on afd9ac1): a model-supplied path with a newline
/// or a right-to-left override is shown escaped on the card — one target can
/// never look like several lines or another name.
#[test]
fn rc1i3_fix1_card_paths_show_control_characters_escaped() {
    let _g = lock();
    let args1 = serde_json::json!({ "path": "a\nb.txt", "contents": "x" }).to_string();
    let args2 = serde_json::json!({ "path": "report\u{202e}txt.exe", "contents": "x" }).to_string();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", args1.as_str())]),
        sse_tool_round(&[("c2", "fs.write", args2.as_str())]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let card = |seen: &[WorkEvent]| {
        seen.iter()
            .find_map(|e| match e {
                WorkEvent::ApprovalRequested {
                    approval_id,
                    description,
                    ..
                } => Some((approval_id.clone(), description.clone().unwrap_or_default())),
                _ => None,
            })
            .expect("a gate")
    };
    let dir = rig.dir.path().display().to_string();
    let (id, words) = card(&rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. })));
    assert_eq!(
        words,
        format!("Write the file {dir}/a\\nb.txt (1 character)")
    );
    assert!(!words.contains('\n'), "{words:?}");
    rig.send(WorkCmd::Approval {
        id,
        choice: "deny".into(),
    });
    let (id, words) = card(&rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. })));
    assert_eq!(
        words,
        format!("Write the file {dir}/report\\u{{202e}}txt.exe (1 character)")
    );
    rig.send(WorkCmd::Approval {
        id,
        choice: "deny".into(),
    });
    let _ = rig.finish();
}

/// RC1-I3-FIX2 (Codex T2 P2 on d02fd80): the card's path is unambiguous. A real
/// newline and the two characters `\` `n` show differently; the line and
/// paragraph separators U+2028/U+2029 and a zero-width space are escaped —
/// through the real worker, card by card.
#[test]
fn rc1i3_fix2_card_paths_are_unambiguous() {
    let _g = lock();
    let paths = [
        ("a\nb.txt", "Write a\\nb.txt (1 character)"),
        ("a\\nb.txt", "Write a\\\\nb.txt (1 character)"),
        ("x\u{2028}y.txt", "Write x\\u{2028}y.txt (1 character)"),
        ("x\u{2029}y.txt", "Write x\\u{2029}y.txt (1 character)"),
        ("a\u{200b}b.txt", "Write a\\u{200b}b.txt (1 character)"),
    ];
    let args: Vec<String> = paths
        .iter()
        .map(|(p, _)| serde_json::json!({ "path": p, "contents": "x" }).to_string())
        .collect();
    // The worker allows four model rounds per turn, so the five calls ride
    // two rounds (three, then two).
    let call = |i: usize| {
        (
            ["c0", "c1", "c2", "c3", "c4"][i],
            "fs.write",
            args[i].as_str(),
        )
    };
    let mut rig = Rig::start(vec![
        sse_tool_round(&[call(0), call(1), call(2)]),
        sse_tool_round(&[call(3), call(4)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write them".into(),
        system: None,
    });
    let mut shown = Vec::new();
    // RC2-R1F1: the card names the file inside the turn's folder.
    let dir = rig.dir.path().display().to_string();
    for (_, want) in paths {
        let want = want.replacen("Write ", &format!("Write the file {dir}/"), 1);
        let want = want.as_str();
        let seen = rig.fence_until(want, Instant::now() + Duration::from_secs(30), |e| {
            matches!(e, WorkEvent::ApprovalRequested { .. })
        });
        let (id, words) = seen
            .iter()
            .find_map(|e| match e {
                WorkEvent::ApprovalRequested {
                    approval_id,
                    description,
                    ..
                } => Some((approval_id.clone(), description.clone().unwrap_or_default())),
                _ => None,
            })
            .expect("a gate");
        assert_eq!(words, want);
        assert!(
            !words.contains(['\n', '\u{2028}', '\u{2029}', '\u{200b}']),
            "{words:?}"
        );
        shown.push(words);
        rig.send(WorkCmd::Approval {
            id,
            choice: "deny".into(),
        });
    }
    let _ = rig.finish();
    assert_ne!(shown[0], shown[1], "a newline and a literal \\n differ");
}

/// RC1-I3-FIX2: no control, format or separator character survives, and the
/// display is reversible (distinct paths never show alike).
#[test]
fn rc1i3_fix2_shown_path_escapes_every_invisible_character() {
    let cases = [
        "\u{ad}",
        "\u{61c}",
        "\u{200e}",
        "\u{200f}",
        "\u{202a}",
        "\u{202e}",
        "\u{2028}",
        "\u{2029}",
        "\u{2060}",
        "\u{2066}",
        "\u{2069}",
        "\u{feff}",
        "\u{e0001}",
        "\u{1b}",
        "\t",
        "\r",
        "\0",
    ];
    for c in cases {
        let shown = super::shown_path(&format!("a{c}b"));
        assert!(
            shown.starts_with("a\\") && shown.ends_with('b'),
            "{c:?}: {shown}"
        );
        assert!(shown.chars().all(super::shown_as_is), "{c:?}: {shown}");
    }
    let all = [
        "a\nb",
        "a\\nb",
        "a\\\nb",
        "a\\\\nb",
        "a\u{2028}b",
        "a\\u{2028}b",
    ];
    let shown: std::collections::BTreeSet<String> =
        all.iter().map(|p| super::shown_path(p)).collect();
    assert_eq!(
        shown.len(),
        all.len(),
        "distinct paths show distinctly: {shown:?}"
    );
    assert_eq!(super::shown_path("plain/é€.txt"), "plain/é€.txt");
}

/// RC1-I3-FIX3 (Codex T2 P2 on 15ac0a2): the card shows only visible
/// characters as themselves. `ab.txt` and `a<U+034F>b.txt` (a default-ignorable
/// mark) show differently; so do a Hangul filler (a letter that displays as
/// nothing), a no-break space and a private-use character — through the real
/// worker, card by card.
#[test]
fn rc1i3_fix3_card_paths_show_only_visible_characters() {
    let _g = lock();
    let paths = [
        ("ab.txt", "Write ab.txt (1 character)"),
        ("a\u{34f}b.txt", "Write a\\u{34f}b.txt (1 character)"),
        ("a\u{3164}b.txt", "Write a\\u{3164}b.txt (1 character)"),
        ("a\u{a0}b.txt", "Write a\\u{a0}b.txt (1 character)"),
        ("a\u{e000}b.txt", "Write a\\u{e000}b.txt (1 character)"),
    ];
    let args: Vec<String> = paths
        .iter()
        .map(|(p, _)| serde_json::json!({ "path": p, "contents": "x" }).to_string())
        .collect();
    // Four model rounds per turn: the five calls ride two rounds.
    let call = |i: usize| {
        (
            ["d0", "d1", "d2", "d3", "d4"][i],
            "fs.write",
            args[i].as_str(),
        )
    };
    let mut rig = Rig::start(vec![
        sse_tool_round(&[call(0), call(1), call(2)]),
        sse_tool_round(&[call(3), call(4)]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write them".into(),
        system: None,
    });
    let mut shown = Vec::new();
    // RC2-R1F1: the card names the file inside the turn's folder.
    let dir = rig.dir.path().display().to_string();
    for (_, want) in paths {
        let want = want.replacen("Write ", &format!("Write the file {dir}/"), 1);
        let want = want.as_str();
        let seen = rig.fence_until(want, Instant::now() + Duration::from_secs(30), |e| {
            matches!(e, WorkEvent::ApprovalRequested { .. })
        });
        let (id, words) = seen
            .iter()
            .find_map(|e| match e {
                WorkEvent::ApprovalRequested {
                    approval_id,
                    description,
                    ..
                } => Some((approval_id.clone(), description.clone().unwrap_or_default())),
                _ => None,
            })
            .expect("a gate");
        assert_eq!(words, want);
        shown.push(words);
        rig.send(WorkCmd::Approval {
            id,
            choice: "deny".into(),
        });
    }
    let _ = rig.finish();
    assert_ne!(shown[0], shown[1], "ab.txt and a<U+034F>b.txt differ");
}

/// RC1-I3-FIX3: every Default_Ignorable_Code_Point (the whole table, code
/// point by code point) is shown escaped, whatever its category.
#[test]
fn rc1i3_fix3_every_default_ignorable_is_escaped() {
    let table = super::DEFAULT_IGNORABLE;
    assert!(
        table.windows(2).all(|w| w[0].1 < w[1].0),
        "sorted, no overlap"
    );
    let mut checked = 0;
    for (lo, hi) in table {
        for cp in lo..=hi {
            let c = char::from_u32(cp).expect("no surrogates in the table");
            assert!(!super::shown_as_is(c), "U+{cp:04X}");
            assert_eq!(
                super::shown_path(&format!("a{c}b")),
                format!("a\\u{{{cp:x}}}b"),
                "U+{cp:04X}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 4174, "every code point of the 27 ranges");
}

/// RC1-I3-FIX3: across ALL of Unicode, a character is shown as itself only if
/// it is visible; everything else becomes a printable ASCII escape. Visually
/// confusable letters (Cyrillic а vs Latin a) are out of scope (RC2).
#[test]
fn rc1i3_fix3_only_visible_characters_show_as_themselves() {
    assert_eq!(unicode_properties::UNICODE_VERSION, (17, 0, 0));
    for cp in 0..=0x10FFFFu32 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let shown = super::shown_path(&c.to_string());
        assert!(
            !(super::shown_as_is(c) && super::is_default_ignorable(c)),
            "U+{cp:04X} is default-ignorable"
        );
        if super::shown_as_is(c) && c != '\\' {
            assert_eq!(shown, c.to_string(), "U+{cp:04X}");
        } else {
            assert!(
                shown.starts_with('\\') && shown.chars().all(|s| s.is_ascii_graphic()),
                "U+{cp:04X}: {shown}"
            );
        }
    }
    for (c, shown) in [
        ('\u{378}', "\\u{378}"),   // unassigned
        ('\u{e000}', "\\u{e000}"), // private use
        ('\u{a0}', "\\u{a0}"),     // no-break space
        ('\u{3000}', "\\u{3000}"), // ideographic space
        ('\u{2028}', "\\u{2028}"), // line separator
        ('\u{1b}', "\\u{1b}"),     // escape (Cc)
        ('\u{34f}', "\\u{34f}"),   // combining grapheme joiner (default-ignorable Mn)
        ('\u{3164}', "\\u{3164}"), // Hangul filler (default-ignorable Lo)
        ('\t', "\\t"),
        (' ', " "),
        ('\u{e9}', "\u{e9}"),     // é
        ('\u{301}', "\u{301}"),   // combining acute (a visible mark)
        ('\u{20ac}', "\u{20ac}"), // €
        ('\u{4e2d}', "\u{4e2d}"), // 中
        ('\u{1f600}', "\u{1f600}"),
        ('\u{430}', "\u{430}"), // Cyrillic а alone: one alphabet, shown (RC2-UX: a mixed word is escaped)
    ] {
        assert_eq!(super::shown_path(&c.to_string()), shown, "{c:?}");
    }
}

/// RC1-J (round 3c Major 2): Progress and the Tools list read the tool's
/// plain words — the card's words, the path allowlist-escaped — never the
/// raw JSON arguments; a tool without plain words shows only its name.
#[test]
fn rc1j_tool_started_shows_plain_words_never_json() {
    let _g = lock();
    let write_args =
        serde_json::json!({ "path": "x\u{34f}.txt", "contents": "{\"k\": 1}" }).to_string();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[
            ("p1", "fs.read", r#"{"path":"a.txt"}"#),
            ("p2", "fs.glob", r#"{"pattern":"*.txt"}"#),
        ]),
        sse_tool_round(&[("p3", "fs.write", write_args.as_str())]),
        sse_text_final("done"),
    ]);
    std::fs::write(rig.dir.path().join("a.txt"), b"hello").unwrap();
    rig.send(WorkCmd::Prompt {
        text: "go".into(),
        system: None,
    });
    let seen = rig.fence_until(
        "the write's card",
        Instant::now() + Duration::from_secs(30),
        |e| matches!(e, WorkEvent::ApprovalRequested { .. }),
    );
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .expect("a gate");
    rig.send(WorkCmd::Approval {
        id,
        choice: "deny".into(),
    });
    let mut evs = seen;
    evs.extend(rig.until(is_turn_complete));
    let details: Vec<Option<String>> = evs
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ToolStarted { detail, .. } => Some(detail.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        details,
        vec![
            Some("Read a.txt".to_string()),
            None,
            Some("Write x\\u{34f}.txt (8 characters)".to_string()),
        ]
    );
    let _ = rig.finish();
}

/// RC1-M-FIX1 (live 2026-10-03: Gemma's hostd outage turn ended "round bound
/// (4) exceeded" while it was still looking things up): a turn that needs
/// several tool rounds — here eight reads, then the answer — completes.
#[test]
fn rc1m_fix1_a_turn_with_many_tool_rounds_completes() {
    let _g = lock();
    let mut bodies: Vec<String> = (0..8)
        .map(|i| sse_tool_round(&[(&format!("c{i}"), "fs.read", r#"{"path":"a.txt"}"#)]))
        .collect();
    bodies.push(sse_text_final("all read"));
    let mut rig = Rig::start(bodies);
    std::fs::write(rig.dir.path().join("a.txt"), b"hi").unwrap();
    rig.send(WorkCmd::Prompt {
        text: "read it eight times".into(),
        system: None,
    });
    let events = rig.until(is_turn_complete);
    let completed = events.iter().find_map(|e| match e {
        WorkEvent::TurnCompleted { status, output, .. } => Some((*status, output.clone())),
        _ => None,
    });
    assert_eq!(
        completed,
        Some((RunStatus::Completed, Some("all read".to_string()))),
        "{events:#?}"
    );
    assert_eq!(
        rig.script.sent().len(),
        9,
        "eight tool rounds and the answer"
    );
    rig.finish();
}

/// RC1-N item 3: `deny-stop` (the Work surface's Deny) runs nothing, emits
/// no HookDenial line, ends the turn as Stopped — and the worker stays up:
/// the next prompt runs on the same session.
#[test]
fn rc1n_deny_stop_ends_the_turn_and_keeps_the_session() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"new.txt","contents":"x"}"#)]),
        sse_text_final("second turn ok"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "write it".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .expect("the engine must raise a correlated gate");
    rig.send(WorkCmd::Approval {
        id,
        choice: "deny-stop".into(),
    });
    let first = rig.until(|e| matches!(e, WorkEvent::TurnComplete));
    assert!(
        first.iter().any(|e| matches!(
            e,
            WorkEvent::TurnCompleted {
                status: RunStatus::Stopped,
                output: None,
                ..
            }
        )),
        "the denied turn ends Stopped: {first:#?}"
    );
    assert!(
        !first.iter().any(|e| matches!(
            e,
            WorkEvent::HookDenial { .. } | WorkEvent::ToolFinished { .. } | WorkEvent::Token(_)
        )),
        "no denial jargon, no failed-tool row, no model text: {first:#?}"
    );
    assert!(!rig.dir.path().join("new.txt").exists(), "nothing written");
    rig.send(WorkCmd::Prompt {
        text: "hello".into(),
        system: None,
    });
    let second = rig.until(|e| matches!(e, WorkEvent::TurnComplete));
    assert!(
        second.iter().any(|e| matches!(
            e,
            WorkEvent::TurnCompleted {
                status: RunStatus::Completed,
                output: Some(o),
                ..
            } if o == "second turn ok"
        )),
        "the session survives the Deny: {second:#?}"
    );
    rig.finish();
}

/// RC1-N (round 4 M3): a turn that cannot fit the model's window ends with
/// one plain sentence, never "context: cannot fit …"; a model server's
/// "context length exceeded" reads the same; other failures are unchanged.
#[test]
fn rc1n_a_turn_too_big_for_the_window_fails_in_plain_words() {
    let _g = lock();
    let mut rig = Rig::start(vec![sse_text_final("never asked")]);
    rig.send(WorkCmd::Prompt {
        text: "x".repeat(400_000),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::TurnComplete));
    let output = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::TurnCompleted {
                status: RunStatus::Failed,
                output,
                ..
            } => output.clone(),
            _ => None,
        })
        .expect("the turn fails: {seen:#?}");
    assert!(
        output.starts_with("This turn needed more room than the model can hold"),
        "{output}"
    );
    assert!(!output.contains("cannot fit") && !output.contains("context:"));
    // RC1-N-FIX1 (whole-slice review P2): no claim that nothing was done.
    assert!(!output.contains("Nothing more was done"), "{output}");
    assert!(output.contains("may have completed"), "{output}");
    rig.finish();
    for raw in [
        "context: cannot fit: 37571 tokens needed after maximal compaction, budget 30208",
        "HTTP 400: the request exceeds the available context size",
        "This model's maximum context length is 32768 tokens",
        "{\"error\":{\"code\":\"context_length_exceeded\"}}",
    ] {
        assert!(
            crate::plain_turn_failure(&SeamError::Transport(raw.into()), 32_768)
                .starts_with("This turn needed more room"),
            "{raw}"
        );
    }
    // The model server's refusal as an error BODY ends the turn plainly too.
    let mut rig = Rig::start(vec![
        r#"{"error":{"message":"This model's maximum context length is 32768 tokens","type":"invalid_request_error"}}"#.into(),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "hi".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::TurnComplete));
    assert!(
        seen.iter().any(|e| matches!(e, WorkEvent::TurnCompleted {
            status: RunStatus::Failed,
            output: Some(o),
            ..
        } if o.starts_with("This turn needed more room"))),
        "{seen:#?}"
    );
    rig.finish();
    // Pre-review P1 (pass 6): the summarizer's refusal reads the same.
    assert!(crate::plain_turn_failure(
        &SeamError::Context(
            "summarizer failed: This model's maximum context length is 32768 tokens".into()
        ),
        32_768
    )
    .starts_with("This turn needed more room"));
    let other = SeamError::Transport("connection refused".into());
    assert_eq!(
        crate::plain_turn_failure(&other, 32_768),
        format!("{other}")
    );
}

/// RC1-N-FIX2: one prompt on a fresh worker over `log_path` (with `history`
/// as the kept model history); returns the request bodies it sent.
fn one_prompt_on_a_new_worker(
    dir: &std::path::Path,
    log_path: &std::path::Path,
    history: Option<ModelHistory>,
    prompt: &str,
) -> Vec<String> {
    let script = Script::new(vec![sse_text_final("ok")]);
    let mut cfg = AwiliEngineConfig::new(
        "http://unused",
        "test-model",
        log_path.to_path_buf(),
        dir.join("grants.jsonl"),
    );
    cfg.history = history;
    let worker =
        AwiliEngineWorker::with_transport(cfg, authority(), Box::new(script.clone())).unwrap();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    cmd_tx
        .send(WorkCmd::Prompt {
            text: prompt.into(),
            system: None,
        })
        .unwrap();
    loop {
        match events.recv_blocking().expect("worker alive") {
            WorkEvent::ApprovalRequested { approval_id, .. } => {
                // A recovery gate would hold the prompt; the cases here have
                // no uncertain dispatch, so none may appear.
                panic!("unexpected card {approval_id:?}");
            }
            WorkEvent::TurnComplete => break,
            WorkEvent::Failed(m) => panic!("worker failed: {m}"),
            _ => {}
        }
    }
    drop(cmd_tx);
    handle.join().unwrap();
    script.sent()
}

/// RC1-N-FIX2 (whole-slice R2 P1), the Stop path's fallback: when the app
/// could not keep a stopped worker's history (it did not publish in time),
/// the next worker over the same log shows the model the completed tool call
/// (and its result) from the log, so it is not repeated. (The app's own path:
/// `rc1n_fix2_in_app_stop_then_resubmit_keeps_the_completed_write`.)
#[test]
fn rc1n_fix2_a_new_worker_recovers_a_stopped_turns_completed_work() {
    let _g = lock();
    let mut rig = Rig::start_with(
        vec![
            sse_tool_round(&[("c1", "fs.read", r#"{"path":"a.txt"}"#)]),
            sse_text_final("must not be reached"),
        ],
        |cancel, _journal| HostHooks {
            on_tool_completed: Some(Arc::new({
                let cancel = cancel.clone();
                move || cancel.store(true, Ordering::SeqCst)
            })),
            on_approval_rejected: None,
            on_log_wait: None,
            log_try_lock: None,
        },
    );
    std::fs::write(rig.dir.path().join("a.txt"), b"STOPPED-TURN-FILE").unwrap();
    rig.send(WorkCmd::Prompt {
        text: "read it".into(),
        system: None,
    });
    let _ = rig.finish_bounded(Instant::now() + FENCE_TIMEOUT);
    let dir = rig.dir.path().to_path_buf();
    let log_path = rig.log_path.clone();
    let sent = one_prompt_on_a_new_worker(&dir, &log_path, None, "and now?");
    assert!(
        sent[0].contains("STOPPED-TURN-FILE"),
        "the stopped turn's completed read is in the next worker's history: {}",
        &sent[0]
    );
}

/// RC1-N-FIX2 (whole-slice R2 P1), the crash path: a log whose newest turn
/// never closed (the app died) holds a completed call and a call that never
/// got a result. A new worker shows the model the completed one as a
/// labelled note of earlier steps and drops the unfinished tail; when the
/// kept history already holds that call (the app kept the turn), it is not
/// shown twice.
#[test]
fn rc1n_fix2_a_new_worker_recovers_a_crashed_turns_completed_work() {
    let _g = lock();
    let c1 = awili_engine::events::ToolCall {
        awili_call_id: AwiliCallId("turn-awili-0-call-0000".into()),
        provider_call_id: Some("p1".into()),
        name: "fs.read".into(),
        arguments: r#"{"path":"a.txt"}"#.into(),
    };
    let c2 = awili_engine::events::ToolCall {
        awili_call_id: AwiliCallId("turn-awili-0-call-0001".into()),
        provider_call_id: Some("p2".into()),
        name: "fs.write".into(),
        arguments: r#"{"path":"never-ran.txt","contents":"x"}"#.into(),
    };
    let r1 = awili_engine::events::ToolResultRecord {
        awili_call_id: c1.awili_call_id.clone(),
        content: "CRASH-RESULT".into(),
        is_error: false,
    };
    // The same crashed log, written fresh for each case.
    let crashed_log_ending = |log_path: &std::path::Path, completed: bool| {
        let store = awili_engine_log::storage::FileLog::open(log_path).unwrap();
        let mut log = ExecutionLog::open(store, "sess-crashed").unwrap();
        log.turn_opened("turn-awili-0", "test", "test-model")
            .unwrap();
        log.record_event("turn-awili-0", &AgentEvent::TurnStarted)
            .unwrap();
        log.record_event("turn-awili-0", &AgentEvent::ToolCallReady(c1.clone()))
            .unwrap();
        log.record_event("turn-awili-0", &AgentEvent::ToolCallReady(c2.clone()))
            .unwrap();
        // c1 ran: its intent, dispatch and finalized result are durable (a
        // crash right after finalizing still counts it).
        let host = authority();
        let resolved = host.registry.mint(awili_engine::ToolIdentity::new(
            awili_engine::ToolOrigin::new("builtin:fs"),
            "fs.read",
            awili_engine::DescriptorDigest([0u8; 32]),
        ));
        let empty = host.authorizer.load_grants(&[], &[]);
        let evidence = match host.authorizer.authorize(
            &c1,
            awili_engine_authz::AuthzContext {
                turn_id: "turn-awili-0",
                session_id: "sess-crashed",
                folder_root: None,
                grants: &empty,
                registry_floor: None,
                resolved_tool: &resolved,
                install_witness: None,
            },
        ) {
            awili_engine_authz::Decision::Allow { evidence } => evidence,
            _ => panic!("read_only policy allow expected"),
        };
        let awili_engine_log::IntentOutcome::Dispatchable(ticket) =
            log.intent("turn-awili-0", &c1, evidence).unwrap()
        else {
            panic!("dispatchable");
        };
        log.dispatch_started(&ticket).unwrap();
        log.dispatch_finalized(&ticket, true, &r1.content).unwrap();
        log.record_event("turn-awili-0", &AgentEvent::ToolResultIngested(r1.clone()))
            .unwrap();
        if completed {
            // The turn ended normally, then the app died before keeping its
            // history (c2 then never ran either: the model did not ask).
            log.turn_closed("turn-awili-0", "completed").unwrap();
        }
        // CRASH: c2 never ran.
    };
    let crashed_log = |log_path: &std::path::Path| crashed_log_ending(log_path, false);
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("b2.jsonl");
    crashed_log(&log_path);
    let sent = one_prompt_on_a_new_worker(dir.path(), &log_path, None, "and now?");
    assert_eq!(sent[0].matches("CRASH-RESULT").count(), 1, "{}", &sent[0]);
    assert!(
        !sent[0].contains("never-ran.txt"),
        "the unfinished tail is dropped"
    );
    // A labelled note of earlier completed steps, on the user side (where the
    // interrupted turn sat among the transcript turns is not known).
    let v: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    assert!(
        v["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|c| c.starts_with(awili_seam::RECOVERED_WORK_LABEL)
                        && c.contains("CRASH-RESULT"))),
        "{}",
        &sent[0]
    );
    // The app kept that turn already: no second copy.
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("b2.jsonl");
    crashed_log(&log_path);
    let mut kept = ContextHistory::new();
    kept.push_user("read it");
    kept.push_exchange("", vec![c1.clone()], vec![r1.clone()])
        .unwrap();
    let sent = one_prompt_on_a_new_worker(
        dir.path(),
        &log_path,
        Some(ModelHistory(kept)),
        "and again?",
    );
    assert_eq!(sent[0].matches("CRASH-RESULT").count(), 1, "{}", &sent[0]);
    // Pre-review pass 4: the turn COMPLETED, but the app died before its
    // history was kept and none was kept before it: its step is recovered.
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("b2.jsonl");
    crashed_log_ending(&log_path, true);
    let sent = one_prompt_on_a_new_worker(dir.path(), &log_path, None, "and now?");
    assert_eq!(sent[0].matches("CRASH-RESULT").count(), 1, "{}", &sent[0]);
    assert!(sent[0].contains("Steps you already completed earlier in this conversation"));
    // A kept history marked as holding that turn (its exchange holds c1):
    // nothing is recovered — the result reaches the model once, as c1's.
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("b2.jsonl");
    crashed_log_ending(&log_path, true);
    let mut marked = ContextHistory::new();
    marked.push_user("read it");
    marked
        .push_exchange("", vec![c1.clone()], vec![r1.clone()])
        .unwrap();
    marked.set_log_turn("turn-awili-0");
    let sent = one_prompt_on_a_new_worker(
        dir.path(),
        &log_path,
        Some(ModelHistory(marked)),
        "and again?",
    );
    assert_eq!(sent[0].matches("CRASH-RESULT").count(), 1, "{}", &sent[0]);
    assert!(
        !sent[0].contains("Steps you already completed"),
        "{}",
        &sent[0]
    );
}

/// RC1-N-FIX4 (whole-slice R3 P1): a file write ran, then the log could not
/// record its result (`dispatch_finalized` failed, as on a full disk). The
/// worker ends with one plain line instead of taking the next prompt (its
/// uncertainty gate is built only at startup), and the next worker over the
/// same log holds that prompt behind the recovery gate, which names the
/// write. Denied, nothing runs: the write is not repeated.
#[test]
fn rc1n_fix4_a_failed_log_write_after_a_write_ends_the_worker_and_the_next_prompt_is_gated() {
    let _g = lock();
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("b2.jsonl");
    let note = dir.path().join("note.txt");
    let start = |bodies: Vec<String>, fault: Option<&'static str>| {
        let script = Script::new(bodies);
        let cfg = AwiliEngineConfig::new(
            "http://unused",
            "test-model",
            log_path.clone(),
            dir.path().join("grants.jsonl"),
        );
        let mut worker =
            AwiliEngineWorker::with_transport(cfg, authority(), Box::new(script.clone()))
                .unwrap()
                .with_approval_mode(awili_work::SharedApprovalMode::new(
                    awili_work::ApprovalMode::Auto,
                ));
        if let Some(text) = fault {
            worker = worker.with_log_fault(text);
        }
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (ev_tx, events) = async_channel::unbounded();
        let handle = awili_work::spawn_worker(
            Box::new(worker),
            WorkerContext {
                workdir: dir.path().display().to_string(),
                cmd_rx,
                cancel: Arc::new(AtomicBool::new(false)),
                events: ev_tx,
            },
        );
        (script, cmd_tx, events, handle)
    };
    let write = |contents: &str| {
        sse_tool_round(&[(
            "c1",
            "fs.write",
            &format!(r#"{{"path":"note.txt","contents":"{contents}"}}"#),
        )])
    };

    // Worker A: the write runs; recording its result fails.
    let (script, cmd_tx, events, handle) = start(
        // Without the fix, the second prompt's request gets the second
        // body: the write is repeated.
        vec![write("FIRST"), write("AGAIN"), sse_text_final("done again")],
        Some("dispatch_finalized"),
    );
    for text in ["write it", "and now?"] {
        // The second prompt finds the worker ended (or, without the fix,
        // runs on the same worker with no gate).
        let _ = cmd_tx.send(WorkCmd::Prompt {
            text: text.into(),
            system: None,
        });
    }
    drop(cmd_tx);
    handle.join().unwrap();
    let mut got = Vec::new();
    while let Ok(ev) = events.try_recv() {
        got.push(ev);
    }
    assert_eq!(std::fs::read_to_string(&note).unwrap(), "FIRST", "{got:#?}");
    assert_eq!(
        script.sent().len(),
        1,
        "the worker took no further turn: {got:#?}"
    );
    assert!(
        matches!(got.last(), Some(WorkEvent::Failed(m)) if m == crate::LOG_WRITE_FAILED),
        "the worker ends with the one plain line: {got:#?}"
    );

    // Worker B (the next prompt) over the same log: held behind the gate,
    // which names the write that may have run.
    let (script, cmd_tx, events, handle) = start(vec![write("AGAIN"), sse_text_final("x")], None);
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "and now?".into(),
            system: None,
        })
        .unwrap();
    let card = loop {
        match events.recv_blocking().expect("worker alive") {
            WorkEvent::ApprovalRequested {
                approval_id,
                description,
                ..
            } => break (approval_id, description),
            WorkEvent::TurnComplete => panic!("the prompt ran without the gate"),
            WorkEvent::Failed(m) => panic!("worker failed: {m}"),
            _ => {}
        }
    };
    assert_eq!(card.0.as_deref(), Some("recovery-gate"));
    assert!(
        card.1.as_deref().is_some_and(|d| d.contains("fs.write")),
        "{card:?}"
    );
    cmd_tx
        .send(WorkCmd::Approval {
            id: Some("recovery-gate".into()),
            choice: "deny-stop".into(),
        })
        .unwrap();
    loop {
        if let WorkEvent::TurnComplete = events.recv_blocking().expect("worker alive") {
            break;
        }
    }
    drop(cmd_tx);
    handle.join().unwrap();
    assert!(script.sent().is_empty(), "nothing ran behind the gate");
    assert_eq!(std::fs::read_to_string(&note).unwrap(), "FIRST");

    // Pre-review P1: the write was recorded, then the model server failed
    // (a recoverable error) and closing the turn in the log failed. That is a
    // log write failure too: the worker ends instead of taking the next
    // prompt.
    std::fs::remove_file(&log_path).unwrap();
    let (script, cmd_tx, events, handle) = start(vec![write("FIRST")], Some("turn_closed"));
    for text in ["write it", "and now?"] {
        let _ = cmd_tx.send(WorkCmd::Prompt {
            text: text.into(),
            system: None,
        });
    }
    drop(cmd_tx);
    handle.join().unwrap();
    let mut got = Vec::new();
    while let Ok(ev) = events.try_recv() {
        got.push(ev);
    }
    assert_eq!(
        script.sent().len(),
        2,
        "the worker took no further turn: {got:#?}"
    );
    assert!(
        matches!(got.last(), Some(WorkEvent::Failed(m)) if m == crate::LOG_WRITE_FAILED),
        "{got:#?}"
    );
}

/// RC1-O (round 5 K3): an owner MCP tool's card reads in plain words, never
/// the authorizer's `write_file [privileged] {"content": …}` JSON. Pre-review
/// 1 P1: the card (full) shows every argument and every value whole.
/// Pre-review 2 P1: names and text are quoted, so different calls never read
/// alike.
#[test]
fn rc1o_other_tools_get_plain_words() {
    let k3 = r#"{"content":"hi","path":"hello3.txt"}"#;
    for full in [true, false] {
        assert_eq!(
            plain_tool_call("write_file", k3, full).as_deref(),
            Some("Run \"write_file\" \u{2014} \"content\": \"hi\", \"path\": \"hello3.txt\"")
        );
    }
    let long = "x".repeat(61);
    let args = serde_json::json!({
        "a_command": "echo a\nrm -rf b",
        "b_body": long,
        "c_items": ["rm", "-rf"],
        "d_opts": {"force": true},
        "e_timeout": 30,
        "f_hidden": "rm -rf /"
    })
    .to_string();
    let card = plain_tool_call("run_command", &args, true).unwrap();
    let want = format!(
        "Run \"run_command\" \u{2014} \"a_command\": \"echo a\\nrm -rf b\", \"b_body\": \"{long}\", \
         \"c_items\": [\"rm\",\"-rf\"], \"d_opts\": {{\"force\":true}}, \"e_timeout\": 30, \
         \"f_hidden\": \"rm -rf /\""
    );
    assert_eq!(card, want);
    assert!(!card.contains('\n'), "{card}");
    // Progress and the Tools list: short form.
    assert_eq!(
        plain_tool_call("run_command", &args, false).as_deref(),
        Some(
            "Run \"run_command\" \u{2014} \"a_command\": \"echo a\\nrm -rf b\", \"b_body\": 61 characters, \
             \"c_items\": [\"rm\",\"-rf\"], \"d_opts\": {\"force\":true}, and 2 more"
        )
    );
    // Different calls never read alike.
    let one = plain_tool_call("t", r#"{"a":"x\", \"b\": \"y"}"#, true).unwrap();
    let two = plain_tool_call("t", r#"{"a":"x","b":"y"}"#, true).unwrap();
    assert_ne!(one, two);
    assert_eq!(two, "Run \"t\" \u{2014} \"a\": \"x\", \"b\": \"y\"");
    assert_eq!(one, "Run \"t\" \u{2014} \"a\": \"x\\\", \\\"b\\\": \\\"y\"");
    // Pre-review 5 P1: the tool's name is quoted too, so a name holding
    // what looks like an argument never reads like a real argument.
    assert_ne!(
        plain_tool_call("x \u{2014} \"a\": 1", "{}", true),
        plain_tool_call("x", r#"{"a":1}"#, true)
    );
    assert_ne!(
        plain_tool_call("t", r#"{"a":"false"}"#, true),
        plain_tool_call("t", r#"{"a":false}"#, true)
    );
    assert_ne!(
        plain_tool_call("t", r#"{"a":"\\\""}"#, true),
        plain_tool_call("t", r#"{"a":"\""}"#, true)
    );
    // A value that would look like something else is escaped.
    assert_eq!(
        plain_tool_call("t", r#"{"path":"a͏b.txt"}"#, true).as_deref(),
        Some("Run \"t\" \u{2014} \"path\": \"a\\u{34f}b.txt\"")
    );
    assert_eq!(
        plain_tool_call("t", "{}", true).as_deref(),
        Some("Run \"t\"")
    );
    assert_eq!(plain_tool_call("t", "[1]", true), None);
    assert_eq!(plain_tool_call("t", "not json", true), None);
    // The engine's own file tools keep their own words (or none).
    assert_eq!(plain_tool_call("fs.glob", r#"{"pattern":"*"}"#, true), None);
    assert_eq!(
        plain_approval("fs.write", r#"{"path":"a.txt","content":"one"}"#).as_deref(),
        Some("Write a.txt (3 characters)")
    );
}

/// RC1-O (round 5 K3): a call to another tool whose registry floor is not
/// read-only, once it ran, tells the surface it may have changed files, just
/// before its ToolFinished — RC1-O-FIX5: whether it then succeeded or
/// failed; the engine's own file tools, read-only tools and calls that never
/// ran (no `tool_dispatching`) never do.
#[test]
fn rc1o_other_tools_that_may_change_files_are_reported() {
    use awili_engine::events::{AwiliCallId, ToolCall, ToolResultRecord};
    use awili_seam::TurnHost;
    let (tx, rx) = async_channel::unbounded();
    let (_ctx, crx) = std::sync::mpsc::channel();
    let cancel = AtomicBool::new(false);
    let mut host = ChannelHost {
        events: &tx,
        cmds: &crx,
        cancel: &cancel,
        model: "m".into(),
        turn_id: "t".into(),
        workdir: String::new(),
        call_names: Default::default(),
        call_args: Default::default(),
        may_change: Default::default(),
        shutdown_seen: false,
        disconnected: false,
        stop_after_deny: false,
        approval_mode: Default::default(),
        any_folder: false,
        channel_split: Default::default(),
        hooks: HostHooks::default(),
    };
    let calls = [
        ("c1", "write_file", RiskClass::Privileged, false),
        ("c2", "recall", RiskClass::ReadOnly, false),
        ("c3", "fs.write", RiskClass::Mutating, false),
        ("c4", "run_command", RiskClass::Privileged, true),
        ("c5", "edit_page", RiskClass::Mutating, false),
    ];
    for (id, name, floor, failed) in calls {
        host.observe(&AgentEvent::ToolCallReady(ToolCall {
            awili_call_id: AwiliCallId(id.into()),
            provider_call_id: None,
            name: name.into(),
            arguments: "{}".into(),
        }))
        .unwrap();
        host.tool_dispatching(id, floor);
        host.observe(&AgentEvent::ToolResultIngested(ToolResultRecord {
            awili_call_id: AwiliCallId(id.into()),
            content: "ok".into(),
            is_error: failed,
        }))
        .unwrap();
    }
    // A call that was denied never dispatched: nothing to report.
    host.observe(&AgentEvent::ToolCallReady(ToolCall {
        awili_call_id: AwiliCallId("c6".into()),
        provider_call_id: None,
        name: "never_ran".into(),
        arguments: "{}".into(),
    }))
    .unwrap();
    host.observe(&AgentEvent::ToolResultIngested(ToolResultRecord {
        awili_call_id: AwiliCallId("c6".into()),
        content: "user denied the request".into(),
        is_error: true,
    }))
    .unwrap();
    drop(host);
    let seen: Vec<WorkEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    let may: Vec<&str> = seen
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ToolMayHaveChangedFiles { name } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    // RC1-O-FIX5: a call that ran and then failed (c4) may have changed
    // files too; a call that never dispatched is never reported (below).
    assert_eq!(may, ["write_file", "run_command", "edit_page"]);
    // Each one comes right before its own ToolFinished.
    for (i, e) in seen.iter().enumerate() {
        if let WorkEvent::ToolMayHaveChangedFiles { name } = e {
            assert!(
                matches!(&seen[i + 1], WorkEvent::ToolFinished { name: Some(n), .. } if n == name),
                "{seen:?}"
            );
        }
    }
}

/// RC1-Q (round 5 minor 5): the Tools list shows a result only when it is one
/// short line of prose; JSON and long or multi-line results are not shown.
#[test]
fn rc1q_only_short_prose_results_reach_the_tools_list() {
    assert_eq!(
        plain_result("wrote 2 bytes to a.txt").as_deref(),
        Some("wrote 2 bytes to a.txt")
    );
    assert_eq!(plain_result("  ok \n").as_deref(), Some("ok"));
    assert_eq!(plain_result(r#"{ "degraded_reason": null }"#), None);
    assert_eq!(plain_result("[1, 2]"), None);
    assert_eq!(plain_result("line one\nline two"), None);
    assert_eq!(plain_result(&"x".repeat(161)), None);
    assert_eq!(plain_result(&"x".repeat(160)).map(|s| s.len()), Some(160));
    assert_eq!(plain_result("   "), None);
}

/// RC1-O-FIX6 (whole-slice review P2): a dispatched call whose result never
/// arrives (the turn was cancelled, or its stream closed) is still reported
/// as one that may have changed files when the turn ends; once.
#[test]
fn rc1o_fix6_a_dispatched_call_without_a_result_is_reported_at_turn_end() {
    use awili_engine::events::{AwiliCallId, ToolCall};
    use awili_seam::TurnHost;
    let (tx, rx) = async_channel::unbounded();
    let (_ctx, crx) = std::sync::mpsc::channel();
    let cancel = AtomicBool::new(false);
    let mut host = ChannelHost {
        events: &tx,
        cmds: &crx,
        cancel: &cancel,
        model: "m".into(),
        turn_id: "t".into(),
        workdir: String::new(),
        call_names: Default::default(),
        call_args: Default::default(),
        may_change: Default::default(),
        shutdown_seen: false,
        disconnected: false,
        stop_after_deny: false,
        approval_mode: Default::default(),
        any_folder: false,
        channel_split: Default::default(),
        hooks: HostHooks::default(),
    };
    for (id, name) in [("c1", "run_command"), ("c2", "fs.write")] {
        host.observe(&AgentEvent::ToolCallReady(ToolCall {
            awili_call_id: AwiliCallId(id.into()),
            provider_call_id: None,
            name: name.into(),
            arguments: "{}".into(),
        }))
        .unwrap();
        host.tool_dispatching(id, RiskClass::Privileged);
    }
    host.flush_may_change();
    host.flush_may_change();
    drop(host);
    let may: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|e| match e {
            WorkEvent::ToolMayHaveChangedFiles { name } => Some(name),
            _ => None,
        })
        .collect();
    assert_eq!(may, ["run_command"]);
}

// ============================================================================
// RC2-ENG4 — the conversation's log has one worker at a time (F0645); an
// unreadable grant store stops the worker plainly (F0644)
// ============================================================================

/// The TurnOpened ids of a log file, in order.
fn rc2eng4_turn_ids(log_path: &std::path::Path) -> Vec<String> {
    let store = awili_engine_log::storage::FileLog::open(log_path).unwrap();
    let (recs, _) = ExecutionLog::open(store, "sess-reader")
        .unwrap()
        .read_records()
        .unwrap();
    recs.iter()
        .filter_map(|r| match r {
            awili_engine_log::schema::LogRecordV1::TurnOpened { turn_id, .. } => {
                Some(turn_id.clone())
            }
            _ => None,
        })
        .collect()
}

/// F0645: while an earlier worker still holds the conversation's log (it is
/// being stopped during a respawn and writes one more turn), a new worker
/// waits for it before numbering its turns — so it never mints a turn id
/// the earlier worker used. Before RC2-ENG4 the new worker scanned the log
/// at once and reused `turn-awili-0`.
#[test]
fn rc2eng4_a_respawned_worker_waits_for_the_log_and_never_reuses_a_turn_id() {
    let _g = lock();
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("b2.jsonl");
    // The earlier worker: holds the log the way a worker does.
    let held = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap();
    held.lock().unwrap();

    let script = Script::new(vec![sse_text_final("ok")]);
    let cfg = AwiliEngineConfig::new(
        "http://unused",
        "test-model",
        log_path.clone(),
        dir.path().join("grants.jsonl"),
    );
    // RC2-ENGF4 (RC2-ENG slice review P3): a deterministic barrier instead of
    // a 500 ms sleep — the worker itself says it found the log held and is
    // waiting. A worker that does not wait never says so, and fails here
    // however slowly it starts.
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let hooks = HostHooks {
        on_log_wait: Some(Arc::new(move || {
            let _ = waiting_tx.send(());
        })),
        ..HostHooks::default()
    };
    let worker = AwiliEngineWorker::with_transport_and_hooks(
        cfg,
        authority(),
        Box::new(script.clone()),
        hooks,
    )
    .unwrap();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let _handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    waiting_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the new worker finds the log held and waits for it");
    // RC2-ENGG4 (RC2-ENGALL slice review P3): the prompt goes out while the
    // log is still held, so a worker that went on without the lock would
    // run its turn now, before the earlier worker's last turn is written.
    // (Ordering proof without timing: rc2engg4_the_turn_scan_follows_the_log_lock.)
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "hello".into(),
            system: None,
        })
        .unwrap();
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok()).any(|e| matches!(e, WorkEvent::Started(_))),
        "the new worker must not start while the log is held"
    );
    // The earlier worker's last turn, then it lets go.
    {
        let store = awili_engine_log::storage::FileLog::open(&log_path).unwrap();
        let mut log = ExecutionLog::open(store, "sess-earlier").unwrap();
        log.turn_opened("turn-awili-0", "test", "test-model")
            .unwrap();
        log.turn_closed("turn-awili-0", "completed").unwrap();
    }
    held.unlock().unwrap();
    drop(held);

    loop {
        match events.recv_blocking().expect("worker alive") {
            WorkEvent::TurnComplete => break,
            WorkEvent::Failed(m) => panic!("worker failed: {m}"),
            _ => {}
        }
    }
    drop(cmd_tx);
    let ids = rc2eng4_turn_ids(&log_path);
    assert_eq!(ids, ["turn-awili-0", "turn-awili-1"], "{ids:?}");
}

/// RC2-ENGG4 (RC2-ENGALL slice review P3): the worker numbers its turns only
/// after it holds the log. The rig plays every lock attempt: the first two
/// find the log held; at the third the earlier worker writes its last turn
/// and lets go, all inside that attempt. A worker that scanned before it
/// held the log — or stopped waiting early — would mint that turn's id
/// again; no timing is involved.
#[test]
fn rc2engg4_the_turn_scan_follows_the_log_lock() {
    let _g = lock();
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("b2.jsonl");
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hooks = HostHooks {
        log_try_lock: Some(Arc::new({
            let attempts = attempts.clone();
            let log_path = log_path.clone();
            move |_log: &std::fs::File| match attempts.fetch_add(1, Ordering::SeqCst) {
                0 | 1 => Err(std::fs::TryLockError::WouldBlock),
                2 => {
                    let store = awili_engine_log::storage::FileLog::open(&log_path).unwrap();
                    let mut log = ExecutionLog::open(store, "sess-earlier").unwrap();
                    log.turn_opened("turn-awili-0", "test", "test-model")
                        .unwrap();
                    log.turn_closed("turn-awili-0", "completed").unwrap();
                    Ok(())
                }
                n => panic!("the log was tried again after it was taken (attempt {n})"),
            }
        })),
        ..HostHooks::default()
    };
    let cfg = AwiliEngineConfig::new(
        "http://unused",
        "test-model",
        log_path.clone(),
        dir.path().join("grants.jsonl"),
    );
    let worker = AwiliEngineWorker::with_transport_and_hooks(
        cfg,
        authority(),
        Box::new(Script::new(vec![sse_text_final("ok")])),
        hooks,
    )
    .unwrap();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "hello".into(),
            system: None,
        })
        .unwrap();
    loop {
        match events.recv_blocking().expect("worker alive") {
            WorkEvent::TurnComplete => break,
            WorkEvent::Failed(m) => panic!("worker failed: {m}"),
            _ => {}
        }
    }
    drop(cmd_tx);
    handle.join().unwrap();
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "the worker kept trying until the log was free"
    );
    let ids = rc2eng4_turn_ids(&log_path);
    assert_eq!(ids, ["turn-awili-0", "turn-awili-1"], "{ids:?}");
}

/// F0644: a grant store that cannot be read at startup is not an empty one
/// (that would call every grant-backed step "failed authorization"): the
/// worker stops in plain words before it starts. Before RC2-ENG4 it started
/// with no grants.
#[test]
fn rc2eng4_an_unreadable_grant_store_stops_the_worker_plainly() {
    let _g = lock();
    let dir = tempfile::tempdir().unwrap();
    let grant_path = dir.path().join("grants.jsonl");
    // The process-long service for this store, then a whole frame that is
    // not a record: every later read of the store fails.
    awili_engine_log::grant_service::GrantStoreService::init(grant_path.clone(), None).unwrap();
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&grant_path)
            .unwrap();
        f.write_all(&awili_engine_log::frame::encode(b"{not a record"))
            .unwrap();
    }
    let script = Script::new(vec![sse_text_final("ok")]);
    let cfg = AwiliEngineConfig::new(
        "http://unused",
        "test-model",
        dir.path().join("b2.jsonl"),
        grant_path,
    );
    let worker =
        AwiliEngineWorker::with_transport(cfg, authority(), Box::new(script.clone())).unwrap();
    let (_cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let _handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    match events.recv_blocking().expect("one event") {
        WorkEvent::Failed(m) => {
            assert_eq!(m, GRANTS_UNREADABLE);
            assert!(!m.contains("Corrupt") && !m.contains("grant store"), "{m}");
        }
        other => panic!("the worker must stop before it starts, got {other:?}"),
    }
}

/// RC2-GEMMA1 (R7-speed): the recalled `<past-context>` block leaves the
/// system message and opens the turn's user message. Two turns with
/// DIFFERENT recall send the SAME system message (the model server answers
/// it from its cache), and the second turn's history holds the first
/// message as the user's words alone.
#[test]
fn rc2gemma1_recalled_context_leaves_the_system_message() {
    let _g = lock();
    let mut rig = Rig::start(vec![sse_text_final("one"), sse_text_final("two")]);
    let block = |fact: &str| {
        format!(
            "<past-context>\nRelevant context from your own memory (background only; \
             standing directives always take precedence):\n- {fact}\n</past-context>"
        )
    };
    let system = |fact: &str| {
        format!(
            "Standing directives (always apply, in priority order):\n1. Rule.\n\n{}\n\nHow to answer in Awili:\n- Be brief.",
            block(fact)
        )
    };
    for (text, fact) in [("first", "fact A"), ("second", "fact B")] {
        rig.send(WorkCmd::Prompt {
            text: text.into(),
            system: Some(system(fact)),
        });
        let _ = rig.until(is_turn_complete);
    }
    let sent: Vec<serde_json::Value> = rig
        .script
        .sent()
        .iter()
        .map(|b| serde_json::from_str(b).unwrap())
        .collect();
    assert_eq!(sent.len(), 2);
    let messages = |i: usize| sent[i]["messages"].as_array().unwrap().clone();
    let stable = "Standing directives (always apply, in priority order):\n1. Rule.\n\nHow to answer in Awili:\n- Be brief.";
    for i in 0..2 {
        let m = messages(i);
        assert_eq!(m[0]["role"], "system");
        assert_eq!(
            m[0]["content"], stable,
            "request {i}: the system message never carries recall"
        );
    }
    let first = messages(0);
    assert_eq!(
        first.last().unwrap()["content"],
        format!("{}\n\nfirst", block("fact A"))
    );
    let second = messages(1);
    let users: Vec<String> = second
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].as_str().unwrap().to_string())
        .collect();
    // RC2-R1F6 (round 1 N5): the kept first message is the one the model
    // was sent — its notes stay, so the next request repeats the cached one.
    assert_eq!(
        users,
        vec![
            format!("{}\n\nfirst", block("fact A")),
            format!("{}\n\nsecond", block("fact B"))
        ],
        "the kept first message is the one the model was sent"
    );
    rig.finish();
}

/// RC2-R1F6 (usability RC2 round 1, N5: a warm Gemma turn took 8-9 s to
/// first text). Every request of a conversation's next turn begins with the
/// previous turn's last request, message for message — the recalled notes
/// included — so the model server answers that part from its cache and
/// reads only the reply and the new message.
#[test]
fn rc2r1f6_the_next_turn_begins_with_the_last_request_byte_for_byte() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.read", r#"{"path":"a.txt"}"#)]),
        sse_text_final("one"),
        sse_text_final("two"),
    ]);
    std::fs::write(rig.dir.path().join("a.txt"), "hello").unwrap();
    let system = |fact: &str| {
        format!(
            "Standing directives (always apply, in priority order):\n1. Rule.\n\n\
             <past-context>\nRelevant context from your own memory (background only; \
             standing directives always take precedence):\n- {fact}\n</past-context>\n\n\
             How to answer in Awili:\n- Be brief."
        )
    };
    for (text, fact) in [("read a.txt", "fact A"), ("and then?", "fact B")] {
        rig.send(WorkCmd::Prompt {
            text: text.into(),
            system: Some(system(fact)),
        });
        let _ = rig.until(is_turn_complete);
    }
    let sent: Vec<serde_json::Value> = rig
        .script
        .sent()
        .iter()
        .map(|b| serde_json::from_str(b).unwrap())
        .collect();
    assert_eq!(sent.len(), 3, "a tool round, the answer, the next turn");
    let messages = |i: usize| sent[i]["messages"].as_array().unwrap().clone();
    let last_of_turn_one = messages(1);
    let next = messages(2);
    assert_eq!(
        next[..last_of_turn_one.len()],
        last_of_turn_one[..],
        "the next turn begins with the last request, notes and all"
    );
    assert_eq!(next[last_of_turn_one.len()]["role"], "assistant");
    assert_eq!(next[last_of_turn_one.len()]["content"], "one");
    let user = next.last().unwrap()["content"].as_str().unwrap();
    assert!(
        user.contains("fact B") && user.ends_with("and then?"),
        "{user}"
    );
    rig.finish();
}

/// RC2-GEMMA3: while the model server reads a long request the owner sees
/// how far it got, in plain words.
#[test]
fn rc2gemma3_the_reading_line_is_plain() {
    assert_eq!(reading_line(2048, 4096), "Reading your request\u{2026} 50%");
    assert_eq!(
        reading_line(4096, 4096),
        "Reading your request\u{2026} 100%"
    );
    assert_eq!(
        reading_line(5000, 4096),
        "Reading your request\u{2026} 100%"
    );
    assert_eq!(reading_line(0, 0), "Reading your request\u{2026} 0%");
}

/// RC2-GEMMA4 (R7-A3): a reply the server cuts off is asked for again once
/// (the app is told to take the cut text back); cut again, the turn ends in
/// plain words — never "stream truncated unexpectedly after N deltas".
#[test]
fn rc2gemma4_a_cut_reply_is_asked_again_then_ends_in_plain_words() {
    let _g = lock();
    let cut = chunk(r#"{"choices":[{"index":0,"delta":{"content":"HERON-77"}}]}"#);
    let mut rig = Rig::start(vec![cut.clone(), cut]);
    rig.send(WorkCmd::Prompt {
        text: "the codeword?".into(),
        system: None,
    });
    let evs = rig.until(is_turn_complete);
    assert_eq!(rig.script.sent().len(), 2, "asked again once");
    assert!(evs.iter().any(|e| matches!(
        e,
        WorkEvent::StatusUpdate { kind: Some(k), .. } if k == REPLY_RESTART
    )));
    let output = evs.iter().find_map(|e| match e {
        WorkEvent::TurnCompleted { status, output, .. } => Some((*status, output.clone())),
        _ => None,
    });
    assert_eq!(
        output,
        Some((
            awili_work::run_events::RunStatus::Failed,
            Some(REPLY_CUT_TWICE.to_string())
        ))
    );
    rig.finish();
}

/// RC2-GEMMA4 (R7-A3): mlx_lm's answer once its generation thread died
/// (`{"error": "generation thread died"}`) reads as a broken model server,
/// in plain words.
#[test]
fn rc2gemma4_a_broken_model_server_reads_plainly() {
    let _g = lock();
    let mut rig = Rig::start(vec![r#"{"error": "generation thread died"}"#.to_string()]);
    rig.send(WorkCmd::Prompt {
        text: "hello".into(),
        system: None,
    });
    let evs = rig.until(is_turn_complete);
    let output = evs.iter().find_map(|e| match e {
        WorkEvent::TurnCompleted { output, .. } => output.clone(),
        _ => None,
    });
    assert_eq!(output.as_deref(), Some(SERVER_BROKEN));
    rig.finish();
}

/// RC2-GEMMA8 (S-N1): the recovery card names what an unfinished step was
/// working on — its file, else its command — in plain words.
#[test]
fn rc2gemma8_the_recovery_card_names_the_file() {
    use awili_engine_log::replay::{IntentRecovery, IntentStatus};
    let intent = |tool: &str, args: &str, status: IntentStatus| IntentRecovery {
        intent_id: "i-1".into(),
        turn_id: "turn-awili-0".into(),
        tool: tool.into(),
        raw_arguments: args.into(),
        canonical_args: None,
        call_fingerprint: None,
        risk_class: "mutating".into(),
        identity_digest: String::new(),
        status,
    };
    let write = recovery_line(&intent(
        "fs.write",
        r#"{"path":"notes/plan.md","contents":"x"}"#,
        IntentStatus::Uncertain,
    ))
    .unwrap();
    assert!(
        write.contains("fs.write on \u{201C}notes/plan.md\u{201D}"),
        "{write}"
    );
    assert!(write.contains("may or may not have changed"), "{write}");
    for jargon in ["finalized", "side-effect", "unaudited"] {
        assert!(!write.contains(jargon), "{jargon}: {write}");
    }
    let shell = recovery_line(&intent(
        "run_command",
        r#"{"command":"zfs   snapshot\n vault@x"}"#,
        IntentStatus::Uncertain,
    ))
    .unwrap();
    assert!(
        shell.contains("\u{201C}zfs snapshot vault@x\u{201D}"),
        "{shell}"
    );
    let unknown = recovery_line(&intent("probe", "not json", IntentStatus::Uncertain)).unwrap();
    assert!(unknown.contains("probe on its target"), "{unknown}");
    let unchecked = recovery_line(&intent(
        "fs.edit",
        r#"{"path":"a.txt"}"#,
        IntentStatus::AuthzInvalid {
            reason: "digest mismatch".into(),
        },
    ))
    .unwrap();
    assert!(
        unchecked.contains("fs.edit on \u{201C}a.txt\u{201D}"),
        "{unchecked}"
    );
    assert!(recovery_line(&intent("fs.read", "{}", IntentStatus::Clean { ok: true })).is_none());
}

/// RC2-UX (S-I1, RC1-I3-FIX3 "not in scope"): a name that mixes Latin
/// letters with look-alike letters of another alphabet shows those letters
/// escaped on the card; a name in one alphabet is shown as itself, and a
/// Latin extension (".txt") never makes a non-Latin name "mixed".
#[test]
fn rc2ux_lookalike_letters_in_a_latin_name_are_escaped() {
    for (path, shown) in [
        ("p\u{430}ypal.txt", "p\\u{430}ypal.txt"), // Cyrillic а
        (
            "g\u{43e}\u{43e}gle/notes.md",
            "g\\u{43e}\\u{43e}gle/notes.md",
        ), // Cyrillic о
        ("\u{3bf}ffice.txt", "\\u{3bf}ffice.txt"), // Greek ο
        ("\u{430}pple", "\\u{430}pple"),
        ("paypal.txt", "paypal.txt"),
        (
            "\u{43e}\u{442}\u{447}\u{451}\u{442}.txt",
            "\u{43e}\u{442}\u{447}\u{451}\u{442}.txt",
        ),
        (
            "\u{414}\u{43e}\u{43a}\u{443}\u{43c}\u{435}\u{43d}\u{442}\u{44b}/report.txt",
            "\u{414}\u{43e}\u{43a}\u{443}\u{43c}\u{435}\u{43d}\u{442}\u{44b}/report.txt",
        ),
        ("caf\u{e9}-\u{3b1}\u{3b2}.md", "caf\u{e9}-\u{3b1}\u{3b2}.md"),
        ("\u{430}", "\u{430}"),
    ] {
        assert_eq!(super::shown_path(path), shown, "{path:?}");
    }
    // The card's words carry it (the built-in file tools' plain wording).
    let card = super::plain_approval("fs.write", r#"{"path":"pаypal.txt","content":"x"}"#)
        .expect("a card");
    assert!(card.contains("p\\u{430}ypal.txt"), "{card}");
}

/// RC2-R1F1 (usability RC2 round 1, N4: the card read "Write hello6.txt (2
/// characters)" — no folder, and not plainly a file write): through the real
/// worker, the in-app file card says it is a file and names the whole path,
/// folder and file; an absolute path is shown as it is.
#[test]
fn rc2r1f1_the_file_card_names_the_whole_path() {
    let _g = lock();
    let mut rig = Rig::start(vec![
        sse_tool_round(&[("c1", "fs.write", r#"{"path":"hello6.txt","contents":"hi"}"#)]),
        sse_text_final("done"),
    ]);
    let dir = rig.dir.path().display().to_string();
    rig.send(WorkCmd::Prompt {
        text: "write hello6.txt".into(),
        system: None,
    });
    let words = rig
        .until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }))
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested {
                approval_id,
                description,
                ..
            } => Some((approval_id.clone(), description.clone().unwrap_or_default())),
            _ => None,
        })
        .expect("a gate");
    assert_eq!(
        words.1,
        format!("Write the file {dir}/hello6.txt (2 characters)")
    );
    rig.send(WorkCmd::Approval {
        id: words.0,
        choice: "deny".into(),
    });
    let _ = rig.finish();
    // The other file tools, and a path given whole.
    assert_eq!(
        super::plain_approval_card("fs.edit", r#"{"path":"./a.txt"}"#, "/w/k3").as_deref(),
        Some("Edit the file /w/k3/a.txt")
    );
    assert_eq!(
        super::plain_approval_card("fs.read", r#"{"path":"/etc/hosts"}"#, "/w/k3").as_deref(),
        Some("Read the file /etc/hosts")
    );
    assert_eq!(
        super::plain_approval_card("write_file", r#"{"path":"a"}"#, "/w"),
        None
    );
}

// ============================================================================
// RC2-R2F7 — a worker that reaches any folder (a council member)
// ============================================================================

/// RC2-R2F7 (usability RC2 round 2, F3 MAJOR: the council's Gemma asked for
/// a read-only glob of a folder outside its session, then was told the path
/// "is outside my session root"). A worker built `with_any_folder(true)`
/// globs and reads a folder outside its own by full paths with no card (the
/// glob names the file by its full path, the read returns its text); a
/// write there still raises the card, and Deny leaves nothing written.
#[test]
fn rc2r2f7_any_folder_reads_outside_freely_and_a_write_still_asks() {
    let _g = lock();
    let outside = tempfile::tempdir().unwrap();
    let w1 = outside.path().join("w1");
    std::fs::create_dir_all(&w1).unwrap();
    std::fs::write(w1.join("alpha.txt"), "one\n").unwrap();
    let w1s = w1.display().to_string();
    let glob = format!(r#"{{"pattern":"{w1s}/*"}}"#);
    let read = format!(r#"{{"path":"{w1s}/alpha.txt"}}"#);
    let write = format!(r#"{{"path":"{w1s}/notes.txt","contents":"hi"}}"#);
    let mut rig = Rig::start_any_folder(vec![
        sse_tool_round(&[("g1", "fs.glob", glob.as_str())]),
        sse_tool_round(&[("r1", "fs.read", read.as_str())]),
        sse_tool_round(&[("w1", "fs.write", write.as_str())]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "what is in w1?".into(),
        system: None,
    });
    let seen = rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }));
    let finished: Vec<(Option<String>, Option<bool>)> = seen
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ToolFinished { name, ok, .. } => Some((name.clone(), *ok)),
            _ => None,
        })
        .collect();
    assert_eq!(
        finished,
        vec![
            (Some("fs.glob".into()), Some(true)),
            (Some("fs.read".into()), Some(true)),
        ],
        "both reads ran before the only card, the write's"
    );
    let sent = rig.script.sent();
    assert!(
        sent[1].contains(&format!("{w1s}/alpha.txt")),
        "the glob names the file by its full path: {}",
        sent[1]
    );
    assert!(sent[2].contains("one\\n"), "the read's text: {}", sent[2]);
    let id = seen
        .iter()
        .find_map(|e| match e {
            WorkEvent::ApprovalRequested { approval_id, .. } => approval_id.clone(),
            _ => None,
        })
        .expect("the write asks");
    rig.send(WorkCmd::Approval {
        id: Some(id),
        choice: "deny".into(),
    });
    let _ = rig.until(is_turn_complete);
    assert!(!w1.join("notes.txt").exists(), "Deny left nothing written");
    rig.finish();
}

/// RC2-R2G2 (RC2-R2F review P1): `fs.grep` was authorized by its folder's
/// name while it read every file beneath it, so a `.env` inside an ordinary
/// folder was read with no card; `fs.glob` named it. Now a search that
/// reaches a secret file asks first, its card naming the file: Deny shows
/// nothing (the model gets the refusal); Allow shows the name (glob) and
/// the text (grep). A search with no secret file still runs with no card.
#[test]
fn rc2r2g2_a_search_reaching_a_secret_file_asks_first() {
    let _g = lock();
    let outside = tempfile::tempdir().unwrap();
    let w1 = outside.path().join("w1");
    std::fs::create_dir_all(&w1).unwrap();
    std::fs::write(w1.join("notes.txt"), "key = plain\n").unwrap();
    std::fs::write(w1.join(".env"), "key = hunter2\n").unwrap();
    let plain = outside.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("a.txt"), "key = open\n").unwrap();
    let w1s = w1.display().to_string();
    let grep = format!(r#"{{"needle":"key","under":"{w1s}"}}"#);
    let glob = format!(r#"{{"pattern":"{w1s}/*"}}"#);
    let open = format!(r#"{{"needle":"key","under":"{}"}}"#, plain.display());
    let mut rig = Rig::start_any_folder(vec![
        sse_tool_round(&[("o1", "fs.grep", open.as_str())]),
        sse_tool_round(&[("g1", "fs.grep", grep.as_str())]),
        sse_tool_round(&[("l1", "fs.glob", glob.as_str())]),
        sse_tool_round(&[("g2", "fs.grep", grep.as_str())]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "what keys are there?".into(),
        system: None,
    });
    let card = |choice: &str| -> String {
        let seen =
            rig.until(|e| matches!(e, WorkEvent::ApprovalRequested { .. }) || is_turn_complete(e));
        assert!(
            !seen.iter().any(is_turn_complete),
            "the turn ended with no card for the secret file; sent: {:#?}",
            rig.script.sent()
        );
        let (id, text) = seen
            .iter()
            .find_map(|e| match e {
                WorkEvent::ApprovalRequested {
                    approval_id,
                    description,
                    ..
                } => Some((approval_id.clone(), description.clone().unwrap_or_default())),
                _ => None,
            })
            .unwrap();
        rig.send(WorkCmd::Approval {
            id,
            choice: choice.into(),
        });
        text
    };
    // The search of the plain folder ran with no card: the first card is
    // the secret one's.
    let first = card("deny");
    assert!(
        first.starts_with(&format!("Search the files in {w1s} for \"key\"")),
        "{first}"
    );
    assert!(
        first.contains(&format!("including the secret files {w1s}/.env")),
        "{first}"
    );
    let second = card("once");
    assert!(
        second.starts_with(&format!("List the files matching {w1s}/*")),
        "{second}"
    );
    assert!(second.contains(&format!("{w1s}/.env")), "{second}");
    let _ = card("once");
    let _ = rig.until(is_turn_complete);
    let sent = rig.script.sent();
    assert!(
        sent[1].contains("key = open"),
        "no card for it: {}",
        sent[1]
    );
    assert!(!sent[2].contains("hunter2"), "denied: {}", sent[2]);
    assert!(sent[2].contains("denied"), "{}", sent[2]);
    assert!(
        sent[3].contains(&format!("{w1s}/.env")),
        "allowed: the glob names it: {}",
        sent[3]
    );
    assert!(
        sent[4].contains("hunter2"),
        "allowed: the grep reads it: {}",
        sent[4]
    );
    rig.finish();
}

/// RC2-R2G3: the searches of [`rc2r2g3_a_search_at_a_mount_root_runs`],
/// at `vol` (a mounted volume's root holding `alpha.txt`): both run with
/// no card and name the file once, by its full path.
fn search_at_mount_root(vol: &std::path::Path) {
    let _g = lock();
    let vols = vol.display().to_string();
    let glob = format!(r#"{{"pattern":"{vols}/*"}}"#);
    let grep = format!(r#"{{"needle":"mounted","under":"{vols}"}}"#);
    let mut rig = Rig::start_any_folder(vec![
        sse_tool_round(&[("l1", "fs.glob", glob.as_str())]),
        sse_tool_round(&[("g1", "fs.grep", grep.as_str())]),
        sse_text_final("done"),
    ]);
    rig.send(WorkCmd::Prompt {
        text: "what is on the volume?".into(),
        system: None,
    });
    let seen = rig.until(is_turn_complete);
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, WorkEvent::ApprovalRequested { .. })),
        "reads at a volume's root ask nothing"
    );
    let finished: Vec<(Option<String>, Option<bool>)> = seen
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ToolFinished { name, ok, .. } => Some((name.clone(), *ok)),
            _ => None,
        })
        .collect();
    let sent = rig.script.sent();
    assert_eq!(
        finished,
        vec![
            (Some("fs.glob".into()), Some(true)),
            (Some("fs.grep".into()), Some(true)),
        ],
        "both searches ran at the volume's root: {sent:#?}"
    );
    let named = format!(r#"[\"{vols}/alpha.txt\"]"#);
    assert!(
        sent[1].contains(&named),
        "the glob names the file once, by its full path: {}",
        sent[1]
    );
    assert!(
        sent[2].contains(&format!(r#"\"rel\":\"{vols}/alpha.txt\""#)),
        "the grep names it the same way: {}",
        sent[2]
    );
    assert!(sent[2].contains("mounted text"), "{}", sent[2]);
    rig.finish();
}

/// RC2-R2G3 (RC2-R2F review P2): `fs.glob` and `fs.grep` ran from the
/// searched folder's PARENT, so a search at a mounted volume's root
/// (`/Projects` is its own mount) started on the volume above it and the
/// sandbox refused the crossing into `/Projects`. A volume is mounted for
/// the test (macOS: a disk image, no admin rights needed; Linux: a bind
/// mount in an unprivileged user + mount namespace, the test re-running
/// itself there); both searches at its root run. Fails, never skips, when
/// the volume cannot be mounted.
#[test]
fn rc2r2g3_a_search_at_a_mount_root_runs() {
    let base = tempfile::tempdir().unwrap();
    let vol = base.path().join("vol");
    std::fs::create_dir_all(&vol).unwrap();
    #[cfg(target_os = "macos")]
    {
        struct Detach(std::path::PathBuf);
        impl Drop for Detach {
            fn drop(&mut self) {
                let _ = std::process::Command::new("hdiutil")
                    .args(["detach", "-force", "-quiet"])
                    .arg(&self.0)
                    .status();
            }
        }
        let image = base.path().join("vol.dmg");
        let made = std::process::Command::new("hdiutil")
            .args([
                "create", "-quiet", "-size", "1m", "-fs", "HFS+", "-volname", "rc2r2g3",
            ])
            .arg(&image)
            .status()
            .expect("hdiutil must exist on macOS");
        assert!(made.success(), "hdiutil create failed");
        let attached = std::process::Command::new("hdiutil")
            .args([
                "attach",
                "-quiet",
                "-nobrowse",
                "-noverify",
                "-noautoopen",
                "-mountpoint",
            ])
            .arg(&vol)
            .arg(&image)
            .status()
            .expect("hdiutil must exist on macOS");
        assert!(attached.success(), "hdiutil attach failed");
        let _detach = Detach(vol.clone());
        std::fs::write(vol.join("alpha.txt"), "mounted text\n").unwrap();
        search_at_mount_root(&vol);
    }
    #[cfg(target_os = "linux")]
    {
        const CHILD: &str = "AWILI_RC2R2G3_MOUNT_CHILD";
        const NAME: &str = "tests::rc2r2g3_a_search_at_a_mount_root_runs";
        if let Ok(mounted) = std::env::var(CHILD) {
            search_at_mount_root(std::path::Path::new(&mounted));
            return;
        }
        let src = base.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("alpha.txt"), "mounted text\n").unwrap();
        let exe = std::env::current_exe().unwrap();
        let script = r#"mount --bind "$2" "$1" && exec "$3" --exact "$4" --nocapture"#;
        let out = std::process::Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "sh",
                "-c",
                script,
                "sh",
            ])
            .arg(&vol)
            .arg(&src)
            .arg(&exe)
            .arg(NAME)
            .env(CHILD, &vol)
            .output()
            .expect("unshare must be available on the Linux gate");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the mounted child must run and pass:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// RC2-R3 (usability RC2 round 3, Major 1): a Work turn's host says a
/// relative path means the Work folder (the seam then answers a local file
/// or shell tool given one at once, with no card); a council member's host,
/// which works in any folder, does not.
#[test]
fn rc2r3_a_work_turn_takes_relative_paths_in_its_folder() {
    use awili_seam::TurnHost;
    let (tx, _rx) = async_channel::unbounded();
    let (_ctx, crx) = std::sync::mpsc::channel();
    let cancel = AtomicBool::new(false);
    let host = |any_folder: bool| ChannelHost {
        events: &tx,
        cmds: &crx,
        cancel: &cancel,
        model: "m".into(),
        turn_id: "t".into(),
        workdir: "/Projects/w1".into(),
        call_names: Default::default(),
        call_args: Default::default(),
        may_change: Default::default(),
        shutdown_seen: false,
        disconnected: false,
        stop_after_deny: false,
        approval_mode: Default::default(),
        any_folder,
        channel_split: Default::default(),
        hooks: HostHooks::default(),
    };
    assert!(host(false).relative_paths_mean_the_folder(), "a Work turn");
    assert!(
        !host(true).relative_paths_mean_the_folder(),
        "a council member"
    );
}

/// RC2-R617 (usability RC2 round 5, Major 1: a cold first Gemma turn took
/// 54 s, almost all of it the server reading the rules, AGENTS.md and the
/// tools). A warm-up sent before the first question reads what the turn's
/// request begins with — the same system text (its recalled notes left out,
/// as the turn leaves them out) and the same tools — and shows nothing; the
/// question that follows is answered as before.
#[test]
fn rc2r617_a_warm_up_reads_what_the_first_turn_begins_with() {
    let _g = lock();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.txt"), "one\n").unwrap();
    let script = Script::new(vec![
        sse_text_final("o"),
        sse_text_final("alpha.txt says one"),
    ]);
    let cfg = AwiliEngineConfig::new(
        "http://r617-warm-test.invalid",
        "test-model",
        dir.path().join("b2.jsonl"),
        dir.path().join("grants.jsonl"),
    );
    let worker =
        AwiliEngineWorker::with_transport(cfg, authority(), Box::new(script.clone())).unwrap();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let (ev_tx, events) = async_channel::unbounded();
    let handle = awili_work::spawn_worker(
        Box::new(worker),
        WorkerContext {
            workdir: dir.path().display().to_string(),
            cmd_rx,
            cancel: Arc::new(AtomicBool::new(false)),
            events: ev_tx,
        },
    );
    let system = |fact: &str| {
        format!(
            "Standing directives (always apply, in priority order):\n1. Rule.\n\n\
             <past-context>\nRelevant context from your own memory (background only; \
             standing directives always take precedence):\n- {fact}\n</past-context>\n\n\
             How to answer in Awili:\n- Be brief."
        )
    };
    cmd_tx
        .send(WorkCmd::Warm {
            system: Some(system("nothing recalled yet")),
        })
        .unwrap();
    cmd_tx
        .send(WorkCmd::Prompt {
            text: "what is in alpha.txt?".into(),
            system: Some(system("alpha.txt is a test file")),
        })
        .unwrap();
    let mut tokens = String::new();
    loop {
        let ev = events.recv_blocking().expect("the worker answers");
        if let WorkEvent::Token(t) = &ev {
            tokens.push_str(t);
        }
        if is_turn_complete(&ev) {
            break;
        }
    }
    assert_eq!(tokens, "alpha.txt says one", "the warm-up shows nothing");
    let sent: Vec<serde_json::Value> = script
        .sent()
        .iter()
        .map(|b| serde_json::from_str(b).unwrap())
        .collect();
    assert_eq!(sent.len(), 2, "the warm-up, then the turn");
    let (warm, turn) = (&sent[0], &sent[1]);
    assert_eq!(warm["max_tokens"], 1);
    assert_eq!(warm["tools"], turn["tools"], "the same tools");
    let (wm, tm) = (
        warm["messages"].as_array().unwrap(),
        turn["messages"].as_array().unwrap(),
    );
    assert_eq!(wm[0], tm[0], "the same system text, notes left out");
    assert!(!wm[0]["content"].as_str().unwrap().contains("past-context"));
    assert_eq!(wm.last().unwrap()["content"], awili_seam::WARM_USER);
    let question = tm.last().unwrap()["content"].as_str().unwrap();
    assert!(question.ends_with("what is in alpha.txt?"), "{question}");
    drop(cmd_tx);
    handle.join().unwrap();
}

/// RC2-R6M2: a transport whose first request is held until `release` (the
/// server still reading it); every request body is kept.
struct HeldTransport {
    sent: Arc<Mutex<Vec<String>>>,
    entered: std::sync::mpsc::Sender<()>,
    release: Option<std::sync::mpsc::Receiver<()>>,
}

impl Transport for HeldTransport {
    fn stream(&mut self, request_body: &str) -> Result<String, SeamError> {
        self.sent.lock().unwrap().push(request_body.to_string());
        let _ = self.entered.send(());
        if let Some(release) = self.release.take() {
            let _ = release.recv();
        }
        Ok(sse_text_final("o"))
    }
}

/// RC2-R6M2 (RC2-R6 review P2 on 0fc8434b: a warm-up was skipped whenever
/// its server was busy, even with another text, and the app had already
/// counted it warm). A Work conversation's warm-up is still being read when
/// a council member's warm-up — another system text — arrives for the same
/// server: the council's waits (it is not sent alongside), then is read in
/// full with its own text; each worker reports its warm-up read
/// (`Warmed { ok: true }`).
#[test]
fn rc2r6m2_a_council_warm_up_waits_for_a_work_warm_up_of_its_server() {
    let _g = lock();
    let server = "http://r6m2-overlap-test.invalid";
    let dir = tempfile::tempdir().unwrap();
    let start = |name: &str,
                 transport: HeldTransport,
                 system: &str|
     -> (
        std::sync::mpsc::Sender<WorkCmd>,
        async_channel::Receiver<WorkEvent>,
        std::thread::JoinHandle<()>,
    ) {
        let cfg = AwiliEngineConfig::new(
            server,
            "test-model",
            dir.path().join(format!("{name}.jsonl")),
            dir.path().join(format!("{name}-grants.jsonl")),
        );
        let worker =
            AwiliEngineWorker::with_transport(cfg, authority(), Box::new(transport)).unwrap();
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (ev_tx, events) = async_channel::unbounded();
        cmd_tx
            .send(WorkCmd::Warm {
                system: Some(system.to_string()),
            })
            .unwrap();
        let handle = awili_work::spawn_worker(
            Box::new(worker),
            WorkerContext {
                workdir: dir.path().display().to_string(),
                cmd_rx,
                cancel: Arc::new(AtomicBool::new(false)),
                events: ev_tx,
            },
        );
        (cmd_tx, events, handle)
    };
    let warmed = |events: &async_channel::Receiver<WorkEvent>| loop {
        match events
            .recv_blocking()
            .expect("the worker reports its warm-up")
        {
            WorkEvent::Warmed { ok } => return ok,
            _ => continue,
        }
    };

    // The Work conversation's warm-up: the server is reading it.
    let work_sent: Arc<Mutex<Vec<String>>> = Default::default();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (work_tx, work_events, work_handle) = start(
        "work",
        HeldTransport {
            sent: work_sent.clone(),
            entered: entered_tx,
            release: Some(release_rx),
        },
        "Standing directives: WORK-RULES.",
    );
    entered_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the Work warm-up was sent");

    // The council member's warm-up arrives meanwhile.
    let council_sent: Arc<Mutex<Vec<String>>> = Default::default();
    let (c_entered_tx, c_entered_rx) = std::sync::mpsc::channel();
    let (council_tx, council_events, council_handle) = start(
        "council",
        HeldTransport {
            sent: council_sent.clone(),
            entered: c_entered_tx,
            release: None,
        },
        "You are one member of a council: COUNCIL-BRIEF.",
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while crate::warm_gate::queued(server) != 1 {
        if let Ok(WorkEvent::Warmed { ok }) = council_events.try_recv() {
            panic!("the council warm-up ended (ok: {ok}) while the server was busy");
        }
        assert!(
            Instant::now() < deadline,
            "the council warm-up never queued"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        council_sent.lock().unwrap().is_empty(),
        "not sent while the server reads the Work warm-up"
    );

    release_tx.send(()).unwrap();
    assert!(warmed(&work_events), "the Work warm-up was read");
    assert!(warmed(&council_events), "the council warm-up was read");
    c_entered_rx.recv().unwrap();
    let body = |sent: &Arc<Mutex<Vec<String>>>| -> serde_json::Value {
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "one warm-up request");
        serde_json::from_str(&sent[0]).unwrap()
    };
    let (w, c) = (body(&work_sent), body(&council_sent));
    let first = |v: &serde_json::Value| v["messages"][0]["content"].as_str().unwrap().to_string();
    assert!(first(&w).contains("WORK-RULES"), "{}", first(&w));
    assert!(first(&c).contains("COUNCIL-BRIEF"), "{}", first(&c));
    assert_eq!(c["max_tokens"], 1);
    assert_eq!(crate::warm_gate::queued(server), 0);
    drop((work_tx, council_tx));
    work_handle.join().unwrap();
    council_handle.join().unwrap();
}
