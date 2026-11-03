#![cfg(unix)] // TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
use awili_dispatch::{protocol::*, ssh::SshClient};
use std::{fs, os::unix::fs::PermissionsExt};

mod support;

// Linux fork can inherit another test's writable script descriptor until exec.
// Serialize script creation/spawn to avoid unrelated ETXTBSY fixture failures.
static SCRIPT_FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The fixture lock, poison-tolerant: one failing test must not fail the
/// others through a poisoned mutex.
fn fixture_lock() -> std::sync::MutexGuard<'static, ()> {
    support::lock(&SCRIPT_FIXTURE)
}

// Stand-ins that must answer get `support::ANSWER_DEADLINE`; the
// deadline tests keep their own short ones.
use support::ANSWER_DEADLINE;

#[test]
fn lost_acknowledgement_reconciles_same_request_without_resubmission() {
    let _fixture = fixture_lock();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let request = BuildRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        turn_id: "t".into(),
        conversation_id: "c".into(),
        repo_id: "r".into(),
        baseline_sha: "a".repeat(40),
        branch: "fix/ssh-fixture".into(),
        spec_text: "$(touch NEVER); `touch NEVER2`\nexact input".into(),
        target_id: "local".into(),
        verify_profile: "test".into(),
    };
    let snapshot = BuildSnapshot {
        request_id: request.request_id.clone(),
        request_sha256: request.fingerprint(),
        revision: 1,
        state: BuildState::Queued,
        executor: "dsh".into(),
        requested_model: "fixture-model".into(),
        baseline_sha: request.baseline_sha.clone(),
        result_sha: None,
        remote_sha: None,
        model_exit: None,
        verify_exit: None,
        push_exit: None,
        worker: None,
        failure: None,
        no_op: false,
    };
    fs::write(
        root.join("snapshot.json"),
        serde_json::to_vec(&snapshot).unwrap(),
    )
    .unwrap();
    let script = root.join("ssh");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
+for arg do operation="$arg"; done
+printf '%s\n' "$operation" >> '{0}/calls'
+printf '%s\n' "$@" >> '{0}/argv'
+cat > '{0}/last-request'
+case "$operation" in
+ submit) cp '{0}/last-request' '{0}/submitted'; exit 255;;
+ status) cat '{0}/snapshot.json';;
+ *) exit 17;;
+esac
+"#,
            root.display()
        )
        .replace("\n+", "\n"),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let client = SshClient {
        ssh: script,
        host: "configured-host".into(),
        runner: "/opt/awili/runner".into(),
        config: "/etc/awili/build.json".into(),
        timeout: ANSWER_DEADLINE,
    };
    let actual = client.submit(&request).unwrap();
    assert_eq!(actual.request_id, request.request_id);
    assert_eq!(
        fs::read_to_string(root.join("calls")).unwrap(),
        "submit\nstatus\n"
    );
    let submitted: BuildRequest =
        serde_json::from_slice(&fs::read(root.join("submitted")).unwrap()).unwrap();
    assert_eq!(submitted, request);
    let status_query: JobQuery =
        serde_json::from_slice(&fs::read(root.join("last-request")).unwrap()).unwrap();
    assert_eq!(status_query.request_id, request.request_id);
    assert!(!root.join("NEVER").exists());
    let mut forged = snapshot;
    forged.request_sha256 = "wrong".into();
    fs::write(
        root.join("snapshot.json"),
        serde_json::to_vec(&forged).unwrap(),
    )
    .unwrap();
    assert!(
        client.submit(&request).is_err(),
        "conflicting response cannot be accepted"
    );
}

#[test]
fn unsafe_remote_command_configuration_makes_no_ssh_call() {
    let client = SshClient {
        ssh: "/definitely/not/invoked".into(),
        host: "host;touch bad".into(),
        runner: "/runner".into(),
        config: "/config".into(),
        timeout: std::time::Duration::from_secs(3),
    };
    let error = client
        .status("00000000-0000-4000-8000-000000000001")
        .unwrap_err()
        .to_string();
    assert!(error.contains("unsafe SSH host"));
}

#[test]
fn hung_transport_has_bounded_deadline_without_reissuing_submit() {
    let _fixture = fixture_lock();
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ssh");
    fs::write(&script, "#!/bin/sh\nsleep 10\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let client = SshClient {
        ssh: script,
        host: "host".into(),
        runner: "/runner".into(),
        config: "/config".into(),
        timeout: std::time::Duration::from_millis(100),
    };
    let before = std::time::Instant::now();
    let error = client
        .status("00000000-0000-4000-8000-000000000001")
        .unwrap_err()
        .to_string();
    assert!(error.contains("deadline exceeded"), "{error}");
    assert!(before.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn exited_parent_with_inherited_or_detached_pipe_cannot_outlive_deadline() {
    let _fixture = fixture_lock();
    for detached in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("ssh");
        let pidfile = temp.path().join("child.pid");
        // The parent exits immediately; the descendant deliberately retains both pipes
        // (stdin is redirected explicitly: a non-job-control `&` would give it /dev/null).
        // Detached: `set -m` puts the descendant in its own process group, out of reach
        // of a signal to the parent's group, as the old os.setsid() did.
        fs::write(
            &script,
            format!(
                "#!/bin/bash\n{}sleep 30 <&0 &\nprintf '%s' \"$!\" > '{}'\nexit 0\n",
                if detached { "set -m\n" } else { "" },
                pidfile.to_str().unwrap()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let client = SshClient {
            ssh: script,
            host: "host".into(),
            runner: "/runner".into(),
            config: "/config".into(),
            timeout: std::time::Duration::from_millis(300),
        };
        let before = std::time::Instant::now();
        let result = client.status("00000000-0000-4000-8000-000000000001");
        let elapsed = before.elapsed();
        // Test owns this explicit fixture PID. Transport never signals an unverifiable reaped group.
        if let Ok(pid) = fs::read_to_string(pidfile) {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-KILL", pid.trim()])
                .status();
        }
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("deadline exceeded"));
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "elapsed {elapsed:?}"
        );
    }
}

#[test]
fn status_and_cancel_reject_a_different_job_response() {
    let _fixture = fixture_lock();
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("ssh");
    let snapshot = serde_json::json!({"request_id":"00000000-0000-4000-8000-000000000002", "request_sha256":"x", "revision":1, "state":"queued", "executor":"fixture", "requested_model":"fixture", "baseline_sha":"a", "result_sha":null, "remote_sha":null, "model_exit":null, "verify_exit":null, "push_exit":null, "worker":null, "failure":null, "no_op":false});
    fs::write(
        &script,
        format!("#!/bin/sh\ncat >/dev/null\nprintf '%s' '{}'\n", snapshot),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let client = SshClient {
        ssh: script,
        host: "host".into(),
        runner: "/runner".into(),
        config: "/config".into(),
        timeout: ANSWER_DEADLINE,
    };
    let id = "00000000-0000-4000-8000-000000000001";
    for result in [client.status(id), client.cancel(id)] {
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("request ID mismatch"),
            "unexpected error: {error}"
        );
    }
}
