//! RC2-UX proof (D-PUNCH S2-F09, S2-F16): Hermes' question is answered in
//! the message box (it was an "APPROVAL · clarify" card whose Allow/Deny
//! sent a verdict Hermes never asked for, while nothing sent the answer),
//! and approval cards read in plain words. End to end over the real
//! `AwiliApp` and the real gateway worker loop over the scripted fake.

use super::gateway_queue_tests::{isolated, pump, wire_gateway};
use super::gateway_turn_tests::{complete, delta, running};
use super::tests::{gw_event, FakeOps};
use super::*;
use serde_json::json;

/// The card's heading and Hermes' words are plain: no "APPROVAL · kind",
/// no "[HIGH]" tag, no scanner words.
#[test]
fn rc2ux_approval_cards_speak_plainly() {
    let card = |kind: &str, risk| ApprovalCard {
        approval_id: "a".into(),
        kind: kind.into(),
        prompt: String::new(),
        risk,
        delivery: Default::default(),
    };
    assert_eq!(
        approval_heading(&card(CLARIFY_KIND, ApprovalRisk::Privileged)),
        "Hermes asks you"
    );
    assert_eq!(
        approval_heading(&card("command", ApprovalRisk::Mutating)),
        "Needs your OK to make a change"
    );
    assert_eq!(
        approval_heading(&card("command", ApprovalRisk::Irreversible)),
        "Needs your OK \u{2014} this cannot be undone"
    );
    assert_eq!(
        plain_approval_text("[HIGH] private network access to 10.77.68.86 (SSRF or lateral movement)"),
        "Risky: reaches a computer on your home network to 10.77.68.86 (reaching other computers on \
         your network)"
    );
    assert_eq!(
        plain_approval_text("[MEDIUM] delete in root path"),
        "deletes files outside your home folder"
    );
    assert_eq!(plain_approval_text("Run the tests"), "Run the tests");
    assert_eq!(plain_approval_text("[x] y"), "y");
}

/// Hermes asks a question mid-turn: the card shows the question and where
/// to answer (no Allow/Deny), an Allow chord sends nothing, the message box
/// stays open while the run waits, and Enter sends the answer to Hermes
/// (`clarify.respond`), keeping both in the reply.
#[gpui::test]
fn rc2ux_hermes_question_is_answered_in_the_message_box(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::clarify_tests::rc2ux_hermes_question_is_answered_in_the_message_box")
    else {
        return;
    };
    let question = gw_event(
        "clarify.request",
        json!({ "question": "Which folder: Documents or Downloads?", "request_id": "c1" }),
    );
    let fake = FakeOps::new(vec![vec![
        running(true),
        delta("Let me check. "),
        question,
        delta("Done."),
        complete(),
        running(false),
    ]]);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.mode = Mode::Work;
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("tidy my files".into(), cx);
        pump(
            app,
            &ev_rx,
            epoch,
            |e| matches!(e, WorkEvent::ClarificationRequested { .. }),
            cx,
        );
        let card = app.work.panel.approval.clone().expect("the question card");
        assert_eq!(card.kind, CLARIFY_KIND);
        assert!(app.clarify_waiting());
        assert!(!app.composer_busy(), "the box is open for the answer");

        // An Allow (button or chord) never answers a question.
        app.resolve_work_approval(ApprovalDecision::Approve, cx);
        assert!(app.work.panel.approval.is_some(), "the question stays");

        app.composer.update(cx, |c, cx| c.set_text("Documents", cx));
        app.submit_composer_draft(cx);
        assert!(app.work.panel.approval.is_none(), "answered");
        assert!(!app.clarify_waiting());
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !fake
            .ops()
            .iter()
            .any(|o| o == "clarify_respond(s1,Documents)")
        {
            assert!(std::time::Instant::now() < deadline, "{:?}", fake.ops());
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !fake.ops().iter().any(|o| o.starts_with("approval_respond")),
            "no verdict was sent: {:?}",
            fake.ops()
        );
        let reply = app.views[app.sidebar.active_id()]
            .convo
            .messages()
            .last()
            .map(|m| m.text.clone())
            .unwrap_or_default();
        assert!(
            reply.contains("**Hermes asked:** Which folder: Documents or Downloads?")
                && reply.contains("**You answered:** Documents"),
            "{reply}"
        );
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

/// RC2-UX (D-PUNCH S2-F15, round 6 Minor 3 for the other helpers): the parts
/// of a Hermes reply on either side of a tool step no longer run together
/// ("…now.Done, …"); text that already has its space is left alone.
#[gpui::test]
fn rc2ux_reply_parts_around_a_tool_step_do_not_run_together(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::clarify_tests::rc2ux_reply_parts_around_a_tool_step_do_not_run_together")
    else {
        return;
    };
    let tool = |name: &str| gw_event("tool.start", json!({ "name": name, "summary": "write" }));
    let fake = FakeOps::new(vec![vec![
        running(true),
        delta("Creating the file now."),
        tool("write_file"),
        delta("Done, hello.txt now exists."),
        tool("read_file"),
        delta(" It says hi."),
        complete(),
        running(false),
    ]]);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.mode = Mode::Work;
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("make hello.txt".into(), cx);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        let reply = app.views[app.sidebar.active_id()]
            .convo
            .messages()
            .last()
            .map(|m| m.text.clone())
            .unwrap_or_default();
        assert_eq!(
            reply,
            "Creating the file now.\n\nDone, hello.txt now exists. It says hi."
        );
        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

/// RC2-UX (D-PUNCH S3-F10: the ⌘F panel stayed open across pages and took
/// "tally" meant for the Sessions box): a page change closes it.
#[gpui::test]
fn rc2ux_the_search_panel_closes_on_a_page_change(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("work::clarify_tests::rc2ux_the_search_panel_closes_on_a_page_change")
    else {
        return;
    };
    let (view, window) = cx.add_window_view(|_w, cx| AwiliApp::new(cx));
    view.update_in(window, |app, window, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Work, cx);
        app.toggle_search(window, cx);
        assert!(app.search.open);
        app.set_mode(Mode::Sessions, cx);
        assert!(!app.search.open, "closed on the page change");
    });
    window.run_until_parked();
    let focus_left_search = view.update_in(window, |app, window, cx| {
        !app.search_input.read(cx).focus_handle().is_focused(window)
    });
    assert!(focus_left_search, "the hidden box keeps no keys");
}

/// RC2-R3 (usability RC2 round 3, Minor 10): a card for a step Awili never
/// runs on its own (a tool that says nothing about what it does, such as the
/// owner's `read_file`) no longer claims "this runs with full rights".
#[test]
fn rc2r3_a_card_never_says_full_rights() {
    let card = ApprovalCard {
        approval_id: "a".into(),
        kind: "command".into(),
        prompt: String::new(),
        risk: ApprovalRisk::Privileged,
        delivery: Default::default(),
    };
    let heading = approval_heading(&card);
    assert_eq!(heading, PRIVILEGED_HEADING);
    assert!(!heading.contains("full rights"), "{heading}");
    assert!(heading.starts_with("Needs your OK"), "{heading}");
}
