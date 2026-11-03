//! What every finished Workshop turn does for the owner, whichever helper ran
//! it (a native Claude/Codex/ZCode/DeepSeek session, the in-app model, the
//! gateway): record what actually ran, book the spend that was reported, write
//! the turn to memory, and read it aloud when asked.
//!
//! These used to hang off the removed Chat stream, so only Chat turns got them.
//! They now run from the one place a Work or Code turn ends
//! ([`AwiliApp::finish_workshop_turn`]). Nothing here is invented: usage and
//! the backend-reported model are recorded only when the helper reported them
//! and an unknown location stays unknown.

use awili_core::{Locality, Provenance};
use awili_providers::Usage;

use crate::graduation_record::Class;
use crate::AwiliApp;

/// One finished turn, as the shared completion hooks need it.
pub(crate) struct FinishedTurn {
    pub(crate) conv_id: String,
    /// The assistant row in the transcript that owns the reply.
    pub(crate) reply_idx: usize,
    /// The Code tab whose transcript holds the reply (`None`: the stored
    /// conversation's own view).
    pub(crate) code_tab: Option<u64>,
    pub(crate) user_text: String,
    pub(crate) reply_text: String,
    /// The ledger's executor name ("claude", "codex", "owned", "gateway", …).
    pub(crate) executor: String,
    /// The backend-reported model when there is one, else the requested one.
    pub(crate) model: String,
    pub(crate) class: Class,
    /// Tokens the helper reported for this turn; `None` = not reported.
    pub(crate) usage: Option<Usage>,
    /// The turn finished normally: not stopped, not failed.
    pub(crate) ok: bool,
    /// The thread is on screen now (so read-aloud may speak it).
    pub(crate) visible: bool,
    /// The short id of the run that produced this turn (S3).
    pub(crate) run_id: String,
}

/// What kind of route a helper is, in the inspector's words.
fn route_of(executor: &str) -> &'static str {
    match executor {
        "owned" => "in-app model",
        "gateway" => "fleet gateway",
        "local-build" => "local build",
        _ => "native helper",
    }
}

/// The provenance for a finished turn: the model and tokens the helper
/// reported, an estimate only where the price is known, and a location only
/// where Awili knows it.
pub(crate) fn provenance_for(turn: &FinishedTurn) -> Provenance {
    let locality = match turn.class {
        Class::Local => Locality::Local,
        Class::Frontier => Locality::Cloud,
        Class::Unknown => Locality::Unknown,
    };
    let label = crate::run_footer::executor_label(&turn.executor);
    let mut p = Provenance::chat(
        route_of(&turn.executor),
        label.clone(),
        turn.model.clone(),
        locality,
        format!("ran on {label}"),
    );
    if let Some(u) = turn.usage {
        let rate = crate::run_footer::price_provider(&turn.executor)
            .map(|prov| awili_core::pricing::price_for(prov, &turn.model))
            .filter(|price| price.input_per_mtok > 0.0 || price.output_per_mtok > 0.0)
            .map(|price| awili_core::CostRate::new(price.input_per_mtok, price.output_per_mtok));
        p = p.with_usage(
            awili_core::Usage {
                cached_input_tokens: u.cached_input_tokens,
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
            },
            rate.as_ref(),
        );
    }
    p
}

impl AwiliApp {
    /// The one completion path for a Work or Code turn.
    pub(crate) fn finish_workshop_turn(
        &mut self,
        turn: FinishedTurn,
        cx: &mut gpui::Context<Self>,
    ) {
        let has_reply = !turn.reply_text.trim().is_empty();
        // S3: the run id belongs to the reply row, shown by Inspect reply.
        let reply_ts = match turn.code_tab {
            Some(tab_id) => self
                .work
                .code_tabs
                .iter()
                .find(|t| t.id == tab_id)
                .and_then(|t| t.convo.message(turn.reply_idx).map(|m| m.ts)),
            None => self
                .views
                .get(&turn.conv_id)
                .and_then(|v| v.convo.message(turn.reply_idx).map(|m| m.ts)),
        };
        if let Some(ts) = reply_ts {
            if let Some(json) = self.run_ids.set(&turn.conv_id, ts, &turn.run_id) {
                if let Some(p) = &self.persist {
                    p.set_state(&crate::run_error::RunIds::key(&turn.conv_id), json);
                }
            }
        }
        // 1. What actually ran, on the reply it belongs to.
        if has_reply {
            let prov = provenance_for(&turn);
            match turn.code_tab {
                Some(tab_id) => {
                    if let Some(tab) = self.work.code_tabs.iter_mut().find(|t| t.id == tab_id) {
                        tab.convo.set_provenance(turn.reply_idx, prov);
                    }
                }
                None => {
                    if let Some(view) = self.views.get_mut(&turn.conv_id) {
                        view.convo.set_provenance(turn.reply_idx, prov);
                    }
                }
            }
        }
        // 2. Reported spend reaches the cost chip and the durable usage rows,
        // finished or not: the tokens were used. The gateway books its own
        // reported dollars (`note_reported_cost`), so it is not counted twice.
        if let (Some(usage), true) = (turn.usage, turn.executor != "gateway") {
            let provider = crate::run_footer::price_provider(&turn.executor)
                .unwrap_or(turn.executor.as_str())
                .to_string();
            let gen = self.cost.generation();
            let note = crate::persistence::apply_usage_event(
                &mut self.cost,
                self.persist.as_ref(),
                &turn.conv_id,
                &provider,
                &turn.model,
                usage,
                crate::feed::unix_now(),
                gen,
            );
            if let Some(note) = note {
                self.post_usage_memory(note);
            }
        }
        // 3. A turn that finished with a real reply goes to memory.
        if turn.ok && has_reply {
            if !turn.user_text.trim().is_empty() {
                self.chat_log(&turn.conv_id, crate::Role::User, &turn.user_text);
            }
            self.chat_log(&turn.conv_id, crate::Role::Assistant, &turn.reply_text);
            self.distill_turn(
                &turn.conv_id,
                turn.user_text.clone(),
                turn.reply_text.clone(),
                cx,
            );
        }
        // 4. Read-aloud / the hands-free loop.
        self.speak_finished_reply(&turn.reply_text, !turn.ok, turn.visible, cx);
        cx.notify();
    }

    /// The tokens the helper reported for the last reply of the thread on
    /// screen, from that reply's recorded provenance: "Last reply: 12,345 in ·
    /// 678 out · Claude". `None` when nothing was reported — an unknown stays
    /// unknown, it is never estimated.
    pub(crate) fn last_reply_usage_label(&self) -> Option<String> {
        let convo = match self.mode {
            crate::work::Mode::Code => &self.work.code_tabs.get(self.work.code_active)?.convo,
            crate::work::Mode::Work => &self.active_view()?.convo,
            _ => return None,
        };
        let prov = convo
            .messages()
            .iter()
            .rev()
            .find(|m| m.role == crate::Role::Assistant && m.provenance.is_some())?
            .provenance
            .as_ref()?;
        let usage = prov.usage?;
        Some(format!(
            "Last reply: {} in \u{b7} {} out \u{b7} {}",
            crate::run_footer::fmt_count(u64::from(usage.input_tokens)),
            crate::run_footer::fmt_count(u64::from(usage.output_tokens)),
            prov.provider
        ))
    }

    /// Read a finished reply aloud (read-aloud on, or the hands-free loop).
    /// A stopped or failed turn, an empty reply, or a thread that is no longer
    /// on screen is not spoken; in the hands-free loop the mic is re-armed
    /// instead, since no speech will end to do it.
    pub(crate) fn speak_finished_reply(
        &mut self,
        text: &str,
        interrupted: bool,
        visible: bool,
        cx: &mut gpui::Context<Self>,
    ) {
        if !(self.voice.auto_speak || self.voice.conversation_mode) {
            return;
        }
        if interrupted || text.trim().is_empty() || !visible {
            if self.voice.conversation_mode {
                self.start_voice_listening(cx);
            }
            return;
        }
        self.speak_text(text.to_string(), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(executor: &str, model: &str, class: Class, usage: Option<Usage>) -> FinishedTurn {
        FinishedTurn {
            conv_id: "c".into(),
            reply_idx: 1,
            code_tab: None,
            user_text: "q".into(),
            reply_text: "a".into(),
            executor: executor.into(),
            model: model.into(),
            class,
            usage,
            ok: true,
            visible: true,
            run_id: String::new(),
        }
    }

    #[test]
    fn a_native_turn_records_the_model_and_tokens_it_reported_and_a_priced_estimate() {
        let u = Usage {
            cached_input_tokens: None,
            input_tokens: 1000,
            output_tokens: 200,
        };
        let p = provenance_for(&turn(
            "claude",
            "claude-haiku-4-5",
            Class::Frontier,
            Some(u),
        ));
        assert_eq!(p.model, "claude-haiku-4-5");
        assert_eq!(p.locality, Locality::Cloud);
        assert_eq!(
            p.usage.map(|u| (u.input_tokens, u.output_tokens)),
            Some((1000, 200))
        );
        assert!(p.est_cost_usd.is_some_and(|c| c > 0.0), "{p:?}");
    }

    #[test]
    fn unreported_usage_stays_unknown_and_an_unseen_location_is_not_claimed() {
        let p = provenance_for(&turn("owned", "qwen", Class::Unknown, None));
        assert_eq!(p.usage, None, "tokens not reported are not invented");
        assert_eq!(p.est_cost_usd, None);
        assert_eq!(p.locality, Locality::Unknown);
    }

    #[test]
    fn an_unpriced_helper_gets_tokens_but_no_dollar_figure() {
        let u = Usage {
            cached_input_tokens: None,
            input_tokens: 10,
            output_tokens: 10,
        };
        let p = provenance_for(&turn("codex", "gpt-6", Class::Frontier, Some(u)));
        assert!(p.usage.is_some());
        assert_eq!(p.est_cost_usd, None, "no price known, no estimate");
    }
}
