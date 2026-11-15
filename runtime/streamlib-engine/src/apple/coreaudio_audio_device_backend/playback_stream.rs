// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;

use objc2_audio_toolbox::{
    AURenderCallback, AudioUnitRenderActionFlags, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitScope_Input,
};
use objc2_core_audio::kAudioDevicePropertyBufferFrameSize;
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use parking_lot::Mutex;

use crate::core::context::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioDeviceStreamRequest,
    AudioPlaybackStream, AudioSampleFormat, AudioStreamFormat, DeviceStreamFailureRecorder,
    DeviceStreamLivenessReport, RequestedAudioStreamFormat,
};
use crate::core::{Error, Result};

use super::NO_ERR;
use super::audio_objects_and_device_discovery::{
    CoreAudioDevice, CoreAudioStreamDirection, audio_object_property, resolve_requested_device,
    stream_format_of,
};
use super::hal_output_unit::CoreAudioStreamFormatBridge;
use super::stream_device_binding_and_following::{
    CoreAudioStreamControlThatFollowsItsDevice, CoreAudioStreamDeviceBinding,
    CoreAudioStreamDevicePolicy, device_changes_reach,
};
use super::stream_unit_and_callback_context::{
    CoreAudioCallbackContext, CoreAudioDeliveryHoldingAHandOff, CoreAudioStreamUnit,
};

/// The playback callback's state: the hand-off it asks for samples.
pub(super) struct CoreAudioPlaybackDelivery {
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
pub(super) struct CoreAudioPlaybackControl {
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
    pub(super) playback_control: Arc<Mutex<CoreAudioPlaybackControl>>,
}

/// The format a playback stream carries: the request's rate and channel count
/// wherever it names them, which AUHAL's output side converts to the device's,
/// else the device's own. The unit's client side is always float.
pub(super) fn playback_stream_format_for(
    requested_format: RequestedAudioStreamFormat,
    device_own_format: AudioStreamFormat,
) -> Result<AudioStreamFormat> {
    if let Some(sample_format) = requested_format.sample_format
        && sample_format != AudioSampleFormat::F32
    {
        return Err(Error::Configuration(format!(
            "the CoreAudio playback stream carries F32 samples, and {sample_format:?} samples \
             were requested. Request F32, or omit the sample format."
        )));
    }
    Ok(AudioStreamFormat {
        sample_rate: requested_format
            .sample_rate
            .unwrap_or(device_own_format.sample_rate),
        channels: requested_format
            .channels
            .unwrap_or(device_own_format.channels),
        sample_format: AudioSampleFormat::F32,
    })
}

impl CoreAudioPlaybackStream {
    pub(super) fn open(request: &AudioDeviceStreamRequest) -> Result<Self> {
        let direction = CoreAudioStreamDirection::Playback;
        let device = resolve_requested_device(request, direction)?;
        let stream_format = playback_stream_format_for(
            request.requested_format,
            stream_format_of(&device, direction)?,
        )?;
        Self::open_on(
            device,
            stream_format,
            CoreAudioStreamDevicePolicy::of(request),
        )
    }

    /// Open on `device`, carrying `stream_format` for the stream's lifetime
    /// whatever the device — or a later default — carries.
    pub(super) fn open_on(
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
