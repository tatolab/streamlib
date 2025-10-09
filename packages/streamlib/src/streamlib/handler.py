"""
StreamHandler base class for stream processing.

Handlers are pure processing logic - inert until StreamRuntime activates them.
They implement async def process(tick) to process each clock tick.

Handlers are reusable across different execution contexts (dispatchers).
Runtime provides clock, dispatcher, and lifecycle management.

Example:
    class BlurFilter(StreamHandler):
        def __init__(self):
            super().__init__()
            self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
            self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])

        async def process(self, tick: TimedTick):
            frame = self.inputs['video'].read_latest()
            if frame:
                blurred = cv2.GaussianBlur(frame.data, (5, 5), 0)
                self.outputs['video'].write(VideoFrame(blurred, tick.timestamp, ...))
"""

import asyncio
from abc import ABC, abstractmethod
from typing import Dict, Optional
from .ports import StreamInput, StreamOutput
from .clocks import Clock, TimedTick
from .dispatchers import Dispatcher


class StreamHandler(ABC):
    """
    Base class for all stream handlers.

    Handlers are INERT until StreamRuntime activates them.
    No auto-start, no self-managed lifecycle.

    Lifecycle:
        1. User creates handler: handler = BlurFilter()
        2. User wraps in Stream: stream = Stream(handler, dispatcher='asyncio')
        3. User adds to runtime: runtime.add_stream(stream)
        4. Runtime activates handler: handler._activate(runtime, clock, dispatcher)
        5. Handler processes ticks: await handler.process(tick)
        6. Runtime deactivates: await handler._deactivate()

    Attributes:
        handler_id: Unique identifier for this handler
        inputs: Dictionary of input ports
        outputs: Dictionary of output ports
        _runtime: Runtime that activated this handler (internal)
        _clock: Clock provided by runtime (internal)
        _dispatcher: Dispatcher assigned by runtime (internal)
        _running: Whether handler is currently running (internal)
        _task: Async task for handler's run loop (internal)
    """

    def __init__(self, handler_id: str = None):
        """
        Initialize handler.

        Args:
            handler_id: Optional unique identifier. If None, auto-generated.

        Note: Subclasses should call super().__init__() and then create their
        input/output ports in __init__.
        """
        self.handler_id = handler_id or f"{self.__class__.__name__}-{id(self)}"

        # Ports (populated by subclasses)
        self.inputs: Dict[str, StreamInput] = {}
        self.outputs: Dict[str, StreamOutput] = {}

        # Runtime-managed (not user-accessible)
        self._runtime = None
        self._clock: Optional[Clock] = None
        self._dispatcher: Optional[Dispatcher] = None
        self._running = False
        self._task: Optional[asyncio.Task] = None

    @abstractmethod
    async def process(self, tick: TimedTick) -> None:
        """
        Process one clock tick.

        This is called by runtime for each clock tick. Handlers should:
        1. Read latest data from inputs using read_latest() (zero-copy)
        2. Process the data
        3. Write results to outputs using write() (zero-copy)

        Args:
            tick: TimedTick with timestamp, frame_number, clock_source_id

        Example:
            async def process(self, tick: TimedTick):
                frame = self.inputs['video'].read_latest()
                if frame:
                    # Process frame
                    result = self.do_something(frame.data)

                    # Write result
                    self.outputs['video'].write(VideoFrame(
                        data=result,
                        timestamp=tick.timestamp,
                        frame_number=tick.frame_number,
                        width=frame.width,
                        height=frame.height
                    ))

        Note: This method is async to allow handlers to await I/O operations
        if needed, but most handlers should be CPU-bound and return quickly.
        """
        pass

    # Optional lifecycle hooks

    async def on_start(self) -> None:
        """
        Called once when runtime starts this handler.

        Use this to initialize resources (open files, create windows, etc.).

        Optional - implement if needed.
        """
        pass

    async def on_stop(self) -> None:
        """
        Called once when runtime stops this handler.

        Use this to clean up resources (close files, destroy windows, etc.).

        Optional - implement if needed.
        """
        pass

    # Internal methods (called by StreamRuntime only)

    async def _run(self) -> None:
        """
        Internal run loop - processes ticks from clock.

        Called by runtime after activation. Runs until _running is set to False.
        """
        try:
            # Call on_start hook
            await self.on_start()

            # Process ticks until stopped
            async for tick in self._tick_generator():
                if not self._running:
                    break
                await self.process(tick)

        except Exception as e:
            print(f"[{self.handler_id}] Error in run loop: {e}")
            import traceback
            traceback.print_exc()
        finally:
            # Call on_stop hook
            try:
                await self.on_stop()
            except Exception as e:
                print(f"[{self.handler_id}] Error in on_stop: {e}")

    async def _tick_generator(self):
        """
        Generate ticks from runtime clock.

        Yields ticks until handler is stopped.
        """
        while self._running:
            try:
                tick = await self._clock.next_tick()
                yield tick
            except Exception as e:
                print(f"[{self.handler_id}] Error getting tick: {e}")
                break

    def _activate(self, runtime, clock: Clock, dispatcher: Dispatcher) -> None:
        """
        Activate handler - called by runtime.

        Args:
            runtime: StreamRuntime that owns this handler
            clock: Clock to use for ticks
            dispatcher: Dispatcher for execution context

        Note: This starts the handler's run loop as an async task.
        """
        if self._running:
            raise RuntimeError(f"Handler {self.handler_id} is already running")

        self._runtime = runtime
        self._clock = clock
        self._dispatcher = dispatcher
        self._running = True

        # Start run loop via dispatcher
        self._task = asyncio.create_task(self._run())

        print(f"[{self.handler_id}] Activated")

    async def _deactivate(self) -> None:
        """
        Deactivate handler - called by runtime.

        Stops the run loop and waits for completion.
        """
        if not self._running:
            return

        print(f"[{self.handler_id}] Deactivating...")
        self._running = False

        # Wait for run loop to finish
        if self._task:
            try:
                await asyncio.wait_for(self._task, timeout=5.0)
            except asyncio.TimeoutError:
                print(f"[{self.handler_id}] Deactivation timeout, cancelling task")
                self._task.cancel()
                try:
                    await self._task
                except asyncio.CancelledError:
                    pass

        print(f"[{self.handler_id}] Deactivated")

    def __repr__(self) -> str:
        return f"StreamHandler(id='{self.handler_id}', inputs={list(self.inputs.keys())}, outputs={list(self.outputs.keys())})"
