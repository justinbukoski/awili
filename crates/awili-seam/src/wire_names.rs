//! RC1-F5: tool names on the OpenAI-compatible wire.
//!
//! The Chat Completions contract names a function with `[A-Za-z0-9_-]` only,
//! and real servers hold to it: mlx_lm's Gemma 4 tool parser
//! (`call:([\w-]+)`) drops a call named `fs.read` and streams
//! `finish_reason=tool_calls` with no call at all (live 2026-10-01, Studio
//! :8844). Awili's own names stay canonical (`fs.read`) everywhere else —
//! registry, authorization, approval prompts, the log, the transcript
//! history. Only the request body the model sees, and the call names it
//! sends back, are translated, per request, by one reversible map.

use std::collections::{BTreeMap, BTreeSet};

use awili_engine::wire::WireDelta;

/// `name` with every character outside `[A-Za-z0-9_-]` replaced by `_`.
pub(crate) fn wire_safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The canonical ↔ wire name map for one request's tools.
/// RC1-R-FIX1: the engine's own file tools, whose wire names are reserved.
const BUILTIN_FS: [&str; 5] = ["fs.read", "fs.write", "fs.edit", "fs.glob", "fs.grep"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WireNames {
    to_wire: BTreeMap<String, String>,
    from_wire: BTreeMap<String, String>,
}

impl WireNames {
    /// Build the map. A name that is already wire-safe keeps itself. The
    /// others, in sorted order, take [`wire_safe`] of themselves, or the
    /// first free `<that>_2`, `<that>_3`, … when it is taken (so `a_b` stays
    /// `a_b` and `a.b` becomes `a_b_2`, whatever order the tools come in).
    pub(crate) fn new<'a>(canonical: impl IntoIterator<Item = &'a str>) -> Self {
        let names: BTreeSet<&str> = canonical.into_iter().collect();
        let mut map = Self::default();
        // RC1-R-FIX1 (slice review P2): the engine's own file tools keep
        // their plain wire names (`fs_glob`, `fs_write`, …), which the in-app
        // brief names — another tool that is itself called `fs_glob` gets
        // `fs_glob_2`, never the builtin.
        for name in names.iter().filter(|n| BUILTIN_FS.contains(n)) {
            map.insert(name, wire_safe(name));
        }
        // RC2-ENG5 (S-R1): every safe name that is still free keeps itself
        // FIRST; only then do the safe names a builtin took look for a
        // suffix — so `fs_read` (taken by `fs.read`) can no longer grab
        // `fs_read_2` from a tool that is itself called `fs_read_2`.
        let safe: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| wire_safe(n) == *n && !BUILTIN_FS.contains(n))
            .collect();
        let mut displaced = Vec::new();
        for name in safe {
            if map.from_wire.contains_key(name) {
                displaced.push(name);
            } else {
                map.insert(name, name.to_string());
            }
        }
        for name in displaced {
            let wire = (2..)
                .map(|k| format!("{name}_{k}"))
                .find(|w| !map.from_wire.contains_key(w))
                .expect("an unbounded range always has a free name");
            map.insert(name, wire);
        }
        for name in names
            .iter()
            .filter(|n| wire_safe(n) != **n && !BUILTIN_FS.contains(n))
        {
            let base = wire_safe(name);
            let wire = std::iter::once(base.clone())
                .chain((2..).map(|k| format!("{base}_{k}")))
                .find(|w| !map.from_wire.contains_key(w))
                .expect("an unbounded range always has a free name");
            map.insert(name, wire);
        }
        map
    }

    /// The map for an OpenAI `tools` array (`[{type, function: {name}}]`).
    pub(crate) fn for_tools(tools: &[serde_json::Value]) -> Self {
        Self::new(
            tools
                .iter()
                .filter_map(|t| t.pointer("/function/name").and_then(|n| n.as_str())),
        )
    }

    fn insert(&mut self, canonical: &str, wire: String) {
        self.from_wire.insert(wire.clone(), canonical.to_string());
        self.to_wire.insert(canonical.to_string(), wire);
    }

    /// The wire name of a canonical name (a name outside the map is made
    /// wire-safe on its own).
    pub(crate) fn wire(&self, canonical: &str) -> String {
        self.to_wire
            .get(canonical)
            .cloned()
            .unwrap_or_else(|| wire_safe(canonical))
    }

    /// The canonical name of a wire name. A name outside the map comes back
    /// unchanged, so an unknown tool is still refused as unknown.
    pub(crate) fn canonical<'a>(&'a self, wire: &'a str) -> &'a str {
        self.from_wire.get(wire).map_or(wire, String::as_str)
    }

    /// Rewrite a request body in place: `tools[].function.name` and every
    /// `messages[].tool_calls[].function.name` take their wire names.
    pub(crate) fn encode_body(&self, body: &mut serde_json::Value) {
        let rename = |slot: Option<&mut serde_json::Value>| {
            if let Some(name) = slot {
                if let Some(s) = name.as_str() {
                    *name = serde_json::Value::String(self.wire(s));
                }
            }
        };
        if let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
            for t in tools {
                rename(t.pointer_mut("/function/name"));
            }
        }
        if let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for m in msgs {
                if let Some(calls) = m.get_mut("tool_calls").and_then(|c| c.as_array_mut()) {
                    for c in calls {
                        rename(c.pointer_mut("/function/name"));
                    }
                }
            }
        }
    }

    /// Give the model's tool-call names back their canonical form.
    pub(crate) fn decode(&self, deltas: Vec<WireDelta>) -> Vec<WireDelta> {
        deltas
            .into_iter()
            .map(|d| match d {
                WireDelta::ToolCallFragment {
                    choice,
                    tool_index,
                    id,
                    name,
                    args_fragment,
                } => WireDelta::ToolCallFragment {
                    choice,
                    tool_index,
                    id,
                    name: name.map(|n| self.canonical(&n).to_string()),
                    args_fragment,
                },
                other => other,
            })
            .collect()
    }
}
