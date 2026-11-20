// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A node type's pin group repeats a slot of ports on each node: a group following its wires
//! holds one empty slot after the last a link reaches, growing as each is wired and giving the
//! empty slot back as the last link is taken out; a group whose count a person sets holds as
//! many slots as its count input says, and refuses a count that leaves out a linked slot.

use std::sync::Arc;

use serde_json::{Value, json};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    NodeTypePinGroupSlots, PinGroupSlotOnTheWire,
};

use super::connect_on_a_test_thread::{
    connect_a_link_of_a_saved_graph_on_this_thread, connect_on_this_thread,
};
use crate::core::compiler::Compiler;
use crate::core::descriptors::{
    DeclaredPinType, NodeClassImportPath, NodeClassShortName, NodeDescriptor, PinDeclaration,
    PinGroupDescriptor, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
};
use crate::core::graph::{InputLinkPortRef, LinkUniqueId, NodeRunId, OutputLinkPortRef};
use crate::core::nodes::{NODE_REGISTRY, NodeSpec};
use crate::core::{Error, PortDirection};

const FLOAT_SOURCE: &str = "PinGroupFloatSource";
const LAYER_MIXER: &str = "PinGroupLayerMixer";
const PASS_EACH_THROUGH: &str = "PinGroupPassEachThrough";
const TAPS: &str = "PinGroupTaps";

fn class_path(class_name: &str) -> NodeClassImportPath {
    NodeClassImportPath::new(format!("{}::{class_name}", module_path!())).unwrap()
}

fn a_records_port(name: &str, pin_type: DeclaredPinType) -> PortDescriptor {
    PortDescriptor::new(
        name,
        name,
        true,
        PortFamilyDeclaration::of_family(PortFamily::Records),
    )
    .with_pin_declaration(PinDeclaration {
        pin_type,
        ..PinDeclaration::exactly(PinType::Float { exact_size: None })
    })
}

fn a_float_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(
        name,
        name,
        true,
        PortFamilyDeclaration::of_family(PortFamily::Records),
    )
    .with_pin_declaration(PinDeclaration::exactly(PinType::Float { exact_size: None }))
}

fn a_node_type(class_name: &str) -> NodeDescriptor {
    NodeDescriptor::new(
        NodeClassShortName::new(class_name).unwrap(),
        class_path(class_name),
        "a fixture",
    )
}

fn register_the_pin_group_fixture_node_types() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        for descriptor in [
            a_node_type(FLOAT_SOURCE).with_output(a_float_port("value")),
            a_node_type(LAYER_MIXER)
                .with_output(a_float_port("mixed"))
                .with_pin_group(PinGroupDescriptor::new(
                    "layers",
                    "each layer mixed",
                    vec![a_float_port("layer")],
                    Vec::new(),
                )),
            a_node_type(PASS_EACH_THROUGH).with_pin_group(PinGroupDescriptor::new(
                "lanes",
                "each lane passed through",
                vec![a_records_port("in", DeclaredPinType::Any)],
                vec![a_records_port(
                    "out",
                    DeclaredPinType::SameAsInput {
                        input_port_name: "in".to_owned(),
                    },
                )],
            )),
            a_node_type(TAPS).with_pin_group(PinGroupDescriptor::new(
                "taps",
                "each tap",
                Vec::new(),
                vec![a_float_port("tap")],
            )),
        ] {
            NODE_REGISTRY
                .register_descriptor_only(descriptor)
                .expect("register the fixture descriptor");
        }
    });
}

fn a_node_with_values(
    compiler: &Compiler,
    class_name: &str,
    values: serde_json::Map<String, Value>,
) -> NodeRunId {
    compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(
                NodeSpec::new(class_path(class_name), Value::Null)
                    .with_pins_and_studio_fields(values, serde_json::Map::new()),
            )
            .expect("the node is named")
            .first()
            .expect("the node is created")
            .id
            .clone()
    })
}

fn a_node_of(compiler: &Compiler, class_name: &str) -> NodeRunId {
    a_node_with_values(compiler, class_name, serde_json::Map::new())
}

/// The names of the ports of `direction` node `node` holds.
fn the_ports_of(compiler: &Compiler, node: &NodeRunId, direction: PortDirection) -> Vec<String> {
    compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .v(node)
            .first()
            .expect("the node is held")
            .ports
            .on(direction)
            .iter()
            .map(|port| port.name.clone())
            .collect()
    })
}

fn a_link(compiler: &Arc<Compiler>, from: &NodeRunId, to: &NodeRunId, input: &str) -> LinkUniqueId {
    connect_on_this_thread(
        compiler,
        OutputLinkPortRef::new(from.clone(), "value"),
        InputLinkPortRef::new(to.clone(), input),
    )
    .expect("a Float links into a Float slot")
}

/// Take `link` out of the graph, as the commit after a disconnect does.
fn take_out(compiler: &Compiler, link: &LinkUniqueId) {
    compiler.scope(|graph, _tx| {
        graph.traversal_mut().e(link).drop();
    });
}

/// Mental-revert: without the growth on a link added, the mixer keeps its one slot, and a second
/// source has no empty slot to link into.
#[test]
fn an_input_group_grows_one_empty_slot_after_each_wired_one() {
    register_the_pin_group_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let mixer = a_node_of(&compiler, LAYER_MIXER);
    assert_eq!(
        the_ports_of(&compiler, &mixer, PortDirection::Input),
        ["layers.0.layer"],
        "a node just placed shows one empty slot"
    );

    let first = a_node_of(&compiler, FLOAT_SOURCE);
    let second = a_node_of(&compiler, FLOAT_SOURCE);
    a_link(&compiler, &first, &mixer, "layers.0.layer");
    assert_eq!(
        the_ports_of(&compiler, &mixer, PortDirection::Input),
        ["layers.0.layer", "layers.1.layer"]
    );
    a_link(&compiler, &second, &mixer, "layers.1.layer");
    assert_eq!(
        the_ports_of(&compiler, &mixer, PortDirection::Input),
        ["layers.0.layer", "layers.1.layer", "layers.2.layer"]
    );
}

/// Mental-revert: without the reshape on a link taken out, the empty slot after the last link
/// stays, and the group holds two empty slots.
#[test]
fn taking_out_the_last_slots_link_gives_its_empty_slot_back() {
    register_the_pin_group_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let mixer = a_node_of(&compiler, LAYER_MIXER);
    let first = a_node_of(&compiler, FLOAT_SOURCE);
    let second = a_node_of(&compiler, FLOAT_SOURCE);
    a_link(&compiler, &first, &mixer, "layers.0.layer");
    let into_the_second_slot = a_link(&compiler, &second, &mixer, "layers.1.layer");

    take_out(&compiler, &into_the_second_slot);

    assert_eq!(
        the_ports_of(&compiler, &mixer, PortDirection::Input),
        ["layers.0.layer", "layers.1.layer"]
    );
}

/// A saved graph's links reach their slots in any order, so a link into a slot past the last
/// grows the group to hold it.
#[test]
fn a_saved_graphs_link_into_a_later_slot_grows_the_group_to_it() {
    register_the_pin_group_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let mixer = a_node_of(&compiler, LAYER_MIXER);
    let source = a_node_of(&compiler, FLOAT_SOURCE);

    connect_a_link_of_a_saved_graph_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "value"),
        InputLinkPortRef::new(mixer.clone(), "layers.2.layer"),
    )
    .expect("a saved link into slot 2 grows the group");

    assert_eq!(
        the_ports_of(&compiler, &mixer, PortDirection::Input),
        [
            "layers.0.layer",
            "layers.1.layer",
            "layers.2.layer",
            "layers.3.layer"
        ]
    );
}

/// A slot holding an input and an output that follows the input's type: each slot's output
/// follows its own slot's input.
#[test]
fn a_slots_output_follows_the_input_of_its_own_slot() {
    register_the_pin_group_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let lanes = a_node_of(&compiler, PASS_EACH_THROUGH);
    let source = a_node_of(&compiler, FLOAT_SOURCE);
    a_link(&compiler, &source, &lanes, "lanes.0.in");

    let followed = compiler.scope(|graph, _tx| {
        let node = graph
            .traversal()
            .v(&lanes)
            .first()
            .expect("the node is held");
        ["lanes.0.out", "lanes.1.out"].map(|output| {
            match node
                .ports
                .output_named(output)
                .and_then(|port| port.declared_pin_type.as_ref())
                .map(|declared| &declared.pin_type)
            {
                Some(DeclaredPinType::SameAsInput { input_port_name }) => input_port_name.clone(),
                other => panic!("`{output}` follows an input, not {other:?}"),
            }
        })
    });

    assert_eq!(followed, ["lanes.0.in", "lanes.1.in"]);
}

/// Mental-revert: without the count input read, the node holds the one slot of a type's ports.
#[test]
fn an_output_group_holds_as_many_slots_as_its_count_input_says() {
    register_the_pin_group_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let one_tap = a_node_of(&compiler, TAPS);
    let three_taps = a_node_with_values(
        &compiler,
        TAPS,
        serde_json::Map::from_iter([("taps".to_owned(), json!(3))]),
    );

    assert_eq!(
        the_ports_of(&compiler, &one_tap, PortDirection::Output),
        ["taps.0.tap"]
    );
    assert_eq!(
        the_ports_of(&compiler, &three_taps, PortDirection::Output),
        ["taps.0.tap", "taps.1.tap", "taps.2.tap"]
    );
    assert_eq!(
        the_ports_of(&compiler, &three_taps, PortDirection::Input),
        ["taps"],
        "the count is an input of the group's name"
    );
}

#[test]
fn a_count_leaving_out_a_linked_slot_is_refused_naming_it() {
    register_the_pin_group_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let taps = a_node_with_values(
        &compiler,
        TAPS,
        serde_json::Map::from_iter([("taps".to_owned(), json!(3))]),
    );
    let mixer = a_node_of(&compiler, LAYER_MIXER);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(taps.clone(), "taps.2.tap"),
        InputLinkPortRef::new(mixer, "layers.0.layer"),
    )
    .expect("a tap links into a layer");

    let refused = compiler.scope(|graph, _tx| {
        graph.refuse_a_slot_count_leaving_out_a_linked_slot(&taps, "taps", Some(&json!(2)))
    });

    let words = match refused {
        Err(Error::Configuration(words)) => words,
        other => panic!("a count leaving out slot 2 is refused, not {other:?}"),
    };
    assert!(words.contains("a link reaches its slot 2"), "{words}");
    compiler
        .scope(|graph, _tx| {
            graph.refuse_a_slot_count_leaving_out_a_linked_slot(&taps, "taps", Some(&json!(3)))
        })
        .expect("a count holding the linked slot is taken");
}

#[test]
fn a_pin_group_named_with_the_slot_separator_is_refused_at_registration() {
    let refused = NODE_REGISTRY.register_descriptor_only(
        a_node_type("PinGroupNamedWithADot").with_pin_group(PinGroupDescriptor::new(
            "lay.ers",
            "",
            vec![a_float_port("layer")],
            Vec::new(),
        )),
    );

    let words = refused
        .expect_err("a dot separates a slot's index")
        .to_string();
    assert!(words.contains("lay.ers"), "{words}");
}

/// Mental-revert: without the slot rendered, `graph` serves the slot's ports as any other.
#[test]
fn graph_names_the_group_and_slot_of_each_slot_port() {
    register_the_pin_group_fixture_node_types();
    let compiler = Arc::new(Compiler::new());
    let mixer = a_node_of(&compiler, LAYER_MIXER);
    let source = a_node_of(&compiler, FLOAT_SOURCE);
    a_link(&compiler, &source, &mixer, "layers.0.layer");

    let rendered = compiler.scope(|graph, _tx| {
        crate::core::json_schema::GraphNodeOutput::from(
            graph
                .traversal()
                .v(&mixer)
                .first()
                .expect("the node is held"),
        )
    });

    let second_slot = rendered
        .ports
        .inputs
        .iter()
        .find(|port| port.name == "layers.1.layer")
        .expect("the empty slot is served");
    assert_eq!(
        second_slot.pin_group_slot,
        Some(PinGroupSlotOnTheWire {
            group: "layers".to_owned(),
            slot: 1,
            pin: "layer".to_owned(),
        })
    );
    let mixed = rendered
        .ports
        .outputs
        .iter()
        .find(|port| port.name == "mixed")
        .expect("the declared output is served");
    assert_eq!(mixed.pin_group_slot, None, "a port of no group names none");
}

#[test]
fn describe_node_types_lists_each_group_with_its_slots_pins_and_its_count_input() {
    register_the_pin_group_fixture_node_types();
    let descriptor = NODE_REGISTRY
        .descriptor(&class_path(TAPS))
        .expect("the fixture is registered");

    let described = crate::core::nodes::NodeTypePortsDescribedOnTheWire::of(&descriptor);

    let [group] = described.pin_groups.as_slice() else {
        panic!("one group: {:?}", described.pin_groups);
    };
    assert_eq!(group.name, "taps");
    assert_eq!(
        group.slots,
        NodeTypePinGroupSlots::SetByTheInputOfTheGroupsName
    );
    assert_eq!(
        group
            .outputs
            .iter()
            .map(|port| port.name.as_str())
            .collect::<Vec<_>>(),
        ["tap"]
    );
    assert_eq!(
        described
            .inputs
            .iter()
            .map(|input| input.port.name.as_str())
            .collect::<Vec<_>>(),
        ["taps"],
        "the count is listed among the inputs, and no slot's port is"
    );
    let mixer = crate::core::nodes::NodeTypePortsDescribedOnTheWire::of(
        &NODE_REGISTRY
            .descriptor(&class_path(LAYER_MIXER))
            .expect("the fixture is registered"),
    );
    assert_eq!(
        mixer.pin_groups[0].slots,
        NodeTypePinGroupSlots::FollowTheirWires
    );
    assert!(mixer.inputs.is_empty(), "{:?}", mixer.inputs);
}
