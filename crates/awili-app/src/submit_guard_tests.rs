//! R1a proof tests: each runs the real `AwiliApp` (built by `AwiliApp::new`
//! in a subprocess with a private HOME, the U2c-FIX1 isolation) and drives
//! the composer submit exactly as Enter does (`submit_composer_draft`).

use std::time::Duration;

use awili_code::CodeEngine;
use gpui::AppContext;

use super::*;
use crate::work::WorkExecutor;
use crate::Mode;

const R1A_CHILD: &str = "AWILI_R1A_TEST_HOME";
const R1A_CONV: &str = "r1a-work-conv";
const DRAFT: &str = "Please look at the notes in this folder. I want to plan a small trip. \
                      Summarise them into one list.";

/// Run `test` in a subprocess with a private HOME. `true` inside the child
/// (run the body); `false` in the parent once the child passed.
fn in_isolated_child(test: &str) -> bool {
    if std::env::var_os(R1A_CHILD).is_some() {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(R1A_CHILD, home.path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(test) && stdout.contains("1 passed"),
        "the subprocess must actually run the test: {stdout}"
    );
    false
}

/// A Work conversation on an agent executor (no gateway gate), bound to
/// `folder`. `with_view: false` leaves the conversation without a loaded
/// transcript (a deleted conversation).
fn work_conversation(
    app: &mut crate::AwiliApp,
    folder: Option<std::path::PathBuf>,
    with_view: bool,
    _cx: &mut gpui::Context<crate::AwiliApp>,
) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.mode = Mode::Work;
    app.work.folder = folder;
    app.work.executor = WorkExecutor::Agent {
        engine: CodeEngine::Claude,
        model: None,
    };
    app.views.remove(R1A_CONV);
    if with_view {
        app.views.insert(
            R1A_CONV.into(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
    }
    app.set_active_conversation(R1A_CONV);
    // These fixtures exercise the job submit guard, including keyboard Enter.
}

/// The Work transcript's message texts (empty when there is no view).
fn transcript(app: &crate::AwiliApp) -> Vec<String> {
    app.views
        .get(R1A_CONV)
        .map(|v| v.convo.messages().iter().map(|m| m.text.clone()).collect())
        .unwrap_or_default()
}

fn toast(app: &crate::AwiliApp) -> String {
    app.toast
        .msg
        .clone()
        .map(|m| m.to_string())
        .unwrap_or_default()
}

/// Blocker 1 (B6): the Work folder was moved away. Enter keeps the full text
/// in the composer, records nothing, starts nothing, and a toast that does
/// not time out says why and what to do (folder named, no path).
#[gpui::test]
fn r1a_missing_folder_keeps_text_and_says_why(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("submit_guard::tests::r1a_missing_folder_keeps_text_and_says_why") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("b6");
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_conversation(app, Some(gone.clone()), true, cx);
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert_eq!(
            app.composer.read(cx).text(),
            DRAFT,
            "the full text is back in the box"
        );
        assert!(transcript(app).is_empty(), "nothing recorded");
        assert!(!app.work.running, "nothing started");
        assert!(app.submit_guard.is_none(), "the guard is settled");
        let t = toast(app);
        assert!(t.contains("\u{201C}b6\u{201D} is no longer there"), "{t}");
        assert!(
            t.contains("still in the box") && t.contains("Choose folder"),
            "{t}"
        );
        assert!(!t.contains('/'), "path-free: {t}");
    });
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(
            toast(app).contains("is no longer there"),
            "the refusal stays on screen until dismissed"
        );
    });
}

/// Blocker 1, the shared net: a path that returns without taking the text
/// and without a reason (a prompt parked behind a Stop for a conversation
/// that no longer exists) still hands the text back, with the fallback
/// notice.
#[gpui::test]
fn r1a_untaken_submit_comes_back_with_a_reason(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("submit_guard::tests::r1a_untaken_submit_comes_back_with_a_reason") {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_conversation(app, None, false, cx);
        app.work.gateway_stop_pending = true;
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert!(app.work.queued_after_stop.is_none(), "nothing parked");
        assert_eq!(app.composer.read(cx).text(), DRAFT);
        assert_eq!(toast(app), NOT_STARTED_NOTICE);
    });
}

/// Control: a submit that IS taken (parked behind a pending Stop for a live
/// conversation) leaves the composer empty and shows no refusal.
#[gpui::test]
fn r1a_taken_submit_is_not_handed_back(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("submit_guard::tests::r1a_taken_submit_is_not_handed_back") {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_conversation(app, None, true, cx);
        app.work.gateway_stop_pending = true;
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert_eq!(
            app.work.queued_after_stop.as_ref().map(|q| q.text.as_str()),
            Some(DRAFT),
            "parked for the replay"
        );
        assert!(
            app.composer.read(cx).text().is_empty(),
            "taken, not handed back"
        );
        // M1a announces the park; that notice is the only toast, never a refusal.
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(crate::gateway_stop_deadline::PARKED_TOAST),
            "the park notice, not a refusal"
        );
        assert!(app.submit_guard.is_none());
    });
}

/// Control: a Work submit that is sent (the gateway lane, known up) is in
/// the transcript, reaches the worker once, and is NOT handed back.
#[gpui::test]
fn r1a_sent_work_submit_is_not_handed_back(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("submit_guard::tests::r1a_sent_work_submit_is_not_handed_back") {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_conversation(app, None, true, cx);
        app.work.gateway.enabled = true;
        app.work.engine.kind = awili_config::EngineKind::HermesGateway;
        app.work.executor = WorkExecutor::Gateway;
        let (tx, rx) = std::sync::mpsc::channel();
        app.work.gateway_cmd_tx = Some(tx);
        app.work.worker_identity = Some(crate::work_run_identity::WorkRunIdentity::HermesGateway);
        assert!(app.work.health.gateway.begin_probe());
        app.work
            .health
            .gateway
            .finish_probe(Ok(()), std::time::Instant::now());
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert!(
            matches!(rx.try_recv(), Ok(crate::work::GatewayCmd::Prompt { .. })),
            "sent to the worker"
        );
        assert!(rx.try_recv().is_err(), "sent once");
        assert_eq!(transcript(app).first().map(String::as_str), Some(DRAFT));
        assert!(
            app.composer.read(cx).text().is_empty(),
            "taken, not handed back"
        );
        assert!(app.toast.msg.is_none(), "no refusal");
    });
}

/// A refusal outside a composer submit (the path an approved plan's
/// follow-up takes) still shows its reason, and it stays.
#[gpui::test]
fn r1a_refusal_outside_the_composer_still_says_why(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("submit_guard::tests::r1a_refusal_outside_the_composer_still_says_why") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("b1");
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_conversation(app, Some(gone.clone()), true, cx);
        app.submit_work_run("Approved \u{2014} go ahead.".to_string(), cx);
        assert!(!app.work.running);
        assert!(
            toast(app).contains("\u{201C}b1\u{201D} is no longer there"),
            "{}",
            toast(app)
        );
    });
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(toast(app).contains("is no longer there"), "still on screen");
    });
}

/// Sticky toasts stay until dismissed; ordinary toasts still clear after 3 s.
#[gpui::test]
fn r1a_sticky_toast_stays_until_dismissed(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("submit_guard::tests::r1a_sticky_toast_stays_until_dismissed") {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| app.show_sticky_toast("refused", cx));
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert_eq!(toast(app), "refused", "a refusal stays well past 3 s");
        app.dismiss_toast(cx);
        assert!(app.toast.msg.is_none(), "a click closes it");
        app.show_toast("saved", cx);
    });
    cx.executor().advance_clock(Duration::from_secs(4));
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert!(app.toast.msg.is_none(), "ordinary toasts still time out");
    });
}

/// The wording: the missing-folder notice is plain, path-free and has a next
/// step, and it is given only when the folder is really gone (RC1-M: the S1
/// CLAUDE.md refusals are gone with CLAUDE.md).
#[test]
fn r1a_refusal_wording_is_plain_with_a_next_step() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(work_folder_gone_notice(Some(dir.path())), None);
    assert_eq!(work_folder_gone_notice(None), None);
    let t = work_folder_gone_notice(Some(&dir.path().join("trip"))).unwrap();
    assert_eq!(t, missing_folder_notice("trip"));
    assert!(t.starts_with("Not sent \u{2014} "), "{t}");
    assert!(
        t.contains("still in the box") && t.contains("then press Enter."),
        "{t}"
    );
    for jargon in ["Refused", "bound", "standing", "import", "/"] {
        assert!(!t.contains(jargon), "{jargon:?} in {t}");
    }
}

/// R1d (priority 4): a refused gateway send keeps its reason on screen until
/// dismissed (it used to vanish after 3 s).
#[gpui::test]
fn r1d_gateway_refusal_stays_until_dismissed(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("submit_guard::tests::r1d_gateway_refusal_stays_until_dismissed") {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_conversation(app, None, true, cx);
        app.work.gateway.enabled = true;
        app.work.engine.kind = awili_config::EngineKind::HermesGateway;
        app.work.executor = WorkExecutor::Gateway;
        assert!(app.work.health.gateway.begin_probe());
        app.work
            .health
            .gateway
            .finish_probe(Err("connection refused".into()), std::time::Instant::now());
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert_eq!(app.composer.read(cx).text(), DRAFT);
    });
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        let t = toast(app);
        assert!(
            t.starts_with(crate::gateway_health::GATEWAY_UNREACHABLE),
            "the refusal is still on screen: {t:?}"
        );
    });
}

/// RC1-H5 (RC1 round 3, Major 5: "Your message is still in the box" but the
/// box was empty): the D1 refusal through the ENTER key — the composer's own
/// Submit event, as a typed Return sends it — puts the text back, so the
/// toast's words are true.
#[gpui::test]
fn rc1h5_enter_refusal_puts_the_text_back(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("submit_guard::tests::rc1h5_enter_refusal_puts_the_text_back") {
        return;
    }
    // RC1-M: the refusal is a Work folder that is gone (the S1 CLAUDE.md
    // refusal it used is gone with CLAUDE.md).
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("d1");
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| {
        work_conversation(app, Some(folder.clone()), true, cx);
        app.composer.update(cx, |c, cx| {
            c.set_text("What is in this folder?", cx);
            cx.emit(crate::composer::ComposerEvent::Submit);
        });
    });
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert_eq!(app.composer.read(cx).text(), "What is in this folder?");
        assert!(transcript(app).is_empty(), "nothing recorded");
        let t = toast(app);
        assert!(t.contains("Your message is still in the box"), "{t}");
    });
}

/// RC1-H7 (RC1 round 3, Major 7: "Toasts never go away by themselves",
/// the Save toast still open after three minutes): an information toast goes
/// by itself after `INFO_TOAST_SECS`; a refusal stays until it is clicked or
/// until the user sends again (RC2-RUN, deliberate change: at most
/// `REFUSAL_TOAST_SECS`, see `rc2_run_refusal_toasts_go_by_themselves`).
#[gpui::test]
fn rc1h7_info_toasts_go_refusals_stay_until_the_next_send(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child(
        "submit_guard::tests::rc1h7_info_toasts_go_refusals_stay_until_the_next_send",
    ) {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| app.show_info_toast("Saved", cx));
    cx.executor()
        .advance_clock(Duration::from_secs(crate::INFO_TOAST_SECS - 1));
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(toast(app), "Saved", "long enough to read");
    });
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(app.toast.msg.is_none(), "then it goes by itself");
        app.show_sticky_toast("refused", cx);
    });
    cx.executor()
        .advance_clock(Duration::from_secs(crate::REFUSAL_TOAST_SECS - 1));
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert_eq!(toast(app), "refused", "a refusal stays");
        // The user sends again and the send goes through (the gateway lane,
        // known up, as in `r1a_sent_work_submit_is_not_handed_back`): the old
        // refusal goes, and no other toast takes its place.
        work_conversation(app, None, true, cx);
        app.work.gateway.enabled = true;
        app.work.engine.kind = awili_config::EngineKind::HermesGateway;
        app.work.executor = WorkExecutor::Gateway;
        let (tx, rx) = std::sync::mpsc::channel();
        app.work.gateway_cmd_tx = Some(tx);
        app.work.worker_identity = Some(crate::work_run_identity::WorkRunIdentity::HermesGateway);
        assert!(app.work.health.gateway.begin_probe());
        app.work
            .health
            .gateway
            .finish_probe(Ok(()), std::time::Instant::now());
        app.composer.update(cx, |c, cx| c.set_text(DRAFT, cx));
        app.submit_composer_draft(cx);
        assert!(
            matches!(rx.try_recv(), Ok(crate::work::GatewayCmd::Prompt { .. })),
            "the send went through"
        );
        assert!(
            app.toast.msg.is_none(),
            "the next send closes the old refusal"
        );
    });
}

/// RC2-RUN (round 3 Major 7, round 3b M7: a refusal toast still open after
/// 42 s, closed only by aiming at its X): a refusal stays long enough to
/// read it twice, then goes by itself; a newer toast restarts the clock.
#[gpui::test]
fn rc2_run_refusal_toasts_go_by_themselves(cx: &mut gpui::TestAppContext) {
    if !in_isolated_child("submit_guard::tests::rc2_run_refusal_toasts_go_by_themselves") {
        return;
    }
    let entity = cx.new(crate::AwiliApp::new);
    entity.update(cx, |app, cx| app.show_sticky_toast("refused", cx));
    cx.executor()
        .advance_clock(Duration::from_secs(crate::REFUSAL_TOAST_SECS - 1));
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(toast(app), "refused", "long enough to read twice");
    });
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    entity.update(cx, |app, cx| {
        assert!(app.toast.msg.is_none(), "then it goes by itself");
        app.show_sticky_toast("first", cx);
    });
    cx.executor()
        .advance_clock(Duration::from_secs(crate::REFUSAL_TOAST_SECS - 5));
    cx.run_until_parked();
    entity.update(cx, |app, cx| app.show_sticky_toast("second", cx));
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    entity.update(cx, |app, _cx| {
        assert_eq!(
            toast(app),
            "second",
            "the older timer does not cut it short"
        );
    });
}
