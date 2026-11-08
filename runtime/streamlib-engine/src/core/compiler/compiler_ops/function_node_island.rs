// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Evaluation islands: each connected run of function nodes, run inside the
//! read of the node it feeds.
//!
//! A link between two function nodes is a function call, and one from a
//! function node into a helper's input port carries the island upstream of it
//! to that helper in its setup command. Neither opens a channel, and no
//! function node is prepared or spawned.

use std::collections::HashSet;

use serde_json::Value as JsonValue;

use crate::core::ProcessorUniqueId;
use crate::core::descriptors::NodeKind;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, Link, LinkState, LinkStateComponent,
    LinkUniqueId, OutOfProcessLinkWiringComponent,
};

/// How the compiler wires one link, by the kinds of its two ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HowALinkIsWired {
    /// Through an iceoryx2 channel: neither end is a function node.
    ThroughAChannel,
    /// As a function call between two function nodes of one island.
    AsACallInsideAFunctionNodeIsland,
    /// As the island upstream of it, carried to the helper the link feeds.
    AsTheIslandItsReaderEvaluatesAtEachRead,
}

/// Whether `node_id` is a function node.
pub(crate) fn is_a_function_node(graph: &Graph, node_id: &ProcessorUniqueId) -> bool {
    graph
        .traversal()
        .v(node_id)
        .first()
        .and_then(|node| node.node_kind())
        .is_some_and(|node_kind| matches!(node_kind, NodeKind::Function { .. }))
}

/// How `link_id` is wired, refusing a link into a function node from a node
/// that is not one.
pub(crate) fn how_a_link_is_wired(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<HowALinkIsWired> {
    let link = link_in(graph, link_id)?;
    let source_id = link.source().processor_id();
    let destination_id = link.to_port().processor_id();
    match (
        is_a_function_node(graph, source_id),
        is_a_function_node(graph, destination_id),
    ) {
        (false, false) => Ok(HowALinkIsWired::ThroughAChannel),
        (true, true) => Ok(HowALinkIsWired::AsACallInsideAFunctionNodeIsland),
        (true, false) => Ok(HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead),
        (false, true) => Err(Error::InvalidLink(format!(
            "link `{link_id}` feeds function node `{destination_id}` from `{source_id}`, which \
             is not a function node; an island's inputs come from other function nodes or \
             from its pins in this release"
        ))),
    }
}

/// Wire `link_id`, from a function node into a helper's input port: record the
/// island upstream of it on the helper's setup command and mark it wired.
///
/// Refused by name when the destination runs in the app process, which runs no
/// node code, and when its setup command has already gone out, since a live
/// edit does not re-island a running reader yet.
pub(crate) fn wire_a_link_from_a_function_node_island_into_its_reader(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let island = the_island_feeding_a_link(graph, link_id)?;
    let reader_id = link_in(graph, link_id)?.to_port().processor_id().clone();
    let reader_link_wiring = graph
        .traversal()
        .v(&reader_id)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
        .map(|component| std::sync::Arc::clone(&component.0))
        .ok_or_else(|| {
            Error::InvalidLink(format!(
                "link `{link_id}` feeds a function node island into `{reader_id}`, which runs in \
                 the app process; a function node runs inside the read of a node in a helper \
                 process, and the app process runs no node code"
            ))
        })?;
    reader_link_wiring.record_a_function_node_island_on_the_setup_command(island)?;
    mark_a_link(graph, link_id, LinkState::Wired)
}

/// Unwire `link_id`, from a function node into its reader: forget the island
/// it carries on the reader's setup command and mark it disconnected.
pub(crate) fn unwire_a_link_from_a_function_node_island_into_its_reader(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let reader_id = link_in(graph, link_id)?.to_port().processor_id().clone();
    if let Some(reader_link_wiring) = graph
        .traversal()
        .v(&reader_id)
        .first()
        .and_then(|node| node.get::<OutOfProcessLinkWiringComponent>())
    {
        reader_link_wiring
            .0
            .forget_a_function_node_island(link_id.as_str());
    }
    mark_a_link(graph, link_id, LinkState::Disconnected)
}

/// Mark `link_id`, a call between two function nodes, wired.
pub(crate) fn wire_a_call_inside_a_function_node_island(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
) -> Result<()> {
    mark_a_link(graph, link_id, LinkState::Wired)
}

fn mark_a_link(graph: &mut Graph, link_id: &LinkUniqueId, link_state: LinkState) -> Result<()> {
    graph
        .traversal_mut()
        .e(link_id)
        .first_mut()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?
        .insert(LinkStateComponent(link_state));
    Ok(())
}

/// The island upstream of `link_id` as its reader's setup command carries it:
/// the reader's input port, the island output feeding it, and every function
/// node it calls, each after the nodes it reads.
///
/// Wire contract with the helper bootstrap: `{"input_port", "link_id",
/// "output": {"node", "port"}, "nodes": [{"node", "import_path", "config",
/// "links_into": [{"port", "from": {"node", "port"}}]}]}`.
pub(crate) fn the_island_feeding_a_link(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<JsonValue> {
    let link = link_in(graph, link_id)?;
    let island_output_node_id = link.source().processor_id().clone();
    let mut nodes_each_after_what_it_reads = Vec::new();
    let mut placed = HashSet::new();
    let mut on_the_path_from_the_output = Vec::new();
    place_a_function_node_after_what_it_reads(
        graph,
        &island_output_node_id,
        &mut placed,
        &mut on_the_path_from_the_output,
        &mut nodes_each_after_what_it_reads,
    )?;
    Ok(serde_json::json!({
        "input_port": link.to_port().port_name(),
        "link_id": link_id.to_string(),
        "output": {
            "node": island_output_node_id.to_string(),
            "port": link.source().port_name(),
        },
        "nodes": nodes_each_after_what_it_reads,
    }))
}

fn place_a_function_node_after_what_it_reads(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
    placed: &mut HashSet<ProcessorUniqueId>,
    on_the_path_from_the_output: &mut Vec<ProcessorUniqueId>,
    nodes_each_after_what_it_reads: &mut Vec<JsonValue>,
) -> Result<()> {
    if placed.contains(node_id) {
        return Ok(());
    }
    if let Some(cycle_start) = on_the_path_from_the_output
        .iter()
        .position(|on_the_path| on_the_path == node_id)
    {
        let cycle: Vec<String> = on_the_path_from_the_output[cycle_start..]
            .iter()
            .chain(std::iter::once(node_id))
            .map(ToString::to_string)
            .collect();
        return Err(Error::InvalidGraph(format!(
            "function nodes {} read each other in a cycle; an island evaluates each node after \
             what it reads, so a cycle needs a delay node, which reads the last evaluation",
            cycle.join(" → ")
        )));
    }
    let node = graph
        .traversal()
        .v(node_id)
        .first()
        .ok_or_else(|| Error::ProcessorNotFound(node_id.to_string()))?;
    let import_path = node.processor_type.to_string();
    let config = node.config.clone().unwrap_or(JsonValue::Null);

    on_the_path_from_the_output.push(node_id.clone());
    let links_into_this_node: Vec<&Link> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| link.to_port().processor_id() == node_id)
        .collect();
    let mut links_into: Vec<JsonValue> = Vec::with_capacity(links_into_this_node.len());
    for link_into in links_into_this_node {
        let upstream_id = link_into.source().processor_id();
        if !is_a_function_node(graph, upstream_id) {
            return Err(Error::InvalidLink(format!(
                "function node `{node_id}` reads `{upstream_id}`, which is not a function \
                 node; an island's inputs come from other function nodes or from its pins in \
                 this release"
            )));
        }
        place_a_function_node_after_what_it_reads(
            graph,
            upstream_id,
            placed,
            on_the_path_from_the_output,
            nodes_each_after_what_it_reads,
        )?;
        links_into.push(serde_json::json!({
            "port": link_into.to_port().port_name(),
            "from": {
                "node": upstream_id.to_string(),
                "port": link_into.source().port_name(),
            },
        }));
    }
    on_the_path_from_the_output.pop();

    placed.insert(node_id.clone());
    nodes_each_after_what_it_reads.push(serde_json::json!({
        "node": node_id.to_string(),
        "import_path": import_path,
        "config": config,
        "links_into": links_into,
    }));
    Ok(())
}

/// The function nodes among `node_ids`, which are never prepared or spawned.
pub(crate) fn the_function_nodes_among<'a>(
    graph: &Graph,
    node_ids: impl IntoIterator<Item = &'a ProcessorUniqueId>,
) -> HashSet<ProcessorUniqueId> {
    node_ids
        .into_iter()
        .filter(|node_id| is_a_function_node(graph, node_id))
        .cloned()
        .collect()
}

fn link_in<'graph>(graph: &'graph Graph, link_id: &LinkUniqueId) -> Result<&'graph Link> {
    graph
        .traversal()
        .e(link_id)
        .first()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::descriptors::{
        FunctionNodeLanguage, PortDescriptor, PortFamily, PortFamilyDeclaration,
        ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
    };
    use crate::core::execution::ProcessExecution;
    use crate::core::graph::{InputLinkPortRef, OutputLinkPortRef};
    use crate::core::processors::{
        HowANodeOfADescribedTypeRuns, NodeTypesOneStreamResolves, OutOfProcessLinkWiringEnvelope,
        ProcessorSpec,
    };

    fn a_function_node_type(
        import_path: &str,
        input_ports: &[&str],
        output_ports: &[&str],
    ) -> ProcessorDescriptor {
        let import_path = ProcessorClassImportPath::new(import_path).unwrap();
        let short_name = import_path.as_str().rsplit(':').next().unwrap().to_string();
        let mut descriptor = ProcessorDescriptor::new(
            ProcessorClassShortName::new(short_name).unwrap(),
            import_path,
            "a function node type for a test",
        );
        descriptor.node_kind = NodeKind::Function {
            language: FunctionNodeLanguage::Python,
        };
        for input_port in input_ports {
            descriptor = descriptor.with_input(PortDescriptor::new(
                *input_port,
                "",
                true,
                PortFamilyDeclaration::of_family(PortFamily::Channels),
            ));
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
    fn a_graph_resolving_three_function_node_types() -> Graph {
        let node_types = NodeTypesOneStreamResolves::new();
        for descriptor in [
            a_function_node_type("test_ops:lfo", &[], &["result"]),
            a_function_node_type("test_ops:remap", &["x"], &["result"]),
            a_function_node_type("test_ops:mix", &["a", "b"], &["result"]),
        ] {
            node_types
                .register_a_type_described_in_this_streams_processor_interpreter(
                    descriptor,
                    HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds,
                )
                .unwrap();
        }
        crate::core::test_support::ensure_test_mocks_registered();
        Graph::new_resolving_node_types_through(Arc::new(node_types))
    }

    fn add_a_node(graph: &mut Graph, import_path: &str, config: JsonValue) -> ProcessorUniqueId {
        graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(
                ProcessorClassImportPath::new(import_path).unwrap(),
                config,
            ))
            .unwrap()
            .first()
            .unwrap()
            .id
            .clone()
    }

    fn link(
        graph: &mut Graph,
        from: (&ProcessorUniqueId, &str),
        to: (&ProcessorUniqueId, &str),
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

    fn a_helper_reader_before_its_setup(graph: &mut Graph) -> ProcessorUniqueId {
        let reader_id = add_a_node(
            graph,
            crate::core::test_support::MockInputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        graph
            .traversal_mut()
            .v(&reader_id)
            .first_mut()
            .unwrap()
            .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
                OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                    ProcessExecution::Reactive,
                ),
            )));
        reader_id
    }

    fn reader_setup_command_ports(graph: &Graph, reader_id: &ProcessorUniqueId) -> JsonValue {
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

    #[test]
    fn each_link_is_wired_by_the_kinds_of_its_two_ends() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let source = add_a_node(
            &mut graph,
            crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        let call = link(&mut graph, (&lfo, "result"), (&remap, "x"));
        let into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
        assert_eq!(
            how_a_link_is_wired(&graph, &call).unwrap(),
            HowALinkIsWired::AsACallInsideAFunctionNodeIsland
        );
        assert_eq!(
            how_a_link_is_wired(&graph, &into_the_reader).unwrap(),
            HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead
        );
        let into_a_function_node_from_a_process_node =
            link(&mut graph, (&source, "out1"), (&remap, "x"));
        let refusal = how_a_link_is_wired(&graph, &into_a_function_node_from_a_process_node)
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("which is not a function node"),
            "{refusal}"
        );
    }

    #[test]
    fn the_island_lists_each_node_after_what_it_reads_once_with_its_config() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(
            &mut graph,
            "test_ops:lfo",
            serde_json::json!({"rate_hz": 2.0}),
        );
        let low = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let high = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&lfo, "result"), (&low, "x"));
        link(&mut graph, (&lfo, "result"), (&high, "x"));
        link(&mut graph, (&low, "result"), (&mix, "a"));
        link(&mut graph, (&high, "result"), (&mix, "b"));
        let into_the_reader = link(&mut graph, (&mix, "result"), (&reader, "in1"));

        let island = the_island_feeding_a_link(&graph, &into_the_reader).unwrap();
        assert_eq!(island["input_port"], "in1");
        assert_eq!(island["output"]["node"], mix.to_string());
        let nodes_in_order: Vec<&str> = island["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["node"].as_str().unwrap())
            .collect();
        assert_eq!(nodes_in_order.len(), 4, "{nodes_in_order:?}");
        let position = |node_id: &ProcessorUniqueId| {
            nodes_in_order
                .iter()
                .position(|listed| *listed == node_id.as_str())
                .unwrap()
        };
        assert!(position(&lfo) < position(&low) && position(&lfo) < position(&high));
        assert!(position(&low) < position(&mix) && position(&high) < position(&mix));
        assert_eq!(island["nodes"][0]["config"]["rate_hz"], 2.0);
        assert_eq!(island["nodes"][0]["import_path"], "test_ops:lfo");
    }

    #[test]
    fn a_cycle_of_function_nodes_is_refused_naming_it() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let first = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
        let second = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        link(&mut graph, (&first, "result"), (&second, "a"));
        link(&mut graph, (&second, "result"), (&first, "x"));
        let into_the_reader = link(&mut graph, (&second, "result"), (&reader, "in1"));
        let refusal = the_island_feeding_a_link(&graph, &into_the_reader)
            .unwrap_err()
            .to_string();
        assert!(refusal.contains("in a cycle"), "{refusal}");
    }

    #[test]
    fn a_link_into_a_helper_carries_its_island_on_the_setup_command_and_is_wired() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));

        wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
            .unwrap();

        let islands = reader_setup_command_ports(&graph, &reader)["islands"].clone();
        assert_eq!(islands.as_array().unwrap().len(), 1);
        assert_eq!(islands[0]["link_id"], into_the_reader.to_string());
        assert!(matches!(
            graph
                .traversal()
                .e(&into_the_reader)
                .first()
                .unwrap()
                .get::<LinkStateComponent>()
                .map(|state| state.0),
            Some(LinkState::Wired)
        ));

        unwire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
            .unwrap();
        assert_eq!(
            reader_setup_command_ports(&graph, &reader)["islands"],
            serde_json::json!([])
        );
    }

    #[test]
    fn an_island_into_a_node_in_the_app_process_is_refused_by_name() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let native_reader = add_a_node(
            &mut graph,
            crate::core::test_support::MockInputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&native_reader, "in1"));
        let refusal =
            wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
                .unwrap_err()
                .to_string();
        assert!(refusal.contains("runs in the app process"), "{refusal}");
    }

    #[test]
    fn an_island_into_a_helper_past_its_setup_is_refused_as_a_live_edit() {
        let mut graph = a_graph_resolving_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_helper_reader_before_its_setup(&mut graph);
        graph
            .traversal()
            .v(&reader)
            .first()
            .unwrap()
            .get::<OutOfProcessLinkWiringComponent>()
            .unwrap()
            .0
            .hold_every_later_link_until_the_setup_command_goes_out();
        let into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
        let refusal =
            wire_a_link_from_a_function_node_island_into_its_reader(&mut graph, &into_the_reader)
                .unwrap_err()
                .to_string();
        assert!(refusal.contains("not re-islanded"), "{refusal}");
    }
}
