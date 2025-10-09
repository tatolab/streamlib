#!/usr/bin/env python3
"""
Compositor demo - combines multiple video sources.

Shows:
- 2 test pattern sources (SMPTE bars + gradient)
- Compositor blending them
- Display output
"""

import asyncio
from streamlib import TestPatternActor, CompositorActor, DisplayActor


async def main():
    print("=" * 60)
    print("🎬 Compositor Demo")
    print("=" * 60)
    print()

    # Create video sources
    print("Creating video sources...")

    source1 = TestPatternActor(
        actor_id='source1',
        width=640,
        height=480,
        pattern='smpte_bars',
        fps=30
    )
    print(f"  ✓ Source 1: SMPTE bars (640x480)")

    source2 = TestPatternActor(
        actor_id='source2',
        width=640,
        height=480,
        pattern='gradient',
        fps=30
    )
    print(f"  ✓ Source 2: Gradient (640x480)")

    # Create compositor
    print()
    print("Creating compositor...")
    compositor = CompositorActor(
        actor_id='compositor',
        width=640,
        height=480,
        fps=30,
        num_inputs=2
    )
    print(f"  ✓ Compositor (640x480, 2 inputs)")

    # Create display
    display = DisplayActor(
        actor_id='display',
        window_name='Compositor Demo - Blended Output'
    )
    print(f"  ✓ Display")

    # Connect pipeline
    print()
    print("Building pipeline...")
    source1.outputs['video'] >> compositor.inputs['input0']
    source2.outputs['video'] >> compositor.inputs['input1']
    compositor.outputs['video'] >> display.inputs['video']
    display.clock = compositor.clock

    print(f"  ✓ source1 >> compositor.input0")
    print(f"  ✓ source2 >> compositor.input1")
    print(f"  ✓ compositor >> display")

    print()
    print("🎥 Running compositor pipeline...")
    print("   Window shows both sources alpha-blended together")
    print("   Press Ctrl+C to stop")
    print()

    # Run with status updates
    try:
        while True:
            await asyncio.sleep(1.0)

            status = compositor.get_status()
            print(f"\r⚡ Compositor: {status['fps']:.1f} FPS, "
                  f"Inputs: {status['inputs']}, Running: {status['running']}",
                  end='', flush=True)

    except KeyboardInterrupt:
        print()
        print()
        print("Stopping...")

        await source1.stop()
        await source2.stop()
        await compositor.stop()
        await display.stop()

        print("  ✓ Stopped all actors")


if __name__ == '__main__':
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\n👋 Exiting...")
