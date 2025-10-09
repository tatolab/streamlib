#!/usr/bin/env python3
"""
Basic demo of actor-based streamlib architecture.

Creates a test pattern generator and displays it in a window.

Usage:
    python demo_actor.py
"""

import asyncio
import sys
from pathlib import Path

# Add parent directory to path for examples.actors imports
sys.path.insert(0, str(Path(__file__).parent.parent))

from examples.actors import TestPatternActor, DisplayActor


async def main():
    print("=" * 60)
    print("streamlib Actor Demo")
    print("=" * 60)
    print()
    print("Creating actors...")

    # Create test pattern generator (30 FPS, SMPTE bars)
    generator = TestPatternActor(
        actor_id='test-pattern',
        width=640,
        height=480,
        pattern='smpte_bars',
        fps=30.0
    )
    print(f"  ✓ Created {generator}")

    # Create display actor
    display = DisplayActor(
        actor_id='display',
        window_name='streamlib - Actor Demo'
    )
    print(f"  ✓ Created {display}")

    # Connect actors using pipe operator
    print()
    print("Connecting actors...")
    generator.outputs['video'] >> display.inputs['video']
    print(f"  ✓ Connected: {generator.actor_id} >> {display.actor_id}")

    # Set display clock to match generator (inherit upstream clock)
    display.clock = generator.clock
    print(f"  ✓ Display inherits clock from generator")

    print()
    print("Running pipeline...")
    print("  Press Ctrl+C to stop")
    print()

    # Print status every second
    try:
        while True:
            await asyncio.sleep(1.0)

            # Print status
            gen_status = generator.get_status()
            disp_status = display.get_status()

            print(f"\r[Generator] FPS={gen_status['fps']:.1f} Running={gen_status['running']} | "
                  f"[Display] Running={disp_status['running']}", end='', flush=True)

    except KeyboardInterrupt:
        print()
        print()
        print("Stopping...")

        # Stop actors
        await generator.stop()
        await display.stop()

        print("  ✓ Stopped")


if __name__ == '__main__':
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\nExiting...")
