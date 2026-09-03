// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The read-side windowing stage: exact blocks in the contract's format,
//! between a windowed port's mailbox and its reader.
//!
//! Windowing is N-in → M-out — one 1024-sample device quantum satisfies two
//! 512-sample windows, and a one-second rolling window needs about forty-seven
//! of them — so this sits between the two and is fed one consumed bag at a
//! time. The order of operations is fixed: decode to f32 → channel-convert →
//! resample → frame → encode to the declared dtype. Internal arithmetic is f32
//! always, because the resampler speaks nothing else.
//!
//! Two rules do the load-bearing work and neither is hygiene:
//!
//! - **No sample is invented to bridge a gap.** A block whose stamp misses its
//!   expected position flushes the accumulator *and the resampler's own filter
//!   state*: a polyphase resampler holds a filter's length of pre-gap samples,
//!   and emitting through it after the gap blends audio across the loss.
//! - **An emitted sample always derives from real input.** The resampler's
//!   first `output_delay()` output frames are filter priming, not audio, so
//!   they are discarded at stream start and after every flush. That discard
//!   *is* the group-delay subtraction the stamp rule names: the kept output
//!   stream is re-indexed so its frame zero is the input frame the anchor
//!   stamped, and a window's stamp is then `anchor + frame / rate` outright.
//!
//! The stage derives a stamp; it never reads a clock.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use super::super::input::BagBodyForTheReader;
use super::audio_block_bag_wire_codec::{
    AudioBlockReadFromTheWire, encode_an_audio_block_onto_the_wire,
    read_an_audio_block_off_the_wire,
};
use super::resolved_audio_window_contract::ResolvedAudioWindowContract;
use crate::core::error::{Error, Result};

/// Source frames the resampler consumes per call — about 5 ms at 48 kHz, so a
/// single device quantum at the engine's own preferred period feeds at least
/// one whole chunk and no bag waits on the next one to produce output.
const RESAMPLER_SOURCE_FRAMES_PER_CHUNK: usize = 256;

/// Windowed sinc filter length. 128 taps is the quality/cost point for the
/// speech rates this contract exists for; the flagship case is 48 kHz to
/// 16 kHz, where the filter is what keeps the decimation from aliasing.
const RESAMPLER_SINC_FILTER_LENGTH: usize = 128;

/// The rate and channel count a run's source blocks arrive in. A block that
/// disagrees with the run's is a format change, handled like a gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceAudioFormat {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u32,
}

/// The format of the most recent bag pushed into a windowed port's mailbox,
/// written by that mailbox's measure as the bag arrives.
///
/// Shared with the stage so its readiness floor is exact before it has
/// consumed anything at all: the rate settles the resampler's ratio, and for a
/// contract that declared no channel count the count settles how many channels
/// that resampler runs across.
#[derive(Debug, Default)]
pub(crate) struct LatestQueuedSourceAudioFormat {
    /// The rate in the high half and the channel count in the low, so the pair
    /// is written and read as one value and no reader can pair one bag's rate
    /// with another's count.
    ///
    /// Zero is the unset state, which no real format collides with: the
    /// measure that writes here refuses a bag stating zero in either half.
    rate_and_channels: AtomicU64,
}

impl LatestQueuedSourceAudioFormat {
    pub(crate) fn record(&self, format: SourceAudioFormat) {
        self.rate_and_channels.store(
            u64::from(format.sample_rate) << 32 | u64::from(format.channels),
            Ordering::Relaxed,
        );
    }

    /// What the mailbox last saw, or `None` before any bag has reached it.
    pub(crate) fn read(&self) -> Option<SourceAudioFormat> {
        let packed = self.rate_and_channels.load(Ordering::Relaxed);
        (packed != 0).then(|| SourceAudioFormat {
            sample_rate: (packed >> 32) as u32,
            channels: packed as u32,
        })
    }
}

/// What a rate conversion was built for.
///
/// A resampler is built for one ratio across one channel count, so a change in
/// either rebuilds it — which is how a contract following its source re-mints
/// when the source's channel count changes, exactly as it does when the rate
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RateConversionInputs {
    source_sample_rate: u32,
    channels_converted: u32,
}

impl RateConversionInputs {
    /// What a conversion must be built for when blocks arrive in this format
    /// under this contract.
    fn for_a_source_in(format: SourceAudioFormat, contract: ResolvedAudioWindowContract) -> Self {
        Self {
            source_sample_rate: format.sample_rate,
            channels_converted: contract.channels_a_window_carries_from(format),
        }
    }
}

/// How the stage gets from the source rate to the contract's.
enum AudioWindowRateConversion {
    /// The rates already agree, so the samples pass through untouched — no
    /// filter, no priming, no group delay, and a window carries the source's
    /// own scalars unchanged.
    RatesAlreadyAgree,
    /// A polyphase sinc resampler, fed fixed-size source chunks.
    Resampled(Box<Async<f32>>),
}

impl AudioWindowRateConversion {
    fn build(inputs: RateConversionInputs, contract_sample_rate: u32) -> Result<Self> {
        if inputs.source_sample_rate == contract_sample_rate {
            return Ok(AudioWindowRateConversion::RatesAlreadyAgree);
        }
        let parameters = SincInterpolationParameters {
            sinc_len: RESAMPLER_SINC_FILTER_LENGTH,
            f_cutoff: None,
            oversampling_factor: 128,
            interpolation: SincInterpolationType::Cubic,
            window: WindowFunction::BlackmanHarris2,
        };
        let resampler = Async::<f32>::new_sinc(
            f64::from(contract_sample_rate) / f64::from(inputs.source_sample_rate),
            // The ratio is fixed for the life of a run: a source that changes
            // format rebuilds the resampler rather than retuning it.
            1.0,
            &parameters,
            RESAMPLER_SOURCE_FRAMES_PER_CHUNK,
            inputs.channels_converted as usize,
            FixedAsync::Input,
        )
        .map_err(|construction_failure| {
            Error::Configuration(format!(
                "no resampler could be built from {} Hz to {contract_sample_rate} Hz across \
                 {} channels: {construction_failure}",
                inputs.source_sample_rate, inputs.channels_converted
            ))
        })?;
        Ok(AudioWindowRateConversion::Resampled(Box::new(resampler)))
    }

    /// Output frames of filter priming to discard before the first real one.
    fn priming_output_frames(&self) -> usize {
        match self {
            AudioWindowRateConversion::RatesAlreadyAgree => 0,
            AudioWindowRateConversion::Resampled(resampler) => resampler.output_delay(),
        }
    }

    /// Source frames one call consumes; `None` when nothing is chunked.
    fn source_frames_per_call(&self) -> Option<usize> {
        match self {
            AudioWindowRateConversion::RatesAlreadyAgree => None,
            AudioWindowRateConversion::Resampled(resampler) => Some(resampler.input_frames_next()),
        }
    }

    /// Drop every sample the filter is holding, so nothing from before a gap
    /// can reach an emitted window after it.
    fn forget_everything_held(&mut self) {
        if let AudioWindowRateConversion::Resampled(resampler) = self {
            resampler.reset();
        }
    }
}

/// One windowed input port's stage: the accumulator sitting between the port's
/// counted mailbox and its reader.
///
/// Holds only the already-consumed remainder — under one window's worth of
/// output plus under one resampler chunk of source — and never evicts. Bags
/// stay in the counted mailbox until [`Self::accept`] takes one, which is what
/// keeps the per-link drop counters the authority on loss at this port.
pub(crate) struct AudioWindowAccumulator {
    port_name: String,
    contract: ResolvedAudioWindowContract,
    /// The format the last accepted block arrived in, for spotting a source
    /// that changes format mid-stream.
    source_format: Option<SourceAudioFormat>,
    rate_conversion: AudioWindowRateConversion,
    /// What [`Self::rate_conversion`] was built for. A flush keeps the
    /// conversion and resets it; only a change of rate or channel count
    /// rebuilds it.
    rate_conversion_built_for: Option<RateConversionInputs>,
    /// The format of the most recent bag pushed into the port's mailbox,
    /// written by that mailbox's measure. Lets the readiness floor be exact
    /// before the stage has consumed anything at all.
    latest_queued_source_audio_format: Arc<LatestQueuedSourceAudioFormat>,
    /// Source-rate scalars already in the count windows are emitted in,
    /// waiting for a whole resampler chunk.
    channel_converted_source_scalars: Vec<f32>,
    /// Contract-rate scalars, interleaved by the count windows are emitted in,
    /// whose front frame is [`Self::next_window_start_output_frame`].
    windowable_output_scalars: VecDeque<f32>,
    /// Where the resampler writes one call's output before it is appended.
    /// Held rather than allocated per bag: this is the per-block read path.
    resampler_output_scratch: Vec<f32>,
    /// Where one window is gathered out of the deque before it is encoded.
    /// Held for the same reason, one per hop rather than one per bag.
    window_scratch: Vec<f32>,
    /// Whether the port has already been told it cannot form a window from a
    /// mailbox that is full. Said once per port, not once per read.
    warned_that_a_full_mailbox_still_cannot_fill_a_window: bool,
    /// Priming frames the current run still owes before an output frame counts
    /// as derived from real input.
    priming_output_frames_still_to_discard: usize,
    /// The device stamp anchoring the current contiguous run; `None` between
    /// runs.
    run_anchor_timestamp_ns: Option<i64>,
    /// Output frames, since the anchor, at which the next window starts.
    next_window_start_output_frame: u64,
    /// Where the next source block's first sample is expected, derived from
    /// the previous block's stamp and count.
    expected_next_source_timestamp_ns: Option<i64>,
    /// Half the previous block's own duration: the tolerance a status-derived
    /// device stamp jitters within before it reads as a gap. A stamp is exact
    /// to the block, not to the sample.
    gap_tolerance_ns: i64,
}

impl AudioWindowAccumulator {
    /// Build the stage one windowed port reads through.
    ///
    /// `latest_queued_source_audio_format` is shared with the mailbox's
    /// measure, which writes every arriving bag's format into it.
    pub(crate) fn new(
        port_name: &str,
        contract: ResolvedAudioWindowContract,
        latest_queued_source_audio_format: Arc<LatestQueuedSourceAudioFormat>,
    ) -> Self {
        Self {
            port_name: port_name.to_string(),
            contract,
            source_format: None,
            rate_conversion: AudioWindowRateConversion::RatesAlreadyAgree,
            rate_conversion_built_for: None,
            latest_queued_source_audio_format,
            channel_converted_source_scalars: Vec::new(),
            windowable_output_scalars: VecDeque::new(),
            resampler_output_scratch: Vec::new(),
            window_scratch: Vec::new(),
            warned_that_a_full_mailbox_still_cannot_fill_a_window: false,
            priming_output_frames_still_to_discard: 0,
            run_anchor_timestamp_ns: None,
            next_window_start_output_frame: 0,
            expected_next_source_timestamp_ns: None,
            gap_tolerance_ns: 0,
        }
    }

    /// Take one bag consumed from the port's mailbox through the stage.
    ///
    /// Refuses by name rather than reshaping: a dtype it does not know, a
    /// payload whose length disagrees with the count beside it, a bag with no
    /// audio-block keys, or a channel pair with neither side at one.
    pub(crate) fn accept(&mut self, bag_body: &[u8]) -> Result<()> {
        let block = read_an_audio_block_off_the_wire(bag_body).map_err(|refusal| {
            Error::AudioWindowStageCannotReadTheBag {
                port: self.port_name.clone(),
                refusal: refusal.to_string(),
            }
        })?;

        let arriving = SourceAudioFormat {
            sample_rate: block.sample_rate,
            channels: block.channels,
        };
        if arriving.sample_rate == 0 || arriving.channels == 0 {
            return Err(Error::AudioWindowStageCannotReadTheBag {
                port: self.port_name.clone(),
                refusal: format!(
                    "the block states {} Hz across {} channels, and neither may be zero",
                    arriving.sample_rate, arriving.channels
                ),
            });
        }

        if self
            .source_format
            .is_some_and(|running| running != arriving)
        {
            self.flush("the source changed format mid-stream");
        }
        self.source_format = Some(arriving);
        let rate_conversion_inputs = RateConversionInputs::for_a_source_in(arriving, self.contract);
        self.build_the_rate_conversion_if_its_inputs_are_new(rate_conversion_inputs)?;

        let arrived_away_from_where_the_last_block_ended = self
            .expected_next_source_timestamp_ns
            .is_some_and(|expected| {
                block.first_sample_timestamp_ns.abs_diff(expected) > self.gap_tolerance_ns as u64
            });
        if arrived_away_from_where_the_last_block_ended {
            self.flush("a block arrived away from where the previous one ended");
        }

        if self.run_anchor_timestamp_ns.is_none() {
            self.run_anchor_timestamp_ns = Some(block.first_sample_timestamp_ns);
            self.next_window_start_output_frame = 0;
            self.priming_output_frames_still_to_discard =
                self.rate_conversion.priming_output_frames();
        }

        self.append_the_blocks_samples_in_the_count_windows_are_emitted_in(&block)?;

        let block_duration_ns =
            frames_as_nanoseconds(u64::from(block.sample_count), arriving.sample_rate);
        self.expected_next_source_timestamp_ns =
            Some(block.first_sample_timestamp_ns + block_duration_ns);
        self.gap_tolerance_ns = block_duration_ns / 2;

        self.push_whole_chunks_through_the_rate_conversion(
            rate_conversion_inputs.channels_converted,
        )
    }

    /// The next full window, encoded as an ordinary audio-block bag, with the
    /// timestamp its first sample derives.
    ///
    /// `None` means the stage holds less than one window — the caller feeds it
    /// another bag and asks again.
    pub(crate) fn next_ready_window(&mut self) -> Result<Option<BagBodyForTheReader>> {
        // `None` only before the first block of a run has been accepted on a
        // contract that declared no count, at which point nothing is held.
        let Some(channels) = self.channels_windows_are_emitted_in() else {
            return Ok(None);
        };
        let scalars_per_window = self.scalars_per_window_at(channels);
        if self.windowable_output_scalars.len() < scalars_per_window {
            return Ok(None);
        }
        let Some(anchor) = self.run_anchor_timestamp_ns else {
            return Ok(None);
        };

        self.window_scratch.clear();
        self.window_scratch.extend(
            self.windowable_output_scalars
                .iter()
                .take(scalars_per_window)
                .copied(),
        );
        let first_sample_timestamp_ns = anchor
            + frames_as_nanoseconds(
                self.next_window_start_output_frame,
                self.contract.sample_rate,
            );

        let bag = encode_an_audio_block_onto_the_wire(
            &self.window_scratch,
            self.contract.sample_rate,
            channels,
            self.contract.window_size,
            self.contract.dtype,
            first_sample_timestamp_ns,
        )
        .map_err(|encode_failure| Error::BagEncodeFailed(encode_failure.to_string()))?;

        // A hop below the window size leaves the overlap in place for the next
        // window; a hop equal to it drains the whole one.
        let scalars_per_hop = self.contract.hop as usize * channels as usize;
        self.windowable_output_scalars.drain(..scalars_per_hop);
        self.next_window_start_output_frame += u64::from(self.contract.hop);

        Ok(Some(BagBodyForTheReader {
            body: bag,
            first_sample_or_publish_timestamp_ns: first_sample_timestamp_ns,
        }))
    }

    /// Whether this port has already been told its mailbox cannot fill a
    /// window.
    ///
    /// Test-only: the warning itself is a log line and the engine tree has no
    /// capture to assert one against, so the guard that makes it once-per-port
    /// is asserted through the state it sets.
    #[cfg(test)]
    pub(crate) fn has_said_a_full_mailbox_cannot_fill_a_window(&self) -> bool {
        self.warned_that_a_full_mailbox_still_cannot_fill_a_window
    }

    /// Whether a full window can be emitted right now.
    pub(crate) fn holds_a_full_window(&self) -> bool {
        self.run_anchor_timestamp_ns.is_some()
            && self
                .scalars_per_window()
                .is_some_and(|scalars| self.windowable_output_scalars.len() >= scalars)
    }

    /// The channel count the windows this stage emits are interleaved by: the
    /// contract's when it declared one, and otherwise the source's own.
    ///
    /// `None` only before the first block of a run has been accepted on a
    /// contract that declared no count — the one moment at which the stage
    /// holds nothing to measure and has nothing to emit. A flush keeps the
    /// last accepted format, so it is `None` at most once per port.
    fn channels_windows_are_emitted_in(&self) -> Option<u32> {
        match self.source_format {
            Some(format) => Some(self.contract.channels_a_window_carries_from(format)),
            // Nothing has arrived yet: a contract that stated a count already
            // knows it, and one that follows its source does not.
            None => self.contract.channels,
        }
    }

    /// Scalars one emitted window carries: `window_size × channels`.
    fn scalars_per_window_at(&self, channels: u32) -> usize {
        self.contract.window_size as usize * channels as usize
    }

    fn scalars_per_window(&self) -> Option<usize> {
        self.channels_windows_are_emitted_in()
            .map(|channels| self.scalars_per_window_at(channels))
    }

    /// Whether a full window would be emittable once the bags still queued in
    /// the port's mailbox — worth `queued_output_frame_equivalents` frames at
    /// the contract's rate — have been taken through the stage.
    ///
    /// A floor, never an estimate: the readiness gate must not claim a window
    /// the read then cannot produce, because a reactive `process()` that woke
    /// and found nothing is exactly the shape the window contract exists to
    /// rule out. Under-reporting costs the drain loop one more bag before it
    /// dispatches; over-reporting costs the contract.
    ///
    /// One bounded exception, and it is inherent rather than an oversight: the
    /// measure counts a queued bag's samples and never reads its stamp, so a
    /// discontinuity or a format change sitting in the queue is invisible here
    /// and the read that follows flushes what it had accumulated. That costs
    /// exactly one empty read per discontinuity — the samples before the gap
    /// are discarded by design, so there genuinely is no window to hand over —
    /// and the queue is counted afresh from the next bag. Reading every queued
    /// bag's stamp at this gate would decode the whole mailbox on every wake to
    /// forecast an event that already cost the stream audio.
    ///
    /// `the_mailbox_is_full` only sharpens the diagnosis: a port that cannot
    /// form a window from a mailbox with no room left is stalled, and says so.
    pub(crate) fn a_full_window_would_be_ready_after(
        &mut self,
        queued_output_frame_equivalents: u64,
        the_mailbox_is_full: bool,
    ) -> bool {
        if self.holds_a_full_window() {
            return true;
        }
        // Building the conversion from the format the mailbox last saw is what
        // makes the floor exact rather than worst-case on the first question,
        // before any bag has been consumed. It costs one construction, reused
        // by the read that follows, and touches nothing a run depends on. The
        // channel count is part of that format because a contract declaring
        // none resamples across the source's own.
        //
        // Only between runs: mid-run the conversion is already built for the
        // format the run is on, and a queued bag announcing a new one is a
        // format change for `accept` to flush — rebuilding here would throw
        // away the filter state and the remainder a reader is still owed.
        let latest_queued = self.latest_queued_source_audio_format.read();
        if self.run_anchor_timestamp_ns.is_none()
            && let Some(queued) = latest_queued
            && self
                .build_the_rate_conversion_if_its_inputs_are_new(
                    RateConversionInputs::for_a_source_in(queued, self.contract),
                )
                .is_err()
        {
            // The read path reports the failure with the bag that caused it;
            // reporting "nothing is ready" here leaves it to do that.
            return false;
        }

        let held = self.output_frames_held() as u64;
        let (staged_equivalents, slack) = self.staged_output_frame_equivalents_and_slack(
            latest_queued.map_or(0, |queued| queued.sample_rate),
        );
        let reachable = held
            .saturating_add(staged_equivalents)
            .saturating_add(queued_output_frame_equivalents);
        let ready = reachable >= u64::from(self.contract.window_size).saturating_add(slack);

        if !ready && the_mailbox_is_full {
            self.warn_once_that_a_full_mailbox_cannot_fill_a_window(reachable);
        }
        ready
    }

    /// Say, once, that this port is stalled: its mailbox has no room left and
    /// what it holds still cannot make one window.
    ///
    /// The depth a windowed port is sized to assumes a source quantum, because
    /// the real one arrives with the bags rather than with the declaration. A
    /// source publishing quanta far smaller than that assumption can fill the
    /// mailbox without ever filling a window, and every bag past that point is
    /// evicted. The per-link drop counter climbs, which is true but says
    /// nothing about why — this names the port, the window it owes and how far
    /// short a full mailbox falls.
    fn warn_once_that_a_full_mailbox_cannot_fill_a_window(&mut self, reachable_output_frames: u64) {
        if self.warned_that_a_full_mailbox_still_cannot_fill_a_window {
            return;
        }
        self.warned_that_a_full_mailbox_still_cannot_fill_a_window = true;
        tracing::warn!(
            port = %self.port_name,
            window_size = self.contract.window_size,
            reachable_output_frames,
            "audio window stage: this port's mailbox is full and everything in it still \
             makes less than one window, so it is delivering nothing and evicting every \
             further bag. The depth is derived from an assumed source quantum; a source \
             publishing much smaller blocks than that outruns it"
        );
    }

    /// Output frames the stage already holds, whole windows included.
    fn output_frames_held(&self) -> usize {
        self.frames_in(self.windowable_output_scalars.len())
    }

    /// Source frames staged for the rate conversion but not yet consumed by
    /// it — under one resampler chunk.
    fn staged_source_frames_held(&self) -> usize {
        self.frames_in(self.channel_converted_source_scalars.len())
    }

    /// Interleaved scalars as frames, in the count windows are emitted in.
    ///
    /// A count the stage does not know yet can only mean a contract that
    /// declared none and has accepted no block, and both buffers are empty
    /// then — so there is nothing to divide rather than a divisor to guess.
    /// A known count is never zero: a declared one is refused at zero by
    /// [`ResolvedAudioWindowContract::from_declared_values`] and a source's own
    /// by `accept`.
    fn frames_in(&self, interleaved_scalars: usize) -> usize {
        self.channels_windows_are_emitted_in()
            .map_or(0, |channels| interleaved_scalars / channels as usize)
    }

    /// What the staged source frames are worth at the contract's rate, and the
    /// frames the floor must give back before claiming a window.
    ///
    /// The slack is what the ratio arithmetic cannot see: priming still owed,
    /// up to one incomplete resampler chunk that will not be processed, and
    /// two frames for the fractional index the resampler carries between
    /// calls (carried, not accumulated — the cumulative output tracks the
    /// ratio across a whole run, not per chunk).
    fn staged_output_frame_equivalents_and_slack(
        &self,
        latest_queued_source_rate: u32,
    ) -> (u64, u64) {
        let source_rate = self
            .source_format
            .map(|format| format.sample_rate)
            .unwrap_or(latest_queued_source_rate)
            .max(1);
        let staged_source_frames = self.staged_source_frames_held() as u64;
        let equivalents =
            staged_source_frames * u64::from(self.contract.sample_rate) / u64::from(source_rate);

        let slack = match self.rate_conversion.source_frames_per_call() {
            None => 0,
            Some(chunk) => {
                let one_chunk_at_the_output_rate = (chunk as u64
                    * u64::from(self.contract.sample_rate))
                .div_ceil(u64::from(source_rate));
                one_chunk_at_the_output_rate + 2
            }
        };
        (
            equivalents,
            slack + self.priming_output_frames_still_to_discard as u64,
        )
    }

    /// Build the rate conversion when these are the first inputs the stage has
    /// seen, or differ from the ones it was built for.
    fn build_the_rate_conversion_if_its_inputs_are_new(
        &mut self,
        inputs: RateConversionInputs,
    ) -> Result<()> {
        if self.rate_conversion_built_for == Some(inputs) {
            return Ok(());
        }
        self.rate_conversion = AudioWindowRateConversion::build(inputs, self.contract.sample_rate)?;
        self.rate_conversion_built_for = Some(inputs);
        Ok(())
    }

    /// Discard the remainder and the filter's held samples, so the next block
    /// starts a run of its own.
    ///
    /// The discarded remainder is under one window — not a bag, and not
    /// counted as one; the port's per-link drop counters stay the authority on
    /// bags lost.
    fn flush(&mut self, why: &str) {
        let discarded_output_frames = self.output_frames_held();
        let discarded_source_frames = self.staged_source_frames_held();
        self.windowable_output_scalars.clear();
        self.channel_converted_source_scalars.clear();
        self.rate_conversion.forget_everything_held();
        self.run_anchor_timestamp_ns = None;
        self.expected_next_source_timestamp_ns = None;
        self.next_window_start_output_frame = 0;
        self.priming_output_frames_still_to_discard = 0;

        tracing::info!(
            port = %self.port_name,
            discarded_output_frames,
            discarded_source_frames,
            "audio window stage: {why}, so the accumulator and the resampler's filter state \
             were flushed rather than emitting a window that spans the gap"
        );
    }

    /// Append this block's samples to the staging buffer in the count windows
    /// are emitted in.
    ///
    /// A contract that declared no count converts nothing: the block's samples
    /// are already in the count its windows carry. A declared count converts
    /// both directions by fixed rule — N→1 averages, 1→N duplicates, and any
    /// other N→M is refused naming both counts. The source count arrives with
    /// the bags, so declaration could not have seen it.
    ///
    /// Written straight into the buffer the stage keeps, from the decode's own
    /// iterator: the samples are being reshaped anyway, so neither the decoded
    /// scalars nor the converted ones need a buffer of their own first.
    fn append_the_blocks_samples_in_the_count_windows_are_emitted_in(
        &mut self,
        block: &AudioBlockReadFromTheWire<'_>,
    ) -> Result<()> {
        let source_channels = block.channels;
        let Some(contract_channels) = self.contract.channels else {
            self.channel_converted_source_scalars
                .extend(block.interleaved_samples_as_f32());
            return Ok(());
        };
        if source_channels != contract_channels && contract_channels != 1 && source_channels != 1 {
            return Err(Error::AudioWindowStageChannelConversionRefused {
                port: self.port_name.clone(),
                source_channels,
                contract_channels,
            });
        }

        let staging = &mut self.channel_converted_source_scalars;
        let mut samples = block.interleaved_samples_as_f32();
        if source_channels == contract_channels {
            staging.extend(samples);
        } else if contract_channels == 1 {
            let source_channels = source_channels as usize;
            let reciprocal = 1.0 / source_channels as f32;
            staging.reserve(block.sample_count as usize);
            for _ in 0..block.sample_count {
                let across_the_frame: f32 = samples.by_ref().take(source_channels).sum();
                staging.push(across_the_frame * reciprocal);
            }
        } else {
            staging.extend(
                samples.flat_map(|sample| std::iter::repeat_n(sample, contract_channels as usize)),
            );
        }
        Ok(())
    }

    /// Run every whole chunk the staging buffer holds through the rate
    /// conversion, appending what survives priming to the windowable output.
    /// The count is passed in rather than re-derived: `accept` is the only
    /// caller and has just settled it against the block it accepted, which is
    /// also the count the conversion was built for.
    fn push_whole_chunks_through_the_rate_conversion(
        &mut self,
        channels_windows_are_emitted_in: u32,
    ) -> Result<()> {
        let emitted_channels = channels_windows_are_emitted_in as usize;
        let AudioWindowRateConversion::Resampled(resampler) = &mut self.rate_conversion else {
            let passed_through = std::mem::take(&mut self.channel_converted_source_scalars);
            self.windowable_output_scalars.extend(passed_through);
            return Ok(());
        };

        let source_frames_per_call = resampler.input_frames_next();
        let scalars_per_call = source_frames_per_call * emitted_channels;
        let output_scratch = &mut self.resampler_output_scratch;
        output_scratch.resize(resampler.output_frames_max() * emitted_channels, 0.0);
        let output_frames_available = output_scratch.len() / emitted_channels;
        let mut consumed_scalars = 0usize;

        while self.channel_converted_source_scalars.len() - consumed_scalars >= scalars_per_call {
            let input = InterleavedSlice::new(
                &self.channel_converted_source_scalars
                    [consumed_scalars..consumed_scalars + scalars_per_call],
                emitted_channels,
                source_frames_per_call,
            )
            .map_err(refused_resampler_buffer)?;
            let mut output = InterleavedSlice::new_mut(
                output_scratch,
                emitted_channels,
                output_frames_available,
            )
            .map_err(refused_resampler_buffer)?;

            let (_, output_frames_written) = resampler
                .process_into_buffer(&input, &mut output, None)
                .map_err(|resample_failure| {
                    Error::Configuration(format!(
                        "the audio window resampler failed: {resample_failure}"
                    ))
                })?;
            consumed_scalars += scalars_per_call;

            let discarded = self
                .priming_output_frames_still_to_discard
                .min(output_frames_written);
            self.priming_output_frames_still_to_discard -= discarded;
            self.windowable_output_scalars.extend(
                &output_scratch
                    [discarded * emitted_channels..output_frames_written * emitted_channels],
            );
        }

        self.channel_converted_source_scalars
            .drain(..consumed_scalars);
        Ok(())
    }
}

/// `frames` at `rate` as nanoseconds, in widened integer arithmetic.
///
/// Never an accumulated per-sample delta, which drifts at 44.1 kHz-family
/// rates: every offset is computed from the frame index against the rate.
///
/// Widened to `u128` because the frame index is not: it counts every output
/// frame since the run's anchor and only a flush resets it, so at `u64` the
/// multiply overflows after 4.4 days of one contiguous run at 48 kHz and 1.1
/// days at 192 kHz — silently, in release. Both callers refuse a zero rate
/// before reaching here, the contract's constructor for one and `accept` for
/// the other.
fn frames_as_nanoseconds(frames: u64, rate: u32) -> i64 {
    (u128::from(frames) * 1_000_000_000 / u128::from(rate)) as i64
}

fn refused_resampler_buffer(size_error: impl std::fmt::Display) -> Error {
    Error::Configuration(format!(
        "the audio window stage sized a resampler buffer wrong: {size_error}"
    ))
}

#[cfg(test)]
mod stamp_arithmetic_tests {
    use super::frames_as_nanoseconds;

    /// The frame index counts every output frame since the run's anchor and
    /// only a flush resets it, so a long-lived always-on node reaches numbers a
    /// `u64` multiply cannot hold: `u64::MAX / 1e9` is 1.84e10 frames, which is
    /// 4.4 days at 48 kHz and 1.1 days at 192 kHz. Release builds wrap there
    /// silently and every window stamp after it is garbage.
    #[test]
    fn a_frame_index_past_a_u64_multiplys_reach_is_still_stamped_exactly() {
        for rate in [16_000u32, 44_100, 48_000, 192_000] {
            let a_week = u64::from(rate) * 60 * 60 * 24 * 7;
            assert_eq!(
                frames_as_nanoseconds(a_week, rate),
                7 * 24 * 60 * 60 * 1_000_000_000,
                "a week of contiguous {rate} Hz audio must still stamp at a week"
            );
        }
    }

    /// Exactness at the sizes a window actually uses is what the 32 ms cadence
    /// assertion rests on, and it must not have been traded for the headroom.
    #[test]
    fn the_widening_changes_no_answer_a_window_sized_run_produces() {
        for rate in [16_000u32, 44_100, 48_000] {
            for frames in [0u64, 1, 160, 512, 1024, 16_000, 1_000_000] {
                assert_eq!(
                    frames_as_nanoseconds(frames, rate),
                    (frames * 1_000_000_000 / u64::from(rate)) as i64,
                );
            }
        }
    }
}
