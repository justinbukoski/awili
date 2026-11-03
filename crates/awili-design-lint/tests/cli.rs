//! CLI tests: the exact exit codes and stdout contract of the binary.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_awili-design-lint")
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin()).args(args).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "awili-design-lint-cli-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// src dir with one finding + a matching baseline.
fn setup(name: &str) -> (PathBuf, PathBuf) {
    let dir = temp_dir(name);
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("a.rs"),
        "pub fn f() { let _ = gpui::rgb(0x111111); }\n",
    )
    .unwrap();
    let baseline = dir.join("baseline.toml");
    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--init",
    ]);
    assert_eq!(code, 0, "init: {stdout}");
    assert!(
        stdout.contains("baseline written: 1 finding(s)"),
        "{stdout}"
    );
    (src, baseline)
}

#[test]
fn list_prints_all_findings() {
    let dir = temp_dir("cli-list");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("x.rs"),
        "pub fn f() { let _ = gpui::rgb(0x111111); let _ = px(2.); }\n",
    )
    .unwrap();
    let (code, stdout, _) = run(&["list", dir.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert!(stdout.contains("x.rs:1  raw_color_fn  "), "{stdout}");
    assert!(stdout.contains("x.rs:1  raw_px  "), "{stdout}");
}

#[test]
fn check_passes_with_zero_new_findings() {
    let (src, baseline) = setup("cli-pass");
    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("0 new finding(s)"), "{stdout}");
}

#[test]
fn check_fails_on_a_new_finding() {
    let (src, baseline) = setup("cli-new");
    fs::write(
        src.join("b.rs"),
        "pub fn g() { let _ = gpui::rgb(0x222222); }\n",
    )
    .unwrap();
    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "stdout: {stdout}");
    assert!(stdout.contains("b.rs:1  raw_color_fn  "), "{stdout}");
    assert!(stdout.contains("1 new finding(s)"), "{stdout}");
}

#[test]
fn stale_entries_are_reported_without_affecting_exit_code() {
    let (src, baseline) = setup("cli-stale");
    // Make one baseline entry stale: shrink the file so its fingerprint's
    // line text no longer appears… simplest: replace the finding with a
    // different one — the old fingerprint goes stale, the new one is… also
    // new. To keep zero new findings, instead remove the file entirely:
    fs::remove_file(src.join("a.rs")).unwrap();
    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "exit unaffected by stale entries: {stdout}");
    assert!(stdout.contains("0 new finding(s)"), "{stdout}");
    assert!(
        stdout.contains("1 baseline entries no longer present — run --update-baseline"),
        "{stdout}"
    );
}

#[test]
fn update_baseline_via_cli_drops_stale_entries() {
    let (src, baseline) = setup("cli-update-drop");
    fs::remove_file(src.join("a.rs")).unwrap();
    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--update-baseline",
    ]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(
        stdout.contains("baseline updated: kept 0 of 1 entry(ies) (dropped 1)"),
        "{stdout}"
    );
    let b: awili_design_lint::Baseline =
        toml::from_str(&fs::read_to_string(&baseline).unwrap()).unwrap();
    assert!(b.findings.is_empty());
}

#[test]
fn update_baseline_via_cli_refuses_to_add() {
    let (src, baseline) = setup("cli-update-refuse");
    let before = fs::read_to_string(&baseline).unwrap();
    fs::write(
        src.join("b.rs"),
        "pub fn g() { let _ = gpui::rgb(0x222222); }\n",
    )
    .unwrap();
    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--update-baseline",
    ]);
    assert_eq!(code, 1, "stdout: {stdout}");
    assert!(
        stdout.contains("refusing to update baseline: 1 finding(s) not in baseline"),
        "{stdout}"
    );
    assert_eq!(
        before,
        fs::read_to_string(&baseline).unwrap(),
        "no write on refusal"
    );
}

#[test]
fn init_is_one_shot_via_cli() {
    let dir = temp_dir("cli-init");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("a.rs"),
        "pub fn f() { let _ = gpui::rgb(0x111111); }\n",
    )
    .unwrap();
    let baseline = dir.join("baseline.toml");

    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--init",
    ]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(baseline.exists());

    let (code, _stdout, stderr) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--init",
    ]);
    assert_eq!(code, 2, "second --init must be a usage error: {stderr}");
    assert!(stderr.contains("--init is one-shot"), "{stderr}");
}

// --- ratchet safety (docs/v2/slices/02b-lint-fixes.md item 9): `--reference`
// — the working baseline can only SHRINK against the committed one.

#[test]
fn check_fails_when_the_baseline_grew_beyond_the_reference() {
    let (src, baseline) = setup("cli-ref-grow");
    let reference = baseline.parent().unwrap().join("reference.toml");
    // The "committed" reference holds exactly the current (one) entry.
    fs::copy(&baseline, &reference).unwrap();
    // Grow the tree and hand-add the finding's fingerprint to the baseline,
    // bypassing --update-baseline (which would refuse to add it).
    fs::write(
        src.join("b.rs"),
        "pub fn g() { let _ = gpui::rgb(0x222222); }\n",
    )
    .unwrap();
    let mut fps: Vec<String> = awili_design_lint::scan(&src)
        .unwrap()
        .into_iter()
        .map(|f| f.fingerprint)
        .collect();
    fps.sort();
    fs::write(
        &baseline,
        awili_design_lint::baseline_to_toml(&awili_design_lint::Baseline {
            version: awili_design_lint::BASELINE_VERSION,
            findings: fps,
        }),
    )
    .unwrap();

    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--reference",
        reference.to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "the grown entry must fail: stdout: {stdout}");
    assert!(stdout.contains("b.rs|raw_color_fn|"), "{stdout}");
}

#[test]
fn check_passes_when_the_baseline_only_shrank_against_the_reference() {
    let (src, baseline) = setup("cli-ref-shrink");
    let reference = baseline.parent().unwrap().join("reference.toml");
    fs::copy(&baseline, &reference).unwrap();
    // Drop the finding from the tree and the baseline (a legal shrink).
    fs::remove_file(src.join("a.rs")).unwrap();
    fs::write(&baseline, "version = 1\nfindings = []\n").unwrap();
    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--reference",
        reference.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "a shrink is never a violation: stdout: {stdout}");
    assert!(stdout.contains("0 new finding(s)"), "{stdout}");
}

// --- ratchet safety (docs/v2/slices/02b-lint-fixes.md item 8): scan errors
// fail with exit 2 and the error, and `--update-baseline` never writes after
// a scan error (a silent empty scan would otherwise shrink the baseline to
// nothing, laundering away the debt record).

#[test]
fn check_fails_with_exit_2_when_the_src_dir_is_missing() {
    let (_, baseline) = setup("cli-missing-src");
    let (code, _stdout, stderr) = run(&[
        "check",
        "/definitely-missing",
        "--baseline",
        baseline.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 2,
        "a missing src-dir is a scan error: exit 2 with the error"
    );
    assert!(stderr.contains("/definitely-missing"), "{stderr}");
}

#[test]
fn list_fails_with_exit_2_when_the_src_dir_is_missing() {
    let (code, _stdout, stderr) = run(&["list", "/definitely-missing"]);
    assert_eq!(code, 2, "list must fail too: exit 2 with the error");
    assert!(stderr.contains("/definitely-missing"), "{stderr}");
}

#[test]
fn check_fails_with_exit_2_when_a_scanned_file_is_unreadable() {
    let dir = temp_dir("cli-unreadable-file");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a.rs"), "pub fn f() { let _ = px(1.0); }\n").unwrap();
    let baseline = dir.join("baseline.toml");
    let (code, stdout, _) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
        "--init",
    ]);
    assert_eq!(code, 0, "init: {stdout}");
    let secret = src.join("secret.rs");
    fs::write(&secret, "pub fn g() { let _ = px(2.0); }\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).unwrap();
    }
    let (code, _stdout, stderr) = run(&[
        "check",
        src.to_str().unwrap(),
        "--baseline",
        baseline.to_str().unwrap(),
    ]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&secret, fs::Permissions::from_mode(0o644));
    }
    assert_eq!(
        code, 2,
        "an unreadable scanned file is a scan error: exit 2"
    );
    assert!(stderr.contains("secret.rs"), "{stderr}");
}

#[test]
fn update_baseline_writes_nothing_after_a_scan_error() {
    let (_, baseline) = setup("cli-scan-err-update");
    let before = fs::read_to_string(&baseline).unwrap();
    let (code, _stdout, stderr) = run(&[
        "check",
        "/definitely-missing",
        "--baseline",
        baseline.to_str().unwrap(),
        "--update-baseline",
    ]);
    assert_eq!(code, 2, "scan errors must fail before any write: {stderr}");
    assert_eq!(
        before,
        fs::read_to_string(&baseline).unwrap(),
        "no write after a scan error"
    );
}

#[test]
fn check_without_baseline_and_without_init_is_a_usage_error() {
    let dir = temp_dir("cli-no-baseline");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("a.rs"),
        "pub fn f() { let _ = gpui::rgb(0x111111); }\n",
    )
    .unwrap();
    let (code, _stdout, stderr) = run(&[
        "check",
        dir.to_str().unwrap(),
        "--baseline",
        dir.join("missing.toml").to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("no baseline file"), "{stderr}");
}
