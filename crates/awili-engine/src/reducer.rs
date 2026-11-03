//! The pure reducer: `(state, input) -> (events, next_action)`.
//!
//! No IO, no clocks, no randomness. Determinism is a tested property
//! (PS-ENG-02): replaying the same inputs over a fresh state yields identical
//! events and identical final state.
//!
//! Phase discipline (hardened per Codex B1 review):
//! - Late WIRE deltas after a terminal state are absorbed silently (stream
//!   tails are harmless and expected on cancel/error).
//! - Late HOST inputs (tool results) after a terminal state are rejected with
//!   an auditable [`AgentEvent::InputRejected`] — B2 must be able to see
//!   replay/duplication bugs.
//! - `Finish` is only legal while `Streaming`; a second finish is a protocol
//!   failure, never a silent overwrite.
//! - `Usage` is only legal while `Streaming` or `Finishing`, and accumulates
//!   across segments (per-segment `UsageReported` + aggregated total on the
//!   terminal event).

use crate::error::EngineError;
use crate::events::{AgentEvent, AwiliCallId, StopKind, ToolCall, ToolResultRecord, Usage};
use crate::provenance::{ContinuationArtifact, ContinuationKind};
use crate::state::{TurnPhase, TurnState};
use crate::wire::{FinishReason, WireDelta};

/// Reducer inputs: decoded wire deltas, tool results fed back by the host,
/// or a user cancellation.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Delta(WireDelta),
    /// Full canonical result — content preserved (Codex review P0).
    ToolResult(ToolResultRecord),
    Cancel,
}

/// What the host should do next. The reducer never does it itself.
#[derive(Debug, Clone, PartialEq)]
pub enum NextAction {
    None,
    /// Send these calls through C1 authorization and dispatch them.
    DispatchTools(Vec<ToolCall>),
    /// All tool results are in: request the model continuation.
    RequestContinuation,
    /// Turn is over (completed, cancelled, or failed).
    Done,
}

pub fn reduce(state: &mut TurnState, input: Input) -> (Vec<AgentEvent>, NextAction) {
    if state.is_terminal() {
        return match input {
            // Stream tails after cancel/error are expected: absorb silently.
            Input::Delta(_) => (vec![], NextAction::Done),
            // Duplicate cancel is harmless and idempotent.
            Input::Cancel => (vec![], NextAction::Done),
            // A tool result landing after terminal is a host/replay bug —
            // make it visible (auditable), do not mutate state.
            Input::ToolResult(r) => (
                vec![AgentEvent::InputRejected {
                    reason: format!(
                        "tool result {:?} after terminal phase {:?}",
                        r.awili_call_id.0, state.phase
                    ),
                }],
                NextAction::Done,
            ),
        };
    }

    match input {
        Input::Cancel => {
            state.cancel_requested = true;
            (vec![], NextAction::None)
        }
        Input::ToolResult(record) => reduce_tool_result(state, record),
        Input::Delta(delta) => reduce_delta(state, delta),
    }
}

/// Fail the turn. RC2-SVC7 (F0668): the events already produced for this
/// input (e.g. the reasoning blob captured at a stream end) are KEPT and the
/// failure is appended after them — the reasoning was already taken out of
/// the state, so dropping its event would lose it.
fn fail(
    state: &mut TurnState,
    mut events: Vec<AgentEvent>,
    err: EngineError,
) -> (Vec<AgentEvent>, NextAction) {
    state.phase = TurnPhase::Failed;
    events.push(AgentEvent::TurnFailed(err));
    (events, NextAction::Done)
}

fn reduce_tool_result(
    state: &mut TurnState,
    record: ToolResultRecord,
) -> (Vec<AgentEvent>, NextAction) {
    if state.phase != TurnPhase::AwaitingToolResults {
        // Non-terminal but wrong phase: auditable rejection, phase unchanged
        // (a mis-sequenced host should not be able to torch a live turn).
        return (
            vec![AgentEvent::InputRejected {
                reason: format!(
                    "tool result {:?} in phase {:?}",
                    record.awili_call_id.0, state.phase
                ),
            }],
            NextAction::None,
        );
    }
    let Some(pos) = state
        .awaiting_results
        .iter()
        .position(|id| *id == record.awili_call_id)
    else {
        return (
            vec![AgentEvent::InputRejected {
                reason: format!(
                    "unknown or duplicate tool result id {:?}",
                    record.awili_call_id.0
                ),
            }],
            NextAction::None,
        );
    };
    state.awaiting_results.remove(pos);
    state.results.push(record.clone());
    let events = vec![AgentEvent::ToolResultIngested(record)];
    if state.awaiting_results.is_empty() {
        state.phase = TurnPhase::ReadyToContinue;
        (events, NextAction::RequestContinuation)
    } else {
        (events, NextAction::None)
    }
}

fn reduce_delta(state: &mut TurnState, delta: WireDelta) -> (Vec<AgentEvent>, NextAction) {
    state.deltas_seen += 1;
    let mut events = Vec::new();

    // Only content-bearing deltas open a stream segment: Usage or errors as
    // the first frame must not fabricate a phantom turn (Codex review P2).
    let opens_segment = matches!(
        delta,
        WireDelta::Text(_)
            | WireDelta::Reasoning(_)
            | WireDelta::ToolCallFragment { .. }
            | WireDelta::Finish { .. }
    );
    if opens_segment {
        match state.phase {
            TurnPhase::Idle => {
                events.push(AgentEvent::TurnStarted);
                state.phase = TurnPhase::Streaming;
            }
            TurnPhase::ReadyToContinue => {
                // Continuation stream from the same turn: new segment.
                state.phase = TurnPhase::Streaming;
                state.pending_finish = None;
            }
            _ => {}
        }
    }

    match delta {
        WireDelta::Text(t) => {
            if state.phase != TurnPhase::Streaming {
                return fail(
                    state,
                    events,
                    EngineError::Protocol(format!("text delta in phase {:?}", state.phase)),
                );
            }
            state.text.push_str(&t);
            events.push(AgentEvent::TextDelta(t));
            (events, NextAction::None)
        }
        WireDelta::Reasoning(r) => {
            if state.phase != TurnPhase::Streaming {
                return fail(
                    state,
                    events,
                    EngineError::Protocol(format!("reasoning delta in phase {:?}", state.phase)),
                );
            }
            state.reasoning.push_str(&r);
            events.push(AgentEvent::ReasoningDelta(r));
            (events, NextAction::None)
        }
        WireDelta::ToolCallFragment {
            choice,
            tool_index,
            id,
            name,
            args_fragment,
        } => {
            if state.phase != TurnPhase::Streaming {
                return fail(
                    state,
                    events,
                    EngineError::Protocol(format!("tool fragment in phase {:?}", state.phase)),
                );
            }
            let slot = state.assembling.entry((choice, tool_index)).or_default();
            // First-wins on repeated metadata: providers repeat id/name on
            // some fragments; a CONFLICTING repeat is provider damage we
            // surface at assembly validation, not here.
            if let Some(id) = id {
                slot.provider_call_id.get_or_insert(id);
            }
            if let Some(name) = name {
                slot.name.get_or_insert(name);
            }
            slot.args_buf.push_str(&args_fragment);
            (events, NextAction::None)
        }
        WireDelta::Finish { choice, reason } => {
            if state.phase != TurnPhase::Streaming {
                // Includes the double-finish case: pending_finish is never
                // silently overwritten (Codex review P1).
                return fail(
                    state,
                    events,
                    EngineError::Protocol(format!("finish in phase {:?}", state.phase)),
                );
            }
            // Per-choice finalization (arbitration fix): only THIS choice's
            // calls finalize; other choices keep assembling.
            let finalize: Vec<_> = state
                .assembling
                .keys()
                .filter(|(c, _)| *c == choice)
                .cloned()
                .collect();
            let mut finalized = Vec::new();
            for key in finalize {
                if let Some(p) = state.assembling.remove(&key) {
                    finalized.push((key, p));
                }
            }
            for ((_choice, _tool_index), partial) in finalized {
                let awili_call_id = state.mint_call_id();
                // Empty-args calls are legal (zero-arg tools).
                let raw = if partial.args_buf.trim().is_empty() {
                    "{}".to_string()
                } else {
                    partial.args_buf
                };
                let valid = serde_json::from_str::<serde_json::Value>(&raw).is_ok();
                match (partial.name, valid) {
                    (Some(name), true) => {
                        let call = ToolCall {
                            awili_call_id,
                            provider_call_id: partial.provider_call_id,
                            name,
                            arguments: raw,
                        };
                        state.ready_calls.push(call.clone());
                        events.push(AgentEvent::ToolCallReady(call));
                    }
                    (name, ok) => {
                        events.push(AgentEvent::ToolCallInvalid {
                            awili_call_id,
                            provider_call_id: partial.provider_call_id,
                            name,
                            raw_arguments: raw,
                            reason: if ok {
                                "missing function name".into()
                            } else {
                                "arguments are not valid JSON".into()
                            },
                        });
                    }
                }
            }
            state.pending_finish = Some(reason);
            state.phase = TurnPhase::Finishing;
            (events, NextAction::None)
        }
        WireDelta::Usage {
            prompt_tokens,
            completion_tokens,
        } => {
            if !matches!(state.phase, TurnPhase::Streaming | TurnPhase::Finishing) {
                return fail(
                    state,
                    events,
                    EngineError::Protocol(format!("usage chunk in phase {:?}", state.phase)),
                );
            }
            let u = Usage {
                prompt_tokens,
                completion_tokens,
            };
            state.add_usage(u);
            events.push(AgentEvent::UsageReported(u));
            (events, NextAction::None)
        }
        WireDelta::ProviderError { message, retryable } => {
            state.phase = TurnPhase::Failed;
            events.push(AgentEvent::TurnFailed(EngineError::Provider {
                message,
                retryable,
            }));
            (events, NextAction::Done)
        }
        WireDelta::StreamEnd => reduce_stream_end(state, events),
        WireDelta::StreamTruncated => {
            if state.cancel_requested {
                state.phase = TurnPhase::Cancelled;
                events.push(AgentEvent::TurnCancelled {
                    usage: state.usage_total,
                });
                (events, NextAction::Done)
            } else {
                let seen = state.deltas_seen;
                state.phase = TurnPhase::Failed;
                events.push(AgentEvent::TurnFailed(EngineError::UnexpectedTruncation {
                    events_seen: seen,
                }));
                (events, NextAction::Done)
            }
        }
    }
}

fn reduce_stream_end(
    state: &mut TurnState,
    mut events: Vec<AgentEvent>,
) -> (Vec<AgentEvent>, NextAction) {
    // Capture provider reasoning as an opaque continuation blob at every
    // segment boundary (PS-ENG-04); it replays only to the same model.
    if !state.reasoning.is_empty() {
        events.push(AgentEvent::ContinuationBlobCaptured(
            ContinuationArtifact::new(
                state.provenance.clone(),
                ContinuationKind::Reasoning,
                std::mem::take(&mut state.reasoning).into_bytes(),
            ),
        ));
    }

    match state.pending_finish.take() {
        Some(FinishReason::ToolCalls) => {
            if state.ready_calls.is_empty() {
                return fail(
                    state,
                    events,
                    EngineError::Protocol("finish=tool_calls but no valid calls assembled".into()),
                );
            }
            let calls = std::mem::take(&mut state.ready_calls);
            state.awaiting_results = calls.iter().map(|c| c.awili_call_id.clone()).collect();
            state.phase = TurnPhase::AwaitingToolResults;
            (events, NextAction::DispatchTools(calls))
        }
        Some(reason) => {
            let stop = match reason {
                FinishReason::Stop => StopKind::EndTurn,
                FinishReason::Length => StopKind::Length,
                FinishReason::ContentFilter | FinishReason::Refusal => StopKind::Refusal,
                FinishReason::PauseTurn => StopKind::Paused,
                FinishReason::Other(s) => StopKind::Other(s),
                FinishReason::ToolCalls => unreachable!("handled above"),
            };
            state.phase = TurnPhase::Completed;
            events.push(AgentEvent::TurnCompleted {
                stop,
                usage: state.usage_total,
            });
            (events, NextAction::Done)
        }
        None => fail(
            state,
            events,
            EngineError::Protocol("stream ended without a finish reason".into()),
        ),
    }
}

/// An `AwiliCallId` re-export point for `Input` consumers.
pub type CallId = AwiliCallId;
