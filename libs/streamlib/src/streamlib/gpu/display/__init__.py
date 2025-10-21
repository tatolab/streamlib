"""
GPU-accelerated display output with platform-specific implementations.

Provides zero-copy rendering to native windows using platform-specific APIs:
- macOS: CAMetalLayer with Metal
- Linux: Vulkan/X11 (future)
- Windows: DirectX/Win32 (future)
"""

import sys

if sys.platform == 'darwin':
    from .macos import DisplayWindow
elif sys.platform == 'linux':
    from .linux import DisplayWindow
elif sys.platform == 'win32':
    from .windows import DisplayWindow
else:
    raise ImportError(f"Display not supported on platform: {sys.platform}")

__all__ = ['DisplayWindow']
