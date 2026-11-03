//! P2e unit tests for `work_last_run` (split out to keep the module under
//! the 400-line rule).

use super::*;

fn all_identities() -> Vec<WorkRunIdentity> {
    vec![
        WorkRunIdentity::Agent {
            engine: CodeEngine::Claude,
            model: Some("sonnet".into()),
        },
        WorkRunIdentity::Agent {
            engine: CodeEngine::Codex,
            model: None,
        },
        WorkRunIdentity::Agent {
            engine: CodeEngine::Glm,
            model: Some("glm-4.7".into()),
        },
        WorkRunIdentity::Agent {
            engine: CodeEngine::Dsh,
            model: None,
        },
        WorkRunIdentity::HermesGateway,
        WorkRunIdentity::OwnedEngine {
            model: "local-model".into(),
            engine_id: None,
        },
        WorkRunIdentity::LocalBuild {
            model: "build-model".into(),
            host: "ssh-host".into(),
        },
    ]
}

fn terminal_statuses() -> Vec<RunStatus> {
    vec![RunStatus::Completed, RunStatus::Failed, RunStatus::Stopped]
}

#[test]
fn last_run_round_trips_through_json() {
    for identity in all_identities() {
        for status in terminal_statuses() {
            let last = LastRun {
                identity: identity.clone(),
                model: Some("reported-model".into()),
                status,
                error: Some("boom".into()),
                finished_at: "2026-09-24T10:00:00+00:00".into(),
            };
            let json = last.to_json();
            let back = LastRun::from_json(&json).expect("stored payload parses");
            assert_eq!(back, last, "round trip {json}");
            // Also through a serde_json string (the app_state row value is
            // a JSON string inside the key/value cell).
            let stringified = serde_json::to_string(&json).unwrap();
            let unstringified: String = serde_json::from_str(&stringified).unwrap();
            assert_eq!(LastRun::from_json(&unstringified), Some(last));
        }
    }
}

#[test]
fn key_is_per_conversation() {
    assert_ne!(key_for("conv-a"), key_for("conv-b"));
    assert_eq!(
        key_for("conv-a"),
        format!("{WORK_LAST_RUN_KEY_PREFIX}conv-a")
    );
    // Two different conversations keep independent rows.
    let last_a = LastRun {
        identity: WorkRunIdentity::HermesGateway,
        model: None,
        status: RunStatus::Completed,
        error: None,
        finished_at: "2026-09-24T10:00:00+00:00".into(),
    };
    let last_b = LastRun {
        identity: WorkRunIdentity::OwnedEngine {
            model: "m".into(),
            engine_id: None,
        },
        model: None,
        status: RunStatus::Failed,
        error: Some("x".into()),
        finished_at: "2026-09-24T11:00:00+00:00".into(),
    };
    // Distinct keys; restoring under one key never touches the other's.
    assert_ne!(key_for("conv-a"), key_for("conv-b"));
    assert_eq!(LastRun::from_json(&last_a.to_json()), Some(last_a));
    assert_eq!(LastRun::from_json(&last_b.to_json()), Some(last_b));
}

/// A worker over a fresh temp sqlite store, for the write-path tests; the
/// returned guard removes the files when dropped.
fn temp_persist(tag: &str) -> (Persistence, std::thread::JoinHandle<()>, TempDb) {
    let path = std::env::temp_dir().join(format!(
        "awili-app-last-run-unit-{tag}-{}.db",
        std::process::id()
    ));
    let store = awili_persist::Store::open(&path).expect("temp store opens");
    let (persist, worker) = Persistence::spawn_worker(store);
    (persist, worker, TempDb(path))
}

struct TempDb(std::path::PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("db-wal"));
        let _ = std::fs::remove_file(self.0.with_extension("db-shm"));
    }
}

#[test]
fn error_only_panel_persists_as_failed() {
    let (persist, worker, _db) = temp_persist("error-only");
    // The `fail_local_build` shape: identity kept, status reset, error set.
    let panel = RunPanel {
        requested_identity: Some(WorkRunIdentity::LocalBuild {
            model: "build-model".into(),
            host: "ssh-host".into(),
        }),
        status: None,
        error: Some("runner unreachable".into()),
        ..RunPanel::default()
    };
    let json = persist_last_run(
        Some(&persist),
        &mut HashMap::new(),
        "conv-e",
        &panel,
        Utc::now(),
    )
    .expect("a terminal error-only panel is written");
    let last = LastRun::from_json(&json).expect("queued payload parses");
    assert_eq!(last.status, RunStatus::Failed);
    assert_eq!(last.error.as_deref(), Some("runner unreachable"));

    // A non-terminal panel writes nothing.
    let running = RunPanel {
        status: Some(RunStatus::Running),
        ..panel
    };
    assert!(persist_last_run(
        Some(&persist),
        &mut HashMap::new(),
        "conv-e",
        &running,
        Utc::now()
    )
    .is_none());
    drop(persist);
    let _ = worker.join();
}

#[test]
fn persist_updates_map_before_readback() {
    let (persist, worker, _db) = temp_persist("map-update");
    // The map as captured at startup: conv X's OLDER run.
    let older = LastRun {
        identity: WorkRunIdentity::HermesGateway,
        model: None,
        status: RunStatus::Failed,
        error: Some("old".into()),
        finished_at: "2026-09-23T10:00:00+00:00".into(),
    };
    let mut map: HashMap<String, String> = [("conv-x".to_string(), older.to_json())]
        .into_iter()
        .collect();

    // A newer terminal run in X through the production helper, which is
    // what updates the map (Codex round-2 F1: the test must not insert).
    let panel = RunPanel {
        requested_identity: Some(WorkRunIdentity::Agent {
            engine: CodeEngine::Codex,
            model: Some("gpt-5".into()),
        }),
        model: Some("reported".into()),
        status: Some(RunStatus::Completed),
        ..RunPanel::default()
    };
    assert!(persist_last_run(Some(&persist), &mut map, "conv-x", &panel, Utc::now()).is_some());

    // No store reopen: the map alone now returns the NEW run.
    let stored = stored_last_run("conv-x", &map).expect("row present");
    assert_eq!(
        stored.identity,
        panel.requested_identity.clone().expect("set")
    );
    assert_eq!(stored.model.as_deref(), Some("reported"));
    assert_eq!(stored.status, RunStatus::Completed);
    assert_eq!(stored.error, None);
    drop(persist);
    let _ = worker.join();
}
