//! Unit 5 slice s1 tests: the per-user grant store service — migration
//! (injective remap, aliases, marker, crash surfaces), torn-tail repair,
//! cross-handle reference re-sync, and the service mutation path.

use std::fs;
use std::path::{Path, PathBuf};

use crate::authz::RiskClass;
use crate::grant_service::{GrantStoreAccess, GrantStoreService, ServiceHandle};
use crate::replay::{classify_with_grants, IntentStatus};
use crate::schema::LogRecordV1;
use crate::storage::{FileLog, MemLog};
use crate::writer::ExecutionLog;
use crate::{frame, LogError};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "awili-u5-s1-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn call(name: &str, args: &str) -> awili_engine::events::ToolCall {
    awili_engine::events::ToolCall {
        awili_call_id: awili_engine::events::AwiliCallId("c1".into()),
        provider_call_id: None,
        name: name.into(),
        arguments: args.into(),
    }
}

/// Build a LEGACY per-folder grant store file (the pre-unit-5 layout:
/// `ExecutionLog::open_grant_store` over a per-folder path — session id
/// "grants", refs `appr-grants-N`). Returns the refs it minted.
fn write_legacy(path: &Path, tools: &[&str], revoke_first: bool) -> Vec<String> {
    let cap = crate::authz::HostApprovalCapability::test_only();
    let mut log = ExecutionLog::open_grant_store(FileLog::open(path).unwrap()).unwrap();
    let mut refs = Vec::new();
    for t in tools {
        let proof = log
            .record_approval(
                &cap,
                "folder",
                "t1",
                &call(t, r#"{"path":"a"}"#),
                RiskClass::Mutating,
                Some("/w"),
                "{}",
            )
            .unwrap();
        refs.push(match proof {
            crate::authz::MintedProof::Grant(g) => g.approval_ref,
            crate::authz::MintedProof::Once(o) => o.approval_ref,
        });
    }
    if revoke_first {
        log.append_grant_revoked(&refs[0]).unwrap();
    }
    refs
}

fn read_store(path: &Path) -> Vec<LogRecordV1> {
    let buf = fs::read(path).unwrap();
    let frame::DecodeResult::Clean(frames) = frame::decode_all(&buf).unwrap() else {
        panic!("committed store never has a torn tail");
    };
    frames
        .iter()
        .map(|f| serde_json::from_slice(f).unwrap())
        .collect()
}

/// Migration: two legacy files that MINTED THE SAME REFS (the collision the
/// remap exists for), one carrying a revocation — refs are remapped
/// injectively, the revocation is translated through its OWN file's map,
/// aliases land beside each approval, and the marker commits last with
/// explicit full identities (gate rows: migration N + M).
#[test]
fn migration_remaps_colliding_refs_and_translates_revocations() {
    let root = tmpdir("mig");
    let legacy = root.join("grants");
    fs::create_dir_all(&legacy).unwrap();
    let refs_a = write_legacy(&legacy.join("aaaa.json"), &["fs.write"], true);
    let refs_b = write_legacy(&legacy.join("bbbb.json"), &["fs.edit"], false);
    // The collision under test: both legacy stores minted appr-grants-0000.
    assert_eq!(refs_a[0], refs_b[0], "precondition: legacy refs collide");

    let store = root.join("grants.log");
    let svc = GrantStoreService::init(&store, Some(&legacy)).unwrap();
    let recs = read_store(&store);

    let approvals: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::ApprovalRecorded { approval_ref, .. } => Some(approval_ref.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(approvals.len(), 2);
    assert_ne!(approvals[0], approvals[1], "remap must be injective");
    for a in &approvals {
        assert!(a.starts_with("appr-migrated-"), "namespaced: {a}");
    }
    let aliases: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::MigrationAlias {
                old_ref, new_ref, ..
            } => Some((old_ref.clone(), new_ref.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(aliases.len(), 2);
    assert!(aliases.iter().all(|(o, _)| o == &refs_a[0]));
    // The revocation translated through file A's own map — it must target
    // A's REMAPPED ref, and that ref must be one of the imported approvals.
    let revoked: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::GrantRevoked { approval_ref } => Some(approval_ref.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(revoked.len(), 1);
    assert!(approvals.contains(&revoked[0]));
    let a_alias = aliases
        .iter()
        .find(|(_, n)| n == &revoked[0])
        .expect("revoked ref is file A's remap");
    assert_eq!(a_alias.0, refs_a[0]);
    // RC2-ENG2 (F0579): both files share the OLD ref, so the alias check
    // above cannot tell which file's approval was revoked. File A's is the
    // only `fs.write` approval: the revocation must name exactly it, and
    // file B's (`fs.edit`) must stay live.
    let new_ref_of = |tool: &str| {
        recs.iter()
            .find_map(|r| match r {
                LogRecordV1::ApprovalRecorded {
                    approval_ref,
                    tool: t,
                    ..
                } if t == tool => Some(approval_ref.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(revoked[0], new_ref_of("fs.write"), "file A's approval");
    assert_ne!(revoked[0], new_ref_of("fs.edit"), "file B's stays live");
    // Marker: last record, sorted by source_id, explicit identities.
    match recs.last().unwrap() {
        LogRecordV1::StoreMigrated { sources, total } => {
            assert_eq!(sources.len(), 2);
            assert!(sources[0].source_id < sources[1].source_id);
            assert!(sources.iter().all(|s| s.source_id.len() == 64));
            assert!(sources.iter().all(|s| s.sha256_bytes.len() == 64));
            assert!(*total >= 3);
        }
        other => panic!("marker must commit last, found {other:?}"),
    }
    // Idempotence: the same path yields the same service; the store is
    // never re-ingested (record count is stable across a second init).
    let svc2 = GrantStoreService::init(&store, Some(&legacy)).unwrap();
    assert!(std::ptr::eq(svc, svc2));
    assert_eq!(read_store(&store).len(), recs.len());
}

/// Identical BYTES at two paths still migrate injectively — the source id
/// covers the canonical path, not just the digest (r4 §1).
#[test]
fn identical_files_at_two_paths_get_distinct_source_ids() {
    let root = tmpdir("ident");
    let legacy = root.join("grants");
    fs::create_dir_all(&legacy).unwrap();
    write_legacy(&legacy.join("aaaa.json"), &["fs.write"], false);
    fs::copy(legacy.join("aaaa.json"), legacy.join("bbbb.json")).unwrap();

    let store = root.join("grants.log");
    GrantStoreService::init(&store, Some(&legacy)).unwrap();
    let recs = read_store(&store);
    let ids: std::collections::BTreeSet<_> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::MigrationAlias { source_id, .. } => Some(source_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2, "same bytes, two canonical paths, two ids");
    let approvals: std::collections::BTreeSet<_> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::ApprovalRecorded { approval_ref, .. } => Some(approval_ref.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(approvals.len(), 2, "remapped refs stay distinct");
}

/// A PRE-migration execution log resolves its legacy grant ref through the
/// alias table: a UNIQUE alias classifies exactly as before; a ref with
/// MULTIPLE aliases is authorization-evidence ambiguity — AuthzInvalid,
/// never Clean, never resumable (r5 verdict ruling 10; gate row O).
#[test]
fn legacy_execution_log_resolves_unique_alias_and_refuses_ambiguous() {
    // Grant store with two aliases for the SAME old ref (ambiguous) and one
    // unique alias for a second old ref.
    let grant_records = vec![
        LogRecordV1::ApprovalRecorded {
            approval_ref: "appr-migrated-s1-0".into(),
            decision: "approved".into(),
            scope: "folder".into(),
            turn_id: String::new(),
            awili_call_id: String::new(),
            normalized_call_hash: "h1".into(),
            session_id: "grants".into(),
            tool: "fs.write".into(),
            risk_class: "mutating".into(),
            folder_root: Some("/w".into()),
            constraints: "{}".into(),
            granted_at_ms: None,
        },
        LogRecordV1::MigrationAlias {
            source_id: "s1".into(),
            old_ref: "appr-grants-0000".into(),
            new_ref: "appr-migrated-s1-0".into(),
        },
        LogRecordV1::ApprovalRecorded {
            approval_ref: "appr-migrated-s2-0".into(),
            decision: "approved".into(),
            scope: "folder".into(),
            turn_id: String::new(),
            awili_call_id: String::new(),
            normalized_call_hash: "h1".into(),
            session_id: "grants".into(),
            tool: "fs.write".into(),
            risk_class: "mutating".into(),
            folder_root: Some("/w".into()),
            constraints: "{}".into(),
            granted_at_ms: None,
        },
        LogRecordV1::MigrationAlias {
            source_id: "s2".into(),
            old_ref: "appr-grants-0000".into(),
            new_ref: "appr-migrated-s2-0".into(),
        },
        LogRecordV1::ApprovalRecorded {
            approval_ref: "appr-migrated-s3-0".into(),
            decision: "approved".into(),
            scope: "folder".into(),
            turn_id: String::new(),
            awili_call_id: String::new(),
            normalized_call_hash: "h2".into(),
            session_id: "grants".into(),
            tool: "fs.read".into(),
            risk_class: "read_only".into(),
            folder_root: Some("/w".into()),
            constraints: "{}".into(),
            granted_at_ms: None,
        },
        LogRecordV1::MigrationAlias {
            source_id: "s3".into(),
            old_ref: "appr-grants-0001".into(),
            new_ref: "appr-migrated-s3-0".into(),
        },
    ];
    // A legacy session log whose grant-basis evidence names the OLD refs.
    let mk_intent = |intent_id: &str, aref: &str, hash: &str, tool: &str| {
        vec![
            LogRecordV1::TurnOpened {
                turn_id: "t1".into(),
                provider: String::new(),
                model: String::new(),
            },
            LogRecordV1::Intent {
                turn_id: "t1".into(),
                intent_id: intent_id.into(),
                awili_call_id: "c1".into(),
                tool: tool.into(),
                raw_arguments: "{}".into(),
                canonical_args: None,
                call_fingerprint: None,
                argument_validation: crate::schema::ArgumentValidationV1::Valid,
                risk_class: "mutating".into(),
                decision_basis: "grant".into(),
                evidence_call_hash: hash.into(),
                identity_digest: String::new(),
                approval_ref: aref.into(),
            },
        ]
    };
    // Ambiguous old ref: two aliases match.
    let recs = mk_intent("intent-1", "appr-grants-0000", "h1", "fs.write");
    let out = classify_with_grants(&recs, &grant_records);
    let st = &out[0].intents[0].status;
    match st {
        IntentStatus::AuthzInvalid { reason } => {
            assert!(
                reason.contains("ambiguous migrated approval reference"),
                "{reason}"
            );
        }
        other => panic!("ambiguity must be AuthzInvalid, got {other:?}"),
    }
    // Unique old ref: resolves and verifies (hash matches) — NOT invalid.
    let recs = mk_intent("intent-2", "appr-grants-0001", "h2", "fs.read");
    let out = classify_with_grants(&recs, &grant_records);
    assert!(
        !matches!(out[0].intents[0].status, IntentStatus::AuthzInvalid { .. }),
        "unique alias must classify as before migration: {:?}",
        out[0].intents[0].status
    );
}

/// Torn tail: a PROVABLE torn final write (an incomplete header) is
/// truncated (and fsynced) before any append, so the next mutation can
/// never entomb torn bytes mid-log (r4 §3 / r5 verdict ruling 12).
/// RVS5C-FIX1 (spec-mandated): the tear is 5 bytes; a complete header with
/// a short payload is now refused, not repaired (covered by
/// `uncertain_grant_tail_is_refused_and_left_byte_identical`).
#[test]
fn torn_tail_is_repaired_before_the_next_append() {
    let root = tmpdir("torn");
    let store = root.join("grants.log");
    let svc = GrantStoreService::init(&store, None).unwrap();
    let cap = crate::authz::HostApprovalCapability::test_only();
    svc.with_store(|log| {
        log.record_approval(
            &cap,
            "always",
            "t1",
            &call("fs.read", "{}"),
            RiskClass::ReadOnly,
            None,
            "{}",
        )
    })
    .unwrap();
    let clean_len = fs::metadata(&store).unwrap().len();
    // Tear inside the header, as a writer that crashed after 5 bytes would.
    let torn = frame::encode(b"never-finished");
    let mut f = fs::OpenOptions::new().append(true).open(&store).unwrap();
    use std::io::Write as _;
    f.write_all(&torn[..5]).unwrap();
    drop(f);
    // Next mutation repairs first, then appends; the store reads clean.
    svc.with_store(|log| {
        log.record_approval(
            &cap,
            "always",
            "t2",
            &call("fs.glob", "{}"),
            RiskClass::ReadOnly,
            None,
            "{}",
        )
    })
    .unwrap();
    let recs = read_store(&store);
    assert!(fs::metadata(&store).unwrap().len() > clean_len);
    let approvals: Vec<_> = recs
        .iter()
        .filter(|r| matches!(r, LogRecordV1::ApprovalRecorded { .. }))
        .collect();
    assert_eq!(approvals.len(), 2, "both real approvals, no torn residue");
}

/// S5(c) / F0595: a SESSION log with a proven torn tail refuses reopen and
/// resync — no append lands behind the torn bytes, and the file is never
/// mutated by the refusal. The grant store's explicit repair path stays
/// separate (tested in `torn_tail_is_repaired_before_the_next_append`).
#[test]
fn torn_tail_refuses_reopen_and_append() {
    let root = tmpdir("refuse");
    let path = root.join("session.log");
    let mut log = ExecutionLog::open(FileLog::open(&path).unwrap(), "s1").unwrap();
    log.turn_closed("t1", "completed").unwrap();
    drop(log);
    // Tear: append half a frame, as a crashed writer would.
    let torn = frame::encode(b"never-finished");
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    use std::io::Write as _;
    f.write_all(&torn[..torn.len() / 2]).unwrap();
    drop(f);
    let torn_len = fs::metadata(&path).unwrap().len();
    // Reopen refuses: a torn tail before any append has no repair path.
    match ExecutionLog::open(FileLog::open(&path).unwrap(), "s1") {
        Ok(_) => panic!("reopen must refuse"),
        Err(LogError::Corrupt { seq: 0, reason }) => {
            assert!(reason.contains("torn tail at open"), "{reason}");
        }
        Err(e) => panic!("reopen must refuse with Corrupt, got {e:?}"),
    };
    // The grant-store open path refuses the SAME way (same open_inner).
    match ExecutionLog::open_grant_store(FileLog::open(&path).unwrap()) {
        Ok(_) => panic!("grant-store open must refuse"),
        Err(LogError::Corrupt { seq: 0, reason }) => {
            assert!(reason.contains("torn tail at open"), "{reason}");
        }
        Err(e) => panic!("grant-store open must refuse, got {e:?}"),
    };
    // Refusal never appends behind the torn bytes or mutates the file.
    assert_eq!(fs::metadata(&path).unwrap().len(), torn_len);

    // Resync refuses too: a LIVE handle that re-reads a torn state refuses
    // rather than reissue ids from a half-torn log.
    let path2 = root.join("session2.log");
    let mut live2 = ExecutionLog::open(FileLog::open(&path2).unwrap(), "s2").unwrap();
    live2.turn_closed("t1", "completed").unwrap();
    // Tear the tail from OUTSIDE the live handle (a crashed writer).
    let torn2 = frame::encode(b"never-finished-2");
    let mut f = fs::OpenOptions::new().append(true).open(&path2).unwrap();
    f.write_all(&torn2[..torn2.len() / 2]).unwrap();
    drop(f);
    let torn_len2 = fs::metadata(&path2).unwrap().len();
    // The live handle's resync must refuse — it re-reads the torn state.
    match live2.resync() {
        Ok(()) => panic!("resync must refuse"),
        Err(LogError::Corrupt { seq: 0, reason }) => {
            assert!(reason.contains("torn tail at resync"), "{reason}");
        }
        Err(e) => panic!("resync must refuse with Corrupt, got {e:?}"),
    };
    // And no append lands behind the torn bytes: the file is untouched.
    assert_eq!(fs::metadata(&path2).unwrap().len(), torn_len2);
    // The torn file still refuses a fresh reopen.
    match ExecutionLog::open(FileLog::open(&path2).unwrap(), "s2") {
        Ok(_) => panic!("fresh reopen must refuse"),
        Err(LogError::Corrupt { .. }) => {}
        Err(e) => panic!("fresh reopen must refuse, got {e:?}"),
    };
}

/// S5(c) / F0564: a legacy source with torn bytes ABORTS the migration —
/// no store is published, no marker is written, the store path does not
/// exist. The operator must repair the source first; migration never
/// silently discards undecodable bytes.
#[test]
fn torn_legacy_source_aborts_migration() {
    let root = tmpdir("tornmig");
    let legacy = root.join("grants");
    fs::create_dir_all(&legacy).unwrap();
    // A LEGITIMATE legacy file with one approval.
    let legacy_path = legacy.join("aaaa.json");
    write_legacy(&legacy_path, &["fs.write"], false);
    // TEAR the tail: append half a frame, as a crashed writer would.
    let torn = frame::encode(b"torn-legacy");
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(&legacy_path)
        .unwrap();
    use std::io::Write as _;
    f.write_all(&torn[..torn.len() / 2]).unwrap();
    drop(f);
    // Migration must REFUSE: no store, no marker.
    let store = root.join("grants.log");
    // The refusal is a LogError::Corrupt (torn tail).
    match GrantStoreService::init(&store, Some(&legacy)) {
        Ok(_) => panic!("migration must refuse a torn legacy source"),
        Err(LogError::Corrupt { seq: 0, reason }) => {
            assert!(
                reason.contains("torn bytes") || reason.contains("torn tail"),
                "refusal must name the torn bytes: {reason}"
            );
        }
        Err(e) => panic!("migration must refuse with Corrupt, got {e:?}"),
    }
    // No store file was published.
    assert!(
        !store.exists(),
        "migration refusal must not publish a store file"
    );
    // No staged tmp file lingers.
    assert!(
        !store.with_extension("log.tmp").exists(),
        "migration refusal must not leave a staged tmp file"
    );
}

/// S5(c) / F0565: an ORPHAN revocation — a `GrantRevoked` ref that names
/// no approval in the store — still reserves its sequence at open. A later
/// grant cannot be minted with the same ref and silently cancel the orphan
/// revocation's target. Uses `ExecutionLog` directly (not `GrantStoreService`)
/// because the service registry is process-global and cannot be re-opened.
#[test]
fn orphan_revocation_reserves_reference() {
    let root = tmpdir("orphan");
    let path = root.join("grants.log");
    // Phase 1: open a grant store, mint one approval (seq 0).
    let cap = crate::authz::HostApprovalCapability::test_only();
    {
        let mut log = ExecutionLog::open_grant_store(FileLog::open(&path).unwrap()).unwrap();
        log.record_approval(
            &cap,
            "always",
            "t1",
            &call("fs.read", "{}"),
            RiskClass::ReadOnly,
            None,
            "{}",
        )
        .unwrap();
        // Append an ORPHAN revocation: a ref no approval names.
        log.append_grant_revoked("appr-grants-0005").unwrap();
    }
    // Phase 2: reopen — the orphan must reserve seq 5.
    let mut log2 = ExecutionLog::open_grant_store(FileLog::open(&path).unwrap()).unwrap();
    let r2 = log2
        .record_approval(
            &cap,
            "always",
            "t2",
            &call("fs.glob", "{}"),
            RiskClass::ReadOnly,
            None,
            "{}",
        )
        .unwrap();
    let ref2 = match r2 {
        crate::authz::MintedProof::Grant(g) => g.approval_ref,
        crate::authz::MintedProof::Once(o) => o.approval_ref,
    };
    // The orphan ref was 0005, so the next mint must be 0006 or later.
    assert_ne!(
        ref2, "appr-grants-0005",
        "orphan revocation must reserve its ref: minted {ref2}"
    );
    // Extract the numeric suffix and compare: the new ref must be ≥ 0006.
    let new_seq: u64 = ref2
        .rsplit('-')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    assert!(
        new_seq >= 6,
        "new ref {ref2} (seq {new_seq}) must be after orphan appr-grants-0005"
    );
    // The orphan revocation is still in the store (audit).
    let recs = read_store(&path);
    let revoked: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::GrantRevoked { approval_ref } => Some(approval_ref.clone()),
            _ => None,
        })
        .collect();
    assert!(revoked.contains(&"appr-grants-0005".to_string()));
}

/// S5(c) / F0592: a revocation that names an OLD (aliased) ref resolves
/// through the alias table and INVALIDATES the live grant — the intent
/// whose evidence names the old ref is AuthzInvalid. An ambiguous alias
/// (multiple aliases for the same old ref) also fails closed.
#[test]
fn aliased_revocation_invalidates_intent() {
    // Grant store with ONE live approval and ONE alias for an old ref.
    let grant_records = vec![
        LogRecordV1::ApprovalRecorded {
            approval_ref: "appr-migrated-s1-0".into(),
            decision: "approved".into(),
            scope: "folder".into(),
            turn_id: String::new(),
            awili_call_id: String::new(),
            normalized_call_hash: "h1".into(),
            session_id: "grants".into(),
            tool: "fs.write".into(),
            risk_class: "mutating".into(),
            folder_root: Some("/w".into()),
            constraints: "{}".into(),
            granted_at_ms: None,
        },
        LogRecordV1::MigrationAlias {
            source_id: "s1".into(),
            old_ref: "appr-grants-0000".into(),
            new_ref: "appr-migrated-s1-0".into(),
        },
        // The revocation names the OLD ref.
        LogRecordV1::GrantRevoked {
            approval_ref: "appr-grants-0000".into(),
        },
    ];
    // A legacy session log whose grant-basis evidence names the OLD ref.
    let mk_intent = |intent_id: &str, aref: &str, hash: &str, tool: &str| {
        vec![
            LogRecordV1::TurnOpened {
                turn_id: "t1".into(),
                provider: String::new(),
                model: String::new(),
            },
            LogRecordV1::Intent {
                turn_id: "t1".into(),
                intent_id: intent_id.into(),
                awili_call_id: "c1".into(),
                tool: tool.into(),
                raw_arguments: "{}".into(),
                canonical_args: None,
                call_fingerprint: None,
                argument_validation: crate::schema::ArgumentValidationV1::Valid,
                risk_class: "mutating".into(),
                decision_basis: "grant".into(),
                evidence_call_hash: hash.into(),
                identity_digest: String::new(),
                approval_ref: aref.into(),
            },
        ]
    };
    // The intent names the OLD ref, which resolves through the alias to the
    // live grant — but that grant is REVOKED. Must be AuthzInvalid.
    let recs = mk_intent("intent-1", "appr-grants-0000", "h1", "fs.write");
    let out = classify_with_grants(&recs, &grant_records);
    let st = &out[0].intents[0].status;
    match st {
        IntentStatus::AuthzInvalid { reason } => {
            assert!(
                reason.contains("revocation") || reason.contains("revoked"),
                "revocation must invalidate the intent: {reason}"
            );
        }
        other => panic!("revoked alias must be AuthzInvalid, got {other:?}"),
    }

    // AMBIGUOUS case: TWO aliases for the same old ref. The revocation
    // cannot resolve unambiguously — fail closed.
    let grant_records_ambig = vec![
        LogRecordV1::ApprovalRecorded {
            approval_ref: "appr-migrated-a-0".into(),
            decision: "approved".into(),
            scope: "folder".into(),
            turn_id: String::new(),
            awili_call_id: String::new(),
            normalized_call_hash: "h2".into(),
            session_id: "grants".into(),
            tool: "fs.read".into(),
            risk_class: "read_only".into(),
            folder_root: Some("/w".into()),
            constraints: "{}".into(),
            granted_at_ms: None,
        },
        LogRecordV1::MigrationAlias {
            source_id: "a".into(),
            old_ref: "appr-grants-0000".into(),
            new_ref: "appr-migrated-a-0".into(),
        },
        LogRecordV1::ApprovalRecorded {
            approval_ref: "appr-migrated-b-0".into(),
            decision: "approved".into(),
            scope: "folder".into(),
            turn_id: String::new(),
            awili_call_id: String::new(),
            normalized_call_hash: "h3".into(),
            session_id: "grants".into(),
            tool: "fs.read".into(),
            risk_class: "read_only".into(),
            folder_root: Some("/w".into()),
            constraints: "{}".into(),
            granted_at_ms: None,
        },
        LogRecordV1::MigrationAlias {
            source_id: "b".into(),
            old_ref: "appr-grants-0000".into(),
            new_ref: "appr-migrated-b-0".into(),
        },
        // The revocation names the OLD ref — ambiguous (two aliases).
        LogRecordV1::GrantRevoked {
            approval_ref: "appr-grants-0000".into(),
        },
    ];
    let recs = mk_intent("intent-2", "appr-migrated-a-0", "h2", "fs.read");
    let out = classify_with_grants(&recs, &grant_records_ambig);
    let st = &out[0].intents[0].status;
    match st {
        IntentStatus::AuthzInvalid { reason } => {
            assert!(
                reason.contains("ambiguous") || reason.contains("could not be resolved"),
                "ambiguous revocation must fail closed: {reason}"
            );
        }
        other => panic!("ambiguous revocation must be AuthzInvalid, got {other:?}"),
    }
}

/// Another writer appending to the same file (a second process in
/// production; a raw handle here) is detected by the length change and the
/// reference sequence re-syncs — refs stay unique across handles (gate
/// row Q's same-file half; the flock serializes true cross-process races).
#[test]
fn reference_sequence_resyncs_after_foreign_append() {
    let root = tmpdir("resync");
    let store = root.join("grants.log");
    let svc = GrantStoreService::init(&store, None).unwrap();
    let cap = crate::authz::HostApprovalCapability::test_only();
    let r1 = svc
        .with_store(|log| {
            log.record_approval(
                &cap,
                "always",
                "t1",
                &call("fs.read", "{}"),
                RiskClass::ReadOnly,
                None,
                "{}",
            )
        })
        .unwrap();
    // Foreign writer: a raw ExecutionLog on the same path (bypasses the
    // service, as another process would).
    let mut foreign = ExecutionLog::open_grant_store(FileLog::open(&store).unwrap()).unwrap();
    let r2 = foreign
        .record_approval(
            &cap,
            "always",
            "t2",
            &call("fs.glob", "{}"),
            RiskClass::ReadOnly,
            None,
            "{}",
        )
        .unwrap();
    // The service's next section must observe the append and allocate PAST it.
    let r3 = svc
        .with_store(|log| {
            log.record_approval(
                &cap,
                "always",
                "t3",
                &call("fs.grep", "{}"),
                RiskClass::ReadOnly,
                None,
                "{}",
            )
        })
        .unwrap();
    let refs: std::collections::BTreeSet<String> = [r1, r2, r3]
        .into_iter()
        .map(|p| match p {
            crate::authz::MintedProof::Grant(g) => g.approval_ref,
            crate::authz::MintedProof::Once(o) => o.approval_ref,
        })
        .collect();
    assert_eq!(refs.len(), 3, "all refs unique across handles: {refs:?}");
}

/// New refs minted in a MIGRATED store can never collide with an aliased
/// legacy old ref: open() advances the sequence past every alias old_ref,
/// so direct approvals-map hits stay unambiguous forever.
#[test]
fn new_refs_never_collide_with_aliased_legacy_refs() {
    let root = tmpdir("collide");
    let legacy = root.join("grants");
    fs::create_dir_all(&legacy).unwrap();
    let old = write_legacy(&legacy.join("aaaa.json"), &["fs.write", "fs.edit"], false);
    let store = root.join("grants.log");
    let svc = GrantStoreService::init(&store, Some(&legacy)).unwrap();
    let cap = crate::authz::HostApprovalCapability::test_only();
    let minted = svc
        .with_store(|log| {
            log.record_approval(
                &cap,
                "always",
                "t1",
                &call("fs.read", "{}"),
                RiskClass::ReadOnly,
                None,
                "{}",
            )
        })
        .unwrap();
    let new_ref = match minted {
        crate::authz::MintedProof::Grant(g) => g.approval_ref,
        crate::authz::MintedProof::Once(o) => o.approval_ref,
    };
    assert!(!old.contains(&new_ref), "no collision: {new_ref}");
    // Specifically: legacy max was appr-grants-0001, so the fresh mint must
    // be at 0002 or later.
    assert_eq!(new_ref, "appr-grants-0002");
}

/// The seam-facing trait: MemLog-backed logs (tests) and the service
/// (production) both satisfy it; the service returns the minted proof from
/// inside one locked critical section (r4 §3 interactive contract).
#[test]
fn service_handle_and_memlog_both_serve_the_grant_access_trait() {
    fn thru(access: &mut dyn GrantStoreAccess) -> String {
        let cap = crate::authz::HostApprovalCapability::test_only();
        let proof = access
            .record_grant_approval(
                &cap,
                "always",
                "t1",
                &call("fs.read", "{}"),
                RiskClass::ReadOnly,
                None,
                "{}",
            )
            .unwrap();
        let (recs, _) = access.read_grant_records().unwrap();
        assert!(recs
            .iter()
            .any(|r| matches!(r, LogRecordV1::ApprovalRecorded { .. })));
        match proof {
            crate::authz::MintedProof::Grant(g) => g.approval_ref,
            crate::authz::MintedProof::Once(o) => o.approval_ref,
        }
    }
    let mut mem = ExecutionLog::open_grant_store(MemLog::new()).unwrap();
    let r_mem = thru(&mut mem);
    let root = tmpdir("trait");
    let svc = GrantStoreService::init(root.join("grants.log"), None).unwrap();
    let mut handle = ServiceHandle(svc);
    let r_svc = thru(&mut handle);
    assert_eq!(r_mem, r_svc, "same first-ref sequence on a fresh store");
}

/// Crash surfaces around the staged migration: a stale tmp file from an
/// interrupted run is harmless (overwritten); a committed store (marker
/// inside the atomic unit) is never re-ingested.
#[test]
fn interrupted_staging_restarts_and_committed_store_never_reingests() {
    let root = tmpdir("crash");
    let legacy = root.join("grants");
    fs::create_dir_all(&legacy).unwrap();
    write_legacy(&legacy.join("aaaa.json"), &["fs.write"], false);
    let store = root.join("grants.log");
    // Simulated crash: a half-written stage from a previous attempt.
    fs::write(store.with_extension("log.tmp"), b"partial garbage").unwrap();
    GrantStoreService::init(&store, Some(&legacy)).unwrap();
    let n = read_store(&store).len();
    assert!(
        read_store(&store)
            .iter()
            .any(|r| matches!(r, LogRecordV1::StoreMigrated { .. })),
        "marker present"
    );
    // A second process arriving later (fresh registry in production terms)
    // sees the committed store and must not re-ingest: count is stable.
    GrantStoreService::init(&store, Some(&legacy)).unwrap();
    assert_eq!(read_store(&store).len(), n);
}

/// LogError surface check: opening a service on an unwritable path fails
/// loudly, never silently (fail-closed plumbing).
#[test]
fn unwritable_store_path_errors() {
    let out = GrantStoreService::init("/dev/null/impossible/grants.log", None);
    assert!(matches!(out, Err(LogError::Storage(_))));
}

/// Impl review r1 P1-3: a source mutated between staging and the
/// pre-rename re-verification discards the staged file and RESTARTS —
/// the committed store reflects the FINAL bytes, never the stale stage.
#[test]
fn source_mutated_during_migration_restarts_with_the_fresh_bytes() {
    let root = tmpdir("mutate");
    let legacy = root.join("grants");
    fs::create_dir_all(&legacy).unwrap();
    write_legacy(&legacy.join("aaaa.json"), &["fs.write"], false);
    let store = root.join("grants.log");
    let mutated = std::sync::atomic::AtomicBool::new(false);
    crate::grant_service::migrate_legacy_hooked(&legacy, &store, || {
        // First pass only: grow the source AFTER staging.
        if !mutated.swap(true, std::sync::atomic::Ordering::SeqCst) {
            write_legacy(&legacy.join("aaaa.json"), &["fs.edit"], false);
        }
    })
    .unwrap();
    let recs = read_store(&store);
    let tools: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::ApprovalRecorded { tool, .. } => Some(tool.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        vec!["fs.write", "fs.edit"],
        "the SECOND attempt migrated the final bytes: {tools:?}"
    );
}

/// Impl review r1 P1-3: an UNREADABLE legacy directory fails
/// initialization loudly — the store is never created over grants that
/// could not be read (which would foreclose re-ingestion forever).
#[test]
#[cfg(unix)]
fn unreadable_legacy_dir_fails_init_instead_of_skipping_grants() {
    use std::os::unix::fs::PermissionsExt;
    let root = tmpdir("unread");
    let legacy = root.join("grants");
    fs::create_dir_all(&legacy).unwrap();
    write_legacy(&legacy.join("aaaa.json"), &["fs.write"], false);
    let mut perm = fs::metadata(&legacy).unwrap().permissions();
    perm.set_mode(0o000);
    fs::set_permissions(&legacy, perm.clone()).unwrap();
    let store = root.join("grants.log");
    let out = GrantStoreService::init(&store, Some(&legacy));
    perm.set_mode(0o755);
    fs::set_permissions(&legacy, perm).unwrap();
    assert!(
        matches!(out, Err(LogError::Storage(_))),
        "init must fail loudly on an unreadable legacy dir"
    );
    assert!(!store.exists(), "no store over unreadable legacy grants");
}

/// Impl review r1 P1-4: a REAL second process appends through its own
/// GrantStoreService on the same store — the flock plus per-section
/// re-sync keep every reference unique and the store totally ordered.
/// The child half runs in a spawned copy of this test binary.
#[test]
fn a_real_second_process_appends_with_unique_references() {
    if std::env::var("AWILI_U5_MP_CHILD").is_ok() {
        return; // the child entry point below does the work
    }
    let root = tmpdir("mp");
    let store = root.join("grants.log");
    let svc = GrantStoreService::init(&store, None).unwrap();
    let cap = crate::authz::HostApprovalCapability::test_only();
    svc.with_store(|log| {
        log.record_approval(
            &cap,
            "always",
            "t1",
            &call("fs.read", "{}"),
            RiskClass::ReadOnly,
            None,
            "{}",
        )
    })
    .unwrap();
    // The CHILD: a fresh process, its own service, same path.
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(exe)
        .args([
            "grant_service_tests::multiprocess_child_appends_one_grant",
            "--exact",
            "--nocapture",
        ])
        .env("AWILI_U5_MP_CHILD", store.to_str().unwrap())
        .output()
        .expect("spawn child test process");
    assert!(
        out.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Parent again: must observe the child's append and allocate past it.
    let r3 = svc
        .with_store(|log| {
            log.record_approval(
                &cap,
                "always",
                "t3",
                &call("fs.grep", "{}"),
                RiskClass::ReadOnly,
                None,
                "{}",
            )
        })
        .unwrap();
    let recs = read_store(&store);
    let refs: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::ApprovalRecorded { approval_ref, .. } => Some(approval_ref.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(refs.len(), 3, "parent, child, parent: {refs:?}");
    let unique: std::collections::BTreeSet<_> = refs.iter().collect();
    assert_eq!(unique.len(), 3, "all references unique: {refs:?}");
    let r3 = match r3 {
        crate::authz::MintedProof::Grant(g) => g.approval_ref,
        crate::authz::MintedProof::Once(o) => o.approval_ref,
    };
    assert_eq!(refs.last().unwrap(), &r3);
}

/// The child half of the multi-process test: appends ONE grant through a
/// service of its own on the path named by AWILI_U5_MP_CHILD, then exits.
/// A no-op unless spawned by the parent above.
#[test]
fn multiprocess_child_appends_one_grant() {
    let Ok(path) = std::env::var("AWILI_U5_MP_CHILD") else {
        return;
    };
    let svc = GrantStoreService::init(std::path::PathBuf::from(path), None).unwrap();
    let cap = crate::authz::HostApprovalCapability::test_only();
    svc.with_store(|log| {
        log.record_approval(
            &cap,
            "always",
            "t2",
            &call("fs.glob", "{}"),
            RiskClass::ReadOnly,
            None,
            "{}",
        )
    })
    .unwrap();
}

/// RVS5C-FIX1 (review RVS5C-sp P1 frame.rs:64): an UNCERTAIN grant-store
/// tail — a complete header whose frame runs past EOF — is refused by the
/// repair path and left BYTE-IDENTICAL, by the next `with_store` section
/// and by a fresh `init`. Two shapes: an inflated length over junk and a
/// real frame torn after its header.
#[test]
fn uncertain_grant_tail_is_refused_and_left_byte_identical() {
    let mut inflated = Vec::new();
    inflated.extend_from_slice(&999u32.to_le_bytes());
    inflated.extend_from_slice(&0u32.to_le_bytes());
    inflated.extend_from_slice(&[1u8, 2, 3, 4, 5]);
    let real = frame::encode(b"never-finished");
    let shapes: [(&str, Vec<u8>); 2] = [
        ("inflated", inflated),
        ("half", real[..real.len() / 2].to_vec()),
    ];
    for (tag, tail) in shapes {
        let root = tmpdir(&format!("uncertain-{tag}"));
        let store = root.join("grants.log");
        let svc = GrantStoreService::init(&store, None).unwrap();
        let cap = crate::authz::HostApprovalCapability::test_only();
        svc.with_store(|log| {
            log.record_approval(
                &cap,
                "always",
                "t1",
                &call("fs.read", "{}"),
                RiskClass::ReadOnly,
                None,
                "{}",
            )
        })
        .unwrap();
        let mut f = fs::OpenOptions::new().append(true).open(&store).unwrap();
        use std::io::Write as _;
        f.write_all(&tail).unwrap();
        drop(f);
        let before = fs::read(&store).unwrap();
        // The next section refuses: no truncation, no append.
        match svc.with_store(|log| log.append_grant_revoked("appr-grants-0000")) {
            Err(LogError::CorruptTail) => {}
            other => panic!("{tag}: an uncertain tail must be refused, got {other:?}"),
        }
        assert_eq!(
            fs::read(&store).unwrap(),
            before,
            "{tag}: store left byte-identical"
        );
        // A fresh init over the same bytes refuses too (new path: the
        // service registry is per path).
        let copy = root.join("grants-copy.log");
        fs::write(&copy, &before).unwrap();
        match GrantStoreService::init(&copy, None) {
            Err(LogError::CorruptTail) => {}
            Ok(_) => panic!("{tag}: init over an uncertain tail must refuse"),
            Err(e) => panic!("{tag}: init must refuse with CorruptTail, got {e:?}"),
        }
        assert_eq!(
            fs::read(&copy).unwrap(),
            before,
            "{tag}: copy left byte-identical"
        );
    }
}

/// RVS5C-FIX1: a grant append that fails AFTER writing (FileLog torn-write
/// seam) poisons the service's handle, and a same-section retry is refused.
/// The NEXT section proves the torn bytes are exactly this handle's own
/// failed frame, truncates to the pre-append offset, reopens, and appends
/// cleanly. The failed revocation never becomes durable.
#[test]
fn failed_grant_append_poisons_then_next_section_rolls_back_own_bytes() {
    let root = tmpdir("ownroll");
    let store = root.join("grants.log");
    let svc = GrantStoreService::init(&store, None).unwrap();
    let cap = crate::authz::HostApprovalCapability::test_only();
    svc.with_store(|log| {
        log.record_approval(
            &cap,
            "always",
            "t1",
            &call("fs.read", "{}"),
            RiskClass::ReadOnly,
            None,
            "{}",
        )
    })
    .unwrap();
    let clean = fs::read(&store).unwrap();
    let retry = svc.with_store(|log| {
        log.storage_mut_for_tests().tear_next_append = Some(11);
        let first = log.append_grant_revoked("appr-grants-0000");
        assert!(matches!(first, Err(LogError::Storage(_))), "{first:?}");
        assert!(
            log.is_poisoned(),
            "a torn grant append must poison the handle"
        );
        log.append_grant_revoked("appr-grants-0000")
    });
    assert!(
        matches!(retry, Err(LogError::Poisoned { .. })),
        "same-handle retry must be refused: {retry:?}"
    );
    let torn = fs::read(&store).unwrap();
    assert_eq!(
        torn.len(),
        clean.len() + 11,
        "only the torn bytes, no retry frame"
    );
    assert_eq!(&torn[..clean.len()], &clean[..]);
    // Next section: proven rollback of our own bytes, reopen, append.
    svc.with_store(|log| {
        assert!(!log.is_poisoned(), "the section runs on a reopened handle");
        log.append_grant_revoked("appr-grants-0000")
    })
    .unwrap();
    let recs = read_store(&store);
    let revocations = recs
        .iter()
        .filter(|r| matches!(r, LogRecordV1::GrantRevoked { .. }))
        .count();
    assert_eq!(
        revocations, 1,
        "failed revocation rolled back; the retry landed once"
    );
    assert_eq!(&fs::read(&store).unwrap()[..clean.len()], &clean[..]);
}

/// RVS5C-FIX1: the rollback is PROOF-based. If the bytes after the failed
/// append's offset are not a prefix of the frame this handle tried to
/// write, nothing is truncated: the decoder sees an uncertain tail, the
/// section refuses, the store stays byte-identical, and it keeps refusing.
#[test]
fn rollback_refuses_bytes_that_are_not_our_failed_append() {
    let root = tmpdir("foreignroll");
    let store = root.join("grants.log");
    let svc = GrantStoreService::init(&store, None).unwrap();
    let clean_len = fs::metadata(&store).unwrap().len();
    let failed = svc.with_store(|log| {
        log.storage_mut_for_tests().tear_next_append = Some(11);
        log.append_grant_revoked("appr-grants-0000")
    });
    assert!(matches!(failed, Err(LogError::Storage(_))), "{failed:?}");
    // Replace our 11 torn bytes with 11 bytes that are NOT ours.
    let f = fs::OpenOptions::new().write(true).open(&store).unwrap();
    f.set_len(clean_len).unwrap();
    drop(f);
    let mut junk = Vec::new();
    junk.extend_from_slice(&999u32.to_le_bytes());
    junk.extend_from_slice(&0u32.to_le_bytes());
    junk.extend_from_slice(&[7u8, 7, 7]);
    let mut f = fs::OpenOptions::new().append(true).open(&store).unwrap();
    use std::io::Write as _;
    f.write_all(&junk).unwrap();
    drop(f);
    let before = fs::read(&store).unwrap();
    match svc.with_store(|log| log.append_grant_revoked("appr-grants-0001")) {
        Err(LogError::CorruptTail) => {}
        other => panic!("foreign uncertain bytes must be refused, got {other:?}"),
    }
    assert_eq!(
        fs::read(&store).unwrap(),
        before,
        "nothing truncated or appended"
    );
    assert!(matches!(
        svc.with_store(|log| log.read_records()),
        Err(LogError::CorruptTail)
    ));
    assert_eq!(fs::read(&store).unwrap(), before);
}

/// RC2-ENG2 (F0566): a legacy source whose metadata cannot be read (here a
/// symlink that loops on itself) fails initialization instead of being
/// skipped — a skipped source would never be migrated (the marker forecloses
/// re-ingestion). A dangling link (nothing to read) is still skipped.
#[test]
#[cfg(unix)]
fn rc2eng2_an_unreadable_legacy_source_fails_init_instead_of_skipping() {
    let root = tmpdir("rc2eng2-loop");
    let legacy = root.join("grants");
    fs::create_dir_all(&legacy).unwrap();
    write_legacy(&legacy.join("aaaa.json"), &["fs.write"], false);
    std::os::unix::fs::symlink("loop.json", legacy.join("loop.json")).unwrap();
    let store = root.join("grants.log");
    let err = GrantStoreService::init(&store, Some(&legacy))
        .err()
        .expect("a looping source must fail init");
    assert!(
        matches!(&err, LogError::Storage(m) if m.contains("loop.json")),
        "{err:?}"
    );
    assert!(!store.exists(), "no store is sealed over a skipped source");

    // A dangling link names nothing: skipped, and the real source migrates.
    let root = tmpdir("rc2eng2-dangling");
    let legacy = root.join("grants");
    fs::create_dir_all(&legacy).unwrap();
    write_legacy(&legacy.join("aaaa.json"), &["fs.write"], false);
    std::os::unix::fs::symlink("gone.json", legacy.join("dangling.json")).unwrap();
    let store = root.join("grants.log");
    GrantStoreService::init(&store, Some(&legacy)).unwrap();
    assert!(read_store(&store)
        .iter()
        .any(|r| matches!(r, LogRecordV1::ApprovalRecorded { tool, .. } if tool == "fs.write")));
}
