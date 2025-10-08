"""
DisplaySink - Show frames in a window for preview.

This sink displays frames in an OpenCV window for development and debugging.
"""

import asyncio
import cv2
import numpy as np
from typing import Optional
from ..base import StreamSink, TimestampedFrame
from ..plugins import register_sink


@register_sink('display')
class DisplaySink(StreamSink):
    """
    Sink that displays frames in a window.

    Uses OpenCV to show frames for preview/debugging.
    Supports keyboard controls for pause, quit, etc.

    Args:
        window_name: Name of the display window
        show_fps: Whether to show FPS counter on screen
        fullscreen: Whether to display in fullscreen mode
        **kwargs: Additional arguments passed to StreamSink

    Keyboard controls:
        - 'q' or ESC: Quit
        - 'p' or SPACE: Pause/unpause
        - 'f': Toggle fullscreen

    Example:
        sink = DisplaySink(window_name='Preview', show_fps=True)
        await sink.start()

        async for frame in source.frames():
            await sink.write_frame(frame)
            if sink.should_quit():
                break

        await sink.stop()
    """

    def __init__(
        self,
        window_name: str = 'Stream Preview',
        show_fps: bool = False,
        fullscreen: bool = False,
        **kwargs
    ):
        super().__init__(**kwargs)

        self.window_name = window_name
        self.show_fps = show_fps
        self.fullscreen = fullscreen

        self._paused = False
        self._quit = False
        self._frame_count = 0
        self._last_fps_time = None
        self._fps_counter = 0.0

    async def start(self) -> None:
        """Create the display window."""
        cv2.namedWindow(self.window_name, cv2.WINDOW_NORMAL)
        if self.fullscreen:
            cv2.setWindowProperty(
                self.window_name,
                cv2.WND_PROP_FULLSCREEN,
                cv2.WINDOW_FULLSCREEN
            )

    async def stop(self) -> None:
        """Close the display window."""
        cv2.destroyWindow(self.window_name)

    async def write_frame(self, frame: TimestampedFrame) -> None:
        """
        Display a frame in the window.

        Args:
            frame: The timestamped frame to display
        """
        # Convert RGB to BGR for OpenCV
        display_frame = cv2.cvtColor(frame.frame, cv2.COLOR_RGB2BGR)

        # Add FPS counter if requested
        if self.show_fps:
            display_frame = self._add_fps_overlay(display_frame, frame.timestamp)

        # Display frame
        cv2.imshow(self.window_name, display_frame)

        # Handle keyboard input (1ms wait)
        # Note: cv2.waitKey is blocking, so we yield to event loop first
        await asyncio.sleep(0)  # Yield to event loop
        key = cv2.waitKey(1) & 0xFF

        if key == ord('q') or key == 27:  # 'q' or ESC
            self._quit = True
        elif key == ord('p') or key == ord(' '):  # 'p' or SPACE
            self._paused = not self._paused
            if self._paused:
                # Wait until unpaused
                while self._paused:
                    key = cv2.waitKey(100) & 0xFF
                    if key == ord('p') or key == ord(' '):
                        self._paused = False
                    elif key == ord('q') or key == 27:
                        self._quit = True
                        self._paused = False
        elif key == ord('f'):  # 'f' for fullscreen toggle
            self.fullscreen = not self.fullscreen
            if self.fullscreen:
                cv2.setWindowProperty(
                    self.window_name,
                    cv2.WND_PROP_FULLSCREEN,
                    cv2.WINDOW_FULLSCREEN
                )
            else:
                cv2.setWindowProperty(
                    self.window_name,
                    cv2.WND_PROP_FULLSCREEN,
                    cv2.WINDOW_NORMAL
                )

        self._frame_count += 1

    def should_quit(self) -> bool:
        """
        Check if the user requested to quit.

        Returns:
            True if the user pressed 'q' or ESC
        """
        return self._quit

    def is_paused(self) -> bool:
        """
        Check if the display is paused.

        Returns:
            True if paused
        """
        return self._paused

    def _add_fps_overlay(self, frame: np.ndarray, timestamp: float) -> np.ndarray:
        """
        Add FPS counter overlay to frame.

        Args:
            frame: Frame to add overlay to
            timestamp: Current timestamp

        Returns:
            Frame with FPS overlay
        """
        if self._last_fps_time is None:
            self._last_fps_time = timestamp
            self._fps_counter = 0.0
        else:
            # Calculate FPS (exponential moving average)
            dt = timestamp - self._last_fps_time
            if dt > 0:
                instant_fps = 1.0 / dt
                self._fps_counter = 0.9 * self._fps_counter + 0.1 * instant_fps
            self._last_fps_time = timestamp

        # Draw FPS text
        fps_text = f"FPS: {self._fps_counter:.1f}"
        cv2.putText(
            frame,
            fps_text,
            (10, 30),
            cv2.FONT_HERSHEY_SIMPLEX,
            1.0,
            (0, 255, 0),
            2,
            cv2.LINE_AA
        )

        return frame
