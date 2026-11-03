//! Per-process parsing state for message boundaries in flattened transcripts.
use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::{CodeEngine, CodeEvent};

pub struct StreamParser {
    engine: CodeEngine,
    codex_message_seen: bool,
    completed_ids: HashSet<String>,
    /// Claude: reply text was already shown this turn, so the next text
    /// block starts after a blank line (RC2-RUN, round 6 Minor 3: "…the
    /// write.The folder has no hello.txt yet").
    claude_text_seen: bool,
    /// Claude: a text block has started (`content_block_start`) and none of
    /// its partial text has been shown yet.
    claude_block_fresh: bool,
    /// Claude: text blocks shown from partial deltas whose complete
    /// `assistant` copy has not arrived yet — that copy is dropped, never
    /// shown twice.
    claude_streamed_blocks: usize,
    /// Claude: each tool call's label by its `tool_use` id, until its result
    /// arrives (RC2-FIXB1).
    claude_tools: HashMap<String, String>,
    /// The model the turn was asked to run, to pick its context window out of
    /// a result that lists several models.
    model: Option<String>,
}

/// The content blocks of a Claude `assistant` or `user` event.
fn claude_blocks(value: &Value) -> &[Value] {
    value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

impl StreamParser {
    pub fn new(engine: CodeEngine) -> Self {
        Self {
            engine,
            codex_message_seen: false,
            completed_ids: HashSet::new(),
            claude_text_seen: false,
            claude_block_fresh: false,
            claude_streamed_blocks: 0,
            claude_tools: HashMap::new(),
            model: None,
        }
    }

    /// The model this turn was asked to run (`None` = the engine's default).
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    pub fn parse_line(&mut self, line: &str) -> Vec<CodeEvent> {
        match self.engine {
            CodeEngine::Codex => self.parse_codex_line(line),
            CodeEngine::Claude => self.parse_claude_line(line),
            _ => self.engine.parse_line_for_model(line, self.model.as_deref()),
        }
    }

    /// RC2-RUN (R3-20, R6-3): Claude runs with `--include-partial-messages`,
    /// so its reply text arrives as `stream_event` text deltas while it is
    /// written; each text block still arrives once more, complete, on an
    /// `assistant` event — that copy is dropped when its deltas were shown.
    /// A CLI that sends no deltas keeps the complete copies. Every text block
    /// after the first starts after a blank line. A subagent's partial text
    /// (`parent_tool_use_id` set) is not streamed; its complete copy is
    /// handled as before.
    fn parse_claude_line(&mut self, line: &str) -> Vec<CodeEvent> {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return self.engine.parse_line_for_model(line, self.model.as_deref());
        };
        let top_level = value.get("parent_tool_use_id").is_none_or(Value::is_null);
        match value.get("type").and_then(Value::as_str) {
            Some("stream_event") => {
                if top_level {
                    self.claude_partial(&value)
                } else {
                    Vec::new()
                }
            }
            Some("assistant") => {
                // RC2-FIXB1 (RC2-UX review P2): remember what each tool call
                // was, so its result can say which step finished.
                for block in claude_blocks(&value) {
                    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                        continue;
                    }
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                        let input = block.get("input").unwrap_or(&Value::Null);
                        self.claude_tools
                            .insert(id.to_string(), crate::claude_tool_label(name, input));
                    }
                }
                let mut out = Vec::new();
                for ev in self.engine.parse_line_for_model(line, self.model.as_deref()) {
                    match ev {
                        CodeEvent::AssistantText(text) => {
                            if top_level && self.claude_streamed_blocks > 0 {
                                self.claude_streamed_blocks -= 1;
                                continue;
                            }
                            if self.claude_text_seen {
                                out.push(CodeEvent::AssistantText("\n\n".into()));
                            }
                            self.claude_text_seen = true;
                            out.push(CodeEvent::AssistantText(text));
                        }
                        other => out.push(other),
                    }
                }
                out
            }
            // RC2-FIXB1 (RC2-UX review P2: a Code reply claimed a file
            // changed when the edit had only started): a tool result names
            // the step it finished ("✓ Edit src/a.rs", not "✓ done"), so what
            // a turn changed is read from the steps that finished well.
            Some("user") => {
                let mut out = Vec::new();
                for block in claude_blocks(&value) {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let label = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .and_then(|id| self.claude_tools.remove(id))
                        .unwrap_or_else(|| "done".to_string());
                    let failed = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    out.push(CodeEvent::ToolFinished { label, ok: !failed });
                }
                out
            }
            _ => self.engine.parse_line_for_model(line, self.model.as_deref()),
        }
    }

    /// One top-level Claude `stream_event`: a text block's start, and its
    /// text deltas as they arrive. Everything else (thinking, tool input,
    /// message start/stop) shows nothing.
    fn claude_partial(&mut self, value: &Value) -> Vec<CodeEvent> {
        let Some(event) = value.get("event") else {
            return Vec::new();
        };
        match event.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                self.claude_block_fresh =
                    event.pointer("/content_block/type").and_then(Value::as_str) == Some("text");
                Vec::new()
            }
            Some("content_block_delta") => {
                if event.pointer("/delta/type").and_then(Value::as_str) != Some("text_delta") {
                    return Vec::new();
                }
                let Some(text) = event
                    .pointer("/delta/text")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                else {
                    return Vec::new();
                };
                let mut out = Vec::new();
                if self.claude_block_fresh {
                    self.claude_block_fresh = false;
                    self.claude_streamed_blocks += 1;
                    if self.claude_text_seen {
                        out.push(CodeEvent::AssistantText("\n\n".into()));
                    }
                }
                self.claude_text_seen = true;
                out.push(CodeEvent::AssistantText(text.to_string()));
                out
            }
            _ => Vec::new(),
        }
    }

    fn parse_codex_line(&mut self, line: &str) -> Vec<CodeEvent> {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        // Public exec events carry snapshots. Render only the authoritative
        // completed snapshot, as before; partial updates are not text deltas.
        let assistant = value.get("type").and_then(Value::as_str) == Some("item.completed")
            && value.pointer("/item/type").and_then(Value::as_str) == Some("agent_message")
            && value
                .pointer("/item/text")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
        if !assistant {
            return self.engine.parse_line_for_model(line, self.model.as_deref());
        }
        // Missing IDs remain supported for older fixtures/releases: each
        // completed record is a distinct message, never deduplicated by prose.
        if let Some(id) = value
            .pointer("/item/id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            if !self.completed_ids.insert(id.to_string()) {
                return Vec::new();
            }
        }
        let mut events = Vec::new();
        if self.codex_message_seen {
            events.push(CodeEvent::AssistantText("\n\n".into()));
        }
        self.codex_message_seen = true;
        events.extend(self.engine.parse_line_for_model(line, self.model.as_deref()));
        events
    }
}
