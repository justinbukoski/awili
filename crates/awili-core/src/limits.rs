//! Context-window sizes (rc.3 S2): how many tokens a model can read at once.
//!
//! Claude and Codex report the window for each turn (Claude's `result` event,
//! Codex's token-usage notifications), and that report is what the app shows.
//! This table is only the FALLBACK for providers that report nothing (a model
//! at home, GLM, …). A model the table does not know has no window: callers
//! show no denominator rather than a guess. Claude and Codex are deliberately
//! not in it, so a table value can never contradict a reported one.

/// One row: a case-insensitive model-name prefix and its window in tokens.
struct Limit {
    prefix: &'static str,
    tokens: u32,
}

/// Published windows, most specific prefix first.
const MODELS: &[Limit] = &[
    Limit {
        prefix: "glm-4.5",
        tokens: 128_000,
    },
    Limit {
        prefix: "glm-4.6",
        tokens: 200_000,
    },
    Limit {
        prefix: "glm-4.7",
        tokens: 200_000,
    },
    Limit {
        prefix: "glm-5",
        tokens: 200_000,
    },
    Limit {
        prefix: "gemma-3",
        tokens: 128_000,
    },
];

/// The fallback window of `model` (case-insensitive prefix match), for a
/// provider that reports none. `None` = unknown.
pub fn context_window(model: &str) -> Option<u32> {
    let m = model.trim().to_ascii_lowercase();
    MODELS
        .iter()
        .find(|l| m.starts_with(l.prefix))
        .map(|l| l.tokens)
}

/// The window to show for a turn: the provider's own report when there is
/// one, else the table's fallback. The flag is true for a reported window.
pub fn window_for_turn(reported: Option<u32>, model: &str) -> Option<(u32, bool)> {
    match reported.filter(|w| *w > 0) {
        Some(w) => Some((w, true)),
        None => context_window(model).map(|w| (w, false)),
    }
}

/// "43.6K of 200K" (reported) or "43.6K of about 200K" (fallback).
pub fn used_of_limit(used: u32, limit: u32, reported: bool) -> String {
    let about = if reported { "" } else { "about " };
    format!("{} of {about}{}", compact(used), compact(limit))
}

/// 43_600 -> "43.6K", 200_000 -> "200K", 1_000_000 -> "1M", 950 -> "950".
pub fn compact(n: u32) -> String {
    let one = |v: f64, s: &str| {
        let r = (v * 10.0).round() / 10.0;
        if (r - r.trunc()).abs() < f64::EPSILON {
            format!("{}{s}", r.trunc() as u64)
        } else {
            format!("{r:.1}{s}")
        }
    };
    if n >= 1_000_000 {
        one(f64::from(n) / 1_000_000.0, "M")
    } else if n >= 1_000 {
        one(f64::from(n) / 1_000.0, "K")
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_models_resolve_by_prefix_case_insensitively() {
        assert_eq!(context_window("GLM-4.5-air"), Some(128_000));
        assert_eq!(context_window("glm-4.6"), Some(200_000));
        assert_eq!(context_window("gemma-3-27b"), Some(128_000));
    }

    #[test]
    fn providers_that_report_their_own_window_are_not_in_the_table() {
        assert_eq!(context_window("claude-haiku-4-5"), None);
        assert_eq!(context_window("gpt-6-luna"), None);
        assert_eq!(context_window("default"), None);
        assert_eq!(context_window(""), None);
    }

    #[test]
    fn reported_window_beats_the_table() {
        // A reported 1M wins over a table row for the same model name.
        assert_eq!(
            window_for_turn(Some(1_000_000), "glm-4.6"),
            Some((1_000_000, true))
        );
        assert_eq!(
            window_for_turn(Some(475_000), "gpt-6-luna"),
            Some((475_000, true))
        );
        // Nothing reported: the table, marked as a fallback.
        assert_eq!(window_for_turn(None, "glm-4.6"), Some((200_000, false)));
        // Nothing reported and unknown: no denominator.
        assert_eq!(window_for_turn(None, "my-local-model"), None);
        assert_eq!(window_for_turn(Some(0), "my-local-model"), None);
    }

    #[test]
    fn compact_and_used_of_limit() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(43_600), "43.6K");
        assert_eq!(compact(200_000), "200K");
        assert_eq!(compact(1_000_000), "1M");
        assert_eq!(used_of_limit(43_600, 200_000, true), "43.6K of 200K");
        assert_eq!(used_of_limit(43_600, 200_000, false), "43.6K of about 200K");
    }
}
