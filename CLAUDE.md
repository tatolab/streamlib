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
runtime.connect(camera.outputs['video'], compositor.inputs['video'])
runtime.connect(compositor.outputs['video'], display.inputs['video'])
```

### Core Philosophy

1. **Intention over implementation**: Tools express *what* you want, not *how* to do it
2. **Composable primitives**: Small, single-purpose components that chain together
3. **Network-transparent**: Operations work seamlessly locally or remotely
4. **Distributed**: Chain operations across machines (phone → edge → cloud)
5. **Zero-dependency core**: No GStreamer installation required
6. **Tool-first, not product**: Like Claude Code - provide tools, let use cases emerge
7. **🔥 GPU-first, opinionated optimization**: Automatically stay on GPU, never bounce to CPU unnecessarily

### The streamlib Superpower: Automatic GPU Optimization

**This is what differentiates streamlib from GStreamer:**

GStreamer pipelines constantly bounce between CPU/GPU because they're not opinionated.
streamlib **automatically chooses the fastest path** and **stays on GPU as long as possible**.

```python
# User writes simple code
runtime.connect(camera.outputs['video'], display.inputs['video'])

# streamlib automatically:
# 1. Detects camera outputs GPU
# 2. Negotiates GPU path
# 3. Selects optimal display backend (Metal/CUDA/OpenGL)
# 4. Zero-copy GPU → screen
#
# Result: NO CPU TRANSFERS!
```

## Architecture

### StreamHandler + Runtime

The **handler-based architecture** is inspired by Cloudflare Durable Objects and GStreamer's capability negotiation:

**StreamHandler** - Pure processing logic (inert until runtime activates)
```python
class BlurFilter(StreamHandler):
    def __init__(self):
        super().__init__()
        # GPU-first by default - no capabilities needed!
        self.inputs['video'] = VideoInput('video')
        self.outputs['video'] = VideoOutput('video')

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()  # Zero-copy
        if frame:
            result = cv2.GaussianBlur(frame.data, (5, 5), 0)
            self.outputs['video'].write(VideoFrame(result, tick.timestamp))
```

**StreamRuntime** - Lifecycle manager
```python
runtime = StreamRuntime(fps=30)
# Runtime automatically infers execution context - no dispatchers needed!
runtime.add_stream(Stream(camera_handler))
runtime.add_stream(Stream(blur_handler))
runtime.connect(camera_handler.outputs['video'], blur_handler.inputs['video'])
await runtime.start()
```

**Key Concepts:**
- **GPU-first by default**: All ports default to GPU, runtime handles everything automatically
- **Automatic execution inference**: Runtime selects optimal execution context based on operations
- **Automatic transfers**: Runtime inserts GPU↔CPU transfers only when necessary
- **Clock-driven**: Runtime clock ticks drive all handlers
- **Zero-copy**: Ring buffers hold references, not data copies
- **Simple API**: No explicit capabilities or dispatchers - runtime handles it all

## When Working on This Project

### Remember the Vision
1. **Composable primitives**, not monolithic platform
2. **Keep handlers stateless**: Each component simple and single-purpose
3. **Think distributed**: Design for network-transparent operations
4. **Test visually**: Save frames, verify they look correct
5. **Use PyAV not GStreamer**: No system dependencies in core
6. **Document discoveries**: Update progress docs as we learn

### Development Principles

**From the Conversation:**
1. **Build then optimize**: Don't imagine performance problems, find them through testing
2. **Start simple**: Begin with basic primitives, add complexity as needed
3. **Visual inspection**: Save frames to files, verify visually
4. **Iterate rapidly**: Test each change quickly

### Testing Philosophy
- **Fast feedback loops**: Tests run in < 1 second
- **Visual verification**: Save PNGs to verify output
- **Progressive validation**: Test each component in isolation first
- **Integration tests**: Verify components work together

## Important Files

### Design Documents
- `docs/internal/architecture.md` - Complete architecture (authoritative)
- `docs/project.md` - Implementation task list and timeline
- `docs/guides/gpu-optimization.md` - GPU-first optimization guide

### Implementation
- `packages/streamlib/src/streamlib/` - StreamHandler implementation
- `tests/` - Test suite
- `examples/` - Example pipelines

## Commit Workflow

**IMPORTANT: DO NOT AUTO-COMMIT CHANGES**

The user wants to personally determine when changes are committed. When working on this project:

1. ❌ **Never** automatically commit changes after completing work
2. ✅ **Always** present changes to the user for review
3. ✅ **Wait** for explicit user instruction to commit
4. ✅ **Let the user** decide when to commit and what commit message to use

This applies to all changes: code, documentation, tests, configuration, etc.

## Original Session

The vision for this project emerged from session `23245f82-5c95-42d4-9018-665fd44b614f`, documented in `docs/markdown/conversation-history.md`.
