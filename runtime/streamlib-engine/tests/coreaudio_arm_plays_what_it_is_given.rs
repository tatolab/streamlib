// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The CoreAudio arm against a real output device: the device asks for
//! samples, takes them, and stops asking when told.
//!
//! Audio tier — needs a Mac with a default output device. Silent: every
//! hand-off writes zeros.

#![cfg(target_os = "macos")]

use streamlib_engine::apple_coreaudio_audio_tier::CoreAudioStreamDirection;

mod audio_arm_playback_contract;
use audio_arm_playback_contract::{
    assert_a_live_playback_stream_reports_no_failure_and_neither_does_a_stopped_one,
    assert_a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off,
    assert_the_device_asks_for_whole_periods_of_its_own_format,
};

#[path = "support/coreaudio_audio_tier.rs"]
mod coreaudio_audio_tier;
use coreaudio_audio_tier::the_coreaudio_arm_with_a_default_device_for;

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a Mac with an output device. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_real_device_asks_for_whole_periods_of_the_format_it_negotiated() {
    let Some(backend) =
        the_coreaudio_arm_with_a_default_device_for(CoreAudioStreamDirection::Playback)
    else {
        return;
    };
    assert_the_device_asks_for_whole_periods_of_its_own_format(backend.as_ref(), None);
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a Mac with an output device. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off() {
    let Some(backend) =
        the_coreaudio_arm_with_a_default_device_for(CoreAudioStreamDirection::Playback)
    else {
        return;
    };
    assert_a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off(
        backend.as_ref(),
        None,
    );
}

/// A device that is playing has not failed, and neither has one its owner
/// stopped.
#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a Mac with an output device. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_live_playback_stream_reports_no_failure_and_neither_does_a_stopped_one() {
    let Some(backend) =
        the_coreaudio_arm_with_a_default_device_for(CoreAudioStreamDirection::Playback)
    else {
        return;
    };
    assert_a_live_playback_stream_reports_no_failure_and_neither_does_a_stopped_one(
        backend.as_ref(),
        None,
    );
}
