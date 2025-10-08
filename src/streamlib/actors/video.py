"""
Video actors for generation, processing, and display.

Actors:
- TestPatternActor: Generate test patterns (SMPTE bars, gradients, etc.)
- DisplayActor: Display video in window (implemented below)
"""

import numpy as np
from typing import Optional

from ..actor import Actor, StreamOutput, StreamInput
from ..clocks import SoftwareClock, TimedTick
from ..messages import VideoFrame


class TestPatternActor(Actor):
    """
    Generate test patterns for video testing.

    Patterns:
    - 'smpte_bars': SMPTE color bars (broadcast standard test pattern)
    - 'gradient': Horizontal RGB gradient
    - 'black': Solid black
    - 'white': Solid white

    Outputs:
        video: VideoFrame messages

    Example:
        gen = TestPatternActor(
            actor_id='test-pattern',
            width=1920,
            height=1080,
            pattern='smpte_bars',
            fps=60.0
        )
        gen.outputs['video'] >> display.inputs['video']
    """

    def __init__(
        self,
        actor_id: str = 'test-pattern',
        width: int = 1920,
        height: int = 1080,
        pattern: str = 'smpte_bars',
        fps: float = 60.0
    ):
        """
        Initialize test pattern generator.

        Args:
            actor_id: Unique actor identifier
            width: Frame width in pixels
            height: Frame height in pixels
            pattern: Pattern type ('smpte_bars', 'gradient', 'black', 'white')
            fps: Frames per second
        """
        # Initialize with software clock at specified FPS
        super().__init__(
            actor_id=actor_id,
            clock=SoftwareClock(fps=fps, clock_id=f'{actor_id}-clock')
        )

        self.width = width
        self.height = height
        self.pattern = pattern

        # Create output port
        self.outputs['video'] = StreamOutput('video')

        # Auto-start
        self.start()

    async def process(self, tick: TimedTick) -> None:
        """
        Generate frame for this tick.

        Args:
            tick: Timing information
        """
        # Generate pattern
        if self.pattern == 'smpte_bars':
            frame_data = self._generate_smpte_bars()
        elif self.pattern == 'gradient':
            frame_data = self._generate_gradient()
        elif self.pattern == 'black':
            frame_data = np.zeros((self.height, self.width, 3), dtype=np.uint8)
        elif self.pattern == 'white':
            frame_data = np.full((self.height, self.width, 3), 255, dtype=np.uint8)
        else:
            # Unknown pattern, default to black
            frame_data = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # Create frame message
        frame = VideoFrame(
            data=frame_data,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height,
            metadata={'pattern': self.pattern, 'actor_id': self.actor_id}
        )

        # Write to output ring buffer
        self.outputs['video'].write(frame)

    def _generate_smpte_bars(self) -> np.ndarray:
        """
        Generate SMPTE color bars test pattern.

        Returns:
            NumPy array (H, W, 3) uint8 RGB
        """
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # 7 vertical bars
        bar_width = self.width // 7

        # SMPTE colors (RGB)
        colors = [
            (180, 180, 180),  # White (gray)
            (180, 180, 16),   # Yellow
            (16, 180, 180),   # Cyan
            (16, 180, 16),    # Green
            (180, 16, 180),   # Magenta
            (180, 16, 16),    # Red
            (16, 16, 180),    # Blue
        ]

        for i, color in enumerate(colors):
            x_start = i * bar_width
            x_end = (i + 1) * bar_width if i < 6 else self.width
            frame[:, x_start:x_end, :] = color

        return frame

    def _generate_gradient(self) -> np.ndarray:
        """
        Generate horizontal RGB gradient.

        Returns:
            NumPy array (H, W, 3) uint8 RGB
        """
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)
        gradient = np.linspace(0, 255, self.width, dtype=np.uint8)
        frame[:, :, 0] = gradient  # Red channel
        frame[:, :, 1] = gradient  # Green channel
        frame[:, :, 2] = gradient  # Blue channel
        return frame


class DisplayActor(Actor):
    """
    Display video frames in OpenCV window.

    Inputs:
        video: VideoFrame messages

    Example:
        display = DisplayActor(
            actor_id='display',
            window_name='Video Output'
        )
        generator.outputs['video'] >> display.inputs['video']

    Note: Requires OpenCV (cv2). Window blocks event loop briefly
    but uses asyncio.sleep(0) to yield control.
    """

    def __init__(
        self,
        actor_id: str = 'display',
        window_name: str = 'streamlib',
        inherit_clock: bool = True
    ):
        """
        Initialize display actor.

        Args:
            actor_id: Unique actor identifier
            window_name: Window title
            inherit_clock: If True, inherits clock from upstream (default: True)

        Note: If inherit_clock=True, clock will be set when connected.
        """
        # Display actors inherit upstream clock (don't generate ticks)
        clock = None if inherit_clock else SoftwareClock(fps=60.0)

        super().__init__(
            actor_id=actor_id,
            clock=clock
        )

        self.window_name = window_name
        self.inherit_clock = inherit_clock

        # Create input port
        self.inputs['video'] = StreamInput('video')

        # Create window (lazy - create on first frame)
        self.window_created = False

        # Auto-start
        self.start()

    async def process(self, tick: TimedTick) -> None:
        """
        Display latest frame.

        Args:
            tick: Timing information
        """
        # Import cv2 here (not at top) to avoid dependency if not used
        try:
            import cv2
        except ImportError:
            print(f"[{self.actor_id}] Error: OpenCV not installed (pip install opencv-python)")
            await self.stop()
            return

        # Read latest frame from ring buffer
        frame = self.inputs['video'].read_latest()

        if frame is None:
            # No frame yet (not connected or no data)
            return

        # Create window on first frame
        if not self.window_created:
            cv2.namedWindow(self.window_name, cv2.WINDOW_NORMAL)
            self.window_created = True

        # Convert RGB to BGR (OpenCV uses BGR)
        bgr_frame = cv2.cvtColor(frame.data, cv2.COLOR_RGB2BGR)

        # Display
        cv2.imshow(self.window_name, bgr_frame)

        # Non-blocking waitKey (1ms)
        # Yield to event loop first to prevent blocking
        await asyncio.sleep(0)
        cv2.waitKey(1)

    async def stop(self) -> None:
        """Clean up window on stop."""
        try:
            import cv2
            if self.window_created:
                cv2.destroyWindow(self.window_name)
        except ImportError:
            pass

        await super().stop()


# Import asyncio for DisplayActor
import asyncio
