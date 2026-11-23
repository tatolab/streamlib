// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ptr::NonNull;

use objc2_audio_toolbox::{kAudioUnitScope_Input, kAudioUnitScope_Output};
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertyScope, AudioObjectPropertySelector,
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyBufferFrameSizeRange,
    kAudioDevicePropertyDataSource, kAudioDevicePropertyDeviceUID, kAudioDevicePropertyLatency,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioDevicePropertyStreams, kAudioDevicePropertyTransportType,
    kAudioDeviceTransportTypeBuiltIn, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioObjectUnknown, kAudioStreamPropertyLatency,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioValueRange,
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
};
use objc2_core_foundation::{CFRetained, CFString};

use crate::core::context::{
    AudioCaptureDevice, AudioCaptureDeviceKind, AudioDeviceStreamRequest, AudioSampleFormat,
    AudioStreamFormat,
};
use crate::core::{Error, Result};

use super::{AUHAL_INPUT_ELEMENT, AUHAL_OUTPUT_ELEMENT, NO_ERR};

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
    pub(super) fn device_property_scope(self) -> AudioObjectPropertyScope {
        match self {
            CoreAudioStreamDirection::Capture => kAudioObjectPropertyScopeInput,
            CoreAudioStreamDirection::Playback => kAudioObjectPropertyScopeOutput,
        }
    }

    pub(super) fn default_device_selector(self) -> AudioObjectPropertySelector {
        match self {
            CoreAudioStreamDirection::Capture => kAudioHardwarePropertyDefaultInputDevice,
            CoreAudioStreamDirection::Playback => kAudioHardwarePropertyDefaultOutputDevice,
        }
    }

    /// The AUHAL element whose device side this direction uses.
    pub(super) fn auhal_element(self) -> u32 {
        match self {
            CoreAudioStreamDirection::Capture => AUHAL_INPUT_ELEMENT,
            CoreAudioStreamDirection::Playback => AUHAL_OUTPUT_ELEMENT,
        }
    }

    /// The AUHAL scope the client's format is set on: the side of the element
    /// facing this process rather than the device.
    pub(super) fn auhal_client_side_scope(self) -> u32 {
        match self {
            CoreAudioStreamDirection::Capture => kAudioUnitScope_Output,
            CoreAudioStreamDirection::Playback => kAudioUnitScope_Input,
        }
    }

    pub(super) fn present_participle(self) -> &'static str {
        match self {
            CoreAudioStreamDirection::Capture => "capturing",
            CoreAudioStreamDirection::Playback => "playing",
        }
    }

    pub(super) fn lowercase_direction_name(self) -> &'static str {
        match self {
            CoreAudioStreamDirection::Capture => "capture",
            CoreAudioStreamDirection::Playback => "playback",
        }
    }
}

/// One CoreAudio device as the backend enumerates it.
#[derive(Debug, Clone)]
pub(super) struct CoreAudioDevice {
    pub(super) object_id: AudioObjectID,
    /// The persistent identifier a caller names the device by.
    pub(super) uid: String,
    pub(super) name: String,
}

impl std::fmt::Display for CoreAudioDevice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "'{}' ({})", self.uid, self.name)
    }
}

impl CoreAudioDevice {
    /// The error a CoreAudio call on this device failed with, naming the device,
    /// what it would not do, and the status it answered.
    pub(super) fn refused_with_status(
        &self,
        what_the_device_would_not_do: &str,
        status: i32,
    ) -> Error {
        Error::Configuration(format!(
            "audio device {self} {what_the_device_would_not_do}: {}",
            osstatus_text(status)
        ))
    }

    /// "refused binding the unit for capture", and the like, with the status.
    pub(super) fn refused_for(
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

pub(super) fn property_address(
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
pub(super) fn audio_object_property<T: Copy + Default>(
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

pub(super) fn default_device_object_id(
    direction: CoreAudioStreamDirection,
) -> Option<AudioObjectID> {
    audio_object_property::<AudioObjectID>(
        kAudioObjectSystemObject as AudioObjectID,
        direction.default_device_selector(),
        kAudioObjectPropertyScopeGlobal,
    )
    .filter(|&object_id| object_id != kAudioObjectUnknown)
}

/// Channels a device carries in one direction, summed over its streams.
pub(super) fn channel_count_of(
    object_id: AudioObjectID,
    direction: CoreAudioStreamDirection,
) -> u32 {
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

pub(super) fn describe_device(object_id: AudioObjectID) -> CoreAudioDevice {
    CoreAudioDevice {
        object_id,
        uid: audio_object_string_property(object_id, kAudioDevicePropertyDeviceUID)
            .unwrap_or_default(),
        name: audio_object_string_property(object_id, kAudioObjectPropertyName).unwrap_or_default(),
    }
}

/// Every device carrying at least one channel in `direction`.
pub(super) fn devices_carrying(direction: CoreAudioStreamDirection) -> Vec<CoreAudioDevice> {
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

/// `devices` as the capture devices a stream opens by UID, the one whose object is
/// `default_object_id` first and the rest in CoreAudio's order. A device with no UID cannot be
/// named, so it is not listed.
pub(super) fn capture_devices_default_first(
    devices: Vec<CoreAudioDevice>,
    default_object_id: Option<AudioObjectID>,
) -> Vec<AudioCaptureDevice> {
    let mut devices: Vec<CoreAudioDevice> = devices
        .into_iter()
        .filter(|device| !device.uid.is_empty())
        .collect();
    if let Some(default_position) = devices
        .iter()
        .position(|device| Some(device.object_id) == default_object_id)
    {
        let default_device = devices.remove(default_position);
        devices.insert(0, default_device);
    }
    devices
        .into_iter()
        .map(|device| AudioCaptureDevice {
            name: if device.name.is_empty() {
                device.uid.clone()
            } else {
                device.name
            },
            id: device.uid,
            kind: AudioCaptureDeviceKind::Input,
        })
        .collect()
}

/// The UID of the system default device in `direction`, if one is set.
pub fn default_audio_device_uid(direction: CoreAudioStreamDirection) -> Option<String> {
    default_device_object_id(direction)
        .map(describe_device)
        .map(|device| device.uid)
        .filter(|uid| !uid.is_empty())
}

/// The UID of this Mac's own internal speaker or microphone in `direction` —
/// transport `kAudioDeviceTransportTypeBuiltIn` with the internal data source —
/// if one carries that direction.
///
/// The data source is what tells the internal speaker from a headphone jack,
/// which is also a built-in-transport device.
pub fn built_in_audio_device_uid(direction: CoreAudioStreamDirection) -> Option<String> {
    let internal_data_source = match direction {
        CoreAudioStreamDirection::Capture => u32::from_be_bytes(*b"imic"),
        CoreAudioStreamDirection::Playback => u32::from_be_bytes(*b"ispk"),
    };
    devices_carrying(direction)
        .into_iter()
        .find(|device| {
            audio_object_property::<u32>(
                device.object_id,
                kAudioDevicePropertyTransportType,
                kAudioObjectPropertyScopeGlobal,
            ) == Some(kAudioDeviceTransportTypeBuiltIn)
                && audio_object_property::<u32>(
                    device.object_id,
                    kAudioDevicePropertyDataSource,
                    direction.device_property_scope(),
                ) == Some(internal_data_source)
        })
        .map(|device| device.uid)
        .filter(|uid| !uid.is_empty())
}

/// The device a request names, or the system default in `direction`. A named
/// device that is not attached is refused naming it.
pub(super) fn resolve_requested_device(
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

pub(super) fn refusal_for_a_named_audio_device_that_is_not_attached(
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
pub(super) fn stream_format_of(
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

pub(super) fn interleaved_f32_description_of(
    stream_format: AudioStreamFormat,
) -> AudioStreamBasicDescription {
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
pub(super) fn capture_latency_in_frames_of(device: &CoreAudioDevice) -> u32 {
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
pub(super) fn largest_io_cycle_in_frames_of(
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
pub(super) fn duration_of_frames_in_ns(frame_count: u32, sample_rate: u32) -> i64 {
    i64::from(frame_count) * 1_000_000_000 / i64::from(sample_rate.max(1))
}

/// The monotonic instant of a captured block's first sample: its input
/// cycle's host time, moved back by the frames the device took to deliver it.
pub(super) fn first_sample_timestamp_ns(
    input_cycle_host_time_ns: i64,
    capture_latency_in_frames: u32,
    sample_rate: u32,
) -> i64 {
    input_cycle_host_time_ns - duration_of_frames_in_ns(capture_latency_in_frames, sample_rate)
}

pub(super) fn osstatus_text(status: i32) -> String {
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

pub(super) fn osstatus_as_result(status: i32) -> std::result::Result<(), i32> {
    if status == NO_ERR {
        Ok(())
    } else {
        Err(status)
    }
}

/// "48000 Hz, 2 ch" — a format as a refusal or a rebind line names it.
pub(super) fn rate_and_channels_of(format: AudioStreamFormat) -> String {
    format!("{} Hz, {} ch", format.sample_rate, format.channels)
}
