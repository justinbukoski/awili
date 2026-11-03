//! Daily-driver P2e: the last Work run's identity and status survive a
//! restart. When the graduation recorder consumes an armed turn's terminal
//! (once per turn, attributed to the conversation the turn was submitted
//! under) — and when a background re-attached build finishes — the requested
//! identity, backend model, terminal status, and error are persisted under
//! the per-conversation `app_state` key `work_last_run:<conv_id>` (the same
//! serialized worker/queue the `work_executor` key uses — the UI thread
//! never touches sqlite, SF6) and mirrored into the app's in-memory map.
//! When a Work conversation becomes active and no run is live, that value is
//! restored into the idle `RunPanel` (which the conversation then owns) so
//! the existing rendering shows `Requested: <executor · model>` and the
//! terminal status exactly as they did before the quit.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use awili_code::CodeEngine;
use awili_work::run_events::RunStatus;

use crate::persistence::{PersistCmd, Persistence, WORK_LAST_RUN_KEY_PREFIX};
use crate::work::{status_label, RunPanel, WorkExecutor, WorkState};
use crate::work_run_identity::WorkRunIdentity;

/// The `LastRun` payload stored (as JSON) under
/// `work_last_run:<conv_id>`. `WorkRunIdentity` and `RunStatus` are the
/// seam vocabulary — serialized by the helpers in [`engine`] and
/// [`status_from_token`] below so `awili-code`/`awili-work` need no changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LastRun {
    /// The requested dispatch identity frozen at submit (which executor and
    /// model the run asked for — never a claim about inference location).
    pub identity: WorkRunIdentity,
    /// The backend-reported model, if one was reported.
    pub model: Option<String>,
    /// The terminal status the run ended with.
    pub status: RunStatus,
    /// The failure message, if the run failed.
    pub error: Option<String>,
    /// When the run ended (RFC 3339, UTC).
    pub finished_at: String,
}

impl LastRun {
    /// Serialize a `LastRun` to the JSON string the app_state row carries.
    /// Infallible — every field serializes through plain primitives.
    pub(crate) fn to_json(&self) -> String {
        let engine = self
            .identity
            .agent_engine()
            .map(|e| engine::to_label(e).to_string());
        let body = serde_json::json!({
            "identity": self.identity.variant_name(),
            "engine": engine,
            "agent_model": self.identity.agent_model(),
            "owned_model": self.identity.owned_model(),
            "owned_engine_id": self.identity.owned_engine_id(),
            "build_model": self.identity.build_model(),
            "build_host": self.identity.build_host(),
            "model": self.model.as_deref(),
            "status": status_label(self.status),
            "error": self.error.as_deref(),
            "finished_at": self.finished_at,
        });
        serde_json::to_string(&body).unwrap_or_default()
    }

    /// Parse a stored JSON payload; `None` for a missing or corrupt value
    /// (a corrupt row is treated as absent — the panel stays as today).
    pub(crate) fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let identity = match v.get("identity")?.as_str()? {
            "agent" => WorkRunIdentity::Agent {
                engine: engine::from_label(v.get("engine")?.as_str()?)?,
                model: v
                    .get("agent_model")
                    .and_then(|m| m.as_str())
                    .map(String::from),
            },
            "hermes_gateway" => WorkRunIdentity::HermesGateway,
            "owned_engine" => WorkRunIdentity::OwnedEngine {
                model: v.get("owned_model")?.as_str()?.to_string(),
                // Absent in records written before `[[engines]]`: the primary.
                engine_id: v
                    .get("owned_engine_id")
                    .and_then(|i| i.as_str())
                    .map(String::from),
            },
            "local_build" => WorkRunIdentity::LocalBuild {
                model: v.get("build_model")?.as_str()?.to_string(),
                host: v.get("build_host")?.as_str()?.to_string(),
            },
            _ => return None,
        };
        Some(LastRun {
            identity,
            model: v.get("model").and_then(|m| m.as_str()).map(String::from),
            status: status_from_token(v.get("status")?.as_str()?)?,
            error: v.get("error").and_then(|e| e.as_str()).map(String::from),
            finished_at: v.get("finished_at")?.as_str()?.to_string(),
        })
    }
}

/// A `CodeEngine` serialized as its display label — `awili-code` cannot take
/// serde derives (the P2e touch list), so the label string is the stored
/// form. `to_label` is `CodeEngine::label` itself; unrecognized labels
/// round-trip as `None` (treated as absent).
mod engine {
    use awili_code::CodeEngine;

    /// The stored form of `e`: its display label (`CodeEngine::label`).
    pub(crate) fn to_label(e: CodeEngine) -> &'static str {
        e.label()
    }

    /// Parse a stored engine label back; `None` for an unrecognized label
    /// (the whole row is then treated as absent).
    pub(crate) fn from_label(s: &str) -> Option<CodeEngine> {
        // Exact labels only — a rename of a label is a format change, not a
        // parse error to swallow.
        match s {
            "Claude" => Some(CodeEngine::Claude),
            "Codex" => Some(CodeEngine::Codex),
            "GLM" => Some(CodeEngine::Glm),
            "DeepSeek" => Some(CodeEngine::Dsh),
            _ => None,
        }
    }
}

/// The token a stored `RunStatus` serializes to: `work::status_label`, the
/// same lowercase tokens the panel already renders and that the gateway's
/// `parse_status` (work.rs) accepts — `Unknown` is the tolerant fallback for
/// an unrecognized token, mirroring `RunStatus`'s own `#[serde(other)]`.
fn status_from_token(s: &str) -> Option<RunStatus> {
    use RunStatus::*;
    Some(match s {
        "started" => Started,
        "running" => Running,
        "completed" => Completed,
        "failed" | "error" | "failure" => Failed,
        "stopped" | "cancelled" | "canceled" | "interrupted" => Stopped,
        _ => Unknown,
    })
}

/// The `app_state` key for a conversation's last run. Per conversation —
/// each Work conversation keeps its own row, and the key never collides
/// with the global `work_executor`/`work_folder` keys.
pub(crate) fn key_for(conv_id: &str) -> String {
    format!("{WORK_LAST_RUN_KEY_PREFIX}{conv_id}")
}

impl WorkRunIdentity {
    /// The stored identity discriminant (`"agent"`, `"hermes_gateway"`,
    /// `"owned_engine"`, `"local_build"`).
    pub(crate) fn variant_name(&self) -> &'static str {
        match self {
            WorkRunIdentity::Agent { .. } => "agent",
            WorkRunIdentity::HermesGateway => "hermes_gateway",
            WorkRunIdentity::OwnedEngine { .. } => "owned_engine",
            WorkRunIdentity::LocalBuild { .. } => "local_build",
        }
    }

    /// The `CodeEngine` of an `Agent` identity, if this is one.
    pub(crate) fn agent_engine(&self) -> Option<CodeEngine> {
        match self {
            WorkRunIdentity::Agent { engine, .. } => Some(*engine),
            _ => None,
        }
    }

    /// The requested model of an `Agent` identity, if any.
    pub(crate) fn agent_model(&self) -> Option<&str> {
        match self {
            WorkRunIdentity::Agent { model, .. } => model.as_deref(),
            _ => None,
        }
    }

    /// The model of an `OwnedEngine` identity, if this is one.
    pub(crate) fn owned_model(&self) -> Option<&str> {
        match self {
            WorkRunIdentity::OwnedEngine { model, .. } => Some(model.as_str()),
            _ => None,
        }
    }

    /// The `[[engines]]` id of an `OwnedEngine` identity (`None`: the primary).
    pub(crate) fn owned_engine_id(&self) -> Option<&str> {
        match self {
            WorkRunIdentity::OwnedEngine { engine_id, .. } => engine_id.as_deref(),
            _ => None,
        }
    }

    /// The model of a `LocalBuild` identity, if this is one.
    pub(crate) fn build_model(&self) -> Option<&str> {
        match self {
            WorkRunIdentity::LocalBuild { model, .. } => Some(model.as_str()),
            _ => None,
        }
    }

    /// The dispatcher host of a `LocalBuild` identity, if this is one.
    pub(crate) fn build_host(&self) -> Option<&str> {
        match self {
            WorkRunIdentity::LocalBuild { host, .. } => Some(host.as_str()),
            _ => None,
        }
    }
}

/// Queue the terminal run's `LastRun` onto the serialized persistence
/// worker (the same `SetState` path as `work_executor`) — fire-and-forget.
/// The status is [`terminal_status`](crate::graduation_record::terminal_status)'s
/// (an error-only panel is `Failed`); a non-terminal panel or one without a
/// requested identity writes nothing. The same JSON is inserted into `map`
/// (the app's `work_last_run` map, P2e R2) so a later activation never
/// restores a stale row; returns it, `None` when nothing was written.
pub(crate) fn persist_last_run(
    persist: Option<&Persistence>,
    map: &mut HashMap<String, String>,
    conv_id: &str,
    panel: &RunPanel,
    finished_at: DateTime<Utc>,
) -> Option<String> {
    let status = crate::graduation_record::terminal_status(panel)?;
    let identity = panel.requested_identity.clone()?;
    persist_last_run_parts(
        persist,
        map,
        conv_id,
        LastRun {
            identity,
            model: panel.model.clone(),
            status,
            error: panel.error.clone(),
            finished_at: finished_at.to_rfc3339(),
        },
    )
}

/// Queue an already-built `LastRun` under `work_last_run:<conv_id>`
/// (P2e R4: the background re-attached build rows have no panel) and insert
/// the same JSON into `map` (R2). Returns the JSON, `None` when persistence
/// is off or nothing was written. A write failure is logged by the
/// persistence worker and ignored.
pub(crate) fn persist_last_run_parts(
    persist: Option<&Persistence>,
    map: &mut HashMap<String, String>,
    conv_id: &str,
    last: LastRun,
) -> Option<String> {
    let persist = persist?; // persistence off: the app runs without restore.
                            // `to_json` is infallible; if it ever produced an empty string (a future
                            // field failing to serialize), do not clobber the stored row.
    let json = last.to_json();
    if json.is_empty() {
        return None;
    }
    persist.send(PersistCmd::SetState {
        key: key_for(conv_id),
        value: json.clone(),
    });
    map.insert(conv_id.to_string(), json.clone());
    Some(json)
}

/// Restore `conv_id`'s stored last run into the idle Work panel and give the
/// panel to that conversation (P2e R3: `work.conv_id` is the presentation
/// owner `panel_is_visible` checks). Never touches a running panel, nor one
/// whose gateway Stop is still pending (P3c round 4, R8: until the worker
/// acknowledges the Stop, the stopped turn owns the panel and its streaming
/// row, so its drained `HookDenial` and `Terminal` land under its own
/// conversation). Returns whether a row was applied; a missing or corrupt row
/// leaves `work` as is.
pub(crate) fn restore_into(
    work: &mut WorkState,
    conv_id: &str,
    map: &HashMap<String, String>,
) -> bool {
    if work.running || work.gateway_stop_pending {
        return false;
    }
    let Some(last) = stored_last_run(conv_id, map) else {
        return false;
    };
    // R3 (round-2 review note): when the idle panel still belongs to another
    // conversation, its leftover output, steps and usage are that run's, not
    // this one's — start from a fresh panel so the restored row is all that
    // shows. A panel already owned by `conv_id` keeps its content (it is the
    // fresher truth for the same run).
    if work.conv_id.as_deref() != Some(conv_id) {
        work.panel = RunPanel::default();
    }
    apply_to_panel(&last, &mut work.panel);
    restore_executor(work, conv_id, map);
    work.conv_id = Some(conv_id.to_string());
    true
}

/// RC1-O (round 5 Major 4): the chip follows the conversation — the owner's
/// own pick there (`chip_picked`, this app session), else the lane its last
/// run used, else the default Work helper. Never while a run is live (the live run's approvals are routed by
/// the chip), and never to a one-shot local Build. Not persisted: the stored
/// chip stays the owner's last pick, and "+ New conversation" still starts on
/// the default helper. Returns whether the chip was set.
pub(crate) fn restore_executor(
    work: &mut WorkState,
    conv_id: &str,
    map: &HashMap<String, String>,
) -> bool {
    if work.running || work.gateway_stop_pending {
        return false;
    }
    let Some(exec) = conversation_helper(work, conv_id, map) else {
        return false;
    };
    work.executor = exec;
    true
}

/// RC2-R1G2: the helper `conv_id`'s chip follows ([`restore_executor`]'s
/// order), read without touching `work.executor` — so the chip can show it
/// while another conversation's run is live. `None` when there is none.
pub(crate) fn conversation_helper(
    work: &WorkState,
    conv_id: &str,
    map: &HashMap<String, String>,
) -> Option<WorkExecutor> {
    // Pre-review 4: a conversation with neither a pick nor a lane to go back
    // to (never run, or only a one-shot Build) takes the default helper, not
    // whatever chip the previous conversation left.
    work.chip_picked
        .get(conv_id)
        .cloned()
        .or_else(|| {
            stored_last_run(conv_id, map)
                .and_then(|last| crate::executor_labels::executor_for(&last.identity))
        })
        // RC2-R1F2: else the helper the owner chose last, else the default.
        .or_else(|| work.last_pick.clone())
        .or_else(|| {
            work.default_helper
                .as_deref()
                .and_then(WorkExecutor::from_persist)
        })
}

/// Restore a stored `LastRun` into an idle `RunPanel` (P2e step 3): sets the
/// requested identity, backend model, terminal status, and error so the
/// existing rendering shows `Requested: <executor · model>` and the status
/// exactly as before the quit. Caller guarantees the panel is idle
/// (`!running`).
pub(crate) fn apply_to_panel(last: &LastRun, panel: &mut RunPanel) {
    panel.requested_identity = Some(last.identity.clone());
    panel.model = last.model.clone();
    panel.status = Some(last.status);
    panel.error = last.error.clone();
}

/// Read a conversation's last-run JSON from the map the app holds (captured
/// at startup, then updated on every persist — SF6: the UI thread never
/// reads sqlite) and parse it. `None` when
/// the row is absent or the stored JSON is corrupt (a corrupt row is treated
/// as absent — the panel stays as today).
pub(crate) fn stored_last_run(
    conv_id: &str,
    work_last_run: &HashMap<String, String>,
) -> Option<LastRun> {
    work_last_run
        .get(conv_id)
        .and_then(|v| LastRun::from_json(v))
}

#[cfg(test)]
#[path = "work_last_run_tests.rs"]
mod work_last_run_tests;
