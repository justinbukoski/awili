//! Display-only separation of Awili's legacy approval notices from prose.
//! Stored messages, Copy, exports and model history keep the original source.
//! This never classifies authority: the live approval card still owns consent.

#[derive(Default)]
pub(crate) struct Presentation {
    pub(crate) body: String,
    pub(crate) activity: Vec<String>,
}

/// Only the exact legacy notice prefixes are folded. Ordinary quotes, failures,
/// and fenced examples remain in the answer. An unquoted line after a notice
/// starts prose immediately, avoiding CommonMark's lazy quote continuation.
pub(crate) fn separate(source: &str) -> Presentation {
    if source.len() > crate::markdown::MAX_SOURCE_BYTES
        || (!source.contains("> **⚠ Approval needed:** ") && !source.contains("> ✓ **"))
    {
        return Presentation {
            body: source.to_owned(),
            activity: Vec::new(),
        };
    }
    // The real CommonMark parser protects quoted, nested and indented code
    // examples, including fences longer than three backticks.
    let mut code_ranges = pulldown_cmark::Parser::new(source)
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::CodeBlock(_)) => Some(range),
            _ => None,
        })
        .peekable();
    let mut out = Presentation::default();
    let mut notice = false;
    let mut command = false;
    let mut first_command_line = false;
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let end = offset + line.len();
        while code_ranges.peek().is_some_and(|r| r.end <= offset) {
            code_ranges.next();
        }
        let in_code = code_ranges
            .peek()
            .is_some_and(|r| r.start < end && r.end > offset);
        offset = end;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if command || (notice && trimmed == "> ```") {
            // Legacy emitters prefix the command once, not each embedded
            // newline. Capture the complete fence, including blank/bare >
            // lines, without applying markdown stripping to its contents.
            let opening = !command;
            let closing = command && trimmed == "> ```";
            if let Some(last) = out.activity.last_mut() {
                last.push('\n');
                last.push_str(if opening || closing || first_command_line {
                    trimmed.strip_prefix("> ").unwrap_or(trimmed)
                } else {
                    trimmed
                });
            }
            first_command_line = opening;
            if opening || closing {
                command = !command;
            }
            notice = false;
        } else if !in_code && out.activity.len() < crate::markdown::MAX_BLOCKS && is_notice(trimmed)
        {
            notice = trimmed.starts_with("> **⚠ Approval needed:** ");
            out.activity
                .push(trimmed.strip_prefix("> ").unwrap_or(trimmed).to_owned());
        } else {
            notice = false;
            out.body.push_str(line);
        }
    }
    if command {
        // A truncated/reveal prefix must fail visible, not hide the answer.
        return Presentation {
            body: source.to_owned(),
            activity: Vec::new(),
        };
    }
    out
}

/// Remove only the known label's markdown, never glob patterns or emphasis
/// appearing in command arguments or subsequent command lines.
pub(crate) fn activity_label(source: &str) -> String {
    let (label, rest) = source.split_once('\n').unwrap_or((source, ""));
    let mut display = label.replacen("**", "", 2);
    if !rest.is_empty() {
        display.push('\n');
        display.push_str(rest);
    }
    display
}

fn is_notice(line: &str) -> bool {
    line.starts_with("> **⚠ Approval needed:** ")
        || [
            "> ✓ **Allowed once**",
            "> ✓ **Allowed for this session**",
            "> ✓ **Allowed for this folder**",
            "> ✓ **Always allowed**",
        ]
        .iter()
        .any(|prefix| {
            line == *prefix
                || line
                    .strip_prefix(prefix)
                    .is_some_and(|s| s.starts_with(" — "))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_notice_does_not_swallow_the_answer() {
        let source = "\n> **⚠ Approval needed:** Run curl\n\n> ✓ **Allowed for this session** — until you switch conversations\nThe answer has **bold**, [a link](https://example.com), and 日本語.\n\n### Prices\n* $100\n";
        let p = separate(source);
        assert_eq!(p.activity.len(), 2);
        assert_eq!(p.body, "\n\nThe answer has **bold**, [a link](https://example.com), and 日本語.\n\n### Prices\n* $100\n");
        assert!(source.contains("Approval needed"));
    }

    #[test]
    fn ordinary_quotes_failures_and_code_are_preserved() {
        for source in [
            "> A real quotation\ncontinues here\n",
            "> **⚠ tool FAILED**\n\nThe reason.\n",
            "````md\n> **⚠ Approval needed:** Example\n```\n> ✓ **Allowed once**\n````\n",
            "~~~text\n> ✓ **Allowed once**\n~~~\n",
            "> ```md\n> ✓ **Allowed once**\n> ```\n",
            "> > ```md\n> > ✓ **Allowed once**\n> > ```\n",
            "    > ✓ **Allowed once**\n",
            "> ✗ **Denied**\n",
        ] {
            let p = separate(source);
            assert_eq!(p.body, source);
            assert!(p.activity.is_empty());
        }
    }

    #[test]
    fn quotation_after_a_notice_is_not_folded() {
        let p = separate("> ✓ **Allowed once**\n> This is the quoted answer.\n");
        assert_eq!(p.body, "> This is the quoted answer.\n");
        assert_eq!(p.activity.len(), 1);
    }

    #[test]
    fn command_and_repeated_notices_remain_available() {
        let p = separate("> **⚠ Approval needed:** Run\n> ```\n> echo hi\n> ```\n\n> ✓ **Allowed once**\nDone.\n\n> **⚠ Approval needed:** Next\n");
        assert_eq!(p.activity.len(), 3);
        assert!(p.activity[0].contains("echo hi"));
        assert_eq!(p.body, "\nDone.\n\n");
    }
    #[test]
    fn verdict_followed_by_quoted_code_preserves_the_answer() {
        for fence in ["```", "```py"] {
            let code = format!("> {fence}\n> print(1)\n> ```\n");
            let p = separate(&format!("> ✓ **Allowed once**\n{code}"));
            assert_eq!(p.body, code);
        }
        let open = "> **⚠ Approval needed:** Run\n> ```\n> echo hi\nAn answer.\n";
        let p = separate(open);
        assert_eq!(p.body, open);
        assert!(p.activity.is_empty());
    }

    #[test]
    fn multiline_legacy_command_and_globs_are_preserved() {
        // Same framing as work_event_updates and gateway_event_updates:
        // format!("\n> **⚠ Approval needed:** {desc}\n> ```\n> {command}\n> ```\n").
        let command = "rg foo src/**/*.rs\n\n>\n> out.txt\necho done";
        let source =
            format!("\n> **⚠ Approval needed:** Run\n> ```\n> {command}\n> ```\nAn answer.\n");
        let p = separate(&source);
        assert_eq!(p.body, "\nAn answer.\n");
        assert_eq!(p.activity.len(), 1);
        let display = activity_label(&p.activity[0]);
        assert!(display.contains(command));
        assert!(display.starts_with("⚠ Approval needed: Run"));
    }
}

#[cfg(test)]
mod row_tests {
    use crate::{markdown_plan::PlanBlock, MessageRow, Role};
    use awili_core::ConversationState;

    #[test]
    fn response_presentation_retains_source_and_separates_only_assistant_activity() {
        let text = "> ✓ **Allowed once**\nAn answer with **emphasis** and [a link](https://example.com).\n\n* One\n* Two\n";
        for role in [Role::Assistant, Role::User] {
            let conversation = ConversationState::seeded([(role, text.to_owned())]);
            let mut row = MessageRow::new(conversation.message(0).unwrap());
            assert_eq!(row.raw.as_ref(), text, "Copy keeps the exact source");
            assert_eq!(conversation.message(0).unwrap().text, text);
            if role == Role::Assistant {
                assert_eq!(row.presentation_activity.len(), 1);
                assert!(matches!(&row.plan[0], PlanBlock::Text { text, .. }
                    if text.text.starts_with("An answer")));
                assert!(matches!(&row.plan[1], PlanBlock::List { items, .. } if items.len() == 2));
            } else {
                assert!(row.presentation_activity.is_empty());
                assert!(matches!(&row.plan[0], PlanBlock::Quote(_)));
            }
            row.replan_display();
            assert_eq!(row.raw.as_ref(), text, "Palette refresh keeps Copy exact");
        }
    }
}
