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
from typing import Dict, Optional, Set
from .handler import StreamHandler
from .stream import Stream
from .clocks import Clock, SoftwareClock
from .dispatchers import Dispatcher, AsyncioDispatcher, ThreadPoolDispatcher
from .ports import StreamOutput, StreamInput
from .transfers import CPUtoGPUTransferHandler, GPUtoCPUTransferHandler


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

    def __init__(self, fps: float = 30.0, clock: Optional[Clock] = None):
        """
        Initialize stream runtime.

        Args:
            fps: Frames per second for default software clock
            clock: Optional custom clock (defaults to SoftwareClock)

        Example:
            runtime = StreamRuntime(fps=30)
        """
        self.clock = clock or SoftwareClock(fps=fps)

        # Flat registry (all handlers are siblings)
        self.handlers: Dict[str, StreamHandler] = {}
        self.streams: Dict[str, Stream] = {}

        # Dispatcher pool
        self.dispatchers: Dict[str, Dispatcher] = {}
        self._init_dispatchers()

        # Auto-inserted transfer handlers
        self._transfer_handlers: Set[StreamHandler] = set()

        self._running = False

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

        # Activate handler (sets clock and dispatcher, starts run loop)
        handler._activate(self, self.clock, dispatcher)
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
        Connect output port to input port with capability negotiation.

        Negotiation rules:
        1. Port types must match (video→video, audio→audio)
        2. Find intersection of capabilities
        3. If intersection exists, negotiate that memory space
        4. If no intersection and auto_transfer=True, insert transfer handler
        5. If no intersection and auto_transfer=False, error

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
            RuntimeError: If no capability overlap and auto_transfer=False
        """
        # Check port type compatibility
        if output_port.port_type != input_port.port_type:
            raise TypeError(
                f"Cannot connect {output_port.port_type} output to "
                f"{input_port.port_type} input"
            )

        # Find capability intersection
        common_caps = set(output_port.capabilities) & set(input_port.capabilities)

        if common_caps:
            # Negotiate: prefer first common capability
            negotiated = list(common_caps)[0]

            output_port.negotiated_memory = negotiated
            input_port.negotiated_memory = negotiated
            input_port.connect(output_port.buffer)

            print(
                f"✅ Connected {output_port.name} → {input_port.name} "
                f"(negotiated: {negotiated})"
            )
        else:
            # No overlap - need transfer handler
            if not auto_transfer:
                raise RuntimeError(
                    f"No capability overlap between {output_port.capabilities} and "
                    f"{input_port.capabilities}. Cannot connect without transfer handler."
                )

            self._insert_transfer_handler(output_port, input_port)

    def _insert_transfer_handler(
        self,
        output_port: StreamOutput,
        input_port: StreamInput
    ) -> None:
        """
        Auto-insert transfer handler between incompatible ports.

        Determines direction (CPU→GPU or GPU→CPU) and inserts appropriate handler.

        Args:
            output_port: Source output port
            input_port: Destination input port
        """
        # Determine transfer direction
        if 'cpu' in output_port.capabilities and 'gpu' in input_port.capabilities:
            # CPU → GPU transfer
            transfer = CPUtoGPUTransferHandler()
            direction = "CPU → GPU"
        elif 'gpu' in output_port.capabilities and 'cpu' in input_port.capabilities:
            # GPU → CPU transfer
            transfer = GPUtoCPUTransferHandler()
            direction = "GPU → CPU"
        else:
            raise RuntimeError(
                f"Cannot determine transfer direction: "
                f"{output_port.capabilities} → {input_port.capabilities}"
            )

        # Add transfer handler to runtime
        transfer_stream = Stream(transfer, dispatcher='asyncio')
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
        Start runtime.

        Handlers have already been activated when added via add_stream().
        This just sets the running flag.

        Example:
            runtime.start()
        """
        self._running = True
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

        # Deactivate all handlers
        for handler in self.handlers.values():
            await handler._deactivate()

        # Shutdown dispatchers
        for dispatcher in self.dispatchers.values():
            await dispatcher.shutdown()

        print("[Runtime] Stopped")

    def __repr__(self) -> str:
        return (
            f"StreamRuntime(handlers={len(self.handlers)}, "
            f"streams={len(self.streams)}, "
            f"running={self._running})"
        )
