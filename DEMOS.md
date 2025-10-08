# streamlib Phase 3 Demos

Collection of demos showcasing the Phase 3 actor-based architecture.

## Quick Start

All demos require Python 3.9+ with dependencies:
```bash
pip install -e .
```

---

## Demos

### 1. **demo_actor.py** - Basic Pipeline ⭐ Start Here

**What it shows:**
- TestPatternActor generating SMPTE color bars
- DisplayActor showing video in OpenCV window
- Basic actor connection with >> operator

**Run it:**
```bash
python demo_actor.py
```

**What you'll see:**
- 1920x1080 window with SMPTE color bars (7 vertical stripes)
- 60 FPS smooth playback
- Status updates in terminal

**Architecture:**
```
TestPatternActor (SMPTE bars, 60 FPS)
    >> DisplayActor (OpenCV window)
```

**Key concepts:**
- Actor auto-start on creation
- Ring buffer communication
- >> operator for connections
- Clock synchronization

---

### 2. **demo_compositor.py** - Multi-Source Compositing

**What it shows:**
- Multiple TestPatternActor sources (SMPTE + gradient)
- CompositorActor alpha-blending them
- Multi-input pipeline

**Run it:**
```bash
python demo_compositor.py
```

**What you'll see:**
- Two video sources blended together
- SMPTE bars + gradient pattern
- Real-time alpha compositing at 60 FPS

**Architecture:**
```
TestPatternActor (SMPTE) ┐
                          ├─> CompositorActor ─> DisplayActor
TestPatternActor (gradient) ┘
```

**Key concepts:**
- Multiple concurrent actors
- CompositorActor with N inputs
- Alpha blending with zero-copy numpy
- Independent clocks per actor

---

### 3. **demo_drawing.py** - Programmatic Graphics

**What it shows:**
- DrawingActor with animated Skia code
- Python code execution for graphics
- Real-time animation

**Run it:**
```bash
python demo_drawing.py
```

**What you'll see:**
- Pulsing red circle with glow effect
- Rotating line animation
- Live time and frame counter
- 60 FPS smooth animation

**Architecture:**
```
DrawingActor (Python/Skia code)
    >> DisplayActor
```

**Key concepts:**
- Python code execution (draw function)
- DrawingContext (time, frame_number, custom vars)
- Skia-based rendering
- Real-time code updates (via set_draw_code)

---

### 4. **demo_all.py** - Complete Pipeline 🌟 Full Demo

**What it shows:**
- Everything working together
- Multi-actor pipeline
- Registry with URI addressing
- Network-transparent design

**Run it:**
```bash
python demo_all.py
```

**What you'll see:**
- SMPTE bars (background)
- Pulsing green badge (top-right corner)
- Live frame counter overlay (bottom)
- All actors running concurrently

**Architecture:**
```
TestPatternActor (SMPTE) ┐
                          ├─> CompositorActor ─> DisplayActor
DrawingActor (overlay)   ┘
```

**Key concepts:**
- Actor registry (URI-based addressing)
- Multi-layer composition
- Concurrent actor execution
- Auto-registration/unregistration
- Network-transparent URIs (ready for distributed)

---

## What's Demonstrated

### ✅ Core Features

1. **Actor Model**
   - Independent, concurrent actors
   - Tick-based processing
   - Auto-start lifecycle
   - Clean stop/cleanup

2. **Ring Buffer Communication**
   - Latest-read semantics (skip old data)
   - Zero-copy transfers
   - No queueing, no backpressure
   - Thread-safe

3. **Connection System**
   - >> operator for intuitive connections
   - StreamInput/StreamOutput ports
   - Automatic buffer creation

4. **Clock Synchronization**
   - Per-actor clocks (Software, PTP, Genlock)
   - Clock inheritance (display syncs to source)
   - 60 FPS timing

5. **Actor Registry**
   - URI-based addressing
   - Auto-registration on creation
   - Auto-unregister on stop
   - Network-transparent design

6. **Video Actors**
   - TestPatternActor (SMPTE, gradient, etc.)
   - DisplayActor (OpenCV window)
   - CompositorActor (alpha blending)
   - DrawingActor (Skia graphics)

---

## Architecture Highlights

### Network-Transparent by Design

All actors have URIs like `actor://host/ActorClass/instance-id`:

```python
# Local actors (current machine)
actor://local/TestPatternActor/test1
actor://local/CompositorActor/main

# Remote actors (future Phase 4)
actor://192.168.1.100/DisplayActor/monitor1
actor://edge-server/CompositorActor/main
```

Connect actors across machines (Phase 4):
```python
# Local source -> Remote compositor -> Local display
source = connect_actor('actor://local/TestPatternActor/test1')
compositor = connect_actor('actor://edge-server/CompositorActor/main')
display = connect_actor('actor://local/DisplayActor/output')

source.outputs['video'] >> compositor.inputs['input0']
compositor.outputs['video'] >> display.inputs['video']
```

### SMPTE ST 2110 Aligned

- Ring buffers (3 slots) match broadcast practice
- Latest-read semantics (no queueing)
- Port allocator for RTP/UDP (20000-30000)
- PTP clock support (stub, ready for Phase 4)
- Genlock support (stub, ready for Phase 4)

---

## Performance

**Target:** 1080p60 < 16ms per frame, jitter < 1ms

**Current Performance:**
- All demos run at 60 FPS on modern hardware
- Compositor handles 2-4 inputs at 60 FPS
- Drawing actor with complex Skia code: 60 FPS
- Memory stable (ring buffers prevent leaks)

**Optimizations:**
- Zero-copy ring buffers
- Optimized alpha blending (uint16 arithmetic)
- Concurrent actor execution
- Efficient dispatchers (Asyncio, ThreadPool)

---

## Stopping Demos

All demos respond to **Ctrl+C**:
- Actors stop cleanly
- OpenCV windows close automatically
- Registry cleanup happens automatically

---

## Next Steps

1. **Try the demos** - Start with `demo_actor.py`, progress to `demo_all.py`
2. **Modify drawing code** - Edit DRAW_CODE in `demo_drawing.py`
3. **Add more sources** - Modify `demo_compositor.py` to add 3+ inputs
4. **Explore registry** - Check `ActorRegistry.get().list_actors()`

---

## Troubleshooting

**Window doesn't appear:**
- Check if OpenCV is installed: `pip install opencv-python`
- macOS: May need to grant Terminal screen recording permission

**Low FPS:**
- Reduce resolution: `width=1280, height=720`
- Reduce FPS: `fps=30`
- Close other applications

**Import errors:**
- Install package: `pip install -e .`
- Check Python version: `python --version` (need 3.9+)

---

## Code Statistics

- **Tests:** 55/55 passing ✅
- **Actors:** 4 types (TestPattern, Display, Compositor, Drawing)
- **Lines of Code:** ~3,700 (Phase 3 only)
- **Performance:** 60 FPS @ 1080p

**Phase 3 Status:** Substantially Complete ✅
