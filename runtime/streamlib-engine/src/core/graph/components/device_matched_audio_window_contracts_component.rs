// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::iceoryx2::DeviceMatchedAudioWindowContractsByInputPort;

/// The `match_device` contracts this processor's input ports have settled,
/// shared live with the mailboxes that settled them.
///
/// Not a rendered component of its own: it exists so a port declaring
/// `audio_window = match_device` renders the five values its device gave rather
/// than the sentinel that asked for them, which is machine-dependent because
/// the device format is — and truer than a static lie.
pub struct DeviceMatchedAudioWindowContractsComponent(
    pub Arc<DeviceMatchedAudioWindowContractsByInputPort>,
);

impl JsonSerializableComponent for DeviceMatchedAudioWindowContractsComponent {
    fn json_key(&self) -> &'static str {
        "device_matched_audio_window_contracts"
    }

    /// Never reached: this component is inserted through
    /// [`GraphNodeWithComponents::insert_component_without_rendering_it`], so no
    /// serializer is registered for it. The trait is what the component map
    /// requires of everything it holds, and the key is stated rather than left
    /// blank so a future decision to render it starts from a name.
    ///
    /// [`GraphNodeWithComponents::insert_component_without_rendering_it`]: crate::core::graph::GraphNodeWithComponents::insert_component_without_rendering_it
    fn to_json(&self) -> JsonValue {
        JsonValue::Null
    }
}
