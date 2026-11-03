//! Sends that cross harnesses in one conversation, over the real app.
//!
//! One Workshop conversation, a real native Code tab backed by a fake
//! `claude` that records its argv, and a real gateway worker loop over the
//! scripted fake (`FakeOps`). Everything goes through the app's own seams: the
//! composer's submit, the chooser's setter (`set_composer_target`), Stop, the
//! worker's events, and the plan card's Approve. Nothing calls a helper the
//! production code uses to capture an owner; they are exercised by the turns.
//!
//! Proved:
//! 1. A conversation with a native tab runs a Gateway turn; Stop is draining;
//!    the NEXT target is a native harness and Send PARKS (no native child, no
//!    row). The parked prompt keeps the target it was accepted under while a
//!    later NEXT pick is kept for the turn after; the replay runs once.
//! 2. A Code-tab send while the same conversation's Work turn is live starts no
//!    parallel native writer; the typed text, the staged file and the native
//!    backing are all still there. A Code->In-app/Gateway send while ANOTHER
//!    conversation's Work turn is live converts nothing, navigates nowhere and
//!    is not accepted.
//! 3. An open Code plan is superseded by an accepted Work message in the same
//!    conversation (its approval is then inert); an ordinary NEXT pick alone
//!    keeps the plan, and the plan's own Approve still runs Autonomous on its
//!    captured owner; explicit Ask/Auto survive the trip across harnesses.
//!
//! Limits: the Work adapter used for the non-native send is the Gateway
//! (In-app needs a live model endpoint to pass its health probe); both reach
//! the same submit path. The held memory gate is not involved (native Code
//! turns have none; Gateway keeps its own memory).

use super::*;
use crate::ask_mode::AskKey;
use crate::work::gateway_queue_tests::{pump, DRAIN};
use crate::work::gateway_turn_tests::{complete, delta, running, spawn_loop, stopped};
use crate::work::tests::FakeOps;
use crate::workshop::Backing;
use crate::workshop_composer::ModelOwner;
use awili_work::ApprovalMode;

/// The app on a native tab that already ran one turn, plus the fake harness.
struct Rig {
    app: gpui::Entity<AwiliApp>,
    bins: tempfile::TempDir,
    _folder: tempfile::TempDir,
    tab_id: u64,
    conv: String,
}

impl Rig {
    /// `plan_first`: the first native turn ends with a plan card (Ask posture).
    fn new(cx: &mut gpui::TestAppContext, home: &Path, plan_first: bool) -> Self {
        let bins = tempfile::tempdir().unwrap();
        let folder = tempfile::tempdir().unwrap();
        let claude_bin = fake_claude(bins.path());
        if plan_first {
            std::fs::write(bins.path().join("plan_1"), "").unwrap();
        }
        let app = cx.new(AwiliApp::new);
        app.update(cx, |a, cx| {
            a.onboarding = crate::onboarding::Onboarding::existing();
            a.fleet_pack = None;
            a.memory.enabled = false;
            a.mode = Mode::Work;
            a.work.agents.claude_bin = Some(claude_bin.display().to_string());
            for bin in [
                &mut a.work.agents.codex_bin,
                &mut a.work.agents.zcode_bin,
                &mut a.work.agents.dsh_bin,
            ] {
                *bin = Some(bins.path().join("not-installed").display().to_string());
            }
            a.work.folder = Some(folder.path().to_path_buf());
            a.work
                .graduation
                .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
            // A real "New" on a native harness: a fresh native tab.
            a.new_workshop_from_default(claude(None), cx);
        });
        let mut rig = Self {
            app,
            bins,
            _folder: folder,
            tab_id: 0,
            conv: String::new(),
        };
        rig.send(cx, "first");
        let tab_id = rig
            .app
            .read_with(cx, |a, _| a.work.code_tabs[a.work.code_active].id);
        rig.tab_id = tab_id;
        rig.native_done(cx, 1);
        rig.conv = rig.app.read_with(cx, |a, _| {
            a.work
                .code_tabs
                .iter()
                .find(|t| t.id == tab_id)
                .and_then(|t| t.history.conv_id.clone())
                .expect("the first send bound the tab to a conversation")
        });
        rig
    }

    fn send(&self, cx: &mut gpui::TestAppContext, text: &str) {
        self.app.update(cx, |a, cx| {
            a.composer.update(cx, |c, cx| c.set_text(text, cx));
            a.submit_composer_draft(cx);
        });
    }

    /// Wait until the native harness has been launched `n` times and its tab
    /// is idle with an answer.
    fn native_done(&self, cx: &mut gpui::TestAppContext, n: usize) {
        let tab_id = self.tab_id;
        let dir = self.bins.path().to_path_buf();
        settle(cx, &self.app, "the native turn", |a| {
            calls(&dir).len() == n
                && a.work
                    .code_tabs
                    .iter()
                    .find(|t| t.id == tab_id)
                    .is_some_and(|t| {
                        !t.streaming
                            && t.convo
                                .messages()
                                .last()
                                .is_some_and(|m| m.role == Role::Assistant && !m.text.is_empty())
                    })
        });
    }

    fn choose_native(&self, cx: &mut gpui::TestAppContext, target: WorkExecutor) {
        let owner = ModelOwner::Native(self.tab_id);
        assert!(self
            .app
            .update(cx, |a, cx| a.set_composer_target(&owner, target, cx)));
    }

    fn choose_work(&self, cx: &mut gpui::TestAppContext, target: WorkExecutor) {
        let owner = ModelOwner::Work(self.conv.clone());
        assert!(self
            .app
            .update(cx, |a, cx| a.set_composer_target(&owner, target, cx)));
    }

    fn next(&self, cx: &mut gpui::TestAppContext) -> Option<WorkExecutor> {
        let owner = ModelOwner::Work(self.conv.clone());
        self.app.read_with(cx, |a, _| a.composer_target_for(&owner))
    }

    /// Back to the native tab, the way the user clicks it.
    fn show_native(&self, cx: &mut gpui::TestAppContext) {
        let tab_id = self.tab_id;
        self.app.update(cx, |a, cx| {
            a.set_mode(Mode::Code, cx);
            let ix = a
                .work
                .code_tabs
                .iter()
                .position(|t| t.id == tab_id)
                .unwrap();
            a.activate_code_tab(ix);
        });
    }

    fn plan_open(&self, cx: &mut gpui::TestAppContext) -> bool {
        let key = AskKey::CodeTab(self.tab_id);
        self.app
            .read_with(cx, |a, _| a.work.plan_gates.open_for(&key).is_some())
    }

    /// A real gateway worker over `fake`, its `Started` applied.
    fn attach_gateway(
        &self,
        cx: &mut gpui::TestAppContext,
        fake: &FakeOps,
    ) -> (
        async_channel::Receiver<WorkEvent>,
        std::thread::JoinHandle<()>,
        u64,
    ) {
        self.app.update(cx, |a, cx| {
            a.work.gateway.enabled = true;
            a.work.engine.kind = awili_config::EngineKind::HermesGateway;
            let (cmd_tx, ev_rx, handle) = spawn_loop(fake, DRAIN, vec![]);
            a.work.gateway_cmd_tx = Some(cmd_tx);
            a.work.worker_identity = Some(WorkRunIdentity::HermesGateway);
            let epoch = a.work.run_seq;
            pump(a, &ev_rx, epoch, |e| matches!(e, WorkEvent::Started(_)), cx);
            (ev_rx, handle, epoch)
        })
    }

    fn pump_until(
        &self,
        cx: &mut gpui::TestAppContext,
        rx: &async_channel::Receiver<WorkEvent>,
        epoch: u64,
        done: impl Fn(&WorkEvent) -> bool,
    ) {
        self.app.update(cx, |a, cx| pump(a, rx, epoch, done, cx));
    }

    fn close_gateway(&self, cx: &mut gpui::TestAppContext, handle: std::thread::JoinHandle<()>) {
        self.app.update(cx, |a, _| a.work.gateway_cmd_tx = None);
        handle.join().unwrap();
    }

    fn user_rows(&self, cx: &mut gpui::TestAppContext) -> Vec<String> {
        let conv = self.conv.clone();
        self.app.read_with(cx, |a, _| {
            a.views[&conv]
                .convo
                .messages()
                .iter()
                .filter(|m| m.role == Role::User)
                .map(|m| m.text.clone())
                .collect()
        })
    }
}

/// A turn on the gateway worker that stays live until it is stopped; the
/// Stop's acknowledgement is what ends it.
fn live_gateway_turn() -> FakeOps {
    let mut fake = FakeOps::new(vec![vec![running(true), delta("gw ")]]);
    fake.interrupt_events = vec![complete(), running(false)];
    fake
}

/// Send a gateway turn from the native tab (NEXT is Gateway), until it streams.
fn start_gateway_turn(
    rig: &Rig,
    cx: &mut gpui::TestAppContext,
    rx: &async_channel::Receiver<WorkEvent>,
    epoch: u64,
) {
    rig.choose_native(cx, WorkExecutor::Gateway);
    rig.send(cx, "gateway turn");
    rig.pump_until(cx, rx, epoch, |e| *e == WorkEvent::Token("gw ".into()));
    rig.app.read_with(cx, |a, _| {
        assert!(a.work.running, "the gateway turn is live");
        assert_eq!(a.mode, Mode::Work, "the send moved to the Work adapter");
        assert_eq!(a.work.conv_id.as_deref(), Some(rig.conv.as_str()));
    });
}

// ---- 1. Stop is draining; NEXT is native; Send parks --------------------------

/// The conversation has a native tab (so a native NEXT diverts to it), its
/// Gateway turn is stopped and still draining, and NEXT is a native harness.
/// Send must park: no native child, no row, the accepted target captured. A
/// later NEXT pick is kept for the turn after the replay.
#[gpui::test]
fn a_native_send_behind_a_gateway_stop_parks_on_its_accepted_target(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gate_follow_tests::send_conflicts::a_native_send_behind_a_gateway_stop_parks_on_its_accepted_target",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let rig = Rig::new(cx, &home, false);
    let fake = live_gateway_turn();
    let (rx, handle, epoch) = rig.attach_gateway(cx, &fake);
    start_gateway_turn(&rig, cx, &rx, epoch);
    let rows_before = rig.user_rows(cx);

    // Stop: the drain is pending, nothing is running.
    rig.app.update(cx, |a, cx| {
        a.cancel_work_run(cx);
        assert!(a.work.gateway_stop_pending);
        assert!(!a.work.running);
    });

    // NEXT is native; Send parks.
    let accepted = claude(Some("accepted-model"));
    rig.choose_work(cx, accepted.clone());
    rig.send(cx, "after the stop");
    rig.app.read_with(cx, |a, _| {
        let parked = a
            .work
            .queued_after_stop
            .as_ref()
            .expect("the send is parked behind the Stop");
        assert_eq!(parked.text, "after the stop");
        assert_eq!(parked.conv_id, rig.conv);
        assert_eq!(parked.executor, accepted, "the accepted target is captured");
        assert!(
            a.work.code_tabs.iter().all(|t| !t.streaming),
            "no native turn beside the draining gateway turn"
        );
    });
    assert_eq!(
        calls(rig.bins.path()).len(),
        1,
        "no native child was started"
    );
    assert_eq!(
        rig.user_rows(cx),
        rows_before,
        "the parked text is not a row yet"
    );

    // The owner changes NEXT while it waits: only the turn after is affected.
    rig.choose_work(cx, claude(Some("later-model")));
    rig.pump_until(cx, &rx, epoch, |e| *e == stopped());
    rig.app
        .read_with(cx, |a, _| assert!(a.work.queued_after_stop.is_none()));
    rig.native_done(cx, 2);

    let all = calls(rig.bins.path());
    assert_eq!(all.len(), 2, "the replay started exactly one native child");
    assert!(
        has_pair(&all[1], "--model", "accepted-model"),
        "the parked prompt ran on its accepted target: {:?}",
        all[1]
    );
    assert!(
        all[1].iter().any(|a| a.contains("after the stop")),
        "{:?}",
        all[1]
    );
    assert_eq!(
        rig.next(cx),
        Some(claude(Some("later-model"))),
        "the later pick is still the NEXT target"
    );
    assert_eq!(
        rig.user_rows(cx)
            .iter()
            .filter(|t| *t == "after the stop")
            .count(),
        1,
        "the parked prompt is in the transcript exactly once"
    );
    rig.close_gateway(cx, handle);
}

// ---- 2. No parallel writer; no false conversion --------------------------------

/// The same conversation's Gateway turn is live and the user sends from its
/// native tab: no native child starts beside it. The typed text and the staged
/// file are still there and the tab still has its native backing.
#[gpui::test]
fn a_code_send_beside_a_live_work_turn_in_the_same_conversation_starts_no_second_writer(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::gate_follow_tests::send_conflicts::a_code_send_beside_a_live_work_turn_in_the_same_conversation_starts_no_second_writer",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let rig = Rig::new(cx, &home, false);
    let fake = live_gateway_turn();
    let (rx, handle, epoch) = rig.attach_gateway(cx, &fake);
    start_gateway_turn(&rig, cx, &rx, epoch);

    // Back on the native tab, with a native target and a staged file.
    rig.show_native(cx);
    rig.choose_native(cx, claude(Some("parallel-model")));
    rig.app.update(cx, |a, cx| {
        a.attach_bytes("keep.txt", "text/plain", b"KEEP-ME", cx)
    });
    let rows_before = rig.app.read_with(cx, |a, _| {
        a.work
            .code_tabs
            .iter()
            .find(|t| t.id == rig.tab_id)
            .map(|t| t.convo.messages().len())
            .unwrap()
    });
    let tabs_before = rig.app.read_with(cx, |a, _| a.shell.tabs.tabs.len());
    rig.send(cx, "write in parallel");

    rig.app.read_with(cx, |a, cx| {
        let tab = a
            .work
            .code_tabs
            .iter()
            .find(|t| t.id == rig.tab_id)
            .unwrap();
        assert!(!tab.streaming, "no native turn started");
        assert_eq!(
            tab.convo.messages().len(),
            rows_before,
            "nothing was appended"
        );
        assert!(a.work.running, "the gateway turn is untouched");
        assert_eq!(
            a.composer.read(cx).text(),
            "write in parallel",
            "the typed prompt is back in the box"
        );
        assert_eq!(
            a.staged_attachments
                .iter()
                .map(|f| f.filename.as_str())
                .collect::<Vec<_>>(),
            ["keep.txt"],
            "the staged file is kept"
        );
        assert_eq!(
            a.shell.tabs.tabs.len(),
            tabs_before,
            "no Workshop tab was added or lost"
        );
        assert!(
            a.shell
                .tabs
                .tabs
                .iter()
                .any(|t| t.backing == Backing::Agent(rig.tab_id)
                    || t.backing == Backing::Conversation(rig.conv.clone())),
            "the thread keeps its Workshop tab"
        );
    });
    assert_eq!(calls(rig.bins.path()).len(), 1, "no second native child");

    // Clean up: stop the gateway turn and let its acknowledgement land.
    rig.app.update(cx, |a, cx| a.cancel_work_run(cx));
    rig.pump_until(cx, &rx, epoch, |e| *e == stopped());
    rig.close_gateway(cx, handle);
}

/// Another conversation's Work turn is live. From a native tab, choosing the
/// Gateway and sending must not convert the tab's backing, navigate to Work, or
/// be treated as accepted: the text and file stay put and no row is written.
#[gpui::test]
fn a_code_send_to_the_work_adapter_while_another_conversation_is_busy_converts_nothing(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::gate_follow_tests::send_conflicts::a_code_send_to_the_work_adapter_while_another_conversation_is_busy_converts_nothing",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let rig = Rig::new(cx, &home, false);

    // Another Work conversation runs a live gateway turn.
    let other = rig.app.update(cx, |a, cx| {
        let (id, _) = a.fresh_conversation_with(awili_core::ConvKind::Work, None);
        a.open_conversation_from_history(&id, cx);
        id
    });
    assert_ne!(other, rig.conv, "setup: a different conversation");
    let fake = live_gateway_turn();
    let (rx, handle, epoch) = rig.attach_gateway(cx, &fake);
    rig.app.update(cx, |a, _| {
        // A fresh gateway send from the Work composer is health-checked first
        // (`submit_work_gated`): with the check unknown it parks the text
        // behind a real HTTP probe of the dashboard, which no fake serves, and
        // nothing reaches the worker. The scripted worker IS reachable, so
        // record that through the gate's own state machine.
        assert!(a.work.health.gateway.begin_probe());
        a.work
            .health
            .gateway
            .finish_probe(Ok(()), std::time::Instant::now());
    });
    let other_owner = ModelOwner::Work(other.clone());
    assert!(rig.app.update(cx, |a, cx| {
        a.set_composer_target(&other_owner, WorkExecutor::Gateway, cx)
    }));
    rig.app.read_with(cx, |a, _| {
        assert_eq!(a.mode, Mode::Work, "setup: the other thread is shown");
        assert_eq!(a.sidebar.active_id(), other);
    });
    rig.send(cx, "the other thread's job");
    rig.pump_until(cx, &rx, epoch, |e| *e == WorkEvent::Token("gw ".into()));
    rig.app.read_with(cx, |a, _| {
        assert!(a.work.running);
        assert_eq!(a.work.conv_id.as_deref(), Some(other.as_str()));
    });

    // The native tab asks for the Gateway and sends.
    rig.show_native(cx);
    rig.choose_native(cx, WorkExecutor::Gateway);
    rig.app.update(cx, |a, cx| {
        a.attach_bytes("keep.txt", "text/plain", b"KEEP-ME", cx)
    });
    let before = rig.app.read_with(cx, |a, _| {
        (
            a.sidebar.active_id().to_string(),
            a.shell
                .tabs
                .tabs
                .iter()
                .map(|t| t.backing.clone())
                .collect::<Vec<_>>(),
            a.views[&rig.conv].convo.messages().len(),
        )
    });
    assert!(
        before.1.contains(&Backing::Agent(rig.tab_id)),
        "setup: the tab has its native backing: {:?}",
        before.1
    );
    rig.send(cx, "move to the gateway");

    rig.app.read_with(cx, |a, cx| {
        assert_eq!(a.mode, Mode::Code, "the app did not navigate to Work");
        assert_eq!(
            a.sidebar.active_id(),
            before.0,
            "the shown thread did not change"
        );
        assert_eq!(
            a.shell
                .tabs
                .tabs
                .iter()
                .map(|t| t.backing.clone())
                .collect::<Vec<_>>(),
            before.1,
            "the native tab was not converted"
        );
        assert_eq!(
            a.views[&rig.conv].convo.messages().len(),
            before.2,
            "no row was written for a refused send"
        );
        assert_eq!(a.work.conv_id.as_deref(), Some(other.as_str()));
        assert_eq!(a.composer.read(cx).text(), "move to the gateway");
        assert_eq!(a.staged_attachments.len(), 1, "the staged file is kept");
        assert!(a.work.running, "the other conversation's job is untouched");
    });

    rig.app.update(cx, |a, cx| a.cancel_work_run(cx));
    rig.pump_until(cx, &rx, epoch, |e| *e == stopped());
    rig.close_gateway(cx, handle);
}

// ---- 3. Plans and postures across harnesses ------------------------------------

/// An open Code plan, then an ordinary NEXT pick: the plan is kept. An accepted
/// Work message in the same conversation then supersedes it; back on the Code
/// tab the card is gone and its Approve is inert (no native child).
#[gpui::test]
fn an_accepted_work_message_supersedes_the_open_code_plan(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gate_follow_tests::send_conflicts::an_accepted_work_message_supersedes_the_open_code_plan",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let rig = Rig::new(cx, &home, true);
    assert!(
        rig.plan_open(cx),
        "setup: the planning turn left a plan card"
    );

    // Choosing another harness alone does not touch the plan.
    rig.choose_native(cx, WorkExecutor::Gateway);
    assert!(rig.plan_open(cx), "a NEXT pick keeps the plan");

    // An accepted gateway message in the same conversation replaces it.
    let fake = FakeOps::new(vec![vec![
        running(true),
        delta("ok"),
        complete(),
        running(false),
    ]]);
    let (rx, handle, epoch) = rig.attach_gateway(cx, &fake);
    rig.send(cx, "never mind, answer instead");
    rig.pump_until(cx, &rx, epoch, |e| *e == WorkEvent::TurnComplete);
    assert!(
        rig.user_rows(cx)
            .contains(&"never mind, answer instead".to_string()),
        "the message was accepted into the conversation"
    );
    assert!(!rig.plan_open(cx), "the older Code plan was superseded");

    // Back on the tab, the old approval does nothing.
    rig.show_native(cx);
    rig.app
        .update(cx, |a, cx| a.resolve_code_plan_gate(true, cx));
    cx.run_until_parked();
    rig.app.read_with(cx, |a, _| {
        let tab = a
            .work
            .code_tabs
            .iter()
            .find(|t| t.id == rig.tab_id)
            .unwrap();
        assert!(!tab.streaming, "a superseded approval starts no turn");
    });
    assert_eq!(calls(rig.bins.path()).len(), 1, "no native child from it");
    assert!(
        !rig.user_rows(cx)
            .contains(&awili_code::PLAN_APPROVED_TEXT.to_string()),
        "no approval row was written"
    );
    rig.close_gateway(cx, handle);
}

/// The control for the above: with the plan open, an ordinary NEXT pick of
/// another model changes nothing about it. Approve still runs the plan's own
/// owner Autonomous, resuming its session, and NEXT stays what was chosen.
#[gpui::test]
fn a_next_pick_alone_keeps_the_code_plan_and_approve_still_runs_autonomous(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::gate_follow_tests::send_conflicts::a_next_pick_alone_keeps_the_code_plan_and_approve_still_runs_autonomous",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let rig = Rig::new(cx, &home, true);
    assert!(rig.plan_open(cx), "setup: a plan card");
    let first = calls(rig.bins.path());
    assert!(
        has_pair(&first[0], "--permission-mode", "plan"),
        "{:?}",
        first[0]
    );

    rig.choose_native(cx, claude(Some("next-model")));
    assert!(rig.plan_open(cx), "the pick left the plan open");
    rig.app
        .update(cx, |a, cx| a.resolve_code_plan_gate(true, cx));
    rig.native_done(cx, 2);

    let all = calls(rig.bins.path());
    assert!(
        !has(&all[1], "--permission-mode"),
        "the approved turn has full access: {:?}",
        all[1]
    );
    assert!(has_pair(&all[1], "--resume", "sess-1"), "{:?}", all[1]);
    assert!(
        !has_pair(&all[1], "--model", "next-model"),
        "the approval ran the plan's own owner, not the picked model: {:?}",
        all[1]
    );
    assert!(!rig.plan_open(cx), "the card is consumed");
    assert_eq!(
        rig.app.read_with(cx, |a, _| {
            a.composer_target_for(&ModelOwner::Native(rig.tab_id))
        }),
        Some(claude(Some("next-model"))),
        "the pick is still the next target"
    );
}

/// An explicit Auto set on the native tab arrives on the Work side after a
/// gateway send; an explicit Ask set there then arrives back on the tab and
/// shapes the next native launch (`--permission-mode plan`, no skip flag).
#[gpui::test]
fn explicit_ask_and_auto_survive_the_trip_across_harnesses(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::gate_follow_tests::send_conflicts::explicit_ask_and_auto_survive_the_trip_across_harnesses",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let rig = Rig::new(cx, &home, false);

    rig.app.update(cx, |a, cx| {
        a.set_ask_mode(AskKey::CodeTab(rig.tab_id), ApprovalMode::Auto, cx)
    });
    rig.choose_native(cx, WorkExecutor::Gateway);
    let fake = FakeOps::new(vec![vec![
        running(true),
        delta("ok"),
        complete(),
        running(false),
    ]]);
    let (rx, handle, epoch) = rig.attach_gateway(cx, &fake);
    rig.send(cx, "gateway turn");
    rig.pump_until(cx, &rx, epoch, |e| *e == WorkEvent::TurnComplete);
    rig.app.read_with(cx, |a, _| {
        assert_eq!(
            a.work.ask_modes.mode_for(&AskKey::Work(rig.conv.clone())),
            ApprovalMode::Auto,
            "Auto arrived on the Work side"
        );
    });

    // Ask, set explicitly on the Work side, comes back with the next native send.
    rig.app.update(cx, |a, cx| {
        a.set_ask_mode(
            AskKey::Work(rig.conv.clone()),
            ApprovalMode::AskBeforeChanges,
            cx,
        )
    });
    rig.choose_work(cx, claude(Some("again")));
    rig.send(cx, "native again");
    rig.native_done(cx, 2);
    let all = calls(rig.bins.path());
    assert!(
        has_pair(&all[1], "--permission-mode", "plan"),
        "the explicit Ask shaped the native launch: {:?}",
        all[1]
    );
    assert!(
        !has(&all[1], "--dangerously-skip-permissions"),
        "{:?}",
        all[1]
    );
    rig.app.read_with(cx, |a, _| {
        assert_eq!(
            a.work.ask_modes.mode_for(&AskKey::CodeTab(rig.tab_id)),
            ApprovalMode::AskBeforeChanges,
            "Ask arrived back on the tab"
        );
    });
    rig.close_gateway(cx, handle);
}

/// Another conversation's Gateway turn was stopped and is still draining
/// (`gateway_stop_pending`, its acknowledgement not yet pumped). A native turn
/// on THIS conversation runs beside Work by policy, so the Code plan's Approve
/// must not be parked behind that unrelated drain: it launches at once as the
/// plan's own owner, Autonomous, resuming the plan's session, and the NEXT pick
/// is kept. The other conversation's run, owner and drain are untouched until
/// its own acknowledgement.
#[gpui::test]
fn a_code_plan_approval_is_not_parked_behind_another_conversations_gateway_stop(
    cx: &mut gpui::TestAppContext,
) {
    let Some(home) = isolated(
        "work::gate_follow_tests::send_conflicts::a_code_plan_approval_is_not_parked_behind_another_conversations_gateway_stop",
    ) else {
        return;
    };
    cx.executor().allow_parking();
    let rig = Rig::new(cx, &home, true);
    assert!(
        rig.plan_open(cx),
        "setup: the planning turn left a plan card"
    );

    // Another Work conversation runs a live gateway turn (fresh health, so the
    // composer's send reaches the worker).
    let other = rig.app.update(cx, |a, cx| {
        let (id, _) = a.fresh_conversation_with(awili_core::ConvKind::Work, None);
        a.open_conversation_from_history(&id, cx);
        id
    });
    assert_ne!(other, rig.conv, "setup: a different conversation");
    let fake = live_gateway_turn();
    let (rx, handle, epoch) = rig.attach_gateway(cx, &fake);
    rig.app.update(cx, |a, _| {
        assert!(a.work.health.gateway.begin_probe());
        a.work
            .health
            .gateway
            .finish_probe(Ok(()), std::time::Instant::now());
    });
    let other_owner = ModelOwner::Work(other.clone());
    assert!(rig.app.update(cx, |a, cx| {
        a.set_composer_target(&other_owner, WorkExecutor::Gateway, cx)
    }));
    rig.send(cx, "the other thread's job");
    rig.pump_until(cx, &rx, epoch, |e| *e == WorkEvent::Token("gw ".into()));

    // Stop it, and do NOT deliver the acknowledgement.
    rig.app.update(cx, |a, cx| {
        assert_eq!(a.work.conv_id.as_deref(), Some(other.as_str()));
        a.cancel_work_run(cx);
        assert!(a.work.gateway_stop_pending);
        assert!(!a.work.running);
    });

    // Back on the native tab: pick another model for the NEXT turn, approve.
    rig.show_native(cx);
    rig.choose_native(cx, claude(Some("next-model")));
    assert!(rig.plan_open(cx), "the pick left the plan open");
    let (run_seq, owner) = rig.app.read_with(cx, |a, _| {
        assert!(
            a.work.gateway_stop_pending,
            "setup: the other drain is pending"
        );
        (a.work.run_seq, a.work.conv_id.clone())
    });
    rig.app
        .update(cx, |a, cx| a.resolve_code_plan_gate(true, cx));
    rig.native_done(cx, 2);

    let all = calls(rig.bins.path());
    assert_eq!(
        all.len(),
        2,
        "the approval launched exactly one native child"
    );
    assert!(
        !has(&all[1], "--permission-mode"),
        "the approved turn has full access: {:?}",
        all[1]
    );
    assert!(has_pair(&all[1], "--resume", "sess-1"), "{:?}", all[1]);
    assert!(
        !has_pair(&all[1], "--model", "next-model"),
        "the plan's own owner ran, not the picked model: {:?}",
        all[1]
    );
    rig.app.read_with(cx, |a, _| {
        assert!(
            a.work.queued_after_stop.is_none(),
            "the approval was not parked behind the other conversation's Stop"
        );
        assert!(a.work.gateway_stop_pending, "the other drain is untouched");
        assert!(!a.work.running);
        assert_eq!(a.work.conv_id, owner, "the Work run's owner is unchanged");
        assert_eq!(a.work.run_seq, run_seq, "no Work run was started");
        assert_eq!(
            a.composer_target_for(&ModelOwner::Native(rig.tab_id)),
            Some(claude(Some("next-model"))),
            "the pick is still the next target"
        );
    });
    assert!(!rig.plan_open(cx), "the card is consumed");

    // The other conversation's own acknowledgement ends its drain.
    rig.pump_until(cx, &rx, epoch, |e| *e == stopped());
    rig.app
        .read_with(cx, |a, _| assert!(!a.work.gateway_stop_pending));
    rig.close_gateway(cx, handle);
}
