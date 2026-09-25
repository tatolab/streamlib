// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The CoreAudio arm carries the built-in microphone's own signal rather than
//! digital silence: over a second of an ordinary room, its level sits above
//! zero.
//!
//! CoreAudio only. The shared capture suite stays content-free because a
//! PipeWire monitor of a silent sink is legitimately all zeros; a CoreAudio
//! stream that hands off zeros — a render that never wrote, a microphone the
//! system silences behind the process's back — passes every cadence and stamp
//! check and fails this one.
//!
//! Audio tier — needs a Mac with a built-in microphone and microphone access
//! allowed. Silent: it only captures, and the room is the signal.

#![cfg(target_os = "macos")]

use std::sync::{Arc, mpsc};
use std::time::Duration;

use streamlib_engine::apple_coreaudio_audio_tier::CoreAudioStreamDirection;
use streamlib_engine::core::context::{
    AudioClockConfig, AudioDeviceStreamRequest, AudioSampleFormat, CapturedAudioBlockFromDevice,
    SoftwareAudioClock,
};

#[path = "support/coreaudio_audio_tier.rs"]
mod coreaudio_audio_tier;
use coreaudio_audio_tier::{
    print_for_the_evidence_record, rms_dbfs, the_built_in_device_uid_for, the_coreaudio_arm,
    the_microphone_must_be_allowed,
};

/// Captured audio discarded before measuring, past the device's start-up.
const SETTLE_BEFORE_MEASURING: Duration = Duration::from_millis(250);

const MEASURED_DURATION: Duration = Duration::from_secs(1);

/// A capture that has produced nothing in this long is a broken device, not a
/// slow one.
const CAPTURE_DEADLINE: Duration = Duration::from_secs(10);

/// Far below any analog front end's own noise, and far above the exact zeros
/// a stream carrying no signal hands off.
const DIGITAL_SILENCE_CEILING_DBFS: f64 = -120.0;

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — needs a Mac with a built-in microphone and microphone access allowed. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn the_built_in_microphone_carries_more_than_digital_silence() {
    let Some(microphone_uid) = the_built_in_device_uid_for(CoreAudioStreamDirection::Capture)
    else {
        return;
    };
    let backend = the_coreaudio_arm();
    the_microphone_must_be_allowed();

    let mut capture_stream = backend
        .open_capture_stream(&AudioDeviceStreamRequest {
            device_id: Some(microphone_uid),
            deviceless_pacing_clock: Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(
                48_000, 512,
            ))),
        })
        .expect("the built-in microphone opens by its UID");
    let capture_format = capture_stream.stream_format();
    print_for_the_evidence_record(format!("capture format: {capture_format:?}"));
    assert_eq!(capture_format.sample_format, AudioSampleFormat::F32);

    let (block_sender, block_receiver) = mpsc::channel::<Vec<f32>>();
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let scalars = block
                .interleaved_sample_bytes
                .chunks_exact(4)
                .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four bytes")))
                .collect();
            let _ = block_sender.send(scalars);
        }))
        .expect("capture starts");

    let scalars_per_second = (capture_format.sample_rate * capture_format.channels) as f64;
    let settle_scalars = (scalars_per_second * SETTLE_BEFORE_MEASURING.as_secs_f64()) as usize;
    let measured_scalars = (scalars_per_second * MEASURED_DURATION.as_secs_f64()) as usize;
    let mut captured: Vec<f32> = Vec::with_capacity(settle_scalars + measured_scalars);
    while captured.len() < settle_scalars + measured_scalars {
        let scalars = block_receiver
            .recv_timeout(CAPTURE_DEADLINE)
            .unwrap_or_else(|_| {
                panic!(
                    "the built-in microphone delivered nothing for {CAPTURE_DEADLINE:?}. Liveness: \
                 {:?}",
                    capture_stream
                        .liveness_report()
                        .failure_that_ended_the_stream()
                )
            });
        captured.extend(scalars);
    }
    capture_stream.stop_delivering().expect("capture stops");

    let measured = &captured[settle_scalars..];
    let level_dbfs = rms_dbfs(measured);
    let peak = measured
        .iter()
        .fold(0.0f32, |peak, &scalar| peak.max(scalar.abs()));
    let exact_zero_count = measured.iter().filter(|&&scalar| scalar == 0.0).count();
    print_for_the_evidence_record(format!(
        "built-in microphone over {:.2} s: rms {level_dbfs:.1} dBFS, peak {:.1} dBFS, \
         {exact_zero_count} of {} scalars exactly zero",
        measured.len() as f64 / scalars_per_second,
        20.0 * f64::from(peak).max(1e-15).log10(),
        measured.len()
    ));

    assert!(
        capture_stream
            .liveness_report()
            .failure_that_ended_the_stream()
            .is_none()
    );
    assert!(
        exact_zero_count < measured.len(),
        "every captured scalar is exactly zero — the stream carried digital silence, not the \
         microphone"
    );
    assert!(
        level_dbfs > DIGITAL_SILENCE_CEILING_DBFS,
        "the built-in microphone measured {level_dbfs:.1} dBFS, at or below the \
         {DIGITAL_SILENCE_CEILING_DBFS} dBFS no analog front end is quiet enough to reach"
    );
}
