// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio backend chain's Apple arm: CoreAudio, through the AUHAL audio unit.
//!
//! Each stream owns one `kAudioUnitSubType_HALOutput` unit bound to one device,
//! with I/O enabled in the stream's direction only. The device's I/O thread is
//! the cadence source, and a block's stamp is the device's `mHostTime` — the
//! `mach_absolute_time` domain every other timestamp on Apple lives in.

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Weak};

use objc2_audio_toolbox::{
    AURenderCallback, AURenderCallbackStruct, AudioComponentDescription, AudioComponentFindNext,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioOutputUnitStart,
    AudioOutputUnitStop, AudioUnit, AudioUnitGetProperty, AudioUnitInitialize, AudioUnitRender,
    AudioUnitRenderActionFlags, AudioUnitSetProperty, AudioUnitUninitialize,
    kAudioOutputUnitProperty_CurrentDevice, kAudioOutputUnitProperty_EnableIO,
    kAudioOutputUnitProperty_SetInputCallback, kAudioUnitManufacturer_Apple,
    kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global, kAudioUnitScope_Input,
    kAudioUnitScope_Output, kAudioUnitSubType_HALOutput, kAudioUnitType_Output,
};
use objc2_core_audio::{
    AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertyScope,
    AudioObjectPropertySelector, AudioObjectRemovePropertyListener,
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
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, AudioTimeStampFlags,
    AudioValueRange, kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
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

/// The format a stream opens at: the device's own rate and channel count, as
/// interleaved 32-bit floats — so AUHAL never resamples, which it cannot do on
/// input at all.
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

/// An AUHAL unit bound to one device in one direction. Dropping it stops,
/// uninitialises and disposes the unit, after which no callback runs.
struct CoreAudioHalOutputUnit {
    audio_unit: AudioUnit,
    is_running: bool,
}

// SAFETY: an AudioUnit handle may be driven from any thread; every call on it
// here is made through `&mut self` or under the owning stream's lock.
unsafe impl Send for CoreAudioHalOutputUnit {}

impl CoreAudioHalOutputUnit {
    fn new_bound_to(
        device: &CoreAudioDevice,
        direction: CoreAudioStreamDirection,
        stream_format: AudioStreamFormat,
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
        let direction_name = direction.lowercase_direction_name();
        let refused = |what_the_device_would_not_do: &str, status: i32| {
            device.refused_with_status(
                &format!("refused {what_the_device_would_not_do} for {direction_name}"),
                status,
            )
        };

        let enable_capture = u32::from(direction == CoreAudioStreamDirection::Capture);
        let enable_playback = u32::from(direction == CoreAudioStreamDirection::Playback);
        unit.set_property(
            kAudioOutputUnitProperty_EnableIO,
            kAudioUnitScope_Input,
            AUHAL_INPUT_ELEMENT,
            &enable_capture,
        )
        .map_err(|status| refused("enabling input", status))?;
        unit.set_property(
            kAudioOutputUnitProperty_EnableIO,
            kAudioUnitScope_Output,
            AUHAL_OUTPUT_ELEMENT,
            &enable_playback,
        )
        .map_err(|status| refused("enabling output", status))?;
        unit.set_property(
            kAudioOutputUnitProperty_CurrentDevice,
            kAudioUnitScope_Global,
            AUHAL_OUTPUT_ELEMENT,
            &device.object_id,
        )
        .map_err(|status| refused("binding the unit", status))?;
        unit.set_property(
            kAudioUnitProperty_StreamFormat,
            direction.auhal_client_side_scope(),
            direction.auhal_element(),
            &interleaved_f32_description_of(stream_format),
        )
        .map_err(|status| refused("its own format as interleaved float", status))?;
        Ok(unit)
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

/// What a device-liveness listener needs, at an address that outlives it.
struct CoreAudioDeviceLivenessListenerContext {
    device_uid: String,
    direction: CoreAudioStreamDirection,
    failure_recorder: DeviceStreamFailureRecorder,
}

/// A `kAudioDevicePropertyDeviceIsAlive` listener that records the stream's
/// ending failure when its device goes away. Dropping it unregisters it.
struct CoreAudioDeviceLivenessWatch {
    device_object_id: AudioObjectID,
    listener_context: Box<CoreAudioDeviceLivenessListenerContext>,
}

impl CoreAudioDeviceLivenessWatch {
    fn watch(
        device: &CoreAudioDevice,
        direction: CoreAudioStreamDirection,
        failure_recorder: DeviceStreamFailureRecorder,
    ) -> Self {
        let watch = Self {
            device_object_id: device.object_id,
            listener_context: Box::new(CoreAudioDeviceLivenessListenerContext {
                device_uid: device.uid.clone(),
                direction,
                failure_recorder,
            }),
        };
        let address = property_address(
            kAudioDevicePropertyDeviceIsAlive,
            kAudioObjectPropertyScopeGlobal,
        );
        // SAFETY: the context is boxed and unregistered in `Drop` before it is freed.
        let status = unsafe {
            AudioObjectAddPropertyListener(
                watch.device_object_id,
                NonNull::from(&address),
                Some(device_liveness_changed),
                watch.listener_context_pointer(),
            )
        };
        if status != NO_ERR {
            tracing::warn!(
                device = %device,
                status = %osstatus_text(status),
                "CoreAudio audio arm: could not watch the device for removal; a device that \
                 disappears will go silent without being reported"
            );
        }
        watch
    }

    fn listener_context_pointer(&self) -> *mut c_void {
        (self.listener_context.as_ref() as *const CoreAudioDeviceLivenessListenerContext)
            .cast_mut()
            .cast()
    }
}

impl Drop for CoreAudioDeviceLivenessWatch {
    fn drop(&mut self) {
        let address = property_address(
            kAudioDevicePropertyDeviceIsAlive,
            kAudioObjectPropertyScopeGlobal,
        );
        // SAFETY: the same listener and context `watch` registered.
        unsafe {
            AudioObjectRemovePropertyListener(
                self.device_object_id,
                NonNull::from(&address),
                Some(device_liveness_changed),
                self.listener_context_pointer(),
            );
        }
    }
}

unsafe extern "C-unwind" fn device_liveness_changed(
    device_object_id: AudioObjectID,
    _address_count: u32,
    _addresses: NonNull<AudioObjectPropertyAddress>,
    listener_context: *mut c_void,
) -> i32 {
    // SAFETY: registered with a `CoreAudioDeviceLivenessListenerContext` that
    // outlives the registration.
    let context = unsafe { &*listener_context.cast::<CoreAudioDeviceLivenessListenerContext>() };
    let is_alive = audio_object_property::<u32>(
        device_object_id,
        kAudioDevicePropertyDeviceIsAlive,
        kAudioObjectPropertyScopeGlobal,
    );
    if is_alive != Some(1) {
        context
            .failure_recorder
            .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(format!(
                "audio device '{}' went away during {}",
                context.device_uid,
                context.direction.lowercase_direction_name()
            )));
    }
    NO_ERR
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
        stream_format: AudioStreamFormat,
        delivery: Delivery,
        failure_recorder: DeviceStreamFailureRecorder,
    ) -> Result<Self> {
        let direction_name = Delivery::DIRECTION.lowercase_direction_name();
        let hal_output_unit =
            CoreAudioHalOutputUnit::new_bound_to(device, Delivery::DIRECTION, stream_format)?;
        // Raised to the device's own ceiling before initialising, because a
        // device whose buffer size another process raises past the unit's
        // default would otherwise fail every render.
        let largest_cycle_in_frames =
            match largest_io_cycle_in_frames_of(device, Delivery::DIRECTION) {
                Some(frames) => {
                    hal_output_unit
                        .set_property(
                            kAudioUnitProperty_MaximumFramesPerSlice,
                            kAudioUnitScope_Global,
                            AUHAL_OUTPUT_ELEMENT,
                            &frames,
                        )
                        .map_err(|status| {
                            device.refused_with_status(
                                &format!(
                                    "refused a largest cycle of {frames} frames for \
                                     {direction_name}"
                                ),
                                status,
                            )
                        })?;
                    frames
                }
                None => hal_output_unit
                    .global_u32_property(kAudioUnitProperty_MaximumFramesPerSlice)
                    .unwrap_or(0),
            };
        let callback_context = Box::new(CoreAudioCallbackContext {
            audio_unit: hal_output_unit.audio_unit,
            failure_recorder,
            delivery: Mutex::new(delivery),
        });
        let callback_context_pointer = (callback_context.as_ref()
            as *const CoreAudioCallbackContext<Delivery>)
            .cast_mut()
            .cast();
        let initialised = hal_output_unit
            .set_property(
                Delivery::CALLBACK_PROPERTY_ID,
                Delivery::CALLBACK_SCOPE,
                AUHAL_OUTPUT_ELEMENT,
                &AURenderCallbackStruct {
                    inputProc: Delivery::CALLBACK,
                    inputProcRefCon: callback_context_pointer,
                },
            )
            .and_then(|()| {
                // SAFETY: a configured, uninitialised unit, whose callback
                // context is boxed beside it and outlives it.
                let status = unsafe { AudioUnitInitialize(hal_output_unit.audio_unit) };
                if status == NO_ERR {
                    Ok(())
                } else {
                    Err(status)
                }
            });
        initialised.map_err(|status| {
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

/// The capture callback's state: the hand-off, and the buffer each input
/// cycle is rendered into before it is handed off.
struct CoreAudioCaptureDelivery {
    installed_hand_off: Option<CapturedAudioBlockHandOff>,
    /// Sized for the unit's largest cycle.
    render_buffer: Vec<u8>,
    stream_format: AudioStreamFormat,
    capture_latency_in_frames: u32,
    device: CoreAudioDevice,
    render_failures: ConsecutiveInputCycleRenderFailures,
    has_reported_a_cycle_without_host_time: bool,
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
        capture_latency_in_frames,
        device,
        render_failures,
        has_reported_a_cycle_without_host_time,
    } = &mut *delivery;
    if installed_hand_off.is_none() {
        return NO_ERR;
    }
    let byte_count = stream_format.interleaved_byte_count_for(frame_count);
    if byte_count > render_buffer.len() {
        let largest_cycle_in_frames =
            render_buffer.len() / stream_format.interleaved_byte_count_for(1).max(1);
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
            mNumberChannels: stream_format.channels,
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
            - duration_of_frames_in_ns(frame_count, stream_format.sample_rate)
    };
    // Whole frames only, so a short render still carries `sample_count ×
    // channels` scalars as the seam promises.
    let bytes_per_frame = stream_format.interleaved_byte_count_for(1).max(1);
    let rendered_frame_count =
        (buffer_list.mBuffers[0].mDataByteSize as usize).min(byte_count) / bytes_per_frame;
    let rendered_byte_count = rendered_frame_count * bytes_per_frame;
    let handed_off = catch_unwind(AssertUnwindSafe(|| {
        hand_off(CapturedAudioBlockFromDevice {
            interleaved_sample_bytes: &render_buffer[..rendered_byte_count],
            sample_count: rendered_frame_count as u32,
            first_sample_timestamp_ns: first_sample_timestamp_ns(
                input_cycle_host_time_ns,
                *capture_latency_in_frames,
                stream_format.sample_rate,
            ),
        })
    }));
    if handed_off.is_err() {
        context.end_the_stream_because_the_hand_off_panicked(installed_hand_off);
    }
    NO_ERR
}

/// Bind an input unit to `device`, with its render buffer sized for the
/// largest cycle the unit will deliver.
///
/// Called only once microphone access is granted: binding a unit with input
/// enabled asks `coreaudiod`, which blocks the binding call until the user has
/// answered the privacy prompt.
fn bind_capture_unit(
    device: &CoreAudioDevice,
    stream_format: AudioStreamFormat,
    failure_recorder: DeviceStreamFailureRecorder,
) -> Result<CoreAudioCaptureUnit> {
    let capture_unit = CoreAudioCaptureUnit::bound_to(
        device,
        stream_format,
        CoreAudioCaptureDelivery {
            installed_hand_off: None,
            render_buffer: Vec::new(),
            stream_format,
            capture_latency_in_frames: capture_latency_in_frames_of(device),
            device: device.clone(),
            render_failures: ConsecutiveInputCycleRenderFailures::default(),
            has_reported_a_cycle_without_host_time: false,
        },
        failure_recorder,
    )?;
    let largest_cycle_in_frames = capture_unit.largest_cycle_in_frames;
    let mut delivery = capture_unit.callback_context.delivery.lock();
    delivery.render_buffer =
        vec![0u8; stream_format.interleaved_byte_count_for(largest_cycle_in_frames)];
    tracing::debug!(
        device = %device,
        largest_cycle_in_frames,
        capture_latency_in_frames = delivery.capture_latency_in_frames,
        "CoreAudio audio arm: capture unit bound"
    );
    drop(delivery);
    Ok(capture_unit)
}

/// Whether the user has let this process use the microphone, as far as one
/// capture stream knows — and the unit, which exists only once they have.
enum MicrophoneAccessForTheStream {
    Granted(CoreAudioCaptureUnit),
    /// The user is being asked; a hand-off installed meanwhile waits here.
    AwaitingTheUsersAnswer {
        parked_hand_off: Option<CapturedAudioBlockHandOff>,
    },
    Refused(String),
}

/// Everything that starts and stops a capture stream, behind one lock: the
/// processor's start and stop and the user's permission answer all reach it,
/// from different threads. The device's I/O thread never takes it.
struct CoreAudioCaptureControl {
    microphone_access: MicrophoneAccessForTheStream,
    device: CoreAudioDevice,
    stream_format: AudioStreamFormat,
    failure_recorder: DeviceStreamFailureRecorder,
    _liveness_watch: CoreAudioDeviceLivenessWatch,
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

    fn record_the_failure_that_ended_the_stream(&self, failure: &Error) {
        tracing::error!(device = %self.device, error = %failure, "CoreAudio capture ended");
        self.failure_recorder
            .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(
                failure.to_string(),
            ));
    }
}

/// A capture stream on one CoreAudio input device.
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
        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();

        tracing::info!(
            device = %device,
            sample_rate = stream_format.sample_rate,
            channels = stream_format.channels,
            "CoreAudio audio arm: capture stream opened"
        );

        let capture_control = Arc::new(Mutex::new(CoreAudioCaptureControl {
            microphone_access: MicrophoneAccessForTheStream::AwaitingTheUsersAnswer {
                parked_hand_off: None,
            },
            _liveness_watch: CoreAudioDeviceLivenessWatch::watch(
                &device,
                direction,
                failure_recorder.clone(),
            ),
            device,
            stream_format,
            failure_recorder,
        }));

        let answer_reaches = Arc::downgrade(&capture_control);
        match authorize_the_capture_device_without_waiting_for_the_user(
            microphone_authorization_authority,
            Box::new(move |granted| the_users_microphone_answer_arrived(&answer_reaches, granted)),
        )? {
            CaptureDeviceAuthorizationAtOpen::Granted => {
                let mut control = capture_control.lock();
                let capture_unit = bind_capture_unit(
                    &control.device,
                    control.stream_format,
                    control.failure_recorder.clone(),
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
        let refusal_error = Error::Configuration(refusal.clone());
        control.record_the_failure_that_ended_the_stream(&refusal_error);
        control.microphone_access = MicrophoneAccessForTheStream::Refused(refusal);
        return;
    }
    tracing::info!(device = %control.device, "microphone access allowed");
    match bind_capture_unit(
        &control.device,
        control.stream_format,
        control.failure_recorder.clone(),
    ) {
        Err(bind_failure) => {
            control.record_the_failure_that_ended_the_stream(&bind_failure);
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
                control.record_the_failure_that_ended_the_stream(&start_failure);
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

/// A playback stream on one CoreAudio output device.
pub struct CoreAudioPlaybackStream {
    stream_format: AudioStreamFormat,
    /// The device's `BufferFrameSize` in output scope when the stream opened.
    device_period_in_per_channel_samples: Option<u32>,
    liveness_report: DeviceStreamLivenessReport,
    playback_unit: CoreAudioStreamUnit<CoreAudioPlaybackDelivery>,
    _liveness_watch: CoreAudioDeviceLivenessWatch,
}

impl CoreAudioPlaybackStream {
    fn open(request: &AudioDeviceStreamRequest) -> Result<Self> {
        let direction = CoreAudioStreamDirection::Playback;
        let device = resolve_requested_device(request, direction)?;
        let stream_format = stream_format_of(&device, direction)?;
        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();
        let playback_unit = CoreAudioStreamUnit::bound_to(
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
            "CoreAudio audio arm: playback stream opened"
        );

        Ok(Self {
            stream_format,
            device_period_in_per_channel_samples,
            liveness_report,
            playback_unit,
            _liveness_watch: CoreAudioDeviceLivenessWatch::watch(
                &device,
                direction,
                failure_recorder,
            ),
        })
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
        self.playback_unit.stop_handing_off()?;
        self.playback_unit.start_handing_off_to(hand_off)
    }

    fn stop_requesting(&mut self) -> Result<()> {
        self.playback_unit.stop_handing_off()
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

    /// The client side of the unit is exactly the format the stream reports,
    /// which is what lets a caller match it without conversion in the arm.
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
