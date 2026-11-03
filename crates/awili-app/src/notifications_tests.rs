//! N1 proof tests 6–28 for the pure kernel (spec §6). No gpui, no backend.

use super::*;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn work(id: &str) -> Target {
    Target::Work(id.to_string())
}

fn event(target: Target, kind: EventKind, key: &str) -> Event {
    Event {
        target,
        kind,
        key: key.to_string(),
        label: "awili".to_string(),
        detail: None,
        elapsed: None,
    }
}

/// A Work finish that ran 20 s.
fn finished(key: &str) -> Event {
    Event {
        elapsed: Some(secs(20)),
        ..event(work("c1"), EventKind::Finished, key)
    }
}

fn approval(key: &str) -> Event {
    event(
        work("c1"),
        EventKind::Approval {
            kind: "dangerous_command".to_string(),
        },
        key,
    )
}

/// Authorized, window inactive for 30 s: every gate but the one under test
/// passes.
fn away(cfg: NotificationsConfig, now: Instant) -> Notifier {
    let mut n = Notifier::new(cfg);
    n.set_initial_permission(Permission::Authorized);
    n.observe_window(false, now - secs(30));
    n
}

fn posted(r: Result<Vec<Action>, Suppressed>) -> Vec<String> {
    r.expect("posts")
        .into_iter()
        .map(|a| match a {
            Action::PostNative(n) => format!("native:{}", n.title),
            Action::PostFallback(n) => format!("fallback:{}", n.title),
            Action::RequestPermission => "request".to_string(),
        })
        .collect()
}

/// 6: `enabled = false` suppresses every kind except the Settings test.
#[test]
fn disabled_suppresses_everything() {
    let now = Instant::now();
    let cfg = NotificationsConfig {
        enabled: false,
        ..NotificationsConfig::default()
    };
    let mut n = away(cfg, now);
    for (i, kind) in [
        EventKind::Approval { kind: "x".into() },
        EventKind::Question,
        EventKind::Credential { sudo: true },
        EventKind::Finished,
        EventKind::Failed,
        EventKind::Stopped,
    ]
    .into_iter()
    .enumerate()
    {
        let ev = Event {
            elapsed: Some(secs(20)),
            ..event(work("c1"), kind, &format!("k{i}"))
        };
        assert_eq!(n.handle(ev, None, None, now), Err(Suppressed::Disabled));
    }
    let test = event(Target::Test, EventKind::Test, "test");
    assert_eq!(
        posted(n.handle(test, None, None, now)),
        vec!["native:Awili notifications are on"]
    );
}

/// 7: each live kind toggle suppresses only its own kinds.
#[test]
fn each_toggle_suppresses_only_its_kinds() {
    let kinds = [
        EventKind::Approval { kind: "x".into() },
        EventKind::Question,
        EventKind::Credential { sudo: false },
        EventKind::Finished,
        EventKind::Failed,
        EventKind::Stopped,
        EventKind::Test,
    ];
    // (toggle, the kinds it owns, by index into `kinds`)
    let toggles: [(&str, &[usize]); 4] = [
        ("approvals", &[0, 1, 2]),
        ("finished", &[3]),
        ("failed", &[4, 5]),
        ("sound", &[]),
    ];
    for (toggle, owned) in toggles {
        let mut cfg = NotificationsConfig::default();
        match toggle {
            "approvals" => cfg.approvals = false,
            "finished" => cfg.finished = false,
            "failed" => cfg.failed = false,
            _ => cfg.sound = false,
        }
        let now = Instant::now();
        let mut n = away(cfg, now);
        for (i, kind) in kinds.iter().enumerate() {
            let target = if *kind == EventKind::Test {
                Target::Test
            } else {
                work("c1")
            };
            let ev = Event {
                elapsed: Some(secs(20)),
                ..event(target, kind.clone(), &format!("{toggle}-{i}"))
            };
            let got = n.handle(ev, None, None, now);
            if owned.contains(&i) {
                assert_eq!(got, Err(Suppressed::KindOff), "{toggle} owns {kind:?}");
            } else {
                assert!(got.is_ok(), "{toggle} must not touch {kind:?}: {got:?}");
            }
        }
    }
}

/// 8: a present owner gets no finish or failure, and no waiting event for the
/// conversation on screen.
#[test]
fn present_user_gets_no_finish_or_waiting() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.set_initial_permission(Permission::Authorized);
    let visible = work("c1");
    for (i, kind) in [EventKind::Finished, EventKind::Failed, EventKind::Stopped]
        .into_iter()
        .enumerate()
    {
        let ev = Event {
            elapsed: Some(secs(60)),
            ..event(work("c1"), kind, &format!("end{i}"))
        };
        assert_eq!(
            n.handle(ev, Some(&visible), None, now),
            Err(Suppressed::Present)
        );
    }
    assert_eq!(
        n.handle(approval("a1"), Some(&visible), None, now),
        Err(Suppressed::Visible)
    );
}

/// 9: a present owner still hears about a waiting agent he is not looking at.
#[test]
fn present_user_gets_waiting_for_hidden_target() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.set_initial_permission(Permission::Authorized);
    let code = Target::Code(9);
    assert_eq!(
        posted(n.handle(approval("a1"), Some(&code), None, now)),
        vec!["native:Approval needed"]
    );
    let cred = event(work("c1"), EventKind::Credential { sudo: true }, "cred");
    assert_eq!(
        posted(n.handle(cred, None, None, now)),
        vec!["native:Password needed"]
    );
}

/// 10: away for 0 s, a waiting event posts at once.
#[test]
fn away_user_gets_waiting_immediately() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.set_initial_permission(Permission::Authorized);
    n.observe_window(false, now);
    let q = event(work("c1"), EventKind::Question, "q");
    assert_eq!(
        posted(n.handle(q, Some(&work("c1")), None, now)),
        vec!["native:Question waiting"]
    );
}

/// 11: a finish with a known run length: 7.9 s is too quick, 8 s posts.
#[test]
fn finish_uses_run_length_gate() {
    let now = Instant::now();
    let mut n = away(NotificationsConfig::default(), now);
    let quick = Event {
        elapsed: Some(Duration::from_millis(7900)),
        ..finished("r1")
    };
    assert_eq!(n.handle(quick, None, None, now), Err(Suppressed::TooQuick));
    let long = Event {
        elapsed: Some(secs(8)),
        ..finished("r2")
    };
    assert_eq!(
        posted(n.handle(long, None, None, now)),
        vec!["native:Work finished"]
    );
}

/// 12: without a run length (Code), the time away is the gate.
#[test]
fn finish_uses_time_away_when_run_length_unknown() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.set_initial_permission(Permission::Authorized);
    n.observe_window(false, now - secs(5));
    let code = event(Target::Code(3), EventKind::Finished, "code:3:2");
    assert_eq!(
        n.handle(code.clone(), None, None, now),
        Err(Suppressed::TooQuick)
    );
    let later = now + secs(4);
    assert_eq!(
        posted(n.handle(code, None, None, later)),
        vec!["native:Code turn finished"]
    );
}

/// 13: a failure posts when away with no duration gate at all.
#[test]
fn failed_posts_when_away() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.set_initial_permission(Permission::Authorized);
    n.observe_window(false, now);
    let failed = Event {
        elapsed: Some(Duration::from_millis(300)),
        ..event(work("c1"), EventKind::Failed, "f1")
    };
    assert_eq!(
        posted(n.handle(failed, None, None, now)),
        vec!["native:Work failed"]
    );
    let stopped = event(work("c2"), EventKind::Stopped, "s1");
    assert_eq!(
        posted(n.handle(stopped, None, None, now)),
        vec!["native:Work stopped"]
    );
}

/// 14: frontmost but idle counts as away: 119 s no, 120 s yes, and
/// `idle_away_secs = 0` never.
#[test]
fn idle_while_frontmost_counts_as_away() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.set_initial_permission(Permission::Authorized);
    assert_eq!(
        n.handle(finished("i1"), None, Some(119), now),
        Err(Suppressed::Present)
    );
    assert_eq!(
        posted(n.handle(finished("i2"), None, Some(120), now)),
        vec!["native:Work finished"]
    );
    let mut off = Notifier::new(NotificationsConfig {
        idle_away_secs: 0,
        ..NotificationsConfig::default()
    });
    off.set_initial_permission(Permission::Authorized);
    assert_eq!(
        off.handle(finished("i3"), None, Some(100_000), now),
        Err(Suppressed::Present)
    );
}

/// 15: Stop then Stopped is silent (the mark is consumed); a Stopped 121 s
/// after the mark posts.
#[test]
fn user_stop_is_silent() {
    let now = Instant::now();
    let mut n = away(NotificationsConfig::default(), now);
    n.note_user_stop(work("c1"), now);
    let stopped = event(work("c1"), EventKind::Stopped, "s1");
    assert_eq!(
        n.handle(stopped, None, None, now),
        Err(Suppressed::UserStopped)
    );
    let again = event(work("c1"), EventKind::Stopped, "s2");
    assert!(n.handle(again, None, None, now).is_ok(), "mark consumed");
    n.note_user_stop(work("c1"), now);
    let failed = event(work("c1"), EventKind::Failed, "f1");
    assert_eq!(
        n.handle(failed, None, None, now + secs(1)),
        Err(Suppressed::UserStopped)
    );
    n.note_user_stop(work("c1"), now);
    let late = event(work("c1"), EventKind::Stopped, "s3");
    assert_eq!(
        posted(n.handle(late, None, None, now + secs(121))),
        vec!["native:Work stopped"]
    );
}

/// 28: the id depends only on the target: an approval then a finish on one
/// conversation share it (the newer replaces the older); two differ.
#[test]
fn click_target_is_conv_id_only() {
    let cfg = NotificationsConfig::default();
    let a = payload(&approval("a"), &cfg);
    let f = payload(&finished("f"), &cfg);
    assert_eq!(a.id, "awili:v1:work:c1");
    assert_eq!(a.id, f.id);
    let other = payload(&event(work("c2"), EventKind::Finished, "f2"), &cfg);
    assert_ne!(a.id, other.id);
}

/// The title per kind and surface (§2.1 payload table), and `sound` follows
/// the config.
#[test]
fn turn_kind_table() {
    let cfg = NotificationsConfig {
        sound: false,
        ..NotificationsConfig::default()
    };
    let rows = [
        (work("c"), EventKind::Finished, "Work finished"),
        (Target::Code(1), EventKind::Finished, "Code turn finished"),
        (work("c"), EventKind::Failed, "Work failed"),
        (Target::Code(1), EventKind::Failed, "Code turn failed"),
        (work("c"), EventKind::Stopped, "Work stopped"),
        (
            work("c"),
            EventKind::Credential { sudo: false },
            "Secret needed",
        ),
        (Target::Test, EventKind::Test, "Awili notifications are on"),
    ];
    for (target, kind, title) in rows {
        let n = payload(&event(target, kind, "k"), &cfg);
        assert_eq!(n.title, title);
        assert!(!n.sound);
    }
    let code = Event {
        detail: Some("Claude Code".into()),
        ..event(Target::Code(1), EventKind::Finished, "k")
    };
    assert_eq!(payload(&code, &cfg).body, "awili \u{b7} Claude Code");
}

/// 16: one key posts once; 300 keys leave at most 256 remembered, and the
/// oldest can post again.
#[test]
fn duplicate_key_posts_once_and_seen_is_bounded() {
    let now = Instant::now();
    let mut n = away(NotificationsConfig::default(), now);
    assert!(n.handle(approval("dup"), None, None, now).is_ok());
    assert_eq!(
        n.handle(approval("dup"), None, None, now),
        Err(Suppressed::Duplicate)
    );
    for i in 0..300 {
        assert!(n
            .handle(approval(&format!("k{i}")), None, None, now)
            .is_ok());
    }
    assert!(n.seen.len() <= 256);
    assert!(n.handle(approval("k0"), None, None, now).is_ok());
    assert_eq!(
        n.handle(approval("k299"), None, None, now),
        Err(Suppressed::Duplicate)
    );
}

/// 17: `observe_window` and a suppressed event ask nothing; the first
/// eligible event asks exactly once; a second one while Requesting asks
/// nothing.
#[test]
fn permission_asked_on_first_eligible_event_only() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.observe_window(false, now - secs(30));
    let quick = Event {
        elapsed: Some(secs(1)),
        ..finished("q")
    };
    assert_eq!(n.handle(quick, None, None, now), Err(Suppressed::TooQuick));
    assert_eq!(n.permission(), &Permission::Unknown);
    assert_eq!(
        n.handle(approval("a1"), None, None, now),
        Ok(vec![Action::RequestPermission])
    );
    assert_eq!(n.permission(), &Permission::Requesting);
    assert_eq!(n.handle(finished("f1"), None, None, now), Ok(Vec::new()));
}

/// 18: pending flushes in order on Authorized; the same id replaces; five
/// distinct ids drop the oldest.
#[test]
fn pending_flushes_in_order_on_authorized_and_caps_at_four() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.observe_window(false, now - secs(30));
    for (i, conv) in ["c1", "c2", "c3", "c4", "c5"].into_iter().enumerate() {
        let ev = event(work(conv), EventKind::Failed, &format!("f{i}"));
        n.handle(ev, None, None, now).unwrap();
    }
    // Same id as c5's: replaces it (and goes last).
    let q = event(work("c5"), EventKind::Question, "q5");
    n.handle(q, None, None, now).unwrap();
    let flushed: Vec<(String, String)> = n
        .permission_changed(Permission::Authorized)
        .into_iter()
        .map(|a| match a {
            Action::PostNative(x) => (x.id, x.title),
            other => panic!("only native posts: {other:?}"),
        })
        .collect();
    assert_eq!(
        flushed,
        vec![
            ("awili:v1:work:c2".to_string(), "Work failed".to_string()),
            ("awili:v1:work:c3".to_string(), "Work failed".to_string()),
            ("awili:v1:work:c4".to_string(), "Work failed".to_string()),
            (
                "awili:v1:work:c5".to_string(),
                "Question waiting".to_string()
            ),
        ]
    );
    assert!(n.permission_changed(Permission::Authorized).is_empty());
}

/// 19: Denied drops what was pending and never falls back to Script Editor.
#[test]
fn denied_drops_pending_and_never_falls_back() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.observe_window(false, now - secs(30));
    n.handle(approval("a1"), None, None, now).unwrap();
    assert!(n.permission_changed(Permission::Denied).is_empty());
    assert_eq!(
        n.handle(approval("a2"), None, None, now),
        Err(Suppressed::Denied)
    );
    assert_eq!(n.status_line(), "off for Awili in System Settings");
}

/// 20: Unavailable posts through the fallback only when it is enabled.
#[test]
fn unavailable_falls_back_only_when_enabled() {
    let now = Instant::now();
    let mut n = Notifier::new(NotificationsConfig::default());
    n.observe_window(false, now - secs(30));
    n.handle(approval("a1"), None, None, now).unwrap();
    assert_eq!(
        posted(Ok(n.permission_changed(Permission::Unavailable("x")))),
        vec!["fallback:Approval needed"]
    );
    assert_eq!(
        posted(n.handle(approval("a2"), None, None, now)),
        vec!["fallback:Approval needed"]
    );
    assert_eq!(
        n.status_line(),
        "native notifications unavailable \u{2014} using Script Editor fallback"
    );
    n.set_config(NotificationsConfig {
        fallback_osascript: false,
        ..NotificationsConfig::default()
    });
    assert_eq!(n.handle(approval("a3"), None, None, now), Ok(Vec::new()));
    assert_eq!(n.status_line(), "native notifications unavailable");
}

/// 21: a failed native post turns native off and re-posts as the fallback.
#[test]
fn post_failed_turns_native_into_fallback() {
    let now = Instant::now();
    let mut n = away(NotificationsConfig::default(), now);
    let Ok(actions) = n.handle(approval("a1"), None, None, now) else {
        panic!("posts");
    };
    let Some(Action::PostNative(note)) = actions.into_iter().next() else {
        panic!("native first");
    };
    assert_eq!(
        n.post_failed(note.clone()),
        vec![Action::PostFallback(note)]
    );
    assert!(matches!(n.permission(), Permission::Unavailable(_)));
    assert_eq!(
        posted(n.handle(approval("a2"), None, None, now)),
        vec!["fallback:Approval needed"]
    );
}

/// 22: granted, (false, None) and (false, Some(1)).
#[test]
fn classify_authorization_table() {
    assert_eq!(classify_authorization(true, None), Permission::Authorized);
    assert_eq!(classify_authorization(false, None), Permission::Denied);
    assert!(matches!(
        classify_authorization(false, Some(1)),
        Permission::Unavailable(_)
    ));
    assert!(matches!(
        classify_authorization(true, Some(1)),
        Permission::Unavailable(_)
    ));
}

/// 23: every `RunStatus` × error × user_stopped.
#[test]
fn classify_work_end_table() {
    use EventKind::{Failed, Finished, Stopped};
    for err in [false, true] {
        for stop in [false, true] {
            let plain = if err { Failed } else { Finished };
            assert_eq!(classify_work_end(None, err, stop), Some(plain.clone()));
            assert_eq!(
                classify_work_end(Some(RunStatus::Completed), err, stop),
                Some(Finished)
            );
            assert_eq!(
                classify_work_end(Some(RunStatus::Failed), err, stop),
                Some(Failed)
            );
            let stopped = if stop { None } else { Some(Stopped) };
            assert_eq!(
                classify_work_end(Some(RunStatus::Stopped), err, stop),
                stopped
            );
            for s in [RunStatus::Started, RunStatus::Running, RunStatus::Unknown] {
                assert_eq!(classify_work_end(Some(s), err, stop), Some(plain.clone()));
            }
        }
    }
}

/// 24: no title, body, id or fallback script carries agent or tool text —
/// even when an event carries it in every field the glue could fill from
/// agent data (`key` from agent ids, `detail`): the approval prompt
/// `rm -rf ~/x TOKEN=abc`, the credential env var `GITEA_TOKEN`, a clarify
/// question and the failed run's error `401 key sk-live-123`.
#[test]
fn payload_never_leaks_agent_text() {
    let cfg = NotificationsConfig::default();
    let prompt = "rm -rf ~/x TOKEN=abc";
    let env_var = "GITEA_TOKEN";
    let question = "What is your TOKEN? rm -rf";
    let error = "401 key sk-live-123";
    let leaky = |target: Target, kind: EventKind, text: &str| Event {
        detail: Some(text.to_string()),
        ..event(target, kind, &format!("work:c1:{text}"))
    };
    let events = [
        leaky(
            work("c1"),
            EventKind::Approval {
                kind: "dangerous_command".into(),
            },
            prompt,
        ),
        leaky(work("c1"), EventKind::Credential { sudo: false }, env_var),
        leaky(work("c1"), EventKind::Credential { sudo: true }, prompt),
        leaky(work("c1"), EventKind::Question, question),
        leaky(work("c1"), EventKind::Failed, error),
        leaky(Target::Code(4), EventKind::Failed, error),
        leaky(work("c1"), EventKind::Stopped, error),
    ];
    for ev in events {
        let n = payload(&ev, &cfg);
        let script = osascript_script(&n);
        for text in [&n.title, &n.body, &n.id, &script] {
            for secret in ["rm -rf", "TOKEN", "GITEA_TOKEN", "sk-live", "401"] {
                assert!(!text.contains(secret), "{text:?} leaks {secret}");
            }
        }
    }
    let a = payload(&approval("a"), &cfg);
    assert_eq!(a.body, "awili \u{b7} dangerous command");
    assert_eq!(
        payload(&event(work("c1"), EventKind::Failed, "f"), &cfg).body,
        "awili \u{b7} open Awili to see why"
    );
}

/// 25: newlines, BEL and 200 × `é` → ≤ 80 chars ending in `…`.
#[test]
fn sanitize_strips_controls_and_truncates_on_char_boundary() {
    assert_eq!(sanitize("a\nb\u{7}\t  c", 80), "a b c");
    let long = "\u{e9}".repeat(200);
    let cut = sanitize(&long, 80);
    assert_eq!(cut.chars().count(), 80);
    assert!(cut.ends_with('\u{2026}'));
    assert!(cut.starts_with("\u{e9}\u{e9}"));
    assert_eq!(sanitize("short", 80), "short");
}

/// 26: `"` and `\` are removed; the title starts with `Awili — `.
#[test]
fn osascript_script_is_escaped_and_prefixed() {
    let n = Notification {
        id: "awili:v1:work:c1".into(),
        title: "Work \"done\"".into(),
        body: "my \\ \"folder\"".into(),
        sound: true,
        post: 0,
    };
    assert_eq!(
        osascript_script(&n),
        "display notification \"my  folder\" with title \"Awili \u{2014} Work done\""
    );
}

/// 27: active route ids round-trip; legacy Chat route ids still open their
/// Workshop conversation; garbage is rejected.
#[test]
fn route_ids_round_trip_and_reject_garbage() {
    for t in [
        work("6f0c1c1e-8f3a-4d2b-9a55-0c2f5b7d9e11"),
        Target::Code(42),
        Target::Test,
    ] {
        assert_eq!(parse_route(&route_id(&t)), Some(t));
    }
    assert_eq!(
        parse_route("awili:v1:chat:c-2"),
        Some(work("c-2")),
        "old notification clicks migrate to Workshop"
    );
    for bad in [
        "awili:v1:work:../x",
        "awili:v1:WORK:a",
        &format!("awili:v1:work:{}", "a".repeat(65)),
        "awili:v1:code:12a",
        "awili:v2:work:a",
        "awili:v1:test:1",
        "awili:v1:work:",
        "",
    ] {
        assert_eq!(parse_route(bad), None, "{bad:?}");
    }
}

/// FIX1f (review of b5a7d6a, P1): a switch turned off while the macOS prompt
/// is up drops what it parked; the answer posts only what is still wanted
/// (and the Settings test, which ignores switches).
#[test]
fn switches_turned_off_during_prompt_drop_parked_events() {
    let now = Instant::now();
    let park = |n: &mut Notifier| {
        n.observe_window(false, now - secs(30));
        n.handle(approval("a1"), None, None, now).unwrap();
        n.handle(event(work("c2"), EventKind::Failed, "f2"), None, None, now)
            .unwrap();
    };
    let mut off = Notifier::new(NotificationsConfig::default());
    park(&mut off);
    off.handle(
        event(Target::Test, EventKind::Test, "test"),
        None,
        None,
        now,
    )
    .unwrap();
    off.set_config(NotificationsConfig {
        enabled: false,
        ..NotificationsConfig::default()
    });
    assert_eq!(
        posted(Ok(off.permission_changed(Permission::Authorized))),
        vec!["native:Awili notifications are on"],
        "master switch off: only the Settings test survives"
    );
    let mut kind_off = Notifier::new(NotificationsConfig::default());
    park(&mut kind_off);
    kind_off.set_config(NotificationsConfig {
        failed: false,
        ..NotificationsConfig::default()
    });
    assert_eq!(
        posted(Ok(kind_off.permission_changed(Permission::Authorized))),
        vec!["native:Approval needed"],
        "`failed` off: the parked failure is dropped"
    );
    let mut fallback = Notifier::new(NotificationsConfig::default());
    park(&mut fallback);
    fallback.set_config(NotificationsConfig {
        approvals: false,
        ..NotificationsConfig::default()
    });
    assert_eq!(
        posted(Ok(fallback.permission_changed(Permission::Unavailable("x")))),
        vec!["fallback:Work failed"],
        "the fallback path re-checks the switches too"
    );
}

/// FIX1f (review of b5a7d6a, P1): a native post that fails after the owner
/// denied Awili stays a denial: no Script Editor fallback, permission stays
/// Denied, and later events are still refused.
#[test]
fn late_post_failure_after_denial_stays_denied() {
    let now = Instant::now();
    let mut n = away(NotificationsConfig::default(), now);
    let Ok(actions) = n.handle(approval("a1"), None, None, now) else {
        panic!("posts");
    };
    let Some(Action::PostNative(note)) = actions.into_iter().next() else {
        panic!("native first");
    };
    assert!(n.permission_changed(Permission::Denied).is_empty());
    assert!(n.post_failed(note).is_empty(), "no fallback after a denial");
    assert_eq!(n.permission(), &Permission::Denied);
    assert_eq!(n.status_line(), "off for Awili in System Settings");
    assert_eq!(
        n.handle(approval("a2"), None, None, now),
        Err(Suppressed::Denied)
    );
}

/// N1a-FIX1i (slice review, P1): a native post the backend reports failed
/// AFTER the owner switched its kind (or everything) off is dropped, not
/// re-posted through Script Editor; a kind still on falls back as before.
#[test]
fn post_failure_after_switch_off_is_dropped() {
    let now = Instant::now();
    let posted_native = |n: &mut Notifier, ev: Event| {
        let Ok(actions) = n.handle(ev, None, None, now) else {
            panic!("posts");
        };
        let Some(Action::PostNative(note)) = actions.into_iter().next() else {
            panic!("native");
        };
        note
    };
    let mut off = away(NotificationsConfig::default(), now);
    let note = posted_native(&mut off, approval("a1"));
    off.set_config(NotificationsConfig {
        enabled: false,
        ..NotificationsConfig::default()
    });
    assert!(off.post_failed(note).is_empty(), "master switch off");
    assert!(matches!(off.permission(), Permission::Unavailable(_)));
    let mut kind_off = away(NotificationsConfig::default(), now);
    let approval_note = posted_native(&mut kind_off, approval("a2"));
    let failed_note = posted_native(&mut kind_off, event(work("c2"), EventKind::Failed, "f2"));
    kind_off.set_config(NotificationsConfig {
        approvals: false,
        ..NotificationsConfig::default()
    });
    assert!(
        kind_off.post_failed(approval_note).is_empty(),
        "`approvals` off: the failed approval is dropped"
    );
    assert_eq!(
        posted(Ok(kind_off.post_failed(failed_note))),
        vec!["fallback:Work failed"],
        "`failed` still on: its failed post falls back"
    );
}

/// N1a-FIX1j (review of cd77d4e, P1): a failure is matched to its own post,
/// not to the id it shares with a later post: an approval and a failure on
/// one conversation share an id; after `approvals` is switched off, the
/// approval's failure is dropped while the failure's still falls back.
#[test]
fn post_failure_is_matched_to_its_own_post() {
    let now = Instant::now();
    let mut n = away(NotificationsConfig::default(), now);
    let native = |n: &mut Notifier, ev: Event| {
        let Ok(actions) = n.handle(ev, None, None, now) else {
            panic!("posts");
        };
        let Some(Action::PostNative(note)) = actions.into_iter().next() else {
            panic!("native");
        };
        note
    };
    let approval_note = native(&mut n, approval("a1"));
    let failed_note = native(&mut n, event(work("c1"), EventKind::Failed, "f1"));
    assert_eq!(approval_note.id, failed_note.id, "one conversation, one id");
    assert_ne!(approval_note.post, failed_note.post, "two posts");
    n.set_config(NotificationsConfig {
        approvals: false,
        ..NotificationsConfig::default()
    });
    assert!(
        n.post_failed(approval_note).is_empty(),
        "the approval's own kind is off"
    );
    assert_eq!(
        posted(Ok(n.post_failed(failed_note))),
        vec!["fallback:Work failed"]
    );
}

/// N1a-FIX1j (review of cd77d4e, P1): fail closed — a failure for a post the
/// kernel no longer remembers (64 newer posts later) or never posted, or one
/// reported twice, produces no fallback.
#[test]
fn unrecognised_post_failure_fails_closed() {
    let now = Instant::now();
    let mut n = away(NotificationsConfig::default(), now);
    let native = |n: &mut Notifier, key: &str| {
        let Ok(actions) = n.handle(approval(key), None, None, now) else {
            panic!("posts");
        };
        let Some(Action::PostNative(note)) = actions.into_iter().next() else {
            panic!("native");
        };
        note
    };
    let oldest = native(&mut n, "old");
    for i in 0..64 {
        native(&mut n, &format!("k{i}"));
    }
    let newest = native(&mut n, "new");
    assert!(n.post_failed(oldest).is_empty(), "evicted: fail closed");
    let never = Notification {
        post: 0,
        ..newest.clone()
    };
    assert!(n.post_failed(never).is_empty(), "never posted: fail closed");
    assert_eq!(
        posted(Ok(n.post_failed(newest.clone()))),
        vec!["fallback:Approval needed"]
    );
    assert!(
        n.post_failed(newest).is_empty(),
        "reported twice: once only"
    );
}

/// RC2-R64 (usability RC2 round 5, Minor 7: Settings › Notifications said
/// "macOS will ask the first time something needs you" while macOS already
/// allowed Awili): what macOS reports as the current permission is what
/// the page says; the macOS prompt's own answer still decides while it is
/// up, and a build that cannot post natively stays so.
#[test]
fn rc2r64_the_current_permission_is_what_macos_reports() {
    assert_eq!(classify_status(0), Permission::Unknown);
    assert_eq!(classify_status(1), Permission::Denied);
    for allowed in [2, 3, 4] {
        assert_eq!(classify_status(allowed), Permission::Authorized);
    }
    assert_eq!(classify_status(9), Permission::Unknown);

    let mut n = Notifier::new(NotificationsConfig::default());
    assert_eq!(
        n.status_line(),
        "macOS will ask the first time something needs you"
    );
    assert!(n.current_permission(Permission::Authorized).is_empty());
    assert_eq!(n.status_line(), "allowed");
    // Turned off later in System Settings, then on again.
    n.current_permission(Permission::Denied);
    assert_eq!(n.status_line(), "off for Awili in System Settings");
    n.current_permission(Permission::Authorized);
    assert_eq!(n.status_line(), "allowed");

    // While the macOS prompt is up, its answer decides.
    let now = Instant::now();
    let mut asking = Notifier::new(NotificationsConfig::default());
    asking.observe_window(false, now - secs(30));
    let first = asking.handle(finished("k1"), None, None, now);
    assert_eq!(first, Ok(vec![Action::RequestPermission]));
    assert!(asking.current_permission(Permission::Unknown).is_empty());
    assert_eq!(asking.permission(), &Permission::Requesting);

    // Allowed, then reset in System Settings: macOS will ask again.
    let mut waited = Notifier::new(NotificationsConfig::default());
    waited.set_initial_permission(Permission::Denied);
    waited.current_permission(Permission::Unknown);
    assert_eq!(waited.permission(), &Permission::Unknown);

    // No native posting here: stays so.
    let mut unbundled = Notifier::new(NotificationsConfig::default());
    unbundled.set_initial_permission(Permission::Unavailable("not running from an app bundle"));
    unbundled.current_permission(Permission::Authorized);
    assert_eq!(
        unbundled.permission(),
        &Permission::Unavailable("not running from an app bundle")
    );
}
