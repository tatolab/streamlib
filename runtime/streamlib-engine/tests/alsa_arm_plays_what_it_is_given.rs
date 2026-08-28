// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The ALSA arm against a real playback device: the device asks for samples,
//! takes them, and stops asking when told.
//!
//! The arm is constructed directly rather than reached through
//! `probe_audio_device_backend`, for the same reason its capture sibling is:
//! the chain takes the first arm that opens and no dial overrides it, so on any
//! machine with a PipeWire session the probe answers "pipewire" and this arm
//! would never be exercised.
//!
//! Audio tier — needs `/dev/snd` and a playback device ALSA can open.

#![cfg(target_os = "linux")]

use streamlib_engine::linux_alsa_audio_device_backend::AlsaAudioDeviceBackend;

mod audio_arm_playback_contract;
use audio_arm_playback_contract::{
    assert_a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off,
    assert_the_device_asks_for_whole_periods_of_its_own_format,
};

/// The arm, or `None` when this machine has no `libasound` and no audio
/// hardware at all.
///
/// The arm's own probe opens a *capture* device, because that is what the chain
/// demotes on. A host with a capture device and no playback device is not a
/// shape this skips over quietly — it fails when the playback stream is opened,
/// which is the honest answer for a machine that cannot play audio.
fn alsa_arm() -> Option<AlsaAudioDeviceBackend> {
    AlsaAudioDeviceBackend::load_and_open().ok()
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs /dev/snd and a playback device ALSA can open. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn the_default_device_asks_for_whole_periods_of_the_format_it_negotiated() {
    let Some(backend) = alsa_arm() else {
        return;
    };
    assert_the_device_asks_for_whole_periods_of_its_own_format(&backend, None);
}

/// The two request clauses the seam states, on the arm that has a device behind
/// them — and the restart path, where a stream that left its PCM running would
/// surface as `EBUSY`.
#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs /dev/snd and a playback device ALSA can open. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off() {
    let Some(backend) = alsa_arm() else {
        return;
    };
    assert_a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off(&backend, None);
}
