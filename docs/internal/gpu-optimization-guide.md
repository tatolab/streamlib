# GPU Optimization Guide

## Overview

This guide explains how to write efficient GPU-accelerated handlers using streamlib's runtime-provided GPU utilities. The runtime handles the complexity, letting you write simple, fast code.

## Architecture

### Runtime-Level GPU Infrastructure

Each runtime node manages its own GPU resources:

```python
runtime = StreamRuntime(fps=60, enable_gpu=True)
# Runtime automatically:
# - Detects backend (MPS/CUDA/CPU)
# - Creates memory pool for tensor reuse
# - Provides transfer optimizer
# - Batches operations when possible
```

**Distributed-Friendly**: Each node in a mesh network has its own GPU context.

```
Phone (Runtime A)     →     Edge GPU (Runtime B)     →     Cloud (Runtime C)
├─ GPU: none                ├─ GPU: CUDA                    ├─ GPU: none
├─ Camera Handler           ├─ Depth Estimator (GPU)        ├─ Display Handler
└─ Network →                └─ Network →
```

### GPU Context Components

The runtime provides:

1. **Memory Pool** (`gpu_context['memory_pool']`)
   - Reuses tensor allocations
   - Reduces 0.5ms overhead per frame
   - Automatically cleans up on runtime stop

2. **Transfer Optimizer** (`gpu_context['transfer_optimizer']`)
   - Tracks where data lives (CPU/GPU)
   - Delays transfers until necessary
   - Batches multiple transfers

3. **Batch Processor** (`gpu_context['batch_processor']`)
   - Collects operations
   - Executes in batches
   - Reduces kernel launch overhead

## Best Practices

### ✅ DO: Use Runtime GPU Context

```python
class OptimizedHandler(StreamHandler):
    def __init__(self):
        super().__init__('optimized')
        self.device = None  # Set in on_start()

    async def on_start(self):
        """Access runtime GPU context here."""
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']
            # Pre-allocate resources using runtime utilities

    async def process(self, tick: TimedTick):
        # Use memory pool for tensor allocation
        if self._runtime.gpu_context:
            mem_pool = self._runtime.gpu_context['memory_pool']
            frame = mem_pool.allocate((height, width, 3), 'uint8')
        else:
            frame = torch.empty((height, width, 3), dtype=torch.uint8)

        # ... process frame on GPU ...
```

**Why**: Runtime handles device detection, memory management, and cleanup.

### ✅ DO: Pre-Compute Resources in `on_start()`

```python
async def on_start(self):
    if self._runtime.gpu_context:
        self.device = self._runtime.gpu_context['device']

        # Pre-compute coordinate grids (allocated ONCE)
        y_coords = torch.arange(h, device=self.device).view(-1, 1).expand(h, w)
        x_coords = torch.arange(w, device=self.device).view(1, -1).expand(h, w)
        self.coord_grids = (y_coords, x_coords)
```

**Why**: Reduces per-frame allocation overhead from 0.5ms to ~0ms.

### ✅ DO: Keep Data on GPU Throughout Pipeline

```python
# Pattern handler (GPU)
self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])

# Overlay handler (GPU)
self.inputs['video'] = VideoInput('video', capabilities=['gpu'])
self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])

# ... more GPU handlers ...

# Final display handler (CPU)
self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
# Runtime auto-inserts GPU→CPU transfer here
```

**Why**: Minimizes expensive CPU↔GPU transfers.

### ✅ DO: Vectorize Operations

```python
# ✅ GOOD: Vectorized operation on GPU
dist = torch.sqrt((x_grid - center_x)**2 + (y_grid - center_y)**2)
mask = dist <= radius
frame[mask] = color

# ❌ BAD: Python loop (slow!)
for y in range(height):
    for x in range(width):
        if dist(x, y) <= radius:
            frame[y, x] = color
```

**Why**: Vectorized operations use GPU parallelism effectively.

### ❌ DON'T: Create Tensors in Tight Loops

```python
# ❌ BAD: Allocating tensors every frame
async def process(self, tick):
    frame = torch.empty(shape, device=self.device)  # 0.5ms overhead!
    color = torch.tensor([r, g, b], device=self.device)  # More overhead!

# ✅ GOOD: Reuse from memory pool or pre-allocate
async def process(self, tick):
    frame = self._runtime.gpu_context['memory_pool'].allocate(shape)
    frame[:] = self.pre_allocated_color  # Reuse pre-allocated tensor
```

**Why**: Tensor allocation has ~0.5ms overhead per allocation.

### ❌ DON'T: Transfer to CPU in Middle of Pipeline

```python
# ❌ BAD: Unnecessary transfer
async def process(self, tick):
    frame_gpu = self.inputs['video'].read_latest().data
    frame_cpu = frame_gpu.cpu().numpy()  # Expensive!
    # ... do something ...
    frame_gpu = torch.from_numpy(frame_cpu).to(device)  # More expensive!

# ✅ GOOD: Stay on GPU
async def process(self, tick):
    frame_gpu = self.inputs['video'].read_latest().data
    # ... work directly on GPU tensor ...
```

**Why**: CPU↔GPU transfers are the slowest operation (~2-5ms each direction).

### ❌ DON'T: Launch Many Small Kernels

```python
# ❌ BAD: Each operation launches a kernel
for corner in corners:
    frame[corner] = color1  # Kernel launch
for edge in edges:
    frame[edge] = color2    # Another kernel launch

# ✅ GOOD: Batch operations
corners_mask = create_corners_mask()  # Single operation
edges_mask = create_edges_mask()      # Single operation
frame[corners_mask] = color1
frame[edges_mask] = color2
```

**Why**: Kernel launch overhead is ~0.1ms per launch.

## Performance Comparison

### Test Setup
- Resolution: 640x480 and 1920x1080
- Pipeline: 5 handlers (pattern, overlay, waveform, FPS, display)
- Target: 60 FPS
- Hardware: Apple Silicon Mac (MPS backend)

### Results

| Implementation | Resolution | FPS | Notes |
|---|---|---|---|
| **CPU (NumPy/OpenCV)** | 640x480 | ~60 FPS | ✅ Best for small resolutions |
| **CPU (NumPy/OpenCV)** | 1920x1080 | ~25-30 FPS | ⚠️ Struggles at 1080p |
| **MPS (Naive)** | 640x480 | ~11 FPS | ❌ Worse than CPU (overhead) |
| **MPS (Runtime Optimized)** | 640x480 | ~40-50 FPS | ✅ 4x improvement |
| **MPS (Runtime Optimized)** | 1920x1080 | ~30-40 FPS | ✅ Better than CPU |

### Key Findings

1. **Small resolutions (640x480)**: CPU wins due to low overhead
   - NumPy/OpenCV are heavily optimized
   - GPU overhead (kernel launches, transfers) dominates

2. **Large resolutions (1920x1080+)**: GPU wins with optimizations
   - More pixels = more parallel work
   - Amortizes GPU overhead

3. **Naive GPU slower than CPU**: Without optimizations, GPU overhead kills performance
   - Memory allocations: 0.5ms × 60fps = 30ms/frame overhead
   - CPU↔GPU transfers: 2-5ms per transfer
   - Many small kernel launches: 0.1ms × N operations

4. **Runtime optimizations critical**: 4x improvement
   - Memory pooling eliminates allocation overhead
   - Keeping data on GPU avoids transfers
   - Pre-computed resources avoid per-frame setup

## When to Use GPU Acceleration

### Use GPU When:
- ✅ **Large resolution** (1920x1080+)
- ✅ **Heavy compute** (ML inference, complex effects)
- ✅ **Many parallel operations** (per-pixel processing)
- ✅ **Batch processing** (processing multiple frames at once)

### Use CPU When:
- ✅ **Small resolution** (640x480 or less)
- ✅ **Simple operations** (basic drawing, text rendering)
- ✅ **Sequential operations** (can't be parallelized)
- ✅ **Low latency critical** (GPU has startup overhead)

### Auto-Detection Strategy:
```python
# Runtime provides auto-detection
runtime = StreamRuntime(fps=60, enable_gpu=True)  # Auto-detects best backend

# Handlers can check and adapt
async def on_start(self):
    if self._runtime.gpu_context:
        print(f"Using GPU: {self._runtime.gpu_context['backend']}")
    else:
        print("Using CPU")
```

## Examples

### Full Working Example

See `examples/demo_mps_runtime_optimized.py` for complete implementation.

Key points:
- Handlers access `self._runtime.gpu_context` in `on_start()`
- Pre-compute resources once (coordinate grids, etc.)
- Use memory pool for frame allocation
- Stay on GPU throughout pipeline
- Transfer to CPU only at display handler

### Comparison Demos

1. **CPU (Baseline)**: `examples/demo_animated_performance.py`
   - Pure NumPy/OpenCV
   - 60 FPS at 640x480
   - ~25-30 FPS at 1920x1080

2. **MPS (Naive)**: `examples/demo_animated_performance_mps.py`
   - Direct PyTorch MPS
   - 11 FPS at 640x480 (worse than CPU!)
   - Shows why optimizations matter

3. **MPS (Runtime Optimized)**: `examples/demo_mps_runtime_optimized.py`
   - Uses runtime GPU utilities
   - ~40-50 FPS at 640x480 (4x improvement!)
   - ~30-40 FPS at 1920x1080 (better than CPU)

## Common Pitfalls

### 1. Forgetting to Pre-Allocate

```python
# ❌ Allocating every frame
async def process(self, tick):
    grid = torch.meshgrid(...)  # 1ms overhead per frame!

# ✅ Pre-allocate in on_start()
async def on_start(self):
    self.grid = torch.meshgrid(...)  # Once, 1ms total

async def process(self, tick):
    # Use self.grid (no overhead)
```

### 2. Unnecessary Tensor Copies

```python
# ❌ Creating unnecessary copies
frame_copy = frame.clone()  # 0.5ms overhead

# ✅ Work in-place when possible
frame[:marker_size, :marker_size] = color  # No copy
```

### 3. Mixed CPU/GPU Operations

```python
# ❌ Mixing CPU and GPU
cpu_array = frame_gpu.cpu().numpy()  # Transfer
result = np.sin(cpu_array)           # CPU compute
result_gpu = torch.from_numpy(result).to(device)  # Transfer back

# ✅ Keep on GPU
result_gpu = torch.sin(frame_gpu)  # All on GPU
```

## Debugging GPU Performance

### Profiling

```python
import time

async def process(self, tick):
    start = time.perf_counter()

    # Your GPU operations
    frame = self.do_something()

    elapsed = time.perf_counter() - start
    if elapsed > 0.016:  # Slower than 60 FPS
        print(f"WARNING: Slow frame {elapsed*1000:.1f}ms")
```

### Memory Usage

```python
async def on_start(self):
    if self._runtime.gpu_context:
        print(f"Memory pool: {self._runtime.gpu_context['memory_pool']}")

async def process(self, tick):
    if tick.frame_number % 60 == 0:  # Every second
        # Check pool stats
        pool = self._runtime.gpu_context['memory_pool'].pool
        print(f"Pool size: {sum(len(v) for v in pool.values())} tensors")
```

## Summary

**Runtime provides the infrastructure** - you write simple code:

1. ✅ Access `self._runtime.gpu_context` in `on_start()`
2. ✅ Use memory pool for allocations
3. ✅ Pre-compute resources once
4. ✅ Stay on GPU throughout pipeline
5. ✅ Vectorize operations

**Result**: Simple code that's 4x faster than naive GPU usage!
