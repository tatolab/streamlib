# streamlib

**Minimal actor-based streaming framework for Python.**

streamlib is an SDK for building composable streaming pipelines using the actor model. It provides the core primitives - you build the implementations.

## 🎯 Philosophy

streamlib is a **framework, not a batteries-included library**. Like Cloudflare Actors or PyTorch core, we provide the primitives and let you build on top.

**What we provide:**
- ✅ Actor base class and lifecycle management
- ✅ Ring buffers with latest-read semantics
- ✅ Clocks (Software, PTP, Genlock)
- ✅ Actor registry with network-transparent addressing
- ✅ Zero dependencies (just Python stdlib)

**What we DON'T provide:**
- ❌ Video processing implementations
- ❌ Drawing/graphics implementations
- ❌ Codec implementations

You build those using whatever libraries you want (OpenCV, Skia, PIL, FFmpeg, etc.).

## 📦 Installation

```bash
# Install core framework only (zero dependencies)
pip install ./packages/streamlib

# Or for development (includes example dependencies)
uv sync
```

## 🚀 Quick Start

```python
import asyncio
from streamlib import Actor, StreamInput, StreamOutput, VideoFrame

class MyActor(Actor):
    """You implement the actor logic."""

    def __init__(self):
        super().__init__('my-actor')
        self.inputs['video'] = StreamInput('video')
        self.outputs['video'] = StreamOutput('video')
        # ✅ Actor auto-starts here (no start() call needed!)

    async def process(self, tick):
        """Process each tick from the clock."""
        frame = self.inputs['video'].read_latest()
        if frame is None:
            return

        # Your processing logic here
        processed = your_processing_function(frame.data)

        output_frame = VideoFrame(
            data=processed,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=frame.width,
            height=frame.height
        )
        self.outputs['video'].write(output_frame)
```

## 📚 Examples

See `examples/` directory for reference implementations:

- **`examples/actors/`** - Example actor implementations (video, compositor, drawing)
- **`examples/demo_*.py`** - Complete demos showing how to use the framework

These are **reference implementations**, not part of the core library. Use them as starting points for your own implementations.

## 🏗️ Architecture

### Core Framework (`packages/streamlib/`)

Minimal SDK with zero dependencies:
- `Actor` - Base class for building actors
- `RingBuffer` - Latest-read ring buffers (3 slots)
- `Clock` - Timing abstraction (Software, PTP, Genlock)
- `Registry` - Network-transparent actor addressing
- `StreamInput`/`StreamOutput` - Connection ports

### Examples (`examples/`)

Reference implementations showing how to build actors:
- `TestPatternActor` - Generate test patterns (uses NumPy)
- `CompositorActor` - Alpha blending (uses NumPy)
- `DrawingActor` - Graphics (uses Skia)
- `DisplayActor` - Display window (uses OpenCV)

**These are NOT maintained as part of core** - they demonstrate patterns.

## 🔧 Building Your Own Actors

streamlib is designed to be extended. Build actors for:
- Video processing (OpenCV, PyAV, FFmpeg)
- Graphics (Skia, PIL, Cairo, Manim)
- Audio (PyAudio, sounddevice)
- ML (PyTorch, TensorFlow, ONNX)
- Network (WebRTC, HLS, RTMP)

Use whatever libraries make sense for your use case.

## 🌐 Network-Transparent Design

All actors have URIs:
```python
actor://local/MyActor/instance-id
actor://192.168.1.100/MyActor/instance-id
```

This enables distributed processing (Phase 4):
```python
source = connect_actor('actor://local/TestPatternActor/test1')
compositor = connect_actor('actor://edge-server/CompositorActor/main')
display = connect_actor('actor://local/DisplayActor/output')

source >> compositor >> display
```

## 🧪 Testing

```bash
# Run tests
uv run pytest

# All tests use core framework + example actors
# 55/55 tests passing ✅
```

## 📖 Documentation

- `packages/streamlib/README.md` - Core framework documentation
- `examples/DEMOS.md` - Demo documentation (moved to examples/)
- `FIXES.md` - Recent fixes and improvements

## 🎨 Design Inspiration

- **Cloudflare Actors** - Minimal framework, you build on top
- **PyTorch** - Core tensor operations, ecosystem builds plugins
- **Unix philosophy** - Composable primitives, not monoliths

## 🤝 Contributing

Contributions welcome! We maintain:
- Core framework (minimal, stable, zero dependencies)
- Example actors (reference implementations)
- Documentation

We do NOT maintain specific implementations - that's for the ecosystem.

## 📄 License

MIT
