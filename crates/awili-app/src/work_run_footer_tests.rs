//! M2d proof: the per-reply run footer (F02/F03/S2-F05) through the real
//! Work path — a gateway turn gets its footer from the ledger line, and the
//! Chat context meter is hidden outside Chat. The harness is
//! `gateway_queue_tests`'s isolated-app plus fake-gateway helpers, driven
//! exactly like `submit_during_pending_stop_is_queued_then_runs`.

use super::gateway_queue_tests::{isolated, pump, wire_gateway, CONV};
use super::gateway_turn_tests::{complete, delta, running, stopped};
use super::tests::FakeOps;
use super::*;

/// The ONE ledger line under `home`, parsed as the `LedgerEntry` that
/// `observe` wrote.
fn only_ledger_entry(home: &std::path::Path) -> awili_selftest::graduation::LedgerEntry {
    let ledger = home.join("ledger.jsonl");
    let text = std::fs::read_to_string(&ledger).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one ledger line: {ledger:?}");
    serde_json::from_str(lines[0]).expect("the ledger line parses as a LedgerEntry")
}

#[gpui::test]
fn m2d_gateway_turn_gets_a_footer_from_the_ledger(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::run_footer_tests::m2d_gateway_turn_gets_a_footer_from_the_ledger")
    else {
        return;
    };
    let fake = FakeOps::new(vec![vec![
        running(true),
        delta("hi"),
        complete(),
        running(false),
    ]]);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("q".into(), cx);
        // The executor's usage frames for this turn, through the one Work
        // seam that feeds `RunPanel.usage` (the agent lane's
        // `ProviderEvent::Usage` handler calls exactly this). Two frames
        // prove they are summed: 30,000 + 3,040 = 33,040.
        app.note_work_usage(awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: 30_000,
            output_tokens: 0,
        });
        app.note_work_usage(awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: 0,
            output_tokens: 3_040,
        });
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);

        // The ledger line the recorder actually wrote.
        let entry = only_ledger_entry(&home);
        assert_eq!(entry.executor, "gateway");
        assert_eq!(
            (entry.frontier_tokens, entry.local_tokens),
            (33_040, 0),
            "the ledger books the summed usage as frontier tokens"
        );

        let rows = &app.views[CONV].rows;
        let last = rows.last().expect("the streamed assistant row");
        assert!(
            last.role == crate::Role::Assistant,
            "last row is the assistant reply"
        );
        let footer = last
            .footer
            .as_deref()
            .expect("the finished turn gets a footer");
        // The footer's executor, class and count are the ledger line's own.
        let parts: Vec<&str> = footer.split(" · ").collect();
        assert_eq!(
            parts.len(),
            4,
            "executor · model · time · tokens, got: {footer}"
        );
        assert_eq!(parts[0], crate::run_footer::executor_label(&entry.executor));
        assert_eq!(parts[1], "default model", "got: {footer}");
        assert!(parts[2].ends_with('s'), "elapsed part, got: {footer}");
        assert_eq!(
            parts[3],
            format!(
                "{} tokens",
                crate::run_footer::fmt_count(entry.frontier_tokens)
            )
        );
        assert_eq!(parts[3], "33,040 tokens");
        assert!(!footer.contains('$'), "the gateway is unpriced: {footer}");

        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

/// A gateway turn whose executor reports no usage: the ledger books zero
/// tokens, and the footer says so. It never shows an estimate.
#[gpui::test]
fn m2d_gateway_turn_without_usage_says_tokens_not_reported(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("work::run_footer_tests::m2d_gateway_turn_without_usage_says_tokens_not_reported")
    else {
        return;
    };
    let fake = FakeOps::new(vec![vec![
        running(true),
        delta("hi"),
        complete(),
        running(false),
    ]]);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("q".into(), cx);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);

        let entry = only_ledger_entry(&home);
        assert_eq!(entry.executor, "gateway");
        assert_eq!((entry.frontier_tokens, entry.local_tokens), (0, 0));
        assert_eq!(
            entry.notes.as_deref(),
            Some("tokens not reported by gateway")
        );

        let footer = app.views[CONV]
            .rows
            .last()
            .and_then(|r| r.footer.as_deref())
            .expect("the finished turn gets a footer")
            .to_string();
        let parts: Vec<&str> = footer.split(" · ").collect();
        assert_eq!(parts[0], "Fleet gateway", "got: {footer}");
        assert_eq!(
            parts.last().copied(),
            Some("tokens not reported"),
            "got: {footer}"
        );
        // R2e: no part is a token count (counts read "{n} tokens" now).
        assert!(
            !parts.iter().any(|p| p.ends_with(" tokens")),
            "got: {footer}"
        );

        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

#[gpui::test]
fn m2d_context_meter_hidden_outside_chat(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::run_footer_tests::m2d_context_meter_hidden_outside_chat")
    else {
        return;
    };
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, _cx| {
        // A non-empty Work view (the meter reads the ACTIVE view — the same
        // one `collect_status_parts` assembles) with the app in Work mode.
        let mut state = awili_core::ConversationState::new();
        state.push(crate::Role::User, "work question");
        state.push(crate::Role::Assistant, "work answer");
        app.views
            .insert(CONV.into(), crate::conv_view::ConvView::from_state(state));
        app.set_active_conversation(CONV);
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.mode = Mode::Work;
        let has_meter = |app: &AwiliApp| {
            app.collect_status_parts()
                .iter()
                .any(|(text, _)| text.starts_with('~'))
        };
        assert!(!has_meter(app), "Work mode hides the Chat context meter");
    });
    // `home` (the private tempdir) is dropped with this scope.
    let _ = home;
}

/// RC1-F4 (RC-17): Hermes reports the SESSION's running token totals on each
/// `message.complete`; Work books each turn's own share. Pure: 100/10 then
/// 250/30 are turns of 100/10 and 150/20; no counts, or no change, is no
/// event; a total that went down counts from zero.
#[test]
fn rc1f4_gateway_session_totals_become_per_turn_usage() {
    use awili_gateway::events::GatewayEventKind;
    let done = |i: Option<u64>, o: Option<u64>| GatewayEventKind::MessageComplete {
        text: None,
        status: None,
        cost_usd: None,
        context_percent: None,
        input_tokens: i,
        output_tokens: o,
    };
    let usage = |i, o| WorkEvent::UsageReported {
        input_tokens: i,
        output_tokens: o,
    };
    let mut seen = (0, 0);
    assert_eq!(
        gateway_turn_usage(&done(Some(100), Some(10)), &mut seen),
        Some(usage(100, 10))
    );
    assert_eq!(
        gateway_turn_usage(&done(Some(250), Some(30)), &mut seen),
        Some(usage(150, 20))
    );
    assert_eq!(gateway_turn_usage(&done(None, None), &mut seen), None);
    assert_eq!(
        gateway_turn_usage(&done(Some(250), Some(30)), &mut seen),
        None
    );
    assert_eq!(
        gateway_turn_usage(&done(Some(40), None), &mut seen),
        Some(usage(40, 0)),
        "a reset session counts from zero"
    );
    assert_eq!(seen, (40, 30));
    assert_eq!(
        gateway_turn_usage(
            &GatewayEventKind::MessageDelta { text: "x".into() },
            &mut seen
        ),
        None
    );
}

fn rc1f4_complete_with_usage(input: u64, output: u64) -> GatewayEvent {
    super::tests::gw_event(
        "message.complete",
        serde_json::json!({
            "status": "complete",
            "usage": { "input": input, "output": output, "total": input + output }
        }),
    )
}

/// RC1-F4 (RC-17): two gateway turns in one conversation (one Hermes
/// session) through the real Work path: each ledger line and footer carries
/// that turn's own tokens, from the session totals Hermes reports.
#[gpui::test]
fn rc1f4_gateway_turns_book_their_own_tokens(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("work::run_footer_tests::rc1f4_gateway_turns_book_their_own_tokens")
    else {
        return;
    };
    let fake = FakeOps::new(vec![
        vec![
            running(true),
            delta("one"),
            rc1f4_complete_with_usage(30_000, 3_040),
            running(false),
        ],
        vec![
            running(true),
            delta("two"),
            rc1f4_complete_with_usage(45_000, 4_000),
            running(false),
        ],
    ]);
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("q1".into(), cx);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);
        app.submit_work_run("q2".into(), cx);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);

        let text = std::fs::read_to_string(home.join("ledger.jsonl")).unwrap();
        let entries: Vec<awili_selftest::graduation::LedgerEntry> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let tokens: Vec<(u64, u64)> = entries
            .iter()
            .map(|e| (e.frontier_tokens, e.local_tokens))
            .collect();
        assert_eq!(tokens, [(33_040, 0), (15_960, 0)], "{text}");
        assert!(entries
            .iter()
            .all(|e| e.executor == "gateway" && e.notes.is_none()));

        let footers: Vec<String> = app.views[CONV]
            .rows
            .iter()
            .filter_map(|r| r.footer.as_deref().map(str::to_string))
            .collect();
        assert_eq!(footers.len(), 2, "{footers:?}");
        assert!(footers[0].ends_with("33,040 tokens"), "{footers:?}");
        assert!(footers[1].ends_with("15,960 tokens"), "{footers:?}");

        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}

/// RC1-F4-FIX1 (Codex T2 P1 on c9ab92f): a stopped gateway turn whose
/// `message.complete` (session totals 100/10) is consumed by the Stop drain,
/// then the next turn in the same Hermes session (totals 250/30): the next
/// ledger line and its footer book that turn's own 170 tokens, never 280.
/// The stopped turn books none.
#[gpui::test]
fn rc1f4_fix1_a_stopped_turn_moves_the_baseline_without_booking(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::run_footer_tests::rc1f4_fix1_a_stopped_turn_moves_the_baseline_without_booking",
    ) else {
        return;
    };
    let mut fake = FakeOps::new(vec![
        vec![running(true), delta("t1 ")],
        vec![
            running(true),
            delta("two"),
            rc1f4_complete_with_usage(250, 30),
            running(false),
        ],
    ]);
    fake.interrupt_events = vec![
        delta("late"),
        rc1f4_complete_with_usage(100, 10),
        running(false),
    ];
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        let (ev_rx, handle, epoch) = wire_gateway(app, &home, &fake, cx);
        app.submit_work_run("first".into(), cx);
        pump(
            app,
            &ev_rx,
            epoch,
            |e| *e == WorkEvent::Token("t1 ".into()),
            cx,
        );
        app.cancel_work_run(cx);
        pump(app, &ev_rx, epoch, |e| *e == stopped(), cx);
        assert_eq!(
            app.work.panel.usage, None,
            "the drained completion is not booked to the stopped turn"
        );
        app.submit_work_run("second".into(), cx);
        pump(app, &ev_rx, epoch, |e| *e == WorkEvent::TurnComplete, cx);

        let text = std::fs::read_to_string(home.join("ledger.jsonl")).unwrap();
        let entries: Vec<awili_selftest::graduation::LedgerEntry> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(entries.len(), 2, "{text}");
        assert_eq!(
            entries[0].outcome,
            awili_selftest::graduation::Outcome::Stopped,
            "{text}"
        );
        assert_eq!(
            (entries[0].frontier_tokens, entries[0].local_tokens),
            (0, 0),
            "the stopped turn books nothing: {text}"
        );
        assert_eq!(
            (entries[1].frontier_tokens, entries[1].local_tokens),
            (170, 0),
            "the next turn books only its own share: {text}"
        );
        let footer = app.views[CONV]
            .rows
            .last()
            .and_then(|r| r.footer.as_deref().map(str::to_string))
            .expect("the second reply has a footer");
        assert!(footer.ends_with(" 170 tokens"), "{footer}");

        app.work.gateway_cmd_tx = None;
        handle.join().unwrap();
    });
}
