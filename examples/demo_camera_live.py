#!/usr/bin/env python3
"""
Live Camera Demo - PURE METAL PIPELINE

Demonstrates true zero-copy GPU pipeline with compositing:
- Interactive camera selection menu (with actual device names!)
- CameraHandlerMetal: Zero-copy Metal texture output
- BlurFilterMetal: Fast Metal compute shader blur (~4-6ms)
- LowerThirdsMetalHandler: Newscast-style animated overlay (ALL Metal!)
- DisplayMetalHandler: CAMetalLayer rendering with FPS overlay

Pipeline: camera (Metal) → blur (Metal) → lower thirds (Metal) → display (Metal)

100% GPU - NO CPU TRANSFERS! TRUE zero-copy!

Press Ctrl+C to quit
"""

import asyncio
import sys
import cv2
from streamlib import (
    StreamRuntime,
    Stream,
)

# Pure Metal handlers - 100% GPU!
try:
    from streamlib_extras import CameraHandlerMetal
    print("✅ Metal camera available")
except ImportError:
    print("❌ CameraHandlerMetal required (macOS only)")
    sys.exit(1)

try:
    from streamlib_extras import BlurFilterMetal
    print("✅ Metal blur available")
except ImportError:
    print("❌ BlurFilterMetal required (macOS only)")
    sys.exit(1)

try:
    from streamlib_extras import LowerThirdsMetalHandler
    print("✅ Metal lower thirds available")
except ImportError:
    print("❌ LowerThirdsMetalHandler required (macOS only)")
    sys.exit(1)

try:
    from streamlib_extras import DisplayMetalHandler
    print("✅ Metal display available")
except ImportError:
    print("❌ DisplayMetalHandler required (macOS only)")
    sys.exit(1)

# Import camera enumeration library
try:
    from cv2_enumerate_cameras import enumerate_cameras
    HAS_CAMERA_ENUM = True
except ImportError:
    HAS_CAMERA_ENUM = False


def detect_cameras():
    """
    Detect all available cameras with their actual names.

    Returns: List of (index, name, resolution) tuples

    Uses cv2-enumerate-cameras library which properly matches
    camera names to OpenCV device indices.
    """
    print("Detecting cameras...")
    cameras = []

    if HAS_CAMERA_ENUM:
        # Use cv2-enumerate-cameras for accurate camera names
        for camera_info in enumerate_cameras():
            idx = camera_info.index
            name = camera_info.name
            backend = camera_info.backend

            # Open camera to get resolution
            cap = cv2.VideoCapture(idx)
            if cap.isOpened():
                width = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
                height = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))
                cap.release()

                cameras.append((idx, name, (width, height)))
    else:
        # Fallback: basic enumeration without names
        print("⚠️  cv2-enumerate-cameras not available, using basic detection")
        for i in range(10):
            cap = cv2.VideoCapture(i)
            if cap.isOpened():
                width = int(cap.get(cv2.CAP_PROP_FRAME_WIDTH))
                height = int(cap.get(cv2.CAP_PROP_FRAME_HEIGHT))

                # Try to read a frame to verify it works
                ret, _ = cap.read()
                if ret:
                    cameras.append((i, f"Camera {i}", (width, height)))

                cap.release()

    return cameras


def select_camera():
    """
    Interactive camera selection menu.

    Returns: Selected camera index
    """
    print("=" * 70)
    print("CAMERA SELECTION")
    print("=" * 70)

    # Detect cameras
    cameras = detect_cameras()

    if not cameras:
        print("\n❌ ERROR: No cameras detected!")
        print("   Make sure your webcam is connected and accessible.")
        sys.exit(1)

    # Display options
    print(f"\n✓ Found {len(cameras)} camera(s):\n")
    for idx, name, (width, height) in cameras:
        print(f"  [{idx}] {name} - {width}x{height}")

    # Get user selection
    while True:
        print("\n" + "=" * 70)
        try:
            selection = input("Select camera number: ").strip()
            camera_idx = int(selection)

            # Validate selection
            valid_indices = [idx for idx, _, _ in cameras]
            if camera_idx in valid_indices:
                selected = next(cam for cam in cameras if cam[0] == camera_idx)
                print(f"\n✓ Selected: {selected[1]} ({selected[2][0]}x{selected[2][1]})")
                return camera_idx
            else:
                print(f"❌ Invalid selection. Please choose from: {valid_indices}")

        except ValueError:
            print("❌ Invalid input. Please enter a number.")
        except KeyboardInterrupt:
            print("\n\nCancelled.")
            sys.exit(0)


async def main():
    # Camera selection with proper names
    cameras = detect_cameras()

    # Interactive camera selection (always ask user to choose)
    camera_idx = select_camera()

    # Get the name for the selected camera
    camera_name = None
    for idx, name, _ in cameras:
        if idx == camera_idx:
            camera_name = name
            break

    print(f"\n💡 Selected camera: {camera_name}")

    print("\n" + "=" * 70)
    print("LIVE CAMERA DEMO - PURE METAL PIPELINE")
    print("=" * 70)
    print("Features:")
    print("  - Live webcam capture (Metal zero-copy)")
    print("  - Real-time blur effect (Metal compute shader - FAST!)")
    print("  - Newscast-style lower thirds overlay (ALL Metal!)")
    print("  - CAMetalLayer display with FPS overlay")
    print("  - 100% GPU: NO CPU TRANSFERS!")
    print()
    print("Pipeline:")
    print("  Camera (Metal) → Blur (Metal) → Lower Thirds (Metal) → Display (Metal)")
    print()
    print("Controls:")
    print("  Press Ctrl+C to quit")
    print("=" * 70)

    # Create Metal handlers - PURE GPU pipeline!
    camera = CameraHandlerMetal(
        device_name=camera_name,
        width=1920,
        height=1080,
        fps=60,  # 60 FPS for pure Metal!
        name='camera-metal'
    )

    # Metal compute shader blur - FAST!
    blur = BlurFilterMetal(
        kernel_size=15,  # Strong blur for dramatic effect
        sigma=8.0
    )

    # Metal lower thirds overlay with animation - ALL GPU!
    lower_thirds = LowerThirdsMetalHandler(
        name="YOUR NAME",
        title="STREAMLIB DEMO",
        bar_color=(255, 165, 0),  # Orange RGB
        live_indicator=True,
        channel="45",
        slide_duration=1.5,  # 1.5 second slide-in
        position="bottom-left"
    )

    # Metal display with FPS overlay - CAMetalLayer!
    display = DisplayMetalHandler(
        name='display-metal',
        window_name='Live Camera - Pure Metal Pipeline with Lower Thirds',
        width=1920,
        height=1080,
        show_fps=True
    )

    # Create runtime at 60 FPS for pure Metal pipeline!
    runtime = StreamRuntime(fps=60)

    # Add streams - PURE METAL pipeline!
    print("\n✓ Adding handlers:")
    print(f"  camera: {camera.preferred_dispatcher} (Metal)")
    print(f"  blur: {blur.preferred_dispatcher} (Metal compute)")
    print(f"  lower_thirds: {lower_thirds.preferred_dispatcher} (Metal compositing)")
    print(f"  display: {display.preferred_dispatcher} (CAMetalLayer)")

    runtime.add_stream(Stream(camera))
    runtime.add_stream(Stream(blur))
    runtime.add_stream(Stream(lower_thirds))
    runtime.add_stream(Stream(display))

    # Connect pure Metal pipeline - NO transfers!
    runtime.connect(camera.outputs['video'], blur.inputs['video'])
    runtime.connect(blur.outputs['video'], lower_thirds.inputs['video'])
    runtime.connect(lower_thirds.outputs['video'], display.inputs['video'])

    # Print pipeline summary
    print("\n" + "=" * 70)
    print("✅ PURE METAL PIPELINE @ 60 FPS:")
    print("  Camera → Blur → Lower Thirds → Display")
    print("  ^^^^^^   ^^^^   ^^^^^^^^^^^^   ^^^^^^^")
    print("  Metal    Metal   Metal comp.   CAMetal")
    print("  ~1ms     ~4-6ms  ~5ms           ~0.5ms")
    print("\n  🚀🚀🚀 100% GPU! NO CPU TRANSFERS!")
    print("\nStarting live video...")
    print("=" * 70)

    # Start runtime
    await runtime.start()

    print("\n✓ Window should appear now with FPS overlay!")
    print("  (If window doesn't appear, check camera permissions)")
    print("\n📊 Expected performance at 60 FPS:")
    print("  - Camera: ~1ms (Metal YUV→RGB)")
    print("  - Blur: ~4-6ms (Metal compute shader)")
    print("  - Lower thirds: ~5ms (Metal compositing)")
    print("  - Display: ~0.5ms (CAMetalLayer)")
    print("  - Total: ~11ms → 60 FPS achievable! 🚀🚀🚀")
    print("\n  100% GPU pipeline - TRUE zero-copy!")
    print("  Watch for lower thirds sliding in over 1.5 seconds!")
    print("\nPress Ctrl+C to quit")

    try:
        # Run until quit
        await asyncio.sleep(3600)

    except KeyboardInterrupt:
        print("\n\nStopping...")

    await runtime.stop()

    print("\n✓ Demo complete!")
    print("✓ Pure Metal pipeline with compositing!")
    print("✓ 100% GPU - TRUE zero-copy with lower thirds! 🚀🚀🚀")


if __name__ == '__main__':
    asyncio.run(main())
