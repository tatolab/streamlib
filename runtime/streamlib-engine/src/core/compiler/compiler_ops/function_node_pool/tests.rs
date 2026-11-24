// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_class_node_feeding, a_function_node_type, a_graph_resolving_these_function_node_types,
    a_graph_resolving_three_function_node_types, a_helper_reader_before_its_setup, add_a_node,
    link,
};
use crate::core::compiler::compiler_ops::function_node_island::{
    HowALinkIsWired, how_a_link_is_wired, wire_a_link_from_a_function_node_island_into_its_reader,
};
use crate::core::compiler::compiler_ops::shader_node_island::the_host_input_port_a_fused_shader_node_input_is_read_at;
use crate::core::compiler::compiler_ops::shader_node_island_program::tests::shader_program;
use crate::core::nodes::HowANodeOfADescribedTypeRuns;
use crate::core::test_support::{MockInputOnlyNode, make_in1_a_setting_read_at_setup};

pub(crate) fn a_graph_resolving_the_pool_and_three_function_node_types() -> Graph {
    let graph = a_graph_resolving_three_function_node_types();
    graph
        .node_types_this_stream_resolves()
        .register_a_type_described_in_this_streams_node_interpreter(
            the_function_node_pool_descriptor(None).unwrap(),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|node| {
                Err(Error::Configuration(format!(
                    "`{}` builds no node in this test",
                    node.id
                )))
            })),
        )
        .unwrap();
    graph
}

pub(crate) fn a_reader_in_the_app_process(graph: &mut Graph) -> NodeRunId {
    add_a_node(
        graph,
        MockInputOnlyNode::node_class_import_path().as_str(),
        JsonValue::Null,
    )
}

fn the_links_of(graph: &Graph) -> Vec<(String, String, Option<String>)> {
    let mut links: Vec<(String, String, Option<String>)> = graph
        .traversal()
        .e(())
        .iter()
        .map(|link| {
            (
                link.source().to_string(),
                link.to_port().to_string(),
                link.get::<LinkCarriedThroughTheFunctionNodePoolComponent>()
                    .map(|carried| carried.carried_link_id.to_string()),
            )
        })
        .collect();
    links.sort();
    links
}

/// A graph whose `lfo` feeds a reader in the app process, the link carried
/// through the pool: the graph, `lfo`, the reader and the carried link.
pub(crate) fn a_graph_carrying_an_lfo_into_its_reader_through_the_pool()
-> (Graph, NodeRunId, NodeRunId, LinkUniqueId) {
    let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_reader_in_the_app_process(&mut graph);
    let lfo_into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
    let mut nodes_to_add = vec![lfo.clone(), reader.clone()];
    let mut links_to_add = vec![lfo_into_the_reader.clone()];
    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut nodes_to_add,
        &mut links_to_add,
    )
    .unwrap();
    assert_eq!(
        the_links_a_link_is_wired_as(&graph, &lfo_into_the_reader).len(),
        2,
        "the link is carried through the pool"
    );
    (graph, lfo, reader, lfo_into_the_reader)
}

#[test]
fn graph_renders_the_pool_beside_the_nodes_and_each_carried_link_as_it_was_given() {
    let (graph, lfo, reader, lfo_into_the_reader) =
        a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
    let name_of = |node_run_id: &NodeRunId| {
        graph
            .traversal()
            .v(node_run_id)
            .first()
            .unwrap()
            .display_name
            .clone()
    };

    let rendered = graph.to_graph_response("rig".to_string());

    assert!(
        rendered
            .nodes
            .iter()
            .all(|node| !is_the_function_node_pool_type(&node.node_type)),
        "the pool is no node of the graph: {:?}",
        rendered
            .nodes
            .iter()
            .map(|node| &node.name)
            .collect::<Vec<_>>()
    );
    assert_eq!(rendered.nodes.len(), 2);
    let pool = rendered
        .function_node_pool
        .as_ref()
        .expect("`function_node_pool` renders the pool");
    assert_eq!(
        pool.config.as_ref().unwrap()["carries"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(rendered.links.len(), 1, "{:?}", rendered.links);
    let carried = &rendered.links[0];
    assert_eq!(carried.id, lfo_into_the_reader.to_string());
    assert_eq!(
        (carried.source.node.as_str(), carried.source.port.as_str()),
        (name_of(&lfo).as_str(), "result")
    );
    assert_eq!(
        (carried.target.node.as_str(), carried.target.port.as_str()),
        (name_of(&reader).as_str(), "in1")
    );
    assert_eq!(
        serde_json::to_value(&carried.components.carried_through_the_function_node_pool).unwrap(),
        serde_json::json!({"input_port": "island_0", "output_port": "island_0_output"})
    );
}

#[test]
fn a_graph_rendered_with_a_pool_loads_as_the_graph_it_was_given() {
    let (graph, ..) = a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
    let rendered = serde_json::to_string(&graph.to_graph_response("rig".to_string())).unwrap();

    let loaded = crate::core::graph::graph_snapshot::GraphSnapshot::from_json_str(&rendered)
        .expect("`graph`'s output loads");

    assert_eq!(loaded.nodes.len(), 2);
    assert_eq!(loaded.links.len(), 1);
}

#[test]
fn islands_read_in_the_app_process_are_carried_through_one_pool() {
    let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let first_reader = a_reader_in_the_app_process(&mut graph);
    let second_reader = a_reader_in_the_app_process(&mut graph);
    let lfo_into_remap = link(&mut graph, (&lfo, "result"), (&remap, "x"));
    let remap_into_the_first = link(&mut graph, (&remap, "result"), (&first_reader, "in1"));
    let lfo_into_the_second = link(&mut graph, (&lfo, "result"), (&second_reader, "in1"));
    let mut nodes_to_add = vec![lfo.clone(), remap.clone(), first_reader, second_reader];
    let mut links_to_add = vec![
        lfo_into_remap.clone(),
        remap_into_the_first.clone(),
        lfo_into_the_second.clone(),
    ];

    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut nodes_to_add,
        &mut links_to_add,
    )
    .unwrap();

    let pool = the_function_node_pool_of(&graph).expect("the stream has a pool");
    assert_eq!(
        nodes_to_add.iter().filter(|id| **id == pool).count(),
        1,
        "one pool, however many islands it carries"
    );
    assert_eq!(links_to_add.len(), 5);
    assert!(links_to_add.contains(&lfo_into_remap));
    assert!(!links_to_add.contains(&remap_into_the_first));
    assert_eq!(
        the_links_a_link_is_wired_as(&graph, &remap_into_the_first).len(),
        2
    );
    for carried in the_links_a_link_is_wired_as(&graph, &lfo_into_the_second) {
        let wired_as = how_a_link_is_wired(&graph, &carried).unwrap();
        let link = graph.traversal().e(&carried).first().unwrap();
        if link.to_port().node_run_id() == &pool {
            assert_eq!(
                wired_as,
                HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead
            );
        } else {
            assert_eq!(wired_as, HowALinkIsWired::ThroughAChannel);
        }
    }
    let pool_node = graph.traversal().v(&pool).first().unwrap();
    assert_eq!(pool_node.ports.inputs.len(), 2);
    assert_eq!(pool_node.ports.outputs.len(), 2);
    assert_eq!(
        pool_node.input_values["carries"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        the_links_of(&graph)
            .iter()
            .filter(|(_, _, carried)| carried.is_some())
            .count(),
        4
    );
}

/// Register `tint`, a shader type taking an Image and a float `amount`.
fn register_a_tint_shader_node_type(graph: &Graph) {
    let import_path = NodeClassImportPath::new("test_shaders:tint").unwrap();
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new("tint").unwrap(),
        import_path,
        "a shader node type for a test",
    );
    descriptor.node_kind = NodeKind::Shader;
    for (port, family) in [
        ("image", PortFamily::Image),
        ("amount", PortFamily::Channels),
    ] {
        descriptor = descriptor.with_input(
            PortDescriptor::new(port, "", true, PortFamilyDeclaration::of_family(family))
                .with_delivery_profile("newest"),
        );
    }
    descriptor = descriptor.with_output(PortDescriptor::new(
        "result",
        "",
        true,
        PortFamilyDeclaration::of_family(PortFamily::Image),
    ));
    graph
        .node_types_this_stream_resolves()
        .register_a_type_described_in_this_streams_node_interpreter(
            descriptor,
            HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                shader_node_program: shader_program(
                    "tint",
                    "return image.pixel * amount;",
                    &["image"],
                    &["amount"],
                ),
                evaluator_of_the_island_it_hosts: Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no node in this test",
                        node.id
                    )))
                }),
            },
        )
        .unwrap();
}

#[test]
fn an_island_a_shader_node_reads_is_carried_through_the_pool_to_its_islands_host() {
    let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
    register_a_tint_shader_node_type(&graph);
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let fused_tint = add_a_node(&mut graph, "test_shaders:tint", JsonValue::Null);
    let host_tint = add_a_node(&mut graph, "test_shaders:tint", JsonValue::Null);
    let lfo_into_remap = link(&mut graph, (&lfo, "result"), (&remap, "x"));
    let remap_into_the_fused_tint = link(&mut graph, (&remap, "result"), (&fused_tint, "amount"));
    let fused_tint_into_the_host = link(&mut graph, (&fused_tint, "result"), (&host_tint, "image"));
    let lfo_into_the_host = link(&mut graph, (&lfo, "result"), (&host_tint, "amount"));
    let mut nodes_to_add = vec![lfo, remap, fused_tint.clone(), host_tint.clone()];
    let mut links_to_add = vec![
        lfo_into_remap,
        remap_into_the_fused_tint.clone(),
        fused_tint_into_the_host,
        lfo_into_the_host.clone(),
    ];

    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut nodes_to_add,
        &mut links_to_add,
    )
    .unwrap();

    let pool = the_function_node_pool_of(&graph).expect("the stream has a pool");
    let how_the_link_out_of_the_pool_is_wired = |carried_link_id: &LinkUniqueId| {
        let carried_as = the_links_a_link_is_wired_as(&graph, carried_link_id);
        assert_eq!(carried_as.len(), 2);
        let out_of_the_pool = carried_as
            .iter()
            .find(|link_id| {
                graph
                    .traversal()
                    .e(*link_id)
                    .first()
                    .is_some_and(|link| link.source().node_run_id() == &pool)
            })
            .expect("one of the two links leaves the pool");
        how_a_link_is_wired(&graph, out_of_the_pool).unwrap()
    };
    assert_eq!(
        how_the_link_out_of_the_pool_is_wired(&remap_into_the_fused_tint),
        HowALinkIsWired::ThroughAChannelIntoTheHostOfItsShaderNodeIsland(
            the_host_input_port_a_fused_shader_node_input_is_read_at(
                &host_tint,
                &fused_tint,
                "amount"
            )
        )
    );
    assert_eq!(
        how_the_link_out_of_the_pool_is_wired(&lfo_into_the_host),
        HowALinkIsWired::ThroughAChannel
    );
}

#[test]
fn an_island_a_helper_reads_stays_inside_its_read() {
    let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    let lfo_into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
    let mut nodes_to_add = vec![lfo, reader];
    let mut links_to_add = vec![lfo_into_the_reader.clone()];

    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut nodes_to_add,
        &mut links_to_add,
    )
    .unwrap();

    assert_eq!(the_function_node_pool_of(&graph), None);
    assert_eq!(links_to_add, vec![lfo_into_the_reader]);
}

const A_CLASS_WITH_A_NATIVE_HALF: &str = "test_nodes:ReadsThroughItsNativeHalf";

/// A graph resolving the pool, `lfo`, a `tally` whose body runs as Python,
/// and a class driven manually, as one lending its ports to native code is.
fn a_graph_resolving_a_manual_class_and_a_function_node_run_as_python() -> Graph {
    let mut tally = a_function_node_type("test_ops:tally", &[], &["result"]);
    tally.function_node_runs_as_python = true;
    let graph = a_graph_resolving_these_function_node_types([
        a_function_node_type("test_ops:lfo", &[], &["result"]),
        tally,
    ]);
    let node_types = graph.node_types_this_stream_resolves();
    let builds_nothing = || -> HowANodeOfADescribedTypeRuns {
        HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|node| {
            Err(Error::Configuration(format!(
                "`{}` builds no node in this test",
                node.id
            )))
        }))
    };
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            the_function_node_pool_descriptor(None).unwrap(),
            builds_nothing(),
        )
        .unwrap();
    let mut manual_class = NodeDescriptor::new(
        NodeClassShortName::new("ReadsThroughItsNativeHalf").unwrap(),
        NodeClassImportPath::new(A_CLASS_WITH_A_NATIVE_HALF).unwrap(),
        "a class driven manually for a test",
    );
    for input_port in ["from_python", "from_compiled"] {
        manual_class = manual_class.with_input(PortDescriptor::new(
            input_port,
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Channels),
        ));
    }
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(manual_class, builds_nothing())
        .unwrap();
    node_types.record_the_execution_a_described_class_declares(
        NodeClassImportPath::new(A_CLASS_WITH_A_NATIVE_HALF).unwrap(),
        ProcessExecution::Manual,
    );
    graph
}

#[test]
fn only_the_island_running_python_into_a_manual_class_goes_to_the_pool() {
    let mut graph = a_graph_resolving_a_manual_class_and_a_function_node_run_as_python();
    let tally = add_a_node(&mut graph, "test_ops:tally", JsonValue::Null);
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = add_a_node(&mut graph, A_CLASS_WITH_A_NATIVE_HALF, JsonValue::Null);
    let python_into_the_reader = link(&mut graph, (&tally, "result"), (&reader, "from_python"));
    let compiled_into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "from_compiled"));
    let mut nodes_to_add = vec![tally, lfo, reader];
    let mut links_to_add = vec![
        python_into_the_reader.clone(),
        compiled_into_the_reader.clone(),
    ];

    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut nodes_to_add,
        &mut links_to_add,
    )
    .unwrap();

    assert!(the_function_node_pool_of(&graph).is_some());
    assert_eq!(
        the_links_a_link_is_wired_as(&graph, &python_into_the_reader).len(),
        2,
        "the island running Python reaches the class on a channel from the pool"
    );
    assert_eq!(
        the_links_a_link_is_wired_as(&graph, &compiled_into_the_reader),
        vec![compiled_into_the_reader.clone()],
        "a compiled island runs inside any read, a native half's included"
    );
    assert!(links_to_add.contains(&compiled_into_the_reader));
}

/// `test_ops:gate`, whose `result` hands on the message at `value` unread.
fn a_gate_type() -> NodeDescriptor {
    let mut gate = a_function_node_type("test_ops:gate", &["value", "open"], &[]);
    let mut follows_its_value = PortFamilyDeclaration::of_family(PortFamily::Image);
    follows_its_value.family_follows_input_port = Some("value".to_string());
    gate = gate.with_output(PortDescriptor::new("result", "", true, follows_its_value));
    gate
}

#[test]
fn a_class_node_read_through_a_gate_by_the_app_process_is_read_at_the_pool_and_handed_on() {
    let graph = a_graph_resolving_these_function_node_types([a_gate_type()]);
    graph
        .node_types_this_stream_resolves()
        .register_a_type_described_in_this_streams_node_interpreter(
            the_function_node_pool_descriptor(None).unwrap(),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|node| {
                Err(Error::Configuration(format!(
                    "`{}` builds no node in this test",
                    node.id
                )))
            })),
        )
        .unwrap();
    let mut graph = graph;
    let class_node = a_class_node_feeding(&mut graph);
    let gate = add_a_node(&mut graph, "test_ops:gate", JsonValue::Null);
    let reader = a_reader_in_the_app_process(&mut graph);
    let into_the_gate = link(&mut graph, (&class_node, "out1"), (&gate, "value"));
    let gate_into_the_reader = link(&mut graph, (&gate, "result"), (&reader, "in1"));
    let mut nodes_to_add = vec![class_node.clone(), gate, reader];
    let mut links_to_add = vec![into_the_gate.clone(), gate_into_the_reader.clone()];

    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut nodes_to_add,
        &mut links_to_add,
    )
    .unwrap();

    let pool = the_function_node_pool_of(&graph).expect("the stream has a pool");
    let class_node_family = graph
        .traversal()
        .v(&class_node)
        .first()
        .unwrap()
        .ports
        .outputs
        .iter()
        .find(|port| port.name == "out1")
        .unwrap()
        .family
        .family;
    let pool_node = graph.traversal().v(&pool).first().unwrap();
    assert_eq!(
        pool_node.input_values["carries"][0],
        serde_json::json!({
            "input_port": "island_0",
            "output_port": "island_0_output",
            "family": "message",
            "message_family": class_node_family.wire_name(),
        })
    );
    assert_eq!(
        pool_node.ports.outputs[0].family.family, class_node_family,
        "the pool hands the message on in the family it arrived in"
    );
    let into_the_pool = the_links_a_link_is_wired_as(&graph, &gate_into_the_reader)
        .into_iter()
        .find(|carried| {
            graph
                .traversal()
                .e(carried)
                .first()
                .unwrap()
                .to_port()
                .node_run_id()
                == &pool
        })
        .unwrap();
    graph
        .traversal_mut()
        .v(&pool)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(
            std::sync::Arc::new(
                crate::core::nodes::OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                    ProcessExecution::Continuous { interval_ms: 10 },
                ),
            ),
        ));

    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_pool,
        &crate::iceoryx2::Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();

    let read_at = graph
        .traversal()
        .e(&into_the_gate)
        .first()
        .unwrap()
        .get::<crate::core::graph::LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
        .and_then(|read_through| read_through.the_read_for_the_island_link(&into_the_pool))
        .map(|read| read.reader_input_port.clone())
        .expect("the pool reads the class node's channel for the island it carries");
    assert_eq!(read_at.node_run_id(), &pool);
}

#[test]
fn a_running_pool_takes_a_new_island_and_names_every_carry_for_its_helper() {
    let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_reader_in_the_app_process(&mut graph);
    let first = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
    let mut nodes_to_add = vec![lfo.clone(), reader];
    let mut links_to_add = vec![first];
    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut nodes_to_add,
        &mut links_to_add,
    )
    .unwrap();

    let pool = the_function_node_pool_of(&graph).expect("the first carry added the pool");

    let late_reader = a_reader_in_the_app_process(&mut graph);
    let late = link(&mut graph, (&lfo, "result"), (&late_reader, "in1"));
    let mut late_links = vec![late.clone()];
    let handed = carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut vec![late_reader.clone()],
        &mut late_links,
    )
    .unwrap()
    .expect("a carry into a running pool is handed to its helper");

    assert_eq!(handed.pool, pool);
    assert_eq!(handed.halves_through_the_pool.len(), 2);
    let carries = graph
        .traversal()
        .v(&pool)
        .first()
        .and_then(|pool| pool.input_values.get("carries").cloned())
        .expect("the pool names its carries");
    let carries = carries
        .as_array()
        .expect("the pool's carries are a list")
        .clone();
    assert_eq!(
        carries.len(),
        2,
        "the running pool keeps its first carry: {carries:?}"
    );
    assert_eq!(carries[1]["input_port"], "island_1");
    assert_eq!(carries[1]["output_port"], "island_1_output");
    let late_halves = the_links_a_link_is_wired_as(&graph, &late);
    assert_eq!(
        late_halves.len(),
        2,
        "the late link is carried through the running pool"
    );
    late_links.sort();
    let mut late_halves_sorted = late_halves.clone();
    late_halves_sorted.sort();
    assert_eq!(
        late_links, late_halves_sorted,
        "the commit wires both halves"
    );
}

#[test]
fn a_carry_into_a_pool_the_commit_adds_hands_nothing_to_a_running_helper() {
    let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_reader_in_the_app_process(&mut graph);
    let lfo_into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));

    let handed = carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut vec![lfo, reader],
        &mut vec![lfo_into_the_reader],
    )
    .unwrap();

    assert_eq!(handed, None);
}

/// An island into a setting its reader reads at setup is carried through the pool, though the
/// reader evaluates islands into its other inputs itself, as no read of the reader takes a
/// setting: its carry is written as the pool sets up, and the half out of the pool is a channel,
/// which the engine reads into the setting. The same island into an input the reader reads is
/// left to the reader.
///
/// Mental-revert: judging a link by its reader alone leaves the island into the setting to a
/// reader that never reads it, and the setting never takes the island's value.
#[test]
fn an_island_into_a_setting_read_at_setup_is_carried_through_the_pool_written_at_its_setup() {
    let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reading_a_setting = a_helper_reader_before_its_setup(&mut graph);
    make_in1_a_setting_read_at_setup(&mut graph, reading_a_setting.as_str());
    let reading_each_run = a_helper_reader_before_its_setup(&mut graph);
    let into_the_setting = link(&mut graph, (&lfo, "result"), (&reading_a_setting, "in1"));
    let into_a_read = link(&mut graph, (&lfo, "result"), (&reading_each_run, "in1"));
    let mut nodes_to_add = vec![lfo, reading_a_setting.clone(), reading_each_run];
    let mut links_to_add = vec![into_the_setting.clone(), into_a_read.clone()];

    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut nodes_to_add,
        &mut links_to_add,
    )
    .unwrap();

    assert_eq!(
        the_links_a_link_is_wired_as(&graph, &into_a_read),
        vec![into_a_read.clone()],
        "a reader reading its input evaluates the island itself"
    );
    assert_eq!(
        how_a_link_is_wired(&graph, &into_a_read).unwrap(),
        HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead
    );
    let carried = the_links_carried_through_the_function_node_pool_of(&graph);
    let [carried] = carried.as_slice() else {
        panic!("one link is carried through the pool");
    };
    assert_eq!(carried.carried_link_id, into_the_setting);
    let out_of_the_pool = carried.out_of_the_function_node_pool;
    assert_eq!(
        out_of_the_pool.to_port().to_string(),
        InputLinkPortRef::new(&reading_a_setting, "in1").to_string()
    );
    assert_eq!(
        how_a_link_is_wired(&graph, &out_of_the_pool.id).unwrap(),
        HowALinkIsWired::ThroughAChannel,
        "the half out of the pool is a channel, read by the engine into the setting"
    );
    let pool = the_function_node_pool_of(&graph).unwrap();
    let carries = graph.traversal().v(&pool).first().unwrap().input_values["carries"].clone();
    assert_eq!(
        carries,
        serde_json::json!([{
            "input_port": "island_0",
            "output_port": "island_0_output",
            "family": "channels",
            "written_at_setup": true,
        }])
    );
}
