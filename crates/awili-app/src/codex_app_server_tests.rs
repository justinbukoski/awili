//! RC2-CXS proofs: a Codex turn over a scripted `codex app-server` (the
//! shapes are the ones codex-cli 0.160.1 printed live on 2026-10-07, see
//! `docs/daily-driver/specs/RC2-CXS1.md`), through the real
//! [`crate::code_provider::CodeProvider`] that Work, Chat and Code share.

use super::*;
use crate::code_provider::CodeProvider;
use awili_code::{Autonomy, CodeEngine};
use awili_providers::{ChatMessage, ChatRequest};
use std::path::{Path, PathBuf};

fn req(text: &str) -> ChatRequest {
    ChatRequest {
        model: "x".into(),
        system: None,
        messages: vec![ChatMessage::user(text)],
    }
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn fake_server(dir: &Path, thread_error: Option<&str>, after_turn: &str) -> PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A scripted app server: records its argv and every line it reads, answers
/// the handshake (`thread/start` or `thread/resume` → `thr-1`, or the
/// `thread_error` when one is given), then runs `after_turn` (shell).
fn fake_server(dir: &Path, thread_error: Option<&str>, after_turn: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("codex-app-server");
    let log = dir.join("stdin");
    let thread_answer = match thread_error {
        Some(e) => format!(r#"{{"id":2,"error":{{"code":-32600,"message":"{e}"}}}}"#),
        None => r#"{"id":2,"result":{"thread":{"id":"thr-1"}}}"#.to_string(),
    };
    let script = format!(
        "#!/bin/sh\n\
         echo $$ > {pid}\n\
         : > {argv}\n\
         for a in \"$@\"; do printf '%s\\n' \"$a\" >> {argv}; done\n\
         read -r l; printf '%s\\n' \"$l\" >> {log}\n\
         printf '%s\\n' '{{\"id\":1,\"result\":{{\"userAgent\":\"fake\"}}}}'\n\
         read -r l; printf '%s\\n' \"$l\" >> {log}\n\
         read -r l; printf '%s\\n' \"$l\" >> {log}\n\
         printf '%s\\n' '{thread_answer}'\n\
         read -r l; printf '%s\\n' \"$l\" >> {log}\n\
         printf '%s\\n' '{{\"id\":3,\"result\":{{\"turn\":{{\"id\":\"turn-1\",\"status\":\"inProgress\"}}}}}}'\n\
         {after_turn}",
        pid = dir.join("pid").display(),
        argv = dir.join("argv").display(),
        log = log.display(),
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// One line the scripted server prints.
fn say(line: &str) -> String {
    format!("printf '%s\\n' '{line}'\n")
}

fn delta(thread: &str, item: &str, text: &str) -> String {
    say(&format!(
        r#"{{"method":"item/agentMessage/delta","params":{{"threadId":"{thread}","turnId":"turn-1","itemId":"{item}","delta":"{text}"}}}}"#
    ))
}

/// RC2-R3L: the ready-server tests share one pool (at most `WARM_CAP`
/// servers) and one switch in this process. Run at the same time, one
/// test's server pushed another's out of the pool ("never made ready" on
/// hosta); each holds this for its whole run.
static POOL_TESTS: Mutex<()> = Mutex::new(());

fn pool_test() -> std::sync::MutexGuard<'static, ()> {
    crate::exec_fixture::lock(&POOL_TESTS)
}

fn provider(
    bin: &Path,
    dir: &Path,
    autonomy: Autonomy,
    resume: Option<&str>,
    slot: crate::code_provider::SessionSlot,
) -> CodeProvider {
    CodeProvider::new(
        bin.display().to_string(),
        CodeEngine::Codex,
        autonomy,
        dir.display().to_string(),
        resume.map(str::to_string),
        None,
        slot,
    )
}

fn drain(rx: async_channel::Receiver<ProviderEvent>) -> Vec<ProviderEvent> {
    let mut events = Vec::new();
    while let Ok(event) = rx.recv_blocking() {
        let done = event == ProviderEvent::Done;
        events.push(event);
        if done {
            break;
        }
    }
    events
}

fn tokens(events: &[ProviderEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Token(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Repeated polling during an approval wait or warm-cache expiry must not
/// reap the Unix server before group cleanup can use its reserved identity.
#[cfg(unix)]
#[test]
fn server_exit_poll_preserves_unreaped_leader_identity() {
    let mut child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !server_has_exited(&mut child) {
        assert!(Instant::now() < deadline, "fixture server did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(server_has_exited(&mut child));
    let observation = crate::proc_compat::child_exit_observed(child.id());
    // Always reap the fixture before asserting, including the failure path.
    assert!(child.wait().unwrap().success());
    assert!(
        matches!(observation, Ok(true)),
        "poll reaped leader: {observation:?}"
    );
}

/// A completed CLI can exit while a background MCP-like child retains stderr.
/// Completion must end that owned child and deliver Done without waiting for
/// its sleep to finish, even while the caller keeps its Stop sender alive.
#[cfg(unix)]
#[test]
fn completed_server_reaps_background_stderr_holder() {
    let dir = tempfile::tempdir().unwrap();
    let after = format!(
        "sleep 30 >/dev/null &\nprintf '%s\\n' \"$!\" > {}\n{}exit 0\n",
        dir.path().join("background-pid").display(),
        say(
            r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}"#
        ),
    );
    let bin = fake_server(dir.path(), None, &after);
    let slot = Arc::new(Mutex::new(None));
    let p = provider(&bin, dir.path(), Autonomy::Autonomous, None, slot);
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let rx = p.run(req("finish"), cancel_rx);
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = result_tx.send(drain(rx));
    });
    let result = result_rx.recv_timeout(Duration::from_secs(5));
    if result.is_err() {
        // Bound failure cleanup too, so a regression does not leak a fixture.
        if let Ok(pid) = std::fs::read_to_string(dir.path().join("background-pid")) {
            if let Ok(pid) = pid.trim().parse::<u32>() {
                crate::proc_compat::kill_pid_reporting(pid);
            }
        }
    }
    let events = result.expect("background stderr holder blocked completed native turn");
    assert_eq!(events.last(), Some(&ProviderEvent::Done), "{events:?}");
    assert!(
        !events.iter().any(|e| matches!(e, ProviderEvent::Error(_))),
        "{events:?}"
    );
    let pid = std::fs::read_to_string(dir.path().join("background-pid")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let status = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid.trim()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&status.stdout);
        // An orphan killed by the group signal may await init's reap. A
        // zombie is stopped and no longer holds a pipe or executes tools.
        if state.trim().is_empty() || state.trim().starts_with('Z') {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "completed turn left background child running"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// RC2-CXS (R3-20 Codex part): a Codex reply streams — each piece of text
/// arrives as Codex writes it, the complete copy is never shown twice, two
/// messages keep their blank line, a subagent's text stays out, a stray
/// approval request is declined, the footer gets the turn's tokens, and the
/// turn ends with exactly one Done once the server has gone.
#[test]
fn rc2_cxs_codex_reply_streams_over_the_app_server() {
    let dir = tempfile::tempdir().unwrap();
    let after = [
        delta("thr-1", "msg-1", "Once upon"),
        delta("thr-sub", "sub-1", "SUBAGENT"),
        delta("thr-1", "msg-1", " a time"),
        say(r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"agentMessage","id":"msg-1","text":"Once upon a time"}}}"#),
        say(r#"{"id":"ask-1","method":"item/commandExecution/requestApproval","params":{"threadId":"thr-1","turnId":"turn-1","itemId":"cmd-1"}}"#),
        format!("read -r l; printf '%s\\n' \"$l\" >> {}\n", dir.path().join("answers").display()),
        say(r#"{"method":"item/completed","params":{"threadId":"thr-1","turnId":"turn-1","item":{"type":"commandExecution","id":"cmd-1","command":"ls","commandActions":[],"cwd":"/","status":"completed","exitCode":0}}}"#),
        delta("thr-1", "msg-2", "The end."),
        say(r#"{"method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","turnId":"turn-1","tokenUsage":{"total":{"totalTokens":120,"inputTokens":100,"cachedInputTokens":0,"outputTokens":20,"reasoningOutputTokens":0},"last":{"totalTokens":12,"inputTokens":10,"cachedInputTokens":0,"outputTokens":2,"reasoningOutputTokens":0}}}}"#),
        say(r#"{"method":"turn/completed","params":{"threadId":"thr-sub","turn":{"id":"turn-sub","items":[],"status":"completed"}}}"#),
        say(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}"#),
        // Stays until Awili closes its stdin.
        "read -r l\nexit 0\n".to_string(),
    ]
    .concat();
    let bin = fake_server(dir.path(), None, &after);
    let slot: crate::code_provider::SessionSlot = Arc::new(Mutex::new(None));
    let p = provider(
        &bin,
        dir.path(),
        Autonomy::Autonomous,
        None,
        Arc::clone(&slot),
    );
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let events = drain(p.run(req("tell me a story"), cancel_rx));

    assert_eq!(
        tokens(&events),
        ["Once upon", " a time", "\n\n", "The end."],
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, ProviderEvent::Error(_))),
        "{events:?}"
    );
    assert!(
        events.contains(&ProviderEvent::Usage(awili_providers::Usage {
            cached_input_tokens: Some(0),
            input_tokens: 100,
            output_tokens: 20,
        }))
    );
    assert_eq!(
        events.iter().filter(|e| **e == ProviderEvent::Done).count(),
        1
    );
    assert_eq!(events.last(), Some(&ProviderEvent::Done));
    assert!(events.contains(&ProviderEvent::Activity("\u{2713} $ ls".into())));
    assert_eq!(slot.lock().unwrap().as_deref(), Some("thr-1"));

    let argv = std::fs::read_to_string(dir.path().join("argv")).unwrap();
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(argv[0], "app-server", "{argv:?}");
    assert!(
        argv.windows(2)
            .any(|w| w == ["-c", "sandbox_mode=\"danger-full-access\""]),
        "{argv:?}"
    );
    assert!(
        argv.windows(2)
            .any(|w| w == ["-c", "approval_policy=\"on-request\""]),
        "{argv:?}"
    );
    let sent = read_lines(&dir.path().join("stdin"));
    let methods: Vec<&str> = sent.iter().filter_map(|v| v["method"].as_str()).collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "thread/start", "turn/start"]
    );
    assert_eq!(sent[2]["params"]["sandbox"], "danger-full-access");
    assert_eq!(sent[2]["params"]["approvalPolicy"], "on-request");
    assert_eq!(sent[2]["params"]["approvalsReviewer"], "auto_review");
    assert_eq!(sent[2]["params"]["cwd"], dir.path().display().to_string());
    assert_eq!(sent[3]["params"]["threadId"], "thr-1");
    assert_eq!(sent[3]["params"]["input"][0]["text"], "tell me a story");
    let answers = read_lines(&dir.path().join("answers"));
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0]["id"], "ask-1");
    assert_eq!(answers[0]["result"]["decision"], "decline");
}

/// RC2-CXS: a conversation that has a Codex session continues it
/// (`thread/resume`), and Ask's planning step runs read-only — on the argv
/// and on the request. A session Codex can no longer find is said plainly.
#[test]
fn rc2_cxs_codex_resume_and_ask_keep_their_posture() {
    let dir = tempfile::tempdir().unwrap();
    let after = [
        delta("thr-1", "msg-1", "Plan: nothing to change."),
        say(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}"#),
        "read -r l\n".to_string(),
    ]
    .concat();
    let bin = fake_server(dir.path(), None, &after);
    let p = provider(
        &bin,
        dir.path(),
        Autonomy::AskFirst,
        Some("thr-1"),
        Arc::default(),
    );
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let events = drain(p.run(req("look around"), cancel_rx));
    assert_eq!(tokens(&events), ["Plan: nothing to change."], "{events:?}");
    let argv = std::fs::read_to_string(dir.path().join("argv")).unwrap();
    assert!(argv.contains("sandbox_mode=\"read-only\""), "{argv}");
    assert!(!argv.contains("danger-full-access"), "{argv}");
    let sent = read_lines(&dir.path().join("stdin"));
    assert_eq!(sent[2]["method"], "thread/resume");
    assert_eq!(sent[2]["params"]["threadId"], "thr-1");
    assert_eq!(sent[2]["params"]["sandbox"], "read-only");
    assert_eq!(sent[2]["params"]["approvalPolicy"], "never");

    let gone = tempfile::tempdir().unwrap();
    let bin = fake_server(
        gone.path(),
        Some("no rollout found for thread id thr-old"),
        "",
    );
    let p = provider(
        &bin,
        gone.path(),
        Autonomy::Autonomous,
        Some("thr-old"),
        Arc::default(),
    );
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let events = drain(p.run(req("go on"), cancel_rx));
    let errors: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            ProviderEvent::Error(m) => Some(m),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1, "{events:?}");
    assert!(
        errors[0].starts_with("Codex can no longer find this conversation"),
        "{errors:?}"
    );
    assert_eq!(events.last(), Some(&ProviderEvent::Done));
}

/// RC2-CXS: Stop while Codex is writing ends the server at once; the text
/// already shown stays, and the turn ends with one Done.
#[test]
fn rc2_cxs_codex_stop_mid_stream_ends_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let after = [
        delta("thr-1", "msg-1", "Once upon"),
        "exec sleep 30\n".to_string(),
    ]
    .concat();
    let bin = fake_server(dir.path(), None, &after);
    let p = provider(&bin, dir.path(), Autonomy::Autonomous, None, Arc::default());
    let (cancel, cancel_rx) = std::sync::mpsc::channel();
    let rx = p.run(req("a long story"), cancel_rx);
    let first = rx.recv_blocking().unwrap();
    assert_eq!(first, ProviderEvent::Token("Once upon".into()));
    let pid: i32 = std::fs::read_to_string(dir.path().join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = |pid: i32| {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    assert!(alive(pid), "the server is still writing");
    let start = Instant::now();
    cancel.send(()).unwrap();
    let rest = drain(rx);
    assert!(start.elapsed() < Duration::from_secs(5), "{rest:?}");
    assert_eq!(rest.last(), Some(&ProviderEvent::Done));
    assert!(!alive(pid), "Stop ends the server");
}

#[cfg(unix)]
/// RC2-CXS: a Codex with no app server (it exits at once on the
/// subcommand) runs the turn on `codex exec --json`, decided at the start
/// with nothing shown twice, and its later turns go straight to exec.
#[test]
fn rc2_cxs_codex_without_app_server_runs_on_exec() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("codex-old");
    let starts = dir.path().join("starts");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> {starts}\n\
             if [ \"$1\" = app-server ]; then echo \"error: unrecognized subcommand 'app-server'\" >&2; exit 2; fi\n\
             printf '%s\\n' '{{\"type\":\"thread.started\",\"thread_id\":\"t-exec\"}}'\n\
             printf '%s\\n' '{{\"type\":\"item.completed\",\"item\":{{\"id\":\"a\",\"type\":\"agent_message\",\"text\":\"from exec\"}}}}'\n\
             printf '%s\\n' '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":3,\"output_tokens\":4}}}}'\n",
            starts = starts.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!exec_fallback_for(&bin.display().to_string()));
    for _ in 0..2 {
        let on_exec = Arc::new(AtomicBool::new(false));
        let p = provider(&bin, dir.path(), Autonomy::AskFirst, None, Arc::default())
            .observe_exec(Arc::clone(&on_exec));
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let events = drain(p.run(req("hello"), cancel_rx));
        assert_eq!(tokens(&events), ["from exec"], "{events:?}");
        assert!(
            !events.iter().any(|e| matches!(e, ProviderEvent::Error(_))),
            "{events:?}"
        );
        assert_eq!(events.last(), Some(&ProviderEvent::Done));
        // RC2-CXSF: each run that ran on exec is told so.
        assert!(on_exec.load(Ordering::Acquire));
    }
    let starts = std::fs::read_to_string(&starts).unwrap();
    assert_eq!(
        starts.lines().collect::<Vec<_>>(),
        ["app-server", "exec", "exec"]
    );
    assert!(exec_fallback_for(&bin.display().to_string()));
}

#[cfg(unix)]
/// RC2-CXS: Stop while a Codex server has not answered yet ends the turn
/// as stopped at once — no wait for the handshake to time out, and no
/// second start on `codex exec --json`.
#[test]
fn rc2_cxs_codex_stop_before_the_server_answers() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("codex-silent");
    let starts = dir.path().join("starts");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> {}\nexec sleep 30\n",
            starts.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let observed = Arc::new(AtomicBool::new(false));
    let p = provider(&bin, dir.path(), Autonomy::Autonomous, None, Arc::default())
        .observe_termination(Arc::clone(&observed));
    let (cancel, cancel_rx) = std::sync::mpsc::channel();
    let rx = p.run(req("hello"), cancel_rx);
    // The server has started (it never answers).
    let up = Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    while !starts.exists() {
        assert!(Instant::now() < up, "the server started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let start = Instant::now();
    cancel.send(()).unwrap();
    let events = drain(rx);
    assert!(start.elapsed() < Duration::from_secs(3), "{events:?}");
    assert!(
        matches!(
            events.as_slice(),
            [ProviderEvent::Error(_), ProviderEvent::Done]
        ),
        "{events:?}"
    );
    assert!(observed.load(Ordering::Acquire));
    assert_eq!(std::fs::read_to_string(&starts).unwrap(), "app-server\n");
    assert!(!exec_fallback_for(&bin.display().to_string()));
}

/// RC2-CXSF (RC2-CXS review P1): a server that closes its output and exits
/// cleanly before Codex said the turn was over leaves an unfinished reply.
/// It ends with one plain-words error, never as a finished reply; text
/// already shown stays as it was.
#[test]
fn rc2cxsf_codex_server_gone_before_the_turn_ends_is_an_error() {
    for (after, shown) in [
        (
            [delta("thr-1", "msg-1", "Once upon"), "exit 0\n".to_string()].concat(),
            vec!["Once upon".to_string()],
        ),
        ("exit 0\n".to_string(), Vec::new()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_server(dir.path(), None, &after);
        let p = provider(&bin, dir.path(), Autonomy::Autonomous, None, Arc::default());
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let events = drain(p.run(req("tell me a story"), cancel_rx));
        assert_eq!(tokens(&events), shown, "{events:?}");
        let errors: Vec<&String> = events
            .iter()
            .filter_map(|e| match e {
                ProviderEvent::Error(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(errors, [ENDED_EARLY], "{events:?}");
        assert_eq!(
            events.iter().filter(|e| **e == ProviderEvent::Done).count(),
            1
        );
        assert_eq!(events.last(), Some(&ProviderEvent::Done));
    }
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn counting_server(dir: &Path) -> PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// RC2-R2F10: a scripted app server that notes each start in `starts`,
/// opens thread `thr-w`, then answers the one turn it is asked ("Hi.").
fn counting_server(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("codex-warm");
    let script = format!(
        "#!/bin/sh\n\
         echo start >> {starts}\n\
         read -r l; printf '%s\\n' \"$l\" >> {log}\n\
         printf '%s\\n' '{{\"id\":1,\"result\":{{\"userAgent\":\"fake\"}}}}'\n\
         read -r l; printf '%s\\n' \"$l\" >> {log}\n\
         read -r l; printf '%s\\n' \"$l\" >> {log}\n\
         printf '%s\\n' '{{\"id\":2,\"result\":{{\"thread\":{{\"id\":\"thr-w\"}}}}}}'\n\
         read -r l; printf '%s\\n' \"$l\" >> {log}\n\
         printf '%s\\n' '{{\"id\":3,\"result\":{{\"turn\":{{\"id\":\"turn-1\",\"status\":\"inProgress\"}}}}}}'\n\
         {delta}\
         {done}\
         read -r l\nexit 0\n",
        starts = dir.join("starts").display(),
        log = dir.join("stdin").display(),
        delta = delta("thr-w", "m1", "Hi."),
        done = say(r#"{"method":"turn/completed","params":{"threadId":"thr-w","turn":{"id":"turn-1","items":[],"status":"completed"}}}"#),
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// RC2-R2F10 (usability RC2 round 2, F7: Codex wrote nothing for 19-33 s
/// on a council; measured, ~1.7 s of every start is Awili spawning Codex
/// and Codex starting its MCP servers). A server made ready ahead — spawned,
/// its thread open — takes the next question at once: no second spawn, the
/// question goes out on the open thread, the reply streams, and the ready
/// server is used up.
#[test]
fn rc2r2f10_a_ready_server_takes_the_next_question() {
    let _pool = pool_test();
    PREWARM.store(true, Ordering::Release);
    let dir = tempfile::tempdir().unwrap();
    let bin = counting_server(dir.path());
    let slot: crate::code_provider::SessionSlot = Arc::new(Mutex::new(None));
    let p = provider(&bin, dir.path(), Autonomy::AskFirst, None, slot.clone())
        .for_council("council-r2f10");
    let starts = || {
        std::fs::read_to_string(dir.path().join("starts"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    // RC2-R2G4: counted for this test's own binary (another test's ready
    // server may come and go meanwhile).
    let bin_s = bin.display().to_string();
    let before = warm_count_of(&bin_s);
    p.prewarm(req(""), "council-r2f10");
    let deadline = Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    while warm_count_of(&bin_s) == before {
        assert!(Instant::now() < deadline, "the server was never made ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(starts(), 1);
    let sent = read_lines(&dir.path().join("stdin"));
    let methods: Vec<&str> = sent.iter().filter_map(|v| v["method"].as_str()).collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "thread/start"],
        "no question yet"
    );
    let (_stop, cancel_rx) = std::sync::mpsc::channel::<()>();
    let events = drain(p.run(req("Say hi"), cancel_rx));
    assert_eq!(tokens(&events), ["Hi."]);
    assert_eq!(events.last(), Some(&ProviderEvent::Done));
    assert_eq!(
        starts(),
        1,
        "the ready server answered; nothing new was spawned"
    );
    let sent = read_lines(&dir.path().join("stdin"));
    let turn = sent.iter().find(|v| v["method"] == "turn/start").unwrap();
    assert_eq!(turn["params"]["threadId"], "thr-w");
    assert!(turn["params"]["input"][0]["text"]
        .as_str()
        .is_some_and(|t| t.ends_with("Say hi")));
    assert_eq!(slot.lock().unwrap().as_deref(), Some("thr-w"));
    assert_eq!(warm_count_of(&bin_s), before, "used up");
}

/// Whether process `pid` is still there.
fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// RC2-R2G4 (RC2-R2F review P2: a ready server was ended only by a later
/// prewarm or turn, so one left idle ran on for good). A ready server is
/// ended when its time is up with nothing more asked: here its wait is
/// 300 ms, and with no further request it leaves the ready list and its
/// process is gone.
#[test]
fn rc2r2g4_a_ready_server_ends_when_its_time_is_up() {
    let _pool = pool_test();
    PREWARM.store(true, Ordering::Release);
    let dir = tempfile::tempdir().unwrap();
    let bin = fake_server(dir.path(), None, "read -r l\nexit 0\n");
    let bin_s = bin.display().to_string();
    TEST_TTL
        .lock()
        .unwrap()
        .push((bin_s.clone(), Duration::from_millis(300)));
    let slot: crate::code_provider::SessionSlot = Arc::new(Mutex::new(None));
    let p = provider(&bin, dir.path(), Autonomy::AskFirst, None, slot);
    p.prewarm(req(""), "council-r2g4");
    let deadline = Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    while warm_count_of(&bin_s) == 0 {
        assert!(Instant::now() < deadline, "the server was never made ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = std::fs::read_to_string(dir.path().join("pid"))
        .unwrap()
        .trim()
        .to_string();
    assert!(alive(&pid), "ready and waiting");
    // Nothing more is asked: the ready server ends on its own.
    let deadline = Instant::now() + Duration::from_secs(5);
    while warm_count_of(&bin_s) > 0 || alive(&pid) {
        assert!(
            Instant::now() < deadline,
            "a ready server outlived its time with no further request"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// RC2-R2G5 (RC2-R2F review P2: a council's ready servers outlived the
/// council). Each ready server belongs to the council it was made for;
/// closing that council ends its servers (off the list at once, the
/// process gone soon after) and leaves another council's ready.
#[test]
fn rc2r2g5_closing_a_council_ends_its_ready_servers() {
    let _pool = pool_test();
    PREWARM.store(true, Ordering::Release);
    let ready = |owner: &str| {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_server(dir.path(), None, "read -r l\nexit 0\n");
        let bin_s = bin.display().to_string();
        let slot: crate::code_provider::SessionSlot = Arc::new(Mutex::new(None));
        provider(&bin, dir.path(), Autonomy::AskFirst, None, slot).prewarm(req(""), owner);
        let deadline = Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
        while warm_count_of(&bin_s) == 0 {
            assert!(Instant::now() < deadline, "{owner}: never made ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = std::fs::read_to_string(dir.path().join("pid"))
            .unwrap()
            .trim()
            .to_string();
        (dir, bin_s, pid)
    };
    let (_a_dir, a_bin, a_pid) = ready("council-r2g5-a");
    let (_b_dir, b_bin, b_pid) = ready("council-r2g5-b");
    close_warm_for("council-r2g5-a");
    assert_eq!(warm_count_of(&a_bin), 0, "off the list at once");
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(&a_pid) {
        assert!(
            Instant::now() < deadline,
            "the closed council's server lives on"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(warm_count_of(&b_bin), 1, "the other council's stays ready");
    assert!(alive(&b_pid));
    close_warm_for("council-r2g5-b");
}

/// RC2-R2H (RC2-R2F review P2 on d5cbd4ca): a council closed (archived or
/// deleted) while its server was still starting keeps nothing ready, a
/// prewarm asked for after the close starts nothing, and once the council
/// is reopened (unarchived) it is made ready again.
#[test]
fn rc2r2h_a_closed_council_keeps_no_ready_server() {
    let _pool = pool_test();
    PREWARM.store(true, Ordering::Release);
    // A server that answers the thread request one second late, so the
    // close lands while it is starting.
    let slow = |dir: &Path| {
        let bin = fake_server(dir, None, "read -r l\nexit 0\n");
        let script = std::fs::read_to_string(&bin).unwrap();
        let script = script.replacen(
            "printf '%s\\n' '{\"id\":2",
            "sleep 1\nprintf '%s\\n' '{\"id\":2",
            1,
        );
        std::fs::write(&bin, script).unwrap();
        bin
    };
    let owner = "council-r2h";
    // 1. Closed while starting.
    let dir = tempfile::tempdir().unwrap();
    let bin = slow(dir.path());
    let bin_s = bin.display().to_string();
    let (b, d) = (bin.clone(), dir.path().to_path_buf());
    let starting = std::thread::spawn(move || {
        let slot: crate::code_provider::SessionSlot = Arc::new(Mutex::new(None));
        provider(&b, &d, Autonomy::AskFirst, None, slot).prewarm(req(""), "council-r2h");
    });
    let deadline = Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    while !dir.path().join("pid").exists() {
        assert!(Instant::now() < deadline, "the server never started");
        std::thread::sleep(Duration::from_millis(10));
    }
    close_warm_for(owner);
    starting.join().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        warm_count_of(&bin_s),
        0,
        "closed while starting: nothing kept"
    );
    // 2. Asked for after the close: nothing starts.
    let dir2 = tempfile::tempdir().unwrap();
    let bin2 = fake_server(dir2.path(), None, "read -r l\nexit 0\n");
    let slot: crate::code_provider::SessionSlot = Arc::new(Mutex::new(None));
    provider(&bin2, dir2.path(), Autonomy::AskFirst, None, slot.clone()).prewarm(req(""), owner);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !dir2.path().join("pid").exists(),
        "a closed council starts nothing"
    );
    // 3. Reopened: made ready again.
    reopen_warm_for(owner);
    provider(&bin2, dir2.path(), Autonomy::AskFirst, None, slot).prewarm(req(""), owner);
    let bin2_s = bin2.display().to_string();
    let deadline = Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
    while warm_count_of(&bin2_s) == 0 {
        assert!(Instant::now() < deadline, "reopened: never made ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    close_warm_for(owner);
}

/// RC2-R2I (RC2-R2F review P2 on d65da1fc: two councils with the same
/// launch shared one ready server, so archiving one took the other's).
/// Two councils with identical launches each get their own ready server;
/// closing one leaves the other's; the other council's next question goes
/// to its own server (no new start); a turn outside any council takes none.
#[test]
fn rc2r2i_councils_with_the_same_launch_each_keep_their_own_server() {
    let _pool = pool_test();
    PREWARM.store(true, Ordering::Release);
    let dir = tempfile::tempdir().unwrap();
    let bin = counting_server(dir.path());
    let bin_s = bin.display().to_string();
    let starts = || {
        std::fs::read_to_string(dir.path().join("starts"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    let slot: crate::code_provider::SessionSlot = Arc::new(Mutex::new(None));
    let member = |council: &str| {
        provider(&bin, dir.path(), Autonomy::AskFirst, None, slot.clone()).for_council(council)
    };
    let wait_for = |n: usize, what: &str| {
        let deadline = Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
        while warm_count_of(&bin_s) != n {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    member("council-r2i-a").prewarm(req(""), "council-r2i-a");
    wait_for(1, "council A: never made ready");
    member("council-r2i-b").prewarm(req(""), "council-r2i-b");
    wait_for(2, "council B got no ready server of its own");
    assert_eq!(starts(), 2);
    // A turn outside any council takes neither.
    let plain = provider(&bin, dir.path(), Autonomy::AskFirst, None, slot.clone());
    let (_stop, cancel_rx) = std::sync::mpsc::channel::<()>();
    assert_eq!(tokens(&drain(plain.run(req("Say hi"), cancel_rx))), ["Hi."]);
    assert_eq!(starts(), 3, "a fresh server, not a council's");
    assert_eq!(warm_count_of(&bin_s), 2);
    close_warm_for("council-r2i-a");
    assert_eq!(warm_count_of(&bin_s), 1, "council B's stays ready");
    let (_stop, cancel_rx) = std::sync::mpsc::channel::<()>();
    let events = drain(member("council-r2i-b").run(req("Say hi"), cancel_rx));
    assert_eq!(tokens(&events), ["Hi."]);
    assert_eq!(starts(), 3, "council B's own ready server answered");
    assert_eq!(warm_count_of(&bin_s), 0, "used up");
}

#[test]
fn resumed_image_turn_emits_only_current_usage_and_real_image_input() {
    let dir = tempfile::tempdir().unwrap();
    let usage = r#"{"method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","turnId":"turn-1","tokenUsage":{"total":{"inputTokens":59621,"outputTokens":183},"last":{"inputTokens":30680,"outputTokens":87}}}}"#;
    let after = [say(usage), say(usage), say(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}"#)].concat();
    let bin = fake_server(dir.path(), None, &after);
    let (cancel, cancel_rx) = std::sync::mpsc::channel();
    let p = provider(
        &bin,
        dir.path(),
        Autonomy::Autonomous,
        Some("thr-1"),
        crate::code_provider::fresh_session_slot(Some("thr-1")),
    );
    let events = drain(p.run(
        req("![s](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)\nDescribe"),
        cancel_rx,
    ));
    drop(cancel);
    assert!(
        events.contains(&ProviderEvent::Usage(awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: 30680,
            output_tokens: 87
        })),
        "{events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ProviderEvent::Usage(_)))
            .count(),
        1
    );
    let wire = read_lines(&dir.path().join("stdin"));
    let request = wire.iter().find(|r| r["method"] == "turn/start").unwrap();
    assert_eq!(request["params"]["input"][1]["type"], "image");
    assert!(!request["params"]["input"][0]["text"]
        .as_str()
        .unwrap()
        .contains("base64"));
}

#[cfg(unix)]
#[test]
fn native_host_approvals_deliver_exact_replies_and_reuse_only_session_grants() {
    use awili_providers::ApprovalChoice;
    let dir = tempfile::tempdir().unwrap();
    let approval = |id| {
        say(&serde_json::json!({"id":id,"method":"mcpServer/elicitation/request","params":{"threadId":"thr-1","turnId":"turn-1","serverName":"desktop","mode":"form","message":"Capture the whole screen?","requestedSchema":{"type":"object","properties":{}},"_meta":{"codex_approval_kind":"mcp_tool_call","tool_name":"screenshot","tool_params":{"allScreens":true}}}}).to_string())
    };
    let after = format!("{}read -r l; printf '%s\\n' \"$l\" >> {}\n{}read -r l; printf '%s\\n' \"$l\" >> {}\n{}exit 0\n", approval(77), dir.path().join("stdin").display(), approval(78), dir.path().join("stdin").display(), say(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}"#));
    let bin = fake_server(dir.path(), None, &after);
    let p = provider(
        &bin,
        dir.path(),
        Autonomy::Autonomous,
        None,
        Arc::new(Mutex::new(None)),
    )
    .with_approval_session(Default::default());
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let rx = p.run(req("capture"), cancel_rx);
    let mut approvals = 0;
    while let Ok(event) = rx.recv_blocking() {
        match event {
            ProviderEvent::HostApproval(prompt) => {
                approvals += 1;
                assert!(prompt.detail.contains("allScreens"));
                assert!(prompt.answer(ApprovalChoice::Session));
            }
            ProviderEvent::Error(error) => panic!("{error}"),
            ProviderEvent::Done => break,
            _ => {}
        }
    }
    assert_eq!(approvals, 1);
    let sent = read_lines(&dir.path().join("stdin"));
    assert_eq!(sent[2]["params"]["approvalsReviewer"], "user");
    assert_eq!(sent[3]["params"]["approvalsReviewer"], "user");
    for id in [77, 78] {
        let reply = sent.iter().find(|v| v["id"] == id).unwrap();
        assert_eq!(reply["result"]["action"], "accept");
        assert!(
            reply["result"].get("_meta").is_none(),
            "no broader provider policy"
        );
    }
}

#[cfg(unix)]
#[test]
fn native_pending_approval_expires_when_server_resolves_it() {
    let dir = tempfile::tempdir().unwrap();
    let after = [
        say(r#"{"id":77,"method":"item/commandExecution/requestApproval","params":{"threadId":"thr-1","turnId":"turn-1","command":"check","cwd":"/project"}}"#),
        "sleep 0.1\n".into(),
        say(r#"{"method":"serverRequest/resolved","params":{"threadId":"thr-1","requestId":77}}"#),
        "read -r l\n".into(),
        say(r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}}"#),
        "exit 0\n".into(),
    ].concat();
    let bin = fake_server(dir.path(), None, &after);
    let p = provider(
        &bin,
        dir.path(),
        Autonomy::Autonomous,
        None,
        Arc::new(Mutex::new(None)),
    )
    .with_approval_session(Default::default());
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let events = drain(p.run(req("check"), cancel_rx));
    let prompt = events
        .iter()
        .find_map(|e| {
            if let ProviderEvent::HostApproval(p) = e {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert!(!prompt.active());
    assert!(!prompt.answer(awili_providers::ApprovalChoice::Session));
    assert_eq!(events.last(), Some(&ProviderEvent::Done));
}

#[cfg(unix)]
#[test]
fn autonomous_never_falls_back_to_bypass_exec() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("old-codex");
    let starts = dir.path().join("starts");
    crate::exec_fixture::write_exec_fixture(
        &bin,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> {}\nexit 2\n",
            starts.display()
        ),
        0o755,
    );
    let p = provider(&bin, dir.path(), Autonomy::Autonomous, None, Arc::default());
    let (_cancel, rx) = std::sync::mpsc::channel();
    let events = drain(p.run(req("hello"), rx));
    assert!(events
        .iter()
        .any(|e| matches!(e, ProviderEvent::Error(s) if s.contains("app-server is required"))));
    assert_eq!(
        std::fs::read_to_string(starts)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["app-server"]
    );
}

#[cfg(unix)]
#[test]
fn oversized_app_server_frame_ends_with_error() {
    let dir = tempfile::tempdir().unwrap();
    let bin = fake_server(
        dir.path(),
        None,
        "head -c 1100000 /dev/zero | tr '\\0' x; sleep 2",
    );
    let p = provider(&bin, dir.path(), Autonomy::AskFirst, None, Arc::default());
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let events = drain(p.run(req("hello"), cancel_rx));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Error(s) if s.contains("size limit"))),
        "{events:?}"
    );
    assert_eq!(events.last(), Some(&ProviderEvent::Done));
}

#[cfg(unix)]
#[test]
fn plan_then_auto_resumes_thread_with_auto_posture() {
    let dir = tempfile::tempdir().unwrap();
    let after = say(
        r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}"#,
    );
    let bin = fake_server(dir.path(), None, &after);
    let slot = Arc::new(Mutex::new(None));
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let plan = provider(&bin, dir.path(), Autonomy::AskFirst, None, slot.clone());
    assert_eq!(
        drain(plan.run(req("plan"), cancel_rx)).last(),
        Some(&ProviderEvent::Done)
    );
    let resume = slot.lock().unwrap().clone().expect("thread id from plan");
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let auto = provider(&bin, dir.path(), Autonomy::Autonomous, Some(&resume), slot);
    assert_eq!(
        drain(auto.run(req("apply"), cancel_rx)).last(),
        Some(&ProviderEvent::Done)
    );
    let sent = read_lines(&dir.path().join("stdin"));
    let threads: Vec<_> = sent
        .iter()
        .filter(|v| matches!(v["method"].as_str(), Some("thread/start" | "thread/resume")))
        .collect();
    assert_eq!(threads.len(), 2, "{sent:?}");
    assert_eq!(threads[0]["method"], "thread/start");
    assert_eq!(threads[0]["params"]["sandbox"], "read-only");
    assert_eq!(threads[0]["params"]["approvalPolicy"], "never");
    assert_eq!(threads[1]["method"], "thread/resume");
    assert_eq!(threads[1]["params"]["threadId"], resume);
    assert_eq!(threads[1]["params"]["sandbox"], "danger-full-access");
    assert_eq!(threads[1]["params"]["approvalPolicy"], "on-request");
    assert_eq!(threads[1]["params"]["approvalsReviewer"], "auto_review");
}

#[cfg(unix)]
#[test]
fn slow_consumer_receives_normal_app_server_burst() {
    let dir = tempfile::tempdir().unwrap();
    let delta = r#"{"method":"item/agentMessage/delta","params":{"threadId":"thr-1","turnId":"turn-1","itemId":"msg-1","delta":"x"}}"#;
    let after = format!(
        "i=0; while [ $i -lt 1000 ]; do printf '%s\\n' '{delta}'; i=$((i+1)); done\n{}",
        say(
            r#"{"method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","items":[],"status":"completed"}}}"#
        )
    );
    let bin = fake_server(dir.path(), None, &after);
    let p = provider(&bin, dir.path(), Autonomy::AskFirst, None, Arc::default());
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let rx = p.run(req("burst"), cancel_rx);
    let mut text = String::new();
    loop {
        match rx.recv_blocking().unwrap() {
            ProviderEvent::Token(t) => {
                text.push_str(&t);
                std::thread::sleep(Duration::from_millis(1));
            }
            ProviderEvent::Error(e) => panic!("unexpected: {e}"),
            ProviderEvent::Done => break,
            _ => {}
        }
    }
    assert_eq!(text, "x".repeat(1000));
}

#[cfg(unix)]
#[test]
fn stop_ends_server_even_when_event_consumer_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let frame = r#"{"method":"item/agentMessage/delta","params":{"threadId":"thr-1","turnId":"turn-1","itemId":"msg-1","delta":"x"}}"#;
    let after = format!("while :; do printf '%s\\n' '{frame}'; done");
    let bin = fake_server(dir.path(), None, &after);
    let p = provider(&bin, dir.path(), Autonomy::AskFirst, None, Arc::default());
    let (cancel, cancel_rx) = std::sync::mpsc::channel();
    let rx = p.run(req("burst"), cancel_rx);
    let deadline = Instant::now() + Duration::from_secs(3);
    while rx.len() < 128 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(rx.len(), 128, "fixture did not fill event queue");
    cancel.send(()).unwrap();
    let pid = std::fs::read_to_string(dir.path().join("pid")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", pid.trim()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&out.stdout);
        if state.trim().is_empty() || state.trim().starts_with('Z') {
            break;
        }
        assert!(Instant::now() < deadline, "Stop did not end held server");
        std::thread::sleep(Duration::from_millis(10));
    }
    let events = drain(rx);
    assert_eq!(events.last(), Some(&ProviderEvent::Done));
}

#[cfg(unix)]
#[test]
fn pending_approval_backlog_expires_and_reports_failure() {
    let dir = tempfile::tempdir().unwrap();
    let approval = say(
        r#"{"id":77,"method":"item/commandExecution/requestApproval","params":{"threadId":"thr-1","turnId":"turn-1","command":"check","cwd":"/project"}}"#,
    );
    let noise = say(r#"{"method":"unrelated/notification","params":{}}"#);
    let resolved =
        say(r#"{"method":"serverRequest/resolved","params":{"threadId":"thr-1","requestId":77}}"#);
    let after = format!("{approval}{}{resolved}sleep 5\n", noise.repeat(8));
    let bin = fake_server(dir.path(), None, &after);
    let p = provider(&bin, dir.path(), Autonomy::Autonomous, None, Arc::default())
        .with_approval_session(Default::default());
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let events = drain(p.run(req("check"), cancel_rx));
    if let Some(prompt) = events.iter().find_map(|e| {
        if let ProviderEvent::HostApproval(p) = e {
            Some(p)
        } else {
            None
        }
    }) {
        assert!(!prompt.active());
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Error(s) if s.contains("approval was pending"))),
        "{events:?}"
    );
    assert_eq!(events.last(), Some(&ProviderEvent::Done));
}

#[cfg(unix)]
#[test]
fn nonblocking_server_write_observes_stop() {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "exec sleep 30"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let pipe = child.stdin.take().unwrap();
    set_nonblocking(&pipe).unwrap();
    let mut stdin = Some(pipe);
    let (cancel, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancel.send(()).unwrap();
    });
    let mut stopped = false;
    let start = Instant::now();
    assert!(!send_lines_cancellable(
        &mut stdin,
        &["x".repeat(1024 * 1024)],
        Some(&rx),
        &mut stopped
    ));
    assert!(stopped && start.elapsed() < Duration::from_secs(1));
    child.kill().unwrap();
    child.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn handshake_deadline_holds_under_finite_notification_flood() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("codex-noisy");
    crate::exec_fixture::write_exec_fixture(&bin, "#!/bin/sh\nread -r line\ni=0; while [ $i -lt 10000000 ]; do printf '%s\\n' '{\"method\":\"unrelated/notification\",\"params\":{}}'; i=$((i+1)); done\n", 0o700);
    let p = provider(&bin, dir.path(), Autonomy::Autonomous, None, Arc::default());
    let (_cancel, cancel_rx) = std::sync::mpsc::channel();
    let started = Instant::now();
    let events = drain(p.run(req("check"), cancel_rx));
    assert!(
        started.elapsed() < HANDSHAKE_TIMEOUT + Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProviderEvent::Error(s) if s.contains("app-server is required"))),
        "{events:?}"
    );
}
