//! M1c (S3-F03) proof tests for `conversation_export`: source routing, the
//! code-tab transcript shape, never-overwriting writes, `~` paths, and the
//! short-title rule.

use std::path::{Path, PathBuf};

use awili_core::{Message, MessageKind, Role};

use crate::conversation_export::{
    code_tab_export, display_path, export_source, short_title, write_export, ExportSource,
};
use crate::work::Mode;

/// A throwaway directory for write tests, removed afterwards.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        // RC2-APP (K-flake2): under this process's scratch root.
        let dir = crate::test_tmp::fresh_dir(&format!("m1c-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn user_msg(text: &str, ts: u64) -> Message {
    Message {
        role: Role::User,
        text: text.to_string(),
        kind: MessageKind::Normal,
        ts,
        provenance: None,
        superseded_by: None,
    }
}

fn assistant_msg(text: &str, ts: u64) -> Message {
    Message {
        role: Role::Assistant,
        text: text.to_string(),
        kind: MessageKind::Normal,
        ts,
        provenance: None,
        superseded_by: None,
    }
}

#[test]
fn m1c_export_source_routes_work_code() {
    assert_eq!(
        export_source(Mode::Work, "conv-2", None),
        ExportSource::Stored("conv-2".into())
    );
    // Code exports the active tab when there is one.
    assert_eq!(
        export_source(Mode::Code, "ignored", Some(1)),
        ExportSource::CodeTab(1)
    );
    // Code with no tab active exports nothing.
    assert_eq!(
        export_source(Mode::Code, "ignored", None),
        ExportSource::NotAConversation
    );
    // Every other mode exports nothing.
    for mode in [
        Mode::Skills,
        Mode::Cron,
        Mode::Sessions,
        Mode::CommandCenter,
        Mode::Messaging,
    ] {
        assert_eq!(
            export_source(mode, "conv-9", None),
            ExportSource::NotAConversation,
            "{mode:?}"
        );
    }
}

#[test]
fn m1c_code_tab_export_keeps_order_and_title_rules() {
    let msgs = vec![
        assistant_msg("hi there", 0),
        user_msg("  fix the login bug\nsecond line should not leak", 1),
        assistant_msg("on it", 2),
    ];
    let exp = code_tab_export("Claude", &msgs, 1750000000);
    // Title = first user message's first line, trimmed.
    assert_eq!(exp.title, "fix the login bug");
    assert_eq!(exp.created_at, 1750000000);
    assert_eq!(exp.messages.len(), 3);
    // Order preserved; ids are positional; roles/kinds use the wire strings.
    assert_eq!(exp.messages[0].id, 0);
    assert_eq!(exp.messages[0].role, "assistant");
    assert_eq!(exp.messages[0].ts, 0);
    assert_eq!(exp.messages[0].kind, "normal");
    assert_eq!(exp.messages[1].id, 1);
    assert_eq!(exp.messages[1].role, "user");
    assert_eq!(
        exp.messages[1].text,
        "  fix the login bug\nsecond line should not leak"
    );
    assert!(exp.messages.iter().all(|m| m.superseded_by.is_none()));
    let md = exp.to_markdown();
    let pos_a = md.find("hi there").expect("first assistant text");
    let pos_u = md
        .find("**User**\n\n  fix the login bug")
        .expect("user block");
    let pos_b = md.find("on it").expect("second assistant text");
    assert!(pos_a < pos_u && pos_u < pos_b, "order: {md}");
}

#[test]
fn m1c_code_tab_export_title_capped_at_60_chars() {
    let long = "a".repeat(80);
    let exp = code_tab_export("Claude", &[user_msg(&long, 0)], 1);
    let mut expected: String = "a".repeat(59);
    expected.push('…');
    assert_eq!(exp.title, expected);
    assert_eq!(
        exp.title.chars().count(),
        60,
        "the ellipsis is inside the cap"
    );
    // Exactly 60 chars survives the cap untouched.
    let exp60 = code_tab_export("Claude", &[user_msg(&"b".repeat(60), 0)], 1);
    assert_eq!(exp60.title, "b".repeat(60));
}

#[test]
fn m1c_code_tab_export_falls_back_to_engine_label() {
    // No messages at all.
    assert_eq!(code_tab_export("Gemini", &[], 5).title, "Gemini session");
    // Only assistant messages (the user has not typed yet).
    assert_eq!(
        code_tab_export("Claude", &[assistant_msg("streaming…", 0)], 5).title,
        "Claude session"
    );
    // First user line blank → the engine-label fallback, not an empty title.
    let blank_user = user_msg("\n\nreal question", 0);
    assert_eq!(
        code_tab_export("Codex", &[blank_user], 5).title,
        "Codex session"
    );
}

#[test]
fn m1c_write_export_never_overwrites() {
    let dir = TempDir::new("overwrite");
    let first = dir.path().join("report.md");
    // Fresh target: plain filename.
    assert_eq!(write_export(&first, "v1").expect("first write"), first);
    // Existing file: -2, then -3, never touching the original.
    assert_eq!(
        write_export(&first, "v2").expect("second write"),
        dir.path().join("report-2.md")
    );
    assert_eq!(
        write_export(&first, "v3").expect("third write"),
        dir.path().join("report-3.md")
    );
    assert_eq!(std::fs::read_to_string(&first).expect("original"), "v1");
    // A file without an extension bumps the stem instead.
    let bare = dir.path().join("notes");
    assert_eq!(write_export(&bare, "n1").expect("bare"), bare);
    assert_eq!(
        write_export(&bare, "n2").expect("bare-2"),
        dir.path().join("notes-2")
    );
    // Parent directories are created on demand.
    let nested = dir.path().join("deep/nest/doc.md");
    assert_eq!(write_export(&nested, "deep").expect("nested"), nested);
}

#[test]
fn m1c_display_path_shrinks_home_to_tilde() {
    let home = Path::new("/home/dev");
    assert_eq!(
        display_path(Path::new("/home/dev/Downloads/a.md"), Some(home)),
        "~/Downloads/a.md"
    );
    assert_eq!(display_path(home, Some(home)), "~");
    // Sibling paths are NOT shrunk.
    assert_eq!(
        display_path(Path::new("/home/developer/x.md"), Some(home)),
        "/home/developer/x.md"
    );
    assert_eq!(display_path(Path::new("/tmp/a.md"), None), "/tmp/a.md");
}

#[test]
fn m1c_short_title_caps_at_40_chars_on_char_boundary() {
    // ≤40 unchanged (40 CJK chars is 120 bytes — the cap is in chars).
    let cjk40 = "字".repeat(40);
    assert_eq!(short_title(&cjk40), cjk40);
    // >40 cut and suffixed.
    let cjk41 = "字".repeat(41);
    assert_eq!(short_title(&cjk41), format!("{}…", "字".repeat(40)));
    let ascii = "x".repeat(45);
    assert_eq!(short_title(&ascii), format!("{}…", "x".repeat(40)));
    assert_eq!(short_title("hello"), "hello");
}

// ---- M1c-FIX1: one gated export path, every entry point, never-overwriting writes ----

use super::gateway_queue_tests::isolated;
use crate::conversation_export::{
    export_candidates, NOT_A_CONVERSATION, NOT_IN_DATABASE, STILL_STREAMING,
};
use crate::AwiliApp;
use awili_code::CodeEngine;
use gpui::{AppContext as _, Context};

/// The first user line of the Code tab on screen (S3 T7), with the
/// characters the tester saw mangled.
const CODE_Q: &str = "code-tab question — é…";
const CODE_A: &str = "code-tab answer";

/// Put a Code tab holding one exchange on screen.
fn code_tab_on_screen(app: &mut AwiliApp, streaming: bool) {
    app.mode = Mode::Code;
    app.new_code_tab(CodeEngine::Claude);
    let i = app.work.code_active;
    let tab = &mut app.work.code_tabs[i];
    tab.convo.push(Role::User, CODE_Q);
    tab.convo.push(Role::Assistant, CODE_A);
    tab.streaming = streaming;
}

/// The export directory (`downloads_path`'s parent) under the isolated HOME.
fn downloads_dir() -> PathBuf {
    crate::attachments::downloads_path("x.md")
        .expect("a downloads dir")
        .parent()
        .expect("a parent")
        .to_path_buf()
}

/// The `.md` files in the export directory, sorted; empty when it is missing.
fn exported_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(downloads_dir())
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn toast_text(app: &AwiliApp) -> String {
    app.toast
        .visible()
        .map(ToString::to_string)
        .unwrap_or_default()
}

fn clipboard_text(cx: &mut Context<AwiliApp>) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

/// P2 (Codex T2 review of 8f9c26f): each permitted name appears exactly once
/// — `first`, then -2 … -99 — so -99 is never tried twice.
#[test]
fn m1c_fix_candidates_enumerate_each_name_once() {
    let first = Path::new("/d/x-2026-09-29.md");
    let names = export_candidates(first);
    assert_eq!(names.len(), 99);
    assert_eq!(names[0], first);
    assert_eq!(names[1], Path::new("/d/x-2026-09-29-2.md"));
    assert_eq!(names[98], Path::new("/d/x-2026-09-29-99.md"));
    let unique: std::collections::HashSet<&PathBuf> = names.iter().collect();
    assert_eq!(unique.len(), 99, "a name is repeated: {names:?}");
    let bare = export_candidates(Path::new("/d/notes"));
    assert_eq!(bare[1], Path::new("/d/notes-2"));
    assert_eq!(bare.len(), 99);
}

/// P2 (Codex T2 review of 8f9c26f): only `AlreadyExists` moves on to the next
/// name. Any other open error (here a file name longer than any filesystem
/// accepts, which fails the same way for root) comes back as itself instead
/// of 99 retries ending in a made-up "already exists"; nothing is written.
#[test]
fn m1c_fix_write_export_propagates_other_open_errors() {
    let dir = TempDir::new("nametoolong");
    let first = dir.path().join(format!("{}.md", "n".repeat(300)));
    let err = write_export(&first, "body").expect_err("a 300-byte name cannot be created");
    assert_ne!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
    assert!(
        !err.to_string().contains("export target already exists"),
        "{err}"
    );
    let left = std::fs::read_dir(dir.path()).expect("dir").count();
    assert_eq!(left, 0, "nothing written");
}

/// S3-F03 + Codex P1: File > "Export Conversation as Markdown" (the real
/// native action through `install_native_menu_actions`) writes the Code tab
/// on screen to a file in Downloads and copies the same bytes.
#[gpui::test]
fn m1c_fix_menu_export_writes_the_code_tab_to_a_file(cx: &mut gpui::TestAppContext) {
    if isolated("work::work_export_tests::m1c_fix_menu_export_writes_the_code_tab_to_a_file")
        .is_none()
    {
        return;
    }
    let window = cx.add_window(|_window, cx| AwiliApp::new(cx));
    cx.update(|cx| {
        cx.set_global(crate::AwiliWindow(window));
        crate::install_native_menu_actions(cx);
    });
    window
        .update(cx, |app, _window, cx| {
            code_tab_on_screen(app, false);
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();
    assert!(
        exported_files().is_empty(),
        "no file before the menu action"
    );

    cx.dispatch_action(window.into(), crate::native_menu::ExportConversation);
    cx.run_until_parked();

    let files = exported_files();
    assert_eq!(files.len(), 1, "the menu export wrote one file: {files:?}");
    let text = std::fs::read_to_string(&files[0]).expect("utf-8 file");
    assert!(text.contains(CODE_Q) && text.contains(CODE_A), "{text}");
    window
        .update(cx, |app, _window, cx| {
            assert_eq!(clipboard_text(cx).as_deref(), Some(text.as_str()));
            let toast = toast_text(app);
            assert!(toast.starts_with("Saved \"code-tab question"), "{toast}");
            assert!(
                toast.contains(".md") && toast.contains("also copied"),
                "{toast}"
            );
        })
        .unwrap();
}

/// S3-F03 + Codex P1: ⌘⇧C (the real key handler) saves a file, and a second
/// press never overwrites the first (-2).
#[gpui::test]
fn m1c_fix_shortcut_saves_a_file_and_never_overwrites(cx: &mut gpui::TestAppContext) {
    if isolated("work::work_export_tests::m1c_fix_shortcut_saves_a_file_and_never_overwrites")
        .is_none()
    {
        return;
    }
    let window = cx.add_window(|_window, cx| AwiliApp::new(cx));
    window
        .update(cx, |app, _window, cx| {
            code_tab_on_screen(app, false);
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();

    cx.simulate_keystrokes(window.into(), "cmd-shift-c");
    let files = exported_files();
    assert_eq!(files.len(), 1, "⌘⇧C wrote one file: {files:?}");
    let first_bytes = std::fs::read(&files[0]).expect("first file");
    assert!(String::from_utf8_lossy(&first_bytes).contains(CODE_Q));

    cx.simulate_keystrokes(window.into(), "cmd-shift-c");
    let files = exported_files();
    assert_eq!(
        files.len(),
        2,
        "the second ⌘⇧C wrote a second file: {files:?}"
    );
    let second = files
        .iter()
        .find(|p| p.to_string_lossy().ends_with("-2.md"))
        .expect("a -2 file");
    let first = files.iter().find(|p| *p != second).expect("the first file");
    assert_eq!(
        std::fs::read(first).unwrap(),
        first_bytes,
        "first unchanged"
    );
}

/// S3-F09: the toolbar / palette Copy copies the Code tab on screen to the
/// clipboard and writes no file; the palette Save writes one.
#[gpui::test]
fn m1c_fix_copy_is_clipboard_only_and_save_writes(cx: &mut gpui::TestAppContext) {
    if isolated("work::work_export_tests::m1c_fix_copy_is_clipboard_only_and_save_writes").is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        code_tab_on_screen(app, false);
        app.export_conversation(cx);
        let copied = clipboard_text(cx).expect("clipboard written");
        assert!(
            copied.contains(CODE_Q) && copied.contains(CODE_A),
            "{copied}"
        );
        assert!(exported_files().is_empty(), "Copy wrote no file");
        let toast = toast_text(app);
        assert!(toast.starts_with("Copied \"code-tab question"), "{toast}");

        app.save_conversation(cx);
        let files = exported_files();
        assert_eq!(files.len(), 1, "Save wrote one file: {files:?}");
        assert_eq!(std::fs::read_to_string(&files[0]).unwrap(), copied);
    });
}

/// Codex P1: a streaming Code tab is refused by every entry point — menu,
/// ⌘⇧C, Copy, Save and even the shared helpers called directly — with no
/// file and an untouched clipboard.
#[gpui::test]
fn m1c_fix_streaming_code_tab_refuses_every_entry_point(cx: &mut gpui::TestAppContext) {
    if isolated("work::work_export_tests::m1c_fix_streaming_code_tab_refuses_every_entry_point")
        .is_none()
    {
        return;
    }
    let window = cx.add_window(|_window, cx| AwiliApp::new(cx));
    cx.update(|cx| {
        cx.set_global(crate::AwiliWindow(window));
        crate::install_native_menu_actions(cx);
    });
    window
        .update(cx, |app, _window, cx| {
            code_tab_on_screen(app, true);
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();

    cx.dispatch_action(window.into(), crate::native_menu::ExportConversation);
    cx.run_until_parked();
    window
        .update(cx, |app, _window, _cx| {
            assert_eq!(toast_text(app), STILL_STREAMING)
        })
        .unwrap();

    cx.simulate_keystrokes(window.into(), "cmd-shift-c");
    window
        .update(cx, |app, _window, cx| {
            assert_eq!(toast_text(app), STILL_STREAMING);
            app.toast.show("");
            app.export_conversation(cx);
            assert_eq!(toast_text(app), STILL_STREAMING, "Copy");
            app.toast.show("");
            app.save_conversation(cx);
            assert_eq!(toast_text(app), STILL_STREAMING, "Save");
            app.toast.show("");
            app.copy_on_screen_conversation(cx);
            assert_eq!(toast_text(app), STILL_STREAMING, "copy helper");
            app.toast.show("");
            app.save_on_screen_conversation(cx);
            assert_eq!(toast_text(app), STILL_STREAMING, "save helper");
            assert_eq!(clipboard_text(cx), None, "clipboard untouched");
        })
        .unwrap();
    assert!(exported_files().is_empty(), "no file while streaming");
}

/// Codex P1: the reply on screen still streaming refuses Save and Copy
/// through the same path (the Stored arm of `export_on_screen`).
#[gpui::test]
fn m1c_fix_streaming_chat_reply_refuses(cx: &mut gpui::TestAppContext) {
    if isolated("work::work_export_tests::m1c_fix_streaming_chat_reply_refuses").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.mode = Mode::Work;
        app.set_active_conversation("chat-a");
        let view = crate::conv_view::ConvView::from_state(awili_core::ConversationState::new());
        app.work.running = true;
        app.work.conv_id = Some("chat-a".into());
        app.views.insert("chat-a".into(), view);
        app.save_conversation(cx);
        assert_eq!(toast_text(app), STILL_STREAMING, "Save");
        app.toast.show("");
        app.export_conversation(cx);
        assert_eq!(toast_text(app), STILL_STREAMING, "Copy");
        assert_eq!(clipboard_text(cx), None);
    });
    assert!(exported_files().is_empty());
}

/// Codex P1 (restored gate): a Work run blocks export only of ITS OWN
/// conversation. The run's conversation on screen is refused; another Work
/// conversation on screen goes past the gate (here to the database read,
/// which has no such row in a fresh HOME).
#[gpui::test]
fn m1c_fix_work_run_blocks_only_its_own_conversation(cx: &mut gpui::TestAppContext) {
    if isolated("work::work_export_tests::m1c_fix_work_run_blocks_only_its_own_conversation")
        .is_none()
    {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.mode = Mode::Work;
        app.work.running = true;
        app.work.conv_id = Some("work-a".into());
        app.set_active_conversation("work-a");
        app.save_conversation(cx);
        assert_eq!(
            toast_text(app),
            STILL_STREAMING,
            "the run's own conversation"
        );

        app.set_active_conversation("work-b");
        app.save_conversation(cx);
        assert_eq!(
            toast_text(app),
            NOT_IN_DATABASE,
            "an unrelated run never blocks"
        );
    });
}

/// S3-F03: a mode with no conversation on screen refuses; nothing written,
/// clipboard untouched.
#[gpui::test]
fn m1c_fix_non_conversation_mode_refuses(cx: &mut gpui::TestAppContext) {
    if isolated("work::work_export_tests::m1c_fix_non_conversation_mode_refuses").is_none() {
        return;
    }
    let entity = cx.new(AwiliApp::new);
    entity.update(cx, |app, cx| {
        app.mode = Mode::Skills;
        app.save_conversation(cx);
        assert_eq!(toast_text(app), NOT_A_CONVERSATION);
        assert_eq!(clipboard_text(cx), None);
    });
    assert!(exported_files().is_empty());
}

/// R2f (RC1 round 2, Minor 10: "file goes to ~/Downloads … the toast is the
/// only clue and is clipped"): File > "Export Conversation as Markdown" (the
/// real native action) says in plain words that the file is in the Downloads
/// folder, names the very file it wrote, and the notice stays long enough to
/// read (RC1-H7: then it goes by itself).
#[gpui::test]
fn r2f_export_says_where_the_file_went_and_stays(cx: &mut gpui::TestAppContext) {
    if isolated("work::work_export_tests::r2f_export_says_where_the_file_went_and_stays").is_none()
    {
        return;
    }
    let window = cx.add_window(|_window, cx| AwiliApp::new(cx));
    cx.update(|cx| {
        cx.set_global(crate::AwiliWindow(window));
        crate::install_native_menu_actions(cx);
    });
    window
        .update(cx, |app, _window, cx| {
            code_tab_on_screen(app, false);
            cx.notify();
        })
        .unwrap();
    cx.run_until_parked();
    cx.dispatch_action(window.into(), crate::native_menu::ExportConversation);
    cx.run_until_parked();
    let files = exported_files();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    let toast = window
        .update(cx, |app, _window, _cx| toast_text(app))
        .unwrap();
    assert!(
        toast.contains(&format!("in your Downloads folder as {name}")),
        "{toast}"
    );
    assert!(!toast.contains("~/"), "no raw path: {toast}");
    // RC1-H7 (amended): long enough to read, then it goes by itself.
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(10));
    cx.run_until_parked();
    let later = window
        .update(cx, |app, _window, _cx| toast_text(app))
        .unwrap();
    assert_eq!(later, toast, "the notice stays long enough to read");
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    cx.run_until_parked();
    let gone = window
        .update(cx, |app, _window, _cx| toast_text(app))
        .unwrap();
    assert_eq!(gone, "", "then it goes by itself");
}

/// RC2-UX (R2-19, round 2 Minor 10: no folder choice, no "Show in Finder"):
/// ⌘⇧S opens the system save panel in Downloads; the conversation goes where
/// the owner chose (and is copied); the toast names the place and its
/// "Show in Finder" button shows the file. The ⌘⇧C save offers the button too.
#[gpui::test]
fn rc2ux_save_as_goes_where_the_owner_chose_and_shows_in_finder(cx: &mut gpui::TestAppContext) {
    let Some(home) = isolated(
        "work::work_export_tests::rc2ux_save_as_goes_where_the_owner_chose_and_shows_in_finder",
    ) else {
        return;
    };
    let (view, window) = cx.add_window_view(|_window, cx| AwiliApp::new(cx));
    window.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    view.update(window, |app, cx| {
        code_tab_on_screen(app, false);
        cx.notify();
    });
    window.run_until_parked();
    window.simulate_keystrokes("cmd-shift-s");
    assert!(window.did_prompt_for_new_path(), "the save panel opened");
    let chosen = home.join("Documents").join("garden notes.md");
    std::fs::create_dir_all(chosen.parent().unwrap()).unwrap();
    let opened_in = std::cell::RefCell::new(None);
    window.simulate_new_path_selection(|dir| {
        *opened_in.borrow_mut() = Some(dir.to_path_buf());
        Some(chosen.clone())
    });
    window.run_until_parked();
    assert_eq!(
        opened_in.into_inner(),
        Some(downloads_dir()),
        "opened in Downloads"
    );
    let text = std::fs::read_to_string(&chosen).expect("the file the owner named");
    assert!(text.contains(CODE_Q) && text.contains(CODE_A), "{text}");
    assert!(exported_files().is_empty(), "nothing went to Downloads");
    view.update(window, |app, cx| {
        assert_eq!(clipboard_text(cx).as_deref(), Some(text.as_str()));
        let toast = toast_text(app);
        assert!(toast.contains("~/Documents/garden notes.md"), "{toast}");
        assert_eq!(app.toast.reveal.as_deref(), Some(chosen.as_path()));
    });
    let pill = window
        .debug_bounds("awili-toast-reveal")
        .expect("the Show in Finder button is drawn");
    window.simulate_click(pill.center(), gpui::Modifiers::default());
    window.run_until_parked();
    crate::conversation_export::REVEALED.with(|r| assert_eq!(*r.borrow(), vec![chosen.clone()]));
    view.read_with(window, |app, _| assert!(app.toast.visible().is_none()));

    // ⌘⇧C saves to Downloads and offers the same button.
    window.simulate_keystrokes("cmd-shift-c");
    let files = exported_files();
    assert_eq!(files.len(), 1);
    view.read_with(window, |app, _| {
        assert_eq!(app.toast.reveal.as_deref(), Some(files[0].as_path()));
    });
    assert!(window.debug_bounds("awili-toast-reveal").is_some());
}
