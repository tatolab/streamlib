# Mutation Operations

Files to create/update in `mutation_ops/`.

---

## add_v_op.rs

Add a new processor vertex to the graph.

- [x] `TraversalSourceMut::add_v::<P>(config) -> ProcessorTraversalMut`

```rust
graph.traversal_mut().add_v::<Encoder>(config)
```

---

## add_e_op.rs

Add a new edge between ports.

- [x] `TraversalSourceMut::add_e(from, to) -> LinkTraversalMut`

```rust
graph.traversal_mut().add_e("encoder.output", "decoder.input")
```

---

## drop_op.rs

Remove matched vertices/edges from the graph.

- [x] `ProcessorTraversalMut::drop() -> ProcessorTraversalMut`
- [x] `LinkTraversalMut::drop() -> LinkTraversalMut`

```rust
graph.traversal_mut().v(()).filter(|n| n.is_stale()).drop()
graph.traversal_mut().e(()).filter(|e| e.is_disconnected()).drop()
```

