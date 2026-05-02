# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cyberpunk pose-overlay renderer (#484 / #615, Linux).

A lightweight TikTok-filter-style renderer that:

- Samples the camera DMA-BUF directly via `samplerExternalOES` (zero
  CPU bounce — the camera frame is bound by the caller as a
  `GL_TEXTURE_EXTERNAL_OES` immediately before `render()`) and draws
  it as a full-screen cyberpunk-tinted background.
- Draws a neon-cyan skeleton connecting YOLOv8 COCO-17 keypoints, with
  glowing magenta circles at each visible joint.
- Applies a soft chromatic-aberration / scanline pass for the
  "screen-with-RGB-shift" look.

No skinned-mesh / GLB / Mixamo dependency — the avatar IS the user, the
overlay is the cyberpunk effect on top. Runs entirely inside the
`OpenGLContext.acquire_write` scope: ModernGL adopts the adapter's EGL
context (`standalone=False`), the imported `GL_TEXTURE_2D` becomes the
output framebuffer attachment via `mgl_ctx.external_texture(...)`.
"""

import ctypes
import logging

import moderngl
import numpy as np

from streamlib.adapters.opengl import GL_TEXTURE_EXTERNAL_OES

logger = logging.getLogger(__name__)


# We need exactly two raw-GL calls (`glActiveTexture` + `glBindTexture`)
# to bind the camera DMA-BUF on `GL_TEXTURE_EXTERNAL_OES` immediately
# before the bg draw — ModernGL doesn't expose the binding target on
# its `external_texture` / `Texture.use` paths. Both functions are
# core GL entry points always exported by libGL; resolve via ctypes
# rather than dragging in PyOpenGL as a new Linux dep (matches
# CLAUDE.md's "extend core systems" rule — the OS GL loader is
# already what ModernGL uses internally).
_GL_LIB = ctypes.CDLL("libGL.so.1")
_GL_LIB.glActiveTexture.argtypes = [ctypes.c_uint]
_GL_LIB.glActiveTexture.restype = None
_GL_LIB.glBindTexture.argtypes = [ctypes.c_uint, ctypes.c_uint]
_GL_LIB.glBindTexture.restype = None
_GL_TEXTURE0 = 0x84C0


# COCO 17 → bone connectivity (skeleton edges to draw between joints).
# Indices match Ultralytics YOLOv8-pose's keypoint output:
#   0 nose, 1-2 eyes, 3-4 ears, 5-6 shoulders, 7-8 elbows, 9-10 wrists,
#   11-12 hips, 13-14 knees, 15-16 ankles.
COCO_SKELETON_EDGES = [
    (5, 7), (7, 9),         # left arm
    (6, 8), (8, 10),        # right arm
    (5, 6),                 # shoulder line
    (5, 11), (6, 12),       # torso sides
    (11, 12),               # hip line
    (11, 13), (13, 15),     # left leg
    (12, 14), (14, 16),     # right leg
    (0, 5), (0, 6),         # neck
]

# Visibility threshold below which a keypoint is treated as undetected.
KEYPOINT_VISIBILITY_THRESHOLD = 0.25

# Maximum joints rendered per frame (COCO has 17; we cap conservatively).
MAX_KEYPOINTS = 17


# =============================================================================
# Shaders
# =============================================================================

# Background pass — full-screen quad sampling the camera DMA-BUF
# imported as `GL_TEXTURE_EXTERNAL_OES`, with a subtle cyberpunk color
# grade and corner vignette. The sampler type is `samplerExternalOES`
# (from `GL_OES_EGL_image_external`); the caller binds the camera GL
# id to `GL_TEXTURE_EXTERNAL_OES` on texture unit 0 immediately before
# `_bg_vao.render(...)`.
_BACKGROUND_VS = """
#version 330 core
in vec2 in_position;
in vec2 in_texcoord;
out vec2 v_texcoord;
void main() {
    gl_Position = vec4(in_position, 0.0, 1.0);
    v_texcoord = in_texcoord;
}
"""

_BACKGROUND_FS = """
#version 330 core
#extension GL_OES_EGL_image_external : require
in vec2 v_texcoord;
out vec4 frag_color;

uniform samplerExternalOES u_camera;
uniform float u_time;

void main() {
    // Subtle chromatic aberration: shift R and B channels horizontally.
    // Use the GL_OES_EGL_image_external `texture2D(samplerExternalOES,
    // vec2)` overload — NVIDIA's desktop-GL driver does not register the
    // unified `texture(samplerExternalOES, vec2)` overload in
    // `#version 330 core`; that overload is ESSL3-only (would require
    // `_essl3` + an actual GLES context).
    float shift = 0.0025;
    vec3 col;
    col.r = texture2D(u_camera, v_texcoord + vec2(shift, 0.0)).r;
    col.g = texture2D(u_camera, v_texcoord).g;
    col.b = texture2D(u_camera, v_texcoord - vec2(shift, 0.0)).b;

    // Cyberpunk grade: lift mids towards cyan, push shadows slightly violet.
    vec3 graded = mix(col, col * vec3(0.78, 1.05, 1.18), 0.55);
    graded = mix(graded, graded + vec3(0.04, 0.0, 0.06), 0.35);

    // Scanline modulation — thin horizontal banding.
    float scan = 0.94 + 0.06 * sin(v_texcoord.y * 800.0);
    graded *= scan;

    // Vignette.
    vec2 uv = v_texcoord * 2.0 - 1.0;
    float vignette = 1.0 - dot(uv, uv) * 0.35;
    graded *= clamp(vignette, 0.5, 1.0);

    frag_color = vec4(graded, 1.0);
}
"""

# Skeleton lines — fat lines (drawn via triangle strips per edge to get
# adjustable thickness without depending on glLineWidth, which the GL
# core profile deprecates).
_SKELETON_LINES_VS = """
#version 330 core
in vec2 in_position;     // already in NDC (-1..1)
in float in_alpha;
out float v_alpha;
void main() {
    gl_Position = vec4(in_position, 0.0, 1.0);
    v_alpha = in_alpha;
}
"""

_SKELETON_LINES_FS = """
#version 330 core
in float v_alpha;
out vec4 frag_color;
uniform vec3 u_color;
void main() {
    frag_color = vec4(u_color, v_alpha);
}
"""

# Joint dots — instanced glow circles drawn as quads with a radial alpha
# falloff in the fragment shader.
_JOINT_DOTS_VS = """
#version 330 core
in vec2 in_corner;       // (-1..1) per quad corner
in vec2 in_center;       // joint NDC center
in float in_visibility;
in float in_radius;
out vec2 v_local;
out float v_visibility;
void main() {
    v_local = in_corner;
    v_visibility = in_visibility;
    gl_Position = vec4(in_center + in_corner * in_radius, 0.0, 1.0);
}
"""

_JOINT_DOTS_FS = """
#version 330 core
in vec2 v_local;
in float v_visibility;
out vec4 frag_color;
uniform vec3 u_inner_color;
uniform vec3 u_outer_color;
void main() {
    float r = length(v_local);
    if (r > 1.0) {
        discard;
    }
    float core = smoothstep(0.55, 0.0, r);
    float halo = smoothstep(1.0, 0.55, r) * 0.55;
    vec3 col = mix(u_outer_color, u_inner_color, core);
    float alpha = (core + halo) * v_visibility;
    frag_color = vec4(col, alpha);
}
"""


# =============================================================================
# Renderer
# =============================================================================

class PoseOverlayRenderer:
    """Render a cyberpunk-tinted camera background + neon pose overlay.

    All rendering targets the FBO passed to ``render(output_fbo, ...)``.
    The camera background is sampled directly from a
    ``GL_TEXTURE_EXTERNAL_OES`` GL texture id passed into ``render()``
    each frame — no CPU upload step.
    """

    # Cyberpunk palette (David Martinez / Edgerunners adjacent).
    BONE_COLOR = (0.0, 0.94, 1.0)         # cyan
    JOINT_INNER = (1.0, 0.22, 0.85)       # hot magenta core
    JOINT_OUTER = (0.0, 0.94, 1.0)        # cyan halo

    BONE_HALF_THICKNESS_NDC = 0.005       # ~5 px at 1080p
    JOINT_RADIUS_NDC = 0.014              # ~15 px at 1080p

    def __init__(self, ctx: moderngl.Context, width: int, height: int):
        self.ctx = ctx
        self.W = width
        self.H = height

        # 1. Camera background --------------------------------------------
        # The camera texture lives on the OpenGL adapter's
        # `GL_TEXTURE_EXTERNAL_OES` import; the caller binds it onto
        # texture unit 0 immediately before each `render()` call.
        self._bg_program = ctx.program(
            vertex_shader=_BACKGROUND_VS, fragment_shader=_BACKGROUND_FS,
        )
        # Full-screen quad — pos.xy + texcoord.xy interleaved.
        # NDC corners + flipped Y in tex coords (camera bytes upload
        # top-down, OpenGL textures are bottom-up, so we sample with
        # flipped Y to keep the camera the right way up).
        bg_verts = np.array([
            -1.0, -1.0, 0.0, 1.0,
             1.0, -1.0, 1.0, 1.0,
            -1.0,  1.0, 0.0, 0.0,
             1.0,  1.0, 1.0, 0.0,
        ], dtype=np.float32)
        self._bg_vbo = ctx.buffer(bg_verts.tobytes())
        self._bg_vao = ctx.vertex_array(
            self._bg_program,
            [(self._bg_vbo, "2f 2f", "in_position", "in_texcoord")],
        )

        # 2. Skeleton lines -----------------------------------------------
        # Each edge is rendered as a triangle strip (4 vertices) so the
        # line has uniform thickness without relying on `glLineWidth`.
        # Interleaved layout: (x, y, alpha) per vertex. Refilled every
        # frame from the keypoint data.
        max_edges = len(COCO_SKELETON_EDGES)
        # 4 verts per edge, 3 floats each.
        self._line_vbo = ctx.buffer(
            reserve=max_edges * 4 * 3 * 4, dynamic=True,
        )
        self._line_program = ctx.program(
            vertex_shader=_SKELETON_LINES_VS, fragment_shader=_SKELETON_LINES_FS,
        )
        self._line_vao = ctx.vertex_array(
            self._line_program,
            [(self._line_vbo, "2f 1f", "in_position", "in_alpha")],
        )

        # 3. Joint dots ---------------------------------------------------
        # Six vertices per joint (two triangles forming a quad). Per-vertex
        # attributes: corner (local quad coord, [-1,1]^2), center (NDC),
        # visibility, radius.
        self._dot_program = ctx.program(
            vertex_shader=_JOINT_DOTS_VS, fragment_shader=_JOINT_DOTS_FS,
        )
        # 6 verts/quad * MAX_KEYPOINTS, 6 floats/vert (corner.xy +
        # center.xy + visibility + radius).
        self._dot_vbo = ctx.buffer(
            reserve=MAX_KEYPOINTS * 6 * 6 * 4, dynamic=True,
        )
        self._dot_vao = ctx.vertex_array(
            self._dot_program,
            [
                (self._dot_vbo, "2f 2f 1f 1f",
                 "in_corner", "in_center", "in_visibility", "in_radius"),
            ],
        )

        logger.info(
            f"PoseOverlayRenderer: initialized {width}x{height} "
            f"GL {self.ctx.version_code}"
        )

    # =====================================================================
    # Geometry generation
    # =====================================================================

    def _build_line_geometry(self, keypoints_xyc: np.ndarray) -> bytes:
        """Build a triangle-strip VBO covering the visible skeleton edges.

        Args:
            keypoints_xyc: ``(17, 3)`` float array, each row is
                ``(x_pixels, y_pixels, confidence)`` in YOLO output frame.

        Returns:
            Raw bytes for the line VBO. Hidden edges contribute zero-area
            triangles (collapsed to a single point) so the stride into the
            VBO stays constant — simpler than tracking a dynamic count.
        """
        verts = np.zeros((len(COCO_SKELETON_EDGES), 4, 3), dtype=np.float32)

        for edge_idx, (i, j) in enumerate(COCO_SKELETON_EDGES):
            xi, yi, ci = keypoints_xyc[i]
            xj, yj, cj = keypoints_xyc[j]

            if (ci < KEYPOINT_VISIBILITY_THRESHOLD
                    or cj < KEYPOINT_VISIBILITY_THRESHOLD):
                # Collapse to origin so the edge takes zero pixels.
                continue

            # Pixel → NDC (-1..1). Y is flipped because YOLO image-space
            # has Y growing downwards while NDC Y grows upwards.
            ax = (xi / max(self.W, 1)) * 2.0 - 1.0
            ay = -((yi / max(self.H, 1)) * 2.0 - 1.0)
            bx = (xj / max(self.W, 1)) * 2.0 - 1.0
            by = -((yj / max(self.H, 1)) * 2.0 - 1.0)

            dx = bx - ax
            dy = by - ay
            length = float(np.hypot(dx, dy))
            if length < 1e-6:
                continue

            # Perpendicular unit vector, then scale to NDC half-thickness.
            # Aspect-correct so the line looks the same thickness in
            # pixels regardless of frame aspect.
            aspect = self.W / max(self.H, 1)
            nx = (-dy / length) * self.BONE_HALF_THICKNESS_NDC
            ny = (dx / length) * self.BONE_HALF_THICKNESS_NDC * aspect

            edge_alpha = float(min(ci, cj))

            # Triangle strip order: A_left, A_right, B_left, B_right.
            verts[edge_idx, 0] = (ax + nx, ay + ny, edge_alpha)
            verts[edge_idx, 1] = (ax - nx, ay - ny, edge_alpha)
            verts[edge_idx, 2] = (bx + nx, by + ny, edge_alpha)
            verts[edge_idx, 3] = (bx - nx, by - ny, edge_alpha)

        return verts.tobytes()

    def _build_joint_geometry(self, keypoints_xyc: np.ndarray) -> bytes:
        """Build a per-joint quad VBO for the 17 keypoints.

        Each joint is rendered as a 6-vertex quad (two triangles). Hidden
        joints get visibility=0 so the fragment shader contributes no
        color (the geometry is still drawn, just transparently).
        """
        verts = np.zeros((MAX_KEYPOINTS, 6, 6), dtype=np.float32)

        # Corner offsets shared across all joints — local quad UV.
        corners = np.array([
            [-1.0, -1.0],
            [ 1.0, -1.0],
            [-1.0,  1.0],
            [-1.0,  1.0],
            [ 1.0, -1.0],
            [ 1.0,  1.0],
        ], dtype=np.float32)

        aspect = self.W / max(self.H, 1)

        for i in range(MAX_KEYPOINTS):
            if i >= len(keypoints_xyc):
                break
            xp, yp, cp = keypoints_xyc[i]
            if cp < KEYPOINT_VISIBILITY_THRESHOLD:
                continue

            cx = (xp / max(self.W, 1)) * 2.0 - 1.0
            cy = -((yp / max(self.H, 1)) * 2.0 - 1.0)

            for vi in range(6):
                cx_corner, cy_corner = corners[vi]
                # Aspect-correct so the dot stays circular in pixels.
                verts[i, vi, 0] = cx_corner
                verts[i, vi, 1] = cy_corner * aspect
                verts[i, vi, 2] = cx
                verts[i, vi, 3] = cy
                verts[i, vi, 4] = cp
                verts[i, vi, 5] = self.JOINT_RADIUS_NDC

        return verts.tobytes()

    # =====================================================================
    # Render
    # =====================================================================

    def render(
        self,
        output_fbo: moderngl.Framebuffer,
        camera_external_oes_tex_id: int,
        keypoints_xyc: np.ndarray | None,
        time_seconds: float,
    ) -> None:
        """Draw camera background + skeleton + joints into ``output_fbo``.

        Args:
            output_fbo: ModernGL framebuffer wrapping the imported
                opengl-adapter `GL_TEXTURE_2D`.
            camera_external_oes_tex_id: GL texture id from
                ``OpenGLContext.acquire_read_external_oes(camera_uuid)``.
                Must be valid for the duration of this call (the
                caller's ``acquire_read_external_oes`` scope must cover
                ``render``).
            keypoints_xyc: ``(17, 3)`` float array of (x_px, y_px, conf),
                or ``None`` if YOLO produced no detection. When ``None``,
                only the camera background renders.
            time_seconds: Wall-clock time delta for shader animation.
        """
        output_fbo.use()
        output_fbo.viewport = (0, 0, self.W, self.H)
        output_fbo.clear(0.05, 0.04, 0.10, 1.0)

        # ---- Background camera pass -------------------------------------
        # Bind the EXTERNAL_OES camera texture on unit 0 by raw GL —
        # ModernGL's `Texture.use` and `external_texture` only handle
        # `GL_TEXTURE_2D` (no `target` argument in the moderngl 5.x
        # signature), so the OES binding has to be issued by hand.
        self.ctx.disable(moderngl.DEPTH_TEST)
        self.ctx.disable(moderngl.CULL_FACE)
        _GL_LIB.glActiveTexture(_GL_TEXTURE0)
        _GL_LIB.glBindTexture(GL_TEXTURE_EXTERNAL_OES, camera_external_oes_tex_id)
        self._bg_program["u_camera"].value = 0
        if "u_time" in self._bg_program:
            self._bg_program["u_time"].value = float(time_seconds)
        self._bg_vao.render(moderngl.TRIANGLE_STRIP)
        _GL_LIB.glBindTexture(GL_TEXTURE_EXTERNAL_OES, 0)

        if keypoints_xyc is None:
            return

        # ---- Skeleton lines pass ----------------------------------------
        self.ctx.enable(moderngl.BLEND)
        self.ctx.blend_func = (moderngl.SRC_ALPHA, moderngl.ONE_MINUS_SRC_ALPHA)

        line_bytes = self._build_line_geometry(keypoints_xyc)
        self._line_vbo.write(line_bytes)
        self._line_program["u_color"].value = self.BONE_COLOR
        # Draw each edge as its own triangle-strip slice so degenerate
        # collapsed edges contribute nothing.
        for edge_idx in range(len(COCO_SKELETON_EDGES)):
            self._line_vao.render(
                moderngl.TRIANGLE_STRIP, vertices=4, first=edge_idx * 4,
            )

        # ---- Joint dots pass --------------------------------------------
        dot_bytes = self._build_joint_geometry(keypoints_xyc)
        self._dot_vbo.write(dot_bytes)
        self._dot_program["u_inner_color"].value = self.JOINT_INNER
        self._dot_program["u_outer_color"].value = self.JOINT_OUTER
        self._dot_vao.render(moderngl.TRIANGLES)

        self.ctx.disable(moderngl.BLEND)

    # =====================================================================
    # Cleanup
    # =====================================================================

    def release(self) -> None:
        """Release ModernGL resources owned by this renderer."""
        for obj_name in (
            "_bg_vao", "_bg_vbo", "_bg_program",
            "_line_vao", "_line_vbo", "_line_program",
            "_dot_vao", "_dot_vbo", "_dot_program",
        ):
            obj = getattr(self, obj_name, None)
            if obj is not None:
                try:
                    obj.release()
                except Exception:
                    pass
            setattr(self, obj_name, None)
