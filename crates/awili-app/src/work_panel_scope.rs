//! Presentation ownership only; never reset the live worker on navigation.
use gpui::{div, prelude::*, Context, SharedString};

use crate::{theme, work::WorkState, AwiliApp};

impl WorkState {
    pub(crate) fn panel_is_visible(&self, active: &str) -> bool {
        self.conv_id.as_deref().is_none_or(|owner| owner == active)
    }

    /// The side panel's note about a run in another conversation. RC1-N
    /// (round 4 Minor 11): only while that run is still going — a finished
    /// run elsewhere is not news on this conversation (a new, empty one
    /// showed "The previous run belongs to another conversation").
    pub(crate) fn other_conversation_notice(&self, active: &str) -> Option<&'static str> {
        (!self.panel_is_visible(active) && self.running).then_some(
            "A run is active in another conversation. Open it for progress, approvals or Stop.",
        )
    }
}

impl AwiliApp {
    pub(crate) fn work_other_conversation_card(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let label = self
            .work
            .other_conversation_notice(self.sidebar.active_id())?;
        let owner = self.work.conv_id.clone()?;
        Some(
            div()
                .flex()
                .flex_col()
                .gap(theme::z(8.))
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().muted)
                .child(label)
                .child(
                    div()
                        .id("work-open-run-conversation")
                        .cursor_pointer()
                        .text_color(theme::p().accent_text)
                        .child(SharedString::from("Open run conversation"))
                        .on_click(cx.listener(move |this, _ev, window, cx| {
                            this.select_conversation(&owner, window, cx);
                        })),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::{GatewayCfg, HermesCfg};
    use awili_gateway::run_events::RunStatus;

    fn state() -> WorkState {
        WorkState::new(
            HermesCfg {
                base_url: String::new(),
                api_key: String::new(),
                enabled: false,
                dashboard_url: String::new(),
            },
            GatewayCfg {
                dashboard_url: String::new(),
                enabled: false,
            },
            Default::default(),
        )
    }

    #[test]
    fn navigation_hides_previous_status_and_return_restores_owner() {
        let mut work = state();
        work.conv_id = Some("original".into());
        work.panel.status = Some(RunStatus::Completed);
        work.panel.tool_attempt_failed = true;
        work.panel.model = Some("reported-model".into());
        for selected in ["new", "existing-other"] {
            assert!(!work.panel_is_visible(selected));
        }
        assert!(work.panel_is_visible("original"));
        assert_eq!(work.panel.status, Some(RunStatus::Completed));
        assert!(work.panel.tool_attempt_failed);
        assert_eq!(work.panel.model.as_deref(), Some("reported-model"));
    }

    /// RC1-N (round 4 Minor 11): a finished run in another conversation
    /// leaves no note on this one; a running one does.
    #[test]
    fn rc1n_only_a_running_run_elsewhere_is_noted() {
        let mut work = state();
        work.conv_id = Some("original".into());
        work.panel.status = Some(RunStatus::Completed);
        assert_eq!(work.other_conversation_notice("new"), None);
        assert_eq!(work.other_conversation_notice("original"), None);
        work.running = true;
        assert!(work
            .other_conversation_notice("new")
            .is_some_and(|n| n.contains("active in another conversation")));
        assert_eq!(work.other_conversation_notice("original"), None);
    }

    #[test]
    fn background_run_and_cancel_ownership_survive_navigation() {
        let mut work = state();
        work.conv_id = Some("running-owner".into());
        work.running = true;
        work.run_seq = 42;
        work.panel.approval = Some(crate::work::ApprovalCard {
            approval_id: "owner-approval".into(),
            kind: "fixture".into(),
            prompt: "fixture".into(),
            // S2(a): the card carries the CLOSED risk class (conservative
            // default for the fixture — no risk reported by the engine).
            risk: awili_gateway::events::ApprovalRisk::Privileged,
            delivery: Default::default(),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        work.agent_abort_tx = Some(tx);
        assert!(!work.panel_is_visible("new"));
        assert!(work.panel_is_visible("running-owner"));
        assert!(work.running);
        assert_eq!(
            work.panel.approval.as_ref().unwrap().approval_id,
            "owner-approval"
        );
        assert_eq!(work.run_seq, 42);
        assert_eq!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        work.agent_abort_tx.take().unwrap().send(()).unwrap();
        assert_eq!(rx.try_recv(), Ok(()));
    }

    #[test]
    fn startup_and_submit_precondition_errors_are_visible() {
        let mut work = state();
        assert!(work.panel_is_visible("first"));
        // submit_work_run pins this owner before folder/engine preconditions.
        work.conv_id = Some("requesting".into());
        work.panel.error = Some("agent runs need a working folder".into());
        assert!(work.panel_is_visible("requesting"));
        assert!(!work.panel_is_visible("previous"));
        assert!(work.panel.error.is_some());
    }
}
