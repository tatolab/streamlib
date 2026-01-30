# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Platform-specific zero-copy GPU surface access.

macOS: IOSurface shared memory via ctypes (kernel-managed, cross-process).
Linux: Not yet implemented (needs Vulkan DMA-BUF / EGL external memory).
Windows: Not yet implemented (needs DirectX 12 shared textures / NT handles).
"""

import sys

if sys.platform == "darwin":
    # macOS — IOSurface implementation
    import ctypes
    import ctypes.util
    import numpy as np

    _iosurface_lib = ctypes.cdll.LoadLibrary(ctypes.util.find_library("IOSurface"))
    _cf_lib = ctypes.cdll.LoadLibrary(ctypes.util.find_library("CoreFoundation"))

    _iosurface_lib.IOSurfaceLookup.argtypes = [ctypes.c_uint32]
    _iosurface_lib.IOSurfaceLookup.restype = ctypes.c_void_p

    _iosurface_lib.IOSurfaceLock.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p]
    _iosurface_lib.IOSurfaceLock.restype = ctypes.c_int32

    _iosurface_lib.IOSurfaceUnlock.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p]
    _iosurface_lib.IOSurfaceUnlock.restype = ctypes.c_int32

    _iosurface_lib.IOSurfaceGetBaseAddress.argtypes = [ctypes.c_void_p]
    _iosurface_lib.IOSurfaceGetBaseAddress.restype = ctypes.c_void_p

    _iosurface_lib.IOSurfaceGetWidth.argtypes = [ctypes.c_void_p]
    _iosurface_lib.IOSurfaceGetWidth.restype = ctypes.c_size_t

    _iosurface_lib.IOSurfaceGetHeight.argtypes = [ctypes.c_void_p]
    _iosurface_lib.IOSurfaceGetHeight.restype = ctypes.c_size_t

    _iosurface_lib.IOSurfaceGetBytesPerRow.argtypes = [ctypes.c_void_p]
    _iosurface_lib.IOSurfaceGetBytesPerRow.restype = ctypes.c_size_t

    _cf_lib.CFRelease.argtypes = [ctypes.c_void_p]
    _cf_lib.CFRelease.restype = None

    kIOSurfaceLockReadOnly = 1

    class GpuSurfaceHandle:
        """RAII wrapper for an IOSurface looked up by global ID."""

        def __init__(self, iosurface_id):
            self._ref = _iosurface_lib.IOSurfaceLookup(iosurface_id)
            if not self._ref:
                raise RuntimeError(f"IOSurfaceLookup failed for id={iosurface_id}")
            self.width = _iosurface_lib.IOSurfaceGetWidth(self._ref)
            self.height = _iosurface_lib.IOSurfaceGetHeight(self._ref)
            self.bytes_per_row = _iosurface_lib.IOSurfaceGetBytesPerRow(self._ref)

        def lock(self, read_only=True):
            """Lock the surface for CPU access."""
            flags = kIOSurfaceLockReadOnly if read_only else 0
            _iosurface_lib.IOSurfaceLock(self._ref, flags, None)

        def unlock(self, read_only=True):
            """Unlock the surface."""
            flags = kIOSurfaceLockReadOnly if read_only else 0
            _iosurface_lib.IOSurfaceUnlock(self._ref, flags, None)

        def as_numpy(self):
            """Create a numpy array VIEW into surface memory. Surface must be locked."""
            base = _iosurface_lib.IOSurfaceGetBaseAddress(self._ref)
            buf = (ctypes.c_uint8 * (self.bytes_per_row * self.height)).from_address(base)
            return np.ndarray(
                shape=(self.height, self.width, 4),
                dtype=np.uint8,
                buffer=buf,
                strides=(self.bytes_per_row, 4, 1),
            )

        @property
        def iosurface_ref(self):
            """Raw IOSurfaceRef pointer for CGL texture binding."""
            return self._ref

        def release(self):
            """Release the surface reference."""
            if self._ref:
                _cf_lib.CFRelease(self._ref)
                self._ref = None

        def __del__(self):
            self.release()

elif sys.platform == "linux":
    raise NotImplementedError(
        "GPU surface access not yet implemented on Linux. "
        "Requires Vulkan DMA-BUF / EGL external memory integration. "
        "See: VK_EXT_external_memory_dma_buf, VK_KHR_external_memory_fd"
    )

elif sys.platform == "win32":
    raise NotImplementedError(
        "GPU surface access not yet implemented on Windows. "
        "Requires DirectX 12 shared textures / NT handle sharing. "
        "See: ID3D12Device::CreateSharedHandle, IDXGIResource1::CreateSharedHandle"
    )

else:
    raise NotImplementedError(
        f"GPU surface access not supported on platform '{sys.platform}'. "
        "Supported: macOS (IOSurface). Planned: Linux (Vulkan DMA-BUF), Windows (DirectX)."
    )
