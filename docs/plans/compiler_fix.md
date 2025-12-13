# Compiler/Runtime Refactor Plan

> **@Jonathan TODO:** Investigate restoring `inventory` crate for automatic processor registration. The `#[streamlib::processor]` macro should auto-register processors at link time, eliminating manual `factory.register::<P>()` calls. This may have broken during the factory refactor. See conversation re: compile-time type collection.

---

**Goal:** Reorganize code so compilation-related logic lives in the Compiler. No logic changes - just moving code to better locations.

**Key Principles:**
- All existing logic is preserved (no deletions, only relocations)
- Same pubsub behaviors maintained
- Same operational capabilities
- PendingOperations work the same, just accumulated in a transaction

---

## Target Ownership Model

```
Compiler
├── graph: Graph                      (moved from Runtime)
└── transaction: CompilerTransaction
    └── operations: Vec<PendingOperation>  (moved from Runtime.pending_operations)
```

**API:**
- `compiler.scope(|graph, tx| { ... })` - access graph and current transaction
- `tx.log(op)` - adds operation to transaction (same as current push())
- `compiler.commit()` - flushes transaction using existing 4-phase logic
- One transaction active at a time

---

## Current State (After File Reorganization)

| Component | Location | Owner |
|-----------|----------|-------|
| `Graph` | `runtime.rs:29` | Runtime (Arc<RwLock<Graph>>) |
| `PendingOperationQueue` | `runtime.rs:48` | Runtime |
| `execute_operations_batched()` | `runtime.rs:174-315` | Runtime |
| `apply_config_update()` | `runtime.rs:318-340` | Runtime |
| `compile()` (4-phase logic) | `compiler.rs:162-235` | Compiler |

---

## Phase 1: Compiler Takes Ownership

Move graph and pending operations from Runtime to Compiler.

### 1a. Add fields to Compiler (`compiler.rs`)

```rust
pub struct Compiler {
    // Existing delegate fields...

    // NEW: Graph ownership (moved from Runtime)
    graph: Arc<RwLock<Graph>>,

    // NEW: Transaction accumulates operations until commit
    transaction: Arc<Mutex<Vec<PendingOperation>>>,
}
```

### 1b. Create `compiler_transaction.rs` (new file, one struct per file pattern)

```rust
// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use parking_lot::Mutex;
use super::PendingOperation;

/// Handle for logging operations to the compiler transaction.
pub struct CompilerTransactionHandle {
    inner: Arc<Mutex<Vec<PendingOperation>>>,
}

impl CompilerTransactionHandle {
    pub(crate) fn new(inner: Arc<Mutex<Vec<PendingOperation>>>) -> Self {
        Self { inner }
    }

    pub fn log(&self, op: PendingOperation) {
        self.inner.lock().push(op);
    }
}
```

### 1c. Add scope() method to Compiler

```rust
impl Compiler {
    /// Access graph and transaction for mutations. Callable from any thread.
    pub fn scope<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Graph, &CompilerTransactionHandle) -> R,
    {
        let mut graph = self.graph.write();
        let tx = CompilerTransactionHandle::new(Arc::clone(&self.transaction));
        f(&mut graph, &tx)
    }

    /// Read-only graph access.
    pub fn graph(&self) -> &Arc<RwLock<Graph>> {
        &self.graph
    }
}
```

---

## Phase 2: Move Commit Logic to Compiler

**MOVE** (not delete) `execute_operations_batched()` and `apply_config_update()` from Runtime to Compiler.

### 2a. Add commit() to Compiler

The existing `execute_operations_batched()` logic moves here verbatim:

```rust
impl Compiler {
    /// Flush transaction: validate operations → build OperationBatch → execute 4-phase compile.
    pub fn commit(&mut self, runtime_ctx: &Arc<RuntimeContext>) -> Result<()> {
        if self.transaction.is_empty() {
            return Ok(());
        }

        let operations = self.transaction.take_all();

        // === MOVED FROM runtime.rs:execute_operations_batched() ===
        // All validation, OperationBatch building, and compile() calls
        // Same logic, just different location
        // ===========================================================

        self.execute_operations_batched(operations, runtime_ctx)
    }

    /// MOVED from Runtime - no logic changes.
    fn execute_operations_batched(
        &mut self,
        operations: Vec<PendingOperation>,
        runtime_ctx: &Arc<RuntimeContext>,
    ) -> Result<()> {
        // Exact same implementation as current runtime.rs:174-315
        // Just uses self.graph instead of self.graph.write()
    }

    /// MOVED from Runtime - no logic changes.
    fn apply_config_update(&mut self, proc_id: &ProcessorUniqueId) -> Result<()> {
        // Exact same implementation as current runtime.rs:318-340
    }
}
```

---

## Phase 3: Update Runtime to Delegate

Runtime becomes a thin wrapper that delegates to Compiler.

### 3a. Remove fields from Runtime

```rust
pub struct StreamRuntime {
    // REMOVE: graph: Arc<RwLock<Graph>>
    // REMOVE: pending_operations: PendingOperationQueue

    // KEEP: compiler (now owns graph + transaction)
    pub(crate) compiler: Compiler,

    // KEEP: all other fields unchanged
    pub(crate) default_factory: Arc<DefaultFactory>,
    pub(crate) factory: Arc<dyn FactoryDelegate>,
    // ...
}
```

### 3b. Update Runtime methods to use scope()

**Before:**
```rust
pub fn add_processor<P>(&mut self, config: P::Config) -> Result<ProcessorUniqueId> {
    let processor_id = self.graph
        .write()
        .traversal_mut()
        .add_v::<P>(config)
        // ...

    self.pending_operations.push(PendingOperation::AddProcessor(processor_id.clone()));
    self.on_graph_changed()?;
    Ok(processor_id)
}
```

**After:**
```rust
pub fn add_processor<P>(&mut self, config: P::Config) -> Result<ProcessorUniqueId> {
    let processor_id = self.compiler.scope(|graph, tx| {
        let id = graph
            .traversal_mut()
            .add_v::<P>(config)
            // ...

        tx.log(PendingOperation::AddProcessor(id.clone()));
        Ok(id)
    })?;

    self.on_graph_changed()?;
    Ok(processor_id)
}
```

### 3c. Simplify Runtime::commit()

**Before:**
```rust
pub fn commit(&mut self) -> Result<()> {
    if self.pending_operations.is_empty() { return Ok(()); }
    if !self.started { return Ok(()); }

    let operations = self.pending_operations.take_all();
    let runtime_ctx = self.runtime_context.as_ref()...;

    self.execute_operations_batched(operations, &runtime_ctx)?;
    Ok(())
}
```

**After:**
```rust
pub fn commit(&mut self) -> Result<()> {
    if !self.started { return Ok(()); }

    let runtime_ctx = self.runtime_context.as_ref()...;
    self.compiler.commit(&runtime_ctx)
}
```

---

## Summary of Code Movement

| Logic | Current Location | New Location | Change Type |
|-------|-----------------|--------------|-------------|
| `Graph` ownership | `Runtime.graph` | `Compiler.graph` | MOVE |
| `PendingOperationQueue` | `Runtime.pending_operations` | `Compiler.transaction` | MOVE |
| Transaction handle | N/A | `compiler_transaction.rs` | NEW FILE |
| `execute_operations_batched()` | `runtime.rs:174-315` | `Compiler::compile()` | MERGE |
| `apply_config_update()` | `runtime.rs:318-340` | `Compiler::compile()` | MERGE |
| Current `Compiler::compile()` | `compiler.rs:162-235` | `Compiler::compile()` | MERGE |
| `compiler_ops/` | `compiler_ops/*.rs` | `compiler_ops/*.rs` | NO CHANGE |
| `OperationBatch` | `operation_batch.rs` | MAY BE REMOVED | SEE NOTE |

**NOTE on OperationBatch:** With `compile()` taking `Vec<PendingOperation>` directly and doing categorization inline, `OperationBatch` may become unnecessary. Evaluate during implementation.

**Nothing is deleted. All logic is preserved - just consolidated into one `compile()` method.**

---

## Threading Model

**Requirements:**
1. **Graph access from any thread** - API services, background workers can call `scope()` to mutate graph and log operations
2. **Compilation on main thread only** - 4-phase execution must happen on main thread (macOS thread spawning, AVFoundation, Metal expectations)

**Implementation:**
```rust
pub struct Compiler {
    // Thread-safe graph access
    graph: Arc<RwLock<Graph>>,

    // Thread-safe operation accumulation
    transaction: Arc<Mutex<Vec<PendingOperation>>>,

    // Delegates (already Arc)
    factory: Arc<dyn FactoryDelegate>,
    // ...
}
```

**scope() - callable from any thread:**
```rust
impl Compiler {
    pub fn scope<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut Graph, &TransactionHandle) -> R,
    {
        let mut graph = self.graph.write();
        let tx = TransactionHandle { inner: Arc::clone(&self.transaction) };
        f(&mut graph, &tx)
    }
}

pub struct TransactionHandle {
    inner: Arc<Mutex<Vec<PendingOperation>>>,
}

impl TransactionHandle {
    pub fn log(&self, op: PendingOperation) {
        self.inner.lock().push(op);
    }
}
```

**commit() - callable from any thread:**
```rust
impl Compiler {
    /// Flush transaction. Callable from any thread - compile() is dispatched to main thread.
    pub fn commit(&self, runtime_ctx: &Arc<RuntimeContext>) -> Result<()> {
        let operations = std::mem::take(&mut *self.transaction.lock());
        if operations.is_empty() { return Ok(()); }

        // Dispatch compile to main thread (required for thread spawning, Apple frameworks)
        runtime_ctx.run_on_main_blocking(|| {
            self.compile(operations, runtime_ctx)
        })
    }
}
```

**compile() - ONE method, NO helpers:**

All orchestration logic inlined. Calls `compiler_ops::*` for actual operations.

```rust
impl Compiler {
    /// Single compile method - ALL logic here, no helper methods.
    fn compile(&self, operations: Vec<PendingOperation>, runtime_ctx: &Arc<RuntimeContext>) -> Result<()> {
        // 1. Validate and categorize operations (moved from execute_operations_batched)
        //    - Check processor exists, not already running, not pending deletion
        //    - Separate into: processors_to_add/remove, links_to_add/remove, config_updates

        // 2. Handle removals FIRST
        //    - Unwire links: compiler_ops::unwire_link()
        //    - Shutdown processors: compiler_ops::shutdown_processor()
        //    - Remove from graph

        // 3. Phase 1 CREATE - for each processor_to_add:
        //    - compiler_ops::create_processor()

        // 4. Phase 2 WIRE - for each link_to_add:
        //    - compiler_ops::wire_link()

        // 5. Phase 3 SETUP - for each processor_to_add:
        //    - compiler_ops::setup_processor()

        // 6. Phase 4 START - for each processor_to_add:
        //    - compiler_ops::start_processor()

        // 7. Config updates - for each config_update:
        //    - Get processor instance, call apply_config_json()

        Ok(())
    }
}
```

**compiler_ops/ stays as-is:**
- `create_processor_op.rs` - `create_processor()`
- `wire_link_op.rs` - `wire_link()`, `unwire_link()`
- `setup_processor_op.rs` - `setup_processor()`
- `start_processor_op.rs` - `start_processor()`
- `shutdown_processor_op.rs` - `shutdown_processor()`, `shutdown_all_processors()`

**Why compile() runs on main thread:**
- Thread spawning assumes main thread is parent (current architecture)
- macOS: AVFoundation, Metal, VideoToolbox expect main thread context
- Processor `__generated_setup` may use Apple frameworks

---

## Implementation Workflow

**DO NOT run `cargo check` automatically.**

1. Claude performs ALL changes in this plan (moves, merges, reorganization)
2. **PAUSE** - Claude announces "Done with all changes, ready for review"
3. User performs manual review
4. User makes changes, fixes issues as needed
5. **Only after user completes their review** → `cargo check`

This is primarily file moves and code consolidation - one batch of work, not phases.
