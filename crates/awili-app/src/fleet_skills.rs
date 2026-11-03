//! P1 fleet-skill wiring: pack loading and task preparation.
//!
//! Lifecycle (P1 spec): the pack is loaded ONCE, at startup and on every
//! config re-apply (the `apply_runtime_config` funnel in `main.rs`) — never
//! on the submit path, which only holds the `Arc<SkillPack>` and renders
//! from it. Skill-file edits take effect on the next reload or app restart;
//! there is no background watcher. A re-apply replaces or clears the old
//! pack, so a disabled or invalid config can never keep serving stale
//! recipes.
//!
//! An invalid or missing pack logs a clear diagnostic and disables
//! injection — the send still works, unchanged.

use std::sync::Arc;

use awili_config::Config;
use awili_skills::SkillPack;

/// Load the pack configured by `[skills].fleet_pack` (`None` when unset).
///
/// This is the only place pack I/O happens, and only on the startup/reload
/// path. A load failure is a clear, non-fatal diagnostic: injection is
/// disabled for this config and the send proceeds without a preamble.
pub fn load_pack(cfg: &Config) -> Option<Arc<SkillPack>> {
    let dir = cfg.skills.fleet_pack.as_deref()?;
    match SkillPack::load(dir) {
        Ok(pack) => {
            eprintln!(
                "awili: fleet skill pack loaded ({} skills) from {}",
                pack.skills().len(),
                dir.display()
            );
            Some(Arc::new(pack))
        }
        Err(e) => {
            eprintln!("awili: fleet skill pack disabled: {e}");
            None
        }
    }
}

/// RC1-H2: whether a Work turn may get fleet recipes at all. The pack is the
/// owner's fleet know-how, so it goes only to a turn in one of the owner's
/// fleet folders — a git checkout (the folder or a folder above it) whose
/// remote is on the configured Gitea host, with the Gitea family enabled —
/// and never to the in-app engine, which has file tools only and cannot
/// follow a fleet recipe. A household folder never gets one.
pub fn recipes_apply(
    in_app: bool,
    folder: Option<&std::path::Path>,
    gitea: &awili_config::GiteaConfig,
) -> bool {
    !in_app && folder.is_some_and(|f| is_fleet_folder(f, gitea))
}

/// RC1-H2: `folder` (or the nearest folder above it holding `.git`) is a git
/// checkout with a remote on the `[gitea]` host. False when the Gitea family
/// is disabled, the host cannot be read, or no git config is found.
/// RC1-H2-FIX1: the NEAREST checkout decides — a `.git` (folder or gitfile)
/// whose config cannot be read or is too large denies, never a checkout
/// further up — and only `url` keys inside `[remote "…"]` sections count (a
/// submodule's or an `insteadOf` URL does not).
pub fn is_fleet_folder(folder: &std::path::Path, gitea: &awili_config::GiteaConfig) -> bool {
    if !gitea.enabled {
        return false;
    }
    let Some(host) = url_host(&gitea.base_url) else {
        return false;
    };
    let Some(checkout) = folder
        .ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
    else {
        return false;
    };
    let Some(config) = git_config_text(checkout) else {
        return false;
    };
    let on_gitea = remote_urls(&config)
        .any(|url| url_host(url).is_some_and(|h| h.eq_ignore_ascii_case(&host)));
    on_gitea
}

/// RC1-H2-FIX1: the values of the `url` keys inside `[remote "…"]` sections
/// of a git config (section and key names are case-insensitive; `#` and `;`
/// start comments; the old `[remote.name]` form counts too).
fn remote_urls(config: &str) -> impl Iterator<Item = &str> {
    let mut in_remote = false;
    config.lines().filter_map(move |line| {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            return None;
        }
        if let Some(header) = line.strip_prefix('[') {
            let name = header.trim_start().to_ascii_lowercase();
            in_remote = name.starts_with("remote \"") || name.starts_with("remote.");
            return None;
        }
        if !in_remote {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("url")
            .then(|| value.trim().trim_matches('"'))
    })
}

/// The host of `http://h:1/x`, `ssh://user@h:2/x` or `user@h:x` (scp form).
fn url_host(url: &str) -> Option<String> {
    let host = match url.split_once("://") {
        Some((_, rest)) => {
            let authority = rest.split('/').next().unwrap_or("");
            let authority = authority.rsplit('@').next().unwrap_or("");
            authority.split(':').next().unwrap_or("")
        }
        None => {
            let (_, rest) = url.split_once('@')?;
            rest.split(':').next().unwrap_or("")
        }
    };
    (!host.is_empty()).then(|| host.to_string())
}

/// The git config text for the checkout at exactly `dir`: `.git/config`, or
/// for a linked worktree (`.git` is a `gitdir:` file) the shared config named
/// by its `commondir`. `None` when `dir` holds no `.git`.
fn git_config_text(dir: &std::path::Path) -> Option<String> {
    const MAX_CONFIG_BYTES: u64 = 256 * 1024;
    let dot_git = dir.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.trim().strip_prefix("gitdir:")?.trim();
        let target = std::path::Path::new(target);
        let git_dir = if target.is_absolute() {
            target.to_path_buf()
        } else {
            dir.join(target)
        };
        match std::fs::read_to_string(git_dir.join("commondir")) {
            Ok(common) => {
                let common = std::path::Path::new(common.trim());
                if common.is_absolute() {
                    common.to_path_buf()
                } else {
                    git_dir.join(common)
                }
            }
            Err(_) => git_dir,
        }
    } else {
        return None;
    };
    let config = git_dir.join("config");
    if std::fs::metadata(&config).ok()?.len() > MAX_CONFIG_BYTES {
        return None;
    }
    std::fs::read_to_string(config).ok()
}

/// Prepend the rendered fleet-skill block to the executor-facing task.
///
/// The user message was displayed and persisted BEFORE this point (unchanged
/// text, driving the auto-title); only the executor's copy is prepared.
/// Every executor path (Agent harness, owned engine, persistent and new
/// gateway) receives this single prepared `task`, so they all get the same
/// input.
///
/// With no pack or an empty selection the task is returned verbatim — no
/// preamble, no trailing-newline change. (The `Arc` stays owned by the app
/// struct; the render borrows the pack through it.)
pub fn prepare_task(pack: Option<&SkillPack>, task: &str) -> String {
    let Some(pack) = pack else {
        return task.to_string();
    };
    let selected = pack.select(task);
    if selected.is_empty() {
        return task.to_string();
    }
    let mut out = pack.render_preamble(&selected);
    // The rendered block ends with a newline on the all-sections path but
    // not on the omission-only path; normalize so the task text always
    // starts on its own line.
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(task);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALPHA_BODY: &str = "\
Use the alpha store endpoint; confirm values against the system first.";

    const BRAVO_BODY: &str = "\
Use the bravo ledger endpoint; confirm values against the system first.";

    fn skill_md(name: &str, description: &str, when: &str, body: &str) -> String {
        format!(
            "---\nname: {name}\ndescription: {description}\nwhen_to_use: {when}\ntools_needed: ssh\n---\n\n{body}\n"
        )
    }

    /// Create `root/<name>/SKILL.md` for one skill and return the root.
    fn write_pack(temp: &std::path::Path, name: &str, description: &str, when: &str, body: &str) {
        let dir = temp.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            skill_md(name, description, when, body),
        )
        .unwrap();
    }

    fn config_with_pack(dir: &std::path::Path) -> Config {
        let mut cfg = Config::builtin();
        cfg.skills.fleet_pack = Some(dir.to_path_buf());
        cfg
    }

    #[test]
    fn disabled_pack_leaves_the_task_verbatim() {
        assert_eq!(
            prepare_task(None, "outline document id"),
            "outline document id"
        );
    }

    #[test]
    fn pack_with_no_match_leaves_the_task_verbatim() {
        let temp = tempfile::tempdir().unwrap();
        write_pack(
            temp.path(),
            "alpha",
            "Alpha store operations.",
            "When the task concerns the alpha store.",
            ALPHA_BODY,
        );
        let pack = load_pack(&config_with_pack(temp.path())).expect("pack loads");
        // "what is the weather" shares no meaningful token with the alpha
        // store recipe: nothing is injected, the task is byte-identical.
        assert_eq!(
            prepare_task(Some(&pack), "what is the weather"),
            "what is the weather"
        );
    }

    #[test]
    fn prepared_task_prepends_the_block_and_keeps_the_original_text() {
        let temp = tempfile::tempdir().unwrap();
        write_pack(
            temp.path(),
            "alpha",
            "Alpha store operations.",
            "When the task concerns the alpha store.",
            ALPHA_BODY,
        );
        let pack = load_pack(&config_with_pack(temp.path())).expect("pack loads");
        let task = "sync the alpha store to the backup host";
        let prepared = prepare_task(Some(&pack), task);
        // The block comes first and names the recipe...
        assert!(prepared.starts_with("Fleet skills matched this task."));
        assert!(prepared.contains("## alpha"));
        // ...and the ORIGINAL user text ends the prepared task untouched
        // (the displayed/persisted message is this exact text).
        assert!(prepared.ends_with(task), "prepared = {prepared:?}");
        assert!(prepared.len() > task.len());
    }

    #[test]
    fn cached_pack_prepares_tasks_after_source_directory_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        write_pack(
            temp.path(),
            "alpha",
            "Alpha store operations.",
            "Use alpha storage.",
            ALPHA_BODY,
        );
        let pack = load_pack(&config_with_pack(temp.path())).expect("pack loads");
        temp.close().unwrap();
        assert!(prepare_task(Some(&pack), "alpha store").contains("## alpha"));
    }

    /// Replacement/disabled snapshot regression: a reconfigure to a
    /// different directory swaps the pack; a reconfigure that drops
    /// `[skills].fleet_pack` (or points at a missing directory) CLEARS it —
    /// no stale recipe survives the re-apply.
    #[test]
    fn reconfigure_replaces_and_disables_the_old_pack() {
        let a = tempfile::tempdir().unwrap();
        write_pack(
            a.path(),
            "alpha",
            "Alpha store operations.",
            "When the task concerns the alpha store.",
            ALPHA_BODY,
        );
        let b = tempfile::tempdir().unwrap();
        write_pack(
            b.path(),
            "bravo",
            "Bravo ledger operations.",
            "When the task concerns the bravo ledger.",
            BRAVO_BODY,
        );

        // Startup load: the alpha pack serves "alpha store" tasks.
        let pack_a = load_pack(&config_with_pack(a.path())).expect("pack A loads");
        let query = "sync the alpha store to the backup host";
        assert!(prepare_task(Some(&pack_a), query).contains("## alpha"));

        // Reconfigure to pack B: the old pack is REPLACED — "alpha store" no
        // longer matches anything (bravo is a ledger recipe), and "bravo
        // ledger" now does.
        let pack_b = load_pack(&config_with_pack(b.path())).expect("pack B loads");
        assert!(!prepare_task(Some(&pack_b), query).contains("## alpha"));
        assert!(prepare_task(Some(&pack_b), "audit the bravo ledger").contains("## bravo"));

        // Reconfigure that DISABLES the section: the old pack is CLEARED —
        // loads return None and the task goes out verbatim.
        let disabled = load_pack(&Config::builtin());
        assert!(disabled.is_none());
        assert_eq!(prepare_task(disabled.as_deref(), query), query);

        // Reconfigure pointing at a MISSING directory: same clearing —
        // diagnostic only, sends unchanged.
        let missing = load_pack(&config_with_pack(&b.path().join("no-such-dir")));
        assert!(missing.is_none());
        assert_eq!(prepare_task(missing.as_deref(), query), query);
    }
}

#[cfg(test)]
#[path = "fleet_skills_h2_tests.rs"]
mod h2_tests;
