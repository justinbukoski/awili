//! awili-seam — the C1→B2→C2 seam, end to end (C2-u5).
//!
//! A deliberately THIN host loop composing the gated pieces:
//! B3 decode (`decode_stream`) → pure reducer (`reduce`) → C1
//! (`Authorizer::authorize`/`resolve`) → B2 (`intent`/tickets/replay) →
//! C2 dispatch through the closed executor vocabulary (unit 4 item 6:
//! builtin `run_builtin_fs`, or MCP through the coordinator facade) → C3
//! (`Sandbox`, transitively). It changes
//! NOTHING in any gated crate: gaps found by composition are findings in
//! docs/reviews/c2-u5-design-sketch-2026-08-13.md, not silent patches.
//!
//! THIS CRATE IS THE ADOPTED COMPOSITION (D2-u1, 2026-08-13). It began as
//! the C2-u5 harness under an adopt-or-delete condition; C4 made it the
//! owner of the context integration and `awili-engine-worker` now drives it
//! as the product turn loop. Composition ownership STAYS here — copying
//! `run_turn` into the worker would create exactly the duplicate composition
//! owner the pivot exists to prevent. Still deliberately absent: config
//! system, retries, scheduling, provider abstraction beyond the blocking
//! `Transport` seam, and UI — those belong to the worker and the app.
//!
//! Continuation requests are built HERE from a harness-owned transcript
//! ledger (design blocker 1) — explicitly temporary pending the D/B3
//! encoder; the wire shape is fixture-pinned by tests.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use awili_builtins::{BuiltinRunOutcome, run_builtin_fs};
use awili_engine::ToolIdentity;
use awili_engine::codec::chat_completions::{SseDecoder, decode_stream};
/// The session-owned context history `run_turn` appends to (C4). Re-exported
/// so consumers need not depend on awili-engine's module path.
pub use awili_engine::context::ContextHistory;
use awili_engine::context::{
    AssembleOutcome, AssembledContext, CompactionPlan, ContextGroup, GroupId, HeuristicCounter,
    ModelContextProfile, TokenCounter, apply_summary, assemble,
};
use awili_engine::events::{AgentEvent, ToolCall, ToolResultRecord};
use awili_engine::generation::ConnectionGeneration;
use awili_engine::reducer::{Input, NextAction, reduce};
use awili_engine::state::TurnState;
use awili_engine_authz::{Authorizer, AuthzContext, Decision, UserVerdict};
use awili_engine_log::authz::HostApprovalCapability;
use awili_engine_log::replay::{TurnRecovery, classify};
use awili_engine_log::storage::AppendOnlyLog;
use awili_engine_log::writer::{ExecutionLog, IntentOutcome};
use awili_engine_log::{LogError, RegistryAuthority, RiskClass};
use awili_engine_sandbox::Limits;
use awili_tools::builtins as fs_builtins;
use awili_tools::runtime::ExecutionCertainty;
use awili_tools::{AllowAllValidator, ToolRegistry};

pub mod any_folder;
/// The executor seam (unit 4 item 6): the closed executor vocabulary, the
/// facade surface the worker's coordinator implements, and the
/// host-controlled accepted-roots record the MCP leg reads its epoch from.
pub mod executor;
mod recovery;
pub use recovery::{
    RECOVERED_FAILED_LABEL, RECOVERED_WORK_LABEL, recover_completed_work, recovered_work_note,
};
mod wire_names;
mod schema_text;

/// RC1-R-FIX1: the characters of description text the extra (MCP) tools may
/// share in one request — half the window's token count in characters (about
/// a seventh of the window at ~3.5 characters per token).
pub(crate) fn description_budget_chars(window_tokens: u64) -> usize {
    usize::try_from(window_tokens / 2).unwrap_or(usize::MAX)
}

/// RC1-R-FIX1 (slice review P1): the descriptions the model gets for the
/// extra tools. When they fit [`description_budget_chars`] they go whole;
/// otherwise each is cut to an equal share (on a character, with "…"), and a
/// share too small to say anything (under 16 characters) sends none. Every
/// tool is still advertised — only its words are shortened.
pub(crate) fn fit_descriptions(descriptions: &[&str], window_tokens: u64) -> Vec<String> {
    let budget = description_budget_chars(window_tokens);
    let total: usize = descriptions.iter().map(|d| d.chars().count()).sum();
    if total <= budget {
        return descriptions.iter().map(|d| d.to_string()).collect();
    }
    let share = budget / descriptions.len().max(1);
    descriptions
        .iter()
        .map(|d| {
            let n = d.chars().count();
            if n <= share {
                d.to_string()
            } else if share < 16 {
                String::new()
            } else {
                d.chars().take(share - 1).collect::<String>() + "\u{2026}"
            }
        })
        .collect()
}
/// RC2-R52 (usability RC2 round 4, Majors 1 and 2): characters of tool
/// definitions per token the seam counts. Measured on the owner's setup
/// (2026-10-08, Gemma 4 26B on the Studio's mlx_lm, `usage.prompt_tokens`
/// with and without the `tools` array): 337 tools, 136,844 characters of
/// JSON, 31,330 tokens — 4.37 characters a token. The general text ratio
/// (3) counted those 31,330 tokens as 45,615, so a request that fit the
/// 32,768-token window by 5,000 tokens failed "cannot fit" before it was
/// sent.
pub const TOOL_CHARS_PER_TOKEN: f64 = 4.0;

/// RC2-R52: the most the tool definitions may take of a request whose
/// system text counts `system_tokens`: the input budget less that system
/// text, the first message's room ([`USER_ROOM_TOKENS`]) and its steps'
/// results ([`STEP_RESULT_ROOM_TOKENS`]) — at least a third of the budget,
/// so a very long brief never squeezes the tools to nothing.
pub fn tool_block_cap_tokens(window_tokens: u64, system_tokens: u64) -> u64 {
    let budget = input_budget_tokens(window_tokens);
    budget
        .saturating_sub(64 + 16 + system_tokens + USER_ROOM_TOKENS + STEP_RESULT_ROOM_TOKENS)
        .max(budget / 3)
}

/// The tokens a request's input may take in `window_tokens`: the window
/// less the reply's room (`max_tokens`) and the safety margin (as
/// [`seam_profile`]).
pub fn input_budget_tokens(window_tokens: u64) -> u64 {
    window_tokens
        .saturating_sub(REPLY_ROOM_TOKENS)
        .saturating_sub(SAFETY_MARGIN_TOKENS)
}

/// Equivalent window for projection helpers that reserve the legacy reply room.
/// Final request assembly uses the real window and configured output reservation.
fn projection_window_tokens(window_tokens: u64, output_tokens: u64) -> u64 {
    window_tokens.saturating_sub(output_tokens).saturating_add(REPLY_ROOM_TOKENS)
}

/// The reply's room in every request (`max_tokens`).
const REPLY_ROOM_TOKENS: u64 = 2048;
/// The seam profile's safety margin.
const SAFETY_MARGIN_TOKENS: u64 = 512;
/// RC2-R52: room kept for a turn's first message (the ask, a council's
/// transcript, recalled notes).
pub const USER_ROOM_TOKENS: u64 = 4096;
/// RC2-R52: room kept for the results of the steps a turn takes.
pub const STEP_RESULT_ROOM_TOKENS: u64 = 2048;

/// RC2-R52 (usability RC2 round 4, Major 1: Gemma's council card failed
/// "needed more room than the model can hold (32768 tokens)" in 4 of 6
/// turns): how many characters a first user message may have so that, with
/// `system`, the tool definitions (at most [`tool_block_cap_tokens`]), the
/// reply's room and room for its steps' results, the request fits
/// `window_tokens` as the seam counts it.
pub fn user_room_chars(window_tokens: u64, system: &str) -> usize {
    let system_tokens = seam_counter().count_text(system);
    let room = input_budget_tokens(window_tokens)
        .saturating_sub(tool_block_cap_tokens(window_tokens, system_tokens))
        .saturating_sub(64 + 16 + system_tokens + STEP_RESULT_ROOM_TOKENS);
    usize::try_from(room.saturating_mul(3)).unwrap_or(usize::MAX)
}

/// RC2-R52 (owner rule: Gemma keeps every one of his tools): the extra
/// (MCP) tools' definitions, made to fit [`tool_block_cap_tokens`] without
/// taking any tool or parameter away. In order, until they fit: schema
/// notes no model reads (`title`, `examples`, `$schema`, `$comment`) go;
/// then the checks its server makes itself (bounds, lengths, formats,
/// defaults: [`SCHEMA_HINTS`]);
/// the parameters' own descriptions are shortened (to the longest common
/// length that fits, cut with "…"), then dropped; then the tools' own
/// descriptions are shortened the same way. Every tool, parameter, type,
/// enum and required list stays; the engine's own file tools (the first
/// `builtins`) are never touched. The same tools give the same definitions,
/// so the model server's prompt cache holds.
pub fn fit_tool_block(
    tools: Vec<serde_json::Value>,
    builtins: usize,
    window_tokens: u64,
    system_tokens: u64,
) -> Vec<serde_json::Value> {
    let cap = usize::try_from(tool_block_cap_tokens(window_tokens, system_tokens))
        .unwrap_or(usize::MAX)
        .saturating_mul(TOOL_CHARS_PER_TOKEN as usize);
    let size = |t: &[serde_json::Value]| serde_json::Value::Array(t.to_vec()).to_string().len();
    if size(&tools) <= cap {
        return tools;
    }
    let mut tools = tools;
    for t in tools.iter_mut().skip(builtins) {
        if let Some(p) = t.pointer_mut("/function/parameters") {
            strip_schema_notes(p);
        }
    }
    if size(&tools) <= cap {
        return tools;
    }
    for t in tools.iter_mut().skip(builtins) {
        if let Some(p) = t.pointer_mut("/function/parameters") {
            strip_schema_hints(p);
        }
    }
    if size(&tools) <= cap {
        return tools;
    }
    let longest = |f: &dyn Fn(&serde_json::Value, &mut usize)| {
        let mut n = 0usize;
        for t in tools.iter().skip(builtins) {
            f(t, &mut n);
        }
        n
    };
    let nested = |t: &serde_json::Value, n: &mut usize| {
        if let Some(p) = t.pointer("/function/parameters") {
            longest_param_description(p, n);
        }
    };
    let param_len = longest(&nested);
    let with_params = |k: usize| {
        let mut out = tools.clone();
        for t in out.iter_mut().skip(builtins) {
            if let Some(p) = t.pointer_mut("/function/parameters") {
                cut_param_descriptions(p, k);
            }
        }
        out
    };
    if let Some(k) = largest_fitting(param_len, |k| size(&with_params(k)) <= cap) {
        return with_params(k);
    }
    let tools = with_params(0);
    let top = |t: &serde_json::Value, n: &mut usize| {
        if let Some(d) = t.pointer("/function/description").and_then(|d| d.as_str()) {
            *n = (*n).max(d.chars().count());
        }
    };
    let desc_len = {
        let mut n = 0usize;
        for t in tools.iter().skip(builtins) {
            top(t, &mut n);
        }
        n
    };
    let with_desc = |k: usize| {
        let mut out = tools.clone();
        for t in out.iter_mut().skip(builtins) {
            if let Some(f) = t.get_mut("function").and_then(|f| f.as_object_mut()) {
                let cut = f
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| cut_text(s, k));
                match cut {
                    // Nothing left to say: no empty description either.
                    Some(c) if c.is_empty() => {
                        f.remove("description");
                    }
                    Some(c) => {
                        f.insert("description".into(), serde_json::Value::String(c));
                    }
                    None => {}
                }
            }
        }
        out
    };
    let k = largest_fitting(desc_len, |k| size(&with_desc(k)) <= cap).unwrap_or(0);
    with_desc(k)
}

/// The largest `k` in `0..=max` for which `fits(k)` holds (`fits` holds for
/// every `k` below one that holds), or `None` when not even 0 fits.
fn largest_fitting(max: usize, fits: impl Fn(usize) -> bool) -> Option<usize> {
    if !fits(0) {
        return None;
    }
    let (mut lo, mut hi) = (0usize, max);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Some(lo)
}

/// `s` in at most `k` characters: whole, or cut with "…" (nothing when `k`
/// is too small to say anything).
fn cut_text(s: &str, k: usize) -> String {
    if s.chars().count() <= k {
        s.to_string()
    } else if k < 8 {
        String::new()
    } else {
        s.chars()
            .take(k - 1)
            .collect::<String>()
            .trim_end()
            .to_string()
            + "\u{2026}"
    }
}

/// The notes in a JSON schema no model reads.
const SCHEMA_NOTES: [&str; 4] = ["title", "examples", "$schema", "$comment"];

/// Remove [`SCHEMA_NOTES`] from schema `v` and every schema in it (the names
/// in a `properties` map are parameter names, never notes).
fn strip_schema_notes(v: &mut serde_json::Value) {
    walk_schemas(v, &mut |schema| {
        for k in SCHEMA_NOTES {
            schema.remove(k);
        }
    });
}

/// Checks the tool's own server makes on what it is sent (bounds, lengths,
/// formats, extra-field rules, defaults): not needed to call it right, and
/// Gemma's chat template never shows them to the model.
const SCHEMA_HINTS: [&str; 14] = [
    "default",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "uniqueItems",
    "pattern",
    "format",
    "additionalProperties",
    "propertyNames",
];

/// Remove [`SCHEMA_HINTS`] from schema `v` and every schema in it (never a
/// parameter's name).
fn strip_schema_hints(v: &mut serde_json::Value) {
    walk_schemas(v, &mut |schema| {
        for k in SCHEMA_HINTS {
            // `additionalProperties` may be a schema (a map's values): its
            // own type stays.
            if k == "additionalProperties" && schema.get(k).is_some_and(|s| s.is_object()) {
                continue;
            }
            schema.remove(k);
        }
    });
}

/// The longest parameter description in schema `v` (into `n`).
fn longest_param_description(v: &serde_json::Value, n: &mut usize) {
    let mut v = v.clone();
    walk_param_schemas(&mut v, &mut |schema| {
        if let Some(d) = schema.get("description").and_then(|d| d.as_str()) {
            *n = (*n).max(d.chars().count());
        }
    });
}

/// Cut every parameter description in schema `v` to `k` characters (`k`
/// under 8: none).
fn cut_param_descriptions(v: &mut serde_json::Value, k: usize) {
    walk_param_schemas(v, &mut |schema| {
        let cut = schema
            .get("description")
            .and_then(|d| d.as_str())
            .map(|d| cut_text(d, k));
        match cut {
            Some(c) if c.is_empty() => {
                schema.remove("description");
            }
            Some(c) => {
                schema.insert("description".into(), serde_json::Value::String(c));
            }
            None => {}
        }
    });
}

/// Call `f` on schema `v` and every schema inside it.
fn walk_schemas(
    v: &mut serde_json::Value,
    f: &mut dyn FnMut(&mut serde_json::Map<String, serde_json::Value>),
) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    f(obj);
    for key in ["properties", "patternProperties", "$defs", "definitions"] {
        if let Some(props) = obj.get_mut(key).and_then(|p| p.as_object_mut()) {
            for (_, s) in props.iter_mut() {
                walk_schemas(s, f);
            }
        }
    }
    for key in ["items", "additionalProperties", "not"] {
        if let Some(s) = obj.get_mut(key) {
            walk_schemas(s, f);
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(list) = obj.get_mut(key).and_then(|l| l.as_array_mut()) {
            for s in list {
                walk_schemas(s, f);
            }
        }
    }
}

/// Call `f` on every schema inside schema `v`, but not on `v` itself (the
/// parameters object: its own description, if any, is the tool's).
fn walk_param_schemas(
    v: &mut serde_json::Value,
    f: &mut dyn FnMut(&mut serde_json::Map<String, serde_json::Value>),
) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    let mut inner = |s: &mut serde_json::Value| walk_schemas(s, f);
    if let Some(props) = obj.get_mut("properties").and_then(|p| p.as_object_mut()) {
        for (_, s) in props.iter_mut() {
            inner(s);
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(s) = obj.get_mut(key) {
            inner(s);
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(list) = obj.get_mut(key).and_then(|l| l.as_array_mut()) {
            for s in list {
                inner(s);
            }
        }
    }
}

/// RC2-R51: the bare names more than one tool of a request has.
fn shared_names<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut shared = BTreeSet::new();
    for n in names {
        if !seen.insert(n) {
            shared.insert(n.to_string());
        }
    }
    shared
}

/// RC2-R51 (usability RC2 round 4, Major 3: Gemma's `run_command` was
/// "batch refused: unresolvable tool(s)" because three of the owner's
/// servers each offer one): the name the model gets for a tool — its own,
/// or, when another tool of the request has the same name, the qualified
/// one ([`awili_tools::qualified_name`]), which the registry resolves to
/// exactly this origin's tool.
///
/// RC2-R5M1 (slice review befb867d, P1): the name is the registry's alias
/// ([`ToolRegistry::aliases`]), allocated over the complete advertised set
/// (builtins and extras), so a qualified alias never equals another tool's
/// own name and every advertised name resolves to exactly this tool.
fn advertised_name(
    d: &awili_tools::ToolDef,
    aliases: &std::collections::BTreeMap<(String, String), String>,
) -> String {
    aliases
        .get(&(d.identity.origin.origin.clone(), d.name().to_string()))
        .cloned()
        .unwrap_or_else(|| d.name().to_string())
}

/// RC2-R51 (usability RC2 round 4, Major 3: Gemma told the owner "⚠
/// run_command FAILED … the run_command tool is currently unavailable in
/// this environment"): what the model is told when Awili's rules refused a
/// step — plain and correct, so it says the same to the owner. The first
/// sentence ("Not done: …") is what the owner's reply shows.
pub(crate) fn refusal_text(reason: &str) -> String {
    format!(
        "Not done: {}. Nothing was changed. Tell the user in plain words what was not done and \
         why, without naming tools, and offer a way that works.",
        reason.trim().trim_end_matches('.')
    )
}

/// RC2-R51: what the model is told when it named no tool it has (an
/// unresolvable tool name): nothing ran, and how to call one.
pub(crate) fn unresolved_text(names: &[String]) -> String {
    let named = names
        .iter()
        .map(|n| format!("\u{201C}{n}\u{201D}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Not done: no tool here is named {named} (an unresolvable tool name), so nothing in this \
         step ran. Call a tool by the exact name it is listed under in this request; if none \
         does what is needed, tell the user in plain words."
    )
}

pub use executor::{
    AcceptedRoots, BringUpError, DEFAULT_MCP_SUBMIT_DEADLINE, Executor, Executors,
    HostRootProposal, MCP_POLL_SLICE, McpCall, McpDispatch, McpDispatchFailure, McpDispatchOutcome,
    McpExecutor, McpSubmitRefused, RootsAcceptance, RootsPolicy, RootsRejection, ValidatedRoots,
    bring_up_roots, validate_roots,
};

/// Re-exported for callers (the bin, D2): the session-owned context history
/// that `run_turn` appends to.
pub use awili_engine::context::ContextHistory as SessionHistory;

/// Hard bound on model rounds (initial + continuations) per turn. RC1-M-FIX1
/// (owner design principle, 2026-10-03): 4 stopped the in-app engine after
/// three tool rounds — live, Gemma's "I need to take an outage on hostd" ended
/// "round bound (4) exceeded" while it was still looking things up, where
/// Claude Code goes on. 24 leaves room for a memory lookup, a few reads and
/// the answer; it remains a runaway guard, not a working limit.
pub const MAX_ROUNDS: usize = 24;

/// Typed seam failures. Never a panic; never a synthesized success.
#[derive(Debug, thiserror::Error)]
pub enum SeamError {
    /// Transport-level failure reaching the provider.
    #[error("transport: {0}")]
    Transport(String),
    /// The provider violated the wire protocol at the HOST boundary
    /// (duplicate/missing provider call ids, empty batch): the turn is
    /// terminated and audited with ZERO members of the batch dispatched
    /// (design blocker 4).
    #[error("provider protocol: {0}")]
    Protocol(String),
    /// B2 itself failed. Storage honesty (design blocker 5): the run
    /// aborts — no pretended finalization; replay tells the truth about
    /// whatever was durably written.
    #[error("storage: {0}")]
    Storage(#[from] LogError),
    /// The harness itself misbehaved (e.g. the reducer audited an
    /// InputRejected). A harness bug, surfaced loudly.
    #[error("harness: {0}")]
    Harness(String),
    /// The host stopped the turn before it finished: a cancel flag trip, a
    /// Cancel/Shutdown answer to a blocking approval, or the event surface
    /// disappearing (D2-u1 P0-1/P0-4). NO authorization is resolved, NO tool
    /// runs, NO result is ingested, and NO further transport call is made
    /// after the stop is observed; the caller's C4 checkpoint restores the
    /// history.
    #[error("turn stopped by host: {0}")]
    Cancelled(StopCause),
    /// C4 context assembly/compaction failed (typed — never a silent
    /// truncation): CannotFit, a second NeedsCompaction after one cycle,
    /// a summarizer failure, or an unencodable call (no provider id).
    #[error("context: {0}")]
    Context(String),
    /// The turn ended in TurnFailed (provider error mid-stream, etc.).
    /// Carries the closed-and-audited report.
    #[error("turn failed: {reason}")]
    TurnFailed {
        reason: String,
        report: Box<TurnReport>,
    },
    /// The immediate pre-dispatch identity check failed (unit 4, design
    /// r3 Delta 2 / r4 Delta A): the registry's current resolution did not
    /// match the evidence-bound identity, or the bound origin is no longer
    /// available. A HARD error — never a fallback dispatch.
    #[error("pre-dispatch identity check: {0}")]
    PreDispatchIdentity(#[from] IdentityCheckError),
    /// An MCP dispatch ended without a terminal (unit 4 item 6): the
    /// submit was finally refused, the submission rejected as a
    /// duplicate, a cancel submission failed, or the runtime event stream
    /// closed. The call's intent was durably logged and dispatch-started;
    /// it is deliberately LEFT UNFINALIZED for B2 replay to classify —
    /// the seam never synthesizes a terminal.
    #[error("mcp dispatch: {0}")]
    McpDispatch(McpDispatchFailure),
}

/// Hard failure of the immediate pre-dispatch identity check (unit 4):
/// the registry's CURRENT resolution of the call's bare name must be
/// AVAILABLE and IDENTICAL to the identity the authorization evidence
/// bound. Either failing is a hard error — the dispatch is never
/// re-resolved by bare name, and a vanished `mcp:*` origin never falls
/// back to `builtin:fs` (or any other origin that now owns the name).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityCheckError {
    #[error("tool {name:?} no longer resolves: {detail}")]
    OriginUnavailable { name: String, detail: String },
    #[error(
        "tool {name:?} now resolves to origin {current:?}, but the authorization bound {bound:?}"
    )]
    Mismatch {
        name: String,
        bound: String,
        current: String,
    },
}

/// The immediate pre-dispatch check (unit 4): compare the registry's
/// CURRENT resolution against the EVIDENCE-BOUND identity — equality AND
/// availability. Hard error on either; never a fallback.
pub fn verify_bound_identity(
    registry: &ToolRegistry,
    name: &str,
    bound: &ToolIdentity,
) -> Result<(), IdentityCheckError> {
    let def = registry
        .lookup_unique(name)
        .map_err(|e| IdentityCheckError::OriginUnavailable {
            name: name.to_string(),
            detail: e.detail,
        })?;
    if &def.identity != bound {
        return Err(IdentityCheckError::Mismatch {
            name: name.to_string(),
            bound: bound.origin.origin.clone(),
            current: def.identity.origin.origin.clone(),
        });
    }
    Ok(())
}

/// One blocking round-trip: request body JSON in, raw response body out
/// (SSE text or bare JSON error — `decode_stream` handles both). Tests
/// inject scripted bodies; the binary speaks HTTP.
pub trait Transport {
    fn set_cancel_flag(&mut self, _flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {}
    fn max_output_tokens(&self) -> u64 { REPLY_ROOM_TOKENS }

    fn stream(&mut self, request_body: &str) -> Result<String, SeamError>;

    /// RC2-GEMMA3 (R7-speed): [`stream`](Self::stream), with each line of
    /// the body handed to `on_line` (without its line ending) as it
    /// arrives; `on_line` returns `false` to stop reading (the turn was
    /// stopped). Returns the body read. The default reads the whole body
    /// first and hands over nothing — the seam then decodes it whole, as
    /// before (scripted test transports keep that path).
    fn stream_lines(
        &mut self,
        request_body: &str,
        on_line: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, SeamError> {
        let _ = on_line;
        self.stream(request_body)
    }
}

/// Why a turn stopped early. Distinct causes because the caller renders
/// them differently: a user Cancel reports a stopped turn, a Shutdown or a
/// vanished surface exits silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// The user cancelled (flag trip or a Cancel answer to a dialog).
    Cancelled,
    /// The host is shutting the worker down.
    Shutdown,
    /// The event surface went away (receiver dropped).
    Disconnected,
    /// RC1-N: the owner pressed Deny on an approval card. The denial is
    /// recorded durably first; then the turn ends here instead of handing
    /// the refusal back to the model, so the owner's request goes back to
    /// the owner (the session itself goes on).
    Denied,
}

impl std::fmt::Display for StopCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "cancelled",
            Self::Shutdown => "shutdown",
            Self::Disconnected => "surface disconnected",
            Self::Denied => "denied by the owner",
        })
    }
}

/// What a blocking approval dialog returned. A non-`Verdict` outcome stops
/// the turn BEFORE authorization is resolved (D2-u1 P0-1): no grant is
/// recorded, no denial is recorded, no tool runs. (`UserVerdict` is neither
/// `Debug` nor `Clone` — it carries the user's own words — so neither is
/// this enum.)
pub enum ApprovalOutcome {
    Verdict(UserVerdict),
    Cancelled,
    Shutdown,
    /// The command channel closed while the dialog was open.
    Disconnected,
}

/// The live event surface went away; the turn must stop before any further
/// side effect (D2-u1 P0-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObserverGone;

/// The host side of one turn: the approval dialog, the live event sink, and
/// the cancellation flag. Bundled so `run_turn` keeps a legible signature.
///
/// `observe` is called for EVERY canonical event, in B2 order and only
/// AFTER the durable append succeeded (a storage failure emits nothing).
/// `cancelled` is polled before every transport request and before every
/// side effect.
pub trait TurnHost {
    /// Blocking approval decision for ONE call. `call_id` is that call's
    /// `awili_call_id` — the stable correlation key when a batch raises
    /// several dialogs (round-1 P1).
    fn approve(&mut self, call_id: &str, prompt: &str, risk: RiskClass) -> ApprovalOutcome;
    /// Live event delivery. `Err` = the surface is gone; the turn stops.
    fn observe(&mut self, _event: &AgentEvent) -> Result<(), ObserverGone> {
        Ok(())
    }
    /// Polled before transport calls and side effects.
    fn cancelled(&self) -> bool {
        false
    }
    /// Auto mode (U4): may this non-floor Ask be approved without a
    /// dialog? Default: never. (The blanket closure impl is unchanged, so
    /// closures never auto-approve.)
    fn auto_approve(&mut self, _call_id: &str, _prompt: &str, _risk: RiskClass) -> bool {
        false
    }
    /// RC1-N: after the owner's Deny was recorded, does the turn end there
    /// (`StopCause::Denied`) rather than letting the model carry on with the
    /// refusal? Default: no — the model sees the denial, as before.
    fn stop_after_denial(&self) -> bool {
        false
    }
    /// RC1-O: a call is about to run — it resolved to a tool, was
    /// authorized, and passed the last pre-dispatch checks (for an MCP call:
    /// its server's accepted roots and its durable intent; RC1-O-FIX7) —
    /// with its registry risk floor. Told once per such call; never for a call that
    /// was denied or refused (RC1-O-FIX5: whole-slice review P2 — a call that
    /// ran may have changed files even when it then returned an error).
    /// Default: nothing.
    fn tool_dispatching(&mut self, _call_id: &str, _floor: RiskClass) {}
    /// Called around the C4 compaction cycle so compaction is never silent
    /// (D2-u1 P1). Default: nothing (the offline suite ignores it).
    fn compaction_started(&mut self) -> Result<(), ObserverGone> {
        Ok(())
    }
    /// RC2-GEMMA3: the model server is reading the request (its SSE
    /// `: keepalive done/total` comments while it prefills): `done` of
    /// `total` prompt tokens so far. Default: nothing.
    fn prompt_progress(&mut self, _done: u64, _total: u64) {}
    /// RC2-GEMMA4 (R7-A3): the model server cut a text-only reply off (no
    /// end of stream) and the round is asked again, once. `shown` is the
    /// text this round had already shown; the retry's reply replaces it.
    /// Default: nothing.
    fn reply_cut(&mut self, _shown: &str) {}
    /// RC2-R2F7: the model works for the owner anywhere (a council member):
    /// the engine's own file tools reach a full path outside the session's
    /// folder through that path's own folder ([`any_folder::call_root`]);
    /// reads pass, changes still ask. Default: no \u{2014} the session's
    /// folder bounds every path, as before.
    fn any_folder(&self) -> bool {
        false
    }
    /// RC2-R3 (usability RC2 round 3, Major 1: Work's in-app Gemma asked to
    /// run the owner's `read_file` with "alpha.txt", which raised a card,
    /// failed, and led to `find / -name alpha.txt`): the session's folder is
    /// the Work folder the owner chose, so a relative path means a file in
    /// it. A call to a local file or shell tool of an MCP server
    /// ([`LOCAL_FILE_TOOLS`]) that names a relative path is then answered at
    /// once, with no card and without running: the engine's own file tools
    /// take that path as it is, that tool needs the full path
    /// ([`relative_path_redirect`]). Default: no.
    fn relative_paths_mean_the_folder(&self) -> bool {
        false
    }
}

/// RC2-R3 (usability RC2 round 3, Minor 6: after the owner pressed Deny on
/// a write, Gemma said "I don't have the permissions required to write files
/// to that directory"): what the model is told when the owner declined a
/// step \u{2014} plainly his choice, not a fault, and not to be tried another way.
pub const OWNER_DECLINED: &str = "Not done: the owner denied this step (he pressed Deny), so it did \
     not run. That was his choice; nothing is wrong with your permissions, the folder or the tool. \
     Tell him plainly that it was not done because he declined it, and do not try it another way \
     unless he asks.";

/// RC2-R3: the path-like argument names of another tool's call that are
/// checked for a relative path (top level only; `cwd` and the like too).
fn is_path_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    matches!(
        k.as_str(),
        "path"
            | "file"
            | "filepath"
            | "filename"
            | "file_name"
            | "dir"
            | "directory"
            | "folder"
            | "cwd"
            | "workdir"
            | "working_directory"
            // RC2-R3M5: the filesystem server's `read_multiple_files`
            // (`paths`) and `move_file` (`source`, `destination`).
            | "paths"
            | "source"
            | "destination"
    ) || k.ends_with("_path")
        || k.ends_with("_file")
        || k.ends_with("_dir")
        || k.ends_with("_paths")
}

/// RC2-R3: the other tools whose path is a path on this Mac, by the names a
/// local file or shell MCP server gives them (the owner's shell server, the
/// standard filesystem server). A remote tool's path \u{2014} a file in a Gitea
/// repository, an Outline document \u{2014} is never taken for one.
pub const LOCAL_FILE_TOOLS: [&str; 15] = [
    "read_file",
    "read_text_file",
    "read_media_file",
    "read_multiple_files",
    "write_file",
    "edit_file",
    "create_directory",
    "list_directory",
    "list_directory_with_sizes",
    "directory_tree",
    "move_file",
    "search_files",
    "get_file_info",
    "run_command",
    "run_job",
];

/// RC2-R3M1 (RC2-R3 review P2): the MCP servers whose file and shell tools
/// work on this Mac: the owner's shell server `mac-shell` (Awili's
/// install-trusted server, `awili_config::INSTALL_TRUSTED_SERVER`) and the
/// standard filesystem server under its usual name. Any other server's tool
/// may work somewhere else \u{2014} another machine, a Gitea repository \u{2014}
/// even when it is called `read_file`, so its relative path is its own
/// business and is never redirected to the Work folder.
pub const LOCAL_SHELL_ORIGIN: &str = "mcp:mac-shell";
pub const LOCAL_FILE_ORIGINS: [&str; 2] = [LOCAL_SHELL_ORIGIN, "mcp:filesystem"];

/// RC2-R3M1: whether a tool's origin is one of [`LOCAL_FILE_ORIGINS`].
pub(crate) fn is_local_file_origin(origin: &str) -> bool {
    LOCAL_FILE_ORIGINS.contains(&origin)
}

/// RC2-R3: the relative path a local file or shell tool (`name`, one of
/// [`LOCAL_FILE_TOOLS`]) was given, or `None`.
pub(crate) fn local_relative_path(name: &str, arguments: &str) -> Option<String> {
    LOCAL_FILE_TOOLS
        .contains(&name)
        .then(|| relative_path_arg(arguments))
        .flatten()
}

/// RC2-R3: the first relative path among `arguments`' path-like values
/// ("alpha.txt", "notes/a.md", "."), or `None`: a full path, `~/…`, a URL,
/// an `$VAR` or an empty value is not one. A list of paths (`paths`) counts
/// by each of its entries (RC2-R3M5).
pub(crate) fn relative_path_arg(arguments: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let relative = |s: &str| {
        // RC2-R3M6: the path as given, spaces and all (a file name may
        // start or end with one); only a value of spaces alone is none.
        !s.trim().is_empty()
            && !s.starts_with('/')
            && !s.starts_with('~')
            && !s.starts_with('$')
            && !s.contains("://")
    };
    v.as_object()?
        .iter()
        .filter(|(k, _)| is_path_key(k))
        .find_map(|(_, val)| match val {
            serde_json::Value::String(s) => relative(s).then(|| s.clone()),
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(|i| i.as_str())
                .find(|s| relative(s))
                .map(str::to_string),
            _ => None,
        })
}

/// RC2-R3: what the model is told, in plain words, when it gave a local file
/// or shell tool (`name`) a relative path in the Work folder `folder`: nothing ran,
/// the engine's own file tools take that path as it is, and this tool needs
/// the file's full path, which is spelled out \u{2014} so the model never has to
/// search the disk for it.
pub(crate) fn relative_path_redirect(name: &str, rel: &str, folder: &str) -> String {
    let full = format!(
        "{}/{}",
        folder.trim_end_matches('/'),
        rel.trim_start_matches("./")
    );
    format!(
        "Not run: \"{rel}\" is not a full path, and {name} does not start in the Work folder \
         {folder}. In the Work folder that is {full}. For a file in the Work folder use your own \
         file tools with the path as it is: fs_read (path \"{rel}\") to read it, fs_write or \
         fs_edit to change it, fs_glob (pattern \"*\") to list the folder. To use {name}, give \
         it the full path {full}. Do not search the whole disk for it."
    )
}

/// Plain closures remain valid hosts: verdict-only, no observation, never
/// cancelled — which is exactly what the offline seam suite wants.
impl<F: FnMut(&str, RiskClass) -> UserVerdict> TurnHost for F {
    fn approve(&mut self, _call_id: &str, prompt: &str, risk: RiskClass) -> ApprovalOutcome {
        ApprovalOutcome::Verdict(self(prompt, risk))
    }
}

/// Issued authority, passed IN (design P2: `run_turn` never calls
/// `issue()` — one process bootstrap owns the singletons).
pub struct HostAuthority {
    pub authorizer: Authorizer,
    pub approval_cap: HostApprovalCapability,
    /// The registry half of the process's authority bundle (unit 4): the
    /// seam mints the `ResolvedTool` for each registry resolution — the
    /// ONLY site that can — and threads it through C1 into the durable
    /// record.
    pub registry: RegistryAuthority,
}

/// What one turn produced. `replay` is the classification of the FULL
/// session log after `turn_closed` — the acceptance surface.
#[derive(Debug)]
pub struct TurnReport {
    pub events: Vec<AgentEvent>,
    pub final_text: String,
    pub rounds: usize,
    pub replay: Vec<TurnRecovery>,
    /// Raw request bodies sent, in order (fixture-shape assertions).
    pub requests: Vec<String>,
}

// -- C4 context assembly (replaces the C2-u5 interim transcript ledger) ------
//
// The harness-owned `Ledger` (design blocker 1, explicitly temporary) is
// GONE: the seam now holds a real `ContextHistory` and assembles every
// request through `awili_engine::context` — the same contract D2 integrates.
// The Chat Completions wire shape is unchanged (fixture-pinned by
// `continuation_request_is_byte_identical_to_legacy_shape`); what changed is
// WHO owns transcript truth (the engine) and what happens when the window
// fills (a deterministic CompactionPlan, summarized by the SAME endpoint,
// applied immutably — never raw truncation).

/// Default context window for the seam profile: laguna-s-2.1-nvfp4 serves
/// 131072 (`/v1/models`, verified live 2026-08-13).
pub const DEFAULT_WINDOW_TOKENS: u64 = 131_072;

/// The seam's explicit context profile (C4 P1-3/P1-4): Chat Completions
/// cannot replay continuation blobs, so `replays_continuations = false`;
/// overheads are conservative explicit constants, with the advertised tool
/// schemas counted into the fixed request overhead.
fn seam_profile(model: &str, window_tokens: u64, tools_json: &str) -> ModelContextProfile {
    ModelContextProfile {
        provider: "chat-completions".into(),
        model: model.into(),
        window_tokens,
        // The initial request pins max_tokens: 2048 — reserve it.
        reserved_output_tokens: REPLY_ROOM_TOKENS,
        safety_margin_tokens: SAFETY_MARGIN_TOKENS,
        assumed_summary_tokens: 1024,
        replays_continuations: false,
        // RC2-R52: tool definitions counted at their measured ratio.
        fixed_request_overhead_tokens: 64 + tool_counter().count_text(tools_json),
        per_message_overhead_tokens: 8,
    }
}

/// Conservative explicit ratios (~1 token per 3 chars over-counts English and
/// JSON, which is the safe direction for a fit check).
/// RC2-R52: the count of tool definitions ([`TOOL_CHARS_PER_TOKEN`]).
fn tool_counter() -> HeuristicCounter {
    HeuristicCounter {
        chars_per_token: TOOL_CHARS_PER_TOKEN,
        bytes_per_token: TOOL_CHARS_PER_TOKEN,
    }
}

fn seam_counter() -> HeuristicCounter {
    HeuristicCounter {
        chars_per_token: 3.0,
        bytes_per_token: 3.0,
    }
}

/// Encode an assembled context into the Chat Completions `messages` array —
/// the exact wire shape the C2-u5 fixtures pin. The tool-result join is by
/// `awili_call_id` back to the call's `provider_call_id` (C4 P0-3): a
/// missing/empty provider id is a typed failure, never a silent `""`.
fn encode_chat_completions(a: &AssembledContext) -> Result<serde_json::Value, SeamError> {
    let mut msgs: Vec<serde_json::Value> = Vec::new();
    let current_user = a
        .groups
        .iter()
        .rposition(|g| matches!(g, ContextGroup::User { .. }));
    for (index, g) in a.groups.iter().enumerate() {
        match g {
            ContextGroup::Pinned { text, .. } => {
                msgs.push(serde_json::json!({"role": "system", "content": text}));
            }
            ContextGroup::User { text, .. } => {
                msgs.push(serde_json::json!({"role": "user", "content": if Some(index) == current_user { awili_image_content::openai(text) } else { serde_json::json!(awili_image_content::split(text).text) }}));
            }
            ContextGroup::Summary { text, .. } => {
                msgs.push(serde_json::json!({
                    "role": "user",
                    "content": format!("[context summary] {text}")
                }));
            }
            ContextGroup::Exchange {
                assistant_text,
                calls,
                results,
                ..
            } => {
                if calls.is_empty() {
                    msgs.push(serde_json::json!({
                        "role": "assistant",
                        "content": assistant_text
                    }));
                } else {
                    let tool_calls = calls
                        .iter()
                        .map(|c| {
                            let pid = c
                                .provider_call_id
                                .as_deref()
                                .filter(|p| !p.is_empty())
                                .ok_or_else(|| {
                                    SeamError::Context(format!(
                                        "call {:?} has no provider_call_id; cannot encode \
                                         tool_calls",
                                        c.awili_call_id
                                    ))
                                })?;
                            Ok(serde_json::json!({
                                "id": pid,
                                "type": "function",
                                "function": {"name": c.name, "arguments": c.arguments}
                            }))
                        })
                        .collect::<Result<Vec<_>, SeamError>>()?;
                    msgs.push(serde_json::json!({
                        "role": "assistant",
                        // Fixture shape: empty string, not null.
                        "content": assistant_text,
                        "tool_calls": tool_calls
                    }));
                    for r in results {
                        let call = calls
                            .iter()
                            .find(|c| c.awili_call_id == r.awili_call_id)
                            .expect("push_exchange enforced the bijection");
                        let pid = call
                            .provider_call_id
                            .as_deref()
                            .filter(|p| !p.is_empty())
                            .ok_or_else(|| {
                                SeamError::Context(format!(
                                    "result {:?}: its call has no provider_call_id",
                                    r.awili_call_id
                                ))
                            })?;
                        msgs.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": pid,
                            "content": r.content
                        }));
                    }
                }
            }
            // Excluded upstream (`replays_continuations = false`); defensive.
            ContextGroup::Continuation { .. } => {}
        }
    }
    Ok(serde_json::Value::Array(msgs))
}

/// Deterministic plain-text rendering of a compaction plan's canonical
/// material — the summarizer model's input. Contains only canonical
/// User/Exchange/Summary content: never pinned text, never blob payloads
/// (C4 P2 — a summary cannot import hidden reasoning).
fn render_plan_material(plan: &CompactionPlan) -> String {
    let mut out = String::new();
    for g in &plan.to_summarize {
        match g {
            ContextGroup::User { text, .. } => {
                out.push_str("User: ");
                out.push_str(&awili_image_content::split(text).text);
                out.push('\n');
            }
            ContextGroup::Summary { text, .. } => {
                out.push_str("Earlier summary: ");
                out.push_str(text);
                out.push('\n');
            }
            ContextGroup::Exchange {
                assistant_text,
                calls,
                results,
                ..
            } => {
                out.push_str("Assistant: ");
                out.push_str(assistant_text);
                out.push('\n');
                for (c, r) in calls.iter().zip(results) {
                    out.push_str(&format!(
                        "[tool {}({}) -> {}]\n",
                        c.name, c.arguments, r.content
                    ));
                }
            }
            ContextGroup::Pinned { .. } | ContextGroup::Continuation { .. } => {
                unreachable!("CompactionPlan never selects pinned/continuation groups")
            }
        }
    }
    out
}

/// Produce the compaction summary via the SAME transport ("compaction
/// summaries via any model" — here, the model already on the wire). The
/// request is recorded in `requests` (evidence); the response's TextDeltas
/// are the summary. Not part of the turn's reducer/authz flow.
fn summarize_plan(
    transport: &mut dyn Transport,
    model: &str,
    plan: &CompactionPlan,
    requests: &mut Vec<String>,
) -> Result<String, SeamError> {
    // RC2-GEMMA6 (S-N4): a summarizer request the model server refuses as
    // longer than its context is sent again with the older half of its
    // material left out (marked), until it fits or little is left.
    let mut material = render_plan_material(plan);
    let raw = loop {
        let body = serde_json::json!({
            "model": model,
            "stream": true,
            "stream_options": {"include_usage": true},
            "max_tokens": 1024,
            "messages": [{
                "role": "user",
                "content": format!(
                    "Summarize the following working context so an assistant can \
                     continue the task. Preserve facts, decisions, file paths, and \
                     tool outcomes. Reply with the summary only.\n\n{material}"
                )
            }]
        })
        .to_string();
        requests.push(body.clone());
        let answer = transport.stream(&body);
        let refused = match &answer {
            Err(SeamError::Transport(m)) => context_overflow_text(m),
            Ok(raw) => provider_context_refusal(raw),
            Err(_) => false,
        };
        if refused {
            if let Some(shorter) = newer_half(&material) {
                eprintln!("awili-seam: the model refused the summary request as too long; halved");
                material = shorter;
                continue;
            }
        }
        break answer?;
    };
    // C4 round-1 P1: a summary becomes CANONICAL context — accept only one
    // successful, text-only, cleanly-terminated completion. Truncation, tool
    // calls, a non-Stop finish, or a missing [DONE] are typed failures; the
    // caller then never applies, and the history stays untouched.
    use awili_engine::wire::{FinishReason, WireDelta};
    let mut summary = String::new();
    let mut finish: Option<FinishReason> = None;
    let mut ended = false;
    for delta in decode_stream(&raw) {
        // Round-2 P1: after Finish, ONLY trailing Usage and the StreamEnd
        // sentinel are legal — post-finish text/reasoning/tool deltas are a
        // malformed stream and must not poison the canonical summary.
        if finish.is_some()
            && !matches!(
                delta,
                WireDelta::Usage { .. } | WireDelta::StreamEnd | WireDelta::ProviderError { .. }
            )
        {
            return Err(SeamError::Context(format!(
                "summarizer emitted {delta:?} after Finish; malformed stream rejected"
            )));
        }
        match delta {
            WireDelta::Text(t) => summary.push_str(&t),
            WireDelta::Reasoning(_) | WireDelta::Usage { .. } => {}
            WireDelta::Finish { reason, .. } => {
                if finish.is_some() {
                    return Err(SeamError::Context(
                        "summarizer stream finished twice".into(),
                    ));
                }
                finish = Some(reason);
            }
            WireDelta::ToolCallFragment { .. } => {
                return Err(SeamError::Context(
                    "summarizer emitted a tool call; text-only completion required".into(),
                ));
            }
            WireDelta::ProviderError { message, .. } => {
                return Err(SeamError::Context(format!("summarizer failed: {message}")));
            }
            WireDelta::StreamTruncated => {
                return Err(SeamError::Context(
                    "summarizer stream truncated; partial text rejected".into(),
                ));
            }
            WireDelta::StreamEnd => ended = true,
        }
    }
    if !ended {
        return Err(SeamError::Context(
            "summarizer stream had no clean end".into(),
        ));
    }
    match finish {
        Some(FinishReason::Stop) => {}
        other => {
            return Err(SeamError::Context(format!(
                "summarizer finish reason {other:?}; Stop required"
            )));
        }
    }
    let summary = summary.trim().to_string();
    if summary.is_empty() {
        return Err(SeamError::Context("summarizer produced no text".into()));
    }
    Ok(summary)
}

/// RC2-GEMMA6 (S-N4): summarizer material with its older half left out (the
/// newer half is what the task goes on from), marked as cut. `None` once
/// what is left is too short to shrink further.
fn newer_half(material: &str) -> Option<String> {
    const MARK: &str = "[The older part of this context was left out: too long for the model.]\n";
    let body = material.strip_prefix(MARK).unwrap_or(material);
    let chars = body.chars().count();
    if chars < SUMMARY_FLOOR_CHARS {
        return None;
    }
    let kept: String = body.chars().skip(chars / 2).collect();
    Some(format!("{MARK}{kept}"))
}

/// RC2-GEMMA6: below this, summarizer material is not shrunk again.
const SUMMARY_FLOOR_CHARS: usize = 2_000;

/// Assemble the request `messages` for this round. Fits → encode. Over
/// budget → ONE summarize/apply cycle (the plan is deterministic; the
/// summary comes from the same endpoint), then re-assemble; a second
/// NeedsCompaction is a typed failure, not a loop. Summary failure leaves
/// `history` untouched (C4 gate 7 — apply happens only after success).
fn context_messages(
    history: &mut ContextHistory,
    profile: &ModelContextProfile,
    transport: &mut dyn Transport,
    model: &str,
    requests: &mut Vec<String>,
    host_io: &mut dyn TurnHost,
) -> Result<serde_json::Value, SeamError> {
    let counter = seam_counter();
    match assemble(history, profile, &counter).map_err(|e| SeamError::Context(e.to_string()))? {
        AssembleOutcome::Fits(a) => encode_chat_completions(&a),
        AssembleOutcome::NeedsCompaction(plan) => {
            // Compaction is never silent (D2-u1 P1): the host is told BEFORE
            // the summarizer request goes out.
            if host_io.compaction_started().is_err() {
                return Err(SeamError::Cancelled(StopCause::Disconnected));
            }
            if host_io.cancelled() {
                return Err(SeamError::Cancelled(StopCause::Cancelled));
            }
            let summary = summarize_plan(transport, model, &plan, requests)?;
            // RC1-N (pre-review P1): the summary takes no more room than the
            // budget assumed for it, so a fitted exchange still assembles.
            let summary = awili_engine::context::fit_summary(
                summary,
                profile.assumed_summary_tokens,
                &counter,
            );
            let compacted = apply_summary(history, &plan.summarize_ids, summary)
                .map_err(|e| SeamError::Context(e.to_string()))?;
            *history = compacted;
            match assemble(history, profile, &counter)
                .map_err(|e| SeamError::Context(e.to_string()))?
            {
                AssembleOutcome::Fits(a) => encode_chat_completions(&a),
                AssembleOutcome::NeedsCompaction(_) => Err(SeamError::Context(
                    "still over budget after one compaction cycle".into(),
                )),
            }
        }
    }
}

/// Push any `ContinuationBlobCaptured` events newly appended to the trace
/// since the last scan into the history as Continuation groups (after the
/// exchange they trail — the caller scans right after `push_exchange`). The
/// seam profile cannot replay them (`replays_continuations = false`), so they
/// exercise the exclusion accounting; the payloads stay opaque throughout.
fn push_new_blobs(history: &mut ContextHistory, trace: &[AgentEvent], cursor: &mut usize) {
    for ev in &trace[*cursor..] {
        if let AgentEvent::ContinuationBlobCaptured(a) = ev {
            history.push_continuation(a.clone());
        }
    }
    *cursor = trace.len();
}

/// The session's tool registry: the engine's own file tools, then the extra
/// (MCP) tools, registered through the public path (reserved origins are
/// refused; a bare-name clash becomes reachable).
fn tool_registry(extra_tools: &[awili_tools::ToolDef]) -> Result<ToolRegistry, SeamError> {
    let mut registry = ToolRegistry::with_validator(AllowAllValidator);
    fs_builtins::install(&mut registry)
        .map_err(|e| SeamError::Harness(format!("builtin install: {e:?}")))?;
    for def in extra_tools {
        registry
            .register(def.clone())
            .map_err(|e| SeamError::Harness(format!("extra tool register: {e:?}")))?;
    }
    Ok(registry)
}

/// The tools a request advertises, as the model sees them: every registered
/// tool, under its registry alias, the extra tools' descriptions sharing
/// one budget, the block fitted beside `history`'s pinned (system) text.
fn advertised_tools(
    registry: &ToolRegistry,
    extra_tools: &[awili_tools::ToolDef],
    window_tokens: u64,
    history: &ContextHistory,
    model: &str,
) -> Result<Vec<serde_json::Value>, SeamError> {
    // The model must be told about EVERY registered tool, not just the
    // builtins. Registering a tool without projecting it produces a tool that
    // would execute if called but that the model never learns exists — the
    // failure looks like a dead transport, not a missing schema, and costs an
    // evening to diagnose. `extra_tools` was registered above and dropped here
    // until 2026-08-13; every caller passed `&[]`, so nothing caught it.
    // RC1-R-FIX1 (slice review P1): the extra tools' descriptions share one
    // budget for the window, so many described tools never crowd it out.
    let extra_descriptions = fit_descriptions(
        &extra_tools
            .iter()
            .map(|d| d.description.as_str())
            .collect::<Vec<_>>(),
        window_tokens,
    );
    let builtin_defs = fs_builtins::defs();
    // RC2-R5M1 (slice review befb867d, P1): every tool of the request is
    // advertised under the registry's alias, allocated over the complete
    // set, and the names are checked distinct: two tools under one name
    // would send a call to whichever the lookup found first.
    let aliases = registry.aliases();
    let names: Vec<String> = builtin_defs
        .iter()
        .chain(extra_tools.iter())
        .map(|d| advertised_name(d, &aliases))
        .collect();
    let shared = shared_names(names.iter().map(String::as_str));
    if !shared.is_empty() {
        return Err(SeamError::Harness(format!(
            "two tools advertised under one name: {shared:?}"
        )));
    }
    let tools: Vec<serde_json::Value> = builtin_defs
        .iter()
        .zip(&names)
        .map(|(d, name)| {
            awili_tools::openai_tool_project_described(&d.descriptor, name, &d.description)
        })
        .chain(
            extra_tools
                .iter()
                .zip(&names[builtin_defs.len()..])
                .zip(&extra_descriptions)
                .map(|((d, name), text)| {
                    awili_tools::openai_tool_project_described(&d.descriptor, name, text)
                }),
        )
        .collect();
    // RC2-R52: the extra tools fit their share of the window, every one kept.
    let system_tokens: u64 = history
        .groups()
        .iter()
        .map(|g| match g {
            ContextGroup::Pinned { text, .. } => seam_counter().count_text(text) + 8,
            _ => 0,
        })
        .sum();
    let mut tools = fit_tool_block(
        tools,
        builtin_defs.len(),
        window_tokens,
        system_tokens,
    );
    schema_text::preserve(model, &mut tools, builtin_defs.iter().chain(extra_tools));
    Ok(tools)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CatalogToolBinding {
    alias: String,
    identity: ToolIdentity,
    accepted_roots: Option<(ConnectionGeneration, u64)>,
}

/// Turn-local presentation state. The complete accepted registry stays
/// immutable; only the set projected to the next request changes.
struct CatalogTurn {
    registry_binding: Vec<(ToolIdentity, Option<(ConnectionGeneration, u64)>)>,
    selected: Vec<CatalogToolBinding>,
}

fn registry_binding(
    registry: &ToolRegistry,
    roots: &RootsAcceptance,
) -> Vec<(ToolIdentity, Option<(ConnectionGeneration, u64)>)> {
    registry
        .snapshot()
        .into_iter()
        .map(|def| {
            let accepted = roots.accepted(&def.identity.origin);
            (def.identity, accepted)
        })
        .collect()
}

fn pinned_system_tokens(history: &ContextHistory) -> u64 {
    history
        .groups()
        .iter()
        .map(|g| match g {
            ContextGroup::Pinned { text, .. } => seam_counter().count_text(text) + 8,
            _ => 0,
        })
        .sum()
}

/// Whether the already-fitted complete tool array exceeds the seam's actual
/// measured tool allowance. The regular path itself is left untouched.
fn needs_catalog_fallback(
    registry: &ToolRegistry,
    extra_tools: &[awili_tools::ToolDef],
    model: &str,
    window_tokens: u64,
    history: &ContextHistory,
) -> Result<bool, SeamError> {
    let tools = advertised_tools(registry, extra_tools, window_tokens, history, model)?;
    let json = serde_json::Value::Array(tools).to_string();
    let tool_tokens = tool_counter().count_text(&json);
    let allowance = tool_block_cap_tokens(window_tokens, pinned_system_tokens(history));
    let full_profile = seam_profile(model, window_tokens, &json);
    match assemble(history, &full_profile, &seam_counter()) {
        Ok(_) if tool_tokens > allowance => {
            base_projection_fits(extra_tools, model, window_tokens, history)
        }
        Ok(_) => Ok(false),
        Err(awili_engine::context::ContextError::CannotFit { .. }) => {
            base_projection_fits(extra_tools, model, window_tokens, history)
        }
        Err(e) => Err(SeamError::Context(e.to_string())),
    }
}

fn base_projection_fits(
    extra_tools: &[awili_tools::ToolDef],
    model: &str,
    window_tokens: u64,
    history: &ContextHistory,
) -> Result<bool, SeamError> {
    // The legacy full-schema request is already unassemblable with the live
    // user/history. Try the small base projection before activating
    // discovery; if it also cannot fit, preserve the original typed failure.
    let mut candidate = tool_registry(extra_tools)?;
    awili_tools::catalog::install(&mut candidate)
        .map_err(|e| SeamError::Harness(format!("catalog install: {e:?}")))?;
    let base = catalog_advertised_tools(&candidate, &[], window_tokens, model)?;
    let base_json = serde_json::Value::Array(base).to_string();
    let base_profile = seam_profile(model, window_tokens, &base_json);
    match assemble(history, &base_profile, &seam_counter()) {
        Ok(_) => Ok(true),
        Err(awili_engine::context::ContextError::CannotFit { .. }) => Ok(false),
        Err(e) => Err(SeamError::Context(e.to_string())),
    }
}

fn catalog_advertised_tools(
    registry: &ToolRegistry,
    selected: &[CatalogToolBinding],
    window_tokens: u64,
    model: &str,
) -> Result<Vec<serde_json::Value>, SeamError> {
    let aliases = registry.aliases();
    let mut defs: Vec<awili_tools::ToolDef> = fs_builtins::defs();
    defs.extend(awili_tools::catalog::defs());
    for binding in selected {
        let def = registry
            .get(&binding.identity.origin.origin, &binding.identity.name)
            .map_err(|e| SeamError::Harness(format!("catalog selection expired: {}", e.name)))?;
        if def.identity != binding.identity {
            return Err(SeamError::Harness(
                "catalog selection identity changed".into(),
            ));
        }
        defs.push(def.clone());
    }
    let extra_descriptions = fit_descriptions(
        &defs[fs_builtins::defs().len() + awili_tools::catalog::defs().len()..]
            .iter()
            .map(|d| d.description.as_str())
            .collect::<Vec<_>>(),
        window_tokens,
    );
    let names: Vec<String> = defs.iter().map(|d| advertised_name(d, &aliases)).collect();
    let shared = shared_names(names.iter().map(String::as_str));
    if !shared.is_empty() {
        return Err(SeamError::Harness(format!(
            "two catalog tools advertised under one name: {shared:?}"
        )));
    }
    let builtins = fs_builtins::defs();
    let catalog = awili_tools::catalog::defs();
    let mut tools: Vec<_> = builtins
        .iter()
        .chain(catalog.iter())
        .zip(&names)
        .map(|(d, name)| {
            awili_tools::openai_tool_project_described(&d.descriptor, name, &d.description)
        })
        .chain(
            defs[builtins.len() + catalog.len()..]
                .iter()
                .zip(&names[builtins.len() + catalog.len()..])
                .zip(&extra_descriptions)
                .map(|((d, name), description)| {
                    awili_tools::openai_tool_project_described(&d.descriptor, name, description)
                }),
        )
        .collect();
    schema_text::preserve(model, &mut tools, defs.iter());
    Ok(tools)
}

fn catalog_turn(registry: &ToolRegistry, roots: &RootsAcceptance) -> CatalogTurn {
    CatalogTurn {
        registry_binding: registry_binding(registry, roots),
        selected: Vec::new(),
    }
}

/// Keep the configured local OS entry point available for ordinary app/URL
/// tasks in deferred mode. This only advertises its current canonical schema;
/// the normal roots, authorization, approvals and dispatch gates still apply.
fn catalog_with_local_shell(
    registry: &ToolRegistry,
    roots: &RootsAcceptance,
    history: &ContextHistory,
    model: &str,
    window_tokens: u64,
) -> Result<CatalogTurn, SeamError> {
    let mut turn = catalog_turn(registry, roots);
    let Ok(def) = registry.get(LOCAL_SHELL_ORIGIN, "run_command") else {
        return Ok(turn);
    };
    let key = (def.identity.origin.origin.clone(), def.identity.name.clone());
    let Some(alias) = registry.aliases().get(&key).cloned() else {
        return Ok(turn);
    };
    let binding = CatalogToolBinding {
        alias,
        identity: def.identity.clone(),
        accepted_roots: roots.accepted(&def.identity.origin),
    };
    let tools = catalog_advertised_tools(registry, std::slice::from_ref(&binding), window_tokens, model)?;
    let mut profile = seam_profile(model, window_tokens,
        &serde_json::Value::Array(tools).to_string());
    profile.fixed_request_overhead_tokens = profile.fixed_request_overhead_tokens
        .saturating_add(STEP_RESULT_ROOM_TOKENS);
    match assemble(history, &profile, &seam_counter()) {
        Ok(_) => turn.selected.push(binding),
        Err(awili_engine::context::ContextError::CannotFit { .. }) => {}
        Err(error) => return Err(SeamError::Context(error.to_string())),
    }
    Ok(turn)
}

fn validate_catalog_turn(
    registry: &ToolRegistry,
    roots: &RootsAcceptance,
    turn: &CatalogTurn,
) -> Result<(), SeamError> {
    if turn.registry_binding != registry_binding(registry, roots) {
        return Err(SeamError::Harness(
            "catalog registry identity or accepted roots changed during turn".into(),
        ));
    }
    for binding in &turn.selected {
        let def = registry
            .get(&binding.identity.origin.origin, &binding.identity.name)
            .map_err(|_| SeamError::Harness("catalog selection expired".into()))?;
        if def.identity != binding.identity
            || roots.accepted(&binding.identity.origin) != binding.accepted_roots
        {
            return Err(SeamError::Harness(
                "catalog selection binding changed".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
enum CatalogPlan {
    Search {
        response: String,
    },
    Selection {
        response: String,
        selected: Vec<CatalogToolBinding>,
    },
    Rejected {
        response: String,
    },
}

#[derive(Clone, Copy, Debug)]
enum CatalogTrial {
    Fits,
    NeedsCompaction,
    CannotFit {
        needed_tokens: u64,
        budget_tokens: u64,
    },
}

fn digest_text(identity: &ToolIdentity) -> String {
    identity
        .descriptor_digest
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn catalog_search_key(text: &str) -> String {
    text.chars()
        .map(|c| {
            if matches!(c, '_' | '-' | '.' | ':') {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn catalog_source_hint(origin: &str) -> (String, bool) {
    // Origins normally contain a configured server name, never a transport
    // endpoint. Withhold unusual names rather than exposing paths or URLs.
    let name = origin
        .strip_prefix("mcp:")
        .or_else(|| origin.strip_prefix("builtin:"));
    if !name.is_some_and(|name| {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c.is_whitespace() || matches!(c, '-' | '_' | '.'))
    }) {
        return ("source unavailable".into(), false);
    }
    let mut chars = origin.chars();
    let hint = chars
        .by_ref()
        .take(160)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    (hint, chars.next().is_some())
}

fn catalog_results(
    registry: &ToolRegistry,
    query: &str,
    limit: usize,
    offset: usize,
    wire: &wire_names::WireNames,
) -> serde_json::Value {
    let aliases = registry.aliases();
    let query = query.to_lowercase();
    let terms: Vec<_> = std::iter::once(query.as_str())
        .chain(query.split(',').map(str::trim))
        .filter(|term| !term.is_empty())
        .map(|term| (term, catalog_search_key(term)))
        .collect();
    let mut entries: Vec<_> = registry
        .snapshot()
        .into_iter()
        .filter_map(|def| {
            if def.identity.origin == awili_tools::catalog::origin()
                && def.identity.name == "tools.find"
            {
                return None;
            }
            let key = (
                def.identity.origin.origin.clone(),
                def.identity.name.clone(),
            );
            let alias = aliases.get(&key)?.clone();
            let (source_hint, source_hint_truncated) =
                catalog_source_hint(&def.identity.origin.origin);
            let matches_query = query.is_empty()
                || [
                    alias.as_str(),
                    def.description.as_str(),
                    source_hint.as_str(),
                ]
                .iter()
                .any(|text| {
                    let text = text.to_lowercase();
                    let text_key = catalog_search_key(&text);
                    terms.iter().any(|(term, key)| {
                        text.contains(*term)
                            || (key.chars().any(char::is_alphanumeric) && text_key.contains(key))
                    })
                });
            if !matches_query {
                return None;
            }
            let description: String = def
                .description
                .chars()
                .filter(|c| !c.is_control())
                .take(160)
                .collect();
            Some((
                alias.clone(),
                serde_json::json!({
                    "alias": alias,
                    "call_name": wire.wire(&alias),
                    "source_hint": source_hint,
                    "source_hint_truncated": source_hint_truncated,
                    "description": description,
                    "schema_digest": digest_text(&def.identity),
                }),
            ))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let total = entries.len();
    let page: Vec<_> = entries
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(_, value)| value)
        .collect();
    let next_offset = (offset + page.len() < total).then_some(offset + page.len());
    let mut result = serde_json::json!({"tools": page, "total": total, "next_offset": next_offset});
    if total == 0 {
        result["guidance"] = serde_json::json!("No matches for this search. This does not establish that a capability is unavailable. Retry one short term such as shell, browser, or archive, then select exact returned aliases.");
    }
    result
}

// Keep the explicit turn, call, and request-budget inputs visible at this boundary.
#[allow(clippy::too_many_arguments)]
fn prepare_catalog_plan(
    registry: &ToolRegistry,
    turn: &CatalogTurn,
    roots: &RootsAcceptance,
    call: &ToolCall,
    history: &ContextHistory,
    assistant_text: &str,
    calls: &[ToolCall],
    window_tokens: u64,
    model: &str,
    wire: &wire_names::WireNames,
) -> Result<CatalogPlan, SeamError> {
    validate_catalog_turn(registry, roots, turn)?;
    let args = match awili_engine_authz::parse_catalog_metadata_arguments(&call.arguments) {
        Ok(args) => args,
        Err(error) => return Ok(CatalogPlan::Rejected {
            response: serde_json::json!({"error": error.message()}).to_string(),
        }),
    };
    let query = args.query;
    let limit = args.limit as usize;
    let offset = args.offset as usize;
    let aliases = args.select;
    if aliases.is_empty() {
        if offset > registry.len() {
            return Ok(CatalogPlan::Rejected {
                response: "{\"error\":\"offset out of range\"}".into(),
            });
        }
        let mut result = catalog_results(registry, &query, limit, offset, wire);
        // Discovery should make the returned tools usable on the next request.
        // This is only schema projection: calls still pass the ordinary roots,
        // argument, approval and dispatch gates. Reuse the selection transaction
        // so the complete page and the next result reserve must fit together.
        let mut selected = turn.selected.clone();
        let mut discovered = false;
        for entry in result["tools"].as_array().into_iter().flatten() {
            let Some(alias) = entry["alias"].as_str() else {
                continue;
            };
            let def = registry
                .lookup_unique(alias)
                .map_err(|e| SeamError::Harness(format!("catalog search binding: {e:?}")))?;
            if def.identity.origin.origin != awili_tools::catalog::origin().origin
                && def.identity.origin.origin != awili_engine_authz::policy::BUILTIN_FS_ORIGIN
                && !selected.iter().any(|binding| binding.identity == def.identity)
            {
                discovered = true;
                selected.push(CatalogToolBinding {
                    alias: alias.to_string(),
                    identity: def.identity.clone(),
                    accepted_roots: roots.accepted(&def.identity.origin),
                });
            }
        }
        if !query.trim().is_empty() && discovered {
            let mut activated = result.clone();
            activated["selected"] = serde_json::json!(
                selected.iter().map(|binding| binding.alias.as_str()).collect::<Vec<_>>()
            );
            activated["next_request"] = serde_json::json!(
                "Returned MCP tools receive complete schemas on the next request; call their call_name directly. Discovery has not executed any tool."
            );
            let response = activated.to_string();
            let trial = if selected.len() > 20 {
                None
            } else {
                Some(catalog_result_fits(
                    history, model, window_tokens, registry, &selected,
                    assistant_text, calls, call, &response,
                )?)
            };
            match trial {
                Some(CatalogTrial::Fits | CatalogTrial::NeedsCompaction) => {
                    return Ok(CatalogPlan::Selection { response, selected });
                }
                None | Some(CatalogTrial::CannotFit { .. }) => {
                    // Never partially activate a page or discard the previous
                    // selection when its complete schemas exceed the budget.
                    result["selection_required"] = serde_json::json!(true);
                    result["next_request"] = serde_json::json!(
                        "The complete search page plus active tools exceeds the schema budget or 20-tool limit; the previous selection is unchanged. Use a narrower query or smaller limit, or select fewer exact returned aliases before calling them."
                    );
                }
            }
        }
        result["selected"] = serde_json::json!(
            turn.selected.iter().map(|binding| binding.alias.as_str()).collect::<Vec<_>>()
        );
        let mut response = result.to_string();
        match catalog_result_fits(
            history, model, window_tokens, registry, &turn.selected,
            assistant_text, calls, call, &response,
        )? {
            CatalogTrial::Fits | CatalogTrial::NeedsCompaction => {}
            CatalogTrial::CannotFit { needed_tokens, budget_tokens } => {
                response = serde_json::json!({
                    "error":"catalog_page_too_large",
                    "needed_tokens":needed_tokens,
                    "available_tokens":budget_tokens,
                    "retry":"use a smaller limit or query"
                }).to_string();
            }
        }
        return Ok(CatalogPlan::Search { response });
    }
    if aliases.len() > 20 {
        return Ok(CatalogPlan::Rejected {
            response: "{\"error\":\"selection exceeds 20 aliases\"}".into(),
        });
    }
    let requested_aliases: Vec<&str> = aliases.iter().map(String::as_str).collect();
    let registry_aliases = registry.aliases();
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for alias in &aliases {
        if !seen.insert(alias.to_string()) {
            return Ok(CatalogPlan::Rejected {
                response: "{\"error\":\"duplicate selection alias\"}".into(),
            });
        }
        let Some((origin, name)) =
            registry_aliases
                .iter()
                .find_map(|((origin, name), registered)| {
                    (registered == alias).then_some((origin, name))
                })
        else {
            let suggestions = catalog_results(registry, &query, limit, 0, wire);
            let mut response = serde_json::json!({
                "error":"unknown exact alias",
                "alias":alias,
                "guidance":"Use an exact canonical alias from matches, or search tools.find, then retry selection; the active selection was not changed.",
                "matches":suggestions["tools"],
                "next_offset":suggestions["next_offset"]
            }).to_string();
            match catalog_result_fits(
                history, model, window_tokens, registry, &turn.selected,
                assistant_text, calls, call, &response,
            )? {
                CatalogTrial::Fits | CatalogTrial::NeedsCompaction => {}
                CatalogTrial::CannotFit { needed_tokens, budget_tokens } => {
                    response = serde_json::json!({
                        "error":"unknown exact alias",
                        "alias":alias,
                        "guidance":"No suggestions fit this request. Search a smaller page, then retry an exact canonical alias; the active selection was not changed.",
                        "needed_tokens":needed_tokens,
                        "available_tokens":budget_tokens
                    }).to_string();
                }
            }
            return Ok(CatalogPlan::Rejected { response });
        };
        let def = registry
            .get(origin, name)
            .map_err(|e| SeamError::Harness(format!("catalog alias lookup: {e:?}")))?;
        if def.identity.origin.origin != awili_tools::catalog::origin().origin
            && def.identity.origin.origin != awili_engine_authz::policy::BUILTIN_FS_ORIGIN
        {
            selected.push(CatalogToolBinding {
                alias: alias.to_string(),
                identity: def.identity.clone(),
                accepted_roots: roots.accepted(&def.identity.origin),
            });
        }
    }
    let response = serde_json::json!({"selected": &requested_aliases, "next_request":"selected tools receive their complete schemas"}).to_string();
    match catalog_result_fits(
        history,
        model,
        window_tokens,
        registry,
        &selected,
        assistant_text,
        calls,
        call,
        &response,
    )? {
        CatalogTrial::Fits | CatalogTrial::NeedsCompaction => {
            Ok(CatalogPlan::Selection { response, selected })
        }
        CatalogTrial::CannotFit {
            needed_tokens,
            budget_tokens,
        } => {
            let response = serde_json::json!({
                "error":"selection_too_large",
                "selected":&requested_aliases,
                "needed_tokens":needed_tokens,
                "available_tokens":budget_tokens,
                "retry":"select fewer exact aliases"
            })
            .to_string();
            Ok(CatalogPlan::Rejected { response })
        }
    }
}

// Keep the active history, exact call, selected schemas, and measured request budget explicit.
#[allow(clippy::too_many_arguments)]
fn catalog_result_fits(
    history: &ContextHistory,
    model: &str,
    window_tokens: u64,
    registry: &ToolRegistry,
    selected: &[CatalogToolBinding],
    assistant_text: &str,
    calls: &[ToolCall],
    call: &ToolCall,
    response: &str,
) -> Result<CatalogTrial, SeamError> {
    let candidate_tools = catalog_advertised_tools(registry, selected, window_tokens, model)?;
    let mut profile = seam_profile(
        model,
        window_tokens,
        &serde_json::Value::Array(candidate_tools).to_string(),
    );
    // Preserve space for the next tool result after the newly-advertised
    // schemas. The current find call and result are already in `projected`;
    // model output/tool-call room and safety margin are in seam_profile.
    profile.fixed_request_overhead_tokens = profile
        .fixed_request_overhead_tokens
        .saturating_add(STEP_RESULT_ROOM_TOKENS);
    let mut projected = history.clone();
    projected
        .push_exchange(
            assistant_text,
            calls.to_vec(),
            vec![ToolResultRecord {
                awili_call_id: call.awili_call_id.clone(),
                content: response.to_string(),
                is_error: false,
            }],
        )
        .map_err(|e| SeamError::Context(e.to_string()))?;
    match assemble(&projected, &profile, &seam_counter()) {
        Ok(AssembleOutcome::Fits(_)) => Ok(CatalogTrial::Fits),
        Ok(AssembleOutcome::NeedsCompaction(_)) => Ok(CatalogTrial::NeedsCompaction),
        Err(awili_engine::context::ContextError::CannotFit {
            needed_tokens,
            budget_tokens,
        }) => Ok(CatalogTrial::CannotFit {
            needed_tokens,
            budget_tokens,
        }),
        Err(error) => Err(SeamError::Context(error.to_string())),
    }
}

#[cfg(test)]
mod catalog_projection_tests {
    use super::*;
    use awili_tools::{CanonicalDescriptor, ToolDef, ToolOrigin};

    fn def(origin: &str, name: &str, schema: serde_json::Value) -> ToolDef {
        ToolDef::from_parts(
            ToolOrigin::new(origin),
            name,
            CanonicalDescriptor {
                schema,
                side_effect: "read_only".into(),
                required_capabilities: vec![],
                executor: origin.into(),
            },
        )
        .with_description("synthetic catalog fixture")
    }

    #[test]
    fn catalog_search_finds_source_name_variants_and_reports_application_scope() {
        let shell = def(
            "mcp:mac-shell",
            "run_command",
            serde_json::json!({"type":"object"}),
        )
        .with_description("Run a one-off shell command, with a timeout.");
        let drawing = def(
            "mcp:vectorcraft",
            "press_key",
            serde_json::json!({"type":"object"}),
        )
        .with_description("Send a key to the desktop application.");
        let mut registry = tool_registry(&[shell, drawing]).unwrap();
        awili_tools::catalog::install(&mut registry).unwrap();
        let aliases: Vec<_> = registry.aliases().into_values().collect();
        let wire = wire_names::WireNames::new(aliases.iter().map(String::as_str));
        for query in [
            "mac_shell",
            "mac-shell",
            "MCP.MAC-SHELL",
            "  mac  shell  ",
            "run.command",
        ] {
            let page = catalog_results(&registry, query, 10, 0, &wire);
            let tools = page["tools"].as_array().unwrap();
            assert_eq!(tools.len(), 1, "query {query}: {page}");
            assert_eq!(tools[0]["alias"], "run_command");
            assert_eq!(tools[0]["source_hint"], "mcp:mac-shell");
            assert_eq!(tools[0]["source_hint_truncated"], false);
        }
        for query in ["", "shell", "browser", "archive", "tools.find"] {
            let page = catalog_results(&registry, query, 10, 0, &wire);
            assert!(page["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| tool["source_hint"] != "builtin:catalog"));
        }
        for query in ["shell, desktop, mac_shell", "  SHELL , desktop , shell  "] {
            let page = catalog_results(&registry, query, 10, 0, &wire);
            assert_eq!(page["total"], 2, "comma alternatives: {page}");
            assert_eq!(page["tools"].as_array().unwrap().len(), 2);
        }
        let page = catalog_results(&registry, "browser, chrome, desktop", 10, 0, &wire);
        assert_eq!(page["total"], 1);
        assert_eq!(page["tools"][0]["alias"], "press_key");
        for query in [",,,", "___,___"] {
            let page = catalog_results(&registry, query, 10, 0, &wire);
            assert_eq!(page["total"], 0, "query {query}: {page}");
            assert!(page["guidance"]
                .as_str()
                .unwrap()
                .contains("Retry one short term"));
        }
        let desktop = catalog_results(&registry, "desktop", 10, 0, &wire);
        assert_eq!(desktop["tools"][0]["source_hint"], "mcp:vectorcraft");
        assert_eq!(desktop["tools"][0]["alias"], "press_key");
        for query in ["open chrome", "___"] {
            assert_eq!(catalog_results(&registry, query, 10, 0, &wire)["total"], 0);
        }
    }

    #[test]
    fn catalog_source_hint_withholds_endpoint_or_path_names() {
        for origin in [
            "mcp:https://user:secret@example.com",
            "mcp:/private/config",
            "mcp:user@host",
            "mcp:label\u{202e}",
        ] {
            assert_eq!(
                catalog_source_hint(origin),
                ("source unavailable".into(), false)
            );
        }
    }

    #[test]
    fn catalog_source_hint_is_bounded_without_changing_the_registered_identity() {
        let origin = format!("mcp:vectorcraft\n{}", "界".repeat(500));
        let tool = def(&origin, "inspect_ui", serde_json::json!({"type":"object"}));
        let registry = tool_registry(&[tool]).unwrap();
        let aliases: Vec<_> = registry.aliases().into_values().collect();
        let wire = wire_names::WireNames::new(aliases.iter().map(String::as_str));
        let page = catalog_results(&registry, "vectorcraft", 10, 0, &wire);
        let entry = &page["tools"][0];
        let hint = entry["source_hint"].as_str().unwrap();
        assert_eq!(hint.chars().count(), 160);
        assert!(!hint.chars().any(char::is_control));
        assert_eq!(entry["source_hint_truncated"], true);
        assert_eq!(
            registry
                .get(&origin, "inspect_ui")
                .unwrap()
                .identity
                .origin
                .origin,
            origin
        );
    }

    #[test]
    fn catalog_projection_keeps_registry_complete_and_selected_schema_canonical() {
        let selected = def(
            "mcp:fixture",
            "read_fixture",
            serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
        );
        let hidden = def(
            "mcp:fixture",
            "other_fixture",
            serde_json::json!({"type":"object"}),
        );
        let mut registry = tool_registry(&[selected.clone(), hidden.clone()]).unwrap();
        awili_tools::catalog::install(&mut registry).unwrap();
        assert_eq!(registry.len(), 8);
        let turn = catalog_turn(&registry, &RootsAcceptance::default());
        let selected_binding = CatalogToolBinding {
            alias: registry.aliases()[&(
                selected.identity.origin.origin.clone(),
                selected.identity.name.clone(),
            )]
                .clone(),
            identity: selected.identity.clone(),
            accepted_roots: None,
        };
        let projected = catalog_advertised_tools(&registry, &[selected_binding], 32_768, "test-model").unwrap();
        let names: Vec<_> = projected
            .iter()
            .filter_map(|t| t.pointer("/function/name").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(names.len(), 7);
        let hidden_name = registry.aliases()[&(
            hidden.identity.origin.origin.clone(),
            hidden.identity.name.clone(),
        )]
            .clone();
        assert!(!names.contains(&hidden_name.as_str()));
        let chosen_name = &registry.aliases()[&(
            selected.identity.origin.origin.clone(),
            selected.identity.name.clone(),
        )];
        let chosen = projected
            .iter()
            .find(|t| t.pointer("/function/name").and_then(|v| v.as_str()) == Some(chosen_name))
            .unwrap();
        assert_eq!(
            chosen.pointer("/function/parameters"),
            Some(&selected.descriptor.schema)
        );
        assert_eq!(
            registry.len(),
            8,
            "selection only changes projection, never registry contents"
        );
        validate_catalog_turn(&registry, &RootsAcceptance::default(), &turn).unwrap();
    }

    #[test]
    fn deferred_local_shell_is_available_without_selecting_an_unrelated_server() {
        let schema = serde_json::json!({"type":"object", "properties":{"command":{"type":"string"}}, "required":["command"]});
        let local = def("mcp:mac-shell", "run_command", schema.clone());
        let remote = def("mcp:remote", "run_command", serde_json::json!({"type":"object", "properties":{"different":{"type":"string"}}}));
        let mut registry = tool_registry(&[local.clone(), remote]).unwrap();
        awili_tools::catalog::install(&mut registry).unwrap();
        let mut history = ContextHistory::new(); history.push_user("open a visible app");
        let turn = catalog_with_local_shell(&registry, &RootsAcceptance::default(), &history, "gemma-4", 32_768).unwrap();
        assert_eq!(turn.selected.len(), 1);
        assert_eq!(turn.selected[0].identity, local.identity);
        let tools = catalog_advertised_tools(&registry, &turn.selected, 32_768, "gemma-4").unwrap();
        let selected = tools.iter().find(|t| t["function"]["name"] == turn.selected[0].alias).unwrap();
        assert_eq!(selected["function"]["parameters"], schema);
        assert_eq!(tools.len(), 7);
        validate_catalog_turn(&registry, &RootsAcceptance::default(), &turn).unwrap();
    }

    #[test]
    fn deferred_local_shell_does_not_force_a_schema_that_cannot_fit() {
        let local = def("mcp:mac-shell", "run_command", serde_json::json!({"type":"object", "description":"S".repeat(100_000)}));
        let mut registry = tool_registry(&[local]).unwrap();
        awili_tools::catalog::install(&mut registry).unwrap();
        let mut history = ContextHistory::new(); history.push_user("open a visible app");
        let turn = catalog_with_local_shell(&registry, &RootsAcceptance::default(), &history, "gemma-4", 8_192).unwrap();
        assert!(turn.selected.is_empty());
        let absent = tool_registry(&[]).unwrap();
        assert!(catalog_with_local_shell(&absent, &RootsAcceptance::default(), &history, "gemma-4", 8_192).unwrap().selected.is_empty());
    }

    #[test]
    fn gemma_complex_schema_survives_both_projections() {
        // REA 6.4's real advertisement: referenced launch/connect union,
        // URL objects and action branches were absent after Gemma templating.
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/browser-input-schema.json"))
                .unwrap();
        let tool = def("mcp:rea", "capture_browser_scenario", schema.clone())
            .with_description("Controlled browser capture");
        let mut registry = tool_registry(std::slice::from_ref(&tool)).unwrap();
        awili_tools::catalog::install(&mut registry).unwrap();
        let binding = CatalogToolBinding {
            alias: "capture_browser_scenario".into(),
            identity: tool.identity.clone(),
            accepted_roots: None,
        };
        let gemma = "/models/gemma-4-26b-a4b-it-qat-mlx-4bit";
        let full = advertised_tools(
            &registry,
            std::slice::from_ref(&tool),
            32_768,
            &ContextHistory::new(),
            gemma,
        )
        .unwrap();
        let selected =
            catalog_advertised_tools(&registry, std::slice::from_ref(&binding), 32_768, gemma)
                .unwrap();
        for tools in [&full, &selected] {
            let projected = tools
                .iter()
                .find(|t| t["function"]["name"] == "capture_browser_scenario")
                .unwrap();
            let description = projected["function"]["description"].as_str().unwrap();
            let (_, text) = description
                .split_once(schema_text::PREFIX)
                .expect("plain text contract");
            let disclosed: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(
                disclosed, schema,
                "all refs, unions, defaults and constraints survive"
            );
            assert!(text.contains("executable_path"));
            assert!(text.contains("destination"));
            assert!(text.contains("wait_until"));
        }
        assert_eq!(selected.last().unwrap()["function"]["parameters"], schema);
        assert_eq!(
            registry
                .lookup_unique("capture_browser_scenario")
                .unwrap()
                .identity,
            tool.identity
        );
        let other = catalog_advertised_tools(&registry, &[binding], 32_768, "test-model").unwrap();
        assert_eq!(
            other.last().unwrap()["function"]["description"],
            "Controlled browser capture"
        );
        assert_eq!(other.last().unwrap()["function"]["parameters"], schema);
    }

    #[test]
    fn gemma_schema_text_keeps_collision_bindings_and_escapes_template_tokens() {
        let first = def("mcp:one", "same", serde_json::json!({
            "type":"object", "properties":{"field":{"const":"<|tool>FIRST<tool|>\n"}}
        }));
        let second = def("mcp:two", "same", serde_json::json!({
            "type":"object", "properties":{"other":{"const":"SECOND"}}
        }));
        let extras = [first.clone(), second.clone()];
        let mut registry = tool_registry(&extras).unwrap();
        awili_tools::catalog::install(&mut registry).unwrap();
        let aliases = registry.aliases();
        let bindings: Vec<_> = extras.iter().map(|def| CatalogToolBinding {
            alias: aliases[&(def.identity.origin.origin.clone(), def.identity.name.clone())].clone(),
            identity: def.identity.clone(), accepted_roots: None,
        }).collect();
        let full = advertised_tools(&registry, &extras, 32_768, &ContextHistory::new(), "Gemma4").unwrap();
        let selected = catalog_advertised_tools(&registry, &bindings, 32_768, "Gemma4").unwrap();
        for tools in [full, selected] {
            for binding in &bindings {
                let projected = tools.iter().find(|t| t["function"]["name"] == binding.alias).unwrap();
                let text = projected["function"]["description"].as_str().unwrap()
                    .split_once(schema_text::PREFIX).unwrap().1;
                assert!(!text.contains("<|tool>"));
                assert!(!text.contains("<tool|>"));
                let original = registry.get(&binding.identity.origin.origin, &binding.identity.name).unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(text).unwrap(), original.descriptor.schema);
            }
        }
    }

    #[test]
    fn gemma_schema_supplement_counts_before_fallback() {
        let schema = serde_json::json!({
            "type":"object", "properties":{"value":{"$ref":"#/$defs/value"}},
            "$defs":{"value":{"type":"string","description":"D".repeat(50_000)}}
        });
        let extra = def("mcp:fixture", "large_contract", schema);
        let extras = [extra];
        let registry = tool_registry(&extras).unwrap();
        let mut history = ContextHistory::new();
        history.push_user("exercise the schema budget");
        assert!(
            !needs_catalog_fallback(&registry, &extras, "test-model", 32_768, &history).unwrap()
        );
        assert!(
            needs_catalog_fallback(&registry, &extras, "gemma-4-26b", 32_768, &history).unwrap()
        );
    }

    #[test]
    fn gemma_simple_tools_keep_their_existing_projection() {
        let simple = def(
            "mcp:fixture",
            "simple",
            serde_json::json!({
                "type":"object", "properties":{"command":{"type":"string"}}, "required":["command"]
            }),
        )
        .with_description("Run a command");
        let extras = [simple];
        let registry = tool_registry(&extras).unwrap();
        let history = ContextHistory::new();
        let plain = advertised_tools(&registry, &extras, 32_768, &history, "test-model").unwrap();
        let gemma = advertised_tools(&registry, &extras, 32_768, &history, "gemma-4-26b").unwrap();
        assert_eq!(gemma, plain);
    }

    #[test]
    fn fallback_wire_names_are_allocated_against_complete_registry() {
        let defs = [
            def("mcp:one", "a.b", serde_json::json!({"type":"object"})),
            def("mcp:two", "a_b", serde_json::json!({"type":"object"})),
            def("mcp:three", "a_b_2", serde_json::json!({"type":"object"})),
            def(
                "mcp:four",
                "tools.find",
                serde_json::json!({"type":"object"}),
            ),
        ];
        let mut registry = tool_registry(&defs).unwrap();
        awili_tools::catalog::install(&mut registry).unwrap();
        let aliases: Vec<_> = registry.aliases().into_values().collect();
        let stable = wire_names::WireNames::new(aliases.iter().map(String::as_str));
        let active = vec![serde_json::json!({"function":{"name":"a.b"}})];
        let unstable = wire_names::WireNames::for_tools(&active);
        assert_eq!(stable.wire("a.b"), "a_b_3");
        assert_ne!(stable.wire("a.b"), unstable.wire("a.b"));
        assert_eq!(stable.canonical(&stable.wire("a_b_2")), "a_b_2");
        let catalog_alias =
            registry.aliases()[&("builtin:catalog".into(), "tools.find".into())].clone();
        let mcp_alias = registry.aliases()[&("mcp:four".into(), "tools.find".into())].clone();
        assert_ne!(catalog_alias, mcp_alias);
        let page = catalog_results(&registry, "tools.find", 20, 0, &stable);
        let entries = page["tools"].as_array().unwrap();
        let call_names: BTreeSet<_> = entries
            .iter()
            .filter(|entry| entry["alias"] == catalog_alias || entry["alias"] == mcp_alias)
            .map(|entry| entry["call_name"].as_str().unwrap())
            .collect();
        assert_eq!(
            call_names.len(),
            1,
            "catalog excludes itself while the MCP alias keeps its stable wire name"
        );
    }

    #[test]
    fn active_user_context_can_activate_base_catalog_when_full_projection_cannot_fit() {
        let extras: Vec<_> = (0..32)
            .map(|index| {
                def(
                    "mcp:fixture",
                    &format!("fixture_{index:02}"),
                    serde_json::json!({
                        "type":"object",
                        "properties":{"value":{"type":"string"}},
                        "additionalProperties":false
                    }),
                )
            })
            .collect();
        let registry = tool_registry(&extras).unwrap();
        let full_tools =
            advertised_tools(&registry, &extras, 32_768, &ContextHistory::new(), "test-model").unwrap();
        let full_json = serde_json::Value::Array(full_tools).to_string();
        let mut base_registry = tool_registry(&extras).unwrap();
        awili_tools::catalog::install(&mut base_registry).unwrap();
        let base_tools = catalog_advertised_tools(&base_registry, &[], 32_768, "test-model").unwrap();
        let base_json = serde_json::Value::Array(base_tools).to_string();
        let full_profile = seam_profile("test-model", 32_768, &full_json);
        let base_profile = seam_profile("test-model", 32_768, &base_json);
        assert!(
            full_profile.fixed_request_overhead_tokens > base_profile.fixed_request_overhead_tokens
        );

        let max_full_user_tokens = input_budget_tokens(32_768)
            .saturating_sub(full_profile.fixed_request_overhead_tokens)
            .saturating_sub(full_profile.per_message_overhead_tokens);
        let mut history = ContextHistory::new();
        history.push_user("x".repeat((max_full_user_tokens as usize) * 3 + 1));
        assert!(matches!(
            assemble(&history, &full_profile, &seam_counter()),
            Err(awili_engine::context::ContextError::CannotFit { .. })
        ));
        assert!(matches!(
            assemble(&history, &base_profile, &seam_counter()),
            Ok(AssembleOutcome::Fits(_))
        ));
        assert!(
            needs_catalog_fallback(&registry, &extras, "test-model", 32_768, &history).unwrap()
        );
    }

    #[test]
    fn full_projection_is_kept_when_tool_cap_overflows_but_base_cannot_fit() {
        let extras: Vec<_> = Vec::new();
        let registry = tool_registry(&extras).unwrap();
        let empty = ContextHistory::new();
        let mut found = false;
        for window in (3_500..=12_000).step_by(64) {
            let full_tools = advertised_tools(&registry, &extras, window, &empty, "test-model").unwrap();
            let full_json = serde_json::Value::Array(full_tools).to_string();
            let full_tokens = tool_counter().count_text(&full_json);
            let allowance = tool_block_cap_tokens(window, pinned_system_tokens(&empty));
            if full_tokens <= allowance {
                continue;
            }

            let mut base_registry = tool_registry(&extras).unwrap();
            awili_tools::catalog::install(&mut base_registry).unwrap();
            let base_tools = catalog_advertised_tools(&base_registry, &[], window, "test-model").unwrap();
            let base_json = serde_json::Value::Array(base_tools).to_string();
            let full_profile = seam_profile("test-model", window, &full_json);
            let base_profile = seam_profile("test-model", window, &base_json);
            let full_user_tokens = input_budget_tokens(window)
                .saturating_sub(full_profile.fixed_request_overhead_tokens)
                .saturating_sub(full_profile.per_message_overhead_tokens);
            let mut history = ContextHistory::new();
            history.push_user("x".repeat(full_user_tokens as usize * 3));
            if !matches!(
                assemble(&history, &full_profile, &seam_counter()),
                Ok(AssembleOutcome::Fits(_))
            ) || !matches!(
                assemble(&history, &base_profile, &seam_counter()),
                Err(awili_engine::context::ContextError::CannotFit { .. })
            ) {
                continue;
            }

            assert!(
                !needs_catalog_fallback(&registry, &extras, "test-model", window, &history)
                    .unwrap()
            );
            found = true;
            break;
        }
        assert!(
            found,
            "fixture should find an over-cap full projection whose base is too large"
        );
    }
}

/// RC2-R617: the user message of a warm-up request.
pub const WARM_USER: &str = "ok";

/// RC2-R617 (usability RC2 round 5, Major 1: a cold first Gemma turn took
/// 54 s, almost all of it the model server reading the owner's rules,
/// AGENTS.md and the tool list for the first time): ask the model server
/// to read what every request of this session begins with — `history`
/// (its pinned system text and kept turns) and the tools — ahead of the
/// owner's question, with a one-word message and a one-token answer. The
/// server keeps what it read (mlx_lm keeps the cache at the end of the
/// system part), so the first real request reads only the question.
///
/// The request is built as a turn's first request is (the same tools,
/// wire names and messages, [`advertised_tools`]); only the last message
/// and `max_tokens` differ. Nothing is logged and `history` is not changed.
/// `host_io.prompt_progress` gets the server's reading progress; a
/// `cancelled` host stops reading. Returns `false` when there was nothing
/// to send (the context would need a summary first). A server that
/// answers with an error is an `Err` (RC2-R6M2): it read nothing. So is
/// an answer that did not end cleanly (RC2-R6M5); a stopped one is `false`.
pub fn warm_prompt_cache(
    transport: &mut dyn Transport,
    model: &str,
    extra_tools: &[awili_tools::ToolDef],
    window_tokens: u64,
    history: &ContextHistory,
    host_io: &mut dyn TurnHost,
) -> Result<bool, SeamError> {
    let projection_window = projection_window_tokens(window_tokens, transport.max_output_tokens());
    let mut registry = tool_registry(extra_tools)?;
    let mut probe = history.clone();
    probe.push_user(WARM_USER);
    let fallback = needs_catalog_fallback(&registry, extra_tools, model, projection_window, &probe)?;
    let tools = if fallback {
        awili_tools::catalog::install(&mut registry)
            .map_err(|e| SeamError::Harness(format!("catalog install: {e:?}")))?;
        let turn = catalog_with_local_shell(&registry, &RootsAcceptance::default(), &probe, model, projection_window)?;
        catalog_advertised_tools(&registry, &turn.selected, projection_window, model)?
    } else {
        advertised_tools(&registry, extra_tools, projection_window, history, model)?
    };
    let mut profile = seam_profile(
        model,
        window_tokens,
        &serde_json::Value::Array(tools.clone()).to_string(),
    );
    profile.reserved_output_tokens = transport.max_output_tokens();
    let wire = if fallback {
        let aliases = registry.aliases().into_values().collect::<Vec<_>>();
        wire_names::WireNames::new(aliases.iter().map(String::as_str))
    } else {
        wire_names::WireNames::for_tools(&tools)
    };
    let assembled = match assemble(&probe, &profile, &seam_counter())
        .map_err(|e| SeamError::Context(e.to_string()))?
    {
        AssembleOutcome::Fits(a) => a,
        AssembleOutcome::NeedsCompaction(_) => return Ok(false),
    };
    let messages = encode_chat_completions(&assembled)?;
    let mut body = serde_json::json!({
        "model": model,
        "stream": true,
        "stream_options": {"include_usage": true},
        "max_tokens": 1,
        "tool_choice": "auto",
        "tools": tools,
        "messages": messages,
    });
    wire.encode_body(&mut body);
    if host_io.cancelled() {
        return Ok(false);
    }
    let mut on_line = |line: &str| -> bool {
        if host_io.cancelled() {
            return false;
        }
        if let Some((done, total)) = keepalive_progress(line) {
            host_io.prompt_progress(done, total);
        }
        true
    };
    let raw = transport.stream_lines(&body.to_string(), &mut on_line)?;
    // RC2-R6M2 (RC2-R6 review P2 on 0fc8434b): a server that answered
    // with an error (a model not loaded, a request it refused) read
    // nothing, and the app must not count it warm.
    let deltas = decode_stream(&raw);
    let refused = deltas.iter().find_map(|d| match d {
        awili_engine::wire::WireDelta::ProviderError { message, .. } => Some(message.clone()),
        _ => None,
    });
    if let Some(message) = refused {
        return Err(SeamError::Transport(format!(
            "the model server refused the warm-up: {message}"
        )));
    }
    // RC2-R6M5 (RC2-R6 review P2 on 9c0fa36d): only an answer that ended
    // cleanly (`[DONE]`) shows the server read the warm-up through; an
    // empty or cut-off answer (even an HTTP 200) does not.
    let ended = deltas
        .iter()
        .any(|d| matches!(d, awili_engine::wire::WireDelta::StreamEnd));
    if !ended {
        if host_io.cancelled() {
            return Ok(false);
        }
        return Err(SeamError::Transport(
            "the model server's answer to the warm-up was cut off".into(),
        ));
    }
    Ok(true)
}

// -- HTTP transport (shared by the bin and awili-engine-worker) --------------

/// A request gate: `Ok` to send, `Err(reason)` to refuse.
pub type Preflight = std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// Blocking Chat Completions transport. Moved out of the bin in D2-u1 so
/// the product worker and the acceptance runner speak the same wire path.
pub struct HttpTransport {
    url: String,
    max_output_tokens: u64,
    /// Called before EVERY request this transport sends (first requests,
    /// tool-loop steps, retries, resumes after an approval): `Err(text)`
    /// refuses the request, which is then never sent and fails the turn with
    /// `text`. The app uses it to keep Awili from making a model host load a
    /// model on demand.
    preflight: Option<Preflight>,
    /// Bearer credential bound to THIS endpoint only. `None`/empty = send no
    /// Authorization header. Never logged, including in error paths.
    api_key: Option<String>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    client: reqwest::blocking::Client,
}

/// The URL [`HttpTransport`] posts to for `base_url` (the server root; the
/// Chat Completions path is appended). RC1-F4b-FIX1: the app classifies the
/// in-app engine's locality from THIS string, parsed by the same URL type the
/// HTTP client uses, so both always agree on the host.
pub fn chat_completions_url(base_url: &str) -> String {
    format!("{}/v1/chat/completions", base_url.trim_end_matches('/'))
}

/// RC1-F-SLICE-FIX1b: the most redirects one engine request follows.
pub const MAX_ENGINE_REDIRECTS: usize = 10;

/// RC1-F-SLICE-FIX1b (slice review P2 on ad1907ca): the engine's redirect
/// rule. The app books an in-app turn's tokens by the host of
/// [`chat_completions_url`]; a redirect to ANOTHER host would send the
/// conversation somewhere else under that booking, so it is refused with an
/// error (the turn fails visibly; the other host is never contacted). A
/// redirect that stays on the same host (scheme, port or path may change) is
/// followed, up to [`MAX_ENGINE_REDIRECTS`].
pub fn engine_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let first_host = attempt
            .previous()
            .first()
            .and_then(|u| u.host_str().map(str::to_owned));
        if attempt.url().host_str().map(str::to_owned) != first_host {
            attempt.error("the model server redirected to another host; Awili does not follow it")
        } else if attempt.previous().len() > MAX_ENGINE_REDIRECTS {
            attempt.error("too many redirects from the model server")
        } else {
            attempt.follow()
        }
    })
}

impl HttpTransport {
    /// `base_url` is the server root; the Chat Completions path is appended.
    pub fn new(base_url: &str, timeout: std::time::Duration) -> Self {
        Self::with_api_key(base_url, timeout, None)
    }

    /// Like [`new`](Self::new), carrying a bearer credential from
    /// `[engine].api_key` (resolved against `AWILI_ENGINE_KEY` by the app, at
    /// `ProviderKeys::from_env_and_config` — never read here).
    pub fn with_api_key(
        base_url: &str,
        timeout: std::time::Duration,
        api_key: Option<String>,
    ) -> Self {
        Self {
            url: chat_completions_url(base_url),
            max_output_tokens: REPLY_ROOM_TOKENS,
            api_key,
            preflight: None,
            cancel: None,
            client: reqwest::blocking::Client::builder()
                .timeout(timeout)
                .redirect(engine_redirect_policy())
                .build()
                .expect("reqwest client build"),
        }
    }

    /// Use the model's saved generation budget for each answer/tool round.
    pub fn with_max_output_tokens(mut self, tokens: u64) -> Self {
        self.max_output_tokens = tokens;
        self
    }

    /// Gate every request through `preflight` (see the field).
    pub fn with_preflight(mut self, preflight: Preflight) -> Self {
        self.preflight = Some(preflight);
        self
    }

    /// Build the POST request, attaching `Authorization: Bearer` **only** when
    /// a non-empty credential is present (a self-hosted box may need none) —
    /// mirrors `build_openai_request` in `awili-providers`. Extracted from
    /// `stream` so the auth-header rule is unit-testable without a socket.
    fn post_request(&self, request_body: &str) -> reqwest::blocking::RequestBuilder {
        let mut rb = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json");
        if let Some(key) = self
            .api_key
            .as_deref()
            .filter(|k| !k.is_empty() && !k.trim_start().starts_with("secret:"))
        {
            rb = rb.header("Authorization", format!("Bearer {key}"));
        }
        rb.body(request_body.to_string())
    }
}

impl Transport for HttpTransport {
    fn set_cancel_flag(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) { self.cancel = Some(flag); }
    fn max_output_tokens(&self) -> u64 { self.max_output_tokens }
    fn stream(&mut self, request_body: &str) -> Result<String, SeamError> {
        self.stream_lines(request_body, &mut |_| true)
    }

    /// RC2-GEMMA3 (R7-speed): the body is read line by line as the server
    /// sends it. The client's timeout now bounds each wait for the next
    /// bytes (and for the response to start), never the whole reply: a
    /// long answer, or a server still reading a long request (it sends a
    /// keepalive every 2048 tokens), is never cut off while it is alive.
    fn stream_lines(
        &mut self,
        request_body: &str,
        on_line: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, SeamError> {
        if let Some(preflight) = &self.preflight {
            preflight().map_err(SeamError::Transport)?;
        }
        if self
            .api_key
            .as_deref()
            .is_some_and(|k| k.trim_start().starts_with("secret:"))
        {
            return Err(SeamError::Transport(
                "the API key is an unresolved secret: reference; enter it again in Settings"
                    .to_string(),
            ));
        }
        let resp = self.post_request(request_body).send().map_err(|e| {
            use std::error::Error as _;
            let mut msg = e.to_string();
            let mut src = e.source();
            while let Some(inner) = src {
                msg.push_str(&format!(": {inner}"));
                src = inner.source();
            }
            SeamError::Transport(msg)
        })?;
        let status = resp.status();
        let body = bounded_stream_body(resp, status.is_success(), on_line, || {
            self.cancel.as_ref().is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
        })?;
        // Non-success WITH a body flows through decode_stream, which
        // classifies canonical JSON error bodies as ProviderError — the
        // audited path. An empty failure body is a transport error.
        if !status.is_success() && body.trim().is_empty() {
            return Err(SeamError::Transport(format!(
                "HTTP {status} with empty body"
            )));
        }
        Ok(body)
    }
}

// Read bounded chunks so cancellation is observed even before a line ends.
fn bounded_stream_body(mut reader: impl std::io::Read, successful: bool, on_line: &mut dyn FnMut(&str) -> bool, cancelled: impl Fn() -> bool) -> Result<String, SeamError> {
    const LINE_LIMIT: usize = 1024 * 1024;
    const BODY_LIMIT: usize = 16 * 1024 * 1024;
    let mut body = String::new();
    let mut line = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if cancelled() { return Err(SeamError::Cancelled(StopCause::Cancelled)); }
        let n = reader.read(&mut chunk).map_err(|e| SeamError::Transport(e.to_string()))?;
        if cancelled() { return Err(SeamError::Cancelled(StopCause::Cancelled)); }
        if n == 0 { break; }
        for part in chunk[..n].split_inclusive(|b| *b == b'\n') {
            if line.len().saturating_add(part.len()) > LINE_LIMIT || body.len().saturating_add(line.len()).saturating_add(part.len()) > BODY_LIMIT {
                return Err(SeamError::Transport("model response exceeds size limit".into()));
            }
            line.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                let text = std::str::from_utf8(&line).map_err(|_| SeamError::Transport("model response is not text".into()))?;
                body.push_str(text);
                let bare = text.trim_end_matches('\n').trim_end_matches('\r');
                if successful && !on_line(bare) { return Ok(body); }
                line.clear();
            }
        }
    }
    if !line.is_empty() {
        let text = std::str::from_utf8(&line).map_err(|_| SeamError::Transport("model response is not text".into()))?;
        body.push_str(text);
        if successful { on_line(text.trim_end_matches('\r')); }
    }
    Ok(body)
}

// -- the host loop ----------------------------------------------------------

/// Run one turn end to end. See the module docs for what this is and is
/// not. `session_root` is the C3 scope root; `log`/`grant_store` are the
/// caller's B2 stores (MemLog in tests, a file log in the binary).
///
/// EVERY exit is audited (round-1 P0): success and `TurnFailed` close
/// the turn inside the loop; any other error path best-effort closes it
/// here (`turn_closed("failed")`) before returning — a B2 failure while
/// closing cannot mask the original error, and replay stays truthful
/// about whatever was durably written.
#[allow(clippy::too_many_arguments)]
pub fn run_turn<S1>(
    host: &HostAuthority,
    transport: &mut dyn Transport,
    model: &str,
    session_root: &str,
    user_msg: &str,
    host_io: &mut dyn TurnHost,
    log: &mut ExecutionLog<S1>,
    grant_store: &mut dyn awili_engine_log::grant_service::GrantStoreAccess,
    extra_tools: &[awili_tools::ToolDef],
    // The executor seam (unit 4 item 6): origin-routed dispatch through
    // the closed executor vocabulary. The composition owns this —
    // `builtin_only()` for the unit-4 worker and acceptance runner (live
    // MCP wiring is 4b), `Executors::mcp(facade)` where a coordinator
    // facade exists.
    executors: &Executors,
    // Unit 5 §5: the per-call install authority query (the app implements
    // it over current config/setup/coordinator state; NoInstallAuthority
    // everywhere else). Queried by execute_one immediately before each
    // authorization.
    install_authority: &dyn awili_engine_authz::InstallAuthorityQuery,
    // The host-controlled accepted-roots records (unit 4 item 6): the
    // ONLY source of the epoch an MCP dispatch carries — never the
    // advertised proposal. Bring-up mutates it composition-side; the
    // turn only reads it.
    roots: &RootsAcceptance,
    window_tokens: u64,
    // Identifies this turn in the B2 log. One worker session runs many
    // turns, so the caller supplies it (`turn-seam-0` was the C2-u5
    // single-turn harness constant).
    turn_id: &str,
    // C4: the SESSION-owned context history — turns append to it, so a later
    // turn assembles over earlier turns' groups (which is where compaction
    // becomes reachable: the active suffix — the newest user message onward —
    // is never summarized, so a single turn alone can only CannotFit).
    history: &mut ContextHistory,
) -> Result<TurnReport, SeamError>
where
    S1: AppendOnlyLog,
{
    // Closure tracking (round-2 P1): close_and_report closes the turn
    // BEFORE building the report, so a report-construction failure must
    // not trigger a second, non-idempotent turn_closed here.
    let mut closed = false;
    // C4 round-1 P1: commit-on-success. A failed turn must leave the SESSION
    // history exactly as it found it — the failed-turn contract of
    // `ingest_turn_events`, honored here for the incremental path too. The
    // checkpoint restores on EVERY error (transport, context, storage,
    // TurnFailed); a successful compaction inside a failed turn is also
    // rolled back (the next turn will re-plan deterministically).
    // RC2-GEMMA1: the turn's memory context (the recalled `<past-context>`
    // block the caller set on the history, see [`split_past_context`]) opens
    // its user message for every request of the turn, so a tool round still
    // extends the prompt the server cached; `user_group` names that message
    // so the context is dropped from it again at the turn's end and never
    // piles up turn after turn.
    let turn_context = history.take_turn_context();
    let checkpoint = history.clone();
    let turn_text = match turn_context
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        Some(context) => format!("{context}\n\n{user_msg}"),
        None => user_msg.to_string(),
    };
    let mut user_group: Option<GroupId> = None;
    let result = run_turn_inner(
        host,
        transport,
        model,
        session_root,
        &turn_text,
        &mut user_group,
        host_io,
        log,
        grant_store,
        extra_tools,
        executors,
        install_authority,
        roots,
        window_tokens,
        turn_id,
        history,
        &mut closed,
    );
    match result {
        Err(e) => {
            // RC1-N-FIX1/2/3 (whole-slice reviews P1/P2): whatever ends the
            // turn — the owner's Deny, a Stop, a shutdown, an overflow, a
            // provider or stream error, a storage or harness error — the
            // history keeps this turn's completed work; only the unfinished
            // tail is dropped. "Completed" is decided by the durable log
            // alone: a call ran only when its dispatch was finalized (a
            // refused batch's exchange is not work). With nothing finalized
            // the history is restored as it was.
            let finalized: Option<Vec<FinalizedCall>> = log.read_records().ok().map(|(r, _)| {
                finalized_calls(&r)
                    .into_iter()
                    .filter(|f| f.turn_id == turn_id)
                    .collect()
            });
            match finalized {
                Some(f) if f.is_empty() => *history = checkpoint,
                Some(f) => {
                    // Every finalized call is in the kept history; its stamp
                    // (the checkpoint recovery reads) is then exact.
                    place_finalized_calls(history, &f);
                    history.set_log_turn(turn_id);
                }
                // The log cannot be read: keep what the turn holds, unstamped
                // (a later recovery then looks at this turn's log too).
                None => {}
            }
            drop_turn_context(history, user_group, &turn_text, user_msg);
            if !closed {
                // A host stop is audited as such, not as a failure.
                let outcome = match &e {
                    SeamError::Cancelled(_) => "cancelled",
                    _ => "failed",
                };
                let closed_ok = log.turn_closed(turn_id, outcome);
                // RC1-N (pre-review P2): a Deny is reported as done only
                // with its turn durably closed. RC1-N-FIX4 (pre-review P1):
                // so is every ending the session survives — a failed log
                // write after a tool may have run ends the worker (a stop
                // that ends it anyway is reported as it was).
                let worker_ends = matches!(&e, SeamError::Cancelled(c) if *c != StopCause::Denied);
                if let (false, Err(le)) = (worker_ends, closed_ok) {
                    return Err(SeamError::Storage(le));
                }
            }
            Err(e)
        }
        ok => {
            if ok.is_ok() {
                history.set_log_turn(turn_id);
            }
            drop_turn_context(history, user_group, &turn_text, user_msg);
            ok
        }
    }
}

/// RC2-GEMMA1: once a turn is over, its user message in the kept history is
/// the user's words alone (the memory context it opened with is dropped).
/// A restored history no longer holds the group, and then nothing changes.
fn drop_turn_context(
    history: &mut ContextHistory,
    user_group: Option<GroupId>,
    turn_text: &str,
    user_msg: &str,
) {
    if let Some(id) = user_group.filter(|_| turn_text != user_msg) {
        let still_ours = matches!(
            history.get(id),
            Some(ContextGroup::User { text, .. }) if text == turn_text
        );
        if still_ours {
            history.set_user_text(id, user_msg);
        }
    }
}

/// RC2-GEMMA1: how Awili's memory preamble opens its recalled context
/// (`awili_memory::try_system_preamble`: the fence line, then this header).
pub const PAST_CONTEXT_HEAD: &str = "<past-context>\nRelevant context from your own memory";

/// RC2-GEMMA1: the closing fence line of that block.
const PAST_CONTEXT_END: &str = "</past-context>";

/// RC2-GEMMA1 (R7-speed): split the recalled `<past-context>` block out of
/// an in-app system text. Returns the system text without it and the block.
/// The recall changes with every message; left in the system message it
/// made the model server read the whole prompt (instructions and tool
/// descriptions) again on every turn (measured on the Studio: 29 s per turn
/// where an unchanged system part answers from the server's cache). The
/// block is found only where the preamble puts it: at the start of a line,
/// opened by [`PAST_CONTEXT_HEAD`], closed by its own fence line. The text
/// around it is joined by one blank line, exactly as when nothing was
/// recalled, so the system text is the same bytes either way.
pub fn split_past_context(system: &str) -> (String, Option<String>) {
    let start = if system.starts_with(PAST_CONTEXT_HEAD) {
        Some(0)
    } else {
        system
            .find(&format!("\n{PAST_CONTEXT_HEAD}"))
            .map(|i| i + 1)
    };
    let Some(start) = start else {
        return (system.to_string(), None);
    };
    let close = format!("\n{PAST_CONTEXT_END}");
    let Some(end) = system[start..]
        .find(&close)
        .map(|i| start + i + close.len())
    else {
        return (system.to_string(), None);
    };
    let ends_line = system[end..].is_empty() || system[end..].starts_with('\n');
    if !ends_line {
        return (system.to_string(), None);
    }
    let block = system[start..end].to_string();
    let before = system[..start].trim_end();
    let after = system[end..].trim_start();
    let rest = match (before.is_empty(), after.is_empty()) {
        (true, _) => after.to_string(),
        (false, true) => before.to_string(),
        (false, false) => format!("{before}\n\n{after}"),
    };
    (rest, Some(block))
}

/// RC2-ENG5 (T26): the acceptance runner's `--expect FILE` — a relative path
/// that stays inside `--dir`. `..` and an absolute path are refused.
/// RC2-ENGG1: the file is found and read in one step, through descriptors
/// anchored to `--dir` (the sandbox's no-follow walk): a link anywhere on
/// the way — including a folder swapped for one while the check runs — is
/// refused, so the check can never read outside the directory the run was
/// given. Returns the file's text.
pub fn expect_file_in_dir(dir: &std::path::Path, file: &str) -> Result<String, String> {
    read_expect_file(dir, file, || {})
}

/// [`expect_file_in_dir`], with `during` run after `--dir` is anchored and
/// before the file is looked up (tests swap a folder there).
pub(crate) fn read_expect_file(
    dir: &std::path::Path,
    file: &str,
    during: impl FnOnce(),
) -> Result<String, String> {
    use std::path::Component;
    let rel = std::path::Path::new(file);
    let plain = rel
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if file.is_empty() || !plain {
        return Err(format!(
            "--expect {file:?} must be a relative path inside --dir"
        ));
    }
    let real_dir = std::fs::canonicalize(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let folder = awili_engine_sandbox::Sandbox::open_root(&real_dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    during();
    let bytes = folder
        .read(file)
        .map_err(|e| format!("--expect {file:?}: {e}"))?;
    String::from_utf8(bytes).map_err(|_| format!("--expect {file:?} is not text"))
}

/// RC1-N: whether a model server's error says the request was longer than
/// its context (llama.cpp, vLLM, OpenAI-style and MLX wordings).
pub fn context_overflow_text(m: &str) -> bool {
    let m = m.to_ascii_lowercase();
    m.contains("context_length_exceeded")
        || m.contains("cannot fit")
        || m.contains("too many tokens")
        || m.contains("prompt is too long")
        || (m.contains("context")
            && [
                "exceed",
                "too long",
                "too large",
                "maximum",
                "overflow",
                "longer than",
            ]
            .iter()
            .any(|w| m.contains(w)))
}

/// RC1-N: whether a model server's answer is a provider error saying the
/// request was longer than its context.
fn provider_context_refusal(raw: &str) -> bool {
    decode_stream(raw).iter().any(|d| {
        matches!(d, awili_engine::wire::WireDelta::ProviderError { message, .. }
            if context_overflow_text(message))
    })
}

/// RC1-N-FIX3: how a step's result is shown, at most, when it is placed in
/// a kept history or a recovery note after the turn ended.
const STEP_RESULT_MAX_CHARS: usize = 1_500;

/// RC1-N-FIX3: one dispatch the durable log shows finalized — the only
/// evidence that a call ran (a refused batch, a denial and an uncertain
/// dispatch have no `dispatch_finalized`).
#[derive(Debug, Clone, PartialEq)]
pub struct FinalizedCall {
    pub turn_id: String,
    pub call: ToolCall,
    /// The finalized result (or error) text.
    pub content: String,
    /// `dispatch_finalized`'s `ok`: the call ran and succeeded. Not OK: it
    /// failed, or never reached its tool (every MCP call refused or
    /// cancelled before dispatch is finalized as an error).
    pub ok: bool,
}

/// RC1-N-FIX3: every finalized dispatch in `records`, in the order the model
/// asked for the calls (their `tool_call` events; a finalized call without
/// one follows, in intent order).
pub fn finalized_calls(records: &[awili_engine_log::schema::LogRecordV1]) -> Vec<FinalizedCall> {
    use awili_engine_log::schema::{CanonicalEventV1, LogRecordV1};
    // intent id -> (turn, call id, tool, raw arguments)
    let mut intents: Vec<(String, String, String, String)> = Vec::new();
    let mut by_intent: std::collections::HashMap<&str, usize> = Default::default();
    for r in records {
        if let LogRecordV1::Intent {
            turn_id,
            intent_id,
            awili_call_id,
            tool,
            raw_arguments,
            ..
        } = r
        {
            by_intent.insert(intent_id.as_str(), intents.len());
            intents.push((
                turn_id.clone(),
                awili_call_id.clone(),
                tool.clone(),
                raw_arguments.clone(),
            ));
        }
    }
    let mut done: std::collections::HashMap<String, (String, bool)> = Default::default();
    for r in records {
        if let LogRecordV1::DispatchFinalized {
            intent_id,
            ok,
            content,
        } = r
        {
            if let Some(&i) = by_intent.get(intent_id.as_str()) {
                done.insert(intents[i].1.clone(), (content.clone(), *ok));
            }
        }
    }
    let mut out: Vec<FinalizedCall> = Vec::new();
    let mut listed: std::collections::HashSet<String> = Default::default();
    for r in records {
        let LogRecordV1::EventAppended { turn_id, event, .. } = r else {
            continue;
        };
        if let CanonicalEventV1::ToolCallReady {
            awili_call_id,
            name,
            arguments,
            provider_call_id,
        } = event
        {
            if let Some((content, ok)) = done.get(awili_call_id) {
                if listed.insert(awili_call_id.clone()) {
                    out.push(FinalizedCall {
                        turn_id: turn_id.clone(),
                        call: ToolCall {
                            awili_call_id: awili_engine::AwiliCallId(awili_call_id.clone()),
                            provider_call_id: provider_call_id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        },
                        content: content.clone(),
                        ok: *ok,
                    });
                }
            }
        }
    }
    for (turn_id, id, tool, args) in &intents {
        if let Some((content, ok)) = done.get(id) {
            if listed.insert(id.clone()) {
                out.push(FinalizedCall {
                    turn_id: turn_id.clone(),
                    call: ToolCall {
                        awili_call_id: awili_engine::AwiliCallId(id.clone()),
                        provider_call_id: None,
                        name: tool.clone(),
                        arguments: args.clone(),
                    },
                    content: content.clone(),
                    ok: *ok,
                });
            }
        }
    }
    out
}

/// RC1-N-FIX3: a step's result text, cut to [`STEP_RESULT_MAX_CHARS`].
fn step_result_text(content: &str) -> String {
    let total = content.chars().count();
    let shown: String = content.chars().take(STEP_RESULT_MAX_CHARS).collect();
    if total > STEP_RESULT_MAX_CHARS {
        format!("{shown} [{STEP_RESULT_MAX_CHARS} of {total} characters shown]")
    } else {
        shown
    }
}

/// RC1-N-FIX3: a kept turn's history holds every call the turn finalized.
/// A finalized call the turn's exchanges lack — an MCP result that arrived
/// together with a Stop, a call finalized just before a storage error — is
/// added as one more exchange with its finalized result, so the history's
/// log-turn stamp is exact: everything the turn ran is in it.
fn place_finalized_calls(history: &mut ContextHistory, finalized: &[FinalizedCall]) {
    let groups = history.groups();
    let last_user = groups
        .iter()
        .rposition(|g| matches!(g, ContextGroup::User { .. }))
        .unwrap_or(0);
    let held: std::collections::HashSet<&str> = groups[last_user..]
        .iter()
        .flat_map(|g| match g {
            ContextGroup::Exchange { calls, .. } => {
                calls.iter().map(|c| c.awili_call_id.0.as_str()).collect()
            }
            _ => Vec::new(),
        })
        .collect();
    let missing: Vec<&FinalizedCall> = finalized
        .iter()
        .filter(|f| !held.contains(f.call.awili_call_id.0.as_str()))
        .collect();
    if missing.is_empty() {
        return;
    }
    let calls: Vec<ToolCall> = missing.iter().map(|f| f.call.clone()).collect();
    let results: Vec<ToolResultRecord> = missing
        .iter()
        .map(|f| ToolResultRecord {
            awili_call_id: f.call.awili_call_id.clone(),
            content: step_result_text(&f.content),
            is_error: !f.ok,
        })
        .collect();
    if let Err(e) = history.push_exchange("", calls, results) {
        eprintln!("awili-seam: a finalized call could not be placed in the history: {e:?}");
    }
}

/// RC1-N: a completed batch's results, fitted to what the window can still
/// hold once the exchange is in the history (see
/// [`awili_engine::context::fit_tool_results`]).
fn fit_exchange(
    history: &ContextHistory,
    profile: &ModelContextProfile,
    assistant_text: &str,
    calls: &[ToolCall],
    results: Vec<ToolResultRecord>,
) -> Vec<ToolResultRecord> {
    let budget = awili_engine::context::exchange_result_budget(
        history,
        profile,
        &seam_counter(),
        assistant_text,
        calls,
        results.len(),
    );
    awili_engine::context::fit_tool_results(results, budget, &seam_counter()).0
}

#[allow(clippy::too_many_arguments)]
fn run_turn_inner<S1>(
    host: &HostAuthority,
    transport: &mut dyn Transport,
    model: &str,
    session_root: &str,
    user_msg: &str,
    // RC2-GEMMA1: set to the id of the user message this turn pushes.
    user_group: &mut Option<GroupId>,
    host_io: &mut dyn TurnHost,
    log: &mut ExecutionLog<S1>,
    grant_store: &mut dyn awili_engine_log::grant_service::GrantStoreAccess,
    // Additional (non-builtin) tool defs, modelling a future MCP
    // advertisement: registered through the PUBLIC registry path, so
    // reserved origins are refused, and bare-name ambiguity becomes
    // reachable (design blocker 3's ambiguous case).
    extra_tools: &[awili_tools::ToolDef],
    executors: &Executors,
    install_authority: &dyn awili_engine_authz::InstallAuthorityQuery,
    roots: &RootsAcceptance,
    window_tokens: u64,
    turn_id: &str,
    history: &mut ContextHistory,
    closed: &mut bool,
) -> Result<TurnReport, SeamError>
where
    S1: AppendOnlyLog,
{
    let projection_window = projection_window_tokens(window_tokens, transport.max_output_tokens());
    let session_id = log.session_id().to_string();
    let mut registry = tool_registry(extra_tools)?;
    // Unit 4 item 6 note: a non-builtin tool registered into a composition
    // with no MCP executor is refused at DISPATCH (`Executors::route`), not
    // here. Registering-and-advertising such a tool is legitimate on its own
    // — the model may never call it — and failing the turn at open would
    // regress that path (see
    // `extra_tools_are_advertised_to_the_model_not_just_registered`). The
    // no-silent-fallback guarantee lives where the fallback would occur.
    log.turn_opened(turn_id, "chat-completions", model)?;
    let mut state = TurnState::new("chat-completions", model, turn_id);
    *user_group = Some(history.push_user(user_msg));
    let mut trace: Vec<AgentEvent> = Vec::new();
    let mut blob_cursor = 0usize;
    let mut requests: Vec<String> = Vec::new();
    // Result ids already fed to the reducer — a second emission for the
    // same awili id is structurally impossible (design blocker 5).
    let mut emitted: BTreeSet<String> = BTreeSet::new();

    // The full-schema projection is left untouched when it fits. Otherwise
    // the registry remains complete while each request advertises the
    // turn-local selected projection.
    let fallback = needs_catalog_fallback(&registry, extra_tools, model, projection_window, history)?;
    let mut catalog = if fallback {
        awili_tools::catalog::install(&mut registry)
            .map_err(|e| SeamError::Harness(format!("catalog install: {e:?}")))?;
        Some(catalog_with_local_shell(&registry, roots, history, model, projection_window)?)
    } else {
        None
    };

    let mut rounds = 0usize;
    let mut retried_cut = false;
    loop {
        rounds += 1;
        if rounds > MAX_ROUNDS {
            let report = close_and_report(
                log, turn_id, "failed", trace, requests, &state, rounds, closed,
            )?;
            return Err(SeamError::TurnFailed {
                reason: format!("round bound ({MAX_ROUNDS}) exceeded"),
                report: Box::new(report),
            });
        }
        let tools = if let Some(turn) = catalog.as_ref() {
            validate_catalog_turn(&registry, roots, turn)?;
            catalog_advertised_tools(&registry, &turn.selected, projection_window, model)?
        } else {
            advertised_tools(&registry, extra_tools, projection_window, history, model)?
        };
        let mut profile = seam_profile(
            model,
            window_tokens,
            &serde_json::Value::Array(tools.clone()).to_string(),
        );
        profile.reserved_output_tokens = transport.max_output_tokens();
        // In catalog mode all historical aliases keep their allocation even
        // while only the selected schemas are projected in this request.
        let wire = if catalog.is_some() {
            let aliases = registry.aliases().into_values().collect::<Vec<_>>();
            wire_names::WireNames::new(aliases.iter().map(String::as_str))
        } else {
            wire_names::WireNames::for_tools(&tools)
        };
        // Initial request carries tool_choice/max_tokens; CONTINUATIONS
        // mirror the pinned fixture shape exactly (round-1 P1:
        // 04-tool-continuation.request.json has model/stream/
        // stream_options/tools/messages and nothing else).
        // Before every transport request (D2-u1 P0-1).
        if host_io.cancelled() {
            return Err(SeamError::Cancelled(StopCause::Cancelled));
        }
        // RC1-N (pre-review P1): Awili's token count is a heuristic. When
        // the model server still refuses the request as longer than its
        // context, this turn's results shrink by half and the request is
        // sent again, as long as they can shrink.
        // Text emitted THIS round only starts here (the live path below feeds
        // text while the body arrives).
        let text_start = state.text().len();
        // RC2-GEMMA4 (R7-A3): where this round started, to ask again once
        // when the server cuts a text-only reply off.
        let round_state = state.clone();
        let round_trace = trace.len();
        let (raw, live) = loop {
            let messages =
                context_messages(history, &profile, transport, model, &mut requests, host_io)?;
            let mut body = if rounds == 1 {
                serde_json::json!({
                    "model": model,
                    "stream": true,
                    "stream_options": {"include_usage": true},
                    "max_tokens": transport.max_output_tokens(),
                    "tool_choice": "auto",
                    "tools": tools,
                    "messages": messages,
                })
            } else {
                // RC2-GEMMA6 (a8): continuations carry `max_tokens` too — without
                // it the server's own default applies (mlx_lm on the Studio:
                // 1024), and a long answer after a tool call was cut at 1024.
                serde_json::json!({
                    "model": model,
                    "stream": true,
                    "stream_options": {"include_usage": true},
                    "max_tokens": transport.max_output_tokens(),
                    "tools": tools,
                    "messages": messages,
                })
            };
            wire.encode_body(&mut body);
            let body = body.to_string();
            requests.push(body.clone());
            if host_io.cancelled() {
                return Err(SeamError::Cancelled(StopCause::Cancelled));
            }
            // RC2-GEMMA3 (R7-speed): the reply is fed to the reducer line by
            // line as it arrives, so its text shows while the model writes
            // it (and a Stop ends the read at once); the server's keepalives
            // while it reads the request become progress.
            let mut live = LiveFeed::new();
            let answer = {
                let mut on_line = |line: &str| -> bool {
                    let Some(deltas) = live.take(line, &wire, host_io) else {
                        return false;
                    };
                    for delta in deltas {
                        let fed = feed_one(
                            &mut state,
                            delta,
                            log,
                            turn_id,
                            &mut trace,
                            host_io,
                            &mut live.decided,
                        );
                        if let Err(e) = fed {
                            live.error = Some(e);
                            return false;
                        }
                    }
                    true
                };
                transport.stream_lines(&body, &mut on_line)
            };
            if let Some(e) = live.error.take() {
                return Err(e);
            }
            if live.cancelled {
                return Err(SeamError::Cancelled(StopCause::Cancelled));
            }
            // Pre-review P1: the refusal arrives either as a transport error
            // or as an HTTP error body the codec decodes to a provider error.
            // A reply already partly shown is never sent again.
            let refused = live.fed.is_empty()
                && match &answer {
                    Err(SeamError::Transport(m)) => context_overflow_text(m),
                    Ok(raw) => provider_context_refusal(raw),
                    Err(_) => false,
                };
            // Each resend halves this turn's newest results that can still
            // shrink; it ends when only markers are left (the shrink makes
            // no progress), so it always terminates.
            if refused && history.shrink_latest_results(&seam_counter()) {
                eprintln!("awili-seam: the model refused the request as too long; results halved");
                if host_io.cancelled() {
                    return Err(SeamError::Cancelled(StopCause::Cancelled));
                }
                continue;
            }
            // RC2-GEMMA4 (R7-A3): a text-only reply the server cut off (the
            // body ended without its end marker; nothing was cancelled and
            // no tool call had started) is asked for again, once per turn —
            // nothing ran, and the request is the same, so the server answers
            // it from its cache. The text it had shown is taken back.
            if let Ok(raw) = &answer {
                if !retried_cut && !host_io.cancelled() && cut_text_reply(raw) {
                    retried_cut = true;
                    let shown = state.text()[text_start..].to_string();
                    state = round_state.clone();
                    trace.truncate(round_trace);
                    eprintln!("awili-seam: the model server cut the reply off; asking again");
                    host_io.reply_cut(&shown);
                    if host_io.cancelled() {
                        return Err(SeamError::Cancelled(StopCause::Cancelled));
                    }
                    continue;
                }
            }
            break (answer?, live);
        };
        // ...and again once it returns (round-2 P0). The HTTP call itself is
        // not abortable here, so a cancel that lands DURING the request is
        // observed at exactly this point — before the response is reduced.
        // Without this, a cancelled turn whose response happened to be
        // text-only would still report a completed turn.
        if host_io.cancelled() {
            return Err(SeamError::Cancelled(StopCause::Cancelled));
        }

        // RC2-GEMMA3: the whole body decodes to what was fed live, then the
        // rest (the end, an error, a truncation, or everything when nothing
        // went live).
        let all = wire.decode(decode_stream(&raw));
        if !all.starts_with(&live.fed) {
            return Err(SeamError::Harness(
                "the reply read line by line differs from the whole reply".into(),
            ));
        }
        let rest = all[live.fed.len()..].to_vec();
        let action = match (
            live.decided,
            feed_deltas(&mut state, rest, log, turn_id, &mut trace, host_io)?,
        ) {
            (None, action) => action,
            (Some(action), NextAction::None) => action,
            (Some(_), _) => {
                return Err(SeamError::Harness(
                    "two terminal actions in one stream".into(),
                ));
            }
        };

        match action {
            NextAction::DispatchTools(calls) => {
                // Text emitted THIS round only (round-1 P1: state.text()
                // is cumulative; a later round must not repeat earlier
                // rounds' text in its assistant message).
                let round_text = state.text()[text_start..].to_string();
                // Batch protocol validation BEFORE any side effect
                // (design blocker 4).
                validate_batch(&calls).map_err(|reason| {
                    // Terminate + audit with zero dispatched.
                    match close_and_report(
                        log,
                        turn_id,
                        "failed",
                        std::mem::take(&mut trace),
                        std::mem::take(&mut requests),
                        &state,
                        rounds,
                        closed,
                    ) {
                        Ok(report) => SeamError::TurnFailed {
                            reason,
                            report: Box::new(report),
                        },
                        Err(e) => e,
                    }
                })?;
                let assistant_text = round_text;

                // In fallback mode, only aliases in this exact projection
                // are callable. A catalog request is a singleton metadata
                // operation so it can be trial-fitted and committed as one
                // transaction before any other tool could run.
                if let Some(turn) = catalog.as_ref() {
                    let active_aliases: BTreeSet<String> = registry
                        .snapshot()
                        .into_iter()
                        .filter(|d| {
                            d.identity.origin.origin
                                == awili_engine_authz::policy::BUILTIN_FS_ORIGIN
                                || d.identity.origin.origin == awili_tools::catalog::origin().origin
                                || turn.selected.iter().any(|s| s.identity == d.identity)
                        })
                        .filter_map(|d| {
                            registry
                                .aliases()
                                .get(&(d.identity.origin.origin.clone(), d.identity.name.clone()))
                                .cloned()
                        })
                        .collect();
                    let inactive = calls.iter().any(|c| !active_aliases.contains(&c.name));
                    let catalog_call = calls.iter().any(|c| {
                        registry.lookup_unique(&c.name).is_ok_and(|d| {
                            d.identity.origin.origin == awili_tools::catalog::origin().origin
                        })
                    });
                    if inactive || (catalog_call && calls.len() != 1) {
                        let content = if inactive {
                            "One or more tools in this batch are not enabled in this request; no batch member ran. Call tools_find alone to search the catalog, then call tools_find alone with select containing exact returned aliases. Only after selection, retry the tool call. This projection error does not establish that the capability is unavailable in Awili.".to_string()
                        } else {
                            "tools.find must be called alone; no batch member ran".to_string()
                        };
                        let mut results = Vec::new();
                        let mut last_action = NextAction::None;
                        for call in &calls {
                            let record = ToolResultRecord {
                                awili_call_id: call.awili_call_id.clone(),
                                content: content.clone(),
                                is_error: true,
                            };
                            if !emitted.insert(record.awili_call_id.0.clone()) {
                                return Err(SeamError::Harness(
                                    "duplicate inactive tool result".into(),
                                ));
                            }
                            last_action = step(
                                &mut state,
                                Input::ToolResult(record.clone()),
                                log,
                                turn_id,
                                &mut trace,
                                host_io,
                            )?;
                            results.push(record);
                        }
                        let fitted =
                            fit_exchange(history, &profile, &assistant_text, &calls, results);
                        history
                            .push_exchange(assistant_text, calls, fitted)
                            .map_err(|e| SeamError::Context(e.to_string()))?;
                        push_new_blobs(history, &trace, &mut blob_cursor);
                        match last_action {
                            NextAction::RequestContinuation => continue,
                            NextAction::Done => {
                                return finish(
                                    log, turn_id, trace, requests, state, rounds, closed,
                                );
                            }
                            other => {
                                return Err(SeamError::Harness(format!(
                                    "unexpected post-results action: {other:?}"
                                )));
                            }
                        }
                    }
                }

                // RC1-N (pre-review P1): a batch whose own calls leave no
                // room for its results never runs — nothing it produced
                // could reach the model.
                if !awili_engine::context::batch_has_room(
                    history,
                    &profile,
                    &seam_counter(),
                    &assistant_text,
                    &calls,
                ) {
                    return Err(SeamError::Context(
                        "cannot fit: the tool calls leave no room for their results".into(),
                    ));
                }
                // Pre-resolve EVERY member before any side effect
                // (round-1 P0: a batch [valid fs.write, unknown tool]
                // must not touch disk before discovering the invalid
                // member). Any resolution failure poisons the WHOLE
                // batch: every member gets a synthetic error result —
                // zero dialogs, zero intents, zero filesystem effects —
                // and the model is told.
                let unresolved: Vec<String> = calls
                    .iter()
                    .filter(|c| registry.lookup_unique(&c.name).is_err())
                    .map(|c| c.name.clone())
                    .collect();
                if !unresolved.is_empty() {
                    let mut results: Vec<ToolResultRecord> = Vec::new();
                    let mut last_action = NextAction::None;
                    for call in &calls {
                        let record = ToolResultRecord {
                            awili_call_id: call.awili_call_id.clone(),
                            content: unresolved_text(&unresolved),
                            is_error: true,
                        };
                        if !emitted.insert(record.awili_call_id.0.clone()) {
                            return Err(SeamError::Harness(format!(
                                "duplicate result emission for {}",
                                record.awili_call_id.0
                            )));
                        }
                        results.push(record.clone());
                        last_action = step(
                            &mut state,
                            Input::ToolResult(record),
                            log,
                            turn_id,
                            &mut trace,
                            host_io,
                        )?;
                    }
                    // RC1-N (pre-review P2): every completed batch is fitted.
                    let results = fit_exchange(history, &profile, &assistant_text, &calls, results);
                    history
                        .push_exchange(assistant_text, calls, results)
                        .map_err(|e| SeamError::Context(e.to_string()))?;
                    push_new_blobs(history, &trace, &mut blob_cursor);
                    match last_action {
                        NextAction::RequestContinuation => continue,
                        NextAction::Done => {
                            return finish(log, turn_id, trace, requests, state, rounds, closed);
                        }
                        other => {
                            return Err(SeamError::Harness(format!(
                                "unexpected post-results action: {other:?}"
                            )));
                        }
                    }
                }
                let mut results: Vec<ToolResultRecord> = Vec::new();
                let mut last_action = NextAction::None;
                // RC1-N-FIX2 (whole-slice R2 P1): whatever ends the turn in
                // the middle of this batch — the owner's Deny, a Stop, a
                // storage or harness error — the calls that already ran stay
                // in the model's history as one exchange (fitted like any
                // batch); the unfinished call and the calls after it are
                // dropped. `run_turn` keeps the history when it holds such
                // completed work, so the next turn does not repeat it.
                let mut batch_error: Option<SeamError> = None;
                let catalog_plan = if calls.len() == 1 {
                    catalog
                        .as_ref()
                        .and_then(|turn| {
                            registry
                                .lookup_unique(&calls[0].name)
                                .ok()
                                .filter(|d| {
                                    d.identity.origin.origin
                                        == awili_tools::catalog::origin().origin
                                })
                                .map(|_| {
                                    prepare_catalog_plan(
                                        &registry,
                                        turn,
                                        roots,
                                        &calls[0],
                                        history,
                                        &assistant_text,
                                        &calls,
                                        projection_window,
                                        model,
                                        &wire,
                                    )
                                })
                        })
                        .transpose()?
                } else {
                    None
                };
                for call in &calls {
                    // S6: grants are re-read at each decision, so a Revoke
                    // clicked mid-turn stops the very next matching action.
                    let grants = host.authorizer.load_grants(
                        &log.read_records()?.0,
                        &grant_store.read_grant_records()?.0,
                    );
                    let record = match execute_one(
                        host,
                        &registry,
                        &grants,
                        log,
                        grant_store,
                        turn_id,
                        &session_id,
                        session_root,
                        call,
                        host_io,
                        executors,
                        install_authority,
                        roots,
                        catalog_plan.as_ref(),
                        catalog.as_mut(),
                    ) {
                        Ok(record) => record,
                        Err(e) => {
                            batch_error = Some(e);
                            break;
                        }
                    };
                    if !emitted.insert(record.awili_call_id.0.clone()) {
                        batch_error = Some(SeamError::Harness(format!(
                            "duplicate result emission for {}",
                            record.awili_call_id.0
                        )));
                        break;
                    }
                    results.push(record.clone());
                    match step(
                        &mut state,
                        Input::ToolResult(record),
                        log,
                        turn_id,
                        &mut trace,
                        host_io,
                    ) {
                        Ok(action) => last_action = action,
                        Err(e) => {
                            batch_error = Some(e);
                            break;
                        }
                    }
                }
                if let Some(e) = batch_error {
                    let done = results.len();
                    if done > 0 {
                        let kept_calls = calls[..done].to_vec();
                        let kept =
                            fit_exchange(history, &profile, &assistant_text, &kept_calls, results);
                        history
                            .push_exchange(assistant_text, kept_calls, kept)
                            .map_err(|e| SeamError::Context(e.to_string()))?;
                    }
                    return Err(e);
                }
                // RC1-N (usability round 4, M3): the results enter the
                // model's context fitted to what the window can still hold —
                // an oversized result is cut with a plain marker and the turn
                // goes on, instead of the next assembly failing "cannot fit".
                // The log and the host already saw the full results.
                let results = fit_exchange(history, &profile, &assistant_text, &calls, results);
                history
                    .push_exchange(assistant_text, calls, results)
                    .map_err(|e| SeamError::Context(e.to_string()))?;
                push_new_blobs(history, &trace, &mut blob_cursor);
                match last_action {
                    NextAction::RequestContinuation => continue,
                    NextAction::Done => {
                        return finish(log, turn_id, trace, requests, state, rounds, closed);
                    }
                    other => {
                        return Err(SeamError::Harness(format!(
                            "unexpected post-results action: {other:?}"
                        )));
                    }
                }
            }
            NextAction::Done => {
                // C4: the final round's assistant text IS session context —
                // the next turn assembles over it. A zero-call exchange
                // carries it (the old intra-turn Ledger could drop it because
                // it never outlived the turn).
                let final_text = state.text()[text_start..].to_string();
                if !final_text.is_empty() {
                    history
                        .push_exchange(final_text, vec![], vec![])
                        .map_err(|e| SeamError::Context(e.to_string()))?;
                }
                push_new_blobs(history, &trace, &mut blob_cursor);
                return finish(log, turn_id, trace, requests, state, rounds, closed);
            }
            NextAction::None | NextAction::RequestContinuation => {
                let report = close_and_report(
                    log, turn_id, "failed", trace, requests, &state, rounds, closed,
                )?;
                return Err(SeamError::TurnFailed {
                    reason: "stream ended without a terminal action".into(),
                    report: Box::new(report),
                });
            }
        }
    }
}

/// ONE reducer/event/audit path (design blocker 5): every event reaches
/// B2 — including `InputRejected`, which is recorded FIRST and then
/// surfaces as a typed harness failure (round-1 P1: audit before
/// abort). `pub(crate)` so the test suite pins that ordering directly.
pub(crate) fn step<S: AppendOnlyLog>(
    state: &mut TurnState,
    input: Input,
    log: &mut ExecutionLog<S>,
    turn_id: &str,
    trace: &mut Vec<AgentEvent>,
    host_io: &mut dyn TurnHost,
) -> Result<NextAction, SeamError> {
    let (events, action) = reduce(state, input);
    for ev in events {
        // ORDER (D2-u1 P0-4): durable append FIRST, then the trace, then the
        // live surface. A B2 failure therefore emits nothing — the surface
        // can never show an event the log does not carry.
        log.record_event(turn_id, &ev)?;
        let rejected = matches!(&ev, AgentEvent::InputRejected { .. });
        let detail = format!("{ev:?}");
        trace.push(ev);
        let gone = host_io.observe(trace.last().expect("just pushed")).is_err();
        if rejected {
            return Err(SeamError::Harness(format!(
                "reducer rejected host input: {detail}"
            )));
        }
        if gone {
            return Err(SeamError::Cancelled(StopCause::Disconnected));
        }
    }
    Ok(action)
}

/// Feed EVERY delta of one stream body (round-1 P1: nothing on the wire
/// goes unaudited — trailing usage after finish included). The terminal
/// action arrives at StreamEnd; a SECOND terminal action in one body is
/// a harness-visible protocol violation. (decode_stream stops at the
/// first `data: [DONE]`, so this guard is defensive today — pinned
/// directly by test against a handcrafted delta sequence.)
pub(crate) fn feed_deltas<S: AppendOnlyLog>(
    state: &mut TurnState,
    deltas: Vec<awili_engine::wire::WireDelta>,
    log: &mut ExecutionLog<S>,
    turn_id: &str,
    trace: &mut Vec<AgentEvent>,
    host_io: &mut dyn TurnHost,
) -> Result<NextAction, SeamError> {
    let mut decided: Option<NextAction> = None;
    for delta in deltas {
        feed_one(state, delta, log, turn_id, trace, host_io, &mut decided)?;
    }
    Ok(decided.unwrap_or(NextAction::None))
}

/// RC2-GEMMA3: one delta of [`feed_deltas`] — the live path feeds a stream's
/// deltas one by one as they arrive, with the same rule: a SECOND terminal
/// action in one body is a harness-visible protocol violation.
fn feed_one<S: AppendOnlyLog>(
    state: &mut TurnState,
    delta: awili_engine::wire::WireDelta,
    log: &mut ExecutionLog<S>,
    turn_id: &str,
    trace: &mut Vec<AgentEvent>,
    host_io: &mut dyn TurnHost,
    decided: &mut Option<NextAction>,
) -> Result<(), SeamError> {
    let a = step(state, Input::Delta(delta), log, turn_id, trace, host_io)?;
    if !matches!(a, NextAction::None) {
        if decided.is_some() {
            return Err(SeamError::Harness(
                "two terminal actions in one stream".into(),
            ));
        }
        *decided = Some(a);
    }
    Ok(())
}

/// RC2-GEMMA3 (R7-speed): what the live path fed while a body was arriving.
/// Only content deltas go live (text, reasoning, tool-call fragments,
/// finish, usage); a provider error, the end and a truncation wait for the
/// whole body, so the refusal-and-resend rule and every ending keep their
/// one path. The whole body is then decoded again and must start with
/// exactly what was fed.
struct LiveFeed {
    decoder: SseDecoder,
    fed: Vec<awili_engine::wire::WireDelta>,
    decided: Option<NextAction>,
    /// A non-content delta arrived: nothing more goes live.
    held: bool,
    error: Option<SeamError>,
    cancelled: bool,
}

impl LiveFeed {
    fn new() -> Self {
        Self {
            decoder: SseDecoder::new(),
            fed: Vec::new(),
            decided: None,
            held: false,
            error: None,
            cancelled: false,
        }
    }

    /// One line of the body: the deltas to feed now (kept in `fed`), or
    /// `None` when the turn was stopped (reading stops).
    fn take(
        &mut self,
        line: &str,
        wire: &wire_names::WireNames,
        host_io: &mut dyn TurnHost,
    ) -> Option<Vec<awili_engine::wire::WireDelta>> {
        use awili_engine::wire::WireDelta;
        if host_io.cancelled() {
            self.cancelled = true;
            return None;
        }
        if let Some((done, total)) = keepalive_progress(line) {
            host_io.prompt_progress(done, total);
        }
        let mut now = Vec::new();
        if self.held {
            return Some(now);
        }
        for delta in wire.decode(self.decoder.push_line(line)) {
            let content = matches!(
                delta,
                WireDelta::Text(_)
                    | WireDelta::Reasoning(_)
                    | WireDelta::ToolCallFragment { .. }
                    | WireDelta::Finish { .. }
                    | WireDelta::Usage { .. }
            );
            if !content {
                self.held = true;
                break;
            }
            self.fed.push(delta.clone());
            now.push(delta);
        }
        Some(now)
    }
}

/// RC2-GEMMA4 (R7-A3): a reply the server cut off before any tool call: the
/// body ended without `[DONE]` (no provider error, no tool-call fragment).
fn cut_text_reply(raw: &str) -> bool {
    use awili_engine::wire::WireDelta;
    let deltas = decode_stream(raw);
    matches!(deltas.last(), Some(WireDelta::StreamTruncated))
        && !deltas.iter().any(|d| {
            matches!(
                d,
                WireDelta::ToolCallFragment { .. } | WireDelta::ProviderError { .. }
            )
        })
}

/// RC2-GEMMA3: an mlx_lm keepalive comment, `: keepalive <done>/<total>`
/// (prompt tokens read so far, of the request's total).
fn keepalive_progress(line: &str) -> Option<(u64, u64)> {
    let (done, total) = line.strip_prefix(": keepalive ")?.trim().split_once('/')?;
    Some((done.parse().ok()?, total.parse().ok()?))
}

/// Whole-batch protocol validation (design blocker 4): nonempty, every
/// provider call id present and unique, unique awili ids. Name
/// RESOLUTION is deliberately NOT here — unknown/ambiguous names are
/// SEMANTIC per-call failures (design blocker 3): the model is told via
/// an error result; protocol failures terminate the turn.
fn validate_batch(calls: &[ToolCall]) -> Result<(), String> {
    if calls.is_empty() {
        return Err("empty DispatchTools batch".into());
    }
    let mut provider_ids = BTreeSet::new();
    let mut awili_ids = BTreeSet::new();
    for c in calls {
        let Some(pid) = c.provider_call_id.as_deref().filter(|p| !p.is_empty()) else {
            return Err(format!(
                "call {} has no provider call id",
                c.awili_call_id.0
            ));
        };
        if !provider_ids.insert(pid.to_string()) {
            return Err(format!("duplicate provider call id {pid:?} in batch"));
        }
        if !awili_ids.insert(c.awili_call_id.0.clone()) {
            return Err(format!(
                "duplicate awili call id {:?} in batch",
                c.awili_call_id.0
            ));
        }
    }
    Ok(())
}

/// One call through C1 → B2 → C2-u4. Every semantic outcome — Allow,
/// Ask-approved, Ask-denied, Deny, unknown/ambiguous, NotDispatchable —
/// yields EXACTLY ONE ToolResultRecord. Denials record durably FIRST
/// (design P2): a failed denial record aborts rather than telling the
/// model the denial was durable.
#[allow(clippy::too_many_arguments)]
fn execute_one<S1: AppendOnlyLog>(
    host: &HostAuthority,
    registry: &ToolRegistry,
    grants: &awili_engine_authz::GrantSet,
    log: &mut ExecutionLog<S1>,
    grant_store: &mut dyn awili_engine_log::grant_service::GrantStoreAccess,
    turn_id: &str,
    session_id: &str,
    session_root: &str,
    call: &ToolCall,
    host_io: &mut dyn TurnHost,
    executors: &Executors,
    install_authority: &dyn awili_engine_authz::InstallAuthorityQuery,
    roots: &RootsAcceptance,
    catalog_plan: Option<&CatalogPlan>,
    mut catalog_turn: Option<&mut CatalogTurn>,
) -> Result<ToolResultRecord, SeamError> {
    // Before ANY side effect (D2-u1 P0-1).
    if host_io.cancelled() {
        return Err(SeamError::Cancelled(StopCause::Cancelled));
    }
    let err_record = |content: String| ToolResultRecord {
        awili_call_id: call.awili_call_id.clone(),
        content,
        is_error: true,
    };

    // Registry resolution BEFORE authorization (design blocker 3):
    // unknown or ambiguous names never reach a dialog or an intent.
    let def = match registry.lookup_unique(&call.name) {
        Ok(d) => d,
        Err(e) => return Ok(err_record(format!("tool not available: {e:?}"))),
    };
    let floor = def.descriptor.risk_floor();

    // RC2-R3 (round 3 Major 1): in the Work folder, a local file or shell
    // tool (`LOCAL_FILE_TOOLS`) given a relative path is answered at once:
    // no card, nothing runs, no intent; it is told the engine's own file
    // tools and the full path. Only a server that works on this Mac
    // (RC2-R3M1): a remote tool of the same name keeps its own paths.
    if host_io.relative_paths_mean_the_folder() && is_local_file_origin(&def.identity.origin.origin)
    {
        if let Some(rel) = local_relative_path(def.name(), &call.arguments) {
            return Ok(err_record(relative_path_redirect(
                &call.name,
                &rel,
                session_root,
            )));
        }
    }

    // The ONLY site that mints a ResolvedTool (unit 4, design r4/r5):
    // registry-resolution time. The token rides the AuthzContext into C1,
    // which binds its identity into the evidence — downstream nothing
    // accepts a caller-selected identity.
    let resolved = host.registry.mint(def.identity.clone());

    // RC2-R2F7: a host whose model works anywhere runs an engine file-tool
    // call with a full path outside the session's folder against that
    // path's own folder (the scope the authorizer screens and the sandbox
    // opens); every other call keeps the session's folder.
    let call_root = (host_io.any_folder()
        && def.identity.origin.origin == awili_engine_authz::policy::BUILTIN_FS_ORIGIN)
        .then(|| any_folder::call_root(&def.identity.name, &call.arguments, session_root))
        .flatten();
    let folder_root: &str = call_root.as_deref().unwrap_or(session_root);

    // RC2-R2G2 (RC2-R2F review P1): a search names and reads files its
    // arguments do not. The secret files it would show are found first
    // (the same walk, in the same sandbox) and put the call at the
    // screening floor: the owner's card names them, and only what he
    // allowed there is shown when it runs.
    let secret_files = if def.identity.origin.origin
        == awili_engine_authz::policy::BUILTIN_FS_ORIGIN
    {
        let mut canonical_call = call.clone();
        canonical_call.name = def.identity.name.clone();
        awili_builtins::secret_files_reached(folder_root, &canonical_call, awili_engine_authz::is_secret_path)
    } else {
        Vec::new()
    };

    // Unit 5 §5 (rulings 19/26): registry resolution FIRST, then the
    // per-call authority query, then ONE authorization inside the
    // synchronous callback — the witness moves by value into the context,
    // the context moves into authorize, and the returned Decision carries
    // nothing witness-bearing out. Successful witness issuance is the
    // authorization linearization point; every later call queries again.
    let decision = install_authority.with_witness(
        &def.identity.origin,
        Box::new(|w: Option<awili_attest_types::InstallOriginWitness<'_>>| {
            let ctx = AuthzContext {
                turn_id,
                session_id,
                folder_root: Some(folder_root),
                grants,
                registry_floor: Some(floor),
                resolved_tool: &resolved,
                install_witness: w,
            };
            host.authorizer.authorize_reaching(call, ctx, &secret_files)
        }),
    );
    let evidence = match decision {
        Decision::Allow { evidence } => evidence,
        Decision::Ask(pending) => {
            // U4 (D7): a non-floor Ask may be auto-approved by the host
            // WITHOUT a dialog — the authorizer records a scope-"once"
            // approval carrying the auto-mode audit constraints. `approve`
            // is never called for an auto-approved Ask, and floor calls
            // (floor_required) always fall through to the dialog below.
            if !pending.floor_required()
                && host_io.auto_approve(&call.awili_call_id.0, pending.prompt(), pending.risk())
            {
                // Re-check cancellation exactly as the dialog path does:
                // a stop between the decision and the resolution records
                // nothing.
                if host_io.cancelled() {
                    return Err(SeamError::Cancelled(StopCause::Cancelled));
                }
                host.authorizer
                    .resolve_auto(pending, &host.approval_cap, log)?
            } else {
                // The dialog path — byte-for-byte the pre-U4 behaviour.
                // The dialog blocks HERE. A non-Verdict outcome stops the
                // turn BEFORE `resolve` — so a cancel records neither a
                // grant nor a denial, and nothing runs (D2-u1 P0-1).
                let verdict = match host_io.approve(
                    &call.awili_call_id.0,
                    pending.prompt(),
                    pending.risk(),
                ) {
                    ApprovalOutcome::Verdict(v) => v,
                    ApprovalOutcome::Cancelled => {
                        return Err(SeamError::Cancelled(StopCause::Cancelled));
                    }
                    ApprovalOutcome::Shutdown => {
                        return Err(SeamError::Cancelled(StopCause::Shutdown));
                    }
                    ApprovalOutcome::Disconnected => {
                        return Err(SeamError::Cancelled(StopCause::Disconnected));
                    }
                };
                // A flag trip DURING the dialog also stops before resolution.
                if host_io.cancelled() {
                    return Err(SeamError::Cancelled(StopCause::Cancelled));
                }
                match host.authorizer.resolve(
                    pending,
                    verdict,
                    &host.approval_cap,
                    log,
                    grant_store,
                )? {
                    Some(evidence) => evidence,
                    // RC1-N: the denial is durable (the authorizer recorded
                    // it); an owner who wants the turn to end there gets a
                    // stop, not a model reply about the refusal.
                    None if host_io.stop_after_denial() => {
                        return Err(SeamError::Cancelled(StopCause::Denied));
                    }
                    None => return Ok(err_record(OWNER_DECLINED.into())),
                }
            }
        }
        Decision::Deny(denial) => {
            // Round-1 P0: a cancel that trips between the entry check and
            // HERE must not leave a durable denial behind. The policy denial
            // is dropped unrecorded — the turn is stopping, and replay says
            // "cancelled", not "denied".
            if host_io.cancelled() {
                return Err(SeamError::Cancelled(StopCause::Cancelled));
            }
            let reason = denial.record(log)?; // durable FIRST, then inform
            return Ok(err_record(refusal_text(&reason)));
        }
    };

    // Last gate before the filesystem is touched.
    if host_io.cancelled() {
        return Err(SeamError::Cancelled(StopCause::Cancelled));
    }
    // Immediate pre-dispatch check (unit 4): the registry's CURRENT
    // resolution must equal the evidence-bound identity — equality AND
    // availability. Hard error on either: never re-resolve by bare name,
    // never fall back from a vanished mcp:* origin to builtin:fs.
    verify_bound_identity(registry, &call.name, evidence.tool_identity())?;
    // Closed executor routing is identity-bound and remains exhaustive.
    match executors.route(&def.identity.origin)? {
        // Builtin leg: byte-for-byte the pre-item-6 call.
        Executor::BuiltinFs => {
            host_io.tool_dispatching(&call.awili_call_id.0, floor);
            match run_builtin_fs(
                log,
                turn_id,
                call,
                evidence,
                Limits::default(),
                awili_builtins::SecretFiles {
                    is_secret: awili_engine_authz::is_secret_path,
                    // RC2-R2G2: only the secret files the owner's card named.
                    allowed: &secret_files,
                },
            )? {
                BuiltinRunOutcome::Completed(record) => Ok(record),
                BuiltinRunOutcome::NotDispatchable { reason, .. } => {
                    Ok(err_record(format!("not dispatchable: {reason}")))
                }
            }
        }
        Executor::Catalog => {
            let Some(plan) = catalog_plan else {
                return Ok(err_record(
                    "catalog operation was not prepared for this request".into(),
                ));
            };
            if host_io.cancelled() {
                return Err(SeamError::Cancelled(StopCause::Cancelled));
            }
            let (content, is_error, replacement) = match plan {
                CatalogPlan::Search { response } => (response.clone(), false, None),
                CatalogPlan::Selection { response, selected } => {
                    (response.clone(), false, Some(selected.clone()))
                }
                CatalogPlan::Rejected { response } => (response.clone(), true, None),
            };
            let intent = log.intent(turn_id, call, evidence)?;
            let ticket = match intent {
                IntentOutcome::Dispatchable(ticket) => ticket,
                IntentOutcome::NotDispatchable { reason, .. } => {
                    return Ok(err_record(format!("not dispatchable: {reason}")));
                }
            };
            log.dispatch_started(&ticket)?;
            host_io.tool_dispatching(&call.awili_call_id.0, floor);
            log.dispatch_finalized(&ticket, !is_error, &content)?;
            if let (Some(selected), Some(turn)) = (replacement, catalog_turn.as_mut()) {
                turn.selected = selected;
            }
            Ok(ToolResultRecord {
                awili_call_id: call.awili_call_id.clone(),
                content,
                is_error,
            })
        }
        Executor::Mcp(facade) => dispatch_mcp(
            facade,
            floor,
            roots,
            executors.submit_deadline(),
            log,
            turn_id,
            call,
            evidence,
            host_io,
        ),
    }
}

/// The MCP dispatch leg (unit 4 item 6): durable B2 intent BEFORE
/// dispatch, the accepted epoch from the host-controlled record, submit
/// through the coordinator facade, then the disposition rule — Definite
/// terminal → `dispatch_finalized` then the reducer; Uncertain terminal →
/// NO finalize, the error result still reaching the reducer so the turn
/// continues. No live launch/reconnect wiring (unit 4b): the facade is
/// whatever the composition threaded in, tested against scripted
/// channels.
#[allow(clippy::too_many_arguments)]
fn dispatch_mcp<S: AppendOnlyLog>(
    facade: &dyn McpExecutor,
    floor: RiskClass,
    roots: &RootsAcceptance,
    submit_deadline: Duration,
    log: &mut ExecutionLog<S>,
    turn_id: &str,
    call: &ToolCall,
    evidence: awili_engine_log::authz::AuthzEvidence,
    host_io: &mut dyn TurnHost,
) -> Result<ToolResultRecord, SeamError> {
    let origin = evidence.tool_identity().origin.clone();
    let evidence_name = evidence.tool_identity().name.clone();
    let err_record = |content: String| ToolResultRecord {
        awili_call_id: call.awili_call_id.clone(),
        content,
        is_error: true,
    };
    // The LIVE BIND, BEFORE the durable intent (unit 4b, design r9 §4.3,
    // row 21): the accepted (generation, epoch) comes from the RECORD,
    // never the advertised proposal (which this leg cannot even read: the
    // executor surface exposes no lifecycle view). No record → the origin
    // is unavailable and the refusal MINTS NO TICKET and WRITES NO
    // DURABLE RECORD — nothing happened that replay could need. A binding
    // that goes stale AFTER this point is refused by the runtime before a
    // dispatch is created, finalizing Definite false (never Uncertain).
    let Some((accepted_generation, root_epoch)) = roots.accepted(&origin) else {
        let content = format!(
            "origin {:?} unavailable: no accepted roots record — a validated bring-up is \
             required before dispatch",
            origin.origin
        );
        return Ok(err_record(content));
    };
    // Durable intent BEFORE dispatch (design r2): the fsync'd intent and
    // dispatch_started precede the command entering the runtime channel,
    // so a crash anywhere past this point replays truthfully.
    let ticket = match log.intent(turn_id, call, evidence)? {
        IntentOutcome::Dispatchable(t) => t,
        IntentOutcome::NotDispatchable { reason, .. } => {
            return Ok(err_record(format!("not dispatchable: {reason}")));
        }
    };
    log.dispatch_started(&ticket)?;
    let intent_id = ticket.intent_id().to_string();
    // ONE absolute deadline for this call's whole submission story (r3 Delta 1
    // / Delta 3): the submit AND a later cancel share it. Computed once, here,
    // and kept across a successful submission — recomputing it at cancel time
    // would hand a cancelled turn a second full retry window against
    // backpressure, which is exactly the bound the design forbids.
    let deadline = Instant::now() + submit_deadline;
    // Submit. A FINAL refusal never entered the runtime, so the ticket comes
    // back by move and the seam is its last owner — it rides inside the typed
    // failure rather than being dropped here.
    let mut live = match facade.dispatch_call(McpDispatch {
        deadline,
        ticket,
        intent_id,
        origin,
        generation: accepted_generation,
        // RC2-R51: the server is asked for its own tool by its own name,
        // also when the model named it by the qualified one.
        call: ToolCall {
            name: evidence_name.clone(),
            ..call.clone()
        },
        root_epoch,
    }) {
        // RC1-O-FIX7/FIX8 (whole-slice review P2): told only once the call
        // is really in the runtime — after the roots and intent checks and
        // after the submission was accepted (a refused submit never ran).
        Ok(live) => {
            host_io.tool_dispatching(&call.awili_call_id.0, floor);
            live
        }
        Err(McpSubmitRefused { kind, ticket }) => {
            return Err(SeamError::McpDispatch(McpDispatchFailure::SubmitRefused {
                kind,
                ticket,
            }));
        }
    };

    // In-flight cancellation (design r2 §"Cancellation"). The seam polls in
    // slices instead of blocking, so a host stop reaches a call that is
    // already in the runtime.
    //
    // ORDER WITHIN AN ITERATION MATTERS. The host stop is read AFTER the poll
    // but is honored BEFORE any outcome that poll produced: a stop the host
    // requested while we were blocked must not be masked by a terminal landing
    // in the same slice. `TurnHost`'s rule is that cancellation is polled
    // before side effects, and feeding a result to the reducer after the user
    // said stop is a side effect.
    //
    // Once a stop is seen with the call still live we submit
    // `Cancel { intent_id }` under the ORIGINAL absolute deadline and keep
    // polling: the coordinator's linearization point picks the winner (design
    // r3 Delta 3). A real terminal wins only if the pump commits it BEFORE
    // the coordinator commits cancel-submission failure; a terminal arriving
    // after that failure is committed is quarantined and cannot revise the
    // returned failure. A refused cancel comes back as its own outcome. The
    // seam never synthesizes a terminal.
    let mut cancel_requested = false;
    let mut cancel_submitted = false;
    let outcome = loop {
        let polled = live.poll_outcome(Instant::now() + MCP_POLL_SLICE);
        // Latch the stop first, whatever the poll produced.
        if host_io.cancelled() {
            cancel_requested = true;
        }
        if let Some(outcome) = polled {
            // The call already reached its single outcome; a cancel now would
            // be for a finished intent. The latched stop still decides how the
            // TURN ends, below.
            break outcome;
        }
        if cancel_requested && !cancel_submitted {
            cancel_submitted = true;
            // A refused cancel is NOT a decision: item 5 commits the refusal
            // at the waiter table and wakes this waiter with
            // `CancelSubmitRefused`, so we drain that winner below rather
            // than assuming the call is over here. The drain is bounded by the
            // coordinator's exactly-one-delivery guarantee, and it opens no
            // new submission window: `deadline` is the original one, so an
            // already-elapsed bound refuses immediately.
            let _ = live.cancel(deadline);
        }
    };

    match outcome {
        McpDispatchOutcome::Terminal {
            ticket,
            disposition,
            result,
        } => {
            match disposition.certainty() {
                // Definite: finalize with the RETURNED ticket, then feed the
                // reducer. `ok` derives from the result, exactly as the
                // builtin leg derives it.
                ExecutionCertainty::Definite => {
                    log.dispatch_finalized(&ticket, !result.is_error, &result.content)?;
                }
                // Uncertain: NO finalize — replay classifies; no uncertain
                // intent auto-dispatches again (C1 contract).
                ExecutionCertainty::Uncertain => {}
            }
            // The call's fate is now durable either way. If the host asked to
            // stop while it was in flight, the TURN stops — mirroring the
            // builtin path, where an observed cancellation is always
            // `StopCause::Cancelled` and never a continued turn. The result
            // is not fed to the reducer, exactly as a builtin call cancelled
            // before it ran is not.
            if cancel_requested {
                return Err(SeamError::Cancelled(StopCause::Cancelled));
            }
            Ok(result)
        }
        // Every non-terminal outcome leaves the intent UNFINALIZED on
        // purpose: the seam never synthesizes a terminal, and replay
        // classifies dispatch_started-without-finalized as Uncertain — the
        // honest truth for a call whose outcome this process cannot know
        // through the failed path. The two ticket-bearing shapes carry it
        // into the typed failure; the other two are ticket-free by contract
        // (the ticket is with the runtime, which is the runtime contract's
        // one admitted loss path, not a new one).
        McpDispatchOutcome::Rejected { reason, ticket } => Err(SeamError::McpDispatch(
            McpDispatchFailure::SubmissionRejected { reason, ticket },
        )),
        McpDispatchOutcome::CancelSubmitRefused { kind } => Err(SeamError::McpDispatch(
            McpDispatchFailure::CancelSubmitRefused { kind },
        )),
        McpDispatchOutcome::StreamClosed => {
            Err(SeamError::McpDispatch(McpDispatchFailure::StreamClosed))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish<S: AppendOnlyLog>(
    log: &mut ExecutionLog<S>,
    turn_id: &str,
    trace: Vec<AgentEvent>,
    requests: Vec<String>,
    state: TurnState,
    rounds: usize,
    closed: &mut bool,
) -> Result<TurnReport, SeamError> {
    let failed = trace.iter().any(|e| matches!(e, AgentEvent::TurnFailed(_)));
    // Reasoning shares the output budget. Exhausting it before any answer
    // must surface as a recoverable failure, not a successful empty reply.
    let empty_at_limit = state.text().trim().is_empty()
        && trace.iter().any(|e| matches!(e, AgentEvent::TurnCompleted {
            stop: awili_engine::events::StopKind::Length, ..
        }));
    let outcome = if failed || empty_at_limit { "failed" } else { "completed" };
    let report = close_and_report(
        log, turn_id, outcome, trace, requests, &state, rounds, closed,
    )?;
    if empty_at_limit && !failed {
        return Err(SeamError::TurnFailed {
            reason: "The model reached its output-token limit before producing an answer. Reduce the model's reasoning budget or increase its output limit, then retry.".into(),
            report: Box::new(report),
        });
    }
    if failed {
        let reason = report
            .events
            .iter()
            .find_map(|e| match e {
                AgentEvent::TurnFailed(err) => Some(err.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "unknown".into());
        return Err(SeamError::TurnFailed {
            reason,
            report: Box::new(report),
        });
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn close_and_report<S: AppendOnlyLog>(
    log: &mut ExecutionLog<S>,
    turn_id: &str,
    outcome: &str,
    trace: Vec<AgentEvent>,
    requests: Vec<String>,
    state: &TurnState,
    rounds: usize,
    closed: &mut bool,
) -> Result<TurnReport, SeamError> {
    log.turn_closed(turn_id, outcome)?;
    *closed = true;
    let (records, _) = log.read_records()?;
    Ok(TurnReport {
        events: trace,
        final_text: state.text().to_string(),
        rounds,
        replay: classify(&records),
        requests,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod rc2_gemma_tests;

#[cfg(test)]
mod image_content_regressions {
    use super::*;
    #[test]
    fn admitted_user_image_is_an_image_part_in_the_tool_capable_wire_request() {
        use awili_engine::context::{AssembledContext, ContextGroup, GroupId};
        let a=AssembledContext {groups:vec![ContextGroup::User{id:GroupId(1),text:"![s](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)\nDescribe".into()}],tokens:16390,excluded_continuations:vec![]};
        let wire = encode_chat_completions(&a).unwrap();
        assert_eq!(wire[0]["content"][1]["type"], "image_url");
        assert!(!wire[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("base64"));
    }
    #[test]
    fn compaction_and_text_followup_do_not_import_image_base64() {
        use awili_engine::context::{AssembledContext, CompactionPlan, ContextGroup, GroupId};
        let old = ContextGroup::User { id:GroupId(1),text:"![s](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==)\nDescribe".into()};
        let plan = CompactionPlan {
            summarize_ids: vec![GroupId(1)],
            to_summarize: vec![old.clone()],
            projected_tokens: 10,
            excluded_continuations: vec![],
        };
        let material = render_plan_material(&plan);
        assert!(!material.contains("base64"));
        assert!(material.contains("embedded-image"));
        let a = AssembledContext {
            groups: vec![
                old,
                ContextGroup::User {
                    id: GroupId(2),
                    text: "Continue using your previous answer".into(),
                },
            ],
            tokens: 10,
            excluded_continuations: vec![],
        };
        let wire = encode_chat_completions(&a).unwrap();
        assert!(!wire.to_string().contains("base64"));
        assert!(wire[0]["content"].is_string());
    }
}
