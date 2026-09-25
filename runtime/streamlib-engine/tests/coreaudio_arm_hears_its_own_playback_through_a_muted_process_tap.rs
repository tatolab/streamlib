// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The CoreAudio arm's samples, proven digitally and in silence: a tone the
//! arm plays to the default output comes back, sample for sample, through a
//! muted process tap of this process that the arm captures by device UID.
//!
//! The acoustic test hears a tone through the air; this one checks the
//! samples themselves — frequency, level, and one unbroken sinusoid, which a
//! block lost or repeated in either direction breaks — and makes no sound,
//! because the tap mutes this process before its output reaches any device.
//!
//! Audio tier — needs a Mac with a default output device, with microphone
//! access and System Audio Recording allowed for the terminal running it.
//! The first run raises the System Audio Recording prompt.

#![cfg(target_os = "macos")]
// The measured numbers go to stdout: they are the evidence a run records.
#![allow(clippy::disallowed_macros)]

use std::f64::consts::{PI, SQRT_2};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use streamlib_engine::core::context::{
    AudioBlockRequestedByDevice, AudioClockConfig, AudioDeviceStreamRequest, AudioSampleFormat,
    CapturedAudioBlockFromDevice, SharedAudioClock, SharedAudioDeviceBackend, SoftwareAudioClock,
    probe_audio_device_backend,
};
use streamlib_engine::core::media_clock::MediaClock;

mod coreaudio_muted_process_tap_of_this_process;
use coreaudio_muted_process_tap_of_this_process::MutedProcessTapOfThisProcessBehindAPrivateAggregateDevice;

const TONE_FREQUENCY_HZ: f64 = 440.0;

/// The reference amplitude `known_audio_signal.py` plays.
const TONE_AMPLITUDE: f32 = 0.5;

/// Two seconds of tone, plus room for the tap's round trip.
const CAPTURE_AFTER_THE_TONE_IS_ARMED: Duration = Duration::from_millis(2500);

/// Skipped after the tone's first captured sample, so only its steady state
/// is analysed.
const SETTLE_AFTER_THE_TONE_ARRIVES: Duration = Duration::from_millis(100);

const STEADY_TONE_ANALYSED: Duration = Duration::from_millis(1500);

const MAX_FREQUENCY_ERROR_HZ: f64 = 1.0;

/// `known_audio_signal.py`'s loopback bound, about ±0.9 dB. The tap should
/// return the tone at unity: a stereo mixdown of a stream that carries the
/// same sample on every channel is that sample, the tap reads this process's
/// output before the device applies its volume, and drift compensation's
/// resampler is unity gain at 440 Hz.
const MAX_AMPLITUDE_ERROR: f64 = 0.05;

/// A clean digital path fits one sinusoid to within float rounding, far below
/// this. A block lost or repeated jumps the phase of everything after it: a
/// 512-frame loss mid-span leaves about −6 dB, and one 1000 frames from the
/// span's end still about −17 dB.
const MAX_SINE_FIT_RESIDUAL_DB: f64 = -40.0;

/// A capture that has produced nothing in this long is a broken device, not a
/// slow one.
const CAPTURE_DEADLINE: Duration = Duration::from_secs(10);

const SYSTEM_AUDIO_RECORDING_SETTING: &str = "System Settings › Privacy & Security › Screen & \
     System Audio Recording › System Audio Recording Only";

fn coreaudio_arm() -> Option<SharedAudioDeviceBackend> {
    let backend = probe_audio_device_backend();
    (backend.backend_name() == "coreaudio").then_some(backend)
}

fn an_unused_deviceless_pacing_clock() -> SharedAudioClock {
    Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(48_000, 512)))
}

/// The application macOS asks on this process's behalf, as the environment
/// the terminal handed down names it.
fn the_application_macos_asks_for_this_process() -> String {
    ["__CFBundleIdentifier", "TERM_PROGRAM"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "the terminal or application this test was launched from".to_owned())
}

/// A sine whose phase carries from one device request to the next.
struct PhaseContinuousSineTone {
    cycles_per_sample: f64,
    amplitude: f32,
    phase_in_cycles: f64,
}

impl PhaseContinuousSineTone {
    fn new(frequency_hz: f64, amplitude: f32, sample_rate: u32) -> Self {
        Self {
            cycles_per_sample: frequency_hz / f64::from(sample_rate),
            amplitude,
            phase_in_cycles: 0.0,
        }
    }

    /// Fill `interleaved_f32_bytes` with the next frames, the same sample on
    /// every channel.
    fn fill_interleaved_f32(&mut self, interleaved_f32_bytes: &mut [u8], channels: usize) {
        for frame in interleaved_f32_bytes.chunks_exact_mut(4 * channels) {
            self.phase_in_cycles = (self.phase_in_cycles + self.cycles_per_sample).fract();
            let sample = self.amplitude * (2.0 * PI * self.phase_in_cycles).sin() as f32;
            for channel in frame.chunks_exact_mut(4) {
                channel.copy_from_slice(&sample.to_le_bytes());
            }
        }
    }
}

/// What the playback hand-off shares with the test thread.
struct TonePlaybackProgress {
    tone: PhaseContinuousSineTone,
    tone_frames_written: u64,
    /// Monotonic nanoseconds at the hand-off call that wrote the tone's first
    /// frame.
    first_tone_frame_written_at_ns: Option<i64>,
}

/// One block as the tap's aggregate delivered it, copied out of the hand-off.
struct CapturedTapBlock {
    first_sample_timestamp_ns: i64,
    interleaved_samples: Vec<f32>,
}

fn f32_samples_of(interleaved_little_endian_bytes: &[u8]) -> Vec<f32> {
    interleaved_little_endian_bytes
        .chunks_exact(4)
        .map(|scalar| f32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]]))
        .collect()
}

/// The first frame carrying any sample that is not exactly zero.
fn first_frame_carrying_sound(interleaved_samples: &[f32], channels: usize) -> Option<usize> {
    interleaved_samples
        .iter()
        .position(|&sample| sample != 0.0)
        .map(|sample_index| sample_index / channels)
}

/// In-place radix-2 FFT; both slices share one power-of-two length.
fn fft_in_place(real: &mut [f64], imaginary: &mut [f64]) {
    let length = real.len();
    let mut reversed = 0usize;
    for index in 1..length {
        let mut bit = length >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed |= bit;
        if index < reversed {
            real.swap(index, reversed);
            imaginary.swap(index, reversed);
        }
    }
    let mut span = 2;
    while span <= length {
        let half_span = span / 2;
        for start in (0..length).step_by(span) {
            for offset in 0..half_span {
                let (twiddle_imaginary, twiddle_real) =
                    (-2.0 * PI * offset as f64 / span as f64).sin_cos();
                let (upper, lower) = (start + offset, start + offset + half_span);
                let product_real =
                    real[lower] * twiddle_real - imaginary[lower] * twiddle_imaginary;
                let product_imaginary =
                    real[lower] * twiddle_imaginary + imaginary[lower] * twiddle_real;
                real[lower] = real[upper] - product_real;
                imaginary[lower] = imaginary[upper] - product_imaginary;
                real[upper] += product_real;
                imaginary[upper] += product_imaginary;
            }
        }
        span *= 2;
    }
}

/// The strongest frequency in `samples`: the peak of a Hann-windowed FFT over
/// the longest power-of-two prefix, placed between bins by a parabola through
/// the log magnitudes around it.
fn dominant_frequency_hz(samples: &[f32], sample_rate: u32) -> f64 {
    let length = 1usize << samples.len().max(8).ilog2();
    assert!(samples.len() >= length, "at least eight samples to analyse");
    let mut real: Vec<f64> = samples[..length]
        .iter()
        .enumerate()
        .map(|(index, &sample)| {
            let hann = 0.5 - 0.5 * (2.0 * PI * index as f64 / length as f64).cos();
            f64::from(sample) * hann
        })
        .collect();
    let mut imaginary = vec![0.0; length];
    fft_in_place(&mut real, &mut imaginary);
    let log_magnitude_at = |bin: usize| {
        (real[bin].hypot(imaginary[bin]))
            .max(f64::MIN_POSITIVE)
            .ln()
    };
    let peak_bin = (1..length / 2 - 1)
        .max_by(|&left, &right| log_magnitude_at(left).total_cmp(&log_magnitude_at(right)))
        .expect("a spectrum with bins between DC and Nyquist");
    let (below, at, above) = (
        log_magnitude_at(peak_bin - 1),
        log_magnitude_at(peak_bin),
        log_magnitude_at(peak_bin + 1),
    );
    let curvature = below - 2.0 * at + above;
    let offset_in_bins = if curvature < 0.0 {
        0.5 * (below - above) / curvature
    } else {
        0.0
    };
    (peak_bin as f64 + offset_in_bins) * f64::from(sample_rate) / length as f64
}

/// A sinusoid fitted to a span of samples by least squares.
#[derive(Debug, Clone, Copy)]
struct SinusoidFittedToSamples {
    frequency_hz: f64,
    amplitude: f64,
    dc_offset: f64,
    residual_rms: f64,
}

impl SinusoidFittedToSamples {
    /// What the fit leaves unexplained, against the fitted tone's own RMS.
    fn residual_relative_to_the_tone_db(&self) -> f64 {
        20.0 * (self.residual_rms / (self.amplitude / SQRT_2))
            .max(1e-30)
            .log10()
    }
}

/// Solve `matrix · x = right_hand_side` by Gaussian elimination with partial
/// pivoting; `None` when the system is singular.
fn solve_linear_system<const ORDER: usize>(
    mut matrix: [[f64; ORDER]; ORDER],
    mut right_hand_side: [f64; ORDER],
) -> Option<[f64; ORDER]> {
    for column in 0..ORDER {
        let pivot_row = (column..ORDER).max_by(|&left, &right| {
            matrix[left][column]
                .abs()
                .total_cmp(&matrix[right][column].abs())
        })?;
        if matrix[pivot_row][column].abs() < 1e-300 {
            return None;
        }
        matrix.swap(column, pivot_row);
        right_hand_side.swap(column, pivot_row);
        let pivot = matrix[column];
        for row in column + 1..ORDER {
            let factor = matrix[row][column] / pivot[column];
            for (entry, pivot_entry) in matrix[row].iter_mut().zip(pivot).skip(column) {
                *entry -= factor * pivot_entry;
            }
            right_hand_side[row] -= factor * right_hand_side[column];
        }
    }
    let mut solution = [0.0; ORDER];
    for row in (0..ORDER).rev() {
        let known: f64 = (row + 1..ORDER)
            .map(|column| matrix[row][column] * solution[column])
            .sum();
        solution[row] = (right_hand_side[row] - known) / matrix[row][row];
    }
    Some(solution)
}

/// The least-squares fit of `samples` against `columns`, evaluated per sample
/// at seconds from the span's centre.
fn least_squares_fit<const ORDER: usize>(
    samples: &[f32],
    sample_rate: u32,
    columns: impl Fn(f64) -> [f64; ORDER],
) -> Option<[f64; ORDER]> {
    let centre = (samples.len() as f64 - 1.0) / 2.0;
    let mut normal_matrix = [[0.0; ORDER]; ORDER];
    let mut projected_samples = [0.0; ORDER];
    for (index, &sample) in samples.iter().enumerate() {
        let row = columns((index as f64 - centre) / f64::from(sample_rate));
        for (left, &left_value) in row.iter().enumerate() {
            projected_samples[left] += left_value * f64::from(sample);
            for (right, &right_value) in row.iter().enumerate() {
                normal_matrix[left][right] += left_value * right_value;
            }
        }
    }
    solve_linear_system(normal_matrix, projected_samples)
}

/// IEEE 1057's four-parameter sine fit — amplitude, phase, offset and
/// frequency — from `initial_frequency_hz`, which has to be within about one
/// cycle per span of the true frequency. `None` when no sinusoid can be
/// pinned down, as in silence.
fn fit_a_sinusoid(
    samples: &[f32],
    sample_rate: u32,
    initial_frequency_hz: f64,
) -> Option<SinusoidFittedToSamples> {
    const MOST_FREQUENCY_REFINEMENTS: usize = 32;
    let quadrature_terms_at = |angular_frequency: f64| {
        least_squares_fit(samples, sample_rate, |seconds| {
            let (sine, cosine) = (angular_frequency * seconds).sin_cos();
            [cosine, sine, 1.0]
        })
    };
    let mut angular_frequency = 2.0 * PI * initial_frequency_hz;
    let [mut cosine_weight, mut sine_weight, _] = quadrature_terms_at(angular_frequency)?;
    for _ in 0..MOST_FREQUENCY_REFINEMENTS {
        let [
            next_cosine_weight,
            next_sine_weight,
            _,
            angular_frequency_step,
        ] = least_squares_fit(samples, sample_rate, |seconds| {
            let (sine, cosine) = (angular_frequency * seconds).sin_cos();
            [
                cosine,
                sine,
                1.0,
                seconds * (sine_weight * cosine - cosine_weight * sine),
            ]
        })?;
        cosine_weight = next_cosine_weight;
        sine_weight = next_sine_weight;
        angular_frequency += angular_frequency_step;
        if angular_frequency_step.abs() <= 1e-12 * angular_frequency {
            break;
        }
    }
    let [cosine_weight, sine_weight, dc_offset] = quadrature_terms_at(angular_frequency)?;
    let centre = (samples.len() as f64 - 1.0) / 2.0;
    let residual_energy: f64 = samples
        .iter()
        .enumerate()
        .map(|(index, &sample)| {
            let seconds = (index as f64 - centre) / f64::from(sample_rate);
            let (sine, cosine) = (angular_frequency * seconds).sin_cos();
            (f64::from(sample) - (cosine_weight * cosine + sine_weight * sine + dc_offset)).powi(2)
        })
        .sum();
    Some(SinusoidFittedToSamples {
        frequency_hz: angular_frequency / (2.0 * PI),
        amplitude: cosine_weight.hypot(sine_weight),
        dc_offset,
        residual_rms: (residual_energy / samples.len() as f64).sqrt(),
    })
}

fn duration_in_frames(duration: Duration, sample_rate: u32) -> usize {
    (duration.as_secs_f64() * f64::from(sample_rate)).round() as usize
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "audio tier — silent: plays a tone into a muted private process tap of this process and captures it back by the tap's aggregate UID. Needs a default output device, and microphone access and System Audio Recording allowed for the terminal running it. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_tone_played_to_the_default_output_comes_back_intact_through_a_muted_process_tap() {
    let Some(backend) = coreaudio_arm() else {
        return;
    };

    let mut playback_stream_started_before_the_tap = backend
        .open_playback_stream(&AudioDeviceStreamRequest {
            device_id: None,
            deviceless_pacing_clock: an_unused_deviceless_pacing_clock(),
        })
        .expect("the default output opens");
    let playback_format = playback_stream_started_before_the_tap.stream_format();
    assert_eq!(playback_format.sample_format, AudioSampleFormat::F32);
    println!("playback format: {playback_format:?}");

    let tone_is_armed = Arc::new(AtomicBool::new(false));
    let playback_progress = Arc::new(Mutex::new(TonePlaybackProgress {
        tone: PhaseContinuousSineTone::new(
            TONE_FREQUENCY_HZ,
            TONE_AMPLITUDE,
            playback_format.sample_rate,
        ),
        tone_frames_written: 0,
        first_tone_frame_written_at_ns: None,
    }));
    let tone_is_armed_for_hand_off = Arc::clone(&tone_is_armed);
    let playback_progress_for_hand_off = Arc::clone(&playback_progress);
    let playback_channels = playback_format.channels as usize;
    // Silence plays before the tap exists, so the tap is made over a process
    // that is already an output client, and an aggregate that auto-starts on
    // its tap — which waits in its own start for the tapped process to play —
    // has output to start on.
    playback_stream_started_before_the_tap
        .start_requesting_from(Box::new(
            move |requested: AudioBlockRequestedByDevice<'_>| {
                if !tone_is_armed_for_hand_off.load(Ordering::Acquire) {
                    requested.interleaved_sample_bytes_to_fill.fill(0);
                    return;
                }
                let mut progress = playback_progress_for_hand_off.lock().expect("unpoisoned");
                if progress.first_tone_frame_written_at_ns.is_none() {
                    progress.first_tone_frame_written_at_ns =
                        Some(MediaClock::now().as_nanos() as i64);
                }
                progress.tone.fill_interleaved_f32(
                    requested.interleaved_sample_bytes_to_fill,
                    playback_channels,
                );
                progress.tone_frames_written += u64::from(requested.sample_count);
            },
        ))
        .expect("playback starts");

    let muted_process_tap = MutedProcessTapOfThisProcessBehindAPrivateAggregateDevice::create();
    println!(
        "tap aggregate: '{}', tap format {:?}",
        muted_process_tap.aggregate_device_uid(),
        muted_process_tap.tap_stream_format()
    );
    // Rebound after the tap so it is dropped before it on every path,
    // unwinding included: no playback outlives the tap, and with it this
    // process's mute.
    let mut playback_stream = playback_stream_started_before_the_tap;

    let mut capture_stream = backend
        .open_capture_stream(&AudioDeviceStreamRequest {
            device_id: Some(muted_process_tap.aggregate_device_uid().to_owned()),
            deviceless_pacing_clock: an_unused_deviceless_pacing_clock(),
        })
        .expect("the tap's private aggregate opens as a named capture device");
    let capture_format = capture_stream.stream_format();
    assert_eq!(capture_format.sample_format, AudioSampleFormat::F32);
    println!("capture format:  {capture_format:?}");

    let (captured_block_sender, captured_block_receiver) = mpsc::channel();
    capture_stream
        .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
            let _ = captured_block_sender.send(CapturedTapBlock {
                first_sample_timestamp_ns: block.first_sample_timestamp_ns,
                interleaved_samples: f32_samples_of(block.interleaved_sample_bytes),
            });
        }))
        .expect("capture from the tap's aggregate starts");

    let channels = capture_format.channels as usize;
    let mut captured_blocks = vec![
        captured_block_receiver
            .recv_timeout(CAPTURE_DEADLINE)
            .expect("the tap's aggregate delivers blocks while this process plays"),
    ];
    tone_is_armed.store(true, Ordering::Release);
    let frames_wanted =
        duration_in_frames(CAPTURE_AFTER_THE_TONE_IS_ARMED, capture_format.sample_rate);
    let mut frames_captured_since_arming = 0;
    while frames_captured_since_arming < frames_wanted {
        let block = captured_block_receiver
            .recv_timeout(CAPTURE_DEADLINE)
            .expect("the tap's aggregate keeps delivering blocks");
        frames_captured_since_arming += block.interleaved_samples.len() / channels;
        captured_blocks.push(block);
    }
    tone_is_armed.store(false, Ordering::Release);
    playback_stream.stop_requesting().expect("playback stops");
    capture_stream.stop_delivering().expect("capture stops");
    drop(capture_stream);
    drop(playback_stream);
    drop(muted_process_tap);

    let (tone_frames_written, first_tone_frame_written_at_ns) = {
        let progress = playback_progress.lock().expect("unpoisoned");
        (
            progress.tone_frames_written,
            progress.first_tone_frame_written_at_ns,
        )
    };
    assert!(
        tone_frames_written > 0,
        "the default output never asked for a single frame of the tone, so there was nothing \
         for the tap to hear"
    );

    let mut block_first_frames = Vec::with_capacity(captured_blocks.len());
    let mut interleaved_samples = Vec::new();
    for block in &captured_blocks {
        block_first_frames.push(interleaved_samples.len() / channels);
        interleaved_samples.extend_from_slice(&block.interleaved_samples);
    }
    let captured_frames = interleaved_samples.len() / channels;
    let largest_cadence_error_ns = captured_blocks
        .windows(2)
        .zip(block_first_frames.windows(2))
        .map(|(blocks, first_frames)| {
            let expected_gap_ns = (first_frames[1] - first_frames[0]) as i64 * 1_000_000_000
                / i64::from(capture_format.sample_rate);
            (blocks[1].first_sample_timestamp_ns
                - blocks[0].first_sample_timestamp_ns
                - expected_gap_ns)
                .abs()
        })
        .max()
        .unwrap_or(0);
    println!(
        "captured {captured_frames} frames in {} blocks; largest stamp gap error {:.1} µs",
        captured_blocks.len(),
        largest_cadence_error_ns as f64 / 1_000.0
    );

    let Some(tone_onset_frame) = first_frame_carrying_sound(&interleaved_samples, channels) else {
        panic!(
            "the tap returned exact digital zeros for all {captured_frames} frames while the \
             default output took {tone_frames_written} frames of a {TONE_AMPLITUDE} tone. macOS \
             feeds a process tap silence, with no error, when System Audio Recording is not \
             allowed: allow {} in {SYSTEM_AUDIO_RECORDING_SETTING}, then run again.",
            the_application_macos_asks_for_this_process()
        );
    };

    let onset_block =
        block_first_frames.partition_point(|&first_frame| first_frame <= tone_onset_frame) - 1;
    let tone_onset_stamp_ns = captured_blocks[onset_block].first_sample_timestamp_ns
        + (tone_onset_frame - block_first_frames[onset_block]) as i64 * 1_000_000_000
            / i64::from(capture_format.sample_rate);
    if let Some(written_at_ns) = first_tone_frame_written_at_ns {
        println!(
            "tap round trip: {:+.2} ms from the playback hand-off writing the tone's first \
             sample to that sample's capture stamp",
            (tone_onset_stamp_ns - written_at_ns) as f64 / 1_000_000.0
        );
    }

    let steady_start_frame = tone_onset_frame
        + duration_in_frames(SETTLE_AFTER_THE_TONE_ARRIVES, capture_format.sample_rate);
    let steady_frames = duration_in_frames(STEADY_TONE_ANALYSED, capture_format.sample_rate);
    assert!(
        captured_frames >= steady_start_frame + steady_frames,
        "the tap delivered {} frames of steady tone where {steady_frames} were needed",
        captured_frames.saturating_sub(steady_start_frame)
    );

    for channel in 0..channels {
        let steady_tone: Vec<f32> = (steady_start_frame..steady_start_frame + steady_frames)
            .map(|frame| interleaved_samples[frame * channels + channel])
            .collect();
        let dominant_hz = dominant_frequency_hz(&steady_tone, capture_format.sample_rate);
        let fitted = fit_a_sinusoid(&steady_tone, capture_format.sample_rate, dominant_hz)
            .expect("a captured tone this long fits a sinusoid");
        let residual_db = fitted.residual_relative_to_the_tone_db();
        println!(
            "channel {channel}: dominant {dominant_hz:.3} Hz, fitted {:.4} Hz, amplitude {:.5} \
             (gain {:+.3} dB against {TONE_AMPLITUDE}), dc {:+.2e}, sine-fit residual \
             {residual_db:.1} dB",
            fitted.frequency_hz,
            fitted.amplitude,
            20.0 * (fitted.amplitude / f64::from(TONE_AMPLITUDE)).log10(),
            fitted.dc_offset,
        );
        assert!(
            (dominant_hz - TONE_FREQUENCY_HZ).abs() <= MAX_FREQUENCY_ERROR_HZ,
            "channel {channel} of the tap is dominated by {dominant_hz:.3} Hz where \
             {TONE_FREQUENCY_HZ} Hz was played"
        );
        assert!(
            (fitted.amplitude - f64::from(TONE_AMPLITUDE)).abs() <= MAX_AMPLITUDE_ERROR,
            "channel {channel} of the tap carries the tone at {:.4} where {TONE_AMPLITUDE} was \
             played — the tap is not unity gain, or it sits after the device's volume",
            fitted.amplitude
        );
        assert!(
            residual_db <= MAX_SINE_FIT_RESIDUAL_DB,
            "channel {channel} of the tap is {residual_db:.1} dB away from one unbroken \
             sinusoid (the limit is {MAX_SINE_FIT_RESIDUAL_DB} dB) — a block was lost or \
             repeated between the playback hand-off and the capture hand-off"
        );
    }
}

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

/// Mono tone samples generated the way the playback hand-off generates them,
/// one device request at a time.
fn tone_generated_in_device_requests(
    frequency_hz: f64,
    amplitude: f32,
    sample_rate: u32,
    request_frame_counts: impl IntoIterator<Item = usize>,
) -> Vec<f32> {
    let mut tone = PhaseContinuousSineTone::new(frequency_hz, amplitude, sample_rate);
    let mut samples = Vec::new();
    for frame_count in request_frame_counts {
        let mut request = vec![0u8; frame_count * 4 * 2];
        tone.fill_interleaved_f32(&mut request, 2);
        samples.extend(
            f32_samples_of(&request)
                .chunks_exact(2)
                .map(|frame| frame[0]),
        );
    }
    samples
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
    assert_eq!(first_frame_carrying_sound(&silence, 2), None);
}

#[test]
fn the_first_frame_carrying_sound_is_the_frame_not_the_sample() {
    let mut interleaved_samples = vec![0.0f32; 20];
    interleaved_samples[13] = -1e-9;
    assert_eq!(first_frame_carrying_sound(&interleaved_samples, 2), Some(6));
}
