"""
Drawing handler for procedural graphics with Python code execution.

Executes user-provided Python code with drawing context.
"""

import numpy as np
from typing import Dict, Any, Optional, Callable

from streamlib.handler import StreamHandler
from streamlib.ports import VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick

# Import drawing backend (cv2 preferred, fallback to basic numpy)
try:
    import cv2
    HAS_CV2 = True
except ImportError:
    HAS_CV2 = False


class DrawingContext:
    """
    Context object passed to drawing code.

    Provides:
    - time: Current timestamp (seconds)
    - frame_number: Current frame index
    - width, height: Canvas dimensions
    - draw: Drawing primitives (rectangle, circle, line, text)
    - variables: Custom user variables
    """

    def __init__(
        self,
        canvas: np.ndarray,
        time: float,
        frame_number: int,
        variables: Dict[str, Any]
    ):
        self.canvas = canvas
        self.time = time
        self.frame_number = frame_number
        self.width = canvas.shape[1]
        self.height = canvas.shape[0]
        self.variables = variables

        # Drawing primitives
        if HAS_CV2:
            self.rectangle = self._cv2_rectangle
            self.circle = self._cv2_circle
            self.line = self._cv2_line
            self.text = self._cv2_text
        else:
            # Fallback: no-op stubs
            self.rectangle = lambda *args, **kwargs: None
            self.circle = lambda *args, **kwargs: None
            self.line = lambda *args, **kwargs: None
            self.text = lambda *args, **kwargs: None

    def _cv2_rectangle(self, x: int, y: int, w: int, h: int, color: tuple, thickness: int = -1):
        """Draw rectangle. color=(R, G, B), thickness=-1 for filled."""
        color_bgr = (color[2], color[1], color[0])  # RGB → BGR
        cv2.rectangle(self.canvas, (x, y), (x + w, y + h), color_bgr, thickness)

    def _cv2_circle(self, x: int, y: int, radius: int, color: tuple, thickness: int = -1):
        """Draw circle. color=(R, G, B), thickness=-1 for filled."""
        color_bgr = (color[2], color[1], color[0])  # RGB → BGR
        cv2.circle(self.canvas, (x, y), radius, color_bgr, thickness)

    def _cv2_line(self, x1: int, y1: int, x2: int, y2: int, color: tuple, thickness: int = 1):
        """Draw line. color=(R, G, B)."""
        color_bgr = (color[2], color[1], color[0])  # RGB → BGR
        cv2.line(self.canvas, (x1, y1), (x2, y2), color_bgr, thickness)

    def _cv2_text(
        self,
        text: str,
        x: int,
        y: int,
        color: tuple,
        font_scale: float = 1.0,
        thickness: int = 2
    ):
        """Draw text. color=(R, G, B)."""
        color_bgr = (color[2], color[1], color[0])  # RGB → BGR
        cv2.putText(
            self.canvas,
            text,
            (x, y),
            cv2.FONT_HERSHEY_SIMPLEX,
            font_scale,
            color_bgr,
            thickness
        )


class DrawingHandler(StreamHandler):
    """
    Execute Python drawing code to generate procedural graphics.

    User provides Python code that receives a DrawingContext and draws on canvas.

    Capabilities: ['cpu'] - generates numpy arrays

    Example:
        ```python
        def my_drawing(ctx):
            # Animated circle
            x = int(ctx.width/2 + 100*np.sin(ctx.time))
            y = int(ctx.height/2)
            ctx.circle(x, y, 50, color=(255, 0, 0))

        drawing = DrawingHandler(
            width=640,
            height=480,
            draw_func=my_drawing
        )
        runtime.add_stream(Stream(drawing, dispatcher='asyncio'))
        ```
    """

    def __init__(
        self,
        width: int = 640,
        height: int = 480,
        draw_func: Optional[Callable[[DrawingContext], None]] = None,
        background_color: tuple = (0, 0, 0),
        variables: Dict[str, Any] = None,
        handler_id: str = None
    ):
        """
        Initialize drawing handler.

        Args:
            width: Canvas width
            height: Canvas height
            draw_func: Function that receives DrawingContext and draws
            background_color: RGB background color (default: black)
            variables: Custom variables available in ctx.variables
            handler_id: Optional custom handler ID
        """
        super().__init__(handler_id or 'drawing')

        self.width = width
        self.height = height
        self.draw_func = draw_func or self._default_draw
        self.background_color = background_color
        self.variables = variables or {}

        # Output: CPU-only
        self.outputs['video'] = VideoOutput('video')

        # Frame counter
        self._frame_count = 0

    def _default_draw(self, ctx: DrawingContext) -> None:
        """Default drawing: animated bouncing circle."""
        import math

        # Bouncing circle
        x = int(ctx.width / 2 + 200 * math.sin(ctx.time))
        y = int(ctx.height / 2 + 150 * math.cos(ctx.time * 0.7))

        ctx.circle(x, y, 50, color=(255, 255, 0))  # Yellow circle

        # Frame counter
        ctx.text(f"Frame: {ctx.frame_number}", 10, 30, color=(255, 255, 255))

    async def process(self, tick: TimedTick) -> None:
        """
        Generate one frame by executing drawing code.

        Creates blank canvas, executes draw_func, outputs result.
        """
        # Create blank canvas
        canvas = np.full((self.height, self.width, 3), self.background_color, dtype=np.uint8)

        # Create drawing context
        ctx = DrawingContext(
            canvas=canvas,
            time=tick.timestamp,
            frame_number=self._frame_count,
            variables=self.variables
        )

        # Execute drawing code
        try:
            self.draw_func(ctx)
        except Exception as e:
            print(f"[DrawingHandler] Error in draw_func: {e}")
            # Continue with blank/partially drawn canvas

        # Create output frame
        frame = VideoFrame(
            data=canvas,
            timestamp=tick.timestamp,
            frame_number=self._frame_count,
            width=self.width,
            height=self.height,
            metadata={'drawing': True}
        )

        # Write to output
        self.outputs['video'].write(frame)
        self._frame_count += 1

    async def on_start(self) -> None:
        """Called when handler starts."""
        print(f"DrawingHandler started: {self.width}x{self.height}, draw_func={self.draw_func.__name__}")

    async def on_stop(self) -> None:
        """Called when handler stops."""
        print(f"DrawingHandler stopped: {self._frame_count} frames drawn")
