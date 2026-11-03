//! L1d — what the user reads about the Work executor (S2-F06 / S3-F06:
//! nobody could tell which executor was in charge or where its tools ran):
//! the chip label, the picker's sections, and the Work header's "Where:" and
//! "Next run:" lines under the existing "Requested:" line.
//!
//! Pure functions plus two thin `AwiliApp` readers the chip and the header
//! call. Nothing here claims where a vendor CLI runs its model: an agent's
//! "Where:" line names only the CLI on this Mac.

use std::collections::BTreeMap;

use awili_config::{BuildTargetConfig, EngineConfig, ExtraEngineConfig, ModelsConfig};

use crate::work::{work_executor_agent_rows, WorkExecutor};
use crate::work_run_identity::WorkRunIdentity;

/// Picker section headers, in order.
pub(crate) const THIS_MAC_HEADER: &str = "ON THIS MAC";
pub(crate) const CLOUD_HEADER: &str = "CLOUD MODELS \u{00B7} CLI ON THIS MAC";
pub(crate) const FLEET_HEADER: &str = "FLEET \u{00B7} REMOTE";
pub(crate) const BUILD_HEADER: &str = "BUILD";

/// The fleet gateway's picker row.
pub(crate) const FLEET_ROW: &str = "Fleet gateway \u{2014} can't see this Mac's folders";

/// The executor chip's text (the caller appends " ▾"): `InApp` →
/// "In-app · {display_label}", `Gateway` → "Fleet gateway", everything
/// else → `executor.label_with(targets)` ("Claude · sonnet", "Build · …").
pub(crate) fn chip_label(
    executor: &WorkExecutor,
    engine: &EngineConfig,
    extras: &[ExtraEngineConfig],
    targets: &BTreeMap<String, BuildTargetConfig>,
) -> String {
    match executor {
        WorkExecutor::InApp => format!("In-app \u{00B7} {}", engine.display_label()),
        // An extra shows its own label; one that is no longer configured says
        // so rather than borrowing the primary's name.
        WorkExecutor::InAppExtra { id } => match extras.iter().find(|e| &e.id == id) {
            Some(e) => format!("In-app \u{00B7} {}", e.to_engine_config().display_label()),
            None => format!("In-app \u{00B7} {id} (not set up)"),
        },
        WorkExecutor::Gateway => "Fleet gateway".to_string(),
        WorkExecutor::Agent { .. } | WorkExecutor::LocalBuild { .. } => {
            executor.label_with(targets)
        }
    }
}

/// The in-app executors, in picker order: the primary, then each
/// `[[engines]]` extra in config order.
pub(crate) fn in_app_executors(extras: &[ExtraEngineConfig]) -> Vec<WorkExecutor> {
    std::iter::once(WorkExecutor::InApp)
        .chain(
            extras
                .iter()
                .map(|e| WorkExecutor::InAppExtra { id: e.id.clone() }),
        )
        .collect()
}

/// One picker section: its header and its `(label, executor)` rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PickerSection {
    pub(crate) header: &'static str,
    pub(crate) rows: Vec<(String, WorkExecutor)>,
}

/// The picker, top to bottom: ON THIS MAC (the in-app row), CLOUD MODELS
/// (the agent CLI rows, `work_executor_agent_rows`), FLEET · REMOTE (the
/// gateway row, which says it can't see this Mac's folders), and BUILD (one
/// row per configured target; absent when there is none).
pub(crate) fn picker_sections(
    engine: &EngineConfig,
    extras: &[ExtraEngineConfig],
    models: &ModelsConfig,
    targets: &BTreeMap<String, BuildTargetConfig>,
) -> Vec<PickerSection> {
    let mut sections = vec![
        PickerSection {
            header: THIS_MAC_HEADER,
            rows: in_app_executors(extras)
                .into_iter()
                .map(|executor| (chip_label(&executor, engine, extras, targets), executor))
                .collect(),
        },
        PickerSection {
            header: CLOUD_HEADER,
            rows: work_executor_agent_rows(models),
        },
        PickerSection {
            header: FLEET_HEADER,
            rows: vec![(FLEET_ROW.to_string(), WorkExecutor::Gateway)],
        },
    ];
    if !targets.is_empty() {
        sections.push(PickerSection {
            header: BUILD_HEADER,
            rows: targets
                .iter()
                .map(|(id, target)| {
                    (
                        target.label(),
                        WorkExecutor::LocalBuild {
                            target_id: id.clone(),
                        },
                    )
                })
                .collect(),
        });
    }
    sections
}

/// One target the Workshop chooser offers for a conversation's next turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetRow {
    pub(crate) label: String,
    pub(crate) target: WorkExecutor,
    /// The setup or capability that is missing, when the target cannot run now.
    pub(crate) unavailable: Option<String>,
}

/// A group of chooser rows (in-app, harnesses and their models, fleet).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TargetSection {
    pub(crate) header: &'static str,
    pub(crate) rows: Vec<TargetRow>,
}

/// What the chooser needs to know about what this Mac can reach.
pub(crate) struct TargetReach<'a> {
    pub(crate) engine: &'a EngineConfig,
    pub(crate) extras: &'a [ExtraEngineConfig],
    pub(crate) models: &'a ModelsConfig,
    pub(crate) agents: &'a awili_config::AgentBinsConfig,
    pub(crate) build_targets: &'a BTreeMap<String, BuildTargetConfig>,
    pub(crate) gateway_set_up: bool,
}

/// Every target a conversation can use next, in the picker's order: the in-app
/// model, each native harness with its default and configured models, and the
/// fleet gateway, then one row per configured Build target (a one-shot lane,
/// never invented). Nothing is hidden. A target that cannot run says which
/// setup is missing.
pub(crate) fn target_sections(reach: &TargetReach<'_>) -> Vec<TargetSection> {
    let none = BTreeMap::new();
    let missing_msg = "The in-app model isn't set up. Add its address and model in Settings.";
    let in_app_missing = (reach.engine.kind == awili_config::EngineKind::Awili
        && (reach.engine.base_url.trim().is_empty() || reach.engine.model.trim().is_empty()))
    .then(|| missing_msg.to_string());
    let mut harness_rows = Vec::new();
    for (label, executor) in work_executor_agent_rows(reach.models) {
        let unavailable = match &executor {
            WorkExecutor::Agent { engine, .. } => {
                crate::harness_setup::installed_binary(*engine, reach.agents)
                    .is_none()
                    .then(|| {
                        format!(
                            "{} isn't installed on this Mac. Awili looks for the `{}` command.",
                            engine.label(),
                            engine.bin()
                        )
                    })
            }
            _ => None,
        };
        harness_rows.push(TargetRow {
            label,
            target: executor,
            unavailable,
        });
    }
    let mut sections = vec![
        TargetSection {
            header: THIS_MAC_HEADER,
            rows: in_app_executors(reach.extras)
                .into_iter()
                .map(|target| {
                    let unavailable = match &target {
                        WorkExecutor::InAppExtra { id } => reach
                            .extras
                            .iter()
                            .find(|e| &e.id == id)
                            .is_none_or(|e| !e.to_engine_config().is_configured())
                            .then(|| missing_msg.to_string()),
                        _ => in_app_missing.clone(),
                    };
                    TargetRow {
                        label: chip_label(&target, reach.engine, reach.extras, &none),
                        target,
                        unavailable,
                    }
                })
                .collect(),
        },
        TargetSection {
            header: CLOUD_HEADER,
            rows: harness_rows,
        },
        TargetSection {
            header: FLEET_HEADER,
            rows: vec![TargetRow {
                label: FLEET_ROW.to_string(),
                target: WorkExecutor::Gateway,
                unavailable: (!reach.gateway_set_up).then(|| {
                    "The fleet gateway isn't set up. Add its address in Settings.".to_string()
                }),
            }],
        },
    ];
    if !reach.build_targets.is_empty() {
        sections.push(TargetSection {
            header: BUILD_HEADER,
            rows: reach
                .build_targets
                .iter()
                .map(|(id, target)| TargetRow {
                    label: target.label(),
                    target: WorkExecutor::LocalBuild {
                        target_id: id.clone(),
                    },
                    unavailable: None,
                })
                .collect(),
        });
    }
    sections
}

/// A target in a few words for the composer chip: harness, then the configured
/// model name as it is ("Claude · sonnet"), "In-app", "Gateway". The raw name
/// keeps two variants of one family distinguishable.
pub(crate) fn short_target(
    executor: &WorkExecutor,
    extras: &[ExtraEngineConfig],
    targets: &BTreeMap<String, BuildTargetConfig>,
) -> String {
    match executor {
        WorkExecutor::InApp => "In-app".to_string(),
        WorkExecutor::InAppExtra { id } => match extras.iter().find(|e| &e.id == id) {
            Some(e) => format!("In-app \u{00B7} {}", e.to_engine_config().display_label()),
            None => format!("In-app \u{00B7} {id}"),
        },
        WorkExecutor::Gateway => "Gateway".to_string(),
        WorkExecutor::Agent {
            engine,
            model: None,
        } => engine.label().to_string(),
        WorkExecutor::Agent {
            engine,
            model: Some(model),
        } => format!("{} \u{00B7} {model}", engine.label()),
        WorkExecutor::LocalBuild { .. } => executor.label_with(targets),
    }
}

/// The composer chip's text and tooltip. `next` is the conversation's saved
/// next-turn target (`None`: unavailable); `active` is the target of a turn
/// running now. The chip shows the next target; only while a turn runs under a
/// different one does it say so, and the tooltip names both.
pub(crate) struct TargetChip {
    pub(crate) label: String,
    pub(crate) tooltip: String,
}

pub(crate) fn target_chip(
    next: Option<&WorkExecutor>,
    active: Option<&WorkExecutor>,
    engine: &EngineConfig,
    extras: &[ExtraEngineConfig],
    targets: &BTreeMap<String, BuildTargetConfig>,
) -> TargetChip {
    let Some(next) = next else {
        return TargetChip {
            label: "Choose model".to_string(),
            tooltip: "This conversation's saved model or helper isn't available. Choose one for your next message."
                .to_string(),
        };
    };
    let full = chip_label(next, engine, extras, targets);
    match active.filter(|active| *active != next) {
        Some(active) => TargetChip {
            label: format!("Next: {}", short_target(next, extras, targets)),
            tooltip: format!(
                "Running now: {}. Your next message goes to {full}.",
                chip_label(active, engine, extras, targets)
            ),
        },
        None => TargetChip {
            label: short_target(next, extras, targets),
            tooltip: format!("{full} \u{2014} used for your next message"),
        },
    }
}

/// Where the shown run's tools ran: the owned engine → "Where: Awili on
/// this Mac · model at {host}" (no host → just "Where: Awili on this Mac");
/// an agent → "Where: {Engine} CLI on this Mac"; the gateway → "Where: fleet
/// gateway (remote) · can't see this Mac's folders"; a Build → "Where:
/// build runner on {host}".
pub(crate) fn where_line(identity: &WorkRunIdentity, engine: &EngineConfig) -> String {
    match identity {
        WorkRunIdentity::OwnedEngine { .. } => match engine.endpoint_host() {
            Some(host) => format!("Where: Awili on this Mac \u{00B7} model at {host}"),
            None => "Where: Awili on this Mac".to_string(),
        },
        WorkRunIdentity::Agent { engine, .. } => {
            format!("Where: {} CLI on this Mac", engine.label())
        }
        WorkRunIdentity::HermesGateway => {
            "Where: fleet gateway (remote) \u{00B7} can't see this Mac's folders".to_string()
        }
        WorkRunIdentity::LocalBuild { host, .. } => format!("Where: build runner on {host}"),
    }
}

/// Whether the shown run used the lane the selected executor would use:
/// owned engine ↔ InApp, gateway ↔ Gateway, Build ↔ LocalBuild, and an
/// agent only with the same engine AND model.
pub(crate) fn same_lane(identity: &WorkRunIdentity, executor: &WorkExecutor) -> bool {
    match (identity, executor) {
        (WorkRunIdentity::OwnedEngine { engine_id, .. }, WorkExecutor::InApp) => {
            engine_id.is_none()
        }
        (WorkRunIdentity::OwnedEngine { engine_id, .. }, WorkExecutor::InAppExtra { id }) => {
            engine_id.as_deref() == Some(id.as_str())
        }
        (WorkRunIdentity::HermesGateway, WorkExecutor::Gateway)
        | (WorkRunIdentity::LocalBuild { .. }, WorkExecutor::LocalBuild { .. }) => true,
        (
            WorkRunIdentity::Agent { engine, model },
            WorkExecutor::Agent {
                engine: e,
                model: m,
            },
        ) => engine == e && model == m,
        _ => false,
    }
}

/// RC1-O (round 5 Major 4): the executor that continues a conversation
/// whose last run had `identity` — the same lane, so a follow-up typed in an
/// old conversation goes where its last run went. A local Build is a
/// one-shot override, never restored (`None`).
pub(crate) fn executor_for(identity: &WorkRunIdentity) -> Option<WorkExecutor> {
    match identity {
        // The run records which engine it ran on (its [[engines]] id), never
        // guessed from the model name; no id = the primary.
        WorkRunIdentity::OwnedEngine { engine_id, .. } => Some(match engine_id {
            Some(id) => WorkExecutor::InAppExtra { id: id.clone() },
            None => WorkExecutor::InApp,
        }),
        WorkRunIdentity::HermesGateway => Some(WorkExecutor::Gateway),
        WorkRunIdentity::Agent { engine, model } => Some(WorkExecutor::Agent {
            engine: *engine,
            model: model.clone(),
        }),
        WorkRunIdentity::LocalBuild { .. } => None,
    }
}

/// The engine a run used, found by the id its identity carries (`None` = the
/// primary). A removed extra gives a blank engine, never the primary.
pub(crate) fn run_engine(
    identity: &WorkRunIdentity,
    primary: &EngineConfig,
    extras: &[ExtraEngineConfig],
) -> EngineConfig {
    match identity {
        WorkRunIdentity::OwnedEngine {
            engine_id: Some(id),
            ..
        } => extras
            .iter()
            .find(|e| &e.id == id)
            .map(ExtraEngineConfig::to_engine_config)
            .unwrap_or_default(),
        _ => primary.clone(),
    }
}

/// The header lines under "Requested:": none without a shown run;
/// otherwise its "Where:" line, plus "Next run: {chip label}" when nothing
/// is running and the selected executor is not the lane the shown run used.
pub(crate) fn run_lines(
    identity: Option<&WorkRunIdentity>,
    executor: &WorkExecutor,
    engine: &EngineConfig,
    extras: &[ExtraEngineConfig],
    targets: &BTreeMap<String, BuildTargetConfig>,
    running: bool,
) -> Vec<String> {
    let Some(identity) = identity else {
        return Vec::new();
    };
    // "Where" is the engine the run used (by id), not the next executor's.
    let ran_on = run_engine(identity, engine, extras);
    let mut lines = vec![where_line(identity, &ran_on)];
    if !running && !same_lane(identity, executor) {
        lines.push(format!(
            "Next run: {}",
            chip_label(executor, engine, extras, targets)
        ));
    }
    lines
}

impl crate::AwiliApp {
    /// The executor chip's label (`chips_row`); the Workshop composer reads
    /// [`target_chip`] instead, so only tests use this now.
    #[cfg(test)]
    pub(crate) fn work_executor_chip_label(&self) -> String {
        chip_label(
            &self.shown_work_executor(),
            &self.work.engine,
            &self.work.extra_engines,
            &self.work.build_targets,
        )
    }

    /// RC2-R1G2 (RC2-R1F review P2): the helper the chip shows. While a run
    /// of ANOTHER conversation is live (or its Stop is pending), the chip of
    /// the conversation on screen shows that conversation's own helper (a
    /// "Continue in Work" one shows the helper that answered in Chat); the
    /// live run keeps `work.executor`, by which its approvals are routed,
    /// and the shown helper takes over when the run ends
    /// (`settle_work_chip`). Otherwise the chip is `work.executor`.
    #[cfg(test)]
    pub(crate) fn shown_work_executor(&self) -> WorkExecutor {
        let active = self.sidebar.active_id();
        let live = self.work.running || self.work.gateway_stop_pending;
        if live && self.work.conv_id.as_deref() != Some(active) {
            if let Some(helper) =
                crate::work_last_run::conversation_helper(&self.work, active, &self.work_last_run)
            {
                return helper;
            }
        }
        self.work.executor.clone()
    }

    /// The Work header's provenance lines (`work_header`): empty when the
    /// panel does not own the shown run; otherwise the run's identity lines
    /// ("Requested: …", "Backend-reported model: …") plus [`run_lines`].
    pub(crate) fn work_provenance_lines(&self, owns_run: bool) -> Vec<String> {
        if !owns_run {
            return Vec::new();
        }
        let panel = &self.work.panel;
        let mut lines = panel.identity_lines();
        // RC1-I3: the in-app engine's model is named, never its file path:
        // the configured display label for this engine, else the path's last
        // part.
        if let Some(crate::work_run_identity::WorkRunIdentity::OwnedEngine { model, .. }) =
            panel.requested_identity.as_ref()
        {
            if model.contains('/') {
                let eff = run_engine(
                    panel.requested_identity.as_ref().unwrap(),
                    &self.work.engine,
                    &self.work.extra_engines,
                );
                let shown = if *model == eff.model {
                    eff.display_label()
                } else {
                    model.rsplit('/').next().unwrap_or(model).to_string()
                };
                for line in &mut lines {
                    *line = line.replace(model.as_str(), &shown);
                }
            }
        }
        lines.extend(run_lines(
            panel.requested_identity.as_ref(),
            &self.work.executor,
            &self.work.engine,
            &self.work.extra_engines,
            &self.work.build_targets,
            self.work.running,
        ));
        lines
    }
}

// ---- RC2-CNCL7: the council picker --------------------------------------------

/// Council picker group headers, in order.
pub(crate) const COUNCIL_LOCAL: &str = "On your Macs \u{00B7} free";
pub(crate) const COUNCIL_SUBS: &str = "Your subscriptions";
pub(crate) const COUNCIL_PAYG: &str = "Pay as you go";
pub(crate) const COUNCIL_FLEET: &str = "Fleet";
pub(crate) const COUNCIL_OTHER: &str = "Other models you set up";

/// One model the council picker offers. `offline` says why it cannot be
/// picked right now (it is shown, never hidden).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CouncilPick {
    pub(crate) route: String,
    pub(crate) label: String,
    pub(crate) note: String,
    pub(crate) cost: String,
    pub(crate) offline: Option<String>,
}

/// What the picker needs to know about what Awili can reach.
pub(crate) struct CouncilReach<'a> {
    pub(crate) engine: &'a EngineConfig,
    /// The `[[engines]]` extras, each offered next to the primary in-app model.
    pub(crate) extras: &'a [ExtraEngineConfig],
    pub(crate) models: &'a ModelsConfig,
    pub(crate) agents: &'a awili_config::AgentBinsConfig,
    pub(crate) fleet: &'a crate::agent_chat::ChatRoute,
    /// The model on this Mac did not answer its last check.
    pub(crate) local_down: bool,
    /// Every chat model from `[[providers]]` (declared and discovered).
    pub(crate) choices: &'a [awili_config::ModelChoice],
    /// Provider reachability by `base_url` (absent = not checked yet).
    pub(crate) provider_health: &'a std::collections::HashMap<String, bool>,
}

/// The route of a `[[providers]]` model on a council.
pub(crate) fn model_route(provider: &str, model: &str) -> String {
    format!("model:{provider}:{model}")
}

/// Every model the council can seat, grouped by where it runs and what it
/// costs. Nothing is hidden: a model that cannot answer right now is listed
/// with the reason.
pub(crate) fn council_pick_groups(r: &CouncilReach<'_>) -> Vec<(&'static str, Vec<CouncilPick>)> {
    use awili_code::CodeEngine;
    let label = |route: &str| crate::council_view::route_label(route, r.engine, r.extras);
    let not_installed = |e: CodeEngine| {
        crate::harness_setup::installed_binary(e, r.agents)
            .is_none()
            .then(|| "not installed on this Mac".to_string())
    };
    let pick = |route: String, note: &str, cost: &str, offline: Option<String>| CouncilPick {
        label: label(&route),
        route,
        note: note.to_string(),
        cost: cost.to_string(),
        offline,
    };
    let in_app_offline = if r.engine.base_url.trim().is_empty() || r.engine.model.trim().is_empty()
    {
        Some("not set up".to_string())
    } else if r.local_down {
        Some("not running".to_string())
    } else {
        None
    };
    let mut local = vec![pick(
        "in-app".to_string(),
        &match r.engine.endpoint_host() {
            Some(h) => format!("{h} \u{00B7} the model Work uses on this Mac"),
            None => "the model Work uses on this Mac".to_string(),
        },
        "free",
        in_app_offline,
    )];
    for e in r.extras {
        let eng = e.to_engine_config();
        let offline = (!eng.is_configured()).then(|| "not set up".to_string());
        local.push(pick(
            format!("{}{}", crate::council_run::IN_APP_EXTRA_PREFIX, e.id),
            &match eng.endpoint_host() {
                Some(h) => format!("{h} \u{00B7} an extra model Work can use"),
                None => "an extra model Work can use".to_string(),
            },
            "free",
            offline,
        ));
    }
    let mut subs = Vec::new();
    for alias in crate::work::agent_model_options(CodeEngine::Claude, r.models) {
        subs.push(pick(
            format!("agent:claude:{alias}"),
            "Claude subscription",
            "included",
            not_installed(CodeEngine::Claude),
        ));
    }
    subs.push(pick(
        "agent:codex".to_string(),
        "ChatGPT subscription",
        "included",
        not_installed(CodeEngine::Codex),
    ));
    for m in crate::work::agent_model_options(CodeEngine::Codex, r.models) {
        subs.push(pick(
            format!("agent:codex:{m}"),
            "ChatGPT subscription",
            "included",
            not_installed(CodeEngine::Codex),
        ));
    }
    let mut payg = Vec::new();
    let mut dsh = crate::work::agent_model_options(CodeEngine::Dsh, r.models);
    if dsh.is_empty() {
        dsh.push("deepseek-flash".to_string());
    }
    for m in dsh {
        payg.push(pick(
            format!("agent:dsh:{m}"),
            "DeepSeek \u{00B7} shows its answer all at once",
            "pay as you go",
            not_installed(CodeEngine::Dsh),
        ));
    }
    payg.push(pick(
        "agent:glm".to_string(),
        "ZCode \u{00B7} shows its answer all at once",
        "billed by Z.ai",
        not_installed(CodeEngine::Glm),
    ));
    let fleet_offline = match r.fleet {
        crate::agent_chat::ChatRoute::Fleet(_) => None,
        _ => Some("not set up".to_string()),
    };
    let fleet = vec![pick(
        crate::agent_chat::FLEET_CHAT_ROUTE.to_string(),
        "your own model at home \u{00B7} nothing leaves the house",
        "free",
        fleet_offline,
    )];
    let other: Vec<CouncilPick> = r
        .choices
        .iter()
        .map(|c| CouncilPick {
            route: model_route(&c.provider, &c.model),
            label: c.model.clone(),
            note: c.provider.clone(),
            cost: if awili_core::pricing::price_for(&c.provider, &c.model)
                == awili_core::pricing::price_for("ollama", "free")
            {
                "free".to_string()
            } else {
                "pay as you go".to_string()
            },
            offline: (r.provider_health.get(&c.base_url) == Some(&false))
                .then(|| "not answering".to_string()),
        })
        .collect();
    let mut groups = vec![
        (COUNCIL_LOCAL, local),
        (COUNCIL_SUBS, subs),
        (COUNCIL_PAYG, payg),
        (COUNCIL_FLEET, fleet),
    ];
    if !other.is_empty() {
        groups.push((COUNCIL_OTHER, other));
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_code::CodeEngine;

    fn engine(label: &str, model: &str, base_url: &str) -> EngineConfig {
        EngineConfig {
            label: label.into(),
            model: model.into(),
            base_url: base_url.into(),
            ..Default::default()
        }
    }

    fn target() -> BTreeMap<String, BuildTargetConfig> {
        let mut targets = BTreeMap::new();
        targets.insert(
            "t1".to_string(),
            BuildTargetConfig {
                ssh_host: "spark".into(),
                runner_path: "/r".into(),
                repo_id: "repo".into(),
                model: "qwen".into(),
                verify_profile: "rust".into(),
            },
        );
        targets
    }

    #[test]
    fn l1d_chip_labels() {
        let none = BTreeMap::new();
        let gemma = engine("Gemma 4 12B (Studio)", "/m/gemma", "http://h:1");
        assert_eq!(
            chip_label(&WorkExecutor::InApp, &gemma, &[], &none),
            "In-app \u{00B7} Gemma 4 12B (Studio)"
        );
        let unlabeled = engine("", "/Users/x/models/gemma-4-12b-it-qat-mlx-4bit", "");
        assert_eq!(
            chip_label(&WorkExecutor::InApp, &unlabeled, &[], &none),
            "In-app \u{00B7} gemma-4-12b-it-qat-mlx-4bit"
        );
        assert_eq!(
            chip_label(&WorkExecutor::InApp, &EngineConfig::default(), &[], &none),
            "In-app \u{00B7} no model set"
        );
        assert_eq!(
            chip_label(&WorkExecutor::Gateway, &gemma, &[], &none),
            "Fleet gateway"
        );
        let sonnet = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: Some("sonnet".into()),
        };
        assert_eq!(
            chip_label(&sonnet, &gemma, &[], &none),
            "Claude \u{00B7} sonnet"
        );
    }

    #[test]
    fn l1d_picker_puts_this_mac_first_and_fleet_after_cloud() {
        let gemma = engine("Gemma 4 12B (Studio)", "m", "http://h:1");
        let models = ModelsConfig::default();
        let sections = picker_sections(&gemma, &[], &models, &BTreeMap::new());
        let headers: Vec<&str> = sections.iter().map(|s| s.header).collect();
        assert_eq!(headers, [THIS_MAC_HEADER, CLOUD_HEADER, FLEET_HEADER]);
        assert_eq!(
            sections[0].rows,
            vec![(
                "In-app \u{00B7} Gemma 4 12B (Studio)".to_string(),
                WorkExecutor::InApp
            )]
        );
        assert_eq!(sections[1].rows, work_executor_agent_rows(&models));
        assert_eq!(sections[2].rows.len(), 1);
        assert_eq!(sections[2].rows[0].1, WorkExecutor::Gateway);
        assert!(sections[2].rows[0]
            .0
            .contains("can't see this Mac's folders"));

        let with_build = picker_sections(&gemma, &[], &models, &target());
        assert_eq!(with_build.len(), 4);
        assert_eq!(with_build[3].header, BUILD_HEADER);
        assert_eq!(
            with_build[3].rows,
            vec![(
                "Build \u{00B7} qwen on spark".to_string(),
                WorkExecutor::LocalBuild {
                    target_id: "t1".into()
                }
            )]
        );
    }

    #[test]
    fn l1d_where_lines() {
        let at_host = engine("", "m", "http://192.0.2.10:8851");
        let owned = WorkRunIdentity::OwnedEngine {
            model: "m".into(),
            engine_id: None,
        };
        assert_eq!(
            where_line(&owned, &at_host),
            "Where: Awili on this Mac \u{00B7} model at 192.0.2.10"
        );
        assert_eq!(
            where_line(&owned, &engine("", "m", "")),
            "Where: Awili on this Mac"
        );
        let glm = WorkRunIdentity::Agent {
            engine: CodeEngine::Glm,
            model: None,
        };
        assert_eq!(where_line(&glm, &at_host), "Where: GLM CLI on this Mac");
        assert_eq!(
            where_line(&WorkRunIdentity::HermesGateway, &at_host),
            "Where: fleet gateway (remote) \u{00B7} can't see this Mac's folders"
        );
        let build = WorkRunIdentity::LocalBuild {
            model: "qwen".into(),
            host: "spark".into(),
        };
        assert_eq!(where_line(&build, &at_host), "Where: build runner on spark");
    }

    /// S3-F06 regression: the last run said "gateway" while GLM was picked,
    /// and nothing on screen said which one the next run would use.
    #[test]
    fn l1d_next_run_line_only_when_lane_differs() {
        let none = BTreeMap::new();
        let cfg = engine("Gemma", "m", "http://h:1");
        let glm = WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None,
        };
        let gw = WorkRunIdentity::HermesGateway;
        let idle = run_lines(Some(&gw), &glm, &cfg, &[], &none, false);
        assert!(idle.contains(&"Next run: GLM".to_string()), "{idle:?}");
        let running = run_lines(Some(&gw), &glm, &cfg, &[], &none, true);
        assert!(
            running.iter().all(|l| !l.starts_with("Next run")),
            "{running:?}"
        );
        let owned = WorkRunIdentity::OwnedEngine {
            model: "m".into(),
            engine_id: None,
        };
        let same = run_lines(Some(&owned), &WorkExecutor::InApp, &cfg, &[], &none, false);
        assert_eq!(same, ["Where: Awili on this Mac \u{00B7} model at h"]);
        let sonnet = WorkRunIdentity::Agent {
            engine: CodeEngine::Claude,
            model: Some("sonnet".into()),
        };
        let claude = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        let lines = run_lines(Some(&sonnet), &claude, &cfg, &[], &none, false);
        assert!(lines.contains(&"Next run: Claude".to_string()), "{lines:?}");
        assert!(run_lines(None, &glm, &cfg, &[], &none, false).is_empty());
    }

    #[cfg(not(unix))]
    #[allow(unused_variables, dead_code)]
    fn fake_cli(dir: &std::path::Path, name: &str) -> String {
        unimplemented!("TODO(windows-phase1): this test fixture needs a Windows twin")
    }
    #[cfg(unix)]
    fn fake_cli(dir: &std::path::Path, name: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path.display().to_string()
    }

    fn row<'a>(sections: &'a [TargetSection], target: &WorkExecutor) -> &'a TargetRow {
        sections
            .iter()
            .flat_map(|section| section.rows.iter())
            .find(|row| &row.target == target)
            .unwrap_or_else(|| panic!("the chooser offers {target:?}"))
    }

    /// The chooser offers the in-app model, every harness with its configured
    /// models and the fleet gateway at once; each unavailable one names the
    /// setup that is missing; configured Build targets are their own group.
    #[test]
    #[cfg(unix)]
    fn chooser_offers_every_target_and_names_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let mut agents = awili_config::Config::networkless().work.agents;
        agents.claude_bin = Some(fake_cli(dir.path(), "claude"));
        agents.codex_bin = Some(dir.path().join("absent-codex").display().to_string());
        agents.zcode_bin = Some(dir.path().join("absent-zcode").display().to_string());
        agents.dsh_bin = Some(dir.path().join("absent-dsh").display().to_string());
        let models = ModelsConfig {
            claude_aliases: vec!["sonnet".into()],
            dsh_models: vec!["deepseek-v4".into()],
            ..Default::default()
        };
        let mut owned = engine("", "", "");
        owned.kind = awili_config::EngineKind::Awili;
        let builds = target();
        let reach = |engine: &EngineConfig, gateway_set_up: bool| {
            target_sections(&TargetReach {
                engine,
                extras: &[],
                models: &models,
                agents: &agents,
                build_targets: &BTreeMap::new(),
                gateway_set_up,
            })
        };
        let sections = reach(&owned, false);
        assert_eq!(
            sections.iter().map(|s| s.header).collect::<Vec<_>>(),
            [THIS_MAC_HEADER, CLOUD_HEADER, FLEET_HEADER]
        );
        let claude = |model: Option<&str>| WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: model.map(str::to_string),
        };
        assert_eq!(row(&sections, &claude(None)).unavailable, None);
        assert_eq!(row(&sections, &claude(Some("sonnet"))).unavailable, None);
        let dsh = row(
            &sections,
            &WorkExecutor::Agent {
                engine: CodeEngine::Dsh,
                model: Some("deepseek-v4".into()),
            },
        );
        let reason = dsh.unavailable.as_deref().unwrap();
        assert!(
            reason.contains("isn't installed") && reason.contains("`dsh`"),
            "{reason}"
        );
        let codex = row(
            &sections,
            &WorkExecutor::Agent {
                engine: CodeEngine::Codex,
                model: None,
            },
        );
        assert!(codex.unavailable.as_deref().unwrap().contains("Codex"));
        // Nothing needed by the in-app model or the fleet is configured.
        assert!(row(&sections, &WorkExecutor::InApp)
            .unavailable
            .as_deref()
            .unwrap()
            .contains("in-app model isn't set up"));
        assert!(row(&sections, &WorkExecutor::Gateway)
            .unavailable
            .as_deref()
            .unwrap()
            .contains("fleet gateway isn't set up"));
        let ready = reach(&engine("Gemma", "m", "http://h:1"), true);
        assert_eq!(row(&ready, &WorkExecutor::InApp).unavailable, None);
        assert_eq!(row(&ready, &WorkExecutor::Gateway).unavailable, None);
        // No configured Build target, no Build row; a configured one is its own
        // group and nothing else is invented.
        assert!(ready
            .iter()
            .flat_map(|s| s.rows.iter())
            .all(|r| !matches!(r.target, WorkExecutor::LocalBuild { .. })));
        let with_builds = target_sections(&TargetReach {
            engine: &engine("Gemma", "m", "http://h:1"),
            extras: &[],
            models: &models,
            agents: &agents,
            build_targets: &builds,
            gateway_set_up: true,
        });
        let last = with_builds.last().unwrap();
        assert_eq!(last.header, BUILD_HEADER);
        assert_eq!(
            last.rows
                .iter()
                .map(|r| r.target.clone())
                .collect::<Vec<_>>(),
            [WorkExecutor::LocalBuild {
                target_id: "t1".into()
            }]
        );
    }

    /// The chip names the next target. Only while a turn runs under a different
    /// one does it say "Next:", and the tooltip then names both.
    #[test]
    fn chip_separates_the_running_target_from_the_next_one() {
        let none = BTreeMap::new();
        let cfg = engine("Gemma", "m", "http://h:1");
        let claude = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: Some("sonnet".into()),
        };
        let codex = WorkExecutor::Agent {
            engine: CodeEngine::Codex,
            model: None,
        };
        let idle = target_chip(Some(&claude), None, &cfg, &[], &none);
        assert_eq!(idle.label, "Claude \u{00B7} sonnet");
        // Distinct configured models stay distinct on the chip, even in one
        // family (every gpt-* used to read just "GPT").
        let codex_model = |m: &str| WorkExecutor::Agent {
            engine: CodeEngine::Codex,
            model: Some(m.to_string()),
        };
        assert_ne!(
            short_target(&codex_model("gpt-5.6-sol"), &[], &none),
            short_target(&codex_model("gpt-5.6-terra"), &[], &none)
        );
        assert_eq!(
            short_target(&codex_model("gpt-5.6-sol"), &[], &none),
            "Codex \u{00B7} gpt-5.6-sol"
        );
        assert!(idle.tooltip.contains("next message"), "{}", idle.tooltip);
        let same = target_chip(Some(&claude), Some(&claude), &cfg, &[], &none);
        assert_eq!(
            same.label, idle.label,
            "a running turn under the same target adds nothing"
        );
        let pending = target_chip(Some(&codex), Some(&claude), &cfg, &[], &none);
        assert_eq!(pending.label, "Next: Codex");
        assert!(
            pending.tooltip.starts_with("Running now: Claude")
                && pending
                    .tooltip
                    .ends_with("Your next message goes to Codex."),
            "{}",
            pending.tooltip
        );
        let missing = target_chip(None, Some(&claude), &cfg, &[], &none);
        assert_eq!(missing.label, "Choose model");
        assert!(missing.tooltip.contains("isn't available"));
    }

    /// The chip, the picker and the header read these functions (the hooks
    /// are one call each; the app-level test drives the readers).
    #[test]
    fn l1d_hooks_are_wired() {
        let main = include_str!("main.rs");
        assert_eq!(
            include_str!("workshop_composer.rs")
                .matches("executor_labels::target_chip(")
                .count(),
            1
        );
        assert_eq!(main.matches("executor_labels::picker_sections(").count(), 1);
        for old in [
            "\"exec-fleet\"",
            "\"exec-agent\"",
            "\"exec-build\"",
            "\"gateway (fleet)\"",
        ] {
            assert!(!main.contains(old), "main.rs still builds {old}");
        }
        let work = include_str!("work.rs");
        assert_eq!(work.matches(".work_provenance_lines(owns_run)").count(), 1);
    }
}
