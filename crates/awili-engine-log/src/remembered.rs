//! S6: the owner-facing view of remembered approvals. Read-only listing of
//! every Always / Folder grant in the per-user grant store plus every live
//! Session grant, and revocation of each.
//!
//! Listing never authorizes anything and revocation only appends the same
//! `grant_revoked` record the authorizer's grant loader already honours
//! (`GrantSet::load`; a revoked ref is dropped wherever it appears), through
//! the grant-store service's own
//! locked critical section — so the next matching action asks again.
//!
//! Session grants live in the per-conversation session log, which only the
//! worker that owns it can append to. They are tracked here in memory for
//! the life of the process (a session ends with its worker) and revoked in
//! memory; `GrantSet::load` consults [`is_session_revoked`].

use std::collections::BTreeSet;
use std::sync::Mutex;

use crate::grant_service::GrantStoreService;
use crate::schema::LogRecordV1;
use crate::LogError;

/// How far a remembered approval reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantScopeKind {
    /// This conversation only, until it closes.
    Session,
    /// One project folder.
    Folder,
    /// Every project.
    Always,
}

/// One remembered approval, as the owner sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct RememberedGrant {
    /// The handle revocation takes. Not shown to the owner.
    pub approval_ref: String,
    /// Every ref of identical grants (same scope, tool, folder, constraints)
    /// collapsed into this one, `approval_ref` included. Revoking the grant
    /// revokes all of them.
    pub all_refs: Vec<String>,
    pub scope: GrantScopeKind,
    pub tool: String,
    pub risk_class: String,
    /// The folder a Folder grant covers.
    pub folder_root: Option<String>,
    /// Exact-match argument constraints (`{}` = any arguments).
    pub constraints: serde_json::Map<String, serde_json::Value>,
    /// When it was granted; `None` for approvals recorded before the time
    /// was kept.
    pub granted_at_ms: Option<u64>,
    /// Session grants only: the session that owns it.
    pub session_id: String,
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn from_record(rec: &LogRecordV1) -> Option<RememberedGrant> {
    let LogRecordV1::ApprovalRecorded {
        approval_ref,
        decision,
        scope,
        session_id,
        tool,
        risk_class,
        folder_root,
        constraints,
        granted_at_ms,
        ..
    } = rec
    else {
        return None;
    };
    if decision != "approved" {
        return None;
    }
    let scope = match scope.as_str() {
        "session" => GrantScopeKind::Session,
        "folder" => GrantScopeKind::Folder,
        "always" => GrantScopeKind::Always,
        _ => return None,
    };
    // Same fail-closed rule as the grant loader: unreadable constraints hold
    // no grant, so there is nothing to show.
    let constraints = serde_json::from_str::<serde_json::Value>(constraints)
        .ok()?
        .as_object()
        .cloned()?;
    Some(RememberedGrant {
        approval_ref: approval_ref.clone(),
        all_refs: vec![approval_ref.clone()],
        scope,
        tool: tool.clone(),
        risk_class: risk_class.clone(),
        folder_root: folder_root.clone(),
        constraints,
        granted_at_ms: *granted_at_ms,
        session_id: session_id.clone(),
    })
}

/// The un-revoked Folder / Always grants in the grant store's records, in
/// file order (oldest first).
pub fn stored_grants_from_records(records: &[LogRecordV1]) -> Vec<RememberedGrant> {
    let revoked: BTreeSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            LogRecordV1::GrantRevoked { approval_ref } => Some(approval_ref.as_str()),
            _ => None,
        })
        .collect();
    records
        .iter()
        .filter_map(from_record)
        .filter(|g| g.scope != GrantScopeKind::Session)
        .filter(|g| !revoked.contains(g.approval_ref.as_str()))
        .collect()
}

static SESSION_GRANTS: Mutex<Vec<RememberedGrant>> = Mutex::new(Vec::new());
static SESSION_REVOKED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Called by the log writer once a session-scope approval is durable.
pub(crate) fn note_session_grant(rec: &LogRecordV1) {
    if let Some(g) = from_record(rec) {
        if g.scope == GrantScopeKind::Session {
            lock(&SESSION_GRANTS).push(g);
        }
    }
}

/// The session ended (its worker stopped): its grants no longer apply and
/// are no longer listed.
pub fn forget_session(session_id: &str) {
    let mut grants = lock(&SESSION_GRANTS);
    let mut revoked = lock(&SESSION_REVOKED);
    for g in grants.iter().filter(|g| g.session_id == session_id) {
        revoked.remove(&g.approval_ref);
    }
    grants.retain(|g| g.session_id != session_id);
    // Refs are `appr-<session>-NNNN`: drop any note for this session even
    // when its grants were never registered here.
    let prefix = format!("appr-{session_id}-");
    revoked.retain(|r| !r.starts_with(&prefix));
}

/// Live Session grants that have not been revoked.
pub fn session_grants() -> Vec<RememberedGrant> {
    let revoked = lock(&SESSION_REVOKED);
    lock(&SESSION_GRANTS)
        .iter()
        .filter(|g| !revoked.contains(&g.approval_ref))
        .cloned()
        .collect()
}

/// Revoke one live Session grant in memory. `false` when no such live grant.
pub fn revoke_session_grant(approval_ref: &str) -> bool {
    let live = lock(&SESSION_GRANTS)
        .iter()
        .any(|g| g.approval_ref == approval_ref);
    live && lock(&SESSION_REVOKED).insert(approval_ref.to_string())
}

/// Whether a Session grant was revoked in memory. Consulted by the grant
/// loader; pure in-memory read.
pub fn is_session_revoked(approval_ref: &str) -> bool {
    lock(&SESSION_REVOKED).contains(approval_ref)
}

/// Live Session grants alone, newest first - the list when no grant store
/// exists yet.
pub fn list_session_only() -> Vec<RememberedGrant> {
    newest_first(session_grants().into_iter().enumerate().collect())
}

/// Newest first, then identical grants (same scope, tool, risk, folder,
/// session and constraints - recorded more than once) collapsed into the
/// newest one, which carries every ref so Revoke takes them all back.
fn newest_first(mut all: Vec<(usize, RememberedGrant)>) -> Vec<RememberedGrant> {
    all.sort_by(|(ia, a), (ib, b)| {
        b.granted_at_ms
            .cmp(&a.granted_at_ms)
            .then_with(|| ib.cmp(ia))
    });
    let mut out: Vec<RememberedGrant> = Vec::new();
    let mut seen: std::collections::BTreeMap<String, usize> = Default::default();
    for (_, g) in all {
        let key = format!(
            "{:?}|{}|{}|{:?}|{}|{}",
            g.scope,
            g.tool,
            g.risk_class,
            g.folder_root,
            if g.scope == GrantScopeKind::Session {
                g.session_id.as_str()
            } else {
                ""
            },
            serde_json::Value::Object(g.constraints.clone()),
        );
        match seen.get(&key) {
            Some(&i) => out[i].all_refs.extend(g.all_refs),
            None => {
                seen.insert(key, out.len());
                out.push(g);
            }
        }
    }
    out
}

impl GrantStoreService {
    /// Every remembered approval: stored Folder / Always grants and live
    /// Session grants, newest first, identical grants collapsed. Grants with
    /// no recorded time sort after the ones with one, later-in-file first.
    pub fn list_remembered(&self) -> Result<Vec<RememberedGrant>, LogError> {
        let (records, _) = self.with_store(|log| log.read_records())?;
        let mut all: Vec<(usize, RememberedGrant)> = stored_grants_from_records(&records)
            .into_iter()
            .enumerate()
            .collect();
        let base = all.len();
        all.extend(
            session_grants()
                .into_iter()
                .enumerate()
                .map(|(i, g)| (base + i, g)),
        );
        Ok(newest_first(all))
    }

    /// Revoke one remembered approval and every identical one collapsed into
    /// it. See [`Self::revoke_refs`].
    pub fn revoke_grant(&self, g: &RememberedGrant) -> Result<bool, LogError> {
        self.revoke_refs(&g.all_refs)
    }

    /// Revoke one remembered approval by ref.
    pub fn revoke_remembered(&self, approval_ref: &str) -> Result<bool, LogError> {
        self.revoke_refs(&[approval_ref.to_string()])
    }

    /// Revoke each ref. A Session grant is revoked in memory; a Folder /
    /// Always grant durably, by appending `grant_revoked` inside the store's
    /// locked critical section. `Ok(false)` when none of them was currently
    /// granted (already revoked, or unknown).
    pub fn revoke_refs(&self, refs: &[String]) -> Result<bool, LogError> {
        let mut any = false;
        let mut stored: Vec<&String> = Vec::new();
        for r in refs {
            if revoke_session_grant(r) {
                any = true;
            } else {
                stored.push(r);
            }
        }
        if stored.is_empty() {
            return Ok(any);
        }
        self.with_store(|log| {
            let (records, _) = log.read_records()?;
            let active = stored_grants_from_records(&records);
            for r in stored {
                if active.iter().any(|g| g.approval_ref == *r) {
                    log.append_grant_revoked(r)?;
                    any = true;
                }
            }
            Ok(any)
        })
    }
}

#[cfg(test)]
pub(crate) fn mark_revoked_for_tests(approval_ref: &str) {
    lock(&SESSION_REVOKED).insert(approval_ref.to_string());
}
