#!/usr/bin/env python3
"""
Test GPU-first pipeline: Source → Effect → Sink

Validates that WebGPU textures flow through the entire pipeline correctly.
"""

import pytest
import asyncio
import sys
sys.path.insert(0, 'packages/streamlib/src')

from streamlib import (
    StreamRuntime,
    StreamHandler,
    Stream,
    VideoInput,
    VideoOutput,
    VideoFrame,
)


class GPUTextureSource(StreamHandler):
    """Source that generates WebGPU textures."""

    def __init__(self):
        super().__init__('gpu_source')
        self.outputs['video'] = VideoOutput('video')
        self.frame_count = 0

    async def process(self, tick):
        """Generate GPU texture."""
        gpu_ctx = self._runtime.gpu_context
        if not gpu_ctx:
            return

        # Create GPU texture (WebGPU-first)
        texture = gpu_ctx.create_texture(width=640, height=480)

        self.outputs['video'].write(VideoFrame(
            data=texture,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=640,
            height=480
        ))
        self.frame_count += 1


class GPUPassThroughEffect(StreamHandler):
    """Pass-through effect that validates GPU textures."""

    def __init__(self):
        super().__init__('gpu_effect')
        self.inputs['video'] = VideoInput('video')
        self.outputs['video'] = VideoOutput('video')
        self.processed_count = 0

    async def process(self, tick):
        """Pass through GPU texture."""
        frame = self.inputs['video'].read_latest()
        if frame:
            # Verify it's a WebGPU texture
            try:
                import wgpu
                assert hasattr(frame.data, 'create_view'), "Expected WebGPU texture"
            except ImportError:
                pass

            # Pass through
            self.outputs['video'].write(frame)
            self.processed_count += 1


class GPUTextureSink(StreamHandler):
    """Sink that receives and validates GPU textures."""

    def __init__(self):
        super().__init__('gpu_sink')
        self.inputs['video'] = VideoInput('video')
        self.frames_received = []

    async def process(self, tick):
        """Receive GPU texture."""
        frame = self.inputs['video'].read_latest()
        if frame:
            # Verify it's a WebGPU texture
            try:
                import wgpu
                assert hasattr(frame.data, 'create_view'), "Expected WebGPU texture"
            except ImportError:
                pass

            self.frames_received.append(frame)


@pytest.mark.asyncio
async def test_gpu_pipeline():
    """Test GPU-first pipeline: Source → Effect → Sink."""

    # Create handlers
    source = GPUTextureSource()
    effect = GPUPassThroughEffect()
    sink = GPUTextureSink()

    # Create runtime with GPU enabled
    runtime = StreamRuntime(fps=30, enable_gpu=True)

    # Add streams
    runtime.add_stream(Stream(source))
    runtime.add_stream(Stream(effect))
    runtime.add_stream(Stream(sink))

    # Connect pipeline
    runtime.connect(source.outputs['video'], effect.inputs['video'])
    runtime.connect(effect.outputs['video'], sink.inputs['video'])

    # Start runtime
    await runtime.start()

    # Verify GPU context was created
    assert runtime.gpu_context is not None, "GPU context not created"
    print(f"✓ GPU context: {runtime.gpu_context.backend_name}")

    # Let it run for a few frames
    await asyncio.sleep(0.2)

    # Stop runtime
    await runtime.stop()

    # Verify frames flowed through pipeline
    assert source.frame_count > 0, "Source didn't generate frames"
    assert effect.processed_count > 0, "Effect didn't process frames"
    assert len(sink.frames_received) > 0, "Sink didn't receive frames"

    print(f"✓ Source generated {source.frame_count} frames")
    print(f"✓ Effect processed {effect.processed_count} frames")
    print(f"✓ Sink received {len(sink.frames_received)} frames")

    # Verify pipeline integrity (all frames made it through)
    assert effect.processed_count == source.frame_count, "Frames lost in effect"
    assert len(sink.frames_received) >= effect.processed_count - 1, "Frames lost in sink"

    print("✓ GPU pipeline test passed!")


if __name__ == '__main__':
    asyncio.run(test_gpu_pipeline())
