// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The PipeWire arm against a real audio session: the device asks for samples,
//! takes them, and stops asking when told.
//!
//! Audio tier — needs a reachable PipeWire session with a playback endpoint.

use streamlib_engine::core::context::{SharedAudioDeviceBackend, probe_audio_device_backend};

mod audio_arm_playback_contract;
use audio_arm_playback_contract::{
    assert_a_live_playback_stream_reports_no_failure_and_neither_does_a_stopped_one,
    assert_a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off,
    assert_the_device_asks_for_whole_periods_of_its_own_format,
};

/// The arm, or `None` when the chain demoted past it.
///
/// `None` rather than a panic, so the tier stays well-behaved when the feature
/// is on but the runner has no audio session — the same shape
/// `try_vulkan_device()` gives the GPU tier (`docs/testing-hardware.md`).
fn pipewire_arm() -> Option<SharedAudioDeviceBackend> {
    let backend = probe_audio_device_backend();
    (backend.backend_name() == "pipewire").then_some(backend)
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a reachable PipeWire session with a playback endpoint. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_real_session_asks_for_whole_periods_of_the_format_it_negotiated() {
    let Some(backend) = pipewire_arm() else {
        return;
    };
    assert_the_device_asks_for_whole_periods_of_its_own_format(backend.as_ref(), None);
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a reachable PipeWire session with a playback endpoint. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_stopped_stream_asks_nothing_and_a_restart_replaces_the_hand_off() {
    let Some(backend) = pipewire_arm() else {
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
    ignore = "audio tier — needs a reachable PipeWire session with a playback endpoint. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_live_playback_stream_reports_no_failure_and_neither_does_a_stopped_one() {
    let Some(backend) = pipewire_arm() else {
        return;
    };
    assert_a_live_playback_stream_reports_no_failure_and_neither_does_a_stopped_one(
        backend.as_ref(),
        None,
    );
}
