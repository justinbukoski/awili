//! Voice configuration, read from the `[voice]` table of the same
//! `~/.config/awili/config.toml` the rest of Awili uses (slice-3 spec §1–3).
//!
//! Parsing lives here — pure, no audio, no network — so the defaults and the TOML
//! shape are unit-testable in isolation. The main [`awili_config::Config`] ignores
//! the `[voice]` table (it does not deny unknown fields), so the two parsers read
//! the one file without fighting; this crate reuses
//! [`awili_config::Config::config_paths`] to find it.

use std::path::PathBuf;

use serde::Deserialize;

/// The optional push-to-talk *modifier* held to dictate. Hold-to-talk is the
/// SECONDARY activation path — the primary mechanic is the click-to-toggle mic
/// button (limited hand mobility; a click is easier than a sustained hold).
///
/// Honestly side-agnostic (review slice-3 SF6): GPUI's aggregate modifier flags
/// cannot tell left from right, so the old `right_option` promise actually
/// triggered on either Option key. Rather than reaching for raw side-specific
/// keycodes/global monitoring for a demoted secondary path, the config now names
/// what is truly detected — the modifier *pair* — and `off` disables hold-to-talk
/// entirely (e.g. for heavy users of Option-key special-character entry). The old
/// sided names are accepted as aliases of their pair so existing configs keep
/// working. Kept as a small closed enum so an unknown value is a parse error
/// surfaced at load, not a silently-dead hotkey.
///
/// The default is **Option** (RC1-F2, owner decision 2026-10-01): every keyboard
/// has it, while Fn exists only on Apple keyboards. The 300 ms hold guard
/// (`ptt_guard`) keeps ⌥-shortcuts, ⌥-arrows and accent keys from dictating:
/// any other key pressed while Option is down cancels. Fn stays a choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PttKey {
    /// Either Option (⌥) key. The default.
    #[default]
    #[serde(alias = "right_option", alias = "left_option")]
    Option,
    /// Either Control key.
    #[serde(alias = "right_control", alias = "left_control")]
    Control,
    /// Either Command (⌘) key.
    #[serde(alias = "right_command", alias = "left_command")]
    Command,
    /// Function (fn) key — the bottom-left key on Apple keyboards only.
    Fn,
    /// Hold-to-talk disabled; the mic button is the only way to dictate.
    #[serde(alias = "none")]
    Off,
}

/// Which GPUI `Modifiers` flag a [`PttKey`] watches. Hold-to-talk watches this
/// flag going down then up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PttModifier {
    Alt,
    Control,
    Platform,
    Function,
}

impl PttKey {
    /// The modifier flag this key corresponds to in GPUI's `Modifiers`, or `None`
    /// when hold-to-talk is disabled.
    pub fn modifier(self) -> Option<PttModifier> {
        match self {
            PttKey::Option => Some(PttModifier::Alt),
            PttKey::Control => Some(PttModifier::Control),
            PttKey::Command => Some(PttModifier::Platform),
            PttKey::Fn => Some(PttModifier::Function),
            PttKey::Off => None,
        }
    }

    /// A short human label for the key, shown next to the mic button / README.
    /// Deliberately says "either" — the side cannot be detected (SF6).
    pub fn label(self) -> &'static str {
        match self {
            PttKey::Option => "either ⌥ Option",
            PttKey::Control => "either Control",
            PttKey::Command => "either ⌘ Command",
            PttKey::Fn => "Fn",
            PttKey::Off => "off",
        }
    }
}

/// The whisper model to use. Either a short name we know how to download
/// (`base.en`, `small.en`, optionally `-q5_1` quantized), or an explicit path to a
/// `.bin` already on disk, which overrides any download (slice-3 spec §1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSpec {
    pub name: String,
    pub path_override: Option<PathBuf>,
}

/// Voice-activity-detection bounds (slice-11 spec §1): the settings panel's
/// slider/stepper clamp to these, and out-of-range config values are rejected
/// at parse so a typo (`vad_threshold = 10`) surfaces instead of silently
/// breaking endpointing.
pub const VAD_THRESHOLD_MIN: f64 = 0.001;
pub const VAD_THRESHOLD_MAX: f64 = 0.1;
pub const VAD_SILENCE_WINDOWS_MIN: u32 = 10;
pub const VAD_SILENCE_WINDOWS_MAX: u32 = 60;

/// The parsed `[voice]` table. (No `Eq`: `vad_threshold` is an `f64`.)
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    /// Whisper model name to auto-download on first use. Quantized `base.en` by
    /// default — small, English-only, good enough for dictation, runs on CPU.
    pub model: String,
    /// Explicit path to a `.bin` model, overriding [`model`](Self::model)'s
    /// download. Absent → download by name to the data dir.
    pub model_path: Option<PathBuf>,
    /// Optional hold-to-talk modifier (default: Option — on every keyboard; a
    /// held Option starts a dictation only after 300 ms with no other key;
    /// `off` disables). Secondary to the click-to-toggle mic button.
    pub ptt_key: PttKey,
    /// Whether read-aloud auto-speaks every assistant reply on launch (the
    /// composer-row toggle flips it at runtime).
    pub auto_speak: bool,
    /// Optional fleet-TTS HTTP endpoint. Absent → the macOS `say` fallback is used
    /// (slice-3 spec §3); present → audio is fetched from this URL instead.
    pub tts_endpoint: Option<String>,
    /// Optional model name sent to the fleet-TTS endpoint. Absent/empty → the
    /// service's default voice.
    pub tts_model: Option<String>,
    /// Optional voice id for OpenAI-shaped `/v1/audio/speech` endpoints
    /// (e.g. kokoro's `af_heart`). Absent/empty → the service default.
    pub tts_voice: Option<String>,
    /// Energy threshold above which a capture window counts as speech
    /// (open-mic endpointing). Range [`VAD_THRESHOLD_MIN`]–[`VAD_THRESHOLD_MAX`];
    /// the settings panel edits it on a log scale.
    pub vad_threshold: f64,
    /// How many consecutive sub-threshold windows close an utterance. Range
    /// [`VAD_SILENCE_WINDOWS_MIN`]–[`VAD_SILENCE_WINDOWS_MAX`].
    pub vad_silence_windows: u32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            model: "base.en-q5_1".to_string(),
            model_path: None,
            ptt_key: PttKey::default(),
            auto_speak: false,
            tts_endpoint: None,
            tts_model: None,
            tts_voice: None,
            vad_threshold: 0.01,
            vad_silence_windows: 30,
        }
    }
}

/// Wrapper so `toml::from_str` can pluck just the `[voice]` table out of the full
/// config document, defaulting every field when the table is absent.
#[derive(Deserialize, Default)]
struct VoiceFile {
    #[serde(default)]
    voice: VoiceConfig,
}

impl VoiceConfig {
    /// Parse the `[voice]` table out of a full `config.toml` document. An absent
    /// table yields all defaults; a present-but-malformed one is an error so a
    /// typo (e.g. an unknown `ptt_key` or an out-of-range `vad_threshold`) is
    /// surfaced, not silently ignored.
    pub fn parse(toml_text: &str) -> Result<VoiceConfig, toml::de::Error> {
        let voice = toml::from_str::<VoiceFile>(toml_text)?.voice;
        if !(VAD_THRESHOLD_MIN..=VAD_THRESHOLD_MAX).contains(&voice.vad_threshold) {
            return Err(serde::de::Error::custom(format!(
                "voice.vad_threshold = {} is outside {VAD_THRESHOLD_MIN}–{VAD_THRESHOLD_MAX}",
                voice.vad_threshold
            )));
        }
        if !(VAD_SILENCE_WINDOWS_MIN..=VAD_SILENCE_WINDOWS_MAX).contains(&voice.vad_silence_windows)
        {
            return Err(serde::de::Error::custom(format!(
                "voice.vad_silence_windows = {} is outside {VAD_SILENCE_WINDOWS_MIN}–{VAD_SILENCE_WINDOWS_MAX}",
                voice.vad_silence_windows
            )));
        }
        Ok(voice)
    }

    /// Load from the first existing Awili config path, falling back to defaults when
    /// no file is present or it cannot be read. A present-but-malformed file is an
    /// error (mirrors [`awili_config::Config::load`]'s strictness).
    pub fn load() -> Result<VoiceConfig, toml::de::Error> {
        for path in awili_config::Config::config_paths() {
            if path.exists() {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    return Self::parse(&text);
                }
            }
        }
        Ok(VoiceConfig::default())
    }

    /// The resolved model spec: an explicit `model_path` wins, else download by
    /// `model` name.
    pub fn model_spec(&self) -> ModelSpec {
        ModelSpec {
            name: self.model.clone(),
            path_override: self.model_path.clone(),
        }
    }

    /// Whether a fleet-TTS HTTP endpoint is configured (non-empty).
    pub fn has_http_tts(&self) -> bool {
        self.tts_endpoint
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    /// A conversation-mode VAD configured from this table (negative thresholds
    /// clamp to 0 — "everything is speech" is at least predictable).
    pub fn vad(&self) -> crate::vad::Vad {
        crate::vad::Vad::new(
            (self.vad_threshold as f32).max(0.0),
            self.vad_silence_windows as usize,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_voice_table_is_all_defaults() {
        let c = VoiceConfig::parse("").unwrap();
        assert_eq!(c, VoiceConfig::default());
        assert_eq!(c.model, "base.en-q5_1");
        assert_eq!(c.ptt_key, PttKey::Option);
        assert!(!c.auto_speak);
        assert!(c.tts_endpoint.is_none());
        assert!(!c.has_http_tts());
    }

    #[test]
    fn parses_full_voice_table() {
        let text = r#"
[voice]
model = "small.en"
model_path = "/models/ggml-small.en.bin"
ptt_key = "control"
auto_speak = true
tts_endpoint = "http://10.77.68.86:8900/tts"
tts_model = "af_bella"
vad_threshold = 0.005
vad_silence_windows = 20
"#;
        let c = VoiceConfig::parse(text).unwrap();
        assert_eq!(c.model, "small.en");
        assert_eq!(
            c.model_path,
            Some(PathBuf::from("/models/ggml-small.en.bin"))
        );
        assert_eq!(c.ptt_key, PttKey::Control);
        assert!(c.auto_speak);
        assert!(c.has_http_tts());
        assert_eq!(c.tts_model.as_deref(), Some("af_bella"));
        assert_eq!(c.vad_threshold, 0.005);
        assert_eq!(c.vad_silence_windows, 20);
        assert_eq!(
            c.model_spec().path_override,
            Some(PathBuf::from("/models/ggml-small.en.bin"))
        );
    }

    #[test]
    fn model_spec_carries_the_configured_model_name_when_no_override() {
        // model_spec() is the SOLE bridge from the [voice] table to the model
        // resolver: model::resolve_path/ensure_model key the download off
        // spec.name (the by-name path, when no model_path is set). Every other
        // model_spec() assertion checks only .path_override, so a mutation that
        // dropped/blanked the name (`name: self.model.clone()` -> String::new())
        // would silently resolve the WRONG whisper model yet survive the suite.
        let c = VoiceConfig::parse("[voice]\nmodel = \"small.en\"").unwrap();
        let spec = c.model_spec();
        assert_eq!(spec.name, "small.en");
        assert_eq!(spec.path_override, None);
    }

    #[test]
    fn vad_fields_default_within_their_documented_ranges() {
        let c = VoiceConfig::default();
        assert!((VAD_THRESHOLD_MIN..=VAD_THRESHOLD_MAX).contains(&c.vad_threshold));
        assert!(
            (VAD_SILENCE_WINDOWS_MIN..=VAD_SILENCE_WINDOWS_MAX).contains(&c.vad_silence_windows)
        );
        assert!(c.tts_model.is_none());
    }

    #[test]
    fn out_of_range_vad_values_are_parse_errors() {
        for bad in [
            "vad_threshold = 0.5",
            "vad_threshold = 0.0",
            "vad_threshold = -1.0",
        ] {
            assert!(
                VoiceConfig::parse(&format!("[voice]\n{bad}")).is_err(),
                "{bad}"
            );
        }
        for bad in ["vad_silence_windows = 5", "vad_silence_windows = 100"] {
            assert!(
                VoiceConfig::parse(&format!("[voice]\n{bad}")).is_err(),
                "{bad}"
            );
        }
        // The boundary values themselves are accepted.
        let edges = "[voice]\nvad_threshold = 0.001\nvad_silence_windows = 60";
        assert!(VoiceConfig::parse(edges).is_ok());
        let edges = "[voice]\nvad_threshold = 0.1\nvad_silence_windows = 10";
        assert!(VoiceConfig::parse(edges).is_ok());
    }

    #[test]
    fn coexists_with_the_main_config_tables() {
        // The same document carries provider/memory tables the main parser reads;
        // the voice parser must ignore them and still pick up [voice].
        let text = r#"
default_provider = "ollama"

[memory]
base_url = "http://h:1"

[[providers]]
name = "ollama"
kind = "ollama"
base_url = "http://localhost:11434"
models = ["x"]

[voice]
auto_speak = true
"#;
        let c = VoiceConfig::parse(text).unwrap();
        assert!(c.auto_speak);
        assert_eq!(c.model, VoiceConfig::default().model);
        // And the main config parses the same text without tripping on [voice].
        let main = awili_config::Config::parse(text).unwrap();
        assert_eq!(main.providers.len(), 1);
    }

    #[test]
    fn unknown_ptt_key_is_an_error() {
        let text = r#"
[voice]
ptt_key = "spacebar"
"#;
        assert!(VoiceConfig::parse(text).is_err());
    }

    #[test]
    fn empty_tts_endpoint_is_not_http() {
        let c = VoiceConfig::parse("[voice]\ntts_endpoint = \"\"").unwrap();
        assert!(!c.has_http_tts());
    }

    #[test]
    fn ptt_labels_are_honest_about_sides() {
        // SF6: the label must not promise a side the detection can't deliver.
        assert_eq!(PttKey::Option.label(), "either ⌥ Option");
        assert_eq!(PttKey::Fn.label(), "Fn");
        assert_eq!(PttKey::Off.label(), "off");
    }

    #[test]
    fn legacy_sided_ptt_names_alias_to_their_pair() {
        // Configs written against the old sided vocabulary keep working — both
        // sides of a pair land on the same (honestly side-agnostic) modifier.
        for (old, now) in [
            ("right_option", PttKey::Option),
            ("left_option", PttKey::Option),
            ("right_control", PttKey::Control),
            ("left_control", PttKey::Control),
            ("right_command", PttKey::Command),
            ("left_command", PttKey::Command),
        ] {
            let c = VoiceConfig::parse(&format!("[voice]\nptt_key = \"{old}\"")).unwrap();
            assert_eq!(c.ptt_key, now, "{old}");
        }
    }

    #[test]
    fn ptt_off_disables_hold_to_talk() {
        for value in ["off", "none"] {
            let c = VoiceConfig::parse(&format!("[voice]\nptt_key = \"{value}\"")).unwrap();
            assert_eq!(c.ptt_key, PttKey::Off);
            assert_eq!(c.ptt_key.modifier(), None);
        }
    }

    #[test]
    fn u3b2_default_ptt_is_fn_and_explicit_option_kept() {
        // RC1-F2 (supersedes U3b2 D2): the default is Option; an explicit
        // Fn and an explicit Option value (new or legacy sided name) are kept.
        assert_eq!(VoiceConfig::parse("").unwrap().ptt_key, PttKey::Option);
        assert_eq!(
            VoiceConfig::parse("[voice]\nptt_key = \"fn\"")
                .unwrap()
                .ptt_key,
            PttKey::Fn
        );
        assert_eq!(
            VoiceConfig::parse("[voice]\nptt_key = \"option\"")
                .unwrap()
                .ptt_key,
            PttKey::Option
        );
        assert_eq!(
            VoiceConfig::parse("[voice]\nptt_key = \"right_option\"")
                .unwrap()
                .ptt_key,
            PttKey::Option
        );
    }

    #[test]
    fn example_config_voice_table_parses() {
        // Guards the documented [voice] shape in config.example.toml against drift:
        // it must parse under both this parser and the main config parser.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config.example.toml");
        let text = std::fs::read_to_string(path).expect("read config.example.toml");
        let v = VoiceConfig::parse(&text).expect("voice table parses");
        assert_eq!(v.model, "base.en-q5_1");
        assert_eq!(v.ptt_key, PttKey::Option);
        assert!(!v.auto_speak);
        awili_config::Config::parse(&text).expect("main config still parses with [voice]");
    }

    #[test]
    fn vad_defaults_and_overrides_parse() {
        // Slice-5: absent fields take the documented defaults…
        let c = VoiceConfig::parse("").unwrap();
        assert_eq!(c.vad_threshold, 0.01);
        assert_eq!(c.vad_silence_windows, 30);
        let v = c.vad();
        assert_eq!(v.threshold, 0.01);
        assert_eq!(v.silence_windows, 30);
        // …and the [voice] table can tune both knobs.
        let c =
            VoiceConfig::parse("[voice]\nvad_threshold = 0.05\nvad_silence_windows = 10").unwrap();
        assert_eq!(c.vad_threshold, 0.05);
        assert_eq!(c.vad_silence_windows, 10);
        // A negative threshold clamps to 0 (a nonsense value must not make the
        // comparison direction silently flip behaviour).
        let bad = VoiceConfig {
            vad_threshold: -1.0,
            ..Default::default()
        };
        assert_eq!(bad.vad().threshold, 0.0);
    }

    #[test]
    fn ptt_keys_map_to_modifiers() {
        assert_eq!(PttKey::Option.modifier(), Some(PttModifier::Alt));
        assert_eq!(PttKey::Control.modifier(), Some(PttModifier::Control));
        assert_eq!(PttKey::Command.modifier(), Some(PttModifier::Platform));
        assert_eq!(PttKey::Fn.modifier(), Some(PttModifier::Function));
        assert_eq!(PttKey::Off.modifier(), None);
    }
}
