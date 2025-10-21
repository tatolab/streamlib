"""
ML inference with multi-backend support.

Provides unified ML inference API with automatic backend selection:
- CoreML backend (macOS): Zero-copy inference with .mlpackage models
- ONNX backend (cross-platform): ONNX Runtime with platform-specific acceleration
- TensorRT backend (future): NVIDIA GPU acceleration

Example:
    from streamlib.gpu import GPUContext

    gpu = await GPUContext.create()
    ml = gpu.ml  # MLRuntime instance

    # Auto-detects backend based on model format
    model = ml.load_model("yolov8n.onnx")  # Uses ONNX backend
    model = ml.load_model("yolov8n.mlpackage")  # Uses CoreML backend (macOS)

    # Unified inference API
    results = ml.run(model, texture, preprocess=True)
"""

from .runtime import MLRuntime, ONNXModel, LoadedModel
from .backends import list_available_backends

__all__ = [
    'MLRuntime',
    'ONNXModel',
    'LoadedModel',
    'list_available_backends',
]
