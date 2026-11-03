//! `@Name` mentions, ported by hand from the July `lumi-council` `mention.rs`
//! (Buzz's rules), addressing council members instead of review seats:
//!
//! - **Case-folded.** `@codex` and `@Codex` name the same member.
//! - **Longest name first.** `@Claude Sonnet` is not eaten by a shorter
//!   `@Claude`; the longest name that fits the span wins.
//! - **Multi-word spans consumed.** A name may contain spaces.
//! - **An ambiguous name wakes nobody.** When one name belongs to more than
//!   one member (two Claude seats answer to `@Claude`), the message goes to
//!   none of them by that name and the owner is asked which he meant.
//!
//! Every member answers to its full label and to the label's first word
//! (`@Gemma` for "Gemma 4 26B").

use std::collections::BTreeMap;

use crate::types::Member;

/// What the `@` names in a message resolve to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mentions {
    /// The members named, left to right, each once. Empty = no member was
    /// named, so the message goes to everyone.
    pub to: Vec<String>,
    /// Names that belong to more than one member (as typed, without `@`).
    /// The owner is asked which member he meant.
    pub ambiguous: Vec<String>,
}

impl Mentions {
    /// The plain line the owner sees when a name was ambiguous.
    pub fn ask_owner(&self) -> Option<String> {
        let name = self.ambiguous.first()?;
        Some(format!(
            "More than one member answers to @{name}. Use the full name, like @{name} Sonnet."
        ))
    }
}

/// Resolve every `@Name` in `text` against the council's present members.
pub fn resolve_mentions(text: &str, members: &[Member]) -> Mentions {
    let mut by_name: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for m in members.iter().filter(|m| !m.removed) {
        let full = fold(&m.label);
        let first = full.split_whitespace().next().unwrap_or("").to_string();
        for key in [full, first] {
            if key.is_empty() {
                continue;
            }
            let ids = by_name.entry(key).or_default();
            if !ids.contains(&m.id.as_str()) {
                ids.push(m.id.as_str());
            }
        }
    }
    let mut candidates: Vec<(Vec<char>, &Vec<&str>)> = by_name
        .iter()
        .map(|(name, ids)| (name.chars().collect::<Vec<char>>(), ids))
        .collect();
    candidates.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));

    let original: Vec<char> = text.chars().collect();
    let chars: Vec<char> = text.to_lowercase().chars().collect();
    let mut out = Mentions::default();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '@' && is_mention_start(&chars, i) {
            if let Some((n, ids)) = match_longest(&chars, i + 1, &candidates) {
                if ids.len() == 1 {
                    let id = ids[0].to_string();
                    if !out.to.contains(&id) {
                        out.to.push(id);
                    }
                } else {
                    // Same length in chars unless lowercasing changed it; fall
                    // back to the folded text then.
                    let typed: String = if original.len() == chars.len() {
                        original[i + 1..i + 1 + n].iter().collect()
                    } else {
                        chars[i + 1..i + 1 + n].iter().collect()
                    };
                    if !out.ambiguous.contains(&typed) {
                        out.ambiguous.push(typed);
                    }
                }
                i += 1 + n;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn fold(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// An `@` starts a mention only at the start of the text or after a
/// non-alphanumeric character, so `mail@codex` is not a mention.
fn is_mention_start(chars: &[char], at: usize) -> bool {
    at == 0 || !chars[at - 1].is_alphanumeric()
}

fn match_longest<'a>(
    chars: &[char],
    start: usize,
    candidates: &'a [(Vec<char>, &'a Vec<&'a str>)],
) -> Option<(usize, &'a Vec<&'a str>)> {
    let rest = &chars[start..];
    for (name, ids) in candidates {
        let n = name.len();
        if n == 0 || rest.len() < n || rest[..n] != name[..] {
            continue;
        }
        if rest.len() == n || !rest[n].is_alphanumeric() {
            return Some((n, ids));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, label: &str) -> Member {
        Member::new(id, "agent:x", label, 0)
    }

    /// RC2-CNCL3: the longest name that fits wins, names fold case, spans
    /// may hold spaces, and a member named twice is asked once.
    #[test]
    fn mention_longest_name_wins() {
        let members = [
            m("m1", "Claude"),
            m("m2", "Claude Sonnet"),
            m("m3", "Codex"),
        ];
        let got = resolve_mentions(
            "@claude sonnet what do you think? and @CODEX, @codex",
            &members,
        );
        assert_eq!(got.to, ["m2", "m3"]);
        assert!(got.ambiguous.is_empty());
        // A longer word is not a shorter name; an address is not a mention.
        let got = resolve_mentions("@Codexes and mail@codex", &members);
        assert!(got.to.is_empty());
        // The first word of a label works on its own.
        let got = resolve_mentions("@Gemma go", &[m("g", "Gemma 4 26B"), m("c", "Codex")]);
        assert_eq!(got.to, ["g"]);
        // No mention: nobody is singled out (the message goes to everyone).
        assert_eq!(resolve_mentions("hello all", &members), Mentions::default());
        // A removed member cannot be named.
        let mut gone = m("m9", "Hermes");
        gone.removed = true;
        assert!(resolve_mentions("@Hermes", &[gone]).to.is_empty());
    }

    /// RC2-CNCL3: a name two members answer to goes to neither and the owner
    /// is asked which he meant, in plain words.
    #[test]
    fn ambiguous_mention_asks_owner() {
        let members = [
            m("m1", "Claude Sonnet"),
            m("m2", "Claude Haiku"),
            m("m3", "Codex"),
        ];
        let got = resolve_mentions("@Claude which is better? @Codex too", &members);
        assert_eq!(got.to, ["m3"]);
        assert_eq!(got.ambiguous, ["Claude"]);
        let line = got.ask_owner().unwrap();
        assert!(line.contains("@Claude"), "{line}");
        assert!(line.contains("More than one member"), "{line}");
        // The full name still reaches exactly one.
        assert_eq!(resolve_mentions("@claude haiku", &members).to, ["m2"]);
    }
}
