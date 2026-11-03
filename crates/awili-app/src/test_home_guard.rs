//! R2h — test builds never touch the owner's Awili data (RC1 round 2,
//! Minor 15: Code Projects and the Work sidebar showed "awili-m2i-…" test
//! folders; three M2i app tests had built `AwiliApp` with the real HOME and
//! written them into the real database on every gate run).
//!
//! In a test build `AwiliApp::new` first checks that HOME lies inside the
//! temp dir (every private-HOME subprocess uses a `tempfile` dir there), and
//! panics with a pointer to the fix otherwise, before any file is opened.

use std::path::{Path, PathBuf};

/// The panic text's first words (tests look for them).
pub(crate) const REFUSAL: &str = "R2h: a test built AwiliApp with a HOME outside the temp dir";

/// True when `home` is really inside `tmp`: both are resolved (symlinks
/// followed; on macOS /var is itself a link to /private/var) and the
/// resolved home must lie inside the resolved temp dir. R2h-FIX1: a link
/// placed inside the temp dir that points at a real home is refused, and a
/// path that cannot be resolved is refused too.
pub(crate) fn inside(home: &Path, tmp: &Path) -> bool {
    match (std::fs::canonicalize(home), std::fs::canonicalize(tmp)) {
        (Ok(home), Ok(tmp)) => home.starts_with(tmp),
        _ => false,
    }
}

/// Panic unless HOME is a private test home (see the module doc).
pub(crate) fn assert_private_home() {
    let home: Option<PathBuf> = std::env::var_os("HOME").map(PathBuf::from);
    let tmp = std::env::temp_dir();
    let ok = home.as_deref().is_some_and(|h| inside(h, &tmp));
    assert!(
        ok,
        "{REFUSAL} ({home:?}, temp dir {tmp:?}): it would read and write that home's Awili \
         database. Run the test in a private-HOME subprocess (e.g. launch_landing_tests::isolated)."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext;

    const CHILD: &str = "AWILI_R2H_GUARD_CHILD";
    const NAME: &str = "test_home_guard::tests::r2h_app_refuses_a_home_outside_the_temp_dir";

    /// A child process whose HOME is outside the temp dir (a folder next to
    /// the test binary) tries to build the app: the guard stops it before any
    /// Awili database is created under that HOME.
    #[test]
    fn r2h_app_refuses_a_home_outside_the_temp_dir() {
        if std::env::var_os(CHILD).is_some() {
            let mut cx = gpui::TestAppContext::single();
            let _app = cx.new(crate::AwiliApp::new);
            return;
        }
        let home = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("r2h-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        assert!(!inside(&home, &std::env::temp_dir()), "{home:?}");
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env(CHILD, "1")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .output()
            .unwrap();
        let db = home.join("Library/Application Support/Awili/awili.db");
        let db_made = db.exists();
        let _ = std::fs::remove_dir_all(&home);
        let all = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "the guard stopped the app:\n{all}");
        assert!(all.contains(REFUSAL), "the refusal says why:\n{all}");
        assert!(!db_made, "no database was created under that HOME");
    }

    #[cfg(unix)]
    /// R2h-FIX1 (Codex T2 P2 on c092b6e): a HOME that is a symlink placed
    /// inside the temp dir but pointing at a real home (here a folder next to
    /// the test binary, outside the temp dir) is refused, by `inside` and by
    /// the app itself in a child process, before any database is created at
    /// the link's target.
    #[test]
    fn r2h_fix1_a_temp_dir_symlink_to_a_real_home_is_refused() {
        const LINK_NAME: &str =
            "test_home_guard::tests::r2h_fix1_a_temp_dir_symlink_to_a_real_home_is_refused";
        if std::env::var_os(CHILD).is_some() {
            let mut cx = gpui::TestAppContext::single();
            let _app = cx.new(crate::AwiliApp::new);
            return;
        }
        let real = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("r2h-real-home-{}", std::process::id()));
        std::fs::create_dir_all(&real).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("home");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(
            link.starts_with(std::env::temp_dir()),
            "the raw link path is inside the temp dir: {link:?}"
        );
        let refused_by_inside = !inside(&link, &std::env::temp_dir());
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", LINK_NAME, "--nocapture"])
            .env(CHILD, "1")
            .env("HOME", &link)
            .env("XDG_CONFIG_HOME", link.join("config"))
            .env("XDG_DATA_HOME", link.join("data"))
            .env("XDG_CACHE_HOME", link.join("cache"))
            .output()
            .unwrap();
        let db_made = real
            .join("Library/Application Support/Awili/awili.db")
            .exists();
        let _ = std::fs::remove_dir_all(&real);
        let all = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            refused_by_inside,
            "inside() follows the link: {link:?} -> {real:?}"
        );
        assert!(!out.status.success(), "the guard stopped the app:\n{all}");
        assert!(all.contains(REFUSAL), "the refusal says why:\n{all}");
        assert!(!db_made, "no database was created at the link's target");
    }

    /// The private homes the isolated tests use pass.
    #[test]
    fn r2h_a_tempfile_home_is_private() {
        let home = tempfile::tempdir().unwrap();
        assert!(inside(home.path(), &std::env::temp_dir()));
        assert!(!inside(Path::new("/Users/someone"), &std::env::temp_dir()));
    }
}
