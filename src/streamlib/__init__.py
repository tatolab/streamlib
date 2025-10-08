"""
streamlib - Composable realtime streaming library for Python

Actor-based streaming library with SMPTE ST 2110 alignment.
Network-transparent operations for distributed realtime processing.

Architecture:
- Actors: Independent components processing ticks
- Ring buffers: Fixed-size circular buffers (latest-read semantics)
- Clocks: Swappable sync sources (Software, PTP, Genlock)
- Dispatchers: Execution contexts (Asyncio, ThreadPool, ProcessPool, GPU)

Example:
    from streamlib import TestPatternActor, DisplayActor

    # Create actors (auto-start)
    gen = TestPatternActor(pattern='smpte_bars', fps=60)
    display = DisplayActor(window_name='Output')

    # Connect (pipe operator)
    gen.outputs['video'] >> display.inputs['video']

    # Already running!
    await asyncio.Event().wait()  # Run forever
"""

# Core infrastructure
from .buffers import RingBuffer, GPURingBuffer
from .clocks import Clock, SoftwareClock, PTPClock, GenlockClock, TimedTick
from .dispatchers import (
    Dispatcher,
    AsyncioDispatcher,
    ThreadPoolDispatcher,
    ProcessPoolDispatcher,
    GPUDispatcher,
)
from .actor import Actor, StreamInput, StreamOutput

# Registry and stubs (network transparency)
from .registry import ActorURI, ActorRegistry, PortAllocator
from .stubs import ActorStub, LocalActorStub, RemoteActorStub, connect_actor

# Message types
from .messages import (
    VideoFrame,
    AudioBuffer,
    KeyEvent,
    MouseEvent,
    DataMessage,
)

# Actors
from .actors import (
    TestPatternActor,
    DisplayActor,
    CompositorActor,
)

__version__ = "0.2.0"  # Phase 3: Actor implementation

__all__ = [
    # Core infrastructure
    "RingBuffer",
    "GPURingBuffer",
    "Clock",
    "SoftwareClock",
    "PTPClock",
    "GenlockClock",
    "TimedTick",
    "Dispatcher",
    "AsyncioDispatcher",
    "ThreadPoolDispatcher",
    "ProcessPoolDispatcher",
    "GPUDispatcher",
    "Actor",
    "StreamInput",
    "StreamOutput",
    # Registry and stubs
    "ActorURI",
    "ActorRegistry",
    "PortAllocator",
    "ActorStub",
    "LocalActorStub",
    "RemoteActorStub",
    "connect_actor",
    # Messages
    "VideoFrame",
    "AudioBuffer",
    "KeyEvent",
    "MouseEvent",
    "DataMessage",
    # Actors
    "TestPatternActor",
    "DisplayActor",
    "CompositorActor",
]
