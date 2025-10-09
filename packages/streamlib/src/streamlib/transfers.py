"""
Transfer handlers for memory space transitions.

These handlers move data between CPU and GPU memory spaces when
runtime capability negotiation determines a transfer is needed.

Runtime automatically inserts these when connecting handlers with
incompatible memory space capabilities.

Example:
    # Automatic insertion by runtime
    cpu_handler.outputs['video'] # capabilities=['cpu']
    gpu_handler.inputs['video']  # capabilities=['gpu']

    # Runtime auto-inserts CPUtoGPUTransferHandler between them
    runtime.connect(cpu_handler.outputs['video'], gpu_handler.inputs['video'])
"""

import numpy as np
from typing import TYPE_CHECKING
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


class CPUtoGPUTransferHandler(StreamHandler):
    """
    Transfer video frames from CPU to GPU memory.

    Input capabilities: ['cpu'] - accepts numpy arrays
    Output capabilities: ['gpu'] - produces torch tensors on GPU

    This handler is automatically inserted by runtime when connecting
    a CPU-only output to a GPU-only input.

    Example:
        # Runtime auto-inserts this handler
        cpu_source.outputs['video'] (cpu) → [CPUtoGPUTransfer] → gpu_filter.inputs['video'] (gpu)
    """

    def __init__(self, device: str = 'cuda:0', handler_id: str = None):
        """
        Initialize CPU to GPU transfer handler.

        Args:
            device: PyTorch device string (e.g., 'cuda:0', 'cuda:1')
            handler_id: Optional handler ID
        """
        super().__init__(handler_id or 'cpu-to-gpu-transfer')

        if not TORCH_AVAILABLE:
            raise RuntimeError("PyTorch required for GPU transfers. Install: pip install torch")

        if not torch.cuda.is_available():
            raise RuntimeError("CUDA not available. Cannot create GPU transfer handler.")

        self.device = torch.device(device)

        # Input: CPU only, Output: GPU only
        self.inputs['in'] = VideoInput('in', capabilities=['cpu'])
        self.outputs['out'] = VideoOutput('out', capabilities=['gpu'])

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
        self.inputs['in'] = VideoInput('in', capabilities=['gpu'])
        self.outputs['out'] = VideoOutput('out', capabilities=['cpu'])

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
