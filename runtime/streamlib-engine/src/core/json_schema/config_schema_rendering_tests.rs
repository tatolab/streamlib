// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::descriptors::{
    ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
};

fn descriptor_carrying(config_schema: Option<serde_json::Value>) -> ProcessorDescriptor {
    let descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new("ChannelsToArray").unwrap(),
        ProcessorClassImportPath::new("streamlib_media_builtins::port_family_conversion").unwrap(),
        "a probe",
    );
    match config_schema {
        Some(document) => descriptor.with_config_schema(document),
        None => descriptor,
    }
}

/// The control plane serves the descriptor's document, not a summary of
/// it: a field's type, its description and its default all survive the
/// hop, because the MCP catalog reads the same rendering.
#[test]
fn a_registered_descriptors_config_schema_reaches_the_rendering_unchanged() {
    let document = serde_json::json!({
        "type": "object",
        "properties": {
            "width": { "type": "integer", "description": "Frame width in pixels.", "default": 1280 },
            "height": { "type": "integer", "description": "Frame height in pixels.", "default": 720 },
        },
    });
    let rendered = serde_json::to_value(ProcessorDescriptorOutput::from(&descriptor_carrying(
        Some(document.clone()),
    )))
    .unwrap();
    assert_eq!(rendered["config_schema"], document);
}

/// A descriptor built without one — which no declared processor is, in
/// either language — renders no key at all rather than a null an agent
/// would have to read as "no config".
#[test]
fn a_descriptor_carrying_no_config_schema_renders_no_key_rather_than_a_null() {
    let rendered =
        serde_json::to_value(ProcessorDescriptorOutput::from(&descriptor_carrying(None))).unwrap();
    assert!(rendered.get("config_schema").is_none(), "{rendered}");
}
