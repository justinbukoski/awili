//! Proof tests for the U2b executor status and silence labels.
//!
//! Pure-function tests only: no windows, no network, no timers.

use std::time::Duration;

use awili_code::CodeEngine;

use super::*;
use crate::theme::{ACCENT, CHIP_WARN, MUTED};

/// The four engine labels in a fixed order (for the F14 sweep).
const ENGINES: [CodeEngine; 4] = [
    CodeEngine::Claude,
    CodeEngine::Codex,
    CodeEngine::Glm,
    CodeEngine::Dsh,
];

#[test]
fn u2b_subject_follows_selected_executor() {
    // Work + gateway → the gateway, whatever the Code tab holds.
    let code_tab = Some((CodeEngine::Codex, Some("gpt-6-sol".to_string())));
    assert_eq!(
        subject_for(
            Mode::Work,
            &WorkExecutor::Gateway,
            code_tab.clone(),
            None,
            |_| None
        ),
        Some(Subject::Gateway),
    );

    // Work + agent → that agent, carrying the caller's cached installed flag.
    let executor = WorkExecutor::Agent {
        engine: CodeEngine::Glm,
        model: None,
    };
    assert_eq!(
        subject_for(Mode::Work, &executor, None, None, |_| Some(true)),
        Some(Subject::Agent {
            engine: CodeEngine::Glm,
            model: None,
            installed: Some(true),
        }),
    );

    // Work + local build with a resolved target label → that label.
    let build = WorkExecutor::LocalBuild {
        target_id: "spark".to_string(),
    };
    assert_eq!(
        subject_for(
            Mode::Work,
            &build,
            None,
            Some("Build · qwen on spark".to_string()),
            |_| None,
        ),
        Some(Subject::Build {
            label: "Build · qwen on spark".to_string(),
        }),
    );

    // Work + local build without a resolvable target degrades to the lane
    // name, never to an invented label.
    assert_eq!(
        subject_for(Mode::Work, &build, None, None, |_| None),
        Some(Subject::Build {
            label: "build".to_string(),
        }),
    );

    // Code with a tab → that tab's agent, even when the Work executor is the
    // gateway.
    assert_eq!(
        subject_for(
            Mode::Code,
            &WorkExecutor::Gateway,
            code_tab.clone(),
            None,
            |_| Some(false)
        ),
        Some(Subject::Agent {
            engine: CodeEngine::Codex,
            model: Some("gpt-6-sol".to_string()),
            installed: Some(false),
        }),
    );

    // Code without a tab → nothing at all.
    assert_eq!(
        subject_for(Mode::Code, &WorkExecutor::Gateway, None, None, |_| None),
        None,
    );

    // Every other mode → nothing.
    {
        let mode = Mode::Sessions;
        assert_eq!(
            subject_for(mode, &WorkExecutor::Gateway, code_tab.clone(), None, |_| {
                None
            }),
            None,
        );
    }
}

#[test]
fn u2b_gateway_part_follows_reach() {
    let gateway = Subject::Gateway;

    assert_eq!(
        executor_part(&gateway, &Reach::Up, false),
        ("● gateway".to_string(), ACCENT),
    );
    assert_eq!(
        executor_part(&gateway, &Reach::Unknown, true),
        ("○ checking gateway…".to_string(), MUTED),
    );
    assert_eq!(
        executor_part(&gateway, &Reach::Unknown, false),
        ("○ gateway".to_string(), MUTED),
    );
    let down = executor_part(
        &gateway,
        &Reach::Down("connection refused".to_string()),
        false,
    );
    assert_eq!(down.1, CHIP_WARN);
    assert!(
        down.0.starts_with("✗ ")
            && down.0.contains(GATEWAY_UNREACHABLE)
            && down.0.contains("check that this Mac is online"),
        "the Down segment carries the tunnel hint: {:?}",
        down.0,
    );
}

#[test]
fn u2b_agent_part_names_engine_and_model() {
    // GLM, model unknown, installed.
    assert_eq!(
        executor_part(
            &Subject::Agent {
                engine: CodeEngine::Glm,
                model: None,
                installed: Some(true),
            },
            &Reach::Unknown,
            false,
        ),
        ("● GLM · default".to_string(), ACCENT),
    );
    // Claude with a named model, installed.
    assert_eq!(
        executor_part(
            &Subject::Agent {
                engine: CodeEngine::Claude,
                model: Some("sonnet".to_string()),
                installed: Some(true),
            },
            &Reach::Unknown,
            false,
        ),
        ("● Claude · sonnet".to_string(), ACCENT),
    );
    // DeepSeek missing its binary.
    assert_eq!(
        executor_part(
            &Subject::Agent {
                engine: CodeEngine::Dsh,
                model: None,
                installed: Some(false),
            },
            &Reach::Unknown,
            false,
        ),
        ("✗ DeepSeek not installed".to_string(), CHIP_WARN),
    );
    // Codex, not checked yet.
    assert_eq!(
        executor_part(
            &Subject::Agent {
                engine: CodeEngine::Codex,
                model: None,
                installed: None,
            },
            &Reach::Unknown,
            false,
        ),
        ("○ Codex · default".to_string(), MUTED),
    );
}

#[test]
fn u2b_selected_agent_never_reads_gateway() {
    // F14 regression: the status run that read "● gateway" with GLM selected.
    // For every engine the Agent part must name the engine, never the gateway.
    for engine in ENGINES {
        for installed in [None, Some(true), Some(false)] {
            let subject = Subject::Agent {
                engine,
                model: None,
                installed,
            };
            let (text, _) = executor_part(&subject, &Reach::Unknown, false);
            assert!(
                !text.to_lowercase().contains("gateway"),
                "engine {engine:?} ({installed:?}) must not read gateway: {text:?}"
            );
            assert!(
                text.contains(engine.label()),
                "the part names the engine: {text:?}"
            );
        }
    }
}

#[test]
fn u2b_idle_label_thresholds() {
    // RC2-RUN (round 6 Minor 1, deliberate change): a streaming run's
    // silence reads as plain progress (muted) for two minutes, then amber
    // with what the owner can do; Codex says how it delivers its reply.
    // Below the threshold: nothing.
    assert_eq!(
        idle_part(Duration::from_secs(19), false, Quiet::Streams),
        None
    );
    // Right at the threshold.
    assert_eq!(
        idle_part(Duration::from_secs(20), false, Quiet::Streams),
        Some(("still working — nothing new for 20s".to_string(), MUTED)),
    );
    assert_eq!(
        idle_part(Duration::from_secs(119), false, Quiet::Streams),
        Some(("still working — nothing new for 119s".to_string(), MUTED)),
    );
    // Two minutes and a bit: minutes + zero-padded seconds, amber.
    assert_eq!(
        idle_part(Duration::from_secs(245), false, Quiet::Streams),
        Some((
            "nothing new for 4m 05s — press Stop if it seems stuck".to_string(),
            CHIP_WARN
        )),
    );
    // Waiting on the user (approval / credential prompt): no label at any
    // silence.
    assert_eq!(
        idle_part(Duration::from_secs(90), true, Quiet::Streams),
        None
    );
    // A whole-output engine: the silence is normal, labelled as such.
    assert_eq!(
        idle_part(Duration::from_secs(45), false, Quiet::WholeReply("GLM")),
        Some((
            "no output yet — GLM replies at the end (45s)".to_string(),
            CHIP_WARN,
        )),
    );
    // Codex prints each message only once it is complete.
    assert_eq!(
        idle_part(
            Duration::from_secs(45),
            false,
            Quiet::WholeMessages("Codex")
        ),
        Some((
            "still working — Codex shows each message when it is written (45s)".to_string(),
            MUTED,
        )),
    );
}

#[test]
fn u2b_merged_draft() {
    // Blank composer: the refused text stands alone.
    assert_eq!(merged_draft("", "x"), "x");
    // Whitespace-only composer: treated as blank too.
    assert_eq!(merged_draft("  ", "x"), "x");
    // Text already there: refused on top, blank line, existing preserved.
    assert_eq!(merged_draft("y", "x"), "x\n\ny");
}

/// RC1-F2 (RC-15): the gateway's words are plain: no "tunnel", "Studio" or
/// "SSH" in the unreachable message or in the gateway's approval-chip help.
#[test]
fn rc1f2_gateway_words_are_plain() {
    let chip = crate::ask_mode::chip_text(crate::ask_mode::Effective::NoAsk(
        crate::ask_mode::NoAsk::FleetGateway,
    ));
    for text in [GATEWAY_UNREACHABLE, chip.label, chip.tooltip] {
        for word in ["tunnel", "Studio", "SSH", "forward"] {
            assert!(!text.contains(word), "{word:?} in {text:?}");
        }
    }
    assert!(GATEWAY_UNREACHABLE.contains("this Mac is online"));
}
