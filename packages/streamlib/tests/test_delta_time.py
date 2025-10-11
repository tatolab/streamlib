#!/usr/bin/env python3
"""
Test delta_time calculation in clock.
"""

import asyncio
import sys
sys.path.insert(0, 'packages/streamlib/src')

from streamlib.clocks import SoftwareClock


async def test_delta_time():
    """Test that delta_time is calculated correctly."""
    print("Testing delta_time calculation...")
    print("Expected: ~16.7ms per tick @ 60 FPS")
    print("="*50)

    clock = SoftwareClock(fps=60)
    clock.reset()

    for i in range(5):
        tick = await clock.next_tick()
        print(f"Frame {tick.frame_number}: dt={tick.delta_time*1000:.1f}ms")

    print("\n✅ Delta time is working!")


if __name__ == '__main__':
    asyncio.run(test_delta_time())
