# LinkInstanceManager → ECS Migration Plan

## Overview

Migrate link instance storage from `LinkInstanceManager` (HashMap-based) to ECS components on link entities in `PropertyGraph`, mirroring how processors are handled.

## Current Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     CURRENT STATE                               │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  StreamRuntime                                                  │
│  ├── graph: PropertyGraph                                       │
│  │   ├── link_entities: HashMap<LinkId, Entity>  ← ECS entity   │
│  │   └── (LinkStateComponent on entities)        ← Only state   │
│  │                                                              │
│  └── link_instance_manager: LinkInstanceManager  ← SEPARATE     │
│      ├── metadata: HashMap<LinkId, LinkMetadata>                │
│      ├── instances: HashMap<LinkId, BoxedLinkInstance>          │
│      ├── source_index: HashMap<LinkPortAddress, Vec<LinkId>>    │
│      └── dest_index: HashMap<LinkPortAddress, LinkId>           │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

**Problems:**
1. Dual storage - link data split between ECS and HashMap
2. `LinkInstanceManager` passed separately through compile pipeline
3. Query methods on `LinkInstanceManager` duplicate what ECS could provide
4. Inconsistent with processor pattern (fully ECS-based)

## Target Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     TARGET STATE                                │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  StreamRuntime                                                  │
│  └── graph: PropertyGraph                                       │
│      └── link_entities: HashMap<LinkId, Entity>                 │
│          │                                                      │
│          └── ECS Components per link entity:                    │
│              ├── LinkStateComponent (existing)                  │
│              ├── LinkInstanceComponent (NEW)                    │
│              │   └── BoxedLinkInstance                          │
│              └── LinkMetadataComponent (NEW)                    │
│                  ├── source: LinkPortAddress                    │
│                  ├── dest: LinkPortAddress                      │
│                  ├── type_id: TypeId                            │
│                  ├── type_name: &'static str                    │
│                  └── capacity: usize                            │
│                                                                 │
│  NO MORE: link_instance_manager field                           │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

## Changes Breakdown

### 1. New ECS Components

**File:** `libs/streamlib/src/core/links/graph/link_instance_component.rs` (NEW)

```rust
/// ECS component storing the link instance (ring buffer ownership).
pub struct LinkInstanceComponent(pub BoxedLinkInstance);

/// ECS component storing link metadata.
pub struct LinkMetadataComponent {
    pub source: LinkPortAddress,
    pub dest: LinkPortAddress,
    pub type_id: TypeId,
    pub type_name: &'static str,
    pub capacity: usize,
}
```

### 2. PropertyGraph Extensions

**File:** `libs/streamlib/src/core/graph/property_graph.rs`

Add query methods that replace `LinkInstanceManager` functionality:

```rust
impl PropertyGraph {
    /// Check if a destination port already has a link (1-to-1 enforcement).
    pub fn is_dest_port_linked(&self, dest: &LinkPortAddress) -> bool {
        // Query all link entities for LinkMetadataComponent where dest matches
    }

    /// Get link ID by destination port.
    pub fn get_link_by_dest_port(&self, dest: &LinkPortAddress) -> Option<LinkId> {
        // Query link entities
    }

    /// Get all link IDs for a source port (fan-out query).
    pub fn get_links_by_source_port(&self, source: &LinkPortAddress) -> Vec<LinkId> {
        // Query link entities
    }

    /// Get link metadata.
    pub fn get_link_metadata(&self, link_id: &LinkId) -> Option<&LinkMetadataComponent> {
        self.get_link::<LinkMetadataComponent>(link_id)
    }

    /// Check if link has an active instance.
    pub fn has_link_instance(&self, link_id: &LinkId) -> bool {
        self.get_link::<LinkInstanceComponent>(link_id).is_some()
    }

    /// Get active link instance count.
    pub fn link_instance_count(&self) -> usize {
        // Query count of entities with LinkInstanceComponent
    }
}
```

### 3. Link Factory

**File:** `libs/streamlib/src/core/links/link_factory.rs` (NEW)

Mirrors `FactoryDelegate` for processors:

```rust
/// Creates LinkInstance from Link metadata.
pub trait LinkFactoryDelegate: Send + Sync {
    /// Create a link instance, returning data writer and reader.
    fn create_link_instance(
        &self,
        link: &Link,
        source: &LinkPortAddress,
        dest: &LinkPortAddress,
        capacity: usize,
    ) -> Result<(BoxedLinkInstance, Box<dyn Any>, Box<dyn Any>)>;
    // Returns: (instance for storage, writer as Any, reader as Any)
}

/// Default implementation that creates ring buffer based on port type.
pub struct DefaultLinkFactory;

impl LinkFactoryDelegate for DefaultLinkFactory {
    fn create_link_instance(...) -> Result<...> {
        // Dispatch based on port type (Audio, Video, Data)
        // Create LinkInstance<T> and return boxed writer/reader
    }
}
```

### 4. Wiring Changes

**File:** `libs/streamlib/src/core/compiler/wiring.rs`

Change signature from:
```rust
pub fn wire_link(
    property_graph: &mut PropertyGraph,
    link_instance_manager: &mut LinkInstanceManager,  // REMOVE
    link_id: &LinkId,
) -> Result<()>
```

To:
```rust
pub fn wire_link(
    property_graph: &mut PropertyGraph,
    link_factory: &dyn LinkFactoryDelegate,  // NEW
    link_id: &LinkId,
) -> Result<()>
```

Inside `wire_link`:
1. Call `link_factory.create_link_instance(...)` 
2. Store `LinkInstanceComponent` on link entity via `property_graph.insert_link(...)`
3. Store `LinkMetadataComponent` on link entity
4. Wire data writer/reader to processors (unchanged)

### 5. Compiler Pipeline Changes

**File:** `libs/streamlib/src/core/compiler/pipeline.rs`

Change `Compiler` struct:
```rust
pub struct Compiler {
    factory: Arc<dyn FactoryDelegate>,
    processor_delegate: Arc<dyn ProcessorDelegate>,
    scheduler: Arc<dyn SchedulerDelegate>,
    link_factory: Arc<dyn LinkFactoryDelegate>,  // NEW
}
```

Remove `link_instance_manager` parameter from all methods:
- `compile()`
- `phase_wire()`
- `handle_removals()`

### 6. Runtime Changes

**File:** `libs/streamlib/src/core/runtime/runtime.rs`

Remove field:
```rust
pub struct StreamRuntime {
    // REMOVE: link_instance_manager: LinkInstanceManager,
    link_factory: Arc<dyn LinkFactoryDelegate>,  // NEW
}
```

Update `commit()` to not pass `link_instance_manager`.

Update `disconnect()`:
```rust
pub fn disconnect(&mut self, link: &Link) -> Result<()> {
    // Instead of: self.link_instance_manager.disconnect(link_id)
    // Do: Remove LinkInstanceComponent from entity (instance drops, handles degrade)
    let mut property_graph = self.graph.write();
    property_graph.remove_link_component::<LinkInstanceComponent>(&link.id)?;
    property_graph.set_link_state(&link.id, LinkState::Disconnected)?;
}
```

### 7. Delete LinkInstanceManager

**File:** `libs/streamlib/src/core/links/link_instance_manager.rs` (DELETE)

Remove entirely. All functionality moved to:
- `PropertyGraph` (queries)
- `LinkFactoryDelegate` (creation)
- ECS components (storage)

### 8. Update Exports

**File:** `libs/streamlib/src/core/links/mod.rs`

```rust
// REMOVE: pub mod link_instance_manager;
// REMOVE: pub use link_instance_manager::LinkInstanceManager;

// ADD:
pub mod link_factory;
pub use link_factory::{LinkFactoryDelegate, DefaultLinkFactory};
```

## Migration Checklist

- [ ] Create `LinkInstanceComponent` and `LinkMetadataComponent`
- [ ] Add query methods to `PropertyGraph`
- [ ] Create `LinkFactoryDelegate` trait and `DefaultLinkFactory`
- [ ] Update `Compiler` to use `LinkFactoryDelegate`
- [ ] Update `wiring.rs` to store components on ECS
- [ ] Update `StreamRuntime` to remove `link_instance_manager`
- [ ] Update `RuntimeBuilder` 
- [ ] Delete `link_instance_manager.rs`
- [ ] Update exports in `links/mod.rs`
- [ ] Migrate tests from `LinkInstanceManager` to new pattern
- [ ] Run full test suite

## Test Coverage

Existing tests in `link_instance_manager.rs` need migration:
- `test_create_link_instance` → Test via `wire_link` + PropertyGraph queries
- `test_link_instance_graceful_degradation` → Test disconnect removes component
- `test_one_to_one_enforcement` → Test `is_dest_port_linked()` query
- `test_multiple_outputs_allowed` → Test `get_links_by_source_port()` query
- `test_link_count` → Test `link_instance_count()` query

## Benefits After Migration

1. **Single source of truth** - All link data in PropertyGraph ECS
2. **Consistent pattern** - Links mirror processor storage pattern
3. **Simpler compile pipeline** - No separate manager to pass around
4. **ECS queries** - Can query links by any component criteria
5. **Future extensibility** - Easy to add more link components (metrics, etc.)
