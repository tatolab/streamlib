# streamlib

A composable streaming library for Python with network-transparent operations.

## Project Status

**Phase 1: Core Infrastructure** - ✅ Complete (9/9 tests passing)
**Next**: Phase 2 - Basic Sources & Sinks

## What Is This?

streamlib provides Unix-pipe-style composable primitives for video streaming:

```python
# Like Unix pipes:
# cat file.txt | grep "error" | sed 's/ERROR/WARNING/'

# But for video:
WebcamSource() → DrawingLayer(code) → HLSSink()
```

### Key Features

- **Composable**: Small, single-purpose components that chain together
- **Network-transparent**: Operations work locally or remotely (Phase 4)
- **Distributed**: Chain operations across machines (phone → edge → cloud)
- **Zero-dependency**: Uses PyAV (no GStreamer installation required)
- **ML-ready**: Zero-copy numpy pipeline for efficient ML integration
- **Plugin system**: Easy to extend with custom sources, sinks, and layers

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

```python
import asyncio
from streamlib import DrawingLayer, DefaultCompositor

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

# Create layer and compositor
layer = DrawingLayer("animated", draw_code=draw_code)
compositor = DefaultCompositor(width=1920, height=1080)
compositor.add_layer(layer)

# Generate frame
async def main():
    frame = await compositor.composite()
    print(f"Generated frame: {frame.frame.shape}")

asyncio.run(main())
```

## Development

### Running Tests

```bash
# All tests
pytest tests/ -v

# Specific test
pytest tests/test_streamlib_core.py -v

# With coverage
pytest tests/ --cov=streamlib
```

### Code Style

```bash
# Format
black streamlib/

# Lint
ruff check streamlib/

# Type check
mypy streamlib/
```

## Architecture

### Core Components

- **Sources**: Produce frames (webcam, file, screen, network, generated)
- **Sinks**: Consume frames (HLS, file, display, network)
- **Layers**: Process/generate visual content (video, drawing, ML)
- **Compositor**: Combines layers using alpha blending

### Design Principles

1. **Network-Transparent**: Operations work locally or across machines
2. **Zero-Copy**: NumPy arrays throughout for efficiency
3. **Async-First**: Built with asyncio for streaming pipelines
4. **Plugin System**: Easy to extend with custom components

## Project Structure

```
streamlib/              # Main library
├── base.py            # Abstract base classes
├── timing.py          # Timing and synchronization
├── plugins.py         # Plugin registration
├── drawing.py         # Drawing layers
├── compositor.py      # Video compositor
├── sources/           # Source implementations (Phase 2+)
├── sinks/             # Sink implementations (Phase 2+)
└── layers/            # Layer implementations (Phase 2+)

tests/                  # Test suite
docs/markdown/          # Design documents
```

## Roadmap

### Phase 1: Core Infrastructure ✅
- Abstract base classes
- Timing infrastructure
- Plugin system
- Drawing layers with Skia
- Compositor with zero-copy blending

### Phase 2: Basic Sources & Sinks (Next)
- FileSource, TestSource
- FileSink, HLSSink, DisplaySink

### Phase 3: Hardware I/O
- WebcamSource, ScreenCaptureSource
- Audio support

### Phase 4: Network-Transparent Operations (Critical)
- NetworkSource, NetworkSink
- Serializable stream format
- Distributed processing

### Phase 5-7: Advanced Features
- Full PTP time sync
- ML layers with GPU acceleration
- 3D tracking examples

## Vision

Unlike traditional streaming platforms, streamlib is designed as **composable primitives for AI orchestration**:

- **Not a product**: A tool that enables emergent use cases
- **Network-transparent**: Operations work across machines seamlessly
- **Unix philosophy**: Do one thing well, compose easily
- **AI-friendly**: Simple enough for agents to reason about

This approach follows the same philosophy as Claude Code - provide powerful primitives, let capabilities emerge through use.

## Documentation

- [CLAUDE.md](CLAUDE.md) - Context for AI assistants
- [Design Document](docs/markdown/standalone-library-design.md) - Complete architecture
- [Implementation Progress](docs/markdown/streamlib-implementation-progress.md) - Current status
- [Library README](streamlib/README.md) - Detailed API docs
- [Conversation History](docs/markdown/conversation-history.md) - Original vision

## Why PyAV?

- No system dependencies (unlike GStreamer)
- Pure Python installation via pip
- Well-maintained FFmpeg bindings
- Used successfully by [fastrtc](https://github.com/gradio-app/fastrtc)

## Inspiration

- [fastrtc](https://github.com/gradio-app/fastrtc) - WebRTC streaming architecture
- Unix philosophy - Composable tools
- Claude Code - Tool-augmented AI capabilities

## License

MIT

## Contributing

This is an early-stage project exploring composable visual primitives. Contributions welcome as we discover what works.

## Original Vision

This project emerged from exploring how AI agents could work with visual/spatial data through composable primitives, rather than monolithic applications. See [conversation-history.md](docs/markdown/conversation-history.md) for the complete philosophical foundation.
