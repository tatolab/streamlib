# streamlib-extras

Reference handler implementations for streamlib.

## Installation

```bash
pip install streamlib-extras
```

## What's Included

### Patterns
- **TestPatternHandler** - Generate test patterns (SMPTE bars, gradients, etc.)

### Camera
- **CameraHandler** - OpenCV-based camera capture (CPU)
- **CameraHandlerGPU** - Zero-copy GPU camera capture (macOS/AVFoundation)

### Display
- **DisplayHandler** - OpenCV-based display (CPU)
- **DisplayGPUHandler** - GPU-accelerated display (OpenGL/moderngl)

### Effects
- **BlurFilter** - Adaptive blur (auto-selects CPU/GPU)
- **BlurFilterGPU** - PyTorch GPU blur
- **BlurFilterMetal** - Metal Performance Shaders blur (macOS)
- **CompositorHandler** - CPU-based compositor
- **MultiInputCompositor** - GPU-accelerated multi-input compositor

### Overlays
- **LowerThirdsHandler** - Lower thirds graphics (CPU)
- **LowerThirdsGPUHandler** - GPU-accelerated lower thirds
- **GPUTextOverlayHandler** - GPU text overlay

### Utils
- **DrawingHandler** - Procedural graphics
- **DrawingContext** - Drawing utilities

## Usage

```python
from streamlib import StreamRuntime, Stream
from streamlib_extras import TestPatternHandler, DisplayGPUHandler

runtime = StreamRuntime(fps=30)

pattern = TestPatternHandler(width=1280, height=720, pattern='smpte_bars')
display = DisplayGPUHandler(window_name='Video', width=1280, height=720)

runtime.add_stream(Stream(pattern))
runtime.add_stream(Stream(display))
runtime.connect(pattern.outputs['video'], display.inputs['video'])

runtime.start()
```

## Optional Dependencies

Install with extras for specific features:

```bash
# GPU support
pip install streamlib-extras[gpu]

# Display support
pip install streamlib-extras[display]

# Text overlays
pip install streamlib-extras[text]

# All features
pip install streamlib-extras[all]
```

## Philosophy

streamlib-extras provides **reference implementations** - handlers that demonstrate best practices for building streamlib pipelines. These handlers are:

- **GPU-first** - Optimized for GPU when available
- **Well-documented** - Clear examples of handler patterns
- **Production-ready** - Tested and performant
- **Modular** - Use only what you need

## See Also

- [streamlib core](../streamlib/) - SDK primitives
- [Documentation](../../docs/) - Complete guides
- [Examples](../../examples/) - Usage examples
