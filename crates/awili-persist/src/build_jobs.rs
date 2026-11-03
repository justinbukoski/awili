//! Durable local Build job correlation (daily-driver P2d).
//!
//! One row per turn: `turn_id` -> the durable runner `request_id`, the
//! configured `target_id`, the latest `revision` and `state` token, the
//! result SHA when known, and the last update time. History is per-turn —
//! there is deliberately NO global "route last" key: two successive turns in
//! one conversation keep independent rows, and a replayed request id can
//! never insert a second row (the unique request index rejects a conflicting
//! payload; the same id for the same turn updates the row in place).
//!
//! The `state` CHECK set mirrors the snake_case tokens of awili-dispatch's
//! `BuildState` plus the two app-local tokens (P10/F4): `submitting` (the row
//! is durable before the runner has acknowledged the submit) and `unknown`
//! (the submit and its same-id `status` fallback both failed — the row stays
//! for reconciliation, never a fresh submit). The persist layer never depends
//! on the dispatch crate, and `Completed` is not a token — a terminal success
//! is `awaiting_review`.

#![forbid(unsafe_code)]

use rusqlite::{params, OptionalExtension};

use crate::Store;

/// The `build_jobs` table and its indexes (the schema v13 addition; the v14
/// rebuild widens the `state` CHECK with the app-local `submitting`/`unknown`
/// tokens and adds the `failure` text).
pub(crate) const BUILD_JOBS_DDL: &str = "
CREATE TABLE IF NOT EXISTS build_jobs (
    turn_id     TEXT PRIMARY KEY,
    request_id  TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    revision    INTEGER NOT NULL,
    state       TEXT NOT NULL CHECK (state IN (
        'submitting', 'unknown',
        'queued', 'running', 'verifying', 'awaiting_review',
        'failed', 'cancel_requested', 'stopped', 'needs_attention')),
    result_sha  TEXT,
    failure     TEXT,
    updated_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_build_jobs_request_id
    ON build_jobs (request_id);
CREATE INDEX IF NOT EXISTS idx_build_jobs_state ON build_jobs (state);
";

/// A row of `build_jobs`, as written or restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildJobRow {
    /// The app turn this build belongs to (the stable turn identity carried
    /// from the Work submit — one row per turn, keyed by this).
    pub turn_id: String,
    /// The durable runner request identity; reconciliation is ALWAYS by this
    /// same id (status of the same id, never a fresh submit).
    pub request_id: String,
    /// The configured `[build_targets.<id>]` binding the run was bound to.
    pub target_id: String,
    /// The latest runner snapshot revision observed for this request.
    pub revision: u64,
    /// The snake_case `BuildState` token (see the CHECK set in the DDL).
    pub state: String,
    /// The resulting commit SHA, once the runner reports it.
    pub result_sha: Option<String>,
    /// The runner's own failure text, when it reported one — or the app-local
    /// submit error for an `unknown` row (P10/F4). Never a synthesized answer.
    pub failure: Option<String>,
    /// Unix seconds of the last state update.
    pub updated_at: i64,
}

impl BuildJobRow {
    /// Whether the job is still OPEN (active on the runner side) and must be
    /// reattached by `status` of the same request id after an app restart.
    /// `awaiting_review` is terminal (the only success); `failed`/`stopped`/
    /// `needs_attention` are terminal non-successes. The app-local
    /// `submitting`/`unknown` rows are OPEN: no runner state was confirmed
    /// yet, so the only safe reconciliation is `status` of the same id.
    pub fn is_open(&self) -> bool {
        matches!(
            self.state.as_str(),
            "submitting" | "unknown" | "queued" | "running" | "verifying" | "cancel_requested"
        )
    }
}

impl Store {
    /// Upsert a build job row keyed by turn. Replaying the SAME request id for
    /// the SAME turn updates the row (revision/state/result/updated_at) and
    /// never inserts a second row; the SAME request id under a DIFFERENT turn
    /// is a conflicting payload and the write fails (unique request index),
    /// leaving every existing row intact.
    pub fn upsert_build_job(&self, row: &BuildJobRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO build_jobs (turn_id, request_id, target_id, revision, state, result_sha, failure, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(turn_id) DO UPDATE SET
                request_id = excluded.request_id,
                target_id = excluded.target_id,
                revision = excluded.revision,
                state = excluded.state,
                result_sha = excluded.result_sha,
                failure = excluded.failure,
                updated_at = excluded.updated_at",
            params![
                row.turn_id,
                row.request_id,
                row.target_id,
                row.revision,
                row.state,
                row.result_sha,
                row.failure,
                row.updated_at
            ],
        )?;
        Ok(())
    }

    /// The build row for one turn, if any (history per turn).
    pub fn build_job_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<Option<BuildJobRow>, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT turn_id, request_id, target_id, revision, state, result_sha, failure, updated_at
                 FROM build_jobs WHERE turn_id = ?1",
                params![turn_id],
                map_build_job_row,
            )
            .optional()
    }

    /// Every OPEN (active-state) row, most recently updated first — the set
    /// the app reattaches by `status` of each row's same request id at
    /// startup. Terminal rows are excluded: they need no reattach. The
    /// app-local `submitting`/`unknown` rows ARE open: no runner state was
    /// confirmed, so reattach reconciles them by `status` of the same id.
    pub fn open_build_jobs(&self) -> Result<Vec<BuildJobRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT turn_id, request_id, target_id, revision, state, result_sha, failure, updated_at
             FROM build_jobs
             WHERE state IN ('submitting', 'unknown', 'queued', 'running', 'verifying', 'cancel_requested')
             ORDER BY updated_at DESC, turn_id ASC",
        )?;
        let rows = stmt
            .query_map([], map_build_job_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Read one `build_jobs` row out of a prepared statement row.
fn map_build_job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BuildJobRow> {
    Ok(BuildJobRow {
        turn_id: row.get(0)?,
        request_id: row.get(1)?,
        target_id: row.get(2)?,
        revision: {
            let v: i64 = row.get(3)?;
            u64::try_from(v).unwrap_or(0)
        },
        state: row.get(4)?,
        result_sha: row.get(5)?,
        failure: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create_schema_v5, migrate_v10_to_v11, migrate_v11_to_v12, migrate_v5_to_v6,
        migrate_v6_to_v7, migrate_v7_to_v8, migrate_v8_to_v9, migrate_v9_to_v10, Connection,
        SCHEMA_VERSION,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_db() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "awili-build-jobs-test-{}-{n}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    fn row(turn: &str, request: &str, state: &str) -> BuildJobRow {
        BuildJobRow {
            turn_id: turn.to_string(),
            request_id: request.to_string(),
            target_id: "hosta".to_string(),
            revision: 1,
            state: state.to_string(),
            result_sha: None,
            failure: None,
            updated_at: 1000,
        }
    }

    /// A v12 file (the pre-P2d layout) opens, gains the `build_jobs` table,
    /// and the stamp moves to the current version.
    #[test]
    fn v12_database_migrates_additively_to_build_jobs() {
        let path = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;",
            )
            .unwrap();
            create_schema_v5(&conn).unwrap();
            migrate_v5_to_v6(&conn).unwrap();
            migrate_v6_to_v7(&conn).unwrap();
            migrate_v7_to_v8(&conn).unwrap();
            migrate_v8_to_v9(&conn).unwrap();
            migrate_v9_to_v10(&conn).unwrap();
            migrate_v10_to_v11(&conn).unwrap();
            migrate_v11_to_v12(&conn).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let v: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        store.upsert_build_job(&row("t1", "r1", "queued")).unwrap();
        assert!(store.open_build_jobs().unwrap().len() == 1);
        drop(store);
        cleanup(&path);
    }

    /// The genuine v13 `build_jobs` shape, frozen verbatim from the P2d commit:
    /// NO `failure` column and the NARROW state CHECK (no app-local tokens).
    /// It is reproduced here because today's `BUILD_JOBS_DDL` (and therefore
    /// `migrate_v12_to_v13`) already carries the v14 shape, so a fixture built
    /// through the current chain would not be a real v13 file.
    const V13_BUILD_JOBS_DDL: &str = "
CREATE TABLE IF NOT EXISTS build_jobs (
    turn_id     TEXT PRIMARY KEY,
    request_id  TEXT NOT NULL,
    target_id   TEXT NOT NULL,
    revision    INTEGER NOT NULL,
    state       TEXT NOT NULL CHECK (state IN (
        'queued', 'running', 'verifying', 'awaiting_review',
        'failed', 'cancel_requested', 'stopped', 'needs_attention')),
    result_sha  TEXT,
    updated_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_build_jobs_request_id
    ON build_jobs (request_id);
CREATE INDEX IF NOT EXISTS idx_build_jobs_state ON build_jobs (state);
";

    /// The v13→v14 rebuild (create/copy/drop/rename) is lossless on a
    /// POPULATED v13 table: all three rows survive with every original column
    /// value byte-for-byte, the new `failure` column is NULL on each, the
    /// stamp is v14, and the widened CHECK accepts the app-local
    /// `submitting`/`unknown` tokens while still rejecting an arbitrary string.
    #[test]
    fn populated_v13_database_migrates_losslessly_to_v14() {
        let path = temp_db();
        // A genuine v13 file: the real chain through v12, then the frozen v13
        // build_jobs DDL and an explicit v13 stamp.
        {
            let conn = Connection::open(&path).unwrap();
            create_schema_v5(&conn).unwrap();
            migrate_v5_to_v6(&conn).unwrap();
            migrate_v6_to_v7(&conn).unwrap();
            migrate_v7_to_v8(&conn).unwrap();
            migrate_v8_to_v9(&conn).unwrap();
            migrate_v9_to_v10(&conn).unwrap();
            migrate_v10_to_v11(&conn).unwrap();
            migrate_v11_to_v12(&conn).unwrap();
            conn.execute_batch(&format!(
                "BEGIN;
                 {V13_BUILD_JOBS_DDL}
                 PRAGMA user_version = 13;
                 COMMIT;"
            ))
            .unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 13, "fixture must be a genuine v13 DB");

            // The narrow v13 CHECK must still reject the P10 tokens — proof
            // the migration, not the fixture, widens the constraint.
            assert!(
                conn.execute(
                    "INSERT INTO build_jobs
                         (turn_id, request_id, target_id, revision, state, result_sha, updated_at)
                     VALUES ('pre', 'pre', 'pre', 0, 'submitting', NULL, 0)",
                    [],
                )
                .is_err(),
                "v13 must not accept the app-local tokens yet"
            );

            // Three rows covering OPEN, terminal success and terminal
            // non-success; every v13 column carries a distinct value.
            conn.execute_batch(
                "INSERT INTO build_jobs
                     (turn_id, request_id, target_id, revision, state, result_sha, updated_at)
                 VALUES
                     ('turn-open',   'req-open',   'target-alpha', 11, 'running',         'sha-open',   1000),
                     ('turn-review', 'req-review', 'target-beta',  22, 'awaiting_review', 'sha-review', 2000),
                     ('turn-failed', 'req-failed', 'target-gamma', 33, 'failed',          'sha-failed', 3000);",
            )
            .unwrap();
        }

        // The migration under test: opening a v13 file runs exactly v13→v14.
        let store = Store::open(&path).unwrap();

        let v: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "v13 must land on v14");
        assert_eq!(v, 14);

        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM build_jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3, "the rebuild must copy every row");

        // Whole-row equality: every original column survives and `failure`,
        // which v13 never held, comes back NULL.
        let expected = [
            BuildJobRow {
                turn_id: "turn-open".to_string(),
                request_id: "req-open".to_string(),
                target_id: "target-alpha".to_string(),
                revision: 11,
                state: "running".to_string(),
                result_sha: Some("sha-open".to_string()),
                failure: None,
                updated_at: 1000,
            },
            BuildJobRow {
                turn_id: "turn-review".to_string(),
                request_id: "req-review".to_string(),
                target_id: "target-beta".to_string(),
                revision: 22,
                state: "awaiting_review".to_string(),
                result_sha: Some("sha-review".to_string()),
                failure: None,
                updated_at: 2000,
            },
            BuildJobRow {
                turn_id: "turn-failed".to_string(),
                request_id: "req-failed".to_string(),
                target_id: "target-gamma".to_string(),
                revision: 33,
                state: "failed".to_string(),
                result_sha: Some("sha-failed".to_string()),
                failure: None,
                updated_at: 3000,
            },
        ];
        for want in &expected {
            let got = store.build_job_for_turn(&want.turn_id).unwrap();
            assert_eq!(
                got.as_ref(),
                Some(want),
                "row {} must survive the v13→v14 rebuild unchanged",
                want.turn_id
            );
            assert!(
                got.unwrap().failure.is_none(),
                "migrated v13 row {} must have failure NULL",
                want.turn_id
            );
        }
        // The open row reattaches; the terminal rows do not.
        let open = store.open_build_jobs().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].turn_id, "turn-open");

        // The widened v14 CHECK accepts the app-local tokens...
        store
            .upsert_build_job(&row("turn-submitting", "req-submitting", "submitting"))
            .unwrap();
        store
            .upsert_build_job(&row("turn-unknown", "req-unknown", "unknown"))
            .unwrap();
        assert_eq!(
            store
                .build_job_for_turn("turn-submitting")
                .unwrap()
                .unwrap()
                .state,
            "submitting"
        );
        assert_eq!(
            store
                .build_job_for_turn("turn-unknown")
                .unwrap()
                .unwrap()
                .state,
            "unknown"
        );
        // ...and still rejects an arbitrary string.
        let err = store
            .upsert_build_job(&row("turn-bogus", "req-bogus", "definitely-not-a-state"))
            .unwrap_err();
        assert!(
            matches!(
                err,
                rusqlite::Error::SqliteFailure(e, _)
                    if e.code == rusqlite::ErrorCode::ConstraintViolation
            ),
            "the v14 CHECK must still reject an unknown token: {err}"
        );
        assert!(store.build_job_for_turn("turn-bogus").unwrap().is_none());

        drop(store);
        cleanup(&path);
    }

    /// Replaying the same request id for the same turn updates the row in
    /// place: one row, latest revision/state — never a second row.
    #[test]
    fn replay_same_request_id_updates_without_second_row() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.upsert_build_job(&row("t1", "r1", "queued")).unwrap();
        let mut updated = row("t1", "r1", "running");
        updated.revision = 7;
        updated.result_sha = Some("abc123".into());
        store.upsert_build_job(&updated).unwrap();
        let rows = store.open_build_jobs().unwrap();
        assert_eq!(rows.len(), 1, "a replay must never add a row");
        assert_eq!(rows[0].revision, 7);
        assert_eq!(rows[0].state, "running");
        assert_eq!(rows[0].result_sha.as_deref(), Some("abc123"));
        drop(store);
        cleanup(&path);
    }

    /// The same request id under a DIFFERENT turn is a conflicting payload:
    /// the write fails and no second row appears.
    #[test]
    fn duplicate_request_id_with_conflicting_payload_fails() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.upsert_build_job(&row("t1", "r1", "queued")).unwrap();
        let err = store
            .upsert_build_job(&row("t2", "r1", "queued"))
            .unwrap_err();
        assert!(
            matches!(
                err,
                rusqlite::Error::SqliteFailure(e, _)
                    if e.code == rusqlite::ErrorCode::ConstraintViolation
            ),
            "conflicting payload must be a constraint failure: {err}"
        );
        // Both reads see exactly the original single row.
        assert_eq!(
            store.build_job_for_turn("t1").unwrap().unwrap().state,
            "queued"
        );
        assert!(store.build_job_for_turn("t2").unwrap().is_none());
        assert_eq!(store.open_build_jobs().unwrap().len(), 1);
        drop(store);
        cleanup(&path);
    }

    /// Open rows survive a simulated restart (drop + reopen) and come back as
    /// the reattach set; terminal rows do not.
    #[test]
    fn open_rows_survive_restart_and_exclude_terminal() {
        let path = temp_db();
        let r1 = row("t1", "r1", "running");
        let r2 = row("t2", "r2", "failed");
        {
            let store = Store::open(&path).unwrap();
            store.upsert_build_job(&r1).unwrap();
            store.upsert_build_job(&r2).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let open = store.open_build_jobs().unwrap();
        assert_eq!(open.len(), 1, "only the active row reattaches");
        assert_eq!(open[0].turn_id, "t1");
        assert_eq!(open[0].request_id, "r1");
        assert!(open[0].is_open());
        // The failed row's history is retained per turn.
        let failed = store.build_job_for_turn("t2").unwrap().unwrap();
        assert!(!failed.is_open());
        drop(store);
        cleanup(&path);
    }

    /// Two successive turns in one conversation keep independent rows, and
    /// updating one never touches the other.
    #[test]
    fn successive_turns_keep_independent_rows() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.upsert_build_job(&row("t1", "r1", "queued")).unwrap();
        store.upsert_build_job(&row("t2", "r2", "queued")).unwrap();
        let mut done = row("t1", "r1", "failed");
        done.revision = 4;
        store.upsert_build_job(&done).unwrap();
        let t1 = store.build_job_for_turn("t1").unwrap().unwrap();
        let t2 = store.build_job_for_turn("t2").unwrap().unwrap();
        assert_eq!(t1.state, "failed");
        assert_eq!(t1.revision, 4);
        assert_eq!(t2.state, "queued", "the other turn's row is untouched");
        assert_eq!(store.open_build_jobs().unwrap().len(), 1);
        drop(store);
        cleanup(&path);
    }

    /// A state outside the BuildState token set is rejected by the CHECK
    /// constraint — in particular `completed`, which does not exist.
    #[test]
    fn unknown_state_token_is_rejected() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        assert!(store
            .upsert_build_job(&row("t1", "r1", "completed"))
            .is_err());
        assert!(store.upsert_build_job(&row("t2", "r2", "garbage")).is_err());
        drop(store);
        cleanup(&path);
    }

    /// P10/F4: the app-local `submitting`/`unknown` tokens are accepted, are
    /// OPEN (reattach reconciles them by `status` of the same id), and the
    /// `unknown` row carries the submit error text in `failure`.
    #[test]
    fn app_local_states_are_open_and_persist_failure() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store
            .upsert_build_job(&row("t1", "r1", "submitting"))
            .unwrap();
        let mut unknown = row("t2", "r2", "unknown");
        unknown.failure = Some("submission uncertain: reconcile this request ID".into());
        store.upsert_build_job(&unknown).unwrap();
        let open = store.open_build_jobs().unwrap();
        assert_eq!(open.len(), 2, "both app-local states are open");
        assert!(open.iter().all(|r| r.is_open()));
        let t2 = store.build_job_for_turn("t2").unwrap().unwrap();
        assert_eq!(t2.state, "unknown");
        assert_eq!(t2.request_id, "r2", "the same id is retained");
        assert_eq!(
            t2.failure.as_deref(),
            Some("submission uncertain: reconcile this request ID")
        );
        drop(store);
        cleanup(&path);
    }
}
