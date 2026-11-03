//! SXb proof tests: live apply, reset-to-default and undo through the real
//! settings save gate, on a temp config file.
use super::*;
use crate::settings_catalog::{current, find, is_modified, step};
use serde_json::json;

/// A settings panel opened on a temp config file holding `text`. The
/// `TempDir` must outlive the state.
fn opened(text: &str) -> (tempfile::TempDir, SettingsState) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, text).unwrap();
    let mut s = SettingsState::with_path(path);
    s.open();
    assert!(s.error.is_none(), "fixture must load: {:?}", s.error);
    (dir, s)
}

fn disk(s: &SettingsState) -> String {
    std::fs::read_to_string(s.path()).unwrap()
}

const HOME: &str =
    "# my settings\ndefault_provider = \"hermes\"\n\n[memory]\nbase_url = \"\"\nrecall = false\n";

#[test]
fn sxb_apply_saves_at_once_and_marks_the_row_changed() {
    let (_d, mut s) = opened(HOME);
    let recall = find("memory.recall").unwrap();
    let doc = s.doc.clone().unwrap();
    assert!(
        !is_modified(&doc, recall),
        "set to its default is not a change"
    );
    let edit = step(recall, &current(&doc, recall), true).unwrap();
    assert_eq!(edit, Edit::Bool(true));
    assert!(s.apply_item("memory.recall", edit), "{:?}", s.error);
    let text = disk(&s);
    assert!(text.contains("recall = true"), "{text}");
    assert!(
        text.starts_with("# my settings\n"),
        "comments survive: {text}"
    );
    assert!(is_modified(s.doc.as_ref().unwrap(), recall));
    assert_eq!(
        s.status.as_deref(),
        Some("Saved: Use my memory when answering")
    );
    assert_eq!(s.last_change(), Some("Use my memory when answering"));
    // Applying the same value again is a no-op: no write, no undo step.
    assert!(s.apply_item("memory.recall", Edit::Bool(true)));
    assert_eq!(s.undo.len(), 1);
}

#[test]
fn sxb_restart_items_say_when_they_apply() {
    let (_d, mut s) = opened(HOME);
    assert!(s.apply_item("voice.auto_speak", Edit::Bool(true)));
    assert_eq!(
        s.status.as_deref(),
        Some("Saved: Read replies aloud (applies after you quit Awili and open it again)")
    );
    assert!(disk(&s).contains("auto_speak = true"));
}

#[test]
fn sxb_reset_removes_the_key_and_the_changed_mark() {
    // A comment on the line above a key belongs to that key (toml_edit
    // decor) and leaves with it; the table's own comment stays.
    let text = format!("{HOME}\n# keep me\n[voice]\nauto_speak = true\nvad_silence_windows = 45\n");
    let (_d, mut s) = opened(&text);
    let speak = find("voice.auto_speak").unwrap();
    assert!(is_modified(s.doc.as_ref().unwrap(), speak));
    assert!(s.reset_item("voice.auto_speak"), "{:?}", s.error);
    let after = disk(&s);
    assert!(!after.contains("auto_speak"), "{after}");
    assert!(
        after.contains("# keep me") && after.contains("vad_silence_windows = 45"),
        "{after}"
    );
    let doc = s.doc.clone().unwrap();
    assert!(!is_modified(&doc, speak) && current(&doc, speak) == json!(false));
    assert_eq!(
        s.status.as_deref().unwrap_or(""),
        "Back to default: Read replies aloud (applies after you quit Awili and open it again)"
    );
}

#[test]
fn sxb_undo_puts_back_the_exact_bytes() {
    let (_d, mut s) = opened(HOME);
    assert!(s.apply_item("memory.write", Edit::Bool(false)));
    assert!(s.apply_item("outline.enabled", Edit::Bool(true)));
    assert_eq!(s.undo.len(), 2);
    assert!(s.undo_last(), "{:?}", s.error);
    assert!(disk(&s).contains("write = false") && !disk(&s).contains("[outline]"));
    assert_eq!(s.status.as_deref(), Some("Undone: Outline wiki"));
    assert!(s.undo_last());
    assert_eq!(disk(&s), HOME, "back to the original bytes");
    assert!(!s.undo_last(), "nothing left to undo");
}

#[test]
fn sxb_undo_refuses_after_an_outside_edit() {
    let (_d, mut s) = opened(HOME);
    assert!(s.apply_item("memory.write", Edit::Bool(false)));
    let hand = format!("{}\n# edited by hand\n", disk(&s));
    std::fs::write(s.path(), &hand).unwrap();
    s.open();
    assert!(!s.undo_last());
    assert!(s
        .error
        .as_deref()
        .unwrap()
        .starts_with("Undo is not possible"));
    assert_eq!(disk(&s), hand, "the hand edit is untouched");
    assert!(s.undo.is_empty());
}

#[test]
fn sxb_refused_values_write_nothing() {
    let (_d, mut s) = opened(HOME);
    // Below the memory floor (awili-config refuses it).
    assert!(!s.apply_item("memory.min_priority", Edit::Int(10)));
    assert!(s
        .error
        .as_deref()
        .unwrap()
        .starts_with("Standing rules threshold was not saved"));
    // Not a hold-to-talk key (awili-voice refuses it).
    assert!(!s.apply_item("voice.ptt_key", Edit::Text("banana".into())));
    assert_eq!(disk(&s), HOME);
    assert!(s.undo.is_empty());
    assert!(!s.apply_item("no.such_setting", Edit::Bool(true)));
}

#[test]
fn sxb_live_change_keeps_an_unsaved_provider_card() {
    let (_d, mut s) = opened(HOME);
    let ix = s.add_provider().unwrap();
    s.draft.as_mut().unwrap().providers[ix].name = "fleet".into();
    assert!(s.apply_item("memory.write", Edit::Bool(false)));
    assert!(s.undo_last());
    let d = s.draft.as_ref().unwrap();
    assert_eq!(d.providers.len(), 1, "the unsaved card is still there");
    assert_eq!(d.providers[0].name, "fleet");
    assert!(!disk(&s).contains("fleet"), "and it is still unsaved");
}

#[test]
fn sxb_undo_refuses_a_hand_edit_made_while_settings_is_open() {
    let (_d, mut s) = opened(HOME);
    assert!(s.apply_item("memory.write", Edit::Bool(false)));
    let hand = format!("{}# edited by hand\n", disk(&s));
    std::fs::write(s.path(), &hand).unwrap();
    assert!(
        !s.undo_last(),
        "the panel was not reopened, but the disk changed"
    );
    assert_eq!(s.error.as_deref(), Some(UNDO_STALE));
    assert_eq!(disk(&s), hand);
}

/// SXb-FIX1 (Codex P2 on 9f8666c): re-applying the current value is a real
/// no-op on disk — the config file and its `.bak` keep their bytes AND their
/// modification times, and no other file appears next to them.
#[test]
fn sxb_fix_repeat_apply_writes_nothing() {
    let (d, mut s) = opened(HOME);
    assert!(
        s.apply_item("memory.recall", Edit::Bool(true)),
        "{:?}",
        s.error
    );
    let path = s.path().clone();
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    let bak = std::path::PathBuf::from(bak);
    assert!(bak.exists(), "the first (real) change wrote a backup");
    // Pin both files to a fixed old time, so any rewrite shows even on a
    // coarse-mtime filesystem.
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    for p in [&path, &bak] {
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let snapshot = |p: &std::path::Path| {
        (
            std::fs::read(p).unwrap(),
            std::fs::metadata(p).unwrap().modified().unwrap(),
        )
    };
    let listing = || {
        let mut v: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        v.sort();
        v
    };
    let (file_before, bak_before, dir_before) = (snapshot(&path), snapshot(&bak), listing());
    assert!(
        s.apply_item("memory.recall", Edit::Bool(true)),
        "{:?}",
        s.error
    );
    assert_eq!(
        snapshot(&path),
        file_before,
        "the config file was not rewritten"
    );
    assert_eq!(snapshot(&bak), bak_before, "the backup was not rewritten");
    assert_eq!(listing(), dir_before, "no new file appeared");
    assert_eq!(s.undo.len(), 1, "and no undo step was recorded");
}

/// RC2-UX (R2-24, D-R1 m1, D-R1 Messaging tab): Settings and the left rail
/// say things plainly — the Chat helpers by what they are, a restart setting
/// by what to do, "Automations" (not "Cron"), and one plain line for every
/// page of the rail.
#[test]
fn rc2ux_settings_and_the_rail_say_things_plainly() {
    let chat = crate::settings_catalog::find("chat.executor").unwrap();
    let crate::settings_catalog::Control::Choice(options) = chat.control else {
        panic!("a choice")
    };
    let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
    assert_eq!(
        labels,
        [
            "Claude Haiku: quick answers, low cost",
            "Claude Sonnet: more careful, slower",
            "DeepSeek Flash: quick, very low cost (online)",
            "Our own model at home (nothing leaves the house)",
        ]
    );
    assert_eq!(
        crate::settings_pages::RESTART_NOTE,
        "Applies after you quit Awili and open it again"
    );
    use crate::work::Mode;
    assert_eq!(
        crate::consolidation::BACKSTAGE[crate::consolidation::backstage_index(Mode::Cron).unwrap()]
            .1,
        "Automations"
    );
    for mode in [
        Mode::Work,
        Mode::Code,
        Mode::Skills,
        Mode::Cron,
        Mode::Sessions,
        Mode::CommandCenter,
        Mode::Messaging,
    ] {
        let hint = mode.rail_hint().to_lowercase();
        assert!(!hint.is_empty());
        for jargon in ["cron", "gateway", "executor", "harness", "connector"] {
            assert!(!hint.contains(jargon), "{mode:?}: {hint}");
        }
    }
}

/// Clearing a stored key in Settings removes its secret-store entry too.
#[test]
fn sxb_clearing_a_stored_key_removes_the_stored_entry() {
    let (_d, mut s) = opened(HOME);
    assert!(
        s.apply_item("memory.token", Edit::Text("tok-stale".into())),
        "{:?}",
        s.error
    );
    let store = crate::secret_store::store();
    assert_eq!(store.get("memory").unwrap().as_deref(), Some("tok-stale"));
    assert!(
        s.apply_item("memory.token", Edit::Text(String::new())),
        "{:?}",
        s.error
    );
    assert_eq!(store.get("memory").unwrap(), None);
}
