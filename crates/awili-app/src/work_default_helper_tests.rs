//! R2b proof tests: the Default Work helper is what a new Work conversation
//! uses (RC1 round 2, Major 2: Settings showed "✓ ZCode (GLM)" while a new
//! conversation, also after relaunch, ran on Claude), "On this Mac" is a
//! choice, and Reset puts Work on this Mac. Each test runs the real
//! `AwiliApp` in a GPUI test window (private HOME subprocess) and drives the
//! chip pick, "+ New conversation" and the Settings rows a user clicks.

use super::*;
use crate::settings_catalog as cat;
use awili_code::CodeEngine;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

const CHILD: &str = "AWILI_R2B_TEST_HOME";

/// Run `test` in a subprocess with a private HOME. `true` inside the child.
fn in_isolated_child(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, home.path())
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

/// Write the config file Awili will load (call before `work_window`).
fn with_config(text: &str) {
    let path = awili_config::Config::config_paths()[0].clone();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
}

/// The app in Work mode, in a test window.
fn work_window(cx: &mut TestAppContext) -> (Entity<AwiliApp>, &mut VisualTestContext) {
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    view.update(window, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Work, cx);
    });
    window.run_until_parked();
    (view, window)
}

fn agent(engine: CodeEngine) -> work::WorkExecutor {
    work::WorkExecutor::Agent {
        engine,
        model: None,
    }
}

fn executor(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> work::WorkExecutor {
    view.update(window, |app, _| app.work.executor.clone())
}

/// The chip pick a user makes (the executor menu row).
fn pick(view: &Entity<AwiliApp>, window: &mut VisualTestContext, exec: work::WorkExecutor) {
    view.update(window, |app, cx| app.pick_work_executor(exec, cx));
    window.run_until_parked();
}

/// "+ New conversation" (the button and ⌘N run this).
fn new_conversation(view: &Entity<AwiliApp>, window: &mut VisualTestContext) {
    view.update_in(window, |app, window, cx| app.new_conversation(window, cx));
    window.run_until_parked();
}

fn sel(selector: &str) -> &'static str {
    Box::leak(selector.to_string().into_boxed_str())
}

fn click(window: &mut VisualTestContext, selector: &str) {
    let b = window
        .debug_bounds(sel(selector))
        .unwrap_or_else(|| panic!("{selector} is on screen"));
    window.simulate_click(b.center(), Modifiers::default());
    window.run_until_parked();
}

fn ix(id: &str) -> usize {
    cat::ITEMS.iter().position(|i| i.id == id).unwrap()
}

/// The persisted executor (the next launch's seed) reaches `want`.
fn remembered(want: &str) {
    let store = awili_persist::Store::open_default().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let saved = store.get_app_state(persistence::WORK_EXECUTOR_KEY).unwrap();
        if saved.as_deref() == Some(want) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "remembered {saved:?}, want {want}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The configured helper wins for new project threads. With no configured
/// helper, the last thread pick remains the fallback.
#[gpui::test]
fn r2b_new_work_conversation_starts_on_the_default_helper(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::default_helper_tests::r2b_new_work_conversation_starts_on_the_default_helper",
    ) {
        return;
    }
    with_config("[work]\ndefault_executor = \"agent:glm\"\n");
    let (view, window) = work_window(cx);
    new_conversation(&view, window);
    assert_eq!(
        executor(&view, window),
        agent(CodeEngine::Glm),
        "with no choice made, a new Work conversation starts on the Default Work helper"
    );
    pick(&view, window, agent(CodeEngine::Claude));
    assert_eq!(executor(&view, window), agent(CodeEngine::Claude));
    new_conversation(&view, window);
    assert_eq!(
        executor(&view, window),
        agent(CodeEngine::Glm),
        "the explicit configured preference wins over the remembered pick"
    );
    assert_eq!(
        view.update(window, |app, _| app.work.last_pick.clone()),
        Some(agent(CodeEngine::Claude)),
        "using a configured default does not rewrite the remembered pick"
    );
    remembered("agent:claude");

    // Pick Claude, then choose Codex in Settings. The current thread remains
    // on Claude and the next project thread starts on Codex.
    pick(&view, window, agent(CodeEngine::Claude));
    let current = executor(&view, window);
    let last_pick = view.update(window, |app, _| app.work.last_pick.clone());
    view.update(window, |app, cx| app.open_settings(cx));
    window.run_until_parked();
    click(window, "sx-nav-2");
    click(
        window,
        &format!("sx-choice-{}", ix("work.default_executor") * 16 + 1),
    );
    assert_eq!(executor(&view, window), current);
    assert_eq!(
        view.update(window, |app, _| app.work.last_pick.clone()),
        last_pick
    );
    new_conversation(&view, window);
    assert_eq!(
        executor(&view, window),
        agent(CodeEngine::Codex),
        "a helper chosen in Settings applies without a restart"
    );
}

/// With no Default Work helper chosen, a new conversation keeps the last
/// pick (nothing is forced on the user).
#[gpui::test]
fn r2b_no_default_helper_keeps_the_last_pick(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::default_helper_tests::r2b_no_default_helper_keeps_the_last_pick",
    ) {
        return;
    }
    let (view, window) = work_window(cx);
    pick(&view, window, agent(CodeEngine::Codex));
    new_conversation(&view, window);
    assert_eq!(executor(&view, window), agent(CodeEngine::Codex));
    remembered("agent:codex");
}

/// A Workshop folder choice seeds the new thread from the configured default
/// without rewriting the persisted helper chosen manually in another thread.
#[gpui::test]
fn r2b_workshop_default_seed_preserves_manual_helper_key(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::default_helper_tests::r2b_workshop_default_seed_preserves_manual_helper_key",
    ) {
        return;
    }
    with_config("[work]\ndefault_executor = \"in-app\"\n");
    let (view, window) = work_window(cx);
    pick(&view, window, agent(CodeEngine::Claude));
    remembered("agent:claude");

    view.update(window, |app, cx| {
        app.new_workshop_from_default(work::WorkExecutor::InApp, cx);
    });
    window.run_until_parked();
    assert_eq!(executor(&view, window), work::WorkExecutor::InApp);
    assert_eq!(
        view.update(window, |app, _| app.work.last_pick.clone()),
        Some(agent(CodeEngine::Claude)),
        "default seeding must retain the manual fallback"
    );
    remembered("agent:claude");
    assert!(
        std::fs::read_to_string(awili_config::Config::config_paths()[0].clone())
            .unwrap()
            .contains("default_executor = \"in-app\""),
        "default seeding must preserve the configured preference"
    );

    pick(&view, window, work::WorkExecutor::Gateway);
    assert_eq!(
        view.update(window, |app, _| app.work.last_pick.clone()),
        Some(work::WorkExecutor::Gateway),
        "an explicit helper pick still updates the manual fallback"
    );
    remembered("gateway");
    assert!(
        std::fs::read_to_string(awili_config::Config::config_paths()[0].clone())
            .unwrap()
            .contains("default_executor = \"in-app\""),
        "manual picking must not rewrite the configured starting preference"
    );
}

/// Reusing a blank thread creates a new logical owner. A stale per-thread
/// chip must not survive when neither the configured default nor last pick
/// supplies a helper.
#[gpui::test]
fn r2b_reused_blank_without_starting_helper_drops_stale_chip(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::default_helper_tests::r2b_reused_blank_without_starting_helper_drops_stale_chip",
    ) {
        return;
    }
    with_config("");
    let (view, window) = work_window(cx);
    let id = view.update(window, |app, _| {
        let id = app.create_conversation(crate::ConvKind::Work);
        app.work.last_pick = None;
        app.work.default_helper = None;
        app.work
            .chip_picked
            .insert(id.clone(), agent(CodeEngine::Codex));
        let mut excluded = std::collections::HashSet::new();
        excluded.insert(app.sidebar.active_id().to_string());
        let (reused, was_reused) =
            app.fresh_conversation_except(crate::ConvKind::Work, None, &excluded);
        assert!(was_reused);
        assert_eq!(reused, id);
        id
    });
    assert_eq!(
        view.read_with(window, |app, _| app.work.chip_picked.get(&id).cloned()),
        None,
        "the stale helper must not carry into the new logical conversation"
    );
}

/// "On this Mac (local model)" is a starting preference. Saving or resetting
/// it leaves the active thread and last pick intact.
#[gpui::test]
fn r2b_on_this_mac_is_a_choice_and_reset_goes_back_to_it(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::default_helper_tests::r2b_on_this_mac_is_a_choice_and_reset_goes_back_to_it",
    ) {
        return;
    }
    with_config("[work]\ndefault_executor = \"agent:claude\"\n");
    let (view, window) = work_window(cx);
    let before = executor(&view, window);
    let last_pick = view.update(window, |app, _| app.work.last_pick.clone());
    view.update(window, |app, cx| app.open_settings(cx));
    window.run_until_parked();
    click(window, "sx-nav-2");
    let item = cat::find("work.default_executor").unwrap();
    let cat::Control::Choice(options) = item.control else {
        panic!("a choice")
    };
    let mac = options
        .iter()
        .position(|(stored, _)| *stored == "in-app")
        .expect("an On this Mac choice");
    assert_eq!(options[mac].1, "On this Mac (local model)");
    click(
        window,
        &format!("sx-choice-{}", ix("work.default_executor") * 16 + mac),
    );
    let text = view.update(window, |app, _| {
        std::fs::read_to_string(app.settings.path()).unwrap_or_default()
    });
    assert!(text.contains("default_executor = \"in-app\""), "{text}");
    assert_eq!(executor(&view, window), before);
    assert_eq!(
        view.update(window, |app, _| app.work.last_pick.clone()),
        last_pick
    );
    assert_eq!(
        view.update(window, |app, _| app.work.default_helper.clone())
            .as_deref(),
        Some("in-app")
    );

    click(
        window,
        &format!("sx-choice-{}", ix("work.default_executor") * 16 + 1),
    );
    assert_eq!(executor(&view, window), before);
    click(window, &format!("sx-reset-{}", ix("work.default_executor")));
    assert_eq!(
        executor(&view, window),
        before,
        "Reset leaves the active thread's helper alone"
    );
    assert_eq!(
        view.update(window, |app, _| app.work.last_pick.clone()),
        last_pick
    );
    assert_eq!(
        view.update(window, |app, _| app.work.default_helper.clone()),
        None
    );
}

fn toast(view: &Entity<AwiliApp>, window: &mut VisualTestContext) -> Option<String> {
    view.update(window, |app, _| app.toast.visible().map(|s| s.to_string()))
}

/// RC2-UX (S-N3, RC1-N item 2): only the helper picker said that GLM,
/// DeepSeek, a local Build or Hermes cannot ask first. Now a new Work
/// conversation (set to ask first) that starts on such a default helper,
/// and a Settings choice of one, say so once in plain words; a helper that
/// asks says nothing, and the picker's own notice is not doubled.
#[gpui::test]
fn rc2ux_a_helper_that_cannot_ask_says_so_off_the_picker_too(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::default_helper_tests::rc2ux_a_helper_that_cannot_ask_says_so_off_the_picker_too",
    ) {
        return;
    }
    with_config("[work]\ndefault_executor = \"agent:glm\"\n");
    let (view, window) = work_window(cx);
    // RC2-R1F2: a chip PICK now carries to the next new conversation, so
    // the chip is put on Claude without one (as opening a conversation that
    // ran on Claude does); the new conversation starts on the default.
    view.update(window, |app, _| {
        app.set_work_executor_persisted(agent(CodeEngine::Claude))
    });
    view.update(window, |app, cx| app.dismiss_toast(cx));
    new_conversation(&view, window);
    assert_eq!(executor(&view, window), agent(CodeEngine::Glm));
    let glm = crate::ask_mode::no_ask_toast(
        crate::ask_mode::work_support(&agent(CodeEngine::Glm), awili_config::EngineKind::default()),
        &agent(CodeEngine::Glm).label(),
    );
    assert_eq!(
        toast(&view, window).as_deref(),
        Some(glm.as_str()),
        "the new conversation says so"
    );
    assert!(glm.contains("run without asking"), "{glm}");

    // The picker: its own notice, once.
    view.update(window, |app, cx| app.dismiss_toast(cx));
    pick(&view, window, agent(CodeEngine::Claude));
    assert_eq!(toast(&view, window), None, "Claude asks: nothing to say");
    pick(&view, window, agent(CodeEngine::Glm));
    assert_eq!(toast(&view, window).as_deref(), Some(glm.as_str()));
    let pending = view.update(window, |app, _| app.work.no_ask_notice.clone());
    assert_eq!(pending, None, "not said twice");

    // Settings changes the preference for the next project thread only.
    pick(&view, window, agent(CodeEngine::Claude));
    let current = executor(&view, window);
    view.update(window, |app, cx| {
        app.dismiss_toast(cx);
        app.open_settings(cx);
    });
    window.run_until_parked();
    click(window, "sx-nav-2");
    let item = cat::find("work.default_executor").unwrap();
    let cat::Control::Choice(options) = item.control else {
        panic!("a choice")
    };
    let dsh = options
        .iter()
        .position(|(stored, _)| *stored == "agent:dsh")
        .expect("a DeepSeek choice");
    click(
        window,
        &format!("sx-choice-{}", ix("work.default_executor") * 16 + dsh),
    );
    assert_eq!(executor(&view, window), current);
    new_conversation(&view, window);
    assert_eq!(executor(&view, window), agent(CodeEngine::Dsh));
    let said = toast(&view, window).unwrap_or_default();
    assert!(said.contains("run without asking"), "{said}");
}

/// With no configured starting helper, the owner's explicit last pick carries
/// to a new or reused thread, an unpicked older thread, and after relaunch.
/// Chat's own choice remains untouched by a Work pick.
#[gpui::test]
fn rc2r1f2_the_helper_picked_last_carries_to_a_new_conversation(cx: &mut TestAppContext) {
    if !in_isolated_child(
        "settings_code::default_helper_tests::rc2r1f2_the_helper_picked_last_carries_to_a_new_conversation",
    ) {
        return;
    }
    // With no starting preference, the owner's explicit last pick remains
    // the new-thread fallback.
    with_config("");
    let (view, window) = work_window(cx);
    assert_eq!(executor(&view, window), work::WorkExecutor::InApp);
    let (older, chat_choice) = view.update(window, |app, _| {
        let id = app
            .conversations
            .create_kind(1_000, awili_core::ConvKind::Work)
            .id;
        app.views
            .insert(id.clone(), crate::conv_view::ConvView::new());
        (id, app.choice_idx)
    });
    pick(&view, window, agent(CodeEngine::Claude));
    new_conversation(&view, window);
    assert_eq!(
        executor(&view, window),
        agent(CodeEngine::Claude),
        "a new conversation starts on the helper picked last"
    );
    // The new conversation was never sent: "+ New conversation" reuses it,
    // still on Claude.
    new_conversation(&view, window);
    assert_eq!(executor(&view, window), agent(CodeEngine::Claude));
    view.update_in(window, |app, window, cx| {
        app.select_conversation(&older, window, cx)
    });
    window.run_until_parked();
    assert_eq!(
        executor(&view, window),
        agent(CodeEngine::Claude),
        "a conversation with no pick and no run opens on the last pick"
    );
    remembered("agent:claude");
    assert_eq!(
        view.update(window, |app, _| app.choice_idx),
        chat_choice,
        "Chat keeps its own choice"
    );
    // Relaunch: a new app on the same store.
    let (again, window2) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    again.update(window2, |app, cx| {
        app.onboarding = crate::onboarding::Onboarding::existing();
        app.set_mode(Mode::Work, cx);
    });
    window2.run_until_parked();
    new_conversation(&again, window2);
    assert_eq!(
        executor(&again, window2),
        agent(CodeEngine::Claude),
        "after a relaunch, + New conversation is still on the last pick"
    );
}
