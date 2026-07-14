# streamlib-processor-schema

Processor schema types and YAML parser shared between the `streamlib`
runtime and `streamlib-macros`.

## Purpose

Both crates need to agree on the on-disk shape of processor manifests
and the small handful of execution-related enums those manifests
declare. This crate is the single source of truth for those types so
the macro and the runtime can't drift.

## Contents

- `ProcessorSchema`, `ProcessorPortSchema`, `ProcessorConfigSchema`,
  `ProcessorStateField`, `ProcessorLanguage` — typed mirror of a
  processor YAML manifest.
- `parse_processor_yaml`, `parse_processor_yaml_file` — YAML parser
  with the project's manifest contract.
- `ProcessExecution`, `ThreadPriority`, `ExecutionConfig` — execution
  mode + priority types referenced by the
  `#[streamlib::processor(execution = Reactive)]` attribute.
- `compute_schema_id`, `to_pascal_case`, `to_snake_case` — helpers
  shared between codegen sites and the runtime.

## Why these types are shared

Without this crate, `streamlib` and `streamlib-macros` would each
have to define `ProcessExecution` / `ExecutionConfig` etc.; a value
constructed in one wouldn't be the same Rust type as the value
expected by the other, and the macro couldn't emit code that
referenced the runtime's types directly.

## Adding new types

Before adding anything here, ask:

1. Is it needed by `streamlib-macros` at compile time?
2. Is there a duplicate definition that would otherwise exist in both
   the runtime and the macros crate?

If neither is true, keep the type in `streamlib`.

## Sibling crate

`streamlib-jtd-codegen` is the JTD-codegen *pipeline* — separate
concern. It reads schemas (some of which use the
`ProcessorSchema` types from this crate) and emits typed
Rust/Python/TypeScript bindings into `_generated_/`.
