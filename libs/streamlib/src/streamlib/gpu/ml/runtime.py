"""
Unified ML runtime with automatic backend selection.

Provides a single API that automatically routes to the best backend
based on model format and platform:

- .mlpackage, .mlmodel → CoreML (macOS, zero-copy)
- .onnx → ONNX Runtime (cross-platform, CPU transfer)
- .trt → TensorRT (NVIDIA, future)

Example:
    ml = MLRuntime(gpu_context)

    # Auto-detects format and selects backend
    model = ml.load_model("yolov8n.onnx")  # Uses ONNX backend
    model = ml.load_model("yolov8n.mlpackage")  # Uses CoreML backend

    # Unified inference API
    results = ml.run(model, texture, preprocess=True)
"""

from typing import Dict, Any, Optional, Tuple
import wgpu

from .backends import (
    get_backend_for_model,
    create_backend,
    list_available_backends,
    MLBackend
)


class LoadedModel:
    """
    Wrapper for loaded model with backend information.

    Keeps track of which backend was used to load the model,
    so run() can route to the correct backend.
    """

    def __init__(self, backend: MLBackend, model_handle: Any, model_path: str):
        """
        Initialize loaded model wrapper.

        Args:
            backend: Backend instance that loaded the model
            model_handle: Backend-specific model handle
            model_path: Path to model file
        """
        self.backend = backend
        self.model_handle = model_handle
        self.model_path = model_path

    def __repr__(self) -> str:
        return f"LoadedModel(backend={self.backend.get_backend_name()}, path={self.model_path})"


class MLRuntime:
    """
    Unified ML runtime with automatic backend selection.

    Automatically selects the best backend based on:
    1. Model format (.mlpackage → CoreML, .onnx → ONNX)
    2. Platform availability (CoreML on macOS only)
    3. Performance (native backends preferred)

    Example:
        ml = MLRuntime(gpu_context)

        # Load any supported model format
        model = ml.load_model("yolov8n.onnx")

        # Run inference with automatic backend routing
        results = ml.run(model, texture, preprocess=True)
    """

    def __init__(self, gpu_context: 'GPUContext'):
        """
        Initialize ML runtime.

        Args:
            gpu_context: Parent GPU context for zero-copy operations
        """
        self.gpu_context = gpu_context
        self._backends = {}  # Cache backend instances

        # Log available backends
        available = list_available_backends()
        print(f"[MLRuntime] Available backends: {available}")

    def _get_backend(self, backend_name: str) -> MLBackend:
        """
        Get or create backend instance.

        Args:
            backend_name: Backend name ('coreml', 'onnx', etc.)

        Returns:
            Backend instance (cached)
        """
        if backend_name not in self._backends:
            self._backends[backend_name] = create_backend(backend_name, self.gpu_context)
        return self._backends[backend_name]

    def load_model(self, model_path: str, backend: Optional[str] = None) -> LoadedModel:
        """
        Load ML model with automatic or explicit backend selection.

        Args:
            model_path: Path to model file
            backend: Optional explicit backend name (auto-detects if None)

        Returns:
            LoadedModel wrapper with backend and model handle

        Raises:
            ValueError: If model format not supported or backend not available

        Example:
            # Auto-detect backend
            model = ml.load_model("yolov8n.onnx")

            # Explicit backend
            model = ml.load_model("yolov8n.onnx", backend='onnx')
        """
        # Auto-detect backend if not specified
        if backend is None:
            backend = get_backend_for_model(model_path)
            if backend is None:
                raise ValueError(
                    f"Unsupported model format: {model_path}\n"
                    f"Supported formats: .mlpackage, .mlmodel (macOS), .onnx"
                )

        # Get or create backend instance
        backend_instance = self._get_backend(backend)

        # Load model
        model_handle = backend_instance.load_model(model_path)

        # Wrap in LoadedModel
        return LoadedModel(backend_instance, model_handle, model_path)

    def run(
        self,
        model: LoadedModel,
        input_texture: wgpu.GPUTexture,
        preprocess: bool = True
    ) -> Dict[str, Any]:
        """
        Run ML inference with automatic backend routing.

        Args:
            model: LoadedModel from load_model()
            input_texture: Input video frame (GPU texture)
            preprocess: If True, automatically resize/normalize for model

        Returns:
            Dictionary of model outputs (format depends on model and backend)

        Example:
            results = ml.run(model, texture, preprocess=True)
        """
        # Route to backend's run() method
        return model.backend.run(model.model_handle, input_texture, preprocess)


# Legacy ONNXModel wrapper for backwards compatibility
class ONNXModel:
    """
    Convenience wrapper for ONNX models (backwards compatibility).

    Example:
        model = ONNXModel("yolov8n.onnx", gpu_context)
        results = model.run(frame.data)
    """

    def __init__(self, model_path: str, gpu_context: 'GPUContext'):
        """
        Initialize ONNX model wrapper.

        Args:
            model_path: Path to .onnx file
            gpu_context: GPU context for ML runtime
        """
        self.gpu_context = gpu_context
        self.ml_runtime = MLRuntime(gpu_context)
        self.model = self.ml_runtime.load_model(model_path, backend='onnx')

    def run(self, input_texture: wgpu.GPUTexture) -> Dict[str, Any]:
        """
        Run model inference.

        Args:
            input_texture: Input frame (GPU texture)

        Returns:
            Model outputs as dictionary
        """
        return self.ml_runtime.run(self.model, input_texture, preprocess=True)


__all__ = ['MLRuntime', 'ONNXModel', 'LoadedModel']
