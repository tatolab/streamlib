# GPU Optimization

Learn how streamlib automatically optimizes GPU pipelines for maximum performance with minimal code.

## The streamlib Superpower

**This is what differentiates streamlib from GStreamer and other video frameworks:**

Traditional video pipelines constantly bounce between CPU and GPU because they're not opinionated. **streamlib is GPU-first by default** - everything stays on GPU automatically.

```python
# You write simple code
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])

# streamlib automatically:
# 1. Camera outputs GPU textures (WebGPU)
# 2. Blur processes on GPU (zero-copy)
# 3. Display receives GPU data directly
# 4. Runtime handles all execution and memory
#
# Result: ZERO CPU TRANSFERS! All automatic!
```

## GPU-First by Default

All streamlib handlers are GPU-first by default. You don't need to declare capabilities or manage memory - the runtime handles everything automatically:

### Example: All-GPU Pipeline

```python
# All handlers are GPU by default
camera = CameraHandlerGPU()
blur = BlurFilterGPU()
display = DisplayGPUHandler()

# Connect - runtime keeps everything on GPU
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])

# Entire pipeline runs on GPU with zero CPU transfers!
# Runtime automatically:
# - Keeps data on GPU throughout
# - Manages execution context
# - Uses zero-copy where possible
```

### CPU Fallback (Rare Cases)

For the rare case where you need CPU processing, you can explicitly configure it:

```python
# Explicitly request CPU-only processing
cpu_filter = CPUFilter(allow_cpu=True)

runtime.connect(camera.outputs['video'], cpu_filter.inputs['video'])
# Runtime automatically inserts GPU→CPU transfer if needed
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
- GPU textures stay on GPU throughout pipeline
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

## Writing GPU Handlers

All handlers are GPU-first by default. Just implement your processing logic and the runtime handles the rest:

```python
class BlurFilter(StreamHandler):
    def __init__(self, kernel_size=15):
        super().__init__()
        self.kernel_size = kernel_size
        # Ports are GPU by default - no explicit declaration needed
        self.inputs['video'] = VideoInput('video')
        self.outputs['video'] = VideoOutput('video')

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        if frame:
            # Process on GPU using WebGPU compute shaders
            result = self.gpu_blur(frame.data)  # WebGPU compute shader
            self.outputs['video'].write(VideoFrame(data=result, ...))
```

**Benefits:**
- Simple, clean code
- GPU by default
- Runtime handles execution automatically
- No capability declarations needed

## GPU Context

Runtime provides GPU context for all handlers:

```python
async def on_start(self):
    """Called once when handler starts."""
    if self._runtime.gpu_context:
        self.gpu_context = self._runtime.gpu_context
        print(f"Using GPU: WebGPU (backend: {self.gpu_context.adapter.properties['backendType']})")
        # backendType: 'Metal' (macOS), 'D3D12' (Windows), 'Vulkan' (Linux), 'WebGPU' (Web)
    else:
        print("Using CPU")
```

## Best Practices

### 1. Trust the GPU-First Approach

```python
# ✅ GOOD: Default GPU-first
self.inputs['video'] = VideoInput('video')  # GPU by default
self.outputs['video'] = VideoOutput('video')  # GPU by default

# ⚠️ Only use CPU when absolutely necessary
self.inputs['video'] = VideoInput('video', allow_cpu=True)
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

### 3. Let Runtime Handle Everything

```python
# ✅ GOOD: Simple, automatic
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])
# Runtime handles execution, memory, and transfers automatically

# ❌ BAD: Don't manually manage dispatchers or transfers
# The runtime does this for you!
```

### 4. Focus on Processing Logic

```python
async def process(self, tick: TimedTick):
    frame = self.inputs['video'].read_latest()
    if frame:
        # Just process the frame - runtime handles memory
        result = self.process_frame(frame.data)
        self.outputs['video'].write(VideoFrame(data=result, ...))
```

## Common Patterns

### Pattern 1: Simple GPU Pipeline

```python
# All GPU by default - runtime handles everything
camera = CameraHandlerGPU()
blur = BlurFilterGPU()
compositor = CompositorGPU()
display = DisplayGPUHandler()

runtime.add_stream(Stream(camera))
runtime.add_stream(Stream(blur))
runtime.add_stream(Stream(compositor))
runtime.add_stream(Stream(display))

runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], compositor.inputs['video'])
runtime.connect(compositor.outputs['video'], display.inputs['video'])

# Everything stays on GPU automatically!
```

### Pattern 2: Multi-Input Composition

```python
# Multiple GPU sources → GPU compositor
camera1 = CameraHandlerGPU()
camera2 = CameraHandlerGPU()
pattern = TestPatternHandler()
compositor = MultiInputCompositor(num_inputs=3, mode='grid')
display = DisplayGPUHandler()

# Add and connect
for handler in [camera1, camera2, pattern, compositor, display]:
    runtime.add_stream(Stream(handler))

runtime.connect(camera1.outputs['video'], compositor.inputs['input_0'])
runtime.connect(camera2.outputs['video'], compositor.inputs['input_1'])
runtime.connect(pattern.outputs['video'], compositor.inputs['input_2'])
runtime.connect(compositor.outputs['video'], display.inputs['video'])

# All GPU, zero transfers!
```

### Pattern 3: Effects Chain

```python
# Chain multiple GPU effects
camera = CameraHandlerGPU()
blur = BlurFilterGPU()
overlay = LowerThirdsGPUHandler()
display = DisplayGPUHandler()

# Runtime figures out optimal execution
for handler in [camera, blur, overlay, display]:
    runtime.add_stream(Stream(handler))

runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], overlay.inputs['video'])
runtime.connect(overlay.outputs['video'], display.inputs['video'])
```

## Debugging GPU Issues

### Verify GPU Context

```python
runtime = StreamRuntime(fps=30)
print(f"GPU available: {runtime.gpu_context is not None}")
if runtime.gpu_context:
    backend_type = runtime.gpu_context.adapter.properties.get('backendType', 'Unknown')
    print(f"WebGPU Backend: {backend_type}")  # Metal, D3D12, Vulkan, or WebGPU
    print(f"Device: {runtime.gpu_context.device}")
```

### Force CPU-Only Mode

```python
# Disable GPU entirely (force CPU mode)
runtime = StreamRuntime(fps=30, disable_gpu=True)
```

## Summary

streamlib's GPU optimization is **automatic and opinionated**:

1. ✅ **GPU-first by default** - All operations use GPU unless explicitly configured
2. ✅ **Automatic execution** - Runtime determines optimal dispatcher for each handler
3. ✅ **Zero-copy by default** - Frames pass as references, stay on GPU
4. ✅ **Auto-transfer when needed** - Runtime handles CPU↔GPU transfers if required
5. ✅ **Simple API** - No explicit capabilities or dispatcher declarations

**Result:** Write simple, clean code that automatically runs as fast as possible!

## See Also

- [Ports](../api/ports.md) - Capability-based port system
- [Runtime](../api/runtime.md) - Connection and negotiation details
- [StreamHandler](../api/handler.md) - Writing GPU-capable handlers
- [Composition](composition.md) - Building complex pipelines
