// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use objc2_audio_toolbox::{AURenderCallback, AURenderCallbackStruct, AudioUnit};
use parking_lot::Mutex;

use crate::core::context::{
    AudioStreamFormat, DeviceStreamFailureReason, DeviceStreamFailureRecorder,
};
use crate::core::logging::{
    ProjectLogRouteEntry, run_in_the_project_log_route_entry_when_there_is_one,
    the_project_log_route_entry_of_this_thread,
};
use crate::core::{Error, Result};

use super::AUHAL_OUTPUT_ELEMENT;
use super::audio_objects_and_device_discovery::{CoreAudioDevice, CoreAudioStreamDirection};
use super::hal_output_unit::CoreAudioHalOutputUnit;

/// The per-direction state a device-thread callback works on under its lock,
/// paired with the one callback that reads it — so a unit cannot be bound
/// with a callback that would cast its context to the wrong type.
pub(super) trait CoreAudioDeliveryHoldingAHandOff: Send + Sized {
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
pub(super) struct CoreAudioCallbackContext<Delivery> {
    pub(super) audio_unit: AudioUnit,
    failure_recorder: DeviceStreamFailureRecorder,
    /// Held by the callback across its whole call, so clearing the hand-off
    /// under it guarantees no call is in flight or to come.
    pub(super) delivery: Mutex<Delivery>,
    /// The route of the thread that bound the unit, which an ended stream's
    /// record is emitted in.
    project_log_route_entry_of_the_binding_thread: Option<Arc<ProjectLogRouteEntry>>,
}

// SAFETY: the AudioUnit handle is only rendered through on the device's I/O
// thread; everything else is `Send + Sync` given a `Send` delivery.
unsafe impl<Delivery: Send> Send for CoreAudioCallbackContext<Delivery> {}
unsafe impl<Delivery: Send> Sync for CoreAudioCallbackContext<Delivery> {}

impl<Delivery: CoreAudioDeliveryHoldingAHandOff> CoreAudioCallbackContext<Delivery> {
    /// Uninstall the hand-off and record why the stream stopped serving, on
    /// the device thread that found out — a stream whose report names a
    /// failure delivers nothing after it.
    pub(super) fn end_the_stream_because(
        &self,
        installed_hand_off: &mut Option<Delivery::HandOff>,
        reason: String,
    ) {
        *installed_hand_off = None;
        run_in_the_project_log_route_entry_when_there_is_one(
            self.project_log_route_entry_of_the_binding_thread.as_ref(),
            || {
                tracing::error!(
                    %reason,
                    "CoreAudio audio arm: the {} stream ended",
                    Delivery::DIRECTION.lowercase_direction_name()
                );
            },
        );
        self.failure_recorder
            .record_the_failure_that_ended_the_stream(DeviceStreamFailureReason::of(reason));
    }

    /// Unwinding into CoreAudio's I/O thread is undefined, so a hand-off that
    /// panicked ends the stream rather than crashing it.
    pub(super) fn end_the_stream_because_the_hand_off_panicked(
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
pub(super) struct CoreAudioStreamUnit<Delivery: CoreAudioDeliveryHoldingAHandOff> {
    /// Declared before the context so it is dropped — and its callback
    /// retired — before the context is freed.
    hal_output_unit: CoreAudioHalOutputUnit,
    pub(super) callback_context: Box<CoreAudioCallbackContext<Delivery>>,
    device: CoreAudioDevice,
    /// The unit's `MaximumFramesPerSlice`: no cycle it renders is longer.
    pub(super) largest_cycle_in_frames: u32,
}

impl<Delivery: CoreAudioDeliveryHoldingAHandOff> CoreAudioStreamUnit<Delivery> {
    pub(super) fn bound_to(
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
            project_log_route_entry_of_the_binding_thread:
                the_project_log_route_entry_of_this_thread(),
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
    pub(super) fn rebind_to(
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

    pub(super) fn start_handing_off_to(&mut self, hand_off: Delivery::HandOff) -> Result<()> {
        *self.callback_context.delivery.lock().installed_hand_off() = Some(hand_off);
        self.hal_output_unit.start().map_err(|status| {
            *self.callback_context.delivery.lock().installed_hand_off() = None;
            self.refused_for_this_direction("start", status)
        })
    }

    pub(super) fn stop_handing_off(&mut self) -> Result<()> {
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
