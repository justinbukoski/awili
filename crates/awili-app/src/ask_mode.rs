//! The app's "Ask before changes" mode core (U4c, pure): which executors can
//! enforce asking, which posture each effective mode means, the chip copy, the
//! Auto switch texts, and the per-conversation mode table.
//!
//! Everything here is pure (the one `impl AwiliApp` helper at the end reads
//! the same fields as the render code). The mode belongs to a conversation
//! (spec D2): Work keys by conversation id, the Code tab keys by tab id. A
//! conversation with no explicit choice uses the global default seeded from
//! `[work] default_approval_mode` at startup (D2: "ask" | "auto"; anything
//! else, or absent, means ask; never an error).
//!
//! U4c lands this module ahead of the U4d UI, which consumes the support
//! matrix, chip copy, confirm/toast texts and the `AwiliApp` helper; the pure
//! functions are exercised by the `u4c_` tests regardless, exactly like
//! [`crate::command_catalog`] and [`crate::usage_chart`].
#![allow(dead_code)]

use std::collections::HashMap;

use awili_code::{Autonomy, CodeEngine};
use awili_config::EngineKind;
use awili_work::ApprovalMode;

use crate::work::{code_root_too_broad, WorkExecutor};

/// The conversation key a mode choice belongs to (spec D2): Work conversations
/// by conversation id, Code tabs by tab id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AskKey {
    /// A Work conversation (`work:<conv_id>`).
    Work(String),
    /// A Code-tab conversation (`code-tab:<tab id>`).
    CodeTab(u64),
}

/// How "Ask before changes" could be enforced for one executor/engine pair
/// (spec D1/D6): per tool call (the in-app engine's approval cards), per plan
/// (the vendor harnesses' read-only planning step), or not at all — the
/// executor simply cannot be asked, so Auto is the only honest posture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AskSupport {
    /// The in-app engine: every file-changing or risky call stops at the
    /// approval card; reads run freely.
    PerAction,
    /// Claude / Codex: the turn runs a read-only planning step the vendor
    /// harness enforces; one approval releases one follow-up turn.
    PlanFirst,
    /// Asking is unenforceable for this executor (`NoAsk` says why); the chip
    /// cannot be clicked and states what really happens.
    Unavailable(NoAsk),
}

/// Why an executor cannot enforce "Ask before changes" (spec D6; the U4c
/// decision table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoAsk {
    /// The fleet gateway (Hermes): its file writes happen on the Studio and
    /// only a *command* can block there.
    FleetGateway,
    /// GLM via ZCode: plan mode does not stop its plugins from changing files.
    Glm,
    /// DeepSeek via dsh: asking first is not verified yet.
    DeepSeek,
    /// Local Build: it runs remotely; Awili cannot pause it.
    Build,
}

/// The posture a run actually takes, once the chosen mode meets the executor's
/// support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Effective {
    /// In-app engine, Ask: per-tool-call approval cards.
    AskPerAction,
    /// Claude/Codex, Ask: read-only planning step, then the plan card.
    AskPlanFirst,
    /// Auto: full access, no card (still held by the in-app engine's floors).
    Auto,
    /// The executor cannot ask (`NoAsk` says why); it runs without asking.
    NoAsk(NoAsk),
}

/// The one row of chip copy for a mode (spec D1/D6 "Exact copy"): the label,
/// the tooltip explaining what really happens, whether the chip is clickable,
/// and whether it renders in the warning colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChipText {
    /// The chip label itself.
    pub label: &'static str,
    /// The tooltip — why this executor asks or doesn't.
    pub tooltip: &'static str,
    /// Clicking the chip opens the two-row Ask/Auto menu.
    pub switchable: bool,
    /// The chip renders in the warning colour.
    pub warn: bool,
}

/// The confirmation dialog title for switching a conversation to Auto.
pub(crate) const AUTO_CONFIRM_TITLE: &str = "Run without asking?";

/// The confirm button label; `None` for the keep button is spelled by
/// `KEEP_ASKING`.
pub(crate) const SWITCH_TO_AUTO: &str = "Switch to Auto";

/// The "keep asking" button label of the Auto confirmation.
pub(crate) const KEEP_ASKING: &str = "Keep asking";

/// The `NoAsk` toast for non-gateway executors, with the executor label
/// substituted (spec D6: picking one of these in an Ask conversation shows a
/// toast saying what really happens).
pub(crate) const NO_ASK_TOAST: &str =
    "{executor} can't ask before changes; it will run without asking in this conversation.";

/// Build the toast for picking an executor that cannot ask, by its support.
/// The fleet gateway gets its own wording: it DOES ask, only before risky
/// commands (its file writes never reach Awili).
pub(crate) fn no_ask_toast(support: AskSupport, executor_label: &str) -> String {
    match support {
        AskSupport::Unavailable(NoAsk::FleetGateway) => format!(
            "{executor_label} asks only before risky commands; it will run without asking for file changes in this conversation."
        ),
        _ => NO_ASK_TOAST.replace("{executor}", executor_label),
    }
}

/// Map a Work executor + the engine kind to what Ask support it has (spec
/// D6): the in-app engine enforces per call, Hermes' gateway can only flag
/// risky commands, a vendor agent harness is whatever its engine enforces,
/// and a local Build runs remotely.
pub(crate) fn work_support(executor: &WorkExecutor, kind: EngineKind) -> AskSupport {
    match executor {
        WorkExecutor::InApp | WorkExecutor::InAppExtra { .. } => AskSupport::PerAction,
        WorkExecutor::Gateway => match kind {
            EngineKind::Awili => AskSupport::PerAction,
            EngineKind::HermesGateway => AskSupport::Unavailable(NoAsk::FleetGateway),
        },
        WorkExecutor::Agent { engine, .. } => code_support(*engine),
        WorkExecutor::LocalBuild { .. } => AskSupport::Unavailable(NoAsk::Build),
    }
}

/// Map a Code engine to its Ask support: Claude and Codex enforce the
/// read-only planning step (U4a probes); GLM and DeepSeek cannot be asked.
pub(crate) fn code_support(engine: CodeEngine) -> AskSupport {
    if engine.enforces_ask() {
        AskSupport::PlanFirst
    } else {
        match engine {
            CodeEngine::Glm => AskSupport::Unavailable(NoAsk::Glm),
            CodeEngine::Dsh => AskSupport::Unavailable(NoAsk::DeepSeek),
            _ => unreachable!("enforces_ask() is true for Claude and Codex"),
        }
    }
}

/// Fold the chosen mode and the executor's support into the effective
/// posture: `Unavailable` wins whatever the mode (spec D6 — the stored Ask
/// choice is kept and applies again when an enforcing executor is picked);
/// otherwise Ask maps to the executor's asking style and Auto to Auto.
pub(crate) fn effective(mode: ApprovalMode, support: AskSupport) -> Effective {
    if let AskSupport::Unavailable(why) = support {
        return Effective::NoAsk(why);
    }
    match mode {
        ApprovalMode::AskBeforeChanges => match support {
            AskSupport::PerAction => Effective::AskPerAction,
            _ => Effective::AskPlanFirst,
        },
        ApprovalMode::Auto => Effective::Auto,
    }
}

/// The `Autonomy` a run is dispatched with for an effective posture (U4d
/// decides the argv from this): only `AskPlanFirst` takes the read-only
/// planning step; `AskPerAction` never reaches a vendor CLI (the in-app
/// engine enforces per call), so it and `Auto` both dispatch `Autonomous`.
pub(crate) fn autonomy_for(eff: Effective) -> awili_code::Autonomy {
    match eff {
        Effective::AskPlanFirst => awili_code::Autonomy::AskFirst,
        _ => awili_code::Autonomy::Autonomous,
    }
}

/// The chip copy for an effective posture — exact wording of spec D1/D6.
pub(crate) fn chip_text(eff: Effective) -> ChipText {
    match eff {
        Effective::AskPerAction => ChipText {
            label: "Ask before changes",
            tooltip: "Awili stops and asks you before the agent changes files or runs commands. \
                       Reading is free.",
            switchable: true,
            warn: false,
        },
        // A native harness does not ask per tool call: it looks around
        // read-only and proposes a plan first. Say that, not "Ask".
        Effective::AskPlanFirst => ChipText {
            label: "Plan first",
            tooltip: "The agent first looks around read-only and proposes a plan. Nothing \
                       changes until you approve it.",
            switchable: true,
            warn: false,
        },
        Effective::Auto => ChipText {
            label: "Auto",
            tooltip: "The agent changes files and runs commands without asking. Click to go \
                       back to asking.",
            switchable: true,
            warn: true,
        },
        Effective::NoAsk(NoAsk::FleetGateway) => ChipText {
            label: "Asks before risky commands",
            tooltip: "The fleet gateway runs its tools on its own computer. Awili can't hold its \
                       file changes for approval; it asks only when the gateway flags a risky \
                       command.",
            switchable: false,
            warn: false,
        },
        Effective::NoAsk(NoAsk::Glm) => ChipText {
            label: "Runs without asking",
            tooltip: "GLM's plan mode doesn't stop its plugins from changing files, so Awili \
                       can't hold GLM's changes for approval. Use Claude or Codex to be asked \
                       first.",
            switchable: false,
            warn: true,
        },
        Effective::NoAsk(NoAsk::DeepSeek) => ChipText {
            label: "Runs without asking",
            tooltip: "Asking first isn't verified for DeepSeek yet. Use Claude or Codex to be \
                       asked first.",
            switchable: false,
            warn: true,
        },
        Effective::NoAsk(NoAsk::Build) => ChipText {
            label: "Runs on the build host",
            tooltip: "Build runs remotely; Awili can't pause it for approval.",
            switchable: false,
            warn: false,
        },
    }
}

/// The Auto confirmation body (spec D7: the floors are named). In-app engines
/// keep asking for secret files and reads outside the folder; vendor
/// harnesses have no such guarantee to promise.
pub(crate) fn auto_confirm_body(support: AskSupport) -> &'static str {
    match support {
        AskSupport::PerAction => {
            "In this conversation the agent will change files and run \
                                  commands without stopping for your OK. Secret files and reads \
                                  outside the folder still ask."
        }
        _ => {
            "In this conversation the agent will change files and run commands without \
              stopping for your OK."
        }
    }
}

/// The toast shown when switching to Auto is refused (spec D8: Auto is refused
/// for `$HOME`, `/` or no folder, and the toast says why). `None` when the
/// folder is fine.
pub(crate) fn auto_refusal(folder: Option<&str>) -> Option<String> {
    match folder.map(str::trim) {
        None | Some("") => Some("Pick a working folder before switching to Auto.".to_string()),
        Some(f) if code_root_too_broad(f) => {
            Some(format!("Auto needs a narrower folder than {f}."))
        }
        Some(_) => None,
    }
}

/// The per-conversation mode table (spec D2): an explicit per-key choice
/// wins, else the global default. Session-only — a relaunch drops it back to
/// the default, so Auto never survives a restart by accident.
#[derive(Clone, Debug, Default)]
pub(crate) struct AskModes {
    /// The global default (from `[work] default_approval_mode`, D2).
    default: ApprovalMode,
    /// Explicit per-conversation choices; an entry here wins over `default`.
    per_key: HashMap<AskKey, ApprovalMode>,
}

impl AskModes {
    /// A fresh table with `default` as the global default and no choices yet.
    pub(crate) fn new(default: ApprovalMode) -> Self {
        Self {
            default,
            per_key: HashMap::new(),
        }
    }

    /// Point the global default at `mode` (startup seeding; existing explicit
    /// choices are kept).
    pub(crate) fn set_default(&mut self, mode: ApprovalMode) {
        self.default = mode;
    }

    /// The global default.
    pub(crate) fn default_mode(&self) -> ApprovalMode {
        self.default
    }

    /// The mode in force for `key`: the explicit choice if there is one,
    /// else the default.
    pub(crate) fn mode_for(&self, key: &AskKey) -> ApprovalMode {
        self.per_key.get(key).copied().unwrap_or(self.default)
    }

    /// Record the explicit choice for `key`.
    pub(crate) fn set(&mut self, key: AskKey, mode: ApprovalMode) {
        self.per_key.insert(key, mode);
    }

    /// Drop the explicit choice for `key` (it falls back to the default).
    pub(crate) fn forget(&mut self, key: &AskKey) {
        self.per_key.remove(key);
    }

    /// Carry only an explicit user choice across two presentation backings of
    /// the same Workshop conversation. Defaults intentionally remain defaults;
    /// active plan-gate ownership is a separate, per-run concern.
    pub(crate) fn copy_explicit(&mut self, from: &AskKey, to: AskKey) {
        if let Some(mode) = self.per_key.get(from).copied() {
            self.per_key.insert(to, mode);
        }
    }
}

impl crate::AwiliApp {
    /// Native and Work backings share a conversation, while plan gates remain
    /// keyed to their active owner. Copying only an explicit Ask/Auto choice
    /// prevents an adapter handoff from silently falling back to the default.
    pub(crate) fn carry_code_ask_mode_to_work(&mut self, tab_id: u64, conv_id: &str) {
        self.work
            .ask_modes
            .copy_explicit(&AskKey::CodeTab(tab_id), AskKey::Work(conv_id.to_string()));
    }

    pub(crate) fn carry_work_ask_mode_to_code(&mut self, conv_id: &str, tab_id: u64) {
        self.work
            .ask_modes
            .copy_explicit(&AskKey::Work(conv_id.to_string()), AskKey::CodeTab(tab_id));
    }

    /// A newly accepted user message replaces any earlier plan for this one
    /// Workshop conversation, even when its previous turn used the other
    /// backing. The optional key is an already-approved follow-up: retain it
    /// until its target-aware autonomy calculation consumes its one-shot
    /// release.
    pub(crate) fn clear_superseded_conversation_plans(
        &mut self,
        conv_id: &str,
        preserve: Option<&AskKey>,
    ) {
        let mut keys = vec![AskKey::Work(conv_id.to_string())];
        keys.extend(
            self.work
                .code_tabs
                .iter()
                .filter(|tab| tab.history.conv_id.as_deref() == Some(conv_id))
                .map(|tab| AskKey::CodeTab(tab.id)),
        );
        for key in keys {
            if Some(&key) != preserve {
                self.work.plan_gates.clear(&key);
            }
        }
    }

    /// The effective posture for a Work conversation (helper for the render code,
    /// which passes the same fields it already holds): the per-conversation
    /// mode meets the executor's support.
    pub fn work_effective(
        &self,
        executor: &WorkExecutor,
        kind: EngineKind,
        conv_id: &str,
    ) -> Effective {
        let mode = self
            .work
            .ask_modes
            .mode_for(&AskKey::Work(conv_id.to_string()));
        effective(mode, work_support(executor, kind))
    }

    /// The [`AskKey`] for a Work conversation (U4d: the mode table's Work row).
    pub fn work_ask_key(&self, conv_id: &str) -> AskKey {
        AskKey::Work(conv_id.to_string())
    }

    /// The [`Autonomy`] a Work agent turn is dispatched with (U4d): the
    /// conversation's stored mode folded with the engine's Ask support. U4e1:
    /// the one follow-up an approved plan released runs `Autonomous`; any
    /// other turn that resolves to `AskFirst` arms the plan gate (see
    /// [`crate::plan_gate::next_turn`]).
    pub fn agent_autonomy_for_turn(
        &mut self,
        conv_id: &str,
        engine: CodeEngine,
        model: Option<&str>,
    ) -> Autonomy {
        let folder = self.work.folder.clone();
        self.agent_autonomy_for_accepted_turn(conv_id, engine, model, folder.as_deref())
    }

    /// Resolve posture at acceptance, before a Work memory gate can yield.
    /// `folder` is the planning folder captured with the accepted owner, not
    /// the mutable chooser folder which may change while the gate is waiting.
    pub(crate) fn agent_autonomy_for_accepted_turn(
        &mut self,
        conv_id: &str,
        engine: CodeEngine,
        model: Option<&str>,
        folder: Option<&std::path::Path>,
    ) -> Autonomy {
        let key = AskKey::Work(conv_id.to_string());
        let mode = self.work.ask_modes.mode_for(&key);
        let autonomy = crate::plan_gate::next_turn(
            &mut self.work.plan_gates,
            key.clone(),
            code_support(engine),
            mode,
            engine,
            model,
        );
        // U4e1-FIX2: a Work plan is bound to the folder its planning step runs in.
        if autonomy.plan_first() {
            let folder = folder.map(|f| f.display().to_string());
            self.work.plan_gates.bind_folder(&key, folder);
        }
        autonomy
    }

    /// The [`Autonomy`] the active Code tab's next turn is dispatched with
    /// (U4d): the tab's stored mode folded with its engine's Ask support.
    /// Without a tab there is no turn to post, so the product default
    /// (`Autonomous`) stands.
    ///
    /// U4e2: the same override and arming as the Work path, keyed by the tab
    /// id — an approved plan's follow-up turn runs `Autonomous` once, and any
    /// other turn resolves to the planning step and arms the gate (see
    /// [`crate::plan_gate::next_turn`]). Because a new message replaces an
    /// open plan, a non-follow-up turn also clears the tab's open gate first.
    pub fn code_autonomy_for_turn(&mut self) -> Autonomy {
        let active = self.work.code_active;
        let Some((tab_id, engine, model)) = self
            .work
            .code_tabs
            .get(active)
            .map(|t| (t.id, t.engine, t.model.clone()))
        else {
            return Autonomy::default();
        };
        self.code_autonomy_for_accepted_target(tab_id, engine, model.as_deref())
    }

    /// Resolve a Code turn against the target accepted for this turn. The tab
    /// is deliberately updated only after acceptance, so reading its previous
    /// engine here would bind a plan to the wrong harness/model on a switch.
    pub(crate) fn code_autonomy_for_accepted_target(
        &mut self,
        tab_id: u64,
        engine: CodeEngine,
        model: Option<&str>,
    ) -> Autonomy {
        let key = AskKey::CodeTab(tab_id);
        let mode = self.work.ask_modes.mode_for(&key);
        crate::plan_gate::next_turn(
            &mut self.work.plan_gates,
            key,
            code_support(engine),
            mode,
            engine,
            model,
        )
    }
}

/// The mode `dispatch_work_prompt` installs into the shared handle at the
/// top of a dispatch (U4d): the conversation's stored choice, or the
/// default when the conversation has none. Pure over the table, so the
/// wiring is testable without a live app.
pub(crate) fn handle_mode_for(modes: &AskModes, conv_id: &str) -> ApprovalMode {
    modes.mode_for(&AskKey::Work(conv_id.to_string()))
}

/// Install conversation `conv_id`'s mode into the shared cell the in-app
/// engine worker reads at each gate (U4d). `dispatch_work_prompt` calls
/// exactly this, so the test below exercises the production path.
pub(crate) fn install_conversation_mode(
    handle: &awili_work::SharedApprovalMode,
    modes: &AskModes,
    conv_id: &str,
) {
    handle.set(handle_mode_for(modes, conv_id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_code::Autonomy;

    /// The full support matrix (spec D6): every `WorkExecutor` variant × both
    /// `EngineKind`s, and all four `CodeEngine`s.
    #[test]
    fn u4c_support_matrix() {
        // Gateway: the in-app engine asks per call; the fleet gateway can
        // only flag risky commands.
        assert_eq!(
            work_support(&WorkExecutor::Gateway, EngineKind::Awili),
            AskSupport::PerAction
        );
        assert_eq!(
            work_support(&WorkExecutor::Gateway, EngineKind::HermesGateway),
            AskSupport::Unavailable(NoAsk::FleetGateway)
        );
        // LocalBuild: it runs remotely, whatever the engine kind.
        for kind in [EngineKind::Awili, EngineKind::HermesGateway] {
            assert_eq!(
                work_support(
                    &WorkExecutor::LocalBuild {
                        target_id: "t".into()
                    },
                    kind
                ),
                AskSupport::Unavailable(NoAsk::Build)
            );
        }
        // Agent harnesses: the engine decides, the model is irrelevant; for
        // an Agent executor the engine kind is irrelevant, so the matrix
        // asserts BOTH engine kinds for EVERY Agent engine.
        for engine in [
            CodeEngine::Claude,
            CodeEngine::Codex,
            CodeEngine::Glm,
            CodeEngine::Dsh,
        ] {
            let expected = match engine {
                CodeEngine::Claude | CodeEngine::Codex => AskSupport::PlanFirst,
                CodeEngine::Glm => AskSupport::Unavailable(NoAsk::Glm),
                CodeEngine::Dsh => AskSupport::Unavailable(NoAsk::DeepSeek),
            };
            // U4c-FIX2: every engine × kind with NO model and with a concrete
            // model (the pre-FIX1 Claude "opus" and DeepSeek "deepseek-v4"
            // cases are in here), so a model can never change the support.
            let concrete = match engine {
                CodeEngine::Claude => "opus",
                CodeEngine::Codex => "gpt-6-sol",
                CodeEngine::Glm => "glm-5",
                CodeEngine::Dsh => "deepseek-v4",
            };
            for kind in [EngineKind::Awili, EngineKind::HermesGateway] {
                for model in [None, Some(concrete.to_string())] {
                    assert_eq!(
                        work_support(
                            &WorkExecutor::Agent {
                                engine,
                                model: model.clone()
                            },
                            kind
                        ),
                        expected,
                        "engine {engine:?} kind {kind:?} model {model:?}"
                    );
                }
            }
        }
        // The four `CodeEngine`s on their own.
        assert_eq!(code_support(CodeEngine::Claude), AskSupport::PlanFirst);
        assert_eq!(code_support(CodeEngine::Codex), AskSupport::PlanFirst);
        assert_eq!(
            code_support(CodeEngine::Glm),
            AskSupport::Unavailable(NoAsk::Glm)
        );
        assert_eq!(
            code_support(CodeEngine::Dsh),
            AskSupport::Unavailable(NoAsk::DeepSeek)
        );
    }

    /// For every `NoAsk`, both modes collapse to `NoAsk`, and the chip is
    /// not switchable (spec D6: Awili never shows a mode it cannot enforce).
    #[test]
    fn u4c_unavailable_never_offers_ask() {
        let cases = [
            (
                NoAsk::FleetGateway,
                AskSupport::Unavailable(NoAsk::FleetGateway),
            ),
            (NoAsk::Glm, AskSupport::Unavailable(NoAsk::Glm)),
            (NoAsk::DeepSeek, AskSupport::Unavailable(NoAsk::DeepSeek)),
            (NoAsk::Build, AskSupport::Unavailable(NoAsk::Build)),
        ];
        for (why, support) in cases {
            assert_eq!(
                effective(ApprovalMode::AskBeforeChanges, support),
                Effective::NoAsk(why),
                "{why:?} must not offer Ask"
            );
            assert_eq!(
                effective(ApprovalMode::Auto, support),
                Effective::NoAsk(why),
                "{why:?} must not offer Auto"
            );
            assert!(
                !chip_text(Effective::NoAsk(why)).switchable,
                "{why:?} chip must not be clickable"
            );
        }
    }

    /// `autonomy_for` per `Effective` (spec D5: only the Ask planning step is
    /// read-only; `AskPerAction` never reaches a vendor CLI).
    #[test]
    fn u4c_ask_maps_to_plan_step_only_for_cli() {
        assert_eq!(autonomy_for(Effective::AskPlanFirst), Autonomy::AskFirst);
        assert_eq!(autonomy_for(Effective::AskPerAction), Autonomy::Autonomous);
        assert_eq!(autonomy_for(Effective::Auto), Autonomy::Autonomous);
        for why in [
            NoAsk::FleetGateway,
            NoAsk::Glm,
            NoAsk::DeepSeek,
            NoAsk::Build,
        ] {
            assert_eq!(
                autonomy_for(Effective::NoAsk(why)),
                Autonomy::Autonomous,
                "{why:?} runs at full access"
            );
        }
    }

    /// No chip label or tooltip may carry the old jargon (spec outcome 5):
    /// "ADVISORY", "advisory", "Ask-first", "headless" or "harness".
    #[test]
    fn u4c_chip_copy_has_no_jargon() {
        let effs = [
            Effective::AskPerAction,
            Effective::AskPlanFirst,
            Effective::Auto,
            Effective::NoAsk(NoAsk::FleetGateway),
            Effective::NoAsk(NoAsk::Glm),
            Effective::NoAsk(NoAsk::DeepSeek),
            Effective::NoAsk(NoAsk::Build),
        ];
        for eff in effs {
            let text = chip_text(eff);
            for word in ["ADVISORY", "advisory", "Ask-first", "headless", "harness"] {
                assert!(
                    !text.label.contains(word) && !text.tooltip.contains(word),
                    "{eff:?}: {word:?} must not appear in the chip copy"
                );
            }
        }
    }

    /// The mode table: default Ask; `set_default(Auto)`; an explicit choice
    /// wins; `forget` falls back (spec D2).
    #[test]
    fn u4c_modes_default_and_override() {
        let mut modes = AskModes::new(ApprovalMode::AskBeforeChanges);
        let key = AskKey::Work("c1".to_string());
        assert_eq!(modes.default_mode(), ApprovalMode::AskBeforeChanges);
        assert_eq!(modes.mode_for(&key), ApprovalMode::AskBeforeChanges);

        modes.set_default(ApprovalMode::Auto);
        assert_eq!(modes.default_mode(), ApprovalMode::Auto);
        assert_eq!(modes.mode_for(&key), ApprovalMode::Auto);

        // An explicit choice beats the default.
        modes.set(key.clone(), ApprovalMode::AskBeforeChanges);
        assert_eq!(modes.mode_for(&key), ApprovalMode::AskBeforeChanges);
        let other = AskKey::CodeTab(7);
        assert_eq!(modes.mode_for(&other), ApprovalMode::Auto);

        // `forget` drops the explicit choice back to the default.
        modes.forget(&key);
        assert_eq!(modes.mode_for(&key), ApprovalMode::Auto);

        // The Code-tab key is independent of the Work key.
        modes.set(AskKey::CodeTab(7), ApprovalMode::AskBeforeChanges);
        assert_eq!(modes.mode_for(&other), ApprovalMode::AskBeforeChanges);
    }

    #[test]
    fn explicit_mode_survives_a_work_native_backing_handoff() {
        let mut modes = AskModes::new(ApprovalMode::AskBeforeChanges);
        let work = AskKey::Work("same-conversation".into());
        let code = AskKey::CodeTab(42);
        modes.set(work.clone(), ApprovalMode::Auto);
        modes.copy_explicit(&work, code.clone());
        assert_eq!(modes.mode_for(&code), ApprovalMode::Auto);

        modes.set(code.clone(), ApprovalMode::AskBeforeChanges);
        modes.copy_explicit(&code, work.clone());
        assert_eq!(modes.mode_for(&work), ApprovalMode::AskBeforeChanges);

        let unrelated = AskKey::CodeTab(99);
        modes.copy_explicit(&unrelated, AskKey::Work("other".into()));
        assert_eq!(
            modes.mode_for(&AskKey::Work("other".into())),
            ApprovalMode::AskBeforeChanges
        );
    }

    /// `auto_refusal`: no folder, `/`, and `$HOME` are refused with a toast;
    /// a narrow folder is allowed (spec D8, via `code_root_too_broad`).
    #[test]
    fn u4c_auto_refused_for_broad_or_missing_folder() {
        assert_eq!(
            auto_refusal(None),
            Some("Pick a working folder before switching to Auto.".to_string())
        );
        let home = std::env::var("HOME").expect("HOME is set on the runner");
        for broad in ["/", home.as_str()] {
            let refusal = auto_refusal(Some(broad));
            assert!(refusal.is_some(), "{broad:?} must be refused");
            assert!(
                refusal
                    .unwrap()
                    .starts_with("Auto needs a narrower folder than"),
                "{broad:?} refusal must name the folder"
            );
        }
        assert_eq!(auto_refusal(Some("/tmp/project")), None);
    }

    /// U4c-FIX1 (P2): an empty or whitespace-only folder counts as "no
    /// folder" — the refusal is identical to picking no folder at all, and a
    /// narrow folder is still allowed.
    #[test]
    fn u4c_fix1_auto_refused_for_empty_folder() {
        assert_eq!(auto_refusal(Some("")), auto_refusal(None));
        assert_eq!(auto_refusal(Some("   ")), auto_refusal(None));
        assert!(auto_refusal(Some("/Users/j/src/p")).is_none());
    }

    /// U4d: the advisory banner strings are gone from the app and the
    /// awili-code crate.
    #[test]
    fn u4d_advisory_banner_strings_are_gone() {
        let main_src = include_str!("main.rs");
        let work_src = include_str!("work.rs");
        let code_src = include_str!("../../awili-code/src/lib.rs");
        // The needle strings are assembled from per-word `char` vectors at
        // runtime so the full literal never appears in this file's own source
        // (the test would otherwise match its own `include_str!` of itself).
        let adv: String = ['A', 'D', 'V', 'I', 'S', 'O', 'R', 'Y']
            .iter()
            .copied()
            .collect();
        let ask_is: String = ['A', 's', 'k', '-', 'f', 'i', 'r', 's', 't', ' ', 'i', 's']
            .iter()
            .copied()
            .collect();
        let ask_adv: String = [
            'A', 's', 'k', ' ', '(', 'a', 'd', 'v', 'i', 's', 'o', 'r', 'y', ')',
        ]
        .iter()
        .copied()
        .collect();
        let headless: String = [
            'h', 'e', 'a', 'd', 'l', 'e', 's', 's', ' ', 'h', 'a', 'r', 'n', 'e', 's', 's', ' ',
            'h', 'a', 's', ' ', 'n', 'o', ' ', 'i', 'n', 't', 'e', 'r', 'a', 'c', 't', 'i', 'v',
            'e', ' ', 'a', 'p', 'p', 'r', 'o', 'v', 'a', 'l', ' ', 'p', 'r', 'o', 'm', 'p', 't',
        ]
        .iter()
        .copied()
        .collect();
        let needles = [&adv, &ask_is, &ask_adv, &headless];
        for (name, src) in [
            ("main.rs", main_src),
            ("work.rs", work_src),
            ("awili-code/src/lib.rs", code_src),
        ] {
            for needle in &needles {
                assert!(
                    !src.contains(needle.as_str()),
                    "{name} must not contain {needle:?} after U4d"
                );
            }
        }
    }

    /// U4d: the shared handle the in-app engine worker reads at each gate
    /// follows the conversation's stored choice. A `dispatch_work_prompt`
    /// dispatch for conversation `a` (stored Auto) installs Auto; a later
    /// dispatch for `b` (no stored choice, default Ask) installs Ask.
    #[test]
    fn u4d_owned_engine_handle_follows_the_conversation() {
        let mut modes = AskModes::new(ApprovalMode::AskBeforeChanges);
        modes.set(AskKey::Work("a".to_string()), ApprovalMode::Auto);
        // `a` has a stored Auto; `b` does not (falls back to the default Ask).
        assert_eq!(
            handle_mode_for(&modes, "a"),
            ApprovalMode::Auto,
            "conversation `a` is stored Auto"
        );
        assert_eq!(
            handle_mode_for(&modes, "b"),
            ApprovalMode::AskBeforeChanges,
            "conversation `b` has no stored choice, default Ask"
        );
    }

    /// U4d-FIX1: the worker holds a CLONE of the app's handle (what
    /// `build_awili_engine` passes). Installing through the same function
    /// `dispatch_work_prompt` calls must switch what that clone reads, in
    /// both directions, across conversations.
    #[test]
    fn u4d_fix_dispatch_install_switches_the_worker_handle() {
        let mut modes = AskModes::new(ApprovalMode::AskBeforeChanges);
        modes.set(AskKey::Work("a".to_string()), ApprovalMode::Auto);
        let app_handle = awili_work::SharedApprovalMode::new(ApprovalMode::AskBeforeChanges);
        let worker_view = app_handle.clone();

        install_conversation_mode(&app_handle, &modes, "a");
        assert_eq!(
            worker_view.clone().get(),
            ApprovalMode::Auto,
            "dispatching `a` installs Auto"
        );
        install_conversation_mode(&app_handle, &modes, "b");
        assert_eq!(
            worker_view.clone().get(),
            ApprovalMode::AskBeforeChanges,
            "dispatching `b` (no stored choice) re-installs Ask"
        );
        install_conversation_mode(&app_handle, &modes, "a");
        assert_eq!(
            worker_view.get(),
            ApprovalMode::Auto,
            "back to `a` installs Auto again"
        );

        // The production dispatch really goes through this function.
        let work_src = include_str!("work.rs");
        let call = concat!("crate::ask_mode::install_conversation_mode(", "\n");
        assert!(
            work_src.contains(call),
            "dispatch_work_prompt must install the mode via install_conversation_mode"
        );
        let old = concat!(".set(crate::ask_mode::", "handle_mode_for(");
        assert!(
            !work_src.contains(old),
            "the inline handle set must be gone"
        );
    }

    /// U4e1-FIX2 (integration fix, S5(a) × U4): a gated prompt handed to an
    /// ALREADY-RUNNING worker installs ITS conversation's mode first, so the
    /// worker's shared handle reads the right mode for each conversation; a
    /// refused gate changes nothing (no prompt, and the handle keeps the
    /// previous conversation's mode).
    #[test]
    fn u4e1_fix2_live_worker_hand_off_installs_the_conversation_mode() {
        use crate::work::{
            hand_off_gated_work_prompt, GatedWorkPrompt, GatewayCfg, GatewayCmd, HermesCfg,
            WorkState,
        };
        let (tx, rx) = std::sync::mpsc::channel::<GatewayCmd>();
        let mut work = WorkState::new(
            HermesCfg {
                base_url: String::new(),
                api_key: String::new(),
                enabled: false,
                dashboard_url: String::new(),
            },
            GatewayCfg {
                dashboard_url: String::new(),
                enabled: false,
            },
            Default::default(),
        );
        work.gateway_cmd_tx = Some(tx); // the live worker
        work.ask_modes
            .set(AskKey::Work("auto-conv".into()), ApprovalMode::Auto);
        // "ask-conv" keeps the default (Ask).
        let worker_view = work.approval_handle.clone();

        for (conv, want) in [
            ("auto-conv", ApprovalMode::Auto),
            ("ask-conv", ApprovalMode::AskBeforeChanges),
            ("auto-conv", ApprovalMode::Auto),
        ] {
            // RC1-I2 (amended): an in-app worker belongs to one conversation,
            // so the live worker here is that conversation's.
            work.worker_conv = Some(conv.into());
            let r = hand_off_gated_work_prompt(&mut work, conv, "task", Ok(None));
            assert!(matches!(r, GatedWorkPrompt::Offered), "{conv}: offered");
            assert!(
                matches!(rx.try_recv(), Ok(GatewayCmd::Prompt { .. })),
                "{conv}: the live worker got the prompt"
            );
            assert_eq!(worker_view.get(), want, "{conv}: the worker reads its mode");
        }

        // A refused gate from the Ask conversation: nothing sent, mode kept.
        work.worker_conv = Some("ask-conv".into()); // RC1-I2 (amended)
        let r = hand_off_gated_work_prompt(&mut work, "ask-conv", "task", Err("rejected".into()));
        assert!(matches!(r, GatedWorkPrompt::Refused(_)));
        assert!(rx.try_recv().is_err(), "nothing reached the worker");
        assert_eq!(
            worker_view.get(),
            ApprovalMode::Auto,
            "a refused gate leaves the handle unchanged"
        );

        // The production closure hands off through this function.
        let src = include_str!("work.rs");
        let call = concat!(
            "match hand_off_gated_work_",
            "prompt(&mut app.work, &conv_id2,"
        );
        assert!(src.contains(call), "the S5 closure uses the hand-off");
    }
}
