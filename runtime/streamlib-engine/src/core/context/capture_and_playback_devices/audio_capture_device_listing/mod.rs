// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The audio capture devices a backend lists, and the one alias a capture `device_id` takes
//! beside them: the default output's monitor, resolved by the backend when a stream opens.

use crate::core::Result;
use crate::core::context::capture_and_playback_devices::audio_device_backend::{
    AudioDeviceBackend, AudioDeviceStreamRequest,
};

pub use streamlib_runtime_client_contract::local_api_wire_contract::DEFAULT_OUTPUT_MONITOR_DEVICE_ID;

/// What a listed capture device captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCaptureDeviceKind {
    /// An input: a microphone, a line in, a virtual source.
    Input,
    /// The monitor of an output: what that output plays.
    OutputMonitor,
}

/// A capture device a backend can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCaptureDevice {
    /// What a capture `device_id` names it by.
    pub id: String,
    /// The name a person reads, as the backend describes the device.
    pub name: String,
    /// Whether it is an input or an output's monitor.
    pub kind: AudioCaptureDeviceKind,
}

impl AudioCaptureDevice {
    /// The device `id` names, called by `name` where the backend gives it a non-empty one and by
    /// its id otherwise.
    pub fn named_or_by_id(id: String, name: Option<String>, kind: AudioCaptureDeviceKind) -> Self {
        let name = name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| id.clone());
        Self { id, name, kind }
    }
}

/// What a backend lists in one look at its devices: the capture devices, and the id of its
/// default output's monitor among them, so the two cannot disagree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioCaptureDeviceListing {
    /// The capture devices, inputs first, then output monitors, each with the backend's default
    /// first.
    pub devices: Vec<AudioCaptureDevice>,
    /// The id of the default output's monitor, `None` where the backend captures no output's
    /// monitor or has no default output.
    pub default_output_monitor_device_id: Option<String>,
}

/// `items` with the first one `is_default` picks moved to the front, the rest in their order.
pub fn moved_to_the_front<Item>(
    mut items: Vec<Item>,
    is_default: impl Fn(&Item) -> bool,
) -> Vec<Item> {
    if let Some(default_position) = items.iter().position(is_default) {
        items[..=default_position].rotate_right(1);
    }
    items
}

/// Every capture device `backend` can open: its inputs, then the monitors of its outputs, the
/// default output's monitor first under [`DEFAULT_OUTPUT_MONITOR_DEVICE_ID`] where the backend
/// has one.
pub fn list_audio_capture_devices(
    backend: &dyn AudioDeviceBackend,
) -> Result<Vec<AudioCaptureDevice>> {
    Ok(backend
        .list_capture_devices()?
        .devices_with_the_default_output_monitor_alias())
}

impl AudioCaptureDeviceListing {
    /// The devices as a client sees them: with an entry for [`DEFAULT_OUTPUT_MONITOR_DEVICE_ID`]
    /// ahead of the first monitor where this listing names a default output's monitor.
    pub fn devices_with_the_default_output_monitor_alias(self) -> Vec<AudioCaptureDevice> {
        with_the_default_output_monitor_alias(
            self.devices,
            self.default_output_monitor_device_id.as_deref(),
        )
    }
}

/// `listed` with an entry for [`DEFAULT_OUTPUT_MONITOR_DEVICE_ID`] ahead of the first monitor,
/// named after the monitor it resolves to now.
fn with_the_default_output_monitor_alias(
    mut listed: Vec<AudioCaptureDevice>,
    default_output_monitor: Option<&str>,
) -> Vec<AudioCaptureDevice> {
    let Some(default_output_monitor) = default_output_monitor else {
        return listed;
    };
    let resolved_name = listed
        .iter()
        .find(|device| device.id == default_output_monitor)
        .map_or(default_output_monitor, |device| device.name.as_str());
    let alias = AudioCaptureDevice {
        id: DEFAULT_OUTPUT_MONITOR_DEVICE_ID.to_owned(),
        name: format!("What the default output plays (now {resolved_name})"),
        kind: AudioCaptureDeviceKind::OutputMonitor,
    };
    let first_monitor = listed
        .iter()
        .position(|device| device.kind == AudioCaptureDeviceKind::OutputMonitor)
        .unwrap_or(listed.len());
    listed.insert(first_monitor, alias);
    listed
}

/// `request` with [`DEFAULT_OUTPUT_MONITOR_DEVICE_ID`] resolved to the monitor `backend` names
/// for its default output now, or `None` when the request names another device or none.
///
/// Refused by name where the backend has no default output monitor, never opened as another
/// device.
pub(crate) fn capture_request_with_the_default_output_monitor_resolved(
    backend: &dyn AudioDeviceBackend,
    request: &AudioDeviceStreamRequest,
) -> Result<Option<AudioDeviceStreamRequest>> {
    if request.device_id.as_deref() != Some(DEFAULT_OUTPUT_MONITOR_DEVICE_ID) {
        return Ok(None);
    }
    let resolved = the_default_output_monitor_or_its_refusal(
        backend.backend_name(),
        backend
            .list_capture_devices()?
            .default_output_monitor_device_id,
    )?;
    tracing::info!(
        audio_backend = backend.backend_name(),
        device_id = DEFAULT_OUTPUT_MONITOR_DEVICE_ID,
        resolved_device_id = %resolved,
        "the default output's monitor resolved for a capture stream"
    );
    Ok(Some(AudioDeviceStreamRequest {
        device_id: Some(resolved),
        deviceless_pacing_clock: std::sync::Arc::clone(&request.deviceless_pacing_clock),
        capture_device_access_prompting: request.capture_device_access_prompting,
        requested_format: request.requested_format,
    }))
}

fn the_default_output_monitor_or_its_refusal(
    backend_name: &str,
    default_output_monitor: Option<String>,
) -> Result<String> {
    default_output_monitor.ok_or_else(|| {
        crate::core::Error::Configuration(format!(
            "audio device '{DEFAULT_OUTPUT_MONITOR_DEVICE_ID}' names the default output's \
             monitor, and the {backend_name} backend has none to capture: it captures no \
             output's monitor, or no default output is set. Name an input by device_id, or omit \
             it to use the default input."
        ))
    })
}

#[cfg(test)]
mod tests;
