"""
Compositor actors for combining multiple video streams.

This module provides actors that combine multiple video inputs:
- CompositorActor: Alpha blend multiple video layers
"""

import asyncio
import numpy as np
from numpy.typing import NDArray
from typing import Optional, List, Tuple

from ..actor import Actor, StreamInput, StreamOutput
from ..clocks import SoftwareClock, TimedTick
from ..messages import VideoFrame


class CompositorActor(Actor):
    """
    Compositor that alpha blends multiple video inputs.

    The compositor:
    - Accepts N video inputs (input0, input1, ...)
    - Sorts by z-index (lowest to highest)
    - Generates background if no inputs
    - Alpha blends each layer on top
    - Outputs composited RGB video

    Usage:
        compositor = CompositorActor(
            actor_id='compositor',
            width=1920,
            height=1080,
            fps=60,
            num_inputs=3
        )

        # Connect video sources
        source1.outputs['video'] >> compositor.inputs['input0']
        source2.outputs['video'] >> compositor.inputs['input1']

        # Connect output
        compositor.outputs['video'] >> display.inputs['video']

    Note: Inputs with no data are skipped (allowing dynamic layer count).
    """

    def __init__(
        self,
        actor_id: str = 'compositor',
        width: int = 1920,
        height: int = 1080,
        fps: float = 60.0,
        num_inputs: int = 4,
        background_color: Tuple[int, int, int, int] = (20, 20, 30, 255)
    ):
        """
        Initialize compositor actor.

        Args:
            actor_id: Unique actor identifier
            width: Output width
            height: Output height
            fps: Output frame rate
            num_inputs: Number of input ports to create
            background_color: RGBA background color tuple
        """
        super().__init__(actor_id=actor_id, clock=SoftwareClock(fps=fps))

        self.width = width
        self.height = height
        self.num_inputs = num_inputs
        self.background_color = background_color

        # Create input ports
        for i in range(num_inputs):
            self.inputs[f'input{i}'] = StreamInput(f'input{i}')

        # Create output port
        self.outputs['video'] = StreamOutput('video')

        # Start processing
        self.start()

    async def process(self, tick: TimedTick) -> None:
        """
        Composite all input layers into output frame.

        Args:
            tick: Clock tick with timing information
        """
        # Collect layers from inputs (skip None/empty)
        layers: List[Tuple[int, VideoFrame]] = []
        for i in range(self.num_inputs):
            input_port = self.inputs[f'input{i}']
            frame = input_port.read_latest()
            if frame is not None and isinstance(frame, VideoFrame):
                layers.append((i, frame))  # Use input index as z-index

        # Start with background
        result = self._generate_background()

        # Composite each layer (already sorted by z-index)
        for z_index, layer_frame in layers:
            # Convert RGB to RGBA (add alpha channel)
            overlay = self._rgb_to_rgba(layer_frame.data)

            # Resize if needed
            if overlay.shape[:2] != (self.height, self.width):
                overlay = self._resize_frame(overlay, self.width, self.height)

            # Alpha blend onto result
            result = self._alpha_blend(result, overlay)

        # Convert RGBA to RGB for output
        result_rgb = result[:, :, :3]

        # Create output frame
        output_frame = VideoFrame(
            data=result_rgb,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )

        # Write to output
        self.outputs['video'].write(output_frame)

    def _generate_background(self) -> NDArray[np.uint8]:
        """
        Generate background pattern.

        Returns:
            RGBA background frame (H, W, 4) uint8
        """
        frame = np.empty((self.height, self.width, 4), dtype=np.uint8)

        # Dark gradient background using vectorized operations
        y_gradient = np.linspace(0, 1, self.height, dtype=np.float32)[:, np.newaxis]

        # RGB channels with gradient
        frame[:, :, 0] = (self.background_color[0] + y_gradient * 30).astype(np.uint8)
        frame[:, :, 1] = (self.background_color[1] + y_gradient * 30).astype(np.uint8)
        frame[:, :, 2] = (self.background_color[2] + y_gradient * 10).astype(np.uint8)
        frame[:, :, 3] = self.background_color[3]  # Constant alpha

        return frame

    def _rgb_to_rgba(self, rgb: NDArray[np.uint8]) -> NDArray[np.uint8]:
        """
        Convert RGB frame to RGBA (add alpha channel).

        Args:
            rgb: RGB frame (H, W, 3) uint8

        Returns:
            RGBA frame (H, W, 4) uint8
        """
        h, w = rgb.shape[:2]
        rgba = np.empty((h, w, 4), dtype=np.uint8)
        rgba[:, :, :3] = rgb
        rgba[:, :, 3] = 255  # Opaque
        return rgba

    def _resize_frame(
        self,
        frame: NDArray[np.uint8],
        width: int,
        height: int
    ) -> NDArray[np.uint8]:
        """
        Resize frame to target dimensions.

        Args:
            frame: Input frame
            width: Target width
            height: Target height

        Returns:
            Resized frame
        """
        import cv2
        return cv2.resize(frame, (width, height))

    def _alpha_blend(
        self,
        background: NDArray[np.uint8],
        overlay: NDArray[np.uint8]
    ) -> NDArray[np.uint8]:
        """
        Alpha blend overlay onto background using optimized numpy operations.

        This is the core compositing algorithm from Phase 1, optimized for
        zero-copy operation and efficient uint16 arithmetic.

        Args:
            background: Background RGBA frame (H, W, 4) uint8
            overlay: Overlay RGBA frame (H, W, 4) uint8

        Returns:
            Blended RGBA frame (H, W, 4) uint8
        """
        if overlay.shape != background.shape:
            # Resize overlay to match background if needed
            overlay = self._resize_frame(overlay, self.width, self.height)

        # Pre-allocate result array (avoids dstack allocation)
        result = np.empty_like(background)

        # Extract alpha as uint16 to avoid overflow in intermediate calculations
        # This avoids expensive float32 conversions
        overlay_alpha = overlay[:, :, 3].astype(np.uint16)
        bg_alpha = background[:, :, 3].astype(np.uint16)

        # Compute blended alpha first (uint16 arithmetic, then back to uint8)
        # Formula: result_alpha = overlay_alpha + bg_alpha * (255 - overlay_alpha) / 255
        result[:, :, 3] = (
            overlay_alpha + (bg_alpha * (255 - overlay_alpha)) // 255
        ).astype(np.uint8)

        # Blend RGB channels using uint16 to avoid overflow
        # Loop is faster than vectorizing all channels due to smaller intermediate arrays
        # Formula: result = (overlay * alpha + background * (255 - alpha)) / 255
        for c in range(3):  # RGB channels
            overlay_c = overlay[:, :, c].astype(np.uint16)
            bg_c = background[:, :, c].astype(np.uint16)
            result[:, :, c] = (
                (overlay_c * overlay_alpha + bg_c * (255 - overlay_alpha)) // 255
            ).astype(np.uint8)

        return result

    def set_layer_opacity(self, layer_index: int, opacity: float) -> None:
        """
        Set opacity for a specific layer (future enhancement).

        Args:
            layer_index: Input index (0 to num_inputs-1)
            opacity: Opacity value (0.0 to 1.0)

        Note: Not yet implemented. Will require modifying frames before blend.
        """
        # TODO Phase 3: Implement per-layer opacity control
        # Store opacity in dict and apply in process() before blending
        pass

    def set_layer_position(
        self,
        layer_index: int,
        x: int,
        y: int,
        width: Optional[int] = None,
        height: Optional[int] = None
    ) -> None:
        """
        Set position and size for a specific layer (future enhancement).

        Args:
            layer_index: Input index (0 to num_inputs-1)
            x: X position
            y: Y position
            width: Optional width (default: keep original)
            height: Optional height (default: keep original)

        Note: Not yet implemented. Will require frame placement before blend.
        """
        # TODO Phase 3: Implement per-layer positioning and scaling
        # Store position/size in dict and apply in process() before blending
        pass
