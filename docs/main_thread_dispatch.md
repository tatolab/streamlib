# Runtime Thread Dispatch

## Overview

The `RuntimeContext` provides utilities for dispatching work to the runtime thread from processor worker threads. This is essential for platform APIs that require execution on a specific thread with an active run loop (e.g., AVFoundation on macOS).

> **Terminology Note**: The "runtime thread" is the thread where StreamRuntime orchestration happens.
> On macOS, this is the main thread (NSApplication run loop) because Apple frameworks require it.
> On other platforms, it may be a different thread depending on platform requirements.

## When to Use

Use runtime thread dispatch when:

- **Platform APIs require it**: AVFoundation, UIKit/AppKit, and other macOS/iOS frameworks require certain operations to run on the runtime thread
- **Thread-specific resources**: APIs that check for CFRunLoop, NSRunLoop, or thread-local storage
- **UI updates**: Any UI-related code must run on the runtime thread

**Do NOT use for:**
- General computation (adds latency)
- High-frequency operations (can bottleneck runtime thread)
- Operations that already work on worker threads

## API

### `run_on_runtime_thread_async`

Dispatches a closure to execute on the runtime thread asynchronously (non-blocking).

```rust
pub fn run_on_runtime_thread_async<F>(&self, f: F)
where
    F: FnOnce() + Send + 'static
```

**Characteristics:**
- Non-blocking - calling thread continues immediately
- No return value
- Queued for execution on runtime thread's event loop
- Executed in FIFO order (serial execution on main queue)

**Example:**

```rust
impl Processor for MyProcessor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.ctx = Some(ctx.clone());
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        let frame = self.pull_input()?;

        // Dispatch to runtime thread
        if let Some(ref ctx) = self.ctx {
            ctx.run_on_runtime_thread_async(move || {
                // This runs on runtime thread
                update_ui_with_frame(frame);
            });
        }

        Ok(())
    }
}
```

### `run_on_runtime_thread_blocking`

Dispatches a closure to execute on the runtime thread and waits for the result (blocking).

```rust
pub fn run_on_runtime_thread_blocking<F, R>(&self, f: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static
```

**Characteristics:**
- Blocking - calling thread waits for completion
- Returns a value
- Uses channel-based synchronization
- **Will deadlock if called FROM the runtime thread when it's blocked**

**Example:**

```rust
impl Processor for MyProcessor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // Create AVFoundation writer on runtime thread
        let writer = ctx.run_on_runtime_thread_blocking(|| {
            AVAssetWriter::assetWriterWithURL_fileType(url, file_type)
        });

        self.writer = Some(writer);
        Ok(())
    }
}
```

## Usage Patterns

### Pattern 1: Lazy Initialization

Initialize platform resources on first `process()` call (after event loop is running):

```rust
struct MyProcessor {
    ctx: Option<RuntimeContext>,
    writer: Option<AVAssetWriter>,
    initialized: bool,
}

impl Processor for MyProcessor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // Just store context, don't initialize yet
        self.ctx = Some(ctx.clone());
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if !self.initialized {
            // Runtime event loop is running now
            let writer = self.ctx.as_ref().unwrap().run_on_runtime_thread_blocking(|| {
                AVAssetWriter::create(...)
            });
            self.writer = Some(writer);
            self.initialized = true;
        }

        // Use writer...
        Ok(())
    }
}
```

### Pattern 2: Shared State with Arc<Mutex<>>

Share mutable state between worker thread and runtime thread:

```rust
struct MyProcessor {
    ctx: Option<RuntimeContext>,
    writer: Arc<Mutex<Option<AVAssetWriter>>>,
}

impl Processor for MyProcessor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.ctx = Some(ctx.clone());

        let writer = Arc::clone(&self.writer);
        ctx.run_on_runtime_thread_async(move || {
            let w = AVAssetWriter::create(...);
            *writer.lock().unwrap() = Some(w);
        });

        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        let frame = self.pull_input()?;
        let writer = Arc::clone(&self.writer);

        self.ctx.as_ref().unwrap().run_on_runtime_thread_async(move || {
            if let Some(w) = writer.lock().unwrap().as_mut() {
                w.append_frame(frame);
            }
        });

        Ok(())
    }
}
```

### Pattern 3: Teardown with Blocking Wait

Ensure cleanup completes before returning:

```rust
impl Processor for MyProcessor {
    fn teardown(&mut self) -> Result<()> {
        if let Some(ref ctx) = self.ctx {
            ctx.run_on_runtime_thread_blocking(|| {
                // Finalize writer on runtime thread
                self.writer.finishWriting();
            });
        }
        Ok(())
    }
}
```

## Platform Notes

### macOS

- Uses GCD's `DispatchQueue::main()`
- Integrates with NSApplication's event loop
- Runtime thread must call `runtime.wait_for_signal()` to start event loop
- Closures queued before event loop starts execute once it begins

### Other Platforms

- Currently no-op (executes immediately on current thread)
- Future: Could be extended for platform-specific threading requirements

## Performance Considerations

### Latency

- Async dispatch adds minimal latency (~microseconds to queue)
- Blocking dispatch adds latency = (queue time + execution time)
- Runtime thread serializes all dispatched work

### Best Practices

1. **Keep closures fast**: Runtime thread processes UI events and other system work
2. **Batch when possible**: Combine multiple operations into one dispatch
3. **Prefer async**: Use blocking only when you need the return value
4. **Avoid hot paths**: Don't dispatch every single frame unless necessary

### Example: Batching

```rust
// Bad: Dispatch for each sample
for sample in samples {
    ctx.run_on_runtime_thread_async(move || process_sample(sample));
}

// Good: Dispatch once for all samples
let samples = samples.clone();
ctx.run_on_runtime_thread_async(move || {
    for sample in samples {
        process_sample(sample);
    }
});
```

## Troubleshooting

### Deadlock on Blocking Call

**Symptom**: Application hangs when calling `run_on_runtime_thread_blocking()`

**Cause**: Called from runtime thread while it's blocked (would wait for itself)

**Solution**: Only call from worker threads, or use async variant

### Closure Not Executing

**Symptom**: Async closure never runs

**Cause**: Runtime thread event loop not started

**Solution**: Ensure `runtime.wait_for_signal()` is called and running

### Compilation Error: Closure Lifetime

**Symptom**: `closure may outlive the current function`

**Cause**: Closure captures references instead of owned values

**Solution**: Clone Arc pointers or move owned data into closure

```rust
// Bad
let data = &self.some_data;
ctx.run_on_runtime_thread_async(|| use_data(data)); // Error

// Good
let data = Arc::clone(&self.some_data);
ctx.run_on_runtime_thread_async(move || use_data(data)); // OK
```

## See Also

- [MP4 Writer Threading](./mp4_writer_threading.md) - Real-world example
- [AVFoundation Threading Fix](./avfoundation_threading_fix.md) - Architecture rationale
