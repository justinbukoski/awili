//! RC2-CNCL: the Council page proof — the real `AwiliApp` (a private HOME, in
//! a subprocess) drawn in a test window and driven like the owner would.

use std::path::PathBuf;

use awili_core::ConvKind;
use gpui::{px, size};

use crate::exec_fixture::ANSWER_DEADLINE;
use crate::work::Mode;
use crate::AwiliApp;

/// How long a told-to-end stand-in process may take to be gone (a bound on
/// the ending, unchanged; waits for a stand-in to start use
/// [`ANSWER_DEADLINE`]).
const ENDS_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

const CHILD: &str = "AWILI_COUNCIL_TEST_HOME";

/// A `debug_selector` name the window can look up (it wants `'static`).
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn sel(i: usize) -> &'static str {
    leak(crate::council_view::seat_selector(i))
}

/// Run `test` in a subprocess with a private HOME. `Some(home)` inside the
/// child; `None` in the parent once the child passed.
fn isolated(test: &str) -> Option<PathBuf> {
    if let Some(home) = std::env::var_os(CHILD) {
        return Some(home.into());
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
    assert!(
        output.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
        "isolated test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    None
}

/// A configured install with memory off (nothing reaches the network).
fn quiet(app: &mut AwiliApp) {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.memory.enabled = false;
    app.work.engine.label = "Gemma 4 26B".into();
}

/// RC2-CNCL5: Council sits on the left rail right after Workshop, says what it
/// is, and opens a Council conversation.
#[gpui::test]
fn council_is_on_the_rail_after_workshop(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::council_is_on_the_rail_after_workshop").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        cx.notify();
    });
    window.run_until_parked();
    let workshop = window
        .debug_bounds("awili-rail-item-0")
        .expect("Workshop on the rail");
    let council = window
        .debug_bounds("awili-rail-item-1")
        .expect("Council on the rail");
    let backstage = window
        .debug_bounds("awili-rail-item-3")
        .expect("Backstage on the rail");
    assert!(workshop.top() < council.top() && council.top() < backstage.top());
    assert_eq!(Mode::Council.rail_hint(), "Ask several models at once");
    assert!(!Mode::Council.shows_model_chip());
    window.simulate_click(council.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |app, _cx| {
        assert_eq!(app.mode, Mode::Council);
        let active = app.sidebar.active_id().to_string();
        assert_eq!(
            app.conversations.get(&active).map(|c| c.kind),
            Some(ConvKind::Council)
        );
        assert!(app.councils.contains_key(&active), "seated on open");
        assert_eq!(
            crate::code_reopen::sidebar_kind_for(Mode::Council),
            ConvKind::Council
        );
        assert_eq!(
            crate::nav_route_to_mode(crate::palette::NavRoute::Council),
            Mode::Council
        );
    });
}

/// RC2-CNCL5: leaving Council and coming back reopens the same council, and
/// a cold launch returns to the Council page.
#[gpui::test]
fn council_reopens_last_council(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::council_reopens_last_council").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.set_mode(Mode::Council, cx);
        let first = app.sidebar.active_id().to_string();
        app.set_mode(Mode::Work, cx);
        assert_ne!(app.sidebar.active_id(), first);
        app.set_mode(Mode::Work, cx);
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), first, "the same council");
        assert_eq!(
            app.conversations
                .active()
                .iter()
                .filter(|c| c.kind == ConvKind::Council)
                .count(),
            1,
            "no second council"
        );
    });
    assert_eq!(
        crate::launch_landing::mode_from_key("council"),
        Some(Mode::Council)
    );
    assert_eq!(
        crate::launch_landing::mode_key(Mode::Council),
        Some("council")
    );
}

/// RC2-CNCL5: a new council opens with the last group used — the member bar
/// and the empty state name exactly those members.
#[gpui::test]
fn council_empty_state_names_the_group(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::council_empty_state_names_the_group").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.council_last_group = Some(vec![
            ("agent:codex".into(), "Codex".into()),
            ("in-app".into(), "Gemma 4 26B".into()),
        ]);
        app.set_mode(Mode::Council, cx);
        let page = app.active_council().expect("seated");
        assert_eq!(page.member_names(), ["Codex", "Gemma 4 26B"]);
        assert_eq!(
            crate::council_view::goes_to_line(&page.member_names()),
            "Goes to Codex and Gemma 4 26B"
        );
        cx.notify();
    });
    window.run_until_parked();
    assert!(window
        .debug_bounds(crate::council_view::EMPTY_STATE_SELECTOR)
        .is_some());
    let a = window.debug_bounds(sel(0)).expect("first member chip");
    let b = window.debug_bounds(sel(1)).expect("second member chip");
    assert!(a.left() < b.left(), "members in column order");
    assert!(window.debug_bounds(sel(2)).is_none());
    // With no group used before, the default: Claude Sonnet, Codex and the
    // model Work uses on this Mac.
    view.update(window, |app, _cx| {
        assert_eq!(
            crate::council_view::default_group(&app.work.engine)
                .iter()
                .map(|(_, l)| l.as_str())
                .collect::<Vec<_>>(),
            ["Claude Sonnet", "Codex", "Gemma 4 26B"]
        );
    });
}

// ---- RC2-CNCL6: one message to every member at once ---------------------------

use crate::council_run::stub;

/// Pump the app until `done`, or fail with `what` after 20 s.
fn settle(
    window: &mut gpui::VisualTestContext,
    view: &gpui::Entity<AwiliApp>,
    what: &str,
    done: impl Fn(&AwiliApp) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        window.run_until_parked();
        if view.read_with(window, |a, _| done(a)) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A slow answer: `words` one at a time, 400 ms apart, then usage.
fn slow(words: &[&str], usage: (u32, u32)) -> stub::Reply {
    let mut r = stub::Reply::text(words);
    // Slow enough that a loaded machine still sees the members mid-answer.
    r.gap_ms = 400;
    r.usage = Some(usage);
    r
}

/// A council of the in-app model (stub `a`) and Hermes (stub `b`).
fn two_member_council(app: &mut AwiliApp, a: &str, b: &str) {
    quiet(app);
    app.work.engine.base_url = format!("{a}/v1");
    app.work.engine.model = "gemma-4-26b-a4b-it-qat-mlx-4bit".into();
    app.council_fleet = crate::agent_chat::ChatRoute::Fleet(crate::agent_chat::FleetChatTarget {
        base_url: b.to_string(),
        model: "qwen38-27b".into(),
        api_key: None,
    });
    app.council_last_group = Some(vec![
        ("in-app".into(), "Gemma 4 26B".into()),
        ("provider:fleet".into(), "Hermes".into()),
    ]);
}

fn round_one(app: &AwiliApp) -> Option<&awili_council::Round> {
    app.active_council()?.council.round(1)
}

fn answer(app: &AwiliApp, member: &str) -> String {
    round_one(app)
        .and_then(|r| r.turn(member))
        .map(|t| t.text.clone())
        .unwrap_or_default()
}

/// Start a two-member council and send `ask`; returns the conversation id.
fn ask_two(
    window: &mut gpui::VisualTestContext,
    view: &gpui::Entity<AwiliApp>,
    a: &str,
    b: &str,
    ask: &str,
) -> String {
    view.update(window, |app, cx| {
        two_member_council(app, a, b);
        app.set_mode(Mode::Council, cx);
        app.composer.update(cx, |c, cx| c.set_text(ask, cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    })
}

/// RC2-CNCL6: one message goes to every member at the same time — both are
/// asked before either could finish, both stream side by side — and each
/// answer lands on its own card.
#[gpui::test]
fn one_message_streams_to_every_member_in_parallel(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::one_message_streams_to_every_member_in_parallel").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| slow(&["Keep ", "Gemma ", "for ", "now."], (180, 40)));
    let (b, seen_b) = stub::spawn(|_| slow(&["Move ", "coding ", "to ", "Qwen."], (200, 50)));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Gemma or Qwen on the Ultra?");
    settle(window, &view, "both members streaming at once", |app| {
        round_one(app).is_some_and(|r| {
            r.turns.len() == 2
                && r.turns
                    .iter()
                    .all(|t| t.status.is_running() && !t.text.is_empty())
        })
    });
    {
        let (sa, sb) = (seen_a.lock().unwrap(), seen_b.lock().unwrap());
        assert_eq!((sa.len(), sb.len()), (1, 1), "each member asked once");
        let gap = if sa[0].at > sb[0].at {
            sa[0].at - sb[0].at
        } else {
            sb[0].at - sa[0].at
        };
        assert!(
            gap < std::time::Duration::from_millis(400),
            "asked together, not one after the other ({gap:?})"
        );
        for s in [&sa[0], &sb[0]] {
            assert!(s.body.contains("Gemma or Qwen on the Ultra?"), "{}", s.body);
            assert!(
                s.body.contains("models on "),
                "the council brief: {}",
                s.body
            );
        }
    }
    window.run_until_parked();
    assert!(window
        .debug_bounds(sel_card(1, "m1"))
        .is_some_and(|c| window
            .debug_bounds(sel_card(1, "m2"))
            .is_some_and(|d| c != d)));
    settle(window, &view, "both answered", |app| {
        round_one(app).is_some_and(|r| !r.is_running())
    });
    view.read_with(window, |app, _| {
        assert_eq!(answer(app, "m1"), "Keep Gemma for now.");
        assert_eq!(answer(app, "m2"), "Move coding to Qwen.");
        let r = round_one(app).unwrap();
        assert_eq!(awili_council::round_summary(r), "All 2 answered");
        let rows = app.views[&conv].convo.messages();
        assert_eq!(rows.len(), 3, "the question and both answers");
        assert!(rows[1..]
            .iter()
            .all(|m| m.kind == awili_core::MessageKind::CouncilAnswer));
    });
}

fn sel_card(round: u32, member: &str) -> &'static str {
    leak(crate::council_view::card_selector(round, member))
}

/// A memory service whose answer Awili cannot check (`mandatory_policy:
/// null`), set as the production memory backend under `home`.
fn rejecting_memory(home: &std::path::Path) -> crate::MemoryCfg {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let mut s = s;
            let mut buf = [0u8; 8192];
            let _ = s.read(&mut buf);
            let body = r#"{"mandatory_policy": null, "items": []}"#;
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let foreman = home.join(".config/foreman");
    std::fs::create_dir_all(&foreman).unwrap();
    let token = foreman.join("v6-reader-token");
    std::fs::write(&token, "reader\n").unwrap();
    std::fs::write(
        foreman.join("memory-backend.conf"),
        format!(
            "FOREMAN_MEMORY_BACKEND=v65\nFOREMAN_MEMORY_V65_BASE={base}\n\
             FOREMAN_MEMORY_V65_READ_TOKEN={}\n",
            token.display()
        ),
    )
    .unwrap();
    crate::MemoryCfg {
        base_url: base,
        token: String::new(),
        min_priority: 90,
        enabled: true,
        recall: true,
        write: false,
    }
}

/// RC2-CNCL6: the memory gate runs once per message; when it refuses, the
/// whole send stops with ONE line — no member is asked, no per-member
/// errors — and the owner's message stays in the council.
#[gpui::test]
fn gate_refusal_stops_the_whole_send_once(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::gate_refusal_stops_the_whole_send_once") else {
        return;
    };
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["no"]));
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["no"]));
    let mem = rejecting_memory(&home);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    settle(window, &view, "the refusal line", |app| {
        round_one(app).is_some_and(|r| !r.left_out.is_empty())
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    window.run_until_parked();
    view.read_with(window, |app, _| {
        let r = round_one(app).unwrap();
        assert!(r.turns.is_empty(), "no member turn");
        assert_eq!(r.left_out.len(), 1, "one line: {:?}", r.left_out);
        assert!(r.left_out[0].starts_with(crate::recall::POLICY_REFUSAL_PREFIX));
        assert_eq!(
            app.toast.visible().map(|s| s.to_string()).as_deref(),
            Some(crate::submit_guard::POLICY_REFUSED_NOTICE)
        );
        assert_eq!(
            app.views[&conv].convo.messages().len(),
            1,
            "the owner's message stays"
        );
    });
    assert!(seen_a.lock().unwrap().is_empty() && seen_b.lock().unwrap().is_empty());
}

/// Wait until the database holds what `done` wants.
/// RC2-CNCLF4: every saved turn has its outcome (none still answering —
/// a turn is kept from the moment it is sent).
fn settled(turns: &[awili_persist::CouncilTurnRow]) -> bool {
    turns
        .iter()
        .all(|t| t.status != crate::council_run::RUNNING)
}

fn wait_store<T>(what: &str, read: impl Fn(&awili_persist::Store) -> Option<T>) -> T {
    let path = awili_persist::Store::default_path().expect("a data folder");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Ok(store) = awili_persist::Store::open(&path) {
            if let Some(v) = read(&store) {
                return v;
            }
        }
        assert!(std::time::Instant::now() < deadline, "never saved: {what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// RC2-CNCL6: every answer is saved with the member who gave it (name and
/// route frozen at send time), as a council-answer row of the conversation;
/// the seats are saved in column order.
#[gpui::test]
fn each_answer_is_saved_with_its_member(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::each_answer_is_saved_with_its_member").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Move to Qwen."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Which one?");
    settle(window, &view, "both answered", |app| {
        round_one(app).is_some_and(|r| r.turns.len() == 2 && !r.is_running())
    });
    let (members, turns, msgs) = wait_store("two answered turns", |s| {
        let turns = s.council_turns(&conv).ok()?;
        (turns.len() == 2 && settled(&turns) && turns.iter().all(|t| t.message_id.is_some())).then(
            || {
                (
                    s.council_members(&conv).unwrap(),
                    turns,
                    s.messages(&conv).unwrap(),
                )
            },
        )
    });
    assert_eq!(
        members
            .iter()
            .map(|m| (m.route.as_str(), m.label.as_str()))
            .collect::<Vec<_>>(),
        [("in-app", "Gemma 4 26B"), ("provider:fleet", "Hermes")]
    );
    for (member, label, route, text) in [
        ("m1", "Gemma 4 26B", "in-app", "Keep Gemma."),
        ("m2", "Hermes", "provider:fleet", "Move to Qwen."),
    ] {
        let t = turns.iter().find(|t| t.member_id == member).unwrap();
        assert_eq!((t.label.as_str(), t.route.as_str()), (label, route));
        assert_eq!(
            (t.status.as_str(), t.round, t.round_kind.as_str()),
            ("answered", 1, "ask")
        );
        assert!(t.owner_ts.is_some() && t.elapsed_ms.is_some() && t.sent_prompt_hash.is_some());
        let m = msgs.iter().find(|m| Some(m.id) == t.message_id).unwrap();
        assert_eq!(
            (m.role.as_str(), m.text.as_str(), m.kind.as_str()),
            ("assistant", text, "council_answer")
        );
    }
    assert_eq!(msgs.iter().filter(|m| m.role == "user").count(), 1);
}

/// RC2-CNCL6: what each member read and wrote is recorded under its own
/// name in the usage ledger (the System totals include councils), and kept
/// on its turn.
#[gpui::test]
fn usage_is_recorded_per_member(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::usage_is_recorded_per_member").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let mut ra = stub::Reply::text(&["Keep Gemma."]);
    ra.usage = Some((180, 40));
    let mut rb = stub::Reply::text(&["Move to Qwen."]);
    rb.usage = Some((200, 50));
    let (a, _) = stub::spawn(move |_| ra.clone());
    let (b, _) = stub::spawn(move |_| rb.clone());
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Which one?");
    settle(window, &view, "both answered", |app| {
        round_one(app).is_some_and(|r| r.turns.len() == 2 && !r.is_running())
    });
    view.read_with(window, |app, _| {
        let r = round_one(app).unwrap();
        assert_eq!(r.turn("m1").unwrap().usage, Some((180, 40)));
        assert_eq!(r.turn("m2").unwrap().usage, Some((200, 50)));
    });
    let mut by_model = wait_store("two usage rows", |s| {
        let rows = s.daily_usage_by_model(crate::feed::unix_now()).ok()?;
        (rows.len() >= 2).then_some(rows)
    });
    by_model.sort_by(|x, y| x.provider.cmp(&y.provider));
    assert_eq!(
        by_model
            .iter()
            .map(|u| (
                u.provider.as_str(),
                u.model.as_str(),
                u.input_tokens,
                u.output_tokens
            ))
            .collect::<Vec<_>>(),
        [
            ("fleet", "qwen38-27b", 200, 50),
            ("in-app", "gemma-4-26b-a4b-it-qat-mlx-4bit", 180, 40)
        ]
    );
    let turns = wait_store("the turns", |s| {
        let t = s.council_turns(&conv).ok()?;
        (t.len() == 2 && settled(&t)).then_some(t)
    });
    assert!(turns
        .iter()
        .all(|t| t.input_tokens.is_some() && t.output_tokens.is_some()));
}

// ---- RC2-CNCL7: who sits on a council -------------------------------------------

use crate::council_picker::{group_selector, pick_selector, ADD_REMOVE_SELECTOR};
use crate::executor_labels::{
    COUNCIL_FLEET, COUNCIL_LOCAL, COUNCIL_OTHER, COUNCIL_PAYG, COUNCIL_SUBS,
};

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn installed_agents(app: &mut AwiliApp, dir: &std::path::Path) {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// Marker agent CLIs in `dir` (installed, so their rows are pickable).
fn installed_agents(app: &mut AwiliApp, dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let make = |name: &str| {
        let bin = dir.join(name);
        std::fs::write(&bin, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Some(bin.display().to_string())
    };
    app.work.agents.claude_bin = make("claude");
    app.work.agents.codex_bin = make("codex");
    app.work.agents.zcode_bin = make("zcode");
    app.work.agents.dsh_bin = make("dsh");
}

fn picked(app: &AwiliApp) -> Vec<String> {
    app.active_council()
        .and_then(|p| p.picker.clone())
        .unwrap_or_default()
}

fn click(window: &mut gpui::VisualTestContext, selector: &'static str) {
    let b = window
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is on screen"));
    window.simulate_click(b.center(), gpui::Modifiers::default());
    window.run_until_parked();
}

/// RC2-CNCL7: "Add or remove" lists every model Awili can reach — on this
/// Mac, the subscriptions, pay as you go, the fleet and any provider model —
/// in that order, and there is no cap on how many sit on a council.
#[gpui::test]
fn picker_lists_every_reachable_model(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::picker_lists_every_reachable_model").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (a, _) = stub::spawn(|_| stub::Reply::text(&["x"]));
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["y"]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(1200.)));
    view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        installed_agents(app, dir.path());
        app.choices = vec![awili_config::ModelChoice {
            provider: "spark".into(),
            kind: awili_config::ProviderKind::OpenAi,
            base_url: b.clone(),
            model: "qwen38-27b".into(),
            api_key: None,
        }];
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    click(window, ADD_REMOVE_SELECTOR);
    let routes: Vec<String> = view.read_with(window, |app, _| {
        let groups = app.council_picks();
        assert_eq!(
            groups.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
            [
                COUNCIL_LOCAL,
                COUNCIL_SUBS,
                COUNCIL_PAYG,
                COUNCIL_FLEET,
                COUNCIL_OTHER
            ]
        );
        let labels: Vec<String> = groups
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|p| p.label.clone()))
            .collect();
        for want in [
            "Gemma 4 26B",
            "Claude Opus",
            "Claude Sonnet",
            "Claude Haiku",
            "Codex",
            "DeepSeek Flash",
            "GLM",
            "Hermes",
            "qwen38-27b",
        ] {
            assert!(labels.iter().any(|l| l == want), "{want} in {labels:?}");
        }
        assert!(groups
            .iter()
            .all(|(_, rows)| rows.iter().all(|p| p.offline.is_none())));
        assert_eq!(
            picked(app),
            ["in-app", "provider:fleet"],
            "the members ticked"
        );
        groups
            .into_iter()
            .flat_map(|(_, rows)| rows.into_iter().map(|p| p.route))
            .collect()
    });
    for r in &routes {
        assert!(
            window.debug_bounds(leak(pick_selector(r))).is_some(),
            "{r} drawn"
        );
    }
    // No cap: everything ticked is seated.
    view.update(window, |app, cx| {
        for r in &routes {
            if !picked(app).contains(r) {
                app.council_toggle_pick(r, cx);
            }
        }
        assert_eq!(picked(app).len(), routes.len());
        app.council_apply_picker(cx);
        assert_eq!(
            app.active_council().unwrap().member_names().len(),
            routes.len()
        );
    });
}

/// RC2-CNCL7: a model that cannot answer is listed with the reason and a
/// click does not tick it; nothing is hidden.
#[gpui::test]
fn offline_model_shows_why_and_cannot_be_ticked(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::offline_model_shows_why_and_cannot_be_ticked").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(1200.)));
    view.update(window, |app, cx| {
        quiet(app);
        installed_agents(app, dir.path());
        app.work.agents.codex_bin = Some(dir.path().join("no-codex").display().to_string());
        app.work.engine.base_url = String::new();
        app.council_last_group = Some(vec![
            ("agent:claude:sonnet".into(), "Claude Sonnet".into()),
            ("agent:codex".into(), "Codex".into()),
        ]);
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    click(window, ADD_REMOVE_SELECTOR);
    view.read_with(window, |app, _| {
        let rows: Vec<_> = app
            .council_picks()
            .into_iter()
            .flat_map(|(_, r)| r)
            .collect();
        let codex = rows.iter().find(|p| p.route == "agent:codex").unwrap();
        assert_eq!(codex.offline.as_deref(), Some("not installed on this Mac"));
        let local = rows.iter().find(|p| p.route == "in-app").unwrap();
        assert_eq!(local.offline.as_deref(), Some("not set up"));
        let hermes = rows.iter().find(|p| p.route == "provider:fleet").unwrap();
        assert_eq!(hermes.offline.as_deref(), Some("not set up"));
    });
    let codex = leak(pick_selector("agent:codex"));
    let codex_availability = leak("ui-text-council-pick-agent:codex-availability".to_string());
    view.read_with(window, |app, _| {
        assert_eq!(
            picked(app),
            ["agent:claude:sonnet", "agent:codex"],
            "the unavailable Codex row can remain selected"
        );
    });
    assert_eq!(
        crate::ui::interaction::test_util::tooltip_at(window, codex_availability).as_deref(),
        Some("not installed on this Mac"),
        "the selected unavailable row exposes its complete cause on the status face"
    );
    click(window, codex);
    view.read_with(window, |app, _| {
        assert_eq!(
            picked(app),
            ["agent:claude:sonnet"],
            "a selected offline row can be unticked"
        );
    });
    assert_eq!(
        crate::ui::interaction::test_util::tooltip_at(window, codex).as_deref(),
        Some("not installed on this Mac"),
        "the unticked disabled row still exposes its concrete reachability reason"
    );
    click(window, codex);
    click(window, leak(pick_selector("in-app")));
    click(window, leak(pick_selector("agent:glm")));
    view.read_with(window, |app, _| {
        assert_eq!(
            picked(app),
            ["agent:claude:sonnet", "agent:glm"],
            "offline rows stay unticked; a reachable one ticks"
        );
    });
}

/// RC2-CNCL7: a saved group is one tap, "Use these N" seats it, and the
/// group is remembered for the next council; "Save this group" keeps the
/// ticked models as a group across a relaunch.
#[gpui::test]
fn saved_group_is_one_tap(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::saved_group_is_one_tap").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| stub::Reply::text(&["x"]));
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["y"]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(1200.)));
    view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.council_groups = Some(vec![crate::council_view::SavedGroup {
            name: "Local only".into(),
            members: vec![("in-app".into(), "Gemma 4 26B".into())],
        }]);
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    click(window, ADD_REMOVE_SELECTOR);
    click(window, leak(group_selector(0)));
    view.read_with(window, |app, _| assert_eq!(picked(app), ["in-app"]));
    click(window, crate::council_picker::PICK_DONE_SELECTOR);
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert!(page.picker.is_none(), "the sheet closes");
        assert_eq!(page.member_names(), ["Gemma 4 26B"]);
        assert_eq!(
            app.council_last_group,
            Some(vec![("in-app".to_string(), "Gemma 4 26B".to_string())])
        );
    });
    // Save a new group: Gemma + Hermes.
    click(window, ADD_REMOVE_SELECTOR);
    click(window, leak(pick_selector("provider:fleet")));
    click(window, crate::council_picker::SAVE_GROUP_SELECTOR);
    view.read_with(window, |app, _| {
        let groups = app.council_groups.clone().unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1].name, "Gemma 4 26B, Hermes");
    });
    let saved = wait_store("the groups", |s| {
        s.get_app_state(crate::council_view::GROUPS_KEY)
            .ok()
            .flatten()
            .filter(|v| v.contains("Hermes"))
    });
    assert!(saved.contains("Local only"), "{saved}");
    let last = wait_store("the last group", |s| {
        s.get_app_state(crate::council_view::LAST_GROUP_KEY)
            .ok()
            .flatten()
    });
    assert_eq!(
        crate::council_view::group_from_json(&last),
        Some(vec![("in-app".to_string(), "Gemma 4 26B".to_string())])
    );
}

/// Opening the compact picker overlays Council without changing its main
/// layout; blank sheet-face clicks preserve the selected seats, while a true
/// outside click closes the picker.
#[gpui::test]
fn compact_picker_is_flow_neutral_and_outside_click_closes(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::compact_picker_is_flow_neutral_and_outside_click_closes").is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(820.), px(712.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.council_last_group = Some(vec![
            ("agent:codex".into(), "Codex".into()),
            ("in-app".into(), "Gemma 4 26B".into()),
        ]);
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    let center_before = window.debug_bounds("awili-center").expect("Council center");
    let council_scroll_before = view.read_with(window, |app, _| {
        app.active_council().unwrap().scroll.bounds()
    });
    assert!(
        council_scroll_before.size.width > px(0.) && council_scroll_before.size.height > px(0.),
        "the Council scroll viewport has measured non-zero bounds: {council_scroll_before:?}"
    );
    let members_before = view.read_with(window, |app, _| {
        app.active_council().unwrap().member_names()
    });

    click(window, ADD_REMOVE_SELECTOR);
    let center_open = window.debug_bounds("awili-center").expect("Council center");
    let council_scroll_open = view.read_with(window, |app, _| {
        app.active_council().unwrap().scroll.bounds()
    });
    assert_eq!(
        center_open, center_before,
        "the overlay keeps center bounds"
    );
    assert_eq!(
        council_scroll_open, council_scroll_before,
        "the overlay keeps the main Council scroll viewport bounds"
    );
    let face = window
        .debug_bounds("ui-sheet-council-picker-panel")
        .expect("compact picker face");
    // A quiet point in the title/header band (away from the close icon) is
    // inside the face but has no action of its own.
    window.simulate_click(
        gpui::point(face.center().x, face.top() + px(20.)),
        gpui::Modifiers::default(),
    );
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(picked(app), ["agent:codex", "in-app"]);
        assert_eq!(app.active_council().unwrap().member_names(), members_before);
    });

    // The picker is nested in Council's center, so the outside point must be
    // inside its actual scrim scope (the top inset), not elsewhere in window.
    let scrim = window
        .debug_bounds("ui-sheet-scrim")
        .expect("compact sheet scrim scope");
    let outside = [
        gpui::point(scrim.center().x, scrim.top() + px(1.)),
        gpui::point(scrim.left() + px(1.), scrim.center().y),
        gpui::point(scrim.right() - px(1.), scrim.center().y),
        gpui::point(scrim.center().x, scrim.bottom() - px(1.)),
    ]
    .into_iter()
    .find(|point| scrim.contains(point) && !face.contains(point))
    .expect("scrim has a measured point outside the picker face");
    assert!(
        scrim.contains(&outside),
        "outside click is in scrim: {scrim:?}"
    );
    assert!(
        !face.contains(&outside),
        "outside click misses face: {face:?}"
    );
    window.simulate_click(outside, gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert!(
            page.picker.is_none(),
            "outside-face click closes the picker"
        );
        assert_eq!(page.member_names(), members_before, "seats remain intact");
    });

    // Exercise the desktop Panel and its parent scrim at the same short
    // height: a click inside the measured face must not be mistaken for an
    // outside click, while a point beyond the face must still close it.
    window.simulate_resize(size(px(1400.), px(712.)));
    window.run_until_parked();
    let center_before = window.debug_bounds("awili-center").expect("Council center");
    let council_scroll_before = view.read_with(window, |app, _| {
        app.active_council().unwrap().scroll.bounds()
    });
    assert!(
        council_scroll_before.size.width > px(0.) && council_scroll_before.size.height > px(0.),
        "the desktop Council scroll viewport has measured bounds: {council_scroll_before:?}"
    );
    click(window, ADD_REMOVE_SELECTOR);
    assert_eq!(
        window.debug_bounds("awili-center").unwrap(),
        center_before,
        "desktop overlay keeps center bounds"
    );
    assert_eq!(
        view.read_with(window, |app, _| {
            app.active_council().unwrap().scroll.bounds()
        }),
        council_scroll_before,
        "desktop overlay keeps the main Council scroll viewport bounds"
    );
    let face = window
        .debug_bounds("ui-panel-council-picker")
        .expect("desktop picker face");
    window.simulate_click(
        gpui::point(face.center().x, face.top() + px(20.)),
        gpui::Modifiers::default(),
    );
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(picked(app), ["agent:codex", "in-app"]);
        assert_eq!(app.active_council().unwrap().member_names(), members_before);
    });
    let scrim = window
        .debug_bounds("council-picker-scrim")
        .expect("desktop scrim scope");
    let outside = [
        gpui::point(scrim.center().x, scrim.top() + px(1.)),
        gpui::point(scrim.left() + px(1.), scrim.center().y),
        gpui::point(scrim.right() - px(1.), scrim.center().y),
        gpui::point(scrim.center().x, scrim.bottom() - px(1.)),
    ]
    .into_iter()
    .find(|point| scrim.contains(point) && !face.contains(point))
    .expect("desktop scrim has a measured point outside the face");
    assert!(
        scrim.contains(&outside) && !face.contains(&outside),
        "outside click is within scrim scope and beyond the face: {scrim:?} / {face:?}"
    );
    window.simulate_click(outside, gpui::Modifiers::default());
    window.run_until_parked();
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert!(page.picker.is_none(), "desktop outside-face click closes");
        assert_eq!(page.member_names(), members_before, "seats remain intact");
    });
}

/// The Kit picker keeps its real model list in a bounded tracked viewport:
/// its face and fixed actions remain inside a short window at every zoom,
/// and the final model can be reached with the wheel at large zoom.
#[gpui::test]
fn picker_viewport_is_bounded_and_wheel_reaches_last_model(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::picker_viewport_is_bounded_and_wheel_reaches_last_model").is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    crate::council_picker::TEST_PICKER_SCROLL.with(|probe| *probe.borrow_mut() = None);
    let dir = tempfile::tempdir().unwrap();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(900.), px(712.)));
    view.update(window, |app, cx| {
        quiet(app);
        installed_agents(app, dir.path());
        app.work.agents.codex_bin = Some(dir.path().join("no-codex").display().to_string());
        app.work.engine.base_url.clear();
        app.choices = vec![awili_config::ModelChoice {
            provider: "spark".into(),
            kind: awili_config::ProviderKind::OpenAi,
            base_url: "http://127.0.0.1:1".into(),
            model: "qwen38-27b".into(),
            api_key: None,
        }];
        app.set_mode(Mode::Council, cx);
        app.council_open_picker(cx);
    });
    window.run_until_parked();
    let routes = view.read_with(window, |app, _| {
        app.council_picks()
            .into_iter()
            .flat_map(|(_, rows)| rows.into_iter().map(|row| row.route))
            .collect::<Vec<_>>()
    });
    let last = routes
        .last()
        .expect("the picker has reachable models")
        .clone();
    let last_selector = leak(pick_selector(&last));

    for scale in [0.9, 1.0, 1.25, 2.5] {
        crate::theme::set_ui_scale(scale);
        view.update(window, |_app, cx| cx.notify());
        window.run_until_parked();

        let (face_selector, is_sheet) = if scale > 1.0 {
            ("ui-sheet-council-picker-panel", true)
        } else {
            ("ui-panel-council-picker", false)
        };
        let face = window
            .debug_bounds(face_selector)
            .unwrap_or_else(|| panic!("picker face {face_selector} at zoom {scale}"));
        let list = window
            .debug_bounds("council-picker-list")
            .expect("the picker model list is present");
        let last_row = window
            .debug_bounds(last_selector)
            .expect("the final model row is laid out");
        let done = window
            .debug_bounds(crate::council_picker::PICK_DONE_SELECTOR)
            .expect("the Use these action stays visible");
        let viewport = window.update(|window, _| window.viewport_size());
        assert!(face.left() >= px(0.) && face.top() >= px(0.), "{face:?}");
        assert!(
            face.right() <= viewport.width && face.bottom() <= viewport.height,
            "{face:?} / {viewport:?}"
        );
        assert!(
            done.left() >= face.left()
                && done.right() <= face.right()
                && done.top() >= face.top()
                && done.bottom() <= face.bottom(),
            "complete action face escaped picker face: {done:?} / {face:?}"
        );
        assert!(
            done.size.width >= crate::theme::target() && done.size.height >= crate::theme::target(),
            "{done:?}"
        );
        assert!(
            list.size.height < face.size.height,
            "the list has its own bounded viewport: {list:?} / {face:?}"
        );
        assert!(
            list.size.height >= last_row.size.height,
            "the list can contain a complete final model row at zoom {scale}: {list:?} / {last_row:?}"
        );
        if is_sheet {
            assert!(
                face.size.height <= viewport.height,
                "sheet face exceeds its window: {face:?}"
            );
        }

        if scale == 2.5 {
            // Wheel budget is based on the measured target-to-list gap, with
            // bounded wheel steps and a progress guard.
            let mut target = window.debug_bounds(last_selector).unwrap();
            let mut list = window.debug_bounds("council-picker-list").unwrap();
            let initial_target = target;
            let initial_list = list;
            let picker_scroll = crate::council_picker::TEST_PICKER_SCROLL
                .with(|probe| probe.borrow().clone())
                .expect("renderer exposes its actual tracked picker scroll handle");
            let initial_offset = picker_scroll.offset();
            let initial_max_offset = picker_scroll.max_offset();
            let scroll_distance = f32::from(initial_max_offset.y).abs();
            let budget = (scroll_distance / 80.0).ceil() as usize + 4;
            let page_scroll = view.read_with(window, |app, _| {
                app.active_council().unwrap().scroll.clone()
            });
            let page_before = page_scroll.offset();
            let mut moved = false;
            let mut first_wheel = None;
            for _ in 0..budget {
                if target.left() >= list.left()
                    && target.right() <= list.right()
                    && target.top() >= list.top()
                    && target.bottom() <= list.bottom()
                {
                    break;
                }
                let gap = (target.bottom() - list.bottom()).max(px(1.));
                let delta = -gap.min(px(120.));
                let before = target.origin.y;
                window.simulate_event(gpui::ScrollWheelEvent {
                    position: list.center(),
                    delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), delta)),
                    touch_phase: gpui::TouchPhase::Moved,
                    ..Default::default()
                });
                window.run_until_parked();
                target = window.debug_bounds(last_selector).unwrap();
                list = window.debug_bounds("council-picker-list").unwrap();
                let offset_after = picker_scroll.offset();
                if first_wheel.is_none() {
                    first_wheel = Some((target, list, offset_after, picker_scroll.max_offset()));
                }
                if target.origin.y >= before {
                    break;
                }
                moved = true;
            }
            assert!(
                moved,
                "measured wheel steps move the picker list; initial target={:?}, list={:?}, scroll offset={:?}, max={:?}; first wheel={:?}",
                initial_target,
                initial_list,
                initial_offset,
                initial_max_offset,
                first_wheel
            );
            assert!(
                target.left() >= list.left()
                    && target.right() <= list.right()
                    && target.top() >= list.top()
                    && target.bottom() <= list.bottom(),
                "measured wheel steps did not reveal the complete final model face: {target:?} / {list:?}"
            );
            assert!(
                target.size.height >= crate::theme::target(),
                "model row lost its touch target: {target:?}"
            );
            click(window, last_selector);
            view.read_with(window, |app, _| {
                assert!(
                    app.active_council()
                        .unwrap()
                        .picker
                        .as_ref()
                        .unwrap()
                        .contains(&last),
                    "clicking the fully visible last row updates the ticked selection"
                )
            });
            // Bring the last row to the exact list edge, then prove another
            // wheel is consumed there instead of scrolling the Council page.
            window.simulate_event(gpui::ScrollWheelEvent {
                position: list.center(),
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-120.))),
                touch_phase: gpui::TouchPhase::Moved,
                ..Default::default()
            });
            window.run_until_parked();
            let edge_target = window.debug_bounds(last_selector).unwrap();
            window.simulate_event(gpui::ScrollWheelEvent {
                position: list.center(),
                delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-120.))),
                touch_phase: gpui::TouchPhase::Moved,
                ..Default::default()
            });
            window.run_until_parked();
            assert_eq!(
                page_scroll.offset(),
                page_before,
                "modal picker edge wheel must not scroll the Council page"
            );
            assert_eq!(
                window.debug_bounds(last_selector).unwrap().origin.y,
                edge_target.origin.y,
                "wheel at the list's exact edge is consumed by the picker"
            );
        }
    }
    crate::theme::set_ui_scale(1.0);
}

/// RC2-CNCL7: a member added between messages reads the whole council so
/// far — the earlier question and every earlier answer — the next time.
#[gpui::test]
fn added_member_sees_the_council_so_far(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::added_member_sees_the_council_so_far").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma for now."]));
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["I agree with Gemma."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(1200.)));
    view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.council_last_group = Some(vec![("in-app".into(), "Gemma 4 26B".into())]);
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen on the Ultra?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "round 1 answered", |app| {
        round_one(app).is_some_and(|r| !r.is_running() && r.turns.len() == 1)
    });
    window.run_until_parked();
    click(window, ADD_REMOVE_SELECTOR);
    click(window, leak(pick_selector("provider:fleet")));
    click(window, crate::council_picker::PICK_DONE_SELECTOR);
    view.update(window, |app, cx| {
        assert_eq!(
            app.active_council().unwrap().member_names(),
            ["Gemma 4 26B", "Hermes"]
        );
        app.composer
            .update(cx, |c, cx| c.set_text("And you, Hermes?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "round 2 answered", |app| {
        app.active_council()
            .and_then(|p| p.council.round(2))
            .is_some_and(|r| !r.is_running() && r.turns.len() == 2)
    });
    let seen = seen_b.lock().unwrap();
    assert_eq!(seen.len(), 1, "Hermes asked once, in round 2");
    let body = &seen[0].body;
    assert!(body.contains("Gemma or Qwen on the Ultra?"), "{body}");
    assert!(body.contains("Gemma 4 26B: Keep Gemma for now."), "{body}");
    assert!(body.contains("And you, Hermes?"), "{body}");
}

// ---- RC2-CNCL8: the round view ------------------------------------------------

/// Seat `members` (`(route, label)`) on the open council and add a round of
/// `kind` whose turns are settled as given (`None` = still answering with
/// `text` so far).
fn seed_round(
    app: &mut AwiliApp,
    kind: awili_council::RoundKind,
    owner: Option<&str>,
    turns: &[(&str, Option<awili_council::TurnStatus>, &str)],
) -> u32 {
    let page = app.councils.get_mut(app.sidebar.active_id()).unwrap();
    let n = page.council.next_round_number();
    let mut r = awili_council::Round::new(n, kind);
    r.owner_text = owner.map(str::to_string);
    for (member, _, _) in turns {
        let m = page.council.member(member).unwrap().clone();
        r.turns.push(awili_council::SeatTurn::running(
            &format!("t{n}{member}"),
            &m,
            0,
        ));
    }
    page.council.rounds.push(r);
    for (member, status, text) in turns {
        match status {
            Some(s) => {
                page.council.settle(n, member, s.clone(), text);
            }
            None => {
                let t = page.council.round_mut(n).unwrap().turn_mut(member).unwrap();
                t.text = text.to_string();
            }
        }
    }
    n
}

/// A council of `labels` (routes given) with nothing asked yet.
fn seated(
    window: &mut gpui::VisualTestContext,
    view: &gpui::Entity<AwiliApp>,
    group: &[(&str, &str)],
) {
    view.update(window, |app, cx| {
        quiet(app);
        app.council_last_group = Some(
            group
                .iter()
                .map(|(r, l)| (r.to_string(), l.to_string()))
                .collect(),
        );
        app.set_mode(Mode::Council, cx);
    });
}

const FOUR: [(&str, &str); 4] = [
    ("agent:claude:sonnet", "Claude Sonnet"),
    ("agent:codex", "Codex"),
    ("in-app", "Gemma 4 26B"),
    ("agent:dsh:deepseek-flash", "DeepSeek Flash"),
];

fn answered() -> Option<awili_council::TurnStatus> {
    Some(awili_council::TurnStatus::Answered)
}

/// RC2-CNCL8: on a wide window the members' answers sit side by side, one
/// column each, in member order.
#[gpui::test]
fn wide_window_shows_columns(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::wide_window_shows_columns").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(window, &view, &FOUR[..3]);
    view.update(window, |app, cx| {
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("Gemma or Qwen?"),
            &[
                ("m1", answered(), "Keep Gemma."),
                ("m2", answered(), "Move coding to Qwen."),
                ("m3", answered(), "Run a trial."),
            ],
        );
        cx.notify();
    });
    window.run_until_parked();
    let c: Vec<_> = ["m1", "m2", "m3"]
        .iter()
        .map(|m| window.debug_bounds(sel_card(1, m)).expect("card drawn"))
        .collect();
    assert_eq!(c[0].top(), c[1].top());
    assert_eq!(c[1].top(), c[2].top());
    assert!(c[0].right() <= c[1].left() && c[1].right() <= c[2].left());
    assert!(f32::from(c[0].size.width) >= crate::council_view::CARD_MIN_W);
    assert_eq!(
        crate::council_view::card_layout(1600. - 240., 1.0, 7),
        crate::council_view::CardLayout::Columns {
            per_row: crate::council_view::CARDS_PER_ROW,
            card_w: (1360. - 40. - 3. * crate::council_view::CARD_GAP) / 4.
        },
        "past four, cards wrap to a second row"
    );
}

/// RC2-CNCL8: on a narrow window the answers stack, full width.
#[gpui::test]
fn narrow_window_uses_two_tracks(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::narrow_window_uses_two_tracks").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    crate::theme::set_ui_scale(1.5);
    window.simulate_resize(size(px(800.), px(900.)));
    seated(window, &view, &FOUR[..3]);
    view.update(window, |app, cx| {
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("Gemma or Qwen?"),
            &[
                ("m1", answered(), "Keep Gemma."),
                ("m2", answered(), "Move coding to Qwen."),
                ("m3", answered(), "Run a trial."),
            ],
        );
        cx.notify();
    });
    window.run_until_parked();
    let c: Vec<_> = ["m1", "m2", "m3"]
        .iter()
        .map(|m| window.debug_bounds(sel_card(1, m)).expect("card drawn"))
        .collect();
    assert_eq!(c[0].top(), c[1].top());
    assert!(c[0].right() <= c[1].left());
    assert_eq!(c[0].left(), c[2].left());
    assert!(c[0].bottom() <= c[2].top());
    assert_eq!(c[0].size.width, c[1].size.width);
    // At the smaller zoom two cards now fit beside the floating history.
    // Verify the actual painted columns and their stage bounds.
    crate::theme::set_ui_scale(1.25);
    window.simulate_resize(size(px(860.), px(900.)));
    window.run_until_parked();
    let c: Vec<_> = ["m1", "m2", "m3"]
        .iter()
        .map(|m| window.debug_bounds(sel_card(1, m)).expect("card drawn"))
        .collect();
    assert_eq!(c[0].top(), c[1].top());
    assert!(c[0].right() <= c[1].left());
    assert_eq!(c[0].left(), c[2].left());
    assert!(c[0].bottom() <= c[2].top());
    assert!(c[0].left() >= crate::theme::z(crate::theme::STAGE_LEFT));
    assert!(c[1].right() <= px(860.));
    crate::theme::set_ui_scale(1.0);
}

/// RC2-CNCL8: once a newer round exists, an older round folds to its
/// header line, and "show" opens it again; the latest round leads.
#[gpui::test]
fn older_round_folds_when_a_new_one_exists(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::older_round_folds_when_a_new_one_exists").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(window, &view, &FOUR[..2]);
    view.update(window, |app, cx| {
        let both = [("m1", answered(), "a"), ("m2", answered(), "b")];
        seed_round(app, awili_council::RoundKind::Ask, Some("first"), &both);
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(sel_card(1, "m1")).is_some(),
        "alone, it shows"
    );
    view.update(window, |app, cx| {
        let both = [
            ("m1", answered(), "Same answer\nok"),
            ("m2", answered(), "Changed partly\nok"),
        ];
        seed_round(app, awili_council::RoundKind::Respond, None, &both);
        cx.notify();
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(sel_card(1, "m1")).is_none(),
        "round 1 folded"
    );
    assert!(
        window.debug_bounds(sel_card(2, "m1")).is_some(),
        "round 2 leads"
    );
    assert!(window
        .debug_bounds(leak(crate::council_view::round_selector(1)))
        .is_some());
    click(window, leak(crate::council_view::fold_selector(1)));
    assert!(
        window.debug_bounds(sel_card(1, "m1")).is_some(),
        "shown again"
    );
    click(window, leak(crate::council_view::fold_selector(1)));
    assert!(
        window.debug_bounds(sel_card(1, "m1")).is_none(),
        "folded again"
    );
}

/// RC2-CNCL8: a member that only shows its whole answer at the end says so
/// while it works; a streaming member's answer ends in a caret; Details
/// opens on a tap and names the route.
#[gpui::test]
fn buffering_member_says_it_answers_all_at_once(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::buffering_member_says_it_answers_all_at_once").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(window, &view, &FOUR);
    view.update(window, |app, cx| {
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("Gemma or Qwen?"),
            &[
                ("m1", None, "Keep Gemma as the"),
                ("m2", None, ""),
                ("m3", answered(), "Keep Gemma."),
                ("m4", None, ""),
            ],
        );
        let r = app.active_council().unwrap().council.round(1).unwrap();
        assert_eq!(
            crate::council_view::card_wait_line(r.turn("m4").unwrap()),
            "Working. DeepSeek Flash shows its whole answer when it is done."
        );
        assert_eq!(
            crate::council_view::card_wait_line(r.turn("m2").unwrap()),
            "Answering\u{2026}"
        );
        cx.notify();
    });
    window.run_until_parked();
    let wait = |m: &str| leak(crate::council_view::wait_selector(1, m));
    let caret = |m: &str| leak(crate::council_view::caret_selector(1, m));
    assert!(window.debug_bounds(wait("m4")).is_some());
    assert!(window.debug_bounds(wait("m2")).is_some());
    assert!(
        window.debug_bounds(caret("m1")).is_some(),
        "streaming: caret"
    );
    assert!(window.debug_bounds(caret("m3")).is_none(), "done: no caret");
    assert!(window.debug_bounds(wait("m1")).is_none());
    click(window, leak(crate::council_view::details_selector(1, "m3")));
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert!(page.details_open.contains("t1m3"));
        let r = page.council.round(1).unwrap();
        let d = crate::council_view::card_details(r, r.turn("m3").unwrap(), None);
        assert_eq!(d[0], ("Route", "in-app".to_string()));
        assert_eq!(d[1], ("It was sent", "your message".to_string()));
    });
}

// ---- RC2-CNCL9: stop one, stop all, continue, and the message that waits --------

/// Answers its first words, then keeps the connection open until stopped.
fn holds(first: &str) -> stub::Reply {
    let mut r = stub::Reply::text(&[first]);
    r.hold = true;
    r
}

fn turn_of(app: &AwiliApp, round: u32, member: &str) -> Option<awili_council::SeatTurn> {
    app.active_council()?
        .council
        .round(round)?
        .turn(member)
        .cloned()
}

fn status_of(app: &AwiliApp, member: &str) -> Option<awili_council::TurnStatus> {
    turn_of(app, 1, member).map(|t| t.status)
}

/// RC2-CNCL9: Stop on one card stops that member only: its words so far
/// stay, marked stopped; the other keeps answering until Stop all.
#[gpui::test]
fn stop_one_leaves_the_others_running(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::stop_one_leaves_the_others_running").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| holds("Keep Gemma as the"));
    let (b, _) = stub::spawn(|_| holds("Move coding"));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    ask_two(window, &view, &a, &b, "Which one?");
    settle(window, &view, "both streaming", |app| {
        round_one(app).is_some_and(|r| r.turns.iter().all(|t| !t.text.is_empty()))
    });
    assert!(window
        .debug_bounds(leak(crate::council_view::answering_hint_selector()))
        .is_some());
    click(window, leak(crate::council_view::stop_selector(1, "m1")));
    settle(window, &view, "Gemma stopped", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Stopped)
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(answer(app, "m1"), "Keep Gemma as the");
        assert!(
            status_of(app, "m2").unwrap().is_running(),
            "Hermes still answering"
        );
        assert_eq!(app.active_council().unwrap().runs.len(), 1);
    });
    assert!(window
        .debug_bounds(leak(crate::council_view::continue_selector(1, "m1")))
        .is_some());
    click(window, crate::council_view::STOP_ALL_SELECTOR);
    settle(window, &view, "Hermes stopped", |app| {
        status_of(app, "m2") == Some(awili_council::TurnStatus::Stopped)
    });
    view.read_with(window, |app, _| {
        assert_eq!(answer(app, "m2"), "Move coding")
    });
}

/// RC2-CNCL9: Esc in the message box stops every member.
#[gpui::test]
fn esc_stops_every_member(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::esc_stops_every_member").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| holds("Keep"));
    let (b, _) = stub::spawn(|_| holds("Move"));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    ask_two(window, &view, &a, &b, "Which one?");
    settle(window, &view, "both streaming", |app| {
        round_one(app).is_some_and(|r| r.turns.iter().all(|t| !t.text.is_empty()))
    });
    view.update_in(window, |app, window, cx| {
        let h = app.composer.read(cx).focus_handle().clone();
        window.focus(&h, cx);
    });
    window.simulate_keystrokes("escape");
    settle(window, &view, "both stopped", |app| {
        round_one(app).is_some_and(|r| {
            r.turns
                .iter()
                .all(|t| t.status == awili_council::TurnStatus::Stopped)
        })
    });
    view.read_with(window, |app, _| {
        assert_eq!(answer(app, "m1"), "Keep");
        assert_eq!(answer(app, "m2"), "Move");
        assert!(app.active_council().unwrap().runs.is_empty());
    });
}

/// RC2-CNCL9: a stopped answer keeps its words — on screen, in the
/// conversation and in `awili.db` — and "Continue it" asks that member to
/// go on from there, appending to what it wrote (the other member is not
/// asked again).
#[gpui::test]
fn stopped_answer_keeps_its_text(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::stopped_answer_keeps_its_text").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    let (a, seen_a) = stub::spawn(move |_| {
        if c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            holds("Keep Gemma as the")
        } else {
            stub::Reply::text(&[" default for now."])
        }
    });
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["Move coding to Qwen."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Which one?");
    settle(window, &view, "Gemma streaming, Hermes done", |app| {
        !answer(app, "m1").is_empty()
            && status_of(app, "m2") == Some(awili_council::TurnStatus::Answered)
    });
    click(window, leak(crate::council_view::stop_selector(1, "m1")));
    settle(window, &view, "Gemma stopped", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Stopped)
    });
    let stopped = wait_store("the stopped turn", |s| {
        s.council_turns(&conv)
            .ok()?
            .into_iter()
            .find(|t| t.member_id == "m1" && t.status == "stopped")
    });
    let msgs = wait_store("its words", |s| {
        let m = s.messages(&conv).ok()?;
        m.iter()
            .any(|m| Some(m.id) == stopped.message_id && m.text == "Keep Gemma as the")
            .then_some(m)
    });
    assert_eq!(msgs.len(), 3);
    click(
        window,
        leak(crate::council_view::continue_selector(1, "m1")),
    );
    settle(window, &view, "Gemma continued", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Answered)
    });
    view.read_with(window, |app, _| {
        assert_eq!(answer(app, "m1"), "Keep Gemma as the default for now.");
        assert_eq!(
            app.views[&conv].convo.messages().len(),
            3,
            "the continued answer replaces its own row"
        );
    });
    let body = seen_a.lock().unwrap()[1].body.clone();
    assert!(body.contains("You were stopped partway"), "{body}");
    assert!(body.contains("Keep Gemma as the"), "{body}");
    assert_eq!(seen_b.lock().unwrap().len(), 1, "Hermes not asked again");
    wait_store("one answer row for Gemma", |s| {
        let m = s.messages(&conv).ok()?;
        (m.len() == 3
            && m.iter()
                .any(|m| m.text == "Keep Gemma as the default for now."))
        .then_some(())
    });
}

/// RC2-CNCL9: a message typed while the council is answering waits its
/// turn, then goes to every member once the round is done.
#[gpui::test]
fn a_message_typed_while_answering_waits_its_turn(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::a_message_typed_while_answering_waits_its_turn").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| slow(&["Keep ", "Gemma."], (10, 2)));
    let (b, seen_b) = stub::spawn(|_| slow(&["Move ", "on."], (10, 2)));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Which one?");
    view.update(window, |app, cx| {
        assert!(app.active_council().unwrap().council.is_running());
        app.composer
            .update(cx, |c, cx| c.set_text("And for Code?", cx));
        app.submit_composer_draft(cx);
        assert_eq!(app.composer.read(cx).text(), "", "taken, not refused");
        let page = app.active_council().unwrap();
        assert_eq!(page.queued.len(), 1);
        assert_eq!(
            page.note.as_deref(),
            Some(crate::council_run::queued_note("And for Code?").as_str())
        );
        assert_eq!(app.views[&conv].convo.messages().len(), 1, "not sent yet");
    });
    settle(window, &view, "round 2 answered", |app| {
        app.active_council()
            .and_then(|p| p.council.round(2))
            .is_some_and(|r| !r.is_running() && r.turns.len() == 2)
    });
    view.read_with(window, |app, _| {
        let r2 = app.active_council().unwrap().council.round(2).unwrap();
        assert_eq!(r2.owner_text.as_deref(), Some("And for Code?"));
        assert!(app.active_council().unwrap().queued.is_empty());
    });
    for seen in [&seen_a, &seen_b] {
        let s = seen.lock().unwrap();
        assert_eq!(s.len(), 2);
        assert!(s[1].body.contains("And for Code?"));
        assert!(s[1].at >= s[0].at);
    }
}

// ---- RC2-CNCL10: a member that didn't answer -------------------------------------

fn fails() -> stub::Reply {
    let mut r = stub::Reply::text(&[]);
    r.status = 500;
    r
}

/// RC2-CNCL10: one member failing never touches the others: its card says
/// what happened in plain words (violet, not red), the other answers in
/// full, and the failure is kept in `awili.db`.
#[gpui::test]
fn one_failure_does_not_touch_the_others(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::one_failure_does_not_touch_the_others").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| fails());
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Move coding to Qwen."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Which one?");
    settle(window, &view, "round settled", |app| {
        round_one(app).is_some_and(|r| r.turns.len() == 2 && !r.is_running())
    });
    view.read_with(window, |app, _| {
        let r = round_one(app).unwrap();
        let t = r.turn("m1").unwrap();
        match &t.status {
            awili_council::TurnStatus::Failed {
                plain, needs_owner, ..
            } => {
                assert!(
                    plain.starts_with("Gemma 4 26B didn\u{2019}t answer"),
                    "{plain}"
                );
                assert!(!needs_owner);
            }
            s => panic!("{s:?}"),
        }
        assert_eq!(
            crate::council_view::card_tone(t),
            crate::theme::COUNCIL_LOOK
        );
        assert_eq!(answer(app, "m2"), "Move coding to Qwen.");
        assert_eq!(
            awili_council::round_summary(r),
            "1 answered \u{00B7} Gemma 4 26B didn\u{2019}t"
        );
    });
    assert!(window
        .debug_bounds(leak(crate::council_view::retry_selector(1, "m1")))
        .is_some());
    assert!(window
        .debug_bounds(leak(crate::council_view::retry_selector(1, "m2")))
        .is_none());
    let failed = wait_store("the failed turn", |s| {
        s.council_turns(&conv)
            .ok()?
            .into_iter()
            .find(|t| t.member_id == "m1" && t.status == "failed")
    });
    assert!(failed.message_id.is_none());
    assert!(failed.error_plain.is_some() && failed.error_detail.is_some());
}

/// RC2-CNCL10: Retry asks only that member again; the others are not
/// asked; the failed turn becomes an answer (also in `awili.db`).
#[gpui::test]
fn retry_reruns_only_that_member(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::retry_reruns_only_that_member").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    let (a, seen_a) = stub::spawn(move |_| {
        if c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            fails()
        } else {
            stub::Reply::text(&["Keep Gemma."])
        }
    });
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["Move coding to Qwen."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Which one?");
    settle(window, &view, "round settled", |app| {
        round_one(app).is_some_and(|r| r.turns.len() == 2 && !r.is_running())
    });
    click(window, leak(crate::council_view::retry_selector(1, "m1")));
    settle(window, &view, "Gemma answered on retry", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Answered)
    });
    view.read_with(window, |app, _| {
        assert_eq!(answer(app, "m1"), "Keep Gemma.");
        assert_eq!(
            round_one(app).unwrap().turns.len(),
            2,
            "same round, same seat"
        );
    });
    assert_eq!(seen_b.lock().unwrap().len(), 1, "Hermes not asked again");
    {
        let sa = seen_a.lock().unwrap();
        assert_eq!(sa.len(), 2);
        assert_eq!(sa[0].body, sa[1].body, "the same prompt");
    }
    wait_store("the retried turn", |s| {
        let t = s.council_turns(&conv).ok()?;
        (t.len() == 2
            && t.iter()
                .any(|t| t.member_id == "m1" && t.status == "answered" && t.message_id.is_some()))
        .then_some(())
    });
}

/// RC2-CNCL10: only a member the owner must do something about (signed
/// out) is red, with its "Sign in to …" action; any other failure is
/// violet with Retry and Remove; Remove takes it off the council.
#[gpui::test]
fn signed_out_member_is_the_only_red(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::signed_out_member_is_the_only_red").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(window, &view, &FOUR[..3]);
    let (codex, _) = crate::council_run::failure_sentence("Codex", None, "Error: 401 Unauthorized");
    assert_eq!(codex, "Codex needs you to sign in again.");
    assert!(crate::council_run::failure_sentence("Codex", None, "Not logged in").1);
    assert!(!crate::council_run::failure_sentence("Gemma", None, "read timeout").1);
    view.update(window, |app, cx| {
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("Which one?"),
            &[
                (
                    "m2",
                    Some(awili_council::TurnStatus::Failed {
                        plain: codex.clone(),
                        detail: "401".into(),
                        needs_owner: true,
                    }),
                    "",
                ),
                (
                    "m3",
                    Some(awili_council::TurnStatus::Failed {
                        plain: "Gemma 4 26B didn\u{2019}t answer: its service stopped responding."
                            .into(),
                        detail: "timeout".into(),
                        needs_owner: false,
                    }),
                    "",
                ),
                ("m1", answered(), "Keep Gemma."),
            ],
        );
        let r = app.active_council().unwrap().council.round(1).unwrap();
        assert_eq!(
            crate::council_view::card_tone(r.turn("m2").unwrap()),
            crate::theme::COUNCIL_ACT
        );
        assert_eq!(
            crate::council_view::card_tone(r.turn("m3").unwrap()),
            crate::theme::COUNCIL_LOOK
        );
        cx.notify();
    });
    window.run_until_parked();
    let sel = |f: fn(u32, &str) -> String, m: &str| leak(f(1, m));
    assert!(window
        .debug_bounds(sel(crate::council_view::signin_selector, "m2"))
        .is_some());
    assert!(window
        .debug_bounds(sel(crate::council_view::signin_selector, "m3"))
        .is_none());
    assert!(window
        .debug_bounds(sel(crate::council_view::signin_selector, "m1"))
        .is_none());
    for m in ["m2", "m3"] {
        assert!(window
            .debug_bounds(sel(crate::council_view::retry_selector, m))
            .is_some());
        assert!(window
            .debug_bounds(sel(crate::council_view::remove_selector, m))
            .is_some());
    }
    click(window, sel(crate::council_view::signin_selector, "m2"));
    view.read_with(window, |app, _| {
        let toast = app
            .toast
            .visible()
            .map(|s| s.to_string())
            .unwrap_or_default();
        assert!(toast.contains("codex login"), "{toast}");
    });
    click(window, sel(crate::council_view::remove_selector, "m3"));
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert_eq!(page.member_names(), ["Claude Sonnet", "Codex"]);
        assert_eq!(
            page.note.as_deref(),
            Some(crate::council_run::removed_note("Gemma 4 26B").as_str())
        );
        assert!(
            page.council.round(1).unwrap().turn("m3").is_some(),
            "its turn stays"
        );
    });
}

// ---- RC2-CNCL11: answer each other ---------------------------------------------

/// A stub that answers `first` to the owner's question and `reply` to an
/// "answer each other" ask (told apart by the ask's own words).
fn answers(first: &'static str, reply: &'static str) -> impl Fn(&str) -> stub::Reply {
    move |body: &str| {
        if body.contains("Start with one line: Same answer") {
            stub::Reply::text(&[reply])
        } else {
            stub::Reply::text(&[first])
        }
    }
}

fn rounds(app: &AwiliApp) -> usize {
    app.active_council().map_or(0, |p| p.council.rounds.len())
}

fn idle(app: &AwiliApp) -> bool {
    app.active_council()
        .is_some_and(|p| !p.council.is_running() && !p.council.rounds.is_empty())
}

/// RC2-CNCL11: one tap on "Let them answer each other" sends each member
/// the other members' answers by name, asking it to open with its stance.
#[gpui::test]
fn answer_each_other_sends_the_others_answers(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::answer_each_other_sends_the_others_answers").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(answers("Keep Gemma for now.", "Same answer\nStill Gemma."));
    let (b, seen_b) = stub::spawn(answers(
        "Move coding to Qwen.",
        "Changed partly\nTrial first.",
    ));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    ask_two(window, &view, &a, &b, "Gemma or Qwen?");
    settle(window, &view, "round 1 answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    window.run_until_parked();
    click(window, crate::council_view::RESPOND_SELECTOR);
    settle(window, &view, "round 2 answered", |app| {
        idle(app) && rounds(app) == 2
    });
    view.read_with(window, |app, _| {
        let r2 = app.active_council().unwrap().council.round(2).unwrap();
        assert_eq!(r2.kind, awili_council::RoundKind::Respond);
        assert_eq!(r2.turns.len(), 2);
        assert!(r2.owner_text.is_none());
    });
    let ga = seen_a.lock().unwrap()[1].body.clone();
    assert!(ga.contains("Hermes: Move coding to Qwen."), "{ga}");
    assert!(
        ga.contains("You (Gemma 4 26B): Keep Gemma for now."),
        "{ga}"
    );
    assert!(
        ga.contains("Same answer, Changed my answer, or Changed partly"),
        "{ga}"
    );
    let hb = seen_b.lock().unwrap()[1].body.clone();
    assert!(hb.contains("Gemma 4 26B: Keep Gemma for now."), "{hb}");
}

/// RC2-CNCL11: each reply's opening line becomes a tag on its card (and
/// leaves the body), and the round header says who kept and who changed.
#[gpui::test]
fn stance_tag_shows_on_the_card(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::stance_tag_shows_on_the_card").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(answers("Keep Gemma.", "Same answer\nStill Gemma."));
    let (b, _) = stub::spawn(answers("Move to Qwen.", "Changed partly\nTrial first."));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Gemma or Qwen?");
    settle(window, &view, "round 1 answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    view.update(window, |app, cx| app.council_answer_each_other(cx));
    settle(window, &view, "round 2 answered", |app| {
        idle(app) && rounds(app) == 2
    });
    window.run_until_parked();
    let tag = |m: &str| leak(crate::council_view::stance_selector(2, m));
    assert!(window.debug_bounds(tag("m1")).is_some());
    assert!(window.debug_bounds(tag("m2")).is_some());
    assert!(window
        .debug_bounds(leak(crate::council_view::stance_selector(1, "m1")))
        .is_none());
    view.read_with(window, |app, _| {
        let r2 = app.active_council().unwrap().council.round(2).unwrap();
        let g = r2.turn("m1").unwrap();
        assert_eq!(g.stance, Some(awili_council::Stance::Same));
        assert_eq!(g.body(), "Still Gemma.");
        assert_eq!(
            r2.turn("m2").unwrap().stance,
            Some(awili_council::Stance::Partly)
        );
        assert_eq!(
            awili_council::round_summary(r2),
            "Gemma 4 26B kept its answer \u{00B7} Hermes changed its answer in part"
        );
    });
    wait_store("the stances", |s| {
        let t = s.council_turns(&conv).ok()?;
        let st: Vec<_> = t.iter().filter_map(|t| t.stance.clone()).collect();
        (st.len() == 2).then_some(())
    });
}

/// RC2-CNCL11: with "answer each other automatically: 2", each message is
/// followed by exactly two reply rounds — the cap counts per message — and
/// the setting is kept with the council.
#[gpui::test]
fn auto_rounds_stop_at_the_setting(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::auto_rounds_stop_at_the_setting").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(answers("Keep Gemma.", "Same answer\nStill."));
    let (b, seen_b) = stub::spawn(answers("Move to Qwen.", "Changed my answer\nOK."));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.set_mode(Mode::Council, cx);
        app.sidebar.active_id().to_string()
    });
    view.update(window, |app, cx| {
        // The control is on the page once there is a round; set it first.
        app.council_set_auto_rounds(2, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "three rounds", |app| {
        idle(app) && rounds(app) == 3
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(rounds(app), 3, "1 ask + 2 automatic replies, then it stops");
        let kinds: Vec<_> = app
            .active_council()
            .unwrap()
            .council
            .rounds
            .iter()
            .map(|r| r.kind)
            .collect();
        use awili_council::RoundKind::{Ask, Respond};
        assert_eq!(kinds, [Ask, Respond, Respond]);
    });
    assert_eq!(seen_a.lock().unwrap().len(), 3);
    assert_eq!(seen_b.lock().unwrap().len(), 3);
    // Shown on the page; a click changes it.
    assert!(window
        .debug_bounds(leak(crate::council_view::auto_selector(2)))
        .is_some());
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("And for Code?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "six rounds", |app| {
        idle(app) && rounds(app) == 6
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(rounds(app), 6, "two more, per message")
    });
    let saved = wait_store("the setting", |s| {
        s.get_app_state(&crate::council_view::settings_key(&conv))
            .ok()
            .flatten()
    });
    assert_eq!(crate::council_view::auto_rounds_from_json(&saved), 2);
    // Six rounds now include their recorded summaries and track headers.
    // Bring the control into its actual scroll viewport before clicking it.
    let selector = leak(crate::council_view::auto_selector(0));
    for _ in 0..4 {
        let target = window.debug_bounds(selector).expect("Off is rendered");
        let viewport = view.read_with(window, |app, _| {
            app.active_council().unwrap().scroll.bounds()
        });
        let delta = if target.top() < viewport.top() {
            viewport.top() - target.top()
        } else if target.bottom() > viewport.bottom() {
            viewport.bottom() - target.bottom()
        } else {
            break;
        };
        window.simulate_event(gpui::ScrollWheelEvent {
            position: viewport.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), delta)),
            touch_phase: gpui::TouchPhase::Moved,
            ..Default::default()
        });
        window.run_until_parked();
    }
    let target = window.debug_bounds(selector).unwrap();
    let viewport = view.read_with(window, |app, _| {
        app.active_council().unwrap().scroll.bounds()
    });
    assert!(
        viewport.contains(&target.center()),
        "{target:?} / {viewport:?}"
    );
    click(window, leak(crate::council_view::auto_selector(0)));
    view.read_with(window, |app, _| {
        assert_eq!(app.active_council().unwrap().auto_rounds, 0)
    });
}

// ---- RC2-CNCL12: time and cost ---------------------------------------------------

/// A council of one priced provider model (DeepSeek Flash through
/// `[[providers]]`, stub `d`) and Hermes (stub `b`).
fn priced_council(app: &mut AwiliApp, d: &str, b: &str) {
    two_member_council(app, d, b);
    app.choices = vec![awili_config::ModelChoice {
        provider: "deepseek".into(),
        kind: awili_config::ProviderKind::OpenAi,
        base_url: d.to_string(),
        model: "deepseek-flash".into(),
        api_key: None,
    }];
    app.council_last_group = Some(vec![
        (
            "model:deepseek:deepseek-flash".into(),
            "deepseek-flash".into(),
        ),
        ("provider:fleet".into(), "Hermes".into()),
    ]);
}

/// RC2-CNCL12: an answered card's footer gives its time, its words and
/// its cost; the round header and the member bar add them up.
#[gpui::test]
fn card_footer_shows_time_and_cost(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::card_footer_shows_time_and_cost").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let mut rd = stub::Reply::text(&["Count failed tool calls."]);
    rd.usage = Some((100_000, 50_000));
    let (d, _) = stub::spawn(move |_| rd.clone());
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        priced_council(app, &d, &b);
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Which one?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "both answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    window.run_until_parked();
    assert!(window
        .debug_bounds(leak(crate::council_view::footer_selector(1, "m1")))
        .is_some());
    assert!(window
        .debug_bounds(crate::council_view::TALLY_SELECTOR)
        .is_some());
    view.read_with(window, |app, _| {
        let r = round_one(app).unwrap();
        let f = crate::run_footer::council_footer(r.turn("m1").unwrap()).unwrap();
        assert!(f.ends_with(" s \u{00B7} 4 words \u{00B7} $0.09"), "{f}");
        let h = crate::run_footer::council_footer(r.turn("m2").unwrap()).unwrap();
        assert!(h.ends_with("2 words \u{00B7} free"), "{h}");
        assert!(crate::run_footer::council_round_tally(r).ends_with("$0.09"));
        let tally = crate::run_footer::council_tally(&app.active_council().unwrap().council);
        assert!(
            tally.starts_with("1 round \u{00B7} ") && tally.ends_with("$0.09"),
            "{tally}"
        );
    });
}

/// RC2-CNCL12: a subscription member says "included", a model at home
/// "free", GLM "billed by Z.ai"; nothing is guessed.
#[gpui::test]
fn subscription_member_says_included(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::subscription_member_says_included").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(
        window,
        &view,
        &[
            ("agent:claude:sonnet", "Claude Sonnet"),
            ("agent:codex", "Codex"),
            ("in-app", "Gemma 4 26B"),
            ("agent:glm", "GLM"),
        ],
    );
    view.update(window, |app, cx| {
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("Which one?"),
            &[
                ("m1", answered(), "Keep Gemma for now."),
                ("m2", answered(), "Move coding to Qwen."),
                ("m3", answered(), "Keep Gemma."),
                ("m4", answered(), "Either."),
            ],
        );
        let page = app.councils.get_mut(app.sidebar.active_id()).unwrap();
        for t in &mut page.council.rounds[0].turns {
            t.elapsed_ms = Some(9_000);
        }
        let r = &page.council.rounds[0];
        let f = |m: &str| crate::run_footer::council_footer(r.turn(m).unwrap()).unwrap();
        assert_eq!(f("m1"), "9 s \u{00B7} 4 words \u{00B7} included");
        assert_eq!(f("m2"), "9 s \u{00B7} 4 words \u{00B7} included");
        assert_eq!(f("m3"), "9 s \u{00B7} 2 words \u{00B7} free");
        assert_eq!(f("m4"), "9 s \u{00B7} 1 word \u{00B7} billed by Z.ai");
        assert_eq!(
            crate::run_footer::council_cost_word("agent:dsh:deepseek-flash", None, None),
            "cost not reported"
        );
        cx.notify();
    });
    window.run_until_parked();
    for m in ["m1", "m2", "m3", "m4"] {
        assert!(window
            .debug_bounds(leak(crate::council_view::footer_selector(1, m)))
            .is_some());
    }
}

/// RC2-CNCL12: what a council spends counts in Awili's own totals — the
/// session and today figures, and the day's ledger by model.
#[gpui::test]
fn council_usage_reaches_system_totals(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::council_usage_reaches_system_totals").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let mut rd = stub::Reply::text(&["Count failed tool calls."]);
    rd.usage = Some((100_000, 50_000));
    let (d, _) = stub::spawn(move |_| rd.clone());
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        priced_council(app, &d, &b);
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.cost.chip_label(crate::feed::unix_now()), None);
        app.composer
            .update(cx, |c, cx| c.set_text("Which one?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "both answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    view.read_with(window, |app, _| {
        assert_eq!(
            app.cost.chip_label(crate::feed::unix_now()).as_deref(),
            Some("$0.09 session \u{00B7} $0.09 today")
        );
    });
    let rows = wait_store("the ledger", |s| {
        let rows = s.daily_usage_by_model(crate::feed::unix_now()).ok()?;
        rows.iter()
            .any(|u| u.provider == "deepseek" && u.model == "deepseek-flash")
            .then_some(rows)
    });
    let ds = rows.iter().find(|u| u.provider == "deepseek").unwrap();
    assert_eq!((ds.input_tokens, ds.output_tokens), (100_000, 50_000));
}

// ---- RC2-CNCL13: ask one to sum up ---------------------------------------------------

/// RC2-CNCL13: "Ask one to sum up" → the chosen member reads the whole
/// council and writes one combined answer, shown full width (RC2-R3: after
/// the round it sums up, in order; it was at the top) as
/// "Summary by …"; nobody else is asked.
#[gpui::test]
fn sum_up_card_leads_the_round(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::sum_up_card_leads_the_round").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma for now."]));
    let (b, seen_b) = stub::spawn(|body: &str| {
        if body.contains("Write one combined answer") {
            stub::Reply::text(&["Both say keep Gemma; trial Qwen."])
        } else {
            stub::Reply::text(&["Move coding to Qwen."])
        }
    });
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    ask_two(window, &view, &a, &b, "Gemma or Qwen?");
    settle(window, &view, "round 1 answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    window.run_until_parked();
    click(window, crate::council_view::SUM_UP_SELECTOR);
    click(
        window,
        leak(crate::council_view::sum_up_member_selector("m2")),
    );
    settle(window, &view, "the summary", |app| {
        idle(app) && rounds(app) == 2
    });
    window.run_until_parked();
    view.read_with(window, |app, _| {
        let r2 = app.active_council().unwrap().council.round(2).unwrap();
        assert_eq!(r2.kind, awili_council::RoundKind::SumUp);
        assert_eq!(r2.turns.len(), 1);
        assert_eq!(r2.turns[0].text, "Both say keep Gemma; trial Qwen.");
        assert_eq!(crate::council_view::round_title(r2), "Summary by Hermes");
    });
    let summary = window
        .debug_bounds(sel_card(2, "m2"))
        .expect("summary drawn");
    let round_one_head = window
        .debug_bounds(leak(crate::council_view::round_selector(1)))
        .expect("round 1 header");
    // RC2-R3 (round 3 Major 3): the summary comes after the round it sums
    // up, in order, not above Round 1.
    assert!(
        summary.top() > round_one_head.top(),
        "the summary follows round 1"
    );
    let center = window.debug_bounds("awili-center").unwrap();
    assert!(
        f32::from(summary.size.width) > f32::from(center.size.width) * 0.8,
        "full width"
    );
    assert_eq!(seen_a.lock().unwrap().len(), 1, "Gemma not asked");
    let sb = seen_b.lock().unwrap();
    assert_eq!(sb.len(), 2);
    assert!(
        sb[1].body.contains("Gemma 4 26B: Keep Gemma for now."),
        "{}",
        sb[1].body
    );
    assert!(
        sb[1].body.contains("where the council agrees"),
        "{}",
        sb[1].body
    );
}

// ---- RC2-CNCL14: take an answer into Work ------------------------------------------

/// RC2-CNCL14: "Continue in Work" opens a new Work conversation with the
/// owner's question and that answer in the box — nothing is sent — and the
/// council is left as it was.
#[gpui::test]
fn continue_in_work_carries_question_and_answer(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::continue_in_work_carries_question_and_answer").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(window, &view, &FOUR[1..3]);
    let council = view.update(window, |app, cx| {
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("Which model should run Work on the Ultra?"),
            &[
                ("m1", answered(), "Move coding to Qwen."),
                ("m2", answered(), "Keep Gemma for now."),
            ],
        );
        cx.notify();
        app.sidebar.active_id().to_string()
    });
    window.run_until_parked();
    click(
        window,
        leak(crate::council_view::continue_in_work_selector(1, "m2")),
    );
    view.read_with(window, |app, cx| {
        assert_eq!(app.mode, Mode::Work);
        let active = app.sidebar.active_id().to_string();
        assert_ne!(active, council);
        assert_eq!(
            app.conversations.get(&active).map(|c| c.kind),
            Some(ConvKind::Work)
        );
        let want = crate::council_run::work_box_text(
            Some("Which model should run Work on the Ultra?"),
            "Gemma 4 26B",
            "Keep Gemma for now.",
        );
        assert_eq!(app.composer.read(cx).text(), want, "in the box, not sent");
        assert!(want.contains("Gemma 4 26B answered:\nKeep Gemma for now."));
        assert!(app
            .views
            .get(&active)
            .is_none_or(|v| v.convo.messages().is_empty()));
        assert_eq!(
            app.toast.visible().map(|s| s.to_string()),
            Some(crate::council_run::continue_in_work_note(
                "Gemma 4 26B",
                true
            ))
        );
        assert_eq!(
            app.councils[&council].council.rounds.len(),
            1,
            "council untouched"
        );
    });
}

/// RC2-CNCL14: the member continues as the Work helper when it can be one
/// (Codex → Codex, Gemma → in-app, Hermes → the fleet gateway); a provider
/// model leaves Work on its default helper.
#[gpui::test]
fn continue_in_work_preselects_the_member(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::continue_in_work_preselects_the_member").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(
        window,
        &view,
        &[("agent:codex", "Codex"), ("provider:fleet", "Hermes")],
    );
    view.update(window, |app, cx| {
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("Which one?"),
            &[("m1", answered(), "Qwen."), ("m2", answered(), "Gemma.")],
        );
        cx.notify();
    });
    window.run_until_parked();
    click(
        window,
        leak(crate::council_view::continue_in_work_selector(1, "m1")),
    );
    view.update(window, |app, cx| {
        let codex = crate::work::WorkExecutor::Agent {
            engine: awili_code::CodeEngine::Codex,
            model: None,
        };
        assert_eq!(app.work.executor, codex);
        assert_eq!(
            app.work.chip_picked.get(app.sidebar.active_id()),
            Some(&codex)
        );
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    click(
        window,
        leak(crate::council_view::continue_in_work_selector(1, "m2")),
    );
    view.read_with(window, |app, _| {
        assert_eq!(app.work.executor, crate::work::WorkExecutor::Gateway);
    });
    assert_eq!(
        crate::council_run::work_helper_for("in-app"),
        Some(crate::work::WorkExecutor::InApp)
    );
    assert_eq!(
        crate::council_run::work_helper_for("model:spark:qwen38-27b"),
        None
    );
}

// ---- RC2-CNCL15: back after a relaunch, and the export -----------------------------

/// RC2-CNCL15: a council comes back after a relaunch as it was — members,
/// rounds, answers, stances, the failure in plain words, time and words —
/// the sidebar names its members, and Retry still sends exactly what the
/// member was sent before.
#[gpui::test]
fn council_comes_back_after_a_relaunch(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::council_comes_back_after_a_relaunch").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let mut first = stub::Reply::text(&["Keep Gemma for now."]);
    first.usage = Some((180, 40));
    let (a, _) = stub::spawn(move |body: &str| {
        if body.contains("Start with one line: Same answer") {
            stub::Reply::text(&["Same answer\nStill Gemma."])
        } else {
            first.clone()
        }
    });
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    let (b, seen_b) =
        stub::spawn(
            move |_| match c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => stub::Reply::text(&["Move coding to Qwen."]),
                1 => fails(),
                _ => stub::Reply::text(&["Changed partly\nTrial first."]),
            },
        );
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Gemma or Qwen?");
    settle(window, &view, "round 1", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    view.update(window, |app, cx| app.council_answer_each_other(cx));
    settle(window, &view, "round 2", |app| {
        idle(app) && rounds(app) == 2
    });
    wait_store("all four turns", |s| {
        let t = s.council_turns(&conv).ok()?;
        (t.len() == 4 && settled(&t)).then_some(())
    });
    // The relaunch: a second Awili on the same database.
    let (view2, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1600.), px(900.)));
    view2.update(window2, |app, cx| {
        two_member_council(app, &a, &b);
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), conv, "the council is open again");
        let page = app.active_council().expect("restored");
        assert_eq!(page.member_names(), ["Gemma 4 26B", "Hermes"]);
        assert_eq!(page.council.rounds.len(), 2);
        let r1 = page.council.round(1).unwrap();
        assert_eq!(r1.owner_text.as_deref(), Some("Gemma or Qwen?"));
        assert_eq!(r1.turn("m1").unwrap().text, "Keep Gemma for now.");
        assert_eq!(r1.turn("m1").unwrap().usage, Some((180, 40)));
        assert!(crate::run_footer::council_footer(r1.turn("m1").unwrap())
            .unwrap()
            .ends_with("4 words \u{00B7} free"));
        let r2 = page.council.round(2).unwrap();
        assert_eq!(r2.kind, awili_council::RoundKind::Respond);
        let g = r2.turn("m1").unwrap();
        assert_eq!(g.stance, Some(awili_council::Stance::Same));
        assert_eq!(g.body(), "Still Gemma.");
        assert!(matches!(
            &r2.turn("m2").unwrap().status,
            awili_council::TurnStatus::Failed { plain, .. } if plain.starts_with("Hermes didn\u{2019}t answer")
        ));
        assert_eq!(
            crate::council_view::sidebar_line(page),
            "Gemma, Hermes \u{00B7} 2 rounds"
        );
        cx.notify();
    });
    window2.run_until_parked();
    assert!(window2.debug_bounds(sel_card(2, "m2")).is_some());
    click(window2, leak(crate::council_view::retry_selector(2, "m2")));
    settle(window2, &view2, "Hermes answers on retry", |app| {
        turn_of(app, 2, "m2").is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
    });
    let sb = seen_b.lock().unwrap();
    assert_eq!(sb.len(), 3);
    assert_eq!(
        sb[1].body, sb[2].body,
        "the same prompt, rebuilt after the relaunch"
    );
}

/// RC2-CNCL15: an exported council names who said what, round by round,
/// with each reply's stance.
#[gpui::test]
fn export_names_who_said_what(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::export_names_who_said_what").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(answers("Keep Gemma.", "Same answer\nStill Gemma."));
    let (b, _) = stub::spawn(answers("Move to Qwen.", "Changed partly\nTrial first."));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Gemma or Qwen?");
    settle(window, &view, "round 1", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    view.update(window, |app, cx| app.council_answer_each_other(cx));
    settle(window, &view, "round 2", |app| {
        idle(app) && rounds(app) == 2
    });
    wait_store("four answers", |s| {
        let t = s.council_turns(&conv).ok()?;
        (t.len() == 4 && settled(&t) && t.iter().all(|t| t.message_id.is_some())).then_some(())
    });
    let md = view.update(window, |app, cx| {
        app.copy_on_screen_conversation(cx);
        cx.read_from_clipboard()
            .and_then(|c| c.text())
            .unwrap_or_default()
    });
    for want in [
        "**User**\n\nGemma or Qwen?",
        "**Gemma 4 26B \u{00B7} round 1**\n\nKeep Gemma.",
        "**Hermes \u{00B7} round 1**\n\nMove to Qwen.",
        // RC2-R613: the tag says whose answer was kept.
        "**Gemma 4 26B \u{00B7} round 2 \u{00B7} Kept its answer**",
        "**Hermes \u{00B7} round 2 \u{00B7} Changed its answer partly**",
    ] {
        assert!(md.contains(want), "{want:?} in:\n{md}");
    }
    assert!(!md.contains("**Assistant**"), "{md}");
}

// ---- RC2-CNCLF1: members work like Work in "Ask before changes" -------------------

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn planning_claude(dir: &std::path::Path) -> String {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` in `dir`: in its planning step it answers with a plan
/// ending in Awili's plan marker; given full access it says what it did.
/// Each start's argv is appended to `dir/claude-starts`.
fn planning_claude(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    let marker = awili_code::PLAN_READY_MARKER;
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nIFS= read -r l\nprintf '%s ' \"$@\" \"$l\" >> {d}/claude-starts\nprintf '\\n@@@\\n' >> {d}/claude-starts\n\
             case \"$*\" in\n\
             *--dangerously-skip-permissions*) printf '%s\\n' \
             '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"Done: notes.txt written.\"}}]}}}}' \
             '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"Done: notes.txt written.\"}}' ;;\n\
             *) printf '%s\\n' \
             '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"1. Write notes.txt.\\n{marker}\"}}]}}}}' \
             '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"1. Write notes.txt.\\n{marker}\"}}' ;;\n\
             esac\n",
            d = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin.display().to_string()
}

fn starts(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("claude-starts"))
        .unwrap_or_default()
        .split("\n@@@\n")
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// RC2-CNCLF1 (review P1 on cf08877c): a Claude member that wants to change
/// something ends its read-only planning step with a plan; the card shows
/// Work's plan card ("Make the changes" / "Don't change anything") and
/// nothing runs until the owner says so. "Make the changes" runs that member
/// alone once with full access, the plan kept above what it did; "Don't
/// change anything" runs nothing.
#[gpui::test]
fn rc2cnclf1_a_members_plan_waits_for_the_owner(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::rc2cnclf1_a_members_plan_waits_for_the_owner") else {
        return;
    };
    cx.executor().allow_parking();
    let bins = home.join("bins");
    std::fs::create_dir_all(&bins).unwrap();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    seated(window, &view, &[("agent:claude:sonnet", "Claude Sonnet")]);
    view.update(window, |app, cx| {
        app.work.agents.claude_bin = Some(planning_claude(&bins));
        app.composer
            .update(cx, |c, cx| c.set_text("Write my notes down.", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "the plan", idle);
    let plan_ok = leak(crate::council_view::plan_approve_selector(1, "m1"));
    let plan_no = leak(crate::council_view::plan_decline_selector(1, "m1"));
    window.run_until_parked();
    assert!(window.debug_bounds(plan_ok).is_some(), "Make the changes");
    assert!(
        window.debug_bounds(plan_no).is_some(),
        "Don't change anything"
    );
    view.read_with(window, |app, _| {
        let t = turn_of(app, 1, "m1").unwrap();
        assert_eq!(t.text, "1. Write notes.txt.", "the marker never shows");
    });
    let s = starts(&bins);
    assert_eq!(s.len(), 1, "{s:?}");
    assert!(s[0].contains("--permission-mode plan"), "{s:?}");
    click(window, plan_ok);
    settle(window, &view, "the approved follow-up", |app| {
        turn_of(app, 1, "m1").is_some_and(|t| t.text.contains("Done: notes.txt written."))
            && idle(app)
    });
    let s = starts(&bins);
    assert_eq!(s.len(), 2, "one follow-up: {s:?}");
    assert!(s[1].contains("--dangerously-skip-permissions"), "{s:?}");
    assert!(
        s[1].contains("1. Write notes.txt."),
        "it is given its plan: {s:?}"
    );
    view.read_with(window, |app, _| {
        let t = turn_of(app, 1, "m1").unwrap();
        assert!(
            t.text.starts_with("1. Write notes.txt.\n\n"),
            "{:?}",
            t.text
        );
        assert!(app.active_council().unwrap().plans.is_empty());
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds(plan_ok).is_none(),
        "the plan is used up"
    );
    // A second plan, declined: nothing runs.
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("And the shopping list.", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "the second plan", |app| {
        idle(app) && rounds(app) == 2
    });
    click(
        window,
        leak(crate::council_view::plan_decline_selector(2, "m1")),
    );
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert!(page.plans.is_empty());
        assert_eq!(
            page.note.as_deref(),
            Some("Claude Sonnet\u{2019}s plan was declined. Nothing was changed.")
        );
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    window.run_until_parked();
    assert_eq!(starts(&bins).len(), 3, "nothing ran after the decline");
}

// ---- RC2-CNCLF2: HTTP models use the owner's tools --------------------------------

fn sse_chunk(payload: serde_json::Value) -> String {
    format!("data: {payload}\n\n")
}

/// One model round that calls the engine tool `name` (its wire name) with
/// `args`.
fn tool_round(id: &str, name: &str, args: serde_json::Value) -> String {
    let mut out = sse_chunk(
        serde_json::json!({"id": "c", "object": "chat.completion.chunk",
        "created": 1, "model": "m", "choices": [{"index": 0, "delta": {"role": "assistant",
        "content": "", "tool_calls": [{"id": id, "index": 0, "type": "function",
        "function": {"name": name, "arguments": args.to_string()}}]}, "finish_reason": null}]}),
    );
    out.push_str(&sse_chunk(
        serde_json::json!({"id": "c", "object": "chat.completion.chunk",
        "created": 1, "model": "m", "choices": [{"index": 0, "delta": {"role": "assistant",
        "content": ""}, "finish_reason": "tool_calls"}]}),
    ));
    out.push_str("data: [DONE]\n\n");
    out
}

/// One closing text round.
fn words_round(text: &str) -> String {
    let mut out = sse_chunk(
        serde_json::json!({"id": "c", "object": "chat.completion.chunk",
        "created": 1, "model": "m", "choices": [{"index": 0, "delta": {"role": "assistant",
        "content": text}, "finish_reason": null}]}),
    );
    out.push_str(&sse_chunk(serde_json::json!({"id": "c", "object": "chat.completion.chunk",
        "created": 1, "model": "m", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})));
    out.push_str("data: [DONE]\n\n");
    out
}

/// The text of the newest `tool` message of a Chat Completions request.
fn last_tool_result(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let last = v["messages"].as_array()?.last()?.clone();
    (last["role"] == "tool").then(|| match &last["content"] {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// A loopback model server: every request body is kept, and `answer` gives
/// the SSE body for it.
fn tool_model(
    answer: impl Fn(&str) -> String + Send + Sync + 'static,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let keep = seen.clone();
    let answer = std::sync::Arc::new(answer);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let (keep, answer) = (keep.clone(), answer.clone());
            std::thread::spawn(move || {
                let mut s = s;
                let mut data = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let n = s.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                    if let Some(h) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&data[..h]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if data.len() >= h + 4 + len {
                            break;
                        }
                    }
                }
                let body = data
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|h| String::from_utf8_lossy(&data[h + 4..]).into_owned())
                    .unwrap_or_default();
                // The status strip's read-only Lemonade check is no request.
                if data.starts_with(b"GET /api/v1/health") {
                    let _ = write!(
                        s,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    return;
                }
                keep.lock().unwrap().push(body.clone());
                let out = answer(&body);
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\
                     Content-Length: {}\r\n\r\n{out}",
                    out.len()
                );
            });
        }
    });
    (format!("http://{addr}"), seen)
}

/// RC2-CNCLF2 (review P1 on cf08877c): an HTTP model on a council — the
/// model on this Mac and an OpenAI-compatible `[[providers]]` model — answers
/// through Awili's own tool engine, as the in-app model does in Work: it is
/// offered the tools, READS a file in the owner's home freely (the result
/// reaches the model and its answer), and a step that changes something
/// stops at Work's approval card on the member's card; "Allow once" lets it
/// run.
#[gpui::test]
fn rc2cnclf2_api_models_use_the_owners_tools(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::rc2cnclf2_api_models_use_the_owners_tools") else {
        return;
    };
    cx.executor().allow_parking();
    std::fs::write(home.join("notes.txt"), "council-note-42\n").unwrap();
    // The model on this Mac reads notes.txt.
    let (a, seen_a) = tool_model(|body| match last_tool_result(body) {
        Some(got) => words_round(&format!("The file says: {got}")),
        None => tool_round("r1", "fs_read", serde_json::json!({"path": "notes.txt"})),
    });
    // The provider model writes answer.txt.
    let (p, seen_p) = tool_model(|body| match last_tool_result(body) {
        Some(_) => words_round("Wrote answer.txt."),
        None => tool_round(
            "w1",
            "fs_write",
            serde_json::json!({"path": "answer.txt", "contents": "ok\n"}),
        ),
    });
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.work.engine.base_url = a.clone();
        app.work.engine.model = "m".into();
        app.choices = vec![awili_config::ModelChoice {
            provider: "spark".into(),
            kind: awili_config::ProviderKind::OpenAi,
            base_url: format!("{p}/v1"),
            model: "qwen38-27b".into(),
            api_key: None,
        }];
        app.council_last_group = Some(vec![
            ("in-app".into(), "Gemma 4 26B".into()),
            ("model:spark:qwen38-27b".into(), "qwen38-27b".into()),
        ]);
        app.set_mode(Mode::Council, cx);
        app.composer.update(cx, |c, cx| {
            c.set_text("Read notes.txt, and write answer.txt saying ok.", cx)
        });
        app.submit_composer_draft(cx);
    });
    // The write waits for the owner's OK; nothing is written before it.
    settle(window, &view, "the approval card", |app| {
        app.active_council()
            .is_some_and(|p| !p.approvals.is_empty())
    });
    assert!(
        !home.join("answer.txt").exists(),
        "nothing written before the OK"
    );
    let allow = leak(crate::council_view::approval_selector(1, "m2", "once"));
    window.run_until_parked();
    assert!(window
        .debug_bounds(leak(crate::council_view::approval_selector(
            1, "m2", "deny"
        )))
        .is_some());
    click(window, allow);
    settle(window, &view, "both answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    assert_eq!(
        std::fs::read_to_string(home.join("answer.txt")).unwrap(),
        "ok\n",
        "Allow once ran the write"
    );
    view.read_with(window, |app, _| {
        assert_eq!(answer(app, "m1"), "The file says: council-note-42\n");
        assert_eq!(answer(app, "m2"), "Wrote answer.txt.");
        let page = app.active_council().unwrap();
        let t = round_one(app).unwrap().turn("m1").unwrap();
        let d = crate::council_view::card_details(round_one(app).unwrap(), t, page.sent.get(&t.id));
        assert!(
            d.iter()
                .any(|(k, v)| *k == "Tools" && v.starts_with("your MCP servers")),
            "{d:?}"
        );
    });
    for seen in [&seen_a, &seen_p] {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "one tool round, one answer");
        let first: serde_json::Value = serde_json::from_str(&seen[0]).unwrap();
        assert!(
            first["tools"]
                .as_array()
                .is_some_and(|t| t.iter().any(|t| t["function"]["name"] == "fs_read")),
            "the tools are offered"
        );
    }
    assert!(
        last_tool_result(&seen_a.lock().unwrap()[1]).is_some_and(|r| r.contains("council-note-42")),
        "the read's result reached the model"
    );
}

// ---- RC2-CNCLF3: Retry sends exactly what was sent -------------------------------

/// RC2-CNCLF3 (review P2 on cf08877c): after a relaunch, with the group
/// changed since and a history that had been trimmed to fit the member's
/// window, Retry sends the member EXACTLY what it was sent (kept with the
/// turn and checked against its fingerprint) on the route it answered on —
/// never a prompt rebuilt from today's council. A kept prompt that doesn't
/// match its fingerprint is not sent; the note says so.
#[gpui::test]
fn rc2cnclf3_retry_after_a_relaunch_sends_exactly_what_was_sent(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2cnclf3_retry_after_a_relaunch_sends_exactly_what_was_sent")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    let (a, seen_a) =
        stub::spawn(
            move |_| match c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => stub::Reply::text(&["Keep Gemma."]),
                1 => fails(),
                _ => stub::Reply::text(&["Still Gemma."]),
            },
        );
    // Hermes answers at length: Gemma's small window can't hold it all.
    let long = "Qwen is better at long coding sessions. ".repeat(1500);
    let long2 = long.clone();
    let (b, _) = stub::spawn(move |_| stub::Reply::text(&[long2.as_str()]));
    let setup = |app: &mut AwiliApp| {
        two_member_council(app, &a, &b);
        app.work.engine.window_tokens = 8_192;
    };
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    let conv = view.update(window, |app, cx| {
        setup(app);
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    settle(window, &view, "round 1", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("And for the Ultra?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "round 2", |app| {
        idle(app) && rounds(app) == 2
    });
    view.read_with(window, |app, _| {
        let t = turn_of(app, 2, "m1").unwrap();
        assert!(matches!(t.status, awili_council::TurnStatus::Failed { .. }));
        let page = app.active_council().unwrap();
        assert!(
            page.sent.get(&t.id).is_some_and(|s| s.older_left_out),
            "Gemma's history was trimmed to fit"
        );
    });
    wait_store("round 2 saved", |s| {
        let t = s.council_turns(&conv).ok()?;
        (t.len() == 4 && settled(&t) && t.iter().all(|t| t.sent_text.is_some())).then_some(())
    });
    let sent_before = seen_a.lock().unwrap()[1].body.clone();
    // The relaunch: a second Awili on the same database, and the group
    // changes before the Retry.
    let (view2, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1600.), px(900.)));
    view2.update(window2, |app, cx| {
        setup(app);
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), conv);
        let page = app.councils.get_mut(&conv).unwrap();
        page.council.add_member("agent:codex", "Codex");
        assert_eq!(page.member_names().len(), 3, "the group changed");
        cx.notify();
    });
    window2.run_until_parked();
    click(window2, leak(crate::council_view::retry_selector(2, "m1")));
    settle(window2, &view2, "Gemma answers on retry", |app| {
        turn_of(app, 2, "m1").is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
    });
    {
        let sa = seen_a.lock().unwrap();
        assert_eq!(sa.len(), 3);
        assert_eq!(sa[2].body, sent_before, "byte for byte what was sent");
        assert!(
            sa[2].body.contains("one of 2 models"),
            "the council as it was then"
        );
    }
    // A kept prompt that no longer matches its fingerprint is not sent.
    view2.update(window2, |app, cx| {
        let page = app.councils.get_mut(&conv).unwrap();
        let id = page
            .council
            .round(2)
            .unwrap()
            .turn("m1")
            .unwrap()
            .id
            .clone();
        page.sent
            .get_mut(&id)
            .unwrap()
            .prompt
            .ask
            .push_str(" (changed)");
        assert!(!app.council_rerun(&conv, 2, "m1", crate::council_run::Rerun::Retry, cx));
        assert_eq!(
            app.active_council().unwrap().note.as_deref(),
            Some(crate::council_run::not_matching_note("Gemma 4 26B").as_str())
        );
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    window2.run_until_parked();
    assert_eq!(seen_a.lock().unwrap().len(), 3, "nothing was sent");
}

// ---- RC2-CNCLF4: a relaunch keeps what was accepted -------------------------------

/// RC2-CNCLF4 (review P1 on cf08877c): Awili closes while the members are
/// still answering and a message waits its turn. After the relaunch the
/// round is there — the question, each member's answer so far, marked as
/// cut off by the quit, with Continue — and the waiting message is back in
/// the council's box, nothing lost; Continue picks the answer up.
#[gpui::test]
fn rc2cnclf4_a_relaunch_mid_answer_keeps_the_round_and_the_queue(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2cnclf4_a_relaunch_mid_answer_keeps_the_round_and_the_queue")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    let (a, _) = stub::spawn(move |_| {
        if c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            holds("Keep Gemma ")
        } else {
            stub::Reply::text(&["for now."])
        }
    });
    let (b, _) = stub::spawn(|_| holds("Move coding "));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Gemma or Qwen?");
    settle(window, &view, "both answering", |app| {
        round_one(app).is_some_and(|r| {
            r.turns
                .iter()
                .all(|t| t.status.is_running() && !t.text.is_empty())
        })
    });
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("And for the Ultra?", cx));
        app.submit_composer_draft(cx);
        assert_eq!(app.active_council().unwrap().queued.len(), 1);
    });
    let key = crate::council_view::queue_key(&conv);
    wait_store("the round, the answers so far and the queue", |s| {
        let turns = s.council_turns(&conv).ok()?;
        let queue = s.get_app_state(&key).ok()??;
        (turns.len() == 2
            && turns
                .iter()
                .all(|t| t.status == "running" && t.message_id.is_some())
            && queue.contains("And for the Ultra?"))
        .then_some(())
    });
    // The relaunch, while the first Awili's members are still answering.
    let (view2, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1600.), px(900.)));
    view2.update(window2, |app, cx| {
        two_member_council(app, &a, &b);
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), conv);
        let page = app.active_council().expect("restored");
        let r = page.council.round(1).expect("the round is kept");
        assert_eq!(r.owner_text.as_deref(), Some("Gemma or Qwen?"));
        for (m, words) in [("m1", "Keep Gemma"), ("m2", "Move coding")] {
            let t = r.turn(m).unwrap();
            assert_eq!(t.status, awili_council::TurnStatus::Stopped, "{m}");
            assert!(t.text.starts_with(words), "{m}: {:?}", t.text);
            assert!(page.cut.contains(&t.id), "{m} is cut off by the quit");
        }
        assert_eq!(
            page.note.as_deref(),
            Some(crate::council_view::queue_back_note(1).as_str())
        );
        app.sync_composer_draft(cx);
        assert_eq!(
            app.composer.read(cx).text(),
            "And for the Ultra?",
            "the waiting message is back in the box"
        );
        cx.notify();
    });
    window2.run_until_parked();
    click(
        window2,
        leak(crate::council_view::continue_selector(1, "m1")),
    );
    settle(window2, &view2, "Gemma continued", |app| {
        turn_of(app, 1, "m1").is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
    });
    view2.read_with(window2, |app, _| {
        assert_eq!(turn_of(app, 1, "m1").unwrap().text, "Keep Gemma for now.");
        let page = app.active_council().unwrap();
        assert!(!page.cut.contains(&turn_of(app, 1, "m1").unwrap().id));
    });
    wait_store("the continued answer", |s| {
        let t = s.council_turns(&conv).ok()?;
        t.iter()
            .any(|t| t.member_id == "m1" && t.status == "answered")
            .then_some(())
    });
}

// ---- RC2-CNCLF5: the memory gate once per message --------------------------------

/// A memory service that answers every lookup (no rules, nothing recalled)
/// and counts them, set as the production memory backend under `home`.
fn counting_memory(
    home: &std::path::Path,
) -> (
    crate::MemoryCfg,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let (mem, hits, _held) = held_memory(home);
    (mem, hits)
}

/// RC2-CNCLG1: `counting_memory` whose answers wait while `held` is set (a
/// slow memory service): every lookup is counted as it arrives.
fn held_memory(
    home: &std::path::Path,
) -> (
    crate::MemoryCfg,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let (mem, hits, held, _refuse) = refusing_memory(home);
    (mem, hits, held)
}

/// RC2-CNCLH2: `held_memory` that refuses (an answer Awili cannot check,
/// `mandatory_policy: null`) every lookup that ARRIVES while `refuse` is set
/// — a held lookup keeps the answer it arrived to.
fn refusing_memory(
    home: &std::path::Path,
) -> (
    crate::MemoryCfg,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::io::{Read, Write};
    use std::sync::atomic::Ordering::SeqCst;
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let held = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let refuse = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (count, hold, refusing) = (hits.clone(), held.clone(), refuse.clone());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let (count, hold, refusing) = (count.clone(), hold.clone(), refusing.clone());
            std::thread::spawn(move || {
                let mut s = s;
                let mut buf = [0u8; 8192];
                let _ = s.read(&mut buf);
                count.fetch_add(1, SeqCst);
                let refused = refusing.load(SeqCst);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                while hold.load(SeqCst) && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                let body = if refused {
                    r#"{"mandatory_policy": null, "items": []}"#
                } else {
                    r#"{"mandatory_policy": [], "items": []}"#
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    let foreman = home.join(".config/foreman");
    std::fs::create_dir_all(&foreman).unwrap();
    let token = foreman.join("v6-reader-token");
    std::fs::write(&token, "reader\n").unwrap();
    std::fs::write(
        foreman.join("memory-backend.conf"),
        format!(
            "FOREMAN_MEMORY_BACKEND=v65\nFOREMAN_MEMORY_V65_BASE={base}\n\
             FOREMAN_MEMORY_V65_READ_TOKEN={}\n",
            token.display()
        ),
    )
    .unwrap();
    let mem = crate::MemoryCfg {
        base_url: base,
        token: String::new(),
        min_priority: 90,
        enabled: true,
        recall: true,
        write: false,
    };
    (mem, hits, held, refuse)
}

/// RC2-CNCLF5 (review P2 on cf08877c): with memory on, the memory gate runs
/// ONCE per owner message — the automatic "answer each other" rounds after
/// it use that message's outcome, they don't ask memory again.
#[gpui::test]
fn rc2cnclf5_automatic_rounds_reuse_the_messages_memory_gate(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("council_tests::rc2cnclf5_automatic_rounds_reuse_the_messages_memory_gate")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(answers("Keep Gemma.", "Same answer\nStill."));
    let (b, _) = stub::spawn(answers("Move to Qwen.", "Changed my answer\nOK."));
    let (mem, hits) = counting_memory(&home);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "the first message answered", |app| {
        idle(app) && rounds(app) == 1
    });
    let one_gate = hits.load(std::sync::atomic::Ordering::SeqCst);
    assert!(one_gate > 0, "memory was asked for the first message");
    // The next message, with two automatic rounds after it.
    view.update(window, |app, cx| {
        app.council_set_auto_rounds(2, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("And for the Ultra?", cx));
        app.submit_composer_draft(cx);
    });
    settle(
        window,
        &view,
        "the message and two automatic rounds",
        |app| idle(app) && rounds(app) == 4,
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    window.run_until_parked();
    assert_eq!(seen_a.lock().unwrap().len(), 4, "every round was answered");
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        2 * one_gate,
        "one gate for the second message, none for its automatic rounds"
    );
    // Retry in an automatic round uses the same outcome too.
    view.update(window, |app, cx| {
        let conv = app.sidebar.active_id().to_string();
        assert!(app.council_rerun(&conv, 4, "m1", crate::council_run::Rerun::Retry, cx));
    });
    settle(window, &view, "the retry answered", idle);
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2 * one_gate);
}

// ---- RC2-CNCLF6: what a council costs ------------------------------------------

/// RC2-CNCLF6 (review P2 on cf08877c): a priced member's first attempt
/// reports its words but sends back nothing; Retry answers. Both attempts
/// were paid for: the card, the round and the council tallies keep both,
/// and match Awili's own usage totals (which count every attempt).
#[gpui::test]
fn rc2cnclf6_retry_keeps_every_attempts_cost(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2cnclf6_retry_keeps_every_attempts_cost").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    let (d, _) = stub::spawn(move |_| {
        let mut r = if c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            stub::Reply::text(&[])
        } else {
            stub::Reply::text(&["Count failed tool calls."])
        };
        r.usage = Some((100_000, 50_000));
        r
    });
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        priced_council(app, &d, &b);
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Which one?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "round settled", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    view.read_with(window, |app, _| {
        assert!(matches!(
            status_of(app, "m1"),
            Some(awili_council::TurnStatus::Failed { .. })
        ));
    });
    click(window, leak(crate::council_view::retry_selector(1, "m1")));
    settle(window, &view, "answered on retry", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Answered)
    });
    view.read_with(window, |app, _| {
        let r = round_one(app).unwrap();
        let t = r.turn("m1").unwrap();
        assert_eq!(
            t.cost_usd.map(awili_core::pricing::format_usd).as_deref(),
            Some("$0.18")
        );
        assert!(crate::run_footer::council_round_tally(r).ends_with("$0.18"));
        let tally = crate::run_footer::council_tally(&app.active_council().unwrap().council);
        assert!(tally.ends_with("$0.18"), "{tally}");
        assert_eq!(
            app.cost.chip_label(crate::feed::unix_now()).as_deref(),
            Some("$0.18 session \u{00B7} $0.18 today"),
            "the council's tally matches Awili's usage totals"
        );
    });
}

// ---- RC2-CNCLG1: Stop while the memory lookup is still out -----------------------

/// RC2-CNCLG1 (review P1 on 74c06bdd): the cards show the members answering
/// as soon as the message is sent, while the memory lookup is still out.
/// Stop on one card, then Esc for the next message, in that interval: those
/// members end stopped at once, and when the lookup answers they are NOT
/// started (the member left waiting is); a stopped one can be retried.
#[gpui::test]
fn rc2cnclg1_stop_while_the_memory_lookup_is_out(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(home) = isolated("council_tests::rc2cnclg1_stop_while_the_memory_lookup_is_out")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["Move to Qwen."]));
    let (mem, hits, held) = held_memory(&home);
    held.store(true, SeqCst);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    settle(
        window,
        &view,
        "the lookup is out, both cards answering",
        |app| {
            hits.load(SeqCst) > 0
                && round_one(app).is_some_and(|r| {
                    r.turns.len() == 2 && r.turns.iter().all(|t| t.status.is_running())
                })
        },
    );
    // Stop on Gemma's card while the lookup is out.
    click(window, leak(crate::council_view::stop_selector(1, "m1")));
    view.read_with(window, |app, _| {
        assert_eq!(
            status_of(app, "m1"),
            Some(awili_council::TurnStatus::Stopped),
            "Stop ended Gemma's card at once"
        );
        assert!(status_of(app, "m2").unwrap().is_running());
    });
    held.store(false, SeqCst);
    settle(window, &view, "Hermes answered", |app| {
        status_of(app, "m2") == Some(awili_council::TurnStatus::Answered)
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    window.run_until_parked();
    assert_eq!(seen_a.lock().unwrap().len(), 0, "Gemma was never asked");
    assert_eq!(seen_b.lock().unwrap().len(), 1);
    view.read_with(window, |app, _| {
        assert_eq!(
            status_of(app, "m1"),
            Some(awili_council::TurnStatus::Stopped)
        );
        assert!(app.active_council().unwrap().runs.is_empty());
    });
    wait_store("Gemma's stopped turn", |s| {
        s.council_turns(&conv)
            .ok()?
            .into_iter()
            .find(|t| t.round == 1 && t.member_id == "m1" && t.status == "stopped")
    });
    // The next message: Esc while its lookup is out stops both.
    let before = hits.load(SeqCst);
    held.store(true, SeqCst);
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("And for the Ultra?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "the second lookup is out", |app| {
        hits.load(SeqCst) > before
            && turn_of(app, 2, "m1").is_some_and(|t| t.status.is_running())
            && turn_of(app, 2, "m2").is_some_and(|t| t.status.is_running())
    });
    view.update_in(window, |app, window, cx| {
        let h = app.composer.read(cx).focus_handle().clone();
        window.focus(&h, cx);
    });
    window.simulate_keystrokes("escape");
    window.run_until_parked();
    view.read_with(window, |app, _| {
        for m in ["m1", "m2"] {
            assert_eq!(
                turn_of(app, 2, m).map(|t| t.status),
                Some(awili_council::TurnStatus::Stopped),
                "Esc ended {m}'s card at once"
            );
        }
    });
    held.store(false, SeqCst);
    // RC2-CNCLI (review P3 on a2c34605): wait until Awili has handled the
    // released lookup, not a fixed time, before checking nothing started.
    settle(window, &view, "the released lookup is handled", |app| {
        app.active_council().is_some_and(|p| p.gating.is_empty())
    });
    window.run_until_parked();
    assert_eq!(seen_a.lock().unwrap().len(), 0, "nothing started");
    assert_eq!(seen_b.lock().unwrap().len(), 1, "nothing started");
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert!(page.runs.is_empty());
        assert!(!page.council.is_running());
    });
    wait_store("round 2 kept as stopped", |s| {
        let t = s.council_turns(&conv).ok()?;
        (t.iter()
            .filter(|t| t.round == 2 && t.status == "stopped")
            .count()
            == 2)
            .then_some(())
    });
    // A stopped member can be asked again: what it was sent is kept.
    view.update(window, |app, cx| {
        assert!(app.council_rerun(&conv, 2, "m1", crate::council_run::Rerun::Retry, cx));
    });
    settle(window, &view, "Gemma answers on Retry", |app| {
        turn_of(app, 2, "m1").is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
    });
    assert_eq!(seen_a.lock().unwrap().len(), 1);
}

// ---- RC2-CNCLG2: a member that uses tools pays for every request -----------------

/// `sse` with a usage report of 100k words read and 50k written (DeepSeek
/// Flash: $0.09) before its end.
fn priced_round(sse: String) -> String {
    let usage = sse_chunk(
        serde_json::json!({"id": "c", "object": "chat.completion.chunk",
        "created": 1, "model": "m", "choices": [],
        "usage": {"prompt_tokens": 100_000, "completion_tokens": 50_000, "total_tokens": 150_000}}),
    );
    sse.replace("data: [DONE]\n\n", &format!("{usage}data: [DONE]\n\n"))
}

/// RC2-CNCLG2 (review P2 on 74c06bdd): DeepSeek Flash reads a file before
/// it answers — two priced requests per answer, each reporting its own
/// usage. Its first answer comes back empty, Retry answers: four requests,
/// $0.36, on the card, the round and council tallies, the saved turn and
/// Awili's usage totals alike.
#[gpui::test]
fn rc2cnclg2_a_tool_using_member_pays_for_every_request(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("council_tests::rc2cnclg2_a_tool_using_member_pays_for_every_request")
    else {
        return;
    };
    cx.executor().allow_parking();
    std::fs::write(home.join("notes.txt"), "council-note-42\n").unwrap();
    let answered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n = answered.clone();
    let (d, seen_d) = tool_model(move |body| {
        priced_round(match last_tool_result(body) {
            None => tool_round("r1", "fs_read", serde_json::json!({"path": "notes.txt"})),
            Some(_) if n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => words_round(""),
            Some(got) => words_round(&format!("The file says: {got}")),
        })
    });
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = view.update(window, |app, cx| {
        priced_council(app, &format!("{d}/v1"), &b);
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("What does notes.txt say?", cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    settle(window, &view, "round settled", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    view.read_with(window, |app, _| {
        assert!(matches!(
            status_of(app, "m1"),
            Some(awili_council::TurnStatus::Failed { .. })
        ));
    });
    click(window, leak(crate::council_view::retry_selector(1, "m1")));
    settle(window, &view, "answered on retry", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Answered)
    });
    assert_eq!(seen_d.lock().unwrap().len(), 4, "two requests per answer");
    view.read_with(window, |app, _| {
        assert_eq!(answer(app, "m1"), "The file says: council-note-42\n");
        let r = round_one(app).unwrap();
        let t = r.turn("m1").unwrap();
        assert_eq!(
            t.usage,
            Some((200_000, 100_000)),
            "this attempt's two requests"
        );
        assert_eq!(
            t.cost_usd.map(awili_core::pricing::format_usd).as_deref(),
            Some("$0.36")
        );
        assert!(crate::run_footer::council_round_tally(r).ends_with("$0.36"));
        let tally = crate::run_footer::council_tally(&app.active_council().unwrap().council);
        assert!(tally.ends_with("$0.36"), "{tally}");
        assert_eq!(
            app.cost.chip_label(crate::feed::unix_now()).as_deref(),
            Some("$0.36 session \u{00B7} $0.36 today"),
            "the council's tally matches Awili's usage totals"
        );
    });
    wait_store("the saved turn", |s| {
        s.council_turns(&conv).ok()?.into_iter().find(|t| {
            t.member_id == "m1"
                && t.status == "answered"
                && t.input_tokens == Some(200_000)
                && t.output_tokens == Some(100_000)
                && t.cost_usd.is_some_and(|c| (c - 0.36).abs() < 1e-9)
        })
    });
}

// ---- RC2-CNCLG3: one memory lookup for retries at once ---------------------------

/// RC2-CNCLG3 (review P2 on 74c06bdd): after a relaunch no memory outcome
/// is kept. Both members failed; the owner retries both before the memory
/// lookup comes back: ONE lookup serves both retries (the second waits on
/// the first), and both answer.
#[gpui::test]
fn rc2cnclg3_retries_at_once_share_one_memory_lookup(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(home) = isolated("council_tests::rc2cnclg3_retries_at_once_share_one_memory_lookup")
    else {
        return;
    };
    cx.executor().allow_parking();
    let fail_then = |text: &'static str| {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        move |_: &str| {
            if calls.fetch_add(1, SeqCst) == 0 {
                fails()
            } else {
                stub::Reply::text(&[text])
            }
        }
    };
    let (a, seen_a) = stub::spawn(fail_then("Keep Gemma."));
    let (b, seen_b) = stub::spawn(fail_then("Move to Qwen."));
    let (mem, hits, held) = held_memory(&home);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let mem2 = mem.clone();
    let conv = view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    settle(window, &view, "both failed", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    let one_gate = hits.load(SeqCst);
    assert!(one_gate > 0, "memory was asked for the message");
    wait_store("both failures saved", |s| {
        let t = s.council_turns(&conv).ok()?;
        (t.len() == 2
            && t.iter()
                .all(|t| t.status == "failed" && t.sent_text.is_some()))
        .then_some(())
    });
    // The relaunch: no memory outcome is kept.
    let (view2, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1400.), px(900.)));
    view2.update(window2, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem2;
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), conv);
        assert!(app.active_council().unwrap().gates.is_empty());
    });
    window2.run_until_parked();
    // Retry both while the lookup is out.
    held.store(true, SeqCst);
    click(window2, leak(crate::council_view::retry_selector(1, "m1")));
    click(window2, leak(crate::council_view::retry_selector(1, "m2")));
    settle(window2, &view2, "both retried, the lookup out", |app| {
        hits.load(SeqCst) > one_gate
            && round_one(app).is_some_and(|r| r.turns.iter().all(|t| t.status.is_running()))
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    held.store(false, SeqCst);
    settle(window2, &view2, "both answered on retry", |app| {
        round_one(app).is_some_and(|r| {
            r.turns
                .iter()
                .all(|t| t.status == awili_council::TurnStatus::Answered)
        })
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    window2.run_until_parked();
    assert_eq!(
        hits.load(SeqCst),
        2 * one_gate,
        "one lookup for both retries"
    );
    assert_eq!(seen_a.lock().unwrap().len(), 2);
    assert_eq!(seen_b.lock().unwrap().len(), 2);
    view2.read_with(window2, |app, _| {
        assert_eq!(answer(app, "m1"), "Keep Gemma.");
        assert_eq!(answer(app, "m2"), "Move to Qwen.");
    });
}

// ---- RC2-CNCLH1: Stop all while the memory lookup is out -------------------------

/// RC2-CNCLH1 (review P2 on f4fdc630): while the memory lookup for a message
/// is out, the cards show both members answering, and so does the line
/// above the box — two answering, with Stop all. Stop all then stops both at
/// once; when the lookup answers, nobody is asked.
#[gpui::test]
fn rc2cnclh1_stop_all_while_the_memory_lookup_is_out(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(home) = isolated("council_tests::rc2cnclh1_stop_all_while_the_memory_lookup_is_out")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["Move to Qwen."]));
    let (mem, hits, held) = held_memory(&home);
    held.store(true, SeqCst);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    settle(
        window,
        &view,
        "the lookup is out, both cards answering",
        |app| {
            hits.load(SeqCst) > 0
                && round_one(app).is_some_and(|r| {
                    r.turns.len() == 2 && r.turns.iter().all(|t| t.status.is_running())
                })
        },
    );
    view.read_with(window, |app, _| {
        assert_eq!(app.active_council().unwrap().answering(), 2);
    });
    click(window, crate::council_view::STOP_ALL_SELECTOR);
    view.read_with(window, |app, _| {
        for m in ["m1", "m2"] {
            assert_eq!(
                status_of(app, m),
                Some(awili_council::TurnStatus::Stopped),
                "Stop all ended {m}'s card at once"
            );
        }
        assert_eq!(app.active_council().unwrap().answering(), 0);
    });
    assert!(
        window
            .debug_bounds(crate::council_view::STOP_ALL_SELECTOR)
            .is_none(),
        "nothing left to stop"
    );
    held.store(false, SeqCst);
    // RC2-CNCLI (review P3 on a2c34605): wait until Awili has handled the
    // released lookup, not a fixed time, before checking nothing started.
    settle(window, &view, "the released lookup is handled", |app| {
        app.active_council().is_some_and(|p| p.gating.is_empty())
    });
    window.run_until_parked();
    assert_eq!(seen_a.lock().unwrap().len(), 0, "nothing started");
    assert_eq!(seen_b.lock().unwrap().len(), 0, "nothing started");
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert!(page.runs.is_empty());
        assert!(!page.council.is_running());
    });
    wait_store("round 1 kept as stopped", |s| {
        let t = s.council_turns(&conv).ok()?;
        (t.iter()
            .filter(|t| t.round == 1 && t.status == "stopped")
            .count()
            == 2)
            .then_some(())
    });
}

// ---- RC2-CNCLH2: a refused lookup sends the message waiting its turn -------------

/// RC2-CNCLH2 (review P2 on f4fdc630): a message typed while the memory
/// lookup is out waits its turn. The lookup refuses: the round ends with its
/// one line, and the waiting message goes at once (its own lookup passes,
/// both members answer it) — it is not left waiting.
#[gpui::test]
fn rc2cnclh2_a_refused_lookup_sends_the_waiting_message(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(home) =
        isolated("council_tests::rc2cnclh2_a_refused_lookup_sends_the_waiting_message")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["Move to Qwen."]));
    let (mem, hits, held, refuse) = refusing_memory(&home);
    held.store(true, SeqCst);
    refuse.store(true, SeqCst);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    settle(
        window,
        &view,
        "the lookup is out, both cards answering",
        |app| {
            hits.load(SeqCst) > 0
                && round_one(app).is_some_and(|r| {
                    r.turns.len() == 2 && r.turns.iter().all(|t| t.status.is_running())
                })
        },
    );
    // Typed while the lookup is out: it waits its turn.
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("And for the Ultra?", cx));
        app.submit_composer_draft(cx);
        assert_eq!(app.active_council().unwrap().queued.len(), 1);
    });
    // The first lookup refuses; the waiting message's own lookup passes.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let first = hits.load(SeqCst);
    refuse.store(false, SeqCst);
    held.store(false, SeqCst);
    settle(window, &view, "the first message refused", |app| {
        round_one(app).is_some_and(|r| !r.left_out.is_empty() && !r.is_running())
    });
    settle(window, &view, "the waiting message answered", |app| {
        idle(app)
            && rounds(app) == 2
            && ["m1", "m2"].iter().all(|m| {
                turn_of(app, 2, m).is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
            })
    });
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert!(page.queued.is_empty(), "nothing left waiting");
        let r1 = page.council.round(1).unwrap();
        assert!(r1.turns.is_empty(), "nobody was asked the first message");
        assert!(r1.left_out[0].starts_with(crate::recall::POLICY_REFUSAL_PREFIX));
        assert_eq!(
            page.council.round(2).unwrap().owner_text.as_deref(),
            Some("And for the Ultra?")
        );
        assert_eq!(
            app.views[&conv].convo.messages().len(),
            4,
            "two asks, two answers"
        );
    });
    assert!(hits.load(SeqCst) > first, "its own lookup");
    for seen in [&seen_a, &seen_b] {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "asked the waiting message only");
        assert!(
            seen[0].body.contains("And for the Ultra?"),
            "{}",
            seen[0].body
        );
    }
    let key = crate::council_view::queue_key(&conv);
    wait_store("the queue kept empty", |s| {
        (s.get_app_state(&key).ok()??.as_str() == "[]").then_some(())
    });
}

// ---- RC2-CNCLH3: a refused round is kept ----------------------------------------

/// RC2-CNCLH3 (review P2 on f4fdc630): a round the memory lookup refused is
/// still in the council after a relaunch — the owner's message and its one
/// refusal line. A member stopped while that lookup was out keeps its
/// stopped card (in the council and saved) instead of going with the
/// members never asked, and Retry on it works after the relaunch.
#[gpui::test]
fn rc2cnclh3_a_refused_round_is_kept_across_a_relaunch(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(home) = isolated("council_tests::rc2cnclh3_a_refused_round_is_kept_across_a_relaunch")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["Move to Qwen."]));
    let (mem, hits, held, refuse) = refusing_memory(&home);
    refuse.store(true, SeqCst);
    let mem2 = mem.clone();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    // Round 1: refused outright.
    let conv = view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    settle(window, &view, "round 1 refused", |app| {
        round_one(app).is_some_and(|r| !r.left_out.is_empty() && !r.is_running())
    });
    // Round 2: Stop on Gemma's card while the lookup is out; it refuses.
    let before = hits.load(SeqCst);
    held.store(true, SeqCst);
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("And for the Ultra?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "the second lookup is out", |app| {
        hits.load(SeqCst) > before
            && turn_of(app, 2, "m1").is_some_and(|t| t.status.is_running())
            && turn_of(app, 2, "m2").is_some_and(|t| t.status.is_running())
    });
    click(window, leak(crate::council_view::stop_selector(2, "m1")));
    held.store(false, SeqCst);
    let round = |app: &AwiliApp, n: u32| app.active_council()?.council.round(n).cloned();
    settle(window, &view, "round 2 refused", |app| {
        round(app, 2).is_some_and(|r| !r.left_out.is_empty() && !r.is_running())
    });
    view.read_with(window, |app, _| {
        let r = round(app, 2).unwrap();
        assert_eq!(r.turns.len(), 1, "the stopped card stays: {:?}", r.turns);
        assert_eq!(r.turns[0].member, "m1");
        assert_eq!(r.turns[0].status, awili_council::TurnStatus::Stopped);
        assert!(r.left_out[0].starts_with(crate::recall::POLICY_REFUSAL_PREFIX));
    });
    let refused = crate::council_run::REFUSED;
    wait_store("both refused rounds and the stopped card", |s| {
        let t = s.council_turns(&conv).ok()?;
        let of = |n: i64| t.iter().filter(move |t| t.round == n);
        (of(1).count() == 1
            && of(1).all(|t| t.status == refused)
            && of(2).count() == 2
            && of(2).any(|t| t.status == refused)
            && of(2).any(|t| t.member_id == "m1" && t.status == "stopped" && t.sent_text.is_some()))
        .then_some(())
    });
    // The relaunch.
    let (view2, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1400.), px(900.)));
    view2.update(window2, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem2;
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), conv);
        let page = app.active_council().expect("restored");
        assert_eq!(page.council.rounds.len(), 2, "both rounds are kept");
        let (r1, r2) = (
            page.council.round(1).unwrap(),
            page.council.round(2).unwrap(),
        );
        assert_eq!(r1.owner_text.as_deref(), Some("Gemma or Qwen?"));
        assert!(r1.turns.is_empty());
        assert_eq!(r1.left_out.len(), 1, "one line: {:?}", r1.left_out);
        assert!(r1.left_out[0].starts_with(crate::recall::POLICY_REFUSAL_PREFIX));
        assert_eq!(r2.owner_text.as_deref(), Some("And for the Ultra?"));
        assert_eq!(r2.left_out.len(), 1, "one line: {:?}", r2.left_out);
        assert!(r2.left_out[0].starts_with(crate::recall::POLICY_REFUSAL_PREFIX));
        assert_eq!(r2.turns.len(), 1);
        assert_eq!(r2.turns[0].member, "m1");
        assert_eq!(r2.turns[0].status, awili_council::TurnStatus::Stopped);
        assert!(!page.cut.contains(&r2.turns[0].id), "stopped, not cut off");
        cx.notify();
    });
    window2.run_until_parked();
    // Retry on the stopped card: memory lets this one through.
    refuse.store(false, SeqCst);
    view2.update(window2, |app, cx| {
        assert!(app.council_rerun(&conv, 2, "m1", crate::council_run::Rerun::Retry, cx));
    });
    settle(window2, &view2, "Gemma answers on Retry", |app| {
        turn_of(app, 2, "m1").is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
    });
    assert_eq!(seen_a.lock().unwrap().len(), 1);
    assert_eq!(seen_b.lock().unwrap().len(), 0, "Hermes was never asked");
}

// ---- RC2-CNCLI: Retry after a refusal asks memory again -------------------------

/// RC2-CNCLI (review P2 on a2c34605): in the same session (no relaunch to
/// clear what was kept), Retry on a card stopped during a refused lookup asks
/// the memory gate again instead of reusing the kept refusal, so the member
/// answers once memory lets the message through.
#[gpui::test]
fn rc2cncli_retry_after_a_refusal_asks_memory_again(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(home) = isolated("council_tests::rc2cncli_retry_after_a_refusal_asks_memory_again")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["Move to Qwen."]));
    let (mem, hits, held, refuse) = refusing_memory(&home);
    refuse.store(true, SeqCst);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    let before = hits.load(SeqCst);
    held.store(true, SeqCst);
    settle(window, &view, "the lookup is out", |app| {
        hits.load(SeqCst) > before && turn_of(app, 1, "m1").is_some_and(|t| t.status.is_running())
    });
    click(window, leak(crate::council_view::stop_selector(1, "m1")));
    held.store(false, SeqCst);
    let round = |app: &AwiliApp, n: u32| app.active_council()?.council.round(n).cloned();
    settle(window, &view, "round 1 refused", |app| {
        round(app, 1).is_some_and(|r| !r.left_out.is_empty() && !r.is_running())
    });
    view.read_with(window, |app, _| {
        let r = round(app, 1).unwrap();
        assert_eq!(r.turns.len(), 1);
        assert_eq!(r.turns[0].status, awili_council::TurnStatus::Stopped);
    });
    // Memory recovers; Retry in the same session, no relaunch.
    refuse.store(false, SeqCst);
    let asked = hits.load(SeqCst);
    view.update(window, |app, cx| {
        assert!(app.council_rerun(&conv, 1, "m1", crate::council_run::Rerun::Retry, cx));
    });
    settle(window, &view, "Gemma answers on Retry", |app| {
        turn_of(app, 1, "m1").is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
    });
    assert!(hits.load(SeqCst) > asked, "Retry asked memory again");
    assert_eq!(seen_a.lock().unwrap().len(), 1);
    assert_eq!(seen_b.lock().unwrap().len(), 0, "Hermes was never asked");
}

// ---- RC2-R2F3: Continue in Work never mixes with an earlier box ----------------

/// RC2-R2F3 (usability RC2 round 2, Part 5: "Continue in Work" on Gemma's
/// answer left "… / In one sentence: what is RAID 1?" at the end of the box
/// — the box of the earlier, never-sent "Continue in Work" conversation was
/// reused and its text kept under the new one). Each "Continue in Work"
/// opens a Work conversation whose box holds exactly that question and
/// that answer. RC2-R54: an earlier one that holds only its untouched copy
/// is that conversation, its box emptied first.
#[gpui::test]
fn rc2r2f3_continue_in_work_twice_gives_a_clean_box(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r2f3_continue_in_work_twice_gives_a_clean_box").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(window, &view, &FOUR[0..3]);
    let q = "In one sentence: what is RAID 1?";
    view.update(window, |app, cx| {
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some(q),
            &[
                ("m1", answered(), "RAID 1 mirrors two disks."),
                ("m3", answered(), "RAID 1 is a mirroring technique."),
            ],
        );
        cx.notify();
    });
    window.run_until_parked();
    click(
        window,
        leak(crate::council_view::continue_in_work_selector(1, "m1")),
    );
    let first = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    let want_first =
        crate::council_run::work_box_text(Some(q), "Claude Sonnet", "RAID 1 mirrors two disks.");
    view.update(window, |app, cx| app.set_mode(Mode::Council, cx));
    window.run_until_parked();
    click(
        window,
        leak(crate::council_view::continue_in_work_selector(1, "m3")),
    );
    view.read_with(window, |app, cx| {
        assert_eq!(app.mode, Mode::Work);
        // RC2-R54 (usability RC2 round 4, Major 4): the first holds only the
        // copy "Continue in Work" put there, untouched, so it is reused (no
        // second empty row) and its box takes only the new text.
        assert_eq!(
            app.sidebar.active_id(),
            first,
            "the untouched copy's conversation is reused"
        );
        assert_eq!(
            app.composer.read(cx).text(),
            crate::council_run::work_box_text(
                Some(q),
                "Gemma 4 26B",
                "RAID 1 is a mirroring technique."
            ),
            "exactly the question and Gemma's answer, nothing after it"
        );
    });
    view.read_with(window, |app, cx| {
        assert_ne!(
            app.composer.read(cx).text(),
            want_first,
            "the old copy is gone"
        );
    });
}

// ---- RC2-R2F4: a card's Stop stays put; answers read as formatted text ---------

/// RC2-R2F4 (usability RC2 round 2, F4: "while a card streams, its Stop
/// button slides down as text arrives, so my clicks on Stop missed twice").
/// Stop sits in the card's header: it does not move while the answer
/// grows, and a click on where it was still stops that member.
#[gpui::test]
fn rc2r2f4_a_cards_stop_stays_put_while_it_streams(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r2f4_a_cards_stop_stays_put_while_it_streams").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| holds("RAID began"));
    let (b, _) = stub::spawn(|_| holds("In 1987"));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    ask_two(
        window,
        &view,
        &a,
        &b,
        "write 300 words on the history of RAID",
    );
    settle(window, &view, "both streaming", |app| {
        round_one(app).is_some_and(|r| r.turns.iter().all(|t| !t.text.is_empty()))
    });
    let stop = leak(crate::council_view::stop_selector(1, "m1"));
    let card = leak(crate::council_view::card_selector(1, "m1"));
    let before = window
        .debug_bounds(stop)
        .expect("Stop shows while it answers");
    let card_before = window.debug_bounds(card).unwrap();
    view.update(window, |app, cx| {
        let page = app.councils.get_mut(app.sidebar.active_id()).unwrap();
        let t = page.council.round_mut(1).unwrap().turn_mut("m1").unwrap();
        for i in 0..40 {
            t.text.push_str(&format!(" at Berkeley, line {i}.\n"));
        }
        cx.notify();
    });
    window.run_until_parked();
    let card_after = window.debug_bounds(card).unwrap();
    assert!(
        card_after.size.height > card_before.size.height,
        "the answer grew"
    );
    assert_eq!(
        window.debug_bounds(stop),
        Some(before),
        "Stop did not move while the answer grew"
    );
    window.simulate_click(before.center(), gpui::Modifiers::default());
    settle(window, &view, "the member stopped", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Stopped)
    });
    click(window, crate::council_view::STOP_ALL_SELECTOR);
}

/// RC2-R2F4 (usability RC2 round 2, F6: Codex and Gemma cards showed raw
/// markdown, "**ZFS**" and "[OpenZFS docs](https://…)"). A card's answer is
/// drawn as formatted text, as Chat draws a reply: the stars and the link
/// syntax are gone from what is drawn.
#[gpui::test]
fn rc2r2f4_answers_are_drawn_as_formatted_text(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r2f4_answers_are_drawn_as_formatted_text").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(window, &view, &FOUR[1..3]);
    view.update(window, |app, cx| {
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("ZFS or btrfs?"),
            &[
                (
                    "m1",
                    answered(),
                    "**ZFS** is safer; see [OpenZFS docs](https://openzfs.org).",
                ),
                ("m2", answered(), "Use **RAID 0** never."),
            ],
        );
        cx.notify();
    });
    window.run_until_parked();
    let mut card = |m: &str| {
        window
            .debug_bounds(leak(crate::council_view::card_selector(1, m)))
            .unwrap()
    };
    let (c1, c2) = (card("m1"), card("m2"));
    let md = window
        .debug_bounds("md-reading-column")
        .expect("the answer is drawn by the formatted-text renderer");
    assert!(
        c1.contains(&md.center()) || c2.contains(&md.center()),
        "inside a card"
    );
    // What the card draws for that answer: the words, bold and a link,
    // without the markup.
    match crate::council_view::card_body_plan(
        "**ZFS** is safer; see [OpenZFS docs](https://openzfs.org).",
    )
    .first()
    {
        Some(crate::markdown_plan::PlanBlock::Text { text, .. }) => {
            assert_eq!(text.text.as_ref(), "ZFS is safer; see OpenZFS docs.");
            assert_eq!(text.urls.as_slice(), ["https://openzfs.org"]);
        }
        other => panic!("a paragraph, not {other:?}"),
    }
}

// ---- RC2-R2F5: a card says which tools its member used ----------------------

/// RC2-R2F5 (usability RC2 round 2, F8: "the cards do not show which tool
/// each member used; only Codex's own words mention `ls -la`"). A member
/// that read a file says so on its card, in a quiet "Used:" line and in
/// Details, and both are still there after a relaunch.
#[gpui::test]
fn rc2r2f5_a_card_names_the_tools_its_member_used(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::rc2r2f5_a_card_names_the_tools_its_member_used")
    else {
        return;
    };
    cx.executor().allow_parking();
    std::fs::write(home.join("notes.txt"), "council-note-42\n").unwrap();
    let (a, _) = tool_model(|body| match last_tool_result(body) {
        Some(got) => words_round(&format!("The file says: {got}")),
        None => tool_round("r1", "fs_read", serde_json::json!({"path": "notes.txt"})),
    });
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["No tools needed."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "What does notes.txt say?");
    settle(window, &view, "both answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    let used = leak(crate::council_view::tools_used_selector(1, "m1"));
    assert!(window.debug_bounds(used).is_some(), "the quiet line shows");
    assert!(
        window
            .debug_bounds(leak(crate::council_view::tools_used_selector(1, "m2")))
            .is_none(),
        "a member that used no tool has no line"
    );
    let check = |app: &AwiliApp| {
        let page = app.active_council().unwrap();
        let r = page.council.round(1).unwrap();
        let t = r.turn("m1").unwrap();
        let line = crate::council_view::tools_used_line(page, &t.id).expect("a line");
        assert!(line.starts_with("Used: "), "{line}");
        assert!(line.contains("notes.txt"), "{line}");
        let d = crate::council_view::card_details_with_tools(page, r, t);
        assert!(
            d.iter()
                .any(|(k, v)| *k == "Tools used" && v.contains("notes.txt")),
            "{d:?}"
        );
    };
    view.read_with(window, |app, _| check(app));
    wait_store("the tools with the turn", |s| {
        let t = s.council_turns(&conv).ok()?;
        t.iter()
            .any(|t| {
                t.tools_used
                    .as_deref()
                    .is_some_and(|u| u.contains("notes.txt"))
            })
            .then_some(())
    });
    let (view2, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1400.), px(900.)));
    view2.update(window2, |app, cx| {
        two_member_council(app, &a, &b);
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), conv);
        check(app);
    });
}

/// RC2-R2L (RC2-R2F review P3 on a38e5e9b: "Used" was kept only when the
/// member finished, so a quit mid-answer lost it). A member that read a
/// file and is still answering when Awili closes keeps its tools: they are
/// saved as the tool starts, and after the relaunch its cut-off card still
/// says what it used.
#[gpui::test]
fn rc2r2l_a_quit_mid_answer_keeps_the_tools_used(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::rc2r2l_a_quit_mid_answer_keeps_the_tools_used")
    else {
        return;
    };
    cx.executor().allow_parking();
    std::fs::write(home.join("notes.txt"), "council-note-42\n").unwrap();
    // Gemma reads notes.txt, then takes its time answering (until released).
    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let r = release.clone();
    let (a, _) = tool_model(move |body| match last_tool_result(body) {
        Some(_) => {
            while !r.load(std::sync::atomic::Ordering::Acquire) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            words_round("Done.")
        }
        None => tool_round("r1", "fs_read", serde_json::json!({"path": "notes.txt"})),
    });
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["No tools needed."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "What does notes.txt say?");
    settle(
        window,
        &view,
        "Gemma read the file and is still answering",
        |app| {
            let Some(t) = turn_of(app, 1, "m1") else {
                return false;
            };
            t.status.is_running()
                && crate::council_view::tools_used_line(app.active_council().unwrap(), &t.id)
                    .is_some_and(|l| l.contains("notes.txt"))
        },
    );
    wait_store("the tools on the running turn", |s| {
        let t = s.council_turns(&conv).ok()?;
        t.iter()
            .any(|t| {
                t.member_id == "m1"
                    && t.status == "running"
                    && t.tools_used
                        .as_deref()
                        .is_some_and(|u| u.contains("notes.txt"))
            })
            .then_some(())
    });
    // The relaunch, while Gemma is still answering.
    let (view2, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1400.), px(900.)));
    view2.update(window2, |app, cx| {
        two_member_council(app, &a, &b);
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), conv);
        let page = app.active_council().expect("restored");
        let t = page.council.round(1).unwrap().turn("m1").unwrap();
        assert_eq!(t.status, awili_council::TurnStatus::Stopped);
        let line = crate::council_view::tools_used_line(page, &t.id).expect("a line");
        assert!(line.contains("notes.txt"), "{line}");
    });
    release.store(true, std::sync::atomic::Ordering::Release);
}

/// RC2-R2M (RC2-R2F review P3 on 93676322: a Retry rewrote the running turn
/// without its tools, and saved none while it had written nothing new). A
/// member answered after reading a file; the owner asks it again; Awili
/// closes while the retry is still working, before any new text. After the
/// relaunch the card keeps its answer and still says what it used.
#[gpui::test]
fn rc2r2m_a_quit_during_a_retry_keeps_the_tools_used(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::rc2r2m_a_quit_during_a_retry_keeps_the_tools_used")
    else {
        return;
    };
    cx.executor().allow_parking();
    std::fs::write(home.join("notes.txt"), "council-note-42\n").unwrap();
    // Gemma reads notes.txt and answers; asked again, it reads it again and
    // then takes its time (until released).
    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let answers = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (r, n) = (release.clone(), answers.clone());
    let (a, _) = tool_model(move |body| match last_tool_result(body) {
        Some(got) => {
            if n.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                while !r.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            words_round(&format!("The file says: {got}"))
        }
        None => tool_round("r1", "fs_read", serde_json::json!({"path": "notes.txt"})),
    });
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["No tools needed."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "What does notes.txt say?");
    settle(window, &view, "both answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    view.update(window, |app, cx| app.council_retry_member(1, "m1", cx));
    settle(
        window,
        &view,
        "the retry read the file and is working",
        |app| {
            answers.load(std::sync::atomic::Ordering::SeqCst) == 2
                && turn_of(app, 1, "m1").is_some_and(|t| t.status.is_running())
        },
    );
    wait_store("the tools on the retried running turn", |s| {
        let t = s.council_turns(&conv).ok()?;
        t.iter()
            .any(|t| {
                t.member_id == "m1"
                    && t.status == "running"
                    && t.message_id.is_some()
                    && t.tools_used
                        .as_deref()
                        .is_some_and(|u| u.contains("notes.txt"))
            })
            .then_some(())
    });
    // The relaunch, while the retry is still working.
    let (view2, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1400.), px(900.)));
    view2.update(window2, |app, cx| {
        two_member_council(app, &a, &b);
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), conv);
        let page = app.active_council().expect("restored");
        let t = page.council.round(1).unwrap().turn("m1").unwrap();
        assert!(t.text.contains("council-note-42"), "{:?}", t.text);
        let line = crate::council_view::tools_used_line(page, &t.id).expect("a line");
        assert!(line.contains("notes.txt"), "{line}");
    });
    release.store(true, std::sync::atomic::Ordering::Release);
}

// ---- RC2-R2F8: an API model on a council reads where the owner can ------------

/// RC2-R2F8 (usability RC2 round 2, F3 MAJOR: the council's Gemma asked to
/// glob `/Projects/…/w1/*`, then said the path "is outside my session root"; a
/// council has no folder to point it at). Like the Claude and Codex
/// members, the model on this Mac reads a folder outside the owner's home
/// freely — no card — and names the files by their full paths; a change
/// there still stops at the owner's card, and "Deny" leaves nothing
/// written.
#[gpui::test]
fn rc2r2f8_an_api_member_reads_anywhere_and_still_asks_to_change(cx: &mut gpui::TestAppContext) {
    let Some(_home) =
        isolated("council_tests::rc2r2f8_an_api_member_reads_anywhere_and_still_asks_to_change")
    else {
        return;
    };
    cx.executor().allow_parking();
    // A folder outside the owner's home (and outside every council folder).
    let outside = tempfile::tempdir().unwrap();
    let w1 = outside.path().join("w1");
    std::fs::create_dir_all(&w1).unwrap();
    std::fs::write(w1.join("alpha.txt"), "one\n").unwrap();
    let w1s = w1.display().to_string();
    let notes = w1.join("notes.txt");
    let (glob, read) = (format!("{w1s}/*"), format!("{w1s}/alpha.txt"));
    // Gemma: glob the folder, read what it found, answer.
    let (a, seen_a) = tool_model(move |body| {
        let rounds = body.matches("\"role\":\"tool\"").count();
        match rounds {
            0 => tool_round(
                "g1",
                "fs_glob",
                serde_json::json!({"pattern": glob.clone()}),
            ),
            1 => tool_round("r1", "fs_read", serde_json::json!({"path": read.clone()})),
            _ => words_round(&format!(
                "alpha.txt says: {}",
                last_tool_result(body).unwrap_or_default()
            )),
        }
    });
    // The provider model tries to write notes.txt there.
    let write_to = notes.display().to_string();
    let (p, _) = tool_model(move |body| match last_tool_result(body) {
        Some(_) => words_round("I did not write it."),
        None => tool_round(
            "w1",
            "fs_write",
            serde_json::json!({"path": write_to.clone(), "contents": "hi"}),
        ),
    });
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.work.engine.base_url = a.clone();
        app.work.engine.model = "m".into();
        app.choices = vec![awili_config::ModelChoice {
            provider: "spark".into(),
            kind: awili_config::ProviderKind::OpenAi,
            base_url: format!("{p}/v1"),
            model: "qwen38-27b".into(),
            api_key: None,
        }];
        app.council_last_group = Some(vec![
            ("in-app".into(), "Gemma 4 26B".into()),
            ("model:spark:qwen38-27b".into(), "qwen38-27b".into()),
        ]);
        app.set_mode(Mode::Council, cx);
        app.composer.update(cx, |c, cx| {
            c.set_text(&format!("What files are in {w1s}?"), cx)
        });
        app.submit_composer_draft(cx);
    });
    // The write waits for the owner; Gemma's reads never did.
    settle(window, &view, "the write's card", |app| {
        app.active_council()
            .is_some_and(|p| !p.approvals.is_empty())
    });
    settle(window, &view, "Gemma answered", |app| {
        round_one(app)
            .and_then(|r| r.turn("m1"))
            .is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
    });
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        let m1 = round_one(app).unwrap().turn("m1").unwrap().id.clone();
        assert!(
            !page.approvals.contains_key(&m1),
            "Gemma's reads asked nothing"
        );
        assert_eq!(page.approvals.len(), 1, "only the write asks");
        assert_eq!(answer(app, "m1"), "alpha.txt says: one\n");
    });
    let first: serde_json::Value = serde_json::from_str(&seen_a.lock().unwrap()[0]).unwrap();
    assert!(
        first["messages"][0]["content"]
            .as_str()
            .is_some_and(|s| s.contains(crate::council_exec::ANY_FOLDER_NOTE)),
        "Gemma is told its file tools take full paths: {}",
        first["messages"][0]
    );
    let glob_result = seen_a
        .lock()
        .unwrap()
        .get(1)
        .and_then(|b| last_tool_result(b))
        .unwrap_or_default();
    assert!(
        glob_result.contains(&format!("{w1s}/alpha.txt")),
        "the glob names the file by its full path: {glob_result}"
    );
    click(
        window,
        leak(crate::council_view::approval_selector(1, "m2", "deny")),
    );
    settle(window, &view, "both answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    assert!(!notes.exists(), "Deny left nothing written");
}

// ---- RC2-R2F10: a Codex member is ready before the question -------------------

#[cfg(unix)]
/// RC2-R2F10 (usability RC2 round 2, F7: Codex wrote nothing for 19-33 s;
/// ~1.7 s of every start was Awili spawning Codex and Codex starting its
/// MCP servers). While the council waits, its Codex member's server is
/// started and its thread opened; the owner's message is answered on it
/// (no start when he presses Enter), and once the round is over the next
/// one is made ready.
#[gpui::test]
fn rc2r2f10_a_codex_member_is_ready_before_the_question(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("council_tests::rc2r2f10_a_codex_member_is_ready_before_the_question")
    else {
        return;
    };
    cx.executor().allow_parking();
    crate::codex_app_server::PREWARM.store(true, std::sync::atomic::Ordering::Release);
    // A scripted Codex app server: notes each start, opens a thread, then
    // answers one turn.
    let bin = home.join("codex");
    let starts = home.join("starts");
    let script = format!(
        "#!/bin/sh\n\
         echo start >> {starts}\n\
         read -r l\n\
         printf '%s\\n' '{{\"id\":1,\"result\":{{\"userAgent\":\"fake\"}}}}'\n\
         read -r l\nread -r l\n\
         printf '%s\\n' '{{\"id\":2,\"result\":{{\"thread\":{{\"id\":\"thr-w\"}}}}}}'\n\
         read -r l\n\
         echo asked >> {starts}\n\
         printf '%s\\n' '{{\"id\":3,\"result\":{{\"turn\":{{\"id\":\"t1\",\"status\":\"inProgress\"}}}}}}'\n\
         printf '%s\\n' '{{\"method\":\"item/agentMessage/delta\",\"params\":{{\"threadId\":\"thr-w\",\"turnId\":\"t1\",\"itemId\":\"m1\",\"delta\":\"Mirroring.\"}}}}'\n\
         printf '%s\\n' '{{\"method\":\"turn/completed\",\"params\":{{\"threadId\":\"thr-w\",\"turn\":{{\"id\":\"t1\",\"items\":[],\"status\":\"completed\"}}}}}}'\n\
         read -r l\nexit 0\n",
        starts = starts.display()
    );
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = || -> Vec<String> {
        std::fs::read_to_string(&starts)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    let started = || log().iter().filter(|l| *l == "start").count();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.work.agents.codex_bin = Some(bin.display().to_string());
        app.council_last_group = Some(vec![("agent:codex".into(), "Codex".into())]);
        app.set_mode(Mode::Council, cx);
    });
    let wait = |what: &str, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + ANSWER_DEADLINE;
        while !done() {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    wait("Codex made ready", &|| {
        crate::codex_app_server::warm_count() == 1
    });
    assert_eq!(started(), 1);
    view.update(window, |app, cx| {
        app.composer.update(cx, |c, cx| {
            c.set_text("In one sentence: what is RAID 1?", cx)
        });
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "Codex answered", |app| {
        idle(app) && answer(app, "m1") == "Mirroring."
    });
    assert_eq!(
        log()[..2],
        ["start", "asked"],
        "the server made ready answered; nothing was started at Enter"
    );
    wait("the next round made ready", &|| {
        crate::codex_app_server::warm_count() == 1 && started() == 2
    });
}

// ---- RC2-R2F11: the council's time moves while it answers -----------------------

/// RC2-R2F11 (usability RC2 round 2, F4: the header read "1 round · 0 s ·
/// $0" until the round was done). While a member answers, the council's
/// time counts from when it was asked, and the page is drawn again every
/// second (also while no word arrives); when the round ends, so does the
/// repaint.
#[gpui::test]
fn rc2r2f11_the_councils_time_moves_while_it_answers(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r2f11_the_councils_time_moves_while_it_answers").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| holds("Keep Gemma as the"));
    let (b, _) = stub::spawn(|_| holds("Move coding"));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    ask_two(window, &view, &a, &b, "Which one?");
    settle(window, &view, "both streaming", |app| {
        round_one(app).is_some_and(|r| r.turns.iter().all(|t| !t.text.is_empty()))
    });
    view.update(window, |app, _| {
        let page = app.councils.get_mut(app.sidebar.active_id()).unwrap();
        assert!(
            page.ticking,
            "the page is drawn every second while it answers"
        );
        // Asked 7 s ago, still answering.
        let started = crate::feed::unix_now() - 7;
        for t in &mut page.council.round_mut(1).unwrap().turns {
            t.started_ts = started;
        }
        let tally = crate::run_footer::council_tally(&page.council);
        assert!(tally.starts_with("1 round \u{00B7} 7 s"), "{tally}");
    });
    // RC2-R2G9 (RC2-R2F review P3: the test read the tally, not the page):
    // what the header DRAWS moves with the clock while the members send no
    // word. Each simulated second (the wall clock past its next second
    // too) the tick alone draws the page again, with a later time.
    let secs = |tally: &str| -> u64 {
        tally
            .split('\u{00B7}')
            .nth(1)
            .and_then(|t| t.trim().strip_suffix(" s"))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("a time in seconds: {tally}"))
    };
    let mut shown = Vec::new();
    for _ in 0..2 {
        let now = crate::feed::unix_now();
        while crate::feed::unix_now() == now {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        crate::council_view::DRAWN_TALLIES.with(|d| d.borrow_mut().clear());
        window
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        window.run_until_parked();
        let drawn = crate::council_view::drawn_tallies();
        let last = drawn
            .last()
            .unwrap_or_else(|| panic!("the tick drew no header: {drawn:?}"))
            .clone();
        shown.push(secs(&last));
    }
    assert!(
        shown[0] >= 8 && shown[1] > shown[0],
        "the drawn time moves on: {shown:?}"
    );
    view.read_with(window, |app, _| {
        let r = round_one(app).unwrap();
        assert!(
            r.turns
                .iter()
                .all(|t| t.text == "Keep Gemma as the" || t.text == "Move coding"),
            "no word came meanwhile"
        );
    });
    click(window, crate::council_view::STOP_ALL_SELECTOR);
    settle(window, &view, "stopped", idle);
    window
        .executor()
        .advance_clock(std::time::Duration::from_secs(2));
    settle(window, &view, "the repaint ended", |app| {
        !app.active_council().unwrap().ticking
    });
}

// ---- RC2-R2G6: a council's ready Codex server ends with the council -------------

#[cfg(unix)]
/// RC2-R2G6 (RC2-R2F review P2: up to four Codex process groups stayed
/// after their council was gone). A council's Codex member made ready ahead
/// ends when that council is archived, when it is deleted, and when Awili
/// quits (the quit hook ends every Codex group it started).
#[gpui::test]
fn rc2r2g6_a_councils_ready_codex_ends_with_the_council(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("council_tests::rc2r2g6_a_councils_ready_codex_ends_with_the_council")
    else {
        return;
    };
    cx.executor().allow_parking();
    crate::codex_app_server::PREWARM.store(true, std::sync::atomic::Ordering::Release);
    // A scripted Codex app server: notes its process id, opens a thread,
    // then waits for its question.
    let bin = home.join("codex");
    let pids = home.join("pids");
    let script = format!(
        "#!/bin/sh\n\
         echo $$ >> {pids}\n\
         read -r l\n\
         printf '%s\\n' '{{\"id\":1,\"result\":{{\"userAgent\":\"fake\"}}}}'\n\
         read -r l\nread -r l\n\
         printf '%s\\n' '{{\"id\":2,\"result\":{{\"thread\":{{\"id\":\"thr-w\"}}}}}}'\n\
         read -r l\nexit 0\n",
        pids = pids.display()
    );
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Running: listed, and not a finished process waiting to be reaped (a
    // quit ends the process; Awili, quitting, never reaps it).
    let alive = |pid: &str| {
        std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .is_ok_and(|o| {
                let stat = String::from_utf8_lossy(&o.stdout);
                let stat = stat.trim();
                !stat.is_empty() && !stat.starts_with('Z')
            })
    };
    let wait = |what: &str, within: std::time::Duration, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + within;
        while !done() {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    // The process of the one ready server, once there is one.
    let ready_pid = |what: &str| -> String {
        wait(what, ANSWER_DEADLINE, &|| {
            crate::codex_app_server::warm_count() == 1
        });
        std::fs::read_to_string(&pids)
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .to_string()
    };
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.work.agents.codex_bin = Some(bin.display().to_string());
        app.council_last_group = Some(vec![("agent:codex".into(), "Codex".into())]);
        app.set_mode(Mode::Council, cx);
    });

    // Archived: its ready server ends.
    let first = ready_pid("the first council made ready");
    let id = view.read_with(window, |app, _| {
        let id = app.sidebar.active_id().to_string();
        assert!(app.councils.contains_key(&id), "a council is open");
        id
    });
    view.update_in(window, |app, window, cx| {
        app.archive_conversation(&id, window, cx)
    });
    wait("the archived council's server ends", ENDS_WITHIN, &|| {
        !alive(&first)
    });

    // Deleted: its ready server ends.
    let has_ready = view.read_with(window, |app, _| {
        assert_eq!(app.mode, Mode::Council, "archiving stays in Council");
        assert_eq!(
            app.conversations.get(app.sidebar.active_id()).unwrap().kind,
            awili_core::ConvKind::Council
        );
        app.councils.contains_key(app.sidebar.active_id())
    });
    if !has_ready {
        view.update_in(window, |app, window, cx| app.new_conversation(window, cx));
    }
    let second = ready_pid("the next council made ready");
    assert_ne!(second, first);
    let id = view.read_with(window, |app, _| {
        let id = app.sidebar.active_id().to_string();
        assert!(app.councils.contains_key(&id), "a council is open");
        id
    });
    view.update_in(window, |app, window, cx| {
        app.delete_conversation(&id, window, cx)
    });
    wait("the deleted council's server ends", ENDS_WITHIN, &|| {
        !alive(&second)
    });

    // Awili quits: every server it started ends.
    let has_ready = view.read_with(window, |app, _| {
        assert_eq!(app.mode, Mode::Council, "deleting stays in Council");
        assert_eq!(
            app.conversations.get(app.sidebar.active_id()).unwrap().kind,
            awili_core::ConvKind::Council
        );
        app.councils.contains_key(app.sidebar.active_id())
    });
    if !has_ready {
        view.update_in(window, |app, window, cx| app.new_conversation(window, cx));
    }
    let third = ready_pid("a council made ready before the quit");
    assert!(third != first && third != second);
    crate::code_provider::kill_all_active_groups();
    wait("the quit ends the ready server", ENDS_WITHIN, &|| {
        !alive(&third)
    });
}

// ---- RC2-R2G8: the "Used:" line names real tools only ---------------------------

#[cfg(unix)]
/// RC2-R2G8 (RC2-R2F review P3: every non-✓ activity line was kept as a
/// tool, so a Codex connection retry could read "Used: Codex lost its
/// connection…"). A Codex member whose turn only retried its connection
/// has no "Used:" line; a turn that also ran a command names the command
/// and not the retry.
#[gpui::test]
fn rc2r2g8_a_retry_note_is_no_tool_used(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::rc2r2g8_a_retry_note_is_no_tool_used") else {
        return;
    };
    cx.executor().allow_parking();
    // A scripted Codex app server: each start answers one turn; every turn
    // has a connection retry, the second also runs `ls`.
    let bin = home.join("codex");
    let count = home.join("count");
    let say = |line: &str| format!("printf '%s\\n' '{line}'\n");
    let retry = say(
        r#"{"method":"error","params":{"threadId":"thr-w","turnId":"t1","error":{"message":"stream disconnected"},"willRetry":true}}"#,
    );
    let ls = [
        say(
            r#"{"method":"item/started","params":{"threadId":"thr-w","turnId":"t1","item":{"type":"commandExecution","id":"c1","command":"ls","commandActions":[],"cwd":"/","status":"inProgress"}}}"#,
        ),
        say(
            r#"{"method":"item/completed","params":{"threadId":"thr-w","turnId":"t1","item":{"type":"commandExecution","id":"c1","command":"ls","commandActions":[],"cwd":"/","status":"completed","exitCode":0}}}"#,
        ),
    ]
    .concat();
    let script = format!(
        "#!/bin/sh\n\
         n=$(cat {count} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {count}\n\
         read -r l\n\
         printf '%s\\n' '{{\"id\":1,\"result\":{{\"userAgent\":\"fake\"}}}}'\n\
         read -r l\nread -r l\n\
         printf '%s\\n' '{{\"id\":2,\"result\":{{\"thread\":{{\"id\":\"thr-w\"}}}}}}'\n\
         read -r l\n\
         printf '%s\\n' '{{\"id\":3,\"result\":{{\"turn\":{{\"id\":\"t1\",\"status\":\"inProgress\"}}}}}}'\n\
         {retry}\
         if [ $n -gt 1 ]; then\n{ls}fi\n\
         printf '%s\\n' '{{\"method\":\"item/agentMessage/delta\",\"params\":{{\"threadId\":\"thr-w\",\"turnId\":\"t1\",\"itemId\":\"m1\",\"delta\":\"Answer.\"}}}}'\n\
         printf '%s\\n' '{{\"method\":\"turn/completed\",\"params\":{{\"threadId\":\"thr-w\",\"turn\":{{\"id\":\"t1\",\"items\":[],\"status\":\"completed\"}}}}}}'\n\
         read -r l\nexit 0\n",
        count = count.display()
    );
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.work.agents.codex_bin = Some(bin.display().to_string());
        app.council_last_group = Some(vec![("agent:codex".into(), "Codex".into())]);
        app.set_mode(Mode::Council, cx);
    });
    let ask = |window: &mut gpui::VisualTestContext, text: &str| {
        view.update(window, |app, cx| {
            app.composer.update(cx, |c, cx| c.set_text(text, cx));
            app.submit_composer_draft(cx);
        });
    };
    let used = |app: &AwiliApp, round: u32| {
        let page = app.active_council().unwrap();
        let t = page.council.round(round)?.turn("m1")?;
        crate::council_view::tools_used_line(page, &t.id)
    };
    let answered = |app: &AwiliApp, round: u32| {
        idle(app)
            && app
                .active_council()
                .and_then(|p| p.council.round(round))
                .and_then(|r| r.turn("m1"))
                .is_some_and(|t| t.text == "Answer.")
    };

    ask(window, "What is RAID 1?");
    settle(window, &view, "the retry-only turn answered", |app| {
        answered(app, 1)
    });
    view.read_with(window, |app, _| {
        assert_eq!(used(app, 1), None, "a retry note is no tool");
    });
    assert!(
        window
            .debug_bounds(leak(crate::council_view::tools_used_selector(1, "m1")))
            .is_none(),
        "no quiet line is drawn"
    );

    ask(window, "And RAID 5?");
    settle(window, &view, "the turn with a command answered", |app| {
        answered(app, 2)
    });
    view.read_with(window, |app, _| {
        // RC2-R63: the command itself, with no "$ " in front.
        assert_eq!(used(app, 2).as_deref(), Some("Used: ls"));
    });
    assert!(window
        .debug_bounds(leak(crate::council_view::tools_used_selector(2, "m1")))
        .is_some());
}

// ---- RC2-R3: usability RC2 round 3 ------------------------------------------------

/// RC2-R3 (usability RC2 round 3, Finding 8: the summary advised "proceed
/// with the one-step action" the owner had just declined, and named him):
/// "Don't change anything" on a member's plan is kept with the council (in
/// `awili.db` too, so a relaunch keeps it), and the sum-up after it is told
/// that plan was declined and asked to speak to the owner as "you".
#[gpui::test]
fn rc2r3_the_sum_up_is_told_a_plan_was_declined(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r3_the_sum_up_is_told_a_plan_was_declined").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| stub::Reply::text(&["1. Write notes.txt."]));
    let (b, seen_b) = stub::spawn(|body: &str| {
        if body.contains("Write one combined answer") {
            stub::Reply::text(&["Nothing was written; you declined it."])
        } else {
            stub::Reply::text(&["Write it."])
        }
    });
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "create notes.txt containing hi");
    settle(window, &view, "round 1 answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    // Gemma's answer is a plan waiting for the owner's OK.
    let turn = view.update(window, |app, cx| {
        let page = app.councils.get_mut(&conv).unwrap();
        let id = page
            .council
            .round(1)
            .unwrap()
            .turn("m1")
            .unwrap()
            .id
            .clone();
        page.plans.insert(id.clone(), "1. Write notes.txt.".into());
        cx.notify();
        id
    });
    window.run_until_parked();
    click(
        window,
        leak(crate::council_view::plan_decline_selector(1, "m1")),
    );
    view.read_with(window, |app, _| {
        assert_eq!(
            app.active_council().unwrap().plan_outcomes.get(&turn),
            Some(&awili_council::PlanOutcome::Declined)
        );
    });
    let kept = wait_store("the declined plan", |s| {
        s.get_app_state(&crate::council_view::plans_key(&conv))
            .ok()
            .flatten()
    });
    assert_eq!(
        crate::council_view::plan_outcomes_from_json(&kept).get(&turn),
        Some(&awili_council::PlanOutcome::Declined)
    );
    click(window, crate::council_view::SUM_UP_SELECTOR);
    click(
        window,
        leak(crate::council_view::sum_up_member_selector("m2")),
    );
    settle(window, &view, "the summary", |app| {
        idle(app) && rounds(app) == 2
    });
    let owner = crate::council_run::owner_name();
    let sb = seen_b.lock().unwrap();
    assert_eq!(sb.len(), 2);
    let body: serde_json::Value = serde_json::from_str(&sb[1].body).unwrap();
    let asked = body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string();
    for words in [
        format!(
            "Gemma 4 26B: 1. Write notes.txt.\n\n({owner} declined Gemma 4 26B\u{2019}s plan: \
             nothing was changed.)"
        ),
        format!("Speak to {owner} as \"you\" and never use his name"),
        "do not recommend doing it anyway".to_string(),
    ] {
        assert!(asked.contains(&words), "{words:?} missing: {asked}");
    }
}

/// RC2-R3M3 (RC2-R3 review P2 on 82deea7a): "Make the changes" is kept as
/// approved when the follow-up starts, not as done. Here the follow-up fails:
/// the plan is kept as "approved, but carrying it out failed" (on the page
/// and in `awili.db`), and the sum-up after it is told so \u{2014} never that it
/// was carried out.
#[gpui::test]
fn rc2r3m3_a_failed_follow_up_is_not_told_as_carried_out(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r3m3_a_failed_follow_up_is_not_told_as_carried_out").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| stub::Reply::text(&["Keep it as it is."]));
    let (b, seen_b) = stub::spawn(|body: &str| {
        if body.contains("Write one combined answer") {
            stub::Reply::text(&["Summary."])
        } else if body.contains("You proposed this plan") {
            let mut r = stub::Reply::text(&[]);
            r.status = 500;
            r
        } else {
            stub::Reply::text(&["1. Write notes.txt."])
        }
    });
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "create notes.txt containing hi");
    settle(window, &view, "round 1 answered", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    // Hermes's answer is a plan waiting for the owner's OK.
    let turn = view.update(window, |app, cx| {
        let page = app.councils.get_mut(&conv).unwrap();
        let id = page
            .council
            .round(1)
            .unwrap()
            .turn("m2")
            .unwrap()
            .id
            .clone();
        page.plans.insert(id.clone(), "1. Write notes.txt.".into());
        cx.notify();
        id
    });
    window.run_until_parked();
    click(
        window,
        leak(crate::council_view::plan_approve_selector(1, "m2")),
    );
    settle(window, &view, "the follow-up failed", |app| {
        idle(app)
            && turn_of(app, 1, "m2")
                .is_some_and(|t| matches!(t.status, awili_council::TurnStatus::Failed { .. }))
    });
    view.read_with(window, |app, _| {
        assert_eq!(
            app.active_council().unwrap().plan_outcomes.get(&turn),
            Some(&awili_council::PlanOutcome::Failed)
        );
    });
    wait_store("the failed follow-up", |s| {
        let raw = s
            .get_app_state(&crate::council_view::plans_key(&conv))
            .ok()
            .flatten()?;
        (crate::council_view::plan_outcomes_from_json(&raw).get(&turn)
            == Some(&awili_council::PlanOutcome::Failed))
        .then_some(())
    });
    click(window, crate::council_view::SUM_UP_SELECTOR);
    click(
        window,
        leak(crate::council_view::sum_up_member_selector("m2")),
    );
    settle(window, &view, "the summary", |app| {
        idle(app) && rounds(app) == 2
    });
    let owner = crate::council_run::owner_name();
    let sb = seen_b.lock().unwrap();
    let body: serde_json::Value = serde_json::from_str(&sb.last().unwrap().body).unwrap();
    let asked = body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default()
        .to_string();
    assert!(asked.contains("Write one combined answer"), "{asked}");
    assert!(
        asked.contains(&format!(
            "({owner} approved Hermes\u{2019}s plan, but carrying it out failed: it was not \
             done, or only partly.)"
        )),
        "{asked}"
    );
    assert!(!asked.contains("and it was carried out"), "{asked}");
}
/// RC2-R3 (usability RC2 round 3, Major 3: the summary card sat at the top
/// of the page, above all rounds, also after a relaunch): a summary comes
/// right after the round it summed up \u{2014} in the order kept in `awili.db`,
/// drawn so, and the same in a second Awili on that database (a relaunch).
#[gpui::test]
fn rc2r3_the_summary_stays_after_its_round_across_a_relaunch(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r3_the_summary_stays_after_its_round_across_a_relaunch")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma for now."]));
    let (b, _) = stub::spawn(|body: &str| {
        if body.contains("Write one combined answer") {
            stub::Reply::text(&["Both say keep Gemma."])
        } else {
            stub::Reply::text(&["Move coding to Qwen."])
        }
    });
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    let conv = ask_two(window, &view, &a, &b, "Gemma or Qwen?");
    settle(window, &view, "round 1", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    click(window, crate::council_view::SUM_UP_SELECTOR);
    click(
        window,
        leak(crate::council_view::sum_up_member_selector("m2")),
    );
    settle(window, &view, "the summary", |app| {
        idle(app) && rounds(app) == 2
    });
    window.run_until_parked();
    let order = |page: &crate::council_view::CouncilPage| -> Vec<(u32, awili_council::RoundKind)> {
        crate::council_view::rounds_in_order(&page.council)
            .iter()
            .map(|r| (r.number, r.kind))
            .collect()
    };
    let want = vec![
        (1, awili_council::RoundKind::Ask),
        (2, awili_council::RoundKind::SumUp),
    ];
    view.read_with(window, |app, _| {
        assert_eq!(order(app.active_council().unwrap()), want);
    });
    wait_store("both rounds kept", |s| {
        let t = s.council_turns(&conv).ok()?;
        (t.len() == 3 && settled(&t) && t.iter().any(|t| t.round_kind == "sum_up")).then_some(())
    });
    // The relaunch: a second Awili on the same database.
    let (view2, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window2.simulate_resize(size(px(1600.), px(900.)));
    view2.update(window2, |app, cx| {
        two_member_council(app, &a, &b);
        app.set_mode(Mode::Council, cx);
        assert_eq!(app.sidebar.active_id(), conv, "the council is open again");
        assert_eq!(order(app.active_council().expect("restored")), want);
        cx.notify();
    });
    window2.run_until_parked();
    // Drawn so: round 1's header, then the summary below it.
    click(window2, leak(crate::council_view::fold_selector(1)));
    let head = window2
        .debug_bounds(leak(crate::council_view::round_selector(1)))
        .expect("round 1 header");
    let summary = window2
        .debug_bounds(sel_card(2, "m2"))
        .expect("summary drawn");
    assert!(
        summary.top() > head.top(),
        "after a relaunch the summary still follows round 1"
    );
}

/// RC2-R3 (usability RC2 round 3, Minor 7): "Continue it" joins the old and
/// new words with a space ("managed striping", not "managedstriping"), and
/// while it runs the card's time runs again from where it stopped, so the
/// header's time ticks (it stood at "1m 30s"); when it ends, its time is the
/// stopped part and the continued part together.
#[gpui::test]
fn rc2r3_continue_joins_with_a_space_and_its_time_runs(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r3_continue_joins_with_a_space_and_its_time_runs").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = calls.clone();
    let (a, _) = stub::spawn(move |_| {
        if c.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            holds("Dedicated hardware controllers managed")
        } else {
            holds("striping, parity")
        }
    });
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Move coding to Qwen."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    ask_two(
        window,
        &view,
        &a,
        &b,
        "write 300 words on the history of RAID",
    );
    settle(window, &view, "Gemma streaming, Hermes done", |app| {
        !answer(app, "m1").is_empty()
            && status_of(app, "m2") == Some(awili_council::TurnStatus::Answered)
    });
    std::thread::sleep(std::time::Duration::from_millis(1100));
    click(window, leak(crate::council_view::stop_selector(1, "m1")));
    settle(window, &view, "Gemma stopped", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Stopped)
    });
    let stopped_ms = view.read_with(window, |app, _| {
        turn_of(app, 1, "m1").unwrap().elapsed_ms.expect("its time")
    });
    assert!(stopped_ms >= 1000, "{stopped_ms}");
    click(
        window,
        leak(crate::council_view::continue_selector(1, "m1")),
    );
    settle(window, &view, "Gemma continuing", |app| {
        answer(app, "m1").contains("parity")
    });
    view.read_with(window, |app, _| {
        assert_eq!(
            answer(app, "m1"),
            "Dedicated hardware controllers managed striping, parity"
        );
        let t = turn_of(app, 1, "m1").unwrap();
        assert!(t.status.is_running());
        assert_eq!(t.elapsed_ms, None, "its time runs again");
        let c = &app.active_council().unwrap().council;
        let now = crate::feed::unix_now();
        assert_ne!(
            crate::run_footer::council_tally_at(c, now),
            crate::run_footer::council_tally_at(c, now + 10),
            "the header's time ticks while it continues"
        );
        assert!(
            now - t.started_ts >= i64::try_from(stopped_ms / 1000).unwrap(),
            "on from where it stopped"
        );
    });
    click(window, leak(crate::council_view::stop_selector(1, "m1")));
    settle(window, &view, "Gemma stopped again", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Stopped)
    });
    view.read_with(window, |app, _| {
        let t = turn_of(app, 1, "m1").unwrap();
        assert!(t.elapsed_ms.unwrap() >= stopped_ms, "both parts counted");
    });
    assert_eq!(
        crate::council_run::continue_join("managed", "striping"),
        " "
    );
    assert_eq!(crate::council_run::continue_join("ended.", "Then"), " ");
    assert_eq!(
        crate::council_run::continue_join("managed ", "striping"),
        ""
    );
    assert_eq!(crate::council_run::continue_join("as the", " default"), "");
    assert_eq!(
        crate::council_run::continue_join("**RAID 1", "**: mirror"),
        ""
    );
    assert_eq!(crate::council_run::continue_join("", "x"), "");
}

/// RC2-R3 (usability RC2 round 3, F7 / Finding 5: Claude and Codex wrote
/// nothing for 13 s on a second message, every member about 22 s on the
/// first): a member whose memory the owner's own hooks bring
/// (`hooks_provide_memory`, his Claude and Codex) gets none of Awili's, so
/// it starts at once, without waiting for Awili's memory lookup; a member
/// that takes Awili's memory (Gemma) still waits for it.
#[gpui::test]
fn rc2r3_a_member_with_hook_memory_never_waits_for_the_lookup(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(home) =
        isolated("council_tests::rc2r3_a_member_with_hook_memory_never_waits_for_the_lookup")
    else {
        return;
    };
    cx.executor().allow_parking();
    let bins = home.join("bins");
    std::fs::create_dir_all(&bins).unwrap();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Move to Qwen."]));
    let (mem, hits, held) = held_memory(&home);
    held.store(true, SeqCst);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.council_last_group = Some(vec![
            ("agent:claude:sonnet".into(), "Claude Sonnet".into()),
            ("in-app".into(), "Gemma 4 26B".into()),
        ]);
        app.memory = mem;
        app.work.agents.claude_bin = Some(planning_claude(&bins));
        app.work.agents.memory = Some(awili_config::AgentMemoryConfig {
            hooks_provide_memory: true,
            ..Default::default()
        });
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
    });
    settle(
        window,
        &view,
        "Claude answered while the lookup is out",
        |app| {
            hits.load(SeqCst) > 0 && turn_of(app, 1, "m1").is_some_and(|t| !t.status.is_running())
        },
    );
    assert_eq!(starts(&bins).len(), 1, "Claude started at once");
    view.read_with(window, |app, _| {
        assert!(
            turn_of(app, 1, "m2").unwrap().status.is_running(),
            "Gemma waits for the lookup"
        );
    });
    assert_eq!(seen_a.lock().unwrap().len(), 0, "Gemma not asked yet");
    held.store(false, SeqCst);
    settle(window, &view, "Gemma answered after the lookup", |app| {
        turn_of(app, 1, "m2").is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
    });
    assert_eq!(seen_a.lock().unwrap().len(), 1);
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn answering_claude(dir: &std::path::Path, text: &str) -> String {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` in `dir` that answers `text` (no plan); each start's argv
/// is appended to `dir/claude-starts`.
fn answering_claude(dir: &std::path::Path, text: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s ' \"$@\" >> {d}/claude-starts\nprintf '\\n@@@\\n' >> {d}/claude-starts\n\
             case \"$*\" in *--input-format*) read -r l ;; esac\n\
             printf '%s\\n' \
             '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}' \
             '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"{text}\"}}'\n",
            d = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin.display().to_string()
}

/// RC2-R3M2 (RC2-R3 review P2 on 82deea7a): Awili's memory lookup for the
/// owner's message refused. A later round for that same message (here
/// "Ask one to sum up", by Claude) keeps the refusal for the members that
/// take Awili's memory, but Claude's memory comes from the owner's own
/// hooks: it is not stopped by the kept refusal, it answers.
#[gpui::test]
fn rc2r3m2_a_kept_refusal_never_stops_a_member_with_hook_memory(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(home) =
        isolated("council_tests::rc2r3m2_a_kept_refusal_never_stops_a_member_with_hook_memory")
    else {
        return;
    };
    cx.executor().allow_parking();
    let bins = home.join("bins");
    std::fs::create_dir_all(&bins).unwrap();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Move to Qwen."]));
    let (mem, hits, _held, refuse) = refusing_memory(&home);
    refuse.store(true, SeqCst);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.council_last_group = Some(vec![
            ("agent:claude:sonnet".into(), "Claude Sonnet".into()),
            ("in-app".into(), "Gemma 4 26B".into()),
        ]);
        app.memory = mem;
        app.work.agents.claude_bin = Some(answering_claude(&bins, "Keep Gemma, both say."));
        app.work.agents.memory = Some(awili_config::AgentMemoryConfig {
            hooks_provide_memory: true,
            ..Default::default()
        });
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
    });
    settle(
        window,
        &view,
        "round 1: Claude answered, Gemma refused",
        |app| {
            idle(app)
                && round_one(app).is_some_and(|r| !r.left_out.is_empty())
                && turn_of(app, 1, "m1")
                    .is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
        },
    );
    assert!(hits.load(SeqCst) > 0, "the lookup was made for Gemma");
    assert_eq!(seen_a.lock().unwrap().len(), 0, "Gemma was not asked");
    assert_eq!(starts(&bins).len(), 1, "Claude answered round 1");
    view.update(window, |app, cx| app.council_sum_up("m1", cx));
    settle(window, &view, "Claude summed up", |app| {
        idle(app)
            && turn_of(app, 2, "m1")
                .is_some_and(|t| t.status == awili_council::TurnStatus::Answered)
    });
    view.read_with(window, |app, _| {
        let r2 = app.active_council().unwrap().council.round(2).unwrap();
        assert_eq!(r2.kind, awili_council::RoundKind::SumUp);
        assert!(r2.left_out.is_empty(), "not refused: {:?}", r2.left_out);
        assert_eq!(r2.turns[0].text, "Keep Gemma, both say.");
    });
    assert_eq!(starts(&bins).len(), 2, "Claude was started for the sum-up");
}
#[cfg(unix)]
/// RC2-R3 (usability RC2 round 3, F7 / Finding 5: Claude wrote nothing for
/// 13.5 s on a second message, 22 s on the first; measured, about 9 s of
/// every Claude start is the owner's own SessionStart and UserPromptSubmit
/// hooks, which run when the process starts). While the council waits \u{2014}
/// from the moment it opens, so the first message too \u{2014} its Claude member
/// is started with `--input-format stream-json` and waits; the owner's
/// message goes in on its stdin (nothing is started at Enter), and once the
/// round is over the next one is made ready.
#[gpui::test]
fn rc2r3_a_claude_member_is_ready_before_the_question(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::rc2r3_a_claude_member_is_ready_before_the_question")
    else {
        return;
    };
    cx.executor().allow_parking();
    crate::codex_app_server::PREWARM.store(true, std::sync::atomic::Ordering::Release);
    let bins = home.join("bins");
    std::fs::create_dir_all(&bins).unwrap();
    let bin = bins.join("claude");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s ' \"$@\" >> {d}/claude-starts\nprintf '\\n@@@\\n' >> {d}/claude-starts\n\
                 case \"$*\" in *--input-format*) echo $$ >> {d}/claude-pids; read -r l; printf '%s\\n' \"$l\" >> {d}/claude-asked ;; esac\n\
                 printf '%s\\n' \
                 '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"Mirroring.\"}}]}}}}' \
                 '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"Mirroring.\"}}'\n",
                d = bins.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let asked = || std::fs::read_to_string(bins.join("claude-asked")).unwrap_or_default();
    let wait = |what: &str, within: std::time::Duration, done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + within;
        while !done() {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.work.agents.claude_bin = Some(bin.display().to_string());
        app.council_last_group = Some(vec![("agent:claude:sonnet".into(), "Claude Sonnet".into())]);
        app.set_mode(Mode::Council, cx);
    });
    wait("Claude made ready", ANSWER_DEADLINE, &|| {
        crate::claude_warm::warm_count() == 1 && starts(&bins).len() == 1
    });
    let s = starts(&bins);
    assert_eq!(s.len(), 1, "{s:?}");
    assert!(s[0].contains("--input-format stream-json"), "{s:?}");
    assert!(s[0].contains("--permission-mode plan"), "{s:?}");
    assert!(asked().is_empty(), "nothing asked yet");
    view.update(window, |app, cx| {
        app.composer.update(cx, |c, cx| {
            c.set_text("In one sentence: what is RAID 1?", cx)
        });
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "Claude answered", |app| {
        idle(app) && answer(app, "m1") == "Mirroring."
    });
    assert_eq!(
        starts(&bins)
            .iter()
            .filter(|s| s.contains("RAID 1"))
            .count(),
        0,
        "the Claude made ready answered; nothing was started at Enter"
    );
    assert!(
        starts(&bins)
            .iter()
            .all(|s| s.contains("--input-format stream-json")),
        "every start was one made ready ahead: {:?}",
        starts(&bins)
    );
    let line = asked();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    let content = v["message"]["content"].as_str().unwrap_or_default();
    assert!(
        content.contains("In one sentence: what is RAID 1?"),
        "{content}"
    );
    assert!(
        content.contains("You are in a read-only planning step."),
        "the planning text goes with the question: {content}"
    );
    wait("the next round made ready", ANSWER_DEADLINE, &|| {
        crate::claude_warm::warm_count() == 1 && starts(&bins).len() == 2
    });
    // A council archived or deleted ends its ready Claude.
    let conv = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    crate::codex_app_server::close_warm_for(&conv);
    wait("ended with its council", ENDS_WITHIN, &|| {
        crate::claude_warm::warm_count() == 0
    });
    // RC2-R3M12 (RC2-R39 review P3): the process itself ends, not only its
    // place on the list (it is ended off the caller's thread).
    wait("both ready ids recorded", ANSWER_DEADLINE, &|| {
        ready_pids(&bins).len() == 2
    });
    let pids = ready_pids(&bins);
    let last = *pids.last().expect("the ready Claude's process id");
    wait("its process ended", ENDS_WITHIN, &|| !alive(last));
}

/// RC2-R3: a memory service on this test's HOME that answers every lookup
/// with `body`.
fn noted_memory(home: &std::path::Path, body: String) -> crate::MemoryCfg {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let body = body.clone();
            std::thread::spawn(move || {
                let mut s = s;
                let mut buf = [0u8; 8192];
                let _ = s.read(&mut buf);
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    let foreman = home.join(".config/foreman");
    std::fs::create_dir_all(&foreman).unwrap();
    let token = foreman.join("v6-reader-token");
    std::fs::write(&token, "reader\n").unwrap();
    std::fs::write(
        foreman.join("memory-backend.conf"),
        format!(
            "FOREMAN_MEMORY_BACKEND=v65\nFOREMAN_MEMORY_V65_BASE={base}\n\
             FOREMAN_MEMORY_V65_READ_TOKEN={}\n",
            token.display()
        ),
    )
    .unwrap();
    crate::MemoryCfg {
        base_url: base,
        token: String::new(),
        min_priority: 90,
        enabled: true,
        recall: true,
        write: false,
    }
}

/// RC2-R3 (usability RC2 round 3, Finding 5: every member took about 22 s
/// to its first words on a first round). A council member that takes
/// Awili's memory (the in-app Gemma) reads every recalled word again each
/// round; each recalled note now reaches it cut near 400 bytes, as in Work
/// (RC2-R1F7), not 1,500.
#[gpui::test]
fn rc2r3_a_council_member_gets_short_recalled_notes(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::rc2r3_a_council_member_gets_short_recalled_notes")
    else {
        return;
    };
    cx.executor().allow_parking();
    let note = "n".repeat(1_400);
    let mem = noted_memory(
        &home,
        format!(
            r#"{{"mandatory_policy": [{{"id": "d0", "subject": "t", "authority": "owner_instruction", "object": "d0"}}], "items": [{{"id": "m1", "subject": "s", "authority": "inference", "object": "{note}"}}]}}"#
        ),
    );
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Move to Qwen."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer
            .update(cx, |c, cx| c.set_text("Gemma or Qwen?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "round 1", |app| {
        idle(app) && round_one(app).is_some_and(|r| r.turns.len() == 2)
    });
    let body = seen_a.lock().unwrap()[0].body.clone();
    let longest = body
        .split(|c: char| c != 'n')
        .map(str::len)
        .max()
        .unwrap_or(0);
    assert!(
        longest > 300 && longest <= crate::recall::INAPP_RECALL_NOTE_BYTES,
        "the note reaches Gemma cut near 400 bytes, not 1,500: {longest} n's"
    );
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn ready_claude(dir: &std::path::Path) -> String {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` in `dir` that, started ready (`--input-format`), records
/// its process id in `dir/claude-pids` and waits for its stdin line; each
/// start's argv is appended to `dir/claude-starts`.
fn ready_claude(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s ' \"$@\" >> {d}/claude-starts\nprintf '\\n@@@\\n' >> {d}/claude-starts\n\
             case \"$*\" in *--input-format*) echo $$ >> {d}/claude-pids; read -r l ;; esac\n\
             printf '%s\\n' \
             '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"Mirroring.\"}}]}}}}' \
             '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"Mirroring.\"}}'\n",
            d = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin.display().to_string()
}

/// The process ids `ready_claude` recorded.
fn ready_pids(dir: &std::path::Path) -> Vec<i32> {
    std::fs::read_to_string(dir.join("claude-pids"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn alive(pid: i32) -> bool {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// Whether process `pid` still exists (a reaped one does not).
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Wait (`within` at most) for `done`.
fn wait_for(what: &str, within: std::time::Duration, done: &dyn Fn() -> bool) {
    let deadline = std::time::Instant::now() + within;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// RC2-R3M4 (RC2-R3 review P3 on 82deea7a: Claude ticked in "+ Add or
/// remove" after the council opened was never made ready, so its first
/// question started cold): changing the members makes the new one ready at
/// once, as opening the council does; taking it off again ends its ready
/// process (the process itself, not only the list).
#[gpui::test]
fn rc2r3m4_a_member_added_in_the_picker_is_made_ready(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated("council_tests::rc2r3m4_a_member_added_in_the_picker_is_made_ready")
    else {
        return;
    };
    cx.executor().allow_parking();
    crate::codex_app_server::PREWARM.store(true, std::sync::atomic::Ordering::Release);
    let bins = home.join("bins");
    std::fs::create_dir_all(&bins).unwrap();
    let bin = ready_claude(&bins);
    let (a, _) = stub::spawn(|_| stub::Reply::text(&["Keep Gemma."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        two_member_council(app, &a, &a);
        app.work.agents.claude_bin = Some(bin.clone());
        app.council_last_group = Some(vec![("in-app".into(), "Gemma 4 26B".into())]);
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    assert_eq!(crate::claude_warm::warm_count(), 0, "no Claude yet");
    assert!(starts(&bins).is_empty());
    // Tick Claude and apply.
    view.update(window, |app, cx| {
        app.council_open_picker(cx);
        let conv = app.sidebar.active_id().to_string();
        app.councils.get_mut(&conv).unwrap().picker =
            Some(vec!["in-app".into(), "agent:claude:sonnet".into()]);
        app.council_apply_picker(cx);
        assert_eq!(app.active_council().unwrap().member_names().len(), 2);
    });
    wait_for("the added Claude made ready", ANSWER_DEADLINE, &|| {
        crate::claude_warm::warm_count() == 1 && ready_pids(&bins).len() == 1
    });
    let s = starts(&bins);
    assert_eq!(s.len(), 1, "{s:?}");
    assert!(s[0].contains("--input-format stream-json"), "{s:?}");
    let pid = ready_pids(&bins)[0];
    assert!(alive(pid));
    // Take Claude off: its ready process ends.
    view.update(window, |app, cx| {
        app.council_open_picker(cx);
        let conv = app.sidebar.active_id().to_string();
        app.councils.get_mut(&conv).unwrap().picker = Some(vec!["in-app".into()]);
        app.council_apply_picker(cx);
    });
    wait_for("the removed Claude's process ended", ENDS_WITHIN, &|| {
        crate::claude_warm::warm_count() == 0 && !alive(pid)
    });
    assert_eq!(starts(&bins).len(), 1, "nothing more was started");
}

/// RC2-R3M9 (RC2-R36 review P2): "Continue it" on a stopped answer, with no
/// memory outcome kept (as after a relaunch), waits for the memory lookup;
/// Stop while it waits keeps the time the answer had already taken \u{2014} it
/// was saved as about 0 ms.
#[gpui::test]
fn rc2r3m9_stop_while_a_continue_waits_keeps_its_time(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(home) = isolated("council_tests::rc2r3m9_stop_while_a_continue_waits_keeps_its_time")
    else {
        return;
    };
    cx.executor().allow_parking();
    let (a, _) = stub::spawn(|_| holds("Dedicated hardware controllers managed"));
    let (b, _) = stub::spawn(|_| stub::Reply::text(&["Move coding to Qwen."]));
    let (mem, hits, held) = held_memory(&home);
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let conv = view.update(window, |app, cx| {
        two_member_council(app, &a, &b);
        app.memory = mem;
        app.set_mode(Mode::Council, cx);
        app.composer.update(cx, |c, cx| {
            c.set_text("write 300 words on the history of RAID", cx)
        });
        app.submit_composer_draft(cx);
        app.sidebar.active_id().to_string()
    });
    settle(window, &view, "Gemma streaming, Hermes done", |app| {
        !answer(app, "m1").is_empty()
            && status_of(app, "m2") == Some(awili_council::TurnStatus::Answered)
    });
    std::thread::sleep(std::time::Duration::from_millis(1100));
    click(window, leak(crate::council_view::stop_selector(1, "m1")));
    settle(window, &view, "Gemma stopped", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Stopped)
    });
    let stopped_ms = view.read_with(window, |app, _| {
        turn_of(app, 1, "m1").unwrap().elapsed_ms.expect("its time")
    });
    assert!(stopped_ms >= 1000, "{stopped_ms}");
    // No outcome kept (a relaunch): the continue waits for the lookup.
    view.update(window, |app, _| {
        app.councils.get_mut(&conv).unwrap().gates.clear();
    });
    let before = hits.load(SeqCst);
    held.store(true, SeqCst);
    click(
        window,
        leak(crate::council_view::continue_selector(1, "m1")),
    );
    settle(window, &view, "the continue waits for the lookup", |app| {
        hits.load(SeqCst) > before && turn_of(app, 1, "m1").is_some_and(|t| t.status.is_running())
    });
    click(window, leak(crate::council_view::stop_selector(1, "m1")));
    settle(window, &view, "stopped while it waited", |app| {
        status_of(app, "m1") == Some(awili_council::TurnStatus::Stopped)
    });
    held.store(false, SeqCst);
    view.read_with(window, |app, _| {
        let t = turn_of(app, 1, "m1").unwrap();
        assert!(
            t.elapsed_ms.unwrap() >= stopped_ms,
            "kept {:?}, had {stopped_ms}",
            t.elapsed_ms
        );
        assert_eq!(t.text, "Dedicated hardware controllers managed");
    });
    // The saved row, once the write after the second stop has landed.
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let kept = wait_store("the stopped turn's time", |s| {
        let t = s.council_turns(&conv).ok()?;
        let t = t
            .into_iter()
            .find(|t| t.round == 1 && t.member_id == "m1" && t.status == "stopped")?;
        t.elapsed_ms
    });
    assert!(kept >= stopped_ms as i64, "saved {kept}, had {stopped_ms}");
}

#[cfg(not(unix))]
#[allow(unused_variables, dead_code)]
fn follow_up_claude(dir: &std::path::Path, reply: &str, write: bool) -> String {
    unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
}
#[cfg(unix)]
/// A fake `claude` in `dir` that plans "1. Write notes.txt." and, given full
/// access, answers `reply`; with `write`, it first runs a Write step on
/// notes.txt that its tool result reports done. Each start's argv is
/// appended to `dir/claude-starts`.
fn follow_up_claude(dir: &std::path::Path, reply: &str, write: bool) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    let marker = awili_code::PLAN_READY_MARKER;
    let steps = if write {
        "'{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"tu1\",\"name\":\"Write\",\"input\":{\"file_path\":\"/w/notes.txt\",\"content\":\"hi\"}}]}}' \
         '{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tu1\",\"is_error\":false,\"content\":\"ok\"}]}}' "
    } else {
        ""
    };
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nIFS= read -r l\nprintf '%s ' \"$@\" \"$l\" >> {d}/claude-starts\nprintf '\\n@@@\\n' >> {d}/claude-starts\n\
             case \"$*\" in\n\
             *--dangerously-skip-permissions*) printf '%s\\n' {steps}\
             '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{reply}\"}}]}}}}' \
             '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"{reply}\"}}' ;;\n\
             *) printf '%s\\n' \
             '{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"1. Write notes.txt.\\n{marker}\"}}]}}}}' \
             '{{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"1. Write notes.txt.\\n{marker}\"}}' ;;\n\
             esac\n",
            d = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin.display().to_string()
}

/// Approve Claude's plan in a one-member council with `bin`, wait for the
/// follow-up, and return its kept outcome.
fn approve_and_follow(
    window: &mut gpui::VisualTestContext,
    view: &gpui::Entity<AwiliApp>,
    bin: String,
) -> Option<awili_council::PlanOutcome> {
    seated(window, view, &[("agent:claude:sonnet", "Claude Sonnet")]);
    view.update(window, |app, cx| {
        app.work.agents.claude_bin = Some(bin);
        app.composer
            .update(cx, |c, cx| c.set_text("Write my notes down.", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, view, "the plan", idle);
    window.run_until_parked();
    click(
        window,
        leak(crate::council_view::plan_approve_selector(1, "m1")),
    );
    settle(window, view, "the follow-up answered", |app| {
        idle(app)
            && turn_of(app, 1, "m1").is_some_and(|t| {
                t.status == awili_council::TurnStatus::Answered && t.text.len() > 20
            })
    });
    view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        let id = page
            .council
            .round(1)
            .unwrap()
            .turn("m1")
            .unwrap()
            .id
            .clone();
        page.plan_outcomes.get(&id).copied()
    })
}

/// RC2-R3M13 (RC2-R3 review P2 on 74e18e33): an approved plan's follow-up
/// that ANSWERS "I couldn't edit the file", with no step of its own that
/// changed a file, is not kept as carried out: it answered, and whether the
/// change was made is not confirmed \u{2014} also in what the sum-up is told.
#[gpui::test]
fn rc2r3m13_a_follow_up_that_only_says_so_is_not_carried_out(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("council_tests::rc2r3m13_a_follow_up_that_only_says_so_is_not_carried_out")
    else {
        return;
    };
    cx.executor().allow_parking();
    let bins = home.join("bins");
    std::fs::create_dir_all(&bins).unwrap();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let bin = follow_up_claude(&bins, "I couldn't edit the file.", false);
    let outcome = approve_and_follow(window, &view, bin);
    assert_eq!(outcome, Some(awili_council::PlanOutcome::Answered));
    let conv = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    wait_store("the answered follow-up", |s| {
        let raw = s
            .get_app_state(&crate::council_view::plans_key(&conv))
            .ok()
            .flatten()?;
        crate::council_view::plan_outcomes_from_json(&raw)
            .values()
            .any(|o| *o == awili_council::PlanOutcome::Answered)
            .then_some(())
    });
    let told = view.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        let mut opts = awili_council::PlanOptions::new("Owner");
        opts.summarizer = Some("m1".into());
        opts.plan_outcomes = page
            .plan_outcomes
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let plan =
            awili_council::plan_round(&page.council, awili_council::RoundKind::SumUp, None, &opts);
        plan.seats[0].user_text("Owner")
    });
    assert!(
        told.contains("whether the change was made is not confirmed"),
        "{told}"
    );
    assert!(!told.contains("and it was carried out"), "{told}");
}

/// RC2-R3M13: a follow-up whose own Write step on the planned file finished
/// well (its tool result, not its words) is kept as carried out.
#[gpui::test]
fn rc2r3m13_a_follow_up_with_a_finished_write_is_carried_out(cx: &mut gpui::TestAppContext) {
    let Some(home) =
        isolated("council_tests::rc2r3m13_a_follow_up_with_a_finished_write_is_carried_out")
    else {
        return;
    };
    cx.executor().allow_parking();
    let bins = home.join("bins");
    std::fs::create_dir_all(&bins).unwrap();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let bin = follow_up_claude(&bins, "Done: notes.txt written.", true);
    let outcome = approve_and_follow(window, &view, bin);
    assert_eq!(outcome, Some(awili_council::PlanOutcome::Approved));
    assert_eq!(
        crate::council_exec::finished_change("\u{2713} Write /w/notes.txt").as_deref(),
        Some("/w/notes.txt")
    );
    assert_eq!(
        crate::council_exec::finished_change("Write /w/notes.txt"),
        None
    );
    assert_eq!(
        crate::council_exec::finished_change("\u{2713} Read /w/notes.txt"),
        None
    );
}

/// RC2-R53 (usability RC2 round 4, Major 1: Gemma's council card failed
/// "This turn needed more room than the model can hold (32768 tokens)" in 4
/// of its 6 turns): a long council — eight earlier rounds of long answers —
/// asked again: the in-app member's request is sent and answered, its
/// transcript cut (oldest answers first) to the room its window leaves once
/// its brief, tools, answer and step results are counted.
#[gpui::test]
fn rc2r53_a_long_council_stays_within_the_window(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r53_a_long_council_stays_within_the_window").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Fits."]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    seated(window, &view, &[("in-app", "Gemma 4 26B")]);
    view.update(window, |app, _| {
        app.work.engine.base_url = format!("{a}/v1");
        app.work.engine.model = "gemma-4-26b-a4b-it-qat-mlx-4bit".into();
        app.work.engine.window_tokens = 32_768;
    });
    view.update(window, |app, cx| {
        app.composer
            .update(cx, |c, cx| c.set_text("Start: ZFS or btrfs?", cx));
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "round 1", |app| {
        app.active_council()
            .and_then(|p| p.council.round(1))
            .is_some_and(|r| r.turns.iter().all(|t| !t.status.is_running()))
    });
    let long = "ZFS keeps checksums and repairs itself; btrfs grows more easily. ".repeat(100);
    view.update(window, |app, _| {
        for i in 0..8 {
            seed_round(
                app,
                awili_council::RoundKind::Ask,
                Some(&format!("Question {i}: say more about it.")),
                &[("m1", answered(), &long)],
            );
        }
    });
    view.update(window, |app, cx| {
        app.composer.update(cx, |c, cx| {
            c.set_text("One more sentence: which would you pick?", cx)
        });
        app.submit_composer_draft(cx);
    });
    settle(window, &view, "the new round's answer", |app| {
        app.active_council()
            .and_then(|p| p.council.rounds.last())
            .is_some_and(|r| r.turns.iter().all(|t| !t.status.is_running()))
    });
    view.read_with(window, |app, _| {
        let last = app.active_council().unwrap().council.rounds.last().unwrap();
        let t = &last.turns[0];
        assert!(
            matches!(t.status, awili_council::TurnStatus::Answered),
            "it answered, no \"more room than the model can hold\": {:?}",
            t.status
        );
        assert_eq!(t.text, "Fits.");
    });
    let seen = seen_a.lock().unwrap();
    let req: serde_json::Value = serde_json::from_str(&seen.last().unwrap().body).unwrap();
    let msgs = req["messages"].as_array().unwrap();
    let system: String = msgs
        .iter()
        .filter(|m| m["role"] == "system")
        .filter_map(|m| m["content"].as_str())
        .collect();
    let user = msgs
        .iter()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap();
    assert!(user.contains("One more sentence: which would you pick?"));
    assert!(
        user.contains(awili_council::OLDER_LEFT_OUT),
        "older answers cut"
    );
    assert!(
        user.chars().count() <= awili_seam::user_room_chars(32_768, &system) + 64,
        "{} characters",
        user.chars().count()
    );
}

/// RC2-R53 (usability RC2 round 4, Major 1: the card's sentence stopped at
/// "… check the steps list. Ask"): a long failure keeps its whole sentences
/// that fit, never a cut word.
#[test]
fn rc2r53_a_long_failure_is_never_cut_mid_sentence() {
    let raw = "This turn needed more room than the model can hold (32768 tokens), so it stopped \
               here. Steps it had already taken may have completed \u{2014} check the steps list. \
               Ask for a smaller step, or use a model with a larger window.";
    let (line, red) = crate::council_run::failure_sentence("Gemma 4 26B (Studio)", None, raw);
    assert!(!red);
    assert!(line.ends_with("check the steps list."), "{line}");
    assert!(!line.contains("Ask"), "{line}");
}

/// RC2-R510 (usability RC2 round 4, Minor: the quit check counted no council
/// member still answering): ⌘Q with a member answering asks first.
#[gpui::test]
fn rc2r510_a_council_answering_counts_as_a_run_in_progress(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r510_a_council_answering_counts_as_a_run_in_progress").is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    seated(window, &view, &FOUR[..2]);
    view.update(window, |app, cx| {
        assert_eq!(app.runs_in_progress(cx), 0);
        // One member answered, one still answering.
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("Which one?"),
            &[("m1", answered(), "ZFS."), ("m2", None, "Still writ")],
        );
        assert_eq!(app.runs_in_progress(cx), 1);
        let (msg, _) =
            crate::app_lifecycle::quit_prompt(app.runs_in_progress(cx)).expect("it asks first");
        assert_eq!(msg, "1 run in progress — quit anyway?");
    });
}

/// RC2-R5M3: a running seat's stream for `member` (round `round`) ends with
/// `error`, through the real finish path.
fn seat_fails(
    app: &mut AwiliApp,
    round: u32,
    member: &str,
    error: &str,
    cx: &mut gpui::Context<AwiliApp>,
) {
    let conv = app.sidebar.active_id().to_string();
    let turn_id = format!("t{round}{member}");
    app.councils.get_mut(&conv).unwrap().runs.insert(
        turn_id.clone(),
        crate::council_run::SeatRun {
            gen: 1,
            abort_tx: None,
            started: std::time::Instant::now(),
            first_token_ms: None,
            usage: None,
            prompt_hash: String::new(),
            plan_first: false,
            approved: false,
            changed: false,
            engine_tx: None,
            saved_at: None,
            carried_ms: 0,
            join_first: false,
        },
    );
    let key = crate::council_run::SeatKey {
        conv,
        round,
        turn_id,
        member: member.to_string(),
        gen: 1,
    };
    app.council_finish_seat(
        &key,
        crate::council_run::SeatEnd::Error(error.to_string()),
        cx,
    );
}

/// RC2-R5M3 (slice review befb867d, P2): a Codex member signed out. Its card
/// reads the failure as Codex's (the helper the member runs): it says how to
/// sign Codex in, for Codex's own "Not logged in" and for an OAuth failure
/// alike — never without the steps, never with Claude's.
#[gpui::test]
fn rc2r5m3_a_signed_out_codex_member_says_how_to_sign_codex_in(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc2r5m3_a_signed_out_codex_member_says_how_to_sign_codex_in")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1600.), px(900.)));
    seated(window, &view, &FOUR[..3]);
    let codex_steps = "Codex needs you to sign in again. Open Terminal and run codex login. \
                       Then press Retry.";
    for (round, raw) in [
        (1, "Not logged in"),
        (
            2,
            "Failed to authenticate: OAuth session expired and could not be refreshed",
        ),
    ] {
        view.update(window, |app, cx| {
            assert_eq!(
                seed_round(
                    app,
                    awili_council::RoundKind::Ask,
                    Some("Which one?"),
                    &[("m2", None, "")],
                ),
                round
            );
            seat_fails(app, round, "m2", raw, cx);
        });
        window.run_until_parked();
        view.read_with(window, |app, _| {
            match turn_of(app, round, "m2").map(|t| t.status) {
                Some(awili_council::TurnStatus::Failed {
                    plain, needs_owner, ..
                }) => {
                    assert_eq!(plain, codex_steps, "{raw}");
                    assert!(needs_owner, "the owner must act: {raw}");
                }
                s => panic!("{raw}: {s:?}"),
            }
        });
    }
}

// ---- Awili v2 session-only Council tracks ------------------------------------
fn track_click(window: &mut gpui::VisualTestContext, selector: String) {
    let bounds = window.debug_bounds(leak(selector)).unwrap();
    window.simulate_click(bounds.center(), gpui::Modifiers::default());
    window.run_until_parked();
}
fn track_setup(
    cx: &mut gpui::TestAppContext,
) -> (gpui::Entity<AwiliApp>, &mut gpui::VisualTestContext) {
    cx.executor().allow_parking();
    let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(1100.)));
    seated(window, &app, &FOUR[..3]);
    app.update(window, |app, cx| {
        app.persist = None;
        seed_round(
            app,
            awili_council::RoundKind::Ask,
            Some("Original whole-round question"),
            &[
                ("m1", answered(), "Full first answer"),
                ("m2", answered(), "Full second answer"),
                ("m3", answered(), "Full third answer"),
            ],
        );
        cx.notify();
    });
    window.run_until_parked();
    (app, window)
}

#[gpui::test]
fn v2_council_kit_actions_keep_the_shared_target_floor(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::v2_council_kit_actions_keep_the_shared_target_floor").is_none() {
        return;
    }
    let (app, window) = track_setup(cx);
    let previous_owner = app.read_with(window, |app, _| app.sidebar.active_id().to_string());
    app.update(window, |app, cx| {
        seat_fails(app, 1, "m2", "Not logged in", cx)
    });
    window.run_until_parked();
    for selector in [
        crate::council_view::RESPOND_SELECTOR,
        crate::council_view::SUM_UP_SELECTOR,
    ] {
        assert!(
            window.debug_bounds(selector).unwrap().size.height >= crate::theme::target(),
            "{selector} retains the shared KIT target floor",
        );
    }
    let auto = crate::council_view::auto_selector(0);
    assert!(
        window.debug_bounds(leak(auto)).unwrap().size.height >= crate::theme::target(),
        "automatic-round choices retain the shared KIT target floor",
    );
    let signin = leak(crate::council_view::signin_selector(1, "m2"));
    assert!(
        window.debug_bounds(signin).unwrap().size.height >= crate::theme::target(),
        "the act-now sign-in control retains the shared KIT target floor",
    );
    track_click(window, signin.to_string());
    app.read_with(window, |app, _| {
        let toast = app
            .toast
            .visible()
            .map(|s| s.to_string())
            .unwrap_or_default();
        assert!(toast.contains("codex login"), "sign-in callback: {toast}");
    });
    app.update_in(window, |app, window, cx| {
        app.dismiss_toast(cx);
        app.new_conversation(window, cx);
    });
    let next_owner = app.read_with(window, |app, _| app.sidebar.active_id().to_string());
    assert_ne!(next_owner, previous_owner);
    // Enter exercises the keyboard path of the just-unmounted focused chip.
    // It must not surface help captured for the previous conversation.
    window.simulate_keystrokes("enter");
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();
    app.read_with(window, |app, _| {
        assert!(
            app.toast.visible().is_none(),
            "stale sign-in help was shown"
        );
    });
}

#[gpui::test]
fn v2_tracks_focus_hide_preserve_whole_round_and_all_headers(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::v2_tracks_focus_hide_preserve_whole_round_and_all_headers")
        .is_none()
    {
        return;
    }
    let (app, window) = track_setup(cx);
    let before = app.read_with(window, |app, _| {
        format!("{:?}", app.active_council().unwrap().council)
    });
    let summary = app.read_with(window, |app, _| {
        awili_council::round_summary(&app.active_council().unwrap().council.rounds[0])
    });
    track_click(window, "council-track-focus-t1m1".into());
    assert!(window.debug_bounds("council-track-body-t1m1").is_some());
    for member in ["m2", "m3"] {
        assert!(window
            .debug_bounds(leak(format!("council-track-body-t1{member}")))
            .is_none());
        assert!(window.debug_bounds(sel_card(1, member)).is_some());
    }
    track_click(window, "council-track-focus-t1m1".into());
    for member in ["m1", "m2", "m3"] {
        track_click(window, format!("council-track-hide-t1{member}"));
        assert!(window
            .debug_bounds(leak(format!("council-track-body-t1{member}")))
            .is_none());
        let header = window
            .debug_bounds(leak(format!("ui-row-council-track-header-t1{member}")))
            .unwrap();
        assert!(
            header.size.height >= crate::theme::target(),
            "Collapsed header retains the target floor"
        );
    }
    let after = app.read_with(window, |app, _| {
        format!("{:?}", app.active_council().unwrap().council)
    });
    assert_eq!(
        before, after,
        "View toggles never edit requested members, rounds, replies or status"
    );
    assert_eq!(
        summary,
        app.read_with(window, |app, _| awili_council::round_summary(
            &app.active_council().unwrap().council.rounds[0]
        ))
    );
    // The just-hidden header survives its own body removal and owns Enter.
    window.simulate_keystrokes("enter");
    window.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    window.run_until_parked();
    assert!(
        window.debug_bounds("council-track-body-t1m3").is_some(),
        "Show remains keyboard-usable after Hide"
    );
}

#[gpui::test]
fn v2_tracks_state_is_owned_by_conversation_and_resets_on_restore(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::v2_tracks_state_is_owned_by_conversation_and_resets_on_restore")
        .is_none()
    {
        return;
    }
    let (app, window) = track_setup(cx);
    let first = app.read_with(window, |app, _| app.sidebar.active_id().to_string());
    track_click(window, "council-track-focus-t1m1".into());
    track_click(window, "council-track-hide-t1m2".into());
    app.update_in(window, |app, win, cx| app.new_conversation(win, cx));
    window.run_until_parked();
    let second = app.read_with(window, |app, _| app.sidebar.active_id().to_string());
    assert_ne!(first, second);
    app.update(window, |app, cx| {
        app.council_toggle_track_hidden(&first, "m3", cx);
        app.council_toggle_track_focus(&first, "m2", cx);
        assert!(
            app.councils[&second].focus_member.is_none()
                && app.councils[&second].hidden_members.is_empty(),
            "Stale same-member callbacks never retarget a new Council"
        );
        assert_eq!(app.councils[&first].focus_member.as_deref(), Some("m1"));
        assert!(!app.councils[&first].hidden_members.contains("m3"));
    });
    app.update_in(window, |app, win, cx| {
        app.select_conversation(&first, win, cx)
    });
    window.run_until_parked();
    assert!(window.debug_bounds("council-track-body-t1m1").is_some());
    assert!(window.debug_bounds("council-track-body-t1m2").is_none());
    let restored = crate::council_view::CouncilPage::with_group(
        &FOUR
            .iter()
            .map(|(r, l)| (r.to_string(), l.to_string()))
            .collect::<Vec<_>>(),
    );
    assert!(
        restored.focus_member.is_none() && restored.hidden_members.is_empty(),
        "View choices have no persisted group/settings lane"
    );
}

#[gpui::test]
fn v2_tracks_narrow_two_across_keep_controls_inside_full_records(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::v2_tracks_narrow_two_across_keep_controls_inside_full_records")
        .is_none()
    {
        return;
    }
    let (app, window) = track_setup(cx);
    app.update(window, |app, cx| {
        let page = app.councils.get_mut(app.sidebar.active_id()).unwrap();
        page.council.rounds[0].turns[0].label =
            "An extremely long recorded model name whose full text remains in its tooltip".into();
        page.council.rounds[0].turns[0].status = awili_council::TurnStatus::Running;
        cx.notify();
    });
    for scale in [0.9, 1.25, 1.5] {
        app.update_in(window, |app, win, cx| app.apply_ui_scale(scale, win, cx));
        window.simulate_resize(size(px(800.), px(900.)));
        window.run_until_parked();
        let a = window.debug_bounds(sel_card(1, "m1")).unwrap();
        let b = window.debug_bounds(sel_card(1, "m2")).unwrap();
        assert_eq!(a.top(), b.top());
        assert!(
            a.right() <= b.left() && b.right() <= px(800.),
            "Actual two-track cells fit at{scale}"
        );
        let stop = crate::council_view::stop_selector(1, "m1");
        for id in [
            "council-track-focus-t1m1",
            "council-track-hide-t1m1",
            stop.as_str(),
        ] {
            let control = window.debug_bounds(leak(id.to_string())).unwrap();
            assert!(
                control.left() >= a.left()
                    && control.right() <= a.right()
                    && control.top() >= a.top()
                    && control.bottom() <= a.bottom(),
                "{scale}:{id} must fit its actual track {control:?}/{a:?}"
            );
            assert!(
                control.size.height >= crate::theme::target()
                    && control.size.width >= crate::theme::target(),
                "Shared target floor at{scale}"
            );
        }
    }
    app.update(window, |app, cx| {
        app.councils
            .get_mut(app.sidebar.active_id())
            .unwrap()
            .council
            .rounds[0]
            .turns[0]
            .status = awili_council::TurnStatus::Answered;
        cx.notify();
    });
    app.update_in(window, |app, win, cx| app.apply_ui_scale(1., win, cx));
}

#[gpui::test]
fn v2_tracks_hidden_models_still_receive_send_mentions_respond_and_sum_up(
    cx: &mut gpui::TestAppContext,
) {
    if isolated(
        "council_tests::v2_tracks_hidden_models_still_receive_send_mentions_respond_and_sum_up",
    )
    .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (a, seen_a) = stub::spawn(|_| stub::Reply::text(&["Owned first model reply."]));
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["Owned second model reply."]));
    let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(1100.)));
    let owner = ask_two(window, &app, &a, &b, "Initial original Council request");
    settle(window, &app, "first complete", |app| {
        app.active_council()
            .is_some_and(|p| !p.council.is_running())
    });
    let (ta, tb) = app.read_with(window, |app, _| {
        let r = &app.councils[&owner].council.rounds[0];
        (
            r.turn("m1").unwrap().id.clone(),
            r.turn("m2").unwrap().id.clone(),
        )
    });
    track_click(window, format!("council-track-focus-{ta}"));
    track_click(window, format!("council-track-hide-{tb}"));
    app.update(window, |app, cx| {
        app.composer.update(cx, |c, cx| {
            c.set_text("A new request still asks both models", cx)
        });
        app.submit_composer_draft(cx);
    });
    settle(window, &app, "second whole round", |app| {
        app.active_council()
            .is_some_and(|p| p.council.rounds.len() == 2 && !p.council.is_running())
    });
    assert_eq!(seen_a.lock().unwrap().len(), 2);
    assert_eq!(
        seen_b.lock().unwrap().len(),
        2,
        "Hiding a model must never remove it from ordinary Send"
    );
    app.update(window, |app, cx| {
        app.composer.update(cx, |c, cx| {
            c.set_text("@Hermes Check the exact mentioned model", cx)
        });
        app.submit_composer_draft(cx);
    });
    settle(window, &app, "mentioned round", |app| {
        app.active_council()
            .is_some_and(|p| p.council.rounds.len() == 3 && !p.council.is_running())
    });
    assert_eq!(seen_a.lock().unwrap().len(), 2);
    assert_eq!(
        seen_b.lock().unwrap().len(),
        3,
        "Mentions retain their existing explicit subset"
    );
    // A one-member mention round still has the existing fewer-than-two refusal.
    app.update(window, |app, cx| app.council_answer_each_other(cx));
    window.run_until_parked();
    app.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert_eq!(page.council.rounds.len(), 3);
        assert!(page
            .note
            .as_deref()
            .is_some_and(|n| n.contains("Fewer than two")));
    });
    assert_eq!(seen_a.lock().unwrap().len(), 2);
    assert_eq!(seen_b.lock().unwrap().len(), 3);
    app.update(window, |app, cx| {
        app.composer.update(cx, |c, cx| {
            c.set_text("Ask both again while one track stays hidden", cx)
        });
        app.submit_composer_draft(cx);
    });
    settle(window, &app, "whole eligible set answered", idle);
    assert_eq!(seen_a.lock().unwrap().len(), 3);
    assert_eq!(seen_b.lock().unwrap().len(), 4);
    app.update(window, |app, cx| app.council_answer_each_other(cx));
    settle(window, &app, "Respond round", |app| {
        app.active_council()
            .is_some_and(|p| p.council.rounds.len() == 5 && !p.council.is_running())
    });
    assert_eq!(seen_a.lock().unwrap().len(), 4);
    assert_eq!(
        seen_b.lock().unwrap().len(),
        5,
        "Respond still asks the whole eligible set"
    );
    app.update(window, |app, cx| app.council_sum_up("m2", cx));
    settle(window, &app, "explicit SumUp", |app| {
        app.active_council()
            .is_some_and(|p| p.council.rounds.len() == 6 && !p.council.is_running())
    });
    assert_eq!(seen_a.lock().unwrap().len(), 4);
    assert_eq!(
        seen_b.lock().unwrap().len(),
        6,
        "Only the explicitly named summarizer receives SumUp"
    );
}

#[gpui::test]
fn v2_tracks_collapsed_pending_gates_remain_visible_and_recoverable(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::v2_tracks_collapsed_pending_gates_remain_visible_and_recoverable")
        .is_none()
    {
        return;
    }
    let (app, window) = track_setup(cx);
    app.update(window, |app, cx| {
        let page = app.councils.get_mut(app.sidebar.active_id()).unwrap();
        page.council.rounds[0].turns[1].status = awili_council::TurnStatus::Running;
        page.approvals.insert(
            "t1m2".into(),
            crate::work::ApprovalCard {
                approval_id: "track-owned-approval".into(),
                kind: "fixture".into(),
                prompt: "Read the selected folder".into(),
                risk: awili_gateway::events::ApprovalRisk::Privileged,
                delivery: Default::default(),
            },
        );
        page.plans
            .insert("t1m3".into(), "1. Write notes.txt.".into());
        cx.notify();
    });
    window.run_until_parked();
    track_click(window, "council-track-focus-t1m1".into());
    for member in ["m2", "m3"] {
        assert!(window
            .debug_bounds(leak(format!("council-track-body-t1{member}")))
            .is_none());
        assert!(window.debug_bounds(sel_card(1, member)).is_some());
        app.read_with(window, |app, _| {
            let page = app.active_council().unwrap();
            let t = page.council.round(1).unwrap().turn(member).unwrap();
            assert_eq!(
                crate::council_view::track_signal(page, t),
                (crate::theme::Tone::ActNow, "Needs you".into())
            );
        });
    }
    track_click(window, "council-track-focus-t1m2".into());
    assert!(window
        .debug_bounds(leak(crate::council_view::approval_selector(
            1, "m2", "once"
        )))
        .is_some());
    app.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert_eq!(page.approvals["t1m2"].approval_id, "track-owned-approval");
        assert!(page.plans.contains_key("t1m3"));
        assert!(page
            .council
            .round(1)
            .unwrap()
            .turn("m2")
            .unwrap()
            .status
            .is_running());
    });
    track_click(window, "council-track-focus-t1m3".into());
    assert!(window
        .debug_bounds(leak(crate::council_view::plan_approve_selector(1, "m3")))
        .is_some());
    app.read_with(window, |app, _| {
        let page = app.active_council().unwrap();
        assert!(
            page.approvals.contains_key("t1m2") && page.plans.contains_key("t1m3"),
            "Focusing exposes the owning gate without accepting or discarding it"
        );
    });
}

#[gpui::test]
fn v2_tracks_maximum_scale_stacks_readable_controls(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::v2_tracks_maximum_scale_stacks_readable_controls").is_none() {
        return;
    }
    let (app, window) = track_setup(cx);
    app.update(window, |app, cx| {
        app.councils
            .get_mut(app.sidebar.active_id())
            .unwrap()
            .council
            .rounds[0]
            .turns[0]
            .status = awili_council::TurnStatus::Running;
        cx.notify();
    });
    window.simulate_resize(size(px(800.), px(900.)));
    app.update_in(window, |app, win, cx| app.apply_ui_scale(2.5, win, cx));
    window.run_until_parked();
    let first = window.debug_bounds(sel_card(1, "m1")).unwrap();
    let second = window.debug_bounds(sel_card(1, "m2")).unwrap();
    assert!(
        second.top() >= first.bottom(),
        "Supported 800px/250% geometry stacks tracks for readable content"
    );
    for selector in ["council-track-focus-t1m1", "council-track-hide-t1m1"] {
        let control = window.debug_bounds(selector).unwrap();
        assert!(
            control.left() >= first.left() && control.right() <= first.right(),
            "Maximum-scale control fits its painted track"
        );
        assert!(
            control.size.width >= crate::theme::target()
                && control.size.height >= crate::theme::target()
        );
    }
    assert!(
        first.size.width >= crate::theme::target() + crate::theme::z(crate::theme::SP_20 * 2.),
        "Readable body retains room after its padding"
    );
    app.update(window, |app, cx| {
        app.councils
            .get_mut(app.sidebar.active_id())
            .unwrap()
            .council
            .rounds[0]
            .turns[0]
            .status = awili_council::TurnStatus::Answered;
        cx.notify();
    });
    app.update_in(window, |app, win, cx| app.apply_ui_scale(1., win, cx));
}

#[gpui::test]
fn v2_tracks_native_short_max_zoom_keeps_editor_and_reply_viewport(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::v2_tracks_native_short_max_zoom_keeps_editor_and_reply_viewport")
        .is_none()
    {
        return;
    }
    let (app, window) = track_setup(cx);
    app.update(window, |app, cx| {
        app.conversations
            .get_mut(app.sidebar.active_id())
            .unwrap()
            .title =
            "Native Council: compare the two owned model replies with a complete long title".into();
        cx.notify();
    });
    window.simulate_resize(size(px(900.), px(712.)));
    app.update_in(window, |app, win, cx| app.apply_ui_scale(2.5, win, cx));
    window.run_until_parked();
    let viewport = app.read_with(window, |app, _| {
        app.active_council().unwrap().scroll.bounds()
    });
    assert!(
        viewport.size.height >= crate::theme::target(),
        "Replies need a real usable viewport at native short geometry: {viewport:?}"
    );
    let frame = window
        .debug_bounds("ui-composer-frame-awili-composer")
        .unwrap();
    let editor = window
        .debug_bounds(crate::plan_gate::COMPOSER_ROW_SELECTOR)
        .unwrap();
    assert!(
        editor.size.width >= crate::theme::target() * 2.,
        "The actual editor retains room for words: {editor:?} / {frame:?}"
    );
    assert!(
        editor.size.height >= crate::theme::target(),
        "The actual editor retains a usable height: {editor:?}"
    );
    for selector in [
        "ui-icon-button-attach",
        "ui-icon-button-mic",
        "ui-icon-button-voice-mode",
        crate::SEND_OR_STOP_SELECTOR,
    ] {
        let control = window.debug_bounds(selector).unwrap();
        assert!(
            control.left() >= frame.left() && control.right() <= frame.right(),
            "{selector} stays in its frame: {control:?} / {frame:?}"
        );
    }
}

#[gpui::test]
fn v2_shared_composer_button_stops_the_active_council(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::v2_shared_composer_button_stops_the_active_council").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let held = |_: &str| {
        let mut reply = stub::Reply::text(&["Owned partial answer."]);
        reply.hold = true;
        reply
    };
    let (a, seen_a) = stub::spawn(held);
    let (b, seen_b) = stub::spawn(held);
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(900.), px(712.)));
    ask_two(window, &view, &a, &b, "Compare both owned replies");
    settle(window, &view, "both real member requests are held", |app| {
        app.active_council().is_some_and(|p| p.answering() == 2)
            && seen_a.lock().unwrap().len() == 1
            && seen_b.lock().unwrap().len() == 1
    });
    let button = window
        .debug_bounds(crate::SEND_OR_STOP_SELECTOR)
        .expect("shared Send/Stop is rendered");
    window.simulate_click(button.center(), gpui::Modifiers::default());
    settle(
        window,
        &view,
        "pointer Stop ends both held members",
        |app| {
            app.active_council()
                .is_some_and(|p| p.answering() == 0 && !p.council.is_running())
        },
    );
    view.read_with(window, |app, _| {
        let round = app.active_council().unwrap().council.round(1).unwrap();
        assert_eq!(round.turns.len(), 2);
        assert!(round
            .turns
            .iter()
            .all(|t| t.status == awili_council::TurnStatus::Stopped));
    });
    assert_eq!(seen_a.lock().unwrap().len(), 1);
    assert_eq!(seen_b.lock().unwrap().len(), 1);
}

#[gpui::test]
fn v2_shared_composer_keeps_enter_queue_and_other_page_owner(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::v2_shared_composer_keeps_enter_queue_and_other_page_owner")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let held = |_: &str| {
        let mut reply = stub::Reply::text(&["Owned partial answer."]);
        reply.hold = true;
        reply
    };
    let (a, seen_a) = stub::spawn(held);
    let (b, seen_b) = stub::spawn(held);
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(900.), px(712.)));
    let owner = ask_two(window, &view, &a, &b, "Ask both members");
    settle(window, &view, "both owned requests started", |app| {
        app.active_council().is_some_and(|p| p.answering() == 2)
            && seen_a.lock().unwrap().len() == 1
            && seen_b.lock().unwrap().len() == 1
    });
    view.update_in(window, |app, win, cx| app.focus_composer(win, cx));
    window.simulate_input("Next question waits its turn");
    window.simulate_keystrokes("enter");
    window.run_until_parked();
    view.read_with(window, |app, cx| {
        assert_eq!(
            app.composer.read(cx).text(),
            "",
            "Enter still takes the queued text"
        );
        let page = app.active_council().unwrap();
        assert_eq!(
            page.queued
                .iter()
                .map(|text| text.trim())
                .collect::<Vec<_>>(),
            ["Next question waits its turn"]
        );
        assert_eq!(page.answering(), 2, "Queuing does not stop either member");
    });
    assert_eq!(
        seen_a.lock().unwrap().len(),
        1,
        "queued request has not started"
    );
    assert_eq!(seen_b.lock().unwrap().len(), 1);
    view.update(window, |app, cx| {
        app.set_mode(Mode::Work, cx);
        app.composer
            .update(cx, |composer, cx| composer.set_text("", cx));
        cx.notify();
    });
    window.run_until_parked();
    let button = window.debug_bounds(crate::SEND_OR_STOP_SELECTOR).unwrap();
    window.simulate_click(button.center(), gpui::Modifiers::default());
    window.run_until_parked();
    view.update(window, |app, _| {
        let hidden = app.councils.get_mut(&owner).unwrap();
        assert_eq!(
            hidden.answering(),
            2,
            "Other page's Send cannot stop the hidden Council"
        );
        // Fixture teardown only: queued delivery is covered by the unchanged
        // a_message_typed_while_answering_waits_its_turn control.
        hidden.queued.clear();
        hidden.stop_all();
    });
}

// ---- RC3-S1a: Council smoke fixes ------------------------------------------------

/// RC3-S1a: a track header never claims a location it did not record.
#[test]
fn rc3_s1a_track_subtitle_has_no_location_placeholder() {
    let line = crate::council_view::track_subtitle("agent:codex", "Done");
    assert!(!line.contains("Location"), "{line}");
    assert!(line.ends_with("\u{00B7} Done"), "{line}");
}

/// RC3-S1a: the intro and the "Goes to" line never overlap, narrow or wide.
#[gpui::test]
fn rc3_s1a_empty_state_paragraphs_do_not_overlap(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc3_s1a_empty_state_paragraphs_do_not_overlap").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        quiet(app);
        app.council_last_group = Some(vec![
            ("agent:codex".into(), "Codex".into()),
            ("in-app".into(), "Gemma 4 26B".into()),
            ("agent:claude".into(), "Claude Sonnet".into()),
        ]);
        app.set_mode(Mode::Council, cx);
    });
    for (width, height) in [
        (360., 900.),
        (360., 320.),
        (520., 240.),
        (900., 300.),
        (1400., 900.),
    ] {
        window.simulate_resize(size(px(width), px(height)));
        window.run_until_parked();
        let intro = window
            .debug_bounds(crate::council_view::EMPTY_INTRO_SELECTOR)
            .expect("intro");
        let goes = window
            .debug_bounds(crate::council_view::EMPTY_GOES_TO_SELECTOR)
            .expect("goes to");
        assert!(
            intro.bottom() <= goes.top(),
            "{width}x{height}: intro {intro:?} overlaps goes-to {goes:?}"
        );
    }
}

/// RC3-S1a: the Council page deletes its council with an inline confirm, and
/// then shows a new empty council; Keep leaves everything alone.
#[gpui::test]
fn rc3_s1a_council_page_delete_confirms_then_shows_a_new_council(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc3_s1a_council_page_delete_confirms_then_shows_a_new_council")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    let old = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    let click = |window: &mut gpui::VisualTestContext, name: &'static str| {
        let b = window
            .debug_bounds(name)
            .unwrap_or_else(|| panic!("{name}"));
        window.simulate_click(b.center(), gpui::Modifiers::default());
        window.run_until_parked();
    };

    click(window, crate::council_view::DELETE_SELECTOR);
    view.read_with(window, |app, _| {
        assert_eq!(app.pending_delete.as_deref(), Some(old.as_str()));
    });
    click(window, crate::council_view::DELETE_CANCEL_SELECTOR);
    view.read_with(window, |app, _| {
        assert!(app.pending_delete.is_none());
        assert_eq!(app.sidebar.active_id(), old);
        assert!(app.conversations.get(&old).is_some(), "Keep keeps it");
    });

    click(window, crate::council_view::DELETE_SELECTOR);
    click(window, crate::council_view::DELETE_CONFIRM_SELECTOR);
    view.read_with(window, |app, _| {
        assert!(app.conversations.get(&old).is_none(), "deleted");
        assert!(!app.councils.contains_key(&old));
        let now = app.sidebar.active_id();
        assert_ne!(now, old);
        let meta = app.conversations.get(now).expect("a new council");
        assert_eq!(meta.kind, ConvKind::Council);
        assert!(app.councils[now].council.rounds.is_empty(), "empty");
        assert_eq!(app.mode, Mode::Council);
    });
    assert!(window
        .debug_bounds(crate::council_view::EMPTY_STATE_SELECTOR)
        .is_some());
}

/// RC3-S1a: deleting the hovered conversation clears the sidebar hover, so no
/// tooltip names a deleted conversation.
#[gpui::test]
fn rc3_s1a_deleting_the_hovered_row_clears_the_hover(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc3_s1a_deleting_the_hovered_row_clears_the_hover").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    let id = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    view.update(window, |app, _| app.sidebar.hovered_id = Some(id.clone()));
    view.update_in(window, |app, window, cx| {
        app.delete_conversation(&id, window, cx)
    });
    view.read_with(window, |app, _| {
        assert!(
            app.sidebar.hovered_id.is_none(),
            "{:?}",
            app.sidebar.hovered_id
        );
    });
    // Another row's hover is left alone.
    let other = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    view.update(window, |app, _| {
        app.sidebar.hovered_id = Some("some-other-row".into())
    });
    view.update_in(window, |app, window, cx| {
        app.delete_conversation(&other, window, cx)
    });
    view.read_with(window, |app, _| {
        assert_eq!(app.sidebar.hovered_id.as_deref(), Some("some-other-row"));
    });
}

/// A page-level Delete on the active council, armed and confirmed by click.
fn rc3_s1a_click_delete_and_confirm(window: &mut gpui::VisualTestContext) {
    for name in [
        crate::council_view::DELETE_SELECTOR,
        crate::council_view::DELETE_CONFIRM_SELECTOR,
    ] {
        let b = window
            .debug_bounds(name)
            .unwrap_or_else(|| panic!("{name}"));
        window.simulate_click(b.center(), gpui::Modifiers::default());
        window.run_until_parked();
    }
}

fn rc3_s1a_council_count(app: &AwiliApp) -> usize {
    app.conversations
        .active()
        .iter()
        .filter(|c| c.kind == ConvKind::Council)
        .count()
}

/// RC3-S1a (review HIGH 1): a delete the app refuses (a held local-model
/// message) keeps the council, shows the reason, persists nothing and opens no
/// new council.
#[gpui::test]
fn rc3_s1a_refused_council_delete_keeps_the_council(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc3_s1a_refused_council_delete_keeps_the_council").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        quiet(app);
        app.persist = Some(crate::persistence::Persistence {
            tx,
            failed: Default::default(),
            recovery: Default::default(),
        });
        app.set_mode(Mode::Council, cx);
        let id = app.sidebar.active_id().to_string();
        app.work.local.down = Some(crate::local_model_health::LocalDown {
            reason: crate::local_model_health::DownReason::NotConfigured,
            text: Some("a held message".into()),
            conv_id: Some(id),
        });
    });
    window.run_until_parked();
    let (old, before) = view.read_with(window, |app, _| {
        (
            app.sidebar.active_id().to_string(),
            rc3_s1a_council_count(app),
        )
    });
    while rx.try_recv().is_ok() {}
    rc3_s1a_click_delete_and_confirm(window);
    view.read_with(window, |app, _| {
        assert!(app.conversations.get(&old).is_some(), "still there");
        assert_eq!(app.sidebar.active_id(), old, "still on it");
        assert_eq!(rc3_s1a_council_count(app), before, "no new council");
        assert!(app.councils.contains_key(&old));
        let toast = app.toast.msg.as_ref().expect("the reason is shown");
        assert!(toast.contains("Retry or edit the message"), "{toast}");
    });
    assert!(
        !rx.try_iter()
            .any(|c| matches!(c, crate::persistence::PersistCmd::DeleteConversation { .. })),
        "a refused delete persists nothing"
    );
}

/// RC3-S1a (review HIGH 1): a delete that goes through is persisted and lands
/// on exactly one new empty council.
#[gpui::test]
fn rc3_s1a_council_delete_persists_and_lands_on_one_new_council(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc3_s1a_council_delete_persists_and_lands_on_one_new_council")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let (tx, rx) = std::sync::mpsc::channel();
    view.update(window, |app, cx| {
        quiet(app);
        app.persist = Some(crate::persistence::Persistence {
            tx,
            failed: Default::default(),
            recovery: Default::default(),
        });
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    let current = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    window.run_until_parked();
    let before = view.read_with(window, |app, _| rc3_s1a_council_count(app));
    while rx.try_recv().is_ok() {}
    rc3_s1a_click_delete_and_confirm(window);
    view.read_with(window, |app, _| {
        assert!(app.conversations.get(&current).is_none(), "deleted");
        let now = app.sidebar.active_id();
        assert_ne!(now, current);
        assert!(app.councils[now].council.rounds.is_empty(), "empty");
        assert_eq!(rc3_s1a_council_count(app), before, "one in, one out");
    });
    let deleted: Vec<String> = rx
        .try_iter()
        .filter_map(|c| match c {
            crate::persistence::PersistCmd::DeleteConversation { id } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(deleted, [current]);
}

/// RC3-S1a (review MEDIUM): a long member list at phone width keeps the intro
/// above the "Goes to" line. (A guard for the layout; the original overlap did
/// not reproduce in the test renderer.)
#[gpui::test]
fn rc3_s1a_long_member_list_stays_below_the_intro(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc3_s1a_long_member_list_stays_below_the_intro").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(360.), px(700.)));
    view.update(window, |app, cx| {
        quiet(app);
        app.council_last_group = Some(
            (0..8)
                .map(|i| (format!("agent:m{i}"), format!("Long Model Name Number {i}")))
                .collect(),
        );
        app.set_mode(Mode::Council, cx);
    });
    window.run_until_parked();
    let intro = window
        .debug_bounds(crate::council_view::EMPTY_INTRO_SELECTOR)
        .expect("intro");
    let goes = window
        .debug_bounds(crate::council_view::EMPTY_GOES_TO_SELECTOR)
        .expect("goes to");
    assert!(intro.bottom() <= goes.top(), "{intro:?} vs {goes:?}");
}

/// RC3-S1a (review HIGH 2): with no label and no model set, the in-app prompt
/// names no model at all (never "You are no model set").
#[gpui::test]
fn rc3_s1a_blank_model_config_names_no_model(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::rc3_s1a_blank_model_config_names_no_model").is_none() {
        return;
    }
    let (view, window) = cx.add_window_view(|_w, cx| AwiliApp::new(cx));
    view.update(window, |app, _| {
        app.work.engine.label = String::new();
        app.work.engine.model = "  ".into();
        let s = app.work_system_for_engine(None, None).unwrap_or_default();
        assert!(!s.contains("no model set"), "{s}");
        assert!(!s.contains("You are "), "{s}");
        app.work.engine.model = "org/gemma-4".into();
        let s = app.work_system_for_engine(None, None).unwrap_or_default();
        assert!(s.contains("You are gemma-4,"), "{s}");
    });
}

/// SAFETY (owner rule: Awili never makes a Lemonade host load a model): a
/// council member on a Lemonade host whose model is not resident is asked
/// nothing -- no chat request -- and its card says why; the other member
/// answers as usual.
#[gpui::test]
fn a_council_member_on_a_non_resident_lemonade_model_is_sent_nothing(
    cx: &mut gpui::TestAppContext,
) {
    if isolated("council_tests::a_council_member_on_a_non_resident_lemonade_model_is_sent_nothing")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (a, seen_a) = crate::strip::tests::serve(
        vec![
            (
                "/api/v1/health",
                200,
                crate::strip::tests::health_body(&["kokoro-v1"]),
            ),
            ("/v1/models", 200, r#"{"data":[]}"#.to_string()),
        ],
        false,
    );
    let (b, seen_b) = stub::spawn(|_| stub::Reply::text(&["fine"]));
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1400.), px(900.)));
    let _conv = ask_two(window, &view, &a, &b, "Anyone loaded?");
    settle(window, &view, "both cards ended", |app| {
        round_one(app)
            .is_some_and(|r| r.turns.len() == 2 && r.turns.iter().all(|t| !t.status.is_running()))
    });
    for line in seen_a.lock().unwrap().iter() {
        assert_eq!(
            line, "GET /api/v1/health HTTP/1.1",
            "only the health read: {line}"
        );
    }
    assert_eq!(
        seen_b.lock().unwrap().len(),
        1,
        "the other member was asked"
    );
    view.read_with(window, |app, _| {
        let r = round_one(app).unwrap();
        let gemma = r
            .turns
            .iter()
            .find(|t| t.route == "in-app")
            .expect("the Lemonade member's card");
        let said = format!("{} {:?}", gemma.text, gemma.status);
        assert!(said.contains("isn\u{2019}t loaded"), "{said}");
    });
}

#[gpui::test]
fn refused_ambiguous_queued_message_stays_with_hidden_council(cx: &mut gpui::TestAppContext) {
    if isolated("council_tests::refused_ambiguous_queued_message_stays_with_hidden_council")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
    seated(
        window,
        &view,
        &[
            ("agent:claude:sonnet", "Claude Sonnet"),
            ("agent:claude:opus", "Claude Opus"),
        ],
    );
    view.update(window, |app, cx| {
        let owner = app.sidebar.active_id().to_string();
        app.councils
            .get_mut(&owner)
            .unwrap()
            .queued
            .push_back("@Claude please explain".into());
        app.set_mode(Mode::Work, cx);
        app.council_after_seat(&owner, cx);
        let page = &app.councils[&owner];
        assert_eq!(
            page.queued.front().map(String::as_str),
            Some("@Claude please explain")
        );
        assert!(page.note.is_some());
    });
}
