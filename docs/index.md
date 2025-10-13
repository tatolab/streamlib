# streamlib

**Composable streaming primitives for Python** — Build video pipelines like Unix pipes.

```python
# Just like Unix pipes
cat file.txt | grep "error" | sed 's/ERROR/WARNING/'

# streamlib for video
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])
```

## Why streamlib?

Most video tools are **monolithic applications** (Unity, OBS, complex streaming platforms). They're environments, not primitives.

**streamlib provides Unix-pipe-style primitives for video** that AI agents (and humans) can orchestrate:

- 🔧 **Composable** - Small, single-purpose handlers that chain together
- 🚀 **GPU-first** - Automatic GPU optimization, stays on GPU when possible
- 🌐 **Network-transparent** - Operations work seamlessly locally or remotely (future)
- 🎯 **Easy installation** - Simple pip install, no GStreamer required
- 🤖 **AI-friendly** - Designed for AI agent orchestration

## Quick Start

```bash
pip install streamlib
```

**Your First Pipeline (30 seconds):**

```python
import asyncio
from streamlib import StreamRuntime, Stream
from streamlib.handlers import TestPatternHandler, DisplayGPUHandler

async def main():
    runtime = StreamRuntime(fps=30)

    pattern = TestPatternHandler(width=1280, height=720, pattern='smpte_bars')
    display = DisplayGPUHandler(window_name='Hello streamlib', width=1280, height=720)

    runtime.add_stream(Stream(pattern))
    runtime.add_stream(Stream(display))
    runtime.connect(pattern.outputs['video'], display.inputs['video'])

    runtime.start()
    try:
        while runtime._running:
            await asyncio.sleep(1)
    except KeyboardInterrupt:
        pass
    runtime.stop()

asyncio.run(main())
```

**Result:** Window displays SMPTE color bars at 30 FPS!

## Next Steps

- [Quick Start Guide](guides/quickstart.md) - Learn the basics
- [Composition Guide](guides/composition.md) - Build complex pipelines
- [API Reference](api/) - Complete documentation

## Examples

```bash
# Test pattern
python examples/demo_test_pattern.py

# Camera
python examples/demo_camera.py

# Multi-pipeline composition
python examples/demo_multi_pipeline.py --mode pip --camera "Live Camera"
```

## Philosophy

### The Problem

Most streaming/visual tools are **large, stateful, monolithic applications**. They're environments, not primitives. There's no equivalent to "pipe grep into sed into awk" for visual operations.

### The Solution

**Stateless visual primitives that can be orchestrated** - like Unix tools but for video/image processing.

### Core Principles

1. **Composable primitives** - Small, single-purpose components that chain together
2. **Stateless handlers** - Pure processing logic, runtime manages lifecycle
3. **GPU-first by default** - All operations use GPU unless explicitly configured otherwise
4. **Automatic optimization** - Runtime infers optimal execution and memory paths
5. **Network-transparent** - Operations work locally or remotely (future)
6. **Tool-first, not product** - Provide primitives, let use cases emerge

## License

MIT
