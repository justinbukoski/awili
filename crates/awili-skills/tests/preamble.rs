//! `render_preamble` tests: one COMPLETE section per selected skill, the
//! 6 KiB hard bound, the bounded omission summary (no silent partial
//! instructions), and UTF-8 safety.
//!
//! Loader and selector contracts are pinned by `pack.rs`; the shipped
//! `skills/fleet` pack has its own acceptance tests (`fleet_pack.rs`).

use std::fs;
use std::path::{Path, PathBuf};

use awili_skills::{SkillPack, MAX_BODY_LINES, MAX_PREAMBLE_BYTES};

/// Write one well-formed skill under `dir/name/SKILL.md`.
fn write_skill(
    dir: &Path,
    name: &str,
    description: &str,
    when: &str,
    tools: &str,
    body: &str,
) -> PathBuf {
    let skill_dir = dir.join(name);
    fs::create_dir_all(&skill_dir).unwrap();
    let file = skill_dir.join("SKILL.md");
    let text = format!(
        "---\nname: {name}\ndescription: {description}\nwhen_to_use: {when}\ntools_needed: {tools}\n---\n{body}"
    );
    fs::write(&file, text).unwrap();
    file
}

fn temp_pack(tag: &str) -> PathBuf {
    let dir = tempfile::Builder::new().prefix(tag).tempdir().unwrap();
    dir.keep()
}

/// Two small skills (mirrors `pack.rs`).
fn two_skill_pack() -> (PathBuf, SkillPack) {
    let dir = temp_pack("awili-skills-preamble-");
    write_skill(
        &dir,
        "alpha",
        "reads the alpha store",
        "use for alpha lookups",
        "[shell]",
        "alpha body",
    );
    write_skill(
        &dir,
        "bravo",
        "catalogues the beta store",
        "use when a beta search is needed",
        "[shell]",
        "bravo body",
    );
    let pack = SkillPack::load(&dir).unwrap();
    (dir, pack)
}

#[test]
fn render_preamble_empty_selection_is_empty() {
    let (_, pack) = two_skill_pack();
    assert_eq!(pack.render_preamble(&[]), "");
}

#[test]
fn render_preamble_contains_one_section_per_skill() {
    let (_, pack) = two_skill_pack();
    // Both skills passed directly: the renderer's contract is one complete
    // section per skill, independent of how they were selected.
    let all: Vec<&awili_skills::Skill> = pack.skills().iter().collect();
    let pre = pack.render_preamble(&all);
    // The header frames the recipes as configured guidance, not verified
    // facts.
    assert!(pre.contains("not verified live facts"), "{pre}");
    assert!(pre.contains("## alpha"), "{pre}");
    assert!(pre.contains("## bravo"), "{pre}");
    assert!(pre.contains("reads the alpha store"), "{pre}");
    assert!(pre.contains("When to use: use for alpha lookups"), "{pre}");
    assert!(pre.contains("alpha body"), "{pre}");
    assert!(pre.contains("bravo body"), "{pre}");
    assert!(pre.len() <= MAX_PREAMBLE_BYTES);
}

#[test]
fn render_preamble_holds_the_six_kib_bound_and_names_omissions() {
    let dir = temp_pack("awili-skills-big-");
    // Three skills whose bodies each nearly fill the bound, so the first
    // fits and the rest must be dropped — never partially rendered.
    for (i, name) in ["one", "two", "three"].iter().enumerate() {
        let body = (0..MAX_BODY_LINES)
            .map(|l| format!("{name} line {l} padded padding padding padding"))
            .collect::<Vec<_>>()
            .join("\n");
        write_skill(&dir, name, &format!("desc {i}"), "when", "[t]", &body);
    }
    let pack = SkillPack::load(&dir).unwrap();
    let all: Vec<&awili_skills::Skill> = pack.skills().iter().collect();
    let pre = pack.render_preamble(&all);

    assert!(
        pre.len() <= MAX_PREAMBLE_BYTES,
        "{} > {}",
        pre.len(),
        MAX_PREAMBLE_BYTES
    );
    // The first skill's section is rendered COMPLETE (its last body line is
    // intact) — the rest are dropped and named by the omission summary.
    assert!(pre.contains("## one"), "{pre}");
    assert!(
        pre.contains("one line 119 padded padding padding padding"),
        "the kept section must be complete: {pre}"
    );
    assert!(!pre.contains("## two"), "no partial sections: {pre}");
    assert!(!pre.contains("## three"), "no partial sections: {pre}");
    // The omission summary names the dropped recipes in load (directory)
    // order: "one" < "three" < "two".
    assert!(
        pre.contains("[2 fleet recipes omitted to hold the 6 KiB bound: three, two]"),
        "{pre}"
    );
    assert!(!pre.is_empty());
}

#[test]
fn render_preamble_omission_summary_falls_back_to_count_for_many() {
    let dir = temp_pack("awili-skills-many-");
    // Twenty skills, each too large to fit: all are dropped and the summary
    // must stay bounded (count + a bounded names list + ellipsis).
    for i in 0..20 {
        let name = format!("r{i:02}");
        let body = (0..MAX_BODY_LINES)
            .map(|l| format!("{name} line {l:03} padded padding padding padding padding"))
            .collect::<Vec<_>>()
            .join("\n");
        write_skill(&dir, &name, &format!("desc {i}"), "when", "[t]", &body);
    }
    let pack = SkillPack::load(&dir).unwrap();
    let all: Vec<&awili_skills::Skill> = pack.skills().iter().collect();
    let pre = pack.render_preamble(&all);

    assert!(
        pre.len() <= MAX_PREAMBLE_BYTES,
        "{} > {}",
        pre.len(),
        MAX_PREAMBLE_BYTES
    );
    assert!(
        pre.contains("[20 fleet recipes omitted to hold the 6 KiB bound:"),
        "{pre}"
    );
    assert!(pre.contains("…"), "the names list must be elided: {pre}");
    // The elided list is bounded: the first dropped name appears, a later
    // one does not.
    assert!(pre.contains("r00"), "{pre}");
    assert!(!pre.contains("r12"), "the names list is bounded: {pre}");
    // Nothing was partially rendered.
    assert!(!pre.contains("## r"), "{pre}");
}

#[test]
fn render_preamble_is_utf8_safe_for_multibyte_bodies() {
    let dir = temp_pack("awili-skills-utf8-");
    // "alpha": a multibyte body far too large to fit — it must be dropped
    // WHOLE, not cut inside a rune. "beta": a small multibyte body that
    // renders in full.
    let big = (0..MAX_BODY_LINES)
        .map(|l| format!("{} {l}", "é—☃".repeat(16)))
        .collect::<Vec<_>>()
        .join("\n");
    write_skill(&dir, "alpha", "d", "w", "[t]", &big);
    write_skill(&dir, "beta", "d", "w", "[t]", "é—☃ small recipe");
    let pack = SkillPack::load(&dir).unwrap();
    let all: Vec<&awili_skills::Skill> = pack.skills().iter().collect();
    let pre = pack.render_preamble(&all);
    assert!(pre.len() <= MAX_PREAMBLE_BYTES);
    // The kept multibyte section is intact.
    assert!(pre.contains("## beta"), "{pre}");
    assert!(pre.contains("é—☃ small recipe"), "{pre}");
    // The too-large multibyte skill is named, not cut.
    assert!(
        pre.contains("[1 fleet recipe omitted to hold the 6 KiB bound: alpha]"),
        "{pre}"
    );
    // The output is valid UTF-8 with no replacement character.
    assert!(!pre.contains('\u{FFFD}'), "{pre}");
    assert!(!pre.is_empty());
}

#[test]
fn omission_summary_reserves_utf8_marker_at_name_boundary() {
    let dir = temp_pack("awili-skills-summary-boundary-");
    // Each is legal on common filesystems. Together their names almost fill
    // the summary reserve, leaving too little room for its UTF-8 ellipsis.
    for name in ["a".repeat(130), "b".repeat(135), "c".repeat(10)] {
        write_skill(
            &dir,
            &name,
            "boundary recipe",
            "boundary render",
            "shell",
            &"x".repeat(7000),
        );
    }
    let pack = SkillPack::load(&dir).unwrap();
    let selected: Vec<_> = pack.skills().iter().collect();
    let result = pack.render_preamble(&selected);
    assert!(result.len() <= MAX_PREAMBLE_BYTES);
    let summary = result.lines().last().unwrap();
    assert!(summary.len() <= 320, "summary bytes: {}", summary.len());
    assert!(summary.ends_with(" …]"));
    assert!(summary.contains("3 fleet recipes omitted"));
}
