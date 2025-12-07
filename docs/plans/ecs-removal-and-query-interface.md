# ECS Removal and Query Interface Plan

## Overview

Remove hecs ECS and embed component storage directly into petgraph node/edge weights. All access to nodes, edges, and their components MUST go through the query interface.

## Architecture

### Component Storage

Components are stored directly in node/edge weights via `anymap2::Map`:

```rust
// ProcessorNode and Link each have embedded TypeMap
type ComponentMap = Map<dyn anymap2::any::Any + Send + Sync>;

pub struct ProcessorNode {
    pub id: ProcessorId,
    pub processor_type: String,
    // ... other fields
    components: ComponentMap,  // embedded storage
}

pub struct Link {
    pub id: LinkId,
    // ... other fields
    components: ComponentMap,  // embedded storage
}
```

### Traits

```rust
pub trait GraphWeight {
    fn id(&self) -> &str;
}

pub trait GraphNode: GraphWeight {
    fn insert<C: Send + Sync + 'static>(&mut self, component: C);
    fn get<C: Send + Sync + 'static>(&self) -> Option<&C>;
    fn get_mut<C: Send + Sync + 'static>(&mut self) -> Option<&mut C>;
    fn remove<C: Send + Sync + 'static>(&mut self) -> Option<C>;
    fn has<C: Send + Sync + 'static>(&self) -> bool;
}

pub trait GraphEdge: GraphWeight {
    // Same methods as GraphNode
}
```

## Access Patterns

### CRITICAL: All Access Through Query Interface

**No direct component access methods on Graph.** Everything goes through queries.

#### Read Access (Query Interface)

```rust
// Get node IDs matching criteria
let ids = graph.execute(&Query::build()
    .V()
    .of_type("H264Encoder")
    .in_state(ProcessorState::Running)
    .ids());

// Get full nodes
let nodes = graph.execute(&Query::build()
    .V()
    .where_field("metrics.fps", |v| v > 30.0)
    .nodes());

// Access components from returned nodes
for node in nodes {
    if let Some(metrics) = node.get::<ProcessorMetrics>() {
        println!("FPS: {}", metrics.fps);
    }
}

// Links work the same way
let links = graph.execute(&Query::build()
    .E()
    .links());
```

#### Write Access (Mutation Interface)

Mutations use callback-based access:

```rust
// Mutate a processor's components
graph.with_processor_mut(&processor_id, |node| {
    node.insert(StateComponent::new());
    node.insert(ProcessorMetrics::default());
});

// Mutate a link's components
graph.with_link_mut(&link_id, |link| {
    link.insert(LinkStateComponent(LinkState::Wired));
});
```

### What Gets Removed

1. **All direct component methods on Graph:**
   - `graph.insert()`
   - `graph.get()`
   - `graph.get_mut()`
   - `graph.remove()`
   - `graph.has()`
   - `graph.insert_link()`
   - `graph.get_link()`
   - etc.

2. **Entity management methods:**
   - `ensure_processor_entity()`
   - `get_processor_entity()`
   - `remove_processor_entity()`
   - `ensure_link_entity()`
   - `entity_count()`
   - `clear_entities()`

3. **Wrapper types:**
   - `ComponentRef`
   - `ComponentRefMut`
   - `LinkComponentRef`

4. **Internal methods:**
   - `clear_all_components()`

5. **Test files:**
   - `ecs_regression_tests.rs` (entire file)

### What Gets Added

1. **Mutation methods on Graph:**
   ```rust
   impl Graph {
       /// Mutate a processor node via callback.
       pub fn with_processor_mut<F, R>(&self, id: &ProcessorId, f: F) -> Option<R>
       where
           F: FnOnce(&mut ProcessorNode) -> R;
       
       /// Mutate a link via callback.
       pub fn with_link_mut<F, R>(&self, id: &LinkId, f: F) -> Option<R>
       where
           F: FnOnce(&mut Link) -> R;
   }
   ```

2. **Component access on nodes/edges (already exists via traits):**
   ```rust
   node.insert(component);
   node.get::<T>();
   node.get_mut::<T>();
   node.remove::<T>();
   node.has::<T>();
   ```

## Lifecycle

- Components are added/removed as needed during processor/link lifecycle
- When a processor is removed (`remove_processor`), petgraph's `remove_node` drops the weight, RAII cleans up components
- When a link is removed, same pattern with `remove_edge`
- No explicit cleanup methods needed

## Migration Tasks

1. [ ] Add `with_processor_mut` and `with_link_mut` to Graph
2. [ ] Remove all direct component access methods from Graph
3. [ ] Remove ComponentRef/ComponentRefMut/LinkComponentRef wrappers
4. [ ] Remove clear_entities, entity_count, clear_all_components
5. [ ] Remove ecs_regression_tests.rs
6. [ ] Update compiler phases to use `with_processor_mut`/`with_link_mut`
7. [ ] Update runtime to use mutation interface
8. [ ] Update any other callers
9. [ ] Run tests and fix failures
