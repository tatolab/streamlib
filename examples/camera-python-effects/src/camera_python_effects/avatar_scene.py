# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The android's stage: an offscreen ModernGL scene rendered to numpy pixels.

The full stage rig: a synthwave sun sinking behind the figure, two coloured
point lights orbiting it, a neon grid floor with a faked planar reflection and
a contact glow pooling under the feet, drifting dust motes, additive motion
trails that arm when the dancer moves fast — and a real bloom pass over the
lot, because neon that does not glow is just paint.

This is a third-party GL world, deliberately: ModernGL owns its own context,
vertex buffers, framebuffers and shaders here, and the engine only ever sees
the finished RGBA frame. GL-free callers can still be tested — everything
below `AvatarSceneRenderer` is plain numpy geometry.
"""

from __future__ import annotations

import math
from collections import deque

import moderngl
import numpy

from .avatar_rig import SegmentPlacement

__all__ = [
    "AvatarSceneRenderer",
    "flat_shaded",
    "prism_mesh",
    "sphere_mesh",
    "unit_box_mesh",
]

# How many past poses the trail keeps, how many ghosts it draws from them, and
# the mean segment speed (units/s) at which the trail reaches full strength.
TRAIL_POSE_MEMORY = 14
TRAIL_GHOST_COUNT = 4
TRAIL_FULL_STRENGTH_SPEED = 1.6

DUST_MOTE_COUNT = 240

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
uniform vec3 u_orbit_light_a;
uniform vec3 u_orbit_light_b;
// Ghost mode: the whole fragment collapses to an additive rim shell.
uniform float u_ghost_alpha;
out vec4 out_colour;

const vec3 KEY_LIGHT_DIRECTION = normalize(vec3(0.5, 0.85, 0.6));
const vec3 KEY_LIGHT_COLOUR = vec3(0.98, 0.99, 1.0);
const vec3 FILL_LIGHT_DIRECTION = normalize(vec3(-0.7, 0.15, 0.35));
const vec3 FILL_LIGHT_COLOUR = vec3(0.85, 0.10, 0.55);
const vec3 RIM_COLOUR = vec3(0.0, 0.95, 1.0);
const vec3 ORBIT_A_COLOUR = vec3(0.10, 0.95, 1.0);
const vec3 ORBIT_B_COLOUR = vec3(1.00, 0.12, 0.62);

vec3 point_light(vec3 light_position, vec3 light_colour, vec3 normal, vec3 to_camera) {
    vec3 to_light = light_position - v_world_position;
    float falloff = 1.0 / (1.0 + dot(to_light, to_light) * 0.55);
    vec3 light_direction = normalize(to_light);
    float diffuse = max(dot(normal, light_direction), 0.0);
    vec3 half_vector = normalize(light_direction + to_camera);
    float specular = pow(max(dot(normal, half_vector), 0.0), 64.0);
    return light_colour * falloff * (diffuse * 0.9 + specular * 1.4);
}

void main() {
    vec3 normal = normalize(v_world_normal);
    vec3 to_camera = normalize(u_camera_position - v_world_position);
    float fresnel = pow(1.0 - max(dot(normal, to_camera), 0.0), 5.0);

    if (u_ghost_alpha > 0.0) {
        // An afterimage is pure edge: the body's shell in trail cyan,
        // brightest at the silhouette, added over what came before.
        out_colour = vec4(RIM_COLOUR * (0.10 + fresnel * 1.4) * u_ghost_alpha, 1.0);
        return;
    }

    float key = max(dot(normal, KEY_LIGHT_DIRECTION), 0.0);
    float fill = max(dot(normal, FILL_LIGHT_DIRECTION), 0.0);
    vec3 lit = u_base_colour * (0.30 + key * KEY_LIGHT_COLOUR * 2.1 + fill * FILL_LIGHT_COLOUR * 1.1);

    // Specular ping off the key light — the chrome in the chrome.
    vec3 half_vector = normalize(KEY_LIGHT_DIRECTION + to_camera);
    lit += KEY_LIGHT_COLOUR * pow(max(dot(normal, half_vector), 0.0), 48.0) * 0.6;

    // The two orbiting stage lights, cyan and magenta, chasing each other.
    lit += u_base_colour * 6.0 * point_light(u_orbit_light_a, ORBIT_A_COLOUR, normal, to_camera);
    lit += u_base_colour * 6.0 * point_light(u_orbit_light_b, ORBIT_B_COLOUR, normal, to_camera);

    // Cyan fresnel rim — a true edge light, not a wash.
    lit += RIM_COLOUR * fresnel * 0.8;

    // An emissive part glows in its own colour, brighter than any light
    // could make it, with a gentle pulse.
    float pulse = 0.85 + 0.15 * sin(u_elapsed_seconds * 3.1);
    lit = mix(lit, u_base_colour * 2.1 * pulse, u_emissive);

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
uniform float u_aspect;
out vec4 out_colour;

float hash21(vec2 p) {
    p = fract(p * vec2(234.34, 435.345));
    p += dot(p, p + 34.23);
    return fract(p.x * p.y);
}

void main() {
    // Deep blue-violet falling to near-black overhead, with a magenta haze
    // low on the horizon.
    float horizon = clamp(1.0 - (v_screen.y * 0.5 + 0.5), 0.0, 1.0);
    vec3 sky = mix(vec3(0.010, 0.008, 0.030), vec3(0.055, 0.015, 0.085), horizon * horizon);
    sky += vec3(0.25, 0.05, 0.35) * pow(horizon, 6.0) * 0.35;

    // The synthwave sun, sunk halfway behind the horizon line the floor cuts.
    vec2 from_sun = vec2((v_screen.x - 0.02) * u_aspect, v_screen.y - 0.28);
    float sun_distance = length(from_sun);
    float sun_disc = smoothstep(0.46, 0.44, sun_distance);
    // Slit gaps widen toward the sun's lower half — the genre's signature.
    float below_centre = clamp(-from_sun.y * 3.2, 0.0, 1.0);
    float slit = step(0.30 * below_centre, fract(from_sun.y * 14.0 - u_elapsed_seconds * 0.15));
    vec3 sun_colour = mix(vec3(1.00, 0.18, 0.55), vec3(1.00, 0.62, 0.12), below_centre);
    sky += sun_colour * sun_disc * slit * 0.9;
    // The halo bleeds past the disc so the bloom pass has something to take.
    sky += sun_colour * exp(-sun_distance * 3.4) * 0.45;

    // Sparse drifting static, so the void is alive.
    float grain = hash21(floor(v_screen * 240.0 + u_elapsed_seconds * 3.0));
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
uniform vec2 u_character_ground_position;
uniform vec3 u_orbit_light_a;
uniform vec3 u_orbit_light_b;
out vec4 out_colour;

const vec3 ORBIT_A_COLOUR = vec3(0.10, 0.95, 1.0);
const vec3 ORBIT_B_COLOUR = vec3(1.00, 0.12, 0.62);

float grid_line(float coordinate) {
    float distance_to_line = abs(fract(coordinate + 0.5) - 0.5);
    return smoothstep(0.035, 0.0, distance_to_line);
}

vec3 light_pool(vec3 light_position, vec3 light_colour) {
    vec2 to_light = light_position.xz - v_world_position.xz;
    float spread = 1.0 + light_position.y * 0.8;
    return light_colour / (1.0 + dot(to_light, to_light) * 2.2 / spread);
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

    // The orbit lights pool on the glass and charge the lines they cross.
    vec3 pooled = light_pool(u_orbit_light_a, ORBIT_A_COLOUR)
                + light_pool(u_orbit_light_b, ORBIT_B_COLOUR);
    lines *= 1.0 + pooled * 2.5;

    // Contact glow under the dancer, cyan, tight.
    float to_character = length(v_world_position.xz - u_character_ground_position);
    vec3 contact = vec3(0.0, 0.85, 1.0) * exp(-to_character * 2.6) * 0.5;

    // Mostly-translucent dark glass: the mirrored character drawn beneath
    // shows through as its reflection.
    vec3 glass = vec3(0.008, 0.008, 0.016) + pooled * 0.06 + contact;
    float line_alpha = clamp(max(minor, major), 0.0, 1.0) * distance_fade;
    float presence = exp(-length(v_world_position.xz) * 0.16);
    out_colour = vec4(glass + lines * distance_fade,
                      (0.28 + 0.60 * line_alpha) * presence);
}
"""

DUST_VERTEX_SHADER = """
#version 330
uniform mat4 u_view_projection;
uniform float u_elapsed_seconds;
out float v_mote_seed;

float hash11(float p) {
    p = fract(p * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return fract(p);
}

void main() {
    float mote = float(gl_VertexID);
    v_mote_seed = hash11(mote * 1.618);
    float column = hash11(mote) * 4.4 - 2.2;
    float depth = hash11(mote + 97.0) * 3.6 - 1.4;
    float rise = fract(hash11(mote + 7.0) + u_elapsed_seconds * (0.014 + v_mote_seed * 0.03));
    float wobble = sin(u_elapsed_seconds * (0.5 + v_mote_seed) + mote) * 0.08;
    vec4 world = vec4(column + wobble, rise * 2.6, depth, 1.0);
    gl_Position = u_view_projection * world;
    gl_PointSize = mix(1.5, 4.5, v_mote_seed) / max(gl_Position.w, 0.4);
}
"""

DUST_FRAGMENT_SHADER = """
#version 330
in float v_mote_seed;
out vec4 out_colour;
void main() {
    // A soft disc, not a square.
    float to_centre = length(gl_PointCoord - 0.5);
    float body = smoothstep(0.5, 0.1, to_centre);
    vec3 tint = mix(vec3(0.1, 0.9, 1.0), vec3(1.0, 0.2, 0.7), step(0.8, v_mote_seed));
    out_colour = vec4(tint * body * 0.35, 1.0);
}
"""

FULL_SCREEN_VERTEX_SHADER = """
#version 330
in vec2 in_position;
out vec2 v_uv;
void main() {
    v_uv = in_position * 0.5 + 0.5;
    gl_Position = vec4(in_position, 0.0, 1.0);
}
"""

BLOOM_EXTRACT_FRAGMENT_SHADER = """
#version 330
in vec2 v_uv;
uniform sampler2D u_scene;
out vec4 out_colour;
void main() {
    vec3 colour = texture(u_scene, v_uv).rgb;
    // Only what genuinely shines feeds the glow.
    out_colour = vec4(max(colour - vec3(0.52), vec3(0.0)) * 1.5, 1.0);
}
"""

BLOOM_BLUR_FRAGMENT_SHADER = """
#version 330
in vec2 v_uv;
uniform sampler2D u_source;
uniform vec2 u_step;
out vec4 out_colour;
void main() {
    const float weights[5] = float[](0.227027, 0.194594, 0.121622, 0.054054, 0.016216);
    vec3 blurred = texture(u_source, v_uv).rgb * weights[0];
    for (int tap = 1; tap < 5; tap++) {
        blurred += texture(u_source, v_uv + u_step * float(tap)).rgb * weights[tap];
        blurred += texture(u_source, v_uv - u_step * float(tap)).rgb * weights[tap];
    }
    out_colour = vec4(blurred, 1.0);
}
"""

BLOOM_COMPOSITE_FRAGMENT_SHADER = """
#version 330
in vec2 v_uv;
uniform sampler2D u_scene;
uniform sampler2D u_bloom;
out vec4 out_colour;
void main() {
    vec3 colour = texture(u_scene, v_uv).rgb + texture(u_bloom, v_uv).rgb * 1.25;
    // A soft shoulder keeps the sun and the glow from clipping flat.
    colour = colour / (1.0 + colour * 0.16);
    out_colour = vec4(colour, 1.0);
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
    return numpy.hstack([corners.reshape(-1, 3), per_vertex_normals]).astype("f4")


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


def _character_speed(
    newest: "list[SegmentPlacement]", previous: "list[SegmentPlacement]", dt: float
) -> float:
    """Mean segment speed between two poses — what arms the motion trail."""
    if len(newest) != len(previous) or dt <= 0.0:
        return 0.0
    travelled = numpy.mean([
        float(numpy.linalg.norm(a.model_matrix[:3, 3] - b.model_matrix[:3, 3]))
        for a, b in zip(newest, previous)
    ])
    return float(travelled) / dt


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
        self.gl.enable(moderngl.PROGRAM_POINT_SIZE)

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
        self._dust_program = self.gl.program(
            vertex_shader=DUST_VERTEX_SHADER,
            fragment_shader=DUST_FRAGMENT_SHADER,
        )
        self._bloom_extract_program = self.gl.program(
            vertex_shader=FULL_SCREEN_VERTEX_SHADER,
            fragment_shader=BLOOM_EXTRACT_FRAGMENT_SHADER,
        )
        self._bloom_blur_program = self.gl.program(
            vertex_shader=FULL_SCREEN_VERTEX_SHADER,
            fragment_shader=BLOOM_BLUR_FRAGMENT_SHADER,
        )
        self._bloom_composite_program = self.gl.program(
            vertex_shader=FULL_SCREEN_VERTEX_SHADER,
            fragment_shader=BLOOM_COMPOSITE_FRAGMENT_SHADER,
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
        full_screen_buffer = self.gl.buffer(
            numpy.array([-1, -1, 3, -1, -1, 3], dtype="f4").tobytes()
        )
        self._backdrop = self.gl.vertex_array(
            self._backdrop_program, [(full_screen_buffer, "2f", "in_position")]
        )
        self._bloom_extract = self.gl.vertex_array(
            self._bloom_extract_program, [(full_screen_buffer, "2f", "in_position")]
        )
        self._bloom_blur = self.gl.vertex_array(
            self._bloom_blur_program, [(full_screen_buffer, "2f", "in_position")]
        )
        self._bloom_composite = self.gl.vertex_array(
            self._bloom_composite_program, [(full_screen_buffer, "2f", "in_position")]
        )
        floor_quad = numpy.array(
            [-14, -14, 14, -14, 14, 14, -14, -14, 14, 14, -14, 14], dtype="f4"
        )
        self._floor = self.gl.vertex_array(
            self._floor_program,
            [(self.gl.buffer(floor_quad.tobytes()), "2f", "in_position")],
        )
        self._dust = self.gl.vertex_array(self._dust_program, [])

        # The scene renders into its own texture so the bloom chain can read
        # it; half-resolution ping-pong targets carry the blur; the output
        # framebuffer is what reaches the CPU.
        self._scene_texture = self.gl.texture((width, height), 4)
        self._scene_framebuffer = self.gl.framebuffer(
            color_attachments=[self._scene_texture],
            depth_attachment=self.gl.depth_renderbuffer((width, height)),
        )
        half = (max(width // 2, 1), max(height // 2, 1))
        self._bloom_texture_a = self.gl.texture(half, 4)
        self._bloom_texture_b = self.gl.texture(half, 4)
        for bloom_texture in (self._bloom_texture_a, self._bloom_texture_b):
            bloom_texture.repeat_x = bloom_texture.repeat_y = False
        self._bloom_framebuffer_a = self.gl.framebuffer([self._bloom_texture_a])
        self._bloom_framebuffer_b = self.gl.framebuffer([self._bloom_texture_b])
        self._output_framebuffer = self.gl.framebuffer([self.gl.texture((width, height), 4)])

        self._recent_poses: "deque[tuple[float, list[SegmentPlacement]]]" = deque(
            maxlen=TRAIL_POSE_MEMORY
        )

    def _draw_character(
        self,
        placements: "list[SegmentPlacement]",
        elapsed_seconds: float,
        camera_position: numpy.ndarray,
        *,
        mirrored_under_the_floor: bool = False,
        ghost_alpha: float = 0.0,
    ) -> None:
        mirror = numpy.diag([1.0, -1.0, 1.0, 1.0])
        self._character_program["u_elapsed_seconds"].value = elapsed_seconds
        self._character_program["u_camera_position"].value = tuple(camera_position)
        self._character_program["u_brightness"].value = 0.85 if mirrored_under_the_floor else 1.0
        self._character_program["u_ghost_alpha"].value = ghost_alpha
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

    def _trail_strength(self) -> float:
        if len(self._recent_poses) < 2:
            return 0.0
        previous_time, previous_pose = self._recent_poses[-2]
        newest_time, newest_pose = self._recent_poses[-1]
        speed = _character_speed(newest_pose, previous_pose, newest_time - previous_time)
        return min(speed / TRAIL_FULL_STRENGTH_SPEED, 1.0)

    def render(
        self, placements: "list[SegmentPlacement]", elapsed_seconds: float
    ) -> numpy.ndarray:
        """One frame of the stage, as an (height, width, 4) uint8 array."""
        self._recent_poses.append((elapsed_seconds, placements))

        sway = math.sin(elapsed_seconds * 0.32)
        eye = numpy.array([sway * 0.30, 1.22 + math.sin(elapsed_seconds * 0.21) * 0.05, 2.65])
        target = numpy.array([0.0, 0.98, 0.0])
        view_projection = (
            _perspective(46.0, self.width / self.height, 0.1, 60.0) @ _look_at(eye, target)
        ).T.astype("f4")

        orbit_light_a = (
            math.cos(elapsed_seconds * 0.9) * 1.5,
            1.35 + math.sin(elapsed_seconds * 0.7) * 0.3,
            math.sin(elapsed_seconds * 0.9) * 1.5,
        )
        orbit_light_b = (
            math.cos(-elapsed_seconds * 0.6 + 2.4) * 1.2,
            0.65 + math.sin(elapsed_seconds * 0.5) * 0.25,
            math.sin(-elapsed_seconds * 0.6 + 2.4) * 1.2,
        )
        self._character_program["u_orbit_light_a"].value = orbit_light_a
        self._character_program["u_orbit_light_b"].value = orbit_light_b
        self._floor_program["u_orbit_light_a"].value = orbit_light_a
        self._floor_program["u_orbit_light_b"].value = orbit_light_b

        # The torso placements lead the list; their translation is where the
        # contact glow pools.
        ground = placements[0].model_matrix[:3, 3] if placements else numpy.zeros(3)
        self._floor_program["u_character_ground_position"].value = (
            float(ground[0]),
            float(ground[2]),
        )

        self._scene_framebuffer.use()
        self.gl.disable(moderngl.BLEND)
        self.gl.disable(moderngl.DEPTH_TEST)
        self.gl.clear(0.0, 0.0, 0.0, 1.0)

        self._backdrop_program["u_elapsed_seconds"].value = elapsed_seconds
        self._backdrop_program["u_aspect"].value = self.width / self.height
        self._backdrop.render(moderngl.TRIANGLES)

        self.gl.enable(moderngl.DEPTH_TEST)
        self._character_program["u_view_projection"].write(view_projection.tobytes())
        self._floor_program["u_view_projection"].write(view_projection.tobytes())
        self._dust_program["u_view_projection"].write(view_projection.tobytes())

        # Reflection first, then the translucent grid floor over it, then the
        # android itself — three passes of fake it till it looks expensive.
        self._draw_character(
            placements, elapsed_seconds, eye, mirrored_under_the_floor=True
        )
        self.gl.front_face = "ccw"
        self.gl.enable(moderngl.BLEND)
        self.gl.blend_func = moderngl.SRC_ALPHA, moderngl.ONE_MINUS_SRC_ALPHA
        self._floor_program["u_elapsed_seconds"].value = elapsed_seconds
        self._floor.render(moderngl.TRIANGLES)
        self.gl.disable(moderngl.BLEND)
        self._draw_character(placements, elapsed_seconds, eye)

        # Afterimages, added over the solid body, armed by how fast it moves —
        # a still figure leaves none, a dancing one smears cyan.
        trail = self._trail_strength()
        if trail > 0.05 and len(self._recent_poses) > TRAIL_GHOST_COUNT:
            self.gl.enable(moderngl.BLEND)
            self.gl.blend_func = moderngl.SRC_ALPHA, moderngl.ONE
            stride = max(len(self._recent_poses) // (TRAIL_GHOST_COUNT + 1), 1)
            ghost_poses = list(self._recent_poses)[:-1][::-stride][:TRAIL_GHOST_COUNT]
            for age, (_, ghost_pose) in enumerate(ghost_poses, start=1):
                fade = trail * 0.32 * (1.0 - age / (TRAIL_GHOST_COUNT + 1.0))
                self._draw_character(
                    ghost_pose, elapsed_seconds, eye, ghost_alpha=fade
                )
            self.gl.disable(moderngl.BLEND)

        # Dust, additive, drifting through the light.
        self.gl.enable(moderngl.BLEND)
        self.gl.blend_func = moderngl.SRC_ALPHA, moderngl.ONE
        self._dust_program["u_elapsed_seconds"].value = elapsed_seconds
        self._dust.render(moderngl.POINTS, vertices=DUST_MOTE_COUNT)
        self.gl.disable(moderngl.BLEND)
        self.gl.disable(moderngl.DEPTH_TEST)

        # The bloom chain: extract what shines, blur it twice at half
        # resolution, lay it back over the scene with a soft shoulder.
        self._bloom_framebuffer_a.use()
        self._scene_texture.use(0)
        self._bloom_extract_program["u_scene"].value = 0
        self._bloom_extract.render(moderngl.TRIANGLES)

        half_width, half_height = self._bloom_texture_a.size
        self._bloom_framebuffer_b.use()
        self._bloom_texture_a.use(0)
        self._bloom_blur_program["u_source"].value = 0
        self._bloom_blur_program["u_step"].value = (1.0 / half_width, 0.0)
        self._bloom_blur.render(moderngl.TRIANGLES)
        self._bloom_framebuffer_a.use()
        self._bloom_texture_b.use(0)
        self._bloom_blur_program["u_step"].value = (0.0, 1.0 / half_height)
        self._bloom_blur.render(moderngl.TRIANGLES)

        self._output_framebuffer.use()
        self._scene_texture.use(0)
        self._bloom_texture_a.use(1)
        self._bloom_composite_program["u_scene"].value = 0
        self._bloom_composite_program["u_bloom"].value = 1
        self._bloom_composite.render(moderngl.TRIANGLES)

        pixels = numpy.frombuffer(
            self._output_framebuffer.read(components=4), dtype=numpy.uint8
        ).reshape(self.height, self.width, 4)
        # GL reads bottom-up; the engine's textures are top-down.
        return numpy.ascontiguousarray(pixels[::-1])
