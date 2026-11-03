//! Cowork command-palette catalog + substring filter (plan unit 28, Phase F,
//! review-05 Palette).
//!
//! The Cowork/Hermes desktop Cmd+K palette is a client-side static catalog
//! (baseGroups: Go-to routes, Command Center deep-links, Appearance, Settings
//! sections) plus four lazy server-backed search providers (recent sessions,
//! settings fields, MCP servers, archived chats), filtered by a custom
//! substring matcher where *every whitespace-separated word of the query must
//! literally appear* in the candidate text. This module is the PURE half: the
//! static catalog, the substring filter, and the provider filter/aggregation
//! logic. The actual fetches and the GPUI rendering + nested sub-pages are the
//! live/UI half (plan unit 29), out of scope here.
//!
//! Distinct from [`crate::palette`], Awili's native conversation palette, which
//! ranks conversations by recency and matches with a *subsequence* fuzzy
//! matcher. Here the algorithm is word-AND substring.
//!
//! Carries `#![allow(dead_code)]` until unit 29's Cmd+K UI consumes it (the
//! usage_chart/terminal_select precedent: pure module lands ahead of view).
#![allow(dead_code)]

/// Which of the four static baseGroups an entry belongs to (review-05 Palette).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogGroup {
    GoTo,
    CommandCenter,
    Appearance,
    Settings,
}

/// One hand-authored static catalog row. `id` is the stable action key the
/// GPUI layer (unit 29) maps to a navigation/command; `label` is displayed;
/// `keywords` widen what the substring filter matches without cluttering the
/// label; `keybind` is the optional shortcut hint shown at the row's right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    pub id: &'static str,
    pub group: CatalogGroup,
    pub label: &'static str,
    pub keywords: &'static [&'static str],
    pub keybind: Option<&'static str>,
}

/// The hand-authored static catalog, in display order (group by group).
pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "goto.new_conversation",
        group: CatalogGroup::GoTo,
        label: "New conversation",
        keywords: &["chat", "new"],
        keybind: Some("Cmd+N"),
    },
    CatalogEntry {
        id: "goto.feed",
        group: CatalogGroup::GoTo,
        label: "Feed",
        keywords: &["activity", "updates"],
        keybind: None,
    },
    CatalogEntry {
        id: "goto.command_center",
        group: CatalogGroup::GoTo,
        label: "Command Center",
        keywords: &["dashboard", "status"],
        keybind: None,
    },
    CatalogEntry {
        id: "goto.messaging",
        group: CatalogGroup::GoTo,
        label: "Messaging",
        keywords: &["connectors", "channels", "platforms"],
        keybind: None,
    },
    CatalogEntry {
        id: "goto.settings",
        group: CatalogGroup::GoTo,
        label: "Settings",
        keywords: &["preferences", "config"],
        keybind: Some("Cmd+,"),
    },
    CatalogEntry {
        id: "goto.skills",
        group: CatalogGroup::GoTo,
        label: "Skills",
        keywords: &["tools", "toolsets"],
        keybind: None,
    },
    CatalogEntry {
        id: "cc.sessions",
        group: CatalogGroup::CommandCenter,
        label: "Command Center: Sessions",
        keywords: &["sessions", "profiles"],
        keybind: None,
    },
    CatalogEntry {
        id: "cc.system",
        group: CatalogGroup::CommandCenter,
        label: "Command Center: System status",
        keywords: &["system", "health"],
        keybind: None,
    },
    CatalogEntry {
        id: "cc.usage",
        group: CatalogGroup::CommandCenter,
        label: "Command Center: Usage",
        keywords: &["usage", "tokens", "cost"],
        keybind: None,
    },
    CatalogEntry {
        id: "appearance.theme",
        group: CatalogGroup::Appearance,
        label: "Appearance: Theme",
        keywords: &["theme", "color"],
        keybind: None,
    },
    CatalogEntry {
        id: "appearance.color_mode",
        group: CatalogGroup::Appearance,
        label: "Appearance: Color mode",
        keywords: &["light", "dark", "system"],
        keybind: None,
    },
    CatalogEntry {
        id: "settings.general",
        group: CatalogGroup::Settings,
        label: "Settings: General",
        keywords: &["general"],
        keybind: None,
    },
    CatalogEntry {
        id: "settings.model",
        group: CatalogGroup::Settings,
        label: "Settings: Model",
        keywords: &["model", "default"],
        keybind: None,
    },
    CatalogEntry {
        id: "settings.providers",
        group: CatalogGroup::Settings,
        label: "Settings: Providers",
        keywords: &["providers", "oauth", "keys"],
        keybind: None,
    },
    CatalogEntry {
        id: "settings.messaging",
        group: CatalogGroup::Settings,
        label: "Settings: Messaging",
        keywords: &["messaging", "platforms", "telegram", "discord"],
        keybind: None,
    },
    CatalogEntry {
        id: "settings.environment",
        group: CatalogGroup::Settings,
        label: "Settings: Environment",
        keywords: &["environment", "env", "variables"],
        keybind: None,
    },
];

/// The Cowork palette filter: split `query` into whitespace-separated words and
/// require EVERY word to appear as a literal, case-insensitive substring of
/// `haystack`. An empty or whitespace-only query matches everything. Word ORDER
/// does not matter (unlike a subsequence match).
pub fn every_word_matches(haystack: &str, query: &str) -> bool {
    let hay = haystack.to_lowercase();
    query
        .split_whitespace()
        .all(|w| hay.contains(&w.to_lowercase()))
}

/// The searchable text for a catalog entry: label followed by keywords, joined.
fn entry_haystack(e: &CatalogEntry) -> String {
    let mut s = e.label.to_lowercase();
    for k in e.keywords {
        s.push(' ');
        s.push_str(&k.to_lowercase());
    }
    s
}

/// True when every query word appears in the entry's label-plus-keywords text.
pub fn catalog_entry_matches(e: &CatalogEntry, query: &str) -> bool {
    every_word_matches(&entry_haystack(e), query)
}

/// Matching static catalog rows, in catalog (display) order.
pub fn search_catalog(query: &str) -> Vec<&'static CatalogEntry> {
    CATALOG
        .iter()
        .filter(|e| catalog_entry_matches(e, query))
        .collect()
}

/// Identifies which lazy provider a dynamic row came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    RecentSessions,
    SettingsFields,
    McpServers,
    ArchivedChats,
}

/// A dynamic (lazily-fetched) palette candidate. The fetch is the live half
/// (unit 29); this module only filters. `id` is opaque to the filter; `text` is
/// what the substring filter runs against and what the row displays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynEntry {
    pub id: String,
    pub text: String,
}

impl DynEntry {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

/// The pure half of a lazy provider: given fetched rows, return those matching
/// every query word (input order preserved).
pub fn filter_provider(rows: &[DynEntry], query: &str) -> Vec<DynEntry> {
    rows.iter()
        .filter(|r| every_word_matches(&r.text, query))
        .cloned()
        .collect()
}

/// Adapt the already-loaded Sessions-browser list into `RecentSessions`
/// provider rows for [`search`]/[`CommandPaletteState::visible_rows`].
///
/// This is the pure data-assembly half of the RecentSessions fetch: the live
/// half is just handing the window's existing [`crate::sessions::SessionsState`]
/// `sessions` slice to this function — no new I/O, since the Sessions browser
/// already fetched them. The mapping is *forced by the engine contract*, not a
/// display choice: `DynEntry::id` must be the session id because
/// [`crate::command_palette::PaletteAction::RunDynamic`] dispatches on it to
/// open that session, and `text` is what the Sessions browser itself shows —
/// the title, falling back to the preview, then the id so a row is never blank
/// (an untitled session must still be selectable and searchable).
pub fn recent_sessions_to_entries(
    sessions: &[awili_gateway::rest::HermesSession],
) -> Vec<DynEntry> {
    sessions
        .iter()
        .map(|s| {
            let text = s
                .title
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .or(s.preview.as_deref().filter(|p| !p.trim().is_empty()))
                .unwrap_or(&s.id);
            DynEntry::new(s.id.clone(), text.to_string())
        })
        .collect()
}

/// Adapt the already-loaded archived conversations into `ArchivedChats`
/// provider rows for [`search`]/[`CommandPaletteState::visible_rows`].
///
/// Pure data-assembly half of the ArchivedChats fetch, exactly parallel to
/// [`recent_sessions_to_entries`]: the live half hands this the output of
/// [`awili_core::ConversationList::archived`] (the sidebar's collapsible
/// "Archived" section, already in memory as `main.rs`'s conversation metadata —
/// no new I/O). The mapping is *forced by the engine contract*, not a display
/// choice: `DynEntry::id` must be the conversation id because
/// [`crate::command_palette::PaletteAction::RunDynamic`] dispatches on it to
/// open that conversation, and `text` is the title the Archived section itself
/// shows, falling back to the id so a row is never blank (blank titles are
/// already rejected by `rename`, but the guard keeps the invariant local).
pub fn archived_chats_to_entries(chats: &[&awili_core::ConversationMeta]) -> Vec<DynEntry> {
    chats
        .iter()
        .map(|c| {
            let text = if c.title.trim().is_empty() {
                c.id.as_str()
            } else {
                c.title.as_str()
            };
            DynEntry::new(c.id.clone(), text.to_string())
        })
        .collect()
}

/// Adapt the decoded settings schema into `SettingsFields` provider rows for
/// [`search`]/[`CommandPaletteState::visible_rows`].
///
/// Pure data-assembly half of the SettingsFields fetch, exactly parallel to
/// [`recent_sessions_to_entries`] and [`archived_chats_to_entries`]: the live
/// half hands this the fields of the
/// [`crate::settings_schema::ConfigSchema`] already decoded from
/// `get_config_schema` for the settings form — no new I/O. The mapping is
/// *forced by the engine contract*, not a display choice: `DynEntry::id` must be
/// the field's dotted config key ([`crate::settings_schema::SchemaField::key`])
/// because [`crate::command_palette::PaletteAction::RunDynamic`] dispatches on it
/// to jump to that field in Settings, and `text` is the human `description` the
/// settings form itself renders as the field's label
/// ([`crate::settings_schema`] documents `description` as "the human description
/// the label shows"), falling back to the key so a row is never blank — the
/// schema decode tolerates a missing description (it decodes to an empty
/// string), and the guard keeps such a field selectable and searchable by its
/// stable key.
pub fn settings_fields_to_entries(fields: &[crate::settings_schema::SchemaField]) -> Vec<DynEntry> {
    fields
        .iter()
        .map(|f| {
            let text = if f.description.trim().is_empty() {
                f.key.as_str()
            } else {
                f.description.as_str()
            };
            DynEntry::new(f.key.clone(), text.to_string())
        })
        .collect()
}

/// One row of the assembled palette result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteHit {
    Static(&'static CatalogEntry),
    Dynamic { kind: ProviderKind, entry: DynEntry },
}

/// Full palette result for `query`: matching static catalog rows first (catalog
/// order), then each provider's matching rows in fixed provider order —
/// RecentSessions, SettingsFields, McpServers, ArchivedChats.
pub fn search(
    query: &str,
    recent_sessions: &[DynEntry],
    settings_fields: &[DynEntry],
    mcp_servers: &[DynEntry],
    archived_chats: &[DynEntry],
) -> Vec<PaletteHit> {
    let mut out: Vec<PaletteHit> = search_catalog(query)
        .into_iter()
        .map(PaletteHit::Static)
        .collect();
    for (kind, rows) in [
        (ProviderKind::RecentSessions, recent_sessions),
        (ProviderKind::SettingsFields, settings_fields),
        (ProviderKind::McpServers, mcp_servers),
        (ProviderKind::ArchivedChats, archived_chats),
    ] {
        for entry in filter_provider(rows, query) {
            out.push(PaletteHit::Dynamic { kind, entry });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_gateway::rest::HermesSession;

    #[test]
    fn every_word_empty_query_matches_all() {
        assert!(every_word_matches("anything here", ""));
        assert!(every_word_matches("x", "   "));
    }

    #[test]
    fn every_word_single_substring() {
        assert!(every_word_matches("Command Center", "comm"));
        assert!(!every_word_matches("Command Center", "zzz"));
    }

    #[test]
    fn every_word_all_words_required() {
        assert!(!every_word_matches("Settings: Messaging", "set msg"));
        assert!(every_word_matches("Settings: Messaging", "set mess"));
    }

    #[test]
    fn every_word_order_irrelevant() {
        assert!(every_word_matches("color mode", "mode color"));
    }

    #[test]
    fn every_word_case_insensitive() {
        assert!(every_word_matches("THEME", "the"));
    }

    #[test]
    fn catalog_has_all_four_groups() {
        let mut seen_go = false;
        let mut seen_cc = false;
        let mut seen_appearance = false;
        let mut seen_settings = false;
        for e in CATALOG {
            match e.group {
                CatalogGroup::GoTo => seen_go = true,
                CatalogGroup::CommandCenter => seen_cc = true,
                CatalogGroup::Appearance => seen_appearance = true,
                CatalogGroup::Settings => seen_settings = true,
            }
        }
        assert!(seen_go && seen_cc && seen_appearance && seen_settings);
    }

    #[test]
    fn catalog_ids_unique() {
        let mut ids = std::collections::HashSet::new();
        for e in CATALOG {
            assert!(ids.insert(e.id), "duplicate id: {}", e.id);
        }
    }

    #[test]
    fn search_catalog_by_keyword() {
        let hits = search_catalog("telegram");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "settings.messaging");
    }

    #[test]
    fn search_catalog_multiword() {
        let hits = search_catalog("settings model");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "settings.model");
    }

    #[test]
    fn search_catalog_empty_returns_all() {
        let hits = search_catalog("");
        assert_eq!(hits.len(), CATALOG.len());
    }

    #[test]
    fn filter_provider_filters_and_preserves_order() {
        let rows = vec![
            DynEntry::new("a", "red apple"),
            DynEntry::new("b", "green apple"),
            DynEntry::new("c", "blue berry"),
        ];
        let got = filter_provider(&rows, "apple");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "a");
        assert_eq!(got[1].id, "b");
        // empty query returns all
        let all = filter_provider(&rows, "");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn search_orders_static_before_dynamic_and_by_provider() {
        let rs = [DynEntry::new("rs1", "recent session")];
        let sf = [DynEntry::new("sf1", "setting field")];
        let mcp = [DynEntry::new("mcp1", "mcp server")];
        let arch = [DynEntry::new("arch1", "archived chat")];

        let hits = search("", &rs, &sf, &mcp, &arch);
        let catalog_len = CATALOG.len();

        // First catalog_len hits are all Static
        for (i, hit) in hits[..catalog_len].iter().enumerate() {
            match hit {
                PaletteHit::Static(_) => {}
                _ => panic!("hit {} should be Static", i),
            }
        }

        // Next four hits are Dynamic, in provider order
        let expected_kinds = [
            ProviderKind::RecentSessions,
            ProviderKind::SettingsFields,
            ProviderKind::McpServers,
            ProviderKind::ArchivedChats,
        ];
        for (i, &expected) in expected_kinds.iter().enumerate() {
            let idx = catalog_len + i;
            match &hits[idx] {
                PaletteHit::Dynamic { kind, entry } => {
                    assert_eq!(*kind, expected, "hit {} kind mismatch", idx);
                    assert_eq!(
                        entry.id,
                        format!(
                            "{}{}",
                            match expected {
                                ProviderKind::RecentSessions => "rs",
                                ProviderKind::SettingsFields => "sf",
                                ProviderKind::McpServers => "mcp",
                                ProviderKind::ArchivedChats => "arch",
                            },
                            "1"
                        )
                    );
                }
                _ => panic!("hit {} should be Dynamic", idx),
            }
        }

        assert_eq!(hits.len(), catalog_len + 4);
    }

    #[test]
    fn keybind_hint_present() {
        let settings = CATALOG.iter().find(|e| e.id == "goto.settings").unwrap();
        assert_eq!(settings.keybind, Some("Cmd+,"));
        let feed = CATALOG.iter().find(|e| e.id == "goto.feed").unwrap();
        assert_eq!(feed.keybind, None);
    }

    fn session_with(id: &str, title: Option<&str>, preview: Option<&str>) -> HermesSession {
        HermesSession {
            id: id.to_string(),
            source: None,
            model: None,
            title: title.map(str::to_string),
            started_at: None,
            ended_at: None,
            end_reason: None,
            message_count: None,
            tool_call_count: None,
            input_tokens: None,
            output_tokens: None,
            preview: preview.map(str::to_string),
            last_active: None,
        }
    }

    #[test]
    fn recent_sessions_map_id_and_title_in_order() {
        let sessions = [
            session_with("s1", Some("Refactor auth"), None),
            session_with("s2", Some("Fix parser"), Some("ignored when titled")),
        ];
        let rows = recent_sessions_to_entries(&sessions);
        // Order preserved (input order == recency order from the browser fetch),
        // id is the opaque dispatch key, text is the browser's title.
        assert_eq!(
            rows,
            vec![
                DynEntry::new("s1", "Refactor auth"),
                DynEntry::new("s2", "Fix parser"),
            ]
        );
    }

    #[test]
    fn recent_sessions_fall_back_preview_then_id_never_blank() {
        let sessions = [
            // No title -> preview.
            session_with("s1", None, Some("hello there")),
            // No title, whitespace-only preview -> id (row must stay selectable).
            session_with("s2", Some("   "), Some("  ")),
            // Nothing at all -> id.
            session_with("s3", None, None),
        ];
        let rows = recent_sessions_to_entries(&sessions);
        assert_eq!(rows[0], DynEntry::new("s1", "hello there"));
        assert_eq!(rows[1], DynEntry::new("s2", "s2"));
        assert_eq!(rows[2], DynEntry::new("s3", "s3"));
        // Every row carries a non-empty label the substring filter can hit.
        assert!(rows.iter().all(|r| !r.text.trim().is_empty()));
    }

    #[test]
    fn recent_sessions_entries_are_filterable_by_search() {
        let sessions = [
            session_with("s1", Some("Refactor auth"), None),
            session_with("s2", Some("Fix parser"), None),
        ];
        let rows = recent_sessions_to_entries(&sessions);
        // The adapter output flows straight into the provider filter/aggregator.
        let hits = search("parser", &rows, &[], &[], &[]);
        let dyn_hits: Vec<_> = hits
            .iter()
            .filter_map(|h| match h {
                PaletteHit::Dynamic {
                    kind: ProviderKind::RecentSessions,
                    entry,
                } => Some(entry),
                _ => None,
            })
            .collect();
        assert_eq!(dyn_hits, vec![&DynEntry::new("s2", "Fix parser")]);
    }

    #[test]
    fn settings_fields_map_key_and_description_in_order() {
        use crate::settings_schema::ConfigSchema;
        use serde_json::json;
        // Drive the real schema-decode seam (parallel to the archived() seam
        // the ArchivedChats tests use).
        let schema = ConfigSchema::from_value(&json!({
            "fields": {
                "model": { "type": "string", "description": "Default model", "category": "general" },
                "agent.service_tier": { "type": "select", "description": "API service tier", "category": "agent", "options": ["", "flex"] }
            }
        }));
        let rows = settings_fields_to_entries(&schema.fields);
        // Schema sorts general before agent; id is the dotted config key (the
        // RunDynamic dispatch key), text is the description the form labels with.
        assert_eq!(
            rows,
            vec![
                DynEntry::new("model", "Default model"),
                DynEntry::new("agent.service_tier", "API service tier"),
            ]
        );
    }

    #[test]
    fn settings_fields_fall_back_to_key_when_description_blank() {
        use crate::settings_schema::{FieldType, SchemaField};
        // A field whose description decoded empty (the schema decode tolerates a
        // missing description) must stay selectable/searchable via its key.
        let fields = [SchemaField {
            key: "agent.max_turns".to_string(),
            field_type: FieldType::Number,
            description: "   ".to_string(),
            category: "agent".to_string(),
            options: Vec::new(),
        }];
        let rows = settings_fields_to_entries(&fields);
        assert_eq!(rows[0], DynEntry::new("agent.max_turns", "agent.max_turns"));
        assert!(rows.iter().all(|r| !r.text.trim().is_empty()));
    }

    #[test]
    fn settings_fields_entries_are_filterable_by_search() {
        use crate::settings_schema::ConfigSchema;
        use serde_json::json;
        let schema = ConfigSchema::from_value(&json!({
            "fields": {
                "model": { "type": "string", "description": "Default model", "category": "general" },
                "agent.service_tier": { "type": "select", "description": "API service tier", "category": "agent", "options": [""] }
            }
        }));
        let rows = settings_fields_to_entries(&schema.fields);
        // Flows into the aggregator as the 2nd (SettingsFields) provider slice.
        let hits = search("tier", &[], &rows, &[], &[]);
        let dyn_hits: Vec<_> = hits
            .iter()
            .filter_map(|h| match h {
                PaletteHit::Dynamic {
                    kind: ProviderKind::SettingsFields,
                    entry,
                } => Some(entry),
                _ => None,
            })
            .collect();
        assert_eq!(
            dyn_hits,
            vec![&DynEntry::new("agent.service_tier", "API service tier")]
        );
    }

    #[test]
    fn archived_chats_map_id_and_title_in_archived_order() {
        use awili_core::ConversationList;
        // Created oldest-first; archived() returns most-recently-updated first.
        let mut list = ConversationList::new();
        let a = list.create(10).id;
        let b = list.create(20).id;
        list.rename(&a, "Old thread");
        list.rename(&b, "New thread");
        assert!(list.archive(&a));
        assert!(list.archive(&b));
        let archived = list.archived();
        let rows = archived_chats_to_entries(&archived);
        // b (updated_ts 20) sorts before a (10); id is the dispatch key, text the title.
        assert_eq!(
            rows,
            vec![
                DynEntry::new(&b, "New thread"),
                DynEntry::new(&a, "Old thread"),
            ]
        );
    }

    #[test]
    fn archived_chats_fall_back_to_id_never_blank() {
        use awili_core::{ConvKind, ConversationMeta};
        // `rename` rejects blank titles, so a blank can only arise from a bad
        // restore; the guard keeps the row selectable by falling back to the id.
        let blank = ConversationMeta {
            id: "c-blank".to_string(),
            title: "   ".to_string(),
            created_ts: 1,
            updated_ts: 1,
            archived: true,
            title_is_custom: false,
            cwd: None,
            profile: None,
            kind: ConvKind::Chat,
        };
        let refs = [&blank];
        let rows = archived_chats_to_entries(&refs);
        assert_eq!(rows[0], DynEntry::new("c-blank", "c-blank"));
        assert!(rows.iter().all(|r| !r.text.trim().is_empty()));
    }

    #[test]
    fn archived_chats_entries_are_filterable_by_search() {
        use awili_core::ConversationList;
        let mut list = ConversationList::new();
        let a = list.create(10).id;
        let b = list.create(20).id;
        list.rename(&a, "Refactor auth");
        list.rename(&b, "Fix parser");
        list.archive(&a);
        list.archive(&b);
        let archived = list.archived();
        let rows = archived_chats_to_entries(&archived);
        // Flows into the aggregator as the 4th (ArchivedChats) provider slice.
        let hits = search("parser", &[], &[], &[], &rows);
        let dyn_hits: Vec<_> = hits
            .iter()
            .filter_map(|h| match h {
                PaletteHit::Dynamic {
                    kind: ProviderKind::ArchivedChats,
                    entry,
                } => Some(entry),
                _ => None,
            })
            .collect();
        assert_eq!(dyn_hits, vec![&DynEntry::new(&b, "Fix parser")]);
    }
}
