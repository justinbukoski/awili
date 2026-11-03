//! An approved Work follow-up and a pending memory gate, over the real app.
//!
//! The memory service is a loopback stub that HOLDS each policy request until
//! the test releases it, and the harness is a fake `claude` that records its
//! argv and working directory. Everything goes through the app's own seams:
//! the composer's submit, the plan card's Approve, the chooser's setter,
//! `set_work_folder` and the Stop path. Nothing here calls the helpers the
//! production code uses to capture an owner; they are exercised by the turns.
//!
//! Proved:
//! 1. Approve, then (while the gate is pending) the next harness/model and the
//!    folder change. Releasing the gate runs the PLAN's captured owner exactly
//!    once, Autonomous, in the plan's folder, resuming the plan's session; the
//!    chosen next target is still chosen; the next ordinary turn is the stored
//!    Ask posture again, on the new model and folder.
//! 2. A gate that refuses before any spawn starts no child, restores the staged
//!    file, and keeps the previous native session id; so does a Stop in the gate
//!    window, whose late answer then cannot touch the run that replaced it.

use super::gateway_queue_tests::isolated;
use super::s5a_policy_tests::accepted_body;
use super::*;
use awili_code::CodeEngine;
use awili_gateway::run_events::ApprovalDecision;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A loopback memory service that holds every policy request until released.
struct Held {
    seen: Arc<AtomicUsize>,
    tickets: Arc<Mutex<Vec<Option<String>>>>,
}

impl Held {
    fn start() -> (Self, String) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(AtomicUsize::new(0));
        let tickets: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let (counter, held) = (seen.clone(), tickets.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (counter, held) = (counter.clone(), held.clone());
                std::thread::spawn(move || {
                    let mut data = Vec::new();
                    let mut buf = [0u8; 4096];
                    while data.windows(4).all(|w| w != b"\r\n\r\n") {
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        data.extend_from_slice(&buf[..n]);
                    }
                    let header = String::from_utf8_lossy(&data).to_ascii_lowercase();
                    let (status, body) = if header.contains("post /v6/bootstrap")
                        || header.contains("post /v6/recall")
                    {
                        let index = {
                            let mut tickets = held.lock().unwrap();
                            tickets.push(None);
                            tickets.len() - 1
                        };
                        counter.fetch_add(1, AtomicOrdering::SeqCst);
                        let deadline = Instant::now() + Duration::from_secs(14);
                        let body = loop {
                            if let Some(body) = held.lock().unwrap()[index].clone() {
                                break body;
                            }
                            if Instant::now() > deadline {
                                break "{}".to_string();
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        };
                        ("200 OK", body)
                    } else {
                        ("404 Not Found", "{}".to_string())
                    };
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                    let _ = stream.flush();
                });
            }
        });
        (Self { seen, tickets }, base)
    }

    /// Policy requests that have reached the service.
    fn requests(&self) -> usize {
        self.seen.load(AtomicOrdering::SeqCst)
    }

    /// Answer request `index` (in arrival order) with `body`.
    fn release(&self, index: usize, body: &str) {
        self.tickets.lock().unwrap()[index] = Some(body.to_string());
    }
}

/// The production memory client reads `$HOME/.config/foreman`; point it at
/// `base`. One request per turn (recall off), enabled.
fn memory_at(home: &Path, base: String) -> crate::MemoryCfg {
    let foreman = home.join(".config/foreman");
    std::fs::create_dir_all(&foreman).unwrap();
    let token = foreman.join("v6-reader-token");
    std::fs::write(&token, "reader\n").unwrap();
    std::fs::write(
        foreman.join("memory-backend.conf"),
        format!(
            "FOREMAN_MEMORY_BACKEND=v65\nFOREMAN_MEMORY_V65_BASE={base}\nFOREMAN_MEMORY_V65_READ_TOKEN={}\n",
            token.display()
        ),
    )
    .unwrap();
    crate::MemoryCfg {
        base_url: base,
        token: String::new(),
        min_priority: 90,
        enabled: true,
        recall: false,
        write: false,
    }
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn fake_claude(dir: &Path) -> PathBuf {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude`: records argv (NUL-separated) and cwd per call under
/// `dir/calls`, answers a stream-json turn with a session id (the `--resume`
/// id when given), and ends with the plan marker on calls that have a
/// `plan_<n>` file.
fn fake_claude(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = r#"#!/bin/sh
d=@DIR@
mkdir -p "$d/calls"
in=$(cat)
n=$(( $(ls "$d/calls" | grep -c '^claude\.[0-9]*$') + 1 ))
: > "$d/calls/claude.$n"
for a in "$@"; do printf '%s\0' "$a" >> "$d/calls/claude.$n"; done
printf '%s\0' "$in" >> "$d/calls/claude.$n"
pwd > "$d/calls/claude.$n.cwd"
sid="sess-$n"; prev=""
for a in "$@"; do [ "$prev" = "--resume" ] && sid="$a"; prev="$a"; done
printf '%s\n' "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$sid\"}"
if [ -f "$d/plan_$n" ]; then
  printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"1. rename the notes\n[[awili:plan-ready]]"}]}}'
else
  printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}'
fi
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"ok","usage":{"input_tokens":1,"output_tokens":1}}'
"#
    .replace("@DIR@", &dir.display().to_string());
    let bin = dir.join("claude");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Every recorded `claude` call, in order, as argv.
fn calls(dir: &Path) -> Vec<Vec<String>> {
    (1..)
        .map(|i| dir.join("calls").join(format!("claude.{i}")))
        .take_while(|p| p.exists())
        .map(|p| {
            let mut args: Vec<String> = std::fs::read(p)
                .unwrap()
                .split(|b| *b == 0)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            args.pop();
            // The last record is the stream-json line the turn was asked on stdin.
            if let Some(last) = args.last_mut() {
                if let Some(prompt) = crate::test_stdin::claude_prompt(last) {
                    *last = prompt;
                }
            }
            args
        })
        .collect()
}

fn cwd_of(dir: &Path, call: usize) -> PathBuf {
    PathBuf::from(
        std::fs::read_to_string(dir.join("calls").join(format!("claude.{call}.cwd")))
            .unwrap()
            .trim(),
    )
}

fn has(call: &[String], flag: &str) -> bool {
    call.iter().any(|a| a == flag)
}

fn has_pair(call: &[String], flag: &str, value: &str) -> bool {
    call.windows(2).any(|w| w[0] == flag && w[1] == value)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
}

fn settle(
    cx: &mut gpui::TestAppContext,
    app: &gpui::Entity<AwiliApp>,
    what: &str,
    done: impl Fn(&AwiliApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if app.read_with(cx, |a, _| done(a)) {
            return;
        }
        assert!(Instant::now() < deadline, "never settled: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn claude(model: Option<&str>) -> WorkExecutor {
    WorkExecutor::Agent {
        engine: CodeEngine::Claude,
        model: model.map(str::to_string),
    }
}

/// Everything a scenario needs: the app on its shown Workshop thread, the
/// fake harness directory, the held memory service, and two folders.
struct Scenario {
    app: gpui::Entity<AwiliApp>,
    conv: String,
    bins: tempfile::TempDir,
    held: Held,
    plan_dir: tempfile::TempDir,
    later_dir: tempfile::TempDir,
}

impl Scenario {
    fn new(cx: &mut gpui::TestAppContext, home: &Path) -> Self {
        let bins = tempfile::tempdir().unwrap();
        let plan_dir = tempfile::tempdir().unwrap();
        let later_dir = tempfile::tempdir().unwrap();
        let claude_bin = fake_claude(bins.path());
        let (held, base) = Held::start();
        let memory = memory_at(home, base);
        let app = cx.new(AwiliApp::new);
        let conv = app.update(cx, |a, cx| {
            a.onboarding = crate::onboarding::Onboarding::existing();
            a.fleet_pack = None;
            a.memory = memory;
            a.mode = Mode::Work;
            // Every other harness is a path that does not exist: nothing
            // installed can run.
            a.work.agents.claude_bin = Some(claude_bin.display().to_string());
            for bin in [
                &mut a.work.agents.codex_bin,
                &mut a.work.agents.zcode_bin,
                &mut a.work.agents.dsh_bin,
            ] {
                *bin = Some(bins.path().join("not-installed").display().to_string());
            }
            a.work.folder = Some(plan_dir.path().to_path_buf());
            a.work
                .graduation
                .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
            let conv = a.sidebar.active_id().to_string();
            let key = crate::ask_mode::AskKey::Work(conv.clone());
            a.work
                .ask_modes
                .set(key, awili_work::ApprovalMode::AskBeforeChanges);
            let owner = crate::workshop_composer::ModelOwner::Work(conv.clone());
            assert!(a.set_composer_target(&owner, claude(None), cx));
            conv
        });
        Self {
            app,
            conv,
            bins,
            held,
            plan_dir,
            later_dir,
        }
    }

    fn owner(&self) -> crate::workshop_composer::ModelOwner {
        crate::workshop_composer::ModelOwner::Work(self.conv.clone())
    }

    fn send(&self, cx: &mut gpui::TestAppContext, text: &str) {
        self.app.update(cx, |a, cx| {
            a.composer.update(cx, |c, cx| c.set_text(text, cx));
            a.submit_composer_draft(cx);
        });
    }

    fn choose(&self, cx: &mut gpui::TestAppContext, target: WorkExecutor) {
        let owner = self.owner();
        assert!(self
            .app
            .update(cx, |a, cx| a.set_composer_target(&owner, target, cx)));
    }

    /// Wait for the gate's `n`th request, answer it, wait for the run to end.
    fn answer(&self, cx: &mut gpui::TestAppContext, index: usize, body: &str) {
        settle(cx, &self.app, "the gate request arrived", |_| {
            self.held.requests() > index
        });
        self.held.release(index, body);
    }

    fn idle(&self, cx: &mut gpui::TestAppContext, what: &str) {
        settle(cx, &self.app, what, |a| {
            !a.work.running && !a.work.agent_gate_pending
        });
    }
}

/// Approve; then, while the memory gate is still pending, the NEXT harness
/// model and the folder change. The released gate runs the plan's own owner
/// once, Autonomous, in the plan's folder, resuming the plan's session. The
/// chosen next target survives, and the next ordinary turn is back to the
/// stored Ask posture on the new model and folder.
#[gpui::test]
fn approved_followup_runs_its_captured_owner_while_next_state_changes_behind_a_held_gate(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::gate_follow_tests::approved_followup_runs_its_captured_owner_while_next_state_changes_behind_a_held_gate",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let s = Scenario::new(cx, &home);
    std::fs::write(s.bins.path().join("plan_1"), "").unwrap();
    let ask = crate::ask_mode::AskKey::Work(s.conv.clone());

    // 1. The planning turn (Ask: read-only) ends with a plan card.
    s.send(cx, "tidy the notes");
    s.answer(cx, 0, &accepted_body());
    settle(cx, &s.app, "the plan card", |a| {
        !a.work.running && a.plan_gate_pending_here()
    });
    let first = calls(s.bins.path());
    assert_eq!(first.len(), 1);
    assert!(
        has_pair(&first[0], "--permission-mode", "plan"),
        "{:?}",
        first[0]
    );
    assert!(same_dir(&cwd_of(s.bins.path(), 1), s.plan_dir.path()));

    // 2. Approve. The run is accepted and waits on the held gate: no child.
    s.app.update(cx, |a, cx| {
        a.resolve_work_approval(ApprovalDecision::Approve, cx)
    });
    settle(cx, &s.app, "the follow-up waits on the gate", |a| {
        a.work.agent_gate_pending && a.work.running
    });
    settle(cx, &s.app, "its policy request", |_| s.held.requests() >= 2);
    assert_eq!(
        calls(s.bins.path()).len(),
        1,
        "no child before the gate answers"
    );

    // 3. Meanwhile the owner picks another model and another folder.
    s.choose(cx, claude(Some("next-model")));
    let later = s.later_dir.path().to_path_buf();
    s.app
        .update(cx, |a, cx| a.set_work_folder(later.clone(), cx));
    s.app.read_with(cx, |a, _| {
        assert_eq!(
            a.composer_target_for(&s.owner()),
            Some(claude(Some("next-model")))
        );
        assert_eq!(
            a.work.ask_modes.mode_for(&ask),
            awili_work::ApprovalMode::AskBeforeChanges,
            "the stored choice is not touched by an approval"
        );
    });

    // 4. The gate is released: the plan's owner runs, once.
    s.held.release(1, &accepted_body());
    s.idle(cx, "the approved follow-up");
    let second = calls(s.bins.path());
    assert_eq!(second.len(), 2, "the follow-up ran exactly once");
    let followup = &second[1];
    assert!(
        has(followup, "--dangerously-skip-permissions"),
        "{followup:?}"
    );
    assert!(!has(followup, "--permission-mode"), "{followup:?}");
    assert!(
        !has(followup, "--model"),
        "the plan's own model, not the next one: {followup:?}"
    );
    assert!(has_pair(followup, "--resume", "sess-1"), "{followup:?}");
    assert!(
        same_dir(&cwd_of(s.bins.path(), 2), s.plan_dir.path()),
        "the plan's folder, not the new one"
    );
    s.app.read_with(cx, |a, _| {
        assert_eq!(
            a.composer_target_for(&s.owner()),
            Some(claude(Some("next-model"))),
            "the next target is still the one chosen"
        );
        assert_eq!(a.work.folder.as_deref(), Some(s.later_dir.path()));
    });

    // 5. The next ordinary turn is the stored Ask posture again.
    s.send(cx, "and the next thing");
    s.answer(cx, 2, &accepted_body());
    s.idle(cx, "the ordinary turn");
    let third = calls(s.bins.path());
    assert_eq!(third.len(), 3);
    assert!(
        has_pair(&third[2], "--model", "next-model"),
        "{:?}",
        third[2]
    );
    assert!(
        has_pair(&third[2], "--permission-mode", "plan"),
        "{:?}",
        third[2]
    );
    assert!(
        !has(&third[2], "--dangerously-skip-permissions"),
        "{:?}",
        third[2]
    );
    assert!(same_dir(&cwd_of(s.bins.path(), 3), s.later_dir.path()));
}

/// A refusal before any spawn, and a Stop in the gate window, start no child,
/// give the staged file back, and keep the previous native session. The late
/// answer of the stopped request cannot touch the run that replaced it, and the
/// files reach the provider exactly once, on the session the thread already had.
#[gpui::test]
fn gate_refusal_and_stop_keep_the_session_and_files_and_a_late_answer_changes_nothing(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::gate_follow_tests::gate_refusal_and_stop_keep_the_session_and_files_and_a_late_answer_changes_nothing",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let s = Scenario::new(cx, &home);
    let staged = |cx: &mut gpui::TestAppContext| {
        s.app.read_with(cx, |a, _| {
            a.staged_attachments
                .iter()
                .map(|f| f.filename.clone())
                .collect::<Vec<_>>()
        })
    };

    // A first turn gives the thread a native session ("sess-1").
    s.send(cx, "first");
    s.answer(cx, 0, &accepted_body());
    s.idle(cx, "the first turn");
    assert_eq!(calls(s.bins.path()).len(), 1);

    // Refusal: the policy lane is malformed. No child; the file is back.
    s.app.update(cx, |a, cx| {
        a.attach_bytes("keep.txt", "text/plain", b"STAGED-ONE", cx)
    });
    s.send(cx, "second");
    settle(cx, &s.app, "the gate holds the second turn", |a| {
        a.work.agent_gate_pending
    });
    s.answer(cx, 1, r#"{"mandatory_policy": null, "items": []}"#);
    s.idle(cx, "the refusal");
    s.app.read_with(cx, |a, _| {
        assert_eq!(a.work.panel.status, Some(RunStatus::Failed));
        assert!(a
            .work
            .panel
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with(crate::recall::POLICY_REFUSAL_PREFIX)));
    });
    assert_eq!(
        calls(s.bins.path()).len(),
        1,
        "a refused turn starts no child"
    );
    assert_eq!(staged(cx), ["keep.txt"], "the file is back in the tray");

    // Stop inside the gate window: the same, and the held answer goes stale.
    s.send(cx, "third");
    settle(cx, &s.app, "the gate holds the third turn", |a| {
        a.work.agent_gate_pending
    });
    settle(cx, &s.app, "its request", |_| s.held.requests() >= 3);
    s.app.update(cx, |a, cx| a.abort_active_stream(cx));
    s.idle(cx, "the stop");
    assert_eq!(
        calls(s.bins.path()).len(),
        1,
        "a stopped turn starts no child"
    );
    assert_eq!(staged(cx), ["keep.txt"], "the file survives the Stop");

    // A new run replaces it. The stopped request's answer arrives late.
    s.send(cx, "fourth");
    settle(cx, &s.app, "the gate holds the fourth turn", |a| {
        a.work.agent_gate_pending
    });
    settle(cx, &s.app, "its request", |_| s.held.requests() >= 4);
    let seq = s.app.read_with(cx, |a, _| a.work.run_seq);
    s.held.release(2, &accepted_body());
    std::thread::sleep(Duration::from_millis(200));
    cx.run_until_parked();
    s.app.read_with(cx, |a, _| {
        assert!(
            a.work.agent_gate_pending && a.work.running,
            "the new run still waits"
        );
        assert_eq!(a.work.run_seq, seq, "the late answer did not touch the run");
    });
    assert_eq!(
        calls(s.bins.path()).len(),
        1,
        "no child from a stale answer"
    );

    // The new run's own answer starts exactly one child, on the old session,
    // with the file once.
    s.held.release(3, &accepted_body());
    s.idle(cx, "the fourth turn");
    let all = calls(s.bins.path());
    assert_eq!(all.len(), 2, "exactly one child");
    assert!(has_pair(&all[1], "--resume", "sess-1"), "{:?}", all[1]);
    let sent = all[1]
        .iter()
        .find(|a| a.contains("fourth"))
        .expect("the task");
    // The bridged prompt legitimately carries the thread's history: the
    // refused second and the stopped third turn each recorded the file they
    // were sent with, once each. The CURRENT request carries it once, and the
    // current request is not repeated inside the history.
    let (before, current) = sent
        .split_once("Current request:")
        .expect("the bridged prompt names the current request");
    assert_eq!(current.matches("STAGED-ONE").count(), 1, "{current}");
    assert!(current.contains("fourth"), "{current}");
    let history_line = before
        .lines()
        .find(|line| line.starts_with("{\"project_folder\""))
        .expect("the history JSON line");
    let history: serde_json::Value = serde_json::from_str(history_line).unwrap();
    let entries: Vec<(String, String)> = history["history"]
        .as_array()
        .expect("history entries")
        .iter()
        .map(|e| {
            (
                e["kind"]["message"]["role"].as_str().unwrap().to_string(),
                e["content"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let with_file = |word: &str| -> usize {
        entries
            .iter()
            .filter(|(role, text)| role == "user" && text.contains(word))
            .map(|(_, text)| text.matches("STAGED-ONE").count())
            .sum()
    };
    assert_eq!(with_file("second"), 1, "{entries:?}");
    assert_eq!(with_file("third"), 1, "{entries:?}");
    assert_eq!(
        entries
            .iter()
            .map(|(_, text)| text.matches("STAGED-ONE").count())
            .sum::<usize>(),
        2,
        "only the second and third turns' own copies are history: {entries:?}"
    );
    assert!(
        entries.iter().all(|(_, text)| !text.contains("fourth")),
        "the current request is not also history: {entries:?}"
    );
    assert!(
        staged(cx).is_empty(),
        "the tray clears once it was accepted"
    );
}

#[path = "work_send_conflict_tests.rs"]
mod send_conflicts;
