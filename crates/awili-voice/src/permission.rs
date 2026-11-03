//! Microphone-permission pre-flight (macOS TCC).
//!
//! `MicCapture::start` blocks inside CoreAudio until the OS microphone
//! permission is resolved — and for a fresh (or rebuilt, ad-hoc-signed) binary
//! that means "until the user answers the permission prompt". A caller on the
//! UI thread must know BEFORE starting a capture whether that block can happen
//! (user-test finding N1: the whole app froze behind the prompt, showing a
//! stale frame). This module reads the authorization state via AVFoundation;
//! when the answer is [`MicAuthStatus::NotDetermined`], the caller fires the
//! prompt from a worker thread instead of the UI thread.

/// The OS microphone authorization state (mirrors `AVAuthorizationStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicAuthStatus {
    /// The user has not been asked yet — starting a capture WILL raise the
    /// blocking permission prompt.
    NotDetermined,
    /// Parental controls / MDM forbid microphone access; no prompt will appear.
    Restricted,
    /// The user said no; no prompt will appear until System Settings changes.
    Denied,
    Authorized,
    /// The state could not be read (non-macOS, or AVFoundation unavailable).
    /// Callers should behave exactly as they did before this module existed —
    /// i.e. start the capture directly.
    Unknown,
}

#[cfg(target_os = "macos")]
pub fn mic_authorization_status() -> MicAuthStatus {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};
    use std::os::raw::c_char;

    // Classes are resolved at runtime; this guarantees AVFoundation is linked
    // into the process so the lookup can succeed.
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    let (Some(device), Some(nsstring)) = (Class::get("AVCaptureDevice"), Class::get("NSString"))
    else {
        return MicAuthStatus::Unknown;
    };
    // AVMediaTypeAudio is the four-char constant string "soun"; building it
    // here avoids binding AVFoundation's extern NSString symbol.
    unsafe {
        let media: *mut Object =
            msg_send![nsstring, stringWithUTF8String: c"soun".as_ptr() as *const c_char];
        if media.is_null() {
            return MicAuthStatus::Unknown;
        }
        let status: isize = msg_send![device, authorizationStatusForMediaType: media];
        match status {
            0 => MicAuthStatus::NotDetermined,
            1 => MicAuthStatus::Restricted,
            2 => MicAuthStatus::Denied,
            3 => MicAuthStatus::Authorized,
            _ => MicAuthStatus::Unknown,
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn mic_authorization_status() -> MicAuthStatus {
    MicAuthStatus::Unknown
}
