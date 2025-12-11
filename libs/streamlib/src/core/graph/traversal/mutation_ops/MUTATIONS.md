# Mutation Operations

Files to create/update in `mutation_ops/`.

---

## add_v.rs

Add a new processor vertex to the graph.

- [ ] `TraversalSourceMut::add_v::<P>(config) -> ProcessorTraversalMut`

```rust
graph.traversal_mut().add_v::<Encoder>(config)
```

---

## add_e.rs

Add a new edge between ports.

- [ ] `TraversalSourceMut::add_e(from, to) -> LinkTraversalMut`

```rust
graph.traversal_mut().add_e("encoder.output", "decoder.input")
```

---

## drop.rs

Remove matched vertices/edges from the graph.

- [x] `ProcessorTraversalMut::drop() -> ProcessorTraversalMut`
- [x] `LinkTraversalMut::drop() -> LinkTraversalMut`

```rust
graph.traversal_mut().v(()).filter(|n| n.is_stale()).drop()
graph.traversal_mut().e(()).filter(|e| e.is_disconnected()).drop()
```

---

## property.rs

Set a property/component on matched vertices/edges.

- [ ] `ProcessorTraversalMut::property(name, value) -> ProcessorTraversalMut`
- [ ] `LinkTraversalMut::property(name, value) -> LinkTraversalMut`

```rust
graph.traversal_mut().v(()).filter(|n| n.is_failed()).property("state", "stopped")
```

