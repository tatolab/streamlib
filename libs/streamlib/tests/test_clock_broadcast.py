#!/usr/bin/env python3
"""
Test clock tick broadcast fix.

Should show all handlers processing the same tick concurrently.
"""

import asyncio
import sys
import os
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'src'))

from streamlib import StreamRuntime, StreamHandler, Stream, VideoInput, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class SourceHandler(StreamHandler):
    """Source handler that generates frames."""

    def __init__(self):
        super().__init__('source')
        self.outputs['video'] = VideoOutput('video')
        self.processed_ticks = []

    async def process(self, tick: TimedTick):
        self.processed_ticks.append(tick.frame_number)
        print(f"[{self.handler_id}] Generating frame for tick {tick.frame_number}")

        # Generate frame (WebGPU-first)
        gpu_ctx = self._runtime.gpu_context if self._runtime else None
        if gpu_ctx:
            texture = gpu_ctx.create_texture(width=100, height=100)
            frame = VideoFrame(texture, tick.timestamp, tick.frame_number, 100, 100)
            # Write output
            self.outputs['video'].write(frame)


class ProcessHandler(StreamHandler):
    """Processing handler that reads and writes frames."""

    def __init__(self, name):
        super().__init__(name)
        self.inputs['video'] = VideoInput('video')
        self.outputs['video'] = VideoOutput('video')
        self.processed_ticks = []

    async def process(self, tick: TimedTick):
        self.processed_ticks.append(tick.frame_number)
        print(f"[{self.handler_id}] Processing tick {tick.frame_number}")

        # Read input
        frame = self.inputs['video'].read_latest()

        if frame:
            # Write output
            self.outputs['video'].write(frame)


async def main():
    print("Testing clock tick broadcast fix...")
    print("="*70)

    # Create source + 2 process handlers
    source = SourceHandler()
    handler1 = ProcessHandler('process-1')
    handler2 = ProcessHandler('process-2')

    # Create runtime
    runtime = StreamRuntime(fps=10)  # 10 FPS for faster testing

    # Add streams
    runtime.add_stream(Stream(source))
    runtime.add_stream(Stream(handler1))
    runtime.add_stream(Stream(handler2))

    # Connect pipeline
    runtime.connect(source.outputs['video'], handler1.inputs['video'])
    runtime.connect(handler1.outputs['video'], handler2.inputs['video'])

    # Start runtime
    await runtime.start()

    # Run for 1 second (should get 10 ticks)
    print("\nRunning for 1 second (10 ticks)...")
    await asyncio.sleep(1.0)

    # Stop runtime
    await runtime.stop()

    # Check results
    print("\n" + "="*70)
    print("Results:")
    print("="*70)
    print(f"Source processed:   {source.processed_ticks}")
    print(f"Handler 1 processed: {handler1.processed_ticks}")
    print(f"Handler 2 processed: {handler2.processed_ticks}")

    # Verify all handlers processed the same ticks
    if source.processed_ticks == handler1.processed_ticks == handler2.processed_ticks:
        print("\n✅ SUCCESS: All handlers processed the same ticks!")
        print(f"   All handlers saw ticks: {source.processed_ticks}")
    else:
        print("\n❌ FAILURE: Handlers processed different ticks")
        print("   This indicates the clock broadcast is not working correctly")

    # Expected: ~10 ticks
    tick_count = len(source.processed_ticks)
    print(f"\nTotal ticks processed: {tick_count}")

    if 8 <= tick_count <= 12:
        print(f"✅ Tick count is reasonable (expected ~10, got {tick_count})")
    else:
        print(f"⚠️  Tick count seems off (expected ~10, got {tick_count})")


if __name__ == '__main__':
    asyncio.run(main())
