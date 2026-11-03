//! C2-u4: builtin fs tool DISPATCH — the C2-owned IO adapter.
//!
//! `fs.read/write/edit/glob/grep` execute through the C3 [`Sandbox`],
//! minted per-call from `AuthzEvidence::scope_root` via
//! `sandbox_for_evidence` — never from a raw pathname. Build plan §u4
//! acceptance: a dispatch that tries to escape is refused by C3 and lands
//! as `dispatch_finalized(ok=false)`.
//!
//! # Shape (Codex u4 design findings 1+2, hardened in code round 2)
//!
//! [`run_builtin_fs`] is the ONLY public entry point and owns the whole
//! sequence: mint the sandbox from `&AuthzEvidence` BEFORE `intent()`
//! consumes the evidence (a refusal is CARRIED, not thrown), write the
//! intent, then drive `dispatch_started` → execute-or-refuse → EXACTLY
//! ONE `dispatch_finalized`, deriving the `ToolResultRecord` from the
//! same result. The prepared value and the ticket are bound
//! STRUCTURALLY — both derive from the same `call` inside the function,
//! so no mispairing API exists (a fingerprint gate cannot distinguish
//! two calls with identical name and arguments; round-2 P1). There is
//! no public outcome type for engine glue to forget to finalize.
//!
//! Honest limits: a B2 append failure is a storage failure (`Err(LogError)`)
//! — no durable finalization can be promised when the log itself cannot be
//! written. A process crash between started and finalized replays
//! `Uncertain`, which is the truth (existing kill-point story, unchanged).
//!
//! Builtins never enter the MCP runtime: no ToolOrigin epochs, no session
//! machinery, no cancellation window (execution is local and synchronous;
//! pre-dispatch cancellation is simply "don't dispatch").

use awili_engine::events::{AwiliCallId, ToolCall, ToolResultRecord};
use awili_engine_log::authz::AuthzEvidence;
use awili_engine_log::storage::AppendOnlyLog;
use awili_engine_log::writer::IntentOutcome;
use awili_engine_log::writer::{ExecutionLog, IntentTicket};
use awili_engine_log::LogError;
use awili_engine_sandbox::{sandbox_for_evidence, Limits, Sandbox, SandboxError};
use awili_tools::builtins::{BuiltinFsCall, BuiltinFsParseError};

/// Bounded diagnostic strings (matches B2's own diagnostic bounding).
const MAX_REASON_BYTES: usize = 300;

fn bounded(mut s: String) -> String {
    if s.len() > MAX_REASON_BYTES {
        s.truncate(
            (0..=MAX_REASON_BYTES)
                .rev()
                .find(|&i| s.is_char_boundary(i))
                .unwrap_or(0),
        );
    }
    s
}

enum Inner {
    /// Parse succeeded and C3 minted a call-bound sandbox.
    Ready { sandbox: Sandbox, op: BuiltinFsCall },
    /// Preparation was refused. Carried — not thrown — so the refusal
    /// still travels through dispatch and FINALIZES as ok=false.
    Refused { reason: String },
}

/// A single-use prepared builtin fs call. Constructible only through
/// [`PreparedBuiltinFs::prepare`]; consumable only through
/// [`PreparedBuiltinFs::dispatch`]. Holds no persistent state — the
/// call-local sandbox dies with it.
pub(crate) struct PreparedBuiltinFs {
    inner: Inner,
    awili_call_id: AwiliCallId,
    limits: Limits,
}

/// How one builtin fs run ended, from B2's perspective.
#[derive(Debug)]
pub enum BuiltinRunOutcome {
    /// Full seam completed — intent → dispatch_started → exactly one
    /// dispatch_finalized — and the record derives from that same result
    /// (`is_error == !ok`). This includes every refusal: escapes,
    /// missing scope root, schema-invalid args, over-limit.
    Completed(ToolResultRecord),
    /// B2 refused the intent (malformed arguments): no ticket exists,
    /// nothing was dispatched, the intent is durably audited.
    NotDispatchable { intent_id: String, reason: String },
}

/// THE public entry point for builtin fs execution — the whole
/// C1→B2→C2→C3 seam in one owned sequence (Codex u4 round-2 P1: a
/// fingerprint gate cannot distinguish two calls with identical name and
/// arguments, so the prepared value and the ticket are now BOUND
/// STRUCTURALLY — both derive from the same `call` inside this function,
/// and no ticket or prepared value ever crosses a component boundary).
///
/// Order (round-1 P0 resolution, unchanged): the sandbox is minted from
/// `&evidence` BEFORE `intent()` consumes the evidence; the refusal, if
/// any, is CARRIED and still finalizes `ok=false`.
///
/// `Err(LogError)` means B2 itself failed — either the evidence does not
/// cover `(turn_id, call)` (write-time rejection; nothing recorded) or
/// storage failed (no durable claim possible; replay tells the truth).
///
/// RC2-R2G1: `secrets` says which files a search (`fs.glob`, `fs.grep`)
/// may show: a secret file the owner did not allow on this call's card is
/// left out (named nowhere, never opened).
pub fn run_builtin_fs<S: AppendOnlyLog>(
    log: &mut ExecutionLog<S>,
    turn_id: &str,
    call: &ToolCall,
    evidence: AuthzEvidence,
    limits: Limits,
    secrets: SecretFiles<'_>,
) -> Result<BuiltinRunOutcome, LogError> {
    let prepared = PreparedBuiltinFs::prepare_with_limits(&evidence, turn_id, call, limits);
    let ticket = match log.intent(turn_id, call, evidence)? {
        IntentOutcome::Dispatchable(t) => t,
        IntentOutcome::NotDispatchable { intent_id, reason } => {
            return Ok(BuiltinRunOutcome::NotDispatchable { intent_id, reason })
        }
    };
    prepared
        .dispatch(log, ticket, secrets)
        .map(BuiltinRunOutcome::Completed)
}

impl PreparedBuiltinFs {
    /// Prepare a builtin fs call against C1 evidence. TOTAL: every failure
    /// (unknown tool, schema-invalid args, no scope root, evidence/call
    /// mismatch, unopenable root) becomes a carried `Refused`, never an
    /// error — refusals must reach `dispatch_finalized(ok=false)`.
    ///
    /// Called BEFORE `ExecutionLog::intent` consumes the evidence —
    /// [`run_builtin_fs`] owns that ordering.
    #[cfg(test)]
    pub(crate) fn prepare(evidence: &AuthzEvidence, turn_id: &str, call: &ToolCall) -> Self {
        Self::prepare_with_limits(evidence, turn_id, call, Limits::default())
    }

    /// `prepare` with explicit resource limits.
    pub(crate) fn prepare_with_limits(
        evidence: &AuthzEvidence,
        turn_id: &str,
        call: &ToolCall,
        limits: Limits,
    ) -> Self {
        let awili_call_id = call.awili_call_id.clone();
        let op = match BuiltinFsCall::parse(&call.name, &call.arguments) {
            Ok(op) => op,
            Err(e @ BuiltinFsParseError::MalformedJson { .. }) => {
                // Behind B2 this is unreachable: C1 refuses to mint
                // evidence for malformed arguments, and B2's intent()
                // classifies them NotDispatchable. Kept total as defense
                // in depth.
                return Self::refused(awili_call_id, limits, e.to_string());
            }
            Err(e) => return Self::refused(awili_call_id, limits, e.to_string()),
        };
        // The ONLY way a sandbox is minted: from the authorization's own
        // scope root, bound to THIS call (C1→C3 handshake, contract v2 §6).
        match sandbox_for_evidence(evidence, turn_id, call) {
            Ok(sandbox) => Self {
                inner: Inner::Ready { sandbox, op },
                awili_call_id,
                limits,
            },
            Err(e) => Self::refused(awili_call_id, limits, e.to_string()),
        }
    }

    fn refused(awili_call_id: AwiliCallId, limits: Limits, reason: String) -> Self {
        Self {
            inner: Inner::Refused {
                reason: bounded(reason),
            },
            awili_call_id,
            limits,
        }
    }

    /// Whether preparation was refused (the dispatch will finalize
    /// `ok=false` without touching the filesystem).
    #[cfg(test)]
    pub(crate) fn is_refused(&self) -> bool {
        matches!(self.inner, Inner::Refused { .. })
    }

    /// Execute (or surface the carried refusal) with structural
    /// finalization: `dispatch_started` → run → exactly one
    /// `dispatch_finalized(ok, content)` → `ToolResultRecord` derived from
    /// the SAME result. Consumes self: one prepared call, one dispatch.
    ///
    /// `Err(LogError)` means B2 itself could not be written (storage
    /// failure) — no durable claim either way; replay tells the truth.
    ///
    /// PRIVATE (round-2 P1): the ticket/prepared pairing is structural —
    /// both are created from the same `call` inside [`run_builtin_fs`],
    /// the only caller. No fingerprint gate can distinguish two calls
    /// with identical name and arguments, so no external pairing API
    /// exists to misuse.
    fn dispatch<S: AppendOnlyLog>(
        self,
        log: &mut ExecutionLog<S>,
        ticket: IntentTicket,
        secrets: SecretFiles<'_>,
    ) -> Result<ToolResultRecord, LogError> {
        log.dispatch_started(&ticket)?;
        let (ok, content) = match self.inner {
            Inner::Refused { reason } => (false, reason),
            Inner::Ready { sandbox, op } => run_op(&sandbox, op, &self.limits, secrets),
        };
        log.dispatch_finalized(&ticket, ok, &content)?;
        Ok(ToolResultRecord {
            awili_call_id: self.awili_call_id,
            content,
            is_error: !ok,
        })
    }
}

/// RC2-R2G1 (RC2-R2F review P1: a search read and named secret files
/// beneath an ordinary folder without asking): which files a search may
/// show. A file the search finds that `is_secret` names is shown only when
/// it is in `allowed`: the secret files the owner's card for this call
/// named ([`secret_files_reached`], found before the call was authorized).
#[derive(Clone, Copy)]
pub struct SecretFiles<'a> {
    /// Whether a name the search finds is a secret file.
    pub is_secret: fn(&str) -> bool,
    /// The secret files the owner allowed for this call.
    pub allowed: &'a [String],
}

fn no_secret(_: &str) -> bool {
    false
}

impl SecretFiles<'static> {
    /// No name counts as a secret file (a caller with no secret test).
    pub const NONE: SecretFiles<'static> = SecretFiles {
        is_secret: no_secret,
        allowed: &[],
    };
}

impl SecretFiles<'_> {
    /// RC2-R2K (RC2-R2F review P1 on 55d863a3: a search run from a secret
    /// folder saw `config`, not `/Users/x/.ssh/config`, and showed it): a
    /// found file is judged, and allowed, by its full path.
    fn shows(&self, sandbox: &Sandbox, rel: &str) -> bool {
        let full = full_path(sandbox, rel);
        !(self.is_secret)(&full) || self.allowed.contains(&full)
    }
}

/// RC2-R2G1: the secret files (`is_secret`) a search call would show if
/// it ran now against `root`: the files its sandbox walk finds, by full
/// path (RC2-R2K: judged by the full path, so a search run from within a
/// secret folder is caught). Empty for any other call, and when the search
/// cannot run (the run then fails the same way, showing nothing).
pub fn secret_files_reached(
    root: &str,
    call: &ToolCall,
    is_secret: fn(&str) -> bool,
) -> Vec<String> {
    let Ok(sandbox) = Sandbox::open_root(std::path::Path::new(root)) else {
        return Vec::new();
    };
    let found = match BuiltinFsCall::parse(&call.name, &call.arguments) {
        Ok(BuiltinFsCall::Glob { pattern }) => sandbox.glob_bounded(&pattern, &Limits::default()),
        Ok(BuiltinFsCall::Grep { under, .. }) => sandbox.files_bounded(&under, &Limits::default()),
        _ => return Vec::new(),
    };
    found
        .unwrap_or_default()
        .into_iter()
        .map(|rel| full_path(&sandbox, &rel))
        .filter(|full| is_secret(full))
        .collect()
}

/// RC2-R2G1: what a search says about the secret files it left out.
fn left_out(n: usize) -> Option<String> {
    (n > 0).then(|| {
        format!("{n} secret file(s) left out: the owner has not allowed them for this search")
    })
}

/// RC2-R2F7: `rel` (beneath the sandbox's root) as a full path. RC2-R2G3:
/// a name the walk already gives in full (the search named its folder by
/// its full path) is kept as it is, never joined to the root a second time.
fn full_path(sandbox: &Sandbox, rel: &str) -> String {
    if rel.starts_with('/') {
        return rel.to_string();
    }
    format!("{}/{rel}", sandbox.root_display().trim_end_matches('/'))
}

/// RC2-R3 (usability RC2 round 3, Major 1: a failed read told the model
/// nothing it could act on, and it went looking with `find /`): why a read
/// failed, in plain words, for the two cases a model can fix itself \u{2014} no
/// file of that name in the folder, or a folder where a file was meant \u{2014}
/// naming the folder and how to list it. `None`: the sandbox's own reason
/// stands.
fn plain_read_failure(sandbox: &Sandbox, path: &str, e: &SandboxError) -> Option<String> {
    let folder = sandbox.root_display().trim_end_matches('/');
    let folder = if folder.is_empty() { "/" } else { folder };
    match e {
        SandboxError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            let says = |p: &str, f: &str| {
                format!(
                    "No file \"{p}\" in the folder {f}. List the folder with fs_glob (pattern \
                     \"*\") to see the names in it; the file is not searched for anywhere else."
                )
            };
            let (p, f) = fit_labels(says("", "").len(), path, folder);
            Some(says(&p, &f))
        }
        SandboxError::NotRegularFile(_) => {
            let pattern = format!("{}/*", path.trim_end_matches('/'));
            let says = |p: &str, f: &str, how: &str| {
                format!(
                    "\"{p}\" in the folder {f} is not a file (it may be a folder). List it with \
                     fs_glob ({how})."
                )
            };
            // The pattern goes whole or not at all: a cut one lists nothing.
            let how = format!("pattern \"{pattern}\"");
            let how = if says("", "", &how).len() + 2 * LABEL_FLOOR <= MAX_REASON_BYTES {
                how
            } else {
                "pattern: its path followed by \"/*\"".to_string()
            };
            let (p, f) = fit_labels(says("", "", &how).len(), path, folder);
            Some(says(&p, &f, &how))
        }
        _ => None,
    }
}

/// RC2-R3M7: the least room a path or folder label is cut to.
const LABEL_FLOOR: usize = 24;

/// RC2-R3M7 (RC2-R32 review P2: a long file name or Work folder pushed the
/// fs_glob instruction past [`MAX_REASON_BYTES`], so it was cut off): the
/// path and folder labels of a message whose other words take `fixed` bytes,
/// each cut from the front ("\u{2026}" and its end) so the whole message fits.
fn fit_labels(fixed: usize, path: &str, folder: &str) -> (String, String) {
    let room = MAX_REASON_BYTES.saturating_sub(fixed);
    if path.len() + folder.len() <= room {
        return (path.to_string(), folder.to_string());
    }
    let half = room / 2;
    let (p_max, f_max) = if path.len() <= half {
        (path.len(), room - path.len())
    } else if folder.len() <= half {
        (room - folder.len(), folder.len())
    } else {
        (half, room - half)
    };
    (short_label(path, p_max), short_label(folder, f_max))
}

/// RC2-R3M7: `s` in at most `max` bytes: whole, or "\u{2026}" and its end.
fn short_label(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub('\u{2026}'.len_utf8());
    let start = (s.len() - keep..=s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(s.len());
    format!("\u{2026}{}", &s[start..])
}

/// Run one parsed op inside the call-bound sandbox. Total: every
/// `SandboxError` — containment refusal, limit, or IO — is a failed
/// RESULT `(false, reason)`, never a thrown error. Success payloads are
/// bounded by `limits.max_result_bytes`, refusing (not truncating)
/// over-limit.
fn run_op(
    sandbox: &Sandbox,
    op: BuiltinFsCall,
    limits: &Limits,
    secrets: SecretFiles<'_>,
) -> (bool, String) {
    let result: Result<String, SandboxError> = match op {
        // STRICT UTF-8 (round-1 P1): a lossy conversion would hand the
        // model content that differs from the file while claiming an
        // exact successful read. Binary content is a failed dispatch
        // until a lossless encoding is specified for the tool surface.
        BuiltinFsCall::Read { path } => match sandbox.read_bounded(&path, limits) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Ok(text),
                Err(_) => {
                    return (
                        false,
                        format!(
                            "{path}: content is not valid UTF-8; binary reads are not supported"
                        ),
                    )
                }
            },
            Err(e) => match plain_read_failure(sandbox, &path, &e) {
                Some(words) => return (false, bounded(words)),
                None => Err(e),
            },
        },
        BuiltinFsCall::Write { path, contents } => sandbox
            .write_bounded(&path, contents.as_bytes(), limits)
            .map(|()| format!("wrote {} bytes to {path}", contents.len())),
        BuiltinFsCall::Edit {
            path,
            find,
            replace,
        } => sandbox
            .edit_bounded(&path, find.as_bytes(), replace.as_bytes(), limits)
            .map(|n| format!("{n} replacement(s) in {path}")),
        // RC2-R2F7: a full-path pattern names its files by their full paths
        // (the root may be that path's own folder, not the session's).
        BuiltinFsCall::Glob { pattern } => sandbox.glob_bounded(&pattern, limits).map(|paths| {
            // RC2-R2G1: a secret file the owner did not allow is not named.
            let (paths, hidden): (Vec<String>, Vec<String>) =
                paths.into_iter().partition(|p| secrets.shows(sandbox, p));
            let paths: Vec<String> = if pattern.starts_with('/') {
                paths.iter().map(|p| full_path(sandbox, p)).collect()
            } else {
                paths
            };
            let mut out = serde_json::json!({ "paths": paths });
            if let Some(note) = left_out(hidden.len()) {
                out["left_out"] = serde_json::json!(note);
            }
            out.to_string()
        }),
        BuiltinFsCall::Grep { needle, under } => {
            let full = under.starts_with('/');
            // RC2-R2G1: a secret file the owner did not allow is not read.
            let hidden = std::cell::Cell::new(0usize);
            let keep = |rel: &str| {
                let shows = secrets.shows(sandbox, rel);
                if !shows {
                    hidden.set(hidden.get() + 1);
                }
                shows
            };
            sandbox
                .grep_bounded_where(&needle, &under, limits, &keep)
                .map(|report| {
                    let name = |rel: &str| {
                        if full {
                            full_path(sandbox, rel)
                        } else {
                            rel.to_string()
                        }
                    };
                    let mut out = serde_json::json!({
                        "matches": report
                            .matches
                            .iter()
                            .map(|m| serde_json::json!({
                                "rel": name(&m.rel), "line": m.line_no, "text": m.line
                            }))
                            .collect::<Vec<_>>(),
                        "skipped": report
                            .skipped
                            .iter()
                            .map(|(rel, why)| serde_json::json!([name(rel), why]))
                            .collect::<Vec<_>>(),
                    });
                    if let Some(note) = left_out(hidden.get()) {
                        out["left_out"] = serde_json::json!(note);
                    }
                    out.to_string()
                })
        }
    };
    match result {
        // Safety net over the in-op accumulation caps: serialization
        // overhead (JSON quoting/keys) on an already-bounded result.
        Ok(content) if content.len() > limits.max_result_bytes.saturating_mul(2) => (
            false,
            format!(
                "result exceeds {} bytes; narrow the request",
                limits.max_result_bytes.saturating_mul(2)
            ),
        ),
        Ok(content) => (true, content),
        Err(e) => (false, bounded(e.to_string())),
    }
}

#[cfg(test)]
mod tests;
