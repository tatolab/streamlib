// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The capture seam's liveness claim on the one arm that needs no hardware.
//!
//! Carries no `hardware-tests` gate, which is the point: the capture suite's
//! two device siblings can only run on a rig, so without this the claim that a
//! healthy stream reports no failure — the assertion keeping the signal from
//! being wired to fire always — would be checked nowhere CI can see. Its
//! playback counterpart is `silent_null_arm_plays_what_it_is_given.rs`.
//!
//! The backend is named directly rather than reached through
//! `probe_audio_device_backend`, for the reason that file states: the chain
//! takes the first arm that opens, so on any machine with an audio server the
//! probe answers something else.
//!
//! Only the liveness claim runs here. The rest of the capture suite is about a
//! device's own timing, and this arm has no device — its cadence is the timerfd
//! clock, which is exactly the case that entry exempts.

use streamlib_engine::core::context::SilentNullAudioDeviceBackend;

// The suite is compiled into each arm's binary, and this arm runs one of its
// claims — the rest need a device.
#[allow(dead_code)]
mod audio_arm_timestamp_contract;
use audio_arm_timestamp_contract::assert_a_live_capture_stream_reports_no_failure_and_neither_does_a_stopped_one;

/// The arm whose streams cannot die, asked whether they have: a graph running
/// in a container has to be able to ask the same question a graph on a
/// workstation asks, and get the answer that arm's design promises.
#[test]
fn a_deviceless_capture_stream_never_reports_a_failure() {
    assert_a_live_capture_stream_reports_no_failure_and_neither_does_a_stopped_one(
        &SilentNullAudioDeviceBackend,
        None,
    );
}
