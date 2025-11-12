# RFC 001: Rename Lifecycle Methods to `setup()` and `teardown()`

## Status
Proposed

## Summary
Rename processor lifecycle methods from `on_start()`/`on_stop()` to `setup()`/`teardown()` to clarify intent and separate resource allocation from processing state control.

## Motivation

### Current Issues
1. **Ambiguous naming**: `on_start()` could mean "start processing" or "initialize resources"
2. **Missing teardown**: `on_stop()` doesn't exist in most processors, leading to resource leaks
3. **Conflates concerns**: Initialization mixed with processing control

### Proposed Clarity
- `setup()` = One-time resource allocation (open devices, allocate buffers, etc.)
- `teardown()` = One-time resource cleanup (close devices, free memory, etc.)
- Processing state (start/stop/pause) = Event-driven (see RFC 002)

## Design

### New Lifecycle Methods

```rust
pub trait DynStreamProcessor: Send {
    /// Called once during processor initialization
    /// Use this to allocate resources, open devices, initialize state
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        Ok(()) // Default: no-op
    }

    /// Called once during processor shutdown
    /// Use this to free resources, close devices, cleanup
    fn teardown(&mut self) -> Result<()> {
        Ok(()) // Default: no-op
    }

    /// Called repeatedly to process data (existing)
    fn process(&mut self) -> Result<()>;
}
```

### Macro Detection

The `StreamProcessor` derive macro will auto-detect these methods:

```rust
#[derive(StreamProcessor)]
pub struct CameraProcessor {
    #[input] /* ... */
}

impl CameraProcessor {
    // Macro detects setup() and generates wrapper
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.camera = Camera::open()?;
        self.buffers = allocate_buffers(ctx.video.buffer_size);
        Ok(())
    }

    // Macro detects teardown() and generates wrapper
    fn teardown(&mut self) -> Result<()> {
        self.camera.close()?;
        self.buffers.clear();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Processing logic
    }
}
```

### Runtime Flow

```
Runtime::new()
  ↓
Runtime::add_processor()
  ↓
processor.setup(ctx)        ← Resource allocation
  ↓
Runtime::run()
  ↓
emit RuntimeEvent::Start    ← Begin processing (RFC 002)
  ↓
process() loops run
  ↓
Ctrl+C / Shutdown
  ↓
emit RuntimeEvent::Stop     ← Stop processing (RFC 002)
  ↓
processor.teardown()        ← Resource cleanup
```

## Implementation Plan

### Phase 1: Core Changes

#### 1. Update DynStreamProcessor Trait
**File**: `libs/streamlib/src/core/processor.rs`

```rust
pub trait DynStreamProcessor: Send {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        Ok(())
    }

    fn process(&mut self) -> Result<()>;

    // ... other methods ...
}
```

#### 2. Update Macro Code Generation
**File**: `libs/streamlib-macros/src/codegen.rs`

Update method detection to look for `setup`/`teardown` instead of `on_start`/`on_stop`:

```rust
// In generate_processor_impl():
let has_setup = analysis.has_method("setup");
let has_teardown = analysis.has_method("teardown");

// Generate trait impl:
if has_setup {
    quote! {
        fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
            Self::setup(self, ctx)
        }
    }
}

if has_teardown {
    quote! {
        fn teardown(&mut self) -> Result<()> {
            Self::teardown(self)
        }
    }
}
```

#### 3. Update Runtime
**File**: `libs/streamlib/src/core/runtime.rs`

```rust
impl StreamRuntime {
    pub fn add_processor<P: StreamProcessorFactory>(&mut self) -> Result<ProcessorHandle<P>> {
        let mut processor = P::from_config(P::Config::default())?;

        // Call setup with context
        processor.setup(&self.context)?;

        // Store processor...
    }

    pub fn run(&mut self) -> Result<()> {
        // Start processors (via events in RFC 002)

        // Run event loop
        if let Some(event_loop) = self.event_loop.take() {
            event_loop()?;
        }

        // Cleanup on shutdown
        for processor in &mut self.processors {
            processor.teardown()?;
        }

        Ok(())
    }
}
```

### Phase 2: Update All Processors

#### Core Processors
1. **ChordGeneratorProcessor** (`libs/streamlib/src/core/processors/chord_generator.rs`)
   - Rename `on_start()` → `setup()`
   - Add `teardown()` to stop thread and cleanup

2. **AudioOutputProcessor** (`libs/streamlib/src/apple/processors/audio_output.rs`)
   - Rename `on_start()` → `setup()`
   - Rename `on_stop()` → `teardown()`

3. **CameraProcessor** (`libs/streamlib/src/apple/processors/camera.rs`)
   - Currently initializes in `process()` - move to `setup()`?
   - Add `teardown()` to cleanup AVFoundation objects

4. **DisplayProcessor** (`libs/streamlib/src/apple/processors/display.rs`)
   - Move display link initialization to `setup()`
   - Add `teardown()` to cleanup display resources

### Phase 3: Update Examples

#### 1. camera-display
**File**: `examples/camera-display/src/main.rs`

No code changes needed (processors handle lifecycle), but verify behavior.

#### 2. audio-mixer-demo
**File**: `examples/audio-mixer-demo/src/main.rs`

No code changes needed, but verify behavior.

### Phase 4: Update Python Bindings

**File**: `libs/streamlib/src/python/runtime.rs`

```rust
#[pymethods]
impl PyStreamRuntime {
    #[new]
    fn new() -> PyResult<Self> {
        Ok(Self {
            runtime: StreamRuntime::new(),
        })
    }

    // No changes needed - setup() called internally by add_processor
    // teardown() called internally by runtime shutdown
}
```

### Phase 5: Documentation

#### 1. Update Macro Documentation
**File**: `libs/streamlib-macros/src/lib.rs`

Update examples and docstrings to use `setup()`/`teardown()`:

```rust
/// ```rust
/// impl MyProcessor {
///     fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
///         // Initialize resources
///         Ok(())
///     }
///
///     fn teardown(&mut self) -> Result<()> {
///         // Cleanup resources
///         Ok(())
///     }
///
///     fn process(&mut self) -> Result<()> {
///         // Process data
///         Ok(())
///     }
/// }
/// ```
```

#### 2. Update README
Show new lifecycle in examples.

#### 3. Add Migration Guide
**File**: `docs/migrations/on_start-to-setup.md`

```markdown
# Migration: on_start/on_stop → setup/teardown

## Quick Changes
- Rename `on_start()` → `setup()`
- Rename `on_stop()` → `teardown()`
- Ensure `teardown()` releases all resources

## Before
\`\`\`rust
fn on_start(&mut self, ctx: &RuntimeContext) -> Result<()> {
    self.device = Device::open()?;
    Ok(())
}
\`\`\`

## After
\`\`\`rust
fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
    self.device = Device::open()?;
    Ok(())
}

fn teardown(&mut self) -> Result<()> {
    self.device.close()?;
    Ok(())
}
\`\`\`
```

### Phase 6: Testing

#### 1. Unit Tests
**File**: `libs/streamlib/src/core/processor.rs` (test module)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_setup_teardown_lifecycle() {
        struct TestProcessor {
            setup_called: bool,
            teardown_called: bool,
        }

        impl DynStreamProcessor for TestProcessor {
            fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> {
                self.setup_called = true;
                Ok(())
            }

            fn teardown(&mut self) -> Result<()> {
                self.teardown_called = true;
                Ok(())
            }

            fn process(&mut self) -> Result<()> {
                Ok(())
            }
        }

        let mut processor = TestProcessor {
            setup_called: false,
            teardown_called: false,
        };

        let ctx = RuntimeContext::default();
        processor.setup(&ctx).unwrap();
        assert!(processor.setup_called);

        processor.teardown().unwrap();
        assert!(processor.teardown_called);
    }
}
```

#### 2. Integration Tests
**File**: `libs/streamlib/tests/lifecycle_test.rs`

```rust
#[test]
fn test_processor_lifecycle_order() {
    let mut runtime = StreamRuntime::new();

    // Add processor (should call setup)
    let processor = runtime.add_processor::<ChordGeneratorProcessor>()?;

    // Verify resources allocated
    // ...

    // Run and shutdown (should call teardown)
    // ...

    // Verify resources cleaned up
}
```

#### 3. Example Smoke Tests

```bash
# Test each example still works
cd examples/camera-display
cargo run

cd examples/audio-mixer-demo
cargo run
```

## Migration Path

### For Internal Processors (Phase 2)
1. Run find-replace: `on_start` → `setup`
2. Add `teardown()` methods where missing
3. Verify each processor cleans up resources

### For External Users
1. Provide deprecation warning (optional transitional period)
2. Update documentation with migration guide
3. Release as minor version bump (breaking change)

## Backward Compatibility

**Breaking Change**: Existing processors using `on_start()`/`on_stop()` will need updates.

**Mitigation**:
1. Clear migration guide
2. Compiler errors will point to exact locations
3. Automated search-replace for most cases

## Alternatives Considered

### 1. Keep `on_start`/`on_stop`, add `on_pause`/`on_resume`
**Rejected**: Still conflates initialization with state control

### 2. Use `init`/`deinit`
**Rejected**: Less clear than `setup`/`teardown`

### 3. Use `create`/`destroy`
**Rejected**: Sounds like constructor/destructor (which we already have)

## Success Metrics

1. All processors have `teardown()` implemented
2. No resource leaks detected in valgrind/instruments
3. All examples run without errors
4. All tests pass
5. Documentation updated

## Timeline

- **Week 1**: Phase 1 (Core changes)
- **Week 1**: Phase 2 (Update processors)
- **Week 1**: Phase 3 (Update examples)
- **Week 1**: Phase 4 (Python bindings)
- **Week 2**: Phase 5 (Documentation)
- **Week 2**: Phase 6 (Testing)

## Related RFCs

- RFC 002: Event Bus Architecture (defines start/stop/pause as events)

## Implementation Task List

Use this checklist when implementing this RFC. Copy tasks to your todo tracker as you begin work.

### Phase 1: Core Changes
- [ ] Update `DynStreamProcessor` trait in `libs/streamlib/src/core/processor.rs`
  - [ ] Rename `on_start()` → `setup()`
  - [ ] Add `teardown()` method with default implementation
- [ ] Update macro in `libs/streamlib-macros/src/codegen.rs`
  - [ ] Change method detection from "on_start" → "setup"
  - [ ] Change method detection from "on_stop" → "teardown"
  - [ ] Generate trait impl for setup/teardown
- [ ] Update `StreamRuntime` in `libs/streamlib/src/core/runtime.rs`
  - [ ] Call `setup()` in `add_processor()`
  - [ ] Call `teardown()` in shutdown sequence

### Phase 2: Update Processors
- [ ] Update `ChordGeneratorProcessor` (`libs/streamlib/src/core/processors/chord_generator.rs`)
  - [ ] Rename `on_start()` → `setup()`
  - [ ] Add `teardown()` method to stop thread and cleanup
  - [ ] Test processor starts and stops cleanly
- [ ] Update `AudioOutputProcessor` (`libs/streamlib/src/apple/processors/audio_output.rs`)
  - [ ] Rename `on_start()` → `setup()`
  - [ ] Rename `on_stop()` → `teardown()`
  - [ ] Test audio device cleanup
- [ ] Update `CameraProcessor` (`libs/streamlib/src/apple/processors/camera.rs`)
  - [ ] Rename lifecycle methods (if any)
  - [ ] Add `teardown()` to cleanup AVFoundation objects
  - [ ] Test camera device released properly
- [ ] Update `DisplayProcessor` (`libs/streamlib/src/apple/processors/display.rs`)
  - [ ] Move display link init to `setup()`
  - [ ] Add `teardown()` to cleanup display resources
  - [ ] Test window closes cleanly

### Phase 3: Update Examples
- [ ] Test `examples/camera-display`
  - [ ] Verify camera-display pipeline works
  - [ ] Check for resource leaks with Instruments
- [ ] Test `examples/audio-mixer-demo`
  - [ ] Verify audio playback works
  - [ ] Check for memory leaks

### Phase 4: Python Bindings
- [ ] Review `libs/streamlib/src/python/runtime.rs`
  - [ ] Verify no changes needed (lifecycle internal)
  - [ ] Test Python examples still work

### Phase 5: Documentation
- [ ] Update macro documentation (`libs/streamlib-macros/src/lib.rs`)
  - [ ] Change examples from `on_start` → `setup`
  - [ ] Add `teardown` examples
  - [ ] Update docstrings
- [ ] Create migration guide (`docs/migrations/on_start-to-setup.md`)
  - [ ] Document name changes
  - [ ] Provide before/after examples
  - [ ] Explain rationale
- [ ] Update README examples
  - [ ] Use new `setup()`/`teardown()` naming

### Phase 6: Testing
- [ ] Write unit tests (`libs/streamlib/src/core/processor.rs`)
  - [ ] Test `setup()` called on processor creation
  - [ ] Test `teardown()` called on shutdown
  - [ ] Test lifecycle order (setup → process → teardown)
- [ ] Write integration tests (`libs/streamlib/tests/lifecycle_test.rs`)
  - [ ] Test processor resource allocation/cleanup
  - [ ] Test error handling in setup/teardown
- [ ] Run all examples as smoke tests
  - [ ] `cargo run --example camera-display`
  - [ ] `cargo run --example audio-mixer-demo`
- [ ] Check for resource leaks
  - [ ] Run with valgrind (Linux) or Instruments (macOS)
  - [ ] Verify no leaked file descriptors
  - [ ] Verify no leaked memory

### Final Checks
- [ ] All tests passing (`cargo test`)
- [ ] All examples working (`cargo run --example ...`)
- [ ] Documentation updated
- [ ] Migration guide created
- [ ] No resource leaks detected
- [ ] Ready for code review
