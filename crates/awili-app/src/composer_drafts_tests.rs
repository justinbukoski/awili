//! RC2-UX proof (R2-18, R5-7, S-O5): the message box keeps one draft per
//! conversation. The real `AwiliApp` (a private HOME, in a subprocess) is
//! drawn in a test window, so the box follows the conversation on screen the
//! way the app does it every frame.

use std::path::PathBuf;

use awili_code::CodeEngine;
use awili_core::{ConvKind, Role};
use gpui::{px, size};

use super::{code_tab_key, conversation_key, Drafts};
use crate::work::{Mode, WorkExecutor};
use crate::AwiliApp;

const CHILD: &str = "AWILI_RC2UX_DRAFTS_TEST_HOME";

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

/// A Work conversation with one turn (so "+ New conversation" makes a new
/// one), made active.
fn work_conversation_with_a_turn(app: &mut AwiliApp) -> String {
    app.onboarding = crate::onboarding::Onboarding::existing();
    app.fleet_pack = None;
    app.memory.enabled = false;
    app.mode = Mode::Work;
    app.work.executor = WorkExecutor::Agent {
        engine: CodeEngine::Claude,
        model: None,
    };
    let meta = app.conversations.create_kind(1_000_000, ConvKind::Work);
    let mut state = awili_core::ConversationState::new();
    state.push(Role::User, "Rename every .txt to .md");
    state.push(Role::Assistant, "Here is the plan: rename the three files.");
    app.views.insert(
        meta.id.clone(),
        crate::conv_view::ConvView::from_state(state),
    );
    app.set_active_conversation(&meta.id);
    meta.id
}

fn box_text(window: &mut gpui::VisualTestContext, view: &gpui::Entity<AwiliApp>) -> String {
    view.read_with(window, |app, cx| app.composer.read(cx).text().to_string())
}

fn type_in_box(window: &mut gpui::VisualTestContext, view: &gpui::Entity<AwiliApp>, text: &str) {
    view.update(window, |app, cx| {
        app.composer.update(cx, |c, cx| c.set_text(text, cx));
    });
    window.run_until_parked();
}

/// The bookkeeping: leaving a conversation keeps its text, coming back
/// brings it back, a new one starts empty, blank drafts are not kept, a
/// handed-back message waits ahead of what was typed there.
#[test]
fn rc2ux_drafts_follow_their_conversation() {
    let mut d = Drafts::default();
    let a = conversation_key("a");
    let b = conversation_key("b");
    assert_eq!(d.switch(&a, ""), None, "the first frame keeps the box");
    assert_eq!(d.switch(&a, "half"), None, "same conversation");
    assert_eq!(
        d.switch(&b, "half").as_deref(),
        Some(""),
        "a new one is empty"
    );
    assert_eq!(d.stored(&a), Some("half"));
    assert_eq!(d.switch(&a, "   ").as_deref(), Some("half"));
    assert_eq!(d.stored(&b), None, "a blank draft is not kept");
    d.hand_back(&b, "parked message");
    d.hand_back(&b, "second");
    assert_eq!(d.stored(&b), Some("second\n\nparked message"));
    assert_eq!(
        d.switch(&b, "").as_deref(),
        Some("second\n\nparked message")
    );
    assert!(d.is_shown(&b));
    assert_eq!(code_tab_key(3), "code:3");
    // A message handed back before the first frame joins the box's text.
    let mut first = Drafts::default();
    first.hand_back(&a, "parked");
    assert_eq!(
        first.switch(&a, "typed").as_deref(),
        Some("parked\n\ntyped")
    );
}

/// R5-7 (round 4 Minor 7, round 5 Minor 7, round 7 Minor 2) and R2-18
/// (round 2 Minor 14): a request a Deny put back, or half a sentence, no
/// longer follows the owner into "+ New conversation", another
/// conversation, Chat or Code; each keeps its own text and gets it back.
#[gpui::test]
fn rc2ux_each_conversation_keeps_its_own_draft(cx: &mut gpui::TestAppContext) {
    if isolated("composer_drafts::tests::rc2ux_each_conversation_keeps_its_own_draft").is_none() {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    let a = view.update(window, |app, cx| {
        let a = work_conversation_with_a_turn(app);
        cx.notify();
        a
    });
    window.run_until_parked();
    // The plan card's Deny puts the request back into the box.
    view.update(window, |app, cx| {
        app.plan_restore_request(Some("Rename every .txt to .md".into()), cx)
    });
    window.run_until_parked();
    assert_eq!(box_text(window, &view), "Rename every .txt to .md");

    // "+ New conversation": an empty box.
    view.update_in(window, |app, window, cx| app.new_conversation(window, cx));
    window.run_until_parked();
    let b = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    assert_ne!(a, b);
    assert_eq!(
        box_text(window, &view),
        "",
        "the Deny's request stays behind"
    );
    type_in_box(window, &view, "B's half sentence");

    // Back to the first conversation: its request is back.
    view.update(window, |app, cx| {
        app.set_active_conversation(&a);
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(box_text(window, &view), "Rename every .txt to .md");

    view.update(window, |app, cx| app.set_mode(Mode::Code, cx));
    window.run_until_parked();
    assert_eq!(box_text(window, &view), "", "Code starts empty");
    type_in_box(window, &view, "fix the build");
    view.update(window, |app, cx| app.set_mode(Mode::Code, cx));
    window.run_until_parked();
    assert_eq!(box_text(window, &view), "fix the build");

    // Work comes back to its conversation and its text; B kept its own.
    view.update(window, |app, cx| app.set_mode(Mode::Work, cx));
    window.run_until_parked();
    let active = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    let expected = if active == a {
        "Rename every .txt to .md"
    } else {
        "B's half sentence"
    };
    assert_eq!(box_text(window, &view), expected);
    view.update(window, |app, cx| {
        app.set_active_conversation(&b);
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(box_text(window, &view), "B's half sentence");
}

/// S-O5 (RC1-O designer decision): a message parked behind the local model
/// check, whose answer came after the owner moved to another conversation,
/// went only to the clipboard. It now also waits in the box of the
/// conversation it was typed in (and never in the one on screen).
#[gpui::test]
fn rc2ux_a_handed_back_message_waits_in_its_own_conversation(cx: &mut gpui::TestAppContext) {
    if isolated("composer_drafts::tests::rc2ux_a_handed_back_message_waits_in_its_own_conversation")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    let a = view.update(window, |app, cx| {
        let a = work_conversation_with_a_turn(app);
        app.work.engine.base_url = "http://127.0.0.1:9".into();
        app.work.engine.model = "m".into();
        app.work.engine.label = "Local M".into();
        app.work.executor = WorkExecutor::InApp;
        cx.notify();
        a
    });
    window.run_until_parked();
    let other = view.update(window, |app, cx| {
        crate::local_model_app::TEST_LOCAL_PROBE_RESULT.with(|slot| {
            *slot.borrow_mut() = Some(Err("x".into()));
        });
        app.submit_work_local_first("hello".into(), cx);
        let other = app.conversations.create_kind(2_000_000, ConvKind::Work);
        app.views.insert(
            other.id.clone(),
            crate::conv_view::ConvView::from_state(awili_core::ConversationState::new()),
        );
        app.set_active_conversation(&other.id);
        cx.notify();
        other.id
    });
    window.run_until_parked();
    view.read_with(window, |app, _| {
        assert_eq!(
            app.toast.msg.as_deref(),
            Some(crate::local_model_health::MOVED_ON_NOTICE)
        );
    });
    assert!(crate::local_model_health::MOVED_ON_NOTICE.contains("back in the box"));
    assert_eq!(
        box_text(window, &view),
        "",
        "never in the conversation on screen"
    );
    assert_eq!(
        window
            .read_from_clipboard()
            .and_then(|c| c.text())
            .as_deref(),
        Some("hello")
    );
    view.update(window, |app, cx| {
        app.set_active_conversation(&a);
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(box_text(window, &view), "hello");
    view.update(window, |app, cx| {
        app.set_active_conversation(&other);
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(box_text(window, &view), "");
}

/// RC2-R54 (usability RC2 round 4, Minor: "Continue in Work" showed
/// `1. **Sort by size or date** in your file manager`): the box gets the
/// answer as plain text \u{2014} no bold marks, code ticks or heading hashes;
/// numbering, bullets, line breaks and a "#" inside a line stay.
#[test]
fn rc2r54_the_work_box_gets_plain_text() {
    let md = "## Steps\n1. **Sort by size or date** in your file manager\n* Run `ls -la`\n   - keep #2 too";
    assert_eq!(
        super::plain_box_text(md),
        "Steps\n1. Sort by size or date in your file manager\n- Run ls -la\n   - keep #2 too"
    );
    let council = crate::council_run::work_box_text(Some("q"), "Codex", "**ZFS** wins");
    assert!(council.ends_with("ZFS wins"), "{council}");
}

/// RC2-R5M5 (slice review befb867d, P3): only the exact text Awili put in
/// the box counts as untouched. An edit that only adds indentation or a
/// trailing newline is the owner's own, so the next "Continue in Work" or
/// "+ New conversation" must not reuse the conversation and erase it.
#[test]
fn rc2r5m5_a_whitespace_only_edit_is_the_owners_draft() {
    let mut d = Drafts::default();
    let a = conversation_key("a");
    let b = conversation_key("b");
    d.place(&a, "copied answer");
    assert!(d.is_untouched(&a, "copied answer"), "the exact copy");
    assert!(!d.is_untouched(&a, "copied answer\n"), "a trailing newline");
    assert!(!d.is_untouched(&a, "  copied answer"), "indentation");
    assert!(!d.is_untouched(&a, "copied answer "), "a trailing space");
    // Stored (not on screen): the whitespace edit is the owner's draft.
    assert_eq!(d.switch(&a, ""), None);
    assert_eq!(d.switch(&b, "  copied answer\n").as_deref(), Some(""));
    assert_eq!(d.stored(&a), Some("  copied answer\n"));
    assert!(d.has_owner_draft(&a), "an indented copy is the owner's");
    // Left exactly as placed, it is not.
    let mut e = Drafts::default();
    e.place(&a, "copied answer");
    assert_eq!(e.switch(&a, ""), None);
    assert_eq!(e.switch(&b, "copied answer").as_deref(), Some(""));
    assert!(!e.has_owner_draft(&a), "the untouched copy is not");
    // A blank placement never counts as untouched.
    let mut f = Drafts::default();
    f.place(&a, "  ");
    assert!(!f.is_untouched(&a, "  "));
}

/// RC2-R68 (usability RC2 round 5, Minor 5): "+ New conversation" re-opened
/// an old never-sent conversation holding the owner's edited draft (the
/// "MY-EDIT" cleanup text). A never-sent conversation with a draft of the
/// owner's is his work in progress: "+ New conversation" opens a blank one,
/// from that conversation or from another one, and the draft stays where
/// he left it.
#[gpui::test]
fn rc2r68_new_conversation_never_reopens_the_owners_draft(cx: &mut gpui::TestAppContext) {
    if isolated("composer_drafts::tests::rc2r68_new_conversation_never_reopens_the_owners_draft")
        .is_none()
    {
        return;
    }
    cx.executor().allow_parking();
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(size(px(1280.), px(800.)));
    let a = view.update(window, |app, cx| {
        let a = work_conversation_with_a_turn(app);
        cx.notify();
        a
    });
    window.run_until_parked();
    // A never-sent conversation with the owner's own draft in it.
    view.update_in(window, |app, window, cx| app.new_conversation(window, cx));
    window.run_until_parked();
    let draft = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    assert_ne!(draft, a);
    type_in_box(window, &view, "clean up Downloads MY-EDIT");

    // "+ New conversation" from that conversation: a new, blank one.
    view.update_in(window, |app, window, cx| app.new_conversation(window, cx));
    window.run_until_parked();
    let fresh = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    assert_ne!(fresh, draft, "the draft's conversation is not the new one");
    assert_eq!(box_text(window, &view), "");

    // From another conversation too.
    view.update(window, |app, cx| {
        app.set_active_conversation(&a);
        cx.notify();
    });
    window.run_until_parked();
    view.update_in(window, |app, window, cx| app.new_conversation(window, cx));
    window.run_until_parked();
    let again = view.read_with(window, |app, _| app.sidebar.active_id().to_string());
    assert_ne!(again, draft, "never the draft's conversation");
    assert_eq!(box_text(window, &view), "", "a blank box");
    assert_eq!(
        again, fresh,
        "the blank never-sent one is reused, no new row"
    );

    // The draft waits where the owner left it.
    view.update(window, |app, cx| {
        app.set_active_conversation(&draft);
        cx.notify();
    });
    window.run_until_parked();
    assert_eq!(box_text(window, &view), "clean up Downloads MY-EDIT");
}
