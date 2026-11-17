// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The schemars 1.2 schemas of the port stream facts a pin annotation states, for the control
//! contract. Written by hand: these types derive schemars 0.8, which reads the same
//! `#[schemars]` attribute the 1.2 derive would need.

use std::borrow::Cow;

use schemars_1::{JsonSchema, Schema, SchemaGenerator, json_schema};

use super::{
    ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    ImageOutputPortFrameRate,
};
use crate::ImageStreamFrameRate;

/// The schema of the variant `{"from_config": {"config_key": "<key>"}}`.
fn from_config_variant(description: &str) -> Schema {
    json_schema!({
        "type": "object",
        "properties": {
            "from_config": {
                "type": "object",
                "properties": { "config_key": { "type": "string" } },
                "required": ["config_key"],
            },
        },
        "required": ["from_config"],
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
            "description": "An Image stream's frame rate, as a rational number of frames per second.",
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
                from_config_variant("The value of the node's config key of this name."),
            ],
            "description": "The frame rate an Image output states, written on the class or read \
                            from its node's config.",
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
                from_config_variant("The value of the node's config key of this name."),
            ],
            "description": "The sample rate a Channels output states, written on the class or \
                            read from its node's config.",
        })
    }
}

impl JsonSchema for ChannelsOutputPortSampleType {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ChannelsOutputPortSampleType")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        one_of_strings(&["f32", "f64"], "The sample type of a Channels output.")
    }
}

impl JsonSchema for ArrayOutputPortResidency {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("ArrayOutputPortResidency")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        one_of_strings(&["gpu", "cpu"], "Where an Array output's attributes live.")
    }
}
