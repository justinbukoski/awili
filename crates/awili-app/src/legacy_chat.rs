//! Read compatibility for transcripts written by the removed answer-only
//! Chat. Nothing here runs a turn or classifies new input: it only recognises
//! the fixed refusal sentences old threads hold, so that they are not replayed
//! to a tool-capable helper as if the helper had said them (a replayed "I can't
//! do that in an answer-only turn" teaches the next model to refuse again).
//! The stored rows themselves are never edited or removed.

/// Every sentence the old Chat wrote when it declined to act, newest first.
const LEGACY_DECLINES: [&str; 3] = [
    "I can't do that in an answer-only turn. Select Run as a job in this Workshop thread, and choose a folder if it needs project files.",
    "I can't do that in Chat \u{2014} switch to Work for that.",
    "I can't run tools in Chat \u{2014} switch to Work mode for that.",
];

/// A comparison form: curly apostrophes read straight, case folded, runs of
/// whitespace collapsed, closing punctuation left off.
fn comparable(text: &str) -> String {
    text.replace('\u{2019}', "'")
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!', '?'])
        .to_string()
}

/// Whether `text` is, as a whole, one of the old Chat's refusal sentences
/// (with or without its trailing `[answered by …]` line).
pub(crate) fn is_legacy_decline(text: &str) -> bool {
    let body = comparable(crate::conv_view::strip_answered_by_tag(text.trim()));
    LEGACY_DECLINES.iter().any(|d| comparable(d) == body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_every_old_refusal_wording_with_or_without_the_provenance_line() {
        for decline in LEGACY_DECLINES {
            assert!(is_legacy_decline(decline), "{decline}");
            assert!(
                is_legacy_decline(&format!("{decline}\n\n[answered by Awili]")),
                "{decline}"
            );
        }
        assert!(is_legacy_decline(
            "I can\u{2019}t do that in an answer-only turn.  Select Run as a job in this Workshop \
             thread, and choose a folder if it needs project files"
        ));
    }

    #[test]
    fn leaves_real_replies_alone() {
        for text in [
            "",
            "Here is the plan.",
            "I can't do that in an answer-only turn. Select Run as a job in this Workshop thread, \
             and choose a folder if it needs project files. But here is the answer: 42.",
            "Before you switch to Work, back up taxes.txt.",
        ] {
            assert!(!is_legacy_decline(text), "{text}");
        }
    }
}
