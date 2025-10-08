"""
TestSource - Generate test patterns for debugging and development.

This source generates various test patterns including SMPTE color bars,
gradients, solid colors, and animated patterns.
"""

import numpy as np
import time
from typing import Literal, Optional
from ..base import StreamSource, TimestampedFrame
from ..plugins import register_source


@register_source('test')
class TestSource(StreamSource):
    """
    Source that generates test patterns.

    Useful for testing pipelines, debugging, and as placeholder content.
    Supports various standard test patterns.

    Args:
        pattern: Type of test pattern to generate:
            - 'smpte_bars': SMPTE color bars
            - 'color_bars': Simple RGB color bars
            - 'solid': Solid color
            - 'gradient': Linear gradient
            - 'checkerboard': Checkerboard pattern
            - 'moving_box': Animated moving box
        color: Color for 'solid' pattern (R, G, B tuple, 0-255)
        **kwargs: Additional arguments passed to StreamSource

    Example:
        # SMPTE color bars
        source = TestSource(pattern='smpte_bars', width=1920, height=1080)
        await source.start()

        # Solid red
        source = TestSource(pattern='solid', color=(255, 0, 0))

        # Animated pattern
        source = TestSource(pattern='moving_box')
    """

    def __init__(
        self,
        pattern: Literal[
            'smpte_bars', 'color_bars', 'solid', 'gradient',
            'checkerboard', 'moving_box'
        ] = 'smpte_bars',
        color: tuple[int, int, int] = (128, 128, 128),
        **kwargs
    ):
        super().__init__(**kwargs)
        self.pattern = pattern
        self.color = color
        self._running = False

    async def start(self) -> None:
        """Start generating frames."""
        self._running = True
        self._frame_count = 0
        self._start_time = time.time()

    async def stop(self) -> None:
        """Stop generating frames."""
        self._running = False

    async def next_frame(self) -> TimestampedFrame:
        """
        Generate the next test pattern frame.

        Returns:
            TimestampedFrame with the generated pattern
        """
        if not self._running:
            raise RuntimeError("Source not started. Call start() first.")

        # Generate pattern based on type
        if self.pattern == 'smpte_bars':
            frame_data = self._generate_smpte_bars()
        elif self.pattern == 'color_bars':
            frame_data = self._generate_color_bars()
        elif self.pattern == 'solid':
            frame_data = self._generate_solid()
        elif self.pattern == 'gradient':
            frame_data = self._generate_gradient()
        elif self.pattern == 'checkerboard':
            frame_data = self._generate_checkerboard()
        elif self.pattern == 'moving_box':
            frame_data = self._generate_moving_box()
        else:
            raise ValueError(f"Unknown pattern: {self.pattern}")

        # Create timestamped frame
        timestamp = time.time()
        ts_frame = TimestampedFrame(
            frame=frame_data,
            timestamp=timestamp,
            frame_number=self._frame_count,
            source_id=f"test_{self.pattern}",
            metadata={
                'pattern': self.pattern,
                'width': self.width,
                'height': self.height,
            }
        )

        self._frame_count += 1

        # Sleep to maintain target frame rate
        if self._start_time:
            expected_time = self._start_time + (self._frame_count / self.fps)
            sleep_time = expected_time - time.time()
            if sleep_time > 0:
                import asyncio
                await asyncio.sleep(sleep_time)

        return ts_frame

    def _generate_smpte_bars(self) -> np.ndarray:
        """Generate SMPTE color bars test pattern."""
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # SMPTE color bars (7 bars across top 2/3)
        bar_width = self.width // 7
        top_height = (self.height * 2) // 3

        # Top bars: white, yellow, cyan, green, magenta, red, blue
        colors_top = [
            (255, 255, 255),  # White
            (255, 255, 0),    # Yellow
            (0, 255, 255),    # Cyan
            (0, 255, 0),      # Green
            (255, 0, 255),    # Magenta
            (255, 0, 0),      # Red
            (0, 0, 255),      # Blue
        ]

        for i, color in enumerate(colors_top):
            x_start = i * bar_width
            x_end = (i + 1) * bar_width if i < 6 else self.width
            frame[:top_height, x_start:x_end] = color

        # Bottom 1/3: gradient and pluge
        bottom_start = top_height
        frame[bottom_start:, :] = (16, 16, 16)  # Dark gray background

        return frame

    def _generate_color_bars(self) -> np.ndarray:
        """Generate simple RGB color bars."""
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # 8 bars: Black, Red, Green, Blue, Yellow, Cyan, Magenta, White
        bar_width = self.width // 8
        colors = [
            (0, 0, 0),        # Black
            (255, 0, 0),      # Red
            (0, 255, 0),      # Green
            (0, 0, 255),      # Blue
            (255, 255, 0),    # Yellow
            (0, 255, 255),    # Cyan
            (255, 0, 255),    # Magenta
            (255, 255, 255),  # White
        ]

        for i, color in enumerate(colors):
            x_start = i * bar_width
            x_end = (i + 1) * bar_width if i < 7 else self.width
            frame[:, x_start:x_end] = color

        return frame

    def _generate_solid(self) -> np.ndarray:
        """Generate solid color frame."""
        frame = np.full((self.height, self.width, 3), self.color, dtype=np.uint8)
        return frame

    def _generate_gradient(self) -> np.ndarray:
        """Generate horizontal gradient from black to white."""
        gradient = np.linspace(0, 255, self.width, dtype=np.uint8)
        frame = np.tile(gradient, (self.height, 1))
        frame = np.stack([frame, frame, frame], axis=-1)
        return frame

    def _generate_checkerboard(self) -> np.ndarray:
        """Generate checkerboard pattern."""
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        square_size = min(self.width, self.height) // 8

        for i in range(0, self.height, square_size):
            for j in range(0, self.width, square_size):
                if (i // square_size + j // square_size) % 2 == 0:
                    frame[i:i+square_size, j:j+square_size] = (255, 255, 255)

        return frame

    def _generate_moving_box(self) -> np.ndarray:
        """Generate animated moving box."""
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # Box dimensions
        box_size = min(self.width, self.height) // 8

        # Calculate position based on frame count (bounce around)
        t = self._frame_count / self.fps
        x = int((np.sin(t) + 1) / 2 * (self.width - box_size))
        y = int((np.cos(t * 1.3) + 1) / 2 * (self.height - box_size))

        # Draw box
        frame[y:y+box_size, x:x+box_size] = (255, 0, 0)  # Red box

        # Draw trail (fade effect)
        frame = frame.astype(np.float32)
        frame[:, :, 1:] *= 0.95  # Slight green/blue fade
        frame = frame.astype(np.uint8)

        return frame
