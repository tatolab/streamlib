// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::apple::responsible_gui_application::responsible_gui_application_name;
use crate::core::{Error, Result};

/// How long a request may sit unanswered before the user is told where the
/// prompt is and what else answers it.
const UNANSWERED_REQUEST_REMINDER_DELAY: Duration = Duration::from_secs(10);

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

/// Runs a piece of work once, after a delay measured on a monotonic clock.
pub(crate) trait OneShotReminderScheduler {
    /// Run `reminder` once `delay` has passed, returning at once.
    fn run_once_after(&self, delay: Duration, reminder: Box<dyn FnOnce() + Send + 'static>);
}

/// GCD's `dispatch_after` on a global utility queue, timed on the host's
/// monotonic clock.
struct GrandCentralDispatchOneShotReminderScheduler;

impl OneShotReminderScheduler for GrandCentralDispatchOneShotReminderScheduler {
    fn run_once_after(&self, delay: Duration, reminder: Box<dyn FnOnce() + Send + 'static>) {
        use dispatch2::{DispatchQoS, DispatchQueue, DispatchTime, GlobalQueueIdentifier};

        let Ok(when) = DispatchTime::try_from(delay) else {
            tracing::warn!(?delay, "a reminder's delay does not fit a dispatch time");
            return;
        };
        let queue = DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(
            DispatchQoS::Utility,
        ));
        if let Err(scheduling_error) = queue.after(when, reminder) {
            tracing::warn!(?scheduling_error, "a reminder could not be scheduled");
        }
    }
}

/// What a user who has not answered a capture-device request is told once
/// [`UNANSWERED_REQUEST_REMINDER_DELAY`] has passed.
fn unanswered_request_reminder_naming(
    device: PrivacyGatedCaptureDevice,
    responsible_application: &str,
) -> String {
    let setting = device.privacy_setting_name();
    format!(
        "{setting} access is still waiting on an answer: macOS asked {} s ago whether \
         {responsible_application} may use the {} and nobody has answered. Answer the prompt — \
         it can sit behind other windows — or turn on {responsible_application} in System \
         Settings › Privacy & Security › {setting}. {} as soon as you allow it.",
        UNANSWERED_REQUEST_REMINDER_DELAY.as_secs(),
        device.lowercase_name(),
        device.what_starts_once_allowed(),
    )
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
/// once and `on_the_users_answer` runs whenever the answer arrives. A request
/// still unanswered after [`UNANSWERED_REQUEST_REMINDER_DELAY`] is warned
/// about once. A refusal already on record is refused here, naming the
/// application macOS holds responsible and the setting to change.
pub(crate) fn authorize_the_capture_device_without_waiting_for_the_user(
    authority: &dyn CaptureDeviceAuthorizationAuthority,
    on_the_users_answer: CaptureDeviceAuthorizationAnswer,
) -> Result<CaptureDeviceAuthorizationAtOpen> {
    authorize_the_capture_device_reminding_the_user_through(
        authority,
        &GrandCentralDispatchOneShotReminderScheduler,
        on_the_users_answer,
    )
}

fn authorize_the_capture_device_reminding_the_user_through(
    authority: &dyn CaptureDeviceAuthorizationAuthority,
    reminder_scheduler: &dyn OneShotReminderScheduler,
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
            let the_user_has_answered = Arc::new(AtomicBool::new(false));
            reminder_scheduler.run_once_after(
                UNANSWERED_REQUEST_REMINDER_DELAY,
                Box::new({
                    let the_user_has_answered = Arc::clone(&the_user_has_answered);
                    let reminder =
                        unanswered_request_reminder_naming(device, &responsible_application);
                    move || {
                        if !the_user_has_answered.load(Ordering::Acquire) {
                            tracing::warn!(
                                responsible_application = %responsible_application,
                                "{reminder}"
                            );
                        }
                    }
                }),
            );
            authority.request_authorization(Box::new(move |granted| {
                the_user_has_answered.store(true, Ordering::Release);
                on_the_users_answer(granted);
            }));
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

/// Microphone access for a hardware test that captures: `Ok` once allowed,
/// else what the person at the machine must do — asking macOS first when
/// nobody has.
pub fn microphone_access_for_a_capture_hardware_test() -> std::result::Result<(), String> {
    let device = PrivacyGatedCaptureDevice::Microphone;
    let authority = AvFoundationCaptureDeviceAuthorizationAuthority(device);
    match authority.authorization_status() {
        CaptureDeviceAuthorizationStatus::Authorized => Ok(()),
        CaptureDeviceAuthorizationStatus::NotDetermined => {
            authority.request_authorization(Box::new(|_granted| {}));
            Err(asked_for_a_hardware_test_naming(
                device,
                &responsible_application_for_the_user(),
            ))
        }
        CaptureDeviceAuthorizationStatus::Denied => Err(capture_device_refusal_for_the_user(
            device,
            CaptureDeviceRefusal::DeniedByTheUser,
        )),
        CaptureDeviceAuthorizationStatus::Restricted => Err(capture_device_refusal_for_the_user(
            device,
            CaptureDeviceRefusal::RestrictedOnThisMac,
        )),
    }
}

fn asked_for_a_hardware_test_naming(
    device: PrivacyGatedCaptureDevice,
    responsible_application: &str,
) -> String {
    format!(
        "{} access had never been asked for, so macOS is asking now whether \
         {responsible_application} may use the {}: allow {responsible_application} in the \
         prompt, then re-run.",
        device.privacy_setting_name(),
        device.lowercase_name(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::CapturedTracingWarnings;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Mutex, mpsc};

    /// A privacy gate whose user never answers: every request is kept and
    /// none is ever called back — what a prompt nobody has clicked looks like.
    struct AUserWhoNeverAnswers {
        device: PrivacyGatedCaptureDevice,
        status: CaptureDeviceAuthorizationStatus,
        requests: AtomicUsize,
        unanswered: Mutex<Vec<CaptureDeviceAuthorizationAnswer>>,
    }

    impl AUserWhoNeverAnswers {
        fn with_status(status: CaptureDeviceAuthorizationStatus) -> Self {
            Self::of_the(PrivacyGatedCaptureDevice::Camera, status)
        }

        fn of_the(
            device: PrivacyGatedCaptureDevice,
            status: CaptureDeviceAuthorizationStatus,
        ) -> Self {
            Self {
                device,
                status,
                requests: AtomicUsize::new(0),
                unanswered: Mutex::new(Vec::new()),
            }
        }

        /// The user finally answers every request still open.
        fn answer_every_request(&self, granted: bool) {
            let unanswered = std::mem::take(&mut *self.unanswered.lock().expect("unpoisoned"));
            for answer in unanswered {
                answer(granted);
            }
        }
    }

    impl CaptureDeviceAuthorizationAuthority for AUserWhoNeverAnswers {
        fn gated_capture_device(&self) -> PrivacyGatedCaptureDevice {
            self.device
        }

        fn authorization_status(&self) -> CaptureDeviceAuthorizationStatus {
            self.status
        }

        fn request_authorization(&self, answer: CaptureDeviceAuthorizationAnswer) {
            self.requests.fetch_add(1, Ordering::SeqCst);
            self.unanswered.lock().expect("unpoisoned").push(answer);
        }
    }

    type ScheduledReminder = (Duration, Box<dyn FnOnce() + Send + 'static>);

    /// A scheduler whose reminders come due only when the test says so, so
    /// what is under test is the decision rather than a timer.
    #[derive(Default)]
    struct RemindersTheTestBringsDue {
        scheduled: Mutex<Vec<ScheduledReminder>>,
    }

    impl RemindersTheTestBringsDue {
        fn delays_scheduled(&self) -> Vec<Duration> {
            self.scheduled
                .lock()
                .expect("unpoisoned")
                .iter()
                .map(|(delay, _)| *delay)
                .collect()
        }

        fn bring_every_reminder_due(&self) {
            let scheduled = std::mem::take(&mut *self.scheduled.lock().expect("unpoisoned"));
            for (_, reminder) in scheduled {
                reminder();
            }
        }
    }

    impl OneShotReminderScheduler for RemindersTheTestBringsDue {
        fn run_once_after(&self, delay: Duration, reminder: Box<dyn FnOnce() + Send + 'static>) {
            self.scheduled
                .lock()
                .expect("unpoisoned")
                .push((delay, reminder));
        }
    }

    fn ask_the_microphone_user(
        user: &AUserWhoNeverAnswers,
        reminders: &RemindersTheTestBringsDue,
        answers_received: &Arc<Mutex<Vec<bool>>>,
    ) -> CaptureDeviceAuthorizationAtOpen {
        let answers_received = Arc::clone(answers_received);
        authorize_the_capture_device_reminding_the_user_through(
            user,
            reminders,
            Box::new(move |granted| answers_received.lock().expect("unpoisoned").push(granted)),
        )
        .expect("a request nobody has answered is not a refusal")
    }

    #[test]
    fn an_unanswered_request_schedules_one_reminder_at_the_deadline() {
        let user = AUserWhoNeverAnswers::of_the(
            PrivacyGatedCaptureDevice::Microphone,
            CaptureDeviceAuthorizationStatus::NotDetermined,
        );
        let reminders = RemindersTheTestBringsDue::default();
        let at_open = ask_the_microphone_user(&user, &reminders, &Arc::default());
        assert_eq!(
            at_open,
            CaptureDeviceAuthorizationAtOpen::AwaitingTheUsersAnswer
        );
        assert_eq!(
            reminders.delays_scheduled(),
            [UNANSWERED_REQUEST_REMINDER_DELAY]
        );
    }

    /// Mental revert: drop the scheduled reminder, as the gate had it, and a
    /// prompt nobody sees leaves one INFO line and then a silent graph.
    #[test]
    fn a_reminder_due_before_any_answer_warns_once_naming_the_application_and_the_setting() {
        let user = AUserWhoNeverAnswers::of_the(
            PrivacyGatedCaptureDevice::Microphone,
            CaptureDeviceAuthorizationStatus::NotDetermined,
        );
        let reminders = RemindersTheTestBringsDue::default();
        ask_the_microphone_user(&user, &reminders, &Arc::default());

        let ((), warnings) =
            CapturedTracingWarnings::captured_while(|| reminders.bring_every_reminder_due());

        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let responsible_application = responsible_application_for_the_user();
        assert!(
            warnings[0].contains(&responsible_application),
            "the reminder names {responsible_application}: {}",
            warnings[0]
        );
        assert!(
            warnings[0].contains("System Settings › Privacy & Security › Microphone"),
            "{}",
            warnings[0]
        );
    }

    #[test]
    fn an_answer_before_the_deadline_silences_the_reminder_and_still_reaches_the_stream() {
        let user = AUserWhoNeverAnswers::of_the(
            PrivacyGatedCaptureDevice::Microphone,
            CaptureDeviceAuthorizationStatus::NotDetermined,
        );
        let reminders = RemindersTheTestBringsDue::default();
        let answers_received = Arc::default();
        ask_the_microphone_user(&user, &reminders, &answers_received);

        user.answer_every_request(true);
        let ((), warnings) =
            CapturedTracingWarnings::captured_while(|| reminders.bring_every_reminder_due());

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(*answers_received.lock().expect("unpoisoned"), [true]);
    }

    #[test]
    fn a_status_already_decided_schedules_no_reminder() {
        for status in [
            CaptureDeviceAuthorizationStatus::Authorized,
            CaptureDeviceAuthorizationStatus::Denied,
            CaptureDeviceAuthorizationStatus::Restricted,
        ] {
            let user = AUserWhoNeverAnswers::of_the(PrivacyGatedCaptureDevice::Camera, status);
            let reminders = RemindersTheTestBringsDue::default();
            let _ = authorize_the_capture_device_reminding_the_user_through(
                &user,
                &reminders,
                Box::new(|_| {}),
            );
            assert!(
                reminders.delays_scheduled().is_empty(),
                "{status:?} needs no answer, so nothing is owed a reminder"
            );
        }
    }

    #[test]
    fn the_cameras_reminder_names_the_cameras_setting() {
        let reminder =
            unanswered_request_reminder_naming(PrivacyGatedCaptureDevice::Camera, "iTerm");
        assert!(reminder.contains("iTerm"), "{reminder}");
        assert!(
            reminder.contains("System Settings › Privacy & Security › Camera"),
            "{reminder}"
        );
        assert!(!reminder.contains("Microphone"), "{reminder}");
    }

    #[test]
    fn a_hardware_test_that_asked_tells_the_person_to_allow_the_application_and_re_run() {
        let instruction =
            asked_for_a_hardware_test_naming(PrivacyGatedCaptureDevice::Microphone, "Terminal");
        assert!(
            instruction.contains("allow Terminal in the prompt, then re-run"),
            "{instruction}"
        );
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
