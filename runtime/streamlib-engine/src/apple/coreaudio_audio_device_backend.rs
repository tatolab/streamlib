// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio backend chain's Apple arm: CoreAudio, through the AUHAL audio unit.
//!
//! Each stream owns one `kAudioUnitSubType_HALOutput` unit bound to one device,
//! with I/O enabled in the stream's direction only. The device's I/O thread is
//! the cadence source, and a block's stamp is the device's `mHostTime` — the
//! `mach_absolute_time` domain every other timestamp on Apple lives in.

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
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
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyDeviceUID, kAudioDevicePropertyLatency,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioDevicePropertyStreams, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject,
    kAudioObjectUnknown, kAudioStreamPropertyLatency,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, AudioTimeStampFlags,
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
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

/// Audio over CoreAudio, one AUHAL unit per stream.
pub struct CoreAudioAudioDeviceBackend;

impl CoreAudioAudioDeviceBackend {
    /// Confirm this Mac has the AUHAL unit and an audio device in either
    /// direction, or say why this arm cannot serve so the chain can demote.
    pub fn find_a_device() -> std::result::Result<Self, DeviceBackendArmUnavailableReason> {
        if hal_output_component().is_none() {
            return Err(DeviceBackendArmUnavailableReason::of(
                "CoreAudio offers no AUHAL output unit",
            ));
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
enum CoreAudioStreamDirection {
    Capture,
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

    fn noun(self) -> &'static str {
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
            NonNull::new(words.as_mut_ptr())
                .expect("a Vec's pointer is non-null")
                .cast(),
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
    let raw = audio_object_property::<usize>(object_id, selector, kAudioObjectPropertyScopeGlobal)?;
    let string = NonNull::new(raw as *mut CFString)?;
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
        let buffers_offset = buffers as usize - buffer_list as usize;
        let buffers_that_fit =
            (byte_count.saturating_sub(buffers_offset)) / std::mem::size_of::<AudioBuffer>();
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
                    direction.noun()
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
    let noun = direction.noun();
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
            "audio device '{}' ({}) carries no {} channels.",
            device.uid,
            device.name,
            direction.noun()
        )));
    }
    let nominal_sample_rate = audio_object_property::<f64>(
        device.object_id,
        kAudioDevicePropertyNominalSampleRate,
        kAudioObjectPropertyScopeGlobal,
    )
    .filter(|&rate| rate >= 1.0)
    .ok_or_else(|| {
        Error::Configuration(format!(
            "audio device '{}' ({}) reports no sample rate.",
            device.uid, device.name
        ))
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

/// The monotonic instant of a captured block's first sample: its input
/// cycle's host time, moved back by the frames the device took to deliver it.
fn first_sample_timestamp_ns(
    input_cycle_host_time_ns: i64,
    capture_latency_in_frames: u32,
    sample_rate: u32,
) -> i64 {
    input_cycle_host_time_ns
        - i64::from(capture_latency_in_frames) * 1_000_000_000 / i64::from(sample_rate.max(1))
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
// here is made under the owning stream's lock.
unsafe impl Send for CoreAudioHalOutputUnit {}

impl CoreAudioHalOutputUnit {
    fn new_bound_to(
        device: &CoreAudioDevice,
        direction: CoreAudioStreamDirection,
        stream_format: AudioStreamFormat,
    ) -> Result<Self> {
        let component = hal_output_component()
            .ok_or_else(|| Error::Configuration("CoreAudio offers no AUHAL output unit".into()))?;
        let mut audio_unit: AudioUnit = std::ptr::null_mut();
        // SAFETY: `component` is a live component and `audio_unit` a writable slot.
        let status =
            unsafe { AudioComponentInstanceNew(component, NonNull::from(&mut audio_unit)) };
        if status != NO_ERR || audio_unit.is_null() {
            return Err(Error::Configuration(format!(
                "CoreAudio could not instantiate an AUHAL unit: {}",
                osstatus_text(status)
            )));
        }
        let unit = Self {
            audio_unit,
            is_running: false,
        };
        let refused = |what: &str, status: i32| {
            Error::Configuration(format!(
                "audio device '{}' ({}) refused {what} for {}: {}",
                device.uid,
                device.name,
                direction.noun(),
                osstatus_text(status)
            ))
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

    /// Install the device-thread callback and initialise the unit.
    ///
    /// # Safety
    ///
    /// `callback_context` must stay valid until the unit is dropped.
    unsafe fn install_callback_and_initialize(
        &self,
        callback_property_id: u32,
        callback: AURenderCallback,
        callback_context: *mut c_void,
    ) -> std::result::Result<(), i32> {
        self.set_property(
            callback_property_id,
            kAudioUnitScope_Global,
            AUHAL_OUTPUT_ELEMENT,
            &AURenderCallbackStruct {
                inputProc: callback,
                inputProcRefCon: callback_context,
            },
        )?;
        // SAFETY: a configured, uninitialised unit.
        let status = unsafe { AudioUnitInitialize(self.audio_unit) };
        if status == NO_ERR {
            Ok(())
        } else {
            Err(status)
        }
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
        // SAFETY: the unit is stopped; uninitialising an unitialised unit and
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

// SAFETY: the context is only read, and everything in it is `Sync`.
unsafe impl Send for CoreAudioDeviceLivenessWatch {}

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
                device_uid = %device.uid,
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
                context.direction.noun()
            )));
    }
    NO_ERR
}

/// Records a hand-off that panicked, since unwinding into CoreAudio's I/O
/// thread is undefined: the stream stops serving rather than crashing.
fn a_hand_off_panicked(
    failure_recorder: &DeviceStreamFailureRecorder,
    direction: CoreAudioStreamDirection,
) {
    failure_recorder.record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(
        format!(
            "the {} hand-off panicked on the device thread",
            direction.noun()
        ),
    ));
}

/// What the capture callback reads on the device's I/O thread.
struct CoreAudioCaptureCallbackContext {
    audio_unit: AudioUnit,
    stream_format: AudioStreamFormat,
    capture_latency_in_frames: u32,
    /// Written only on the device's single I/O thread, and only while the unit runs.
    render_buffer: UnsafeCell<Vec<u8>>,
    installed_hand_off: Mutex<Option<CapturedAudioBlockHandOff>>,
    failure_recorder: DeviceStreamFailureRecorder,
    has_reported_an_oversized_cycle: AtomicBool,
}

// SAFETY: `render_buffer` is touched only by the device's one I/O thread;
// everything else is `Sync` or an AudioUnit handle rendered through on that
// same thread.
unsafe impl Sync for CoreAudioCaptureCallbackContext {}
unsafe impl Send for CoreAudioCaptureCallbackContext {}

unsafe extern "C-unwind" fn captured_input_became_available(
    callback_context: NonNull<c_void>,
    action_flags: NonNull<AudioUnitRenderActionFlags>,
    time_stamp: NonNull<AudioTimeStamp>,
    bus_number: u32,
    frame_count: u32,
    _unused_buffer_list: *mut AudioBufferList,
) -> i32 {
    // SAFETY: registered with a `CoreAudioCaptureCallbackContext` that outlives
    // the unit it was registered on.
    let context = unsafe {
        callback_context
            .cast::<CoreAudioCaptureCallbackContext>()
            .as_ref()
    };
    let installed_hand_off = context.installed_hand_off.lock();
    let Some(hand_off) = installed_hand_off.as_ref() else {
        return NO_ERR;
    };
    // SAFETY: only this, the device's one I/O thread, touches the buffer.
    let render_buffer = unsafe { &mut *context.render_buffer.get() };
    let byte_count = context
        .stream_format
        .interleaved_byte_count_for(frame_count);
    if byte_count > render_buffer.len() {
        if !context
            .has_reported_an_oversized_cycle
            .swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                frame_count,
                capacity_in_bytes = render_buffer.len(),
                "CoreAudio audio arm: an input cycle outgrew the unit's own frame limit; \
                 dropping such cycles"
            );
        }
        return NO_ERR;
    }
    let mut buffer_list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: context.stream_format.channels,
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
    if status != NO_ERR {
        return status;
    }
    // SAFETY: CoreAudio passes a valid timestamp for the cycle.
    let time_stamp = unsafe { time_stamp.as_ref() };
    let input_cycle_host_time_ns = if time_stamp
        .mFlags
        .contains(AudioTimeStampFlags::HostTimeValid)
    {
        MediaClock::nanos_from_raw_timestamp(time_stamp.mHostTime).as_nanos() as i64
    } else {
        // AUHAL always stamps its input cycles with host time; were one ever
        // not, the cycle ended now and began a block ago.
        MediaClock::now().as_nanos() as i64
            - i64::from(frame_count) * 1_000_000_000
                / i64::from(context.stream_format.sample_rate.max(1))
    };
    let rendered_byte_count = (buffer_list.mBuffers[0].mDataByteSize as usize).min(byte_count);
    let handed_off = catch_unwind(AssertUnwindSafe(|| {
        hand_off(CapturedAudioBlockFromDevice {
            interleaved_sample_bytes: &render_buffer[..rendered_byte_count],
            sample_count: frame_count,
            first_sample_timestamp_ns: first_sample_timestamp_ns(
                input_cycle_host_time_ns,
                context.capture_latency_in_frames,
                context.stream_format.sample_rate,
            ),
        })
    }));
    if handed_off.is_err() {
        a_hand_off_panicked(&context.failure_recorder, CoreAudioStreamDirection::Capture);
    }
    NO_ERR
}

/// An input-bound AUHAL unit and the context its callback reads.
///
/// Built only once microphone access is granted: binding a unit with input
/// enabled asks `coreaudiod`, which blocks the binding call until the user has
/// answered the privacy prompt.
struct CoreAudioCaptureUnit {
    /// Declared before the context so it is dropped — and its callback
    /// retired — before the context is freed.
    hal_output_unit: CoreAudioHalOutputUnit,
    callback_context: Box<CoreAudioCaptureCallbackContext>,
}

impl CoreAudioCaptureUnit {
    fn bound_to(
        device: &CoreAudioDevice,
        stream_format: AudioStreamFormat,
        failure_recorder: DeviceStreamFailureRecorder,
    ) -> Result<Self> {
        let direction = CoreAudioStreamDirection::Capture;
        let hal_output_unit =
            CoreAudioHalOutputUnit::new_bound_to(device, direction, stream_format)?;
        let callback_context = Box::new(CoreAudioCaptureCallbackContext {
            audio_unit: hal_output_unit.audio_unit,
            stream_format,
            capture_latency_in_frames: capture_latency_in_frames_of(device),
            render_buffer: UnsafeCell::new(Vec::new()),
            installed_hand_off: Mutex::new(None),
            failure_recorder,
            has_reported_an_oversized_cycle: AtomicBool::new(false),
        });
        // SAFETY: the context is boxed and owned beside the unit, which this
        // struct drops first.
        unsafe {
            hal_output_unit.install_callback_and_initialize(
                kAudioOutputUnitProperty_SetInputCallback,
                Some(captured_input_became_available),
                (callback_context.as_ref() as *const CoreAudioCaptureCallbackContext)
                    .cast_mut()
                    .cast(),
            )
        }
        .map_err(|status| {
            Error::Configuration(format!(
                "audio device '{}' ({}) would not initialise for capture: {}",
                device.uid,
                device.name,
                osstatus_text(status)
            ))
        })?;
        let largest_cycle_in_frames = hal_output_unit
            .global_u32_property(kAudioUnitProperty_MaximumFramesPerSlice)
            .unwrap_or(0)
            .max(
                audio_object_property::<u32>(
                    device.object_id,
                    kAudioDevicePropertyBufferFrameSize,
                    direction.device_property_scope(),
                )
                .unwrap_or(0),
            );
        // SAFETY: the unit has not started, so no I/O thread touches the buffer.
        unsafe {
            *callback_context.render_buffer.get() =
                vec![0u8; stream_format.interleaved_byte_count_for(largest_cycle_in_frames)]
        };
        tracing::debug!(
            device_uid = %device.uid,
            largest_cycle_in_frames,
            capture_latency_in_frames = callback_context.capture_latency_in_frames,
            "CoreAudio audio arm: capture unit bound"
        );
        Ok(Self {
            hal_output_unit,
            callback_context,
        })
    }

    fn start_delivering_into(
        &mut self,
        device: &CoreAudioDevice,
        hand_off: CapturedAudioBlockHandOff,
    ) -> Result<()> {
        *self.callback_context.installed_hand_off.lock() = Some(hand_off);
        self.hal_output_unit.start().map_err(|status| {
            *self.callback_context.installed_hand_off.lock() = None;
            Error::Configuration(format!(
                "audio device '{}' ({}) would not start capturing: {}",
                device.uid,
                device.name,
                osstatus_text(status)
            ))
        })
    }

    fn stop_delivering(&mut self, device: &CoreAudioDevice) -> Result<()> {
        let stopped = self.hal_output_unit.stop();
        // Cleared after the unit stops, and under the lock the callback holds
        // across its call, so no hand-off runs once this returns.
        *self.callback_context.installed_hand_off.lock() = None;
        stopped.map_err(|status| {
            Error::Configuration(format!(
                "audio device '{}' ({}) would not stop capturing: {}",
                device.uid,
                device.name,
                osstatus_text(status)
            ))
        })
    }
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
            MicrophoneAccessForTheStream::Granted(capture_unit) => {
                capture_unit.stop_delivering(&self.device)
            }
            MicrophoneAccessForTheStream::AwaitingTheUsersAnswer { parked_hand_off } => {
                *parked_hand_off = None;
                Ok(())
            }
            MicrophoneAccessForTheStream::Refused(_) => Ok(()),
        }
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
            device_uid = %device.uid,
            device_name = %device.name,
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
                let capture_unit = CoreAudioCaptureUnit::bound_to(
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
    let parked_hand_off = match &mut control.microphone_access {
        MicrophoneAccessForTheStream::AwaitingTheUsersAnswer { parked_hand_off } => {
            parked_hand_off.take()
        }
        _ => return,
    };
    if !granted {
        let refusal = capture_device_refusal_for_the_user(
            PrivacyGatedCaptureDevice::Microphone,
            CaptureDeviceRefusal::DeniedByTheUser,
        );
        tracing::error!(device_uid = %control.device.uid, "{refusal}");
        control
            .failure_recorder
            .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(
                refusal.clone(),
            ));
        control.microphone_access = MicrophoneAccessForTheStream::Refused(refusal);
        return;
    }
    tracing::info!(device_uid = %control.device.uid, "microphone access allowed");
    let control = &mut *control;
    let started = CoreAudioCaptureUnit::bound_to(
        &control.device,
        control.stream_format,
        control.failure_recorder.clone(),
    )
    .and_then(|mut capture_unit| {
        let started = match parked_hand_off {
            Some(hand_off) => capture_unit.start_delivering_into(&control.device, hand_off),
            None => Ok(()),
        };
        control.microphone_access = MicrophoneAccessForTheStream::Granted(capture_unit);
        started
    });
    if let Err(start_failure) = started {
        tracing::error!(device_uid = %control.device.uid, error = %start_failure, "microphone allowed, but capture could not start");
        control
            .failure_recorder
            .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(
                start_failure.to_string(),
            ));
        if !matches!(
            control.microphone_access,
            MicrophoneAccessForTheStream::Granted(_)
        ) {
            control.microphone_access =
                MicrophoneAccessForTheStream::Refused(start_failure.to_string());
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
        let control = &mut *control;
        match &mut control.microphone_access {
            MicrophoneAccessForTheStream::Granted(capture_unit) => {
                capture_unit.start_delivering_into(&control.device, hand_off)
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

/// What the playback callback reads on the device's I/O thread.
struct CoreAudioPlaybackCallbackContext {
    installed_hand_off: Mutex<Option<AudioBlockForPlaybackHandOff>>,
    failure_recorder: DeviceStreamFailureRecorder,
}

unsafe extern "C-unwind" fn playback_samples_requested(
    callback_context: NonNull<c_void>,
    action_flags: NonNull<AudioUnitRenderActionFlags>,
    _time_stamp: NonNull<AudioTimeStamp>,
    _bus_number: u32,
    frame_count: u32,
    buffer_list: *mut AudioBufferList,
) -> i32 {
    // SAFETY: registered with a `CoreAudioPlaybackCallbackContext` that
    // outlives the unit it was registered on.
    let context = unsafe {
        callback_context
            .cast::<CoreAudioPlaybackCallbackContext>()
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
    let installed_hand_off = context.installed_hand_off.lock();
    let Some(hand_off) = installed_hand_off.as_ref() else {
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
        a_hand_off_panicked(
            &context.failure_recorder,
            CoreAudioStreamDirection::Playback,
        );
    }
    NO_ERR
}

/// A playback stream on one CoreAudio output device.
pub struct CoreAudioPlaybackStream {
    stream_format: AudioStreamFormat,
    liveness_report: DeviceStreamLivenessReport,
    device: CoreAudioDevice,
    /// Declared before the context so it is dropped — and its callback
    /// retired — before the context is freed.
    hal_output_unit: CoreAudioHalOutputUnit,
    callback_context: Box<CoreAudioPlaybackCallbackContext>,
    _liveness_watch: CoreAudioDeviceLivenessWatch,
}

impl CoreAudioPlaybackStream {
    fn open(request: &AudioDeviceStreamRequest) -> Result<Self> {
        let direction = CoreAudioStreamDirection::Playback;
        let device = resolve_requested_device(request, direction)?;
        let stream_format = stream_format_of(&device, direction)?;
        let hal_output_unit =
            CoreAudioHalOutputUnit::new_bound_to(&device, direction, stream_format)?;

        let (failure_recorder, liveness_report) =
            DeviceStreamFailureRecorder::recording_into_a_new_report();
        let callback_context = Box::new(CoreAudioPlaybackCallbackContext {
            installed_hand_off: Mutex::new(None),
            failure_recorder: failure_recorder.clone(),
        });
        // SAFETY: the context is boxed and owned beside the unit, which this
        // stream drops first.
        unsafe {
            hal_output_unit.install_callback_and_initialize(
                kAudioUnitProperty_SetRenderCallback,
                Some(playback_samples_requested),
                (callback_context.as_ref() as *const CoreAudioPlaybackCallbackContext)
                    .cast_mut()
                    .cast(),
            )
        }
        .map_err(|status| {
            Error::Configuration(format!(
                "audio device '{}' ({}) would not initialise for playback: {}",
                device.uid,
                device.name,
                osstatus_text(status)
            ))
        })?;

        tracing::info!(
            device_uid = %device.uid,
            device_name = %device.name,
            sample_rate = stream_format.sample_rate,
            channels = stream_format.channels,
            "CoreAudio audio arm: playback stream opened"
        );

        Ok(Self {
            stream_format,
            liveness_report,
            _liveness_watch: CoreAudioDeviceLivenessWatch::watch(
                &device,
                direction,
                failure_recorder,
            ),
            device,
            hal_output_unit,
            callback_context,
        })
    }
}

impl AudioPlaybackStream for CoreAudioPlaybackStream {
    fn stream_format(&self) -> AudioStreamFormat {
        self.stream_format
    }

    fn liveness_report(&self) -> DeviceStreamLivenessReport {
        self.liveness_report.clone()
    }

    fn start_requesting_from(&mut self, hand_off: AudioBlockForPlaybackHandOff) -> Result<()> {
        self.stop_requesting()?;
        *self.callback_context.installed_hand_off.lock() = Some(hand_off);
        self.hal_output_unit.start().map_err(|status| {
            *self.callback_context.installed_hand_off.lock() = None;
            Error::Configuration(format!(
                "audio device '{}' ({}) would not start playing: {}",
                self.device.uid,
                self.device.name,
                osstatus_text(status)
            ))
        })
    }

    fn stop_requesting(&mut self) -> Result<()> {
        let stopped = self.hal_output_unit.stop();
        // Cleared after the unit stops, and under the lock the callback holds
        // across its call, so no hand-off runs once this returns.
        *self.callback_context.installed_hand_off.lock() = None;
        stopped.map_err(|status| {
            Error::Configuration(format!(
                "audio device '{}' ({}) would not stop playing: {}",
                self.device.uid,
                self.device.name,
                osstatus_text(status)
            ))
        })
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
