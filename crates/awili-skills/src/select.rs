//! Fleet skills selection (daily-driver P1): match a task prompt to skills.
//!
//! Pure token matching — no LLM, no substring matching. The query is
//! tokenized into WHOLE alphanumeric tokens (Unicode-aware, lowercased,
//! deduplicated). Common English stopwords and short fragments are dropped,
//! but the actual short tool identifiers [`ssh`](TOOL_IDENTITIES), `dsh` and
//! `git` are retained. A skill is selected when
//!
//! - at least one query token is a token of the skill's `name` — an EXPLICIT
//!   match; one token suffices, or
//! - at least two DISTINCT meaningful query tokens occur in the skill's
//!   metadata (`description` + `when_to_use`). A single
//!   metadata token (e.g. one shared word like "store" or "ssh") is not
//!   enough — that is what makes `what is the weather` select nothing.
//!
//! Substrings never match: `outlining` is a different token than `outline`,
//! so it does not select the `outline` skill.
//!
//! Ordering is deterministic: explicit name-hit count descending, metadata
//! hit count descending, then pack load order.
//!
//! Selection performs no logging; rendering records only injected names.

use crate::Skill;

/// The actual short tool identifiers retained despite looking like fragments:
/// a bare `dsh` (or `ssh`, or `git`) must stay a usable query.
pub const TOOL_IDENTITIES: &[&str] = &["ssh", "dsh", "git"];

/// How many distinct meaningful query tokens the skill's METADATA must
/// contain before a metadata-only match selects the skill. An explicit name
/// match needs only one.
pub const MIN_METADATA_TOKENS: u32 = 2;

/// A token shorter than this (in characters) is a fragment and is dropped
/// (unless it is one of [`TOOL_IDENTITIES`]).
const MIN_TOKEN_CHARS: usize = 3;

/// Common English stopwords: function words that carry no topic signal.
/// Deliberately does NOT include tool-ish words (`run`, `use`, `check`,
/// `name`, `get`, …) — those can be the whole point of a short prompt.
const STOPWORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "because",
    "been",
    "before",
    "behind",
    "being",
    "below",
    "beneath",
    "beside",
    "between",
    "beyond",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "done",
    "down",
    "during",
    "each",
    "either",
    "else",
    "enough",
    "even",
    "ever",
    "few",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "him",
    "his",
    "how",
    "i",
    "if",
    "in",
    "inside",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "less",
    "me",
    "more",
    "most",
    "much",
    "must",
    "my",
    "myself",
    "no",
    "nor",
    "not",
    "now",
    "of",
    "off",
    "on",
    "once",
    "one",
    "only",
    "onto",
    "or",
    "other",
    "our",
    "ours",
    "out",
    "outside",
    "over",
    "own",
    "per",
    "s",
    "same",
    "she",
    "should",
    "since",
    "so",
    "some",
    "such",
    "t",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "thus",
    "to",
    "too",
    "under",
    "until",
    "up",
    "upon",
    "us",
    "very",
    "was",
    "we",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "within",
    "without",
    "would",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
    "shall",
    "may",
    "might",
];

pub fn is_stopword(token: &str) -> bool {
    STOPWORDS.contains(&token)
}

/// The WHOLE alphanumeric runs of `s` (Unicode-aware: a run is split at any
/// non-alphanumeric character), each lowercased. Empty runs are dropped.
/// No stopword/fragment filtering — use [`meaningful_tokens`] for the query.
pub fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// The query's MEANINGFUL tokens: [`tokens`] with duplicates removed (first
/// occurrence wins) and stopwords and short fragments (< [`MIN_TOKEN_CHARS`]
/// characters) dropped — except [`TOOL_IDENTITIES`], which always survive.
pub fn meaningful_tokens(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tokens(s) {
        if is_stopword(&t) {
            continue;
        }
        if t.chars().count() < MIN_TOKEN_CHARS && !TOOL_IDENTITIES.contains(&t.as_str()) {
            continue;
        }
        if !out.iter().any(|k| k == &t) {
            out.push(t);
        }
    }
    out
}

/// One skill's match counts against the query's meaningful tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Score {
    /// Query tokens that are tokens of the skill's `name` (explicit match).
    pub name_hits: u32,
    /// DISTINCT query tokens that occur in the skill's metadata
    /// (`description` + `when_to_use` — the self-description fields the
    /// loader requires on every `SKILL.md`).
    pub meta_hits: u32,
}

impl Score {
    /// Selected: an explicit name match, or at least
    /// [`MIN_METADATA_TOKENS`] distinct metadata tokens.
    pub fn selected(self) -> bool {
        self.name_hits > 0 || self.meta_hits >= MIN_METADATA_TOKENS
    }
}

/// The [`Score`] of `skill` against the pre-tokenized meaningful `query_tokens`.
pub fn score_tokens(skill: &Skill, query_tokens: &[String]) -> Score {
    let name_tokens = tokens(&skill.name);
    let mut name_hits = 0;
    for t in query_tokens {
        if name_tokens.iter().any(|n| n == t) {
            name_hits += 1;
        }
    }
    let meta = tokens(&format!("{} {}", skill.description, skill.when_to_use));
    let meta_hits = query_tokens
        .iter()
        .filter(|t| meta.iter().any(|m| m.as_str() == **t))
        .count() as u32;
    Score {
        name_hits,
        meta_hits,
    }
}

/// The [`Score`] of `skill` against `query` (tokenized and filtered fresh).
#[cfg(test)]
fn score(skill: &Skill, query: &str) -> Score {
    score_tokens(skill, &meaningful_tokens(query))
}

/// The selected skills for `query` as (pack index, score) pairs, in
/// deterministic order: name hits desc, metadata hits desc, pack index asc.
/// Empty when the query has no meaningful tokens.
pub fn selected_indices(skills: &[Skill], query: &str) -> Vec<(usize, Score)> {
    let query_tokens = meaningful_tokens(query);
    if query_tokens.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(usize, Score)> = Vec::new();
    for (i, skill) in skills.iter().enumerate() {
        let s = score_tokens(skill, &query_tokens);
        if s.selected() {
            out.push((i, s));
        }
    }
    out.sort_by(|a, b| {
        b.1.name_hits
            .cmp(&a.1.name_hits)
            .then(b.1.meta_hits.cmp(&a.1.meta_hits))
            .then(a.0.cmp(&b.0))
    });
    out
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
