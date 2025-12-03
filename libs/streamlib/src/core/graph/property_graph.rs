//! Unified PropertyGraph combining topology with ECS component storage.
//!
//! PropertyGraph merges the Graph (topology) with runtime components using hecs ECS.
//! Instead of separate Graph + ExecutionGraph, all state lives in one structure.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use hecs::{Component, Entity, World};
use parking_lot::RwLock;
use serde_json::Value as JsonValue;

use crate::core::error::{Result, StreamError};
use crate::core::graph::components::{
    EcsComponentJson, LinkStateComponent, ProcessorInstance, ProcessorMetrics, StateComponent,
};
use crate::core::graph::link::LinkState;
use crate::core::graph::{Graph, GraphChecksum, Link, ProcessorId, ProcessorNode};
use crate::core::links::graph::{LinkInstanceComponent, LinkTypeInfoComponent};
use crate::core::links::LinkId;

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
/// PropertyGraph combines:
/// - Topology (processor nodes and links) from Graph
/// - Runtime components (processor instances, threads, channels) via ECS
///
/// Components are attached to processor entities, allowing flexible
/// querying and dynamic attachment/detachment.
pub struct PropertyGraph {
    /// The underlying topology graph.
    graph: Arc<RwLock<Graph>>,

    /// ECS world for runtime components.
    world: World,

    /// Map from ProcessorId to hecs Entity.
    processor_entities: HashMap<ProcessorId, Entity>,

    /// Map from LinkId to hecs Entity (for link-level components if needed).
    link_entities: HashMap<LinkId, Entity>,

    /// When the graph was last compiled.
    compiled_at: Option<Instant>,

    /// Checksum of the source Graph at compile time.
    source_checksum: Option<GraphChecksum>,

    /// Graph-level state.
    state: GraphState,
}

impl PropertyGraph {
    /// Create a new PropertyGraph wrapping an existing Graph.
    pub fn new(graph: Arc<RwLock<Graph>>) -> Self {
        Self {
            graph,
            world: World::new(),
            processor_entities: HashMap::new(),
            link_entities: HashMap::new(),
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

    /// Get reference to the underlying Graph.
    pub fn graph(&self) -> &Arc<RwLock<Graph>> {
        &self.graph
    }

    /// Get when the graph was compiled.
    pub fn compiled_at(&self) -> Option<Instant> {
        self.compiled_at
    }

    /// Mark as compiled with current checksum.
    pub fn mark_compiled(&mut self) {
        self.compiled_at = Some(Instant::now());
        self.source_checksum = Some(self.graph.read().checksum());
    }

    /// Check if recompilation is needed.
    pub fn needs_recompile(&self) -> bool {
        match self.source_checksum {
            Some(checksum) => self.graph.read().checksum() != checksum,
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
        if let Some(&entity) = self.processor_entities.get(id) {
            entity
        } else {
            let entity = self.world.spawn(());
            self.processor_entities.insert(id.clone(), entity);
            entity
        }
    }

    /// Get the entity for a processor, if it exists.
    pub fn get_processor_entity(&self, id: &ProcessorId) -> Option<Entity> {
        self.processor_entities.get(id).copied()
    }

    /// Remove a processor's entity from the ECS world.
    pub fn remove_processor_entity(&mut self, id: &ProcessorId) -> Option<Entity> {
        if let Some(entity) = self.processor_entities.remove(id) {
            let _ = self.world.despawn(entity);
            Some(entity)
        } else {
            None
        }
    }

    /// Get all processor IDs with entities.
    pub fn processor_ids(&self) -> impl Iterator<Item = &ProcessorId> {
        self.processor_entities.keys()
    }

    /// Get number of processors with entities.
    pub fn entity_count(&self) -> usize {
        self.processor_entities.len()
    }

    // =========================================================================
    // Component Operations
    // =========================================================================

    /// Attach a component to a processor.
    pub fn insert<C: Component>(&mut self, id: &ProcessorId, component: C) -> Result<()> {
        let entity = self
            .processor_entities
            .get(id)
            .ok_or_else(|| StreamError::ProcessorNotFound(id.clone()))?;

        self.world
            .insert_one(*entity, component)
            .map_err(|e| StreamError::Runtime(format!("Failed to insert component: {}", e)))?;

        Ok(())
    }

    /// Get a component for a processor.
    pub fn get<C: Component>(&self, id: &ProcessorId) -> Option<hecs::Ref<'_, C>> {
        let entity = self.processor_entities.get(id)?;
        self.world.get::<&C>(*entity).ok()
    }

    /// Get a mutable component for a processor.
    pub fn get_mut<C: Component>(&mut self, id: &ProcessorId) -> Option<hecs::RefMut<'_, C>> {
        let entity = self.processor_entities.get(id)?;
        self.world.get::<&mut C>(*entity).ok()
    }

    /// Remove a component from a processor.
    pub fn remove<C: Component>(&mut self, id: &ProcessorId) -> Option<C> {
        let entity = self.processor_entities.get(id)?;
        self.world.remove_one::<C>(*entity).ok()
    }

    /// Check if a processor has a component.
    pub fn has<C: Component>(&self, id: &ProcessorId) -> bool {
        self.processor_entities
            .get(id)
            .map(|e| self.world.get::<&C>(*e).is_ok())
            .unwrap_or(false)
    }

    // =========================================================================
    // Link Entity Management (for future use)
    // =========================================================================

    /// Ensure a link has an entity in the ECS world.
    pub fn ensure_link_entity(&mut self, id: &LinkId) -> Entity {
        if let Some(&entity) = self.link_entities.get(id) {
            entity
        } else {
            let entity = self.world.spawn(());
            self.link_entities.insert(id.clone(), entity);
            entity
        }
    }

    /// Get the entity for a link, if it exists.
    pub fn get_link_entity(&self, id: &LinkId) -> Option<Entity> {
        self.link_entities.get(id).copied()
    }

    /// Remove a link's entity from the ECS world.
    pub fn remove_link_entity(&mut self, id: &LinkId) -> Option<Entity> {
        if let Some(entity) = self.link_entities.remove(id) {
            let _ = self.world.despawn(entity);
            Some(entity)
        } else {
            None
        }
    }

    /// Insert a component on a link entity.
    pub fn insert_link<C: Component>(&mut self, id: &LinkId, component: C) -> Result<()> {
        let entity = self
            .link_entities
            .get(id)
            .ok_or_else(|| StreamError::NotFound(format!("Link '{}' not found", id)))?;

        self.world
            .insert_one(*entity, component)
            .map_err(|e| StreamError::Runtime(format!("Failed to insert component: {}", e)))?;

        Ok(())
    }

    /// Remove a component from a link entity.
    pub fn remove_link_component<C: Component>(&mut self, id: &LinkId) -> Result<()> {
        let entity = *self
            .link_entities
            .get(id)
            .ok_or_else(|| StreamError::NotFound(format!("Link '{}' not found", id)))?;

        // Remove component if it exists (ignore if not present)
        let _ = self.world.remove_one::<C>(entity);

        Ok(())
    }

    /// Get a component from a link entity.
    pub fn get_link_component<C: Component>(&self, id: &LinkId) -> Option<hecs::Ref<'_, C>> {
        let entity = self.link_entities.get(id)?;
        self.world.get::<&C>(*entity).ok()
    }

    /// Get the state of a link from its ECS component.
    pub fn get_link_state(&self, id: &LinkId) -> Option<LinkState> {
        let entity = self.link_entities.get(id)?;
        self.world
            .get::<&LinkStateComponent>(*entity)
            .ok()
            .map(|c| c.0)
    }

    /// Set the state of a link via its ECS component.
    pub fn set_link_state(&mut self, id: &LinkId, state: LinkState) -> Result<()> {
        let entity = *self
            .link_entities
            .get(id)
            .ok_or_else(|| StreamError::NotFound(format!("Link '{}' not found", id)))?;

        // Check if component exists first
        let has_component = self.world.get::<&LinkStateComponent>(entity).is_ok();

        if has_component {
            // Update existing component
            if let Ok(mut comp) = self.world.get::<&mut LinkStateComponent>(entity) {
                comp.0 = state;
            }
        } else {
            // Insert new component
            self.world
                .insert_one(entity, LinkStateComponent(state))
                .map_err(|e| StreamError::Runtime(format!("Failed to set link state: {}", e)))?;
        }

        Ok(())
    }

    // =========================================================================
    // Query Operations
    // =========================================================================

    /// Find all processors with a specific component type.
    pub fn processors_with<C: Component>(&self) -> Vec<ProcessorId> {
        self.world
            .query::<&C>()
            .iter()
            .filter_map(|(entity, _)| {
                self.processor_entities
                    .iter()
                    .find(|(_, e)| **e == entity)
                    .map(|(id, _)| id.clone())
            })
            .collect()
    }

    /// Clear all ECS state (entities and components).
    pub fn clear_entities(&mut self) {
        self.world.clear();
        self.processor_entities.clear();
        self.link_entities.clear();
        self.compiled_at = None;
        self.source_checksum = None;
    }

    // =========================================================================
    // Convenience Methods (delegating to Graph)
    // =========================================================================

    /// Get a processor node by ID.
    pub fn get_processor(&self, id: &ProcessorId) -> Option<ProcessorNode> {
        self.graph.read().get_processor(id).cloned()
    }

    /// Get a link by ID.
    pub fn get_link(&self, id: &LinkId) -> Option<Link> {
        self.graph.read().get_link(id).cloned()
    }

    /// Check if processor exists in graph.
    pub fn has_processor(&self, id: &ProcessorId) -> bool {
        self.graph.read().has_processor(id)
    }

    /// Get the number of processors in the graph.
    pub fn processor_count(&self) -> usize {
        self.graph.read().processor_count()
    }

    /// Get the number of links in the graph.
    pub fn link_count(&self) -> usize {
        self.graph.read().link_count()
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
        let graph = self.graph.read();

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
            if let Some(&entity) = self.processor_entities.get(&node.id) {
                // StateComponent
                if let Ok(state) = self.world.get::<&StateComponent>(entity) {
                    proc_json.insert(state.json_key().into(), state.to_json());
                }

                // ProcessorMetrics
                if let Ok(metrics) = self.world.get::<&ProcessorMetrics>(entity) {
                    proc_json.insert(metrics.json_key().into(), metrics.to_json());
                }

                // ProcessorInstance - get config and runtime state from the processor itself
                if let Ok(instance) = self.world.get::<&ProcessorInstance>(entity) {
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
            if let Some(&entity) = self.link_entities.get(&link.id) {
                // LinkStateComponent
                if let Ok(state) = self.world.get::<&LinkStateComponent>(entity) {
                    link_json.insert(state.json_key().into(), state.to_json());
                }

                // LinkTypeInfoComponent
                if let Ok(type_info) = self.world.get::<&LinkTypeInfoComponent>(entity) {
                    link_json.insert(type_info.json_key().into(), type_info.to_json());
                }

                // LinkInstanceComponent (buffer stats)
                if let Ok(instance) = self.world.get::<&LinkInstanceComponent>(entity) {
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
        let graph = self.graph.read();
        let mut dot = String::new();

        dot.push_str("digraph StreamGraph {\n");
        dot.push_str("    rankdir=LR;\n");
        dot.push_str("    node [shape=box, style=rounded];\n\n");

        // Output processor nodes
        for node in graph.nodes() {
            let mut label_parts = vec![node.id.to_string(), node.processor_type.clone()];

            // Add state if available
            if let Some(&entity) = self.processor_entities.get(&node.id) {
                if let Ok(state) = self.world.get::<&StateComponent>(entity) {
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
            if let Some(&entity) = self.link_entities.get(&link.id) {
                if let Ok(type_info) = self.world.get::<&LinkTypeInfoComponent>(entity) {
                    // Shorten type name (just the last part)
                    let short_type = type_info
                        .type_name
                        .rsplit("::")
                        .next()
                        .unwrap_or(type_info.type_name);
                    label_parts.push(short_type.to_string());
                }

                if let Ok(instance) = self.world.get::<&LinkInstanceComponent>(entity) {
                    label_parts.push(format!("[{}/{}]", instance.0.len(), {
                        // Get capacity from type_info if available
                        if let Ok(ti) = self.world.get::<&LinkTypeInfoComponent>(entity) {
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

#[cfg(test)]
mod tests {
    use super::*;

    // Simple test component
    struct TestComponent(i32);

    #[test]
    fn test_property_graph_creation() {
        let graph = Arc::new(RwLock::new(Graph::new()));
        let pg = PropertyGraph::new(graph);

        assert_eq!(pg.state(), GraphState::Idle);
        assert_eq!(pg.entity_count(), 0);
        assert!(pg.needs_recompile()); // Never compiled
    }

    #[test]
    fn test_property_graph_entity_management() {
        let graph = Arc::new(RwLock::new(Graph::new()));
        let mut pg = PropertyGraph::new(graph);

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
        let graph = Arc::new(RwLock::new(Graph::new()));
        let mut pg = PropertyGraph::new(graph);

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
        let graph = Arc::new(RwLock::new(Graph::new()));
        let mut pg = PropertyGraph::new(Arc::clone(&graph));

        // Initially needs recompile
        assert!(pg.needs_recompile());

        // Mark as compiled
        pg.mark_compiled();
        assert!(!pg.needs_recompile());

        // Modify graph
        graph
            .write()
            .add_processor("test".into(), "TestProcessor".into(), 0);

        // Now needs recompile
        assert!(pg.needs_recompile());
    }

    #[test]
    fn test_property_graph_query() {
        let graph = Arc::new(RwLock::new(Graph::new()));
        let mut pg = PropertyGraph::new(graph);

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
        let graph = Arc::new(RwLock::new(Graph::new()));
        let mut pg = PropertyGraph::new(graph);

        assert_eq!(pg.state(), GraphState::Idle);

        pg.set_state(GraphState::Running);
        assert_eq!(pg.state(), GraphState::Running);

        pg.set_state(GraphState::Paused);
        assert_eq!(pg.state(), GraphState::Paused);
    }

    #[test]
    fn test_property_graph_to_json_basic() {
        let graph = Arc::new(RwLock::new(Graph::new()));

        // Add processors to graph
        graph
            .write()
            .add_processor("camera".into(), "CameraProcessor".into(), 0);
        graph
            .write()
            .add_processor("display".into(), "DisplayProcessor".into(), 1);

        let mut pg = PropertyGraph::new(Arc::clone(&graph));
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
        let graph = Arc::new(RwLock::new(Graph::new()));

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

        let mut pg = PropertyGraph::new(Arc::clone(&graph));

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
        let graph = Arc::new(RwLock::new(Graph::new()));

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

        let mut pg = PropertyGraph::new(Arc::clone(&graph));

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
        let graph = Arc::new(RwLock::new(Graph::new()));

        graph
            .write()
            .add_processor("test".into(), "TestProcessor".into(), 0);

        let pg = PropertyGraph::new(Arc::clone(&graph));

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
