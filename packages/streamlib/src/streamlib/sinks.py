"""
Sink handlers for streamlib.

This module provides reusable sink handlers that consume streaming data.
Sinks have only input ports (no outputs).

For simple use cases, prefer the @display_sink decorator from streamlib.decorators.
Use these classes when you need:
- Custom subclassing
- Explicit control over initialization
- Direct instantiation without decorators

Example:
    from streamlib import StreamRuntime, Stream, DisplaySink

    # Simple display sink
    display = DisplaySink(title="My Stream")

    runtime = StreamRuntime(fps=30, width=1920, height=1080)
    runtime.add_stream(Stream(display))
    runtime.connect(camera.outputs['video'], display.inputs['video'])
    await runtime.start()
"""

from typing import Optional
from .handler import StreamHandler
from .ports import VideoInput
from .clocks import TimedTick


class DisplaySink(StreamHandler):
    """
    Display sink handler - renders VideoFrames to a window.

    This is a simple, reusable display sink that creates a window and renders
    incoming VideoFrames directly to the swapchain. All rendering is GPU-only
    with zero CPU copies.

    Zero-copy pipeline:
        WebGPU texture → Swapchain → Display (no CPU transfer)

    Args:
        width: Window width (None = use runtime width)
        height: Window height (None = use runtime height)
        title: Window title
        handler_id: Optional handler ID (defaults to "display_sink")

    Attributes:
        inputs['video']: VideoInput port that receives VideoFrames
        display: DisplayWindow instance (created on start)

    Example:
        # Basic usage
        display = DisplaySink(title="Camera Feed")
        runtime = StreamRuntime(fps=30, width=1920, height=1080)
        runtime.add_stream(Stream(camera))
        runtime.add_stream(Stream(display))
        runtime.connect(camera.outputs['video'], display.inputs['video'])
        await runtime.start()

        # Custom dimensions
        display = DisplaySink(width=1280, height=720, title="Preview")

        # Custom subclass
        class MyDisplay(DisplaySink):
            async def on_start(self):
                await super().on_start()
                print(f"Display opened: {self.display.width}x{self.display.height}")

            async def process(self, tick):
                frame = self.inputs['video'].read_latest()
                if frame:
                    # Add custom processing before display
                    self.display.render(frame.data)
    """

    def __init__(
        self,
        width: Optional[int] = None,
        height: Optional[int] = None,
        title: str = "streamlib Display",
        handler_id: Optional[str] = None
    ):
        """
        Initialize display sink.

        Args:
            width: Window width (None = use runtime width)
            height: Window height (None = use runtime height)
            title: Window title
            handler_id: Optional handler ID (defaults to "display_sink")
        """
        super().__init__(handler_id=handler_id or "display_sink")
        self.display_width = width
        self.display_height = height
        self.display_title = title
        self.display = None

        # Create input port only (sinks have no outputs)
        self.inputs['video'] = VideoInput('video')

    async def on_start(self) -> None:
        """
        Create display window when handler starts.

        This is called by the runtime after the handler is activated.
        Creates a display window using the GPU context.

        On macOS: Uses Cocoa window with Metal swapchain
        On Linux: Uses GLFW/X11 with Vulkan swapchain
        On Windows: Uses Win32 with D3D12 swapchain
        """
        # Create display window (zero-copy rendering via swapchain)
        self.display = self._runtime.gpu_context.create_display(
            width=self.display_width,
            height=self.display_height,
            title=self.display_title
        )

    async def process(self, tick: TimedTick) -> None:
        """
        Read incoming frame and render to display.

        Called by the runtime for each clock tick. Reads the latest frame
        from the input port and renders it to the display window.

        Args:
            tick: Timed tick from the runtime clock
        """
        if self.display is None:
            return

        try:
            # Read latest frame (zero-copy from ring buffer)
            frame = self.inputs['video'].read_latest()

            if frame is not None:
                # Render to display (zero-copy to swapchain)
                self.display.render(frame.data)

        except Exception as e:
            print(f"[{self.handler_id}] Error in display sink: {e}")
            import traceback
            traceback.print_exc()

    async def on_stop(self) -> None:
        """
        Close display window when handler stops.

        This is called by the runtime when the handler is deactivated.
        Cleans up the display window resources.
        """
        if self.display:
            self.display.close()
            self.display = None

    def __repr__(self) -> str:
        return f"DisplaySink(width={self.display_width}, height={self.display_height}, title={self.display_title!r})"


__all__ = [
    'DisplaySink',
]
