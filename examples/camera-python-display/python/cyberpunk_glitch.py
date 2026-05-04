# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk glitch post-processing — Linux GLSL fragment shader (#486).

Linux subprocess processor on the canonical surface-adapter pattern
(post-#485): reads its ``video_in`` surface (the BlendingCompositor's
output, a tiled DMA-BUF ``VkImage``) via
:meth:`streamlib.adapters.opengl.OpenGLContext.acquire_read`, runs a
GLSL fragment shader that produces chromatic aberration / scanlines /
slice displacement / film-grain glitches, and writes into the host's
pre-registered ``video_out`` surface via
:meth:`OpenGLContext.acquire_write`.

ModernGL adopts the adapter's EGL context (``standalone=False``) for
shader compilation, FBO construction, and the fullscreen draw; the
input texture is bound on unit 0 by raw GL via ``ctypes`` —
``moderngl.Context.external_texture`` would work but would churn a
fresh Python wrapper per frame as the upstream ring slot rotates.
This matches ``pose_overlay_renderer.py``'s pattern, just on
``GL_TEXTURE_2D`` instead of ``GL_TEXTURE_EXTERNAL_OES`` (the
BlendingCompositor's output is allocated render-target-capable with a
tiled DRM modifier — see
``docs/learnings/nvidia-egl-dmabuf-render-target.md``).

The intermittent dramatic-mode triggering stays Python-side via
:class:`GlitchState`. The shader receives ``intensity`` and
``isDramatic`` uniforms each frame; ``intensity < 0.01`` short-
circuits to a passthrough sample of the input.

macOS support was removed in #485; the pre-RHI CGL+IOSurface path
predated the surface-adapter pattern.

Config keys (set by ``examples/camera-python-display/src/linux.rs``):
    output_surface_uuid (str) — pre-registered render-target DMA-BUF VkImage.
    width, height (int) — surface dimensions.
"""

import ctypes
import logging
import random
import time

import moderngl
import numpy as np

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.opengl import GL_TEXTURE_2D, OpenGLContext

logger = logging.getLogger(__name__)


# Raw GL entry points for binding the upstream input texture on unit 0.
# ModernGL's `Texture.use` works only for textures it owns; rebinding an
# externally-supplied `GL_TEXTURE_2D` id requires raw GL. Both functions
# are core GL entry points always exported by libGL — resolving via
# ctypes avoids dragging PyOpenGL in as a Linux dep, and matches
# `pose_overlay_renderer.py`'s pattern (the OS GL loader is what
# ModernGL uses internally).
_GL_LIB = ctypes.CDLL("libGL.so.1")
_GL_LIB.glActiveTexture.argtypes = [ctypes.c_uint]
_GL_LIB.glActiveTexture.restype = None
_GL_LIB.glBindTexture.argtypes = [ctypes.c_uint, ctypes.c_uint]
_GL_LIB.glBindTexture.restype = None
_GL_TEXTURE0 = 0x84C0


# =============================================================================
# Shaders
# =============================================================================

# Fullscreen-quad vertex shader. NDC y aligns with uv.y so a write at
# NDC y=-1 lands in row 0 of the FBO (GL bottom = Vulkan top under the
# DMA-BUF aliasing) sampling uv.y=0 of the input. Both producer and
# consumer use the same convention so the GL→Vulkan double flip cancels
# and downstream sees the image upright. See the bg_verts comment in
# `pose_overlay_renderer.py` for the full rationale; do NOT invert
# texcoord Y to "fix" orientation — that re-introduces #621.
_VERTEX_SHADER = """
#version 330 core
in vec2 in_position;
out vec2 v_uv;
void main() {
    gl_Position = vec4(in_position, 0.0, 1.0);
    v_uv = in_position * 0.5 + 0.5;
}
"""

# Fragment shader translated from the macOS sampler2DRect path. Pixel-
# space offsets become normalized-UV offsets via the precomputed
# `1.0 / resolution`. Same chromatic-aberration / scanlines / slice /
# film-grain modes; same intermittent dramatic gate.
_GLITCH_FRAGMENT_SHADER = """
#version 330 core
in vec2 v_uv;
out vec4 frag_color;

uniform sampler2D u_input;
uniform vec2 u_resolution;
uniform float u_time;
uniform float u_intensity;
uniform float u_seed;
uniform float u_is_dramatic;

float hash11(float p) {
    p = fract(p * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return fract(p);
}

float hash21(vec2 p) {
    p = fract(p * vec2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

void main() {
    vec2 uv = v_uv;
    vec2 inv_res = 1.0 / u_resolution;

    if (u_intensity < 0.01) {
        frag_color = texture(u_input, uv);
        return;
    }

    vec3 color;

    if (u_is_dramatic > 0.5) {
        // Horizontal slice displacement + multi-octave film grain.
        float slice_height_px = mix(15.0, 30.0, hash11(u_seed * 0.5));
        float slice_index = floor(uv.y * u_resolution.y / slice_height_px);
        float slice_random = hash11(slice_index + u_seed);
        float max_offset_px = 200.0 * u_intensity;
        float x_offset_px = (slice_random - 0.5) * 2.0 * max_offset_px;
        vec2 displaced_uv = uv + vec2(x_offset_px * inv_res.x, 0.0);
        color = texture(u_input, displaced_uv).rgb;

        float grain = 0.0;
        float scale = 1.0;
        float amp = 0.5;
        for (int i = 0; i < 3; i++) {
            vec2 grain_uv = uv * u_resolution * scale * 0.01 + u_seed;
            grain += (hash21(grain_uv) - 0.5) * amp;
            scale *= 2.0;
            amp *= 0.5;
        }
        grain += (hash21(uv * 800.0 + u_time * 10.0) - 0.5) * 0.15;
        color += grain * 0.12 * u_intensity;
        color = clamp(color, 0.0, 1.0);
    } else {
        // Subtle: chromatic aberration + scanlines + sparse slice + cyan lines.
        float aberration_px = 8.0 * u_intensity;
        vec2 r_offset = vec2(
            aberration_px * (hash11(u_seed * 1.1) - 0.5) * 2.0 * inv_res.x, 0.0
        );
        vec2 b_offset = vec2(
            aberration_px * (hash11(u_seed * 2.2) - 0.5) * 2.0 * inv_res.x, 0.0
        );
        float r = texture(u_input, uv + r_offset).r;
        float g = texture(u_input, uv).g;
        float b = texture(u_input, uv + b_offset).b;
        color = vec3(r, g, b);

        // Scanlines indexed in pixel space so spacing stays constant at any res.
        float scanline = sin(uv.y * u_resolution.y * 3.14159 * 2.0) * 0.5 + 0.5;
        scanline = pow(scanline, 0.5);
        color *= 0.85 + 0.15 * scanline;

        float slice_noise = hash11(floor(uv.y * 60.0) + u_seed);
        if (slice_noise > 0.75 && u_intensity > 0.3) {
            float slice_strength = (slice_noise - 0.75) / 0.25;
            float slice_offset_px =
                (hash11(u_seed + floor(uv.y * 60.0) * 0.1) - 0.5)
                * 60.0 * u_intensity * slice_strength;
            vec2 slice_uv = uv + vec2(slice_offset_px * inv_res.x, 0.0);
            color = texture(u_input, slice_uv).rgb;
        }

        float line_noise = hash11(u_time * 50.0 + floor(uv.y * u_resolution.y));
        if (line_noise > 0.97) {
            color += vec3(0.0, 0.3 * u_intensity, 0.3 * u_intensity);
        }
    }

    frag_color = vec4(color, 1.0);
}
"""


# =============================================================================
# Glitch State Tracking
# =============================================================================

class GlitchState:
    """Single timer firing every 0–8 s after a 2 s cooldown.

    On each fire: 50/50 between a dramatic glitch (0.3–0.8 s, intensity
    0.8–1.0) and a minor glitch (0.1–0.3 s, intensity 0.3–0.6).
    """

    COOLDOWN = 2.0  # seconds

    def __init__(self):
        self.active = False
        self.is_dramatic = False
        self.intensity = 0.0
        self.start_time = 0.0
        self.duration = 0.0
        self.seed = 0.0
        self.in_cooldown = False
        self.cooldown_end_time = 0.0
        self.next_glitch = random.uniform(0.0, 8.0)

    def update(self, elapsed: float) -> bool:
        """Step the state machine. Returns True iff a glitch is active."""
        if self.active:
            if elapsed - self.start_time > self.duration:
                self.active = False
                self.is_dramatic = False
                self.intensity = 0.0
                self.in_cooldown = True
                self.cooldown_end_time = elapsed + self.COOLDOWN
            return self.active

        if self.in_cooldown:
            if elapsed >= self.cooldown_end_time:
                self.in_cooldown = False
                self.next_glitch = elapsed + random.uniform(0.0, 8.0)
            return False

        if elapsed >= self.next_glitch:
            self.active = True
            self.start_time = elapsed
            self.seed = elapsed
            if random.random() < 0.5:
                self.is_dramatic = True
                self.duration = random.uniform(0.3, 0.8)
                self.intensity = random.uniform(0.8, 1.0)
            else:
                self.is_dramatic = False
                self.duration = random.uniform(0.1, 0.3)
                self.intensity = random.uniform(0.3, 0.6)
            return True

        return False


# =============================================================================
# Cyberpunk Glitch Processor (Linux)
# =============================================================================

class CyberpunkGlitch:
    """GLSL fragment-shader glitch post-processor (Linux, #486).

    Reads ``video_in`` via :meth:`OpenGLContext.acquire_read`
    (`GL_TEXTURE_2D`), applies the glitch shader, writes into the host-
    pre-registered output surface via :meth:`OpenGLContext.acquire_write`,
    and emits a frame referencing that UUID downstream.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._uuid = str(cfg["output_surface_uuid"])
        self._W = int(cfg["width"])
        self._H = int(cfg["height"])

        self.frame_count = 0
        self._start_time = time.monotonic()
        self.glitch_state = GlitchState()

        # No application-level frame limiter — `video_in` is a
        # SkipToLatest mailbox, so the upstream's pace governs ours.
        # `BlendingCompositor` is paced at the display's 60 Hz today;
        # if a future upstream produces faster, we'd want to consume
        # at that rate and let the display do its own SkipToLatest
        # rather than gating here. AvatarCharacter (the precedent
        # consumer of an `acquire_read` upstream) follows the same
        # convention.

        self._opengl = OpenGLContext.from_runtime(ctx)

        # ModernGL state — initialized lazily on first acquire_write.
        # The adapter's EGL context is current on this thread inside an
        # `acquire_*` scope; `moderngl.create_context(standalone=False)`
        # adopts that context.
        self._mgl_ctx = None
        self._program = None
        self._vbo = None
        self._vao = None
        self._mgl_external_color = None
        self._mgl_output_fbo = None
        self._cached_output_gl_id = None

        logger.info(
            f"Cyberpunk Glitch initialized "
            f"({self._W}x{self._H}, uuid={self._uuid})"
        )

    def _ensure_render_state(self, output_gl_texture_id: int) -> None:
        """Lazy-init ModernGL ctx + shader program + VAO + output FBO.

        Must be called from inside an :meth:`OpenGLContext.acquire_write`
        scope. Idempotent on repeat calls; rebuilds the FBO if the
        adapter ever returns a different output GL id (defensive — in
        practice the adapter's id is stable per-UUID across acquires).
        """
        if self._mgl_ctx is None:
            self._mgl_ctx = moderngl.create_context(standalone=False)
            logger.info(
                f"Cyberpunk Glitch: ModernGL context created "
                f"(GL {self._mgl_ctx.version_code})"
            )
            self._program = self._mgl_ctx.program(
                vertex_shader=_VERTEX_SHADER,
                fragment_shader=_GLITCH_FRAGMENT_SHADER,
            )
            verts = np.array([
                -1.0, -1.0,
                 1.0, -1.0,
                -1.0,  1.0,
                 1.0,  1.0,
            ], dtype=np.float32)
            self._vbo = self._mgl_ctx.buffer(verts.tobytes())
            self._vao = self._mgl_ctx.vertex_array(
                self._program,
                [(self._vbo, "2f", "in_position")],
            )

        if (self._mgl_output_fbo is None
                or self._cached_output_gl_id != output_gl_texture_id):
            if self._mgl_output_fbo is not None:
                try:
                    self._mgl_output_fbo.release()
                except Exception:
                    pass
                try:
                    if self._mgl_external_color is not None:
                        self._mgl_external_color.release()
                except Exception:
                    pass
                self._mgl_output_fbo = None
                self._mgl_external_color = None
            # `external_texture(glo, size, components, samples, dtype)` —
            # ModernGL 5.x requires all five positional args. The OpenGL
            # adapter allocates the imported texture as 8-bit RGBA,
            # single-sample. The wrapper does NOT own the underlying GL
            # name — releasing it only frees ModernGL bookkeeping.
            self._mgl_external_color = self._mgl_ctx.external_texture(
                output_gl_texture_id, (self._W, self._H), 4, 0, "f1",
            )
            self._mgl_output_fbo = self._mgl_ctx.framebuffer(
                color_attachments=[self._mgl_external_color],
            )
            self._cached_output_gl_id = output_gl_texture_id
            logger.info(
                f"Cyberpunk Glitch: external FBO bound "
                f"(gl_texture_id={output_gl_texture_id})"
            )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return

        upstream_id = frame.get("surface_id")
        if upstream_id is None:
            return

        elapsed = time.monotonic() - self._start_time
        glitch_active = self.glitch_state.update(elapsed)

        # Acquire ordering: write outer, read inner — matches
        # `avatar_character._process_linux`. The OpenGL adapter
        # serializes `acquire_*` calls behind a single make-current
        # mutex on the EGL context, so nesting is safe.
        try:
            with self._opengl.acquire_write(self._uuid) as out_view:
                self._ensure_render_state(out_view.gl_texture_id)
                with self._opengl.acquire_read(upstream_id) as in_view:
                    self._mgl_output_fbo.use()
                    self._mgl_output_fbo.viewport = (0, 0, self._W, self._H)
                    # No clear — the fragment shader covers every pixel
                    # of the fullscreen quad and ignores the prior FBO
                    # contents.

                    # Bind upstream `GL_TEXTURE_2D` on unit 0 by raw GL.
                    _GL_LIB.glActiveTexture(_GL_TEXTURE0)
                    _GL_LIB.glBindTexture(GL_TEXTURE_2D, in_view.gl_texture_id)

                    self._program["u_input"].value = 0
                    self._program["u_resolution"].value = (
                        float(self._W), float(self._H),
                    )
                    self._program["u_time"].value = float(elapsed)
                    self._program["u_intensity"].value = (
                        float(self.glitch_state.intensity)
                        if glitch_active else 0.0
                    )
                    self._program["u_seed"].value = (
                        float(self.glitch_state.seed)
                        if glitch_active else 0.0
                    )
                    self._program["u_is_dramatic"].value = (
                        1.0 if (glitch_active and self.glitch_state.is_dramatic)
                        else 0.0
                    )

                    self._mgl_ctx.disable(moderngl.DEPTH_TEST)
                    self._mgl_ctx.disable(moderngl.CULL_FACE)
                    self._vao.render(moderngl.TRIANGLE_STRIP)

                    _GL_LIB.glBindTexture(GL_TEXTURE_2D, 0)
        except Exception as e:
            if self.frame_count <= 5 or self.frame_count % 60 == 0:
                logger.warning(
                    f"Cyberpunk Glitch: opengl acquire / render failed "
                    f"(frame={self.frame_count} upstream_id={upstream_id}): {e}"
                )
            return

        out_frame = dict(frame)
        out_frame["surface_id"] = self._uuid
        out_frame["width"] = self._W
        out_frame["height"] = self._H
        ctx.outputs.write("video_out", out_frame)

        self.frame_count += 1

        if self.frame_count == 1:
            logger.info(
                f"Cyberpunk Glitch: First frame processed "
                f"({self._W}x{self._H})"
            )
        elif self.frame_count % 300 == 0:
            logger.info(
                f"Cyberpunk Glitch: {self.frame_count} frames "
                f"(active={glitch_active})"
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        # Best-effort release; the EGL context may already be torn down
        # by the time teardown runs.
        for obj_name in (
            "_mgl_output_fbo", "_mgl_external_color",
            "_vao", "_vbo", "_program",
        ):
            obj = getattr(self, obj_name, None)
            if obj is not None:
                try:
                    obj.release()
                except Exception:
                    pass
            setattr(self, obj_name, None)
        logger.info(
            f"Cyberpunk Glitch: Shutdown ({self.frame_count} frames)"
        )
