//! `ui::motion` — the kit's motion policy: the system Reduce Motion
//! preference (KIT.md v3, batch K1).
//!
//! macOS reads `NSWorkspace.accessibilityDisplayShouldReduceMotion` through
//! objc2 and caches it in an `AtomicBool`, so a paint path never blocks on
//! an ObjC call. The cache is refreshed at launch ([`install`]) and when a
//! loop starts ([`refresh`]) — and live: an `NSNotificationCenter` observer
//! on `NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification` re-reads
//! the setting the moment the user flips it in System Settings.
//!
//! Elsewhere the preference is absent and [`reduce_motion`] is `false`.
//!
//! `#[cfg(test)]` override: tests pin the preference with
//! [`set_test_override`] / [`clear_test_override`] — thread-local, so the
//! concurrent test harness never leaks a preference across tests (the same
//! pattern as `theme`'s thread-local zoom scale).

use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use std::sync::{Mutex, OnceLock};

fn listeners() -> &'static Mutex<Vec<UnboundedSender<bool>>> {
    static LISTENERS: OnceLock<Mutex<Vec<UnboundedSender<bool>>>> = OnceLock::new();
    LISTENERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Subscribe to the same preference change the native observer publishes.
/// Delivery wakes the app's foreground task, including while no clock runs.
pub(crate) fn preference_changes() -> UnboundedReceiver<bool> {
    let (sender, receiver) = mpsc::unbounded();
    listeners().lock().unwrap().push(sender);
    receiver
}

#[cfg(any(target_os = "macos", test))]
fn publish(reduced: bool) {
    listeners()
        .lock()
        .unwrap()
        .retain(|sender| sender.unbounded_send(reduced).is_ok());
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ptr::NonNull;
    use std::sync::{atomic::AtomicBool, atomic::Ordering, Once};

    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSNotification, NSString};

    /// The macOS Reduce Motion setting, cached for paint paths.
    pub(super) static REDUCE: AtomicBool = AtomicBool::new(false);

    // The AppKit notification the OS posts when the accessibility display
    // options (Reduce Motion included) change: a global `NSString` constant.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification: &'static NSString;
    }

    static INSTALL: Once = Once::new();

    /// Read the OS setting into the cache.
    pub(super) fn read_setting() {
        let ws: Retained<NSWorkspace> = NSWorkspace::sharedWorkspace();
        // SAFETY: `accessibilityDisplayShouldReduceMotion` is a `BOOL`
        // property on `NSWorkspace`; `ws` is a valid object.
        let should: Bool = unsafe { msg_send![&*ws, accessibilityDisplayShouldReduceMotion] };
        let reduced = should.as_bool();
        if REDUCE.swap(reduced, Ordering::Relaxed) != reduced {
            super::publish(reduced);
        }
    }

    /// Refresh the cache and register the live-update observer once. The
    /// observer is a block (the same `RcBlock` pattern `notify_macos.rs`
    /// uses); its registration token is kept for the process's life.
    pub(super) fn install() {
        read_setting();
        INSTALL.call_once(|| {
            // Apple posts this notification on NSWorkspace's own notification
            // center, not the default one (Codex K1 review).
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            let block = RcBlock::new(|_note: NonNull<NSNotification>| read_setting());
            // SAFETY: the name is AppKit's exported constant. A nil object
            // observes every poster; a nil queue runs the block on the POSTING
            // thread (not guaranteed main). The block only re-reads an NSWorkspace
            // BOOL property and stores an atomic, both safe off the main thread.
            let token = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification),
                    None,
                    None,
                    &block,
                )
            };
            // Keep the registration alive for the process's life.
            std::mem::forget(token);
        });
    }
}

/// Whether the OS is currently in Reduce Motion.
///
/// macOS: the cached `NSWorkspace` setting (see the module docs). Elsewhere:
/// `false` — there is no preference to read. Under `#[cfg(test)]` a set
/// override wins (thread-local).
pub fn reduce_motion() -> bool {
    #[cfg(test)]
    {
        if let Some(v) = TEST_OVERRIDE.with(|c| c.get()) {
            return v;
        }
    }
    #[cfg(target_os = "macos")]
    {
        mac::REDUCE.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Refresh the cache from the OS and install the live-update observer.
/// Call at launch (the app does, in `main`); no-op off macOS. Idempotent.
pub fn install() {
    #[cfg(target_os = "macos")]
    mac::install();
}

/// Re-read the OS setting. Call when a loop starts (KIT.md: "refreshed at
/// launch and when a loop starts"); no-op off macOS.
pub fn refresh() {
    #[cfg(target_os = "macos")]
    mac::read_setting();
}

#[cfg(test)]
thread_local! {
    static TEST_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Test-only: pin the Reduce Motion preference for this thread.
#[cfg(test)]
pub fn set_test_override(reduce: bool) {
    TEST_OVERRIDE.with(|c| c.set(Some(reduce)));
}

/// Test-only: clear the pinned preference (falls back to the real reading).
#[cfg(test)]
pub fn clear_test_override() {
    TEST_OVERRIDE.with(|c| c.set(None));
}

/// Drive the observer's real delivery path, with no system setting mutation.
#[cfg(test)]
pub(crate) fn publish_test_preference(reduced: bool) {
    set_test_override(reduced);
    publish(reduced);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without an override the preference is off (the default cache value on
    /// macOS; the constant `false` elsewhere).
    #[test]
    fn reduce_motion_defaults_off() {
        clear_test_override();
        assert!(
            !reduce_motion(),
            "the default (no override) is Reduce Motion off"
        );
    }

    /// The test override pins the preference while set, and clearing it falls
    /// back to the real reading.
    #[test]
    fn the_test_override_pins_the_preference() {
        set_test_override(true);
        assert!(reduce_motion(), "the override wins while set");
        clear_test_override();
        assert!(!reduce_motion(), "clearing the override falls back");
    }

    /// The override is thread-local: another thread must not see it (the
    /// test harness runs tests concurrently).
    #[test]
    fn the_override_is_thread_local() {
        set_test_override(true);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(reduce_motion());
        })
        .join()
        .expect("the probe thread panics only on a poisoned channel");
        assert!(
            !rx.recv().expect("the probe thread sent its reading"),
            "another thread must not see this thread's override"
        );
        clear_test_override();
    }
}
