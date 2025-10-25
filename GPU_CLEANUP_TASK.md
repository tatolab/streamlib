# GPU Object Cleanup - Task B

## Problem Statement

The current GPU wrapper implementation in `libs/streamlib/src/python/gpu_wrappers.rs` creates GPU resources via raw pointers (`usize` handles) but doesn't properly clean them up when Python objects are garbage collected. This leads to memory leaks.

## Root Cause: Unclear Ownership Semantics

The fundamental issue is that we can't distinguish between **owned** and **borrowed** handles at cleanup time:

### Owned Resources (Created via `Box::into_raw()`)
Most GPU resources are created by wrapping a wgpu object in a Box and converting to a raw pointer:

```rust
// In gpu_wrappers.rs - these CREATE owned resources:
let handle = Box::into_raw(Box::new(shader_module)) as usize;  // Line 501
let handle = Box::into_raw(Box::new(buffer)) as usize;         // Line 517
let handle = Box::into_raw(Box::new(layout)) as usize;         // Line 545
let handle = Box::into_raw(Box::new(pipeline_layout)) as usize; // Line 569
let handle = Box::into_raw(Box::new(pipeline)) as usize;       // Line 598
let handle = Box::into_raw(Box::new(bind_group)) as usize;     // Line 657
let handle = Box::into_raw(Box::new(encoder)) as usize;        // Line 668
let handle = Box::into_raw(Box::new(view)) as usize;           // Line 471
let handle = Box::into_raw(Box::new(compute_pass)) as usize;   // Line 368
let handle = Box::into_raw(Box::new(texture)) as usize;        // types_ext.rs:197
```

These handles **must** be cleaned up with `Box::from_raw()` when the Python object is dropped.

### Borrowed Resources (From Arc)
However, `VideoFrame.data` returns a **borrowed** pointer to a texture owned by an `Arc<wgpu::Texture>`:

```rust
// In types.rs:53-58
fn data(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
    use super::gpu_wrappers::PyWgpuTexture;
    // Get raw pointer to the Arc's inner texture
    let texture_ptr = self.inner.texture.as_ref() as *const wgpu::Texture as usize;
    let texture_wrapper = PyWgpuTexture { handle: texture_ptr };
    Ok(Py::new(py, texture_wrapper)?.into_py(py))
}
```

This handle **must NOT** be cleaned up - it's borrowed from the Arc and will be freed when the Arc is dropped.

### The Problem
At cleanup time (in `__del__`), we can't tell if a `PyWgpuTexture::handle` points to:
- An owned `Box<wgpu::Texture>` (must call `Box::from_raw()`)
- A borrowed `&wgpu::Texture` from Arc (must NOT free)

Calling `Box::from_raw()` on a borrowed pointer causes undefined behavior (double-free).

## Current State

All GPU wrapper structs store handles as raw pointers:

```rust
pub struct PyWgpuBuffer { handle: usize }
pub struct PyWgpuTexture { handle: usize }
pub struct PyWgpuShaderModule { handle: usize }
pub struct PyWgpuBindGroupLayout { handle: usize }
pub struct PyWgpuPipelineLayout { handle: usize }
pub struct PyWgpuComputePipeline { handle: usize }
pub struct PyWgpuBindGroup { handle: usize }
pub struct PyWgpuCommandEncoder { handle: usize }
pub struct PyWgpuComputePass { handle: usize }
pub struct PyWgpuTextureView { handle: usize }
```

None of these implement `Drop` or `__del__`, so resources leak.

## Proposed Solutions

### Option 1: Ownership Flag (Quick Fix)
Add a boolean flag to track ownership:

```rust
pub struct PyWgpuTexture {
    handle: usize,
    owned: bool,  // true if created via Box::into_raw(), false if borrowed from Arc
}

impl Drop for PyWgpuTexture {
    fn drop(&mut self) {
        if self.owned && self.handle != 0 {
            unsafe {
                let _ = Box::from_raw(self.handle as *mut wgpu::Texture);
            }
        }
    }
}
```

**Pros:**
- Minimal code changes
- Fixes the immediate leak

**Cons:**
- Brittle - easy to forget to set the flag correctly
- Doesn't solve the fundamental design issue
- Extra memory overhead (8 bytes per wrapper)

### Option 2: Arc Throughout (Proper Fix)
Redesign to use `Arc<T>` for all GPU resources instead of raw pointers:

```rust
pub struct PyWgpuTexture {
    texture: Arc<wgpu::Texture>,
}

// No Drop needed - Arc handles cleanup automatically
// Can freely clone and share
```

**Pros:**
- Proper Rust ownership semantics
- Automatic cleanup via Arc::drop
- Thread-safe sharing
- No unsafe code needed for cleanup

**Cons:**
- Requires refactoring all GPU wrapper creation sites
- Need to store Arc in GpuContext instead of raw objects
- More pervasive changes to codebase

### Option 3: Separate Types (Most Explicit)
Create distinct types for owned vs borrowed:

```rust
pub struct PyWgpuTextureOwned { handle: usize }
pub struct PyWgpuTextureBorrowed { handle: usize }

impl Drop for PyWgpuTextureOwned { /* cleanup */ }
// PyWgpuTextureBorrowed has no Drop
```

**Pros:**
- Makes ownership explicit in the type system
- Compiler enforces correct usage

**Cons:**
- Doubles the number of wrapper types
- Python code needs to handle two different types

## Recommended Approach

**Option 2 (Arc Throughout)** is the cleanest long-term solution because:
1. Aligns with Rust's ownership philosophy
2. Eliminates entire class of bugs (use-after-free, double-free)
3. Enables safe multi-threading
4. Simplifies the codebase (no unsafe cleanup code)

## Implementation Plan

### Phase 1: Update GpuContext
Change `GpuContext` to store GPU resources in Arc:

```rust
// In core/gpu_context.rs
pub struct GpuContext {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    // ...
}
```

### Phase 2: Update Wrapper Types
Change all wrapper types to hold Arc instead of usize:

```rust
pub struct PyWgpuTexture {
    texture: Arc<wgpu::Texture>,
}

pub struct PyWgpuBuffer {
    buffer: Arc<wgpu::Buffer>,
}
// ... etc
```

### Phase 3: Update Creation Sites
Update all places that create wrappers to use Arc::new():

```rust
// Old:
let handle = Box::into_raw(Box::new(texture)) as usize;
PyWgpuTexture { handle }

// New:
PyWgpuTexture { texture: Arc::new(texture) }
```

### Phase 4: Update VideoFrame
Change VideoFrame.data to clone the Arc:

```rust
// In types.rs
fn data(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
    let texture_wrapper = PyWgpuTexture {
        texture: self.inner.texture.clone()  // Arc::clone, cheap
    };
    Ok(Py::new(py, texture_wrapper)?.into_py(py))
}
```

### Phase 5: Update All Usages
Update method implementations to use `.as_ref()` instead of raw pointer derefs:

```rust
// Old:
unsafe { &*(self.handle as *const wgpu::Texture) }

// New:
self.texture.as_ref()  // Safe!
```

## Testing Checklist

After implementing:
- [ ] Run `camera_with_bouncing_ball.py` example
- [ ] Verify no crashes on exit (cleanup working)
- [ ] Check for memory leaks using `top` or Activity Monitor
- [ ] Ensure zero-copy still works (no performance regression)
- [ ] Run all Python processor examples
- [ ] Verify thread safety (if using async processors)

## Files to Modify

1. `libs/streamlib-core/src/gpu_context.rs` - Store Arc in GpuContext
2. `libs/streamlib/src/python/gpu_wrappers.rs` - Change all wrapper types
3. `libs/streamlib/src/python/types.rs` - Update VideoFrame.data
4. `libs/streamlib/src/python/types_ext.rs` - Update create_texture()

## References

- Current implementation: `libs/streamlib/src/python/gpu_wrappers.rs`
- Borrowed texture example: `libs/streamlib/src/python/types.rs:53-58`
- Owned texture example: `libs/streamlib/src/python/types_ext.rs:197`
- wgpu Arc usage: Check how wgpu-rs examples handle Device/Queue sharing

## Timeline Estimate

- Option 1 (flags): 2-4 hours
- Option 2 (Arc): 1-2 days (recommended)
- Option 3 (separate types): 3-5 days

## Notes

- The current implementation **works** but leaks memory on long-running processes
- Not a critical bug for short-lived examples
- Becomes important for:
  - Production deployments (robots, helmets)
  - Long-running streaming sessions
  - Memory-constrained devices (Jetson)

---

**Created:** 2025-10-25
**Status:** Deferred (requires architectural redesign)
**Priority:** Medium (works now, but should be fixed before production use)
