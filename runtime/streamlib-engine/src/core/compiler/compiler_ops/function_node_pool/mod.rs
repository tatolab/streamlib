// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The stream's function node pool: one helper per stream that evaluates each
//! island no reader can evaluate itself and publishes its output on a channel.
//!
//! A link from such an island into its reader is carried as two: the island
//! into an input of the pool, and a channel from the matching output of the
//! pool into the reader's port.

use serde_json::Value as JsonValue;

mod carries_handed_to_the_running_pool;
pub(crate) use carries_handed_to_the_running_pool::{
    CarriesHandedToTheRunningFunctionNodePool, hand_the_running_function_node_pool_its_carries,
};

use super::function_node_island::runs_inside_the_read_of_each_node_it_feeds;
use crate::core::NodeRunId;
use crate::core::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind, PortDescriptor, PortFamily,
    PortFamilyDeclaration,
};
use crate::core::error::{Error, Result};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents, InputLinkPortRef, Link,
    LinkCarriedThroughTheFunctionNodePoolComponent, LinkUniqueId, OutOfProcessLinkWiringComponent,
    OutputLinkPortRef, PortInfo, the_family_a_link_out_of_an_output_carries,
};
use crate::core::nodes::NodeSpec;

pub(crate) const FUNCTION_NODE_POOL_NODE_TYPE: &str =
    "tatolab.runtime._function_node_pool:FunctionNodePool";

/// The input value of the pool naming every link it carries.
pub(crate) const FUNCTION_NODE_POOL_CARRIES_INPUT: &str = "carries";

/// Every carry `pool` names under [`FUNCTION_NODE_POOL_CARRIES_INPUT`]; none when it names none.
pub(crate) fn every_carry_of(pool: &crate::core::graph::GraphNode) -> Vec<JsonValue> {
    pool.input_values
        .get(FUNCTION_NODE_POOL_CARRIES_INPUT)
        .and_then(JsonValue::as_array)
        .cloned()
        .unwrap_or_default()
}

/// How often the pool evaluates every island it carries.
pub(crate) const FUNCTION_NODE_POOL_INTERVAL_MS: u32 = 10;

const FUNCTION_NODE_POOL_SHORT_NAME: &str = "FunctionNodePool";

/// The import path the stream's function node pool is registered under.
pub(crate) fn the_function_node_pool_type() -> Result<NodeClassImportPath> {
    NodeClassImportPath::new(FUNCTION_NODE_POOL_NODE_TYPE)
        .map_err(|refusal| Error::Configuration(refusal.to_string()))
}

/// Whether `node_type` is the stream's function node pool.
pub(crate) fn is_the_function_node_pool_type(node_type: &NodeClassImportPath) -> bool {
    node_type.as_str() == FUNCTION_NODE_POOL_NODE_TYPE
}

/// The pool's descriptor with no ports, which `node_ports` fills: the ports
/// each carried island adds to the pool's node.
pub(crate) fn the_function_node_pool_descriptor(
    node_ports: Option<(&[PortInfo], &[PortInfo])>,
) -> Result<NodeDescriptor> {
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(FUNCTION_NODE_POOL_SHORT_NAME)
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
/// lock, fed an island that runs a node as Python; or into a setting its
/// reader reads at setup, which the engine reads in the reader's place.
pub(crate) fn needs_the_function_node_pool(graph: &Graph, link_id: &LinkUniqueId) -> bool {
    graph.traversal().e(link_id).first().is_some_and(|link| {
        the_function_node_pool_evaluates_an_island_from_into(
            graph,
            link.source().node_run_id(),
            link.to_port(),
        )
    })
}

/// Whether a link from `source_id` into `to` carries an island the stream's function node pool
/// evaluates, as [`needs_the_function_node_pool`] judges a link already made.
pub(crate) fn the_function_node_pool_evaluates_an_island_from_into(
    graph: &Graph,
    source_id: &NodeRunId,
    to: &InputLinkPortRef,
) -> bool {
    let reader_id = to.node_run_id();
    if !runs_inside_the_read_of_each_node_it_feeds(graph, source_id)
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
    if reader.node_kind() == Some(NodeKind::Shader)
        || graph.is_a_setting_read_at_setup(reader_id, to.port_name())
    {
        return true;
    }
    let node_types = graph.node_types_this_stream_resolves();
    let runs_in_the_app_process = !reader.has::<OutOfProcessLinkWiringComponent>()
        && node_types.is_a_natively_compiled_node_type(&reader.node_type);
    let is_driven_manually = node_types.the_execution_a_described_class_declares(&reader.node_type)
        == Some(ProcessExecution::Manual);
    runs_in_the_app_process
        || (is_driven_manually && the_island_upstream_of_runs_a_node_as_python(graph, source_id))
}

/// Whether `island_output_node` or any function node it reads runs as Python.
fn the_island_upstream_of_runs_a_node_as_python(
    graph: &Graph,
    island_output_node: &NodeRunId,
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
                .descriptor(&node.node_type)
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
                .filter(|link| link.to_port().node_run_id() == &following)
                .map(|link| link.source().node_run_id().clone())
                .filter(|upstream| runs_inside_the_read_of_each_node_it_feeds(graph, upstream)),
        );
    }
    false
}

/// The stream's pool node, if it has one.
pub(crate) fn the_function_node_pool_of(graph: &Graph) -> Option<NodeRunId> {
    graph
        .traversal()
        .v(())
        .iter()
        .find(|node| is_the_function_node_pool_type(&node.node_type))
        .map(|node| node.id.clone())
}

/// Carry each link of `links_to_add` whose reader cannot evaluate its island through the
/// stream's function node pool, adding the pool when the stream has none; the carries handed
/// to a pool already running, which its helper takes without a restart.
pub(crate) fn carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
    graph: &mut Graph,
    nodes_to_add: &mut Vec<NodeRunId>,
    links_to_add: &mut Vec<LinkUniqueId>,
) -> Result<Option<CarriesHandedToTheRunningFunctionNodePool>> {
    let carried: Vec<LinkUniqueId> = links_to_add
        .iter()
        .filter(|link_id| needs_the_function_node_pool(graph, link_id))
        .cloned()
        .collect();
    if carried.is_empty() {
        return Ok(None);
    }
    let mut the_running_pool_before_the_carries = None;
    let pool_id = match the_function_node_pool_of(graph) {
        Some(running_pool) if !nodes_to_add.contains(&running_pool) => {
            the_running_pool_before_the_carries =
                graph.traversal().v(&running_pool).first().map(|pool| {
                    (
                        pool.ports.clone(),
                        pool.input_values
                            .get(FUNCTION_NODE_POOL_CARRIES_INPUT)
                            .cloned(),
                    )
                });
            running_pool
        }
        Some(pool_being_added) => pool_being_added,
        None => {
            let pool_id = graph
                .traversal_mut()
                .add_v(NodeSpec::new(
                    the_function_node_pool_type()?,
                    serde_json::json!({ FUNCTION_NODE_POOL_CARRIES_INPUT: [] }),
                ))?
                .first()
                .ok_or_else(|| {
                    Error::GraphError("the function node pool could not be added".to_string())
                })?
                .id
                .clone();
            nodes_to_add.push(pool_id.clone());
            pool_id
        }
    };
    let mut halves_through_the_pool = Vec::with_capacity(carried.len() * 2);
    for link_id in carried {
        let (into_the_pool, out_of_the_pool) =
            carry_one_link_through_the_pool(graph, &pool_id, &link_id)?;
        links_to_add.retain(|queued| queued != &link_id);
        links_to_add.push(into_the_pool.clone());
        links_to_add.push(out_of_the_pool.clone());
        halves_through_the_pool.extend([into_the_pool, out_of_the_pool]);
    }
    let Some((ports_before_the_carries, carries_held_before)) = the_running_pool_before_the_carries
    else {
        return Ok(None);
    };
    Ok(Some(CarriesHandedToTheRunningFunctionNodePool {
        pool: pool_id,
        ports_before_the_carries,
        carries_held_before,
        halves_through_the_pool,
    }))
}

/// Replace `link_id` with a link from its island into a new input of the pool
/// and a link from the matching new output of the pool into its reader. A
/// carry into a setting read at setup is written as the pool sets up too, so
/// its reader, set up after the pool, is built with what the island computes.
fn carry_one_link_through_the_pool(
    graph: &mut Graph,
    pool_id: &NodeRunId,
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
        .v(source.node_run_id())
        .first()
        .and_then(|node| {
            node.ports
                .outputs
                .iter()
                .find(|port| port.name == source.port_name())
        })
        .is_some_and(|port| port.family.family_follows_input_port.is_some());
    let feeds_a_setting_read_at_setup =
        graph.is_a_setting_read_at_setup(destination.node_run_id(), destination.port_name());
    let family =
        the_family_a_link_out_of_an_output_carries(graph, source.node_run_id(), source.port_name())
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
        .ok_or_else(|| Error::NodeNotFound(pool_id.to_string()))?;
    let carry_number = pool.ports.inputs.len();
    let pool_input_port = format!("island_{carry_number}");
    let pool_output_port = format!("island_{carry_number}_output");
    let description = format!(
        "carries `{}`.`{}` to `{}`.`{}`",
        source.node_run_id(),
        source.port_name(),
        destination.node_run_id(),
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
    let mut carry = serde_json::Map::from_iter([
        (
            "input_port".to_owned(),
            JsonValue::String(pool_input_port.clone()),
        ),
        (
            "output_port".to_owned(),
            JsonValue::String(pool_output_port.clone()),
        ),
    ]);
    if let Some(carry_family) = carry_family.as_object() {
        carry.extend(carry_family.clone());
    }
    if feeds_a_setting_read_at_setup {
        carry.insert("written_at_setup".to_owned(), JsonValue::Bool(true));
    }
    let mut every_carry = every_carry_of(pool);
    every_carry.push(JsonValue::Object(carry));
    pool.set_input_value(
        FUNCTION_NODE_POOL_CARRIES_INPUT,
        Some(JsonValue::Array(every_carry)),
    );

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
    link.insert_component_without_rendering_it(LinkCarriedThroughTheFunctionNodePoolComponent {
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
        if link.source().node_run_id() == &function_node_pool {
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
mod carries_handed_to_the_running_pool_tests;
#[cfg(test)]
pub(crate) mod tests;
