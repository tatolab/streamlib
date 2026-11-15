// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::apple::permissions::{
    CaptureDeviceAuthorizationAnswer, CaptureDeviceAuthorizationAuthority,
    CaptureDeviceAuthorizationStatus, PrivacyGatedCaptureDevice,
    microphone_access_for_a_capture_hardware_test,
};
use crate::core::context::{
    AudioCaptureStream, AudioClockConfig, AudioDeviceStreamRequest, CaptureDeviceAccessPrompting,
    CapturedAudioBlockFromDevice, RequestedAudioStreamFormat, SoftwareAudioClock,
};

use super::audio_objects_and_device_discovery::{
    CoreAudioStreamDirection, default_device_object_id,
};
use super::capture_stream::{CoreAudioCaptureStream, MicrophoneAccessForTheStream};

/// Long against a device period, so a parked stream that was delivering
/// would have been seen to.
const HOW_LONG_A_PARKED_STREAM_IS_WATCHED: Duration = Duration::from_millis(500);

const FIRST_BLOCK_DEADLINE: Duration = Duration::from_secs(10);

/// A privacy gate that has not decided and keeps the request until the
/// test answers it.
#[derive(Default)]
struct AUserWhoAnswersOnCue {
    held_answer: Mutex<Option<CaptureDeviceAuthorizationAnswer>>,
}

impl AUserWhoAnswersOnCue {
    fn answer(&self, granted: bool) {
        let answer = self
            .held_answer
            .lock()
            .take()
            .expect("the stream asked the user when it opened");
        answer(granted);
    }
}

impl CaptureDeviceAuthorizationAuthority for AUserWhoAnswersOnCue {
    fn gated_capture_device(&self) -> PrivacyGatedCaptureDevice {
        PrivacyGatedCaptureDevice::Microphone
    }

    fn authorization_status(&self) -> CaptureDeviceAuthorizationStatus {
        CaptureDeviceAuthorizationStatus::NotDetermined
    }

    fn request_authorization(&self, answer: CaptureDeviceAuthorizationAnswer) {
        *self.held_answer.lock() = Some(answer);
    }
}

/// A request for the default input, or `None` when this Mac has none. The
/// real gate must already allow the microphone: "allowed" binds a real
/// unit, and against a real prompt that bind would wait on a person.
#[allow(clippy::disallowed_macros)]
fn a_request_for_the_default_input() -> Option<AudioDeviceStreamRequest> {
    if default_device_object_id(CoreAudioStreamDirection::Capture).is_none() {
        println!("cannot run: CoreAudio lists no default input device");
        return None;
    }
    if let Err(instruction) = microphone_access_for_a_capture_hardware_test() {
        panic!("{instruction}");
    }
    Some(AudioDeviceStreamRequest {
        device_id: None,
        deviceless_pacing_clock: Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(
            48_000, 512,
        ))),
        capture_device_access_prompting: CaptureDeviceAccessPrompting::MayAskTheUser,
        requested_format: RequestedAudioStreamFormat::default(),
    })
}

fn is_awaiting_the_users_answer(stream: &CoreAudioCaptureStream) -> bool {
    matches!(
        stream.capture_control.lock().microphone_access,
        MicrophoneAccessForTheStream::AwaitingTheUsersAnswer { .. }
    )
}

fn has_bound_a_unit(stream: &CoreAudioCaptureStream) -> bool {
    matches!(
        stream.capture_control.lock().microphone_access,
        MicrophoneAccessForTheStream::Granted(_)
    )
}

/// Binding a unit with input enabled blocks until the user answers, so a
/// stream that bound before the answer would hang its opener. Mental
/// revert: bind at open whatever the gate says, and the first two
/// assertions fail.
#[test]
fn a_stream_waiting_on_the_user_binds_no_unit_and_delivers_once_allowed() {
    let Some(request) = a_request_for_the_default_input() else {
        return;
    };
    let user = AUserWhoAnswersOnCue::default();
    let mut stream = CoreAudioCaptureStream::open(&request, &user)
        .expect("a stream opens without waiting on the user");
    assert!(
        is_awaiting_the_users_answer(&stream),
        "no unit is bound before the user has answered"
    );

    let (block_sender, block_receiver) = mpsc::channel();
    stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let _ = block_sender.send(block.sample_count);
        }))
        .expect("a start while the user is being asked parks rather than failing");
    assert!(
        is_awaiting_the_users_answer(&stream),
        "starting parks the hand-off and still binds nothing"
    );
    assert_eq!(
        block_receiver.recv_timeout(HOW_LONG_A_PARKED_STREAM_IS_WATCHED),
        Err(mpsc::RecvTimeoutError::Timeout),
        "a stream still waiting on the user delivers nothing"
    );

    user.answer(true);
    assert!(has_bound_a_unit(&stream), "the answer binds the unit");
    let first_block_sample_count = block_receiver
        .recv_timeout(FIRST_BLOCK_DEADLINE)
        .expect("the parked hand-off starts receiving blocks once the user allows it");
    assert!(first_block_sample_count > 0);
    assert!(
        stream
            .liveness_report()
            .failure_that_ended_the_stream()
            .is_none()
    );
    stream.stop_delivering().expect("delivery stops");
}

#[test]
fn a_stream_the_user_refused_reports_the_microphone_refusal_and_never_hands_off() {
    let Some(request) = a_request_for_the_default_input() else {
        return;
    };
    let user = AUserWhoAnswersOnCue::default();
    let mut stream = CoreAudioCaptureStream::open(&request, &user)
        .expect("a stream opens without waiting on the user");
    let hand_off_calls = Arc::new(AtomicUsize::new(0));
    stream
        .start_delivering_to(Box::new({
            let hand_off_calls = Arc::clone(&hand_off_calls);
            move |_block: CapturedAudioBlockFromDevice<'_>| {
                hand_off_calls.fetch_add(1, Ordering::SeqCst);
            }
        }))
        .expect("a start while the user is being asked parks rather than failing");

    user.answer(false);

    let failure = stream
        .liveness_report()
        .failure_that_ended_the_stream()
        .expect("a refusal ends the stream through its liveness report");
    assert!(
        failure.to_string().contains("Microphone"),
        "the refusal names the Microphone setting: {failure}"
    );
    assert!(!has_bound_a_unit(&stream), "a refused stream binds nothing");
    assert_eq!(hand_off_calls.load(Ordering::SeqCst), 0);

    let restart = stream.start_delivering_to(Box::new(|_block| {}));
    let refusal = restart.expect_err("starting a stream the user refused fails");
    assert!(refusal.to_string().contains("Microphone"), "{refusal}");
}
