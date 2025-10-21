#!/usr/bin/env python3
"""
Minimal test with single handler to find CPU busy loop.
"""

import asyncio
import sys
import time
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'src'))

from streamlib import StreamRuntime, StreamHandler, Stream, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class SimpleSourceHandler(StreamHandler):
    """Minimal source handler."""

    def __init__(self):
        super().__init__('simple-source')
        self.outputs['video'] = VideoOutput('video')
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

        # Generate minimal frame (WebGPU-first)
        gpu_ctx = self._runtime.gpu_context if self._runtime else None
        if gpu_ctx:
            texture = gpu_ctx.create_texture(width=100, height=100)
            frame = VideoFrame(texture, tick.timestamp, tick.frame_number, 100, 100)
            self.outputs['video'].write(frame)


async def main():
    print("Testing single handler...")
    print("Should show ~10 ticks per second")
    print("Press Ctrl+C to stop")
    print("="*50)

    # Create minimal pipeline
    source = SimpleSourceHandler()
    runtime = StreamRuntime(fps=10)
    runtime.add_stream(Stream(source))

    # Start
    await runtime.start()

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
