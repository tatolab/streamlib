// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified Graph combining topology with ECS component storage.
//!
//! Graph is the public API for the processor pipeline graph. It internally manages:
//! - [`InternalProcessorLinkGraph`] - the petgraph-based processor/link topology
//! - [`InternalProcessorLinkGraphEcsExtension`] - hecs ECS world for runtime components
//!
//! Users interact only with this `Graph` type - the internal stores are implementation details.

use std::sync::Arc;
use std::time::Instant;

use hecs::{Component, Entity};
use parking_lot::RwLock;
use serde_json::Value as JsonValue;

use crate::core::error::Result;
use crate::core::graph::components::{
    EcsComponentJson, LinkStateComponent, ProcessorInstance, ProcessorMetrics, StateComponent,
};
use crate::core::graph::internal::InternalProcessorLinkGraphEcsExtension;
use crate::core::graph::link::LinkState;
use crate::core::graph::link_port_ref::IntoLinkPortRef;
use crate::core::graph::{
    GraphChecksum, InternalProcessorLinkGraph, Link, ProcessorId, ProcessorNode,
};
use crate::core::links::graph::{LinkInstanceComponent, LinkTypeInfoComponent};
use crate::core::links::LinkId;
use crate::core::processors::Processor;

/// Graph state.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GraphState {
    #[default]
    Idle,
    Running,
    Paused,
    Stopping,
}

/// Unified graph with topology and ECS components.
///
/// Graph is the public API combining:
/// - [`InternalProcessorLinkGraph`] - processor nodes and link edges (topology)
/// - [`InternalProcessorLinkGraphEcsExtension`] - runtime components via ECS
///
/// Components are attached to processor/link entities, allowing flexible
/// querying and dynamic attachment/detachment.
pub struct Graph {
    /// Internal topology store for processor nodes and link edges.
    processor_link_graph: Arc<RwLock<InternalProcessorLinkGraph>>,

    /// ECS extension for runtime components (state, instances, metrics, etc.).
    ecs_extension: InternalProcessorLinkGraphEcsExtension,

    /// When the graph was last compiled.
    compiled_at: Option<Instant>,

    /// Checksum of the source graph at compile time.
    source_checksum: Option<GraphChecksum>,

    /// Graph-level state.
    state: GraphState,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    /// Create a new empty Graph.
    pub fn new() -> Self {
        Self {
            processor_link_graph: Arc::new(RwLock::new(InternalProcessorLinkGraph::new())),
            ecs_extension: InternalProcessorLinkGraphEcsExtension::new(),
            compiled_at: None,
            source_checksum: None,
            state: GraphState::Idle,
        }
    }

    /// Create a Graph wrapping an existing internal topology graph.
    ///
    /// This is for testing only - external code should use `Graph::new()`.
    #[cfg(test)]
    pub(crate) fn new_with_internal(
        processor_link_graph: Arc<RwLock<InternalProcessorLinkGraph>>,
    ) -> Self {
        Self {
            processor_link_graph,
            ecs_extension: InternalProcessorLinkGraphEcsExtension::new(),
            compiled_at: None,
            source_checksum: None,
            state: GraphState::Idle,
        }
    }

    /// Get the current state.
    pub fn state(&self) -> GraphState {
        self.state
    }

    /// Set the graph state.
    pub fn set_state(&mut self, state: GraphState) {
        self.state = state;
    }

    /// Get when the graph was compiled.
    pub fn compiled_at(&self) -> Option<Instant> {
        self.compiled_at
    }

    /// Mark as compiled with current checksum.
    pub fn mark_compiled(&mut self) {
        self.compiled_at = Some(Instant::now());
        self.source_checksum = Some(self.processor_link_graph.read().checksum());
    }

    /// Check if recompilation is needed.
    pub fn needs_recompile(&self) -> bool {
        match self.source_checksum {
            Some(checksum) => self.processor_link_graph.read().checksum() != checksum,
            None => true, // Never compiled
        }
    }

    // =========================================================================
    // Entity Management
    // =========================================================================

    /// Ensure a processor has an entity in the ECS world.
    ///
    /// Creates an entity if one doesn't exist.
    pub fn ensure_processor_entity(&mut self, id: &ProcessorId) -> Entity {
        self.ecs_extension.ensure_processor_entity(id)
    }

    /// Get the entity for a processor, if it exists.
    pub fn get_processor_entity(&self, id: &ProcessorId) -> Option<Entity> {
        self.ecs_extension.get_processor_entity(id)
    }

    /// Remove a processor's entity from the ECS world.
    pub fn remove_processor_entity(&mut self, id: &ProcessorId) -> Option<Entity> {
        self.ecs_extension.remove_processor_entity(id)
    }

    /// Get all processor IDs with entities.
    pub fn processor_ids(&self) -> impl Iterator<Item = &ProcessorId> {
        self.ecs_extension.processor_ids()
    }

    /// Get number of processors with entities.
    pub fn entity_count(&self) -> usize {
        self.ecs_extension.processor_entity_count()
    }

    // =========================================================================
    // Component Operations
    // =========================================================================

    /// Attach a component to a processor.
    pub fn insert<C: Component>(&mut self, id: &ProcessorId, component: C) -> Result<()> {
        self.ecs_extension.insert_processor_component(id, component)
    }

    /// Get a component for a processor.
    pub fn get<C: Component>(&self, id: &ProcessorId) -> Option<hecs::Ref<'_, C>> {
        self.ecs_extension.get_processor_component(id)
    }

    /// Get a mutable component for a processor.
    pub fn get_mut<C: Component>(&mut self, id: &ProcessorId) -> Option<hecs::RefMut<'_, C>> {
        self.ecs_extension.get_processor_component_mut(id)
    }

    /// Remove a component from a processor.
    pub fn remove<C: Component>(&mut self, id: &ProcessorId) -> Option<C> {
        self.ecs_extension.remove_processor_component(id)
    }

    /// Check if a processor has a component.
    pub fn has<C: Component>(&self, id: &ProcessorId) -> bool {
        self.ecs_extension.processor_has_component::<C>(id)
    }

    // =========================================================================
    // Link Entity Management
    // =========================================================================

    /// Ensure a link has an entity in the ECS world.
    pub fn ensure_link_entity(&mut self, id: &LinkId) -> Entity {
        self.ecs_extension.ensure_link_entity(id)
    }

    /// Get the entity for a link, if it exists.
    pub fn get_link_entity(&self, id: &LinkId) -> Option<Entity> {
        self.ecs_extension.get_link_entity(id)
    }

    /// Remove a link's entity from the ECS world.
    pub fn remove_link_entity(&mut self, id: &LinkId) -> Option<Entity> {
        self.ecs_extension.remove_link_entity(id)
    }

    /// Insert a component on a link entity.
    pub fn insert_link<C: Component>(&mut self, id: &LinkId, component: C) -> Result<()> {
        self.ecs_extension.insert_link_component(id, component)
    }

    /// Remove a component from a link entity.
    pub fn remove_link_component<C: Component>(&mut self, id: &LinkId) -> Result<()> {
        self.ecs_extension.remove_link_component::<C>(id)
    }

    /// Get a component from a link entity.
    pub fn get_link_component<C: Component>(&self, id: &LinkId) -> Option<hecs::Ref<'_, C>> {
        self.ecs_extension.get_link_component(id)
    }

    /// Get the state of a link from its ECS component.
    pub fn get_link_state(&self, id: &LinkId) -> Option<LinkState> {
        self.ecs_extension.get_link_state(id)
    }

    /// Set the state of a link via its ECS component.
    pub fn set_link_state(&mut self, id: &LinkId, state: LinkState) -> Result<()> {
        self.ecs_extension.set_link_state(id, state)
    }

    // =========================================================================
    // Query Operations
    // =========================================================================

    /// Find all processors with a specific component type.
    pub fn processors_with<C: Component>(&self) -> Vec<ProcessorId> {
        self.ecs_extension.processors_with_component::<C>()
    }

    /// Clear all ECS state (entities and components).
    pub fn clear_entities(&mut self) {
        self.ecs_extension.clear();
        self.compiled_at = None;
        self.source_checksum = None;
    }

    // =========================================================================
    // Convenience Methods (delegating to topology graph)
    // =========================================================================

    /// Get a processor node by ID.
    pub fn get_processor(&self, id: &ProcessorId) -> Option<ProcessorNode> {
        self.processor_link_graph.read().get_processor(id).cloned()
    }

    /// Get a link by ID.
    pub fn get_link(&self, id: &LinkId) -> Option<Link> {
        self.processor_link_graph.read().get_link(id).cloned()
    }

    /// Check if processor exists in graph.
    pub fn has_processor(&self, id: &ProcessorId) -> bool {
        self.processor_link_graph.read().has_processor(id)
    }

    /// Get the number of processors in the graph.
    pub fn processor_count(&self) -> usize {
        self.processor_link_graph.read().processor_count()
    }

    /// Get the number of links in the graph.
    pub fn link_count(&self) -> usize {
        self.processor_link_graph.read().link_count()
    }

    /// Get all processor nodes (cloned).
    pub fn nodes(&self) -> Vec<ProcessorNode> {
        self.processor_link_graph.read().nodes().to_vec()
    }

    /// Get all links (cloned).
    pub fn links(&self) -> Vec<Link> {
        self.processor_link_graph.read().links().to_vec()
    }

    // =========================================================================
    // Topology Mutation Operations
    // =========================================================================

    /// Add a processor to the graph.
    ///
    /// Creates both the topology node and an ECS entity for the processor.
    pub fn add_processor(
        &mut self,
        id: ProcessorId,
        processor_type: String,
        port_mask: u64,
    ) -> ProcessorNode {
        self.processor_link_graph.write().add_processor(
            id.clone(),
            processor_type.clone(),
            port_mask,
        );

        // Create ECS entity for this processor
        self.ensure_processor_entity(&id);

        // Return the created node
        ProcessorNode::new(id, processor_type, None, vec![], vec![])
    }

    /// Add a processor node using its type and config.
    ///
    /// Creates both the topology node and an ECS entity for the processor.
    pub fn add_processor_node<P>(&mut self, config: P::Config) -> Result<ProcessorNode>
    where
        P: Processor + 'static,
        P::Config: serde::Serialize,
    {
        let node = self
            .processor_link_graph
            .write()
            .add_processor_node::<P>(config)?;

        // Create ECS entity for this processor
        self.ensure_processor_entity(&node.id);

        Ok(node)
    }

    /// Remove a processor node from the topology.
    pub fn remove_processor_node(&mut self, id: &ProcessorId) {
        self.processor_link_graph.write().remove_processor_node(id);
    }

    /// Remove a processor completely (topology + ECS entity).
    pub fn remove_processor(&mut self, id: &ProcessorId) {
        self.processor_link_graph.write().remove_processor(id);
        self.remove_processor_entity(id);
    }

    /// Add a link between two ports.
    ///
    /// Creates both the topology edge and an ECS entity for the link.
    pub fn add_link(
        &mut self,
        from: impl IntoLinkPortRef,
        to: impl IntoLinkPortRef,
    ) -> Result<Link> {
        let link = self.processor_link_graph.write().add_link(from, to)?;

        // Create ECS entity for this link
        self.ensure_link_entity(&link.id);

        Ok(link)
    }

    /// Remove a link from the topology.
    pub fn remove_link(&mut self, id: &LinkId) {
        self.processor_link_graph.write().remove_link(id);
    }

    /// Remove a link completely (topology + ECS entity).
    pub fn remove_link_fully(&mut self, id: &LinkId) {
        self.processor_link_graph.write().remove_link(id);
        self.remove_link_entity(id);
    }

    /// Add a link by port address strings.
    pub fn add_link_by_address(&mut self, from_port: String, to_port: String) -> LinkId {
        let link_id = self
            .processor_link_graph
            .write()
            .add_link_by_address(from_port, to_port);

        // Create ECS entity for this link
        self.ensure_link_entity(&link_id);

        link_id
    }

    /// Find a link by its source and target port addresses.
    pub fn find_link(&self, from_port: &str, to_port: &str) -> Option<LinkId> {
        self.processor_link_graph
            .read()
            .find_link(from_port, to_port)
    }

    /// Update a processor's configuration.
    pub fn update_processor_config(
        &mut self,
        processor_id: &ProcessorId,
        config: serde_json::Value,
    ) -> Result<()> {
        self.processor_link_graph
            .write()
            .update_processor_config(processor_id, config)?;
        Ok(())
    }

    /// Get processor config checksum.
    pub fn get_processor_config_checksum(&self, processor_id: &ProcessorId) -> Option<u64> {
        self.processor_link_graph
            .read()
            .get_processor_config_checksum(processor_id)
    }

    /// Get topological order of processors.
    pub fn topological_order(&self) -> Result<Vec<ProcessorId>> {
        self.processor_link_graph.read().topological_order()
    }

    /// Find source processors (no inputs).
    pub fn find_sources(&self) -> Vec<ProcessorId> {
        self.processor_link_graph.read().find_sources()
    }

    /// Find sink processors (no outputs).
    pub fn find_sinks(&self) -> Vec<ProcessorId> {
        self.processor_link_graph.read().find_sinks()
    }

    /// Validate the graph structure.
    pub fn validate(&self) -> Result<()> {
        self.processor_link_graph.read().validate()
    }

    // =========================================================================
    // Serialization
    // =========================================================================

    /// Serialize the entire graph to JSON, including topology and ECS components.
    ///
    /// Output structure:
    /// ```json
    /// {
    ///   "state": "Running",
    ///   "processors": {
    ///     "camera": {
    ///       "type": "CameraProcessor",
    ///       "state": "Running",
    ///       "metrics": { ... },
    ///       "config": { ... },
    ///       "runtime": { ... }
    ///     }
    ///   },
    ///   "links": {
    ///     "link_0": {
    ///       "from": { "processor": "camera", "port": "output" },
    ///       "to": { "processor": "display", "port": "input" },
    ///       "state": "Connected",
    ///       "type_info": { "type_name": "VideoFrame", "capacity": 4 },
    ///       "buffer": { "fill_level": 2, "is_empty": false }
    ///     }
    ///   }
    /// }
    /// ```
    pub fn to_json(&self) -> JsonValue {
        let graph = self.processor_link_graph.read();
        let processor_entities = self.ecs_extension.processor_entities();
        let link_entities = self.ecs_extension.link_entities();
        let world = self.ecs_extension.world();

        // Serialize processors
        let mut processors = serde_json::Map::new();
        for node in graph.nodes() {
            let mut proc_json = serde_json::Map::new();

            // Basic topology info
            proc_json.insert(
                "type".into(),
                JsonValue::String(node.processor_type.clone()),
            );

            // Add ECS components if entity exists
            if let Some(&entity) = processor_entities.get(&node.id) {
                // StateComponent
                if let Ok(state) = world.get::<&StateComponent>(entity) {
                    proc_json.insert(state.json_key().into(), state.to_json());
                }

                // ProcessorMetrics
                if let Ok(metrics) = world.get::<&ProcessorMetrics>(entity) {
                    proc_json.insert(metrics.json_key().into(), metrics.to_json());
                }

                // ProcessorInstance - get config and runtime state from the processor itself
                if let Ok(instance) = world.get::<&ProcessorInstance>(entity) {
                    let processor = instance.0.lock();

                    // Config JSON
                    let config = processor.config_json();
                    if !config.is_null() {
                        proc_json.insert("config".into(), config);
                    }

                    // Runtime JSON (custom processor state)
                    let runtime = processor.to_runtime_json();
                    if !runtime.is_null() {
                        proc_json.insert("runtime".into(), runtime);
                    }
                }
            }

            processors.insert(node.id.to_string(), JsonValue::Object(proc_json));
        }

        // Serialize links
        let mut links = serde_json::Map::new();
        for link in graph.links() {
            let mut link_json = serde_json::Map::new();

            // Topology info
            link_json.insert(
                "from".into(),
                serde_json::json!({
                    "processor": link.source.node,
                    "port": link.source.port
                }),
            );
            link_json.insert(
                "to".into(),
                serde_json::json!({
                    "processor": link.target.node,
                    "port": link.target.port
                }),
            );

            // Add ECS components if entity exists
            if let Some(&entity) = link_entities.get(&link.id) {
                // LinkStateComponent
                if let Ok(state) = world.get::<&LinkStateComponent>(entity) {
                    link_json.insert(state.json_key().into(), state.to_json());
                }

                // LinkTypeInfoComponent
                if let Ok(type_info) = world.get::<&LinkTypeInfoComponent>(entity) {
                    link_json.insert(type_info.json_key().into(), type_info.to_json());
                }

                // LinkInstanceComponent (buffer stats)
                if let Ok(instance) = world.get::<&LinkInstanceComponent>(entity) {
                    link_json.insert(instance.json_key().into(), instance.to_json());
                }
            }

            links.insert(link.id.to_string(), JsonValue::Object(link_json));
        }

        serde_json::json!({
            "state": format!("{:?}", self.state),
            "processors": processors,
            "links": links
        })
    }

    /// Generate DOT format for Graphviz visualization.
    ///
    /// Includes node labels with processor type and state, and edge labels
    /// with link type and buffer status.
    pub fn to_dot(&self) -> String {
        let graph = self.processor_link_graph.read();
        let processor_entities = self.ecs_extension.processor_entities();
        let link_entities = self.ecs_extension.link_entities();
        let world = self.ecs_extension.world();

        let mut dot = String::new();

        dot.push_str("digraph StreamGraph {\n");
        dot.push_str("    rankdir=LR;\n");
        dot.push_str("    node [shape=box, style=rounded];\n\n");

        // Output processor nodes
        for node in graph.nodes() {
            let mut label_parts = vec![node.id.to_string(), node.processor_type.clone()];

            // Add state if available
            if let Some(&entity) = processor_entities.get(&node.id) {
                if let Ok(state) = world.get::<&StateComponent>(entity) {
                    let state_str = format!("{:?}", *state.0.lock());
                    label_parts.push(state_str);
                }
            }

            let label = label_parts.join("\\n");
            dot.push_str(&format!("    \"{}\" [label=\"{}\"];\n", node.id, label));
        }

        dot.push('\n');

        // Output edges (links)
        for link in graph.links() {
            let mut label_parts = Vec::new();

            // Add type info and buffer stats if available
            if let Some(&entity) = link_entities.get(&link.id) {
                if let Ok(type_info) = world.get::<&LinkTypeInfoComponent>(entity) {
                    // Shorten type name (just the last part)
                    let short_type = type_info
                        .type_name
                        .rsplit("::")
                        .next()
                        .unwrap_or(type_info.type_name);
                    label_parts.push(short_type.to_string());
                }

                if let Ok(instance) = world.get::<&LinkInstanceComponent>(entity) {
                    label_parts.push(format!("[{}/{}]", instance.0.len(), {
                        // Get capacity from type_info if available
                        if let Ok(ti) = world.get::<&LinkTypeInfoComponent>(entity) {
                            ti.capacity
                        } else {
                            0
                        }
                    }));
                }
            }

            let label = if label_parts.is_empty() {
                String::new()
            } else {
                format!(" [label=\"{}\"]", label_parts.join("\\n"))
            };

            dot.push_str(&format!(
                "    \"{}\":\"{}\" -> \"{}\":\"{}\"{};",
                link.source.node, link.source.port, link.target.node, link.target.port, label
            ));
            dot.push('\n');
        }

        dot.push_str("}\n");
        dot
    }
}

// =============================================================================
// Query Interface Implementation
// =============================================================================

use super::internal::InternalProcessorLinkGraphQueryOperations;
use super::query::executor::{
    GraphQueryExecutor, GraphQueryInterface, LinkQueryResult, ProcessorQueryResult,
};
use super::query::field_resolver::{resolve_json_path, FieldResolver};
use super::query::{LinkQuery, ProcessorQuery};
use crate::core::processors::ProcessorState;

impl FieldResolver for Graph {
    fn resolve_processor_field(&self, processor_id: &ProcessorId, path: &str) -> Option<JsonValue> {
        self.processor_to_json(processor_id)
            .and_then(|json| resolve_json_path(&json, path))
    }

    fn resolve_link_field(&self, link_id: &LinkId, path: &str) -> Option<JsonValue> {
        self.link_to_json(link_id)
            .and_then(|json| resolve_json_path(&json, path))
    }

    fn processor_to_json(&self, processor_id: &ProcessorId) -> Option<JsonValue> {
        let graph = self.processor_link_graph.read();
        let node = graph.get_processor(processor_id)?;

        let mut proc_json = serde_json::Map::new();
        proc_json.insert(
            "type".into(),
            JsonValue::String(node.processor_type.clone()),
        );

        if let Some(config) = &node.config {
            proc_json.insert("config".into(), config.clone());
        }

        // Add ECS components
        if let Some(&entity) = self.ecs_extension.processor_entities().get(processor_id) {
            let world = self.ecs_extension.world();

            if let Ok(state) = world.get::<&StateComponent>(entity) {
                proc_json.insert(state.json_key().into(), state.to_json());
            }

            if let Ok(metrics) = world.get::<&ProcessorMetrics>(entity) {
                proc_json.insert(metrics.json_key().into(), metrics.to_json());
            }

            if let Ok(instance) = world.get::<&ProcessorInstance>(entity) {
                let processor = instance.0.lock();

                let config = processor.config_json();
                if !config.is_null() {
                    proc_json.insert("config".into(), config);
                }

                let runtime = processor.to_runtime_json();
                if !runtime.is_null() {
                    proc_json.insert("runtime".into(), runtime);
                }
            }
        }

        Some(JsonValue::Object(proc_json))
    }

    fn link_to_json(&self, link_id: &LinkId) -> Option<JsonValue> {
        let graph = self.processor_link_graph.read();
        let link = graph.get_link(link_id)?;

        let mut link_json = serde_json::Map::new();

        link_json.insert(
            "from".into(),
            serde_json::json!({
                "processor": link.source.node,
                "port": link.source.port
            }),
        );
        link_json.insert(
            "to".into(),
            serde_json::json!({
                "processor": link.target.node,
                "port": link.target.port
            }),
        );

        // Add ECS components
        if let Some(&entity) = self.ecs_extension.link_entities().get(link_id) {
            let world = self.ecs_extension.world();

            if let Ok(state) = world.get::<&LinkStateComponent>(entity) {
                link_json.insert(state.json_key().into(), state.to_json());
            }

            if let Ok(type_info) = world.get::<&LinkTypeInfoComponent>(entity) {
                link_json.insert(type_info.json_key().into(), type_info.to_json());
            }

            if let Ok(instance) = world.get::<&LinkInstanceComponent>(entity) {
                link_json.insert(instance.json_key().into(), instance.to_json());
            }
        }

        Some(JsonValue::Object(link_json))
    }
}

impl GraphQueryInterface for Graph {
    fn all_processor_ids(&self) -> Vec<ProcessorId> {
        self.processor_link_graph
            .read()
            .nodes()
            .iter()
            .map(|n| n.id.clone())
            .collect()
    }

    fn get_processor_node(&self, id: &ProcessorId) -> Option<ProcessorNode> {
        self.processor_link_graph.read().get_processor(id).cloned()
    }

    fn has_processor(&self, id: &ProcessorId) -> bool {
        self.processor_link_graph.read().has_processor(id)
    }

    fn get_processor_type(&self, id: &ProcessorId) -> Option<String> {
        self.processor_link_graph
            .read()
            .get_processor(id)
            .map(|n| n.processor_type.clone())
    }

    fn get_processor_state(&self, id: &ProcessorId) -> Option<ProcessorState> {
        self.ecs_extension
            .get_processor_component::<StateComponent>(id)
            .map(|sc| *sc.0.lock())
    }

    fn get_processor_config(&self, id: &ProcessorId) -> Option<JsonValue> {
        self.processor_link_graph
            .read()
            .get_processor(id)
            .and_then(|n| n.config.clone())
    }

    fn all_link_ids(&self) -> Vec<LinkId> {
        self.processor_link_graph.read().query_all_link_ids()
    }

    fn get_link(&self, id: &LinkId) -> Option<Link> {
        self.processor_link_graph.read().get_link(id).cloned()
    }

    fn has_link(&self, id: &LinkId) -> bool {
        self.processor_link_graph.read().get_link(id).is_some()
    }

    fn get_link_source(&self, id: &LinkId) -> Option<ProcessorId> {
        self.processor_link_graph
            .read()
            .get_link(id)
            .map(|l| l.source.node.clone())
    }

    fn get_link_target(&self, id: &LinkId) -> Option<ProcessorId> {
        self.processor_link_graph
            .read()
            .get_link(id)
            .map(|l| l.target.node.clone())
    }

    fn downstream_processor_ids(&self, id: &ProcessorId) -> Vec<ProcessorId> {
        self.processor_link_graph
            .read()
            .query_downstream_processor_ids(id)
    }

    fn upstream_processor_ids(&self, id: &ProcessorId) -> Vec<ProcessorId> {
        self.processor_link_graph
            .read()
            .query_upstream_processor_ids(id)
    }

    fn outgoing_link_ids(&self, id: &ProcessorId) -> Vec<LinkId> {
        self.processor_link_graph.read().query_outgoing_link_ids(id)
    }

    fn incoming_link_ids(&self, id: &ProcessorId) -> Vec<LinkId> {
        self.processor_link_graph.read().query_incoming_link_ids(id)
    }

    fn topological_order(&self) -> Option<Vec<ProcessorId>> {
        self.processor_link_graph.read().topological_order().ok()
    }

    fn source_processor_ids(&self) -> Vec<ProcessorId> {
        self.processor_link_graph.read().find_sources()
    }

    fn sink_processor_ids(&self) -> Vec<ProcessorId> {
        self.processor_link_graph.read().find_sinks()
    }
}

impl GraphQueryExecutor for Graph {
    fn execute_processor_query<T>(&self, query: &ProcessorQuery<T>) -> T
    where
        T: ProcessorQueryResult,
    {
        let ids = super::query::executor::execute_processor_query_full(query, self);
        T::from_terminal(query.terminal, ids, self)
    }

    fn execute_link_query<T>(&self, query: &LinkQuery<T>) -> T
    where
        T: LinkQueryResult,
    {
        let ids = super::query::executor::execute_link_query_full(query, self);
        T::from_terminal(query.terminal, ids, self)
    }
}

impl Graph {
    /// Start building a query on this graph.
    ///
    /// Returns a [`Query`] entry point for the fluent query API.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let query = Query::build()
    ///     .V()
    ///     .of_type("CameraProcessor")
    ///     .ids();
    ///
    /// let cameras = graph.execute(&query);
    /// ```
    pub fn execute<T>(&self, query: &ProcessorQuery<T>) -> T
    where
        T: ProcessorQueryResult,
    {
        self.execute_processor_query(query)
    }

    /// Execute a link query.
    pub fn execute_link<T>(&self, query: &LinkQuery<T>) -> T
    where
        T: LinkQueryResult,
    {
        self.execute_link_query(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Simple test component
    struct TestComponent(i32);

    #[test]
    fn test_property_graph_creation() {
        let pg = Graph::new();

        assert_eq!(pg.state(), GraphState::Idle);
        assert_eq!(pg.entity_count(), 0);
        assert!(pg.needs_recompile()); // Never compiled
    }

    #[test]
    fn test_property_graph_entity_management() {
        let mut pg = Graph::new();

        let id: ProcessorId = "test_proc".into();

        // No entity initially
        assert!(pg.get_processor_entity(&id).is_none());

        // Create entity
        let entity = pg.ensure_processor_entity(&id);
        assert!(pg.get_processor_entity(&id).is_some());
        assert_eq!(pg.entity_count(), 1);

        // Same entity on second call
        let entity2 = pg.ensure_processor_entity(&id);
        assert_eq!(entity, entity2);
        assert_eq!(pg.entity_count(), 1);

        // Remove entity
        pg.remove_processor_entity(&id);
        assert!(pg.get_processor_entity(&id).is_none());
        assert_eq!(pg.entity_count(), 0);
    }

    #[test]
    fn test_property_graph_components() {
        let mut pg = Graph::new();

        let id: ProcessorId = "test_proc".into();
        pg.ensure_processor_entity(&id);

        // Insert component
        pg.insert(&id, TestComponent(42)).unwrap();
        assert!(pg.has::<TestComponent>(&id));

        // Get component
        let comp = pg.get::<TestComponent>(&id).unwrap();
        assert_eq!(comp.0, 42);
        drop(comp);

        // Remove component
        let removed = pg.remove::<TestComponent>(&id).unwrap();
        assert_eq!(removed.0, 42);
        assert!(!pg.has::<TestComponent>(&id));
    }

    #[test]
    fn test_property_graph_needs_recompile() {
        let internal_graph = Arc::new(RwLock::new(InternalProcessorLinkGraph::new()));
        let mut pg = Graph::new_with_internal(Arc::clone(&internal_graph));

        // Initially needs recompile
        assert!(pg.needs_recompile());

        // Mark as compiled
        pg.mark_compiled();
        assert!(!pg.needs_recompile());

        // Modify graph
        internal_graph
            .write()
            .add_processor("test".into(), "TestProcessor".into(), 0);

        // Now needs recompile
        assert!(pg.needs_recompile());
    }

    #[test]
    fn test_property_graph_query() {
        let mut pg = Graph::new();

        let id1: ProcessorId = "proc1".into();
        let id2: ProcessorId = "proc2".into();
        let id3: ProcessorId = "proc3".into();

        pg.ensure_processor_entity(&id1);
        pg.ensure_processor_entity(&id2);
        pg.ensure_processor_entity(&id3);

        // Only attach component to some processors
        pg.insert(&id1, TestComponent(1)).unwrap();
        pg.insert(&id3, TestComponent(3)).unwrap();

        // Query for processors with the component
        let with_component = pg.processors_with::<TestComponent>();
        assert_eq!(with_component.len(), 2);
        assert!(with_component.contains(&id1));
        assert!(!with_component.contains(&id2));
        assert!(with_component.contains(&id3));
    }

    #[test]
    fn test_property_graph_state() {
        let mut pg = Graph::new();

        assert_eq!(pg.state(), GraphState::Idle);

        pg.set_state(GraphState::Running);
        assert_eq!(pg.state(), GraphState::Running);

        pg.set_state(GraphState::Paused);
        assert_eq!(pg.state(), GraphState::Paused);
    }

    #[test]
    fn test_property_graph_to_json_basic() {
        let graph = Arc::new(RwLock::new(InternalProcessorLinkGraph::new()));

        // Add processors to graph
        graph
            .write()
            .add_processor("camera".into(), "CameraProcessor".into(), 0);
        graph
            .write()
            .add_processor("display".into(), "DisplayProcessor".into(), 1);

        let mut pg = Graph::new_with_internal(Arc::clone(&graph));
        pg.set_state(GraphState::Running);

        // Create entities for processors
        pg.ensure_processor_entity(&"camera".into());
        pg.ensure_processor_entity(&"display".into());

        // Add state component
        pg.insert(&"camera".into(), StateComponent::default())
            .unwrap();

        // Add metrics component
        let mut metrics = ProcessorMetrics::default();
        metrics.throughput_fps = 30.0;
        metrics.frames_processed = 100;
        pg.insert(&"display".into(), metrics).unwrap();

        let json = pg.to_json();

        // Verify structure
        assert_eq!(json["state"], "Running");
        assert!(json["processors"]["camera"].is_object());
        assert!(json["processors"]["display"].is_object());
        assert_eq!(json["processors"]["camera"]["type"], "CameraProcessor");
        assert_eq!(json["processors"]["display"]["type"], "DisplayProcessor");

        // Verify state component was serialized
        assert!(json["processors"]["camera"]["state"].is_string());

        // Verify metrics component was serialized
        assert_eq!(
            json["processors"]["display"]["metrics"]["throughput_fps"],
            30.0
        );
        assert_eq!(
            json["processors"]["display"]["metrics"]["frames_processed"],
            100
        );
    }

    #[test]
    fn test_property_graph_to_json_with_links() {
        let graph = Arc::new(RwLock::new(InternalProcessorLinkGraph::new()));

        // Add processors
        graph
            .write()
            .add_processor("source".into(), "SourceProcessor".into(), 0);
        graph
            .write()
            .add_processor("sink".into(), "SinkProcessor".into(), 1);

        // Add link - add_link returns the created Link
        let link = graph
            .write()
            .add_link("source.output", "sink.input")
            .unwrap();
        let link_id = link.id.clone();

        let mut pg = Graph::new_with_internal(Arc::clone(&graph));

        // Create link entity with components
        pg.ensure_link_entity(&link_id);
        pg.insert_link(&link_id, LinkStateComponent(LinkState::Wired))
            .unwrap();
        pg.insert_link(
            &link_id,
            LinkTypeInfoComponent {
                type_id: std::any::TypeId::of::<u32>(),
                type_name: "u32",
                capacity: 8,
            },
        )
        .unwrap();

        let json = pg.to_json();

        // Verify link is in output
        let links = &json["links"];
        assert!(links.is_object());

        // Find our link (key is the link_id string)
        let link_json = &links[link_id.to_string()];
        assert!(link_json.is_object());

        // Verify from/to
        assert_eq!(link_json["from"]["processor"], "source");
        assert_eq!(link_json["from"]["port"], "output");
        assert_eq!(link_json["to"]["processor"], "sink");
        assert_eq!(link_json["to"]["port"], "input");

        // Verify ECS components
        assert!(link_json["state"].is_string());
        assert_eq!(link_json["type_info"]["type_name"], "u32");
        assert_eq!(link_json["type_info"]["capacity"], 8);
    }

    #[test]
    fn test_property_graph_to_dot() {
        let graph = Arc::new(RwLock::new(InternalProcessorLinkGraph::new()));

        // Add processors
        graph
            .write()
            .add_processor("camera".into(), "CameraProcessor".into(), 0);
        graph
            .write()
            .add_processor("display".into(), "DisplayProcessor".into(), 1);

        // Add link
        let link = graph
            .write()
            .add_link("camera.output", "display.input")
            .unwrap();
        let link_id = link.id.clone();

        let mut pg = Graph::new_with_internal(Arc::clone(&graph));

        // Create entities
        pg.ensure_processor_entity(&"camera".into());
        pg.ensure_link_entity(&link_id);

        // Add state to camera
        pg.insert(&"camera".into(), StateComponent::default())
            .unwrap();

        // Add type info to link
        pg.insert_link(
            &link_id,
            LinkTypeInfoComponent {
                type_id: std::any::TypeId::of::<String>(),
                type_name: "streamlib::core::frames::VideoFrame",
                capacity: 4,
            },
        )
        .unwrap();

        let dot = pg.to_dot();

        // Basic DOT structure
        assert!(dot.starts_with("digraph StreamGraph {"));
        assert!(dot.ends_with("}\n"));
        assert!(dot.contains("rankdir=LR"));

        // Node declarations
        assert!(dot.contains("\"camera\""));
        assert!(dot.contains("\"display\""));
        assert!(dot.contains("CameraProcessor"));

        // Edge declaration
        assert!(dot.contains("->"));

        // Type info in edge label (shortened)
        assert!(dot.contains("VideoFrame"));
    }

    #[test]
    fn test_property_graph_to_json_handles_missing_components() {
        // Verify that processors/links without ECS components still serialize
        let graph = Arc::new(RwLock::new(InternalProcessorLinkGraph::new()));

        graph
            .write()
            .add_processor("test".into(), "TestProcessor".into(), 0);

        let pg = Graph::new_with_internal(Arc::clone(&graph));

        // Don't create any entities - just serialize
        let json = pg.to_json();

        // Should still have the processor with basic info
        assert!(json["processors"]["test"].is_object());
        assert_eq!(json["processors"]["test"]["type"], "TestProcessor");

        // But no ECS components (no state, metrics, etc)
        assert!(json["processors"]["test"]["state"].is_null());
        assert!(json["processors"]["test"]["metrics"].is_null());
    }
}
