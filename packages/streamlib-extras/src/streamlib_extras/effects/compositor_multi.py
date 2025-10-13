"""
Multi-input compositor for combining multiple video streams.

Demonstrates streamlib's composability: independent pipelines feeding into
a single compositor, like Unix pipes for video.
"""

import torch
from typing import List, Literal, Optional, Tuple
from enum import Enum

from streamlib.handler import StreamHandler
from streamlib.ports import VideoInput, VideoOutput
from streamlib.messages import VideoFrame
from streamlib.clocks import TimedTick


class CompositeMode(Enum):
    """Compositor layout modes."""
    ALPHA_BLEND = "alpha_blend"          # Layer inputs with alpha blending
    PICTURE_IN_PICTURE = "pip"           # Small overlay in corner
    SIDE_BY_SIDE = "side_by_side"        # Inputs arranged horizontally
    VERTICAL_STACK = "vertical_stack"    # Inputs stacked vertically
    GRID = "grid"                        # 2x2 or 3x3 grid layout


class MultiInputCompositor(StreamHandler):
    """
    GPU-accelerated compositor for combining multiple video streams.

    Demonstrates streamlib's Unix-pipe philosophy for video:
    - Multiple independent pipelines → one compositor
    - Pure GPU operations (MPS)
    - Flexible layout modes

    Example:
        ```python
        # Pipeline 1: Camera → Blur
        camera = CameraHandlerGPU()
        blur = BlurFilterGPU()

        # Pipeline 2: Lower thirds overlay
        lower_thirds = LowerThirdsGPU()

        # Compositor combines them
        compositor = MultiInputCompositor(
            num_inputs=2,
            mode='alpha_blend',
            width=1920,
            height=1080
        )

        # Wire it up
        runtime.connect(camera.outputs['video'], blur.inputs['video'])
        runtime.connect(blur.outputs['video'], compositor.inputs['input_0'])
        runtime.connect(lower_thirds.outputs['video'], compositor.inputs['input_1'])
        runtime.connect(compositor.outputs['video'], display.inputs['video'])
        ```

    Compositing Modes:
        - alpha_blend: Layer inputs with alpha blending (input_1 over input_0)
        - pip: Picture-in-picture (input_1 in corner of input_0)
        - side_by_side: Arrange inputs horizontally
        - vertical_stack: Stack inputs vertically
        - grid: 2x2 or 3x3 grid layout
    """

    preferred_dispatcher = 'asyncio'  # GPU operations are non-blocking

    def __init__(
        self,
        num_inputs: int = 2,
        mode: Literal['alpha_blend', 'pip', 'side_by_side', 'vertical_stack', 'grid'] = 'alpha_blend',
        width: int = 1920,
        height: int = 1080,
        pip_position: Literal['top_left', 'top_right', 'bottom_left', 'bottom_right'] = 'bottom_right',
        pip_scale: float = 0.25,
        name: str = 'compositor-multi'
    ):
        """
        Initialize multi-input compositor.

        Args:
            num_inputs: Number of input ports (2-4)
            mode: Compositing mode
            width: Output frame width
            height: Output frame height
            pip_position: Corner for picture-in-picture mode
            pip_scale: Scale factor for PIP overlay (0.0-1.0)
            name: Handler identifier
        """
        super().__init__(name)

        if num_inputs < 2 or num_inputs > 4:
            raise ValueError("num_inputs must be between 2 and 4")

        self.num_inputs = num_inputs
        self.mode = CompositeMode(mode)
        self.width = width
        self.height = height
        self.pip_position = pip_position
        self.pip_scale = pip_scale

        # Create input ports dynamically
        for i in range(num_inputs):
            port_name = f'input_{i}'
            self.inputs[port_name] = VideoInput(port_name)

        # Output port
        self.outputs['video'] = VideoOutput('video')

        # GPU device (detected at runtime)
        self.device = None
        self.frame_count = 0

        # Cache for GPU tensors (to avoid repeated CPU→GPU transfers)
        self._gpu_tensor_cache: dict[int, torch.Tensor] = {}

    async def on_start(self):
        """Initialize GPU device."""
        # Detect GPU backend
        if torch.backends.mps.is_available():
            self.device = torch.device('mps')
            print(f"[{self.handler_id}] GPU compositor initialized (MPS, {self.num_inputs} inputs, mode={self.mode.value})")
        elif torch.cuda.is_available():
            self.device = torch.device('cuda')
            print(f"[{self.handler_id}] GPU compositor initialized (CUDA, {self.num_inputs} inputs, mode={self.mode.value})")
        else:
            self.device = torch.device('cpu')
            print(f"[{self.handler_id}] Compositor initialized (CPU fallback, {self.num_inputs} inputs, mode={self.mode.value})")

    def _ensure_gpu_tensor(self, frame_data, input_idx: int) -> torch.Tensor:
        """
        Convert frame data to GPU tensor if needed.

        For CPU frames, caches static frames to avoid repeated CPU→GPU transfers.
        Detects static frames by comparing shape and a few pixel samples.
        """
        if isinstance(frame_data, torch.Tensor):
            # Already a tensor, just ensure correct device
            if frame_data.device != self.device:
                return frame_data.to(self.device)
            return frame_data
        else:
            # NumPy array - check cache first
            cache_key = input_idx

            # Check if we have a cached version
            if cache_key in self._gpu_tensor_cache:
                cached = self._gpu_tensor_cache[cache_key]

                # Verify it's still the same frame (compare shape and a few pixels)
                # This catches static test patterns without expensive full comparison
                if (cached.shape[0] == frame_data.shape[0] and
                    cached.shape[1] == frame_data.shape[1] and
                    cached[0, 0, 0].item() == frame_data[0, 0, 0] and
                    cached[-1, -1, -1].item() == frame_data[-1, -1, -1] and
                    cached[cached.shape[0]//2, cached.shape[1]//2, 0].item() == frame_data[frame_data.shape[0]//2, frame_data.shape[1]//2, 0]):
                    # Frame unchanged, reuse cached GPU tensor (zero CPU→GPU transfer!)
                    return cached

            # New or different frame - convert and cache
            was_cached = cache_key in self._gpu_tensor_cache
            gpu_tensor = torch.from_numpy(frame_data).to(self.device)
            self._gpu_tensor_cache[cache_key] = gpu_tensor

            if not was_cached:
                print(f"[{self.handler_id}] Cached GPU tensor for input_{input_idx} (static frame optimization)")

            return gpu_tensor

    def _resize_to_fit(self, tensor: torch.Tensor, target_height: int, target_width: int) -> torch.Tensor:
        """Resize tensor to target size using GPU interpolation."""
        if tensor.shape[0] == target_height and tensor.shape[1] == target_width:
            return tensor

        # PyTorch expects (N, C, H, W) for interpolation
        # Our tensors are (H, W, C)
        tensor_nchw = tensor.permute(2, 0, 1).unsqueeze(0).float() / 255.0

        # Resize
        resized = torch.nn.functional.interpolate(
            tensor_nchw,
            size=(target_height, target_width),
            mode='bilinear',
            align_corners=False
        )

        # Convert back to (H, W, C)
        result = (resized.squeeze(0).permute(1, 2, 0) * 255.0).to(torch.uint8)
        return result

    def _composite_alpha_blend(self, frames: List[torch.Tensor]) -> torch.Tensor:
        """Alpha blend inputs (layer input_1 over input_0)."""
        # Use first frame as base
        base = self._resize_to_fit(frames[0], self.height, self.width).float() / 255.0

        # Layer remaining frames on top
        for overlay_frame in frames[1:]:
            overlay = self._resize_to_fit(overlay_frame, self.height, self.width).float() / 255.0

            # Simple alpha blend (assume uniform alpha for now)
            # For proper alpha, we'd need RGBA frames
            alpha = 0.7  # Overlay alpha
            base = base * (1 - alpha) + overlay * alpha

        return (base * 255.0).to(torch.uint8)

    def _composite_picture_in_picture(self, frames: List[torch.Tensor]) -> torch.Tensor:
        """Picture-in-picture: small overlay in corner."""
        # Base frame (input_0)
        base = self._resize_to_fit(frames[0], self.height, self.width)

        if len(frames) < 2:
            return base

        # Overlay frame (input_1) - scaled down
        pip_height = int(self.height * self.pip_scale)
        pip_width = int(self.width * self.pip_scale)
        overlay = self._resize_to_fit(frames[1], pip_height, pip_width)

        # Position based on corner
        if self.pip_position == 'top_left':
            y, x = 10, 10
        elif self.pip_position == 'top_right':
            y, x = 10, self.width - pip_width - 10
        elif self.pip_position == 'bottom_left':
            y, x = self.height - pip_height - 10, 10
        else:  # bottom_right
            y, x = self.height - pip_height - 10, self.width - pip_width - 10

        # Composite overlay onto base
        result = base.clone()
        result[y:y+pip_height, x:x+pip_width, :] = overlay

        return result

    def _composite_side_by_side(self, frames: List[torch.Tensor]) -> torch.Tensor:
        """Arrange inputs horizontally."""
        # Each input gets equal width
        input_width = self.width // len(frames)

        # Resize all frames to same height, proportional width
        resized = []
        for frame in frames:
            resized.append(self._resize_to_fit(frame, self.height, input_width))

        # Concatenate horizontally
        return torch.cat(resized, dim=1)

    def _composite_vertical_stack(self, frames: List[torch.Tensor]) -> torch.Tensor:
        """Stack inputs vertically."""
        # Each input gets equal height
        input_height = self.height // len(frames)

        # Resize all frames
        resized = []
        for frame in frames:
            resized.append(self._resize_to_fit(frame, input_height, self.width))

        # Concatenate vertically
        return torch.cat(resized, dim=0)

    def _composite_grid(self, frames: List[torch.Tensor]) -> torch.Tensor:
        """Arrange inputs in grid (2x2 or 2x1 depending on count)."""
        if len(frames) == 2:
            # 2x1 grid (side by side)
            return self._composite_side_by_side(frames)
        elif len(frames) == 3:
            # Top: 2 side by side, Bottom: 1 centered
            top_height = self.height // 2
            top_width = self.width // 2

            top_left = self._resize_to_fit(frames[0], top_height, top_width)
            top_right = self._resize_to_fit(frames[1], top_height, top_width)
            bottom = self._resize_to_fit(frames[2], top_height, self.width)

            top = torch.cat([top_left, top_right], dim=1)
            return torch.cat([top, bottom], dim=0)
        else:  # 4 inputs
            # 2x2 grid
            cell_height = self.height // 2
            cell_width = self.width // 2

            # Resize all
            cells = [self._resize_to_fit(f, cell_height, cell_width) for f in frames]

            # Arrange in 2x2
            top = torch.cat([cells[0], cells[1]], dim=1)
            bottom = torch.cat([cells[2], cells[3]], dim=1)
            return torch.cat([top, bottom], dim=0)

    async def process(self, tick: TimedTick):
        """Composite all input frames."""
        # Read all inputs
        input_frames = []
        for i in range(self.num_inputs):
            port_name = f'input_{i}'
            frame_msg = self.inputs[port_name].read_latest()

            if frame_msg is None:
                # Missing input - skip this tick
                if tick.frame_number % 30 == 0:  # Log every second
                    print(f"[{self.handler_id}] Waiting for input_{i}")
                return

            input_frames.append((i, frame_msg.data))

        # Ensure all frames are GPU tensors (with caching for static frames)
        gpu_frames = [self._ensure_gpu_tensor(data, idx) for idx, data in input_frames]

        # Composite based on mode
        if self.mode == CompositeMode.ALPHA_BLEND:
            result = self._composite_alpha_blend(gpu_frames)
        elif self.mode == CompositeMode.PICTURE_IN_PICTURE:
            result = self._composite_picture_in_picture(gpu_frames)
        elif self.mode == CompositeMode.SIDE_BY_SIDE:
            result = self._composite_side_by_side(gpu_frames)
        elif self.mode == CompositeMode.VERTICAL_STACK:
            result = self._composite_vertical_stack(gpu_frames)
        elif self.mode == CompositeMode.GRID:
            result = self._composite_grid(gpu_frames)
        else:
            raise ValueError(f"Unknown composite mode: {self.mode}")

        # Output composited frame
        output_frame = VideoFrame(
            data=result,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height,
            metadata={'source': 'compositor-multi', 'mode': self.mode.value}
        )

        self.outputs['video'].write(output_frame)
        self.frame_count += 1

        if self.frame_count <= 3:
            print(f"[{self.handler_id}] ✅ Frame {self.frame_count} composited: {self.width}x{self.height}")

    async def on_stop(self):
        """Cleanup."""
        print(f"[{self.handler_id}] Compositor stopped ({self.frame_count} frames composited)")
