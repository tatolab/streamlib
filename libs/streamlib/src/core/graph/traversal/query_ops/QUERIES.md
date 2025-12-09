# Query Operations

One operator per file. Operations are grouped by type.

---

## Files to Remove

These are replaced by `.iter()` + std library:
- `first_op.rs` - use `.iter().next()`
- `value_op.rs` - use `.iter().next()`
- `collect_op.rs` - use `.iter().collect()`
- `count_op.rs` - use `.iter().count()`
- `exists_op.rs` - not needed, use `.iter().next().is_some()`

---

## Entry Points (TraversalSource)

These are the only way to start a traversal.

| Operation | Returns | Description |
|-----------|---------|-------------|
| `v()` | `ProcessorTraversal` | Start with all vertices |
| `v(id)` | `ProcessorTraversal` | Start with specific vertex |
| `e()` | `LinkTraversal` | Start with all edges |
| `e(id)` | `LinkTraversal` | Start with specific edge |

Status: [x] Implemented in `node_op.rs` and `edge_op.rs`

---
---

## Navigation Operations

---

### out_e - Outgoing Edges

From vertices, get their outgoing edges.

- **LinkTraversal**: N/A (edges don't have outgoing edges)
- **ProcessorTraversal**: Returns `LinkTraversal` with edges where current vertices are the source

Status: [x] Implemented in `out_e_op.rs`

---

### in_e - Incoming Edges

From vertices, get their incoming edges.

- **LinkTraversal**: N/A
- **ProcessorTraversal**: Returns `LinkTraversal` with edges where current vertices are the target

Status: [x] Implemented in `in_e_op.rs`

---

### out_v - Target Vertices

From edges, get the vertices they point to.

- **LinkTraversal**: Returns `ProcessorTraversal` with target vertices of current edges
- **ProcessorTraversal**: N/A (use `out_e().out_v()` to get neighbors)

Status: [x] Implemented in `out_v_op.rs`

---

### in_v - Source Vertices

From edges, get the vertices they came from.

- **LinkTraversal**: Returns `ProcessorTraversal` with source vertices of current edges
- **ProcessorTraversal**: N/A (use `in_e().in_v()` to get neighbors)

Status: [x] Implemented in `in_v_op.rs`

---
---

## Filter Operations

---

### filter

Filter elements by a predicate closure.

- **LinkTraversal**: `filter(|link| ...) -> LinkTraversal`
- **ProcessorTraversal**: `filter(|node| ...) -> ProcessorTraversal`

Status: [ ] Not implemented

---

### has

Filter by component existence or property value.

- **LinkTraversal**: `has::<Component>() -> LinkTraversal`
- **ProcessorTraversal**: `has::<Component>() -> ProcessorTraversal`

Status: [ ] Not implemented

---
---

## Terminal Operations

---

### iter

Convert traversal to a standard Rust iterator. This is the only terminal operation needed - all other terminal behavior comes from std.

- **LinkTraversal**: `iter() -> impl Iterator<Item = &Link>`
- **ProcessorTraversal**: `iter() -> impl Iterator<Item = &ProcessorNode>`

Status: [x] Not implemented

---

### Common patterns using iter()

```rust
// Get first element
traversal.iter().next()

// Get first or error
traversal.iter().next().ok_or_else(|| ...)

// Collect all
traversal.iter().collect::<Vec<_>>()

// Count
traversal.iter().count()

// Check existence
traversal.iter().next().is_some()

// Find by predicate
traversal.iter().find(|node| ...)

// Any match
traversal.iter().any(|node| ...)
```
