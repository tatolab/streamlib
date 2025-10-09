"""
Display handler for showing video frames in OpenCV window.

Implements DisplaySink with macOS-specific fixes for window management.
"""

import cv2
from typing import Optional

from ..handler import StreamHandler
from ..ports import VideoInput
from ..clocks import TimedTick

# macOS fix: Start window thread at module import (not per-instance)
# Note: startWindowThread() not available in all OpenCV builds
try:
    cv2.startWindowThread()
except AttributeError:
    pass  # Not available in this OpenCV build


class DisplayHandler(StreamHandler):
    """
    Display video frames in an OpenCV window.

    Input capabilities: ['cpu'] - accepts numpy arrays

    Includes macOS-specific fixes:
    - WINDOW_AUTOSIZE instead of WINDOW_NORMAL (prevents crash)
    - WND_PROP_TOPMOST=1 to bring window to foreground

    Example:
        ```python
        display = DisplayHandler(window_name="Test Pattern")
        runtime.add_stream(Stream(display, dispatcher='asyncio'))
        runtime.connect(pattern.outputs['video'], display.inputs['video'])
        ```
    """

    def __init__(
        self,
        window_name: str = "streamlib",
        handler_id: str = None
    ):
        """
        Initialize display handler.

        Args:
            window_name: Name for OpenCV window
            handler_id: Optional custom handler ID
        """
        super().__init__(handler_id or f'display-{window_name}')

        self.window_name = window_name

        # Input: CPU-only (numpy arrays)
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])

        # Frame counter
        self._frame_count = 0
        self._window_created = False

    def _create_window(self) -> None:
        """
        Create OpenCV window with macOS fixes.

        Called on first frame to ensure window is created in correct thread.
        """
        if self._window_created:
            return

        # Use WINDOW_AUTOSIZE (not WINDOW_NORMAL) to prevent macOS crash
        cv2.namedWindow(self.window_name, cv2.WINDOW_AUTOSIZE)

        # Bring window to foreground on macOS
        cv2.setWindowProperty(
            self.window_name,
            cv2.WND_PROP_TOPMOST,
            1
        )

        self._window_created = True

    async def process(self, tick: TimedTick) -> None:
        """
        Display one frame per tick.

        Reads latest frame from input and displays in OpenCV window.
        Creates window on first frame.
        """
        frame = self.inputs['video'].read_latest()

        if frame is None:
            return

        # Create window on first frame
        if not self._window_created:
            self._create_window()

        # Display frame (BGR format expected by OpenCV)
        # Note: test_pattern.py generates RGB, so we need to convert
        frame_bgr = cv2.cvtColor(frame.data, cv2.COLOR_RGB2BGR)
        cv2.imshow(self.window_name, frame_bgr)

        # Process events (required for window to update)
        cv2.waitKey(1)

        self._frame_count += 1

    async def on_start(self) -> None:
        """Called when handler starts."""
        print(f"DisplayHandler started: window='{self.window_name}'")

    async def on_stop(self) -> None:
        """Called when handler stops - cleanup window."""
        print(f"DisplayHandler stopped: {self._frame_count} frames displayed")

        if self._window_created:
            cv2.destroyWindow(self.window_name)
            cv2.waitKey(1)  # Process final events
