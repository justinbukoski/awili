//! U3d window/app-command helpers: ⌘Q quit-confirmation, native About,
//! minimum window size, and the ⌘Q/⌘H/⌥⌘H key bindings.
//!
//! The window/app command behaviour (⌘W hides, red button hides, ⌘Q confirms
//! while runs are in flight) is wired in `main.rs` and `native_menu.rs`; this
//! module holds the pure helpers, the `AwiliApp` methods those hooks call, and
//! the `u3d_` proof tests.

use gpui::{
    App, ClipboardItem, Context, FocusHandle, Global, KeyBinding, PromptButton, PromptLevel, Window,
};

use crate::{code_provider, native_menu, AwiliApp};

/// The floor for the window size (D12). RC1-H6: 800 wide, so at normal text
/// size the narrowed side columns still leave the middle column its
/// `layout::CENTER_MIN_W` (168 + 240 + 360 = 768).
pub const MIN_WINDOW_SIZE: (f32, f32) = (800., 480.);

/// A saved geometry is restorable only at or above the minimum size (D12);
/// smaller saves fall back to the centered default.
pub fn restorable(w: f32, h: f32) -> bool {
    w >= MIN_WINDOW_SIZE.0 && h >= MIN_WINDOW_SIZE.1
}

/// The key context of the whole window. GPUI evaluates a predicate like `!Terminal`
/// only against a non-empty context path, so the root view declares one.
pub const ROOT_KEY_CONTEXT: &str = "Awili";

/// Off macOS: if nothing in the window has focus (a click on empty canvas, a closed
/// overlay), give it to the root view. GPUI builds the key-context path from the
/// focused element, and a scoped binding such as `!Terminal` never matches an
/// empty path, so Ctrl+K and friends would silently stop working.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn ensure_root_focus(window: &mut Window, cx: &mut App, root: &FocusHandle) {
    // `contains_focused` is true for the root itself and for any focused element
    // inside it (never stolen from). It is false for no focus at all AND for a live
    // handle whose element left the tree, which also leaves the path empty.
    //
    // It is ALSO false for an element focused during this very update but not yet
    // painted (GPUI answers from the last rendered frame): a composer or editor that
    // a click handler just focused. Stealing then would take focus away from the
    // field the person is about to type in. So the decision waits for the end of
    // this effect cycle, after the frame has been drawn, and is made again
    // against it (not `on_next_frame`: that waits for the next platform frame,
    // which may be a while away on an idle window).
    if !root.contains_focused(window, cx) {
        let root = root.clone();
        window.defer(cx, move |window, cx| {
            if !root.contains_focused(window, cx) {
                window.focus(&root, cx);
            }
        });
    }
}

/// Where the app-wide bindings apply. macOS: everywhere, exactly as before (⌘ chords
/// never collided with a terminal). Windows and Linux: not while a terminal has
/// focus, whose shell needs Ctrl+W, Ctrl+K, Ctrl+N, Ctrl+Y and Ctrl+M raw.
fn binding_scope() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        None
    } else {
        Some("!Terminal")
    }
}

/// The standard ⌘/⌥⌘ key bindings (one source of truth; `main` binds these).
/// `secondary` is GPUI's "the platform's main modifier": ⌘ on macOS (so every
/// binding there is exactly what it was) and Ctrl on Windows and Linux. Hide and
/// Hide Others are macOS ideas and bound only there; Redo also answers Ctrl+Y off
/// macOS, as Windows users expect.
pub fn standard_bindings() -> Vec<KeyBinding> {
    let scope = binding_scope();
    let mut bindings = vec![
        gpui::KeyBinding::new("secondary-z", native_menu::Undo, scope),
        gpui::KeyBinding::new("secondary-shift-z", native_menu::Redo, scope),
        gpui::KeyBinding::new("secondary-x", native_menu::Cut, scope),
        gpui::KeyBinding::new("secondary-c", native_menu::Copy, scope),
        gpui::KeyBinding::new("secondary-a", native_menu::SelectAll, scope),
        gpui::KeyBinding::new("secondary-=", native_menu::ZoomIn, scope),
        gpui::KeyBinding::new("secondary--", native_menu::ZoomOut, scope),
        gpui::KeyBinding::new("secondary-0", native_menu::ActualSize, scope),
        gpui::KeyBinding::new("secondary-k", native_menu::OpenCommandPalette, scope),
        gpui::KeyBinding::new("secondary-n", native_menu::NewConversation, scope),
        gpui::KeyBinding::new("secondary-,", native_menu::OpenSettings, scope),
        gpui::KeyBinding::new("secondary-w", native_menu::CloseWindow, scope),
        gpui::KeyBinding::new("secondary-m", native_menu::Minimize, scope),
        gpui::KeyBinding::new("secondary-q", native_menu::Quit, scope),
    ];
    #[cfg(target_os = "macos")]
    bindings.extend([
        gpui::KeyBinding::new("secondary-h", native_menu::Hide, scope),
        gpui::KeyBinding::new("alt-secondary-h", native_menu::HideOthers, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    bindings.push(gpui::KeyBinding::new("ctrl-y", native_menu::Redo, scope));
    bindings
}

/// Re-entrancy guard: a second `cx.prompt` while one is open panics in gpui.
struct DialogOpen(std::cell::Cell<bool>);

impl Global for DialogOpen {}

impl Default for DialogOpen {
    fn default() -> Self {
        Self(std::cell::Cell::new(false))
    }
}

impl DialogOpen {
    /// Whether a prompt (quit or About) is currently open.
    pub fn is_open(cx: &App) -> bool {
        cx.try_global::<DialogOpen>().is_some_and(|g| g.0.get())
    }

    /// Mark a prompt open (creating the global if needed).
    pub fn mark_open(cx: &mut App) {
        cx.default_global::<DialogOpen>().0.set(true);
    }

    /// Clear the flag once the prompt settles (via `cx.update`).
    pub fn mark_closed(cx: &mut App) {
        if cx.has_global::<DialogOpen>() {
            cx.global_mut::<DialogOpen>().0.set(false);
        }
    }
}

/// The (message, detail) of the ⌘Q confirmation; `None` when nothing runs.
pub fn quit_prompt(n: usize) -> Option<(String, String)> {
    if n == 0 {
        return None;
    }
    let msg = if n == 1 {
        "1 run in progress — quit anyway?".to_string()
    } else {
        format!("{n} runs in progress — quit anyway?")
    };
    let detail =
        "Quitting stops them. Close the window instead (⌘W) to keep them running in the background."
            .to_string();
    Some((msg, detail))
}

/// The About dialog content (D9): title + version/commit. The commit comes
/// from `AWILI_GIT_SHA` (set by `scripts/bundle-mac.sh`); dev builds read `dev build`.
pub fn about_text() -> (String, String) {
    (
        "Awili".to_string(),
        format!(
            "Version {} (commit {})\nNative AI workspace",
            env!("CARGO_PKG_VERSION"),
            option_env!("AWILI_GIT_SHA").unwrap_or("dev build")
        ),
    )
}

/// The clipboard form of the version string the "Copy Version" button writes:
/// the About line's own words (RC1-F2: "commit" included).
pub fn about_version_string() -> String {
    format!(
        "Awili {} (commit {})",
        env!("CARGO_PKG_VERSION"),
        option_env!("AWILI_GIT_SHA").unwrap_or("dev build")
    )
}

impl AwiliApp {
    /// Number of in-flight runs (D8): the Work run +
    /// streaming Code tabs + council members still answering (RC2-R510,
    /// usability RC2 round 4 Minor: a quit with a council answering said
    /// nothing was running). Drives the ⌘Q confirmation.
    pub fn runs_in_progress(&self, _cx: &App) -> usize {
        let work = usize::from(self.work.running);
        let code = self.work.code_tabs.iter().filter(|t| t.streaming).count();
        let council: usize = self
            .councils
            .values()
            .flat_map(|p| p.council.rounds.iter())
            .flat_map(|r| r.turns.iter())
            .filter(|t| t.status.is_running())
            .count();
        work + code + council
    }

    /// D8: quit, confirming first when runs are in progress. A re-entrant
    /// `request_quit` while the dialog is open is a no-op (the `DialogOpen`
    /// guard), not a re-entrant prompt panic.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut App) {
        let n = self.runs_in_progress(cx);
        let Some((msg, detail)) = quit_prompt(n) else {
            // Nothing running: quit at once (the app's quit hook kills any
            // leftover agent process groups).
            code_provider::kill_all_active_groups();
            cx.quit();
            return;
        };
        // A re-entrant prompt panics in gpui — guard before prompting.
        if DialogOpen::is_open(cx) {
            return;
        }
        // Unhide so the alert is visible even when Awili is hidden.
        cx.activate(true);
        DialogOpen::mark_open(cx);
        let rx = window.prompt(
            PromptLevel::Warning,
            &msg,
            Some(&detail),
            &[
                PromptButton::new("Quit Anyway"),
                PromptButton::Cancel("Cancel".into()),
            ],
            cx,
        );
        cx.spawn(async move |cx| {
            match rx.await {
                Ok(0) => {
                    // "Quit Anyway": clear the guard, kill leftover groups and quit.
                    cx.update(|app| {
                        DialogOpen::mark_closed(app);
                        app.quit();
                    });
                    code_provider::kill_all_active_groups();
                }
                _ => {
                    // "Cancel" (or the receiver was dropped): just clear the guard.
                    cx.update(DialogOpen::mark_closed);
                }
            }
        })
        .detach();
    }

    /// D9: show the native About dialog. Re-entrant prompts are guarded the
    /// same way as [`Self::request_quit`].
    pub fn about(&mut self, window: &mut Window, cx: &mut App) {
        let (title, detail) = about_text();
        if DialogOpen::is_open(cx) {
            return;
        }
        cx.activate(true);
        DialogOpen::mark_open(cx);
        let rx = window.prompt(
            PromptLevel::Info,
            &title,
            Some(&detail),
            &[PromptButton::new("OK"), PromptButton::new("Copy Version")],
            cx,
        );
        cx.spawn(async move |cx| {
            if let Ok(ix) = rx.await {
                // "Copy Version" (index 1): write the version string to the clipboard.
                if ix == 1 {
                    let version = about_version_string();
                    cx.update(|app| app.write_to_clipboard(ClipboardItem::new_string(version)));
                }
            }
            cx.update(DialogOpen::mark_closed);
        })
        .detach();
    }
}

/// D7: ⌘W and the red button hide Awili; the window and its runs live on.
/// Tests count the calls instead: gpui's test platform panics on `hide`.
pub fn hide_app(cx: &mut App) {
    #[cfg(not(test))]
    cx.hide();
    #[cfg(test)]
    {
        let _ = cx;
        tests::HIDES.with(|n| n.set(n.get() + 1));
    }
}

/// D7: the red button hides instead of closing (`main` calls this inside
/// `open_window`); returning `false` keeps the window.
pub fn hide_on_close(window: &mut Window, cx: &mut App) {
    window.on_window_should_close(cx, |_window, cx| {
        hide_app(cx);
        false
    });
}

/// RC2-R61 (usability RC2 round 5, Minor 1: after ⌘Q "Quit Anyway" the run
/// came back as "⚠ the coding engine was terminated" with a "Failed" footer):
/// the line a Work reply ends with when quitting Awili stopped it.
pub(crate) const QUIT_STOPPED_LINE: &str = "*Stopped when you quit Awili.*";

/// RC2-R61: a Work reply that quitting stopped before it wrote anything.
pub(crate) const QUIT_STOPPED_EMPTY: &str = "*(stopped when you quit Awili, before any text)*";

impl AwiliApp {
    /// RC2-R61: every quit (⌘Q "Quit Anyway", the menu, the last window
    /// closing) first stops what is running the way the owner's Stop does,
    /// so the turn is kept as stopped by the quit, never as a failure, and
    /// waits (within gpui's shutdown limit) for those last writes to be
    /// saved. Registered once, from [`AwiliApp::new`].
    pub(crate) fn watch_quit(cx: &mut Context<Self>) {
        cx.on_app_quit(|app, cx| {
            app.stop_runs_for_quit(cx);
            let saved = app.persist.as_ref().map(|p| {
                let (tx, rx) = async_channel::bounded(1);
                p.send(crate::PersistCmd::ReadState {
                    key: String::new(),
                    reply: tx,
                });
                rx
            });
            async move {
                if let Some(rx) = saved {
                    let _ = rx.recv().await;
                }
            }
        })
        .detach();
    }

    /// RC2-R61: stop the Work run and every Code turn that is still going,
    /// as the owner's Stop would; the Work reply then says the quit stopped
    /// it ([`QUIT_STOPPED_LINE`]).
    pub(crate) fn stop_runs_for_quit(&mut self, cx: &mut Context<Self>) {
        if self.work.running {
            self.work.quit_stop = true;
            self.cancel_work_run(cx);
            // A run whose engine answers a Stop later (the in-app and
            // gateway workers) is finished here: Awili is closing.
            if self.work.stream_row_idx.is_some() {
                self.work.panel.status = Some(awili_work::run_events::RunStatus::Stopped);
                self.work_finish_stream(cx);
            }
            self.work.quit_stop = false;
        }
        let streaming: Vec<usize> = self
            .work
            .code_tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.stream_abort_tx.is_some())
            .map(|(i, _)| i)
            .collect();
        for i in streaming {
            self.stop_code_tab(i, cx);
        }
    }

    /// K02 (U3d-FIX1): Help › Keyboard Shortcuts toggles the sheet like ⌘?:
    /// opening focuses the overlay (so Escape reaches the root handler even
    /// when the old focus is stale), closing gives the composer focus back.
    pub fn toggle_shortcuts_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shell.tabs.delete_project.is_some() {
            return;
        }
        self.toggle_shortcuts(cx);
        if self.shortcuts.open {
            self.focus_shortcuts_overlay(window, cx);
        } else {
            self.focus_composer(window, cx);
        }
    }
}

/// K02 focus fix: the Keyboard Shortcuts sheet's scrim tracks this handle so
/// Escape (and ⌘?) dispatch lands on the window's root handler even when the
/// previously-focused element is no longer rendered.
#[derive(Clone)]
pub struct OverlayFocus(FocusHandle);

impl OverlayFocus {
    /// A fresh overlay focus handle.
    pub fn new(cx: &mut Context<AwiliApp>) -> Self {
        Self(cx.focus_handle())
    }

    /// The underlying focus handle (for `window.focus`).
    pub fn handle(&self) -> &FocusHandle {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext;
    use gpui::{px, size, Focusable};

    thread_local! {
        /// `hide_app` calls on this test thread (see `hide_app`).
        pub(super) static HIDES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// Run `test` in a subprocess with a private HOME (`AwiliApp::new` reads
    /// config and opens persistence; pattern of `s1a_proof_tests::isolated`).
    /// `true` inside the child; `false` in the parent once the child passed.
    fn isolated(test: &str) -> bool {
        const CHILD: &str = "AWILI_U3D_TEST_HOME";
        if std::env::var_os(CHILD).is_some() {
            return true;
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "isolated test failed:\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains(test) && stdout.contains("1 passed"),
            "subprocess must actually execute the test: {stdout}"
        );
        false
    }

    #[test]
    fn u3d_quit_prompt_text() {
        assert_eq!(quit_prompt(0), None, "0 runs → no dialog");
        let (msg, detail) = quit_prompt(1).unwrap();
        assert_eq!(msg, "1 run in progress — quit anyway?");
        assert!(
            detail.contains("⌘W"),
            "detail must name the keep-running path"
        );
        let (msg3, _) = quit_prompt(3).unwrap();
        assert_eq!(msg3, "3 runs in progress — quit anyway?");
    }

    /// D8: `runs_in_progress` is 0 by default.
    #[gpui::test]
    fn u3d_runs_in_progress_zero_by_default(cx: &mut gpui::TestAppContext) {
        if !isolated("app_lifecycle::tests::u3d_runs_in_progress_zero_by_default") {
            return;
        }
        let app = cx.new(AwiliApp::new);
        app.update(cx, |app, cx| {
            assert_eq!(app.runs_in_progress(cx), 0, "no runs by default");
        });
    }

    #[test]
    fn u3d_restorable_rejects_below_minimum() {
        assert!(
            restorable(MIN_WINDOW_SIZE.0, MIN_WINDOW_SIZE.1),
            "exactly min is ok"
        );
        assert!(restorable(900., 680.));
        assert!(
            !restorable(679., MIN_WINDOW_SIZE.1),
            "below-min width rejected"
        );
        assert!(
            !restorable(MIN_WINDOW_SIZE.0, 479.),
            "below-min height rejected"
        );
        assert!(!restorable(0., 0.), "zero size rejected");
    }

    #[test]
    fn u3d_about_text_has_version_and_commit() {
        let (title, detail) = about_text();
        assert_eq!(title, "Awili");
        assert!(
            detail.contains(env!("CARGO_PKG_VERSION")),
            "About must carry the version, got: {detail:?}"
        );
        assert!(
            detail.contains("dev build"),
            "with AWILI_GIT_SHA unset the commit reads 'dev build', got: {detail:?}"
        );
        assert_eq!(
            about_version_string(),
            format!("Awili {} (commit dev build)", env!("CARGO_PKG_VERSION"))
        );
    }

    /// RC1-F2 (RC-04): Copy Version copies the About line's words: the
    /// same version and the same "(commit …)" part, after "Awili".
    #[test]
    fn rc1f2_copy_version_matches_the_about_line() {
        let (_, detail) = about_text();
        let line = detail.lines().next().unwrap();
        let copied = about_version_string();
        assert_eq!(
            copied.strip_prefix("Awili "),
            line.strip_prefix("Version "),
            "About says {line:?}, Copy Version gives {copied:?}"
        );
        assert!(copied.contains("(commit "), "{copied:?}");
    }

    /// D8: Quit is bound (and on macOS Hide / Hide Others) in `standard_bindings`.
    #[test]
    fn u3d_bindings_include_quit_hide() {
        let b = standard_bindings();
        let keys: Vec<String> = b
            .iter()
            .flat_map(|k| k.keystrokes().iter().map(|ks| ks.unparse()))
            .collect();
        // The main modifier unparses as "cmd" on macOS and "ctrl" elsewhere.
        #[cfg(target_os = "macos")]
        {
            assert!(keys.iter().any(|k| k == "cmd-q"), "⌘Q bound");
            assert!(keys.iter().any(|k| k == "cmd-h"), "⌘H bound");
            assert!(keys.iter().any(|k| k == "alt-cmd-h"), "⌥⌘H bound");
        }
        #[cfg(not(target_os = "macos"))]
        {
            assert!(keys.iter().any(|k| k == "ctrl-q"), "Ctrl+Q bound");
            assert!(keys.iter().any(|k| k == "ctrl-y"), "Ctrl+Y redo bound");
            // Every edit and navigation chord is on Ctrl, as Windows users expect.
            for chord in [
                "ctrl-z",
                "ctrl-shift-z",
                "ctrl-x",
                "ctrl-c",
                "ctrl-a",
                "ctrl-n",
                "ctrl-k",
                "ctrl-,",
                "ctrl-=",
                "ctrl--",
                "ctrl-0",
                "ctrl-w",
                "ctrl-m",
            ] {
                assert!(keys.iter().any(|k| k == chord), "{chord} bound: {keys:?}");
            }
            // Hide and Hide Others are macOS ideas; Win/Super never leaks in.
            assert!(!keys.iter().any(|k| k.ends_with("-h")), "no Hide off macOS");
            assert!(
                !keys
                    .iter()
                    .any(|k| k.contains("win") || k.contains("super")),
                "no Windows-key bindings: {keys:?}"
            );
        }
    }

    #[test]
    fn u3d_shortcuts_sheet_lists_close_quit_and_menu() {
        use crate::shortcuts::SHORTCUTS;
        assert!(SHORTCUTS.iter().any(|(c, _)| *c == "⌘W"));
        assert!(SHORTCUTS.iter().any(|(c, _)| *c == "⌘Q"));
        assert!(SHORTCUTS.iter().any(|(c, _)| *c == "⌘H"));
        assert!(SHORTCUTS.iter().any(|(c, _)| c.contains("⌘Z")));
    }

    /// D8 (proof): with a run in flight, `request_quit` opens the ⌘Q
    /// confirmation; "Cancel" clears the guard so a second ⌘Q re-prompts.
    #[gpui::test]
    fn u3d_request_quit_prompts_when_runs_in_progress(cx: &mut gpui::TestAppContext) {
        if !isolated("app_lifecycle::tests::u3d_request_quit_prompts_when_runs_in_progress") {
            return;
        }
        let window = cx.open_window(size(px(800.), px(600.)), |_w, cx| crate::AwiliApp::new(cx));
        let _ = window.update(cx, |app, window, cx| {
            app.work.running = true;
            app.request_quit(window, cx)
        });
        assert!(
            cx.has_pending_prompt(),
            "a ⌘Q prompt must be open while a run is in flight"
        );
        let (msg, detail) = cx
            .pending_prompt()
            .expect("the pending prompt must carry text");
        assert!(msg.contains("1 run in progress"), "got: {msg:?}");
        assert!(
            detail.contains("⌘W"),
            "detail must name the keep-running path, got: {detail:?}"
        );
        // Cancel resolves, the guard clears, the app keeps running.
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(
            !cx.has_pending_prompt(),
            "the prompt must be gone after the answer"
        );
        // A second request_quit must re-prompt cleanly (guard cleared).
        let _ = window.update(cx, |app, window, cx| app.request_quit(window, cx));
        assert!(
            cx.has_pending_prompt(),
            "after cancel, ⌘Q must prompt again (guard cleared)"
        );
    }

    /// D9 (proof): `about` opens the About dialog; "OK" clears the guard so a
    /// second About re-prompts cleanly (no re-entrant panic).
    #[gpui::test]
    fn u3d_about_opens_version_dialog(cx: &mut gpui::TestAppContext) {
        if !isolated("app_lifecycle::tests::u3d_about_opens_version_dialog") {
            return;
        }
        let window = cx.open_window(size(px(800.), px(600.)), |_w, cx| crate::AwiliApp::new(cx));
        let _ = window.update(cx, |app, window, cx| app.about(window, cx));
        assert!(cx.has_pending_prompt(), "the About dialog must be open");
        let (msg, detail) = cx
            .pending_prompt()
            .expect("the About prompt must carry text");
        assert_eq!(msg, "Awili", "About title");
        assert!(
            detail.contains(env!("CARGO_PKG_VERSION")),
            "About detail must carry the version, got: {detail:?}"
        );
        // OK settles the dialog and clears the guard.
        cx.simulate_prompt_answer("OK");
        cx.run_until_parked();
        assert!(!cx.has_pending_prompt(), "the dialog must be gone after OK");
        let _ = window.update(cx, |app, window, cx| app.about(window, cx));
        assert!(
            cx.has_pending_prompt(),
            "after OK, About must prompt again (guard cleared)"
        );
    }

    // ---- U3d-FIX1 ------------------------------------------------------------

    /// D8: the Workshop run and one streaming Code tab are two runs; an idle
    /// Code tab does not count.
    #[gpui::test]
    fn u3d_runs_in_progress_counts_work_code(cx: &mut gpui::TestAppContext) {
        if !isolated("app_lifecycle::tests::u3d_runs_in_progress_counts_work_code") {
            return;
        }
        let app = cx.new(AwiliApp::new);
        app.update(cx, |app, cx| {
            app.work.running = true;
            for (id, streaming) in [(1, true), (2, false)] {
                app.work.code_tabs.push(crate::work::CodeSession {
                    id,
                    engine: awili_code::CodeEngine::Claude,
                    route: crate::conversation_route::ConversationRoute::new(
                        crate::work::WorkExecutor::Agent {
                            engine: awili_code::CodeEngine::Claude,
                            model: None,
                        },
                    ),
                    convo: Default::default(),
                    reply_presentation_token: crate::conv_view::TurnToken::fresh(),
                    rows: Vec::new(),
                    errored: Default::default(),
                    streaming,
                    context_gap: false,
                    session_slot: Default::default(),
                    project_folder: None,
                    model: None,
                    scroll: gpui::ScrollHandle::new(),
                    retry_focus: None,
                    retry_reveal_key: std::rc::Rc::default(),
                    retry_bounds: std::rc::Rc::new(std::cell::Cell::new(None)),
                    stream_abort_tx: None,
                    history: Default::default(),
                });
            }
            assert_eq!(app.runs_in_progress(cx), 2);
            let (msg, _) = quit_prompt(app.runs_in_progress(cx)).expect("a confirmation");
            assert_eq!(msg, "2 runs in progress — quit anyway?");
        });
    }

    /// D7: ⌘W, dispatched through the app's real menu actions, hides Awili;
    /// the window (and the runs it owns) survives.
    #[gpui::test]
    fn u3d_close_window_action_hides_not_quits(cx: &mut gpui::TestAppContext) {
        if !isolated("app_lifecycle::tests::u3d_close_window_action_hides_not_quits") {
            return;
        }
        cx.update(crate::install_native_menu_actions);
        let window = cx.add_window(|_, cx| AwiliApp::new(cx));
        cx.update(|cx| cx.set_global(crate::AwiliWindow(window)));
        cx.dispatch_action(window.into(), native_menu::CloseWindow);
        cx.run_until_parked();
        assert_eq!(HIDES.with(|n| n.get()), 1, "⌘W hides Awili");
        assert_eq!(cx.windows().len(), 1, "the window is not destroyed");
        assert!(window.update(cx, |_, _, _| ()).is_ok(), "and still updates");
    }

    struct Blank;

    impl gpui::Render for Blank {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    /// D7: the red button's should-close callback hides Awili and returns
    /// `false`, so the window survives.
    #[gpui::test]
    fn u3d_red_button_should_close_returns_false(cx: &mut gpui::TestAppContext) {
        let (_view, window) = cx.add_window_view(|window, cx| {
            hide_on_close(window, cx);
            Blank
        });
        assert!(!window.simulate_close(), "should-close returns false");
        assert_eq!(HIDES.with(|n| n.get()), 1, "the red button hides Awili");
        assert_eq!(window.windows().len(), 1, "the window survives");
        // `main` registers it on the real window.
        let main = include_str!("main.rs");
        assert!(main.contains("app_lifecycle::hide_on_close(window, cx);"));
    }

    /// K02 (review P1): Help › Keyboard Shortcuts while focus sits on a handle
    /// that is never rendered; Escape must still close the sheet.
    #[gpui::test]
    fn u3d_escape_closes_shortcuts_with_stale_focus(cx: &mut gpui::TestAppContext) {
        if !isolated("app_lifecycle::tests::u3d_escape_closes_shortcuts_with_stale_focus") {
            return;
        }
        cx.update(crate::install_native_menu_actions);
        let (app, window) = cx.add_window_view(|_, cx| AwiliApp::new(cx));
        // Past onboarding (its welcome panel would take focus on close).
        app.update(window, |a, cx| {
            a.onboarding = crate::onboarding::Onboarding::existing();
            cx.notify();
        });
        window.run_until_parked();
        window.update(|window, cx| {
            let handle = window.window_handle().downcast::<AwiliApp>();
            cx.set_global(crate::AwiliWindow(handle.expect("an Awili window")));
            // A focus handle no element renders: the K02 stale focus.
            let stale = cx.focus_handle();
            window.focus(&stale, cx);
        });
        window.run_until_parked();
        window.dispatch_action(native_menu::ShowShortcuts);
        window.run_until_parked();
        assert!(
            app.read_with(window, |a, _| a.shortcuts.open),
            "the menu opened the sheet"
        );
        window.simulate_keystrokes("escape");
        assert!(
            !app.read_with(window, |a, _| a.shortcuts.open),
            "Escape closed it"
        );
        let composer = app.read_with(window, |a, cx| a.composer.focus_handle(cx));
        assert!(
            window.update(|window, _| composer.is_focused(window)),
            "composer refocused"
        );
        // The menu path closes it too, and gives the composer focus back.
        window.dispatch_action(native_menu::ShowShortcuts);
        window.run_until_parked();
        window.dispatch_action(native_menu::ShowShortcuts);
        window.run_until_parked();
        assert!(!app.read_with(window, |a, _| a.shortcuts.open));
        assert!(window.update(|window, _| composer.is_focused(window)));
    }

    /// Review P2: the three U3d rows the sheet was missing.
    #[test]
    fn u3d_shortcuts_sheet_lists_navigation_deletion_and_context_menu() {
        use crate::shortcuts::SHORTCUTS;
        let has = |combo: &str, action: &str| SHORTCUTS.contains(&(combo, action));
        assert!(has("⌥← ⌥→  ⌘← ⌘→", "Word / line jump — add ⇧ to select"));
        assert!(has("⌥⌫ / ⌘⌫", "Delete word / to line start"));
        assert!(has(
            "Right-click or ⌃-click",
            "Cut, Copy, Paste, Select All"
        ));
    }

    /// RC2-R61 (usability RC2 round 5, Minor 1: after ⌘Q "Quit Anyway" a
    /// Claude Work turn came back as "⚠ the coding engine was terminated",
    /// footer "Failed"): quitting stops the running turn as a Stop does; its
    /// words stay, it says the quit stopped it, and nothing reads as failed.
    #[gpui::test]
    fn rc2r61_a_turn_stopped_by_quitting_reads_as_stopped(cx: &mut gpui::TestAppContext) {
        if !isolated("app_lifecycle::tests::rc2r61_a_turn_stopped_by_quitting_reads_as_stopped") {
            return;
        }
        use awili_core::{ConversationState, Role};
        let app = cx.new(AwiliApp::new);
        let (abort_tx, abort_rx) = std::sync::mpsc::channel::<()>();
        app.update(cx, |app, _| {
            app.work.executor = crate::work::WorkExecutor::Agent {
                engine: awili_code::CodeEngine::Claude,
                model: None,
            };
            app.views.insert(
                "quit-conv".into(),
                crate::conv_view::ConvView::from_state(ConversationState::seeded([
                    (Role::User, "rename alpha.txt".to_string()),
                    (Role::Assistant, "I found alpha.txt and".to_string()),
                ])),
            );
            app.work.conv_id = Some("quit-conv".into());
            app.work.stream_row_idx = Some(1);
            app.work.running = true;
            app.work.agent_abort_tx = Some(abort_tx);
            // The quit hook waits for the persistence worker's answer, which
            // wakes this test's scheduler from another thread (gpui calls
            // that non-deterministic); this test checks what the quit leaves
            // on screen, so it runs without the database.
            app.persist = None;
        });
        assert_eq!(app.read_with(cx, |a, cx| a.runs_in_progress(cx)), 1);
        cx.update(|cx| cx.shutdown());
        assert!(abort_rx.try_recv().is_ok(), "the turn was stopped");
        app.read_with(cx, |app, _| {
            assert!(!app.work.running);
            assert_eq!(
                app.work.panel.status,
                Some(awili_work::run_events::RunStatus::Stopped)
            );
            assert!(app.work.panel.error.is_none(), "{:?}", app.work.panel.error);
            let reply = app.views["quit-conv"]
                .convo
                .message(1)
                .unwrap()
                .text
                .clone();
            assert_eq!(
                reply,
                format!("I found alpha.txt and\n\n{QUIT_STOPPED_LINE}")
            );
            assert!(!reply.contains("terminated"), "{reply}");
        });
    }
}
