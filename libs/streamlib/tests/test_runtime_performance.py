# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

#!/usr/bin/env python3
"""
Test runtime performance with delta time.
Shows FPS and frame timing without heavy rendering.
"""

import asyncio
import sys
import time
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'src'))

from streamlib import StreamRuntime, StreamHandler, Stream, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class LightweightSourceHandler(StreamHandler):
    """Minimal source - just creates small frames."""

    def __init__(self):
        super().__init__('source')
        self.outputs['video'] = VideoOutput('video')
        self.frame_count = 0
        self.start_time = time.time()
        self.last_report = time.time()
        self.delta_times = []

    async def process(self, tick: TimedTick):
        self.frame_count += 1
        self.delta_times.append(tick.delta_time * 1000)  # Convert to ms

        # Report every second
        now = time.time()
        if now - self.last_report >= 1.0:
            elapsed = now - self.start_time
            actual_fps = self.frame_count / elapsed
            avg_dt = sum(self.delta_times) / len(self.delta_times) if self.delta_times else 0

            print(f"FPS: {actual_fps:.1f} | Avg dt: {avg_dt:.1f}ms | Frames: {self.frame_count}")

            self.last_report = now
            self.delta_times = []

        # Create minimal frame (WebGPU-first architecture)
        gpu_ctx = self._runtime.gpu_context if self._runtime else None
        if gpu_ctx:
            texture = gpu_ctx.create_texture(width=100, height=100)
            frame = VideoFrame(texture, tick.timestamp, tick.frame_number, 100, 100)
            self.outputs['video'].write(frame)


async def main():
    print("Testing runtime performance at 60 FPS target")
    print("This should show consistent FPS and frame timing")
    print("="*60)

    # Create minimal pipeline
    source = LightweightSourceHandler()
    runtime = StreamRuntime(fps=60)
    runtime.add_stream(Stream(source))

    # Start
    await runtime.start()

    # Run for 5 seconds
    try:
        await asyncio.sleep(5.0)
    except KeyboardInterrupt:
        pass

    # Stop
    await runtime.stop()

    print("\nDone!")
    print(f"Total frames: {source.frame_count}")
    print(f"Expected: ~300 frames (60 FPS * 5 seconds)")


if __name__ == '__main__':
    asyncio.run(main())
