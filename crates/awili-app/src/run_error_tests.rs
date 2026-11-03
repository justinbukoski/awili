use super::*;
use RunErrorAction as A;
use RunErrorCategory as C;

/// Every error source found in the app: CLI text, app notices, provider
/// events, council member text.
#[test]
fn classifier_table_covers_every_existing_error_source() {
    let table: &[(&str, C, A)] = &[
        // CLI sign-in text (agent_signin).
        (
            "Failed to authenticate: OAuth session expired",
            C::SignedOut,
            A::SignIn,
        ),
        (
            "Claude is signed out. Open Terminal, run claude, then type /login. Then press Retry.",
            C::SignedOut,
            A::SignIn,
        ),
        (
            "The helper is signed out. Sign it in again in Terminal, then press Retry.",
            C::SignedOut,
            A::SignIn,
        ),
        (
            "Not logged in \u{b7} Please run /login",
            C::SignedOut,
            A::SignIn,
        ),
        ("Error: 401 Unauthorized", C::SignedOut, A::SignIn),
        (
            "authentication_error: invalid api key",
            C::SignedOut,
            A::SignIn,
        ),
        // Local setup: CLI missing, folder missing, config missing.
        ("could not start `claude`", C::LocalSetup, A::OpenSettings),
        (
            "could not start `codex`: No such file or directory (os error 2)",
            C::LocalSetup,
            A::OpenSettings,
        ),
        ("failed to spawn helper", C::LocalSetup, A::OpenSettings),
        (
            crate::work::GATEWAY_SETUP_NOTICE,
            C::LocalSetup,
            A::OpenSettings,
        ),
        (
            crate::work::ENGINE_ENDPOINT_NOTICE,
            C::LocalSetup,
            A::OpenSettings,
        ),
        (
            crate::work::ENGINE_MODEL_NOTICE,
            C::LocalSetup,
            A::OpenSettings,
        ),
        ("no_provider_configured", C::LocalSetup, A::OpenSettings),
        (
            "set an API key for openrouter",
            C::LocalSetup,
            A::OpenSettings,
        ),
        (
            "agent runs need a working folder \u{2014} choose one first",
            C::LocalSetup,
            A::ChooseFolder,
        ),
        (
            "local builds need a working folder \u{2014} choose one first",
            C::LocalSetup,
            A::ChooseFolder,
        ),
        (
            &crate::submit_guard::missing_folder_notice("proj"),
            C::LocalSetup,
            A::ChooseFolder,
        ),
        // Rate limit / quota (awili-providers).
        (
            "rate_limit_error: slow down (429)",
            C::RateLimitOrQuota,
            A::TryAnotherModel,
        ),
        (
            "Your credit balance is too low",
            C::RateLimitOrQuota,
            A::TryAnotherModel,
        ),
        (
            "usage limit reached, resets at 5pm",
            C::RateLimitOrQuota,
            A::TryAnotherModel,
        ),
        (
            "quota exceeded (429)",
            C::RateLimitOrQuota,
            A::TryAnotherModel,
        ),
        // Provider.
        ("overloaded_error (529)", C::Provider, A::Retry),
        (
            "HTTP 503 service unavailable",
            C::Provider,
            A::TryAnotherModel,
        ),
        (
            "provider protocol: duplicate call id",
            C::Provider,
            A::TryAnotherModel,
        ),
        // Connection.
        (
            "ollama request failed: error sending request for url",
            C::Connection,
            A::Retry,
        ),
        ("read timeout", C::Connection, A::Retry),
        ("connection refused", C::Connection, A::Retry),
        ("transport: broken pipe", C::Connection, A::Retry),
        // Policy.
        (
            "blocked by rule: outside the project folder",
            C::Policy,
            A::None,
        ),
        ("the owner denied this action", C::Policy, A::None),
        ("turn stopped by host: denied by the owner", C::Policy, A::None),
        // S1b: fail-closed brief error.
        ("Awili could not prepare this run's instructions: cannot create /x: Permission denied (os error 13). Nothing was sent.", C::LocalSetup, A::Retry),
        // False positives that must NOT be read as a status or a rule.
        ("open ./notes.txt: Permission denied (os error 13)", C::Unknown, A::Retry),
        ("wrote 5001 bytes to /tmp/x500.txt", C::Unknown, A::Retry),
        ("syntax error at line 401 of main.rs", C::Unknown, A::Retry),
        ("cannot read /etc/dnsmasq.conf", C::Unknown, A::Retry),
        ("cannot read approvals.json: invalid data", C::Unknown, A::Retry),
        ("HTTP 500 Internal Server Error", C::Provider, A::TryAnotherModel),
        ("status: 502", C::Provider, A::TryAnotherModel),
        ("error 401", C::SignedOut, A::SignIn),
        ("DNS lookup failed", C::Connection, A::Retry),
        // Cancelled.
        ("stopped by user", C::Cancelled, A::None),
        ("turn stopped by host: cancelled", C::Cancelled, A::None),
        // Unknown.
        (
            "the fixture helper failed after partial output.",
            C::Unknown,
            A::Retry,
        ),
        ("", C::Unknown, A::Retry),
    ];
    for (text, cat, act) in table {
        let got = classify(text);
        assert_eq!((got.category, got.action), (*cat, *act), "{text:?}");
    }
}

#[test]
fn seam_errors_map_to_categories() {
    use awili_seam::{SeamError, StopCause};
    let cases = [
        (SeamError::Transport("socket closed".into()), C::Connection),
        (
            SeamError::Transport("401 unauthorized".into()),
            C::SignedOut,
        ),
        (SeamError::Protocol("empty batch".into()), C::Provider),
        (SeamError::Context("CannotFit".into()), C::Provider),
        (SeamError::Harness("bug".into()), C::Unknown),
        (SeamError::Cancelled(StopCause::Cancelled), C::Cancelled),
        (SeamError::Cancelled(StopCause::Denied), C::Policy),
    ];
    for (err, cat) in cases {
        assert_eq!(classify_seam(&err).category, cat, "{err}");
    }
}

#[test]
fn provider_kinds_map_without_reading_text() {
    for (kind, cat) in [
        (ProviderErrorKind::Quota, C::RateLimitOrQuota),
        (ProviderErrorKind::RateLimit, C::RateLimitOrQuota),
        (ProviderErrorKind::Overloaded, C::Provider),
        (ProviderErrorKind::Other, C::Unknown),
    ] {
        assert_eq!(classify_provider_kind(kind, "x").category, cat);
    }
}

#[test]
fn categories_have_titles_fix_flags_and_labelled_actions() {
    let all = [
        C::Connection,
        C::Provider,
        C::SignedOut,
        C::RateLimitOrQuota,
        C::Policy,
        C::LocalSetup,
        C::Cancelled,
        C::Unknown,
    ];
    let titles: std::collections::HashSet<_> = all.iter().map(|c| c.title()).collect();
    assert_eq!(titles.len(), all.len());
    assert!(C::SignedOut.user_can_fix() && C::LocalSetup.user_can_fix());
    assert!(!C::Unknown.user_can_fix() && !C::Provider.user_can_fix());
    assert_eq!(A::None.label(), None);
    assert_eq!(A::SignIn.label(), Some("Sign in"));
    assert_eq!(A::TryAnotherModel.label(), Some("Try another model"));
}

#[test]
fn run_id_is_short_unique_and_in_the_log_line() {
    let a = new_run_id();
    let b = new_run_id();
    assert_eq!(a.len(), 8);
    assert_ne!(a, b);
    assert_eq!(run_id_of_token("0123456789abcdef"), "01234567");
    let line = log_line(&a, "work", "conv-1");
    assert!(line.contains(&a) && line.contains("conv-1"));
    let mut ids = RunIds::default();
    ids.set("c", 3, &a);
    assert_eq!(ids.get("c", 3), Some(a.as_str()));
    assert_eq!(ids.get("c", 4), None);
}

#[test]
fn ledger_task_id_carries_the_run_id() {
    let mut rec = crate::graduation_record::Recorder::default();
    rec.arm("conv-1");
    let id = rec.run_id();
    assert_eq!(id.len(), 8);
    let panel = crate::work::RunPanel {
        status: Some(awili_work::run_events::RunStatus::Completed),
        ..Default::default()
    };
    let entry = rec.preview(&panel, chrono::Utc::now()).unwrap();
    assert!(
        entry.task_id.starts_with(&format!("work:conv-1:{id}")),
        "{}",
        entry.task_id
    );
}

#[test]
fn sign_in_command_follows_the_helper_that_ran_not_the_failure_text() {
    use awili_code::CodeEngine;
    assert_eq!(
        crate::run_error_ui::sign_in_command(Some(CodeEngine::Claude)),
        Some("claude")
    );
    assert_eq!(
        crate::run_error_ui::sign_in_command(Some(CodeEngine::Codex)),
        Some("codex login")
    );
    assert_eq!(crate::run_error_ui::sign_in_command(None), None);
}

/// A reply's run id survives a restart: saved with the conversation, then
/// put back on the rebuilt messages, whose in-memory timestamps are new.
#[test]
fn run_ids_round_trip_through_a_restore() {
    let mut convo = awili_core::ConversationState::new();
    convo.push(awili_core::Role::User, "q");
    let ts = convo.push(awili_core::Role::Assistant, "a");
    let mut ids = RunIds::default();
    let json = ids.set("c1", ts, "deadbeef").expect("a row to save");
    assert_eq!(RunIds::key("c1"), "run_ids:c1");
    // Same id again: nothing new to save.
    assert!(ids.set("c1", ts, "deadbeef").is_none());
    // Restart: stored rows carry their old timestamps; the rebuilt convo is
    // numbered afresh (skipping nothing here), so the pairing is by order.
    let msgs = vec![
        awili_persist::StoredMessage {
            id: 1,
            role: "user".into(),
            text: "q".into(),
            ts: 100,
            kind: "normal".into(),
            superseded_by: None,
        },
        awili_persist::StoredMessage {
            id: 2,
            role: "assistant".into(),
            text: "a".into(),
            ts: ts as i64,
            kind: "normal".into(),
            superseded_by: None,
        },
    ];
    let restored = crate::restore_conversation(&msgs);
    let mut after = RunIds::default();
    after.restore("c1", &json, &msgs, &restored);
    let new_ts = restored.message(1).unwrap().ts;
    assert_eq!(after.get("c1", new_ts), Some("deadbeef"));
    assert_eq!(after.get("c1", new_ts + 7), None);
    // Deleting the conversation forgets them.
    after.forget("c1");
    assert_eq!(after.get("c1", new_ts), None);
}

/// rc.4 S7: the sandbox could not be set up, so the turn did not start: a
/// setup problem the owner fixes in Settings (never "unknown, retry").
#[test]
fn s7_sandbox_start_failure_points_at_settings() {
    for text in [
        "Couldn't start the sandbox: /usr/bin/sandbox-exec is missing on this Mac.",
        "Couldn't start the sandbox: a path contains a line break or a hidden character: /tmp/a\\nb.",
    ] {
        let e = classify(text);
        assert_eq!((e.category, e.action), (C::LocalSetup, A::OpenSettings), "{text}");
    }
}
