"""
ML backend registry and auto-detection.

Automatically detects model format and routes to appropriate backend:
- .mlpackage, .mlmodel → CoreML backend (macOS only)
- .trt → TensorRT backend (NVIDIA, future)
"""

import sys
from typing import Optional
from .base import MLBackend

# Import backends based on platform
_BACKENDS = {}

# CoreML backend (macOS only)
if sys.platform == 'darwin':
    try:
        from .coreml import CoreMLBackend
        _BACKENDS['coreml'] = CoreMLBackend
    except ImportError:
        pass


def get_backend_for_model(model_path: str) -> Optional[str]:
    """
    Auto-detect appropriate backend for model file.

    Args:
        model_path: Path to model file

    Returns:
        Backend name ('coreml', 'tensorrt', etc.) or None if unsupported

    Example:
        backend_name = get_backend_for_model("yolov8n.mlpackage")
        # Returns: 'coreml'
    """
    if model_path.endswith('.mlpackage') or model_path.endswith('.mlmodel'):
        return 'coreml' if 'coreml' in _BACKENDS else None
    elif model_path.endswith('.trt'):
        return 'tensorrt' if 'tensorrt' in _BACKENDS else None
    else:
        return None


def create_backend(backend_name: str, gpu_context: 'GPUContext') -> MLBackend:
    """
    Create backend instance by name.

    Args:
        backend_name: Backend name ('coreml', 'tensorrt', etc.)
        gpu_context: GPU context for backend

    Returns:
        Backend instance

    Raises:
        ValueError: If backend not available

    Example:
        backend = create_backend('coreml', gpu_context)
    """
    if backend_name not in _BACKENDS:
        available = list(_BACKENDS.keys())
        raise ValueError(
            f"Backend '{backend_name}' not available. "
            f"Available backends: {available}"
        )

    backend_class = _BACKENDS[backend_name]
    return backend_class(gpu_context)


def list_available_backends() -> list:
    """
    List all available backends on current platform.

    Returns:
        List of backend names

    Example:
        backends = list_available_backends()
        # On macOS: ['coreml']
        # On Linux/Windows: ['tensorrt'] (future)
    """
    return list(_BACKENDS.keys())


__all__ = [
    'MLBackend',
    'get_backend_for_model',
    'create_backend',
    'list_available_backends',
]
