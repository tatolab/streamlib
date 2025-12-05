# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

#!/usr/bin/env python3
"""
Minimal test of event bus functionality.
"""

import asyncio
import sys
sys.path.insert(0, 'packages/streamlib/src')

from streamlib.events import EventBus, ClockTickEvent
from streamlib.clocks import TimedTick


async def test_event_bus():
    print("Creating event bus...")
    bus = EventBus()

    print("Subscribing to ClockTickEvent...")
    subscription = bus.subscribe(ClockTickEvent)

    print("Publishing first tick...")
    tick1 = TimedTick(timestamp=1.0, frame_number=0, clock_id='test')
    bus.publish(ClockTickEvent(tick1))

    print("Trying to receive event...")
    try:
        event = await asyncio.wait_for(subscription.queue.get(), timeout=1.0)
        print(f"✅ Received event: {event}")
        print(f"   Tick frame_number: {event.tick.frame_number}")
    except asyncio.TimeoutError:
        print("❌ Timeout waiting for event")

    print("\nTest complete!")


if __name__ == '__main__':
    asyncio.run(test_event_bus())
