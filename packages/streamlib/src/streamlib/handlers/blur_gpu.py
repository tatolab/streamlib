"""
GPU-only blur filter handler.

Forces GPU processing to demonstrate transfer handler insertion.
"""

import numpy as np
from typing import Optional, Any

from ..handler import StreamHandler
from ..ports import VideoInput, VideoOutput
from ..messages import VideoFrame
from ..clocks import TimedTick

# Import torch for GPU blur
try:
    import torch
    import torch.nn.functional as F
    HAS_TORCH = True
except ImportError:
    HAS_TORCH = False


class BlurFilterGPU(StreamHandler):
    """
    GPU-only Gaussian blur filter.

    Capabilities: ['gpu'] only - forces transfer handler insertion

    Unlike BlurFilter (flexible), this handler ONLY works on GPU.
    Runtime must insert CPUtoGPU transfer before and GPUtoCPU after.

    Example:
        ```python
        blur = BlurFilterGPU(kernel_size=15, sigma=3.0)
        runtime.add_stream(Stream(blur, dispatcher='asyncio'))
        
        # Runtime will auto-insert transfers:
        # cpu_source → [CPUtoGPU] → blur_gpu → [GPUtoCPU] → cpu_sink
        runtime.connect(cpu_source.outputs['video'], blur.inputs['video'])
        ```
    """

    def __init__(
        self,
        kernel_size: int = 5,
        sigma: float = 1.0,
        device: str = 'cuda:0',
        handler_id: str = None
    ):
        """
        Initialize GPU-only blur filter.

        Args:
            kernel_size: Size of Gaussian kernel (must be odd)
            sigma: Standard deviation for Gaussian kernel
            device: CUDA device (e.g. 'cuda:0')
            handler_id: Optional custom handler ID
        """
        super().__init__(handler_id or 'blur-gpu')

        if not HAS_TORCH:
            raise RuntimeError("BlurFilterGPU requires PyTorch. Install with: pip install torch")

        if not torch.cuda.is_available():
            raise RuntimeError("BlurFilterGPU requires CUDA. No CUDA device available.")

        if kernel_size % 2 == 0:
            raise ValueError(f"kernel_size must be odd, got {kernel_size}")

        self.kernel_size = kernel_size
        self.sigma = sigma
        self.device = torch.device(device)

        # GPU-only ports
        self.inputs['video'] = VideoInput('video', capabilities=['gpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])

        # Frame counter
        self._frame_count = 0

        # GPU kernel (lazy init)
        self._gpu_kernel: Optional[Any] = None

    def _get_gpu_kernel(self) -> Any:
        """
        Get or create Gaussian kernel for GPU processing.

        Returns:
            Gaussian kernel tensor [1, 1, kernel_size, kernel_size] on GPU
        """
        if self._gpu_kernel is not None:
            return self._gpu_kernel

        # Create 1D Gaussian kernel
        coords = torch.arange(self.kernel_size, dtype=torch.float32, device=self.device)
        coords = coords - (self.kernel_size - 1) / 2
        gauss = torch.exp(-(coords ** 2) / (2 * self.sigma ** 2))
        gauss = gauss / gauss.sum()

        # Create 2D kernel via outer product
        kernel_2d = gauss[:, None] * gauss[None, :]
        kernel_2d = kernel_2d / kernel_2d.sum()

        # Shape for conv2d: [1, 1, kernel_size, kernel_size]
        self._gpu_kernel = kernel_2d[None, None, :, :]

        return self._gpu_kernel

    def _blur_gpu(self, frame_data: Any) -> Any:
        """
        Apply Gaussian blur on GPU.

        Args:
            frame_data: Input frame as torch tensor [H, W, 3] on GPU

        Returns:
            Blurred frame as torch tensor [H, W, 3] on GPU
        """
        # Prepare for conv2d: [B, C, H, W]
        # Input: [H, W, 3] → [1, 3, H, W]
        frame_batched = frame_data.permute(2, 0, 1).unsqueeze(0)

        # Get kernel
        kernel = self._get_gpu_kernel()

        # Apply conv2d per channel (groups=3 for 3 independent channels)
        kernel_rgb = kernel.repeat(3, 1, 1, 1)  # [3, 1, K, K]

        # Apply convolution with padding to maintain size
        padding = self.kernel_size // 2
        blurred = F.conv2d(frame_batched, kernel_rgb, padding=padding, groups=3)

        # Back to [H, W, 3]
        blurred = blurred.squeeze(0).permute(1, 2, 0)

        return blurred

    async def process(self, tick: TimedTick) -> None:
        """
        Process one frame on GPU.

        Expects GPU tensor input, produces GPU tensor output.
        """
        frame = self.inputs['video'].read_latest()
        if frame is None:
            return

        # Apply GPU blur
        blurred_data = self._blur_gpu(frame.data)

        # Create output frame
        blurred_frame = VideoFrame(
            data=blurred_data,
            timestamp=frame.timestamp,
            frame_number=frame.frame_number,
            width=frame.width,
            height=frame.height,
            metadata={**frame.metadata, 'blur_kernel': self.kernel_size, 'gpu': True}
        )

        # Write to output
        self.outputs['video'].write(blurred_frame)
        self._frame_count += 1

    async def on_start(self) -> None:
        """Called when handler starts."""
        print(
            f"BlurFilterGPU started: kernel={self.kernel_size}, sigma={self.sigma:.1f}, "
            f"device={self.device}"
        )

    async def on_stop(self) -> None:
        """Called when handler stops."""
        print(f"BlurFilterGPU stopped: {self._frame_count} frames processed on GPU")
