#!/usr/bin/env python3
"""
Quick demo of streamlib capabilities.

Shows what we can do with Phase 1, 2, and 3 components.
"""

import asyncio
from streamlib import (
    Stream,
    TestSource,
    DisplaySink,
    DrawingLayer,
    DefaultCompositor,
)


async def demo_test_patterns():
    """Demo 1: Show test patterns."""
    print("Demo 1: Test Patterns")
    print("Press 'q' to quit, 'p' to pause, 'f' for fullscreen\n")

    # Create stream with test source and display
    stream = Stream(
        source=TestSource(pattern='smpte_bars', width=1280, height=720, fps=30),
        sink=DisplaySink(window_name='Test Pattern - SMPTE Bars', show_fps=True),
        fps=30
    )

    # Run for 3 seconds
    await stream.run_for_duration(3.0)


async def demo_animated_pattern():
    """Demo 2: Show animated moving box."""
    print("\nDemo 2: Animated Pattern")
    print("Press 'q' to quit\n")

    # Create stream with animated test pattern
    stream = Stream(
        source=TestSource(pattern='moving_box', width=1280, height=720, fps=60),
        sink=DisplaySink(window_name='Animated Pattern', show_fps=True),
        fps=60
    )

    # Run for 5 seconds
    await stream.run_for_duration(5.0)


async def demo_compositor():
    """Demo 3: Show compositor with drawing layers."""
    print("\nDemo 3: Compositor with Drawing Layers")
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


async def main():
    """Run all demos."""
    print("=" * 60)
    print("streamlib Visual Demo")
    print("=" * 60)
    print()

    # Demo 1: Test patterns
    await demo_test_patterns()

    # Demo 2: Animated pattern
    await demo_animated_pattern()

    # Demo 3: Compositor
    await demo_compositor()

    print("\nDemo complete!")


if __name__ == '__main__':
    asyncio.run(main())
