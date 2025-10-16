# Option B Analysis: Using WGPUDevice C API Directly

## Summary

Option B implementation created at `/Users/fonta/Repositories/tatolab/gst-mcp-tools/packages/streamlib/rust/iosurface_hal/src/lib_option_b.rs`

**Status: 🚧 SCAFFOLDED - Compiles but faces fundamental limitation**

## What Was Implemented

### 1. C API Type Definitions

Created Rust FFI bindings for wgpu-native C API types (matching webgpu.h):

```rust
// Opaque pointers
pub type WGPUDevice = *mut WGPUDeviceImpl;
pub type WGPUTexture = *mut WGPUTextureImpl;

// Structures
pub struct WGPUTextureDescriptor { ... }
pub struct WGPUExtent3D { ... }
pub struct WGPUStringView { ... }

// Constants
pub const WGPU_TEXTURE_FORMAT_BGRA8_UNORM: u32 = 0x00000017;
pub const WGPU_TEXTURE_USAGE_TEXTURE_BINDING: u32 = 0x00000004;
```

### 2. Extern C Function Declarations

Declared wgpu-native C API functions:

```rust
extern "C" {
    fn wgpuDeviceCreateTexture(
        device: WGPUDevice,
        descriptor: *const WGPUTextureDescriptor,
    ) -> WGPUTexture;

    fn wgpuTextureRelease(texture: WGPUTexture);
}
```

### 3. New Function Signature

```rust
#[pyfunction]
fn create_texture_from_iosurface_c_api(
    device_ptr: u64,      // WGPUDevice C pointer (not wgpu::Device)
    iosurface_ptr: u64,
) -> PyResult<(u64, u32, u32)>
```

### 4. Implementation Approach

1. **Accept WGPUDevice C pointer** (instead of wgpu::Device Rust type)
2. **Create Metal texture from IOSurface** (zero-copy via Objective-C)
3. **Convert Metal → HAL texture** (using wgpu_hal::metal::Device::texture_from_raw)
4. **Call wgpuDeviceCreateTexture** (C API function)

## Compilation Results

### Compilation Status: ✅ Partial Success

The code compiles successfully but **fails at linking**:

```
error: linking with `cc` failed: exit status: 1
  = note: ld: Undefined symbols:
    "_wgpuDeviceCreateTexture", referenced from:
        iosurface_hal::create_texture_from_iosurface_c_api::...
    "_wgpuTextureRelease", referenced from:
        iosurface_hal::release_texture_c_api::...
  ld: symbol(s) not found for architecture arm64
```

**Why:** We declared extern "C" functions but didn't link against wgpu-native library.

**To fix:** Would need to add to Cargo.toml:
```toml
[build-dependencies]
cc = "1.0"

# And add build script to link wgpu-native
```

## Fundamental Limitation Discovered

### ❌ The C API Cannot Import External Textures

**CRITICAL ISSUE:** `wgpuDeviceCreateTexture()` **creates a NEW texture** with GPU-allocated memory. It does NOT allow wrapping an existing Metal texture.

```rust
// This creates a NEW texture (allocates GPU memory):
let wgpu_texture = wgpuDeviceCreateTexture(wgpu_device, &wgpu_desc);

// But we need to WRAP an existing Metal texture (from IOSurface):
// ❌ No C API function exists for this!
```

### What We Need But C API Doesn't Provide

We need something like:
```rust
// DOES NOT EXIST in wgpu-native C API:
fn wgpuDeviceImportMetalTexture(
    device: WGPUDevice,
    metal_texture: *mut c_void,
    descriptor: *const WGPUTextureDescriptor,
) -> WGPUTexture;
```

## Solutions Explored

### 1. wgpu-native Extension (NOT FOUND)

**Searched for:** Metal-specific extension functions in wgpu-native
**Result:** Standard C API doesn't expose texture import

### 2. wgpu-core Direct Access (RISKY)

**Idea:** Bypass C API and access wgpu-core internals:
```rust
// Cast WGPUDevice → wgpu_core::id::DeviceId
// Access global registry: wgpu_core::hub::global::Global
// Call internal register_texture() method
```

**Problem:**
- Bypasses C API safety guarantees
- Requires internal knowledge of wgpu-core
- Fragile (breaks with wgpu version changes)
- NOT RECOMMENDED

### 3. Return Metal Texture to Python (CURRENT APPROACH)

**This is what lib.rs currently does:**
```rust
// Rust: Create Metal texture, return pointer
let mtl_texture = create_from_iosurface(...);
Ok((mtl_texture as u64, width, height))
```

```python
# Python: Wrap using wgpu-py
metal_texture_ptr, w, h = create_metal_texture_from_iosurface(iosurface)
wgpu_texture = device.create_texture_from_hal(metal_texture_ptr, ...)
```

### 4. Use wgpu::Device API (ORIGINAL PROBLEM)

**Original approach that hangs:**
```rust
let device = device_ptr as *mut wgpu::Device;
let device_ref = &*device;  // ❌ Hangs here
```

**Why it hangs:** Unknown - possibly:
- Invalid pointer from Python
- Ownership/lifetime issues
- wgpu-py internals incompatibility

## Conclusion

### Option B Cannot Achieve Zero-Copy Import

**The wgpu-native C API fundamentally lacks texture import functionality.**

Three paths forward:

1. **Keep current lib.rs approach** (return Metal texture to Python)
   - ✅ Works with zero-copy
   - ✅ Simple and clean
   - ❌ Requires Python-side wrapping

2. **Add wgpu-native extension** (would require upstream changes)
   - Submit PR to wgpu-native for `wgpuDeviceImportMetalTexture()`
   - Wait for release and adoption
   - Timeline: months

3. **Access wgpu-core internals** (not recommended)
   - Fragile and unsafe
   - Breaks encapsulation
   - Would break with wgpu updates

### Recommendation

**Stick with current lib.rs approach** (return Metal texture pointer to Python).

The C API limitation means Option B cannot work as intended without:
- Upstream wgpu-native changes, OR
- Unsafe internal registry manipulation, OR
- Using Rust wgpu::Device API (which currently hangs)

The current approach is the most practical solution.

## Files Created

1. `/Users/fonta/Repositories/tatolab/gst-mcp-tools/packages/streamlib/rust/iosurface_hal/src/lib_option_b.rs`
   - Complete C API implementation
   - ~310 lines with extensive documentation
   - Shows why C API approach hits limitations

2. `/Users/fonta/Repositories/tatolab/gst-mcp-tools/packages/streamlib/rust/iosurface_hal/OPTION_B_ANALYSIS.md`
   - This document

## C API Functions Used

From wgpu-native (wgpu.h / webgpu.h):

```c
// Texture creation (allocates NEW texture)
WGPUTexture wgpuDeviceCreateTexture(
    WGPUDevice device,
    WGPUTextureDescriptor const * descriptor
);

// Texture cleanup
void wgpuTextureRelease(WGPUTexture texture);
```

**Missing (would need for zero-copy import):**
```c
// Does NOT exist in standard API:
WGPUTexture wgpuDeviceImportMetalTexture(
    WGPUDevice device,
    void* metalTexture,
    WGPUTextureDescriptor const * descriptor
);
```

## Next Steps

If you want to pursue Option B further:

1. **Add wgpu-native linking** to Cargo.toml
2. **Find wgpu-native library** location
3. **Link against it** in build.rs
4. **Test if linking works** (it will compile and link)
5. **Discover texture import limitation** (same conclusion)

Or:

**Accept current approach** and focus on making it robust in Python.
