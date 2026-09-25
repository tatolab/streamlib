// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The CoreAudio arm against a real input device: blocks arrive, and their
//! timestamps are the device's rather than the moment of delivery.
//!
//! The distinction is the whole of block-level A/V sync, and it is invisible
//! without an assertion — a stamp taken at delivery looks perfectly plausible
//! and is wrong by a device quantum, every block, forever.
//!
//! Audio tier — needs a Mac with an input device, and microphone access
//! already allowed for the terminal running it: a stream still waiting on the
//! user's answer delivers nothing, and fails these on their deadline.

#![cfg(target_os = "macos")]

use streamlib_engine::core::context::{SharedAudioDeviceBackend, probe_audio_device_backend};

mod audio_arm_timestamp_contract;
use audio_arm_timestamp_contract::{
    assert_a_live_capture_stream_reports_no_failure_and_neither_does_a_stopped_one,
    assert_a_stopped_stream_is_silent_and_a_restart_replaces_the_hand_off,
    assert_the_timestamp_contract_holds_on,
};

/// The arm, or `None` when the chain demoted past it.
///
/// `None` rather than a panic, so the tier stays well-behaved when the feature
/// is on but the runner has no audio device — the same shape
/// `try_vulkan_device()` gives the GPU tier (`docs/testing-hardware.md`). A
/// machine with no audio is a supported environment; it just cannot answer the
/// question these tests ask.
fn coreaudio_arm() -> Option<SharedAudioDeviceBackend> {
    let backend = probe_audio_device_backend();
    (backend.backend_name() == "coreaudio").then_some(backend)
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a Mac with an input device and microphone access allowed. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_real_input_device_stamps_its_blocks_with_the_devices_own_timing() {
    let Some(backend) = coreaudio_arm() else {
        return;
    };
    assert_the_timestamp_contract_holds_on(backend.as_ref(), None);
}

/// The two delivery clauses `AudioCaptureStream` states, on the arm a desktop
/// actually runs.
#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a Mac with an input device and microphone access allowed. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_stopped_stream_is_silent_and_a_restart_replaces_the_hand_off() {
    let Some(backend) = coreaudio_arm() else {
        return;
    };
    assert_a_stopped_stream_is_silent_and_a_restart_replaces_the_hand_off(backend.as_ref(), None);
}

/// A device that is delivering has not failed, and neither has one its owner
/// stopped.
#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a Mac with an input device and microphone access allowed. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_live_capture_stream_reports_no_failure_and_neither_does_a_stopped_one() {
    let Some(backend) = coreaudio_arm() else {
        return;
    };
    assert_a_live_capture_stream_reports_no_failure_and_neither_does_a_stopped_one(
        backend.as_ref(),
        None,
    );
}
