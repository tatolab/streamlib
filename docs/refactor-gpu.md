# GPU-Only Refactor Plan

## Executive Summary

This document outlines the complete architectural refactor of streamlib to a **GPU-only, zero-copy design** using the **Slang shading language** for cross-platform shader compilation.

### Why This Refactor?

**Current architecture has fundamental limitations:**
- CPU/GPU hybrid design requires constant capability negotiation
- Transfer handlers add complexity and latency
- Framework limitations (PyTorch Metal texture interop) cause unavoidable CPU↔GPU bounces
- Performance bottlenecks from memory transfers (4-7ms per bounce)
- ML operations (object detection) cannot tolerate CPU roundtrips

**New architecture goals:**
- **GPU-only**: All operations on GPU, no CPU fallbacks
- **Zero-copy**: Data never leaves GPU memory space
- **Cross-platform**: Slang shaders compile to Metal/CUDA/Vulkan/SPIR-V
- **Declarative API**: AI agents focus on "what", not "how"
- **Realtime ML-ready**: Support object detection, tracking, inference at 60 FPS

### Architecture Philosophy

```
Old (CPU/GPU hybrid):
Camera → [CPU?GPU?] → Blur → [Transfer?] → Compositor → [Transfer?] → Display
         ↑_____constant negotiation, transfers, complexity_____↑

New (GPU-only):
Camera → Blur → Compositor → Display
  ↑____________pure GPU, zero-copy, simple____________↑
```

**Core principle**: Stay on GPU from capture → processing → display. No exceptions.

---

## Phase 1: GPU-Only Handler Architecture

**Goal**: Eliminate CPU fallbacks and capability negotiation, establish GPU-native foundation.

### 1.1 GPU Memory Model

**Create new base classes** (`packages/streamlib/src/streamlib/gpu/`):

```python
# gpu/memory.py
class GPUBuffer:
    """
    Abstract GPU buffer (Metal texture, CUDA buffer, Vulkan image).
    Zero-copy reference to GPU memory.
    """
    def __init__(self, device: GPUDevice, width: int, height: int, format: PixelFormat):
        self.device = device
        self.width = width
        self.height = height
        self.format = format
        self._native_handle = None  # Platform-specific (MTLTexture, cudaArray_t, VkImage)

    @property
    def native_handle(self):
        """Platform-specific GPU memory handle."""
        return self._native_handle

class GPUDevice:
    """
    Abstract GPU device (Metal, CUDA, Vulkan).
    Manages GPU context and resource allocation.
    """
    def __init__(self, backend: Literal['metal', 'cuda', 'vulkan']):
        self.backend = backend
        self._native_device = None  # MTLDevice, cudaDevice_t, VkDevice

    def create_buffer(self, width: int, height: int, format: PixelFormat) -> GPUBuffer:
        """Allocate GPU buffer."""
        raise NotImplementedError

class PixelFormat(Enum):
    """Platform-agnostic pixel formats."""
    RGBA8 = "rgba8"
    RGB8 = "rgb8"
    YUV420 = "yuv420"
    YUV422 = "yuv422"
    FLOAT16_RGBA = "float16_rgba"  # For ML inference
    FLOAT32_RGBA = "float32_rgba"
```

### 1.2 GPU-Only Ports

**Replace capability-based ports** with GPU-only:

```python
# Before (cpu/gpu negotiation):
class VideoInput(Port):
    def __init__(self, name: str, capabilities: List[str] = ['cpu', 'gpu']):
        self.capabilities = capabilities

# After (GPU-only):
class GPUVideoInput(Port):
    """GPU-only video input. Accepts GPUBuffer frames."""
    def __init__(self, name: str, format: PixelFormat = PixelFormat.RGBA8):
        self.format = format
        self._buffer = None  # Ring buffer of GPUBuffer references

    def read_latest(self) -> Optional[GPUVideoFrame]:
        """Returns GPUVideoFrame with GPUBuffer reference (zero-copy)."""
        pass

class GPUVideoOutput(Port):
    """GPU-only video output. Emits GPUBuffer frames."""
    def __init__(self, name: str, format: PixelFormat = PixelFormat.RGBA8):
        self.format = format

    def write(self, frame: GPUVideoFrame):
        """Write GPU frame (zero-copy reference)."""
        pass
```

### 1.3 GPU-Only StreamHandler

**Update base handler** to enforce GPU-only:

```python
# handler.py (updated)
class StreamHandler:
    """
    Base handler for GPU-only processing.

    All handlers operate exclusively on GPU. No CPU fallbacks.
    """
    def __init__(self, handler_id: str):
        self.handler_id = handler_id
        self.inputs: Dict[str, GPUVideoInput] = {}
        self.outputs: Dict[str, GPUVideoOutput] = {}
        self.gpu_device: Optional[GPUDevice] = None  # Set by runtime

    async def on_start(self):
        """Initialize GPU resources. Device already assigned by runtime."""
        pass

    async def process(self, tick: TimedTick):
        """Process GPU frames. All inputs/outputs are GPUBuffer."""
        raise NotImplementedError

    async def on_stop(self):
        """Release GPU resources."""
        pass
```

### 1.4 Breaking Changes

**What gets removed:**
- ❌ `capabilities` parameter on ports
- ❌ CPU-based handlers (BlurFilter, DisplayHandler, etc.)
- ❌ Transfer handlers (no longer needed)
- ❌ Capability negotiation logic in Runtime
- ❌ NumPy array support in VideoFrame

**What gets added:**
- ✅ `GPUBuffer` abstraction
- ✅ `GPUDevice` abstraction
- ✅ `PixelFormat` enum
- ✅ GPU-only ports (`GPUVideoInput`, `GPUVideoOutput`)
- ✅ Platform detection (Metal/CUDA/Vulkan)

### 1.5 Migration Path

**For users:**
```python
# Old (hybrid):
camera = CameraHandlerGPU(...)
blur = BlurFilter(...)  # CPU-based
display = DisplayHandler(...)  # CPU-based

# New (GPU-only):
camera = CameraHandler(...)  # Auto-detects GPU backend
blur = BlurHandler(...)  # GPU-only
display = DisplayHandler(...)  # GPU-only
```

**For handler developers:**
```python
# Old:
self.inputs['video'] = VideoInput('video', capabilities=['cpu', 'gpu'])
frame_data = frame.data  # Could be np.ndarray or torch.Tensor

# New:
self.inputs['video'] = GPUVideoInput('video', format=PixelFormat.RGBA8)
gpu_buffer = frame.buffer  # Always GPUBuffer
```

### 1.6 Success Criteria

- [ ] All handlers use `GPUBuffer` exclusively
- [ ] No CPU fallback code paths
- [ ] Runtime auto-detects and initializes GPU backend
- [ ] Zero-copy verified with profiling
- [ ] All Phase 3.x examples converted to GPU-only

### 1.7 Implementation Tasks

1. **Create `gpu/` module structure**
   - `gpu/memory.py` - GPUBuffer, GPUDevice abstractions
   - `gpu/formats.py` - PixelFormat enum
   - `gpu/backends/` - Metal, CUDA, Vulkan implementations

2. **Implement Metal backend** (`gpu/backends/metal.py`)
   - `MetalDevice(GPUDevice)`
   - `MetalBuffer(GPUBuffer)` wrapping `MTLTexture`
   - IOSurface-backed textures for zero-copy

3. **Update base handler** (`handler.py`)
   - Add `gpu_device` property
   - Replace port types with GPU-only variants
   - Remove CPU code paths

4. **Convert existing handlers** (one by one)
   - `camera_gpu.py` → `camera.py` (GPU-only)
   - `blur_gpu.py` → `blur.py` (using Slang shaders - Phase 2)
   - `display_gpu.py` → `display.py` (Metal layer or Vulkan surface)
   - `compositor_multi.py` → uses GPU-only compositor

5. **Update Runtime** (`runtime.py`)
   - Auto-detect GPU backend (Metal on macOS, CUDA on NVIDIA, Vulkan fallback)
   - Initialize `GPUDevice` and assign to handlers
   - Remove transfer handler logic
   - Simplify connection logic (all GPU)

---

## Phase 2: Slang Shader Integration

**Goal**: Replace platform-specific shaders with Slang for cross-platform support.

### 2.1 Why Slang?

**Current problem**: Metal shaders lock us to macOS
```
camera_gpu.py: Metal shader (YUV→RGB)  ❌ macOS only
blur_gpu.py:   PyTorch MPS               ❌ macOS only, CPU bounces
```

**Slang solution**: Write once, compile to any backend
```
shaders/yuv_to_rgb.slang → Metal (macOS)
                         → CUDA (NVIDIA)
                         → SPIR-V (Vulkan, any GPU)
                         → HLSL (DirectX, future)
```

### 2.2 Slang Architecture

**Create shader library** (`packages/streamlib/src/streamlib/shaders/`):

```
shaders/
├── core/
│   ├── color_conversion.slang    # YUV↔RGB, color spaces
│   ├── compositing.slang          # Alpha blend, PIP, grid
│   └── filters.slang              # Blur, sharpen, edge detect
├── ml/
│   ├── preprocessing.slang        # Normalize, resize for ML
│   └── postprocessing.slang       # Bounding boxes, masks
└── effects/
    ├── text_overlay.slang         # GPU text rendering
    └── transitions.slang          # Fades, wipes, dissolves
```

**Example Slang shader**:
```c
// shaders/core/color_conversion.slang
[shader("compute")]
[numthreads(8, 8, 1)]
void yuv420_to_rgb(
    uint3 dispatchThreadID : SV_DispatchThreadID,
    Texture2D<float> y_plane,
    Texture2D<float2> cbcr_plane,
    RWTexture2D<float4> output_rgb,
    ConstantBuffer<ConversionParams> params
) {
    uint2 pixel = dispatchThreadID.xy;

    // Sample YUV
    float y = y_plane[pixel];
    float2 cbcr = cbcr_plane[pixel / 2];  // 4:2:0 subsampling

    // BT.709 conversion matrix
    float3 rgb;
    rgb.r = y + 1.5748 * (cbcr.y - 0.5);
    rgb.g = y - 0.1873 * (cbcr.x - 0.5) - 0.4681 * (cbcr.y - 0.5);
    rgb.b = y + 1.8556 * (cbcr.x - 0.5);

    output_rgb[pixel] = float4(rgb, 1.0);
}
```

### 2.3 Slang Compilation Pipeline

**Build-time compilation**:
```python
# gpu/slang_compiler.py
class SlangCompiler:
    """
    Compiles Slang shaders to platform-specific formats.
    """
    def __init__(self, backend: Literal['metal', 'cuda', 'vulkan']):
        self.backend = backend
        self.slangc_path = self._find_slangc()

    def compile(self, shader_source: str, entry_point: str) -> CompiledShader:
        """
        Compile Slang shader to backend-specific format.

        Returns:
            Metal: .metallib
            CUDA: .ptx
            Vulkan: .spv (SPIR-V)
        """
        target = {
            'metal': 'metal',
            'cuda': 'cuda',
            'vulkan': 'spirv'
        }[self.backend]

        # Run slangc compiler
        result = subprocess.run([
            self.slangc_path,
            '-target', target,
            '-entry', entry_point,
            '-o', output_path,
            shader_source
        ], capture_output=True)

        return CompiledShader(output_path, self.backend)
```

**Runtime shader loading**:
```python
# gpu/shader_library.py
class ShaderLibrary:
    """
    Manages compiled shaders for GPU device.
    """
    def __init__(self, device: GPUDevice):
        self.device = device
        self._cache = {}

    def load_shader(self, name: str) -> GPUShader:
        """
        Load compiled shader by name.
        Auto-detects correct backend variant.
        """
        if name in self._cache:
            return self._cache[name]

        shader_path = self._get_shader_path(name)
        shader = self.device.load_compiled_shader(shader_path)
        self._cache[name] = shader
        return shader
```

### 2.4 Handler Shader Usage

**Example: Camera handler with Slang YUV→RGB**:
```python
class CameraHandler(StreamHandler):
    async def on_start(self):
        # Load compiled YUV→RGB shader
        self.yuv_to_rgb_shader = self.gpu_device.shader_library.load_shader('yuv420_to_rgb')

        # Create output buffer
        self.rgb_buffer = self.gpu_device.create_buffer(
            self.width, self.height, PixelFormat.RGBA8
        )

    async def process(self, tick: TimedTick):
        # Get YUV frame from camera (CVPixelBuffer on Metal)
        yuv_buffer = self._capture_frame()

        # Dispatch YUV→RGB shader (pure GPU)
        self.yuv_to_rgb_shader.dispatch(
            inputs={'y_plane': yuv_buffer.y, 'cbcr_plane': yuv_buffer.cbcr},
            outputs={'output_rgb': self.rgb_buffer},
            grid_size=(self.width // 8, self.height // 8, 1)
        )

        # Emit GPU frame (zero-copy)
        self.outputs['video'].write(GPUVideoFrame(self.rgb_buffer, tick.timestamp))
```

**Example: Blur handler with Slang**:
```python
class BlurHandler(StreamHandler):
    async def on_start(self):
        self.blur_shader = self.gpu_device.shader_library.load_shader('gaussian_blur')
        self.temp_buffer = self.gpu_device.create_buffer(
            self.width, self.height, PixelFormat.RGBA8
        )

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()

        # Two-pass blur (horizontal + vertical)
        self.blur_shader.dispatch(
            inputs={'input': frame.buffer},
            outputs={'output': self.temp_buffer},
            params={'direction': (1, 0), 'kernel_size': self.kernel_size}
        )
        self.blur_shader.dispatch(
            inputs={'input': self.temp_buffer},
            outputs={'output': frame.buffer},  # In-place
            params={'direction': (0, 1), 'kernel_size': self.kernel_size}
        )

        self.outputs['video'].write(frame)
```

### 2.5 Breaking Changes

**Removed:**
- ❌ Metal-specific shader code in handlers
- ❌ PyTorch MPS operations (replaced with Slang compute shaders)
- ❌ OpenCV CPU operations (cv2.GaussianBlur, etc.)

**Added:**
- ✅ Slang shader library (`shaders/`)
- ✅ `SlangCompiler` for build-time compilation
- ✅ `ShaderLibrary` for runtime loading
- ✅ `GPUShader` abstraction for dispatch
- ✅ Cross-platform support (Metal/CUDA/Vulkan)

### 2.6 Implementation Tasks

1. **Set up Slang toolchain**
   - Add Slang compiler to build dependencies
   - Create shader compilation build step
   - Pre-compile shaders for all backends

2. **Create shader library** (`shaders/`)
   - Port existing Metal shaders to Slang
   - Implement core shaders (color conversion, compositing)
   - Create filter shaders (blur, sharpen, edge detect)
   - Add ML preprocessing shaders

3. **Implement shader system** (`gpu/shader_library.py`)
   - `SlangCompiler` - build-time compilation
   - `ShaderLibrary` - runtime loading
   - `GPUShader` - dispatch abstraction

4. **Update handlers to use Slang**
   - Camera: Replace Metal YUV→RGB with Slang
   - Blur: Replace PyTorch MPS with Slang compute shader
   - Compositor: Replace PyTorch ops with Slang alpha blend
   - Text overlay: Implement Slang text rendering

5. **Verify cross-platform**
   - Test Metal backend on macOS
   - Test CUDA backend on NVIDIA GPU
   - Test Vulkan backend on AMD/Intel GPU

### 2.7 Success Criteria

- [ ] All shaders written in Slang
- [ ] Shaders compile to Metal/CUDA/Vulkan
- [ ] Zero PyTorch/OpenCV dependencies in handlers
- [ ] Camera → Display pipeline runs pure GPU
- [ ] Performance >= 60 FPS on 1080p

---

## Phase 3: Runtime Simplification

**Goal**: Remove complexity now that everything is GPU-only.

### 3.1 Simplified Connection Logic

**Before (capability negotiation):**
```python
# Old runtime had to check:
if source.capabilities != dest.capabilities:
    # Insert transfer handler
    transfer = create_transfer_handler(source.capabilities, dest.capabilities)
    runtime.connect(source, transfer.input)
    runtime.connect(transfer.output, dest)
```

**After (GPU-only):**
```python
# New runtime: direct connection (all GPU)
runtime.connect(source, dest)
# That's it! No negotiation, no transfers.
```

### 3.2 Updated Runtime API

```python
class StreamRuntime:
    """
    Simplified GPU-only runtime.

    All handlers run on GPU. No capability negotiation needed.
    """
    def __init__(self, fps: int = 30, backend: Optional[str] = None):
        """
        Args:
            fps: Target frame rate
            backend: GPU backend ('metal', 'cuda', 'vulkan', or None for auto-detect)
        """
        self.fps = fps
        self.gpu_device = self._initialize_gpu(backend)
        self.streams: List[Stream] = []

    def _initialize_gpu(self, backend: Optional[str]) -> GPUDevice:
        """Auto-detect or create specified GPU device."""
        if backend is None:
            # Auto-detect
            if sys.platform == 'darwin':
                backend = 'metal'
            elif torch.cuda.is_available():
                backend = 'cuda'
            else:
                backend = 'vulkan'

        return GPUDevice.create(backend)

    def add_stream(self, stream: Stream):
        """
        Add handler to runtime.
        Automatically assigns GPU device to handler.
        """
        stream.handler.gpu_device = self.gpu_device
        self.streams.append(stream)

    def connect(self, source: GPUVideoOutput, dest: GPUVideoInput):
        """
        Connect GPU ports directly (zero-copy).
        No negotiation, no transfers.
        """
        if source.format != dest.format:
            raise ValueError(f"Format mismatch: {source.format} → {dest.format}")

        # Direct connection (ring buffer reference)
        dest._connect_to(source)
```

### 3.3 Dispatcher Model

**Simplified to two types**:
1. **`asyncio`**: Non-blocking, lightweight handlers (generators, routing)
2. **`compute`**: GPU compute handlers (all processing)

```python
# Before (4 types):
Stream(handler, dispatcher='asyncio')      # Non-blocking
Stream(handler, dispatcher='threadpool')   # Blocking CPU
Stream(handler, dispatcher='gpu')          # GPU (not implemented)
Stream(handler, dispatcher='processpool')  # Heavy CPU (not implemented)

# After (2 types):
Stream(handler, dispatcher='asyncio')   # Generators, routing
Stream(handler, dispatcher='compute')   # All GPU processing (default)
```

**Default is `compute`** (GPU processing):
```python
runtime.add_stream(Stream(camera))        # Defaults to 'compute'
runtime.add_stream(Stream(blur))          # Defaults to 'compute'
runtime.add_stream(Stream(compositor))    # Defaults to 'compute'
```

### 3.4 Implementation Tasks

1. **Remove complexity from Runtime**
   - Delete transfer handler creation
   - Delete capability negotiation
   - Simplify connection logic

2. **Update Stream/dispatcher**
   - Remove `threadpool` and `processpool`
   - Add `compute` dispatcher type
   - Make `compute` the default

3. **Update documentation**
   - Remove dispatcher decision tree
   - Document GPU-only model
   - Update examples

### 3.5 Success Criteria

- [ ] Runtime code reduced by >50%
- [ ] No transfer handlers
- [ ] Direct GPU connections verified
- [ ] All examples work with simplified API

---

## Phase 4: Declarative API for AI Agents

**Goal**: High-level, intention-focused API that AI agents can easily use.

### 4.1 The Problem with Current API

**Current API is imperative** (too much "how"):
```python
# AI agent has to think about:
camera = CameraHandlerGPU(device_name="...", width=1920, height=1080)
blur = BlurFilterGPU(kernel_size=15, sigma=8.0)
display = DisplayGPUHandler(width=1920, height=1080, show_fps=True)

runtime = StreamRuntime(fps=30)
runtime.add_stream(Stream(camera, dispatcher='asyncio'))
runtime.add_stream(Stream(blur, dispatcher='asyncio'))
runtime.add_stream(Stream(display, dispatcher='threadpool'))

runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])

runtime.start()
```

**Too many decisions**: dispatcher types, port names, parameters, lifecycle

### 4.2 Declarative API Design

**New API focuses on "what"**:
```python
# AI agent just declares intent:
pipeline = (
    stream.camera(device="FaceTime HD Camera")
    | stream.blur(amount=15)
    | stream.display(show_fps=True)
)

pipeline.run()
```

**Or using builder pattern**:
```python
pipeline = StreamPipeline()
pipeline.add(stream.camera())
pipeline.add(stream.blur())
pipeline.add(stream.display())
pipeline.run()
```

**Multi-pipeline composition**:
```python
# Declarative composition
camera_feed = stream.camera() | stream.blur()
overlay = stream.pattern("smpte_bars")

composited = stream.compositor(
    inputs=[camera_feed, overlay],
    mode="picture_in_picture"
)

display = composited | stream.display()
display.run()
```

### 4.3 Stream Builder Implementation

```python
# api/declarative.py
class StreamNode:
    """
    Declarative stream node.

    Can be composed with | operator (Unix pipe style).
    """
    def __init__(self, handler: StreamHandler):
        self._handler = handler
        self._upstream = None

    def __or__(self, other: 'StreamNode') -> 'StreamNode':
        """Pipe operator: camera | blur | display"""
        other._upstream = self
        return other

    def run(self, fps: int = 30, backend: Optional[str] = None):
        """
        Execute pipeline.

        Automatically:
        - Creates runtime
        - Adds handlers
        - Connects ports
        - Starts execution
        """
        runtime = StreamRuntime(fps=fps, backend=backend)

        # Traverse upstream to build handler chain
        handlers = []
        node = self
        while node is not None:
            handlers.append(node._handler)
            node = node._upstream

        # Add handlers in reverse order (upstream first)
        for handler in reversed(handlers):
            runtime.add_stream(Stream(handler))

        # Connect ports sequentially
        for i in range(len(handlers) - 1):
            source = handlers[i]
            dest = handlers[i + 1]
            runtime.connect(source.outputs['video'], dest.inputs['video'])

        # Start runtime
        runtime.start()
        return runtime


class stream:
    """
    Declarative stream API.

    Usage:
        pipeline = stream.camera() | stream.blur() | stream.display()
        pipeline.run()
    """

    @staticmethod
    def camera(
        device: Optional[str] = None,
        resolution: Literal['720p', '1080p', '4k'] = '1080p'
    ) -> StreamNode:
        """Capture from camera."""
        width, height = {'720p': (1280, 720), '1080p': (1920, 1080), '4k': (3840, 2160)}[resolution]
        handler = CameraHandler(device_name=device, width=width, height=height)
        return StreamNode(handler)

    @staticmethod
    def blur(amount: int = 15) -> StreamNode:
        """Apply Gaussian blur."""
        handler = BlurHandler(kernel_size=amount, sigma=amount / 2.0)
        return StreamNode(handler)

    @staticmethod
    def display(
        show_fps: bool = True,
        fullscreen: bool = False
    ) -> StreamNode:
        """Display video."""
        handler = DisplayHandler(show_fps=show_fps, fullscreen=fullscreen)
        return StreamNode(handler)

    @staticmethod
    def pattern(
        type: Literal['smpte_bars', 'gradient', 'checkerboard'] = 'smpte_bars',
        resolution: Literal['720p', '1080p', '4k'] = '1080p'
    ) -> StreamNode:
        """Generate test pattern."""
        width, height = {'720p': (1280, 720), '1080p': (1920, 1080), '4k': (3840, 2160)}[resolution]
        handler = TestPatternHandler(pattern=type, width=width, height=height)
        return StreamNode(handler)

    @staticmethod
    def compositor(
        inputs: List[StreamNode],
        mode: Literal['alpha_blend', 'pip', 'side_by_side', 'vertical_stack', 'grid'] = 'pip'
    ) -> StreamNode:
        """Compose multiple streams."""
        handler = MultiInputCompositor(num_inputs=len(inputs), mode=mode)
        # TODO: Handle multiple upstream connections
        return StreamNode(handler)

    @staticmethod
    def text_overlay(
        text: str,
        position: Literal['top', 'bottom', 'center'] = 'bottom'
    ) -> StreamNode:
        """Overlay text."""
        handler = TextOverlayHandler(text=text, position=position)
        return StreamNode(handler)

    @staticmethod
    def detect_objects(
        model: Literal['yolo', 'ssd', 'faster_rcnn'] = 'yolo',
        show_boxes: bool = True
    ) -> StreamNode:
        """ML object detection."""
        handler = ObjectDetectionHandler(model=model, show_boxes=show_boxes)
        return StreamNode(handler)
```

### 4.4 AI Agent Usage Examples

**Example 1: Simple camera display**
```python
# AI agent prompt: "Show me the camera feed"
stream.camera() | stream.display()
```

**Example 2: Blurred camera**
```python
# AI agent prompt: "Show camera with blur effect"
stream.camera() | stream.blur(amount=20) | stream.display()
```

**Example 3: Object detection**
```python
# AI agent prompt: "Run YOLO object detection on camera"
stream.camera() | stream.detect_objects(model='yolo') | stream.display()
```

**Example 4: Multi-input composition**
```python
# AI agent prompt: "Show camera in corner with test pattern background"
camera = stream.camera() | stream.blur()
pattern = stream.pattern("smpte_bars")

pipeline = stream.compositor([pattern, camera], mode='pip') | stream.display()
pipeline.run()
```

### 4.5 Benefits for AI Agents

1. **No device management**: GPU backend auto-selected
2. **No dispatcher decisions**: All handled automatically
3. **No port wiring**: Connections inferred from `|` operator
4. **Sensible defaults**: Resolution, FPS, parameters pre-configured
5. **Composable**: Unix pipe metaphor is familiar
6. **Type-safe**: Literal types guide valid options

### 4.6 Implementation Tasks

1. **Create declarative API** (`api/declarative.py`)
   - `StreamNode` class with `|` operator
   - `stream` builder class with static methods
   - Multi-input composition support

2. **Update all handlers** to work with declarative API
   - Sensible default parameters
   - Auto-resolution detection where possible

3. **Create examples** showing declarative API
   - `examples/declarative_basic.py`
   - `examples/declarative_ml.py`
   - `examples/declarative_composition.py`

4. **Documentation**
   - API reference for `stream` builder
   - Migration guide from imperative API
   - AI agent usage guide

### 4.7 Success Criteria

- [ ] Single-line pipelines work: `stream.camera() | stream.display()`
- [ ] Multi-input composition works declaratively
- [ ] No explicit runtime/connection management needed
- [ ] AI agents can build pipelines without GPU knowledge
- [ ] All examples converted to declarative API

---

## Phase 5: Cross-Platform GPU Support

**Goal**: Support Metal (macOS), CUDA (NVIDIA), Vulkan (AMD/Intel/cross-platform).

### 5.1 Backend Architecture

```
streamlib/gpu/
├── backends/
│   ├── metal/          # macOS (Apple Silicon + Intel)
│   │   ├── device.py
│   │   ├── buffer.py
│   │   └── shader.py
│   ├── cuda/           # NVIDIA GPUs
│   │   ├── device.py
│   │   ├── buffer.py
│   │   └── shader.py
│   └── vulkan/         # Cross-platform (AMD, Intel, others)
│       ├── device.py
│       ├── buffer.py
│       └── shader.py
└── abstract.py         # Abstract base classes
```

### 5.2 Backend Detection

```python
# gpu/backend_detection.py
def detect_gpu_backend() -> str:
    """
    Auto-detect best available GPU backend.

    Priority:
    1. Metal (macOS, optimized for Apple Silicon)
    2. CUDA (NVIDIA, best performance)
    3. Vulkan (cross-platform fallback)
    """
    if sys.platform == 'darwin':
        # macOS: Always use Metal
        return 'metal'

    # Check for CUDA
    try:
        import pycuda.driver as cuda
        cuda.init()
        if cuda.Device.count() > 0:
            return 'cuda'
    except ImportError:
        pass

    # Fallback to Vulkan
    return 'vulkan'
```

### 5.3 Metal Backend (macOS)

**Already partially implemented**, needs cleanup:

```python
# gpu/backends/metal/device.py
class MetalDevice(GPUDevice):
    """Metal GPU device (macOS)."""
    def __init__(self):
        self.device = Metal.MTLCreateSystemDefaultDevice()
        self.command_queue = self.device.newCommandQueue()
        self.texture_cache = None  # For CVPixelBuffer zero-copy

    def create_buffer(self, width: int, height: int, format: PixelFormat) -> MetalBuffer:
        """Allocate Metal texture."""
        descriptor = Metal.MTLTextureDescriptor.texture2DDescriptorWithPixelFormat_width_height_mipmapped_(
            self._format_to_metal(format),
            width,
            height,
            False
        )
        texture = self.device.newTextureWithDescriptor_(descriptor)
        return MetalBuffer(texture, width, height, format)
```

### 5.4 CUDA Backend (NVIDIA)

**New implementation**:

```python
# gpu/backends/cuda/device.py
import pycuda.driver as cuda
from pycuda.compiler import SourceModule

class CUDADevice(GPUDevice):
    """CUDA GPU device (NVIDIA)."""
    def __init__(self, device_id: int = 0):
        cuda.init()
        self.device = cuda.Device(device_id)
        self.context = self.device.make_context()
        self.stream = cuda.Stream()

    def create_buffer(self, width: int, height: int, format: PixelFormat) -> CUDABuffer:
        """Allocate CUDA array."""
        bytes_per_pixel = self._format_to_bytes(format)
        size = width * height * bytes_per_pixel

        array = cuda.mem_alloc(size)
        return CUDABuffer(array, width, height, format)
```

### 5.5 Vulkan Backend (Cross-Platform)

**New implementation**:

```python
# gpu/backends/vulkan/device.py
import vulkan as vk

class VulkanDevice(GPUDevice):
    """Vulkan GPU device (cross-platform)."""
    def __init__(self, device_id: int = 0):
        # Initialize Vulkan instance
        app_info = vk.VkApplicationInfo(
            pApplicationName='streamlib',
            applicationVersion=vk.VK_MAKE_VERSION(1, 0, 0),
            pEngineName='streamlib',
            engineVersion=vk.VK_MAKE_VERSION(1, 0, 0),
            apiVersion=vk.VK_API_VERSION_1_2
        )

        self.instance = vk.vkCreateInstance(
            vk.VkInstanceCreateInfo(pApplicationInfo=app_info)
        )

        # Select physical device
        physical_devices = vk.vkEnumeratePhysicalDevices(self.instance)
        self.physical_device = physical_devices[device_id]

        # Create logical device
        self.device = self._create_logical_device()

    def create_buffer(self, width: int, height: int, format: PixelFormat) -> VulkanBuffer:
        """Allocate Vulkan image."""
        # Create VkImage for texture
        image_info = vk.VkImageCreateInfo(
            imageType=vk.VK_IMAGE_TYPE_2D,
            format=self._format_to_vulkan(format),
            extent=vk.VkExtent3D(width=width, height=height, depth=1),
            mipLevels=1,
            arrayLayers=1,
            usage=vk.VK_IMAGE_USAGE_STORAGE_BIT | vk.VK_IMAGE_USAGE_TRANSFER_SRC_BIT
        )

        image = vk.vkCreateImage(self.device, image_info, None)
        return VulkanBuffer(image, width, height, format)
```

### 5.6 Implementation Tasks

1. **Complete Metal backend**
   - Clean up existing Metal code
   - Ensure IOSurface zero-copy for camera
   - Test on Apple Silicon and Intel Macs

2. **Implement CUDA backend**
   - Device initialization
   - Buffer allocation (CUDA arrays)
   - Shader loading (PTX from Slang)
   - Test on NVIDIA RTX GPUs

3. **Implement Vulkan backend**
   - Instance/device setup
   - Image allocation
   - Shader loading (SPIR-V from Slang)
   - Test on AMD, Intel, NVIDIA GPUs

4. **Unified shader dispatch**
   - Abstract `GPUShader.dispatch()` works across backends
   - Handle platform differences (Metal compute, CUDA kernels, Vulkan compute)

5. **Cross-platform testing**
   - macOS (Metal): MacBook Pro M1/M2
   - Linux + NVIDIA (CUDA): Ubuntu + RTX 3080
   - Linux + AMD (Vulkan): Ubuntu + RX 6800
   - Windows (Vulkan or CUDA): Windows 11 + RTX 4090

### 5.7 Success Criteria

- [ ] Metal backend: 60 FPS on macOS (1080p)
- [ ] CUDA backend: 60 FPS on NVIDIA GPU (1080p)
- [ ] Vulkan backend: 60 FPS on AMD/Intel GPU (1080p)
- [ ] Same codebase runs on all platforms
- [ ] Auto-detection works correctly
- [ ] User can override: `StreamRuntime(backend='cuda')`

---

## Migration Strategy

### For Existing Code

**Phase 1-2 (Non-breaking):**
- Keep old handlers alongside new GPU-only handlers
- Add `_gpu` suffix to new handlers initially
- Provide migration guide

**Phase 3-4 (Breaking):**
- Remove old handlers
- Update all examples
- Update documentation
- Release as v2.0.0

### For Users

**Minimal changes for simple cases:**
```python
# Before:
from streamlib.handlers import CameraHandlerGPU, DisplayGPUHandler
camera = CameraHandlerGPU(device_name="...", width=1920, height=1080)
display = DisplayGPUHandler(width=1920, height=1080)

# After (imperative):
from streamlib.handlers import CameraHandler, DisplayHandler
camera = CameraHandler(device_name="...", width=1920, height=1080)
display = DisplayHandler(width=1920, height=1080)

# After (declarative):
from streamlib import stream
stream.camera(resolution='1080p') | stream.display()
```

### For Handler Developers

**Update handler base class:**
```python
# Before:
class MyHandler(StreamHandler):
    def __init__(self):
        self.inputs['video'] = VideoInput('video', capabilities=['cpu', 'gpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu', 'gpu'])

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        data = frame.data  # np.ndarray or torch.Tensor

# After:
class MyHandler(StreamHandler):
    def __init__(self):
        self.inputs['video'] = GPUVideoInput('video', format=PixelFormat.RGBA8)
        self.outputs['video'] = GPUVideoOutput('video', format=PixelFormat.RGBA8)

    async def on_start(self):
        self.shader = self.gpu_device.shader_library.load_shader('my_shader')

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        buffer = frame.buffer  # GPUBuffer (zero-copy)

        # Dispatch GPU shader
        self.shader.dispatch(
            inputs={'input': buffer},
            outputs={'output': buffer},  # In-place
            params={'my_param': self.my_value}
        )

        self.outputs['video'].write(frame)
```

---

## Timeline Estimate

**Phase 1: GPU-Only Architecture** (2 weeks)
- Week 1: GPU abstractions (GPUBuffer, GPUDevice, ports)
- Week 2: Update handlers, test pipeline

**Phase 2: Slang Integration** (3 weeks)
- Week 1: Slang toolchain, compilation pipeline
- Week 2: Core shader library (color, compositing, filters)
- Week 3: Convert handlers to use Slang, test

**Phase 3: Runtime Simplification** (1 week)
- Remove capability negotiation, transfer handlers
- Simplify connection logic
- Update documentation

**Phase 4: Declarative API** (2 weeks)
- Week 1: StreamNode, builder pattern, pipe operator
- Week 2: Examples, documentation, testing

**Phase 5: Cross-Platform** (4 weeks)
- Week 1: CUDA backend implementation
- Week 2: Vulkan backend implementation
- Week 3: Testing on multiple platforms
- Week 4: Performance tuning, documentation

**Total: ~12 weeks** for complete GPU-only refactor with cross-platform support.

---

## Success Metrics

### Performance
- [ ] 60 FPS sustained on 1080p video
- [ ] Zero CPU↔GPU transfers in happy path
- [ ] <1ms latency per processing stage
- [ ] ML inference (YOLO) at 30+ FPS

### Code Quality
- [ ] Runtime code reduced by 50%+
- [ ] No CPU fallback code paths
- [ ] All operations verified zero-copy
- [ ] Cross-platform tests passing

### Developer Experience
- [ ] Declarative API: 1 line for simple pipelines
- [ ] AI agents can build pipelines without GPU knowledge
- [ ] Handler development: <50 lines for basic filter
- [ ] Clear error messages for format mismatches

### Platform Support
- [ ] macOS (Metal): Full support
- [ ] Linux + NVIDIA (CUDA): Full support
- [ ] Linux + AMD (Vulkan): Full support
- [ ] Windows (CUDA/Vulkan): Full support

---

## Risk Mitigation

### Risk 1: Slang Compilation Complexity
**Mitigation**: Pre-compile shaders at build time, ship with library

### Risk 2: Platform-Specific Bugs
**Mitigation**: Comprehensive cross-platform CI/CD testing

### Risk 3: Performance Regression
**Mitigation**: Benchmark each phase, compare to previous implementation

### Risk 4: Breaking Changes for Users
**Mitigation**: Clear migration guide, deprecation warnings, v2.0.0 semantic versioning

### Risk 5: Shader Debugging Difficulty
**Mitigation**: Shader validation tools, GPU debugger integration (RenderDoc, Nsight)

---

## Open Questions

1. **ML Framework Integration**: How to integrate PyTorch/TensorFlow inference on GPU?
   - Option A: Direct GPU buffer sharing (ideal, requires framework support)
   - Option B: Minimal transfer to ML framework tensor format
   - Recommendation: Start with B, push for A in PyTorch/TF

2. **Display Backend**: Metal layer (macOS) or Vulkan WSI (cross-platform)?
   - Metal layer: Best for macOS, not portable
   - Vulkan WSI: Portable, but more complex
   - Recommendation: Metal layer for macOS, Vulkan WSI for others

3. **Camera Capture**: Platform-specific APIs?
   - macOS: AVFoundation (current implementation)
   - Linux: V4L2 or GStreamer
   - Windows: Media Foundation
   - Recommendation: Wrap platform APIs, provide unified interface

4. **Network Streaming**: How to serialize GPU buffers for network?
   - Must encode GPU frames to H.264/H.265/AV1
   - Requires hardware encoder access (VideoToolbox, NVENC, VAAPI)
   - Future phase: Network-transparent operations

---

## Conclusion

This refactor represents a **fundamental architectural shift** from a flexible CPU/GPU hybrid to a **pure GPU-only, zero-copy design**. The benefits are:

1. **Simplified API**: No capability negotiation, no transfer handlers
2. **Better performance**: Zero CPU↔GPU transfers, 60 FPS sustained
3. **Cross-platform**: Slang shaders compile to Metal/CUDA/Vulkan
4. **AI-agent-friendly**: Declarative API focuses on "what", not "how"
5. **ML-ready**: GPU-native for realtime object detection, inference

The 12-week timeline is aggressive but achievable with focused execution. The result will be a **best-in-class streaming library** that sets a new standard for GPU-accelerated video processing in Python.
