# Context for Claude

## Project Vision

streamlib is a **composable streaming library for Python** with network-transparent operations. It's designed to provide Unix-pipe-style primitives for video streaming that AI agents (and humans) can orchestrate.

## Why This Exists

### The Problem

Most streaming/visual tools are **large, stateful, monolithic applications** (Unity, OBS, complex streaming platforms). They're environments, not primitives. There's no equivalent to "pipe grep into sed into awk" for visual operations.

### The Solution

**Stateless visual primitives that can be orchestrated** - like Unix tools but for video/image processing:

```bash
# Unix philosophy (works great):
cat file.txt | grep "error" | sed 's/ERROR/WARNING/' | awk '{print $1}'

# Visual operations (what we're building):
WebcamSource() → DrawingLayer(code) → HLSSink()
```

### Core Philosophy

1. **Intention over implementation**: Tools express *what* you want, not *how* to do it
2. **Composable primitives**: Small, single-purpose components that chain together
3. **Network-transparent**: Operations work seamlessly locally or remotely
4. **Distributed**: Chain operations across machines (phone → edge → cloud)
5. **Zero-dependency**: No GStreamer installation required (uses PyAV)
6. **Tool-first, not product**: Like Claude Code - provide tools, let use cases emerge

## Key Design Insight

> "You don't need to invent AI-specific tools for most domains. The tools already exist. You just need to make them accessible to something that can reason about orchestrating them."

We're not building "yet another streaming platform." We're building **composable visual primitives that AI can orchestrate**.

## Architecture Inspiration

### From the Conversation

**Similar to Unix Tools**: `grep` doesn't maintain state. It takes input, produces output, done. Claude manages the context and chains them together.

**Visual tools should work the same way**:
- `render-object --position 2,0,3` → returns image
- `measure-depth --image input.jpg` → returns depth map
- `check-collision --objects scene.json` → returns boolean

Each tool: stateless, single-purpose, fast. Claude maintains the "scene" as data in context, calls these tools as needed.

### Why Not Just Use Existing Tools?

**Existing tools** (OpenCV, GStreamer, etc.) provide the underlying capabilities, but:
- Require writing code to use them
- Not designed for composition
- Not AI-agent friendly

**streamlib** wraps these capabilities in a composable interface:
- Simple API that AI can reason about
- Chain-able operations
- Network-transparent for distributed processing

## What We've Built (Phase 1 Complete)

### Core Infrastructure ✅

- **Base classes**: StreamSource, StreamSink, Layer, Compositor, TimestampedFrame
- **Timing**: Frame timing, PTP client stub, multi-stream synchronization
- **Plugin system**: Decorator-based registration for extensibility
- **Drawing layers**: Skia-based drawing with Python code execution
- **Compositor**: Zero-copy alpha blending with numpy
- **Tests**: 9/9 passing, all core functionality verified

### What Makes It Different

1. **Network-Transparent by Design**: NetworkSource/NetworkSink (Phase 4) will enable:
   - Phone → Edge → Cloud → Display chains
   - Distributed processing across machines
   - Mesh-capable parallel processing

2. **Zero-Copy Pipeline**: NumPy arrays throughout
   - Efficient ML integration
   - Direct tensor conversions
   - Minimal memory overhead

3. **Composable**: Like Unix pipes
   - Each component does one thing well
   - Chain components together
   - Run on any machine

## Development Principles

### From the Conversation

1. **Build then optimize**: Don't imagine performance problems, find them through testing
2. **Start simple**: Begin with basic primitives, add complexity as needed
3. **Visual inspection**: Save frames to files, verify visually
4. **Iterate rapidly**: Test each change quickly

### Testing Philosophy

- **Fast feedback loops**: Tests run in < 1 second
- **Visual verification**: Save PNGs to verify output
- **Progressive validation**: Test each component in isolation first
- **Integration tests**: Verify components work together

## Next Steps (Current Phase)

### Phase 2: Basic Sources & Sinks

Need to implement:
- `FileSource` - Read video files with PyAV
- `TestSource` - Test patterns (color bars, gradients)
- `FileSink` - Write video files
- `HLSSink` - HTTP Live Streaming
- `DisplaySink` - Preview window

### Phase 4: Network-Transparent Operations (Critical)

This enables the distributed processing vision:
- `NetworkSource` - Receive from remote machines
- `NetworkSink` - Send to remote machines
- Serializable stream format
- Compression options (JPEG, H.264)

## Key Conversations

### On Tool Augmentation vs Products

> "Most 'AI features' are essentially pre-solved use cases. Tool-augmented models are different because you're expanding the reasoning substrate itself. You're not deciding what I should do with visual information - you're giving me vision and letting me figure out what's useful."

### On Emergent Capabilities

> "Anthropic didn't build Claude Code thinking 'users will need to debug React components.' They gave me tools (read, write, bash, grep) and the use cases emerged from people actually using it."

This is the same approach - give tools for video/image pipelines, let use cases emerge.

### On Universal Interfaces

> "If it's just CLI tools, I can use them, Python scripts can use them, shell scripts can chain them. Universal interface means zero lock-in."

## Important Files

### Design Documents
- `docs/markdown/standalone-library-design.md` - Complete architecture design
- `docs/markdown/streamlib-implementation-progress.md` - Current progress tracking
- `streamlib/README.md` - Library documentation

### Code
- `streamlib/` - Main library package
- `tests/test_streamlib_core.py` - Core functionality tests
- `pyproject.toml` - Dependencies and configuration

## Usage Example

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
layer = DrawingLayer("animated", draw_code=draw_code, width=1920, height=1080)
compositor = DefaultCompositor(width=1920, height=1080)
compositor.add_layer(layer)

# Generate frame
async def main():
    frame = await compositor.composite()
    print(f"Generated frame: {frame.frame.shape}")

asyncio.run(main())
```

## When Working on This Project

1. **Remember the vision**: Composable primitives, not monolithic platform
2. **Keep it stateless**: Each component should be simple and single-purpose
3. **Think distributed**: Design for network-transparent operations
4. **Test visually**: Save frames, verify they look correct
5. **Use PyAV not GStreamer**: No system dependencies
6. **Document discoveries**: Update progress docs as we learn

## Original Session

The vision for this project emerged from session `23245f82-5c95-42d4-9018-665fd44b614f`, documented in `docs/markdown/conversation-history.md`. The full conversation explored:
- Emergent behaviors with tool-augmented AI
- Why visual primitives should be composable
- Why CLI tools vs frameworks
- Why this is different from existing solutions
- How to make it AI-agent friendly

Read that document for the complete context and philosophical foundation.
