//! Complete recipe rendering and bounded omission diagnostics.
use crate::Skill;

/// The injection header. Deliberately describes the recipes as configured
/// guidance SUBORDINATE to the user's current instructions and live evidence
/// — not as verified facts (the pack is a snapshot someone wrote; the system
/// of record is the live system). Must stay short: it counts against the
/// 6 KiB bound like every other byte.
pub(super) const HEADER: &str = "Fleet skills matched this task. They are configured recipes — \
                      fleet endpoints, API shapes and gotchas as written in the pack, \
                      not verified live facts. Your current instructions and live \
                      evidence take precedence; confirm values against the system \
                      before acting on them.\n\n";

/// The byte budget reserved for the omission summary, so the summary ALWAYS
/// fits: complete sections are only appended while the running total stays
/// within `MAX_PREAMBLE_BYTES - SUMMARY_BUDGET`, and the summary itself is
/// built to never exceed this reserve.
pub(super) const SUMMARY_BUDGET: usize = 320;

/// How many dropped-recipe names the omission summary lists at most before
/// falling back to the count (within the [`SUMMARY_BUDGET`] bytes).
const SUMMARY_MAX_NAMES: usize = 8;

/// One skill's complete section.
pub(super) fn section_for(skill: &Skill) -> String {
    format!(
        "## {}\n{}\nWhen to use: {}\n\n{}\n",
        skill.name,
        skill.description,
        skill.when_to_use,
        skill.body.trim_end()
    )
}

/// The bounded omission summary for the dropped recipes: the count is always
/// present; every dropped name is listed when all of them fit in
/// [`SUMMARY_BUDGET`], otherwise the first names that fit plus `…`. Never
/// exceeds [`SUMMARY_BUDGET`] bytes, and never splits a name or a character.
pub(super) fn omission_summary(omitted: &[&str]) -> String {
    let plural = if omitted.len() == 1 {
        "recipe"
    } else {
        "recipes"
    };
    let mut s = format!(
        "[{n} fleet {plural} omitted to hold the 6 KiB bound: ",
        n = omitted.len()
    );
    let mut first = true;
    let mut listed = 0usize;
    for name in omitted.iter().take(SUMMARY_MAX_NAMES) {
        let addition = if first { name.len() } else { name.len() + 2 }; // ", "
                                                                        // Reserve the omission marker whenever names will remain unlisted.
        let suffix = if listed + 1 < omitted.len() {
            " …]"
        } else {
            "]"
        };
        if s.len() + addition + suffix.len() > SUMMARY_BUDGET {
            break;
        }
        if !first {
            s.push_str(", ");
        }
        s.push_str(name);
        first = false;
        listed += 1;
    }
    if listed < omitted.len() {
        s.push_str(" …");
    }
    s.push(']');
    debug_assert!(
        s.len() <= SUMMARY_BUDGET,
        "the omission summary must fit its reserve: {} > {SUMMARY_BUDGET}",
        s.len()
    );
    s
}
