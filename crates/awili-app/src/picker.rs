//! The per-turn model picker's pure state (slice 15 §1): whether the popover
//! is open, how a clicked index resolves against the flattened
//! `model_choices()` list, and the provider grouping the popover renders. No
//! GPUI — selection wrapping and grouping are the invariants the spec asks to
//! unit-test, so they live where a test can drive them without a window (the
//! `palette.rs` pattern).

use awili_config::ModelChoice;

/// Picker popover view-state. The selection itself stays in the app's
/// `choice_idx` — this is only the open/closed flag layered on top.
#[derive(Clone, Debug)]
pub struct PickerState {
    /// The popover is showing.
    pub open: bool,
}

impl PickerState {
    pub fn new() -> Self {
        Self { open: false }
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }
}

/// Resolve a selected index against the choices list: in-range passes through,
/// out-of-bounds wraps (`idx % len`), and an empty list pins to 0 so the
/// caller's `choices.get(...)` simply finds nothing.
pub fn select_choice(idx: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        idx % len
    }
}

/// `model_choices()` grouped by provider for the popover, preserving config
/// order. Each model keeps its index into the FLAT list — the one
/// `choice_idx` and [`select_choice`] speak.
pub fn grouped_choices(choices: &[ModelChoice]) -> Vec<(String, Vec<(usize, ModelChoice)>)> {
    let mut out: Vec<(String, Vec<(usize, ModelChoice)>)> = Vec::new();
    for (i, c) in choices.iter().enumerate() {
        match out.iter_mut().find(|(p, _)| *p == c.provider) {
            Some((_, models)) => models.push((i, c.clone())),
            None => out.push((c.provider.clone(), vec![(i, c.clone())])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_config::ProviderKind;

    fn choice(provider: &str, kind: ProviderKind, model: &str) -> ModelChoice {
        ModelChoice {
            provider: provider.to_string(),
            kind,
            base_url: "http://example.invalid".to_string(),
            model: model.to_string(),
            api_key: None,
        }
    }

    /// Selecting an in-range index changes the selected choice to exactly
    /// that index — the picker's click path.
    #[test]
    fn select_changes_selected_choice() {
        let selected = 0usize;
        let clicked = select_choice(2, 3);
        assert_ne!(
            clicked, selected,
            "selecting another row must move the selection"
        );
        assert_eq!(clicked, 2);
        assert_eq!(select_choice(1, 3), 1);
    }

    /// An out-of-bounds index wraps instead of panicking or dangling past the
    /// list end (spec §4); the empty list pins to 0.
    #[test]
    fn select_wraps_out_of_bounds() {
        assert_eq!(select_choice(3, 3), 0);
        assert_eq!(select_choice(5, 3), 2);
        assert_eq!(select_choice(7, 1), 0);
        assert_eq!(select_choice(4, 0), 0);
    }

    #[test]
    fn grouped_choices_keep_flat_indices() {
        let choices = vec![
            choice("ollama", ProviderKind::Ollama, "llama3.1:8b"),
            choice("ollama", ProviderKind::Ollama, "qwen2.5:14b"),
            choice("anthropic", ProviderKind::Anthropic, "claude-sonnet-4-6"),
        ];
        let groups = grouped_choices(&choices);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "ollama");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[0].1[1].0, 1);
        assert_eq!(groups[1].0, "anthropic");
        assert_eq!(groups[1].1[0].0, 2);
        assert_eq!(groups[1].1[0].1.model, "claude-sonnet-4-6");
    }

    /// A provider that reappears AFTER a different provider merges back into
    /// its first group rather than opening a second — `grouped_choices` scans
    /// every existing group (`find`), not just the last. The flat indices the
    /// popover clicks stay attached across the split (0 and 2 both land in the
    /// first group), which a naive "compare with the last group only" grouping
    /// would break by emitting a duplicate provider group.
    #[test]
    fn grouped_choices_regroup_non_adjacent_provider() {
        let choices = vec![
            choice("ollama", ProviderKind::Ollama, "llama3.1:8b"),
            choice("anthropic", ProviderKind::Anthropic, "claude-sonnet-4-6"),
            choice("ollama", ProviderKind::Ollama, "qwen2.5:14b"),
        ];
        let groups = grouped_choices(&choices);
        assert_eq!(
            groups.len(),
            2,
            "non-adjacent same provider must not open a second group"
        );
        assert_eq!(groups[0].0, "ollama");
        assert_eq!(
            groups[0].1.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 2],
            "both ollama rows keep their flat indices in the first group"
        );
        assert_eq!(groups[1].0, "anthropic");
        assert_eq!(groups[1].1[0].0, 1);
    }

    #[test]
    fn picker_state_opens_and_closes() {
        let mut s = PickerState::new();
        assert!(!s.open);
        s.open();
        assert!(s.open);
        s.close();
        assert!(!s.open);
    }
}
