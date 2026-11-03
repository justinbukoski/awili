//! The user's home folder, one definition for every platform.
//!
//! Unix and macOS read `$HOME` exactly as before (tests and wrappers redirect
//! the profile through it). Windows has no `$HOME`: it uses `%USERPROFILE%`
//! (or the shell's profile folder), so `~/` in configured paths expands there.

use std::ffi::OsString;

/// The home folder, if one is known.
pub(crate) fn home_os() -> Option<OsString> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .filter(|v| !v.is_empty())
            .or_else(|| dirs::home_dir().map(|p| p.into_os_string()))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
    }
}

/// The home folder as text, `String::new()` when unknown or not UTF-8 (the
/// behaviour of `var("HOME").unwrap_or_default()` this replaces).
pub(crate) fn home_string() -> String {
    home_os()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_default()
}

/// The home folder as text, `None` when unknown or not UTF-8.
pub(crate) fn home_opt() -> Option<String> {
    home_os().and_then(|h| h.into_string().ok())
}

#[cfg(test)]
mod tests {
    #[cfg(not(windows))]
    #[test]
    fn unix_home_is_the_home_variable() {
        assert_eq!(super::home_os(), std::env::var_os("HOME"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_is_the_user_profile() {
        let want = std::env::var_os("USERPROFILE").expect("USERPROFILE is set");
        assert_eq!(super::home_os(), Some(want));
        assert!(!super::home_string().is_empty());
    }
}

#[cfg(test)]
mod path_pins {
    /// macOS app-data locations are pinned: the Windows port must not move them.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_data_paths_are_pinned() {
        let home = dirs::home_dir().expect("home dir");
        let data = dirs::data_dir().expect("data dir");
        assert_eq!(data, home.join("Library/Application Support"));
        assert_eq!(
            crate::work::engine_data_root(),
            Some(data.join("Awili").join("engine"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_data_is_under_roaming_appdata() {
        let appdata = std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap());
        assert_eq!(dirs::data_dir(), Some(appdata.clone()));
        assert_eq!(
            crate::work::engine_data_root(),
            Some(appdata.join("Awili").join("engine"))
        );
    }
}
