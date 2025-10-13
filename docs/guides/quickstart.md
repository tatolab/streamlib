# Quick Start

Build your first streamlib pipeline in 5 minutes.

## Installation

```bash
pip install streamlib
```

## Your First Pipeline

Let's create the simplest possible pipeline: Generate a test pattern and display it.

### Step 1: Import

```python
import asyncio
from streamlib import StreamRuntime, Stream
from streamlib.handlers import TestPatternHandler, DisplayGPUHandler
```

### Step 2: Create Handlers

```python
# Test pattern generator
pattern = TestPatternHandler(
    width=1280,
    height=720,
    pattern='smpte_bars'  # SMPTE color bars
)

# Display window
display = DisplayGPUHandler(
    window_name='My First Pipeline',
    width=1280,
    height=720,
    show_fps=True
)
```

### Step 3: Create Runtime and Connect

```python
# Create runtime (30 FPS)
runtime = StreamRuntime(fps=30)

# Add handlers
runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
runtime.add_stream(Stream(display, dispatcher='threadpool'))

# Connect: pattern → display
runtime.connect(pattern.outputs['video'], display.inputs['video'])
```

### Step 4: Run

```python
async def main():
    # Start pipeline
    runtime.start()

    # Run until interrupted
    try:
        while runtime._running:
            await asyncio.sleep(1)
    except KeyboardInterrupt:
        print("Stopping...")

    # Cleanup
    runtime.stop()

# Run
asyncio.run(main())
```

### Complete Code

```python
#!/usr/bin/env python3
"""Your first streamlib pipeline."""

import asyncio
from streamlib import StreamRuntime, Stream
from streamlib.handlers import TestPatternHandler, DisplayGPUHandler

async def main():
    # Create runtime
    runtime = StreamRuntime(fps=30)

    # Create handlers
    pattern = TestPatternHandler(
        width=1280,
        height=720,
        pattern='smpte_bars'
    )

    display = DisplayGPUHandler(
        window_name='My First Pipeline',
        width=1280,
        height=720,
        show_fps=True
    )

    # Add to runtime
    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='threadpool'))

    # Connect
    runtime.connect(pattern.outputs['video'], display.inputs['video'])

    # Start
    runtime.start()

    # Run until interrupted
    try:
        while runtime._running:
            await asyncio.sleep(1)
    except KeyboardInterrupt:
        print("\nStopping...")

    runtime.stop()

if __name__ == '__main__':
    asyncio.run(main())
```

Run it:
```bash
python my_first_pipeline.py
```

You should see an OpenGL window displaying SMPTE color bars at 30 FPS!

## Add a Camera

Replace the test pattern with a real camera:

```python
from streamlib.handlers import CameraHandlerGPU

# Replace pattern with camera
camera = CameraHandlerGPU(
    device_name="Live Camera",  # Your camera name
    width=1280,
    height=720
)

runtime.add_stream(Stream(camera, dispatcher='asyncio'))

# Connect: camera → display
runtime.connect(camera.outputs['video'], display.inputs['video'])
```

## Add a Filter

Insert a blur filter between camera and display:

```python
from streamlib.handlers import BlurFilterGPU

# Create handlers
camera = CameraHandlerGPU(device_name="Live Camera", width=1280, height=720)
blur = BlurFilterGPU(kernel_size=15, sigma=8.0)
display = DisplayGPUHandler(width=1280, height=720)

# Add to runtime
runtime.add_stream(Stream(camera, dispatcher='asyncio'))
runtime.add_stream(Stream(blur, dispatcher='gpu'))
runtime.add_stream(Stream(display, dispatcher='threadpool'))

# Connect: camera → blur → display
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])
```

Now your camera feed is blurred in real-time on the GPU!

## Understanding the Pattern

Every streamlib pipeline follows this structure:

```python
# 1. Create runtime
runtime = StreamRuntime(fps=30)

# 2. Create handlers
handler1 = SomeHandler(...)
handler2 = AnotherHandler(...)

# 3. Add to runtime
runtime.add_stream(Stream(handler1))
runtime.add_stream(Stream(handler2))

# 4. Connect handlers
runtime.connect(handler1.outputs['video'], handler2.inputs['video'])

# 5. Start
runtime.start()

# 6. Run
try:
    while runtime._running:
        await asyncio.sleep(1)
except KeyboardInterrupt:
    pass

# 7. Stop
runtime.stop()
```

## Available Handlers

streamlib includes several built-in handlers:

### Sources
- **`TestPatternHandler`** - Generate test patterns (SMPTE bars, gradients, etc.)
- **`CameraHandlerGPU`** - Capture from camera with zero-copy GPU

### Filters
- **`BlurFilterGPU`** - Gaussian blur on GPU
- **`GrayscaleHandler`** - Convert to grayscale

### Composition
- **`MultiInputCompositor`** - Combine multiple video streams (PIP, side-by-side, etc.)

### Outputs
- **`DisplayGPUHandler`** - OpenGL window display with FPS overlay
- **`FileWriterHandler`** - Save frames to files

### Graphics
- **`LowerThirdsGPUHandler`** - Text overlays
- **`DrawingHandler`** - Custom graphics

See the [API documentation](../api/handler.md) for complete handler reference.

## Next Steps

- [Composition Guide](composition.md) - Build complex multi-pipeline systems
- [Write Your Own Handler](custom-handler.md) - Create custom processing logic
- [GPU Optimization](gpu-optimization.md) - Understand zero-copy GPU pipelines
- [Examples](../examples/) - More complete examples

## Common Issues

### Window doesn't appear

Make sure you're using `dispatcher='threadpool'` for DisplayGPUHandler:

```python
runtime.add_stream(Stream(display, dispatcher='threadpool'))  # Not 'asyncio'!
```

### Camera not found

List available cameras:

```python
import AVFoundation

devices = AVFoundation.AVCaptureDevice.devicesWithMediaType_(
    AVFoundation.AVMediaTypeVideo
)
for device in devices:
    print(device.localizedName())
```

### Low FPS

Check your processing time. If handlers take longer than 1/FPS seconds, you'll drop frames:

```python
# 30 FPS = 33ms per frame
# If your handler takes 50ms, you'll only get ~20 FPS
runtime = StreamRuntime(fps=30)  # Try lower FPS
```

## Philosophy

streamlib is designed around composability:

- **Handlers are Unix tools** - Small, single-purpose, composable
- **Runtime is the shell** - Orchestrates and connects
- **Ports are pipes** - Data flows through ports like Unix pipes
- **Zero-copy where possible** - GPU data stays on GPU

This makes it easy for AI agents (and humans) to orchestrate complex video pipelines by combining simple primitives.

## Help

- [GitHub Issues](https://github.com/tatolab/streamlib/issues)
- [API Reference](../api/)
- [Examples](../examples/)
