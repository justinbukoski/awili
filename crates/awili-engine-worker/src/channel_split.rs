//! RC2-R4 (S-M1 re-measure, 2026-10-08: one of fifteen Gemma replies showed
//! `<|channel>5000 <channel|>` in the middle of its answer). Gemma 4 marks
//! its thinking with `<|channel>` … `<channel|>`; when the model server
//! leaves those spans in the reply text, the owner reads them. The split
//! moves every span (markers included) out of the reply and into the turn's
//! reasoning, also when a marker arrives cut across two stream pieces. Only
//! what is shown changes; the model's own transcript is untouched.
//!
//! RC2-R4M3 (slice review P2): only a COMPLETED span moves. Text after a
//! `<|channel>` is held until its `<channel|>` arrives; when the reply pauses
//! or ends first (or a second `<|channel>` starts), the marker was literal
//! and it and the held text go out as reply text. And only a Gemma model's
//! reply is split at all: another model's reply passes through unchanged.

const OPEN: &str = "<|channel>";
const CLOSE: &str = "<channel|>";

/// One piece of a reply after the split: reply text or thinking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    Text(String),
    Thinking(String),
}

/// The split state of one turn's reply stream.
#[derive(Debug)]
pub(crate) struct ChannelSplit {
    /// RC2-R4M3: false for a model that does not use these markers.
    enabled: bool,
    /// After a `<|channel>` with no `<channel|>` yet: the text since it.
    open: Option<String>,
    /// The end of the last piece when it may be the start of a marker.
    held: String,
}

impl Default for ChannelSplit {
    fn default() -> Self {
        ChannelSplit {
            enabled: true,
            open: None,
            held: String::new(),
        }
    }
}

impl ChannelSplit {
    /// RC2-R4M3: the split for one turn of `model`: on for Gemma (the only
    /// model known to write these markers), off for every other model.
    pub(crate) fn for_model(model: &str) -> ChannelSplit {
        ChannelSplit {
            enabled: model.to_ascii_lowercase().contains("gemma"),
            ..ChannelSplit::default()
        }
    }

    /// Split the next piece of reply text.
    pub(crate) fn push(&mut self, text: &str) -> Vec<Piece> {
        let mut out = Vec::new();
        if !self.enabled {
            push_piece(&mut out, Piece::Text(text.to_string()));
            return out;
        }
        let mut buf = std::mem::take(&mut self.held);
        buf.push_str(text);
        loop {
            match self.open.take() {
                None => {
                    if let Some(at) = buf.find(OPEN) {
                        push_piece(&mut out, Piece::Text(buf[..at].to_string()));
                        buf.drain(..at + OPEN.len());
                        self.open = Some(String::new());
                        continue;
                    }
                    let cut = buf.len() - marker_start_len(&buf, &[OPEN]);
                    push_piece(&mut out, Piece::Text(buf[..cut].to_string()));
                    self.held = buf[cut..].to_string();
                    return out;
                }
                Some(mut span) => {
                    let close = buf.find(CLOSE);
                    let reopen = buf.find(OPEN);
                    match (close, reopen) {
                        (Some(c), r) if r.is_none_or(|r| c < r) => {
                            span.push_str(&buf[..c]);
                            push_piece(&mut out, Piece::Thinking(span));
                            buf.drain(..c + CLOSE.len());
                        }
                        (_, Some(r)) => {
                            // A second `<|channel>` before any close: the
                            // first was literal reply text.
                            span.push_str(&buf[..r]);
                            push_piece(&mut out, Piece::Text(format!("{OPEN}{span}")));
                            buf.drain(..r + OPEN.len());
                            self.open = Some(String::new());
                        }
                        _ => {
                            // Neither marker yet: keep the span open.
                            let cut = buf.len() - marker_start_len(&buf, &[OPEN, CLOSE]);
                            span.push_str(&buf[..cut]);
                            self.open = Some(span);
                            self.held = buf[cut..].to_string();
                            return out;
                        }
                    }
                }
            }
        }
    }

    /// The reply stream paused or ended: what was held back goes out, and a
    /// `<|channel>` still waiting for its close was literal reply text.
    pub(crate) fn flush(&mut self) -> Vec<Piece> {
        let held = std::mem::take(&mut self.held);
        let mut out = Vec::new();
        match self.open.take() {
            Some(span) => push_piece(&mut out, Piece::Text(format!("{OPEN}{span}{held}"))),
            None => push_piece(&mut out, Piece::Text(held)),
        }
        out
    }
}

/// How many trailing bytes of `buf` could still grow into one of `markers`.
fn marker_start_len(buf: &str, markers: &[&str]) -> usize {
    let longest = markers.iter().map(|m| m.len()).max().unwrap_or(0);
    (1..longest)
        .rev()
        .find(|n| {
            buf.len() >= *n
                && buf.is_char_boundary(buf.len() - n)
                && markers
                    .iter()
                    .any(|m| m.len() > *n && m.starts_with(&buf[buf.len() - n..]))
        })
        .unwrap_or(0)
}

fn push_piece(out: &mut Vec<Piece>, piece: Piece) {
    match &piece {
        Piece::Text(s) | Piece::Thinking(s) if s.is_empty() => {}
        _ => out.push(piece),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(pieces: &[Piece]) -> String {
        pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Text(t) => Some(t.as_str()),
                Piece::Thinking(_) => None,
            })
            .collect()
    }

    /// RC2-R4: the S-M1 reply, whole and cut into every two pieces: the
    /// thinking span never reaches the reply text, the reply is intact.
    #[test]
    fn rc2r4_a_gemma_thinking_span_never_reaches_the_reply() {
        let reply = "I will first recall it.\n\n<|channel>5000\n<channel|>\n\nUnderstood. Plan:";
        let want = "I will first recall it.\n\n\n\nUnderstood. Plan:";
        let mut s = ChannelSplit::default();
        let mut all = s.push(reply);
        all.extend(s.flush());
        assert_eq!(text(&all), want);
        assert!(all.contains(&Piece::Thinking("5000\n".into())), "{all:?}");
        for cut in 0..=reply.len() {
            if !reply.is_char_boundary(cut) {
                continue;
            }
            let mut s = ChannelSplit::default();
            let mut all = s.push(&reply[..cut]);
            all.extend(s.push(&reply[cut..]));
            all.extend(s.flush());
            assert_eq!(text(&all), want, "cut at {cut}");
        }
        // Plain text, and a lone "<" or "<|", pass through unchanged.
        let mut s = ChannelSplit::default();
        let mut all = s.push("a < b <| c <");
        all.extend(s.flush());
        assert_eq!(text(&all), "a < b <| c <");
    }

    /// RC2-R4: through the worker's event host, a Gemma thinking span cut
    /// across stream pieces reaches the app as reasoning, never as reply
    /// tokens; what was held back goes out before the next tool event.
    #[test]
    fn rc2r4_the_host_sends_a_thinking_span_as_reasoning() {
        use awili_engine::events::AgentEvent;
        use awili_seam::TurnHost;
        use awili_work::WorkEvent;
        let (tx, rx) = async_channel::unbounded();
        let (_ctx, crx) = std::sync::mpsc::channel();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut host = crate::ChannelHost {
            events: &tx,
            cmds: &crx,
            cancel: &cancel,
            model: "m".into(),
            turn_id: "t".into(),
            workdir: String::new(),
            call_names: Default::default(),
            call_args: Default::default(),
            may_change: Default::default(),
            shutdown_seen: false,
            disconnected: false,
            stop_after_deny: false,
            approval_mode: Default::default(),
            any_folder: false,
            channel_split: Default::default(),
            hooks: crate::HostHooks::default(),
        };
        for piece in [
            "Checking.\n<|chan",
            "nel>5000\n<chan",
            "nel|>Plan: one.",
            " <",
        ] {
            host.observe(&AgentEvent::TextDelta(piece.into())).unwrap();
        }
        host.observe(&AgentEvent::TurnStarted).unwrap();
        let (mut tokens, mut reasoning) = (String::new(), String::new());
        while let Ok(ev) = rx.try_recv() {
            match ev {
                WorkEvent::Token(t) => tokens.push_str(&t),
                WorkEvent::ReasoningDelta { text } => reasoning.push_str(&text),
                _ => {}
            }
        }
        assert_eq!(tokens, "Checking.\nPlan: one. <");
        assert_eq!(reasoning, "5000\n");
    }

    fn split_all(s: &mut ChannelSplit, parts: &[&str]) -> Vec<Piece> {
        let mut all = Vec::new();
        for p in parts {
            all.extend(s.push(p));
        }
        all.extend(s.flush());
        all
    }

    fn thinking(pieces: &[Piece]) -> String {
        pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Thinking(t) => Some(t.as_str()),
                Piece::Text(_) => None,
            })
            .collect()
    }

    /// RC2-R4M3 (slice review P2): a literal `<|channel>` with no close is
    /// reply text: the rest of the answer is not diverted to the reasoning,
    /// whole or cut at every character boundary.
    #[test]
    fn rc2r4m3_an_unmatched_literal_marker_stays_in_the_reply() {
        let reply =
            "Gemma marks thinking with <|channel> — the rest of this answer must stay visible.";
        for cut in 0..=reply.len() {
            if !reply.is_char_boundary(cut) {
                continue;
            }
            let all = split_all(
                &mut ChannelSplit::for_model("/models/gemma-4-26b"),
                &[&reply[..cut], &reply[cut..]],
            );
            assert_eq!(text(&all), reply, "cut at {cut}");
            assert_eq!(thinking(&all), "", "cut at {cut}");
        }
        // A literal marker, then a real span: only the completed span moves.
        let reply = "a <|channel> b <|channel>x<channel|> c";
        let all = split_all(&mut ChannelSplit::default(), &[reply]);
        assert_eq!(text(&all), "a <|channel> b  c");
        assert_eq!(thinking(&all), "x");
        // A lone close marker is reply text too.
        let all = split_all(&mut ChannelSplit::default(), &["a <channel|> b"]);
        assert_eq!(text(&all), "a <channel|> b");
    }

    /// RC2-R4M3: only Gemma's replies are split; another model's reply that
    /// happens to contain both markers passes through unchanged.
    #[test]
    fn rc2r4m3_only_a_gemma_reply_is_split() {
        let reply = "x <|channel>5000<channel|> y";
        for model in ["qwen3:8b", "gpt-oss-20b", "m"] {
            let all = split_all(
                &mut ChannelSplit::for_model(model),
                &["x <|chan", "nel>5000<channel|> y"],
            );
            assert_eq!(text(&all), reply, "{model}");
            assert_eq!(thinking(&all), "", "{model}");
        }
        for model in ["/models/gemma-4-26b", "Gemma-4-12B-it"] {
            let all = split_all(&mut ChannelSplit::for_model(model), &[reply]);
            assert_eq!(text(&all), "x  y", "{model}");
            assert_eq!(thinking(&all), "5000", "{model}");
        }
    }
}
