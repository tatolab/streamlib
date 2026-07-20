// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, Schema, SchemaObject, SubschemaValidation};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use streamlib_idents::{SchemaIdent, TypeName};

use crate::ThreadPriority;

// ============================================================================
// Processor Schema Types
// ============================================================================

/// Schema spec for a port — tri-state across the use-site lifecycle.
///
/// YAML accepts exactly two string shapes at use-sites:
/// - `schema: any` — wildcard (any payload accepted)
/// - `schema: <BarePascalCaseTypeName>` (e.g. `VideoFrame`) — bare type-name
///   reference resolved against the enclosing manifest's `schemas:` map at
///   proc-macro expansion / runtime startup
///
/// The structured 4-field map (`{ org, package, type, version }`) and any
/// joined-string shorthand (`'@org/pkg/Type@version'`) are rejected at the
/// parser boundary — declare the type once in the manifest's `schemas:`
/// map and reference it by bare name at every use-site. See
/// `docs/architecture/schema-identity-and-packaging.md`.
///
/// The [`PortSchemaSpec::Specific`] variant is constructed downstream of
/// the parser (proc-macro expansion / manifest resolution) once a bare
/// [`Named`](PortSchemaSpec::Named) reference has been resolved against
/// the manifest's `schemas:` map. The wire layer, generated code, and
/// `ProcessorDescriptor` continue to carry [`SchemaIdent`] verbatim.
///
/// `Default` resolves to [`PortSchemaSpec::Any`] — the most permissive
/// shape. Used by callers that build a `PortInfo` before the routing tag
/// is known (e.g. graph-builder fallbacks); a default `Any` carries no
/// false specificity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum PortSchemaSpec {
    #[default]
    Any,
    /// Bare PascalCase type-name reference (e.g. `VideoFrame`), unresolved.
    /// Produced by the YAML parser; resolved to [`Specific`](Self::Specific)
    /// downstream against the enclosing manifest's `schemas:` map.
    Named(TypeName),
    /// Fully-qualified schema identifier — the resolved post-lookup form.
    Specific(SchemaIdent),
}

impl Serialize for PortSchemaSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            PortSchemaSpec::Any => serializer.serialize_str("any"),
            PortSchemaSpec::Named(name) => serializer.serialize_str(name.as_str()),
            // Round-trips to `Named` on next parse — by the time a spec has
            // been resolved to `Specific`, the full SchemaIdent is the
            // source of truth and re-serialization to YAML is uncommon.
            PortSchemaSpec::Specific(ident) => serializer.serialize_str(ident.r#type.as_str()),
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
            serde_yaml::Value::String(s) => {
                let name = TypeName::new(s.clone()).map_err(|e| {
                    D::Error::custom(format!(
                        "port schema `{}` is not a valid bare PascalCase TypeName \
                         (must match `^[A-Z][A-Za-z0-9]*$`): {}. Declare the type \
                         once in the manifest's `schemas:` map and reference it by \
                         bare name (`docs/architecture/schema-identity-and-packaging.md`).",
                        s, e
                    ))
                })?;
                Ok(PortSchemaSpec::Named(name))
            }
            serde_yaml::Value::Mapping(_) => Err(D::Error::custom(
                "port schema must be either `any` or a bare PascalCase TypeName \
                 string (e.g. `VideoFrame`); the structured 4-field map form \
                 (`{ org, package, type, version }`) is no longer accepted. \
                 Declare the type once in the manifest's `schemas:` map and \
                 reference it by bare name (`docs/architecture/schema-identity-and-packaging.md`).",
            )),
            other => Err(D::Error::custom(format!(
                "port schema must be either `any` or a bare PascalCase TypeName string; got {:?}",
                other
            ))),
        }
    }
}

impl PortSchemaSpec {
    /// Returns the inner [`SchemaIdent`] only when this spec has been
    /// resolved to its fully-qualified form. Returns `None` for both
    /// [`Any`](Self::Any) and unresolved [`Named`](Self::Named) variants.
    pub fn specific(&self) -> Option<&SchemaIdent> {
        match self {
            PortSchemaSpec::Specific(ident) => Some(ident),
            PortSchemaSpec::Any | PortSchemaSpec::Named(_) => None,
        }
    }
}

impl std::fmt::Display for PortSchemaSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PortSchemaSpec::Any => f.write_str("any"),
            PortSchemaSpec::Named(name) => f.write_str(name.as_str()),
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
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        let any_literal = Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            enum_values: Some(vec![serde_json::Value::String("any".into())]),
            ..Default::default()
        });
        let bare_typename = Schema::Object(SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            string: Some(Box::new(schemars::schema::StringValidation {
                pattern: Some(r"^[A-Z][A-Za-z0-9]*$".to_string()),
                ..Default::default()
            })),
            ..Default::default()
        });
        Schema::Object(SchemaObject {
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "Either the literal `any` (wildcard, accepts any payload) or a bare PascalCase TypeName resolved against the enclosing manifest's `schemas:` map."
                        .into(),
                ),
                ..Default::default()
            })),
            subschemas: Some(Box::new(SubschemaValidation {
                one_of: Some(vec![any_literal, bare_typename]),
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ProcessorPortSchema {
    /// Port name (e.g., "video_in").
    pub name: String,
    /// Schema spec — either `any` or a bare PascalCase TypeName resolved
    /// against the enclosing manifest's `schemas:` map.
    pub schema: PortSchemaSpec,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Read mode for this input port (e.g., "skip_to_latest", "read_next_in_order").
    #[serde(default)]
    pub read_mode: Option<String>,
    /// Producer-side overflow policy for the service feeding this input
    /// port. `"drop_oldest"` (the engine-wide realtime default) lets
    /// the publisher's `send()` never block — the iceoryx2 subscriber
    /// buffer evicts the oldest sample to make room. `"block"` keeps
    /// the producer waiting until the consumer drains a slot; reserve
    /// for sinks that need every sample in order (file writers,
    /// muxers, loggers).
    #[serde(default)]
    pub overflow: Option<String>,
    /// Ring buffer capacity for this input port.
    #[serde(default)]
    pub buffer_size: Option<usize>,
}

/// Config definition within a processor schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessorConfigSchema {
    /// Config field name (e.g., "config").
    pub name: String,
    /// Bare PascalCase TypeName (e.g. `H264EncoderConfig`). Resolved
    /// against the enclosing manifest's `schemas:` map at proc-macro
    /// expansion / runtime startup.
    pub schema: TypeName,
}

/// A state field definition within a processor schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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

/// Declarative scheduling intent for a processor — replaces the substring-
/// matching heuristic that picked priority by processor short name.
///
/// Optional; omission means [`ThreadPriority::Normal`]. The OS thread name
/// is always auto-generated by the compiler from the processor's
/// PascalCase short name plus its instance id (`{TypeName}-{node_id}`),
/// so it's both unique and traceable to the processor instance — authors
/// don't choose thread names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessorScheduling {
    /// Thread priority. Defaults to [`ThreadPriority::Normal`] when absent.
    #[serde(default)]
    pub priority: ThreadPriority,
}

/// A complete processor schema definition — the manifest-shaped view of one
/// processor, derived from its `#[processor(...)]` attribute (the source of
/// truth) by the source-scan extractor.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessorSchema {
    /// The `Type` segment of the processor's `@org/package/Type` identity — the
    /// PascalCase short name (e.g. "Camera"), NOT reverse-DNS.
    pub name: String,

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

    /// Declarative scheduling intent. Absent → `Normal` priority, default
    /// `processor-{id}` thread name.
    #[serde(default)]
    pub scheduling: Option<ProcessorScheduling>,

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
