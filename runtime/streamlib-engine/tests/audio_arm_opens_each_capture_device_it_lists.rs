// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The audio arm against a real machine: each capture device it lists opens by the id it lists
//! and delivers, an id it does not list is refused at open, and on PipeWire a stream whose device
//! goes away fails, and the default sink's monitor is listed, with the alias that resolves to it
//! ahead of it.
//!
//! Audio tier — needs a reachable audio backend with a capture device. Each test but the last
//! captures and never plays; the last plays a tone into the default output and is audible unless
//! that output is a null sink, so it runs under `audible-hardware-tests` only.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

#[cfg(target_os = "linux")]
use streamlib_engine::core::context::{
    AudioBlockRequestedByDevice, AudioSampleFormat, DEFAULT_OUTPUT_MONITOR_DEVICE_ID,
    open_playback_stream_in_requested_format,
};
use streamlib_engine::core::context::{
    AudioClockConfig, AudioDeviceStreamRequest, CaptureDeviceAccessPrompting,
    CapturedAudioBlockFromDevice, RequestedAudioStreamFormat, SharedAudioDeviceBackend,
    SoftwareAudioClock, list_audio_capture_devices, open_capture_stream_in_requested_format,
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

/// A capture request on `device_id` as Microphone Source makes one.
fn a_capture_request_for(device_id: &str) -> AudioDeviceStreamRequest {
    AudioDeviceStreamRequest {
        device_id: Some(device_id.to_owned()),
        deviceless_pacing_clock: Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(
            48_000, 512,
        ))),
        capture_device_access_prompting: CaptureDeviceAccessPrompting::MayAskTheUser,
        requested_format: RequestedAudioStreamFormat::default(),
    }
}

/// Open `device_id` the way Microphone Source does and wait for its first block.
fn the_first_block_of(backend: &SharedAudioDeviceBackend, device_id: &str) -> Result<(), String> {
    let mut capture_stream = open_capture_stream_in_requested_format(
        backend.as_ref(),
        &a_capture_request_for(device_id),
    )
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
        !devices.is_empty(),
        "the {} arm lists no capture device",
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

/// An id no backend lists, spelled the way PipeWire names an ALSA source.
const A_CAPTURE_DEVICE_ID_THAT_IS_NOT_ATTACHED: &str = "alsa_input.usb-gone.mono";

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a reachable audio backend with a capture device. Run with --features streamlib/hardware-tests"
)]
fn a_capture_device_id_the_arm_does_not_list_is_refused_at_open_rather_than_opening_another() {
    let Some(backend) = a_real_audio_arm_or_skip() else {
        return;
    };
    let refusal = match the_first_block_of(&backend, A_CAPTURE_DEVICE_ID_THAT_IS_NOT_ATTACHED) {
        Ok(()) => panic!(
            "the {} arm opened '{A_CAPTURE_DEVICE_ID_THAT_IS_NOT_ATTACHED}', which is not \
             attached, and delivered from another device",
            backend.backend_name()
        ),
        Err(refusal) => refusal,
    };
    eprintln!("{}: {refusal}", backend.backend_name());
    assert!(
        refusal.starts_with("did not open:")
            && refusal.contains(A_CAPTURE_DEVICE_ID_THAT_IS_NOT_ATTACHED),
        "refused at open, naming the id: {refusal}"
    );
    if backend.backend_name() == "pipewire" {
        let listed = backend.list_capture_devices().expect("the arm lists");
        for device in &listed.devices {
            assert!(
                refusal.contains(&device.id),
                "the refusal names each listed id, '{}' among them: {refusal}",
                device.id
            );
        }
    }
}

/// `virtual_audio_device.sh`, beside this suite's other fixtures.
#[cfg(target_os = "linux")]
fn the_virtual_audio_device_fixture(verb: &str) -> Result<String, String> {
    let output = std::process::Command::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/virtual_audio_device.sh"
    ))
    .arg(verb)
    .output()
    .map_err(|refusal| format!("the fixture did not run: {refusal}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "{stdout} {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The fixture's null sink, removed when this drops unless `remove` already removed it, so a
/// failed test leaves no sink in the session.
#[cfg(target_os = "linux")]
struct FixtureNullSinkInTheSession {
    sink_name: String,
    removed: bool,
}

#[cfg(target_os = "linux")]
impl FixtureNullSinkInTheSession {
    fn add() -> Result<Self, String> {
        Ok(Self {
            sink_name: the_virtual_audio_device_fixture("start")?,
            removed: false,
        })
    }

    fn remove(&mut self) -> Result<(), String> {
        self.removed = true;
        the_virtual_audio_device_fixture("stop").map(|_| ())
    }
}

#[cfg(target_os = "linux")]
impl Drop for FixtureNullSinkInTheSession {
    fn drop(&mut self) {
        if !self.removed {
            let _ = the_virtual_audio_device_fixture("stop");
        }
    }
}

/// How long the session has to end a stream whose device it removed.
#[cfg(target_os = "linux")]
const REMOVED_DEVICE_FAILURE_DEADLINE: Duration = Duration::from_secs(5);

/// How long a stream whose device went away is watched for a block from another device.
#[cfg(target_os = "linux")]
const AFTER_REMOVAL_DELIVERY_WATCH: Duration = Duration::from_secs(2);

/// What the arm's recorder prefixes to every reason the shim reports.
#[cfg(target_os = "linux")]
const PIPEWIRE_STREAM_FAILURE_PREFIX: &str = "the PipeWire stream stopped serving its device";

/// What the shim's format path records; every other reason it records is the error state's.
#[cfg(target_os = "linux")]
const PIPEWIRE_FORMAT_PATH_REASONS: [&str; 2] = ["PipeWire negotiated", "PipeWire renegotiated"];

#[cfg(target_os = "linux")]
#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a reachable PipeWire session the fixture can add a null sink to. Run with --features streamlib/hardware-tests"
)]
fn a_capture_stream_whose_device_goes_away_fails_rather_than_moving_to_the_default() {
    let Some(backend) = a_real_audio_arm_or_skip() else {
        return;
    };
    if backend.backend_name() != "pipewire" || the_virtual_audio_device_fixture("check").is_err() {
        skip_because_this_machine_lacks(RigOnlyCapability::AudioDevice);
        return;
    }
    let mut fixture_sink =
        FixtureNullSinkInTheSession::add().expect("the fixture adds its null sink");
    let monitor = format!("{}.monitor", fixture_sink.sink_name);
    let mut capture_stream =
        open_capture_stream_in_requested_format(backend.as_ref(), &a_capture_request_for(&monitor))
            .unwrap_or_else(|refusal| panic!("'{monitor}' did not open: {refusal}"));
    let liveness_report = capture_stream.liveness_report();
    let (delivered_sender, delivered_receiver) = mpsc::channel();
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let _ = delivered_sender.send(block.sample_count);
        }))
        .expect("the monitor delivers");
    delivered_receiver
        .recv_timeout(FIRST_BLOCK_DEADLINE)
        .unwrap_or_else(|_| panic!("'{monitor}' delivered no block in {FIRST_BLOCK_DEADLINE:?}"));

    fixture_sink
        .remove()
        .expect("the fixture removes its null sink");
    let removed_at = std::time::Instant::now();
    while liveness_report.failure_that_ended_the_stream().is_none()
        && removed_at.elapsed() < REMOVED_DEVICE_FAILURE_DEADLINE
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    let failure = liveness_report.failure_that_ended_the_stream();
    let _ = delivered_receiver.try_iter().count();
    std::thread::sleep(AFTER_REMOVAL_DELIVERY_WATCH);
    let blocks_after_the_failure = delivered_receiver.try_iter().count();
    capture_stream.stop_delivering().expect("the stream stops");

    let reason = failure
        .unwrap_or_else(|| {
            panic!(
                "'{monitor}' went away and the stream reported no failure in \
                 {REMOVED_DEVICE_FAILURE_DEADLINE:?}"
            )
        })
        .to_string();
    eprintln!(
        "'{monitor}' went away, recorded by the shim's {} path: {reason}",
        if PIPEWIRE_FORMAT_PATH_REASONS
            .iter()
            .any(|format_reason| reason.contains(format_reason))
        {
            "format"
        } else {
            "error-state"
        }
    );
    assert!(
        reason.contains(PIPEWIRE_STREAM_FAILURE_PREFIX),
        "the reason is the PipeWire arm's: {reason}"
    );
    assert_eq!(
        blocks_after_the_failure, 0,
        "'{monitor}' went away and the stream went on delivering from another device"
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
    let listing = backend.list_capture_devices().expect("the session answers");
    let default_output_monitor = listing
        .default_output_monitor_device_id
        .clone()
        .expect("the session has a default sink");
    let ids: Vec<String> = listing
        .devices_with_the_default_output_monitor_alias()
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

/// The tone played into the default output: loud enough to read back off its monitor far above
/// silence, and quiet through a speaker.
#[cfg(target_os = "linux")]
const TONE_AMPLITUDE: f32 = 0.1;

#[cfg(target_os = "linux")]
const TONE_FREQUENCY_HZ: f32 = 440.0;

/// How long the monitor is captured while the tone plays.
#[cfg(target_os = "linux")]
const MONITOR_CAPTURE_DURATION: Duration = Duration::from_millis(1500);

/// The peak the monitor must reach: half the tone's, far above a silent output's zeros.
#[cfg(target_os = "linux")]
const MONITOR_PEAK_MINIMUM: f32 = TONE_AMPLITUDE / 2.0;

/// The largest sample magnitude in one block of interleaved little-endian scalars.
#[cfg(target_os = "linux")]
fn peak_of(interleaved_sample_bytes: &[u8], sample_format: AudioSampleFormat) -> f32 {
    match sample_format {
        AudioSampleFormat::F32 => interleaved_sample_bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|scalar| f32::from_le_bytes(*scalar).abs())
            .fold(0.0, f32::max),
        AudioSampleFormat::I16 => interleaved_sample_bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|scalar| f32::from(i16::from_le_bytes(*scalar)).abs() / 32_768.0)
            .fold(0.0, f32::max),
    }
}

#[cfg(target_os = "linux")]
#[test]
#[cfg_attr(
    not(feature = "audible-hardware-tests"),
    ignore = "audible unless the default sink is a null sink — plays a tone into the default output and captures the default output's monitor. Run with --features streamlib-engine/audible-hardware-tests"
)]
fn the_alias_captures_what_the_default_output_plays() {
    let Some(backend) = a_real_audio_arm_or_skip() else {
        return;
    };
    if backend.backend_name() != "pipewire" {
        skip_because_this_machine_lacks(RigOnlyCapability::AudioDevice);
        return;
    }

    let mut playback_stream = open_playback_stream_in_requested_format(
        backend.as_ref(),
        &AudioDeviceStreamRequest {
            device_id: None,
            deviceless_pacing_clock: Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(
                48_000, 512,
            ))),
            capture_device_access_prompting: CaptureDeviceAccessPrompting::MayAskTheUser,
            requested_format: RequestedAudioStreamFormat {
                sample_rate: Some(48_000),
                channels: Some(1),
                sample_format: Some(AudioSampleFormat::F32),
            },
        },
    )
    .expect("the default output opens");
    let samples_played = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let samples_played_by_the_hand_off = Arc::clone(&samples_played);
    playback_stream
        .start_requesting_from(Box::new(
            move |requested: AudioBlockRequestedByDevice<'_>| {
                let first_sample = samples_played_by_the_hand_off.fetch_add(
                    u64::from(requested.sample_count),
                    std::sync::atomic::Ordering::Relaxed,
                );
                for (offset, scalar) in requested
                    .interleaved_sample_bytes_to_fill
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .enumerate()
                {
                    let phase =
                        (first_sample + offset as u64) as f32 * TONE_FREQUENCY_HZ / 48_000.0;
                    let sample = TONE_AMPLITUDE * (phase * std::f32::consts::TAU).sin();
                    *scalar = sample.to_le_bytes();
                }
            },
        ))
        .expect("the default output asks for samples");

    let mut capture_stream = open_capture_stream_in_requested_format(
        backend.as_ref(),
        &a_capture_request_for(DEFAULT_OUTPUT_MONITOR_DEVICE_ID),
    )
    .expect("the default output's monitor opens through the alias");
    let sample_format = capture_stream.stream_format().sample_format;
    let (peak_sender, peak_receiver) = mpsc::channel();
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let _ = peak_sender.send(peak_of(block.interleaved_sample_bytes, sample_format));
        }))
        .expect("the monitor delivers");
    std::thread::sleep(MONITOR_CAPTURE_DURATION);
    capture_stream.stop_delivering().expect("the monitor stops");
    playback_stream.stop_requesting().expect("the output stops");

    let peak = peak_receiver.try_iter().fold(0.0, f32::max);
    eprintln!(
        "the alias captured a peak of {peak:.3} while the default output played a {TONE_AMPLITUDE} \
         tone"
    );
    assert!(
        peak >= MONITOR_PEAK_MINIMUM,
        "the alias captured a peak of {peak}, below {MONITOR_PEAK_MINIMUM}: it is not the monitor \
         of the output the tone played into"
    );
}
