//! `awili-persist` — crash-safe conversation persistence (slice-2 spec §3, gripe 6;
//! slice-7 spec §1: multi-conversation schema + migration).
//!
//! A SQLite database (via `rusqlite`) at `~/Library/Application Support/Awili/
//! awili.db` (the platform data dir on other OSes). Every appended message is its
//! own committed transaction, so a `kill -9` mid-stream loses *at most* the
//! in-flight reply: the user's message and every completed turn are already
//! durable. The database runs in WAL mode so a committed transaction survives an
//! application crash — on reopen SQLite replays the write-ahead log even though
//! the connection was never closed cleanly.
//!
//! Design choice meeting "lose at most the in-flight reply": the user's message
//! is written the instant it is sent; the assistant's reply is written once, when
//! its stream completes. A crash part-way through generation therefore drops only
//! the unfinished reply, never a completed turn.
//!
//! ## Durability scope
//!
//! `synchronous=NORMAL` (with WAL) guarantees that a committed transaction
//! survives an **application** crash — a `kill -9`, panic, or any abrupt process
//! death — because the commit is durable in the WAL and replayed on reopen. It
//! does NOT guarantee that the newest WAL commit survives a **power loss** or OS
//! crash: in `NORMAL` mode SQLite does not fsync on every commit, so the last
//! commit(s) may be lost if the machine itself dies before the WAL reaches disk.
//! That trade is deliberate — it avoids an fsync per keystroke-driven write — and
//! is the right call for a chat transcript. The crash-safety claim this crate
//! makes and tests (see `tests/kill9.rs`) is the process-death case, not
//! power-loss durability of the very latest commit.
//!
//! ## Slice-7 schema (version 2) and migration
//!
//! Conversations carry a UUID id, a title, created/updated timestamps, and a
//! SOFT `archived` flag. `PRAGMA user_version` tracks the schema: `0` is either
//! an empty file or the pre-slice7 layout (integer conversation ids, no titles);
//! [`SCHEMA_VERSION`] is current. Opening a pre-slice7 database migrates it in
//! ONE transaction — every legacy conversation becomes `"Conversation N"` (N in
//! original id order) with a fresh UUID, and every message row is re-pointed at
//! its conversation's new id. Nothing is dropped until the copy has succeeded
//! inside the same transaction; a crash mid-migration rolls back to the legacy
//! layout, which migrates again on the next open. `tests/migration.rs` proves
//! the round-trip on a byte-for-byte pre-slice7 fixture.
//!
//! There is deliberately NO delete control for conversations — archive is a
//! flag (slice-7 spec §1). The single exception is
//! [`Store::truncate_messages`] (slice 12): regenerate and edit-resend are
//! user-intentional history REWRITES, so the rows after the kept prefix must
//! go — otherwise the next restore would resurrect the replaced turn next to
//! its replacement.
//!
//! ## Slice-8 schema (version 3): usage accounting
//!
//! `usage_events` records the token counts the provider reported for each turn
//! (slice-8 spec §1) — one row per `ProviderEvent::Usage`, carrying the
//! provider/model so cost can be priced per event even when the user cycles
//! models mid-day. Migration v2→v3 is purely additive (one `CREATE TABLE`);
//! the v0 legacy path now lands on v3 by running the v2 migration first.
//!
//! ## Slice-14 schema (version 4): full-text search
//!
//! `messages_fts` is an external-content FTS5 table mirroring
//! `messages(conversation_id, text)`, kept in sync by AFTER INSERT/DELETE/
//! UPDATE triggers. The v3→v4 migration creates it and backfills every
//! existing message row in the SAME transaction, so the backfill runs exactly
//! once: a populated v3 file lands on v4 fully indexed, and a crash
//! mid-migration rolls back to a clean v3 that migrates again on reopen.
//!
//! ## Slice-18 schema (version 5): prompt templates
//!
//! `prompt_templates` stores reusable system prompts ("standing directives")
//! by unique name, and `conversations` gains a nullable `template_id` binding
//! (`ON DELETE SET NULL`, so deleting a template silently unbinds the
//! conversations using it). The v4→v5 migration is purely additive — one
//! `CREATE TABLE` and one `ALTER TABLE ADD COLUMN`; no existing row is
//! touched.
//!
//! ## Slice-19 schema (version 6): file attachments
//!
//! `attachments` holds dropped/pasted file blobs; `staged_attachments` links
//! them to a conversation until the next send, cascading away with their
//! conversation or attachment. The v5→v6 migration is purely additive.

pub mod build_jobs;
pub mod capture_outbox;
/// RC2-CNCL2: who sits on a council and how each member answered.
pub mod council;
use build_jobs::BUILD_JOBS_DDL;
use capture_outbox::{CAPTURE_DELETION_INTENTS_DDL, CAPTURE_OUTBOX_DDL};
pub use council::{CouncilMemberRow, CouncilTurnRow};

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current Unix-epoch timestamp in seconds. Used for the `requested_ts`
/// field of deletion-intent rows. `chrono::Utc::now()` is unavailable
/// because this workspace pins `chrono` to `features = ["std"]` only;
/// `SystemTime` is the portable, dependency-free equivalent.
fn now_unix_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub const SCHEMA_VERSION: i64 = 14;

const SCHEMA_V2_VERSION: i64 = 2;
const SCHEMA_V3_VERSION: i64 = 3;
const SCHEMA_V4_VERSION: i64 = 4;
const SCHEMA_V5_VERSION: i64 = 5;
const SCHEMA_V6_VERSION: i64 = 6;
const SCHEMA_V7_VERSION: i64 = 7;
const SCHEMA_V8_VERSION: i64 = 8;
const SCHEMA_V9_VERSION: i64 = 9;
const SCHEMA_V10_VERSION: i64 = 10;
const SCHEMA_V11_VERSION: i64 = 11;
const SCHEMA_V12_VERSION: i64 = 12;
const SCHEMA_V13_VERSION: i64 = 13;
const SCHEMA_V14_VERSION: i64 = 14;

/// Hard bound on a single `capture_events` retrieval (slice 22, stage 1A).
/// A caller requesting more rows is clamped down to this value; the retrieval
/// is ALWAYS bounded. The bound exists so that a huge (e.g. `usize::MAX`)
/// request cannot silently become an unbounded full-table scan — and, on the
/// SQLite side, so that a negative `LIMIT` (which SQLite reads as *no* limit)
/// can never be produced.
pub const MAX_CAPTURE_RETRIEVAL: usize = 512;

const DAY_SECONDS: i64 = 86_400;

pub fn utc_day_start(unix_ts: i64) -> i64 {
    unix_ts.div_euclid(DAY_SECONDS) * DAY_SECONDS
}

fn day_start_ts(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time of every date")
        .and_utc()
        .timestamp()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationSummary {
    pub id: i64,
    pub started_ts: i64,
    pub preview: String,
    pub message_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredProject {
    pub id: i64,
    pub name: String,
    pub folder: String,
    pub instructions: String,
    pub created_ts: i64,
}

/// A persisted message row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessage {
    pub id: i64,
    pub role: String,
    pub text: String,
    pub ts: i64,
    /// `"normal"` for an ordinary turn or `"summary"` for a user-made summary
    /// (slice 21 §D4). Legacy rows (pre-v9) read as `"normal"`.
    pub kind: String,
    /// When `Some(summary_id)`, this row was replaced by that summary message and
    /// is skipped when assembling the prompt (still shown, dimmed). `None` = live.
    pub superseded_by: Option<i64>,
}

/// A persisted conversation row (slice 7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredConversation {
    /// UUID, stable for the conversation's lifetime.
    pub id: String,
    pub title: String,
    pub created_ts: i64,
    pub updated_ts: i64,
    /// SOFT archive flag — the row (and its messages) are never deleted.
    pub archived: bool,
    /// Whether the title was set by a manual rename (auto-titling stops).
    pub title_is_custom: bool,
    /// Working directory (workspace grouping key, Phase 4).
    pub cwd: Option<String>,
    /// Profile name this conversation belongs to (Phase 4).
    pub profile: Option<String>,
    /// Surface token: `"chat"` or `"work"`. NULL legacy rows read as `"chat"`.
    pub kind: String,
}

/// One UTC day's token usage for a single provider·model pair (slice 8). The
/// per-model grouping is what lets the app price a mixed day correctly — a
/// haiku morning and an opus afternoon must not be summed before pricing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageByModel {
    pub provider: String,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// A reusable system prompt the user stored by name (slice 18): injected as
/// the leading `system` message of any conversation it is applied to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptTemplate {
    pub id: i64,
    pub name: String,
    pub content: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One conversation matching a full-text search (slice 14): the id plus an
/// FTS5-built snippet of the best matching message, with `**bold**` markers
/// around the matched terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResult {
    pub conversation_id: String,
    pub snippet: String,
}

/// A conversation pulled out of the database for export (slice 17): the title
/// (or `"Untitled"`), its creation timestamp, and every message in
/// chronological order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedConversation {
    pub title: String,
    /// Unix seconds (`conversations.created_ts`).
    pub created_at: i64,
    pub messages: Vec<StoredMessage>,
}

impl ExportedConversation {
    /// Render the conversation as a self-contained Markdown document:
    /// a title heading, the export date line, then each message under its
    /// bolded role label, every block fenced by `---` separators. An empty
    /// conversation is just the header — no separators.
    pub fn to_markdown(&self) -> String {
        let mut out = format!(
            "# {}\n\n_Exported {}_\n",
            self.title,
            utc_date_string(self.created_at)
        );
        for m in &self.messages {
            out.push_str("\n---\n\n**");
            out.push_str(&role_label(&m.role));
            out.push_str("**\n\n");
            out.push_str(&m.text);
            out.push('\n');
        }
        if !self.messages.is_empty() {
            out.push_str("\n---\n");
        }
        out
    }

    /// The `~/Downloads` filename for this export:
    /// `<sanitised-title>-<YYYY-MM-DD>.md`, dated like the document header
    /// (one date source). A title that sanitises to nothing becomes
    /// `untitled`.
    pub fn filename(&self) -> String {
        let mut stem = sanitise_title(&self.title);
        if stem.is_empty() {
            stem = "untitled".to_string();
        }
        format!("{stem}-{}.md", utc_date_string(self.created_at))
    }
}

/// RC2-CNCL15: a council answer's heading names its member exactly as
/// shown (`member:Gemma 4 26B` reads "Gemma 4 26B").
pub const MEMBER_ROLE_PREFIX: &str = "member:";

/// The Markdown role heading: the three known roles get their fixed labels;
/// a council member's name is kept as written; anything else is Title Cased
/// as-is (slice 17 §1).
fn role_label(role: &str) -> String {
    match role {
        named if named.starts_with(MEMBER_ROLE_PREFIX) => {
            named[MEMBER_ROLE_PREFIX.len()..].to_string()
        }
        "user" => "User".to_string(),
        "assistant" => "Assistant".to_string(),
        "system" => "System".to_string(),
        other => other
            .split_whitespace()
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                    }
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The UTC calendar date of a unix timestamp, as `YYYY-MM-DD`.
fn utc_date_string(unix_ts: i64) -> String {
    chrono::DateTime::from_timestamp(unix_ts, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

/// Make a conversation title safe as a filename stem (slice 17 §2): lowercase,
/// every run of characters outside `[a-z0-9_-]` becomes ONE `-`, consecutive
/// `-` collapse, leading/trailing `-` are stripped, and the result is capped
/// at 60 chars (re-stripping a `-` the cut may expose).
pub fn sanitise_title(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '_' {
            c.to_ascii_lowercase()
        } else {
            '-'
        };
        if c == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push(c);
    }
    out.truncate(60); // all-ASCII by construction, so a char cap is a byte cap
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A stored file attachment (slice 19): the blob plus the metadata the tray
/// chip and the message formatter need.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub id: i64,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub content: Vec<u8>,
    pub created_at: i64,
}

/// Cap on the text rendered into a message from one text attachment
/// (slice 19 §2) — characters, not bytes.
const ATTACHMENT_TEXT_CHAR_LIMIT: usize = 8000;

/// Largest image (bytes) that becomes an inline data URI (slice 19 §2): 4 MB.
const ATTACHMENT_IMAGE_BYTE_LIMIT: i64 = 4_194_304;

/// Render one attachment as the text block prepended to the outgoing message
/// (slice 19 §2). Pure — no database access:
/// - text-like mime types become a fenced code block labelled with the
///   filename, truncated to 8000 chars with a trailing `…[truncated]`;
/// - images up to 4 MB become a Markdown image with a base64 data URI;
/// - everything else becomes a bracketed unsupported-attachment notice.
pub fn format_attachment_for_message(att: &Attachment) -> String {
    let text_like = att.mime_type.starts_with("text/")
        || matches!(
            att.mime_type.as_str(),
            "application/json" | "application/toml" | "application/xml"
        );
    if text_like {
        let content = String::from_utf8_lossy(&att.content);
        let mut body: String = content.chars().take(ATTACHMENT_TEXT_CHAR_LIMIT).collect();
        if content.chars().nth(ATTACHMENT_TEXT_CHAR_LIMIT).is_some() {
            body.push_str("…[truncated]");
        }
        return format!("```{}\n{}\n```", att.filename, body);
    }
    if att.mime_type.starts_with("image/") && att.size_bytes <= ATTACHMENT_IMAGE_BYTE_LIMIT {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&att.content);
        return format!("![{}](data:{};base64,{})", att.filename, att.mime_type, b64);
    }
    format!(
        "[Attachment: {} — unsupported type or too large (max 4 MB for images)]",
        att.filename
    )
}

/// Make an existing database file and its `-wal` / `-shm` companions 0600 when
/// any group/other bit is set. Missing files are skipped.
#[cfg(target_os = "linux")]
fn tighten_db_files(db: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for suffix in ["", "-wal", "-shm"] {
        let mut name = db.as_os_str().to_os_string();
        name.push(suffix);
        let p = PathBuf::from(name);
        match std::fs::metadata(&p) {
            Ok(m) if m.is_file() && m.permissions().mode() & 0o077 != 0 => {
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tighten_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn loose_db_files_are_tightened_and_missing_ones_skipped() {
        let dir = std::env::temp_dir().join(format!("awili-persist-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("awili.db");
        for (name, mode) in [("awili.db", 0o644), ("awili.db-wal", 0o664)] {
            let p = dir.join(name);
            std::fs::write(&p, b"x").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        // No -shm file on purpose.
        tighten_db_files(&db).unwrap();
        for name in ["awili.db", "awili.db-wal"] {
            let m = std::fs::metadata(dir.join(name)).unwrap().permissions().mode();
            assert_eq!(m & 0o777, 0o600, "{name}");
        }
        assert!(!dir.join("awili.db-shm").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// The persistence handle: owns the SQLite connection.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// The default database path: `<data_dir>/<APP_DIR_NAME>/awili.db`: `~/Library/Application Support/Awili/awili.db`
    /// on macOS (the spec), `$XDG_DATA_HOME/awili/awili.db` on Linux.
    pub fn default_path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join(awili_config::APP_DIR_NAME).join("awili.db"))
    }

    /// Open (creating if needed) the database at `path`, ensuring the parent
    /// directory exists, enabling WAL, creating the schema, and migrating a
    /// pre-slice7 file if that is what's on disk.
    pub fn open(path: &Path) -> Result<Store, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            // A failure here surfaces as a clearer open error below.
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open the database at [`default_path`](Self::default_path). Errors if the
    /// platform has no data dir.
    pub fn open_default() -> Result<Store, rusqlite::Error> {
        let path = Self::default_path()
            .ok_or_else(|| rusqlite::Error::InvalidPath(PathBuf::from("<no platform data dir>")))?;
        // Linux: the data folder is private to the account (0700), the
        // counterpart of the Windows owner-only ACL below.
        #[cfg(target_os = "linux")]
        if let Some(parent) = path.parent() {
            awili_config::ensure_private_app_dir(parent)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        }
        // The database file itself is created 0600 (SQLite copies its mode to the
        // -wal and -shm files).
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if !path.exists() {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            }
        }
        // Linux: a database from an earlier run (or another tool) that is group- or
        // world-accessible is tightened, with its -wal and -shm files. A failure
        // only warns: the 0700 folder above already protects the files.
        #[cfg(target_os = "linux")]
        if let Err(e) = tighten_db_files(&path) {
            eprintln!("awili: could not tighten database file modes: {e}");
        }
        // Windows: the app data folder (database, window state, engine logs,
        // briefs) is owner-only, the counterpart of a 0700 folder. Fail
        // closed: no database is opened in a folder that cannot be protected.
        #[cfg(windows)]
        if let Some(parent) = path.parent() {
            let plain = |e: std::io::Error| {
                // ToSqlConversionFailure displays as the inner message alone,
                // so the NTFS wording reaches the person unchanged.
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            };
            std::fs::create_dir_all(parent).map_err(plain)?;
            awili_winacl::ensure_owner_only(parent, true).map_err(|e| {
                plain(if e.kind() == std::io::ErrorKind::Unsupported {
                    e
                } else {
                    std::io::Error::new(
                        e.kind(),
                        format!(
                            "could not make {} private to your account, so no database was opened: {e}",
                            parent.display()
                        ),
                    )
                })
            })?;
        }
        Self::open(&path)
    }

    fn init(conn: Connection) -> Result<Store, rusqlite::Error> {
        // WAL + synchronous=NORMAL: committed transactions survive an application
        // crash (kill -9) and are replayed on reopen, without an fsync per commit.
        // busy_timeout: two connections may write (the serialized worker and
        // the UI-side attachment staging, slice 19) — briefly wait out the
        // other's write lock instead of surfacing SQLITE_BUSY.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;
             PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            if has_legacy_schema(&conn)? {
                migrate_v0_to_v2(&conn)?;
                migrate_v2_to_v3(&conn)?;
                migrate_v3_to_v4(&conn)?;
                migrate_v4_to_v5(&conn)?;
                migrate_v5_to_v6(&conn)?;
                migrate_v6_to_v7(&conn)?;
                migrate_v7_to_v8(&conn)?;
                migrate_v8_to_v9(&conn)?;
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V2_VERSION {
                migrate_v2_to_v3(&conn)?;
                migrate_v3_to_v4(&conn)?;
                migrate_v4_to_v5(&conn)?;
                migrate_v5_to_v6(&conn)?;
                migrate_v6_to_v7(&conn)?;
                migrate_v7_to_v8(&conn)?;
                migrate_v8_to_v9(&conn)?;
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V3_VERSION {
                migrate_v3_to_v4(&conn)?;
                migrate_v4_to_v5(&conn)?;
                migrate_v5_to_v6(&conn)?;
                migrate_v6_to_v7(&conn)?;
                migrate_v7_to_v8(&conn)?;
                migrate_v8_to_v9(&conn)?;
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V4_VERSION {
                migrate_v4_to_v5(&conn)?;
                migrate_v5_to_v6(&conn)?;
                migrate_v6_to_v7(&conn)?;
                migrate_v7_to_v8(&conn)?;
                migrate_v8_to_v9(&conn)?;
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V5_VERSION {
                migrate_v5_to_v6(&conn)?;
                migrate_v6_to_v7(&conn)?;
                migrate_v7_to_v8(&conn)?;
                migrate_v8_to_v9(&conn)?;
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V6_VERSION {
                migrate_v6_to_v7(&conn)?;
                migrate_v7_to_v8(&conn)?;
                migrate_v8_to_v9(&conn)?;
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V7_VERSION {
                migrate_v7_to_v8(&conn)?;
                migrate_v8_to_v9(&conn)?;
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V8_VERSION {
                migrate_v8_to_v9(&conn)?;
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V9_VERSION {
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V10_VERSION {
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V11_VERSION {
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V12_VERSION {
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            } else if version == SCHEMA_V13_VERSION {
                migrate_v13_to_v14(&conn)?;
            } else {
                // version 0 with no legacy tables = a brand-new file.
                create_schema_v5(&conn)?;
                migrate_v5_to_v6(&conn)?;
                migrate_v6_to_v7(&conn)?;
                migrate_v7_to_v8(&conn)?;
                migrate_v8_to_v9(&conn)?;
                migrate_v9_to_v10(&conn)?;
                migrate_v10_to_v11(&conn)?;
                migrate_v11_to_v12(&conn)?;
                migrate_v12_to_v13(&conn)?;
                migrate_v13_to_v14(&conn)?;
            }
        }
        // RC2-CNCL2: the council tables, additive and idempotent (no
        // version step, so older builds still read this file).
        conn.execute_batch(council::COUNCIL_DDL)?;
        council::add_sent_prompt_columns(&conn)?;
        // FK enforcement on for the lifetime of the connection (set OUTSIDE any
        // transaction — the pragma is a no-op inside one).
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let has_cached = conn
            .prepare("PRAGMA table_info(usage_events)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|c| c == "cached_input_tokens");
        if !has_cached {
            conn.execute_batch("ALTER TABLE usage_events ADD COLUMN cached_input_tokens INTEGER;")?;
        }
        Ok(Store { conn })
    }

    /// Upsert a small key/value app-state entry (used to remember the selected
    /// provider·model and the active conversation across launches). Its own
    /// committed transaction.
    pub fn set_app_state(&self, key: &str, value: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO app_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// Read an app-state entry, if present.
    pub fn get_app_state(&self, key: &str) -> Result<Option<String>, rusqlite::Error> {
        self.conn
            .query_row("SELECT value FROM app_state WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    /// Recover a malformed UI snapshot without losing its original bytes.
    /// The compare guard, backup and replacement commit together. A stale
    /// recovery or failed backup never changes the current snapshot.
    pub fn replace_app_state_with_backup(
        &self,
        key: &str,
        expected: &str,
        replacement: &str,
        backup_key: &str,
    ) -> Result<bool, rusqlite::Error> {
        if key == backup_key {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self.conn.unchecked_transaction()?;
        let current: Option<String> = tx
            .query_row("SELECT value FROM app_state WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        if current.as_deref() != Some(expected) {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO app_state (key, value) VALUES (?1, ?2)",
            rusqlite::params![backup_key, expected],
        )?;
        tx.execute(
            "UPDATE app_state SET value = ?2 WHERE key = ?1",
            rusqlite::params![key, replacement],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Create a conversation. The caller supplies the UUID (generated by
    /// `awili_core::new_conversation_id`) so the id is known synchronously on the
    /// UI side before the write commits on the worker.
    pub fn create_conversation(
        &self,
        id: &str,
        title: &str,
        created_ts: i64,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO conversations (id, title, created_ts, updated_ts, archived, title_is_custom)
             VALUES (?1, ?2, ?3, ?3, 0, 0)",
            rusqlite::params![id, title, created_ts],
        )?;
        Ok(())
    }

    /// RC2-APP (F0189): create a conversation WITH its kind in one statement,
    /// so a new Work conversation can never be left behind as a Chat row by a
    /// failed second write (the old create-then-set-kind pair).
    pub fn create_conversation_with_kind(
        &self,
        id: &str,
        title: &str,
        created_ts: i64,
        kind: &str,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO conversations
                 (id, title, created_ts, updated_ts, archived, title_is_custom, kind)
             VALUES (?1, ?2, ?3, ?3, 0, 0, ?4)",
            rusqlite::params![id, title, created_ts, kind],
        )?;
        Ok(())
    }

    /// Switch the internal Workshop adapter without replacing the thread.
    /// Missing/archived/other-family rows cannot acknowledge a conversion.
    pub fn promote_workshop_thread(&self, id: &str) -> Result<(), rusqlite::Error> {
        let changed = self.conn.execute(
            "UPDATE conversations SET kind = 'work'
             WHERE id = ?1 AND archived = 0 AND kind IN ('chat', 'work')",
            [id],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(rusqlite::Error::QueryReturnedNoRows)
        }
    }

    /// Materialize a thread before its first input needs storage. Its folder
    /// and helper binding commit with the row, so a file-only thread can be
    /// reopened even if Awili exits before its first model turn.
    pub fn create_conversation_with_binding(
        &self,
        id: &str,
        title: &str,
        created_ts: i64,
        kind: &str,
        cwd: Option<&str>,
        binding: (&str, &str),
    ) -> Result<(), rusqlite::Error> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO conversations
                 (id, title, created_ts, updated_ts, archived, title_is_custom, kind, cwd)
             VALUES (?1, ?2, ?3, ?3, 0, 0, ?4, ?5)",
            rusqlite::params![id, title, created_ts, kind, cwd],
        )?;
        tx.execute(
            "INSERT INTO app_state (key, value) VALUES (?1, ?2)",
            rusqlite::params![binding.0, binding.1],
        )?;
        tx.commit()
    }

    /// RC2-APP: remove app-state entries (a deleted conversation's own rows,
    /// such as its last Work run and its saved reply footers). Missing keys
    /// are fine. One committed transaction.
    pub fn delete_app_states(&self, keys: &[String]) -> Result<(), rusqlite::Error> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        for key in keys {
            self.conn
                .execute("DELETE FROM app_state WHERE key = ?1", [key])?;
        }
        tx.commit()
    }

    /// Every conversation, archived included (soft archive means the row is
    /// always here), in creation order.
    pub fn conversations(&self) -> Result<Vec<StoredConversation>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_ts, updated_ts, archived, title_is_custom, cwd, profile,
                    COALESCE(kind, 'chat')
             FROM conversations ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredConversation {
                id: row.get(0)?,
                title: row.get(1)?,
                created_ts: row.get(2)?,
                updated_ts: row.get(3)?,
                archived: row.get::<_, i64>(4)? != 0,
                title_is_custom: row.get::<_, i64>(5)? != 0,
                cwd: row.get(6)?,
                profile: row.get(7)?,
                kind: row.get(8)?,
            })
        })?;
        rows.collect()
    }

    /// Set a conversation's title. `custom` records whether this was a manual
    /// rename (blocks future auto-titling) or the first-message auto-title.
    pub fn rename_conversation(
        &self,
        id: &str,
        title: &str,
        custom: bool,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE conversations SET title = ?2, title_is_custom = ?3 WHERE id = ?1",
            rusqlite::params![id, title, custom as i64],
        )?;
        Ok(())
    }

    /// Flip the SOFT archive flag. This is the only thing "archive" does to the
    /// database — messages and metadata stay exactly where they are.
    /// HARD-delete a conversation and everything hanging off it: messages
    /// (the FTS delete-trigger keeps the index in step), usage events, and
    /// staged-attachment links — explicitly, in one transaction, so the
    /// cleanup never depends on the `foreign_keys` pragma being on. The
    /// attachment BLOBs themselves stay (they may be staged elsewhere).
    pub fn delete_conversation(&self, id: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let all = (|| -> Result<(), rusqlite::Error> {
            // 1B-b: run the shared bookkeeping helper inside this same
            // transaction BEFORE anything is deleted. `keep` is 0 — the
            // whole-conversation delete victim set is every message of the
            // conversation (the helper computes it from the `messages` table,
            // so no Rust-side `Vec` of ids is ever materialized).
            // `never_sent` outbox rows are dropped, `possibly_sent` ones
            // become deletion-intent rows, and ALL victim outbox rows
            // (including the possibly_sent payloads) are removed.
            self.bookkeeping_delete_outbox(id, 0, now_unix_ts(), "conversation_delete")?;
            self.conn.execute(
                "DELETE FROM staged_attachments WHERE conversation_id = ?1",
                rusqlite::params![id],
            )?;
            self.conn.execute(
                "DELETE FROM usage_events WHERE conversation_id = ?1",
                rusqlite::params![id],
            )?;
            // RC2-CNCL2: a council's seats and turns go with it.
            self.delete_council_rows(id)?;
            self.conn.execute(
                "DELETE FROM messages WHERE conversation_id = ?1",
                rusqlite::params![id],
            )?;
            self.conn.execute(
                "DELETE FROM conversations WHERE id = ?1",
                rusqlite::params![id],
            )?;
            Ok(())
        })();
        match all {
            Ok(()) => self.conn.execute_batch("COMMIT"),
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn set_archived(&self, id: &str, archived: bool) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE conversations SET archived = ?2 WHERE id = ?1",
            rusqlite::params![id, archived as i64],
        )?;
        Ok(())
    }

    /// Bump a conversation's `updated_ts` (message activity).
    pub fn touch_conversation(&self, id: &str, updated_ts: i64) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE conversations SET updated_ts = ?2 WHERE id = ?1",
            rusqlite::params![id, updated_ts],
        )?;
        Ok(())
    }

    /// Set the working directory for a conversation (workspace grouping).
    pub fn set_conversation_cwd(&self, id: &str, cwd: Option<&str>) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE conversations SET cwd = ?2 WHERE id = ?1",
            rusqlite::params![id, cwd],
        )?;
        Ok(())
    }

    /// Set the surface kind (`"chat"` or `"work"`) for a conversation.
    pub fn set_conversation_kind(&self, id: &str, kind: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE conversations SET kind = ?2 WHERE id = ?1",
            rusqlite::params![id, kind],
        )?;
        Ok(())
    }

    /// Set the profile for a conversation.
    pub fn set_conversation_profile(
        &self,
        id: &str,
        profile: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE conversations SET profile = ?2 WHERE id = ?1",
            rusqlite::params![id, profile],
        )?;
        Ok(())
    }

    /// Append a message to a conversation, returning its row id. Each call is its
    /// own committed transaction (autocommit) — that is the durability guarantee.
    /// Fails (FK violation) for an unknown conversation id.
    pub fn append_message(
        &self,
        conversation_id: &str,
        role: &str,
        text: &str,
        ts: i64,
    ) -> Result<i64, rusqlite::Error> {
        self.append_message_kind(conversation_id, role, text, ts, "normal")
    }

    /// [`Self::append_message`] with the row's `kind` (RC2-RUNF: `"plan_click"`
    /// marks a row Awili wrote for a plan-card click; `"normal"` is every
    /// other turn). The column already exists (v9), so older databases need
    /// no migration: their rows keep reading as `"normal"`.
    pub fn append_message_kind(
        &self,
        conversation_id: &str,
        role: &str,
        text: &str,
        ts: i64,
        kind: &str,
    ) -> Result<i64, rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO messages (conversation_id, role, text, ts, kind) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![conversation_id, role, text, ts, kind],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Append a message and, in the SAME committed transaction, record the
    /// offline-capture outbox row for it (slice 22, stage 1A).
    ///
    /// `capture` is `Some` only when the caller has already validated the
    /// role (`user` or `assistant`) and holds a valid non-empty payload/version;
    /// the DB enforces the non-empty CHECK on its own. A valid `role` is
    /// required because the outbox CHECK only allows `actor IN ('user','assistant')`.
    /// Pass `None` to get the plain `append_message` behavior (no outbox row).
    ///
    /// On `Some`, the method mints exactly one `awili:<uuid-v4>` source_event_id
    /// and inserts the outbox row in the same transaction as the message insert.
    /// If either insert fails, the whole transaction rolls back and neither row
    /// persists.
    pub fn append_message_with_capture(
        &self,
        conversation_id: &str,
        role: &str,
        text: &str,
        ts: i64,
        capture: Option<&capture_outbox::OfflineCaptureInput>,
    ) -> Result<capture_outbox::CaptureAppendReceipt, rusqlite::Error> {
        // The outbox CHECK allows `actor IN ('user','assistant')` only, so a
        // capture is only valid for those two roles. Validate explicitly BEFORE
        // starting the transaction (rather than relying on the CHECK to roll
        // the write back): a `tool`/`system` (or any other) role must fail fast
        // with zero rows written, not leave a partial write to be unwound.
        if capture.is_some() {
            match role {
                "user" | "assistant" => {}
                other => {
                    return Err(rusqlite::Error::ToSqlConversionFailure(
                        format!("capture role must be 'user' or 'assistant', got '{other}'",)
                            .into(),
                    ))
                }
            }
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO messages (conversation_id, role, text, ts) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![conversation_id, role, text, ts],
        )?;
        let message_id = tx.last_insert_rowid();
        let source_event_id = match capture {
            Some(input) => {
                let event_id = format!("awili:{}", uuid::Uuid::new_v4());
                let b = &input.binding;
                tx.execute(
                    "INSERT INTO capture_outbox (
                         source_event_id, message_id, conversation_id,
                         binding_epoch, installation_id, tenant_id, user_id,
                         consumer_id, actor, source_ts, redacted_content, adapter_version
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    rusqlite::params![
                        event_id,
                        message_id,
                        conversation_id,
                        b.epoch,
                        b.installation_id,
                        b.tenant_id,
                        b.user_id,
                        b.consumer_id.to_string(),
                        role,
                        ts,
                        input.redacted_content,
                        input.adapter_version
                    ],
                )?;
                Some(event_id)
            }
            None => None,
        };
        tx.commit()?;
        Ok(capture_outbox::CaptureAppendReceipt {
            message_id,
            source_event_id,
        })
    }

    /// Read the outbox rows for one exact capture binding, in stable order
    /// (`source_ts ASC, message_id ASC`), bounded by `limit`. Stage 1A is
    /// read-only bookkeeping: this method never mutates the outbox.
    /// A different binding (any of the five components) returns no rows.
    ///
    /// Excludes rows already marked `possibly_sent` (stage 1B-a exposure):
    /// a row that may have been delivered to a remote recipient is no longer
    /// a pending local capture, so retrieval of the pending queue returns
    /// only `never_sent` rows.
    ///
    /// The requested `limit` is clamped down to [`MAX_CAPTURE_RETRIEVAL`]
    /// before it is converted to the SQLite `LIMIT` parameter: a huge caller
    /// value must never turn the retrieval unbounded. (In particular
    /// `usize::MAX` must not be allowed to survive to SQLite, where a negative
    /// `LIMIT` is treated as *no* limit.) `0` returns an empty list.
    pub fn capture_events(
        &self,
        binding: &capture_outbox::OfflineCaptureBinding,
        limit: usize,
    ) -> Result<Vec<capture_outbox::StoredCaptureEvent>, rusqlite::Error> {
        // Clamp to the hard retrieval bound first, then convert. usize -> i64
        // is exact for values <= 512, so the `expect` can never fire; it exists
        // only so this code has no `as`-cast path into SQL.
        let effective_limit: i64 = (limit.min(MAX_CAPTURE_RETRIEVAL))
            .try_into()
            .expect("clamped value fits in i64");
        let mut stmt = self.conn.prepare(
            "SELECT source_event_id, message_id, conversation_id,
                    binding_epoch, installation_id, tenant_id, user_id,
                    consumer_id, actor, source_ts, redacted_content, adapter_version
             FROM capture_outbox
             WHERE binding_epoch = ?1 AND installation_id = ?2 AND tenant_id = ?3
               AND user_id = ?4 AND consumer_id = ?5
               AND exposure = 'never_sent'
             ORDER BY source_ts ASC, message_id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                binding.epoch,
                binding.installation_id,
                binding.tenant_id,
                binding.user_id,
                binding.consumer_id.to_string(),
                effective_limit
            ],
            |row| {
                Ok(capture_outbox::StoredCaptureEvent {
                    source_event_id: row.get(0)?,
                    message_id: row.get(1)?,
                    conversation_id: row.get(2)?,
                    binding: capture_outbox::OfflineCaptureBinding {
                        epoch: row.get(3)?,
                        installation_id: row.get(4)?,
                        tenant_id: row.get(5)?,
                        user_id: row.get(6)?,
                        consumer_id: uuid::Uuid::parse_str(&row.get::<_, String>(7)?).map_err(
                            |e| {
                                rusqlite::Error::ToSqlConversionFailure(std::convert::Into::into(
                                    e.to_string(),
                                ))
                            },
                        )?,
                    },
                    actor: row.get(8)?,
                    ts: row.get(9)?,
                    redacted_content: row.get(10)?,
                    adapter_version: row.get(11)?,
                })
            },
        )?;
        rows.collect()
    }

    /// One-way exposure transition for a captured source event (stage 1B-a:
    /// exposure only; deletion-intent bookkeeping is 1B-b, which does NOT
    /// clear intents — only a future, independently verified remote protocol
    /// may do that).
    ///
    /// Looks up the `capture_outbox` row by `source_event_id` AND all five
    /// binding components, and in ONE transaction:
    ///
    /// * no row under this exact full binding → [`MarkExposure::NotFound`]
    ///   (identical to the deleted-source case below, on purpose — the store
    ///   must not reveal whether the source exists at all; nothing mutates);
    /// * a deletion intent exists for this source → [`MarkExposure::NotFound`]
    ///   (a deleted source is refused, never resurrected — even if the outbox
    ///   row is still `possibly_sent`; nothing mutates);
    /// * row is already `possibly_sent` → [`MarkExposure::AlreadyMarked`]
    ///   (idempotent; grants no new authority; nothing mutates);
    /// * row is `never_sent` AND at least one deletion intent for the SAME
    ///   FULL binding exists → [`MarkExposure::BindingPaused`] (new exposure
    ///   for this binding is paused; nothing mutates);
    /// * row is `never_sent` and no same-binding intent exists → the row is
    ///   atomically set to `possibly_sent` and [`MarkExposure::Marked`] is
    ///   returned.
    ///
    /// There is NO reverse transition and NO public setter: `exposure` can
    /// only move `never_sent` → `possibly_sent`, and only through this
    /// method. This marks *possibility* of delivery only — it is NOT a send
    /// receipt (acceptance, acknowledgement, leases, retries are later units).
    ///
    /// The one-way rule is enforced by this API, not by the schema: the
    /// `CHECK` on `exposure` only validates the two legal values. Every
    /// failure to read the outbox row or the intent state is propagated as
    /// `Err`, never coerced into a result: an unknown exposure value
    /// (anything other than the literal `never_sent`) fails with an error
    /// rather than being treated as permission to mark, and no exposure
    /// mutation is ever committed when a read fails.
    pub fn mark_capture_possibly_sent(
        &self,
        binding: &capture_outbox::OfflineCaptureBinding,
        source_event_id: &str,
    ) -> Result<capture_outbox::MarkExposure, rusqlite::Error> {
        let tx = self.conn.unchecked_transaction()?;
        // Every read runs on the transaction (not the bare connection) and
        // distinguishes a genuine no-row from a database error:
        // `optional()` propagates `Err` instead of failing open into a
        // result. Only a literal no-row may produce `NotFound` / "no
        // intent"; any read failure aborts the transaction uncommitted, so
        // no exposure mutation is ever committed on a read error.
        let exposure: Option<String> = tx
            .query_row(
                "SELECT exposure FROM capture_outbox
                 WHERE source_event_id = ?1
                   AND binding_epoch = ?2 AND installation_id = ?3
                   AND tenant_id = ?4 AND user_id = ?5 AND consumer_id = ?6",
                rusqlite::params![
                    source_event_id,
                    binding.epoch,
                    binding.installation_id,
                    binding.tenant_id,
                    binding.user_id,
                    binding.consumer_id.to_string()
                ],
                |row| row.get(0),
            )
            .optional()?;
        match exposure {
            // No row for this source under this exact full binding.
            None => Ok(capture_outbox::MarkExposure::NotFound),
            Some(exposure) => {
                // Deleted source: a deletion intent for this source event
                // refuses the mark (never resurrect), reported as NotFound so
                // the store does not reveal that the source exists.
                let deleted = tx
                    .query_row(
                        "SELECT 1 FROM capture_deletion_intents
                         WHERE source_event_id = ?1",
                        rusqlite::params![source_event_id],
                        |_| Ok(true),
                    )
                    .optional()?
                    .unwrap_or(false);
                if deleted {
                    return Ok(capture_outbox::MarkExposure::NotFound);
                }
                if exposure == "possibly_sent" {
                    // Idempotent: already marked; no new authority granted.
                    return Ok(capture_outbox::MarkExposure::AlreadyMarked);
                }
                if exposure == "never_sent" {
                    // Is ANY deletion intent for this same FULL binding
                    // pending? (1B-a has no "cleared" status; presence =
                    // paused.)
                    let paused = tx
                        .query_row(
                            "SELECT 1 FROM capture_deletion_intents
                             WHERE binding_epoch = ?1 AND installation_id = ?2
                               AND tenant_id = ?3 AND user_id = ?4
                               AND consumer_id = ?5",
                            rusqlite::params![
                                binding.epoch,
                                binding.installation_id,
                                binding.tenant_id,
                                binding.user_id,
                                binding.consumer_id.to_string()
                            ],
                            |_| Ok(true),
                        )
                        .optional()?
                        .unwrap_or(false);
                    if paused {
                        return Ok(capture_outbox::MarkExposure::BindingPaused);
                    }

                    // One-way transition. The `exposure` CHECK guarantees it
                    // can only become `possibly_sent`.
                    let changed = tx.execute(
                        "UPDATE capture_outbox
                         SET exposure = 'possibly_sent'
                         WHERE source_event_id = ?1
                           AND binding_epoch = ?2 AND installation_id = ?3
                           AND tenant_id = ?4 AND user_id = ?5
                           AND consumer_id = ?6",
                        rusqlite::params![
                            source_event_id,
                            binding.epoch,
                            binding.installation_id,
                            binding.tenant_id,
                            binding.user_id,
                            binding.consumer_id.to_string()
                        ],
                    )?;
                    debug_assert_eq!(
                        changed, 1,
                        "row read as never_sent cannot have been updated away"
                    );
                    tx.commit()?;
                    return Ok(capture_outbox::MarkExposure::Marked);
                }
                // Unknown exposure value: corrupt state. Never treat anything
                // besides the literal `never_sent` as permission to mark.
                Err(rusqlite::Error::ToSqlConversionFailure(
                    "unknown capture outbox exposure value".into(),
                ))
            }
        }
    }

    /// Read the deletion-intent rows for one exact capture binding (stage
    /// 1B-a), in stable order (`requested_ts ASC, source_event_id ASC`),
    /// bounded by `limit`. This unit exposes READ authority only — there is
    /// no application API to create or clear intents (the independently
    /// reviewed 1B-b unit performs the local deletion bookkeeping and
    /// populates the table; it does NOT clear intents — only a future,
    /// independently verified remote protocol may do that. Tests insert
    /// synthetic fixtures directly through the test connection).
    ///
    /// A different binding (any of the five components) returns no rows.
    ///
    /// The requested `limit` is clamped down to [`MAX_CAPTURE_RETRIEVAL`]
    /// before it is converted to the SQLite `LIMIT` parameter, exactly as in
    /// [`capture_events`](Self::capture_events): `usize::MAX` must never
    /// survive to SQLite (a negative `LIMIT` would mean *no* limit).
    /// `0` returns an empty list.
    pub fn capture_deletion_intents(
        &self,
        binding: &capture_outbox::OfflineCaptureBinding,
        limit: usize,
    ) -> Result<Vec<capture_outbox::StoredDeletionIntent>, rusqlite::Error> {
        // Clamp to the hard retrieval bound first, then convert. usize -> i64
        // is exact for values <= 512, so the `expect` can never fire; it exists
        // only so this code has no `as`-cast path into SQL.
        let effective_limit: i64 = (limit.min(MAX_CAPTURE_RETRIEVAL))
            .try_into()
            .expect("clamped value fits in i64");
        let mut stmt = self.conn.prepare(
            "SELECT source_event_id, binding_epoch, installation_id,
                    tenant_id, user_id, consumer_id, conversation_id,
                    source_ts, requested_ts, cause
             FROM capture_deletion_intents
             WHERE binding_epoch = ?1 AND installation_id = ?2 AND tenant_id = ?3
               AND user_id = ?4 AND consumer_id = ?5
             ORDER BY requested_ts ASC, source_event_id ASC
             LIMIT ?6",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                binding.epoch,
                binding.installation_id,
                binding.tenant_id,
                binding.user_id,
                binding.consumer_id.to_string(),
                effective_limit
            ],
            |row| {
                Ok(capture_outbox::StoredDeletionIntent {
                    source_event_id: row.get(0)?,
                    binding: capture_outbox::OfflineCaptureBinding {
                        epoch: row.get(1)?,
                        installation_id: row.get(2)?,
                        tenant_id: row.get(3)?,
                        user_id: row.get(4)?,
                        consumer_id: uuid::Uuid::parse_str(&row.get::<_, String>(5)?).map_err(
                            |e| {
                                rusqlite::Error::ToSqlConversionFailure(std::convert::Into::into(
                                    e.to_string(),
                                ))
                            },
                        )?,
                    },
                    conversation_id: row.get(6)?,
                    source_ts: row.get(7)?,
                    requested_ts: row.get(8)?,
                    cause: row.get(9)?,
                })
            },
        )?;
        rows.collect()
    }

    /// Stage 1B-b: the atomic local deletion bookkeeping shared by
    /// [`truncate_messages`](Self::truncate_messages) and
    /// [`delete_conversation`](Self::delete_conversation).
    ///
    /// MUST run inside an already-open `BEGIN IMMEDIATE` transaction — this
    /// method performs no `BEGIN` of its own and commits nothing. The victim
    /// set (the conversation's messages OUTSIDE the first `keep` in restore
    /// order `ts ASC, id ASC`) is computed entirely in SQL and is NEVER
    /// materialised into a `Vec` in Rust. Outbox rows are matched by the
    /// (conversation_id, victim message_id) pair, never by `message_id`
    /// alone — a reused rowid may legitimately belong to a new message in a
    /// different conversation.
    ///
    /// The victim subquery is repeated in each statement rather than cached:
    /// the caller does not delete the victim messages until AFTER this
    /// helper returns, so the set is stable for the whole transaction.
    ///
    /// Steps, in order (any error propagates with the transaction left open
    /// for the caller to roll back — the caller owns commit/rollback):
    ///
    /// 1. Fail-closed validation: any victim outbox row whose `exposure` is
    ///    neither `never_sent` nor `possibly_sent` → `Err` before any
    ///    mutation (same policy as
    ///    [`mark_capture_possibly_sent`](Self::mark_capture_possibly_sent)).
    /// 2. Conflict validation: an EXISTING `capture_deletion_intents` row
    ///    for a victim source ID but with a different binding,
    ///    `conversation_id` or `source_ts` → `Err` before any mutation (a conflict the store
    ///    must not silently resolve).
    /// 3. For each `possibly_sent` victim without a matching source intent,
    ///    insert a deletion-intent row carrying the EXACT original
    ///    `source_event_id`, the full 5-field binding, `conversation_id`,
    ///    the original `source_ts`, the caller-supplied FRESH
    ///    `requested_ts`, and the `cause`. Identity + metadata only — no
    ///    payload, hash or token. An existing intent matching the source is
    ///    left UNCHANGED (idempotency — including its `requested_ts` and
    ///    `cause`). No `INSERT OR IGNORE`, no replacing an old intent, ever.
    /// 4. Delete the `capture_outbox` row of EVERY victim — unconditionally,
    ///    including `possibly_sent` victims. The intent row is the complete
    ///    local record for a `possibly_sent` source; its payload must not
    ///    remain in local storage once the message is gone.
    ///
    /// `keep == usize::MAX` (retain all messages) is a no-op on the
    /// retention side: the subquery's `LIMIT` is clamped to `i64::MAX`, so
    /// the retained prefix covers every message and the victim set is empty.
    fn bookkeeping_delete_outbox(
        &self,
        conversation_id: &str,
        keep: usize,
        requested_ts: i64,
        cause: &str,
    ) -> Result<(), rusqlite::Error> {
        // `keep` is applied through a SQL `LIMIT`; clamp to the i64 range so a
        // `usize::MAX` (retain all messages) or any hostile value can
        // never cast to a negative bound. `LIMIT i64::MAX` keeps every row,
        // so the victim set is empty and the helper is a no-op.
        let limit: i64 = if keep >= i64::MAX as usize {
            i64::MAX
        } else {
            keep as i64
        };

        // Repeated victim subquery: the conversation's message ids that fall
        // OUTSIDE the retained prefix (restore order = `ts ASC, id ASC`).
        // Repeated verbatim in each statement (not cached in a temp table or
        // a `Vec`): the victim messages are only deleted by the caller AFTER
        // this helper returns, so the set is stable for the whole
        // transaction. `?1` = conversation_id, `?2` = limit.
        const VICTIM: &str = "
            SELECT id FROM messages
             WHERE conversation_id = ?1
               AND id NOT IN (
                   SELECT id FROM messages
                    WHERE conversation_id = ?1
                    ORDER BY ts ASC, id ASC
                    LIMIT ?2
               )
        ";

        // ---- Step 1: fail-closed exposure validation (no mutation) -------
        // Any victim outbox row with an unknown exposure is an error BEFORE
        // anything is touched, so the caller's rollback leaves the outbox,
        // the intents and the messages exactly as they were.
        let unknown: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*)
                   FROM capture_outbox o
                  WHERE o.conversation_id = ?1
                    AND o.message_id IN ({0})
                    AND o.exposure NOT IN ('never_sent', 'possibly_sent')",
                VICTIM
            ),
            rusqlite::params![conversation_id, limit],
            |r| r.get(0),
        )?;
        if unknown > 0 {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "capture outbox row with unknown exposure among deletion victims".into(),
            ));
        }

        // Existing source IDs must refer to the same immutable identity.
        // Cause and requested_ts belong to the first intent and remain unchanged.
        let conflict: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM capture_outbox o
                 JOIN capture_deletion_intents i ON i.source_event_id = o.source_event_id
                 WHERE o.conversation_id = ?1 AND o.message_id IN ({0})
                   AND (i.binding_epoch <> o.binding_epoch
                     OR i.installation_id <> o.installation_id
                     OR i.tenant_id <> o.tenant_id OR i.user_id <> o.user_id
                     OR i.consumer_id <> o.consumer_id
                     OR i.conversation_id <> o.conversation_id
                     OR i.source_ts <> o.source_ts)",
                VICTIM
            ),
            rusqlite::params![conversation_id, limit],
            |r| r.get(0),
        )?;
        if conflict > 0 {
            return Err(rusqlite::Error::ToSqlConversionFailure(
                "existing deletion intent conflicts with victim identity".into(),
            ));
        }

        // ---- Step 3: insert intents for newly-intended possibly_sent -----
        // Identity/metadata only — no payload, hash or token leaves the
        // outbox into the intent table. Fresh `requested_ts` + `cause`.
        // `NOT EXISTS` keeps any existing identical intent untouched
        // (idempotency); there is no `INSERT OR IGNORE` and no replacement
        // path.
        self.conn.execute(
            &format!(
                "INSERT INTO capture_deletion_intents
                    (source_event_id, binding_epoch, installation_id,
                     tenant_id, user_id, consumer_id, conversation_id,
                     source_ts, requested_ts, cause)
                 SELECT o.source_event_id, o.binding_epoch, o.installation_id,
                        o.tenant_id, o.user_id, o.consumer_id,
                        o.conversation_id, o.source_ts, ?3, ?4
                   FROM capture_outbox o
                  WHERE o.conversation_id = ?1
                    AND o.message_id IN ({0})
                    AND o.exposure = 'possibly_sent'
                    AND NOT EXISTS (
                        SELECT 1
                           FROM capture_deletion_intents i
                          WHERE i.source_event_id = o.source_event_id

                    )",
                VICTIM
            ),
            rusqlite::params![conversation_id, limit, requested_ts, cause],
        )?;

        // ---- Step 4: delete ALL victim outbox rows (payload removal) -----
        // Unconditional — including `possibly_sent` victims, whose intent
        // row (step 3) is the complete local record. Runs only after steps
        // 1–3 succeeded, so a validation failure deletes nothing.
        self.conn.execute(
            &format!(
                "DELETE FROM capture_outbox
                  WHERE conversation_id = ?1
                    AND message_id IN ({0})",
                VICTIM
            ),
            rusqlite::params![conversation_id, limit],
        )?;

        Ok(())
    }

    /// Replace a message's text (used to finalise a streamed reply if the app
    /// chooses to checkpoint partial text). Not required by the durability model
    /// but available for richer crash recovery.
    pub fn update_message(&self, message_id: i64, text: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE messages SET text = ?1 WHERE id = ?2",
            rusqlite::params![text, message_id],
        )?;
        Ok(())
    }

    /// Delete every message of `conversation_id` AFTER the first `keep` rows in
    /// restore order (`ts ASC, id ASC` — the exact order [`messages`](Self::messages)
    /// returns). Slice 12's regenerate/edit-resend path: the caller is rewriting
    /// history from a point onward, and the replaced rows must not survive to the
    /// next restore. Returns how many rows were deleted; `keep >=` the message
    /// count is a no-op. Other conversations are untouched.
    pub fn truncate_messages(
        &self,
        conversation_id: &str,
        keep: usize,
    ) -> Result<usize, rusqlite::Error> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let deleted = self.truncate_messages_in_transaction(conversation_id, keep)?;
        tx.commit()?;
        Ok(deleted)
    }

    /// Atomically replace the tail with one ordinary message. If the replacement
    /// insert fails, original messages, outbox rows and deletion intents roll back.
    /// This does not enroll or enqueue capture for the replacement.
    pub fn truncate_and_append_message(
        &self,
        conversation_id: &str,
        keep: usize,
        role: &str,
        text: &str,
        ts: i64,
    ) -> Result<i64, rusqlite::Error> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        self.truncate_messages_in_transaction(conversation_id, keep)?;
        let id = self.append_message(conversation_id, role, text, ts)?;
        tx.commit()?;
        Ok(id)
    }

    // Caller owns the transaction. Victims remain stable until bookkeeping ends.
    fn truncate_messages_in_transaction(
        &self,
        conversation_id: &str,
        keep: usize,
    ) -> Result<usize, rusqlite::Error> {
        let limit = i64::try_from(keep).unwrap_or(i64::MAX);
        self.bookkeeping_delete_outbox(conversation_id, keep, now_unix_ts(), "truncate")?;
        self.conn.execute(
            "DELETE FROM messages WHERE conversation_id = ?1 AND id NOT IN (
                SELECT id FROM messages WHERE conversation_id = ?1
                ORDER BY ts ASC, id ASC LIMIT ?2)",
            rusqlite::params![conversation_id, limit],
        )
    }

    /// Recent conversations, newest first, with a preview of the first user
    /// message. Limited to `limit` entries.
    pub fn list_conversations(
        &self,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.started_ts,
                    COALESCE(
                        (SELECT SUBSTR(m.text, 1, 80) FROM messages m
                         WHERE m.conversation_id = c.id AND m.role = 'user'
                         ORDER BY m.ts ASC, m.id ASC LIMIT 1),
                        ''
                    ) AS preview,
                    (SELECT COUNT(*) FROM messages m WHERE m.conversation_id = c.id) AS cnt
             FROM conversations c
             ORDER BY c.id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(ConversationSummary {
                id: row.get(0)?,
                started_ts: row.get(1)?,
                preview: row.get(2)?,
                message_count: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    // ---- Projects ---------------------------------------------------------------

    /// Import the legacy Code list into the shared project table exactly once.
    /// Existing rows (including their instructions) win by folder. The marker
    /// and active folder commit with the imports, so a deleted shared project
    /// cannot reappear from the retained legacy JSON after a restart.
    pub fn import_legacy_projects_once(
        &self,
        projects: &[(String, String)],
        active_folder: Option<&str>,
        ts: i64,
    ) -> Result<(), rusqlite::Error> {
        let tx = self.conn.unchecked_transaction()?;
        let imported: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM app_state WHERE key = 'workshop_projects_imported')",
            [],
            |row| row.get(0),
        )?;
        if imported {
            return Ok(());
        }
        for (name, folder) in projects {
            tx.execute(
                "INSERT OR IGNORE INTO projects (name, folder, instructions, created_ts)
                 VALUES (?1, ?2, '', ?3)",
                rusqlite::params![name, folder, ts],
            )?;
        }
        if let Some(folder) = active_folder {
            tx.execute(
                "INSERT OR IGNORE INTO app_state (key, value)
                 VALUES ('workshop_active_project_folder', ?1)",
                [folder],
            )?;
        }
        tx.execute(
            "INSERT INTO app_state (key, value) VALUES ('workshop_projects_imported', '1')",
            [],
        )?;
        tx.commit()
    }

    /// Add a project (name + folder path + optional instructions). Idempotent
    /// by `folder` (`UNIQUE`): saving an already-saved folder is a no-op, so a
    /// double-click — or two dispatches racing before the UI mirror refreshes —
    /// can never create duplicate rows. Returns the new row id, or the last
    /// insert id when the folder already existed (callers that need the row read
    /// it back via `list_projects`).
    pub fn add_project(
        &self,
        name: &str,
        folder: &str,
        instructions: &str,
        ts: i64,
    ) -> Result<i64, rusqlite::Error> {
        self.conn.execute(
            "INSERT OR IGNORE INTO projects (name, folder, instructions, created_ts)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![name, folder, instructions, ts],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update a project's instructions. Returns the number of rows affected (0
    /// when no project carries `project_id` — a thin SQL wrapper, like
    /// `remove_project`; the *edit-hit-a-row* semantic is enforced one layer up
    /// in the worker so a vanished-row edit surfaces as a failure, not a silent
    /// no-op that loses the user's typed instructions).
    pub fn update_project_instructions(
        &self,
        project_id: i64,
        instructions: &str,
    ) -> Result<usize, rusqlite::Error> {
        self.conn.execute(
            "UPDATE projects SET instructions = ?1 WHERE id = ?2",
            rusqlite::params![instructions, project_id],
        )
    }

    /// Delete a project by id.
    pub fn remove_project(&self, project_id: i64) -> Result<(), rusqlite::Error> {
        self.conn
            .execute("DELETE FROM projects WHERE id = ?1", [project_id])?;
        Ok(())
    }

    /// All projects, newest first.
    pub fn list_projects(&self) -> Result<Vec<StoredProject>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, folder, instructions, created_ts
             FROM projects ORDER BY created_ts DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredProject {
                id: row.get(0)?,
                name: row.get(1)?,
                folder: row.get(2)?,
                instructions: row.get(3)?,
                created_ts: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// All messages of a conversation, oldest first.
    pub fn messages(&self, conversation_id: &str) -> Result<Vec<StoredMessage>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, role, text, ts, kind, superseded_by FROM messages
             WHERE conversation_id = ?1 ORDER BY ts ASC, id ASC",
        )?;
        let rows = stmt.query_map([conversation_id], |row| {
            Ok(StoredMessage {
                id: row.get(0)?,
                role: row.get(1)?,
                text: row.get(2)?,
                ts: row.get(3)?,
                kind: row.get(4)?,
                superseded_by: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// Append a `kind = "summary"` message (slice 21 §D4) — the compressed
    /// stand-in for older turns. Returns its row id so the caller can mark the
    /// turns it supersedes. Its own committed transaction, like [`append_message`].
    pub fn append_summary_message(
        &self,
        conversation_id: &str,
        text: &str,
        ts: i64,
    ) -> Result<i64, rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO messages (conversation_id, role, text, ts, kind)
             VALUES (?1, ?2, ?3, ?4, 'summary')",
            rusqlite::params![conversation_id, "assistant", text, ts],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Mark `message_id` as superseded by `summary_id` (slice 21 §D4): it stays
    /// in storage and on screen (dimmed) but is skipped when assembling the
    /// prompt. Idempotent.
    pub fn set_superseded(&self, message_id: i64, summary_id: i64) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE messages SET superseded_by = ?2 WHERE id = ?1",
            rusqlite::params![message_id, summary_id],
        )?;
        Ok(())
    }

    /// Persist a complete summarize action atomically (slice 21 §D4): append the
    /// `kind = "summary"` row, then mark each turn it replaces as superseded by
    /// it. Turns are addressed by their in-memory `ts` (the monotonic counter the
    /// app persists verbatim into the `ts` column), so the caller passes the same
    /// `ts` values [`set_superseded`](crate::conversation) uses in memory — no
    /// row-id bookkeeping crosses the UI boundary. A `ts` with no matching live
    /// row is skipped (the turn stays live), never an error: the summary is still
    /// committed. The summary row never supersedes itself (`id != summary_id`
    /// guards a `ts` collision). One committed transaction. Returns the summary
    /// row id (the `superseded_by` value, remapped to its in-memory `ts` on
    /// restore).
    pub fn summarize_earlier(
        &mut self,
        conversation_id: &str,
        summary_text: &str,
        summary_ts: i64,
        superseded_ts: &[i64],
    ) -> Result<i64, rusqlite::Error> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO messages (conversation_id, role, text, ts, kind)
             VALUES (?1, 'assistant', ?2, ?3, 'summary')",
            rusqlite::params![conversation_id, summary_text, summary_ts],
        )?;
        let summary_id = tx.last_insert_rowid();
        for &ts in superseded_ts {
            tx.execute(
                "UPDATE messages SET superseded_by = ?3
                 WHERE conversation_id = ?1 AND ts = ?2 AND id != ?3",
                rusqlite::params![conversation_id, ts, summary_id],
            )?;
        }
        tx.commit()?;
        Ok(summary_id)
    }

    /// Clear the `superseded_by` mark from every row a given summary replaced
    /// (slice 21 §D4 "Discard summary" — fully reversible). Returns how many rows
    /// were un-superseded.
    pub fn clear_superseded(&self, summary_id: i64) -> Result<usize, rusqlite::Error> {
        self.conn.execute(
            "UPDATE messages SET superseded_by = NULL WHERE superseded_by = ?1",
            rusqlite::params![summary_id],
        )
    }

    /// The durable inverse of [`summarize_earlier`](Self::summarize_earlier)
    /// (slice 21 §D4 "Discard summary"): in ONE transaction un-supersede every
    /// row the summary at `summary_ts` replaced (restoring them to live), delete
    /// the summary row, and close the `ts` gap the delete leaves. The summary is
    /// addressed by its in-memory `ts` — no row id crosses the UI boundary,
    /// mirroring `summarize_earlier`. A `ts` that names no `summary` row restores
    /// and deletes nothing (the subquery yields no id, so the update matches no
    /// rows and the delete is a no-op). Returns how many turns were restored.
    ///
    /// Closing the gap (`ts -= 1` for every later row) is what keeps this safe
    /// across a restart: a restore re-derives `ts` positionally (`0..n`), so a
    /// hole in the stored sequence would make the reloaded in-memory `ts` diverge
    /// from the column every future supersede write addresses. `superseded_by` is
    /// keyed by row `id`, never `ts`, so the shift never disturbs a link — and
    /// `idx_messages_conversation` is non-unique, so the bulk decrement cannot
    /// transiently collide.
    pub fn discard_summary(
        &mut self,
        conversation_id: &str,
        summary_ts: i64,
    ) -> Result<usize, rusqlite::Error> {
        let tx = self.conn.transaction()?;
        let restored = tx.execute(
            "UPDATE messages SET superseded_by = NULL
             WHERE superseded_by = (
                 SELECT id FROM messages
                 WHERE conversation_id = ?1 AND ts = ?2 AND kind = 'summary'
             )",
            rusqlite::params![conversation_id, summary_ts],
        )?;
        let deleted = tx.execute(
            "DELETE FROM messages
             WHERE conversation_id = ?1 AND ts = ?2 AND kind = 'summary'",
            rusqlite::params![conversation_id, summary_ts],
        )?;
        if deleted > 0 {
            tx.execute(
                "UPDATE messages SET ts = ts - 1
                 WHERE conversation_id = ?1 AND ts > ?2",
                rusqlite::params![conversation_id, summary_ts],
            )?;
        }
        tx.commit()?;
        Ok(restored)
    }

    // ---- usage accounting (slice 8 §1) -------------------------------------

    /// Record one turn's token usage. Its own committed transaction, like every
    /// other write. Fails (FK violation) for an unknown conversation id.
    pub fn record_usage(
        &self,
        conversation_id: &str,
        turn_ts: i64,
        provider: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<(), rusqlite::Error> {
        self.record_usage_with_cache(
            conversation_id,
            turn_ts,
            provider,
            model,
            input_tokens,
            output_tokens,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_usage_with_cache(
        &self,
        conversation_id: &str,
        turn_ts: i64,
        provider: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cached_input_tokens: Option<u32>,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO usage_events (conversation_id, turn_ts, provider, model, input_tokens, output_tokens, cached_input_tokens)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![conversation_id, turn_ts, provider, model, input_tokens, output_tokens, cached_input_tokens.map(|v|v.min(input_tokens))],
        )?;
        Ok(())
    }

    /// Total `(input, output)` tokens across every event in the UTC calendar
    /// day `date` — the canonical public API per the slice-8 spec (review SF5:
    /// the calendar-day contract belongs in the signature). Sums saturate at
    /// `u32::MAX`.
    pub fn daily_usage(&self, date: NaiveDate) -> Result<(u32, u32), rusqlite::Error> {
        self.daily_usage_at(day_start_ts(date))
    }

    /// The unix-floor core of [`daily_usage`](Self::daily_usage): any timestamp
    /// within the day works — it is floored to the day start.
    fn daily_usage_at(&self, unix_ts: i64) -> Result<(u32, u32), rusqlite::Error> {
        let start = utc_day_start(unix_ts);
        let (input, output): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0)
             FROM usage_events WHERE turn_ts >= ?1 AND turn_ts < ?2",
            rusqlite::params![start, start + DAY_SECONDS],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let clamp = |v: i64| u32::try_from(v).unwrap_or(u32::MAX);
        Ok((clamp(input), clamp(output)))
    }

    /// The same UTC day rolled up per provider·model — what the cost ticker
    /// needs to price a day in which the user cycled models (slice 8 §4).
    pub fn daily_usage_by_model(&self, unix_ts: i64) -> Result<Vec<UsageByModel>, rusqlite::Error> {
        let start = utc_day_start(unix_ts);
        let mut stmt = self.conn.prepare(
            "SELECT provider, model, COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0)
             FROM usage_events WHERE turn_ts >= ?1 AND turn_ts < ?2
             GROUP BY provider, model ORDER BY provider, model",
        )?;
        let clamp = |v: i64| u32::try_from(v).unwrap_or(u32::MAX);
        let rows = stmt.query_map(rusqlite::params![start, start + DAY_SECONDS], |row| {
            Ok(UsageByModel {
                provider: row.get(0)?,
                model: row.get(1)?,
                input_tokens: clamp(row.get::<_, i64>(2)?),
                output_tokens: clamp(row.get::<_, i64>(3)?),
            })
        })?;
        rows.collect()
    }

    // ---- full-text search (slice 14) ---------------------------------------

    /// Full-text search over message bodies: one row per matching conversation
    /// (its best-ranked snippet). Ordering (slice 16, slice-14 review SF1+SF2):
    /// conversations whose TITLE contains the query first, then by best FTS5
    /// `rank` (bm25 — lower is more relevant), then by `updated_ts` DESC as the
    /// tie-break. An empty or whitespace-only query returns no results without
    /// touching the database. The query is treated as one quoted prefix phrase
    /// (`"…"*`), so partial words match and FTS5 operator characters in user
    /// input cannot break the MATCH syntax.
    pub fn search_messages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, rusqlite::Error> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        // Escape embedded double quotes, quote the whole thing as a phrase,
        // and append `*` for prefix matching ("conv" finds "conversation").
        let match_expr = format!("\"{}\"*", query.replace('"', "\"\""));
        // The CTE computes a snippet per matching MESSAGE — FTS aux functions
        // only work in that per-row context, and MATERIALIZED stops the
        // planner flattening it back into the aggregate below. The outer
        // GROUP BY collapses to one row per conversation; `min(r)` makes
        // SQLite's documented bare-column rule pick that best-ranked row's
        // snippet.
        let mut stmt = self.conn.prepare(
            "WITH hits AS MATERIALIZED (
                 SELECT messages_fts.conversation_id AS conversation_id,
                        snippet(messages_fts, 1, '**', '**', '…', 10) AS snip,
                        c.updated_ts AS updated_ts,
                        c.title AS title,
                        rank AS r
                 FROM messages_fts
                 JOIN conversations c ON c.id = messages_fts.conversation_id
                 WHERE messages_fts MATCH ?1
             )
             SELECT conversation_id, snip, min(r) AS best_rank FROM hits
             GROUP BY conversation_id
             ORDER BY CASE WHEN title LIKE '%' || ?3 || '%' THEN 0 ELSE 1 END,
                      best_rank,
                      max(updated_ts) DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![match_expr, limit as i64, query], |row| {
            Ok(SearchResult {
                conversation_id: row.get(0)?,
                snippet: row.get(1)?,
            })
        })?;
        rows.collect()
    }

    // ---- conversation export (slice 17) ------------------------------------

    /// Pull one conversation out of the database for export: its title (or
    /// `"Untitled"` when blank), creation timestamp, and every message in
    /// chronological order ([`messages`](Self::messages)' order). Errors with
    /// `QueryReturnedNoRows` for an unknown id.
    pub fn export_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<ExportedConversation, rusqlite::Error> {
        let (title, created_at): (String, i64) = self.conn.query_row(
            "SELECT title, created_ts FROM conversations WHERE id = ?1",
            [conversation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let title = if title.trim().is_empty() {
            "Untitled".to_string()
        } else {
            title
        };
        let messages = self.messages(conversation_id)?;
        Ok(ExportedConversation {
            title,
            created_at,
            messages,
        })
    }

    // ---- prompt templates (slice 18) ----------------------------------------

    /// Every saved template, sorted by name — the order the palette lists them.
    pub fn list_templates(&self) -> Result<Vec<PromptTemplate>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, content, created_at, updated_at
             FROM prompt_templates ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PromptTemplate {
                id: row.get(0)?,
                name: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// Insert a template, or update the content of the one already saved under
    /// `name`. Returns the row id. Deliberately `ON CONFLICT DO UPDATE`, not
    /// `INSERT OR REPLACE`: REPLACE deletes the conflicting row first, which
    /// would fire the binding column's `ON DELETE SET NULL` and silently unbind
    /// every conversation using the template being edited. The update path
    /// keeps the id (and `created_at`) stable and stamps `updated_at` = now.
    pub fn upsert_template(&self, name: &str, content: &str) -> Result<i64, rusqlite::Error> {
        self.conn.query_row(
            "INSERT INTO prompt_templates (name, content) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET
                 content = excluded.content,
                 updated_at = strftime('%s','now')
             RETURNING id",
            rusqlite::params![name, content],
            |r| r.get(0),
        )
    }

    /// Delete a template. Conversations bound to it are unbound (`template_id`
    /// goes NULL via the FK action), never deleted. Unknown id is a no-op.
    pub fn delete_template(&self, id: i64) -> Result<(), rusqlite::Error> {
        self.conn
            .execute("DELETE FROM prompt_templates WHERE id = ?1", [id])?;
        Ok(())
    }

    /// One template by id, or `None`.
    pub fn get_template(&self, id: i64) -> Result<Option<PromptTemplate>, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT id, name, content, created_at, updated_at
                 FROM prompt_templates WHERE id = ?1",
                [id],
                |row| {
                    Ok(PromptTemplate {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        content: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    /// Bind (`Some(id)`) or clear (`None`) a conversation's template. The next
    /// send injects the bound template as the leading system message; nothing
    /// in the messages table changes.
    pub fn set_conversation_template(
        &self,
        conversation_id: &str,
        template_id: Option<i64>,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE conversations SET template_id = ?2 WHERE id = ?1",
            rusqlite::params![conversation_id, template_id],
        )?;
        Ok(())
    }

    /// The full template row bound to `conversation_id`, or `None` (no binding,
    /// or the conversation does not exist).
    pub fn get_conversation_template(
        &self,
        conversation_id: &str,
    ) -> Result<Option<PromptTemplate>, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT t.id, t.name, t.content, t.created_at, t.updated_at
                 FROM conversations c
                 JOIN prompt_templates t ON t.id = c.template_id
                 WHERE c.id = ?1",
                [conversation_id],
                |row| {
                    Ok(PromptTemplate {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        content: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
    }

    // ---- file attachments (slice 19) ----------------------------------------

    /// Store one attachment blob, returning its id. `size_bytes` is derived
    /// from `content` here — the caller never supplies a size that could
    /// disagree with the blob.
    pub fn store_attachment(
        &self,
        filename: &str,
        mime_type: &str,
        content: &[u8],
    ) -> Result<i64, rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO attachments (filename, mime_type, size_bytes, content)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![filename, mime_type, content.len() as i64, content],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Stage an attachment on a conversation (it rides along with the next
    /// send). Fails (FK violation) for an unknown conversation or attachment.
    pub fn stage_attachment(
        &self,
        conversation_id: &str,
        attachment_id: i64,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO staged_attachments (conversation_id, attachment_id) VALUES (?1, ?2)",
            rusqlite::params![conversation_id, attachment_id],
        )?;
        Ok(())
    }

    /// The attachments staged on a conversation, in staging order.
    pub fn get_staged_attachments(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<Attachment>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.filename, a.mime_type, a.size_bytes, a.content, a.created_at
             FROM staged_attachments sa
             JOIN attachments a ON a.id = sa.attachment_id
             WHERE sa.conversation_id = ?1
             ORDER BY sa.created_at ASC, sa.id ASC",
        )?;
        let rows = stmt.query_map([conversation_id], |row| {
            Ok(Attachment {
                id: row.get(0)?,
                filename: row.get(1)?,
                mime_type: row.get(2)?,
                size_bytes: row.get(3)?,
                content: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// Drop every staging row for a conversation (after its message was
    /// sent). The attachment blobs stay — they are part of history.
    pub fn clear_staged_attachments(&self, conversation_id: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "DELETE FROM staged_attachments WHERE conversation_id = ?1",
            [conversation_id],
        )?;
        Ok(())
    }

    /// Remove ONE staged attachment (the tray chip's ✕). The blob stays.
    pub fn unstage_attachment(
        &self,
        conversation_id: &str,
        attachment_id: i64,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "DELETE FROM staged_attachments WHERE conversation_id = ?1 AND attachment_id = ?2",
            rusqlite::params![conversation_id, attachment_id],
        )?;
        Ok(())
    }
}

/// Whether the file holds the pre-slice7 layout: a `conversations` table whose
/// shape is the old one (`started_ts` column, integer ids, no `title`).
fn has_legacy_schema(conn: &Connection) -> Result<bool, rusqlite::Error> {
    let table_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'conversations'",
        [],
        |r| r.get(0),
    )?;
    if table_exists == 0 {
        return Ok(false);
    }
    let started_ts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('conversations') WHERE name = 'started_ts'",
        [],
        |r| r.get(0),
    )?;
    Ok(started_ts > 0)
}

/// The slice-7 schema, created fresh (new database) or as the migration target.
const SCHEMA_V2: &str = "
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
";

/// The slice-8 addition: per-turn token accounting (spec §1). `turn_ts` is unix
/// seconds; the index serves the daily-rollup queries.
const USAGE_EVENTS_DDL: &str = "
    CREATE TABLE usage_events (
        id              INTEGER PRIMARY KEY,
        conversation_id TEXT    NOT NULL REFERENCES conversations(id),
        turn_ts         INTEGER NOT NULL,
        provider        TEXT    NOT NULL,
        model           TEXT    NOT NULL,
        input_tokens    INTEGER NOT NULL DEFAULT 0,
        output_tokens   INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_usage_events_ts ON usage_events (turn_ts);
";

/// The slice-14 addition (spec §"Schema change"): an external-content FTS5
/// table over `messages(conversation_id, text)`, plus the three sync triggers.
/// `IF NOT EXISTS` keeps the batch idempotent; the one-time backfill for
/// populated databases lives in [`migrate_v3_to_v4`], inside the same
/// transaction that creates the table.
const MESSAGES_FTS_DDL: &str = "
    CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts
    USING fts5(conversation_id UNINDEXED, text, content=messages, content_rowid=id);
    CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
      INSERT INTO messages_fts(rowid, conversation_id, text)
        VALUES (new.id, new.conversation_id, new.text);
    END;
    CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
      INSERT INTO messages_fts(messages_fts, rowid, conversation_id, text)
        VALUES ('delete', old.id, old.conversation_id, old.text);
    END;
    CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
      INSERT INTO messages_fts(messages_fts, rowid, conversation_id, text)
        VALUES ('delete', old.id, old.conversation_id, old.text);
      INSERT INTO messages_fts(rowid, conversation_id, text)
        VALUES (new.id, new.conversation_id, new.text);
    END;
";

/// The slice-18 addition (spec §1 + §4): the named system-prompt store, plus
/// the per-conversation binding column. `ON DELETE SET NULL` makes deleting a
/// template unbind — never delete — the conversations that used it.
const PROMPT_TEMPLATES_DDL: &str = "
    CREATE TABLE IF NOT EXISTS prompt_templates (
        id      INTEGER PRIMARY KEY AUTOINCREMENT,
        name    TEXT NOT NULL UNIQUE,
        content TEXT NOT NULL,
        created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
        updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
    );
    ALTER TABLE conversations ADD COLUMN template_id INTEGER
        REFERENCES prompt_templates(id) ON DELETE SET NULL;
";

/// The slice-19 addition (spec §1): attachment blobs plus the staging rows
/// linking them to a conversation before send. Staged rows cascade away with
/// their conversation or attachment; the blobs themselves are kept for
/// history when a send clears the staging (only `staged_attachments` rows are
/// deleted).
const ATTACHMENTS_DDL: &str = "
    CREATE TABLE IF NOT EXISTS attachments (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        filename    TEXT NOT NULL,
        mime_type   TEXT NOT NULL,
        size_bytes  INTEGER NOT NULL,
        content     BLOB NOT NULL,
        created_at  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
    );
    CREATE TABLE IF NOT EXISTS staged_attachments (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        attachment_id   INTEGER NOT NULL REFERENCES attachments(id) ON DELETE CASCADE,
        created_at      INTEGER NOT NULL DEFAULT (strftime('%s','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_staged_attachments_conv
        ON staged_attachments (conversation_id, created_at, id);
";

/// Create the schema through v5 (everything except the slice-19 attachment
/// tables) in a fresh file; [`migrate_v5_to_v6`] then lands it on the current
/// version.
fn create_schema_v5(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         {SCHEMA_V2}
         {USAGE_EVENTS_DDL}
         {MESSAGES_FTS_DDL}
         {PROMPT_TEMPLATES_DDL}
         PRAGMA user_version = {SCHEMA_V5_VERSION};
         COMMIT;"
    ))
}

/// The slice-19 attachment tables, stamping schema v6. This is the v5→v6
/// migration body; later migrations (v6→v7…v8→v9) advance the file the rest of
/// the way.
fn create_schema_v6(conn: &Connection) -> Result<(), rusqlite::Error> {
    // Stamp the version this step actually reaches (v6), NOT SCHEMA_VERSION —
    // otherwise a crash after this commit but before the later migrations would
    // leave a DB marked current while missing their columns, and `Store::open`
    // would skip the rest (Codex review, same class as the v7->v8 fix).
    conn.execute_batch(&format!(
        "BEGIN;
         {ATTACHMENTS_DDL}
         PRAGMA user_version = {SCHEMA_V6_VERSION};
         COMMIT;"
    ))
}

/// v2 → v3 is purely additive: one new table, nothing touched. Its own
/// transaction so a crash leaves either a clean v2 or a clean v3 file.
fn migrate_v2_to_v3(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         {USAGE_EVENTS_DDL}
         PRAGMA user_version = {SCHEMA_V3_VERSION};
         COMMIT;"
    ))
}

/// v3 → v4 (slice 14): create the FTS table + triggers and backfill every
/// existing message in ONE transaction — the backfill therefore runs exactly
/// once (only this migration creates the table, and only an un-stamped file
/// reaches it). A crash mid-migration rolls back to a clean v3.
fn migrate_v3_to_v4(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         {MESSAGES_FTS_DDL}
         INSERT INTO messages_fts(rowid, conversation_id, text)
           SELECT id, conversation_id, text FROM messages;
         PRAGMA user_version = {SCHEMA_V4_VERSION};
         COMMIT;"
    ))
}

/// v4 → v5 (slice 18) is purely additive: the template table and the nullable
/// binding column. Its own transaction, so a crash leaves either a clean v4
/// or a clean v5 file.
fn migrate_v4_to_v5(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         {PROMPT_TEMPLATES_DDL}
         PRAGMA user_version = {SCHEMA_V5_VERSION};
         COMMIT;"
    ))
}

/// v5 → v6 (slice 19): purely additive — the two attachment tables and their
/// index. Its own transaction, like every other migration step.
fn migrate_v5_to_v6(conn: &Connection) -> Result<(), rusqlite::Error> {
    create_schema_v6(conn)
}

fn migrate_v6_to_v7(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         ALTER TABLE conversations ADD COLUMN cwd TEXT;
         ALTER TABLE conversations ADD COLUMN profile TEXT;
         PRAGMA user_version = {SCHEMA_V7_VERSION};
         COMMIT;"
    ))?;
    Ok(())
}

/// Add the conversation `kind` column (Chat vs Work). Additive: existing rows get
/// NULL, which the app's `ConvKind::from_token` reads as Chat — so every
/// pre-existing conversation stays a chat thread.
fn migrate_v7_to_v8(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         ALTER TABLE conversations ADD COLUMN kind TEXT;
         PRAGMA user_version = {SCHEMA_V8_VERSION};
         COMMIT;"
    ))?;
    Ok(())
}

/// Add per-message `kind` and `superseded_by` (slice 21 §D4: explicit
/// summarize). `kind` is `'normal'` for ordinary turns or `'summary'` for a
/// user-made summary message; `superseded_by` points at the summary message id
/// that replaced this row (NULL = live). Additive and back-compatible: existing
/// rows default to `'normal'` / NULL, so every prior message stays a normal,
/// live turn. Stamps `SCHEMA_V9_VERSION` (the v9→v10 `projects` migration
/// follows it and stamps `SCHEMA_VERSION`).
fn migrate_v8_to_v9(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         ALTER TABLE messages ADD COLUMN kind TEXT NOT NULL DEFAULT 'normal';
         ALTER TABLE messages ADD COLUMN superseded_by INTEGER;
         PRAGMA user_version = {SCHEMA_V9_VERSION};
         COMMIT;"
    ))?;
    Ok(())
}

/// v9→v10: the `projects` table backing Work-mode saved projects (a folder bound
/// as a named project). Purely additive — one `CREATE TABLE`, no existing row
/// touched. The `Store::{add,list,remove}_project` API referenced this table
/// but no migration ever created it, so every project write failed at runtime;
/// this closes that gap. `IF NOT EXISTS` keeps the migration idempotent.
fn migrate_v9_to_v10(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         CREATE TABLE IF NOT EXISTS projects (
             id           INTEGER PRIMARY KEY AUTOINCREMENT,
             name         TEXT NOT NULL,
             folder       TEXT NOT NULL UNIQUE,
             instructions TEXT NOT NULL DEFAULT '',
             created_ts   INTEGER NOT NULL
         );
         PRAGMA user_version = {SCHEMA_V10_VERSION};
         COMMIT;"
    ))?;
    Ok(())
}

/// The slice-22 (offline capture stage 1A) addition: the dormant
/// `capture_outbox` table and its full-binding index, in ONE transaction,
/// stamping schema v11. Purely additive — the table starts EMPTY (no
/// historical backfill, no startup enqueue), and nothing about the existing
/// `messages`/`conversations` rows is touched.
///
/// This step stamps the EXPLICIT `SCHEMA_V11_VERSION`, not `SCHEMA_VERSION`,
/// for exactly the reason the v9→v10 and v5→v6 steps do: the v11→v12
/// migration now follows it, and a crash after this commit but before the
/// 11→12 step must leave a file marked v11 so `Store::open` still runs the
/// rest of the chain (otherwise the file would be marked current while
/// missing the v12 columns/tables — same class as the v7→v8 stamp fix).
///
/// Deliberately NO `message_id` foreign key: the outbox is remote-evidence
/// bookkeeping that must survive deletion of the local message it was captured
/// alongside (stage 1B will carry deletion/truncate bookkeeping on top of it).
fn migrate_v10_to_v11(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         {CAPTURE_OUTBOX_DDL}
         PRAGMA user_version = {SCHEMA_V11_VERSION};
         COMMIT;"
    ))?;
    Ok(())
}

/// The slice-22 (offline capture stage 1B-a) addition: exposure and
/// deletion-intent bookkeeping for the capture outbox, in ONE transaction,
/// stamping schema v12 (`SCHEMA_V12_VERSION` — its own constant, since a later
/// step now follows it). Purely additive:
/// * `capture_outbox` gains `exposure TEXT NOT NULL DEFAULT 'never_sent'
///   CHECK (exposure IN ('never_sent', 'possibly_sent'))` — every existing
///   queued row migrates to `never_sent` via the column default;
/// * the new `capture_deletion_intents` table + full-binding index starts
///   EMPTY (zero intent rows; there is no application API to create or clear
///   intents in this unit — the independently reviewed 1B-b unit performs the
///   local deletion/truncate bookkeeping that will populate it, but does NOT
///   clear intents; only a future, independently verified remote protocol may
///   do that).
///
/// Nothing about the existing `messages`/`conversations` rows or the
/// outbox's existing rows is touched beyond the default-filled column.
///
/// Like its predecessors this step commits as its own transaction and stamps
/// the exact version it reaches (its own `SCHEMA_V12_VERSION`, not the current
/// `SCHEMA_VERSION` — a later step now follows it), so a crash after the
/// commit but before the next step leaves the file one version behind, and
/// the next open runs exactly this step (it is idempotent: `IF NOT EXISTS`).
fn migrate_v11_to_v12(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         ALTER TABLE capture_outbox
             ADD COLUMN exposure TEXT NOT NULL DEFAULT 'never_sent'
             CHECK (exposure IN ('never_sent', 'possibly_sent'));
         {CAPTURE_DELETION_INTENTS_DDL}
         PRAGMA user_version = {SCHEMA_V12_VERSION};
         COMMIT;"
    ))?;
    Ok(())
}

/// The daily-driver P2d addition: the `build_jobs` table (durable local
/// Build job correlation, `crates/awili-persist/src/build_jobs.rs`), in ONE
/// transaction, stamping schema v13. Purely additive: the table starts EMPTY;
/// no existing row is touched. Like its predecessors this step commits as its
/// own transaction and stamps the exact version it reaches (its own
/// `SCHEMA_V13_VERSION`, not the current `SCHEMA_VERSION` — a later step now
/// follows it).
fn migrate_v12_to_v13(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         {BUILD_JOBS_DDL}
         PRAGMA user_version = {SCHEMA_V13_VERSION};
         COMMIT;"
    ))?;
    Ok(())
}

/// P10/F4 widening of the `build_jobs` table: the `state` CHECK gains the
/// app-local `submitting`/`unknown` tokens and the row gains the `failure`
/// text. SQLite cannot alter a CHECK in place, so this rebuilds the table in
/// ONE transaction: create the v14 table, copy every row (v13 never persisted
/// a `failure`, so it copies as NULL), drop the old table and its indexes,
/// rename, and recreate the indexes. A crash anywhere rolls back to the intact
/// v13 table; no row is lost.
fn migrate_v13_to_v14(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         CREATE TABLE build_jobs_v14 (
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
         INSERT INTO build_jobs_v14 (turn_id, request_id, target_id, revision, state, result_sha, failure, updated_at)
             SELECT turn_id, request_id, target_id, revision, state, result_sha, NULL, updated_at
             FROM build_jobs;
         DROP TABLE build_jobs;
         ALTER TABLE build_jobs_v14 RENAME TO build_jobs;
         CREATE UNIQUE INDEX idx_build_jobs_request_id ON build_jobs (request_id);
         CREATE INDEX idx_build_jobs_state ON build_jobs (state);
         PRAGMA user_version = {SCHEMA_V14_VERSION};
         COMMIT;"
    ))?;
    Ok(())
}

/// Migrate the pre-slice7 layout to schema v2 in ONE transaction:
/// 1. rename the legacy tables out of the way,
/// 2. create the v2 schema under the final names,
/// 3. copy every legacy conversation (as `"Conversation N"`, fresh UUID) and
///    every legacy message (re-pointed at the UUID),
/// 4. drop the legacy tables and stamp `user_version`.
///
/// A crash anywhere in between rolls the whole transaction back to the intact
/// legacy layout. No row is ever lost: messages copy 1:1 (ids, roles, text,
/// timestamps), and `app_state` is untouched.
fn migrate_v0_to_v2(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(&format!(
        "BEGIN;
         ALTER TABLE messages RENAME TO messages_legacy;
         ALTER TABLE conversations RENAME TO conversations_legacy;
         {SCHEMA_V2}"
    ))?;

    // One UUID per legacy conversation, titled "Conversation N" in original id
    // order. updated_ts starts at started_ts — the legacy schema kept no
    // per-conversation activity clock (message `ts` is a logical counter).
    {
        let mut read =
            conn.prepare("SELECT id, started_ts FROM conversations_legacy ORDER BY id ASC")?;
        let legacy: Vec<(i64, i64)> = read
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut insert_conv = conn.prepare(
            "INSERT INTO conversations (id, title, created_ts, updated_ts, archived, title_is_custom)
             VALUES (?1, ?2, ?3, ?3, 0, 0)",
        )?;
        let mut copy_msgs = conn.prepare(
            "INSERT INTO messages (id, conversation_id, role, text, ts)
             SELECT id, ?2, role, text, ts FROM messages_legacy WHERE conversation_id = ?1",
        )?;
        for (n, (old_id, started_ts)) in legacy.iter().enumerate() {
            let title = format!("Conversation {}", n + 1);
            // A v4 UUID collision is astronomically unlikely, but the PRIMARY
            // KEY would reject the row and lose the conversation; retry with a
            // fresh UUID. Bounded so a constraint failure with another cause
            // (or a broken RNG) still surfaces instead of looping forever.
            let mut attempts = 0;
            let uuid = loop {
                let uuid = uuid::Uuid::new_v4().to_string();
                match insert_conv.execute(rusqlite::params![uuid, title, started_ts]) {
                    Ok(_) => break uuid,
                    Err(rusqlite::Error::SqliteFailure(e, _))
                        if e.code == rusqlite::ErrorCode::ConstraintViolation && attempts < 16 =>
                    {
                        attempts += 1;
                    }
                    Err(e) => return Err(e),
                }
            };
            copy_msgs.execute(rusqlite::params![old_id, uuid])?;
        }
    }

    // Belt and braces INSIDE the transaction: if any message row failed to copy
    // (e.g. an orphan pointing at a conversation id that never existed), abort
    // and roll back rather than silently dropping it.
    let legacy_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM messages_legacy", [], |r| r.get(0))?;
    let new_count: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
    if legacy_count != new_count {
        conn.execute_batch("ROLLBACK;")?;
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!(
                "slice-7 migration aborted: {legacy_count} legacy messages but {new_count} copied"
            )),
        ));
    }

    conn.execute_batch(&format!(
        "DROP TABLE messages_legacy;
         DROP TABLE conversations_legacy;
         PRAGMA user_version = {SCHEMA_V2_VERSION};
         COMMIT;"
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn workshop_snapshot_recovery_is_atomic_and_compare_guarded() {
        let path = temp_db();
        let store = super::Store::open(&path).unwrap();
        store
            .set_app_state("workshop_tabs", "original-bad-bytes")
            .unwrap();
        assert!(!store
            .replace_app_state_with_backup("workshop_tabs", "stale", "replacement", "backup-stale")
            .unwrap());
        assert!(store.get_app_state("backup-stale").unwrap().is_none());
        store.conn.execute_batch("CREATE TRIGGER reject_snapshot BEFORE UPDATE ON app_state WHEN NEW.key = 'workshop_tabs' BEGIN SELECT RAISE(ABORT, 'fixture refuses replacement'); END;").unwrap();
        assert!(store
            .replace_app_state_with_backup(
                "workshop_tabs",
                "original-bad-bytes",
                "replacement",
                "backup-failed"
            )
            .is_err());
        assert_eq!(
            store.get_app_state("workshop_tabs").unwrap().as_deref(),
            Some("original-bad-bytes")
        );
        assert!(
            store.get_app_state("backup-failed").unwrap().is_none(),
            "backup and replacement roll back together"
        );
        store
            .conn
            .execute_batch("DROP TRIGGER reject_snapshot")
            .unwrap();
        assert!(store
            .replace_app_state_with_backup(
                "workshop_tabs",
                "original-bad-bytes",
                "replacement",
                "backup-ok"
            )
            .unwrap());
        drop(store);
        let reopened = super::Store::open(&path).unwrap();
        assert_eq!(
            reopened.get_app_state("backup-ok").unwrap().as_deref(),
            Some("original-bad-bytes")
        );
        assert_eq!(
            reopened.get_app_state("workshop_tabs").unwrap().as_deref(),
            Some("replacement")
        );
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn workshop_project_import_preserves_instructions_and_deletion_after_reopen() {
        let path = temp_db();
        let legacy = vec![
            ("old name".into(), "/work/existing".into()),
            ("Code only".into(), "/work/code".into()),
        ];
        {
            let store = Store::open(&path).unwrap();
            store
                .add_project("Work name", "/work/existing", "Keep these rules", 10)
                .unwrap();
            store
                .set_app_state("code_projects", "legacy bytes retained")
                .unwrap();
            store
                .import_legacy_projects_once(&legacy, Some("/work/code"), 20)
                .unwrap();
            let projects = store.list_projects().unwrap();
            assert_eq!(projects.len(), 2);
            let existing = projects
                .iter()
                .find(|p| p.folder == "/work/existing")
                .unwrap();
            assert_eq!(existing.name, "Work name");
            assert_eq!(existing.instructions, "Keep these rules");
            assert_eq!(existing.created_ts, 10);
            assert_eq!(
                store.get_app_state("code_projects").unwrap().as_deref(),
                Some("legacy bytes retained")
            );
            assert_eq!(
                store
                    .get_app_state("workshop_active_project_folder")
                    .unwrap()
                    .as_deref(),
                Some("/work/code")
            );
            store
                .remove_project(
                    projects
                        .iter()
                        .find(|p| p.folder == "/work/code")
                        .unwrap()
                        .id,
                )
                .unwrap();
        }
        let reopened = Store::open(&path).unwrap();
        reopened
            .import_legacy_projects_once(&legacy, Some("/work/existing"), 30)
            .unwrap();
        assert_eq!(
            reopened.list_projects().unwrap().len(),
            1,
            "deleted Code project must not resurrect"
        );
        assert_eq!(
            reopened
                .get_app_state("workshop_active_project_folder")
                .unwrap()
                .as_deref(),
            Some("/work/code")
        );
        drop(reopened);
        cleanup(&path);
    }

    #[test]
    fn workshop_project_import_failure_rolls_back_rows_marker_and_selection() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_project BEFORE INSERT ON projects
            WHEN NEW.folder = '/work/rejected' BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();
        let legacy = vec![
            ("First".into(), "/work/first".into()),
            ("Rejected".into(), "/work/rejected".into()),
        ];
        assert!(store
            .import_legacy_projects_once(&legacy, Some("/work/first"), 20)
            .is_err());
        assert!(store.list_projects().unwrap().is_empty());
        assert_eq!(
            store.get_app_state("workshop_projects_imported").unwrap(),
            None
        );
        assert_eq!(
            store
                .get_app_state("workshop_active_project_folder")
                .unwrap(),
            None
        );
        store
            .conn
            .execute_batch("DROP TRIGGER reject_project;")
            .unwrap();
        store
            .import_legacy_projects_once(&legacy, Some("/work/first"), 20)
            .unwrap();
        assert_eq!(store.list_projects().unwrap().len(), 2);
        drop(store);
        cleanup(&path);
    }

    /// A unique temp path per test (no time source; a process-id + counter is
    /// enough to avoid collisions within a run).
    fn temp_db() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "awili-persist-test-{}-{}.db",
            std::process::id(),
            n
        ));
        // Remove any leftover from a previous process with a reused PID so
        // `create_schema_v5` (no IF NOT EXISTS) never hits "already exists".
        let _ = std::fs::remove_file(&path);
        path
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    fn new_conv(store: &Store, title: &str, ts: i64) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        store.create_conversation(&id, title, ts).unwrap();
        id
    }

    #[test]
    fn roundtrip_messages() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "New conversation", 1000);
        store.append_message(&convo, "user", "hello", 0).unwrap();
        store
            .append_message(&convo, "assistant", "hi there", 1)
            .unwrap();

        let msgs = store.messages(&convo).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].text, "hello");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].text, "hi there");
        cleanup(&path);
    }

    #[test]
    fn conversation_metadata_roundtrips() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let a = new_conv(&store, "New conversation", 10);
        let b = new_conv(&store, "New conversation", 20);

        store.rename_conversation(&a, "Garden plan", true).unwrap();
        store.touch_conversation(&a, 99).unwrap();
        store.set_archived(&b, true).unwrap();

        let convs = store.conversations().unwrap();
        assert_eq!(convs.len(), 2);
        assert_eq!(convs[0].id, a);
        assert_eq!(convs[0].title, "Garden plan");
        assert!(convs[0].title_is_custom);
        assert_eq!(convs[0].created_ts, 10);
        assert_eq!(convs[0].updated_ts, 99);
        assert!(!convs[0].archived);
        assert!(convs[1].archived, "soft archive: still present, flagged");
        assert!(!convs[1].title_is_custom);
        cleanup(&path);
    }

    #[test]
    fn archived_conversation_keeps_its_messages() {
        // The soft-archive guarantee at the persistence layer: flipping the flag
        // changes NOTHING about the message rows, both ways.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "t", 0);
        store.append_message(&convo, "user", "precious", 0).unwrap();

        store.set_archived(&convo, true).unwrap();
        assert_eq!(store.messages(&convo).unwrap().len(), 1);
        store.set_archived(&convo, false).unwrap();
        assert_eq!(store.messages(&convo).unwrap()[0].text, "precious");
        cleanup(&path);
    }

    #[test]
    fn fk_integrity_rejects_unknown_conversation() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let err = store.append_message("no-such-conversation", "user", "x", 0);
        assert!(err.is_err(), "FK enforcement must reject an orphan message");
        cleanup(&path);
    }

    #[test]
    fn update_message_replaces_text() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "t", 0);
        let id = store
            .append_message(&convo, "assistant", "partial", 0)
            .unwrap();
        store.update_message(id, "complete reply").unwrap();
        assert_eq!(store.messages(&convo).unwrap()[0].text, "complete reply");
        cleanup(&path);
    }

    #[test]
    fn truncate_messages_keeps_the_restore_order_prefix() {
        // Slice 12: regenerate/edit-resend rewrite history from a point onward.
        // The kept prefix is defined by the SAME order restore reads (ts, id).
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "t", 0);
        store.append_message(&convo, "user", "first", 0).unwrap();
        store
            .append_message(&convo, "assistant", "reply", 1)
            .unwrap();
        store.append_message(&convo, "user", "second", 2).unwrap();
        store
            .append_message(&convo, "assistant", "stale reply", 3)
            .unwrap();

        let deleted = store.truncate_messages(&convo, 3).unwrap();
        assert_eq!(deleted, 1);
        let texts: Vec<_> = store
            .messages(&convo)
            .unwrap()
            .into_iter()
            .map(|m| m.text)
            .collect();
        assert_eq!(texts, ["first", "reply", "second"]);
        cleanup(&path);
    }

    #[test]
    fn truncate_messages_keep_all_is_a_noop() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "t", 0);
        store.append_message(&convo, "user", "only", 0).unwrap();
        assert_eq!(store.truncate_messages(&convo, 1).unwrap(), 0);
        assert_eq!(store.truncate_messages(&convo, 99).unwrap(), 0);
        assert_eq!(store.messages(&convo).unwrap().len(), 1);
        cleanup(&path);
    }

    #[test]
    fn truncate_messages_is_scoped_to_one_conversation() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let a = new_conv(&store, "a", 0);
        let b = new_conv(&store, "b", 0);
        store.append_message(&a, "user", "a0", 0).unwrap();
        store.append_message(&a, "assistant", "a1", 1).unwrap();
        store.append_message(&b, "user", "b0", 0).unwrap();
        store.append_message(&b, "assistant", "b1", 1).unwrap();

        // keep=0 wipes A entirely; B must be untouched.
        assert_eq!(store.truncate_messages(&a, 0).unwrap(), 2);
        assert!(store.messages(&a).unwrap().is_empty());
        assert_eq!(store.messages(&b).unwrap().len(), 2);
        cleanup(&path);
    }

    #[test]
    fn app_state_roundtrips_and_upserts() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get_app_state("selection").unwrap(), None);
        store
            .set_app_state("selection", "anthropic\u{1f}claude-opus-4-8")
            .unwrap();
        assert_eq!(
            store.get_app_state("selection").unwrap().as_deref(),
            Some("anthropic\u{1f}claude-opus-4-8")
        );
        // Upsert replaces, does not duplicate.
        store
            .set_app_state("selection", "ollama\u{1f}llama3.1:8b")
            .unwrap();
        assert_eq!(
            store.get_app_state("selection").unwrap().as_deref(),
            Some("ollama\u{1f}llama3.1:8b")
        );
        cleanup(&path);
    }

    #[test]
    fn fresh_db_is_stamped_with_current_version() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let v: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        cleanup(&path);
    }

    #[test]
    fn reopen_without_clean_close_replays_wal() {
        // NOTE: this is the SAME-process WAL-replay check — `mem::forget` leaks a
        // still-live connection, it does NOT kill the process or release OS locks.
        // The real process-death guarantee is proven by `tests/kill9.rs`, which
        // SIGKILLs a child process and reopens the file from the parent. Here we
        // only assert that committed rows are visible after reopening a file whose
        // connection was never closed or checkpointed.
        let path = temp_db();
        let conv;
        {
            let store = Store::open(&path).unwrap();
            conv = new_conv(&store, "t", 42);
            store
                .append_message(&conv, "user", "before the crash", 0)
                .unwrap();
            store
                .append_message(&conv, "assistant", "completed reply", 1)
                .unwrap();
            // No close, no checkpoint, no Drop — the connection just vanishes.
            std::mem::forget(store);
        }

        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.conversations().unwrap().len(), 1);
        let msgs = reopened.messages(&conv).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text, "before the crash");
        assert_eq!(msgs[1].text, "completed reply");
        cleanup(&path);
    }

    #[test]
    fn crash_loses_only_the_in_flight_reply() {
        // Model the spec's guarantee precisely: the user message is committed on
        // send; the assistant reply is NOT yet written when the crash hits. After
        // recovery the user turn survives and only the in-flight reply is missing.
        let path = temp_db();
        let conv;
        {
            let store = Store::open(&path).unwrap();
            conv = new_conv(&store, "t", 0);
            store
                .append_message(&conv, "user", "ask something", 0)
                .unwrap();
            // ...streaming reply in progress, nothing appended yet... crash:
            std::mem::forget(store);
        }
        let reopened = Store::open(&path).unwrap();
        let msgs = reopened.messages(&conv).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "user");
        cleanup(&path);
    }

    fn date_of(ts: i64) -> NaiveDate {
        chrono::DateTime::from_timestamp(ts, 0)
            .unwrap()
            .date_naive()
    }

    #[test]
    fn record_usage_roundtrips_through_daily_usage() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        store
            .record_usage(&conv, 1_000, "anthropic", "claude-haiku-4-5", 120, 45)
            .unwrap();
        store
            .record_usage(&conv, 2_000, "anthropic", "claude-haiku-4-5", 80, 15)
            .unwrap();
        assert_eq!(store.daily_usage(date_of(1_500)).unwrap(), (200, 60));
        cleanup(&path);
    }

    #[test]
    fn daily_usage_is_bounded_to_one_utc_day() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let day = utc_day_start(1_750_000_000); // an arbitrary real day
        store
            .record_usage(&conv, day - 1, "anthropic", "claude-opus-4-8", 1, 1)
            .unwrap();
        store
            .record_usage(&conv, day, "anthropic", "claude-opus-4-8", 10, 20)
            .unwrap();
        store
            .record_usage(&conv, day + 86_399, "anthropic", "claude-opus-4-8", 30, 40)
            .unwrap();
        store
            .record_usage(
                &conv,
                day + 86_400,
                "anthropic",
                "claude-opus-4-8",
                500,
                500,
            )
            .unwrap();
        // Inclusive of the day start and its last second; exclusive of the
        // neighbours either side.
        assert_eq!(store.daily_usage(date_of(day)).unwrap(), (40, 60));
        cleanup(&path);
    }

    #[test]
    fn daily_usage_of_an_empty_day_is_zero() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.daily_usage(date_of(123_456_789)).unwrap(), (0, 0));
        assert!(store.daily_usage_by_model(123_456_789).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn daily_usage_by_model_groups_per_provider_model() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let day = utc_day_start(1_750_000_000);
        store
            .record_usage(&conv, day + 10, "anthropic", "claude-haiku-4-5", 100, 10)
            .unwrap();
        store
            .record_usage(&conv, day + 20, "anthropic", "claude-haiku-4-5", 100, 10)
            .unwrap();
        store
            .record_usage(&conv, day + 30, "anthropic", "claude-opus-4-8", 50, 5)
            .unwrap();
        store
            .record_usage(&conv, day + 40, "ollama", "llama3.1:8b", 999, 999)
            .unwrap();
        let rows = store.daily_usage_by_model(day).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0],
            UsageByModel {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
                input_tokens: 200,
                output_tokens: 20,
            }
        );
        assert_eq!(rows[1].model, "claude-opus-4-8");
        assert_eq!(rows[2].provider, "ollama");
        cleanup(&path);
    }

    #[test]
    fn record_usage_rejects_unknown_conversation() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        assert!(
            store
                .record_usage("no-such-conv", 0, "anthropic", "m", 1, 1)
                .is_err(),
            "FK enforcement must reject usage for a conversation that does not exist"
        );
        cleanup(&path);
    }

    #[test]
    fn usage_events_survive_reopen() {
        // The same WAL-replay guarantee messages get: a committed usage row is
        // visible after reopening a never-closed file.
        let path = temp_db();
        let conv;
        {
            let store = Store::open(&path).unwrap();
            conv = new_conv(&store, "t", 0);
            store
                .record_usage(&conv, 500, "anthropic", "claude-fable-5", 7, 11)
                .unwrap();
            std::mem::forget(store);
        }
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.daily_usage(date_of(500)).unwrap(), (7, 11));
        cleanup(&path);
    }

    #[test]
    fn utc_day_start_floors_correctly() {
        assert_eq!(utc_day_start(0), 0);
        assert_eq!(utc_day_start(86_399), 0);
        assert_eq!(utc_day_start(86_400), 86_400);
        assert_eq!(utc_day_start(1_750_003_222), 1_749_945_600);
        // Pre-epoch floors DOWN (div_euclid), not toward zero.
        assert_eq!(utc_day_start(-1), -86_400);
    }

    #[test]
    fn day_start_ts_bridges_calendar_days_to_unix_seconds() {
        assert_eq!(
            day_start_ts(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
            0
        );
        // The same instant utc_day_start floors 1_750_003_222 to (see above).
        assert_eq!(
            day_start_ts(NaiveDate::from_ymd_opt(2025, 6, 15).unwrap()),
            1_749_945_600
        );
        // Pre-epoch dates are valid and negative.
        assert_eq!(
            day_start_ts(NaiveDate::from_ymd_opt(1969, 12, 31).unwrap()),
            -86_400
        );
    }

    // ---- full-text search (slice 14) ----------------------------------------

    #[test]
    fn fts_search_finds_message_body() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        store
            .append_message(&conv, "user", "the quick brown fox", 1)
            .unwrap();

        let hits = store.search_messages("brown", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, conv);
        assert!(
            hits[0].snippet.contains("**brown**"),
            "snippet was {:?}",
            hits[0].snippet
        );
        cleanup(&path);
    }

    #[test]
    fn fts_search_is_case_insensitive() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        store
            .append_message(&conv, "user", "Hello World", 1)
            .unwrap();

        let hits = store.search_messages("hello", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, conv);
        cleanup(&path);
    }

    #[test]
    fn fts_search_prefix_match() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        store
            .append_message(&conv, "user", "an important conversation", 1)
            .unwrap();

        let hits = store.search_messages("conv", 10).unwrap();
        assert_eq!(hits.len(), 1, "prefix \"conv\" must find \"conversation\"");
        assert!(hits[0].snippet.contains("**conversation**"));
        cleanup(&path);
    }

    #[test]
    fn fts_search_empty_query_returns_empty() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        store
            .append_message(&conv, "user", "anything at all", 1)
            .unwrap();

        assert!(store.search_messages("", 10).unwrap().is_empty());
        assert!(store.search_messages("   \t ", 10).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn fts_search_respects_limit() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        for i in 0..10 {
            let conv = new_conv(&store, "t", i);
            store
                .append_message(&conv, "user", "needle in a haystack", i)
                .unwrap();
        }
        assert_eq!(store.search_messages("needle", 3).unwrap().len(), 3);
        cleanup(&path);
    }

    #[test]
    fn fts_search_ranks_by_recency() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let older = new_conv(&store, "older", 0);
        let newer = new_conv(&store, "newer", 0);
        store
            .append_message(&older, "user", "shared needle", 1)
            .unwrap();
        store
            .append_message(&newer, "user", "shared needle", 2)
            .unwrap();
        store.touch_conversation(&older, 100).unwrap();
        store.touch_conversation(&newer, 200).unwrap();

        let hits = store.search_messages("needle", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].conversation_id, newer, "updated_ts=200 ranks first");
        assert_eq!(hits[1].conversation_id, older);
        cleanup(&path);
    }

    #[test]
    fn fts_search_orders_by_rank_before_recency() {
        // Slice 16 (slice-14 review SF1): bm25 relevance is now the primary
        // sort. `strong` matches harder (the term twice in a two-word message)
        // but was updated LESS recently — it must still come first.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let weak = new_conv(&store, "weak", 0);
        let strong = new_conv(&store, "strong", 0);
        store
            .append_message(
                &weak,
                "user",
                "a needle buried somewhere in a very long haystack of filler words \
                 that dilute the term frequency of the match considerably",
                1,
            )
            .unwrap();
        store
            .append_message(&strong, "user", "needle needle", 2)
            .unwrap();
        store.touch_conversation(&strong, 100).unwrap();
        store.touch_conversation(&weak, 200).unwrap(); // recency favors `weak`

        let hits = store.search_messages("needle", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].conversation_id, strong,
            "better bm25 outranks recency"
        );
        assert_eq!(hits[1].conversation_id, weak);
        cleanup(&path);
    }

    #[test]
    fn fts_search_title_match_boosts_to_top() {
        // Slice 16 (slice-14 review SF2): the term in the conversation TITLE
        // boosts that conversation above every body-only match, even one with
        // a better bm25 score and a fresher updated_ts.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let body_only = new_conv(&store, "misc notes", 0);
        let titled = new_conv(&store, "Deploy checklist", 0);
        store
            .append_message(&body_only, "user", "deploy deploy deploy", 1)
            .unwrap();
        store
            .append_message(
                &titled,
                "user",
                "we should deploy at some point next week",
                2,
            )
            .unwrap();
        store.touch_conversation(&body_only, 200).unwrap();
        store.touch_conversation(&titled, 100).unwrap();

        let hits = store.search_messages("deploy", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0].conversation_id, titled,
            "title LIKE-match leads regardless of rank or recency"
        );
        assert_eq!(hits[1].conversation_id, body_only);
        cleanup(&path);
    }

    #[test]
    fn fts_update_trigger_reflects_edit() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let id = store
            .append_message(&conv, "assistant", "draft zebra text", 1)
            .unwrap();
        store.update_message(id, "final aardvark text").unwrap();

        assert!(
            store.search_messages("zebra", 10).unwrap().is_empty(),
            "old term gone"
        );
        let hits = store.search_messages("aardvark", 10).unwrap();
        assert_eq!(hits.len(), 1, "new term found");
        assert_eq!(hits[0].conversation_id, conv);
        cleanup(&path);
    }

    // ---- conversation export (slice 17) --------------------------------------

    /// Mid-day UTC instant on 2025-06-15 — the date both the document header
    /// and the filename must carry.
    const EXPORT_TS: i64 = 1_750_000_000;

    #[test]
    fn export_conversation_empty() {
        // No messages: just the title header + date line, no separator blocks.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "Garden plan", EXPORT_TS);

        let exp = store.export_conversation(&conv).unwrap();
        assert_eq!(exp.title, "Garden plan");
        assert_eq!(exp.created_at, EXPORT_TS);
        assert_eq!(
            exp.to_markdown(),
            "# Garden plan\n\n_Exported 2025-06-15_\n"
        );
        cleanup(&path);
    }

    // ---- prompt templates (slice 18) ----------------------------------------

    #[test]
    fn template_upsert_and_list() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.upsert_template("Zen mode", "be terse").unwrap();
        store
            .upsert_template("Foreman assistant", "you are the foreman")
            .unwrap();

        let all = store.list_templates().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].name, "Foreman assistant", "list is sorted by name");
        assert_eq!(all[0].content, "you are the foreman");
        assert_eq!(all[1].name, "Zen mode");
        cleanup(&path);
    }

    #[test]
    fn export_conversation_roundtrip() {
        // Two messages: both texts present, in chronological order.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "Trip notes", EXPORT_TS);
        store
            .append_message(&conv, "user", "where should we go?", 0)
            .unwrap();
        store
            .append_message(&conv, "assistant", "somewhere with good coffee", 1)
            .unwrap();

        let md = store.export_conversation(&conv).unwrap().to_markdown();
        let ask = md.find("where should we go?").expect("user text present");
        let reply = md
            .find("somewhere with good coffee")
            .expect("assistant text present");
        assert!(ask < reply, "messages must export in chronological order");
        assert!(md.starts_with("# Trip notes\n"));
        assert!(md.ends_with("\n---\n"), "message blocks are ----fenced");
        cleanup(&path);
    }

    #[test]
    fn template_upsert_idempotent() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let first = store.upsert_template("Foreman", "v1").unwrap();
        let second = store.upsert_template("Foreman", "v2").unwrap();
        assert_eq!(first, second, "same name keeps the same id");

        let all = store.list_templates().unwrap();
        assert_eq!(all.len(), 1, "upsert updates, never duplicates");
        assert_eq!(all[0].content, "v2");
        cleanup(&path);
    }

    #[test]
    fn export_conversation_role_labels() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", EXPORT_TS);
        store.append_message(&conv, "user", "q", 0).unwrap();
        store.append_message(&conv, "assistant", "a", 1).unwrap();
        store.append_message(&conv, "system", "s", 2).unwrap();
        store.append_message(&conv, "tool output", "t", 3).unwrap();

        let md = store.export_conversation(&conv).unwrap().to_markdown();
        assert!(md.contains("**User**"));
        assert!(md.contains("**Assistant**"));
        assert!(md.contains("**System**"));
        assert!(
            md.contains("**Tool Output**"),
            "unknown roles are Title Cased"
        );
        cleanup(&path);
    }

    #[test]
    fn template_delete() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let id = store.upsert_template("Foreman", "x").unwrap();
        store.upsert_template("Other", "y").unwrap();
        store.delete_template(id).unwrap();

        let all = store.list_templates().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Other");
        assert_eq!(store.get_template(id).unwrap(), None);
        cleanup(&path);
    }

    #[test]
    fn export_conversation_blank_title_is_untitled() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "  ", EXPORT_TS);
        let exp = store.export_conversation(&conv).unwrap();
        assert_eq!(exp.title, "Untitled");
        assert_eq!(exp.filename(), "untitled-2025-06-15.md");
        cleanup(&path);
    }

    #[test]
    fn template_get_none() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get_template(424242).unwrap(), None);
        cleanup(&path);
    }

    #[test]
    fn export_conversation_unknown_id_errors() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        assert!(store.export_conversation("no-such-conversation").is_err());
        cleanup(&path);
    }

    #[test]
    fn conversation_template_bind() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let id = store
            .upsert_template("Foreman", "you are the foreman")
            .unwrap();

        assert_eq!(
            store.get_conversation_template(&conv).unwrap(),
            None,
            "unbound at first"
        );
        store.set_conversation_template(&conv, Some(id)).unwrap();
        let bound = store.get_conversation_template(&conv).unwrap().unwrap();
        assert_eq!(bound.id, id);
        assert_eq!(bound.content, "you are the foreman");
        cleanup(&path);
    }

    #[test]
    fn filename_sanitise() {
        let exp = ExportedConversation {
            title: "Hello World! #1".to_string(),
            created_at: EXPORT_TS,
            messages: Vec::new(),
        };
        assert_eq!(exp.filename(), "hello-world-1-2025-06-15.md");
    }

    #[test]
    fn sanitise_title_rules() {
        // Runs of disallowed chars become ONE '-'; case folds; '_' survives.
        assert_eq!(sanitise_title("Hello World! #1"), "hello-world-1");
        assert_eq!(sanitise_title("a___b"), "a___b");
        // Leading/trailing separators (literal or replaced) are stripped.
        assert_eq!(sanitise_title("--wow--"), "wow");
        assert_eq!(sanitise_title("?!?"), "");
        // Consecutive '-' collapse even when typed literally.
        assert_eq!(sanitise_title("a--b"), "a-b");
        // Truncation to 60, re-stripping a '-' the cut exposes.
        let long = "a".repeat(59) + "-bcd";
        assert_eq!(sanitise_title(&long), "a".repeat(59));
        let exact = "b".repeat(80);
        assert_eq!(sanitise_title(&exact), "b".repeat(60));
    }

    // ---- file attachments (slice 19) -----------------------------------------

    /// An [`Attachment`] literal for the pure-formatter tests — `id`/timestamps
    /// are irrelevant to formatting; `size_bytes` mirrors the content unless a
    /// test overrides it to model an oversized blob.
    fn att(filename: &str, mime: &str, content: &[u8]) -> Attachment {
        Attachment {
            id: 1,
            filename: filename.to_string(),
            mime_type: mime.to_string(),
            size_bytes: content.len() as i64,
            content: content.to_vec(),
            created_at: 0,
        }
    }

    #[test]
    fn attachment_store_and_retrieve() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let id = store
            .store_attachment("notes.txt", "text/plain", b"hello file")
            .unwrap();
        store.stage_attachment(&conv, id).unwrap();

        let staged = store.get_staged_attachments(&conv).unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].id, id);
        assert_eq!(staged[0].filename, "notes.txt");
        assert_eq!(staged[0].mime_type, "text/plain");
        assert_eq!(staged[0].size_bytes, 10);
        assert_eq!(staged[0].content, b"hello file");
        cleanup(&path);
    }

    #[test]
    fn attachment_stage_and_clear() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let a = store.store_attachment("a.txt", "text/plain", b"a").unwrap();
        let b = store.store_attachment("b.txt", "text/plain", b"b").unwrap();
        store.stage_attachment(&conv, a).unwrap();
        store.stage_attachment(&conv, b).unwrap();
        assert_eq!(store.get_staged_attachments(&conv).unwrap().len(), 2);

        store.clear_staged_attachments(&conv).unwrap();
        assert!(store.get_staged_attachments(&conv).unwrap().is_empty());
        // The blobs themselves survive the clear — they are history.
        let kept: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 2);
        cleanup(&path);
    }

    #[test]
    fn attachment_unstage_one() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let a = store.store_attachment("a.txt", "text/plain", b"a").unwrap();
        let b = store.store_attachment("b.txt", "text/plain", b"b").unwrap();
        store.stage_attachment(&conv, a).unwrap();
        store.stage_attachment(&conv, b).unwrap();

        store.unstage_attachment(&conv, a).unwrap();
        let staged = store.get_staged_attachments(&conv).unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].id, b, "the other staged row is untouched");
        cleanup(&path);
    }

    #[test]
    fn attachment_cascade_delete() {
        // staged_attachments cascades with its conversation. No public delete
        // exists (archive is soft), so the cascade is exercised with raw SQL.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let a = store.store_attachment("a.txt", "text/plain", b"a").unwrap();
        store.stage_attachment(&conv, a).unwrap();

        store
            .conn
            .execute("DELETE FROM conversations WHERE id = ?1", [&conv])
            .unwrap();
        let staged: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM staged_attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(staged, 0, "staged rows cascade away with the conversation");
        cleanup(&path);
    }

    #[test]
    fn attachment_staging_order_is_preserved() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        // Stage in reverse-alphabetical order; readback must follow staging
        // order (created_at, id), not attachment id or name.
        let b = store.store_attachment("b.txt", "text/plain", b"b").unwrap();
        let a = store.store_attachment("a.txt", "text/plain", b"a").unwrap();
        store.stage_attachment(&conv, a).unwrap();
        store.stage_attachment(&conv, b).unwrap();
        let names: Vec<_> = store
            .get_staged_attachments(&conv)
            .unwrap()
            .into_iter()
            .map(|x| x.filename)
            .collect();
        assert_eq!(names, ["a.txt", "b.txt"]);
        cleanup(&path);
    }

    #[test]
    fn attachments_migrate_v4_database_additively() {
        // A populated v4 file (the pre-slice19 layout) opens, gains the two
        // attachment tables, and keeps its data.
        let path = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "BEGIN;
                 {SCHEMA_V2}
                 {USAGE_EVENTS_DDL}
                 {MESSAGES_FTS_DDL}
                 PRAGMA user_version = {SCHEMA_V4_VERSION};
                 COMMIT;"
            ))
            .unwrap();
            conn.execute_batch(
                "INSERT INTO conversations VALUES ('c1', 't', 0, 0, 0, 0);
                 INSERT INTO messages (conversation_id, role, text, ts)
                   VALUES ('c1', 'user', 'pre-attachment era', 1);",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let v: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(
            store.messages("c1").unwrap().len(),
            1,
            "existing data intact"
        );
        let id = store.store_attachment("a.txt", "text/plain", b"a").unwrap();
        store.stage_attachment("c1", id).unwrap();
        assert_eq!(store.get_staged_attachments("c1").unwrap().len(), 1);
        cleanup(&path);
    }

    #[test]
    fn projects_migrate_v9_database_additively() {
        // A real v9 file (built through the migration chain, stamped v9 with no
        // `projects` table) opens, gains the table, and the project API — which
        // failed at runtime before v10 because the table never existed — works.
        let path = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            create_schema_v5(&conn).unwrap();
            migrate_v5_to_v6(&conn).unwrap();
            migrate_v6_to_v7(&conn).unwrap();
            migrate_v7_to_v8(&conn).unwrap();
            migrate_v8_to_v9(&conn).unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_V9_VERSION, "fixture must be a genuine v9 DB");
            assert!(
                conn.query_row(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name='projects'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .is_err(),
                "a v9 DB must NOT have the projects table yet"
            );
        }
        let store = Store::open(&path).unwrap();
        let v: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let id = store
            .add_project("awili", "/Users/example/Projects/awili", "", 1)
            .unwrap();
        assert!(id > 0);
        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].folder, "/Users/example/Projects/awili");
        cleanup(&path);
    }

    #[test]
    fn add_project_is_idempotent_by_folder() {
        // A double-click (or two SaveProject dispatches racing before the UI
        // mirror refreshes) must not create duplicate rows: `folder` is UNIQUE
        // and `add_project` is INSERT OR IGNORE, so the second save is a no-op.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store
            .add_project("awili", "/Users/example/Projects/awili", "", 1)
            .unwrap();
        store
            .add_project("awili-again", "/Users/example/Projects/awili", "x", 2)
            .unwrap();
        let projects = store.list_projects().unwrap();
        assert_eq!(
            projects.len(),
            1,
            "the duplicate folder is ignored, no second row"
        );
        assert_eq!(projects[0].name, "awili", "the first save wins");
        cleanup(&path);
    }

    #[test]
    fn remove_project_deletes_only_the_named_row() {
        // Deleting one saved project leaves the others intact, and the gone row
        // never reappears in a later `list_projects` (the backing for the UI's
        // post-delete mirror refresh).
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let a = store.add_project("alpha", "/p/alpha", "", 1).unwrap();
        let b = store.add_project("beta", "/p/beta", "", 2).unwrap();
        store.remove_project(a).unwrap();
        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 1, "only the deleted row is gone");
        assert_eq!(projects[0].id, b, "the surviving row is beta");
        // Deleting an id that no longer exists is a harmless no-op, never an error.
        store.remove_project(a).unwrap();
        assert_eq!(store.list_projects().unwrap().len(), 1);
        cleanup(&path);
    }

    #[test]
    fn update_project_instructions_edits_only_the_named_row() {
        // Editing one project's standing instructions persists for that row and
        // leaves the others untouched (the backing for the editor's post-save
        // mirror refresh). Read back via list_projects, the source of the mirror.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let a = store.add_project("alpha", "/p/alpha", "old-a", 1).unwrap();
        let b = store.add_project("beta", "/p/beta", "old-b", 2).unwrap();
        assert_eq!(
            store.update_project_instructions(a, "new-a").unwrap(),
            1,
            "exactly one row updated"
        );
        let by_id = |id: i64, ps: &[StoredProject]| {
            ps.iter().find(|p| p.id == id).unwrap().instructions.clone()
        };
        let projects = store.list_projects().unwrap();
        assert_eq!(
            by_id(a, &projects),
            "new-a",
            "alpha's instructions are updated"
        );
        assert_eq!(by_id(b, &projects), "old-b", "beta is untouched");
        // Updating an id that no longer exists touches zero rows (the Store is a
        // thin SQL wrapper, so it's Ok(0), not an error) — the worker turns that
        // 0 into a user-facing failure so the edit isn't silently lost.
        store.remove_project(a).unwrap();
        assert_eq!(
            store.update_project_instructions(a, "ghost").unwrap(),
            0,
            "no row updated for a deleted project"
        );
        assert_eq!(store.list_projects().unwrap().len(), 1);
        cleanup(&path);
    }

    // ---- attachment formatting (slice 19 §2) ----------------------------------

    #[test]
    fn format_text_file() {
        let a = att("notes.txt", "text/plain", b"line one\nline two");
        assert_eq!(
            format_attachment_for_message(&a),
            "```notes.txt\nline one\nline two\n```"
        );
    }

    #[test]
    fn format_text_truncation() {
        let long = "x".repeat(8001);
        let a = att("big.txt", "text/plain", long.as_bytes());
        let out = format_attachment_for_message(&a);
        assert!(out.starts_with("```big.txt\n"));
        assert!(out.contains("…[truncated]"));
        // 8000 kept chars, no more.
        let body = out
            .strip_prefix("```big.txt\n")
            .and_then(|s| s.strip_suffix("\n```"))
            .unwrap();
        assert_eq!(body.chars().filter(|c| *c == 'x').count(), 8000);
        // An exactly-8000-char file is NOT truncated.
        let exact = "y".repeat(8000);
        let b = att("ok.txt", "text/plain", exact.as_bytes());
        assert!(!format_attachment_for_message(&b).contains("[truncated]"));
    }

    #[test]
    fn format_image_small() {
        let a = att("pic.png", "image/png", &[0x89, 0x50, 0x4e, 0x47]);
        assert_eq!(
            format_attachment_for_message(&a),
            "![pic.png](data:image/png;base64,iVBORw==)"
        );
    }

    #[test]
    fn format_image_large() {
        // Model the oversized blob via size_bytes — no 4 MB buffer needed.
        let mut a = att("huge.png", "image/png", &[0u8; 4]);
        a.size_bytes = 4_194_305;
        assert_eq!(
            format_attachment_for_message(&a),
            "[Attachment: huge.png — unsupported type or too large (max 4 MB for images)]"
        );
        // The boundary itself (exactly 4 MB) is still inlined.
        let mut b = att("edge.png", "image/png", &[0u8; 4]);
        b.size_bytes = 4_194_304;
        assert!(format_attachment_for_message(&b).starts_with("![edge.png](data:image/png;"));
    }

    #[test]
    fn format_unsupported_mime() {
        let a = att("bundle.zip", "application/zip", b"PK");
        assert_eq!(
            format_attachment_for_message(&a),
            "[Attachment: bundle.zip — unsupported type or too large (max 4 MB for images)]"
        );
    }

    #[test]
    fn format_json_file() {
        let a = att("data.json", "application/json", b"{\"k\": 1}");
        assert_eq!(
            format_attachment_for_message(&a),
            "```data.json\n{\"k\": 1}\n```"
        );
    }

    #[test]
    fn conversation_template_unset() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let id = store.upsert_template("Foreman", "x").unwrap();
        store.set_conversation_template(&conv, Some(id)).unwrap();
        store.set_conversation_template(&conv, None).unwrap();
        assert_eq!(store.get_conversation_template(&conv).unwrap(), None);
        cleanup(&path);
    }

    #[test]
    fn template_delete_unbinds_conversations() {
        // The FK action: deleting a template clears the binding (SET NULL) on
        // every conversation that used it — the conversations survive intact.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        store.append_message(&conv, "user", "precious", 0).unwrap();
        let id = store.upsert_template("Foreman", "x").unwrap();
        store.set_conversation_template(&conv, Some(id)).unwrap();

        store.delete_template(id).unwrap();
        assert_eq!(store.get_conversation_template(&conv).unwrap(), None);
        assert_eq!(store.messages(&conv).unwrap().len(), 1);
        cleanup(&path);
    }

    #[test]
    fn template_get_returns_saved_row() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let id = store
            .upsert_template("Foreman", "you are the foreman")
            .unwrap();
        let t = store.get_template(id).unwrap().expect("row exists");
        assert_eq!(t.id, id);
        assert_eq!(t.name, "Foreman");
        assert_eq!(t.content, "you are the foreman");
        cleanup(&path);
    }

    #[test]
    fn conversation_template_rebind_replaces() {
        // Spec §3: applying a template to a conversation that already has one
        // replaces the binding.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "t", 0);
        let a = store.upsert_template("A", "first").unwrap();
        let b = store.upsert_template("B", "second").unwrap();
        store.set_conversation_template(&conv, Some(a)).unwrap();
        store.set_conversation_template(&conv, Some(b)).unwrap();
        let bound = store.get_conversation_template(&conv).unwrap().unwrap();
        assert_eq!(bound.id, b);
        assert_eq!(bound.content, "second");
        cleanup(&path);
    }

    #[test]
    fn template_delete_missing_id_is_ok() {
        // Deleting an id that does not exist is a no-op, not an error — the
        // palette row may race a delete from another window.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.upsert_template("Keep", "x").unwrap();
        store.delete_template(424242).unwrap();
        assert_eq!(store.list_templates().unwrap().len(), 1);
        cleanup(&path);
    }

    #[test]
    fn migration_idempotent() {
        // Opening a store twice (schema already at v5) must not error — the
        // ALTER TABLE step in particular only ever runs on an un-stamped file.
        let path = temp_db();
        {
            let store = Store::open(&path).unwrap();
            store.upsert_template("Foreman", "x").unwrap();
        }
        let reopened = Store::open(&path).unwrap();
        let v: i64 = reopened
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(reopened.list_templates().unwrap().len(), 1);
        cleanup(&path);
    }

    #[test]
    fn v4_database_migrates_to_v5_with_data_intact() {
        // A populated pre-slice18 (v4) file gains the template table and the
        // binding column without touching existing rows.
        let path = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "BEGIN;
                 {SCHEMA_V2}
                 {USAGE_EVENTS_DDL}
                 {MESSAGES_FTS_DDL}
                 PRAGMA user_version = {SCHEMA_V4_VERSION};
                 COMMIT;"
            ))
            .unwrap();
            conn.execute_batch(
                "INSERT INTO conversations VALUES ('c1', 't', 0, 0, 0, 0);
                 INSERT INTO messages (conversation_id, role, text, ts)
                   VALUES ('c1', 'user', 'pre-template era', 1);",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let v: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(store.messages("c1").unwrap().len(), 1);
        assert_eq!(store.get_conversation_template("c1").unwrap(), None);
        let id = store.upsert_template("Foreman", "x").unwrap();
        store.set_conversation_template("c1", Some(id)).unwrap();
        assert!(store.get_conversation_template("c1").unwrap().is_some());
        cleanup(&path);
    }

    /// RC2-RUNF: a plan-card click row keeps its `"plan_click"` kind; a row
    /// with the same words appended the ordinary way reads as `"normal"`.
    #[test]
    fn rc2runf_a_plan_click_row_keeps_its_kind() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "Plan", 0);
        let words = "Approved \u{2014} go ahead and make the changes you proposed.";
        store
            .append_message_kind(&conv, "user", words, 1, "plan_click")
            .unwrap();
        store.append_message(&conv, "user", words, 2).unwrap();
        let kinds: Vec<(String, String)> = store
            .messages(&conv)
            .unwrap()
            .into_iter()
            .map(|m| (m.text, m.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                (words.to_string(), "plan_click".to_string()),
                (words.to_string(), "normal".to_string()),
            ]
        );
        cleanup(&path);
    }

    #[test]
    fn summarize_earlier_appends_summary_and_supersedes_by_ts() {
        // Slice 21 §D4: the atomic durable half of the summarize action — a
        // summary row is appended (kind = "summary") and exactly the turns named
        // by their `ts` are marked superseded by it; an untouched turn stays
        // live, and a `ts` with no matching row is skipped without error.
        let path = temp_db();
        let mut store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "Long thread", 0);
        store.append_message(&conv, "user", "turn one", 1).unwrap();
        store
            .append_message(&conv, "assistant", "reply one", 2)
            .unwrap();
        store.append_message(&conv, "user", "turn two", 3).unwrap();

        // Supersede the two oldest turns (ts 1, 2) plus a non-existent ts 99.
        let summary_id = store
            .summarize_earlier(&conv, "compressed earlier turns", 4, &[1, 2, 99])
            .unwrap();

        let msgs = store.messages(&conv).unwrap();
        assert_eq!(msgs.len(), 4, "summary row appended");
        let by_ts = |ts: i64| msgs.iter().find(|m| m.ts == ts).unwrap();
        assert_eq!(by_ts(1).superseded_by, Some(summary_id), "ts 1 superseded");
        assert_eq!(by_ts(2).superseded_by, Some(summary_id), "ts 2 superseded");
        assert_eq!(by_ts(3).superseded_by, None, "untouched turn stays live");
        let summary = by_ts(4);
        assert_eq!(summary.kind, "summary");
        assert_eq!(summary.text, "compressed earlier turns");
        assert_eq!(
            summary.superseded_by, None,
            "the summary never supersedes itself"
        );
        cleanup(&path);
    }

    #[test]
    fn discard_summary_restores_turns_and_deletes_only_that_summary_row() {
        // Slice 21 §D4 "Discard summary": the durable inverse of
        // `summarize_earlier` — un-supersede every row the summary replaced and
        // delete the summary row itself, in one transaction, addressed by ts.
        let path = temp_db();
        let mut store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "Long thread", 0);
        store.append_message(&conv, "user", "turn one", 1).unwrap();
        store
            .append_message(&conv, "assistant", "reply one", 2)
            .unwrap();
        store.append_message(&conv, "user", "turn two", 3).unwrap();
        store
            .summarize_earlier(&conv, "compressed earlier turns", 4, &[1, 2])
            .unwrap();
        assert_eq!(store.messages(&conv).unwrap().len(), 4, "summary present");

        let restored = store.discard_summary(&conv, 4).unwrap();
        assert_eq!(restored, 2, "both superseded turns restored");
        let msgs = store.messages(&conv).unwrap();
        assert_eq!(msgs.len(), 3, "the summary row is gone");
        assert!(
            msgs.iter().all(|m| m.superseded_by.is_none()),
            "all turns live again"
        );
        assert!(
            msgs.iter().all(|m| m.kind == "normal"),
            "no summary row left"
        );
        cleanup(&path);
    }

    #[test]
    fn discard_summary_closes_ts_gap_and_preserves_a_later_summarys_link() {
        // The restart-safety guarantee: discarding a summary that is NOT the last
        // row must leave the stored `ts` column gapless (0..n in order), because
        // a restore re-derives `ts` positionally. A later summary's link (keyed by
        // row id) must survive the bulk ts shift untouched.
        let path = temp_db();
        let mut store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "Long thread", 0);
        // ts 0 alpha, 1 beta, then summary at ts 2 (supersedes ts 0)...
        store.append_message(&conv, "user", "alpha", 0).unwrap();
        store.append_message(&conv, "user", "beta", 1).unwrap();
        store.summarize_earlier(&conv, "S1", 2, &[0]).unwrap();
        // ...the user keeps chatting (ts 3), then a second summary at ts 4
        // supersedes beta (ts 1).
        store.append_message(&conv, "user", "gamma", 3).unwrap();
        store.summarize_earlier(&conv, "S2", 4, &[1]).unwrap();
        let s2_id = store
            .messages(&conv)
            .unwrap()
            .iter()
            .find(|m| m.ts == 4)
            .unwrap()
            .id;

        let restored = store.discard_summary(&conv, 2).unwrap();
        assert_eq!(restored, 1, "alpha restored");
        let msgs = store.messages(&conv).unwrap();
        // The hole at ts 2 is closed: ts is a contiguous 0..n in row order.
        let tss: Vec<i64> = msgs.iter().map(|m| m.ts).collect();
        assert_eq!(tss, vec![0, 1, 2, 3], "ts stays gapless after the delete");
        // alpha is live again; beta is still superseded by the SAME summary row.
        assert_eq!(
            msgs.iter()
                .find(|m| m.text == "alpha")
                .unwrap()
                .superseded_by,
            None
        );
        assert_eq!(
            msgs.iter()
                .find(|m| m.text == "beta")
                .unwrap()
                .superseded_by,
            Some(s2_id),
            "the surviving summary's link is by row id, unbroken by the ts shift"
        );
        // S2 slid down from ts 4 to ts 3 but kept its row id.
        let s2 = msgs.iter().find(|m| m.id == s2_id).unwrap();
        assert_eq!(s2.ts, 3);
        assert_eq!(s2.kind, "summary");
        cleanup(&path);
    }

    #[test]
    fn discard_summary_unknown_ts_is_a_noop() {
        // A ts that names no summary row restores and deletes nothing — never an
        // error, and the live conversation is untouched.
        let path = temp_db();
        let mut store = Store::open(&path).unwrap();
        let conv = new_conv(&store, "Thread", 0);
        store.append_message(&conv, "user", "turn one", 1).unwrap();
        store.summarize_earlier(&conv, "summary", 2, &[1]).unwrap();

        // ts 99 names no row; ts 1 is a normal turn, not a summary.
        assert_eq!(store.discard_summary(&conv, 99).unwrap(), 0);
        assert_eq!(store.discard_summary(&conv, 1).unwrap(), 0);
        let msgs = store.messages(&conv).unwrap();
        assert_eq!(msgs.len(), 2, "nothing deleted");
        assert_eq!(
            msgs.iter().find(|m| m.ts == 1).unwrap().superseded_by,
            msgs.iter().find(|m| m.ts == 2).map(|m| m.id),
            "turn one still superseded by the summary"
        );
        cleanup(&path);
    }

    #[test]
    fn fts_backfill_indexes_existing_v3_database() {
        // Acceptance §7: a populated pre-slice14 (v3) file must be searchable
        // after the v3→v4 migration — the backfill runs, and exactly once
        // (reopening at v4 must not duplicate index rows).
        let path = temp_db();
        {
            // Build a real v3 file: current schema minus the FTS table, with a
            // populated conversation.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "BEGIN;
                 {SCHEMA_V2}
                 {USAGE_EVENTS_DDL}
                 PRAGMA user_version = {SCHEMA_V3_VERSION};
                 COMMIT;"
            ))
            .unwrap();
            conn.execute_batch(
                "INSERT INTO conversations VALUES ('c1', 't', 0, 0, 0, 0);
                 INSERT INTO messages (conversation_id, role, text, ts)
                   VALUES ('c1', 'user', 'pre-migration walrus', 1);",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.search_messages("walrus", 10).unwrap().len(), 1);
        drop(store);

        let reopened = Store::open(&path).unwrap();
        let indexed: i64 = reopened
            .conn
            .query_row("SELECT COUNT(*) FROM messages_fts('walrus')", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(indexed, 1, "reopen at v4 must not re-run the backfill");
        cleanup(&path);
    }
    // ---- Capture outbox tests (slice 22, stage 1A) --------------------------------------

    fn test_binding() -> capture_outbox::OfflineCaptureBinding {
        capture_outbox::OfflineCaptureBinding {
            epoch: "ep-1".to_string(),
            installation_id: "inst-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            user_id: "user-1".to_string(),
            consumer_id: uuid::Uuid::new_v4(),
        }
    }

    fn test_input(
        binding: &capture_outbox::OfflineCaptureBinding,
    ) -> capture_outbox::OfflineCaptureInput {
        capture_outbox::OfflineCaptureInput {
            binding: binding.clone(),
            redacted_content: "redacted text".to_string(),
            adapter_version: "1.0.0".to_string(),
        }
    }

    #[test]
    fn fresh_schema_is_v14() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 14, "fresh DB must be at v14");
        cleanup(&path);
    }

    #[test]
    fn captured_append_returns_receipt_and_event() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "test", 1000);
        let binding = test_binding();
        let input = test_input(&binding);

        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&input))
            .unwrap();

        assert!(
            receipt.source_event_id.is_some(),
            "capture must mint an event id"
        );
        let event_id = receipt.source_event_id.as_ref().unwrap();
        assert!(
            event_id.starts_with("awili:"),
            "event id must be awili:<uuid>"
        );

        let events = store.capture_events(&binding, 10).unwrap();
        assert_eq!(events.len(), 1, "must have exactly one outbox row");
        assert_eq!(events[0].source_event_id, *event_id);
        assert_eq!(events[0].message_id, receipt.message_id);
        assert_eq!(events[0].redacted_content, "redacted text");
        assert_eq!(events[0].adapter_version, "1.0.0");
        assert_eq!(events[0].actor, "user");
        assert_eq!(events[0].ts, 0);
        cleanup(&path);
    }

    #[test]
    fn no_capture_no_outbox_row() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "test", 1000);
        let binding = test_binding();

        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, None)
            .unwrap();

        assert!(
            receipt.source_event_id.is_none(),
            "no capture = no event id"
        );

        let events = store.capture_events(&binding, 10).unwrap();
        assert_eq!(events.len(), 0, "no outbox rows when capture is None");
        cleanup(&path);
    }

    #[test]
    fn reopen_stability() {
        let path = temp_db();
        let (original_event_id, original_msg_id);
        // Use a fixed consumer UUID so the second open can query with the same binding
        let fixed_consumer = uuid::Uuid::new_v4();
        {
            let store = Store::open(&path).unwrap();
            let convo = new_conv(&store, "test", 1000);
            let binding = capture_outbox::OfflineCaptureBinding {
                epoch: "ep-1".to_string(),
                installation_id: "inst-1".to_string(),
                tenant_id: "tenant-1".to_string(),
                user_id: "user-1".to_string(),
                consumer_id: fixed_consumer,
            };
            let input = capture_outbox::OfflineCaptureInput {
                binding: binding.clone(),
                redacted_content: "redacted text".to_string(),
                adapter_version: "1.0.0".to_string(),
            };

            let receipt = store
                .append_message_with_capture(&convo, "user", "hello", 0, Some(&input))
                .unwrap();
            original_event_id = receipt.source_event_id.unwrap();
            original_msg_id = receipt.message_id;
        }

        // Reopen and verify the event id is stable
        {
            let store = Store::open(&path).unwrap();
            let binding = capture_outbox::OfflineCaptureBinding {
                epoch: "ep-1".to_string(),
                installation_id: "inst-1".to_string(),
                tenant_id: "tenant-1".to_string(),
                user_id: "user-1".to_string(),
                consumer_id: fixed_consumer,
            };
            let events = store.capture_events(&binding, 10).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(
                events[0].source_event_id, original_event_id,
                "event id must be stable across reopens"
            );
            assert_eq!(events[0].message_id, original_msg_id);
        }
        cleanup(&path);
    }

    #[test]
    fn different_consumer_no_rows() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "test", 1000);
        let binding = test_binding();
        let input = test_input(&binding);

        store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&input))
            .unwrap();

        // Query with a different consumer_id
        let mut other_binding = binding.clone();
        other_binding.consumer_id = uuid::Uuid::new_v4();

        let events = store.capture_events(&other_binding, 10).unwrap();
        assert_eq!(events.len(), 0, "different consumer_id must return no rows");
        cleanup(&path);
    }

    #[test]
    fn equal_ts_different_event_ids() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "test", 1000);
        let binding = test_binding();
        let input = test_input(&binding);

        let r1 = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&input))
            .unwrap();
        let r2 = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&input))
            .unwrap();

        assert_ne!(
            r1.source_event_id, r2.source_event_id,
            "same content/ts must still mint different event ids"
        );

        let events = store.capture_events(&binding, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_ne!(events[0].source_event_id, events[1].source_event_id);
        cleanup(&path);
    }

    #[test]
    fn old_append_message_no_outbox() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "test", 1000);
        let binding = test_binding();

        // Use the old append_message
        store.append_message(&convo, "user", "hello", 0).unwrap();

        let events = store.capture_events(&binding, 10).unwrap();
        assert_eq!(
            events.len(),
            0,
            "old append_message must not create outbox rows"
        );
        cleanup(&path);
    }

    #[test]
    fn retrieval_is_bounded_to_max() {
        // The LIMIT bug guard: with more than MAX_CAPTURE_RETRIEVAL eligible
        // rows, a `usize::MAX` request must still return exactly
        // MAX_CAPTURE_RETRIEVAL rows (and not the whole table). `0` is an
        // empty retrieval, and the returned prefix is the stable
        // (source_ts ASC, message_id ASC) head of the ordered list.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let binding = test_binding();
        let convo = new_conv(&store, "bounded", 1000);

        // Insert 600 distinct eligible rows through the public API (one
        // capture per message), so the rows are all genuinely present and
        // identity-identical in the binding.
        let n = 600;
        let mut receipt_ids = Vec::with_capacity(n);
        for i in 0..n {
            let input = capture_outbox::OfflineCaptureInput {
                binding: binding.clone(),
                redacted_content: format!("msg-{i}"),
                adapter_version: "1.0.0".to_string(),
            };
            let receipt = store
                .append_message_with_capture(
                    &convo,
                    "user",
                    &format!("m{i}"),
                    i as i64,
                    Some(&input),
                )
                .unwrap();
            receipt_ids.push(receipt.source_event_id.unwrap());
        }

        // 0 => empty, never errors.
        let zero = store.capture_events(&binding, 0).unwrap();
        assert!(zero.is_empty(), "limit 0 must return no rows");

        // 1 => exactly the first row in stable order.
        let one = store.capture_events(&binding, 1).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].source_event_id, receipt_ids[0]);
        assert_eq!(one[0].message_id, store.messages(&convo).unwrap()[0].id);

        // usize::MAX must clamp to the hard bound, not the whole table.
        let max = store.capture_events(&binding, usize::MAX).unwrap();
        assert_eq!(
            max.len(),
            MAX_CAPTURE_RETRIEVAL,
            "usize::MAX must clamp to MAX_CAPTURE_RETRIEVAL, not return all {n} rows"
        );

        // 513 > 512 clamps identically.
        let just_over = store
            .capture_events(&binding, MAX_CAPTURE_RETRIEVAL + 1)
            .unwrap();
        assert_eq!(just_over.len(), MAX_CAPTURE_RETRIEVAL);
        assert_eq!(just_over, max, "clamp of 513 and usize::MAX must agree");

        // The returned block is exactly the stable-ordered prefix: a larger
        // request (still clamped to the same bound) must return the same head
        // row-for-row, and each returned row must be the earliest `len` rows
        // of the full ordered list (source_ts ASC — ts was set to 0..n).
        assert_eq!(
            just_over.iter().map(|e| e.ts).collect::<Vec<_>>(),
            (0..MAX_CAPTURE_RETRIEVAL as i64).collect::<Vec<_>>(),
            "the clamped result must be the earliest MAX_CAPTURE_RETRIEVAL rows in order"
        );
        assert_eq!(
            just_over
                .iter()
                .map(|e| e.source_event_id.clone())
                .collect::<Vec<_>>(),
            receipt_ids[..MAX_CAPTURE_RETRIEVAL].to_vec()
        );
        cleanup(&path);
    }

    #[test]
    fn check_failure_rolls_back_message_and_outbox() {
        // A CHECK violation inside the capture (empty redacted_content or
        // empty adapter_version) must fail the append as a whole: the
        // in-transaction message insert is rolled back too, leaving the
        // messages table and the outbox exactly as before — and the next,
        // valid append must still succeed.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "check", 1000);
        let binding = test_binding();
        let input = test_input(&binding);
        let before = store.messages(&convo).unwrap();
        assert!(before.is_empty());
        let before_outbox = store.capture_events(&binding, 10).unwrap();
        assert!(before_outbox.is_empty());

        // Empty redacted_content violates CHECK (length(redacted_content) > 0).
        let bad_content = capture_outbox::OfflineCaptureInput {
            binding: binding.clone(),
            redacted_content: String::new(),
            adapter_version: "1.0.0".to_string(),
        };
        assert!(
            store
                .append_message_with_capture(
                    &convo,
                    "user",
                    "will not persist",
                    0,
                    Some(&bad_content)
                )
                .is_err(),
            "empty redacted_content must be rejected"
        );

        // Empty adapter_version violates CHECK (length(adapter_version) > 0).
        let bad_adapter = capture_outbox::OfflineCaptureInput {
            binding: binding.clone(),
            redacted_content: "fine text".to_string(),
            adapter_version: String::new(),
        };
        assert!(
            store
                .append_message_with_capture(
                    &convo,
                    "user",
                    "will not persist",
                    0,
                    Some(&bad_adapter)
                )
                .is_err(),
            "empty adapter_version must be rejected"
        );

        // Nothing partial landed: no message rows, no outbox rows.
        let after = store.messages(&convo).unwrap();
        assert_eq!(
            after.len(),
            0,
            "message insert must be rolled back with the failed capture"
        );
        let after_outbox = store.capture_events(&binding, 10).unwrap();
        assert_eq!(
            after_outbox.len(),
            0,
            "outbox insert must be rolled back with the failed capture"
        );

        // The store still works: a valid append succeeds afterwards.
        let receipt = store
            .append_message_with_capture(&convo, "user", "good", 1, Some(&input))
            .unwrap();
        let good = store.messages(&convo).unwrap();
        assert_eq!(good.len(), 1);
        assert_eq!(good[0].id, receipt.message_id);
        assert_eq!(good[0].text, "good");
        let good_outbox = store.capture_events(&binding, 10).unwrap();
        assert_eq!(good_outbox.len(), 1);
        assert_eq!(good_outbox[0].message_id, receipt.message_id);
        cleanup(&path);
    }

    #[test]
    fn invalid_capture_role_fails_fast() {
        // Roles other than user/assistant must be rejected EXPLICITLY (the
        // store's own error, before any write): zero rows in messages AND
        // zero rows in the outbox, for both `tool` and `system`.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "role", 1000);
        let binding = test_binding();
        let input = test_input(&binding);

        for role in ["tool", "system"] {
            let err = store
                .append_message_with_capture(&convo, role, "x", 0, Some(&input))
                .unwrap_err();
            assert!(
                err.to_string().contains(role),
                "error must name the rejected role, got: {err}"
            );
            let msgs = store.messages(&convo).unwrap();
            assert!(
                msgs.is_empty(),
                "no partial message row may survive a {role} capture"
            );
            let outbox = store.capture_events(&binding, 10).unwrap();
            assert!(
                outbox.is_empty(),
                "no partial outbox row may survive a {role} capture"
            );
        }

        // The store remains usable for valid roles after the rejections.
        let receipt = store
            .append_message_with_capture(&convo, "assistant", "ok", 1, Some(&input))
            .unwrap();
        assert_eq!(store.messages(&convo).unwrap().len(), 1);
        assert_eq!(
            store
                .capture_events(&binding, 10)
                .unwrap()
                .iter()
                .find(|e| e.message_id == receipt.message_id)
                .map(|e| e.actor.clone()),
            Some("assistant".to_string())
        );
        cleanup(&path);
    }

    #[test]
    fn migrate_v10_database_additively() {
        // A genuine v10 file — built through the REAL migration chain
        // (create_schema_v5, then v5->v6 ... v9->v10) and stamped exactly v10
        // with NO capture_outbox table — carries a conversation, a message
        // (ts/kind/text preserved) and a project. Opening it must land it at
        // v12 (through the explicit v10→v11 and v11→v12 steps), keep every
        // old row byte-for-byte, create an EMPTY capture outbox with every
        // row at exposure 'never_sent' and a ZERO-row intent table, and stay
        // idempotent on a second open.
        let path = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            create_schema_v5(&conn).unwrap();
            migrate_v5_to_v6(&conn).unwrap();
            migrate_v6_to_v7(&conn).unwrap();
            migrate_v7_to_v8(&conn).unwrap();
            migrate_v8_to_v9(&conn).unwrap();
            migrate_v9_to_v10(&conn).unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_V10_VERSION, "fixture must be a genuine v10 DB");
            assert!(
                conn.query_row(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name='capture_outbox'",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .is_err(),
                "a v10 DB must NOT have the capture_outbox table yet"
            );
            // v10 conversations has 10 columns (id, title, created_ts,
            // updated_ts, archived, title_is_custom, cwd, profile, kind), so
            // the fixture insert lists the 6 legacy columns explicitly and
            // leaves cwd/profile/kind NULL (the pre-v7 shape of the row).
            conn.execute_batch(
                "INSERT INTO conversations (id, title, created_ts, updated_ts, archived, title_is_custom)
                   VALUES ('c1', 'migrated', 100, 200, 0, 0);
                 INSERT INTO messages (conversation_id, role, text, ts, kind)
                   VALUES ('c1', 'user', 'original text', 42, 'note');
                 INSERT INTO projects (name, folder, instructions, created_ts)
                   VALUES ('awili', '/proj/awili', 'instr', 7);",
            )
            .unwrap();
        }
        {
            let store = Store::open(&path).unwrap();
            let v: i64 = store
                .conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_VERSION, "open must migrate v10 to v12 (via v11)");

            // Conversation row byte-exact.
            let convos = store.conversations().unwrap();
            assert_eq!(convos.len(), 1);
            assert_eq!(convos[0].id, "c1");
            assert_eq!(convos[0].title, "migrated");
            assert_eq!(convos[0].created_ts, 100);
            assert_eq!(convos[0].updated_ts, 200);
            assert!(!convos[0].archived);
            assert!(!convos[0].title_is_custom);

            // Message row byte-exact: id, role, text, ts, kind all preserved.
            let msgs = store.messages("c1").unwrap();
            assert_eq!(msgs.len(), 1);
            assert_eq!(msgs[0].role, "user");
            assert_eq!(msgs[0].text, "original text");
            assert_eq!(msgs[0].ts, 42);
            assert_eq!(msgs[0].kind, "note");

            // Project row intact (v10 table, untouched by the v11 migration).
            let projects = store.list_projects().unwrap();
            assert_eq!(projects.len(), 1);
            assert_eq!(projects[0].name, "awili");
            assert_eq!(projects[0].folder, "/proj/awili");
            assert_eq!(projects[0].instructions, "instr");
            assert_eq!(projects[0].created_ts, 7);

            // The capture outbox exists but is EMPTY for any binding.
            let binding = test_binding();
            let outbox = store.capture_events(&binding, 10).unwrap();
            assert!(
                outbox.is_empty(),
                "freshly migrated capture_outbox must be empty"
            );
        }
        // Reopen: idempotent, still v12, data still intact.
        {
            let store = Store::open(&path).unwrap();
            let v: i64 = store
                .conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_VERSION, "second open must stay at v12");
            let msgs = store.messages("c1").unwrap();
            assert_eq!(msgs.len(), 1);
            assert_eq!(msgs[0].text, "original text");
            assert_eq!(msgs[0].ts, 42);
            assert_eq!(store.list_projects().unwrap().len(), 1);
            assert!(store
                .capture_events(&test_binding(), 10)
                .unwrap()
                .is_empty());
        }
        cleanup(&path);
    }

    #[test]
    fn binding_isolation_all_five_fields() {
        // Identity isolation is per-FIELD: changing any ONE of the five binding
        // components must yield zero rows, while the unmodified binding
        // returns the event.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "iso", 1000);
        let binding = test_binding();
        let input = test_input(&binding);
        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&input))
            .unwrap();

        let mut variants = Vec::new();
        let mut b = binding.clone();
        b.epoch = "ep-other".to_string();
        variants.push((b, "epoch"));
        let mut b = binding.clone();
        b.installation_id = "inst-other".to_string();
        variants.push((b, "installation_id"));
        let mut b = binding.clone();
        b.tenant_id = "tenant-other".to_string();
        variants.push((b, "tenant_id"));
        let mut b = binding.clone();
        b.user_id = "user-other".to_string();
        variants.push((b, "user_id"));
        let mut b = binding.clone();
        b.consumer_id = uuid::Uuid::new_v4();
        variants.push((b, "consumer_id"));

        for (variant, field) in &variants {
            let events = store.capture_events(variant, 10).unwrap();
            assert!(
                events.is_empty(),
                "changing {field} alone must hide the event, got {} rows",
                events.len()
            );
        }

        // The exact original binding still finds its event.
        let events = store.capture_events(&binding, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message_id, receipt.message_id);
        cleanup(&path);
    }

    #[test]
    fn reopen_full_event_equality() {
        // Reopening a store must return every capture event with ALL fields
        // exactly as written (not just the stable event id), and the
        // underlying message must keep its original text and ts.
        let path = temp_db();
        let (orig, convo_id, binding) = {
            let store = Store::open(&path).unwrap();
            let convo = new_conv(&store, "full", 1000);
            let b = test_binding();
            let input = capture_outbox::OfflineCaptureInput {
                binding: b.clone(),
                redacted_content: "secret redacted text".to_string(),
                adapter_version: "2.1.0".to_string(),
            };
            let r = store
                .append_message_with_capture(&convo, "assistant", "visible text", 99, Some(&input))
                .unwrap();
            (r, convo, b)
        };

        let store = Store::open(&path).unwrap();
        let events = store.capture_events(&binding, 10).unwrap();
        assert_eq!(events.len(), 1, "exactly one event after reopen");
        let e = &events[0];
        assert_eq!(e.source_event_id, orig.source_event_id.unwrap());
        assert_eq!(e.message_id, orig.message_id);
        assert_eq!(e.conversation_id, convo_id);
        assert_eq!(e.binding, binding, "full 5-field binding must round-trip");
        assert_eq!(e.actor, "assistant");
        assert_eq!(e.ts, 99, "source_ts must round-trip");
        assert_eq!(e.redacted_content, "secret redacted text");
        assert_eq!(e.adapter_version, "2.1.0");

        // The original message is untouched by the reopen.
        let msgs = store.messages(&convo_id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].id, orig.message_id);
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[0].text, "visible text");
        assert_eq!(msgs[0].ts, 99);
        cleanup(&path);
    }

    // ---- Exposure + deletion-intent tests (stage 1B-a) ---------------------------------
    //
    // Fixtures are inserted DIRECTLY through the raw test connection: 1B-a
    // deliberately has no application API to create or clear deletion
    // intents (that is 1B-b). These fixtures are NOT enrollment.

    /// Insert a synthetic deletion-intent row straight into the table.
    fn insert_intent(
        conn: &Connection,
        source_event_id: &str,
        conversation_id: &str,
        binding: &capture_outbox::OfflineCaptureBinding,
        source_ts: i64,
        requested_ts: i64,
        cause: &str,
    ) {
        conn.execute(
            "INSERT INTO capture_deletion_intents
                 (source_event_id, binding_epoch, installation_id, tenant_id,
                  user_id, consumer_id, conversation_id, source_ts, requested_ts,
                  cause)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                source_event_id,
                binding.epoch,
                binding.installation_id,
                binding.tenant_id,
                binding.user_id,
                binding.consumer_id.to_string(),
                conversation_id,
                source_ts,
                requested_ts,
                cause
            ],
        )
        .unwrap();
    }

    /// Build a POPULATED, genuine v11 file: created through the REAL chain
    /// (create_schema_v5, v5→…→v11, each step stamping exactly the version
    /// it reaches), stamped v11, with N capture outbox rows carrying real
    /// content. Returns the file path.
    ///
    /// NOTE: `create_schema_v5` is NOT the genuine v5 file shape — it already
    /// includes the `template_id` ALTER added by the v4→v5 migration, and the
    /// v4→v5 step re-adds it ("duplicate column name"). Every fresh-path
    /// dispatch skips v4→v5 (it lands at v5 via `create_schema_v5`), so this
    /// helper reproduces the FRESH-DB chain exactly: v5→v6…v10→v11 only.
    /// The result is a genuine v11 file with real v11 columns and real rows.
    ///
    /// The chain runs on a FRESH connection: `create_schema_v5` leaves no
    /// open transaction, but a second DDL batch on the same connection can
    /// fail with "table conversations already exists" (observed empirically,
    /// once, on the first connection — a fresh one is deterministic).
    fn populated_v11_db(binding: &capture_outbox::OfflineCaptureBinding, n_rows: i64) -> PathBuf {
        let path = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            create_schema_v5(&conn).unwrap();
        }
        {
            let conn = Connection::open(&path).unwrap();
            migrate_v5_to_v6(&conn).unwrap();
            migrate_v6_to_v7(&conn).unwrap();
            migrate_v7_to_v8(&conn).unwrap();
            migrate_v8_to_v9(&conn).unwrap();
            migrate_v9_to_v10(&conn).unwrap();
            migrate_v10_to_v11(&conn).unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, 11, "fixture must be a genuine v11 DB");
            // Fixture columns are the v11 shape (no `exposure` - v11's DDL
            // has never carried it).
            conn.execute(
                "INSERT INTO conversations (id, title, created_ts, updated_ts, archived, title_is_custom)
                 VALUES ('c1', 'v11 fixture', 1000, 1000, 0, 0)",
                [],
            )
            .unwrap();
            for i in 0..n_rows {
                let source_event_id = format!("awili:00000000-0000-4000-8000-{i:012x}");
                conn.execute(
                    "INSERT INTO capture_outbox
                         (source_event_id, message_id, conversation_id, binding_epoch,
                          installation_id, tenant_id, user_id, consumer_id, actor,
                          source_ts, redacted_content, adapter_version)
                       VALUES (?1, ?2, 'c1', ?3, ?4, ?5, ?6, ?7, 'user', ?8, 'redacted', '1.0.0')",
                    rusqlite::params![
                        source_event_id,
                        1000 + i,
                        binding.epoch,
                        binding.installation_id,
                        binding.tenant_id,
                        binding.user_id,
                        binding.consumer_id.to_string(),
                        50 + i,
                    ],
                )
                .unwrap();
            }
        }
        path
    }

    #[test]
    fn populated_v11_migrates_to_v12_with_never_sent_and_zero_intents() {
        // A populated v11 file (real old schema, real queued rows) opens at
        // v12: every queued row survives at exposure 'never_sent', the
        // intent table exists and has ZERO rows, old columns are intact.
        let binding = test_binding();
        let path = populated_v11_db(&binding, 5);
        let store = Store::open(&path).unwrap();
        let v: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "populated v11 DB must land at v12");

        // Every queued row preserved, exposure defaulted to never_sent,
        // still returned by capture_events (it is still pending).
        let events = store.capture_events(&binding, 512).unwrap();
        assert_eq!(events.len(), 5);
        for (i, e) in events.iter().enumerate() {
            assert_eq!(
                e.source_event_id,
                format!("awili:00000000-0000-4000-8000-{i:012x}")
            );
            assert_eq!(e.ts, 50 + i as i64);
            assert_eq!(e.redacted_content, "redacted");
        }

        // Exposure is stored as never_sent on every row.
        let exposure: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT exposure FROM capture_outbox ORDER BY source_ts ASC")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(exposure.len(), 5);
        assert!(
            exposure.iter().all(|e| e == "never_sent"),
            "migrated rows must be never_sent, got {exposure:?}"
        );

        // The intent table exists and is EMPTY.
        let intents = store.capture_deletion_intents(&binding, 512).unwrap();
        assert!(intents.is_empty(), "migration must create ZERO intent rows");

        // A wrong binding still sees nothing.
        let other = test_binding();
        assert!(store
            .capture_deletion_intents(&other, 512)
            .unwrap()
            .is_empty());
        assert!(store.capture_events(&other, 512).unwrap().is_empty());

        // Reopen: idempotent, still v12, exposure persisted.
        let reopened = Store::open(&path).unwrap();
        let v: i64 = reopened
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "second open must stay at v12");
        assert_eq!(
            reopened.capture_events(&binding, 512).unwrap().len(),
            5,
            "queued rows must survive reopen"
        );
        cleanup(&path);
    }

    #[test]
    fn mark_initial_then_already_marked() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "exposure", 1000);
        let binding = test_binding();
        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&test_input(&binding)))
            .unwrap();
        let source_event_id = receipt.source_event_id.unwrap();

        // Initial mark: never_sent -> possibly_sent.
        let m = store
            .mark_capture_possibly_sent(&binding, &source_event_id)
            .unwrap();
        assert_eq!(m, capture_outbox::MarkExposure::Marked);

        // Repeated mark: idempotent, no new authority, no error.
        let m2 = store
            .mark_capture_possibly_sent(&binding, &source_event_id)
            .unwrap();
        assert_eq!(m2, capture_outbox::MarkExposure::AlreadyMarked);

        // Third time: still AlreadyMarked.
        let m3 = store
            .mark_capture_possibly_sent(&binding, &source_event_id)
            .unwrap();
        assert_eq!(m3, capture_outbox::MarkExposure::AlreadyMarked);

        // capture_events now EXCLUDES the marked row.
        let events = store.capture_events(&binding, 10).unwrap();
        assert!(
            events.is_empty(),
            "marked row must be excluded from pending retrieval"
        );
        cleanup(&path);
    }

    #[test]
    fn mark_binding_isolation_all_five_fields() {
        // A mark (and an intent read) must be exact on ALL FIVE binding
        // components: changing any one alone yields NotFound on the mark and
        // zero intent rows on the read.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "iso2", 1000);
        let binding = test_binding();
        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&test_input(&binding)))
            .unwrap();
        let source_event_id = receipt.source_event_id.unwrap();

        let mut variants = Vec::new();
        let mut b = binding.clone();
        b.epoch = "ep-other".to_string();
        variants.push((b, "epoch"));
        let mut b = binding.clone();
        b.installation_id = "inst-other".to_string();
        variants.push((b, "installation_id"));
        let mut b = binding.clone();
        b.tenant_id = "tenant-other".to_string();
        variants.push((b, "tenant_id"));
        let mut b = binding.clone();
        b.user_id = "user-other".to_string();
        variants.push((b, "user_id"));
        let mut b = binding.clone();
        b.consumer_id = uuid::Uuid::new_v4();
        variants.push((b, "consumer_id"));

        for (variant, field) in &variants {
            // No row under the wrong binding: NotFound, no existence leak,
            // no mutation of the real row.
            let m = store
                .mark_capture_possibly_sent(variant, &source_event_id)
                .unwrap();
            assert_eq!(
                m,
                capture_outbox::MarkExposure::NotFound,
                "wrong {field} alone must be NotFound"
            );
            // And an intent under the wrong binding is invisible.
            assert!(
                store
                    .capture_deletion_intents(variant, 10)
                    .unwrap()
                    .is_empty(),
                "wrong {field} alone must see no intents"
            );
        }

        // The exact original binding still works: Marked on the real row.
        let m = store
            .mark_capture_possibly_sent(&binding, &source_event_id)
            .unwrap();
        assert_eq!(m, capture_outbox::MarkExposure::Marked);
        // And capture_events excludes it (proving the real row was the one
        // marked, not a wrong-binding shadow).
        assert!(store.capture_events(&binding, 10).unwrap().is_empty());
        cleanup(&path);
    }

    #[test]
    fn synthetic_pending_intent_blocks_new_mark_in_same_binding_only() {
        // A pending (synthetic) deletion intent for the SAME full binding
        // pauses NEW exposure marks (BindingPaused), while an UNRELATED
        // binding (one of the five components different) is unaffected.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "paused", 1000);
        let binding = test_binding();
        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&test_input(&binding)))
            .unwrap();
        let source_event_id = receipt.source_event_id.unwrap();

        // Synthetic pending intent for an UNRELATED source under the SAME
        // full binding (1B-a: no cleared status, presence = paused).
        insert_intent(
            &store.conn,
            "awili:pending-0001",
            &convo,
            &binding,
            1,
            2,
            "truncate",
        );

        // Same full binding -> BindingPaused, row stays never_sent.
        let m = store
            .mark_capture_possibly_sent(&binding, &source_event_id)
            .unwrap();
        assert_eq!(m, capture_outbox::MarkExposure::BindingPaused);
        assert_eq!(
            store.capture_events(&binding, 10).unwrap().len(),
            1,
            "paused mark must not mutate the row"
        );

        // An UNRELATED binding (different user_id) -> Marked, unaffected.
        let mut other = binding.clone();
        other.user_id = "user-other".to_string();
        let receipt2 = store
            .append_message_with_capture(&convo, "user", "other", 10, Some(&test_input(&other)))
            .unwrap();
        let source2 = receipt2.source_event_id.unwrap();
        let m2 = store.mark_capture_possibly_sent(&other, &source2).unwrap();
        assert_eq!(m2, capture_outbox::MarkExposure::Marked);
        cleanup(&path);
    }

    #[test]
    fn deleted_source_with_intent_is_never_resurrected() {
        // If a deletion intent exists FOR the source event itself, the mark
        // is refused (NotFound — never resurrect), even when the outbox row
        // still exists and is already possibly_sent.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "deleted", 1000);
        let binding = test_binding();
        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&test_input(&binding)))
            .unwrap();
        let source_event_id = receipt.source_event_id.unwrap();

        // First: no intent yet -> Marked (row is now possibly_sent).
        let m = store
            .mark_capture_possibly_sent(&binding, &source_event_id)
            .unwrap();
        assert_eq!(m, capture_outbox::MarkExposure::Marked);

        // Now a deletion intent for THIS source appears (as 1B-b will
        // create on delete/truncate). The source is deleted: the mark must
        // be refused — NotFound, never resurrected — and the row is NOT
        // mutated further.
        insert_intent(
            &store.conn,
            &source_event_id,
            &convo,
            &binding,
            99,
            100,
            "conversation_delete",
        );
        let m2 = store
            .mark_capture_possibly_sent(&binding, &source_event_id)
            .unwrap();
        assert_eq!(
            m2,
            capture_outbox::MarkExposure::NotFound,
            "a deleted source must never be resurrected"
        );

        // Third call: still refused.
        let m3 = store
            .mark_capture_possibly_sent(&binding, &source_event_id)
            .unwrap();
        assert_eq!(m3, capture_outbox::MarkExposure::NotFound);
        cleanup(&path);
    }

    #[test]
    fn mark_unknown_source_is_not_found() {
        // A source_event_id with no row under this full binding -> NotFound
        // (identical to the wrong-binding / deleted cases: no existence
        // leak, no mutation).
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let _convo = new_conv(&store, "unknown", 1000);
        let binding = test_binding();
        let m = store
            .mark_capture_possibly_sent(&binding, "awili:does-not-exist")
            .unwrap();
        assert_eq!(m, capture_outbox::MarkExposure::NotFound);
        // Nothing to retrieve, no intents.
        assert!(store.capture_events(&binding, 10).unwrap().is_empty());
        assert!(store
            .capture_deletion_intents(&binding, 10)
            .unwrap()
            .is_empty());
        cleanup(&path);
    }

    #[test]
    fn mark_intent_read_failure_is_err_and_exposure_unchanged() {
        // NEGATIVE (fail-open regression): if the deletion-intent read
        // FAILS (here the intents table is missing), mark must return Err
        // — never a coerced "no intent" — and no exposure mutation may be
        // committed: an independent read of the outbox row still sees
        // literal `never_sent`.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "neg1", 1000);
        let binding = test_binding();
        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&test_input(&binding)))
            .unwrap();
        let source_event_id = receipt.source_event_id.unwrap();

        // Force the intent read to fail: the table does not exist.
        store
            .conn
            .execute_batch("DROP TABLE capture_deletion_intents;")
            .unwrap();

        let m = store.mark_capture_possibly_sent(&binding, &source_event_id);
        assert!(
            m.is_err(),
            "a failed intent read must be an Err, never coerced into a result"
        );

        // Independent assertion: the exposure column was NOT mutated.
        let exposure: String = store
            .conn
            .query_row(
                "SELECT exposure FROM capture_outbox WHERE source_event_id = ?1",
                rusqlite::params![source_event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            exposure, "never_sent",
            "no exposure mutation may be committed when a read fails"
        );
        cleanup(&path);
    }

    #[test]
    fn mark_outbox_lookup_failure_is_err_not_not_found() {
        // NEGATIVE: if the INITIAL capture_outbox lookup itself fails (here
        // the table is missing), mark must return Err — NOT NotFound.
        // NotFound is reserved for a literal no-row result of a successful
        // read.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "neg2", 1000);
        let binding = test_binding();
        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&test_input(&binding)))
            .unwrap();
        let source_event_id = receipt.source_event_id.unwrap();

        // Force the initial outbox read to fail: the table does not exist.
        store
            .conn
            .execute_batch("DROP TABLE capture_outbox;")
            .unwrap();

        let m = store.mark_capture_possibly_sent(&binding, &source_event_id);
        assert!(
            m.is_err(),
            "a failed outbox lookup must be an Err, not NotFound"
        );
        cleanup(&path);
    }

    #[test]
    fn mark_unknown_exposure_is_err_and_no_mutation() {
        // NEGATIVE: an exposure value that is neither of the two legal
        // literals is corrupt state. The CHECK in the shipped schema
        // forbids it; this test simulates a corrupted file by inserting
        // directly under `PRAGMA ignore_check_constraints` (a test-only
        // fixture; the shipped DDL still enforces the two legal values).
        // mark must return Err and commit NO mutation.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "neg3", 1000);
        let binding = test_binding();
        let receipt = store
            .append_message_with_capture(&convo, "user", "hello", 0, Some(&test_input(&binding)))
            .unwrap();
        let source_event_id = receipt.source_event_id.unwrap();

        store
            .conn
            .pragma_update(None, "ignore_check_constraints", 1)
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE capture_outbox SET exposure = 'bogus' WHERE source_event_id = ?1",
                rusqlite::params![source_event_id],
            )
            .unwrap();
        store
            .conn
            .pragma_update(None, "ignore_check_constraints", 0)
            .unwrap();

        let m = store.mark_capture_possibly_sent(&binding, &source_event_id);
        assert!(
            m.is_err(),
            "an unknown exposure value must be an Err, never permission to mark"
        );

        // Independent assertion: the value is untouched (no write-through).
        let exposure: String = store
            .conn
            .query_row(
                "SELECT exposure FROM capture_outbox WHERE source_event_id = ?1",
                rusqlite::params![source_event_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            exposure, "bogus",
            "no mutation may be committed on unknown exposure"
        );
        cleanup(&path);
    }

    #[test]
    fn intent_retrieval_clamps_and_orders() {
        // capture_deletion_intents: 0 -> empty, 1 -> the single earliest
        // row, and a huge limit over >512 REAL intent rows clamps to 512.
        // Order is requested_ts ASC, then source_event_id ASC.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "intents", 1000);
        let binding = test_binding();

        // Zero intents -> empty.
        assert!(store
            .capture_deletion_intents(&binding, 10)
            .unwrap()
            .is_empty());
        // limit 0 -> empty even with rows present (inserted below).
        assert!(store
            .capture_deletion_intents(&binding, 0)
            .unwrap()
            .is_empty());

        // Insert 600 REAL intent rows (a mix of cause and ts).
        for i in 0..600i64 {
            let source_event_id = format!("awili:{i:032}");
            let requested_ts = i % 7; // repeats to test tie-breaking
            let cause = if i % 2 == 0 {
                "truncate"
            } else {
                "conversation_delete"
            };
            // conversation_id is deliberately DISTINCT per row (below) so
            // the retrieval equality can verify full ID preservation.
            // The fixture conversation id is reused for every row of this
            // binding; the full-ID-preservation assertion uses the distinct
            // per-row ids in intent_retrieval_full_fields_and_stable_order.
            insert_intent(
                &store.conn,
                &source_event_id,
                &convo,
                &binding,
                i,
                requested_ts,
                cause,
            );
        }

        // limit 1 -> exactly the earliest row (requested_ts, then id).
        let one = store.capture_deletion_intents(&binding, 1).unwrap();
        assert_eq!(one.len(), 1);

        // usize::MAX over >512 real rows -> clamped to 512.
        let all = store
            .capture_deletion_intents(&binding, usize::MAX)
            .unwrap();
        assert_eq!(all.len(), 512, "usize::MAX must clamp to the 512 bound");

        // Stable order: requested_ts ASC, then source_event_id ASC.
        for w in all.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            assert!(
                (a.requested_ts, &a.source_event_id) < (b.requested_ts, &b.source_event_id),
                "order violated: {:?} >= {:?}",
                (a.requested_ts, &a.source_event_id),
                (b.requested_ts, &b.source_event_id)
            );
        }

        // conversation_id filter-result equality: every row in the
        // retrieved set carries the exact conversation id that was
        // inserted, and the returned set equals the inserted set.
        assert!(
            all.iter().all(|i| i.conversation_id == convo),
            "every returned row must carry the exact conversation id that was inserted"
        );
        let retrieved_ids: std::collections::BTreeSet<String> =
            all.iter().map(|i| i.source_event_id.clone()).collect();
        // The result is clamped to 512 rows: the expected set is the FIRST
        // 512 rows in stable order (requested_ts ASC, then
        // source_event_id ASC), not all 600 inserted.
        let mut expected: Vec<String> = (0..600i64).map(|i| format!("awili:{i:032}")).collect();
        expected.sort_by_key(|id| {
            let i: i64 = id[6..].parse().unwrap();
            (i % 7, id.clone())
        });
        let expected_ids: std::collections::BTreeSet<String> =
            expected.iter().take(512).cloned().collect();
        assert_eq!(
            retrieved_ids, expected_ids,
            "retrieved source_event_id set must exactly equal the first 512 inserted rows in stable order (clamped, ordered, no loss)"
        );
        cleanup(&path);
    }

    #[test]
    fn intent_retrieval_full_fields_and_stable_order() {
        // Every field of a StoredDeletionIntent round-trips exactly, two
        // rows with the SAME requested_ts are ordered by source_event_id,
        // and the FULL conversation ID is preserved exactly.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let _convo = new_conv(&store, "intents2", 1000);
        let binding = test_binding();

        // Two rows with DISTINCT, full-length conversation IDs.
        let convo_a = uuid::Uuid::new_v4().to_string();
        let convo_b = uuid::Uuid::new_v4().to_string();
        insert_intent(
            &store.conn,
            "awili:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            &convo_b,
            &binding,
            11,
            50,
            "truncate",
        );
        insert_intent(
            &store.conn,
            "awili:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            &convo_a,
            &binding,
            10,
            50,
            "conversation_delete",
        );

        let intents = store.capture_deletion_intents(&binding, 10).unwrap();
        assert_eq!(intents.len(), 2);
        // Same requested_ts (50): ordered by source_event_id -> 'a…' first.
        assert_eq!(
            intents[0].source_event_id,
            "awili:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        );
        assert_eq!(
            intents[1].source_event_id,
            "awili:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        );

        // Full-field equality on both rows, INCLUDING the conversation ID.
        let expected = [
            capture_outbox::StoredDeletionIntent {
                source_event_id: "awili:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
                binding: binding.clone(),
                conversation_id: convo_a.clone(),
                source_ts: 10,
                requested_ts: 50,
                cause: "conversation_delete".to_string(),
            },
            capture_outbox::StoredDeletionIntent {
                source_event_id: "awili:bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_string(),
                binding: binding.clone(),
                conversation_id: convo_b.clone(),
                source_ts: 11,
                requested_ts: 50,
                cause: "truncate".to_string(),
            },
        ];
        assert_eq!(intents, expected, "full intent fields must round-trip");

        // Explicit full conversation-ID preservation: the exact full UUID
        // string per source event id, no truncation or reordering.
        assert_eq!(
            intents[0].conversation_id, convo_a,
            "full conversation ID must be preserved"
        );
        assert_eq!(
            intents[1].conversation_id, convo_b,
            "full conversation ID must be preserved"
        );

        // Filter-result equality: a second, unrelated conversation's
        // intents must NOT leak into this binding's result, and the
        // returned set equals exactly the two inserted rows.
        let convo_c = uuid::Uuid::new_v4().to_string();
        let other_binding = {
            let mut b = binding.clone();
            b.consumer_id = uuid::Uuid::new_v4();
            b
        };
        insert_intent(
            &store.conn,
            "awili:cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            &convo_c,
            &other_binding,
            20,
            50,
            "truncate",
        );
        let rerun = store.capture_deletion_intents(&binding, 10).unwrap();
        assert_eq!(
            rerun.len(),
            2,
            "filter must return exactly the matching binding's rows"
        );
        assert_eq!(
            rerun, expected,
            "re-filtered result must equal the original insert set"
        );
        let ids: Vec<&str> = rerun.iter().map(|i| i.conversation_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![convo_a.as_str(), convo_b.as_str()],
            "conversation IDs must equal the inserted set"
        );

        // Wrong binding (one component different) -> zero rows.
        let mut other = binding.clone();
        other.tenant_id = "tenant-other".to_string();
        assert!(store
            .capture_deletion_intents(&other, 10)
            .unwrap()
            .is_empty());
        cleanup(&path);
    }

    #[test]
    fn reopen_preserves_exposure_and_capture_events_excludes_marked() {
        // Exposure is durable: after a reopen the marked row is still
        // possibly_sent and capture_events still excludes it, while an
        // unmarked row is still returned.
        let path = temp_db();
        let (binding, source_event_id) = {
            let store = Store::open(&path).unwrap();
            let convo = new_conv(&store, "dur", 1000);
            let b = test_binding();
            let r = store
                .append_message_with_capture(&convo, "user", "marked", 0, Some(&test_input(&b)))
                .unwrap();
            // Mark this one.
            store
                .mark_capture_possibly_sent(&b, &r.source_event_id.unwrap())
                .unwrap();
            // Add a SECOND, unmarked row under the same binding.
            let r2 = store
                .append_message_with_capture(&convo, "user", "unmarked", 10, Some(&test_input(&b)))
                .unwrap();
            (b, r2.source_event_id.unwrap())
        };

        let store = Store::open(&path).unwrap();
        // Exactly the unmarked row is returned; the marked one is excluded.
        let events = store.capture_events(&binding, 10).unwrap();
        assert_eq!(events.len(), 1, "only the unmarked row must survive");
        assert_eq!(events[0].source_event_id, source_event_id);

        // The marked row is still possibly_sent in the DB (durable), and the
        // surviving row is still never_sent.
        let (marked_exposure, unmarked_exposure): (String, String) = store
            .conn
            .query_row(
                "SELECT (SELECT exposure FROM capture_outbox WHERE source_event_id != ?1),
                        (SELECT exposure FROM capture_outbox WHERE source_event_id = ?1)",
                rusqlite::params![source_event_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            marked_exposure, "possibly_sent",
            "marked row must stay possibly_sent across reopen"
        );
        assert_eq!(
            unmarked_exposure, "never_sent",
            "unmarked row must stay never_sent across reopen"
        );
        cleanup(&path);
    }

    #[test]
    fn intent_schema_has_no_payload_columns() {
        // capture_deletion_intents must carry ONLY identity/metadata: no
        // payload, content, hash, token, or raw reference column.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let cols: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("PRAGMA table_info(capture_deletion_intents)")
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let expected = [
            "source_event_id",
            "binding_epoch",
            "installation_id",
            "tenant_id",
            "user_id",
            "consumer_id",
            "conversation_id",
            "source_ts",
            "requested_ts",
            "cause",
        ];
        assert_eq!(
            cols,
            expected.to_vec(),
            "intent schema must be exactly the identity/metadata columns"
        );
        // Explicitly: none of the payload-like names are present.
        for forbidden in ["payload", "content", "hash", "token", "raw", "body", "text"] {
            assert!(
                !cols.iter().any(|c| c.contains(forbidden)),
                "intent schema must not contain a '{forbidden}' column"
            );
        }
        cleanup(&path);
    }

    // ── 1B-b: atomic local deletion bookkeeping ──────────────────────────────
    // The shared private helper `bookkeeping_delete_outbox` is exercised
    // through both public entry points: `truncate_messages` (cause
    // "truncate") and `delete_conversation` (cause "conversation_delete").

    /// Append N capture messages to a conversation with strictly increasing
    /// ts, return the list of `source_event_id`s.
    fn seed_capture_messages(
        store: &Store,
        convo: &str,
        binding: &capture_outbox::OfflineCaptureBinding,
        n: i64,
        base_ts: i64,
    ) -> Vec<String> {
        (0..n)
            .map(|i| {
                let receipt = store
                    .append_message_with_capture(
                        convo,
                        "user",
                        &format!("msg {i}"),
                        base_ts + i,
                        Some(&test_input(binding)),
                    )
                    .unwrap();
                receipt.source_event_id.unwrap()
            })
            .collect()
    }

    #[test]
    fn truncate_mixed_exposures_across_bindings() {
        // Two bindings, mixed never_sent / possibly_sent across both; the
        // retained prefix has its own mixed rows. Truncate keeps 3 of 6.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "mixed", 1000);
        let b1 = test_binding();
        let b2 = test_binding();

        // 6 messages, ts 0..9 (restore order = insertion order).
        // First 3 under b1 (ts 0,1,2), next 3 under b2 (ts 10,11,12).
        let srcs1: Vec<String> = seed_capture_messages(&store, &convo, &b1, 3, 0);
        store.mark_capture_possibly_sent(&b1, &srcs1[0]).unwrap();
        store.mark_capture_possibly_sent(&b1, &srcs1[2]).unwrap();
        let srcs2: Vec<String> = seed_capture_messages(&store, &convo, &b2, 3, 10);
        store.mark_capture_possibly_sent(&b2, &srcs2[1]).unwrap();

        // keep = 3 → victims are the last 3 appends (all binding b2).
        let deleted = store.truncate_messages(&convo, 3).unwrap();
        assert_eq!(deleted, 3);

        // b1: srcs1[0],srcs1[2] possibly_sent, retained → outbox rows KEPT.
        //     srcs1[1] never_sent, retained → still pending.
        // b2: all 3 rows are victims. srcs2[1] possibly_sent → intent;
        //     srcs2[0],srcs2[2] never_sent → rows deleted.
        // Outbox: exactly the 3 retained rows remain.
        let outbox: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT source_event_id FROM capture_outbox WHERE conversation_id = ?1")
                .unwrap();
            stmt.query_map(rusqlite::params![convo], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(outbox.len(), 3);
        assert!(outbox.contains(&srcs1[0]));
        assert!(outbox.contains(&srcs1[1]));
        assert!(outbox.contains(&srcs1[2]));

        let b1_events = store.capture_events(&b1, 50).unwrap();
        assert_eq!(b1_events.len(), 1);
        assert_eq!(b1_events[0].source_event_id, srcs1[1]);

        // Exactly one intent: srcs2[1] with cause "truncate".
        let intents = store.capture_deletion_intents(&b2, 50).unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].source_event_id, srcs2[1]);
        assert_eq!(intents[0].cause, "truncate");
        assert_eq!(intents[0].conversation_id, convo);
        assert_eq!(intents[0].source_ts, 11); // base_ts 10 + index 1

        cleanup(&path);
    }

    #[test]
    fn delete_conversation_mixed_exposures() {
        // Whole-conversation delete: victims are ALL messages. Both
        // bindings have mixed never_sent / possibly_sent rows.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "del-mixed", 1000);
        let b1 = test_binding();
        let b2 = test_binding();

        let srcs1: Vec<String> = seed_capture_messages(&store, &convo, &b1, 3, 0);
        store.mark_capture_possibly_sent(&b1, &srcs1[1]).unwrap();
        let srcs2: Vec<String> = seed_capture_messages(&store, &convo, &b2, 3, 10);
        store.mark_capture_possibly_sent(&b2, &srcs2[0]).unwrap();
        store.mark_capture_possibly_sent(&b2, &srcs2[2]).unwrap();

        store.delete_conversation(&convo).unwrap();

        // Conversation row is gone.
        assert!(store.conversations().unwrap().iter().all(|c| c.id != convo));

        // Messages gone.
        assert!(store.messages(&convo).unwrap().is_empty());

        // Outbox: ALL 6 victim rows are deleted locally (possibly_sent
        // victims become intent rows with no payload; never_sent victims
        // are simply gone). Nothing remains for this conversation.
        let outbox: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT source_event_id FROM capture_outbox WHERE conversation_id = ?1")
                .unwrap();
            stmt.query_map(rusqlite::params![convo], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert!(outbox.is_empty());

        // Intents: only the possibly_sent victims.
        // b1: srcs1[1] possibly_sent (intent).
        // b2: srcs2[0], srcs2[2] possibly_sent (intents).
        assert!(store.capture_events(&b1, 50).unwrap().is_empty());
        assert!(store.capture_events(&b2, 50).unwrap().is_empty());

        let b1_intents = store.capture_deletion_intents(&b1, 50).unwrap();
        assert_eq!(b1_intents.len(), 1);
        assert_eq!(b1_intents[0].source_event_id, srcs1[1]);
        assert_eq!(b1_intents[0].cause, "conversation_delete");
        assert_eq!(b1_intents[0].conversation_id, convo);

        let b2_intents = store.capture_deletion_intents(&b2, 50).unwrap();
        assert_eq!(b2_intents.len(), 2);
        let b2_intent_ids: std::collections::HashSet<&str> = b2_intents
            .iter()
            .map(|i| i.source_event_id.as_str())
            .collect();
        assert!(b2_intent_ids.contains(srcs2[0].as_str()));
        assert!(b2_intent_ids.contains(srcs2[2].as_str()));
        for i in &b2_intents {
            assert_eq!(i.cause, "conversation_delete");
        }

        cleanup(&path);
    }

    #[test]
    fn truncate_exact_restore_ordering_with_ts_ties() {
        // 6 messages with tied ts values to verify restore order is
        // `ts ASC, id ASC` (id breaks the tie, not ts alone).
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "ties", 1000);
        let binding = test_binding();

        // Insert with deliberately tied ts values:
        // id 1 → ts 5, id 2 → ts 3, id 3 → ts 3,
        // id 4 → ts 7, id 5 → ts 7, id 6 → ts 1.
        // Restore order: ts=1 (id6), ts=3 (id2), ts=3 (id3),
        //               ts=5 (id1), ts=7 (id4), ts=7 (id5).
        let srcs: Vec<String> = (0..6)
            .map(|i| {
                let ts = [5i64, 3, 3, 7, 7, 1][i as usize];
                let receipt = store
                    .append_message_with_capture(
                        &convo,
                        "user",
                        &format!("msg {i}"),
                        ts,
                        Some(&test_input(&binding)),
                    )
                    .unwrap();
                receipt.source_event_id.unwrap()
            })
            .collect();

        // Mark all 6 possibly_sent so every victim gets an intent.
        for s in &srcs {
            store.mark_capture_possibly_sent(&binding, s).unwrap();
        }

        // keep = 4 → victims are the last 2 in restore order:
        // ts=7 (id4, srcs[3]) and ts=7 (id5, srcs[4]).
        let deleted = store.truncate_messages(&convo, 4).unwrap();
        assert_eq!(deleted, 2);

        // Remaining messages: 4 (the first 4 in restore order).
        let msgs = store.messages(&convo).unwrap();
        assert_eq!(msgs.len(), 4);
        // Restore order: ts=1 (srcs[5]), ts=3 (srcs[1]), ts=3 (srcs[2]),
        //                ts=5 (srcs[0]).
        assert_eq!(msgs[0].ts, 1);
        assert_eq!(msgs[1].ts, 3);
        assert_eq!(msgs[2].ts, 3);
        assert_eq!(msgs[3].ts, 5);

        // Intents: exactly the 2 victim sources (srcs[3], srcs[4]).
        // (Order is `requested_ts ASC, source_event_id ASC`; same
        // requested_ts → lexicographic UUID order, not insertion order.)
        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(intents.len(), 2);
        let intent_ids: std::collections::HashSet<&str> =
            intents.iter().map(|i| i.source_event_id.as_str()).collect();
        assert!(intent_ids.contains(srcs[3].as_str()));
        assert!(intent_ids.contains(srcs[4].as_str()));
        for i in &intents {
            assert_eq!(i.source_ts, 7);
            assert_eq!(i.cause, "truncate");
        }

        // Outbox: the 4 retained (possibly_sent) rows remain; the 2 victim
        // rows (srcs[3], srcs[4]) are deleted.
        let outbox: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT source_event_id FROM capture_outbox WHERE conversation_id = ?1")
                .unwrap();
            stmt.query_map(rusqlite::params![convo], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(outbox.len(), 4);
        assert!(!outbox.contains(&srcs[3]));
        assert!(!outbox.contains(&srcs[4]));

        cleanup(&path);
    }

    #[test]
    fn truncate_keep_zero_and_keep_all_and_max() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "keep", 1000);
        let binding = test_binding();

        let srcs: Vec<String> = seed_capture_messages(&store, &convo, &binding, 4, 100);
        for s in &srcs {
            store.mark_capture_possibly_sent(&binding, s).unwrap();
        }

        // keep = usize::MAX → no-op (no victims, no intents).
        let n = store.truncate_messages(&convo, usize::MAX).unwrap();
        assert_eq!(n, 0);
        assert_eq!(store.messages(&convo).unwrap().len(), 4);
        // All 4 possibly_sent outbox rows remain (no-op).
        let outbox_max: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT source_event_id FROM capture_outbox WHERE conversation_id = ?1")
                .unwrap();
            stmt.query_map(rusqlite::params![convo], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(outbox_max.len(), 4);
        assert!(store
            .capture_deletion_intents(&binding, 50)
            .unwrap()
            .is_empty());

        // keep = 0 → all 4 are victims.
        let n = store.truncate_messages(&convo, 0).unwrap();
        assert_eq!(n, 4);
        assert!(store.messages(&convo).unwrap().is_empty());
        // All 4 victim possibly_sent outbox rows are DELETED; the 4 intent
        // rows are the complete local record.
        let outbox_zero: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT source_event_id FROM capture_outbox WHERE conversation_id = ?1")
                .unwrap();
            stmt.query_map(rusqlite::params![convo], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert!(outbox_zero.is_empty());
        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(intents.len(), 4);
        let intent_ids: std::collections::HashSet<&str> =
            intents.iter().map(|i| i.source_event_id.as_str()).collect();
        for s in &srcs {
            assert!(intent_ids.contains(s.as_str()));
        }

        // keep = all (4) on now-empty conversation → no-op.
        let n = store.truncate_messages(&convo, 4).unwrap();
        assert_eq!(n, 0);

        cleanup(&path);
    }

    #[test]
    fn truncate_retained_prefix_untouched() {
        // Retained prefix messages keep their outbox rows (both
        // never_sent and possibly_sent). Victims' outbox rows are
        // deleted/intent-ified; the prefix is never touched.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "retain", 1000);
        let binding = test_binding();

        let srcs: Vec<String> = seed_capture_messages(&store, &convo, &binding, 5, 0);
        // Make the first 2 (retained) and the last 3 (victims) possibly_sent.
        for s in &srcs {
            store.mark_capture_possibly_sent(&binding, s).unwrap();
        }

        let deleted = store.truncate_messages(&convo, 2).unwrap();
        assert_eq!(deleted, 3);

        // Retained prefix: 2 messages remain.
        assert_eq!(store.messages(&convo).unwrap().len(), 2);

        // Outbox: only the 2 retained (possibly_sent) rows remain. The 3
        // victim possibly_sent rows (srcs[2..4]) are DELETED — their intent
        // rows are the complete local record.
        let outbox: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT source_event_id FROM capture_outbox WHERE conversation_id = ?1")
                .unwrap();
            stmt.query_map(rusqlite::params![convo], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(outbox.len(), 2);
        assert!(outbox.contains(&srcs[0]));
        assert!(outbox.contains(&srcs[1]));

        // Pending events: all 5 are possibly_sent → excluded from pending.
        assert!(store.capture_events(&binding, 50).unwrap().is_empty());

        // Intents: exactly the 3 victim possibly_sent sources.
        // srcs[0],srcs[1] are retained → NO intents for them.
        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(intents.len(), 3);
        let intent_ids: std::collections::HashSet<&str> =
            intents.iter().map(|i| i.source_event_id.as_str()).collect();
        assert!(intent_ids.contains(srcs[2].as_str()));
        assert!(intent_ids.contains(srcs[3].as_str()));
        assert!(intent_ids.contains(srcs[4].as_str()));
        for i in &intents {
            assert_eq!(i.cause, "truncate");
        }

        cleanup(&path);
    }

    #[test]
    fn truncate_victims_and_payloads_gone() {
        // After truncation, victim message rows are gone and their
        // outbox payloads (redacted_content) are removed from local
        // storage (possibly_sent victims become intent rows with no content).
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "payloads", 1000);
        let binding = test_binding();

        let srcs: Vec<String> = seed_capture_messages(&store, &convo, &binding, 4, 0);
        for s in &srcs {
            store.mark_capture_possibly_sent(&binding, s).unwrap();
        }

        store.truncate_messages(&convo, 2).unwrap();

        // Messages: only first 2 remain.
        let msgs = store.messages(&convo).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text, "msg 0");
        assert_eq!(msgs[1].text, "msg 1");

        // Outbox: only srcs[0],srcs[1] remain (retained prefix,
        // possibly_sent). The victim rows srcs[2],srcs[3] are DELETED
        // locally — their deletion-intent rows are the complete local
        // record (no payload, no content).
        let remaining: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT source_event_id FROM capture_outbox WHERE conversation_id = ?1")
                .unwrap();
            stmt.query_map(rusqlite::params![convo], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&srcs[0]));
        assert!(remaining.contains(&srcs[1]));
        assert!(!remaining.contains(&srcs[2]));
        assert!(!remaining.contains(&srcs[3]));

        // Intents: exactly the 2 victim possibly_sent sources.
        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(intents.len(), 2);
        let intent_ids: std::collections::HashSet<&str> =
            intents.iter().map(|i| i.source_event_id.as_str()).collect();
        assert!(intent_ids.contains(srcs[2].as_str()));
        assert!(intent_ids.contains(srcs[3].as_str()));
        for i in &intents {
            assert_eq!(i.cause, "truncate");
            assert_eq!(i.conversation_id, convo);
        }

        cleanup(&path);
    }

    #[test]
    fn delete_conversation_usage_and_staged_attachments_unchanged() {
        // Deleting a conversation also removes its usage_events and
        // staged_attachments rows. This test verifies that side-effect
        // behaviour is preserved (the bookkeeping does not interfere).
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "usage", 1000);
        let binding = test_binding();

        let srcs: Vec<String> = seed_capture_messages(&store, &convo, &binding, 1, 0);
        store
            .mark_capture_possibly_sent(&binding, &srcs[0])
            .unwrap();

        let att = store
            .store_attachment("a.txt", "text/plain", b"blob")
            .unwrap();
        store.stage_attachment(&convo, att).unwrap();

        store
            .record_usage(&convo, 100, "test-provider", "test-model", 10, 20)
            .unwrap();

        store.delete_conversation(&convo).unwrap();

        // Usage events for this conversation are gone.
        let usage = store.daily_usage_by_model(100).unwrap();
        assert!(usage.is_empty());

        // Staged attachments gone.
        assert!(store.get_staged_attachments(&convo).unwrap().is_empty());

        // Intent for the possibly_sent source exists.
        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].source_event_id, srcs[0]);

        cleanup(&path);
    }

    #[test]
    fn repeat_delete_conversation_is_idempotent() {
        // Deleting an already-deleted conversation is a no-op: no error,
        // no victims, no new intents.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "idem", 1000);
        let binding = test_binding();

        let srcs: Vec<String> = seed_capture_messages(&store, &convo, &binding, 2, 0);
        store
            .mark_capture_possibly_sent(&binding, &srcs[0])
            .unwrap();

        store.delete_conversation(&convo).unwrap();
        store.delete_conversation(&convo).unwrap();

        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].source_event_id, srcs[0]);

        cleanup(&path);
    }

    #[test]
    fn truncate_repeat_is_idempotent() {
        // Truncating an already-truncated conversation is a no-op.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "trunc-idem", 1000);
        let binding = test_binding();

        let srcs: Vec<String> = seed_capture_messages(&store, &convo, &binding, 3, 0);
        for s in &srcs {
            store.mark_capture_possibly_sent(&binding, s).unwrap();
        }

        store.truncate_messages(&convo, 1).unwrap();
        let n = store.truncate_messages(&convo, 1).unwrap();
        assert_eq!(n, 0);

        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(intents.len(), 2);

        cleanup(&path);
    }

    #[test]
    fn matching_pre_existing_intent_unchanged() {
        // A matching pre-existing intent (same binding, conversation,
        // source_ts) is left UNCHANGED by a second delete.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "pre-exist", 1000);
        let binding = test_binding();

        let srcs: Vec<String> = seed_capture_messages(&store, &convo, &binding, 1, 50);
        store
            .mark_capture_possibly_sent(&binding, &srcs[0])
            .unwrap();

        // Pre-seed a matching intent (same binding, convo, source_ts=50).
        insert_intent(&store.conn, &srcs[0], &convo, &binding, 50, 999, "truncate");

        let before = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].requested_ts, 999);

        store.delete_conversation(&convo).unwrap();

        let after = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].requested_ts, 999,
            "pre-existing matching intent must be unchanged"
        );
        assert_eq!(after[0].cause, "truncate");

        cleanup(&path);
    }

    #[test]
    fn conflicting_pre_existing_intent_is_err_and_local_state_intact() {
        // A pre-existing intent that MISMATCHES the outbox row (different
        // binding) → Err + full rollback. Local state is fully intact.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "conflict", 1000);
        let binding = test_binding();
        let other_binding = test_binding();

        let srcs: Vec<String> = seed_capture_messages(&store, &convo, &binding, 1, 50);
        store
            .mark_capture_possibly_sent(&binding, &srcs[0])
            .unwrap();

        // Pre-seed a CONFLICTING intent: same source_event_id but wrong binding.
        insert_intent(
            &store.conn,
            &srcs[0],
            &convo,
            &other_binding,
            50,
            999,
            "truncate",
        );

        let result = store.delete_conversation(&convo);
        assert!(
            result.is_err(),
            "conflicting pre-existing intent must cause Err"
        );

        // Full rollback: conversation, message, outbox row all intact.
        assert!(store.conversations().unwrap().iter().any(|c| c.id == convo));
        assert_eq!(store.messages(&convo).unwrap().len(), 1);
        let exposure: String = store
            .conn
            .query_row(
                "SELECT exposure FROM capture_outbox WHERE source_event_id = ?1",
                rusqlite::params![srcs[0]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exposure, "possibly_sent");

        // The conflicting intent is still there (not deleted, not replaced).
        let intents = store.capture_deletion_intents(&other_binding, 50).unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].source_event_id, srcs[0]);

        cleanup(&path);
    }

    #[test]
    fn trigger_forced_intent_insert_failure_rolls_back_then_succeeds() {
        // Fault injection: a BEFORE INSERT trigger on
        // capture_deletion_intents RAISEs ABORT. The helper's insert fails
        // → the whole transaction rolls back. All local state is intact.
        // After dropping the trigger, the same call succeeds.
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "injected-fail", 1000);
        let binding = test_binding();

        let srcs: Vec<String> = seed_capture_messages(&store, &convo, &binding, 2, 0);
        store
            .mark_capture_possibly_sent(&binding, &srcs[0])
            .unwrap();
        store
            .mark_capture_possibly_sent(&binding, &srcs[1])
            .unwrap();

        store
            .record_usage(&convo, 200, "prov", "model", 5, 5)
            .unwrap();
        let att = store.store_attachment("x.txt", "text/plain", b"x").unwrap();
        store.stage_attachment(&convo, att).unwrap();

        store
            .conn
            .execute_batch(
                "CREATE TRIGGER injected_fail_insert
                 BEFORE INSERT ON capture_deletion_intents
                 BEGIN
                     SELECT RAISE(ABORT, 'injected');
                 END;",
            )
            .unwrap();

        let result = store.delete_conversation(&convo);
        assert!(result.is_err(), "injected trigger failure must cause Err");

        // Rollback: everything intact.
        assert!(store.conversations().unwrap().iter().any(|c| c.id == convo));
        assert_eq!(store.messages(&convo).unwrap().len(), 2);
        let usage = store.daily_usage_by_model(200).unwrap();
        assert!(usage
            .iter()
            .any(|u| u.provider == "prov" && u.model == "model"));
        assert_eq!(store.get_staged_attachments(&convo).unwrap().len(), 1);
        assert!(store
            .capture_deletion_intents(&binding, 50)
            .unwrap()
            .is_empty());

        // Drop the trigger; the same call now succeeds.
        store
            .conn
            .execute_batch("DROP TRIGGER injected_fail_insert;")
            .unwrap();
        store.delete_conversation(&convo).unwrap();

        assert!(store.conversations().unwrap().iter().all(|c| c.id != convo));
        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(intents.len(), 2);

        cleanup(&path);
    }

    #[test]
    fn wrong_conversation_same_message_id_outbox_not_removed() {
        // Two conversations share a binding. Deleting conversation A must
        // NOT remove conversation B's outbox row even if B's message has
        // the same rowid as A's deleted message (rowid-reuse trap).
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let binding = test_binding();

        let convo_a = new_conv(&store, "A", 1000);
        let convo_b = new_conv(&store, "B", 1000);

        let ra = store
            .append_message_with_capture(&convo_a, "user", "A msg", 0, Some(&test_input(&binding)))
            .unwrap();
        let a_src = ra.source_event_id.unwrap();
        store.mark_capture_possibly_sent(&binding, &a_src).unwrap();

        let rb = store
            .append_message_with_capture(&convo_b, "user", "B msg", 1, Some(&test_input(&binding)))
            .unwrap();
        let b_src = rb.source_event_id.unwrap();
        store.mark_capture_possibly_sent(&binding, &b_src).unwrap();

        // Delete A: victims = A's message only. B's outbox row is NOT
        // touched (the helper filters by conversation_id in SQL).
        store.delete_conversation(&convo_a).unwrap();

        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        let a_intent = intents.iter().find(|i| i.source_event_id == a_src);
        assert!(
            a_intent.is_some(),
            "A's possibly_sent victim must have an intent"
        );
        let b_intent = intents.iter().find(|i| i.source_event_id == b_src);
        assert!(
            b_intent.is_none(),
            "B's outbox row must NOT produce an intent"
        );

        // B's outbox row is still present.
        let b_exposure: String = store
            .conn
            .query_row(
                "SELECT exposure FROM capture_outbox WHERE source_event_id = ?1",
                rusqlite::params![b_src],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(b_exposure, "possibly_sent");

        cleanup(&path);
    }

    #[test]
    fn reused_rowid_new_source_event_id_cannot_alter_prior_intent() {
        // After deleting a conversation, a new message in a different
        // conversation gets a new source_event_id. Deleting the new
        // conversation must not alter the prior intent (different
        // source_event_id → the helper's per-source comparison leaves
        // the old intent untouched).
        //
        // Two distinct bindings are used so that the second
        // mark_capture_possibly_sent is not paused by the first
        // conversation's pre-existing intent (same-binding pause rule).
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let b1 = test_binding();
        let b2 = test_binding();

        let c1 = new_conv(&store, "C1", 1000);
        let r1 = store
            .append_message_with_capture(&c1, "user", "c1 msg", 0, Some(&test_input(&b1)))
            .unwrap();
        let c1_src = r1.source_event_id.unwrap();
        store.mark_capture_possibly_sent(&b1, &c1_src).unwrap();

        store.delete_conversation(&c1).unwrap();
        let intents_before = store.capture_deletion_intents(&b1, 50).unwrap();
        assert_eq!(intents_before.len(), 1);
        assert_eq!(intents_before[0].source_event_id, c1_src);
        let c1_requested_ts = intents_before[0].requested_ts;

        let c2 = new_conv(&store, "C2", 2000);
        let r2 = store
            .append_message_with_capture(&c2, "user", "c2 msg", 1, Some(&test_input(&b2)))
            .unwrap();
        let c2_src = r2.source_event_id.unwrap();
        assert_ne!(
            c2_src, c1_src,
            "new message must have a new source_event_id"
        );
        store.mark_capture_possibly_sent(&b2, &c2_src).unwrap();

        store.delete_conversation(&c2).unwrap();

        // c1's intent is unchanged (same requested_ts, same fields).
        let c1_intents_after = store.capture_deletion_intents(&b1, 50).unwrap();
        assert_eq!(c1_intents_after.len(), 1);
        assert_eq!(c1_intents_after[0].source_event_id, c1_src);
        assert_eq!(
            c1_intents_after[0].requested_ts, c1_requested_ts,
            "prior intent must be unchanged"
        );

        // c2's intent exists with the new source_event_id.
        let c2_intents_after = store.capture_deletion_intents(&b2, 50).unwrap();
        assert_eq!(c2_intents_after.len(), 1);
        assert_eq!(c2_intents_after[0].source_event_id, c2_src);
        assert_eq!(c2_intents_after[0].cause, "conversation_delete");

        cleanup(&path);
    }

    #[test]
    fn reopen_after_delete_shows_no_payloads_and_stable_intent_fields() {
        // After a whole-conversation delete with possibly_sent victims,
        // reopen the database: no victim payloads remain, and the intent
        // fields are stable (exact source_event_id, full binding,
        // conversation_id, source_ts, cause).
        let path = temp_db();
        let binding = test_binding();
        let srcs: Vec<String>;
        let convo: String;
        {
            let store = Store::open(&path).unwrap();
            convo = new_conv(&store, "reopen", 1000);
            srcs = seed_capture_messages(&store, &convo, &binding, 3, 100);
            store
                .mark_capture_possibly_sent(&binding, &srcs[0])
                .unwrap();
            store
                .mark_capture_possibly_sent(&binding, &srcs[2])
                .unwrap();
            store.delete_conversation(&convo).unwrap();
        }

        let store = Store::open(&path).unwrap();

        assert!(store.conversations().unwrap().iter().all(|c| c.id != convo));

        // No victim payloads remain: all 3 victim outbox rows (srcs[0]
        // possibly_sent, srcs[1] never_sent, srcs[2] possibly_sent) are
        // deleted locally. The 2 intent rows are the complete local record.
        let outbox_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM capture_outbox WHERE conversation_id = ?1",
                rusqlite::params![convo],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(outbox_count, 0);

        assert!(store.capture_events(&binding, 50).unwrap().is_empty());

        let intents = store.capture_deletion_intents(&binding, 50).unwrap();
        assert_eq!(intents.len(), 2);
        for i in &intents {
            assert_eq!(i.binding.epoch, binding.epoch);
            assert_eq!(i.binding.installation_id, binding.installation_id);
            assert_eq!(i.binding.tenant_id, binding.tenant_id);
            assert_eq!(i.binding.user_id, binding.user_id);
            assert_eq!(i.binding.consumer_id, binding.consumer_id);
            assert_eq!(i.conversation_id, convo);
            assert_eq!(i.cause, "conversation_delete");
        }
        let i0 = intents
            .iter()
            .find(|i| i.source_event_id == srcs[0])
            .unwrap();
        let i2 = intents
            .iter()
            .find(|i| i.source_event_id == srcs[2])
            .unwrap();
        assert_eq!(i0.source_ts, 100);
        assert_eq!(i2.source_ts, 102);

        cleanup(&path);
    }
    #[test]
    fn deletion_rejects_each_identity_conflict_for_both_exposures() {
        for marked in [false, true] {
            for column in [
                "binding_epoch",
                "installation_id",
                "tenant_id",
                "user_id",
                "consumer_id",
                "conversation_id",
                "source_ts",
            ] {
                let path = temp_db();
                let store = Store::open(&path).unwrap();
                let convo = new_conv(&store, "identity conflict", 1000);
                let binding = test_binding();
                let sources = seed_capture_messages(&store, &convo, &binding, 1, 50);
                if marked {
                    store
                        .mark_capture_possibly_sent(&binding, &sources[0])
                        .unwrap();
                }
                insert_intent(
                    &store.conn,
                    &sources[0],
                    &convo,
                    &binding,
                    50,
                    999,
                    "truncate",
                );
                let value = if column == "source_ts" {
                    "51"
                } else {
                    "'different'"
                };
                store
                    .conn
                    .execute_batch(&format!(
                        "UPDATE capture_deletion_intents SET {column} = {value}"
                    ))
                    .unwrap();
                assert!(
                    store.delete_conversation(&convo).is_err(),
                    "{column}, marked={marked}"
                );
                assert_eq!(store.messages(&convo).unwrap().len(), 1);
                let count: i64 = store
                    .conn
                    .query_row("SELECT COUNT(*) FROM capture_outbox", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(count, 1);
                cleanup(&path);
            }
        }
    }

    #[test]
    fn late_message_delete_failure_restores_outbox_and_intents() {
        for whole in [false, true] {
            let path = temp_db();
            let store = Store::open(&path).unwrap();
            let convo = new_conv(&store, "late failure", 1000);
            let binding = test_binding();
            let sources = seed_capture_messages(&store, &convo, &binding, 2, 50);
            store
                .mark_capture_possibly_sent(&binding, &sources[0])
                .unwrap();
            // This trigger runs after bookkeeping has inserted the intent and
            // removed victim outbox rows. Abort the final message mutation.
            store
                .conn
                .execute_batch(
                    "CREATE TRIGGER fail_message_delete BEFORE DELETE ON messages
                BEGIN SELECT RAISE(ABORT, 'injected late failure'); END;",
                )
                .unwrap();
            let failed = if whole {
                store.delete_conversation(&convo)
            } else {
                store.truncate_messages(&convo, 0).map(|_| ())
            };
            assert!(failed.is_err());
            assert_eq!(store.messages(&convo).unwrap().len(), 2);
            let count: i64 = store
                .conn
                .query_row("SELECT COUNT(*) FROM capture_outbox", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 2);
            assert!(store
                .capture_deletion_intents(&binding, 10)
                .unwrap()
                .is_empty());
            store
                .conn
                .execute_batch("DROP TRIGGER fail_message_delete")
                .unwrap();
            store.delete_conversation(&convo).unwrap();
            drop(store);
            let reopened = Store::open(&path).unwrap();
            let count: i64 = reopened
                .conn
                .query_row("SELECT COUNT(*) FROM capture_outbox", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
            assert_eq!(
                reopened
                    .capture_deletion_intents(&binding, 10)
                    .unwrap()
                    .len(),
                1
            );
            cleanup(&path);
        }
    }
    #[test]
    fn replacement_insert_failure_rolls_back_tail_and_capture_bookkeeping() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        let convo = new_conv(&store, "replace", 1000);
        let binding = test_binding();
        let sources = seed_capture_messages(&store, &convo, &binding, 3, 50);
        store
            .mark_capture_possibly_sent(&binding, &sources[1])
            .unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER reject_replacement BEFORE INSERT ON messages
            WHEN NEW.text = 'replacement' BEGIN SELECT RAISE(ABORT, 'replacement rejected'); END;",
            )
            .unwrap();
        assert!(store
            .truncate_and_append_message(&convo, 1, "user", "replacement", 51)
            .is_err());
        assert_eq!(store.messages(&convo).unwrap().len(), 3);
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM capture_outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);
        assert!(store
            .capture_deletion_intents(&binding, 10)
            .unwrap()
            .is_empty());
        store
            .conn
            .execute_batch("DROP TRIGGER reject_replacement")
            .unwrap();
        let id = store
            .truncate_and_append_message(&convo, 1, "user", "replacement", 51)
            .unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        let msgs = reopened.messages(&convo).unwrap();
        assert_eq!(
            msgs.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["msg 0", "replacement"]
        );
        assert_eq!(msgs[1].id, id);
        let count: i64 = reopened
            .conn
            .query_row("SELECT COUNT(*) FROM capture_outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let intents = reopened.capture_deletion_intents(&binding, 10).unwrap();
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].source_event_id, sources[1]);
        cleanup(&path);
    }

    /// RC2-APP (F0189): a Work conversation is created WITH its kind in one
    /// write. With a fault that refuses any later kind change, the old
    /// create-then-set-kind pair leaves a Chat row behind; the one-statement
    /// create cannot.
    #[test]
    fn rc2app_a_conversation_is_created_with_its_kind_in_one_write() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER refuse_kind_change BEFORE UPDATE OF kind ON conversations
                 BEGIN SELECT RAISE(ABORT, 'kind change refused'); END;",
            )
            .unwrap();
        // The old two-step path: the second write fails, the first stays.
        store.create_conversation("two-step", "t", 1).unwrap();
        assert!(store.set_conversation_kind("two-step", "work").is_err());
        // The one-step path.
        store
            .create_conversation_with_kind("one-step", "t", 2, "work")
            .unwrap();
        let kinds: Vec<(String, String)> = store
            .conversations()
            .unwrap()
            .into_iter()
            .map(|c| (c.id, c.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("two-step".to_string(), "chat".to_string()),
                ("one-step".to_string(), "work".to_string())
            ]
        );
        cleanup(&path);
    }

    #[test]
    fn workshop_input_binding_commits_or_rolls_back_with_its_thread() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store
            .create_conversation_with_binding(
                "files-only",
                "New thread",
                7,
                "code",
                Some("/project"),
                ("code_conv:files-only", "helper binding"),
            )
            .unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let rows = store.conversations().unwrap();
        assert_eq!(rows[0].kind, "code");
        assert_eq!(rows[0].cwd.as_deref(), Some("/project"));
        assert_eq!(
            store
                .get_app_state("code_conv:files-only")
                .unwrap()
                .as_deref(),
            Some("helper binding")
        );
        // An existing key rejects the second write. The conversation insert
        // must roll back too, leaving no half-bound thread to reopen.
        assert!(store
            .create_conversation_with_binding(
                "refused",
                "New thread",
                8,
                "code",
                None,
                ("code_conv:files-only", "different binding"),
            )
            .is_err());
        assert_eq!(store.conversations().unwrap().len(), 1);
        cleanup(&path);
    }

    /// RC2-APP: app-state rows are removed by key; other keys stay.
    #[test]
    fn rc2app_app_state_rows_are_removed_by_key() {
        let path = temp_db();
        let store = Store::open(&path).unwrap();
        store.set_app_state("work_last_run:a", "1").unwrap();
        store.set_app_state("work_footers:a", "2").unwrap();
        store.set_app_state("work_last_run:b", "3").unwrap();
        store
            .delete_app_states(&[
                "work_last_run:a".to_string(),
                "work_footers:a".to_string(),
                "missing".to_string(),
            ])
            .unwrap();
        assert_eq!(store.get_app_state("work_last_run:a").unwrap(), None);
        assert_eq!(store.get_app_state("work_footers:a").unwrap(), None);
        assert_eq!(
            store.get_app_state("work_last_run:b").unwrap().as_deref(),
            Some("3")
        );
        cleanup(&path);
    }
}

#[cfg(test)]
mod cached_receipt_tests {
    use super::*;
    #[test]
    fn nullable_cached_usage_survives_reopen_without_changing_totals() {
        let store = Store::init(Connection::open_in_memory().unwrap()).unwrap();
        store
            .create_conversation_with_kind("cached-test", "test", 0, "work")
            .unwrap();
        store
            .record_usage_with_cache("cached-test", 1, "codex", "subscription", 100, 5, Some(80))
            .unwrap();
        let row: (u32, u32, Option<u32>) = store
            .conn
            .query_row(
                "SELECT input_tokens,output_tokens,cached_input_tokens FROM usage_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (100, 5, Some(80)));
        store
            .record_usage("cached-test", 2, "codex", "subscription", 10, 1)
            .unwrap();
        let missing: Option<u32> = store
            .conn
            .query_row(
                "SELECT cached_input_tokens FROM usage_events ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(missing, None);
    }
}
