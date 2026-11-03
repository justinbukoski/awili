//! Terminal state — PTY management + ANSI parsing via alacritty_terminal.
//!
//! A background thread owns the PTY read loop, feeding bytes into
//! `alacritty_terminal::Term`. The UI thread reads the parsed screen state
//! for rendering. Agent read-back extracts screen text + cwd for
//! `terminal.read.respond`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

/// RC1-F2c (F0254): the bytes a paste of `text` sends. When the program in
/// the terminal turned on bracketed paste (zsh, bash, fish, vim do), the text
/// goes between `ESC[200~` and `ESC[201~`, so the shell inserts it and runs
/// nothing; control characters other than tab and newlines are dropped from
/// it, so a pasted `ESC[201~` cannot end the bracket early. Otherwise newlines
/// become carriage returns, as in every terminal.
pub(crate) fn paste_bytes(text: &str, bracketed: bool) -> String {
    if bracketed {
        let body: String = text
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
            .collect();
        format!("\x1b[200~{body}\x1b[201~")
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    }
}

/// RC1-F-SLICE-FIX1a (F0254): what a paste does. A program that did NOT turn
/// on bracketed paste would run every pasted line, so a paste that holds a
/// line break is held until the user says yes once ([`TerminalState::paste`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PastePlan {
    /// Sent now: bracketed, or no line break.
    Sent,
    /// Held: `lines` lines would each run; ask first.
    AskFirst { lines: usize },
}

/// RC1-F-SLICE-FIX1a: `Some(lines)` when a paste of `text` must be confirmed
/// (no bracketed paste and at least one line break), else `None`.
/// RC1-F-SLICE-FIX1a-FIX1: `lines` is counted from the bytes a confirmed paste
/// actually sends ([`paste_bytes`] without brackets, where CR LF, LF and CR all
/// become one CR): every CR is a line that runs, and text after the last CR is
/// one more line (typed, waiting for Return).
pub(crate) fn paste_needs_confirmation(text: &str, bracketed: bool) -> Option<usize> {
    if bracketed {
        return None;
    }
    let sent = paste_bytes(text, false);
    let runs = sent.matches('\r').count();
    let partial = !sent.is_empty() && !sent.ends_with('\r');
    (runs > 0).then_some(runs + usize::from(partial))
}

/// Unique terminal identifier within a workspace session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TerminalId(pub u64);

/// Terminal lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Running,
    Exited(i32),
}

/// Events sent from the PTY background thread to the UI.
pub enum TerminalEvent {
    /// New content available — UI should re-render.
    Wakeup,
    /// The shell process exited.
    ChildExit(i32),
    /// Title changed (shell sets window title via escape sequence).
    Title(String),
}

/// A simple event listener that forwards to an mpsc channel.
pub(crate) struct ChannelListener {
    pub(crate) tx: mpsc::Sender<TerminalEvent>,
}

impl EventListener for ChannelListener {
    fn send_event(&self, event: alacritty_terminal::event::Event) {
        match event {
            alacritty_terminal::event::Event::Wakeup => {
                let _ = self.tx.send(TerminalEvent::Wakeup);
            }
            alacritty_terminal::event::Event::Title(title) => {
                let _ = self.tx.send(TerminalEvent::Title(title));
            }
            alacritty_terminal::event::Event::Exit => {
                let _ = self.tx.send(TerminalEvent::ChildExit(0));
            }
            _ => {}
        }
    }
}

/// Terminal dimensions for alacritty_terminal.
struct TermDimensions {
    cols: usize,
    lines: usize,
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.cols
    }

    fn last_column(&self) -> alacritty_terminal::index::Column {
        alacritty_terminal::index::Column(self.cols.saturating_sub(1))
    }

    fn bottommost_line(&self) -> alacritty_terminal::index::Line {
        alacritty_terminal::index::Line(self.lines as i32 - 1)
    }

    fn topmost_line(&self) -> alacritty_terminal::index::Line {
        alacritty_terminal::index::Line(0)
    }
}

/// One live terminal, owned by the workspace.
pub struct TerminalState {
    // Identifiers the terminal pane paints once it ships (Phase 5).
    #[allow(dead_code)]
    pub id: TerminalId,
    #[allow(dead_code)]
    pub cwd: PathBuf,
    pub status: TerminalStatus,
    pub title: String,
    /// The parsed terminal screen state (shared with background thread).
    pub(crate) term: Arc<Mutex<Term<ChannelListener>>>,
    /// PTY master for writing input.
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// RC2-RUN (round 3 Minor 17): the PTY master, kept so the shell can be
    /// told its new size when the card is narrower than the grid.
    pty: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Event channel from the background thread.
    event_rx: mpsc::Receiver<TerminalEvent>,
    /// Keep the reader thread alive.
    _reader_handle: Option<thread::JoinHandle<()>>,
}

/// RC2-RUN (round 3 Minor 17: the Work terminal card cut long lines at its
/// edge — the grid stayed 80 columns in a card about 40 wide): resizes the
/// screen grid and the PTY together, only when the size really changes.
#[derive(Clone)]
pub(crate) struct TerminalFit {
    term: Arc<Mutex<Term<ChannelListener>>>,
    pty: Arc<Mutex<Box<dyn MasterPty + Send>>>,
}

/// The fewest columns the grid is ever fitted to.
pub(crate) const MIN_FIT_COLS: u16 = 20;

/// How many whole cells of `cell_width` fit in `width` (never fewer than
/// [`MIN_FIT_COLS`]).
pub(crate) fn fit_cols(width: f32, cell_width: f32) -> u16 {
    let cols = (width / cell_width).floor();
    if cell_width > 0.0 && cols.is_finite() && cols >= f32::from(MIN_FIT_COLS) {
        cols.min(f32::from(u16::MAX)) as u16
    } else {
        MIN_FIT_COLS
    }
}

impl TerminalFit {
    /// Make the terminal `cols` x `rows`; nothing when it already is.
    pub(crate) fn fit(&self, cols: u16, rows: u16) {
        if let Ok(mut term) = self.term.lock() {
            if term.grid().columns() == usize::from(cols)
                && term.grid().screen_lines() == usize::from(rows)
            {
                return;
            }
            term.resize(TermDimensions {
                cols: usize::from(cols),
                lines: usize::from(rows),
            });
        }
        if let Ok(pty) = self.pty.lock() {
            let _ = pty.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }
}

// Most of the surface (spawn_program/resize/render) lands with the terminal
// pane (Phase 5); `spawn` is what the workspace holds today.
#[allow(dead_code)]
impl TerminalState {
    /// Spawn a new login shell in the given cwd.
    pub fn spawn(
        id: TerminalId,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.cwd(&cwd);
        Self::from_command(id, cwd, cmd, cols, rows)
    }

    /// Spawn a specific program (e.g. a coding-agent CLI like `claude` or
    /// `codex`) in the given cwd. `args` are passed through; `env` entries are
    /// added to the child's environment.
    pub fn spawn_program(
        id: TerminalId,
        cwd: PathBuf,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        cols: u16,
        rows: u16,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut cmd = CommandBuilder::new(program);
        for a in args {
            cmd.arg(a);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.cwd(&cwd);
        Self::from_command(id, cwd, cmd, cols, rows)
    }

    /// Shared PTY + parser + reader-thread setup for any prepared command.
    fn from_command(
        id: TerminalId,
        cwd: PathBuf,
        cmd: CommandBuilder,
        cols: u16,
        rows: u16,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        // RC2-APP (F0243): the child is kept; the reader waits on it at EOF
        // so the terminal reports the program's real exit status.
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let (event_tx, event_rx) = mpsc::channel();

        let listener = ChannelListener {
            tx: event_tx.clone(),
        };
        let config = TermConfig::default();
        let dims = TermDimensions {
            cols: cols as usize,
            lines: rows as usize,
        };
        let term = Term::new(config, &dims, listener);
        let term = Arc::new(Mutex::new(term));

        let writer: Box<dyn Write + Send> = pair.master.take_writer()?;
        let writer = Arc::new(Mutex::new(writer));

        let reader = pair.master.try_clone_reader()?;
        let pty = Arc::new(Mutex::new(pair.master));

        // Background thread: read PTY output, feed to Term parser.
        let term_clone = Arc::clone(&term);
        let tx_clone = event_tx;
        let reader_handle = thread::spawn(move || {
            Self::read_loop(reader, term_clone, tx_clone, child);
        });

        Ok(Self {
            id,
            cwd,
            status: TerminalStatus::Running,
            title: String::new(),
            term,
            writer,
            pty,
            event_rx,
            _reader_handle: Some(reader_handle),
        })
    }

    /// Write input bytes to the PTY (user keystrokes).
    pub fn write_input(&self, data: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(data);
            let _ = w.flush();
        }
    }

    /// Write a string to the PTY (agent input or typed text).
    pub fn write_str(&self, s: &str) {
        self.write_input(s.as_bytes());
    }

    /// Clear the displayed screen and scrollback locally. This sends no shell
    /// command or PTY input and preserves the program's terminal modes,
    /// protocol cursor and pending-wrap state. The child still owns those
    /// coordinates; moving them locally would misplace its later redraws.
    pub(crate) fn clear_display(&self) {
        use alacritty_terminal::vte::ansi::{ClearMode, Handler};
        if let Ok(mut term) = self.term.lock() {
            term.clear_screen(ClearMode::All);
            term.clear_screen(ClearMode::Saved);
        }
    }

    /// RC1-F2c (F0254): paste clipboard text the way a terminal must
    /// ([`paste_bytes`]): bracketed when the program asked for it.
    /// RC1-F-SLICE-FIX1a: without bracketed paste, text with a line break is
    /// NOT sent; the caller asks first and then calls [`Self::paste_confirmed`].
    pub(crate) fn paste(&self, text: &str) -> PastePlan {
        let bracketed = self
            .term
            .lock()
            .map(|t| t.mode().contains(TermMode::BRACKETED_PASTE))
            .unwrap_or(false);
        if let Some(lines) = paste_needs_confirmation(text, bracketed) {
            return PastePlan::AskFirst { lines };
        }
        self.write_str(&paste_bytes(text, bracketed));
        PastePlan::Sent
    }

    /// RC1-F-SLICE-FIX1a: the user said yes to a held paste: send it as typed
    /// lines (each one runs).
    pub(crate) fn paste_confirmed(&self, text: &str) {
        self.write_str(&paste_bytes(text, false));
    }

    /// Poll for events from the PTY thread. Returns true if screen changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.event_rx.try_recv() {
            match ev {
                TerminalEvent::Wakeup => changed = true,
                TerminalEvent::ChildExit(code) => {
                    self.status = TerminalStatus::Exited(code);
                    changed = true;
                }
                TerminalEvent::Title(title) => {
                    self.title = title;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Extract the current screen text for agent read-back.
    pub fn screen_text(&self) -> String {
        let term = lock_term(&self.term);
        let grid = term.grid();
        let mut lines = Vec::new();
        for line_idx in 0..grid.screen_lines() {
            let row = &grid[alacritty_terminal::index::Line(line_idx as i32)];
            let text: String = (0..grid.columns())
                .map(|col| {
                    let cell: &Cell = &row[alacritty_terminal::index::Column(col)];
                    cell.c
                })
                .collect();
            lines.push(text.trim_end().to_string());
        }
        // Trim trailing empty lines.
        while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        lines.join("\n")
    }

    /// Resize the terminal: the screen grid and (RC2-RUN) the shell's own
    /// idea of its size, so what it prints next wraps at the new width.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.fitter().fit(cols, rows);
    }

    /// RC2-RUN (round 3 Minor 17): a handle the drawn grid uses to fit the
    /// terminal to the width it is given.
    pub(crate) fn fitter(&self) -> TerminalFit {
        TerminalFit {
            term: Arc::clone(&self.term),
            pty: Arc::clone(&self.pty),
        }
    }

    /// Get the cell grid for rendering. Returns (cols, rows, cells).
    /// Each cell is (char, fg_color, bg_color, flags).
    pub fn render_cells(&self) -> Vec<RenderCell> {
        let term = lock_term(&self.term);
        let grid = term.grid();
        let mut cells = Vec::new();
        for line_idx in 0..grid.screen_lines() {
            let row = &grid[alacritty_terminal::index::Line(line_idx as i32)];
            for col_idx in 0..grid.columns() {
                let cell: &Cell = &row[alacritty_terminal::index::Column(col_idx)];
                cells.push(RenderCell {
                    line: line_idx,
                    col: col_idx,
                    c: cell.c,
                    fg: cell.fg,
                    bg: cell.bg,
                    flags: cell.flags,
                });
            }
        }
        cells
    }

    pub fn grid_size(&self) -> (usize, usize) {
        let term = lock_term(&self.term);
        let grid = term.grid();
        (grid.columns(), grid.screen_lines())
    }

    /// The current selection's text, or `None` when there is no selection (or
    /// it spans no cells). A non-clearing peek — `Cmd+C` uses this to decide
    /// whether to copy, then calls [`clear_selection`](Self::clear_selection).
    ///
    /// Until the mouse-drag wiring lands (a later slice), `term.selection` is
    /// never set, so this is always `None` in practice — but the copy path is
    /// correct and ready the moment a selection can exist.
    pub fn selection_text(&self) -> Option<String> {
        let term = self.term.lock().ok()?;
        term.selection_to_string().filter(|s| !s.is_empty())
    }

    /// Drop any active selection (after a copy, or on click-to-deselect).
    pub fn clear_selection(&self) {
        if let Ok(mut term) = self.term.lock() {
            term.selection = None;
        }
    }

    fn read_loop(
        mut reader: Box<dyn Read + Send>,
        term: Arc<Mutex<Term<ChannelListener>>>,
        tx: mpsc::Sender<TerminalEvent>,
        mut child: Box<dyn portable_pty::Child + Send + Sync>,
    ) {
        let mut buf = [0u8; 4096];
        let mut parser = vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new();
        loop {
            match reader.read(&mut buf) {
                Ok(n) if n > 0 => {
                    parser.advance(&mut *lock_term(&term), &buf[..n]);
                    let _ = tx.send(TerminalEvent::Wakeup);
                }
                // RC2-APP (F0243): the output ended (EOF, or the error a
                // closed PTY gives on Linux): report the program's own exit
                // status, never a made-up 0.
                _ => {
                    let _ = tx.send(TerminalEvent::ChildExit(exit_code(child.wait())));
                    break;
                }
            }
        }
    }
}

/// RC2-APP (F0243): the exit code a finished program reports (`-1` when its
/// status cannot be read). A program ended by a signal reports 1.
fn exit_code(status: std::io::Result<portable_pty::ExitStatus>) -> i32 {
    status
        .map(|s| i32::try_from(s.exit_code()).unwrap_or(i32::MAX))
        .unwrap_or(-1)
}

/// RC2-APP (F0242): the terminal screen for a UI read. A panic elsewhere
/// while the screen was held must not take the UI down with it: the screen
/// is still drawn (poisoning is ignored).
pub(crate) fn lock_term<T>(term: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    term.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A single cell for rendering.
#[allow(dead_code)] // consumed by the terminal pane renderer (Phase 5)
#[derive(Clone, Debug)]
pub struct RenderCell {
    pub line: usize,
    pub col: usize,
    pub c: char,
    pub fg: alacritty_terminal::vte::ansi::Color,
    pub bg: alacritty_terminal::vte::ansi::Color,
    pub flags: alacritty_terminal::term::cell::Flags,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// RC2-APP (F0243): when the program ends, the terminal reports its own
    /// exit status (it used to report 0, or -1 on Linux, whatever it was).
    #[test]
    fn rc2app_the_terminal_reports_the_programs_exit_status() {
        let tmp = tempfile::tempdir().unwrap();
        for (script, want) in [("exit 3", 3), ("exit 0", 0)] {
            let mut term = TerminalState::spawn_program(
                TerminalId(9),
                tmp.path().to_path_buf(),
                "sh",
                &["-c".to_string(), script.to_string()],
                &[],
                80,
                24,
            )
            .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while term.status == TerminalStatus::Running {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the program never ended"
                );
                thread::sleep(Duration::from_millis(20));
                term.poll();
            }
            assert_eq!(term.status, TerminalStatus::Exited(want), "{script}");
        }
    }

    /// RC2-APP (F0242): a panic elsewhere while the screen was held does not
    /// take every later screen read down with it.
    #[test]
    fn rc2app_a_poisoned_screen_still_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let term = TerminalState::spawn_program(
            TerminalId(10),
            tmp.path().to_path_buf(),
            "sh",
            &["-c".to_string(), "sleep 5".to_string()],
            &[],
            80,
            24,
        )
        .unwrap();
        let shared = Arc::clone(&term.term);
        let _ = thread::spawn(move || {
            let _held = shared.lock().unwrap();
            panic!("a panic while the screen is held");
        })
        .join();
        assert!(term.term.is_poisoned());
        assert_eq!(term.grid_size(), (80, 24));
        assert_eq!(term.render_cells().len(), 80 * 24);
        let _ = term.screen_text();
    }

    #[test]
    fn spawn_and_read_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let term = TerminalState::spawn(TerminalId(1), tmp.path().to_path_buf(), 80, 24);
        assert!(term.is_ok(), "failed to spawn: {:?}", term.err());
        let mut term = term.unwrap();

        // Give the shell a moment to start and print its prompt.
        thread::sleep(Duration::from_millis(500));
        term.poll();

        let _text = term.screen_text();
        // The screen should have something (prompt, at minimum).
        // We can't assert exact content since it depends on the user's shell.
        assert!(
            term.status == TerminalStatus::Running,
            "terminal should be running"
        );

        // Clean up: send exit.
        term.write_str("exit\n");
        thread::sleep(Duration::from_millis(300));
        term.poll();
    }

    #[test]
    fn echo_and_readback() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term =
            TerminalState::spawn(TerminalId(2), tmp.path().to_path_buf(), 80, 24).unwrap();

        thread::sleep(Duration::from_millis(300));
        term.poll();

        term.write_str("echo AWILI_TEST_MARKER\n");
        thread::sleep(Duration::from_millis(500));
        term.poll();

        let text = term.screen_text();
        assert!(
            text.contains("AWILI_TEST_MARKER"),
            "screen should contain echoed text, got: {text}"
        );

        term.write_str("exit\n");
        thread::sleep(Duration::from_millis(200));
        term.poll();
    }

    /// RC2-RUN (round 3 Minor 17): the columns that fit a width.
    #[test]
    fn rc2_run_fit_cols() {
        assert_eq!(fit_cols(300.0, 7.5), 40);
        assert_eq!(fit_cols(303.0, 7.5), 40);
        assert_eq!(fit_cols(50.0, 7.5), MIN_FIT_COLS);
        assert_eq!(fit_cols(300.0, 0.0), MIN_FIT_COLS);
        assert_eq!(fit_cols(f32::NAN, 7.5), MIN_FIT_COLS);
    }

    #[test]
    fn grid_size_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let term = TerminalState::spawn(TerminalId(3), tmp.path().to_path_buf(), 100, 30).unwrap();

        let (cols, lines) = term.grid_size();
        assert_eq!(cols, 100);
        assert_eq!(lines, 30);

        // Clean up.
        term.write_str("exit\n");
    }

    #[test]
    fn render_cells_populated() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term =
            TerminalState::spawn(TerminalId(4), tmp.path().to_path_buf(), 80, 24).unwrap();

        thread::sleep(Duration::from_millis(500));
        term.poll();

        let cells = term.render_cells();
        assert_eq!(cells.len(), 80 * 24);

        term.write_str("exit\n");
    }

    #[test]
    fn spawn_program_runs_command() {
        let tmp = tempfile::tempdir().unwrap();
        let mut term = TerminalState::spawn_program(
            TerminalId(5),
            tmp.path().to_path_buf(),
            "/bin/echo",
            &["AWILI_PROG_MARKER".to_string()],
            &[],
            80,
            24,
        )
        .unwrap();

        thread::sleep(Duration::from_millis(400));
        term.poll();

        let text = term.screen_text();
        assert!(
            text.contains("AWILI_PROG_MARKER"),
            "screen should contain program output, got: {text}"
        );
    }

    #[test]
    fn rc1f2c_paste_bytes_brackets_and_strips_controls() {
        assert_eq!(
            paste_bytes("ls\nrm -rf x\n", true),
            "\x1b[200~ls\nrm -rf x\n\x1b[201~"
        );
        // A pasted end marker or other control cannot escape the bracket.
        let hostile = "a\x1b[201~\necho pwned\x03\x07\tb\u{9b}c";
        assert_eq!(
            paste_bytes(hostile, true),
            "\x1b[200~a[201~\necho pwned\tbc\x1b[201~"
        );
        let sent = paste_bytes(hostile, true);
        assert_eq!(sent.matches('\x1b').count(), 2, "only the two markers");
        // Not bracketed: newlines become carriage returns.
        assert_eq!(paste_bytes("a\r\nb\nc", false), "a\rb\rc");
        assert_eq!(paste_bytes("one line", false), "one line");
    }

    /// The real PTY path: a program that turned bracketed paste on gets the
    /// paste wrapped at once; one that did not gets NOTHING until the paste is
    /// confirmed (RC1-F-SLICE-FIX1a), then plain carriage returns. The child
    /// records the raw bytes it receives.
    #[test]
    fn rc1f2c_paste_follows_the_programs_bracketed_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let text = "echo A\necho B\n";
        for (on, want) in [
            (true, paste_bytes(text, true)),
            (false, paste_bytes(text, false)),
        ] {
            let out = tmp.path().join(format!("got-{on}"));
            let enable = if on { "printf '\\033[?2004h'; " } else { "" };
            let script = format!(
                "{enable}stty raw -echo; printf 'READY\\n'; head -c {} > '{}'",
                want.len(),
                out.display()
            );
            let mut term = TerminalState::spawn_program(
                TerminalId(40 + on as u64),
                tmp.path().to_path_buf(),
                "/bin/sh",
                &["-c".to_string(), script],
                &[],
                80,
                24,
            )
            .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !term.screen_text().contains("READY") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "child never got ready"
                );
                thread::sleep(Duration::from_millis(20));
                term.poll();
            }
            assert_eq!(
                term.term
                    .lock()
                    .unwrap()
                    .mode()
                    .contains(TermMode::BRACKETED_PASTE),
                on
            );
            if on {
                assert_eq!(term.paste(text), PastePlan::Sent);
            } else {
                assert_eq!(term.paste(text), PastePlan::AskFirst { lines: 2 });
                thread::sleep(Duration::from_millis(300));
                assert_eq!(
                    std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0),
                    0,
                    "a held paste sends nothing"
                );
                term.paste_confirmed(text);
            }
            while std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) < want.len() as u64 {
                assert!(std::time::Instant::now() < deadline, "paste never arrived");
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                std::fs::read_to_string(&out).unwrap(),
                want,
                "bracketed={on}"
            );
        }
    }

    /// The terminal's ⌘V/⌃V uses the paste path, never a raw write.
    #[test]
    fn rc1f2c_terminal_view_pastes_through_paste() {
        let view = include_str!("terminal_view.rs");
        assert!(view.contains("self.terminal.paste(&text)"));
        assert!(!view.contains("self.terminal.write_str(&text)"));
    }

    /// RC1-F-SLICE-FIX1a (F0254, slice review P1 on ad1907ca): without
    /// bracketed paste, any line break holds the paste; with it, or with no
    /// line break, the paste goes at once.
    #[test]
    fn rc1f_slice_fix1a_line_breaks_without_brackets_are_held() {
        assert_eq!(paste_needs_confirmation("echo A\necho B\n", false), Some(2));
        assert_eq!(paste_needs_confirmation("ls\n", false), Some(1));
        assert_eq!(paste_needs_confirmation("a\r\nb", false), Some(2));
        assert_eq!(paste_needs_confirmation("a\rb", false), Some(2));
        assert_eq!(paste_needs_confirmation("echo A", false), None);
        assert_eq!(paste_needs_confirmation("", false), None);
        assert_eq!(paste_needs_confirmation("echo A\necho B\n", true), None);
    }

    /// RC1-F-SLICE-FIX1a-FIX1 (Codex T2 P1 on 4a74698): the question counts
    /// the lines the confirmed bytes really hold, for every line-end shape.
    #[test]
    fn rc1f_slice_fix1a_fix1_line_count_follows_the_sent_bytes() {
        let table = [
            ("echo A\recho B\r", 2),
            ("echo A\recho B", 2),
            ("echo A\r\necho B\r\n", 2),
            ("echo A\necho B\n", 2),
            ("echo A\necho B", 2),
            ("echo A\r\necho B", 2),
            ("echo A\rB\nC\r\n", 3),
            ("ls\r", 1),
            ("ls\n", 1),
            ("\n", 1),
            ("a\n\nb", 3),
        ];
        for (text, lines) in table {
            assert_eq!(
                paste_needs_confirmation(text, false),
                Some(lines),
                "{text:?}"
            );
            let sent = paste_bytes(text, false);
            let runs = sent.matches('\r').count();
            let partial = usize::from(!sent.ends_with('\r'));
            assert_eq!(lines, runs + partial, "{text:?} -> {sent:?}");
            assert_eq!(paste_needs_confirmation(text, true), None, "{text:?}");
        }
        assert_eq!(paste_needs_confirmation("echo A", false), None);
    }
}
