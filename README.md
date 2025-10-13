# streamlib

**Composable streaming primitives for Python** — Build video pipelines like Unix pipes.

```python
# Just like Unix pipes
cat file.txt | grep "error" | sed 's/ERROR/WARNING/'

# streamlib for video
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])
```

[![PyPI](https://img.shields.io/pypi/v/streamlib)](https://pypi.org/project/streamlib/)
[![Python 3.10+](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Why streamlib?

Most video tools are **monolithic applications** (Unity, OBS, complex streaming platforms). They're environments, not primitives.

**streamlib provides Unix-pipe-style primitives for video** that AI agents (and humans) can orchestrate:

- 🔧 **Composable** - Small, single-purpose handlers that chain together
- 🚀 **GPU-first** - Automatic GPU optimization, stays on GPU when possible
- 🌐 **Network-transparent** - Operations work seamlessly locally or remotely (future)
- 🎯 **Easy installation** - Simple pip install, no GStreamer required
- 🤖 **AI-friendly** - Designed for AI agent orchestration

## Quick Start

\`\`\`bash
pip install streamlib
\`\`\`

**Your First Pipeline (30 seconds):**

\`\`\`python
import asyncio
from streamlib import StreamRuntime, Stream
from streamlib.handlers import TestPatternHandler, DisplayGPUHandler

async def main():
    runtime = StreamRuntime(fps=30)

    pattern = TestPatternHandler(width=1280, height=720, pattern='smpte_bars')
    display = DisplayGPUHandler(window_name='Hello streamlib', width=1280, height=720)

    # Runtime automatically infers execution context - no dispatchers needed!
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
\`\`\`

**Result:** Window displays SMPTE color bars at 30 FPS!

## Documentation

- **[Quick Start Guide](docs/guides/quickstart.md)** - Build your first pipeline
- **[Composition Guide](docs/guides/composition.md)** - Complex pipelines
- **[API Reference](docs/api/)** - Complete docs
  - [StreamHandler](docs/api/handler.md)
  - [StreamRuntime](docs/api/runtime.md)
  - [Ports](docs/api/ports.md)
  - [Messages](docs/api/messages.md)

## Examples

\`\`\`bash
# Test pattern
python examples/demo_test_pattern.py

# Camera
python examples/demo_camera.py

# Multi-pipeline composition
python examples/demo_multi_pipeline.py --mode pip --camera "Live Camera"
\`\`\`

## License

MIT
