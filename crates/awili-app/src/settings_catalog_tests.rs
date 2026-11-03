//! SXa proof tests: the catalog, its defaults, search, pages, steps and display.
use super::*;

#[test]
fn sxa_catalog_ids_are_unique_and_match_their_keys() {
    let mut seen = std::collections::HashSet::new();
    for i in ITEMS {
        assert!(seen.insert(i.id), "duplicate id {}", i.id);
        // `table.key`; rc.4 S7 adds one nested table (`work.sandbox.enabled`).
        assert!(i.id.contains('.'), "{}: id is table.key", i.id);
        assert_eq!(format!("{}.{}", i.table(), i.key()), i.id);
        assert!(
            !i.label.is_empty() && !i.help.is_empty(),
            "{} needs words",
            i.id
        );
        // The sandbox sentence is the owner-approved wording (S7), one line longer.
        let limit = if i.id == "work.sandbox.enabled" { 230 } else { 110 };
        assert!(i.help.len() <= limit, "{}: help is one short line", i.id);
        assert!(
            !i.label.contains('_'),
            "{}: no config words in labels",
            i.id
        );
        assert_ne!(i.page, Page::General, "General only lists common items");
        assert_ne!(i.page, Page::Advanced, "Advanced only lists advanced items");
    }
    assert_eq!(Page::ALL.len(), 10);
    assert!(Page::ALL.contains(&Page::Workshop));
}

#[test]
fn sxa_catalog_defaults_match_the_real_defaults() {
    // An empty file is all built-in defaults: each item's declared default
    // must be what Awili really uses when the key is absent.
    let doc = ConfigDoc::parse("").unwrap();
    for i in ITEMS {
        let now = current(&doc, i);
        assert!(
            same(&now, &i.default.json()),
            "{}: default {:?} but real {now}",
            i.id,
            i.default
        );
        assert!(!is_set(&doc, i) && !is_modified(&doc, i), "{}", i.id);
    }
}

#[test]
fn sxa_search_finds_plain_words_and_synonyms() {
    let ids = |q: &str| search(q).iter().map(|i| i.id).collect::<Vec<_>>();
    assert!(ids("").is_empty() && ids("   ").is_empty());
    assert!(ids("zebra").is_empty());
    let aloud = ids("read aloud");
    assert_eq!(
        aloud[0], "voice.auto_speak",
        "the label hit comes first: {aloud:?}"
    );
    assert!(aloud.iter().all(|id| id.starts_with("voice.")), "{aloud:?}");
    assert_eq!(ids("READ ALOUD"), aloud, "case does not matter");
    let mic = ids("mic");
    for id in [
        "voice.ptt_key",
        "voice.vad_threshold",
        "voice.vad_silence_windows",
    ] {
        assert!(mic.contains(&id), "mic finds {id}: {mic:?}");
    }
    assert_eq!(ids("save memories"), ["memory.write"]);
    assert_eq!(
        ids("vad_threshold"),
        ["voice.vad_threshold"],
        "the config key works too"
    );
    // Label hits rank first (catalog order inside each rank); "Save new
    // memories" and the threshold only match on their group/page.
    let memory = ids("memory");
    assert_eq!(
        memory,
        [
            "memory.recall",
            "memory.base_url",
            "memory.token",
            "memory.write",
            "memory.min_priority"
        ]
    );
    assert!(
        memory.contains(&"memory.token"),
        "advanced items are searchable"
    );
    for (query, id) in [
        ("work helper", "work.default_executor"),
        ("code helper", "work.default_executor"),
        ("project helper", "work.default_executor"),
    ] {
        let hits = search(query);
        let hit = hits
            .iter()
            .find(|hit| hit.id == id)
            .unwrap_or_else(|| panic!("legacy/search term {query} finds {id}"));
        assert_eq!(home_page(hit), Page::Workshop, "{query} opens Workshop");
    }
}

#[test]
fn sxa_pages_list_common_and_hide_advanced() {
    let ids = |p, adv| items_on(p, adv).iter().map(|i| i.id).collect::<Vec<_>>();
    assert_eq!(
        ids(Page::General, false),
        ["memory.recall", "notifications.enabled", "voice.auto_speak"]
    );
    assert_eq!(ids(Page::Memory, false), ["memory.recall", "memory.write"]);
    assert_eq!(
        ids(Page::Workshop, false),
        [
            "work.default_executor",
            "work.default_approval_mode",
            "work.sandbox.enabled"
        ]
    );
    assert_eq!(
        ids(Page::Memory, true).len(),
        5,
        "advanced shown on request"
    );
    let advanced = ids(Page::Advanced, false);
    assert!(advanced.contains(&"memory.token") && advanced.contains(&"gateway.dashboard_url"));
    assert!(ITEMS
        .iter()
        .filter(|i| i.advanced)
        .all(|i| advanced.contains(&i.id)));
    assert!(!advanced.contains(&"memory.recall"));
}

#[test]
fn sxa_steps_clamp_and_values_read_plainly() {
    let pause = find("voice.vad_silence_windows").unwrap();
    assert_eq!(step(pause, &json!(30), true), Some(Edit::Int(35)));
    assert_eq!(step(pause, &json!(60), true), None, "top end");
    assert_eq!(step(pause, &json!(10), false), None, "bottom end");
    assert_eq!(display(pause, &json!(30)), "0.6 s");
    let noise = find("voice.vad_threshold").unwrap();
    assert_eq!(display(noise, &json!(0.01)), "8 of 16");
    let Some(Edit::Float(up)) = step(noise, &json!(0.01), true) else {
        panic!("a level steps")
    };
    assert_eq!(display(noise, &json!(up)), "9 of 16");
    assert_eq!(step(noise, &json!(0.1), true), None);
    let who = find("work.default_executor").unwrap();
    assert_eq!(display(who, &Value::Null), "Not chosen yet");
    assert_eq!(display(who, &json!("agent:codex")), "Codex");
    assert_eq!(
        display(who, &json!("agent:codex:o5")),
        "Custom: agent:codex:o5"
    );
    assert!(choice_selected(
        who,
        Some("agent:claude:haiku"),
        "agent:claude"
    ));
    assert!(choice_selected(who, Some("agent:codex:o5"), "agent:codex"));
    assert!(!choice_selected(
        who,
        Some("agent:claudefoo"),
        "agent:claude"
    ));
    assert!(
        !choice_selected(
            find("chat.executor").unwrap(),
            Some("agent:claude:haiku"),
            "agent:claude"
        ),
        "pinned-family selection is scoped to the job-helper control"
    );
    let key = find("memory.token").unwrap();
    assert_eq!(display(key, &json!("s3cret")), "Set (hidden)");
    assert_eq!(display(find("memory.recall").unwrap(), &json!(true)), "On");
}

/// SXg: every `[notifications]` field is a catalog item, and each declared
/// default is N1's real default, field by field.
#[test]
fn sxa_notification_items_match_n1_defaults() {
    let real = serde_json::to_value(awili_config::NotificationsConfig::default()).unwrap();
    let fields = real.as_object().unwrap();
    assert_eq!(
        fields.len(),
        8,
        "eight notification settings; the retired Chat-reply toggle is still read from old files but no longer written"
    );
    assert!(
        find("notifications.chat_replies").is_none(),
        "no Settings control for a notification that no longer exists"
    );
    for (key, value) in fields {
        let id = format!("notifications.{key}");
        let item = find(&id).unwrap_or_else(|| panic!("{id} is in the catalog"));
        assert!(
            same(&item.default.json(), value),
            "{id}: declared {:?}, N1 {value}",
            item.default
        );
        assert_eq!(item.page, Page::Notifications, "{id}");
    }
    let n = |id: &str| find(id).unwrap();
    assert!(
        n("notifications.enabled").common,
        "Notifications on/off is on General"
    );
    assert!(n("notifications.idle_away_secs").advanced);
    assert!(n("notifications.fallback_osascript").advanced);
    for id in [
        "notifications.approvals",
        "notifications.finished",
        "notifications.failed",
        "notifications.sound",
        "notifications.min_run_secs",
    ] {
        assert_eq!(n(id).depends_on, Some("notifications.enabled"), "{id}");
    }
    assert_eq!(display(n("notifications.min_run_secs"), &json!(8)), "8 s");
}

/// SXh: an absent `default_approval_mode` means ask (U4 D2), and reads so;
/// "approval" finds the setting first.
#[test]
fn sxa_approval_mode_unset_reads_as_ask() {
    let item = find("work.default_approval_mode").unwrap();
    assert_eq!(display(item, &Value::Null), "Ask me first (safer)");
    assert_eq!(display(item, &json!("auto")), "Go ahead without asking");
    assert_eq!(item.unset_means, Some("ask"));
    assert_eq!(item.confirm_to, Some("auto"));
    assert_eq!(
        awili_work::ApprovalMode::from_config(None),
        awili_work::ApprovalMode::AskBeforeChanges,
        "absent really means ask"
    );
    assert_eq!(search("approval")[0].id, "work.default_approval_mode");
    assert_eq!(search("ask before")[0].id, "work.default_approval_mode");
    let who = find("work.default_executor").unwrap();
    assert_eq!(
        display(who, &Value::Null),
        "Not chosen yet",
        "other unset choices are unchanged"
    );
}

/// RC1-F2 (RC-02, owner decision 2026-10-01): push-to-talk defaults to Option
/// everywhere a person sees it: the real default, Settings' declared default
/// and its "chosen" button on an empty file, and the shortcuts sheet. Fn stays
/// a choice.
#[test]
fn rc1f2_ptt_default_is_option_everywhere() {
    use awili_voice::{PttKey, VoiceConfig};
    assert_eq!(VoiceConfig::default().ptt_key, PttKey::Option);
    let item = find("voice.ptt_key").unwrap();
    assert!(same(&item.default.json(), &serde_json::json!("option")));
    let doc = ConfigDoc::parse("").unwrap();
    assert_eq!(current(&doc, item), serde_json::json!("option"));
    let Control::Choice(keys) = item.control else {
        panic!("a choice")
    };
    assert!(
        keys.iter().any(|(stored, _)| *stored == "fn"),
        "Fn stays a choice"
    );
    assert!(crate::shortcuts::SHORTCUTS
        .iter()
        .any(|(combo, _)| combo.contains("Option") && !combo.contains("Fn")));
}

/// rc.4 S7: "Sandbox agent commands" is a Workshop > Permissions switch, OFF
/// by default; it lives in the nested `[work.sandbox]` table, and a switch
/// writes and removes exactly that key.
#[test]
fn s7_sandbox_switch_is_off_by_default_and_persists_in_its_own_table() {
    let item = find("work.sandbox.enabled").unwrap();
    assert_eq!((item.table(), item.key()), ("work.sandbox", "enabled"));
    assert_eq!(item.page, Page::Workshop);
    assert_eq!(item.label, "Sandbox agent commands");
    assert_eq!(
        item.help,
        "Commands the agent runs can change files only in the working folder, temp and the agent's own data. Network stays open. This guards against mistakes; it is not a security boundary against a hostile agent."
    );
    assert_eq!(item.control, Control::Toggle);
    assert_eq!(search("sandbox")[0].id, "work.sandbox.enabled");

    let mut doc = ConfigDoc::parse("[work]\ndefault_approval_mode = \"ask\"\n").unwrap();
    assert_eq!(current(&doc, item), json!(false), "default OFF");
    assert!(!is_set(&doc, item) && !is_modified(&doc, item));

    doc.set_bool(&item.target(), true).unwrap();
    assert!(doc.text().contains("[work.sandbox]\nenabled = true"), "{}", doc.text());
    assert!(doc.text().contains("default_approval_mode = \"ask\""), "other keys stay");
    assert_eq!(current(&doc, item), json!(true));
    assert!(doc.config().work.sandbox.enabled);
    assert!(is_set(&doc, item) && is_modified(&doc, item));
    // The saved text reads back the same (it persists).
    let again = ConfigDoc::parse(&doc.text()).unwrap();
    assert_eq!(current(&again, item), json!(true));

    // The next click flips it back; Reset removes the key.
    assert_eq!(step(item, &json!(true), false), Some(Edit::Bool(false)));
    doc.remove(&item.target()).unwrap();
    assert_eq!(current(&doc, item), json!(false));
    assert!(!is_set(&doc, item));
    assert!(!doc.config().work.sandbox.enabled);
}

/// rc.4 S7: on a config with no `[work]` at all the switch still writes a
/// real `[work.sandbox]` table (not an inline one), and it loads.
#[test]
fn s7_sandbox_switch_on_an_empty_config_writes_a_real_table() {
    let item = find("work.sandbox.enabled").unwrap();
    let mut doc = ConfigDoc::parse("").unwrap();
    doc.set_bool(&item.target(), true).unwrap();
    assert!(doc.text().contains("[work.sandbox]\nenabled = true"), "{}", doc.text());
    assert!(!doc.text().contains("{ enabled"), "{}", doc.text());
    assert!(ConfigDoc::parse(&doc.text()).unwrap().config().work.sandbox.enabled);
}
