// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The carries a commit hands the stream's function node pool while it runs: its helper takes
//! them before the links into and out of their ports are wired, and a refusal leaves the pool
//! as it was.

use parking_lot::RwLock;
use serde_json::Value as JsonValue;

use super::{FUNCTION_NODE_POOL_CARRIES_INPUT, every_carry_of, the_function_node_pool_descriptor};
use crate::core::compiler::compiler_ops::node_interpreter_spawn_host::NodeInterpreterSpawnHost;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodePorts, GraphNodeWithComponents, LinkUniqueId, NodeInstanceComponent, NodeRunId,
};

/// The carries a commit made through the stream's running pool, beside what the pool held
/// before them, which a refusal puts back.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CarriesHandedToTheRunningFunctionNodePool {
    pub(crate) pool: NodeRunId,
    pub(crate) ports_before_the_carries: GraphNodePorts,
    /// The carries the pool named before them; `None` when it named none.
    pub(crate) carries_held_before: Option<JsonValue>,
    /// The links into and out of the pool each carried link is wired as.
    pub(crate) halves_through_the_pool: Vec<LinkUniqueId>,
}

/// Hand the running pool's helper every carry the pool now names, so it reads and writes the
/// new ones from its next `process` on, before the commit wires their links; refused when the
/// helper refuses them, with the pool's ports and config put back and the links through the
/// new ports taken out, so the graph holds nothing the refusal says it took back.
pub(crate) fn hand_the_running_function_node_pool_its_carries(
    graph: &RwLock<Graph>,
    carries: &CarriesHandedToTheRunningFunctionNodePool,
) -> Result<()> {
    let handed = the_pool_takes_its_carries(graph, &carries.pool);
    if handed.is_err() {
        put_the_pool_back_as_it_was_before_the_carries(&mut graph.write(), carries);
    }
    handed
}

fn the_pool_takes_its_carries(graph: &RwLock<Graph>, pool: &NodeRunId) -> Result<()> {
    let (instance, descriptor, every_carry) = {
        let graph = graph.read();
        let node = graph
            .traversal()
            .v(pool)
            .first()
            .ok_or_else(|| Error::NodeNotFound(pool.to_string()))?;
        let instance = node
            .get::<NodeInstanceComponent>()
            .map(|instance| instance.0.clone())
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "the running function node pool `{pool}` has no instance"
                ))
            })?;
        let descriptor =
            the_function_node_pool_descriptor(Some((&node.ports.inputs, &node.ports.outputs)))?;
        (instance, descriptor, JsonValue::Array(every_carry_of(node)))
    };
    let mut instance = instance.lock();
    let host = instance
        .as_any_mut()
        .downcast_mut::<NodeInterpreterSpawnHost>()
        .ok_or_else(|| {
            Error::Runtime(format!(
                "the running function node pool `{pool}` does not run in a helper process"
            ))
        })?;
    host.hand_the_running_function_node_pool_its_carries(descriptor, every_carry)
}

fn put_the_pool_back_as_it_was_before_the_carries(
    graph: &mut Graph,
    carries: &CarriesHandedToTheRunningFunctionNodePool,
) {
    for half in &carries.halves_through_the_pool {
        graph.traversal_mut().e(half).drop();
    }
    if let Some(pool) = graph.traversal_mut().v(&carries.pool).first_mut() {
        pool.ports = carries.ports_before_the_carries.clone();
        pool.set_input_value(
            FUNCTION_NODE_POOL_CARRIES_INPUT,
            carries.carries_held_before.clone(),
        );
    }
}
