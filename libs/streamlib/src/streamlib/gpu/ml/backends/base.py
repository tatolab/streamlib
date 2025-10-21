"""
Base interface for ML inference backends.

All backends must implement this interface to be compatible with MLRuntime.
"""

from abc import ABC, abstractmethod
from typing import Dict, Any, Optional
import wgpu


class MLBackend(ABC):
    """
    Abstract base class for ML inference backends.

    Each backend (CoreML, ONNX, TensorRT, etc.) implements this interface
    to provide unified access to different ML frameworks.
    """

    @abstractmethod
    def __init__(self, gpu_context: 'GPUContext'):
        """
        Initialize backend with GPU context.

        Args:
            gpu_context: Parent GPU context for zero-copy operations
        """
        pass

    @abstractmethod
    def load_model(self, model_path: str) -> Any:
        """
        Load model from file.

        Args:
            model_path: Path to model file (.mlpackage, .onnx, .trt, etc.)

        Returns:
            Backend-specific model handle

        Raises:
            RuntimeError: If model cannot be loaded
        """
        pass

    @abstractmethod
    def run(
        self,
        model: Any,
        input_texture: wgpu.GPUTexture,
        preprocess: bool = True
    ) -> Dict[str, Any]:
        """
        Run inference on input texture.

        Args:
            model: Model handle from load_model()
            input_texture: Input video frame (GPU texture)
            preprocess: If True, automatically resize/normalize for model

        Returns:
            Dictionary of output tensors/results

        Example:
            {
                'boxes': [(x, y, w, h), ...],
                'classes': [0, 15, 2, ...],
                'scores': [0.95, 0.87, ...]
            }
        """
        pass

    @abstractmethod
    def get_backend_name(self) -> str:
        """
        Get human-readable backend name.

        Returns:
            Backend name (e.g., "CoreML", "ONNX Runtime", "TensorRT")
        """
        pass

    @abstractmethod
    def is_available(self) -> bool:
        """
        Check if backend is available on current platform.

        Returns:
            True if backend can be used, False otherwise
        """
        pass
