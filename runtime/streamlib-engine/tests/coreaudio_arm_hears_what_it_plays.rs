// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The CoreAudio arm through the air: a tone played through the default output
//! reaches the default input, both opened through the seam.
//!
//! The contract suites prove cadence and stop semantics over silence; this is
//! the one test that proves the samples themselves — a known tone leaves the
//! speaker and the microphone hears it, at the frequency played, well above
//! the room's own level at that frequency.
//!
//! Audio tier — needs a Mac whose default output is audible to its default
//! input (built-in speakers and microphone), microphone access already allowed
//! for the terminal running it, and a room quiet enough for a tone at about −8 dBFS
//! to stand out. It is audible by design.

#![cfg(target_os = "macos")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib_engine::core::context::{
    AudioBlockRequestedByDevice, AudioClockConfig, AudioDeviceStreamRequest, AudioSampleFormat,
    AudioStreamFormat, CapturedAudioBlockFromDevice, SharedAudioClock, SharedAudioDeviceBackend,
    SoftwareAudioClock, probe_audio_device_backend,
};

/// The two tones played, in order, each for [`TONE_DURATION`].
const TONE_FREQUENCIES_HZ: [f64; 2] = [1000.0, 2000.0];

const TONE_DURATION: Duration = Duration::from_millis(2000);

/// The room, recorded before anything plays: the baseline each tone is judged
/// against.
const ROOM_BASELINE_DURATION: Duration = Duration::from_millis(1000);

/// About −8 dBFS: clearly audible through laptop speakers at an ordinary
/// system volume. The tones sit where small drivers reproduce well.
const TONE_AMPLITUDE: f32 = 0.4;

/// How much stronger a tone must be at its own frequency while it plays than
/// the room was at that frequency. 20 dB is far above what room noise or a
/// fan produces at a pure tone, and far below what an acoustic path loses.
const TONE_OVER_ROOM_MINIMUM_DB: f64 = 20.0;

/// How much stronger the played tone must be than the other tone's frequency
/// in the same window — the check that the microphone heard *this* tone rather
/// than broadband noise.
const TONE_OVER_OTHER_TONE_MINIMUM_DB: f64 = 12.0;

/// Captured audio skipped at the start of each tone window, covering the
/// output and input latency plus the speaker's ramp.
const SETTLE_AT_EACH_WINDOW_START: Duration = Duration::from_millis(300);

fn coreaudio_arm() -> Option<SharedAudioDeviceBackend> {
    let backend = probe_audio_device_backend();
    (backend.backend_name() == "coreaudio").then_some(backend)
}

fn an_unused_deviceless_pacing_clock() -> SharedAudioClock {
    Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(48_000, 512)))
}

/// Power of `frequency_hz` in `samples` by the Goertzel recurrence, normalised
/// by length so windows of different lengths compare.
fn goertzel_power(samples: &[f32], sample_rate: u32, frequency_hz: f64) -> f64 {
    let coefficient =
        2.0 * (2.0 * std::f64::consts::PI * frequency_hz / f64::from(sample_rate)).cos();
    let (mut previous, mut before_previous) = (0.0f64, 0.0f64);
    for &sample in samples {
        let current = f64::from(sample) + coefficient * previous - before_previous;
        before_previous = previous;
        previous = current;
    }
    let power = previous * previous + before_previous * before_previous
        - coefficient * previous * before_previous;
    power / (samples.len() as f64).powi(2)
}

fn decibels(ratio: f64) -> f64 {
    10.0 * ratio.max(1e-30).log10()
}

fn read_f32_frame_first_channel(interleaved: &[u8], format: AudioStreamFormat) -> Vec<f32> {
    assert_eq!(format.sample_format, AudioSampleFormat::F32);
    let frame_bytes = format.interleaved_byte_count_for(1);
    interleaved
        .chunks_exact(frame_bytes)
        .map(|frame| f32::from_le_bytes(frame[..4].try_into().expect("four bytes")))
        .collect()
}

/// The tone generator the playback hand-off reads: which tone, and where in
/// its cycle, advanced one frame at a time.
struct TonePlayback {
    frequency_hz: Option<f64>,
    phase: f64,
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — audible: plays two tones through the default output and needs the default input to hear them, with microphone access allowed. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_tone_played_through_the_default_output_is_heard_by_the_default_input() {
    let Some(backend) = coreaudio_arm() else {
        return;
    };
    let request = AudioDeviceStreamRequest {
        device_id: None,
        deviceless_pacing_clock: an_unused_deviceless_pacing_clock(),
    };
    let mut playback_stream = backend
        .open_playback_stream(&request)
        .expect("the default output opens");
    let mut capture_stream = backend
        .open_capture_stream(&request)
        .expect("the default input opens");
    let playback_format = playback_stream.stream_format();
    let capture_format = capture_stream.stream_format();
    println!("playback format: {playback_format:?}");
    println!("capture format:  {capture_format:?}");

    let captured: Arc<Mutex<Vec<f32>>> = Arc::default();
    let captured_by_hand_off = Arc::clone(&captured);
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let samples = read_f32_frame_first_channel(block.interleaved_sample_bytes, capture_format);
            captured_by_hand_off
                .lock()
                .expect("unpoisoned")
                .extend_from_slice(&samples);
        }))
        .expect("capture starts");

    let tone = Arc::new(Mutex::new(TonePlayback {
        frequency_hz: None,
        phase: 0.0,
    }));
    let tone_for_hand_off = Arc::clone(&tone);
    let playback_rate = f64::from(playback_format.sample_rate);
    let playback_channels = playback_format.channels as usize;
    playback_stream
        .start_requesting_from(Box::new(move |requested: AudioBlockRequestedByDevice<'_>| {
            let mut tone = tone_for_hand_off.lock().expect("unpoisoned");
            for frame in requested
                .interleaved_sample_bytes_to_fill
                .chunks_exact_mut(4 * playback_channels)
            {
                let sample = match tone.frequency_hz {
                    Some(frequency_hz) => {
                        tone.phase = (tone.phase + frequency_hz / playback_rate).fract();
                        TONE_AMPLITUDE * (2.0 * std::f64::consts::PI * tone.phase).sin() as f32
                    }
                    None => 0.0,
                };
                for channel in frame.chunks_exact_mut(4) {
                    channel.copy_from_slice(&sample.to_le_bytes());
                }
            }
        }))
        .expect("playback starts");

    // Each window is marked by where the capture stood when it began, so the
    // analysis never guesses at timing.
    let mut windows: Vec<(Option<f64>, usize, usize)> = Vec::new();
    let captured_len = || captured.lock().expect("unpoisoned").len();

    let room_start = captured_len();
    std::thread::sleep(ROOM_BASELINE_DURATION);
    windows.push((None, room_start, captured_len()));
    for frequency_hz in TONE_FREQUENCIES_HZ {
        tone.lock().expect("unpoisoned").frequency_hz = Some(frequency_hz);
        let start = captured_len();
        std::thread::sleep(TONE_DURATION);
        windows.push((Some(frequency_hz), start, captured_len()));
    }
    tone.lock().expect("unpoisoned").frequency_hz = None;

    playback_stream.stop_requesting().expect("playback stops");
    capture_stream.stop_delivering().expect("capture stops");

    let captured = captured.lock().expect("unpoisoned").clone();
    let settle = (capture_format.sample_rate as f64 * SETTLE_AT_EACH_WINDOW_START.as_secs_f64())
        as usize;
    let window_samples = |start: usize, end: usize| -> &[f32] {
        let start = (start + settle).min(end);
        &captured[start..end]
    };
    let (_, room_start, room_end) = windows[0];
    let room = window_samples(room_start, room_end);
    assert!(
        !room.is_empty(),
        "the microphone delivered nothing in the room window — is microphone access allowed?"
    );
    let rms = |samples: &[f32]| {
        (samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len().max(1) as f64)
            .sqrt()
    };
    println!(
        "room: {} samples, rms {:.1} dBFS",
        room.len(),
        20.0 * rms(room).max(1e-15).log10()
    );

    for &(frequency_hz, start, end) in &windows[1..] {
        let frequency_hz = frequency_hz.expect("tone windows carry their frequency");
        let other_hz = TONE_FREQUENCIES_HZ
            .into_iter()
            .find(|&other| other != frequency_hz)
            .expect("two tones");
        let heard = window_samples(start, end);
        let tone_power = goertzel_power(heard, capture_format.sample_rate, frequency_hz);
        let room_power = goertzel_power(room, capture_format.sample_rate, frequency_hz);
        let other_power = goertzel_power(heard, capture_format.sample_rate, other_hz);
        let tone_over_room_db = decibels(tone_power / room_power);
        let tone_over_other_db = decibels(tone_power / other_power);
        println!(
            "{frequency_hz} Hz: {} samples, rms {:.1} dBFS, tone over room {tone_over_room_db:.1} dB, \
             tone over {other_hz} Hz {tone_over_other_db:.1} dB",
            heard.len(),
            20.0 * rms(heard).max(1e-15).log10()
        );
        assert!(
            tone_over_room_db >= TONE_OVER_ROOM_MINIMUM_DB,
            "the microphone heard {frequency_hz} Hz only {tone_over_room_db:.1} dB above the room \
             while it played — the speaker or the microphone path is not carrying the signal"
        );
        assert!(
            tone_over_other_db >= TONE_OVER_OTHER_TONE_MINIMUM_DB,
            "while {frequency_hz} Hz played the microphone heard it only {tone_over_other_db:.1} dB \
             over {other_hz} Hz — that is noise, not the tone"
        );
    }
}
