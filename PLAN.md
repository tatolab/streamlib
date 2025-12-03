# Plan: LinkState ECS Component and Proper Disconnect Error Handling

## Problem Statement

Currently:
1. `runtime.disconnect()` doesn't validate if a link is actually wired before disconnecting
2. There's no way to query the current wiring state of a link
3. Link state isn't included in the JSON serialization (needed for React Flow visualization)
4. The 1-to-1 rule in `LinkChannelManager` is checked during connect, but there's no visibility into why a reconnect fails

## Proposed Solution

### 1. Create `LinkState` Enum

Following the pattern of `ProcessorState`, create a `LinkState` enum in `libs/streamlib/src/core/graph/link.rs`:

```rust
/// State of a link in the graph
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    /// Link exists in graph but not yet wired (pending commit)
    #[default]
    Pending,
    /// Link is actively wired with a ring buffer channel
    Wired,
    /// Link is being disconnected
    Disconnecting,
    /// Link was disconnected (will be removed from graph)
    Disconnected,
    /// Link is in error state (wiring failed)
    Error,
}
```

### 2. Add `state` Field to `Link` Struct

Update the `Link` struct to include state:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub id: LinkId,
    pub source: LinkEndpoint,
    pub target: LinkEndpoint,
    #[serde(default)]
    pub state: LinkState,
}
```

This ensures JSON serialization automatically includes the state.

### 3. Create ECS Component for Link Runtime State

In `libs/streamlib/src/core/graph/components.rs`, add:

```rust
/// Runtime state component for links (attached to link entities)
pub struct LinkStateComponent(pub LinkState);

impl Default for LinkStateComponent {
    fn default() -> Self {
        Self(LinkState::Pending)
    }
}
```

### 4. Update Error Types

Add specific error variants in `libs/streamlib/src/core/error.rs`:

```rust
#[error("Link not wired: {0}")]
LinkNotWired(String),

#[error("Link already disconnected: {0}")]
LinkAlreadyDisconnected(String),
```

### 5. Update `runtime.disconnect()` Method

In `libs/streamlib/src/core/runtime/runtime.rs`:

```rust
pub fn disconnect(&mut self, link: &Link) -> Result<()> {
    let link_id = link.id.clone();
    
    // Check current link state via ECS
    let current_state = {
        let property_graph = self.graph.read();
        property_graph.get_link_entity(&link_id)
            .and_then(|_| property_graph.get::<LinkStateComponent>(&link_id))
            .map(|c| c.0)
            .unwrap_or(LinkState::Pending)
    };
    
    match current_state {
        LinkState::Disconnected => {
            return Err(StreamError::LinkAlreadyDisconnected(link_id.to_string()));
        }
        LinkState::Pending => {
            return Err(StreamError::LinkNotWired(link_id.to_string()));
        }
        LinkState::Wired | LinkState::Disconnecting => {
            // Proceed with disconnect
        }
        LinkState::Error => {
            // Allow disconnecting errored links
        }
    }
    
    // Update state to Disconnecting
    {
        let mut property_graph = self.graph.write();
        if let Err(e) = property_graph.insert(&link_id, LinkStateComponent(LinkState::Disconnecting)) {
            tracing::warn!("Failed to update link state: {}", e);
        }
    }
    
    // ... rest of disconnect logic ...
    
    // Update state to Disconnected after successful unwire
    {
        let mut property_graph = self.graph.write();
        if let Err(e) = property_graph.insert(&link_id, LinkStateComponent(LinkState::Disconnected)) {
            tracing::warn!("Failed to update link state: {}", e);
        }
    }
    
    Ok(())
}
```

### 6. Update Compiler Wiring Phase

In `libs/streamlib/src/core/compiler/wiring.rs`, update state after successful wiring:

```rust
pub fn wire_link(...) -> Result<()> {
    // ... existing wiring logic ...
    
    // Update link state to Wired
    property_graph.ensure_link_entity(link_id);
    property_graph.insert_link(link_id, LinkStateComponent(LinkState::Wired))?;
    
    Ok(())
}
```

### 7. Add Link State Query API

In `PropertyGraph`, add method to query link state:

```rust
/// Get the current state of a link
pub fn get_link_state(&self, link_id: &LinkId) -> Option<LinkState> {
    self.get_link_entity(link_id)?;
    self.world
        .get::<&LinkStateComponent>(self.link_entities.get(link_id)?.clone())
        .ok()
        .map(|c| c.0)
}
```

### 8. Update JSON Serialization

The `Link` struct already derives `Serialize`, so adding `state: LinkState` will automatically include it in JSON output:

```json
{
  "links": [
    {
      "id": "link_0",
      "source": { "node": "source_0", "port": "output", "direction": "output" },
      "target": { "node": "sink_0", "port": "input", "direction": "input" },
      "state": "wired"
    }
  ]
}
```

## Files to Modify

1. `libs/streamlib/src/core/graph/link.rs` - Add `LinkState` enum and `state` field
2. `libs/streamlib/src/core/graph/components.rs` - Add `LinkStateComponent`
3. `libs/streamlib/src/core/error.rs` - Add `LinkNotWired`, `LinkAlreadyDisconnected`
4. `libs/streamlib/src/core/runtime/runtime.rs` - Update `disconnect()` method
5. `libs/streamlib/src/core/compiler/wiring.rs` - Update state after wiring
6. `libs/streamlib/src/core/graph/property_graph.rs` - Add `get_link_state()` helper
7. `libs/streamlib/src/core/graph/mod.rs` - Export `LinkState`
8. `libs/streamlib/tests/property_graph_ecs_test.rs` - Update tests

## Testing Strategy

1. Test `disconnect()` on a non-wired link returns `LinkNotWired`
2. Test `disconnect()` twice on same link returns `LinkAlreadyDisconnected`
3. Test link state transitions: `Pending` → `Wired` → `Disconnecting` → `Disconnected`
4. Test JSON output includes `state` field
5. Test `get_link_state()` API

## Migration Notes

- Existing `Link` structs without `state` field will deserialize with `LinkState::Pending` (due to `#[serde(default)]`)
- No breaking changes to public API - `disconnect()` return type remains `Result<()>`, just with more specific errors
