// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio backend chain's Apple arm: CoreAudio, through the AUHAL audio unit.
//!
//! Each stream owns one `kAudioUnitSubType_HALOutput` unit with I/O enabled in
//! the stream's direction only. A stream opened with no `device_id` follows the
//! system default device, keeping the format it opened with. The device's I/O
//! thread is the cadence source, and a block's stamp is the device's
//! `mHostTime` — the `mach_absolute_time` domain every other timestamp on Apple
//! lives in.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Weak};

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2_audio_toolbox::{
    AURenderCallback, AURenderCallbackStruct, AudioComponentDescription, AudioComponentFindNext,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioConverterDispose,
    AudioConverterFillComplexBuffer, AudioConverterGetProperty, AudioConverterNew,
    AudioConverterPrimeInfo, AudioConverterRef, AudioConverterSetProperty, AudioOutputUnitStart,
    AudioOutputUnitStop, AudioUnit, AudioUnitGetProperty, AudioUnitInitialize, AudioUnitRender,
    AudioUnitRenderActionFlags, AudioUnitSetProperty, AudioUnitUninitialize,
    kAudioConverterPrimeInfo, kAudioConverterPrimeMethod, kAudioOutputUnitProperty_CurrentDevice,
    kAudioOutputUnitProperty_EnableIO, kAudioOutputUnitProperty_SetInputCallback,
    kAudioUnitManufacturer_Apple, kAudioUnitProperty_MaximumFramesPerSlice,
    kAudioUnitProperty_SetRenderCallback, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global,
    kAudioUnitScope_Input, kAudioUnitScope_Output, kAudioUnitSubType_HALOutput,
    kAudioUnitType_Output, kConverterPrimeMethod_None,
};
use objc2_core_audio::{
    AudioObjectAddPropertyListenerBlock, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectPropertyScope, AudioObjectPropertySelector, AudioObjectRemovePropertyListenerBlock,
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyBufferFrameSizeRange,
    kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyDeviceUID, kAudioDevicePropertyLatency,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioDevicePropertyStreams, kAudioDevicePropertyTransportType,
    kAudioDeviceTransportTypeBuiltIn, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioObjectUnknown, kAudioStreamPropertyLatency,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioStreamPacketDescription,
    AudioTimeStamp, AudioTimeStampFlags, AudioValueRange, kAudioFormatFlagIsFloat,
    kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
};
use objc2_core_foundation::{CFRetained, CFString};
use parking_lot::Mutex;

use crate::apple::permissions::{
    AvFoundationCaptureDeviceAuthorizationAuthority, CaptureDeviceAuthorizationAtOpen,
    CaptureDeviceAuthorizationAuthority, CaptureDeviceRefusal, PrivacyGatedCaptureDevice,
    authorize_the_capture_device_without_waiting_for_the_user, capture_device_refusal_for_the_user,
};
use crate::core::context::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioCaptureStream,
    AudioDeviceBackend, AudioDeviceStreamRequest, AudioPlaybackStream, AudioSampleFormat,
    AudioStreamFormat, CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff,
    DeviceBackendArmUnavailableReason, DeviceStreamFailureReason, DeviceStreamFailureRecorder,
    DeviceStreamLivenessReport,
};
use crate::core::media_clock::MediaClock;
use crate::core::{Error, Result};

/// `noErr`.
const NO_ERR: i32 = 0;

/// The AUHAL element that carries the device's input.
const AUHAL_INPUT_ELEMENT: u32 = 1;

/// The AUHAL element that carries the device's output.
const AUHAL_OUTPUT_ELEMENT: u32 = 0;

const NO_AUHAL_OUTPUT_UNIT: &str = "CoreAudio offers no AUHAL output unit";

/// Input cycles in a row whose render may fail before the device is taken to
/// have stopped delivering — the ALSA arm's patience with a silent device.
const CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS: u32 = 25;

/// Audio over CoreAudio, one AUHAL unit per stream.
pub struct CoreAudioAudioDeviceBackend;

impl CoreAudioAudioDeviceBackend {
    /// Confirm this Mac has the AUHAL unit and an audio device in either
    /// direction, or say why this arm cannot serve so the chain can demote.
    pub fn find_a_device() -> std::result::Result<Self, DeviceBackendArmUnavailableReason> {
        if hal_output_component().is_none() {
            return Err(DeviceBackendArmUnavailableReason::of(NO_AUHAL_OUTPUT_UNIT));
        }
        let has_a_default_device = [
            CoreAudioStreamDirection::Capture,
            CoreAudioStreamDirection::Playback,
        ]
        .into_iter()
        .any(|direction| default_device_object_id(direction).is_some());
        if !has_a_default_device {
            return Err(DeviceBackendArmUnavailableReason::of(
                "CoreAudio lists no default input or output device",
            ));
        }
        Ok(Self)
    }
}

impl AudioDeviceBackend for CoreAudioAudioDeviceBackend {
    fn backend_name(&self) -> &'static str {
        "coreaudio"
    }

    fn open_capture_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioCaptureStream>> {
        Ok(Box::new(CoreAudioCaptureStream::open(
            request,
            &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Microphone),
        )?))
    }

    fn open_playback_stream(
        &self,
        request: &AudioDeviceStreamRequest,
    ) -> Result<Box<dyn AudioPlaybackStream>> {
        Ok(Box::new(CoreAudioPlaybackStream::open(request)?))
    }
}

/// Which way a stream moves samples, and every CoreAudio constant that follows
/// from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreAudioStreamDirection {
    /// Samples from an input device.
    Capture,
    /// Samples to an output device.
    Playback,
}

impl CoreAudioStreamDirection {
    fn device_property_scope(self) -> AudioObjectPropertyScope {
        match self {
            CoreAudioStreamDirection::Capture => kAudioObjectPropertyScopeInput,
            CoreAudioStreamDirection::Playback => kAudioObjectPropertyScopeOutput,
        }
    }

    fn default_device_selector(self) -> AudioObjectPropertySelector {
        match self {
            CoreAudioStreamDirection::Capture => kAudioHardwarePropertyDefaultInputDevice,
            CoreAudioStreamDirection::Playback => kAudioHardwarePropertyDefaultOutputDevice,
        }
    }

    /// The AUHAL element whose device side this direction uses.
    fn auhal_element(self) -> u32 {
        match self {
            CoreAudioStreamDirection::Capture => AUHAL_INPUT_ELEMENT,
            CoreAudioStreamDirection::Playback => AUHAL_OUTPUT_ELEMENT,
        }
    }

    /// The AUHAL scope the client's format is set on: the side of the element
    /// facing this process rather than the device.
    fn auhal_client_side_scope(self) -> u32 {
        match self {
            CoreAudioStreamDirection::Capture => kAudioUnitScope_Output,
            CoreAudioStreamDirection::Playback => kAudioUnitScope_Input,
        }
    }

    fn present_participle(self) -> &'static str {
        match self {
            CoreAudioStreamDirection::Capture => "capturing",
            CoreAudioStreamDirection::Playback => "playing",
        }
    }

    fn lowercase_direction_name(self) -> &'static str {
        match self {
            CoreAudioStreamDirection::Capture => "capture",
            CoreAudioStreamDirection::Playback => "playback",
        }
    }
}

/// One CoreAudio device as the backend enumerates it.
#[derive(Debug, Clone)]
struct CoreAudioDevice {
    object_id: AudioObjectID,
    /// The persistent identifier a caller names the device by.
    uid: String,
    name: String,
}

impl std::fmt::Display for CoreAudioDevice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "'{}' ({})", self.uid, self.name)
    }
}

impl CoreAudioDevice {
    /// The error a CoreAudio call on this device failed with, naming the device,
    /// what it would not do, and the status it answered.
    fn refused_with_status(&self, what_the_device_would_not_do: &str, status: i32) -> Error {
        Error::Configuration(format!(
            "audio device {self} {what_the_device_would_not_do}: {}",
            osstatus_text(status)
        ))
    }

    /// "refused binding the unit for capture", and the like, with the status.
    fn refused_for(
        &self,
        direction: CoreAudioStreamDirection,
        what_the_device_refused: &str,
        status: i32,
    ) -> Error {
        self.refused_with_status(
            &format!(
                "refused {what_the_device_refused} for {}",
                direction.lowercase_direction_name()
            ),
            status,
        )
    }
}

fn property_address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// A fixed-size property value.
fn audio_object_property<T: Copy + Default>(
    object_id: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Option<T> {
    let address = property_address(selector, scope);
    let mut value = T::default();
    let mut value_byte_count = std::mem::size_of::<T>() as u32;
    // SAFETY: `value` is a writable `T` and the size passed is its own.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut value_byte_count),
            NonNull::from(&mut value).cast(),
        )
    };
    (status == NO_ERR && value_byte_count as usize == std::mem::size_of::<T>()).then_some(value)
}

/// A variable-size property value, as the bytes CoreAudio wrote, in a buffer
/// aligned for the structs such properties hold.
fn audio_object_property_words(
    object_id: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Option<(Vec<u64>, usize)> {
    let address = property_address(selector, scope);
    let mut byte_count = 0u32;
    // SAFETY: `byte_count` is a writable `u32`.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object_id,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut byte_count),
        )
    };
    if status != NO_ERR {
        return None;
    }
    let mut words = vec![0u64; (byte_count as usize).div_ceil(8).max(1)];
    // SAFETY: `words` holds at least `byte_count` writable bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object_id,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut byte_count),
            NonNull::from(words.as_mut_slice()).cast(),
        )
    };
    (status == NO_ERR).then_some((words, byte_count as usize))
}

/// A list of `AudioObjectID`s — devices, or a device's streams.
fn audio_object_id_list_property(
    object_id: AudioObjectID,
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> Vec<AudioObjectID> {
    let Some((words, byte_count)) = audio_object_property_words(object_id, selector, scope) else {
        return Vec::new();
    };
    // SAFETY: CoreAudio wrote `byte_count` bytes of packed `AudioObjectID`s.
    let ids = unsafe {
        std::slice::from_raw_parts(
            words.as_ptr().cast::<AudioObjectID>(),
            byte_count / std::mem::size_of::<AudioObjectID>(),
        )
    };
    ids.to_vec()
}

/// A `CFStringRef` property, which CoreAudio hands over retained.
fn audio_object_string_property(
    object_id: AudioObjectID,
    selector: AudioObjectPropertySelector,
) -> Option<String> {
    let string = audio_object_property::<Option<NonNull<CFString>>>(
        object_id,
        selector,
        kAudioObjectPropertyScopeGlobal,
    )??;
    // SAFETY: the property's contract is a +1 `CFStringRef` the caller releases.
    let string = unsafe { CFRetained::from_raw(string) };
    Some(string.to_string())
}

fn default_device_object_id(direction: CoreAudioStreamDirection) -> Option<AudioObjectID> {
    audio_object_property::<AudioObjectID>(
        kAudioObjectSystemObject as AudioObjectID,
        direction.default_device_selector(),
        kAudioObjectPropertyScopeGlobal,
    )
    .filter(|&object_id| object_id != kAudioObjectUnknown)
}

/// Channels a device carries in one direction, summed over its streams.
fn channel_count_of(object_id: AudioObjectID, direction: CoreAudioStreamDirection) -> u32 {
    let Some((words, byte_count)) = audio_object_property_words(
        object_id,
        kAudioDevicePropertyStreamConfiguration,
        direction.device_property_scope(),
    ) else {
        return 0;
    };
    if byte_count < std::mem::size_of::<u32>() {
        return 0;
    }
    let buffer_list = words.as_ptr().cast::<AudioBufferList>();
    // SAFETY: CoreAudio wrote an `AudioBufferList` of `byte_count` bytes: a
    // count followed by that many `AudioBuffer`s, read no further than written.
    unsafe {
        let buffer_count = (*buffer_list).mNumberBuffers as usize;
        let buffers = std::ptr::addr_of!((*buffer_list).mBuffers).cast::<AudioBuffer>();
        let buffers_that_fit = byte_count
            .saturating_sub(std::mem::offset_of!(AudioBufferList, mBuffers))
            / std::mem::size_of::<AudioBuffer>();
        std::slice::from_raw_parts(buffers, buffer_count.min(buffers_that_fit))
            .iter()
            .map(|buffer| buffer.mNumberChannels)
            .sum()
    }
}

fn describe_device(object_id: AudioObjectID) -> CoreAudioDevice {
    CoreAudioDevice {
        object_id,
        uid: audio_object_string_property(object_id, kAudioDevicePropertyDeviceUID)
            .unwrap_or_default(),
        name: audio_object_string_property(object_id, kAudioObjectPropertyName).unwrap_or_default(),
    }
}

/// Every device carrying at least one channel in `direction`.
fn devices_carrying(direction: CoreAudioStreamDirection) -> Vec<CoreAudioDevice> {
    audio_object_id_list_property(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    )
    .into_iter()
    .filter(|&object_id| channel_count_of(object_id, direction) > 0)
    .map(describe_device)
    .collect()
}

/// The UID of the system default device in `direction`, if one is set.
pub fn default_audio_device_uid(direction: CoreAudioStreamDirection) -> Option<String> {
    default_device_object_id(direction)
        .map(describe_device)
        .map(|device| device.uid)
        .filter(|uid| !uid.is_empty())
}

/// The UID of this Mac's own built-in device in `direction` — transport
/// `kAudioDeviceTransportTypeBuiltIn` — if one carries that direction.
pub fn built_in_audio_device_uid(direction: CoreAudioStreamDirection) -> Option<String> {
    devices_carrying(direction)
        .into_iter()
        .find(|device| {
            audio_object_property::<u32>(
                device.object_id,
                kAudioDevicePropertyTransportType,
                kAudioObjectPropertyScopeGlobal,
            ) == Some(kAudioDeviceTransportTypeBuiltIn)
        })
        .map(|device| device.uid)
        .filter(|uid| !uid.is_empty())
}

/// The device a request names, or the system default in `direction`. A named
/// device that is not attached is refused naming it.
fn resolve_requested_device(
    request: &AudioDeviceStreamRequest,
    direction: CoreAudioStreamDirection,
) -> Result<CoreAudioDevice> {
    let Some(device_id) = request.device_id.as_deref() else {
        return default_device_object_id(direction)
            .map(describe_device)
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "no default audio {} device is set on this Mac. Choose one in System \
                     Settings › Sound, or name one by device_id.",
                    direction.lowercase_direction_name()
                ))
            });
    };
    let attached = devices_carrying(direction);
    attached
        .iter()
        .find(|device| device.uid == device_id)
        .cloned()
        .ok_or_else(|| {
            Error::Configuration(refusal_for_a_named_audio_device_that_is_not_attached(
                device_id, direction, &attached,
            ))
        })
}

fn refusal_for_a_named_audio_device_that_is_not_attached(
    device_id: &str,
    direction: CoreAudioStreamDirection,
    attached: &[CoreAudioDevice],
) -> String {
    let noun = direction.lowercase_direction_name();
    if attached.is_empty() {
        return format!(
            "audio device '{device_id}' cannot be opened for {noun}: no device on this Mac \
             carries {noun} channels."
        );
    }
    let attached = attached
        .iter()
        .map(|device| format!("{} ({})", device.uid, device.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "audio device '{device_id}' cannot be opened for {noun}: no attached device has that \
         CoreAudio UID. Devices carrying {noun} channels: {attached}. Fix device_id, or omit it \
         to use the system default."
    )
}

/// A device's own format in `direction`: its nominal rate and channel count, as
/// interleaved 32-bit floats. A stream opens at it, so on the device it opened
/// on nothing converts.
fn stream_format_of(
    device: &CoreAudioDevice,
    direction: CoreAudioStreamDirection,
) -> Result<AudioStreamFormat> {
    let channels = channel_count_of(device.object_id, direction);
    if channels == 0 {
        return Err(Error::Configuration(format!(
            "audio device {device} carries no {} channels.",
            direction.lowercase_direction_name()
        )));
    }
    let nominal_sample_rate = audio_object_property::<f64>(
        device.object_id,
        kAudioDevicePropertyNominalSampleRate,
        kAudioObjectPropertyScopeGlobal,
    )
    .filter(|&rate| rate >= 1.0)
    .ok_or_else(|| {
        Error::Configuration(format!("audio device {device} reports no sample rate."))
    })?;
    Ok(AudioStreamFormat {
        sample_rate: nominal_sample_rate.round() as u32,
        channels,
        sample_format: AudioSampleFormat::F32,
    })
}

fn interleaved_f32_description_of(stream_format: AudioStreamFormat) -> AudioStreamBasicDescription {
    let bytes_per_frame = stream_format.interleaved_byte_count_for(1) as u32;
    AudioStreamBasicDescription {
        mSampleRate: f64::from(stream_format.sample_rate),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: stream_format.channels,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

/// Frames between a sample reaching the device and the timestamp its input
/// cycle carries: the device's latency plus its first input stream's.
fn capture_latency_in_frames_of(device: &CoreAudioDevice) -> u32 {
    let scope = CoreAudioStreamDirection::Capture.device_property_scope();
    let device_latency =
        audio_object_property::<u32>(device.object_id, kAudioDevicePropertyLatency, scope)
            .unwrap_or(0);
    let stream_latency =
        audio_object_id_list_property(device.object_id, kAudioDevicePropertyStreams, scope)
            .first()
            .and_then(|&stream_id| {
                audio_object_property::<u32>(
                    stream_id,
                    kAudioStreamPropertyLatency,
                    kAudioObjectPropertyScopeGlobal,
                )
            })
            .unwrap_or(0);
    device_latency.saturating_add(stream_latency)
}

/// An `AudioValueRange` read as the `[minimum, maximum]` pair it is laid out
/// as, since the generated struct has no `Default`.
type AudioValueRangeAsMinimumAndMaximum = [f64; 2];

const _: () = assert!(
    std::mem::size_of::<AudioValueRange>()
        == std::mem::size_of::<AudioValueRangeAsMinimumAndMaximum>()
        && std::mem::align_of::<AudioValueRange>()
            == std::mem::align_of::<AudioValueRangeAsMinimumAndMaximum>()
);

/// The most frames one I/O cycle of `device` can carry in `direction`: the top
/// of its buffer-size range, or its current buffer size where the range is
/// not reported.
fn largest_io_cycle_in_frames_of(
    device: &CoreAudioDevice,
    direction: CoreAudioStreamDirection,
) -> Option<u32> {
    let scope = direction.device_property_scope();
    let top_of_the_buffer_size_range = audio_object_property::<AudioValueRangeAsMinimumAndMaximum>(
        device.object_id,
        kAudioDevicePropertyBufferFrameSizeRange,
        scope,
    )
    .map(|[_minimum, maximum]| maximum.ceil() as u32);
    let current_buffer_size =
        audio_object_property::<u32>(device.object_id, kAudioDevicePropertyBufferFrameSize, scope);
    top_of_the_buffer_size_range
        .into_iter()
        .chain(current_buffer_size)
        .max()
        .filter(|&frames| frames > 0)
}

/// Nanoseconds `frame_count` frames occupy at `sample_rate`.
fn duration_of_frames_in_ns(frame_count: u32, sample_rate: u32) -> i64 {
    i64::from(frame_count) * 1_000_000_000 / i64::from(sample_rate.max(1))
}

/// The monotonic instant of a captured block's first sample: its input
/// cycle's host time, moved back by the frames the device took to deliver it.
fn first_sample_timestamp_ns(
    input_cycle_host_time_ns: i64,
    capture_latency_in_frames: u32,
    sample_rate: u32,
) -> i64 {
    input_cycle_host_time_ns - duration_of_frames_in_ns(capture_latency_in_frames, sample_rate)
}

fn osstatus_text(status: i32) -> String {
    let four_char_code = status.to_be_bytes();
    if four_char_code.iter().all(|byte| byte.is_ascii_graphic()) {
        format!(
            "OSStatus {status} ('{}')",
            String::from_utf8_lossy(&four_char_code)
        )
    } else {
        format!("OSStatus {status}")
    }
}

fn osstatus_as_result(status: i32) -> std::result::Result<(), i32> {
    if status == NO_ERR {
        Ok(())
    } else {
        Err(status)
    }
}

/// "48000 Hz, 2 ch" — a format as a refusal or a rebind line names it.
fn rate_and_channels_of(format: AudioStreamFormat) -> String {
    format!("{} Hz, {} ch", format.sample_rate, format.channels)
}

fn hal_output_component() -> Option<objc2_audio_toolbox::AudioComponent> {
    let description = AudioComponentDescription {
        componentType: kAudioUnitType_Output,
        componentSubType: kAudioUnitSubType_HALOutput,
        componentManufacturer: kAudioUnitManufacturer_Apple,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    // SAFETY: a lookup over a valid description, from the start of the list.
    let component =
        unsafe { AudioComponentFindNext(std::ptr::null_mut(), NonNull::from(&description)) };
    (!component.is_null()).then_some(component)
}

/// An AUHAL unit with I/O enabled in one direction. Dropping it stops,
/// uninitialises and disposes the unit, after which no callback runs.
struct CoreAudioHalOutputUnit {
    audio_unit: AudioUnit,
    is_running: bool,
}

// SAFETY: an AudioUnit handle may be driven from any thread; every call on it
// here is made through `&mut self` or under the owning stream's lock.
unsafe impl Send for CoreAudioHalOutputUnit {}

impl CoreAudioHalOutputUnit {
    /// A new AUHAL instance with I/O enabled in `direction` only and no device
    /// bound yet; `device` is the one a refusal names.
    fn new_enabled_for(
        device: &CoreAudioDevice,
        direction: CoreAudioStreamDirection,
    ) -> Result<Self> {
        let component = hal_output_component()
            .ok_or_else(|| Error::Configuration(NO_AUHAL_OUTPUT_UNIT.into()))?;
        let mut audio_unit: AudioUnit = std::ptr::null_mut();
        // SAFETY: `component` is a live component and `audio_unit` a writable slot.
        let status =
            unsafe { AudioComponentInstanceNew(component, NonNull::from(&mut audio_unit)) };
        if status != NO_ERR || audio_unit.is_null() {
            return Err(device.refused_with_status("could not get an AUHAL unit", status));
        }
        let unit = Self {
            audio_unit,
            is_running: false,
        };
        let enable_capture = u32::from(direction == CoreAudioStreamDirection::Capture);
        let enable_playback = u32::from(direction == CoreAudioStreamDirection::Playback);
        unit.set_property(
            kAudioOutputUnitProperty_EnableIO,
            kAudioUnitScope_Input,
            AUHAL_INPUT_ELEMENT,
            &enable_capture,
        )
        .map_err(|status| device.refused_for(direction, "enabling input", status))?;
        unit.set_property(
            kAudioOutputUnitProperty_EnableIO,
            kAudioUnitScope_Output,
            AUHAL_OUTPUT_ELEMENT,
            &enable_playback,
        )
        .map_err(|status| device.refused_for(direction, "enabling output", status))?;
        Ok(unit)
    }

    /// Point this uninitialised unit at `device` with its client side in
    /// `unit_client_side_format`, and return the longest cycle, in frames, it
    /// will run there.
    fn bind_to(
        &self,
        device: &CoreAudioDevice,
        direction: CoreAudioStreamDirection,
        unit_client_side_format: AudioStreamFormat,
    ) -> Result<u32> {
        self.set_property(
            kAudioOutputUnitProperty_CurrentDevice,
            kAudioUnitScope_Global,
            AUHAL_OUTPUT_ELEMENT,
            &device.object_id,
        )
        .map_err(|status| device.refused_for(direction, "binding the unit", status))?;
        self.set_property(
            kAudioUnitProperty_StreamFormat,
            direction.auhal_client_side_scope(),
            direction.auhal_element(),
            &interleaved_f32_description_of(unit_client_side_format),
        )
        .map_err(|status| {
            device.refused_for(
                direction,
                &format!(
                    "{} as interleaved float",
                    rate_and_channels_of(unit_client_side_format)
                ),
                status,
            )
        })?;
        // Raised to the device's own ceiling before initialising, because a
        // device whose buffer size another process raises past the unit's
        // default would otherwise fail every render.
        let Some(largest_cycle_in_frames) = largest_io_cycle_in_frames_of(device, direction) else {
            return Ok(self
                .global_u32_property(kAudioUnitProperty_MaximumFramesPerSlice)
                .unwrap_or(0));
        };
        self.set_property(
            kAudioUnitProperty_MaximumFramesPerSlice,
            kAudioUnitScope_Global,
            AUHAL_OUTPUT_ELEMENT,
            &largest_cycle_in_frames,
        )
        .map_err(|status| {
            device.refused_for(
                direction,
                &format!("a largest cycle of {largest_cycle_in_frames} frames"),
                status,
            )
        })?;
        Ok(largest_cycle_in_frames)
    }

    fn initialise(&self) -> std::result::Result<(), i32> {
        // SAFETY: a configured unit, whose callback context outlives it.
        osstatus_as_result(unsafe { AudioUnitInitialize(self.audio_unit) })
    }

    fn uninitialise(&self) -> std::result::Result<(), i32> {
        // SAFETY: a live, stopped unit.
        osstatus_as_result(unsafe { AudioUnitUninitialize(self.audio_unit) })
    }

    fn set_property<T>(
        &self,
        property_id: u32,
        scope: u32,
        element: u32,
        value: &T,
    ) -> std::result::Result<(), i32> {
        // SAFETY: `value` is a readable `T` and the size passed is its own.
        let status = unsafe {
            AudioUnitSetProperty(
                self.audio_unit,
                property_id,
                scope,
                element,
                (value as *const T).cast(),
                std::mem::size_of::<T>() as u32,
            )
        };
        if status == NO_ERR {
            Ok(())
        } else {
            Err(status)
        }
    }

    fn global_u32_property(&self, property_id: u32) -> Option<u32> {
        let mut value = 0u32;
        let mut value_byte_count = std::mem::size_of::<u32>() as u32;
        // SAFETY: `value` is a writable `u32` and the size passed is its own.
        let status = unsafe {
            AudioUnitGetProperty(
                self.audio_unit,
                property_id,
                kAudioUnitScope_Global,
                0,
                NonNull::from(&mut value).cast(),
                NonNull::from(&mut value_byte_count),
            )
        };
        (status == NO_ERR).then_some(value)
    }

    fn start(&mut self) -> std::result::Result<(), i32> {
        if self.is_running {
            return Ok(());
        }
        // SAFETY: an initialised unit.
        let status = unsafe { AudioOutputUnitStart(self.audio_unit) };
        if status != NO_ERR {
            return Err(status);
        }
        self.is_running = true;
        Ok(())
    }

    fn stop(&mut self) -> std::result::Result<(), i32> {
        if !self.is_running {
            return Ok(());
        }
        self.is_running = false;
        // SAFETY: an initialised, running unit.
        let status = unsafe { AudioOutputUnitStop(self.audio_unit) };
        if status == NO_ERR {
            Ok(())
        } else {
            Err(status)
        }
    }
}

impl Drop for CoreAudioHalOutputUnit {
    fn drop(&mut self) {
        let _ = self.stop();
        // SAFETY: the unit is stopped; uninitialising an uninitialised unit and
        // disposing are both valid on a live instance, which this is until here.
        unsafe {
            AudioUnitUninitialize(self.audio_unit);
            AudioComponentInstanceDispose(self.audio_unit);
        }
    }
}

/// How a stream's fixed format meets the format of the device it is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreAudioStreamFormatBridge {
    /// The device carries the stream's rate and channel count; nothing converts.
    TheDeviceCarriesTheStreamFormat,
    /// Playback on a device of another format: AUHAL's output side converts
    /// the stream's format, which stays the unit's client format, to the
    /// device's.
    AuhalConvertsTheStreamFormatForTheDevice,
    /// Capture on a device of another format: AUHAL input cannot resample, so
    /// the unit renders the device's own format and an `AudioConverter` in the
    /// input callback takes it to the stream's.
    AnAudioConverterTakesTheDevicesFormatToTheStreams,
}

impl CoreAudioStreamFormatBridge {
    fn between(
        direction: CoreAudioStreamDirection,
        stream_format: AudioStreamFormat,
        device_own_format: AudioStreamFormat,
    ) -> Self {
        let the_device_carries_the_stream_format = stream_format.sample_rate
            == device_own_format.sample_rate
            && stream_format.channels == device_own_format.channels;
        match direction {
            _ if the_device_carries_the_stream_format => Self::TheDeviceCarriesTheStreamFormat,
            CoreAudioStreamDirection::Playback => Self::AuhalConvertsTheStreamFormatForTheDevice,
            CoreAudioStreamDirection::Capture => {
                Self::AnAudioConverterTakesTheDevicesFormatToTheStreams
            }
        }
    }

    /// The format the unit's client side is set to.
    fn unit_client_side_format(
        self,
        stream_format: AudioStreamFormat,
        device_own_format: AudioStreamFormat,
    ) -> AudioStreamFormat {
        match self {
            Self::TheDeviceCarriesTheStreamFormat
            | Self::AuhalConvertsTheStreamFormatForTheDevice => stream_format,
            Self::AnAudioConverterTakesTheDevicesFormatToTheStreams => device_own_format,
        }
    }
}

impl std::fmt::Display for CoreAudioStreamFormatBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::TheDeviceCarriesTheStreamFormat => "no conversion",
            Self::AuhalConvertsTheStreamFormatForTheDevice => {
                "converted by AUHAL's output converter"
            }
            Self::AnAudioConverterTakesTheDevicesFormatToTheStreams => {
                "converted by an AudioConverter in the input callback"
            }
        })
    }
}

/// A change CoreAudio reports about the device a stream is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreAudioStreamDeviceChange {
    /// The system default device in the stream's direction changed.
    SystemDefaultDeviceMoved,
    /// The bound device's `DeviceIsAlive` changed.
    BoundDeviceLivenessChanged,
    /// The bound device's nominal rate or stream configuration changed.
    BoundDeviceFormatChanged,
}

impl CoreAudioStreamDeviceChange {
    fn what_goes_unnoticed_without_its_listener(self) -> &'static str {
        match self {
            Self::SystemDefaultDeviceMoved => {
                "the stream stays on this device when the system default changes"
            }
            Self::BoundDeviceLivenessChanged => {
                "a device that disappears will go silent without being reported"
            }
            Self::BoundDeviceFormatChanged => {
                "a change to the device's rate or channels will not be followed"
            }
        }
    }
}

/// What CoreAudio reports once a change reaches the stream's control queue.
#[derive(Debug, Clone, Copy)]
struct CoreAudioDeviceFactsAfterAChange {
    the_bound_device_is_alive: bool,
    /// Its rate or channel count is no longer the one the stream bound at.
    the_bound_devices_own_format_changed: bool,
    system_default_device: Option<AudioObjectID>,
}

/// What a stream does about a change to its device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreAudioStreamDeviceChangeResponse {
    StayOnTheBoundDevice,
    RebindToTheBoundDeviceAtItsNewFormat,
    MoveToTheSystemDefault(AudioObjectID),
    /// A named device went away: its stream ends rather than landing on
    /// another.
    EndTheStreamBecauseTheNamedDeviceWentAway,
    /// A followed default went away and no device became the default.
    EndTheStreamBecauseNoDeviceIsTheDefault,
}

/// Whether a stream stays on the device it named or follows the system
/// default as it moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreAudioStreamDevicePolicy {
    PinnedToTheNamedDevice,
    FollowsTheSystemDefault,
}

impl CoreAudioStreamDevicePolicy {
    /// A request naming a device pins the stream to it; one naming none follows
    /// the default.
    fn of(request: &AudioDeviceStreamRequest) -> Self {
        match request.device_id {
            Some(_) => Self::PinnedToTheNamedDevice,
            None => Self::FollowsTheSystemDefault,
        }
    }
}

/// A named stream stays pinned to its device; one opened with no `device_id`
/// follows the system default. Either rebinds when its device's own format
/// changes.
fn how_a_stream_responds_to_a_device_change(
    change: CoreAudioStreamDeviceChange,
    device_policy: CoreAudioStreamDevicePolicy,
    bound_device_object_id: AudioObjectID,
    facts: CoreAudioDeviceFactsAfterAChange,
) -> CoreAudioStreamDeviceChangeResponse {
    use CoreAudioStreamDeviceChange as Change;
    use CoreAudioStreamDeviceChangeResponse as Response;
    let toward_the_system_default = || match facts.system_default_device {
        None => Response::EndTheStreamBecauseNoDeviceIsTheDefault,
        // Already there — or the default has yet to move off a device that went
        // away, and that move arrives as its own change.
        Some(default_device) if default_device == bound_device_object_id => {
            Response::StayOnTheBoundDevice
        }
        Some(default_device) => Response::MoveToTheSystemDefault(default_device),
    };
    match change {
        Change::SystemDefaultDeviceMoved
            if device_policy == CoreAudioStreamDevicePolicy::FollowsTheSystemDefault =>
        {
            toward_the_system_default()
        }
        Change::SystemDefaultDeviceMoved => Response::StayOnTheBoundDevice,
        Change::BoundDeviceLivenessChanged if facts.the_bound_device_is_alive => {
            Response::StayOnTheBoundDevice
        }
        Change::BoundDeviceLivenessChanged
            if device_policy == CoreAudioStreamDevicePolicy::FollowsTheSystemDefault =>
        {
            toward_the_system_default()
        }
        Change::BoundDeviceLivenessChanged => Response::EndTheStreamBecauseTheNamedDeviceWentAway,
        Change::BoundDeviceFormatChanged
            if facts.the_bound_device_is_alive && facts.the_bound_devices_own_format_changed =>
        {
            Response::RebindToTheBoundDeviceAtItsNewFormat
        }
        Change::BoundDeviceFormatChanged => Response::StayOnTheBoundDevice,
    }
}

/// Runs a device change on the stream's control queue.
type CoreAudioStreamDeviceChangeHandler = Arc<dyn Fn(CoreAudioStreamDeviceChange) + Send + Sync>;

/// The block a stream's property listener runs, on the stream's control queue.
type CoreAudioStreamDevicePropertyListenerBlock =
    block2::RcBlock<dyn Fn(u32, NonNull<AudioObjectPropertyAddress>)>;

/// One CoreAudio property listener whose block the HAL runs on the stream's
/// control queue. Dropping it unregisters it.
///
/// A block rather than a function and a raw context: the HAL retains the block
/// for every invocation it has queued, so an unregistration racing a change
/// already in flight can never free what the invocation reads.
struct CoreAudioStreamDevicePropertyListener {
    object_id: AudioObjectID,
    address: AudioObjectPropertyAddress,
    stream_control_queue: DispatchRetained<DispatchQueue>,
    listener_block: CoreAudioStreamDevicePropertyListenerBlock,
}

// SAFETY: the block's captures are a `Copy` change and an `Arc` of a
// `Send + Sync` handler, and Objective-C block retain and release are
// thread-safe, so the listener may be dropped from any thread.
unsafe impl Send for CoreAudioStreamDevicePropertyListener {}

impl CoreAudioStreamDevicePropertyListener {
    /// Listen for `address` on `object_id`, or warn — naming `device` — what
    /// goes unnoticed when CoreAudio refuses.
    fn listen(
        object_id: AudioObjectID,
        address: AudioObjectPropertyAddress,
        change: CoreAudioStreamDeviceChange,
        stream_control_queue: DispatchRetained<DispatchQueue>,
        handle_the_change: CoreAudioStreamDeviceChangeHandler,
        device: &CoreAudioDevice,
    ) -> Option<Self> {
        let listener_block: CoreAudioStreamDevicePropertyListenerBlock =
            block2::RcBlock::new(move |_address_count: u32, _addresses| handle_the_change(change));
        // SAFETY: `address` is read for the call; the block is retained by the
        // HAL and by the returned listener, which unregisters it in `Drop` with
        // the same queue and block.
        let status = unsafe {
            AudioObjectAddPropertyListenerBlock(
                object_id,
                NonNull::from(&address),
                Some(&stream_control_queue),
                block2::RcBlock::as_ptr(&listener_block),
            )
        };
        if status != NO_ERR {
            tracing::warn!(
                device = %device,
                status = %osstatus_text(status),
                "CoreAudio audio arm: could not listen for a device change; {}",
                change.what_goes_unnoticed_without_its_listener()
            );
            return None;
        }
        Some(Self {
            object_id,
            address,
            stream_control_queue,
            listener_block,
        })
    }
}

impl Drop for CoreAudioStreamDevicePropertyListener {
    fn drop(&mut self) {
        // SAFETY: the same object, address, queue and block `listen` registered.
        unsafe {
            AudioObjectRemovePropertyListenerBlock(
                self.object_id,
                NonNull::from(&self.address),
                Some(&self.stream_control_queue),
                block2::RcBlock::as_ptr(&self.listener_block),
            );
        }
    }
}

/// The device a stream is bound to, whether it follows the system default,
/// and the listeners that report a change to either.
struct CoreAudioStreamDeviceBinding {
    direction: CoreAudioStreamDirection,
    device: CoreAudioDevice,
    /// The device's own rate and channel count when the stream last bound to it.
    device_own_format: AudioStreamFormat,
    device_policy: CoreAudioStreamDevicePolicy,
    failure_recorder: DeviceStreamFailureRecorder,
    liveness_report: DeviceStreamLivenessReport,
    /// Serial and the stream's own, so changes are handled one at a time.
    stream_control_queue: DispatchRetained<DispatchQueue>,
    handle_a_device_change: CoreAudioStreamDeviceChangeHandler,
    /// `DeviceIsAlive`, nominal rate and stream configuration, on the bound
    /// device.
    bound_device_listeners: Vec<CoreAudioStreamDevicePropertyListener>,
    /// On the system object, for a stream that follows the default.
    system_default_device_listener: Option<CoreAudioStreamDevicePropertyListener>,
}

impl CoreAudioStreamDeviceBinding {
    fn new(
        direction: CoreAudioStreamDirection,
        device: CoreAudioDevice,
        device_own_format: AudioStreamFormat,
        device_policy: CoreAudioStreamDevicePolicy,
        failure_recorder: DeviceStreamFailureRecorder,
        liveness_report: DeviceStreamLivenessReport,
        handle_a_device_change: CoreAudioStreamDeviceChangeHandler,
    ) -> Self {
        Self {
            direction,
            device,
            device_own_format,
            device_policy,
            failure_recorder,
            liveness_report,
            stream_control_queue: DispatchQueue::new(
                &format!(
                    "com.streamlib.coreaudio-{}-stream-control",
                    direction.lowercase_direction_name()
                ),
                None,
            ),
            handle_a_device_change,
            bound_device_listeners: Vec::new(),
            system_default_device_listener: None,
        }
    }

    /// Register every listener the stream follows its device by.
    fn start_listening(&mut self) {
        if self.device_policy == CoreAudioStreamDevicePolicy::FollowsTheSystemDefault {
            self.system_default_device_listener = self.listen(
                kAudioObjectSystemObject as AudioObjectID,
                property_address(
                    self.direction.default_device_selector(),
                    kAudioObjectPropertyScopeGlobal,
                ),
                CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
            );
        }
        self.listen_to_the_bound_device();
    }

    fn listen_to_the_bound_device(&mut self) {
        self.bound_device_listeners.clear();
        let bound_device_object_id = self.device.object_id;
        let bound_device_listeners = [
            (
                kAudioDevicePropertyDeviceIsAlive,
                kAudioObjectPropertyScopeGlobal,
                CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
            ),
            (
                kAudioDevicePropertyNominalSampleRate,
                kAudioObjectPropertyScopeGlobal,
                CoreAudioStreamDeviceChange::BoundDeviceFormatChanged,
            ),
            (
                kAudioDevicePropertyStreamConfiguration,
                self.direction.device_property_scope(),
                CoreAudioStreamDeviceChange::BoundDeviceFormatChanged,
            ),
        ]
        .into_iter()
        .filter_map(|(selector, scope, change)| {
            self.listen(
                bound_device_object_id,
                property_address(selector, scope),
                change,
            )
        })
        .collect();
        self.bound_device_listeners = bound_device_listeners;
    }

    fn listen(
        &self,
        object_id: AudioObjectID,
        address: AudioObjectPropertyAddress,
        change: CoreAudioStreamDeviceChange,
    ) -> Option<CoreAudioStreamDevicePropertyListener> {
        CoreAudioStreamDevicePropertyListener::listen(
            object_id,
            address,
            change,
            self.stream_control_queue.clone(),
            Arc::clone(&self.handle_a_device_change),
            &self.device,
        )
    }

    /// Record that the stream is bound to `device`, moving the bound-device
    /// listeners there if it is another device.
    fn moved_to(&mut self, device: &CoreAudioDevice, device_own_format: AudioStreamFormat) {
        let is_another_device = device.object_id != self.device.object_id;
        self.device = device.clone();
        self.device_own_format = device_own_format;
        if is_another_device {
            self.listen_to_the_bound_device();
        }
    }

    fn has_ended(&self) -> bool {
        self.liveness_report
            .failure_that_ended_the_stream()
            .is_some()
    }

    fn record_the_failure_that_ended_the_stream(&self, reason: String) {
        tracing::error!(
            device = %self.device,
            %reason,
            "CoreAudio audio arm: the {} stream ended",
            self.direction.lowercase_direction_name()
        );
        self.failure_recorder
            .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(reason));
    }
}

/// A stream's control state as the device-following path drives it: on the
/// stream's control queue, under the stream's lock.
trait CoreAudioStreamControlThatFollowsItsDevice: Send + 'static {
    fn stream_format(&self) -> AudioStreamFormat;

    fn device_binding(&mut self) -> &mut CoreAudioStreamDeviceBinding;

    /// Rebind the stream's unit, where one is bound yet, to `device`.
    fn rebind_the_unit_to(
        &mut self,
        device: &CoreAudioDevice,
        device_own_format: AudioStreamFormat,
    ) -> Result<()>;

    /// Let go of the unit a failed rebind left unusable; a later start
    /// reports `failure`.
    fn release_the_unit_after_a_failed_rebind(&mut self, failure: String);
}

/// The handler a stream's listeners run on its control queue, reaching the
/// stream only while it exists.
fn device_changes_reach<Control: CoreAudioStreamControlThatFollowsItsDevice>(
    stream_control: Weak<Mutex<Control>>,
) -> CoreAudioStreamDeviceChangeHandler {
    Arc::new(move |change| {
        if let Some(stream_control) = stream_control.upgrade() {
            the_streams_device_changed(&mut *stream_control.lock(), change);
        }
    })
}

fn audio_device_is_alive(object_id: AudioObjectID) -> bool {
    audio_object_property::<u32>(
        object_id,
        kAudioDevicePropertyDeviceIsAlive,
        kAudioObjectPropertyScopeGlobal,
    )
    .is_some_and(|is_alive| is_alive != 0)
}

fn the_streams_device_changed<Control: CoreAudioStreamControlThatFollowsItsDevice>(
    stream_control: &mut Control,
    change: CoreAudioStreamDeviceChange,
) {
    let binding = stream_control.device_binding();
    if binding.has_ended() {
        return;
    }
    let direction = binding.direction;
    let bound_device = binding.device.clone();
    let bound_device_own_format_now = stream_format_of(&bound_device, direction).ok();
    let facts = CoreAudioDeviceFactsAfterAChange {
        the_bound_device_is_alive: audio_device_is_alive(bound_device.object_id),
        the_bound_devices_own_format_changed: bound_device_own_format_now
            .is_some_and(|format_now| format_now != binding.device_own_format),
        system_default_device: default_device_object_id(direction),
    };
    let direction_name = direction.lowercase_direction_name();
    match how_a_stream_responds_to_a_device_change(
        change,
        binding.device_policy,
        bound_device.object_id,
        facts,
    ) {
        CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice => {}
        CoreAudioStreamDeviceChangeResponse::RebindToTheBoundDeviceAtItsNewFormat => {
            if let Some(device_own_format) = bound_device_own_format_now {
                rebind_the_stream_to(stream_control, &bound_device, device_own_format);
            }
        }
        CoreAudioStreamDeviceChangeResponse::MoveToTheSystemDefault(default_object_id) => {
            let default_device = describe_device(default_object_id);
            match stream_format_of(&default_device, direction) {
                Ok(device_own_format) => {
                    rebind_the_stream_to(stream_control, &default_device, device_own_format)
                }
                Err(unreadable_format) => {
                    binding.record_the_failure_that_ended_the_stream(format!(
                        "the {direction_name} stream could not follow the system default from \
                         {bound_device} to {default_device}: {unreadable_format}"
                    ))
                }
            }
        }
        CoreAudioStreamDeviceChangeResponse::EndTheStreamBecauseTheNamedDeviceWentAway => binding
            .record_the_failure_that_ended_the_stream(format!(
                "audio device '{}' went away during {direction_name}",
                bound_device.uid
            )),
        CoreAudioStreamDeviceChangeResponse::EndTheStreamBecauseNoDeviceIsTheDefault => binding
            .record_the_failure_that_ended_the_stream(format!(
                "the default audio {direction_name} device {bound_device} went away and no \
                 device replaced it as the default"
            )),
    }
}

/// Rebind the stream to `device` and move its listeners there, or end the
/// stream naming the device and the status that refused.
fn rebind_the_stream_to<Control: CoreAudioStreamControlThatFollowsItsDevice>(
    stream_control: &mut Control,
    device: &CoreAudioDevice,
    device_own_format: AudioStreamFormat,
) {
    let stream_format = stream_control.stream_format();
    let binding = stream_control.device_binding();
    let direction = binding.direction;
    let direction_name = direction.lowercase_direction_name();
    let previous_device = binding.device.clone();
    if let Err(rebind_failure) = stream_control.rebind_the_unit_to(device, device_own_format) {
        let failure = format!(
            "rebinding the {direction_name} stream from {previous_device} to {device} failed: \
             {rebind_failure}"
        );
        stream_control
            .device_binding()
            .record_the_failure_that_ended_the_stream(failure.clone());
        stream_control.release_the_unit_after_a_failed_rebind(failure);
        return;
    }
    stream_control
        .device_binding()
        .moved_to(device, device_own_format);
    let format_bridge =
        CoreAudioStreamFormatBridge::between(direction, stream_format, device_own_format);
    let what_the_stream_did = if previous_device.object_id == device.object_id {
        format!("rebound to {device}")
    } else {
        format!("moved from {previous_device} to {device}")
    };
    tracing::info!(
        from_device = %previous_device,
        to_device = %device,
        device_sample_rate = device_own_format.sample_rate,
        device_channels = device_own_format.channels,
        stream_sample_rate = stream_format.sample_rate,
        stream_channels = stream_format.channels,
        conversion_active = format_bridge != CoreAudioStreamFormatBridge::TheDeviceCarriesTheStreamFormat,
        "CoreAudio audio arm: the {direction_name} stream {what_the_stream_did}, {format_bridge}"
    );
}

/// The per-direction state a device-thread callback works on under its lock,
/// paired with the one callback that reads it — so a unit cannot be bound
/// with a callback that would cast its context to the wrong type.
trait CoreAudioDeliveryHoldingAHandOff: Send + Sized {
    type HandOff;

    const DIRECTION: CoreAudioStreamDirection;

    /// The AUHAL property the callback is installed through.
    const CALLBACK_PROPERTY_ID: u32;

    /// The scope the callback is installed on, element 0: Global for the
    /// input callback, Input for the render callback (TN2091).
    const CALLBACK_SCOPE: u32;

    /// Reads its refcon as a `CoreAudioCallbackContext<Self>`.
    const CALLBACK: AURenderCallback;

    fn installed_hand_off(&mut self) -> &mut Option<Self::HandOff>;
}

/// What a callback reads on the device's I/O thread.
struct CoreAudioCallbackContext<Delivery> {
    audio_unit: AudioUnit,
    failure_recorder: DeviceStreamFailureRecorder,
    /// Held by the callback across its whole call, so clearing the hand-off
    /// under it guarantees no call is in flight or to come.
    delivery: Mutex<Delivery>,
}

// SAFETY: the AudioUnit handle is only rendered through on the device's I/O
// thread; everything else is `Send + Sync` given a `Send` delivery.
unsafe impl<Delivery: Send> Send for CoreAudioCallbackContext<Delivery> {}
unsafe impl<Delivery: Send> Sync for CoreAudioCallbackContext<Delivery> {}

impl<Delivery: CoreAudioDeliveryHoldingAHandOff> CoreAudioCallbackContext<Delivery> {
    /// Uninstall the hand-off and record why the stream stopped serving, on
    /// the device thread that found out — a stream whose report names a
    /// failure delivers nothing after it.
    fn end_the_stream_because(
        &self,
        installed_hand_off: &mut Option<Delivery::HandOff>,
        reason: String,
    ) {
        *installed_hand_off = None;
        tracing::error!(
            %reason,
            "CoreAudio audio arm: the {} stream ended",
            Delivery::DIRECTION.lowercase_direction_name()
        );
        self.failure_recorder
            .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(reason));
    }

    /// Unwinding into CoreAudio's I/O thread is undefined, so a hand-off that
    /// panicked ends the stream rather than crashing it.
    fn end_the_stream_because_the_hand_off_panicked(
        &self,
        installed_hand_off: &mut Option<Delivery::HandOff>,
    ) {
        self.end_the_stream_because(
            installed_hand_off,
            format!(
                "the {} hand-off panicked on the device thread and was uninstalled",
                Delivery::DIRECTION.lowercase_direction_name()
            ),
        );
    }
}

/// An AUHAL unit bound to a device, the callback context it calls with, and
/// the device it serves.
struct CoreAudioStreamUnit<Delivery: CoreAudioDeliveryHoldingAHandOff> {
    /// Declared before the context so it is dropped — and its callback
    /// retired — before the context is freed.
    hal_output_unit: CoreAudioHalOutputUnit,
    callback_context: Box<CoreAudioCallbackContext<Delivery>>,
    device: CoreAudioDevice,
    /// The unit's `MaximumFramesPerSlice`: no cycle it renders is longer.
    largest_cycle_in_frames: u32,
}

impl<Delivery: CoreAudioDeliveryHoldingAHandOff> CoreAudioStreamUnit<Delivery> {
    fn bound_to(
        device: &CoreAudioDevice,
        unit_client_side_format: AudioStreamFormat,
        delivery: Delivery,
        failure_recorder: DeviceStreamFailureRecorder,
    ) -> Result<Self> {
        let direction_name = Delivery::DIRECTION.lowercase_direction_name();
        let hal_output_unit = CoreAudioHalOutputUnit::new_enabled_for(device, Delivery::DIRECTION)?;
        let largest_cycle_in_frames =
            hal_output_unit.bind_to(device, Delivery::DIRECTION, unit_client_side_format)?;
        let callback_context = Box::new(CoreAudioCallbackContext {
            audio_unit: hal_output_unit.audio_unit,
            failure_recorder,
            delivery: Mutex::new(delivery),
        });
        let callback_context_pointer = (callback_context.as_ref()
            as *const CoreAudioCallbackContext<Delivery>)
            .cast_mut()
            .cast();
        hal_output_unit
            .set_property(
                Delivery::CALLBACK_PROPERTY_ID,
                Delivery::CALLBACK_SCOPE,
                AUHAL_OUTPUT_ELEMENT,
                &AURenderCallbackStruct {
                    inputProc: Delivery::CALLBACK,
                    inputProcRefCon: callback_context_pointer,
                },
            )
            .and_then(|()| hal_output_unit.initialise())
            .map_err(|status| {
                device.refused_with_status(
                    &format!("would not initialise for {direction_name}"),
                    status,
                )
            })?;
        Ok(Self {
            hal_output_unit,
            callback_context,
            device: device.clone(),
            largest_cycle_in_frames,
        })
    }

    /// Move the unit to `device`, or rebind it to the same one at a new
    /// format: stopped, uninitialised, rebound and reinitialised, then
    /// restarted if it was running, with the hand-off it already holds.
    /// `prepare_the_delivery` readies the callback's state, given the new
    /// longest cycle, while no cycle can run.
    fn rebind_to(
        &mut self,
        device: &CoreAudioDevice,
        unit_client_side_format: AudioStreamFormat,
        prepare_the_delivery: impl FnOnce(&mut Delivery, u32) -> Result<()>,
    ) -> Result<()> {
        let direction_name = Delivery::DIRECTION.lowercase_direction_name();
        let was_running = self.hal_output_unit.is_running;
        self.hal_output_unit
            .stop()
            .map_err(|status| self.refused_for_this_direction("stop", status))?;
        self.hal_output_unit.uninitialise().map_err(|status| {
            self.device.refused_with_status(
                &format!("would not uninitialise for {direction_name}"),
                status,
            )
        })?;
        let largest_cycle_in_frames =
            self.hal_output_unit
                .bind_to(device, Delivery::DIRECTION, unit_client_side_format)?;
        prepare_the_delivery(
            &mut self.callback_context.delivery.lock(),
            largest_cycle_in_frames,
        )?;
        self.hal_output_unit.initialise().map_err(|status| {
            device.refused_with_status(
                &format!("would not initialise for {direction_name}"),
                status,
            )
        })?;
        self.device = device.clone();
        self.largest_cycle_in_frames = largest_cycle_in_frames;
        if was_running {
            self.hal_output_unit
                .start()
                .map_err(|status| self.refused_for_this_direction("start", status))?;
        }
        Ok(())
    }

    fn start_handing_off_to(&mut self, hand_off: Delivery::HandOff) -> Result<()> {
        *self.callback_context.delivery.lock().installed_hand_off() = Some(hand_off);
        self.hal_output_unit.start().map_err(|status| {
            *self.callback_context.delivery.lock().installed_hand_off() = None;
            self.refused_for_this_direction("start", status)
        })
    }

    fn stop_handing_off(&mut self) -> Result<()> {
        let stopped = self.hal_output_unit.stop();
        // Cleared after the unit stops, and under the lock the callback holds
        // across its call, so no hand-off runs once this returns.
        *self.callback_context.delivery.lock().installed_hand_off() = None;
        stopped.map_err(|status| self.refused_for_this_direction("stop", status))
    }

    /// "would not start capturing", "would not stop playing", and so on.
    fn refused_for_this_direction(&self, verb: &str, status: i32) -> Error {
        self.device.refused_with_status(
            &format!(
                "would not {verb} {}",
                Delivery::DIRECTION.present_participle()
            ),
            status,
        )
    }
}

/// What one input cycle's render status calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputCycleRenderVerdict {
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
struct ConsecutiveInputCycleRenderFailures {
    consecutive_failures: u32,
    has_reported_a_failure: bool,
}

impl ConsecutiveInputCycleRenderFailures {
    fn verdict_on(&mut self, render_status: i32) -> InputCycleRenderVerdict {
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
fn duration_of_many_frames_in_ns(frame_count: u64, sample_rate: u32) -> i128 {
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
struct ConvertedCaptureBlockTimeline {
    device_sample_rate: u32,
    stream_sample_rate: u32,
    converter_latency_in_device_frames: u32,
    device_frames_lent: u64,
    stream_frames_produced: u64,
    current_cycle_first_input_sample_ns: i64,
    current_cycle_first_device_frame: u64,
}

impl ConvertedCaptureBlockTimeline {
    fn for_a_new_binding(
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
    fn a_cycle_is_lent(&mut self, first_input_sample_ns: i64, device_frame_count: u32) {
        self.current_cycle_first_input_sample_ns = first_input_sample_ns;
        self.current_cycle_first_device_frame = self.device_frames_lent;
        self.device_frames_lent += u64::from(device_frame_count);
    }

    /// The stamp of the next converted block's first sample; the timeline
    /// then moves past its `converted_frame_count` frames.
    fn stamp_the_next_block(&mut self, converted_frame_count: u32) -> i64 {
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
enum ConvertedInputCycle {
    HandedOff,
    TheConverterFailed(i32),
    TheHandOffPanicked,
}

/// An `AudioConverter` from a capture device's own format to its stream's,
/// with every buffer it writes into allocated before the device thread runs
/// it.
struct CoreAudioCaptureFormatConverter {
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
    fn new(
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
    fn convert_the_cycle(
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

/// The capture callback's state: the hand-off, and the buffer each input
/// cycle is rendered into before it is handed off.
struct CoreAudioCaptureDelivery {
    installed_hand_off: Option<CapturedAudioBlockHandOff>,
    /// Sized for the unit's largest cycle, in the format the unit renders.
    render_buffer: Vec<u8>,
    stream_format: AudioStreamFormat,
    /// The stream's format, or the device's own where a converter runs.
    unit_render_format: AudioStreamFormat,
    /// Present only while the device's own format is not the stream's.
    format_converter: Option<CoreAudioCaptureFormatConverter>,
    capture_latency_in_frames: u32,
    device: CoreAudioDevice,
    render_failures: ConsecutiveInputCycleRenderFailures,
    has_reported_a_cycle_without_host_time: bool,
}

impl CoreAudioCaptureDelivery {
    /// The callback's state before its unit is bound: nothing to render into,
    /// nothing to hand off to.
    fn before_its_first_binding(
        stream_format: AudioStreamFormat,
        device: &CoreAudioDevice,
    ) -> Self {
        Self {
            installed_hand_off: None,
            render_buffer: Vec::new(),
            stream_format,
            unit_render_format: stream_format,
            format_converter: None,
            capture_latency_in_frames: 0,
            device: device.clone(),
            render_failures: ConsecutiveInputCycleRenderFailures::default(),
            has_reported_a_cycle_without_host_time: false,
        }
    }

    /// Ready the callback for a unit bound to `device`, which carries
    /// `device_own_format` in cycles of at most `largest_cycle_in_frames`.
    /// Allocates, so it runs only while no cycle can.
    fn prepare_for_a_binding(
        &mut self,
        device: &CoreAudioDevice,
        format_bridge: CoreAudioStreamFormatBridge,
        device_own_format: AudioStreamFormat,
        largest_cycle_in_frames: u32,
    ) -> Result<()> {
        let unit_render_format =
            format_bridge.unit_client_side_format(self.stream_format, device_own_format);
        let format_converter = match format_bridge {
            CoreAudioStreamFormatBridge::AnAudioConverterTakesTheDevicesFormatToTheStreams => {
                Some(CoreAudioCaptureFormatConverter::new(
                    device,
                    device_own_format,
                    self.stream_format,
                    largest_cycle_in_frames,
                )?)
            }
            CoreAudioStreamFormatBridge::TheDeviceCarriesTheStreamFormat
            | CoreAudioStreamFormatBridge::AuhalConvertsTheStreamFormatForTheDevice => None,
        };
        self.render_buffer =
            vec![0u8; unit_render_format.interleaved_byte_count_for(largest_cycle_in_frames)];
        self.unit_render_format = unit_render_format;
        self.format_converter = format_converter;
        self.capture_latency_in_frames = capture_latency_in_frames_of(device);
        self.device = device.clone();
        self.render_failures = ConsecutiveInputCycleRenderFailures::default();
        Ok(())
    }
}

impl CoreAudioDeliveryHoldingAHandOff for CoreAudioCaptureDelivery {
    type HandOff = CapturedAudioBlockHandOff;

    const DIRECTION: CoreAudioStreamDirection = CoreAudioStreamDirection::Capture;
    const CALLBACK_PROPERTY_ID: u32 = kAudioOutputUnitProperty_SetInputCallback;
    const CALLBACK_SCOPE: u32 = kAudioUnitScope_Global;
    const CALLBACK: AURenderCallback = Some(captured_input_became_available);

    fn installed_hand_off(&mut self) -> &mut Option<CapturedAudioBlockHandOff> {
        &mut self.installed_hand_off
    }
}

type CoreAudioCaptureUnit = CoreAudioStreamUnit<CoreAudioCaptureDelivery>;

unsafe extern "C-unwind" fn captured_input_became_available(
    callback_context: NonNull<c_void>,
    action_flags: NonNull<AudioUnitRenderActionFlags>,
    time_stamp: NonNull<AudioTimeStamp>,
    bus_number: u32,
    frame_count: u32,
    _unused_buffer_list: *mut AudioBufferList,
) -> i32 {
    // SAFETY: registered with this context type, which outlives the unit it
    // was registered on.
    let context = unsafe {
        callback_context
            .cast::<CoreAudioCallbackContext<CoreAudioCaptureDelivery>>()
            .as_ref()
    };
    let mut delivery = context.delivery.lock();
    let CoreAudioCaptureDelivery {
        installed_hand_off,
        render_buffer,
        stream_format,
        unit_render_format,
        format_converter,
        capture_latency_in_frames,
        device,
        render_failures,
        has_reported_a_cycle_without_host_time,
    } = &mut *delivery;
    let unit_render_format = *unit_render_format;
    if installed_hand_off.is_none() {
        return NO_ERR;
    }
    let byte_count = unit_render_format.interleaved_byte_count_for(frame_count);
    if byte_count > render_buffer.len() {
        let largest_cycle_in_frames =
            render_buffer.len() / unit_render_format.interleaved_byte_count_for(1).max(1);
        context.end_the_stream_because(
            installed_hand_off,
            format!(
                "audio device {device} delivered an input cycle of {frame_count} frames, longer \
                 than the {largest_cycle_in_frames} frames its capture unit was sized for"
            ),
        );
        return NO_ERR;
    }
    let mut buffer_list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: unit_render_format.channels,
            mDataByteSize: byte_count as u32,
            mData: render_buffer.as_mut_ptr().cast(),
        }],
    };
    // SAFETY: the arguments are the ones this cycle was called with, and the
    // buffer list points at `byte_count` writable bytes.
    let status = unsafe {
        AudioUnitRender(
            context.audio_unit,
            action_flags.as_ptr(),
            time_stamp,
            bus_number,
            frame_count,
            NonNull::from(&mut buffer_list),
        )
    };
    match render_failures.verdict_on(status) {
        InputCycleRenderVerdict::HandTheBlockOff => {}
        InputCycleRenderVerdict::DropTheCycleAndReportTheFirstFailure => {
            tracing::warn!(
                device = %device,
                status = %osstatus_text(status),
                "CoreAudio audio arm: an input cycle failed to render and was dropped; the \
                 stream ends if {CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS} fail in \
                 a row"
            );
            return status;
        }
        InputCycleRenderVerdict::DropTheCycleQuietly => return status,
        InputCycleRenderVerdict::EndTheStream {
            consecutive_failures,
        } => {
            context.end_the_stream_because(
                installed_hand_off,
                format!(
                    "audio device {device} failed to render {consecutive_failures} input cycles \
                     in a row, the last with {}",
                    osstatus_text(status)
                ),
            );
            return status;
        }
    }
    let Some(hand_off) = installed_hand_off.as_ref() else {
        return NO_ERR;
    };
    // SAFETY: CoreAudio passes a valid timestamp for the cycle.
    let time_stamp = unsafe { time_stamp.as_ref() };
    let input_cycle_host_time_ns = if time_stamp
        .mFlags
        .contains(AudioTimeStampFlags::HostTimeValid)
    {
        MediaClock::nanos_from_raw_timestamp(time_stamp.mHostTime).as_nanos() as i64
    } else {
        if !*has_reported_a_cycle_without_host_time {
            *has_reported_a_cycle_without_host_time = true;
            tracing::warn!(
                device = %device,
                "CoreAudio audio arm: an input cycle carried no host time, so this stream's \
                 blocks are stamped from when they were delivered rather than by the device"
            );
        }
        // Were a cycle ever without host time, it ended now and began a
        // block ago.
        MediaClock::now().as_nanos() as i64
            - duration_of_frames_in_ns(frame_count, unit_render_format.sample_rate)
    };
    // Whole frames only, so a short render still carries `sample_count ×
    // channels` scalars as the seam promises.
    let bytes_per_frame = unit_render_format.interleaved_byte_count_for(1).max(1);
    let rendered_frame_count =
        (buffer_list.mBuffers[0].mDataByteSize as usize).min(byte_count) / bytes_per_frame;
    let rendered_byte_count = rendered_frame_count * bytes_per_frame;
    let first_input_sample_timestamp_ns = first_sample_timestamp_ns(
        input_cycle_host_time_ns,
        *capture_latency_in_frames,
        unit_render_format.sample_rate,
    );
    let Some(format_converter) = format_converter.as_mut() else {
        let handed_off = catch_unwind(AssertUnwindSafe(|| {
            hand_off(CapturedAudioBlockFromDevice {
                interleaved_sample_bytes: &render_buffer[..rendered_byte_count],
                sample_count: rendered_frame_count as u32,
                first_sample_timestamp_ns: first_input_sample_timestamp_ns,
            })
        }));
        if handed_off.is_err() {
            context.end_the_stream_because_the_hand_off_panicked(installed_hand_off);
        }
        return NO_ERR;
    };
    match format_converter.convert_the_cycle(
        &mut render_buffer[..rendered_byte_count],
        rendered_frame_count as u32,
        first_input_sample_timestamp_ns,
        &**hand_off,
    ) {
        ConvertedInputCycle::HandedOff => {}
        ConvertedInputCycle::TheConverterFailed(status) => context.end_the_stream_because(
            installed_hand_off,
            format!(
                "audio device {device}'s input could not be converted from {} to the stream's \
                 {}: {}",
                rate_and_channels_of(unit_render_format),
                rate_and_channels_of(*stream_format),
                osstatus_text(status)
            ),
        ),
        ConvertedInputCycle::TheHandOffPanicked => {
            context.end_the_stream_because_the_hand_off_panicked(installed_hand_off)
        }
    }
    NO_ERR
}

/// Bind an input unit to `device`: rendering the stream's format where the
/// device carries it, and the device's own through a converter where it does
/// not.
///
/// Called only once microphone access is granted: binding a unit with input
/// enabled asks `coreaudiod`, which blocks the binding call until the user has
/// answered the privacy prompt.
fn bind_capture_unit(
    device: &CoreAudioDevice,
    stream_format: AudioStreamFormat,
    device_own_format: AudioStreamFormat,
    failure_recorder: DeviceStreamFailureRecorder,
) -> Result<CoreAudioCaptureUnit> {
    let format_bridge = CoreAudioStreamFormatBridge::between(
        CoreAudioStreamDirection::Capture,
        stream_format,
        device_own_format,
    );
    let capture_unit = CoreAudioCaptureUnit::bound_to(
        device,
        format_bridge.unit_client_side_format(stream_format, device_own_format),
        CoreAudioCaptureDelivery::before_its_first_binding(stream_format, device),
        failure_recorder,
    )?;
    let largest_cycle_in_frames = capture_unit.largest_cycle_in_frames;
    let mut delivery = capture_unit.callback_context.delivery.lock();
    delivery.prepare_for_a_binding(
        device,
        format_bridge,
        device_own_format,
        largest_cycle_in_frames,
    )?;
    tracing::debug!(
        device = %device,
        largest_cycle_in_frames,
        capture_latency_in_frames = delivery.capture_latency_in_frames,
        conversion = %format_bridge,
        "CoreAudio audio arm: capture unit bound"
    );
    drop(delivery);
    Ok(capture_unit)
}

/// Move a bound input unit to `device`, on the terms [`bind_capture_unit`]
/// binds one.
fn rebind_capture_unit(
    capture_unit: &mut CoreAudioCaptureUnit,
    device: &CoreAudioDevice,
    stream_format: AudioStreamFormat,
    device_own_format: AudioStreamFormat,
) -> Result<()> {
    let format_bridge = CoreAudioStreamFormatBridge::between(
        CoreAudioStreamDirection::Capture,
        stream_format,
        device_own_format,
    );
    capture_unit.rebind_to(
        device,
        format_bridge.unit_client_side_format(stream_format, device_own_format),
        |delivery, largest_cycle_in_frames| {
            delivery.prepare_for_a_binding(
                device,
                format_bridge,
                device_own_format,
                largest_cycle_in_frames,
            )
        },
    )
}

/// Whether the user has let this process use the microphone, as far as one
/// capture stream knows — and the unit, which exists only once they have.
enum MicrophoneAccessForTheStream {
    Granted(CoreAudioCaptureUnit),
    /// The user is being asked; a hand-off installed meanwhile waits here.
    AwaitingTheUsersAnswer {
        parked_hand_off: Option<CapturedAudioBlockHandOff>,
    },
    /// The user refused, or a device refused a unit; the text says which.
    Refused(String),
}

/// Everything that starts, stops and rebinds a capture stream, behind one
/// lock: the processor's start and stop, the user's permission answer and
/// the stream's device changes all reach it, from different threads. The
/// device's I/O thread never takes it.
struct CoreAudioCaptureControl {
    microphone_access: MicrophoneAccessForTheStream,
    stream_format: AudioStreamFormat,
    device_binding: CoreAudioStreamDeviceBinding,
}

impl CoreAudioCaptureControl {
    fn stop_delivering(&mut self) -> Result<()> {
        match &mut self.microphone_access {
            MicrophoneAccessForTheStream::Granted(capture_unit) => capture_unit.stop_handing_off(),
            MicrophoneAccessForTheStream::AwaitingTheUsersAnswer { parked_hand_off } => {
                *parked_hand_off = None;
                Ok(())
            }
            MicrophoneAccessForTheStream::Refused(_) => Ok(()),
        }
    }
}

impl CoreAudioStreamControlThatFollowsItsDevice for CoreAudioCaptureControl {
    fn stream_format(&self) -> AudioStreamFormat {
        self.stream_format
    }

    fn device_binding(&mut self) -> &mut CoreAudioStreamDeviceBinding {
        &mut self.device_binding
    }

    fn rebind_the_unit_to(
        &mut self,
        device: &CoreAudioDevice,
        device_own_format: AudioStreamFormat,
    ) -> Result<()> {
        match &mut self.microphone_access {
            MicrophoneAccessForTheStream::Granted(capture_unit) => {
                rebind_capture_unit(capture_unit, device, self.stream_format, device_own_format)
            }
            // No unit yet: the user's answer binds wherever the binding points
            // by then.
            MicrophoneAccessForTheStream::AwaitingTheUsersAnswer { .. }
            | MicrophoneAccessForTheStream::Refused(_) => Ok(()),
        }
    }

    fn release_the_unit_after_a_failed_rebind(&mut self, failure: String) {
        self.microphone_access = MicrophoneAccessForTheStream::Refused(failure);
    }
}

/// A capture stream on one CoreAudio input device at a time.
pub struct CoreAudioCaptureStream {
    stream_format: AudioStreamFormat,
    liveness_report: DeviceStreamLivenessReport,
    capture_control: Arc<Mutex<CoreAudioCaptureControl>>,
}

impl CoreAudioCaptureStream {
    fn open(
        request: &AudioDeviceStreamRequest,
        microphone_authorization_authority: &dyn CaptureDeviceAuthorizationAuthority,
    ) -> Result<Self> {
        let direction = CoreAudioStreamDirection::Capture;
        let device = resolve_requested_device(request, direction)?;
        let stream_format = stream_format_of(&device, direction)?;
        Self::open_on(
            device,
            stream_format,
            CoreAudioStreamDevicePolicy::of(request),
            microphone_authorization_authority,
        )
    }

    /// Open on `device`, carrying `stream_format` for the stream's lifetime
    /// whatever the device — or a later default — carries.
    fn open_on(
        device: CoreAudioDevice,
        stream_format: AudioStreamFormat,
        device_policy: CoreAudioStreamDevicePolicy,
        microphone_authorization_authority: &dyn CaptureDeviceAuthorizationAuthority,
    ) -> Result<Self> {
        let direction = CoreAudioStreamDirection::Capture;
        let device_own_format = stream_format_of(&device, direction)?;
        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();

        tracing::info!(
            device = %device,
            sample_rate = stream_format.sample_rate,
            channels = stream_format.channels,
            ?device_policy,
            "CoreAudio audio arm: capture stream opened"
        );

        let capture_control = Arc::new_cyclic(|capture_control| {
            Mutex::new(CoreAudioCaptureControl {
                microphone_access: MicrophoneAccessForTheStream::AwaitingTheUsersAnswer {
                    parked_hand_off: None,
                },
                stream_format,
                device_binding: CoreAudioStreamDeviceBinding::new(
                    direction,
                    device,
                    device_own_format,
                    device_policy,
                    failure_recorder,
                    liveness_report.clone(),
                    device_changes_reach(capture_control.clone()),
                ),
            })
        });
        capture_control.lock().device_binding.start_listening();

        let answer_reaches = Arc::downgrade(&capture_control);
        match authorize_the_capture_device_without_waiting_for_the_user(
            microphone_authorization_authority,
            Box::new(move |granted| the_users_microphone_answer_arrived(&answer_reaches, granted)),
        )? {
            CaptureDeviceAuthorizationAtOpen::Granted => {
                let mut control = capture_control.lock();
                let capture_unit = bind_capture_unit(
                    &control.device_binding.device,
                    control.stream_format,
                    control.device_binding.device_own_format,
                    control.device_binding.failure_recorder.clone(),
                )?;
                control.microphone_access = MicrophoneAccessForTheStream::Granted(capture_unit);
            }
            CaptureDeviceAuthorizationAtOpen::AwaitingTheUsersAnswer => {}
        }

        Ok(Self {
            stream_format,
            liveness_report,
            capture_control,
        })
    }
}

/// What a stream does with the user's answer to its microphone request,
/// whenever and on whatever thread it arrives.
fn the_users_microphone_answer_arrived(
    capture_control: &Weak<Mutex<CoreAudioCaptureControl>>,
    granted: bool,
) {
    let Some(capture_control) = capture_control.upgrade() else {
        return;
    };
    let mut control = capture_control.lock();
    let MicrophoneAccessForTheStream::AwaitingTheUsersAnswer { parked_hand_off } =
        &mut control.microphone_access
    else {
        return;
    };
    let parked_hand_off = parked_hand_off.take();
    if !granted {
        let refusal = capture_device_refusal_for_the_user(
            PrivacyGatedCaptureDevice::Microphone,
            CaptureDeviceRefusal::DeniedByTheUser,
        );
        control
            .device_binding
            .record_the_failure_that_ended_the_stream(refusal.clone());
        control.microphone_access = MicrophoneAccessForTheStream::Refused(refusal);
        return;
    }
    tracing::info!(device = %control.device_binding.device, "microphone access allowed");
    match bind_capture_unit(
        &control.device_binding.device,
        control.stream_format,
        control.device_binding.device_own_format,
        control.device_binding.failure_recorder.clone(),
    ) {
        Err(bind_failure) => {
            control
                .device_binding
                .record_the_failure_that_ended_the_stream(bind_failure.to_string());
            control.microphone_access =
                MicrophoneAccessForTheStream::Refused(bind_failure.to_string());
        }
        Ok(mut capture_unit) => {
            let started = match parked_hand_off {
                Some(hand_off) => capture_unit.start_handing_off_to(hand_off),
                None => Ok(()),
            };
            control.microphone_access = MicrophoneAccessForTheStream::Granted(capture_unit);
            if let Err(start_failure) = started {
                control
                    .device_binding
                    .record_the_failure_that_ended_the_stream(start_failure.to_string());
            }
        }
    }
}

impl AudioCaptureStream for CoreAudioCaptureStream {
    fn stream_format(&self) -> AudioStreamFormat {
        self.stream_format
    }

    fn liveness_report(&self) -> DeviceStreamLivenessReport {
        self.liveness_report.clone()
    }

    fn start_delivering_to(&mut self, hand_off: CapturedAudioBlockHandOff) -> Result<()> {
        let mut control = self.capture_control.lock();
        control.stop_delivering()?;
        match &mut control.microphone_access {
            MicrophoneAccessForTheStream::Granted(capture_unit) => {
                capture_unit.start_handing_off_to(hand_off)
            }
            MicrophoneAccessForTheStream::AwaitingTheUsersAnswer { parked_hand_off } => {
                *parked_hand_off = Some(hand_off);
                Ok(())
            }
            MicrophoneAccessForTheStream::Refused(refusal) => {
                Err(Error::Configuration(refusal.clone()))
            }
        }
    }

    fn stop_delivering(&mut self) -> Result<()> {
        self.capture_control.lock().stop_delivering()
    }
}

impl Drop for CoreAudioCaptureStream {
    fn drop(&mut self) {
        if let Err(stop_error) = self.stop_delivering() {
            tracing::warn!(
                error = %stop_error,
                "CoreAudio capture stream dropped while delivering"
            );
        }
    }
}

/// The playback callback's state: the hand-off it asks for samples.
struct CoreAudioPlaybackDelivery {
    installed_hand_off: Option<AudioBlockForPlaybackHandOff>,
}

impl CoreAudioDeliveryHoldingAHandOff for CoreAudioPlaybackDelivery {
    type HandOff = AudioBlockForPlaybackHandOff;

    const DIRECTION: CoreAudioStreamDirection = CoreAudioStreamDirection::Playback;
    const CALLBACK_PROPERTY_ID: u32 = kAudioUnitProperty_SetRenderCallback;
    const CALLBACK_SCOPE: u32 = kAudioUnitScope_Input;
    const CALLBACK: AURenderCallback = Some(playback_samples_requested);

    fn installed_hand_off(&mut self) -> &mut Option<AudioBlockForPlaybackHandOff> {
        &mut self.installed_hand_off
    }
}

type CoreAudioPlaybackUnit = CoreAudioStreamUnit<CoreAudioPlaybackDelivery>;

unsafe extern "C-unwind" fn playback_samples_requested(
    callback_context: NonNull<c_void>,
    action_flags: NonNull<AudioUnitRenderActionFlags>,
    _time_stamp: NonNull<AudioTimeStamp>,
    _bus_number: u32,
    frame_count: u32,
    buffer_list: *mut AudioBufferList,
) -> i32 {
    // SAFETY: registered with this context type, which outlives the unit it
    // was registered on.
    let context = unsafe {
        callback_context
            .cast::<CoreAudioCallbackContext<CoreAudioPlaybackDelivery>>()
            .as_ref()
    };
    let Some(buffer_list) = NonNull::new(buffer_list) else {
        return NO_ERR;
    };
    // SAFETY: an interleaved client format renders into exactly one buffer,
    // which CoreAudio owns for the length of this call.
    let buffer = unsafe { &mut (*buffer_list.as_ptr()).mBuffers[0] };
    let Some(data) = NonNull::new(buffer.mData.cast::<u8>()) else {
        return NO_ERR;
    };
    // SAFETY: `mData` holds `mDataByteSize` writable bytes for this call.
    let interleaved_sample_bytes_to_fill =
        unsafe { std::slice::from_raw_parts_mut(data.as_ptr(), buffer.mDataByteSize as usize) };
    let mut delivery = context.delivery.lock();
    let Some(hand_off) = delivery.installed_hand_off.as_ref() else {
        interleaved_sample_bytes_to_fill.fill(0);
        // SAFETY: the flags are this cycle's, writable for its length.
        unsafe {
            (*action_flags.as_ptr()).0 |=
                AudioUnitRenderActionFlags::UnitRenderAction_OutputIsSilence.0
        };
        return NO_ERR;
    };
    let handed_off = catch_unwind(AssertUnwindSafe(|| {
        hand_off(AudioBlockRequestedByDevice {
            interleaved_sample_bytes_to_fill: &mut *interleaved_sample_bytes_to_fill,
            sample_count: frame_count,
        })
    }));
    if handed_off.is_err() {
        interleaved_sample_bytes_to_fill.fill(0);
        context.end_the_stream_because_the_hand_off_panicked(&mut delivery.installed_hand_off);
    }
    NO_ERR
}

/// Everything that starts, stops and rebinds a playback stream, behind one
/// lock the device's I/O thread never takes.
struct CoreAudioPlaybackControl {
    /// `None` once a failed rebind has ended the stream.
    playback_unit: Option<CoreAudioPlaybackUnit>,
    stream_format: AudioStreamFormat,
    device_binding: CoreAudioStreamDeviceBinding,
}

impl CoreAudioStreamControlThatFollowsItsDevice for CoreAudioPlaybackControl {
    fn stream_format(&self) -> AudioStreamFormat {
        self.stream_format
    }

    fn device_binding(&mut self) -> &mut CoreAudioStreamDeviceBinding {
        &mut self.device_binding
    }

    fn rebind_the_unit_to(
        &mut self,
        device: &CoreAudioDevice,
        _device_own_format: AudioStreamFormat,
    ) -> Result<()> {
        let Some(playback_unit) = self.playback_unit.as_mut() else {
            return Ok(());
        };
        playback_unit.rebind_to(device, self.stream_format, |_delivery, _largest_cycle| {
            Ok(())
        })
    }

    fn release_the_unit_after_a_failed_rebind(&mut self, _failure: String) {
        self.playback_unit = None;
    }
}

/// A playback stream on one CoreAudio output device at a time.
pub struct CoreAudioPlaybackStream {
    stream_format: AudioStreamFormat,
    /// The device's `BufferFrameSize` in output scope when the stream opened.
    device_period_in_per_channel_samples: Option<u32>,
    liveness_report: DeviceStreamLivenessReport,
    playback_control: Arc<Mutex<CoreAudioPlaybackControl>>,
}

impl CoreAudioPlaybackStream {
    fn open(request: &AudioDeviceStreamRequest) -> Result<Self> {
        let direction = CoreAudioStreamDirection::Playback;
        let device = resolve_requested_device(request, direction)?;
        let stream_format = stream_format_of(&device, direction)?;
        Self::open_on(
            device,
            stream_format,
            CoreAudioStreamDevicePolicy::of(request),
        )
    }

    /// Open on `device`, carrying `stream_format` for the stream's lifetime
    /// whatever the device — or a later default — carries.
    fn open_on(
        device: CoreAudioDevice,
        stream_format: AudioStreamFormat,
        device_policy: CoreAudioStreamDevicePolicy,
    ) -> Result<Self> {
        let direction = CoreAudioStreamDirection::Playback;
        let device_own_format = stream_format_of(&device, direction)?;
        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();
        let playback_unit = CoreAudioPlaybackUnit::bound_to(
            &device,
            stream_format,
            CoreAudioPlaybackDelivery {
                installed_hand_off: None,
            },
            failure_recorder.clone(),
        )?;
        let device_period_in_per_channel_samples = audio_object_property::<u32>(
            device.object_id,
            kAudioDevicePropertyBufferFrameSize,
            direction.device_property_scope(),
        )
        .filter(|&frames| frames > 0);

        tracing::info!(
            device = %device,
            sample_rate = stream_format.sample_rate,
            channels = stream_format.channels,
            device_period_in_per_channel_samples,
            largest_cycle_in_frames = playback_unit.largest_cycle_in_frames,
            ?device_policy,
            conversion = %CoreAudioStreamFormatBridge::between(direction, stream_format, device_own_format),
            "CoreAudio audio arm: playback stream opened"
        );

        let playback_control = Arc::new_cyclic(|playback_control| {
            Mutex::new(CoreAudioPlaybackControl {
                playback_unit: Some(playback_unit),
                stream_format,
                device_binding: CoreAudioStreamDeviceBinding::new(
                    direction,
                    device,
                    device_own_format,
                    device_policy,
                    failure_recorder,
                    liveness_report.clone(),
                    device_changes_reach(playback_control.clone()),
                ),
            })
        });
        playback_control.lock().device_binding.start_listening();

        Ok(Self {
            stream_format,
            device_period_in_per_channel_samples,
            liveness_report,
            playback_control,
        })
    }

    /// Why a stream whose rebind failed cannot play again.
    fn ended_by_a_failed_rebind(&self) -> Error {
        Error::Configuration(
            self.liveness_report
                .failure_that_ended_the_stream()
                .map(|failure| failure.to_string())
                .unwrap_or_else(|| "the CoreAudio playback stream has ended".to_owned()),
        )
    }
}

impl AudioPlaybackStream for CoreAudioPlaybackStream {
    fn stream_format(&self) -> AudioStreamFormat {
        self.stream_format
    }

    fn device_period_in_per_channel_samples(&self) -> Option<u32> {
        self.device_period_in_per_channel_samples
    }

    fn liveness_report(&self) -> DeviceStreamLivenessReport {
        self.liveness_report.clone()
    }

    fn start_requesting_from(&mut self, hand_off: AudioBlockForPlaybackHandOff) -> Result<()> {
        let mut control = self.playback_control.lock();
        let Some(playback_unit) = control.playback_unit.as_mut() else {
            return Err(self.ended_by_a_failed_rebind());
        };
        playback_unit.stop_handing_off()?;
        playback_unit.start_handing_off_to(hand_off)
    }

    fn stop_requesting(&mut self) -> Result<()> {
        match self.playback_control.lock().playback_unit.as_mut() {
            Some(playback_unit) => playback_unit.stop_handing_off(),
            None => Ok(()),
        }
    }
}

impl Drop for CoreAudioPlaybackStream {
    fn drop(&mut self) {
        if let Err(stop_error) = self.stop_requesting() {
            tracing::warn!(
                error = %stop_error,
                "CoreAudio playback stream dropped while playing"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_with_no_latency_stamps_its_first_sample_at_the_cycles_host_time() {
        assert_eq!(
            first_sample_timestamp_ns(5_000_000_000, 0, 48_000),
            5_000_000_000
        );
    }

    /// The device's latency is the distance between a sample arriving and the
    /// cycle that carries it; a stamp that ignored it would sit that far late,
    /// every block, forever.
    #[test]
    fn the_devices_latency_moves_the_stamp_back_by_that_many_frames() {
        assert_eq!(
            first_sample_timestamp_ns(5_000_000_000, 480, 48_000),
            5_000_000_000 - 10_000_000,
            "480 frames at 48 kHz is 10 ms"
        );
        assert_eq!(
            first_sample_timestamp_ns(5_000_000_000, 441, 44_100),
            5_000_000_000 - 10_000_000,
            "441 frames at 44.1 kHz is 10 ms"
        );
    }

    #[test]
    fn a_named_device_that_is_not_attached_is_refused_naming_it_and_the_ones_that_are() {
        let refusal = refusal_for_a_named_audio_device_that_is_not_attached(
            "NoSuchDeviceUID",
            CoreAudioStreamDirection::Capture,
            &[CoreAudioDevice {
                object_id: 42,
                uid: "BuiltInMicrophoneDevice".into(),
                name: "MacBook Pro Microphone".into(),
            }],
        );
        assert!(refusal.contains("'NoSuchDeviceUID'"), "{refusal}");
        assert!(
            refusal.contains("BuiltInMicrophoneDevice (MacBook Pro Microphone)"),
            "{refusal}"
        );
        assert!(refusal.contains("capture"), "{refusal}");
    }

    #[test]
    fn a_named_device_with_nothing_attached_says_no_device_carries_that_direction() {
        let refusal = refusal_for_a_named_audio_device_that_is_not_attached(
            "NoSuchDeviceUID",
            CoreAudioStreamDirection::Playback,
            &[],
        );
        assert!(refusal.contains("'NoSuchDeviceUID'"), "{refusal}");
        assert!(
            refusal.contains("no device on this Mac carries playback"),
            "{refusal}"
        );
    }

    const A_FAILED_RENDER: i32 = -10863;

    #[test]
    fn a_rendered_cycle_is_handed_off() {
        let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
        assert_eq!(
            render_failures.verdict_on(NO_ERR),
            InputCycleRenderVerdict::HandTheBlockOff
        );
    }

    /// The first failed render is the one worth a line; the ones behind it
    /// until the bound would only repeat it at device cadence.
    #[test]
    fn the_first_failed_render_is_reported_and_the_rest_below_the_bound_are_not() {
        let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
        assert_eq!(
            render_failures.verdict_on(A_FAILED_RENDER),
            InputCycleRenderVerdict::DropTheCycleAndReportTheFirstFailure
        );
        for _ in 2..CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS {
            assert_eq!(
                render_failures.verdict_on(A_FAILED_RENDER),
                InputCycleRenderVerdict::DropTheCycleQuietly
            );
        }
    }

    /// Mental revert: return the status and nothing else, as the arm did, and
    /// a device that stopped rendering leaves its stream's report clear
    /// forever.
    #[test]
    fn failed_renders_reaching_the_bound_in_a_row_end_the_stream_once() {
        let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
        let verdicts: Vec<InputCycleRenderVerdict> = (0
            ..CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS + 5)
            .map(|_| render_failures.verdict_on(A_FAILED_RENDER))
            .collect();
        let endings: Vec<usize> = verdicts
            .iter()
            .enumerate()
            .filter(|(_, verdict)| matches!(verdict, InputCycleRenderVerdict::EndTheStream { .. }))
            .map(|(cycle, _)| cycle)
            .collect();
        assert_eq!(
            endings,
            [CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS as usize - 1],
            "the stream ends on the bound-th failure in a row, and only then"
        );
        assert_eq!(
            verdicts[endings[0]],
            InputCycleRenderVerdict::EndTheStream {
                consecutive_failures: CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS
            }
        );
    }

    /// A device that drops the odd cycle is not a device that stopped.
    #[test]
    fn a_rendered_cycle_resets_the_run_so_scattered_failures_never_end_the_stream() {
        let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
        for _ in 0..4 {
            for _ in 1..CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS {
                assert!(!matches!(
                    render_failures.verdict_on(A_FAILED_RENDER),
                    InputCycleRenderVerdict::EndTheStream { .. }
                ));
            }
            assert_eq!(
                render_failures.verdict_on(NO_ERR),
                InputCycleRenderVerdict::HandTheBlockOff
            );
        }
    }

    #[test]
    fn a_later_run_of_failures_is_not_reported_again_before_it_ends_the_stream() {
        let mut render_failures = ConsecutiveInputCycleRenderFailures::default();
        render_failures.verdict_on(A_FAILED_RENDER);
        render_failures.verdict_on(NO_ERR);
        assert_eq!(
            render_failures.verdict_on(A_FAILED_RENDER),
            InputCycleRenderVerdict::DropTheCycleQuietly
        );
    }

    /// TN2091: the render callback belongs on the output element's input
    /// scope, the input callback on global scope.
    #[test]
    fn each_callback_is_installed_on_the_scope_its_property_documents() {
        assert_eq!(
            CoreAudioPlaybackDelivery::CALLBACK_SCOPE,
            kAudioUnitScope_Input
        );
        assert_eq!(
            CoreAudioCaptureDelivery::CALLBACK_SCOPE,
            kAudioUnitScope_Global
        );
    }

    #[test]
    fn a_status_that_is_a_four_character_code_is_spelled_out() {
        assert_eq!(
            osstatus_text(i32::from_be_bytes(*b"!dev")),
            format!("OSStatus {} ('!dev')", i32::from_be_bytes(*b"!dev"))
        );
        assert_eq!(osstatus_text(-10863), "OSStatus -10863");
    }

    /// Whatever rate and channel count the unit's client side carries, it is
    /// described as the seam's interleaved float.
    #[test]
    fn the_units_client_format_is_the_streams_interleaved_float_format() {
        let description = interleaved_f32_description_of(AudioStreamFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: AudioSampleFormat::F32,
        });
        assert_eq!(description.mSampleRate, 48_000.0);
        assert_eq!(description.mChannelsPerFrame, 2);
        assert_eq!(description.mBytesPerFrame, 8);
        assert_eq!(description.mBitsPerChannel, 32);
        assert_eq!(
            description.mFormatFlags & objc2_core_audio_types::kAudioFormatFlagIsNonInterleaved,
            0,
            "the seam carries interleaved payloads"
        );
    }

    fn interleaved_float(sample_rate: u32, channels: u32) -> AudioStreamFormat {
        AudioStreamFormat {
            sample_rate,
            channels,
            sample_format: AudioSampleFormat::F32,
        }
    }

    #[test]
    fn a_capture_device_carrying_the_streams_format_renders_it_with_no_converter() {
        let stream_format = interleaved_float(48_000, 1);
        let format_bridge = CoreAudioStreamFormatBridge::between(
            CoreAudioStreamDirection::Capture,
            stream_format,
            interleaved_float(48_000, 1),
        );
        assert_eq!(
            format_bridge,
            CoreAudioStreamFormatBridge::TheDeviceCarriesTheStreamFormat
        );
        assert_eq!(
            format_bridge.unit_client_side_format(stream_format, stream_format),
            stream_format
        );
    }

    /// AUHAL input cannot resample, so a capture unit on a device of another
    /// format renders the device's own and the arm converts. Mental revert:
    /// keep the stream's format on the unit, and the unit is asked for a rate
    /// its device does not run at.
    #[test]
    fn a_capture_device_at_another_rate_or_channel_count_renders_its_own_format_for_a_converter() {
        let stream_format = interleaved_float(48_000, 1);
        for device_own_format in [
            interleaved_float(24_000, 1),
            interleaved_float(16_000, 1),
            interleaved_float(48_000, 2),
            interleaved_float(96_000, 2),
        ] {
            let format_bridge = CoreAudioStreamFormatBridge::between(
                CoreAudioStreamDirection::Capture,
                stream_format,
                device_own_format,
            );
            assert_eq!(
                format_bridge,
                CoreAudioStreamFormatBridge::AnAudioConverterTakesTheDevicesFormatToTheStreams,
                "{device_own_format:?}"
            );
            assert_eq!(
                format_bridge.unit_client_side_format(stream_format, device_own_format),
                device_own_format
            );
        }
    }

    #[test]
    fn a_playback_device_of_another_format_is_left_to_auhal_and_the_client_keeps_the_streams() {
        let stream_format = interleaved_float(48_000, 2);
        let device_own_format = interleaved_float(24_000, 1);
        let format_bridge = CoreAudioStreamFormatBridge::between(
            CoreAudioStreamDirection::Playback,
            stream_format,
            device_own_format,
        );
        assert_eq!(
            format_bridge,
            CoreAudioStreamFormatBridge::AuhalConvertsTheStreamFormatForTheDevice
        );
        assert_eq!(
            format_bridge.unit_client_side_format(stream_format, device_own_format),
            stream_format,
            "the stream's format is fixed for its lifetime"
        );
        assert_eq!(
            CoreAudioStreamFormatBridge::between(
                CoreAudioStreamDirection::Playback,
                stream_format,
                stream_format
            ),
            CoreAudioStreamFormatBridge::TheDeviceCarriesTheStreamFormat
        );
    }

    const THE_BOUND_DEVICE: AudioObjectID = 71;
    const ANOTHER_DEVICE: AudioObjectID = 93;

    fn device_facts(
        the_bound_device_is_alive: bool,
        the_bound_devices_own_format_changed: bool,
        system_default_device: Option<AudioObjectID>,
    ) -> CoreAudioDeviceFactsAfterAChange {
        CoreAudioDeviceFactsAfterAChange {
            the_bound_device_is_alive,
            the_bound_devices_own_format_changed,
            system_default_device,
        }
    }

    fn response_of_a_stream(
        device_policy: CoreAudioStreamDevicePolicy,
        change: CoreAudioStreamDeviceChange,
        facts: CoreAudioDeviceFactsAfterAChange,
    ) -> CoreAudioStreamDeviceChangeResponse {
        how_a_stream_responds_to_a_device_change(change, device_policy, THE_BOUND_DEVICE, facts)
    }

    const FOLLOWS_THE_DEFAULT: CoreAudioStreamDevicePolicy =
        CoreAudioStreamDevicePolicy::FollowsTheSystemDefault;
    const NAMED: CoreAudioStreamDevicePolicy = CoreAudioStreamDevicePolicy::PinnedToTheNamedDevice;

    #[test]
    fn a_named_stream_whose_device_went_away_ends_rather_than_landing_elsewhere() {
        assert_eq!(
            response_of_a_stream(
                NAMED,
                CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
                device_facts(false, false, Some(ANOTHER_DEVICE)),
            ),
            CoreAudioStreamDeviceChangeResponse::EndTheStreamBecauseTheNamedDeviceWentAway
        );
    }

    #[test]
    fn a_named_stream_stays_on_its_device_when_the_default_moves() {
        assert_eq!(
            response_of_a_stream(
                NAMED,
                CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
                device_facts(true, false, Some(ANOTHER_DEVICE)),
            ),
            CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
        );
    }

    /// Plugging in headphones or connecting AirPods moves the default; an
    /// unnamed stream goes with it, as a macOS app does.
    #[test]
    fn an_unnamed_stream_moves_to_a_default_that_moved() {
        assert_eq!(
            response_of_a_stream(
                FOLLOWS_THE_DEFAULT,
                CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
                device_facts(true, false, Some(ANOTHER_DEVICE)),
            ),
            CoreAudioStreamDeviceChangeResponse::MoveToTheSystemDefault(ANOTHER_DEVICE)
        );
    }

    /// Unplugging the headphones takes the device away and moves the default
    /// back; whichever of the two reports arrives first, the stream follows.
    #[test]
    fn an_unnamed_stream_whose_device_went_away_moves_to_the_new_default() {
        assert_eq!(
            response_of_a_stream(
                FOLLOWS_THE_DEFAULT,
                CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
                device_facts(false, false, Some(ANOTHER_DEVICE)),
            ),
            CoreAudioStreamDeviceChangeResponse::MoveToTheSystemDefault(ANOTHER_DEVICE)
        );
    }

    /// A device can go away a moment before the default moves off it; the
    /// move is its own report, so ending the stream here would end it early.
    #[test]
    fn an_unnamed_stream_waits_for_the_default_to_move_off_a_device_that_went_away() {
        assert_eq!(
            response_of_a_stream(
                FOLLOWS_THE_DEFAULT,
                CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
                device_facts(false, false, Some(THE_BOUND_DEVICE)),
            ),
            CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
        );
    }

    #[test]
    fn an_unnamed_stream_whose_default_went_away_with_no_replacement_ends() {
        for change in [
            CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
            CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
        ] {
            assert_eq!(
                response_of_a_stream(
                    FOLLOWS_THE_DEFAULT,
                    change,
                    device_facts(false, false, None)
                ),
                CoreAudioStreamDeviceChangeResponse::EndTheStreamBecauseNoDeviceIsTheDefault,
                "{change:?}"
            );
        }
    }

    #[test]
    fn a_default_that_lands_on_the_bound_device_changes_nothing() {
        assert_eq!(
            response_of_a_stream(
                FOLLOWS_THE_DEFAULT,
                CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
                device_facts(true, false, Some(THE_BOUND_DEVICE)),
            ),
            CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
        );
    }

    #[test]
    fn a_liveness_report_from_a_device_still_alive_changes_nothing() {
        for device_policy in [NAMED, FOLLOWS_THE_DEFAULT] {
            assert_eq!(
                response_of_a_stream(
                    device_policy,
                    CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
                    device_facts(true, false, Some(ANOTHER_DEVICE)),
                ),
                CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
            );
        }
    }

    /// A 48 kHz input switched to 24 kHz — AirPods going to the headset
    /// profile — keeps the stream's format by rebinding, named or not.
    #[test]
    fn a_bound_device_at_a_new_format_is_rebound_named_or_not() {
        for device_policy in [NAMED, FOLLOWS_THE_DEFAULT] {
            assert_eq!(
                response_of_a_stream(
                    device_policy,
                    CoreAudioStreamDeviceChange::BoundDeviceFormatChanged,
                    device_facts(true, true, Some(ANOTHER_DEVICE)),
                ),
                CoreAudioStreamDeviceChangeResponse::RebindToTheBoundDeviceAtItsNewFormat
            );
        }
    }

    #[test]
    fn a_format_report_that_changed_nothing_the_stream_bound_at_rebinds_nothing() {
        assert_eq!(
            response_of_a_stream(
                NAMED,
                CoreAudioStreamDeviceChange::BoundDeviceFormatChanged,
                device_facts(true, false, Some(THE_BOUND_DEVICE)),
            ),
            CoreAudioStreamDeviceChangeResponse::StayOnTheBoundDevice
        );
    }

    const FIRST_INPUT_SAMPLE_NS: i64 = 1_000_000_000_000;

    /// The blocks a converted stream stamps from a device whose frames each
    /// take `host_ns_per_device_frame` on the host clock, with the converter
    /// keeping pace with its input as latency mode does. Each block is its
    /// stamp, its frames, and the stream frames produced before it.
    fn blocks_stamped_from_a_device(
        timeline: &mut ConvertedCaptureBlockTimeline,
        cycle_frames: u32,
        cycle_count: u64,
        host_ns_at_device_frame: impl Fn(u64) -> i64,
    ) -> Vec<(i64, u32, u64)> {
        let mut stream_frames_produced = 0u64;
        (0..cycle_count)
            .map(|cycle| {
                let first_device_frame = cycle * u64::from(cycle_frames);
                timeline.a_cycle_is_lent(host_ns_at_device_frame(first_device_frame), cycle_frames);
                let stream_frames_producible = (first_device_frame + u64::from(cycle_frames))
                    * u64::from(timeline.stream_sample_rate)
                    / u64::from(timeline.device_sample_rate);
                let converted_frame_count =
                    (stream_frames_producible - stream_frames_produced) as u32;
                let produced_before = stream_frames_produced;
                stream_frames_produced = stream_frames_producible;
                (
                    timeline.stamp_the_next_block(converted_frame_count),
                    converted_frame_count,
                    produced_before,
                )
            })
            .collect()
    }

    fn a_steady_device_at(device_sample_rate: u32) -> impl Fn(u64) -> i64 {
        move |device_frame| {
            FIRST_INPUT_SAMPLE_NS
                + duration_of_many_frames_in_ns(device_frame, device_sample_rate) as i64
        }
    }

    #[test]
    fn a_converted_streams_first_block_is_stamped_at_its_first_input_sample_less_the_latency() {
        let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(44_100, 48_000, 16);
        timeline.a_cycle_is_lent(FIRST_INPUT_SAMPLE_NS, 512);
        assert_eq!(
            timeline.stamp_the_next_block(557),
            FIRST_INPUT_SAMPLE_NS - 362_811,
            "16 frames at 44.1 kHz is 362.8 µs"
        );
    }

    /// On a steady device the stamps are exactly the first cycle's, advanced
    /// by the stream frames produced, less the converter's latency — and so
    /// back to back.
    #[test]
    fn a_steady_devices_converted_blocks_are_stamped_back_to_back_from_the_first_cycle() {
        let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(44_100, 48_000, 16);
        let blocks =
            blocks_stamped_from_a_device(&mut timeline, 512, 2_000, a_steady_device_at(44_100));
        let latency_ns = duration_of_frames_in_ns(16, 44_100);
        for &(stamp_ns, _, produced_before) in &blocks {
            let from_the_first_cycle_ns = FIRST_INPUT_SAMPLE_NS
                + duration_of_many_frames_in_ns(produced_before, 48_000) as i64
                - latency_ns;
            assert!(
                (stamp_ns - from_the_first_cycle_ns).abs() <= 2,
                "{stamp_ns} vs {from_the_first_cycle_ns}"
            );
        }
        for pair in blocks.windows(2) {
            let (earlier_stamp_ns, earlier_frames, _) = pair[0];
            let (later_stamp_ns, _, _) = pair[1];
            let gap_ns = later_stamp_ns
                - (earlier_stamp_ns + duration_of_frames_in_ns(earlier_frames, 48_000));
            assert!(gap_ns.abs() <= 3, "blocks overlap or part by {gap_ns} ns");
        }
    }

    /// Mental revert: anchor once and advance at the nominal rate, and after
    /// ten minutes on a device 100 ppm fast the stamps sit 60 ms off the host
    /// clock.
    #[test]
    fn a_device_clock_off_nominal_never_pulls_the_stamps_off_the_host_clock() {
        let host_ns_per_device_frame = 1e9 / 44_100.0 * (1.0 - 100e-6);
        let host_ns_at_device_frame = move |device_frame: u64| {
            FIRST_INPUT_SAMPLE_NS + (device_frame as f64 * host_ns_per_device_frame).round() as i64
        };
        let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(44_100, 48_000, 16);
        let ten_minutes_of_cycles = 600 * 44_100 / 512;
        let blocks = blocks_stamped_from_a_device(
            &mut timeline,
            512,
            ten_minutes_of_cycles,
            host_ns_at_device_frame,
        );
        let &(last_stamp_ns, _, produced_before) = blocks.last().expect("blocks were stamped");
        let input_frame_the_block_begins_at = produced_before as f64 * 44_100.0 / 48_000.0 - 16.0;
        let where_that_frame_is_on_the_host_clock_ns = FIRST_INPUT_SAMPLE_NS
            + (input_frame_the_block_begins_at * host_ns_per_device_frame).round() as i64;
        assert!(
            (last_stamp_ns - where_that_frame_is_on_the_host_clock_ns).abs() < 1_000,
            "stamped {last_stamp_ns}, captured at {where_that_frame_is_on_the_host_clock_ns}"
        );
    }

    /// A stop and restart, or cycles the device dropped, leave a real gap in
    /// the input; the stamps show it rather than running on as if none
    /// happened.
    #[test]
    fn a_gap_in_the_input_is_the_same_gap_in_the_stamps() {
        const GAP_NS: i64 = 250_000_000;
        let steady = a_steady_device_at(48_000);
        let after_a_gap_at_frame = 100 * 512;
        let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(48_000, 16_000, 18);
        let blocks = blocks_stamped_from_a_device(&mut timeline, 512, 200, move |device_frame| {
            steady(device_frame)
                + if device_frame >= after_a_gap_at_frame {
                    GAP_NS
                } else {
                    0
                }
        });
        let gaps_ns: Vec<i64> = blocks
            .windows(2)
            .map(|pair| pair[1].0 - (pair[0].0 + duration_of_frames_in_ns(pair[0].1, 16_000)))
            .collect();
        assert!((gaps_ns[99] - GAP_NS).abs() <= 3, "{}", gaps_ns[99]);
        assert!(
            gaps_ns
                .iter()
                .enumerate()
                .all(|(index, gap_ns)| index == 99 || gap_ns.abs() <= 3)
        );
    }

    /// Mental revert: count in `i64` nanoseconds, and a month of 192 kHz
    /// frames overflows it.
    #[test]
    fn a_month_long_converted_stream_still_stamps_exactly() {
        let a_month_of_device_frames = 30 * 24 * 3_600 * 192_000u64;
        let mut timeline = ConvertedCaptureBlockTimeline::for_a_new_binding(192_000, 48_000, 16);
        timeline.device_frames_lent = a_month_of_device_frames;
        timeline.stream_frames_produced = a_month_of_device_frames / 4;
        timeline.a_cycle_is_lent(FIRST_INPUT_SAMPLE_NS, 4_096);
        assert_eq!(
            timeline.stamp_the_next_block(1_024),
            FIRST_INPUT_SAMPLE_NS - duration_of_frames_in_ns(16, 192_000)
        );
    }

    fn no_device() -> CoreAudioDevice {
        CoreAudioDevice {
            object_id: kAudioObjectUnknown,
            uid: "NoDeviceAConverterTestNeeds".into(),
            name: "no device".into(),
        }
    }

    fn interleaved_f32_bytes_of(samples: &[f32]) -> Vec<u8> {
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect()
    }

    fn interleaved_f32_samples_of(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(4)
            .map(|scalar| f32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]]))
            .collect()
    }

    /// Each converted block's stamp and its interleaved samples, from cycles
    /// of `cycle_frames` lent on a steady device, carrying one full-scale
    /// impulse at `impulse_at_input_frame` in every channel.
    fn blocks_converted_from_an_impulse(
        device_own_format: AudioStreamFormat,
        stream_format: AudioStreamFormat,
        cycle_frames: u32,
        cycle_count: u32,
        impulse_at_input_frame: usize,
    ) -> Vec<(i64, Vec<f32>)> {
        let mut converter = CoreAudioCaptureFormatConverter::new(
            &no_device(),
            device_own_format,
            stream_format,
            cycle_frames,
        )
        .expect("AudioToolbox converts between two linear PCM float formats");
        let device_channels = device_own_format.channels as usize;
        let mut input = vec![0.0f32; (cycle_frames * cycle_count) as usize * device_channels];
        input[impulse_at_input_frame * device_channels..][..device_channels].fill(1.0);
        let blocks = std::cell::RefCell::new(Vec::new());
        for cycle in 0..cycle_count {
            let first_device_frame = u64::from(cycle * cycle_frames);
            let mut cycle_bytes = interleaved_f32_bytes_of(
                &input[first_device_frame as usize * device_channels..]
                    [..cycle_frames as usize * device_channels],
            );
            let outcome = converter.convert_the_cycle(
                &mut cycle_bytes,
                cycle_frames,
                a_steady_device_at(device_own_format.sample_rate)(first_device_frame),
                &|block: CapturedAudioBlockFromDevice<'_>| {
                    assert_eq!(
                        block.interleaved_sample_bytes.len(),
                        stream_format.interleaved_byte_count_for(block.sample_count)
                    );
                    blocks.borrow_mut().push((
                        block.first_sample_timestamp_ns,
                        interleaved_f32_samples_of(block.interleaved_sample_bytes),
                    ));
                },
            );
            assert_eq!(outcome, ConvertedInputCycle::HandedOff);
        }
        blocks.into_inner()
    }

    /// The loudest output frame's stamp, and every channel's value there.
    fn where_the_impulse_came_out(
        blocks: &[(i64, Vec<f32>)],
        stream_format: AudioStreamFormat,
    ) -> (i64, Vec<f32>) {
        let channels = stream_format.channels as usize;
        blocks
            .iter()
            .flat_map(|(stamp_ns, samples)| {
                samples
                    .chunks_exact(channels)
                    .enumerate()
                    .map(move |(frame, frame_samples)| {
                        (
                            stamp_ns
                                + duration_of_frames_in_ns(frame as u32, stream_format.sample_rate),
                            frame_samples.to_vec(),
                        )
                    })
            })
            .max_by(|(_, left), (_, right)| left[0].abs().total_cmp(&right[0].abs()))
            .expect("the converter produced frames")
    }

    /// The whole converter path without a device: an impulse lent at a known
    /// instant comes out stamped at that instant, to within one output frame.
    /// Mental revert: subtract no latency, and the 44.1 → 48 kHz impulse is
    /// stamped 16 input frames — 363 µs — late.
    #[test]
    fn an_impulse_through_the_capture_converter_is_stamped_where_it_was_captured() {
        const IMPULSE_AT_INPUT_FRAME: usize = 3_000;
        for (device_own_format, stream_format) in [
            (interleaved_float(44_100, 1), interleaved_float(48_000, 2)),
            (interleaved_float(48_000, 2), interleaved_float(16_000, 1)),
            (interleaved_float(16_000, 1), interleaved_float(48_000, 1)),
            (interleaved_float(96_000, 2), interleaved_float(48_000, 2)),
        ] {
            let blocks = blocks_converted_from_an_impulse(
                device_own_format,
                stream_format,
                512,
                12,
                IMPULSE_AT_INPUT_FRAME,
            );
            let (impulse_stamp_ns, impulse_frame) =
                where_the_impulse_came_out(&blocks, stream_format);
            let impulse_captured_at_ns = FIRST_INPUT_SAMPLE_NS
                + duration_of_frames_in_ns(
                    IMPULSE_AT_INPUT_FRAME as u32,
                    device_own_format.sample_rate,
                );
            let one_output_frame_ns = duration_of_frames_in_ns(1, stream_format.sample_rate);
            assert!(
                (impulse_stamp_ns - impulse_captured_at_ns).abs() <= one_output_frame_ns,
                "{device_own_format:?} → {stream_format:?}: stamped {impulse_stamp_ns}, captured \
                 at {impulse_captured_at_ns}"
            );
            assert!(
                impulse_frame.iter().all(|&sample| sample > 0.2),
                "every channel carries the impulse: {impulse_frame:?}"
            );
        }
    }

    #[test]
    fn a_converter_that_only_moves_channels_adds_no_latency_and_copies_mono_to_both() {
        let stream_format = interleaved_float(48_000, 2);
        let blocks = blocks_converted_from_an_impulse(
            interleaved_float(48_000, 1),
            stream_format,
            512,
            4,
            700,
        );
        assert_eq!(blocks[0].0, FIRST_INPUT_SAMPLE_NS);
        let (impulse_stamp_ns, impulse_frame) = where_the_impulse_came_out(&blocks, stream_format);
        let impulse_captured_at_ns = FIRST_INPUT_SAMPLE_NS + duration_of_frames_in_ns(700, 48_000);
        assert!(
            (impulse_stamp_ns - impulse_captured_at_ns).abs() <= 1,
            "stamped {impulse_stamp_ns}, captured at {impulse_captured_at_ns}"
        );
        assert_eq!(impulse_frame, [1.0, 1.0]);
    }
}

/// A capture stream's microphone permission flow, driven by a scripted privacy
/// gate against the real default input. Audio tier: the unit it binds once
/// "allowed" is real, so it needs an input device and the microphone already
/// allowed for the application running it. It captures and never plays.
#[cfg(all(test, feature = "hardware-tests"))]
mod microphone_permission_flow_against_the_default_input {
    use super::*;
    use crate::apple::permissions::{
        CaptureDeviceAuthorizationAnswer, CaptureDeviceAuthorizationStatus,
        microphone_access_for_a_capture_hardware_test,
    };
    use crate::core::context::{AudioClockConfig, SoftwareAudioClock};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    /// Long against a device period, so a parked stream that was delivering
    /// would have been seen to.
    const HOW_LONG_A_PARKED_STREAM_IS_WATCHED: Duration = Duration::from_millis(500);

    const FIRST_BLOCK_DEADLINE: Duration = Duration::from_secs(10);

    /// A privacy gate that has not decided and keeps the request until the
    /// test answers it.
    #[derive(Default)]
    struct AUserWhoAnswersOnCue {
        held_answer: Mutex<Option<CaptureDeviceAuthorizationAnswer>>,
    }

    impl AUserWhoAnswersOnCue {
        fn answer(&self, granted: bool) {
            let answer = self
                .held_answer
                .lock()
                .take()
                .expect("the stream asked the user when it opened");
            answer(granted);
        }
    }

    impl CaptureDeviceAuthorizationAuthority for AUserWhoAnswersOnCue {
        fn gated_capture_device(&self) -> PrivacyGatedCaptureDevice {
            PrivacyGatedCaptureDevice::Microphone
        }

        fn authorization_status(&self) -> CaptureDeviceAuthorizationStatus {
            CaptureDeviceAuthorizationStatus::NotDetermined
        }

        fn request_authorization(&self, answer: CaptureDeviceAuthorizationAnswer) {
            *self.held_answer.lock() = Some(answer);
        }
    }

    /// A request for the default input, or `None` when this Mac has none. The
    /// real gate must already allow the microphone: "allowed" binds a real
    /// unit, and against a real prompt that bind would wait on a person.
    #[allow(clippy::disallowed_macros)]
    fn a_request_for_the_default_input() -> Option<AudioDeviceStreamRequest> {
        if default_device_object_id(CoreAudioStreamDirection::Capture).is_none() {
            println!("cannot run: CoreAudio lists no default input device");
            return None;
        }
        if let Err(instruction) = microphone_access_for_a_capture_hardware_test() {
            panic!("{instruction}");
        }
        Some(AudioDeviceStreamRequest {
            device_id: None,
            deviceless_pacing_clock: Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(
                48_000, 512,
            ))),
        })
    }

    fn is_awaiting_the_users_answer(stream: &CoreAudioCaptureStream) -> bool {
        matches!(
            stream.capture_control.lock().microphone_access,
            MicrophoneAccessForTheStream::AwaitingTheUsersAnswer { .. }
        )
    }

    fn has_bound_a_unit(stream: &CoreAudioCaptureStream) -> bool {
        matches!(
            stream.capture_control.lock().microphone_access,
            MicrophoneAccessForTheStream::Granted(_)
        )
    }

    /// Binding a unit with input enabled blocks until the user answers, so a
    /// stream that bound before the answer would hang its opener. Mental
    /// revert: bind at open whatever the gate says, and the first two
    /// assertions fail.
    #[test]
    fn a_stream_waiting_on_the_user_binds_no_unit_and_delivers_once_allowed() {
        let Some(request) = a_request_for_the_default_input() else {
            return;
        };
        let user = AUserWhoAnswersOnCue::default();
        let mut stream = CoreAudioCaptureStream::open(&request, &user)
            .expect("a stream opens without waiting on the user");
        assert!(
            is_awaiting_the_users_answer(&stream),
            "no unit is bound before the user has answered"
        );

        let (block_sender, block_receiver) = mpsc::channel();
        stream
            .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
                let _ = block_sender.send(block.sample_count);
            }))
            .expect("a start while the user is being asked parks rather than failing");
        assert!(
            is_awaiting_the_users_answer(&stream),
            "starting parks the hand-off and still binds nothing"
        );
        assert_eq!(
            block_receiver.recv_timeout(HOW_LONG_A_PARKED_STREAM_IS_WATCHED),
            Err(mpsc::RecvTimeoutError::Timeout),
            "a stream still waiting on the user delivers nothing"
        );

        user.answer(true);
        assert!(has_bound_a_unit(&stream), "the answer binds the unit");
        let first_block_sample_count = block_receiver
            .recv_timeout(FIRST_BLOCK_DEADLINE)
            .expect("the parked hand-off starts receiving blocks once the user allows it");
        assert!(first_block_sample_count > 0);
        assert!(
            stream
                .liveness_report()
                .failure_that_ended_the_stream()
                .is_none()
        );
        stream.stop_delivering().expect("delivery stops");
    }

    #[test]
    fn a_stream_the_user_refused_reports_the_microphone_refusal_and_never_hands_off() {
        let Some(request) = a_request_for_the_default_input() else {
            return;
        };
        let user = AUserWhoAnswersOnCue::default();
        let mut stream = CoreAudioCaptureStream::open(&request, &user)
            .expect("a stream opens without waiting on the user");
        let hand_off_calls = Arc::new(AtomicUsize::new(0));
        stream
            .start_delivering_to(Box::new({
                let hand_off_calls = Arc::clone(&hand_off_calls);
                move |_block: CapturedAudioBlockFromDevice<'_>| {
                    hand_off_calls.fetch_add(1, Ordering::SeqCst);
                }
            }))
            .expect("a start while the user is being asked parks rather than failing");

        user.answer(false);

        let failure = stream
            .liveness_report()
            .failure_that_ended_the_stream()
            .expect("a refusal ends the stream through its liveness report");
        assert!(
            failure.to_string().contains("Microphone"),
            "the refusal names the Microphone setting: {failure}"
        );
        assert!(!has_bound_a_unit(&stream), "a refused stream binds nothing");
        assert_eq!(hand_off_calls.load(Ordering::SeqCst), 0);

        let restart = stream.start_delivering_to(Box::new(|_block| {}));
        let refusal = restart.expect_err("starting a stream the user refused fails");
        assert!(refusal.to_string().contains("Microphone"), "{refusal}");
    }
}

/// Rebinding — and converting where a stream's format is not its device's —
/// on the real default devices, through the path a device change takes. Audio
/// tier, and silent: it captures from the default input and plays only zeros
/// to the default output. Moving the system default itself needs a person
/// plugging something in, so these rebind to the device already bound.
#[cfg(all(test, feature = "hardware-tests"))]
mod following_the_device_against_the_default_devices {
    use super::*;
    use crate::apple::permissions::microphone_access_for_a_capture_hardware_test;
    use crate::core::context::{AudioClockConfig, SoftwareAudioClock};
    use std::sync::mpsc;
    use std::time::Duration;

    const DELIVERY_DEADLINE: Duration = Duration::from_secs(10);

    /// Stamps of one binding's consecutive blocks part or overlap by no more
    /// than this — far under the shortest cycle a device runs, so a lost
    /// cycle shows.
    const CONTIGUOUS_STAMP_TOLERANCE_NS: i64 = 500_000;

    /// How long after its last sample was captured a block may arrive.
    const LONGEST_DELIVERY_DELAY_NS: i64 = 250_000_000;

    /// How far before its stamp says a block's last sample was captured it
    /// may arrive: the device's timing model, not a block from the future.
    const TIMING_MODEL_ALLOWANCE_NS: i64 = 2_000_000;

    #[allow(clippy::disallowed_macros)]
    fn print_for_the_evidence_record(line: impl std::fmt::Display) {
        println!("{line}");
    }

    fn the_engine_logs_into_this_tests_output() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        let _ = tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with(tracing_subscriber::fmt::layer().with_test_writer())
            .try_init();
    }

    fn now_ns() -> i64 {
        MediaClock::now().as_nanos() as i64
    }

    /// The system default device in `direction` and its own format, or `None`
    /// — said so — where there is none.
    fn the_default_device(
        direction: CoreAudioStreamDirection,
    ) -> Option<(CoreAudioDevice, AudioStreamFormat)> {
        the_engine_logs_into_this_tests_output();
        let Some(object_id) = default_device_object_id(direction) else {
            print_for_the_evidence_record(format!(
                "cannot run: CoreAudio lists no default {} device",
                direction.lowercase_direction_name()
            ));
            return None;
        };
        let device = describe_device(object_id);
        let device_own_format =
            stream_format_of(&device, direction).expect("the default device reports its format");
        print_for_the_evidence_record(format!(
            "default {} device: {device} at {}",
            direction.lowercase_direction_name(),
            rate_and_channels_of(device_own_format)
        ));
        Some((device, device_own_format))
    }

    fn the_microphone_must_be_allowed() {
        if let Err(instruction) = microphone_access_for_a_capture_hardware_test() {
            panic!("{instruction}");
        }
    }

    fn a_request_for_the_default_device() -> AudioDeviceStreamRequest {
        AudioDeviceStreamRequest {
            device_id: None,
            deviceless_pacing_clock: Arc::new(SoftwareAudioClock::new(AudioClockConfig::new(
                48_000, 512,
            ))),
        }
    }

    /// A format differing from `device_own_format` in both rate and channel
    /// count.
    fn a_format_the_device_does_not_carry(
        device_own_format: AudioStreamFormat,
    ) -> AudioStreamFormat {
        AudioStreamFormat {
            sample_rate: if device_own_format.sample_rate == 44_100 {
                48_000
            } else {
                44_100
            },
            channels: if device_own_format.channels == 1 {
                2
            } else {
                1
            },
            sample_format: AudioSampleFormat::F32,
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct CapturedBlockAsRecorded {
        sample_count: u32,
        first_sample_timestamp_ns: i64,
        received_at_ns: i64,
        interleaved_byte_count: usize,
        has_a_nonzero_sample: bool,
        every_frame_has_equal_channels: bool,
    }

    fn start_recording_blocks(
        stream: &mut CoreAudioCaptureStream,
    ) -> mpsc::Receiver<CapturedBlockAsRecorded> {
        let channels = stream.stream_format().channels as usize;
        let (block_sender, block_receiver) = mpsc::channel();
        stream
            .start_delivering_to(Box::new(move |block: CapturedAudioBlockFromDevice<'_>| {
                let received_at_ns = now_ns();
                let samples: Vec<f32> = block
                    .interleaved_sample_bytes
                    .chunks_exact(4)
                    .map(|scalar| f32::from_le_bytes([scalar[0], scalar[1], scalar[2], scalar[3]]))
                    .collect();
                let _ = block_sender.send(CapturedBlockAsRecorded {
                    sample_count: block.sample_count,
                    first_sample_timestamp_ns: block.first_sample_timestamp_ns,
                    received_at_ns,
                    interleaved_byte_count: block.interleaved_sample_bytes.len(),
                    has_a_nonzero_sample: samples.iter().any(|&sample| sample != 0.0),
                    every_frame_has_equal_channels: samples
                        .chunks_exact(channels)
                        .all(|frame| frame.iter().all(|&sample| sample == frame[0])),
                });
            }))
            .expect("delivery starts");
        block_receiver
    }

    fn blocks_covering(
        block_receiver: &mpsc::Receiver<CapturedBlockAsRecorded>,
        frame_count: u64,
    ) -> Vec<CapturedBlockAsRecorded> {
        let mut blocks = Vec::new();
        let mut frames_covered = 0u64;
        while frames_covered < frame_count {
            let block = block_receiver
                .recv_timeout(DELIVERY_DEADLINE)
                .expect("the stream keeps delivering");
            frames_covered += u64::from(block.sample_count);
            blocks.push(block);
        }
        blocks
    }

    /// Where each block begins against where the one before it ended.
    fn gaps_between_consecutive_blocks_ns(
        blocks: &[CapturedBlockAsRecorded],
        sample_rate: u32,
    ) -> Vec<i64> {
        blocks
            .windows(2)
            .map(|pair| {
                pair[1].first_sample_timestamp_ns
                    - (pair[0].first_sample_timestamp_ns
                        + duration_of_frames_in_ns(pair[0].sample_count, sample_rate))
            })
            .collect()
    }

    /// A binding's blocks follow each other back to back, and each arrives
    /// after — and soon after — its last sample was captured.
    fn assert_one_bindings_blocks_are_continuous_on_the_host_clock(
        blocks: &[CapturedBlockAsRecorded],
        sample_rate: u32,
        which_binding: &str,
    ) {
        let largest_gap_ns = gaps_between_consecutive_blocks_ns(blocks, sample_rate)
            .into_iter()
            .map(i64::abs)
            .max()
            .unwrap_or(0);
        let delivery_delays_ns: Vec<i64> = blocks
            .iter()
            .map(|block| {
                block.received_at_ns
                    - (block.first_sample_timestamp_ns
                        + duration_of_frames_in_ns(block.sample_count, sample_rate))
            })
            .collect();
        let frames: u64 = blocks
            .iter()
            .map(|block| u64::from(block.sample_count))
            .sum();
        print_for_the_evidence_record(format!(
            "{which_binding}: {} blocks, {frames} frames at {sample_rate} Hz, largest |gap| between \
             consecutive stamps {:.1} µs, delivery delay {:.2}..{:.2} ms",
            blocks.len(),
            largest_gap_ns as f64 / 1e3,
            *delivery_delays_ns.iter().min().unwrap_or(&0) as f64 / 1e6,
            *delivery_delays_ns.iter().max().unwrap_or(&0) as f64 / 1e6,
        ));
        assert!(
            largest_gap_ns <= CONTIGUOUS_STAMP_TOLERANCE_NS,
            "{which_binding}: consecutive stamps part by up to {largest_gap_ns} ns"
        );
        for delivery_delay_ns in delivery_delays_ns {
            assert!(
                (-TIMING_MODEL_ALLOWANCE_NS..LONGEST_DELIVERY_DELAY_NS)
                    .contains(&delivery_delay_ns),
                "{which_binding}: a block arrived {delivery_delay_ns} ns after its stamp says its \
                 last sample was captured"
            );
        }
    }

    /// The blocks either side of a rebind: one gap, where the unit was
    /// stopped, splits two runs that are each continuous; the stamps move
    /// forward across it and the stream delivers on after it.
    fn assert_two_continuous_bindings_parted_at_the_rebind(
        blocks: &[CapturedBlockAsRecorded],
        sample_rate: u32,
        rebind_began_ns: i64,
    ) {
        let gaps_ns = gaps_between_consecutive_blocks_ns(blocks, sample_rate);
        let (last_block_before_the_rebind, rebind_gap_ns) = gaps_ns
            .iter()
            .copied()
            .enumerate()
            .max_by_key(|&(_, gap_ns)| gap_ns)
            .expect("blocks either side of the rebind");
        let (before, after) = blocks.split_at(last_block_before_the_rebind + 1);
        print_for_the_evidence_record(format!(
            "the rebind parted the stamps by {:.2} ms",
            rebind_gap_ns as f64 / 1e6
        ));
        assert!(
            rebind_gap_ns > CONTIGUOUS_STAMP_TOLERANCE_NS && rebind_gap_ns < 1_000_000_000,
            "a rebind stops the unit for a moment, never a second: {rebind_gap_ns} ns"
        );
        assert!(
            after[0].received_at_ns >= rebind_began_ns,
            "the stamps part where the rebind happened"
        );
        assert_one_bindings_blocks_are_continuous_on_the_host_clock(
            before,
            sample_rate,
            "before the rebind",
        );
        assert_one_bindings_blocks_are_continuous_on_the_host_clock(
            after,
            sample_rate,
            "after the rebind",
        );
        let frames_after: u32 = after.iter().map(|block| block.sample_count).sum();
        assert!(
            u64::from(frames_after) * 10 >= u64::from(sample_rate) * 4,
            "blocks keep arriving after the rebind: {frames_after} frames"
        );
    }

    fn force_a_capture_rebind_to(stream: &CoreAudioCaptureStream, device: &CoreAudioDevice) {
        let device_own_format = stream_format_of(device, CoreAudioStreamDirection::Capture)
            .expect("the device reports its format");
        rebind_the_stream_to(
            &mut *stream.capture_control.lock(),
            device,
            device_own_format,
        );
    }

    fn a_converter_runs_on(stream: &CoreAudioCaptureStream) -> bool {
        match &stream.capture_control.lock().microphone_access {
            MicrophoneAccessForTheStream::Granted(capture_unit) => capture_unit
                .callback_context
                .delivery
                .lock()
                .format_converter
                .is_some(),
            _ => false,
        }
    }

    /// The owner's ruling: a stream with no device_id follows the default, and
    /// a rebind keeps the stream's hand-off and its stamps' clock. Mental
    /// revert: restart without the installed hand-off, and nothing arrives
    /// after the rebind.
    #[test]
    fn an_unnamed_capture_stream_rebound_to_its_default_keeps_delivering_with_continuous_stamps() {
        let Some((default_input, _)) = the_default_device(CoreAudioStreamDirection::Capture) else {
            return;
        };
        the_microphone_must_be_allowed();
        let mut stream = CoreAudioCaptureStream::open(
            &a_request_for_the_default_device(),
            &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Microphone),
        )
        .expect("an unnamed capture stream opens on the default input");
        assert!(
            stream.capture_control.lock().device_binding.device_policy
                == CoreAudioStreamDevicePolicy::FollowsTheSystemDefault
        );
        let sample_rate = stream.stream_format().sample_rate;
        let block_receiver = start_recording_blocks(&mut stream);
        let mut blocks = blocks_covering(&block_receiver, u64::from(sample_rate) / 2);

        let rebind_began_ns = now_ns();
        force_a_capture_rebind_to(&stream, &default_input);
        assert_eq!(
            stream.liveness_report().failure_that_ended_the_stream(),
            None
        );
        blocks.extend(blocks_covering(&block_receiver, u64::from(sample_rate) / 2));
        stream.stop_delivering().expect("delivery stops");

        assert_two_continuous_bindings_parted_at_the_rebind(&blocks, sample_rate, rebind_began_ns);
        assert_eq!(
            stream.liveness_report().failure_that_ended_the_stream(),
            None
        );
    }

    /// The path a real change takes: every listener registers, and a change run
    /// on the stream's control queue — where the HAL runs the listener blocks —
    /// reaches the stream, which keeps delivering.
    #[test]
    fn an_unnamed_capture_streams_listeners_register_and_a_change_reaches_it_through_its_queue() {
        let Some(_) = the_default_device(CoreAudioStreamDirection::Capture) else {
            return;
        };
        the_microphone_must_be_allowed();
        let mut stream = CoreAudioCaptureStream::open(
            &a_request_for_the_default_device(),
            &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Microphone),
        )
        .expect("an unnamed capture stream opens on the default input");
        let sample_rate = stream.stream_format().sample_rate;
        let block_receiver = start_recording_blocks(&mut stream);
        blocks_covering(&block_receiver, u64::from(sample_rate) / 4);

        let (handle_a_device_change, stream_control_queue) = {
            let control = stream.capture_control.lock();
            assert!(
                control
                    .device_binding
                    .system_default_device_listener
                    .is_some(),
                "an unnamed stream listens for the system default moving"
            );
            assert_eq!(
                control.device_binding.bound_device_listeners.len(),
                3,
                "liveness, nominal rate and stream configuration are all listened for"
            );
            (
                Arc::clone(&control.device_binding.handle_a_device_change),
                control.device_binding.stream_control_queue.clone(),
            )
        };
        for change in [
            CoreAudioStreamDeviceChange::SystemDefaultDeviceMoved,
            CoreAudioStreamDeviceChange::BoundDeviceFormatChanged,
            CoreAudioStreamDeviceChange::BoundDeviceLivenessChanged,
        ] {
            let handle_a_device_change = Arc::clone(&handle_a_device_change);
            stream_control_queue.exec_async(move || handle_a_device_change(change));
        }
        stream_control_queue.exec_sync(|| {});

        assert_eq!(
            stream.liveness_report().failure_that_ended_the_stream(),
            None,
            "a change that left the default and the device as they were ends nothing"
        );
        let after_the_changes = blocks_covering(&block_receiver, u64::from(sample_rate) / 4);
        assert!(!after_the_changes.is_empty(), "the stream keeps delivering");
        stream.stop_delivering().expect("delivery stops");
    }

    /// A stream whose device carries another rate and channel count — as
    /// AirPods' microphone does once the headset profile engages — is
    /// converted to the stream's format, with the room's signal intact and
    /// the stamps continuous on the host clock, through a rebind too.
    #[test]
    fn a_capture_stream_at_a_format_its_device_does_not_carry_converts_with_continuous_stamps() {
        let Some((default_input, device_own_format)) =
            the_default_device(CoreAudioStreamDirection::Capture)
        else {
            return;
        };
        the_microphone_must_be_allowed();
        let stream_format = a_format_the_device_does_not_carry(device_own_format);
        let mut stream = CoreAudioCaptureStream::open_on(
            default_input.clone(),
            stream_format,
            CoreAudioStreamDevicePolicy::FollowsTheSystemDefault,
            &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Microphone),
        )
        .expect("a capture stream opens at a format its device does not carry");
        assert!(
            a_converter_runs_on(&stream),
            "the unit renders the device's own format and a converter takes it to the stream's"
        );
        let block_receiver = start_recording_blocks(&mut stream);
        let mut blocks = blocks_covering(&block_receiver, u64::from(stream_format.sample_rate));

        let rebind_began_ns = now_ns();
        force_a_capture_rebind_to(&stream, &default_input);
        assert!(
            a_converter_runs_on(&stream),
            "the rebind made a converter again"
        );
        blocks.extend(blocks_covering(
            &block_receiver,
            u64::from(stream_format.sample_rate) / 2,
        ));
        stream.stop_delivering().expect("delivery stops");

        print_for_the_evidence_record(format!(
            "converted {} to {}",
            rate_and_channels_of(device_own_format),
            rate_and_channels_of(stream_format)
        ));
        for block in &blocks {
            assert_eq!(
                block.interleaved_byte_count,
                stream_format.interleaved_byte_count_for(block.sample_count),
                "every block carries the stream's channel count"
            );
        }
        assert!(
            blocks.iter().any(|block| block.has_a_nonzero_sample),
            "the room reaches the stream through the converter"
        );
        if device_own_format.channels == 1 {
            assert!(
                blocks
                    .iter()
                    .all(|block| block.every_frame_has_equal_channels),
                "a mono device's one channel is copied to each of the stream's"
            );
        }
        assert_two_continuous_bindings_parted_at_the_rebind(
            &blocks,
            stream_format.sample_rate,
            rebind_began_ns,
        );
        assert_eq!(
            stream.liveness_report().failure_that_ended_the_stream(),
            None
        );
    }

    /// A rebind CoreAudio refuses ends the stream: the failure names the
    /// device and the status, and the stream starts no more. An output-only
    /// device cannot take an input unit.
    #[test]
    fn a_capture_rebind_coreaudio_refuses_ends_the_stream_naming_the_device() {
        let Some((default_input, device_own_format)) =
            the_default_device(CoreAudioStreamDirection::Capture)
        else {
            return;
        };
        let Some((default_output, _)) = the_default_device(CoreAudioStreamDirection::Playback)
        else {
            return;
        };
        if channel_count_of(default_output.object_id, CoreAudioStreamDirection::Capture) > 0 {
            print_for_the_evidence_record(
                "cannot run: the default output device also carries input channels",
            );
            return;
        }
        the_microphone_must_be_allowed();
        let mut stream = CoreAudioCaptureStream::open(
            &a_request_for_the_default_device(),
            &AvFoundationCaptureDeviceAuthorizationAuthority(PrivacyGatedCaptureDevice::Microphone),
        )
        .expect("an unnamed capture stream opens on the default input");
        let block_receiver = start_recording_blocks(&mut stream);
        blocks_covering(
            &block_receiver,
            u64::from(device_own_format.sample_rate) / 10,
        );

        rebind_the_stream_to(
            &mut *stream.capture_control.lock(),
            &default_output,
            device_own_format,
        );

        let failure = stream
            .liveness_report()
            .failure_that_ended_the_stream()
            .expect("a refused rebind ends the stream");
        print_for_the_evidence_record(format!("recorded: {failure}"));
        let failure = failure.to_string();
        assert!(failure.contains(&default_output.uid), "{failure}");
        assert!(failure.contains(&default_input.uid), "{failure}");
        assert!(failure.contains("OSStatus"), "{failure}");
        let restart = stream.start_delivering_to(Box::new(|_block| {}));
        assert!(
            restart.is_err(),
            "a stream a refused rebind ended starts no more"
        );
    }

    fn start_answering_with_silence(
        stream: &mut CoreAudioPlaybackStream,
    ) -> mpsc::Receiver<(u32, usize, i64)> {
        let (request_sender, request_receiver) = mpsc::channel();
        stream
            .start_requesting_from(Box::new(
                move |requested: AudioBlockRequestedByDevice<'_>| {
                    requested.interleaved_sample_bytes_to_fill.fill(0);
                    let _ = request_sender.send((
                        requested.sample_count,
                        requested.interleaved_sample_bytes_to_fill.len(),
                        now_ns(),
                    ));
                },
            ))
            .expect("the device starts asking");
        request_receiver
    }

    fn requests_covering(
        request_receiver: &mpsc::Receiver<(u32, usize, i64)>,
        frame_count: u64,
        received_after_ns: i64,
    ) -> Vec<(u32, usize, i64)> {
        let mut requests = Vec::new();
        let mut frames_covered = 0u64;
        while frames_covered < frame_count {
            let request = request_receiver
                .recv_timeout(DELIVERY_DEADLINE)
                .expect("the device keeps asking");
            if request.2 > received_after_ns {
                frames_covered += u64::from(request.0);
                requests.push(request);
            }
        }
        requests
    }

    /// AUHAL's output side converts the client's format to the device's, so
    /// a playback stream is asked at its own rate and channel count whatever
    /// the device runs at. It plays only zeros.
    #[test]
    fn a_playback_stream_at_a_format_its_device_does_not_carry_is_asked_at_its_own_rate() {
        let Some((default_output, device_own_format)) =
            the_default_device(CoreAudioStreamDirection::Playback)
        else {
            return;
        };
        let stream_format = a_format_the_device_does_not_carry(device_own_format);
        let mut stream = CoreAudioPlaybackStream::open_on(
            default_output,
            stream_format,
            CoreAudioStreamDevicePolicy::PinnedToTheNamedDevice,
        )
        .expect("AUHAL takes a client format its device does not run at");
        let request_receiver = start_answering_with_silence(&mut stream);
        let requests = requests_covering(
            &request_receiver,
            u64::from(stream_format.sample_rate) * 3 / 2,
            i64::MIN,
        );
        stream.stop_requesting().expect("requests stop");

        for &(sample_count, byte_count, _) in &requests {
            assert_eq!(
                byte_count,
                stream_format.interleaved_byte_count_for(sample_count)
            );
        }
        let (_, _, first_request_at_ns) = requests[0];
        let (_, _, last_request_at_ns) = requests[requests.len() - 1];
        let frames_after_the_first: u64 = requests[1..]
            .iter()
            .map(|&(sample_count, _, _)| u64::from(sample_count))
            .sum();
        let asked_rate_hz =
            frames_after_the_first as f64 * 1e9 / (last_request_at_ns - first_request_at_ns) as f64;
        let request_sizes: std::collections::BTreeSet<u32> = requests
            .iter()
            .map(|&(sample_count, _, _)| sample_count)
            .collect();
        print_for_the_evidence_record(format!(
            "played {} as zeros to a device at {}: asked at {asked_rate_hz:.0} Hz, request sizes \
             {request_sizes:?}",
            rate_and_channels_of(stream_format),
            rate_and_channels_of(device_own_format)
        ));
        let rate_error = (asked_rate_hz / f64::from(stream_format.sample_rate) - 1.0).abs();
        assert!(
            rate_error < 0.02,
            "asked at {asked_rate_hz:.0} Hz for a {} Hz stream",
            stream_format.sample_rate
        );
        assert_eq!(
            stream.liveness_report().failure_that_ended_the_stream(),
            None
        );
    }

    /// Mental revert: rebind without restarting a running unit, and the
    /// device never asks again.
    #[test]
    fn an_unnamed_playback_stream_rebound_to_its_default_keeps_being_asked_for_samples() {
        let Some((default_output, device_own_format)) =
            the_default_device(CoreAudioStreamDirection::Playback)
        else {
            return;
        };
        let mut stream = CoreAudioPlaybackStream::open(&a_request_for_the_default_device())
            .expect("an unnamed playback stream opens on the default output");
        let request_receiver = start_answering_with_silence(&mut stream);
        requests_covering(
            &request_receiver,
            u64::from(device_own_format.sample_rate) / 4,
            i64::MIN,
        );

        rebind_the_stream_to(
            &mut *stream.playback_control.lock(),
            &default_output,
            device_own_format,
        );
        let rebind_returned_ns = now_ns();
        assert_eq!(
            stream.liveness_report().failure_that_ended_the_stream(),
            None
        );
        let requests_after_the_rebind = requests_covering(
            &request_receiver,
            u64::from(device_own_format.sample_rate) / 4,
            rebind_returned_ns,
        );
        stream.stop_requesting().expect("requests stop");
        print_for_the_evidence_record(format!(
            "after the rebind: {} requests of zeros",
            requests_after_the_rebind.len()
        ));
        assert_eq!(
            stream.liveness_report().failure_that_ended_the_stream(),
            None
        );
    }
}
