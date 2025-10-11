#!/usr/bin/env python3
"""
Minimal test with single handler to find CPU busy loop.
"""

import asyncio
import sys
import time
sys.path.insert(0, 'packages/streamlib/src')

from streamlib import StreamRuntime, StreamHandler, Stream, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick
import numpy as np


class SimpleSourceHandler(StreamHandler):
    """Minimal source handler."""

    def __init__(self):
        super().__init__('simple-source')
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])
        self.tick_count = 0
        self.last_print = time.time()

    async def process(self, tick: TimedTick):
        self.tick_count += 1

        # Print FPS every second
        now = time.time()
        if now - self.last_print >= 1.0:
            print(f"[Source] Processed {self.tick_count} ticks in last second")
            self.tick_count = 0
            self.last_print = now

        # Generate minimal frame
        data = np.zeros((100, 100, 3), dtype=np.uint8)
        frame = VideoFrame(data, tick.timestamp, tick.frame_number, 100, 100)
        self.outputs['video'].write(frame)


async def main():
    print("Testing single handler...")
    print("Should show ~10 ticks per second")
    print("Press Ctrl+C to stop")
    print("="*50)

    # Create minimal pipeline
    source = SimpleSourceHandler()
    runtime = StreamRuntime(fps=10)
    runtime.add_stream(Stream(source, dispatcher='asyncio'))

    # Start
    runtime.start()

    # Run for 5 seconds
    try:
        await asyncio.sleep(5.0)
    except KeyboardInterrupt:
        print("\nInterrupted")

    # Stop
    await runtime.stop()
    print("Done")


if __name__ == '__main__':
    asyncio.run(main())
