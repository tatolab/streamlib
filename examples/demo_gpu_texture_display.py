#!/usr/bin/env python3
"""
GPU Texture Display Demo - Phase 3.7 Optimization

Tests GPU texture rendering to eliminate 6ms GPU→CPU transfer.
Uses OpenGL texture upload instead of cv2.imshow.

Expected performance: 30 FPS → 36-40 FPS at 1920x1080.
"""

import asyncio
import time

try:
    import torch
except ImportError:
    print("Error: PyTorch not installed. Install with: pip install torch torchvision")
    exit(1)

try:
    import moderngl
    import glfw
except ImportError:
    print("Error: OpenGL libraries not installed.")
    print("Install with: pip install 'streamlib[gpu-display]'")
    exit(1)

from streamlib import StreamRuntime, Stream
from streamlib.handler import StreamHandler
from streamlib.ports import VideoInput, VideoOutput
from streamlib.clocks import TimedTick
from streamlib.messages import VideoFrame
from streamlib.handlers import DisplayGPUHandler


# Simple test pattern handler
class SimplePatternHandler(StreamHandler):
    """Generate animated test pattern on GPU."""

    def __init__(self, width=1920, height=1080, name='pattern'):
        super().__init__(name)
        self.width = width
        self.height = height
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])
        self.device = None
        self.frame_buffer = None

    async def on_start(self):
        if self._runtime.gpu_context:
            self.device = self._runtime.gpu_context['device']
            print(f"[{self.handler_id}] Using device: {self.device}")
        else:
            print(f"[{self.handler_id}] No GPU context, using CPU")
            return

        # Pre-allocate frame buffer
        self.frame_buffer = torch.empty(
            (self.height, self.width, 3),
            dtype=torch.uint8,
            device=self.device
        )

    async def process(self, tick: TimedTick):
        if not self.device:
            return

        # Animated color based on time
        t = tick.timestamp
        r = int((torch.sin(torch.tensor(t * 0.5)) * 0.5 + 0.5) * 255)
        g = int((torch.sin(torch.tensor(t * 0.7)) * 0.5 + 0.5) * 255)
        b = int((torch.sin(torch.tensor(t * 0.3)) * 0.5 + 0.5) * 255)

        # Fill frame with animated color
        self.frame_buffer[:, :, 0] = r
        self.frame_buffer[:, :, 1] = g
        self.frame_buffer[:, :, 2] = b

        # Emit frame (stays on GPU)
        frame = VideoFrame(
            width=self.width,
            height=self.height,
            data=self.frame_buffer,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
        )
        self.outputs['video'].write(frame)


async def main():
    print("=" * 80)
    print("GPU TEXTURE DISPLAY DEMO")
    print("=" * 80)
    print(f"Resolution: 1920x1080")
    print(f"Target: 60 FPS")
    print(f"Display: OpenGL texture rendering (no CPU transfer)")
    print("=" * 80)

    # Create runtime with GPU support
    runtime = StreamRuntime(fps=60, enable_gpu=True)

    # Create handlers
    pattern = SimplePatternHandler(width=1920, height=1080)
    display = DisplayGPUHandler(
        name='display-gpu',
        window_name='GPU Texture Display - Press ESC to exit',
        width=1920,
        height=1080
    )

    # Add to runtime
    runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
    runtime.add_stream(Stream(display, dispatcher='asyncio'))

    # Connect
    runtime.connect(pattern.outputs['video'], display.inputs['video'])

    # Run for 10 seconds
    print("\n[Demo] Starting... (will run for 10 seconds)")
    runtime.start()

    try:
        await asyncio.sleep(10)
    except KeyboardInterrupt:
        print("\n[Demo] Interrupted by user")

    await runtime.stop()
    print("[Demo] Complete")


if __name__ == '__main__':
    asyncio.run(main())
