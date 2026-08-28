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

use std::sync::Arc;

use streamlib_engine::core::context::{
    AudioClockConfig, AudioDeviceBackend, AudioDeviceStreamRequest, SoftwareAudioClock,
};
use streamlib_engine::linux_alsa_audio_device_backend::AlsaAudioDeviceBackend;

mod audio_arm_playback_contract;
use audio_arm_playback_contract::{
    assert_a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off,
    assert_the_device_asks_for_whole_periods_of_its_own_format,
};

/// The arm, or `None` when this machine cannot play audio through it.
///
/// The arm's own probe opens a *capture* device, because that is what the chain
/// demotes on — so a capture-only host passes it and then panics inside the
/// shared suite's `expect`. Playback is probed here instead, and a host that
/// has none skips: it genuinely cannot answer the question these tests ask,
/// and an `expect` that aborts is not a more honest answer than saying so.
/// A failure *after* the suite has a stream is left to propagate, which is
/// where a real defect shows up.
fn alsa_arm() -> Option<AlsaAudioDeviceBackend> {
    let backend = AlsaAudioDeviceBackend::load_and_open().ok()?;
    // Opened and dropped: holding it across the probe would claim a device the
    // suite is about to open for itself.
    backend
        .open_playback_stream(&AudioDeviceStreamRequest {
            device_id: None,
            deviceless_pacing_clock: Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(
                48_000, 512,
            ))),
        })
        .ok()?;
    Some(backend)
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
