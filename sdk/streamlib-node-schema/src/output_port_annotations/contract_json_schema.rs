// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The schemas of the port stream facts a pin annotation states, for the control contract,
//! written by hand to state each variant as the contract renders it.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

use super::{
    ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    ImageOutputPortFrameRate,
};
use crate::ImageStreamFrameRate;

/// The schema of the variant `{"from_setting": {"setting": "<name>"}}`.
fn from_setting_variant(description: &str) -> Schema {
    json_schema!({
        "type": "object",
        "properties": {
            "from_setting": {
                "type": "object",
                "properties": { "setting": { "type": "string" } },
                "required": ["setting"],
            },
        },
        "required": ["from_setting"],
        "additionalProperties": false,
        "description": description,
    })
}

/// The schema of the variant `{"stated": <stated>}`.
fn stated_variant(stated: Schema, description: &str) -> Schema {
    json_schema!({
        "type": "object",
        "properties": { "stated": stated },
        "required": ["stated"],
        "additionalProperties": false,
        "description": description,
    })
}

/// The schema of a unit-only enum serialized as one of `values`.
fn one_of_strings(values: &[&str], description: &str) -> Schema {
    let one_of: Vec<serde_json::Value> = values
        .iter()
        .map(|value| serde_json::json!({ "type": "string", "const": value }))
        .collect();
    json_schema!({ "oneOf": one_of, "description": description })
}

impl JsonSchema for ImageStreamFrameRate {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ImageStreamFrameRate")
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "numerator": generator.subschema_for::<u32>(),
                "denominator": generator.subschema_for::<u32>(),
            },
            "required": ["numerator", "denominator"],
            "additionalProperties": false,
            "description": "A Texture stream's frame rate, as a rational number of frames per second.",
        })
    }
}

impl JsonSchema for ImageOutputPortFrameRate {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ImageOutputPortFrameRate")
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                stated_variant(
                    generator.subschema_for::<ImageStreamFrameRate>(),
                    "Frames per second, stated on the class.",
                ),
                from_setting_variant("The value of the node's setting of this name."),
            ],
            "description": "The frame rate a Texture output states, written on the class or \
                            read from one of its node's settings.",
        })
    }
}

impl JsonSchema for ChannelsOutputPortSampleRate {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ChannelsOutputPortSampleRate")
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                stated_variant(
                    generator.subschema_for::<u32>(),
                    "Samples per second, stated on the class.",
                ),
                from_setting_variant("The value of the node's setting of this name."),
            ],
            "description": "The sample rate an Audio output states, written on the class or \
                            read from one of its node's settings.",
        })
    }
}

impl JsonSchema for ChannelsOutputPortSampleType {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ChannelsOutputPortSampleType")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        one_of_strings(&["f32", "f64"], "The sample type of an Audio output.")
    }
}

impl JsonSchema for ArrayOutputPortResidency {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ArrayOutputPortResidency")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        one_of_strings(&["gpu", "cpu"], "Where a Tensor output's attributes live.")
    }
}
