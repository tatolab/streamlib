# ECS Removal Migration Plan

## Overview

Remove the `hecs` ECS system and embed dynamic component storage directly into petgraph node/edge weights. After migration:

- **ProcessorNode IS the entity** - not "has an entity reference"
- **Link IS the entity** - not "has an entity reference"
- Each weight contains a `TypeMap` for dynamic component storage
- No secondary indexing - all lookups traverse the graph via ID scan
- IDs are random CUIDs (10 characters), never reused
- Queries are read-only; mutations happen via direct weight access

## Success Criteria

1. ✅ The `hecs` ECS system is fully removed (no hecs imports, types, or wrappers)
2. ✅ Pure petgraph `DiGraph`-based solution with no external component storage
3. ✅ No secondary indexing for traversal (no HashMaps mapping ID → NodeIndex)
4. ✅ All data embedded directly in node/link weights via TypeMap
5. ✅ JSON output unchanged - components appear as node/link properties
6. ✅ No string-based port address traversal patterns
7. ✅ Random CUID-based IDs (10 characters, never reused)
8. ✅ All regression tests pass (see [regression tests](./ecs-removal-regression-tests.md))

## Regression Test Plan

See [ecs-removal-regression-tests.md](./ecs-removal-regression-tests.md) for detailed test specifications.

**Summary: 61 regression tests across 7 categories**

**Test Approach:**
1. Identify what each existing ECS-related test was testing
2. Remove old test completely
3. Create new test for equivalent behavior in new architecture
4. Never modify tests to "make them pass" - that creates a race to the bottom

---

## Architecture Changes

### Before: Separate ECS World

```
Graph
├── processor_link_graph: Arc<RwLock<InternalProcessorLinkGraph>>
│   └── DiGraph<ProcessorNode, Link>  ← topology only, no components
│
└── ecs_extension: InternalProcessorLinkGraphEcsExtension
    ├── world: hecs::World            ← component storage
    ├── processor_entities: HashMap<ProcessorId, Entity>  ← secondary index
    └── link_entities: HashMap<LinkId, Entity>            ← secondary index
```

### After: Embedded Component Storage

```
Graph
└── graph: DiGraph<ProcessorNode, Link>
    │
    ├── ProcessorNode (weight) implements GraphWeight + GraphNode
    │   ├── id: String (CUID, 10 chars, never reused)
    │   ├── processor_type: String
    │   ├── config: Option<Value>
    │   ├── config_checksum: u64
    │   ├── ports: NodePorts
    │   └── components: TypeMap        ← embedded component storage
    │
    └── Link (weight) implements GraphWeight + GraphEdge
        ├── id: String (CUID, 10 chars, never reused)
        ├── source: LinkEndpoint
        ├── target: LinkEndpoint
        ├── capacity: usize
        └── components: TypeMap        ← embedded component storage
```

### Graph Type Decision: DiGraph

We evaluated petgraph options:

| Type | Index Stability | Index Reuse | Fits Us? |
|------|-----------------|-------------|----------|
| `DiGraph` | Indices shift on remove | Reused via swap-remove | ✅ Use this |
| `StableGraph` | Stable after remove | Reused via free list | ❌ Still reuses |
| `GraphMap` | N/A (node=weight) | N/A | ❌ Requires `Copy` |

**Decision:** Use `DiGraph`. Neither petgraph type guarantees "index gone forever" - they all reuse indices. Our CUID-based `id` field in the weight is the only truly stable, never-reused identifier. `NodeIndex` is an internal implementation detail, never exposed to callers.

---

## Trait Hierarchy

Unified trait structure for graph weights:

```rust
/// Base trait for all graph weights (nodes and edges).
pub trait GraphWeight {
    /// Get the unique identifier for this weight.
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
    fn insert<C: Send + Sync + 'static>(&mut self, component: C);
    fn get<C: Send + Sync + 'static>(&self) -> Option<&C>;
    fn get_mut<C: Send + Sync + 'static>(&mut self) -> Option<&mut C>;
    fn remove<C: Send + Sync + 'static>(&mut self) -> Option<C>;
    fn has<C: Send + Sync + 'static>(&self) -> bool;
}
```

**Decisions:**
- `ProcessorId` and `LinkId` type aliases removed - both are just `String`
- Unified `id()` method provides consistent access
- Traits go in `core/graph/traits.rs`
- Rename `node.rs` → `processor_node.rs` for clarity

---

## Dependencies

### TypeMap Crate: `anymap2`

Selected [anymap2](https://crates.io/crates/anymap2):
- Fork of well-established `anymap` with active maintenance
- Supports `Send + Sync` constraints (thread safety)
- Minimal API, lightweight, battle-tested

```toml
[dependencies]
anymap2 = "0.13"
```

### ID Generation: `cuid2`

Selected [cuid2](https://crates.io/crates/cuid2):
- Collision-resistant unique identifiers
- 10-character slugs via `cuid2_slug()`
- Cryptographically secure, no coordination needed
- **IDs are never reused** (unlike petgraph indices)

```toml
[dependencies]
cuid2 = "0.1"
```

---

## Access Patterns

### Queries: Read-Only

Queries find nodes/edges and return IDs or cloned data:

```rust
// Find processors by type
let ids = graph.execute(&Query::build().V().of_type("H264Encoder").ids());

// Get node data (cloned)
let nodes = graph.execute(&Query::build().V().of_type("H264Encoder").nodes());
```

### Mutations: Direct Weight Access

After finding an ID via query, mutate via direct weight access:

```rust
// Get mutable weight and operate on it
if let Some(node) = graph.processor_mut("abc123") {
    node.insert(StateComponent::default());
    node.get_mut::<ProcessorMetrics>().map(|m| m.frames_processed += 1);
}
```

**Graph methods for weight access:**

```rust
impl Graph {
    /// Get immutable processor node by ID.
    pub fn processor(&self, id: &str) -> Option<&ProcessorNode>;
    
    /// Get mutable processor node by ID.
    pub fn processor_mut(&mut self, id: &str) -> Option<&mut ProcessorNode>;
    
    /// Get immutable link by ID.
    pub fn link(&self, id: &str) -> Option<&Link>;
    
    /// Get mutable link by ID.
    pub fn link_mut(&mut self, id: &str) -> Option<&mut Link>;
}
```

These internally scan the graph by ID to find the `NodeIndex`/`EdgeIndex`, then use petgraph's `node_weight_mut()`/`edge_weight_mut()`.

### Removed: Graph-Level Component Methods

These convenience methods are **removed** - callers access components via the weight directly:

```rust
// REMOVED - no longer exists
graph.insert(id, component);
graph.get::<C>(id);
graph.get_mut::<C>(id);

// NEW - access weight, then component
graph.processor_mut(id)?.insert(component);
graph.processor(id)?.get::<C>();
graph.processor_mut(id)?.get_mut::<C>();
```

---

## Files to Modify

### Core Graph Files

| File | Action | Changes |
|------|--------|---------|
| `core/graph/node.rs` | RENAME+MODIFY | → `processor_node.rs`, add `components: TypeMap`, impl traits |
| `core/graph/link.rs` | MODIFY | Add `components: TypeMap`, impl `GraphWeight` + `GraphEdge` |
| `core/graph/traits.rs` | CREATE | `GraphWeight`, `GraphNode`, `GraphEdge` traits |
| `core/graph/graph.rs` | MODIFY | Remove ecs_extension, add `processor_mut`/`link_mut` |
| `core/graph/internal/processor_link_graph.rs` | MODIFY | CUID generation, weight access methods |
| `core/graph/internal/processor_link_graph_ecs_extension.rs` | DELETE | No longer needed |
| `core/graph/internal/mod.rs` | MODIFY | Remove ecs_extension export |
| `core/graph/components.rs` | MODIFY | Remove hecs imports, keep component types |
| `core/graph/mod.rs` | MODIFY | Export new traits, remove Entity, update re-exports |

### Compiler Files

| File | Action | Changes |
|------|--------|---------|
| `core/compiler/phases.rs` | MODIFY | Use `graph.processor_mut(id)?.insert(...)` |
| `core/compiler/wiring.rs` | MODIFY | Use `graph.link_mut(id)?.insert(...)` |

### Runtime Files

| File | Action | Changes |
|------|--------|---------|
| `core/runtime/runtime.rs` | MODIFY | Use `graph.link(id)?.has::<C>()` for checks |

### Links Files

| File | Action | Changes |
|------|--------|---------|
| `core/links/graph/link_state_ecs_component.rs` | MODIFY | Remove duplicate, keep LinkState enum |
| `core/links/graph/link_instance_component.rs` | MODIFY | Remove hecs references |

### Query Files

| File | Action | Changes |
|------|--------|---------|
| `core/graph/query/field_resolver.rs` | MODIFY | Access components from weights directly |

### Dependencies

| File | Action | Changes |
|------|--------|---------|
| `Cargo.toml` | MODIFY | Remove `hecs`, add `anymap2`, `cuid2` |

---

## Existing Tests to Remove and Replace

| Old Test | What It Tested | New Test |
|----------|----------------|----------|
| `test_property_graph_entity_management` | `ensure/get/remove_processor_entity` | `test_processor_component_lifecycle` |
| `test_property_graph_components` | Components with entity setup first | `test_processor_component_operations` |

---

## Implementation Tasks

### Phase 1: Preparation

1. Add `anymap2` and `cuid2` dependencies
2. Create `core/graph/traits.rs` with trait hierarchy
3. Write regression tests against current implementation

### Phase 2: Embed Storage in Weights

1. Rename `node.rs` → `processor_node.rs`
2. Add `components: TypeMap` to `ProcessorNode`, impl traits
3. Add `components: TypeMap` to `Link`, impl traits
4. Update ID generation to use `cuid2_slug()`

### Phase 3: Migrate Graph API

1. Add `processor()`, `processor_mut()`, `link()`, `link_mut()` methods
2. Remove `ensure_*_entity()`, `get_*_entity()`, `remove_*_entity()` methods
3. Remove `insert()`, `get()`, `get_mut()`, `remove()`, `has()` convenience methods
4. Delete `processor_link_graph_ecs_extension.rs`

### Phase 4: Update Callers

1. Update compiler phases to use new access pattern
2. Update wiring to use new access pattern
3. Update runtime to use new access pattern
4. Update query field resolver

### Phase 5: Cleanup

1. Remove `hecs` from Cargo.toml
2. Remove all `hecs::` imports
3. Remove `ProcessorId`/`LinkId` type aliases (use `String`)
4. Run all tests, fix any remaining issues

---

## API Changes Summary

### Removed

| Item | Reason |
|------|--------|
| `ensure_processor_entity(id)` | Entity concept removed |
| `get_processor_entity(id)` | Entity concept removed |
| `remove_processor_entity(id)` | Entity concept removed |
| `ensure_link_entity(id)` | Entity concept removed |
| `get_link_entity(id)` | Entity concept removed |
| `remove_link_entity(id)` | Entity concept removed |
| `graph.insert(id, component)` | Use `processor_mut(id)?.insert(c)` |
| `graph.get::<C>(id)` | Use `processor(id)?.get::<C>()` |
| `graph.get_mut::<C>(id)` | Use `processor_mut(id)?.get_mut::<C>()` |
| `ProcessorId` type alias | Use `String` directly |
| `LinkId` type alias | Use `String` directly |

### Added

| Item | Purpose |
|------|---------|
| `graph.processor(id)` | Get immutable node reference |
| `graph.processor_mut(id)` | Get mutable node reference |
| `graph.link(id)` | Get immutable edge reference |
| `graph.link_mut(id)` | Get mutable edge reference |
| `GraphWeight` trait | Common `id()` access |
| `GraphNode` trait | Node component operations |
| `GraphEdge` trait | Edge component operations |

---

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| anymap2 limitations | Well-tested fork of anymap, minimal API |
| TypeMap serialization | `#[serde(skip)]` - runtime components shouldn't serialize |
| Thread safety | Graph uses `Arc<RwLock<>>`, components are `Send + Sync` |
| O(n) ID scan performance | Acceptable now; can add caching layer later |
