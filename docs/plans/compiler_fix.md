# Compiler/Runtime Refactor Plan

## Group 1: Move Graph Ownership to Compiler

- [ ] Move `Graph` field from runtime to compiler
- [ ] Move `pending_operations` field from runtime to compiler
- [ ] Remove `Arc<RwLock<Graph>>` wrapper (compiler owns directly)
- [ ] Remove `runtime.graph()` public method

## Group 2: Add Compiler Scope API

- [ ] Add `CompilerTransaction` struct with `Mutex<Vec<PendingOperation>>`
- [ ] Add `compiler.scope(|graph, tx| ...)` method

## Group 3: Refactor Runtime Methods

- [ ] Refactor `runtime.add_processor()` to use `compiler.scope()`
- [ ] Refactor `runtime.connect()` to use `compiler.scope()`
- [ ] Refactor `runtime.to_json()` to use `compiler.scope()`
- [ ] Refactor other graph-accessing methods

## Group 4: Refactor Compiler

- [ ] Refactor `compiler.compile()` to use internal graph
- [ ] Remove `CompileRequest` (no longer needed)
