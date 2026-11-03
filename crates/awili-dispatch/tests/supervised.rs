#![cfg(unix)] // TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
use awili_dispatch::{protocol::*, supervised::*};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
};
mod support;

fn request() -> BuildRequest {
    BuildRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        turn_id: "turn".into(),
        conversation_id: "conv".into(),
        repo_id: "repo".into(),
        baseline_sha: "a".repeat(40),
        branch: "fix/supervised-test".into(),
        spec_text: "café\n\u{1} $(touch NEVER)".into(),
        target_id: "smoke".into(),
        verify_profile: "test".into(),
    }
}
#[test]
fn shared_read_only_validation_preserves_exact_request_without_creating_job_root() {
    let tmp = tempfile::tempdir().unwrap();
    let request = request();
    let config = RunnerConfig {
        job_root: tmp.path().join("absent-jobs"),
        targets: [(
            "smoke".into(),
            BuildTarget {
                repo_id: "repo".into(),
                // RC2-R4 (TOP43 #13): a repository is named by a plain
                // transport or an absolute path; a bare word is refused.
                repo_url: "/srv/git/fixture.git".into(),
                model: "fixture".into(),
                command: CommandSpec {
                    program: "/bin/true".into(),
                    args: vec![],
                    env: Default::default(),
                },
                verify_profile: "test".into(),
                verify: CommandSpec {
                    program: "/bin/true".into(),
                    args: vec![],
                    env: Default::default(),
                },
                isolation: None,
            },
        )]
        .into(),
    };
    let path = tmp.path().join("config.json");
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    // Installed as the deploy does (`install -m 0644`), not left to the
    // umask: under umask 0002 the runner rightly refuses a group-writable
    // configuration (RC2-R4M1).
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let run = |request: &BuildRequest| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_awili-build-runner"))
            .args(["--config", path.to_str().unwrap(), "validate"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(request).unwrap())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let output = run(&request);
    assert!(output.status.success());
    let receipt: ValidatedRequest = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt.request, request);
    assert_eq!(receipt.request_sha256, request.fingerprint());
    assert!(!config.job_root.exists());
    let mut invalid = request;
    invalid.target_id = "unconfigured".into();
    assert!(!run(&invalid).status.success());
    assert!(!config.job_root.exists());
}

#[test]
fn supervised_lost_ack_is_status_only_and_responses_are_correlated() {
    let tmp = tempfile::tempdir().unwrap();
    let req = request();
    let mut envelope = SupervisedSnapshot {
        request_id: req.request_id.clone(),
        request_sha256: req.fingerprint(),
        supervisor_state: SupervisorState::Starting,
        snapshot: None,
        termination_confirmed: false,
        diagnostic: None,
    };
    let path = tmp.path().join("response");
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    let script = tmp.path().join("ssh");
    // Through cp, never fs::write: a sibling test forking the runner while
    // this file is open for writing makes its exec fail with ETXTBSY, and the
    // client then (rightly) treats the lost submit as uncertain and asks only
    // for status, so "calls" read "status" (seen under load on hosta).
    support::write_exec_fixture(&script, format!("#!/bin/sh\nfor arg do op=\"$arg\"; done\nprintf '%s\\n' \"$op\" >> '{0}/calls'\ncat > '{0}/payload'\nif [ \"$op\" = submit ]; then exit 255; fi\ncat '{0}/response'\n",tmp.path().display()), 0o755);
    let client =
        SupervisedSshClient::new(script, "configured-host".into(), support::ANSWER_DEADLINE);
    assert_eq!(
        client.submit(&req).unwrap().supervisor_state,
        SupervisorState::Starting
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("calls")).unwrap(),
        "submit\nstatus\n"
    );
    let query: JobQuery =
        serde_json::from_slice(&fs::read(tmp.path().join("payload")).unwrap()).unwrap();
    assert_eq!(query.request_id, req.request_id);
    envelope.request_sha256 = "b".repeat(64);
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert!(client.status(&req).is_err());
    envelope.request_sha256 = req.fingerprint();
    envelope.supervisor_state = SupervisorState::Stopped;
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert!(client.cancel(&req).is_err());
    envelope.supervisor_state = SupervisorState::Terminal;
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert!(client.status(&req).is_err());
}
