#!/usr/bin/env python3
"""
Advanced pipeline demo: Complex multi-layer composition

Demonstrates:
- BlurFilter with flexible CPU/GPU capabilities
- CompositorHandler with multiple layers
- DrawingHandler with procedural graphics
- Complex pipeline: TestPattern → Blur → Compositor ← Drawing → Display

Pipeline topology:
    TestPattern (SMPTE) ─→ Blur ──┐
                                    ├─→ Compositor ─→ Display
    Drawing (animated) ────────────┘
"""

import asyncio
import math
from streamlib import (
    StreamRuntime,
    Stream,
    TestPatternHandler,
    BlurFilter,
    CompositorHandler,
    DrawingHandler,
    DrawingContext,
    DisplayHandler,
)


def custom_drawing(ctx: DrawingContext):
    """Custom drawing function: rotating square + text overlay."""
    # Rotating square in center
    angle = ctx.time * 2  # 2 radians/second
    size = 100
    center_x = ctx.width // 2
    center_y = ctx.height // 2

    # Calculate square corners (rotated)
    cos_a = math.cos(angle)
    sin_a = math.sin(angle)

    corners = []
    for dx, dy in [(-size, -size), (size, -size), (size, size), (-size, size)]:
        rx = int(center_x + dx * cos_a - dy * sin_a)
        ry = int(center_y + dx * sin_a + dy * cos_a)
        corners.append((rx, ry))

    # Draw square (4 lines)
    for i in range(4):
        x1, y1 = corners[i]
        x2, y2 = corners[(i + 1) % 4]
        ctx.line(x1, y1, x2, y2, color=(0, 255, 255), thickness=3)  # Cyan

    # Text overlay
    ctx.text("streamlib", 10, 30, color=(255, 255, 255), font_scale=1.0, thickness=2)
    ctx.text(f"Time: {ctx.time:.1f}s", 10, 60, color=(255, 255, 255), font_scale=0.7, thickness=1)
    ctx.text(f"Frame: {ctx.frame_number}", 10, 85, color=(255, 255, 255), font_scale=0.7, thickness=1)

    # Pulsing circle in bottom-right
    radius = int(30 + 20 * math.sin(ctx.time * 3))
    ctx.circle(ctx.width - 60, ctx.height - 60, radius, color=(255, 100, 100), thickness=-1)


async def main():
    """Run advanced composition pipeline."""

    # Create handlers
    pattern = TestPatternHandler(
        width=640,
        height=480,
        pattern='smpte_bars'
    )

    blur = BlurFilter(
        kernel_size=15,  # Heavy blur
        sigma=3.0
    )

    drawing = DrawingHandler(
        width=640,
        height=480,
        draw_func=custom_drawing,
        background_color=(0, 0, 0)  # Transparent black
    )

    compositor = CompositorHandler(
        width=640,
        height=480,
        num_layers=2,
        alphas=[0.7, 0.9]  # Blurred background at 70%, drawing overlay at 90%
    )

    display = DisplayHandler(
        window_name="Advanced Pipeline Demo"
    )

    # Create runtime
    runtime = StreamRuntime(fps=30)

    # Add streams
    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(blur, dispatcher='asyncio'))
    runtime.add_stream(Stream(drawing, dispatcher='asyncio'))
    runtime.add_stream(Stream(compositor, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='asyncio'))

    # Connect pipeline
    # TestPattern → Blur → Compositor layer0
    runtime.connect(pattern.outputs['video'], blur.inputs['video'])
    runtime.connect(blur.outputs['video'], compositor.inputs['layer0'])

    # Drawing → Compositor layer1
    runtime.connect(drawing.outputs['video'], compositor.inputs['layer1'])

    # Compositor → Display
    runtime.connect(compositor.outputs['video'], display.inputs['video'])

    # Start runtime
    runtime.start()

    # Run for 10 seconds
    print("\nRunning advanced pipeline for 10 seconds...")
    print("Pipeline: TestPattern → Blur → Compositor ← Drawing → Display")
    print("Press Ctrl+C to stop early\n")

    try:
        await asyncio.sleep(10)
    except KeyboardInterrupt:
        print("\nStopping...")

    # Stop runtime
    await runtime.stop()

    print("\nDemo complete!")


if __name__ == '__main__':
    asyncio.run(main())
