//! U3e1: the Work approval chords (⌘⇧A Allow once, ⌘⇧D Deny; Ctrl+Shift on
//! Linux) and the in-chat outcome line of a delivered verdict (S2-F08).

use awili_gateway::run_events::ApprovalDecision;
use gpui::Keystroke;

/// The chat line for a delivered verdict. `choice` is the wire scope string
/// (`ApprovalDecision::scope_str`); anything else has no line.
pub fn outcome_line(choice: &str) -> Option<String> {
    let text = match choice {
        "once" => "✓ **Allowed once**",
        // RC1-I2-FIX1: the in-app session is this conversation's worker.
        "session" => {
            "✓ **Allowed for this session** \u{2014} until you switch to another \
             conversation or quit Awili (a run that is still going finishes first); then \
             Awili asks again"
        }
        "folder" => "✓ **Allowed for this folder**",
        "always" => "✓ **Always allowed**",
        "deny" => "✗ **Denied**",
        _ => return None,
    };
    Some(format!("\n> {text}\n"))
}

/// The chord printed on the Allow once (or, with `deny`, the Deny) pill.
pub fn hint(mac: bool, deny: bool) -> &'static str {
    match (mac, deny) {
        (true, false) => "⌘⇧A",
        (true, true) => "⌘⇧D",
        (false, false) => "Ctrl⇧A",
        (false, true) => "Ctrl⇧D",
    }
}

/// ⌘⇧A → Allow once, ⌘⇧D → Deny (Ctrl+Shift on Linux), nothing else (D10:
/// three-key chords, no default button, so Return never approves).
pub fn chord(ks: &Keystroke, mac: bool) -> Option<ApprovalDecision> {
    let m = &ks.modifiers;
    let primary = if mac { m.platform } else { m.control };
    let other = if mac { m.control } else { m.platform };
    if !(primary && m.shift) || other || m.alt {
        return None;
    }
    match ks.key.as_str() {
        "a" => Some(ApprovalDecision::Approve),
        "d" => Some(ApprovalDecision::Deny),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u3e1_outcome_lines_cover_every_scope() {
        use ApprovalDecision::*;
        for decision in [Approve, Session, Folder, Always, Deny] {
            let line = outcome_line(decision.scope_str()).expect("every scope has a line");
            assert!(line.starts_with("\n> ") && line.ends_with('\n'), "{line:?}");
        }
        assert_eq!(
            outcome_line("once").as_deref(),
            Some("\n> ✓ **Allowed once**\n")
        );
        assert_eq!(outcome_line("deny").as_deref(), Some("\n> ✗ **Denied**\n"));
        assert_eq!(outcome_line("install"), None);
        assert_eq!(outcome_line(""), None);
    }

    #[test]
    fn u3e1_chords_and_hints_per_platform() {
        let ks = |s: &str| Keystroke::parse(s).unwrap();
        assert_eq!(
            chord(&ks("cmd-shift-a"), true),
            Some(ApprovalDecision::Approve)
        );
        assert_eq!(
            chord(&ks("cmd-shift-d"), true),
            Some(ApprovalDecision::Deny)
        );
        assert_eq!(
            chord(&ks("ctrl-shift-a"), false),
            Some(ApprovalDecision::Approve)
        );
        assert_eq!(
            chord(&ks("ctrl-shift-d"), false),
            Some(ApprovalDecision::Deny)
        );
        for (s, mac) in [
            ("cmd-a", true),
            ("cmd-shift-a", false),
            ("ctrl-shift-a", true),
            ("cmd-ctrl-shift-a", true),
            ("alt-cmd-shift-a", true),
            ("cmd-shift-x", true),
            ("enter", true),
            ("shift-enter", false),
        ] {
            assert_eq!(chord(&ks(s), mac), None, "{s} mac={mac}");
        }
        assert_eq!((hint(true, false), hint(true, true)), ("⌘⇧A", "⌘⇧D"));
        assert_eq!(
            (hint(false, false), hint(false, true)),
            ("Ctrl⇧A", "Ctrl⇧D")
        );
    }
}
