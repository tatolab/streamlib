# streamlib

**Minimal actor-based streaming framework for Python.**

streamlib is an SDK for building composable streaming pipelines using the actor model. It provides the core primitives - you build the implementations.

## Philosophy

streamlib is a **framework, not a batteries-included library**. Like Cloudflare Actors or PyTorch core, we provide the primitives and let you build on top:

- ✅ **Actor base class** - Build your own actors
- ✅ **Ring buffers** - Latest-read semantics
- ✅ **Clocks** - Software, PTP, Genlock
- ✅ **Registry** - Network-transparent addressing
- ✅ **Zero dependencies** - Just Python stdlib

We do NOT provide:
- ❌ Video processing implementations
- ❌ Drawing/graphics implementations
- ❌ Codec implementations
- ❌ UI implementations

**You** build those using whatever libraries you want (OpenCV, Skia, PIL, FFmpeg, etc.).

## Installation

```bash
pip install streamlib
```

## Quick Start

```python
import asyncio
from streamlib import Actor, StreamInput, StreamOutput, VideoFrame

class MyActor(Actor):
    """You implement the actor logic."""

    def __init__(self):
        super().__init__('my-actor')
        self.inputs['video'] = StreamInput('video')
        self.outputs['video'] = StreamOutput('video')
        # Actor begins processing automatically

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

# Actors start processing immediately when created
source = MySourceActor()
processor = MyActor()
sink = MySinkActor()

# Connect actors (creates data flow paths)
source.outputs['video'] >> processor.inputs['video']
processor.outputs['video'] >> sink.inputs['video']
```

## Core Concepts

### Actors

Actors are independent, concurrent components that:
- Process ticks from a clock
- Read from input ring buffers (latest-read semantics)
- Write to output ring buffers
- Auto-start on creation

### Ring Buffers

Fixed-size (3 slots) ring buffers with latest-read semantics:
- No queueing, no backpressure
- Always get the latest data
- Old frames are automatically skipped

### Clocks

Swappable clock sources:
- `SoftwareClock` - Free-running software timer
- `PTPClock` - IEEE 1588 Precision Time Protocol (stub)
- `GenlockClock` - Hardware sync (stub)

### Registry

Network-transparent actor addressing:
```python
actor://local/MyActor/instance-id
actor://192.168.1.100/MyActor/instance-id
```

## Examples

See the `examples/` directory for reference implementations:
- `examples/actors/video.py` - Video processing actors
- `examples/actors/compositor.py` - Alpha blending compositor
- `examples/actors/drawing.py` - Skia-based drawing
- `examples/demo_*.py` - Complete demos

These are **reference implementations**, not part of the core library.

## Building Your Own Actors

streamlib is designed to be extended. Build actors for:
- Video processing (OpenCV, PyAV, FFmpeg)
- Graphics (Skia, PIL, Cairo, Manim)
- Audio (PyAudio, sounddevice)
- ML (PyTorch, TensorFlow, ONNX)
- Network (WebRTC, HLS, RTMP)

Use whatever libraries make sense for your use case.

## Design Inspiration

- **Cloudflare Actors** - Minimal framework, you build on top
- **PyTorch** - Core tensor operations, ecosystem builds plugins
- **Unix philosophy** - Composable primitives, not monoliths

## License

MIT
