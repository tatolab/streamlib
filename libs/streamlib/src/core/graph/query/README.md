# Graph Query Interface

> **STATUS: DESIGN ONLY - DO NOT IMPLEMENT WITHOUT APPROVAL**
>
> This module contains trait definitions for a future graph query interface.
> The traits capture the design vision but are not yet implemented.
> Do not build implementations or integrate into the codebase until approved.

## Vision

The query interface provides a **unified way to traverse and query the graph**
without knowing anything about internal data stores (petgraph topology, hecs ECS).

Users (including AI agents and internal library code) interact with:
- **Processors** (nodes) - identified by `ProcessorId`
- **Links** (edges) - identified by `LinkId`
- **Properties** - type, state, metrics, config, etc.

They never interact with:
- `InternalProcessorLinkGraph` (petgraph)
- `InternalProcessorLinkGraphEcsExtension` (hecs)
- `NodeIndex`, `Entity`, or other internal handles

## Design Principles

### 1. Gremlin-Inspired, Not Gremlin-Compatible

The API follows Gremlin's traversal patterns (`V()`, `out()`, `has()`, etc.) because:
- Familiar to developers who know graph databases
- AI agents can transfer Gremlin knowledge
- Well-established semantics for graph traversal

We diverge from Gremlin where Rust's type system provides better ergonomics.

### 2. Lazy Query Building

Queries are builders that accumulate operations. Nothing executes until a
terminal operation (`ids()`, `count()`, `first()`, `collect()`) is called.

This enables:
- Query optimization before execution
- Reusable partial queries
- Clear separation between building and executing

```rust
// Build a partial query (no execution yet)
let cameras = graph.query().V().of_type("CameraProcessor");

// Reuse for different purposes
let camera_ids = cameras.clone().ids();           // Execute: get IDs
let camera_count = cameras.clone().count();       // Execute: count them
let downstream = cameras.downstream().ids();      // Extend then execute
```

### 3. Read-Only

Queries only read the graph. Mutations go through `Graph` methods or the runtime.

Rationale: Live graphs have complex state (running processors, active links).
Mutations need coordination that queries shouldn't handle.

### 4. Unified Property Access

Properties come from different internal sources:
- `type` → `ProcessorNode.processor_type` (topology)
- `state` → `StateComponent` (ECS)
- `metrics.fps` → `ProcessorMetrics` (ECS)
- `config.*` → processor config JSON (topology)

The query interface hides this. Users just ask for properties.

## Core Concepts

### Traversal

A traversal is a lazy sequence of steps that, when executed, produces results.

```
graph.query()           // Entry point
    .V()                // Start: all processors
    .of_type("Encoder") // Filter: by processor type
    .in_state(Running)  // Filter: by state component
    .downstream()       // Traverse: follow outgoing links
    .ids()              // Terminal: collect ProcessorIds
```

### Steps

| Category | Steps | Description |
|----------|-------|-------------|
| **Start** | `V()`, `E()` | Begin from all processors or links |
| **Start** | `V(id)`, `E(id)` | Begin from specific processor/link |
| **Filter** | `of_type()`, `has()` | Filter current selection |
| **Filter** | `in_state()`, `with_component()` | Filter by ECS state |
| **Traverse** | `out()`, `in_()`, `both()` | Follow links to neighbors |
| **Traverse** | `out_links()`, `in_links()` | Get links instead of neighbors |
| **Terminal** | `ids()`, `count()`, `first()` | Execute and return results |

### Results

Queries return domain types, not internal handles:
- `ProcessorId` - not `NodeIndex` or `Entity`
- `LinkId` - not edge index
- `ProcessorRef` - lightweight view with property access
- `LinkRef` - lightweight view with property access

## Usage Scenarios

### Internal Library Code

```rust
// Runtime starting processors in topological order
let start_order = graph.query()
    .V()
    .in_state(ProcessorState::Idle)
    .topological_order()
    .ids();

for id in start_order {
    runtime.start_processor(&id)?;
}
```

### User Application Code

```rust
// Find slow encoders and their sources
let bottleneck_sources = graph.query()
    .V()
    .of_type("H264Encoder")
    .where_metrics(|m| m.latency_p99_ms > 30.0)
    .upstream()
    .ids();
```

### AI Agent (Python)

```python
# Explore the graph structure
processor_types = graph.query().V().values("type").distinct()

# Find candidates for optimization
slow = graph.query() \
    .V() \
    .where("metrics.fps", "<", 30) \
    .ids()

# Understand the topology
for proc_id in slow:
    upstream = graph.query().V(proc_id).upstream().ids()
    downstream = graph.query().V(proc_id).downstream().ids()
    print(f"{proc_id}: {len(upstream)} inputs, {len(downstream)} outputs")
```

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     User Code / AI Agent                │
│   graph.query().V().of_type("Camera").downstream()      │
└────────────────────────┬────────────────────────────────┘
                         │
┌────────────────────────▼────────────────────────────────┐
│                  Query Builder (lazy)                   │
│   Accumulates steps, validates at build time            │
│   ProcessorQuery, LinkQuery, PathQuery                  │
└────────────────────────┬────────────────────────────────┘
                         │ terminal operation called
┌────────────────────────▼────────────────────────────────┐
│               QueryExecutor (trait object)              │
│   Executes against a GraphQueryInterface impl           │
└────────────────────────┬────────────────────────────────┘
                         │
┌────────────────────────▼────────────────────────────────┐
│              GraphQueryInterface (trait)                │
│   Defines primitive operations the executor needs       │
│   Implemented by Graph, backed by petgraph + hecs       │
└─────────────────────────────────────────────────────────┘
```

## Files in This Module

- `mod.rs` - Module exports (currently minimal)
- `traits.rs` - Core trait definitions
- `README.md` - This design document

## Future Considerations

### Path Queries

Return the path taken, not just endpoints:

```rust
let path = graph.query()
    .V("camera_0")
    .path_to("file_writer_0")
    .first();
// => Some(["camera_0", "encoder_0", "muxer_0", "file_writer_0"])
```

### Reactive Queries (Subscriptions)

Subscribe to query result changes:

```rust
graph.query()
    .V()
    .in_state(ProcessorState::Failed)
    .subscribe(|failed_ids| {
        alert_operator(failed_ids);
    });
```

### Query Serialization

Serialize queries for logging, debugging, or remote execution:

```rust
let query = graph.query().V().of_type("Encoder").downstream();
println!("{}", query.to_gremlin_string());
// => "g.V().has('type', 'Encoder').out()"
```

## References

- [Apache TinkerPop Gremlin](https://tinkerpop.apache.org/gremlin.html)
- [Practical Gremlin Book](https://www.kelvinlawrence.net/book/PracticalGremlin.html)
- [Gremlin DSL Guide](https://github.com/m-thirumal/gremlin-dsl)
