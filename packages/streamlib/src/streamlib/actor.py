"""
Actor base class and connection primitives.

Actors are independent, concurrent components that:
- Process ticks from a clock
- Read from input ring buffers
- Write to output ring buffers
- Auto-start on creation
- Run until stopped

Example:
    class MyActor(Actor):
        def __init__(self):
            super().__init__(actor_id='my-actor')
            self.outputs['data'] = StreamOutput('data')
            # Actor begins processing automatically

        async def process(self, tick: TimedTick):
            # Read latest from inputs (if any)
            # Do work
            # Write to outputs
            result = do_work()
            self.outputs['data'].write(result)

    # Actors begin processing immediately when created
    actor = MyActor()  # Already running!

    # Connect actors
    actor1.outputs['data'] >> actor2.inputs['data']
"""

import asyncio
import traceback
from abc import ABC, abstractmethod
from typing import Dict, Optional, Any, TypeVar, Generic

from .buffers import RingBuffer
from .clocks import Clock, SoftwareClock, TimedTick
from .dispatchers import Dispatcher, AsyncioDispatcher


T = TypeVar('T')


class StreamInput(Generic[T]):
    """
    Input port for an actor (reads from ring buffer).

    Usage:
        # In actor __init__:
        self.inputs['video'] = StreamInput('video')

        # In actor process():
        frame = self.inputs['video'].read_latest()
        if frame is not None:
            # Process frame
            pass
    """

    def __init__(self, name: str):
        """
        Initialize input port.

        Args:
            name: Port name (e.g., 'video', 'audio', 'data')
        """
        self.name = name
        self.buffer: Optional[RingBuffer[T]] = None

    def connect(self, buffer: RingBuffer[T]) -> None:
        """
        Connect input to a ring buffer.

        Args:
            buffer: Ring buffer to read from

        Note: Usually called via >> operator, not directly.
        """
        self.buffer = buffer

    def read_latest(self) -> Optional[T]:
        """
        Read latest data from ring buffer.

        Returns:
            Most recent data, or None if:
            - Not connected yet
            - No data written yet

        Note: This is non-blocking and always returns immediately.
        Old data is automatically skipped (latest-read semantics).
        """
        if self.buffer is None:
            return None
        return self.buffer.read_latest()

    def is_connected(self) -> bool:
        """
        Check if input is connected to a buffer.

        Returns:
            True if connected, False otherwise
        """
        return self.buffer is not None

    def is_empty(self) -> bool:
        """
        Check if input has data available.

        Returns:
            True if no data available, False if data available
        """
        if self.buffer is None:
            return True
        return self.buffer.is_empty()


class StreamOutput(Generic[T]):
    """
    Output port for an actor (writes to ring buffer).

    Usage:
        # In actor __init__:
        self.outputs['video'] = StreamOutput('video')

        # In actor process():
        frame = generate_frame()
        self.outputs['video'].write(frame)

        # Connect to downstream actor:
        actor1.outputs['video'] >> actor2.inputs['video']
    """

    def __init__(self, name: str, slots: int = 3):
        """
        Initialize output port.

        Args:
            name: Port name (e.g., 'video', 'audio', 'data')
            slots: Ring buffer size (default: 3, matches broadcast practice)
        """
        self.name = name
        self.buffer: RingBuffer[T] = RingBuffer(slots=slots)
        self.subscribers = []  # Track connections (for debugging)

    def write(self, data: T) -> None:
        """
        Write data to ring buffer.

        Args:
            data: Data to write

        Note: Overwrites oldest slot if buffer full (no backpressure).
        """
        self.buffer.write(data)

    def __rshift__(self, other: StreamInput[T]) -> StreamInput[T]:
        """
        Pipe operator: output >> input

        Connects this output to an input, creating a data flow path.

        Args:
            other: StreamInput to connect to

        Returns:
            The input (for chaining)

        Raises:
            TypeError: If trying to connect to non-StreamInput

        Usage:
            # Basic connection
            actor1.outputs['video'] >> actor2.inputs['video']

            # Chaining (if actor2 has output)
            actor1.outputs['video'] >> actor2.inputs['video']
            actor2.outputs['video'] >> actor3.inputs['video']

            # Or shorter:
            actor1 >> actor2 >> actor3  # If using default ports
        """
        if not isinstance(other, StreamInput):
            raise TypeError(
                f"Cannot connect StreamOutput to {type(other).__name__}.\n"
                f"Expected StreamInput, got {type(other).__name__}.\n"
                f"\n"
                f"Correct usage:\n"
                f"  source.outputs['video'] >> target.inputs['video']\n"
                f"\n"
                f"Make sure you're connecting:\n"
                f"  - outputs['port_name'] to inputs['port_name']\n"
                f"  - Not the actors themselves directly"
            )

        other.connect(self.buffer)
        self.subscribers.append(other)
        return other


class Actor(ABC):
    """
    Base class for all actors.

    Actors are independent, concurrent components that:
    - Auto-start when created (immediately begin processing)
    - Process ticks from a clock
    - Read from input ring buffers (latest-read semantics)
    - Write to output ring buffers (overwrite oldest)
    - Communicate only via ring buffers (no shared state)

    Subclasses must:
    1. Call super().__init__() with actor_id
    2. Create input/output ports in __init__
    3. Implement async def process(self, tick)

    Example:
        class VideoGenerator(Actor):
            def __init__(self):
                super().__init__('video-gen', clock=SoftwareClock(fps=60))
                self.outputs['video'] = StreamOutput('video')
                # Actor begins processing automatically

            async def process(self, tick: TimedTick):
                frame = self.generate_frame()
                self.outputs['video'].write(frame)
    """

    def __init__(
        self,
        actor_id: str,
        clock: Optional[Clock] = None,
        dispatcher: Optional[Dispatcher] = None,
        auto_register: bool = True
    ):
        """
        Initialize actor.

        The actor begins processing automatically after __init__ completes.

        Args:
            actor_id: Unique identifier for this actor
            clock: Clock source (default: SoftwareClock(60fps))
            dispatcher: Execution dispatcher (default: AsyncioDispatcher)
            auto_register: Automatically register in global registry (default: True)

        Note: Actor begins processing automatically - no manual action needed.
        """
        self.actor_id = actor_id
        self.clock = clock or SoftwareClock(fps=60.0)
        self.dispatcher = dispatcher or AsyncioDispatcher()

        # Input/output ports (populated by subclasses)
        self.inputs: Dict[str, StreamInput] = {}
        self.outputs: Dict[str, StreamOutput] = {}

        # Internal state
        self._running = False
        self._task: Optional[asyncio.Task] = None
        self._registry_uri: Optional[str] = None

        # Auto-register in global registry
        if auto_register:
            try:
                from .registry import ActorRegistry
                registry = ActorRegistry.get()
                self._registry_uri = registry.register(self)
            except Exception as e:
                # Don't fail if registration fails
                print(f"[{actor_id}] Warning: Failed to register in registry: {e}")

        # Auto-start actor (schedule processing task)
        # This happens after subclass __init__ completes
        self._schedule_start()

    def _schedule_start(self) -> None:
        """
        Schedule actor to start on next event loop iteration.

        This ensures the actor's __init__ has fully completed (including
        subclass setup) before processing begins.
        """
        def _start_callback():
            if not self._running:
                self._running = True
                self._task = asyncio.create_task(self._run())

        # Schedule start on next loop iteration
        try:
            loop = asyncio.get_running_loop()
            loop.call_soon(_start_callback)
        except RuntimeError:
            # No event loop running, start immediately
            # (This can happen during testing)
            self._running = True
            # Task will be created when event loop starts

    async def _run(self) -> None:
        """
        Internal run loop (processes ticks).

        This runs continuously until stop() is called.
        Catches and logs exceptions without stopping.
        """
        try:
            async for tick in self._tick_generator():
                if not self._running:
                    break

                try:
                    await self.process(tick)
                except Exception as e:
                    print(f"[{self.actor_id}] Error processing tick {tick.frame_number}: {e}")
                    traceback.print_exc()
                    # Continue processing (don't crash on single tick error)

        except Exception as e:
            print(f"[{self.actor_id}] Fatal error in run loop: {e}")
            traceback.print_exc()

    async def _tick_generator(self):
        """
        Generate ticks from clock.

        Yields ticks until actor is stopped.
        """
        while self._running:
            try:
                tick = await self.clock.next_tick()
                yield tick
            except Exception as e:
                print(f"[{self.actor_id}] Error getting tick: {e}")
                traceback.print_exc()
                # Continue trying (clock errors shouldn't stop actor)
                await asyncio.sleep(0.001)  # Brief pause before retry

    @abstractmethod
    async def process(self, tick: TimedTick) -> None:
        """
        Process one tick.

        Subclasses must implement this method. Called once per tick.

        Args:
            tick: Timing information for this tick

        Pattern:
            async def process(self, tick: TimedTick):
                # 1. Read latest from inputs
                input_data = self.inputs['in'].read_latest()

                # 2. Do work
                if input_data is not None:
                    result = transform(input_data)

                    # 3. Write to outputs
                    self.outputs['out'].write(result)
        """
        pass

    async def stop(self) -> None:
        """
        Stop actor (stop processing ticks).

        Waits for current tick to complete, then stops.
        Also unregisters from global registry if registered.

        Note: Usually not needed (actors run until program exit).
        """
        self._running = False
        if self._task:
            await self._task

        # Unregister from global registry
        if self._registry_uri is not None:
            try:
                from .registry import ActorRegistry
                registry = ActorRegistry.get()
                registry.unregister(self._registry_uri)
                self._registry_uri = None
            except Exception as e:
                print(f"[{self.actor_id}] Warning: Failed to unregister from registry: {e}")

    def is_running(self) -> bool:
        """
        Check if actor is running.

        Returns:
            True if running, False if stopped
        """
        return self._running

    def get_status(self) -> Dict[str, Any]:
        """
        Get actor status (for debugging/monitoring).

        Returns:
            Dictionary with status info
        """
        return {
            'actor_id': self.actor_id,
            'running': self._running,
            'clock_id': self.clock.get_clock_id(),
            'fps': self.clock.get_fps(),
            'inputs': {name: inp.is_connected() for name, inp in self.inputs.items()},
            'outputs': {name: len(out.subscribers) for name, out in self.outputs.items()},
        }

    def __repr__(self) -> str:
        """String representation for debugging."""
        status = "running" if self._running else "stopped"
        return f"<{self.__class__.__name__} id={self.actor_id} status={status}>"
