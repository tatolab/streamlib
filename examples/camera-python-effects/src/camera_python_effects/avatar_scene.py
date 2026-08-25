# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The android's stage: an offscreen ModernGL scene rendered to numpy pixels.

Night-city look-development in one pass: a deep gradient sky, a neon grid
floor with a faked planar reflection (the character drawn mirrored and dimmed
under a translucent floor), and the android itself — flat-shaded hard-surface
primitives with a cyan fresnel rim, a magenta fill light, emissive cores and
a rolling holographic scanline.

This is a third-party GL world, deliberately: ModernGL owns its own context,
vertex buffers and shaders here, and the engine only ever sees the finished
RGBA frame. GL-free callers can still be tested — everything below
`AvatarSceneRenderer` is plain numpy geometry.
"""

from __future__ import annotations

import math

import moderngl
import numpy

from .avatar_rig import SegmentPlacement

__all__ = ["AvatarSceneRenderer", "flat_shaded", "prism_mesh", "sphere_mesh", "unit_box_mesh"]

CHARACTER_VERTEX_SHADER = """
#version 330
in vec3 in_position;
in vec3 in_normal;
uniform mat4 u_view_projection;
uniform mat4 u_model;
uniform mat3 u_normal_matrix;
out vec3 v_world_position;
out vec3 v_world_normal;
void main() {
    vec4 world = u_model * vec4(in_position, 1.0);
    v_world_position = world.xyz;
    // The model carries non-uniform scale (a limb is thin and long), so the
    // normal needs the inverse-transpose — passed in, never mat3(u_model):
    // that bends every facet toward the long axis and floods the fresnel.
    v_world_normal = u_normal_matrix * in_normal;
    gl_Position = u_view_projection * world;
}
"""

CHARACTER_FRAGMENT_SHADER = """
#version 330
in vec3 v_world_position;
in vec3 v_world_normal;
uniform vec3 u_base_colour;
uniform float u_emissive;
uniform vec3 u_camera_position;
uniform float u_elapsed_seconds;
uniform float u_brightness;
out vec4 out_colour;

const vec3 KEY_LIGHT_DIRECTION = normalize(vec3(0.5, 0.85, 0.6));
const vec3 KEY_LIGHT_COLOUR = vec3(0.98, 0.99, 1.0);
const vec3 FILL_LIGHT_DIRECTION = normalize(vec3(-0.7, 0.15, 0.35));
const vec3 FILL_LIGHT_COLOUR = vec3(0.85, 0.10, 0.55);
const vec3 RIM_COLOUR = vec3(0.0, 0.95, 1.0);

void main() {
    vec3 normal = normalize(v_world_normal);
    vec3 to_camera = normalize(u_camera_position - v_world_position);

    float key = max(dot(normal, KEY_LIGHT_DIRECTION), 0.0);
    float fill = max(dot(normal, FILL_LIGHT_DIRECTION), 0.0);
    vec3 lit = u_base_colour * (0.30 + key * KEY_LIGHT_COLOUR * 2.1 + fill * FILL_LIGHT_COLOUR * 1.1);

    // Specular ping off the key light — the chrome in the chrome.
    vec3 half_vector = normalize(KEY_LIGHT_DIRECTION + to_camera);
    lit += KEY_LIGHT_COLOUR * pow(max(dot(normal, half_vector), 0.0), 48.0) * 0.6;

    // Cyan fresnel rim — a true edge light, not a wash.
    float fresnel = pow(1.0 - max(dot(normal, to_camera), 0.0), 5.0);
    lit += RIM_COLOUR * fresnel * 0.8;

    // An emissive part glows in its own colour, brighter than any light
    // could make it, with a gentle pulse.
    float pulse = 0.85 + 0.15 * sin(u_elapsed_seconds * 3.1);
    lit = mix(lit, u_base_colour * 1.7 * pulse, u_emissive);

    // A rolling holographic scanline over everything.
    float scan = 0.978 + 0.022 * sin(v_world_position.y * 140.0 - u_elapsed_seconds * 9.0);
    lit *= scan;

    out_colour = vec4(lit * u_brightness, 1.0);
}
"""

BACKDROP_VERTEX_SHADER = """
#version 330
in vec2 in_position;
out vec2 v_screen;
void main() {
    v_screen = in_position;
    gl_Position = vec4(in_position, 0.0, 1.0);
}
"""

BACKDROP_FRAGMENT_SHADER = """
#version 330
in vec2 v_screen;
uniform float u_elapsed_seconds;
out vec4 out_colour;
void main() {
    // Deep blue-violet falling to near-black overhead, with a faint magenta
    // haze low on the horizon.
    float horizon = clamp(1.0 - (v_screen.y * 0.5 + 0.5), 0.0, 1.0);
    vec3 sky = mix(vec3(0.010, 0.008, 0.030), vec3(0.055, 0.015, 0.085), horizon * horizon);
    sky += vec3(0.25, 0.05, 0.35) * pow(horizon, 6.0) * 0.35;
    // Sparse drifting static, so the void is alive.
    float grain = fract(sin(dot(floor(v_screen * 240.0 + u_elapsed_seconds * 3.0),
                                vec2(12.9898, 78.233))) * 43758.5453);
    sky += vec3(0.05) * step(0.997, grain);
    out_colour = vec4(sky, 1.0);
}
"""

FLOOR_VERTEX_SHADER = """
#version 330
in vec2 in_position;
uniform mat4 u_view_projection;
out vec3 v_world_position;
void main() {
    vec3 world = vec3(in_position.x, 0.0, in_position.y);
    v_world_position = world;
    gl_Position = u_view_projection * vec4(world, 1.0);
}
"""

FLOOR_FRAGMENT_SHADER = """
#version 330
in vec3 v_world_position;
uniform float u_elapsed_seconds;
out vec4 out_colour;

float grid_line(float coordinate) {
    float distance_to_line = abs(fract(coordinate + 0.5) - 0.5);
    return smoothstep(0.035, 0.0, distance_to_line);
}

void main() {
    vec2 cell = v_world_position.xz * 2.2;
    // The grid scrolls toward the camera, slowly — the treadmill of every
    // synthwave horizon.
    cell.y += u_elapsed_seconds * 0.55;
    float minor = max(grid_line(cell.x), grid_line(cell.y));
    float major = max(grid_line(cell.x / 5.0), grid_line(cell.y / 5.0));

    float distance_fade = exp(-length(v_world_position.xz) * 0.30);
    vec3 lines = vec3(0.0, 0.75, 0.85) * minor * 0.6
               + vec3(0.85, 0.10, 0.60) * major * 0.9;

    // Mostly-translucent dark glass: the mirrored character drawn beneath
    // shows through as its reflection.
    vec3 glass = vec3(0.008, 0.008, 0.016);
    float line_alpha = clamp(max(minor, major), 0.0, 1.0) * distance_fade;
    float presence = exp(-length(v_world_position.xz) * 0.16);
    out_colour = vec4(glass + lines * distance_fade,
                      (0.28 + 0.60 * line_alpha) * presence);
}
"""


def flat_shaded(vertices: numpy.ndarray, triangles: numpy.ndarray) -> numpy.ndarray:
    """Indexed triangles → de-indexed `[position, facet-normal]` float32 rows.

    Facet normals — every vertex of a face carries the face's own normal — are
    what give hard-surface primitives their machined look; smooth normals
    would read as rubber.
    """
    corners = vertices[triangles.reshape(-1)].reshape(-1, 3, 3)
    normals = numpy.cross(corners[:, 1] - corners[:, 0], corners[:, 2] - corners[:, 0])
    lengths = numpy.linalg.norm(normals, axis=1, keepdims=True)
    normals = normals / numpy.maximum(lengths, 1e-9)
    per_vertex_normals = numpy.repeat(normals, 3, axis=0)
    return numpy.hstack(
        [corners.reshape(-1, 3), per_vertex_normals]
    ).astype("f4")


def unit_box_mesh() -> numpy.ndarray:
    """A box spanning y 0..1, centred in x/z — grown along its bone."""
    x, z = 0.5, 0.5
    v = numpy.array([
        [-x, 0, -z], [x, 0, -z], [x, 0, z], [-x, 0, z],
        [-x, 1, -z], [x, 1, -z], [x, 1, z], [-x, 1, z],
    ], dtype="f8")
    t = numpy.array([
        [0, 2, 1], [0, 3, 2],  # bottom
        [4, 5, 6], [4, 6, 7],  # top
        [0, 1, 5], [0, 5, 4],  # -z
        [2, 3, 7], [2, 7, 6],  # +z
        [1, 2, 6], [1, 6, 5],  # +x
        [3, 0, 4], [3, 4, 7],  # -x
    ])
    # As listed the faces wind inward; flipped here so every normal points out.
    return flat_shaded(v, t[:, ::-1])


def prism_mesh(sides: int = 8, taper: float = 0.78) -> numpy.ndarray:
    """An octagonal limb segment, slightly narrower at its far end."""
    angles = numpy.linspace(0.0, 2.0 * math.pi, sides, endpoint=False)
    ring = numpy.stack([numpy.cos(angles) * 0.5, numpy.sin(angles) * 0.5], axis=1)
    bottom = numpy.hstack([ring[:, :1], numpy.zeros((sides, 1)), ring[:, 1:]])
    top = numpy.hstack([ring[:, :1] * taper, numpy.ones((sides, 1)), ring[:, 1:] * taper])
    v = numpy.vstack([bottom, top, [[0, 0, 0]], [[0, 1, 0]]])
    triangles = []
    for side in range(sides):
        after = (side + 1) % sides
        triangles += [
            [side, after, sides + after], [side, sides + after, sides + side],
            [2 * sides, after, side], [2 * sides + 1, sides + side, sides + after],
        ]
    # Same inward winding as the box's listing; flipped for outward normals.
    return flat_shaded(v, numpy.array(triangles)[:, ::-1])


def sphere_mesh(rings: int = 6, sectors: int = 10) -> numpy.ndarray:
    """A faceted unit sphere — low-poly on purpose; the facets are the style."""
    v = []
    for ring in range(rings + 1):
        polar = math.pi * ring / rings
        for sector in range(sectors):
            azimuth = 2.0 * math.pi * sector / sectors
            v.append([
                math.sin(polar) * math.cos(azimuth),
                math.cos(polar),
                math.sin(polar) * math.sin(azimuth),
            ])
    triangles = []
    for ring in range(rings):
        for sector in range(sectors):
            after = (sector + 1) % sectors
            a = ring * sectors + sector
            b = ring * sectors + after
            c = (ring + 1) * sectors + after
            d = (ring + 1) * sectors + sector
            if ring != 0:
                triangles.append([a, b, c])
            if ring != rings - 1:
                triangles.append([a, c, d])
    return flat_shaded(numpy.array(v, dtype="f8"), numpy.array(triangles))


def _perspective(fov_y_degrees: float, aspect: float, near: float, far: float) -> numpy.ndarray:
    focal = 1.0 / math.tan(math.radians(fov_y_degrees) / 2.0)
    projection = numpy.zeros((4, 4))
    projection[0, 0] = focal / aspect
    projection[1, 1] = focal
    projection[2, 2] = (far + near) / (near - far)
    projection[2, 3] = 2.0 * far * near / (near - far)
    projection[3, 2] = -1.0
    return projection


def _look_at(eye: numpy.ndarray, target: numpy.ndarray) -> numpy.ndarray:
    forward = eye - target
    forward /= numpy.linalg.norm(forward)
    right = numpy.cross(numpy.array([0.0, 1.0, 0.0]), forward)
    right /= numpy.linalg.norm(right)
    up = numpy.cross(forward, right)
    view = numpy.identity(4)
    view[0, :3], view[1, :3], view[2, :3] = right, up, forward
    view[:3, 3] = -view[:3, :3] @ eye
    return view


class AvatarSceneRenderer:
    """Owns the GL context and turns segment placements into an RGBA frame."""

    def __init__(self, width: int, height: int) -> None:
        self.width = width
        self.height = height
        try:
            self.gl = moderngl.create_context(standalone=True)
        except Exception:
            # A helper with no X reaches the GPU through EGL instead.
            self.gl = moderngl.create_context(standalone=True, backend="egl")
        self.gl.enable(moderngl.DEPTH_TEST)

        self._character_program = self.gl.program(
            vertex_shader=CHARACTER_VERTEX_SHADER,
            fragment_shader=CHARACTER_FRAGMENT_SHADER,
        )
        self._backdrop_program = self.gl.program(
            vertex_shader=BACKDROP_VERTEX_SHADER,
            fragment_shader=BACKDROP_FRAGMENT_SHADER,
        )
        self._floor_program = self.gl.program(
            vertex_shader=FLOOR_VERTEX_SHADER,
            fragment_shader=FLOOR_FRAGMENT_SHADER,
        )

        self._primitives = {
            name: self.gl.vertex_array(
                self._character_program,
                [(self.gl.buffer(mesh.tobytes()), "3f 3f", "in_position", "in_normal")],
            )
            for name, mesh in (
                ("box", unit_box_mesh()),
                ("prism", prism_mesh()),
                ("sphere", sphere_mesh()),
            )
        }
        full_screen = numpy.array([-1, -1, 3, -1, -1, 3], dtype="f4")
        self._backdrop = self.gl.vertex_array(
            self._backdrop_program,
            [(self.gl.buffer(full_screen.tobytes()), "2f", "in_position")],
        )
        floor_quad = numpy.array(
            [-14, -14, 14, -14, 14, 14, -14, -14, 14, 14, -14, 14], dtype="f4"
        )
        self._floor = self.gl.vertex_array(
            self._floor_program,
            [(self.gl.buffer(floor_quad.tobytes()), "2f", "in_position")],
        )

        self._framebuffer = self.gl.framebuffer(
            color_attachments=[self.gl.texture((width, height), 4)],
            depth_attachment=self.gl.depth_renderbuffer((width, height)),
        )

    def _draw_character(
        self,
        placements: "list[SegmentPlacement]",
        mirrored_under_the_floor: bool,
        elapsed_seconds: float,
        camera_position: numpy.ndarray,
    ) -> None:
        mirror = numpy.diag([1.0, -1.0, 1.0, 1.0])
        self._character_program["u_elapsed_seconds"].value = elapsed_seconds
        self._character_program["u_camera_position"].value = tuple(camera_position)
        self._character_program["u_brightness"].value = 0.85 if mirrored_under_the_floor else 1.0
        # A mirrored draw flips the winding, so the cull side flips with it.
        self.gl.front_face = "cw" if mirrored_under_the_floor else "ccw"
        for placement in placements:
            model = mirror @ placement.model_matrix if mirrored_under_the_floor else placement.model_matrix
            # Inverse-transpose of rotation-times-scale is the rotation with
            # the scale divided back out — the model's columns, normalized.
            linear = model[:3, :3]
            normal_matrix = linear / numpy.linalg.norm(linear, axis=0, keepdims=True)
            self._character_program["u_model"].write(model.T.astype("f4").tobytes())
            self._character_program["u_normal_matrix"].write(
                normal_matrix.T.astype("f4").tobytes()
            )
            self._character_program["u_base_colour"].value = placement.base_colour
            self._character_program["u_emissive"].value = placement.emissive
            self._primitives[placement.primitive].render(moderngl.TRIANGLES)

    def render(
        self, placements: "list[SegmentPlacement]", elapsed_seconds: float
    ) -> numpy.ndarray:
        """One frame of the stage, as an (height, width, 4) uint8 array."""
        sway = math.sin(elapsed_seconds * 0.32)
        eye = numpy.array([sway * 0.30, 1.22 + math.sin(elapsed_seconds * 0.21) * 0.05, 2.65])
        target = numpy.array([0.0, 0.98, 0.0])
        view_projection = (
            _perspective(46.0, self.width / self.height, 0.1, 60.0) @ _look_at(eye, target)
        ).T.astype("f4")

        self._framebuffer.use()
        self.gl.clear(0.0, 0.0, 0.0, 1.0)

        self.gl.disable(moderngl.DEPTH_TEST)
        self._backdrop_program["u_elapsed_seconds"].value = elapsed_seconds
        self._backdrop.render(moderngl.TRIANGLES)
        self.gl.enable(moderngl.DEPTH_TEST)

        self._character_program["u_view_projection"].write(view_projection.tobytes())
        self._floor_program["u_view_projection"].write(view_projection.tobytes())

        # Reflection first, then the translucent grid floor over it, then the
        # android itself — three passes of fake it till it looks expensive.
        self._draw_character(placements, True, elapsed_seconds, eye)
        self.gl.front_face = "ccw"
        self.gl.enable(moderngl.BLEND)
        self._floor_program["u_elapsed_seconds"].value = elapsed_seconds
        self._floor.render(moderngl.TRIANGLES)
        self.gl.disable(moderngl.BLEND)
        self._draw_character(placements, False, elapsed_seconds, eye)

        pixels = numpy.frombuffer(
            self._framebuffer.read(components=4), dtype=numpy.uint8
        ).reshape(self.height, self.width, 4)
        # GL reads bottom-up; the engine's textures are top-down.
        return numpy.ascontiguousarray(pixels[::-1])
