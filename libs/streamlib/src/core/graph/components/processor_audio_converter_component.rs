// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::{Arc, Mutex};

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::utils::ProcessorAudioConverterStatus;

/// ECS component exposing a processor's audio converter status.
pub struct ProcessorAudioConverterComponent(pub Arc<Mutex<ProcessorAudioConverterStatus>>);

impl JsonSerializableComponent for ProcessorAudioConverterComponent {
    fn json_key(&self) -> &'static str {
        "processor_audio_converter"
    }

    fn to_json(&self) -> JsonValue {
        let status = self.0.lock().unwrap();
        serde_json::json!({
            "is_resampling": status.is_resampling,
            "is_converting_channels": status.is_converting_channels,
            "is_rechunking": status.is_rechunking,
            "frames_converted": status.frames_converted,
            "source_sample_rate": status.source_sample_rate,
            "source_channels": status.source_channels,
            "target_sample_rate": status.target_sample_rate,
            "target_channels": status.target_channels,
            "target_buffer_size": status.target_buffer_size,
        })
    }
}
