//! Incremental, transport-free decoders that turn raw response bytes into
//! [`ProviderEvent`]s. These are the contract-tested core (slice-2 spec §6): they
//! never touch the network, so the exact event sequence for every provider is
//! pinned by fixtures.

use crate::event::ProviderEvent;

/// Accumulates bytes and yields complete `\n`-terminated lines, retaining any
/// trailing partial line until more bytes arrive. Shared by the SSE and NDJSON
/// decoders — both are line-oriented over a chunked byte stream.
///
/// The tail is held as raw bytes (not a `String`) so a multibyte UTF-8 char split
/// across two network chunks is reassembled intact rather than lossily replaced.
/// A completed line is decoded with `from_utf8_lossy` — by then it is whole.
#[derive(Default)]
pub struct LineBuffer {
    buf: Vec<u8>,
    /// S7a (F0782): set once a line outgrew [`MAX_LINE_BYTES`]. From then on
    /// every byte is discarded and nothing is buffered again.
    overflowed: bool,
}

/// S7a (F0782): the most bytes one stream line (or one assembled SSE event)
/// may hold. 4 MiB: a normal streamed delta is under 1 KiB, and even a
/// whole 128k-token reply sent as ONE JSON line (about 512 KiB of text,
/// at most about 1 MiB once JSON-escaped) fits four times over. Without a
/// cap, an endpoint that never sends a newline grows the buffer until the
/// app runs out of memory.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// The owner-visible error when a stream line or event passes
/// [`MAX_LINE_BYTES`]. Plain words, no paths; matches no fallback keyword
/// in `classify_error`, so it never triggers an automatic retry.
pub const STREAM_TOO_LARGE: &str =
    "The model's reply stream sent one piece larger than 4 MB, so Awili stopped reading it.";

/// S7a: report [`STREAM_TOO_LARGE`] and the terminal `Done`, once per stream.
pub(crate) fn push_too_large(done_emitted: &mut bool, out: &mut Vec<ProviderEvent>) {
    if !*done_emitted {
        *done_emitted = true;
        out.push(ProviderEvent::Error(STREAM_TOO_LARGE.to_string()));
        out.push(ProviderEvent::Done);
    }
}

impl LineBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk; returns every newly-completed line (without the trailing
    /// `\n`, and with a trailing `\r` stripped so `\r\n` framing is handled).
    ///
    /// S7a (F0782): a line longer than [`MAX_LINE_BYTES`] (every byte before
    /// its `\n`, a trailing `\r` included) is never buffered.
    /// Lines completed before it are still returned; the buffer is freed,
    /// [`overflowed`](Self::overflowed) turns true, and every later byte is
    /// discarded.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        if self.overflowed {
            return lines;
        }
        let mut rest = chunk;
        while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
            if self.buf.len() + nl > MAX_LINE_BYTES {
                self.overflow();
                return lines;
            }
            self.buf.extend_from_slice(&rest[..nl]);
            let mut line = std::mem::take(&mut self.buf);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
            rest = &rest[nl + 1..];
        }
        if self.buf.len() + rest.len() > MAX_LINE_BYTES {
            self.overflow();
            return lines;
        }
        self.buf.extend_from_slice(rest);
        lines
    }

    /// True once a line passed [`MAX_LINE_BYTES`] (S7a). Never resets.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    fn overflow(&mut self) {
        self.overflowed = true;
        self.buf = Vec::new();
    }

    /// Any buffered bytes not yet terminated by a newline (the final line of a
    /// body that did not end in `\n`).
    pub fn take_remainder(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            None
        } else {
            let bytes = std::mem::take(&mut self.buf);
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
    }
}

/// What a streaming decoder turns response bytes into. Implementors are stateful
/// (they remember partial frames and whether the terminal event was seen).
pub trait Decoder {
    /// Decode a chunk of body bytes into zero or more events.
    fn feed(&mut self, chunk: &[u8]) -> Vec<ProviderEvent>;
    /// Flush at end-of-body and guarantee a terminal [`ProviderEvent::Done`] if
    /// one was not already emitted (covers a truncated upstream stream).
    fn finish(&mut self) -> Vec<ProviderEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_across_chunks() {
        let mut lb = LineBuffer::new();
        assert_eq!(lb.feed(b"hel"), Vec::<String>::new());
        assert_eq!(lb.feed(b"lo\nwor"), vec!["hello".to_string()]);
        assert_eq!(lb.feed(b"ld\n"), vec!["world".to_string()]);
        assert_eq!(lb.take_remainder(), None);
    }

    #[test]
    fn strips_crlf() {
        let mut lb = LineBuffer::new();
        assert_eq!(
            lb.feed(b"a\r\nb\r\n"),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn remainder_holds_unterminated_tail() {
        let mut lb = LineBuffer::new();
        assert_eq!(lb.feed(b"done\ntail"), vec!["done".to_string()]);
        assert_eq!(lb.take_remainder(), Some("tail".to_string()));
    }

    /// The documented reason the tail is raw bytes, not a `String` (see the
    /// type doc): a multibyte UTF-8 char split across two `feed` chunks must be
    /// reassembled intact, never lossily replaced. A naive `String` tail would
    /// decode the leading half with `from_utf8_lossy` and corrupt the char to
    /// `�` — silently mangling every CJK/emoji token that straddles a network
    /// chunk boundary. The existing tests are all ASCII and would pass through
    /// that regression; this pins the invariant.
    #[test]
    fn reassembles_multibyte_char_split_across_chunks() {
        let mut lb = LineBuffer::new();
        // "日" = U+65E5 = e6 97 a5, split e6 97 | a5\n across two chunks.
        assert_eq!(lb.feed(b"\xe6\x97"), Vec::<String>::new());
        assert_eq!(lb.feed(b"\xa5\n"), vec!["日".to_string()]);
        // "😀" = U+1F600 = f0 9f 98 80, split f0 9f | 98 | 80\n across three chunks.
        assert_eq!(lb.feed(b"\xf0\x9f"), Vec::<String>::new());
        assert_eq!(lb.feed(b"\x98"), Vec::<String>::new());
        assert_eq!(lb.feed(b"\x80\n"), vec!["😀".to_string()]);
    }

    /// Same raw-bytes reassembly guarantee, but on the unterminated-tail path:
    /// a multibyte char split across chunks that ends the body with no trailing
    /// `\n` must come back intact from `take_remainder`, not half-decoded.
    #[test]
    fn remainder_reassembles_split_multibyte_tail() {
        let mut lb = LineBuffer::new();
        // "日" split e6 97 | a5, no trailing newline → lives in the tail.
        assert_eq!(lb.feed(b"\xe6\x97"), Vec::<String>::new());
        assert_eq!(lb.feed(b"\xa5"), Vec::<String>::new());
        assert_eq!(lb.take_remainder(), Some("日".to_string()));
    }

    /// S7a (F0782): an unterminated line is held up to the cap and never past
    /// it. One byte over frees the buffer, sets `overflowed`, and every later
    /// byte (even complete lines) is discarded.
    #[test]
    fn s7a_unterminated_line_is_capped() {
        let mut lb = LineBuffer::new();
        let chunk = vec![b'a'; 128 * 1024];
        for _ in 0..(MAX_LINE_BYTES / chunk.len()) {
            assert_eq!(lb.feed(&chunk), Vec::<String>::new());
            assert!(!lb.overflowed());
        }
        assert_eq!(lb.buf.len(), MAX_LINE_BYTES);
        assert_eq!(lb.feed(b"a"), Vec::<String>::new());
        assert!(lb.overflowed(), "one byte over the cap must overflow");
        assert_eq!(lb.buf.len(), 0, "the oversized line is freed");
        assert_eq!(lb.buf.capacity(), 0, "the oversized line is freed");
        for _ in 0..64 {
            assert_eq!(lb.feed(&chunk), Vec::<String>::new());
            assert_eq!(lb.buf.len(), 0, "nothing is buffered after overflow");
        }
        assert_eq!(lb.feed(b"ok\n"), Vec::<String>::new());
        assert_eq!(lb.take_remainder(), None);
    }

    /// S7a: a line of exactly `MAX_LINE_BYTES` bytes before its `\n` is still delivered,
    /// and lines completed before an overflow in the same chunk are kept.
    #[test]
    fn s7a_line_at_the_cap_is_kept_and_earlier_lines_survive_overflow() {
        let mut lb = LineBuffer::new();
        let mut at_cap = vec![b'b'; MAX_LINE_BYTES];
        at_cap.push(b'\n');
        let lines = lb.feed(&at_cap);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), MAX_LINE_BYTES);
        assert!(!lb.overflowed());

        let mut lb = LineBuffer::new();
        let mut mixed = b"first\n".to_vec();
        mixed.extend(vec![b'c'; MAX_LINE_BYTES + 1]);
        mixed.extend_from_slice(b"\nafter\n");
        assert_eq!(lb.feed(&mixed), vec!["first".to_string()]);
        assert!(lb.overflowed());
        assert_eq!(lb.buf.len(), 0);
    }
}
