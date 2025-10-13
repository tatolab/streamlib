# Compositor Optimization

## WebGPU GPU-First Approach

In the WebGPU GPU-first architecture, compositor operations are performed entirely on the GPU using:

1. **Render Pipelines** - Alpha blending via GPU fragment shaders (WGSL)
2. **Compute Shaders** - Complex compositing operations stay on GPU
3. **Zero-Copy** - All video frames remain as GPU textures throughout pipeline

This eliminates the need for CPU-side alpha blending optimizations (NumPy, uint16 arithmetic, etc.) that were previously required.

## Performance

GPU compositing via WebGPU render pipelines provides:
- **Native performance** - Hardware-accelerated alpha blending
- **Parallel execution** - Per-pixel operations run in parallel
- **Zero CPU overhead** - No CPU↔GPU transfers during compositing
- **Scalable** - Performance scales with GPU capability, not CPU

## Implementation

See architecture documentation for compositor handler implementation:
- WebGPU render pipeline for multi-layer compositing
- WGSL fragment shaders for alpha blending
- Pre-allocated GPU texture ring buffers

**Reference:** `docs/internal/architecture.md` - WebGPU GPU-First Architecture section
