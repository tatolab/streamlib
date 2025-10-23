"""
StreamRuntime - WebGPU-first lifecycle manager.

Runtime manages handler lifecycle, provides shared WebGPU context,
and connects handlers with zero-copy GPU pipelines.

No dispatchers, no capability negotiation - everything runs on GPU.

Example:
    runtime = StreamRuntime(fps=30)

    # Add streams
    runtime.add_stream(Stream(camera_handler))
    runtime.add_stream(Stream(blur_handler))

    # Connect - all GPU, zero-copy
    runtime.connect(camera_handler.outputs['video'], blur_handler.inputs['video'])

    # Start runtime
    await runtime.start()
"""

import asyncio
from typing import Dict, Optional, Set
from .handler import StreamHandler
from .stream import Stream
from .clocks import Clock, SoftwareClock
from .ports import StreamOutput, StreamInput
from .events import EventBus, ClockTickEvent

# WebGPU support for shared GPU context
try:
    from .gpu import GPUContext
    HAS_WEBGPU = True
except ImportError:
    HAS_WEBGPU = False


class StreamRuntime:
    """
    Runtime for managing stream handlers with WebGPU-first architecture.

    Provides shared WebGPU context, manages handler lifecycle,
    and connects handlers with zero-copy GPU pipelines.

    Attributes:
        clock: Shared clock for all handlers
        handlers: Dict of handler_id → StreamHandler
        gpu_context: Shared WebGPU context for all handlers
    """

    def __init__(
        self,
        fps: float = 30.0,
        clock: Optional[Clock] = None,
        enable_gpu: bool = True,
        width: int = 1920,
        height: int = 1080
    ):
        """
        Initialize stream runtime.

        Args:
            fps: Frames per second for default software clock
            clock: Optional custom clock (defaults to SoftwareClock)
            enable_gpu: Enable GPU optimizations (auto-detect backend)
            width: Default frame width for video handlers
            height: Default frame height for video handlers

        Example:
            runtime = StreamRuntime(fps=30, width=1920, height=1080)
        """
        self.clock = clock or SoftwareClock(fps=fps)

        # Frame dimensions (used by camera, display, etc.)
        self.width = width
        self.height = height

        # Event bus for tick broadcast and error propagation
        self.event_bus = EventBus(buffer_size=100)

        # Flat registry (all handlers are siblings)
        self.handlers: Dict[str, StreamHandler] = {}
        self.streams: Dict[str, Stream] = {}

        # Shared WebGPU context (created async in start())
        # All handlers share this single context for zero-copy GPU operations
        self.gpu_context: Optional['GPUContext'] = None
        self._enable_gpu = enable_gpu

        self._running = False
        self._clock_task: Optional[asyncio.Task] = None

    def add_stream(self, stream: Stream, stream_id: str = None) -> None:
        """
        Add stream and activate its handler.

        Args:
            stream: Stream configuration (handler + optional transport)
            stream_id: Optional stream ID (auto-generated if None)

        Example:
            runtime.add_stream(Stream(blur_handler))

        Note: Handler is activated immediately when added (but not started yet).
        """
        if stream_id is None:
            stream_id = f"stream-{id(stream)}"

        self.streams[stream_id] = stream
        handler = stream.handler

        # Activate handler with WebGPU context (no dispatcher needed)
        handler._activate(self, self.event_bus, None)
        self.handlers[handler.handler_id] = handler

        print(f"[Runtime] Added stream '{stream_id}' with handler '{handler.handler_id}'")

    def connect(
        self,
        output_port: StreamOutput,
        input_port: StreamInput
    ) -> None:
        """
        Connect output port to input port (WebGPU-only).

        All connections are zero-copy GPU texture references.
        No capability negotiation needed - everything is WebGPU.

        Args:
            output_port: Output port from source handler
            input_port: Input port from destination handler

        Example:
            runtime.connect(
                camera.outputs['video'],
                blur.inputs['video']
            )

        Raises:
            TypeError: If port types don't match
        """
        # Check port type compatibility
        if output_port.port_type != input_port.port_type:
            raise TypeError(
                f"Cannot connect {output_port.port_type} output to "
                f"{input_port.port_type} input"
            )

        # Direct connection - all WebGPU, zero-copy
        input_port.connect(output_port.buffer)
        print(f"✅ Connected {output_port.name} → {input_port.name} (WebGPU)")

    async def start(self) -> None:
        """
        Start runtime with central clock loop.

        Creates shared WebGPU context and starts clock loop
        that broadcasts ticks to all handlers concurrently.

        Example:
            await runtime.start()
        """
        # Create shared WebGPU context for all handlers
        if self._enable_gpu and HAS_WEBGPU:
            try:
                self.gpu_context = await GPUContext.create(power_preference='high-performance')

                # Store runtime dimensions on GPU context for camera/display
                self.gpu_context._runtime_width = self.width
                self.gpu_context._runtime_height = self.height

                print(f"[Runtime] Shared GPU context created: {self.gpu_context.backend_name}")
                print(f"[Runtime] Frame dimensions: {self.width}x{self.height}")
            except Exception as e:
                print(f"[Runtime] Warning: Failed to create GPU context: {e}")
                print(f"[Runtime] GPU handlers will fail if they require GPU context")

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
        await self.start()
        try:
            while self._running:
                await asyncio.sleep(1)
        except KeyboardInterrupt:
            print("\n[Runtime] Interrupted")
        finally:
            await self.stop()

    async def stop(self) -> None:
        """
        Stop all handlers and clean up.

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

        # Clean up shared WebGPU context
        if self.gpu_context:
            # WebGPU contexts are automatically cleaned up by wgpu
            # but we can explicitly release the reference
            self.gpu_context = None

        print("[Runtime] Stopped")

    async def _clock_loop(self) -> None:
        """
        Central clock loop - broadcasts ticks to all handlers.

        Each tick is generated by the runtime clock and published to the
        event bus. All handlers receive the same tick and process it
        concurrently.
        """
        try:
            # Reset clock to start now
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