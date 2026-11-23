// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The audio arm against a real machine: each capture device it lists opens by the id it lists
//! and delivers, and on PipeWire the default sink's monitor is listed, with the alias that
//! resolves to it ahead of it.
//!
//! Audio tier — needs a reachable audio backend with a capture device. It captures and never
//! plays.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use streamlib_engine::core::context::{
    AudioCaptureDeviceKind, AudioClockConfig, AudioDeviceStreamRequest,
    CaptureDeviceAccessPrompting, CapturedAudioBlockFromDevice, DEFAULT_OUTPUT_MONITOR_DEVICE_ID,
    RequestedAudioStreamFormat, SharedAudioDeviceBackend, SoftwareAudioClock,
    list_audio_capture_devices, open_capture_stream_in_requested_format,
    probe_audio_device_backend,
};

#[path = "support/rig_only_capability_skip.rs"]
mod rig_only_capability_skip;
use rig_only_capability_skip::{RigOnlyCapability, skip_because_this_machine_lacks};

/// A capture that has delivered nothing in this long did not open on a working device.
const FIRST_BLOCK_DEADLINE: Duration = Duration::from_secs(10);

/// The arm the chain chose, or `None`, printing the skip line, where it found no audio.
fn a_real_audio_arm_or_skip() -> Option<SharedAudioDeviceBackend> {
    let backend = probe_audio_device_backend();
    if backend.backend_name() == "silent-null" {
        skip_because_this_machine_lacks(RigOnlyCapability::AudioDevice);
        return None;
    }
    Some(backend)
}

/// Open `device_id` the way Microphone Source does and wait for its first block.
fn the_first_block_of(backend: &SharedAudioDeviceBackend, device_id: &str) -> Result<(), String> {
    let request = AudioDeviceStreamRequest {
        device_id: Some(device_id.to_owned()),
        deviceless_pacing_clock: Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(
            48_000, 512,
        ))),
        capture_device_access_prompting: CaptureDeviceAccessPrompting::MayAskTheUser,
        requested_format: RequestedAudioStreamFormat::default(),
    };
    let mut capture_stream = open_capture_stream_in_requested_format(backend.as_ref(), &request)
        .map_err(|refusal| format!("did not open: {refusal}"))?;
    let (delivered_sender, delivered_receiver) = mpsc::channel();
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let _ = delivered_sender.send(block.sample_count);
        }))
        .map_err(|refusal| format!("did not start delivering: {refusal}"))?;
    let delivered = delivered_receiver.recv_timeout(FIRST_BLOCK_DEADLINE);
    capture_stream
        .stop_delivering()
        .map_err(|refusal| format!("did not stop: {refusal}"))?;
    delivered
        .map(|_| ())
        .map_err(|_| format!("delivered no block in {FIRST_BLOCK_DEADLINE:?}"))
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a reachable audio backend with a capture device. Run with --features streamlib/hardware-tests"
)]
fn every_capture_device_the_arm_lists_opens_by_its_id_and_delivers() {
    let Some(backend) = a_real_audio_arm_or_skip() else {
        return;
    };
    let devices = list_audio_capture_devices(backend.as_ref()).expect("the arm lists its devices");
    assert!(
        devices
            .iter()
            .any(|device| device.kind == AudioCaptureDeviceKind::Input),
        "the {} arm lists no input: {devices:?}",
        backend.backend_name()
    );
    let refused: Vec<String> = devices
        .iter()
        .filter_map(|device| {
            let outcome = the_first_block_of(&backend, &device.id);
            eprintln!(
                "{} {:?} '{}' ({}): {}",
                backend.backend_name(),
                device.kind,
                device.id,
                device.name,
                match &outcome {
                    Ok(()) => "opened and delivered",
                    Err(refusal) => refusal.as_str(),
                }
            );
            outcome
                .err()
                .map(|refusal| format!("'{}': {refusal}", device.id))
        })
        .collect();
    assert!(
        refused.is_empty(),
        "listed devices that do not capture: {refused:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a reachable PipeWire session with a default sink. Run with --features streamlib/hardware-tests"
)]
fn the_pipewire_arm_lists_the_default_sinks_monitor_behind_the_alias_that_resolves_to_it() {
    let Some(backend) = a_real_audio_arm_or_skip() else {
        return;
    };
    if backend.backend_name() != "pipewire" {
        skip_because_this_machine_lacks(RigOnlyCapability::AudioDevice);
        return;
    }
    let default_output_monitor = backend
        .default_output_monitor_device_id()
        .expect("the session answers")
        .expect("the session has a default sink");
    let ids: Vec<String> = list_audio_capture_devices(backend.as_ref())
        .expect("the arm lists its devices")
        .into_iter()
        .map(|device| device.id)
        .collect();
    let alias_position = ids
        .iter()
        .position(|id| id == DEFAULT_OUTPUT_MONITOR_DEVICE_ID)
        .expect("the alias is listed");
    assert_eq!(
        ids.get(alias_position + 1),
        Some(&default_output_monitor),
        "the default sink's monitor is the first monitor, right behind the alias: {ids:?}"
    );
}
