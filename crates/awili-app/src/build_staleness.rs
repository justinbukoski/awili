//! Daily-driver P2f — the staleness exit for the local Build poll loops.
//!
//! GPUI-free: [`PollStaleness`] watches one build row's `status` results and
//! turns an unbroken run of failed polls into the app's own `stale: ` verdict
//! (Fable ruling 1: at least [`STALE_MIN_FAILURES`] consecutive failures that
//! span at least [`STALE_AFTER`] — both must hold). The streak lives in memory
//! only and restarts on every launch. The verdict never fabricates a runner
//! snapshot: the closed row carries it as `needs_attention` failure text.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

/// How long `status` must keep failing before a row is marked stale.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(600);

/// How many consecutive `status` failures a row needs before it is stale.
pub(crate) const STALE_MIN_FAILURES: u32 = 3;

/// The closing text when the user's Cancel run ends a local build (Fable
/// ruling 3, option B). The runner job itself is left as is.
pub(crate) const CANCEL_REASON: &str = "stale: stopped by user (remote job not cancelled)";

/// What one observed poll result means for the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PollVerdict {
    /// The last poll succeeded.
    Healthy,
    /// Polls are failing; `remaining` is the time left before the row can
    /// be marked stale (zero once only the count is missing).
    Failing { streak: u32, remaining: Duration },
    /// The row is stale; the text is the exact `stale: …` closing reason.
    Stale(String),
}

/// The failure streak of one polled build row (in memory only).
#[derive(Clone, Debug)]
pub(crate) struct PollStaleness {
    streak: u32,
    since: Option<Instant>,
    last_error: Option<String>,
    after: Duration,
    min_failures: u32,
}

impl Default for PollStaleness {
    fn default() -> Self {
        Self::with_limits(STALE_AFTER, STALE_MIN_FAILURES)
    }
}

impl PollStaleness {
    /// A tracker with explicit limits (tests inject small values; production
    /// uses [`Default`], i.e. [`STALE_AFTER`] / [`STALE_MIN_FAILURES`]).
    pub(crate) fn with_limits(after: Duration, min_failures: u32) -> Self {
        Self {
            streak: 0,
            since: None,
            last_error: None,
            after,
            min_failures,
        }
    }

    /// Record one `status` result at `now`. A success resets the streak; a
    /// failure extends it and is `Stale` only when the streak is at least the
    /// minimum count AND the first failure is at least `after` old.
    pub(crate) fn observe(&mut self, result: Result<(), &str>, now: Instant) -> PollVerdict {
        let error = match result {
            Ok(()) => {
                *self = Self::with_limits(self.after, self.min_failures);
                return PollVerdict::Healthy;
            }
            Err(e) => e,
        };
        self.streak = self.streak.saturating_add(1);
        let since = *self.since.get_or_insert(now);
        self.last_error = Some(error.to_string());
        let elapsed = now.saturating_duration_since(since);
        if self.streak >= self.min_failures && elapsed >= self.after {
            return PollVerdict::Stale(format!(
                "stale: build host unreachable for {}m after {} failed status polls (last error: {error})",
                elapsed.as_secs() / 60,
                self.streak
            ));
        }
        PollVerdict::Failing {
            streak: self.streak,
            remaining: self.after.saturating_sub(elapsed),
        }
    }
}

/// The one-line panel message while the driving row's polls are failing.
pub(crate) fn failing_line(streak: u32, remaining: Duration, error: &str) -> String {
    format!(
        "build status unreachable ({streak} tries) — marking stale in {}m: {error}",
        remaining.as_secs().div_ceil(60)
    )
}

/// The live local build the Work panel is driving (fresh submit or the
/// reattached driving row): the row identity Cancel run closes, and the flag
/// that stops its poll task.
#[derive(Clone, Debug)]
pub(crate) struct LocalBuildHandle {
    /// The durable turn id of the build row.
    pub turn_id: String,
    /// The runner request id of the build row.
    pub request_id: String,
    /// The configured `[build_targets.<id>]` the row was submitted to.
    pub target_id: String,
    /// The last observed runner revision (the persisted one until a poll lands).
    pub revision: u64,
    /// Set when the poll task must stop without writing anything else.
    pub stop: Arc<AtomicBool>,
}

impl LocalBuildHandle {
    /// Whether the poll task was told to stop.
    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Close this row as terminal `needs_attention` with `failure`.
    ///
    /// RC2-APP (F0040): closing also stops the row's poll task first, so a
    /// poll that lands later can never write over the closed row — whatever
    /// the caller did before.
    pub(crate) fn close(&self, persist: Option<&crate::persistence::Persistence>, failure: String) {
        self.stop.store(true, Ordering::SeqCst);
        crate::build_dispatch::close_row(
            persist,
            &self.turn_id,
            &self.request_id,
            &self.target_id,
            self.revision,
            failure,
        );
    }
}

#[cfg(test)]
mod rc2app_tests {
    use super::*;

    /// RC2-APP (F0040): closing a row stops its poll task, even when the
    /// caller did not stop it first.
    #[test]
    fn rc2app_closing_a_row_stops_its_poll() {
        let handle = LocalBuildHandle {
            turn_id: "turn".into(),
            request_id: "req".into(),
            target_id: "hosta".into(),
            revision: 3,
            stop: Arc::new(AtomicBool::new(false)),
        };
        assert!(!handle.stopped());
        handle.close(None, CANCEL_REASON.to_string());
        assert!(handle.stopped(), "a late poll finds the row closed");
    }
}
