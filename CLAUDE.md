# CLAUDE.md

This file provides guidance to Claude Code when working with code in this repository.

## Licensing

StreamLib is licensed under the **Business Source License 1.1** (BUSL-1.1).

- All new Rust files must include the copyright header
- Do NOT suggest MIT, Apache, or other licenses for this codebase
- Do NOT modify license files without explicit approval

**Copyright header for new files:**
```rust
// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
```

See [LICENSE](LICENSE) and [docs/license/](docs/license/) for full terms.

---

## Naming Standards

Names should be understood with **ZERO context**. An AI agent who just woke up with amnesia should understand what something does from the name alone.

### What Makes a Good Name
1. **Encodes relationships**: Where it comes from, where it goes
2. **Encodes role**: What it DOES in the system, not what it IS technically
3. **Explicit direction**: `FromUpstream`, `ToDownstream`, `Input`, `Output`
4. **No generic words alone**: Never just `Inner`, `State`, `Manager`, `Handler`, `Context`

### Examples
```rust
// ✅ CORRECT - explicit, self-documenting
LinkOutputDataWriter         // writes data from a link output
LinkInputDataReader          // reads data for a link input
LinkInputFromUpstreamProcessor   // binding FROM upstream TO this input
LinkOutputToDownstreamProcessor  // binding FROM this output TO downstream
add_link_output_data_writer()    // adds a data writer to a link output
set_link_output_to_processor_message_writer()  // 43 chars is FINE

// ❌ WRONG - too short, requires context
Writer, Reader, Producer, Consumer
Connection, Binding, Handle
ctx, mgr, conn, buf, cfg
```

### The Test
Ask: "If I saw this name 200 lines away from its declaration, would I know exactly what it is?"

Use `/refine-name <current_name>` for naming suggestions that follow this pattern.

---

## Prohibited Patterns

1. `unimplemented!()` or `todo!()` in library code (tests/examples are OK)
2. "Temporary" hacks or workarounds
3. Methods that do nothing: `fn foo() { /* no-op */ }`
4. Bypassing type safety "just to make it compile"
5. Tests that paper over broken APIs — if you have to mock half the system or ignore errors, the test is lying

---

## Documentation Standards

Minimal, focused on developer experience (autocomplete, IDE tooltips).

**Document**: One-line descriptions for structs/enums/traits/functions. Public fields only if name isn't self-explanatory.

**Do NOT document**: File-level `//!` module docs, `# Example` sections, `# Usage` sections, ASCII diagrams, design rationale, verbose parameter descriptions.

**Style**: One line preferred. Use intra-doc links: [`TypeName`] not `` `TypeName` ``. No examples in docs — examples belong in `examples/`.

Run `cargo doc -p streamlib --no-deps` to verify.

---

## Conventions

### Error Handling
- Use `StreamError` enum from `streamlib::core::error`
- Return `Result<T>` from all fallible operations
- Prefer `?` operator over `.unwrap()` in library code
- `.unwrap()` acceptable in examples and tests

### Code Organization
- **Platform-agnostic code**: `libs/streamlib/src/core/`
- **macOS/iOS code**: `libs/streamlib/src/apple/`
- **DO NOT** use `#[cfg]` inside platform-specific directories (already conditionally compiled)

### Work Tracking
Prefer the Task system for tracking multi-step work and plan mode implementations.
