// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The analysis this test leans on, proven without a device: the FFT and the sinusoid fit catch
//! a block lost or repeated anywhere in the span, the pilot gate arms the tone only once a block
//! carrying the pilot comes back through the tap, and a pilot that never comes back is blamed on
//! the path that lost it.

use super::*;

/// A deterministic ±`amplitude` noise source, so the analysis tests need no
/// random-number crate and never flake.
fn deterministic_noise(sample_count: usize, amplitude: f32) -> Vec<f32> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    (0..sample_count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            amplitude * ((state >> 40) as f32 / (1u64 << 23) as f32 - 1.0)
        })
        .collect()
}

/// Mono samples of `tone`, generated the way the playback hand-off generates
/// them, one device request at a time.
fn samples_generated_in_device_requests(
    tone: &mut PhaseContinuousSineTone,
    request_frame_counts: impl IntoIterator<Item = usize>,
) -> Vec<f32> {
    let mut samples = Vec::new();
    for frame_count in request_frame_counts {
        let mut request = vec![0u8; frame_count * 4 * 2];
        tone.fill_interleaved_f32(&mut request, 2);
        samples.extend(
            f32_samples_of(&request)
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[first_channel, _]| *first_channel),
        );
    }
    samples
}

fn tone_generated_in_device_requests(
    frequency_hz: f64,
    amplitude: f32,
    sample_rate: u32,
    request_frame_counts: impl IntoIterator<Item = usize>,
) -> Vec<f32> {
    samples_generated_in_device_requests(
        &mut PhaseContinuousSineTone::new(frequency_hz, amplitude, sample_rate),
        request_frame_counts,
    )
}

fn one_and_a_half_seconds_of_440_hz_at_48_khz() -> Vec<f32> {
    tone_generated_in_device_requests(440.0, 0.5, 48_000, [512; 141])
}

#[test]
fn the_dominant_frequency_between_fft_bins_is_found_to_a_twentieth_of_a_hertz() {
    let sample_rate = 48_000;
    let noise = deterministic_noise(72_000, 0.01);
    let samples: Vec<f32> = noise
        .iter()
        .enumerate()
        .map(|(index, &noise_sample)| {
            let seconds = index as f64 / f64::from(sample_rate);
            (0.5 * (2.0 * PI * 997.3 * seconds).sin() + 0.05 * (2.0 * PI * 3000.0 * seconds).sin())
                as f32
                + noise_sample
        })
        .collect();
    let dominant_hz = dominant_frequency_hz(&samples, sample_rate);
    assert!(
        (dominant_hz - 997.3).abs() < 0.05,
        "found {dominant_hz} Hz for a 997.3 Hz tone"
    );
}

#[test]
fn a_tone_generated_one_device_request_at_a_time_is_one_unbroken_sinusoid() {
    let samples = tone_generated_in_device_requests(
        440.0,
        0.5,
        48_000,
        [512, 471, 1, 1024, 333, 4096].into_iter().cycle().take(112),
    );
    let fitted = fit_a_sinusoid(&samples, 48_000, dominant_frequency_hz(&samples, 48_000))
        .expect("a clean tone fits");
    assert!((fitted.frequency_hz - 440.0).abs() < 1e-4, "{fitted:?}");
    assert!((fitted.amplitude - 0.5).abs() < 1e-5, "{fitted:?}");
    assert!(fitted.dc_offset.abs() < 1e-6, "{fitted:?}");
    assert!(
        fitted.residual_relative_to_the_tone_db() < -100.0,
        "a phase-continuous tone left a {:.1} dB residual",
        fitted.residual_relative_to_the_tone_db()
    );
}

#[test]
fn a_block_lost_in_the_middle_lifts_the_residual_past_the_limit() {
    let mut samples = one_and_a_half_seconds_of_440_hz_at_48_khz();
    samples.drain(36_000..36_512);
    let fitted = fit_a_sinusoid(&samples, 48_000, dominant_frequency_hz(&samples, 48_000))
        .expect("a tone with a hole still fits something");
    assert!(
        fitted.residual_relative_to_the_tone_db() > MAX_SINE_FIT_RESIDUAL_DB,
        "a lost block left only a {:.1} dB residual",
        fitted.residual_relative_to_the_tone_db()
    );
}

#[test]
fn a_block_lost_near_the_end_of_the_span_still_lifts_the_residual_past_the_limit() {
    let mut samples = one_and_a_half_seconds_of_440_hz_at_48_khz();
    let near_the_end = samples.len() - 1_000;
    samples.drain(near_the_end..near_the_end + 512);
    let fitted = fit_a_sinusoid(&samples, 48_000, dominant_frequency_hz(&samples, 48_000))
        .expect("a tone with a hole still fits something");
    assert!(
        fitted.residual_relative_to_the_tone_db() > MAX_SINE_FIT_RESIDUAL_DB,
        "a block lost near the end left only a {:.1} dB residual",
        fitted.residual_relative_to_the_tone_db()
    );
}

/// The live test fits everything from the settled tone to the end of about
/// 2.4 s of capture, so a loss in its last full block has to show there too.
#[test]
fn a_block_lost_one_block_before_the_end_of_the_whole_capture_lifts_the_residual_past_the_limit() {
    let mut samples = tone_generated_in_device_requests(440.0, 0.5, 48_000, [512; 225]);
    let one_block_before_the_end = samples.len() - 2 * 512;
    samples.drain(one_block_before_the_end..one_block_before_the_end + 512);
    let fitted = fit_a_sinusoid(&samples, 48_000, dominant_frequency_hz(&samples, 48_000))
        .expect("a tone with a hole still fits something");
    assert!(
        fitted.residual_relative_to_the_tone_db() > MAX_SINE_FIT_RESIDUAL_DB,
        "a block lost one block before the end left only a {:.1} dB residual",
        fitted.residual_relative_to_the_tone_db()
    );
}

#[test]
fn a_repeated_block_lifts_the_residual_past_the_limit() {
    let mut samples = one_and_a_half_seconds_of_440_hz_at_48_khz();
    let repeated_block = samples[36_000..36_512].to_vec();
    samples.splice(36_512..36_512, repeated_block);
    let fitted = fit_a_sinusoid(&samples, 48_000, dominant_frequency_hz(&samples, 48_000))
        .expect("a tone with a repeat still fits something");
    assert!(
        fitted.residual_relative_to_the_tone_db() > MAX_SINE_FIT_RESIDUAL_DB,
        "a repeated block left only a {:.1} dB residual",
        fitted.residual_relative_to_the_tone_db()
    );
}

#[test]
fn digital_silence_fits_no_sinusoid_and_carries_no_sound() {
    let silence = vec![0.0f32; 72_000];
    assert!(fit_a_sinusoid(&silence, 48_000, 440.0).is_none());
    assert_eq!(first_frame_louder_than(&silence, 2, 0.0), None);
}

#[test]
fn the_first_frame_louder_than_a_level_is_the_frame_not_the_sample() {
    let mut interleaved_samples = vec![0.0f32; 20];
    interleaved_samples[13] = -1e-9;
    assert_eq!(
        first_frame_louder_than(&interleaved_samples, 2, 0.0),
        Some(6)
    );
}

/// Every sample of the pilot is non-zero, for the tap to show it reads, and
/// none reaches the tone's onset level, which the tone's first sample does.
#[test]
fn the_pilot_carries_sound_and_is_never_taken_for_the_tones_onset() {
    let pilot_frames = 48_000;
    let mut pilot_then_tone = samples_generated_in_device_requests(
        &mut PhaseContinuousSineTone::new(440.0, SUB_AUDIBLE_PILOT_AMPLITUDE, 48_000),
        [512; 93].into_iter().chain([pilot_frames - 512 * 93]),
    );
    assert!(pilot_then_tone.iter().all(|&sample| sample != 0.0));
    pilot_then_tone.extend(tone_generated_in_device_requests(
        440.0,
        TONE_AMPLITUDE,
        48_000,
        [512; 4],
    ));
    assert_eq!(
        first_frame_louder_than(&pilot_then_tone, 1, TONE_ONSET_LEVEL),
        Some(pilot_frames)
    );
}

fn stereo_tap_block_of(mono_samples: impl IntoIterator<Item = f32>) -> CapturedTapBlock {
    CapturedTapBlock {
        first_sample_timestamp_ns: 0,
        interleaved_samples: mono_samples
            .into_iter()
            .flat_map(|sample| [sample, sample])
            .collect(),
    }
}

#[test]
fn a_tap_returning_only_digital_zeros_fails_the_pilot_gate_at_its_budget_and_never_arms_the_tone() {
    let signal_the_playback_hand_off_writes =
        Cell::new(SignalThePlaybackHandOffWrites::DigitalSilence);
    let mut signals_armed_when_each_block_was_pulled = Vec::new();
    let mut captured_blocks = Vec::new();
    let pilot_through_the_tap = arm_the_tone_only_once_the_pilot_comes_back_through_the_tap(
        || {
            signals_armed_when_each_block_was_pulled
                .push(signal_the_playback_hand_off_writes.get());
            stereo_tap_block_of([0.0; 512])
        },
        |signal| signal_the_playback_hand_off_writes.set(signal),
        2,
        48_000,
        &mut captured_blocks,
    );
    assert_eq!(
        pilot_through_the_tap,
        SubAudiblePilotThroughTheTap::NeverCameBack {
            captured_frames_after_arming_the_pilot: 94 * 512,
        }
    );
    assert_eq!(
        signals_armed_when_each_block_was_pulled,
        [SignalThePlaybackHandOffWrites::SubAudiblePilot; 94]
    );
    assert_eq!(
        signal_the_playback_hand_off_writes.get(),
        SignalThePlaybackHandOffWrites::SubAudiblePilot
    );
    assert_eq!(captured_blocks.len(), 94);
}

#[test]
fn the_tone_is_armed_only_after_a_block_carrying_the_pilot_comes_back_through_the_tap() {
    let signal_the_playback_hand_off_writes =
        Cell::new(SignalThePlaybackHandOffWrites::DigitalSilence);
    let mut signals_armed_when_each_block_was_pulled = Vec::new();
    let mut pilot = PhaseContinuousSineTone::new(440.0, SUB_AUDIBLE_PILOT_AMPLITUDE, 48_000);
    let mut captured_blocks = Vec::new();
    let pilot_through_the_tap = arm_the_tone_only_once_the_pilot_comes_back_through_the_tap(
        || {
            signals_armed_when_each_block_was_pulled
                .push(signal_the_playback_hand_off_writes.get());
            if signals_armed_when_each_block_was_pulled.len() <= 3 {
                stereo_tap_block_of([0.0; 512])
            } else {
                stereo_tap_block_of(
                    [0.0; 100]
                        .into_iter()
                        .chain(samples_generated_in_device_requests(&mut pilot, [412])),
                )
            }
        },
        |signal| signal_the_playback_hand_off_writes.set(signal),
        2,
        48_000,
        &mut captured_blocks,
    );
    let SubAudiblePilotThroughTheTap::CameBack {
        captured_frames_after_arming_the_pilot,
        peak_in_its_first_block,
    } = pilot_through_the_tap
    else {
        panic!("a tap that returned the pilot failed the gate: {pilot_through_the_tap:?}");
    };
    assert_eq!(captured_frames_after_arming_the_pilot, 3 * 512 + 100);
    assert!(
        peak_in_its_first_block > 0.0 && peak_in_its_first_block <= SUB_AUDIBLE_PILOT_AMPLITUDE
    );
    assert_eq!(
        signals_armed_when_each_block_was_pulled,
        [SignalThePlaybackHandOffWrites::SubAudiblePilot; 4]
    );
    assert_eq!(
        signal_the_playback_hand_off_writes.get(),
        SignalThePlaybackHandOffWrites::Tone
    );
    assert_eq!(captured_blocks.len(), 4);
}

#[test]
fn a_pilot_the_playback_was_never_asked_for_is_blamed_on_the_playback_path_not_on_system_audio_recording()
 {
    let why = why_the_pilot_never_came_back_through_the_tap(48_128, 0, "Terminal");
    assert!(why.contains("playback path"), "{why}");
    assert!(!why.contains(SYSTEM_AUDIO_RECORDING_SETTING), "{why}");
    assert!(!why.contains("Terminal"), "{why}");
}

#[test]
fn a_written_pilot_the_tap_returned_as_zeros_names_system_audio_recording_for_the_responsible_application()
 {
    let why = why_the_pilot_never_came_back_through_the_tap(48_128, 47_616, "Terminal");
    assert!(
        why.contains(&format!(
            "allow Terminal in {SYSTEM_AUDIO_RECORDING_SETTING}"
        )),
        "{why}"
    );
    assert!(why.contains("wrote 47616 frames"), "{why}");
}

#[test]
fn the_playback_hand_off_counts_the_pilot_frames_it_writes_apart_from_the_tones() {
    let mut playback_progress = PlaybackHandOffProgress::writing_digital_silence(48_000);
    let mut device_buffer = vec![0u8; 512 * 2 * 4];
    for signal in [
        SignalThePlaybackHandOffWrites::DigitalSilence,
        SignalThePlaybackHandOffWrites::SubAudiblePilot,
        SignalThePlaybackHandOffWrites::SubAudiblePilot,
        SignalThePlaybackHandOffWrites::Tone,
    ] {
        playback_progress.now_writing = signal;
        playback_progress.fill_the_devices_request(
            AudioBlockRequestedByDevice {
                interleaved_sample_bytes_to_fill: &mut device_buffer,
                sample_count: 512,
            },
            2,
        );
    }
    assert_eq!(playback_progress.pilot_frames_written, 1024);
    assert_eq!(playback_progress.tone_frames_written, 512);
}
