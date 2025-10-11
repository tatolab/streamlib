#!/usr/bin/env python3
"""
Ultra-minimal test - just clock + event bus, no frame processing.
"""

import asyncio
import sys
import time
sys.path.insert(0, 'packages/streamlib/src')

from streamlib.events import EventBus, ClockTickEvent
from streamlib.clocks import SoftwareClock


async def handler_task(bus, handler_id):
    """Minimal handler that just counts ticks."""
    tick_count = 0
    last_print = time.time()

    subscription = bus.subscribe(ClockTickEvent)

    print(f"[{handler_id}] Started, waiting for ticks...")

    async for event in subscription:
        tick_count += 1

        # Print FPS every second
        now = time.time()
        if now - last_print >= 1.0:
            print(f"[{handler_id}] {tick_count} ticks/sec")
            tick_count = 0
            last_print = now


async def clock_task(bus, clock):
    """Minimal clock loop."""
    print("[Clock] Starting...")
    clock.reset()

    while True:
        tick = await clock.next_tick()
        bus.publish(ClockTickEvent(tick))


async def main():
    print("Ultra-minimal test: Clock + EventBus only")
    print("Expected: ~10 ticks/sec, low CPU usage")
    print("="*50)

    # Create clock and event bus
    clock = SoftwareClock(fps=10)
    bus = EventBus()

    # Start tasks
    clock_t = asyncio.create_task(clock_task(bus, clock))
    handler_t = asyncio.create_task(handler_task(bus, 'handler-1'))

    # Run for 3 seconds
    await asyncio.sleep(3.0)

    # Cancel tasks
    clock_t.cancel()
    handler_t.cancel()

    try:
        await clock_t
    except asyncio.CancelledError:
        pass

    try:
        await handler_t
    except asyncio.CancelledError:
        pass

    print("\nDone!")


if __name__ == '__main__':
    asyncio.run(main())
