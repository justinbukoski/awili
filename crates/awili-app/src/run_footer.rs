//! M2d: the per-reply run footer for Work (F02/F03/S2-F05).
//!
//! When a Work run's ledger line is written, the same facts become one muted
//! line under that reply — `GLM · glm-5.1 · 3m 12s · 33,040 tokens ·
//! $0.21`. The tokens are the ledger's figure (never an estimate), the model
//! is the backend-reported `RunPanel.model` when present (the ledger's
//! "default" shows as "default model"), the time is `run_started` to
//! terminal, and the cost appears only when the price table knows the model
//! AND usage was reported. No guessing.
//!
//! The footer lives on the row (`MessageRow.footer`), so it survives
//! re-render and is dropped with the row. RC2-RUN (UX-FOOTER): it is also
//! saved — `work_footers:<conv_id>` in `app_state`, keyed by the reply's
//! saved `ts` and a hash of its text — and put back on the reply when the
//! conversation is restored after a relaunch.

use awili_core::pricing::{cost_usd, format_usd, price_for};
use awili_selftest::graduation::{LedgerEntry, Outcome};
use gpui::prelude::*;
use gpui::SharedString;

/// The facts that shape one footer line — all taken from the run's ledger
/// entry plus the panel's backend-reported model/usage and the turn's
/// elapsed time.
pub(crate) struct FooterFacts<'a> {
    /// The ledger line for the turn.
    pub(crate) entry: &'a LedgerEntry,
    /// The backend-reported model (`RunPanel.model`), if the panel had one.
    pub(crate) reported_model: Option<&'a str>,
    /// The turn's token usage (needed before the cost can be shown).
    pub(crate) usage: Option<awili_providers::Usage>,
    /// The turn's duration (`run_started` to terminal), if known.
    pub(crate) elapsed: Option<std::time::Duration>,
}

/// "42s" (<60 s), "3m 05s" (<1 h), "1h 02m".
pub(crate) fn fmt_elapsed(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// 33040 → "33,040".
pub(crate) fn fmt_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.char_indices() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// gateway→"Fleet gateway", owned→"In-app", local-build→"Build", claude→"Claude",
/// codex→"Codex", glm→"GLM", deepseek→"DeepSeek", anything else verbatim.
pub(crate) fn executor_label(executor: &str) -> String {
    match executor {
        "gateway" => "Fleet gateway".into(),
        "owned" => "In-app".into(),
        "local-build" => "Build".into(),
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        "glm" => "GLM".into(),
        "deepseek" => "DeepSeek".into(),
        other => other.to_string(),
    }
}

/// claude→Some("anthropic"), deepseek→Some("deepseek"), else None.
pub(crate) fn price_provider(executor: &str) -> Option<&'static str> {
    match executor {
        "claude" => Some("anthropic"),
        "deepseek" => Some("deepseek"),
        _ => None,
    }
}

/// The ledger's "default" model shows as "default model"; everything else
/// verbatim.
fn model_label(model: &str) -> &str {
    if model == "default" {
        "default model"
    } else {
        model
    }
}

/// The run's outcome as the footer's FIRST part: Success adds no part; a
/// Failed run that was actually a user stop reads "Stopped", a Failed run
/// "Failed", a Fallback run "Fallback", and any other (future) outcome
/// "Stopped".
///
/// An if/else chain (NOT a `match`) on purpose: a future `Outcome::Stopped`
/// (M1b) needs no edit here — it falls through to the final `else`.
fn outcome_prefix(outcome: &Outcome) -> Option<&'static str> {
    if let Outcome::Failed { reason } = outcome {
        if reason == "stopped by user" {
            Some("Stopped")
        } else {
            Some("Failed")
        }
    } else if matches!(outcome, Outcome::Fallback { .. }) {
        Some("Fallback")
    } else if matches!(outcome, Outcome::Success) {
        None
    } else {
        Some("Stopped")
    }
}

/// Parts joined by " · ": [outcome] [executor label] [model] [elapsed]
/// [tokens] [cost]. The outcome part is present only for a non-success run,
/// and it is its own part: "Stopped · Fleet gateway · …".
///
/// - **Tokens** are the ledger's figure: `frontier_tokens` when >0 →
///   "{n} tokens"; else `local_tokens` when >0 → "{n} tokens on this
///   Mac"; else "tokens not reported" (R2e: plain words, no "frontier").
/// - **Cost** is `format_usd(cost_usd(..))` only when
///   `price_provider` + `price_for(provider, model)` give a non-zero rate
///   AND `usage` is Some. Otherwise the cost part is omitted — no guessing.
pub(crate) fn footer_text(f: &FooterFacts<'_>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(prefix) = outcome_prefix(&f.entry.outcome) {
        parts.push(prefix.to_string());
    }
    parts.push(executor_label(&f.entry.executor));
    // The backend-reported model when present, else the ledger model (whose
    // "default" shows as "default model").
    let model = model_label(f.reported_model.unwrap_or(f.entry.model.as_str())).to_string();
    parts.push(model.clone());
    if let Some(elapsed) = f.elapsed {
        parts.push(fmt_elapsed(elapsed));
    }
    let (frontier, local) = (f.entry.frontier_tokens, f.entry.local_tokens);
    parts.push(if frontier > 0 {
        format!("{} tokens", fmt_count(frontier))
    } else if local > 0 {
        format!("{} tokens on this Mac", fmt_count(local))
    } else {
        "tokens not reported".to_string()
    });
    if f.entry.executor == "codex" {
        if let Some(usage) = f.usage {
            parts.push(format!(
                "this turn: {} input + {} output",
                fmt_count(usage.input_tokens as u64),
                fmt_count(usage.output_tokens as u64)
            ));
        }
    }
    if let Some(usage) = f.usage {
        if let Some(cached) = usage.cached_input_tokens {
            parts.push(format!(
                "{} cached (included in input)",
                fmt_count(cached as u64)
            ));
        } else if f.entry.executor == "codex" {
            parts.push("cached input unreported".into());
        }
    }
    if let Some(provider) = price_provider(&f.entry.executor) {
        let price = price_for(provider, &model);
        if price.input_per_mtok > 0.0 || price.output_per_mtok > 0.0 {
            if let Some(usage) = &f.usage {
                parts.push(format_usd(cost_usd(
                    price,
                    usage.input_tokens,
                    usage.output_tokens,
                )));
            }
        }
    }
    parts.join(" · ")
}

/// The rendered line: LABEL_SIZE, MUTED, px(SPACE_M). Pure element builder.
///
/// RC2-R1G3 (RC2-R1F review P3): the text is drawn by a
/// [`gpui::StyledText`] whose layout a test reads back
/// ([`DRAWN_FOOTERS`]): the words it laid out, as the reply shows them.
pub(crate) fn footer_line(text: SharedString) -> gpui::Div {
    use crate::theme;
    let drawn = gpui::StyledText::new(text);
    #[cfg(test)]
    DRAWN_FOOTERS.with(|d| d.borrow_mut().push(drawn.layout().clone()));
    gpui::div()
        // The transcript's intrinsic height pass needs the same definite
        // width used to paint the wrapped footer. An auto-width block can
        // measure this long line at min-content width, reserving a tall blank
        // tail below the reply even though its children paint at full width.
        .w_full()
        .text_size(theme::z(theme::LABEL_SIZE))
        .text_color(theme::p().muted)
        .px(theme::z(theme::SPACE_M))
        .child(drawn)
}

#[cfg(test)]
thread_local! {
    /// RC2-R1G3: the layout of every reply footer drawn on this thread, in
    /// draw order (a test clears it, then reads the frame it draws).
    pub(crate) static DRAWN_FOOTERS: std::cell::RefCell<Vec<gpui::TextLayout>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// RC2-R1G3: the footer texts the last frame laid out (a footer element
/// that was never laid out drew nothing and is left out).
#[cfg(test)]
pub(crate) fn drawn_footers() -> Vec<String> {
    DRAWN_FOOTERS.with(|d| {
        d.borrow()
            .iter()
            .filter_map(|layout| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| layout.text())).ok()
            })
            .collect()
    })
}

impl crate::AwiliApp {
    /// Attach `footer_text` to the turn's assistant row in `views[conv]`: the
    /// Work stream row (`self.work.stream_row_idx`) when Some, else the last
    /// row if it is an assistant row. No row → nothing.
    pub(crate) fn attach_work_footer(&mut self, conv: &str, entry: &LedgerEntry) {
        let idx = {
            let Some(view) = self.views.get(conv) else {
                return;
            };
            if let Some(i) = self.work.stream_row_idx {
                match view.rows.get(i) {
                    Some(row) if row.role == crate::Role::Assistant => Some(i),
                    _ => None,
                }
            } else {
                view.rows
                    .last()
                    .filter(|r| r.role == crate::Role::Assistant)
                    .map(|_| view.rows.len() - 1)
            }
        };
        let Some(idx) = idx else {
            return;
        };
        // RC1-H1: an in-app run names its model by the configured label (not
        // the model server's file path) and ends with Awili's own record of
        // the files it changed.
        // RC1-H1-FIX1: a blank label falls back to the config's display
        // label (the model's last path segment), never the server's path.
        let in_app = entry.executor == "owned";
        let label = self.work.effective_engine().display_label();
        let reported_model = if in_app {
            Some(label.as_str())
        } else {
            self.work.panel.model.as_deref()
        };
        let facts = FooterFacts {
            entry,
            reported_model,
            usage: self.work.panel.usage,
            elapsed: self.work.run_started.map(|t| t.elapsed()),
        };
        let mut text = footer_text(&facts);
        // rc.3 S2: how full the model's window was, when its size is known.
        let window_model = if in_app {
            self.work.effective_engine().model
        } else {
            reported_model.unwrap_or(entry.model.as_str()).to_string()
        };
        let has_steps = self
            .views
            .get(conv)
            .and_then(|v| v.rows.get(idx))
            .is_some_and(|r| !r.activity.is_empty());
        let used = crate::context_breakdown::window_fill(
            self.work.panel.usage,
            self.work.panel.usage_frames,
            self.work.panel.last_usage_input,
            has_steps,
        );
        if let Some(part) = crate::context_breakdown::footer_context_part(
            &window_model,
            used,
            self.work.panel.context_window,
        ) {
            text = format!("{text} \u{b7} {part}");
        }
        if in_app {
            text.push_str(" \u{b7} ");
            // RC1-O-FIX8: a stopped turn claims only what was seen before
            // the Stop.
            let part = if self.work.owner_stopped {
                self.work.inapp_files.stopped_footer_part()
            } else {
                self.work.inapp_files.footer_part()
            };
            text.push_str(&part);
        } else if let (crate::work::WorkExecutor::Agent { .. }, Some(folder)) =
            (&self.work.executor, self.work.folder.clone())
        {
            // RC2-R612 (usability RC2 round 5, Minor 12: Gemma's footer said
            // "changed notes.txt"; Claude's and Codex's said nothing even when
            // they changed a file): an agent's footer says what changed too —
            // its finished write steps and what the folder shows changed.
            let activity = self.views[conv].rows[idx].activity.clone();
            let shown = folder.display().to_string();
            let parts = agent_change_parts(
                &activity,
                &shown,
                &self.work.folder_stamps,
                &dir_stamps(&folder),
                self.work.owner_stopped,
            );
            for part in parts {
                text = format!("{text} \u{b7} {part}");
            }
        }
        let view = self.views.get_mut(conv).expect("checked above");
        view.rows[idx].footer = Some(SharedString::from(text));
    }

    /// M2f: attach `footer_text` to a finished Code turn's reply
    /// (`rows[aidx]` of tab `tab_id`, an assistant row). Vendor CLIs report no
    /// model id, so the ledger's model shows; `usage` is the turn's summed
    /// usage; the time is M2e's run start (`code_runs.started`) to now.
    pub(crate) fn attach_code_footer(
        &mut self,
        tab_id: u64,
        aidx: usize,
        entry: &LedgerEntry,
        usage: Option<awili_providers::Usage>,
    ) {
        let facts = FooterFacts {
            entry,
            reported_model: None,
            usage,
            elapsed: self.work.code_runs.started(tab_id).map(|t| t.elapsed()),
        };
        let mut text = footer_text(&facts);
        let has_steps = self
            .work
            .code_tabs
            .iter()
            .find(|s| s.id == tab_id)
            .and_then(|t| t.rows.get(aidx))
            .is_some_and(|r| !r.activity.is_empty());
        // Code reports one summed usage per turn.
        let used = crate::context_breakdown::window_fill(usage, 1, None, has_steps);
        if let Some(part) = crate::context_breakdown::footer_context_part(
            &entry.model,
            used,
            self.work.code_windows.get(&tab_id).copied(),
        ) {
            text = format!("{text} \u{b7} {part}");
        }
        let Some(tab) = self.work.code_tabs.iter_mut().find(|s| s.id == tab_id) else {
            return;
        };
        let folder = tab.project_folder.clone().unwrap_or_default();
        if let Some(row) = tab
            .rows
            .get_mut(aidx)
            .filter(|r| r.role == crate::Role::Assistant)
        {
            // RC2-UX (D-PUNCH F08): the files the turn edited or wrote, named
            // in the footer as the in-app footer does.
            // RC2-FIXB1 (RC2-UX review P2): only steps that finished well
            // count as changed; a write that never said it finished reads
            // "tried to edit".
            let changed = changed_files_from_activity(&row.activity, &folder);
            let tried = tried_files_from_activity(&row.activity, &folder);
            let mut text = text;
            for part in [changed_part(&changed), tried_part(&tried)]
                .into_iter()
                .flatten()
            {
                text = format!("{text} \u{b7} {part}");
            }
            row.footer = Some(SharedString::from(text));
        }
    }

    /// Add one executor-reported usage frame to the live Work run's panel,
    /// summed across frames (saturating). `RunPanel.usage` is what
    /// `graduation_record::to_entry` books as the ledger line's
    /// frontier/local tokens — and so what the M2d footer shows. The agent
    /// lane's `ProviderEvent::Usage` handler is the production caller.
    pub(crate) fn note_work_usage(&mut self, u: awili_providers::Usage) {
        self.work.panel.usage_frames = self.work.panel.usage_frames.saturating_add(1);
        self.work.panel.last_usage_input = Some(u.input_tokens);
        let prev = self.work.panel.usage.unwrap_or_default();
        self.work.panel.usage = Some(awili_providers::Usage {
            cached_input_tokens: if self.work.panel.usage.is_none() {
                u.cached_input_tokens
            } else {
                prev.cached_input_tokens
                    .zip(u.cached_input_tokens)
                    .map(|(a, b)| a.saturating_add(b))
            },
            input_tokens: prev.input_tokens.saturating_add(u.input_tokens),
            output_tokens: prev.output_tokens.saturating_add(u.output_tokens),
        });
    }
}

/// RC2-R612: one entry of a folder: its size and last change; a directory
/// is recorded as present only (its own stamp changes with every file
/// added to it, which is no change of the owner's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    dir: bool,
}

/// RC2-R612: what a folder holds, path (relative, "src/file.rs") to stamp.
///
/// RC2-R6M3 (RC2-R6 review P2 on 0fc8434b: only the top level was
/// recorded, so a command that edited `src/file.rs` changed nothing the
/// footer could see and it said "no files changed"): the folder's files at
/// every depth, except inside [`STAMP_SKIP`] directories, at most
/// [`STAMP_CAP`] entries. `complete` says every entry was seen (no cap hit,
/// no unreadable directory); only then may a footer say nothing changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stamps {
    entries: std::collections::BTreeMap<String, Stamp>,
    complete: bool,
    /// RC2-R6M6: the [`STAMP_SKIP`] directories met (and not walked).
    skipped: std::collections::BTreeSet<String>,
    /// RC2-R6M8: a directory reached through a symbolic link was met and
    /// not walked (it may lie anywhere, or lead back into the folder).
    linked_dirs: bool,
}

/// RC2-R6M3: directories whose contents are a build's, a package manager's
/// or version control's, not the owner's files: recorded, not walked.
pub(crate) const STAMP_SKIP: [&str; 3] = [".git", "target", "node_modules"];

/// RC2-R6M3: the most entries a folder's walk records (it runs when an
/// agent run starts and ends, on the app's thread).
pub(crate) const STAMP_CAP: usize = 5_000;

/// RC2-R612 / RC2-R6M3: what `folder` holds now (unreadable: nothing, and
/// not complete).
pub(crate) fn dir_stamps(folder: &std::path::Path) -> Stamps {
    dir_stamps_capped(folder, STAMP_CAP)
}

/// RC2-R6M3: [`dir_stamps`] recording at most `cap` entries. Symbolic links
/// are recorded, never followed.
pub(crate) fn dir_stamps_capped(folder: &std::path::Path, cap: usize) -> Stamps {
    let mut out = Stamps {
        entries: std::collections::BTreeMap::new(),
        complete: true,
        skipped: std::collections::BTreeSet::new(),
        linked_dirs: false,
    };
    let mut todo: Vec<(std::path::PathBuf, String)> = vec![(folder.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            out.complete = false;
            continue;
        };
        for e in rd {
            let Ok(e) = e else {
                out.complete = false;
                continue;
            };
            if out.entries.len() >= cap {
                out.complete = false;
                return out;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let rel = format!("{prefix}{name}");
            let file_type = e.file_type().ok();
            let is_dir = file_type.is_some_and(|t| t.is_dir());
            // RC2-R6M8 (RC2-R6 review P3 on 0dd1a343): a link is stamped by
            // what it leads to, so an edit made through it shows; a link to
            // a directory is not walked, and the folder is then not seen
            // whole.
            let linked = file_type
                .is_some_and(|t| t.is_symlink())
                .then(|| std::fs::metadata(e.path()).ok())
                .flatten();
            if linked.as_ref().is_some_and(|m| m.is_dir()) {
                out.linked_dirs = true;
                out.entries.insert(
                    rel,
                    Stamp {
                        len: 0,
                        modified: None,
                        dir: true,
                    },
                );
                continue;
            }
            if is_dir {
                if STAMP_SKIP.contains(&name.as_str()) {
                    out.skipped.insert(name.clone());
                } else {
                    todo.push((e.path(), format!("{rel}/")));
                }
                out.entries.insert(
                    rel,
                    Stamp {
                        len: 0,
                        modified: None,
                        dir: true,
                    },
                );
                continue;
            }
            let meta = linked.or_else(|| e.metadata().ok());
            out.entries.insert(
                rel,
                Stamp {
                    len: meta.as_ref().map_or(0, |m| m.len()),
                    modified: meta.and_then(|m| m.modified().ok()),
                    dir: false,
                },
            );
        }
    }
    out
}

/// RC2-R612: the entries `after` added or changed (by size or change time)
/// against `before`, and the ones it no longer has, each sorted.
///
/// RC2-R6M3: a directory is named only when nothing named is inside it
/// (`newdir/a.rs` says enough about `newdir`). When either walk is not
/// complete, only what both saw and differs is named: an entry one walk
/// missed is no proof it was added or removed.
pub(crate) fn stamp_changes(before: &Stamps, after: &Stamps) -> (Vec<String>, Vec<String>) {
    let complete = before.complete && after.complete;
    let touched: Vec<String> = after
        .entries
        .iter()
        .filter(|(name, stamp)| match before.entries.get(*name) {
            Some(was) => was != *stamp,
            None => complete,
        })
        .map(|(name, _)| name.clone())
        .collect();
    let removed: Vec<String> = before
        .entries
        .keys()
        .filter(|name| complete && !after.entries.contains_key(*name))
        .cloned()
        .collect();
    let tidy = |names: Vec<String>, stamps: &Stamps| -> Vec<String> {
        names
            .iter()
            .filter(|name| {
                let dir = stamps.entries.get(*name).is_some_and(|s| s.dir);
                !dir || !names
                    .iter()
                    .any(|other| other.starts_with(&format!("{name}/")))
            })
            .cloned()
            .collect()
    };
    (tidy(touched, after), tidy(removed, before))
}

/// RC2-R612: an agent run's footer parts about files: "changed …" (its
/// finished write steps, then what the folder shows added or changed),
/// "removed …", "tried to edit …"; "no files changed" when it changed
/// nothing (not claimed for a stopped run, which may still be writing).
/// RC2-R6M3: nor when the folder could not be seen whole (its walk hit
/// [`STAMP_CAP`] or an unreadable directory): the footer is never wrong.
/// RC2-R6M6: and when it met [`STAMP_SKIP`] directories it did not look
/// inside, it says so: "no files changed outside .git and target".
/// RC2-R6M8: a directory reached through a symbolic link is not walked, so
/// a folder with one gets no such claim at all.
pub(crate) fn agent_change_parts(
    activity: &[SharedString],
    folder: &str,
    before: &Stamps,
    after: &Stamps,
    stopped: bool,
) -> Vec<String> {
    let mut changed = changed_files_from_activity(activity, folder);
    let (touched, removed) = stamp_changes(before, after);
    for name in touched {
        if !changed.contains(&name) {
            changed.push(name);
        }
    }
    let tried = tried_files_from_activity(activity, folder);
    let mut parts: Vec<String> = [
        changed_part(&changed),
        removed_part(&removed),
        tried_part(&tried),
    ]
    .into_iter()
    .flatten()
    .collect();
    // RC2-R6M8: nor when a directory reached through a link was not walked.
    let whole = before.complete && after.complete && !before.linked_dirs && !after.linked_dirs;
    if parts.is_empty() && !stopped && whole {
        // RC2-R6M6 (RC2-R6 review P3 on 9c0fa36d): what was not looked
        // inside is named, so the claim is never wider than the walk.
        let skipped: Vec<&str> = STAMP_SKIP
            .iter()
            .copied()
            .filter(|n| before.skipped.contains(*n) || after.skipped.contains(*n))
            .collect();
        parts.push(match skipped.as_slice() {
            [] => "no files changed".to_string(),
            [one] => format!("no files changed outside {one}"),
            [rest @ .., last] => {
                format!("no files changed outside {} and {last}", rest.join(", "))
            }
        });
    }
    parts
}

/// RC2-R612: the footer's words for files a run removed.
pub(crate) fn removed_part(removed: &[String]) -> Option<String> {
    match removed {
        [] => None,
        names if names.len() <= 3 => Some(format!("removed {}", names.join(", "))),
        names => Some(format!("removed {} files", names.len())),
    }
}

/// RC2-FIXB1: the start of a step that writes a file — Claude's `Edit`,
/// `Write`, `MultiEdit` and `NotebookEdit`, Codex's `edit`.
const WRITE_STEPS: [&str; 5] = ["Edit ", "Write ", "MultiEdit ", "NotebookEdit ", "edit "];

/// RC2-FIXB1: the mark a finished step's activity line starts with.
const FINISHED_MARK: &str = "\u{2713} ";

/// One activity line read as a write step: `(finished, path)`, or `None` for
/// any other line. Claude's own plan notes are not the owner's files
/// (RC1-N); the "outside project" mark in front is read through (M2b).
fn write_step(line: &str) -> Option<(bool, &str)> {
    if line.starts_with(crate::agent_brief::PLAN_NOTES_MARK) {
        return None;
    }
    let line: &str = line.rsplit(" \u{b7} ").next().unwrap_or(line);
    let (finished, step) = match line.strip_prefix(FINISHED_MARK) {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let path = WRITE_STEPS
        .iter()
        .find_map(|p| step.strip_prefix(p))
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != "file")?;
    Some((finished, path))
}

/// The paths of the write steps whose `finished` is `want`, in order, once
/// each, relative to the project folder when inside it.
fn write_paths(lines: &[SharedString], folder: &str, want: bool) -> Vec<String> {
    let folder = folder.trim_end_matches('/');
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        let Some((finished, path)) = write_step(line) else {
            continue;
        };
        if finished != want {
            continue;
        }
        let shown = match path.strip_prefix(&format!("{folder}/")) {
            Some(rel) if !folder.is_empty() => rel.to_string(),
            _ => path.to_string(),
        };
        if !out.contains(&shown) {
            out.push(shown);
        }
    }
    out
}

/// RC2-UX (D-PUNCH F08: no list of the files a Code turn changed): the files a
/// Code turn edited or wrote, in order, once each, relative to the project
/// folder when inside it.
///
/// RC2-FIXB1 (RC2-UX review P2: a step that only started counted as a
/// change): only a write step that FINISHED WELL counts — "✓ Edit src/a.rs"
/// (Claude's tool result names its step) or "✓ edit src/a.rs" (Codex reports
/// a file change once it is applied). A failed step shows no ✓ line.
pub(crate) fn changed_files_from_activity(lines: &[SharedString], folder: &str) -> Vec<String> {
    write_paths(lines, folder, true)
}

/// RC2-FIXB1: the files a Code turn started to write that never finished
/// well (the step failed, or the turn stopped first), leaving out any it
/// changed on another try. The footer says "tried to edit" for these.
pub(crate) fn tried_files_from_activity(lines: &[SharedString], folder: &str) -> Vec<String> {
    let changed = changed_files_from_activity(lines, folder);
    write_paths(lines, folder, false)
        .into_iter()
        .filter(|p| !changed.contains(p))
        .collect()
}

/// RC2-FIXB1: the footer's words for [`tried_files_from_activity`] (`None`
/// when every write it started finished).
pub(crate) fn tried_part(tried: &[String]) -> Option<String> {
    match tried {
        [] => None,
        names if names.len() <= 3 => Some(format!("tried to edit {}", names.join(", "))),
        names => Some(format!("tried to edit {} files", names.len())),
    }
}

/// RC2-UX (D-PUNCH F08): the footer's words for [`changed_files_from_activity`]
/// (`None` when the turn edited nothing it reported).
pub(crate) fn changed_part(changed: &[String]) -> Option<String> {
    match changed {
        [] => None,
        names if names.len() <= 3 => Some(format!("changed {}", names.join(", "))),
        names => Some(format!("changed {} files", names.len())),
    }
}

/// RC2-RUN (UX-FOOTER): the `app_state` key prefix for a Work conversation's
/// saved reply footers: `work_footers:<conv_id>` → a JSON object of
/// [`footer_key`] → footer line.
pub(crate) const FOOTERS_KEY_PREFIX: &str = "work_footers:";

/// One conversation's saved footers, by [`footer_key`].
pub(crate) type SavedFooters = std::collections::BTreeMap<String, String>;

/// The `app_state` key holding `conv`'s saved footers.
pub(crate) fn footers_key(conv: &str) -> String {
    format!("{FOOTERS_KEY_PREFIX}{conv}")
}

/// A saved footer's key: the reply's saved `ts` and a 64-bit FNV-1a hash of
/// its saved text, so a footer only ever goes back on the reply it was
/// written for (a `ts` alone can repeat across relaunches).
pub(crate) fn footer_key(ts: u64, text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{ts}:{hash:016x}")
}

/// Parse a saved footers value; anything unreadable is an empty set (a bad
/// row never blocks a restore).
pub(crate) fn parse_footers(raw: &str) -> SavedFooters {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Put `saved` footers back on a restored conversation's rows. Row `i` is
/// the `i`-th stored message `restore_conversation` kept (it skips rows with
/// an unknown role, so the same filter is applied here).
pub(crate) fn apply_saved_footers(
    view: &mut crate::conv_view::ConvView,
    msgs: &[awili_persist::StoredMessage],
    saved: &SavedFooters,
) {
    if saved.is_empty() {
        return;
    }
    let kept = msgs
        .iter()
        .filter(|m| crate::role_from_str(&m.role).is_some());
    for (row, m) in view.rows.iter_mut().zip(kept) {
        if row.role != crate::Role::Assistant {
            continue;
        }
        let ts = u64::try_from(m.ts).unwrap_or_default();
        if let Some(footer) = saved.get(&footer_key(ts, &m.text)) {
            row.footer = Some(SharedString::from(footer.clone()));
        }
    }
}

/// RC2-UX (D-R1 m6: after a relaunch the "N steps" disclosure was gone): the
/// `app_state` key prefix of a Work conversation's saved reply steps.
pub(crate) const STEPS_KEY_PREFIX: &str = "work_steps:";

/// The saved steps key of `conv`.
pub(crate) fn steps_key(conv: &str) -> String {
    format!("{STEPS_KEY_PREFIX}{conv}")
}

/// One reply's saved steps: the activity lines and how long they took.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedStepList {
    pub(crate) lines: Vec<String>,
    pub(crate) secs: Option<f32>,
}

/// A Work conversation's saved steps, by [`footer_key`].
pub(crate) type SavedSteps = std::collections::BTreeMap<String, SavedStepList>;

/// Parse saved steps; anything unreadable is an empty set.
pub(crate) fn parse_steps(raw: &str) -> SavedSteps {
    serde_json::from_str(raw).unwrap_or_default()
}

/// Put `saved` steps back on a restored conversation's replies (the rows
/// line up with the stored messages as in [`apply_saved_footers`]).
pub(crate) fn apply_saved_steps(
    view: &mut crate::conv_view::ConvView,
    msgs: &[awili_persist::StoredMessage],
    saved: &SavedSteps,
) {
    if saved.is_empty() {
        return;
    }
    let kept = msgs
        .iter()
        .filter(|m| crate::role_from_str(&m.role).is_some());
    for (row, m) in view.rows.iter_mut().zip(kept) {
        if row.role != crate::Role::Assistant {
            continue;
        }
        let ts = u64::try_from(m.ts).unwrap_or_default();
        if let Some(steps) = saved.get(&footer_key(ts, &m.text)) {
            row.activity = steps
                .lines
                .iter()
                .cloned()
                .map(SharedString::from)
                .collect();
            row.activity_secs = steps.secs;
        }
    }
}

impl crate::AwiliApp {
    /// RC2-RUN (UX-FOOTER): the reply at `idx` in `conv` was just saved with
    /// `ts` and `text`: save its footer (when it has one) under the same
    /// identity, replacing any earlier footer saved for that `ts`.
    pub(crate) fn save_work_footer(&mut self, conv: &str, idx: usize, ts: u64, text: &str) {
        let Some(footer) = self
            .views
            .get(conv)
            .and_then(|v| v.rows.get(idx))
            .and_then(|r| r.footer.clone())
        else {
            return;
        };
        let saved = self.work_footers.entry(conv.to_string()).or_default();
        let prefix = format!("{ts}:");
        saved.retain(|k, _| !k.starts_with(&prefix));
        saved.insert(footer_key(ts, text), footer.to_string());
        if let (Some(p), Ok(json)) = (&self.persist, serde_json::to_string(saved)) {
            p.set_state(&footers_key(conv), json);
        }
        // RC2-UX (D-R1 m6): and its steps, so "N steps" opens after a relaunch.
        let steps = self
            .views
            .get(conv)
            .and_then(|v| v.rows.get(idx))
            .filter(|r| !r.activity.is_empty())
            .map(|r| SavedStepList {
                lines: r.activity.iter().map(ToString::to_string).collect(),
                secs: r.activity_secs,
            });
        if let Some(steps) = steps {
            let saved = self.work_steps.entry(conv.to_string()).or_default();
            saved.retain(|k, _| !k.starts_with(&prefix));
            saved.insert(footer_key(ts, text), steps);
            if let (Some(p), Ok(json)) = (&self.persist, serde_json::to_string(saved)) {
                p.set_state(&steps_key(conv), json);
            }
        }
    }
}

// ---- RC2-CNCL12: council card footers and tallies ---------------------------------

/// What a council member costs, in one word or a price: "included" for the
/// subscription CLIs (Claude, Codex), "free" for models at home or a free
/// provider, "billed by Z.ai" for GLM (Awili sees no price), the price when
/// its rate is known, else "cost not reported" (RC2-CNCLF6: words without a
/// known rate are never "free"; a known free rate — a model at home — is).
pub(crate) fn council_cost_word(
    route: &str,
    cost_usd: Option<f64>,
    _usage: Option<(u64, u64)>,
) -> String {
    if route.starts_with("agent:claude") || route.starts_with("agent:codex") {
        return "included".to_string();
    }
    if route == "in-app" || route == crate::agent_chat::FLEET_CHAT_ROUTE {
        return "free".to_string();
    }
    if route.starts_with("agent:glm") {
        return "billed by Z.ai".to_string();
    }
    // RC2-CNCLF6: "free" only for a known free rate (a model at home); an
    // unknown rate is "cost not reported", whatever its words.
    match cost_usd {
        Some(c) if c > 0.0 => format_usd(c),
        Some(_) => "free".to_string(),
        None => "cost not reported".to_string(),
    }
}

fn council_secs(ms: u64) -> String {
    let s = (ms as f64 / 1000.).round() as u64;
    if s < 60 {
        format!("{s} s")
    } else {
        fmt_elapsed(std::time::Duration::from_secs(s))
    }
}

/// One card's footer: "9 s · 41 words · included". `None` while it is
/// still answering or when it didn't answer.
pub(crate) fn council_footer(t: &awili_council::SeatTurn) -> Option<String> {
    use awili_council::TurnStatus;
    if !matches!(t.status, TurnStatus::Answered | TurnStatus::Stopped) {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(ms) = t.elapsed_ms {
        parts.push(council_secs(ms));
    }
    let words = t.body().split_whitespace().count() as u64;
    parts.push(match words {
        1 => "1 word".to_string(),
        n => format!("{} words", fmt_count(n)),
    });
    parts.push(council_cost_word(&t.route, t.cost_usd, t.usage));
    Some(parts.join(" \u{00B7} "))
}

/// A round's time (its slowest member) and what it cost: "14 s · $0.002".
pub(crate) fn council_round_tally(r: &awili_council::Round) -> String {
    let ms = r
        .turns
        .iter()
        .filter_map(|t| t.elapsed_ms)
        .max()
        .unwrap_or(0);
    let usd: f64 = r.turns.iter().filter_map(|t| t.cost_usd).sum();
    format!("{} \u{00B7} {}", council_secs(ms), format_usd(usd))
}

/// The council so far: "2 rounds · 36 s · $0.002".
pub(crate) fn council_tally(c: &awili_council::Council) -> String {
    council_tally_at(c, crate::feed::unix_now())
}

/// RC2-R2F11 (usability RC2 round 2, F4: the header read "1 round · 0 s"
/// until the round was over): [`council_tally`] at `now` (unix seconds) —
/// a member still answering counts the time since it was asked.
pub(crate) fn council_tally_at(c: &awili_council::Council, now: i64) -> String {
    let n = c.rounds.len();
    let ms: u64 = c
        .rounds
        .iter()
        .map(|r| {
            r.turns
                .iter()
                .filter_map(|t| match t.elapsed_ms {
                    Some(ms) => Some(ms),
                    None if t.status.is_running() => {
                        Some(u64::try_from(now.saturating_sub(t.started_ts)).unwrap_or(0) * 1000)
                    }
                    None => None,
                })
                .max()
                .unwrap_or(0)
        })
        .sum();
    let usd: f64 = c
        .rounds
        .iter()
        .flat_map(|r| r.turns.iter().filter_map(|t| t.cost_usd))
        .sum();
    let rounds = if n == 1 {
        "1 round".to_string()
    } else {
        format!("{n} rounds")
    };
    format!(
        "{rounds} \u{00B7} {} \u{00B7} {}",
        council_secs(ms),
        format_usd(usd)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        executor: &str,
        model: &str,
        outcome: Outcome,
        frontier: u64,
        local: u64,
    ) -> LedgerEntry {
        LedgerEntry {
            v: 1,
            recorded_at: "2026-09-29T10:00:00Z".to_string(),
            task_id: "work:c1:t1".to_string(),
            executor: executor.to_string(),
            model: model.to_string(),
            outcome,
            frontier_tokens: frontier,
            local_tokens: local,
            notes: None,
        }
    }

    fn usage(input: u32, output: u32) -> awili_providers::Usage {
        awili_providers::Usage {
            cached_input_tokens: None,
            input_tokens: input,
            output_tokens: output,
        }
    }

    #[test]
    fn m2d_fmt_elapsed_and_count() {
        use std::time::Duration;
        assert_eq!(fmt_elapsed(Duration::ZERO), "0s");
        assert_eq!(fmt_elapsed(Duration::from_secs(59)), "59s");
        assert_eq!(fmt_elapsed(Duration::from_secs(60)), "1m 00s");
        assert_eq!(fmt_elapsed(Duration::from_secs(3725)), "1h 02m");
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1234567), "1,234,567");
    }

    #[test]
    fn m2d_footer_names_executor_model_time_tokens() {
        let e = entry("glm", "default", Outcome::Success, 33040, 0);
        let f = FooterFacts {
            entry: &e,
            reported_model: Some("glm-5.1"),
            usage: Some(usage(1000, 200)),
            elapsed: Some(std::time::Duration::from_secs(192)),
        };
        let text = footer_text(&f);
        assert_eq!(text, "GLM · glm-5.1 · 3m 12s · 33,040 tokens");
    }

    #[test]
    fn m2d_footer_cost_only_when_priced_and_reported() {
        let e = entry(
            "claude",
            "claude-sonnet-4-6",
            Outcome::Success,
            1_000_000,
            0,
        );
        let price = price_for("anthropic", "claude-sonnet-4-6");
        let expected_tail = format_usd(cost_usd(price, 1_000_000, 0));
        // $3.00 / 1M in → the tail must end in the formatted rate.
        let f = FooterFacts {
            entry: &e,
            reported_model: None,
            usage: Some(usage(1_000_000, 0)),
            elapsed: Some(std::time::Duration::from_secs(192)),
        };
        let text = footer_text(&f);
        assert!(
            text.ends_with(&expected_tail),
            "expected tail {expected_tail}, got {text}"
        );
        // usage: None → no "$".
        let f = FooterFacts {
            entry: &e,
            reported_model: None,
            usage: None,
            elapsed: None,
        };
        assert!(!footer_text(&f).contains('$'));
        // model "default" → no "$".
        let e_default = entry("claude", "default", Outcome::Success, 1_000_000, 0);
        let f = FooterFacts {
            entry: &e_default,
            reported_model: Some("default"),
            usage: Some(usage(1_000_000, 0)),
            elapsed: None,
        };
        assert!(!footer_text(&f).contains('$'));
    }

    #[test]
    fn m2d_footer_outcome_prefix() {
        let text = |e: &LedgerEntry, elapsed: Option<std::time::Duration>| -> String {
            footer_text(&FooterFacts {
                entry: e,
                reported_model: None,
                usage: None,
                elapsed,
            })
        };
        // Stopped: the outcome is its OWN part, then the executor label.
        let e_stop = entry(
            "gateway",
            "default",
            Outcome::Failed {
                reason: "stopped by user".into(),
            },
            0,
            0,
        );
        assert_eq!(
            text(&e_stop, None),
            "Stopped · Fleet gateway · default model · tokens not reported"
        );
        // Failed.
        let e_fail = entry(
            "glm",
            "default",
            Outcome::Failed { reason: "x".into() },
            0,
            0,
        );
        assert_eq!(
            text(&e_fail, None),
            "Failed · GLM · default model · tokens not reported"
        );
        // Fallback, bare and with time + tokens (the full part order).
        let e_fb = entry(
            "claude",
            "claude-sonnet-4-6",
            Outcome::Fallback {
                reason: "after local failure".into(),
            },
            0,
            0,
        );
        assert_eq!(
            text(&e_fb, None),
            "Fallback · Claude · claude-sonnet-4-6 · tokens not reported"
        );
        let e_fb_tokens = entry(
            "claude",
            "claude-sonnet-4-6",
            Outcome::Fallback {
                reason: "after local failure".into(),
            },
            1_222_000,
            0,
        );
        assert_eq!(
            text(&e_fb_tokens, Some(std::time::Duration::from_secs(192))),
            "Fallback · Claude · claude-sonnet-4-6 · 3m 12s · 1,222,000 tokens"
        );
        // Success: no outcome part at all — the executor label leads.
        let e_ok = entry("glm", "default", Outcome::Success, 0, 0);
        assert_eq!(
            text(&e_ok, None),
            "GLM · default model · tokens not reported"
        );
    }

    #[test]
    fn m2d_footer_local_and_unreported_tokens() {
        let e = entry("owned", "default", Outcome::Success, 0, 2310);
        let f = FooterFacts {
            entry: &e,
            reported_model: Some("some-local-model"),
            usage: None,
            elapsed: None,
        };
        let text = footer_text(&f);
        assert!(text.starts_with("In-app · "));
        assert!(text.contains("2,310 tokens on this Mac"), "got: {text}");

        let e = entry("owned", "default", Outcome::Success, 0, 0);
        let f = FooterFacts {
            entry: &e,
            reported_model: None,
            usage: None,
            elapsed: None,
        };
        assert!(footer_text(&f).contains("tokens not reported"));
    }

    /// M2f: a Code turn's footer from its ledger line: no reported model (the
    /// ledger's "default" shows), the turn's time, the ledger's tokens; GLM is
    /// not priced, so no cost.
    #[test]
    fn m2f_code_turn_footer_from_ledger() {
        let e = entry("glm", "default", Outcome::Success, 1500, 0);
        let f = FooterFacts {
            entry: &e,
            reported_model: None,
            usage: Some(usage(1000, 500)),
            elapsed: Some(std::time::Duration::from_secs(30)),
        };
        assert_eq!(footer_text(&f), "GLM · default model · 30s · 1,500 tokens");
    }

    /// R2e (RC1 round 2, Minor 9: "frontier tokens" is jargon): no footer
    /// says "frontier"; a count is "{n} tokens", or "{n} tokens on this Mac"
    /// for the in-app engine; the cost part stays omitted when unknown.
    #[test]
    fn r2e_footer_words_are_plain() {
        for (executor, frontier, local) in [
            ("claude", 9_390, 0),
            ("glm", 33_040, 0),
            ("gateway", 0, 0),
            ("owned", 0, 2_310),
        ] {
            let e = entry(executor, "default", Outcome::Success, frontier, local);
            let text = footer_text(&FooterFacts {
                entry: &e,
                reported_model: None,
                usage: None,
                elapsed: Some(std::time::Duration::from_secs(13)),
            });
            assert!(!text.to_lowercase().contains("frontier"), "{text}");
            assert!(!text.contains('$') && !text.contains('\u{2014}'), "{text}");
            let last = text.rsplit(" · ").next().unwrap_or_default().to_string();
            let want = match (frontier, local) {
                (0, 0) => "tokens not reported".to_string(),
                (0, n) => format!("{} tokens on this Mac", fmt_count(n)),
                (n, _) => format!("{} tokens", fmt_count(n)),
            };
            assert_eq!(last, want, "{text}");
        }
    }

    /// RC2-R6M3 (RC2-R6 review P2 on 0fc8434b): files are seen at every
    /// depth — a nested edit, a nested new file, a removed nested folder —
    /// but not inside `target/`, `node_modules/` or `.git/`; a folder seen
    /// only in part (the walk's cap) never reads "no files changed", and
    /// names only what both walks saw change.
    #[test]
    fn rc2r6m3_nested_changes_are_seen_and_a_partial_walk_claims_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write("src/file.rs", "fn one() {}\n");
        write("src/old/gone.rs", "x\n");
        write("target/debug/out", "1\n");
        write("node_modules/a/index.js", "1\n");
        write(".git/HEAD", "ref\n");
        let before = dir_stamps(root);
        assert!(before.complete);
        write("src/file.rs", "fn one() {}\nfn two() {}\n");
        write("src/new/mod.rs", "y\n");
        std::fs::remove_dir_all(root.join("src/old")).unwrap();
        write("target/debug/out", "22\n");
        write("node_modules/a/index.js", "22\n");
        write(".git/HEAD", "ref two\n");
        let after = dir_stamps(root);
        let (touched, removed) = stamp_changes(&before, &after);
        assert_eq!(touched, ["src/file.rs", "src/new/mod.rs"]);
        assert_eq!(removed, ["src/old/gone.rs"]);
        assert_eq!(
            agent_change_parts(&[], "", &before, &after, false),
            [
                "changed src/file.rs, src/new/mod.rs",
                "removed src/old/gone.rs"
            ]
        );
        // Nothing changed, seen whole: said so (RC2-R6M6: outside the
        // directories it did not look inside).
        let again = dir_stamps(root);
        assert_eq!(
            agent_change_parts(&[], "", &after, &again, false),
            ["no files changed outside .git, target and node_modules"]
        );
        // Seen in part: no claim either way.
        let part = dir_stamps_capped(root, 2);
        assert!(!part.complete);
        assert!(agent_change_parts(&[], "", &part, &part, false).is_empty());
        assert!(agent_change_parts(&[], "", &after, &part, false).is_empty());
        // Unreadable: no claim.
        let missing = dir_stamps(&root.join("no-such"));
        assert!(agent_change_parts(&[], "", &missing, &missing, false).is_empty());
    }

    /// RC2-R6M6 (RC2-R6 review P3 on 9c0fa36d: with `.git`, `target` or
    /// `node_modules` skipped, a run that changed only a file there read
    /// "no files changed"). The claim names what was not looked inside;
    /// a folder with none of them still reads "no files changed".
    #[test]
    fn rc2r6m6_no_change_names_the_directories_not_looked_inside() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        let plain = dir_stamps(root);
        assert_eq!(
            agent_change_parts(&[], "", &plain, &dir_stamps(root), false),
            ["no files changed"]
        );
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: one\n").unwrap();
        let before = dir_stamps(root);
        std::fs::write(root.join(".git/HEAD"), "ref: two, longer\n").unwrap();
        assert_eq!(
            agent_change_parts(&[], "", &before, &dir_stamps(root), false),
            ["no files changed outside .git"]
        );
        std::fs::create_dir_all(root.join("web/node_modules/x")).unwrap();
        let before = dir_stamps(root);
        std::fs::write(root.join("web/node_modules/x/i.js"), "1\n").unwrap();
        assert_eq!(
            agent_change_parts(&[], "", &before, &dir_stamps(root), false),
            ["no files changed outside .git and node_modules"]
        );
    }

    #[cfg(unix)]
    /// RC2-R6M8 (RC2-R6 review P3 on 0dd1a343: a directory reached through
    /// a symbolic link was not walked, yet the folder read "no files
    /// changed" after a file in it was edited). A folder with a linked
    /// directory gets no "no files changed" claim; a file reached through a
    /// link is stamped by the file it leads to, so an edit through it shows;
    /// other changes are still named.
    #[test]
    fn rc2r6m8_a_linked_directory_is_never_claimed_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        let root = dir.path().join("w");
        std::fs::create_dir_all(outside.join("lib")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(outside.join("lib/a.rs"), "one\n").unwrap();
        std::fs::write(outside.join("n.txt"), "one\n").unwrap();
        std::os::unix::fs::symlink(outside.join("lib"), root.join("lib")).unwrap();
        std::os::unix::fs::symlink(outside.join("n.txt"), root.join("note.txt")).unwrap();
        std::fs::write(root.join("keep.txt"), "k\n").unwrap();

        let before = dir_stamps(&root);
        std::fs::write(root.join("lib/a.rs"), "one\nand two\n").unwrap();
        let after = dir_stamps(&root);
        assert!(
            agent_change_parts(&[], "", &before, &after, false).is_empty(),
            "no claim about what was not looked inside"
        );

        let before = dir_stamps(&root);
        std::fs::write(root.join("note.txt"), "one\nand two\n").unwrap();
        std::fs::write(root.join("new.txt"), "n\n").unwrap();
        assert_eq!(
            agent_change_parts(&[], "", &before, &dir_stamps(&root), false),
            ["changed new.txt, note.txt"]
        );
    }
}
