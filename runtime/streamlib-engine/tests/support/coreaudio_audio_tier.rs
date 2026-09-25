// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What every CoreAudio audio-tier test settles before it measures: that the
//! arm under test is the one the chain chose, which device it will measure,
//! that the microphone may be used, and where its numbers are printed.

// Each test binary compiles its own copy and uses part of it.
#![allow(dead_code)]

use streamlib_engine::apple_coreaudio_audio_tier::{
    CoreAudioStreamDirection, built_in_audio_device_uid, default_audio_device_uid,
    microphone_access_for_a_capture_hardware_test,
};
use streamlib_engine::core::context::{SharedAudioDeviceBackend, probe_audio_device_backend};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Print a measured number or a cannot-run reason into the test's own output:
/// evidence for whoever reads the run, not a library log line.
#[allow(clippy::disallowed_macros)]
pub fn print_for_the_evidence_record(line: impl std::fmt::Display) {
    println!("{line}");
}

fn device_word(direction: CoreAudioStreamDirection) -> &'static str {
    match direction {
        CoreAudioStreamDirection::Capture => "input",
        CoreAudioStreamDirection::Playback => "output",
    }
}

/// Route the engine's tracing into this test's output, filtered by
/// `RUST_LOG` (info otherwise), so a run quotes the probe line and the device
/// each stream opened.
fn the_engine_logs_into_this_tests_output() {
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with(tracing_subscriber::fmt::layer().with_test_writer())
        .try_init();
}

/// The chain's arm, which must be CoreAudio on a Mac that lists a device the
/// test can use: any other arm answering would make the test pass vacuously.
pub fn the_coreaudio_arm() -> SharedAudioDeviceBackend {
    the_engine_logs_into_this_tests_output();
    let backend = probe_audio_device_backend();
    assert_eq!(
        backend.backend_name(),
        "coreaudio",
        "CoreAudio lists a device this test can use, but the audio backend chain chose '{}' — \
         the arm under test was never probed",
        backend.backend_name()
    );
    print_for_the_evidence_record(format!(
        "audio device backend chain probed audio_backend={}",
        backend.backend_name()
    ));
    backend
}

/// The CoreAudio arm, or `None` — said so — when CoreAudio lists no default
/// device in `direction`, the one environment these tests cannot ask their
/// question in.
pub fn the_coreaudio_arm_with_a_default_device_for(
    direction: CoreAudioStreamDirection,
) -> Option<SharedAudioDeviceBackend> {
    let Some(default_device_uid) = default_audio_device_uid(direction) else {
        print_for_the_evidence_record(format!(
            "cannot run: CoreAudio lists no default {} device",
            device_word(direction)
        ));
        return None;
    };
    let backend = the_coreaudio_arm();
    print_for_the_evidence_record(format!(
        "default {} device: {default_device_uid}",
        device_word(direction)
    ));
    Some(backend)
}

/// The UID of this Mac's own built-in device in `direction`, or `None` —
/// said so — when it has none.
pub fn the_built_in_device_uid_for(direction: CoreAudioStreamDirection) -> Option<String> {
    let uid = built_in_audio_device_uid(direction);
    match &uid {
        Some(uid) => print_for_the_evidence_record(format!(
            "built-in {} device: {uid}",
            device_word(direction)
        )),
        None => print_for_the_evidence_record(format!(
            "cannot run: this Mac lists no built-in {} device",
            device_word(direction)
        )),
    }
    uid
}

/// Fail naming what the person at the machine must do when the microphone is
/// not allowed yet, rather than on a capture deadline.
pub fn the_microphone_must_be_allowed() {
    if let Err(instruction) = microphone_access_for_a_capture_hardware_test() {
        panic!("{instruction}");
    }
}

/// Root-mean-square of `samples`, in dB relative to full scale.
pub fn rms_dbfs(samples: &[f32]) -> f64 {
    let mean_square = samples
        .iter()
        .map(|&sample| f64::from(sample).powi(2))
        .sum::<f64>()
        / samples.len().max(1) as f64;
    10.0 * mean_square.max(1e-30).log10()
}
