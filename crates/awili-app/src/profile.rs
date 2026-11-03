//! Profile system — multi-context support for Awili.
//!
//! Each profile represents an independent agent context: its own gateway
//! connection, session history, and optional remote backend configuration.
//! The sidebar scopes to the active profile by default, with a "show all"
//! toggle for cross-profile views.

/// A named agent profile.
#[derive(Clone, Debug)]
pub struct Profile {
    pub name: String,
    pub color: ProfileColor,
    // Created-at stamps the profile list once it ships (Phase 4).
    #[allow(dead_code)]
    pub created_ts: i64,
}

/// Profile accent color for the sidebar rail.
///
/// The palette is fixed at build; the colors the sidebar paints land with
/// the profile UI (Phase 4), so the variants stay lint-quiet until then.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileColor {
    Blue,
    Green,
    Orange,
    Purple,
    Red,
    Teal,
    Yellow,
    Pink,
}

// The accessors ship with the profile UI (Phase 4).
#[allow(dead_code)]
impl ProfileColor {
    pub fn all() -> &'static [ProfileColor] {
        &[
            ProfileColor::Blue,
            ProfileColor::Green,
            ProfileColor::Orange,
            ProfileColor::Purple,
            ProfileColor::Red,
            ProfileColor::Teal,
            ProfileColor::Yellow,
            ProfileColor::Pink,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ProfileColor::Blue => "blue",
            ProfileColor::Green => "green",
            ProfileColor::Orange => "orange",
            ProfileColor::Purple => "purple",
            ProfileColor::Red => "red",
            ProfileColor::Teal => "teal",
            ProfileColor::Yellow => "yellow",
            ProfileColor::Pink => "pink",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "green" => ProfileColor::Green,
            "orange" => ProfileColor::Orange,
            "purple" => ProfileColor::Purple,
            "red" => ProfileColor::Red,
            "teal" => ProfileColor::Teal,
            "yellow" => ProfileColor::Yellow,
            "pink" => ProfileColor::Pink,
            _ => ProfileColor::Blue,
        }
    }
}

/// Manages named profiles. The "default" profile always exists.
#[derive(Clone, Debug)]
pub struct ProfileStore {
    profiles: Vec<Profile>,
    active: String,
    show_all: bool,
}

// Most of the store's surface ships with the profile UI (Phase 4); `new` is
// what the app holds today.
#[allow(dead_code)]
impl ProfileStore {
    pub fn new() -> Self {
        Self {
            profiles: vec![Profile {
                name: "default".to_string(),
                color: ProfileColor::Blue,
                created_ts: 0,
            }],
            active: "default".to_string(),
            show_all: false,
        }
    }

    pub fn from_saved(profiles: Vec<Profile>, active: &str) -> Self {
        let mut store = Self {
            profiles,
            active: active.to_string(),
            show_all: false,
        };
        if store.profiles.is_empty() {
            store.profiles.push(Profile {
                name: "default".to_string(),
                color: ProfileColor::Blue,
                created_ts: 0,
            });
        }
        if store.get(&store.active).is_none() {
            store.active = store.profiles[0].name.clone();
        }
        store
    }

    pub fn active(&self) -> &str {
        &self.active
    }

    pub fn show_all(&self) -> bool {
        self.show_all
    }

    pub fn toggle_show_all(&mut self) {
        self.show_all = !self.show_all;
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Switch the active profile. Returns false if the profile doesn't exist.
    pub fn set_active(&mut self, name: &str) -> bool {
        if self.get(name).is_some() {
            self.active = name.to_string();
            true
        } else {
            false
        }
    }

    /// Create a new profile. Returns false if the name is taken or blank.
    pub fn create(&mut self, name: &str, color: ProfileColor, now_ts: i64) -> bool {
        let normalized = normalize_profile_name(name);
        if normalized.is_empty() || self.get(&normalized).is_some() {
            return false;
        }
        self.profiles.push(Profile {
            name: normalized,
            color,
            created_ts: now_ts,
        });
        true
    }

    /// Rename a profile. Cannot rename to blank or an existing name.
    /// Cannot rename "default".
    pub fn rename(&mut self, old: &str, new: &str) -> bool {
        if old == "default" {
            return false;
        }
        let normalized = normalize_profile_name(new);
        if normalized.is_empty() || self.get(&normalized).is_some() {
            return false;
        }
        if let Some(p) = self.profiles.iter_mut().find(|p| p.name == old) {
            let was_active = self.active == old;
            p.name = normalized.clone();
            if was_active {
                self.active = normalized;
            }
            true
        } else {
            false
        }
    }

    /// Delete a profile. Cannot delete "default" or the last profile.
    /// If the deleted profile was active, falls back to "default".
    pub fn delete(&mut self, name: &str) -> bool {
        if name == "default" || self.profiles.len() <= 1 {
            return false;
        }
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        if self.profiles.len() < before {
            if self.active == name {
                self.active = "default".to_string();
            }
            true
        } else {
            false
        }
    }

    /// Recolor a profile.
    pub fn set_color(&mut self, name: &str, color: ProfileColor) -> bool {
        if let Some(p) = self.profiles.iter_mut().find(|p| p.name == name) {
            p.color = color;
            true
        } else {
            false
        }
    }

    /// Move a profile to a new position in the rail.
    pub fn reorder(&mut self, name: &str, new_index: usize) -> bool {
        let Some(old_idx) = self.profiles.iter().position(|p| p.name == name) else {
            return false;
        };
        if new_index >= self.profiles.len() {
            return false;
        }
        let profile = self.profiles.remove(old_idx);
        self.profiles.insert(new_index, profile);
        true
    }

    /// Cycle to the next profile (Cmd+] style).
    pub fn cycle_next(&mut self) {
        let idx = self
            .profiles
            .iter()
            .position(|p| p.name == self.active)
            .unwrap_or(0);
        let next = (idx + 1) % self.profiles.len();
        self.active = self.profiles[next].name.clone();
    }

    /// Cycle to the previous profile (Cmd+[ style).
    pub fn cycle_prev(&mut self) {
        let idx = self
            .profiles
            .iter()
            .position(|p| p.name == self.active)
            .unwrap_or(0);
        let prev = if idx == 0 {
            self.profiles.len() - 1
        } else {
            idx - 1
        };
        self.active = self.profiles[prev].name.clone();
    }

    /// Switch to the profile at the given rail slot (1-indexed, Cmd+N style).
    pub fn switch_to_slot(&mut self, slot: usize) -> bool {
        if slot == 0 || slot > self.profiles.len() {
            return false;
        }
        self.active = self.profiles[slot - 1].name.clone();
        true
    }
}

#[allow(dead_code)] // ships with the profile UI (Phase 4)
fn normalize_profile_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        trimmed.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_always_exists() {
        let store = ProfileStore::new();
        assert_eq!(store.profiles().len(), 1);
        assert_eq!(store.active(), "default");
        assert!(store.get("default").is_some());
    }

    #[test]
    fn create_and_switch() {
        let mut store = ProfileStore::new();
        assert!(store.create("Work", ProfileColor::Green, 100));
        assert_eq!(store.profiles().len(), 2);
        assert!(store.set_active("work"));
        assert_eq!(store.active(), "work");
    }

    #[test]
    fn create_rejects_duplicate() {
        let mut store = ProfileStore::new();
        assert!(store.create("dev", ProfileColor::Orange, 100));
        assert!(!store.create("dev", ProfileColor::Red, 200));
        assert!(!store.create("  DEV  ", ProfileColor::Red, 200));
    }

    #[test]
    fn create_rejects_blank() {
        let mut store = ProfileStore::new();
        assert!(!store.create("", ProfileColor::Blue, 100));
        assert!(!store.create("   ", ProfileColor::Blue, 100));
    }

    #[test]
    fn rename_profile() {
        let mut store = ProfileStore::new();
        store.create("old-name", ProfileColor::Green, 100);
        store.set_active("old-name");
        assert!(store.rename("old-name", "new-name"));
        assert_eq!(store.active(), "new-name");
        assert!(store.get("new-name").is_some());
        assert!(store.get("old-name").is_none());
    }

    #[test]
    fn cannot_rename_default() {
        let mut store = ProfileStore::new();
        assert!(!store.rename("default", "main"));
    }

    #[test]
    fn delete_profile() {
        let mut store = ProfileStore::new();
        store.create("temp", ProfileColor::Red, 100);
        store.set_active("temp");
        assert!(store.delete("temp"));
        assert_eq!(store.active(), "default");
        assert!(store.get("temp").is_none());
    }

    #[test]
    fn cannot_delete_default() {
        let mut store = ProfileStore::new();
        assert!(!store.delete("default"));
    }

    #[test]
    fn cannot_delete_last_profile() {
        let mut store = ProfileStore::new();
        assert!(!store.delete("default"));
    }

    #[test]
    fn cycle_profiles() {
        let mut store = ProfileStore::new();
        store.create("a", ProfileColor::Green, 1);
        store.create("b", ProfileColor::Red, 2);
        assert_eq!(store.active(), "default");
        store.cycle_next();
        assert_eq!(store.active(), "a");
        store.cycle_next();
        assert_eq!(store.active(), "b");
        store.cycle_next();
        assert_eq!(store.active(), "default");
        store.cycle_prev();
        assert_eq!(store.active(), "b");
    }

    #[test]
    fn switch_to_slot() {
        let mut store = ProfileStore::new();
        store.create("work", ProfileColor::Green, 1);
        assert!(store.switch_to_slot(2));
        assert_eq!(store.active(), "work");
        assert!(store.switch_to_slot(1));
        assert_eq!(store.active(), "default");
        assert!(!store.switch_to_slot(0));
        assert!(!store.switch_to_slot(99));
    }

    #[test]
    fn reorder() {
        let mut store = ProfileStore::new();
        store.create("a", ProfileColor::Green, 1);
        store.create("b", ProfileColor::Red, 2);
        // default, a, b -> b, default, a
        assert!(store.reorder("b", 0));
        assert_eq!(store.profiles()[0].name, "b");
        assert_eq!(store.profiles()[1].name, "default");
    }

    #[test]
    fn set_color() {
        let mut store = ProfileStore::new();
        assert!(store.set_color("default", ProfileColor::Teal));
        assert_eq!(store.get("default").unwrap().color, ProfileColor::Teal);
    }

    #[test]
    fn show_all_toggle() {
        let mut store = ProfileStore::new();
        assert!(!store.show_all());
        store.toggle_show_all();
        assert!(store.show_all());
        store.toggle_show_all();
        assert!(!store.show_all());
    }

    #[test]
    fn from_saved_handles_empty() {
        let store = ProfileStore::from_saved(vec![], "nonexistent");
        assert_eq!(store.profiles().len(), 1);
        assert_eq!(store.active(), "default");
    }

    #[test]
    fn from_saved_fixes_invalid_active() {
        let profiles = vec![Profile {
            name: "dev".to_string(),
            color: ProfileColor::Green,
            created_ts: 1,
        }];
        let store = ProfileStore::from_saved(profiles, "nonexistent");
        assert_eq!(store.active(), "dev");
    }

    #[test]
    fn from_saved_preserves_a_valid_active() {
        // A valid saved `active` must be kept as-is, NOT snapped to the first
        // profile. The two invalid-active tests above both happen to want
        // profiles[0] as the answer, so they don't distinguish "preserve valid
        // active" from "always reset to profiles[0]" — pin that here with a
        // valid active that is deliberately NOT the first profile.
        let profiles = vec![
            Profile {
                name: "personal".to_string(),
                color: ProfileColor::Blue,
                created_ts: 1,
            },
            Profile {
                name: "work".to_string(),
                color: ProfileColor::Green,
                created_ts: 2,
            },
        ];
        let store = ProfileStore::from_saved(profiles, "work");
        assert_eq!(store.active(), "work");
    }

    #[test]
    fn profile_color_persists_round_trip_for_every_variant() {
        // The profile-rail color is persisted as a lowercase string via as_str
        // and rehydrated via from_str. as_str and from_str are two INDEPENDENT
        // match tables, so a typo in either (e.g. Teal => "teal" vs "teal" =>
        // Yellow) silently corrupts a saved profile's color on reload. Pin the
        // bijection over the full palette so any single-table drift is caught.
        for &c in ProfileColor::all() {
            assert_eq!(
                ProfileColor::from_str(c.as_str()),
                c,
                "color {:?} did not survive the as_str/from_str round trip",
                c
            );
        }
    }

    #[test]
    fn profile_color_from_unknown_string_defaults_to_blue() {
        // A persisted color string we don't recognize (legacy value, corrupted
        // config, future palette entry) must fall back to Blue, never panic.
        assert_eq!(ProfileColor::from_str("chartreuse"), ProfileColor::Blue);
        assert_eq!(ProfileColor::from_str(""), ProfileColor::Blue);
    }
}
