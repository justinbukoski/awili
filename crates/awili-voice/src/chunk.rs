//! Sentence chunking for streaming read-aloud (slice-3 spec §3).
//!
//! Read-aloud must begin speaking before the assistant has finished generating, so
//! tokens are fed in as they stream and the chunker emits *complete sentences* the
//! moment they close. Pure and fully unit-tested — the part of TTS that can be
//! wrong is the boundary logic, not the audio.
//!
//! A sentence ends at `.`/`!`/`?` (optionally followed by closing quotes/brackets)
//! when the next character is whitespace. A terminator at the very end of the
//! buffered text is HELD, not emitted — mid-stream we cannot tell whether the next
//! token continues it (`3` then `.14`); [`flush`](SentenceChunker::flush) speaks
//! whatever remains at end-of-stream. Three guards keep it from
//! cutting mid-thought: a period immediately followed by a digit is treated as a
//! decimal point (`3.14`); a single letter before the period — preceded by a
//! boundary *or another period* — is taken to be an initial or acronym letter
//! (`J. Smith`, `U.S.`, `e.g.`); and a small known-abbreviation list keeps common
//! titles (`Dr.`, `Mrs.`, `Prof.` — review slice-3 nit 1) from ending a chunk.

/// Accumulates streamed text and yields complete sentences as their boundaries
/// arrive. Anything after the last boundary stays buffered until [`flush`] or the
/// next boundary.
#[derive(Debug, Default)]
pub struct SentenceChunker {
    buf: String,
}

impl SentenceChunker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of streamed text; return every complete sentence that closed
    /// as a result, trimmed. Text past the last boundary remains buffered.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.buf.push_str(text);
        let mut out = Vec::new();
        while let Some(end) = self.next_boundary() {
            let sentence: String = self.buf.drain(..end).collect();
            let trimmed = sentence.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
        out
    }

    /// Emit whatever remains buffered (the final, possibly unterminated sentence)
    /// at end-of-stream. Returns `None` if only whitespace remains.
    pub fn flush(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        let trimmed = rest.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    }

    /// Byte offset just past the end of the first complete sentence in `buf`, or
    /// `None` if no boundary has arrived yet.
    fn next_boundary(&self) -> Option<usize> {
        let chars: Vec<(usize, char)> = self.buf.char_indices().collect();
        for (idx, &(_byte_pos, ch)) in chars.iter().enumerate() {
            if !matches!(ch, '.' | '!' | '?') {
                continue;
            }
            // Decimal point: a '.' wedged between digits is not a sentence end.
            if ch == '.' {
                let prev_digit = idx > 0 && chars[idx - 1].1.is_ascii_digit();
                let next_digit = chars
                    .get(idx + 1)
                    .map(|c| c.1.is_ascii_digit())
                    .unwrap_or(false);
                if prev_digit && next_digit {
                    continue;
                }
                // Single-letter initial / acronym letter: "J. Smith", "U.S.".
                if is_initial(&chars, idx) {
                    continue;
                }
                // Known abbreviation before the period: "Dr.", "Mrs.", "e.g."
                // (review nit 1 — titles used to split into tiny speech chunks).
                if is_abbreviation(&chars, idx) {
                    continue;
                }
            }
            // Absorb any run of closing punctuation that belongs to the sentence:
            // quotes, brackets, and repeated terminators ("?!", "...").
            let mut end_char = idx;
            while let Some(&(_, c)) = chars.get(end_char + 1) {
                if matches!(
                    c,
                    '"' | '\'' | ')' | ']' | '}' | '.' | '!' | '?' | '”' | '’'
                ) {
                    end_char += 1;
                } else {
                    break;
                }
            }
            // Boundary only if followed by whitespace. A terminator at the end of
            // the buffer (next char `None`) is held: more streamed text may extend
            // it (a decimal, an ellipsis, a longer token). `flush` emits the tail.
            match chars.get(end_char + 1) {
                Some(&(_, c)) if c.is_whitespace() => {
                    return Some(chars[end_char].0 + chars[end_char].1.len_utf8());
                }
                _ => continue,
            }
        }
        None
    }
}

/// Is the period at `chars[idx]` part of a single-letter initial or an acronym
/// letter (the "J." in "J. Smith", or any "S." in "U.S.")? True when the char
/// before is a lone letter that is itself preceded by start-of-text, whitespace,
/// or another period (the acronym chain).
fn is_initial(chars: &[(usize, char)], idx: usize) -> bool {
    if idx == 0 {
        return false;
    }
    let prev = chars[idx - 1].1;
    if !prev.is_alphabetic() {
        return false;
    }
    match idx.checked_sub(2).map(|i| chars[i].1) {
        None => true,                             // "J." at the very start
        Some(c) => c.is_whitespace() || c == '.', // " J." or the "S." in "U.S."
    }
}

/// Common abbreviations that end in a period without ending a sentence: titles,
/// Latin shorthands, address/figure markers. Compared case-insensitively against
/// the word before the period. Deliberately small — a false negative merely
/// yields a shorter (still speakable, still ordered) chunk, while a false
/// positive could glue two real sentences together, so only unambiguous entries
/// belong here.
const ABBREVIATIONS: &[&str] = &[
    "dr", "mr", "mrs", "ms", "prof", "rev", "hon", "jr", "sr", "st", "mt", "vs", "etc", "approx",
    "dept", "fig", "inc", "ltd", "co",
];

/// Is the period at `chars[idx]` the end of a known abbreviation? Looks back over
/// the immediately preceding letter run (must itself start at a boundary) and
/// checks it against [`ABBREVIATIONS`].
fn is_abbreviation(chars: &[(usize, char)], idx: usize) -> bool {
    let mut start = idx;
    while start > 0 && chars[start - 1].1.is_alphabetic() {
        start -= 1;
    }
    if start == idx {
        return false; // no letters directly before the period
    }
    // The word must begin at start-of-text or after whitespace — "example.com"'s
    // "com" run is preceded by '.', so it is not consulted (and URLs are already
    // safe via the followed-by-whitespace rule).
    if start > 0 && !chars[start - 1].1.is_whitespace() {
        return false;
    }
    let word: String = chars[start..idx]
        .iter()
        .map(|&(_, c)| c.to_ascii_lowercase())
        .collect();
    ABBREVIATIONS.contains(&word.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stream `text` one char at a time, collecting all emitted sentences plus the
    /// final flush — models the worst-case token granularity.
    fn stream_by_char(text: &str) -> Vec<String> {
        let mut c = SentenceChunker::new();
        let mut out = Vec::new();
        for ch in text.chars() {
            out.extend(c.push(&ch.to_string()));
        }
        out.extend(c.flush());
        out
    }

    #[test]
    fn splits_basic_sentences() {
        let mut c = SentenceChunker::new();
        let got = c.push("Hello there. How are you? I am fine!");
        assert_eq!(got, vec!["Hello there.", "How are you?"]);
        // The last, unterminated-by-trailing-space sentence flushes out.
        assert_eq!(c.flush().as_deref(), Some("I am fine!"));
    }

    #[test]
    fn boundary_requires_following_whitespace() {
        // A period with no following whitespace yet is NOT a boundary — more text
        // may make it a decimal or a longer token. flush() speaks the tail.
        let mut c = SentenceChunker::new();
        assert!(c.push("Wait").is_empty());
        assert!(c.push(".").is_empty()); // held: could be "..." or end
        let got = c.push(" Go.");
        assert_eq!(got, vec!["Wait."]); // "Go." held — at buffer end
        assert_eq!(c.flush().as_deref(), Some("Go."));
    }

    #[test]
    fn decimal_points_do_not_split() {
        let got = stream_by_char("Pi is 3.14 today. Done.");
        assert_eq!(got, vec!["Pi is 3.14 today.", "Done."]);
    }

    #[test]
    fn single_letter_initials_do_not_split() {
        let got = stream_by_char("Ask J. Smith now. Thanks.");
        assert_eq!(got, vec!["Ask J. Smith now.", "Thanks."]);
    }

    #[test]
    fn absorbs_trailing_quotes_and_repeated_terminators() {
        // A closing quote after a terminator is absorbed into the chunk, and a run
        // of terminators ("?!") counts as one boundary. (Dialogue punctuation is
        // not literary-perfect — "Stop!" splits from "she said" — but every chunk
        // is independently speakable, which is all read-aloud needs.)
        let mut c = SentenceChunker::new();
        let got = c.push("\"Stop!\" she said. Really?! Yes.");
        assert_eq!(got, vec!["\"Stop!\"", "she said.", "Really?!"]);
        assert_eq!(c.flush().as_deref(), Some("Yes."));
    }

    #[test]
    fn streaming_one_char_at_a_time_matches_whole() {
        let got = stream_by_char("One. Two. Three.");
        assert_eq!(got, vec!["One.", "Two.", "Three."]);
    }

    #[test]
    fn flush_returns_none_for_empty_or_whitespace() {
        let mut c = SentenceChunker::new();
        assert_eq!(c.flush(), None);
        c.push("   ");
        assert_eq!(c.flush(), None);
    }

    // ---- nit 1: abbreviation policy ---------------------------------------

    #[test]
    fn titles_do_not_split() {
        // The review's example verbatim, plus more titles, streamed per-char.
        assert_eq!(stream_by_char("Ask Dr. Smith."), vec!["Ask Dr. Smith."]);
        assert_eq!(
            stream_by_char("Meet Mrs. Jones and Prof. Lee today. Then rest."),
            vec!["Meet Mrs. Jones and Prof. Lee today.", "Then rest."]
        );
        // Case-insensitive: whisper/LLM output may vary capitalisation.
        assert_eq!(
            stream_by_char("see dr. smith now. ok."),
            vec!["see dr. smith now.", "ok."]
        );
    }

    #[test]
    fn acronyms_do_not_split_internally() {
        // Every '.' inside "U.S." is an acronym-letter period (chained initials),
        // so the sentence holds together until its real terminator.
        assert_eq!(
            stream_by_char("The U.S. economy grew. Inflation fell."),
            vec!["The U.S. economy grew.", "Inflation fell."]
        );
        assert_eq!(
            stream_by_char("Wake at 6 a.m. sharp. Go."),
            vec!["Wake at 6 a.m. sharp.", "Go."]
        );
    }

    #[test]
    fn urls_and_filenames_do_not_split() {
        // "example.com" never has whitespace after its inner periods, and the
        // "com" run is preceded by '.', not whitespace — both rules protect it.
        assert_eq!(
            stream_by_char("See example.com for docs. Thanks."),
            vec!["See example.com for docs.", "Thanks."]
        );
        assert_eq!(
            stream_by_char("Open main.rs first. Then build."),
            vec!["Open main.rs first.", "Then build."]
        );
    }

    #[test]
    fn a_number_ending_a_sentence_still_splits() {
        // The decimal guard must require a digit on BOTH sides of the '.'. A period
        // that merely *follows* a digit but is then followed by whitespace closes a
        // real sentence ("...is 42." then a new one) — it is not a decimal point.
        // Every other digit test keeps the terminating period after a letter
        // ("3.14 today.", "v1.2.3 in prod."), so weakening the guard to "preceded by
        // a digit" alone would glue a number-ending sentence onto the next one and
        // read them aloud as one run, yet survive the rest of the suite.
        let got = stream_by_char("The answer is 42. Yes.");
        assert_eq!(got, vec!["The answer is 42.", "Yes."]);
    }

    #[test]
    fn version_numbers_do_not_split() {
        assert_eq!(
            stream_by_char("Use v1.2.3 in prod. It is stable."),
            vec!["Use v1.2.3 in prod.", "It is stable."]
        );
    }

    #[test]
    fn unknown_abbreviations_still_split_safely() {
        // A word not on the list still ends the sentence — a shorter chunk is the
        // safe failure mode (audible and ordered), never a glued one.
        assert_eq!(
            stream_by_char("It was huge. Next."),
            vec!["It was huge.", "Next."]
        );
    }

    #[test]
    fn abbreviation_word_must_begin_at_a_boundary_not_after_a_dot() {
        // The abbreviation guard only treats a word as an abbreviation when that
        // word starts at start-of-text or right after whitespace. A country-code
        // domain like "example.co" ends its "co" run against a '.', not
        // whitespace, so even though "co" is on the ABBREVIATIONS list the period
        // after it is a real sentence end — the guard must reject it. Every
        // abbreviation test places the word after a space ("Dr.", "Mrs.",
        // "Prof."), so dropping the `!chars[start - 1].is_whitespace()` guard
        // would let "co" match and glue "example.co." onto the next sentence, yet
        // survive the rest of the suite (no other test abuts a listed abbrev
        // against a non-space char).
        assert_eq!(
            stream_by_char("Visit example.co. Then leave."),
            vec!["Visit example.co.", "Then leave."]
        );
    }

    #[test]
    fn leading_period_is_a_boundary_not_an_initial() {
        // A '.' at the very start of the buffer (idx 0) has no preceding char to
        // inspect, so `is_initial` must short-circuit on `idx == 0` before it
        // reaches `chars[idx - 1]`. Whisper/LLM output can begin a stream with a
        // stray terminator ("Uh. ..." mis-transcribed, a leading ellipsis, or a
        // sentence fragment starting with a period). The real code emits the lone
        // "." as its own (still speakable) chunk and moves on; without the
        // `idx == 0` guard the `chars[idx - 1]` access underflows and panics on
        // this input. No other test begins its buffer with a period, so removing
        // the guard fails ONLY this test — the seam is otherwise unpinned.
        let got = stream_by_char(". Hello. World.");
        assert_eq!(got, vec![".", "Hello.", "World."]);
    }

    #[test]
    fn closing_bracket_after_terminator_is_absorbed_into_the_chunk() {
        // The absorb loop (chunk.rs §"Absorb any run of closing punctuation")
        // pulls not just quotes and repeated terminators but also closing
        // BRACKETS — ')', ']', '}' — into the sentence, so a parenthetical whose
        // own period closes just before the paren keeps the ')' attached rather
        // than orphaning it onto the next chunk. Every other absorb test uses a
        // quote ('"') or a terminator run ("?!", "..."); no test ever ends a
        // sentence with a bracket, so the three bracket entries in the closer set
        // are wholly unpinned. Dropping ')' from the set makes the '.' inside
        // "(yes.)" a non-boundary (the following ')' is not whitespace), so the
        // whole input never splits and flushes as one glued run — yet the rest of
        // the suite still passes. Pin the bracket-absorb behaviour on ')'.
        let got = stream_by_char("Well (yes.) No.");
        assert_eq!(got, vec!["Well (yes.)", "No."]);
    }

    #[test]
    fn ellipsis_is_one_boundary() {
        // "..." is absorbed as a single terminator run; the trailing-off chunk is
        // still fine to speak on its own.
        let mut c = SentenceChunker::new();
        let got = c.push("Well... maybe. Okay.");
        assert_eq!(got, vec!["Well...", "maybe."]);
        assert_eq!(c.flush().as_deref(), Some("Okay."));
    }

    #[test]
    fn curly_smart_quote_closers_are_absorbed_into_the_chunk() {
        // LLM output routinely uses typographic “smart quotes” (U+201D ” and
        // U+2019 ’) rather than ASCII " and '. The absorb loop (chunk.rs
        // §"Absorb any run of closing punctuation") lists both curly closers,
        // but every other absorb test uses straight quotes — so dropping '”'/'’'
        // from the closer set leaves the whole suite green. Without them the
        // terminator inside the quote is still a boundary but the trailing curly
        // quote is not absorbed: it fails the following-whitespace check (the next
        // char is the quote, not space), so the boundary is missed and the quote
        // orphans onto the next chunk. Pin curly-closer absorb on both closers.
        let mut c = SentenceChunker::new();
        let got = c.push("“Yes.” No.");
        assert_eq!(got, vec!["“Yes.”"]);
        assert_eq!(c.flush().as_deref(), Some("No."));

        // Curly single close-quote (’) travels the same absorb path.
        let mut c2 = SentenceChunker::new();
        let got2 = c2.push("He said ‘go.’ Now.");
        assert_eq!(got2, vec!["He said ‘go.’"]);
        assert_eq!(c2.flush().as_deref(), Some("Now."));
    }
}
