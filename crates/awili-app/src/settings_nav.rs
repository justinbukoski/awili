//! Keyboard navigation state for the settings dialog.
//!
//! Providers each expose a fixed set of focusable actions, with global
//! actions at the ends. This module owns only the selection model; rendering
//! and the settings values themselves live elsewhere.

/// A focusable settings action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    AddProvider,
    LocalDiscovery,
    HarnessRecipe(usize),
    HarnessDocs(usize),
    ProviderName(usize),
    ProviderKind(usize),
    ProviderEnabled(usize),
    ProviderUrl(usize),
    ProviderModels(usize),
    ProviderApiKey(usize),
    RemoveProvider(usize),
    Save,
    Discard,
    ErrorDetails,
}

/// The ordered action list for `count` providers.
pub fn actions(count: usize) -> Vec<Action> {
    let mut out = Vec::with_capacity(3 + 6 * count);
    out.push(Action::AddProvider);
    for i in 0..count {
        out.push(Action::ProviderName(i));
        out.push(Action::ProviderKind(i));
        out.push(Action::ProviderEnabled(i));
        out.push(Action::ProviderUrl(i));
        out.push(Action::ProviderModels(i));
        out.push(Action::RemoveProvider(i));
    }
    out.push(Action::Save);
    out.push(Action::Discard);
    out
}

/// RC2-APP (F0226): one recipe row (and its docs link) per coding helper in
/// [`crate::harness_setup::RECIPES`] — it was fixed at three while there are
/// four, so the fourth helper could not be reached from the keyboard. The
/// General form keeps five spacer rows for them (`3 + count ..= 7 + count`).
const HELPERS: usize = crate::harness_setup::RECIPES.len();
const LOCAL_DISCOVERY_ROWS: usize = 2;
const _: () = assert!(HELPERS <= 4, "the General form keeps four helper rows");

/// Include only key controls actually rendered by the provider form.
pub fn actions_with_keys(key_rows: &[bool]) -> Vec<Action> {
    let mut list = actions(key_rows.len());
    list.splice(
        1..1,
        (0..HELPERS).flat_map(|i| [Action::HarnessRecipe(i), Action::HarnessDocs(i)]),
    );
    list.insert(1 + 2 * HELPERS, Action::LocalDiscovery);
    for i in (0..key_rows.len()).rev() {
        if key_rows[i] {
            let pos = list
                .iter()
                .position(|a| *a == Action::RemoveProvider(i))
                .unwrap();
            list.insert(pos, Action::ProviderApiKey(i));
        }
    }
    list
}

/// Tracks which settings action currently has keyboard focus.
#[derive(Clone, Debug)]
pub struct Navigation {
    selected: Action,
}

impl Navigation {
    /// New navigation with `AddProvider` focused.
    pub fn new() -> Self {
        Self {
            selected: Action::AddProvider,
        }
    }

    /// The currently selected action.
    pub fn selected(&self) -> Action {
        self.selected
    }

    /// Set the selection explicitly (click / action identity). Does NOT
    /// reconcile — the caller passes an action that is valid for the current
    /// provider count; use [`Self::reconcile`] separately for count changes.
    pub fn select(&mut self, action: Action) {
        self.selected = action;
    }

    /// Reset the selection to `AddProvider` when it references a provider
    /// index that no longer exists.
    pub fn reconcile(&mut self, count: usize) {
        if let Some(i) = self.provider_index() {
            if i >= count {
                self.selected = Action::AddProvider;
            }
        }
    }

    /// Move the selection one step through `actions(count)`, wrapping at the
    /// ends. Reconciles first so a stale index is never traversed.
    #[allow(dead_code)] // exercised only by the settings_nav tests
    pub fn move_selection(&mut self, count: usize, backwards: bool) {
        self.reconcile(count);
        self.move_in_list(actions(count), backwards);
    }

    pub fn reconcile_with_keys(&mut self, key_rows: &[bool]) {
        self.reconcile(key_rows.len());
        if let Action::ProviderApiKey(i) = self.selected {
            if !key_rows[i] {
                self.selected = Action::ProviderName(i);
            }
        }
    }

    pub fn move_selection_with_keys(&mut self, key_rows: &[bool], backwards: bool) {
        self.reconcile_with_keys(key_rows);
        self.move_in_list(actions_with_keys(key_rows), backwards);
    }

    fn move_in_list(&mut self, list: Vec<Action>, backwards: bool) {
        let pos = list.iter().position(|a| *a == self.selected).unwrap_or(0);
        let next = if backwards {
            if pos == 0 {
                list.len() - 1
            } else {
                pos - 1
            }
        } else {
            (pos + 1) % list.len()
        };
        self.selected = list[next];
    }

    /// Focus the name field of a newly added provider.
    pub fn after_add(&mut self, new_index: usize) {
        self.selected = Action::ProviderName(new_index);
    }

    /// Keep the selection pointed at the same logical provider after removal.
    /// Provider actions with an index larger than `removed` move down by one;
    /// an action that pointed at the removed provider retargets to the name
    /// field of `min(removed, count_after - 1)` (or `AddProvider` when no
    /// providers remain); global actions are untouched.
    pub fn after_remove(&mut self, removed: usize, count_after: usize) {
        self.selected = match self.selected {
            Action::ProviderName(i) => remap(i, removed, count_after, Action::ProviderName),
            Action::ProviderKind(i) => remap(i, removed, count_after, Action::ProviderKind),
            Action::ProviderEnabled(i) => remap(i, removed, count_after, Action::ProviderEnabled),
            Action::ProviderUrl(i) => remap(i, removed, count_after, Action::ProviderUrl),
            Action::ProviderModels(i) => remap(i, removed, count_after, Action::ProviderModels),
            Action::ProviderApiKey(i) => remap(i, removed, count_after, Action::ProviderApiKey),
            Action::RemoveProvider(i) => remap(i, removed, count_after, Action::RemoveProvider),
            other => other,
        };
    }

    fn provider_index(&self) -> Option<usize> {
        match self.selected {
            Action::ProviderName(i)
            | Action::ProviderKind(i)
            | Action::ProviderEnabled(i)
            | Action::ProviderUrl(i)
            | Action::ProviderModels(i)
            | Action::ProviderApiKey(i)
            | Action::RemoveProvider(i) => Some(i),
            _ => None,
        }
    }
}

fn remap(
    index: usize,
    removed: usize,
    count_after: usize,
    construct: impl Fn(usize) -> Action,
) -> Action {
    if index > removed {
        construct(index - 1)
    } else if index == removed {
        if count_after == 0 {
            Action::AddProvider
        } else {
            Action::ProviderName(removed.min(count_after - 1))
        }
    } else {
        construct(index)
    }
}

/// The direct row index in the General form's scroll container for a
/// selected nav action. The rows are the "Providers" header (0), one card
/// per provider or the single "no providers" row (1..1+count), the Add row
/// at `1 + count.max(1)`, then the default-model row, four reserved helper
/// slots, the Local models heading and card, and the service notice/actions.
/// Save and Discard reveal that final row in the same scroll container.
///
/// This is a pure function so the General form's scroll container can be
/// tracked with a `ScrollHandle` and `scroll_to_item(ix)` can be called
/// with a row index that maps to a direct child of the tracked container.
pub fn selected_settings_row(action: Action, count: usize) -> Option<usize> {
    let ix = match action {
        Action::AddProvider => return Some(1 + count.max(1)),
        Action::LocalDiscovery => return Some(7 + count.max(1)),
        Action::HarnessRecipe(i) | Action::HarnessDocs(i) => {
            return (i < HELPERS).then_some(3 + count.max(1) + i)
        }
        Action::Save | Action::Discard => return Some(7 + count.max(1) + LOCAL_DISCOVERY_ROWS),
        Action::ErrorDetails => return None,
        Action::ProviderName(ix)
        | Action::ProviderKind(ix)
        | Action::ProviderEnabled(ix)
        | Action::ProviderUrl(ix)
        | Action::ProviderModels(ix)
        | Action::ProviderApiKey(ix)
        | Action::RemoveProvider(ix) => ix,
    };
    (ix < count).then_some(1 + ix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_recipes_are_keyboard_accessible_and_scroll_to_their_cards() {
        let mut nav = Navigation::new();
        // RC2-APP (F0226, deliberate change): every helper, not the first three.
        for i in 0..HELPERS {
            nav.move_selection_with_keys(&[], false);
            assert_eq!(nav.selected(), Action::HarnessRecipe(i));
            assert_eq!(selected_settings_row(nav.selected(), 0), Some(4 + i));
            nav.move_selection_with_keys(&[], false);
            assert_eq!(nav.selected(), Action::HarnessDocs(i));
            assert_eq!(selected_settings_row(nav.selected(), 0), Some(4 + i));
        }
        nav.move_selection_with_keys(&[], false);
        assert_eq!(nav.selected(), Action::LocalDiscovery);
        assert_eq!(selected_settings_row(nav.selected(), 0), Some(8));
        nav.move_selection_with_keys(&[], false);
        assert_eq!(nav.selected(), Action::Save);
        nav.move_selection_with_keys(&[], true);
        assert_eq!(nav.selected(), Action::LocalDiscovery);
        nav.move_selection_with_keys(&[], true);
        assert_eq!(nav.selected(), Action::HarnessDocs(HELPERS - 1));
        nav.move_selection_with_keys(&[], true);
        assert_eq!(nav.selected(), Action::HarnessRecipe(HELPERS - 1));
        nav.after_remove(0, 0);
        assert_eq!(nav.selected(), Action::HarnessRecipe(HELPERS - 1));
    }

    /// RC2-APP (F0226): the fourth helper (DeepSeek Harness) has its recipe
    /// and docs rows in the keyboard order, on its own form row.
    #[test]
    fn rc2app_every_helper_is_reachable_from_the_keyboard() {
        assert_eq!(HELPERS, 4);
        let list = actions_with_keys(&[]);
        assert!(list.contains(&Action::HarnessRecipe(3)));
        assert!(list.contains(&Action::HarnessDocs(3)));
        assert_eq!(selected_settings_row(Action::HarnessRecipe(3), 0), Some(7));
        assert_eq!(selected_settings_row(Action::LocalDiscovery, 0), Some(8));
    }

    #[test]
    fn model_form_tab_order_keeps_local_discovery_before_provider_actions() {
        let list = actions_with_keys(&[false]);
        assert_eq!(list[0], Action::AddProvider);
        assert_eq!(list[1 + 2 * HELPERS], Action::LocalDiscovery);
        assert_eq!(list[2 + 2 * HELPERS], Action::ProviderName(0));

        let mut nav = Navigation::new();
        // The app skips helper recipe/docs actions here because those controls
        // live on the Code page; Local Discovery remains the first active stop.
        for _ in 0..=(2 * HELPERS) {
            nav.move_selection_with_keys(&[false], false);
        }
        assert_eq!(nav.selected(), Action::LocalDiscovery);
        nav.move_selection_with_keys(&[false], false);
        assert_eq!(nav.selected(), Action::ProviderName(0));
    }

    #[test]
    fn key_navigation_tracks_visible_rows_and_remaps() {
        let keys = [true, false, true];
        let list = actions_with_keys(&keys);
        assert!(list.contains(&Action::ProviderApiKey(0)));
        assert!(!list.contains(&Action::ProviderApiKey(1)));
        let mut nav = Navigation::new();
        nav.select(Action::ProviderModels(0));
        nav.move_selection_with_keys(&keys, false);
        assert_eq!(nav.selected(), Action::ProviderApiKey(0));
        assert_eq!(selected_settings_row(nav.selected(), 3), Some(1));
        nav.move_selection_with_keys(&keys, false);
        assert_eq!(nav.selected(), Action::RemoveProvider(0));
        nav.move_selection_with_keys(&keys, true);
        assert_eq!(nav.selected(), Action::ProviderApiKey(0));
        nav.select(Action::ProviderModels(1));
        nav.move_selection_with_keys(&keys, false);
        assert_eq!(nav.selected(), Action::RemoveProvider(1));
        nav.select(Action::ProviderApiKey(2));
        nav.after_remove(0, 2);
        assert_eq!(nav.selected(), Action::ProviderApiKey(1));
        assert_eq!(selected_settings_row(nav.selected(), 2), Some(2));
        nav.reconcile_with_keys(&[false, false]);
        assert_eq!(nav.selected(), Action::ProviderName(1));
        nav.select(Action::ProviderApiKey(1));
        nav.after_remove(1, 1);
        assert_eq!(nav.selected(), Action::ProviderName(0));
        nav.select(Action::ProviderApiKey(9));
        nav.reconcile_with_keys(&[]);
        assert_eq!(nav.selected(), Action::AddProvider);
        nav.move_selection_with_keys(&[], true);
        assert_eq!(nav.selected(), Action::Discard);
    }

    #[test]
    fn zero_provider_order_and_wrap() {
        assert_eq!(
            actions(0),
            vec![Action::AddProvider, Action::Save, Action::Discard]
        );

        let mut nav = Navigation::new();
        assert_eq!(nav.selected(), Action::AddProvider);
        nav.move_selection(0, false);
        assert_eq!(nav.selected(), Action::Save);
        nav.move_selection(0, true);
        assert_eq!(nav.selected(), Action::AddProvider);
        nav.move_selection(0, false);
        nav.move_selection(0, false);
        assert_eq!(nav.selected(), Action::Discard);
        nav.move_selection(0, false);
        assert_eq!(nav.selected(), Action::AddProvider);
    }

    #[test]
    fn two_provider_order_and_traversal() {
        let list = actions(2);
        assert_eq!(list.len(), 15);
        assert_eq!(
            &list[..5],
            &[
                Action::AddProvider,
                Action::ProviderName(0),
                Action::ProviderKind(0),
                Action::ProviderEnabled(0),
                Action::ProviderUrl(0),
            ]
        );
        assert_eq!(
            &list[10..],
            &[
                Action::ProviderUrl(1),
                Action::ProviderModels(1),
                Action::RemoveProvider(1),
                Action::Save,
                Action::Discard,
            ]
        );

        let mut nav = Navigation::new();
        for _ in 0..15 {
            nav.move_selection(2, false);
        }
        assert_eq!(nav.selected(), Action::AddProvider);
        nav.move_selection(2, true);
        assert_eq!(nav.selected(), Action::Discard);
    }

    #[test]
    fn add_and_remove_keep_logical_selection() {
        let mut nav = Navigation::new();

        nav.after_add(0);
        assert_eq!(nav.selected(), Action::ProviderName(0));
        nav.after_add(1);
        assert_eq!(nav.selected(), Action::ProviderName(1));

        // Removing the focused provider lands on its successor's name.
        nav.after_remove(1, 1);
        assert_eq!(nav.selected(), Action::ProviderName(0));

        // Larger indices shift down by one.
        nav.selected = Action::ProviderModels(2);
        nav.reconcile(3);
        nav.after_remove(1, 2);
        assert_eq!(nav.selected(), Action::ProviderModels(1));

        // Removing the first provider keeps the logical target.
        nav.selected = Action::RemoveProvider(2);
        nav.after_remove(0, 2);
        assert_eq!(nav.selected(), Action::RemoveProvider(1));

        // Removing the last provider falls back to AddProvider.
        nav.selected = Action::ProviderName(0);
        nav.after_remove(0, 0);
        assert_eq!(nav.selected(), Action::AddProvider);

        // Global actions are untouched.
        nav.selected = Action::Save;
        nav.after_remove(0, 1);
        assert_eq!(nav.selected(), Action::Save);
        nav.selected = Action::Discard;
        nav.after_remove(1, 0);
        assert_eq!(nav.selected(), Action::Discard);

        // Reconcile resets out-of-range indices.
        nav.selected = Action::ProviderName(2);
        nav.reconcile(2);
        assert_eq!(nav.selected(), Action::AddProvider);
    }

    #[test]
    fn selected_row_zero_providers() {
        // Header=0, empty "no providers" row=1, Add=1+0.max(1)=2.
        assert_eq!(selected_settings_row(Action::AddProvider, 0), Some(2));
        assert_eq!(selected_settings_row(Action::Save, 0), Some(10));
        assert_eq!(selected_settings_row(Action::Discard, 0), Some(10));
    }

    #[test]
    fn selected_row_two_providers() {
        // Header=0, cards=1,2, Add=1+2=3.
        assert_eq!(selected_settings_row(Action::AddProvider, 2), Some(3));
        assert_eq!(selected_settings_row(Action::ProviderName(0), 2), Some(1));
        assert_eq!(selected_settings_row(Action::ProviderKind(0), 2), Some(1));
        assert_eq!(
            selected_settings_row(Action::ProviderEnabled(0), 2),
            Some(1)
        );
        assert_eq!(selected_settings_row(Action::ProviderUrl(0), 2), Some(1));
        assert_eq!(selected_settings_row(Action::ProviderModels(0), 2), Some(1));
        assert_eq!(selected_settings_row(Action::RemoveProvider(0), 2), Some(1));
        assert_eq!(selected_settings_row(Action::ProviderName(1), 2), Some(2));
        assert_eq!(selected_settings_row(Action::ProviderKind(1), 2), Some(2));
        assert_eq!(
            selected_settings_row(Action::ProviderEnabled(1), 2),
            Some(2)
        );
        assert_eq!(selected_settings_row(Action::ProviderUrl(1), 2), Some(2));
        assert_eq!(selected_settings_row(Action::ProviderModels(1), 2), Some(2));
        assert_eq!(selected_settings_row(Action::RemoveProvider(1), 2), Some(2));
    }

    #[test]
    fn selected_row_invalid_provider_index() {
        // Index out of range (stale selection) is not a real row.
        assert_eq!(selected_settings_row(Action::ProviderName(2), 2), None);
        assert_eq!(selected_settings_row(Action::RemoveProvider(0), 0), None);
    }

    #[test]
    // Historical test-runner entrypoint; the assertions now include the
    // service-action row after Local models, which scrolls as one child.
    fn selected_row_footer_never_scrolls() {
        // The Local models section has a heading and a card; the service
        // notice/actions follow both as one direct child in the scroll list.
        assert_eq!(selected_settings_row(Action::Save, 0), Some(10));
        assert_eq!(selected_settings_row(Action::Discard, 0), Some(10));
        assert_eq!(selected_settings_row(Action::Save, 3), Some(12));
        assert_eq!(selected_settings_row(Action::Discard, 3), Some(12));
        assert_eq!(selected_settings_row(Action::Save, 1), Some(10));
    }
}
