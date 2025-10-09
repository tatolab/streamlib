#!/usr/bin/env python3
"""
Drawing demo - programmatic graphics with Skia.

Shows:
- DrawingActor with animated Python code
- Pulsing circle animation
- Live clock display
"""

import asyncio
from streamlib import DrawingActor, DisplayActor


# Drawing code with animation
DRAW_CODE = """
def draw(canvas, ctx):
    import skia
    import numpy as np
    import math

    # Background
    paint = skia.Paint()
    paint.setAntiAlias(True)

    # Animated pulsing circle
    pulse = 0.8 + 0.2 * np.sin(ctx.time * 3)
    radius = 100 + 80 * pulse

    # Outer glow
    for i in range(10):
        alpha = int(30 * (1 - i/10) * pulse)
        paint.setColor(skia.Color(255, 100, 100, alpha))
        canvas.drawCircle(ctx.width / 2, ctx.height / 2, radius + i*10, paint)

    # Main circle
    paint.setColor(skia.Color(255, 50, 50, 255))
    canvas.drawCircle(ctx.width / 2, ctx.height / 2, radius, paint)

    # Inner highlight
    paint.setColor(skia.Color(255, 200, 200, 180))
    canvas.drawCircle(ctx.width / 2 - 30, ctx.height / 2 - 30, radius * 0.4, paint)

    # Draw clock text
    paint.setColor(skia.Color(255, 255, 255, 255))
    font_title = skia.Font(None, 72)
    font_sub = skia.Font(None, 36)

    # Title
    title = "streamlib Drawing Demo"
    title_width = font_title.measureText(title)
    canvas.drawString(
        title,
        (ctx.width - title_width) / 2,
        100,
        font_title,
        paint
    )

    # Time display
    time_text = f"Time: {ctx.time:.2f}s"
    time_width = font_sub.measureText(time_text)
    canvas.drawString(
        time_text,
        (ctx.width - time_width) / 2,
        ctx.height - 100,
        font_sub,
        paint
    )

    # Frame counter
    frame_text = f"Frame: {ctx.frame_number}"
    frame_width = font_sub.measureText(frame_text)
    canvas.drawString(
        frame_text,
        (ctx.width - frame_width) / 2,
        ctx.height - 50,
        font_sub,
        paint
    )

    # Rotating line
    angle = ctx.time * 2
    line_length = 150
    x1 = ctx.width / 2
    y1 = ctx.height / 2
    x2 = x1 + math.cos(angle) * line_length
    y2 = y1 + math.sin(angle) * line_length

    paint.setStrokeWidth(5)
    paint.setColor(skia.Color(100, 200, 255, 255))
    canvas.drawLine(x1, y1, x2, y2, paint)
"""


async def main():
    print("=" * 60)
    print("🎨 Drawing Demo")
    print("=" * 60)
    print()

    print("Creating actors...")

    # Create drawing actor with animated code
    drawing = DrawingActor(
        actor_id='drawing',
        draw_code=DRAW_CODE,
        width=640,
        height=480,
        fps=30,
        background_color=(20, 20, 40, 255)
    )
    print(f"  ✓ DrawingActor with animated Skia code")

    # Create display
    display = DisplayActor(
        actor_id='display',
        window_name='Drawing Demo - Animated Graphics'
    )
    print(f"  ✓ DisplayActor")

    # Connect
    print()
    print("Building pipeline...")
    drawing.outputs['video'] >> display.inputs['video']
    display.clock = drawing.clock
    print(f"  ✓ drawing >> display")

    print()
    print("🎥 Running animation...")
    print("   - Pulsing red circle")
    print("   - Rotating line")
    print("   - Live time & frame counter")
    print("   Press Ctrl+C to stop")
    print()

    # Run with status
    try:
        while True:
            await asyncio.sleep(1.0)

            status = drawing.get_status()
            ctx = drawing.get_context()

            print(f"\r⚡ Drawing: {status['fps']:.1f} FPS, "
                  f"Time: {ctx.time:.1f}s, "
                  f"Frame: {ctx.frame_number}",
                  end='', flush=True)

    except KeyboardInterrupt:
        print()
        print()
        print("Stopping...")

        await drawing.stop()
        await display.stop()

        print("  ✓ Stopped")


if __name__ == '__main__':
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\n👋 Exiting...")
