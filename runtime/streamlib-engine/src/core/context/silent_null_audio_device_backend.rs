// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The backend chain's last arm: no audio library, no device, silence.
//!
//! `manylinux_2_28` and stock container images carry no audio libraries at
//! all, and the wheel has to import and run there — so a graph authored on a
//! workstation runs unchanged in a container, capturing silence on the timerfd
//! audio clock instead of failing to start. A test needs no audio hardware for
//! the same reason.

use std::num::NonZeroU32;
use std::sync::Arc;

use parking_lot::Mutex;

use super::audio_device_backend::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioCaptureStream,
    AudioDeviceBackend, AudioDeviceStreamRequest, AudioPlaybackStream, AudioSampleFormat,
    AudioStreamFormat, CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff,
};
use super::{AudioTickContext, SharedAudioClock};
use crate::core::{Error, Result};

/// A device that captures nothing has nothing to place in a stereo field, and
/// playback matches it so that a microphone wired to a speaker runs unchanged
/// here: both ends of a null-backend graph agree on one format, and the
/// refusal a speaker owes a mismatched block never fires on a graph that
/// played on a workstation.
const SILENT_NULL_STREAM_CHANNELS: u32 = 1;

/// Silence at the pacing clock's own rate and quantum, on any machine.
pub struct SilentNullAudioDeviceBackend;

impl AudioDeviceBackend for SilentNullAudioDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "silent-null"
    }

    fn open_capture_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioCaptureStream>> {
        let sample_rate = pacing_rate_a_stream_can_open_on(request)?;
        Ok(Box::new(SilentNullAudioCaptureStream::opened_on(
            Arc::clone(&request.deviceless_pacing_clock),
            sample_rate,
        )))
    }

    fn open_playback_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioPlaybackStream>> {
        let sample_rate = pacing_rate_a_stream_can_open_on(request)?;
        Ok(Box::new(SilentNullAudioPlaybackStream::opened_on(
            Arc::clone(&request.deviceless_pacing_clock),
            sample_rate,
        )))
    }
}

/// The two refusals every null-backend stream owes, in either direction.
///
/// One function rather than a copy per direction: a machine with no audio is a
/// supported environment and a wrong device id is a wiring error, and that
/// distinction must not be able to hold for capture while drifting for
/// playback.
fn pacing_rate_a_stream_can_open_on(request: &AudioDeviceStreamRequest) -> Result<NonZeroU32> {
    if let Some(device_id) = &request.device_id {
        return Err(Error::Configuration(format!(
            "audio device '{device_id}' cannot be opened: this process found no audio \
             backend, so audio runs on the silent null backend and that backend has no \
             devices. Omit device_id to run against silence, or run where an audio server \
             is reachable."
        )));
    }
    NonZeroU32::new(request.deviceless_pacing_clock.sample_rate()).ok_or_else(|| {
        Error::Configuration(
            "the pacing clock reports a sample rate of 0 Hz, which no block duration \
             can be derived from"
                .into(),
        )
    })
}

/// The format both null streams carry: the pacing clock's rate, one channel,
/// and the wire's default scalar encoding.
fn silent_null_stream_format(sample_rate: NonZeroU32) -> AudioStreamFormat {
    AudioStreamFormat {
        sample_rate: sample_rate.get(),
        channels: SILENT_NULL_STREAM_CHANNELS,
        sample_format: AudioSampleFormat::F32,
    }
}

/// Everything a tick reads and writes, under one lock.
///
/// One lock rather than four primitives because beginning delivery resets the
/// anchor and the count while a tick may be mid-flight: split, a tick can read
/// the old anchor and then the reset count, and stamp one block with a
/// timestamp from neither stream.
struct SilentNullAudioCaptureStreamPacing {
    /// Where captured blocks go; `None` while the stream is not delivering.
    hand_off: Option<CapturedAudioBlockHandOff>,
    /// Zeroed once and re-handed every tick, so a tick allocates nothing.
    silence: Vec<u8>,
    /// The monotonic instant delivery began, taken from the first tick after
    /// it did. Every block's timestamp derives from this and the samples
    /// delivered before it, so the timeline is exact and gap-free even when
    /// the timer fires a catch-up burst whose ticks all carry one wake time.
    anchor_timestamp_ns: Option<i64>,
    delivered_sample_count: u64,
}

/// Silence, paced by the clock the request handed the backend.
struct SilentNullAudioCaptureStream {
    pacing_clock: SharedAudioClock,
    capture_stream_format: AudioStreamFormat,
    pacing: Arc<Mutex<SilentNullAudioCaptureStreamPacing>>,
}

impl SilentNullAudioCaptureStream {
    fn opened_on(pacing_clock: SharedAudioClock, sample_rate: NonZeroU32) -> Self {
        let capture_stream_format = silent_null_stream_format(sample_rate);
        let pacing = Arc::new(Mutex::new(SilentNullAudioCaptureStreamPacing {
            hand_off: None,
            silence: vec![
                0u8;
                capture_stream_format
                    .interleaved_byte_count_for(pacing_clock.buffer_size() as u32)
            ],
            anchor_timestamp_ns: None,
            delivered_sample_count: 0,
        }));

        // An `AudioClock` never unregisters a callback, so a dropped stream
        // cannot take its own back. Holding the state weakly is what lets the
        // stream's memory go and leaves an inert callback rather than a live
        // one delivering into nothing — the clock still walks it every tick.
        let pacing_from_tick = Arc::downgrade(&pacing);
        pacing_clock.on_tick(Box::new(move |tick: AudioTickContext| {
            if let Some(pacing) = pacing_from_tick.upgrade() {
                deliver_one_silent_block(&pacing, capture_stream_format, sample_rate, tick);
            }
        }));

        Self {
            pacing_clock,
            capture_stream_format,
            pacing,
        }
    }
}

impl AudioCaptureStream for SilentNullAudioCaptureStream {
    fn stream_format(&self) -> AudioStreamFormat {
        self.capture_stream_format
    }

    fn start_delivering_to(&mut self, hand_off: CapturedAudioBlockHandOff) -> Result<()> {
        {
            let mut pacing = self.pacing.lock();
            pacing.anchor_timestamp_ns = None;
            pacing.delivered_sample_count = 0;
            pacing.hand_off = Some(hand_off);
        }
        // Idempotent, and the runtime stops it at teardown. Starting it here
        // is what keeps a device-paced graph — and a graph with no audio in
        // it — from ever running the timer.
        self.pacing_clock.start()
    }

    fn stop_delivering(&mut self) -> Result<()> {
        // The clock keeps running for whatever else paces on it; the runtime
        // owns stopping it.
        self.pacing.lock().hand_off = None;
        Ok(())
    }
}

fn deliver_one_silent_block(
    pacing: &Mutex<SilentNullAudioCaptureStreamPacing>,
    capture_stream_format: AudioStreamFormat,
    sample_rate: NonZeroU32,
    tick: AudioTickContext,
) {
    let mut pacing = pacing.lock();
    if pacing.hand_off.is_none() {
        return;
    }

    let sample_count = tick.samples_needed as u32;
    let anchor_timestamp_ns = *pacing.anchor_timestamp_ns.get_or_insert(tick.timestamp_ns);
    let samples_delivered_before = pacing.delivered_sample_count;
    pacing.delivered_sample_count += u64::from(sample_count);

    let quantum_byte_count = capture_stream_format.interleaved_byte_count_for(sample_count);
    if pacing.silence.len() < quantum_byte_count {
        pacing.silence.resize(quantum_byte_count, 0);
    }

    let SilentNullAudioCaptureStreamPacing {
        hand_off, silence, ..
    } = &*pacing;
    let hand_off = hand_off
        .as_ref()
        .expect("the hand-off was present at the top of this lock scope");
    hand_off(CapturedAudioBlockFromDevice {
        interleaved_sample_bytes: &silence[..quantum_byte_count],
        sample_count,
        first_sample_timestamp_ns: anchor_timestamp_ns
            + nanoseconds_occupied_by_sample_count_at_rate(samples_delivered_before, sample_rate),
    });
}

/// Nanoseconds `sample_count` per-channel samples occupy at `sample_rate`,
/// computed wide so a long run accumulates no truncation drift.
fn nanoseconds_occupied_by_sample_count_at_rate(sample_count: u64, sample_rate: NonZeroU32) -> i64 {
    let nanoseconds = i128::from(sample_count) * 1_000_000_000 / i128::from(sample_rate.get());
    i64::try_from(nanoseconds).unwrap_or(i64::MAX)
}

/// What a playback tick reads and writes, under one lock.
///
/// One lock rather than two primitives because a tick may be mid-flight while
/// delivery is being stopped: split, a tick can read a hand-off that the stop
/// has already dropped.
struct SilentNullAudioPlaybackStreamPacing {
    /// Who is asked for samples; `None` while the stream is not playing.
    hand_off: Option<AudioBlockForPlaybackHandOff>,
    /// Handed over to be filled every tick and then thrown away, so a tick
    /// allocates nothing.
    ///
    /// It is what makes this arm exercise the same path a device does: a
    /// speaker still assembles a real period into a real buffer, so the code
    /// a container runs is the code the rig runs.
    samples_asked_for_and_discarded: Vec<u8>,
}

/// A device that plays nothing, asking for samples at the pacing clock's
/// cadence so a graph with a speaker in it runs unchanged where no audio
/// library exists.
struct SilentNullAudioPlaybackStream {
    pacing_clock: SharedAudioClock,
    playback_stream_format: AudioStreamFormat,
    pacing: Arc<Mutex<SilentNullAudioPlaybackStreamPacing>>,
}

impl SilentNullAudioPlaybackStream {
    fn opened_on(pacing_clock: SharedAudioClock, sample_rate: NonZeroU32) -> Self {
        let playback_stream_format = silent_null_stream_format(sample_rate);
        let pacing = Arc::new(Mutex::new(SilentNullAudioPlaybackStreamPacing {
            hand_off: None,
            samples_asked_for_and_discarded: vec![
                0u8;
                playback_stream_format.interleaved_byte_count_for(
                    pacing_clock.buffer_size() as u32
                )
            ],
        }));

        // Held weakly for the same reason the capture stream holds its state
        // weakly: an `AudioClock` never unregisters a callback, so a dropped
        // stream cannot take its own back and has to leave an inert one.
        let pacing_from_tick = Arc::downgrade(&pacing);
        pacing_clock.on_tick(Box::new(move |tick: AudioTickContext| {
            if let Some(pacing) = pacing_from_tick.upgrade() {
                ask_for_one_block_and_discard_it(&pacing, playback_stream_format, tick);
            }
        }));

        Self {
            pacing_clock,
            playback_stream_format,
            pacing,
        }
    }
}

impl AudioPlaybackStream for SilentNullAudioPlaybackStream {
    fn stream_format(&self) -> AudioStreamFormat {
        self.playback_stream_format
    }

    fn start_requesting_from(&mut self, hand_off: AudioBlockForPlaybackHandOff) -> Result<()> {
        self.pacing.lock().hand_off = Some(hand_off);
        // Idempotent, and the runtime stops it at teardown. Starting it here is
        // what keeps a graph with no deviceless audio in it from ever running
        // the timer.
        self.pacing_clock.start()
    }

    fn stop_requesting(&mut self) -> Result<()> {
        // The clock keeps running for whatever else paces on it; the runtime
        // owns stopping it.
        self.pacing.lock().hand_off = None;
        Ok(())
    }
}

fn ask_for_one_block_and_discard_it(
    pacing: &Mutex<SilentNullAudioPlaybackStreamPacing>,
    playback_stream_format: AudioStreamFormat,
    tick: AudioTickContext,
) {
    let mut pacing = pacing.lock();
    if pacing.hand_off.is_none() {
        return;
    }

    let sample_count = tick.samples_needed as u32;
    let quantum_byte_count = playback_stream_format.interleaved_byte_count_for(sample_count);
    if pacing.samples_asked_for_and_discarded.len() < quantum_byte_count {
        pacing
            .samples_asked_for_and_discarded
            .resize(quantum_byte_count, 0);
    }

    let SilentNullAudioPlaybackStreamPacing {
        hand_off,
        samples_asked_for_and_discarded,
    } = &mut *pacing;
    let hand_off = hand_off
        .as_ref()
        .expect("the hand-off was present at the top of this lock scope");
    hand_off(AudioBlockRequestedByDevice {
        interleaved_sample_bytes_to_fill: &mut samples_asked_for_and_discarded
            [..quantum_byte_count],
        sample_count,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context::{AudioClock, AudioClockConfig, AudioTickCallback};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
            .open_capture_stream(&AudioDeviceStreamRequest {
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
        let Err(refusal) =
            SilentNullAudioDeviceBackend.open_capture_stream(&AudioDeviceStreamRequest {
                device_id: Some("alsa_input.pci-0000_00_1f.3".to_string()),
                deviceless_pacing_clock: clock as SharedAudioClock,
            })
        else {
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
                .open_capture_stream(&AudioDeviceStreamRequest {
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
            AudioStreamFormat {
                sample_rate: TEST_SAMPLE_RATE,
                channels: SILENT_NULL_STREAM_CHANNELS,
                sample_format: AudioSampleFormat::F32,
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
            TEST_QUANTUM_SAMPLES * SILENT_NULL_STREAM_CHANNELS as usize * 4,
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
                one_wake_time_ns
                    + 2 * TEST_QUANTUM_SAMPLES as i64 * 1_000_000_000 / TEST_SAMPLE_RATE as i64,
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

    /// One request as the stream made it, copied so an assertion can outlive
    /// the borrow the hand-off gets.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RequestedBlockRecord {
        interleaved_sample_byte_count: usize,
        sample_count: u32,
    }

    type RequestedBlockLog = Arc<Mutex<Vec<RequestedBlockRecord>>>;

    fn recording_playback_hand_off() -> (RequestedBlockLog, AudioBlockForPlaybackHandOff) {
        let requested: RequestedBlockLog = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&requested);
        let hand_off: AudioBlockForPlaybackHandOff =
            Box::new(move |block: AudioBlockRequestedByDevice<'_>| {
                recorder.lock().push(RequestedBlockRecord {
                    interleaved_sample_byte_count: block.interleaved_sample_bytes_to_fill.len(),
                    sample_count: block.sample_count,
                });
                // Written into rather than ignored, so a stream that handed
                // over a buffer it does not own would fault here.
                block.interleaved_sample_bytes_to_fill.fill(0x7F);
            });
        (requested, hand_off)
    }

    fn open_playback_stream_on(
        clock: &Arc<HandFiredTestAudioClock>,
    ) -> Box<dyn AudioPlaybackStream> {
        SilentNullAudioDeviceBackend
            .open_playback_stream(&AudioDeviceStreamRequest {
                device_id: None,
                deviceless_pacing_clock: Arc::clone(clock) as SharedAudioClock,
            })
            .expect("the null backend opens its default device on any machine")
    }

    /// Both directions carry one format here, which is what lets a microphone
    /// wired to a speaker run on this arm at all: a speaker refuses a block
    /// whose format it cannot play, and on a machine with no audio the two ends
    /// must not disagree.
    #[test]
    fn a_playback_stream_carries_the_same_format_a_capture_stream_does() {
        let clock = test_clock();
        let capture_stream = open_capture_stream_on(&clock);
        let playback_stream = open_playback_stream_on(&clock);
        assert_eq!(
            playback_stream.stream_format(),
            capture_stream.stream_format()
        );
    }

    #[test]
    fn a_named_playback_device_is_refused_by_name_rather_than_opened_as_something_else() {
        let clock = test_clock();
        let Err(refusal) =
            SilentNullAudioDeviceBackend.open_playback_stream(&AudioDeviceStreamRequest {
                device_id: Some("alsa_output.pci-0000_00_1f.3".to_string()),
                deviceless_pacing_clock: clock as SharedAudioClock,
            })
        else {
            panic!("a named device cannot exist on a backend with no devices");
        };
        assert!(
            refusal.to_string().contains("alsa_output.pci-0000_00_1f.3"),
            "the refusal must name the device that was asked for: {refusal}"
        );
    }

    /// The narrowed clock role on the playback side too: nothing starts the
    /// timer until something actually paces on it.
    #[test]
    fn opening_a_playback_stream_leaves_the_clock_stopped_and_requesting_starts_it() {
        let clock = test_clock();
        let mut stream = open_playback_stream_on(&clock);
        assert!(
            !clock.is_running(),
            "opening a stream must not start the clock"
        );

        let (_requested, hand_off) = recording_playback_hand_off();
        stream
            .start_requesting_from(hand_off)
            .expect("requesting starts the clock");
        assert!(clock.is_running());
    }

    /// A whole quantum is asked for every tick, in the stream's own format —
    /// the property a caller sizes a period against.
    #[test]
    fn every_tick_asks_for_a_full_quantum_in_the_streams_format() {
        let clock = test_clock();
        let mut stream = open_playback_stream_on(&clock);
        let stream_format = stream.stream_format();
        let (requested, hand_off) = recording_playback_hand_off();
        stream.start_requesting_from(hand_off).expect("start");

        clock.fire_one_tick_stamped(1_000_000_000);

        let requested = requested.lock();
        let [block] = requested.as_slice() else {
            panic!("one tick asks for exactly one block, got {requested:?}");
        };
        assert_eq!(block.sample_count, TEST_QUANTUM_SAMPLES as u32);
        assert_eq!(
            block.interleaved_sample_byte_count,
            stream_format.interleaved_byte_count_for(TEST_QUANTUM_SAMPLES as u32)
        );
    }

    #[test]
    fn a_stopped_playback_stream_asks_for_nothing_more() {
        let clock = test_clock();
        let mut stream = open_playback_stream_on(&clock);
        let (requested, hand_off) = recording_playback_hand_off();
        stream.start_requesting_from(hand_off).expect("start");

        clock.fire_one_tick_stamped(1_000_000_000);
        stream.stop_requesting().expect("stop");
        clock.fire_one_tick_stamped(2_000_000_000);

        assert_eq!(
            requested.lock().len(),
            1,
            "the hand-off must not be called again once stop_requesting returned"
        );
    }

    /// An `AudioClock` never unregisters a callback, so the one a dropped
    /// playback stream left behind has to go inert on its own.
    #[test]
    fn a_dropped_playback_stream_leaves_an_inert_callback_behind() {
        let clock = test_clock();
        let (requested, hand_off) = recording_playback_hand_off();
        {
            let mut stream = open_playback_stream_on(&clock);
            stream.start_requesting_from(hand_off).expect("start");
            clock.fire_one_tick_stamped(1_000_000_000);
        }

        clock.fire_one_tick_stamped(2_000_000_000);

        assert_eq!(requested.lock().len(), 1);
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
