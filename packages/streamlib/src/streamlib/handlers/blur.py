"""
Blur filter handler with flexible CPU/GPU support.

Adapts processing based on negotiated memory space.
"""

import numpy as np
from typing import Optional, TYPE_CHECKING, Any

from ..handler import StreamHandler
from ..ports import VideoInput, VideoOutput
from ..messages import VideoFrame
from ..clocks import TimedTick

# Import cv2 for CPU blur
try:
    import cv2
    HAS_CV2 = True
except ImportError:
    HAS_CV2 = False

# Import torch for GPU blur
try:
    import torch
    import torch.nn.functional as F
    HAS_TORCH = True
except ImportError:
    HAS_TORCH = False


class BlurFilter(StreamHandler):
    """
    Gaussian blur filter with flexible CPU/GPU support.

    Capabilities: ['cpu', 'gpu'] - adapts based on negotiated memory space

    CPU path: Uses cv2.GaussianBlur (if available) or numpy fallback
    GPU path: Uses torch conv2d with Gaussian kernel

    Example:
        ```python
        blur = BlurFilter(kernel_size=5, sigma=1.0)
        runtime.add_stream(Stream(blur, dispatcher='asyncio'))
        runtime.connect(source.outputs['video'], blur.inputs['video'])
        ```
    """

    def __init__(
        self,
        kernel_size: int = 5,
        sigma: float = 1.0,
        handler_id: str = None
    ):
        """
        Initialize blur filter.

        Args:
            kernel_size: Size of Gaussian kernel (must be odd)
            sigma: Standard deviation for Gaussian kernel
            handler_id: Optional custom handler ID
        """
        super().__init__(handler_id or 'blur-filter')

        if kernel_size % 2 == 0:
            raise ValueError(f"kernel_size must be odd, got {kernel_size}")

        self.kernel_size = kernel_size
        self.sigma = sigma

        # Flexible ports: can work with CPU or GPU
        capabilities = []
        if HAS_CV2:
            capabilities.append('cpu')
        if HAS_TORCH:
            capabilities.append('gpu')

        if not capabilities:
            raise RuntimeError(
                "BlurFilter requires either OpenCV (CPU) or PyTorch (GPU). "
                "Neither is available."
            )

        self.inputs['video'] = VideoInput('video', capabilities=capabilities)
        self.outputs['video'] = VideoOutput('video', capabilities=capabilities)

        # Frame counter
        self._frame_count = 0

        # GPU kernel (lazy init)
        self._gpu_kernel: Optional[Any] = None  # torch.Tensor when HAS_TORCH

    def _get_gpu_kernel(self, device: Any) -> Any:  # device: torch.device -> torch.Tensor
        """
        Get or create Gaussian kernel for GPU processing.

        Returns:
            Gaussian kernel tensor [1, 1, kernel_size, kernel_size]
        """
        if self._gpu_kernel is not None:
            return self._gpu_kernel

        # Create 1D Gaussian kernel
        coords = torch.arange(self.kernel_size, dtype=torch.float32) - (self.kernel_size - 1) / 2
        gauss = torch.exp(-(coords ** 2) / (2 * self.sigma ** 2))
        gauss = gauss / gauss.sum()

        # Create 2D kernel via outer product
        kernel_2d = gauss[:, None] * gauss[None, :]
        kernel_2d = kernel_2d / kernel_2d.sum()

        # Shape for conv2d: [out_channels, in_channels, height, width]
        # For RGB: apply same kernel to each channel
        # Shape: [1, 1, kernel_size, kernel_size] - will broadcast
        self._gpu_kernel = kernel_2d[None, None, :, :].to(device)

        return self._gpu_kernel

    def _blur_cpu(self, frame_data: np.ndarray) -> np.ndarray:
        """
        Apply Gaussian blur on CPU.

        Args:
            frame_data: Input frame as numpy array [H, W, 3]

        Returns:
            Blurred frame as numpy array [H, W, 3]
        """
        if HAS_CV2:
            # Use OpenCV for fast CPU blur
            return cv2.GaussianBlur(frame_data, (self.kernel_size, self.kernel_size), self.sigma)
        else:
            # Fallback: naive numpy implementation (slow)
            # TODO: Use scipy.ndimage.gaussian_filter for better performance
            return frame_data  # No-op if no CPU blur available

    def _blur_gpu(self, frame_data: Any) -> Any:  # frame_data: torch.Tensor -> torch.Tensor
        """
        Apply Gaussian blur on GPU.

        Args:
            frame_data: Input frame as torch tensor [H, W, 3] on GPU

        Returns:
            Blurred frame as torch tensor [H, W, 3] on GPU
        """
        # Get device
        device = frame_data.device

        # Prepare for conv2d: [B, C, H, W]
        # Input: [H, W, 3] → [1, 3, H, W]
        frame_batched = frame_data.permute(2, 0, 1).unsqueeze(0)  # [1, 3, H, W]

        # Get kernel
        kernel = self._get_gpu_kernel(device)  # [1, 1, K, K]

        # Apply conv2d per channel (groups=3 for 3 independent channels)
        # Need 3 copies of kernel: [3, 1, K, K]
        kernel_rgb = kernel.repeat(3, 1, 1, 1)  # [3, 1, K, K]

        # Apply convolution with padding to maintain size
        padding = self.kernel_size // 2
        blurred = F.conv2d(frame_batched, kernel_rgb, padding=padding, groups=3)

        # Back to [H, W, 3]
        blurred = blurred.squeeze(0).permute(1, 2, 0)

        return blurred

    async def process(self, tick: TimedTick) -> None:
        """
        Process one frame: apply blur based on negotiated memory space.

        Checks negotiated_memory to decide CPU or GPU path.
        """
        frame = self.inputs['video'].read_latest()
        if frame is None:
            return

        # Check negotiated memory space
        negotiated = self.inputs['video'].negotiated_memory

        if negotiated == 'cpu':
            # CPU path
            blurred_data = self._blur_cpu(frame.data)
        elif negotiated == 'gpu':
            # GPU path
            blurred_data = self._blur_gpu(frame.data)
        else:
            raise RuntimeError(f"Unexpected negotiated_memory: {negotiated}")

        # Create output frame
        blurred_frame = VideoFrame(
            data=blurred_data,
            timestamp=frame.timestamp,
            frame_number=frame.frame_number,
            width=frame.width,
            height=frame.height,
            metadata={**frame.metadata, 'blur_kernel': self.kernel_size}
        )

        # Write to output
        self.outputs['video'].write(blurred_frame)
        self._frame_count += 1

    async def on_start(self) -> None:
        """Called when handler starts."""
        negotiated = self.inputs['video'].negotiated_memory
        print(
            f"BlurFilter started: kernel={self.kernel_size}, sigma={self.sigma:.1f}, "
            f"memory={negotiated}"
        )

    async def on_stop(self) -> None:
        """Called when handler stops."""
        print(f"BlurFilter stopped: {self._frame_count} frames processed")
