"""
streamlib: Composable streaming library for Python.

Phase 3: StreamHandler + Runtime Architecture

Core framework for building handler-based streaming pipelines with
capability negotiation and runtime-managed lifecycle.

Example:
    from streamlib import StreamRuntime, Stream, StreamHandler, VideoInput, VideoOutput

    class BlurFilter(StreamHandler):
        def __init__(self):
            super().__init__()
            self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
            self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])

        async def process(self, tick):
            frame = self.inputs['video'].read_latest()
            if frame:
                result = cv2.GaussianBlur(frame.data, (5, 5), 0)
                self.outputs['video'].write(VideoFrame(result, tick.timestamp, ...))

    # Create runtime
    runtime = StreamRuntime(fps=30)

    # Add streams
    runtime.add_stream(Stream(camera_handler, dispatcher='asyncio'))
    runtime.add_stream(Stream(blur_handler, dispatcher='threadpool'))

    # Connect with capability negotiation
    runtime.connect(camera_handler.outputs['video'], blur_handler.inputs['video'])

    # Start
    await runtime.start()
"""

# Phase 3.1: Core Infrastructure (NEW)
from .runtime import StreamRuntime
from .handler import StreamHandler
from .stream import Stream

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

# Transfer handlers (NEW)
from .transfers import CPUtoGPUTransferHandler, GPUtoCPUTransferHandler

# Phase 3.2: Basic Handlers (NEW)
from .handlers import TestPatternHandler, DisplayHandler

# Ring buffers
from .buffers import RingBuffer, GPURingBuffer

# Clocks
from .clocks import Clock, SoftwareClock, PTPClock, GenlockClock, TimedTick

# Dispatchers
from .dispatchers import Dispatcher, AsyncioDispatcher, ThreadPoolDispatcher

# Messages
from .messages import VideoFrame, AudioBuffer, KeyEvent, MouseEvent, DataMessage

# Legacy Actor model (DEPRECATED - will be removed)
# Keeping for backward compatibility during transition
from .actor import Actor, StreamInput as LegacyStreamInput, StreamOutput as LegacyStreamOutput
from .registry import ActorRegistry, ActorURI, PortAllocator
from .stubs import ActorStub, LocalActorStub, RemoteActorStub, connect_actor

__all__ = [
    # Phase 3.1: StreamHandler + Runtime (NEW)
    'StreamRuntime',
    'StreamHandler',
    'Stream',

    # Capability-based ports (NEW)
    'StreamInput',
    'StreamOutput',
    'VideoInput',
    'VideoOutput',
    'AudioInput',
    'AudioOutput',
    'DataInput',
    'DataOutput',

    # Transfer handlers (NEW)
    'CPUtoGPUTransferHandler',
    'GPUtoCPUTransferHandler',

    # Phase 3.2: Basic Handlers (NEW)
    'TestPatternHandler',
    'DisplayHandler',

    # Ring buffers
    'RingBuffer',
    'GPURingBuffer',

    # Clocks
    'Clock',
    'SoftwareClock',
    'PTPClock',
    'GenlockClock',
    'TimedTick',

    # Dispatchers
    'Dispatcher',
    'AsyncioDispatcher',
    'ThreadPoolDispatcher',

    # Messages
    'VideoFrame',
    'AudioBuffer',
    'KeyEvent',
    'MouseEvent',
    'DataMessage',

    # Legacy Actor model (DEPRECATED)
    'Actor',
    'LegacyStreamInput',
    'LegacyStreamOutput',
    'ActorRegistry',
    'ActorURI',
    'PortAllocator',
    'ActorStub',
    'LocalActorStub',
    'RemoteActorStub',
    'connect_actor',
]

__version__ = '0.2.0'  # Phase 3.1: StreamHandler + Runtime
