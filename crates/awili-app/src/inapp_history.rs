//! RC1-J: what the in-app model is shown as its own past.
//!
//! A conversation's in-app worker keeps the model's exact history (prompts,
//! replies, tool calls and their results) and publishes it after every turn.
//! Awili keeps it — in memory and in a file next to the conversation's engine
//! log, so it survives a relaunch — together with the transcript it covers
//! (its length and a fingerprint of those messages).
//!
//! A new worker for the conversation (after a switch ended the session, or a
//! relaunch) is shown, as its OWN past, only that kept history — and only
//! when the transcript still begins with exactly the messages it covers.
//! Everything else in the transcript (turns after it, e.g. another helper's
//! turn in the same conversation; or the whole transcript when nothing is
//! kept, e.g. a conversation from before RC1-J) is shown as CONTEXT: the
//! user's words as they were, and each reply as a clearly labelled note in a
//! user message, never as the model's own assistant message
//! ([`context_turns`], RC1-J1-FIX2). A model shown other words as its own —
//! Awili's approval and outcome lines, or another helper's reply — copies
//! them instead of calling its tools (round 3c Major 1).

use std::path::PathBuf;

use awili_core::{Message, Role};
use awili_engine_worker::{ModelHistory, RestoredTurn};

/// The model history kept for one conversation, with the transcript it
/// covers: its length (every message, the turn that produced it included) and
/// a fingerprint of those messages.
#[derive(Clone, Debug)]
pub(crate) struct KeptHistory {
    pub(crate) history: ModelHistory,
    pub(crate) transcript_len: usize,
    pub(crate) transcript_hash: String,
}

impl KeptHistory {
    /// The history as of the end of a turn, covering all of `transcript`.
    pub(crate) fn new(history: ModelHistory, transcript: &[Message]) -> Self {
        Self {
            history,
            transcript_len: transcript.len(),
            transcript_hash: transcript_hash(transcript),
        }
    }

    fn to_stored(&self) -> String {
        serde_json::json!({
            "transcript_len": self.transcript_len,
            "transcript_sha256": self.transcript_hash,
            "history": self.history.to_json(),
        })
        .to_string()
    }

    fn from_stored(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        Some(Self {
            transcript_len: usize::try_from(v.get("transcript_len")?.as_u64()?).ok()?,
            transcript_hash: v.get("transcript_sha256")?.as_str()?.to_string(),
            history: ModelHistory::from_json(v.get("history")?.as_str()?)?,
        })
    }

    /// The transcript still begins with exactly the messages this covers.
    fn covers_the_start_of(&self, transcript: &[Message]) -> bool {
        transcript.len() >= self.transcript_len
            && transcript_hash(&transcript[..self.transcript_len]) == self.transcript_hash
    }
}

/// A fingerprint of a transcript (RC1-J1-FIX3): SHA-256, hex, over an
/// unambiguous encoding — the message count, then per message a role byte and
/// the text's length (u64, little-endian) before its bytes — so no two
/// different transcripts share an input, whatever their text contains.
fn transcript_hash(msgs: &[Message]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update((msgs.len() as u64).to_le_bytes());
    for m in msgs {
        h.update(match m.role {
            Role::User => [b'u'],
            Role::Assistant => [b'a'],
        });
        h.update((m.text.len() as u64).to_le_bytes());
        h.update(m.text.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// `<engine data>/logs/<conversation>.history.json`, beside the
/// conversation's engine log.
fn stored_path(conv_id: &str) -> Option<PathBuf> {
    super::engine_data_root().map(|root| {
        root.join("logs").join(format!(
            "{}.history.json",
            super::sanitize_artifact_key(conv_id)
        ))
    })
}

/// The temporary file a save writes before renaming it into place.
fn temp_path(path: &std::path::Path) -> PathBuf {
    path.with_extension("json.tmp")
}

/// Keep `kept` for `conv_id` across launches.
pub(crate) fn save(conv_id: &str, kept: &KeptHistory) {
    if let Some(path) = stored_path(conv_id) {
        if let Err(e) = save_at(&path, kept) {
            eprintln!("awili: model history not kept for {conv_id}: {e}");
        }
    }
}

/// Write `kept` whole to a temporary file, then rename it into place, so a
/// crash never leaves half a file at `path`. When any step fails the
/// temporary file is removed (RC1-J1-FIX2): no stray copy of the history.
fn save_at(path: &std::path::Path, kept: &KeptHistory) -> std::io::Result<()> {
    let tmp = temp_path(path);
    let written = path
        .parent()
        .map_or(Ok(()), awili_config::ensure_private_app_dir)
        .and_then(|()| std::fs::write(&tmp, kept.to_stored()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The history kept for `conv_id` by an earlier launch, if any.
pub(crate) fn load(conv_id: &str) -> Option<KeptHistory> {
    let text = std::fs::read_to_string(stored_path(conv_id)?).ok()?;
    KeptHistory::from_stored(&text)
}

/// Delete the kept history with its conversation — and any temporary file a
/// crashed save left beside it (RC1-J1-FIX2).
pub(crate) fn forget(conv_id: &str) {
    if let Some(path) = stored_path(conv_id) {
        forget_at(&path);
    }
}

fn forget_at(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(temp_path(path));
}

/// What a new worker starts from, given the transcript BEFORE the turn being
/// submitted: the kept history as the model's own past when the transcript
/// still begins with exactly what it covers, and every later message as
/// context; otherwise no own past and the whole transcript as context.
pub(crate) fn resume(
    kept: Option<&KeptHistory>,
    before: &[Message],
) -> (Option<ModelHistory>, Vec<RestoredTurn>) {
    match kept.filter(|k| k.covers_the_start_of(before)) {
        Some(k) => (
            Some(k.history.clone()),
            context_turns(&before[k.transcript_len..]),
        ),
        None => (None, context_turns(before)),
    }
}

/// How a reply from the transcript is shown to the model: a labelled note in a
/// user message. Transcript replies may be another helper's (Claude, the
/// gateway …) or from before RC1-J; none is the model's own record, so none is
/// ever an assistant message of its own. RC2-GEMMA7 (S-J1): shown only such notes,
/// Gemma sometimes described a write instead of making it; the label says
/// that nothing in it was done here, and that a change takes a tool call.
pub(crate) const CONTEXT_LABEL: &str =
    "[An earlier reply in this conversation, shown for context only. It is not \
     your own reply, and it is not a record of any tool call: nothing it says was done was \
     done by you here. To change a file now, call your tools.]";

/// Transcript messages as context, oldest first: each user message as itself,
/// each reply — reduced to its words ([`model_text`]), Awili's lines removed —
/// as a [`CONTEXT_LABEL`] note on the user side. Never an assistant message.
pub(crate) fn context_turns(msgs: &[Message]) -> Vec<RestoredTurn> {
    msgs.iter()
        .filter_map(|m| match m.role {
            Role::User => Some(m.text.clone()),
            Role::Assistant => {
                let text = model_text(&m.text);
                (!text.is_empty()).then(|| format!("{CONTEXT_LABEL}\n{text}"))
            }
        })
        .filter(|t| !t.trim().is_empty())
        .map(|user| RestoredTurn {
            user,
            assistant: String::new(),
        })
        .collect()
}

/// A reply as the model wrote it: every line Awili adds to a Work reply is
/// removed. Those are the quoted notice lines (`> …`: "Approval needed", the
/// command block under it, "Allowed …" / "Denied", "<tool> FAILED", "saved",
/// sub-agent lines), a failed run's "⚠ <error>" line, the two-line denial ("Blocked by …" and its recovery
/// line), and the placeholders for a reply with no text. A quoted line the
/// model wrote itself is dropped too — losing a quote is harmless; showing
/// the model Awili's words as its own is not.
pub(crate) fn model_text(text: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut skip_next = false;
    for line in text.lines() {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        let t = line.trim_start();
        if t == ">" || t.starts_with("> ") || t.starts_with("\u{26A0} ") {
            continue;
        }
        if t.starts_with("Blocked by ") {
            skip_next = true;
            continue;
        }
        if matches!(
            t,
            "*(no text returned)*"
                | "*(turn failed \u{2014} no text returned)*"
                | "*(turn stopped \u{2014} no text returned)*"
                // RC2-UX: the same notes in their plain words (older
                // conversations keep the words above).
                | crate::conv_view::NO_TEXT
                | crate::conv_view::FAILED_EMPTY
                | crate::conv_view::STOPPED_EMPTY
        ) {
            continue;
        }
        kept.push(line);
    }
    kept.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line Awili writes into a Work reply, exactly as work.rs and
    /// approval_keys.rs write them, around the model's own words.
    fn reply_with_every_awili_line() -> String {
        let mut t = String::new();
        t.push_str("\n> **\u{26A0} Approval needed:** Write s1.txt (2 characters)\n");
        t.push_str("\n> **\u{26A0} Approval needed:** Run a command\n> ```\n> ls -la\n> ```\n");
        t.push_str(&crate::approval_keys::outcome_line("session").unwrap());
        t.push_str(&crate::approval_keys::outcome_line("once").unwrap());
        t.push_str(&crate::approval_keys::outcome_line("deny").unwrap());
        t.push_str("\n> **\u{26A0} fs.write FAILED (0.1s)**\n\n");
        t.push_str(
            "Blocked by authorization: denied by the user\nNo automatic recovery supplied\n",
        );
        t.push_str("Blocked by hook: denied\nask the owner first\n");
        t.push_str("\n> saved: note\n");
        t.push_str("\n> *spawned agent: helper*\n> *agent done: ok*\n\n");
        t.push_str("I wrote s1.txt.\nIt holds: ok");
        t
    }

    #[test]
    fn rc1j_model_text_keeps_only_the_models_words() {
        assert_eq!(
            model_text(&reply_with_every_awili_line()),
            "I wrote s1.txt.\nIt holds: ok"
        );
        assert_eq!(model_text("*(no text returned)*"), "");
        assert_eq!(model_text("\u{26A0} the model server is down"), "");
        assert_eq!(
            model_text("Plain answer.\n\n- one\n- two"),
            "Plain answer.\n\n- one\n- two"
        );
    }

    #[test]
    fn rc1j_restored_turns_never_carry_awili_lines() {
        let mut convo = awili_core::ConversationState::new();
        convo.push(Role::User, "Create s1.txt containing ok.");
        convo.push(Role::Assistant, reply_with_every_awili_line());
        convo.push(Role::User, "And s2.txt?");
        convo.push(
            Role::Assistant,
            "\n> **\u{26A0} Approval needed:** Write s2.txt (2 characters)\n",
        );
        // RC1-J1-FIX2: a transcript reply is context on the user side, never
        // an assistant message of the model's own.
        let turns = context_turns(convo.messages());
        let user = |t: &str| RestoredTurn {
            user: t.into(),
            assistant: String::new(),
        };
        assert_eq!(
            turns,
            vec![
                user("Create s1.txt containing ok."),
                user(&format!("{CONTEXT_LABEL}\nI wrote s1.txt.\nIt holds: ok")),
                user("And s2.txt?"),
            ]
        );
    }

    /// A four-message transcript: an in-app turn, then another turn.
    fn transcript(first_reply: &str) -> Vec<Message> {
        let mut convo = awili_core::ConversationState::new();
        convo.push(Role::User, "Create s1.txt containing ok.");
        convo.push(Role::Assistant, first_reply);
        convo.push(Role::User, "Say hi.");
        convo.push(Role::Assistant, "Hi from Claude.");
        convo.messages().to_vec()
    }

    #[test]
    fn rc1j_kept_history_is_used_only_for_the_transcript_it_matches() {
        assert!(ModelHistory::from_json("not a history").is_none());
        let t = transcript("Wrote s1.txt.");
        let kept = KeptHistory::new(empty_history(), &t[..2]);
        // The transcript is exactly the kept one: the model's own past only.
        let (own, context) = resume(Some(&kept), &t[..2]);
        assert!(own.is_some() && context.is_empty());
        // Another helper's turn ran since: own past + that turn as context.
        let (own, context) = resume(Some(&kept), &t);
        assert!(own.is_some());
        assert_eq!(context.len(), 2);
        assert_eq!(context[0].user, "Say hi.");
        assert_eq!(context[1].user, format!("{CONTEXT_LABEL}\nHi from Claude."));
        assert!(context.iter().all(|c| c.assistant.is_empty()));
        // The covered start changed (same length): nothing is own past.
        let (own, context) = resume(Some(&kept), &transcript("Edited.")[..2]);
        assert!(own.is_none());
        assert_eq!(context.len(), 2);
        // A message was removed: nothing is own past.
        assert!(resume(Some(&kept), &t[..1]).0.is_none());
        assert!(resume(None, &t).0.is_none());
        // The fingerprint survives the stored form.
        let back = KeptHistory::from_stored(&kept.to_stored()).expect("stored");
        assert_eq!(back.transcript_hash, kept.transcript_hash);
        assert!(back.covers_the_start_of(&t));
    }

    /// RC1-J1-FIX3 (Codex T2 P2 on 935a600): text containing NUL cannot make
    /// a changed start look covered. With NUL as the only separator these two
    /// different transcripts encoded to the same bytes.
    #[test]
    fn rc1j1_fix3_a_changed_start_with_nul_text_is_not_covered() {
        let two = |user: &str, reply: &str| {
            let mut convo = awili_core::ConversationState::new();
            convo.push(Role::User, user);
            convo.push(Role::Assistant, reply);
            convo.messages().to_vec()
        };
        let x = two("p\0a\0q", "r");
        let y = two("p", "q\0a\0r");
        let kept = KeptHistory::new(empty_history(), &x);
        assert!(kept.covers_the_start_of(&x));
        assert!(
            !kept.covers_the_start_of(&y),
            "a different start is not covered"
        );
        let (own, context) = resume(Some(&kept), &y);
        assert!(own.is_none());
        assert_eq!(context.len(), 2);
    }

    /// RC1-J1-FIX2 (Codex T2 P2 on 91299095): a save whose rename fails leaves
    /// no temporary copy of the history behind.
    #[test]
    fn rc1j1_fix2_a_failed_save_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conv.history.json");
        // A directory where the file goes: the rename cannot replace it.
        std::fs::create_dir(&path).unwrap();
        let kept = KeptHistory::new(empty_history(), &transcript("x"));
        assert!(save_at(&path, &kept).is_err());
        assert!(!temp_path(&path).exists(), "the temporary file is removed");
        // A good save leaves only the file.
        let good = dir.path().join("ok.history.json");
        save_at(&good, &kept).unwrap();
        assert!(good.exists() && !temp_path(&good).exists());
    }

    /// RC1-J1-FIX2: forgetting a conversation removes a temporary file a
    /// crashed save left, too.
    #[test]
    fn rc1j1_fix2_forget_removes_a_left_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conv.history.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::write(temp_path(&path), "{}").unwrap();
        forget_at(&path);
        assert!(!path.exists());
        assert!(!temp_path(&path).exists());
    }

    /// RC2-GEMMA7 (S-J1): a context note never reads as work done here, and
    /// says a change takes a tool call.
    #[test]
    fn rc2gemma7_a_context_note_says_a_change_takes_a_tool_call() {
        let turns = context_turns(&transcript("I created s1.txt."));
        let note = &turns[1].user;
        assert!(note.starts_with(CONTEXT_LABEL), "{note}");
        assert!(CONTEXT_LABEL.contains("To change a file now, call your tools."));
        assert!(CONTEXT_LABEL.contains("nothing it says was done was done by you here"));
    }

    fn empty_history() -> ModelHistory {
        let json = serde_json::to_string(&awili_engine::context::ContextHistory::new()).unwrap();
        ModelHistory::from_json(&json).expect("a stored history")
    }
}
