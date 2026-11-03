//! N1 notification kernel (`docs/daily-driver/specs/N1-macos-notifications.md`
//! §2.1). PURE: no gpui, no platform, no I/O; `Instant` is passed in. The
//! [`Notifier`] decides whether an event posts, holds the permission state
//! machine and the pending queue, and turns each decision into [`Action`]s
//! the glue (`notify_hooks.rs`) runs on a backend (`notify_platform.rs`).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use awili_config::NotificationsConfig;
use awili_work::run_events::RunStatus;

/// How many event keys the dedupe set remembers (FIFO).
const SEEN_CAP: usize = 256;
/// How many notifications wait while permission is being asked.
const PENDING_CAP: usize = 4;

/// How many native posts are remembered (with their kind) in case the
/// backend later reports one failed; a failure for an older one is dropped.
const SENT_CAP: usize = 64;
/// A user-stop mark is consumed by the next Stopped/Failed on its target, or
/// expires after this long.
pub const USER_STOP_TTL: Duration = Duration::from_secs(120);
/// The `Unavailable` reason of a backend that cannot post at all.
pub const UNSUPPORTED: &str = "not supported on this platform";

/// What a notification is about; its route id is the click target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A Work conversation (by id).
    Work(String),
    /// A Code tab (by its per-launch id).
    Code(u64),
    /// The Settings "send test" button.
    Test,
}

/// The event families of §1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// An approval card (`kind` is the approval's kind, never its prompt).
    Approval { kind: String },
    /// A `clarify` approval: the agent is asking a question.
    Question,
    /// A sudo (`sudo: true`) or secret credential prompt.
    Credential { sudo: bool },
    /// A run or turn finished.
    Finished,
    /// A run or turn failed.
    Failed,
    /// A run stopped by something other than the owner.
    Stopped,
    /// The Settings test.
    Test,
}

impl EventKind {
    /// Approval, question and credential: the agent is waiting on the owner.
    fn is_waiting(&self) -> bool {
        matches!(
            self,
            EventKind::Approval { .. } | EventKind::Question | EventKind::Credential { .. }
        )
    }
}

/// One event, built by the glue from Awili-owned labels only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub target: Target,
    pub kind: EventKind,
    /// The dedupe key (§1 table).
    pub key: String,
    /// Folder basename, conversation title or project name.
    pub label: String,
    /// Optional Awili-owned detail (the Code engine label).
    pub detail: Option<String>,
    /// The run's length, when known.
    pub elapsed: Option<Duration>,
}

/// A notification ready to post. `id` is also the thread id and the route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub id: String,
    pub title: String,
    pub body: String,
    pub sound: bool,
    /// The kernel's token for one native post (0 until it is posted
    /// natively). A failure report names this post, not the reusable `id`.
    pub post: u64,
}

/// The native permission state machine (§2.1, §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Permission {
    /// Nothing asked yet; the first eligible event asks.
    Unknown,
    /// The macOS prompt is up; events wait in the pending queue.
    Requesting,
    /// Native posting is allowed.
    Authorized,
    /// The owner turned Awili off: nothing posts, no fallback.
    Denied,
    /// Native posting cannot work here (reason); the fallback may post.
    Unavailable(&'static str),
}

/// What the backend must do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    RequestPermission,
    PostNative(Notification),
    PostFallback(Notification),
}

/// Why an event did not post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suppressed {
    Disabled,
    KindOff,
    Duplicate,
    UserStopped,
    Present,
    Visible,
    TooQuick,
    Denied,
}

/// The decision state: config, presence, permission, dedupe and pending.
pub struct Notifier {
    cfg: NotificationsConfig,
    permission: Permission,
    window_active: bool,
    inactive_since: Option<Instant>,
    seen: VecDeque<String>,
    /// Parked while the macOS prompt is up, with the kind that raised each
    /// (re-checked against the switches when the answer comes).
    pending: Vec<(EventKind, Notification)>,
    /// Native posts not yet reported failed, by post token, newest last, with
    /// the kind that raised each (a late failure re-checks the switches).
    sent: VecDeque<(u64, EventKind)>,
    /// The last post token handed out.
    last_post: u64,
    user_stops: Vec<(Target, Instant)>,
}

impl Notifier {
    /// A fresh notifier: window active, permission `Unknown`.
    pub fn new(cfg: NotificationsConfig) -> Self {
        Self {
            cfg,
            permission: Permission::Unknown,
            window_active: true,
            inactive_since: None,
            seen: VecDeque::new(),
            pending: Vec::new(),
            sent: VecDeque::new(),
            last_post: 0,
            user_stops: Vec::new(),
        }
    }

    /// Live re-apply (Settings save); keeps permission, seen and pending.
    pub fn set_config(&mut self, cfg: NotificationsConfig) {
        self.cfg = cfg;
    }

    /// The config in force.
    #[cfg(test)]
    pub fn config(&self) -> &NotificationsConfig {
        &self.cfg
    }

    /// The backend's launch-time answer (`Unknown` or `Unavailable`).
    pub fn set_initial_permission(&mut self, p: Permission) {
        self.permission = p;
    }

    /// The window's focus, sampled every frame. Records when it went inactive.
    pub fn observe_window(&mut self, active: bool, now: Instant) {
        if self.window_active && !active {
            self.inactive_since = Some(now);
        }
        if active {
            self.inactive_since = None;
        }
        self.window_active = active;
    }

    /// The owner pressed Stop on `t`: the next Stopped/Failed on it is silent.
    pub fn note_user_stop(&mut self, t: Target, now: Instant) {
        self.user_stops
            .retain(|(_, at)| now.saturating_duration_since(*at) < USER_STOP_TTL);
        self.user_stops.push((t, now));
    }

    /// Re-arm the permission request ("send test"): Unknown, Requesting and
    /// Denied become Unknown so the next event asks macOS again.
    pub fn rearm(&mut self) {
        if matches!(
            self.permission,
            Permission::Unknown | Permission::Requesting | Permission::Denied
        ) {
            self.permission = Permission::Unknown;
        }
    }

    /// Decide one event (§2.1 `handle` order, exact).
    pub fn handle(
        &mut self,
        ev: Event,
        visible: Option<&Target>,
        idle_secs: Option<u64>,
        now: Instant,
    ) -> Result<Vec<Action>, Suppressed> {
        if ev.kind != EventKind::Test {
            if !self.cfg.enabled {
                return Err(Suppressed::Disabled);
            }
            if !self.kind_on(&ev.kind) {
                return Err(Suppressed::KindOff);
            }
            if matches!(ev.kind, EventKind::Stopped | EventKind::Failed) {
                let mark = self.user_stops.iter().position(|(t, at)| {
                    *t == ev.target && now.saturating_duration_since(*at) < USER_STOP_TTL
                });
                if let Some(i) = mark {
                    self.user_stops.remove(i);
                    return Err(Suppressed::UserStopped);
                }
            }
            if self.seen.contains(&ev.key) {
                return Err(Suppressed::Duplicate);
            }
            self.presence_gate(&ev, visible, idle_secs, now)?;
            self.seen.push_back(ev.key.clone());
            while self.seen.len() > SEEN_CAP {
                self.seen.pop_front();
            }
        }
        let n = payload(&ev, &self.cfg);
        match self.permission {
            Permission::Authorized => Ok(vec![self.native(ev.kind.clone(), n)]),
            Permission::Unknown => {
                self.permission = Permission::Requesting;
                self.park(ev.kind.clone(), n);
                Ok(vec![Action::RequestPermission])
            }
            Permission::Requesting => {
                self.park(ev.kind.clone(), n);
                Ok(Vec::new())
            }
            Permission::Unavailable(_) => Ok(self.fallback(n)),
            Permission::Denied => Err(Suppressed::Denied),
        }
    }

    /// The backend answered the permission request (or changed its mind).
    /// Parked events are re-checked against the switches in force NOW: one
    /// the owner turned off while the prompt was up is dropped, not posted.
    /// RC2-R64: what macOS says Awili's permission is now (asked at launch
    /// and when Settings opens), so the page reports the real state. While
    /// the macOS prompt is up its answer decides, and a build that cannot
    /// post natively stays so; otherwise the report is taken like an
    /// answer (an event that waited posts once allowed).
    pub fn current_permission(&mut self, p: Permission) -> Vec<Action> {
        match (&self.permission, &p) {
            (Permission::Requesting | Permission::Unavailable(_), _) => Vec::new(),
            (_, Permission::Unavailable(_) | Permission::Requesting) => Vec::new(),
            _ => self.permission_changed(p),
        }
    }

    pub fn permission_changed(&mut self, p: Permission) -> Vec<Action> {
        self.permission = p;
        let pending: Vec<(EventKind, Notification)> = std::mem::take(&mut self.pending)
            .into_iter()
            .filter(|(kind, _)| self.still_wanted(kind))
            .collect();
        match self.permission {
            Permission::Authorized => pending
                .into_iter()
                .map(|(kind, n)| self.native(kind, n))
                .collect(),
            Permission::Unavailable(_) => pending
                .into_iter()
                .flat_map(|(_, n)| self.fallback(n))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A native post failed: native is unavailable from now on, and this one
    /// goes through the fallback only if the kernel still knows that exact
    /// post (by its token) and its kind is still switched on. Fail closed:
    /// a denial, a switched-off kind, or a post it no longer (or never)
    /// recorded posts nothing.
    pub fn post_failed(&mut self, n: Notification) -> Vec<Action> {
        if self.permission == Permission::Denied {
            return Vec::new();
        }
        self.permission = Permission::Unavailable("macOS could not post a native notification");
        let kind = self
            .sent
            .iter()
            .position(|(post, _)| *post == n.post)
            .and_then(|i| self.sent.remove(i))
            .map(|(_, kind)| kind);
        match kind {
            Some(kind) if self.still_wanted(&kind) => self.fallback(n),
            _ => Vec::new(),
        }
    }

    /// The permission state (Settings shows "open System Settings" on Denied).
    pub fn permission(&self) -> &Permission {
        &self.permission
    }

    /// The Settings status text (§3 table).
    pub fn status_line(&self) -> &'static str {
        match self.permission {
            Permission::Unknown => "macOS will ask the first time something needs you",
            Permission::Requesting => "waiting for your answer in the macOS prompt",
            Permission::Authorized => "allowed",
            Permission::Denied => "off for Awili in System Settings",
            Permission::Unavailable(UNSUPPORTED) => UNSUPPORTED,
            Permission::Unavailable(_) if self.cfg.fallback_osascript => {
                "native notifications unavailable \u{2014} using Script Editor fallback"
            }
            Permission::Unavailable(_) => "native notifications unavailable",
        }
    }

    fn kind_on(&self, kind: &EventKind) -> bool {
        match kind {
            EventKind::Approval { .. } | EventKind::Question | EventKind::Credential { .. } => {
                self.cfg.approvals
            }
            EventKind::Finished => self.cfg.finished,
            EventKind::Failed | EventKind::Stopped => self.cfg.failed,
            EventKind::Test => true,
        }
    }

    /// Step 6: never notify for what the owner is looking at; a finish also
    /// needs the run (or the time away) to be long enough.
    fn presence_gate(
        &self,
        ev: &Event,
        visible: Option<&Target>,
        idle_secs: Option<u64>,
        now: Instant,
    ) -> Result<(), Suppressed> {
        let idle_limit = self.cfg.effective_idle_away_secs();
        let idle = idle_secs.unwrap_or(0);
        let away = !self.window_active || (idle_limit > 0 && idle >= idle_limit);
        if !away {
            if !ev.kind.is_waiting() {
                return Err(Suppressed::Present);
            }
            if visible == Some(&ev.target) {
                return Err(Suppressed::Visible);
            }
            return Ok(());
        }
        if matches!(ev.kind, EventKind::Finished) {
            let inactive_for = match (self.window_active, self.inactive_since) {
                (false, Some(t)) => now.saturating_duration_since(t),
                _ => Duration::ZERO,
            };
            let away_for = inactive_for.max(Duration::from_secs(idle));
            let min = Duration::from_secs(self.cfg.effective_min_run_secs());
            if ev.elapsed.unwrap_or(away_for) < min {
                return Err(Suppressed::TooQuick);
            }
        }
        Ok(())
    }

    /// Queue a notification while permission is asked: the same id replaces
    /// the older one; beyond the cap the oldest is dropped.
    fn park(&mut self, kind: EventKind, n: Notification) {
        self.pending.retain(|(_, p)| p.id != n.id);
        if self.pending.len() >= PENDING_CAP {
            self.pending.remove(0);
        }
        self.pending.push((kind, n));
    }

    /// A native post: stamped with a fresh post token and remembered with its
    /// kind (posts that share an id stay distinct).
    fn native(&mut self, kind: EventKind, mut n: Notification) -> Action {
        self.last_post += 1;
        n.post = self.last_post;
        if self.sent.len() >= SENT_CAP {
            self.sent.pop_front();
        }
        self.sent.push_back((n.post, kind));
        Action::PostNative(n)
    }

    /// Steps 2–3 of `handle` against the config in force now (the Settings
    /// test ignores both).
    fn still_wanted(&self, kind: &EventKind) -> bool {
        *kind == EventKind::Test || (self.cfg.enabled && self.kind_on(kind))
    }

    fn fallback(&self, n: Notification) -> Vec<Action> {
        if self.cfg.fallback_osascript {
            vec![Action::PostFallback(n)]
        } else {
            Vec::new()
        }
    }
}

/// `awili:v1:<surface>:<target>`: the notification id, thread id and route.
pub fn route_id(t: &Target) -> String {
    match t {
        Target::Work(id) => format!("awili:v1:work:{id}"),
        Target::Code(id) => format!("awili:v1:code:{id}"),
        Target::Test => "awili:v1:test:0".to_string(),
    }
}

/// Strict inverse of [`route_id`]; anything else is `None` (click ignored).
pub fn parse_route(id: &str) -> Option<Target> {
    let (surface, target) = id.strip_prefix("awili:v1:")?.split_once(':')?;
    let conv_ok = !target.is_empty()
        && target.len() <= 64
        && target
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    let digits = !target.is_empty() && target.bytes().all(|b| b.is_ascii_digit());
    match surface {
        "work" if conv_ok => Some(Target::Work(target.to_string())),
        // Chat was consolidated into Workshop. Continue accepting previously
        // posted click routes so old native notifications still open the
        // conversation, but never emit a Chat route again.
        "chat" if conv_ok => Some(Target::Work(target.to_string())),
        "code" if digits => target.parse().ok().map(Target::Code),
        "test" if target == "0" => Some(Target::Test),
        _ => None,
    }
}

/// The notification text (§2.1 payload table). Only Awili-owned labels.
pub fn payload(ev: &Event, cfg: &NotificationsConfig) -> Notification {
    let label = sanitize(&ev.label, 80);
    let code = matches!(ev.target, Target::Code(_));
    let (title, body) = match &ev.kind {
        // RC2-R69 (usability RC2 round 5, Minor 6: "Approval needed" said only
        // "w1 · command"): the kind is Awili's own words for who wants to do
        // what kind of step where, so it may be longer.
        EventKind::Approval { kind } => (
            "Approval needed",
            format!("{label} \u{b7} {}", sanitize(&kind.replace('_', " "), 80)),
        ),
        EventKind::Question => (
            "Question waiting",
            format!("{label} \u{b7} the agent is asking you something"),
        ),
        EventKind::Credential { sudo } => (
            if *sudo {
                "Password needed"
            } else {
                "Secret needed"
            },
            format!("{label} \u{b7} the agent is waiting for it"),
        ),
        EventKind::Finished => {
            let detail = ev.detail.as_deref().map(|d| sanitize(d, 80));
            let body = match detail.filter(|d| !d.is_empty()) {
                Some(d) => format!("{label} \u{b7} {d}"),
                None => label.clone(),
            };
            let title = if code {
                "Code turn finished"
            } else {
                "Work finished"
            };
            (title, body)
        }
        EventKind::Failed => (
            if code {
                "Code turn failed"
            } else {
                "Work failed"
            },
            format!("{label} \u{b7} open Awili to see why"),
        ),
        EventKind::Stopped => ("Work stopped", label.clone()),
        EventKind::Test => (
            "Awili notifications are on",
            "You'll hear from Awili when an agent needs you or finishes.".to_string(),
        ),
    };
    Notification {
        id: route_id(&ev.target),
        title: title.to_string(),
        body,
        sound: cfg.sound,
        post: 0,
    }
}

/// The osascript fallback shows Script Editor as the sender, so its title
/// names Awili.
pub fn fallback_title(title: &str) -> String {
    format!("Awili \u{2014} {title}")
}

/// The AppleScript for the fallback: `"` and `\` removed from every string.
pub fn osascript_script(n: &Notification) -> String {
    let clean = |s: &str| {
        s.chars()
            .filter(|c| *c != '"' && *c != '\\')
            .collect::<String>()
    };
    format!(
        "display notification \"{}\" with title \"{}\"",
        clean(&n.body),
        clean(&fallback_title(&n.title))
    )
}

/// Control characters and newlines become spaces, whitespace collapses, and
/// the result is at most `max_chars` characters, ending in `…` when cut.
pub fn sanitize(s: &str, max_chars: usize) -> String {
    let spaced: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut cut: String = collapsed
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect();
    cut.push('\u{2026}');
    cut
}

/// The end of a Work run (or Code turn) as an event kind; `None` = silent.
pub fn classify_work_end(
    status: Option<RunStatus>,
    has_error: bool,
    user_stopped: bool,
) -> Option<EventKind> {
    let finished_or_failed = if has_error {
        EventKind::Failed
    } else {
        EventKind::Finished
    };
    match status {
        Some(RunStatus::Completed) => Some(EventKind::Finished),
        Some(RunStatus::Failed) => Some(EventKind::Failed),
        Some(RunStatus::Stopped) if user_stopped => None,
        Some(RunStatus::Stopped) => Some(EventKind::Stopped),
        None | Some(RunStatus::Started | RunStatus::Running | RunStatus::Unknown) => {
            Some(finished_or_failed)
        }
    }
}

/// The adapter's authorization answer (N1b calls this from its completion).
#[cfg_attr(not(test), allow(dead_code))]
pub fn classify_authorization(granted: bool, error_code: Option<isize>) -> Permission {
    match (granted, error_code) {
        (_, Some(_)) => {
            Permission::Unavailable("macOS refused native notifications for this build")
        }
        (true, None) => Permission::Authorized,
        (false, None) => Permission::Denied,
    }
}

/// RC2-R64 (usability RC2 round 5, Minor 7: Settings › Notifications said
/// "macOS will ask the first time something needs you" while macOS already
/// allowed Awili): what macOS's current `UNAuthorizationStatus` means —
/// not asked yet (0), off for Awili (1), allowed (2; provisional 3 and
/// ephemeral 4 post too). Anything else is not known.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn classify_status(status: isize) -> Permission {
    match status {
        1 => Permission::Denied,
        2..=4 => Permission::Authorized,
        _ => Permission::Unknown,
    }
}

#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests;
