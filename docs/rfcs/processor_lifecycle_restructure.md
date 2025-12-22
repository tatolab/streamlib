# RFC: Processor Lifecycle Restructure

## Status: Draft

---

## Problem Statement

### Original Issue: RuntimeProxy Complexity

We were investigating whether the `RuntimeProxy` command processor thread was necessary. The proxy uses channels to send commands to a dedicated thread that processes them. The question was: can we simplify to direct calls?

### Root Cause Discovery: Deadlocks

During analysis, we discovered potential deadlocks in the current architecture:

#### Deadlock 1: Lock Held During `join()` (FIXED)

```
commit() on main thread:
  1. Acquires write lock on graph
  2. Calls shutdown_processor()
  3. shutdown_processor() calls join() - BLOCKS waiting for thread

Processor thread:
  1. Calls ctx.runtime().add_processor()
  2. Tries to acquire lock - BLOCKED (commit holds it)
  3. Can't exit until add_processor() completes

DEADLOCK: commit waits for thread, thread waits for lock held by commit
```

**This was fixed** by releasing the lock before `join()`:
- Phase 1: Signal shutdown, extract handle (with lock)
- Phase 2: Join thread (no lock)
- Phase 3: Cleanup (re-acquire lock)

#### Deadlock 2: Lock Held During Processor Lifecycle Methods (NOT YET FIXED)

```
commit() on main thread:
  1. Acquires write lock on graph
  2. Calls setup_processor()
  3. setup_processor() calls processor.setup(ctx)
  4. Processor's setup() calls ctx.runtime().add_processor()
  5. add_processor() tries to acquire lock - BLOCKED (already held)

DEADLOCK: Same thread trying to acquire lock it already holds (RwLock not reentrant)
```

Even with channels, this deadlocks:
- Main thread holds lock, calls setup(), processor sends command via channel
- Main thread blocks waiting for reply
- Command processor thread tries to acquire lock
- Lock held by main thread
- **DEADLOCK**

### Why the Current Architecture Causes This

Currently, processor lifecycle methods run on the **commit thread** (main thread):

```
commit() ─────────────────────────────────────────────────────────
    │
    ├─► CREATE:  creates ProcessorInstance (on commit thread)
    │
    ├─► WIRE:    connects ring buffers
    │
    ├─► SETUP:   calls processor.setup() (on commit thread) ← PROBLEM
    │
    └─► START:   spawns thread, runs process loop
```

The `setup()` method runs while the lock is held, so any runtime operation from `setup()` will deadlock.

---

## Solution: Move All Processor Lifecycle to Processor Thread

### Design Principle

**Separation of concerns:**
- **Graph nodes + components** = the *description* of a processor (data in graph)
- **Processor instance** = the *running* processor (created and managed by processor thread)

**Commit should only:**
- Modify graph structure (nodes, edges, components)
- Spawn threads
- Never call processor methods while holding locks

**Processor thread should:**
- Create its own instance from the spec in the graph
- Run all lifecycle methods (setup, process, teardown)
- Handle its own errors and state

### Target Architecture

```
COMMIT (main thread)
    │
    ├─► CREATE: Attach infrastructure components to graph node
    │           - ShutdownChannelComponent
    │           - StateComponent
    │           - ProcessorPauseGateComponent
    │           - LinkOutputToProcessorWriterAndReader
    │           - ReadyBarrierComponent (NEW)
    │           NO ProcessorInstanceComponent yet
    │
    ├─► START: Spawn processor threads
    │       │
    │       ├─► Thread 1: create instance → attach to graph → signal READY → wait
    │       ├─► Thread 2: create instance → attach to graph → signal READY → wait
    │       └─► Thread 3: create instance → attach to graph → signal READY → wait
    │
    ├─► Wait for all READY signals
    │
    ├─► WIRE: Instances exist now, attach ring buffer ends to processors
    │
    └─► Signal all threads: CONTINUE
            │
            ▼
        Threads continue (commit done, no locks held):
            ├─► setup(ctx)      ← can call add_processor() freely
            ├─► process loop    ← can call add_processor() freely
            └─► teardown()
```

### Why This Eliminates Deadlocks

1. **Lock only held for graph operations** - brief, atomic
2. **No processor methods called while lock held** - setup/process run after commit
3. **Processors can call runtime ops freely** - commit is done, locks released
4. **No re-entrancy issues** - different threads, different lock acquisitions

---

## Detailed Changes

### 1. New Component: `ReadyBarrierComponent`

**File:** `libs/streamlib/src/core/graph/components.rs` (or appropriate location)

```rust
/// Synchronization for processor startup.
/// Processor thread signals ready after creating instance.
/// Commit waits for all ready signals before wiring.
pub struct ReadyBarrierComponent {
    ready_tx: Option<oneshot::Sender<()>>,
    continue_rx: Option<oneshot::Receiver<()>>,
}

impl ReadyBarrierComponent {
    pub fn new() -> (Self, oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (ready_tx, ready_rx) = oneshot::channel();
        let (continue_tx, continue_rx) = oneshot::channel();

        let component = Self {
            ready_tx: Some(ready_tx),
            continue_rx: Some(continue_rx),
        };

        (component, ready_rx, continue_tx)
    }

    /// Called by processor thread after instance created
    pub fn signal_ready(&mut self) {
        if let Some(tx) = self.ready_tx.take() {
            let _ = tx.send(());
        }
    }

    /// Called by processor thread to wait for wiring to complete
    pub fn wait_for_continue(&mut self) {
        if let Some(rx) = self.continue_rx.take() {
            let _ = rx.recv();
        }
    }
}
```

### 2. Modify `create_processor_op.rs`

**Current behavior:** Creates `ProcessorInstance`, attaches as component

**New behavior:** Only attach infrastructure components, no instance yet

```rust
pub(crate) fn create_processor(
    graph: &mut Graph,
    proc_id: &ProcessorUniqueId,
) -> Result<(oneshot::Receiver<()>, oneshot::Sender<()>)> {
    let node_mut = graph
        .traversal_mut()
        .v(proc_id)
        .first_mut()
        .ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", proc_id))
        })?;

    // Create barrier for synchronization
    let (barrier, ready_rx, continue_tx) = ReadyBarrierComponent::new();

    // Attach infrastructure components (NO ProcessorInstanceComponent)
    node_mut.insert(barrier);
    node_mut.insert(ShutdownChannelComponent::new());
    node_mut.insert(LinkOutputToProcessorWriterAndReader::new());
    node_mut.insert(StateComponent::default());
    node_mut.insert(ProcessorPauseGateComponent::new());

    tracing::debug!("[{}] Infrastructure components attached", proc_id);

    Ok((ready_rx, continue_tx))
}
```

### 3. Modify `start_processor_op.rs`

**Current behavior:** Spawns thread that runs `run_processor_loop`

**New behavior:** Spawns thread that:
1. Creates processor instance from spec
2. Attaches `ProcessorInstanceComponent`
3. Signals ready
4. Waits for continue
5. Calls setup
6. Runs process loop

```rust
pub(crate) fn start_processor(
    graph: &mut Graph,
    runtime_ctx: &Arc<RuntimeContext>,
    factory: &ProcessorInstanceFactory,
    processor_id: impl AsRef<str>,
) -> Result<()> {
    let processor_id = processor_id.as_ref();

    // Extract data needed by thread (with lock)
    let (processor_type, config, node_spec) = {
        let node = graph.traversal().v(processor_id).first().ok_or_else(|| {
            StreamError::ProcessorNotFound(format!("Processor '{}' not found", processor_id))
        })?;

        (
            node.processor_type.clone(),
            node.config.clone(),
            // ... other spec data needed to create instance
        )
    };

    // Get barrier component
    let barrier = {
        let node = graph.traversal_mut().v(processor_id).first_mut().ok_or_else(|| {
            StreamError::ProcessorNotFound(processor_id.to_string())
        })?;
        node.remove::<ReadyBarrierComponent>()
    };

    // Clone Arcs for thread
    let graph_arc = Arc::clone(&graph_arc);  // Need access to graph arc
    let runtime_ctx = Arc::clone(runtime_ctx);
    let factory = factory.clone();  // Or Arc<ProcessorInstanceFactory>
    let proc_id: ProcessorUniqueId = processor_id.into();

    // Spawn thread
    let thread = std::thread::Builder::new()
        .name(format!("processor-{}", processor_id))
        .spawn(move || {
            // === PHASE 1: Create instance ===
            let processor = match factory.create_from_spec(&processor_type, &config) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("[{}] Failed to create instance: {}", proc_id, e);
                    return;
                }
            };
            let processor_arc = Arc::new(Mutex::new(processor));

            // Attach instance to graph (brief lock)
            {
                let mut graph = graph_arc.write();
                if let Some(node) = graph.traversal_mut().v(&proc_id).first_mut() {
                    node.insert(ProcessorInstanceComponent(processor_arc.clone()));
                }
            }

            // === PHASE 2: Signal ready, wait for wiring ===
            if let Some(mut barrier) = barrier {
                barrier.signal_ready();
                barrier.wait_for_continue();
            }

            // === PHASE 3: Setup ===
            // No locks held by commit at this point
            {
                let pause_gate = // ... get from graph
                let ctx = runtime_ctx.with_pause_gate(pause_gate);

                let mut guard = processor_arc.lock();
                if let Err(e) = guard.__generated_setup(&ctx) {
                    tracing::error!("[{}] Setup failed: {}", proc_id, e);
                    return;
                }
            }

            // === PHASE 4: Process loop ===
            run_processor_loop(
                proc_id,
                processor_arc,
                shutdown_rx,
                message_reader,
                state_arc,
                pause_gate_inner,
                exec_config,
            );

            // === PHASE 5: Teardown ===
            // ... cleanup
        })?;

    // Attach thread handle
    let node = graph.traversal_mut().v(processor_id).first_mut().ok_or_else(|| {
        StreamError::ProcessorNotFound(processor_id.to_string())
    })?;
    node.insert(ThreadHandleComponent(thread));

    Ok(())
}
```

### 4. Modify `compiler.rs` - Compile Flow

**Current phases:**
1. Validate operations
2. Handle removals
3. CREATE - create processor instances
4. WIRE - connect ports
5. SETUP - call processor.setup()
6. START - spawn threads

**New phases:**
1. Validate operations
2. Handle removals
3. CREATE - attach infrastructure components only
4. START - spawn threads (threads create instances, signal ready)
5. Wait for all ready signals
6. WIRE - connect ports (instances now exist)
7. Signal all threads to continue

```rust
// Phase: CREATE - infrastructure only
let mut barriers: Vec<(ProcessorUniqueId, oneshot::Receiver<()>, oneshot::Sender<()>)> = vec![];

for proc_id in &plan.processors_to_add {
    let mut graph = graph_arc.write();
    let (ready_rx, continue_tx) = super::compiler_ops::create_processor(&mut graph, proc_id)?;
    barriers.push((proc_id.clone(), ready_rx, continue_tx));
}

// Phase: START - spawn threads
for proc_id in &plan.processors_to_add {
    let mut graph = graph_arc.write();
    super::compiler_ops::start_processor(&mut graph, runtime_ctx, &factory, proc_id)?;
}

// Wait for all processors to be ready
for (proc_id, ready_rx, _) in &barriers {
    if ready_rx.recv().is_err() {
        tracing::warn!("[{}] Processor failed during startup", proc_id);
    }
}

// Phase: WIRE - instances now exist
for link_id in &plan.links_to_add {
    let mut graph = graph_arc.write();
    super::compiler_ops::wire_link(&mut graph, link_factory.as_ref(), link_id)?;
}

// Signal all threads to continue
for (proc_id, _, continue_tx) in barriers {
    if continue_tx.send(()).is_err() {
        tracing::warn!("[{}] Failed to signal continue", proc_id);
    }
}
```

### 5. Remove `setup_processor_op.rs`

This file is no longer needed - setup is called by the processor thread, not by the compiler.

### 6. Update `shutdown_processor` in `compiler.rs`

Already fixed - releases lock before `join()`.

### 7. Fix `runtime.stop()`

Same pattern as shutdown in compile:

```rust
pub fn stop(&self) -> Result<()> {
    *self.status.lock() = RuntimeStatus::Stopping;

    // Phase 1: Signal shutdown, extract handles (with lock)
    let thread_handles: Vec<_> = self.compiler.scope(|graph, _tx| {
        let processor_ids: Vec<ProcessorUniqueId> = graph.traversal().v(()).ids();

        processor_ids.iter().filter_map(|proc_id| {
            let node = graph.traversal_mut().v(proc_id).first_mut()?;

            // Set state to stopping
            if let Some(state) = node.get::<StateComponent>() {
                *state.0.lock() = ProcessorState::Stopping;
            }

            // Send shutdown signal
            if let Some(channel) = node.get::<ShutdownChannelComponent>() {
                let _ = channel.sender.send(());
            }

            // Extract thread handle
            node.remove::<ThreadHandleComponent>()
        }).collect()
    });

    // Phase 2: Join threads (no lock)
    for handle in thread_handles {
        if let Err(e) = handle.0.join() {
            tracing::error!("Processor thread panicked: {:?}", e);
        }
    }

    // Phase 3: Cleanup (re-acquire lock)
    self.compiler.scope(|graph, _tx| {
        let processor_ids: Vec<ProcessorUniqueId> = graph.traversal().v(()).ids();
        for proc_id in processor_ids {
            if let Some(node) = graph.traversal_mut().v(&proc_id).first_mut() {
                if let Some(state) = node.get::<StateComponent>() {
                    *state.0.lock() = ProcessorState::Stopped;
                }
            }
        }
        graph.set_state(GraphState::Idle);
    });

    *self.runtime_context.lock() = None;
    *self.status.lock() = RuntimeStatus::Stopped;

    Ok(())
}
```

---

## Impact on RuntimeProxy

With these changes, the deadlock scenarios are eliminated. The question of whether to simplify `RuntimeProxy` to direct calls becomes:

**Current:** Channel + command processor thread
**Simplified:** `Weak<StreamRuntime>` with direct calls

Both will work safely now. The decision is about code cleanliness:
- Direct calls avoid the channel overhead
- But `Weak::upgrade()` inside each method is "noisy"
- Alternative: Proxy holds `Arc<Compiler>` directly and implements operations itself

This can be addressed separately after the lifecycle restructure is complete.

---

## Testing Strategy

1. **Unit tests:** Verify processors can call `add_processor()` from `setup()`
2. **Integration tests:** Multiple processors starting concurrently
3. **Stress tests:** Rapid add/remove cycles while processors are running
4. **Deadlock detection:** Run with `RUST_BACKTRACE=1` and thread sanitizers

---

## Migration Path

1. Implement `ReadyBarrierComponent`
2. Modify `create_processor_op` to only create infrastructure
3. Modify `start_processor_op` to create instance on thread
4. Update `compiler.rs` compile flow with synchronization
5. Remove `setup_processor_op.rs`
6. Fix `runtime.stop()` with lock-release-before-join pattern
7. Update tests
8. (Optional) Simplify RuntimeProxy

---

## Summary

| Before | After |
|--------|-------|
| Instance created on commit thread | Instance created on processor thread |
| setup() called on commit thread | setup() called on processor thread |
| Lock held during processor methods | Lock only for graph operations |
| Deadlock if processor calls runtime ops | Safe - no locks held during processor code |
