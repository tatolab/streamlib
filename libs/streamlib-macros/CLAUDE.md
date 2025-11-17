# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

`streamlib-macros` is a procedural macro crate that provides the `#[derive(StreamProcessor)]` macro for the streamlib project. This macro automates boilerplate code generation for stream processors.

## Building and Testing

```bash
# Build the macro crate
cargo build

# Run tests (compile-time verification)
cargo test

# Test macro expansion output
cargo expand --test macro_tests

# Build from workspace root
cd ../.. && cargo build -p streamlib-macros
```

## Architecture

### Three-Phase Pipeline

The macro follows a clean separation of concerns:

1. **attributes.rs** - Parse `#[processor(...)]`, `#[input(...)]`, `#[output(...)]` attributes
2. **analysis.rs** - Classify fields as ports/config/state, extract type information
3. **codegen.rs** - Generate trait implementations and helper methods

### Key Data Structures

**PortField** - Represents an input or output port:
- `field_name` - Rust field identifier
- `port_name` - Port name (from attribute or field name)
- `direction` - Input or Output
- `message_type` - Generic type parameter (e.g., `VideoFrame`)
- `is_arc_wrapped` - Whether field is `Arc<StreamInput/Output<T>>`

**ConfigField** - Non-port fields that become config struct fields

**StateField** - Runtime state with default initialization

**AnalysisResult** - Complete analysis of the struct, fed to code generator

### What Gets Generated

For a processor struct, the macro generates:

1. **Port view structs** (`MyProcessorInputs`, `MyProcessorOutputs`) - Ergonomic port access
2. **Convenience methods** (`inputs()`, `outputs()`, `video_out()`, etc.)
3. **Port introspection** (`input_ports()`, `output_ports()`)
4. **StreamElement trait** - Core lifecycle methods
5. **StreamProcessor trait** - Type information and scheduling config
6. **Optional: unsafe impl Send** - If `unsafe_send` attribute present

### Arc-Wrapped Output Enforcement

**Critical validation** (lib.rs:190-214): Output ports MUST be Arc-wrapped.

This ensures cloned ports (e.g., in callbacks) share wakeup channels for Push mode scheduling.

The macro produces a compile error with helpful message if outputs aren't Arc-wrapped.

## Code Organization

```
src/
├── lib.rs          # Entry point, macro definition, Arc validation
├── attributes.rs   # Parse #[processor], #[input], #[output] attrs
├── analysis.rs     # Field classification, type extraction
└── codegen.rs      # Code generation (trait impls, helpers)

tests/
├── macro_tests.rs       # Integration tests (compile-time)
└── complete_impl_test.rs # Full trait implementation test
```

## Common Development Tasks

### Adding a New Processor Attribute

1. Add field to `ProcessorAttributes` struct in attributes.rs
2. Parse it in `ProcessorAttributes::parse()` method
3. Use it in code generation (codegen.rs)

Example:
```rust
// attributes.rs
pub struct ProcessorAttributes {
    pub new_attr: Option<String>,
}

// In parse() method:
if meta.path.is_ident("new_attr") {
    let value = parse_string_value(&meta)?;
    result.new_attr = Some(value);
    return Ok(());
}

// codegen.rs
let new_attr_value = &analysis.processor_attrs.new_attr;
```

### Adding a New Port Attribute

Same pattern as processor attributes but in `PortAttributes`:
```rust
#[input(new_field = "value")]
```

### Modifying Generated Code

All code generation happens in codegen.rs using the `quote!` macro.

Find the relevant `generate_*()` function and modify the TokenStream:
```rust
quote! {
    // Generated code here
    impl #struct_name {
        // ...
    }
}
```

### Testing Macro Changes

1. **Compile-time tests** - Add test to tests/macro_tests.rs:
   ```rust
   #[derive(StreamProcessor)]
   struct TestProcessor {
       #[input] input: Arc<StreamInput<VideoFrame>>,
       #[output] output: Arc<StreamOutput<VideoFrame>>,
   }
   ```

2. **Expand and inspect** - Use cargo expand:
   ```bash
   cargo expand --test macro_tests
   ```

3. **Full integration** - Test with actual streamlib by building examples

## Key Design Decisions

### Why Require Arc-Wrapped Outputs?

Push mode processors need wakeup channels to notify downstream processors when data is available. If outputs aren't Arc-wrapped, cloned ports (e.g., stored in closures) won't share the wakeup channel, breaking Push mode.

Enforcing Arc at compile time prevents runtime bugs.

### Why Separate Analysis and Codegen?

**Analysis phase** deals with syn types (AST parsing, type extraction)
**Codegen phase** deals with quote types (TokenStream generation)

This separation makes the code easier to understand and test.

### Why Use quote! Macro?

The `quote!` macro provides:
- Interpolation (`#variable`)
- Hygiene (avoids name collisions)
- Type safety (compile-time checks)
- Readable code (looks like Rust, not strings)

## Common Patterns

### Parsing String Attributes

```rust
// #[processor(description = "my description")]
if meta.path.is_ident("description") {
    let value = parse_string_value(&meta)?;
    result.description = Some(value);
    return Ok(());
}
```

### Parsing Type Attributes

```rust
// #[processor(config = MyConfig)]
if meta.path.is_ident("config") {
    let value: Type = meta.value()?.parse()?;
    result.config_type = Some(value);
    return Ok(());
}
```

### Extracting Generic Type Parameters

```rust
// StreamInput<VideoFrame> -> VideoFrame
fn extract_message_type(ty: &Type) -> Result<Type> {
    if let Type::Path(type_path) = ty {
        if let PathArguments::AngleBracketed(args) = &segment.arguments {
            if let GenericArgument::Type(msg_ty) = &args.args[0] {
                return Ok(msg_ty.clone());
            }
        }
    }
    Err(Error::new_spanned(ty, "Expected StreamInput<T> or StreamOutput<T>"))
}
```

### Generating Methods

```rust
fn generate_method(name: &Ident) -> TokenStream {
    quote! {
        pub fn #name(&self) -> SomeType {
            // Method implementation
        }
    }
}
```

## Debugging Tips

### Enable Macro Debugging

Set environment variable:
```bash
RUST_LOG=trace cargo build
```

### Inspect Generated Code

```bash
# Expand macros for tests
cargo expand --test macro_tests

# Expand specific test
cargo expand --test macro_tests test_minimal_processor

# Expand with color
cargo expand --test macro_tests --color=always | less -R
```

### Common Errors

**"no rules expected this token"** - Syntax error in quote! macro, check interpolation

**"expected Type, found..."** - Incorrect parsing in attributes.rs

**"cannot find type `Foo` in this scope"** - Generated code references type not in scope, check imports

**"trait bound not satisfied"** - Generated code assumes trait impl that doesn't exist

## Integration with streamlib

The macro generates code that expects these types from streamlib:

- `StreamInput<T>`, `StreamOutput<T>` - Port types
- `StreamElement`, `StreamProcessor` - Core traits
- `SchedulingConfig`, `SchedulingMode` - Scheduling types
- `PortMessage` - Sealed trait for valid port message types
- `ProcessorDescriptor` - Metadata type

When modifying the macro, ensure generated code matches streamlib's trait definitions.

## Performance Considerations

Procedural macros run at compile time, so runtime performance isn't a concern. However:

- Keep generated code minimal (less to compile)
- Avoid generating duplicate impls
- Use `#[inline]` for trivial generated methods
- Consider compile time impact of complex parsing

## TODOs and Cleanup

The codebase has several `#![allow(dead_code)]` items and TODO comments:

**analysis.rs:6-7** - Unused helper functions may be leftover from old implementation
**codegen.rs:16-18** - Many unused code generation functions

These should be reviewed and cleaned up if confirmed unnecessary.

## Related Documentation

- streamlib traits: `../streamlib/src/core/traits/`
- Example usage: `../../examples/`
- Main macro docs: `src/lib.rs` top-level doc comments
