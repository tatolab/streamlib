// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading one described node type — the `@node` stamps a processor
//! interpreter printed for a class — into what the engine registers.
//!
//! The keys of a described node type are the `__tatolab_node_*__` attributes
//! `tatolab/stream/_node_declaration.py` stamps, carried verbatim by the
//! bootstrap's `--describe`; the two move together.

use crate::core::descriptors::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AudioWindowContract,
    AudioWindowContractDeclaredValues, PortDescriptor, ProcessorClassImportPath,
    ProcessorClassShortName, ProcessorDescriptor, ProcessorRuntime, ProcessorScheduling,
    refuse_audio_window_beside_a_skipping_delivery_profile,
};
use crate::core::execution::{ExecutionConfig, ProcessExecution, ThreadPriority};

const DESCRIBED_IMPORT_PATH_KEY: &str = "import_path";
const DESCRIBED_SHORT_NAME_KEY: &str = "short_name";
const DESCRIBED_DESCRIPTION_KEY: &str = "description";
const DESCRIBED_EXECUTION_KEY: &str = "execution";
const DESCRIBED_SCHEDULING_PRIORITY_KEY: &str = "scheduling_priority";
const DESCRIBED_CONFIG_SCHEMA_KEY: &str = "config_schema";
const DESCRIBED_INPUT_PORTS_KEY: &str = "input_ports";
const DESCRIBED_OUTPUT_PORTS_KEY: &str = "output_ports";

/// Everything the engine needs to register and instantiate one Python
/// processor class.
pub struct PythonProcessorDeclaration {
    /// The class's descriptor, registered under its import path.
    pub descriptor: ProcessorDescriptor,
    /// The execution the class declared, which its processor interpreter
    /// drives its own loop in.
    pub execution_config: ExecutionConfig,
}

impl PythonProcessorDeclaration {
    /// Read one entry of a describe document's `described_node_types`,
    /// refusing it — with the reason — when it is not the declaration of
    /// `requested_import_path`.
    pub fn read_from_described_node_type(
        described_node_type: &serde_json::Value,
        requested_import_path: &ProcessorClassImportPath,
    ) -> std::result::Result<Self, String> {
        let described_import_path = read_string(described_node_type, DESCRIBED_IMPORT_PATH_KEY)?;
        if described_import_path != requested_import_path.as_str() {
            return Err(format!(
                "the processor interpreter described `{described_import_path}` when \
                 `{requested_import_path}` was asked for"
            ));
        }
        let class_short_name = ProcessorClassShortName::new(read_string(
            described_node_type,
            DESCRIBED_SHORT_NAME_KEY,
        )?)
        .map_err(|blank| blank.to_string())?;
        let execution_config = read_execution_config(described_node_type)?;

        let mut descriptor = ProcessorDescriptor::new(
            class_short_name,
            requested_import_path.clone(),
            read_string(described_node_type, DESCRIBED_DESCRIPTION_KEY)?,
        )
        .with_runtime(ProcessorRuntime::Python)
        .with_entrypoint(requested_import_path.as_str())
        .with_scheduling(ProcessorScheduling {
            priority: read_thread_priority(described_node_type)?,
        })
        .with_config_schema(read_config_schema_document(described_node_type)?);

        descriptor.inputs = read_port_descriptors(described_node_type, PortDirection::Input)?;
        descriptor.outputs = read_port_descriptors(described_node_type, PortDirection::Output)?;

        Ok(Self {
            descriptor,
            execution_config,
        })
    }
}

/// The JSON Schema the decorator derived from the class's config class.
///
/// Derived in Python, where the config class is, and carried across as the
/// document the catalog serves — the engine never re-derives it and never
/// inspects it.
fn read_config_schema_document(
    described_node_type: &serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let document = described_field(described_node_type, DESCRIBED_CONFIG_SCHEMA_KEY)?;
    if !document.is_object() {
        return Err(format!(
            "{DESCRIBED_CONFIG_SCHEMA_KEY} must be a JSON object, got {} — the decorator derives \
             this document, so a class reaching here was built by hand rather than by \
             @tatolab.stream.node",
            json_kind_name(document)
        ));
    }
    Ok(document.clone())
}

fn read_execution_config(
    described_node_type: &serde_json::Value,
) -> std::result::Result<ExecutionConfig, String> {
    let execution = described_field(described_node_type, DESCRIBED_EXECUTION_KEY)?;
    if !execution.is_object() {
        return Err(format!("{DESCRIBED_EXECUTION_KEY} must be a dict"));
    }
    let execution = match read_string(execution, "mode")?.as_str() {
        "reactive" => ProcessExecution::Reactive,
        "manual" => ProcessExecution::Manual,
        "continuous" => ProcessExecution::Continuous {
            interval_ms: match execution.get("interval_ms") {
                None => 0,
                Some(interval_ms) => interval_ms
                    .as_u64()
                    .and_then(|interval_ms| u32::try_from(interval_ms).ok())
                    .ok_or_else(|| {
                        format!("{DESCRIBED_EXECUTION_KEY}.interval_ms must be an int")
                    })?,
            },
        },
        unknown => {
            return Err(format!(
                "unknown execution mode {unknown:?} — the decorator validates this, so a class \
                 reaching here was built by hand rather than by @tatolab.stream.node"
            ));
        }
    };
    Ok(ExecutionConfig::new(execution))
}

fn read_thread_priority(
    described_node_type: &serde_json::Value,
) -> std::result::Result<ThreadPriority, String> {
    let priority = described_field(described_node_type, DESCRIBED_SCHEDULING_PRIORITY_KEY)?;
    if priority.is_null() {
        return Ok(ThreadPriority::Normal);
    }
    let priority = priority.as_str().ok_or_else(|| {
        format!(
            "{DESCRIBED_SCHEDULING_PRIORITY_KEY} must be a string or null, got {}",
            json_kind_name(priority)
        )
    })?;
    match priority {
        "realtime" => Ok(ThreadPriority::RealTime),
        "high" => Ok(ThreadPriority::High),
        "normal" => Ok(ThreadPriority::Normal),
        unknown => Err(format!("unknown scheduling priority {unknown:?}")),
    }
}

#[derive(Clone, Copy)]
enum PortDirection {
    Input,
    Output,
}

impl PortDirection {
    fn described_ports_key(self) -> &'static str {
        match self {
            Self::Input => DESCRIBED_INPUT_PORTS_KEY,
            Self::Output => DESCRIBED_OUTPUT_PORTS_KEY,
        }
    }
}

fn read_port_descriptors(
    described_node_type: &serde_json::Value,
    direction: PortDirection,
) -> std::result::Result<Vec<PortDescriptor>, String> {
    let ports_key = direction.described_ports_key();
    let declared = described_field(described_node_type, ports_key)?
        .as_array()
        .ok_or_else(|| format!("{ports_key} must be a list"))?;

    let mut ports = Vec::with_capacity(declared.len());
    for declaration in declared {
        if !declaration.is_object() {
            return Err(format!("{ports_key} must hold dicts"));
        }

        let mut port = PortDescriptor::new(
            read_string(declaration, "name")?,
            read_string(declaration, "description")?,
            true,
        );
        if let Some(delivery_profile) = declaration
            .get("delivery_profile")
            .filter(|declared| !declared.is_null())
        {
            let delivery_profile = delivery_profile.as_str().ok_or_else(|| {
                format!(
                    "port {:?}: delivery_profile must be a string, got {}",
                    port.name,
                    json_kind_name(delivery_profile)
                )
            })?;
            port = port.with_delivery_profile(delivery_profile);
        }
        if let Some(audio_window) = declaration
            .get("audio_window")
            .filter(|declared| !declared.is_null())
        {
            if matches!(direction, PortDirection::Output) {
                return Err(format!(
                    "output port {:?} declares an audio_window — a producer publishes what it \
                     has, and only a consuming input port states the window it needs",
                    port.name
                ));
            }
            let contract = read_audio_window_contract(
                audio_window,
                &port.name,
                port.delivery_profile.as_deref(),
            )?;
            port = port.with_audio_window_contract(contract);
        }
        ports.push(port);
    }
    Ok(ports)
}

/// Read one `audio_window` declaration off a described port.
///
/// The decorator validates first, so this is not the only guard — it is the
/// guard that holds when the marker was built by something other than the
/// decorator, and it renders its refusals from the same shared validator the
/// `#[processor]` grammar uses.
fn read_audio_window_contract(
    audio_window: &serde_json::Value,
    port_name: &str,
    delivery_profile: Option<&str>,
) -> std::result::Result<AudioWindowContract, String> {
    if !audio_window.is_object() {
        return Err(format!(
            "input port {port_name:?}: audio_window must be a dict"
        ));
    }

    refuse_audio_window_beside_a_skipping_delivery_profile(delivery_profile)
        .map_err(|refusal| format!("input port {port_name:?}: {refusal}"))?;

    let resolved_from = read_audio_window_string_field(audio_window, "resolved_from", port_name)?;
    match resolved_from.as_str() {
        "match_device" => Ok(AudioWindowContract::MatchDevice {}),
        "declaration" => {
            let values = AudioWindowContractDeclaredValues {
                sample_rate: read_audio_window_numeric_field(
                    audio_window,
                    "sample_rate",
                    port_name,
                )?,
                channels: read_audio_window_channel_count(audio_window, port_name)?,
                dtype: read_audio_window_string_field(audio_window, "dtype", port_name)?,
                window_size: read_audio_window_numeric_field(
                    audio_window,
                    "window_size",
                    port_name,
                )?,
                hop: read_audio_window_numeric_field(audio_window, "hop", port_name)?,
            };
            values
                .refuse_if_unhonourable()
                .map_err(|refusal| format!("input port {port_name:?}: {refusal}"))?;
            Ok(AudioWindowContract::Declaration(values))
        }
        other => Err(format!(
            "input port {port_name:?}: audio_window `resolved_from` is {other:?} — expected \
             \"declaration\" or \"match_device\""
        )),
    }
}

/// Read the `audio_window` channel count an author declared, or `None` where
/// they left it to the source.
///
/// The count is the one value a contract may omit, so an absent key is legal
/// here where every other field's absence is refused by name.
fn read_audio_window_channel_count(
    audio_window: &serde_json::Value,
    port_name: &str,
) -> std::result::Result<Option<u32>, String> {
    let Some(value) = audio_window.get("channels") else {
        return Ok(None);
    };
    read_a_channel_count_or_the_source_spelling(value).map_err(|refusal| {
        refusal.framed_as(format!(
            "input port {port_name:?}: audio_window field \"channels\""
        ))
    })
}

/// Why an `audio_window` field could not be read: the tail of the sentence,
/// and whether the value was the wrong kind of thing or an unusable one.
///
/// The kind travels with the refusal so a language host can raise the same
/// mistake as the same exception whichever field it was made on —
/// `channels=1.5` and `window_size=1.5` are one error, not two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioWindowFieldRefusal {
    /// The value is not the kind of thing the field takes at all.
    WrongKindOfValue(String),
    /// The right kind, but not one the field may hold.
    UnusableValue(String),
}

impl AudioWindowFieldRefusal {
    /// The refusal behind the caller's own naming of what was being read.
    pub fn framed_as(self, naming: impl std::fmt::Display) -> String {
        match self {
            Self::WrongKindOfValue(reason) | Self::UnusableValue(reason) => {
                format!("{naming} {reason}")
            }
        }
    }
}

/// Read a `channels` value written either as a count or as the
/// source-following spelling.
///
/// The one parse both readers call — a described declaration, and a processor
/// interpreter's reading of what the parent wired. The refusal comes back bare
/// so each frames it in its own terms, the way the contract's own validator
/// does: one names a declaration, the other names a wiring.
pub fn read_a_channel_count_or_the_source_spelling(
    value: &serde_json::Value,
) -> std::result::Result<Option<u32>, AudioWindowFieldRefusal> {
    if let Some(spelling) = value.as_str() {
        if spelling == AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE {
            return Ok(None);
        }
        return Err(AudioWindowFieldRefusal::UnusableValue(format!(
            "is {spelling:?} — expected a channel count, or \
             {AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE:?} to carry whatever count the \
             source sends"
        )));
    }

    // The sentinel is offered on top of whatever the shared core said, rather
    // than in place of it: "must be an int" and "must be an int, and a bool is
    // not one" are different things to tell an author, and only the second one
    // explains why `True` was rejected.
    a_strictly_positive_count(value)
        .map(Some)
        .map_err(|refusal| match refusal {
            AudioWindowFieldRefusal::WrongKindOfValue(reason) => {
                AudioWindowFieldRefusal::WrongKindOfValue(format!(
                    "{reason} — or {AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE:?} to carry \
                     whatever count the source sends"
                ))
            }
            unusable => unusable,
        })
}

/// One strictly-positive count off a value, telling a value of the wrong kind
/// apart from a number the field cannot hold.
///
/// The shared core of every numeric `audio_window` field, `channels` included,
/// so one spelling of "strictly positive" serves them all.
fn a_strictly_positive_count(
    value: &serde_json::Value,
) -> std::result::Result<u32, AudioWindowFieldRefusal> {
    // A Python `bool` is an `int` subclass, so `True` would otherwise reach the
    // stage as a plausible count of one. The Python constructor refuses one by
    // name; this is the same rule at the seams that constructor does not guard
    // — a hand-built marker, and the envelope a parent wires a child with.
    if value.is_boolean() {
        return Err(AudioWindowFieldRefusal::WrongKindOfValue(
            "must be an int, and a bool is not one".to_string(),
        ));
    }
    let declared = value
        .as_i64()
        .ok_or_else(|| AudioWindowFieldRefusal::WrongKindOfValue("must be an int".to_string()))?;
    u32::try_from(declared).map_err(|_| {
        AudioWindowFieldRefusal::UnusableValue(format!(
            "is {declared} — every numeric field is strictly positive"
        ))
    })
}

/// Read one strictly-positive `audio_window` numeric field, refusing a
/// negative integer by name rather than as an extraction failure.
fn read_audio_window_numeric_field(
    audio_window: &serde_json::Value,
    key: &str,
    port_name: &str,
) -> std::result::Result<u32, String> {
    let value = audio_window_field(audio_window, key, port_name)?;
    a_strictly_positive_count(value).map_err(|refusal| {
        refusal.framed_as(format!(
            "input port {port_name:?}: audio_window field {key:?}"
        ))
    })
}

/// Read one `audio_window` string field, naming the port the way every other
/// field of the contract does.
fn read_audio_window_string_field(
    audio_window: &serde_json::Value,
    key: &str,
    port_name: &str,
) -> std::result::Result<String, String> {
    audio_window_field(audio_window, key, port_name)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            format!("input port {port_name:?}: audio_window field {key:?} must be a string")
        })
}

/// One missing-`audio_window`-field refusal, so no field of the contract
/// falls through to a bare `missing key` with no port and no contract named.
fn audio_window_field<'a>(
    audio_window: &'a serde_json::Value,
    key: &str,
    port_name: &str,
) -> std::result::Result<&'a serde_json::Value, String> {
    audio_window.get(key).ok_or_else(|| {
        format!(
            "input port {port_name:?}: audio_window is missing {key:?} — the contract is \
             all-or-nothing"
        )
    })
}

fn described_field<'a>(
    described: &'a serde_json::Value,
    key: &str,
) -> std::result::Result<&'a serde_json::Value, String> {
    described
        .get(key)
        .ok_or_else(|| format!("missing key {key:?}"))
}

fn read_string(described: &serde_json::Value, key: &str) -> std::result::Result<String, String> {
    let value = described_field(described, key)?;
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{key:?} must be a string, got {}", json_kind_name(value)))
}

/// What kind of JSON value `value` is, as a refusal names it.
fn json_kind_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a bool",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "a list",
        serde_json::Value::Object(_) => "an object",
    }
}
