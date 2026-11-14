// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading one described node type — the `@node.process` stamps a processor
//! interpreter printed for a class — into what the engine registers.
//!
//! The keys of a described node type are the `__tatolab_node_*__` attributes
//! `tatolab/stream/_node_declaration.py` stamps, carried verbatim by the
//! bootstrap's `--describe`; the two move together.

use super::shader_node_program::ShaderNodeProgram;
use crate::core::descriptors::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AudioWindowContract,
    AudioWindowContractDeclaredValues, FunctionNodeLanguage, NodeKind, PortDescriptor,
    PortFamilyDeclaration, PortFamilyDeclarationDirection, ProcessorClassImportPath,
    ProcessorClassShortName, ProcessorDescriptor, ProcessorRuntime, ProcessorScheduling,
    refuse_audio_window_beside_a_skipping_delivery_profile,
};
use crate::core::execution::{ExecutionConfig, ProcessExecution, ThreadPriority};
use crate::iceoryx2::MAX_INPUT_PORT_BORROW_QUOTA;
use streamlib_ipc_types::STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS;

const DESCRIBED_IMPORT_PATH_KEY: &str = "import_path";
const DESCRIBED_SHORT_NAME_KEY: &str = "short_name";
const DESCRIBED_DESCRIPTION_KEY: &str = "description";
const DESCRIBED_CATEGORY_KEY: &str = "category";
const DESCRIBED_KIND_KEY: &str = "kind";
const DESCRIBED_LANGUAGE_KEY: &str = "language";
const DESCRIBED_RUNS_AS_PYTHON_KEY: &str = "runs_as_python";
const DESCRIBED_EXECUTION_KEY: &str = "execution";
const DESCRIBED_SCHEDULING_PRIORITY_KEY: &str = "scheduling_priority";
const DESCRIBED_PACED_BY_REAL_TIME_KEY: &str = "paced_by_real_time";
const DESCRIBED_CONFIG_SCHEMA_KEY: &str = "config_schema";
const DESCRIBED_INPUT_PORTS_KEY: &str = "input_ports";
const DESCRIBED_OUTPUT_PORTS_KEY: &str = "output_ports";
const DESCRIBED_STATE_FIELDS_KEY: &str = "state_fields";
const DESCRIBED_SHADER_SOURCE_KEY: &str = "shader_source";
const DESCRIBED_SHADER_SIGNATURE_KEY: &str = "shader_signature";

/// Everything the engine needs to register one Python node type and run its
/// nodes.
pub struct PythonProcessorDeclaration {
    /// The type's descriptor, registered under its import path.
    pub descriptor: ProcessorDescriptor,
    /// How a node of the type runs, by its kind.
    pub how_a_python_node_of_the_type_runs: HowAPythonNodeOfTheTypeRuns,
}

/// How a node of a described Python type runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HowAPythonNodeOfTheTypeRuns {
    /// A `@node.process` class, in its own processor interpreter, driven on
    /// the execution it declared.
    InItsOwnProcessorInterpreter(ExecutionConfig),
    /// A `@node.function`, inside the read of each node it feeds.
    InsideTheReadOfEachNodeItFeeds,
    /// A `@node.shader`, its Slang function compiled and run on the GPU in
    /// this process, with no Python process.
    OnItsNativeShaderEvaluator(ShaderNodeProgram),
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
        if let Some(category) = read_category(described_node_type)? {
            descriptor = descriptor.with_category(category);
        }
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
                    descriptor.function_node_state_fields =
                        read_function_node_state_fields(described_node_type)?;
                    descriptor.function_node_runs_as_python =
                        read_whether_a_function_node_runs_as_python(
                            described_node_type,
                            function_kind,
                        )?;
                    HowAPythonNodeOfTheTypeRuns::InsideTheReadOfEachNodeItFeeds
                }
                NodeKind::Shader => {
                    descriptor.node_kind = NodeKind::Shader;
                    HowAPythonNodeOfTheTypeRuns::OnItsNativeShaderEvaluator(
                        ShaderNodeProgram::read_from_described_shader(
                            described_field(described_node_type, DESCRIBED_SHADER_SOURCE_KEY)?,
                            described_field(described_node_type, DESCRIBED_SHADER_SIGNATURE_KEY)?,
                        )?,
                    )
                }
            };

        descriptor.inputs = read_port_descriptors(described_node_type, PortDirection::Input)?;
        descriptor.outputs = read_port_descriptors(described_node_type, PortDirection::Output)?;
        for output in &descriptor.outputs {
            if let Some(followed_input_port) = output.family.family_follows_input_port.as_deref()
                && !descriptor
                    .inputs
                    .iter()
                    .any(|input| input.name == followed_input_port)
            {
                return Err(format!(
                    "output port {:?} follows the family of input port {followed_input_port:?}, \
                     which the node does not declare",
                    output.name
                ));
            }
        }

        Ok(Self {
            descriptor,
            how_a_python_node_of_the_type_runs,
        })
    }
}

/// The palette category a described node type declares, refused when it is
/// not a non-blank string; none when it declares none.
fn read_category(
    described_node_type: &serde_json::Value,
) -> std::result::Result<Option<String>, String> {
    match described_node_type.get(DESCRIBED_CATEGORY_KEY) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(category)) if !category.trim().is_empty() => {
            Ok(Some(category.clone()))
        }
        Some(other) => Err(format!(
            "{DESCRIBED_CATEGORY_KEY} must be a non-blank string, got {}",
            json_kind_name(other)
        )),
    }
}

/// Whether a described Python function node's body does not compile, as the
/// describe found by compiling it; a Rust function node is native code.
fn read_whether_a_function_node_runs_as_python(
    described_node_type: &serde_json::Value,
    function_kind: NodeKind,
) -> std::result::Result<bool, String> {
    if function_kind
        != (NodeKind::Function {
            language: FunctionNodeLanguage::Python,
        })
    {
        return Ok(false);
    }
    described_field(described_node_type, DESCRIBED_RUNS_AS_PYTHON_KEY)?
        .as_bool()
        .ok_or_else(|| format!("{DESCRIBED_RUNS_AS_PYTHON_KEY} must be a bool"))
}

/// The fields of a described function node's state record, refused past what
/// one function node's state entry holds.
fn read_function_node_state_fields(
    described_node_type: &serde_json::Value,
) -> std::result::Result<Vec<String>, String> {
    let state_fields: Vec<String> = described_node_type
        .get(DESCRIBED_STATE_FIELDS_KEY)
        .cloned()
        .ok_or_else(|| format!("{DESCRIBED_STATE_FIELDS_KEY} is missing"))
        .and_then(|state_fields| {
            serde_json::from_value(state_fields).map_err(|not_a_list_of_names| {
                format!(
                    "{DESCRIBED_STATE_FIELDS_KEY} is not a list of names: {not_a_list_of_names}"
                )
            })
        })?;
    if state_fields.len() > STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS {
        return Err(format!(
            "its state record has {} fields, and a function node's state carries at most \
             {STATE_VALUES_ONE_FUNCTION_NODE_STATE_BOARD_ENTRY_HOLDS} numbers; keep fewer fields \
             in `State[...]`",
            state_fields.len()
        ));
    }
    Ok(state_fields)
}

/// The kind a described node type declares: `process`, or `function` beside
/// its `language`: `python` for a `@node.function`, `rust` for a module's
/// `#[function_node]`.
fn read_described_node_kind(
    described_node_type: &serde_json::Value,
) -> std::result::Result<NodeKind, String> {
    match read_string(described_node_type, DESCRIBED_KIND_KEY)?.as_str() {
        "process" => Ok(NodeKind::Process),
        "function" => match read_string(described_node_type, DESCRIBED_LANGUAGE_KEY)?.as_str() {
            "python" => Ok(NodeKind::Function {
                language: FunctionNodeLanguage::Python,
            }),
            "rust" => Ok(NodeKind::Function {
                language: FunctionNodeLanguage::Rust,
            }),
            other => Err(format!(
                "{DESCRIBED_LANGUAGE_KEY} is {other:?}, but a function node a processor \
                 interpreter describes is `python` or `rust`"
            )),
        },
        "shader" => Ok(NodeKind::Shader),
        other => Err(format!(
            "{DESCRIBED_KIND_KEY} is {other:?}, which names no node kind: a class is \
             `@node.process`, a function `@node.function` and a Slang function `@node.shader`"
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
        if let Some(pin_default) = declaration.get("pin_default") {
            let pin_default = read_pin_default(pin_default, &port, direction)?;
            port = port.with_pin_default(pin_default);
        }
        ports.push(port);
    }
    Ok(ports)
}

/// Read the value an input port reads while nothing is linked into it: a
/// number or a bool on a Channels input, or `null` on an optional function
/// node input of any family.
fn read_pin_default(
    pin_default: &serde_json::Value,
    port: &PortDescriptor,
    direction: PortDirection,
) -> std::result::Result<serde_json::Value, String> {
    if matches!(direction, PortDirection::Output) {
        return Err(format!(
            "output port {:?} declares a pin default, and only an input reads one",
            port.name
        ));
    }
    if !pin_default.is_null()
        && port.family.family != crate::core::descriptors::PortFamily::Channels
    {
        return Err(format!(
            "input port {:?} of family {} declares a pin default, and only a Channels input \
             carries a control value",
            port.name, port.family.family
        ));
    }
    match pin_default {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            Ok(pin_default.clone())
        }
        other => Err(format!(
            "input port {:?}: pin_default must be a number or a bool, got {}",
            port.name,
            json_kind_name(other)
        )),
    }
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
mod tests;
