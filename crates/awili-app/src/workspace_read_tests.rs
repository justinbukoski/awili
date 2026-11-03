use super::*;

fn diagnostic(folder: &std::path::Path) -> Diagnostic {
    let fixture = Challenge::create(folder).unwrap();
    Diagnostic {
        key: Correlation {
            fixture_id: fixture.id.clone(),
            conversation: "diagnostic-conversation".into(),
            run_seq: 7,
        },
        fixture,
        request: "Read a path".into(),
        answer: None,
        success: false,
        tool_attempt_failed: false,
        done: false,
        stream_closed: false,
        cancelled: false,
        terminated: Arc::new(AtomicBool::new(false)),
        reading: false,
        started: Instant::now(),
        executor: "agent:claude:sonnet".into(),
        stop_seq: None,
        timed_out: false,
    }
}

#[test]
fn stale_completion_cannot_settle_a_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = diagnostic(dir.path());
    d.answer = Some(d.fixture.nonce.clone());
    d.success = true;
    d.terminated.store(true, Ordering::Release);
    d.observe_end("another-conversation", 7, true);
    d.observe_end("diagnostic-conversation", 8, true);
    assert!(!d.ready_for_readback());
    assert!(!d.done);
    d.observe_end("diagnostic-conversation", 7, true);
    assert!(d.ready_for_readback());
    assert!(d.done);
}

#[test]
fn cancel_epoch_change_does_not_discard_old_provider_termination() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = diagnostic(dir.path());
    d.cancelled = true;
    d.answer = Some(d.fixture.nonce.clone());
    // UI has advanced to epoch 8; this observer remains bound to original 7.
    d.observe_end("diagnostic-conversation", 7, true);
    assert!(!d.ready_for_readback());
    d.terminated.store(true, Ordering::Release);
    assert!(d.ready_for_readback());
    let rows = vec![
        ("user".into(), d.request.clone()),
        ("assistant".into(), d.answer.clone().unwrap()),
    ];
    assert_ne!(
        workspace_read_outcome(
            &d.fixture.nonce,
            &d.request,
            d.answer.as_deref().unwrap(),
            true,
            d.cancelled,
            Some(&rows)
        ),
        Outcome::Pass
    );
}

#[test]
fn closed_channel_without_done_is_never_success() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = diagnostic(dir.path());
    d.answer = Some(d.fixture.nonce.clone());
    d.success = true;
    d.terminated.store(true, Ordering::Release);
    d.observe_end("diagnostic-conversation", 7, false);
    assert!(d.ready_for_readback());
    let rows = vec![
        ("user".into(), d.request.clone()),
        ("assistant".into(), d.answer.clone().unwrap()),
    ];
    assert_ne!(
        workspace_read_outcome(
            &d.fixture.nonce,
            &d.request,
            d.answer.as_deref().unwrap(),
            d.success && d.done,
            false,
            Some(&rows)
        ),
        Outcome::Pass
    );
}

#[test]
fn serialized_readback_observes_exact_prior_appends() {
    let dir = tempfile::tempdir().unwrap();
    let store = awili_persist::Store::open(&dir.path().join("readback.db")).unwrap();
    let (p, worker) = crate::persistence::Persistence::spawn_worker(store);
    p.send(PersistCmd::CreateConversation {
        id: "diagnostic".into(),
        title: "diagnostic".into(),
        created_ts: 1,
        kind: "work".into(),
    });
    p.send(PersistCmd::Append {
        conv: "diagnostic".into(),
        role: "user",
        text: "original raw request".into(),
        ts: 1,
        kind: awili_core::MessageKind::Normal,
    });
    p.send(PersistCmd::Append {
        conv: "diagnostic".into(),
        role: "assistant",
        text: "answer".into(),
        ts: 2,
        kind: awili_core::MessageKind::Normal,
    });
    let (tx, rx) = async_channel::bounded(1);
    p.send(PersistCmd::ReadDiagnosticMessages {
        conv: "diagnostic".into(),
        reply: tx,
    });
    assert_eq!(
        rx.recv_blocking().unwrap().unwrap(),
        vec![
            ("user".into(), "original raw request".into()),
            ("assistant".into(), "answer".into())
        ]
    );
    drop(p);
    worker.join().unwrap();
}

#[test]
fn pending_diagnostic_keeps_evidence_while_normal_turns_continue() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = diagnostic(dir.path());
    d.cancelled = true;
    // RC2-APP (F0317, deliberate change): the Stop moved the run to 8.
    d.note_stop_epoch("diagnostic-conversation", 8);
    let original = d.fixture.nonce.clone();
    d.capture_terminal(
        "diagnostic-conversation",
        8,
        Some(original.clone()),
        false,
        true,
    );
    assert!(!d.ready_for_readback()); // termination still uncertain
                                      // Ordinary Work may now run in another conversation or reuse this one.
    d.capture_terminal(
        "ordinary-conversation",
        9,
        Some("normal answer".into()),
        true,
        false,
    );
    d.capture_terminal(
        "diagnostic-conversation",
        8,
        Some("later gateway answer".into()),
        true,
        false,
    );
    d.observe_end("ordinary-conversation", 9, true);
    assert!(!d.done);
    assert_eq!(d.answer.as_deref(), Some(original.as_str()));
    assert!(!d.success);
    assert!(d.tool_attempt_failed);
    d.terminated.store(true, Ordering::Release);
    d.observe_end("diagnostic-conversation", 7, true);
    assert!(d.ready_for_readback());
    assert_eq!(d.answer.as_deref(), Some(original.as_str()));
}

#[test]
fn later_rows_in_same_conversation_fail_readback_instead_of_passing_old_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = diagnostic(dir.path());
    let answer = d.fixture.nonce.clone();
    d.capture_terminal(
        "diagnostic-conversation",
        7,
        Some(answer.clone()),
        true,
        true,
    );
    let rows = vec![
        ("user".into(), d.request.clone()),
        ("assistant".into(), answer.clone()),
        ("user".into(), "new ordinary Work request".into()),
    ];
    assert!(matches!(
        workspace_read_outcome(
            &d.fixture.nonce,
            &d.request,
            &answer,
            d.success,
            false,
            Some(&rows)
        ),
        Outcome::Error { .. }
    ));
    assert!(d.tool_attempt_failed);
}

/// RC2-APP (F0317): a stopped diagnostic takes its answer only under the
/// number the Stop moved it to — never from a later run that happens to use
/// the next number.
#[test]
fn rc2app_a_stopped_diagnostic_takes_no_later_runs_answer() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = diagnostic(dir.path());
    d.cancelled = true;
    // No Stop moved the number on (a gateway Stop keeps it): a later run
    // under 8 is not this diagnostic's.
    d.capture_terminal(
        "diagnostic-conversation",
        8,
        Some("a later run's answer".into()),
        true,
        false,
    );
    assert_eq!(d.answer, None);
    // The owner's agent Stop moved it to 8: the stopped turn finishes there.
    d.note_stop_epoch("another-conversation", 8);
    d.note_stop_epoch("diagnostic-conversation", 9);
    assert_eq!(d.stop_seq, None, "only this conversation's next number");
    d.note_stop_epoch("diagnostic-conversation", 8);
    d.capture_terminal(
        "diagnostic-conversation",
        8,
        Some("the stopped turn".into()),
        false,
        false,
    );
    assert_eq!(d.answer.as_deref(), Some("the stopped turn"));
    // A run that was not stopped records no stop number.
    let mut live = diagnostic(dir.path());
    live.note_stop_epoch("diagnostic-conversation", 8);
    assert_eq!(live.stop_seq, None);
}

/// RC2-APP (F0316): past its deadline a diagnostic is ready to be written up
/// (as cancelled, never passed) even though its evidence never came, so it
/// does not hold the slot forever.
#[test]
fn rc2app_a_timed_out_diagnostic_is_written_up() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = diagnostic(dir.path());
    assert!(!d.ready_for_readback(), "no answer, no end, no termination");
    d.time_out();
    assert!(d.ready_for_readback());
    assert!(d.cancelled, "a timed-out diagnostic never passes");
    d.reading = true;
    assert!(!d.ready_for_readback(), "written up once");
}

/// RC2-APP (F0318): the answer is the latest assistant reply, wherever it
/// sits in the conversation.
#[test]
fn rc2app_the_answer_is_the_latest_reply() {
    let convo = awili_core::ConversationState::seeded([
        (awili_core::Role::User, "first".to_string()),
        (awili_core::Role::Assistant, "first reply".to_string()),
        (awili_core::Role::User, "second".to_string()),
        (awili_core::Role::Assistant, "second reply".to_string()),
        (awili_core::Role::User, "third, no reply yet".to_string()),
    ]);
    assert_eq!(
        latest_assistant_text(&convo).as_deref(),
        Some("second reply")
    );
    assert_eq!(
        latest_assistant_text(&awili_core::ConversationState::new()),
        None
    );
}
