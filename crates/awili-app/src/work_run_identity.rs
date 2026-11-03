//! Requested dispatch configuration, never a claim about inference location.
use awili_code::CodeEngine;
use awili_config::{EngineConfig, EngineKind};

use crate::work::RunPanel;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkRunIdentity {
    Agent {
        engine: CodeEngine,
        model: Option<String>,
    },
    HermesGateway,
    OwnedEngine {
        model: String,
        /// The `[[engines]]` id of the extra this ran on; `None` = the primary
        /// `[engine]` (and every record written before `[[engines]]` existed).
        engine_id: Option<String>,
    },
    /// A local Build dispatched through the durable awili-dispatch runner
    /// (daily-driver P2d) — the ACTUAL configured binding: which model runs
    /// on which dispatcher host. Never a claim about frontier inference.
    LocalBuild {
        model: String,
        host: String,
    },
}

impl WorkRunIdentity {
    #[cfg(test)]
    pub(crate) fn worker(config: &EngineConfig) -> Self {
        Self::worker_on(config, None)
    }

    /// [`worker`](Self::worker) for the engine `engine_id` (an `[[engines]]` id;
    /// `None` = the primary).
    pub(crate) fn worker_on(config: &EngineConfig, engine_id: Option<String>) -> Self {
        match config.kind {
            EngineKind::HermesGateway => Self::HermesGateway,
            EngineKind::Awili => Self::OwnedEngine {
                model: config.model.clone(),
                engine_id,
            },
        }
    }

    fn requested_line(&self) -> String {
        match self {
            Self::Agent { engine, model } => format!(
                "Requested: {} · {}",
                engine.label(),
                model.as_deref().unwrap_or("harness default model")
            ),
            Self::HermesGateway => "Requested: Hermes gateway · gateway chooses model".into(),
            Self::OwnedEngine { model, .. } => format!("Requested: Awili engine · {model}"),
            Self::LocalBuild { model, host } => {
                format!("Requested: Build · {model} on {host}")
            }
        }
    }
}

impl RunPanel {
    pub(crate) fn identity_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(identity) = &self.requested_identity {
            lines.push(identity.requested_line());
        }
        if let Some(model) = self.model.as_deref().filter(|m| !m.is_empty()) {
            lines.push(format!("Backend-reported model: {model}"));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_gateway::run_events::RunStatus;

    #[test]
    fn requested_alias_and_reported_model_remain_separate() {
        let mut panel = RunPanel {
            requested_identity: Some(WorkRunIdentity::Agent {
                engine: CodeEngine::Claude,
                model: Some("sonnet".into()),
            }),
            model: Some("reported-concrete-model".into()),
            ..Default::default()
        };
        assert_eq!(
            panel.identity_lines(),
            [
                "Requested: Claude · sonnet",
                "Backend-reported model: reported-concrete-model",
            ]
        );
        panel.apply_event(&awili_gateway::run_events::RunEvent::Completion(
            awili_gateway::run_events::CompletionEvent {
                status: Some(RunStatus::Completed),
                output: Some("done".into()),
            },
        ));
        panel.activity = None;
        assert_eq!(panel.identity_lines().len(), 2);
        // The existing new-turn/preflight reset removes both kinds of identity.
        panel = RunPanel::default();
        assert!(panel.identity_lines().is_empty());
    }

    #[test]
    fn persistent_worker_snapshot_does_not_follow_config_changes() {
        let mut config = EngineConfig {
            kind: EngineKind::Awili,
            model: "local-looking-name".into(),
            ..Default::default()
        };
        let worker_identity = WorkRunIdentity::worker(&config);
        config.model = "another-model".into();
        config.kind = EngineKind::HermesGateway;
        let next_panel = RunPanel {
            requested_identity: Some(worker_identity.clone()),
            ..Default::default()
        };
        assert_eq!(
            next_panel.identity_lines(),
            ["Requested: Awili engine · local-looking-name"]
        );
        assert_ne!(worker_identity, WorkRunIdentity::worker(&config));
        assert!(!next_panel.identity_lines()[0].contains("cloud"));
    }

    /// P9: the Dsh requested-identity line follows from `label()` —
    /// "Requested: DeepSeek · <model>" for a chosen model, and
    /// "Requested: DeepSeek · harness default model" when the picker's default
    /// is chosen (which sets NO `DSH_MODEL`).
    #[test]
    fn dsh_requested_identity_follows_label() {
        let mut panel = RunPanel {
            requested_identity: Some(WorkRunIdentity::Agent {
                engine: CodeEngine::Dsh,
                model: Some("deepseek-flash".into()),
            }),
            ..Default::default()
        };
        assert_eq!(
            panel.identity_lines(),
            ["Requested: DeepSeek · deepseek-flash"]
        );
        panel.requested_identity = Some(WorkRunIdentity::Agent {
            engine: CodeEngine::Dsh,
            model: None,
        });
        assert_eq!(
            panel.identity_lines(),
            ["Requested: DeepSeek · harness default model"]
        );
        // A backend-reported model is a SEPARATE line, and Dsh never reports
        // one (headless prints no usage) — but the machinery stays the same.
        panel.model = Some("deepseek-v4-pro".into());
        assert_eq!(
            panel.identity_lines(),
            [
                "Requested: DeepSeek · harness default model",
                "Backend-reported model: deepseek-v4-pro",
            ]
        );
    }

    #[test]
    fn gateway_and_cli_defaults_do_not_claim_a_resolved_model() {
        let mut panel = RunPanel {
            requested_identity: Some(WorkRunIdentity::HermesGateway),
            model: Some("qwen-local-name".into()),
            ..Default::default()
        };
        assert_eq!(
            panel.identity_lines(),
            [
                "Requested: Hermes gateway · gateway chooses model",
                "Backend-reported model: qwen-local-name",
            ]
        );
        panel.model = None;
        panel.requested_identity = Some(WorkRunIdentity::Agent {
            engine: CodeEngine::Codex,
            model: None,
        });
        assert_eq!(
            panel.identity_lines(),
            ["Requested: Codex · harness default model"]
        );
    }

    #[test]
    fn fresh_or_preflight_failed_panel_has_no_dispatch_claim() {
        let mut panel = RunPanel::default();
        assert!(panel.identity_lines().is_empty());
        panel.error = Some("agent runs need a working folder".into());
        assert!(panel.identity_lines().is_empty());
    }
}
