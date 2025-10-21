#!/usr/bin/env python3
"""
Test that clock properly rate-limits ticks.
"""

import asyncio
import sys
import time
sys.path.insert(0, 'packages/streamlib/src')

from streamlib.clocks import SoftwareClock


async def test_clock_timing():
    """Test clock generates ticks at correct rate."""
    print("Testing clock timing...")

    # Create clock at 10 FPS
    clock = SoftwareClock(fps=10)

    # Reset clock (simulates what runtime does)
    clock.reset()

    # Generate 10 ticks and measure time
    start_time = time.time()
    for i in range(10):
        tick = await clock.next_tick()
        print(f"Tick {tick.frame_number} at {time.time() - start_time:.3f}s")

    elapsed = time.time() - start_time
    print(f"\nGenerated 10 ticks in {elapsed:.3f} seconds")
    print(f"Expected: ~1.0 seconds (10 ticks @ 10 FPS)")

    if 0.9 <= elapsed <= 1.1:
        print("✅ Clock timing is correct!")
        return True
    else:
        print(f"❌ Clock timing is wrong! (expected ~1.0s, got {elapsed:.3f}s)")
        return False


if __name__ == '__main__':
    result = asyncio.run(test_clock_timing())
    sys.exit(0 if result else 1)
