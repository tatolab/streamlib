// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, Schema, SchemaObject, SubschemaValidation};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use streamlib_idents::SchemaIdent;

// ============================================================================
// Processor Schema Types
// ============================================================================

/// Schema spec for a port — either a fully-qualified [`SchemaIdent`] or the
/// `Any` wildcard for ports that accept arbitrary serialized payloads (used
/// today by MoQ publish/subscribe tracks).
///
/// YAML accepts only two shapes:
/// - `schema: any` — wildcard
/// - `schema: { org, package, type, version }` — 4-field structured map
///
/// Joined-string `'@org/pkg/Type@version'` shorthand is rejected per the
/// `joined_string_is_not_a_valid_yaml_shape` invariant in
/// `streamlib-idents`.
///
/// `Default` resolves to [`PortSchemaSpec::Any`] — the most permissive
/// shape. Used by callers that build a `PortInfo` before the routing tag
/// is known (e.g. graph-builder fallbacks); a default `Any` carries no
/// false specificity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PortSchemaSpec {
    #[default]
    Any,
    Specific(SchemaIdent),
}

impl Serialize for PortSchemaSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            PortSchemaSpec::Any => serializer.serialize_str("any"),
            PortSchemaSpec::Specific(ident) => ident.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for PortSchemaSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match value {
            serde_yaml::Value::String(s) if s == "any" => Ok(PortSchemaSpec::Any),
            serde_yaml::Value::String(s) => Err(D::Error::custom(format!(
                "port schema must be either `any` or a 4-field structured map \
                 ({{ org, package, type, version }}); got string `{}`. Joined-string \
                 shorthand is not allowed (`docs/architecture/schema-identity-and-packaging.md`).",
                s
            ))),
            mapping @ serde_yaml::Value::Mapping(_) => {
                let ident: SchemaIdent =
                    serde_yaml::from_value(mapping).map_err(D::Error::custom)?;
                Ok(PortSchemaSpec::Specific(ident))
            }
            other => Err(D::Error::custom(format!(
                "port schema must be either `any` or a 4-field structured map; got {:?}",
                other
            ))),
        }
    }
}

impl std::fmt::Display for PortSchemaSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortSchemaSpec::Any => f.write_str("any"),
            PortSchemaSpec::Specific(ident) => ident.fmt(f),
        }
    }
}

impl JsonSchema for PortSchemaSpec {
    fn schema_name() -> String {
        "PortSchemaSpec".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_processor_schema::PortSchemaSpec")
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let any_literal = Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            enum_values: Some(vec![serde_json::Value::String("any".into())]),
            ..Default::default()
        });
        let structured = generator.subschema_for::<SchemaIdent>();
        Schema::Object(SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "Either the literal `any` (wildcard, accepts any payload) or a structured 4-field SchemaIdent map."
                        .into(),
                ),
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                one_of: Some(vec![any_literal, structured]),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

/// Runtime language for a processor.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProcessorLanguage {
    #[default]
    Rust,
    Python,
    #[serde(alias = "deno")]
    TypeScript,
}

impl JsonSchema for ProcessorLanguage {
    fn schema_name() -> String {
        "ProcessorLanguage".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_processor_schema::ProcessorLanguage")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        Schema::Object(SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "Processor runtime language. `deno` is accepted as an alias for `typescript`."
                        .into(),
                ),
                ..Default::default()
            })),
            instance_type: Some(InstanceType::String.into()),
            enum_values: Some(vec![
                serde_json::Value::String("rust".into()),
                serde_json::Value::String("python".into()),
                serde_json::Value::String("typescript".into()),
                serde_json::Value::String("deno".into()),
            ]),
            ..Default::default()
        })
    }
}

/// Language-specific runtime options.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeOptions {
    /// [Rust] Generate unsafe Send impl for !Send processors (AVFoundation, etc.)
    #[serde(default)]
    pub unsafe_send: bool,
    /// [Python] Required Python version spec (e.g., ">=3.10"). Python runtime only.
    #[serde(default)]
    pub python_version: Option<String>,
}

/// Internal helper for deserializing RuntimeConfig from either string or object.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RuntimeConfigHelper {
    /// Simple string form: `runtime: rust`
    Simple(ProcessorLanguage),
    /// Full object form: `runtime: { language: rust, options: { unsafe_send: true } }`
    Full(RuntimeConfigFull),
}

/// Full runtime configuration object.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeConfigFull {
    /// Language runtime (rust, python, typescript). Defaults to rust.
    #[serde(default)]
    pub language: ProcessorLanguage,

    /// Language-specific options.
    #[serde(default)]
    pub options: RuntimeOptions,

    /// Environment variables for subprocess runtimes.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Runtime configuration for a processor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Language runtime (rust, python, typescript). Defaults to rust.
    pub language: ProcessorLanguage,

    /// Language-specific options.
    pub options: RuntimeOptions,

    /// Environment variables for subprocess runtimes.
    pub env: std::collections::HashMap<String, String>,
}

impl From<RuntimeConfigHelper> for RuntimeConfig {
    fn from(helper: RuntimeConfigHelper) -> Self {
        match helper {
            RuntimeConfigHelper::Simple(language) => RuntimeConfig {
                language,
                options: RuntimeOptions::default(),
                env: std::collections::HashMap::new(),
            },
            RuntimeConfigHelper::Full(full) => RuntimeConfig {
                language: full.language,
                options: full.options,
                env: full.env,
            },
        }
    }
}

impl Serialize for RuntimeConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // If only language is set with defaults, serialize as simple string
        if self.options == RuntimeOptions::default() && self.env.is_empty() {
            return self.language.serialize(serializer);
        }

        // Otherwise serialize as full object
        let full = RuntimeConfigFull {
            language: self.language.clone(),
            options: self.options.clone(),
            env: self.env.clone(),
        };
        full.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RuntimeConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RuntimeConfigHelper::deserialize(deserializer).map(RuntimeConfig::from)
    }
}

impl JsonSchema for RuntimeConfig {
    fn schema_name() -> String {
        "RuntimeConfig".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_processor_schema::RuntimeConfig")
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let simple = generator.subschema_for::<ProcessorLanguage>();
        let full = generator.subschema_for::<RuntimeConfigFull>();
        Schema::Object(SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "Runtime configuration: either a bare language string (`rust`, `python`, `typescript`) or a `{ language, options, env }` object."
                        .into(),
                ),
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                one_of: Some(vec![simple, full]),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

/// Execution mode for a processor (YAML parsing version).
///
/// Supports flexible YAML formats:
/// - Simple string: `execution: reactive` or `execution: manual`
/// - Object for continuous with interval: `execution: { type: continuous, interval_ms: 10 }`
/// - Object for continuous default interval: `execution: continuous` (interval_ms defaults to 0)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProcessorSchemaExecution {
    /// Reactive: process() is called when input arrives.
    #[default]
    Reactive,
    /// Manual: start()/stop() control execution.
    Manual,
    /// Continuous: runs in a loop calling process() repeatedly.
    Continuous {
        /// Minimum interval between process() calls in milliseconds.
        interval_ms: u32,
    },
}

impl ProcessorSchemaExecution {
    /// Returns the interval_ms for Continuous mode, or None for other modes.
    pub fn interval_ms(&self) -> Option<u32> {
        match self {
            ProcessorSchemaExecution::Continuous { interval_ms } => Some(*interval_ms),
            _ => None,
        }
    }
}

impl Serialize for ProcessorSchemaExecution {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        match self {
            ProcessorSchemaExecution::Reactive => serializer.serialize_str("reactive"),
            ProcessorSchemaExecution::Manual => serializer.serialize_str("manual"),
            ProcessorSchemaExecution::Continuous { interval_ms } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "continuous")?;
                map.serialize_entry("interval_ms", interval_ms)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ProcessorSchemaExecution {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};

        struct ProcessorSchemaExecutionVisitor;

        impl<'de> Visitor<'de> for ProcessorSchemaExecutionVisitor {
            type Value = ProcessorSchemaExecution;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a string ('reactive', 'manual', 'continuous') or an object with 'type' field",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<ProcessorSchemaExecution, E>
            where
                E: de::Error,
            {
                match value.to_lowercase().as_str() {
                    "reactive" => Ok(ProcessorSchemaExecution::Reactive),
                    "manual" => Ok(ProcessorSchemaExecution::Manual),
                    "continuous" => Ok(ProcessorSchemaExecution::Continuous { interval_ms: 0 }),
                    _ => Err(de::Error::unknown_variant(
                        value,
                        &["reactive", "manual", "continuous"],
                    )),
                }
            }

            fn visit_map<M>(self, mut map: M) -> Result<ProcessorSchemaExecution, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut type_field: Option<String> = None;
                let mut interval_ms: Option<u32> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => {
                            type_field = Some(map.next_value()?);
                        }
                        "interval_ms" => {
                            interval_ms = Some(map.next_value()?);
                        }
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }

                let type_val = type_field.ok_or_else(|| de::Error::missing_field("type"))?;
                match type_val.to_lowercase().as_str() {
                    "reactive" => Ok(ProcessorSchemaExecution::Reactive),
                    "manual" => Ok(ProcessorSchemaExecution::Manual),
                    "continuous" => Ok(ProcessorSchemaExecution::Continuous {
                        interval_ms: interval_ms.unwrap_or(0),
                    }),
                    _ => Err(de::Error::unknown_variant(
                        &type_val,
                        &["reactive", "manual", "continuous"],
                    )),
                }
            }
        }

        deserializer.deserialize_any(ProcessorSchemaExecutionVisitor)
    }
}

impl JsonSchema for ProcessorSchemaExecution {
    fn schema_name() -> String {
        "ProcessorSchemaExecution".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_processor_schema::ProcessorSchemaExecution")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let simple = Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            enum_values: Some(vec![
                serde_json::Value::String("reactive".into()),
                serde_json::Value::String("manual".into()),
                serde_json::Value::String("continuous".into()),
            ]),
            ..Default::default()
        });
        let continuous_object = {
            use schemars::schema::{ObjectValidation, SingleOrVec};
            let mut props = schemars::Map::new();
            props.insert(
                "type".into(),
                Schema::Object(SchemaObject {
                    instance_type: Some(InstanceType::String.into()),
                    enum_values: Some(vec![serde_json::Value::String("continuous".into())]),
                    ..Default::default()
                }),
            );
            props.insert(
                "interval_ms".into(),
                Schema::Object(SchemaObject {
                    instance_type: Some(SingleOrVec::Single(Box::new(InstanceType::Integer))),
                    number: Some(Box::new(schemars::schema::NumberValidation {
                        minimum: Some(0.0),
                        ..Default::default()
                    })),
                    ..Default::default()
                }),
            );
            Schema::Object(SchemaObject {
                instance_type: Some(InstanceType::Object.into()),
                object: Some(Box::new(ObjectValidation {
                    properties: props,
                    required: ["type".to_string()].into_iter().collect(),
                    additional_properties: Some(Box::new(Schema::Bool(false))),
                    ..Default::default()
                })),
                ..Default::default()
            })
        };
        Schema::Object(SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "Execution mode: `reactive`, `manual`, `continuous` (string), or `{ type: continuous, interval_ms: N }`."
                        .into(),
                ),
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                one_of: Some(vec![simple, continuous_object]),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

/// A port definition within a processor schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProcessorPortSchema {
    /// Port name (e.g., "video_in").
    pub name: String,
    /// Structured schema spec — either `any` or a 4-field [`SchemaIdent`].
    pub schema: PortSchemaSpec,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Read mode for this input port (e.g., "skip_to_latest", "read_next_in_order").
    #[serde(default)]
    pub read_mode: Option<String>,
    /// Ring buffer capacity for this input port.
    #[serde(default)]
    pub buffer_size: Option<usize>,
}

/// Config definition within a processor schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProcessorConfigSchema {
    /// Config field name (e.g., "config").
    pub name: String,
    /// Schema reference with version (e.g., "com.example.blur.config@1.0.0").
    pub schema: String,
}

/// A state field definition within a processor schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProcessorStateField {
    /// Field name (e.g., "buffer").
    pub name: String,
    /// Rust type (e.g., "Vec<f32>", "u32", "u64").
    #[serde(rename = "type")]
    pub field_type: String,
    /// Default value expression (e.g., "Vec::new()", "0", "0").
    #[serde(default)]
    pub default: Option<String>,
}

/// A complete processor schema definition parsed from YAML.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProcessorSchema {
    /// Processor name in reverse domain notation (e.g., "com.example.blur").
    pub name: String,

    /// Processor version (e.g., "1.0.0").
    pub version: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Runtime configuration (language, options, env).
    #[serde(default)]
    pub runtime: RuntimeConfig,

    /// Entrypoint for non-Rust runtimes (e.g., "src.blur:BlurProcessor").
    #[serde(default)]
    pub entrypoint: Option<String>,

    /// Execution mode (reactive, manual, continuous).
    #[serde(default)]
    pub execution: ProcessorSchemaExecution,

    /// Config schema reference.
    #[serde(default)]
    pub config: Option<ProcessorConfigSchema>,

    /// State fields (internal processor state, Default-initialized).
    #[serde(default)]
    pub state: Vec<ProcessorStateField>,

    /// Input port definitions.
    #[serde(default)]
    pub inputs: Vec<ProcessorPortSchema>,

    /// Output port definitions.
    #[serde(default)]
    pub outputs: Vec<ProcessorPortSchema>,
}

impl ProcessorSchema {
    /// Returns the full processor name with version (e.g., "com.example.blur@1.0.0").
    pub fn full_name(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }

    /// Computes the processor ID (hash of full name).
    pub fn processor_id(&self) -> u64 {
        compute_schema_id(&self.full_name())
    }

    /// Returns the Rust struct name derived from the processor name.
    pub fn rust_struct_name(&self) -> String {
        let last_segment = self.name.split('.').next_back().unwrap_or(&self.name);
        to_pascal_case(last_segment)
    }

    /// Returns the Rust module name derived from the processor name.
    pub fn rust_module_name(&self) -> String {
        self.name.replace('.', "_").to_lowercase()
    }
}

/// Compute schema ID from full name (first 8 bytes of SHA-256).
pub fn compute_schema_id(full_name: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(full_name.as_bytes());
    let result = hasher.finalize();
    u64::from_be_bytes(result[0..8].try_into().unwrap())
}

/// Convert a string to PascalCase.
pub fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' || c == '-' || c == '.' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    result
}

/// Convert a string to snake_case.
pub fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    let mut prev_was_upper = false;

    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !prev_was_upper {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
            prev_was_upper = true;
        } else {
            result.push(c);
            prev_was_upper = false;
        }
    }

    result
}
