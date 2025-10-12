#!/usr/bin/env python3
"""
Live Camera Demo

Demonstrates live video capture and real-time processing:
- Interactive camera selection menu (with actual device names!)
- CameraHandler: Captures from selected webcam
- BlurFilter: Applies blur effect
- LowerThirdsHandler: Newscast-style graphics overlay with slide-in animation
- DisplayHandler: Shows live video

Pipeline: camera → blur → lower_thirds → display

Press Ctrl+C to quit
"""

import asyncio
import sys
import cv2
from streamlib import (
    StreamRuntime,
    Stream,
)

# Smart GPU handlers - auto-select GPU when available
try:
    from streamlib.handlers import CameraHandlerGPU as CameraHandler
    HAS_GPU_CAMERA = True
    print("✅ GPU camera available (zero-copy AVFoundation → Metal)")
except ImportError:
    from streamlib import CameraHandler
    from streamlib import CPUtoGPUTransferHandler
    HAS_GPU_CAMERA = False
    print("⚠️  Using CPU camera (GPU camera not available)")

try:
    from streamlib.handlers import BlurFilterGPU as BlurFilter
    HAS_GPU_BLUR = True
except ImportError:
    from streamlib import BlurFilter
    HAS_GPU_BLUR = False
    print("⚠️  GPU blur not available")

try:
    from streamlib.handlers import LowerThirdsGPUHandler as LowerThirdsHandler
    HAS_GPU_LOWER_THIRDS = True
except ImportError:
    from streamlib.handlers import LowerThirdsHandler
    HAS_GPU_LOWER_THIRDS = False
    print("⚠️  GPU lower thirds not available")

try:
    from streamlib.handlers import DisplayGPUHandler as DisplayHandler
    HAS_GPU_DISPLAY = True
except ImportError:
    from streamlib import DisplayHandler
    HAS_GPU_DISPLAY = False
    print("⚠️  GPU display not available")

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

    # Try to auto-select FaceTime HD Camera
    camera_idx = None
    camera_name = None
    for idx, name, (width, height) in cameras:
        if "FaceTime HD Camera" in name:
            camera_idx = idx
            camera_name = name
            print(f"\n✓ Auto-selected: [{idx}] {name} - {width}x{height}")
            break

    if camera_idx is None:
        # Fall back to interactive selection
        print("\n⚠️  FaceTime HD Camera not found")
        camera_idx = select_camera()
        # Get the name for the selected camera
        for idx, name, _ in cameras:
            if idx == camera_idx:
                camera_name = name
                break
        print(f"\n💡 TIP: Selected camera: {camera_name}")

    print("\n" + "=" * 70)
    print("LIVE CAMERA DEMO")
    print("=" * 70)
    print("Features:")
    print("  - Live webcam capture")
    print("  - Real-time blur effect")
    print("  - Newscast-style lower thirds overlay (slides in!)")
    print("  - All dispatchers inferred automatically!")
    print()
    print("Controls:")
    print("  Press Ctrl+C to quit")
    print("=" * 70)

    # Create handlers with selected camera
    if HAS_GPU_CAMERA:
        # Zero-copy GPU camera (AVFoundation → Metal)
        # Use device_name for AVFoundation (indices from cv2-enumerate-cameras don't work)
        camera = CameraHandler(
            device_name=camera_name,
            width=1920,
            height=1080,
            fps=30,
            name='camera-gpu'
        )
    else:
        # CPU camera + transfer
        camera = CameraHandler(
            device_id=camera_idx,
            width=1920,
            height=1080,
            fps=30
        )
        cpu_to_gpu = CPUtoGPUTransferHandler()

    # GPU blur
    blur = BlurFilter(
        kernel_size=15,  # Strong blur for dramatic effect
        sigma=8.0 if HAS_GPU_BLUR else 1.0
    )

    # GPU lower thirds overlay (pre-rendered, composited on GPU)
    lower_thirds = LowerThirdsHandler(
        name="YOUR NAME",
        title="STREAMLIB DEMO",
        bar_color=(255, 165, 0),  # Orange RGB
        live_indicator=True,
        channel="45",
        slide_duration=1.5,  # 1.5 second slide-in
        position="bottom-left"
    )

    # GPU display (renders GPU tensor → OpenGL texture)
    display = DisplayHandler(
        name='display-gpu' if HAS_GPU_DISPLAY else 'display',
        window_name='Live Camera - GPU Accelerated',
        width=1920,
        height=1080
    )

    # Create runtime
    runtime = StreamRuntime(fps=30)

    # Add streams - dispatchers inferred automatically!
    print("\n✓ Adding handlers:")
    print(f"  camera: {camera.preferred_dispatcher} ({'GPU' if HAS_GPU_CAMERA else 'CPU'})")
    if not HAS_GPU_CAMERA:
        print(f"  cpu_to_gpu: {cpu_to_gpu.preferred_dispatcher}")
    print(f"  blur: {blur.preferred_dispatcher} ({'GPU' if HAS_GPU_BLUR else 'CPU'})")
    print(f"  lower_thirds: {lower_thirds.preferred_dispatcher} ({'GPU' if HAS_GPU_LOWER_THIRDS else 'CPU'})")
    print(f"  display: {display.preferred_dispatcher if hasattr(display, 'preferred_dispatcher') else 'asyncio'} ({'GPU' if HAS_GPU_DISPLAY else 'CPU'})")

    runtime.add_stream(Stream(camera))
    if not HAS_GPU_CAMERA:
        runtime.add_stream(Stream(cpu_to_gpu))
    runtime.add_stream(Stream(blur))
    runtime.add_stream(Stream(lower_thirds))
    runtime.add_stream(Stream(display))

    # Connect pipeline
    if HAS_GPU_CAMERA:
        # Zero-copy GPU pipeline: camera (GPU) → blur (GPU) → lower_thirds (GPU) → display (GPU)
        runtime.connect(camera.outputs['video'], blur.inputs['video'])
    else:
        # CPU camera with transfer: camera (CPU) → cpu_to_gpu → blur (GPU) → ...
        runtime.connect(camera.outputs['video'], cpu_to_gpu.inputs['video'])
        runtime.connect(cpu_to_gpu.outputs['video'], blur.inputs['video'])

    runtime.connect(blur.outputs['video'], lower_thirds.inputs['video'])
    runtime.connect(lower_thirds.outputs['video'], display.inputs['video'])

    # Print pipeline summary
    print("\n" + "=" * 70)
    if HAS_GPU_CAMERA and HAS_GPU_DISPLAY:
        print("✅ ZERO-COPY GPU PIPELINE:")
        print("  Camera → Blur → Lower Thirds → Display")
        print("  ^^^^^^^^                        ^^^^^^^")
        print("  Metal texture              OpenGL texture")
        print("\n  🚀 No CPU transfers! Everything on GPU!")
    elif HAS_GPU_CAMERA:
        print("✅ GPU PIPELINE (minimal transfers):")
        print("  Camera (GPU) → Blur → Lower Thirds → Display (CPU)")
        print("  ^^^^^^^^^^^^                          ^^^^^^^^^^^^")
        print("  Metal texture                    1x GPU→CPU transfer")
    else:
        print("✅ GPU-ACCELERATED PIPELINE:")
        print("  Camera (CPU) → [Transfer] → Blur → Lower Thirds → Display")
        print("               ^^^^^^^^^^^^                          ")
        print("               1x CPU→GPU transfer")
    print("\nStarting live video...")
    print("=" * 70)

    # Start runtime
    runtime.start()

    print("\n✓ Window should appear now...")
    print("  (If window doesn't appear, check camera permissions)")
    print("\nPress 'q' in terminal to quit")

    try:
        # Run until quit
        await asyncio.sleep(3600)

    except KeyboardInterrupt:
        print("\n\nStopping...")

    await runtime.stop()

    print("\n✓ Demo complete!")
    print("✓ All handlers used their preferred dispatchers")


if __name__ == '__main__':
    asyncio.run(main())
