// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::apple::responsible_gui_application::responsible_gui_application_name;
use crate::core::{Error, Result};
use objc2::MainThreadMarker;

/// Where macOS stands on this process's access to the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CameraAuthorizationStatus {
    /// The user allowed it.
    Authorized,
    /// Nobody has asked yet.
    NotDetermined,
    /// The user refused it.
    Denied,
    /// A device-management profile or Screen Time forbids it.
    Restricted,
}

/// Called once with the user's answer to a camera request: `true` if allowed.
pub(crate) type CameraAuthorizationAnswer = Box<dyn FnOnce(bool) + Send>;

/// The system's camera privacy gate.
pub(crate) trait CameraAuthorizationAuthority: Send + Sync {
    /// Where access stands right now, without asking anyone.
    fn camera_authorization_status(&self) -> CameraAuthorizationStatus;

    /// Ask for access. Returns at once; `answer` runs later, on a thread of the
    /// system's choosing, once the user has answered — which can be never.
    fn request_camera_authorization(&self, answer: CameraAuthorizationAnswer);
}

/// AVFoundation's camera privacy gate.
pub(crate) struct AvFoundationCameraAuthorizationAuthority;

impl CameraAuthorizationAuthority for AvFoundationCameraAuthorizationAuthority {
    fn camera_authorization_status(&self) -> CameraAuthorizationStatus {
        use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeVideo};

        // SAFETY: `AVMediaTypeVideo` is an AVFoundation-exported constant.
        let Some(video) = (unsafe { AVMediaTypeVideo }) else {
            return CameraAuthorizationStatus::Restricted;
        };
        // SAFETY: a status read, callable from any thread.
        match unsafe { AVCaptureDevice::authorizationStatusForMediaType(video) } {
            AVAuthorizationStatus::Authorized => CameraAuthorizationStatus::Authorized,
            AVAuthorizationStatus::NotDetermined => CameraAuthorizationStatus::NotDetermined,
            AVAuthorizationStatus::Denied => CameraAuthorizationStatus::Denied,
            _ => CameraAuthorizationStatus::Restricted,
        }
    }

    fn request_camera_authorization(&self, answer: CameraAuthorizationAnswer) {
        use objc2::runtime::Bool;
        use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeVideo};
        use std::sync::Mutex;

        // SAFETY: `AVMediaTypeVideo` is an AVFoundation-exported constant.
        let Some(video) = (unsafe { AVMediaTypeVideo }) else {
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
            AVCaptureDevice::requestAccessForMediaType_completionHandler(video, &completion_handler)
        };
    }
}

/// Where camera access stands once a stream has made sure it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CameraAuthorizationAtOpen {
    /// The camera may be opened now.
    Granted,
    /// The user is being asked; the answer arrives through the callback
    /// handed to [`authorize_the_camera_without_waiting_for_the_user`].
    AwaitingTheUsersAnswer,
}

/// Make sure camera access has been requested, never waiting on the user.
///
/// A request that is still with the user never answers until they do — a
/// caller that waited would look hung with no output — so this returns at
/// once and `on_the_users_answer` runs whenever the answer arrives. A refusal
/// already on record is refused here, naming the application macOS holds
/// responsible and the setting to change.
pub(crate) fn authorize_the_camera_without_waiting_for_the_user(
    authority: &dyn CameraAuthorizationAuthority,
    on_the_users_answer: CameraAuthorizationAnswer,
) -> Result<CameraAuthorizationAtOpen> {
    match authority.camera_authorization_status() {
        CameraAuthorizationStatus::Authorized => Ok(CameraAuthorizationAtOpen::Granted),
        CameraAuthorizationStatus::NotDetermined => {
            let responsible_application = responsible_application_for_the_user();
            tracing::info!(
                responsible_application = %responsible_application,
                "camera access requested: macOS is asking whether {responsible_application} may \
                 use the camera. Frames start as soon as you allow it."
            );
            authority.request_camera_authorization(on_the_users_answer);
            Ok(CameraAuthorizationAtOpen::AwaitingTheUsersAnswer)
        }
        CameraAuthorizationStatus::Denied => Err(Error::Configuration(
            camera_refusal_for_the_user(CameraRefusal::DeniedByTheUser),
        )),
        CameraAuthorizationStatus::Restricted => Err(Error::Configuration(
            camera_refusal_for_the_user(CameraRefusal::RestrictedOnThisMac),
        )),
    }
}

/// Why macOS will not let this process use the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CameraRefusal {
    /// The user refused it.
    DeniedByTheUser,
    /// A device-management profile or Screen Time forbids it.
    RestrictedOnThisMac,
}

/// Why the camera cannot be used, naming the application macOS holds
/// responsible for this process and what to change.
pub(crate) fn camera_refusal_for_the_user(refusal: CameraRefusal) -> String {
    camera_refusal_naming(refusal, &responsible_application_for_the_user())
}

fn camera_refusal_naming(refusal: CameraRefusal, responsible_application: &str) -> String {
    match refusal {
        CameraRefusal::RestrictedOnThisMac => format!(
            "Camera access is restricted on this Mac — a device-management profile or Screen \
             Time forbids it — so {responsible_application}, the application macOS asks on \
             this process's behalf, cannot be allowed the camera here."
        ),
        CameraRefusal::DeniedByTheUser => format!(
            "Camera access is denied for {responsible_application}. macOS asks the application \
             that launched this process, not the program running inside it, so turn on \
             {responsible_application} in System Settings › Privacy & Security › Camera and run \
             again. If {responsible_application} is not in that list it has never asked, and the \
             list cannot add it by hand: run from a different terminal instead."
        ),
    }
}

/// The responsible application's name, or a description a user can act on
/// when it cannot be found.
fn responsible_application_for_the_user() -> String {
    responsible_gui_application_name()
        .unwrap_or_else(|| "the terminal or application this was launched from".to_owned())
}

/// Make sure camera access has been requested, returning at once. `true` when
/// the camera is allowed or the user is being asked; `false`, with the reason
/// logged, when it is refused.
pub fn request_camera_permission() -> Result<bool> {
    match authorize_the_camera_without_waiting_for_the_user(
        &AvFoundationCameraAuthorizationAuthority,
        Box::new(|granted| {
            if !granted {
                tracing::error!(
                    "{}",
                    camera_refusal_for_the_user(CameraRefusal::DeniedByTheUser)
                );
            }
        }),
    ) {
        Ok(_) => Ok(true),
        Err(refusal) => {
            tracing::error!(%refusal, "camera permission refused");
            Ok(false)
        }
    }
}

pub fn request_display_permission() -> Result<bool> {
    tracing::info!("Display permission granted (no system prompt required on macOS)");
    Ok(true)
}

pub fn request_audio_permission() -> Result<bool> {
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};

    let _mtm = MainThreadMarker::new().ok_or_else(|| {
        crate::core::Error::Configuration(
            "request_audio_permission must be called on main thread".into(),
        )
    })?;

    tracing::info!("Checking audio permission status...");

    let media_type = unsafe {
        AVMediaTypeAudio.ok_or_else(|| {
            crate::core::Error::Configuration("AVMediaTypeAudio not available".into())
        })?
    };

    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) };

    match status.0 {
        3 => {
            tracing::info!("Audio permission already granted");
            Ok(true)
        }
        0 => {
            tracing::info!("Audio permission not determined, will be requested on first use");
            Ok(true)
        }
        1 | 2 => {
            tracing::error!(
                "Audio permission denied or restricted (status={})",
                status.0
            );
            Ok(false)
        }
        _ => {
            tracing::warn!("Unknown audio authorization status: {}", status.0);
            Ok(false)
        }
    }
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
        status: CameraAuthorizationStatus,
        requests: AtomicUsize,
        unanswered: Mutex<Vec<CameraAuthorizationAnswer>>,
    }

    impl AUserWhoNeverAnswers {
        fn with_status(status: CameraAuthorizationStatus) -> Self {
            Self {
                status,
                requests: AtomicUsize::new(0),
                unanswered: Mutex::new(Vec::new()),
            }
        }
    }

    impl CameraAuthorizationAuthority for AUserWhoNeverAnswers {
        fn camera_authorization_status(&self) -> CameraAuthorizationStatus {
            self.status
        }

        fn request_camera_authorization(&self, answer: CameraAuthorizationAnswer) {
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
            CameraAuthorizationStatus::NotDetermined,
        ));
        let (returned, returned_with) = mpsc::channel();
        let asking_authority = Arc::clone(&authority);
        std::thread::spawn(move || {
            let at_open = authorize_the_camera_without_waiting_for_the_user(
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
            Ok(CameraAuthorizationAtOpen::AwaitingTheUsersAnswer)
        );
        assert_eq!(authority.requests.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_authorized_camera_is_opened_without_asking_again() {
        let authority = AUserWhoNeverAnswers::with_status(CameraAuthorizationStatus::Authorized);
        assert_eq!(
            authorize_the_camera_without_waiting_for_the_user(&authority, Box::new(|_| {}))
                .map_err(|e| e.to_string()),
            Ok(CameraAuthorizationAtOpen::Granted)
        );
        assert_eq!(authority.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_denied_camera_is_refused_without_asking_again() {
        let authority = AUserWhoNeverAnswers::with_status(CameraAuthorizationStatus::Denied);
        assert!(
            authorize_the_camera_without_waiting_for_the_user(&authority, Box::new(|_| {}))
                .is_err()
        );
        assert_eq!(authority.requests.load(Ordering::SeqCst), 0);
    }

    /// The user acts on this text in System Settings, where the entry is the
    /// terminal's — never the interpreter's.
    #[test]
    fn a_denial_names_the_responsible_application_and_the_setting_but_not_python() {
        let refusal = camera_refusal_naming(CameraRefusal::DeniedByTheUser, "iTerm");
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
        let refusal = camera_refusal_naming(CameraRefusal::RestrictedOnThisMac, "iTerm");
        assert!(refusal.contains("restricted"), "{refusal}");
        assert!(refusal.contains("iTerm"), "{refusal}");
    }
}
