//! RC2-R3 (usability RC2 round 3, F7 / Finding 5: Claude wrote nothing for
//! 13.5 s on a second council message, about 22 s on the first): a council's
//! Claude member is started ahead, while the owner reads.
//!
//! Measured 2026-10-07 on the owner's Mac (claude 2.1.282, his own settings
//! and hooks): a `claude -p` turn spends about 9 s before its first request
//! (`system init` at 8.7-9.3 s; 0.3 s with his hooks off) — his SessionStart
//! hook alone takes 7.3 s, his UserPromptSubmit hook 1.2-1.6 s — and that
//! start-up only begins when the process does. A Claude started with
//! `--input-format stream-json` runs its start-up, the SessionStart hook
//! included, while it waits for its message: asked 12 s later, its first
//! words came 3.6-4.0 s after the message (10.7 s for one started at the
//! message). The message then goes in on its stdin, which is closed, so the
//! process ends after that one reply, as a `-p` turn does.
//!
//! Like the Codex servers made ready ahead (`codex_app_server::prewarm`): a
//! ready Claude belongs to one council, takes only a turn with exactly its
//! launch, ends after [`crate::codex_app_server::WARM_TTL`] unasked, at most
//! [`WARM_CAP`] wait at once, and a council archived or deleted ends its own
//! (`codex_app_server::close_warm_for` calls [`close_warm_for`]).

use crate::proc_compat::OwnProcessGroup;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::code_provider::{BriefFile, McpConfigGuard};

/// The flags a ready Claude is started with in place of the question.
pub(crate) const INPUT_FLAGS: [&str; 2] = ["--input-format", "stream-json"];

/// At most this many ready Claude processes at once.
const WARM_CAP: usize = 4;

/// What a ready Claude must match to take a turn: the same binary, launch
/// (every flag but the question), folder, council and connector server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WarmKey {
    pub(crate) bin: String,
    /// The turn's arguments with the question replaced by [`INPUT_FLAGS`]
    /// (and no `--mcp-config` file: each ready process has its own).
    pub(crate) args: Vec<String>,
    pub(crate) cwd: String,
    /// The council it was made for.
    pub(crate) owner: String,
    /// The connector server (`command`, `args`), when the turn has one.
    pub(crate) connectors: Option<(String, Vec<String>)>,
    /// rc.4 S7: the seatbelt profile it was started under (`None` = not
    /// sandboxed); a turn takes only a Claude started the same way.
    pub(crate) sandbox: Option<std::path::PathBuf>,
}

/// A ready Claude taken for a turn: the process, and its `--mcp-config`
/// file (removed when the turn ends).
pub(crate) struct Taken {
    pub(crate) child: Child,
    pub(crate) guard: Option<McpConfigGuard>,
    /// The brief file the ready process was started with (removed with it).
    pub(crate) brief: Option<BriefFile>,
}

struct Entry {
    key: WarmKey,
    taken: Taken,
    born: Instant,
}

static WARM: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// A turn's Claude arguments (its question last) as a ready process's: the
/// question replaced by [`INPUT_FLAGS`].
pub(crate) fn warm_args(args: &[String]) -> Vec<String> {
    let mut out = args[..args.len().saturating_sub(1)].to_vec();
    out.extend(INPUT_FLAGS.iter().map(|s| s.to_string()));
    out
}

/// The one stdin line that asks a ready Claude `prompt`.
pub(crate) fn user_message(prompt: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": awili_image_content::anthropic(prompt)},
    })
    .to_string()
}

/// Start a ready Claude for `key`: `bin` with `args` (the key's, plus its own
/// `--mcp-config` file when `guard` is one), in `key.cwd`, its own process
/// group (registered, so a quit ends it). Nothing when one is ready already,
/// when the council was closed, or when it cannot start.
pub(crate) fn prewarm(
    key: WarmKey,
    args: &[String],
    child_path: &str,
    guard: Option<McpConfigGuard>,
    brief: Option<BriefFile>,
) {
    if crate::codex_app_server::is_closed(&key.owner) {
        return;
    }
    if let Ok(mut pool) = WARM.lock() {
        expire(&mut pool);
        if pool.iter().any(|e| e.key == key) {
            return;
        }
    }
    let (program, full_args) =
        crate::agent_sandbox::launch_command(key.sandbox.as_deref(), &key.bin, args);
    let mut cmd = Command::new(&program);
    cmd.args(&full_args)
        .current_dir(&key.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .own_process_group()
        .env("PATH", child_path);
    let Ok(child) = crate::proc_compat::spawn_tracked(&mut cmd) else {
        return;
    };
    let pgid = child.id() as i32;
    crate::code_provider::register_group(pgid);
    let entry = Entry {
        key,
        taken: Taken {
            child,
            guard,
            brief,
        },
        born: Instant::now(),
    };
    let Ok(mut pool) = WARM.lock() else {
        end(entry.taken);
        return;
    };
    // A council closed while it started keeps nothing ready.
    if pool.iter().any(|e| e.key == entry.key)
        || crate::codex_app_server::is_closed(&entry.key.owner)
    {
        drop(pool);
        end(entry.taken);
        return;
    }
    while pool.len() >= WARM_CAP {
        end(pool.remove(0).taken);
    }
    pool.push(entry);
    drop(pool);
    // When its time is up it is ended, asked or not.
    std::thread::spawn(|| {
        std::thread::sleep(ttl());
        if let Ok(mut pool) = WARM.lock() {
            expire(&mut pool);
        }
    });
}

/// The ready Claude for this launch, if one is still waiting.
pub(crate) fn take(key: &WarmKey) -> Option<Taken> {
    let mut pool = WARM.lock().ok()?;
    expire(&mut pool);
    let at = pool.iter().position(|e| &e.key == key)?;
    Some(pool.remove(at).taken)
}

/// Ask a taken Claude `prompt`: the message goes in, then its stdin closes
/// (it ends after this one reply). `false` when it could not be written.
pub(crate) fn ask(child: &mut Child, prompt: &str) -> bool {
    let Some(mut stdin) = child.stdin.take() else {
        return false;
    };
    let ok = stdin.write_all(user_message(prompt).as_bytes()).is_ok()
        && stdin.write_all(b"\n").is_ok()
        && stdin.flush().is_ok();
    drop(stdin);
    ok
}

/// End a ready Claude that will not take a turn.
pub(crate) fn end(mut taken: Taken) {
    let pgid = taken.child.id() as i32;
    crate::code_provider::kill_process_group(pgid);
    let _ = taken.child.wait();
    crate::code_provider::deregister_group(pgid);
    drop(taken.guard.take());
}

/// End the ready Claude processes made for council `owner` (archived or
/// deleted), off the caller's thread.
pub(crate) fn close_warm_for(owner: &str) {
    let gone: Vec<Entry> = match WARM.lock() {
        Ok(mut pool) => {
            let (gone, keep) = pool.drain(..).partition(|e| e.key.owner == owner);
            *pool = keep;
            gone
        }
        Err(_) => return,
    };
    if !gone.is_empty() {
        std::thread::spawn(move || {
            for e in gone {
                end(e.taken);
            }
        });
    }
}

/// End every ready Claude that waited too long or has gone.
fn expire(pool: &mut Vec<Entry>) {
    let ttl = ttl();
    let mut keep = Vec::with_capacity(pool.len());
    for mut e in pool.drain(..) {
        let alive = matches!(e.taken.child.try_wait(), Ok(None));
        if alive && e.born.elapsed() < ttl {
            keep.push(e);
        } else {
            end(e.taken);
        }
    }
    *pool = keep;
}

fn ttl() -> Duration {
    crate::codex_app_server::WARM_TTL
}

/// How many Claude processes are ready now (tests).
#[cfg(test)]
pub(crate) fn warm_count() -> usize {
    WARM.lock().map(|p| p.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The question gives way to the stdin flags; it goes in as one JSON
    /// line instead.
    #[test]
    fn rc2r3_a_ready_claude_takes_its_question_on_stdin() {
        let args: Vec<String> = ["-p", "--output-format", "stream-json", "Q?"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            warm_args(&args),
            [
                "-p",
                "--output-format",
                "stream-json",
                "--input-format",
                "stream-json"
            ]
        );
        let line = user_message("Gemma or \"Qwen\"?\nwhy");
        assert!(!line.contains('\n'), "{line}");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "Gemma or \"Qwen\"?\nwhy");
    }

    #[cfg(unix)]
    /// RC2-R3M11 (RC2-R39 review P2): a ready Claude that cannot take its
    /// question (its stdin is gone) is ended and the turn starts its own
    /// Claude \u{2014} with its own `--mcp-config` file, so the connectors stay;
    /// the ready one's file went with it.
    #[test]
    fn rc2r3m11_a_cold_start_after_a_failed_ask_keeps_the_connectors() {
        use crate::code_provider::CodeProvider;
        use awili_providers::{ChatMessage, ChatRequest, ProviderEvent};
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display().to_string();
        let bin = dir.path().join("claude");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s ' \"$@\" >> {d}/starts\nprintf '\\n@@@\\n' >> {d}/starts\n\
                 case \"$*\" in *--input-format*) if [ ! -f {d}/ready ]; then exec 0<&-; echo up >> {d}/ready; sleep 30; exit 0; fi; cat > {d}/cold-stdin ;; esac\n\
                 printf '%s\\n' \
                 '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"Cold.\"}}]}}}}' \
                 '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"Cold.\"}}'\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let council = "rc2r3m11-council";
        let provider = CodeProvider::new(
            bin.display().to_string(),
            awili_code::CodeEngine::Claude,
            awili_code::Autonomy::Autonomous,
            d.clone(),
            None,
            None,
            std::sync::Arc::new(std::sync::Mutex::new(None)),
        )
        .for_council(council)
        .with_connectors_mcp(awili_config::McpServerConfig {
            name: "awili-connectors".into(),
            command: "/bin/echo".into(),
            args: vec!["connectors".into()],
            env: Default::default(),
            trust: Default::default(),
            modes: vec![awili_config::McpMode::Work],
            enabled: true,
        });
        let req = || ChatRequest {
            model: String::new(),
            system: Some("BRIEF".into()),
            messages: vec![ChatMessage::user("Gemma or Qwen?")],
        };
        provider.prewarm(req(), council);
        let deadline = Instant::now() + crate::exec_fixture::ANSWER_DEADLINE;
        while !dir.path().join("ready").exists() {
            assert!(Instant::now() < deadline, "never made ready");
            std::thread::sleep(Duration::from_millis(20));
        }
        let (_cancel, cancel_rx) = std::sync::mpsc::channel();
        let rx = provider.run(req(), cancel_rx);
        let mut text = String::new();
        while let Ok(ev) = rx.recv_blocking() {
            match ev {
                ProviderEvent::Token(t) => text.push_str(&t),
                ProviderEvent::Done => break,
                _ => {}
            }
        }
        assert_eq!(text.trim(), "Cold.");
        let starts = std::fs::read_to_string(dir.path().join("starts")).unwrap();
        let starts: Vec<&str> = starts
            .split("\n@@@\n")
            .filter(|s| !s.trim().is_empty())
            .collect();
        assert_eq!(starts.len(), 2, "{starts:?}");
        assert!(starts[0].contains("--input-format"), "{starts:?}");
        // Neither the ready one nor the cold start has the brief on its argv.
        for s in &starts {
            assert!(!s.contains("BRIEF"), "{starts:?}");
            assert!(s.contains("--append-system-prompt-file"), "{starts:?}");
        }
        assert!(
            starts[1].contains("--input-format"),
            "the cold start is asked on stdin too: {starts:?}"
        );
        assert!(
            starts[1].contains("--mcp-config "),
            "the cold start keeps the connectors: {starts:?}"
        );
        assert!(!starts[1].contains("Gemma or Qwen?"), "{starts:?}");
        let asked = std::fs::read_to_string(dir.path().join("cold-stdin")).unwrap();
        assert!(asked.contains("Gemma or Qwen?"), "{asked}");
    }
}
