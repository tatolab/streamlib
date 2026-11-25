// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn a_texture_outputs_frame_rate_resolves_from_its_class_or_its_nodes_config() {
    let stated = ImageOutputPortFrameRate::Stated(crate::ImageStreamFrameRate {
        numerator: 30_000,
        denominator: 1_001,
    });
    assert_eq!(
        output_port_frame_rate_resolved_against_config(Some(&stated), None),
        Ok(Some(crate::ImageStreamFrameRate {
            numerator: 30_000,
            denominator: 1_001
        }))
    );
    let from_config = ImageOutputPortFrameRate::FromSetting {
        setting: "frames_per_second".into(),
    };
    let resolved = |config: serde_json::Value| {
        output_port_frame_rate_resolved_against_config(Some(&from_config), Some(&config))
    };
    assert_eq!(
        resolved(serde_json::json!({"frames_per_second": 60})),
        Ok(Some(crate::ImageStreamFrameRate {
            numerator: 60,
            denominator: 1
        }))
    );
    assert_eq!(
        resolved(
            serde_json::json!({"frames_per_second": {"numerator": 24000, "denominator": 1001}})
        ),
        Ok(Some(crate::ImageStreamFrameRate {
            numerator: 24_000,
            denominator: 1_001
        }))
    );
    assert_eq!(resolved(serde_json::json!({})), Ok(None));
    assert!(resolved(serde_json::json!({"frames_per_second": 0})).is_err());
    assert!(resolved(serde_json::json!({"frames_per_second": "fast"})).is_err());
}

mod contract_json_schema {
    use std::collections::BTreeSet;

    use serde::Serialize;
    use serde_json::{Value, json};

    use schemars::{JsonSchema, SchemaGenerator};

    use crate::{
        ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
        ImageOutputPortFrameRate, ImageStreamFrameRate,
    };

    fn schema_of<T: JsonSchema>() -> Value {
        SchemaGenerator::default()
            .into_root_schema_for::<T>()
            .to_value()
    }

    fn keys_of(object: &Value) -> BTreeSet<String> {
        object
            .as_object()
            .unwrap_or_else(|| panic!("an object: {object}"))
            .keys()
            .cloned()
            .collect()
    }

    fn one_of(schema: &Value) -> &Vec<Value> {
        schema["oneOf"]
            .as_array()
            .unwrap_or_else(|| panic!("a `oneOf`: {schema}"))
    }

    fn assert_each_serializes_as_a_const_of_the_schema<T: JsonSchema + Serialize>(values: &[T]) {
        let schema = schema_of::<T>();
        let consts: Vec<&Value> = one_of(&schema)
            .iter()
            .map(|variant| &variant["const"])
            .collect();
        assert_eq!(consts.len(), values.len());
        for value in values {
            let written = serde_json::to_value(value).expect("writes");
            assert!(consts.contains(&&written), "{written} is not in {schema}");
        }
    }

    /// Asserts `value` serializes with exactly the property names of the schema variant that
    /// requires its one key, and, for an object under that key, of that variant's inner object.
    fn assert_serializes_as_a_variant_of_the_schema<T: JsonSchema + Serialize>(value: T) {
        let schema = schema_of::<T>();
        let written = serde_json::to_value(value).expect("writes");
        let written_keys = keys_of(&written);
        let variant = one_of(&schema)
            .iter()
            .find(|variant| keys_of(&variant["properties"]) == written_keys)
            .unwrap_or_else(|| panic!("no variant of {schema} has the keys of {written}"));
        assert_eq!(
            variant["required"],
            json!(written_keys.iter().collect::<Vec<_>>())
        );
        for (key, inner) in written.as_object().into_iter().flatten() {
            let inner_schema = &variant["properties"][key];
            if inner.is_object() && inner_schema.get("properties").is_some() {
                assert_eq!(
                    keys_of(inner),
                    keys_of(&inner_schema["properties"]),
                    "{key}"
                );
            }
        }
    }

    #[test]
    fn each_unit_only_enum_serializes_as_one_of_its_schema_consts() {
        assert_each_serializes_as_a_const_of_the_schema(&[
            ChannelsOutputPortSampleType::F32,
            ChannelsOutputPortSampleType::F64,
        ]);
        assert_each_serializes_as_a_const_of_the_schema(&[
            ArrayOutputPortResidency::Gpu,
            ArrayOutputPortResidency::Cpu,
        ]);
    }

    #[test]
    fn each_stated_or_config_key_value_has_its_schema_variant_property_names() {
        let frame_rate = ImageStreamFrameRate {
            numerator: 30,
            denominator: 1,
        };
        assert_serializes_as_a_variant_of_the_schema(ImageOutputPortFrameRate::Stated(frame_rate));
        assert_serializes_as_a_variant_of_the_schema(ImageOutputPortFrameRate::FromSetting {
            setting: "fps".into(),
        });
        assert_serializes_as_a_variant_of_the_schema(ChannelsOutputPortSampleRate::Stated(48_000));
        assert_serializes_as_a_variant_of_the_schema(ChannelsOutputPortSampleRate::FromSetting {
            setting: "rate".into(),
        });
        let frame_rate_schema = schema_of::<ImageStreamFrameRate>();
        let written = serde_json::to_value(frame_rate).expect("writes");
        assert_eq!(keys_of(&written), keys_of(&frame_rate_schema["properties"]));
        assert_eq!(
            frame_rate_schema["required"],
            json!(["numerator", "denominator"])
        );
    }
}
