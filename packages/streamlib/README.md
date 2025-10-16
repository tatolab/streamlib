# streamlib

Realtime streaming SDK for composing camera streams, ML models, audio/video generation, and visual effects on GPU.

## API Levels

### Decorator API

Functions decorated with `@camera_source`, `@video_effect`, or `@audio_effect` become handlers. GPU context is automatically injected.

```python
from streamlib import camera_source, video_effect, VideoFrame
from streamlib.gpu import GPUContext

@camera_source(device_id=None)  # First available camera
def camera():
    """Zero-copy camera source - no code needed!"""
    pass  # Decorator handles everything (IOSurface → WebGPU on macOS)

@video_effect
def blur(frame: VideoFrame, gpu: GPUContext, sigma: float = 2.0) -> VideoFrame:
    # GPU context automatically injected
    # frame.data is wgpu.GPUTexture (zero-copy from camera!)

    if not hasattr(blur, 'pipeline'):
        blur.pipeline = gpu.create_compute_pipeline(BLUR_SHADER)

    output = gpu.create_texture(frame.width, frame.height)
    gpu.run_compute(blur.pipeline, input=frame.data, output=output)

    return frame.clone_with_texture(output)

@audio_effect
def reverb(audio: AudioBuffer, decay: float = 0.5) -> AudioBuffer:
    # Process audio buffer
    processed = apply_reverb(audio.data, decay)
    return AudioBuffer(data=processed, timestamp=audio.timestamp)
```

### Pipeline Builder API (Recommended)

Fluent API for composing handlers with automatic connections:

```python
from streamlib import StreamRuntime, camera_source, video_effect

runtime = StreamRuntime(fps=60, width=1920, height=1080)

@camera_source(device_id=None)  # Zero-copy camera
def camera():
    pass

@video_effect
def blur(frame, gpu, sigma=2.0):
    if not hasattr(blur, 'pipeline'):
        blur.pipeline = gpu.create_compute_pipeline(BLUR_SHADER)
    output = gpu.create_texture(frame.width, frame.height)
    gpu.run_compute(blur.pipeline, input=frame.data, output=output)
    return frame.clone_with_texture(output)

# Fluent pipeline - runtime.pipeline() method!
p = (
    runtime.pipeline()
    .source(camera)
    .effect(blur, sigma=5.0)
    .effect(sharpen, strength=0.5)
    .sink(display)
)
p.build()  # Adds streams and connects them
await runtime.start()  # Starts the runtime
```

Automatic port connections, type validation, supports branching. Zero-copy camera → GPU effects → display.

### StreamHandler API

Direct handler implementation for custom behavior:

```python
from streamlib import StreamHandler, VideoInput, VideoOutput

class CustomEffect(StreamHandler):
    def __init__(self):
        super().__init__()
        self.inputs['video'] = VideoInput('video')
        self.outputs['video'] = VideoOutput('video')

    async def on_start(self):
        self._pipeline = self._runtime.gpu_context.create_compute_pipeline(SHADER)

    async def process(self, tick):
        frame = self.inputs['video'].read_latest()
        if frame:
            output = self._runtime.gpu_context.create_texture(frame.width, frame.height)
            self._runtime.gpu_context.run_compute(self._pipeline,
                                                   input=frame.data,
                                                   output=output)
            self.outputs['video'].write(VideoFrame(data=output, timestamp=tick.timestamp))
```

## Architecture

### WebGPU Throughout

VideoFrame.data is always `wgpu.GPUTexture`. No CPU transfers between handlers.

```
Camera → wgpu.GPUTexture
  ↓
Blur → wgpu.GPUTexture (WGSL compute shader)
  ↓
ML Model → wgpu.GPUTexture (ONNX WebGPU EP)
  ↓
Display → wgpu.GPUTexture (swapchain)
```

### GPU Context

Runtime creates single `GPUContext` shared by all handlers. Decorators receive it automatically.

```python
runtime = StreamRuntime(fps=60)  # Creates GPU context
# Handlers access via self._runtime.gpu_context or decorator injection
```

### Ports

Handlers declare typed inputs/outputs. Runtime connects compatible ports.

```python
self.inputs['video'] = VideoInput('video')
self.outputs['video'] = VideoOutput('video')

runtime.connect(handler1.outputs['video'], handler2.inputs['video'])
```

Ports use ring buffers for zero-copy texture reference passing.

## Platform Integration

Camera capture uses platform APIs but outputs WebGPU texture:

- macOS: AVFoundation → IOSurface → WebGPU texture import
- Linux: V4L2 → DMA-BUF → WebGPU texture import
- Windows: MediaFoundation → D3D11 → WebGPU texture import

### WGSL Shaders

Compute shaders written in WGSL compile to platform backend:

- macOS: Metal Shading Language
- Windows: HLSL (DirectX 12)
- Linux: SPIR-V (Vulkan)

Single shader source works across all platforms.

## Audio and A/V Processing

### Audio Buffers

AudioBuffer contains PCM audio data with timestamp and sample rate.

```python
from streamlib import audio_effect, AudioBuffer

@audio_effect
def normalize(audio: AudioBuffer, target_db: float = -20.0) -> AudioBuffer:
    normalized = normalize_loudness(audio.data, target_db)
    return AudioBuffer(
        data=normalized,
        timestamp=audio.timestamp,
        sample_rate=audio.sample_rate,
        channels=audio.channels
    )
```

### Multiple Outputs

Handlers can output both audio and video:

```python
class CameraHandler(StreamHandler):
    def __init__(self):
        super().__init__()
        self.outputs['video'] = VideoOutput('video')
        self.outputs['audio'] = AudioOutput('audio')

    async def process(self, tick):
        video_texture, audio_buffer = self.capture.get_frame()
        self.outputs['video'].write(VideoFrame(data=video_texture, timestamp=tick.timestamp))
        self.outputs['audio'].write(AudioBuffer(data=audio_buffer, timestamp=tick.timestamp))
```

### A/V Synchronization

Runtime provides synchronized ticks at configured FPS. Handlers receive same timestamp for alignment.

```python
runtime = StreamRuntime(fps=60)  # 60 ticks/second
# All handlers receive tick with same timestamp
# Audio and video use tick.timestamp to stay synchronized
```

Pipeline builder handles A/V automatically:

```python
p = (
    pipeline(runtime)
    .source(camera)  # Outputs both audio and video
    .split(
        audio=lambda p: p.effect(reverb).effect(normalize),
        video=lambda p: p.effect(blur).effect(color_grade)
    )
    .join(av_sync)  # Receives both streams, ensures sync
    .sink(display)
)
