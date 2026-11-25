// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a `GraphInput` and a `GraphOutput` declare: each of their settings as an input, and their
//! one `value` port, whose type the `pin_type` setting chooses.

use std::sync::LazyLock;

use crate::core::descriptors::{
    DeclaredPinType, NodeDescriptor, NodeInputMode, NodeValueLifetime, PinAnnotations,
    PinDeclaration, PinFlow, PinType, PortDescriptor, SettingReadTime,
};
use crate::core::error::Result;
use crate::core::graph::graph_interface_resolution::{
    GRAPH_INPUT_CONTROL_SETTING, GRAPH_INPUT_LIFETIME_CONFIG_KEY, GRAPH_INPUT_MEANING_SETTING,
    GRAPH_INPUT_NODE_TYPE, GRAPH_INPUT_RANGE_CONFIG_KEY, GRAPH_INPUT_STEP_SETTING,
    GRAPH_INPUT_UNIT_SETTING, GRAPH_INPUT_VALUE_FORMER_NAME, GRAPH_INTERFACE_PIN_TYPE_SETTING,
    GRAPH_INTERFACE_PORT_CONFIG_KEY, GRAPH_INTERFACE_VALUE_PORT_NAME, GRAPH_OUTPUT_NODE_TYPE,
};

use super::node_types_of_the_graph_itself::node_type_of_the_graph_itself;

/// The settings of the `GraphInput` node type, built once.
static THE_SETTINGS_OF_A_GRAPH_INPUT: LazyLock<Vec<PortDescriptor>> =
    LazyLock::new(|| the_settings_of(the_node_type_of_a_graph_input()));

/// The settings of the `GraphOutput` node type, built once.
static THE_SETTINGS_OF_A_GRAPH_OUTPUT: LazyLock<Vec<PortDescriptor>> =
    LazyLock::new(|| the_settings_of(the_node_type_of_a_graph_output()));

/// The settings a `GraphInput` takes, as its type declares them: `port`, `pin_type`, the
/// annotations of its `value` (`range`, `step`, `unit`, `meaning` and `control`) and
/// `lifetime`.
pub(crate) fn the_settings_of_a_graph_input() -> &'static [PortDescriptor] {
    &THE_SETTINGS_OF_A_GRAPH_INPUT
}

/// The settings a `GraphOutput` takes, as its type declares them: `port` and `pin_type`.
pub(crate) fn the_settings_of_a_graph_output() -> &'static [PortDescriptor] {
    &THE_SETTINGS_OF_A_GRAPH_OUTPUT
}

/// The inputs of the interface node type `descriptor` other than its `value`: the settings it
/// reads at setup.
fn the_settings_of(descriptor: Result<NodeDescriptor>) -> Vec<PortDescriptor> {
    descriptor
        .expect("an interface node type's names are valid")
        .inputs
        .into_iter()
        .filter(|input| input.name != GRAPH_INTERFACE_VALUE_PORT_NAME)
        .collect()
}

/// The descriptor of the `GraphInput` node type: its settings `port`, `pin_type` and
/// `lifetime`, each read at setup, and the annotations of its `value` (`range`, `step`, `unit`,
/// `meaning` and `control`), each read each run; its `value` input, a setting read each run that
/// holds what a client sets, whose former name is `default`; and its `value` output, carrying
/// that value as a value, which keeps its last message for a reader that connects later. Both
/// `value` ports are of the type `pin_type` chooses, else of the first port linked to it.
pub(crate) fn the_node_type_of_a_graph_input() -> Result<NodeDescriptor> {
    let mut descriptor = node_type_of_the_graph_itself(
        "GraphInput",
        GRAPH_INPUT_NODE_TYPE,
        "One Input of a stream: every port `value` is linked to reads what enters it.",
    )?;
    descriptor.inputs = vec![
        port_setting("The Input's name on the stream."),
        pin_type_setting(
            "The type of what enters the Input; unset, the type of the first port linked to it.",
        ),
        annotation_setting(
            GRAPH_INPUT_RANGE_CONFIG_KEY,
            "`[lowest, highest]`, the span a control moves the Input's number in.",
            PinType::ListOf {
                element_type: Box::new(PinType::Float { exact_size: None }),
            },
        ),
        annotation_setting(
            GRAPH_INPUT_STEP_SETTING,
            "The step a control moves the Input's number by.",
            PinType::Float { exact_size: None },
        ),
        annotation_setting(
            GRAPH_INPUT_UNIT_SETTING,
            "The unit the Input's number is in, such as `Hz`.",
            PinType::String {},
        ),
        annotation_setting(
            GRAPH_INPUT_MEANING_SETTING,
            "What the Input's value stands for, such as `frequency`.",
            PinType::String {},
        ),
        annotation_setting(
            GRAPH_INPUT_CONTROL_SETTING,
            "The control that sets the Input's value, such as `dial`: one of the Studio's, or a \
             module's qualified name.",
            PinType::String {},
        ),
        setting(
            GRAPH_INPUT_LIFETIME_CONFIG_KEY,
            "How long a value a client sets lasts: kept through Stop and a restart, or reset at \
             Stop.",
            PinType::Menu {
                option_names: NodeValueLifetime::EVERY_LIFETIME
                    .map(|lifetime| lifetime.wire_name().to_string())
                    .to_vec(),
            },
            false,
            Some(serde_json::Value::String(
                NodeValueLifetime::Kept.wire_name().to_string(),
            )),
        ),
        value_port(
            "What a client sets on the Input; it takes no wire.",
            vec![GRAPH_INPUT_VALUE_FORMER_NAME.to_string()],
            true,
        )
        .with_input_mode(NodeInputMode::SettingOnly {
            read: SettingReadTime::EachRun,
        }),
    ];
    descriptor.outputs = vec![value_port("What enters the Input.", Vec::new(), false)];
    Ok(descriptor)
}

/// The descriptor of the `GraphOutput` node type: its settings `port` and `pin_type`, each read
/// at setup, and its `value` input, the port linked into it, of the type `pin_type` chooses,
/// else of that port.
pub(crate) fn the_node_type_of_a_graph_output() -> Result<NodeDescriptor> {
    let mut descriptor = node_type_of_the_graph_itself(
        "GraphOutput",
        GRAPH_OUTPUT_NODE_TYPE,
        "One Output of a stream: the port linked into `value`.",
    )?;
    descriptor.inputs = vec![
        port_setting("The Output's name on the stream."),
        pin_type_setting(
            "The type of what leaves the Output; unset, the type of the port linked into it.",
        ),
        value_port("The port the Output offers.", Vec::new(), false),
    ];
    Ok(descriptor)
}

/// A setting of a node type the runtime owns, named `name` of `pin_type`, stating
/// `description`, read at setup, reading `pin_default` while it holds no value, and unset only
/// when `may_be_unset` says so.
pub(super) fn setting(
    name: &str,
    description: &str,
    pin_type: PinType,
    may_be_unset: bool,
    pin_default: Option<serde_json::Value>,
) -> PortDescriptor {
    let port = PortDescriptor::new(
        name,
        description,
        false,
        PinDeclaration {
            may_be_unset,
            ..PinDeclaration::exactly(pin_type)
        },
    )
    .with_input_mode(NodeInputMode::SettingOnly {
        read: SettingReadTime::AtSetup,
    });
    match pin_default {
        Some(pin_default) => port.with_pin_default(pin_default),
        None => port,
    }
}

/// A setting of a `GraphInput` named `name` of `pin_type`, stating `description`, that holds
/// one annotation of its `value` and may be unset. The running node never reads it: it changes
/// what `graph` serves on `value` and how a value set there is checked, so it is read each run
/// and a change rebuilds nothing.
fn annotation_setting(name: &str, description: &str, pin_type: PinType) -> PortDescriptor {
    setting(name, description, pin_type, true, None).with_input_mode(NodeInputMode::SettingOnly {
        read: SettingReadTime::EachRun,
    })
}

/// The `port` setting: the name the interface node declares, a String every node holds.
fn port_setting(description: &str) -> PortDescriptor {
    setting(
        GRAPH_INTERFACE_PORT_CONFIG_KEY,
        description,
        PinType::String {},
        false,
        None,
    )
}

/// The `pin_type` setting: a pin type choice that may choose any type, unset until one is chosen.
fn pin_type_setting(description: &str) -> PortDescriptor {
    setting(
        GRAPH_INTERFACE_PIN_TYPE_SETTING,
        description,
        PinType::a_choice_of_any(),
        true,
        None,
    )
}

/// A `value` port stating `description` and `former_names`, carried as a value of the type the
/// `pin_type` setting chooses; one a client may clear when `may_be_unset`.
pub(super) fn value_port(
    description: &str,
    former_names: Vec<String>,
    may_be_unset: bool,
) -> PortDescriptor {
    PortDescriptor::new(
        GRAPH_INTERFACE_VALUE_PORT_NAME,
        description,
        false,
        PinDeclaration {
            pin_type: DeclaredPinType::SetByInput {
                input_name: GRAPH_INTERFACE_PIN_TYPE_SETTING.to_string(),
            },
            flow: PinFlow::Value,
            annotations: PinAnnotations::default(),
            former_names,
            may_be_unset,
        },
    )
}
