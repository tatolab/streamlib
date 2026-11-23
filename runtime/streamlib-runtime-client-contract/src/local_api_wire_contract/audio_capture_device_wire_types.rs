// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// The capture `device_id` that names the default output's monitor: what the machine plays,
/// through whichever output is the default when the stream opens, so a saved project names no
/// machine's own output.
pub const DEFAULT_OUTPUT_MONITOR_DEVICE_ID: &str = "@DEFAULT_MONITOR@";

/// The `list_audio_capture_devices` operation's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ListAudioCaptureDevicesToolResult {
    /// The audio backend this machine's runtime opens devices through: `pipewire`, `alsa`,
    /// `coreaudio`, or `silent-null` where it found none.
    pub audio_backend: String,
    /// Each capture device the backend can open: its inputs, the default input first, then the
    /// monitors of its outputs, the default output's first.
    pub devices: Vec<AudioCaptureDeviceOnTheWire>,
}

/// One capture device a microphone's `device_id` names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct AudioCaptureDeviceOnTheWire {
    /// What a microphone's `device_id` names the device by.
    pub id: String,
    /// The name a person reads, as the backend describes the device.
    pub name: String,
    /// Whether the device is an input or an output's monitor.
    pub kind: AudioCaptureDeviceKindOnTheWire,
}

/// What a listed capture device captures. The set grows: a value a client does not know reads
/// as `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum AudioCaptureDeviceKindOnTheWire {
    /// An input: a microphone, a line in, a virtual source.
    Input,
    /// The monitor of an output: what that output plays.
    OutputMonitor,
    /// A kind this client does not know; the runtime never sends it.
    // `#[serde(other)]` holds only on the last variant, so a new kind goes above this one.
    #[serde(other)]
    Other,
}
