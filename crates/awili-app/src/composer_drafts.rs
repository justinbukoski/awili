//! RC2-UX (R2-18, R5-7, S-O5): one draft per conversation.
//!
//! Awili has one message box for every conversation (Work, Chat, each Code
//! tab). Before this, whatever sat in it — half a sentence, or a request a
//! Deny put back — followed the owner into "+ New conversation", another
//! conversation, Chat or Code. Now the box's text belongs to the
//! conversation on screen: leaving it keeps the text with that
//! conversation, coming back brings it back, and a new conversation starts
//! empty. A message handed back to a conversation the owner already left
//! (a send parked behind a check that finished after he moved on) waits in
//! that conversation's box.
//!
//! [`Drafts`] is the pure bookkeeping; the app calls
//! [`AwiliApp::sync_composer_draft`] each frame (and wherever it needs the
//! box in step before the next frame).

use std::collections::HashMap;

use gpui::Context;

use crate::work::Mode;
use crate::AwiliApp;

/// The drafts of the conversations not on screen, and which conversation
/// the message box holds now.
#[derive(Debug, Default)]
pub(crate) struct Drafts {
    /// Draft text by conversation key; only non-blank drafts are kept.
    by_key: HashMap<String, String>,
    /// The key whose draft the message box holds (`None` before the first
    /// sync).
    shown: Option<String>,
    /// RC2-R54 (usability RC2 round 4, Major 4): the text Awili itself put
    /// in a conversation's box ("Continue in Work"), by key. While the box
    /// still holds exactly that, the conversation holds nothing of the
    /// owner's: the text is a copy of an answer that stays where it came
    /// from.
    placed: HashMap<String, String>,
}

impl Drafts {
    /// The box moves to `key`. `current` is what the box holds now (it
    /// belongs to the key shown until now). Returns the text the box must
    /// show for `key`, or `None` when the key did not change (or on the
    /// first sync with nothing handed back, when the box already holds its
    /// own text).
    pub(crate) fn switch(&mut self, key: &str, current: &str) -> Option<String> {
        match self.shown.as_deref() {
            Some(shown) if shown == key => None,
            None => {
                // The first sync: the box holds its own text; a draft handed
                // back before it joins that text.
                self.shown = Some(key.to_string());
                self.by_key
                    .remove(key)
                    .map(|t| crate::executor_status::merged_draft(current, &t))
            }
            Some(shown) => {
                let shown = shown.to_string();
                self.keep(&shown, current);
                self.shown = Some(key.to_string());
                Some(self.by_key.remove(key).unwrap_or_default())
            }
        }
    }

    /// Whether the message box holds `key`'s draft now.
    pub(crate) fn is_shown(&self, key: &str) -> bool {
        self.shown.as_deref() == Some(key)
    }

    /// Put `text` back into the draft of `key` (not on screen), ahead of
    /// anything already typed there, as [`crate::executor_status::merged_draft`]
    /// does for the box itself.
    pub(crate) fn hand_back(&mut self, key: &str, text: &str) {
        let existing = self.by_key.remove(key).unwrap_or_default();
        let merged = crate::executor_status::merged_draft(&existing, text);
        self.keep(key, &merged);
    }

    /// RC2-R3: whether `key` (not on screen) has a draft waiting.
    pub(crate) fn has_draft(&self, key: &str) -> bool {
        self.by_key.contains_key(key)
    }

    /// RC2-R54: Awili put `text` in `key`'s box (the owner has not touched
    /// it yet).
    pub(crate) fn place(&mut self, key: &str, text: &str) {
        self.placed.insert(key.to_string(), text.to_string());
    }

    /// RC2-R54: whether `text` (the box's text for `key`: on screen, or
    /// stored) is still exactly what Awili put there.
    ///
    /// RC2-R5M5 (slice review befb867d, P3): exact equality, whitespace
    /// included. An edit that only adds indentation or a trailing newline
    /// is still the owner's edit, and must not be discarded as untouched.
    pub(crate) fn is_untouched(&self, key: &str, text: &str) -> bool {
        self.placed
            .get(key)
            .is_some_and(|p| p == text && !p.trim().is_empty())
    }

    /// RC2-R54: whether `key` (not on screen) has a draft of the owner's
    /// own: one he typed or changed, not one Awili placed and he left as it
    /// was.
    pub(crate) fn has_owner_draft(&self, key: &str) -> bool {
        self.by_key
            .get(key)
            .is_some_and(|t| !self.is_untouched(key, t))
    }

    /// RC2-R54: forget `key`'s stored draft (not on screen) and what Awili
    /// placed there.
    pub(crate) fn discard(&mut self, key: &str) {
        self.by_key.remove(key);
        self.placed.remove(key);
    }

    /// The stored draft of `key` (not on screen), if any.
    #[cfg(test)]
    pub(crate) fn stored(&self, key: &str) -> Option<&str> {
        self.by_key.get(key).map(String::as_str)
    }

    fn keep(&mut self, key: &str, text: &str) {
        if text.trim().is_empty() {
            self.by_key.remove(key);
        } else {
            self.by_key.insert(key.to_string(), text.to_string());
        }
    }
}

/// The draft key of a Work or Chat conversation.
pub(crate) fn conversation_key(conv_id: &str) -> String {
    format!("conv:{conv_id}")
}

/// The draft key of a Code tab.
pub(crate) fn code_tab_key(tab_id: u64) -> String {
    format!("code:{tab_id}")
}

impl AwiliApp {
    /// The draft key of what is on screen: the active conversation in Work
    /// and Chat, the active tab in Code; `None` on the other pages (Skills,
    /// Automations, Sessions, System, Messaging), which have no message box —
    /// the box keeps its text for when the owner comes back.
    pub(crate) fn composer_draft_key(&self) -> Option<String> {
        if self.shell.archived_reader.is_some() {
            return None;
        }
        match self.mode {
            Mode::Work | Mode::Council => {
                let id = self.sidebar.active_id();
                (!id.is_empty()).then(|| conversation_key(id))
            }
            Mode::Code => self
                .work
                .code_tabs
                .get(self.work.code_active)
                .map(|t| code_tab_key(t.id)),
            _ => None,
        }
    }

    /// Keep the message box on the conversation on screen: its text stays
    /// with the conversation it was typed in, and the box shows the draft of
    /// the conversation now on screen (empty for a new one).
    pub(crate) fn sync_composer_draft(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.composer_draft_key() else {
            return;
        };
        let current = self.composer.read(cx).text().to_string();
        if let Some(text) = self.drafts.switch(&key, &current) {
            self.composer.update(cx, |c, cx| {
                c.set_text(&text, cx);
                c.set_recalled(None, cx);
            });
            self.prompt_history.reset();
        }
    }

    /// RC2-R54: empty Work conversation `conv_id`'s box when it holds only
    /// what Awili placed there (a "Continue in Work" copy the owner never
    /// touched), on screen or stored. Returns whether it did.
    pub(crate) fn drop_placed_draft(&mut self, conv_id: &str, cx: &mut Context<Self>) -> bool {
        let key = conversation_key(conv_id);
        if self.drafts.is_shown(&key) {
            let current = self.composer.read(cx).text().to_string();
            if !self.drafts.is_untouched(&key, &current) {
                return false;
            }
            self.composer.update(cx, |c, cx| c.set_text("", cx));
            self.drafts.discard(&key);
            return true;
        }
        if self.drafts.has_draft(&key) && !self.drafts.has_owner_draft(&key) {
            self.drafts.discard(&key);
            return true;
        }
        false
    }

    /// S-O5: give `text` back to Work conversation `conv_id` that is no
    /// longer on screen: it waits in that conversation's box. (When the box
    /// still holds that conversation — no frame has moved it yet — it goes
    /// straight into the box.)
    pub(crate) fn hand_back_to_conversation(
        &mut self,
        conv_id: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let key = conversation_key(conv_id);
        if self.drafts.is_shown(&key) {
            self.restore_draft(text, cx);
        } else {
            self.drafts.hand_back(&key, text);
        }
    }
}

#[cfg(test)]
#[path = "composer_drafts_tests.rs"]
mod tests;

/// RC2-R54 (usability RC2 round 4, Minor: "Continue in Work" showed raw
/// markdown, `**Sort by size or date**`): an answer as plain text for the
/// message box \u{2014} bold marks, code ticks and heading hashes go, a "* " bullet
/// reads "- "; numbering, bullets and line breaks stay.
pub(crate) fn plain_box_text(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            let indent = line.len() - line.trim_start().len();
            let (lead, rest) = line.split_at(indent);
            let hashes = rest.chars().take_while(|c| *c == '#').count();
            let rest = match rest[hashes..].strip_prefix(' ') {
                Some(r) if (1..=6).contains(&hashes) => r,
                _ => rest,
            };
            let rest = match rest.strip_prefix("* ") {
                Some(r) => format!("- {r}"),
                None => rest.to_string(),
            };
            format!("{lead}{}", rest.replace("**", "").replace('`', ""))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
