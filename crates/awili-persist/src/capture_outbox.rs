//! Offline capture outbox (stage 1A) — dormant, offline preparation only.
//!
//! This module defines the *data* side of the offline capture stage: the
//! binding/input/receipt/stored-event types and the `capture_outbox` table that
//! stage 1A's schema migration creates. It is **not** capture activation.
//!
//! The API here is offline preparation. `Some(capture)` on
//! [`Store::append_message_with_capture`](crate::Store::append_message_with_capture)
//! is preparatory *caller* input — it records a capture event but performs NO
//! enrollment verification, no claims, no acks, no retries, and no deletion
//! transitions. Nothing in this module touches the network or the sender —
//! there is no live sender, no claims, no retries, and no app wiring in this
//! slice.
//!
//! Immutability is enforced by API behaviour (there is no setter for a stored
//! event's payload) and by the fact that reopened rows come back unchanged —
//! not by a SQL-admin trigger. The outbox carries NO foreign-key cascade to
//! `messages`: a future remote-evidence bookkeeping row must survive the
//! deletion of the local message it was captured alongside.

/// The identity tuple that a capture outbox row is bound to. Retrieval requires
/// ALL five fields to match; there is no cross-binding retrieval. No field is a
/// token, filename, or endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfflineCaptureBinding {
    pub epoch: String,
    pub installation_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub consumer_id: uuid::Uuid,
}

/// Preperatory caller input for a captured append. `redacted_content` is the
/// redacted event text and MAY differ from the local message text. This is NOT
/// an enrollment; it is prepared offline for later activation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfflineCaptureInput {
    pub binding: OfflineCaptureBinding,
    pub redacted_content: String,
    pub adapter_version: String,
}

/// Receipt of a captured append: the local message row id and the immutable
/// source id that the capture event is addressed by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureAppendReceipt {
    pub message_id: i64,
    pub source_event_id: Option<String>,
}

/// A stored, immutable capture event. The full binding, the local message id it
/// was captured alongside, and the redacted content with its original
/// timestamp are all carried. No secret/token fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCaptureEvent {
    /// Immutable source id, minted once as `awili:<UUID>`.
    pub source_event_id: String,
    /// The local message row this event was captured alongside.
    pub message_id: i64,
    pub conversation_id: String,
    pub binding: OfflineCaptureBinding,
    pub actor: String,
    /// Original timestamp carried verbatim from the local message row.
    pub ts: i64,
    pub redacted_content: String,
    pub adapter_version: String,
}

/// The slice-22 (offline capture stage 1A) addition: an offline, dormant
/// capture outbox. One row per captured append.
///
/// `source_event_id` is PRIMARY KEY (the immutable `awili:<UUID>`). `message_id`
/// deliberately has NO foreign key: the outbox is remote-evidence bookkeeping
/// that must not be erased when the local message is truncated or deleted.
/// `actor` is constrained to the capture-eligible roles `user`/`assistant`.
/// `redacted_content` and `adapter_version` are enforced non-empty at the
/// database level. The full binding is indexed so retrieval by the complete
/// identity tuple is a single index scan.
///
/// The `exposure` column (schema v12, stage 1B-a) records whether the event
/// has *possibly* been delivered to a remote recipient. It is one-way
/// (`never_sent` → `possibly_sent`) and is NOT send authority: acceptance,
/// acknowledgement, leases and retries belong to later units.
///
/// `CAPTURE_OUTBOX_DDL` carries the v11 column set exactly as the v10→v11
/// migrator created it; the `exposure` column is added by
/// `migrate_v11_to_v12` via `ALTER TABLE ... ADD COLUMN`. Every `init` path
/// (fresh and migrating) runs that step, so every DB reaches v12 with the
/// column — and the v11→v12 ALTER cannot collide with a pre-existing column
/// because this DDL never creates one.
pub(crate) const CAPTURE_OUTBOX_DDL: &str = "
    CREATE TABLE IF NOT EXISTS capture_outbox (
        source_event_id   TEXT    PRIMARY KEY,
        message_id        INTEGER NOT NULL,
        conversation_id   TEXT    NOT NULL,
        binding_epoch     TEXT    NOT NULL,
        installation_id   TEXT    NOT NULL,
        tenant_id         TEXT    NOT NULL,
        user_id           TEXT    NOT NULL,
        consumer_id       TEXT    NOT NULL,
        actor             TEXT    NOT NULL CHECK (actor IN ('user', 'assistant')),
        source_ts         INTEGER NOT NULL,
        redacted_content  TEXT    NOT NULL CHECK (length(redacted_content) > 0),
        adapter_version   TEXT    NOT NULL CHECK (length(adapter_version) > 0)
    );
    CREATE INDEX IF NOT EXISTS idx_capture_outbox_binding
        ON capture_outbox (binding_epoch, installation_id, tenant_id, user_id, consumer_id);
";

/// The schema v12 (stage 1B-a) addition: the dormant deletion-intent bookkeeping
/// table. It stores the schema's IDENTITY and METADATA only — deliberately NO
/// payload, content, hash, token or raw-reference columns. It is populated
/// exclusively by the deletion/truncate bookkeeping of the independently
/// reviewed 1B-b unit; this unit exposes read + mark authority only, and tests
/// insert synthetic fixtures directly through the test connection.
///
/// `source_event_id` is a PRIMARY KEY (a source event can carry at most one
/// deletion intent — there is no "cleared" status in this schema).
/// `cause` is constrained to the two bookkeeping causes. The full binding is
/// indexed so retrieval by the complete identity tuple is a single index scan.
/// No foreign keys: a deleted source's intent must survive independent of any
/// other table.
pub(crate) const CAPTURE_DELETION_INTENTS_DDL: &str = "
    CREATE TABLE IF NOT EXISTS capture_deletion_intents (
        source_event_id   TEXT    PRIMARY KEY,
        binding_epoch     TEXT    NOT NULL,
        installation_id   TEXT    NOT NULL,
        tenant_id         TEXT    NOT NULL,
        user_id           TEXT    NOT NULL,
        consumer_id       TEXT    NOT NULL,
        conversation_id   TEXT    NOT NULL,
        source_ts         INTEGER NOT NULL,
        requested_ts      INTEGER NOT NULL,
        cause             TEXT    NOT NULL CHECK (cause IN ('truncate', 'conversation_delete'))
    );
    CREATE INDEX IF NOT EXISTS idx_capture_deletion_intents_binding
        ON capture_deletion_intents (binding_epoch, installation_id, tenant_id, user_id, consumer_id);
";

/// Whether a source event has possibly been delivered to a remote recipient.
///
/// One-way at the API level: `never_sent` is the default for every queued
/// event; `possibly_sent` is the terminal exposure state, and the
/// `never_sent` → `possibly_sent` transition happens only inside
/// [`Store::mark_capture_possibly_sent`](crate::Store). The schema `CHECK`
/// merely validates that the column holds one of the two legal values — it
/// does not enforce one-wayness (a direct write could set either value).
/// This is a *possibility* flag, not a delivery receipt — nothing about send
/// authority (acceptance, acknowledgement, leases, retries) is represented
/// here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureExposure {
    NeverSent,
    PossiblySent,
}

/// The one-way exposure transition a capture row can be asked for
/// (stage 1B-a: exposure only; deletion intent mutations are 1B-b).
///
/// * `Marked` — the row was `never_sent` and is now `possibly_sent` (the
///   atomic transition that granted this call the new exposure state).
/// * `AlreadyMarked` — the row was ALREADY `possibly_sent`. The call is
///   idempotent; it does not grant or re-grant anything.
/// * `NotFound` — no `capture_outbox` row exists for this source event under
///   this EXACT full binding, or a deletion intent exists for the source
///   (a deleted source is refused, never resurrected). Both cases return the
///   SAME variant on purpose: the store must not reveal which one, and the
///   call must not mutate anything.
/// * `BindingPaused` — the row is `never_sent` and at least one deletion
///   intent for the SAME FULL binding is pending. New exposure for this
///   binding is paused. NOTE: the independently reviewed 1B-b unit performs
///   the LOCAL deletion bookkeeping but does NOT clear these intents; only a
///   future, independently verified remote protocol may clear them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkExposure {
    Marked,
    AlreadyMarked,
    NotFound,
    BindingPaused,
}

/// A deletion-intent row, immutable as returned (stage 1B-a carries no public
/// mutation API for intents — 1B-b does).
///
/// Carries EXACTLY the schema's identity/metadata columns and nothing else:
/// no payload, content, hash, token or raw-reference field exists here by
/// design.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDeletionIntent {
    pub source_event_id: String,
    pub binding: OfflineCaptureBinding,
    /// The full conversation ID the source event belongs to. Metadata for the
    /// future remote-protocol unit; never used for authorization.
    pub conversation_id: String,
    pub source_ts: i64,
    pub requested_ts: i64,
    pub cause: String,
}
