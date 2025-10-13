"""
Compositor handler for alpha blending multiple video layers.

Implements optimized alpha blending from benchmark_results.md learnings.
"""

import numpy as np
from typing import List

from streamlib.handler import StreamHandler
from streamlib.ports import VideoInput, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class CompositorHandler(StreamHandler):
    """
    Composite multiple video layers with alpha blending.

    Supports N input layers (default 4) with configurable alpha values.
    Uses optimized numpy operations for CPU blending.

    Capabilities: ['cpu'] initially (GPU support in future)

    Example:
        ```python
        compositor = CompositorHandler(
            width=640,
            height=480,
            num_layers=4,
            alphas=[1.0, 0.8, 0.6, 0.4]
        )
        runtime.add_stream(Stream(compositor, dispatcher='asyncio'))

        # Connect layers
        runtime.connect(layer0.outputs['video'], compositor.inputs['layer0'])
        runtime.connect(layer1.outputs['video'], compositor.inputs['layer1'])
        ```
    """

    def __init__(
        self,
        width: int = 640,
        height: int = 480,
        num_layers: int = 4,
        alphas: List[float] = None,
        handler_id: str = None
    ):
        """
        Initialize compositor.

        Args:
            width: Output frame width
            height: Output frame height
            num_layers: Number of input layers (default 4)
            alphas: Alpha values for each layer (0.0-1.0). Defaults to [1.0, 0.8, ...}
            handler_id: Optional custom handler ID
        """
        super().__init__(handler_id or 'compositor')

        self.width = width
        self.height = height
        self.num_layers = num_layers

        # Default alphas: first layer full opacity, others fade
        if alphas is None:
            alphas = [1.0 - (i * 0.2) for i in range(num_layers)]
            alphas = [max(0.2, a) for a in alphas]  # Minimum 0.2 alpha

        if len(alphas) != num_layers:
            raise ValueError(f"alphas length ({len(alphas)}) must match num_layers ({num_layers})")

        self.alphas = alphas

        # Create N input ports
        for i in range(num_layers):
            self.inputs[f'layer{i}'] = VideoInput(f'layer{i}')

        # Single output port
        self.outputs['video'] = VideoOutput('video')

        # Accumulator buffer (reused across frames)
        self._accumulator = np.zeros((height, width, 3), dtype=np.float32)

        # Frame counter
        self._frame_count = 0

    def _composite_layers(self, frames: List[VideoFrame]) -> np.ndarray:
        """
        Composite multiple frames using alpha blending.

        Uses optimized numpy operations:
        - Float32 accumulator (avoid repeated int→float conversions)
        - Vectorized blending (no per-pixel loops)
        - Pre-allocated accumulator buffer

        Args:
            frames: List of VideoFrame objects (can contain None for missing layers)

        Returns:
            Composited frame as uint8 numpy array [H, W, 3]
        """
        # Reset accumulator
        self._accumulator.fill(0)

        # Blend layers bottom-to-top (layer0 is background, layerN is foreground)
        for i, frame in enumerate(frames):
            if frame is None:
                continue  # Skip missing layers

            alpha = self.alphas[i]
            layer_data = frame.data.astype(np.float32)  # Convert to float32

            # Alpha blend: accumulator = accumulator * (1 - alpha) + layer * alpha
            # Simplified for opaque background: accumulator += layer * alpha
            if i == 0:
                # First layer: replace accumulator
                self._accumulator[:] = layer_data * alpha
            else:
                # Subsequent layers: blend over existing
                self._accumulator[:] = self._accumulator * (1 - alpha) + layer_data * alpha

        # Convert back to uint8 and clip
        result = np.clip(self._accumulator, 0, 255).astype(np.uint8)
        return result

    async def process(self, tick: TimedTick) -> None:
        """
        Read all input layers and composite into single output frame.

        Missing layers are treated as transparent (skipped).
        """
        # Read all layers
        frames = []
        for i in range(self.num_layers):
            frame = self.inputs[f'layer{i}'].read_latest()
            frames.append(frame)

        # Check if we have at least one frame
        if all(f is None for f in frames):
            return  # No input frames available

        # Composite
        composited_data = self._composite_layers(frames)

        # Create output frame
        output_frame = VideoFrame(
            data=composited_data,
            timestamp=tick.timestamp,
            frame_number=self._frame_count,
            width=self.width,
            height=self.height,
            metadata={'compositor': {'num_layers': self.num_layers, 'alphas': self.alphas}}
        )

        # Write to output
        self.outputs['video'].write(output_frame)
        self._frame_count += 1

    async def on_start(self) -> None:
        """Called when handler starts."""
        print(
            f"CompositorHandler started: {self.width}x{self.height}, "
            f"{self.num_layers} layers, alphas={self.alphas}"
        )

    async def on_stop(self) -> None:
        """Called when handler stops."""
        print(f"CompositorHandler stopped: {self._frame_count} frames composited")
