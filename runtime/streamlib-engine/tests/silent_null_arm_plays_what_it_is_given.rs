// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The playback seam's contract on the one arm that needs no hardware.
//!
//! Carries no `hardware-tests` gate, which is the point: its two device
//! siblings can only run on a rig, so without this the shared suite — and the
//! clause about a restart replacing the hand-off rather than adding to it —
//! would be asserted nowhere CI can see. The arm that runs everywhere is what
//! keeps the seam's contract honest between rig runs.
//!
//! The backend is named directly rather than reached through
//! `probe_audio_device_backend`: the chain takes the first arm that opens, so
//! on any machine with an audio server the probe answers something else and
//! this would be testing that machine's hardware instead.

use streamlib_engine::core::context::SilentNullAudioDeviceBackend;

mod audio_arm_playback_contract;
use audio_arm_playback_contract::{
    assert_a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off,
    assert_the_device_asks_for_whole_periods_of_its_own_format,
};

#[test]
fn the_deviceless_arm_asks_for_whole_periods_of_the_format_it_reports() {
    assert_the_device_asks_for_whole_periods_of_its_own_format(&SilentNullAudioDeviceBackend, None);
}

#[test]
fn a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off() {
    assert_a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off(
        &SilentNullAudioDeviceBackend,
        None,
    );
}
