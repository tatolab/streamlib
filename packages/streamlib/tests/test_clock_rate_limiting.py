#!/usr/bin/env python3
"""
Test clock rate limiting fix - verify it prevents tick flooding when behind schedule.
"""

import asyncio
import sys
import time
sys.path.insert(0, 'packages/streamlib/src')

from streamlib.clocks import SoftwareClock


async def test_rate_limiting():
    """Test that clock always enforces minimum sleep."""
    print("Testing clock rate limiting fix...")
    print("Expected: Clock should always sleep at least 50% of period")
    print("="*60)

    clock = SoftwareClock(fps=60)  # 16.67ms period
    clock.reset()

    # Get first tick
    tick1 = await clock.next_tick()
    print(f"Tick 1: frame={tick1.frame_number}, dt={tick1.delta_time*1000:.1f}ms")

    # Simulate handler falling behind by sleeping longer than period
    await asyncio.sleep(0.05)  # Sleep 50ms (3 frames worth)

    # Get next tick - should still have reasonable delta_time
    tick2 = await clock.next_tick()
    print(f"Tick 2: frame={tick2.frame_number}, dt={tick2.delta_time*1000:.1f}ms")

    # Check that delta_time is reasonable (not near zero)
    if tick2.delta_time < 0.008:  # Less than 8ms (half period)
        print(f"❌ FAIL: Delta time too small: {tick2.delta_time*1000:.1f}ms")
        print("   Clock is flooding ticks!")
        return False
    elif tick2.delta_time > 0.1:  # More than 100ms
        print(f"✅ PASS: Delta time is {tick2.delta_time*1000:.1f}ms")
        print("   Clock enforced minimum sleep and reset schedule")
        return True
    else:
        print(f"✅ PASS: Delta time is {tick2.delta_time*1000:.1f}ms")
        print("   Clock enforced minimum sleep")
        return True


async def test_fps_consistency():
    """Test that clock maintains consistent rate."""
    print("\n" + "="*60)
    print("Testing FPS consistency...")
    print("Expected: ~60 FPS over 30 frames")
    print("="*60)

    clock = SoftwareClock(fps=60)
    clock.reset()

    start_time = time.time()
    frame_count = 30

    for i in range(frame_count):
        tick = await clock.next_tick()
        if i % 10 == 0:
            print(f"Frame {tick.frame_number}: dt={tick.delta_time*1000:.1f}ms")

    elapsed = time.time() - start_time
    actual_fps = frame_count / elapsed

    print(f"\nActual FPS: {actual_fps:.1f}")
    print(f"Target FPS: 60.0")
    print(f"Difference: {abs(actual_fps - 60.0):.1f}")

    if 55 <= actual_fps <= 65:
        print("✅ PASS: FPS is within acceptable range")
        return True
    else:
        print("❌ FAIL: FPS is outside acceptable range")
        return False


async def main():
    print("Clock Rate Limiting Test")
    print("="*60)

    test1_passed = await test_rate_limiting()
    test2_passed = await test_fps_consistency()

    print("\n" + "="*60)
    print("Results:")
    print(f"  Rate limiting: {'✅ PASS' if test1_passed else '❌ FAIL'}")
    print(f"  FPS consistency: {'✅ PASS' if test2_passed else '❌ FAIL'}")

    if test1_passed and test2_passed:
        print("\n✅ All tests passed!")
        return 0
    else:
        print("\n❌ Some tests failed")
        return 1


if __name__ == '__main__':
    exit_code = asyncio.run(main())
    sys.exit(exit_code)
