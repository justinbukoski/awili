//! Explicit opt-in: real subscription app-server against a thread held by the desktop app.
use awili_code::{
    app_server::{AppServerTurn, TurnSpec},
    Autonomy, CodeEvent,
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

fn run(source: &str, prompt: &str) -> (String, String, usize) {
    let mut child = Command::new(std::env::var("AWILI_TEST_CODEX_BIN").unwrap())
        .arg("app-server")
        .env("AWILI_NATIVE_RESUME_ID", source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut protocol = AppServerTurn::new(TurnSpec {
        cwd: std::env::current_dir().unwrap().display().to_string(),
        prompt: prompt.into(),
        resume_id: Some(source.into()),
        model: Some("gpt-6.1-sol".into()),
        autonomy: Autonomy::AskFirst,
        host_approvals: false,
    });
    let mut pending = protocol.start();
    let mut answer = String::new();
    let mut session = String::new();
    let mut forks = 0;
    let mut turns = 0;
    let deadline = Instant::now() + Duration::from_secs(180);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        while !protocol.finished() {
            for line in pending.drain(..) {
                let v: Value = serde_json::from_str(&line).unwrap();
                forks += usize::from(v["method"] == "thread/fork");
                turns += usize::from(v["method"] == "turn/start");
                writeln!(child.stdin.as_mut().unwrap(), "{line}").unwrap();
            }
            let line = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            let step = protocol.on_line(&line);
            pending = step.send;
            for event in step.events {
                match event {
                    CodeEvent::SessionStarted { session_id } => session = session_id,
                    CodeEvent::AssistantText(text) => answer.push_str(&text),
                    CodeEvent::Error(error) => panic!("native failure: {error}"),
                    _ => {}
                }
            }
        }
        assert!(protocol.completed());
        assert_eq!(turns, 1, "pending input submitted once");
    }));
    drop(child.stdin.take());
    let end = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    child.wait().unwrap();
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
    (session, answer, forks)
}

#[test]
#[ignore = "live subscription: explicitly supply desktop-held thread and Codex binary"]
fn desktop_writer_and_awili_protocol_continue_independently() {
    let parent = std::env::var("AWILI_TEST_LOCKED_THREAD").unwrap();
    let marker = format!("branch-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    let prompt = format!("Acceptance test only, no tools: from the prior conversation name the application being developed and the session ownership problem reported in its screenshot. Remember branch-only marker {marker}. Answer in one sentence.");
    let (fork, answer, forks) = run(&parent, &prompt);
    assert_eq!(forks, 1);
    assert_ne!(fork, parent);
    assert!(answer.to_lowercase().contains("awili"), "{answer}");
    assert!(
        answer.to_lowercase().contains("writer")
            || answer.to_lowercase().contains("ownership")
            || answer.to_lowercase().contains("conflict"),
        "{answer}"
    );
    let (resumed, recall, forks) = run(
        &fork,
        "No tools: reply only with the branch-only marker you were asked to remember.",
    );
    assert_eq!(forks, 0);
    assert_eq!(resumed, fork);
    assert!(recall.contains(&marker), "{recall}");
    std::fs::write("/tmp/awili-ownership-live-receipt.json", serde_json::to_vec_pretty(&json!({"parent":parent,"fork":fork,"parent_context_answer":answer,"branch_recall":recall,"forks_on_conflict":1,"forks_on_later_resume":0,"pending_turn_submissions_each":1})).unwrap()).unwrap();
}
