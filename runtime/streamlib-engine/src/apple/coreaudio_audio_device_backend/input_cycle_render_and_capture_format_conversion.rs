// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

use objc2_audio_toolbox::{
    AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterGetProperty,
    AudioConverterNew, AudioConverterPrimeInfo, AudioConverterRef, AudioConverterSetProperty,
    kAudioConverterPrimeInfo, kAudioConverterPrimeMethod, kConverterPrimeMethod_None,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioStreamPacketDescription};

use crate::core::Result;
use crate::core::context::{AudioStreamFormat, CapturedAudioBlockFromDevice};

use super::audio_objects_and_device_discovery::{
    CoreAudioDevice, interleaved_f32_description_of, osstatus_as_result, rate_and_channels_of,
};
use super::{CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS, NO_ERR};

/// What one input cycle's render status calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputCycleRenderVerdict {
    /// The render succeeded: hand the block off.
    HandTheBlockOff,
    /// The stream's first failed render: drop the cycle and say so, once.
    DropTheCycleAndReportTheFirstFailure,
    /// A failed render that adds nothing to what was already said.
    DropTheCycleQuietly,
    /// Renders failed for long enough that the device has stopped delivering.
    EndTheStream { consecutive_failures: u32 },
}

/// A capture stream's record of failed input renders, which tells a device
/// that stopped from one that dropped a cycle.
#[derive(Debug, Default)]
pub(super) struct ConsecutiveInputCycleRenderFailures {
    consecutive_failures: u32,
    has_reported_a_failure: bool,
}

impl ConsecutiveInputCycleRenderFailures {
    pub(super) fn verdict_on(&mut self, render_status: i32) -> InputCycleRenderVerdict {
        if render_status == NO_ERR {
            self.consecutive_failures = 0;
            return InputCycleRenderVerdict::HandTheBlockOff;
        }
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures == CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS {
            return InputCycleRenderVerdict::EndTheStream {
                consecutive_failures: self.consecutive_failures,
            };
        }
        if !self.has_reported_a_failure {
            self.has_reported_a_failure = true;
            return InputCycleRenderVerdict::DropTheCycleAndReportTheFirstFailure;
        }
        InputCycleRenderVerdict::DropTheCycleQuietly
    }
}

/// Nanoseconds `frame_count` frames occupy at `sample_rate`, for counts that
/// run for the life of a stream.
pub(super) fn duration_of_many_frames_in_ns(frame_count: u64, sample_rate: u32) -> i128 {
    i128::from(frame_count) * 1_000_000_000 / i128::from(sample_rate.max(1))
}

/// Where a converted capture stream's blocks sit on the host clock.
///
/// Counted from the binding's first cycle: the output frames produced at the
/// stream's rate, less the converter's latency, say which lent input frame a
/// block begins at. The cycle being converted, whose first sample the device
/// timed, puts that frame on the host clock — so on a steady device the
/// stamps are the first cycle's plus frames produced, a device clock that
/// runs off nominal never pulls them off the host's, and a gap in the input
/// is a gap in the stamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ConvertedCaptureBlockTimeline {
    pub(super) device_sample_rate: u32,
    pub(super) stream_sample_rate: u32,
    converter_latency_in_device_frames: u32,
    pub(super) device_frames_lent: u64,
    pub(super) stream_frames_produced: u64,
    current_cycle_first_input_sample_ns: i64,
    current_cycle_first_device_frame: u64,
}

impl ConvertedCaptureBlockTimeline {
    pub(super) fn for_a_new_binding(
        device_sample_rate: u32,
        stream_sample_rate: u32,
        converter_latency_in_device_frames: u32,
    ) -> Self {
        Self {
            device_sample_rate,
            stream_sample_rate,
            converter_latency_in_device_frames,
            device_frames_lent: 0,
            stream_frames_produced: 0,
            current_cycle_first_input_sample_ns: 0,
            current_cycle_first_device_frame: 0,
        }
    }

    /// A cycle of `device_frame_count` input frames, whose first sample the
    /// device timed at `first_input_sample_ns`, is lent to the converter.
    pub(super) fn a_cycle_is_lent(&mut self, first_input_sample_ns: i64, device_frame_count: u32) {
        self.current_cycle_first_input_sample_ns = first_input_sample_ns;
        self.current_cycle_first_device_frame = self.device_frames_lent;
        self.device_frames_lent += u64::from(device_frame_count);
    }

    /// The stamp of the next converted block's first sample; the timeline
    /// then moves past its `converted_frame_count` frames.
    pub(super) fn stamp_the_next_block(&mut self, converted_frame_count: u32) -> i64 {
        let produced_ns =
            duration_of_many_frames_in_ns(self.stream_frames_produced, self.stream_sample_rate);
        let current_cycle_start_less_the_latency_ns = duration_of_many_frames_in_ns(
            self.current_cycle_first_device_frame
                + u64::from(self.converter_latency_in_device_frames),
            self.device_sample_rate,
        );
        self.stream_frames_produced += u64::from(converted_frame_count);
        self.current_cycle_first_input_sample_ns
            + (produced_ns - current_cycle_start_less_the_latency_ns) as i64
    }
}

/// Input frames of room past the device's longest cycle that the converted
/// buffer is sized for — more than a sample-rate converter holds back, so one
/// pass takes the whole cycle.
const CONVERTER_INPUT_HEADROOM_IN_FRAMES: u64 = 1024;

/// The input proc's answer once it has lent the cycle: not end of stream,
/// only nothing more until the device's next cycle.
const THE_INPUT_CYCLE_WAS_ALREADY_LENT: i32 = i32::from_be_bytes(*b"lent");

/// One rendered input cycle, lent to the converter's input proc for one pass.
struct RenderedInputCycleLentToTheConverter {
    interleaved_sample_bytes: *mut u8,
    frame_count: u32,
    channels: u32,
    bytes_per_frame: u32,
    has_been_lent: bool,
    was_asked_for_more_after_it_was_lent: bool,
}

unsafe extern "C-unwind" fn the_converter_asks_for_the_rendered_cycle(
    _audio_converter: AudioConverterRef,
    packet_count: NonNull<u32>,
    buffer_list: NonNull<AudioBufferList>,
    _packet_descriptions: *mut *mut AudioStreamPacketDescription,
    lent_cycle: *mut c_void,
) -> i32 {
    // SAFETY: `convert_the_cycle` passes its own `RenderedInputCycleLentToTheConverter`,
    // alive for the whole `AudioConverterFillComplexBuffer` call.
    let lent_cycle = unsafe { &mut *lent_cycle.cast::<RenderedInputCycleLentToTheConverter>() };
    if lent_cycle.has_been_lent {
        lent_cycle.was_asked_for_more_after_it_was_lent = true;
        // SAFETY: the converter's own count, writable for this call.
        unsafe { *packet_count.as_ptr() = 0 };
        return THE_INPUT_CYCLE_WAS_ALREADY_LENT;
    }
    lent_cycle.has_been_lent = true;
    // SAFETY: the converter's own buffer list and count, writable for this
    // call; an interleaved input takes exactly one buffer, pointed at the
    // rendered bytes, which stay untouched until the proc is asked again.
    unsafe {
        let buffer_list = &mut *buffer_list.as_ptr();
        buffer_list.mNumberBuffers = 1;
        buffer_list.mBuffers[0] = AudioBuffer {
            mNumberChannels: lent_cycle.channels,
            mDataByteSize: lent_cycle.frame_count * lent_cycle.bytes_per_frame,
            mData: lent_cycle.interleaved_sample_bytes.cast(),
        };
        *packet_count.as_ptr() = lent_cycle.frame_count;
    }
    NO_ERR
}

/// What converting one input cycle came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConvertedInputCycle {
    HandedOff,
    TheConverterFailed(i32),
    TheHandOffPanicked,
}

/// An `AudioConverter` from a capture device's own format to its stream's,
/// with every buffer it writes into allocated before the device thread runs
/// it.
pub(super) struct CoreAudioCaptureFormatConverter {
    audio_converter: AudioConverterRef,
    converted_buffer: Vec<u8>,
    converted_capacity_in_frames: u32,
    device_own_format: AudioStreamFormat,
    stream_format: AudioStreamFormat,
    /// Starts with the binding the converter was made for.
    converted_block_timeline: ConvertedCaptureBlockTimeline,
}

// SAFETY: the converter is driven only by the capture callback, under the
// delivery lock, and disposed of once no callback can run.
unsafe impl Send for CoreAudioCaptureFormatConverter {}

impl CoreAudioCaptureFormatConverter {
    pub(super) fn new(
        device: &CoreAudioDevice,
        device_own_format: AudioStreamFormat,
        stream_format: AudioStreamFormat,
        largest_cycle_in_frames: u32,
    ) -> Result<Self> {
        let refused = |what_the_converter_would_not_do: &str, status: i32| {
            device.refused_with_status(
                &format!(
                    "could not have its {} converted to the stream's {}: the AudioConverter {}",
                    rate_and_channels_of(device_own_format),
                    rate_and_channels_of(stream_format),
                    what_the_converter_would_not_do
                ),
                status,
            )
        };
        let mut audio_converter: AudioConverterRef = std::ptr::null_mut();
        // SAFETY: two valid descriptions and a writable slot.
        let status = unsafe {
            AudioConverterNew(
                NonNull::from(&interleaved_f32_description_of(device_own_format)),
                NonNull::from(&interleaved_f32_description_of(stream_format)),
                NonNull::from(&mut audio_converter),
            )
        };
        if status != NO_ERR || audio_converter.is_null() {
            return Err(refused("would not be created", status));
        }
        let converted_capacity_in_frames =
            ((u64::from(largest_cycle_in_frames) + CONVERTER_INPUT_HEADROOM_IN_FRAMES)
                * u64::from(stream_format.sample_rate))
            .div_ceil(u64::from(device_own_format.sample_rate.max(1))) as u32;
        let mut converter = Self {
            audio_converter,
            converted_buffer: vec![
                0u8;
                stream_format
                    .interleaved_byte_count_for(converted_capacity_in_frames)
            ],
            converted_capacity_in_frames,
            device_own_format,
            stream_format,
            converted_block_timeline: ConvertedCaptureBlockTimeline::for_a_new_binding(
                device_own_format.sample_rate,
                stream_format.sample_rate,
                0,
            ),
        };
        // Only a converter that resamples primes; one that only moves channels
        // refuses the property ('prop') and adds no delay.
        if device_own_format.sample_rate == stream_format.sample_rate {
            return Ok(converter);
        }
        // Latency mode: live input has no earlier frames to pre-seek into, and
        // the converter's first output then begins with its first input.
        let prime_method = kConverterPrimeMethod_None;
        // SAFETY: a live converter and a `u32` value, as the property takes.
        let status = unsafe {
            AudioConverterSetProperty(
                converter.audio_converter,
                kAudioConverterPrimeMethod,
                std::mem::size_of::<u32>() as u32,
                NonNull::from(&prime_method).cast(),
            )
        };
        if status != NO_ERR {
            return Err(refused("refused latency mode", status));
        }
        converter
            .converted_block_timeline
            .converter_latency_in_device_frames = converter
            .latency_in_device_frames()
            .map_err(|status| refused("reported no latency", status))?;
        Ok(converter)
    }

    /// In latency mode a resampling converter delays its output by
    /// `trailingFrames` at the input rate.
    fn latency_in_device_frames(&self) -> std::result::Result<u32, i32> {
        let mut prime_info = AudioConverterPrimeInfo {
            leadingFrames: 0,
            trailingFrames: 0,
        };
        let mut prime_info_byte_count = std::mem::size_of::<AudioConverterPrimeInfo>() as u32;
        // SAFETY: a live converter and a writable `AudioConverterPrimeInfo`.
        let status = unsafe {
            AudioConverterGetProperty(
                self.audio_converter,
                kAudioConverterPrimeInfo,
                NonNull::from(&mut prime_info_byte_count),
                NonNull::from(&mut prime_info).cast(),
            )
        };
        osstatus_as_result(status).map(|()| prime_info.trailingFrames)
    }

    /// Convert one rendered input cycle and hand off every block it yields,
    /// stamped on the binding's timeline.
    pub(super) fn convert_the_cycle(
        &mut self,
        rendered_interleaved_sample_bytes: &mut [u8],
        rendered_frame_count: u32,
        first_input_sample_timestamp_ns: i64,
        hand_off: &dyn Fn(CapturedAudioBlockFromDevice<'_>),
    ) -> ConvertedInputCycle {
        // A lent cycle of no frames would read as the end of the stream.
        if rendered_frame_count == 0 {
            return ConvertedInputCycle::HandedOff;
        }
        let Self {
            audio_converter,
            converted_buffer,
            converted_capacity_in_frames,
            device_own_format,
            stream_format,
            converted_block_timeline,
        } = self;
        converted_block_timeline
            .a_cycle_is_lent(first_input_sample_timestamp_ns, rendered_frame_count);
        let mut lent_cycle = RenderedInputCycleLentToTheConverter {
            interleaved_sample_bytes: rendered_interleaved_sample_bytes.as_mut_ptr(),
            frame_count: rendered_frame_count,
            channels: device_own_format.channels,
            bytes_per_frame: device_own_format.interleaved_byte_count_for(1) as u32,
            has_been_lent: false,
            was_asked_for_more_after_it_was_lent: false,
        };
        let bytes_per_converted_frame = stream_format.interleaved_byte_count_for(1);
        loop {
            let mut converted_frame_count = *converted_capacity_in_frames;
            let mut converted_buffer_list = AudioBufferList {
                mNumberBuffers: 1,
                mBuffers: [AudioBuffer {
                    mNumberChannels: stream_format.channels,
                    mDataByteSize: converted_buffer.len() as u32,
                    mData: converted_buffer.as_mut_ptr().cast(),
                }],
            };
            // SAFETY: a live converter, an input proc reading the cycle lent
            // here, and an output list over `converted_buffer`, which holds
            // the capacity passed.
            let status = unsafe {
                AudioConverterFillComplexBuffer(
                    *audio_converter,
                    Some(the_converter_asks_for_the_rendered_cycle),
                    (&mut lent_cycle as *mut RenderedInputCycleLentToTheConverter).cast(),
                    NonNull::from(&mut converted_frame_count),
                    NonNull::from(&mut converted_buffer_list),
                    std::ptr::null_mut(),
                )
            };
            if status != NO_ERR && status != THE_INPUT_CYCLE_WAS_ALREADY_LENT {
                return ConvertedInputCycle::TheConverterFailed(status);
            }
            let converted_frame_count = converted_frame_count.min(*converted_capacity_in_frames);
            if converted_frame_count > 0 {
                let first_sample_timestamp_ns =
                    converted_block_timeline.stamp_the_next_block(converted_frame_count);
                let converted_byte_count =
                    converted_frame_count as usize * bytes_per_converted_frame;
                let handed_off = catch_unwind(AssertUnwindSafe(|| {
                    hand_off(CapturedAudioBlockFromDevice {
                        interleaved_sample_bytes: &converted_buffer[..converted_byte_count],
                        sample_count: converted_frame_count,
                        first_sample_timestamp_ns,
                    })
                }));
                if handed_off.is_err() {
                    return ConvertedInputCycle::TheHandOffPanicked;
                }
            }
            // Asked again means the whole cycle was taken; only then may the
            // next render overwrite it.
            if lent_cycle.was_asked_for_more_after_it_was_lent || converted_frame_count == 0 {
                return ConvertedInputCycle::HandedOff;
            }
        }
    }
}

impl Drop for CoreAudioCaptureFormatConverter {
    fn drop(&mut self) {
        // SAFETY: a live converter no callback can reach any more.
        unsafe { AudioConverterDispose(self.audio_converter) };
    }
}
