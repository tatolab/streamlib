# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot OpenGL fragment-shader processor — Python.

End-to-end gate for the subprocess `OpenGLContext` runtime (#530). The
host pre-allocates a render-target-capable DMA-BUF surface and registers
it with surface-share. This processor receives a trigger Videoframe,
opens the host surface through ``OpenGLContext.acquire_write`` (which
imports the DMA-BUF as an `EGLImage` + `GL_TEXTURE_2D` and makes the
adapter's EGL context current on the calling thread), uses raw ctypes
against ``libGL.so.1`` to compile a Mandelbrot fragment shader, attaches
an FBO to the imported texture, draws a fullscreen quad, and releases —
the adapter's `glFinish` on release ensures the host's DMA-BUF readback
sees the writes.

No PyOpenGL / ModernGL dependency: ``ctypes`` is sufficient for the
~12 GL functions this needs, parallel to what the Deno twin does via
``Deno.dlopen``. Real customers can use whatever GL library they prefer
— the SDK is library-agnostic; it just makes the EGL context current
and hands back a `GL_TEXTURE_2D` id.

Config keys:
    opengl_surface_uuid (str, required)
        Surface-share UUID the host registered the render-target image
        under. Passed to ``OpenGLContext.acquire_write``.
    width (int, required)
        Surface width in pixels — the FBO viewport is set to this.
    height (int, required)
        Surface height in pixels.
"""

from __future__ import annotations

import ctypes
import ctypes.util
from typing import Optional

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.opengl import OpenGLContext


# =============================================================================
# Minimal libGL.so.1 binding via ctypes
# =============================================================================
#
# Only the ~12 functions this scenario needs. Real customers using a
# library like PyOpenGL get a full surface for free.
#
# We resolve via libGL.so.1 directly — the function pointers are valid
# in whatever EGL context is current on the calling thread, which is the
# adapter's context for the lifetime of an `acquire_write` scope.

GL_FRAGMENT_SHADER = 0x8B30
GL_VERTEX_SHADER = 0x8B31
GL_COMPILE_STATUS = 0x8B81
GL_LINK_STATUS = 0x8B82
GL_INFO_LOG_LENGTH = 0x8B84
GL_FRAMEBUFFER = 0x8D40
GL_COLOR_ATTACHMENT0 = 0x8CE0
GL_FRAMEBUFFER_COMPLETE = 0x8CD5
GL_TEXTURE_2D = 0x0DE1
GL_TRIANGLE_STRIP = 0x0005
GL_FLOAT = 0x1406
GL_NO_ERROR = 0


def _load_libgl():
    """dlopen libGL.so.1 with all the symbol signatures this processor needs."""
    name = ctypes.util.find_library("GL") or "libGL.so.1"
    lib = ctypes.cdll.LoadLibrary(name)

    def sig(fn_name, restype, *argtypes):
        fn = getattr(lib, fn_name)
        fn.restype = restype
        fn.argtypes = list(argtypes)
        return fn

    return {
        "CreateShader": sig("glCreateShader", ctypes.c_uint32, ctypes.c_uint32),
        "ShaderSource": sig(
            "glShaderSource",
            None,
            ctypes.c_uint32,
            ctypes.c_int32,
            ctypes.POINTER(ctypes.c_char_p),
            ctypes.POINTER(ctypes.c_int32),
        ),
        "CompileShader": sig("glCompileShader", None, ctypes.c_uint32),
        "GetShaderiv": sig(
            "glGetShaderiv",
            None,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_int32),
        ),
        "GetShaderInfoLog": sig(
            "glGetShaderInfoLog",
            None,
            ctypes.c_uint32,
            ctypes.c_int32,
            ctypes.POINTER(ctypes.c_int32),
            ctypes.c_char_p,
        ),
        "DeleteShader": sig("glDeleteShader", None, ctypes.c_uint32),
        "CreateProgram": sig("glCreateProgram", ctypes.c_uint32),
        "AttachShader": sig("glAttachShader", None, ctypes.c_uint32, ctypes.c_uint32),
        "LinkProgram": sig("glLinkProgram", None, ctypes.c_uint32),
        "GetProgramiv": sig(
            "glGetProgramiv",
            None,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_int32),
        ),
        "GetProgramInfoLog": sig(
            "glGetProgramInfoLog",
            None,
            ctypes.c_uint32,
            ctypes.c_int32,
            ctypes.POINTER(ctypes.c_int32),
            ctypes.c_char_p,
        ),
        "DeleteProgram": sig("glDeleteProgram", None, ctypes.c_uint32),
        "UseProgram": sig("glUseProgram", None, ctypes.c_uint32),
        "GetUniformLocation": sig(
            "glGetUniformLocation", ctypes.c_int32, ctypes.c_uint32, ctypes.c_char_p
        ),
        "Uniform2f": sig(
            "glUniform2f", None, ctypes.c_int32, ctypes.c_float, ctypes.c_float
        ),
        "GenFramebuffers": sig(
            "glGenFramebuffers", None, ctypes.c_int32, ctypes.POINTER(ctypes.c_uint32)
        ),
        "DeleteFramebuffers": sig(
            "glDeleteFramebuffers",
            None,
            ctypes.c_int32,
            ctypes.POINTER(ctypes.c_uint32),
        ),
        "BindFramebuffer": sig(
            "glBindFramebuffer", None, ctypes.c_uint32, ctypes.c_uint32
        ),
        "FramebufferTexture2D": sig(
            "glFramebufferTexture2D",
            None,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_int32,
        ),
        "CheckFramebufferStatus": sig(
            "glCheckFramebufferStatus", ctypes.c_uint32, ctypes.c_uint32
        ),
        "GenVertexArrays": sig(
            "glGenVertexArrays", None, ctypes.c_int32, ctypes.POINTER(ctypes.c_uint32)
        ),
        "DeleteVertexArrays": sig(
            "glDeleteVertexArrays",
            None,
            ctypes.c_int32,
            ctypes.POINTER(ctypes.c_uint32),
        ),
        "BindVertexArray": sig("glBindVertexArray", None, ctypes.c_uint32),
        "Viewport": sig(
            "glViewport",
            None,
            ctypes.c_int32,
            ctypes.c_int32,
            ctypes.c_int32,
            ctypes.c_int32,
        ),
        "DrawArrays": sig(
            "glDrawArrays", None, ctypes.c_uint32, ctypes.c_int32, ctypes.c_int32
        ),
        "Finish": sig("glFinish", None),
        "GetError": sig("glGetError", ctypes.c_uint32),
    }


# =============================================================================
# Shaders — Mandelbrot zoom into Seahorse Valley
# =============================================================================

_VERTEX_SHADER = b"""\
#version 330 core
const vec2 positions[4] = vec2[4](
    vec2(-1.0, -1.0), vec2( 1.0, -1.0),
    vec2(-1.0,  1.0), vec2( 1.0,  1.0)
);
void main() {
    gl_Position = vec4(positions[gl_VertexID], 0.0, 1.0);
}
"""

_FRAGMENT_SHADER = b"""\
#version 330 core
out vec4 fragColor;
uniform vec2 resolution;
void main() {
    vec2 uv = gl_FragCoord.xy / resolution;
    // Seahorse Valley region - narrow zoom shows the recursive structure.
    vec2 c = vec2(-0.7453, 0.1127) + (uv - 0.5) * 0.018;
    vec2 z = vec2(0.0);
    int last_iter = 0;
    const int max_iter = 256;
    bool escaped = false;
    for (int i = 0; i < max_iter; i++) {
        if (dot(z, z) > 4.0) {
            last_iter = i;
            escaped = true;
            break;
        }
        z = vec2(z.x*z.x - z.y*z.y, 2.0*z.x*z.y) + c;
    }
    if (!escaped) {
        fragColor = vec4(0.0, 0.0, 0.0, 1.0);
    } else {
        // Smooth coloring + cosine palette (Inigo Quilez style).
        float smoothed = float(last_iter) - log2(log2(dot(z, z))) + 4.0;
        float t = smoothed / float(max_iter);
        vec3 a = vec3(0.5);
        vec3 b = vec3(0.5);
        vec3 c2 = vec3(1.0);
        vec3 d = vec3(0.0, 0.33, 0.67);
        vec3 col = a + b * cos(6.28318 * (c2 * t + d));
        fragColor = vec4(col, 1.0);
    }
}
"""


def _compile_shader(gl, source: bytes, kind: int, kind_name: str) -> int:
    """Compile a shader and raise with the GL info log if it fails."""
    sh = gl["CreateShader"](kind)
    if sh == 0:
        raise RuntimeError(f"glCreateShader({kind_name}) returned 0")
    src_buf = ctypes.c_char_p(source)
    arr = (ctypes.c_char_p * 1)(src_buf)
    gl["ShaderSource"](sh, 1, arr, None)
    gl["CompileShader"](sh)
    status = ctypes.c_int32(0)
    gl["GetShaderiv"](sh, GL_COMPILE_STATUS, ctypes.byref(status))
    if status.value == 0:
        log_len = ctypes.c_int32(0)
        gl["GetShaderiv"](sh, GL_INFO_LOG_LENGTH, ctypes.byref(log_len))
        log_buf = ctypes.create_string_buffer(log_len.value or 1024)
        actual = ctypes.c_int32(0)
        gl["GetShaderInfoLog"](sh, log_len.value or 1024, ctypes.byref(actual), log_buf)
        gl["DeleteShader"](sh)
        raise RuntimeError(
            f"{kind_name} shader compile failed: {log_buf.value.decode(errors='replace')}"
        )
    return sh


def _link_program(gl, vs: int, fs: int) -> int:
    prog = gl["CreateProgram"]()
    if prog == 0:
        raise RuntimeError("glCreateProgram returned 0")
    gl["AttachShader"](prog, vs)
    gl["AttachShader"](prog, fs)
    gl["LinkProgram"](prog)
    status = ctypes.c_int32(0)
    gl["GetProgramiv"](prog, GL_LINK_STATUS, ctypes.byref(status))
    if status.value == 0:
        log_len = ctypes.c_int32(0)
        gl["GetProgramiv"](prog, GL_INFO_LOG_LENGTH, ctypes.byref(log_len))
        log_buf = ctypes.create_string_buffer(log_len.value or 1024)
        actual = ctypes.c_int32(0)
        gl["GetProgramInfoLog"](prog, log_len.value or 1024, ctypes.byref(actual), log_buf)
        gl["DeleteProgram"](prog)
        raise RuntimeError(
            f"program link failed: {log_buf.value.decode(errors='replace')}"
        )
    return prog


class OpenGlFragmentShaderProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._uuid = str(cfg["opengl_surface_uuid"])
        self._width = int(cfg["width"])
        self._height = int(cfg["height"])
        self._opengl = OpenGLContext.from_runtime(ctx)
        self._gl: Optional[dict] = None
        self._rendered = False
        self._error: Optional[str] = None
        print(
            f"[OpenGlFragmentShader/py] setup uuid={self._uuid} "
            f"size={self._width}x{self._height}",
            flush=True,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return
        if self._rendered:
            return  # Render once; subsequent frames are no-ops.
        try:
            self._render_once()
            self._rendered = True
            print(
                f"[OpenGlFragmentShader/py] Mandelbrot rendered into surface '{self._uuid}'",
                flush=True,
            )
        except Exception as e:
            self._error = str(e)
            print(
                f"[OpenGlFragmentShader/py] render failed: {e}", flush=True,
            )

    def _render_once(self) -> None:
        # Acquire the surface — adapter makes the EGL context current on
        # this thread for the lifetime of the with-block.
        with self._opengl.acquire_write(self._uuid) as view:
            texture_id = view.gl_texture_id
            # Lazy-load libGL — defer until the EGL context is current
            # so symbol resolution sees a real GL state machine.
            if self._gl is None:
                self._gl = _load_libgl()
            gl = self._gl

            # Empty VAO — desktop core requires one bound; we generate
            # geometry from gl_VertexID in the vertex shader.
            vao = ctypes.c_uint32(0)
            gl["GenVertexArrays"](1, ctypes.byref(vao))
            gl["BindVertexArray"](vao.value)

            vs = _compile_shader(gl, _VERTEX_SHADER, GL_VERTEX_SHADER, "vertex")
            fs = _compile_shader(
                gl, _FRAGMENT_SHADER, GL_FRAGMENT_SHADER, "fragment"
            )
            try:
                program = _link_program(gl, vs, fs)
            finally:
                gl["DeleteShader"](vs)
                gl["DeleteShader"](fs)

            try:
                # FBO with the imported texture as color attachment.
                fbo = ctypes.c_uint32(0)
                gl["GenFramebuffers"](1, ctypes.byref(fbo))
                gl["BindFramebuffer"](GL_FRAMEBUFFER, fbo.value)
                gl["FramebufferTexture2D"](
                    GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D,
                    texture_id, 0,
                )
                status = gl["CheckFramebufferStatus"](GL_FRAMEBUFFER)
                if status != GL_FRAMEBUFFER_COMPLETE:
                    raise RuntimeError(
                        f"FBO incomplete (status=0x{status:x}) — the imported "
                        "texture may have been bound with an external_only "
                        "modifier; the host allocator should pick a tiled, "
                        "render-target-capable DRM modifier"
                    )

                gl["Viewport"](0, 0, self._width, self._height)
                gl["UseProgram"](program)

                resolution_loc = gl["GetUniformLocation"](program, b"resolution")
                if resolution_loc >= 0:
                    gl["Uniform2f"](
                        resolution_loc, float(self._width), float(self._height)
                    )

                gl["DrawArrays"](GL_TRIANGLE_STRIP, 0, 4)
                gl["Finish"]()

                gl_err = gl["GetError"]()
                if gl_err != GL_NO_ERROR:
                    raise RuntimeError(
                        f"GL error 0x{gl_err:x} after draw — see "
                        "docs/learnings/nvidia-egl-dmabuf-render-target.md"
                    )

                gl["BindFramebuffer"](GL_FRAMEBUFFER, 0)
                gl["DeleteFramebuffers"](1, ctypes.byref(fbo))
            finally:
                gl["DeleteProgram"](program)
                gl["BindVertexArray"](0)
                gl["DeleteVertexArrays"](1, ctypes.byref(vao))
        # acquire_write's __exit__ runs adapter `end_write_access`
        # which does a final glFinish so the host's DMA-BUF readback
        # sees a fully-flushed image.

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print(
            f"[OpenGlFragmentShader/py] teardown rendered={self._rendered} "
            f"error={self._error}",
            flush=True,
        )
