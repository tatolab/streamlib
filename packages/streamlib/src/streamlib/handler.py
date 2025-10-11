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
from typing import Dict, Optional, TYPE_CHECKING
from .ports import StreamInput, StreamOutput
from .clocks import TimedTick
from .dispatchers import Dispatcher

if TYPE_CHECKING:
    from .events import EventBus, ClockTickEvent


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
        preferred_dispatcher: Suggested dispatcher type (class attribute)
        _runtime: Runtime that activated this handler (internal)
        _clock: Clock provided by runtime (internal)
        _dispatcher: Dispatcher assigned by runtime (internal)
        _running: Whether handler is currently running (internal)
        _task: Async task for handler's run loop (internal)
    """

    # Class attribute: preferred dispatcher for this handler type
    # Subclasses can override to declare their dispatcher requirements
    # Options: 'asyncio', 'threadpool', 'gpu', 'processpool'
    preferred_dispatcher: str = 'asyncio'

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
        self._event_bus = None  # EventBus for tick subscription
        self._dispatcher: Optional[Dispatcher] = None
        self._running = False
        self._task: Optional[asyncio.Task] = None
        self._tick_subscription = None  # Event subscription

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
        Internal run loop - receives ticks from event bus.

        Subscribes to ClockTickEvent and processes each tick.
        All handlers receive the same tick concurrently (fixes sequential tick bug).
        """
        from .events import ClockTickEvent  # Import here to avoid circular dependency

        try:
            # Call on_start hook
            await self.on_start()

            # Subscribe to clock tick events from runtime
            self._tick_subscription = self._event_bus.subscribe(ClockTickEvent)

            # Process ticks until stopped
            async for event in self._tick_subscription:
                if not self._running:
                    break

                # Extract tick from event
                tick = event.tick

                # Process tick
                try:
                    await self.process(tick)
                except Exception as e:
                    # Propagate error to runtime via event bus (non-blocking)
                    from .events import ErrorEvent
                    self._event_bus.publish(ErrorEvent(
                        handler_id=self.handler_id,
                        exception=e,
                        tick=tick
                    ))
                    print(f"[{self.handler_id}] Error processing tick {tick.frame_number}: {e}")

        except asyncio.CancelledError:
            pass
        except Exception as e:
            print(f"[{self.handler_id}] Error in run loop: {e}")
            import traceback
            traceback.print_exc()
        finally:
            # Unsubscribe from events
            if self._tick_subscription:
                self._tick_subscription.unsubscribe()

            # Call on_stop hook
            try:
                await self.on_stop()
            except Exception as e:
                print(f"[{self.handler_id}] Error in on_stop: {e}")

    def _activate(self, runtime, event_bus, dispatcher: Dispatcher) -> None:
        """
        Activate handler - called by runtime.

        Args:
            runtime: StreamRuntime that owns this handler
            event_bus: EventBus for tick subscription and error propagation
            dispatcher: Dispatcher for execution context

        Note: This starts the handler's run loop as an async task.
        """
        if self._running:
            raise RuntimeError(f"Handler {self.handler_id} is already running")

        self._runtime = runtime
        self._event_bus = event_bus
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
