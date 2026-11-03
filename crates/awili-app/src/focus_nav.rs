//! U3e1: keyboard reach. The Tab order with a visible focus ring, and the
//! Work approval chords, both dispatched from the root key handler.

use gpui::{App, Context, KeyDownEvent, Rgba, StyleRefinement, Styled, Window};

use crate::work::Mode;
use crate::{approval_keys, context_menu, theme, AwiliApp};

/// The Tab order (gpui orders by `tab_index`, then paint order): composer →
/// executor chip → Send/Stop → approval pills → sidebar rows → composer.
/// Message bubbles are not tab stops.
pub const COMPOSER: isize = 1;
pub const EXECUTOR: isize = 2;
pub const SEND: isize = 3;
pub const APPROVAL: isize = 4;
pub const SIDEBAR: isize = 5;

/// The legacy keyboard-focus halo for the current appearance.
pub fn focus_ring_color() -> Rgba {
    theme::p().legacy_focus_ring
}

/// The keyboard-focus ring: only the border colour changes, so layout never
/// shifts. A mouse click shows no ring (`focus_visible`).
pub fn ring(s: StyleRefinement) -> StyleRefinement {
    s.border_color(focus_ring_color())
}

/// RC2-R611 (usability RC2 round 5, Minor 10: on the blue Send button the
/// ring was "only a faint lighter edge"): the ring of a control filled with
/// the accent \u{2014} a light inner edge and a blue halo around it. The halo is
/// drawn outside the box, so layout never shifts.
#[cfg(test)]
pub fn ring_on_accent(s: StyleRefinement) -> StyleRefinement {
    s.border_color(crate::theme::p().on_accent)
        .shadow(vec![gpui::BoxShadow {
            color: focus_ring_color().into(),
            offset: gpui::point(gpui::px(0.), gpui::px(0.)),
            blur_radius: gpui::px(0.),
            spread_radius: gpui::px(3.),
            inset: false,
        }])
}

/// A sheet, palette, editor, onboarding or context menu has the keyboard.
pub fn overlay_open(app: &AwiliApp, cx: &mut App) -> bool {
    app.settings.open
        || app.shell.tabs.menu.is_some()
        || app.shell.composer_menus.model.is_some()
        || app.search.open
        || app.palette.open
        || app.command_palette.open
        || app.shortcuts.open
        || app.picker.open
        || app.template_editor.is_some()
        || app.project_editor.is_some()
        || app.onboarding.is_active()
        || context_menu::is_open(cx)
}

impl AwiliApp {
    /// The root key hook: Tab / ⇧Tab move focus; ⌘⇧A / ⌘⇧D (Ctrl+Shift on
    /// Linux) answer a pending Work approval. `true` when the key was used.
    pub fn handle_focus_keys(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if overlay_open(self, cx)
            || (self.shell.inspector.selection.is_some() && crate::job_panel::uses_sheet(window))
            || (self.mode == Mode::Work
                && self.shell.job_open
                && crate::job_panel::uses_sheet(window))
            || (self.shell.conversations
                && crate::consolidation::conversation_mode(self.mode)
                && crate::ui::is_compact(window))
        {
            return false;
        }
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        if ks.key == "tab" && !(m.platform || m.control || m.alt) {
            match m.shift {
                true => window.focus_prev(cx),
                false => window.focus_next(cx),
            }
            return true;
        }
        let pending = self.mode == Mode::Work && self.work.panel.approval.is_some();
        match approval_keys::chord(ks, cfg!(target_os = "macos")) {
            Some(decision) if pending => {
                self.resolve_work_approval_focusing(decision, window, cx);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::{ApprovalCard, GatewayCmd, WorkExecutor, WorkUpdate};
    use awili_core::Role;
    use gpui::{AppContext, Entity, EntityInputHandler, Focusable, VisualTestContext};
    use std::sync::mpsc::{channel, Receiver};

    /// Run `test` in a subprocess with a private HOME (`AwiliApp::new` reads
    /// config and opens persistence; pattern of `s1a_proof_tests::isolated`).
    /// `true` inside the child; `false` in the parent once the child passed.
    fn isolated(test: &str) -> bool {
        const CHILD: &str = "AWILI_U3E1_TEST_HOME";
        if std::env::var_os(CHILD).is_some() {
            return true;
        }
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, home.path())
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "isolated test failed:\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains(test) && stdout.contains("1 passed"),
            "subprocess must actually execute the test: {stdout}"
        );
        false
    }

    /// The app in a window, past onboarding (its welcome panel owns the keys),
    /// with the composer focused.
    fn app_window(cx: &mut gpui::TestAppContext) -> (Entity<AwiliApp>, &mut VisualTestContext) {
        let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
        // Active, like the real window (a context menu lives only there).
        window.update(|window, _| window.activate_window());
        app.update(window, |a, cx| {
            a.onboarding = crate::onboarding::Onboarding::existing();
            cx.notify();
        });
        window.run_until_parked();
        focus_composer(&app, window);
        (app, window)
    }

    fn focus_composer(app: &Entity<AwiliApp>, window: &mut VisualTestContext) {
        let composer = app.read_with(window, |a, cx| a.composer.focus_handle(cx));
        window.update(|window, cx| window.focus(&composer, cx));
        window.run_until_parked();
    }

    fn composer_focused(app: &Entity<AwiliApp>, window: &mut VisualTestContext) -> bool {
        let composer = app.read_with(window, |a, cx| a.composer.focus_handle(cx));
        window.update(|window, _| composer.is_focused(window))
    }

    /// Work mode on the gateway executor with an engine of `kind`, a command
    /// channel, a finished assistant row, and a pending approval card.
    fn pending_card(
        app: &Entity<AwiliApp>,
        window: &mut VisualTestContext,
        kind: awili_config::EngineKind,
    ) -> Receiver<GatewayCmd> {
        let (tx, rx) = channel();
        app.update(window, |a, cx| {
            a.set_mode(Mode::Work, cx);
            a.work.executor = WorkExecutor::Gateway;
            a.work.engine.kind = kind;
            // U3-INT1: main (L1) routes the verdict by the live worker's
            // identity, not `engine.kind`; set the identity `kind` names, as
            // main's own `l1b_in_app_approval_card_is_answerable` does.
            a.work.worker_identity = Some(match kind {
                awili_config::EngineKind::Awili => {
                    crate::work_run_identity::WorkRunIdentity::OwnedEngine {
                        model: "m".into(),
                        engine_id: None,
                    }
                }
                _ => crate::work_run_identity::WorkRunIdentity::HermesGateway,
            });
            a.work.gateway_cmd_tx = Some(tx);
            // The Work conversation set_mode opened owns the run (the panel shows).
            let conv = a.sidebar.active_id().to_string();
            a.work.conv_id = Some(conv.clone());
            let mut view = crate::conv_view::ConvView::new();
            view.convo.push(Role::Assistant, "Done.");
            view.rows
                .push(crate::MessageRow::new(view.convo.message(0).unwrap()));
            a.views.insert(conv, view);
            a.work.stream_row_idx = Some(0);
            new_card(a);
            cx.notify();
        });
        window.run_until_parked();
        rx
    }

    fn new_card(a: &mut AwiliApp) {
        a.work.panel.approval = Some(ApprovalCard {
            approval_id: "a1".into(),
            kind: "shell".into(),
            prompt: "rm build/".into(),
            // U3-INT1: main typed the risk (S2(a)); a missing risk now decodes
            // to Privileged, which refuses "Allow always". `Mutating` keeps
            // this card offering every scope but folder, as before the merge.
            risk: awili_gateway::events::ApprovalRisk::Mutating,
            delivery: Default::default(),
        });
    }

    fn stream_text(app: &Entity<AwiliApp>, window: &mut VisualTestContext) -> String {
        app.read_with(window, |a, _| {
            let conv = a.work.conv_id.clone().unwrap();
            a.views[&conv].convo.message(0).unwrap().text.clone()
        })
    }

    fn has_card(app: &Entity<AwiliApp>, window: &mut VisualTestContext) -> bool {
        app.read_with(window, |a, _| a.work.panel.approval.is_some())
    }

    fn approve() -> &'static str {
        if cfg!(target_os = "macos") {
            "cmd-shift-a"
        } else {
            "ctrl-shift-a"
        }
    }

    fn deny() -> &'static str {
        if cfg!(target_os = "macos") {
            "cmd-shift-d"
        } else {
            "ctrl-shift-d"
        }
    }

    fn sent(rx: &Receiver<GatewayCmd>) -> Option<(Option<String>, String)> {
        match rx.try_recv().ok()? {
            GatewayCmd::Approval { id, choice } => Some((id, choice)),
            _ => None,
        }
    }

    #[gpui::test]
    fn u3e1_approval_chords_resolve_card(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::u3e1_approval_chords_resolve_card") {
            return;
        }
        let (app, window) = app_window(cx);
        let rx = pending_card(&app, window, awili_config::EngineKind::Awili);
        window.simulate_keystrokes(approve());
        assert_eq!(sent(&rx), Some((Some("a1".into()), "once".into())));
        assert!(
            !has_card(&app, window),
            "the owned engine clears the card on send"
        );
        assert_eq!(stream_text(&app, window), "Done.\n> ✓ **Allowed once**\n");
        app.update(window, |a, _| new_card(a));
        window.simulate_keystrokes(deny());
        // RC1-N: the in-app engine's Deny ends the turn (`deny-stop`); the
        // reply becomes one plain line when the turn ends, never "✗ Denied".
        assert_eq!(sent(&rx), Some((Some("a1".into()), "deny-stop".into())));
        assert!(!has_card(&app, window));
        assert!(!stream_text(&app, window).contains("Denied"));
        assert_eq!(sent(&rx), None, "one command per chord");
    }

    /// RC1-N item 3 (round 4 K3): Deny on the in-app engine's card sends `deny-stop`, puts the request back in the box with
    /// the plan card's toast, moves the cursor there, drops anything the
    /// ending turn still streams, and leaves ONE plain line as the reply:
    /// no "Approval needed", "Denied", "Blocked by authorization", "No
    /// automatic recovery supplied" or "FAILED".
    #[gpui::test]
    fn rc1n_in_app_deny_is_one_plain_line_and_the_request_comes_back(
        cx: &mut gpui::TestAppContext,
    ) {
        if !isolated(
            "focus_nav::tests::rc1n_in_app_deny_is_one_plain_line_and_the_request_comes_back",
        ) {
            return;
        }
        let (app, window) = app_window(cx);
        let rx = pending_card(&app, window, awili_config::EngineKind::Awili);
        app.update(window, |a, cx| {
            let conv = a.work.conv_id.clone().unwrap();
            let view = a.views.get_mut(&conv).unwrap();
            // The request, then the streaming reply with the card's notice.
            view.convo.push(Role::User, "make hello3.txt saying hi");
            view.rows
                .push(crate::MessageRow::new(view.convo.message(1).unwrap()));
            view.convo.push(
                Role::Assistant,
                "\n> **\u{26A0} Approval needed:** Write hello3.txt (2 characters)\n",
            );
            view.rows
                .push(crate::MessageRow::new(view.convo.message(2).unwrap()));
            a.work.stream_row_idx = Some(2);
            a.work.running = true;
            a.work.panel.approval.as_mut().unwrap().prompt =
                "Write hello3.txt (2 characters)".into();
            cx.notify();
        });
        window.run_until_parked();
        // The cursor is elsewhere when Deny is pressed. (RC2-UX: not the
        // closed search box, which now hands its keys back to the composer.)
        let other = app.read_with(window, |a, cx| {
            a.palette_input.read(cx).focus_handle().clone()
        });
        window.update(|window, cx| window.focus(&other, cx));
        window.run_until_parked();
        assert!(!composer_focused(&app, window));
        // The Deny pill's listener (the chord shares the same call).
        window.update(|window, cx| {
            app.update(cx, |a, cx| {
                a.resolve_work_approval_focusing(
                    awili_gateway::run_events::ApprovalDecision::Deny,
                    window,
                    cx,
                )
            })
        });
        window.run_until_parked();
        assert_eq!(sent(&rx), Some((Some("a1".into()), "deny-stop".into())));
        assert!(!has_card(&app, window));
        assert!(composer_focused(&app, window), "the cursor is in the box");
        let reply_now = |app: &Entity<AwiliApp>, window: &mut VisualTestContext| {
            app.read_with(window, |a, _| {
                let conv = a.work.conv_id.clone().unwrap();
                a.views[&conv].convo.message(2).unwrap().text.clone()
            })
        };
        let before = reply_now(&app, window);
        // What the ending turn still sends never reaches the reply.
        app.update(window, |a, cx| {
            for up in [
                crate::work::WorkUpdate::Token("Blocked by authorization: x\n".into()),
                crate::work::WorkUpdate::Token("\n> **\u{26A0} fs.write FAILED**\n\n".into()),
            ] {
                a.apply_work_update(up, cx);
            }
        });
        window.run_until_parked();
        assert_eq!(reply_now(&app, window), before, "nothing more is streamed");
        // Pre-review P1: nothing is put back before the engine confirms.
        let composer = app.read_with(window, |a, cx| a.composer.read(cx).text().to_string());
        assert_eq!(composer, "", "not before the engine confirms the Deny");
        // The engine confirms: the turn ended Stopped.
        app.update(window, |a, cx| {
            for up in [
                crate::work::WorkUpdate::Event(awili_gateway::run_events::RunEvent::Completion(
                    awili_gateway::run_events::CompletionEvent {
                        status: Some(awili_gateway::run_events::RunStatus::Stopped),
                        output: None,
                    },
                )),
                crate::work::WorkUpdate::TurnComplete,
            ] {
                a.apply_work_update(up, cx);
            }
        });
        window.run_until_parked();
        let composer = app.read_with(window, |a, cx| a.composer.read(cx).text().to_string());
        assert_eq!(composer, "make hello3.txt saying hi", "the request is back");
        let toast = app.read_with(window, |a, _| a.toast.msg.as_ref().map(|m| m.to_string()));
        assert_eq!(toast.as_deref(), Some(crate::plan_gate::DENY_STOPPED_TOAST));
        let reply = app.read_with(window, |a, _| {
            let conv = a.work.conv_id.clone().unwrap();
            a.views[&conv].convo.message(2).unwrap().text.clone()
        });
        assert_eq!(
            reply,
            "Okay \u{2014} you declined \u{201C}Write hello3.txt (2 characters)\u{201D}, so it \
             was not done."
        );
        for jargon in [
            "Approval needed",
            "Denied",
            "Blocked by",
            "No automatic recovery",
            "FAILED",
        ] {
            assert!(!reply.contains(jargon), "{jargon}: {reply}");
        }
        assert!(app.read_with(window, |a, _| !a.work.running
            && a.work.deny_stopped.is_none()));
    }

    /// RC1-N (round 4 Minor 15): the approval card reads Allow once, Deny,
    /// then the broader grants — "Allow always" last, never next to "Allow
    /// once".
    #[gpui::test]
    fn rc1n_approval_pills_put_allow_always_last(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::rc1n_approval_pills_put_allow_always_last") {
            return;
        }
        let (app, window) = app_window(cx);
        window.simulate_resize(gpui::size(gpui::px(1600.), gpui::px(900.)));
        let _rx = pending_card(&app, window, awili_config::EngineKind::Awili);
        app.update(window, |a, cx| {
            a.work.folder = Some(std::env::temp_dir());
            cx.notify();
        });
        window.run_until_parked();
        let order = [
            "work-allow-once",
            "work-deny",
            "work-allow-session",
            "work-allow-folder",
            "work-allow-always",
        ];
        let at: Vec<(gpui::Pixels, gpui::Pixels)> = order
            .iter()
            .map(|id| {
                let b = window
                    .debug_bounds(id)
                    .unwrap_or_else(|| panic!("{id} is drawn"));
                (b.top(), b.left())
            })
            .collect();
        for (i, pair) in at.windows(2).enumerate() {
            assert!(
                pair[0].0 < pair[1].0 || (pair[0].0 == pair[1].0 && pair[0].1 < pair[1].1),
                "{} comes before {}: {at:?}",
                order[i],
                order[i + 1]
            );
        }
    }

    /// RC1-N (pre-review P1): when the denied turn ends any other way than
    /// Stopped (the denial could not be recorded, the turn failed), the
    /// failure shows as itself and nothing is put back.
    #[gpui::test]
    fn rc1n_an_unconfirmed_deny_shows_the_failure(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::rc1n_an_unconfirmed_deny_shows_the_failure") {
            return;
        }
        let (app, window) = app_window(cx);
        let rx = pending_card(&app, window, awili_config::EngineKind::Awili);
        app.update(window, |a, cx| {
            let conv = a.work.conv_id.clone().unwrap();
            let view = a.views.get_mut(&conv).unwrap();
            view.convo.push(Role::User, "make hello3.txt saying hi");
            view.rows
                .push(crate::MessageRow::new(view.convo.message(1).unwrap()));
            view.convo.push(Role::Assistant, "");
            view.rows
                .push(crate::MessageRow::new(view.convo.message(2).unwrap()));
            a.work.stream_row_idx = Some(2);
            a.work.running = true;
            a.resolve_work_approval(awili_gateway::run_events::ApprovalDecision::Deny, cx);
            for up in [
                crate::work::WorkUpdate::Event(awili_gateway::run_events::RunEvent::Completion(
                    awili_gateway::run_events::CompletionEvent {
                        status: Some(awili_gateway::run_events::RunStatus::Failed),
                        output: Some("storage: the log is full".into()),
                    },
                )),
                crate::work::WorkUpdate::TurnComplete,
            ] {
                a.apply_work_update(up, cx);
            }
        });
        window.run_until_parked();
        assert_eq!(sent(&rx), Some((Some("a1".into()), "deny-stop".into())));
        let (reply, composer) = app.read_with(window, |a, cx| {
            let conv = a.work.conv_id.clone().unwrap();
            (
                a.views[&conv].convo.message(2).unwrap().text.clone(),
                a.composer.read(cx).text().to_string(),
            )
        });
        assert!(reply.contains("storage: the log is full"), "{reply}");
        assert!(!reply.contains("you declined"), "{reply}");
        assert_eq!(composer, "", "nothing is put back");
        assert!(app.read_with(window, |a, _| a.work.deny_stopped.is_none()));
    }

    /// RC1-N: the Hermes gateway's Deny is unchanged — plain `deny`, the
    /// agent is told and may go on; nothing is put back.
    #[gpui::test]
    fn rc1n_gateway_deny_is_unchanged(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::rc1n_gateway_deny_is_unchanged") {
            return;
        }
        let (app, window) = app_window(cx);
        let rx = pending_card(&app, window, awili_config::EngineKind::HermesGateway);
        window.simulate_keystrokes(deny());
        let (_, choice) = sent(&rx).expect("the verdict is sent");
        assert_eq!(choice, "deny");
        assert!(app.read_with(window, |a, _| a.work.deny_stopped.is_none()));
    }

    #[gpui::test]
    fn u3e1_delivery_ack_returns_choice_once(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::u3e1_delivery_ack_returns_choice_once") {
            return;
        }
        let (app, window) = app_window(cx);
        let rx = pending_card(&app, window, awili_config::EngineKind::HermesGateway);
        window.simulate_keystrokes(approve());
        let (id, choice) = sent(&rx).expect("the verdict is sent");
        assert_eq!(choice, "once");
        assert!(
            has_card(&app, window),
            "Hermes keeps the card until the ack"
        );
        assert_eq!(
            stream_text(&app, window),
            "Done.",
            "no line before delivery"
        );
        let ack = |delivered| WorkUpdate::ApprovalDelivery {
            id: id.clone(),
            delivered,
        };
        app.update(window, |a, cx| a.apply_work_update(ack(true), cx));
        assert!(!has_card(&app, window));
        assert_eq!(stream_text(&app, window), "Done.\n> ✓ **Allowed once**\n");
        // A repeated (stale) ack writes nothing.
        app.update(window, |a, cx| a.apply_work_update(ack(true), cx));
        assert_eq!(stream_text(&app, window), "Done.\n> ✓ **Allowed once**\n");
        // A failed delivery writes no line; the card stays for the Retry.
        app.update(window, |a, _| new_card(a));
        window.simulate_keystrokes(deny());
        let (id, _) = sent(&rx).expect("the deny is sent");
        let nack = WorkUpdate::ApprovalDelivery {
            id,
            delivered: false,
        };
        app.update(window, |a, cx| a.apply_work_update(nack, cx));
        assert!(has_card(&app, window));
        assert_eq!(stream_text(&app, window), "Done.\n> ✓ **Allowed once**\n");
    }

    #[gpui::test]
    fn u3e1_chords_inert_without_card_or_with_overlay(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::u3e1_chords_inert_without_card_or_with_overlay") {
            return;
        }
        let (app, window) = app_window(cx);
        let rx = pending_card(&app, window, awili_config::EngineKind::Awili);
        app.update(window, |a, _| a.work.panel.approval = None);
        window.simulate_keystrokes(approve());
        assert_eq!(sent(&rx), None, "no card: nothing is sent");
        // A card behind the shortcuts sheet: the chord stands down.
        app.update(window, |a, cx| {
            new_card(a);
            a.shortcuts.open = true;
            cx.notify();
        });
        focus_composer(&app, window);
        window.simulate_keystrokes(approve());
        assert_eq!(sent(&rx), None, "an overlay has the keyboard");
        // An open context menu has the keyboard too.
        app.update(window, |a, cx| {
            a.shortcuts.open = false;
            cx.notify();
        });
        focus_composer(&app, window);
        window.update(|window, cx| {
            crate::context_menu::open(gpui::point(gpui::px(9.), gpui::px(9.)), vec![], window, cx)
        });
        focus_composer(&app, window);
        window.simulate_keystrokes(approve());
        assert_eq!(sent(&rx), None, "a context menu has the keyboard");
        window.update(crate::context_menu::close);
        // Outside Work mode a leftover card is not answered.
        app.update(window, |a, cx| a.set_mode(Mode::Code, cx));
        focus_composer(&app, window);
        window.simulate_keystrokes(approve());
        assert_eq!(sent(&rx), None, "chords answer only in Work");
        assert!(has_card(&app, window));
        assert_eq!(stream_text(&app, window), "Done.");
    }

    #[gpui::test]
    fn u3e1_plain_return_never_approves(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::u3e1_plain_return_never_approves") {
            return;
        }
        let (app, window) = app_window(cx);
        let rx = pending_card(&app, window, awili_config::EngineKind::Awili);
        focus_composer(&app, window);
        window.simulate_keystrokes("enter");
        window.simulate_keystrokes("shift-a");
        assert_eq!(sent(&rx), None);
        assert!(has_card(&app, window), "Return is not an approval (D10)");
    }

    /// Tab from the composer walks every tab stop and comes back; ⇧Tab goes
    /// back. Returns the stops visited, the composer included.
    fn tab_cycle(app: &Entity<AwiliApp>, window: &mut VisualTestContext) -> usize {
        focus_composer(app, window);
        for stops in 1..64 {
            window.simulate_keystrokes("tab");
            if composer_focused(app, window) {
                return stops;
            }
            let any = window.update(|window, cx| window.focused(cx).is_some());
            assert!(any, "Tab always lands on a tab stop");
        }
        panic!("Tab never came back to the composer");
    }

    /// The named stops of one Tab cycle from the composer, in the order Tab
    /// visits them (the bar's chips that carry a tracked focus handle).
    fn named_stops(app: &Entity<AwiliApp>, window: &mut VisualTestContext) -> Vec<&'static str> {
        let named: Vec<(&'static str, gpui::FocusHandle)> = app.read_with(window, |a, _| {
            vec![
                ("project", a.workshop_header_focus(2)),
                ("title", a.workshop_header_focus(3)),
                ("model", a.workshop_header_focus(1)),
                ("toggle", a.workshop_header_focus(10)),
                ("more", a.workshop_header_focus(12)),
                ("state", a.strip.focus_state.clone()),
            ]
        });
        focus_composer(app, window);
        let mut order = Vec::new();
        for _ in 1..64 {
            window.simulate_keystrokes("tab");
            if composer_focused(app, window) {
                return order;
            }
            let focused = window.update(|window, cx| window.focused(cx));
            if let Some(name) = named
                .iter()
                .find(|(_, handle)| focused.as_ref() == Some(handle))
                .map(|(name, _)| *name)
            {
                order.push(name);
            }
        }
        panic!("Tab never came back to the composer");
    }

    #[gpui::test]
    fn u3e1_tab_moves_focus_from_composer(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::u3e1_tab_moves_focus_from_composer") {
            return;
        }
        let (app, window) = app_window(cx);
        window.simulate_keystrokes("tab");
        assert!(!composer_focused(&app, window), "Tab leaves the composer");
        window.simulate_keystrokes("shift-tab");
        assert!(composer_focused(&app, window), "⇧Tab comes back");
        // Workshop: the composer's attach, mic, speaker and Send, the rail, the
        // top bar (project, title, mode, New, panel toggle, more) and, last, the model
        // chip at the left end of the composer footer, then the status strip's state
        // area, plus
        // the docked right-hand Conversations panel (the open thread and its
        // Close, the search field, Archive and the one catalog row: five
        // stops). The panel has no close of its own and no New: the bar has both.
        assert_eq!(tab_cycle(&app, window), 23);
        // The bar is walked left to right: project, title, then the panel toggle
        // and more; the model chip (moved to the composer footer) comes last.
        assert_eq!(
            named_stops(&app, window),
            ["project", "title", "toggle", "more", "model", "state"]
        );
        // Re-entering Work changes nothing: there is one Workshop surface.
        app.update(window, |a, cx| {
            a.set_mode(Mode::Work, cx);
            cx.notify();
        });
        window.run_until_parked();
        assert_eq!(tab_cycle(&app, window), 23);
    }

    #[gpui::test]
    fn u3e1_approval_pills_are_tab_stops(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::u3e1_approval_pills_are_tab_stops") {
            return;
        }
        let (app, window) = app_window(cx);
        let _rx = pending_card(&app, window, awili_config::EngineKind::Awili);
        assert!(window.debug_bounds("ui-pill-job-activity-island").is_some());
        let with_card = tab_cycle(&app, window);
        app.update(window, |a, cx| {
            a.work.panel.approval = None;
            cx.notify();
        });
        window.run_until_parked();
        // Allow once, Allow for session, Allow always and Deny; "Allow for
        // folder" is disabled (no bound folder), so it is no tab stop.
        // Four decision pills and the island surface/Review enter the cycle.
        // The model control stays a stop: choosing the next turn's target is
        // allowed while an approval is pending.
        assert!(window.debug_bounds("ui-pill-job-activity-island").is_none());
        assert_eq!(with_card - tab_cycle(&app, window), 6);
    }

    #[gpui::test]
    fn u3e1_tab_with_marked_text_stays_in_composer(cx: &mut gpui::TestAppContext) {
        if !isolated("focus_nav::tests::u3e1_tab_with_marked_text_stays_in_composer") {
            return;
        }
        let (app, window) = app_window(cx);
        let composer = app.read_with(window, |a, _| a.composer.clone());
        composer.update_in(window, |c, window, cx| {
            c.replace_and_mark_text_in_range(None, "か", None, window, cx)
        });
        window.simulate_keystrokes("tab");
        assert!(
            composer_focused(&app, window),
            "IME candidate navigation keeps Tab"
        );
    }

    #[gpui::test]
    fn u3e1_composer_is_tab_stop(cx: &mut gpui::TestAppContext) {
        let composer = cx.new(crate::composer::Composer::new);
        let handle = composer.read_with(cx, |c, _| c.focus_handle().clone());
        assert!(handle.tab_stop);
        assert_eq!(handle.tab_index, COMPOSER);
    }

    /// RC2-R611 (usability RC2 round 5, Minor 10): the Send button's ring
    /// was "only a faint lighter edge on the blue button", and Tab skipped
    /// "Ask before changes", the mic and the speaker. On a blue button the
    /// ring is a light edge with a blue halo outside it; in Work, Tab from
    /// the box walks the helper chip, the Ask chip, the mic, the speaker,
    /// Send and the five navigation stops, and comes back.
    #[gpui::test]
    fn rc2r611_tab_reaches_every_composer_control_and_send_shows_its_ring(
        cx: &mut gpui::TestAppContext,
    ) {
        let on_accent = ring_on_accent(StyleRefinement::default());
        assert_eq!(
            on_accent.border_color,
            Some(crate::theme::p().on_accent.into()),
            "a light edge, not blue on blue"
        );
        let halo = on_accent.box_shadow.clone().expect("a halo");
        assert_eq!(halo.len(), 1);
        assert!(halo[0].spread_radius > gpui::px(0.) && !halo[0].inset);
        assert_eq!(halo[0].color, focus_ring_color().into());
        if !isolated(
            "focus_nav::tests::rc2r611_tab_reaches_every_composer_control_and_send_shows_its_ring",
        ) {
            return;
        }
        let (app, window) = app_window(cx);
        app.update(window, |a, cx| {
            a.set_mode(Mode::Work, cx);
            cx.notify();
        });
        window.run_until_parked();
        // Includes the bar's project, title, model, mode, New, toggle and more.
        assert_eq!(tab_cycle(&app, window), 23);
        // The speaker's Stop, while Awili reads aloud, is one more.
        app.update(window, |a, cx| {
            a.voice.speaking = true;
            cx.notify();
        });
        window.run_until_parked();
        assert_eq!(tab_cycle(&app, window), 24);
    }

    #[test]
    fn u3e1_ring_only_recolours_the_border() {
        let ringed = ring(StyleRefinement::default());
        let want = StyleRefinement {
            border_color: Some(focus_ring_color().into()),
            ..Default::default()
        };
        assert_eq!(ringed, want);
    }
}
