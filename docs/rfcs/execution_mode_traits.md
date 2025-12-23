# RFC: Execution Mode Traits

## Status: Draft (In Discussion)

## Pre-requisite: Create `streamlib-types` Crate

Before implementing execution mode traits, we should consolidate shared types into a new `streamlib-types` crate that both `streamlib` and `streamlib-macros` can depend on.

### Current Duplication

**Macro crate** (`libs/streamlib-macros/src/attributes.rs:14-24`):
```rust
pub enum ExecutionMode {
    Continuous { interval_ms: Option<u32> },
    Reactive,
    Manual,
}
```

**Streamlib** (`libs/streamlib/src/core/execution/process_execution.rs:22-111`):
```rust
pub enum ProcessExecution {
    Continuous { interval_ms: u32 },
    Reactive,
    Manual,
}
```

**Problem:** The macro parses strings ("Continuous", "Reactive", "Manual") and generates code that references `::streamlib::core::ProcessExecution`. It cannot use the actual type because that would create a circular dependency.

### Candidates for `streamlib-types`

| Type | Current Location | Macro Uses? | Notes |
|------|------------------|-------------|-------|
| `ProcessExecution` | `core/execution/process_execution.rs` | Yes (generates refs) | Primary candidate - eliminates `ExecutionMode` duplication |
| `ExecutionConfig` | `core/execution/mod.rs` | Yes (generates refs) | Wraps ProcessExecution |
| `LinkPortType` | `core/links/traits/link_port_type.rs` | Yes (generates refs) | Port type matching |
| `Config` trait | `core/processors/traits/config.rs` | Yes (trait bounds) | Processor config trait |
| `Processor` trait | `core/processors/traits/processor.rs` | Yes (generates impls) | **Will be replaced by mode traits** |
| `GeneratedProcessor` trait | `core/processors/__generated_private/` | Yes (generates impls) | Internal macro trait |
| `Result`/`StreamError` | `core/error.rs` | Yes (return types) | Error handling |

### Proposed `streamlib-types` Contents

- `ProcessExecution` enum (eliminates duplicate `ExecutionMode`)
- `ExecutionConfig` struct
- `LinkPortType` enum
- `Config` trait (+ `ConfigValidationError`)
- `StreamError` enum and `Result` type alias
- `ProcessorLifecycle` trait (new)
- `ManualProcessor` trait (new)
- `ContinuousProcessor` trait (new)
- `ReactiveProcessor` trait (new)

### Benefits

1. **Single source of truth** - No more `ExecutionMode` ↔ `ProcessExecution` duplication
2. **Type safety in macro** - Macro can use `ProcessExecution::Manual` directly instead of string parsing
3. **Simpler codegen** - Less string matching, more direct type usage
4. **Cleaner architecture** - Clear separation of foundational types

### Migration Path

1. Create `libs/streamlib-types` crate with all contents listed above
2. Define new mode traits (`ProcessorLifecycle`, `ManualProcessor`, etc.) in `streamlib-types`
3. Update `streamlib-macros` to depend on `streamlib-types`
4. Update `streamlib` to depend on and re-export from `streamlib-types`
5. Delete duplicate `ExecutionMode` from macro crate
6. Delete old `Processor` trait from streamlib (replaced by mode traits)
7. Update all existing processors to implement mode-specific traits

---

## Session Restoration Point

**If you are Claude resuming after compaction, read this entire document first. This captures the conversation state.**

---

## Current Architecture

### Layers (Top to Bottom)

```
┌─────────────────────────────────────────────────────────────┐
│  Runtime / Executor / Scheduler                             │
│  - thread_runner.rs dispatches based on ProcessExecution    │
│  - spawn_processor_op.rs calls guard.__generated_setup()    │
└─────────────────────┬───────────────────────────────────────┘
                      │ talks to (via DynGeneratedProcessor)
                      ▼
┌─────────────────────────────────────────────────────────────┐
│  Generated Processor (implements GeneratedProcessor trait)  │
│  - Created by #[streamlib::processor] macro ON THE STRUCT   │
│  - Stored in ProcessorInstance as Box<dyn DynGeneratedProcessor>
│  - Has __generated_setup(), __generated_teardown(), process()
│  - Internally holds user's code in ::Processor field        │
└─────────────────────┬───────────────────────────────────────┘
                      │ calls via Processor trait (adapter pattern)
                      ▼
┌─────────────────────────────────────────────────────────────┐
│  User's Implementation                                      │
│  - impl Processor for MyProcessor::Processor                │
│  - Today: same Processor trait regardless of mode           │
│  - Has setup(), teardown(), process()                       │
└─────────────────────────────────────────────────────────────┘
```

### Key Traits (Current)

1. **`Processor`** (`processor.rs`) - Public trait users implement
   - `setup(&mut self, ctx) -> impl Future`
   - `teardown(&mut self) -> impl Future`
   - `process(&mut self) -> Result<()>`

2. **`GeneratedProcessor`** (`generated_processor.rs`) - Internal trait macro implements
   - `__generated_setup()`, `__generated_teardown()` - call user's setup/teardown
   - `process()` - calls user's process
   - Port management, config, descriptors, etc.

3. **`DynGeneratedProcessor`** (`generated_processor_impl.rs`) - Object-safe wrapper
   - Blanket impl for all `GeneratedProcessor` types
   - Uses `BoxFuture` for async methods
   - This is what `ProcessorInstance` actually holds

### How Execution Mode Works Today

1. User specifies `execution = Reactive` in macro attribute
2. Macro generates `execution_config()` returning that mode
3. `spawn_processor_op.rs` extracts config: `processor_arc.lock().execution_config()`
4. `thread_runner.rs` matches on `ProcessExecution` enum and runs different loops:
   - `run_continuous_mode()` - loop with interval
   - `run_reactive_mode()` - wait for input messages
   - `run_manual_mode()` - call process() once, then wait for shutdown

### Key Files

- `libs/streamlib/src/core/processors/traits/processor.rs` - Public Processor trait
- `libs/streamlib/src/core/processors/__generated_private/generated_processor.rs` - GeneratedProcessor trait
- `libs/streamlib/src/core/processors/__generated_private/generated_processor_impl.rs` - DynGeneratedProcessor
- `libs/streamlib/src/core/execution/process_execution.rs` - ProcessExecution enum
- `libs/streamlib/src/core/execution/thread_runner.rs` - Mode-specific loops
- `libs/streamlib/src/core/compiler/compiler_ops/spawn_processor_op.rs` - Spawns processor threads

---

## Goal

**Convert execution modes (Manual, Continuous, Reactive) into separate Rust traits.**

User implements mode-specific trait instead of generic `Processor` trait.

---

## Design Discussion (In Progress)

### Reconsidered: Keep the Mode Attribute

Jonathan asked: "What if we do keep the mode attribute, would that simplify things?"

**Answer: Yes.** If we keep `execution = Manual`:
- Macro knows at expansion time which mode
- Macro generates code expecting specific trait (e.g., `ManualProcessor`)
- If user implements wrong trait → compile error
- Trait becomes contract: "you said Manual, so implement these methods"

**Benefit:** User could implement all three traits and switch mode via attribute without changing impl code.

### Open Questions from Jonathan

1. **thread_runner behavior differs per mode** - Continuous/Reactive have pause/stop event handling, Manual doesn't. How does this change?

2. **Adapter access pattern** - Today: `guard.process()` calls through DynGeneratedProcessor.
   - Should it become `guard.adapter().mode_specific_method()`?
   - Or does generated processor handle mode internally?

3. **Cascading changes** - `spawn_processor_op` calls `guard.__generated_setup()`. Would this change to `guard.adapter().try_setup()`?

4. **Why `__generated_setup` exists** - These are internal hooks that:
   - Live on GeneratedProcessor trait
   - Generated code implements them
   - They internally call user's `Processor::setup()`
   - Double-underscore = "internal, don't call directly"

---

## Proposed Trait Structure

### Base Trait (shared lifecycle)

```rust
trait ProcessorLifecycle {
    fn setup(&mut self, _ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))  // default no-op
    }
    fn teardown(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))  // default no-op
    }
}
```

### Mode-Specific Traits

```rust
/// Runtime calls process() in a loop with optional interval.
trait ContinuousProcessor: ProcessorLifecycle {
    fn process(&mut self) -> Result<()>;
    fn on_pause(&mut self) -> Result<()> { Ok(()) }   // default no-op
    fn on_resume(&mut self) -> Result<()> { Ok(()) }  // default no-op
}

/// Runtime calls process() when input data arrives.
trait ReactiveProcessor: ProcessorLifecycle {
    fn process(&mut self) -> Result<()>;
    fn on_pause(&mut self) -> Result<()> { Ok(()) }   // default no-op
    fn on_resume(&mut self) -> Result<()> { Ok(()) }  // default no-op
}

/// User controls timing via external callbacks (hardware, vsync, etc.).
/// Runtime provides lifecycle hooks.
trait ManualProcessor: ProcessorLifecycle {
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn on_pause(&mut self) -> Result<()> { Ok(()) }   // default no-op
    fn on_resume(&mut self) -> Result<()> { Ok(()) }  // default no-op
}
```

### User Code Examples

**Minimal (defaults only):**
```rust
#[streamlib::processor(execution = Reactive)]
pub struct MyFilter {
    #[streamlib::input]
    input: LinkInput<VideoFrame>,
    #[streamlib::output]
    output: LinkOutput<VideoFrame>,
}

// ProcessorLifecycle impl not needed - defaults are fine
impl ReactiveProcessor for MyFilter::Processor {
    fn process(&mut self) -> Result<()> {
        // read input, transform, write output
        Ok(())
    }
    // on_pause/on_resume use defaults
}
```

**With custom lifecycle:**
```rust
#[streamlib::processor(execution = Manual)]
pub struct AudioOutput {
    #[streamlib::config]
    config: AudioOutputConfig,
}

impl ProcessorLifecycle for AudioOutput::Processor {
    fn setup(&mut self, ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send {
        async move {
            // initialize audio device
            Ok(())
        }
    }

    fn teardown(&mut self) -> impl Future<Output = Result<()>> + Send {
        async move {
            // cleanup audio device
            Ok(())
        }
    }
}

impl ManualProcessor for AudioOutput::Processor {
    fn start(&mut self) -> Result<()> {
        // start audio callback
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // stop audio callback
        Ok(())
    }
    // on_pause/on_resume use defaults
}
```

---

## Decisions Made

- [x] Keep the `execution = X` attribute on macro (simplifies compile-time knowledge)
- [x] Base trait `ProcessorLifecycle` with `setup()` and `teardown()` (default no-ops)
- [x] Mode traits extend `ProcessorLifecycle`
- [x] All modes have `on_pause()` and `on_resume()` (default no-ops)
- [x] Continuous/Reactive have `process()`
- [x] Manual has `start()` and `stop()` instead of `process()`
- [x] All lifecycle methods have default no-op implementations

## Decisions NOT Made (Require Discussion)

- [x] How thread_runner interacts with mode-specific traits → matches on `execution_config()`, calls appropriate methods
- [x] How DynGeneratedProcessor exposes mode methods → all methods on trait, contract enforced at compile time
- [x] What to do if wrong method called → `unreachable!()` (violators of the contract shall be handled swiftly)
- [ ] See "Open Questions" section below for remaining items

---

## Design Decision: Contract-Based Method Dispatch

### The Insight

The `execution_config()` method already tells us the mode at runtime. The macro enforces the contract at **compile time**:

1. User declares `execution = Manual` in macro attribute
2. Macro generates code that calls `ManualProcessor::start()`, `ManualProcessor::stop()`
3. If user doesn't implement `ManualProcessor` → **compile error**

So at runtime, when `execution_config()` returns `Manual`, we **know** the processor implements `ManualProcessor`. The contract is enforced at compile time; runtime just follows through.

### Chosen Approach: DynGeneratedProcessor Has All Methods

DynGeneratedProcessor exposes all mode methods. Generated code routes to the user's trait impl. thread_runner's existing mode-matching guarantees only valid methods are called:

```rust
trait DynGeneratedProcessor {
    // Existing methods...

    // Mode-specific (generated code routes to user's trait)
    fn start(&mut self) -> Result<()>;   // Manual only
    fn stop(&mut self) -> Result<()>;    // Manual only
    fn process(&mut self) -> Result<()>; // Continuous/Reactive
    fn on_pause(&mut self) -> Result<()>;
    fn on_resume(&mut self) -> Result<()>;
}

// In thread_runner - same pattern as today:
match execution_config {
    ProcessExecution::Manual { .. } => {
        // Contract: execution=Manual → implements ManualProcessor
        // Therefore start()/stop() are valid
        guard.start()?;
        // wait for shutdown...
        guard.stop()?;
    }
    ProcessExecution::Reactive { .. } => {
        // Contract: execution=Reactive → implements ReactiveProcessor
        // Therefore process() is valid
        guard.process()?;
    }
    ProcessExecution::Continuous { .. } => {
        // Contract: execution=Continuous → implements ContinuousProcessor
        guard.process()?;
    }
}
```

### What Happens If Wrong Method Is Called?

If thread_runner (or other runtime code) calls `guard.start()` on a Reactive processor, it's a **bug in the runtime**, not user code. The generated impl can:
- `unreachable!()` - panic with clear message (catches runtime bugs fast)
- Return error - more defensive but hides bugs

**Decision:** Use `unreachable!("start() called on non-Manual processor")` - violators of the contract shall be handled swiftly.

---

## Open Questions

### Q1: How does spawn_processor_op change?

**Current behavior:** `spawn_processor_op.rs` calls `guard.__generated_setup(ctx)` to initialize the processor before the thread_runner loop starts.

**Question:** Does `__generated_setup()` remain unchanged, or does it need to call `ProcessorLifecycle::setup()` differently now that we have the base trait?

Specifically:
- Today: `__generated_setup()` calls `Processor::setup()`
- After: Should it call `ProcessorLifecycle::setup()` instead?
- Are there any mode-specific setup steps (e.g., should Manual mode do something different during setup)?

### Q2: How does the macro generate different code per mode?

**Current behavior:** Macro generates a single `GeneratedProcessor` impl that calls `Processor::process()`.

**Question:** How does the macro know which trait methods to call in the generated code?

Options:
1. Match on the `execution` attribute and generate different `process()`/`start()`/`stop()` implementations
2. Generate a single impl that checks mode at runtime (seems wrong - defeats compile-time contract)
3. Something else?

Example of what generated code might look like for Manual:
```rust
// Generated for #[streamlib::processor(execution = Manual)]
impl GeneratedProcessor for MyProcessor::Generated {
    fn start(&mut self) -> Result<()> {
        <Inner as ManualProcessor>::start(&mut self.processor)
    }
    fn stop(&mut self) -> Result<()> {
        <Inner as ManualProcessor>::stop(&mut self.processor)
    }
    fn process(&mut self) -> Result<()> {
        unreachable!("process() called on Manual processor")
    }
}
```

### Q3: What about the existing `Processor` trait?

**Current behavior:** Users implement `Processor` trait with `setup()`, `teardown()`, `process()`.

**Question:** What happens to the existing `Processor` trait?

Options:
1. **Delete it** - replaced by `ProcessorLifecycle` + mode traits
2. **Keep as alias** - `type Processor = ReactiveProcessor` for backwards compatibility
3. **Deprecate** - keep but mark deprecated, remove in future version

### Q4: Do we need to update ProcessExecution enum?

**Current behavior:** `ProcessExecution` enum has `Manual`, `Continuous`, `Reactive` variants with config data.

**Question:** Does this enum need to change, or does it remain as the runtime discriminant for mode?

---

## Context for Claude (Post-Compaction)

If you just compacted and are reading this:

1. Jonathan wants to carefully guard each decision - don't make assumptions
2. Previous attempts at this refactor failed because Claude took too much autonomy
3. Work incrementally - discuss before implementing
4. The user drives design, you assist and ask clarifying questions

**Decisions already made:**
- Keep `execution = X` attribute on macro
- Base trait `ProcessorLifecycle` with setup/teardown (default no-ops)
- Mode traits: `ContinuousProcessor`, `ReactiveProcessor`, `ManualProcessor`
- All modes have `on_pause()`, `on_resume()` (default no-ops)
- Continuous/Reactive have `process()`, Manual has `start()`/`stop()`
- ManualProcessor, ContinuousProcessor, ReactiveProcessor ARE the adapters (no separate ProcessorAdapter trait)
- Contract-based dispatch: DynGeneratedProcessor has all mode methods, macro enforces correct trait at compile time, thread_runner matches on `execution_config()` to call appropriate methods

**Remaining open questions (see "Open Questions" section above):**
- Q1: How does spawn_processor_op change?
- Q2: How does the macro generate different code per mode?
- Q3: What happens to the existing `Processor` trait?
- Q4: Do we need to update ProcessExecution enum?
