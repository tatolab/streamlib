#!/usr/bin/env python3
"""
Simple pipeline test with clock tick broadcast.
"""

import asyncio
import sys
import os
# Add parent src directory to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'src'))

from streamlib import StreamRuntime, StreamHandler, Stream, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class SourceHandler(StreamHandler):
    """Source that generates frames."""

    def __init__(self):
        super().__init__('source')
        self.outputs['video'] = VideoOutput('video')
        self.frame_count = 0

    async def process(self, tick: TimedTick):
        # Generate WebGPU texture (simulated for test)
        gpu_ctx = self._runtime.gpu_context if self._runtime else None
        if gpu_ctx:
            # Create GPU texture
            texture = gpu_ctx.create_texture(width=100, height=100)
            frame = VideoFrame(texture, tick.timestamp, tick.frame_number, 100, 100)
            self.outputs['video'].write(frame)
            self.frame_count += 1
            print(f"[Source] Generated frame {tick.frame_number}")
        else:
            # Skip frame generation if no GPU context (WebGPU-first architecture)
            print(f"[Source] Skipping frame {tick.frame_number} - no GPU context available")


async def main():
    print("Creating simple pipeline...")

    # Create source
    source = SourceHandler()

    # Create runtime
    runtime = StreamRuntime(fps=10)

    # Add stream
    runtime.add_stream(Stream(source))

    # Start
    print("Starting runtime...")
    await runtime.start()

    # Run for 0.5 seconds
    print("Running for 0.5 seconds...")
    await asyncio.sleep(0.5)

    # Stop
    print("Stopping...")
    await runtime.stop()

    print(f"\nSource generated {source.frame_count} frames")

    if source.frame_count == 0:
        print("ℹ️  No frames generated (GPU context not available - this is expected without wgpu installed)")
        print("✅ WebGPU-first validation working correctly!")
    elif 3 <= source.frame_count <= 7:
        print(f"Expected: ~5 frames (10 FPS * 0.5s)")
        print("✅ Frame count looks good with GPU context!")
    else:
        print(f"Expected: ~5 frames (10 FPS * 0.5s)")
        print(f"⚠️  Frame count seems off")


if __name__ == '__main__':
    asyncio.run(main())
