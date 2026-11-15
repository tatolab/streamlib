// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ptr::NonNull;

use objc2_audio_toolbox::{
    AudioComponentDescription, AudioComponentFindNext, AudioComponentInstanceDispose,
    AudioComponentInstanceNew, AudioOutputUnitStart, AudioOutputUnitStop, AudioUnit,
    AudioUnitGetProperty, AudioUnitInitialize, AudioUnitSetProperty, AudioUnitUninitialize,
    kAudioOutputUnitProperty_CurrentDevice, kAudioOutputUnitProperty_EnableIO,
    kAudioUnitManufacturer_Apple, kAudioUnitProperty_MaximumFramesPerSlice,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global, kAudioUnitScope_Input,
    kAudioUnitScope_Output, kAudioUnitSubType_HALOutput, kAudioUnitType_Output,
};

use crate::core::context::AudioStreamFormat;
use crate::core::{Error, Result};

use super::audio_objects_and_device_discovery::{
    CoreAudioDevice, CoreAudioStreamDirection, interleaved_f32_description_of,
    largest_io_cycle_in_frames_of, osstatus_as_result, rate_and_channels_of,
};
use super::{AUHAL_INPUT_ELEMENT, AUHAL_OUTPUT_ELEMENT, NO_AUHAL_OUTPUT_UNIT, NO_ERR};

pub(super) fn hal_output_component() -> Option<objc2_audio_toolbox::AudioComponent> {
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
pub(super) struct CoreAudioHalOutputUnit {
    pub(super) audio_unit: AudioUnit,
    pub(super) is_running: bool,
}

// SAFETY: an AudioUnit handle may be driven from any thread; every call on it
// here is made through `&mut self` or under the owning stream's lock.
unsafe impl Send for CoreAudioHalOutputUnit {}

impl CoreAudioHalOutputUnit {
    /// A new AUHAL instance with I/O enabled in `direction` only and no device
    /// bound yet; `device` is the one a refusal names.
    pub(super) fn new_enabled_for(
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
    pub(super) fn bind_to(
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

    pub(super) fn initialise(&self) -> std::result::Result<(), i32> {
        // SAFETY: a configured unit, whose callback context outlives it.
        osstatus_as_result(unsafe { AudioUnitInitialize(self.audio_unit) })
    }

    pub(super) fn uninitialise(&self) -> std::result::Result<(), i32> {
        // SAFETY: a live, stopped unit.
        osstatus_as_result(unsafe { AudioUnitUninitialize(self.audio_unit) })
    }

    pub(super) fn set_property<T>(
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

    pub(super) fn start(&mut self) -> std::result::Result<(), i32> {
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

    pub(super) fn stop(&mut self) -> std::result::Result<(), i32> {
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
pub(super) enum CoreAudioStreamFormatBridge {
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
    pub(super) fn between(
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
    pub(super) fn unit_client_side_format(
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
