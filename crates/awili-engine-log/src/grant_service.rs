//! Unit 5 (design r9 §§1-3): the ONE per-user grant store.
//!
//! C1 contract §5 specifies a single fixed per-user store
//! (`<data_dir>/awili/grants.log`) for reusable grants; production had
//! drifted to per-folder files (`work.rs` `folder_grant_key`). This module
//! is the correction: one process-global service owning the one
//! `ExecutionLog<FileLog>` handle, a cross-process advisory lock, torn-tail
//! repair before any append, and the one-shot, crash-atomic migration of
//! the legacy per-folder files.
//!
//! Lock order, binding (r2 verdict rev 2): process mutex FIRST, then
//! `flock(LOCK_EX)` on the sibling `grants.lock`; released in reverse. No
//! configuration, attestation, or coordinator query is made while holding
//! either lock — callers gather inputs fully before entering.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::schema::{LogRecordV1, MigratedSource, SCHEMA_VERSION};
use crate::storage::{AppendOnlyLog, FileLog};
use crate::writer::ExecutionLog;
use crate::{frame, LogError};

/// The narrow surface the seam/authorizer need from a grant store. The
/// worker's long-lived local handle is withdrawn (r3 verdict rev 3);
/// production routes every mutation through [`GrantStoreService`], while
/// tests keep using `ExecutionLog<MemLog>` through the same trait.
pub trait GrantStoreAccess {
    fn read_grant_records(&mut self) -> Result<(Vec<LogRecordV1>, usize), LogError>;
    #[allow(clippy::too_many_arguments)]
    fn record_grant_approval(
        &mut self,
        cap: &crate::authz::HostApprovalCapability,
        scope: &str,
        turn_id: &str,
        call: &awili_engine::events::ToolCall,
        risk_class: crate::authz::RiskClass,
        folder_root: Option<&str>,
        constraints: &str,
    ) -> Result<crate::authz::MintedProof, LogError>;
}

impl<S: AppendOnlyLog> GrantStoreAccess for ExecutionLog<S> {
    fn read_grant_records(&mut self) -> Result<(Vec<LogRecordV1>, usize), LogError> {
        self.read_records()
    }
    fn record_grant_approval(
        &mut self,
        cap: &crate::authz::HostApprovalCapability,
        scope: &str,
        turn_id: &str,
        call: &awili_engine::events::ToolCall,
        risk_class: crate::authz::RiskClass,
        folder_root: Option<&str>,
        constraints: &str,
    ) -> Result<crate::authz::MintedProof, LogError> {
        self.record_approval(
            cap,
            scope,
            turn_id,
            call,
            risk_class,
            folder_root,
            constraints,
        )
    }
}

/// Advisory whole-file lock, held for one critical section. Dropping
/// releases. flock (not fcntl) so the lock rides the open file description
/// and a forked child cannot silently inherit a byte-range lock.
struct FlockGuard {
    file: File,
}

impl FlockGuard {
    fn acquire(lock_path: &Path) -> Result<Self, LogError> {
        // Lock file only: never truncated, its contents are irrelevant.
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)
            .map_err(|e| LogError::Storage(format!("open lock {lock_path:?}: {e}")))?;
        // std::fs::File::lock: flock(LOCK_EX) on Unix, LockFileEx on Windows.
        file.lock()
            .map_err(|e| LogError::Storage(format!("flock {lock_path:?}: {e}")))?;
        Ok(Self { file })
    }
}

impl Drop for FlockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// The process-global owner of the per-user grant store.
pub struct GrantStoreService {
    store_path: PathBuf,
    lock_path: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    log: ExecutionLog<FileLog>,
    /// File length at our last durable observation — another process
    /// appending is detected by a length change, forcing a re-sync of the
    /// reference sequence before we allocate (r3 verdict rev 3).
    last_len: u64,
}

/// One process-long service per store path. Production wiring passes the
/// ONE fixed per-user path (C1 §5), so exactly one service exists there;
/// the per-path registry exists so test rigs get isolated stores without
/// weakening the per-store single-owner property (the owner IS the
/// process-long leaked service; its mutex + flock serialize all access).
static SERVICES: Mutex<Vec<&'static GrantStoreService>> = Mutex::new(Vec::new());

impl GrantStoreService {
    /// Initialize (or return) the process-long service for `store_path`.
    /// Idempotent per path; the same path always yields the same owner.
    pub fn init(
        store_path: impl Into<PathBuf>,
        legacy_dir: Option<&Path>,
    ) -> Result<&'static GrantStoreService, LogError> {
        let store_path = store_path.into();
        let mut registry = SERVICES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = registry.iter().find(|s| s.store_path == store_path) {
            return Ok(existing);
        }
        let lock_path = store_path.with_extension("lock");
        if let Some(parent) = store_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| LogError::Storage(format!("create {parent:?}: {e}")))?;
        }
        // The sibling lock is taken BEFORE probing for or creating the
        // store — probe, migrate, and create are all inside it (r4 §1).
        let _flock = FlockGuard::acquire(&lock_path)?;
        if !store_path.exists() {
            if let Some(dir) = legacy_dir {
                migrate_legacy(dir, &store_path)?;
            }
        }
        // RVS5C-FIX1: repair BEFORE the fresh handle is derived from the
        // existing bytes — a provable torn tail is truncated so the
        // handle's open can read a clean store; an uncertain tail fails
        // init with CorruptTail and leaves the file byte-identical.
        let repaired = repair_torn_tail(&store_path)?;
        let mut log = ExecutionLog::open_grant_store(FileLog::open(&store_path)?)?;
        if repaired {
            log.resync()?;
        }
        let last_len = file_len(&store_path)?;
        let svc: &'static GrantStoreService = Box::leak(Box::new(GrantStoreService {
            store_path,
            lock_path,
            inner: Mutex::new(Inner { log, last_len }),
        }));
        drop(_flock);
        registry.push(svc);
        Ok(svc)
    }

    /// In-crate test accessor: the store path, for tear-injection rigs.
    #[cfg(test)]
    pub(crate) fn store_path_for_tests(&self) -> std::path::PathBuf {
        self.store_path.clone()
    }

    /// Unit 5 seed entry point (design r9 slice s3): one locked critical
    /// section around [`ExecutionLog::record_install_grants`] — the
    /// candidate is consumed, the tombstone/supersession rules apply
    /// against CURRENT durable state, and the outcome (seeded, skipped,
    /// tombstoned, superseded) returns after lock release for the
    /// degradation surface.
    pub fn seed_install_grants(
        &self,
        cap: &crate::authz::HostApprovalCapability,
        candidate: crate::authz::InstallSeedCandidate,
    ) -> Result<crate::writer::InstallSeedOutcome, LogError> {
        self.with_store(move |log| log.record_install_grants(cap, candidate))
    }

    /// One critical section: process mutex, then flock, then re-sync if
    /// another process appended, then `f`. Locks are held ONLY for the
    /// duration of `f` — never across a transport call or a query.
    pub fn with_store<R>(
        &self,
        f: impl FnOnce(&mut ExecutionLog<FileLog>) -> Result<R, LogError>,
    ) -> Result<R, LogError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let _flock = FlockGuard::acquire(&self.lock_path)?;
        // RVS5C-FIX1: a poisoned handle (an earlier append in THIS process
        // failed after writing) is never reused. First undo exactly our own
        // failed bytes when they are provably ours; anything else is left
        // for the decoder to refuse.
        let failed = inner.log.failed_append().cloned();
        if let Some(fa) = &failed {
            rollback_own_failed_append(&self.store_path, fa.offset, &fa.frame)?;
        }
        // Another process's crash may have left a torn tail; the explicit
        // repair path truncates only a PROVABLE one (an incomplete header)
        // and refuses an uncertain one (r4 §3 / RVS5C-FIX1).
        let repaired = repair_torn_tail(&self.store_path)?;
        if failed.is_some() {
            // Reopen: open re-validates the whole store and re-derives every
            // sequence. The poisoned handle is replaced only on success.
            inner.log = ExecutionLog::open_grant_store(FileLog::open(&self.store_path)?)?;
        } else {
            let len = file_len(&self.store_path)?;
            if len != inner.last_len || repaired {
                inner.log.resync()?;
            }
        }
        let out = f(&mut inner.log);
        inner.last_len = file_len(&self.store_path)?;
        out
    }
}

/// A `&'static GrantStoreService` adapter carrying the seam's `&mut`
/// calling convention; every method is one service critical section.
pub struct ServiceHandle(pub &'static GrantStoreService);

impl GrantStoreAccess for ServiceHandle {
    fn read_grant_records(&mut self) -> Result<(Vec<LogRecordV1>, usize), LogError> {
        self.0.with_store(|log| log.read_records())
    }
    fn record_grant_approval(
        &mut self,
        cap: &crate::authz::HostApprovalCapability,
        scope: &str,
        turn_id: &str,
        call: &awili_engine::events::ToolCall,
        risk_class: crate::authz::RiskClass,
        folder_root: Option<&str>,
        constraints: &str,
    ) -> Result<crate::authz::MintedProof, LogError> {
        self.0.with_store(|log| {
            log.record_approval(
                cap,
                scope,
                turn_id,
                call,
                risk_class,
                folder_root,
                constraints,
            )
        })
    }
}

fn file_len(path: &Path) -> Result<u64, LogError> {
    Ok(fs::metadata(path)
        .map_err(|e| LogError::Storage(format!("stat {path:?}: {e}")))?
        .len())
}

/// Truncate a PROVABLE torn tail (an incomplete final header) to the last
/// whole frame and fsync the repair, so a later `FileLog` append cannot
/// entomb torn bytes mid-log (r4 §3). RVS5C-FIX1: an UNCERTAIN tail (a
/// complete header running past EOF) may be a corrupted length hiding the
/// store's last record — it is refused with `CorruptTail` and the file is
/// left byte-identical. Returns whether a repair happened.
fn repair_torn_tail(path: &Path) -> Result<bool, LogError> {
    if !path.exists() {
        return Ok(false);
    }
    let buf = fs::read(path).map_err(|e| LogError::Storage(format!("read {path:?}: {e}")))?;
    let torn = match frame::decode_all(&buf)? {
        frame::DecodeResult::Clean(_) => return Ok(false),
        frame::DecodeResult::TornTail { torn, .. } => torn,
        frame::DecodeResult::UncertainTail { .. } => return Err(LogError::CorruptTail),
    };
    truncate_and_sync(path, (buf.len() - torn) as u64)?;
    Ok(true)
}

fn truncate_and_sync(path: &Path, keep: u64) -> Result<(), LogError> {
    let f = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| LogError::Storage(format!("open for repair {path:?}: {e}")))?;
    f.set_len(keep)
        .map_err(|e| LogError::Storage(format!("truncate {path:?}: {e}")))?;
    f.sync_data()
        .map_err(|e| LogError::Storage(format!("fsync repair {path:?}: {e}")))?;
    Ok(())
}

/// RVS5C-FIX1: undo THIS process's own failed append, proven byte for
/// byte. `offset` is the store length before the failed append and
/// `attempted` the exact frame it tried to write. Truncates to `offset`
/// ONLY when everything before `offset` decodes clean AND the bytes after
/// it are a prefix of (or equal to) `attempted`. Anything else is left
/// untouched for the decoder to refuse. Returns whether it truncated.
fn rollback_own_failed_append(
    path: &Path,
    offset: u64,
    attempted: &[u8],
) -> Result<bool, LogError> {
    let buf = fs::read(path).map_err(|e| LogError::Storage(format!("read {path:?}: {e}")))?;
    let Ok(at) = usize::try_from(offset) else {
        return Ok(false);
    };
    if buf.len() <= at || !attempted.starts_with(&buf[at..]) {
        return Ok(false);
    }
    if !matches!(
        frame::decode_all(&buf[..at])?,
        frame::DecodeResult::Clean(_)
    ) {
        return Ok(false);
    }
    truncate_and_sync(path, offset)?;
    Ok(true)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Full-width source identity (r4 §1):
/// `sha256(canonical_path_bytes || 0x00 || sha256(file_bytes))`, hex.
fn source_id(canonical_path: &Path, file_bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(canonical_path.as_os_str().as_encoded_bytes());
    h.update([0u8]);
    h.update(Sha256::digest(file_bytes));
    hex(&h.finalize())
}

struct SourceSnapshot {
    path: PathBuf, // as discovered (for re-verification re-canonicalization)
    canonical: PathBuf,
    bytes: Vec<u8>,
    id: String,
}

/// One-shot migration of the legacy per-folder grant files into a staged
/// per-user store, committed by atomic rename (design r9 §1; r3 §1 as
/// amended by the r3/r4 verdicts). Caller holds the sibling flock.
fn migrate_legacy(legacy_dir: &Path, store_path: &Path) -> Result<(), LogError> {
    migrate_legacy_hooked(legacy_dir, store_path, || {})
}

/// The hook runs between staging and the pre-rename re-verification —
/// tests inject a source mutation there to prove discard-and-restart.
pub(crate) fn migrate_legacy_hooked(
    legacy_dir: &Path,
    store_path: &Path,
    between_stage_and_verify: impl Fn(),
) -> Result<(), LogError> {
    const MAX_RESTARTS: u32 = 3;
    for _attempt in 0..MAX_RESTARTS {
        let snaps = snapshot_sources(legacy_dir)?;
        if snaps.is_empty() {
            return Ok(()); // nothing to migrate; open creates a fresh store
        }
        let staged = build_staged(&snaps)?;
        between_stage_and_verify();
        // Immediately before rename: re-canonicalize, re-read, re-digest,
        // recompute the complete source_id; ANY change discards the staged
        // bytes and restarts (r4 §1).
        let fresh = snapshot_sources(legacy_dir)?;
        let same = fresh.len() == snaps.len()
            && fresh
                .iter()
                .zip(&snaps)
                .all(|(a, b)| a.canonical == b.canonical && a.id == b.id && a.bytes == b.bytes);
        if !same {
            continue;
        }
        let tmp = store_path.with_extension("log.tmp");
        {
            let mut f = File::create(&tmp)
                .map_err(|e| LogError::Storage(format!("create {tmp:?}: {e}")))?;
            f.write_all(&staged)
                .map_err(|e| LogError::Storage(format!("write staged: {e}")))?;
            f.sync_all()
                .map_err(|e| LogError::Storage(format!("fsync staged: {e}")))?;
        }
        fs::rename(&tmp, store_path)
            .map_err(|e| LogError::Storage(format!("rename staged: {e}")))?;
        if let Some(parent) = store_path.parent() {
            // Directory fsync makes the rename durable; a failure here is
            // a failure of the migration's durability claim (r1 P1-3) —
            // surfaced, never discarded.
            let d = File::open(parent)
                .map_err(|e| LogError::Storage(format!("open dir {parent:?}: {e}")))?;
            d.sync_all()
                .map_err(|e| LogError::Storage(format!("fsync dir {parent:?}: {e}")))?;
        }
        return Ok(());
    }
    Err(LogError::Storage(
        "grant migration restarted 3 times: legacy sources kept changing".into(),
    ))
}

/// Read every legacy source ONCE into memory; digest and parse the same
/// buffer (r4 §1 "parse what was digested"). Lexicographic filename order.
fn snapshot_sources(legacy_dir: &Path) -> Result<Vec<SourceSnapshot>, LogError> {
    // Impl review r1 P1-3: ONLY a missing directory means "nothing to
    // migrate". Any other read_dir or entry error fails initialization —
    // creating the store over an unreadable legacy directory would
    // permanently skip its grants (the marker forecloses re-ingestion).
    let rd = match fs::read_dir(legacy_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(LogError::Storage(format!(
                "legacy grants dir {legacy_dir:?}: {e}"
            )))
        }
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in rd {
        let entry = entry
            .map_err(|e| LogError::Storage(format!("legacy dir entry in {legacy_dir:?}: {e}")))?;
        let p = entry.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        // RC2-ENG2 (F0566): `is_file()` turned a metadata error into "not a
        // file", silently skipping that source forever (the marker forecloses
        // re-ingestion). Only an entry that is gone (removed since the
        // listing, or a dangling link) is skipped; any other error fails
        // initialization like an unreadable directory.
        match fs::metadata(&p) {
            Ok(m) if m.is_file() => paths.push(p),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(LogError::Storage(format!(
                    "legacy grants source {p:?}: {e}"
                )))
            }
        }
    }
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes =
            fs::read(&path).map_err(|e| LogError::Storage(format!("read {path:?}: {e}")))?;
        let canonical = fs::canonicalize(&path)
            .map_err(|e| LogError::Storage(format!("canonicalize {path:?}: {e}")))?;
        let id = source_id(&canonical, &bytes);
        out.push(SourceSnapshot {
            path,
            canonical,
            bytes,
            id,
        });
    }
    Ok(out)
}

/// Build the complete staged store: Header, then per source its remapped
/// approvals (each immediately followed by its MigrationAlias), translated
/// revocations, and other records verbatim; LAST the StoreMigrated marker
/// (inside the atomic unit, so "store exists => migration committed").
fn build_staged(snaps: &[SourceSnapshot]) -> Result<Vec<u8>, LogError> {
    let mut out = Vec::new();
    let push = |rec: &LogRecordV1, out: &mut Vec<u8>| -> Result<(), LogError> {
        out.extend_from_slice(&frame::encode(&serde_json::to_vec(rec)?));
        Ok(())
    };
    push(
        &LogRecordV1::Header {
            version: SCHEMA_VERSION,
            session_id: "grants".into(),
        },
        &mut out,
    )?;
    let mut marker_sources = Vec::with_capacity(snaps.len());
    let mut total = 0u64;
    for snap in snaps {
        // S5(c) / F0564: a legacy source with ANY torn bytes — or one the
        // decoder cannot prove to end in a clean torn frame — aborts the
        // migration. No marker is written (the staged file is discarded
        // before the rename) and the store is never published over sources
        // whose tail we cannot account for; re-ingestion stays possible.
        let frames = match frame::decode_all(&snap.bytes)? {
            frame::DecodeResult::Clean(frames) => frames,
            frame::DecodeResult::TornTail { torn: n, .. }
            | frame::DecodeResult::UncertainTail { tail: n, .. } => {
                return Err(LogError::Corrupt {
                    seq: 0,
                    reason: format!(
                        "legacy source {:?} has {n} torn bytes; migration refuses",
                        snap.path
                    ),
                });
            }
        };
        let mut records = Vec::with_capacity(frames.len());
        for (i, fbytes) in frames.iter().enumerate() {
            let rec =
                serde_json::from_slice::<LogRecordV1>(fbytes).map_err(|e| LogError::Corrupt {
                    seq: i as u64,
                    reason: format!("legacy record undecodable in {:?}: {e}", snap.path),
                })?;
            records.push(rec);
        }
        // Per-file remap: approval index -> new ref (r4 §1: keyed by
        // (source_id, old_ref, in-file index); refs are injective because
        // source_id is full-width and N is the in-file index).
        let mut remap: std::collections::BTreeMap<String, String> = Default::default();
        let mut imported = 0u64;
        let mut n = 0u64;
        for rec in &records {
            match rec {
                LogRecordV1::Header { .. } => {}
                LogRecordV1::ApprovalRecorded {
                    approval_ref,
                    decision,
                    scope,
                    turn_id,
                    awili_call_id,
                    normalized_call_hash,
                    session_id,
                    tool,
                    risk_class,
                    folder_root,
                    constraints,
                    granted_at_ms,
                } => {
                    let new_ref = format!("appr-migrated-{}-{n}", snap.id);
                    n += 1;
                    remap.insert(approval_ref.clone(), new_ref.clone());
                    push(
                        &LogRecordV1::ApprovalRecorded {
                            approval_ref: new_ref.clone(),
                            decision: decision.clone(),
                            scope: scope.clone(),
                            turn_id: turn_id.clone(),
                            awili_call_id: awili_call_id.clone(),
                            normalized_call_hash: normalized_call_hash.clone(),
                            session_id: session_id.clone(),
                            tool: tool.clone(),
                            risk_class: risk_class.clone(),
                            folder_root: folder_root.clone(),
                            constraints: constraints.clone(),
                            granted_at_ms: *granted_at_ms,
                        },
                        &mut out,
                    )?;
                    push(
                        &LogRecordV1::MigrationAlias {
                            source_id: snap.id.clone(),
                            old_ref: approval_ref.clone(),
                            new_ref,
                        },
                        &mut out,
                    )?;
                    imported += 1;
                }
                LogRecordV1::GrantRevoked { approval_ref } => {
                    // Translated through this file's OWN map; a revocation
                    // referencing a ref absent from its own file imports
                    // verbatim — kept for audit, resolvable never (fails
                    // closed by construction).
                    let target = remap
                        .get(approval_ref)
                        .cloned()
                        .unwrap_or_else(|| approval_ref.clone());
                    push(
                        &LogRecordV1::GrantRevoked {
                            approval_ref: target,
                        },
                        &mut out,
                    )?;
                    imported += 1;
                }
                other => {
                    push(other, &mut out)?;
                    imported += 1;
                }
            }
        }
        total += imported;
        marker_sources.push(MigratedSource {
            source_id: snap.id.clone(),
            canonical_path: snap.canonical.to_string_lossy().into_owned(),
            sha256_bytes: sha256_hex(&snap.bytes),
            imported_record_count: imported,
        });
    }
    marker_sources.sort_by(|a, b| a.source_id.cmp(&b.source_id));
    push(
        &LogRecordV1::StoreMigrated {
            sources: marker_sources,
            total,
        },
        &mut out,
    )?;
    Ok(out)
}
