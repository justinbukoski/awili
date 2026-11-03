//! N1 notification backends (`docs/daily-driver/specs/N1-macos-notifications.md`
//! §2.2). The kernel decides; a backend only asks for permission, posts, and
//! reports back through the global [`signals`] channel. Tests never reach a
//! real backend: under `cfg(test)` the app always gets [`RecordingBackend`].

use std::sync::OnceLock;

use crate::notifications::{osascript_script, Notification, Permission, UNSUPPORTED};

/// N1b: the native UNUserNotificationCenter adapter, the click delegate and
/// the HID idle clock.
#[cfg(target_os = "macos")]
#[path = "notify_macos.rs"]
mod macos;

/// What a backend reports back to the app (drained on the UI thread by
/// `AwiliApp::start_notifications`). Only N1b's native adapter sends these.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendSignal {
    /// The answer to a permission request.
    Permission(Permission),
    /// RC2-R64: macOS's current permission for Awili (asked, never
    /// prompted).
    Current(Permission),
    /// The owner clicked a notification (its id, which is the route).
    Clicked(String),
    /// A native post failed; it goes through the fallback instead.
    PostFailed(Notification),
    /// RC2-FIXC2 (RC2-FIXB review P2: a test said "sent" stayed so after
    /// macOS refused it): macOS refused this native post because Awili's
    /// notifications are not allowed. It reads as the owner's denial
    /// ([`Permission::Denied`]) and names the post it refused.
    PostRefused(Notification),
}

/// One notification backend.
pub trait NotifyBackend {
    /// The launch-time permission: `Unavailable(..)` for Noop and Osascript,
    /// `Unknown` for the native adapter.
    fn initial_permission(&self) -> Permission;
    /// Ask macOS; the answer arrives as [`BackendSignal::Permission`].
    fn request_permission(&mut self);
    /// RC2-R64: ask macOS what Awili's permission is now, without
    /// prompting; the answer arrives as [`BackendSignal::Current`].
    fn query_permission(&mut self);
    fn post_native(&mut self, n: &Notification);
    fn post_fallback(&mut self, n: &Notification);
    /// Seconds since the last HID input, when the platform can tell.
    fn idle_secs(&self) -> Option<u64>;
    fn open_system_settings(&self);
    /// What a recording backend saw; `None` for every real backend.
    #[cfg(test)]
    fn recorded(&self) -> Option<Vec<Recorded>> {
        None
    }
}

/// `UNErrorDomain` (UserNotifications/UNError.h).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const UN_ERROR_DOMAIN: &str = "UNErrorDomain";
/// `UNErrorCodeNotificationsNotAllowed`: the app is not authorized.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const UN_NOT_ALLOWED: isize = 1;

/// What a native post's error means (N1a-FIX1k). "Notifications are not
/// allowed" from UserNotifications is the owner's denial (permission revoked
/// after an earlier Allow): it reads as `Denied`, never as a reason to fall
/// back to Script Editor. Any other error is a failed post.
///
/// RC2-FIXC2: the denial names the post macOS refused
/// ([`BackendSignal::PostRefused`]), so a test notification can say so.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn post_error_signal(n: Notification, domain: &str, code: isize) -> BackendSignal {
    if domain == UN_ERROR_DOMAIN && code == UN_NOT_ALLOWED {
        BackendSignal::PostRefused(n)
    } else {
        BackendSignal::PostFailed(n)
    }
}

/// The process-wide signal channel (unbounded).
pub fn signals() -> &'static (
    async_channel::Sender<BackendSignal>,
    async_channel::Receiver<BackendSignal>,
) {
    static SIGNALS: OnceLock<(
        async_channel::Sender<BackendSignal>,
        async_channel::Receiver<BackendSignal>,
    )> = OnceLock::new();
    SIGNALS.get_or_init(async_channel::unbounded)
}

/// Called first thing in `main()`: on macOS, installs the UN delegate when
/// running from an app bundle (a no-op anywhere else). Never prompts.
pub fn install_early() {
    #[cfg(target_os = "macos")]
    macos::install_early();
}

/// The backend for this build: the recorder under test (below), else the
/// native adapter on macOS and nothing elsewhere.
#[cfg(not(test))]
pub fn default_backend() -> Box<dyn NotifyBackend> {
    if cfg!(target_os = "macos") {
        platform_backend()
    } else {
        Box::new(NoopBackend)
    }
}

/// macOS: the native adapter (which falls back to osascript itself).
#[cfg(all(not(test), target_os = "macos"))]
fn platform_backend() -> Box<dyn NotifyBackend> {
    Box::new(macos::MacBackend::new())
}

/// Other platforms never call this (see `default_backend`); it keeps the
/// osascript backend type-checked there.
#[cfg(all(not(test), not(target_os = "macos")))]
fn platform_backend() -> Box<dyn NotifyBackend> {
    Box::new(OsascriptBackend)
}

/// A platform with no notification support.
#[cfg_attr(test, allow(dead_code))]
pub struct NoopBackend;

impl NotifyBackend for NoopBackend {
    fn initial_permission(&self) -> Permission {
        Permission::Unavailable(UNSUPPORTED)
    }
    fn request_permission(&mut self) {}
    fn query_permission(&mut self) {}
    fn post_native(&mut self, _n: &Notification) {}
    fn post_fallback(&mut self, _n: &Notification) {}
    fn idle_secs(&self) -> Option<u64> {
        None
    }
    fn open_system_settings(&self) {}
}

/// The Script Editor fallback (`osascript`): used by the native adapter when
/// native posting is unavailable (never after a denial). Script Editor is the
/// sender, so titles carry "Awili — ".
#[cfg_attr(test, allow(dead_code))]
pub struct OsascriptBackend;

impl NotifyBackend for OsascriptBackend {
    fn initial_permission(&self) -> Permission {
        Permission::Unavailable("native notifications need the app bundle")
    }
    fn request_permission(&mut self) {}
    fn query_permission(&mut self) {}
    fn post_native(&mut self, n: &Notification) {
        self.post_fallback(n);
    }
    fn post_fallback(&mut self, n: &Notification) {
        if cfg!(test) {
            panic!("tests must never reach osascript");
        }
        let _ = std::process::Command::new("osascript")
            .arg("-e")
            .arg(osascript_script(n))
            .spawn();
    }
    fn idle_secs(&self) -> Option<u64> {
        None
    }
    fn open_system_settings(&self) {
        if cfg!(target_os = "macos") {
            let _ = open::that_detached(
                "x-apple.systempreferences:com.apple.Notifications-Settings.extension",
            );
        }
    }
}

/// What the test backend saw.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    Native(Notification),
    Fallback(Notification),
    RequestPermission,
    OpenSettings,
}

/// The backend for a test build: always the recorder, so no test can reach
/// Notification Center or osascript.
#[cfg(test)]
pub fn default_backend() -> Box<dyn NotifyBackend> {
    Box::<RecordingBackend>::default()
}

/// The only backend a test can get: it records, and never posts.
#[cfg(test)]
#[derive(Default)]
pub struct RecordingBackend {
    log: std::sync::Mutex<Vec<Recorded>>,
}

#[cfg(test)]
impl RecordingBackend {
    fn push(&self, r: Recorded) {
        self.log.lock().unwrap().push(r);
    }
}

#[cfg(test)]
impl NotifyBackend for RecordingBackend {
    fn initial_permission(&self) -> Permission {
        Permission::Unknown
    }
    fn request_permission(&mut self) {
        self.push(Recorded::RequestPermission);
    }
    /// RC2-R64: counted by `NotifyState` (the log stays posts and prompts).
    fn query_permission(&mut self) {}
    fn post_native(&mut self, n: &Notification) {
        self.push(Recorded::Native(n.clone()));
    }
    fn post_fallback(&mut self, n: &Notification) {
        self.push(Recorded::Fallback(n.clone()));
    }
    fn idle_secs(&self) -> Option<u64> {
        None
    }
    fn open_system_settings(&self) {
        self.push(Recorded::OpenSettings);
    }
    fn recorded(&self) -> Option<Vec<Recorded>> {
        Some(self.log.lock().unwrap().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note() -> Notification {
        Notification {
            id: "awili:v1:test:0".into(),
            title: "t".into(),
            body: "b".into(),
            sound: false,
            post: 0,
        }
    }

    /// N1a-FIX1g: the factory a test build gets is the recorder — it asks
    /// nothing of macOS (`Unknown`, where osascript and noop say
    /// `Unavailable`) and records every call instead of posting.
    #[test]
    fn default_backend_under_test_is_the_recorder() {
        let mut b = default_backend();
        assert_eq!(b.initial_permission(), Permission::Unknown);
        b.request_permission();
        b.post_native(&note());
        b.post_fallback(&note());
        b.open_system_settings();
        assert_eq!(
            b.recorded(),
            Some(vec![
                Recorded::RequestPermission,
                Recorded::Native(note()),
                Recorded::Fallback(note()),
                Recorded::OpenSettings,
            ])
        );
    }

    /// N1a-FIX1g: the osascript backend refuses to run in a test build, so a
    /// test that reached it would fail instead of posting to the desktop.
    #[test]
    #[should_panic(expected = "tests must never reach osascript")]
    fn osascript_backend_refuses_to_run_under_test() {
        OsascriptBackend.post_fallback(&note());
    }

    /// N1a-FIX1k: UserNotifications' "not allowed" error on a post is the
    /// owner's denial; every other error (another UN code, another domain)
    /// is a failed post.
    #[test]
    fn post_error_not_allowed_reads_as_denial() {
        assert_eq!(
            post_error_signal(note(), UN_ERROR_DOMAIN, UN_NOT_ALLOWED),
            BackendSignal::PostRefused(note())
        );
        for (domain, code) in [
            (UN_ERROR_DOMAIN, 100),
            (UN_ERROR_DOMAIN, 1401),
            ("NSCocoaErrorDomain", UN_NOT_ALLOWED),
        ] {
            assert_eq!(
                post_error_signal(note(), domain, code),
                BackendSignal::PostFailed(note()),
                "{domain} {code}"
            );
        }
    }
}
