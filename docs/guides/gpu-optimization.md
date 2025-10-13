# GPU Optimization

Learn how streamlib automatically optimizes GPU pipelines for maximum performance with minimal code.

## The streamlib Superpower

**This is what differentiates streamlib from GStreamer and other video frameworks:**

Traditional video pipelines constantly bounce between CPU and GPU because they're not opinionated. **streamlib automatically chooses the fastest path** and **stays on GPU as long as possible**.

```python
# You write simple code
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])

# streamlib automatically:
# 1. Detects camera outputs GPU textures (Metal/CUDA)
# 2. Negotiates GPU path with blur filter
# 3. Blur stays on GPU (zero-copy)
# 4. Display receives GPU data directly
#
# Result: ZERO CPU TRANSFERS!
```

## Automatic Capability Negotiation

streamlib uses **capability-based ports** (inspired by GStreamer) to automatically find the optimal memory path:

### Example: All-GPU Pipeline

```python
# Camera outputs GPU textures
camera = CameraHandlerGPU()  # outputs: capabilities=['gpu']

# Blur accepts GPU or CPU
blur = BlurFilterGPU()       # inputs: capabilities=['gpu', 'cpu']
                             # outputs: capabilities=['gpu']

# Display accepts GPU or CPU
display = DisplayGPUHandler() # inputs: capabilities=['gpu', 'cpu']

# Connect
runtime.connect(camera.outputs['video'], blur.inputs['video'])
# ✅ Negotiated: gpu (no transfer)

runtime.connect(blur.outputs['video'], display.inputs['video'])
# ✅ Negotiated: gpu (no transfer)

# Entire pipeline runs on GPU with zero CPU transfers!
```

### Example: Automatic Transfers

When handlers don't share capabilities, runtime auto-inserts transfers:

```python
# GPU camera → CPU-only filter
camera = CameraHandlerGPU()    # outputs: capabilities=['gpu']
cpu_filter = CPUFilter()       # inputs: capabilities=['cpu']

runtime.connect(camera.outputs['video'], cpu_filter.inputs['video'])
# ⚠️ WARNING: Auto-inserting gpu→cpu transfer (performance cost ~2ms)

# Runtime automatically inserted a GPUtoCPUTransferHandler between them
```

## Zero-Copy Architecture

streamlib uses **zero-copy ring buffers** - frames pass as references, not copies:

```python
async def process(self, tick: TimedTick):
    # Read returns reference (not copy)
    frame = self.inputs['video'].read_latest()

    if frame:
        # Process on GPU (data stays on GPU)
        result = self.gpu_process(frame.data)

        # Write reference (not copy)
        self.outputs['video'].write(VideoFrame(data=result, ...))
```

**Benefits:**
- GPU tensors stay on GPU throughout pipeline
- No unnecessary memory allocations
- Minimal latency
- Maximum throughput

## When to Use GPU

### ✅ Use GPU for:

- **Large resolutions** (1920x1080+)
- **Heavy compute** (ML inference, complex effects)
- **Many parallel operations** (per-pixel processing)
- **Long pipelines** (many GPU handlers chained together)

### ✅ Use CPU for:

- **Small resolutions** (640x480 or less)
- **Simple operations** (basic drawing, text)
- **Sequential operations** (can't parallelize)
- **Low latency critical** (GPU has startup overhead)

### Example Performance

| Resolution | CPU (NumPy) | GPU (Naive) | GPU (Optimized) |
|------------|-------------|-------------|-----------------|
| 640×480    | 60 FPS ✅   | 11 FPS ❌   | 40-50 FPS ✅    |
| 1920×1080  | 25-30 FPS ⚠️ | 15 FPS ❌   | 30-40 FPS ✅    |

**Key insight:** At small resolutions, CPU is faster due to low overhead. At large resolutions, GPU wins with proper optimization.

## Flexible Handlers

Write handlers that adapt to both CPU and GPU:

```python
class AdaptiveBlur(StreamHandler):
    def __init__(self):
        super().__init__()
        # Declare flexible capabilities
        self.inputs['video'] = VideoInput('video', capabilities=['cpu', 'gpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu', 'gpu'])

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        if frame:
            # Runtime tells you which memory space was negotiated
            if self.inputs['video'].negotiated_memory == 'gpu':
                result = self.gpu_blur(frame.data)  # PyTorch
            else:
                result = self.cpu_blur(frame.data)  # NumPy

            self.outputs['video'].write(VideoFrame(data=result, ...))
```

**Benefits:**
- Handler works in any pipeline
- Runtime chooses optimal path
- User doesn't need to think about it

## GPU Context

Runtime provides GPU context for all handlers:

```python
async def on_start(self):
    """Called once when handler starts."""
    if self._runtime.gpu_context:
        self.device = self._runtime.gpu_context['device']
        print(f"Using GPU: {self._runtime.gpu_context['backend']}")
        # backend is 'mps' (macOS), 'cuda' (NVIDIA), or 'cpu' (fallback)
    else:
        self.device = 'cpu'
        print("Using CPU")
```

## Best Practices

### 1. Declare Flexible Capabilities

```python
# ✅ GOOD: Handler can adapt
self.inputs['video'] = VideoInput('video', capabilities=['cpu', 'gpu'])
self.outputs['video'] = VideoOutput('video', capabilities=['cpu', 'gpu'])

# ⚠️ OKAY: Handler only works on GPU
self.inputs['video'] = VideoInput('video', capabilities=['gpu'])
self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])
```

### 2. Keep Pipelines On GPU

```python
# ✅ GOOD: Entire pipeline on GPU
camera (GPU) → blur (GPU) → compositor (GPU) → display (GPU)
# Zero transfers!

# ❌ BAD: Bouncing between CPU and GPU
camera (GPU) → cpu_filter (CPU) → gpu_effect (GPU) → cpu_display (CPU)
# 3 transfers! (~6ms overhead)
```

### 3. Let Runtime Handle Transfers

```python
# ✅ GOOD: Auto-transfer
runtime.connect(gpu_source.outputs['video'], cpu_sink.inputs['video'])
# Runtime warns about transfer, inserts handler automatically

# ❌ BAD: Manual transfer
gpu_to_cpu = GPUtoCPUTransferHandler()
runtime.add_stream(Stream(gpu_to_cpu))
runtime.connect(gpu_source.outputs['video'], gpu_to_cpu.inputs['in'])
runtime.connect(gpu_to_cpu.outputs['out'], cpu_sink.inputs['video'])
# More code, same result
```

### 4. Check Negotiated Memory

```python
async def process(self, tick: TimedTick):
    frame = self.inputs['video'].read_latest()
    if frame:
        # Adapt to negotiated memory space
        memory = self.inputs['video'].negotiated_memory

        if memory == 'gpu':
            result = self.process_gpu(frame.data)
        else:
            result = self.process_cpu(frame.data)

        self.outputs['video'].write(VideoFrame(data=result, ...))
```

## Common Patterns

### Pattern 1: GPU Pipeline with CPU Endpoints

```python
# CPU camera → GPU processing → CPU display
camera = CameraHandler()         # outputs: ['cpu']
to_gpu = CPUtoGPUTransfer()      # cpu → gpu
blur = BlurFilterGPU()           # gpu → gpu
compositor = CompositorGPU()     # gpu → gpu
to_cpu = GPUtoCPUTransfer()      # gpu → cpu
display = DisplayHandler()       # inputs: ['cpu']

# Only 2 transfers (at boundaries)
# All processing stays on GPU
```

### Pattern 2: Flexible Pipeline

```python
# All handlers flexible
camera = CameraHandlerGPU()      # outputs: ['cpu', 'gpu']
blur = BlurFilter()              # both: ['cpu', 'gpu']
display = DisplayHandler()       # inputs: ['cpu', 'gpu']

# Runtime negotiates best path (probably GPU throughout)
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])
```

### Pattern 3: Mixed Pipeline

```python
# Some GPU, some CPU
camera = CameraHandlerGPU()      # outputs: ['gpu']
ml_model = MLInference()         # gpu → gpu (needs GPU)
text_overlay = TextOverlay()     # inputs: ['cpu'] (CPU-only)
display = DisplayHandler()       # inputs: ['cpu', 'gpu']

runtime.connect(camera.outputs['video'], ml_model.inputs['video'])
# ✅ Negotiated: gpu

runtime.connect(ml_model.outputs['video'], text_overlay.inputs['video'])
# ⚠️ WARNING: Auto-inserting gpu→cpu transfer

runtime.connect(text_overlay.outputs['video'], display.inputs['video'])
# ✅ Negotiated: cpu
```

## Debugging GPU Issues

### Check What Was Negotiated

```python
# After connecting, check negotiated memory:
runtime.connect(output, input)
print(f"Negotiated: {input.negotiated_memory}")  # 'cpu', 'gpu', or None
```

### Verify GPU Context

```python
runtime = StreamRuntime(fps=30, gpu_backend='auto')
print(f"GPU available: {runtime.gpu_context is not None}")
if runtime.gpu_context:
    print(f"Backend: {runtime.gpu_context['backend']}")
    print(f"Device: {runtime.gpu_context['device']}")
```

### Force CPU or GPU

```python
# Force specific backend
runtime = StreamRuntime(fps=30, gpu_backend='metal')  # or 'cuda', 'none'

# Disable GPU entirely
runtime = StreamRuntime(fps=30, gpu_backend='none')
```

## Summary

streamlib's GPU optimization is **automatic and opinionated**:

1. ✅ **Declare capabilities** - Tell runtime what memory spaces you support
2. ✅ **Let runtime negotiate** - It finds the optimal path
3. ✅ **Zero-copy by default** - Frames pass as references
4. ✅ **Auto-transfer when needed** - Runtime warns about performance costs
5. ✅ **Write flexible handlers** - Work on CPU or GPU

**Result:** Simple code that automatically runs as fast as possible!

## See Also

- [Ports](../api/ports.md) - Capability-based port system
- [Runtime](../api/runtime.md) - Connection and negotiation details
- [StreamHandler](../api/handler.md) - Writing GPU-capable handlers
- [Composition](composition.md) - Building complex pipelines
