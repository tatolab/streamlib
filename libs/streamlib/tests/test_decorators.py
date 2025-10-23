"""
Test the high-level decorator API.

Tests @video_effect, @audio_effect, and @stream_processor decorators.
"""

import pytest
import asyncio
import numpy as np
from streamlib import (
    StreamRuntime,
    Stream,
    video_effect,
    audio_effect,
    stream_processor,
    VideoFrame,
    AudioBuffer,
    VideoInput,
    VideoOutput,
    TimedTick,
)


def test_video_effect_decorator():
    """Test that @video_effect creates a valid StreamHandler."""

    @video_effect
    def test_effect(frame: VideoFrame) -> VideoFrame:
        """Simple pass-through effect."""
        return frame

    # Check that decorator returns a StreamHandler
    from streamlib.handler import StreamHandler
    assert isinstance(test_effect, StreamHandler)

    # Check that handler has correct ports
    assert 'video' in test_effect.inputs
    assert 'video' in test_effect.outputs

    # Check handler ID
    assert test_effect.handler_id == 'test_effect'


def test_audio_effect_decorator():
    """Test that @audio_effect creates a valid StreamHandler."""

    @audio_effect
    def test_audio_effect(buffer: AudioBuffer) -> AudioBuffer:
        """Simple pass-through audio effect."""
        return buffer

    # Check that decorator returns a StreamHandler
    from streamlib.handler import StreamHandler
    assert isinstance(test_audio_effect, StreamHandler)

    # Check that handler has correct ports
    assert 'audio' in test_audio_effect.inputs
    assert 'audio' in test_audio_effect.outputs

    # Check handler ID
    assert test_audio_effect.handler_id == 'test_audio_effect'


def test_stream_processor_decorator():
    """Test that @stream_processor creates a valid StreamHandler."""

    @stream_processor(
        inputs={'video': VideoInput('video')},
        outputs={'video': VideoOutput('video')}
    )
    async def test_processor(tick: TimedTick, inputs: dict, outputs: dict) -> None:
        """Simple pass-through processor."""
        frame = inputs['video'].read_latest()
        if frame:
            outputs['video'].write(frame)

    # Check that decorator returns a StreamHandler
    from streamlib.handler import StreamHandler
    assert isinstance(test_processor, StreamHandler)

    # Check that handler has correct ports
    assert 'video' in test_processor.inputs
    assert 'video' in test_processor.outputs


@pytest.mark.asyncio
async def test_video_effect_processing():
    """Test that @video_effect actually processes frames."""

    # Track whether effect was called
    effect_called = False

    @video_effect
    def simple_effect(frame: VideoFrame, gpu) -> VideoFrame:
        """Effect that modifies frame data (pass-through for GPU textures)."""
        nonlocal effect_called
        effect_called = True

        # For WebGPU textures, just pass through
        # (Actual GPU compute would require WGSL shader)
        return VideoFrame(
            data=frame.data,  # WebGPU texture
            timestamp=frame.timestamp,
            frame_number=frame.frame_number,
            width=frame.width,
            height=frame.height
        )

    # Create a simple test pattern source (GPU-first)
    class TestSource:
        def __init__(self):
            from streamlib.handler import StreamHandler
            from streamlib.ports import VideoOutput

            class Source(StreamHandler):
                def __init__(self):
                    super().__init__()
                    self.outputs['video'] = VideoOutput('video')
                    self.frame_count = 0

                async def process(self, tick):
                    # Generate WebGPU texture
                    gpu_ctx = self._runtime.gpu_context
                    if not gpu_ctx:
                        return

                    # Create GPU texture (WebGPU-first architecture)
                    texture = gpu_ctx.create_texture(width=640, height=480)

                    self.outputs['video'].write(VideoFrame(
                        data=texture,  # WebGPU texture, not NumPy
                        timestamp=tick.timestamp,
                        frame_number=tick.frame_number,
                        width=640,
                        height=480
                    ))
                    self.frame_count += 1

            self.handler = Source()

        def __getattr__(self, name):
            return getattr(self.handler, name)

    # Create a simple sink to verify output
    class TestSink:
        def __init__(self):
            from streamlib.handler import StreamHandler
            from streamlib.ports import VideoInput

            class Sink(StreamHandler):
                def __init__(self):
                    super().__init__()
                    self.inputs['video'] = VideoInput('video')
                    self.frames_received = []

                async def process(self, tick):
                    frame = self.inputs['video'].read_latest()
                    if frame:
                        self.frames_received.append(frame)

            self.handler = Sink()

        def __getattr__(self, name):
            return getattr(self.handler, name)

    # Create runtime and pipeline (GPU-first)
    runtime = StreamRuntime(fps=30, enable_gpu=True)

    source = TestSource()
    sink = TestSink()

    runtime.add_stream(Stream(source.handler))
    runtime.add_stream(Stream(simple_effect))
    runtime.add_stream(Stream(sink.handler))

    # Connect pipeline
    runtime.connect(source.outputs['video'], simple_effect.inputs['video'])
    runtime.connect(simple_effect.outputs['video'], sink.inputs['video'])

    # Start runtime
    await runtime.start()

    # Let it run for a few frames
    await asyncio.sleep(0.2)

    # Stop runtime
    await runtime.stop()

    # Verify effect was called
    assert effect_called, "Effect function was not called"

    # Verify sink received frames
    assert len(sink.handler.frames_received) > 0, "Sink did not receive any frames"

    print(f"✓ Effect processed {len(sink.handler.frames_received)} frames")


@pytest.mark.asyncio
async def test_decorator_with_custom_id():
    """Test that decorator respects custom handler_id."""

    @video_effect(handler_id='custom_blur_id')
    def blur_with_custom_id(frame: VideoFrame) -> VideoFrame:
        return frame

    assert blur_with_custom_id.handler_id == 'custom_blur_id'


if __name__ == '__main__':
    # Run basic tests
    print("Running decorator tests...")
    print()

    print("1. Testing @video_effect decorator...")
    test_video_effect_decorator()
    print("   ✓ @video_effect creates valid StreamHandler")

    print("2. Testing @audio_effect decorator...")
    test_audio_effect_decorator()
    print("   ✓ @audio_effect creates valid StreamHandler")

    print("3. Testing @stream_processor decorator...")
    test_stream_processor_decorator()
    print("   ✓ @stream_processor creates valid StreamHandler")

    print("4. Testing decorator with custom ID...")
    asyncio.run(test_decorator_with_custom_id())
    print("   ✓ Custom handler_id works")

    print("5. Testing @video_effect processing...")
    asyncio.run(test_video_effect_processing())
    print("   ✓ Effect processes frames correctly")

    print()
    print("All decorator tests passed!")
