//! C3 acceptance: the Set A traversal/symlink cases (preserved per the
//! arbitration) PLUS the parent-race case their implementation could not
//! defend — driven through the real `Sandbox` API, not raw `OpenOptions`.
#![cfg(unix)] // TODO(windows-phase1): symlink/parent-race cases need the Windows sandbox

use std::fs;
use std::os::unix::fs::symlink;

use awili_engine_sandbox::{Sandbox, SandboxError};

fn root() -> (tempfile::TempDir, Sandbox) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("sub/deep")).unwrap();
    fs::write(dir.path().join("inside.txt"), b"in").unwrap();
    fs::write(dir.path().join("sub/deep/leaf.txt"), b"leaf").unwrap();
    let sb = Sandbox::open_root(dir.path()).unwrap();
    (dir, sb)
}

#[test]
fn path_inside_root_allowed() {
    let (_d, sb) = root();
    assert_eq!(sb.read("inside.txt").unwrap(), b"in");
    assert_eq!(sb.read("sub/deep/leaf.txt").unwrap(), b"leaf");
}

#[test]
fn nonexistent_file_inside_root_resolves_then_creates() {
    let (_d, sb) = root();
    assert!(!sb.exists("new.txt").unwrap());
    sb.write("new.txt", b"x").unwrap();
    assert_eq!(sb.read("new.txt").unwrap(), b"x");
}

#[test]
fn dotdot_climbing_above_root_denied() {
    let (_d, sb) = root();
    assert!(matches!(
        sb.read("../etc/passwd"),
        Err(SandboxError::Escape(_))
    ));
    assert!(matches!(
        sb.read("sub/../../x"),
        Err(SandboxError::Escape(_))
    ));
}

#[test]
fn absolute_path_outside_root_denied() {
    let (_d, sb) = root();
    assert!(matches!(
        sb.read("/etc/passwd"),
        Err(SandboxError::Escape(_))
    ));
    assert!(matches!(
        sb.write("/tmp/evil", b"x"),
        Err(SandboxError::Escape(_))
    ));
    assert!(matches!(sb.read("~/secrets"), Err(SandboxError::Escape(_))));
}

#[test]
fn symlink_leaf_to_outside_root_denied() {
    let (d, sb) = root();
    let outside = d.path().parent().unwrap().join("outside-secret.txt");
    fs::write(&outside, b"secret").unwrap();
    symlink(&outside, d.path().join("link.txt")).unwrap();
    // O_NOFOLLOW on the leaf: the open itself fails.
    assert!(matches!(sb.read("link.txt"), Err(SandboxError::Symlink(_))));
    // And a write through the symlink cannot land bytes on the target.
    assert!(matches!(
        sb.write("link.txt", b"pwned"),
        Err(SandboxError::Symlink(_))
    ));
    assert_eq!(fs::read(&outside).unwrap(), b"secret");
}

/// THE case the Set A implementation could not defend: the escape is in a
/// PARENT component, not the leaf. Pathname validation + a final-component
/// O_NOFOLLOW open lets this through; descriptor-relative walking does not.
#[test]
fn symlink_parent_component_denied() {
    let (d, sb) = root();
    let outside_dir = d.path().parent().unwrap().join("outside-dir");
    fs::create_dir_all(&outside_dir).unwrap();
    fs::write(outside_dir.join("target.txt"), b"secret").unwrap();
    symlink(&outside_dir, d.path().join("parentlink")).unwrap();

    match sb.read("parentlink/target.txt") {
        Err(SandboxError::Symlink(_)) => {}
        other => panic!("parent-component symlink must be refused, got {other:?}"),
    }
    match sb.write("parentlink/target.txt", b"pwned") {
        Err(SandboxError::Symlink(_)) => {}
        other => panic!("parent-component symlink write must be refused, got {other:?}"),
    }
    assert_eq!(fs::read(outside_dir.join("target.txt")).unwrap(), b"secret");
}

/// Sibling directory sharing the root's name prefix is NOT inside it —
/// structurally impossible here (resolution is anchored at the root fd, so
/// there is no string comparison to fool), asserted for the record.
#[test]
fn sibling_prefix_is_not_containment() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("work");
    let sibling = dir.path().join("work-evil");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("x.txt"), b"evil").unwrap();
    let sb = Sandbox::open_root(&root).unwrap();
    assert!(sb.read("../work-evil/x.txt").is_err());
    assert!(sb.read("/work-evil/x.txt").is_err());
}

/// Mid-walk swap: replace a real directory with a symlink between
/// resolution steps. The walk holds descriptors, so the swap either has no
/// effect (we already hold the real dir fd) or is refused at open — never
/// silently redirected.
#[test]
fn parent_swapped_mid_walk_is_never_followed() {
    let (d, sb) = root();
    let outside_dir = d.path().parent().unwrap().join("swap-target");
    fs::create_dir_all(&outside_dir).unwrap();
    fs::write(outside_dir.join("leaf.txt"), b"attacker").unwrap();

    // Swap sub/deep for a symlink to the outside dir AFTER the sandbox was
    // opened (simulating the race window a pathname-validating design has).
    fs::remove_file(d.path().join("sub/deep/leaf.txt")).unwrap();
    fs::remove_dir(d.path().join("sub/deep")).unwrap();
    symlink(&outside_dir, d.path().join("sub/deep")).unwrap();

    match sb.read("sub/deep/leaf.txt") {
        Err(SandboxError::Symlink(_)) => {}
        other => panic!("swapped parent must be refused, got {other:?}"),
    }
}

/// Root stays anchored to the authorized directory even if it is renamed.
#[test]
fn root_fd_survives_rename() {
    let (d, sb) = root();
    let renamed = d.path().parent().unwrap().join("renamed-root");
    fs::rename(d.path(), &renamed).unwrap();
    assert_eq!(sb.read("inside.txt").unwrap(), b"in");
    fs::rename(&renamed, d.path()).unwrap();
}

/// Hardlink containment: a link inside the root to outside content is
/// refused (Codex C3 P0) — no disclosure, no overwrite.
#[test]
fn hardlink_to_outside_content_denied() {
    let (d, sb) = root();
    let outside = d.path().parent().unwrap().join("hl-secret.txt");
    fs::write(&outside, b"secret").unwrap();
    // Same filesystem (tempdir parent), so the link succeeds — RC2-ENG6
    // (F0601): and it MUST, or the denial below was never exercised (a
    // failed link used to skip every assertion silently).
    fs::hard_link(&outside, d.path().join("hl.txt"))
        .expect("a hard link inside the temp dir must be possible here");
    assert!(matches!(sb.read("hl.txt"), Err(SandboxError::Hardlink(_))));
    assert!(matches!(
        sb.write("hl.txt", b"pwned"),
        Err(SandboxError::Hardlink(_))
    ));
    assert_eq!(fs::read(&outside).unwrap(), b"secret");
    let _ = fs::remove_file(&outside);
}

/// A genuinely REPLACED parent between resolution steps: the walk holds
/// descriptors, so a swap after the fd is obtained cannot redirect it.
#[test]
fn parent_replaced_after_descriptor_held() {
    let (d, sb) = root();
    let outside_dir = d.path().parent().unwrap().join("race-target");
    fs::create_dir_all(&outside_dir).unwrap();
    fs::write(outside_dir.join("leaf.txt"), b"attacker").unwrap();

    // Read once to prove the real content is reachable.
    assert_eq!(sb.read("sub/deep/leaf.txt").unwrap(), b"leaf");

    // Now swap the REAL directory for a symlink and re-read: refused,
    // never redirected to the attacker's content.
    fs::remove_file(d.path().join("sub/deep/leaf.txt")).unwrap();
    fs::remove_dir(d.path().join("sub/deep")).unwrap();
    symlink(&outside_dir, d.path().join("sub/deep")).unwrap();
    match sb.read("sub/deep/leaf.txt") {
        Err(SandboxError::Symlink(_)) => {}
        Ok(v) => panic!(
            "swapped parent redirected to {:?}",
            String::from_utf8_lossy(&v)
        ),
        Err(e) => panic!("unexpected {e:?}"),
    }
    let _ = fs::remove_dir_all(&outside_dir);
}

/// Degenerate and hostile path shapes are refused, never panic.
#[test]
fn degenerate_paths_refused() {
    let (_d, sb) = root();
    for p in ["", ".", "./", "//", "sub//deep", "sub/./deep/leaf.txt"] {
        // Either resolves safely or errors — never panics, never escapes.
        let _ = sb.exists(p);
    }
    assert!(sb.read("").is_err());
    assert!(sb.read(".").is_err());
    // Embedded NUL cannot form a CString.
    assert!(matches!(
        sb.read("a\0b"),
        Err(SandboxError::InvalidComponent(_))
    ));
}

// --- C3b: edit / list / glob / grep, same containment guarantees ---------

#[test]
fn edit_replaces_within_sandbox() {
    let (_d, sb) = root();
    sb.write("edit.txt", b"alpha beta alpha").unwrap();
    assert_eq!(sb.edit("edit.txt", b"alpha", b"gamma").unwrap(), 2);
    assert_eq!(sb.read("edit.txt").unwrap(), b"gamma beta gamma");
    assert_eq!(sb.edit("edit.txt", b"nope", b"x").unwrap(), 0);
    assert!(sb.edit("edit.txt", b"", b"x").is_err());
}

#[test]
fn edit_refuses_escapes_like_read_and_write() {
    let (d, sb) = root();
    let outside = d.path().parent().unwrap().join("edit-secret.txt");
    fs::write(&outside, b"secret").unwrap();
    symlink(&outside, d.path().join("elink.txt")).unwrap();
    assert!(matches!(
        sb.edit("elink.txt", b"s", b"z"),
        Err(SandboxError::Symlink(_))
    ));
    assert!(matches!(
        sb.edit("../x", b"a", b"b"),
        Err(SandboxError::Escape(_))
    ));
    assert_eq!(fs::read(&outside).unwrap(), b"secret");
}

#[test]
fn list_dir_is_bounded_and_labels_dirs() {
    let (_d, sb) = root();
    let top = sb.list_dir("").unwrap();
    assert!(top.iter().any(|e| e.rel == "inside.txt" && !e.is_dir));
    assert!(top.iter().any(|e| e.rel == "sub" && e.is_dir));
    let deep = sb.list_dir("sub/deep").unwrap();
    assert!(deep.iter().any(|e| e.rel == "sub/deep/leaf.txt"));
    assert!(matches!(sb.list_dir("../"), Err(SandboxError::Escape(_))));
}

#[test]
fn glob_matches_and_never_leaves_the_root() {
    let (d, sb) = root();
    sb.write("a.md", b"x").unwrap();
    sb.write("b.md", b"y").unwrap();
    // A symlinked directory must not be walked into.
    let outside_dir = d.path().parent().unwrap().join("glob-outside");
    fs::create_dir_all(&outside_dir).unwrap();
    fs::write(outside_dir.join("secret.md"), b"s").unwrap();
    symlink(&outside_dir, d.path().join("linkdir")).unwrap();

    let hits = sb.glob("*.md").unwrap();
    assert!(hits.contains(&"a.md".to_string()) && hits.contains(&"b.md".to_string()));
    assert!(
        !hits.iter().any(|h| h.contains("secret")),
        "glob escaped: {hits:?}"
    );
    let _ = fs::remove_dir_all(&outside_dir);
}

#[test]
fn grep_finds_inside_and_skips_unreadable() {
    let (d, sb) = root();
    sb.write("hay.txt", b"one\nneedle here\nthree").unwrap();
    let outside = d.path().parent().unwrap().join("grep-secret.txt");
    fs::write(&outside, b"needle secret").unwrap();
    symlink(&outside, d.path().join("slink.txt")).unwrap();

    let report = sb.grep("needle", "").unwrap();
    assert_eq!(
        report.matches.len(),
        1,
        "grep must not read through the symlink: {report:?}"
    );
    assert_eq!(report.matches[0].line_no, 2);
    assert_eq!(report.matches[0].rel, "hay.txt");
    // The refusal is REPORTED, not silently omitted.
    assert!(
        report
            .skipped
            .iter()
            .any(|(p, why)| p == "slink.txt" && why.contains("symlink")),
        "skipped entries must be reported: {:?}",
        report.skipped
    );
    assert!(sb.grep("", "").is_err());
    let _ = fs::remove_file(&outside);
}

#[test]
fn edit_preserves_non_utf8_bytes() {
    let (_d, sb) = root();
    let raw: Vec<u8> = vec![0xff, 0xfe, b'A', b'B', 0x00, 0x80];
    sb.write("bin.dat", &raw).unwrap();
    assert_eq!(sb.edit("bin.dat", b"AB", b"CD").unwrap(), 1);
    assert_eq!(
        sb.read("bin.dat").unwrap(),
        vec![0xff, 0xfe, b'C', b'D', 0x00, 0x80]
    );
}

#[test]
fn glob_pattern_shapes() {
    let (_d, sb) = root();
    sb.write("readme.md", b"x").unwrap();
    sb.write("notes-2026.txt", b"x").unwrap();
    assert!(sb
        .glob("readme.md")
        .unwrap()
        .contains(&"readme.md".to_string()));
    assert!(sb.glob("*").unwrap().len() >= 2);
    assert!(sb
        .glob("notes-*.txt")
        .unwrap()
        .contains(&"notes-2026.txt".to_string()));
    assert!(sb.glob("*.md").unwrap().iter().all(|h| h.ends_with(".md")));
    assert!(sb.glob("nomatch*").unwrap().is_empty());
}

/// Regression for the dup-shares-offset bug: listing the SAME directory
/// twice must return the same entries, not empty the second time.
#[test]
fn repeated_listing_is_stable() {
    let (_d, sb) = root();
    let a = sb.list_dir("").unwrap();
    let b = sb.list_dir("").unwrap();
    assert_eq!(a, b);
    assert!(!a.is_empty());
    assert_eq!(sb.glob("*.txt").unwrap(), sb.glob("*.txt").unwrap());
}

/// RC1-O: the model is told the folder's full path and then uses it. An
/// absolute path that names a place inside the root works like the relative
/// path that follows the root; anything else absolute is still an escape.
#[test]
fn rc1o_absolute_path_inside_root_is_the_relative_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("work");
    let sibling = dir.path().join("work-evil");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::create_dir_all(&sibling).unwrap();
    fs::write(root.join("inside.txt"), b"in").unwrap();
    fs::write(sibling.join("x.txt"), b"evil").unwrap();
    let sb = Sandbox::open_root(&root).unwrap();
    let abs = |p: &str| root.join(p).to_str().unwrap().to_string();

    assert_eq!(sb.read(&abs("inside.txt")).unwrap(), b"in");
    sb.write(&abs("sub/a.txt"), b"one").unwrap();
    assert_eq!(fs::read(root.join("sub/a.txt")).unwrap(), b"one");
    assert!(sb.exists(&abs("sub/a.txt")).unwrap());
    let names: Vec<String> = sb
        .list_dir(root.to_str().unwrap())
        .unwrap()
        .into_iter()
        .map(|e| e.rel)
        .collect();
    assert!(names.iter().any(|n| n.ends_with("inside.txt")), "{names:?}");
    assert!(sb.list_dir(&abs("sub")).is_ok());

    // Component-wise: a sibling sharing the root's name prefix is outside.
    let evil = sibling.join("x.txt");
    assert!(matches!(
        sb.read(evil.to_str().unwrap()),
        Err(SandboxError::Escape(_))
    ));
    assert!(matches!(
        sb.write(sibling.join("y.txt").to_str().unwrap(), b"x"),
        Err(SandboxError::Escape(_))
    ));
    assert!(!sibling.join("y.txt").exists());
    // `..` after the root is still refused.
    assert!(matches!(
        sb.read(&abs("../work-evil/x.txt")),
        Err(SandboxError::Escape(_))
    ));
    // The root itself is not a file.
    assert!(sb.read(root.to_str().unwrap()).is_err());
    // A symlink inside still cannot lead out by the absolute spelling.
    symlink(&sibling, root.join("link")).unwrap();
    assert!(sb.read(&abs("link/x.txt")).is_err());
}

/// RC1-P (Codex pre-review P2 on RC1-O, three times): a same-device bind
/// mount inside the root — a directory, and a single file — reaches outside
/// the root while the device id matches; the sandbox refuses it by mount
/// identity. Linux only (an unprivileged user + mount namespace via
/// `unshare`); the test re-runs itself inside that namespace and fails if it
/// cannot, so a passing gate means the property was exercised.
#[cfg(target_os = "linux")]
#[test]
fn rc1p_a_same_device_bind_mount_inside_the_root_is_refused() {
    use std::os::unix::fs::MetadataExt;
    const CHILD: &str = "AWILI_RC1P_BIND_CHILD";
    const NAME: &str = "rc1p_a_same_device_bind_mount_inside_the_root_is_refused";
    if let Ok(root) = std::env::var(CHILD) {
        let root = std::path::PathBuf::from(root);
        // The mounts are in place and on the root's own device.
        let dev = fs::metadata(&root).unwrap().dev();
        assert_eq!(fs::metadata(root.join("mnt")).unwrap().dev(), dev);
        assert_eq!(
            fs::metadata(root.join("mnt/secret.txt")).unwrap().dev(),
            dev
        );
        assert_eq!(fs::read(root.join("mnt/secret.txt")).unwrap(), b"secret");
        assert_eq!(fs::read(root.join("file-bind.txt")).unwrap(), b"secret");
        let sb = Sandbox::open_root(&root).unwrap();
        assert_eq!(sb.read("inside.txt").unwrap(), b"in");
        assert!(matches!(
            sb.read("mnt/secret.txt"),
            Err(SandboxError::Escape(_))
        ));
        assert!(matches!(
            sb.write("mnt/new.txt", b"x"),
            Err(SandboxError::Escape(_))
        ));
        assert!(matches!(sb.list_dir("mnt"), Err(SandboxError::Escape(_))));
        assert!(matches!(
            sb.read("file-bind.txt"),
            Err(SandboxError::Escape(_))
        ));
        assert!(matches!(
            sb.write("file-bind.txt", b"x"),
            Err(SandboxError::Escape(_))
        ));
        assert!(!root.join("mnt/new.txt").exists());
        assert_eq!(fs::read(root.join("file-bind.txt")).unwrap(), b"secret");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let outside = dir.path().join("outside");
    fs::create_dir_all(root.join("mnt")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(root.join("inside.txt"), b"in").unwrap();
    fs::write(root.join("file-bind.txt"), b"placeholder").unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    let exe = std::env::current_exe().unwrap();
    let script = r#"mount --bind "$2" "$1/mnt" && mount --bind "$2/secret.txt" "$1/file-bind.txt" && exec "$3" --exact "$4" --nocapture"#;
    let out = std::process::Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "sh",
            "-c",
            script,
            "sh",
        ])
        .arg(&root)
        .arg(&outside)
        .arg(&exe)
        .arg(NAME)
        .env(CHILD, &root)
        .output()
        .expect("unshare must be available on the Linux gate");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the bind-mount child must run and pass:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Outside the namespace nothing was mounted or written.
    assert_eq!(
        fs::read(root.join("file-bind.txt")).unwrap(),
        b"placeholder"
    );
    assert!(!outside.join("new.txt").exists());
}

/// RC2-ENG6 (S-P2, S-P3): an unprivileged macOS cross-mount test. A disk
/// image is attached (no admin rights needed) on a folder INSIDE the root:
/// reads, writes, listing and globbing under it are refused, and neither
/// `exists` nor `glob` names the mount point or what is on it. Fails —
/// never skips — if the image cannot be made or attached, so a passing run
/// means the mount was exercised. Detached on every exit.
#[cfg(target_os = "macos")]
#[test]
fn rc2eng6_a_disk_image_mounted_inside_the_folder_is_refused() {
    struct Detach(std::path::PathBuf);
    impl Drop for Detach {
        fn drop(&mut self) {
            let _ = std::process::Command::new("hdiutil")
                .args(["detach", "-force", "-quiet"])
                .arg(&self.0)
                .status();
        }
    }
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let mnt = root.join("mnt");
    fs::create_dir_all(&mnt).unwrap();
    fs::write(root.join("inside.txt"), b"in").unwrap();
    let image = base.path().join("image.dmg");
    let made = std::process::Command::new("hdiutil")
        .args([
            "create", "-quiet", "-size", "1m", "-fs", "HFS+", "-volname", "rc2eng6",
        ])
        .arg(&image)
        .status()
        .expect("hdiutil must exist on macOS");
    assert!(made.success(), "hdiutil create failed");
    let attached = std::process::Command::new("hdiutil")
        .args([
            "attach",
            "-quiet",
            "-nobrowse",
            "-noverify",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(&mnt)
        .arg(&image)
        .status()
        .expect("hdiutil must exist on macOS");
    assert!(attached.success(), "hdiutil attach failed");
    let _detach = Detach(mnt.clone());
    fs::write(mnt.join("secret.txt"), b"secret").unwrap();

    let sb = Sandbox::open_root(&root).unwrap();
    assert_eq!(sb.read("inside.txt").unwrap(), b"in");
    assert!(matches!(
        sb.read("mnt/secret.txt"),
        Err(SandboxError::Escape(_))
    ));
    assert!(matches!(
        sb.write("mnt/new.txt", b"x"),
        Err(SandboxError::Escape(_))
    ));
    assert!(matches!(sb.list_dir("mnt"), Err(SandboxError::Escape(_))));
    assert!(matches!(
        sb.exists("mnt/secret.txt"),
        Err(SandboxError::Escape(_))
    ));
    // S-P3: the mount point itself is not reported as the folder's entry.
    assert!(matches!(sb.exists("mnt"), Err(SandboxError::Escape(_))));
    assert!(sb.glob("mnt/*.txt").is_err());
    assert!(!mnt.join("new.txt").exists());
    assert_eq!(fs::read(mnt.join("secret.txt")).unwrap(), b"secret");
}

/// RC2-ENG6 (S-P3): a same-device bind mount inside the root — a directory
/// and a single file — is never named by `exists` or `glob` (Linux; the
/// test re-runs itself in an unprivileged user + mount namespace, as
/// `rc1p_a_same_device_bind_mount_inside_the_root_is_refused` does, and
/// fails if it cannot).
#[cfg(target_os = "linux")]
#[test]
fn rc2eng6_exists_and_glob_never_name_a_bind_mounted_entry() {
    const CHILD: &str = "AWILI_RC2ENG6_BIND_CHILD";
    const NAME: &str = "rc2eng6_exists_and_glob_never_name_a_bind_mounted_entry";
    if let Ok(root) = std::env::var(CHILD) {
        let root = std::path::PathBuf::from(root);
        assert_eq!(
            fs::read(root.join("files/file-bind.txt")).unwrap(),
            b"secret"
        );
        let sb = Sandbox::open_root(&root).unwrap();
        assert!(sb.exists("inside.txt").unwrap());
        assert!(matches!(
            sb.exists("files/file-bind.txt"),
            Err(SandboxError::Escape(_))
        ));
        assert!(matches!(sb.exists("mnt"), Err(SandboxError::Escape(_))));
        assert!(matches!(
            sb.exists("mnt/secret.txt"),
            Err(SandboxError::Escape(_))
        ));
        // A glob over the folder with the bind-mounted file leaves it out.
        // (A walk that meets a mounted DIRECTORY still refuses the whole
        // glob, as before — it never names what is under it.)
        assert_eq!(
            sb.glob("files/*.txt").unwrap(),
            vec!["files/plain.txt".to_string()]
        );
        assert!(matches!(sb.glob("*.txt"), Err(SandboxError::Escape(_))));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let outside = dir.path().join("outside");
    fs::create_dir_all(root.join("mnt")).unwrap();
    fs::create_dir_all(root.join("files")).unwrap();
    fs::write(root.join("files/plain.txt"), b"plain").unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(root.join("inside.txt"), b"in").unwrap();
    fs::write(root.join("files/file-bind.txt"), b"placeholder").unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    let exe = std::env::current_exe().unwrap();
    let script = r#"mount --bind "$2" "$1/mnt" && mount --bind "$2/secret.txt" "$1/files/file-bind.txt" && exec "$3" --exact "$4" --nocapture"#;
    let out = std::process::Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "sh",
            "-c",
            script,
            "sh",
        ])
        .arg(&root)
        .arg(&outside)
        .arg(&exe)
        .arg(NAME)
        .env(CHILD, &root)
        .output()
        .expect("unshare must be available on the Linux gate");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the bind-mount child must run and pass:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// RC2-ENGF1 (RC2-ENG slice review P2): an UNREADABLE same-device bind
/// mount inside the root — a file and a directory — is never named by
/// `exists` or `glob`. Before RC2-ENGF1 a leaf that would not open for
/// reading was judged by its device id alone, so these looked like ordinary
/// entries of the folder. An unreadable plain file of the folder is still
/// found. Linux: the test re-runs itself in an unprivileged user + mount
/// namespace, makes the mounts, then drops every capability (`setpriv`) so
/// the mode-000 leaves are unreadable to it; it fails — never skips — if it
/// cannot.
#[cfg(target_os = "linux")]
#[test]
fn rc2engf1_an_unreadable_bind_mount_is_never_named() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    const CHILD: &str = "AWILI_RC2ENGF1_BIND_CHILD";
    const NAME: &str = "rc2engf1_an_unreadable_bind_mount_is_never_named";
    if let Ok(root) = std::env::var(CHILD) {
        let root = std::path::PathBuf::from(root);
        // The setup holds: same device, and this process cannot read them.
        let dev = fs::metadata(&root).unwrap().dev();
        for leaf in [
            "files/locked-bind.txt",
            "locked-mnt",
            "files/locked-plain.txt",
        ] {
            assert_eq!(
                fs::symlink_metadata(root.join(leaf)).unwrap().dev(),
                dev,
                "{leaf}"
            );
        }
        assert_eq!(
            fs::read(root.join("files/locked-bind.txt"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(fs::read_dir(root.join("locked-mnt")).is_err());
        let sb = Sandbox::open_root(&root).unwrap();
        assert!(matches!(
            sb.exists("files/locked-bind.txt"),
            Err(SandboxError::Escape(_))
        ));
        assert!(matches!(
            sb.exists("locked-mnt"),
            Err(SandboxError::Escape(_))
        ));
        // An unreadable file that is the folder's own is still found.
        assert!(sb.exists("files/locked-plain.txt").unwrap());
        assert_eq!(
            sb.glob("files/*.txt").unwrap(),
            vec![
                "files/locked-plain.txt".to_string(),
                "files/plain.txt".to_string()
            ]
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let outside = dir.path().join("outside");
    fs::create_dir_all(root.join("files")).unwrap();
    fs::create_dir_all(root.join("locked-mnt")).unwrap();
    fs::create_dir_all(outside.join("locked-dir")).unwrap();
    fs::write(root.join("files/plain.txt"), b"plain").unwrap();
    fs::write(root.join("files/locked-plain.txt"), b"mine").unwrap();
    fs::write(root.join("files/locked-bind.txt"), b"placeholder").unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    let lock = |p: &std::path::Path, mode| {
        fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap()
    };
    lock(&outside.join("secret.txt"), 0o000);
    lock(&outside.join("locked-dir"), 0o000);
    lock(&root.join("files/locked-plain.txt"), 0o000);
    let exe = std::env::current_exe().unwrap();
    let script = r#"mount --bind "$2/secret.txt" "$1/files/locked-bind.txt" && mount --bind "$2/locked-dir" "$1/locked-mnt" && exec setpriv --inh-caps=-all --bounding-set=-all "$3" --exact "$4" --nocapture"#;
    let out = std::process::Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "sh",
            "-c",
            script,
            "sh",
        ])
        .arg(&root)
        .arg(&outside)
        .arg(&exe)
        .arg(NAME)
        .env(CHILD, &root)
        .output()
        .expect("unshare must be available on the Linux gate");
    // Readable again, so the temporary folder can be removed.
    lock(&outside.join("secret.txt"), 0o600);
    lock(&outside.join("locked-dir"), 0o700);
    lock(&root.join("files/locked-plain.txt"), 0o600);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "the bind-mount child must run and pass:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// RC2-ENGF1 (macOS): leaves that cannot be opened for reading are judged
/// without opening them. An unreadable file and folder of the folder are
/// still found (their filesystem id is the root's), and an unreadable disk
/// image mounted inside the folder is never named. Fails — never skips — if
/// the image cannot be made or attached; detached on every exit.
#[cfg(target_os = "macos")]
#[test]
fn rc2engf1_unreadable_leaves_are_judged_without_opening_them() {
    use std::os::unix::fs::PermissionsExt;
    struct Detach(std::path::PathBuf);
    impl Drop for Detach {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            let _ = std::process::Command::new("hdiutil")
                .args(["detach", "-force", "-quiet"])
                .arg(&self.0)
                .status();
        }
    }
    let lock = |p: &std::path::Path, mode| {
        fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap()
    };
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let mnt = root.join("mnt");
    fs::create_dir_all(&mnt).unwrap();
    fs::create_dir_all(root.join("locked-dir")).unwrap();
    fs::create_dir_all(root.join("files")).unwrap();
    fs::write(root.join("files/plain.txt"), b"plain").unwrap();
    fs::write(root.join("files/locked.txt"), b"mine").unwrap();
    let image = base.path().join("image.dmg");
    let made = std::process::Command::new("hdiutil")
        .args([
            "create", "-quiet", "-size", "1m", "-fs", "HFS+", "-volname", "rc2engf1",
        ])
        .arg(&image)
        .status()
        .expect("hdiutil must exist on macOS");
    assert!(made.success(), "hdiutil create failed");
    let attached = std::process::Command::new("hdiutil")
        .args([
            "attach",
            "-quiet",
            "-nobrowse",
            "-noverify",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(&mnt)
        .arg(&image)
        .status()
        .expect("hdiutil must exist on macOS");
    assert!(attached.success(), "hdiutil attach failed");
    let _detach = Detach(mnt.clone());
    lock(&mnt, 0o000);
    lock(&root.join("locked-dir"), 0o000);
    lock(&root.join("files/locked.txt"), 0o000);
    let sb = Sandbox::open_root(&root).unwrap();
    let locked_file = sb.exists("files/locked.txt");
    let locked_dir = sb.exists("locked-dir");
    let mounted = sb.exists("mnt");
    let globbed = sb.glob("files/*.txt");
    lock(&root.join("locked-dir"), 0o700);
    lock(&root.join("files/locked.txt"), 0o600);
    assert!(fs::read(root.join("files/plain.txt")).is_ok());
    assert!(
        locked_file.unwrap(),
        "an unreadable file of the folder is found"
    );
    assert!(
        locked_dir.unwrap(),
        "an unreadable folder of the folder is found"
    );
    assert!(
        matches!(mounted, Err(SandboxError::Escape(_))),
        "an unreadable mount point is not the folder's: {mounted:?}"
    );
    assert_eq!(
        globbed.unwrap(),
        vec![
            "files/locked.txt".to_string(),
            "files/plain.txt".to_string()
        ]
    );
}
