// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The backend chain's last arm: no audio library, no device, silence.
//!
//! `manylinux_2_28` and stock container images carry no audio libraries at
//! all, and the wheel has to import and run there — so a graph authored on a
//! workstation runs unchanged in a container, capturing silence on the timerfd
//! audio clock instead of failing to start. A test needs no audio hardware for
//! the same reason.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use super::audio_device_backend::{
    AudioCaptureSampleFormat, AudioCaptureStream, AudioCaptureStreamFormat,
    AudioCaptureStreamRequest, AudioDeviceBackend, CapturedAudioBlockFromDevice,
    CapturedAudioBlockHandOff,
};
use super::{AudioTickContext, SharedAudioClock};
use crate::core::{Error, Result};

/// A device that captures nothing has nothing to place in a stereo field.
const SILENT_NULL_CAPTURE_CHANNELS: u32 = 1;

/// Silence at the pacing clock's own rate and quantum, on any machine.
pub struct SilentNullAudioDeviceBackend;

impl AudioDeviceBackend for SilentNullAudioDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "silent-null"
    }

    fn open_capture_stream(
        &self,
        request: &AudioCaptureStreamRequest,
    ) -> Result<Box<dyn AudioCaptureStream>> {
        if let Some(device_id) = &request.device_id {
            return Err(Error::Configuration(format!(
                "audio device '{device_id}' cannot be opened: this process found no audio \
                 backend, so audio runs on the silent null backend and that backend has no \
                 devices. Omit device_id to capture silence, or run where an audio server \
                 is reachable."
            )));
        }
        let sample_rate = request.deviceless_pacing_clock.sample_rate();
        if sample_rate == 0 {
            return Err(Error::Configuration(
                "the pacing clock reports a sample rate of 0 Hz, which no block duration \
                 can be derived from"
                    .into(),
            ));
        }
        Ok(Box::new(SilentNullAudioCaptureStream::opened_on(
            Arc::clone(&request.deviceless_pacing_clock),
            sample_rate,
        )))
    }
}

/// What the clock's tick callback and the stream share.
///
/// Held by the callback as a [`Weak`] because an [`AudioClock`] never
/// unregisters one: a dropped stream must leave an inert callback behind
/// rather than a live one delivering into nothing.
///
/// [`AudioClock`]: super::AudioClock
struct SilentNullAudioCaptureStreamPacing {
    /// Where captured blocks go; `None` while the stream is not delivering.
    hand_off: Mutex<Option<CapturedAudioBlockHandOff>>,
    /// Zeroed once and re-handed every tick, so a tick allocates nothing.
    silence: Mutex<Vec<u8>>,
    /// The monotonic instant delivery began, taken from the first tick after
    /// it did. Every block's timestamp derives from this and the samples
    /// delivered before it, so the timeline is exact and gap-free even when
    /// the timer fires a catch-up burst whose ticks all carry one wake time.
    anchor_timestamp_ns: Mutex<Option<i64>>,
    delivered_sample_count: AtomicU64,
    format: AudioCaptureStreamFormat,
}

/// Silence, paced by the clock the request handed the backend.
struct SilentNullAudioCaptureStream {
    pacing_clock: SharedAudioClock,
    pacing: Arc<SilentNullAudioCaptureStreamPacing>,
}

impl SilentNullAudioCaptureStream {
    fn opened_on(pacing_clock: SharedAudioClock, sample_rate: u32) -> Self {
        let format = AudioCaptureStreamFormat {
            sample_rate,
            channels: SILENT_NULL_CAPTURE_CHANNELS,
            sample_format: AudioCaptureSampleFormat::F32,
        };
        let quantum_byte_count = pacing_clock.buffer_size()
            * SILENT_NULL_CAPTURE_CHANNELS as usize
            * format.sample_format.bytes_per_sample();
        let pacing = Arc::new(SilentNullAudioCaptureStreamPacing {
            hand_off: Mutex::new(None),
            silence: Mutex::new(vec![0u8; quantum_byte_count]),
            anchor_timestamp_ns: Mutex::new(None),
            delivered_sample_count: AtomicU64::new(0),
            format,
        });

        let pacing_from_tick = Arc::downgrade(&pacing);
        pacing_clock.on_tick(Box::new(move |tick: AudioTickContext| {
            if let Some(pacing) = pacing_from_tick.upgrade() {
                deliver_one_silent_block(&pacing, tick);
            }
        }));

        Self {
            pacing_clock,
            pacing,
        }
    }
}

impl AudioCaptureStream for SilentNullAudioCaptureStream {
    fn stream_format(&self) -> AudioCaptureStreamFormat {
        self.pacing.format
    }

    fn start_delivering_to(&mut self, hand_off: CapturedAudioBlockHandOff) -> Result<()> {
        *self.pacing.anchor_timestamp_ns.lock() = None;
        self.pacing.delivered_sample_count.store(0, Ordering::SeqCst);
        *self.pacing.hand_off.lock() = Some(hand_off);
        // Idempotent, and the runtime stops it at teardown. Starting it here
        // is what keeps a device-paced graph — and a graph with no audio in
        // it — from ever running the timer.
        self.pacing_clock.start()
    }

    fn stop_delivering(&mut self) -> Result<()> {
        // The clock keeps running for whatever else paces on it; the runtime
        // owns stopping it.
        *self.pacing.hand_off.lock() = None;
        Ok(())
    }
}

fn deliver_one_silent_block(pacing: &SilentNullAudioCaptureStreamPacing, tick: AudioTickContext) {
    let hand_off = pacing.hand_off.lock();
    let Some(hand_off) = hand_off.as_ref() else {
        return;
    };

    let sample_count = tick.samples_needed as u32;
    let anchor_timestamp_ns = *pacing
        .anchor_timestamp_ns
        .lock()
        .get_or_insert(tick.timestamp_ns);
    let delivered_before = pacing
        .delivered_sample_count
        .fetch_add(u64::from(sample_count), Ordering::SeqCst);

    let mut silence = pacing.silence.lock();
    let quantum_byte_count = sample_count as usize
        * pacing.format.channels as usize
        * pacing.format.sample_format.bytes_per_sample();
    if silence.len() < quantum_byte_count {
        silence.resize(quantum_byte_count, 0);
    }

    hand_off(CapturedAudioBlockFromDevice {
        interleaved_sample_bytes: &silence[..quantum_byte_count],
        sample_count,
        first_sample_timestamp_ns: anchor_timestamp_ns
            + nanoseconds_occupied_by(delivered_before, pacing.format.sample_rate),
    });
}

/// Nanoseconds `sample_count` per-channel samples occupy at `sample_rate`,
/// computed wide so a long run accumulates no truncation drift.
fn nanoseconds_occupied_by(sample_count: u64, sample_rate: u32) -> i64 {
    (i128::from(sample_count) * 1_000_000_000 / i128::from(sample_rate)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context::{AudioClock, AudioClockConfig, AudioTickCallback};
    use std::sync::atomic::AtomicBool;

    const TEST_SAMPLE_RATE: u32 = 48_000;
    const TEST_QUANTUM_SAMPLES: usize = 512;
    /// 512 samples at 48 kHz, truncated the way the derivation truncates.
    const TEST_QUANTUM_NANOS: i64 = 10_666_666;

    /// An [`AudioClock`] whose ticks a test fires by hand, so a cadence
    /// assertion is about the block timeline rather than about how promptly a
    /// timer thread happened to wake.
    struct HandFiredTestAudioClock {
        config: AudioClockConfig,
        callbacks: Mutex<Vec<AudioTickCallback>>,
        running: AtomicBool,
        next_tick_number: AtomicU64,
    }

    impl HandFiredTestAudioClock {
        fn new(config: AudioClockConfig) -> Self {
            Self {
                config,
                callbacks: Mutex::new(Vec::new()),
                running: AtomicBool::new(false),
                next_tick_number: AtomicU64::new(0),
            }
        }

        fn fire_one_tick_stamped(&self, timestamp_ns: i64) {
            let tick = AudioTickContext {
                timestamp_ns,
                samples_needed: self.config.buffer_size,
                sample_rate: self.config.sample_rate,
                tick_number: self.next_tick_number.fetch_add(1, Ordering::SeqCst),
            };
            for callback in self.callbacks.lock().iter() {
                callback(tick);
            }
        }
    }

    impl AudioClock for HandFiredTestAudioClock {
        fn on_tick(&self, callback: AudioTickCallback) {
            self.callbacks.lock().push(callback);
        }
        fn sample_rate(&self) -> u32 {
            self.config.sample_rate
        }
        fn buffer_size(&self) -> usize {
            self.config.buffer_size
        }
        fn start(&self) -> Result<()> {
            self.running.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn stop(&self) -> Result<()> {
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn is_running(&self) -> bool {
            self.running.load(Ordering::SeqCst)
        }
    }

    /// One block as the stream handed it over, copied so an assertion can
    /// outlive the borrow the hand-off gets.
    #[derive(Debug, Clone, PartialEq)]
    struct DeliveredBlockRecord {
        interleaved_sample_bytes: Vec<u8>,
        sample_count: u32,
        first_sample_timestamp_ns: i64,
    }

    type DeliveredBlockLog = Arc<Mutex<Vec<DeliveredBlockRecord>>>;

    fn recording_hand_off() -> (DeliveredBlockLog, CapturedAudioBlockHandOff) {
        let delivered: DeliveredBlockLog = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&delivered);
        let hand_off: CapturedAudioBlockHandOff =
            Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
                recorder.lock().push(DeliveredBlockRecord {
                    interleaved_sample_bytes: block.interleaved_sample_bytes.to_vec(),
                    sample_count: block.sample_count,
                    first_sample_timestamp_ns: block.first_sample_timestamp_ns,
                });
            });
        (delivered, hand_off)
    }

    fn test_clock() -> Arc<HandFiredTestAudioClock> {
        Arc::new(HandFiredTestAudioClock::new(AudioClockConfig::new(
            TEST_SAMPLE_RATE,
            TEST_QUANTUM_SAMPLES,
        )))
    }

    fn open_capture_stream_on(clock: &Arc<HandFiredTestAudioClock>) -> Box<dyn AudioCaptureStream> {
        SilentNullAudioDeviceBackend
            .open_capture_stream(&AudioCaptureStreamRequest {
                device_id: None,
                deviceless_pacing_clock: Arc::clone(clock) as SharedAudioClock,
            })
            .expect("the null backend opens its default device on any machine")
    }

    /// A machine with no audio is a supported environment; a wrong device id
    /// is a wiring error, and landing on a different device would be worse
    /// than failing.
    #[test]
    fn a_named_device_is_refused_by_name_rather_than_opened_as_something_else() {
        let clock = test_clock();
        let Err(refusal) = SilentNullAudioDeviceBackend.open_capture_stream(
            &AudioCaptureStreamRequest {
                device_id: Some("alsa_input.pci-0000_00_1f.3".to_string()),
                deviceless_pacing_clock: clock as SharedAudioClock,
            },
        ) else {
            panic!("a named device cannot exist on a backend with no devices");
        };
        assert!(
            refusal.to_string().contains("alsa_input.pci-0000_00_1f.3"),
            "the refusal must name the device that was asked for: {refusal}"
        );
    }

    #[test]
    fn a_pacing_clock_with_no_sample_rate_is_refused_at_open() {
        let clock = Arc::new(HandFiredTestAudioClock::new(AudioClockConfig::new(
            0,
            TEST_QUANTUM_SAMPLES,
        )));
        assert!(
            SilentNullAudioDeviceBackend
                .open_capture_stream(&AudioCaptureStreamRequest {
                    device_id: None,
                    deviceless_pacing_clock: clock as SharedAudioClock,
                })
                .is_err(),
            "no block duration derives from a rate of zero"
        );
    }

    #[test]
    fn the_stream_takes_its_rate_and_quantum_from_the_clock_that_paces_it() {
        let clock = test_clock();
        let stream = open_capture_stream_on(&clock);
        assert_eq!(
            stream.stream_format(),
            AudioCaptureStreamFormat {
                sample_rate: TEST_SAMPLE_RATE,
                channels: SILENT_NULL_CAPTURE_CHANNELS,
                sample_format: AudioCaptureSampleFormat::F32,
            }
        );
    }

    /// The narrowed clock role, at the seam: nothing starts the timer until
    /// something actually paces on it.
    #[test]
    fn opening_a_stream_leaves_the_clock_stopped_and_delivering_starts_it() {
        let clock = test_clock();
        let mut stream = open_capture_stream_on(&clock);
        assert!(
            !clock.is_running(),
            "opening a stream must not start the clock"
        );

        let (_delivered, hand_off) = recording_hand_off();
        stream
            .start_delivering_to(hand_off)
            .expect("delivery starts the clock");
        assert!(clock.is_running());
    }

    #[test]
    fn every_block_carries_a_full_quantum_of_silence() {
        let clock = test_clock();
        let mut stream = open_capture_stream_on(&clock);
        let (delivered, hand_off) = recording_hand_off();
        stream.start_delivering_to(hand_off).expect("start");

        clock.fire_one_tick_stamped(1_000_000_000);

        let delivered = delivered.lock();
        let [block] = delivered.as_slice() else {
            panic!("one tick delivers exactly one block, got {delivered:?}");
        };
        assert_eq!(block.sample_count, TEST_QUANTUM_SAMPLES as u32);
        assert_eq!(
            block.interleaved_sample_bytes.len(),
            TEST_QUANTUM_SAMPLES * SILENT_NULL_CAPTURE_CHANNELS as usize * 4,
            "sample_count × channels × 4 bytes per f32 scalar"
        );
        assert!(
            block.interleaved_sample_bytes.iter().all(|byte| *byte == 0),
            "the null backend captures silence, and silence is zeroed scalars"
        );
    }

    /// The timerfd reads one wake time for a whole catch-up burst, so ticks
    /// can share a timestamp. Deriving each block's stamp from the samples
    /// before it is what keeps N blocks from claiming one instant — and what
    /// makes `sample_count / sample_rate` the gap between blocks by
    /// construction rather than by luck.
    #[test]
    fn block_timestamps_advance_by_one_block_even_when_ticks_share_a_wake_time() {
        let clock = test_clock();
        let mut stream = open_capture_stream_on(&clock);
        let (delivered, hand_off) = recording_hand_off();
        stream.start_delivering_to(hand_off).expect("start");

        let one_wake_time_ns = 4_000_000_000;
        for _ in 0..3 {
            clock.fire_one_tick_stamped(one_wake_time_ns);
        }

        let stamps: Vec<i64> = delivered
            .lock()
            .iter()
            .map(|block| block.first_sample_timestamp_ns)
            .collect();
        assert_eq!(
            stamps,
            vec![
                one_wake_time_ns,
                one_wake_time_ns + TEST_QUANTUM_NANOS,
                one_wake_time_ns + 2 * TEST_QUANTUM_SAMPLES as i64 * 1_000_000_000
                    / TEST_SAMPLE_RATE as i64,
            ]
        );
    }

    /// Truncating each step and summing would drift; deriving each stamp from
    /// the total samples before it does not. At 48 kHz the per-block
    /// truncation is 2/3 ns, which only a long run makes visible.
    #[test]
    fn a_long_run_of_blocks_accumulates_no_truncation_drift() {
        let clock = test_clock();
        let mut stream = open_capture_stream_on(&clock);
        let (delivered, hand_off) = recording_hand_off();
        stream.start_delivering_to(hand_off).expect("start");

        let anchor_ns = 0;
        let block_count = 10_000;
        for _ in 0..block_count {
            clock.fire_one_tick_stamped(anchor_ns);
        }

        let delivered = delivered.lock();
        let last = delivered.last().expect("blocks were delivered");
        let samples_before_last = (block_count - 1) * TEST_QUANTUM_SAMPLES as i64;
        assert_eq!(
            last.first_sample_timestamp_ns,
            samples_before_last * 1_000_000_000 / TEST_SAMPLE_RATE as i64,
            "summing a truncated per-block step would be ~6.6 µs early here"
        );
    }

    #[test]
    fn a_stopped_stream_delivers_nothing_more() {
        let clock = test_clock();
        let mut stream = open_capture_stream_on(&clock);
        let (delivered, hand_off) = recording_hand_off();
        stream.start_delivering_to(hand_off).expect("start");

        clock.fire_one_tick_stamped(1_000_000_000);
        stream.stop_delivering().expect("stop");
        clock.fire_one_tick_stamped(2_000_000_000);

        assert_eq!(
            delivered.lock().len(),
            1,
            "the hand-off must not be called again once stop_delivering returned"
        );
    }

    /// An `AudioClock` never unregisters a callback, so the one a dropped
    /// stream left behind has to go inert on its own.
    #[test]
    fn a_dropped_stream_leaves_an_inert_callback_behind() {
        let clock = test_clock();
        let (delivered, hand_off) = recording_hand_off();
        {
            let mut stream = open_capture_stream_on(&clock);
            stream.start_delivering_to(hand_off).expect("start");
            clock.fire_one_tick_stamped(1_000_000_000);
        }

        clock.fire_one_tick_stamped(2_000_000_000);

        assert_eq!(delivered.lock().len(), 1);
    }
}
