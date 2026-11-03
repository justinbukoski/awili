//! RC2-CNCL2: what a Council keeps beyond its messages — who sits on it (in
//! column order) and one row per member per round (answered, stopped or
//! failed, with time, words and cost).
//!
//! The answers themselves stay in `messages` (kind `council_answer`), so
//! search, export and the sidebar keep working; these two tables only add the
//! council facts. They are created with `IF NOT EXISTS` on every open — purely
//! additive, no `user_version` step, so a database stays readable by an older
//! build (it simply ignores the tables).

use rusqlite::OptionalExtension;

use crate::Store;

/// The two council tables and their index. Idempotent.
pub(crate) const COUNCIL_DDL: &str = "
    CREATE TABLE IF NOT EXISTS council_members (
        conversation_id TEXT NOT NULL REFERENCES conversations(id),
        member_id       TEXT NOT NULL,
        position        INTEGER NOT NULL,
        route           TEXT NOT NULL,
        label           TEXT NOT NULL,
        added_ts        INTEGER NOT NULL,
        removed_ts      INTEGER,
        PRIMARY KEY (conversation_id, member_id)
    );
    CREATE TABLE IF NOT EXISTS council_turns (
        id               TEXT PRIMARY KEY,
        conversation_id  TEXT NOT NULL REFERENCES conversations(id),
        round            INTEGER NOT NULL,
        round_kind       TEXT NOT NULL,
        owner_ts         INTEGER,
        member_id        TEXT NOT NULL,
        route            TEXT NOT NULL,
        label            TEXT NOT NULL,
        message_id       INTEGER,
        status           TEXT NOT NULL,
        stance           TEXT,
        error_plain      TEXT,
        error_detail     TEXT,
        needs_owner      INTEGER NOT NULL DEFAULT 0,
        started_ts       INTEGER NOT NULL,
        first_token_ms   INTEGER,
        elapsed_ms       INTEGER,
        input_tokens     INTEGER,
        output_tokens    INTEGER,
        cost_usd         REAL,
        sent_prompt_hash TEXT,
        sent_system      TEXT,
        sent_text        TEXT,
        tools_used       TEXT
    );
    CREATE INDEX IF NOT EXISTS council_turns_conv ON council_turns(conversation_id, round);
";

/// One council seat. `removed_ts` is set when the owner takes the member off
/// the council; the row stays so earlier answers still name who gave them.
#[derive(Clone, Debug, PartialEq)]
pub struct CouncilMemberRow {
    pub conversation_id: String,
    /// Stable per council ("m1", "m2", …).
    pub member_id: String,
    /// Column order.
    pub position: i64,
    /// The route as persisted (`agent:claude:sonnet`, `in-app`, `provider:fleet`, …).
    pub route: String,
    /// The name shown at the time ("Claude Sonnet").
    pub label: String,
    pub added_ts: i64,
    pub removed_ts: Option<i64>,
}

/// One member's turn in one round — kept for every outcome, including a
/// failure that wrote no message.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CouncilTurnRow {
    pub id: String,
    pub conversation_id: String,
    /// 1.. ; each owner message, "answer each other" or sum-up opens a round.
    pub round: i64,
    /// `ask` | `respond` | `sum_up`.
    pub round_kind: String,
    /// The `ts` of the owner's message this round answers (`None` for a
    /// round the owner opened with a button).
    pub owner_ts: Option<i64>,
    pub member_id: String,
    /// Frozen at send time.
    pub route: String,
    /// Frozen at send time.
    pub label: String,
    /// The answer's row in `messages`; `None` when nothing was written.
    pub message_id: Option<i64>,
    /// `answered` | `stopped` | `failed`.
    pub status: String,
    /// `same` | `changed` | `partly`.
    pub stance: Option<String>,
    /// The sentence the card shows for a failure.
    pub error_plain: Option<String>,
    /// The raw error, for Details.
    pub error_detail: Option<String>,
    /// The owner must act outside the council (signed out, key missing).
    pub needs_owner: bool,
    pub started_ts: i64,
    pub first_token_ms: Option<i64>,
    pub elapsed_ms: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    /// `None` = included in a subscription, or free.
    pub cost_usd: Option<f64>,
    pub sent_prompt_hash: Option<String>,
    /// RC2-CNCLF3: exactly what the member was sent — the system text and
    /// the message — so Retry sends it again byte for byte after a relaunch.
    pub sent_system: Option<String>,
    pub sent_text: Option<String>,
    /// RC2-R2F5: the tools the member used for this answer, one per line
    /// (its card's "Used:" line and Details).
    pub tools_used: Option<String>,
}

/// RC2-CNCLF3: a database whose council tables predate the sent-prompt
/// columns (RC2-R2F5: and the tools-used one) gets them (additive; no
/// version step, older builds ignore them).
pub(crate) fn add_sent_prompt_columns(conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
    let mut stmt = conn.prepare("PRAGMA table_info(council_turns)")?;
    let have: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    for col in ["sent_system", "sent_text", "tools_used"] {
        if !have.iter().any(|c| c == col) {
            conn.execute_batch(&format!("ALTER TABLE council_turns ADD COLUMN {col} TEXT;"))?;
        }
    }
    Ok(())
}

const TURN_COLUMNS: &str = "id, conversation_id, round, round_kind, owner_ts, member_id, route, \
     label, message_id, status, stance, error_plain, error_detail, needs_owner, started_ts, \
     first_token_ms, elapsed_ms, input_tokens, output_tokens, cost_usd, sent_prompt_hash, \
     sent_system, sent_text, tools_used";

fn turn_from_row(r: &rusqlite::Row<'_>) -> Result<CouncilTurnRow, rusqlite::Error> {
    Ok(CouncilTurnRow {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        round: r.get(2)?,
        round_kind: r.get(3)?,
        owner_ts: r.get(4)?,
        member_id: r.get(5)?,
        route: r.get(6)?,
        label: r.get(7)?,
        message_id: r.get(8)?,
        status: r.get(9)?,
        stance: r.get(10)?,
        error_plain: r.get(11)?,
        error_detail: r.get(12)?,
        needs_owner: r.get::<_, i64>(13)? != 0,
        started_ts: r.get(14)?,
        first_token_ms: r.get(15)?,
        elapsed_ms: r.get(16)?,
        input_tokens: r.get(17)?,
        output_tokens: r.get(18)?,
        cost_usd: r.get(19)?,
        sent_prompt_hash: r.get(20)?,
        sent_system: r.get(21)?,
        sent_text: r.get(22)?,
        tools_used: r.get(23)?,
    })
}

impl Store {
    /// Add (or update) a council seat. Its own committed transaction.
    pub fn put_council_member(&self, m: &CouncilMemberRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO council_members
                 (conversation_id, member_id, position, route, label, added_ts, removed_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(conversation_id, member_id) DO UPDATE SET
                 position = excluded.position, route = excluded.route,
                 label = excluded.label, removed_ts = excluded.removed_ts",
            rusqlite::params![
                m.conversation_id,
                m.member_id,
                m.position,
                m.route,
                m.label,
                m.added_ts,
                m.removed_ts
            ],
        )?;
        Ok(())
    }

    /// Take a member off the council. The row and its turns stay.
    pub fn remove_council_member(
        &self,
        conversation_id: &str,
        member_id: &str,
        removed_ts: i64,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE council_members SET removed_ts = ?3
             WHERE conversation_id = ?1 AND member_id = ?2",
            rusqlite::params![conversation_id, member_id, removed_ts],
        )?;
        Ok(())
    }

    /// Every seat of a council (removed ones included), in column order.
    pub fn council_members(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<CouncilMemberRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT conversation_id, member_id, position, route, label, added_ts, removed_ts
             FROM council_members WHERE conversation_id = ?1
             ORDER BY position ASC, member_id ASC",
        )?;
        let rows = stmt.query_map([conversation_id], |r| {
            Ok(CouncilMemberRow {
                conversation_id: r.get(0)?,
                member_id: r.get(1)?,
                position: r.get(2)?,
                route: r.get(3)?,
                label: r.get(4)?,
                added_ts: r.get(5)?,
                removed_ts: r.get(6)?,
            })
        })?;
        rows.collect()
    }

    /// Write (or replace, by id) one turn row. A retry replaces its turn.
    pub fn put_council_turn(&self, t: &CouncilTurnRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            &format!(
                "INSERT OR REPLACE INTO council_turns ({TURN_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)"
            ),
            rusqlite::params![
                t.id,
                t.conversation_id,
                t.round,
                t.round_kind,
                t.owner_ts,
                t.member_id,
                t.route,
                t.label,
                t.message_id,
                t.status,
                t.stance,
                t.error_plain,
                t.error_detail,
                t.needs_owner as i64,
                t.started_ts,
                t.first_token_ms,
                t.elapsed_ms,
                t.input_tokens,
                t.output_tokens,
                t.cost_usd,
                t.sent_prompt_hash,
                t.sent_system,
                t.sent_text,
                t.tools_used
            ],
        )?;
        Ok(())
    }

    /// Write a member's answer and its turn row in ONE transaction: the
    /// answer becomes an assistant row of kind `council_answer` at `answer_ts`
    /// and the turn points at it. A turn that already had an answer (a retry
    /// of a stopped one) has that old row removed, so a council never shows
    /// two answers for one seat in one round. Returns the answer's row id.
    pub fn put_council_answer(
        &self,
        t: &CouncilTurnRow,
        text: &str,
        answer_ts: i64,
    ) -> Result<i64, rusqlite::Error> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let all = (|| -> Result<i64, rusqlite::Error> {
            let old: Option<Option<i64>> = self
                .conn
                .query_row(
                    "SELECT message_id FROM council_turns WHERE id = ?1",
                    [&t.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(Some(old_id)) = old {
                self.conn
                    .execute("DELETE FROM messages WHERE id = ?1", [old_id])?;
            }
            let id = self.append_message_kind(
                &t.conversation_id,
                "assistant",
                text,
                answer_ts,
                "council_answer",
            )?;
            let mut row = t.clone();
            row.message_id = Some(id);
            self.put_council_turn(&row)?;
            Ok(id)
        })();
        match all {
            Ok(id) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(id)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Every turn of a council, round by round, in the order they started.
    pub fn council_turns(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<CouncilTurnRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {TURN_COLUMNS} FROM council_turns WHERE conversation_id = ?1
             ORDER BY round ASC, started_ts ASC, id ASC"
        ))?;
        let rows = stmt.query_map([conversation_id], turn_from_row)?;
        rows.collect()
    }

    /// RC2-CNCLF4: drop turns that never went out (the memory gate refused
    /// the send they were kept for). Their answers, if any, go with them.
    pub fn delete_council_turns(&self, ids: &[String]) -> Result<(), rusqlite::Error> {
        for id in ids {
            let msg: Option<Option<i64>> = self
                .conn
                .query_row(
                    "SELECT message_id FROM council_turns WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(Some(m)) = msg {
                self.conn
                    .execute("DELETE FROM messages WHERE id = ?1", [m])?;
            }
            self.conn
                .execute("DELETE FROM council_turns WHERE id = ?1", [id])?;
        }
        Ok(())
    }

    /// The council rows of one conversation (part of a hard delete).
    pub(crate) fn delete_council_rows(&self, conversation_id: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "DELETE FROM council_turns WHERE conversation_id = ?1",
            [conversation_id],
        )?;
        self.conn.execute(
            "DELETE FROM council_members WHERE conversation_id = ?1",
            [conversation_id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_db() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "awili-persist-council-{}-{}.db",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    fn council(store: &Store) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        store
            .create_conversation_with_kind(&id, "Which model?", 100, "council")
            .unwrap();
        id
    }

    fn turn(
        id: &str,
        conv: &str,
        member_id: &str,
        route: &str,
        label: &str,
        status: &str,
    ) -> CouncilTurnRow {
        CouncilTurnRow {
            id: id.to_string(),
            conversation_id: conv.to_string(),
            round: 1,
            round_kind: "ask".to_string(),
            member_id: member_id.to_string(),
            route: route.to_string(),
            label: label.to_string(),
            status: status.to_string(),
            started_ts: 200,
            ..CouncilTurnRow::default()
        }
    }

    fn member(conv: &str, id: &str, pos: i64, route: &str, label: &str) -> CouncilMemberRow {
        CouncilMemberRow {
            conversation_id: conv.to_string(),
            member_id: id.to_string(),
            position: pos,
            route: route.to_string(),
            label: label.to_string(),
            added_ts: 100,
            removed_ts: None,
        }
    }

    /// RC2-CNCL2: members come back in column order and a turn keeps every
    /// fact the card shows (time, words, cost, stance), with its answer row.
    #[test]
    fn council_tables_round_trip() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = council(&store);
        store
            .put_council_member(&member(&conv, "m2", 1, "agent:codex", "Codex"))
            .unwrap();
        store
            .put_council_member(&member(
                &conv,
                "m1",
                0,
                "agent:claude:sonnet",
                "Claude Sonnet",
            ))
            .unwrap();
        let got = store.council_members(&conv).unwrap();
        assert_eq!(
            got.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(),
            ["Claude Sonnet", "Codex"]
        );
        store
            .append_message(&conv, "user", "which model?", 1)
            .unwrap();
        let mut t = turn(
            "t1",
            &conv,
            "m1",
            "agent:claude:sonnet",
            "Claude Sonnet",
            "answered",
        );
        t.owner_ts = Some(1);
        t.stance = Some("same".into());
        t.first_token_ms = Some(2100);
        t.elapsed_ms = Some(9000);
        t.input_tokens = Some(180);
        t.output_tokens = Some(95);
        t.cost_usd = Some(0.002);
        t.sent_prompt_hash = Some("abc".into());
        let mid = store.put_council_answer(&t, "Keep Gemma.", 2).unwrap();
        let turns = store.council_turns(&conv).unwrap();
        assert_eq!(turns.len(), 1);
        let mut want = t.clone();
        want.message_id = Some(mid);
        assert_eq!(turns[0], want);
        let msgs = store.messages(&conv).unwrap();
        let answer = msgs.iter().find(|m| m.id == mid).unwrap();
        assert_eq!(
            (
                answer.role.as_str(),
                answer.text.as_str(),
                answer.kind.as_str()
            ),
            ("assistant", "Keep Gemma.", "council_answer")
        );
        // A retry replaces the turn and its answer: one answer per seat.
        let mid2 = store
            .put_council_answer(&t, "Keep Gemma, test Qwen.", 3)
            .unwrap();
        let msgs = store.messages(&conv).unwrap();
        assert!(msgs.iter().all(|m| m.id != mid));
        assert_eq!(
            store.council_turns(&conv).unwrap()[0].message_id,
            Some(mid2)
        );
        // A hard delete takes the council rows with it.
        store.delete_conversation(&conv).unwrap();
        assert!(store.council_members(&conv).unwrap().is_empty());
        assert!(store.council_turns(&conv).unwrap().is_empty());
        cleanup(&path);
    }

    /// RC2-CNCL2: a database written before councils existed opens with its
    /// rows untouched, gains the two tables, and keeps its schema version.
    #[test]
    fn council_migration_is_additive() {
        let path = temp_db();
        let conv;
        {
            let store = Store::open(&path).unwrap();
            conv = uuid::Uuid::new_v4().to_string();
            store.create_conversation(&conv, "old chat", 10).unwrap();
            store.append_message(&conv, "user", "hello", 1).unwrap();
            store
                .conn
                .execute_batch(
                    "DROP INDEX council_turns_conv;
                     DROP TABLE council_turns;
                     DROP TABLE council_members;",
                )
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let v: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, crate::SCHEMA_VERSION, "no version step");
        let msgs = store.messages(&conv).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "hello");
        assert!(store.council_members(&conv).unwrap().is_empty());
        assert!(store.council_turns(&conv).unwrap().is_empty());
        // And opening again is harmless.
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.messages(&conv).unwrap().len(), 1);
        cleanup(&path);
    }

    /// RC2-CNCL2: a member taken off the council keeps its seat row (marked
    /// removed) and every answer it gave.
    #[test]
    fn removed_member_keeps_its_turns() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = council(&store);
        store
            .put_council_member(&member(&conv, "m1", 0, "in-app", "Gemma 4 26B"))
            .unwrap();
        let t = turn("t1", &conv, "m1", "in-app", "Gemma 4 26B", "answered");
        store.put_council_answer(&t, "Keep Gemma.", 2).unwrap();
        store.remove_council_member(&conv, "m1", 300).unwrap();
        let m = store.council_members(&conv).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].removed_ts, Some(300));
        let turns = store.council_turns(&conv).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].label, "Gemma 4 26B");
        assert!(turns[0].message_id.is_some());
        cleanup(&path);
    }

    /// RC2-CNCL2: a member that did not answer still has its turn kept —
    /// what happened, in plain words and raw, and whether the owner must act.
    #[test]
    fn failed_turn_without_message_is_kept() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = council(&store);
        let mut t = turn(
            "t9",
            &conv,
            "m4",
            "agent:dsh:deepseek-flash",
            "DeepSeek Flash",
            "failed",
        );
        t.error_plain = Some("Its service stopped responding after 30 seconds.".into());
        t.error_detail = Some("read timeout".into());
        t.needs_owner = true;
        store.put_council_turn(&t).unwrap();
        let turns = store.council_turns(&conv).unwrap();
        assert_eq!(turns, vec![t]);
        assert!(store.messages(&conv).unwrap().is_empty());
        cleanup(&path);
    }

    /// RC2-CNCLF3 (review P2 on cf08877c): a turn keeps exactly what its
    /// member was sent (system text and message, byte for byte), and a
    /// database whose council table predates those columns gains them on
    /// open, keeping its rows.
    #[test]
    fn rc2cnclf3_the_sent_prompt_is_kept_and_old_tables_gain_it() {
        let path = temp_db();
        let conv;
        {
            let store = Store::open(&path).unwrap();
            conv = council(&store);
            // The table as RC2-CNCL2 made it: no sent-prompt columns.
            store
                .conn
                .execute_batch(
                    "DROP INDEX council_turns_conv;
                     DROP TABLE council_turns;
                     CREATE TABLE council_turns (
                         id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL,
                         round INTEGER NOT NULL, round_kind TEXT NOT NULL,
                         owner_ts INTEGER, member_id TEXT NOT NULL, route TEXT NOT NULL,
                         label TEXT NOT NULL, message_id INTEGER, status TEXT NOT NULL,
                         stance TEXT, error_plain TEXT, error_detail TEXT,
                         needs_owner INTEGER NOT NULL DEFAULT 0, started_ts INTEGER NOT NULL,
                         first_token_ms INTEGER, elapsed_ms INTEGER, input_tokens INTEGER,
                         output_tokens INTEGER, cost_usd REAL, sent_prompt_hash TEXT);
                     INSERT INTO council_turns (id, conversation_id, round, round_kind,
                         member_id, route, label, status, started_ts)
                     VALUES ('old', '__CONV__', 1, 'ask', 'm1', 'in-app', 'Gemma', 'answered', 5);"
                        .replace("__CONV__", &conv)
                        .as_str(),
                )
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let old = store.council_turns(&conv).unwrap();
        assert_eq!(old.len(), 1, "the old row stays");
        assert_eq!(old[0].sent_text, None);
        let mut t = turn("t2", &conv, "m2", "provider:fleet", "Hermes", "failed");
        t.sent_system = Some("You are Hermes, one of 2 models on Owner's council.".into());
        t.sent_text =
            Some("The council so far:\n\nOwner: Gemma or Qwen?\n\n---\n\nAnswer.".into());
        t.sent_prompt_hash = Some("00ff".into());
        store.put_council_turn(&t).unwrap();
        let got = store
            .council_turns(&conv)
            .unwrap()
            .into_iter()
            .find(|r| r.id == "t2")
            .unwrap();
        assert_eq!(got, t);
        drop(store);
        // Opening again is harmless.
        let store = Store::open(&path).unwrap();
        assert_eq!(store.council_turns(&conv).unwrap().len(), 2);
        cleanup(&path);
    }

    /// RC2-CNCLF4: turns kept for a send that never went out (the memory
    /// gate refused it) are dropped, with any answer they had; the others
    /// stay.
    #[test]
    fn rc2cnclf4_turns_that_never_went_out_are_dropped() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = council(&store);
        let keep = turn("keep", &conv, "m1", "in-app", "Gemma", "answered");
        store.put_council_answer(&keep, "Keep Gemma.", 3).unwrap();
        let gone = turn("gone", &conv, "m2", "provider:fleet", "Hermes", "running");
        store.put_council_answer(&gone, "Move", 4).unwrap();
        let empty = turn("empty", &conv, "m1", "in-app", "Gemma", "running");
        store.put_council_turn(&empty).unwrap();
        store
            .delete_council_turns(&["gone".to_string(), "empty".to_string()])
            .unwrap();
        let left = store.council_turns(&conv).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, "keep");
        let msgs = store.messages(&conv).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "Keep Gemma.");
        cleanup(&path);
    }
}
