# streamlib-codegen-shared

Types shared between `streamlib` and `streamlib-macros` for code generation.

## Purpose

This crate exists to break the circular dependency between `streamlib` and `streamlib-macros`. Both crates need to agree on execution-related types.

## Contents

- `ProcessExecution` - Execution mode enum (Continuous, Reactive, Manual)
- `ThreadPriority` - Thread scheduling priority (RealTime, High, Normal)
- `ExecutionConfig` - Container combining ProcessExecution + ThreadPriority

## Why these types are shared

The `#[streamlib::processor(execution = Reactive)]` macro needs to:
1. Parse the execution mode attribute → requires `ProcessExecution`
2. Generate code that creates `ExecutionConfig` with the parsed mode and `ThreadPriority`

Without this shared crate, we'd have duplicate type definitions that could drift apart.

## Adding new types

Before adding anything here, ask:
1. Is it needed by `streamlib-macros` at compile time?
2. Is there a duplicate definition that would exist in both crates otherwise?

When in doubt, keep types in `streamlib`.
