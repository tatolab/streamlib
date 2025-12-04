//! ECS extension for the processor-link graph.
//!
//! This module provides runtime component storage via hecs ECS, extending
//! the topology graph with state, metrics, instances, and other runtime data.
//!
//! This is an internal implementation detail - use [`Graph`](super::super::Graph) instead.

use std::collections::HashMap;

use hecs::{Component, Entity, World};

use crate::core::error::{Result, StreamError};
use crate::core::graph::components::LinkStateComponent;
use crate::core::graph::link::LinkState;
use crate::core::graph::ProcessorId;
use crate::core::links::LinkId;

/// Internal ECS extension for runtime components.
///
/// This wraps the hecs World and provides component storage for processors and links.
/// It is an internal implementation detail - do not use directly.
pub(crate) struct InternalProcessorLinkGraphEcsExtension {
    /// The hecs ECS world for component storage.
    world: World,

    /// Map from ProcessorId to hecs Entity.
    processor_entities: HashMap<ProcessorId, Entity>,

    /// Map from LinkId to hecs Entity.
    link_entities: HashMap<LinkId, Entity>,
}

impl InternalProcessorLinkGraphEcsExtension {
    /// Create a new empty ECS extension.
    pub(crate) fn new() -> Self {
        Self {
            world: World::new(),
            processor_entities: HashMap::new(),
            link_entities: HashMap::new(),
        }
    }

    // =========================================================================
    // Processor Entity Management
    // =========================================================================

    /// Ensure a processor has an entity in the ECS world.
    pub(crate) fn ensure_processor_entity(&mut self, id: &ProcessorId) -> Entity {
        if let Some(&entity) = self.processor_entities.get(id) {
            entity
        } else {
            let entity = self.world.spawn(());
            self.processor_entities.insert(id.clone(), entity);
            entity
        }
    }

    /// Get the entity for a processor, if it exists.
    pub(crate) fn get_processor_entity(&self, id: &ProcessorId) -> Option<Entity> {
        self.processor_entities.get(id).copied()
    }

    /// Remove a processor's entity from the ECS world.
    pub(crate) fn remove_processor_entity(&mut self, id: &ProcessorId) -> Option<Entity> {
        if let Some(entity) = self.processor_entities.remove(id) {
            let _ = self.world.despawn(entity);
            Some(entity)
        } else {
            None
        }
    }

    /// Get all processor IDs with entities.
    pub(crate) fn processor_ids(&self) -> impl Iterator<Item = &ProcessorId> {
        self.processor_entities.keys()
    }

    /// Get number of processors with entities.
    pub(crate) fn processor_entity_count(&self) -> usize {
        self.processor_entities.len()
    }

    // =========================================================================
    // Processor Component Operations
    // =========================================================================

    /// Attach a component to a processor.
    pub(crate) fn insert_processor_component<C: Component>(
        &mut self,
        id: &ProcessorId,
        component: C,
    ) -> Result<()> {
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
    pub(crate) fn get_processor_component<C: Component>(
        &self,
        id: &ProcessorId,
    ) -> Option<hecs::Ref<'_, C>> {
        let entity = self.processor_entities.get(id)?;
        self.world.get::<&C>(*entity).ok()
    }

    /// Get a mutable component for a processor.
    pub(crate) fn get_processor_component_mut<C: Component>(
        &mut self,
        id: &ProcessorId,
    ) -> Option<hecs::RefMut<'_, C>> {
        let entity = self.processor_entities.get(id)?;
        self.world.get::<&mut C>(*entity).ok()
    }

    /// Remove a component from a processor.
    pub(crate) fn remove_processor_component<C: Component>(
        &mut self,
        id: &ProcessorId,
    ) -> Option<C> {
        let entity = self.processor_entities.get(id)?;
        self.world.remove_one::<C>(*entity).ok()
    }

    /// Check if a processor has a component.
    pub(crate) fn processor_has_component<C: Component>(&self, id: &ProcessorId) -> bool {
        self.processor_entities
            .get(id)
            .map(|e| self.world.get::<&C>(*e).is_ok())
            .unwrap_or(false)
    }

    // =========================================================================
    // Link Entity Management
    // =========================================================================

    /// Ensure a link has an entity in the ECS world.
    pub(crate) fn ensure_link_entity(&mut self, id: &LinkId) -> Entity {
        if let Some(&entity) = self.link_entities.get(id) {
            entity
        } else {
            let entity = self.world.spawn(());
            self.link_entities.insert(id.clone(), entity);
            entity
        }
    }

    /// Get the entity for a link, if it exists.
    pub(crate) fn get_link_entity(&self, id: &LinkId) -> Option<Entity> {
        self.link_entities.get(id).copied()
    }

    /// Remove a link's entity from the ECS world.
    pub(crate) fn remove_link_entity(&mut self, id: &LinkId) -> Option<Entity> {
        if let Some(entity) = self.link_entities.remove(id) {
            let _ = self.world.despawn(entity);
            Some(entity)
        } else {
            None
        }
    }

    /// Get number of links with entities.
    #[allow(dead_code)]
    pub(crate) fn link_entity_count(&self) -> usize {
        self.link_entities.len()
    }

    // =========================================================================
    // Link Component Operations
    // =========================================================================

    /// Insert a component on a link entity.
    pub(crate) fn insert_link_component<C: Component>(
        &mut self,
        id: &LinkId,
        component: C,
    ) -> Result<()> {
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
    pub(crate) fn remove_link_component<C: Component>(&mut self, id: &LinkId) -> Result<()> {
        let entity = *self
            .link_entities
            .get(id)
            .ok_or_else(|| StreamError::NotFound(format!("Link '{}' not found", id)))?;

        let _ = self.world.remove_one::<C>(entity);
        Ok(())
    }

    /// Get a component from a link entity.
    pub(crate) fn get_link_component<C: Component>(&self, id: &LinkId) -> Option<hecs::Ref<'_, C>> {
        let entity = self.link_entities.get(id)?;
        self.world.get::<&C>(*entity).ok()
    }

    /// Get the state of a link from its ECS component.
    pub(crate) fn get_link_state(&self, id: &LinkId) -> Option<LinkState> {
        let entity = self.link_entities.get(id)?;
        self.world
            .get::<&LinkStateComponent>(*entity)
            .ok()
            .map(|c| c.0)
    }

    /// Set the state of a link via its ECS component.
    pub(crate) fn set_link_state(&mut self, id: &LinkId, state: LinkState) -> Result<()> {
        let entity = *self
            .link_entities
            .get(id)
            .ok_or_else(|| StreamError::NotFound(format!("Link '{}' not found", id)))?;

        let has_component = self.world.get::<&LinkStateComponent>(entity).is_ok();

        if has_component {
            if let Ok(mut comp) = self.world.get::<&mut LinkStateComponent>(entity) {
                comp.0 = state;
            }
        } else {
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
    pub(crate) fn processors_with_component<C: Component>(&self) -> Vec<ProcessorId> {
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
    pub(crate) fn clear(&mut self) {
        self.world.clear();
        self.processor_entities.clear();
        self.link_entities.clear();
    }

    // =========================================================================
    // Direct World Access (for advanced operations)
    // =========================================================================

    /// Get direct access to the hecs World for advanced queries.
    ///
    /// This is an escape hatch for operations not covered by the standard API.
    pub(crate) fn world(&self) -> &World {
        &self.world
    }

    /// Get mutable access to the hecs World for advanced operations.
    #[allow(dead_code)]
    pub(crate) fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Get the processor entities map for iteration.
    pub(crate) fn processor_entities(&self) -> &HashMap<ProcessorId, Entity> {
        &self.processor_entities
    }

    /// Get the link entities map for iteration.
    pub(crate) fn link_entities(&self) -> &HashMap<LinkId, Entity> {
        &self.link_entities
    }
}

impl Default for InternalProcessorLinkGraphEcsExtension {
    fn default() -> Self {
        Self::new()
    }
}
