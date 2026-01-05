# Proposal: `ctx.time` Unified Timing API

## Status: Draft

## Problem Statement

Currently, processors that need timing information must:
1. Create their own `start_time` in `setup()` and calculate elapsed time manually
2. Use wall-clock time (`time.time()` in Python, `Instant::now()` in Rust)
3. Cannot coordinate animations/physics with other processors using a shared clock

## Design Principles

1. **Single monotonic clock** - One clock starts when runtime starts, always moves forward
2. **Centralized context** - Like `ctx.gpu`, `ctx.time` is shared/global
3. **Processors are mini-programs** - They decide how to handle timing, interpolation, delta computation
4. **Real-time streaming** - This is a real-time pipeline, not a game engine with replay/seek
5. **Lazy computation** - Values computed when requested

## Proposed API

### Python API

```python
def process(self, ctx):
    ctx.time.elapsed_secs    # float: Seconds since runtime.start()
    ctx.time.elapsed_ns      # int: Nanoseconds since runtime.start()
    ctx.time.now_ns          # int: Raw MediaClock::now() value
```

### Rust API

```rust
impl ProcessorContext {
    pub fn time(&self) -> &TimeContext;
}

impl TimeContext {
    /// Nanoseconds since runtime.start()
    pub fn elapsed_ns(&self) -> i64;

    /// Seconds since runtime.start()
    pub fn elapsed_secs(&self) -> f64;

    /// Raw MediaClock::now() value
    pub fn now_ns(&self) -> i64;
}
```

## Usage Examples

### Basic Animation

```python
def process(self, ctx):
    # Sine wave oscillation
    phase = ctx.time.elapsed_secs * 2.0 * math.pi  # 1Hz
    amplitude = math.sin(phase)
```

### Frame-Rate Independent Animation (Delta Time)

Delta time is the processor's responsibility:

```python
def setup(self, ctx):
    self.last_time = ctx.time.elapsed_secs
    self.position = 0.0
    self.velocity = 100.0  # pixels per second

def process(self, ctx):
    # Compute delta ourselves
    now = ctx.time.elapsed_secs
    delta = now - self.last_time
    self.last_time = now

    # Frame-rate independent movement
    self.position += self.velocity * delta
```

### Coordinated Multi-Processor Animation

All processors see the same clock, enabling coordination:

```python
# Processor A: Bouncing ball
def process(self, ctx):
    self.ball_y = abs(math.sin(ctx.time.elapsed_secs * 3)) * 200

# Processor B: Shadow (different processor, same clock)
def process(self, ctx):
    # Same elapsed_secs = shadow stays in sync
    ball_y = abs(math.sin(ctx.time.elapsed_secs * 3)) * 200
    self.shadow_scale = 1.0 - (ball_y / 200) * 0.5
```

### Handling Slow Operations (e.g., ML Inference)

Processors that take variable time handle it themselves:

```python
def process(self, ctx):
    start = ctx.time.elapsed_secs

    result = self.ml_model.infer(frame)  # Takes 50-200ms

    elapsed = ctx.time.elapsed_secs - start
    if elapsed > 0.1:
        logger.warning(f"ML inference took {elapsed:.3f}s")

    # Processor decides: use stale result, interpolate, skip, etc.
```

## Implementation

### Rust Core

```rust
// libs/streamlib/src/core/context/time_context.rs

use crate::core::clock::MediaClock;

pub struct TimeContext {
    runtime_start_ns: i64,
}

impl TimeContext {
    pub fn new() -> Self {
        Self {
            runtime_start_ns: MediaClock::now(),
        }
    }

    /// Nanoseconds since runtime start.
    pub fn elapsed_ns(&self) -> i64 {
        MediaClock::now() - self.runtime_start_ns
    }

    /// Seconds since runtime start.
    pub fn elapsed_secs(&self) -> f64 {
        self.elapsed_ns() as f64 / 1_000_000_000.0
    }

    /// Raw monotonic clock value.
    pub fn now_ns(&self) -> i64 {
        MediaClock::now()
    }
}
```

### Integration with RuntimeContext

```rust
// In RuntimeContext
pub struct RuntimeContext {
    // ... existing fields ...
    time: TimeContext,
}

impl RuntimeContext {
    pub fn time(&self) -> &TimeContext {
        &self.time
    }
}
```

### Python Bindings

```rust
// libs/streamlib-python/src/time_context_binding.rs

#[pyclass(name = "TimeContext")]
pub struct PyTimeContext {
    inner: Arc<TimeContext>,
}

#[pymethods]
impl PyTimeContext {
    #[getter]
    fn elapsed_ns(&self) -> i64 {
        self.inner.elapsed_ns()
    }

    #[getter]
    fn elapsed_secs(&self) -> f64 {
        self.inner.elapsed_secs()
    }

    #[getter]
    fn now_ns(&self) -> i64 {
        self.inner.now_ns()
    }

    fn __repr__(&self) -> String {
        format!("TimeContext(elapsed={:.3}s)", self.inner.elapsed_secs())
    }
}
```

### Add to PyProcessorContext

```rust
#[pymethods]
impl PyProcessorContext {
    #[getter]
    fn time(&self) -> PyTimeContext {
        PyTimeContext { inner: self.time_context.clone() }
    }
}
```

## Future Extensions

These are NOT part of the initial implementation but could be added later:

### Time Scale (Slow-Mo)

```python
runtime.set_time_scale(0.5)  # Half speed

# In processor - elapsed_secs would be scaled
# Processor can check ctx.time.scale if needed
```

### Pause/Resume

```python
runtime.pause()   # Clock stops advancing
runtime.resume()  # Clock continues
```

### Global Frame Counter

```python
ctx.time.frame_number  # Monotonically increasing counter
```

## Migration: Cyberpunk Processor

Current code:
```python
def setup(self, ctx):
    self.start_time = time.time()

def process(self, ctx):
    elapsed = time.time() - self.start_time
```

Updated code:
```python
def process(self, ctx):
    elapsed = ctx.time.elapsed_secs
```

## Summary

The `ctx.time` API provides:
- **Simplicity**: Just a clock that starts when runtime starts
- **Consistency**: All processors share the same clock
- **Flexibility**: Processors handle delta time, interpolation, etc. themselves
- **Zero overhead**: Values computed lazily on access

This follows the same pattern as `ctx.gpu` - centralized shared context, processors are independent mini-programs that use it as needed.
