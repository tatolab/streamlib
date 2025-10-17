"""Platform-specific audio capture implementations.

Automatically imports the correct AudioCapture class based on platform.
All implementations provide the same API:
- __init__(gpu_context, sample_rate, chunk_size, device_name, process_callback)
- start() -> None
- stop() -> None
- chunks_captured -> int
- chunks_dropped -> int
"""

import sys

if sys.platform == 'darwin':
    from .macos import CoreAudioCapture as AudioCapture
elif sys.platform == 'linux':
    from .linux import ALSACapture as AudioCapture
elif sys.platform == 'win32':
    from .windows import WASAPICapture as AudioCapture
else:
    raise RuntimeError(f"Audio capture not supported on platform: {sys.platform}")

__all__ = ['AudioCapture']
