# Graph Migration Verification Tasks

Status: Work in Progress
Goal: Get the library back to a running state after graph traversal API migration

---

## Overview

The migration introduced:
- New `Graph` struct (data_structure.rs) with traversal API
- Split `LinkPortRef` into `InputLinkPortRef` and `OutputLinkPortRef`
- Renamed `has()` to `has_component()`
- Added `exists()` op
- Changed `add_v`/`add_e` to return Self for chainability

Two graph systems currently coexist:
- `Graph` (new) - used by runtime, compiler, traversals
- `InternalProcessorLinkGraph` (old) - in `internal/`, has lots of legacy methods

---

## Phase 1: Core Library Compiles ✅

### 1.1 Fix Dead Code Warnings
- [x] `source_checksum` field in `Graph` - removed (feature not started yet)
- [x] `digraph()` and `digraph_mut()` methods - already removed
- [~] Thread priority functions in `apple/thread_priority.rs` - deferred

### 1.2 Add Missing Copyright Headers
- [x] `libs/streamlib/src/core/graph/nodes/mod.rs`
- [x] `libs/streamlib/src/core/graph/traversal/mutation_ops/drop_op.rs`
- [x] `libs/streamlib/src/core/graph/traversal/query_ops/filter_op.rs`
- [x] `libs/streamlib/src/core/graph/traversal/query_ops/edge_op.rs`
- [x] `libs/streamlib/src/core/graph/edges/mod.rs`
- [x] `libs/streamlib/src/core/graph/edges/link_capacity.rs`

### 1.3 Run Library-Only Check
- [~] `cargo check -p streamlib` - deferred (examples have errors)
- [~] `cargo clippy -p streamlib` - deferred

---

## Phase 2: Verify Add Processor Flow (1-2 hours)

### 2.1 Trace `runtime.add_processor<P>(config)`
- [ ] Verify `graph.traversal_mut().add_v::<P>(config)` creates correct `ProcessorNode`
- [ ] Verify node has: `id`, `processor_type`, `config`, `inputs`, `outputs`
- [ ] Verify `exists()` returns true after adding
- [ ] Verify `first()` returns the added node

### 2.2 Write Unit Test
```rust
#[test]
fn test_add_processor_via_traversal() {
    let mut graph = Graph::new();
    // Add processor, verify fields
}
```

---

## Phase 3: Verify Add Edge Flow (1-2 hours)

### 3.1 Trace `runtime.connect(from, to)`
- [ ] Verify `graph.traversal_mut().add_e(from, to)` creates correct `Link`
- [ ] Verify link has: `id`, `from_port`, `to_port`
- [ ] Verify `e(link_id).exists()` returns true after adding
- [ ] Verify `first()` returns the added link

### 3.2 Verify Port Ref Types
- [ ] `OutputLinkPortRef` used for source ports
- [ ] `InputLinkPortRef` used for destination ports
- [ ] `from_port()` and `to_port()` on Link work correctly

### 3.3 Write Unit Test
```rust
#[test]
fn test_add_edge_via_traversal() {
    let mut graph = Graph::new();
    // Add two processors, connect them, verify link
}
```

---

## Phase 4: Verify Compiler Phase 1 - CREATE (1-2 hours)

### 4.1 Trace `phases::create_processor()`
- [ ] `graph.traversal().v(proc_id).first()` gets the ProcessorNode
- [ ] Factory creates processor instance from node
- [ ] `graph.traversal_mut().v(proc_id).first_mut()` gets mutable node
- [ ] Components attached: `ProcessorInstanceComponent`, `ShutdownChannelComponent`, etc.

### 4.2 Verify Component Insertion
- [ ] `node.insert(component)` works on ProcessorNode
- [ ] `node.get::<ComponentType>()` retrieves component
- [ ] `node.has::<ComponentType>()` returns correct bool

---

## Phase 5: Verify Compiler Phase 2 - WIRE (2-3 hours)

### 5.1 Trace `wiring::wire_link()`
- [ ] `graph.traversal_mut().e(link_id).first()` gets the Link
- [ ] `link.from_port()` and `link.to_port()` return correct refs
- [ ] Ring buffer created via `link_factory.create()`
- [ ] Data writer attached to source processor
- [ ] Data reader attached to destination processor

### 5.2 Trace `wiring::unwire_link()`
- [ ] Link found via traversal
- [ ] Data writer removed from source
- [ ] Data reader removed from destination
- [ ] Link components cleared

---

## Phase 6: Verify Compiler Phase 3 & 4 - SETUP & START (1-2 hours)

### 6.1 Phase 3: SETUP
- [ ] `graph.traversal().v(proc_id).first()` gets node
- [ ] `ProcessorInstanceComponent` retrieved
- [ ] `__generated_setup()` called on processor

### 6.2 Phase 4: START
- [ ] Thread spawned for processor
- [ ] `ThreadHandleComponent` attached to node
- [ ] State updated to `Running`

---

## Phase 7: Verify Runtime Lifecycle (1-2 hours)

### 7.1 Start/Stop
- [ ] `runtime.start()` - commits pending ops, sets state to Running
- [ ] `runtime.stop()` - calls `shutdown_all_processors()`, sets state to Idle

### 7.2 Pause/Resume
- [ ] `runtime.pause()` - iterates processors via `traversal().v(()).ids()`
- [ ] `runtime.resume()` - same pattern
- [ ] Per-processor pause/resume works

### 7.3 Status
- [ ] `runtime.status()` uses traversal correctly
- [ ] `has_component::<StateComponent>()` filters properly

---

## Phase 8: Fix Examples (2-4 hours)

### 8.1 Identify Pattern Issues
The examples have this error pattern:
```
expected `&ProcessorNode`, found `&ProcessorUniqueId`
```

This is in `input<M>()` and `output<M>()` helper functions.

### 8.2 Fix Helper Functions
Location: `link_port_markers.rs` lines 19, 24
- [ ] Update `input()` and `output()` to accept `ProcessorUniqueId` or `&ProcessorNode`
- [ ] Or update examples to pass correct type

### 8.3 Fix Each Example
- [ ] `camera-display`
- [ ] `simple-pipeline`
- [ ] `audio-mixer-demo`
- [ ] `camera-audio-recorder`
- [ ] `microphone-reverb-speaker`
- [ ] `webrtc-cloudflare-stream`
- [ ] `whep-player`
- [ ] `graph-json-demo`
- [ ] `test-main-thread-dispatch`

---

## Phase 9: Run Tests (1-2 hours)

### 9.1 Unit Tests
- [ ] `cargo test -p streamlib --lib` passes

### 9.2 Integration Tests
- [ ] Run each example manually, verify it starts and processes

---

## Phase 10: Cleanup (2-4 hours, can be deferred)

### 10.1 Remove `InternalProcessorLinkGraph`
Once everything works:
- [ ] Identify what methods are still used from `internal/processor_link_graph.rs`
- [ ] Migrate remaining functionality to `Graph` + traversals
- [ ] Delete `internal/` directory

### 10.2 Simplify Compiler
- [ ] Review abstraction layers in compiler
- [ ] Consolidate helper functions where possible

### 10.3 Add Missing Tests
- [ ] Traversal ops coverage
- [ ] Compiler phase coverage
- [ ] Runtime lifecycle coverage

---

## Quick Reference: Key Files

| Area | File |
|------|------|
| New Graph | `core/graph/data_structure.rs` |
| Old Graph | `core/graph/internal/processor_link_graph.rs` |
| Traversal Types | `core/graph/traversal/traversal_source.rs` |
| Add Vertex | `core/graph/traversal/mutation_ops/add_v_op.rs` |
| Add Edge | `core/graph/traversal/mutation_ops/add_e_op.rs` |
| Runtime | `core/runtime/runtime.rs` |
| Compiler | `core/compiler/pipeline.rs` |
| Phases | `core/compiler/phases.rs` |
| Wiring | `core/compiler/wiring.rs` |
| Port Refs | `core/graph/edges/input_link_port_ref.rs`, `output_link_port_ref.rs` |
| Port Helpers | `core/graph/edges/link_port_markers.rs` |

---

## Estimated Total Time

| Phase | Time |
|-------|------|
| 1. Core Compiles | 1-2h |
| 2. Add Processor | 1-2h |
| 3. Add Edge | 1-2h |
| 4. Phase CREATE | 1-2h |
| 5. Phase WIRE | 2-3h |
| 6. Phase SETUP/START | 1-2h |
| 7. Runtime Lifecycle | 1-2h |
| 8. Fix Examples | 2-4h |
| 9. Run Tests | 1-2h |
| 10. Cleanup (deferred) | 2-4h |
| **Total** | **13-25h** |

Start with Phase 1, then work through sequentially. Phases 2-7 can sometimes be done in parallel if you're verifying vs fixing.
