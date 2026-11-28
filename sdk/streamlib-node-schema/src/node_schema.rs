// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use crate::ThreadPriority;

/// Runtime language for a node.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeLanguage {
    #[default]
    Rust,
    Python,
}

impl JsonSchema for NodeLanguage {
    fn schema_name() -> Cow<'static, str> {
        "NodeLanguage".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_node_schema::NodeLanguage")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Node runtime language. `deno` is accepted as an alias for `typescript`.",
            "type": "string",
            "enum": ["rust", "python", "typescript", "deno"]
        })
    }
}

/// Language-specific runtime options.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeOptions {
    /// [Python] Required Python version spec (e.g., ">=3.12"). Python runtime only.
    #[serde(default)]
    pub python_version: Option<String>,
}

/// Internal helper for deserializing RuntimeConfig from either string or object.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RuntimeConfigHelper {
    /// Simple string form: `runtime: rust`
    Simple(NodeLanguage),
    /// Full object form: `runtime: { language: python, options: { python_version: ">=3.12" } }`
    Full(RuntimeConfigFull),
}

/// Full runtime configuration object.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfigFull {
    /// Language runtime (rust, python, typescript). Defaults to rust.
    #[serde(default)]
    pub language: NodeLanguage,

    /// Language-specific options.
    #[serde(default)]
    pub options: RuntimeOptions,

    /// Environment variables for subprocess runtimes.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

/// Runtime configuration for a node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Language runtime (rust, python, typescript). Defaults to rust.
    pub language: NodeLanguage,

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
    fn schema_name() -> Cow<'static, str> {
        "RuntimeConfig".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_node_schema::RuntimeConfig")
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let simple = generator.subschema_for::<NodeLanguage>();
        let full = generator.subschema_for::<RuntimeConfigFull>();
        json_schema!({
            "description": "Runtime configuration: either a bare language string (`rust`, `python`, `typescript`) or a `{ language, options, env }` object.",
            "oneOf": [simple, full]
        })
    }
}

/// Execution mode for a node (YAML parsing version).
///
/// Supports flexible YAML formats:
/// - Simple string: `execution: reactive` or `execution: manual`
/// - Object for continuous with interval: `execution: { type: continuous, interval_ms: 10 }`
/// - Object for continuous default interval: `execution: continuous` (interval_ms defaults to 0)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum NodeSchemaExecution {
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

impl NodeSchemaExecution {
    /// Returns the interval_ms for Continuous mode, or None for other modes.
    pub fn interval_ms(&self) -> Option<u32> {
        match self {
            NodeSchemaExecution::Continuous { interval_ms } => Some(*interval_ms),
            _ => None,
        }
    }
}

impl Serialize for NodeSchemaExecution {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        match self {
            NodeSchemaExecution::Reactive => serializer.serialize_str("reactive"),
            NodeSchemaExecution::Manual => serializer.serialize_str("manual"),
            NodeSchemaExecution::Continuous { interval_ms } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("type", "continuous")?;
                map.serialize_entry("interval_ms", interval_ms)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for NodeSchemaExecution {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, MapAccess, Visitor};

        struct NodeSchemaExecutionVisitor;

        impl<'de> Visitor<'de> for NodeSchemaExecutionVisitor {
            type Value = NodeSchemaExecution;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a string ('reactive', 'manual', 'continuous') or an object with 'type' field",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<NodeSchemaExecution, E>
            where
                E: de::Error,
            {
                match value.to_lowercase().as_str() {
                    "reactive" => Ok(NodeSchemaExecution::Reactive),
                    "manual" => Ok(NodeSchemaExecution::Manual),
                    "continuous" => Ok(NodeSchemaExecution::Continuous { interval_ms: 0 }),
                    _ => Err(de::Error::unknown_variant(
                        value,
                        &["reactive", "manual", "continuous"],
                    )),
                }
            }

            fn visit_map<M>(self, mut map: M) -> Result<NodeSchemaExecution, M::Error>
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
                    "reactive" => Ok(NodeSchemaExecution::Reactive),
                    "manual" => Ok(NodeSchemaExecution::Manual),
                    "continuous" => Ok(NodeSchemaExecution::Continuous {
                        interval_ms: interval_ms.unwrap_or(0),
                    }),
                    _ => Err(de::Error::unknown_variant(
                        &type_val,
                        &["reactive", "manual", "continuous"],
                    )),
                }
            }
        }

        deserializer.deserialize_any(NodeSchemaExecutionVisitor)
    }
}

impl JsonSchema for NodeSchemaExecution {
    fn schema_name() -> Cow<'static, str> {
        "NodeSchemaExecution".into()
    }
    fn schema_id() -> Cow<'static, str> {
        Cow::Borrowed("streamlib_node_schema::NodeSchemaExecution")
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Execution mode: `reactive`, `manual`, `continuous` (string), or `{ type: continuous, interval_ms: N }`.",
            "oneOf": [
                {
                    "type": "string",
                    "enum": ["reactive", "manual", "continuous"]
                },
                {
                    "type": "object",
                    "properties": {
                        "type": { "type": "string", "enum": ["continuous"] },
                        "interval_ms": { "type": "integer", "minimum": 0.0 }
                    },
                    "required": ["type"],
                    "additionalProperties": false
                }
            ]
        })
    }
}

/// The borrow quota an input port takes when it states none.
pub const DEFAULT_INPUT_PORT_BORROW_QUOTA: usize = 2;

/// The largest borrow quota an input port may state. Every channel data
/// service is created to admit it, because iceoryx2 fixes a service's borrow
/// limit when the first opener creates it.
pub const MAX_INPUT_PORT_BORROW_QUOTA: usize = 8;

/// The delivery profile that reads only the newest message an input has received.
pub const NEWEST_DELIVERY_PROFILE: &str = "newest";

/// The delivery profile that reads every message an input receives, in order.
pub const ORDERED_DELIVERY_PROFILE: &str = "ordered";

pub const DELIVERY_PROFILE_DECLARATION_VALUES: [&str; 2] =
    [NEWEST_DELIVERY_PROFILE, ORDERED_DELIVERY_PROFILE];

/// A port definition within a node schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodePortSchema {
    /// Port name (e.g., "video_in").
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Delivery profile declared by this input port — `"newest"` or
    /// `"ordered"`. The one delivery knob on the authoring surface: it names
    /// a read policy and resolves to the consumer-side drain order and the
    /// ring depth. Optional on an input port with a pin, which reads by its
    /// pin's default, required on every other input port, and always `None`
    /// on an output port.
    #[serde(default)]
    pub delivery_profile: Option<String>,
    /// The samples each link into this input port may hold beyond its mailbox,
    /// or `None` for the engine's default. Absent from the rendering on a port
    /// that declares none, and always `None` on an output port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borrow_quota: Option<usize>,
    /// Window contract declared by this audio input port, or `None`. Absent
    /// from the rendering entirely on a port that declares none, and always
    /// `None` on an output port: a producer publishes what it has, and only a
    /// consumer states what it needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_window: Option<crate::AudioWindowContract>,
    /// What an output states of its stream as the annotations of its pin.
    #[serde(default)]
    pub annotations: crate::pin_type::PinAnnotations,
    /// The pin type this port declares; `None` when a value input chooses it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_type: Option<crate::pin_type::PinType>,
    /// The value input whose chosen pin type this port carries, as `pin_type = from_input(…)`
    /// states it; `None` when the port states its type itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_type_from_input: Option<String>,
    /// Whether a value set live on this input is kept across Stop and a restart of the runtime,
    /// or reset at Stop; kept when it declares none, and on every output port.
    #[serde(default, skip_serializing_if = "crate::NodeValueLifetime::is_kept")]
    #[schemars(with = "Option<String>")]
    pub lifetime: crate::NodeValueLifetime,
    /// The flow this port states — `value`, `stream` or `trigger`; `None` when it reads in its
    /// pin type's default flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub flow: Option<crate::pin_type::PinFlow>,
    /// Names this port had before, each of which still finds it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub former_names: Vec<String>,
    /// Whether this input may hold no value, and then reads as nothing; never on an output.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub may_be_unset: bool,
    /// Whether this input takes a value set on it as well as a wire, its node reading the value
    /// while no wire carries it; never on an output.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub takes_a_value: bool,
}

/// Config definition within a node schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeConfigSchema {
    /// Config field name (e.g., "config").
    pub name: String,
    /// The config type's name (e.g. `H264EncoderConfig`). Descriptor metadata
    /// only — there is no schema layer for it to resolve against.
    pub schema: String,
}

/// A state field definition within a node schema.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeStateField {
    /// Field name (e.g., "buffer").
    pub name: String,
    /// Rust type (e.g., "Vec<f32>", "u32", "u64").
    #[serde(rename = "type")]
    pub field_type: String,
    /// Default value expression (e.g., "Vec::new()", "0", "0").
    #[serde(default)]
    pub default: Option<String>,
}

/// Declarative scheduling intent for a node — replaces the substring-
/// matching heuristic that picked priority by node short name.
///
/// Optional; omission means [`ThreadPriority::Normal`]. Authors do not name
/// node threads: the compiler leaves the OS thread name unset, because
/// Linux truncates it at 15 characters and the node id in the log lines
/// identifies the instance without it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeScheduling {
    /// Thread priority. Defaults to [`ThreadPriority::Normal`] when absent.
    #[serde(default)]
    pub priority: ThreadPriority,
}

/// A complete node schema definition — the manifest-shaped view of one
/// node, derived from its `#[node(...)]` attribute (the source of
/// truth) by the source-scan extractor.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeSchema {
    pub name: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// Runtime configuration (language, options, env).
    #[serde(default)]
    pub runtime: RuntimeConfig,

    /// Entrypoint for non-Rust runtimes (e.g., "src.blur:BlurNode").
    #[serde(default)]
    pub entrypoint: Option<String>,

    /// Execution mode (reactive, manual, continuous).
    #[serde(default)]
    pub execution: NodeSchemaExecution,

    /// Declarative scheduling intent. Absent → `Normal` priority.
    #[serde(default)]
    pub scheduling: Option<NodeScheduling>,

    /// Whether the node paces itself on real time, as a live device does.
    #[serde(default)]
    pub paced_by_real_time: bool,

    /// Config schema reference.
    #[serde(default)]
    pub config: Option<NodeConfigSchema>,

    /// State fields (internal node state, Default-initialized).
    #[serde(default)]
    pub state: Vec<NodeStateField>,

    /// Input port definitions.
    #[serde(default)]
    pub inputs: Vec<NodePortSchema>,

    /// Output port definitions.
    #[serde(default)]
    pub outputs: Vec<NodePortSchema>,
}

impl NodeSchema {
    pub fn rust_struct_name(&self) -> String {
        let last_segment = self.name.split('.').next_back().unwrap_or(&self.name);
        to_pascal_case(last_segment)
    }

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
