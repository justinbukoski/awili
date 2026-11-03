#![cfg(unix)] // TODO(windows-phase1): fake-ssh/runner fixtures are shell scripts and modes
use awili_dispatch::protocol::*;

fn request() -> BuildRequest {
    BuildRequest {
        request_id: "00000000-0000-4000-8000-000000000001".into(),
        turn_id: "turn-1".into(),
        conversation_id: "conversation-1".into(),
        repo_id: "fixture".into(),
        baseline_sha: "a".repeat(40),
        branch: "fix/fixture".into(),
        spec_text: "literal $(touch NEVER); `echo NO`\n\"quoted\"".into(),
        target_id: "local".into(),
        verify_profile: "tests".into(),
    }
}

#[test]
fn request_roundtrip_preserves_shell_metacharacters_as_data() {
    let r = request();
    r.validate().unwrap();
    let back: BuildRequest = serde_json::from_slice(&serde_json::to_vec(&r).unwrap()).unwrap();
    assert_eq!(r, back);
    assert_eq!(r.fingerprint(), back.fingerprint());
}

#[test]
fn same_id_different_payload_has_different_fingerprint() {
    let a = request();
    let mut b = a.clone();
    b.spec_text.push('!');
    assert_eq!(a.request_id, b.request_id);
    assert_ne!(a.fingerprint(), b.fingerprint());
}

#[test]
fn reject_unsafe_ids_baselines_and_branches() {
    for id in ["../escape", "id;touch bad", "", "-option"] {
        let mut r = request();
        r.request_id = id.into();
        assert!(r.validate().is_err());
    }
    for branch in [
        "main",
        "fix/../main",
        "fix/a.lock",
        "fix/a b",
        "fix/a;touch",
        "-option",
    ] {
        let mut r = request();
        r.branch = branch.into();
        assert!(r.validate().is_err());
    }
    let mut r = request();
    r.baseline_sha = "HEAD".into();
    assert!(r.validate().is_err());
}

#[test]
fn failure_and_review_states_never_mean_deployed() {
    for state in [
        BuildState::Queued,
        BuildState::Running,
        BuildState::Verifying,
        BuildState::Failed,
        BuildState::CancelRequested,
        BuildState::Stopped,
        BuildState::NeedsAttention,
    ] {
        assert!(!state.ready_for_review());
    }
    assert!(BuildState::AwaitingReview.ready_for_review());
}

#[test]
fn worst_case_json_escaping_fits_shared_wire_limit() {
    let mut request = request();
    request.spec_text = "\u{1}".repeat(96 * 1024);
    request.validate().unwrap();
    let wire = serde_json::to_vec(&request).unwrap();
    assert!(wire.len() > 128 * 1024);
    assert!(wire.len() <= MAX_REQUEST_JSON_BYTES);
    assert_eq!(
        serde_json::from_slice::<BuildRequest>(&wire).unwrap(),
        request
    );
}

/// A one-target configuration for the RC2-R4 checks.
fn rc2r4_config(target: BuildTarget) -> RunnerConfig {
    RunnerConfig {
        job_root: "/srv/awili-build/jobs".into(),
        targets: std::collections::BTreeMap::from([("local".to_string(), target)]),
    }
}

fn rc2r4_target(repo_url: &str) -> BuildTarget {
    let command = CommandSpec {
        program: "/usr/bin/true".into(),
        args: vec!["--flag".into(), "value".into()],
        env: std::collections::BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
    };
    BuildTarget {
        repo_id: "fixture".into(),
        repo_url: repo_url.into(),
        model: "m".into(),
        command: command.clone(),
        verify_profile: "tests".into(),
        verify: command,
        isolation: None,
    }
}

/// RC2-R4 (TOP43 #13): a target names its repository by `ssh://`,
/// `https://`, `file:///` or an absolute path; Git's helper and descriptor
/// transports, plain http/git, scp-style addresses and option-like or
/// spaced values are refused before any job is made.
#[test]
fn rc2r4_a_repository_url_names_only_plain_transports() {
    for ok in [
        "ssh://git@10.77.68.86:2222/owner/lumi.git",
        "https://gitea.example/owner/lumi.git",
        "file:///srv/git/lumi.git",
        "/tmp/fixture/remote.git",
    ] {
        assert!(valid_repo_url(ok).is_ok(), "{ok}");
        assert!(
            rc2r4_config(rc2r4_target(ok)).target(&request()).is_ok(),
            "{ok}"
        );
    }
    for bad in [
        "ext::sh -c touch% /tmp/pwned",
        "ext::ssh",
        "fd::3",
        "http://gitea.example/lumi.git",
        "git://gitea.example/lumi.git",
        "git@gitea.example:owner/lumi.git",
        "-uhack",
        "ssh://host/repo with space",
        "//host/share/repo",
        "file://host/srv/repo.git",
        "relative/repo.git",
        "ssh://",
        "",
    ] {
        assert!(valid_repo_url(bad).is_err(), "{bad:?} accepted");
        assert!(
            rc2r4_config(rc2r4_target(bad)).target(&request()).is_err(),
            "{bad:?} target accepted"
        );
    }
    assert_eq!(GIT_ALLOWED_PROTOCOLS, "ssh:https:file");
}

/// RC2-R4 (TOP43 #11, #12): configured arguments are bounded data without
/// NUL; configured environment names are plain identifiers and never the
/// loader's (`LD_*`, `DYLD_*`, `GCONV_PATH`, `LOCPATH`, `MALLOC_*`), for the
/// model command, the verifier and both isolation profiles.
#[test]
fn rc2r4_loader_variables_and_bad_arguments_are_refused() {
    let base = rc2r4_target("/tmp/fixture/remote.git");
    assert!(rc2r4_config(base.clone()).target(&request()).is_ok());
    for name in [
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "ld_audit",
        "DYLD_INSERT_LIBRARIES",
        "GCONV_PATH",
        "LOCPATH",
        "MALLOC_CONF",
        "1BAD",
        "BAD-NAME",
        "",
    ] {
        for verify in [false, true] {
            let mut t = base.clone();
            let spec = if verify {
                &mut t.verify
            } else {
                &mut t.command
            };
            spec.env.insert(name.to_string(), "x".into());
            assert!(
                rc2r4_config(t).target(&request()).is_err(),
                "{name:?} accepted (verify {verify})"
            );
        }
        assert!(launch_env_name(name).is_err(), "{name:?}");
    }
    for name in ["PATH", "DSH_HOME", "SPARK_API_KEY", "_X1"] {
        assert!(launch_env_name(name).is_ok(), "{name}");
    }
    let profile = |env: &[(&str, &str)]| BwrapProfile {
        bwrap: "/usr/bin/bwrap".into(),
        readonly: vec!["/usr".into()],
        writable: vec![],
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };
    for (model, verifier) in [
        (profile(&[("LD_PRELOAD", "/x.so")]), profile(&[])),
        (profile(&[]), profile(&[("LD_PRELOAD", "/x.so")])),
    ] {
        let mut t = base.clone();
        t.isolation = Some(IsolationSpec {
            bwrap: model,
            verifier,
        });
        assert!(rc2r4_config(t).target(&request()).is_err());
    }
    let mut nul = base.clone();
    nul.command.args.push("a\0b".into());
    assert!(rc2r4_config(nul).target(&request()).is_err());
    let mut many = base.clone();
    many.verify.args = vec!["x".into(); MAX_COMMAND_ARGS + 1];
    assert!(rc2r4_config(many).target(&request()).is_err());
    let mut long = base;
    long.command.args = vec!["x".repeat(MAX_COMMAND_ARG_BYTES + 1)];
    assert!(rc2r4_config(long).target(&request()).is_err());
}

/// RC2-R4 (TOP43 #11): the runner configuration is the administrator's; one
/// that every user can change is refused before anything runs, and a missing
/// one is still "config read failed".
#[test]
fn rc2r4_a_configuration_every_user_can_change_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runner.json");
    let text = serde_json::to_string(&serde_json::json!({
        "job_root": "/srv/awili-build/jobs",
        "targets": {}
    }))
    .unwrap();
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(RunnerConfig::load(&path).is_ok());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    let err = RunnerConfig::load(&path).unwrap_err().to_string();
    assert!(err.contains("writable by every user"), "{err}");
    let missing = dir.path().join("none.json");
    let err = format!("{:#}", RunnerConfig::load(&missing).unwrap_err());
    assert!(err.contains("config read failed"), "{err}");
    let err = format!("{:#}", RunnerConfig::load(dir.path()).unwrap_err());
    assert!(err.contains("config read failed"), "{err}");
}

/// RC2-R4M1: a configuration its group can change (0664, 0660, 0620) is refused
/// just like a world-writable one; group or other read access stays fine.
#[test]
fn rc2r4m1_a_configuration_its_group_can_change_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runner.json");
    let text = serde_json::to_string(&serde_json::json!({
        "job_root": "/srv/awili-build/jobs",
        "targets": {}
    }))
    .unwrap();
    std::fs::write(&path, text).unwrap();
    for ok in [0o600, 0o640, 0o644] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(ok)).unwrap();
        assert!(RunnerConfig::load(&path).is_ok(), "{ok:o} must load");
    }
    for bad in [0o664, 0o660, 0o620] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(bad)).unwrap();
        let err = RunnerConfig::load(&path).unwrap_err().to_string();
        assert!(err.contains("by its group"), "{bad:o}: {err}");
    }
}
