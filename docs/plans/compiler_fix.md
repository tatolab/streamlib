# Compiler/Runtime Refactor Plan

## Ownership Model

```
Compiler
├── graph: Graph
└── transaction: CompilerTransaction
    └── operations: Vec<PendingOperation>  (accumulates until commit)
```

- `scope(|graph, tx|)` - access graph and current transaction
- `tx.log(op)` - adds operation to transaction
- `commit()` - flushes transaction (validate → execute phases → clear)
- One transaction active at a time

---

## Phase 1: Compiler Takes Ownership (user-managed)

Must happen FIRST. Runtime changes depend on this.

| File | Action |
|------|--------|
| `pipeline.rs` | Add `graph: Graph` field to `Compiler` |
| `pipeline.rs` | Add `CompilerTransaction` struct (owns `Vec<PendingOperation>`) |
| `pipeline.rs` | Add transaction field to `Compiler` |
| `pipeline.rs` | Add `scope()` method |
| `pipeline.rs` | Update constructors to create/accept graph |

## Phase 2: Runtime Delegates to Compiler

Depends on Phase 1. Runtime stops owning graph/pending_operations.

| File | Action |
|------|--------|
| `runtime.rs:48` | Remove `graph: Arc<RwLock<Graph>>` field |
| `runtime.rs:67` | Remove `pending_operations: PendingOperationQueue` field |
| `runtime.rs:89-100` | Remove graph/pending_operations from `Default` impl |
| `runtime.rs:119-121` | Delete `graph()` method |
| `runtime.rs` | Change all `self.graph.read()/write()` → `self.compiler.scope()` |
| `runtime.rs` | Change all `self.pending_operations.push()` → `tx.log()` |

Methods to update:
- `add_processor()` - use scope + tx.log()
- `connect()` - use scope + tx.log()
- `disconnect_by_id()` - use scope + tx.log()
- `remove_processor_by_id()` - use scope + tx.log()
- `update_processor_config()` - use scope + tx.log()
- `start()` - use scope
- `stop()` - use scope
- `pause_processor()` - use scope
- `resume_processor()` - use scope
- `is_processor_paused()` - use scope
- `pause()` - use scope
- `resume()` - use scope
- `status()` - use scope
- `to_json()` - use scope

## Phase 3: Move Validation to Compiler

`execute_operations_batched` logic belongs in compiler.

| File | Action |
|------|--------|
| `runtime.rs:193-334` | Delete `execute_operations_batched()` entirely |
| `runtime.rs:337-359` | Delete `apply_config_update()` (moves to compiler) |
| `runtime.rs:139-179` | Simplify `commit()` to just call `self.compiler.compile()` |
| `pipeline.rs` | `compile()` flushes transaction: validate → order → execute phases → clear |

## Phase 4: Delete GraphDelta

No longer needed - compiler works directly with transaction operations.

| File | Action |
|------|--------|
| `delta.rs` | Delete file |
| `mod.rs` | Remove delta exports |
| `pipeline.rs` | Remove GraphDelta usage, iterate transaction.operations directly |

## Phase 5: Reorganize Compiler Module (optional)

| File | Action |
|------|--------|
| `phase.rs` + `phases.rs` | Merge into single file |
| `pipeline.rs` | Move `Compiler` to `mod.rs` or rename to `compiler.rs` |
| `pending.rs` | `PendingOperationQueue` no longer needed (transaction replaces it) |
