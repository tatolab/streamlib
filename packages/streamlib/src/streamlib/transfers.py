"""
Transfer handlers for memory space transitions.

These handlers move data between CPU and GPU memory spaces when
runtime capability negotiation determines a transfer is needed.

Runtime automatically inserts these when connecting handlers with
incompatible memory space capabilities.

Supports both CUDA (NVIDIA) and MPS (Apple Silicon) backends.

Example:
    # Automatic insertion by runtime
    cpu_handler.outputs['video'] # capabilities=['cpu']
    gpu_handler.inputs['video']  # capabilities=['gpu']

    # Runtime auto-inserts CPUtoGPUTransferHandler between them
    runtime.connect(cpu_handler.outputs['video'], gpu_handler.inputs['video'])
"""

import numpy as np
from typing import TYPE_CHECKING, Optional
from .handler import StreamHandler
from .ports import VideoInput, VideoOutput
from .clocks import TimedTick
from .messages import VideoFrame

if TYPE_CHECKING:
    import torch

try:
    import torch
    TORCH_AVAILABLE = True
except ImportError:
    TORCH_AVAILABLE = False


def get_available_gpu_device() -> Optional[str]:
    """
    Detect available GPU backend.

    Returns:
        'cuda:0' if NVIDIA CUDA available
        'mps' if Apple Metal (MPS) available
        None if no GPU available
    """
    if not TORCH_AVAILABLE:
        return None

    # Check CUDA (NVIDIA)
    if torch.cuda.is_available():
        return 'cuda:0'

    # Check MPS (Apple Metal)
    if hasattr(torch.backends, 'mps') and torch.backends.mps.is_available():
        return 'mps'

    return None


class CPUtoGPUTransferHandler(StreamHandler):
    """
    Transfer video frames from CPU to GPU memory.

    Supports both CUDA (NVIDIA) and MPS (Apple Silicon) backends.

    Input capabilities: ['cpu'] - accepts numpy arrays
    Output capabilities: ['gpu'] - produces torch tensors on GPU

    This handler is automatically inserted by runtime when connecting
    a CPU-only output to a GPU-only input.

    Example:
        # Runtime auto-inserts this handler
        cpu_source.outputs['video'] (cpu) → [CPUtoGPUTransfer] → gpu_filter.inputs['video'] (gpu)
    """

    def __init__(self, device: str = 'auto', handler_id: str = None):
        """
        Initialize CPU to GPU transfer handler.

        Args:
            device: PyTorch device string ('auto', 'cuda:0', 'mps', etc.)
                   'auto' (default) automatically detects available GPU
            handler_id: Optional handler ID
        """
        super().__init__(handler_id or 'cpu-to-gpu-transfer')

        if not TORCH_AVAILABLE:
            raise RuntimeError("PyTorch required for GPU transfers. Install: pip install torch")

        # Auto-detect GPU device
        if device == 'auto':
            device = get_available_gpu_device()
            if device is None:
                raise RuntimeError(
                    "No GPU available for transfer handler.\n"
                    "  - For NVIDIA GPUs: Install CUDA-enabled PyTorch\n"
                    "  - For Apple Silicon: Install PyTorch with MPS support"
                )

        self.device = torch.device(device)
        self.backend = 'CUDA' if 'cuda' in device else 'MPS' if device == 'mps' else device

        # Input: CPU only, Output: GPU only
        self.inputs['in'] = VideoInput('in', cpu_only=True)
        self.outputs['out'] = VideoOutput('out')  # GPU by default

    async def process(self, tick: TimedTick):
        """
        Transfer latest frame from CPU to GPU.

        Reads numpy array from CPU input, converts to torch tensor on GPU,
        writes to GPU output.
        """
        frame = self.inputs['in'].read_latest()
        if frame is None:
            return

        # Validate it's a VideoFrame with numpy data
        if not isinstance(frame, VideoFrame):
            print(f"[{self.handler_id}] Warning: Expected VideoFrame, got {type(frame)}")
            return

        if not isinstance(frame.data, np.ndarray):
            print(f"[{self.handler_id}] Warning: Expected numpy array, got {type(frame.data)}")
            return

        # Transfer: numpy → torch tensor on GPU
        try:
            gpu_data = torch.from_numpy(frame.data).to(self.device)

            # Create new VideoFrame with GPU data
            # Note: We're reusing VideoFrame but data is now torch.Tensor
            gpu_frame = VideoFrame(
                data=gpu_data,  # Now a torch.Tensor on GPU
                timestamp=frame.timestamp,
                frame_number=frame.frame_number,
                width=frame.width,
                height=frame.height,
                metadata=frame.metadata
            )

            self.outputs['out'].write(gpu_frame)

        except Exception as e:
            print(f"[{self.handler_id}] Transfer error: {e}")


class GPUtoCPUTransferHandler(StreamHandler):
    """
    Transfer video frames from GPU to CPU memory.

    Input capabilities: ['gpu'] - accepts torch tensors on GPU
    Output capabilities: ['cpu'] - produces numpy arrays on CPU

    This handler is automatically inserted by runtime when connecting
    a GPU-only output to a CPU-only input.

    Example:
        # Runtime auto-inserts this handler
        gpu_filter.outputs['video'] (gpu) → [GPUtoCPUTransfer] → cpu_sink.inputs['video'] (cpu)
    """

    def __init__(self, handler_id: str = None):
        """
        Initialize GPU to CPU transfer handler.

        Args:
            handler_id: Optional handler ID
        """
        super().__init__(handler_id or 'gpu-to-cpu-transfer')

        if not TORCH_AVAILABLE:
            raise RuntimeError("PyTorch required for GPU transfers. Install: pip install torch")

        # Input: GPU only, Output: CPU only
        self.inputs['in'] = VideoInput('in')  # GPU by default
        self.outputs['out'] = VideoOutput('out', cpu_only=True)

    async def process(self, tick: TimedTick):
        """
        Transfer latest frame from GPU to CPU.

        Reads torch tensor from GPU input, converts to numpy array on CPU,
        writes to CPU output.
        """
        frame = self.inputs['in'].read_latest()
        if frame is None:
            return

        # Validate it's a VideoFrame
        if not isinstance(frame, VideoFrame):
            print(f"[{self.handler_id}] Warning: Expected VideoFrame, got {type(frame)}")
            return

        # Check if data is torch.Tensor
        if not torch.is_tensor(frame.data):
            print(f"[{self.handler_id}] Warning: Expected torch.Tensor, got {type(frame.data)}")
            return

        # Transfer: torch tensor → numpy on CPU
        try:
            cpu_data = frame.data.cpu().numpy()

            # Create new VideoFrame with CPU data
            cpu_frame = VideoFrame(
                data=cpu_data,  # Now a numpy array on CPU
                timestamp=frame.timestamp,
                frame_number=frame.frame_number,
                width=frame.width,
                height=frame.height,
                metadata=frame.metadata
            )

            self.outputs['out'].write(cpu_frame)

        except Exception as e:
            print(f"[{self.handler_id}] Transfer error: {e}")


# Metal transfer handlers (macOS only)
try:
    from .metal_utils import MetalContext, check_metal_available
    HAS_METAL = True
except ImportError:
    HAS_METAL = False


class CPUtoMetalTransferHandler(StreamHandler):
    """
    Transfer video frames from CPU (numpy) to Metal textures.

    Input capabilities: ['cpu'] - accepts numpy arrays
    Output capabilities: ['metal'] - produces Metal textures

    Example:
        # Runtime auto-inserts this handler
        cpu_source.outputs['video'] (cpu) → [CPUtoMetalTransfer] → metal_blur.inputs['video'] (metal)
    """

    def __init__(self, handler_id: str = None):
        """Initialize CPU to Metal transfer handler."""
        super().__init__(handler_id or 'cpu-to-metal-transfer')

        if not HAS_METAL:
            raise RuntimeError(
                "Metal not available. Install Metal frameworks: "
                "pip install pyobjc-framework-Metal pyobjc-framework-MetalPerformanceShaders"
            )

        available, error = check_metal_available()
        if not available:
            raise RuntimeError(f"Metal not available: {error}")

        # Input: CPU only, Output: Metal/GPU
        self.inputs['in'] = VideoInput('in', cpu_only=True)
        self.outputs['out'] = VideoOutput('out')  # GPU by default (Metal backend)

        # Metal context (singleton)
        self._ctx = None

    async def process(self, tick: TimedTick):
        """Transfer latest frame from CPU to Metal texture."""
        frame = self.inputs['in'].read_latest()
        if frame is None:
            return

        # Initialize Metal context on first frame
        if self._ctx is None:
            self._ctx = MetalContext.get()

        # Validate numpy array
        if not isinstance(frame.data, np.ndarray):
            print(f"[{self.handler_id}] Warning: Expected numpy array, got {type(frame.data)}")
            return

        # Transfer: numpy → Metal texture
        try:
            metal_texture = self._ctx.numpy_to_texture(frame.data)

            # Create new VideoFrame with Metal texture
            metal_frame = VideoFrame(
                data=metal_texture,
                timestamp=frame.timestamp,
                frame_number=frame.frame_number,
                width=frame.width,
                height=frame.height,
                metadata=frame.metadata
            )

            self.outputs['out'].write(metal_frame)

        except Exception as e:
            print(f"[{self.handler_id}] Transfer error: {e}")


class MetalToCPUTransferHandler(StreamHandler):
    """
    Transfer video frames from Metal textures to CPU (numpy).

    Input capabilities: ['metal'] - accepts Metal textures
    Output capabilities: ['cpu'] - produces numpy arrays

    Example:
        # Runtime auto-inserts this handler
        metal_blur.outputs['video'] (metal) → [MetalToCPUTransfer] → cpu_display.inputs['video'] (cpu)
    """

    def __init__(self, handler_id: str = None):
        """Initialize Metal to CPU transfer handler."""
        super().__init__(handler_id or 'metal-to-cpu-transfer')

        if not HAS_METAL:
            raise RuntimeError(
                "Metal not available. Install Metal frameworks: "
                "pip install pyobjc-framework-Metal pyobjc-framework-MetalPerformanceShaders"
            )

        available, error = check_metal_available()
        if not available:
            raise RuntimeError(f"Metal not available: {error}")

        # Input: Metal/GPU, Output: CPU only
        self.inputs['in'] = VideoInput('in')  # GPU by default (Metal backend)
        self.outputs['out'] = VideoOutput('out', cpu_only=True)

        # Metal context (singleton)
        self._ctx = None

    async def process(self, tick: TimedTick):
        """Transfer latest frame from Metal texture to CPU."""
        frame = self.inputs['in'].read_latest()
        if frame is None:
            return

        # Initialize Metal context on first frame
        if self._ctx is None:
            self._ctx = MetalContext.get()

        # Validate Metal texture
        if not hasattr(frame.data, 'width') or not callable(frame.data.width):
            print(f"[{self.handler_id}] Warning: Expected Metal texture, got {type(frame.data)}")
            return

        # Transfer: Metal texture → numpy
        try:
            cpu_data = self._ctx.texture_to_numpy(frame.data, channels=3)

            # Create new VideoFrame with CPU data
            cpu_frame = VideoFrame(
                data=cpu_data,
                timestamp=frame.timestamp,
                frame_number=frame.frame_number,
                width=frame.width,
                height=frame.height,
                metadata=frame.metadata
            )

            self.outputs['out'].write(cpu_frame)

        except Exception as e:
            print(f"[{self.handler_id}] Transfer error: {e}")
