// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::apple::responsible_gui_application::responsible_gui_application_name;
use crate::core::{Error, Result};

/// A capture device macOS gates behind a privacy prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrivacyGatedCaptureDevice {
    Camera,
    Microphone,
}

impl PrivacyGatedCaptureDevice {
    /// The device as a sentence names it mid-way.
    fn lowercase_name(self) -> &'static str {
        match self {
            PrivacyGatedCaptureDevice::Camera => "camera",
            PrivacyGatedCaptureDevice::Microphone => "microphone",
        }
    }

    /// The device as System Settings › Privacy & Security lists it.
    fn privacy_setting_name(self) -> &'static str {
        match self {
            PrivacyGatedCaptureDevice::Camera => "Camera",
            PrivacyGatedCaptureDevice::Microphone => "Microphone",
        }
    }

    /// What starts flowing once the user allows it.
    fn what_starts_once_allowed(self) -> &'static str {
        match self {
            PrivacyGatedCaptureDevice::Camera => "Frames start",
            PrivacyGatedCaptureDevice::Microphone => "Audio starts",
        }
    }

    fn av_media_type(self) -> Option<&'static objc2_av_foundation::AVMediaType> {
        use objc2_av_foundation::{AVMediaTypeAudio, AVMediaTypeVideo};

        // SAFETY: both are AVFoundation-exported constants.
        unsafe {
            match self {
                PrivacyGatedCaptureDevice::Camera => AVMediaTypeVideo,
                PrivacyGatedCaptureDevice::Microphone => AVMediaTypeAudio,
            }
        }
    }
}

/// Where macOS stands on this process's access to a capture device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureDeviceAuthorizationStatus {
    /// The user allowed it.
    Authorized,
    /// Nobody has asked yet.
    NotDetermined,
    /// The user refused it.
    Denied,
    /// A device-management profile or Screen Time forbids it.
    Restricted,
}

/// Called once with the user's answer to a capture-device request: `true` if
/// allowed.
pub(crate) type CaptureDeviceAuthorizationAnswer = Box<dyn FnOnce(bool) + Send>;

/// The system's privacy gate for one capture device.
pub(crate) trait CaptureDeviceAuthorizationAuthority: Send + Sync {
    /// The device this gate guards.
    fn gated_capture_device(&self) -> PrivacyGatedCaptureDevice;

    /// Where access stands right now, without asking anyone.
    fn authorization_status(&self) -> CaptureDeviceAuthorizationStatus;

    /// Ask for access. Returns at once; `answer` runs later, on a thread of the
    /// system's choosing, once the user has answered — which can be never.
    fn request_authorization(&self, answer: CaptureDeviceAuthorizationAnswer);
}

/// AVFoundation's privacy gate for one capture device.
pub(crate) struct AvFoundationCaptureDeviceAuthorizationAuthority(pub PrivacyGatedCaptureDevice);

impl CaptureDeviceAuthorizationAuthority for AvFoundationCaptureDeviceAuthorizationAuthority {
    fn gated_capture_device(&self) -> PrivacyGatedCaptureDevice {
        self.0
    }

    fn authorization_status(&self) -> CaptureDeviceAuthorizationStatus {
        use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice};

        let Some(media_type) = self.0.av_media_type() else {
            return CaptureDeviceAuthorizationStatus::Restricted;
        };
        // SAFETY: a status read, callable from any thread.
        match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) } {
            AVAuthorizationStatus::Authorized => CaptureDeviceAuthorizationStatus::Authorized,
            AVAuthorizationStatus::NotDetermined => CaptureDeviceAuthorizationStatus::NotDetermined,
            AVAuthorizationStatus::Denied => CaptureDeviceAuthorizationStatus::Denied,
            _ => CaptureDeviceAuthorizationStatus::Restricted,
        }
    }

    fn request_authorization(&self, answer: CaptureDeviceAuthorizationAnswer) {
        use objc2::runtime::Bool;
        use objc2_av_foundation::AVCaptureDevice;
        use std::sync::Mutex;

        let Some(media_type) = self.0.av_media_type() else {
            answer(false);
            return;
        };
        // The block is `Fn`, the answer is `FnOnce`: AVFoundation calls the
        // handler once, and the `take` holds that to exactly once.
        let answer = Mutex::new(Some(answer));
        let completion_handler = block2::RcBlock::new(move |granted: Bool| {
            if let Some(answer) = answer.lock().ok().and_then(|mut answer| answer.take()) {
                answer(granted.as_bool());
            }
        });
        // SAFETY: documented not to block while the user is asked, and to run
        // the handler on an arbitrary queue, which the handler is `Send` for.
        unsafe {
            AVCaptureDevice::requestAccessForMediaType_completionHandler(
                media_type,
                &completion_handler,
            )
        };
    }
}

/// Where access stands once a stream has made sure it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureDeviceAuthorizationAtOpen {
    /// The device may be opened now.
    Granted,
    /// The user is being asked; the answer arrives through the callback
    /// handed to [`authorize_the_capture_device_without_waiting_for_the_user`].
    AwaitingTheUsersAnswer,
}

/// Make sure access to the authority's device has been requested, never
/// waiting on the user.
///
/// A request that is still with the user never answers until they do — a
/// caller that waited would look hung with no output — so this returns at
/// once and `on_the_users_answer` runs whenever the answer arrives. A refusal
/// already on record is refused here, naming the application macOS holds
/// responsible and the setting to change.
pub(crate) fn authorize_the_capture_device_without_waiting_for_the_user(
    authority: &dyn CaptureDeviceAuthorizationAuthority,
    on_the_users_answer: CaptureDeviceAuthorizationAnswer,
) -> Result<CaptureDeviceAuthorizationAtOpen> {
    let device = authority.gated_capture_device();
    match authority.authorization_status() {
        CaptureDeviceAuthorizationStatus::Authorized => {
            Ok(CaptureDeviceAuthorizationAtOpen::Granted)
        }
        CaptureDeviceAuthorizationStatus::NotDetermined => {
            let responsible_application = responsible_application_for_the_user();
            tracing::info!(
                responsible_application = %responsible_application,
                "{} access requested: macOS is asking whether {responsible_application} may \
                 use the {}. {} as soon as you allow it.",
                device.privacy_setting_name(),
                device.lowercase_name(),
                device.what_starts_once_allowed(),
            );
            authority.request_authorization(on_the_users_answer);
            Ok(CaptureDeviceAuthorizationAtOpen::AwaitingTheUsersAnswer)
        }
        CaptureDeviceAuthorizationStatus::Denied => Err(Error::Configuration(
            capture_device_refusal_for_the_user(device, CaptureDeviceRefusal::DeniedByTheUser),
        )),
        CaptureDeviceAuthorizationStatus::Restricted => Err(Error::Configuration(
            capture_device_refusal_for_the_user(device, CaptureDeviceRefusal::RestrictedOnThisMac),
        )),
    }
}

/// Why macOS will not let this process use a capture device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureDeviceRefusal {
    /// The user refused it.
    DeniedByTheUser,
    /// A device-management profile or Screen Time forbids it.
    RestrictedOnThisMac,
}

/// Why the device cannot be used, naming the application macOS holds
/// responsible for this process and what to change.
pub(crate) fn capture_device_refusal_for_the_user(
    device: PrivacyGatedCaptureDevice,
    refusal: CaptureDeviceRefusal,
) -> String {
    capture_device_refusal_naming(device, refusal, &responsible_application_for_the_user())
}

fn capture_device_refusal_naming(
    device: PrivacyGatedCaptureDevice,
    refusal: CaptureDeviceRefusal,
    responsible_application: &str,
) -> String {
    let setting = device.privacy_setting_name();
    let lowercase_name = device.lowercase_name();
    match refusal {
        CaptureDeviceRefusal::RestrictedOnThisMac => format!(
            "{setting} access is restricted on this Mac — a device-management profile or Screen \
             Time forbids it — so {responsible_application}, the application macOS asks on \
             this process's behalf, cannot be allowed the {lowercase_name} here."
        ),
        CaptureDeviceRefusal::DeniedByTheUser => format!(
            "{setting} access is denied for {responsible_application}. macOS asks the application \
             that launched this process, not the program running inside it, so turn on \
             {responsible_application} in System Settings › Privacy & Security › {setting} and \
             run again. If {responsible_application} is not in that list it has never asked, and \
             the list cannot add it by hand: run from a different terminal instead."
        ),
    }
}

/// The responsible application's name, or a description a user can act on
/// when it cannot be found.
fn responsible_application_for_the_user() -> String {
    responsible_gui_application_name()
        .unwrap_or_else(|| "the terminal or application this was launched from".to_owned())
}

/// Make sure access to `device` has been requested, returning at once. `true`
/// when it is allowed or the user is being asked; `false`, with the reason
/// logged, when it is refused.
fn request_capture_device_permission(device: PrivacyGatedCaptureDevice) -> Result<bool> {
    match authorize_the_capture_device_without_waiting_for_the_user(
        &AvFoundationCaptureDeviceAuthorizationAuthority(device),
        Box::new(move |granted| {
            if !granted {
                tracing::error!(
                    "{}",
                    capture_device_refusal_for_the_user(
                        device,
                        CaptureDeviceRefusal::DeniedByTheUser
                    )
                );
            }
        }),
    ) {
        Ok(_) => Ok(true),
        Err(refusal) => {
            tracing::error!(%refusal, "{} permission refused", device.lowercase_name());
            Ok(false)
        }
    }
}

/// Make sure camera access has been requested, returning at once. `true` when
/// the camera is allowed or the user is being asked; `false`, with the reason
/// logged, when it is refused.
pub fn request_camera_permission() -> Result<bool> {
    request_capture_device_permission(PrivacyGatedCaptureDevice::Camera)
}

pub fn request_display_permission() -> Result<bool> {
    tracing::info!("Display permission granted (no system prompt required on macOS)");
    Ok(true)
}

/// Make sure microphone access has been requested, returning at once. `true`
/// when the microphone is allowed or the user is being asked; `false`, with the
/// reason logged, when it is refused.
pub fn request_audio_permission() -> Result<bool> {
    request_capture_device_permission(PrivacyGatedCaptureDevice::Microphone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    /// A privacy gate whose user never answers: every request is kept and
    /// none is ever called back — what a prompt nobody has clicked looks like.
    struct AUserWhoNeverAnswers {
        status: CaptureDeviceAuthorizationStatus,
        requests: AtomicUsize,
        unanswered: Mutex<Vec<CaptureDeviceAuthorizationAnswer>>,
    }

    impl AUserWhoNeverAnswers {
        fn with_status(status: CaptureDeviceAuthorizationStatus) -> Self {
            Self {
                status,
                requests: AtomicUsize::new(0),
                unanswered: Mutex::new(Vec::new()),
            }
        }
    }

    impl CaptureDeviceAuthorizationAuthority for AUserWhoNeverAnswers {
        fn gated_capture_device(&self) -> PrivacyGatedCaptureDevice {
            PrivacyGatedCaptureDevice::Camera
        }

        fn authorization_status(&self) -> CaptureDeviceAuthorizationStatus {
            self.status
        }

        fn request_authorization(&self, answer: CaptureDeviceAuthorizationAnswer) {
            self.requests.fetch_add(1, Ordering::SeqCst);
            self.unanswered.lock().expect("unpoisoned").push(answer);
        }
    }

    /// The measured hazard: a pending request never answers, so a caller that
    /// waited on it would hang with no output. Run on its own thread with a
    /// bound, so waiting fails the test rather than hanging it.
    #[test]
    fn a_pending_request_is_made_once_and_never_waited_on() {
        let authority = Arc::new(AUserWhoNeverAnswers::with_status(
            CaptureDeviceAuthorizationStatus::NotDetermined,
        ));
        let (returned, returned_with) = mpsc::channel();
        let asking_authority = Arc::clone(&authority);
        std::thread::spawn(move || {
            let at_open = authorize_the_capture_device_without_waiting_for_the_user(
                asking_authority.as_ref(),
                Box::new(|_granted| {}),
            );
            let _ = returned.send(at_open.map_err(|e| e.to_string()));
        });
        let at_open = returned_with
            .recv_timeout(Duration::from_secs(5))
            .expect("asking for the camera waited on a user who never answers");
        assert_eq!(
            at_open,
            Ok(CaptureDeviceAuthorizationAtOpen::AwaitingTheUsersAnswer)
        );
        assert_eq!(authority.requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_authorized_camera_is_opened_without_asking_again() {
        let authority =
            AUserWhoNeverAnswers::with_status(CaptureDeviceAuthorizationStatus::Authorized);
        assert_eq!(
            authorize_the_capture_device_without_waiting_for_the_user(&authority, Box::new(|_| {}))
                .map_err(|e| e.to_string()),
            Ok(CaptureDeviceAuthorizationAtOpen::Granted)
        );
        assert_eq!(authority.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_denied_camera_is_refused_without_asking_again() {
        let authority = AUserWhoNeverAnswers::with_status(CaptureDeviceAuthorizationStatus::Denied);
        assert!(
            authorize_the_capture_device_without_waiting_for_the_user(&authority, Box::new(|_| {}))
                .is_err()
        );
        assert_eq!(authority.requests.load(Ordering::SeqCst), 0);
    }

    /// The user acts on this text in System Settings, where the entry is the
    /// terminal's — never the interpreter's.
    #[test]
    fn a_denial_names_the_responsible_application_and_the_setting_but_not_python() {
        let refusal = capture_device_refusal_naming(
            PrivacyGatedCaptureDevice::Camera,
            CaptureDeviceRefusal::DeniedByTheUser,
            "iTerm",
        );
        assert!(refusal.contains("iTerm"), "{refusal}");
        assert!(
            refusal.contains("System Settings › Privacy & Security › Camera"),
            "{refusal}"
        );
        assert!(refusal.contains("different terminal"), "{refusal}");
        assert!(!refusal.to_lowercase().contains("python"), "{refusal}");
    }

    #[test]
    fn a_restriction_says_the_camera_cannot_be_allowed_here() {
        let refusal = capture_device_refusal_naming(
            PrivacyGatedCaptureDevice::Camera,
            CaptureDeviceRefusal::RestrictedOnThisMac,
            "iTerm",
        );
        assert!(refusal.contains("restricted"), "{refusal}");
        assert!(refusal.contains("iTerm"), "{refusal}");
    }

    /// The microphone's refusal points at the microphone's own list, which is
    /// a different page from the camera's.
    #[test]
    fn a_microphone_denial_names_the_microphone_setting_and_not_the_cameras() {
        let refusal = capture_device_refusal_naming(
            PrivacyGatedCaptureDevice::Microphone,
            CaptureDeviceRefusal::DeniedByTheUser,
            "iTerm",
        );
        assert!(refusal.contains("iTerm"), "{refusal}");
        assert!(
            refusal.contains("System Settings › Privacy & Security › Microphone"),
            "{refusal}"
        );
        assert!(!refusal.contains("Camera"), "{refusal}");
        assert!(!refusal.to_lowercase().contains("python"), "{refusal}");
    }
}
