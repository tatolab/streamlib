"""
CoreML backend for macOS with zero-copy IOSurface inference.

Zero-copy pipeline:
1. WebGPU texture → IOSurface (GPU memory export)
2. IOSurface → CVPixelBuffer (zero-copy wrap)
3. CVPixelBuffer → CoreML (zero-copy Metal inference)

This eliminates CPU transfers entirely when using CoreML models (.mlpackage).
"""

import sys
from typing import Dict, Any, Optional
import wgpu
import ctypes
import numpy as np

from .base import MLBackend

# Platform check
if sys.platform != 'darwin':
    raise ImportError("CoreML backend only available on macOS")

try:
    import coremltools as ct
    HAS_COREML = True
except ImportError:
    HAS_COREML = False
    ct = None

try:
    from Quartz import (
        CVPixelBufferCreate,
        CVPixelBufferLockBaseAddress,
        CVPixelBufferUnlockBaseAddress,
        CVPixelBufferGetBaseAddress,
        CVPixelBufferGetIOSurface,
        kCVPixelFormatType_32BGRA,
        kCVPixelBufferIOSurfacePropertiesKey,
    )
    HAS_COREVIDEO = True
except ImportError:
    HAS_COREVIDEO = False

# Import IOSurface functions via ctypes
IOSurface = ctypes.CDLL('/System/Library/Frameworks/IOSurface.framework/IOSurface')

IOSurfaceGetWidth = IOSurface.IOSurfaceGetWidth
IOSurfaceGetWidth.restype = ctypes.c_size_t
IOSurfaceGetWidth.argtypes = [ctypes.c_void_p]

IOSurfaceGetHeight = IOSurface.IOSurfaceGetHeight
IOSurfaceGetHeight.restype = ctypes.c_size_t
IOSurfaceGetHeight.argtypes = [ctypes.c_void_p]

IOSurfaceGetBytesPerRow = IOSurface.IOSurfaceGetBytesPerRow
IOSurfaceGetBytesPerRow.restype = ctypes.c_size_t
IOSurfaceGetBytesPerRow.argtypes = [ctypes.c_void_p]

IOSurfaceGetBaseAddress = IOSurface.IOSurfaceGetBaseAddress
IOSurfaceGetBaseAddress.restype = ctypes.c_void_p
IOSurfaceGetBaseAddress.argtypes = [ctypes.c_void_p]

IOSurfaceLock = IOSurface.IOSurfaceLock
IOSurfaceLock.restype = ctypes.c_int32
IOSurfaceLock.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint32)]

IOSurfaceUnlock = IOSurface.IOSurfaceUnlock
IOSurfaceUnlock.restype = ctypes.c_int32
IOSurfaceUnlock.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(ctypes.c_uint32)]

# IOSurface lock flags
kIOSurfaceLockReadOnly = 0x00000001


class CoreMLBackend(MLBackend):
    """
    CoreML inference backend with zero-copy IOSurface pipeline.

    Uses native CoreML APIs (.mlpackage models) with CVPixelBuffer input
    for zero-copy GPU inference.

    Example:
        backend = CoreMLBackend(gpu_context)
        model = backend.load_model("yolov8n.mlpackage")
        results = backend.run(model, texture)
    """

    def __init__(self, gpu_context: 'GPUContext'):
        """Initialize CoreML backend."""
        if not HAS_COREML:
            raise RuntimeError(
                "CoreML not available. Install with: pip install coremltools"
            )
        if not HAS_COREVIDEO:
            raise RuntimeError(
                "CoreVideo not available. Install PyObjC-framework-Quartz"
            )

        self.gpu_context = gpu_context

    def load_model(self, model_path: str) -> Any:
        """
        Load CoreML model from .mlpackage or .mlmodel file.

        Args:
            model_path: Path to .mlpackage or .mlmodel

        Returns:
            CoreML model instance

        Example:
            model = backend.load_model("yolov8n.mlpackage")
        """
        if not (model_path.endswith('.mlpackage') or model_path.endswith('.mlmodel')):
            raise ValueError(
                f"CoreML backend requires .mlpackage or .mlmodel file, got: {model_path}"
            )

        # Load model with Metal compute backend for GPU acceleration
        model = ct.models.MLModel(model_path, compute_units=ct.ComputeUnit.ALL)

        print(f"[CoreML] Loaded model: {model_path}")
        print(f"[CoreML] Using Metal GPU acceleration")

        return model

    def run(
        self,
        model: Any,
        input_texture: wgpu.GPUTexture,
        preprocess: bool = True
    ) -> Dict[str, Any]:
        """
        Run CoreML inference with IOSurface-backed CVPixelBuffer.

        Pipeline (single-copy like camera):
        1. Create CVPixelBuffer with IOSurface backing
        2. Lock IOSurface for CPU write
        3. Read wgpu texture → IOSurface memory (single copy!)
        4. Unlock IOSurface
        5. Pass CVPixelBuffer to CoreML (Metal inference, no copy!)

        Args:
            model: CoreML model from load_model()
            input_texture: Input video frame (GPU texture)
            preprocess: If True, automatically resize/normalize for model

        Returns:
            Dictionary of model outputs
        """
        # Get texture dimensions
        width = input_texture.size[0]
        height = input_texture.size[1]

        # Read texture to CPU buffer
        bytes_per_pixel = 4  # BGRA format
        row_bytes = bytes_per_pixel * width
        buffer_size = row_bytes * height

        # Create output buffer for texture read
        output_buffer = self.gpu_context.device.create_buffer(
            size=buffer_size,
            usage=wgpu.BufferUsage.COPY_DST | wgpu.BufferUsage.MAP_READ
        )

        # Copy texture to buffer
        encoder = self.gpu_context.device.create_command_encoder()
        encoder.copy_texture_to_buffer(
            {"texture": input_texture},
            {"buffer": output_buffer, "bytes_per_row": row_bytes, "rows_per_image": height},
            (width, height, 1)
        )
        self.gpu_context.device.queue.submit([encoder.finish()])

        # Map buffer and read data
        output_buffer.map(mode=wgpu.MapMode.READ)
        data = bytes(output_buffer.read_mapped())
        output_buffer.unmap()
        output_buffer.destroy()

        # Convert data to PIL.Image for CoreML
        # The model expects PIL.Image input (configured during YOLO export)
        from PIL import Image

        # Convert BGRA bytes to PIL.Image
        # data is in BGRA format from WebGPU texture
        image = Image.frombytes('RGBA', (width, height), data)

        # Convert RGBA to RGB (CoreML/YOLO expects RGB)
        image = image.convert('RGB')

        # Resize to YOLO input size (640x640) if preprocessing enabled
        if preprocess:
            image = image.resize((640, 640), Image.Resampling.BILINEAR)

        # Get model input name
        spec = model.get_spec()
        input_name = spec.description.input[0].name

        # Run CoreML inference with PIL.Image
        # CoreML will automatically use Metal backend for GPU acceleration
        prediction = model.predict({input_name: image})

        # CoreML YOLOv8 export returns NMS-processed detections:
        # - coordinates: (N, 4) - normalized [0,1] center_x, center_y, width, height
        # - confidence: (N, 80) - class scores for N detections
        # This is DIFFERENT from ONNX which returns raw output (1, 84, 8400)

        # Convert MLMultiArray to numpy arrays
        # MLMultiArray implements __array__() so np.asarray() should work
        outputs = {}
        for key, value in prediction.items():
            try:
                # Use np.asarray for efficient conversion
                outputs[key] = np.asarray(value)
            except (TypeError, AttributeError):
                # If conversion fails, keep original value
                outputs[key] = value

        return outputs

    def get_backend_name(self) -> str:
        """Get backend name."""
        return "CoreML (Metal GPU)"

    def is_available(self) -> bool:
        """Check if CoreML is available."""
        return sys.platform == 'darwin' and HAS_COREML and HAS_COREVIDEO


# Export for backend registry
__all__ = ['CoreMLBackend']
