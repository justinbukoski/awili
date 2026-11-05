//! RC2-R56: sign-in state and the signed-out line.

use super::*;

/// The round-4 failure reads as one plain line that says what to do; any
/// other failure is left as it is.
#[test]
fn rc2r56_a_signed_out_failure_says_how_to_sign_in() {
    let raw = "Failed to authenticate: OAuth session expired and could not be refreshed";
    assert_eq!(
        signed_out_line(Some(CodeEngine::Claude), raw).as_deref(),
        Some(
            "Claude is signed out. Open Terminal, run claude, then type /login. Then press Retry."
        )
    );
    // Read from its own words when the helper is not known.
    assert_eq!(
        signed_out_line(None, raw).as_deref(),
        Some(
            "Claude is signed out. Open Terminal, run claude, then type /login. Then press Retry."
        )
    );
    assert_eq!(
        signed_out_line(Some(CodeEngine::Codex), "Not logged in").as_deref(),
        Some("Codex is signed out. Open Terminal and run codex login. Then press Retry.")
    );
    assert_eq!(
        signed_out_line(Some(CodeEngine::Claude), "rate limited"),
        None
    );
}

/// What `claude auth status` and `codex login status` answer, as the CLIs
/// print it (checked live 2026-10-08, claude 2.1.282).
#[test]
fn rc2r57_status_answers_are_read() {
    let out = br#"{"loggedIn": false, "authMethod": "none"}"#;
    assert_eq!(
        read_status(CodeEngine::Claude, false, out),
        SignIn::SignedOut
    );
    let ok = br#"{"loggedIn": true, "authMethod": "claude.ai"}"#;
    assert_eq!(read_status(CodeEngine::Claude, true, ok), SignIn::SignedIn);
    assert_eq!(
        read_status(CodeEngine::Claude, false, b"usage: ..."),
        SignIn::Unknown
    );
    assert_eq!(
        read_status(CodeEngine::Codex, true, b"Logged in using ChatGPT\n"),
        SignIn::SignedIn
    );
    assert_eq!(
        read_status(CodeEngine::Codex, false, b"Not logged in\n"),
        SignIn::SignedOut
    );
    assert_eq!(read_status(CodeEngine::Glm, true, b"x"), SignIn::Unknown);
}

/// Ask a stand-in CLI that always answers, with the answer budget
/// ([`crate::exec_fixture::ANSWER_DEADLINE`]): the
/// production `STATUS_DEADLINE` (3 s) is not what these tests check, and on
/// macOS the first exec of a freshly written script can stall for seconds
/// while the system scans it under load — which turned a quick "signed in"
/// into `Unknown` (seen in the K3b Mac gate). The deadline itself is
/// covered by the hanging-CLI test with its own short deadline.
fn stub_answers(engine: CodeEngine, bin: &str) -> SignIn {
    sign_in_state_within(engine, bin, crate::exec_fixture::ANSWER_DEADLINE)
}

/// The state comes from the CLI itself (a stand-in script here), asked once
/// and remembered until Check again.
#[test]
fn rc2r57_the_cli_is_asked_once_until_check_again() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("claude");
    let count = dir.path().join("count");
    let script = |logged: bool| {
        format!(
            "#!/bin/sh\necho x >> '{}'\necho '{{\"loggedIn\": {logged}}}'\nexit {}\n",
            count.display(),
            if logged { 0 } else { 1 }
        )
    };
    // Never `fs::write` an executable a test will exec (ETXTBSY, RC1-G2).
    crate::exec_fixture::write_exec_fixture(&bin, &script(false), 0o755);
    let path = bin.to_str().unwrap();
    assert_eq!(stub_answers(CodeEngine::Claude, path), SignIn::SignedOut);
    crate::exec_fixture::write_exec_fixture(&bin, &script(true), 0o755);
    assert_eq!(
        stub_answers(CodeEngine::Claude, path),
        SignIn::SignedOut,
        "remembered"
    );
    assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
    forget_sign_in();
    assert_eq!(stub_answers(CodeEngine::Claude, path), SignIn::SignedIn);
    assert_eq!(
        sign_in_state(CodeEngine::Claude, "/nonexistent/claude"),
        SignIn::Unknown
    );
}

/// The council card for the round-4 failure: red (the owner must act), and
/// it says how.
#[test]
fn rc2r56_a_council_card_says_how_to_sign_in() {
    let raw = "Failed to authenticate: OAuth session expired and could not be refreshed";
    let (line, red) = crate::council_run::failure_sentence("Claude Sonnet", None, raw);
    assert!(red, "the owner must act");
    assert_eq!(
        line,
        "Claude Sonnet needs you to sign in again. Open Terminal, run claude, then type /login. \
         Then press Retry."
    );
}

/// RC2-R5M4 (slice review befb867d, P2): a status command that hangs is
/// given up on at the deadline and stopped; the state is `Unknown` (claimed
/// neither way). (That it is remembered is proven in the Settings > Code
/// test, in its own process: another test here may press Check again.)
#[test]
fn rc2r5m4_a_hung_status_command_is_given_up_on() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("claude");
    let pid = dir.path().join("pid");
    // Never `fs::write` an executable a test will exec (ETXTBSY, RC1-G2).
    crate::exec_fixture::write_exec_fixture(
        &bin,
        &format!("#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n", pid.display()),
        0o755,
    );
    let path = bin.to_str().unwrap();
    assert_eq!(
        known_sign_in(CodeEngine::Claude, path),
        None,
        "not asked yet"
    );
    let started = std::time::Instant::now();
    assert_eq!(
        sign_in_state_within(
            CodeEngine::Claude,
            path,
            std::time::Duration::from_millis(300)
        ),
        SignIn::Unknown
    );
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_secs(3),
        "given up at the deadline, not after the 30 s hang: {took:?}"
    );
    // The hung command was stopped (when it got as far as saying who it is).
    if let Ok(p) = std::fs::read_to_string(&pid) {
        let alive = std::process::Command::new("kill")
            .args(["-0", p.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "the hung status command was stopped");
    }
    assert!(STATUS_DEADLINE <= std::time::Duration::from_secs(5));
    // The app's own deadline is pinned (test builds use a longer one).
    assert_eq!(STATUS_DEADLINE, std::time::Duration::from_secs(3));
}

/// RC2-R5M7: run `test` in a subprocess, so its "Check again" never meets
/// another test's remembered answers. `true` inside the child.
fn in_own_process(test: &str) -> bool {
    const CHILD: &str = "AWILI_RC2R5M7_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

/// RC2-R5M7 (slice review 3775ed08, P2): "Check again" pressed while a
/// check stores its answer — exactly between the check's generation test
/// and its insertion, driven step by step (no timing). The answer that
/// check got is obsolete and is never left behind: afterwards nothing is
/// remembered, and the next check asks the CLI anew.
#[test]
fn rc2r5m7_check_again_during_a_store_never_keeps_the_old_answer() {
    if !in_own_process(
        "agent_signin::tests::rc2r5m7_check_again_during_a_store_never_keeps_the_old_answer",
    ) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("claude");
    let count = dir.path().join("count");
    // Never `fs::write` an executable a test will exec (ETXTBSY, RC1-G2).
    crate::exec_fixture::write_exec_fixture(
        &bin,
        &format!(
            "#!/bin/sh\necho x >> '{}'\necho '{{\"loggedIn\": true}}'\n",
            count.display()
        ),
        0o755,
    );
    let path = bin.to_str().unwrap().to_string();
    let key = format!("{}\u{0}{path}", CodeEngine::Claude.bin());
    let forgetter: std::sync::Arc<Mutex<Option<std::thread::JoinHandle<()>>>> =
        std::sync::Arc::new(Mutex::new(None));
    let slot = forgetter.clone();
    race_hooks::on_between_test_and_store(
        key,
        Box::new(move || {
            // "Check again" on another thread, now. It either finishes, or
            // finds the answers held by this store and waits for it; this
            // store goes on once it is told which.
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let done = tx.clone();
            let handle = std::thread::spawn(move || {
                race_hooks::on_forget_waits(std::thread::current().id(), tx);
                forget_sign_in();
                let _ = done.send(());
            });
            rx.recv().unwrap();
            *slot.lock().unwrap() = Some(handle);
        }),
    );
    assert_eq!(
        stub_answers(CodeEngine::Claude, &path),
        SignIn::SignedIn,
        "the check itself answers"
    );
    let handle = forgetter.lock().unwrap().take().expect("the hook ran");
    handle.join().unwrap();
    assert_eq!(
        known_sign_in(CodeEngine::Claude, &path),
        None,
        "Check again came after the check began: its answer is not kept"
    );
    assert_eq!(stub_answers(CodeEngine::Claude, &path), SignIn::SignedIn);
    assert_eq!(
        std::fs::read_to_string(&count).unwrap().lines().count(),
        2,
        "the next check asked the CLI anew"
    );
}

/// RC2-R610 (usability RC2 round 5, Minor 9: only Claude said "signed in";
/// Codex said "✓ Installed"): `codex login status` answers on stderr
/// ("Logged in using ChatGPT", exit 0; "Not logged in", exit 1). The answer
/// is read there when stdout is empty; a stdout answer (Claude's JSON) is
/// still read alone.
#[test]
fn rc2r610_a_status_said_on_stderr_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let wait = crate::exec_fixture::ANSWER_DEADLINE;
    let codex_in = dir.path().join("codex-in");
    crate::exec_fixture::write_exec_fixture(
        &codex_in,
        "#!/bin/sh\necho 'Logged in using ChatGPT' >&2\nexit 0\n",
        0o755,
    );
    assert_eq!(
        sign_in_state_within(CodeEngine::Codex, codex_in.to_str().unwrap(), wait),
        SignIn::SignedIn
    );
    let codex_out = dir.path().join("codex-out");
    crate::exec_fixture::write_exec_fixture(
        &codex_out,
        "#!/bin/sh\necho 'Not logged in' >&2\nexit 1\n",
        0o755,
    );
    assert_eq!(
        sign_in_state_within(CodeEngine::Codex, codex_out.to_str().unwrap(), wait),
        SignIn::SignedOut
    );
    let claude = dir.path().join("claude-noisy");
    crate::exec_fixture::write_exec_fixture(
        &claude,
        "#!/bin/sh\necho 'a warning' >&2\necho '{\"loggedIn\": true}'\n",
        0o755,
    );
    assert_eq!(
        sign_in_state_within(CodeEngine::Claude, claude.to_str().unwrap(), wait),
        SignIn::SignedIn
    );
}

/// RC2-R610: ZCode and the DeepSeek Harness work with a key kept in a file;
/// whether it is there is their state (its value is never read out).
#[test]
fn rc2r610_a_key_helper_says_whether_its_key_is_set() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let env = "~/.config/foreman/deepseek.conf";
    assert_eq!(
        key_state(CodeEngine::Claude, h, env),
        None,
        "Claude signs in"
    );
    assert_eq!(key_state(CodeEngine::Codex, h, env), None);
    assert_eq!(key_state(CodeEngine::Glm, h, env), Some(SignIn::SignedOut));
    assert_eq!(key_state(CodeEngine::Dsh, h, env), Some(SignIn::SignedOut));

    let zcode = h.join(".zcode/cli/config.json");
    std::fs::create_dir_all(zcode.parent().unwrap()).unwrap();
    std::fs::write(
        &zcode,
        r#"{"provider":{"zai":{"options":{"baseURL":"x","apiKey":""}}}}"#,
    )
    .unwrap();
    assert_eq!(key_state(CodeEngine::Glm, h, env), Some(SignIn::SignedOut));
    std::fs::write(
        &zcode,
        r#"{"provider":{"zai":{"options":{"baseURL":"x","apiKey":"k-123"}}}}"#,
    )
    .unwrap();
    assert_eq!(key_state(CodeEngine::Glm, h, env), Some(SignIn::SignedIn));

    let conf = h.join(".config/foreman/deepseek.conf");
    std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
    std::fs::write(&conf, "# key\nDEEPSEEK_API_KEY=\n").unwrap();
    assert_eq!(key_state(CodeEngine::Dsh, h, env), Some(SignIn::SignedOut));
    std::fs::write(&conf, "export DEEPSEEK_API_KEY=\"sk-abc\"\n").unwrap();
    assert_eq!(key_state(CodeEngine::Dsh, h, env), Some(SignIn::SignedIn));
    let elsewhere = h.join("dsh.env");
    std::fs::write(&elsewhere, "DEEPSEEK_API_KEY=sk-1\n").unwrap();
    assert_eq!(
        key_state(CodeEngine::Dsh, h, elsewhere.to_str().unwrap()),
        Some(SignIn::SignedIn)
    );
    assert_eq!(
        key_steps(CodeEngine::Dsh, env).as_deref(),
        Some(
            "Put your DeepSeek key in ~/.config/foreman/deepseek.conf as DEEPSEEK_API_KEY=\u{2026}"
        )
    );
}

/// The app's own deadline, exercised (DeepSeek, K4b round 4): a hung
/// status command asked with `STATUS_DEADLINE` is given up on at about
/// 3 s — not before it, and not long after (room for the stop).
#[test]
fn the_production_deadline_gives_up_at_three_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("claude");
    // Never `fs::write` an executable a test will exec (ETXTBSY, RC1-G2).
    crate::exec_fixture::write_exec_fixture(&bin, "#!/bin/sh\nexec sleep 30\n", 0o755);
    let path = bin.to_str().unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        sign_in_state_within(CodeEngine::Claude, path, STATUS_DEADLINE),
        SignIn::Unknown
    );
    let took = started.elapsed();
    assert!(
        took >= STATUS_DEADLINE && took < STATUS_DEADLINE + std::time::Duration::from_millis(1500),
        "given up at the 3 s deadline: {took:?}"
    );
}

#[cfg(unix)]
#[test]
fn status_output_overflow_and_inherited_pipe_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let flood = dir.path().join("flood");
    crate::exec_fixture::write_exec_fixture(&flood, "#!/bin/sh\nhead -c 131072 /dev/zero\n", 0o755);
    assert!(ask_cli(flood.to_str().unwrap(), &[], Duration::from_secs(2)).is_none());
    let holder = dir.path().join("holder");
    crate::exec_fixture::write_exec_fixture(&holder, "#!/bin/sh\nsleep 30 &\nprintf 'ok'\n", 0o755);
    let start = Instant::now();
    assert_eq!(
        ask_cli(holder.to_str().unwrap(), &[], Duration::from_secs(2)),
        Some((true, b"ok".to_vec()))
    );
    assert!(start.elapsed() < Duration::from_secs(2));
}
