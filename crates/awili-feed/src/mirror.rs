//! The local sqlite mirror of the central feed.
//!
//! Awili never renders straight off the network: the Needs You panel reads this
//! mirror, which a worker thread fills by cursor polling. That gives instant
//! startup (last sync's events are already on disk), offline browse, and an
//! honest "last sync N min ago" stamp when HOSTC is unreachable.
//!
//! Sync is three reads against the service, then ONE commit:
//! 1. `GET /events?after=<cursor>` pages — append-only pull of everything new.
//!    S5(b): a pulled event carrying an `acked_ts` is an explicit authoritative
//!    ack fact (another consumer acked it); the central row is authoritative,
//!    so the commit applies the stamp.
//! 2. `GET /needs-you` — VALIDATION only. The snapshot's `total` must agree
//!    with the grouped `event_ids` count; an empty snapshot is a legitimate
//!    "nothing needs you" and a mismatching one is rejected. Absence from the
//!    snapshot is NEVER an ack: S5(b) stops inferring acknowledgement from
//!    absence (a truncated snapshot must not wipe the panel).
//! 3. `GET /acks?after=<ack_cursor>` pages — S5(b) FIX1: the explicit ack-fact
//!    lane. The event row's `acked_ts` flip never advances `seq`, so a remote
//!    ack of an already-mirrored event reaches the mirror ONLY through this
//!    append-only log, applied to its named row alone.
//!
//! All local writes — event rows, ack facts, both cursors, `last_sync_ts` —
//! land in a single transaction in phase 4. Any error before the commit
//! leaves the mirror byte-identical.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::client::{FeedClient, FeedError};
use crate::types::{AckFact, Event, Severity};

/// What one sync pass accomplished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncOutcome {
    /// New events pulled this pass.
    pub pulled: usize,
    /// Explicit ack facts that changed a local row this pass: pulled
    /// attention/critical events arriving already stamped, plus ack-log facts
    /// that flipped a local unacked row.
    pub reconciled_acks: usize,
    /// The cursor after this pass.
    pub cursor: i64,
}

/// One row of the local Needs You list: the newest event of a dedup group plus
/// how many unacked events share it ("4 jobs blocked: …" storm collapse), and
/// every member id so Ack clears the whole condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeedsItem {
    /// The newest unacked event in the group — what the row displays.
    pub event: Event,
    /// The WORST severity present in the group — what the row colors/announces.
    /// Distinct from `event.severity`: a critical followed by an attention on
    /// the same condition must keep the group critical until the critical is
    /// acked (review slice-4 SF1).
    pub severity: Severity,
    /// Unacked events sharing the dedup_key (1 when keyless).
    pub count: usize,
    /// Every unacked event_id in the group, for bulk ack.
    pub event_ids: Vec<String>,
}

/// The mirror handle: owns the sqlite connection. Lives on the feed worker
/// thread (same posture as `awili_persist::Store` on the persistence worker).
pub struct FeedMirror {
    conn: Connection,
}

/// Page size for cursor pulls. Big enough that steady-state sync is one
/// request; small enough that a cold first sync against a long history streams
/// in bounded chunks.
const PULL_LIMIT: usize = 500;

#[cfg(test)]
impl FeedMirror {
    /// The stored `acked_ts` for an event (test accessor for the S5(b)
    /// proof tests: asserts the authoritative stamp and which rows it hit).
    fn acked_ts_for(&self, event_id: &str) -> Result<Option<i64>, FeedError> {
        Ok(self.conn.query_row(
            "SELECT acked_ts FROM events WHERE event_id = ?1",
            [event_id],
            |r| r.get(0),
        )?)
    }

    /// How many event rows the mirror holds (test accessor).
    fn row_count(&self) -> Result<usize, FeedError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)?)
    }
}

/// S5(b) validation: the snapshot's `total` must equal the total number of
/// grouped `event_ids` (the service's own badge number, which it computes as
/// the sum of group counts). An empty snapshot (0 groups, total 0) is a
/// legitimate "nothing needs you". A mismatch means the snapshot is
/// inconsistent (e.g. truncated) and must be rejected as a parse failure —
/// fail closed rather than trust a half-formed picture.
fn validate_needs_you(needs: &crate::types::NeedsYou) -> Result<(), FeedError> {
    let grouped: usize = needs.groups.iter().map(|g| g.event_ids.len()).sum();
    if grouped != needs.total {
        return Err(FeedError::Parse(format!(
            "needs-you snapshot inconsistent: total={:?} but grouped event_ids={:?}",
            needs.total, grouped
        )));
    }
    Ok(())
}

impl FeedMirror {
    /// The default cache path: `<data_dir>/Awili/feed-cache.db`, next to the
    /// conversation store.
    pub fn default_path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join(awili_config::APP_DIR_NAME).join("feed-cache.db"))
    }

    /// Open (creating if needed) the mirror at `path`.
    pub fn open(path: &Path) -> Result<FeedMirror, FeedError> {
        if let Some(parent) = path.parent() {
            let _ = awili_config::ensure_private_app_dir(parent);
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open at [`default_path`](Self::default_path).
    pub fn open_default() -> Result<FeedMirror, FeedError> {
        let path = Self::default_path()
            .ok_or_else(|| FeedError::Db("no platform data dir".to_string()))?;
        Self::open(&path)
    }

    fn init(conn: Connection) -> Result<FeedMirror, FeedError> {
        // WAL like the conversation store: a crash mid-sync replays cleanly.
        // This is a CACHE of the authoritative central store — losing it costs
        // one full re-pull, never data.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS events (
                 seq        INTEGER PRIMARY KEY,
                 event_id   TEXT    NOT NULL UNIQUE,
                 source     TEXT    NOT NULL,
                 severity   TEXT    NOT NULL,
                 title      TEXT    NOT NULL,
                 body       TEXT    NOT NULL,
                 dedup_key  TEXT,
                 created_ts INTEGER NOT NULL,
                 acked_ts   INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_events_needs
                 ON events (severity) WHERE acked_ts IS NULL;
             CREATE TABLE IF NOT EXISTS sync_state (
                 key   TEXT PRIMARY KEY,
                 value INTEGER NOT NULL
             );",
        )?;
        Ok(FeedMirror { conn })
    }

    fn state(&self, key: &str) -> Result<Option<i64>, FeedError> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM sync_state WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get(0)?),
            None => None,
        })
    }

    fn set_state(&self, key: &str, value: i64) -> Result<(), FeedError> {
        self.conn.execute(
            "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// The read cursor: the highest `seq` pulled so far (0 = never synced).
    pub fn cursor(&self) -> i64 {
        self.state("cursor").ok().flatten().unwrap_or(0)
    }

    /// The ack-fact cursor: the highest `ack_seq` applied (0 = none).
    pub fn ack_cursor(&self) -> i64 {
        self.state("ack_cursor").ok().flatten().unwrap_or(0)
    }

    /// Unix seconds of the last successful sync, if any — what the panel shows
    /// as "last sync N min ago" while offline.
    pub fn last_sync_ts(&self) -> Option<i64> {
        self.state("last_sync_ts").ok().flatten()
    }

    /// Apply one explicit ack fact (S5(b) FIX1) to its named row ONLY: flips
    /// an unacked row to the authoritative stamp. Unknown ids are a no-op —
    /// the event will arrive on the event cursor already stamped. Returns
    /// `true` when a local row changed.
    fn apply_ack_fact(&self, fact: &AckFact) -> Result<bool, FeedError> {
        let changed = self.conn.execute(
            "UPDATE events SET acked_ts = ?2 WHERE event_id = ?1 AND acked_ts IS NULL",
            rusqlite::params![fact.event_id, fact.acked_ts],
        )?;
        Ok(changed == 1)
    }

    /// Upsert one event from the service. `INSERT OR REPLACE`: the central row
    /// is authoritative, and replacement is how a reconciled ack/edit lands.
    pub fn upsert(&self, e: &Event) -> Result<(), FeedError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO events
                 (seq, event_id, source, severity, title, body, dedup_key, created_ts, acked_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                e.seq,
                e.event_id,
                e.source,
                e.severity.as_str(),
                e.title,
                e.body,
                e.dedup_key,
                e.created_ts,
                e.acked_ts,
            ],
        )?;
        Ok(())
    }

    /// One full sync pass against the service. Three reads, then ONE commit
    /// (S5(b) FIX1): fetch the new events from the event cursor, validate the
    /// `/needs-you` snapshot, fetch the new ack facts from the ack-fact
    /// cursor, and only then write everything (event rows, ack facts, both
    /// cursors, `last_sync_ts`) in a single transaction. Any error before the
    /// commit leaves the mirror byte-identical. `now_ts` (unix seconds)
    /// stamps the sync time — the caller owns the clock.
    ///
    /// S5(b): the pull itself reconciles ack state. Any pulled
    /// attention/critical event carrying an `acked_ts` is an explicit
    /// authoritative ack fact — the central row is authoritative, so the
    /// commit applies it. Absence from the `/needs-you` snapshot is NOT an
    /// ack: an empty or truncated snapshot is valid or rejected on its own
    /// terms and never touches local rows. The only other way a row becomes
    /// acked is the app's explicit `mark_acked` after a successful ack
    /// passthrough.
    pub fn sync(&mut self, client: &FeedClient, now_ts: i64) -> Result<SyncOutcome, FeedError> {
        // 1. FETCH events: cursor pull, paging until a short page says we are
        //    caught up. No local writes yet.
        let mut cursor = self.cursor();
        let mut events = Vec::new();
        loop {
            let page = client.events_after(cursor, PULL_LIMIT)?;
            let short = page.events.len() < PULL_LIMIT;
            events.extend(page.events);
            cursor = cursor.max(page.cursor);
            if short {
                break;
            }
        }

        // 2. VALIDATE the snapshot; do NOT reconcile acks from it. A mismatch
        //    stops after exactly these two requests.
        let needs = client.needs_you()?;
        validate_needs_you(&needs)?;

        // 3. FETCH ack facts: the same paged pull on the ack-fact cursor.
        let mut ack_cursor = self.ack_cursor();
        let mut acks = Vec::new();
        loop {
            let page = client.acks_after(ack_cursor, PULL_LIMIT)?;
            let short = page.acks.len() < PULL_LIMIT;
            acks.extend(page.acks);
            ack_cursor = ack_cursor.max(page.cursor);
            if short {
                break;
            }
        }

        // 4. COMMIT, one transaction: rows, ack facts, both cursors, stamp.
        //    An early `?` drops the tx and rolls the mirror back to its
        //    pre-sync state.
        let mut reconciled_acks = 0usize;
        let tx = self.conn.unchecked_transaction()?;
        for event in &events {
            let fact = matches!(
                (event.severity, event.acked_ts),
                (Severity::Attention | Severity::Critical, Some(_))
            );
            self.upsert(event)?;
            if fact {
                reconciled_acks += 1;
            }
        }
        for fact in &acks {
            if self.apply_ack_fact(fact)? {
                reconciled_acks += 1;
            }
        }
        self.set_state("cursor", cursor)?;
        self.set_state("ack_cursor", ack_cursor)?;
        self.set_state("last_sync_ts", now_ts)?;
        tx.commit()?;
        Ok(SyncOutcome {
            pulled: events.len(),
            reconciled_acks,
            cursor,
        })
    }

    /// Mark a mirrored event acked after a SUCCESSFUL ack passthrough (the app
    /// worker's explicit ack). S5(b): this is the only local path that marks a
    /// row acked from outside the event stream — the snapshot never does.
    /// Unknown ids are a no-op — the row may simply not have been pulled yet.
    pub fn mark_acked(&self, event_id: &str, acked_ts: i64) -> Result<(), FeedError> {
        self.conn.execute(
            "UPDATE events SET acked_ts = ?2 WHERE event_id = ?1 AND acked_ts IS NULL",
            rusqlite::params![event_id, acked_ts],
        )?;
        Ok(())
    }

    /// The badge number: unacked attention+critical EVENTS (not groups).
    pub fn unacked_count(&self) -> Result<usize, FeedError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events
             WHERE acked_ts IS NULL AND severity IN ('attention', 'critical')",
            [],
            |row| row.get(0),
        )?;
        Ok(n as usize)
    }

    /// The local Needs You list: unacked attention+critical collapsed by
    /// `dedup_key` (keyless events stand alone), newest group first. Pure local
    /// read — this is what the panel renders, online or not.
    pub fn needs_you_local(&self) -> Result<Vec<NeedsItem>, FeedError> {
        let mut stmt = self.conn.prepare(
            // g.worst is the group's MAX severity (review SF1): the central
            // /needs-you computes the same, and the panel must keep rendering
            // a group critical while an unacked critical member remains, even
            // when the NEWEST member is merely attention. Mapped through CASE
            // because the severity STRINGS do not sort by loudness.
            "SELECT e.seq, e.event_id, e.source, e.severity, e.title, e.body,
                    e.dedup_key, e.created_ts, e.acked_ts, g.cnt, g.worst
             FROM events e
             JOIN (SELECT MAX(seq) AS latest_seq, COUNT(*) AS cnt,
                          MAX(CASE severity WHEN 'critical' THEN 2 ELSE 1 END) AS worst
                   FROM events
                   WHERE acked_ts IS NULL AND severity IN ('attention', 'critical')
                   GROUP BY COALESCE(dedup_key, 'seq:' || seq)) g
               ON e.seq = g.latest_seq
             ORDER BY e.created_ts DESC, e.seq DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let severity: String = row.get(3)?;
            Ok((
                Event {
                    seq: row.get(0)?,
                    event_id: row.get(1)?,
                    source: row.get(2)?,
                    // Unknown severity cannot occur here: the WHERE clause
                    // matched it against the known strings.
                    severity: Severity::parse(&severity).unwrap_or(Severity::Attention),
                    title: row.get(4)?,
                    body: row.get(5)?,
                    dedup_key: row.get(6)?,
                    created_ts: row.get(7)?,
                    acked_ts: row.get(8)?,
                },
                row.get::<_, i64>(9)? as usize,
                if row.get::<_, i64>(10)? >= 2 {
                    Severity::Critical
                } else {
                    Severity::Attention
                },
            ))
        })?;

        let mut items = Vec::new();
        for row in rows {
            let (event, count, severity) = row?;
            let event_ids = match &event.dedup_key {
                Some(key) => {
                    let mut stmt = self.conn.prepare(
                        "SELECT event_id FROM events
                         WHERE acked_ts IS NULL AND severity IN ('attention', 'critical')
                           AND dedup_key = ?1
                         ORDER BY seq",
                    )?;
                    let ids = stmt.query_map([key], |row| row.get::<_, String>(0))?;
                    ids.collect::<Result<_, _>>()?
                }
                None => vec![event.event_id.clone()],
            };
            items.push(NeedsItem {
                event,
                severity,
                count,
                event_ids,
            });
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;

    /// A unique temp mirror per test (same convention as the crate root's
    /// `temp_mirror`).
    fn s5b_temp_mirror() -> (FeedMirror, PathBuf) {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "awili-s5b-mirror-{}-{}.db",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        (FeedMirror::open(&path).unwrap(), path)
    }

    /// An S5(b) attention event with id `a{seq}`, unacked.
    fn s5b_event(seq: i64) -> Event {
        Event {
            seq,
            event_id: format!("a{seq}"),
            source: "test".to_string(),
            severity: Severity::Attention,
            title: format!("event {seq}"),
            body: String::new(),
            dedup_key: None,
            created_ts: 1000 + seq,
            acked_ts: None,
        }
    }

    /// A single JSON event body with only the fields under test set. `acked`
    /// serialises as a number or `null`.
    fn s5b_ev_json(seq: i64, id: &str, acked: Option<i64>) -> String {
        let acked = match acked {
            Some(t) => t.to_string(),
            None => "null".to_string(),
        };
        format!(
            "{{\"seq\": {seq}, \"event_id\": \"{id}\", \"source\": \"t\", \"severity\": \"attention\", \
             \"title\": \"t\", \"body\": \"b\", \"dedup_key\": null, \"created_ts\": 100, \
             \"acked_ts\": {acked}}}"
        )
    }

    /// One request the stub saw: `"METHOD path"` (query included), in order.
    type S5bCaptures = Arc<Mutex<Vec<String>>>;

    /// A single well-formed ack-fact body (S5(b) FIX1).
    fn s5b_ack_json(ack_seq: i64, id: &str, ts: i64) -> String {
        format!("{{\"ack_seq\": {ack_seq}, \"event_id\": \"{id}\", \"acked_ts\": {ts}}}")
    }

    /// A single well-formed ROUTINE event body (S5(b) FIX1): the same shape as
    /// `s5b_ev_json` but `"severity": "routine"` and `"acked_ts": null` — a
    /// pull that moves the event cursor without touching the unacked count.
    fn s5b_routine_json(seq: i64, id: &str) -> String {
        format!(
            "{{\"seq\": {seq}, \"event_id\": \"{id}\", \"source\": \"t\", \"severity\": \"routine\", \
             \"title\": \"t\", \"body\": \"b\", \"dedup_key\": null, \"created_ts\": 100, \
             \"acked_ts\": null}}"
        )
    }

    /// Spawn a loopback stub answering the three mirror sync calls with fixed
    /// bodies, routed by path prefix: `events_body` (a JSON array) for
    /// `GET /events?after=…` — the service returns `{"events": [...], "cursor":
    /// 0}` — `acks_body` (a JSON array) for `GET /acks?after=…` — `{"acks":
    /// [...], "cursor": 0}` — and `needs_body` for anything else (the
    /// `/needs-you` validation read). Every request is captured as
    /// `"METHOD path"`.
    fn s5b_stub_full(
        events_body: &str,
        acks_body: &str,
        needs_body: &str,
    ) -> (String, S5bCaptures) {
        let captures: S5bCaptures = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let cap = Arc::clone(&captures);
        let events = format!("{{\"events\": {events_body}, \"cursor\": 0}}");
        let acks = format!("{{\"acks\": {acks_body}, \"cursor\": 0}}");
        let needs = needs_body.to_string();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let cap = Arc::clone(&cap);
                let events = events.clone();
                let acks = acks.clone();
                let needs = needs.clone();
                thread::spawn(move || {
                    let mut data = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        let n = stream.read(&mut buf).unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        data.extend_from_slice(&buf[..n]);
                        if data.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let req = String::from_utf8_lossy(&data).into_owned();
                    let mut parts = req.split_whitespace();
                    let method = parts.next().unwrap_or("");
                    let path = parts.next().unwrap_or("/");
                    cap.lock().unwrap().push(format!("{method} {path}"));
                    let body = if path.starts_with("/events") {
                        events
                    } else if path.starts_with("/acks") {
                        acks
                    } else {
                        needs
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        (format!("http://{addr}"), captures)
    }

    /// [`s5b_stub_full`] with an empty ack-fact page.
    fn s5b_stub(events_body: &str, needs_body: &str) -> (String, S5bCaptures) {
        s5b_stub_full(events_body, "[]", needs_body)
    }

    /// A single well-formed NeedsGroup body: `count` = number of ids, `latest`
    /// is the last id, and `event_ids` is the full member list.
    fn s5b_group_json(key: &str, ids: &[&str]) -> String {
        let ids_json: Vec<String> = ids.iter().map(|i| format!("\"{i}\"")).collect();
        let latest = ids.last().copied().unwrap_or("");
        format!(
            "{{\"dedup_key\": \"{key}\", \"count\": {}, \"severity\": \"attention\", \
             \"latest\": {{\"seq\": 1, \"event_id\": \"{latest}\", \"source\": \"t\", \
             \"severity\": \"attention\", \"title\": \"t\", \"body\": \"b\", \
             \"dedup_key\": \"{key}\", \"created_ts\": 100, \"acked_ts\": null}}, \
             \"event_ids\": [{}]}}",
            ids.len(),
            ids_json.join(", ")
        )
    }

    /// S5(b) required proof: an EMPTY well-formed snapshot (total = 0, no
    /// groups) and a TRUNCATED one (`total` larger than the grouped
    /// `event_ids` count) both leave the local mirror's unacked rows and their
    /// timestamps untouched. Only an explicit acked fact may clear a row.
    /// Driven against the loopback stub so the wire contract is the one under
    /// test.
    #[test]
    fn empty_or_truncated_needs_snapshot_preserves_attention() {
        let (mut mirror, path) = s5b_temp_mirror();
        // The mirror holds two unacked attention events — the panel's current
        // truth.
        mirror.upsert(&s5b_event(1)).unwrap();
        mirror.upsert(&s5b_event(2)).unwrap();
        assert_eq!(mirror.unacked_count().unwrap(), 2);

        // (1) Empty but well-formed: valid, and it must NOT ack anything.
        let (base, _caps) = s5b_stub("[]", r#"{"groups": [], "total": 0}"#);
        let client = FeedClient::new(&base, Some("t".to_string()));
        let outcome = mirror.sync(&client, 5000).unwrap();
        assert_eq!(outcome.reconciled_acks, 0);
        assert_eq!(
            mirror.unacked_count().unwrap(),
            2,
            "empty snapshot must not ack"
        );
        assert!(matches!(mirror.last_sync_ts(), Some(5000)));

        // (2) Truncated: `total` claims two events but the groups list none.
        let (base, caps) = s5b_stub("[]", r#"{"groups": [], "total": 2}"#);
        let client = FeedClient::new(&base, Some("t".to_string()));
        assert!(
            matches!(mirror.sync(&client, 5100), Err(FeedError::Parse(_))),
            "inconsistent total must be rejected as a parse failure"
        );
        // The rejection is fail-closed: no local change at all.
        assert_eq!(
            mirror.unacked_count().unwrap(),
            2,
            "rejected snapshot must not ack"
        );
        assert_eq!(
            mirror.last_sync_ts(),
            Some(5000),
            "a rejected sync stamps nothing new"
        );
        // …and the sync stopped after the pull and the rejected read.
        let calls = caps.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            2,
            "events pull + needs-you, then stop: {calls:?}"
        );

        // (3) A later sync with a CORRECT snapshot heals without touching rows.
        let group = s5b_group_json("k1", &["a1", "a2"]);
        let needs = format!(r#"{{"groups": [{group}], "total": 2}}"#);
        let (base, _caps) = s5b_stub("[]", &needs);
        let client = FeedClient::new(&base, Some("t".to_string()));
        let outcome = mirror.sync(&client, 5200).unwrap();
        assert_eq!((outcome.pulled, outcome.reconciled_acks), (0, 0));
        assert_eq!(mirror.unacked_count().unwrap(), 2);
        let _ = std::fs::remove_file(&path);
    }

    /// S5(b) required proof: an explicit acked fact carried on a pulled event
    /// marks exactly its named row — no other row in the mirror is touched,
    /// whether by the snapshot or by the event stream.
    #[test]
    fn explicit_ack_updates_one_row() {
        let (mut mirror, path) = s5b_temp_mirror();
        mirror.upsert(&s5b_event(1)).unwrap();
        mirror.upsert(&s5b_event(2)).unwrap();
        assert_eq!(mirror.unacked_count().unwrap(), 2);

        // First sync: the ack for the already-mirrored a2 arrives the way the
        // real service sends it — as an ack fact on the ack lane, NOT as a
        // re-sequenced event (the event cursor never re-delivers it). The
        // event page carries only one NEW routine event (a3); a2 must be
        // marked acked with the authoritative stamp and a1 stays untouched.
        let body = format!("[{}]", s5b_routine_json(3, "a3"));
        let acks = format!("[{}]", s5b_ack_json(1, "a2", 9000));
        let group = s5b_group_json("k1", &["a1"]);
        let needs = format!(r#"{{"groups": [{group}], "total": 1}}"#);
        let (base, _caps) = s5b_stub_full(&body, &acks, &needs);
        let client = FeedClient::new(&base, Some("t".to_string()));
        let outcome = mirror.sync(&client, 10_000).unwrap();
        assert_eq!(outcome.pulled, 1);
        assert_eq!(
            outcome.reconciled_acks, 1,
            "the explicit fact is the only ack"
        );
        assert_eq!(mirror.unacked_count().unwrap(), 1);
        assert_eq!(
            mirror.acked_ts_for("a2").unwrap(),
            Some(9000),
            "the authoritative stamp wins"
        );
        assert_eq!(
            mirror.acked_ts_for("a1").unwrap(),
            None,
            "no other row is touched by the fact"
        );

        // Second sync: nothing new, and an empty-but-valid snapshot. a1 stays
        // unacked — absence is not an ack (the pre-S5(b) bug).
        let (base, _caps) = s5b_stub("[]", r#"{"groups": [], "total": 0}"#);
        let client = FeedClient::new(&base, Some("t".to_string()));
        let outcome = mirror.sync(&client, 11_000).unwrap();
        assert_eq!(outcome.reconciled_acks, 0);
        assert_eq!(mirror.unacked_count().unwrap(), 1);
        let _ = std::fs::remove_file(&path);
    }

    /// S5(b) FIX1 required proof (closes P2): sync must fetch everything and
    /// validate the snapshot BEFORE any local write. A mismatched snapshot
    /// leaves the mirror byte-identical even though both pulls (the event with
    /// an ack stamp, the ack fact) succeeded — the pre-fix sync had already
    /// upserted rows and stamped acks before it rejected the snapshot.
    #[test]
    fn pulled_ack_fact_then_mismatched_snapshot_changes_nothing() {
        let (mut mirror, path) = s5b_temp_mirror();
        mirror.upsert(&s5b_event(1)).unwrap();
        mirror.upsert(&s5b_event(2)).unwrap();
        assert_eq!(mirror.unacked_count().unwrap(), 2);

        // Sync 1: the pulls carry work (a3 arrives stamped, a fact names a1),
        // but the snapshot is inconsistent.
        let body = format!("[{}]", s5b_ev_json(3, "a3", Some(9000)));
        let acks = format!("[{}]", s5b_ack_json(1, "a1", 9100));
        let (base, caps) = s5b_stub_full(&body, &acks, r#"{"groups": [], "total": 3}"#);
        let client = FeedClient::new(&base, Some("t".to_string()));
        assert!(
            matches!(mirror.sync(&client, 7_000), Err(FeedError::Parse(_))),
            "the mismatched snapshot must be rejected as a parse failure"
        );
        // Nothing landed: rows, stamps, cursors, stamp — all pre-sync.
        assert_eq!(mirror.row_count().unwrap(), 2);
        assert_eq!(mirror.acked_ts_for("a1").unwrap(), None);
        assert_eq!(mirror.acked_ts_for("a2").unwrap(), None);
        assert_eq!(mirror.unacked_count().unwrap(), 2);
        assert_eq!(mirror.cursor(), 0);
        assert_eq!(mirror.ack_cursor(), 0);
        assert_eq!(mirror.last_sync_ts(), None);
        // …and the sync stopped after the two reads, before the ack lane.
        let calls = caps.lock().unwrap().clone();
        assert_eq!(
            calls.len(),
            2,
            "events pull + needs-you, then stop: {calls:?}"
        );

        // Sync 2: the same pulls against a VALID snapshot now commit.
        let group = s5b_group_json("k", &["a2"]);
        let needs = format!(r#"{{"groups": [{group}], "total": 1}}"#);
        let (base, _caps) = s5b_stub_full(&body, &acks, &needs);
        let client = FeedClient::new(&base, Some("t".to_string()));
        let outcome = mirror.sync(&client, 8_000).unwrap();
        assert_eq!(outcome.pulled, 1);
        // a3 stamped on arrival + a1 via its ack fact.
        assert_eq!(outcome.reconciled_acks, 2);
        assert_eq!(mirror.acked_ts_for("a3").unwrap(), Some(9000));
        assert_eq!(mirror.acked_ts_for("a1").unwrap(), Some(9100));
        assert_eq!(mirror.acked_ts_for("a2").unwrap(), None);
        assert_eq!(mirror.unacked_count().unwrap(), 1);
        assert_eq!(mirror.last_sync_ts(), Some(8_000));
        let _ = std::fs::remove_file(&path);
    }

    /// A unique temp mirror per test (same convention as awili-persist).
    fn temp_mirror() -> (FeedMirror, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "awili-feed-mirror-{}-{}.db",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        (FeedMirror::open(&path).unwrap(), path)
    }

    fn event(seq: i64, severity: Severity, dedup_key: Option<&str>) -> Event {
        Event {
            seq,
            event_id: format!("e{seq}"),
            source: "test".to_string(),
            severity,
            title: format!("event {seq}"),
            body: String::new(),
            dedup_key: dedup_key.map(str::to_string),
            created_ts: 1000 + seq,
            acked_ts: None,
        }
    }

    #[test]
    fn fresh_mirror_is_empty_with_zero_cursor() {
        let (mirror, path) = temp_mirror();
        assert_eq!(mirror.cursor(), 0);
        assert_eq!(mirror.last_sync_ts(), None);
        assert_eq!(mirror.unacked_count().unwrap(), 0);
        assert!(mirror.needs_you_local().unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn needs_you_groups_by_dedup_key_and_counts() {
        let (mirror, path) = temp_mirror();
        // Three events on one condition, one keyless, one routine (excluded).
        mirror
            .upsert(&event(1, Severity::Attention, Some("gitea-auth")))
            .unwrap();
        mirror
            .upsert(&event(2, Severity::Attention, Some("gitea-auth")))
            .unwrap();
        mirror
            .upsert(&event(3, Severity::Critical, Some("gitea-auth")))
            .unwrap();
        mirror.upsert(&event(4, Severity::Critical, None)).unwrap();
        mirror.upsert(&event(5, Severity::Routine, None)).unwrap();

        assert_eq!(mirror.unacked_count().unwrap(), 4); // routine never counts
        let items = mirror.needs_you_local().unwrap();
        assert_eq!(items.len(), 2); // the storm collapsed to one row
                                    // Newest first: the keyless seq-4 event (created_ts 1004) beats the
                                    // group whose latest is seq 3 (1003)? No — 1004 > 1003, so seq 4 first.
        assert_eq!(items[0].event.seq, 4);
        assert_eq!(items[0].count, 1);
        assert_eq!(items[0].event_ids, vec!["e4"]);
        assert_eq!(items[1].event.seq, 3); // the group's NEWEST event displays
        assert_eq!(items[1].count, 3);
        assert_eq!(items[1].event_ids, vec!["e1", "e2", "e3"]);
        assert_eq!(items[1].severity, Severity::Critical);
        let _ = std::fs::remove_file(path);
    }

    /// Review SF1 regression: critical followed by attention on one condition
    /// (the contract test's exact storm). The row displays the newest event but
    /// must report the group's WORST severity — an unacked critical remains.
    #[test]
    fn group_severity_is_the_worst_not_the_latest() {
        let (mirror, path) = temp_mirror();
        mirror
            .upsert(&event(1, Severity::Critical, Some("gitea-auth")))
            .unwrap();
        mirror
            .upsert(&event(2, Severity::Attention, Some("gitea-auth")))
            .unwrap();

        let items = mirror.needs_you_local().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].event.seq, 2, "the newest event still displays");
        assert_eq!(items[0].event.severity, Severity::Attention);
        assert_eq!(
            items[0].severity,
            Severity::Critical,
            "the group must stay critical while an unacked critical member remains"
        );

        // Acking the critical member demotes the group to attention.
        mirror.mark_acked("e1", 2000).unwrap();
        let items = mirror.needs_you_local().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].severity, Severity::Attention);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn ack_hides_and_is_idempotent() {
        let (mirror, path) = temp_mirror();
        mirror.upsert(&event(1, Severity::Attention, None)).unwrap();
        mirror.mark_acked("e1", 2000).unwrap();
        assert_eq!(mirror.unacked_count().unwrap(), 0);
        // Second ack does not move the stamp.
        mirror.mark_acked("e1", 3000).unwrap();
        let ts: Option<i64> = mirror
            .conn
            .query_row(
                "SELECT acked_ts FROM events WHERE event_id = 'e1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ts, Some(2000));
        // Acking an id we never mirrored is a quiet no-op.
        mirror.mark_acked("never-seen", 2000).unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn upsert_is_idempotent_on_replay() {
        // A crash between upserting and committing the cursor means the next
        // sync re-pulls the same rows; REPLACE keeps them single.
        let (mirror, path) = temp_mirror();
        mirror.upsert(&event(1, Severity::Attention, None)).unwrap();
        mirror.upsert(&event(1, Severity::Attention, None)).unwrap();
        assert_eq!(mirror.unacked_count().unwrap(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mirror_survives_reopen() {
        let (mirror, path) = temp_mirror();
        mirror.upsert(&event(1, Severity::Critical, None)).unwrap();
        mirror.set_state("cursor", 1).unwrap();
        mirror.set_state("last_sync_ts", 999).unwrap();
        drop(mirror);
        let reopened = FeedMirror::open(&path).unwrap();
        assert_eq!(reopened.cursor(), 1);
        assert_eq!(reopened.last_sync_ts(), Some(999));
        assert_eq!(reopened.unacked_count().unwrap(), 1);
        let _ = std::fs::remove_file(path);
    }
}
