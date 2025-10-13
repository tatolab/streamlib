#!/usr/bin/env python3
"""Quick verification that GPU texture display works."""

import asyncio
import sys

try:
    import torch
    import moderngl
    import glfw
except ImportError as e:
    print(f"Error: Missing dependency - {e}")
    print("Install with: uv pip install 'streamlib[gpu-display,gpu]'")
    sys.exit(1)

from streamlib import StreamRuntime, Stream
from streamlib.handler import StreamHandler
from streamlib.ports import VideoInput, VideoOutput
from streamlib.clocks import TimedTick
from streamlib.messages import VideoFrame

try:
    from streamlib_extras import DisplayGPUHandler
except ImportError:
    print("Error: DisplayGPUHandler not available")
    sys.exit(1)


class SimplePatternHandler(StreamHandler):
    """Generate simple test pattern."""

    def __init__(self, width=640, height=480):
        super().__init__('pattern')
        self.width = width
        self.height = height
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])
        self.device = None
        self.frame_buffer = None

    async def on_start(self):
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']
            self.frame_buffer = torch.empty(
                (self.height, self.width, 3),
                dtype=torch.uint8,
                device=self.device
            )
            print(f"[{self.handler_id}] Initialized on {self.device}")

    async def process(self, tick: TimedTick):
        if not self.device:
            return

        # Simple animated color
        t = tick.timestamp
        r = int((torch.sin(torch.tensor(t * 2.0)) * 0.5 + 0.5) * 255)

        self.frame_buffer[:, :, 0] = r
        self.frame_buffer[:, :, 1] = 128
        self.frame_buffer[:, :, 2] = 255 - r

        frame = VideoFrame(
            width=self.width,
            height=self.height,
            data=self.frame_buffer,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
        )
        self.outputs['video'].write(frame)


async def main():
    print("=" * 60)
    print("GPU Texture Display Verification")
    print("=" * 60)

    # Create runtime with GPU support
    runtime = StreamRuntime(fps=30, enable_gpu=True)

    # Create handlers
    pattern = SimplePatternHandler(width=640, height=480)
    display = DisplayGPUHandler(
        name='display-gpu',
        window_name='GPU Display Test',
        width=640,
        height=480
    )

    # Build pipeline
    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='asyncio'))
    runtime.connect(pattern.outputs['video'], display.inputs['video'])

    print("\nStarting runtime (will run for 3 seconds)...")
    runtime.start()

    try:
        await asyncio.sleep(3)
    except KeyboardInterrupt:
        print("\nInterrupted by user")

    print("\nStopping runtime...")
    await runtime.stop()

    print("\n" + "=" * 60)
    print("Verification complete!")
    print("=" * 60)


if __name__ == '__main__':
    asyncio.run(main())
