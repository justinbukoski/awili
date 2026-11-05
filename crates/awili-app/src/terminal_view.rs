//! TerminalView — a gpui Entity that wraps a terminal with focus + keyboard input.
//!
//! Renders the TerminalElement inside a focusable div and routes keystrokes
//! to the PTY via `write_input`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::{
    div, prelude::*, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    PromptButton, PromptLevel, Window,
};

use crate::terminal::{ChannelListener, PastePlan, TerminalId, TerminalState, TerminalStatus};
use crate::terminal_element::TerminalElement;
use crate::terminal_select::{self, CopyKeyAction};
use crate::{theme, ui};

#[allow(dead_code)] // the pane's close event lands with the pane (Phase 5)
pub enum TerminalViewEvent {
    Closed,
}

pub struct TerminalView {
    pub terminal: TerminalState,
    focus: FocusHandle,
    /// When true, the next render grabs keyboard focus once — so a freshly
    /// opened Code terminal is typeable immediately, no click required.
    needs_focus: bool,
    /// RC1-F-SLICE-FIX1a: a held multi-line paste is waiting for its answer
    /// (a second ⌘V meanwhile does nothing; gpui allows one prompt at a time).
    asking_to_paste: bool,
}

/// RC1-F-SLICE-FIX1a: the plain question for a held paste of `lines` lines.
pub(crate) fn paste_question(lines: usize) -> (String, &'static str) {
    if lines == 1 {
        (
            "Paste 1 line into the terminal?".to_string(),
            "It will run as a command. Return pastes, Esc cancels.",
        )
    } else {
        (
            format!("Paste {lines} lines into the terminal?"),
            "Each line will run as a command. Return pastes, Esc cancels.",
        )
    }
}

// The view's entry points land with the terminal pane (Phase 5).
#[allow(dead_code)]
impl TerminalView {
    pub fn new(id: TerminalId, cwd: PathBuf, cx: &mut Context<Self>) -> Option<Self> {
        let terminal = TerminalState::spawn(id, cwd, 80, 24).ok()?;
        Some(Self {
            terminal,
            focus: cx.focus_handle(),
            needs_focus: false,
            asking_to_paste: false,
        })
    }

    /// Create a TerminalView Entity from a parent context. Returns None if PTY spawn fails.
    pub fn try_new<T: 'static>(
        id: TerminalId,
        cwd: PathBuf,
        cx: &mut Context<T>,
    ) -> Option<Entity<Self>> {
        let terminal = TerminalState::spawn(id, cwd, 80, 24).ok()?;
        Some(cx.new(|cx| Self {
            terminal,
            focus: cx.focus_handle(),
            needs_focus: false,
            asking_to_paste: false,
        }))
    }

    /// Create a TerminalView running a specific program (e.g. a coding-agent
    /// CLI such as `claude` or `codex`) in `cwd`. Returns None if PTY spawn fails.
    pub fn try_new_command<T: 'static>(
        id: TerminalId,
        cwd: PathBuf,
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cx: &mut Context<T>,
    ) -> Option<Entity<Self>> {
        let terminal = TerminalState::spawn_program(id, cwd, &program, &args, &env, 80, 24).ok()?;
        let entity = cx.new(|cx| Self {
            terminal,
            focus: cx.focus_handle(),
            needs_focus: true,
            asking_to_paste: false,
        });
        // The child writes output on its own schedule, so drive repaints from a
        // ticker: poll the PTY + notify ~20×/s until it exits. Without this the
        // screen only updates on user input.
        entity.update(cx, |_this, cx| {
            cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
                let keep = this.update(cx, |this, cx| {
                    this.terminal.poll();
                    cx.notify();
                    this.terminal.status == TerminalStatus::Running
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            })
            .detach();
        });
        Some(entity)
    }

    /// RC1-F-SLICE-FIX1a: one native question (Return = Paste, the default
    /// button; Esc = Cancel). Paste sends the lines; Cancel or closing the
    /// question sends nothing.
    fn ask_before_paste(
        &mut self,
        text: String,
        lines: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (message, detail) = paste_question(lines);
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(detail),
            &[
                PromptButton::new("Paste"),
                PromptButton::Cancel("Cancel".into()),
            ],
            cx,
        );
        self.asking_to_paste = true;
        cx.spawn(async move |this, cx| {
            let paste = matches!(answer.await, Ok(0));
            this.update(cx, |this, _| {
                this.asking_to_paste = false;
                if paste {
                    this.terminal.paste_confirmed(&text);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn term_arc(&self) -> Arc<Mutex<alacritty_terminal::term::Term<ChannelListener>>> {
        Arc::clone(&self.terminal.term)
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;

        // Paste.
        // RC2-APP (F0255): ⌘V (and Ctrl+Shift+V) paste; a plain Ctrl+V is
        // the program's own key (0x16, e.g. "insert the next key literally").
        if is_paste_key(ks) {
            if self.asking_to_paste {
                return;
            }
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                // RC1-F2c (F0254): bracketed, so pasted lines never run;
                // RC1-F-SLICE-FIX1a: otherwise a multi-line paste asks first.
                if let PastePlan::AskFirst { lines } = self.terminal.paste(&text) {
                    self.ask_before_paste(text, lines, window, cx);
                }
            }
            return;
        }

        // Copy vs interrupt. On macOS, Cmd+C is clipboard copy and Ctrl+C is
        // the process interrupt — these must not be conflated (the old branch
        // sent ETX for both, so Cmd+C interrupted the running program). The
        // decision lives in a pure, tested kernel; the selection is a peek so
        // the copy path is correct even before the mouse-drag slice sets one.
        if ks.key == "c" {
            // Peek the selection once: classify and copy off the same snapshot,
            // so a reader-thread selection change between locks can't make us
            // classify "copy" then copy nothing.
            let selection = self.terminal.selection_text();
            match terminal_select::classify_copy_key(
                ks.modifiers.platform,
                ks.modifiers.control,
                ks.modifiers.shift,
                selection.is_some(),
            ) {
                Some(CopyKeyAction::CopySelection) => {
                    if let Some(text) = selection {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        self.terminal.clear_selection();
                        cx.notify();
                    }
                    return;
                }
                Some(CopyKeyAction::Interrupt) => {
                    self.terminal.write_input(&[0x03]);
                    return;
                }
                Some(CopyKeyAction::Ignore) => return,
                None => {} // fall through to normal key handling
            }
        }

        if let Some(bytes) = keystroke_to_bytes(ks) {
            self.terminal.write_input(&bytes);
        }

        cx.notify();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

/// R1h: the test lookup key prefix of the terminal's status (the word follows).
pub(crate) const TERMINAL_STATUS_SELECTOR: &str = "r1h-terminal-";

/// The key context of a focused terminal. App-wide bindings on Windows and Linux are
/// scoped `!Terminal` (see `app_lifecycle::standard_bindings`), so a shell gets raw
/// Ctrl chords (Ctrl+W, Ctrl+K, Ctrl+N, Ctrl+Y and Ctrl+M are readline keys).
pub(crate) const KEY_CONTEXT: &str = "Terminal";

/// The focusable shell of a terminal, with its key context. One function so the
/// real view and the test that proves the scoping cannot drift apart.
pub(crate) fn terminal_shell() -> gpui::Stateful<gpui::Div> {
    div().id("terminal-view").key_context(KEY_CONTEXT)
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.terminal.poll();

        // Grab focus once when a fresh Code terminal first paints, so it is
        // typeable without a click.
        if self.needs_focus {
            window.focus(&self.focus, cx);
            self.needs_focus = false;
        }

        // R1h: the shell's state, in words that cannot be read as "the
        // agent is still working".
        let status_text = match self.terminal.status {
            TerminalStatus::Running => "open",
            TerminalStatus::Exited(_) => "closed",
        };

        // RC2-RUN (round 3 Minor 17): the grid fits the card's width.
        let term_el =
            TerminalElement::new(Arc::clone(&self.terminal.term)).fitted(self.terminal.fitter());

        let running = self.terminal.status == TerminalStatus::Running;
        let body = terminal_shell()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.on_key(ev, window, cx);
            }))
            .on_click(cx.listener(|this, _ev, window, cx| {
                window.focus(&this.focus, cx);
                cx.notify();
            }))
            .flex()
            .flex_col()
            // Chrome and decoration scale with the interface zoom (A4); only
            // the grid itself stays font-metric driven via rem_size.
            .gap(theme::z(theme::SP_4))
            .child(
                div()
                    .debug_selector(|| format!("{TERMINAL_STATUS_SELECTOR}{status_text}"))
                    .child(
                        ui::label(format!(
                            "Shell {status_text}. Stop interrupts the foreground command."
                        ))
                        .dim()
                        .wrap(),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "awili-terminal-grid-viewport".into())
                    .w_full()
                    // Cell fitting, glyph advances and ANSI backgrounds share
                    // one terminal font; kit labels retain their UI font.
                    .font_family(theme::MONO_FONT)
                    .font_weight(gpui::FontWeight::NORMAL)
                    .not_italic()
                    .overflow_hidden()
                    .bg(theme::palette().canvas)
                    .rounded(theme::z(theme::RADIUS_M))
                    .p(theme::z(theme::SP_4))
                    .border_1()
                    .border_color(theme::palette().line)
                    .child(term_el),
            );
        let mut frame = ui::TerminalFrame::new("job-terminal")
            .title(self.terminal.cwd.display().to_string())
            .body(move |_| body)
            .on_clear(cx.listener(|this, _, _, cx| {
                this.terminal.clear_display();
                cx.notify();
            }));
        if running {
            frame = frame
                .running(theme::Tone::Neutral)
                .on_stop(cx.listener(|this, _, _, cx| {
                    // The same Ctrl+C the existing terminal key handler sends.
                    this.terminal.write_input(&[3]);
                    cx.notify();
                }));
        }
        frame
    }
}

/// Convert a gpui Keystroke into terminal bytes.
/// RC2-APP (F0255): the clipboard paste keys — ⌘V, and Ctrl+Shift+V (the
/// usual terminal paste where there is no ⌘). A plain Ctrl+V is not one:
/// it reaches the program as 0x16.
fn is_paste_key(ks: &gpui::Keystroke) -> bool {
    let m = &ks.modifiers;
    ks.key == "v" && ((m.platform && !m.control) || (m.control && m.shift && !m.platform))
}

fn keystroke_to_bytes(ks: &gpui::Keystroke) -> Option<Vec<u8>> {
    // Control key combos (Ctrl+A through Ctrl+Z).
    if ks.modifiers.control && !ks.modifiers.platform && ks.key.len() == 1 {
        let ch = ks.key.chars().next()?;
        if ch.is_ascii_alphabetic() {
            let ctrl = (ch.to_ascii_uppercase() as u8) - b'A' + 1;
            return Some(vec![ctrl]);
        }
    }

    // Named keys.
    match ks.key.as_str() {
        "enter" | "return" => return Some(vec![b'\r']),
        "tab" => return Some(vec![b'\t']),
        "escape" => return Some(vec![0x1b]),
        "backspace" => return Some(vec![0x7f]),
        "delete" => return Some(b"\x1b[3~".to_vec()),
        "up" => return Some(b"\x1b[A".to_vec()),
        "down" => return Some(b"\x1b[B".to_vec()),
        "right" => return Some(b"\x1b[C".to_vec()),
        "left" => return Some(b"\x1b[D".to_vec()),
        "home" => return Some(b"\x1b[H".to_vec()),
        "end" => return Some(b"\x1b[F".to_vec()),
        "pageup" => return Some(b"\x1b[5~".to_vec()),
        "pagedown" => return Some(b"\x1b[6~".to_vec()),
        "f1" => return Some(b"\x1bOP".to_vec()),
        "f2" => return Some(b"\x1bOQ".to_vec()),
        "f3" => return Some(b"\x1bOR".to_vec()),
        "f4" => return Some(b"\x1bOS".to_vec()),
        "f5" => return Some(b"\x1b[15~".to_vec()),
        "f6" => return Some(b"\x1b[17~".to_vec()),
        "f7" => return Some(b"\x1b[18~".to_vec()),
        "f8" => return Some(b"\x1b[19~".to_vec()),
        "f9" => return Some(b"\x1b[20~".to_vec()),
        "f10" => return Some(b"\x1b[21~".to_vec()),
        "f11" => return Some(b"\x1b[23~".to_vec()),
        "f12" => return Some(b"\x1b[24~".to_vec()),
        "space" => return Some(vec![b' ']),
        _ => {}
    }

    // GPUI normalizes the binding key; key_char retains Shift, Caps Lock and
    // composed text as entered by the user.
    if !ks.modifiers.platform && !ks.modifiers.control {
        if let Some(text) = ks
            .key_char
            .as_ref()
            .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        {
            return Some(text.as_bytes().to_vec());
        }
    }

    // Regular printable characters.
    if ks.key.len() == 1 && !ks.modifiers.platform && !ks.modifiers.control {
        return Some(ks.key.as_bytes().to_vec());
    }

    // Multi-byte unicode.
    if !ks.key.is_empty() && !ks.modifiers.platform && !ks.modifiers.control {
        let key = &ks.key;
        if key.chars().count() == 1 && key.chars().next().is_some_and(|c| !c.is_control()) {
            return Some(key.as_bytes().to_vec());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_maps_to_cr() {
        let ks = gpui::Keystroke {
            key: "enter".into(),
            modifiers: Default::default(),
            key_char: None,
        };
        assert_eq!(keystroke_to_bytes(&ks), Some(vec![b'\r']));
    }

    /// RC2-APP (F0255): ⌘V and Ctrl+Shift+V paste; a plain Ctrl+V goes to
    /// the program as 0x16.
    #[test]
    fn rc2app_ctrl_v_reaches_the_program() {
        let key = |platform: bool, control: bool, shift: bool| gpui::Keystroke {
            key: "v".into(),
            modifiers: gpui::Modifiers {
                platform,
                control,
                shift,
                ..Default::default()
            },
            key_char: None,
        };
        assert!(is_paste_key(&key(true, false, false)), "⌘V pastes");
        assert!(is_paste_key(&key(false, true, true)), "Ctrl+Shift+V pastes");
        let ctrl_v = key(false, true, false);
        assert!(!is_paste_key(&ctrl_v), "Ctrl+V is the program's");
        assert_eq!(keystroke_to_bytes(&ctrl_v), Some(vec![0x16]));
    }

    #[test]
    fn ctrl_c_maps_to_etx() {
        let ks = gpui::Keystroke {
            key: "c".into(),
            modifiers: gpui::Modifiers {
                control: true,
                ..Default::default()
            },
            key_char: None,
        };
        assert_eq!(keystroke_to_bytes(&ks), Some(vec![0x03]));
    }

    #[test]
    fn arrow_keys_produce_escape_sequences() {
        let up = gpui::Keystroke {
            key: "up".into(),
            modifiers: Default::default(),
            key_char: None,
        };
        assert_eq!(keystroke_to_bytes(&up), Some(b"\x1b[A".to_vec()));
    }

    #[test]
    fn shifted_and_caps_text_preserves_uppercase() {
        for shift in [false, true] {
            let ks = gpui::Keystroke {
                key: "a".into(),
                key_char: Some("A".into()),
                modifiers: gpui::Modifiers {
                    shift,
                    ..Default::default()
                },
            };
            assert_eq!(keystroke_to_bytes(&ks), Some(b"A".to_vec()));
        }
    }

    #[test]
    fn regular_char() {
        let ks = gpui::Keystroke {
            key: "a".into(),
            modifiers: Default::default(),
            key_char: None,
        };
        assert_eq!(keystroke_to_bytes(&ks), Some(vec![b'a']));
    }

    #[test]
    fn cmd_letter_produces_no_bytes() {
        // On macOS a Cmd+letter is a menu/app shortcut (Cmd+A select-all,
        // Cmd+W close, …), NOT terminal input — the printable-char arm must
        // exclude the platform modifier or the terminal receives a stray byte.
        // Cmd+V and Cmd+C are intercepted earlier in on_key; every other
        // Cmd+letter reaches keystroke_to_bytes and must map to None.
        let ks = gpui::Keystroke {
            key: "a".into(),
            modifiers: gpui::Modifiers {
                platform: true,
                ..Default::default()
            },
            key_char: None,
        };
        assert_eq!(keystroke_to_bytes(&ks), None);
    }

    // ---- RC1-F-SLICE-FIX1a: a multi-line paste without brackets asks first ----

    /// A terminal view running a child that never turns on bracketed paste
    /// and records every byte it receives into `out`.
    fn recording_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        out: &std::path::Path,
    ) -> (gpui::Entity<TerminalView>, &'a mut gpui::VisualTestContext) {
        let script = format!(
            "stty raw -echo; printf 'READY\\n'; cat > '{}'",
            out.display()
        );
        let dir = out.parent().unwrap().to_path_buf();
        let (view, window) = cx.add_window_view(|_, cx| TerminalView {
            terminal: TerminalState::spawn_program(
                TerminalId(77),
                dir,
                "/bin/sh",
                &["-c".to_string(), script],
                &[],
                80,
                24,
            )
            .unwrap(),
            focus: cx.focus_handle(),
            needs_focus: true,
            asking_to_paste: false,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !view.update(window, |v, _| {
            v.terminal.poll();
            v.terminal.screen_text().contains("READY")
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "child never got ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        window.run_until_parked();
        (view, window)
    }

    fn received(out: &std::path::Path, want_len: usize) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let got = std::fs::read_to_string(out).unwrap_or_default();
            if got.len() >= want_len || std::time::Instant::now() > deadline {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[gpui::test]
    fn terminal_grid_font_and_ansi_runs_match_cells_under_ui_parent(cx: &mut gpui::TestAppContext) {
        if !crate::consolidation_tests::isolated(
            "terminal_view::tests::terminal_grid_font_and_ansi_runs_match_cells_under_ui_parent",
        ) {
            return;
        }
        use crate::terminal_element::font_probe;
        use alacritty_terminal::vte::ansi::Processor;
        struct Parent {
            terminal: Entity<TerminalView>,
        }
        impl Render for Parent {
            fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                window.set_rem_size(gpui::px(16.0 * theme::ui_scale()));
                // Deliberately incompatible chrome inheritance. The real
                // TerminalView must bind only its grid to a normal mono face.
                div()
                    .font_family(".SystemUIFont")
                    .font_weight(gpui::FontWeight::BOLD)
                    .italic()
                    .child(ui::label("Terminal chrome remains UI text"))
                    .child(self.terminal.clone())
            }
        }
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let (parent, window) = cx.add_window_view(|_, cx| {
            let terminal = cx.new(|cx| TerminalView {
                terminal: TerminalState::spawn_program(
                    TerminalId(79),
                    dir.path().into(),
                    "/bin/sh",
                    &[
                        "-c".into(),
                        "stty raw -echo; printf READY; cat >/dev/null".into(),
                    ],
                    &[],
                    80,
                    24,
                )
                .unwrap(),
                focus: cx.focus_handle(),
                needs_focus: false,
                asking_to_paste: false,
            });
            Parent { terminal }
        });
        let terminal = parent.read_with(window, |p, _| p.terminal.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !terminal.update(window, |v, _| {
            v.terminal.poll();
            v.terminal.screen_text().contains("READY")
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "owned child never ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for scale in [0.9_f32, 1.25] {
            theme::set_ui_scale(scale);
            font_probe::begin();
            terminal.update(window, |v, cx| {
                let mut term = v.terminal.term.lock().unwrap();
                // Adjacent normal/bold/normal cells have identical foreground
                // colors, so incorrect run coalescing cannot hide in color changes.
                let mut processor: Processor = Processor::new();
                processor.advance(
                    &mut *term,
                    b"\x1b[2J\x1b[Hi\x1b[1mM\x1b[22m iM\x1b[3;10H\x1b[41mX\x1b[0m",
                );
                drop(term);
                cx.notify();
            });
            window.simulate_resize(gpui::size(gpui::px(860.), gpui::px(700.)));
            window.update(|window, cx| window.draw(cx).clear());
            window.run_until_parked();
            let samples = font_probe::take();
            assert!(
                samples.iter().any(|s| s.phase == "layout"),
                "actual layout observed"
            );
            let row = samples
                .iter()
                .rev()
                .find(|s| s.text.starts_with("iM iM"))
                .expect("actual shaped row observed");
            for s in &samples {
                assert_eq!(
                    s.font.family.as_ref(),
                    theme::MONO_FONT,
                    "effective {} font at scale{scale}",
                    s.phase
                );
                assert_eq!(s.font.weight, gpui::FontWeight::NORMAL);
                assert_eq!(s.font.style, gpui::FontStyle::Normal);
            }
            assert_eq!(row.runs[0].0, 1, "normal cell keeps its own font run");
            assert_eq!(row.runs[0].1.weight, gpui::FontWeight::NORMAL);
            assert_eq!(row.runs[1].0, 1);
            assert_eq!(row.runs[1].1.weight, gpui::FontWeight::BOLD);
            assert_eq!(row.runs[2].1.weight, gpui::FontWeight::NORMAL);
            assert!(row
                .runs
                .iter()
                .all(|(_, f)| f.family.as_ref() == theme::MONO_FONT));
            for i in 0..=5 {
                assert!(
                    (f32::from(row.positions[i] - row.cell_width * i as f32)).abs() < 0.1,
                    "ASCII spaces/i/M share fixed cells: column{i}, scale{scale}"
                );
            }
            let colored = samples
                .iter()
                .rev()
                .find(|s| s.text.trim() == "X")
                .expect("actual colored row observed");
            assert_eq!(colored.backgrounds, vec![(9, 10)]);
            assert!((f32::from(colored.positions[9] - colored.cell_width * 9.)).abs() < 0.1);
            assert!((f32::from(colored.positions[10] - colored.cell_width * 10.)).abs() < 0.1);
            let cols = terminal.read_with(window, |v, _| {
                use alacritty_terminal::grid::Dimensions;
                v.terminal.term.lock().unwrap().grid().columns()
            });
            assert!(cols >= 2, "real fitted terminal remains usable");
        }
        theme::set_ui_scale(1.0);
    }

    #[gpui::test]
    fn job_interiors_terminal_actions_use_the_owned_pty(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("bytes");
        let (view, window) = recording_view(cx, &out);
        let modes = view.read_with(window, |v, _| *v.terminal.term.lock().unwrap().mode());
        let clear = window
            .debug_bounds("ui-icon-button-job-terminal-clear")
            .unwrap();
        window.simulate_click(clear.center(), gpui::Modifiers::default());
        window.run_until_parked();
        assert!(!view
            .read_with(window, |v, _| v.terminal.screen_text())
            .contains("READY"));
        assert_eq!(
            view.read_with(window, |v, _| *v.terminal.term.lock().unwrap().mode()),
            modes
        );
        assert!(
            std::fs::read(&out).unwrap_or_default().is_empty(),
            "Clear sends no PTY input"
        );
        let stop = window
            .debug_bounds("ui-icon-button-job-terminal-stop")
            .unwrap();
        window.simulate_click(stop.center(), gpui::Modifiers::default());
        window.run_until_parked();
        assert_eq!(
            received(&out, 1).as_bytes(),
            &[3],
            "Stop is the existing Ctrl+C byte"
        );
    }

    #[gpui::test]
    fn terminal_clear_preserves_live_relative_redraw(cx: &mut gpui::TestAppContext) {
        use alacritty_terminal::index::{Column, Line};
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("trigger");
        // The real child owns a nonzero cursor and waits for input before
        // using a relative redraw. A local Clear must not move that cursor.
        let script = format!(
            "stty raw -echo; printf '\\033[4;10HREADY'; \
             dd bs=1 count=1 of='{}' 2>/dev/null; \
             printf '\\033[2DXY'; cat >> '{}'",
            out.display(),
            out.display()
        );
        let (view, window) = cx.add_window_view(|_, cx| TerminalView {
            terminal: TerminalState::spawn_program(
                TerminalId(78),
                dir.path().into(),
                "/bin/sh",
                &["-c".to_string(), script],
                &[],
                80,
                24,
            )
            .unwrap(),
            focus: cx.focus_handle(),
            needs_focus: true,
            asking_to_paste: false,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !view.update(window, |v, _| {
            v.terminal.poll();
            v.terminal.screen_text().contains("READY")
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "child never reached its output latch"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        window.run_until_parked();
        let cursor = view.read_with(window, |v, _| {
            v.terminal.term.lock().unwrap().grid().cursor.clone()
        });
        assert_eq!(cursor.point.line, Line(3));
        assert_eq!(cursor.point.column, Column(14));
        let clear = window
            .debug_bounds("ui-icon-button-job-terminal-clear")
            .unwrap();
        window.simulate_click(clear.center(), gpui::Modifiers::default());
        window.run_until_parked();
        assert!(view
            .read_with(window, |v, _| v.terminal.screen_text())
            .trim()
            .is_empty());
        assert!(
            std::fs::read(&out).unwrap_or_default().is_empty(),
            "Clear sends no child input"
        );
        window.update(|win, cx| view.update(cx, |v, cx| win.focus(&v.focus, cx)));
        window.simulate_keystrokes("x");
        assert_eq!(
            received(&out, 1),
            "x",
            "normal terminal key input releases the child"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !view.update(window, |v, _| {
            v.terminal.poll();
            v.terminal.screen_text().contains("XY")
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "relative child redraw never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        view.read_with(window, |v, _| {
            let term = v.terminal.term.lock().unwrap();
            assert_eq!(
                term.grid()[Line(3)][Column(12)].c,
                'X',
                "relative output stays at the child's owned row/column"
            );
            assert_eq!(term.grid()[Line(3)][Column(13)].c, 'Y');
            assert_eq!(
                term.grid()[Line(0)][Column(0)].c,
                ' ',
                "local Clear does not secretly relocate the child"
            );
            assert_eq!(term.grid().cursor.point, cursor.point);
            assert_eq!(term.grid().cursor.input_needs_wrap, cursor.input_needs_wrap);
        });
    }

    #[gpui::test]
    fn terminal_clear_preserves_live_pending_wrap(cx: &mut gpui::TestAppContext) {
        use alacritty_terminal::index::{Column, Line};
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("wrap-trigger");
        let prepared = dir.path().join("wrap-prepared");
        // CUP clamps column 999 to the actual fitted last column. The child
        // fills it, then waits; its next printable byte must wrap after Clear.
        let script = format!(
            "stty raw -echo; printf 'BOOT'; \
             dd bs=1 count=1 of='{}' 2>/dev/null; printf '\\033[4;999HZ'; \
             dd bs=1 count=1 of='{}' 2>/dev/null; printf 'XY'; cat >> '{}'",
            prepared.display(),
            out.display(),
            out.display()
        );
        let (view, window) = cx.add_window_view(|_, cx| TerminalView {
            terminal: TerminalState::spawn_program(
                TerminalId(79),
                dir.path().into(),
                "/bin/sh",
                &["-c".to_string(), script],
                &[],
                80,
                24,
            )
            .unwrap(),
            focus: cx.focus_handle(),
            needs_focus: true,
            asking_to_paste: false,
        });
        // Hold the child until layout has fitted the PTY: resizing after Z
        // would clear its wrap latch and turn this into a readiness race.
        window.run_until_parked();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !view.update(window, |v, _| {
            v.terminal.poll();
            v.terminal.screen_text().contains("BOOT")
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "child never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        window.update(|win, cx| view.update(cx, |v, cx| win.focus(&v.focus, cx)));
        window.simulate_keystrokes("p");
        assert_eq!(received(&prepared, 1), "p");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !view.update(window, |v, _| {
            v.terminal.poll();
            let term = v.terminal.term.lock().unwrap();
            term.grid().cursor.point.line == Line(3) && term.grid().cursor.input_needs_wrap
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "child never reached the pending-wrap latch"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let clear = window
            .debug_bounds("ui-icon-button-job-terminal-clear")
            .unwrap();
        window.simulate_click(clear.center(), gpui::Modifiers::default());
        window.run_until_parked();
        assert!(view
            .read_with(window, |v, _| v.terminal.screen_text())
            .trim()
            .is_empty());
        assert!(std::fs::read(&out).unwrap_or_default().is_empty());
        window.update(|win, cx| view.update(cx, |v, cx| win.focus(&v.focus, cx)));
        window.simulate_keystrokes("x");
        assert_eq!(received(&out, 1), "x");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !view.update(window, |v, _| {
            v.terminal.poll();
            v.terminal.screen_text().contains("XY")
        }) {
            assert!(
                std::time::Instant::now() < deadline,
                "wrapped child output never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        view.read_with(window, |v, _| {
            let term = v.terminal.term.lock().unwrap();
            assert_eq!(
                term.grid()[Line(4)][Column(0)].c,
                'X',
                "next child output honors the pending wrap"
            );
            assert_eq!(term.grid()[Line(4)][Column(1)].c, 'Y');
            assert_eq!(term.grid()[Line(0)][Column(0)].c, ' ');
        });
    }

    fn paste(window: &mut gpui::VisualTestContext, text: &str) {
        window.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_string()));
        window.simulate_keystrokes("cmd-v");
        window.run_until_parked();
    }

    /// Slice review P1 (ad1907ca): ⌘V of two lines into a program without
    /// bracketed paste asks once, in plain words; Cancel sends nothing; Paste
    /// sends the lines; a single line goes at once with no question.
    #[gpui::test]
    fn rc1f_slice_fix1a_view_asks_before_a_multi_line_paste(cx: &mut gpui::TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("got");
        let (_view, window) = recording_view(cx, &out);

        paste(window, "echo A\necho B\n");
        assert!(window.has_pending_prompt(), "a multi-line paste asks first");
        assert_eq!(
            window.pending_prompt(),
            Some((
                "Paste 2 lines into the terminal?".to_string(),
                "Each line will run as a command. Return pastes, Esc cancels.".to_string()
            ))
        );
        // A second ⌘V while asking does nothing (one question at a time).
        paste(window, "rm -rf x\n");
        window.simulate_prompt_answer("Cancel");
        window.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(received(&out, 0), "", "Cancel inserts nothing");

        paste(window, "echo A\necho B\n");
        assert!(window.has_pending_prompt());
        window.simulate_prompt_answer("Paste");
        window.run_until_parked();
        assert_eq!(
            received(&out, 14),
            "echo A\recho B\r",
            "Paste sends the lines"
        );

        paste(window, "echo C");
        assert!(!window.has_pending_prompt(), "a single line never asks");
        assert_eq!(received(&out, 20), "echo A\recho B\recho C");
    }

    /// RC2-RUN (round 3 Minor 17: `pwd` cut at the terminal card's edge): in
    /// a window narrower than 80 columns the grid takes the width it is
    /// given — every column fits — and the shell is told the new width
    /// (`stty size` prints it), so long lines wrap instead of being cut.
    #[gpui::test]
    fn rc2_run_the_terminal_fits_its_card(cx: &mut gpui::TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let script = "sleep 1; stty size; sleep 30".to_string();
        let (view, window) = cx.add_window_view(|_, cx| TerminalView {
            terminal: TerminalState::spawn_program(
                TerminalId(78),
                tmp.path().to_path_buf(),
                "/bin/sh",
                &["-c".to_string(), script],
                &[],
                80,
                24,
            )
            .unwrap(),
            focus: cx.focus_handle(),
            needs_focus: false,
            asking_to_paste: false,
        });
        window.simulate_resize(gpui::size(gpui::px(340.), gpui::px(700.)));
        window.run_until_parked();
        let cols = view.update(window, |v, _| v.terminal.grid_size().0);
        assert!(
            (usize::from(crate::terminal::MIN_FIT_COLS)..80).contains(&cols),
            "the grid fits a 340 px window: {cols} columns"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let text = view.update(window, |v, _| {
                v.terminal.poll();
                v.terminal.screen_text()
            });
            if text.contains(&format!("24 {cols}")) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the shell was told {cols} columns: {text:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        view.update(window, |v, _| v.terminal.write_str("\x03"));
    }

    #[test]
    fn rc1f_slice_fix1a_question_words() {
        assert_eq!(
            paste_question(1),
            (
                "Paste 1 line into the terminal?".to_string(),
                "It will run as a command. Return pastes, Esc cancels."
            )
        );
        assert_eq!(paste_question(3).0, "Paste 3 lines into the terminal?");
    }
}

/// Windows and Linux: the app-wide Ctrl bindings step aside for a focused
/// terminal, so its shell gets Ctrl+W, Ctrl+K, Ctrl+N, Ctrl+Y and Ctrl+M raw.
#[cfg(all(test, not(target_os = "macos")))]
mod key_scope_tests {
    use super::*;
    use crate::native_menu;
    #[allow(unused_imports)] // flagged unused on Linux builds only
    use gpui::{div, InteractiveElement as _, ParentElement as _, TestAppContext};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct Probe {
        focus: FocusHandle,
        in_terminal: bool,
        root_focus: FocusHandle,
        /// False once the focused element has been removed from the tree.
        inner_visible: bool,
        keys: Rc<RefCell<Vec<String>>>,
    }

    impl Render for Probe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            // What the real root does: focus of last resort when nothing is focused.
            crate::app_lifecycle::ensure_root_focus(window, cx, &self.root_focus);
            let keys = self.keys.clone();
            let shell = if self.in_terminal {
                // The very constructor the real TerminalView renders.
                terminal_shell()
            } else {
                div().id("not-a-terminal")
            };
            // The window root declares a context, as the real AwiliApp root does.
            let root = div()
                .key_context(crate::app_lifecycle::ROOT_KEY_CONTEXT)
                .track_focus(&self.root_focus);
            if !self.inner_visible {
                return root.child("the focused element is gone");
            }
            root.child(
                shell
                    .track_focus(&self.focus)
                    .on_key_down(move |ev: &KeyDownEvent, _, _| {
                        keys.borrow_mut().push(ev.keystroke.key.clone());
                    })
                    .child("probe"),
            )
        }
    }

    const CHORDS: &str = "ctrl-w ctrl-k ctrl-n ctrl-y ctrl-m";

    /// Counts how many app-level actions the bindings fired.
    fn count_actions(cx: &mut TestAppContext) -> Rc<Cell<usize>> {
        let fired = Rc::new(Cell::new(0));
        cx.update(|cx| {
            cx.bind_keys(crate::app_lifecycle::standard_bindings());
            let f = fired.clone();
            cx.on_action(move |_: &native_menu::CloseWindow, _| f.set(f.get() + 1));
            let f = fired.clone();
            cx.on_action(move |_: &native_menu::OpenCommandPalette, _| f.set(f.get() + 1));
            let f = fired.clone();
            cx.on_action(move |_: &native_menu::NewConversation, _| f.set(f.get() + 1));
            let f = fired.clone();
            cx.on_action(move |_: &native_menu::Redo, _| f.set(f.get() + 1));
            let f = fired.clone();
            cx.on_action(move |_: &native_menu::Minimize, _| f.set(f.get() + 1));
        });
        fired
    }

    fn run(in_terminal: bool, focused: bool, cx: &mut TestAppContext) -> (Vec<String>, usize) {
        let fired = count_actions(cx);
        let keys = Rc::new(RefCell::new(Vec::new()));
        let k = keys.clone();
        let (view, window) = cx.add_window_view(|_, cx| Probe {
            focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            inner_visible: true,
            in_terminal,
            keys: k,
        });
        if focused {
            window.update(|window, cx| {
                let focus = view.read(cx).focus.clone();
                window.focus(&focus, cx);
            });
        }
        window.run_until_parked();
        window.simulate_keystrokes(CHORDS);
        window.run_until_parked();
        let got = keys.borrow().clone();
        (got, fired.get())
    }

    #[gpui::test]
    fn a_focused_terminal_receives_raw_ctrl_chords(cx: &mut TestAppContext) {
        let (keys, actions) = run(true, true, cx);
        assert_eq!(
            keys,
            ["w", "k", "n", "y", "m"],
            "the shell sees every chord"
        );
        assert_eq!(actions, 0, "no app action stole one");
    }

    /// The residual risk: nothing focused (a click on empty canvas), so the dispatch
    /// path may not include the root view's context at all.
    #[gpui::test]
    fn with_nothing_focused_the_app_bindings_still_fire(cx: &mut TestAppContext) {
        let (keys, actions) = run(false, false, cx);
        assert_eq!(
            actions, 5,
            "Ctrl+W/K/N/Y/M still run their app actions (shell saw {keys:?})"
        );
    }

    /// A live focus handle whose element left the tree leaves the path empty too.
    #[gpui::test]
    fn a_removed_focused_element_does_not_kill_the_app_bindings(cx: &mut TestAppContext) {
        let fired = count_actions(cx);
        let keys = Rc::new(RefCell::new(Vec::new()));
        let k = keys.clone();
        let (view, window) = cx.add_window_view(|_, cx| Probe {
            focus: cx.focus_handle(),
            root_focus: cx.focus_handle(),
            inner_visible: true,
            in_terminal: false,
            keys: k,
        });
        window.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            window.focus(&focus, cx);
        });
        window.run_until_parked();
        // The focused element leaves the tree; its handle is still live.
        window.update(|_, cx| {
            view.update(cx, |probe, cx| {
                probe.inner_visible = false;
                cx.notify();
            })
        });
        window.run_until_parked();
        // The root takes focus at the end of the effect cycle that drew the empty
        // frame; the frame that follows carries its context.
        window.update(|window, _| window.refresh());
        window.run_until_parked();
        window.simulate_keystrokes(CHORDS);
        window.run_until_parked();
        assert_eq!(
            fired.get(),
            5,
            "app bindings fire (shell saw {:?})",
            keys.borrow()
        );
    }

    #[gpui::test]
    fn outside_a_terminal_the_app_bindings_still_fire(cx: &mut TestAppContext) {
        let (keys, actions) = run(false, true, cx);
        assert_eq!(
            actions, 5,
            "Ctrl+W/K/N/Y/M run their app actions elsewhere (shell saw {keys:?})"
        );
    }
}
