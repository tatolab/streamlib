"""
Camera capture handler for live video input.

Captures video from webcam or other camera devices using OpenCV.
"""

import cv2
import numpy as np
from streamlib.handler import StreamHandler
from streamlib.ports import VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class CameraHandler(StreamHandler):
    """
    Captures video from camera device.

    Uses OpenCV VideoCapture to read frames from webcam or camera device.
    Blocking operations (cv2.VideoCapture.read) require threadpool dispatcher.

    **Output format**: RGB (converts from OpenCV's BGR format)

    Example:
        camera = CameraHandler(device_id=0, width=1280, height=720)
        runtime.add_stream(Stream(camera))  # Auto-uses threadpool dispatcher
        runtime.connect(camera.outputs['video'], display.inputs['video'])
    """

    preferred_dispatcher = 'threadpool'  # cv2.VideoCapture.read() is blocking

    def __init__(
        self,
        device_id: int = 0,
        width: int = 640,
        height: int = 480,
        fps: int = 30,
        name: str = 'camera'
    ):
        """
        Initialize camera handler.

        Args:
            device_id: Camera device index (0 = default camera)
            width: Desired frame width
            height: Desired frame height
            fps: Desired frames per second (camera may not support all values)
            name: Handler identifier
        """
        super().__init__(name)

        self.device_id = device_id
        self.width = width
        self.height = height
        self.fps = fps

        # Output port
        self.outputs['video'] = VideoOutput('video')

        # Camera state
        self.capture = None
        self.frame_count = 0

    async def on_start(self):
        """Open camera device."""
        print(f"[{self.handler_id}] Opening camera device {self.device_id}...")

        self.capture = cv2.VideoCapture(self.device_id)

        if not self.capture.isOpened():
            raise RuntimeError(f"Failed to open camera device {self.device_id}")

        # Set camera properties
        self.capture.set(cv2.CAP_PROP_FRAME_WIDTH, self.width)
        self.capture.set(cv2.CAP_PROP_FRAME_HEIGHT, self.height)
        self.capture.set(cv2.CAP_PROP_FPS, self.fps)

        # Get actual properties (camera may not support requested values)
        actual_width = int(self.capture.get(cv2.CAP_PROP_FRAME_WIDTH))
        actual_height = int(self.capture.get(cv2.CAP_PROP_FRAME_HEIGHT))
        actual_fps = int(self.capture.get(cv2.CAP_PROP_FPS))

        print(f"[{self.handler_id}] Camera opened:")
        print(f"  Resolution: {actual_width}x{actual_height} (requested {self.width}x{self.height})")
        print(f"  FPS: {actual_fps} (requested {self.fps})")

        # Update dimensions to actual
        self.width = actual_width
        self.height = actual_height

    async def process(self, tick: TimedTick):
        """Capture frame from camera."""
        if self.capture is None:
            return

        # Read frame (blocking operation)
        ret, frame_bgr = self.capture.read()

        if not ret:
            print(f"[{self.handler_id}] Failed to read frame")
            return

        # Convert BGR (OpenCV format) to RGB (streamlib standard)
        frame_rgb = cv2.cvtColor(frame_bgr, cv2.COLOR_BGR2RGB)

        # Write to output
        video_frame = VideoFrame(
            data=frame_rgb,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )

        self.outputs['video'].write(video_frame)
        self.frame_count += 1

    async def on_stop(self):
        """Release camera device."""
        if self.capture is not None:
            self.capture.release()
            print(f"[{self.handler_id}] Camera released ({self.frame_count} frames captured)")
