# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Standalone CGL OpenGL context and IOSurface-to-GL texture binding.

macOS: Creates independent CGL contexts for subprocess processors that need
their own GPU rendering pipeline. Binds IOSurfaces as GL_TEXTURE_RECTANGLE
textures via CGLTexImageIOSurface2D (zero-copy).

Linux/Windows: Not yet implemented.
"""

import sys

if sys.platform == "darwin":
    import ctypes
    import ctypes.util

    # Load OpenGL framework (contains both CGL and GL functions)
    _gl_lib = ctypes.cdll.LoadLibrary(ctypes.util.find_library("OpenGL"))

    # =========================================================================
    # CGL Constants
    # =========================================================================

    kCGLPFAOpenGLProfile = 99
    kCGLOGLPVersion_3_2_Core = 0x3200
    kCGLPFAAccelerated = 73
    kCGLPFAColorSize = 56
    kCGLPFADepthSize = 12
    kCGLPFADoubleBuffer = 5
    kCGLPFAAllowOfflineRenderers = 96
    kCGLPFASupportsAutomaticGraphicsSwitching = 101
    # Terminator for pixel format attribute list
    _kCGLPFATerminator = 0

    # GL Constants
    GL_TEXTURE_RECTANGLE = 0x84F5
    GL_RGBA = 0x1908
    GL_BGRA = 0x80E1
    GL_UNSIGNED_INT_8_8_8_8_REV = 0x8367

    # =========================================================================
    # CGL Function Signatures
    # =========================================================================

    # CGLChoosePixelFormat(attributes, &pix, &npix) -> CGLError
    _gl_lib.CGLChoosePixelFormat.argtypes = [
        ctypes.POINTER(ctypes.c_int32),  # attributes array
        ctypes.POINTER(ctypes.c_void_p),  # pixel format out
        ctypes.POINTER(ctypes.c_int32),  # num formats out
    ]
    _gl_lib.CGLChoosePixelFormat.restype = ctypes.c_int32

    # CGLCreateContext(pix, share, &ctx) -> CGLError
    _gl_lib.CGLCreateContext.argtypes = [
        ctypes.c_void_p,  # pixel format
        ctypes.c_void_p,  # share context (NULL for standalone)
        ctypes.POINTER(ctypes.c_void_p),  # context out
    ]
    _gl_lib.CGLCreateContext.restype = ctypes.c_int32

    # CGLSetCurrentContext(ctx) -> CGLError
    _gl_lib.CGLSetCurrentContext.argtypes = [ctypes.c_void_p]
    _gl_lib.CGLSetCurrentContext.restype = ctypes.c_int32

    # CGLDestroyContext(ctx) -> CGLError
    _gl_lib.CGLDestroyContext.argtypes = [ctypes.c_void_p]
    _gl_lib.CGLDestroyContext.restype = ctypes.c_int32

    # CGLDestroyPixelFormat(pix) -> CGLError
    _gl_lib.CGLDestroyPixelFormat.argtypes = [ctypes.c_void_p]
    _gl_lib.CGLDestroyPixelFormat.restype = ctypes.c_int32

    # CGLGetCurrentContext() -> CGLContextObj
    _gl_lib.CGLGetCurrentContext.argtypes = []
    _gl_lib.CGLGetCurrentContext.restype = ctypes.c_void_p

    # CGLTexImageIOSurface2D(ctx, target, internal_format, width, height,
    #                         format, type, iosurface, plane) -> CGLError
    _gl_lib.CGLTexImageIOSurface2D.argtypes = [
        ctypes.c_void_p,   # CGL context
        ctypes.c_uint32,   # GL target (GL_TEXTURE_RECTANGLE)
        ctypes.c_uint32,   # internal format (GL_RGBA)
        ctypes.c_uint32,   # width
        ctypes.c_uint32,   # height
        ctypes.c_uint32,   # format (GL_BGRA)
        ctypes.c_uint32,   # type (GL_UNSIGNED_INT_8_8_8_8_REV)
        ctypes.c_void_p,   # IOSurfaceRef
        ctypes.c_uint32,   # plane
    ]
    _gl_lib.CGLTexImageIOSurface2D.restype = ctypes.c_int32

    # glFlush()
    _gl_lib.glFlush.argtypes = []
    _gl_lib.glFlush.restype = None

    # =========================================================================
    # Public API
    # =========================================================================

    def create_cgl_context():
        """Create a standalone CGL OpenGL 3.2 Core context.

        Returns an opaque CGL context handle (ctypes.c_void_p).
        """
        # Build pixel format attributes
        attrs = (ctypes.c_int32 * 8)(
            kCGLPFAOpenGLProfile, kCGLOGLPVersion_3_2_Core,
            kCGLPFAAccelerated,
            kCGLPFAColorSize, 32,
            kCGLPFAAllowOfflineRenderers,
            kCGLPFASupportsAutomaticGraphicsSwitching,
            _kCGLPFATerminator,
        )

        pix = ctypes.c_void_p()
        npix = ctypes.c_int32()

        err = _gl_lib.CGLChoosePixelFormat(attrs, ctypes.byref(pix), ctypes.byref(npix))
        if err != 0:
            raise RuntimeError(f"CGLChoosePixelFormat failed with error {err}")

        ctx = ctypes.c_void_p()
        err = _gl_lib.CGLCreateContext(pix, None, ctypes.byref(ctx))

        # Destroy pixel format (context retains what it needs)
        _gl_lib.CGLDestroyPixelFormat(pix)

        if err != 0:
            raise RuntimeError(f"CGLCreateContext failed with error {err}")

        return ctx

    def make_current(cgl_ctx):
        """Set the given CGL context as the current GL context."""
        err = _gl_lib.CGLSetCurrentContext(cgl_ctx)
        if err != 0:
            raise RuntimeError(f"CGLSetCurrentContext failed with error {err}")

    def bind_iosurface_to_texture(cgl_ctx, texture_id, iosurface_ref, width, height):
        """Bind an IOSurface as the backing store for a GL_TEXTURE_RECTANGLE texture.

        This is the zero-copy path: the GPU reads directly from the IOSurface
        memory without any pixel copies.
        """
        from OpenGL.GL import glBindTexture

        glBindTexture(GL_TEXTURE_RECTANGLE, texture_id)

        err = _gl_lib.CGLTexImageIOSurface2D(
            cgl_ctx,
            GL_TEXTURE_RECTANGLE,
            GL_RGBA,
            width,
            height,
            GL_BGRA,
            GL_UNSIGNED_INT_8_8_8_8_REV,
            iosurface_ref,
            0,  # plane 0
        )
        if err != 0:
            raise RuntimeError(f"CGLTexImageIOSurface2D failed with error {err}")

    def flush():
        """Flush all pending GL commands."""
        _gl_lib.glFlush()

    def destroy_cgl_context(cgl_ctx):
        """Destroy a CGL context."""
        if cgl_ctx:
            _gl_lib.CGLDestroyContext(cgl_ctx)

elif sys.platform == "linux":
    raise NotImplementedError(
        "CGL context not available on Linux. "
        "Requires EGL context creation + EGL_EXT_image_dma_buf_import for zero-copy. "
        "See: eglCreateContext, eglCreateImageKHR with EGL_LINUX_DMA_BUF_EXT"
    )

elif sys.platform == "win32":
    raise NotImplementedError(
        "CGL context not available on Windows. "
        "Requires WGL context creation + DirectX/OpenGL interop. "
        "See: wglCreateContext, WGL_NV_DX_interop2"
    )

else:
    raise NotImplementedError(
        f"CGL context not supported on platform '{sys.platform}'. "
        "Supported: macOS (CGL). Planned: Linux (EGL), Windows (WGL)."
    )
