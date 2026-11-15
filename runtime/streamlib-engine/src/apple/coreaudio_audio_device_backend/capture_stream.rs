// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Weak};

use objc2_audio_toolbox::{
    AURenderCallback, AudioUnitRender, AudioUnitRenderActionFlags,
    kAudioOutputUnitProperty_SetInputCallback, kAudioUnitScope_Global,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp, AudioTimeStampFlags};
use parking_lot::Mutex;

use crate::apple::permissions::{
    CaptureDeviceAuthorizationAtOpen, CaptureDeviceAuthorizationAuthority, CaptureDeviceRefusal,
    PrivacyGatedCaptureDevice, authorize_the_capture_device_without_waiting_for_the_user,
    capture_device_refusal_for_the_user,
};
use crate::core::context::{
    AudioCaptureStream, AudioDeviceStreamRequest, AudioStreamFormat, CaptureDeviceAccessPrompting,
    CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff, DeviceStreamFailureRecorder,
    DeviceStreamLivenessReport,
};
use crate::core::media_clock::MediaClock;
use crate::core::{Error, Result};

use super::audio_objects_and_device_discovery::{
    CoreAudioDevice, CoreAudioStreamDirection, capture_latency_in_frames_of,
    duration_of_frames_in_ns, first_sample_timestamp_ns, osstatus_text, rate_and_channels_of,
    resolve_requested_device, stream_format_of,
};
use super::hal_output_unit::CoreAudioStreamFormatBridge;
use super::input_cycle_render_and_capture_format_conversion::{
    ConsecutiveInputCycleRenderFailures, ConvertedInputCycle, CoreAudioCaptureFormatConverter,
    InputCycleRenderVerdict,
};
use super::stream_device_binding_and_following::{
    CoreAudioStreamControlThatFollowsItsDevice, CoreAudioStreamDeviceBinding,
    CoreAudioStreamDevicePolicy, device_changes_reach,
};
use super::stream_unit_and_callback_context::{
    CoreAudioCallbackContext, CoreAudioDeliveryHoldingAHandOff, CoreAudioStreamUnit,
};
use super::{CONSECUTIVE_FAILED_INPUT_RENDERS_BEFORE_THE_STREAM_ENDS, NO_ERR};

/// The capture callback's state: the hand-off, and the buffer each input
/// cycle is rendered into before it is handed off.
pub(super) struct CoreAudioCaptureDelivery {
    installed_hand_off: Option<CapturedAudioBlockHandOff>,
    /// Sized for the unit's largest cycle, in the format the unit renders.
    render_buffer: Vec<u8>,
    stream_format: AudioStreamFormat,
    /// The stream's format, or the device's own where a converter runs.
    unit_render_format: AudioStreamFormat,
    /// Present only while the device's own format is not the stream's.
    pub(super) format_converter: Option<CoreAudioCaptureFormatConverter>,
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
pub(super) enum MicrophoneAccessForTheStream {
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
pub(super) struct CoreAudioCaptureControl {
    pub(super) microphone_access: MicrophoneAccessForTheStream,
    stream_format: AudioStreamFormat,
    pub(super) device_binding: CoreAudioStreamDeviceBinding,
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
    pub(super) capture_control: Arc<Mutex<CoreAudioCaptureControl>>,
}

impl CoreAudioCaptureStream {
    pub(super) fn open(
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
            request.capture_device_access_prompting,
        )
    }

    /// Open on `device`, carrying `stream_format` for the stream's lifetime
    /// whatever the device — or a later default — carries.
    pub(super) fn open_on(
        device: CoreAudioDevice,
        stream_format: AudioStreamFormat,
        device_policy: CoreAudioStreamDevicePolicy,
        microphone_authorization_authority: &dyn CaptureDeviceAuthorizationAuthority,
        capture_device_access_prompting: CaptureDeviceAccessPrompting,
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
            capture_device_access_prompting,
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
