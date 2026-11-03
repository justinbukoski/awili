//! L1b — Work runs in the app by default. The selected executor, not
//! `[engine].kind`, decides the engine (spec L1, required behaviour 2):
//! `InApp` always builds the in-app engine, `Gateway` always runs the Hermes
//! gateway, and a live persistent worker from the other lane (or an in-app
//! worker for a different model) is retired before the next prompt, so a
//! turn never lands on an engine the chip does not show.

use awili_config::{EngineConfig, EngineKind, ExtraEngineConfig};

use crate::work::{GatewayCmd, WorkExecutor, WorkState};
use crate::work_run_identity::WorkRunIdentity;

/// `InApp` → the in-app engine; every other executor → the Hermes gateway.
/// Only `InApp` and `Gateway` reach the persistent-worker path; `Agent` and
/// `LocalBuild` branch off earlier in `submit_work_run_in`.
pub(crate) fn engine_kind_for(executor: &WorkExecutor) -> EngineKind {
    match executor {
        WorkExecutor::InApp | WorkExecutor::InAppExtra { .. } => EngineKind::Awili,
        WorkExecutor::Gateway | WorkExecutor::Agent { .. } | WorkExecutor::LocalBuild { .. } => {
            EngineKind::HermesGateway
        }
    }
}

/// The engine the in-app executor `executor` runs on: the primary `engine`
/// for `InApp`, the `[[engines]]` entry with that id for `InAppExtra`. `None`
/// only for an `InAppExtra` whose id is not (or no longer) configured --
/// never the primary in its place. Non-in-app executors resolve to `engine`.
pub(crate) fn resolve_engine(
    executor: &WorkExecutor,
    engine: &EngineConfig,
    extras: &[ExtraEngineConfig],
) -> Option<EngineConfig> {
    match executor {
        WorkExecutor::InAppExtra { id } => extras
            .iter()
            .find(|e| &e.id == id)
            .map(ExtraEngineConfig::to_engine_config),
        _ => Some(engine.clone()),
    }
}

/// `engine` (or the selected extra's) with `kind = engine_kind_for(executor)`;
/// every other field is unchanged (the endpoint, model, key and limits stay
/// the config's). An unknown extra id gives a blank, unconfigured engine so
/// nothing can reach the primary by mistake; the run's precondition reports
/// it as an error.
pub(crate) fn effective_engine(
    executor: &WorkExecutor,
    engine: &EngineConfig,
    extras: &[ExtraEngineConfig],
) -> EngineConfig {
    let mut out = resolve_engine(executor, engine, extras).unwrap_or_default();
    out.kind = engine_kind_for(executor);
    out
}

/// The identity a fresh persistent worker for `executor` would have:
/// `InApp` / `Gateway` → `Some(WorkRunIdentity::worker(effective))`;
/// `Agent` / `LocalBuild` → `None` (they never use the persistent worker).
pub(crate) fn lane_identity(
    executor: &WorkExecutor,
    engine: &EngineConfig,
    extras: &[ExtraEngineConfig],
) -> Option<WorkRunIdentity> {
    match executor {
        WorkExecutor::InApp | WorkExecutor::InAppExtra { .. } | WorkExecutor::Gateway => {
            Some(WorkRunIdentity::worker_on(
                &effective_engine(executor, engine, extras),
                executor.extra_engine_id().map(String::from),
            ))
        }
        WorkExecutor::Agent { .. } | WorkExecutor::LocalBuild { .. } => None,
    }
}

/// The engine of the worker that raised an approval card: an owned-engine
/// identity → `Awili`; any other identity → `HermesGateway`; no identity →
/// the selected executor's engine.
pub(crate) fn identity_engine_kind(
    identity: Option<&WorkRunIdentity>,
    executor: &WorkExecutor,
) -> EngineKind {
    match identity {
        Some(WorkRunIdentity::OwnedEngine { .. }) => EngineKind::Awili,
        Some(_) => EngineKind::HermesGateway,
        None => engine_kind_for(executor),
    }
}

/// D6: retire a live, idle persistent worker that belongs to another lane.
///
/// When a worker is live (`gateway_cmd_tx` and `worker_identity` are both
/// set), no gateway Stop is pending, and the selected executor's lane
/// identity is `Some(want)` with `want` != the live worker's identity: send
/// it `GatewayCmd::Shutdown` (a dead channel is fine), forget the sender,
/// and clear `session_id` and `worker_identity`, so the next dispatch
/// spawns the selected lane's worker. Returns whether it retired one;
/// otherwise nothing changes. RC2-R6M9: an in-app worker of another model
/// server counts as another lane too.
pub(crate) fn retire_other_lane_worker(work: &mut WorkState) -> bool {
    if work.gateway_stop_pending || work.gateway_cmd_tx.is_none() {
        return false;
    }
    let Some(live) = work.worker_identity.as_ref() else {
        return false;
    };
    let Some(want) = lane_identity(&work.executor, &work.engine, &work.extra_engines) else {
        return false;
    };
    // RC2-R6M9 (RC2-R6 review P1 on 8197ec08: the identity holds the
    // model but not its server, so after the endpoint changed and changed
    // back a turn could reuse a live worker of the other server): an
    // in-app worker is also retired when it reads from another server or
    // model than the selected one (`WarmState::worker_engine`, set when it
    // was started).
    let other_server = matches!(want, WorkRunIdentity::OwnedEngine { .. })
        && matches!(live, WorkRunIdentity::OwnedEngine { .. })
        && work.warm.worker_engine.as_deref().is_some_and(|id| {
            let engine = work.effective_engine();
            id != crate::inapp_warm::engine_id(&engine.base_url, &engine.model)
        });
    if &want == live && !other_server {
        return false;
    }
    if let Some(cmd_tx) = work.gateway_cmd_tx.take() {
        let _ = cmd_tx.send(GatewayCmd::Shutdown);
    }
    work.session_id = None;
    work.worker_identity = None;
    true
}

/// RC1-J1-FIX2: a Work turn on a lane that runs no worker (the agent CLI,
/// local Build) first retires a live in-app or gateway worker. That worker's
/// event pump belongs to an earlier run (the new turn takes a new run number),
/// so a later prompt handed to it would have every event dropped and the turn
/// would never end; and a new in-app worker is then built from the
/// conversation, the other lane's turn shown only as context. Not while a
/// Stop is still settling. Returns whether a worker was retired.
pub(crate) fn retire_worker_for_workerless_lane(work: &mut WorkState) -> bool {
    if work.gateway_stop_pending {
        return false;
    }
    let Some(cmd_tx) = work.gateway_cmd_tx.take() else {
        return false;
    };
    let _ = cmd_tx.send(GatewayCmd::Shutdown);
    work.session_id = None;
    work.worker_identity = None;
    true
}

impl crate::AwiliApp {
    /// The engine kind the selected Work executor runs on — the ONLY runtime
    /// source of the Work engine kind (`[engine].kind` is legacy since L1).
    pub(crate) fn work_engine_kind(&self) -> EngineKind {
        engine_kind_for(&self.work.executor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::{seed_executor, GatewayCfg, HermesCfg};
    use awili_code::CodeEngine;

    fn engine(kind: EngineKind, model: &str) -> EngineConfig {
        EngineConfig {
            kind,
            base_url: "http://127.0.0.1:9".into(),
            model: model.into(),
            api_key: "k".into(),
            ..Default::default()
        }
    }

    fn work_state() -> WorkState {
        WorkState::new(
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
        )
    }

    #[test]
    fn l1b_in_app_persists_and_seeds() {
        assert_eq!(WorkExecutor::InApp.to_persist(), "in-app");
        assert_eq!(
            WorkExecutor::from_persist("in-app"),
            Some(WorkExecutor::InApp)
        );
        assert_eq!(WorkExecutor::InApp.label(), "in-app");
        assert_eq!(
            seed_executor(Some("in-app"), Some("agent:glm")),
            Some(WorkExecutor::InApp)
        );
        assert_eq!(
            seed_executor(None, Some("in-app")),
            Some(WorkExecutor::InApp)
        );
        // The fleet gateway keeps its own persisted form.
        assert_eq!(
            WorkExecutor::from_persist("gateway"),
            Some(WorkExecutor::Gateway)
        );
    }

    #[test]
    fn l1b_engine_follows_executor_not_config() {
        let agent = WorkExecutor::Agent {
            engine: CodeEngine::Claude,
            model: None,
        };
        let build = WorkExecutor::LocalBuild {
            target_id: "t".into(),
        };
        assert_eq!(engine_kind_for(&WorkExecutor::InApp), EngineKind::Awili);
        assert_eq!(
            engine_kind_for(&WorkExecutor::Gateway),
            EngineKind::HermesGateway
        );
        assert_eq!(engine_kind_for(&agent), EngineKind::HermesGateway);
        assert_eq!(engine_kind_for(&build), EngineKind::HermesGateway);

        let cfg = engine(EngineKind::HermesGateway, "m");
        let eff = effective_engine(&WorkExecutor::InApp, &cfg, &[]);
        assert_eq!(eff.kind, EngineKind::Awili);
        assert_eq!(eff.base_url, cfg.base_url);
        assert_eq!(eff.model, cfg.model);
        assert_eq!(eff.api_key, cfg.api_key);

        let cfg = engine(EngineKind::Awili, "m");
        let eff = effective_engine(&WorkExecutor::Gateway, &cfg, &[]);
        assert_eq!(eff.kind, EngineKind::HermesGateway);
        assert_eq!(
            lane_identity(&WorkExecutor::InApp, &cfg, &[]),
            Some(WorkRunIdentity::OwnedEngine {
                model: "m".into(),
                engine_id: None
            })
        );
        assert_eq!(
            lane_identity(&WorkExecutor::Gateway, &cfg, &[]),
            Some(WorkRunIdentity::HermesGateway)
        );
        assert_eq!(lane_identity(&agent, &cfg, &[]), None);
        assert_eq!(lane_identity(&build, &cfg, &[]), None);
    }

    #[test]
    fn l1b_identity_engine_kind() {
        let owned = WorkRunIdentity::OwnedEngine {
            model: "m".into(),
            engine_id: None,
        };
        assert_eq!(
            identity_engine_kind(Some(&owned), &WorkExecutor::Gateway),
            EngineKind::Awili
        );
        assert_eq!(
            identity_engine_kind(Some(&WorkRunIdentity::HermesGateway), &WorkExecutor::InApp),
            EngineKind::HermesGateway
        );
        assert_eq!(
            identity_engine_kind(None, &WorkExecutor::InApp),
            EngineKind::Awili
        );
        assert_eq!(
            identity_engine_kind(None, &WorkExecutor::Gateway),
            EngineKind::HermesGateway
        );
    }

    #[test]
    fn l1b_retire_only_on_lane_change() {
        let mut work = work_state();
        let (tx, rx) = std::sync::mpsc::channel();
        work.gateway_cmd_tx = Some(tx);
        work.session_id = Some("s1".into());
        work.worker_identity = Some(WorkRunIdentity::HermesGateway);

        // Same lane: nothing happens.
        work.executor = WorkExecutor::Gateway;
        assert!(!retire_other_lane_worker(&mut work));
        // An agent never uses the persistent worker: nothing happens.
        work.executor = WorkExecutor::Agent {
            engine: CodeEngine::Glm,
            model: None,
        };
        assert!(!retire_other_lane_worker(&mut work));
        // A Stop is pending: the Stop owns the worker's end.
        work.executor = WorkExecutor::InApp;
        work.gateway_stop_pending = true;
        assert!(!retire_other_lane_worker(&mut work));
        assert!(rx.try_recv().is_err(), "nothing was sent so far");
        assert!(work.gateway_cmd_tx.is_some());

        // Lane change: Shutdown, then everything is forgotten.
        work.gateway_stop_pending = false;
        assert!(retire_other_lane_worker(&mut work));
        assert!(matches!(rx.try_recv(), Ok(GatewayCmd::Shutdown)));
        assert!(work.gateway_cmd_tx.is_none());
        assert!(work.session_id.is_none());
        assert!(work.worker_identity.is_none());

        // An in-app worker for another model is retired too ...
        let (tx, rx) = std::sync::mpsc::channel();
        work.gateway_cmd_tx = Some(tx);
        work.worker_identity = Some(WorkRunIdentity::OwnedEngine {
            model: "a".into(),
            engine_id: None,
        });
        work.engine.model = "b".into();
        assert!(retire_other_lane_worker(&mut work));
        assert!(matches!(rx.try_recv(), Ok(GatewayCmd::Shutdown)));
        // ... and one for the configured model is kept.
        let (tx, rx) = std::sync::mpsc::channel();
        work.gateway_cmd_tx = Some(tx);
        work.worker_identity = Some(WorkRunIdentity::OwnedEngine {
            model: "b".into(),
            engine_id: None,
        });
        assert!(!retire_other_lane_worker(&mut work));
        assert!(rx.try_recv().is_err());
        assert!(work.gateway_cmd_tx.is_some());
    }

    /// `[engine].kind` is not read at runtime: every Work engine decision
    /// goes through `work_engine_kind()` / `effective_engine`.
    #[test]
    fn l1b_engine_kind_config_is_not_read_at_runtime() {
        let needle = concat!("self.work.engine", ".kind");
        for (name, src) in [
            ("work.rs", include_str!("work.rs")),
            ("main.rs", include_str!("main.rs")),
            (
                "gateway_health_app.rs",
                include_str!("gateway_health_app.rs"),
            ),
            ("ask_mode.rs", include_str!("ask_mode.rs")),
            ("ask_mode_ui.rs", include_str!("ask_mode_ui.rs")),
            ("plan_gate.rs", include_str!("plan_gate.rs")),
        ] {
            assert!(!src.contains(needle), "{name} still reads {needle}");
        }
    }

    /// The in-app lane has exactly one construction path, and it is GATED:
    /// `build_awili_engine` builds the worker over `HttpTransport::
    /// with_preflight(...)` (the residency backstop). The ungated
    /// `AwiliEngineWorker::new` is test-only, so no production caller can
    /// reach a model host without the gate.
    #[test]
    fn l1b_one_owned_engine_constructor() {
        let new = concat!("AwiliEngineWorker", "::new(");
        let with_transport = concat!("AwiliEngineWorker", "::with_transport(");
        let preflight = concat!(".with", "_preflight(");
        let work = include_str!("work.rs");
        // Exactly one production construction, over a transport with the gate.
        assert_eq!(work.matches(with_transport).count(), 1);
        assert_eq!(work.matches(preflight).count(), 1);
        let at = work.find(with_transport).unwrap();
        let before = &work[..at];
        assert!(
            before.rfind(preflight).is_some_and(|p| at - p < 1500),
            "the one construction is built from the gated transport"
        );
        // None in the other app files, and the ungated `new` is not used.
        for (name, src) in [
            ("work.rs", work),
            ("main.rs", include_str!("main.rs")),
            ("local_first.rs", include_str!("local_first.rs")),
        ] {
            // (this test's own text builds the needles with `concat!`)
            assert!(!src.contains(new), "{name} builds an ungated worker");
        }
        for (name, src) in [
            ("main.rs", include_str!("main.rs")),
            ("local_first.rs", include_str!("local_first.rs")),
        ] {
            assert!(!src.contains(with_transport), "{name} builds a worker");
        }
        // And `new` itself exists only under cfg(test).
        let worker = include_str!("../../awili-engine-worker/src/lib.rs");
        let at = worker.find("pub fn new(cfg: AwiliEngineConfig").unwrap();
        assert!(
            worker[..at].trim_end().ends_with("#[cfg(test)]"),
            "AwiliEngineWorker::new is test-only"
        );
    }
}
