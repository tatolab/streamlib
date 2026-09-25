// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The CoreAudio arm's samples, proven digitally: a tone the arm plays to the
//! default output comes back, sample for sample, through a muted process tap
//! of this process that the arm captures by device UID.
//!
//! The acoustic test hears a tone through the air; this one checks the
//! samples themselves — frequency, level, and one unbroken sinusoid from the
//! moment the tone settles to the end of the capture, which a block lost or
//! repeated in either direction breaks.
//!
//! Meant to make no sound. The tone plays only once a pilot 120 dB below full
//! scale has come back through the tap, so a tap macOS does not let read —
//! System Audio Recording not allowed — fails on the pilot with nothing
//! audible played, and a tap that reads mutes this process's output.
//!
//! Audio tier — needs a Mac with a default output device, with microphone
//! access and System Audio Recording allowed for the terminal running it.
//! The first run raises the System Audio Recording prompt.

#![cfg(target_os = "macos")]

use std::cell::Cell;
use std::f64::consts::{PI, SQRT_2};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use streamlib_engine::apple_coreaudio_audio_tier::{
    CoreAudioStreamDirection, responsible_gui_application_name,
};
use streamlib_engine::core::context::{
    AudioBlockRequestedByDevice, AudioCaptureStream, AudioClockConfig, AudioDeviceStreamRequest,
    AudioSampleFormat, CapturedAudioBlockFromDevice, SharedAudioClock, SoftwareAudioClock,
};
use streamlib_engine::core::media_clock::MediaClock;

#[path = "support/coreaudio_audio_tier.rs"]
mod coreaudio_audio_tier;
use coreaudio_audio_tier::{
    print_for_the_evidence_record, the_coreaudio_arm_with_a_default_device_for,
    the_microphone_must_be_allowed,
};

mod coreaudio_muted_process_tap_of_this_process;
use coreaudio_muted_process_tap_of_this_process::MutedProcessTapOfThisProcessBehindAPrivateAggregateDevice;

const TONE_FREQUENCY_HZ: f64 = 440.0;

/// The reference amplitude `known_audio_signal.py` plays.
const TONE_AMPLITUDE: f32 = 0.5;

/// About −120 dBFS: below hearing on any output at any volume, and far above
/// the exact zeros a tap that may not read delivers.
const SUB_AUDIBLE_PILOT_AMPLITUDE: f32 = 1e-6;

/// Captured audio within which the pilot has to come back. The tap's round
/// trip is tens of milliseconds.
const PILOT_MUST_COME_BACK_WITHIN: Duration = Duration::from_secs(1);

/// Far above the pilot, and below the tone's first sample, which is played
/// from phase zero.
const TONE_ONSET_LEVEL: f32 = 1e-3;

/// Two seconds of tone, plus room for the tap's round trip.
const CAPTURE_AFTER_THE_TONE_IS_ARMED: Duration = Duration::from_millis(2500);

/// Skipped after the tone's first captured sample, so only its steady state
/// is analysed.
const SETTLE_AFTER_THE_TONE_ARRIVES: Duration = Duration::from_millis(100);

/// The least steady tone the analysis accepts. It analyses every captured
/// frame from the settled tone to the end of the capture.
const LEAST_STEADY_TONE_ANALYSED: Duration = Duration::from_millis(1500);

const MAX_FREQUENCY_ERROR_HZ: f64 = 1.0;

/// `known_audio_signal.py`'s loopback bound, about ±0.9 dB. The tap should
/// return the tone at unity: a stereo mixdown of a stream that carries the
/// same sample on every channel is that sample, the tap reads this process's
/// output before the device applies its volume, and drift compensation's
/// resampler is unity gain at 440 Hz.
const MAX_AMPLITUDE_ERROR: f64 = 0.05;

/// A clean digital path fits one sinusoid to within float rounding, far below
/// this. A block lost or repeated jumps the phase of everything after it: a
/// 512-frame loss mid-span leaves about −6 dB, and one a single block before
/// the end of 2.4 s of capture still about −19 dB.
const MAX_SINE_FIT_RESIDUAL_DB: f64 = -40.0;

/// A capture that has produced nothing in this long is a broken device, not a
/// slow one.
const CAPTURE_DEADLINE: Duration = Duration::from_secs(10);

const SYSTEM_AUDIO_RECORDING_SETTING: &str = "System Settings › Privacy & Security › Screen & \
     System Audio Recording › System Audio Recording Only";

fn an_unused_deviceless_pacing_clock() -> SharedAudioClock {
    Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(48_000, 512)))
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

/// What the playback hand-off writes on its next request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalThePlaybackHandOffWrites {
    DigitalSilence,
    SubAudiblePilot,
    Tone,
}

/// What the playback hand-off shares with the test thread.
struct PlaybackHandOffProgress {
    now_writing: SignalThePlaybackHandOffWrites,
    sub_audible_pilot: PhaseContinuousSineTone,
    tone: PhaseContinuousSineTone,
    pilot_frames_written: u64,
    tone_frames_written: u64,
    /// Monotonic nanoseconds at the hand-off call that wrote the tone's first
    /// frame.
    first_tone_frame_written_at_ns: Option<i64>,
}

impl PlaybackHandOffProgress {
    fn writing_digital_silence(sample_rate: u32) -> Self {
        Self {
            now_writing: SignalThePlaybackHandOffWrites::DigitalSilence,
            sub_audible_pilot: PhaseContinuousSineTone::new(
                TONE_FREQUENCY_HZ,
                SUB_AUDIBLE_PILOT_AMPLITUDE,
                sample_rate,
            ),
            tone: PhaseContinuousSineTone::new(TONE_FREQUENCY_HZ, TONE_AMPLITUDE, sample_rate),
            pilot_frames_written: 0,
            tone_frames_written: 0,
            first_tone_frame_written_at_ns: None,
        }
    }

    fn fill_the_devices_request(
        &mut self,
        requested: AudioBlockRequestedByDevice<'_>,
        playback_channels: usize,
    ) {
        match self.now_writing {
            SignalThePlaybackHandOffWrites::DigitalSilence => {
                requested.interleaved_sample_bytes_to_fill.fill(0);
            }
            SignalThePlaybackHandOffWrites::SubAudiblePilot => {
                self.sub_audible_pilot.fill_interleaved_f32(
                    requested.interleaved_sample_bytes_to_fill,
                    playback_channels,
                );
                self.pilot_frames_written += u64::from(requested.sample_count);
            }
            SignalThePlaybackHandOffWrites::Tone => {
                if self.first_tone_frame_written_at_ns.is_none() {
                    self.first_tone_frame_written_at_ns = Some(MediaClock::now().as_nanos() as i64);
                }
                self.tone.fill_interleaved_f32(
                    requested.interleaved_sample_bytes_to_fill,
                    playback_channels,
                );
                self.tone_frames_written += u64::from(requested.sample_count);
            }
        }
    }
}

/// One block as the tap's aggregate delivered it, copied out of the hand-off.
struct CapturedTapBlock {
    first_sample_timestamp_ns: i64,
    interleaved_samples: Vec<f32>,
}

fn the_next_block_from_the_taps_aggregate(
    captured_block_receiver: &mpsc::Receiver<CapturedTapBlock>,
    capture_stream: &dyn AudioCaptureStream,
) -> CapturedTapBlock {
    captured_block_receiver
        .recv_timeout(CAPTURE_DEADLINE)
        .unwrap_or_else(|_| {
            panic!(
                "the tap's private aggregate delivered nothing for {CAPTURE_DEADLINE:?} while \
                 this process played. Liveness: {:?}",
                capture_stream
                    .liveness_report()
                    .failure_that_ended_the_stream()
            )
        })
}

fn f32_samples_of(interleaved_little_endian_bytes: &[u8]) -> Vec<f32> {
    interleaved_little_endian_bytes
        .chunks_exact(4)
        .map(|scalar| f32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]]))
        .collect()
}

/// The first frame carrying a sample whose magnitude exceeds `level`.
fn first_frame_louder_than(
    interleaved_samples: &[f32],
    channels: usize,
    level: f32,
) -> Option<usize> {
    interleaved_samples
        .iter()
        .position(|&sample| sample.abs() > level)
        .map(|sample_index| sample_index / channels)
}

/// How the wait for the sub-audible pilot to come back through the tap ended.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SubAudiblePilotThroughTheTap {
    /// A captured block carried a non-zero sample, and the tone was armed
    /// after it.
    CameBack {
        captured_frames_after_arming_the_pilot: usize,
        peak_in_its_first_block: f32,
    },
    /// Every captured block was exact zeros until the budget ran out, and the
    /// tone was never armed.
    NeverCameBack {
        captured_frames_after_arming_the_pilot: usize,
    },
}

/// Arm the sub-audible pilot, then pull blocks from the tap until one carries
/// a non-zero sample — arming the tone only then — or until
/// `most_frames_before_the_pilot_is_back` frames of exact zeros have come
/// back. Every block pulled is appended to `captured_blocks`.
fn arm_the_tone_only_once_the_pilot_comes_back_through_the_tap(
    mut next_block_from_the_tap: impl FnMut() -> CapturedTapBlock,
    mut playback_hand_off_writes_next: impl FnMut(SignalThePlaybackHandOffWrites),
    channels: usize,
    most_frames_before_the_pilot_is_back: usize,
    captured_blocks: &mut Vec<CapturedTapBlock>,
) -> SubAudiblePilotThroughTheTap {
    playback_hand_off_writes_next(SignalThePlaybackHandOffWrites::SubAudiblePilot);
    let mut captured_frames_after_arming_the_pilot = 0;
    while captured_frames_after_arming_the_pilot < most_frames_before_the_pilot_is_back {
        let block = next_block_from_the_tap();
        if let Some(pilot_frame_in_block) =
            first_frame_louder_than(&block.interleaved_samples, channels, 0.0)
        {
            let peak_in_its_first_block = block
                .interleaved_samples
                .iter()
                .fold(0.0f32, |peak, &sample| peak.max(sample.abs()));
            captured_blocks.push(block);
            playback_hand_off_writes_next(SignalThePlaybackHandOffWrites::Tone);
            return SubAudiblePilotThroughTheTap::CameBack {
                captured_frames_after_arming_the_pilot: captured_frames_after_arming_the_pilot
                    + pilot_frame_in_block,
                peak_in_its_first_block,
            };
        }
        captured_frames_after_arming_the_pilot += block.interleaved_samples.len() / channels;
        captured_blocks.push(block);
    }
    SubAudiblePilotThroughTheTap::NeverCameBack {
        captured_frames_after_arming_the_pilot,
    }
}

/// Why no tone was played once the pilot never came back, told apart by
/// whether the playback hand-off was asked for any of the pilot.
fn why_the_pilot_never_came_back_through_the_tap(
    captured_frames_after_arming_the_pilot: usize,
    pilot_frames_the_playback_hand_off_wrote: u64,
    responsible_application_name: &str,
) -> String {
    if pilot_frames_the_playback_hand_off_wrote == 0 {
        return format!(
            "the default output asked the arm's playback for no frames of the \
             {SUB_AUDIBLE_PILOT_AMPLITUDE:e} pilot while the tap delivered \
             {captured_frames_after_arming_the_pilot} frames, so this process played nothing for \
             the tap to hear and no tone was played. The arm's playback path is not being asked \
             for frames while this process is tapped — a playback fault, not a missing grant."
        );
    }
    format!(
        "the tap returned exact digital zeros for {captured_frames_after_arming_the_pilot} frames \
         while the arm's playback hand-off wrote {pilot_frames_the_playback_hand_off_wrote} \
         frames of a {SUB_AUDIBLE_PILOT_AMPLITUDE:e} pilot, so no tone was played. macOS feeds a \
         process tap silence, with no error, when System Audio Recording is not allowed: allow \
         {responsible_application_name} in {SYSTEM_AUDIO_RECORDING_SETTING}, then run again. If \
         it is allowed there already, the arm's render path turned the pilot its hand-off wrote \
         into zeros."
    )
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
    ignore = "audio tier — plays a tone only into a muted private process tap of this process, once an inaudible pilot has come back through it, and captures it by the tap's aggregate UID. Needs a default output device, and microphone access and System Audio Recording allowed for the terminal running it. Run with --features streamlib/hardware-tests. See docs/testing-hardware.md"
)]
fn a_tone_played_to_the_default_output_comes_back_intact_through_a_muted_process_tap() {
    let Some(backend) =
        the_coreaudio_arm_with_a_default_device_for(CoreAudioStreamDirection::Playback)
    else {
        return;
    };
    the_microphone_must_be_allowed();

    let mut playback_stream_started_before_the_tap = backend
        .open_playback_stream(&AudioDeviceStreamRequest {
            device_id: None,
            deviceless_pacing_clock: an_unused_deviceless_pacing_clock(),
        })
        .expect("the default output opens");
    let playback_format = playback_stream_started_before_the_tap.stream_format();
    assert_eq!(playback_format.sample_format, AudioSampleFormat::F32);
    print_for_the_evidence_record(format!("playback format: {playback_format:?}"));

    let playback_progress = Arc::new(Mutex::new(
        PlaybackHandOffProgress::writing_digital_silence(playback_format.sample_rate),
    ));
    let playback_progress_for_hand_off = Arc::clone(&playback_progress);
    let playback_channels = playback_format.channels as usize;
    let write_next = |signal: SignalThePlaybackHandOffWrites| {
        playback_progress.lock().expect("unpoisoned").now_writing = signal;
    };
    // Silence plays before the tap exists, so the tap is made over a process
    // that is already an output client, and an aggregate that auto-starts on
    // its tap — which waits in its own start for the tapped process to play —
    // has output to start on.
    playback_stream_started_before_the_tap
        .start_requesting_from(Box::new(
            move |requested: AudioBlockRequestedByDevice<'_>| {
                playback_progress_for_hand_off
                    .lock()
                    .expect("unpoisoned")
                    .fill_the_devices_request(requested, playback_channels);
            },
        ))
        .expect("playback starts");

    let muted_process_tap = MutedProcessTapOfThisProcessBehindAPrivateAggregateDevice::create();
    print_for_the_evidence_record(format!(
        "tap aggregate: '{}', tap format {:?}",
        muted_process_tap.aggregate_device_uid(),
        muted_process_tap.tap_stream_format()
    ));
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
    print_for_the_evidence_record(format!("capture format:  {capture_format:?}"));

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
    let mut captured_blocks = vec![the_next_block_from_the_taps_aggregate(
        &captured_block_receiver,
        capture_stream.as_ref(),
    )];

    match arm_the_tone_only_once_the_pilot_comes_back_through_the_tap(
        || {
            the_next_block_from_the_taps_aggregate(
                &captured_block_receiver,
                capture_stream.as_ref(),
            )
        },
        write_next,
        channels,
        duration_in_frames(PILOT_MUST_COME_BACK_WITHIN, capture_format.sample_rate),
        &mut captured_blocks,
    ) {
        SubAudiblePilotThroughTheTap::CameBack {
            captured_frames_after_arming_the_pilot,
            peak_in_its_first_block,
        } => print_for_the_evidence_record(format!(
            "the tap reads: the {SUB_AUDIBLE_PILOT_AMPLITUDE:e} pilot came back \
             {captured_frames_after_arming_the_pilot} captured frames after it was armed, \
             peaking at {peak_in_its_first_block:e} in its first block"
        )),
        SubAudiblePilotThroughTheTap::NeverCameBack {
            captured_frames_after_arming_the_pilot,
        } => {
            let pilot_frames_the_playback_hand_off_wrote = playback_progress
                .lock()
                .expect("unpoisoned")
                .pilot_frames_written;
            panic!(
                "{}",
                why_the_pilot_never_came_back_through_the_tap(
                    captured_frames_after_arming_the_pilot,
                    pilot_frames_the_playback_hand_off_wrote,
                    &responsible_gui_application_name().unwrap_or_else(|| {
                        "the terminal or application this test was launched from".to_owned()
                    }),
                )
            );
        }
    }

    let frames_wanted =
        duration_in_frames(CAPTURE_AFTER_THE_TONE_IS_ARMED, capture_format.sample_rate);
    let mut frames_captured_since_arming = 0;
    while frames_captured_since_arming < frames_wanted {
        let block = the_next_block_from_the_taps_aggregate(
            &captured_block_receiver,
            capture_stream.as_ref(),
        );
        frames_captured_since_arming += block.interleaved_samples.len() / channels;
        captured_blocks.push(block);
    }
    write_next(SignalThePlaybackHandOffWrites::DigitalSilence);
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
    print_for_the_evidence_record(format!(
        "captured {captured_frames} frames in {} blocks; largest stamp gap error {:.1} µs",
        captured_blocks.len(),
        largest_cadence_error_ns as f64 / 1_000.0
    ));

    let Some(tone_onset_frame) =
        first_frame_louder_than(&interleaved_samples, channels, TONE_ONSET_LEVEL)
    else {
        panic!(
            "the tap carried the pilot, but nothing louder than {TONE_ONSET_LEVEL} in all \
             {captured_frames} frames while the default output took {tone_frames_written} \
             frames of a {TONE_AMPLITUDE} tone"
        );
    };

    let onset_block =
        block_first_frames.partition_point(|&first_frame| first_frame <= tone_onset_frame) - 1;
    let tone_onset_stamp_ns = captured_blocks[onset_block].first_sample_timestamp_ns
        + (tone_onset_frame - block_first_frames[onset_block]) as i64 * 1_000_000_000
            / i64::from(capture_format.sample_rate);
    if let Some(written_at_ns) = first_tone_frame_written_at_ns {
        print_for_the_evidence_record(format!(
            "tap round trip: {:+.2} ms from the playback hand-off writing the tone's first \
             sample to that sample's capture stamp",
            (tone_onset_stamp_ns - written_at_ns) as f64 / 1_000_000.0
        ));
    }

    let steady_start_frame = tone_onset_frame
        + duration_in_frames(SETTLE_AFTER_THE_TONE_ARRIVES, capture_format.sample_rate);
    let steady_frames = captured_frames.saturating_sub(steady_start_frame);
    let least_steady_frames =
        duration_in_frames(LEAST_STEADY_TONE_ANALYSED, capture_format.sample_rate);
    assert!(
        steady_frames >= least_steady_frames,
        "the tap delivered {steady_frames} frames of steady tone where at least \
         {least_steady_frames} were needed"
    );

    for channel in 0..channels {
        let steady_tone: Vec<f32> = (steady_start_frame..captured_frames)
            .map(|frame| interleaved_samples[frame * channels + channel])
            .collect();
        let dominant_hz = dominant_frequency_hz(&steady_tone, capture_format.sample_rate);
        let fitted = fit_a_sinusoid(&steady_tone, capture_format.sample_rate, dominant_hz)
            .expect("a captured tone this long fits a sinusoid");
        let residual_db = fitted.residual_relative_to_the_tone_db();
        print_for_the_evidence_record(format!(
            "channel {channel} over {:.3} s of steady tone: dominant {dominant_hz:.3} Hz, fitted \
             {:.4} Hz, amplitude {:.5} (gain {:+.3} dB against {TONE_AMPLITUDE}), dc {:+.2e}, \
             sine-fit residual {residual_db:.1} dB",
            steady_frames as f64 / f64::from(capture_format.sample_rate),
            fitted.frequency_hz,
            fitted.amplitude,
            20.0 * (fitted.amplitude / f64::from(TONE_AMPLITUDE)).log10(),
            fitted.dc_offset,
        ));
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
                .chunks_exact(2)
                .map(|frame| frame[0]),
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
