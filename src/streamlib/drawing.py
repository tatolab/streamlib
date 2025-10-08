"""
Drawing layer implementation using Skia.

This module provides a layer that executes Python drawing code to generate
visual overlays on video frames.
"""

import skia
import numpy as np
from numpy.typing import NDArray
from typing import Any, Dict, Optional
import traceback
from .base import Layer, TimestampedFrame
from .plugins import register_layer


class DrawingContext:
    """
    Context object passed to draw functions.

    Attributes:
        frame: Current video frame as numpy array (if available)
        time: Current time in seconds
        frame_number: Current frame number
        width: Output width
        height: Output height
        custom: Dictionary for user-defined custom variables
    """

    def __init__(self):
        self.frame: Optional[NDArray[np.uint8]] = None
        self.time: float = 0.0
        self.frame_number: int = 0
        self.width: int = 0
        self.height: int = 0
        self.custom: Dict[str, Any] = {}


@register_layer('drawing')
class DrawingLayer(Layer):
    """
    A layer that executes Python drawing code using Skia.

    The drawing code must define a `draw(canvas, ctx)` function that
    receives a Skia canvas and a DrawingContext object.

    Example drawing code:
        ```python
        def draw(canvas, ctx):
            paint = skia.Paint()
            paint.setColor(skia.Color(255, 0, 0))
            canvas.drawCircle(ctx.width / 2, ctx.height / 2, 100, paint)
        ```
    """

    # Shared GPU context for all drawing layers
    _gpu_context = None
    _gpu_context_initialized = False

    @classmethod
    def _get_gpu_context(cls):
        """Get or create shared GPU context."""
        if not cls._gpu_context_initialized:
            try:
                # Try OpenGL first (more compatible than Metal via Python bindings)
                cls._gpu_context = skia.GrDirectContext.MakeGL()
                if cls._gpu_context:
                    print(f"[DrawingLayer] GPU context created successfully (OpenGL)")
                else:
                    print(f"[DrawingLayer] GPU context creation returned None, using CPU")
            except Exception as e:
                print(f"[DrawingLayer] GPU context error: {e}, using CPU")
                cls._gpu_context = None
            cls._gpu_context_initialized = True
        return cls._gpu_context

    def __init__(
        self,
        name: str,
        draw_code: str,
        z_index: int = 0,
        visible: bool = True,
        opacity: float = 1.0,
        width: int = 1920,
        height: int = 1080,
        use_gpu: bool = False  # GPU not available in Python bindings, CPU is faster
    ):
        """
        Initialize drawing layer.

        Args:
            name: Unique layer name
            draw_code: Python code defining a draw(canvas, ctx) function
            z_index: Layer ordering
            visible: Whether layer is visible
            opacity: Layer opacity (0.0 to 1.0)
            width: Layer width
            height: Layer height
            use_gpu: Whether to use GPU acceleration (default: False, GPU->CPU transfer overhead makes CPU faster)
        """
        super().__init__(name, z_index, visible, opacity)
        self.width = width
        self.height = height
        self.draw_function = None
        self.context = DrawingContext()
        self.code_namespace = {}
        self.use_gpu = use_gpu
        self.set_draw_code(draw_code)

    def set_draw_code(self, code: str) -> Dict[str, Any]:
        """
        Compile and set drawing code.

        Args:
            code: Python code defining a draw(canvas, ctx) function

        Returns:
            Dictionary with status and error (if any)
        """
        try:
            # Create namespace with available modules
            namespace = {
                'skia': skia,
                'np': np,
                'numpy': np,
            }

            # Execute the code to get the draw function
            exec(code, namespace)

            if 'draw' not in namespace:
                return {
                    "status": "error",
                    "error": "Code must define a 'draw(canvas, ctx)' function"
                }

            self.draw_function = namespace['draw']
            self.code_namespace = namespace

            return {"status": "success"}

        except Exception as e:
            return {
                "status": "error",
                "error": f"Failed to compile code: {str(e)}\n{traceback.format_exc()}"
            }

    def update_context(self, **kwargs) -> None:
        """
        Update custom context variables.

        Args:
            **kwargs: Custom variables to add to context

        Example:
            layer.update_context(score=100, message="Hello")
        """
        self.context.custom.update(kwargs)

    async def process_frame(
        self,
        input_frame: Optional[TimestampedFrame],
        width: int,
        height: int
    ) -> NDArray[np.uint8]:
        """
        Render the drawing layer.

        Args:
            input_frame: Optional input frame (available in ctx.frame)
            width: Target width
            height: Target height

        Returns:
            RGBA numpy array of the rendered overlay
        """
        if self.draw_function is None:
            # Return transparent frame
            return np.zeros((height, width, 4), dtype=np.uint8)

        try:
            # Update context
            self.context.frame = input_frame.frame if input_frame else None
            self.context.time = input_frame.timestamp if input_frame else 0.0
            self.context.frame_number = input_frame.frame_number if input_frame else 0
            self.context.width = width
            self.context.height = height

            # Copy custom attributes to context
            for key, value in self.context.custom.items():
                setattr(self.context, key, value)

            # Create Skia surface (CPU is faster than GPU due to transfer overhead)
            surface = skia.Surface(width, height)
            canvas = surface.getCanvas()

            # Clear to transparent
            canvas.clear(skia.Color(0, 0, 0, 0))

            # Call draw function
            self.draw_function(canvas, self.context)

            # Get image as numpy array
            image = surface.makeImageSnapshot()
            array = image.toarray()

            return array

        except Exception as e:
            print(f"Error in draw function: {e}")
            traceback.print_exc()
            # Return transparent frame on error
            return np.zeros((height, width, 4), dtype=np.uint8)


class VideoLayer(Layer):
    """
    A simple pass-through layer that displays a video frame.

    This is useful for displaying camera feeds or video files as layers
    that can be positioned and composited with other layers.
    """

    def __init__(
        self,
        name: str,
        z_index: int = 0,
        visible: bool = True,
        opacity: float = 1.0
    ):
        """
        Initialize video layer.

        Args:
            name: Unique layer name
            z_index: Layer ordering
            visible: Whether layer is visible
            opacity: Layer opacity (0.0 to 1.0)
        """
        super().__init__(name, z_index, visible, opacity)

    async def process_frame(
        self,
        input_frame: Optional[TimestampedFrame],
        width: int,
        height: int
    ) -> NDArray[np.uint8]:
        """
        Pass through the input frame as RGBA.

        Args:
            input_frame: Input frame
            width: Target width
            height: Target height

        Returns:
            RGBA numpy array
        """
        if input_frame is None:
            return np.zeros((height, width, 4), dtype=np.uint8)

        frame = input_frame.frame

        # Resize if needed
        if frame.shape[0] != height or frame.shape[1] != width:
            import cv2
            frame = cv2.resize(frame, (width, height))

        # Convert to RGBA if needed
        if frame.shape[2] == 3:
            # RGB to RGBA
            alpha = np.ones((height, width, 1), dtype=np.uint8) * 255
            frame = np.concatenate([frame, alpha], axis=2)
        elif frame.shape[2] == 4:
            # Already RGBA
            pass
        else:
            raise ValueError(f"Unsupported frame shape: {frame.shape}")

        return frame
