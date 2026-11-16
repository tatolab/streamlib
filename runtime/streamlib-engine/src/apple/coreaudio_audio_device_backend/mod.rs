// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The audio backend chain's Apple arm: CoreAudio, through the AUHAL audio unit.
//!
//! Each stream owns one `kAudioUnitSubType_HALOutput` unit with I/O enabled in
//! the stream's direction only. A stream opened with no `device_id` follows the
//! system default device, keeping the format it opened with. The device's I/O
//! thread is the cadence source, and a block's stamp is the device's
//! `mHostTime` — the `mach_absolute_time` domain every other timestamp on Apple
//! lives in.

mod audio_objects_and_device_discovery;
mod capture_stream;
mod hal_output_unit;
mod input_cycle_render_and_capture_format_conversion;
mod playback_stream;
mod stream_device_binding_and_following;
mod stream_unit_and_callback_context;

#[cfg(test)]
mod tests;

/// A capture stream's microphone permission flow, driven by a scripted privacy
/// gate against the real default input. Audio tier: the unit it binds once
/// "allowed" is real, so it needs an input device and the microphone already
/// allowed for the application running it. It captures and never plays.
#[cfg(all(test, feature = "hardware-tests"))]
mod microphone_permission_flow_against_the_default_input;

/// Rebinding — and converting where a stream's format is not its device's —
/// on the real default devices, through the path a device change takes. Audio
/// tier, and silent: it captures from the default input and plays only zeros
/// to the default output. Moving the system default itself needs a person
/// plugging something in, so these rebind to the device already bound.
#[cfg(all(test, feature = "hardware-tests"))]
mod following_the_device_against_the_default_devices;

pub use audio_objects_and_device_discovery::{
    CoreAudioStreamDirection, built_in_audio_device_uid, default_audio_device_uid,
};
pub(crate) use capture_stream::CoreAudioCaptureStream;
pub(crate) use playback_stream::CoreAudioPlaybackStream;

use crate::apple::permissions::{
    AvFoundationCaptureDeviceAuthorizationAuthority, PrivacyGatedCaptureDevice,
};
use crate::core::Result;
use crate::core::context::{
    AudioCaptureStream, AudioDeviceBackend, AudioDeviceStreamRequest, AudioPlaybackStream,
    DeviceBackendArmUnavailableReason,
};

use audio_objects_and_device_discovery::default_device_object_id;
use hal_output_unit::hal_output_component;

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
pub(crate) struct CoreAudioAudioDeviceBackend;

impl CoreAudioAudioDeviceBackend {
    /// Confirm this Mac has the AUHAL unit and an audio device in either
    /// direction, or say why this arm cannot serve so the chain can demote.
    pub(crate) fn find_a_device() -> std::result::Result<Self, DeviceBackendArmUnavailableReason> {
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
