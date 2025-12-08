# Mutation Operations

Mutations chain like queries. All use petgraph directly via `&mut DiGraph`.

## Entry Points (QueryBuilder)
- add_v<P>(config) → ProcessorQuery (adds node, returns query with new node)
- add_e(from, to) → LinkQuery (adds edge, returns query with new edge)

## Node Mutations (ProcessorQuery)
- property(name, value) → ProcessorQuery (set property on matched nodes)
- drop() → () (remove matched nodes and their edges)

## Edge Mutations (LinkQuery)
- property(name, value) → LinkQuery (set property on matched edges)
- drop() → () (remove matched edges)

## Chained Examples
```rust
// Add a node
graph.query().add_v::<Encoder>(config)

// Add edge between existing nodes
graph.query().add_e("encoder.output", "decoder.input")

// Update property on filtered nodes
graph.query().v(()).filter(|n| n.is_failed()).property("state", "stopped")

// Delete all matching
graph.query().v(()).filter(|n| n.is_stale()).drop()

// Add edge from traversal
graph.query().v("encoder").add_e("feeds").to("decoder")
```

## Graph-Level (on Graph directly, not query)
- validate() → Result<()>
- checksum() → GraphChecksum
- topological_order() → Result<Vec<ProcessorUniqueId>>
