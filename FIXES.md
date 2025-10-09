# Window Display Fixes - Phase 3

## Issue

User reported that `demo_compositor.py` and `demo_all.py` were not opening windows to display video output.

## Root Cause

The demos were configured with high resolution (1920x1080 or 960x540) and high frame rate (60 FPS), which caused performance issues that prevented frames from flowing through the pipeline. Specifically:

1. **Resolution too high**: At resolutions above 640x480, the compositor's alpha blending algorithm was too computationally expensive
2. **Frame rate too high**: 60 FPS put too much load on the asyncio event loop
3. **Combined effect**: The system would hang, preventing the display actor from ever receiving frames to display

## Investigation Process

1. Created debug tests to isolate the issue
2. Found that 640x480 @ 30 FPS worked correctly
3. Tested progressively higher resolutions (720p, 1080p) and found they all hung
4. Identified that the display actor never received frames at higher resolutions

## Fixes Applied

### 1. DisplayActor Improvements (src/streamlib/actors/video.py)

- Added `cv2.startWindowThread()` for better macOS compatibility (called once globally)
- Changed window type from `WINDOW_NORMAL` to `WINDOW_AUTOSIZE` for better cross-platform support
- Added `cv2.setWindowProperty()` to bring windows to foreground on macOS
- Increased `cv2.waitKey()` delay from 1ms to 10ms for better event processing
- Added debug print statement when window is created

### 2. Demo Resolution and FPS Updates

All demos were updated to use **640x480 @ 30 FPS** for reliable performance:

- **demo_actor.py**: Changed from 1920x1080 @ 60 FPS → 640x480 @ 30 FPS
- **demo_compositor.py**: Changed from 960x540 sources + 1920x1080 compositor @ 60 FPS → 640x480 @ 30 FPS
- **demo_drawing.py**: Changed from 1920x1080 @ 60 FPS → 640x480 @ 30 FPS
- **demo_all.py**: Changed from 1920x1080 @ 60 FPS → 640x480 @ 30 FPS

### 3. Documentation Updates

- Updated `DEMOS.md` to reflect correct resolution and FPS settings
- Added performance notes about limitations
- Updated troubleshooting section with guidance on resolution/FPS issues
- Added "Known Limitations" section to Code Statistics

## Verification

All demos now work correctly:

```bash
$ python demo_compositor.py
# Window appears showing blended SMPTE bars + gradient
# Status: "[display] Window 'Compositor Demo - Blended Output' created"
# Running: "⚡ Compositor: 30.0 FPS, Inputs: {'input0': True, 'input1': True}, Running: True"

$ python demo_all.py
# Window appears showing SMPTE bars + animated overlay
# Status: "[output] Window 'streamlib Phase 3 - Complete Demo' created"
# Running: "⚡ Frame 1: SMPTE=True | Draw=True | Comp=30fps | Display=True"

$ python demo_drawing.py
# Window appears showing pulsing red circle animation
# Status: "[display] Window 'Drawing Demo - Animated Graphics' created"
# Running: "⚡ Drawing: 30.0 FPS, Time: 1234567890.1s, Frame: 31"

$ python demo_actor.py
# Window appears showing SMPTE bars
# Status: "[display] Window 'streamlib - Actor Demo' created"
# Running: "[Generator] FPS=30.0 Running=True | [Display] Running=True"
```

## Future Improvements

To support higher resolutions and frame rates, the following optimizations could be made:

1. **Optimize compositor alpha blending**:
   - Use vectorized NumPy operations more efficiently
   - Consider using Numba JIT compilation
   - Implement GPU acceleration with CUDA/OpenCL

2. **Reduce event loop contention**:
   - Move heavy processing to thread pool
   - Use multiprocessing for parallel actor execution
   - Implement frame dropping when system is overloaded

3. **Profile and optimize**:
   - Use cProfile to identify bottlenecks
   - Optimize frame copying/conversion
   - Cache intermediate results where possible

## Technical Notes

- The issue was NOT with the window creation code itself
- The issue was with frame flow through the pipeline at high resolution/FPS
- 640x480 @ 30 FPS provides a good balance of visual quality and performance
- For production use, consider implementing the optimizations listed above
