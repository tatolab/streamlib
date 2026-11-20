// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What a `GraphInput` and a `GraphOutput` declare: each of their settings as an input, and their
//! one `value` port, whose type the `pin_type` setting chooses.

use crate::core::descriptors::{
    DeclaredPinType, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeInputMode,
    NodeValueLifetime, PinAnnotations, PinDeclaration, PinFlow, PinType, PortDescriptor,
    PortFamily, PortFamilyDeclaration, SettingReadTime,
};
use crate::core::error::{Error, Result};
use crate::core::graph::graph_interface_resolution::{
    GRAPH_INPUT_LIFETIME_CONFIG_KEY, GRAPH_INPUT_NODE_TYPE, GRAPH_INPUT_RANGE_CONFIG_KEY,
    GRAPH_INPUT_VALUE_FORMER_NAME, GRAPH_INTERFACE_FAMILY_CONFIG_KEY,
    GRAPH_INTERFACE_PIN_TYPE_SETTING, GRAPH_INTERFACE_PORT_CONFIG_KEY,
    GRAPH_INTERFACE_VALUE_PORT_NAME, GRAPH_OUTPUT_NODE_TYPE,
};

/// The family the `value` ports of a `GraphInput` declare, which they carry while it chooses no
/// pin type and names no family.
pub(crate) const THE_FAMILY_A_GRAPH_INPUT_DECLARES: PortFamily = PortFamily::Records;

/// The descriptor of the `GraphInput` node type: its settings `port`, `pin_type`, `family`,
/// `range` and `lifetime`, each read at setup; its `value` input, a setting read each run that
/// holds what a client sets, whose former name is `default`; and its `value` output, carrying
/// that value as a value, which keeps its last message for a reader that connects later. Both
/// `value` ports are of the type `pin_type` chooses, and of the family `family` names while it
/// chooses none.
pub(crate) fn the_node_type_of_a_graph_input() -> Result<NodeDescriptor> {
    let mut descriptor = interface_node_type(
        "GraphInput",
        GRAPH_INPUT_NODE_TYPE,
        "One Input of a stream: every port `value` is linked to reads what enters it.",
    )?;
    descriptor.inputs = vec![
        port_setting("The Input's name on the stream."),
        pin_type_setting("The type of what enters the Input; unset, `family` decides it."),
        family_setting(),
        setting(
            GRAPH_INPUT_RANGE_CONFIG_KEY,
            "`[lowest, highest]`, the span a control moves the Input's number in.",
            PinType::ListOf {
                element_type: Box::new(PinType::Float { exact_size: None }),
            },
            None,
            SettingReadTime::AtSetup,
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
            Some(serde_json::Value::String(
                NodeValueLifetime::Kept.wire_name().to_string(),
            )),
            SettingReadTime::AtSetup,
        ),
        value_port(
            "What a client sets on the Input; it takes no wire.",
            vec![GRAPH_INPUT_VALUE_FORMER_NAME.to_string()],
        )
        .with_input_mode(NodeInputMode::SettingOnly {
            read: SettingReadTime::EachRun,
        }),
    ];
    descriptor.outputs = vec![value_port("What enters the Input.", Vec::new())];
    Ok(descriptor)
}

/// The descriptor of the `GraphOutput` node type: its settings `port`, `pin_type` and `family`,
/// each read at setup, and its `value` input, the port linked into it, of the type `pin_type`
/// chooses, and of the family `family` names while it chooses none.
pub(crate) fn the_node_type_of_a_graph_output() -> Result<NodeDescriptor> {
    let mut descriptor = interface_node_type(
        "GraphOutput",
        GRAPH_OUTPUT_NODE_TYPE,
        "One Output of a stream: the port linked into `value`.",
    )?;
    descriptor.inputs = vec![
        port_setting("The Output's name on the stream."),
        pin_type_setting("The type of what leaves the Output; unset, `family` decides it."),
        family_setting(),
        value_port("The port the Output offers.", Vec::new()),
    ];
    Ok(descriptor)
}

/// The descriptor of the interface node type `node_type`, of short name `short_name`, stating
/// `description` and no ports yet.
fn interface_node_type(
    short_name: &str,
    node_type: &str,
    description: &str,
) -> Result<NodeDescriptor> {
    Ok(NodeDescriptor::new(
        NodeClassShortName::new(short_name.to_string())
            .map_err(|refusal| Error::Configuration(refusal.to_string()))?,
        NodeClassImportPath::new(node_type.to_string())
            .map_err(|refusal| Error::Configuration(refusal.to_string()))?,
        description.to_string(),
    ))
}

/// A setting named `name` of `pin_type`, stating `description`, read at `read`, reading
/// `pin_default` while it holds no value; one with no default may be unset.
fn setting(
    name: &str,
    description: &str,
    pin_type: PinType,
    pin_default: Option<serde_json::Value>,
    read: SettingReadTime,
) -> PortDescriptor {
    let family = PortFamily::carrying(&pin_type).unwrap_or(PortFamily::Records);
    let may_be_unset = pin_default.is_none();
    let port = PortDescriptor::new(
        name,
        description,
        false,
        PortFamilyDeclaration::of_family(family),
    )
    .with_pin_declaration(PinDeclaration {
        may_be_unset,
        ..PinDeclaration::exactly(pin_type)
    })
    .with_input_mode(NodeInputMode::SettingOnly { read });
    match pin_default {
        Some(pin_default) => port.with_pin_default(pin_default),
        None => port,
    }
}

/// The `port` setting: the name the interface node declares, a String every node holds.
fn port_setting(description: &str) -> PortDescriptor {
    let mut port = setting(
        GRAPH_INTERFACE_PORT_CONFIG_KEY,
        description,
        PinType::String {},
        None,
        SettingReadTime::AtSetup,
    );
    if let Some(declaration) = port.pin_declaration.as_mut() {
        declaration.may_be_unset = false;
    }
    port
}

/// The `pin_type` setting: a pin type choice that may choose any type, unset until one is chosen.
fn pin_type_setting(description: &str) -> PortDescriptor {
    setting(
        GRAPH_INTERFACE_PIN_TYPE_SETTING,
        description,
        PinType::PinTypeChoice { among: Vec::new() },
        None,
        SettingReadTime::AtSetup,
    )
}

/// The `family` setting: one family's name, which types the `value` port as a family
/// declaration does while no pin type is chosen.
fn family_setting() -> PortDescriptor {
    setting(
        GRAPH_INTERFACE_FAMILY_CONFIG_KEY,
        "The family of the port while no pin type is chosen; unset, the family of the first \
         port linked to it.",
        PinType::Menu {
            option_names: PortFamily::ALL
                .map(|family| family.wire_name().to_string())
                .to_vec(),
        },
        None,
        SettingReadTime::AtSetup,
    )
}

/// A `value` port stating `description` and `former_names`, carried as a value of the type the
/// `pin_type` setting chooses, of the family a `GraphInput` declares until it is resolved.
fn value_port(description: &str, former_names: Vec<String>) -> PortDescriptor {
    let mut family = PortFamilyDeclaration::of_family(THE_FAMILY_A_GRAPH_INPUT_DECLARES);
    family.family_from_config_key = Some(GRAPH_INTERFACE_FAMILY_CONFIG_KEY.to_string());
    PortDescriptor::new(GRAPH_INTERFACE_VALUE_PORT_NAME, description, false, family)
        .with_pin_declaration(PinDeclaration {
            pin_type: DeclaredPinType::SetByInput {
                input_name: GRAPH_INTERFACE_PIN_TYPE_SETTING.to_string(),
            },
            flow: PinFlow::Value,
            annotations: PinAnnotations::default(),
            former_names,
            may_be_unset: false,
        })
}
