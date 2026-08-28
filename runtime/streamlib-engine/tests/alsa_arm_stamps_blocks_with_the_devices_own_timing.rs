// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The ALSA arm against a real capture device: blocks arrive, and their
//! timestamps are the device's rather than the moment of delivery.
//!
//! The arm is constructed directly rather than reached through
//! `probe_audio_device_backend`. The chain takes the first arm that opens and
//! no dial overrides it — which is the decided behaviour — so on any machine
//! with a PipeWire session the probe answers "pipewire" and this arm would
//! never be exercised.
//!
//! Two device paths are covered, because they answer different questions. The
//! `default` PCM is what a caller gets, but on a desktop it resolves to the
//! PipeWire ALSA compat plugin — so it proves the seam without proving the
//! driver path this arm exists for. A raw `hw:` node bypasses any daemon and
//! is the machine-with-no-PipeWire case the ticket names.
//!
//! Audio tier — needs `/dev/snd` and a capture device ALSA can open.

#![cfg(target_os = "linux")]

use streamlib_engine::linux_alsa_audio_device_backend::AlsaAudioDeviceBackend;

mod audio_arm_timestamp_contract;
use audio_arm_timestamp_contract::{
    assert_a_stopped_stream_is_silent_and_a_restart_replaces_the_hand_off,
    assert_the_timestamp_contract_holds_on,
};

/// The arm, or `None` when this machine has no `libasound` or no capture device
/// behind it.
///
/// `None` rather than a panic, so the tier stays well-behaved when the feature
/// is on but the runner has no sound card — the same shape `try_vulkan_device()`
/// gives the GPU tier (`docs/testing-hardware.md`). A machine with no audio is
/// a supported environment; it just cannot answer the question these tests ask.
fn alsa_arm() -> Option<AlsaAudioDeviceBackend> {
    AlsaAudioDeviceBackend::load_and_open().ok()
}

/// A raw `hw:` capture node, read off `/dev/snd` rather than assumed.
///
/// The kernel names capture PCMs `pcmC<card>D<device>c`, which is exactly the
/// `hw:<card>,<device>` spelling ALSA opens — so this is the driver, with no
/// daemon or compat plugin in the path. `None` when the machine has none, or
/// when every one of them is already held: `EBUSY` on a card a daemon has open
/// is expected, not a failure.
fn a_raw_capture_device_this_machine_will_open(backend: &AlsaAudioDeviceBackend) -> Option<String> {
    use std::sync::Arc;
    use streamlib_engine::core::context::{
        AudioCaptureStreamRequest, AudioClockConfig, AudioDeviceBackend, SharedAudioClock,
        SoftwareAudioClock,
    };

    let mut raw_capture_device_names: Vec<String> = std::fs::read_dir("/dev/snd")
        .ok()?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            let card_and_device = name.strip_prefix("pcmC")?.strip_suffix('c')?;
            let (card, device) = card_and_device.split_once('D')?;
            Some(format!("hw:{card},{device}"))
        })
        .collect();
    raw_capture_device_names.sort();

    raw_capture_device_names.into_iter().find(|device_name| {
        let deviceless_pacing_clock: SharedAudioClock =
            Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(48_000, 512)));
        backend
            .open_capture_stream(&AudioCaptureStreamRequest {
                device_id: Some(device_name.clone()),
                deviceless_pacing_clock,
            })
            .is_ok()
    })
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs /dev/snd and a capture device ALSA can open. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn the_default_device_stamps_its_blocks_with_the_devices_own_timing() {
    let Some(backend) = alsa_arm() else {
        return;
    };
    assert_the_timestamp_contract_holds_on(&backend, None);
}

/// The arm's reason to exist: `libasound` talking to the driver, with no
/// daemon and no compat plugin between. On a desktop the `default` PCM resolves
/// to PipeWire's ALSA plugin, which synthesizes its timestamps — so that path
/// alone cannot prove this.
#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs /dev/snd and a capture device ALSA can open. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_raw_hardware_device_stamps_its_blocks_with_the_drivers_own_timing() {
    let Some(backend) = alsa_arm() else {
        return;
    };
    let Some(raw_capture_device) = a_raw_capture_device_this_machine_will_open(&backend) else {
        return;
    };
    assert_the_timestamp_contract_holds_on(&backend, Some(raw_capture_device));
}

/// The two delivery clauses the seam states, on the arm that has a device
/// behind them — and the restart path, where a stream that left its PCM
/// running would surface as `EBUSY`.
#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs /dev/snd and a capture device ALSA can open. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_stopped_stream_is_silent_and_a_restart_replaces_the_hand_off() {
    let Some(backend) = alsa_arm() else {
        return;
    };
    assert_a_stopped_stream_is_silent_and_a_restart_replaces_the_hand_off(&backend, None);
}
