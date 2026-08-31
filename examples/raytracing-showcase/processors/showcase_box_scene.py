# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""The scene both renderers draw, and the GLSL the two of them share.

One table places ten boxes — a sky, a mirror floor, and eight on a ring. Out
of it come the acceleration-structure instances the ray tracer traces and the
`const` arrays the rasterizer's vertex shader reads, so the two halves of the
picture cannot drift into drawing different scenes.

That sharing is only possible because a StreamLib shader is a Python string
the engine compiles at `setup()`: the scene is *generated into* the GLSL. An
ahead-of-time `glslc` step would leave this table spelled twice, once in
Python and once in each shader.

Nothing here is a processor. The camera and the light are the only things
that move, and they move through push constants: a scene's geometry lives in
an acceleration structure, and `build_tlas` is a method on the Full
capability, which is `setup()`'s alone.
"""

from __future__ import annotations

import dataclasses
import math
from typing import Sequence

# The camera laps the scene about every twenty seconds and the light about
# every seven, so the shadows sweep across the floor on a period that is not
# the camera's — a run never looks like it is repeating a loop.
CAMERA_ORBIT_RADIUS = 9.5
CAMERA_HEIGHT = 3.6
CAMERA_LOOK_AT_HEIGHT = 1.0
CAMERA_ORBIT_RADIANS_PER_SECOND = 0.31
CAMERA_FIELD_OF_VIEW_RADIANS = 0.95

LIGHT_ORBIT_RADIUS = 7.0
LIGHT_HEIGHT = 4.6
LIGHT_ORBIT_RADIANS_PER_SECOND = 0.89

# How much of the floor's colour comes from what it reflects. The ray-traced
# side is the only one that can answer that question at all.
FLOOR_MIRROR_STRENGTH = 0.38

AMBIENT_LIGHT = 0.14
BOX_EDGE_DARKENING = 0.45

# Pushing the shadow ray off the surface it starts from, so a hit does not
# immediately re-hit the geometry it left.
SHADOW_RAY_BIAS = 0.004

# The near and far planes the rasterizer projects between. The ray tracer
# needs neither — a ray has an interval, not a frustum — so they are the
# rasterizer's own numbers, sized to hold the sky box.
NEAR_PLANE = 0.05
FAR_PLANE = 500.0

SKY_BOX_INDEX = 0
FLOOR_BOX_INDEX = 1
FIRST_RING_CUBE_BOX_INDEX = 2
RING_CUBE_COUNT = 8
RING_RADIUS = 4.3

# Four bits per cube, so the whole back-to-front order of the ring packs into
# one push-constant word.
DRAW_ORDER_BITS_PER_CUBE = 4


@dataclasses.dataclass(frozen=True)
class ShowcaseBox:
    """One axis-aligned box: where it sits, how big it is, what colour it is."""

    centre: tuple[float, float, float]
    size: tuple[float, float, float]
    colour: tuple[float, float, float]


# Each ring cube's size and colour. Heights vary a lot on purpose: a pillar
# throws a long shadow and a slab throws a short one, which is what makes the
# light's orbit legible on the traced side.
_RING_CUBE_SIZES_AND_COLOURS: tuple[
    tuple[tuple[float, float, float], tuple[float, float, float]], ...
] = (
    ((1.10, 1.10, 1.10), (0.95, 0.27, 0.31)),
    ((0.78, 2.40, 0.78), (0.98, 0.60, 0.22)),
    ((1.50, 0.72, 1.50), (0.96, 0.87, 0.33)),
    ((0.92, 1.80, 0.92), (0.36, 0.87, 0.44)),
    ((1.24, 1.24, 1.24), (0.28, 0.83, 0.91)),
    ((0.72, 2.85, 0.72), (0.34, 0.54, 0.98)),
    ((1.40, 0.94, 1.40), (0.66, 0.42, 0.96)),
    ((1.02, 1.55, 1.02), (0.96, 0.39, 0.79)),
)

RING_CUBE_AZIMUTHS: tuple[float, ...] = tuple(
    index * math.tau / RING_CUBE_COUNT for index in range(RING_CUBE_COUNT)
)


def _ring_cube(index: int) -> ShowcaseBox:
    """The ring's `index`-th cube, sitting on the floor at its own azimuth."""
    size, colour = _RING_CUBE_SIZES_AND_COLOURS[index]
    azimuth = RING_CUBE_AZIMUTHS[index]
    return ShowcaseBox(
        centre=(
            RING_RADIUS * math.sin(azimuth),
            size[1] / 2.0,
            RING_RADIUS * math.cos(azimuth),
        ),
        size=size,
        colour=colour,
    )


SHOWCASE_BOXES: tuple[ShowcaseBox, ...] = (
    # The sky, drawn only by the rasterizer — it has no miss shader to answer
    # a ray that hits nothing with, so it paints the inside of a box big
    # enough to hold the scene. Its colour field goes unread: both sides get
    # the gradient from `sky_colour_towards` below.
    ShowcaseBox(centre=(0.0, 0.0, 0.0), size=(320.0, 320.0, 320.0), colour=(0.0, 0.0, 0.0)),
    # The mirror floor, its top face at y = 0. Light enough to read a
    # shadow against: on a near-black floor the traced half's whole point
    # would be invisible.
    ShowcaseBox(centre=(0.0, -0.2, 0.0), size=(26.0, 0.4, 26.0), colour=(0.20, 0.21, 0.25)),
    *(_ring_cube(index) for index in range(RING_CUBE_COUNT)),
)

# The unit cube every box is an instance of, spanning [-0.5, 0.5]³ — the one
# piece of geometry in the app. The ray tracer builds its bottom-level
# acceleration structure from these two lists and the rasterizer reads them
# back as `const` arrays, so there is one mesh and not two.
UNIT_CUBE_CORNER_POSITIONS: tuple[float, ...] = (
    -0.5, -0.5, -0.5,
    0.5, -0.5, -0.5,
    0.5, 0.5, -0.5,
    -0.5, 0.5, -0.5,
    -0.5, -0.5, 0.5,
    0.5, -0.5, 0.5,
    0.5, 0.5, 0.5,
    -0.5, 0.5, 0.5,
)

UNIT_CUBE_TRIANGLE_INDICES: tuple[int, ...] = (
    0, 1, 2, 0, 2, 3,  # -Z
    4, 6, 5, 4, 7, 6,  # +Z
    4, 0, 3, 4, 3, 7,  # -X
    1, 5, 6, 1, 6, 2,  # +X
    0, 4, 5, 0, 5, 1,  # -Y
    3, 2, 6, 3, 6, 7,  # +Y
)

# One per face, in the order the index list walks them. Six vertices of the
# list share a face, which is how the vertex shader finds a normal without a
# vertex buffer to read one from.
UNIT_CUBE_FACE_NORMALS: tuple[tuple[float, float, float], ...] = (
    (0.0, 0.0, -1.0),
    (0.0, 0.0, 1.0),
    (-1.0, 0.0, 0.0),
    (1.0, 0.0, 0.0),
    (0.0, -1.0, 0.0),
    (0.0, 1.0, 0.0),
)

UNIT_CUBE_VERTEX_COUNT = len(UNIT_CUBE_TRIANGLE_INDICES)


def showcase_tlas_instances(cube_structure: object) -> list[dict[str, object]]:
    """One instance per traced box — the floor and the ring, never the sky.

    Every instance places the same bottom-level structure; what differs is the
    row-major 3×4 affine that stretches the unit cube into that box. The
    `custom_index` is the box's own index in `SHOWCASE_BOXES`, which is how a
    hit shader reads its colour back out of the generated table.
    """
    return [
        {
            "blas": cube_structure,
            "transform": _row_major_scale_and_translation(box),
            "custom_index": index,
        }
        for index, box in enumerate(SHOWCASE_BOXES)
        if index != SKY_BOX_INDEX
    ]


def _row_major_scale_and_translation(box: ShowcaseBox) -> list[float]:
    """The row-major 3×4 affine mapping the unit cube onto `box`."""
    width, height, depth = box.size
    centre_x, centre_y, centre_z = box.centre
    return [
        width, 0.0, 0.0, centre_x,
        0.0, height, 0.0, centre_y,
        0.0, 0.0, depth, centre_z,
    ]


def camera_position_at(elapsed_seconds: float) -> tuple[float, float, float]:
    """Where the camera is, in world space.

    Spelled here as well as in the GLSL because the sort below needs it on the
    CPU and the shaders need it on the GPU. Both read the constants above, so
    the two can disagree about nothing except this arithmetic.
    """
    azimuth = elapsed_seconds * CAMERA_ORBIT_RADIANS_PER_SECOND
    return (
        CAMERA_ORBIT_RADIUS * math.sin(azimuth),
        CAMERA_HEIGHT,
        CAMERA_ORBIT_RADIUS * math.cos(azimuth),
    )


def packed_ring_draw_order_furthest_first(elapsed_seconds: float) -> int:
    """The ring's cubes furthest-from-the-camera first, four bits each.

    The rasterizing pass has no depth attachment to test against — depth
    attachments are an unbuilt engine capability, reachable from neither
    language — so its picture is only right if the boxes are painted back to
    front. Ordering by the distance from the camera to each cube's centre is
    exact here because the cubes are convex and none of them touches another:
    one is then wholly in front of the other or wholly beside it, and there is
    no pair a single order gets wrong.
    """
    eye = camera_position_at(elapsed_seconds)
    furthest_first = sorted(
        range(RING_CUBE_COUNT),
        key=lambda cube: -math.dist(
            eye, SHOWCASE_BOXES[FIRST_RING_CUBE_BOX_INDEX + cube].centre
        ),
    )
    packed = 0
    for slot, cube in enumerate(furthest_first):
        packed |= cube << (DRAW_ORDER_BITS_PER_CUBE * slot)
    return packed


def _glsl_float(value: float) -> str:
    """A float GLSL will read as one — `repr` always spells a decimal point."""
    return repr(float(value))


def _glsl_vec3_array(name: str, rows: Sequence[Sequence[float]]) -> str:
    entries = ",\n    ".join(
        f"vec3({_glsl_float(x)}, {_glsl_float(y)}, {_glsl_float(z)})" for x, y, z in rows
    )
    return f"const vec3 {name}[{len(rows)}] = vec3[{len(rows)}](\n    {entries});\n\n"


def _glsl_int_array(name: str, values: Sequence[int]) -> str:
    entries = ", ".join(str(value) for value in values)
    return f"const int {name}[{len(values)}] = int[{len(values)}]({entries});\n\n"


# Every number the two renderers must agree on, as `#define`s so a shader
# reads them as literals. The defines are the only interpolated part: the
# function bodies below stay a plain string, so their braces need no doubling
# and they read as GLSL.
_SHOWCASE_SCENE_DEFINES = f"""\
#define CAMERA_ORBIT_RADIUS {_glsl_float(CAMERA_ORBIT_RADIUS)}
#define CAMERA_HEIGHT {_glsl_float(CAMERA_HEIGHT)}
#define CAMERA_LOOK_AT_HEIGHT {_glsl_float(CAMERA_LOOK_AT_HEIGHT)}
#define CAMERA_ORBIT_RADIANS_PER_SECOND {_glsl_float(CAMERA_ORBIT_RADIANS_PER_SECOND)}
#define CAMERA_FIELD_OF_VIEW_RADIANS {_glsl_float(CAMERA_FIELD_OF_VIEW_RADIANS)}
#define LIGHT_ORBIT_RADIUS {_glsl_float(LIGHT_ORBIT_RADIUS)}
#define LIGHT_HEIGHT {_glsl_float(LIGHT_HEIGHT)}
#define LIGHT_ORBIT_RADIANS_PER_SECOND {_glsl_float(LIGHT_ORBIT_RADIANS_PER_SECOND)}
#define FLOOR_MIRROR_STRENGTH {_glsl_float(FLOOR_MIRROR_STRENGTH)}
#define AMBIENT_LIGHT {_glsl_float(AMBIENT_LIGHT)}
#define BOX_EDGE_DARKENING {_glsl_float(BOX_EDGE_DARKENING)}
#define SHADOW_RAY_BIAS {_glsl_float(SHADOW_RAY_BIAS)}
#define NEAR_PLANE {_glsl_float(NEAR_PLANE)}
#define FAR_PLANE {_glsl_float(FAR_PLANE)}
#define SKY_BOX_INDEX {SKY_BOX_INDEX}
#define FLOOR_BOX_INDEX {FLOOR_BOX_INDEX}
#define FIRST_RING_CUBE_BOX_INDEX {FIRST_RING_CUBE_BOX_INDEX}
#define RING_CUBE_COUNT {RING_CUBE_COUNT}
#define DRAW_ORDER_BITS_PER_CUBE {DRAW_ORDER_BITS_PER_CUBE}
#define SKY_ZENITH_COLOUR vec3(0.05, 0.1, 0.4)
#define SKY_HORIZON_COLOUR vec3(0.85, 0.55, 0.35)

"""

_SHOWCASE_SCENE_FUNCTIONS = """
vec3 camera_position_at(float elapsed_seconds) {
    float azimuth = elapsed_seconds * CAMERA_ORBIT_RADIANS_PER_SECOND;
    return vec3(CAMERA_ORBIT_RADIUS * sin(azimuth),
                CAMERA_HEIGHT,
                CAMERA_ORBIT_RADIUS * cos(azimuth));
}

vec3 light_position_at(float elapsed_seconds) {
    float azimuth = elapsed_seconds * LIGHT_ORBIT_RADIANS_PER_SECOND;
    return vec3(LIGHT_ORBIT_RADIUS * sin(azimuth),
                LIGHT_HEIGHT,
                LIGHT_ORBIT_RADIUS * cos(azimuth));
}

// The one camera both sides look through. The ray generator turns a pixel
// into a ray with this basis; the vertex shader turns a point into a pixel
// with the same one, which is why the split lines up across the divider.
void showcase_camera_basis(float elapsed_seconds, out vec3 eye, out vec3 forward,
                           out vec3 right, out vec3 up) {
    eye = camera_position_at(elapsed_seconds);
    forward = normalize(vec3(0.0, CAMERA_LOOK_AT_HEIGHT, 0.0) - eye);
    right = normalize(cross(forward, vec3(0.0, 1.0, 0.0)));
    up = cross(right, forward);
}

vec3 sky_colour_towards(vec3 direction) {
    float height = clamp(direction.y * 0.5 + 0.5, 0.0, 1.0);
    return mix(SKY_HORIZON_COLOUR, SKY_ZENITH_COLOUR, height);
}

// The old showcase tinted its cube's edges with the hit's barycentrics. Same
// idea from the object-space position instead, because a rasterized fragment
// has no hit attributes and both halves have to draw the same lines.
float box_edge_darkening(vec3 in_unit_cube) {
    vec3 from_centre = abs(in_unit_cube);
    // On a face one axis is at 0.5 and the other two are inside it, so the
    // median of the three says how far across the face the point is.
    float across_the_face = max(min(from_centre.x, from_centre.y),
                                min(max(from_centre.x, from_centre.y), from_centre.z));
    return mix(1.0, BOX_EDGE_DARKENING, smoothstep(0.46, 0.5, across_the_face));
}

// One directional term, one ambient term, and a fraction saying how much of
// the light reaches this point. The rasterizer can only ever pass 1.0 — being
// able to answer that question is the whole of what the traced half adds.
vec3 directly_lit_colour(vec3 base_colour, vec3 world_position, vec3 world_normal,
                         float elapsed_seconds, float lit_fraction) {
    vec3 towards_light = normalize(light_position_at(elapsed_seconds) - world_position);
    float lambert = max(dot(world_normal, towards_light), 0.0);
    return base_colour * (AMBIENT_LIGHT + (1.0 - AMBIENT_LIGHT) * lambert * lit_fraction);
}

// The face of the unit cube a point sits on, as its outward normal.
vec3 unit_cube_face_normal(vec3 in_unit_cube) {
    vec3 from_centre = abs(in_unit_cube);
    if (from_centre.x > from_centre.y && from_centre.x > from_centre.z) {
        return vec3(sign(in_unit_cube.x), 0.0, 0.0);
    }
    if (from_centre.y > from_centre.z) {
        return vec3(0.0, sign(in_unit_cube.y), 0.0);
    }
    return vec3(0.0, 0.0, sign(in_unit_cube.z));
}
"""

SHOWCASE_SCENE_GLSL = (
    _SHOWCASE_SCENE_DEFINES
    + _glsl_vec3_array("SHOWCASE_BOX_CENTRES", [box.centre for box in SHOWCASE_BOXES])
    + _glsl_vec3_array("SHOWCASE_BOX_SIZES", [box.size for box in SHOWCASE_BOXES])
    + _glsl_vec3_array("SHOWCASE_BOX_COLOURS", [box.colour for box in SHOWCASE_BOXES])
    + _SHOWCASE_SCENE_FUNCTIONS
)

# Only the rasterizer reads the mesh: the ray tracer gets the same triangles
# through the acceleration structure it built them into, which is the point.
SHOWCASE_CUBE_MESH_GLSL = _glsl_vec3_array(
    "UNIT_CUBE_CORNERS",
    [
        UNIT_CUBE_CORNER_POSITIONS[corner * 3 : corner * 3 + 3]
        for corner in range(len(UNIT_CUBE_CORNER_POSITIONS) // 3)
    ],
) + _glsl_int_array("UNIT_CUBE_TRIANGLE_INDICES", UNIT_CUBE_TRIANGLE_INDICES) + _glsl_vec3_array(
    "UNIT_CUBE_FACE_NORMALS", UNIT_CUBE_FACE_NORMALS
)
