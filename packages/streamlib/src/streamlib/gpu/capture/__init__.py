"""Platform-specific camera capture implementations.

Automatically imports the correct CameraCapture class based on platform.
All implementations provide the same API:
- __init__(gpu_context, runtime_width, runtime_height, device_id)
- get_texture() -> wgpu.GPUTexture
- stop()
"""

import sys

if sys.platform == 'darwin':
    from .macos import AVFoundationCapture as CameraCapture
elif sys.platform == 'linux':
    from .linux import V4L2Capture as CameraCapture
elif sys.platform == 'win32':
    from .windows import MediaFoundationCapture as CameraCapture
else:
    raise RuntimeError(f"Camera capture not supported on platform: {sys.platform}")

__all__ = ['CameraCapture']
