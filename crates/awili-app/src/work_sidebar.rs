//! R1f — the Work conversation you are in is easy to find. A Work
//! conversation files under the folder its first turn runs in (the folder the
//! composer shows), keeps that folder group when the folder is switched later,
//! and "+ New conversation" reuses a never-sent Work conversation instead of
//! leaving another empty "New conversation" row behind.

use awili_core::ConvKind;

use crate::ask_mode::AskKey;

impl crate::AwiliApp {
    /// Every Work conversation that was never sent, newest first: no
    /// messages, the automatic title, not archived, and no prompt parked for
    /// it. None for any other `kind`. (RC2-R68: the caller picks one that
    /// holds no draft of the owner's.)
    pub(crate) fn unsent_work_conversations_of(&self, kind: ConvKind) -> Vec<String> {
        if kind != ConvKind::Work {
            return Vec::new();
        }
        self.unsent_work_conversations()
    }

    /// RC2-R3: every never-sent Work conversation, newest first (see
    /// [`Self::unsent_work_conversations_of`]).
    fn unsent_work_conversations(&self) -> Vec<String> {
        let parked = |id: &str| {
            self.work
                .queued_after_stop
                .as_ref()
                .is_some_and(|q| q.conv_id == id)
                || self
                    .work
                    .health
                    .gateway
                    .pending_send
                    .as_ref()
                    .is_some_and(|p| p.conv_id == id)
        };
        // R1f-FIX1: only a conversation the active profile's own sidebar
        // lists qualifies (the sidebar's profile filter, without "show all"),
        // so another profile's conversation, and anything staged in it, is
        // never reused.
        let visible: std::collections::HashSet<String> = crate::sidebar::workspace_groups_scoped(
            &self.conversations,
            Some(self.profile_store.active()),
            false,
        )
        .into_iter()
        .flat_map(|group| group.session_ids)
        .collect();
        self.conversations
            .active()
            .into_iter()
            .filter(|c| visible.contains(&c.id))
            .filter(|c| c.kind == ConvKind::Work && !c.title_is_custom)
            .filter(|c| {
                self.views
                    .get(&c.id)
                    .is_some_and(|v| v.convo.messages().is_empty())
            })
            .filter(|c| !parked(&c.id))
            .map(|c| c.id.clone())
            .collect()
    }

    /// RC2-R3: a never-sent Work conversation whose box is empty too \u{2014} no
    /// draft waiting in it (the one on screen: an empty message box).
    /// RC2-R3M10 (RC2-R37 review P1): a file attached and waiting in its tray
    /// (kept in `awili.db`) is not blank either: filling it would empty the
    /// tray on screen while the file stayed attached, to come back later.
    /// RC2-R54 (usability RC2 round 4, Major 4: two "Continue in Work" made
    /// two "New conversation" rows): a box that holds only what an earlier
    /// "Continue in Work" put there, untouched, counts as empty \u{2014} that
    /// text is a copy of an answer still in Chat or the council.
    /// RC2-R6M12: whether conversation `id` has a file staged (one the
    /// database cannot be read for counts as having one).
    pub(crate) fn has_staged_attachments(&self, id: &str) -> bool {
        self.search_store
            .as_ref()
            .is_some_and(|s| s.get_staged_attachments(id).map_or(true, |a| !a.is_empty()))
    }

    pub(crate) fn blank_work_conversation(&self, cx: &gpui::App) -> Option<String> {
        self.unsent_work_conversations().into_iter().find(|id| {
            if self.has_staged_attachments(id) {
                return false;
            }
            let key = crate::composer_drafts::conversation_key(id);
            if self.drafts.is_shown(&key) {
                let text = self.composer.read(cx).text().to_string();
                text.trim().is_empty() || self.drafts.is_untouched(&key, &text)
            } else {
                !self.drafts.has_owner_draft(&key)
            }
        })
    }

    /// RC2-R3 (usability RC2 round 3, Minor 14: empty "New conversation"
    /// rows piled up in the Work sidebar \u{2014} each "Continue in Work" made a
    /// new one, and one never sent stayed behind, empty after a relaunch):
    /// the Work conversation a "Continue in Work" fills. A blank never-sent
    /// one ([`Self::blank_work_conversation`]) is reused, as "+ New
    /// conversation" does (R1f); only when there is none is a new one made.
    /// One with a draft waiting is never used (RC2-R1F4: nothing mixes with
    /// it). RC2-R54: a reused one's untouched "Continue in Work" text goes,
    /// so the box takes only the new text.
    pub(crate) fn blank_or_new_work_conversation(
        &mut self,
        cx: &mut gpui::Context<Self>,
    ) -> String {
        if let Some(id) = self.blank_work_conversation(cx) {
            self.reuse_unsent_work_conversation(&id);
            self.drop_placed_draft(&id, cx);
            self.work.chip_picked.remove(&id);
            self.work.folder_picked.remove(&id);
            return id;
        }
        self.create_conversation(ConvKind::Work)
    }

    /// Make a never-sent Work conversation the new one: it moves to the top
    /// (touched now), starts in the default Ask/Auto mode like a fresh
    /// conversation, and files under the folder the composer shows.
    pub(crate) fn reuse_unsent_work_conversation(&mut self, id: &str) {
        let now = crate::feed::unix_now();
        self.conversations.touch(id, now);
        if let Some(p) = &self.persist {
            p.send(crate::PersistCmd::Touch {
                id: id.to_string(),
                updated_ts: now,
            });
        }
        self.work.ask_modes.forget(&AskKey::Work(id.to_string()));
        self.file_work_conversation(id);
        // A blank thread reused as the new one belongs to the profile the
        // owner is in now (the thread the app opens with predates any switch).
        let active = self.profile_store.active().to_string();
        let profile = (active != "default").then_some(active);
        if let Some(c) = self.conversations.get_mut(id) {
            if c.profile != profile {
                c.profile = profile.clone();
                if let Some(p) = &self.persist {
                    p.send(crate::PersistCmd::SetProfile {
                        id: id.to_string(),
                        profile,
                    });
                }
            }
        }
    }

    /// File `conv_id` under the bound Work folder (no folder bound: none),
    /// in memory and in the database.
    pub(crate) fn file_work_conversation(&mut self, conv_id: &str) {
        let cwd = self.work.folder.as_ref().map(|f| f.display().to_string());
        let Some(c) = self.conversations.get_mut(conv_id) else {
            return;
        };
        if c.cwd == cwd {
            return;
        }
        c.cwd = cwd.clone();
        if let Some(p) = &self.persist {
            let _ = p.tx.send(crate::PersistCmd::SetCwd {
                id: conv_id.to_string(),
                cwd,
            });
        }
    }

    /// A Work turn is being submitted in `conv_id`: a conversation without a
    /// folder takes the one the turn runs in. One that has a folder is never
    /// moved.
    pub(crate) fn file_work_conversation_at_submit(&mut self, conv_id: &str) {
        let unfiled = self
            .conversations
            .get(conv_id)
            .is_some_and(|c| c.cwd.as_deref().is_none_or(str::is_empty));
        if unfiled {
            self.file_work_conversation(conv_id);
        }
    }

    /// The folder was switched: the active conversation follows it only while
    /// it has no turns yet. A conversation with history stays where it is
    /// (an unfiled one is filed by its next turn's submit).
    /// The owner choosing a folder inside it is different: that re-files it
    /// (`choose_work_folder`, RC2-RUN).
    pub(crate) fn file_work_conversation_on_folder_switch(&mut self) {
        let conv_id = self.sidebar.active_id().to_string();
        let has_turns = self
            .views
            .get(&conv_id)
            .is_some_and(|v| !v.convo.messages().is_empty());
        if !has_turns {
            self.file_work_conversation(&conv_id);
        }
    }
}

#[cfg(test)]
#[path = "work_sidebar_tests.rs"]
mod tests;
