#!/usr/bin/env python3
"""
Quick compositor demo only.
"""

import asyncio
from streamlib import (
    Stream,
    DisplaySink,
    DrawingLayer,
    DefaultCompositor,
)


async def main():
    """Show compositor with drawing layers."""
    print("Compositor Demo with Drawing Layers")
    print("Press 'q' to quit\n")

    # Create compositor
    compositor = DefaultCompositor(width=1280, height=720)

    # Add background gradient layer
    gradient_code = """
def draw(canvas, ctx):
    import skia
    import numpy as np

    # Gradient background
    gradient = skia.GradientShader.MakeLinear(
        points=[(0, 0), (ctx.width, ctx.height)],
        colors=[0xFF1a1a2e, 0xFF16213e, 0xFF0f3460]
    )
    paint = skia.Paint(Shader=gradient)
    canvas.drawRect(skia.Rect(0, 0, ctx.width, ctx.height), paint)
"""

    bg_layer = DrawingLayer('background', draw_code=gradient_code, z_index=0)
    compositor.add_layer(bg_layer)

    # Add animated circle layer
    circle_code = """
def draw(canvas, ctx):
    import skia
    import numpy as np

    # Animated circle
    paint = skia.Paint(AntiAlias=True)

    # Pulsing radius
    base_radius = 80
    radius = base_radius + 40 * np.sin(ctx.time * 3)

    # Moving position
    x = ctx.width / 2 + 200 * np.sin(ctx.time * 1.5)
    y = ctx.height / 2 + 150 * np.cos(ctx.time * 2)

    # Gradient fill
    gradient = skia.GradientShader.MakeRadial(
        center=(x, y),
        radius=radius,
        colors=[0xFFe94560, 0xFF533483]
    )
    paint.setShader(gradient)

    canvas.drawCircle(x, y, radius, paint)
"""

    circle_layer = DrawingLayer('circle', draw_code=circle_code, z_index=1)
    compositor.add_layer(circle_layer)

    # Add text overlay
    text_code = """
def draw(canvas, ctx):
    import skia

    paint = skia.Paint(AntiAlias=True)
    paint.setColor(skia.Color(255, 255, 255, 200))

    font = skia.Font(skia.Typeface('Arial'), 64)

    text = "streamlib"
    canvas.drawString(text, 50, 100, font, paint)

    # Subtitle
    font_small = skia.Font(skia.Typeface('Arial'), 24)
    paint.setColor(skia.Color(255, 255, 255, 150))
    canvas.drawString("Composable streaming for Python", 50, 140, font_small, paint)
"""

    text_layer = DrawingLayer('text', draw_code=text_code, z_index=2)
    compositor.add_layer(text_layer)

    # Create stream with compositor and display
    stream = Stream(
        compositor=compositor,
        sink=DisplaySink(window_name='Compositor Demo', show_fps=True),
        fps=60
    )

    # Run for 10 seconds
    await stream.run_for_duration(10.0)


if __name__ == '__main__':
    asyncio.run(main())
