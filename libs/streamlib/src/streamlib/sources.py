"""
Source handlers for streamlib.

This module provides reusable source handlers that generate streaming data.
Sources have only output ports (no inputs).

For simple use cases, prefer the @camera_source decorator from streamlib.decorators.
Use these classes when you need:
- Custom subclassing
- Explicit control over initialization
- Direct instantiation without decorators

Example:
    from streamlib import StreamRuntime, Stream, CameraSource

    # Simple camera source
    camera = CameraSource(device_id=None)  # First available camera

    runtime = StreamRuntime(fps=30, width=1920, height=1080)
    runtime.add_stream(Stream(camera))
    await runtime.start()
"""

from typing import Optional
from .handler import StreamHandler
from .ports import VideoOutput
from .messages import VideoFrame
from .clocks import TimedTick


class CameraSource(StreamHandler):
    """
    Camera source handler - outputs WebGPU textures at runtime size.

    This is a simple, reusable camera source that captures frames from a camera
    device and emits them as VideoFrames containing WebGPU textures. All frames
    are automatically scaled to the runtime's configured width/height.

    Zero-copy pipeline on macOS:
        AVFoundation → IOSurface → WebGPU texture (single copy)

    Args:
        device_id: Camera device ID (None = first available camera)
        handler_id: Optional handler ID (defaults to "camera_source")

    Attributes:
        outputs['video']: VideoOutput port that emits VideoFrames
        capture: Platform-specific camera capture instance (created on start)

    Example:
        # Basic usage
        camera = CameraSource(device_id=None)
        runtime = StreamRuntime(fps=30, width=1920, height=1080)
        runtime.add_stream(Stream(camera))
        runtime.connect(camera.outputs['video'], blur.inputs['video'])
        await runtime.start()

        # Specific camera
        camera = CameraSource(device_id="0x1234567890abcdef")

        # Custom subclass
        class MyCamera(CameraSource):
            async def on_start(self):
                await super().on_start()
                print(f"Camera started: {self.device_id}")

            async def process(self, tick):
                await super().process(tick)
                # Add custom logic after frame capture
    """

    def __init__(self, device_id: Optional[str] = None, handler_id: Optional[str] = None):
        """
        Initialize camera source.

        Args:
            device_id: Camera device ID (None = first available)
            handler_id: Optional handler ID (defaults to "camera_source")
        """
        super().__init__(handler_id=handler_id or "camera_source")
        self.device_id = device_id
        self.capture = None

        # Create output port only (sources have no inputs)
        self.outputs['video'] = VideoOutput('video')

    async def on_start(self) -> None:
        """
        Create camera capture when handler starts.

        This is called by the runtime after the handler is activated.
        Creates a platform-specific camera capture using the GPU context.

        On macOS: Uses AVFoundation with IOSurface → WebGPU (single-copy)
        On Linux: Uses V4L2
        On Windows: Uses Media Foundation
        """
        # Create camera capture (zero-copy IOSurface → WebGPU on macOS)
        self.capture = self._runtime.gpu_context.create_camera_capture(
            device_id=self.device_id
        )

    async def process(self, tick: TimedTick) -> None:
        """
        Get latest camera frame and emit to output.

        Called by the runtime for each clock tick. Gets the latest frame
        from the camera capture and emits it as a VideoFrame.

        Args:
            tick: Timed tick from the runtime clock
        """
        if self.capture is None:
            return

        try:
            # Get latest frame (WebGPU texture, zero-copy from camera)
            texture = self.capture.get_texture()

            # Create VideoFrame
            frame = VideoFrame(
                data=texture,  # wgpu.GPUTexture - never touched CPU!
                timestamp=tick.timestamp,
                frame_number=tick.frame_number,
                width=self._runtime.width,
                height=self._runtime.height
            )

            # Emit frame
            self.outputs['video'].write(frame)

        except Exception as e:
            print(f"[{self.handler_id}] Error in camera source: {e}")
            import traceback
            traceback.print_exc()

    async def on_stop(self) -> None:
        """
        Stop camera capture when handler stops.

        This is called by the runtime when the handler is deactivated.
        Cleans up the camera capture resources.
        """
        if self.capture:
            self.capture.stop()
            self.capture = None

    def __repr__(self) -> str:
        return f"CameraSource(device_id={self.device_id!r})"


__all__ = [
    'CameraSource',
]
