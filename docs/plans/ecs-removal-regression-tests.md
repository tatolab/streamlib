# ECS Removal - Regression Test Plan

This document details all regression tests required for the ECS removal migration.
See [ecs-removal-plan.md](./ecs-removal-plan.md) for the full migration plan.

## Test Approach

**Critical Rule: Never modify tests to make them pass.**

For each existing test that relies on ECS:
1. **Identify** what behavior the test was validating
2. **Remove** the old test completely
3. **Create** a new test for equivalent behavior in new architecture

This prevents a "race to the bottom" where we twist tests to fit implementation rather than validating correct behavior.

## Test Summary

| Category | Count |
|----------|-------|
| ProcessorNode Component Tests | 12 |
| Link Component Tests | 10 |
| Graph API Tests | 14 |
| Compiler Phase Tests | 8 |
| Runtime Tests | 6 |
| Query Interface Tests | 5 |
| Serialization Tests | 6 |
| **Total** | **61** |

---

## Existing Tests to Remove and Replace

These tests in `graph.rs` rely on ECS entity semantics and must be replaced:

| Old Test | What It Tested | New Test | What It Should Test |
|----------|----------------|----------|---------------------|
| `test_property_graph_entity_management` | `ensure_processor_entity`, `get_processor_entity`, `remove_processor_entity` | `test_processor_component_storage_lifecycle` | Insert/get/remove components on node weight without entity concept |
| `test_property_graph_components` | Insert/get/has/remove with `ensure_processor_entity` first | `test_processor_component_operations` | Same operations but components are part of node, no entity setup |

---

## 1. ProcessorNode Component Tests

Tests verifying component storage works correctly on ProcessorNode weights.

| # | Test Name | Description | Pre-Migration Behavior |
|---|-----------|-------------|------------------------|
| 1.1 | `test_processor_node_insert_component` | Insert a component onto a processor node | `graph.insert(id, Component)` stores via ECS entity |
| 1.2 | `test_processor_node_get_component` | Retrieve a component from a processor node | `graph.get::<C>(id)` returns `hecs::Ref` |
| 1.3 | `test_processor_node_get_mut_component` | Mutate a component on a processor node | `graph.get_mut::<C>(id)` returns `hecs::RefMut` |
| 1.4 | `test_processor_node_remove_component` | Remove a component from a processor node | `graph.remove::<C>(id)` returns owned component |
| 1.5 | `test_processor_node_has_component` | Check if processor has a specific component | `graph.has::<C>(id)` returns bool |
| 1.6 | `test_processor_node_multiple_components` | Store multiple component types on same node | Each type stored independently |
| 1.7 | `test_processor_node_component_overwrite` | Overwrite existing component of same type | Second insert replaces first |
| 1.8 | `test_processor_node_component_after_removal` | Access components after processor removed from graph | Components gone with processor |
| 1.9 | `test_processor_node_state_component` | StateComponent stores ProcessorState correctly | Arc<Mutex<ProcessorState>> accessible |
| 1.10 | `test_processor_node_pause_gate` | ProcessorPauseGate atomic operations work | is_paused/pause/resume work correctly |
| 1.11 | `test_processor_node_instance_component` | ProcessorInstance stores Arc<Mutex<BoxedProcessor>> | Can lock and call processor methods |
| 1.12 | `test_processor_node_shutdown_channel` | ShutdownChannel sender/receiver work | Can send shutdown signal, receiver receives |

---

## 2. Link Component Tests

Tests verifying component storage works correctly on Link weights.

| # | Test Name | Description | Pre-Migration Behavior |
|---|-----------|-------------|------------------------|
| 2.1 | `test_link_insert_component` | Insert a component onto a link | `graph.insert_link(id, Component)` stores via ECS entity |
| 2.2 | `test_link_get_component` | Retrieve a component from a link | `graph.get_link_component::<C>(id)` returns `hecs::Ref` |
| 2.3 | `test_link_remove_component` | Remove a component from a link | `graph.remove_link_component::<C>(id)` succeeds |
| 2.4 | `test_link_state_get_set` | Get and set link state | `get_link_state`/`set_link_state` work |
| 2.5 | `test_link_state_transitions` | Link state transitions (Pending→Wired→Disconnected) | State changes correctly |
| 2.6 | `test_link_instance_component` | LinkInstanceComponent stores ring buffer | Can access buffer fill level |
| 2.7 | `test_link_type_info_component` | LinkTypeInfoComponent stores type metadata | type_name, capacity accessible |
| 2.8 | `test_link_component_after_removal` | Access components after link removed | Components gone with link |
| 2.9 | `test_link_multiple_components` | Store multiple component types on same link | Each type stored independently |
| 2.10 | `test_link_pending_deletion_marker` | PendingDeletion component marks link for removal | Component presence detectable |

---

## 3. Graph API Tests

Tests verifying the Graph public API continues to work.

| # | Test Name | Description | Pre-Migration Behavior |
|---|-----------|-------------|------------------------|
| 3.1 | `test_graph_add_processor_creates_entity` | add_processor creates storage for components | Entity created, can insert components |
| 3.2 | `test_graph_add_link_creates_entity` | add_link creates storage for components | Entity created, can insert components |
| 3.3 | `test_graph_remove_processor_cleans_components` | remove_processor removes all components | No components remain for removed processor |
| 3.4 | `test_graph_remove_link_cleans_components` | remove_link removes all components | No components remain for removed link |
| 3.5 | `test_graph_processors_with_component` | Find all processors with a specific component type | `processors_with::<C>()` returns correct IDs |
| 3.6 | `test_graph_clear_entities` | clear_entities removes all component storage | All components cleared |
| 3.7 | `test_graph_entity_count` | entity_count returns correct count | Matches number of processors with components |
| 3.8 | `test_graph_processor_ids` | processor_ids returns all IDs with components | Iterator yields all registered IDs |
| 3.9 | `test_graph_needs_recompile` | needs_recompile detects graph changes | Returns true after modifications |
| 3.10 | `test_graph_mark_compiled` | mark_compiled updates checksum | needs_recompile returns false after |
| 3.11 | `test_graph_state_transitions` | GraphState (Idle/Running/Paused/Stopping) | state()/set_state() work correctly |
| 3.12 | `test_graph_to_json_includes_components` | to_json serializes component data | Components appear as node/link properties |
| 3.13 | `test_graph_to_dot_includes_state` | to_dot includes state info in labels | State visible in DOT output |
| 3.14 | `test_graph_concurrent_read_access` | Multiple readers can access simultaneously | RwLock allows concurrent reads |

---

## 4. Compiler Phase Tests

Tests verifying compiler phases work correctly with new component storage.

| # | Test Name | Description | Pre-Migration Behavior |
|---|-----------|-------------|------------------------|
| 4.1 | `test_phase_create_attaches_components` | CREATE phase attaches all required components | ProcessorInstance, ShutdownChannel, StateComponent, etc. |
| 4.2 | `test_phase_setup_accesses_instance` | SETUP phase retrieves ProcessorInstance | Can lock and call __generated_setup |
| 4.3 | `test_phase_start_spawns_thread` | START phase attaches ThreadHandle | Thread running, handle stored |
| 4.4 | `test_phase_start_updates_state` | START phase sets state to Running | StateComponent shows Running |
| 4.5 | `test_shutdown_sends_signal` | shutdown_processor sends via ShutdownChannel | Receiver gets signal |
| 4.6 | `test_shutdown_joins_thread` | shutdown_processor joins ThreadHandle | Thread joined successfully |
| 4.7 | `test_shutdown_updates_state` | shutdown_processor sets state to Stopped | StateComponent shows Stopped |
| 4.8 | `test_shutdown_all_processors` | shutdown_all_processors shuts down all | All processors stopped |

---

## 5. Runtime Tests

Tests verifying runtime behavior with new component storage.

| # | Test Name | Description | Pre-Migration Behavior |
|---|-----------|-------------|------------------------|
| 5.1 | `test_runtime_commit_creates_processors` | commit() creates and starts processors | Processors running with components |
| 5.2 | `test_runtime_commit_wires_links` | commit() wires links with components | Links have LinkInstanceComponent |
| 5.3 | `test_runtime_add_link_checks_wired` | AddLink checks for existing LinkInstanceComponent | Skips already-wired links |
| 5.4 | `test_runtime_pending_deletion_check` | Checks PendingDeletion before operations | Skips pending-deletion entities |
| 5.5 | `test_runtime_hot_reload_config` | Config update triggers recompile | Component state preserved |
| 5.6 | `test_runtime_dynamic_add_remove` | Add/remove processors while running | Components managed correctly |

---

## 6. Query Interface Tests

Tests verifying query interface works with new component storage.

| # | Test Name | Description | Pre-Migration Behavior |
|---|-----------|-------------|------------------------|
| 6.1 | `test_query_where_field_state` | Query processors by state component value | `.where_field("state", ...)` works |
| 6.2 | `test_query_where_field_metrics` | Query processors by metrics values | `.where_field("metrics.throughput_fps", ...)` works |
| 6.3 | `test_query_has_field_component` | Query for presence of component field | `.has_field("paused")` works |
| 6.4 | `test_query_link_where_field` | Query links by component values | Link `.where_field(...)` works |
| 6.5 | `test_query_field_resolver_components` | FieldResolver extracts component JSON | Components in to_json output |

---

## 7. Serialization Tests

Tests verifying serialization works with embedded components.

| # | Test Name | Description | Pre-Migration Behavior |
|---|-----------|-------------|------------------------|
| 7.1 | `test_processor_node_serialize` | ProcessorNode serializes to JSON | All fields present |
| 7.2 | `test_processor_node_deserialize` | ProcessorNode deserializes from JSON | All fields restored |
| 7.3 | `test_link_serialize` | Link serializes to JSON | All fields present |
| 7.4 | `test_link_deserialize` | Link deserializes from JSON | All fields restored |
| 7.5 | `test_graph_serialize_roundtrip` | Full graph serialize/deserialize | Topology and static data preserved |
| 7.6 | `test_components_excluded_from_serialization` | Runtime components not serialized | Only static data in JSON |

---

## Test Implementation Strategy

### Phase 1: Write All Tests Against Current API
1. Create `tests/ecs_regression.rs` with all 61 tests
2. Tests use current `hecs`-based API
3. Verify all tests pass before migration

### Phase 2: Update Tests for New API
1. Change `hecs::Ref`/`RefMut` to direct references
2. Remove entity-related assertions
3. Update component access patterns

### Phase 3: Run Tests After Migration
1. All 61 tests must pass
2. No API behavior changes visible to callers
3. Performance characteristics maintained

---

## Notes on Traits

Post-migration, we use a unified trait hierarchy instead of `hecs::Component`:

```rust
/// Base trait for all graph weights.
pub trait GraphWeight {
    fn id(&self) -> &str;
}

/// Trait for node weights (processors).
pub trait GraphNode: GraphWeight {
    fn insert<C: Send + Sync + 'static>(&mut self, component: C);
    fn get<C: Send + Sync + 'static>(&self) -> Option<&C>;
    fn get_mut<C: Send + Sync + 'static>(&mut self) -> Option<&mut C>;
    fn remove<C: Send + Sync + 'static>(&mut self) -> Option<C>;
    fn has<C: Send + Sync + 'static>(&self) -> bool;
}

/// Trait for edge weights (links).
pub trait GraphEdge: GraphWeight {
    // Same methods as GraphNode
}
```

Components just need to be `Send + Sync + 'static` - no special trait implementation required.
