# streamlib

A composable streaming library for Python with network-transparent operations.

## Overview

**streamlib** is a Unix-pipe-style library for video streaming that enables you to chain operations locally or across machines. It's designed to be:

- **Network-transparent**: Operations work seamlessly locally or remotely
- **Distributed**: Chain operations across machines (phone → edge → cloud)
- **Mesh-capable**: Multiple machines collaborate on processing
- **Zero-dependency**: No GStreamer or system packages required (uses PyAV)
- **Composable**: Build complex pipelines from simple primitives

## Philosophy

Like Unix tools (`cat`, `grep`, `awk`), streamlib provides single-purpose components that can be composed together. Each component does one thing well and can run on any machine.

## Core Concepts

### Sources
Produce frames:
- `WebcamSource` - Capture from camera
- `FileSource` - Read video files
- `ScreenSource` - Screen capture
- `NetworkSource` - Receive from remote machine
- `TestSource` - Procedural content

### Sinks
Consume frames:
- `HLSSink` - HTTP Live Streaming
- `FileSink` - Write video files
- `DisplaySink` - Show in window
- `NetworkSink` - Send to remote machine

### Layers
Process/generate visual content:
- `VideoLayer` - Pass-through video
- `DrawingLayer` - Execute Python drawing code (Skia)
- `MLLayer` - Run ML models with zero-copy

### Compositor
Combines layers using alpha blending:
- `DefaultCompositor` - Zero-copy numpy pipeline
- Manages z-ordering, visibility, opacity

## Installation

```bash
# Basic installation
pip install -e .

# With ML support
pip install -e ".[ml]"

# With network streaming (WebRTC)
pip install -e ".[network]"

# Development
pip install -e ".[dev]"
```

## Quick Start

### Example 1: Simple Drawing Layer

```python
import asyncio
from streamlib import DrawingLayer, DefaultCompositor, TimestampedFrame

# Define drawing code
draw_code = """
def draw(canvas, ctx):
    import skia
    paint = skia.Paint()
    paint.setColor(skia.Color(255, 0, 0, 255))

    # Animated circle
    radius = 50 + 30 * np.sin(ctx.time * 2)
    canvas.drawCircle(ctx.width / 2, ctx.height / 2, radius, paint)
"""

# Create layer
layer = DrawingLayer(
    name="animated_circle",
    draw_code=draw_code,
    width=1920,
    height=1080
)

# Create compositor
compositor = DefaultCompositor(width=1920, height=1080)
compositor.add_layer(layer)

# Generate a frame
async def main():
    frame = await compositor.composite()
    print(f"Generated frame: {frame.frame.shape}")

asyncio.run(main())
```

### Example 2: Multi-Layer Composition

```python
import asyncio
from streamlib import DrawingLayer, VideoLayer, DefaultCompositor

# Background video
video_layer = VideoLayer(name="background", z_index=0)

# Overlay graphics
overlay_code = """
def draw(canvas, ctx):
    import skia
    paint = skia.Paint()
    paint.setAntiAlias(True)

    # Semi-transparent overlay
    paint.setColor(skia.Color(0, 0, 0, 128))
    canvas.drawRect(skia.Rect(0, ctx.height - 100, ctx.width, ctx.height), paint)

    # Text
    paint.setColor(skia.Color(255, 255, 255, 255))
    font = skia.Font(skia.Typeface('Arial'), 32)
    canvas.drawString(f"Frame {ctx.frame_number}", 20, ctx.height - 40, font, paint)
"""

overlay_layer = DrawingLayer(
    name="overlay",
    draw_code=overlay_code,
    z_index=1
)

# Compose
compositor = DefaultCompositor(width=1920, height=1080)
compositor.add_layer(video_layer)
compositor.add_layer(overlay_layer)

async def main():
    frame = await compositor.composite()
    print(f"Composited frame: {frame.frame.shape}")

asyncio.run(main())
```

### Example 3: Custom Context Variables

```python
from streamlib import DrawingLayer

draw_code = """
def draw(canvas, ctx):
    import skia
    paint = skia.Paint()
    paint.setColor(skia.Color(255, 255, 255, 255))
    font = skia.Font(skia.Typeface('Arial'), 48)

    # Access custom variables
    message = getattr(ctx, 'score', 'No score')
    canvas.drawString(f"Score: {message}", 100, 100, font, paint)
"""

layer = DrawingLayer(name="hud", draw_code=draw_code)

# Update context dynamically
layer.update_context(score=1234)
```

## Architecture

### TimestampedFrame

Every frame in streamlib carries precise timing information:

```python
@dataclass
class TimestampedFrame:
    frame: NDArray[np.uint8]  # The actual frame data
    timestamp: float            # Wall clock time
    frame_number: int          # Sequential frame number
    ptp_time: Optional[float]  # PTP synchronized time
    source_id: Optional[str]   # Source identifier
    metadata: Optional[Dict]   # Additional metadata
```

### Zero-Copy Pipeline

streamlib uses numpy arrays throughout, enabling:
- Zero-copy frame processing
- Direct tensor conversions for ML
- Efficient alpha blending
- Minimal memory overhead

## Phase 1: Core Infrastructure ✅

**Status**: Complete

The following components are implemented and tested:

- ✅ Abstract base classes (Source, Sink, Layer, Compositor)
- ✅ TimestampedFrame with precise timing
- ✅ DrawingLayer with Skia support
- ✅ VideoLayer for pass-through frames
- ✅ DefaultCompositor with zero-copy alpha blending
- ✅ Plugin registration system
- ✅ Frame timing utilities
- ✅ PTP client for time synchronization
- ✅ Multi-stream synchronizer

**Tests**: 9/9 passing

## Roadmap

### Phase 2: Basic Sources & Sinks
- [ ] FileSource using PyAV
- [ ] TestSource (test patterns)
- [ ] FileSink using PyAV
- [ ] HLSSink using PyAV
- [ ] DisplaySink for preview

### Phase 3: Hardware I/O
- [ ] WebcamSource using PyAV
- [ ] ScreenCaptureSource (platform-specific)
- [ ] Audio support

### Phase 4: Network-Transparent Operations
- [ ] Serializable stream format
- [ ] NetworkSource (TCP/UDP/WebRTC)
- [ ] NetworkSink
- [ ] Compression options (JPEG, H.264)

### Phase 5: Time Synchronization
- [ ] Full PTP implementation
- [ ] SyncedSource with hardware timestamps
- [ ] MultiStreamCompositor for multi-camera
- [ ] Temporal alignment algorithms

### Phase 6: ML & GPU Acceleration
- [ ] MLLayer base class
- [ ] Zero-copy numpy ↔ tensor conversions
- [ ] GPU device management
- [ ] PyTorch/TensorFlow integration

### Phase 7: Advanced Features
- [ ] Object detection examples
- [ ] AR measurement tools
- [ ] 3D tracking examples
- [ ] Performance optimization

## Development

### Running Tests

```bash
# Run all tests
pytest tests/ -v

# Run specific test
pytest tests/test_streamlib_core.py -v

# Run with coverage
pytest tests/ --cov=streamlib
```

### Code Style

```bash
# Format code
black streamlib/

# Lint
ruff check streamlib/

# Type check
mypy streamlib/
```

## License

MIT

## Contributing

Contributions welcome! Please:
1. Fork the repository
2. Create a feature branch
3. Add tests for new features
4. Ensure all tests pass
5. Submit a pull request

## Credits

Inspired by:
- [fastrtc](https://github.com/gradio-app/fastrtc) - WebRTC streaming
- Unix philosophy - Composable tools
- GStreamer - Pipeline architecture
