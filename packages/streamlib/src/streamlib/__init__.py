"""
streamlib: Composable streaming library for Python.

Core framework for building handler-based streaming pipelines with
GPU-first optimization and runtime-managed lifecycle.

Example:
    from streamlib import StreamRuntime, Stream, StreamHandler, VideoInput, VideoOutput, VideoFrame

    class BlurFilter(StreamHandler):
        def __init__(self):
            super().__init__()
            # GPU-first by default - runtime handles everything
            self.inputs['video'] = VideoInput('video')
            self.outputs['video'] = VideoOutput('video')

        async def process(self, tick):
            frame = self.inputs['video'].read_latest()
            if frame:
                result = cv2.GaussianBlur(frame.data, (5, 5), 0)
                self.outputs['video'].write(VideoFrame(
                    data=result,
                    timestamp=tick.timestamp,
                    frame_number=frame.frame_number,
                    width=frame.width,
                    height=frame.height
                ))

    # Create runtime
    runtime = StreamRuntime(fps=30)

    # Add streams (runtime infers execution context)
    runtime.add_stream(Stream(camera_handler))
    runtime.add_stream(Stream(blur_handler))

    # Connect (runtime handles capability negotiation and memory transfers)
    runtime.connect(camera_handler.outputs['video'], blur_handler.inputs['video'])

    # Start
    await runtime.start()

For concrete handler implementations, see streamlib-extras:
    pip install streamlib-extras
    from streamlib_extras import TestPatternHandler, DisplayGPUHandler
"""

# Phase 3.1: Core Infrastructure (NEW)
from .runtime import StreamRuntime
from .handler import StreamHandler
from .stream import Stream
from .function_handler import FunctionHandler, stream_handler
from .pipeline import PipelineBuilder, pipeline

# High-level decorators for AI-friendly API (Level 1 - Recommended)
from .decorators import video_effect, audio_effect, stream_processor, camera_source

# Event bus for communication (NEW - Phase 3.6)
from .events import (
    EventBus,
    Event,
    ClockTickEvent,
    ErrorEvent,
    HandlerStartedEvent,
    HandlerStoppedEvent,
)

# Capability-based ports (NEW)
from .ports import (
    StreamInput,
    StreamOutput,
    VideoInput,
    VideoOutput,
    AudioInput,
    AudioOutput,
    DataInput,
    DataOutput,
)

# Note: Concrete handler implementations have moved to streamlib-extras
# Install with: pip install streamlib-extras
# Import from: from streamlib_extras import TestPatternHandler, DisplayGPUHandler

# Ring buffers
from .buffers import RingBuffer, GPURingBuffer

# Clocks
from .clocks import Clock, SoftwareClock, PTPClock, GenlockClock, TimedTick

# Messages
from .messages import VideoFrame, AudioBuffer, KeyEvent, MouseEvent, DataMessage

__all__ = [
    # Core framework
    'StreamRuntime',
    'StreamHandler',
    'Stream',
    'FunctionHandler',
    'stream_handler',
    'PipelineBuilder',
    'pipeline',

    # High-level decorators (Level 1 API - Recommended for AI agents)
    'video_effect',
    'audio_effect',
    'stream_processor',
    'camera_source',

    # Event bus
    'EventBus',
    'Event',
    'ClockTickEvent',
    'ErrorEvent',
    'HandlerStartedEvent',
    'HandlerStoppedEvent',

    # Ports
    'StreamInput',
    'StreamOutput',
    'VideoInput',
    'VideoOutput',
    'AudioInput',
    'AudioOutput',
    'DataInput',
    'DataOutput',

    # Ring buffers
    'RingBuffer',
    'GPURingBuffer',

    # Clocks
    'Clock',
    'SoftwareClock',
    'PTPClock',
    'GenlockClock',
    'TimedTick',

    # Messages
    'VideoFrame',
    'AudioBuffer',
    'KeyEvent',
    'MouseEvent',
    'DataMessage',
]

__version__ = '0.2.0'  # Phase 3.1: StreamHandler + Runtime
