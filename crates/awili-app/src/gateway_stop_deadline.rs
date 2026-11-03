//! M1a (S2-F01): a gateway Stop the worker never acknowledges must not park
//! Work sends forever. The UI arms a one-shot [`GATEWAY_STOP_ACK_DEADLINE`]
//! per sent Stop; if that same Stop is still pending when it fires, the old
//! worker is abandoned (Shutdown, sender dropped, session forgotten, run_seq
//! bumped) and the stopped turn is finished through the existing reducer,
//! which replays the parked prompt once on a fresh worker.

use std::time::Duration;

use gpui::Context;

use awili_gateway::run_events::RunStatus;

use crate::work::{GatewayCmd, WorkUpdate};
use crate::AwiliApp;

/// How long a gateway Stop may stay unacknowledged before the UI abandons the
/// worker: the 10 s drain bound plus 5 s for a healthy interrupt RPC.
pub(crate) const GATEWAY_STOP_ACK_DEADLINE: Duration = Duration::from_secs(15);
/// Shown when a prompt is parked behind a pending Stop (M1a).
pub(crate) const PARKED_TOAST: &str =
    "Queued — it sends when the stopped turn ends (at most 15 s).";
/// Shown when a further submit arrives while one prompt is already parked: the
/// new text goes back to the composer and the first parked prompt keeps its
/// place (M1a).
pub(crate) const ALREADY_PARKED_TOAST: &str =
    "One message is already waiting for the Stop — this one is back in the composer.";
/// Shown when a Stop the worker never acknowledged is abandoned at the
/// deadline (M1a).
pub(crate) const ABANDONED_TOAST: &str =
    "The stopped turn didn't end in 15 s — continuing on a fresh gateway session.";

/// The deadline armed for Stop `timer_gen` acts only if that same Stop is
/// still pending: a Stop that was already acknowledged (no longer `pending`),
/// or a newer Stop (a different generation), makes it a no-op.
pub(crate) fn stop_ack_overdue(pending: bool, current_gen: u64, timer_gen: u64) -> bool {
    pending && current_gen == timer_gen
}

/// The Progress card's activity line is visible while a run is live OR a Stop
/// is still pending, so "Stopping the previous turn…" is on screen for the
/// whole barrier, not only while the run is live.
pub(crate) fn activity_visible(running: bool, stop_pending: bool) -> bool {
    running || stop_pending
}

impl AwiliApp {
    /// Bump `work.gateway_stop_gen` and arm the one-shot ack deadline for this
    /// Stop. The callback is generation-guarded ([`stop_ack_overdue`]): an
    /// acknowledgement that arrived first, or a newer Stop, makes it a no-op —
    /// the `show_toast` pattern (main.rs).
    pub(crate) fn arm_gateway_stop_deadline(&mut self, cx: &mut Context<Self>) {
        self.work.gateway_stop_gen = self.work.gateway_stop_gen.wrapping_add(1);
        let gen = self.work.gateway_stop_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(GATEWAY_STOP_ACK_DEADLINE)
                .await;
            this.update(cx, |app, cx| app.expire_gateway_stop(gen, cx))
                .ok();
        })
        .detach();
    }

    /// The deadline fired for the Stop with generation `gen`. No-op unless
    /// that same Stop is still pending; otherwise the old worker is abandoned
    /// and the stopped turn is finished through the existing reducer:
    ///
    /// 1. log; 2. `Shutdown` the worker and drop its sender; 3. forget the
    ///    session and identity; 4. bump `run_seq` (every late event from the
    ///    old worker is dropped by the epoch check); 5.
    ///    `apply_work_update(Terminal { Stopped, .. })` — ends the barrier,
    ///    finishes the stopped row, replays the parked prompt on a fresh
    ///    worker; 6. the abandonment toast.
    pub(crate) fn expire_gateway_stop(&mut self, gen: u64, cx: &mut Context<Self>) {
        if !stop_ack_overdue(
            self.work.gateway_stop_pending,
            self.work.gateway_stop_gen,
            gen,
        ) {
            return;
        }
        eprintln!("awili: gateway Stop not acknowledged within 15s; abandoning the worker");
        if let Some(cmd_tx) = self.work.gateway_cmd_tx.take() {
            let _ = cmd_tx.send(GatewayCmd::Shutdown);
        }
        self.work.session_id = None;
        self.work.worker_identity = None;
        self.work.run_seq = self.work.run_seq.wrapping_add(1);
        self.apply_work_update(
            WorkUpdate::Terminal {
                status: RunStatus::Stopped,
                output: None,
                model: None,
            },
            cx,
        );
        self.show_toast(ABANDONED_TOAST, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The deadline acts only for the exact Stop it was armed for.
    #[test]
    fn m1a_overdue_rule() {
        assert!(stop_ack_overdue(true, 3, 3), "the same Stop still pending");
        assert!(!stop_ack_overdue(false, 3, 3), "the ack arrived first");
        assert!(
            !stop_ack_overdue(true, 4, 3),
            "a newer Stop supersedes the timer"
        );
    }

    /// The activity line is visible for the whole barrier, not only while the
    /// run is live.
    #[test]
    fn m1a_activity_visible_while_stopping() {
        assert!(activity_visible(false, true));
        assert!(activity_visible(true, false));
        assert!(!activity_visible(false, false));
    }
}
