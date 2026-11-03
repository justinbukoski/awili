//! SX: the Settings catalog, the one place every plain-language setting is
//! declared (design: the settings design notes). Zed's
//! `settings_ui` pattern: a flat, data-only list of items, each naming its
//! page, group, label, one-line help, control, config key and default. The
//! sidebar, search, "changed" marks and reset all read this list, so a new
//! setting is one entry here and nothing else.
//!
//! Pure and window-free: the values are read from a [`ConfigDoc`], never
//! written here (live apply, reset and undo are `settings_live`).
use awili_config::{ConfigDoc, Target};
use awili_voice::{PttKey, VoiceConfig};
use serde_json::{json, Value};

/// The Settings sidebar, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    General,
    Appearance,
    Workshop,
    Models,
    Memory,
    Connectors,
    Notifications,
    Voice,
    Advanced,
    /// S6: what Awili may do without asking; listed live, not from the
    /// settings file (see `allowed_actions`).
    Allowed,
}

impl Page {
    pub const ALL: [Page; 10] = [
        Page::General,
        Page::Appearance,
        Page::Workshop,
        Page::Models,
        Page::Memory,
        Page::Connectors,
        Page::Notifications,
        Page::Voice,
        Page::Advanced,
        Page::Allowed,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Appearance => "Appearance",
            Page::Workshop => "Workshop",
            Page::Models => "Models & Fleet",
            Page::Memory => "Memory",
            Page::Connectors => "Connectors",
            Page::Notifications => "Notifications",
            Page::Voice => "Voice",
            Page::Advanced => "Advanced",
            Page::Allowed => "Allowed actions",
        }
    }

    /// The one line under the page title.
    pub fn blurb(self) -> &'static str {
        match self {
            Page::General => "The settings you change most.",
            Page::Appearance => "Light, Dark, or follow this Mac.",
            Page::Workshop => {
                "Defaults for answers and project work, permissions, and helper setup."
            }
            Page::Models => "Model services, the local fleet, and the default model.",
            Page::Memory => "What Awili remembers between conversations.",
            Page::Connectors => "Outside tools Awili can use for you.",
            Page::Notifications => "When Awili lets you know something happened.",
            Page::Voice => "Dictation and reading replies aloud.",
            Page::Advanced => "Technical settings. Change these only when you know you need to.",
            Page::Allowed => {
                "Things Awili may do without asking. Take any of them back whenever you like."
            }
        }
    }
}

/// How a setting is changed. Every control is a click target; only `Text`
/// and `Secret` ever need typing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    /// On / Off.
    Toggle,
    /// One of a fixed list: `(stored value, plain label)`.
    Choice(&'static [(&'static str, &'static str)]),
    /// Whole-number −/+ buttons, shown as `value × scale` + `unit`.
    Stepper {
        min: i64,
        max: i64,
        step: i64,
        scale: f64,
        unit: &'static str,
    },
    /// A level on a log scale, moved by −/+ in `notches` steps.
    Level { min: f64, max: f64, notches: u32 },
    /// Short text, click to edit.
    Text,
    /// A key or password: never shown, click to replace.
    Secret,
}

/// The built-in default: what applies when the key is absent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dflt {
    /// Absent means "not chosen" (the key has no default value).
    Unset,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'static str),
}

impl Dflt {
    pub fn json(self) -> Value {
        match self {
            Dflt::Unset => Value::Null,
            Dflt::Bool(b) => json!(b),
            Dflt::Int(n) => json!(n),
            Dflt::Float(f) => json!(f),
            Dflt::Str(s) => json!(s),
        }
    }
}

/// One setting. `id` is its config address, `table.key`.
#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub id: &'static str,
    pub page: Page,
    /// The heading this item sits under on its page.
    pub group: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub control: Control,
    pub default: Dflt,
    /// Listed on the Advanced page (not its own page unless asked).
    pub advanced: bool,
    /// Also listed on General.
    pub common: bool,
    /// Takes effect the next time Awili starts.
    pub restart: bool,
    /// Extra search words (what a person would type).
    pub keywords: &'static [&'static str],
    /// SXe: while the setting holds this value, switching away from it
    /// takes a second click (a household's `provider:fleet` Chat route).
    pub confirm_leave: Option<&'static str>,
    /// SXf: a second page that also lists this item.
    pub also_on: Option<Page>,
    /// SXg: an On/Off item this one only matters under; while that is Off,
    /// the row is dimmed with a note (it stays clickable).
    pub depends_on: Option<&'static str>,
    /// SXh: what an absent key means (its option shows as chosen).
    pub unset_means: Option<&'static str>,
    /// SXh: choosing this value takes a second click (the riskier choice).
    pub confirm_to: Option<&'static str>,
    /// RC2-R511: what an empty text or key means, shown in place of "Not set".
    pub blank_means: Option<&'static str>,
}

const fn it(
    id: &'static str,
    (page, group): (Page, &'static str),
    label: &'static str,
    help: &'static str,
    control: Control,
    default: Dflt,
) -> Item {
    Item {
        id,
        page,
        group,
        label,
        help,
        control,
        default,
        advanced: false,
        common: false,
        restart: false,
        keywords: &[],
        confirm_leave: None,
        also_on: None,
        depends_on: None,
        unset_means: None,
        confirm_to: None,
        blank_means: None,
    }
}

impl Item {
    const fn adv(mut self) -> Self {
        self.advanced = true;
        self
    }
    const fn common(mut self) -> Self {
        self.common = true;
        self
    }
    const fn restart(mut self) -> Self {
        self.restart = true;
        self
    }
    const fn words(mut self, keywords: &'static [&'static str]) -> Self {
        self.keywords = keywords;
        self
    }
    const fn unset_means(mut self, value: &'static str) -> Self {
        self.unset_means = Some(value);
        self
    }
    const fn confirm_to(mut self, value: &'static str) -> Self {
        self.confirm_to = Some(value);
        self
    }
    const fn blank_means(mut self, value: &'static str) -> Self {
        self.blank_means = Some(value);
        self
    }
    const fn needs(mut self, id: &'static str) -> Self {
        self.depends_on = Some(id);
        self
    }
    const fn confirm_leave(mut self, value: &'static str) -> Self {
        self.confirm_leave = Some(value);
        self
    }

    pub fn table(&self) -> &'static str {
        self.id.rsplit_once('.').map_or(self.id, |(t, _)| t)
    }

    pub fn key(&self) -> &'static str {
        self.id.rsplit_once('.').map_or(self.id, |(_, k)| k)
    }

    pub fn target(&self) -> Target {
        Target::Table {
            table: self.table().into(),
            key: self.key().into(),
        }
    }

    fn haystack(&self) -> String {
        let (page, words) = (self.page.title(), self.keywords.join(" "));
        format!(
            "{} {} {} {page} {words} {}",
            self.label, self.help, self.group, self.id
        )
        .to_lowercase()
    }
}

/// SXh: U4's `[work] default_approval_mode` ("ask" | "auto").
const APPROVAL_MODES: &[(&str, &str)] = &[
    ("ask", "Ask me first (safer)"),
    ("auto", "Go ahead without asking"),
];

const CHAT_EXECUTORS: &[(&str, &str)] = &[
    // RC2-UX (R2-24: Chat option names): what each one is, in plain words.
    (
        "agent:claude:haiku",
        "Claude Haiku: quick answers, low cost",
    ),
    ("agent:claude:sonnet", "Claude Sonnet: more careful, slower"),
    (
        "agent:dsh:deepseek-flash",
        "DeepSeek Flash: quick, very low cost (online)",
    ),
    // S6(c): the household route, closed to the fleet's own model.
    (
        "provider:fleet",
        "Our own model at home (nothing leaves the house)",
    ),
];

const WORK_EXECUTORS: &[(&str, &str)] = &[
    ("agent:claude", "Claude Code"),
    ("agent:codex", "Codex"),
    ("agent:glm", "ZCode (GLM)"),
    ("agent:dsh", "DeepSeek Harness"),
    ("gateway", "Hermes gateway"),
    // R2b: L1's local-first default, choosable (RC1 round 2: "no In-app
    // (local) choice exists in that setting").
    ("in-app", "On this Mac (local model)"),
];

const PTT_KEYS: &[(&str, &str)] = &[
    ("option", "Option (⌥)"),
    ("control", "Control"),
    ("command", "Command (⌘)"),
    ("fn", "Fn (Apple keyboards)"),
    ("off", "Off: use the mic button only"),
];

use Control as C;
use Dflt as D;
use Page as P;

/// Every catalog setting, in page order.
#[rustfmt::skip]
pub static ITEMS: &[Item] = &[
    it("appearance.color_mode", (P::Appearance, "Colors"), "Appearance",
        "Choose Light, Dark, or follow this Mac. Changes apply immediately.",
        C::Choice(&[("dark", "Dark"), ("light", "Light"), ("system", "System")]), D::Str("dark"))
        .words(&["color", "theme", "light", "dark", "system"]),
    // Task-oriented defaults for the unified Workshop. IDs stay stable for
    // existing config files; Chat/Work/Code remain useful search terms.
    // Retired (hidden by `CHAT_PICKER`): kept only so old configs stay known.
    it("chat.executor", (P::Workshop, "Answers"), "Answer helper",
        "Retired: every Workshop thread uses its own helper. Old configs that set this still load.",
        C::Choice(CHAT_EXECUTORS), D::Str("agent:claude:haiku"))
        .words(&["chat", "assistant", "answer", "answers", "question", "questions", "model", "ai", "haiku", "sonnet", "deepseek", "fleet"])
        .confirm_leave("provider:fleet")
        // RC1-F2b (owner decision 2026-10-01): DeepSeek is a cloud model
        // abroad (the no-PII rule), so choosing it takes a second click.
        .confirm_to("agent:dsh:deepseek-flash"),
    it("work.default_executor", (P::Workshop, "New project threads"), "Starting project helper",
        "For new project threads. Existing helpers stay; with no saved preference, your last helper is used.",
        C::Choice(WORK_EXECUTORS), D::Unset)
        .words(&["chat", "work", "code", "project", "thread", "helper", "agent", "executor", "claude", "codex", "zcode", "run"]),
    // Shared default; each existing Work or Code thread keeps its own override.
    // "Go ahead without asking" still takes a second click.
    it("work.default_approval_mode", (P::Workshop, "Permissions"), "Before making changes",
        "Applies unless this thread has its own approval setting. Each thread can change its own approval setting.",
        C::Choice(APPROVAL_MODES), D::Unset)
        .unset_means("ask").confirm_to("auto")
        .words(&["chat", "work", "code", "project", "thread", "approval", "approve", "permission", "auto", "ask", "safe"]),
    // rc.4 S7: the opt-in shell sandbox (default OFF; the standing rule is no
    // sandbox unless the owner turns it on).
    it("work.sandbox.enabled", (P::Workshop, "Permissions"), "Sandbox agent commands",
        "Commands the agent runs can change files only in the working folder, temp and the agent's own data. Network stays open. This guards against mistakes; it is not a security boundary against a hostile agent.",
        C::Toggle, D::Bool(false))
        .words(&["sandbox", "seatbelt", "safe", "write", "files", "folder", "protect", "permission"]),
    it("gateway.dashboard_url", (P::Workshop, "Hermes gateway"), "Hermes gateway address",
        "Web address of the Hermes dashboard. Empty turns the gateway off.", C::Text, D::Str(""))
        .adv(),
    it("memory.recall", (P::Memory, "Remembering"), "Use my memory when answering",
        "Awili looks up saved notes and adds what fits to the conversation.", C::Toggle, D::Bool(false))
        .common().words(&["recall", "remember", "notes"]),
    it("memory.write", (P::Memory, "Remembering"), "Save new memories",
        "Awili may save facts from your conversations for later.", C::Toggle, D::Bool(false))
        .words(&["remember", "notes", "store"]),
    it("memory.base_url", (P::Memory, "Memory service"), "Memory service address",
        "Awili's own memory service. Empty: off, and your agents' memory (~/.config/foreman) is used.",
        C::Text,
        D::Str(""))
        .adv()
        // RC2-R511 (usability RC2 round 4, Minor: "Not set" here while Work
        // said "5 notes recalled").
        .blank_means("Off \u{2014} your agents' memory (~/.config/foreman) is used"),
    it("memory.min_priority", (P::Memory, "Memory service"), "Standing rules threshold",
        "Only rules at or above this priority join every turn (90 is the lowest allowed).",
        C::Stepper { min: 90, max: 100, step: 1, scale: 1.0, unit: "" }, D::Int(90))
        .adv(),
    it("memory.token", (P::Memory, "Memory service"), "Memory service key",
        "Leave empty to use the standard key file.", C::Secret, D::Str(""))
        .adv()
        .blank_means("The standard key file"),
    it("outline.enabled", (P::Connectors, "Built-in connectors"), "Outline wiki",
        "Let Awili search, read and write your Outline documents.", C::Toggle, D::Bool(false))
        .words(&["docs", "wiki", "runbook"]),
    it("gitea.enabled", (P::Connectors, "Built-in connectors"), "Gitea code server",
        "Let Awili read your Gitea repositories and issues.", C::Toggle, D::Bool(false))
        .words(&["git", "repo", "code"]),
    // SXg: N1's `[notifications]` (defaults: NotificationsConfig::default()).
    it("notifications.enabled", (P::Notifications, "Notifications"), "Notifications",
        "Let Awili tell you when something needs you or is done.", C::Toggle, D::Bool(true))
        .common().words(&["notify", "alert", "banner"]),
    it("notifications.approvals", (P::Notifications, "When to tell me"), "A card is waiting for you",
        "An approval, a question, or a password request in Work.", C::Toggle, D::Bool(true))
        .needs("notifications.enabled").words(&["notify", "approval", "question"]),
    it("notifications.finished", (P::Notifications, "When to tell me"), "A run or turn finished",
        "Work runs and Code turns that took a while.", C::Toggle, D::Bool(true))
        .needs("notifications.enabled").words(&["notify", "done", "finished"]),
    it("notifications.failed", (P::Notifications, "When to tell me"), "Something failed or stopped",
        "A run that failed, or stopped for a reason other than you.", C::Toggle, D::Bool(true))
        .needs("notifications.enabled").words(&["notify", "error", "failed"]),
    it("notifications.sound", (P::Notifications, "When to tell me"), "Play a sound",
        "Use the Mac's notification sound.", C::Toggle, D::Bool(true))
        .needs("notifications.enabled").words(&["notify", "sound", "beep"]),
    it("notifications.min_run_secs", (P::Notifications, "When to tell me"), "Only for runs longer than",
        "Short runs finish while you watch, so there is no need to tell you.",
        C::Stepper { min: 0, max: 600, step: 5, scale: 1.0, unit: " s" }, D::Int(8))
        .needs("notifications.enabled").words(&["notify", "seconds", "long"]),
    it("notifications.idle_away_secs", (P::Notifications, "Advanced"), "Count me as away after",
        "Frontmost but untouched this long counts as away. 0 turns it off.",
        C::Stepper { min: 0, max: 900, step: 30, scale: 1.0, unit: " s" }, D::Int(120))
        .adv().words(&["notify", "idle", "away"]),
    it("notifications.fallback_osascript", (P::Notifications, "Advanced"), "Backup notifications",
        "If macOS notifications are unavailable, post through Script Editor instead.",
        C::Toggle, D::Bool(true))
        .adv().words(&["notify", "script editor", "osascript"]),
    it("voice.auto_speak", (P::Voice, "Reading aloud"), "Read replies aloud",
        "Speak every answer out loud. The speaker button in the chat bar still turns it on or off.",
        C::Toggle, D::Bool(false))
        .common().restart().words(&["speak", "tts", "speech", "talk"]),
    it("voice.ptt_key", (P::Voice, "Dictation"), "Hold-to-talk key",
        "Hold this key to dictate. The mic button always works too.", C::Choice(PTT_KEYS),
        D::Str("option")) // RC1-F2: Option is the real default (every keyboard)
        .restart().words(&["push to talk", "ptt", "mic", "microphone", "keyboard"]),
    it("voice.vad_threshold", (P::Voice, "Dictation"), "Noise filter",
        "Raise it if background noise keeps the mic listening. Lower it if quiet speech is missed.",
        C::Level { min: 0.001, max: 0.1, notches: 16 }, D::Float(0.01))
        .restart().words(&["mic", "microphone", "sensitivity", "vad", "noise"]),
    it("voice.vad_silence_windows", (P::Voice, "Dictation"), "Pause before Awili stops listening",
        "How long a pause ends what you are saying. Raise it if you get cut off.",
        C::Stepper { min: 10, max: 60, step: 5, scale: 0.02, unit: "s" }, D::Int(30))
        .restart().words(&["mic", "microphone", "silence", "pause", "cut off"]),
    it("voice.tts_voice", (P::Voice, "Reading aloud"), "Reading voice",
        "Voice name for the fleet speech service. Empty uses its default.", C::Text, D::Unset)
        .adv().restart(),
    it("voice.tts_endpoint", (P::Voice, "Reading aloud"), "Speech service address",
        "Empty uses the Mac's own voice.", C::Text, D::Unset)
        .adv().restart(),
    it("voice.tts_model", (P::Voice, "Reading aloud"), "Speech service model",
        "Empty uses the service's default.", C::Text, D::Unset)
        .adv().restart(),
    it("voice.model", (P::Voice, "Dictation"), "Dictation model",
        "Which Whisper model turns speech into text.", C::Text, D::Str("base.en-q5_1"))
        .adv().restart(),
];

/// The item with this id.
pub fn find(id: &str) -> Option<&'static Item> {
    ITEMS.iter().find(|i| i.id == id)
}

/// Whether a choice is visibly selected. The project-helper picker stores a
/// possibly model-pinned Claude value, while its option is the Claude family.
pub fn choice_selected(item: &Item, current: Option<&str>, stored: &str) -> bool {
    let Some(current) = current else {
        return item.unset_means == Some(stored);
    };
    if item.id == "work.default_executor" {
        current == stored
            || current
                .strip_prefix(stored)
                .is_some_and(|suffix| suffix.starts_with(':'))
    } else {
        current == stored
    }
}

/// The items a page lists. General lists the common items; Advanced lists
/// every advanced item; other pages list their own items, the advanced ones
/// only when `show_advanced` is on.
pub fn items_on(page: Page, show_advanced: bool) -> Vec<&'static Item> {
    ITEMS
        .iter()
        .filter(|i| listed_on(page, i, show_advanced))
        .collect()
}

/// The old answer-only Chat assistant picker is retired (Workshop has one
/// tool-capable path). `false` hides that one row everywhere (pages and
/// search); an old config that sets `[chat].executor` still reads.
pub const CHAT_PICKER: bool = false;

/// Whether Settings offers `item` at all.
pub fn offered(item: &Item) -> bool {
    offered_with(item, CHAT_PICKER)
}

/// [`offered`] with the Chat-picker switch given (tests flip it).
pub fn offered_with(item: &Item, chat_picker: bool) -> bool {
    chat_picker || item.id != "chat.executor"
}

/// Whether `page` lists `item` (the rule [`items_on`] applies).
pub fn listed_on(page: Page, item: &Item, show_advanced: bool) -> bool {
    offered(item)
        && match page {
            Page::General => item.common,
            Page::Advanced => item.advanced,
            Page::Allowed => false,
            p => (item.page == p || item.also_on == Some(p)) && (show_advanced || !item.advanced),
        }
}

/// Where a search hit lives: its own page, or Advanced for advanced items.
pub fn home_page(item: &Item) -> Page {
    if item.advanced {
        Page::Advanced
    } else {
        item.page
    }
}

/// Plain-word search over labels, help, groups, pages, synonyms and ids.
/// Every word must match; items whose label holds the first word come
/// first. Advanced items are included (the UI tags them).
pub fn search(query: &str) -> Vec<&'static Item> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let Some(first) = words.first() else {
        return Vec::new();
    };
    let mut hits: Vec<(bool, usize, &'static Item)> = ITEMS
        .iter()
        .enumerate()
        .filter(|(_, i)| offered(i))
        .filter(|(_, i)| {
            let hay = i.haystack();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .map(|(ix, i)| (!i.label.to_lowercase().contains(first.as_str()), ix, i))
        .collect();
    hits.sort_by_key(|(rank, ix, _)| (*rank, *ix));
    hits.into_iter().map(|(_, _, i)| i).collect()
}

/// The effective settings as JSON (`table` → `key` → value), defaults
/// included: the typed `Config` plus awili-voice's `[voice]` table.
pub fn effective(doc: &ConfigDoc) -> Value {
    let mut all = serde_json::to_value(doc.config()).unwrap_or_else(|_| json!({}));
    let v = VoiceConfig::parse(&doc.text()).unwrap_or_default();
    let ptt = match v.ptt_key {
        PttKey::Option => "option",
        PttKey::Control => "control",
        PttKey::Command => "command",
        PttKey::Fn => "fn",
        PttKey::Off => "off",
    };
    all["voice"] = json!({
        "model": v.model,
        "ptt_key": ptt,
        "auto_speak": v.auto_speak,
        "tts_endpoint": v.tts_endpoint,
        "tts_model": v.tts_model,
        "tts_voice": v.tts_voice,
        "vad_threshold": v.vad_threshold,
        "vad_silence_windows": v.vad_silence_windows,
    });
    all
}

/// The item's effective value (`Null` when unset with no default).
pub fn current(doc: &ConfigDoc, item: &Item) -> Value {
    // rc.4 S7: a dotted table (`work.sandbox`) is a nested path.
    let all = effective(doc);
    let table = item.table().split('.').fold(&all, |v, seg| &v[seg]);
    table[item.key()].clone()
}

/// Whether the config file itself sets this key.
pub fn is_set(doc: &ConfigDoc, item: &Item) -> bool {
    let mut probe = doc.clone();
    probe.remove(&item.target()).is_ok() && probe.text() != doc.text()
}

/// Set in the file AND different from the default: the "changed" mark and
/// the reset button show only then.
pub fn is_modified(doc: &ConfigDoc, item: &Item) -> bool {
    is_set(doc, item) && !same(&current(doc, item), &item.default.json())
}

/// Equal values; numbers compare as numbers (`30` == `30.0`).
pub fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-12,
        _ => a == b,
    }
}

/// A new value for an item.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

/// The value one click moves to: a toggle flips, a stepper or level moves
/// one step (clamped; `None` at the end), anything else is `None`.
pub fn step(item: &Item, now: &Value, up: bool) -> Option<Edit> {
    match item.control {
        Control::Toggle => Some(Edit::Bool(!now.as_bool().unwrap_or(false))),
        Control::Stepper { min, max, step, .. } => {
            let v = now.as_i64()?;
            let next = if up { v + step } else { v - step }.clamp(min, max);
            (next != v).then_some(Edit::Int(next))
        }
        Control::Level { min, max, notches } => {
            let v = now.as_f64()?;
            let factor = (max / min).powf(1.0 / notches as f64);
            let next = if up { v * factor } else { v / factor }.clamp(min, max);
            ((next - v).abs() > 1e-12).then_some(Edit::Float(next))
        }
        _ => None,
    }
}

/// The value as a person reads it: "On", "Claude Code", "0.6 s", "8 of 16".
/// A secret never shows its value.
pub fn display(item: &Item, now: &Value) -> String {
    let text = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(str::to_string);
    match item.control {
        Control::Toggle if now.as_bool() == Some(true) => "On".into(),
        Control::Toggle => "Off".into(),
        Control::Choice(options) => match now.as_str() {
            // SXh: an absent key that has a meaning reads as that choice.
            None => item
                .unset_means
                .and_then(|v| options.iter().find(|(stored, _)| *stored == v))
                .map_or_else(|| "Not chosen yet".into(), |(_, label)| label.to_string()),
            Some(v) => options
                .iter()
                .find(|(stored, _)| *stored == v)
                .map_or_else(|| format!("Custom: {v}"), |(_, label)| label.to_string()),
        },
        Control::Stepper { scale, unit, .. } => match now.as_i64() {
            Some(v) if scale != 1.0 => format!("{:.1} {unit}", v as f64 * scale),
            Some(v) => format!("{v}{unit}"),
            None => "Not set".into(),
        },
        Control::Level { min, max, notches } => match now.as_f64() {
            Some(v) => {
                let pos = (v / min).ln() / (max / min).ln() * notches as f64;
                format!("{} of {notches}", pos.round() as i64)
            }
            None => "Not set".into(),
        },
        Control::Text => {
            text(now.as_str()).unwrap_or_else(|| item.blank_means.unwrap_or("Not set").to_string())
        }
        Control::Secret => text(now.as_str()).map_or_else(
            || item.blank_means.unwrap_or("Not set").to_string(),
            |_| "Set (hidden)".into(),
        ),
    }
}

#[cfg(test)]
#[path = "settings_catalog_tests.rs"]
mod tests;
