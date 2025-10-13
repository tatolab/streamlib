# Performance Benchmarks

## WebGPU GPU-First Architecture

streamlib uses **WebGPU** as the unified GPU backend, providing cross-platform performance across macOS, Windows, and Linux.

## Expected Performance

WebGPU provides native GPU performance on all platforms:

### Resolution Guidelines

| Resolution | Performance Target | Backend |
|------------|-------------------|---------|
| 640×480 (SD) | 60 FPS | CPU or GPU |
| 1280×720 (HD) | 60 FPS | GPU recommended |
| 1920×1080 (Full HD) | 30-60 FPS | GPU |
| 3840×2160 (4K) | 30 FPS | GPU required |

### When to Use GPU vs CPU

#### ✅ Use GPU for:
- **Large resolutions** (≥1080p)
- **Heavy compute** (ML inference, complex effects)
- **Per-pixel operations** (shaders, filters)
- **Long pipelines** (many GPU handlers chained)

#### ✅ Use CPU for:
- **Small resolutions** (≤720p simple operations)
- **Simple drawing** (text, basic shapes)
- **Sequential operations** (can't parallelize)

## WebGPU Performance Characteristics

### Zero-Copy GPU Pipeline

WebGPU enables true zero-copy pipelines where data stays on GPU from capture to display:

```
Camera (GPU) → Blur (GPU) → Compositor (GPU) → Display (GPU)
     ↓              ↓              ↓                ↓
  Platform      WGSL           WGSL           Platform
  specific      compute        render         specific
  capture       shader         pipeline       present
```

**Benefits:**
- **No CPU transfers** - Data stays on GPU throughout
- **Hardware acceleration** - Native GPU performance on all platforms
- **Scalable** - Performance scales with GPU capability
- **Cross-platform** - Same code runs on Metal, D3D12, Vulkan

### Platform-Specific Zero-Copy Optimizations

#### macOS (Metal Backend)
- AVFoundation → IOSurface → WebGPU texture
- Zero-copy camera capture
- Native Metal performance

#### Windows (D3D12 Backend)
- MediaFoundation → D3D11 texture → WebGPU
- Zero-copy via D3D11 interop
- DXGI hardware acceleration

#### Linux (Vulkan Backend)
- v4l2 → DMA-BUF → WebGPU texture
- Zero-copy via Vulkan external memory
- VA-API hardware decode

## Performance Optimization Best Practices

### 1. Stay on GPU

```python
# ✅ GOOD: Entire pipeline on GPU
camera_gpu → blur_gpu → compositor_gpu → display_gpu
# Zero CPU↔GPU transfers

# ❌ BAD: Bouncing between CPU and GPU
camera_gpu → cpu_filter → gpu_effect → cpu_display
# 3 transfers, significant overhead
```

### 2. Use WebGPU Compute Shaders

WGSL compute shaders provide optimal GPU performance:

```python
# GPU blur via WGSL compute shader
blur_pipeline = gpu.create_compute_pipeline("""
    @group(0) @binding(0) var input_texture: texture_2d<f32>;
    @group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

    @compute @workgroup_size(8, 8)
    fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
        // Gaussian blur implementation (runs in parallel on GPU)
    }
""")
```

**Performance:** Processes millions of pixels in parallel

### 3. Pre-Allocate GPU Resources

Allocate textures and buffers in `on_start()`:

```python
async def on_start(self):
    # Allocate once
    self.temp_texture = gpu_context.create_texture(1920, 1080)
    self.compute_pipeline = gpu_context.create_compute_pipeline(shader)

async def process(self, tick):
    # Reuse (no allocation overhead)
    # ... use self.temp_texture ...
```

### 4. Minimize CPU↔GPU Transfers

Only transfer at pipeline boundaries:

```python
# Camera (GPU) → Processing (GPU) → Display (GPU)
# Only 0 transfers if display supports GPU textures
# Or 1 transfer if display requires CPU (WebGPU → CPU for cv2.imshow)
```

## Benchmarking Your Pipeline

### Profile GPU Operations

Use WebGPU timestamp queries to measure GPU operations:

```python
query_set = device.create_query_set(type="timestamp", count=2)

encoder = device.create_command_encoder()
encoder.write_timestamp(query_set, 0)  # Start
compute_pass = encoder.begin_compute_pass()
# ... GPU operations ...
compute_pass.end()
encoder.write_timestamp(query_set, 1)  # End
device.queue.submit([encoder.finish()])

# Read back timing
resolve_buffer = device.create_buffer(...)
encoder.resolve_query_set(query_set, 0, 2, resolve_buffer, 0)
```

### Monitor FPS

Use built-in FPS tracking:

```python
runtime = StreamRuntime(fps=60)
await runtime.start()

# Runtime tracks actual FPS
print(f"Actual FPS: {runtime.measured_fps}")
print(f"Frame drops: {runtime.frame_drops}")
```

## Performance Targets

### Broadcast Quality Standards

- **NTSC**: 29.97 FPS (broadcast standard)
- **PAL**: 25 FPS (European broadcast)
- **Cinema**: 24 FPS (film standard)
- **Interactive**: 60 FPS (gaming/live streaming)

### Latency Requirements

- **Interactive applications**: < 50ms end-to-end
- **Live streaming**: < 200ms end-to-end
- **Broadcast**: < 1 second end-to-end

## Summary

WebGPU GPU-first architecture provides:

1. ✅ **Cross-platform performance** - Metal, D3D12, Vulkan backends
2. ✅ **Zero-copy pipelines** - Data stays on GPU
3. ✅ **Hardware acceleration** - Native GPU performance
4. ✅ **Scalable** - Performance scales with GPU capability
5. ✅ **Simple** - Same API across all platforms

**Key insight:** With WebGPU, you get native GPU performance on all platforms without platform-specific code.
