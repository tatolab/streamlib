"""
StreamRuntime - Lifecycle manager with capability negotiation.

Runtime manages handler lifecycle, provides shared clock, assigns dispatchers,
and negotiates capabilities when connecting handlers.

Inspired by Cloudflare Wrangler and GStreamer's capability negotiation.

Example:
    runtime = StreamRuntime(fps=30)

    # Add streams
    runtime.add_stream(Stream(camera_handler, dispatcher='asyncio'))
    runtime.add_stream(Stream(blur_handler, dispatcher='threadpool'))

    # Connect with capability negotiation
    runtime.connect(camera_handler.outputs['video'], blur_handler.inputs['video'])

    # Start runtime
    await runtime.start()
"""

import asyncio
from typing import Dict, Optional, Set, Any
from .handler import StreamHandler
from .stream import Stream
from .clocks import Clock, SoftwareClock, TimedTick
from .dispatchers import Dispatcher, AsyncioDispatcher, ThreadPoolDispatcher
from .ports import StreamOutput, StreamInput
from .transfers import CPUtoGPUTransferHandler, GPUtoCPUTransferHandler
from .events import EventBus, ClockTickEvent, ErrorEvent

# GPU utilities for runtime-level optimizations
try:
    from .gpu_utils import create_gpu_context
    HAS_GPU_UTILS = True
except ImportError:
    HAS_GPU_UTILS = False

# Metal transfer handlers (macOS only, optional)
try:
    from .transfers import CPUtoMetalTransferHandler, MetalToCPUTransferHandler
    HAS_METAL_TRANSFERS = True
except (ImportError, RuntimeError):
    HAS_METAL_TRANSFERS = False


class StreamRuntime:
    """
    Runtime for managing stream handlers.

    Like Cloudflare Wrangler - activates and manages inert handlers.
    Provides shared clock, assigns dispatchers, supervises lifecycle,
    and negotiates capabilities when connecting handlers.

    Attributes:
        clock: Shared clock for all handlers
        handlers: Dict of handler_id → StreamHandler
        streams: Dict of stream_id → Stream
        dispatchers: Pool of dispatcher instances
    """

    def __init__(self, fps: float = 30.0, clock: Optional[Clock] = None, enable_gpu: bool = True):
        """
        Initialize stream runtime.

        Args:
            fps: Frames per second for default software clock
            clock: Optional custom clock (defaults to SoftwareClock)
            enable_gpu: Enable GPU optimizations (auto-detect backend)

        Example:
            runtime = StreamRuntime(fps=30)
        """
        self.clock = clock or SoftwareClock(fps=fps)

        # Event bus for tick broadcast and error propagation
        self.event_bus = EventBus(buffer_size=100)

        # Flat registry (all handlers are siblings)
        self.handlers: Dict[str, StreamHandler] = {}
        self.streams: Dict[str, Stream] = {}

        # Dispatcher pool
        self.dispatchers: Dict[str, Dispatcher] = {}
        self._init_dispatchers()

        # GPU context (provides memory pooling, batching, transfer optimization)
        self.gpu_context: Optional[Dict[str, Any]] = None
        if enable_gpu and HAS_GPU_UTILS:
            try:
                self.gpu_context = create_gpu_context(backend='auto')
                print(f"[Runtime] GPU context initialized: {self.gpu_context['backend']}")
            except Exception as e:
                print(f"[Runtime] GPU context initialization failed: {e}")

        # Auto-inserted transfer handlers
        self._transfer_handlers: Set[StreamHandler] = set()

        self._running = False
        self._clock_task: Optional[asyncio.Task] = None

    def _init_dispatchers(self) -> None:
        """Initialize dispatcher pool."""
        self.dispatchers['asyncio'] = AsyncioDispatcher()
        self.dispatchers['threadpool'] = ThreadPoolDispatcher(max_workers=4)
        # GPU and ProcessPool dispatchers are stubs for now

    def add_stream(self, stream: Stream, stream_id: str = None) -> None:
        """
        Add stream and activate its handler.

        Args:
            stream: Stream configuration (handler + dispatcher + transport)
            stream_id: Optional stream ID (auto-generated if None)

        Example:
            runtime.add_stream(Stream(blur_handler, dispatcher='threadpool'))

        Note: Handler is activated immediately when added (but not started yet).
        """
        if stream_id is None:
            stream_id = f"stream-{id(stream)}"

        self.streams[stream_id] = stream
        handler = stream.handler

        # Get dispatcher instance
        if isinstance(stream.dispatcher, str):
            dispatcher = self._get_dispatcher_by_name(stream.dispatcher)
        else:
            dispatcher = stream.dispatcher

        # Activate handler (passes event bus for tick subscription)
        handler._activate(self, self.event_bus, dispatcher)
        self.handlers[handler.handler_id] = handler

        print(f"[Runtime] Added stream '{stream_id}' with handler '{handler.handler_id}'")

    def _get_dispatcher_by_name(self, name: str) -> Dispatcher:
        """
        Get dispatcher instance by name.

        Args:
            name: Dispatcher name ('asyncio', 'threadpool', etc.)

        Returns:
            Dispatcher instance

        Raises:
            ValueError: If dispatcher name not found
        """
        if name not in self.dispatchers:
            raise ValueError(
                f"Dispatcher '{name}' not found. Available: {list(self.dispatchers.keys())}"
            )
        return self.dispatchers[name]

    def connect(
        self,
        output_port: StreamOutput,
        input_port: StreamInput,
        auto_transfer: bool = True
    ) -> None:
        """
        Connect output port to input port (GPU-first by default).

        Connection rules:
        1. Port types must match (video→video, audio→audio)
        2. GPU by default - most connections just work
        3. If CPU↔GPU mismatch, auto-insert transfer (rare, with warning)
        4. Runtime handles all memory management automatically

        Args:
            output_port: Output port from source handler
            input_port: Input port from destination handler
            auto_transfer: Auto-insert transfer handlers (default: True)

        Example:
            runtime.connect(
                camera.outputs['video'],
                blur.inputs['video']
            )

        Raises:
            TypeError: If port types don't match
            RuntimeError: If memory space mismatch and auto_transfer=False
        """
        # Check port type compatibility
        if output_port.port_type != input_port.port_type:
            raise TypeError(
                f"Cannot connect {output_port.port_type} output to "
                f"{input_port.port_type} input"
            )

        # GPU-first: most connections just work
        if not output_port.cpu_only and not input_port.cpu_only:
            # Both ports are GPU (default) - direct connection
            input_port.connect(output_port.buffer)
            print(f"✅ Connected {output_port.name} → {input_port.name} (GPU)")
            return

        # Rare case: CPU involved
        if output_port.cpu_only and input_port.cpu_only:
            # Both CPU - direct connection
            input_port.connect(output_port.buffer)
            print(f"✅ Connected {output_port.name} → {input_port.name} (CPU)")
            return

        # Very rare: GPU↔CPU transfer needed
        if not auto_transfer:
            raise TypeError(
                f"Memory space mismatch: output is "
                f"{'CPU' if output_port.cpu_only else 'GPU'}, input is "
                f"{'CPU' if input_port.cpu_only else 'GPU'}. "
                f"Set auto_transfer=True to allow automatic transfer."
            )

        # Auto-insert transfer handler
        self._insert_transfer_handler(output_port, input_port)

    def _insert_transfer_handler(
        self,
        output_port: StreamOutput,
        input_port: StreamInput
    ) -> None:
        """
        Auto-insert transfer handler (rare case).

        Inserts CPU↔GPU transfer when memory spaces don't match.
        Warns user about performance cost to encourage GPU-first design.

        Args:
            output_port: Source output port
            input_port: Destination input port
        """
        # Determine transfer direction
        if output_port.cpu_only:
            # CPU → GPU transfer
            print(
                f"⚠️  WARNING: Auto-inserting CPU→GPU transfer "
                f"for {output_port.port_type} (performance cost ~2ms). "
                f"Consider making entire pipeline GPU-first."
            )
            transfer = CPUtoGPUTransferHandler()
            direction = "CPU → GPU"
        else:
            # GPU → CPU transfer
            print(
                f"⚠️  WARNING: Auto-inserting GPU→CPU transfer "
                f"for {output_port.port_type} (performance cost ~2ms). "
                f"Consider making entire pipeline GPU-first."
            )
            transfer = GPUtoCPUTransferHandler()
            direction = "GPU → CPU"

        # Add transfer handler to runtime (dispatcher inferred automatically)
        transfer_stream = Stream(transfer, dispatcher='threadpool')
        self.add_stream(transfer_stream)
        self._transfer_handlers.add(transfer)

        # Connect: output → transfer.input
        self.connect(output_port, transfer.inputs['in'], auto_transfer=False)

        # Connect: transfer.output → input
        self.connect(transfer.outputs['out'], input_port, auto_transfer=False)

        print(
            f"⚠️  Auto-inserted {direction} transfer: "
            f"{output_port.name} → [{transfer.handler_id}] → {input_port.name}"
        )

    def start(self) -> None:
        """
        Start runtime with central clock loop.

        Starts clock loop that broadcasts ticks to all handlers concurrently.

        Example:
            runtime.start()
        """
        self._running = True

        # Start central clock loop (broadcasts ticks to all handlers)
        self._clock_task = asyncio.create_task(self._clock_loop())

        print(f"[Runtime] Started with {len(self.handlers)} handlers")

    async def run(self) -> None:
        """
        Run until stopped or interrupted.

        Example:
            await runtime.run()
        """
        self.start()
        try:
            while self._running:
                await asyncio.sleep(1)
        except KeyboardInterrupt:
            print("\n[Runtime] Interrupted")
        finally:
            await self.stop()

    async def stop(self) -> None:
        """
        Stop all handlers and dispatchers.

        Example:
            await runtime.stop()
        """
        print("[Runtime] Stopping...")
        self._running = False

        # Stop clock loop
        if self._clock_task:
            self._clock_task.cancel()
            try:
                await self._clock_task
            except asyncio.CancelledError:
                pass

        # Deactivate all handlers
        for handler in self.handlers.values():
            await handler._deactivate()

        # Clear event bus
        await self.event_bus.clear()

        # Shutdown dispatchers
        for dispatcher in self.dispatchers.values():
            await dispatcher.shutdown()

        # Clean up GPU resources
        if self.gpu_context and 'memory_pool' in self.gpu_context:
            self.gpu_context['memory_pool'].clear()

        print("[Runtime] Stopped")

    async def _clock_loop(self) -> None:
        """
        Central clock loop - broadcasts ticks to all handlers.

        This fixes the sequential tick bug by broadcasting the same tick
        to all handlers concurrently via the event bus.

        Each tick is generated by the runtime clock and published to the
        event bus. All handlers receive the same tick and process it
        concurrently.
        """
        try:
            # Reset clock to start now (fixes start_time initialization bug)
            if hasattr(self.clock, 'reset'):
                self.clock.reset()

            while self._running:
                # Generate next tick from runtime clock
                tick = await self.clock.next_tick()

                # Broadcast to ALL handlers simultaneously (non-blocking)
                self.event_bus.publish(ClockTickEvent(tick))

        except asyncio.CancelledError:
            pass
        except Exception as e:
            print(f"[Runtime] Clock loop error: {e}")
            import traceback
            traceback.print_exc()

    def __repr__(self) -> str:
        return (
            f"StreamRuntime(handlers={len(self.handlers)}, "
            f"streams={len(self.streams)}, "
            f"running={self._running})"
        )
