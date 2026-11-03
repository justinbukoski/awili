//! Tolerant list-envelope decoding for the gateway's REST surface.
//!
//! The Hermes/gateway backend is inconsistent about how it wraps array
//! responses: a list of sessions or jobs may arrive as a bare top-level JSON
//! array, or wrapped under one of several envelope keys (`data`, `sessions`,
//! `jobs`, ...). [`decode_list`] absorbs that variation so callers do not have
//! to special-case each shape.
//!
//! This is the decoder seam of Phase C unit 9 (see
//! `docs/GATEWAY-3WEEK-PLAN.md`): the later REST units (10-14) hang their typed
//! decoders and verb methods off these functions and off [`RestClient`].
//!
//! [`RestClient`] is the blocking-reqwest skeleton (unit 9 follow-up): it shares
//! the gateway's host/token, carries `Authorization: Bearer <token>`, and is
//! time-bounded the same way the legacy `awili-hermes` `HermesClient` was. Only the two list
//! GETs (`/api/sessions`, `/api/cron/jobs`) are wired so far; units 10/12/14 add the
//! typed list/CRUD methods on top of the same `send` + `decode_list` seam.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::GatewayError;

/// Pull a `Vec<T>` out of a list response that may wrap its array under any of
/// the given `keys`, or be a bare top-level array.
///
/// Tolerance rules (a faithful port of `awili-hermes::decode_list`):
/// - The body is parsed as a [`serde_json::Value`] first; a parse failure maps
///   to [`GatewayError::Decode`].
/// - A top-level JSON array is used directly and `keys` are ignored.
/// - Otherwise the first key that is **present and non-null** is taken. A
///   present-but-null key (e.g. `{"data": null}`) is skipped so it falls
///   through to the next key rather than erroring on
///   "invalid type: null, expected a sequence".
/// - If no key matches (all missing or null), an empty array is used, yielding
///   `Ok(vec![])` rather than an error.
/// - The resolved array is deserialized into `Vec<T>`; an element-type mismatch
///   maps to [`GatewayError::Decode`].
pub fn decode_list<T: DeserializeOwned>(body: &str, keys: &[&str]) -> Result<Vec<T>, GatewayError> {
    let value: Value =
        serde_json::from_str(body).map_err(|e| GatewayError::Decode(e.to_string()))?;
    let array = if let Value::Array(_) = value {
        value
    } else {
        keys.iter()
            // Skip a present-but-null key so it falls through to the next key /
            // empty array instead of erroring on "invalid type: null".
            .find_map(|k| value.get(*k).filter(|v| !v.is_null()).cloned())
            .unwrap_or(Value::Array(Vec::new()))
    };
    serde_json::from_value(array).map_err(|e| GatewayError::Decode(e.to_string()))
}

/// Decode a sessions-list response, tolerating the `data` / `sessions`
/// envelopes (and a bare array).
pub fn decode_sessions_list<T: DeserializeOwned>(body: &str) -> Result<Vec<T>, GatewayError> {
    decode_list(body, &["data", "sessions"])
}

/// Decode a jobs-list response, tolerating the `data` / `jobs` envelopes (and a
/// bare array).
pub fn decode_jobs_list<T: DeserializeOwned>(body: &str) -> Result<Vec<T>, GatewayError> {
    decode_list(body, &["data", "jobs"])
}

// Skills / toolsets (Phase C unit 12 — ported from awili-hermes)
// ============================================================================

/// An installed agent skill (agentskills.io standard), as returned by
/// `GET /api/skills` (the desktop's `SkillInfo`). Skills are server-side
/// filesystem-discovered; the desktop only lists them (grouped by `category`,
/// searchable) and flips `enabled` via [`RestClient::toggle_skill`]. Every
/// field past `name` is `#[serde(default)]` so the decoder stays tolerant of
/// older/leaner server envelopes (and the legacy `awili-hermes` shape).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Skill {
    pub name: String,
    /// Grouping the desktop lists the skill under (e.g. `"coding"`).
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Whether the skill is currently active for new sessions.
    #[serde(default)]
    pub enabled: bool,
    /// Kept for backwards-compatibility with the legacy `awili-hermes` shape;
    /// the current desktop contract does not surface it.
    #[serde(default)]
    pub version: Option<String>,
}

/// An installed toolset and the tool names it exposes, as returned by
/// `GET /api/tools/toolsets` (the desktop's `ToolsetInfo`). The desktop shows
/// a configured / needs-keys pill (`configured`), a toggle (`enabled`), and the
/// `tools` chips. Every field past `name` is `#[serde(default)]` so the decoder
/// tolerates leaner server envelopes (and the legacy `awili-hermes` shape).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Toolset {
    pub name: String,
    /// Human-friendly display name (falls back to `name` in the UI when absent).
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Whether the toolset is currently active for new sessions.
    #[serde(default)]
    pub enabled: bool,
    /// Whether the toolset's required provider keys are all present (drives the
    /// configured / needs-keys pill).
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub tools: Vec<String>,
}

/// Decode a skills-list response, tolerating the `data` / `skills` envelopes (and
/// a bare array).
pub fn decode_skills(body: &str) -> Result<Vec<Skill>, GatewayError> {
    decode_list(body, &["data", "skills"])
}

/// Decode a toolsets-list response, tolerating the `data` / `toolsets` envelopes
/// (and a bare array).
pub fn decode_toolsets(body: &str) -> Result<Vec<Toolset>, GatewayError> {
    decode_list(body, &["data", "toolsets"])
}

// ============================================================================
// Typed scheduled-job types (Phase C unit 10 — ported from awili-hermes)
// ============================================================================

fn null_to_empty_vec<'de, D, T>(de: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(de)?.unwrap_or_default())
}

/// Deserialize a field, treating an explicit JSON `null` as `T::default()`.
///
/// `#[serde(default)]` only covers a *missing* key; an explicit `null` still
/// errors. Collection/map/nested-struct fields on the Command Center decoders
/// use this so a "no data" response that surfaces a field as `null` (rather
/// than omitting it) decodes to the empty default instead of failing the whole
/// body. (Scalar counters stay on plain `#[serde(default)]` — they are computed
/// DB aggregates that are never emitted as `null`.)
fn null_to_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    pub prompt: String,
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub skills: Vec<String>,
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub schedule: Option<JobSchedule>,
    #[serde(default)]
    pub schedule_display: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub paused_at: Option<String>,
    #[serde(default)]
    pub paused_reason: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub enabled_toolsets: Vec<String>,
    #[serde(default)]
    pub workdir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct JobSchedule {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub expr: Option<String>,
    #[serde(default)]
    pub display: Option<String>,
}

pub fn decode_jobs(body: &str) -> Result<Vec<ScheduledJob>, GatewayError> {
    // Tolerant like the other list decoders: the dashboard returns a BARE
    // ARRAY from `/api/cron/jobs` (live-verified 2026-08-22), while older
    // servers wrapped it in `{jobs:[…]}`. The old rigid `JobsEnvelope` decode
    // was worse than strict on a bare array: serde will fit a JSON array into
    // a struct positionally, so the first job OBJECT landed on the `jobs`
    // field's sequence deserializer and every fetch failed with
    // "invalid type: map, expected a sequence".
    decode_list(body, &["jobs", "data"])
}

// ============================================================================
// Typed HermesSession (Phase C unit 14 — ported from awili-hermes lib.rs)
// ============================================================================

/// A past Hermes session, from `GET /api/sessions`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct HermesSession {
    pub id: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub started_at: Option<f64>,
    #[serde(default)]
    pub ended_at: Option<f64>,
    #[serde(default)]
    pub end_reason: Option<String>,
    #[serde(default)]
    pub message_count: Option<u32>,
    #[serde(default)]
    pub tool_call_count: Option<u32>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub last_active: Option<f64>,
}

/// Decode a sessions-list response, tolerating the `data` / `sessions`
/// envelopes (and a bare array). Delegates to the existing tolerant seam.
pub fn decode_sessions(body: &str) -> Result<Vec<HermesSession>, GatewayError> {
    decode_sessions_list(body)
}

// ============================================================================
// /v1/capabilities types (Phase C unit 16a — ported from awili-hermes lib.rs 69-133)
// ============================================================================

/// Self-description from the Hermes gateway, as returned by `GET /v1/capabilities`.
///
/// The server advertises what auth scheme it expects, which runtime mode it runs
/// in, a mixed-type `features` map (boolean flags + string entries), and a named
/// set of `endpoints` the client can call. Every field is `#[serde(default)]` so a
/// leaner server response is tolerated — the caller can `feature_bool` / `feature_str`
/// to inspect known keys and ignore the rest.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Capabilities {
    /// Always `"hermes.api_server.capabilities"`.
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub auth: Option<AuthInfo>,
    #[serde(default)]
    pub runtime: Option<RuntimeInfo>,
    /// The server's `features` map. The real server mixes value types here:
    /// boolean flags (e.g. `tool_progress_events: true`) alongside string entries
    /// (e.g. `session_continuity_header: "X-Hermes-Session-Id"`). So the map is
    /// kept as raw [`Value`] — a `BTreeMap<String, bool>` would reject the real
    /// response. Use [`feature`](Self::feature) / [`feature_bool`](Self::feature_bool)
    /// for the boolean flags and [`feature_str`](Self::feature_str) for the strings.
    #[serde(default)]
    pub features: BTreeMap<String, Value>,
    /// Named endpoints, e.g. `submit_run` → `{method:"POST", path:"/v1/runs"}`.
    #[serde(default)]
    pub endpoints: BTreeMap<String, Endpoint>,
}

impl Capabilities {
    /// Whether a named boolean feature is advertised and true. A missing key or a
    /// non-boolean value reads as `false`.
    pub fn feature(&self, name: &str) -> bool {
        self.feature_bool(name).unwrap_or(false)
    }

    /// The boolean value of a feature flag, or `None` if absent or not a bool.
    pub fn feature_bool(&self, name: &str) -> Option<bool> {
        self.features.get(name).and_then(Value::as_bool)
    }

    /// The string value of a feature entry (e.g. `session_continuity_header`),
    /// or `None` if absent or not a string.
    pub fn feature_str(&self, name: &str) -> Option<&str> {
        self.features.get(name).and_then(Value::as_str)
    }
}

/// Authentication scheme the gateway expects, from `/v1/capabilities`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AuthInfo {
    /// e.g. `"bearer"`.
    #[serde(default)]
    pub scheme: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
}

/// Runtime-mode advertisement from `/v1/capabilities`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RuntimeInfo {
    /// e.g. `"server_agent"`.
    #[serde(default)]
    pub mode: Option<String>,
    /// Where tools run, e.g. `"server"`.
    #[serde(default)]
    pub tool_execution: Option<String>,
}

/// A named endpoint advertised in `/v1/capabilities`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Endpoint {
    pub method: String,
    pub path: String,
}

/// Parse a `/v1/capabilities` response body into [`Capabilities`].
///
/// An unparseable body maps to [`GatewayError::Decode`]; a well-formed body that
/// is missing optional fields simply gets `None`/default values.
pub fn decode_capabilities(body: &str) -> Result<Capabilities, GatewayError> {
    serde_json::from_str(body).map_err(|e| GatewayError::Decode(e.to_string()))
}

// ============================================================================
// /v1/models types (Phase C unit 16b — ported from awili-hermes lib.rs 136-156)
// ============================================================================

/// An OpenAI-shaped model-list envelope returned by `GET /v1/models`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ModelList {
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub data: Vec<Model>,
}

/// A single model entry from the gateway's model list.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub created: Option<i64>,
    #[serde(default)]
    pub owned_by: Option<String>,
}

/// Parse a `GET /v1/models` response body into the model list. An unparseable
/// body maps to `GatewayError::Decode`; a well-formed body with an absent
/// `data` key decodes to an empty list (serde default). Mirrors awili-hermes.
pub fn decode_models(body: &str) -> Result<Vec<Model>, GatewayError> {
    let list: ModelList =
        serde_json::from_str(body).map_err(|e| GatewayError::Decode(e.to_string()))?;
    Ok(list.data)
}

// ============================================================================
// Command Center decoders (unit 20) — Hermes desktop REST surface
// ============================================================================

/// System/gateway status from `GET /api/status`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SystemStatus {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub config_version: Option<i64>,
    #[serde(default)]
    pub latest_config_version: Option<i64>,
    #[serde(default)]
    pub can_update_hermes: bool,
    #[serde(default)]
    pub gateway_running: bool,
    #[serde(default)]
    pub gateway_state: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub gateway_platforms: BTreeMap<String, PlatformStatus>,
    #[serde(default)]
    pub gateway_exit_reason: Option<String>,
    #[serde(default)]
    pub gateway_updated_at: Option<String>,
    #[serde(default)]
    pub active_agents: i64,
    #[serde(default)]
    pub gateway_busy: bool,
    #[serde(default)]
    pub gateway_drainable: bool,
    #[serde(default)]
    pub restart_drain_timeout: Option<f64>,
    #[serde(default)]
    pub active_sessions: i64,
    #[serde(default)]
    pub auth_required: bool,
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub auth_providers: Vec<String>,
    #[serde(default)]
    pub hermes_home: Option<String>,
    #[serde(default)]
    pub config_path: Option<String>,
    #[serde(default)]
    pub env_path: Option<String>,
    #[serde(default)]
    pub gateway_pid: Option<i64>,
    #[serde(default)]
    pub gateway_health_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PlatformStatus {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Parse a `GET /api/status` response body into [`SystemStatus`].
pub fn decode_status(body: &str) -> Result<SystemStatus, GatewayError> {
    serde_json::from_str(body).map_err(|e| GatewayError::Decode(e.to_string()))
}

/// Agent-log tail from `GET /api/logs?file=<file>&lines=<lines>`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AgentLog {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub lines: Vec<String>,
}

/// Parse a `GET /api/logs` response body into [`AgentLog`].
pub fn decode_agent_log(body: &str) -> Result<AgentLog, GatewayError> {
    serde_json::from_str(body).map_err(|e| GatewayError::Decode(e.to_string()))
}

/// Usage analytics from `GET /api/analytics/usage?days=<days>`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct UsageAnalytics {
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub daily: Vec<DailyUsage>,
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub by_model: Vec<ModelUsage>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub totals: UsageTotals,
    #[serde(default)]
    pub period_days: Option<i64>,
    #[serde(default, deserialize_with = "null_to_default")]
    pub skills: SkillsAnalytics,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct DailyUsage {
    #[serde(default)]
    pub day: Option<String>,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub reasoning_tokens: i64,
    #[serde(default)]
    pub estimated_cost: f64,
    #[serde(default)]
    pub actual_cost: f64,
    #[serde(default)]
    pub sessions: i64,
    #[serde(default)]
    pub api_calls: i64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ModelUsage {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub estimated_cost: f64,
    #[serde(default)]
    pub sessions: i64,
    #[serde(default)]
    pub api_calls: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct UsageTotals {
    #[serde(default)]
    pub total_input: i64,
    #[serde(default)]
    pub total_output: i64,
    #[serde(default)]
    pub total_cache_read: i64,
    #[serde(default)]
    pub total_reasoning: i64,
    #[serde(default)]
    pub total_estimated_cost: f64,
    #[serde(default)]
    pub total_actual_cost: f64,
    #[serde(default)]
    pub total_sessions: i64,
    #[serde(default)]
    pub total_api_calls: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct SkillsAnalytics {
    #[serde(default, deserialize_with = "null_to_default")]
    pub summary: SkillsSummary,
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub top_skills: Vec<SkillUsage>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct SkillsSummary {
    #[serde(default)]
    pub total_skill_loads: i64,
    #[serde(default)]
    pub total_skill_edits: i64,
    #[serde(default)]
    pub total_skill_actions: i64,
    #[serde(default)]
    pub distinct_skills_used: i64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SkillUsage {
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub view_count: i64,
    #[serde(default)]
    pub manage_count: i64,
    #[serde(default)]
    pub total_count: i64,
    #[serde(default)]
    pub percentage: f64,
    #[serde(default)]
    pub last_used_at: Option<f64>,
}

/// Parse a `GET /api/analytics/usage` response body into [`UsageAnalytics`].
pub fn decode_usage(body: &str) -> Result<UsageAnalytics, GatewayError> {
    serde_json::from_str(body).map_err(|e| GatewayError::Decode(e.to_string()))
}

/// Per-day input/output tokens for the Command Center bar chart.
#[derive(Clone, Debug, PartialEq)]
pub struct DayInOut {
    pub day: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// Condensed analytics for the Command Center dashboard. `per_day` preserves the
/// server's daily order (day string, in/out tokens; a missing `day` renders as
/// empty string). `top_models` is by_model sorted by (input+output) tokens
/// descending, capped at `top_n`. `top_skills` is skills.top_skills as-is
/// (already server-ranked by count), capped at `top_n`. `totals` is copied through.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageRollup {
    pub per_day: Vec<DayInOut>,
    pub top_models: Vec<ModelUsage>,
    pub top_skills: Vec<SkillUsage>,
    pub totals: UsageTotals,
}

/// Condense analytics for the Command Center dashboard.
pub fn roll_up_usage(a: &UsageAnalytics, top_n: usize) -> UsageRollup {
    let per_day: Vec<DayInOut> = a
        .daily
        .iter()
        .map(|d| DayInOut {
            day: d.day.clone().unwrap_or_default(),
            input_tokens: d.input_tokens,
            output_tokens: d.output_tokens,
        })
        .collect();

    let mut top_models = a.by_model.clone();
    top_models.sort_by_key(|m| std::cmp::Reverse(m.input_tokens + m.output_tokens));
    top_models.truncate(top_n);

    let top_skills: Vec<SkillUsage> = a.skills.top_skills.iter().take(top_n).cloned().collect();

    UsageRollup {
        per_day,
        top_models,
        top_skills,
        totals: a.totals.clone(),
    }
}

// ============================================================================
// Messaging-connectors decoders (unit 24 — review-06 Messaging pane)
// ============================================================================

/// Messaging platforms response from `GET /api/messaging/platforms`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct MessagingPlatforms {
    #[serde(default)]
    pub env_path: String,
    #[serde(default)]
    pub gateway_start_command: String,
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub platforms: Vec<MessagingPlatform>,
}

/// A single messaging connector platform.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct MessagingPlatform {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub docs_url: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub gateway_running: bool,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub home_channel: Option<MessagingHomeChannel>,
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub env_vars: Vec<MessagingEnvVar>,
}

/// A home-channel reference for a messaging platform.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct MessagingHomeChannel {
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub name: String,
}

/// An environment variable entry for a messaging platform.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct MessagingEnvVar {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub is_set: bool,
    #[serde(default)]
    pub redacted_value: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub help: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub is_password: bool,
    #[serde(default)]
    pub advanced: bool,
}

/// Parse a `GET /api/messaging/platforms` response body into [`MessagingPlatforms`].
pub fn decode_messaging_platforms(body: &str) -> Result<MessagingPlatforms, GatewayError> {
    serde_json::from_str(body).map_err(|e| GatewayError::Decode(e.to_string()))
}

/// One commit the running Hermes is behind, from `GET /api/hermes/update/check`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct HermesUpdateCommit {
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// Commit timestamp (unix seconds); absent/null on malformed rows.
    #[serde(default)]
    pub at: Option<i64>,
}

/// Result of `GET /api/hermes/update/check` — whether a Hermes update is
/// available and, if so, how far behind the running install is. Drives the
/// Command Center "Update Hermes" affordance (the static [LIVE] chip in
/// `awili-app`'s `command_center`): `update_available && can_apply` gates the
/// live action, and `behind`/`commits` render the "N commits behind" detail.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct HermesUpdateCheck {
    /// How Hermes was installed (`"git"`, `"pip"`, ...); decides `can_apply`.
    #[serde(default)]
    pub install_method: Option<String>,
    #[serde(default)]
    pub current_version: Option<String>,
    /// Commits behind upstream; `0` when up to date or unknown.
    #[serde(default)]
    pub behind: i64,
    #[serde(default)]
    pub update_available: bool,
    /// Whether the gateway can apply the update in place (git installs) — a
    /// non-git install may report `update_available` yet `can_apply == false`.
    #[serde(default)]
    pub can_apply: bool,
    #[serde(default)]
    pub update_command: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default, deserialize_with = "null_to_empty_vec")]
    pub commits: Vec<HermesUpdateCommit>,
}

/// Parse a `GET /api/hermes/update/check` response body into [`HermesUpdateCheck`].
pub fn decode_hermes_update_check(body: &str) -> Result<HermesUpdateCheck, GatewayError> {
    serde_json::from_str(body).map_err(|e| GatewayError::Decode(e.to_string()))
}

// ============================================================================
// Blocking REST client (unit 9 follow-up)
// ============================================================================

/// S7b (F0716): the most bytes of one gateway REST response Awili reads.
/// 32 MiB: the largest body is a long session's full message log
/// (`GET /api/sessions/{id}/messages`, tool output included), which runs to
/// a few MB; every other route returns small JSON. 32 MiB leaves about ten
/// times that headroom while a broken or hostile gateway can no longer
/// grow one reply without limit.
pub const MAX_REST_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// S7b: the owner-visible error for a 2xx body over [`MAX_REST_BODY_BYTES`].
pub const BODY_TOO_LARGE: &str =
    "The gateway sent a reply larger than 32 MB, so Awili stopped reading it.";

/// S7b (F0716): read at most `cap + 1` bytes from `reader` and never more.
/// Returns the first `cap` bytes and whether the body was longer than `cap`.
fn read_capped<R: Read>(reader: R, cap: u64) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    reader.take(cap + 1).read_to_end(&mut bytes)?;
    let over = bytes.len() as u64 > cap;
    if over {
        bytes.truncate(cap as usize);
    }
    Ok((bytes, over))
}

/// S7b (F0714): the owner-visible error for an id that cannot be sent.
pub const BAD_PATH_ID: &str =
    "Awili did not send this request: the item's id is empty or only dots.";

/// S7b (F0714): one dynamic id as exactly ONE URL path segment. Every byte
/// outside the RFC 3986 unreserved set (`A-Z a-z 0-9 - . _ ~`) is
/// percent-encoded, so `/`, `?`, `#`, `%`, spaces and non-ASCII can never
/// add a segment, a query or a fragment. An empty id, `.` or `..` is
/// refused before any request is built: the URL parser would resolve them
/// as dot segments and send the request to a different route.
fn path_id(value: &str) -> Result<String, GatewayError> {
    if value.is_empty() || value == "." || value == ".." {
        return Err(GatewayError::Transport(BAD_PATH_ID.to_string()));
    }
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(out)
}

/// A blocking client for the gateway's `/api/*` REST surface.
///
/// The Hermes gateway exposes its workspace state (sessions, scheduled jobs,
/// skills, ...) over plain HTTP at the *same* host/port the WebSocket lives on —
/// `http://<host>:<port>/api/...` — authenticated with the same token the WS
/// query parameter carries, here sent as `Authorization: Bearer <token>`. This
/// client mirrors the legacy `awili-hermes` `HermesClient`'s transport: a blocking reqwest
/// client with bounded connect/read timeouts, so a reachable-but-stalled gateway
/// fails fast instead of hanging the caller thread.
///
/// Only the two list GETs are wired so far; the typed list/CRUD verbs land in
/// units 10/12/14 on the same [`send`](RestClient::send) + [`decode_list`] seam.
pub struct RestClient {
    base_url: String,
    token: String,
    http: reqwest::blocking::Client,
}

impl RestClient {
    /// Build a client against the gateway at `host:port`, authenticating with
    /// `token`. The base URL is `http://host:port` (the gateway serves its REST
    /// surface over plain HTTP on the LAN, same as the `ws://` transport).
    pub fn new(host: &str, port: u16, token: impl Into<String>) -> Self {
        // Bound BOTH the connect and the whole request: `connect_timeout` caps
        // the handshake, and reqwest's `timeout` caps the entire request. These
        // list GETs return small JSON bodies (not SSE streams), so an overall
        // request bound is the right shape — a stalled gateway errors instead of
        // hanging the caller thread.
        Self::with_timeouts(
            host,
            port,
            token,
            Duration::from_secs(10),
            Duration::from_secs(60),
        )
    }

    /// Build a client from a full base URL (e.g. `http://host:port`),
    /// authenticating with `token`. Mirrors `awili_hermes::HermesClient::new`'s
    /// `(base_url, api_key)` shape so the GPUI surfaces (skills/cron/sessions
    /// panels) can swap their hermes client for this REST client with a
    /// near-mechanical constructor change — they hold a base-URL string, not a
    /// split host+port. A trailing `/` is trimmed so [`api_url`](Self::api_url)
    /// joins cleanly.
    pub fn from_base_url(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self::from_base_url_with_timeouts(
            base_url,
            token,
            Duration::from_secs(10),
            Duration::from_secs(60),
        )
    }

    /// Construct with explicit connect/request timeouts. Extracted from [`new`]
    /// so the time-bound guarantee is testable against a stalled server without
    /// waiting the full production bound.
    fn with_timeouts(
        host: &str,
        port: u16,
        token: impl Into<String>,
        connect: Duration,
        read: Duration,
    ) -> Self {
        Self::from_base_url_with_timeouts(format!("http://{host}:{port}"), token, connect, read)
    }

    /// Shared constructor: build the bounded blocking client against an explicit
    /// `base_url`. Both [`new`](Self::new)/[`with_timeouts`](Self::with_timeouts)
    /// (host+port) and [`from_base_url`](Self::from_base_url) (full URL) funnel
    /// here so the timeout + client-build policy lives in one place.
    fn from_base_url_with_timeouts(
        base_url: impl Into<String>,
        token: impl Into<String>,
        connect: Duration,
        read: Duration,
    ) -> Self {
        // Canonicalize to the origin root. The re-point seams ([`from_base_url`])
        // are fed the Hermes `[hermes].base_url`, which carries the server's `/v1`
        // chat-completions prefix (default `http://127.0.0.1:8642/v1`). The
        // `/api/*` surface this client targets lives at the origin root, never
        // under `/v1` — so strip a trailing `/v1` (matching the old
        // `HermesClient::api_url`) as well as a trailing slash. Host+port
        // constructors ([`new`]/[`with_timeouts`]) pass `http://host:port` with no
        // `/v1`, so this is a no-op for them.
        let base_url = base_url
            .into()
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .trim_end_matches('/')
            .to_string();
        Self {
            base_url,
            token: token.into(),
            http: reqwest::blocking::Client::builder()
                .connect_timeout(connect)
                .timeout(read)
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new()),
        }
    }

    /// Build a URL against the gateway's `/api/*` base.
    fn api_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Attach `Authorization: Bearer <token>`. The token is never logged.
    fn auth(&self, rb: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        rb.header("Authorization", format!("Bearer {}", self.token))
    }

    /// Send a request, attaching auth, and return the body text on a 2xx. A
    /// request that never completes maps to [`GatewayError::Transport`]; a
    /// completed non-2xx maps to [`GatewayError::Http`] (the token is never
    /// included in either).
    ///
    /// S7b (F0716): at most [`MAX_REST_BODY_BYTES`] of the body are read. A
    /// larger 2xx body is refused with [`BODY_TOO_LARGE`]; a larger non-2xx
    /// body keeps its status and only the first `MAX_REST_BODY_BYTES`.
    fn send(&self, rb: reqwest::blocking::RequestBuilder) -> Result<String, GatewayError> {
        let resp = self
            .auth(rb)
            .send()
            .map_err(|e| GatewayError::Transport(e.to_string()))?;
        let status = resp.status();
        let (bytes, over) = read_capped(resp, MAX_REST_BODY_BYTES)
            .map_err(|e| GatewayError::Transport(e.to_string()))?;
        if over && status.is_success() {
            return Err(GatewayError::Transport(BODY_TOO_LARGE.to_string()));
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        if !status.is_success() {
            return Err(GatewayError::Http {
                status: status.as_u16(),
                body,
            });
        }
        Ok(body)
    }

    /// `GET /api/sessions` — list recent gateway sessions, tolerating the
    /// `data` / `sessions` envelopes (and a bare array).
    pub fn list_sessions(&self) -> Result<Vec<HermesSession>, GatewayError> {
        decode_sessions(&self.send(self.http.get(self.api_url("/api/sessions")))?)
    }

    /// `GET /api/sessions/{id}/messages` — fetch the raw message log for one
    /// session. Intentionally untyped (the gateway's message shape varies);
    /// the body is parsed as a free-form JSON value. A non-2xx maps to
    /// `GatewayError::Http`; an unparseable body maps to `GatewayError::Decode`.
    pub fn session_messages(&self, session_id: &str) -> Result<serde_json::Value, GatewayError> {
        let seg = path_id(session_id)?;
        let body = self.send(
            self.http
                .get(self.api_url(&format!("/api/sessions/{seg}/messages"))),
        )?;
        serde_json::from_str(&body).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// `PATCH /api/sessions/{id}` — archive or unarchive a session. Body `{"archived":<archived>}`.
    pub fn set_session_archived(
        &self,
        session_id: &str,
        archived: bool,
    ) -> Result<(), GatewayError> {
        let seg = path_id(session_id)?;
        let body = serde_json::json!({"archived": archived});
        self.send(
            self.http
                .patch(self.api_url(&format!("/api/sessions/{seg}")))
                .json(&body),
        )?;
        Ok(())
    }

    /// `PATCH /api/sessions/{id}` — rename a session. Body `{"title":<title>}`.
    pub fn set_session_title(&self, session_id: &str, title: &str) -> Result<(), GatewayError> {
        let seg = path_id(session_id)?;
        let body = serde_json::json!({"title": title});
        self.send(
            self.http
                .patch(self.api_url(&format!("/api/sessions/{seg}")))
                .json(&body),
        )?;
        Ok(())
    }

    /// `DELETE /api/sessions/{id}` — delete a session.
    pub fn delete_session(&self, session_id: &str) -> Result<(), GatewayError> {
        let seg = path_id(session_id)?;
        self.send(
            self.http
                .delete(self.api_url(&format!("/api/sessions/{seg}"))),
        )?;
        Ok(())
    }

    /// `GET /api/sessions/search?q=<query>` — full-text search sessions; tolerant-decoded like list_sessions.
    pub fn search_sessions(&self, query: &str) -> Result<Vec<HermesSession>, GatewayError> {
        decode_sessions(
            &self.send(
                self.http
                    .get(self.api_url("/api/sessions/search"))
                    .query(&[("q", query)]),
            )?,
        )
    }

    // NOTE: a speculative `fork_session` (`POST /api/sessions/{id}/fork`) was
    // removed 2026-07-02. Audited against the live gateway `/openapi.json`, that
    // route does not exist — the sessions surface exposes no fork/branch/clone
    // endpoint (branching is server-side; the tree is read via
    // `GET /api/sessions/{id}/latest-descendant`). The method had no callers and
    // its own docstring admitted the shape "is not yet pinned to a live server",
    // so it was a latent runtime 404. If a fork endpoint is later added to the
    // gateway, re-introduce it against the then-verified live path/shape.

    /// `GET /api/cron/jobs` — list scheduled jobs.
    ///
    /// NOTE (verified against a live gateway 2026-07-02): the jobs surface moved
    /// under `/api/cron/` — the bare `/api/jobs` now 404s
    /// (`{"detail":"No such API endpoint: /api/jobs"}`). This endpoint
    /// returns only *active* (non-paused) jobs — no query parameter includes
    /// paused ones, so a job paused via [`pause_job`] disappears from the listing
    /// until resumed. A UI that needs the paused row to persist (to keep a Resume
    /// affordance reachable) must track it client-side across refetches; the cron
    /// panel does this in `CronState::paused_overlay`. That overlay only lasts the
    /// process lifetime — surfacing jobs paused in a prior run would need a
    /// Hermes-side change to list paused jobs.
    pub fn list_jobs(&self) -> Result<Vec<ScheduledJob>, GatewayError> {
        let body = self.send(self.http.get(self.api_url("/api/cron/jobs")))?;
        decode_jobs(&body)
    }

    /// `GET /api/skills` — list installed agent skills.
    pub fn list_skills(&self) -> Result<Vec<Skill>, GatewayError> {
        decode_skills(&self.send(self.http.get(self.api_url("/api/skills")))?)
    }

    /// `GET /api/tools/toolsets` — list installed toolsets.
    pub fn list_toolsets(&self) -> Result<Vec<Toolset>, GatewayError> {
        decode_toolsets(&self.send(self.http.get(self.api_url("/api/tools/toolsets")))?)
    }

    /// `PUT /api/skills/toggle` — enable or disable a skill by name. Body
    /// `{"name":<name>,"enabled":<enabled>}`.
    pub fn toggle_skill(&self, name: &str, enabled: bool) -> Result<(), GatewayError> {
        let body = serde_json::json!({"name": name, "enabled": enabled});
        self.send(
            self.http
                .put(self.api_url("/api/skills/toggle"))
                .json(&body),
        )?;
        Ok(())
    }

    /// `PUT /api/tools/toolsets/{name}` — update a toolset. The `updates` value
    /// is sent as-is (pass-through, untyped — the gateway's body schema for this
    /// verb is not yet pinned to a live server), mirroring create_job's flat-body
    /// idiom.
    pub fn set_toolset(&self, name: &str, updates: &serde_json::Value) -> Result<(), GatewayError> {
        let seg = path_id(name)?;
        self.send(
            self.http
                .put(self.api_url(&format!("/api/tools/toolsets/{seg}")))
                .json(updates),
        )?;
        Ok(())
    }

    /// `POST /api/cron/jobs/{job_id}/pause` — pause a scheduled job.
    pub fn pause_job(&self, job_id: &str) -> Result<(), GatewayError> {
        let seg = path_id(job_id)?;
        self.send(
            self.http
                .post(self.api_url(&format!("/api/cron/jobs/{seg}/pause"))),
        )?;
        Ok(())
    }

    /// `POST /api/cron/jobs/{job_id}/resume` — resume a paused job.
    pub fn resume_job(&self, job_id: &str) -> Result<(), GatewayError> {
        let seg = path_id(job_id)?;
        self.send(
            self.http
                .post(self.api_url(&format!("/api/cron/jobs/{seg}/resume"))),
        )?;
        Ok(())
    }

    /// `POST /api/cron/jobs/{id}/trigger` — run the job now.
    ///
    /// The leaf is `/trigger` under the `/api/cron/jobs/` surface (verified
    /// against a live gateway 2026-07-02, `POST /api/cron/jobs/{id}/trigger` in
    /// the OpenAPI spec). This inverts the 2026-06-22 state, where the surface
    /// was the bare `/api/jobs/{id}/run` and `/trigger` 404'd — the gateway moved
    /// jobs under `/api/cron/` and renamed `run`→`trigger`.
    pub fn trigger_job(&self, job_id: &str) -> Result<(), GatewayError> {
        let seg = path_id(job_id)?;
        self.send(
            self.http
                .post(self.api_url(&format!("/api/cron/jobs/{seg}/trigger"))),
        )?;
        Ok(())
    }

    /// `POST /api/cron/jobs` — create a new scheduled job from a flat spec JSON
    /// body (`CronJobCreate`: `prompt`+`schedule` required, plus `name`/`deliver`/
    /// `skills`). The spec is sent as-is (not wrapped), matching the gateway's
    /// create endpoint.
    pub fn create_job(&self, spec: &serde_json::Value) -> Result<(), GatewayError> {
        self.send(self.http.post(self.api_url("/api/cron/jobs")).json(spec))?;
        Ok(())
    }

    /// `PUT /api/cron/jobs/{job_id}` — update fields of an existing scheduled job.
    ///
    /// The `updates` value is WRAPPED as `{"updates": <updates>}` before sending,
    /// because the gateway's update endpoint (`CronJobUpdate`, `updates` required)
    /// expects the wrapper (unlike the create endpoint which takes the spec flat).
    /// The verb is PUT, not POST (verified against the live gateway's OpenAPI spec
    /// 2026-07-02) — POST to this path 404s.
    pub fn update_job(
        &self,
        job_id: &str,
        updates: &serde_json::Value,
    ) -> Result<(), GatewayError> {
        let seg = path_id(job_id)?;
        let wrapper = serde_json::json!({"updates": updates});
        self.send(
            self.http
                .put(self.api_url(&format!("/api/cron/jobs/{seg}")))
                .json(&wrapper),
        )?;
        Ok(())
    }

    // Config read/write (Phase C unit 22a)
    // ============================================================================

    /// `GET /api/config` — the current effective config (arbitrary JSON object,
    /// untyped like session_messages).
    pub fn get_config(&self) -> Result<serde_json::Value, GatewayError> {
        let body = self.send(self.http.get(self.api_url("/api/config")))?;
        serde_json::from_str(&body).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// `GET /api/config/defaults` — the default config values (untyped).
    pub fn get_config_defaults(&self) -> Result<serde_json::Value, GatewayError> {
        let body = self.send(self.http.get(self.api_url("/api/config/defaults")))?;
        serde_json::from_str(&body).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// `GET /api/config/schema` — the config schema that drives the settings form
    /// widgets (untyped).
    pub fn get_config_schema(&self) -> Result<serde_json::Value, GatewayError> {
        let body = self.send(self.http.get(self.api_url("/api/config/schema")))?;
        serde_json::from_str(&body).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// `PUT /api/config` — persist config. Body `{"config":<config>}` (the desktop
    /// autosave shape, review-06).
    pub fn set_config(&self, config: &serde_json::Value) -> Result<(), GatewayError> {
        let body = serde_json::json!({"config": config});
        self.send(self.http.put(self.api_url("/api/config")).json(&body))?;
        Ok(())
    }

    /// `GET /api/env` — environment/credential entries (arbitrary JSON, untyped).
    pub fn get_env(&self) -> Result<serde_json::Value, GatewayError> {
        let body = self.send(self.http.get(self.api_url("/api/env")))?;
        serde_json::from_str(&body).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// `PUT /api/env` — set/update an env var. Body `{"key":..,"value":..}` (live dashboard setEnvVar shape).
    pub fn set_env_var(&self, key: &str, value: &str) -> Result<(), GatewayError> {
        let body = serde_json::json!({"key": key, "value": value});
        self.send(self.http.put(self.api_url("/api/env")).json(&body))?;
        Ok(())
    }

    /// `DELETE /api/env` — delete an env var. Body `{"key":..}` (live dashboard deleteEnvVar shape).
    pub fn delete_env_var(&self, key: &str) -> Result<(), GatewayError> {
        let body = serde_json::json!({"key": key});
        self.send(self.http.delete(self.api_url("/api/env")).json(&body))?;
        Ok(())
    }

    /// `GET /api/model/info` — the active main/auxiliary model selections (untyped,
    /// review-06 Model pane).
    pub fn model_info(&self) -> Result<serde_json::Value, GatewayError> {
        let body = self.send(self.http.get(self.api_url("/api/model/info")))?;
        serde_json::from_str(&body).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// `GET /api/model/options` — the available models to choose from (untyped).
    pub fn model_options(&self) -> Result<serde_json::Value, GatewayError> {
        let body = self.send(self.http.get(self.api_url("/api/model/options")))?;
        serde_json::from_str(&body).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// `GET /api/model/recommended-default` — the backend-recommended default model
    /// (untyped).
    pub fn model_recommended_default(&self) -> Result<serde_json::Value, GatewayError> {
        let body = self.send(
            self.http
                .get(self.api_url("/api/model/recommended-default")),
        )?;
        serde_json::from_str(&body).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// `GET /api/providers/oauth` — connected OAuth provider accounts (untyped,
    /// review-06 Providers pane).
    pub fn providers_oauth(&self) -> Result<serde_json::Value, GatewayError> {
        let body = self.send(self.http.get(self.api_url("/api/providers/oauth")))?;
        serde_json::from_str(&body).map_err(|e| GatewayError::Decode(e.to_string()))
    }

    /// `GET /v1/capabilities` — the gateway's self-description (auth, runtime,
    /// features, endpoints).
    ///
    /// Unlike the `/api/*` desktop-dashboard methods on this client, capabilities
    /// lives on the `/v1` OpenAI-gateway surface; both hang off the same
    /// canonicalized origin root, so [`api_url`](Self::api_url) with
    /// `/v1/capabilities` is correct. Mirrors [`HermesClient::capabilities`].
    pub fn capabilities(&self) -> Result<Capabilities, GatewayError> {
        decode_capabilities(&self.send(self.http.get(self.api_url("/v1/capabilities")))?)
    }

    /// `GET /v1/models` — the OpenAI-shaped model list the gateway serves. Like
    /// capabilities this lives on the `/v1` surface (not `/api/*`); both hang off
    /// the same canonicalized origin root so `api_url("/v1/models")` is correct.
    /// Mirrors `HermesClient::models`.
    pub fn models(&self) -> Result<Vec<Model>, GatewayError> {
        decode_models(&self.send(self.http.get(self.api_url("/v1/models")))?)
    }

    /// `GET /api/status` — Hermes system/gateway status for the Command Center System section.
    pub fn status(&self) -> Result<SystemStatus, GatewayError> {
        decode_status(&self.send(self.http.get(self.api_url("/api/status")))?)
    }

    /// `GET /api/logs?file=<file>&lines=<lines>` — tail of a Hermes log file (agent-log tail).
    pub fn agent_log(&self, file: &str, lines: u32) -> Result<AgentLog, GatewayError> {
        decode_agent_log(
            &self.send(
                self.http
                    .get(self.api_url("/api/logs"))
                    .query(&[("file", file), ("lines", &lines.to_string())]),
            )?,
        )
    }

    /// `GET /api/analytics/usage?days=<days>` — token/cost usage analytics for the Usage section.
    pub fn usage(&self, days: u32) -> Result<UsageAnalytics, GatewayError> {
        decode_usage(
            &self.send(
                self.http
                    .get(self.api_url("/api/analytics/usage"))
                    .query(&[("days", &days.to_string())]),
            )?,
        )
    }

    /// `GET /api/messaging/platforms` — messaging connectors for the Messaging pane.
    pub fn messaging_platforms(&self) -> Result<MessagingPlatforms, GatewayError> {
        decode_messaging_platforms(
            &self.send(self.http.get(self.api_url("/api/messaging/platforms")))?,
        )
    }

    /// `PUT /api/messaging/platforms/{id}` — update a connector's config (passthrough, like set_config).
    pub fn update_messaging(&self, id: &str, body: &serde_json::Value) -> Result<(), GatewayError> {
        let seg = path_id(id)?;
        self.send(
            self.http
                .put(self.api_url(&format!("/api/messaging/platforms/{seg}")))
                .json(body),
        )?;
        Ok(())
    }

    /// `POST /api/messaging/platforms/{id}/test` — trigger a connector self-test (passthrough).
    pub fn test_messaging(&self, id: &str) -> Result<(), GatewayError> {
        let seg = path_id(id)?;
        self.send(
            self.http
                .post(self.api_url(&format!("/api/messaging/platforms/{seg}/test"))),
        )?;
        Ok(())
    }

    /// `POST /api/gateway/restart` — restart the running gateway. Backs the
    /// Command Center "Restart Gateway" action; the caller has already checked
    /// [`SystemStatus::gateway_drainable`] to know the restart can drain cleanly.
    /// No response body — success is the 2xx alone.
    pub fn restart_gateway(&self) -> Result<(), GatewayError> {
        self.send(self.http.post(self.api_url("/api/gateway/restart")))?;
        Ok(())
    }

    /// `POST /api/gateway/drain` — drain in-flight agents without restarting, the
    /// graceful precursor a restart performs implicitly. Exposed on its own for
    /// a "drain then hold" affordance. No response body.
    pub fn drain_gateway(&self) -> Result<(), GatewayError> {
        self.send(self.http.post(self.api_url("/api/gateway/drain")))?;
        Ok(())
    }

    /// `GET /api/hermes/update/check` — whether a Hermes update is available and
    /// how far behind the install is. Read-only; drives the "Update Hermes"
    /// affordance's enablement and its "N commits behind" detail before the user
    /// commits to the (side-effecting) [`update_hermes`](Self::update_hermes).
    pub fn check_hermes_update(&self) -> Result<HermesUpdateCheck, GatewayError> {
        decode_hermes_update_check(
            &self.send(self.http.get(self.api_url("/api/hermes/update/check")))?,
        )
    }

    /// `POST /api/hermes/update` — apply the available Hermes update in place.
    /// Only meaningful when [`check_hermes_update`](Self::check_hermes_update)
    /// reported `update_available && can_apply`. No response body.
    pub fn update_hermes(&self) -> Result<(), GatewayError> {
        self.send(self.http.post(self.api_url("/api/hermes/update")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize, PartialEq, Debug)]
    struct Item {
        id: u32,
    }

    #[test]
    fn bare_top_level_array_ignores_keys() {
        let items: Vec<Item> =
            decode_list(r#"[{"id":1},{"id":2}]"#, &["data", "sessions"]).unwrap();
        assert_eq!(items, vec![Item { id: 1 }, Item { id: 2 }]);
    }

    #[test]
    fn wrapped_under_first_key() {
        let items: Vec<Item> =
            decode_list(r#"{"data":[{"id":1}]}"#, &["data", "sessions"]).unwrap();
        assert_eq!(items, vec![Item { id: 1 }]);
    }

    #[test]
    fn wrapped_under_second_key() {
        let items: Vec<Item> =
            decode_list(r#"{"sessions":[{"id":3}]}"#, &["data", "sessions"]).unwrap();
        assert_eq!(items, vec![Item { id: 3 }]);
    }

    #[test]
    fn present_but_null_first_key_falls_through() {
        let items: Vec<Item> = decode_list(
            r#"{"data":null,"sessions":[{"id":4}]}"#,
            &["data", "sessions"],
        )
        .unwrap();
        assert_eq!(items, vec![Item { id: 4 }]);
    }

    #[test]
    fn all_keys_null_is_empty_ok() {
        let items: Vec<Item> = decode_list(r#"{"data":null}"#, &["data"]).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn missing_keys_is_empty_ok() {
        let items: Vec<Item> = decode_list(r#"{"other":1}"#, &["data", "sessions"]).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn empty_object_is_empty_ok() {
        let items: Vec<Item> = decode_list(r#"{}"#, &["data", "sessions"]).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn invalid_json_is_decode_err() {
        let err = decode_list::<Item>("not json", &["data"]).unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn element_type_mismatch_is_decode_err() {
        let err = decode_list::<Item>(r#"[{"id":"x"}]"#, &["data"]).unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn sessions_and_jobs_wrappers_resolve() {
        let sessions: Vec<Item> = decode_sessions_list(r#"{"sessions":[{"id":5}]}"#).unwrap();
        assert_eq!(sessions, vec![Item { id: 5 }]);
        let jobs: Vec<Item> = decode_jobs_list(r#"{"jobs":[{"id":6}]}"#).unwrap();
        assert_eq!(jobs, vec![Item { id: 6 }]);
    }

    // ------------------------------------------------------------------------
    // RestClient — exercised against a one-shot localhost stub HTTP server, the
    // same approach awili-hermes uses (its blocking client is not pure either).
    // ------------------------------------------------------------------------

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Instant;

    /// Spawn a one-shot HTTP/1.1 server on an ephemeral port that replies to the
    /// first connection with `status`/`body`, and reports the bytes of the
    /// request it received back over the returned channel (so a test can assert
    /// on the headers the client actually sent). Returns `(host, port, rx)`.
    fn stub_once(
        status: &'static str,
        body: &'static str,
    ) -> (String, u16, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        (addr.ip().to_string(), addr.port(), rx)
    }

    #[test]
    fn api_url_joins_base_and_path() {
        let c = RestClient::new("10.77.68.94", 8645, "tok");
        assert_eq!(
            c.api_url("/api/sessions"),
            "http://10.77.68.94:8645/api/sessions"
        );
    }

    #[test]
    fn from_base_url_joins_path_and_trims_trailing_slash() {
        // The GPUI surfaces hold a base-URL string; `from_base_url` must produce
        // the same joined paths as the host+port `new`, and a trailing slash on
        // the base must not double up into `//api/...`.
        let c = RestClient::from_base_url("http://10.77.68.94:8645", "tok");
        assert_eq!(
            c.api_url("/api/sessions"),
            "http://10.77.68.94:8645/api/sessions"
        );
        let trimmed = RestClient::from_base_url("http://10.77.68.94:8645/", "tok");
        assert_eq!(
            trimmed.api_url("/api/config"),
            "http://10.77.68.94:8645/api/config"
        );
    }

    #[test]
    fn from_base_url_strips_hermes_v1_prefix() {
        // The GPUI re-point seams pass `[hermes].base_url`, which carries the
        // server's `/v1` chat-completions prefix (default `…:8642/v1`). The
        // `/api/*` surface lives at the origin root — so `/v1` must be stripped,
        // matching the old `HermesClient::api_url`, or every re-pointed lane would
        // request `/v1/api/...` and 404.
        let c = RestClient::from_base_url("http://127.0.0.1:8642/v1", "tok");
        assert_eq!(
            c.api_url("/api/sessions"),
            "http://127.0.0.1:8642/api/sessions"
        );
        let with_slash = RestClient::from_base_url("http://127.0.0.1:8642/v1/", "tok");
        assert_eq!(
            with_slash.api_url("/api/cron/jobs"),
            "http://127.0.0.1:8642/api/cron/jobs"
        );
    }

    #[test]
    fn from_base_url_client_routes_request_and_sends_bearer() {
        // End-to-end: a client built from a base-URL string reaches the stub,
        // routes the path, and carries the bearer token verbatim — proving it is
        // a drop-in for the host+port constructor on the live REST surface.
        let (host, port, rx) = stub_once("200 OK", r#"{"sessions":[{"id":"z1"}]}"#);
        let client = RestClient::from_base_url(format!("http://{host}:{port}"), "Tok-XyZ9");
        let sessions = client.list_sessions().expect("list_sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "z1");
        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("GET /api/sessions "), "path: {request}");
        let header_line = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or_else(|| panic!("no Authorization header: {request}"));
        assert_eq!(
            header_line.trim_start_matches(|c: char| c != ' '),
            " Bearer Tok-XyZ9",
            "auth header value corrupted: {header_line}"
        );
    }

    #[test]
    fn list_sessions_decodes_envelope_and_sends_bearer() {
        // Mixed-case token: the header NAME is matched case-insensitively (reqwest
        // lowercases it on the wire) but the token VALUE must survive verbatim, so
        // a corrupting transform on the token would fail this test.
        let (host, port, rx) = stub_once("200 OK", r#"{"sessions":[{"id":"s7"},{"id":"s8"}]}"#);
        let client = RestClient::new(&host, port, "Secret-Token-AbC123");
        let sessions = client.list_sessions().expect("list_sessions");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "s7");
        assert_eq!(sessions[1].id, "s8");

        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("GET /api/sessions "), "path: {request}");
        let header_line = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or_else(|| panic!("no Authorization header: {request}"));
        // Case-sensitive on the value: catches any token mangling.
        assert_eq!(
            header_line.trim_start_matches(|c: char| c != ' '),
            " Bearer Secret-Token-AbC123",
            "auth header value corrupted: {header_line}"
        );
    }

    #[test]
    fn list_sessions_tolerates_bare_array() {
        let (host, port, _rx) = stub_once("200 OK", r#"[{"id":"a"},{"id":"b"},{"id":"c"}]"#);
        let client = RestClient::new(&host, port, "");
        let sessions = client.list_sessions().expect("list_sessions");
        assert_eq!(sessions.len(), 3);
    }

    #[test]
    fn non_2xx_maps_to_http_error_with_status_and_body() {
        let (host, port, _rx) = stub_once("404 Not Found", "no such endpoint");
        let client = RestClient::new(&host, port, "");
        let err = client.list_sessions().expect_err("404 must error");
        match err {
            GatewayError::Http { status, body } => {
                assert_eq!(status, 404);
                assert_eq!(body, "no such endpoint");
            }
            other => panic!("expected GatewayError::Http, got {other:?}"),
        }
    }

    #[test]
    fn stalled_server_errors_within_read_timeout_not_hang() {
        // A reachable endpoint that accepts the connection but never replies must
        // make a list call return a bounded Transport error, not block forever.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                std::thread::sleep(Duration::from_secs(2));
            }
        });
        let client = RestClient::with_timeouts(
            &addr.ip().to_string(),
            addr.port(),
            "",
            Duration::from_millis(500),
            Duration::from_millis(500),
        );
        let start = Instant::now();
        let result = client.list_sessions();
        let elapsed = start.elapsed();
        assert!(
            matches!(result, Err(GatewayError::Transport(_))),
            "got {result:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "must return within the read-timeout bound, took {elapsed:?}"
        );
    }

    #[test]
    fn session_messages_routes_to_per_session_path_and_returns_value() {
        let (host, port, rx) =
            stub_once("200 OK", r#"{"messages":[{"role":"user","content":"hi"}]}"#);
        let client = RestClient::new(&host, port, "");
        let val = client.session_messages("abc123").expect("session_messages");
        assert_eq!(val["messages"][0]["role"], "user");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/sessions/abc123/messages "),
            "path: {request}"
        );
    }

    #[test]
    fn session_messages_non_2xx_maps_to_http_error() {
        let (host, port, _rx) = stub_once("404 Not Found", "no such session");
        let client = RestClient::new(&host, port, "");
        let err = client
            .session_messages("missing")
            .expect_err("404 must error");
        match err {
            GatewayError::Http { status, body } => {
                assert_eq!(status, 404);
                assert!(body.contains("no such session"));
            }
            other => panic!("expected Http error, got {other:?}"),
        }
    }

    #[test]
    fn session_messages_invalid_json_is_decode_err() {
        let (host, port, _rx) = stub_once("200 OK", "not json");
        let client = RestClient::new(&host, port, "");
        let err = client
            .session_messages("x")
            .expect_err("invalid json must error");
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    // Session mutators (unit 14b)
    // ------------------------------------------------------------------------

    #[test]
    fn set_session_archived_sends_patch_with_archived_true() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client
            .set_session_archived("s1", true)
            .expect("set_session_archived");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("PATCH /api/sessions/s1 "),
            "path: {request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            body.contains(r#""archived":true"#),
            "body missing archived: {body}"
        );
    }

    #[test]
    fn set_session_title_sends_patch_with_title() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client
            .set_session_title("s1", "New Name")
            .expect("set_session_title");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("PATCH /api/sessions/s1 "),
            "path: {request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            body.contains(r#""title":"New Name""#),
            "body missing title: {body}"
        );
    }

    #[test]
    fn delete_session_sends_delete() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client.delete_session("s1").expect("delete_session");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("DELETE /api/sessions/s1 "),
            "path: {request}"
        );
    }

    #[test]
    fn search_sessions_routes_and_decodes() {
        let (host, port, rx) = stub_once("200 OK", r#"{"data":[{"id":"a"}]}"#);
        let client = RestClient::new(&host, port, "");
        let sessions = client.search_sessions("foo").expect("search_sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "a");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/sessions/search?q=foo "),
            "path: {request}"
        );
    }

    // ------------------------------------------------------------------------
    // Typed HermesSession fixture tests (unit 14) — pure decode_sessions, no server
    // ------------------------------------------------------------------------

    #[test]
    fn decode_sessions_envelope_with_title_and_message_count() {
        let sessions =
            decode_sessions(r#"{"sessions":[{"id":"S1","title":"hi","message_count":3}]}"#)
                .unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "S1");
        assert_eq!(sessions[0].title.as_deref(), Some("hi"));
        assert_eq!(sessions[0].message_count, Some(3));
    }

    #[test]
    fn decode_sessions_data_key_envelope() {
        let sessions = decode_sessions(r#"{"data":[{"id":"S2"}]}"#).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "S2");
    }

    #[test]
    fn decode_sessions_bare_array() {
        let sessions = decode_sessions(r#"[{"id":"S3"}]"#).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "S3");
    }

    #[test]
    fn decode_sessions_null_data_is_empty() {
        let sessions = decode_sessions(r#"{"data":null}"#).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn decode_sessions_empty_object_is_empty() {
        let sessions = decode_sessions(r#"{}"#).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn decode_sessions_minimal_id_all_fields_default() {
        let sessions = decode_sessions(r#"{"sessions":[{"id":"S4"}]}"#).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "S4");
        assert!(sessions[0].source.is_none());
        assert!(sessions[0].model.is_none());
        assert!(sessions[0].title.is_none());
        assert!(sessions[0].started_at.is_none());
        assert!(sessions[0].ended_at.is_none());
        assert!(sessions[0].end_reason.is_none());
        assert!(sessions[0].message_count.is_none());
        assert!(sessions[0].tool_call_count.is_none());
        assert!(sessions[0].input_tokens.is_none());
        assert!(sessions[0].output_tokens.is_none());
        assert!(sessions[0].preview.is_none());
        assert!(sessions[0].last_active.is_none());
    }

    #[test]
    fn decode_sessions_invalid_json_is_decode_err() {
        let err = decode_sessions("nope").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    // ------------------------------------------------------------------------
    // Typed scheduled-jobs surface (unit 10) — decode_jobs fixtures + the
    // `/api/cron/jobs` route/verb wiring (list/pause/resume/trigger GET/POST,
    // create POST-flat, update PUT-wraps body shape).
    // ------------------------------------------------------------------------

    #[test]
    fn decode_jobs_reads_envelope() {
        let jobs = decode_jobs(r#"{"jobs":[{"id":"J1","name":"n","prompt":"p"}]}"#).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "J1");
        assert_eq!(jobs[0].name, "n");
        assert_eq!(jobs[0].prompt, "p");
    }

    #[test]
    fn decode_jobs_null_jobs_is_empty() {
        assert!(decode_jobs(r#"{"jobs":null}"#).unwrap().is_empty());
    }

    #[test]
    fn decode_jobs_missing_jobs_is_empty() {
        assert!(decode_jobs(r#"{}"#).unwrap().is_empty());
    }

    #[test]
    fn decode_jobs_null_skills_and_toolsets_become_empty_vecs() {
        let jobs = decode_jobs(
            r#"{"jobs":[{"id":"J1","name":"n","prompt":"p","skills":null,"enabled_toolsets":null}]}"#,
        )
        .unwrap();
        assert!(jobs[0].skills.is_empty());
        assert!(jobs[0].enabled_toolsets.is_empty());
    }

    #[test]
    fn decode_jobs_nested_schedule_decodes() {
        let jobs = decode_jobs(
            r#"{"jobs":[{"id":"J1","name":"n","prompt":"p","schedule":{"kind":"cron","expr":"0 9 * * *","display":"daily"}}]}"#,
        )
        .unwrap();
        let sched = jobs[0].schedule.as_ref().expect("schedule present");
        assert_eq!(sched.kind.as_deref(), Some("cron"));
        assert_eq!(sched.expr.as_deref(), Some("0 9 * * *"));
        assert_eq!(sched.display.as_deref(), Some("daily"));
    }

    #[test]
    fn decode_jobs_invalid_json_is_decode_err() {
        let err = decode_jobs("not json").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn list_jobs_decodes_typed_scheduled_job() {
        let (host, port, _rx) = stub_once(
            "200 OK",
            r#"{"jobs":[{"id":"J1","name":"n","prompt":"p"}]}"#,
        );
        let client = RestClient::new(&host, port, "");
        let jobs = client.list_jobs().expect("list_jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "J1");
    }

    #[test]
    fn pause_job_hits_pause_path() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client.pause_job("J1").expect("pause_job");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("POST /api/cron/jobs/J1/pause "),
            "path: {request}"
        );
    }

    #[test]
    fn resume_job_hits_resume_path() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client.resume_job("J1").expect("resume_job");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("POST /api/cron/jobs/J1/resume "),
            "path: {request}"
        );
    }

    #[test]
    fn trigger_job_hits_cron_trigger_path_not_bare_run() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client.trigger_job("J1").expect("trigger_job");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("POST /api/cron/jobs/J1/trigger "),
            "path: {request}"
        );
        // Guard the 2026-07-02 route move: the leaf is `/trigger` under
        // `/api/cron/jobs/`, not the old bare `/api/jobs/{id}/run` (which now 404s).
        assert!(
            !request.contains("/run"),
            "must not use the old /run leaf: {request}"
        );
    }

    #[test]
    fn create_job_posts_flat_spec_without_updates_wrapper() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client
            .create_job(&serde_json::json!({"name": "n", "prompt": "p"}))
            .expect("create_job");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("POST /api/cron/jobs "),
            "path: {request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            !body.contains("updates"),
            "create body must be flat: {body}"
        );
        assert!(
            body.contains("\"name\""),
            "create body missing spec fields: {body}"
        );
    }

    #[test]
    fn update_job_wraps_body_in_updates_key() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client
            .update_job("J1", &serde_json::json!({"prompt": "new"}))
            .expect("update_job");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("PUT /api/cron/jobs/J1 "),
            "path: {request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            body.contains("\"updates\""),
            "update body must wrap in updates: {body}"
        );
        assert!(
            body.contains("\"prompt\""),
            "update body missing wrapped fields: {body}"
        );
    }

    // ------------------------------------------------------------------------
    // Skills / toolsets (unit 12)
    // ------------------------------------------------------------------------

    // Pure decoder fixtures — no server needed

    #[test]
    fn decode_skills_skills_key_with_all_fields() {
        let skills =
            decode_skills(r#"{"skills":[{"name":"a","description":"d","version":"1"}]}"#).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "a");
        assert_eq!(skills[0].description.as_deref(), Some("d"));
        assert_eq!(skills[0].version.as_deref(), Some("1"));
    }

    #[test]
    fn decode_skills_data_key_with_defaults() {
        let skills = decode_skills(r#"{"data":[{"name":"a"}]}"#).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "a");
        assert!(skills[0].category.is_none());
        assert!(skills[0].description.is_none());
        assert!(!skills[0].enabled);
        assert!(skills[0].version.is_none());
    }

    #[test]
    fn decode_skills_full_contract_shape() {
        // The desktop's `SkillInfo {name, category, description, enabled}`.
        let skills = decode_skills(
            r#"{"skills":[{"name":"a","category":"coding","description":"d","enabled":true}]}"#,
        )
        .unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].category.as_deref(), Some("coding"));
        assert_eq!(skills[0].description.as_deref(), Some("d"));
        assert!(skills[0].enabled);
    }

    #[test]
    fn decode_skills_bare_array() {
        let skills = decode_skills(r#"[{"name":"x"}]"#).unwrap();
        assert_eq!(skills.len(), 1);
    }

    #[test]
    fn decode_skills_null_key_is_empty() {
        let skills = decode_skills(r#"{"skills":null}"#).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn decode_skills_missing_key_is_empty() {
        let skills = decode_skills(r#"{}"#).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn decode_skills_invalid_json_is_err() {
        let err = decode_skills("not json").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn decode_toolsets_toolsets_key_with_tools() {
        let toolsets = decode_toolsets(r#"{"toolsets":[{"name":"t","tools":["x","y"]}]}"#).unwrap();
        assert_eq!(toolsets.len(), 1);
        assert_eq!(toolsets[0].name, "t");
        assert_eq!(toolsets[0].tools, vec!["x", "y"]);
    }

    #[test]
    fn decode_toolsets_data_key_with_defaults() {
        let toolsets = decode_toolsets(r#"{"data":[{"name":"t"}]}"#).unwrap();
        assert_eq!(toolsets.len(), 1);
        assert_eq!(toolsets[0].name, "t");
        assert!(toolsets[0].label.is_none());
        assert!(toolsets[0].description.is_none());
        assert!(!toolsets[0].enabled);
        assert!(!toolsets[0].configured);
        assert!(toolsets[0].tools.is_empty());
    }

    #[test]
    fn decode_toolsets_full_contract_shape() {
        // The desktop's `ToolsetInfo {name, label, description, enabled, configured, tools[]}`.
        let toolsets = decode_toolsets(
            r#"{"toolsets":[{"name":"web","label":"Web","description":"d","enabled":true,"configured":false,"tools":["search","fetch"]}]}"#,
        )
        .unwrap();
        assert_eq!(toolsets.len(), 1);
        assert_eq!(toolsets[0].label.as_deref(), Some("Web"));
        assert_eq!(toolsets[0].description.as_deref(), Some("d"));
        assert!(toolsets[0].enabled);
        assert!(!toolsets[0].configured);
        assert_eq!(toolsets[0].tools, vec!["search", "fetch"]);
    }

    // Server-stub routing tests

    #[test]
    fn list_skills_routes_to_get_api_skills_and_decodes() {
        let (host, port, rx) = stub_once("200 OK", r#"{"skills":[{"name":"a"}]}"#);
        let client = RestClient::new(&host, port, "");
        let skills = client.list_skills().expect("list_skills");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "a");
        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("GET /api/skills "), "path: {request}");
    }

    #[test]
    fn list_toolsets_routes_to_get_api_tools_toolsets_and_decodes() {
        let (host, port, rx) = stub_once("200 OK", r#"{"toolsets":[{"name":"t"}]}"#);
        let client = RestClient::new(&host, port, "");
        let toolsets = client.list_toolsets().expect("list_toolsets");
        assert_eq!(toolsets.len(), 1);
        assert_eq!(toolsets[0].name, "t");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/tools/toolsets "),
            "path: {request}"
        );
    }

    #[test]
    fn toggle_skill_sends_put_with_name_and_enabled() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client.toggle_skill("myskill", true).expect("toggle_skill");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("PUT /api/skills/toggle "),
            "path: {request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            body.contains(r#""name":"myskill""#),
            "body missing name: {body}"
        );
        assert!(
            body.contains(r#""enabled":true"#),
            "body missing enabled: {body}"
        );
    }

    #[test]
    fn set_toolset_sends_put_with_updates_to_named_path() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client
            .set_toolset("ts1", &serde_json::json!({"enabled": false}))
            .expect("set_toolset");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("PUT /api/tools/toolsets/ts1 "),
            "path: {request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            body.contains(r#""enabled":false"#),
            "body missing enabled: {body}"
        );
    }

    // Config read/write (unit 22a)

    #[test]
    fn get_config_routes_to_config_path_and_returns_value() {
        let (host, port, rx) = stub_once("200 OK", r#"{"foo":"bar"}"#);
        let client = RestClient::new(&host, port, "");
        let val = client.get_config().expect("get_config");
        assert_eq!(val["foo"], "bar");
        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("GET /api/config "), "path: {request}");
    }

    #[test]
    fn get_config_defaults_routes_to_defaults_path() {
        let (host, port, rx) = stub_once("200 OK", r#"{"a":1}"#);
        let client = RestClient::new(&host, port, "");
        let val = client.get_config_defaults().expect("get_config_defaults");
        assert_eq!(val["a"], 1);
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/config/defaults "),
            "path: {request}"
        );
    }

    #[test]
    fn get_config_schema_routes_to_schema_path() {
        let (host, port, rx) = stub_once("200 OK", r#"{"type":"object"}"#);
        let client = RestClient::new(&host, port, "");
        let val = client.get_config_schema().expect("get_config_schema");
        assert_eq!(val["type"], "object");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/config/schema "),
            "path: {request}"
        );
    }

    #[test]
    fn set_config_sends_put_with_config_wrapper() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        let cfg = serde_json::json!({"theme": "dark"});
        client.set_config(&cfg).expect("set_config");
        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("PUT /api/config "), "path: {request}");
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            body.contains(r#""config""#),
            "body missing config wrapper: {body}"
        );
        assert!(
            body.contains(r#""theme":"dark""#),
            "body missing theme: {body}"
        );
    }

    #[test]
    fn get_config_non_2xx_maps_to_http_error() {
        let (host, port, _rx) = stub_once("404 Not Found", "");
        let client = RestClient::new(&host, port, "");
        let err = client.get_config().expect_err("404 must error");
        assert!(
            matches!(err, GatewayError::Http { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn get_config_invalid_json_is_decode_err() {
        let (host, port, _rx) = stub_once("200 OK", "not json");
        let client = RestClient::new(&host, port, "");
        let err = client.get_config().expect_err("invalid json must error");
        assert!(matches!(err, GatewayError::Decode(_)), "got {err:?}");
    }

    // Model/providers read passthroughs (unit 22c)

    #[test]
    fn model_info_routes_to_path() {
        let (host, port, rx) = stub_once("200 OK", r#"{"main":"claude"}"#);
        let client = RestClient::new(&host, port, "");
        let val = client.model_info().expect("model_info");
        assert_eq!(val["main"], "claude");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/model/info "),
            "path: {request}"
        );
    }

    #[test]
    fn model_options_routes_to_path() {
        let (host, port, rx) = stub_once("200 OK", r#"{"options":[]}"#);
        let client = RestClient::new(&host, port, "");
        let val = client.model_options().expect("model_options");
        assert!(val["options"].is_array(), "options should be array");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/model/options "),
            "path: {request}"
        );
    }

    #[test]
    fn model_recommended_default_routes_to_path() {
        let (host, port, rx) = stub_once("200 OK", r#"{"model":"x"}"#);
        let client = RestClient::new(&host, port, "");
        let val = client
            .model_recommended_default()
            .expect("model_recommended_default");
        assert_eq!(val["model"], "x");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/model/recommended-default "),
            "path: {request}"
        );
    }

    #[test]
    fn providers_oauth_routes_to_path() {
        let (host, port, rx) = stub_once("200 OK", r#"{"accounts":[]}"#);
        let client = RestClient::new(&host, port, "");
        let val = client.providers_oauth().expect("providers_oauth");
        assert!(val["accounts"].is_array(), "accounts should be array");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/providers/oauth "),
            "path: {request}"
        );
    }

    #[test]
    fn model_info_non_2xx_maps_to_http_error() {
        let (host, port, _rx) = stub_once("404 Not Found", "");
        let client = RestClient::new(&host, port, "");
        let err = client.model_info().expect_err("404 must error");
        assert!(
            matches!(err, GatewayError::Http { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn model_info_invalid_json_is_decode_err() {
        let (host, port, _rx) = stub_once("200 OK", "not json");
        let client = RestClient::new(&host, port, "");
        let err = client.model_info().expect_err("invalid json must error");
        assert!(matches!(err, GatewayError::Decode(_)), "got {err:?}");
    }

    #[test]
    fn get_env_routes_to_env_path_and_returns_value() {
        let (host, port, rx) = stub_once("200 OK", r#"{"OPENAI_API_KEY":"set"}"#);
        let client = RestClient::new(&host, port, "");
        let val = client.get_env().expect("get_env");
        assert_eq!(val["OPENAI_API_KEY"], "set");
        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("GET /api/env "), "path: {request}");
    }

    #[test]
    fn set_env_var_puts_to_env_path_with_body() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client
            .set_env_var("OPENAI_API_KEY", "sk-123")
            .expect("set_env_var");
        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("PUT /api/env "), "path: {request}");
        assert!(
            request.contains(r#""key":"OPENAI_API_KEY""#),
            "body: {request}"
        );
        assert!(request.contains(r#""value":"sk-123""#), "body: {request}");
    }

    #[test]
    fn delete_env_var_deletes_env_path_with_body() {
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client
            .delete_env_var("OPENAI_API_KEY")
            .expect("delete_env_var");
        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("DELETE /api/env "), "path: {request}");
        assert!(
            request.contains(r#""key":"OPENAI_API_KEY""#),
            "body: {request}"
        );
    }

    #[test]
    fn set_env_var_non_2xx_maps_to_http_error() {
        let (host, port, _rx) = stub_once("500 Internal Server Error", "");
        let client = RestClient::new(&host, port, "");
        let err = client.set_env_var("K", "V").expect_err("500 must error");
        assert!(
            matches!(err, GatewayError::Http { status: 500, .. }),
            "got {err:?}"
        );
    }

    // ------------------------------------------------------------------------
    // /v1/capabilities (unit 16a)
    // ------------------------------------------------------------------------

    #[test]
    fn decode_capabilities_parses_full_shape() {
        let caps = decode_capabilities(
            r#"{"object":"hermes.api_server.capabilities","auth":{"scheme":"bearer","required":true},"runtime":{"mode":"server_agent","tool_execution":"server"},"features":{"streaming":true,"approval_events":true,"session_continuity_header":"X-Hermes-Session-Id"},"endpoints":{"submit_run":{"method":"POST","path":"/v1/runs"}}}"#,
        )
        .expect("decode full shape");
        assert_eq!(
            caps.object.as_deref(),
            Some("hermes.api_server.capabilities")
        );
        let auth = caps.auth.as_ref().unwrap();
        assert_eq!(auth.scheme.as_deref(), Some("bearer"));
        let runtime = caps.runtime.as_ref().unwrap();
        assert_eq!(runtime.mode.as_deref(), Some("server_agent"));
        assert!(caps.feature("streaming"));
        assert!(!caps.feature("nonexistent"));
        assert_eq!(
            caps.feature_str("session_continuity_header"),
            Some("X-Hermes-Session-Id")
        );
        assert_eq!(caps.feature_bool("approval_events"), Some(true));
        let ep = caps.endpoints.get("submit_run").unwrap();
        assert_eq!(ep.path, "/v1/runs");
    }

    #[test]
    fn decode_capabilities_tolerates_missing_and_unknown() {
        let caps = decode_capabilities("{}").expect("empty object");
        assert!(caps.object.is_none());
        assert!(caps.auth.is_none());
        assert!(caps.features.is_empty());
        assert!(caps.endpoints.is_empty());
        assert!(!caps.feature("x"));

        let caps = decode_capabilities(r#"{"features":{"future_flag":true,"a_string":"v"}}"#)
            .expect("mixed features");
        assert!(caps.feature("future_flag"));
        assert_eq!(caps.feature_str("a_string"), Some("v"));
    }

    #[test]
    fn feature_accessors_reject_type_mismatch() {
        // The real server mixes value types in `features` (bool flags alongside
        // string entries), so the typed accessors must degrade — not panic and
        // not coerce — when a key is present but the WRONG type. Existing tests
        // only hit the matched-type happy paths (bool key -> Some(bool), string
        // key -> Some(str)) and the missing-key arm; the cross-type arms
        // (feature_bool on a string, feature_str on a bool) were unguarded, as
        // was the documented `feature()` contract "a non-boolean value reads as
        // false".
        let caps = decode_capabilities(
            r#"{"features":{"session_continuity_header":"X-Hermes-Session-Id","streaming":true}}"#,
        )
        .expect("mixed-type features");

        // feature_bool on a STRING-valued key -> None (Value::as_bool fails),
        // and the public feature() wrapper therefore reports false, not a panic.
        assert_eq!(caps.feature_bool("session_continuity_header"), None);
        assert!(!caps.feature("session_continuity_header"));

        // feature_str on a BOOL-valued key -> None (Value::as_str fails).
        assert_eq!(caps.feature_str("streaming"), None);

        // Sanity: the matched-type reads still work on the same map, proving the
        // None results above are type-mismatch rejections, not a missing map.
        assert_eq!(caps.feature_bool("streaming"), Some(true));
        assert_eq!(
            caps.feature_str("session_continuity_header"),
            Some("X-Hermes-Session-Id")
        );
    }

    #[test]
    fn decode_capabilities_invalid_json_is_decode_err() {
        let err = decode_capabilities("not json").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn capabilities_routes_to_v1_path_and_sends_bearer() {
        let (host, port, rx) = stub_once(
            "200 OK",
            r#"{"object":"hermes.api_server.capabilities","auth":{"scheme":"bearer","required":true},"runtime":{"mode":"server_agent","tool_execution":"server"},"features":{"streaming":true,"approval_events":true,"session_continuity_header":"X-Hermes-Session-Id"},"endpoints":{"submit_run":{"method":"POST","path":"/v1/runs"}}}"#,
        );
        let client = RestClient::new(&host, port, "Tok-AbC1");
        let caps = client.capabilities().expect("capabilities");
        assert!(caps.feature("streaming"));
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /v1/capabilities "),
            "path: {request}"
        );
        let header_line = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or_else(|| panic!("no Authorization header: {request}"));
        assert_eq!(
            header_line.trim_start_matches(|c: char| c != ' '),
            " Bearer Tok-AbC1",
            "auth header value corrupted: {header_line}"
        );
    }

    #[test]
    fn capabilities_404_is_http_err() {
        let (host, port, _rx) = stub_once("404 Not Found", "nope");
        let client = RestClient::new(&host, port, "");
        let err = client.capabilities().expect_err("404 must error");
        assert!(
            matches!(err, GatewayError::Http { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn decode_models_parses_full_shape() {
        let models = decode_models(
            r#"{"object":"list","data":[{"id":"hermes-agent","object":"model","created":1700000000,"owned_by":"hermes"}]}"#,
        )
        .expect("full shape");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "hermes-agent");
        assert_eq!(models[0].owned_by.as_deref(), Some("hermes"));
    }

    #[test]
    fn decode_models_tolerates_missing_and_unknown() {
        let models = decode_models("{}").expect("empty object");
        assert!(models.is_empty());

        let models =
            decode_models(r#"{"future":true,"data":[{"id":"m1"}]}"#).expect("extra fields");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "m1");
        assert!(models[0].object.is_none());
        assert!(models[0].created.is_none());
        assert!(models[0].owned_by.is_none());
    }

    #[test]
    fn decode_models_invalid_json_is_decode_err() {
        let err = decode_models("not json").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn models_routes_to_v1_path_and_sends_bearer() {
        let (host, port, rx) = stub_once(
            "200 OK",
            r#"{"object":"list","data":[{"id":"hermes-agent"}]}"#,
        );
        let client = RestClient::new(&host, port, "Tok-AbC1");
        let models = client.models().expect("models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "hermes-agent");
        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("GET /v1/models "), "path: {request}");
        let header_line = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or_else(|| panic!("no Authorization header: {request}"));
        assert_eq!(
            header_line.trim_start_matches(|c: char| c != ' '),
            " Bearer Tok-AbC1",
            "auth header value corrupted: {header_line}"
        );
    }

    #[test]
    fn models_404_is_http_err() {
        let (host, port, _rx) = stub_once("404 Not Found", "nope");
        let client = RestClient::new(&host, port, "");
        let err = client.models().expect_err("404 must error");
        assert!(
            matches!(err, GatewayError::Http { status: 404, .. }),
            "got {err:?}"
        );
    }

    // ------------------------------------------------------------------------
    // Command Center decoders (unit 20)
    // ------------------------------------------------------------------------

    #[test]
    fn status_full_shape_parses() {
        let body = r#"{
            "version": "0.17.0",
            "release_date": "2026.6.19",
            "config_version": 30,
            "latest_config_version": 30,
            "can_update_hermes": true,
            "gateway_running": true,
            "gateway_state": "running",
            "gateway_platforms": {
                "email": {"state": "connected", "error_code": null, "error_message": null, "updated_at": "2026-06-29T15:21:24.524565+00:00"},
                "sms": {"state": "connected", "error_code": null, "error_message": null, "updated_at": "2026-06-29T16:29:12.215584+00:00"}
            },
            "gateway_exit_reason": null,
            "gateway_updated_at": "2026-06-29T16:29:12.215566+00:00",
            "active_agents": 0,
            "gateway_busy": false,
            "gateway_drainable": true,
            "restart_drain_timeout": 180.0,
            "active_sessions": 0,
            "auth_required": false,
            "auth_providers": [],
            "hermes_home": "/Users/example/.hermes",
            "config_path": "/Users/example/.hermes/config.yaml",
            "env_path": "/Users/example/.hermes/.env",
            "gateway_pid": 1375,
            "gateway_health_url": null
        }"#;
        let s = decode_status(body).expect("full shape");
        assert_eq!(s.version.as_deref(), Some("0.17.0"));
        assert!(s.gateway_running);
        assert_eq!(s.active_agents, 0);
        assert_eq!(s.gateway_platforms.len(), 2);
        assert_eq!(
            s.gateway_platforms["email"].state.as_deref(),
            Some("connected")
        );
        assert!(s.auth_providers.is_empty());
        assert_eq!(s.restart_drain_timeout, Some(180.0));
        assert_eq!(s.gateway_pid, Some(1375));
        assert!(s.gateway_exit_reason.is_none());
        assert!(s.gateway_health_url.is_none());
    }

    #[test]
    fn status_missing_and_unknown_fields_tolerated() {
        let s = decode_status(r#"{"version":"9.9","extra_unknown":123}"#).expect("partial");
        assert_eq!(s.version.as_deref(), Some("9.9"));
        assert!(!s.gateway_running);
        assert!(s.gateway_platforms.is_empty());
    }

    #[test]
    fn status_invalid_is_decode_err() {
        let err = decode_status("{ not json").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn hermes_update_check_full_shape_parses() {
        // Verbatim shape from a live `GET /api/hermes/update/check` (git install,
        // 2026-07-02): update available, commits list populated.
        let body = r#"{
            "install_method": "git",
            "current_version": "0.17.0",
            "behind": 1210,
            "update_available": true,
            "can_apply": true,
            "update_command": "hermes update",
            "message": null,
            "commits": [
                {"sha":"201b646","summary":"fix(gateway): coverage","author":"kshitijk4poor","at":1783030603},
                {"sha":"90b618f","summary":"fix(gateway): keep idle agents","author":"Hermes Trismegistus","at":1783030603}
            ]
        }"#;
        let u = decode_hermes_update_check(body).expect("full shape");
        assert_eq!(u.install_method.as_deref(), Some("git"));
        assert_eq!(u.current_version.as_deref(), Some("0.17.0"));
        assert_eq!(u.behind, 1210);
        assert!(u.update_available);
        assert!(u.can_apply);
        assert_eq!(u.update_command.as_deref(), Some("hermes update"));
        assert!(u.message.is_none());
        assert_eq!(u.commits.len(), 2);
        assert_eq!(u.commits[0].sha.as_deref(), Some("201b646"));
        assert_eq!(u.commits[0].at, Some(1783030603));
    }

    #[test]
    fn hermes_update_check_up_to_date_defaults() {
        // The "nothing to do" shape: absent flags default to false/0/empty, so a
        // terse up-to-date body decodes without update affordances lighting up.
        let u = decode_hermes_update_check(r#"{"current_version":"0.17.0"}"#).expect("minimal");
        assert!(!u.update_available);
        assert!(!u.can_apply);
        assert_eq!(u.behind, 0);
        assert!(u.commits.is_empty());
    }

    #[test]
    fn hermes_update_check_null_commits_tolerated() {
        // `commits: null` must map to an empty vec, not a decode error.
        let u = decode_hermes_update_check(r#"{"update_available":true,"commits":null}"#)
            .expect("null commits tolerated");
        assert!(u.update_available);
        assert!(u.commits.is_empty());
    }

    #[test]
    fn hermes_update_check_invalid_is_decode_err() {
        let err = decode_hermes_update_check("{ not json").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn agent_log_full_shape_parses() {
        let log =
            decode_agent_log(r#"{"file":"agent","lines":["a\n","b\n"]}"#).expect("full shape");
        assert_eq!(log.file.as_deref(), Some("agent"));
        assert_eq!(log.lines, vec!["a\n", "b\n"]);
    }

    #[test]
    fn agent_log_absent_lines_is_empty() {
        let log = decode_agent_log(r#"{"file":"agent"}"#).expect("no lines");
        assert!(log.lines.is_empty());
    }

    #[test]
    fn usage_full_shape_parses() {
        let body = r#"{
            "daily": [
                {"day": "2026-06-24", "input_tokens": 425031, "output_tokens": 28957, "cache_read_tokens": 3009408, "reasoning_tokens": 4164, "estimated_cost": 0.0, "actual_cost": 0, "sessions": 7, "api_calls": 99}
            ],
            "by_model": [
                {"model": "gpt-5.5", "input_tokens": 2378708, "output_tokens": 146987, "estimated_cost": 0.0, "sessions": 57, "api_calls": 423}
            ],
            "totals": {"total_input": 3096199, "total_output": 382988, "total_cache_read": 29947373, "total_reasoning": 21477, "total_estimated_cost": 0.00323148, "total_actual_cost": 0, "total_sessions": 85, "total_api_calls": 841},
            "period_days": 7,
            "skills": {
                "summary": {"total_skill_loads": 25, "total_skill_edits": 1, "total_skill_actions": 26, "distinct_skills_used": 13},
                "top_skills": [
                    {"skill": "ebay-search", "view_count": 5, "manage_count": 0, "total_count": 5, "percentage": 19.23, "last_used_at": 1782709600.852185}
                ]
            }
        }"#;
        let u = decode_usage(body).expect("full shape");
        assert_eq!(u.daily.len(), 1);
        assert_eq!(u.daily[0].input_tokens, 425031);
        assert_eq!(u.daily[0].actual_cost, 0.0);
        assert_eq!(u.by_model[0].model.as_deref(), Some("gpt-5.5"));
        assert_eq!(u.totals.total_input, 3096199);
        assert_eq!(u.period_days, Some(7));
        assert_eq!(u.skills.summary.distinct_skills_used, 13);
        assert_eq!(u.skills.top_skills[0].skill.as_deref(), Some("ebay-search"));
        assert!((u.skills.top_skills[0].percentage - 19.23).abs() < 1e-6);
    }

    #[test]
    fn usage_missing_totals_and_skills_defaulted() {
        let u = decode_usage(r#"{"daily":[],"by_model":[]}"#).expect("minimal");
        assert_eq!(u.totals, UsageTotals::default());
        assert!(u.skills.top_skills.is_empty());
    }

    #[test]
    fn usage_invalid_is_decode_err() {
        let err = decode_usage("nope").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn explicit_null_collections_default_not_error() {
        // `#[serde(default)]` alone rejects an explicit `null`; the Command
        // Center collection/map/nested-struct fields opt into null_to_default /
        // null_to_empty_vec so a "no data" response that emits `null` (rather
        // than omitting the key) still decodes.
        let s = decode_status(r#"{"gateway_platforms":null,"auth_providers":null}"#)
            .expect("null platforms/providers tolerated");
        assert!(s.gateway_platforms.is_empty());
        assert!(s.auth_providers.is_empty());

        let log =
            decode_agent_log(r#"{"file":"agent","lines":null}"#).expect("null lines tolerated");
        assert!(log.lines.is_empty());

        let u = decode_usage(r#"{"daily":null,"by_model":null,"totals":null,"skills":null}"#)
            .expect("null usage collections tolerated");
        assert!(u.daily.is_empty());
        assert!(u.by_model.is_empty());
        assert_eq!(u.totals, UsageTotals::default());
        assert!(u.skills.top_skills.is_empty());

        let u2 = decode_usage(r#"{"skills":{"summary":null,"top_skills":null}}"#)
            .expect("null nested skills tolerated");
        assert_eq!(u2.skills.summary, SkillsSummary::default());
        assert!(u2.skills.top_skills.is_empty());
    }

    #[test]
    fn roll_up_usage_sorts_models_and_caps() {
        let body = r#"{
            "daily": [{"day":"2026-06-24","input_tokens":100,"output_tokens":50,"cache_read_tokens":0,"reasoning_tokens":0,"estimated_cost":0.0,"actual_cost":0,"sessions":1,"api_calls":1}],
            "by_model": [
                {"model":"big","input_tokens":300,"output_tokens":100,"estimated_cost":0.0,"sessions":2,"api_calls":5},
                {"model":"mid","input_tokens":200,"output_tokens":50,"estimated_cost":0.0,"sessions":1,"api_calls":3},
                {"model":"small","input_tokens":50,"output_tokens":10,"estimated_cost":0.0,"sessions":1,"api_calls":1}
            ],
            "totals": {"total_input":550,"total_output":160,"total_cache_read":0,"total_reasoning":0,"total_estimated_cost":0.0,"total_actual_cost":0,"total_sessions":4,"total_api_calls":9},
            "period_days": 1,
            "skills": {
                "summary": {"total_skill_loads":1,"total_skill_edits":0,"total_skill_actions":1,"distinct_skills_used":1},
                "top_skills": [
                    {"skill":"a","view_count":3,"manage_count":0,"total_count":3,"percentage":100.0,"last_used_at":0.0},
                    {"skill":"b","view_count":2,"manage_count":0,"total_count":2,"percentage":50.0,"last_used_at":0.0},
                    {"skill":"c","view_count":1,"manage_count":0,"total_count":1,"percentage":25.0,"last_used_at":0.0}
                ]
            }
        }"#;
        let a = decode_usage(body).expect("full shape");
        let r = roll_up_usage(&a, 2);
        assert_eq!(r.top_models.len(), 2);
        assert_eq!(r.top_models[0].model.as_deref(), Some("big"));
        assert_eq!(r.top_skills.len(), 2);
        assert_eq!(r.top_skills[0].skill.as_deref(), Some("a"));
        assert_eq!(r.per_day.len(), 1);
        assert_eq!(r.per_day[0].day, "2026-06-24");
        assert_eq!(r.per_day[0].input_tokens, 100);
        assert_eq!(r.totals.total_input, 550);
    }

    #[test]
    fn roll_up_usage_empty() {
        let a = UsageAnalytics::default();
        let r = roll_up_usage(&a, 5);
        assert!(r.per_day.is_empty());
        assert!(r.top_models.is_empty());
        assert!(r.top_skills.is_empty());
        assert_eq!(r.totals, UsageTotals::default());
    }

    #[test]
    fn roll_up_usage_missing_day_renders_empty() {
        // DailyUsage.day is Option<String> (#[serde(default)]), so the server
        // can omit it on a partial daily row. roll_up_usage maps None -> ""
        // via unwrap_or_default (the documented "a missing `day` renders as
        // empty string" contract on UsageRollup). The happy-path rollup test
        // always supplies a day, and roll_up_usage_empty has no daily rows at
        // all, so this None arm was uncovered. Assert the token fields still
        // carry through so this proves the empty-string is the day fallback,
        // not a dropped/blank row.
        let a = decode_usage(r#"{"daily":[{"input_tokens":42,"output_tokens":7}]}"#)
            .expect("daily row without a day field");
        assert_eq!(a.daily[0].day, None, "precondition: day omitted -> None");

        let r = roll_up_usage(&a, 5);
        assert_eq!(r.per_day.len(), 1);
        assert_eq!(r.per_day[0].day, "", "missing day renders as empty string");
        assert_eq!(r.per_day[0].input_tokens, 42);
        assert_eq!(r.per_day[0].output_tokens, 7);
    }

    #[test]
    fn status_routes_to_api_path_and_sends_bearer() {
        let (host, port, rx) = stub_once("200 OK", r#"{"version":"1.0"}"#);
        let client = RestClient::new(&host, port, "Tkn-Xyz");
        let s = client.status().expect("status");
        assert_eq!(s.version.as_deref(), Some("1.0"));
        let request = rx.recv().expect("captured request");
        assert!(request.starts_with("GET /api/status "), "path: {request}");
        let header_line = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or_else(|| panic!("no Authorization header: {request}"));
        assert_eq!(
            header_line.trim_start_matches(|c: char| c != ' '),
            " Bearer Tkn-Xyz",
            "auth header value corrupted: {header_line}"
        );
    }

    #[test]
    fn agent_log_routes_with_file_and_lines_query_and_decodes() {
        // `agent_log` is the only RestClient method whose routing was untested:
        // every prior agent-log test drives the pure `decode_agent_log` decoder,
        // never the method that builds the request. Its URL wiring is
        // method-specific — `GET /api/logs?file=<file>&lines=<lines>` where the
        // numeric `lines` goes through `lines.to_string()`. A refactor that
        // swapped the two query keys, dropped a param, used the wrong path, or
        // wired the wrong decoder would pass every decoder-only test yet break
        // the live tail. Capture the request line to pin all of it at once, and
        // confirm the body still decodes through to `AgentLog`.
        let (host, port, rx) = stub_once("200 OK", r#"{"file":"agent","lines":["one\n","two\n"]}"#);
        let client = RestClient::new(&host, port, "Log-Tok-42");
        let log = client.agent_log("agent", 50).expect("agent_log");
        assert_eq!(log.file.as_deref(), Some("agent"));
        assert_eq!(log.lines, vec!["one\n".to_string(), "two\n".to_string()]);

        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/logs?"),
            "must route to /api/logs with a query string: {request}"
        );
        // Both params must be present with their exact wired values — the `lines`
        // key proves the `u32 -> to_string()` conversion, `file` proves the arg
        // is threaded verbatim, and asserting both catches a swapped-key regression.
        assert!(
            request.contains("file=agent"),
            "missing file param: {request}"
        );
        assert!(
            request.contains("lines=50"),
            "missing lines param: {request}"
        );
        let header_line = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or_else(|| panic!("no Authorization header: {request}"));
        assert_eq!(
            header_line.trim_start_matches(|c: char| c != ' '),
            " Bearer Log-Tok-42",
            "auth header value corrupted: {header_line}"
        );
    }

    #[test]
    fn usage_404_is_http_err() {
        let (host, port, _rx) = stub_once("404 Not Found", "{}");
        let client = RestClient::new(&host, port, "");
        let err = client.usage(7).expect_err("404 must error");
        assert!(
            matches!(err, GatewayError::Http { status: 404, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn usage_routes_with_days_query_and_decodes() {
        // Sibling gap to `agent_log`'s routing hole: every other `usage` test
        // drives the pure `decode_usage` decoder, and `usage_404_is_http_err`
        // discards its captured request — so `usage`'s own request wiring
        // (`GET /api/analytics/usage?days=<days>` with the numeric `days`
        // threaded through `days.to_string()`) had ZERO coverage. A refactor
        // that used the wrong path (e.g. `/api/usage`), renamed/dropped the
        // `days` key, or wired the wrong decoder would pass every decode-only
        // test and the 404 test yet silently break the live Usage section.
        // Capture the request line to pin path + query at once, and confirm the
        // 200 body still decodes through to `UsageAnalytics`.
        let (host, port, rx) = stub_once("200 OK", r#"{"daily":[],"by_model":[]}"#);
        let client = RestClient::new(&host, port, "Usage-Tok-9");
        let usage = client.usage(14).expect("usage");
        assert!(usage.daily.is_empty());
        assert!(usage.by_model.is_empty());

        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/analytics/usage?"),
            "must route to /api/analytics/usage with a query string: {request}"
        );
        // Anchor on the `?` boundary so this pins the exact key + the
        // `u32 -> to_string()` conversion and can't be fooled by a renamed
        // `usage_days=`/`xdays=` param; `usage` sends exactly one query param.
        assert!(
            request.contains("/api/analytics/usage?days=14"),
            "missing/renamed days param: {request}"
        );
        let header_line = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or_else(|| panic!("no Authorization header: {request}"));
        assert_eq!(
            header_line.trim_start_matches(|c: char| c != ' '),
            " Bearer Usage-Tok-9",
            "auth header value corrupted: {header_line}"
        );
    }

    // ------------------------------------------------------------------------
    // Messaging-connectors decoders (unit 24)
    // ------------------------------------------------------------------------

    #[test]
    fn decode_messaging_platforms_full_shape() {
        let body = r#"{"env_path":"/Users/example/.hermes/.env","gateway_start_command":"hermes gateway start","platforms":[{"id":"telegram","name":"Telegram","description":"Run Hermes from Telegram DMs, groups, and topics.","docs_url":"https://core.telegram.org/bots/features#botfather","enabled":false,"configured":false,"gateway_running":true,"state":"disabled","error_code":null,"error_message":null,"updated_at":null,"home_channel":null,"env_vars":[{"key":"TELEGRAM_BOT_TOKEN","required":true,"is_set":false,"redacted_value":null,"description":"Telegram bot token from @BotFather","prompt":"Telegram bot token","help":"","url":"https://t.me/BotFather","is_password":true,"advanced":false}]},{"id":"discord","name":"Discord","description":"Connect Hermes to Discord DMs, channels, and threads.","docs_url":"https://discord.com/developers/applications","enabled":false,"configured":false,"gateway_running":true,"state":"disabled","error_code":null,"error_message":"failed to reconnect","updated_at":"2026-05-30T14:29:42.084787+00:00","home_channel":null,"env_vars":[{"key":"DISCORD_BOT_TOKEN","required":true,"is_set":false,"redacted_value":null,"description":"Discord bot token from Developer Portal","prompt":"Discord bot token","help":"","url":"https://discord.com/developers/applications","is_password":true,"advanced":false}]},{"id":"email","name":"Email","description":"Talk to Hermes through an IMAP/SMTP mailbox.","docs_url":"https://hermes-agent.nousresearch.com/docs/user-guide/messaging/","enabled":true,"configured":true,"gateway_running":true,"state":"connected","error_code":null,"error_message":null,"updated_at":"2026-06-29T15:21:24.524565+00:00","home_channel":{"platform":"email","chat_id":"user@example.org","name":"Home"},"env_vars":[{"key":"EMAIL_ADDRESS","required":true,"is_set":true,"redacted_value":"herm....org","description":"Email account address","prompt":"Email address","help":"","url":null,"is_password":false,"advanced":false}]}]}"#;
        let p = decode_messaging_platforms(body).expect("full shape");
        assert_eq!(p.platforms.len(), 3);
        assert_eq!(p.platforms[0].id, "telegram");
        assert!(p.platforms[0].home_channel.is_none());
        assert!(p.platforms[0].env_vars[0].is_password);
        assert_eq!(
            p.platforms[1].error_message.as_deref(),
            Some("failed to reconnect")
        );
        assert_eq!(p.platforms[2].id, "email");
        assert!(p.platforms[2].enabled);
        assert!(p.platforms[2].configured);
        assert_eq!(
            p.platforms[2].home_channel.as_ref().unwrap().chat_id,
            "user@example.org"
        );
        assert!(!p.platforms[2].env_vars[0].is_password);
    }

    #[test]
    fn decode_messaging_missing_keys_tolerant() {
        let p =
            decode_messaging_platforms(r#"{"platforms":[{"id":"x"}]}"#).expect("minimal platform");
        assert_eq!(p.platforms.len(), 1);
        assert_eq!(p.platforms[0].id, "x");
        assert_eq!(p.platforms[0].name, "");
        assert!(p.platforms[0].env_vars.is_empty());
        assert!(p.platforms[0].home_channel.is_none());
        assert!(!p.platforms[0].enabled);
    }

    #[test]
    fn decode_messaging_explicit_null_collections_default_not_error() {
        let p = decode_messaging_platforms(
            r#"{"platforms":null,"env_path":"p","gateway_start_command":"g"}"#,
        )
        .expect("null platforms");
        assert!(p.platforms.is_empty());
        let p2 = decode_messaging_platforms(r#"{"platforms":[{"id":"x","env_vars":null}],"env_path":"p","gateway_start_command":"g"}"#).expect("null env_vars");
        assert!(p2.platforms[0].env_vars.is_empty());
    }

    #[test]
    fn decode_messaging_invalid_json_is_decode_err() {
        let err = decode_messaging_platforms("not json").unwrap_err();
        assert!(matches!(err, GatewayError::Decode(_)));
    }

    #[test]
    fn messaging_platforms_client_routes_and_sends_bearer() {
        let (host, port, rx) = stub_once(
            "200 OK",
            r#"{"env_path":"p","gateway_start_command":"g","platforms":[{"id":"telegram","name":"Telegram","description":"d","docs_url":"","enabled":false,"configured":false,"gateway_running":true,"state":"disabled","error_code":null,"error_message":null,"updated_at":null,"home_channel":null,"env_vars":[]}]}"#,
        );
        let client = RestClient::new(&host, port, "Tok-Msg9");
        let p = client.messaging_platforms().expect("messaging_platforms");
        assert_eq!(p.platforms[0].id, "telegram");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("GET /api/messaging/platforms "),
            "path: {request}"
        );
        let header_line = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or_else(|| panic!("no Authorization header: {request}"));
        assert_eq!(
            header_line.trim_start_matches(|c: char| c != ' '),
            " Bearer Tok-Msg9",
            "auth header value corrupted: {header_line}"
        );
    }

    #[test]
    fn update_and_test_messaging_route_correctly() {
        let (host, port, rx1) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "");
        client
            .update_messaging("telegram", &serde_json::json!({"enabled": true}))
            .expect("update_messaging");
        let request1 = rx1.recv().expect("captured request");
        assert!(
            request1.starts_with("PUT /api/messaging/platforms/telegram "),
            "path: {request1}"
        );

        let (host2, port2, rx2) = stub_once("200 OK", "");
        let client2 = RestClient::new(&host2, port2, "");
        client2.test_messaging("telegram").expect("test_messaging");
        let request2 = rx2.recv().expect("captured request");
        assert!(
            request2.starts_with("POST /api/messaging/platforms/telegram/test "),
            "path: {request2}"
        );
    }

    #[test]
    fn update_messaging_sends_body_flat_and_interpolates_id() {
        // `update_and_test_messaging_route_correctly` pins the PUT verb + path
        // but never inspects the body. update_messaging is a *passthrough* (doc:
        // "like set_config") — it must send the caller's config FLAT and must
        // actually wire it to the request. Two silent-failure regressions pass
        // every existing test: dropping `.json(body)` (empty PUT, the config
        // update is lost) or copy-pasting update_job's `{"updates": …}` wrapper
        // (the gateway's passthrough handler gets the wrong shape). Use a
        // distinctive id so a hardcoded path can't false-pass interpolation.
        let (host, port, rx) = stub_once("200 OK", "");
        let client = RestClient::new(&host, port, "Tok-Upd7");
        client
            .update_messaging("sig_nal-7", &serde_json::json!({"home_channel": "ops"}))
            .expect("update_messaging");
        let request = rx.recv().expect("captured request");
        assert!(
            request.starts_with("PUT /api/messaging/platforms/sig_nal-7 "),
            "distinctive id not interpolated into path: {request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            body.contains("\"home_channel\":\"ops\""),
            "config not wired to the PUT body (dropped .json(body)?): {body}"
        );
        assert!(
            !body.contains("\"updates\""),
            "passthrough body must be flat, not wrapped like update_job: {body}"
        );
        let header_line = request
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or_else(|| panic!("no Authorization header: {request}"));
        assert_eq!(
            header_line.trim_start_matches(|c: char| c != ' '),
            " Bearer Tok-Upd7",
            "auth header value corrupted: {header_line}"
        );
    }
}

#[cfg(test)]
#[path = "rest_s7_tests.rs"]
mod rest_s7_tests;
