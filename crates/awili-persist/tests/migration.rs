//! Pre-slice7 → slice-7 schema migration: the no-data-loss proof.
//!
//! The fixture is built with the EXACT DDL and write patterns the pre-slice7
//! `Store` used (copied verbatim from `awili-persist/src/lib.rs` at commit
//! 7088526, the slice-4 merge) via a raw `rusqlite` connection — the current
//! crate code never touches the file until the migration under test runs. That
//! makes this a true fixture: byte-for-byte the layout a real user database has
//! on disk before this slice.

use std::path::{Path, PathBuf};

use awili_persist::{Store, SCHEMA_VERSION};
use rusqlite::Connection;

/// The pre-slice7 schema, verbatim from the old `Store::init`.
const LEGACY_SCHEMA: &str = "PRAGMA journal_mode=WAL;
     PRAGMA synchronous=NORMAL;
     PRAGMA foreign_keys=ON;
     CREATE TABLE IF NOT EXISTS conversations (
         id         INTEGER PRIMARY KEY AUTOINCREMENT,
         started_ts INTEGER NOT NULL
     );
     CREATE TABLE IF NOT EXISTS messages (
         id              INTEGER PRIMARY KEY AUTOINCREMENT,
         conversation_id INTEGER NOT NULL REFERENCES conversations(id),
         role            TEXT    NOT NULL,
         text            TEXT    NOT NULL,
         ts              INTEGER NOT NULL
     );
     CREATE TABLE IF NOT EXISTS app_state (
         key   TEXT PRIMARY KEY,
         value TEXT NOT NULL
     );";

fn temp_db(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("awili-migration-{tag}-{}.db", std::process::id()))
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

/// Build the fixture: one conversation, a real-looking transcript, a saved
/// model selection — exactly what the slice-2..6 app wrote.
fn write_single_conversation_fixture(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(LEGACY_SCHEMA).unwrap();
    conn.execute("INSERT INTO conversations (started_ts) VALUES (0)", [])
        .unwrap();
    let conv = conn.last_insert_rowid();
    let transcript: &[(&str, &str)] = &[
        ("user", "hello awili"),
        ("assistant", "Hi Owner — how can I help?"),
        ("user", "explain WAL mode\nin two lines"),
        (
            "assistant",
            "Committed transactions land in the write-ahead log…\nreplayed on reopen. 😀",
        ),
    ];
    for (ts, (role, text)) in transcript.iter().enumerate() {
        conn.execute(
            "INSERT INTO messages (conversation_id, role, text, ts) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![conv, role, text, ts as i64],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO app_state (key, value) VALUES ('selection', 'ollama\u{1f}llama3.1:8b')",
        [],
    )
    .unwrap();
}

#[test]
fn migrates_pre_slice7_db_without_losing_a_row() {
    let path = temp_db("single");
    write_single_conversation_fixture(&path);

    // The migration under test: just open the fixture with today's Store.
    let store = Store::open(&path).unwrap();

    let convs = store.conversations().unwrap();
    assert_eq!(convs.len(), 1, "the one legacy conversation survives");
    let c = &convs[0];
    assert_eq!(
        c.title, "Conversation 1",
        "spec: existing history becomes 'Conversation 1'"
    );
    assert!(!c.archived);
    assert!(
        !c.title_is_custom,
        "a migrated title must not block a future rename"
    );
    assert_eq!(c.created_ts, 0);
    // The id is now a UUID, not an integer.
    assert_eq!(c.id.len(), 36);
    assert_eq!(c.id.matches('-').count(), 4);

    // ROUND-TRIP: every message, in order, byte-identical (incl. newlines and
    // multibyte text), with roles and timestamps intact.
    let msgs = store.messages(&c.id).unwrap();
    assert_eq!(msgs.len(), 4);
    let expect: &[(&str, &str)] = &[
        ("user", "hello awili"),
        ("assistant", "Hi Owner — how can I help?"),
        ("user", "explain WAL mode\nin two lines"),
        (
            "assistant",
            "Committed transactions land in the write-ahead log…\nreplayed on reopen. 😀",
        ),
    ];
    for (i, ((role, text), msg)) in expect.iter().zip(&msgs).enumerate() {
        assert_eq!(msg.role, *role, "message {i} role");
        assert_eq!(msg.text, *text, "message {i} text");
        assert_eq!(msg.ts, i as i64, "message {i} ts");
    }

    // app_state rides through untouched.
    assert_eq!(
        store.get_app_state("selection").unwrap().as_deref(),
        Some("ollama\u{1f}llama3.1:8b")
    );

    // The legacy tables are gone and the version stamp is current.
    let conn = Connection::open(&path).unwrap();
    let legacy: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '%_legacy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(legacy, 0);
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);

    cleanup(&path);
}

#[test]
fn v9_adds_message_kind_superseded_with_safe_defaults_and_methods() {
    let path = temp_db("v9-summary");
    write_single_conversation_fixture(&path);

    // The legacy fixture migrates all the way to v9 (slice 21 §D4 adds
    // messages.kind / superseded_by). Existing rows MUST default to a normal,
    // live message — mislabeling or dropping them would be data loss.
    let store = Store::open(&path).unwrap();
    let c = store.conversations().unwrap().into_iter().next().unwrap();
    let msgs = store.messages(&c.id).unwrap();
    assert_eq!(msgs.len(), 4);
    for m in &msgs {
        assert_eq!(
            m.kind, "normal",
            "migrated rows default to a normal message"
        );
        assert_eq!(
            m.superseded_by, None,
            "migrated rows are live, not superseded"
        );
    }

    // The summarize path: append a summary message, supersede the two oldest
    // turns, then prove it is fully reversible ("Discard summary").
    let oldest = msgs[0].id;
    let second = msgs[1].id;
    let summary_id = store
        .append_summary_message(&c.id, "SUMMARY of turns 1-2", 100)
        .unwrap();

    let summary = store
        .messages(&c.id)
        .unwrap()
        .into_iter()
        .find(|m| m.id == summary_id)
        .unwrap();
    assert_eq!(summary.kind, "summary");
    assert_eq!(summary.text, "SUMMARY of turns 1-2");
    assert_eq!(summary.superseded_by, None);

    store.set_superseded(oldest, summary_id).unwrap();
    store.set_superseded(second, summary_id).unwrap();
    let after = store.messages(&c.id).unwrap();
    assert_eq!(
        after.iter().find(|m| m.id == oldest).unwrap().superseded_by,
        Some(summary_id)
    );
    assert_eq!(
        after.iter().find(|m| m.id == second).unwrap().superseded_by,
        Some(summary_id)
    );
    // The other turns stay live.
    assert_eq!(
        after
            .iter()
            .find(|m| m.id == msgs[2].id)
            .unwrap()
            .superseded_by,
        None
    );

    // Discard: clear_superseded reverses it and reports how many it restored.
    let cleared = store.clear_superseded(summary_id).unwrap();
    assert_eq!(cleared, 2, "both superseded turns are restored");
    let restored = store.messages(&c.id).unwrap();
    assert!(restored.iter().all(|m| m.superseded_by.is_none()));

    cleanup(&path);
}

#[test]
fn migrates_multiple_legacy_conversations_in_id_order() {
    // A db can hold several legacy conversations (create_conversation existed
    // since slice 2). Each becomes its own "Conversation N" — merging them into
    // one would lose the grouping, which IS data.
    let path = temp_db("multi");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(LEGACY_SCHEMA).unwrap();
        for (started, (role, text)) in [
            (100, ("user", "first convo")),
            (200, ("user", "second convo")),
        ] {
            conn.execute(
                "INSERT INTO conversations (started_ts) VALUES (?1)",
                [started],
            )
            .unwrap();
            let conv = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO messages (conversation_id, role, text, ts) VALUES (?1, ?2, ?3, 0)",
                rusqlite::params![conv, role, text],
            )
            .unwrap();
        }
        // An empty conversation must survive too — it is still user data.
        conn.execute("INSERT INTO conversations (started_ts) VALUES (300)", [])
            .unwrap();
    }

    let store = Store::open(&path).unwrap();
    let convs = store.conversations().unwrap();
    assert_eq!(convs.len(), 3);
    assert_eq!(convs[0].title, "Conversation 1");
    assert_eq!(convs[1].title, "Conversation 2");
    assert_eq!(convs[2].title, "Conversation 3");
    assert_eq!(convs[0].created_ts, 100);
    assert_eq!(convs[2].created_ts, 300);
    assert_eq!(store.messages(&convs[0].id).unwrap()[0].text, "first convo");
    assert_eq!(
        store.messages(&convs[1].id).unwrap()[0].text,
        "second convo"
    );
    assert!(store.messages(&convs[2].id).unwrap().is_empty());

    cleanup(&path);
}

#[test]
fn migrated_db_reopens_idempotently() {
    // Open the fixture twice: the second open must see schema v2 and change
    // nothing (user_version gates the migration).
    let path = temp_db("reopen");
    write_single_conversation_fixture(&path);

    let first = Store::open(&path).unwrap();
    let convs_before = first.conversations().unwrap();
    let msgs_before = first.messages(&convs_before[0].id).unwrap();
    drop(first);

    let second = Store::open(&path).unwrap();
    let convs_after = second.conversations().unwrap();
    assert_eq!(convs_before, convs_after, "second open must not re-migrate");
    assert_eq!(msgs_before, second.messages(&convs_after[0].id).unwrap());

    cleanup(&path);
}

#[test]
fn migrated_conversation_accepts_new_writes_and_archive() {
    // Post-migration the conversation is a first-class slice-7 row: appends,
    // renames, soft archive — all work against the migrated UUID.
    let path = temp_db("writes");
    write_single_conversation_fixture(&path);

    let store = Store::open(&path).unwrap();
    let id = store.conversations().unwrap()[0].id.clone();
    store
        .append_message(&id, "user", "a post-migration message", 4)
        .unwrap();
    store.rename_conversation(&id, "Old times", true).unwrap();
    store.set_archived(&id, true).unwrap();

    let c = store.conversations().unwrap()[0].clone();
    assert_eq!(c.title, "Old times");
    assert!(c.archived);
    let msgs = store.messages(&id).unwrap();
    assert_eq!(msgs.len(), 5, "old rows + the new append");
    assert_eq!(msgs[4].text, "a post-migration message");

    cleanup(&path);
}

// ---- slice 8: v2 → v3 (usage_events) ---------------------------------------

/// The slice-7 schema verbatim, stamped `user_version = 2` — what a database
/// written by the slice-7 app looks like on disk before slice 8 opens it.
fn write_v2_fixture(path: &Path) -> String {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         BEGIN;
         CREATE TABLE conversations (
             id              TEXT PRIMARY KEY,
             title           TEXT    NOT NULL,
             created_ts      INTEGER NOT NULL,
             updated_ts      INTEGER NOT NULL,
             archived        INTEGER NOT NULL DEFAULT 0,
             title_is_custom INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE messages (
             id              INTEGER PRIMARY KEY AUTOINCREMENT,
             conversation_id TEXT    NOT NULL REFERENCES conversations(id),
             role            TEXT    NOT NULL,
             text            TEXT    NOT NULL,
             ts              INTEGER NOT NULL
         );
         CREATE INDEX idx_messages_conversation ON messages (conversation_id, ts, id);
         CREATE TABLE IF NOT EXISTS app_state (
             key   TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );
         PRAGMA user_version = 2;
         COMMIT;",
    )
    .unwrap();
    let id = "5a1e8400-e29b-41d4-a716-446655440000".to_string();
    conn.execute(
        "INSERT INTO conversations (id, title, created_ts, updated_ts, archived, title_is_custom)
         VALUES (?1, 'Garden plan', 10, 20, 0, 1)",
        [&id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (conversation_id, role, text, ts) VALUES (?1, 'user', 'hi', 0)",
        [&id],
    )
    .unwrap();
    id
}

#[test]
fn migrates_v2_db_to_v3_additively() {
    let path = temp_db("v2-to-v3");
    let conv_id = write_v2_fixture(&path);

    let store = Store::open(&path).unwrap();

    // Existing data is untouched...
    let convs = store.conversations().unwrap();
    assert_eq!(convs.len(), 1);
    assert_eq!(convs[0].id, conv_id);
    assert_eq!(convs[0].title, "Garden plan");
    assert!(convs[0].title_is_custom);
    assert_eq!(store.messages(&conv_id).unwrap().len(), 1);

    // ...and the new table is live: usage records and rolls up (turn_ts 1_000
    // falls on the UTC day 1970-01-01).
    store
        .record_usage(&conv_id, 1_000, "anthropic", "claude-haiku-4-5", 12, 7)
        .unwrap();
    let epoch_day = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    assert_eq!(store.daily_usage(epoch_day).unwrap(), (12, 7));

    let conn = Connection::open(&path).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);

    cleanup(&path);
}

#[test]
fn legacy_v0_db_lands_on_v3_with_working_usage_table() {
    // The two-step v0→v2→v3 path: the heavy slice-7 copy, then the additive
    // usage table — one open does both.
    let path = temp_db("v0-to-v3");
    write_single_conversation_fixture(&path);

    let store = Store::open(&path).unwrap();
    let id = store.conversations().unwrap()[0].id.clone();
    store
        .record_usage(&id, 50, "ollama", "llama3.1:8b", 3, 4)
        .unwrap();
    let epoch_day = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    assert_eq!(store.daily_usage(epoch_day).unwrap(), (3, 4));

    let conn = Connection::open(&path).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);

    cleanup(&path);
}
