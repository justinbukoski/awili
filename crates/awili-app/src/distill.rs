//! Turn distillation (slice 10) — after a turn commits, ask the active provider
//! to compress it into one durable memory, gated and rate-capped. Extracted from
//! `main.rs` as part of the full-review decomposition: the worker body and the
//! GPUI-free admit/settle/usage helpers live behind one boundary so the cap and
//! staleness logic stay unit-testable. `distill_settlement_tests` remain in
//! `main.rs`, reaching `admit_distillation`/`settle_distillation` through here.

use futures::StreamExt;

use awili_config::ModelChoice;
use awili_memory::MemoryClient;
use awili_providers::{ChatMessage, ChatRequest, ProviderEvent, Role as PRole, Usage};

use crate::cost::CostTicker;
use crate::persistence::{PersistCmd, Persistence};
use crate::provider_build::{build_provider, ProviderKeys};
use crate::MemoryCfg;

/// What a distillation worker reports back to the app thread.
pub(crate) struct DistillOutcome {
    /// Whether a memory was actually written.
    pub(crate) wrote: bool,
    /// The token usage the provider reported for the distillation call, if
    /// any — real spend that must reach the today total even when the call
    /// failed or the model answered SKIP (review SF3).
    pub(crate) usage: Option<Usage>,
}

/// Worker-thread body of turn distillation (slice 10 §2): ask the active
/// provider to compress the committed turn into one factual memory, gate the
/// answer (SKIP / off-contract / too short — [`awili_memory::distilled_content`]),
/// then POST it under category `conversation` at priority 60. Runs on a
/// detached thread, so the blocking drain of the provider stream is fine;
/// every failure path is a stderr line and `wrote: false` — never a panic,
/// never a blocked UI (§2d). When the memory service is disabled or the
/// `[memory] write` toggle is off (slice 11), it returns immediately with
/// `wrote: false, usage: None` — no provider or memory request is ever made.
/// The memory write uses the client the caller resolved and moved in — the
/// one authoritative resolution made before admission — never a fresh
/// [`MemoryClient::resolved`] after the provider call. A client that
/// resolved to V65 is refused before the provider request is built (V65
/// hold): `wrote: false, usage: None`, zero provider and memory requests.
pub(crate) fn distill_and_store(
    choice: &ModelChoice,
    keys: &ProviderKeys,
    mem: &MemoryCfg,
    client: MemoryClient,
    user_text: &str,
    reply_text: &str,
    hermes_base_url: &str,
) -> DistillOutcome {
    // Slice 11: never build the provider request or touch the memory network
    // when distillation is disabled. `distill_turn` already refuses to spawn a
    // worker in this state (and `admit_distillation` will not charge the cap);
    // this is the last line of defence so a future caller cannot accidentally
    // invoke the provider or write a memory.
    if !mem.distillation_allowed() {
        return DistillOutcome {
            wrote: false,
            usage: None,
        };
    }
    // V65 hold: the caller resolved ONE authoritative client before admission
    // and moved it in — a v6.5 backend is not yet a valid destination for
    // automatic distillation (candidate integration pending). Refuse HERE,
    // before the provider request is built: no provider call, no canonical
    // memory write, no settled usage. Defensive by design — admission above
    // already refused a V65 client; this is the last line of defence.
    if client.backend() == awili_memory::MemoryBackend::V65 {
        return DistillOutcome {
            wrote: false,
            usage: None,
        };
    }
    let request = ChatRequest {
        model: choice.model.clone(),
        system: Some(awili_memory::DISTILL_SYSTEM.to_string()),
        messages: vec![ChatMessage {
            role: PRole::User,
            content: awili_memory::distill_prompt(user_text, reply_text),
        }],
    };
    // Distillation is a plain text-in/text-out side call — never the gateway.
    let provider = build_provider(choice, keys, hermes_base_url);
    let mut stream = provider.stream(request);
    let mut answer = String::new();
    let mut failed = false;
    let mut usage: Option<Usage> = None;
    futures::executor::block_on(async {
        while let Some(ev) = stream.next().await {
            match ev {
                ProviderEvent::HostApproval(_)
                    | ProviderEvent::Activity(_)
                | ProviderEvent::ToolStarted(_)
                | ProviderEvent::ContextWindow(_)
                | ProviderEvent::ToolAttemptFailed => {}
                ProviderEvent::Token(t) => answer.push_str(&t),
                ProviderEvent::Error(e) => {
                    eprintln!("awili: turn distillation failed (ignored): {e}");
                    failed = true;
                }
                // Captured even on a failed call: the spend already happened,
                // and the app feeds it into the today total (review SF3).
                ProviderEvent::Usage(u) => usage = Some(u),
                ProviderEvent::Done => break,
            }
        }
    });
    if failed {
        return DistillOutcome {
            wrote: false,
            usage,
        };
    }
    let Some(content) = awili_memory::distilled_content(&answer) else {
        // The model declined (SKIP/refusal prose) or produced nothing usable.
        return DistillOutcome {
            wrote: false,
            usage,
        };
    };
    // The SAME authoritative client that was admitted writes the memory —
    // never re-resolved here, where the backend could have moved (and a
    // v6.5 resolution after the provider ran would defeat the V65 hold).
    let wrote = match client.write_memory(
        &content,
        awili_memory::DISTILL_CATEGORY,
        awili_memory::DISTILL_PRIORITY,
    ) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("awili: distilled memory write failed (ignored): {e}");
            false
        }
    };
    DistillOutcome { wrote, usage }
}

/// Spawn-side admission of a distillation attempt (review SF1): the session
/// cap is charged HERE, at spawn — the counter counts ATTEMPTS, not
/// completions — so two turns finishing within one distillation round-trip
/// cannot both read a stale counter and overrun the flood cap. An attempt
/// that fails or SKIPs refunds its slot in [`settle_distillation`].
///
/// A disabled memory service or an off `[memory] write` toggle (slice 11)
/// refuses admission HERE and never charges the cap — the budget stays intact
/// for when writes are re-enabled, and no worker (and thus no provider or
/// memory request) can be spawned from a refused admission. A client that
/// resolved to V65 is likewise refused HERE, before the increment (V65
/// hold): the resolved [`MemoryClient`] is passed in so the check sees the
/// authoritative backend — never a re-guess from the URL — and the budget
/// stays intact for the candidate integration.
pub(crate) fn admit_distillation(
    memory: &MemoryCfg,
    client: &MemoryClient,
    user_text: &str,
    reply_text: &str,
    writes_this_session: &mut usize,
) -> bool {
    if !memory.distillation_allowed() {
        return false;
    }
    // V65 hold: refuse BEFORE the cap is charged — the budget must not be
    // spent on a backend that automatic distillation may not write to yet.
    if client.backend() == awili_memory::MemoryBackend::V65 {
        return false;
    }
    if !awili_memory::should_distill(user_text, reply_text, *writes_this_session) {
        return false;
    }
    *writes_this_session += 1;
    true
}

/// Completion-side settlement of a distillation attempt (review SF1/SF2).
/// Returns whether the "memory saved" chip should flash. A worker that lands
/// after `new_conversation` (stale generation — the same guard slice 8 uses
/// for late usage events) belongs to a session whose counter was already
/// reset: it must neither refund nor flash in the unrelated new session (SF2).
/// A current-session failure/SKIP refunds the slot charged at spawn (SF1).
pub(crate) fn settle_distillation(
    writes_this_session: &mut usize,
    spawn_gen: u64,
    current_gen: u64,
    wrote: bool,
) -> bool {
    if spawn_gen != current_gen {
        return false;
    }
    if !wrote {
        *writes_this_session = writes_this_session.saturating_sub(1);
        return false;
    }
    true
}

/// Account a distillation call's token usage (review SF3): the spend is real,
/// so it reaches today's total and the durable `usage_events` row — the same
/// accounting a visible turn gets — but never the session ticker, which
/// tracks the conversation the user is watching. Applied regardless of
/// session staleness: today's total spans sessions.
pub(crate) fn apply_distill_usage(
    cost: &mut CostTicker,
    persist: Option<&Persistence>,
    conv_id: &str,
    provider: &str,
    model: &str,
    usage: Usage,
    now: i64,
) {
    cost.note_background_usage(provider, model, usage, now);
    if let Some(p) = persist {
        p.send(PersistCmd::RecordUsage {
            cached: usage.cached_input_tokens,
            conv: conv_id.to_string(),
            turn_ts: now,
            provider: provider.to_string(),
            model: model.to_string(),
            input: usage.input_tokens,
            output: usage.output_tokens,
        });
    }
}
