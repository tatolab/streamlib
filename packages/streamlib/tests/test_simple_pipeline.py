#!/usr/bin/env python3
"""
Simple pipeline test with clock tick broadcast.
"""

import asyncio
import sys
sys.path.insert(0, 'packages/streamlib/src')

from streamlib import StreamRuntime, StreamHandler, Stream, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick
import numpy as np


class SourceHandler(StreamHandler):
    """Source that generates frames."""

    def __init__(self):
        super().__init__('source')
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])
        self.frame_count = 0

    async def process(self, tick: TimedTick):
        # Generate frame
        data = np.zeros((100, 100, 3), dtype=np.uint8)
        frame = VideoFrame(data, tick.timestamp, tick.frame_number, 100, 100)
        self.outputs['video'].write(frame)
        self.frame_count += 1
        print(f"[Source] Generated frame {tick.frame_number}")


async def main():
    print("Creating simple pipeline...")

    # Create source
    source = SourceHandler()

    # Create runtime
    runtime = StreamRuntime(fps=10)

    # Add stream
    runtime.add_stream(Stream(source, dispatcher='asyncio'))

    # Start
    print("Starting runtime...")
    runtime.start()

    # Run for 0.5 seconds
    print("Running for 0.5 seconds...")
    await asyncio.sleep(0.5)

    # Stop
    print("Stopping...")
    await runtime.stop()

    print(f"\nSource generated {source.frame_count} frames")
    print(f"Expected: ~5 frames (10 FPS * 0.5s)")

    if 3 <= source.frame_count <= 7:
        print("✅ Frame count looks good!")
    else:
        print(f"⚠️  Frame count seems off")


if __name__ == '__main__':
    asyncio.run(main())
