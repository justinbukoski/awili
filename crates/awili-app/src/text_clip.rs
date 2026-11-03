//! RC1-I1: the one char-safe way the Work surface shortens display text
//! (RC1 round 3b Blocker: `&subtitle[..57]` in the Tools card cut through a
//! 4-byte emoji and aborted the app). Counted in characters, never bytes.

/// `s` unchanged when it has at most `limit` characters; otherwise its first
/// `keep` characters followed by "…". Never splits a character.
pub(crate) fn clip(s: &str, limit: usize, keep: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let mut out: String = s.chars().take(keep).collect();
    out.push('\u{2026}');
    out
}
