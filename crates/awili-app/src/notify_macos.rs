//! N1b: the native macOS notification backend
//! (`docs/daily-driver/specs/N1-macos-notifications.md` §8 N1b). Declared
//! inside `notify_platform.rs` behind `#[cfg(target_os = "macos")]`.
//!
//! `UNUserNotificationCenter` aborts the process with an uncaught ObjC
//! exception outside an app bundle (`cargo test`, `cargo run`), so nothing here
//! touches a UN symbol until [`bundled`] has said yes. Completion blocks run
//! off the main thread: they only send on the signal channel, never touch gpui.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread};
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSettings, UNNotificationSound,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use super::{post_error_signal, signals, BackendSignal, NotifyBackend, OsascriptBackend};
use crate::notifications::{classify_authorization, classify_status, Notification, Permission};

/// Why native posting is off in a process that is not an app bundle.
const UNBUNDLED: &str = "not running from an app bundle";

/// True only inside an app bundle (a bundle id AND a path ending in `.app`).
/// No UN symbol is touched before this returns true.
pub(super) fn bundled() -> bool {
    let bundle = NSBundle::mainBundle();
    bundle.bundleIdentifier().is_some() && bundle.bundlePath().to_string().ends_with(".app")
}

/// What the delegate lets a notification do while Awili is frontmost: show as
/// a banner and in the list, with its sound when it has one — so a waiting
/// event for a hidden conversation still shows (§5).
pub(super) fn presentation_options(sound: bool) -> UNNotificationPresentationOptions {
    let shown = UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List;
    if sound {
        shown | UNNotificationPresentationOptions::Sound
    } else {
        shown
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and this class adds
    // no ivars and does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "AwiliNotifyDelegate"]
    struct AwiliNotifyDelegate;

    unsafe impl NSObjectProtocol for AwiliNotifyDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for AwiliNotifyDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion_handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let sound = notification.request().content().sound().is_some();
            completion_handler.call((presentation_options(sound),));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &block2::DynBlock<dyn Fn()>,
        ) {
            // Only the default action (a click on the notification) routes.
            // SAFETY: a constant NSString exported by UserNotifications.
            let default = unsafe { UNNotificationDefaultActionIdentifier };
            if response.actionIdentifier().isEqualToString(default) {
                let id = response.notification().request().identifier().to_string();
                let _ = signals().0.try_send(BackendSignal::Clicked(id));
            }
            completion_handler.call(());
        }
    }
);

impl AwiliNotifyDelegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// From `main()`, before the app finishes launching (so the click that
/// cold-launches Awili is delivered): install the delegate — bundled only.
/// `setDelegate` holds a weak reference, so the delegate is leaked on purpose
/// and lives for the whole process. This never prompts.
pub(super) fn install_early() {
    if !bundled() {
        return;
    }
    let delegate = AwiliNotifyDelegate::new();
    UNUserNotificationCenter::currentNotificationCenter()
        .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let _ = Retained::into_raw(delegate);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
}

/// `kCGEventSourceStateHIDSystemState`.
const HID_SYSTEM_STATE: i32 = 1;
/// `kCGAnyInputEventType`.
const ANY_INPUT_EVENT: u32 = u32::MAX;

/// The native backend: UNUserNotificationCenter in a bundle, and `Unavailable`
/// (so the kernel falls back to osascript) anywhere else.
pub(super) struct MacBackend {
    bundled: bool,
}

impl MacBackend {
    pub(super) fn new() -> Self {
        Self { bundled: bundled() }
    }
}

impl NotifyBackend for MacBackend {
    fn initial_permission(&self) -> Permission {
        if self.bundled {
            Permission::Unknown
        } else {
            Permission::Unavailable(UNBUNDLED)
        }
    }

    fn request_permission(&mut self) {
        if !self.bundled {
            let _ = signals()
                .0
                .try_send(BackendSignal::Permission(Permission::Unavailable(
                    UNBUNDLED,
                )));
            return;
        }
        let done = RcBlock::new(|granted: Bool, error: *mut NSError| {
            // SAFETY: UN passes either null or a valid NSError.
            let code = unsafe { error.as_ref() }.map(|e| e.code());
            let answer = classify_authorization(granted.as_bool(), code);
            let _ = signals().0.try_send(BackendSignal::Permission(answer));
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &done,
            );
    }

    fn query_permission(&mut self) {
        if !self.bundled {
            return;
        }
        let done = RcBlock::new(|settings: std::ptr::NonNull<UNNotificationSettings>| {
            // SAFETY: UN passes a valid settings object for the call.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            let _ = signals()
                .0
                .try_send(BackendSignal::Current(classify_status(status.0)));
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .getNotificationSettingsWithCompletionHandler(&done);
    }

    fn post_native(&mut self, n: &Notification) {
        if !self.bundled {
            let _ = signals().0.try_send(BackendSignal::PostFailed(n.clone()));
            return;
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&n.title));
        content.setBody(&NSString::from_str(&n.body));
        content.setThreadIdentifier(&NSString::from_str(&n.id));
        if n.sound {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&n.id),
            &content,
            None,
        );
        let failed = n.clone();
        let done = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: UN passes either null or a valid NSError.
            if let Some(error) = unsafe { error.as_ref() } {
                let domain = error.domain().to_string();
                let signal = post_error_signal(failed.clone(), &domain, error.code());
                let _ = signals().0.try_send(signal);
            }
        });
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, Some(&done));
    }

    fn post_fallback(&mut self, n: &Notification) {
        OsascriptBackend.post_fallback(n);
    }

    fn idle_secs(&self) -> Option<u64> {
        // SAFETY: a pure CoreGraphics query with constant arguments.
        let secs =
            unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT) };
        (secs.is_finite() && secs >= 0.0).then_some(secs as u64)
    }

    fn open_system_settings(&self) {
        OsascriptBackend.open_system_settings();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// N1 test 39: in the unbundled `cargo test` binary the backend reports
    /// `Unavailable` and `install_early` is a no-op — neither touches the
    /// notification center (which would abort this process).
    #[test]
    fn unbundled_test_process_is_unavailable_without_touching_center() {
        assert!(!bundled(), "cargo test is not an app bundle");
        let backend = MacBackend::new();
        assert_eq!(
            backend.initial_permission(),
            Permission::Unavailable(UNBUNDLED)
        );
        install_early();
    }

    /// N1 test 40: the HID idle time is readable and sane.
    #[test]
    fn idle_seconds_is_readable() {
        let idle = MacBackend::new().idle_secs().expect("idle time");
        assert!(idle < 10 * 365 * 86_400, "{idle}");
    }

    /// N1 test 41: the delegate presents as banner + list, with sound only
    /// when the notification has one.
    #[test]
    fn presentation_options_are_banner_list_sound() {
        let shown =
            UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List;
        assert_eq!(
            presentation_options(true),
            shown | UNNotificationPresentationOptions::Sound
        );
        assert_eq!(presentation_options(false), shown);
    }

    /// N1a-FIX1k: the constants `post_error_signal` matches are the SDK's, and
    /// a real `NSError` in `UNErrorDomain` with code NotificationsNotAllowed,
    /// read the way the post completion reads it, is a denial.
    #[test]
    fn not_allowed_nserror_reads_as_denial() {
        use objc2_foundation::NSError;
        use objc2_user_notifications::{UNErrorCode, UNErrorDomain};
        // SAFETY: a constant NSString exported by UserNotifications.
        let domain = unsafe { UNErrorDomain }.expect("UNErrorDomain");
        assert_eq!(domain.to_string(), super::super::UN_ERROR_DOMAIN);
        assert_eq!(
            UNErrorCode::NotificationsNotAllowed.0,
            super::super::UN_NOT_ALLOWED
        );
        let note = Notification {
            id: "awili:v1:test:0".into(),
            title: "t".into(),
            body: "b".into(),
            sound: false,
            post: 7,
        };
        // SAFETY: a plain NSError with no userInfo.
        let error = |code| unsafe { NSError::errorWithDomain_code_userInfo(domain, code, None) };
        let not_allowed = error(UNErrorCode::NotificationsNotAllowed.0);
        assert_eq!(
            post_error_signal(
                note.clone(),
                &not_allowed.domain().to_string(),
                not_allowed.code()
            ),
            BackendSignal::PostRefused(note.clone())
        );
        let invalid = error(UNErrorCode::NotificationInvalidNoContent.0);
        assert_eq!(
            post_error_signal(note.clone(), &invalid.domain().to_string(), invalid.code()),
            BackendSignal::PostFailed(note)
        );
    }
}
