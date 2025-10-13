"""
Test pattern generator handler.

Generates various test patterns for pipeline testing and debugging.
"""

import numpy as np
from typing import Optional, Literal

from streamlib.handler import StreamHandler
from streamlib.ports import VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


PatternType = Literal['smpte_bars', 'gradient', 'solid', 'checkerboard']


class TestPatternHandler(StreamHandler):
    """
    Generate test pattern video frames.

    Produces standard test patterns useful for debugging and pipeline testing.

    Output capabilities: ['cpu'] - generates numpy arrays

    Example:
        ```python
        pattern = TestPatternHandler(
            width=640,
            height=480,
            pattern='smpte_bars'
        )
        runtime.add_stream(Stream(pattern, dispatcher='asyncio'))
        ```
    """

    def __init__(
        self,
        width: int = 640,
        height: int = 480,
        pattern: PatternType = 'smpte_bars',
        color: Optional[tuple] = None,
        handler_id: str = None
    ):
        """
        Initialize test pattern generator.

        Args:
            width: Frame width in pixels
            height: Frame height in pixels
            pattern: Pattern type ('smpte_bars', 'gradient', 'solid', 'checkerboard')
            color: RGB color for 'solid' pattern (0-255), defaults to white
            handler_id: Optional custom handler ID
        """
        super().__init__(handler_id or 'test-pattern')

        self.width = width
        self.height = height
        self.pattern = pattern
        self.color = color or (255, 255, 255)

        # Output: Generates numpy arrays (runtime handles GPU transfer if needed)
        self.outputs['video'] = VideoOutput('video')

        # Frame counter
        self._frame_number = 0

        # Pre-generate pattern if static
        if pattern in ['smpte_bars', 'solid', 'checkerboard']:
            self._static_pattern = self._generate_pattern()
        else:
            self._static_pattern = None

    def _generate_pattern(self) -> np.ndarray:
        """Generate the test pattern based on type."""
        if self.pattern == 'smpte_bars':
            return self._generate_smpte_bars()
        elif self.pattern == 'gradient':
            return self._generate_gradient()
        elif self.pattern == 'solid':
            return self._generate_solid()
        elif self.pattern == 'checkerboard':
            return self._generate_checkerboard()
        else:
            raise ValueError(f"Unknown pattern type: {self.pattern}")

    def _generate_smpte_bars(self) -> np.ndarray:
        """
        Generate SMPTE color bars test pattern.

        Classic 7-bar pattern: white, yellow, cyan, green, magenta, red, blue.
        """
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # 7 bars of equal width
        bar_width = self.width // 7

        # RGB values for SMPTE bars
        colors = [
            (255, 255, 255),  # White
            (255, 255, 0),    # Yellow
            (0, 255, 255),    # Cyan
            (0, 255, 0),      # Green
            (255, 0, 255),    # Magenta
            (255, 0, 0),      # Red
            (0, 0, 255),      # Blue
        ]

        for i, color in enumerate(colors):
            x_start = i * bar_width
            x_end = (i + 1) * bar_width if i < 6 else self.width
            frame[:, x_start:x_end] = color

        return frame

    def _generate_gradient(self) -> np.ndarray:
        """
        Generate horizontal gradient from black to white.

        Useful for testing color reproduction and dynamic range.
        """
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # Horizontal gradient
        gradient = np.linspace(0, 255, self.width, dtype=np.uint8)
        frame[:, :] = gradient[np.newaxis, :, np.newaxis]

        return frame

    def _generate_solid(self) -> np.ndarray:
        """
        Generate solid color frame.

        Useful for testing compositing and alpha blending.
        """
        frame = np.full((self.height, self.width, 3), self.color, dtype=np.uint8)
        return frame

    def _generate_checkerboard(self) -> np.ndarray:
        """
        Generate checkerboard pattern.

        Useful for testing motion and alignment.
        """
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # 8x8 checkerboard
        square_size = min(self.width, self.height) // 8

        for i in range(8):
            for j in range(8):
                if (i + j) % 2 == 0:
                    y_start = i * square_size
                    y_end = min(y_start + square_size, self.height)
                    x_start = j * square_size
                    x_end = min(x_start + square_size, self.width)
                    frame[y_start:y_end, x_start:x_end] = (255, 255, 255)

        return frame

    async def process(self, tick: TimedTick) -> None:
        """
        Generate and output one frame per tick.

        For static patterns, reuses pre-generated pattern.
        For dynamic patterns (gradient with animation), regenerates each frame.
        """
        # Use pre-generated pattern if available
        if self._static_pattern is not None:
            frame_data = self._static_pattern.copy()  # Copy to avoid mutations
        else:
            frame_data = self._generate_pattern()

        # Create VideoFrame
        frame = VideoFrame(
            data=frame_data,
            timestamp=tick.timestamp,
            frame_number=self._frame_number,
            width=self.width,
            height=self.height,
            metadata={'pattern': self.pattern}
        )

        # Write to output
        self.outputs['video'].write(frame)
        self._frame_number += 1

    async def on_start(self) -> None:
        """Called when handler starts."""
        print(f"TestPatternHandler started: {self.width}x{self.height} @ {self.pattern}")

    async def on_stop(self) -> None:
        """Called when handler stops."""
        print(f"TestPatternHandler stopped: {self._frame_number} frames generated")
