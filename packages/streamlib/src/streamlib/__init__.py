"""
streamlib: Composable streaming library for Python.

Core framework for building actor-based streaming pipelines.
This is the minimal SDK - implementations are in examples/.

Example:
    from streamlib import Actor, StreamInput, StreamOutput

    class MyActor(Actor):
        def __init__(self):
            super().__init__('my-actor')
            self.inputs['data'] = StreamInput('data')
            self.outputs['data'] = StreamOutput('data')
            self.start()

        async def process(self, tick):
            data = self.inputs['data'].read_latest()
            # Your processing logic
            self.outputs['data'].write(processed_data)
"""

# Core actor framework
from .actor import Actor, StreamInput, StreamOutput

# Ring buffer communication
from .buffers import RingBuffer

# Timing and synchronization
from .clocks import Clock, SoftwareClock, PTPClock, GenlockClock, TimedTick

# Execution dispatchers
from .dispatchers import Dispatcher, AsyncioDispatcher, ThreadPoolDispatcher

# Message types
from .messages import VideoFrame, AudioBuffer, KeyEvent, MouseEvent, DataMessage

# Actor registry (for network-transparent addressing)
from .registry import ActorRegistry, ActorURI, PortAllocator

# Actor stubs (for local/remote actors)
from .stubs import ActorStub, LocalActorStub, RemoteActorStub

# Utility function for connecting to actors
from .stubs import connect_actor


__all__ = [
    # Core actor framework
    'Actor',
    'StreamInput',
    'StreamOutput',

    # Ring buffer
    'RingBuffer',

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

    # Registry
    'ActorRegistry',
    'ActorURI',
    'PortAllocator',

    # Stubs
    'ActorStub',
    'LocalActorStub',
    'RemoteActorStub',
    'connect_actor',
]

__version__ = '0.1.0'
