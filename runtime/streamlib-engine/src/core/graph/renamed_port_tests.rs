// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A port a node type renamed is found under each name it had, and what the graph holds, reads
//! and answers names it by its current name: a node's values, a link's ends, a graph document read
//! in, and the check of a value set on it. An input that may be unset takes no value at all.

use serde_json::{Map, Value, json};

use super::graph_snapshot::GraphSnapshot;
use super::input_values::{
    InputValueRefusalCode, NodeAnInputValueIsSetOn, refuse_an_input_value_its_input_does_not_take,
};
use super::{Graph, GraphNodePorts, InputLinkPortRef, OutputLinkPortRef, PortInfo};
use crate::core::descriptors::NodeClassImportPath;
use crate::core::error::Error;
use crate::core::node_interpreter::PythonNodeDeclaration;
use crate::core::nodes::HowANodeOfADescribedTypeRuns;
use crate::core::nodes::NODE_REGISTRY;
use crate::core::nodes::{NodeSpec, NodeTypesOneStreamResolves};
use crate::core::test_support::{MockPinTypedNode, ensure_test_mocks_registered};

/// Mock node whose ports were renamed: its input `gain` was `level`, and may be unset; its input
/// `picture` was `video_in`; its output `frames` was `video`.
#[crate::node(
    execution = reactive,
    input("gain", pin_type = Float, former_names = ["level"], may_be_unset = true),
    input("picture", pin_type = Texture, former_names = ["video_in"]),
    output("frames", pin_type = Texture, formats = ["rgba8_unorm"], former_names = ["video"]),
)]
struct MockRenamedPortsNode;

impl crate::core::ReactiveNode for MockRenamedPortsNode::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        Ok(())
    }
}

/// Register the test mocks and [`MockRenamedPortsNode`], once.
fn ensure_the_renamed_ports_node_registered() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    ensure_test_mocks_registered();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<MockRenamedPortsNode::Node>());
}

/// Add a node of `node_type` named `name`, holding `pins`, to `graph`, answering its run id.
fn add_a_node_holding(
    graph: &mut Graph,
    node_type: NodeClassImportPath,
    name: &str,
    pins: Value,
) -> String {
    let Value::Object(pins) = pins else {
        panic!("the pins are an object");
    };
    graph
        .traversal_mut()
        .add_v(
            NodeSpec::new(node_type, json!({}))
                .with_display_name(name)
                .with_input_values_and_studio_fields(pins, Map::new()),
        )
        .expect("the node is added")
        .first()
        .expect("the node is held")
        .id
        .to_string()
}

/// A value saved under a name an input had lands on the input under its name, beside a value
/// saved under its name, which stays; a link to and from a port under a name it had joins the
/// ports under their names, and the node answers the port by either name.
///
/// Mental-revert: holding the pins as given leaves `level` on the node and `gain` reading
/// nothing; finding a port by exact name alone adds no link to `video_in`.
#[test]
fn a_value_and_a_link_under_a_name_a_port_had_land_on_the_port_under_its_name() {
    ensure_the_renamed_ports_node_registered();
    let mut graph = Graph::new();
    let renamed = add_a_node_holding(
        &mut graph,
        MockRenamedPortsNode::node_class_import_path(),
        "mixer",
        json!({ "level": 0.25 }),
    );
    let held_twice = add_a_node_holding(
        &mut graph,
        MockRenamedPortsNode::node_class_import_path(),
        "mixer-2",
        json!({ "level": 0.25, "gain": 0.75 }),
    );
    let source = add_a_node_holding(
        &mut graph,
        MockPinTypedNode::node_class_import_path(),
        "source",
        json!({}),
    );

    let pins_of = |graph: &Graph, node: &str| {
        graph
            .traversal()
            .v(node)
            .first()
            .expect("the node is held")
            .input_values
            .clone()
    };
    assert_eq!(
        Value::Object(pins_of(&graph, &renamed)),
        json!({ "gain": 0.25 })
    );
    assert_eq!(
        Value::Object(pins_of(&graph, &held_twice)),
        json!({ "gain": 0.75 })
    );

    let into_the_former_name = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(source.as_str(), "frames"),
            InputLinkPortRef::new(renamed.as_str(), "video_in"),
        )
        .first()
        .map(|link| link.to_port().port_name().to_owned());
    assert_eq!(into_the_former_name.as_deref(), Some("picture"));
    let out_of_the_former_name = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(renamed.as_str(), "video"),
            InputLinkPortRef::new(held_twice.as_str(), "picture"),
        )
        .first()
        .map(|link| link.source().port_name().to_owned());
    assert_eq!(out_of_the_former_name.as_deref(), Some("frames"));

    let node = graph.traversal().v(renamed.as_str()).first().unwrap();
    assert_eq!(node.input("level").port_name(), "gain");
    assert_eq!(node.output("video").port_name(), "frames");
    assert!(node.try_input("loudness").is_none());
}

/// A graph document whose values and links name ports by names they had reads with every one
/// under its port's current name, so what validates, loads and reconciles after it knows only
/// the current names.
///
/// Mental-revert: dropping the step that moves them to the current names leaves `level` among
/// the mixer's values and the link naming `video_in`, which validation refuses.
#[test]
fn a_graph_document_naming_ports_by_names_they_had_reads_under_their_names() {
    ensure_the_renamed_ports_node_registered();
    let document = json!({
        "nodes": [
            { "name": "source", "type": MockPinTypedNode::node_class_import_path().as_str() },
            { "name": "mixer", "type": MockRenamedPortsNode::node_class_import_path().as_str(),
              "input_values": { "level": 0.5 } },
            { "name": "mixer-2", "type": MockRenamedPortsNode::node_class_import_path().as_str() },
        ],
        "links": [
            { "source": { "node": "source", "port": "frames" },
              "target": { "node": "mixer", "port": "video_in" } },
            { "source": { "node": "mixer", "port": "video" },
              "target": { "node": "mixer-2", "port": "Video_In" } },
        ],
    });
    let node_types = NodeTypesOneStreamResolves::new();
    let resolution = GraphSnapshot::from_graph_document(document)
        .expect("the document reads")
        .with_graph_only_nodes_resolved(&node_types)
        .expect("its graph-only nodes resolve");
    resolution
        .validate(&node_types)
        .expect("the graph under its current port names validates");

    let graph = &resolution.graph_the_runtime_runs;
    let mixer = graph
        .nodes
        .iter()
        .find(|node| node.name == "mixer")
        .expect("the mixer is held");
    assert_eq!(
        Value::Object(mixer.input_values.clone()),
        json!({ "gain": 0.5 })
    );
    let link_ends: Vec<(&str, &str)> = graph
        .links
        .iter()
        .map(|link| (link.source.port.as_str(), link.target.port.as_str()))
        .collect();
    assert_eq!(link_ends, [("frames", "picture"), ("frames", "picture")]);
}

/// The refusal, if any, of `value` set on input `input` of a node named `mixer` of `node_type`.
fn the_refusal_of(
    node_type: &NodeClassImportPath,
    input: &str,
    value: Value,
) -> Result<(), super::input_values::InputValueRefusal> {
    ensure_the_renamed_ports_node_registered();
    let input_values_after_the_set = Map::from_iter([(input.to_owned(), value.clone())]);
    refuse_an_input_value_its_input_does_not_take(
        &NodeTypesOneStreamResolves::new(),
        &NodeAnInputValueIsSetOn {
            name: "mixer",
            node_type,
            input_values_after_the_set: &input_values_after_the_set,
        },
        input,
        None,
        Some(&value),
    )
}

/// A value set under a name an input had is checked as the input's, and refused naming the
/// input by its name; an input that may be unset takes `null`, which one that may not refuses.
///
/// Mental-revert: finding the input by exact name refuses `level` as no input; checking `null`
/// by the pin type alone refuses it on `gain`.
#[test]
fn a_value_set_under_a_name_an_input_had_is_checked_as_the_inputs_and_null_as_unset() {
    let renamed = MockRenamedPortsNode::node_class_import_path();
    assert_eq!(the_refusal_of(&renamed, "level", json!(0.5)), Ok(()));
    let refusal = the_refusal_of(&renamed, "level", json!("loud")).expect_err("text is no Float");
    assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
    assert!(refusal.words.contains("input `gain`"), "{refusal}");

    assert_eq!(the_refusal_of(&renamed, "gain", Value::Null), Ok(()));
    let refusal = the_refusal_of(
        &MockPinTypedNode::node_class_import_path(),
        "gain",
        Value::Null,
    )
    .expect_err("an input that must hold a value refuses null");
    assert_eq!(refusal.code, InputValueRefusalCode::InputValueNotValid);
}

/// A fragment whose inner node is linked to the fragment's interface under names its ports had
/// loads: each level is read under the current port names before it is flattened, so the
/// fragment's interface checks and the flattened links name the current ports.
///
/// Mental-revert: moving names only once the fragments are flattened refuses the fragment, its
/// interface check naming `video_in` as a port the mixer does not have.
#[test]
fn a_fragment_linking_its_interface_to_ports_under_names_they_had_loads_under_their_names() {
    ensure_the_renamed_ports_node_registered();
    let renamed = MockRenamedPortsNode::node_class_import_path();
    let document = json!({
        "nodes": [
            { "name": "source", "type": MockPinTypedNode::node_class_import_path().as_str() },
            { "name": "look", "type": "looks.generators:look",
              "fragment": {
                  "nodes": [
                      { "name": "in", "type": "tatolab.stream:GraphInput",
                        "input_values": { "port": "video", "pin_type": { "type": "texture" } } },
                      { "name": "mixer", "type": renamed.as_str(),
                        "input_values": { "level": 0.5 } },
                      { "name": "out", "type": "tatolab.stream:GraphOutput",
                        "input_values": { "port": "video", "pin_type": { "type": "texture" } } },
                  ],
                  "links": [
                      { "source": { "node": "in", "port": "value" },
                        "target": { "node": "mixer", "port": "video_in" } },
                      { "source": { "node": "mixer", "port": "video" },
                        "target": { "node": "out", "port": "value" } },
                  ],
              } },
            { "name": "mixer-2", "type": renamed.as_str() },
        ],
        "links": [
            { "source": { "node": "source", "port": "frames" },
              "target": { "node": "look", "port": "video" } },
            { "source": { "node": "look", "port": "video" },
              "target": { "node": "mixer-2", "port": "video_in" } },
        ],
    });
    let node_types = NodeTypesOneStreamResolves::new();
    let graph = GraphSnapshot::from_graph_document(document).expect("the document reads");
    graph
        .validate(&node_types)
        .expect("the fragment under its current port names validates");
    let resolved = graph
        .with_graph_only_nodes_resolved(&node_types)
        .expect("its graph-only nodes resolve")
        .graph_the_runtime_runs;
    let mut link_ends: Vec<(String, String)> = resolved
        .links
        .iter()
        .map(|link| {
            (
                format!("{}/{}", link.source.node, link.source.port),
                format!("{}/{}", link.target.node, link.target.port),
            )
        })
        .collect();
    link_ends.sort();
    assert_eq!(
        link_ends,
        [
            ("look/mixer/frames".to_owned(), "mixer-2/picture".to_owned()),
            ("source/frames".to_owned(), "look/mixer/picture".to_owned()),
        ]
    );
    let mixer = resolved
        .nodes
        .iter()
        .find(|node| node.name == "look/mixer")
        .expect("the fragment's mixer is held");
    assert_eq!(
        Value::Object(mixer.input_values.clone()),
        json!({ "gain": 0.5 })
    );
}

/// A `SetVariable` linked from an output under a name it had holds the output under its
/// current name, so the check that the setter holds a port the graph holds passes.
///
/// Mental-revert: holding the setter's link as written refuses it, naming `mixer/video` as a
/// port the graph does not hold; moving names only once fragments are flattened refuses the
/// setter inside the fragment.
#[test]
fn a_set_variable_holding_an_output_under_a_name_it_had_holds_it_under_its_name() {
    ensure_the_renamed_ports_node_registered();
    let document = json!({
        "nodes": [
            { "name": "mixer", "type": MockRenamedPortsNode::node_class_import_path().as_str() },
            { "name": "set-frames", "type": "tatolab.stream:SetVariable",
              "input_values": { "variable": "frames" } },
        ],
        "links": [
            { "source": { "node": "mixer", "port": "video" },
              "target": { "node": "set-frames", "port": "value" } },
        ],
    });
    let inside_a_fragment = json!({
        "nodes": [
            { "name": "look", "type": "looks.generators:look",
              "fragment": document.clone() },
        ],
        "links": [],
    });
    for document in [document, inside_a_fragment] {
        GraphSnapshot::from_graph_document(document)
            .expect("the document reads")
            .validate(&NodeTypesOneStreamResolves::new())
            .expect("the setter holds the mixer's output under its current name");
    }
}

/// The input `gain`, which was `level` and before that `volume`.
fn ports_of_an_input_renamed_twice() -> GraphNodePorts {
    GraphNodePorts {
        inputs: vec![PortInfo {
            former_names: vec!["level".to_owned(), "volume".to_owned()],
            ..PortInfo::declared_by_no_node_type("gain", None)
        }],
        outputs: Vec::new(),
    }
}

/// An input holding values under several of its names reads the one under its current name,
/// else the one under the first name it declares it had; every other is dropped, and a key no
/// input names is kept.
///
/// Mental-revert: keeping whichever value the map yields last reads `volume`'s 1.0 for the
/// first case.
#[test]
fn an_input_holding_values_under_several_names_reads_the_current_then_the_first_declared() {
    let ports = ports_of_an_input_renamed_twice();
    let read = |values: Value| {
        let Value::Object(values) = values else {
            panic!("the values are an object");
        };
        Value::Object(ports.input_values_under_the_current_names(values))
    };
    assert_eq!(
        read(json!({ "volume": 1.0, "level": 2.0, "other": true })),
        json!({ "gain": 2.0, "other": true })
    );
    assert_eq!(
        read(json!({ "volume": 1.0, "level": 2.0, "gain": 3.0 })),
        json!({ "gain": 3.0 })
    );
    assert_eq!(read(json!({ "volume": 1.0 })), json!({ "gain": 1.0 }));
    let moved = ports.inputs_holding_a_value_under_a_name_they_had(|name| name != "level");
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].name_read, "gain");
    assert_eq!(moved[0].other_names_held, ["volume"]);
}

/// What the Python node `Hum` declares under bare `@node`, as `tatolab-stream`'s own test reads
/// it off the decorator: its input `frequency` was `frequency_hz` and, before that, `Pitch`; its
/// input `detune` may be unset; its output `result` was `level`.
const RENAMED_PYTHON_PIN_DECLARATION: &str =
    include_str!("../../../tests/fixtures/renamed_python_pin_declaration.json");

/// `node_types`, holding `Hum` as a project's interpreter describes it, under `import_path`.
fn node_types_holding_the_renamed_python_node(import_path: &str) -> NodeTypesOneStreamResolves {
    let declared: Value =
        serde_json::from_str(RENAMED_PYTHON_PIN_DECLARATION).expect("the fixture is JSON");
    let described = json!({
        "import_path": import_path,
        "short_name": "Hum",
        "description": "",
        "kind": "function",
        "language": "python",
        "runs_as_python": true,
        "state_fields": [],
        "config_schema": {"type": "object"},
        "input_ports": declared["input_ports"],
        "output_ports": declared["output_ports"],
        "function_signature": declared["function_signature"],
    });
    let declaration = PythonNodeDeclaration::read_from_described_node_type(
        &described,
        &NodeClassImportPath::new(import_path).expect("the import path names a class"),
    )
    .expect("the engine reads what the decorator declares");
    let node_types = NodeTypesOneStreamResolves::new();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            declaration.descriptor,
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no node in this test",
                        node.id
                    )))
                }),
            },
        )
        .expect("the stream registers the described type");
    node_types
}

/// A document saved before a Python node's pins were renamed loads onto the node that declares
/// their new names: the value saved under `frequency_hz` lands on `frequency`, a link into
/// `pitch` and out of `level` joins `frequency` and `result`, and `detune`, which may be unset,
/// holds no value.
///
/// Mental-revert: dropping `former_names` from what the Python reader declares leaves
/// `frequency_hz` among the hum's values and the links naming ports the node does not have,
/// which validation refuses.
#[test]
fn a_document_saved_under_the_names_a_python_nodes_pins_had_loads_under_their_names() {
    let hum = "project_nodes.hum:Hum";
    let node_types = node_types_holding_the_renamed_python_node(hum);
    let document = json!({
        "nodes": [
            { "name": "hum", "type": hum, "input_values": { "frequency_hz": 220.0 } },
            { "name": "hum-2", "type": hum },
        ],
        "links": [
            { "source": { "node": "hum", "port": "level" },
              "target": { "node": "hum-2", "port": "Pitch" } },
        ],
    });
    let resolution = GraphSnapshot::from_graph_document(document)
        .expect("the document reads")
        .with_graph_only_nodes_resolved(&node_types)
        .expect("its graph-only nodes resolve");
    resolution
        .validate(&node_types)
        .expect("the graph under the pins' current names validates");

    let graph = &resolution.graph_the_runtime_runs;
    let input_values_of = |name: &str| {
        let node = graph
            .nodes
            .iter()
            .find(|node| node.name == name)
            .expect("the node is held");
        Value::Object(node.input_values.clone())
    };
    assert_eq!(input_values_of("hum"), json!({ "frequency": 220.0 }));
    assert_eq!(input_values_of("hum-2"), json!({}));
    let link_ends: Vec<(&str, &str)> = graph
        .links
        .iter()
        .map(|link| (link.source.port.as_str(), link.target.port.as_str()))
        .collect();
    assert_eq!(link_ends, [("result", "frequency")]);
}
