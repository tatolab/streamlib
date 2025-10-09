#!/usr/bin/env python3
"""
Complete demo showing all Phase 3 capabilities.

Pipeline:
  TestPattern (SMPTE) ┐
                       ├─> Compositor ─> Display
  DrawingActor       ┘

Shows:
- Multiple concurrent actors
- Actor registry (URI-based addressing)
- Compositor blending
- Drawing with Skia
- Ring buffer communication
- >> operator connections
"""

import asyncio
from streamlib import (
    TestPatternActor,
    DrawingActor,
    CompositorActor,
    DisplayActor,
    connect_actor,
    ActorRegistry
)


# Drawing code for overlay graphics
OVERLAY_CODE = """
def draw(canvas, ctx):
    import skia
    import numpy as np

    # Animated corner badge
    pulse = 0.8 + 0.2 * np.sin(ctx.time * 2)
    size = 150 * pulse

    paint = skia.Paint()
    paint.setAntiAlias(True)

    # Badge circle (top-right)
    x = ctx.width - 150
    y = 150

    # Outer glow
    paint.setColor(skia.Color(100, 255, 100, 80))
    canvas.drawCircle(x, y, size, paint)

    # Inner circle
    paint.setColor(skia.Color(50, 200, 50, 255))
    canvas.drawCircle(x, y, size * 0.7, paint)

    # Text overlay (bottom)
    paint.setColor(skia.Color(255, 255, 255, 255))
    font = skia.Font(None, 48)

    text = f"streamlib Phase 3 • Frame {ctx.frame_number} • {ctx.time:.1f}s"
    text_width = font.measureText(text)

    # Semi-transparent background for text
    bg_paint = skia.Paint()
    bg_paint.setColor(skia.Color(0, 0, 0, 180))
    canvas.drawRect(
        skia.Rect(0, ctx.height - 100, ctx.width, ctx.height),
        bg_paint
    )

    # Text
    canvas.drawString(
        text,
        (ctx.width - text_width) / 2,
        ctx.height - 40,
        font,
        paint
    )
"""


async def main():
    print("=" * 80)
    print("🚀 streamlib Phase 3 Complete Demo")
    print("=" * 80)
    print()

    print("📋 Pipeline Architecture:")
    print("  ┌─ TestPatternActor (SMPTE bars)")
    print("  │")
    print("  ├─ DrawingActor (animated overlay)")
    print("  │")
    print("  └─> CompositorActor (alpha blending)")
    print("        │")
    print("        └─> DisplayActor (OpenCV window)")
    print()

    # Create actors
    print("🔧 Creating actors...")

    test_pattern = TestPatternActor(
        actor_id='smpte-source',
        width=640,
        height=480,
        pattern='smpte_bars',
        fps=30
    )
    print(f"  ✓ TestPatternActor: SMPTE color bars (640x480 @ 30 FPS)")

    drawing = DrawingActor(
        actor_id='overlay',
        draw_code=OVERLAY_CODE,
        width=640,
        height=480,
        fps=30,
        background_color=(0, 0, 0, 0)  # Transparent background
    )
    print(f"  ✓ DrawingActor: Animated overlay graphics")

    compositor = CompositorActor(
        actor_id='main-compositor',
        width=640,
        height=480,
        fps=30,
        num_inputs=2
    )
    print(f"  ✓ CompositorActor: 2-input alpha blender")

    display = DisplayActor(
        actor_id='output',
        window_name='streamlib Phase 3 - Complete Demo'
    )
    print(f"  ✓ DisplayActor: OpenCV display")

    # Show registry
    print()
    print("📖 Actor Registry:")
    registry = ActorRegistry.get()
    for uri, actor in registry.list_actors().items():
        print(f"  • {uri}")

    # Connect pipeline
    print()
    print("🔗 Building pipeline...")
    test_pattern.outputs['video'] >> compositor.inputs['input0']
    print(f"  ✓ smpte-source >> compositor.input0")

    drawing.outputs['video'] >> compositor.inputs['input1']
    print(f"  ✓ overlay >> compositor.input1")

    compositor.outputs['video'] >> display.inputs['video']
    print(f"  ✓ compositor >> output")

    display.clock = compositor.clock
    print(f"  ✓ Display synced to compositor clock")

    # Alternative: Connect via URIs (network-transparent!)
    print()
    print("🌐 Network-Transparent Addressing:")
    print(f"  • Test pattern: actor://local/TestPatternActor/smpte-source")
    print(f"  • Drawing: actor://local/DrawingActor/overlay")
    print(f"  • Could be: actor://192.168.1.100/CompositorActor/main-compositor")
    print(f"  • Ready for distributed processing!")

    print()
    print("=" * 80)
    print("🎬 Running pipeline...")
    print("=" * 80)
    print()
    print("What you should see:")
    print("  • SMPTE color bars (background layer)")
    print("  • Pulsing green circle (top-right corner)")
    print("  • Live frame counter and time display (bottom)")
    print()
    print("Press Ctrl+C to stop")
    print()

    # Run with detailed status
    try:
        frame_count = 0
        while True:
            await asyncio.sleep(1.0)
            frame_count += 1

            # Get status from all actors
            test_status = test_pattern.get_status()
            draw_status = drawing.get_status()
            comp_status = compositor.get_status()
            disp_status = display.get_status()

            print(f"\r⚡ Frame {frame_count}: "
                  f"SMPTE={test_status['running']} | "
                  f"Draw={draw_status['running']} | "
                  f"Comp={comp_status['fps']:.0f}fps | "
                  f"Display={disp_status['running']}",
                  end='', flush=True)

    except KeyboardInterrupt:
        print()
        print()
        print("🛑 Stopping all actors...")

        await test_pattern.stop()
        await drawing.stop()
        await compositor.stop()
        await display.stop()

        print()
        print("✅ All actors stopped cleanly")
        print()
        print("📊 Final Registry Status:")
        registry = ActorRegistry.get()
        print(f"  • Registered actors: {len(registry.list_actors())}")
        print("  • All actors auto-unregistered on stop ✓")


if __name__ == '__main__':
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\n\n👋 Goodbye!")
