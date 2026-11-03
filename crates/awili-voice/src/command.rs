//! Verbal command classification (slice-5 hands-free control).
//!
//! Some users have limited hand mobility: in hands-free conversation mode the mic
//! is the *primary* control surface, not just a dictation source. He must be able
//! to *operate* the app by voice — halt a read-aloud, submit a draft, throw away
//! a mis-heard utterance, hear the last reply again, start fresh — without ever
//! reaching for a key or a button.
//!
//! This module is the pure kernel that answers one question for a finished
//! utterance: **is this a control command, or is it dictation?** It is
//! deliberately *not* wired into the turn loop here — the app-side
//! [`voice_turn_action`](../../awili-app) decides Submit/Exit/Ignore today, and
//! acting on a [`VoiceCommand`] (stopping speech, clearing the composer, re-reading
//! the reply, opening a conversation) is GPUI orchestration that lands in a later
//! [LIVE] slice. Here we just build and unit-test the classifier, on Linux CI,
//! like every other pure voice kernel ([`WakeGate`](crate::WakeGate),
//! [`UtteranceSegmenter`](crate::UtteranceSegmenter)).
//!
//! ## The correctness property: no false positives from dictation
//!
//! The single most important behaviour is that a *dictated sentence* which merely
//! **contains** a command word is never mistaken for a command. "Please don't
//! stop helping me", "send the report to my accountant", "let's start over on the
//! tax section" are all things Owner might *say to the model*, and every one of
//! them contains a control verb. So [`classify`] matches only when the **whole**
//! normalized utterance is a known command phrase — never a substring. A command
//! is a short, whole thing you say *to the app*; a request is a sentence you say
//! *to Awili*. Length is the tell, and whole-utterance matching encodes it exactly.
//!
//! Normalization mirrors the app's `voice_turn_action`: lowercased, punctuation
//! dropped (whisper renders "Stop." with a period), whitespace collapsed. A small,
//! fixed set of address/politeness fillers ("awili", "hey awili", "please", "ok",
//! "okay") is stripped from the ends first, because a hands-free user naturally
//! addresses the assistant ("Awili, stop", "stop please") — but the *core* between
//! the fillers must still match a command phrase exactly, so the guard holds.

/// A recognized hands-free control command — an action to take on the app itself,
/// as opposed to dictation to be sent to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VoiceCommand {
    /// Halt the current read-aloud / interrupt the assistant ("stop", "stop it",
    /// "stop talking", "cancel", "quiet", "silence", "hush", "shut up", "that's
    /// enough", or the word repeated: "stop stop"). This is the same family the app's
    /// `voice_turn_action` treats as its Exit word, surfaced as an explicit
    /// intent so the turn loop can stop *speech* without necessarily leaving the
    /// conversation.
    Stop,
    /// Submit the current draft as a user turn ("send", "send it", "submit", "go
    /// ahead", "send that"). Lets a user dictate, review, then commit by voice
    /// rather than waiting for the silence endpoint to auto-submit.
    Send,
    /// Discard the current draft / mis-heard utterance ("scratch that", "never
    /// mind", "delete that", "clear", "start over", "forget it").
    Scratch,
    /// Read the last assistant reply aloud again ("repeat", "say that again",
    /// "read that again", "come again", "what did you say").
    Repeat,
    /// Begin a fresh conversation ("new conversation", "new chat", "start a new
    /// conversation", "start over new").
    NewConversation,
    /// Answer a pending tool/skill approval gate with **allow** ("approve",
    /// "approve it", "allow it", "accept", "confirm"). Work mode raises an inline
    /// [`ApprovalCard`](../../awili-app) the user must Allow or Deny; a hands-free,
    /// user with limited hand mobility cannot click it, so today saying "approve" falls
    /// through to dictation and posts a junk user turn while the card stays up and
    /// blocks the run. This surfaces the *allow* intent so the turn loop can answer
    /// the gate by voice. The decision maps to the gateway's
    /// `ApprovalDecision::Approve`; whether a card is actually open is the [LIVE]
    /// caller's business (it no-ops otherwise), exactly as [`Send`](Self::Send)
    /// leaves the empty-draft check to the caller.
    Approve,
    /// Answer a pending tool/skill approval gate with **deny** ("deny", "deny it",
    /// "reject", "don't allow", "decline"). The Deny counterpart of
    /// [`Approve`](Self::Approve); maps to the gateway's `ApprovalDecision::Deny`.
    Deny,
}

/// Fillers stripped from either end before matching: how a hands-free user
/// addresses the assistant or softens a command. The *core* between them must
/// still be an exact command phrase, so stripping these cannot turn a dictated
/// sentence into a command — it only tolerates "Awili, stop please" → "stop".
const FILLERS: &[&str] = &["hey awili", "hey", "awili", "please", "ok", "okay", "now"];

/// Classify a finished utterance's transcript as a control command, or `None`
/// if it is dictation. Matches only when the whole normalized utterance (after
/// filler stripping) is a known command phrase — never a substring — so a
/// dictated sentence containing a command word is left as dictation.
pub fn classify(transcript: &str) -> Option<VoiceCommand> {
    let core = strip_fillers(&normalize(transcript));
    if core.is_empty() {
        return None;
    }
    if let Some(cmd) = match_phrase(&core) {
        return Some(cmd);
    }
    // whisper often renders a repeated word ("Stop. Stop. Stop.") as one
    // utterance; a run of one command word is that command (review F0956).
    let words: Vec<&str> = core.split_whitespace().collect();
    if words.len() > 1 && words.iter().all(|w| *w == words[0]) {
        return match_phrase(words[0]);
    }
    None
}

/// The command for an exact, normalized, filler-stripped phrase.
fn match_phrase(core: &str) -> Option<VoiceCommand> {
    match core {
        // Halting speech. The common ways to say it (review F0956) — "stop it",
        // "stop talking", "silence", "hush" — not only the bare word. "wait",
        // "hold on" and "pause" are deliberately absent: they ask for a moment,
        // not for the reply to end.
        "stop" | "cancel" | "quiet" | "shut up" | "thats enough" | "enough" | "be quiet"
        | "stop it" | "stop that" | "stop talking" | "stop speaking" | "stop reading"
        | "stop reading that" | "silence" | "hush" | "shush" | "quiet down" | "enough already" => {
            Some(VoiceCommand::Stop)
        }
        "send" | "send it" | "send that" | "submit" | "go ahead" | "send message" => {
            Some(VoiceCommand::Send)
        }
        "scratch that" | "never mind" | "nevermind" | "delete that" | "clear" | "start over"
        | "forget it" | "forget that" => Some(VoiceCommand::Scratch),
        "repeat" | "repeat that" | "say that again" | "read that again" | "come again"
        | "what did you say" | "say again" => Some(VoiceCommand::Repeat),
        "new conversation"
        | "new chat"
        | "start a new conversation"
        | "start new conversation"
        | "start over new"
        | "new topic" => Some(VoiceCommand::NewConversation),
        // Approval-gate answers. Kept to *unambiguous* control phrases only: bare
        // "yes"/"no"/"sure"/"okay" are deliberately NOT here — they are far too
        // common as dictation (the tests pin `classify("yes") == None`), and "okay"
        // is already a stripped filler. "approve"/"deny"/"reject"/"accept" as a
        // whole utterance are vanishingly unlikely to be dictation, and the
        // whole-utterance guard keeps "approve the budget for next year" / "please
        // deny my earlier request" as dictation.
        "approve" | "approve it" | "approve that" | "allow it" | "allow that" | "accept"
        | "accept it" | "confirm" => Some(VoiceCommand::Approve),
        "deny" | "deny it" | "deny that" | "reject" | "reject it" | "dont allow"
        | "do not allow" | "decline" => Some(VoiceCommand::Deny),
        _ => None,
    }
}

/// Lowercase, drop everything but alphanumerics and whitespace (so "Stop." and
/// the apostrophe in "that's" fall away, giving "thats"), and collapse runs of
/// whitespace to single spaces. Mirrors the app's `voice_turn_action`
/// normalization exactly — dropping punctuation rather than replacing it with a
/// space — so the two agree on what a bare command word looks like and
/// contractions like "that's"/"don't" collapse the way whisper-transcribed
/// speech is compared elsewhere.
fn normalize(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Remove leading/trailing address/politeness fillers from an already-normalized
/// string, repeatedly, so "hey awili stop please" → "stop". Only whole
/// space-delimited fillers at the ends are removed; an interior word is never
/// touched, and the loop stops as soon as neither end is a filler.
fn strip_fillers(normalized: &str) -> String {
    let mut words: Vec<&str> = normalized.split_whitespace().collect();
    let mut changed = true;
    while changed && !words.is_empty() {
        changed = false;
        // Try to strip the longest matching filler (up to two words) from the front.
        for n in [2usize, 1] {
            if words.len() >= n && FILLERS.contains(&words[..n].join(" ").as_str()) {
                // Never strip the whole utterance away: keep at least the tail.
                if words.len() > n {
                    words.drain(..n);
                    changed = true;
                    break;
                }
            }
        }
        // ...and from the back.
        for n in [2usize, 1] {
            // Never strip the whole utterance away: keep at least the head.
            if words.len() > n && FILLERS.contains(&words[words.len() - n..].join(" ").as_str()) {
                words.truncate(words.len() - n);
                changed = true;
                break;
            }
        }
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_command_words_classify() {
        assert_eq!(classify("stop"), Some(VoiceCommand::Stop));
        assert_eq!(classify("send"), Some(VoiceCommand::Send));
        assert_eq!(classify("scratch that"), Some(VoiceCommand::Scratch));
        assert_eq!(classify("repeat"), Some(VoiceCommand::Repeat));
        assert_eq!(
            classify("new conversation"),
            Some(VoiceCommand::NewConversation)
        );
    }

    #[test]
    fn whisper_punctuation_and_case_ignored() {
        // whisper renders short utterances with a capital and a period.
        assert_eq!(classify("Stop."), Some(VoiceCommand::Stop));
        assert_eq!(classify("Send it!"), Some(VoiceCommand::Send));
        assert_eq!(classify("Scratch that,"), Some(VoiceCommand::Scratch));
        assert_eq!(classify("New Chat."), Some(VoiceCommand::NewConversation));
    }

    #[test]
    fn address_and_politeness_fillers_stripped() {
        assert_eq!(classify("Awili, stop"), Some(VoiceCommand::Stop));
        assert_eq!(classify("hey awili, send it"), Some(VoiceCommand::Send));
        assert_eq!(classify("stop please"), Some(VoiceCommand::Stop));
        assert_eq!(classify("ok stop now"), Some(VoiceCommand::Stop));
        assert_eq!(classify("please repeat that"), Some(VoiceCommand::Repeat));
    }

    #[test]
    fn synonyms_map_to_the_same_intent() {
        for s in [
            "stop",
            "cancel",
            "quiet",
            "shut up",
            "enough",
            "that's enough",
            "be quiet",
        ] {
            assert_eq!(classify(s), Some(VoiceCommand::Stop), "{s:?}");
        }
        for s in ["send", "send it", "submit", "go ahead", "send message"] {
            assert_eq!(classify(s), Some(VoiceCommand::Send), "{s:?}");
        }
        for s in [
            "repeat",
            "say that again",
            "read that again",
            "come again",
            "what did you say",
        ] {
            assert_eq!(classify(s), Some(VoiceCommand::Repeat), "{s:?}");
        }
    }

    // --- the correctness property: dictation is never a command ---

    #[test]
    fn command_word_inside_a_sentence_is_dictation() {
        // Every one of these CONTAINS a command word but is a thing said to the
        // model, not to the app.
        assert_eq!(classify("please don't stop helping me"), None);
        assert_eq!(classify("send the report to my accountant"), None);
        assert_eq!(classify("let's start over on the tax section"), None);
        assert_eq!(
            classify("can you repeat the third point in more detail"),
            None
        );
        assert_eq!(
            classify("i want to start a new conversation about retirement"),
            None
        );
        assert_eq!(classify("that is enough money to retire on"), None);
    }

    #[test]
    fn empty_and_filler_only_are_not_commands() {
        assert_eq!(classify(""), None);
        assert_eq!(classify("   "), None);
        assert_eq!(classify("..."), None);
        assert_eq!(classify("awili"), None); // pure address, no command core
        assert_eq!(classify("please"), None);
        assert_eq!(classify("hey awili please"), None);
    }

    #[test]
    fn unrelated_short_utterance_is_dictation() {
        assert_eq!(classify("hello"), None);
        assert_eq!(classify("yes"), None);
        assert_eq!(classify("thank you"), None);
    }

    #[test]
    fn approval_gate_answers_classify() {
        for s in [
            "approve",
            "Approve.",
            "approve it",
            "approve that",
            "allow it",
            "accept",
            "confirm",
            "awili approve it", // address filler tolerated
        ] {
            assert_eq!(classify(s), Some(VoiceCommand::Approve), "{s:?}");
        }
        for s in [
            "deny",
            "Deny.",
            "deny it",
            "reject",
            "don't allow",
            "do not allow",
            "decline",
            "deny that please", // trailing filler "please" stripped, leaving "deny that"
        ] {
            assert_eq!(classify(s), Some(VoiceCommand::Deny), "{s:?}");
        }
    }

    #[test]
    fn bare_yes_no_are_never_approval_commands() {
        // The whole point of the tight vocabulary: the ubiquitous dictation words
        // must stay dictation so a spoken "yes" mid-conversation is never eaten as
        // an approval answer.
        for s in [
            "yes", "no", "nope", "yeah", "sure", "okay", "ok", "yep", "nah",
        ] {
            assert_eq!(classify(s), None, "{s:?}");
        }
    }

    #[test]
    fn approval_word_inside_a_sentence_is_dictation() {
        // Whole-utterance guard: an approval verb inside a real request said to the
        // model is dictation, never an answer to the gate.
        assert_eq!(classify("approve the budget for next year"), None);
        assert_eq!(classify("please deny my earlier request"), None);
        assert_eq!(classify("can you accept these terms for me"), None);
        assert_eq!(classify("reject the null hypothesis"), None);
        assert_eq!(classify("i want to confirm my flight"), None);
    }

    #[test]
    fn filler_stripping_never_consumes_the_whole_utterance() {
        // A command phrase that happens to end in a filler word keeps its core.
        assert_eq!(classify("start over"), Some(VoiceCommand::Scratch));
        // A back filler ("now") after a two-word command whose 2nd word is not a
        // filler must strip only the filler, leaving the whole command intact.
        assert_eq!(classify("start over now"), Some(VoiceCommand::Scratch));
        // Repeated/both-ends fillers still resolve to the core.
        assert_eq!(
            classify("ok awili stop now please"),
            Some(VoiceCommand::Stop)
        );
        // All-filler collapses to empty -> None (covered above), never a panic.
        assert_eq!(classify("please please please"), None);
    }

    // --- F0956: the common ways to say "stop" ---

    #[test]
    fn rc2_voice_common_stop_phrases_classify_as_stop() {
        for s in [
            "Stop it.",
            "stop that",
            "Stop talking.",
            "stop speaking",
            "stop reading",
            "Silence!",
            "hush",
            "Shush.",
            "quiet down",
            "enough already",
            "okay stop it",
            "Awili, stop talking please",
        ] {
            assert_eq!(classify(s), Some(VoiceCommand::Stop), "{s:?}");
        }
    }

    #[test]
    fn rc2_voice_a_repeated_stop_word_is_still_stop() {
        for s in [
            "Stop. Stop.",
            "stop stop stop",
            "Quiet, quiet.",
            "ok stop stop",
        ] {
            assert_eq!(classify(s), Some(VoiceCommand::Stop), "{s:?}");
        }
        // A repeated word that is not a command stays dictation.
        assert_eq!(classify("no no no"), None);
        assert_eq!(classify("hello hello"), None);
        // Mixed words are matched as a phrase only.
        assert_eq!(classify("stop go"), None);
    }

    #[test]
    fn rc2_voice_stop_phrases_inside_a_sentence_stay_dictation() {
        assert_eq!(classify("stop talking about the weather and help me"), None);
        assert_eq!(classify("how do i stop it from crashing"), None);
        assert_eq!(classify("silence is golden"), None);
        // "wait" / "hold on" / "pause" ask for a moment; they are not Stop.
        for s in ["wait", "hold on", "pause"] {
            assert_eq!(classify(s), None, "{s:?}");
        }
    }
}
