// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ptr::NonNull;
use std::sync::{Arc, Weak};

use dispatch2::{DispatchQueue, DispatchRetained};
use objc2_core_audio::{
    AudioObjectAddPropertyListenerBlock, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListenerBlock, kAudioDevicePropertyDeviceIsAlive,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyStreamConfiguration,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};
use parking_lot::Mutex;

use crate::core::Result;
use crate::core::context::{
    AudioDeviceStreamRequest, AudioStreamFormat, DeviceStreamFailureReason,
    DeviceStreamFailureRecorder, DeviceStreamLivenessReport,
};
use crate::core::logging::{
    run_in_the_project_log_route_entry_when_there_is_one,
    the_project_log_route_entry_of_this_thread,
};

use super::NO_ERR;
use super::audio_objects_and_device_discovery::{
    CoreAudioDevice, CoreAudioStreamDirection, audio_object_property, default_device_object_id,
    describe_device, osstatus_text, property_address, stream_format_of,
};
use super::hal_output_unit::CoreAudioStreamFormatBridge;

/// A change CoreAudio reports about the device a stream is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CoreAudioStreamDeviceChange {
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
pub(super) struct CoreAudioDeviceFactsAfterAChange {
    pub(super) the_bound_device_is_alive: bool,
    /// Its rate or channel count is no longer the one the stream bound at.
    pub(super) the_bound_devices_own_format_changed: bool,
    pub(super) system_default_device: Option<AudioObjectID>,
}

/// What a stream does about a change to its device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CoreAudioStreamDeviceChangeResponse {
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
pub(super) enum CoreAudioStreamDevicePolicy {
    PinnedToTheNamedDevice,
    FollowsTheSystemDefault,
}

impl CoreAudioStreamDevicePolicy {
    /// A request naming a device pins the stream to it; one naming none follows
    /// the default.
    pub(super) fn of(request: &AudioDeviceStreamRequest) -> Self {
        match request.device_id {
            Some(_) => Self::PinnedToTheNamedDevice,
            None => Self::FollowsTheSystemDefault,
        }
    }
}

/// A named stream stays pinned to its device; one opened with no `device_id`
/// follows the system default. Either rebinds when its device's own format
/// changes.
pub(super) fn how_a_stream_responds_to_a_device_change(
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
pub(super) struct CoreAudioStreamDevicePropertyListener {
    object_id: AudioObjectID,
    address: AudioObjectPropertyAddress,
    stream_control_queue: DispatchRetained<DispatchQueue>,
    listener_block: CoreAudioStreamDevicePropertyListenerBlock,
}

// SAFETY: the block's captures are a `Copy` change, an `Arc` of a
// `Send + Sync` handler and an optional `Arc` of a `Send + Sync` log route,
// and Objective-C block retain and release are thread-safe, so the listener
// may be dropped from any thread.
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
        let project_log_route_entry_of_the_listening_thread =
            the_project_log_route_entry_of_this_thread();
        let listener_block: CoreAudioStreamDevicePropertyListenerBlock =
            block2::RcBlock::new(move |_address_count: u32, _addresses| {
                run_in_the_project_log_route_entry_when_there_is_one(
                    project_log_route_entry_of_the_listening_thread.as_ref(),
                    || handle_the_change(change),
                )
            });
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
pub(super) struct CoreAudioStreamDeviceBinding {
    direction: CoreAudioStreamDirection,
    pub(super) device: CoreAudioDevice,
    /// The device's own rate and channel count when the stream last bound to it.
    pub(super) device_own_format: AudioStreamFormat,
    pub(super) device_policy: CoreAudioStreamDevicePolicy,
    pub(super) failure_recorder: DeviceStreamFailureRecorder,
    liveness_report: DeviceStreamLivenessReport,
    /// Serial and the stream's own, so changes are handled one at a time.
    pub(super) stream_control_queue: DispatchRetained<DispatchQueue>,
    pub(super) handle_a_device_change: CoreAudioStreamDeviceChangeHandler,
    /// `DeviceIsAlive`, nominal rate and stream configuration, on the bound
    /// device.
    pub(super) bound_device_listeners: Vec<CoreAudioStreamDevicePropertyListener>,
    /// On the system object, for a stream that follows the default.
    pub(super) system_default_device_listener: Option<CoreAudioStreamDevicePropertyListener>,
}

impl CoreAudioStreamDeviceBinding {
    pub(super) fn new(
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
    pub(super) fn start_listening(&mut self) {
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

    pub(super) fn record_the_failure_that_ended_the_stream(&self, reason: String) {
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
pub(super) trait CoreAudioStreamControlThatFollowsItsDevice: Send + 'static {
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
pub(super) fn device_changes_reach<Control: CoreAudioStreamControlThatFollowsItsDevice>(
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
pub(super) fn rebind_the_stream_to<Control: CoreAudioStreamControlThatFollowsItsDevice>(
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
