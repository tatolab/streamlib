# StreamLib Runtime Architecture

**Last Updated**: 2024-11-28
**Status**: Living Document - Single Source of Truth

This document consolidates all runtime design documentation into a single reference.

---

## Table of Contents

1. [Architecture Vision](#architecture-vision)
2. [Core Concepts](#core-concepts)
3. [Implementation Status](#implementation-status)
4. [Remaining Work Checklist](#remaining-work-checklist)
5. [API Reference (Pseudocode)](#api-reference-pseudocode)
6. [File Structure](#file-structure)
7. [Claude Code Context](#claude-code-context)

---

## Architecture Vision

### Design Principles

1. **Graph as Source of Truth**: The `Graph` represents desired state (what the user wants), the `ExecutionGraph` represents actual state (what's running)
2. **DOM/VDOM Pattern**: Like React - Graph is the DOM, ExecutionGraph is the VDOM with runtime metadata
3. **Delta-Based Updates**: Changes are computed as diffs and applied incrementally, not full rebuilds
4. **Unified API**: Same methods work whether runtime is stopped, running, or paused - no dual-path confusion
5. **Zero-Copy Where Possible**: Lock-free ring buffers between processors, no unnecessary allocations
6. **Dynamic Everything**: Add/remove processors and links at runtime without stopping

### Mental Model

**Key Insight**: Graph mutations work in ANY state. `start()` can be called at any time - before or after adding processors. Changes apply immediately when running (Auto mode).

```
User Code                    StreamLib                         Hardware
    │                            │                                │
    │  // Can build graph        │                                │
    │  // before OR after        │                                │
    │  // start() - order        │                                │
    │  // doesn't matter!        │                                │
    │                            │                                │
    │  runtime.add_processor()   │  ┌─────────────────────────┐   │
    │ ─────────────────────────► │  │ Graph (Desired State)   │   │
    │                            │  │ - ProcessorNodes        │   │
    │  runtime.connect()         │  │ - Links (edges)         │   │
    │ ─────────────────────────► │  │ - Validation (DAG)      │   │
    │                            │  └──────────┬──────────────┘   │
    │                            │             │                  │
    │  runtime.start()           │             │ compile()        │
    │ ─────────────────────────► │             ▼                  │
    │                            │  ┌─────────────────────────┐   │
    │  // OR add more while      │  │ ExecutionGraph (VDOM)   │   │
    │  // running - works too!   │  │ - RunningProcessors     │   │
    │                            │  │ - WiredLinks            │   │
    │  runtime.add_processor()   │  │ - Thread handles        │   │
    │ ─────────────────────────► │  └──────────┬──────────────┘   │
    │  (auto-syncs if running)   │             │ spawn threads    │
    │                            │             ▼                  │
    │                            │  ┌─────────────────────────┐   │
    │                            │  │ Processor Threads       │──►│ Camera
    │                            │  │ [Camera]──►[Display]    │──►│ Display
    │                            │  │     └──►[Encoder]       │──►│ GPU
    │                            │  └─────────────────────────┘   │
```

**Unified API - No State-Dependent Behavior**:
```rust
// These all work identically whether runtime is Idle, Running, or Paused:
runtime.add_processor::<P>(config)?;    // Adds to graph
runtime.connect(output, input)?;         // Adds link to graph
runtime.remove_processor(handle)?;       // Removes from graph
runtime.disconnect(link)?;               // Removes link from graph

// If running + Auto mode: changes sync immediately
// If stopped: changes wait until start()
// If Manual mode: changes wait until commit()
```

### Data Flow

```
┌────────────────────────────────────────────────────────────────────────┐
│                           StreamRuntime                                 │
│                                                                        │
│  ┌──────────────────┐    ┌──────────────────┐    ┌─────────────────┐  │
│  │      Graph       │    │    Executor      │    │ ProcessorFactory│  │
│  │  (Desired State) │───►│  (Orchestrator)  │◄───│   (Registry)    │  │
│  │                  │    │                  │    │                 │  │
│  │ • ProcessorNodes │    │ • compile()      │    │ • create()      │  │
│  │ • Links          │    │ • sync_to_graph()│    │ • type lookup   │  │
│  │ • validate()     │    │ • apply_delta()  │    └─────────────────┘  │
│  │ • checksum       │    │                  │                         │
│  └──────────────────┘    └────────┬─────────┘                         │
│                                   │                                    │
│                                   ▼                                    │
│                    ┌──────────────────────────────┐                   │
│                    │     ExecutionGraph (VDOM)    │                   │
│                    │                              │                   │
│                    │  ┌────────────────────────┐  │                   │
│                    │  │   RunningProcessor     │  │                   │
│                    │  │ • ProcessorNode (ref)  │  │                   │
│                    │  │ • thread handle        │  │                   │
│                    │  │ • shutdown channel     │  │                   │
│                    │  │ • wakeup channel       │  │                   │
│                    │  │ • processor instance   │  │                   │
│                    │  └────────────────────────┘  │                   │
│                    │                              │                   │
│                    │  ┌────────────────────────┐  │                   │
│                    │  │      WiredLink         │  │                   │
│                    │  │ • Link metadata        │  │                   │
│                    │  │ • ring buffer (rtrb)   │  │                   │
│                    │  │ • producer/consumer    │  │                   │
│                    │  └────────────────────────┘  │                   │
│                    └──────────────────────────────┘                   │
└────────────────────────────────────────────────────────────────────────┘
```

---

## Core Concepts

### 1. Graph (Desired State)

The `Graph` is a petgraph-based DAG representing what the user wants:

```rust
pub struct Graph {
    inner: DiGraph<ProcessorNode, Link>,
    processor_indices: HashMap<ProcessorId, NodeIndex>,
    link_indices: HashMap<LinkId, EdgeIndex>,
}

pub struct ProcessorNode {
    pub id: ProcessorId,
    pub processor_type: String,      // e.g., "CameraProcessor"
    pub config_json: String,         // Serialized config
    pub config_checksum: u64,        // For change detection
    pub ports: ProcessorPorts,       // Input/output port metadata
}

pub struct Link {
    pub id: LinkId,
    pub source: LinkPortAddress,     // "camera_0.video_output"
    pub target: LinkPortAddress,     // "display_0.video_input"
    pub config: LinkConfig,          // Buffer capacity, strategy
}
```

**Key Operations**:
- `add_processor()` / `remove_processor()` - Mutate nodes
- `add_link()` / `remove_link()` - Mutate edges
- `validate()` - Ensure DAG (no cycles), type compatibility
- `topological_order()` - Get processing order
- `find_sources()` / `find_sinks()` - Find entry/exit points
- `compute_checksum()` - Hash for change detection

### 2. ExecutionGraph (Actual State)

The `ExecutionGraph` wraps Graph and adds runtime state:

```rust
pub struct ExecutionGraph {
    graph: Arc<RwLock<Graph>>,                              // Shared reference to Graph
    processors: HashMap<ProcessorId, RunningProcessor>,     // Runtime state
    links: HashMap<LinkId, WiredLink>,                      // Runtime state
    metadata: CompilationMetadata,                          // When compiled, checksum
}

pub struct RunningProcessor {
    pub node: ProcessorNode,                    // Copy of graph node
    pub thread: Option<JoinHandle<()>>,         // Thread handle
    pub shutdown_tx: Sender<()>,                // Graceful shutdown
    pub wakeup_tx: Sender<LinkWakeupEvent>,     // Reactive wakeup
    pub state: Arc<Mutex<ProcessorState>>,      // Current state
    pub processor: Option<Arc<Mutex<BoxedProcessor>>>, // Instance
}

pub struct WiredLink {
    pub link: Link,                             // Copy of graph edge
    pub channel: Option<LinkChannel>,           // Ring buffer
}
```

### 3. Delta-Based Synchronization

When Graph changes, compute delta and apply incrementally:

```rust
pub struct GraphDelta {
    pub processors_to_add: Vec<ProcessorId>,
    pub processors_to_remove: Vec<ProcessorId>,
    pub processors_to_update: Vec<ProcessorConfigChange>,  // Config changed
    pub links_to_add: Vec<LinkId>,
    pub links_to_remove: Vec<LinkId>,
    pub links_to_update: Vec<LinkConfigChange>,            // Capacity changed
}

// Application order (critical for correctness):
// 1. Unwire links_to_remove (before removing processors that use them)
// 2. Shutdown processors_to_remove
// 3. Spawn processors_to_add
// 4. Wire links_to_add
// 5. Apply processors_to_update (hot-reload config)
// 6. Apply links_to_update (resize buffers) [TODO]
```

### 4. Execution Modes

Processors declare how they want to be scheduled:

```rust
pub enum ProcessExecution {
    /// Tight loop, optional sleep interval
    Continuous { interval_ms: u32 },

    /// Sleep until data arrives (wakeup event)
    Reactive,

    /// User controls timing via callbacks
    Manual,
}

pub struct ExecutionConfig {
    pub execution: ProcessExecution,
    pub priority: ThreadPriority,  // Normal, High, RealTime
}
```

### 5. Commit Modes

Control when graph changes take effect:

```rust
pub enum CommitMode {
    /// Changes apply immediately (sync_to_graph called automatically)
    Auto,

    /// Batch changes, apply on explicit commit()
    Manual,
}
```

### 6. Port System (Plug Pattern)

Ports always have at least one connection (a "plug" if disconnected):

```rust
pub struct LinkOutput<T: LinkPortMessage> {
    connections: Vec<LinkOutputConnection<T>>,  // Always >= 1
}

pub enum LinkOutputConnection<T> {
    Connected {
        id: LinkId,
        producer: LinkOwnedProducer<T>,
        wakeup: Sender<LinkWakeupEvent>,
    },
    Disconnected {
        plug: LinkDisconnectedProducer<T>,  // Silently drops data
    },
}
```

**Why Plugs?**
- Processors never crash from empty connection lists
- No null checks needed in hot path
- Safe disconnect/reconnect at runtime

---

## Implementation Status

### Fully Implemented

| Component | Location | Notes |
|-----------|----------|-------|
| Graph with petgraph | `core/graph/graph.rs` | DiGraph, validation, topology |
| ProcessorNode | `core/graph/node.rs` | Full metadata, checksums |
| Link | `core/graph/link.rs` | Port addresses, config |
| ExecutionGraph | `core/executor/execution_graph.rs` | VDOM pattern |
| GraphDelta | `core/executor/delta.rs` | Add/remove/update tracking |
| Delta application | `core/executor/simple_executor.rs` | Ordered apply |
| Config hot-reload | `core/executor/simple_executor.rs` | Via checksum detection |
| ExecutorState | `core/executor/state.rs` | Idle/Compiled/Running/Paused |
| Execution modes | `core/execution/process_execution.rs` | Continuous/Reactive/Manual |
| Thread priorities | `core/execution/thread_priority.rs` | + Apple implementation |
| Commit modes | `core/executor/` | Auto/Manual |
| Dynamic add/remove | `core/runtime/` | Works in all states |
| Unified connect API | `core/runtime/` | No dual-path |
| Plug pattern | `core/link_channel/` | Disconnected producers/consumers |
| Port-level cleanup | `core/executor/` | unwire on disconnect |
| Macro generation | `streamlib-macros/` | Full port introspection |
| ProcessorFactory | `core/registry.rs` | Type-based instantiation |

### Partially Implemented

| Component | Status | What's Missing |
|-----------|--------|----------------|
| Link config update | Delta computed | `links_to_update` not applied |
| Pause/Resume | State tracked | Processor signal mechanism |

### Not Implemented (Future Work)

| Component | Notes |
|-----------|-------|
| Graph optimization passes | Buffer sizing, fusion - not needed yet |
| ExecutionPlan IR | Over-engineered, delta approach sufficient |
| DOT/Graphviz export | Nice to have for debugging |
| MCP graph tools | Nice to have for visualization |

---

## Remaining Work Checklist

### Must Complete (Functional Gaps)

- [ ] **Link Config Hot-Reload** (`simple_executor.rs:301`)
  - `links_to_update` computed but not applied
  - Need: Disconnect old link, create new with updated capacity, rewire
  - Complexity: Medium
  - Assignee: _______
  - Due: _______

- [ ] **Pause/Resume Processor Signals** (`simple_executor.rs:1332-1347`)
  - State transitions work, processors don't actually pause
  - Need: Channel-based suspend/resume signals to processor loops
  - Complexity: Medium
  - Assignee: _______
  - Due: _______

### Should Complete (Code Quality)

- [ ] **Registry Cleanup** (`registry.rs:1`)
  - TODO comment about Phase 1 redesign
  - Review for cruft, simplify if possible
  - Complexity: Low
  - Assignee: _______
  - Due: _______

- [ ] **Windows Signal Handling** (`signals.rs:195`)
  - macOS/Linux work, Windows TODO
  - Need: `SetConsoleCtrlHandler` implementation
  - Complexity: Low
  - Assignee: _______
  - Due: _______

### Nice to Have (DX/Debugging)

- [ ] **Graph Visualization**
  - `to_dot()` for Graphviz
  - Useful for debugging complex pipelines
  - Complexity: Low
  - Assignee: _______
  - Due: _______

- [ ] **CLAP Host Cleanup** (`clap/host.rs:1`)
  - Unused structs/fields noted
  - Low priority
  - Assignee: _______
  - Due: _______

### Documentation

- [x] **Consolidate runtime docs** (this document)
- [ ] **Update examples README** if needed
- [ ] **API documentation pass** after remaining work complete

---

## API Reference (Pseudocode)

### StreamRuntime

```
StreamRuntime {
    // Internal state
    graph: Graph                      // Desired state (DOM)
    executor: SimpleExecutor          // Orchestrates execution
    factory: ProcessorFactory         // Creates processor instances
    commit_mode: CommitMode           // Auto or Manual

    new() -> StreamRuntime {
        // Initialize empty graph
        // Initialize executor with graph reference
        // Initialize factory with registered processors
        // Default to Auto commit mode
    }

    // === Lifecycle Methods ===

    start() -> Result<()> {
        // Validate graph (DAG check, type compatibility)
        // Compile: Graph -> ExecutionGraph
        // Spawn processor threads
        // Wire links (create ring buffers, connect ports)
        // Transition: Idle -> Running
        // Emit RuntimeStarted event
    }

    stop() -> Result<()> {
        // Signal all processors to shutdown
        // Wait for threads to complete (with timeout)
        // Unwire all links
        // Transition: Running -> Idle
        // Emit RuntimeStopped event
    }

    pause() -> Result<()> {
        // Signal processors to suspend (TODO: implement signals)
        // Transition: Running -> Paused
        // Emit RuntimePaused event
    }

    resume() -> Result<()> {
        // If graph dirty, sync changes
        // Signal processors to continue (TODO: implement signals)
        // Transition: Paused -> Running
        // Emit RuntimeResumed event
    }

    restart() -> Result<()> {
        // stop() then start()
        // Emit RuntimeRestarted event
    }

    // === Graph Mutation Methods ===

    add_processor<P: Processor>(config: P::Config) -> Result<ProcessorHandle> {
        // Create ProcessorNode with metadata
        // Add to graph
        // If running + Auto mode: sync_to_graph()
        // Return handle for later reference
    }

    remove_processor(handle: ProcessorHandle) -> Result<()> {
        // Remove all links connected to this processor first
        // Remove from graph
        // If running + Auto mode: sync_to_graph()
    }

    connect<T>(output: OutputRef<T>, input: InputRef<T>) -> Result<LinkHandle> {
        // Validate port types match
        // Create Link in graph
        // If running + Auto mode: sync_to_graph() -> wires immediately
        // Return handle for later disconnect
    }

    disconnect(handle: LinkHandle) -> Result<()> {
        // Remove from graph
        // If running + Auto mode: sync_to_graph() -> unwires immediately
    }

    update_processor_config<P>(handle: &ProcessorHandle, config: P::Config) -> Result<()> {
        // Update config in graph (new checksum)
        // If running + Auto mode: sync_to_graph() -> hot-reloads config
    }

    // === Commit Control ===

    set_commit_mode(mode: CommitMode) {
        // Switch between Auto and Manual
    }

    commit() -> Result<()> {
        // Only relevant in Manual mode
        // Force sync_to_graph() to apply batched changes
    }
}
```

### Graph

```
Graph {
    inner: DiGraph<ProcessorNode, Link>
    processor_indices: HashMap<ProcessorId, NodeIndex>
    link_indices: HashMap<LinkId, EdgeIndex>

    // === Mutation ===

    add_processor(node: ProcessorNode) -> ProcessorId {
        // Add node to petgraph
        // Update index
        // Return ID
    }

    remove_processor(id: &ProcessorId) -> Option<ProcessorNode> {
        // Remove from petgraph (also removes connected edges)
        // Update index
        // Return removed node
    }

    add_link(link: Link) -> LinkId {
        // Validate source/target exist
        // Add edge to petgraph
        // Update index
        // Return ID
    }

    remove_link(id: &LinkId) -> Option<Link> {
        // Remove from petgraph
        // Update index
        // Return removed link
    }

    // === Queries ===

    validate() -> Result<()> {
        // Check for cycles (must be DAG)
        // Check port type compatibility
        // Return errors if invalid
    }

    topological_order() -> Result<Vec<ProcessorId>> {
        // Return processors in dependency order
        // Sources first, sinks last
    }

    find_sources() -> Vec<ProcessorId> {
        // Processors with no incoming links
    }

    find_sinks() -> Vec<ProcessorId> {
        // Processors with no outgoing links
    }

    compute_checksum() -> u64 {
        // Hash entire graph structure
        // Used for change detection
    }

    // === Serialization ===

    to_json() -> String {
        // Serialize for persistence/debugging
    }

    from_json(json: &str) -> Result<Graph> {
        // Deserialize saved graph
    }
}
```

### Executor

```
SimpleExecutor {
    graph: Arc<RwLock<Graph>>           // Shared reference
    execution_graph: Option<ExecutionGraph>  // Compiled state
    state: ExecutorState                // Idle/Compiled/Running/Paused
    factory: Arc<ProcessorFactory>      // For creating instances

    // === Compilation ===

    compile_from_graph() -> Result<()> {
        // Read graph
        // Create ExecutionGraph (empty runtime state)
        // Transition: Idle -> Compiled
    }

    // === Synchronization ===

    sync_to_graph() -> Result<()> {
        // If not compiled, compile first
        // Compute delta between Graph and ExecutionGraph
        // Apply delta (ordered: unwire -> stop -> spawn -> wire -> update)
    }

    apply_delta(delta: GraphDelta) -> Result<()> {
        // 1. for link_id in delta.links_to_remove: unwire_link(link_id)
        // 2. for proc_id in delta.processors_to_remove: shutdown_processor(proc_id)
        // 3. for proc_id in delta.processors_to_add: spawn_processor(proc_id)
        // 4. for link_id in delta.links_to_add: wire_link(link_id)
        // 5. for change in delta.processors_to_update: apply_config_change(change)
        // 6. for change in delta.links_to_update: apply_link_change(change) [TODO]
    }

    // === Lifecycle ===

    start() -> Result<()> {
        // For each processor in execution_graph:
        //   spawn_processor_thread()
        // Transition: Compiled -> Running
    }

    stop() -> Result<()> {
        // For each processor:
        //   send shutdown signal
        //   join thread (with timeout)
        // Transition: Running -> Idle
    }

    // === Internal ===

    spawn_processor(proc_id: &ProcessorId) -> Result<()> {
        // Get ProcessorNode from graph
        // Create instance via factory
        // Spawn thread with execution loop
        // Store in ExecutionGraph
    }

    wire_link(link_id: &LinkId) -> Result<()> {
        // Get Link from graph
        // Create ring buffer channel
        // Wire producer to source processor's output port
        // Wire consumer to target processor's input port
        // Store in ExecutionGraph
    }
}
```

---

## File Structure

```
libs/streamlib/src/core/
├── runtime/
│   ├── mod.rs              # StreamRuntime public API
│   └── runtime.rs          # Implementation
├── graph/
│   ├── mod.rs              # Graph public API
│   ├── graph.rs            # Graph implementation
│   ├── node.rs             # ProcessorNode
│   ├── link.rs             # Link (edge)
│   └── checksum.rs         # Graph hashing
├── executor/
│   ├── mod.rs              # Executor trait + exports
│   ├── simple_executor.rs  # Thread-per-processor executor
│   ├── execution_graph.rs  # ExecutionGraph (VDOM)
│   ├── delta.rs            # GraphDelta computation
│   ├── running.rs          # RunningProcessor, WiredLink
│   └── state.rs            # ExecutorState enum
├── execution/
│   ├── mod.rs              # Execution mode exports
│   ├── process_execution.rs # Continuous/Reactive/Manual
│   └── thread_priority.rs  # Normal/High/RealTime
├── link_channel/
│   ├── mod.rs              # Port system exports
│   ├── channel.rs          # Ring buffer wrapper
│   ├── input.rs            # LinkInput<T>
│   ├── output.rs           # LinkOutput<T>
│   └── disconnected.rs     # Plug pattern (null objects)
├── traits/
│   ├── mod.rs              # Trait exports
│   ├── processor.rs        # Processor trait
│   └── base_processor.rs   # BaseProcessor trait
└── registry.rs             # ProcessorFactory
```

---

## Claude Code Context

> **For future Claude Code sessions**: This section provides essential context for understanding and working on the StreamLib runtime.

### Key Architecture Decisions

1. **Why DOM/VDOM?** - Separating desired state (Graph) from actual state (ExecutionGraph) allows safe mutations. Users can build/modify the graph freely; changes only apply when synced.

2. **Why Delta-Based?** - Full recompilation is expensive. Computing deltas (what changed) allows incremental updates: only spawn new processors, only wire new links.

3. **Why Plugs?** - The "plug pattern" (always having at least one connection) eliminates null checks in hot paths and makes disconnect safe.

4. **Why Unified API?** - Originally had `connect()` before start and `connect_at_runtime()` after. Now unified: same `connect()` works in all states.

5. **Why CommitMode?** - Auto mode is convenient (changes apply immediately). Manual mode allows batching many changes before applying (performance).

### Common Patterns

```rust
// Adding a processor (user code)
let camera = runtime.add_processor::<camera::Processor>(CameraConfig {
    device_id: None,
})?;

// Type-safe connection
runtime.connect(
    camera.output::<camera::OutputLink::video>(),
    display.input::<display::InputLink::video>(),
)?;

// Start after setup
runtime.start()?;

// Dynamic changes while running
let encoder = runtime.add_processor::<encoder::Processor>(config)?;
runtime.connect(camera.output::<...>(), encoder.input::<...>())?;
// ^ Automatically compiled and wired because Auto commit mode
```

### Critical Invariants

1. **Graph is always valid DAG** - `validate()` before compile
2. **Delta application order matters** - Unwire before remove, spawn before wire
3. **Ports always have >= 1 connection** - Plug pattern
4. **Config checksums detect changes** - Hot-reload without restart
5. **Shutdown is graceful** - Signal then join with timeout

### When Modifying Runtime Code

1. **Read the delta application order** in `apply_delta()` - order is critical
2. **Check ExecutorState transitions** - not all transitions are valid
3. **Preserve plug pattern** - never leave ports empty
4. **Update tests** - runtime has integration tests in `tests/`
5. **Update this document** if architecture changes

### Known TODOs

Search codebase for `TODO` to find remaining work. Key ones:
- `simple_executor.rs:301` - Link config update not applied
- `simple_executor.rs:1332` - Pause signals not implemented
- `signals.rs:195` - Windows support

---

*This document supersedes: graph_optimization_prework.md, graph_optimizer_infrastructure.md, graph_optimizer_strategies.md, runtime_redesign_graph_based.md, runtime_redesign_summary.md, idea_of_runtime*
