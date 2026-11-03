//! Cron / scheduled jobs panel — listing, pause/resume, manual trigger.
//!
//! Fetches from `GET /api/cron/jobs` and supports `POST .../pause`,
//! `POST .../resume`, `POST .../trigger` actions.

use awili_gateway::rest::{RestClient, ScheduledJob};

/// Visual state for a job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    Enabled,
    /// The normal idle-and-armed state Hermes reports for an active cron job
    /// (`state: "scheduled"`, verified live). Rendered as healthy, same as
    /// `Enabled` — without this it fell through to `Other` and every normal job
    /// showed an amber attention icon.
    Scheduled,
    Paused,
    Running,
    Error,
    Disabled,
    /// A state string the client does not recognise — e.g. a server that grows a
    /// new `queued`/`blocked`/`needs_attention` state after this build shipped
    /// (L1). Previously any unknown state fell through to `Enabled` and rendered
    /// as a healthy green job; now it is rendered neutral/attention and the raw
    /// server string is preserved so the user sees what was actually reported.
    Other(String),
}

impl JobState {
    pub fn from_str(s: &str) -> Self {
        match s {
            "enabled" => JobState::Enabled,
            "scheduled" => JobState::Scheduled,
            "running" => JobState::Running,
            "paused" => JobState::Paused,
            "error" => JobState::Error,
            "disabled" => JobState::Disabled,
            other => JobState::Other(other.to_string()),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            JobState::Enabled => "enabled",
            JobState::Scheduled => "scheduled",
            JobState::Paused => "paused",
            JobState::Running => "running",
            JobState::Error => "error",
            JobState::Disabled => "disabled",
            JobState::Other(s) => s.as_str(),
        }
    }
}

/// A per-row action the user can take on a scheduled job. Dispatched
/// asynchronously by `cron_job_action` (work.rs) so the blocking gateway REST call
/// never runs on the UI thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CronAction {
    Pause,
    Resume,
    Trigger,
}

/// The cron panel state.
#[derive(Clone, Debug)]
pub struct CronState {
    pub jobs: Vec<ScheduledJob>,
    pub selected_id: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// Jobs paused from the UI this session, keyed by id. The gateway `GET /api/jobs`
    /// omits paused jobs entirely (see [`awili_gateway::rest::RestClient::list_jobs`]),
    /// so without this a job would vanish from the panel the instant we refetch
    /// after pausing — stranding its Resume action (no row, no chip). We keep the
    /// paused row here and re-merge it into every fresh listing via [`set_jobs`]
    /// until the job is resumed (or reappears as active, e.g. resumed elsewhere).
    pub paused_overlay: std::collections::HashMap<String, ScheduledJob>,
}

// The fetch/selection surface lands with the cron panel (Phase 5).
#[allow(dead_code)]
impl CronState {
    pub fn new() -> Self {
        Self {
            jobs: Vec::new(),
            selected_id: None,
            loading: false,
            error: None,
            paused_overlay: std::collections::HashMap::new(),
        }
    }

    /// Replace the job list from a fresh server listing, re-merging any
    /// UI-paused jobs the server no longer reports. An overlay entry that
    /// reappears in the listing (i.e. the server now considers it active) is
    /// dropped, so resuming a job out-of-band reconciles cleanly.
    pub fn set_jobs(&mut self, jobs: Vec<ScheduledJob>) {
        let listed: std::collections::HashSet<String> = jobs.iter().map(|j| j.id.clone()).collect();
        self.paused_overlay.retain(|id, _| !listed.contains(id));
        let mut merged = jobs;
        merged.extend(self.paused_overlay.values().cloned());
        self.jobs = merged;
    }

    /// Fetch the job list from the gateway REST backend.
    pub fn fetch(&mut self, client: &RestClient) {
        self.loading = true;
        self.error = None;
        match client.list_jobs() {
            Ok(jobs) => {
                self.set_jobs(jobs);
                self.loading = false;
            }
            Err(e) => {
                self.error = Some(format!("{e}"));
                self.loading = false;
            }
        }
    }

    /// Get the selected job.
    pub fn selected(&self) -> Option<&ScheduledJob> {
        let id = self.selected_id.as_deref()?;
        self.jobs.iter().find(|j| j.id == id)
    }

    /// Number of jobs by state.
    pub fn counts(&self) -> (usize, usize, usize) {
        let enabled = self
            .jobs
            .iter()
            .filter(|j| j.enabled && j.state != "paused" && j.state != "error")
            .count();
        let paused = self.jobs.iter().filter(|j| j.state == "paused").count();
        let errored = self.jobs.iter().filter(|j| j.state == "error").count();
        (enabled, paused, errored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use awili_gateway::rest::ScheduledJob;

    fn make_job(id: &str, name: &str, state: &str, enabled: bool) -> ScheduledJob {
        ScheduledJob {
            id: id.to_string(),
            name: name.to_string(),
            prompt: "test".to_string(),
            skills: Vec::new(),
            skill: None,
            model: None,
            schedule: None,
            schedule_display: None,
            enabled,
            state: state.to_string(),
            paused_at: None,
            paused_reason: None,
            created_at: None,
            next_run_at: None,
            last_run_at: None,
            last_status: None,
            last_error: None,
            enabled_toolsets: Vec::new(),
            workdir: None,
        }
    }

    #[test]
    fn job_state_from_str() {
        assert_eq!(JobState::from_str("running"), JobState::Running);
        assert_eq!(JobState::from_str("paused"), JobState::Paused);
        assert_eq!(JobState::from_str("error"), JobState::Error);
        assert_eq!(JobState::from_str("enabled"), JobState::Enabled);
        // "scheduled" is Hermes' normal active state — a known healthy state,
        // NOT an unrecognised one, so it must not fall through to `Other`.
        assert_eq!(JobState::from_str("scheduled"), JobState::Scheduled);
        assert_ne!(
            JobState::from_str("scheduled"),
            JobState::Other("scheduled".to_string())
        );
        assert_eq!(JobState::from_str("scheduled").label(), "scheduled");
    }

    #[test]
    fn unknown_state_is_other_not_enabled() {
        // L1: a server state this build doesn't know must NOT masquerade as a
        // healthy enabled job. It maps to `Other` (preserving the raw string),
        // and its label round-trips so the UI can show what the server reported.
        let s = JobState::from_str("queued");
        assert_eq!(s, JobState::Other("queued".to_string()));
        assert_ne!(s, JobState::Enabled);
        assert_eq!(s.label(), "queued");
    }

    #[test]
    fn counts() {
        let mut state = CronState::new();
        state.jobs = vec![
            make_job("1", "a", "enabled", true),
            make_job("2", "b", "paused", true),
            make_job("3", "c", "error", true),
            make_job("4", "d", "enabled", true),
        ];
        let (enabled, paused, errored) = state.counts();
        assert_eq!(enabled, 2);
        assert_eq!(paused, 1);
        assert_eq!(errored, 1);
    }

    #[test]
    fn counts_excludes_disabled_jobs_from_enabled_tally() {
        // The enabled tally gates on THREE conjuncts: `j.enabled` AND state is
        // neither "paused" nor "error". The sibling `counts` test only carries
        // jobs with enabled=true, so the leading `j.enabled &&` conjunct is
        // wholly unpinned there — dropping it leaves that count unchanged. A
        // job the server reports as disabled (enabled=false, state="disabled")
        // is neither paused nor error, so without the `j.enabled` guard it would
        // masquerade as an active job and inflate the cron-panel "enabled" chip.
        let mut state = CronState::new();
        state.jobs = vec![
            make_job("1", "a", "enabled", true),
            make_job("2", "b", "disabled", false),
        ];
        let (enabled, paused, errored) = state.counts();
        assert_eq!(enabled, 1, "the disabled job must not count as enabled");
        assert_eq!(paused, 0);
        assert_eq!(errored, 0);
    }

    #[test]
    fn selected_job() {
        let mut state = CronState::new();
        state.jobs = vec![make_job("abc", "test", "enabled", true)];
        state.selected_id = Some("abc".to_string());
        assert_eq!(state.selected().unwrap().name, "test");
        state.selected_id = Some("nonexistent".to_string());
        assert!(state.selected().is_none());
    }

    #[test]
    fn paused_overlay_persists_across_refetch_until_resumed() {
        let mut state = CronState::new();
        state.set_jobs(vec![make_job("a", "watcher", "scheduled", true)]);
        assert_eq!(state.jobs.len(), 1);

        // Pause: stash a paused copy in the overlay, then a refetch that omits it
        // (Hermes hides paused jobs). The row must survive so Resume stays usable.
        let mut paused = make_job("a", "watcher", "paused", true);
        paused.state = "paused".to_string();
        state.paused_overlay.insert("a".to_string(), paused);
        state.set_jobs(vec![]);
        assert_eq!(state.jobs.len(), 1, "paused job re-merged from overlay");
        assert_eq!(state.jobs[0].state, "paused");
        assert_eq!(state.counts().1, 1, "paused count reflects the overlay");

        // Resume: drop from overlay; the server lists it active again.
        state.paused_overlay.remove("a");
        state.set_jobs(vec![make_job("a", "watcher", "scheduled", true)]);
        assert_eq!(state.jobs[0].state, "scheduled");

        // Reconcile: an overlay entry that reappears active is dropped, no dupe.
        state
            .paused_overlay
            .insert("a".to_string(), make_job("a", "watcher", "paused", true));
        state.set_jobs(vec![make_job("a", "watcher", "scheduled", true)]);
        assert!(
            state.paused_overlay.is_empty(),
            "reappeared job left overlay"
        );
        assert_eq!(state.jobs.len(), 1, "no duplicate row");
    }
}
