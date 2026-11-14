// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The stream's function node pool: one helper per stream that evaluates each
//! island no reader can evaluate itself and publishes its output on a channel.
//!
//! A link from such an island into its reader is carried as two: the island
//! into an input of the pool, and a channel from the matching output of the
//! pool into the reader's port.

use serde_json::Value as JsonValue;

use super::function_node_island::runs_inside_the_read_of_each_node_it_feeds;
use crate::core::ProcessorUniqueId;
use crate::core::descriptors::{
    NodeKind, PortDescriptor, PortFamily, PortFamilyDeclaration, ProcessorClassImportPath,
    ProcessorClassShortName, ProcessorDescriptor,
};
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, InputLinkPortRef, Link,
    LinkCarriedThroughTheFunctionNodePoolComponent, LinkUniqueId, OutOfProcessLinkWiringComponent,
    OutputLinkPortRef, PortInfo, the_family_a_link_out_of_an_output_carries,
};
use crate::core::processors::ProcessorSpec;

/// The import path the pool's helper loads, which builds the processor it
/// runs from the pool's config.
pub(crate) const FUNCTION_NODE_POOL_NODE_TYPE: &str =
    "tatolab.runtime._function_node_pool:FunctionNodePool";

/// How often the pool evaluates every island it carries.
pub(crate) const FUNCTION_NODE_POOL_INTERVAL_MS: u32 = 10;

/// The name the pool's processor class is known by in the catalog.
const FUNCTION_NODE_POOL_SHORT_NAME: &str = "FunctionNodePool";

/// The import path the stream's function node pool is registered under.
pub(crate) fn the_function_node_pool_type() -> Result<ProcessorClassImportPath> {
    ProcessorClassImportPath::new(FUNCTION_NODE_POOL_NODE_TYPE)
        .map_err(|refusal| Error::Configuration(refusal.to_string()))
}

/// Whether `node_type` is the stream's function node pool.
pub(crate) fn is_the_function_node_pool_type(node_type: &ProcessorClassImportPath) -> bool {
    node_type.as_str() == FUNCTION_NODE_POOL_NODE_TYPE
}

/// The pool's descriptor with no ports, which `node_ports` fills: the ports
/// each carried island adds to the pool's node.
pub(crate) fn the_function_node_pool_descriptor(
    node_ports: Option<(&[PortInfo], &[PortInfo])>,
) -> Result<ProcessorDescriptor> {
    let mut descriptor = ProcessorDescriptor::new(
        ProcessorClassShortName::new(FUNCTION_NODE_POOL_SHORT_NAME)
            .map_err(|refusal| Error::Configuration(refusal.to_string()))?,
        the_function_node_pool_type()?,
        "Evaluates every function node island no reader of it can evaluate, and publishes each \
         island's output on a channel",
    );
    descriptor.node_kind = NodeKind::Process;
    if let Some((inputs, outputs)) = node_ports {
        descriptor.inputs = inputs
            .iter()
            .map(|port| {
                PortDescriptor::new(&port.name, &port.description, true, port.family.clone())
                    .with_delivery_profile("newest")
            })
            .collect();
        descriptor.outputs = outputs
            .iter()
            .map(|port| {
                PortDescriptor::new(&port.name, &port.description, false, port.family.clone())
            })
            .collect();
    }
    Ok(descriptor)
}

/// Whether `link_id` carries an island into a reader that cannot evaluate it:
/// one that runs in the app process, which runs no node code, a shader node,
/// which the stream's shader evaluator runs with no node code, or a class
/// driven manually, whose native half reads without the Python interpreter's
/// lock, fed an island that runs a node as Python.
fn needs_the_function_node_pool(graph: &Graph, link_id: &LinkUniqueId) -> bool {
    let Some(link) = graph.traversal().e(link_id).first() else {
        return false;
    };
    let reader_id = link.to_port().processor_id();
    if !runs_inside_the_read_of_each_node_it_feeds(graph, link.source().processor_id())
        || runs_inside_the_read_of_each_node_it_feeds(graph, reader_id)
    {
        return false;
    }
    let Some(reader) = graph.traversal().v(reader_id).first() else {
        return false;
    };
    if matches!(reader.node_kind(), Some(NodeKind::Function { .. })) {
        return false;
    }
    if reader.node_kind() == Some(NodeKind::Shader) {
        return true;
    }
    let node_types = graph.node_types_this_stream_resolves();
    let runs_in_the_app_process = !reader.has::<OutOfProcessLinkWiringComponent>()
        && node_types.is_a_natively_compiled_node_type(&reader.processor_type);
    let is_driven_manually = node_types
        .the_execution_a_described_class_declares(&reader.processor_type)
        == Some(ProcessExecution::Manual);
    runs_in_the_app_process
        || (is_driven_manually
            && the_island_upstream_of_runs_a_node_as_python(graph, link.source().processor_id()))
}

/// Whether `island_output_node` or any function node it reads runs as Python.
fn the_island_upstream_of_runs_a_node_as_python(
    graph: &Graph,
    island_output_node: &ProcessorUniqueId,
) -> bool {
    let mut reached = std::collections::HashSet::new();
    let mut to_follow = vec![island_output_node.clone()];
    while let Some(following) = to_follow.pop() {
        if !reached.insert(following.clone()) {
            continue;
        }
        let runs_as_python = graph.traversal().v(&following).first().is_some_and(|node| {
            graph
                .node_types_this_stream_resolves()
                .descriptor(&node.processor_type)
                .is_some_and(|descriptor| descriptor.function_node_runs_as_python)
        });
        if runs_as_python {
            return true;
        }
        to_follow.extend(
            graph
                .traversal()
                .e(())
                .iter()
                .filter(|link| link.to_port().processor_id() == &following)
                .map(|link| link.source().processor_id().clone())
                .filter(|upstream| runs_inside_the_read_of_each_node_it_feeds(graph, upstream)),
        );
    }
    false
}

/// The stream's pool node, if it has one.
pub(crate) fn the_function_node_pool_of(graph: &Graph) -> Option<ProcessorUniqueId> {
    graph
        .traversal()
        .v(())
        .iter()
        .find(|node| is_the_function_node_pool_type(&node.processor_type))
        .map(|node| node.id.clone())
}

/// Carry each of `links_to_add` whose reader cannot evaluate its island
/// through the stream's pool, adding the pool to `processors_to_add` when the
/// stream has none yet and replacing each carried link with its two links.
///
/// Refused by name, changing nothing, when the pool already runs, since a
/// running pool is not re-islanded in this release.
pub(crate) fn carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
    graph: &mut Graph,
    processors_to_add: &mut Vec<ProcessorUniqueId>,
    links_to_add: &mut Vec<LinkUniqueId>,
) -> Result<()> {
    let carried: Vec<LinkUniqueId> = links_to_add
        .iter()
        .filter(|link_id| needs_the_function_node_pool(graph, link_id))
        .cloned()
        .collect();
    if carried.is_empty() {
        return Ok(());
    }
    let pool_id = match the_function_node_pool_of(graph) {
        Some(running_pool) if !processors_to_add.contains(&running_pool) => {
            return Err(Error::InvalidGraph(format!(
                "link `{}` feeds a function node island into a reader that cannot evaluate it, which \
                 the stream's function node pool evaluates, and the pool is already running; a \
                 running pool is not re-islanded in this release. To fix: stop the stream and \
                 start it with the edit",
                carried[0]
            )));
        }
        Some(pool_being_added) => pool_being_added,
        None => {
            let pool_id = graph
                .traversal_mut()
                .add_v(ProcessorSpec::new(
                    the_function_node_pool_type()?,
                    serde_json::json!({ "carries": [] }),
                ))?
                .first()
                .ok_or_else(|| {
                    Error::GraphError("the function node pool could not be added".to_string())
                })?
                .id
                .clone();
            processors_to_add.push(pool_id.clone());
            pool_id
        }
    };
    for link_id in carried {
        let (into_the_pool, out_of_the_pool) =
            carry_one_link_through_the_pool(graph, &pool_id, &link_id)?;
        links_to_add.retain(|queued| queued != &link_id);
        links_to_add.push(into_the_pool);
        links_to_add.push(out_of_the_pool);
    }
    Ok(())
}

/// Replace `link_id` with a link from its island into a new input of the pool
/// and a link from the matching new output of the pool into its reader.
fn carry_one_link_through_the_pool(
    graph: &mut Graph,
    pool_id: &ProcessorUniqueId,
    link_id: &LinkUniqueId,
) -> Result<(LinkUniqueId, LinkUniqueId)> {
    let (source, destination) = {
        let link = graph
            .traversal()
            .e(link_id)
            .first()
            .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
        (link.source().clone(), link.to_port().clone())
    };
    let hands_a_message_on_unread = graph
        .traversal()
        .v(source.processor_id())
        .first()
        .and_then(|node| {
            node.ports
                .outputs
                .iter()
                .find(|port| port.name == source.port_name())
        })
        .is_some_and(|port| port.family.family_follows_input_port.is_some());
    let family = the_family_a_link_out_of_an_output_carries(
        graph,
        source.processor_id(),
        source.port_name(),
    )
    .unwrap_or_else(|| PortFamilyDeclaration::of_family(PortFamily::Channels));
    let carry_family = if hands_a_message_on_unread {
        serde_json::json!({
            "family": "message",
            "message_family": family.family.wire_name(),
        })
    } else {
        serde_json::json!({ "family": family_name(family.family) })
    };
    let pool = graph
        .traversal_mut()
        .v(pool_id)
        .first_mut()
        .ok_or_else(|| Error::ProcessorNotFound(pool_id.to_string()))?;
    let carry_number = pool.ports.inputs.len();
    let pool_input_port = format!("island_{carry_number}");
    let pool_output_port = format!("island_{carry_number}_output");
    let description = format!(
        "carries `{}`.`{}` to `{}`.`{}`",
        source.processor_id(),
        source.port_name(),
        destination.processor_id(),
        destination.port_name()
    );
    let input_descriptor = PortDescriptor::new(
        &pool_input_port,
        &description,
        true,
        PortFamilyDeclaration::of_family(family.family),
    )
    .with_delivery_profile("newest");
    let output_descriptor =
        PortDescriptor::new(&pool_output_port, &description, false, family.clone());
    pool.ports.inputs.push(PortInfo::from(&input_descriptor));
    pool.ports.outputs.push(PortInfo::from(&output_descriptor));
    let mut config = pool
        .config
        .clone()
        .unwrap_or_else(|| serde_json::json!({ "carries": [] }));
    if let Some(carries) = config.get_mut("carries").and_then(JsonValue::as_array_mut) {
        let mut carry = serde_json::json!({
            "input_port": pool_input_port,
            "output_port": pool_output_port,
        });
        if let (Some(carry), Some(carry_family)) = (carry.as_object_mut(), carry_family.as_object())
        {
            carry.extend(carry_family.clone());
        }
        carries.push(carry);
    }
    pool.set_config(config);

    graph.traversal_mut().e(link_id).drop();
    let into_the_pool = add_a_link_carrying(
        graph,
        source,
        InputLinkPortRef::new(pool_id, &pool_input_port),
        link_id,
    )?;
    let out_of_the_pool = add_a_link_carrying(
        graph,
        OutputLinkPortRef::new(pool_id, &pool_output_port),
        destination,
        link_id,
    )?;
    Ok((into_the_pool, out_of_the_pool))
}

fn add_a_link_carrying(
    graph: &mut Graph,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    carried_link_id: &LinkUniqueId,
) -> Result<LinkUniqueId> {
    let link = graph
        .traversal_mut()
        .add_e(from.clone(), to.clone())
        .first_mut()
        .ok_or_else(|| {
            Error::InvalidLink(format!(
                "`{from}` → `{to}` could not be added to carry `{carried_link_id}` through the \
             function node pool"
            ))
        })?;
    link.insert(LinkCarriedThroughTheFunctionNodePoolComponent {
        carried_link_id: carried_link_id.clone(),
    });
    Ok(link.id.clone())
}

/// The links `link_id` was carried as through the pool; `link_id` alone when
/// the graph still holds it.
pub(crate) fn the_links_a_link_is_wired_as(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Vec<LinkUniqueId> {
    if graph.traversal().e(link_id).first().is_some() {
        return vec![link_id.clone()];
    }
    graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| {
            link.get::<LinkCarriedThroughTheFunctionNodePoolComponent>()
                .is_some_and(|carried| &carried.carried_link_id == link_id)
        })
        .map(|link| link.id.clone())
        .collect()
}

/// One link the graph carries through the function node pool, beside the two
/// links it is wired as.
pub(crate) struct LinkCarriedThroughTheFunctionNodePool<'graph> {
    /// The link as the graph was given it.
    pub(crate) carried_link_id: LinkUniqueId,
    /// The link from the island into an input of the pool.
    pub(crate) into_the_function_node_pool: &'graph Link,
    /// The link from the matching output of the pool into the reader.
    pub(crate) out_of_the_function_node_pool: &'graph Link,
}

/// Each link `graph` carries through its function node pool, in the order
/// the graph holds the first of its two links; one missing either link is
/// left out.
pub(crate) fn the_links_carried_through_the_function_node_pool_of(
    graph: &Graph,
) -> Vec<LinkCarriedThroughTheFunctionNodePool<'_>> {
    let Some(function_node_pool) = the_function_node_pool_of(graph) else {
        return Vec::new();
    };
    let mut halves_of_each_carried_link: Vec<(LinkUniqueId, Option<&Link>, Option<&Link>)> =
        Vec::new();
    for link in graph.traversal().e(()).iter() {
        let Some(carried) = link.get::<LinkCarriedThroughTheFunctionNodePoolComponent>() else {
            continue;
        };
        let index = match halves_of_each_carried_link
            .iter()
            .position(|(held, _, _)| held == &carried.carried_link_id)
        {
            Some(index) => index,
            None => {
                halves_of_each_carried_link.push((carried.carried_link_id.clone(), None, None));
                halves_of_each_carried_link.len() - 1
            }
        };
        let (_, into_the_pool, out_of_the_pool) = &mut halves_of_each_carried_link[index];
        if link.source().processor_id() == &function_node_pool {
            *out_of_the_pool = Some(link);
        } else {
            *into_the_pool = Some(link);
        }
    }
    halves_of_each_carried_link
        .into_iter()
        .filter_map(|(carried_link_id, into_the_pool, out_of_the_pool)| {
            Some(LinkCarriedThroughTheFunctionNodePool {
                carried_link_id,
                into_the_function_node_pool: into_the_pool?,
                out_of_the_function_node_pool: out_of_the_pool?,
            })
        })
        .collect()
}

fn family_name(family: PortFamily) -> &'static str {
    match family {
        PortFamily::Records => "records",
        _ => "channels",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::compiler::compiler_ops::function_node_island::tests::{
        a_class_node_feeding, a_function_node_type, a_graph_resolving_these_function_node_types,
        a_graph_resolving_three_function_node_types, a_helper_reader_before_its_setup, add_a_node,
        link,
    };
    use crate::core::compiler::compiler_ops::function_node_island::{
        HowALinkIsWired, how_a_link_is_wired,
        wire_a_link_from_a_function_node_island_into_its_reader,
    };
    use crate::core::compiler::compiler_ops::shader_node_island::the_host_input_port_a_fused_shader_node_input_is_read_at;
    use crate::core::compiler::compiler_ops::shader_node_island_program::tests::shader_program;
    use crate::core::processors::HowANodeOfADescribedTypeRuns;
    use crate::core::test_support::MockInputOnlyProcessor;

    fn a_graph_resolving_the_pool_and_three_function_node_types() -> Graph {
        let graph = a_graph_resolving_three_function_node_types();
        graph
            .node_types_this_stream_resolves()
            .register_a_type_described_in_this_streams_processor_interpreter(
                the_function_node_pool_descriptor(None).unwrap(),
                HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no processor in this test",
                        node.id
                    )))
                })),
            )
            .unwrap();
        graph
    }

    fn a_reader_in_the_app_process(graph: &mut Graph) -> ProcessorUniqueId {
        add_a_node(
            graph,
            MockInputOnlyProcessor::processor_class_import_path().as_str(),
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
    -> (Graph, ProcessorUniqueId, ProcessorUniqueId, LinkUniqueId) {
        let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_reader_in_the_app_process(&mut graph);
        let lfo_into_the_reader = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
        let mut processors_to_add = vec![lfo.clone(), reader.clone()];
        let mut links_to_add = vec![lfo_into_the_reader.clone()];
        carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
            &mut graph,
            &mut processors_to_add,
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
        let name_of = |processor_id: &ProcessorUniqueId| {
            graph
                .traversal()
                .v(processor_id)
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
                .all(|node| !is_the_function_node_pool_type(&node.processor_type)),
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
            carried.components["carried_through_the_function_node_pool"],
            serde_json::json!({"input_port": "island_0", "output_port": "island_0_output"})
        );
    }

    #[test]
    fn a_graph_rendered_with_a_pool_loads_as_the_graph_it_was_given() {
        let (graph, ..) = a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
        let rendered = serde_json::to_string(&graph.to_graph_response("rig".to_string())).unwrap();

        let loaded = crate::core::graph_snapshot::GraphSnapshot::from_json_str(&rendered)
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
        let mut processors_to_add = vec![lfo.clone(), remap.clone(), first_reader, second_reader];
        let mut links_to_add = vec![
            lfo_into_remap.clone(),
            remap_into_the_first.clone(),
            lfo_into_the_second.clone(),
        ];

        carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
            &mut graph,
            &mut processors_to_add,
            &mut links_to_add,
        )
        .unwrap();

        let pool = the_function_node_pool_of(&graph).expect("the stream has a pool");
        assert_eq!(
            processors_to_add.iter().filter(|id| **id == pool).count(),
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
            if link.to_port().processor_id() == &pool {
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
            pool_node.config.as_ref().unwrap()["carries"]
                .as_array()
                .map(Vec::len),
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
        let import_path = ProcessorClassImportPath::new("test_shaders:tint").unwrap();
        let mut descriptor = ProcessorDescriptor::new(
            ProcessorClassShortName::new("tint").unwrap(),
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
            .register_a_type_described_in_this_streams_processor_interpreter(
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
                            "`{}` builds no processor in this test",
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
        let remap_into_the_fused_tint =
            link(&mut graph, (&remap, "result"), (&fused_tint, "amount"));
        let fused_tint_into_the_host =
            link(&mut graph, (&fused_tint, "result"), (&host_tint, "image"));
        let lfo_into_the_host = link(&mut graph, (&lfo, "result"), (&host_tint, "amount"));
        let mut processors_to_add = vec![lfo, remap, fused_tint.clone(), host_tint.clone()];
        let mut links_to_add = vec![
            lfo_into_remap,
            remap_into_the_fused_tint.clone(),
            fused_tint_into_the_host,
            lfo_into_the_host.clone(),
        ];

        carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
            &mut graph,
            &mut processors_to_add,
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
                        .is_some_and(|link| link.source().processor_id() == &pool)
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
        let mut processors_to_add = vec![lfo, reader];
        let mut links_to_add = vec![lfo_into_the_reader.clone()];

        carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
            &mut graph,
            &mut processors_to_add,
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
            HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(|node| {
                Err(Error::Configuration(format!(
                    "`{}` builds no processor in this test",
                    node.id
                )))
            }))
        };
        node_types
            .register_a_type_described_in_this_streams_processor_interpreter(
                the_function_node_pool_descriptor(None).unwrap(),
                builds_nothing(),
            )
            .unwrap();
        let mut manual_class = ProcessorDescriptor::new(
            ProcessorClassShortName::new("ReadsThroughItsNativeHalf").unwrap(),
            ProcessorClassImportPath::new(A_CLASS_WITH_A_NATIVE_HALF).unwrap(),
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
            .register_a_type_described_in_this_streams_processor_interpreter(
                manual_class,
                builds_nothing(),
            )
            .unwrap();
        node_types.record_the_execution_a_described_class_declares(
            ProcessorClassImportPath::new(A_CLASS_WITH_A_NATIVE_HALF).unwrap(),
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
        let compiled_into_the_reader =
            link(&mut graph, (&lfo, "result"), (&reader, "from_compiled"));
        let mut processors_to_add = vec![tally, lfo, reader];
        let mut links_to_add = vec![
            python_into_the_reader.clone(),
            compiled_into_the_reader.clone(),
        ];

        carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
            &mut graph,
            &mut processors_to_add,
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
    fn a_gate_type() -> ProcessorDescriptor {
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
            .register_a_type_described_in_this_streams_processor_interpreter(
                the_function_node_pool_descriptor(None).unwrap(),
                HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(Box::new(|node| {
                    Err(Error::Configuration(format!(
                        "`{}` builds no processor in this test",
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
        let mut processors_to_add = vec![class_node.clone(), gate, reader];
        let mut links_to_add = vec![into_the_gate.clone(), gate_into_the_reader.clone()];

        carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
            &mut graph,
            &mut processors_to_add,
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
            pool_node.config.as_ref().unwrap()["carries"][0],
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
                    .processor_id()
                    == &pool
            })
            .unwrap();
        graph
            .traversal_mut()
            .v(&pool)
            .first_mut()
            .unwrap()
            .insert_component_without_rendering_it(
            OutOfProcessLinkWiringComponent(std::sync::Arc::new(
                crate::core::processors::OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                    ProcessExecution::Continuous { interval_ms: 10 },
                ),
            )),
        );

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
        assert_eq!(read_at.processor_id(), &pool);
    }

    #[test]
    fn a_running_pool_refuses_a_new_island_by_name() {
        let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
        let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
        let reader = a_reader_in_the_app_process(&mut graph);
        let first = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
        let mut processors_to_add = vec![lfo.clone(), reader];
        let mut links_to_add = vec![first];
        carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
            &mut graph,
            &mut processors_to_add,
            &mut links_to_add,
        )
        .unwrap();

        let late_reader = a_reader_in_the_app_process(&mut graph);
        let late = link(&mut graph, (&lfo, "result"), (&late_reader, "in1"));
        let refusal = carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
            &mut graph,
            &mut vec![late_reader],
            &mut vec![late.clone()],
        )
        .unwrap_err()
        .to_string();
        assert!(refusal.contains("already running"), "{refusal}");
        assert!(graph.traversal().e(&late).first().is_some());
    }
}
