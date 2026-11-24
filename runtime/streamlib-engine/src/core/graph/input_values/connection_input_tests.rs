// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Map, Value, json};

use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    NamedPinTypeImportPath, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeInputMode,
    PinDeclaration, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
};
use crate::core::graph::input_values::the_unlinked_input_values;
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};

const SPOKE: &str = "ConnectionInputTestsSpoke";

fn the_spoke_type() -> NodeClassImportPath {
    NodeClassImportPath::new(format!("{}::{SPOKE}", module_path!())).unwrap()
}

/// A spoke taking a hub on its field `hub`, as a class node's connection field is declared.
fn the_spoke_type_registered() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        let mut descriptor = NodeDescriptor::new(
            NodeClassShortName::new(SPOKE).unwrap(),
            the_spoke_type(),
            "a fixture",
        );
        descriptor.inputs = vec![
            PortDescriptor::new(
                "hub",
                "hub",
                false,
                PortFamilyDeclaration::of_family(PortFamily::Records),
            )
            .with_pin_declaration(PinDeclaration::exactly(PinType::Connection {
                connection_type: NamedPinTypeImportPath::new(
                    "connection_input_tests:HubConnection",
                )
                .unwrap(),
                one_referrer_per_referenced_node: false,
            }))
            .with_input_mode(NodeInputMode::WireOrValue),
        ];
        NODE_REGISTRY
            .register_descriptor_only(descriptor)
            .expect("register the fixture descriptor");
    });
}

fn the_values_handed_a_spoke_holding(pins: Map<String, Value>) -> Map<String, Value> {
    the_spoke_type_registered();
    let compiler = Compiler::new();
    let spoke = compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(
                NodeSpec::new(the_spoke_type(), Value::Null)
                    .with_pins_and_studio_fields(pins, Map::new()),
            )
            .expect("the node is named")
            .first()
            .expect("the node is created")
            .id
            .clone()
    });
    compiler.scope(|graph, _tx| {
        let node = graph
            .traversal()
            .v(&spoke)
            .first()
            .expect("the node is held");
        the_unlinked_input_values(graph, node)
    })
}

/// A wire into a connection input is lowered to the reference it brings, and the node is handed
/// that reference as the field's value; with no wire it is handed nothing.
///
/// Mental-revert: a Connection holds no stored value, so reading the input as any other leaves
/// it out, and a Python node's connection field stays `None` while its wire is drawn.
#[test]
fn a_connection_input_is_handed_the_node_its_lowered_wire_brings() {
    let mut lowered = Map::new();
    lowered.insert("hub".to_owned(), json!({"node": "hub"}));

    assert_eq!(
        the_values_handed_a_spoke_holding(lowered).get("hub"),
        Some(&json!({"node": "hub"}))
    );
    assert_eq!(
        the_values_handed_a_spoke_holding(Map::new()).get("hub"),
        None
    );
}
