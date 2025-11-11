# Simplify Macro: Eliminate Port Wiring Boilerplate

## Current Problem

The `#[derive(StreamProcessor)]` macro generates `_impl` methods in a separate `impl ChordGeneratorProcessor` block, but users still need to manually add delegation methods to their `impl StreamProcessor` trait blocks:

```rust
#[derive(StreamProcessor)]
struct ChordGeneratorProcessor {
    #[output]
    chord: Arc<StreamOutput<AudioFrame<2>>>,
}

impl StreamProcessor for ChordGeneratorProcessor {
    type Config = ChordGeneratorConfig;

    fn process(&mut self) -> Result<()> {
        // Custom logic
    }

    // ❌ BOILERPLATE - Must manually add these delegation methods
    fn get_output_port_type(&self, port_name: &str) -> Option<PortType> {
        self.get_output_port_type_impl(port_name)
    }

    fn wire_output_producer(&mut self, port_name: &str, producer: Box<dyn Any + Send>) -> bool {
        self.wire_output_producer_impl(port_name, producer)
    }
}
```

## Why This Happens

1. The macro generates these methods:
   - `get_output_port_type_impl()` - in `impl ChordGeneratorProcessor`
   - `wire_output_producer_impl()` - in `impl ChordGeneratorProcessor`
   - `get_output_port_type()` - in `impl ChordGeneratorProcessor` ❌ WRONG LOCATION
   - `wire_output_producer()` - in `impl ChordGeneratorProcessor` ❌ WRONG LOCATION

2. The trait methods need to be inside `impl StreamProcessor for ChordGeneratorProcessor`, not `impl ChordGeneratorProcessor`

3. Since they're in the wrong impl block, they don't override the trait's default implementations (which return `None`/`false`)

## Current Boilerplate Required

Every processor needs to manually add **2 delegation methods per port direction**:

**For processors with inputs:**
```rust
fn get_input_port_type(&self, port_name: &str) -> Option<PortType> {
    self.get_input_port_type_impl(port_name)
}

fn wire_input_consumer(&mut self, port_name: &str, consumer: Box<dyn Any + Send>) -> bool {
    self.wire_input_consumer_impl(port_name, consumer)
}
```

**For processors with outputs:**
```rust
fn get_output_port_type(&self, port_name: &str) -> Option<PortType> {
    self.get_output_port_type_impl(port_name)
}

fn wire_output_producer(&mut self, port_name: &str, producer: Box<dyn Any + Send>) -> bool {
    self.wire_output_producer_impl(port_name, producer)
}
```

## Processors Currently Requiring Manual Boilerplate

All processors using `#[derive(StreamProcessor)]` need this:

### Sources (have outputs)
- `libs/streamlib/src/core/sources/chord_generator.rs` ✅ Fixed
- `libs/streamlib/src/core/sources/camera.rs`
- `libs/streamlib/src/core/sources/audio_capture.rs`
- `libs/streamlib/src/apple/sources/camera.rs`
- `libs/streamlib/src/apple/sources/audio_capture.rs`

### Sinks (have inputs)
- `libs/streamlib/src/apple/sinks/audio_output.rs` ✅ Fixed
- `libs/streamlib/src/apple/sinks/display.rs`
- `libs/streamlib/src/core/sinks/audio_output.rs`
- `libs/streamlib/src/core/sinks/display.rs`

### Transformers (have both inputs and outputs)
- `libs/streamlib/src/core/transformers/audio_mixer.rs`
- `libs/streamlib/src/core/transformers/clap_effect.rs`
- `libs/streamlib/src/core/transformers/simple_passthrough.rs`
- `libs/streamlib/src/core/transformers/performance_overlay.rs`

## Solutions Considered

### Option 1: Keep Current Pattern (CHOSEN FOR NOW)
**Status:** This is what we're doing currently

**Pros:**
- Matches main branch pattern
- Simple to understand
- Works with current Rust macro limitations

**Cons:**
- 2-4 lines of boilerplate per processor
- Easy to forget during migration
- Not ideal developer experience

**Action Required:**
- Add delegation methods to all processors listed above
- Document the pattern clearly in macro docs

### Option 2: Generate Complete Trait Impl
**Status:** Not feasible

**Why it doesn't work:**
- Derive macros can't know about user's custom `process()` implementation
- Would require users to put ALL logic in helper methods
- Major breaking change to architecture

### Option 3: Separate Trait Pattern
**Status:** Possible future refactor

**Concept:**
```rust
// Users implement this trait with just their logic
impl ProcessorCore for MyProcessor {
    type Config = MyConfig;
    fn from_config(config: Self::Config) -> Result<Self> { ... }
    fn process(&mut self) -> Result<()> { ... }
}

// Macro generates blanket impl for StreamProcessor
impl<T: ProcessorCore> StreamProcessor for T where ... {
    // Forwards to ProcessorCore + adds all port methods
}
```

**Pros:**
- Eliminates ALL boilerplate
- Clean separation of concerns

**Cons:**
- Major breaking change
- Requires refactoring entire codebase
- Complexity in macro implementation

## Immediate Action Plan

1. **Add delegation methods to all processors** (2-3 hours)
   - For each processor file listed above
   - Check if it has inputs, outputs, or both
   - Add the appropriate 2-4 delegation methods

2. **Update macro documentation** (30 mins)
   - Add clear example showing delegation methods
   - Explain why they're needed
   - Show the pattern for inputs, outputs, and both

3. **Create test to catch missing methods** (1 hour)
   - Integration test that verifies port introspection works
   - Will catch if delegation methods are forgotten

## Long-Term Solution (Future)

If the boilerplate becomes too painful, consider the "Separate Trait Pattern" (Option 3) as a future architecture improvement. This would be a streamlib 2.0 level change.

## Files to Modify

### Priority 1: Processors Used in Examples
- `libs/streamlib/src/core/sources/chord_generator.rs` ✅ Done
- `libs/streamlib/src/apple/sinks/audio_output.rs` ✅ Done
- `libs/streamlib/src/core/transformers/audio_mixer.rs` (audio-mixer-demo)

### Priority 2: Other Apple Processors
- `libs/streamlib/src/apple/sources/camera.rs`
- `libs/streamlib/src/apple/sources/audio_capture.rs`
- `libs/streamlib/src/apple/sinks/display.rs`

### Priority 3: Platform-Agnostic Processors
- `libs/streamlib/src/core/sources/camera.rs`
- `libs/streamlib/src/core/sources/audio_capture.rs`
- `libs/streamlib/src/core/sinks/audio_output.rs`
- `libs/streamlib/src/core/sinks/display.rs`
- `libs/streamlib/src/core/transformers/clap_effect.rs`
- `libs/streamlib/src/core/transformers/simple_passthrough.rs`
- `libs/streamlib/src/core/transformers/performance_overlay.rs`

## Template for Adding Delegation Methods

```rust
impl StreamProcessor for YourProcessor {
    // ... existing methods (process, from_config, etc.) ...

    // Add at the end of the impl block:

    // For processors with inputs:
    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::PortType> {
        self.get_input_port_type_impl(port_name)
    }

    fn wire_input_consumer(&mut self, port_name: &str, consumer: Box<dyn std::any::Any + Send>) -> bool {
        self.wire_input_consumer_impl(port_name, consumer)
    }

    // For processors with outputs:
    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::PortType> {
        self.get_output_port_type_impl(port_name)
    }

    fn wire_output_producer(&mut self, port_name: &str, producer: Box<dyn std::any::Any + Send>) -> bool {
        self.wire_output_producer_impl(port_name, producer)
    }
}
```

## Success Criteria

- [ ] All processors compile without errors
- [ ] All examples run successfully (especially audio-mixer-demo)
- [ ] Port introspection returns correct types (not None)
- [ ] Connections can be wired successfully
- [ ] No runtime "port not found" errors
