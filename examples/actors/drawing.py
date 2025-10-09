"""
Reference drawing actor implementation.

This is an EXAMPLE showing how to build a drawing actor using Skia.
Not part of streamlib core - use as a starting point for your own implementation.

Shows:
- Dynamic code execution pattern
- Skia integration
- Drawing context management

Dependencies:
- skia-python (for 2D graphics)
- numpy (for frame data)
"""

import skia
import numpy as np
from numpy.typing import NDArray
from typing import Optional, Dict, Any
import traceback

from streamlib import Actor, StreamOutput
from streamlib import SoftwareClock, TimedTick
from streamlib import VideoFrame


class DrawingContext:
    """
    Context object passed to draw functions.

    Attributes:
        time: Current time in seconds
        frame_number: Current frame number
        width: Output width
        height: Output height
        custom: Dictionary for user-defined custom variables
    """

    def __init__(self):
        self.time: float = 0.0
        self.frame_number: int = 0
        self.width: int = 0
        self.height: int = 0
        self.custom: Dict[str, Any] = {}


class DrawingActor(Actor):
    """
    Actor that generates video frames using Python drawing code (Skia).

    The drawing code must define a `draw(canvas, ctx)` function that
    receives a Skia canvas and a DrawingContext object.

    Usage:
        draw_code = '''
def draw(canvas, ctx):
    import skia

    # Animated circle
    radius = 50 + 30 * np.sin(ctx.time * 2)

    paint = skia.Paint()
    paint.setColor(skia.Color(255, 0, 0, 255))
    paint.setAntiAlias(True)

    canvas.drawCircle(ctx.width / 2, ctx.height / 2, radius, paint)
        '''

        drawing = DrawingActor(
            actor_id='drawing',
            draw_code=draw_code,
            width=1920,
            height=1080,
            fps=60
        )

        drawing.outputs['video'] >> display.inputs['video']

    Example with custom variables:
        drawing.update_context(score=100, message="Hello")
    """

    def __init__(
        self,
        actor_id: str = 'drawing',
        draw_code: str = '',
        width: int = 1920,
        height: int = 1080,
        fps: float = 60.0,
        background_color: tuple[int, int, int, int] = (0, 0, 0, 255)
    ):
        """
        Initialize drawing actor.

        Args:
            actor_id: Unique actor identifier
            draw_code: Python code defining a draw(canvas, ctx) function
            width: Output width
            height: Output height
            fps: Output frame rate
            background_color: RGBA background color tuple
        """
        super().__init__(actor_id=actor_id, clock=SoftwareClock(fps=fps))

        self.width = width
        self.height = height
        self.background_color = background_color

        # Drawing state
        self.draw_function = None
        self.context = DrawingContext()
        self.code_namespace = {}

        # Set initial drawing code
        if draw_code:
            self.set_draw_code(draw_code)

        # Create output port
        self.outputs['video'] = StreamOutput('video')

        # Start processing
        self.start()

    def set_draw_code(self, code: str) -> Dict[str, Any]:
        """
        Compile and set drawing code.

        Args:
            code: Python code defining a draw(canvas, ctx) function

        Returns:
            Dictionary with status and error (if any)

        Example:
            result = actor.set_draw_code('''
def draw(canvas, ctx):
    paint = skia.Paint()
    paint.setColor(skia.Color(255, 0, 0))
    canvas.drawCircle(100, 100, 50, paint)
            ''')

            if result['status'] == 'error':
                print(f"Error: {result['error']}")
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
            actor.update_context(score=100, message="Hello World")

            # In draw code, access via:
            # ctx.score, ctx.message
        """
        self.context.custom.update(kwargs)

    async def process(self, tick: TimedTick) -> None:
        """
        Generate and output video frame.

        Args:
            tick: Clock tick with timing information
        """
        # Render frame
        frame_data = self._render_frame(tick)

        # Create output frame
        output_frame = VideoFrame(
            data=frame_data,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )

        # Write to output
        self.outputs['video'].write(output_frame)

    def _render_frame(self, tick: TimedTick) -> NDArray[np.uint8]:
        """
        Render frame using drawing code.

        Args:
            tick: Clock tick with timing information

        Returns:
            RGB numpy array (H, W, 3) uint8
        """
        if self.draw_function is None:
            # Return solid background if no draw function
            frame = np.full((self.height, self.width, 3), self.background_color[:3], dtype=np.uint8)
            return frame

        try:
            # Update context
            self.context.time = tick.timestamp
            self.context.frame_number = tick.frame_number
            self.context.width = self.width
            self.context.height = self.height

            # Copy custom attributes to context
            for key, value in self.context.custom.items():
                setattr(self.context, key, value)

            # Create Skia surface (CPU, faster than GPU due to transfer overhead)
            surface = skia.Surface(self.width, self.height)
            canvas = surface.getCanvas()

            # Clear to background color
            canvas.clear(skia.Color(*self.background_color))

            # Call draw function
            self.draw_function(canvas, self.context)

            # Get image as numpy array (RGBA)
            image = surface.makeImageSnapshot()
            array = image.toarray()  # Shape (H, W, 4)

            # Convert RGBA to RGB
            rgb = array[:, :, :3]

            return rgb

        except Exception as e:
            print(f"[{self.actor_id}] Error in draw function: {e}")
            traceback.print_exc()

            # Return solid background on error
            frame = np.full((self.height, self.width, 3), self.background_color[:3], dtype=np.uint8)
            return frame

    def get_context(self) -> DrawingContext:
        """
        Get drawing context (for inspection).

        Returns:
            Current DrawingContext
        """
        return self.context
