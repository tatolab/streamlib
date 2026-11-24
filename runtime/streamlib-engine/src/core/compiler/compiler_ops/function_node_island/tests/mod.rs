// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Fixtures shared by the evaluation island's tests.

use std::sync::Arc;

use super::*;
use crate::core::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, PortDescriptor,
    PortFamily, PortFamilyDeclaration,
};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
use crate::core::nodes::{
    HowANodeOfADescribedTypeRuns, NodeSpec, NodeTypesOneStreamResolves,
    OutOfProcessLinkWiringEnvelope,
};

mod class_node_reads_at_the_reader_tests;
mod island_formation_and_live_edit_tests;

pub(crate) fn a_function_node_type(
    import_path: &str,
    input_ports: &[&str],
    output_ports: &[&str],
) -> NodeDescriptor {
    let import_path = NodeClassImportPath::new(import_path).unwrap();
    let short_name = import_path.as_str().rsplit(':').next().unwrap().to_string();
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).unwrap(),
        import_path,
        "a function node type for a test",
    );
    descriptor.node_kind = NodeKind::Function {
        language: FunctionNodeLanguage::Python,
    };
    for input_port in input_ports {
        descriptor = descriptor.with_input(
            PortDescriptor::new(
                *input_port,
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            )
            .with_delivery_profile("newest"),
        );
    }
    for output_port in output_ports {
        descriptor = descriptor.with_output(PortDescriptor::new(
            *output_port,
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Channels),
        ));
    }
    descriptor
}

/// A graph resolving `lfo`, `remap` and `mix`, three function node types.
pub(crate) fn a_graph_resolving_three_function_node_types() -> Graph {
    a_graph_resolving_these_function_node_types([
        a_function_node_type("test_ops:lfo", &[], &["result"]),
        a_function_node_type("test_ops:remap", &["x"], &["result"]),
        a_function_node_type("test_ops:mix", &["a", "b"], &["result"]),
    ])
}

/// A graph resolving each of `function_node_types`.
pub(crate) fn a_graph_resolving_these_function_node_types(
    function_node_types: impl IntoIterator<Item = NodeDescriptor>,
) -> Graph {
    let node_types = NodeTypesOneStreamResolves::new();
    for descriptor in function_node_types {
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                descriptor,
                HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                    node_once_promoted_to_its_own_process: Box::new(|node| {
                        Err(Error::Configuration(format!(
                            "`{}` builds no node in this test",
                            node.id
                        )))
                    }),
                },
            )
            .unwrap();
    }
    crate::core::test_support::ensure_test_mocks_registered();
    Graph::new_resolving_node_types_through(Arc::new(node_types))
}

pub(crate) fn add_a_node(graph: &mut Graph, import_path: &str, config: JsonValue) -> NodeRunId {
    graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            NodeClassImportPath::new(import_path).unwrap(),
            config,
        ))
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone()
}

pub(crate) fn link(
    graph: &mut Graph,
    from: (&NodeRunId, &str),
    to: (&NodeRunId, &str),
) -> LinkUniqueId {
    graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(from.0, from.1),
            InputLinkPortRef::new(to.0, to.1),
        )
        .first()
        .unwrap()
        .id
        .clone()
}

pub(crate) fn a_helper_reader_before_its_setup(graph: &mut Graph) -> NodeRunId {
    a_helper_reader_before_its_setup_driven_in(
        graph,
        ProcessExecution::Continuous { interval_ms: 10 },
    )
}

pub(crate) fn a_helper_reader_before_its_setup_driven_in(
    graph: &mut Graph,
    far_side_process_execution: ProcessExecution,
) -> NodeRunId {
    let reader_id = add_a_node(
        graph,
        crate::core::test_support::MockInputOnlyNode::node_class_import_path().as_str(),
        JsonValue::Null,
    );
    graph
        .traversal_mut()
        .v(&reader_id)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(far_side_process_execution),
        )));
    reader_id
}

pub(crate) fn reader_setup_command_ports(graph: &Graph, reader_id: &NodeRunId) -> JsonValue {
    graph
        .traversal()
        .v(reader_id)
        .first()
        .unwrap()
        .get::<OutOfProcessLinkWiringComponent>()
        .unwrap()
        .0
        .as_setup_command_ports()
}

fn the_refusal_of_a_commit(
    graph: &Graph,
    nodes_to_add: &[NodeRunId],
    nodes_to_remove: &[NodeRunId],
    links_to_add: &[LinkUniqueId],
    links_to_remove: &[LinkUniqueId],
) -> Option<String> {
    refuse_an_edit_an_island_cannot_take(
        graph,
        nodes_to_add,
        nodes_to_remove,
        links_to_add,
        links_to_remove,
    )
    .err()
    .map(|refusal| refusal.to_string())
}

fn link_state_of(graph: &Graph, link_id: &LinkUniqueId) -> LinkState {
    graph
        .traversal()
        .e(link_id)
        .first()
        .unwrap()
        .get::<LinkStateComponent>()
        .unwrap()
        .0
}

/// A class node in a helper of its own, whose `out1` feeds `remap`'s `x`,
/// and `remap` feeding a helper reader's `in1`.
pub(crate) fn a_class_node_feeding_remap_into_a_helper_reader(
    graph: &mut Graph,
) -> (NodeRunId, NodeRunId, LinkUniqueId, LinkUniqueId) {
    let source = add_a_node(
        graph,
        crate::core::test_support::MockOutputOnlyNode::node_class_import_path().as_str(),
        JsonValue::Null,
    );
    graph
        .traversal_mut()
        .v(&source)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive),
        )));
    let remap = add_a_node(graph, "test_ops:remap", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(graph);
    let into_the_island = link(graph, (&source, "out1"), (&remap, "x"));
    let into_the_reader = link(graph, (&remap, "result"), (&reader, "in1"));
    (remap, reader, into_the_island, into_the_reader)
}

/// The inbound link a reader's input port is asked about by `inbound_link`
/// answers when one is recorded there by that name, and is refused when
/// none is.
fn the_reader_input_port_records_an_inbound_link_named(
    graph: &Graph,
    reader: &NodeRunId,
    input_port: &str,
    inbound_link: &str,
) -> bool {
    graph
        .traversal()
        .v(reader)
        .first()
        .unwrap()
        .upstream_stream_facts_by_inbound_link()
        .stream_facts_of_inbound_link(input_port, Some(inbound_link))
        .is_ok()
}

pub(crate) fn a_class_node_feeding(graph: &mut Graph) -> NodeRunId {
    let source = add_a_node(
        graph,
        crate::core::test_support::MockOutputOnlyNode::node_class_import_path().as_str(),
        JsonValue::Null,
    );
    graph
        .traversal_mut()
        .v(&source)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive),
        )));
    source
}
