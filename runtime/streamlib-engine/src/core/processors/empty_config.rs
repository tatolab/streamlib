// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmptyConfig;

impl serde::Serialize for EmptyConfig {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::ser::SerializeMap::end(serializer.serialize_map(Some(0))?)
    }
}

impl<'de> serde::Deserialize<'de> for EmptyConfig {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(EmptyConfigVisitor)
    }
}

struct EmptyConfigVisitor;

impl<'de> serde::de::Visitor<'de> for EmptyConfigVisitor {
    type Value = EmptyConfig;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an empty configuration")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut named_map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        match named_map.next_key::<String>()? {
            Some(refused_key) => Err(serde::de::Error::custom(format!(
                "this processor declares no config and takes none, \
                 so `{refused_key}` has nowhere to go"
            ))),
            None => Ok(EmptyConfig),
        }
    }

    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(EmptyConfig)
    }
}

impl schemars::JsonSchema for EmptyConfig {
    fn schema_name() -> String {
        "EmptyConfig".to_string()
    }

    fn json_schema(_: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        let schema: schemars::schema::Schema = schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::Object.into()),
            object: Some(Box::new(schemars::schema::ObjectValidation {
                additional_properties: Some(Box::new(schemars::schema::Schema::Bool(false))),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into();
        crate::core::descriptors::schema_carrying_the_declared_description(
            schema,
            "This node declares no configuration.",
        )
    }
}
