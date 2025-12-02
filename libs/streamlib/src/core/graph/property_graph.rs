//! Unified PropertyGraph combining topology with ECS component storage.
//!
//! PropertyGraph merges the Graph (topology) with runtime components using hecs ECS.
//! Instead of separate Graph + ExecutionGraph, all state lives in one structure.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use hecs::{Component, Entity, World};
use parking_lot::RwLock;

use crate::core::error::{Result, StreamError};
use crate::core::graph::{Graph, GraphChecksum, Link, ProcessorId, ProcessorNode};
use crate::core::link_channel::LinkId;

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
}
