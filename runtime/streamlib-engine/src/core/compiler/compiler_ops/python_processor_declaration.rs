// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading one described node type — the `@node.process` stamps a processor
//! interpreter printed for a class — into what the engine registers.
//!
//! The keys of a described node type are the `__tatolab_node_*__` attributes
//! `tatolab/stream/_node_declaration.py` stamps, carried verbatim by the
//! bootstrap's `--describe`; the two move together.

use crate::core::descriptors::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AudioWindowContract,
    AudioWindowContractDeclaredValues, CatalogEntry, FunctionNodeLanguage, NodeKind,
    PortDescriptor, PortFamilyDeclaration, PortFamilyDeclarationDirection,
    ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor, ProcessorRuntime,
    ProcessorScheduling, refuse_audio_window_beside_a_skipping_delivery_profile,
};
use crate::core::execution::{ExecutionConfig, ProcessExecution, ThreadPriority};
use crate::core::processors::refuse_catalog_entries_hang_does_not_read;
use crate::iceoryx2::MAX_INPUT_PORT_BORROW_QUOTA;

const DESCRIBED_IMPORT_PATH_KEY: &str = "import_path";
const DESCRIBED_SHORT_NAME_KEY: &str = "short_name";
const DESCRIBED_DESCRIPTION_KEY: &str = "description";
const DESCRIBED_KIND_KEY: &str = "kind";
const DESCRIBED_LANGUAGE_KEY: &str = "language";
const DESCRIBED_EXECUTION_KEY: &str = "execution";
const DESCRIBED_SCHEDULING_PRIORITY_KEY: &str = "scheduling_priority";
const DESCRIBED_PACED_BY_REAL_TIME_KEY: &str = "paced_by_real_time";
const DESCRIBED_CONFIG_SCHEMA_KEY: &str = "config_schema";
const DESCRIBED_INPUT_PORTS_KEY: &str = "input_ports";
const DESCRIBED_OUTPUT_PORTS_KEY: &str = "output_ports";

/// Everything the engine needs to register one Python node type and run its
/// nodes.
pub struct PythonProcessorDeclaration {
    /// The type's descriptor, registered under its import path.
    pub descriptor: ProcessorDescriptor,
    /// How a node of the type runs, by its kind.
    pub how_a_python_node_of_the_type_runs: HowAPythonNodeOfTheTypeRuns,
}

/// How a node of a described Python type runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HowAPythonNodeOfTheTypeRuns {
    /// A `@node.process` class, in its own processor interpreter, driven on
    /// the execution it declared.
    InItsOwnProcessorInterpreter(ExecutionConfig),
    /// A `@node.function`, inside the read of each node it feeds.
    InsideTheReadOfEachNodeItFeeds,
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
        let mut descriptor = ProcessorDescriptor::new(
            class_short_name,
            requested_import_path.clone(),
            read_string(described_node_type, DESCRIBED_DESCRIPTION_KEY)?,
        )
        .with_runtime(ProcessorRuntime::Python)
        .with_entrypoint(requested_import_path.as_str())
        .with_config_schema(read_config_schema_document(described_node_type)?);
        let how_a_python_node_of_the_type_runs =
            match read_described_node_kind(described_node_type)? {
                NodeKind::Process => {
                    descriptor = descriptor.with_scheduling(ProcessorScheduling {
                        priority: read_thread_priority(described_node_type)?,
                    });
                    if read_paced_by_real_time(described_node_type)? {
                        descriptor = descriptor.paced_by_real_time();
                    }
                    HowAPythonNodeOfTheTypeRuns::InItsOwnProcessorInterpreter(
                        read_execution_config(described_node_type)?,
                    )
                }
                function_kind @ NodeKind::Function { .. } => {
                    descriptor.node_kind = function_kind;
                    HowAPythonNodeOfTheTypeRuns::InsideTheReadOfEachNodeItFeeds
                }
                NodeKind::Shader => {
                    return Err(format!(
                        "{DESCRIBED_KIND_KEY} is \"shader\", which a processor interpreter never \
                     describes: a shader is Slang the engine compiles"
                    ));
                }
            };

        descriptor.inputs = read_port_descriptors(described_node_type, PortDirection::Input)?;
        descriptor.outputs = read_port_descriptors(described_node_type, PortDirection::Output)?;
        refuse_catalog_entries_hang_does_not_read(&descriptor)
            .map_err(|refusal| refusal.to_string())?;

        Ok(Self {
            descriptor,
            how_a_python_node_of_the_type_runs,
        })
    }
}

/// The kind a described node type declares: `process`, or `function` beside
/// its `language`, which a processor interpreter describes only as `python`.
fn read_described_node_kind(
    described_node_type: &serde_json::Value,
) -> std::result::Result<NodeKind, String> {
    match read_string(described_node_type, DESCRIBED_KIND_KEY)?.as_str() {
        "process" => Ok(NodeKind::Process),
        "function" => match read_string(described_node_type, DESCRIBED_LANGUAGE_KEY)?.as_str() {
            "python" => Ok(NodeKind::Function {
                language: FunctionNodeLanguage::Python,
            }),
            other => Err(format!(
                "{DESCRIBED_LANGUAGE_KEY} is {other:?}, but a function node a processor \
                 interpreter describes is `python`"
            )),
        },
        "shader" => Ok(NodeKind::Shader),
        other => Err(format!(
            "{DESCRIBED_KIND_KEY} is {other:?}, which names no node kind: a class is \
             `@node.process` and a function `@node.function`"
        )),
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
             @tatolab.stream.node.process",
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
                 reaching here was built by hand rather than by @tatolab.stream.node.process"
            ));
        }
    };
    Ok(ExecutionConfig::new(execution))
}

fn read_paced_by_real_time(
    described_node_type: &serde_json::Value,
) -> std::result::Result<bool, String> {
    described_field(described_node_type, DESCRIBED_PACED_BY_REAL_TIME_KEY)?
        .as_bool()
        .ok_or_else(|| format!("{DESCRIBED_PACED_BY_REAL_TIME_KEY} must be a bool"))
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

        let port_name = read_string(declaration, "name")?;
        let family = declaration
            .get("family")
            .filter(|declared| !declared.is_null())
            .ok_or_else(|| format!("port {port_name:?} declares no family"))?;
        let family = read_port_family_declaration(family, &port_name, direction)?;
        let mut port = PortDescriptor::new(
            port_name,
            read_string(declaration, "description")?,
            true,
            family,
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
        if let Some(borrow_quota) = declaration
            .get("borrow_quota")
            .filter(|declared| !declared.is_null())
        {
            if matches!(direction, PortDirection::Output) {
                return Err(format!(
                    "output port {:?} declares a borrow_quota — each link's reader holds its \
                     own, so the quota is declared by the consuming input port",
                    port.name
                ));
            }
            let borrow_quota = borrow_quota
                .as_u64()
                .and_then(|quota| usize::try_from(quota).ok())
                .filter(|quota| *quota <= MAX_INPUT_PORT_BORROW_QUOTA)
                .ok_or_else(|| {
                    format!(
                        "port {:?}: borrow_quota must be a whole number from 0 to {}, got {}",
                        port.name, MAX_INPUT_PORT_BORROW_QUOTA, borrow_quota
                    )
                })?;
            port = port.with_borrow_quota(borrow_quota);
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
        if let Some(catalog_entry) = declaration
            .get("catalog_entry")
            .filter(|declared| !declared.is_null())
        {
            let catalog_entry = read_catalog_entry(catalog_entry, &port.name)?;
            port = port.with_catalog_entry(catalog_entry);
        }
        ports.push(port);
    }
    Ok(ports)
}

/// Read one `family` declaration off a described port, refusing a key its
/// family or its direction does not take.
fn read_port_family_declaration(
    family: &serde_json::Value,
    port_name: &str,
    direction: PortDirection,
) -> std::result::Result<PortFamilyDeclaration, String> {
    let declaration = serde_json::from_value::<PortFamilyDeclaration>(family.clone())
        .map_err(|refusal| format!("port {port_name:?}: family {refusal}"))?;
    let declaration_direction = match direction {
        PortDirection::Input => PortFamilyDeclarationDirection::Input,
        PortDirection::Output => PortFamilyDeclarationDirection::Output,
    };
    match declaration.refusal_of_its_keys(port_name, declaration_direction) {
        Some(refusal) => Err(refusal),
        None => Ok(declaration),
    }
}

/// Read one `catalog_entry` declaration off a described port. Whether hang
/// reads it, and whether its port may carry one, is the registration check's.
fn read_catalog_entry(
    catalog_entry: &serde_json::Value,
    port_name: &str,
) -> std::result::Result<CatalogEntry, String> {
    serde_json::from_value::<CatalogEntry>(catalog_entry.clone())
        .map_err(|refusal| format!("port {port_name:?}: catalog entry {refusal}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_formats_spell_every_engine_texture_format() {
        let mut engine_texture_formats: Vec<&str> = (0..=u32::from(u8::MAX))
            .filter_map(crate::core::rhi::TextureFormat::from_repr_value)
            .map(|texture_format| texture_format.wire_name())
            .collect();
        engine_texture_formats.sort_unstable();
        let mut image_formats = crate::core::descriptors::IMAGE_TEXTURE_FORMAT_WIRE_NAMES.to_vec();
        image_formats.sort_unstable();
        assert_eq!(engine_texture_formats, image_formats);
    }

    fn declaration_execution_config(declaration: &PythonProcessorDeclaration) -> ExecutionConfig {
        match declaration.how_a_python_node_of_the_type_runs {
            HowAPythonNodeOfTheTypeRuns::InItsOwnProcessorInterpreter(execution_config) => {
                execution_config
            }
            HowAPythonNodeOfTheTypeRuns::InsideTheReadOfEachNodeItFeeds => {
                panic!("a process node runs in its own processor interpreter")
            }
        }
    }

    fn requested(import_path: &str) -> ProcessorClassImportPath {
        ProcessorClassImportPath::new(import_path).expect("the test path names a class")
    }

    /// A described node type carrying what `@tatolab.stream.node.process` stamps,
    /// with `input_ports` as given.
    fn described_node_type_with_input_ports(input_ports: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "import_path": "my_app.audio:AudioConsumer",
            "short_name": "AudioConsumer",
            "description": "consumes audio",
            "kind": "process",
            "execution": {"mode": "continuous", "interval_ms": 20},
            "scheduling_priority": "high",
            "paced_by_real_time": false,
            "config_schema": {"type": "object"},
            "input_ports": input_ports,
            "output_ports": [{"name": "levels", "family": {"family": "channels"}, "description": "the levels it measured"}],
        })
    }

    fn read(described_node_type: &serde_json::Value) -> Result<PythonProcessorDeclaration, String> {
        PythonProcessorDeclaration::read_from_described_node_type(
            described_node_type,
            &requested("my_app.audio:AudioConsumer"),
        )
    }

    fn refusal_of(described_node_type: &serde_json::Value) -> String {
        match read(described_node_type) {
            Ok(_) => panic!("the described node type was accepted; a refusal was expected"),
            Err(refusal) => refusal,
        }
    }

    /// An audio window marker with `fields`, on a port declared `ordered`.
    fn described_with_audio_window(fields: serde_json::Value) -> serde_json::Value {
        described_node_type_with_input_ports(serde_json::json!([{
            "name": "audio", "family": {"family": "channels"},
            "description": "",
            "delivery_profile": "ordered",
            "audio_window": fields,
        }]))
    }

    fn whole_declared_audio_window() -> serde_json::Map<String, serde_json::Value> {
        serde_json::json!({
            "resolved_from": "declaration",
            "sample_rate": 48_000,
            "channels": 2,
            "dtype": "f32",
            "window_size": 960,
            "hop": 960,
        })
        .as_object()
        .cloned()
        .expect("an object")
    }

    /// A described node type whose one output port carries `catalog_entry`.
    fn described_with_output_catalog_entry(catalog_entry: serde_json::Value) -> serde_json::Value {
        let mut described = described_node_type_with_input_ports(serde_json::json!([]));
        described["output_ports"] = serde_json::json!([{
            "name": "levels", "family": {"family": "channels"},
            "description": "",
            "catalog_entry": catalog_entry,
        }]);
        described
    }

    #[test]
    fn a_described_process_reads_as_a_process_node_and_a_function_as_a_python_function_node() {
        let mut described = described_node_type_with_input_ports(serde_json::json!([]));
        let process = read(&described).expect("reads");
        assert_eq!(process.descriptor.node_kind, NodeKind::Process);
        assert!(matches!(
            process.how_a_python_node_of_the_type_runs,
            HowAPythonNodeOfTheTypeRuns::InItsOwnProcessorInterpreter(_)
        ));

        described["kind"] = serde_json::json!("function");
        described["language"] = serde_json::json!("python");
        let function = read(&described).expect("reads");
        assert_eq!(
            function.descriptor.node_kind,
            NodeKind::Function {
                language: FunctionNodeLanguage::Python
            }
        );
        assert_eq!(
            function.how_a_python_node_of_the_type_runs,
            HowAPythonNodeOfTheTypeRuns::InsideTheReadOfEachNodeItFeeds
        );
    }

    #[test]
    fn a_kind_a_processor_interpreter_never_describes_is_refused_naming_it() {
        let mut described = described_node_type_with_input_ports(serde_json::json!([]));
        described["kind"] = serde_json::json!("function");
        described["language"] = serde_json::json!("rust");
        assert!(refusal_of(&described).contains("\"rust\""));

        described["kind"] = serde_json::json!("shader");
        assert!(refusal_of(&described).contains("Slang"));

        described["kind"] = serde_json::json!("sorcery");
        assert!(refusal_of(&described).contains("\"sorcery\""));

        described.as_object_mut().expect("an object").remove("kind");
        assert!(refusal_of(&described).contains("kind"));
    }

    #[test]
    fn a_node_paced_by_real_time_reads_into_its_descriptor() {
        let mut described = described_node_type_with_input_ports(serde_json::json!([]));
        assert!(
            !read(&described)
                .expect("reads")
                .descriptor
                .paced_by_real_time
        );
        described["paced_by_real_time"] = serde_json::json!(true);
        assert!(
            read(&described)
                .expect("reads")
                .descriptor
                .paced_by_real_time
        );
        described["paced_by_real_time"] = serde_json::json!("yes");
        assert!(read(&described).is_err());
    }

    #[test]
    fn an_output_ports_catalog_entry_reads_into_its_descriptor() {
        let declaration = read(&described_with_output_catalog_entry(serde_json::json!({
            "section": "json",
            "entry": {"mode": "stream"},
        })))
        .unwrap_or_else(|refusal| panic!("an entry hang reads is accepted: {refusal}"));

        assert_eq!(
            declaration.descriptor.outputs[0].catalog_entry,
            Some(CatalogEntry {
                section: crate::core::descriptors::CatalogSection::Json,
                entry: serde_json::json!({"mode": "stream"}),
            })
        );
    }

    #[test]
    fn a_catalog_entry_hang_does_not_read_is_refused_naming_the_port_and_the_field() {
        let refusal = refusal_of(&described_with_output_catalog_entry(serde_json::json!({
            "section": "binary",
            "entry": {"mime": "application/msgpack"},
        })));
        assert!(
            refusal.contains("levels") && refusal.contains("mode"),
            "the refusal must name the port and hang's missing field; got {refusal}"
        );

        let unknown_section = refusal_of(&described_with_output_catalog_entry(serde_json::json!({
            "section": "media",
            "entry": {},
        })));
        assert!(unknown_section.contains("levels"), "got {unknown_section}");
    }

    #[test]
    fn a_ports_family_reads_into_its_descriptor_and_a_key_its_family_does_not_take_is_refused() {
        let declaration = read(&described_node_type_with_input_ports(serde_json::json!([{
            "name": "signals",
            "description": "",
            "delivery_profile": "ordered",
            "family": {"family": "channels", "requires": ["rpm"]},
        }])))
        .unwrap_or_else(|refusal| panic!("a Channels input requiring a name reads: {refusal}"));
        let family = &declaration.descriptor.inputs[0].family;
        assert_eq!(
            family.family,
            crate::core::descriptors::PortFamily::Channels
        );
        assert_eq!(family.requires, vec!["rpm".to_string()]);

        let refusal = refusal_of(&described_node_type_with_input_ports(serde_json::json!([{
            "name": "video",
            "description": "",
            "delivery_profile": "newest",
            "family": {"family": "image", "requires": ["luma"]},
        }])));
        assert!(
            refusal.contains("\"video\"") && refusal.contains("`requires=`"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_port_declaring_no_family_is_refused_naming_the_port() {
        let refusal = refusal_of(&described_node_type_with_input_ports(serde_json::json!([{
            "name": "frames",
            "description": "",
            "delivery_profile": "ordered",
        }])));
        assert!(
            refusal.contains("\"frames\" declares no family"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_declared_borrow_quota_reaches_the_descriptor_and_one_past_the_most_is_refused() {
        let declaration = read(&described_node_type_with_input_ports(serde_json::json!([{
            "name": "frames", "family": {"family": "records"},
            "description": "",
            "delivery_profile": "ordered",
            "borrow_quota": 5,
        }])))
        .unwrap_or_else(|refusal| panic!("a quota within the most reads: {refusal}"));
        assert_eq!(declaration.descriptor.inputs[0].borrow_quota, Some(5));

        let refusal = refusal_of(&described_node_type_with_input_ports(serde_json::json!([{
            "name": "frames", "family": {"family": "records"},
            "description": "",
            "delivery_profile": "ordered",
            "borrow_quota": MAX_INPUT_PORT_BORROW_QUOTA + 1,
        }])));
        assert!(
            refusal.contains("\"frames\"") && refusal.contains("borrow_quota"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_catalog_entry_on_an_input_port_is_refused_naming_the_port() {
        let refusal = refusal_of(&described_node_type_with_input_ports(serde_json::json!([{
            "name": "audio", "family": {"family": "channels"},
            "description": "",
            "delivery_profile": "ordered",
            "catalog_entry": {"section": "json", "entry": {"mode": "stream"}},
        }])));
        assert!(
            refusal.contains("input port `audio`") && refusal.contains("only an output port"),
            "got {refusal}"
        );
    }

    #[test]
    fn a_described_node_type_reads_into_a_python_descriptor_and_its_execution() {
        let declaration = read(&described_node_type_with_input_ports(serde_json::json!([
            {"name": "audio", "family": {"family": "channels"}, "description": "what it hears", "delivery_profile": "ordered"}
        ])))
        .unwrap_or_else(|refusal| panic!("the described node type reads: {refusal}"));

        let descriptor = &declaration.descriptor;
        assert_eq!(
            descriptor.processor_class_import_path.as_str(),
            "my_app.audio:AudioConsumer"
        );
        assert_eq!(
            descriptor.entrypoint.as_deref(),
            Some("my_app.audio:AudioConsumer")
        );
        assert_eq!(
            descriptor.processor_class_short_name.as_str(),
            "AudioConsumer"
        );
        assert_eq!(descriptor.runtime, ProcessorRuntime::Python);
        assert_eq!(descriptor.description, "consumes audio");
        assert_eq!(descriptor.scheduling.priority, ThreadPriority::High);
        assert_eq!(
            descriptor.config_schema,
            Some(serde_json::json!({"type": "object"}))
        );
        assert_eq!(descriptor.inputs[0].name, "audio");
        assert_eq!(
            descriptor.inputs[0].delivery_profile.as_deref(),
            Some("ordered")
        );
        assert_eq!(descriptor.outputs[0].name, "levels");
        assert_eq!(
            declaration.how_a_python_node_of_the_type_runs,
            HowAPythonNodeOfTheTypeRuns::InItsOwnProcessorInterpreter(ExecutionConfig {
                execution: ProcessExecution::Continuous { interval_ms: 20 },
                ..declaration_execution_config(&declaration)
            })
        );
    }

    /// The interpreter answers for the path it was asked; a document naming
    /// another class is refused rather than registered under the wrong name.
    #[test]
    fn a_described_node_type_naming_another_import_path_is_refused_naming_both() {
        let mut described = described_node_type_with_input_ports(serde_json::json!([]));
        described["import_path"] = serde_json::json!("my_app.audio:SomethingElse");

        let refusal = refusal_of(&described);

        assert!(
            refusal.contains("my_app.audio:SomethingElse")
                && refusal.contains("my_app.audio:AudioConsumer"),
            "{refusal}"
        );
    }

    #[test]
    fn an_unknown_execution_mode_is_refused_naming_it() {
        let mut described = described_node_type_with_input_ports(serde_json::json!([]));
        described["execution"] = serde_json::json!({"mode": "eventually"});

        assert!(refusal_of(&described).contains("\"eventually\""));
    }

    #[test]
    fn a_config_schema_that_is_not_an_object_is_refused() {
        let mut described = described_node_type_with_input_ports(serde_json::json!([]));
        described["config_schema"] = serde_json::json!(["not", "an", "object"]);

        assert!(refusal_of(&described).contains("config_schema must be a JSON object"));
    }

    /// The decorator refuses the sentinel; this reader does not, and must not.
    /// A marker the decorator never built still carries `match_device` through
    /// to the compiler, where the wire-time refusal — which knows the port's
    /// placement, as nothing here does — is the guard that speaks.
    #[test]
    fn a_hand_built_match_device_marker_still_reaches_the_descriptor() {
        let declaration = read(&described_with_audio_window(
            serde_json::json!({"resolved_from": "match_device"}),
        ))
        .unwrap_or_else(|refusal| panic!("a hand-built sentinel reads: {refusal}"));

        assert_eq!(
            declaration.descriptor.inputs[0].audio_window,
            Some(AudioWindowContract::MatchDevice {})
        );
    }

    #[test]
    fn a_hand_built_marker_smuggling_an_unhonourable_contract_is_refused_naming_both_numbers() {
        let mut fields = whole_declared_audio_window();
        fields.insert("window_size".to_string(), serde_json::json!(512));
        fields.insert("hop".to_string(), serde_json::json!(4096));

        let refusal = refusal_of(&described_with_audio_window(fields.into()));

        assert!(
            refusal.contains("4096") && refusal.contains("512"),
            "{refusal}"
        );
    }

    #[test]
    fn a_contract_beside_a_skipping_profile_is_refused_naming_both_knobs() {
        let described = described_node_type_with_input_ports(serde_json::json!([{
            "name": "audio", "family": {"family": "channels"},
            "description": "",
            "delivery_profile": "newest",
            "audio_window": serde_json::Value::Object(whole_declared_audio_window()),
        }]));

        let refusal = refusal_of(&described);

        assert!(
            refusal.contains("audio_window")
                && refusal.contains("newest")
                && refusal.contains("ordered"),
            "{refusal}"
        );
    }

    #[test]
    fn a_negative_count_is_refused_naming_the_field_and_the_value() {
        let mut fields = whole_declared_audio_window();
        fields.insert("sample_rate".to_string(), serde_json::json!(-1));

        let refusal = refusal_of(&described_with_audio_window(fields.into()));

        assert!(
            refusal.contains("sample_rate") && refusal.contains("-1"),
            "{refusal}"
        );
    }

    /// A Python `bool` is an `int` subclass, so a marker carrying `True` would
    /// otherwise reach the stage as one channel — a plausible count nobody
    /// wrote.
    #[test]
    fn a_bool_where_a_number_belongs_is_refused_naming_the_field_and_the_kind() {
        for field in ["channels", "sample_rate", "window_size", "hop"] {
            let mut fields = whole_declared_audio_window();
            fields.insert(field.to_string(), serde_json::json!(true));

            let refusal = refusal_of(&described_with_audio_window(fields.into()));

            assert!(
                refusal.contains(field) && refusal.contains("bool"),
                "a bool in {field:?} must be refused naming the field and the kind; got {refusal}"
            );
        }
    }

    /// The count is the one value a marker may leave out, and the reader
    /// carries the omission through: a port that follows its source is
    /// spelled by saying nothing, or by the `"source"` word.
    #[test]
    fn an_omitted_or_source_spelled_channel_count_follows_the_source() {
        let mut omitted = whole_declared_audio_window();
        omitted.remove("channels");
        let mut spelled = whole_declared_audio_window();
        spelled.insert("channels".to_string(), serde_json::json!("source"));

        for fields in [omitted, spelled] {
            let declaration =
                read(&described_with_audio_window(fields.into())).unwrap_or_else(|refusal| {
                    panic!("an omitted count is a whole contract: {refusal}")
                });

            assert_eq!(
                declaration.descriptor.inputs[0].audio_window,
                Some(AudioWindowContract::Declaration(
                    AudioWindowContractDeclaredValues {
                        sample_rate: 48_000,
                        channels: None,
                        dtype: "f32".to_string(),
                        window_size: 960,
                        hop: 960,
                    }
                ))
            );
        }
    }

    #[test]
    fn a_channel_count_naming_no_count_is_refused_offering_the_spelling() {
        let mut fields = whole_declared_audio_window();
        fields.insert("channels".to_string(), serde_json::json!("stereo"));

        let refusal = refusal_of(&described_with_audio_window(fields.into()));

        assert!(
            refusal.contains("channels") && refusal.contains("source"),
            "{refusal}"
        );
    }

    /// Every field the contract requires names the port and the field when it
    /// is missing; `channels` is the one a port may leave to its source.
    #[test]
    fn a_marker_missing_any_required_contract_field_is_refused_naming_the_port_and_the_field() {
        for missing_field in [
            "resolved_from",
            "sample_rate",
            "dtype",
            "window_size",
            "hop",
        ] {
            let mut fields = whole_declared_audio_window();
            fields.remove(missing_field);

            let refusal = refusal_of(&described_with_audio_window(fields.into()));

            assert!(
                refusal.contains("input port \"audio\"") && refusal.contains(missing_field),
                "a missing {missing_field:?} must name the port and the field; got {refusal}"
            );
        }
    }

    /// An output port declares no contract — only a consuming input port
    /// states the window it needs.
    #[test]
    fn an_output_port_declaring_a_contract_is_refused() {
        let mut described = described_node_type_with_input_ports(serde_json::json!([]));
        described["output_ports"] = serde_json::json!([{
            "name": "windows", "family": {"family": "channels"},
            "description": "",
            "audio_window": {"resolved_from": "match_device"},
        }]);

        let refusal = refusal_of(&described);

        assert!(
            refusal.contains("output port \"windows\"") && refusal.contains("consuming"),
            "{refusal}"
        );
    }

    /// The kind of a channel-count refusal travels with it, so a language host
    /// raises the same mistake as the same exception on every field.
    #[test]
    fn a_channel_count_refusal_says_whether_the_value_was_the_wrong_kind_or_unusable() {
        assert_eq!(
            read_a_channel_count_or_the_source_spelling(&serde_json::json!(2)),
            Ok(Some(2))
        );
        assert!(matches!(
            read_a_channel_count_or_the_source_spelling(&serde_json::json!(1.5)),
            Err(AudioWindowFieldRefusal::WrongKindOfValue(_))
        ));
        assert!(matches!(
            read_a_channel_count_or_the_source_spelling(&serde_json::json!(-2)),
            Err(AudioWindowFieldRefusal::UnusableValue(_))
        ));
        assert!(matches!(
            read_a_channel_count_or_the_source_spelling(&serde_json::json!("stereo")),
            Err(AudioWindowFieldRefusal::UnusableValue(_))
        ));
    }
}
