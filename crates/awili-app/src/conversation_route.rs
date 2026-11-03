//! Per-conversation next-turn routing and native-session continuity.
//!
//! This is deliberately state only. It reuses [`WorkExecutor`] for target
//! selection and existing run/approval/cancellation machinery for execution;
//! it does not create a second runner, transcript, or outcome ledger. The
//! integration seam snapshots [`ActiveTurn`] at dispatch and uses that snapshot
//! for the existing run sequence, Stop, approval and completion paths.

use awili_code::continuity::{
    bridge_plan, native_continuity, BridgePlan, BridgeRecord, NativeContinuity, TranscriptCursor,
};
use awili_code::CodeEngine;
use serde::{Deserialize, Serialize};

use crate::work::WorkExecutor;

/// The native-session namespace: a token is valid only for this harness and
/// this canonical cwd. The model is intentionally not part of the key; the
/// native CLI contract, not a chooser label, determines whether a model change
/// can resume this harness session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeSessionKey {
    pub(crate) harness: CodeEngine,
    pub(crate) cwd: String,
}

/// What one harness has already observed from the app-owned transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeSessionState {
    /// `None` is normal for Dsh, whose headless CLI has no resume token.
    pub(crate) native_id: Option<String>,
    pub(crate) seen_through: TranscriptCursor,
    pub(crate) model_at_last_native_turn: Option<String>,
    pub(crate) image_parts: bool,
}

/// A route's immutable native portion for one active turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveNativeTurn {
    pub(crate) key: NativeSessionKey,
    pub(crate) continuity: NativeContinuity,
    pub(crate) image_parts: bool,
}

impl ActiveNativeTurn {
    /// Build the sole provider input for this active native turn. Consumers
    /// must use both `plan.prompt` and `plan.resume_id()` from the returned
    /// value: an over-budget delta becomes `Fresh`, so an old native token
    /// cannot accompany its full bounded bridge.
    pub(crate) fn prepare_bridge(
        &self,
        canonical_records: &[BridgeRecord],
        current: &str,
        project: &str,
        capabilities: &[String],
        max_history_bytes: usize,
    ) -> BridgePlan {
        bridge_plan(
            if !self.image_parts
                && canonical_records
                    .iter()
                    .any(|r| r.content.contains("data:image/"))
            {
                // Legacy native history contains base64 as prose. Rebuild once
                // from canonical app records using typed image inputs.
                NativeContinuity::Fresh
            } else {
                self.continuity.clone()
            },
            canonical_records,
            current,
            project,
            capabilities,
            max_history_bytes,
        )
    }
}

/// The immutable owner for one active turn. Changing the chooser updates only
/// [`ConversationRoute::next_target`], never this structure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveTurn {
    pub(crate) conversation_id: String,
    /// The existing Work/Code run sequence supplied by the dispatcher.
    pub(crate) run_id: u64,
    pub(crate) target: WorkExecutor,
    pub(crate) cwd: String,
    pub(crate) native: Option<ActiveNativeTurn>,
}

/// Completion data supplied after the existing canonical bridge transcript is
/// appended. A regular completion can retain its native id; interruption
/// deliberately cannot, because a stopped/failed provider may have consumed a
/// partial turn that the app could not safely reconcile through `--resume`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeTurnFinish {
    Completed {
        native_id_after: Option<String>,
        transcript_through: TranscriptCursor,
    },
    Interrupted {
        transcript_through: TranscriptCursor,
    },
}

impl NativeTurnFinish {
    fn transcript_through(&self) -> TranscriptCursor {
        match self {
            Self::Completed {
                transcript_through, ..
            }
            | Self::Interrupted { transcript_through } => *transcript_through,
        }
    }
}

/// A turn cannot replace another active turn. The caller should use the
/// existing Stop path first, which invalidates that run's sequence/approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BeginTurnError {
    ActiveTurn,
}

/// Persisted route decoding never falls back to an ambient/global default.
/// The caller can render this error as an explicit unavailable saved choice and
/// retain the raw record for diagnosis or a later migration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RoutePersistenceError {
    Malformed,
    UnsupportedVersion(u32),
    UnsupportedTarget(String),
    UnsupportedHarness(String),
    InvalidSession(String),
}

impl std::fmt::Display for RoutePersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed => write!(f, "malformed saved conversation route"),
            Self::UnsupportedVersion(version) => {
                write!(
                    f,
                    "saved conversation route version {version} is unavailable"
                )
            }
            Self::UnsupportedTarget(target) => {
                write!(f, "saved next-turn target is unavailable: {target}")
            }
            Self::UnsupportedHarness(harness) => {
                write!(f, "saved native harness is unavailable: {harness}")
            }
            Self::InvalidSession(reason) => write!(f, "invalid saved native session: {reason}"),
        }
    }
}

const ROUTE_PERSISTENCE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRouteV1 {
    version: u32,
    next_target: String,
    native_sessions: Vec<PersistedNativeSessionV1>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedNativeSessionV1 {
    harness: String,
    cwd: String,
    native_id: Option<String>,
    seen_through: usize,
    model_at_last_native_turn: Option<String>,
    #[serde(default)]
    image_parts: bool,
}

/// Per-conversation selection and native session map. It is intentionally
/// small enough to be embedded in existing `CodeSession`/Workshop persistence
/// later; no new transcript or result history is maintained here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConversationRoute {
    next_target: WorkExecutor,
    native_sessions: Vec<(NativeSessionKey, NativeSessionState)>,
    active: Option<ActiveTurn>,
    /// Set only once Awili has handed the bridge to a native provider. A
    /// memory-gate wait owns the route but has not consumed the native session.
    active_native_started: bool,
}

impl ConversationRoute {
    pub(crate) fn new(next_target: WorkExecutor) -> Self {
        Self {
            next_target,
            native_sessions: Vec::new(),
            active: None,
            active_native_started: false,
        }
    }

    /// Compatibility seed for an old Code row with its sole executor/folder/
    /// session slot. No second session is invented, and its cursor begins at
    /// the already-restored transcript length so the next fresh bridge is safe.
    pub(crate) fn from_legacy(
        next_target: WorkExecutor,
        folder: Option<String>,
        session: Option<String>,
        restored_through: TranscriptCursor,
    ) -> Self {
        let mut route = Self::new(next_target);
        let Some(folder) = folder else {
            return route;
        };
        let Some((harness, model)) = agent_parts(&route.next_target) else {
            return route;
        };
        route.native_sessions.push((
            NativeSessionKey {
                harness,
                cwd: folder,
            },
            NativeSessionState {
                native_id: session,
                seen_through: restored_through,
                model_at_last_native_turn: model.map(str::to_string),
                image_parts: false,
            },
        ));
        route
    }

    /// Migrate one legacy helper value without silently substituting the
    /// global default. The workshop migration uses this for both
    /// `workshop:general_models` and per-thread helper metadata.
    pub(crate) fn from_legacy_persisted_target(
        raw_target: &str,
        folder: Option<String>,
        session: Option<String>,
        restored_through: TranscriptCursor,
    ) -> Result<Self, RoutePersistenceError> {
        Ok(Self::from_legacy(
            parse_persisted_target(raw_target)?,
            folder,
            session,
            restored_through,
        ))
    }

    /// Persist only next-turn state and native-session continuity. Active run
    /// ownership is intentionally omitted: after restart, existing run tokens,
    /// process handles and approvals cannot be made live again.
    pub(crate) fn to_persisted_json(&self) -> Result<String, RoutePersistenceError> {
        let next_target = persist_target(&self.next_target)?;
        let native_sessions = self
            .native_sessions
            .iter()
            .map(|(key, state)| {
                validate_session(key, state)?;
                // An in-flight child can have consumed a partial request that
                // is not represented in the saved transcript.  A restart has
                // no process/approval owner for it, so retaining its resume
                // token would join the next turn to unknown state.
                let native_id = if self.active_native_started
                    && self
                        .active
                        .as_ref()
                        .and_then(|active| active.native.as_ref())
                        .is_some_and(|active| active.key == *key)
                {
                    None
                } else {
                    state.native_id.clone()
                };
                Ok(PersistedNativeSessionV1 {
                    harness: persist_harness(key.harness).to_string(),
                    cwd: key.cwd.clone(),
                    native_id,
                    seen_through: state.seen_through.0,
                    model_at_last_native_turn: state.model_at_last_native_turn.clone(),
                    image_parts: state.image_parts,
                })
            })
            .collect::<Result<Vec<_>, RoutePersistenceError>>()?;
        serde_json::to_string(&PersistedRouteV1 {
            version: ROUTE_PERSISTENCE_VERSION,
            next_target,
            native_sessions,
        })
        .map_err(|_| RoutePersistenceError::Malformed)
    }

    /// Restore a complete, validated persisted route. This never reinstates an
    /// active turn; callers must start a new turn through the existing run
    /// sequence and provider adapters.
    pub(crate) fn from_persisted_json(raw: &str) -> Result<Self, RoutePersistenceError> {
        let persisted: PersistedRouteV1 =
            serde_json::from_str(raw).map_err(|_| RoutePersistenceError::Malformed)?;
        if persisted.version != ROUTE_PERSISTENCE_VERSION {
            return Err(RoutePersistenceError::UnsupportedVersion(persisted.version));
        }
        let mut route = Self::new(parse_persisted_target(&persisted.next_target)?);
        for session in persisted.native_sessions {
            let key = NativeSessionKey {
                harness: parse_persisted_harness(&session.harness)?,
                cwd: session.cwd,
            };
            let state = NativeSessionState {
                native_id: session.native_id,
                seen_through: TranscriptCursor(session.seen_through),
                model_at_last_native_turn: session.model_at_last_native_turn,
                image_parts: session.image_parts,
            };
            validate_session(&key, &state)?;
            if route.native_session(&key).is_some() {
                return Err(RoutePersistenceError::InvalidSession(
                    "duplicate harness/cwd entry".into(),
                ));
            }
            route.native_sessions.push((key, state));
        }
        Ok(route)
    }

    pub(crate) fn next_target(&self) -> &WorkExecutor {
        &self.next_target
    }

    /// Selection is next-turn-only. It does not touch `active`, native ids,
    /// approvals, cancellation, or provider state.
    pub(crate) fn set_next_target(&mut self, target: WorkExecutor) {
        self.next_target = target;
    }

    pub(crate) fn active(&self) -> Option<&ActiveTurn> {
        self.active.as_ref()
    }

    pub(crate) fn native_session(&self, key: &NativeSessionKey) -> Option<&NativeSessionState> {
        self.native_sessions
            .iter()
            .find_map(|(stored, state)| (stored == key).then_some(state))
    }

    /// Capture the next target before the caller appends the current user row.
    /// `cwd` must already be the canonical folder the provider will receive.
    pub(crate) fn begin_turn(
        &mut self,
        conversation_id: impl Into<String>,
        run_id: u64,
        cwd: impl Into<String>,
        canonical_transcript_before_turn: TranscriptCursor,
    ) -> Result<ActiveTurn, BeginTurnError> {
        self.begin_turn_for_target(
            conversation_id,
            run_id,
            cwd,
            canonical_transcript_before_turn,
            self.next_target.clone(),
        )
    }

    /// Freeze a target already accepted by the dispatcher without changing the
    /// conversation's next-turn choice. This is required for a run parked on
    /// an async preflight: the chooser may legitimately change while that
    /// preflight is pending, but it cannot retarget the accepted run.
    pub(crate) fn begin_turn_for_target(
        &mut self,
        conversation_id: impl Into<String>,
        run_id: u64,
        cwd: impl Into<String>,
        canonical_transcript_before_turn: TranscriptCursor,
        target: WorkExecutor,
    ) -> Result<ActiveTurn, BeginTurnError> {
        if self.active.is_some() {
            return Err(BeginTurnError::ActiveTurn);
        }
        let conversation_id = conversation_id.into();
        let cwd = cwd.into();
        let native = if let Some((harness, model)) = agent_parts(&target) {
            let key = NativeSessionKey {
                harness,
                cwd: cwd.clone(),
            };
            let state = self.session_mut(&key, canonical_transcript_before_turn);
            Some(ActiveNativeTurn {
                image_parts: state.image_parts,
                key,
                continuity: native_continuity(
                    harness,
                    state.native_id.as_deref(),
                    state.model_at_last_native_turn.as_deref(),
                    model,
                    state.seen_through,
                ),
            })
        } else {
            None
        };
        let active = ActiveTurn {
            conversation_id,
            run_id,
            target,
            cwd,
            native,
        };
        self.active = Some(active.clone());
        self.active_native_started = false;
        Ok(active)
    }

    /// The provider is about to receive this exact native bridge. From this
    /// point a restart or interruption must not reuse its old resume token.
    pub(crate) fn mark_native_started(&mut self, active: &ActiveTurn) -> bool {
        if self.active.as_ref() != Some(active) || active.native.is_none() {
            return false;
        }
        self.active_native_started = true;
        true
    }

    /// Drop an accepted-but-unspawned native owner. Its original native
    /// session has not observed the request, so retain its id and cursor.
    pub(crate) fn finish_unstarted_turn(&mut self, active: &ActiveTurn) -> bool {
        if self.active.as_ref() != Some(active) || self.active_native_started {
            return false;
        }
        self.active = None;
        self.active_native_started = false;
        true
    }

    /// Settle a turn only if it is still the route's active owner. This is the
    /// route half of the existing run-sequence late-event guard: a stale finish
    /// cannot advance another harness/cwd session's cursor or native id.
    pub(crate) fn finish_turn(
        &mut self,
        active: &ActiveTurn,
        finish: Option<NativeTurnFinish>,
    ) -> bool {
        if self.active.as_ref() != Some(active) {
            return false;
        }
        if let Some(native) = &active.native {
            // `None` means the provider failed before its terminal state was
            // reconciled.  Retain the transcript cursor already known to the
            // app, but never reuse that native id.
            let finish = finish.unwrap_or(NativeTurnFinish::Interrupted {
                transcript_through: self
                    .native_session(&native.key)
                    .map(|state| state.seen_through)
                    .unwrap_or_default(),
            });
            let state = self.session_mut(&native.key, finish.transcript_through());
            state.seen_through = finish.transcript_through();
            state.image_parts = true;
            state.model_at_last_native_turn =
                agent_parts(&active.target).and_then(|(_, model)| model.map(str::to_string));
            state.native_id = match finish {
                NativeTurnFinish::Completed {
                    native_id_after, ..
                } => native_id_after,
                NativeTurnFinish::Interrupted { .. } => None,
            };
        }
        self.active = None;
        self.active_native_started = false;
        true
    }

    /// Stop addresses this exact run owner. Existing dispatch code performs
    /// the actual process kill and approval void; this makes later completion
    /// callbacks stale while preserving the chosen next target.
    pub(crate) fn stop_turn(
        &mut self,
        active: &ActiveTurn,
        canonical_transcript_through: TranscriptCursor,
    ) -> bool {
        if self.active.as_ref() != Some(active) {
            return false;
        }
        if let Some(native) = &active.native {
            // No resumed native session can be trusted after a Stop. Keep the
            // entry and its canonical cursor for persistence/diagnostics, but
            // force the next turn to receive a complete bridge.
            let state = self.session_mut(&native.key, canonical_transcript_through);
            state.native_id = None;
            state.seen_through = canonical_transcript_through;
        }
        self.active = None;
        self.active_native_started = false;
        true
    }

    fn session_mut(
        &mut self,
        key: &NativeSessionKey,
        initial_cursor: TranscriptCursor,
    ) -> &mut NativeSessionState {
        if let Some(index) = self
            .native_sessions
            .iter()
            .position(|(stored, _)| stored == key)
        {
            return &mut self.native_sessions[index].1;
        }
        self.native_sessions.push((
            key.clone(),
            NativeSessionState {
                native_id: None,
                seen_through: initial_cursor,
                model_at_last_native_turn: None,
                image_parts: true,
            },
        ));
        &mut self.native_sessions.last_mut().expect("just pushed").1
    }
}

fn persist_target(target: &WorkExecutor) -> Result<String, RoutePersistenceError> {
    let raw = target.to_persist();
    (WorkExecutor::from_persist(&raw) == Some(target.clone()))
        .then_some(raw.clone())
        .ok_or(RoutePersistenceError::UnsupportedTarget(raw))
}

fn parse_persisted_target(raw: &str) -> Result<WorkExecutor, RoutePersistenceError> {
    WorkExecutor::from_persist(raw)
        .filter(|target| target.to_persist() == raw)
        .ok_or_else(|| RoutePersistenceError::UnsupportedTarget(raw.to_string()))
}

fn persist_harness(harness: CodeEngine) -> &'static str {
    match harness {
        CodeEngine::Claude => "claude",
        CodeEngine::Codex => "codex",
        CodeEngine::Glm => "glm",
        CodeEngine::Dsh => "dsh",
    }
}

fn parse_persisted_harness(raw: &str) -> Result<CodeEngine, RoutePersistenceError> {
    match raw {
        "claude" => Ok(CodeEngine::Claude),
        "codex" => Ok(CodeEngine::Codex),
        "glm" => Ok(CodeEngine::Glm),
        "dsh" => Ok(CodeEngine::Dsh),
        _ => Err(RoutePersistenceError::UnsupportedHarness(raw.to_string())),
    }
}

fn validate_session(
    key: &NativeSessionKey,
    state: &NativeSessionState,
) -> Result<(), RoutePersistenceError> {
    if key.cwd.is_empty() {
        return Err(RoutePersistenceError::InvalidSession("empty cwd".into()));
    }
    if state.native_id.as_deref().is_some_and(str::is_empty) {
        return Err(RoutePersistenceError::InvalidSession(
            "empty native id".into(),
        ));
    }
    if state
        .model_at_last_native_turn
        .as_deref()
        .is_some_and(str::is_empty)
    {
        return Err(RoutePersistenceError::InvalidSession("empty model".into()));
    }
    Ok(())
}

fn agent_parts(target: &WorkExecutor) -> Option<(CodeEngine, Option<&str>)> {
    match target {
        WorkExecutor::Agent { engine, model } => Some((*engine, model.as_deref())),
        WorkExecutor::InApp
        | WorkExecutor::InAppExtra { .. }
        | WorkExecutor::Gateway
        | WorkExecutor::LocalBuild { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(engine: CodeEngine, model: Option<&str>) -> WorkExecutor {
        WorkExecutor::Agent {
            engine,
            model: model.map(str::to_string),
        }
    }

    #[test]
    fn target_change_does_not_retarget_the_active_run() {
        let claude = agent(CodeEngine::Claude, Some("sonnet"));
        let codex = agent(CodeEngine::Codex, Some("gpt"));
        let mut route = ConversationRoute::new(claude.clone());
        let active = route
            .begin_turn("conversation", 7, "/repo", TranscriptCursor(2))
            .unwrap();
        route.set_next_target(codex.clone());
        assert_eq!(route.active(), Some(&active));
        assert_eq!(route.active().unwrap().target, claude);
        assert_eq!(route.next_target(), &codex);
        assert!(route.stop_turn(&active, TranscriptCursor(3)));
        let next = route
            .begin_turn("conversation", 8, "/repo", TranscriptCursor(3))
            .unwrap();
        assert_eq!(next.target, codex);
    }

    #[test]
    fn model_change_uses_the_same_harness_cwd_session() {
        let mut route = ConversationRoute::from_legacy(
            agent(CodeEngine::Claude, Some("old")),
            Some("/repo".into()),
            Some("claude-session".into()),
            TranscriptCursor(4),
        );
        route.set_next_target(agent(CodeEngine::Claude, Some("new")));
        let active = route
            .begin_turn("conversation", 1, "/repo", TranscriptCursor(4))
            .unwrap();
        assert!(matches!(
            active.native.as_ref().unwrap().continuity,
            NativeContinuity::Resume { .. }
        ));
        assert_eq!(active.native.as_ref().unwrap().key.cwd, "/repo");
    }

    #[test]
    fn a_to_b_to_a_resumes_a_with_each_intervening_record_once() {
        let mut route = ConversationRoute::new(agent(CodeEngine::Claude, Some("sonnet")));
        let a1 = route
            .begin_turn("conversation", 1, "/repo", TranscriptCursor(0))
            .unwrap();
        assert!(route.finish_turn(
            &a1,
            Some(NativeTurnFinish::Completed {
                native_id_after: Some("claude-a".into()),
                transcript_through: TranscriptCursor(2),
            }),
        ));
        route.set_next_target(agent(CodeEngine::Codex, Some("gpt")));
        let b = route
            .begin_turn("conversation", 2, "/repo", TranscriptCursor(2))
            .unwrap();
        assert!(route.finish_turn(
            &b,
            Some(NativeTurnFinish::Completed {
                native_id_after: Some("codex-b".into()),
                transcript_through: TranscriptCursor(4),
            }),
        ));
        route.set_next_target(agent(CodeEngine::Claude, Some("new-sonnet")));
        let a2 = route
            .begin_turn("conversation", 3, "/repo", TranscriptCursor(4))
            .unwrap();
        let records = [
            BridgeRecord::message("user", "A request"),
            BridgeRecord::message("assistant", "A outcome"),
            BridgeRecord::message("user", "B correction"),
            BridgeRecord::outcome("B artifact written"),
        ];
        let plan =
            a2.native
                .as_ref()
                .unwrap()
                .prepare_bridge(&records, "A follow-up", "/repo", &[], 4096);
        assert_eq!(plan.resume_id(), Some("claude-a"));
        assert_eq!(plan.prompt.matches("B correction").count(), 1);
        assert_eq!(plan.prompt.matches("B artifact written").count(), 1);
        assert!(!plan.prompt.contains("A request"));
    }

    #[test]
    fn active_native_bridge_pairs_an_oversized_delta_with_fresh_execution() {
        let target = agent(CodeEngine::Claude, Some("sonnet"));
        let mut route = ConversationRoute::from_legacy(
            target.clone(),
            Some("/repo".into()),
            Some("native-session".into()),
            TranscriptCursor(0),
        );
        let active = route
            .begin_turn("conversation", 1, "/repo", TranscriptCursor(0))
            .unwrap();
        let plan = active.native.as_ref().unwrap().prepare_bridge(
            &[BridgeRecord::outcome("\u{1f63a}".repeat(512))],
            "exact current request",
            "/repo",
            &[],
            96,
        );

        assert_eq!(plan.continuity, NativeContinuity::Fresh);
        assert_eq!(plan.resume_id(), None);
        assert!(plan.prompt.ends_with("exact current request"));
        assert_eq!(route.next_target(), &target);
        assert_eq!(
            route
                .native_session(&NativeSessionKey {
                    harness: CodeEngine::Claude,
                    cwd: "/repo".into(),
                })
                .unwrap()
                .native_id
                .as_deref(),
            Some("native-session")
        );
    }

    #[test]
    fn folder_change_cannot_reuse_another_cwd_native_id() {
        let mut route = ConversationRoute::from_legacy(
            agent(CodeEngine::Codex, None),
            Some("/one".into()),
            Some("thread-one".into()),
            TranscriptCursor(2),
        );
        let one = route
            .begin_turn("conversation", 1, "/one", TranscriptCursor(2))
            .unwrap();
        assert!(matches!(
            one.native.as_ref().unwrap().continuity,
            NativeContinuity::Resume { .. }
        ));
        assert!(route.stop_turn(&one, TranscriptCursor(3)));
        let two = route
            .begin_turn("conversation", 2, "/two", TranscriptCursor(2))
            .unwrap();
        assert_eq!(
            two.native.as_ref().unwrap().continuity,
            NativeContinuity::Fresh
        );
        assert!(route
            .native_session(&NativeSessionKey {
                harness: CodeEngine::Codex,
                cwd: "/one".into(),
            })
            .is_some());
    }

    #[test]
    fn stale_finish_cannot_advance_the_new_active_session() {
        let mut route = ConversationRoute::new(agent(CodeEngine::Claude, None));
        let old = route
            .begin_turn("conversation", 1, "/repo", TranscriptCursor(0))
            .unwrap();
        route.set_next_target(agent(CodeEngine::Codex, None));
        assert!(route.stop_turn(&old, TranscriptCursor(1)));
        let current = route
            .begin_turn("conversation", 2, "/repo", TranscriptCursor(1))
            .unwrap();
        assert!(!route.finish_turn(
            &old,
            Some(NativeTurnFinish::Completed {
                native_id_after: Some("wrong-session".into()),
                transcript_through: TranscriptCursor(99),
            }),
        ));
        assert!(route.finish_turn(
            &current,
            Some(NativeTurnFinish::Completed {
                native_id_after: Some("codex-thread".into()),
                transcript_through: TranscriptCursor(2),
            }),
        ));
        let state = route
            .native_session(&NativeSessionKey {
                harness: CodeEngine::Codex,
                cwd: "/repo".into(),
            })
            .unwrap();
        assert_eq!(state.native_id.as_deref(), Some("codex-thread"));
        assert_eq!(state.seen_through, TranscriptCursor(2));
    }

    #[test]
    fn old_row_without_an_agent_session_keeps_its_next_target_only() {
        let route = ConversationRoute::from_legacy(
            WorkExecutor::InApp,
            Some("/repo".into()),
            Some("ignored-native-id".into()),
            TranscriptCursor(6),
        );
        assert_eq!(route.next_target(), &WorkExecutor::InApp);
        assert!(route.native_sessions.is_empty());
        assert!(route.active().is_none());
    }

    #[test]
    fn persistence_round_trips_target_and_all_native_sessions_without_active_run() {
        let mut route = ConversationRoute::from_legacy(
            agent(CodeEngine::Claude, Some("sonnet")),
            Some("/one".into()),
            Some("claude-session".into()),
            TranscriptCursor(3),
        );
        route.set_next_target(agent(CodeEngine::Codex, Some("gpt")));
        let codex = route
            .begin_turn("conversation", 1, "/two", TranscriptCursor(4))
            .unwrap();
        assert!(route.finish_turn(
            &codex,
            Some(NativeTurnFinish::Completed {
                native_id_after: Some("codex-thread".into()),
                transcript_through: TranscriptCursor(6),
            }),
        ));
        let raw = route.to_persisted_json().unwrap();
        assert!(
            !raw.contains("run_id"),
            "active ownership is never persisted"
        );
        let restored = ConversationRoute::from_persisted_json(&raw).unwrap();
        assert_eq!(restored.next_target(), route.next_target());
        assert!(restored.active().is_none());
        assert_eq!(
            restored
                .native_session(&NativeSessionKey {
                    harness: CodeEngine::Claude,
                    cwd: "/one".into(),
                })
                .unwrap()
                .native_id
                .as_deref(),
            Some("claude-session")
        );
        let codex = restored
            .native_session(&NativeSessionKey {
                harness: CodeEngine::Codex,
                cwd: "/two".into(),
            })
            .unwrap();
        assert_eq!(codex.native_id.as_deref(), Some("codex-thread"));
        assert_eq!(codex.seen_through, TranscriptCursor(6));
        assert_eq!(codex.model_at_last_native_turn.as_deref(), Some("gpt"));
    }

    #[test]
    fn persistence_never_restores_an_inflight_owner() {
        let mut route = ConversationRoute::new(agent(CodeEngine::Claude, Some("sonnet")));
        let active = route
            .begin_turn("conversation", 44, "/repo", TranscriptCursor(2))
            .unwrap();
        let raw = route.to_persisted_json().unwrap();
        assert!(!raw.contains("conversation"));
        assert!(!raw.contains("run_id"));
        let restored = ConversationRoute::from_persisted_json(&raw).unwrap();
        assert!(restored.active().is_none());
        assert_eq!(restored.next_target(), &active.target);
        let session = restored
            .native_session(&NativeSessionKey {
                harness: CodeEngine::Claude,
                cwd: "/repo".into(),
            })
            .unwrap();
        assert_eq!(
            session.native_id, None,
            "active resume ids never survive restart"
        );
    }

    #[test]
    fn persisted_unknowns_are_explicitly_unavailable_not_defaulted() {
        assert_eq!(
            ConversationRoute::from_persisted_json(
                r#"{"version":2,"next_target":"in-app","native_sessions":[]}"#
            ),
            Err(RoutePersistenceError::UnsupportedVersion(2))
        );
        assert_eq!(
            ConversationRoute::from_persisted_json(
                r#"{"version":1,"next_target":"agent:missing","native_sessions":[]}"#
            ),
            Err(RoutePersistenceError::UnsupportedTarget(
                "agent:missing".into()
            ))
        );
        assert_eq!(
            ConversationRoute::from_persisted_json(
                r#"{"version":1,"next_target":"in-app","native_sessions":[{"harness":"missing","cwd":"/repo","native_id":null,"seen_through":0,"model_at_last_native_turn":null}]}"#
            ),
            Err(RoutePersistenceError::UnsupportedHarness("missing".into()))
        );
        assert_eq!(
            ConversationRoute::from_legacy_persisted_target(
                "agent:missing",
                None,
                None,
                TranscriptCursor(0),
            ),
            Err(RoutePersistenceError::UnsupportedTarget(
                "agent:missing".into()
            ))
        );
    }

    #[test]
    fn interrupted_turn_clears_only_its_native_id_and_keeps_other_entries() {
        let mut route = ConversationRoute::from_legacy(
            agent(CodeEngine::Claude, Some("sonnet")),
            Some("/one".into()),
            Some("claude-session".into()),
            TranscriptCursor(2),
        );
        route.set_next_target(agent(CodeEngine::Codex, None));
        let codex = route
            .begin_turn("conversation", 1, "/two", TranscriptCursor(3))
            .unwrap();
        assert!(route.finish_turn(
            &codex,
            Some(NativeTurnFinish::Completed {
                native_id_after: Some("codex-thread".into()),
                transcript_through: TranscriptCursor(5),
            }),
        ));
        route.set_next_target(agent(CodeEngine::Claude, Some("sonnet")));
        let claude = route
            .begin_turn("conversation", 2, "/one", TranscriptCursor(5))
            .unwrap();
        assert!(route.stop_turn(&claude, TranscriptCursor(6)));
        let claude_state = route
            .native_session(&NativeSessionKey {
                harness: CodeEngine::Claude,
                cwd: "/one".into(),
            })
            .unwrap();
        assert_eq!(claude_state.native_id, None);
        assert_eq!(claude_state.seen_through, TranscriptCursor(6));
        assert_eq!(
            route
                .native_session(&NativeSessionKey {
                    harness: CodeEngine::Codex,
                    cwd: "/two".into(),
                })
                .unwrap()
                .native_id
                .as_deref(),
            Some("codex-thread")
        );
        assert_eq!(
            route.next_target(),
            &agent(CodeEngine::Claude, Some("sonnet"))
        );
    }

    #[test]
    fn failed_native_turn_cannot_leave_a_resumable_id() {
        let mut route = ConversationRoute::from_legacy(
            agent(CodeEngine::Claude, None),
            Some("/repo".into()),
            Some("old-session".into()),
            TranscriptCursor(1),
        );
        let active = route
            .begin_turn("conversation", 1, "/repo", TranscriptCursor(1))
            .unwrap();
        assert!(route.finish_turn(
            &active,
            Some(NativeTurnFinish::Interrupted {
                transcript_through: TranscriptCursor(3),
            }),
        ));
        let state = route
            .native_session(&NativeSessionKey {
                harness: CodeEngine::Claude,
                cwd: "/repo".into(),
            })
            .unwrap();
        assert_eq!(state.native_id, None);
        assert_eq!(state.seen_through, TranscriptCursor(3));
    }

    #[test]
    fn missing_finish_is_an_interruption_not_a_stale_resume() {
        let mut route = ConversationRoute::from_legacy(
            agent(CodeEngine::Claude, None),
            Some("/repo".into()),
            Some("old-session".into()),
            TranscriptCursor(4),
        );
        let active = route
            .begin_turn("conversation", 7, "/repo", TranscriptCursor(4))
            .unwrap();
        assert!(route.finish_turn(&active, None));
        let state = route
            .native_session(&NativeSessionKey {
                harness: CodeEngine::Claude,
                cwd: "/repo".into(),
            })
            .unwrap();
        assert_eq!(state.native_id, None);
        assert_eq!(state.seen_through, TranscriptCursor(4));
    }

    #[test]
    fn frozen_accepted_target_does_not_replace_the_next_target() {
        let mut route = ConversationRoute::new(agent(CodeEngine::Claude, Some("A")));
        let accepted = agent(CodeEngine::Claude, Some("A"));
        route.set_next_target(agent(CodeEngine::Codex, Some("B")));
        let active = route
            .begin_turn_for_target(
                "conversation",
                9,
                "/repo",
                TranscriptCursor(0),
                accepted.clone(),
            )
            .unwrap();
        assert_eq!(active.target, accepted);
        assert_eq!(route.next_target(), &agent(CodeEngine::Codex, Some("B")));
    }

    #[test]
    fn gate_wait_keeps_a_resumable_id_but_started_turn_does_not() {
        let mut route = ConversationRoute::from_legacy(
            agent(CodeEngine::Claude, None),
            Some("/repo".into()),
            Some("old-session".into()),
            TranscriptCursor(2),
        );
        let waiting = route
            .begin_turn("conversation", 1, "/repo", TranscriptCursor(2))
            .unwrap();
        let waiting_json = route.to_persisted_json().unwrap();
        assert_eq!(
            ConversationRoute::from_persisted_json(&waiting_json)
                .unwrap()
                .native_session(&NativeSessionKey {
                    harness: CodeEngine::Claude,
                    cwd: "/repo".into(),
                })
                .unwrap()
                .native_id
                .as_deref(),
            Some("old-session"),
            "a memory gate has not sent the native bridge"
        );
        assert!(route.finish_unstarted_turn(&waiting));
        let started = route
            .begin_turn("conversation", 2, "/repo", TranscriptCursor(2))
            .unwrap();
        assert!(route.mark_native_started(&started));
        let started_json = route.to_persisted_json().unwrap();
        assert_eq!(
            ConversationRoute::from_persisted_json(&started_json)
                .unwrap()
                .native_session(&NativeSessionKey {
                    harness: CodeEngine::Claude,
                    cwd: "/repo".into(),
                })
                .unwrap()
                .native_id,
            None,
            "a provider that received a bridge cannot resume after restart"
        );
        assert!(route.stop_turn(&started, TranscriptCursor(3)));
        assert_eq!(
            route
                .native_session(&NativeSessionKey {
                    harness: CodeEngine::Claude,
                    cwd: "/repo".into(),
                })
                .unwrap()
                .native_id,
            None,
            "an interrupted started turn invalidates the in-memory token too"
        );
    }
}

#[cfg(test)]
mod image_encoding_migration_tests {
    use super::*;
    #[test]
    fn legacy_text_image_session_rebuilds_once_and_new_sessions_resume() {
        let mut route = ConversationRoute::from_legacy(
            WorkExecutor::Agent {
                engine: CodeEngine::Codex,
                model: None,
            },
            Some("/repo".into()),
            Some("legacy-thread".into()),
            TranscriptCursor(1),
        );
        let records = vec![BridgeRecord::message(
            "user",
            "![s](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)",
        )];
        let active = route
            .begin_turn("conv", 1, "/repo", TranscriptCursor(1))
            .unwrap();
        assert!(active
            .native
            .as_ref()
            .unwrap()
            .prepare_bridge(&records, "next", "/repo", &[], 2048)
            .resume_id()
            .is_none());
        assert!(route.finish_turn(
            &active,
            Some(NativeTurnFinish::Completed {
                native_id_after: Some("typed-thread".into()),
                transcript_through: TranscriptCursor(1)
            })
        ));
        let saved = route.to_persisted_json().unwrap();
        let mut restored = ConversationRoute::from_persisted_json(&saved).unwrap();
        let active = restored
            .begin_turn("conv", 2, "/repo", TranscriptCursor(1))
            .unwrap();
        assert_eq!(
            active
                .native
                .as_ref()
                .unwrap()
                .prepare_bridge(&records, "next", "/repo", &[], 2048)
                .resume_id(),
            Some("typed-thread")
        );
    }
}
