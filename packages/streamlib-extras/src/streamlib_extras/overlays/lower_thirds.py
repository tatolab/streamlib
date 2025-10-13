"""
Lower thirds graphics overlay handler.

Draws newscast-style lower thirds with slide-in animation:
- Name/title text
- Colored bars
- Optional "LIVE" indicator
- Slide-in animation from right
"""

import cv2
import numpy as np
from typing import Optional, Tuple

from streamlib.handler import StreamHandler
from streamlib.ports import VideoInput, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class LowerThirdsHandler(StreamHandler):
    """
    Newscast-style lower thirds overlay with slide-in animation.

    Draws a professional lower thirds graphic that slides in from the right,
    then stays visible. Perfect for adding names, titles, and branding to
    live video feeds.

    Features:
    - Slide-in animation (configurable duration)
    - Two-line text (name + title/subtitle)
    - Colored accent bar
    - Optional "LIVE" indicator
    - Optional channel/number display

    Example:
        ```python
        lower_thirds = LowerThirdsHandler(
            name="SARAH MCFINN",
            title="ULTIMATE FIGHTER",
            bar_color=(0, 165, 255),  # Orange in BGR
            live_indicator=True,
            channel="45"
        )
        runtime.connect(source.outputs['video'], lower_thirds.inputs['video'])
        runtime.connect(lower_thirds.outputs['video'], display.inputs['video'])
        ```
    """

    preferred_dispatcher = 'threadpool'  # CPU drawing operations

    def __init__(
        self,
        name: str = "JOHN DOE",
        title: str = "TITLE HERE",
        bar_color: Tuple[int, int, int] = (0, 165, 255),  # Orange BGR
        text_color: Tuple[int, int, int] = (255, 255, 255),  # White
        bg_color: Tuple[int, int, int] = (40, 40, 80),  # Dark blue-gray
        live_indicator: bool = True,
        live_color: Tuple[int, int, int] = (0, 0, 255),  # Red BGR
        channel: Optional[str] = None,
        slide_duration: float = 1.0,  # Seconds to slide in
        position: str = "bottom-left",  # or "bottom-right", "top-left", "top-right"
        handler_id: str = None
    ):
        """
        Initialize lower thirds overlay handler.

        Args:
            name: Primary text (top line, larger)
            title: Secondary text (bottom line, smaller)
            bar_color: Color for accent bar (BGR)
            text_color: Color for text (BGR)
            bg_color: Color for background box (BGR)
            live_indicator: Show "LIVE" indicator
            live_color: Color for LIVE indicator (BGR)
            channel: Optional channel number/text
            slide_duration: Duration of slide-in animation (seconds)
            position: Position on screen (bottom-left, bottom-right, top-left, top-right)
            handler_id: Optional custom handler ID
        """
        super().__init__(handler_id or 'lower-thirds')

        self.name = name.upper()
        self.title = title.upper()
        self.bar_color = bar_color
        self.text_color = text_color
        self.bg_color = bg_color
        self.live_indicator = live_indicator
        self.live_color = live_color
        self.channel = channel
        self.slide_duration = slide_duration
        self.position = position

        # Flexible capabilities: accept both CPU and GPU
        self.inputs['video'] = VideoInput('video')
        self.outputs['video'] = VideoOutput('video')

        # Animation state
        self._start_time: Optional[float] = None
        self._animation_complete = False

        # Cached dimensions (calculated on first frame)
        self._box_width = 0
        self._box_height = 0
        self._bar_width = 0

    def _calculate_dimensions(self, frame_width: int, frame_height: int):
        """Calculate lower thirds dimensions based on frame size."""
        # Box dimensions (proportion of frame)
        self._box_width = int(frame_width * 0.35)  # 35% of frame width
        self._box_height = int(frame_height * 0.15)  # 15% of frame height

        # Accent bar (left side of box)
        self._bar_width = int(self._box_width * 0.02)  # 2% of box width

    def _get_slide_offset(self, current_time: float, frame_width: int) -> int:
        """
        Calculate horizontal offset for slide-in animation.

        Returns:
            Pixel offset from target position (0 = fully visible)
        """
        if self._animation_complete:
            return 0

        if self._start_time is None:
            self._start_time = current_time

        # Calculate animation progress (0.0 to 1.0)
        elapsed = current_time - self._start_time
        progress = min(1.0, elapsed / self.slide_duration)

        if progress >= 1.0:
            self._animation_complete = True
            return 0

        # Ease-out cubic: progress^3
        eased = 1.0 - pow(1.0 - progress, 3)

        # Start from right edge of screen
        max_offset = frame_width
        offset = int(max_offset * (1.0 - eased))

        return offset

    def _draw_lower_thirds(
        self,
        frame: np.ndarray,
        current_time: float
    ) -> np.ndarray:
        """
        Draw lower thirds overlay on frame.

        Args:
            frame: Input frame (RGB, uint8)
            current_time: Current timestamp for animation

        Returns:
            Frame with lower thirds overlay
        """
        h, w = frame.shape[:2]

        # Calculate dimensions on first frame
        if self._box_width == 0:
            self._calculate_dimensions(w, h)

        # Get animation offset
        slide_offset = self._get_slide_offset(current_time, w)

        # Calculate box position based on position setting
        margin = 40
        if self.position.startswith("bottom"):
            box_y = h - self._box_height - margin
        else:  # top
            box_y = margin

        if self.position.endswith("left"):
            box_x = margin + slide_offset
        else:  # right
            box_x = w - self._box_width - margin + slide_offset

        # Only draw if box is visible
        if box_x < w:
            # Draw background box
            cv2.rectangle(
                frame,
                (box_x, box_y),
                (box_x + self._box_width, box_y + self._box_height),
                self.bg_color,
                -1  # Filled
            )

            # Draw accent bar
            cv2.rectangle(
                frame,
                (box_x, box_y),
                (box_x + self._bar_width, box_y + self._box_height),
                self.bar_color,
                -1  # Filled
            )

            # Text positioning
            text_x = box_x + self._bar_width + 20
            name_y = box_y + int(self._box_height * 0.4)
            title_y = box_y + int(self._box_height * 0.75)

            # Draw name (larger text, use DUPLEX which is heavier/bolder)
            cv2.putText(
                frame,
                self.name,
                (text_x, name_y),
                cv2.FONT_HERSHEY_DUPLEX,
                0.8,
                self.text_color,
                2,
                cv2.LINE_AA
            )

            # Draw title (smaller text)
            cv2.putText(
                frame,
                self.title,
                (text_x, title_y),
                cv2.FONT_HERSHEY_SIMPLEX,
                0.5,
                self.bar_color,
                1,
                cv2.LINE_AA
            )

            # Draw LIVE indicator (top right of box)
            if self.live_indicator:
                live_x = box_x + self._box_width - 80
                live_y = box_y + 25

                # Red circle
                cv2.circle(frame, (live_x - 10, live_y - 5), 6, self.live_color, -1)

                # "LIVE" text
                cv2.putText(
                    frame,
                    "LIVE",
                    (live_x, live_y),
                    cv2.FONT_HERSHEY_DUPLEX,
                    0.5,
                    self.text_color,
                    1,
                    cv2.LINE_AA
                )

            # Draw channel number (bottom right of box)
            if self.channel:
                channel_x = box_x + self._box_width - 60
                channel_y = box_y + self._box_height - 15

                cv2.putText(
                    frame,
                    self.channel,
                    (channel_x, channel_y),
                    cv2.FONT_HERSHEY_BOLD,
                    1.2,
                    self.text_color,
                    2,
                    cv2.LINE_AA
                )

        return frame

    async def process(self, tick: TimedTick) -> None:
        """
        Draw lower thirds overlay on each frame.
        """
        frame = self.inputs['video'].read_latest()
        if frame is None:
            return

        # Get frame data (handle both CPU and GPU)
        if hasattr(frame.data, 'cpu'):  # PyTorch tensor
            frame_np = frame.data.cpu().numpy()
        else:
            frame_np = frame.data

        # OpenCV expects BGR, our frames are RGB
        frame_bgr = cv2.cvtColor(frame_np, cv2.COLOR_RGB2BGR)

        # Draw lower thirds
        frame_with_overlay = self._draw_lower_thirds(frame_bgr, tick.timestamp)

        # Convert back to RGB
        frame_rgb = cv2.cvtColor(frame_with_overlay, cv2.COLOR_BGR2RGB)

        # Create output frame
        output_frame = VideoFrame(
            data=frame_rgb,
            timestamp=frame.timestamp,
            frame_number=frame.frame_number,
            width=frame.width,
            height=frame.height,
            metadata=frame.metadata
        )

        self.outputs['video'].write(output_frame)

    async def on_start(self) -> None:
        """Called when handler starts."""
        print(f"LowerThirdsHandler started: '{self.name}' / '{self.title}'")

    async def on_stop(self) -> None:
        """Called when handler stops."""
        print(f"LowerThirdsHandler stopped")
