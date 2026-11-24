# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`PaintBall`: a ball of about 38,000 small cubes that scatters and gathers with the music.

It draws from the `BallCamera` `OrbitCamera` makes out of the stage view's drag and zoom,
and a burst from the stage throws every band outward at once.

Every cube sits on a lattice cell inside a sphere and belongs to one of 24 bands, low bands at
the bottom and high bands at the top along a twisting spiral. Each band's clock (from
`SpectrumListener`) says how far its cubes have flown out; the flight, swirl, spin and colour
are all worked out in the vertex stage from the cube's cell, so nothing is stored per cube.

The runtime's graphics kernels have no depth buffer yet. The cubes are drawn back to front
instead: the lattice is walked from the far side to the near side on each axis, and each cube
draws only the faces that face the camera.
"""

from __future__ import annotations

import math
import struct
from typing import Annotated

import numpy
from tatolab.stream import (
    ColorInfo,
    Description,
    TextureToWrite,
    InputPin,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    node,
)

from nodes.motion import BAND_COUNT, SILENCE, BallCamera, MusicMotion

GRID_HALF = 20
GRID_SIDE = 2 * GRID_HALF + 1
CUBE_VERTEX_COUNT = 36
TEXTURE_FORMAT = "rgba8_unorm"
RENDER_ATTACHMENT_AND_SAMPLED = ["render_attachment", "texture_binding"]
RENDERED_FRAME_COLOUR = ColorInfo(primaries="bt709", transfer="srgb", range="full")
NANOSECONDS_PER_SECOND = 1_000_000_000

# time, yaw, pitch, aspect, beat, flow, burst, distance; then 24 (clock, level) half pairs.
FRAME_PUSH_CONSTANT_FORMAT = "<8f24I"
FRAME_PUSH_CONSTANT_SIZE = struct.calcsize(FRAME_PUSH_CONSTANT_FORMAT)
BURST_SECONDS = 1.4
# The camera rides a spring that each explosion throws outward: a stiff spring out, so it is
# thrown back within a few frames, then a soft one that glides it home after a short hold.
PULLBACK_FROM_SCATTER = 0.04
PULLBACK_FULL_AT_SCATTER = 0.45
THROW_PER_SCATTER_JUMP = 9.0

_SHARED_SLANG = f"""
static const int BAND_COUNT = {BAND_COUNT};
static const int GRID_HALF = {GRID_HALF};
static const int GRID_SIDE = {GRID_SIDE};
static const float CELL = 0.105;
static const float BALL_RADIUS = 2.1;
static const float NEAR_PLANE = 0.1;
static const float FAR_PLANE = 100.0;
static const float HALF_FIELD_OF_VIEW_TAN = 0.3839;

struct BallFrame {{
    float time;
    float yaw;
    float pitch;
    float aspect;
    float beat;
    float flow;
    float burst;
    float distance;
    uint4 bands[6];
}};
[[vk::push_constant]] ConstantBuffer<BallFrame> frame;

struct CubeVertexToFragment {{
    float4 clip_position : SV_Position;
    float3 world_normal : WORLD_NORMAL;
    float3 base_colour : BASE_COLOUR;
}};
"""

PAINT_BALL_VERTEX_SLANG = (
    _SHARED_SLANG
    + """
static const float3 CORNERS[8] = {
    float3(-0.5, -0.5, -0.5), float3(0.5, -0.5, -0.5), float3(0.5, 0.5, -0.5), float3(-0.5, 0.5, -0.5),
    float3(-0.5, -0.5, 0.5), float3(0.5, -0.5, 0.5), float3(0.5, 0.5, 0.5), float3(-0.5, 0.5, 0.5)
};
// Six faces, two triangles each, in the order of FACE_NORMALS.
static const int FACE_CORNERS[36] = {
    1, 2, 6, 1, 6, 5,   // +x
    0, 4, 7, 0, 7, 3,   // -x
    3, 7, 6, 3, 6, 2,   // +y
    0, 1, 5, 0, 5, 4,   // -y
    4, 5, 6, 4, 6, 7,   // +z
    0, 3, 2, 0, 2, 1    // -z
};
static const float3 FACE_NORMALS[6] = {
    float3(1, 0, 0), float3(-1, 0, 0), float3(0, 1, 0), float3(0, -1, 0), float3(0, 0, 1), float3(0, 0, -1)
};

float hash31(float3 p) {
    p = frac(p * 0.1031);
    p += dot(p, p.zyx + 31.32);
    return frac((p.x + p.y) * p.z);
}

float2 band_clock_and_level(int band) {
    uint word = frame.bands[band / 4][band % 4];
    return float2(f16tof32(word & 0xFFFFu), f16tof32(word >> 16));
}

float3 eye_position() {
    float cp = cos(frame.pitch);
    return frame.distance * float3(sin(frame.yaw) * cp, sin(frame.pitch), cos(frame.yaw) * cp);
}

// The colour of a point on the spectrum, 0 for the lowest band to 1 for the highest:
// crimson, orange, gold, a quiet olive, teal, blue, violet.
float3 spectrum_colour(float u) {
    float3 stops[7] = {
        float3(0.70, 0.07, 0.12), float3(0.93, 0.33, 0.09), float3(0.96, 0.70, 0.17),
        float3(0.46, 0.60, 0.28), float3(0.17, 0.56, 0.60), float3(0.20, 0.33, 0.86),
        float3(0.56, 0.28, 0.86)
    };
    float positions[7] = { 0.0, 0.18, 0.36, 0.50, 0.62, 0.80, 1.0 };
    float3 colour = stops[6];
    for (int i = 1; i < 7; i++) {
        if (u <= positions[i]) {
            colour = lerp(stops[i - 1], stops[i], (u - positions[i - 1]) / (positions[i] - positions[i - 1]));
            break;
        }
    }
    return colour;
}

float3 rotate_about(float3 v, float3 axis, float angle) {
    float c = cos(angle), s = sin(angle);
    return v * c + cross(axis, v) * s + axis * dot(axis, v) * (1.0 - c);
}

int lattice_coordinate(int index_along_axis, float eye_along_axis) {
    // Far side first: the cells away from the eye are drawn before the cells near it.
    return eye_along_axis > 0.0 ? index_along_axis - GRID_HALF : GRID_HALF - index_along_axis;
}

CubeVertexToFragment collapsed() {
    CubeVertexToFragment output;
    output.clip_position = float4(0.0, 0.0, -1.0, 1.0);
    output.world_normal = float3(0.0, 1.0, 0.0);
    output.base_colour = float3(0.0, 0.0, 0.0);
    return output;
}

CubeVertexToFragment main(uint vertex_index : SV_VulkanVertexID, uint instance_index : SV_VulkanInstanceID) {
    float3 eye = eye_position();
    int instance = int(instance_index);
    int3 order = int3(instance / (GRID_SIDE * GRID_SIDE), (instance / GRID_SIDE) % GRID_SIDE, instance % GRID_SIDE);
    int3 cell = int3(lattice_coordinate(order.x, eye.x), lattice_coordinate(order.y, eye.y), lattice_coordinate(order.z, eye.z));

    float3 home = float3(cell) * CELL;
    float r = length(home);
    if (r > BALL_RADIUS) {
        return collapsed();
    }
    float f = r / BALL_RADIUS;
    float3 n = r > 1e-4 ? home / r : float3(0.0, 1.0, 0.0);
    float3 cell_f = float3(cell);
    float h1 = hash31(cell_f + 0.17), h2 = hash31(cell_f + 3.71), h3 = hash31(cell_f + 7.13);
    float h4 = hash31(cell_f + 11.9), h5 = hash31(cell_f + 19.3);

    // Which band: height, twisted along a spiral and a wobble, so bands lean into each other.
    float lon = atan2(home.z, home.x);
    float yy = clamp(home.y / BALL_RADIUS + 0.24 * sin(2.0 * lon + 3.0 * home.y / BALL_RADIUS)
                     + 0.06 * sin(home.x * 2.1 + home.z * 1.9), -1.0, 1.0);
    float u = saturate(0.5 + 0.72 * yy - 0.22 * yy * yy * yy);
    int band = clamp(int(floor(u * BAND_COUNT + (h1 - 0.5) * 1.6)), 0, BAND_COUNT - 1);
    float2 clock_and_level = band_clock_and_level(band);

    // The flight: the skin leaves first and the core last, outward with a swirl about the y axis.
    float launch = (1.0 - f * f * f) * 1.25 + 0.15 * h2;
    float local = max(0.0, clock_and_level.x + frame.burst * 3.2 * (0.7 + 0.6 * h4) - launch);
    float speed = (0.75 + 1.15 * h3) * (0.6 + 0.5 * f);
    float3 tangent = float3(-n.z, 0.0, n.x);
    float3 velocity = n * speed + tangent * 0.45 * (0.4 + h4) + (float3(h5, h1, h4) - 0.5) * 0.3;
    float travel = (1.0 - exp(-0.9 * local)) / 0.9 * 3.2;
    float3 centre = home + velocity * travel;

    float swirl = frame.flow * 0.6 * local * sin(float(band) * 0.55 + 0.6);
    centre = float3(centre.x * cos(swirl) - centre.z * sin(swirl), centre.y, centre.x * sin(swirl) + centre.z * cos(swirl));
    if (local > 0.0) {
        float k = frame.flow * smoothstep(0.0, 1.8, local);
        float t = frame.time;
        float3 q = centre * 0.42;
        float3 drift = float3(sin(q.y * 1.9 + t * 0.9) + sin(q.z * 1.3 - t * 0.6),
                              sin(q.z * 1.7 + t * 0.7) + sin(q.x * 1.1 + t * 0.8),
                              sin(q.x * 1.5 - t * 0.8) + sin(q.y * 1.2 + t * 0.5));
        centre += k * 0.5 * drift;
    }

    // Breathing: each band's level pushes its skin out and makes it shiver; the beat pumps the ball.
    float skin = smoothstep(1.2, 2.1, r);
    float shiver = 0.35 * sin(frame.time * 9.0 + dot(n, float3(5.0, 3.0, 4.0))) + 0.25 * sin(frame.time * 4.0 + home.y * 3.0);
    centre += n * skin * (0.26 * clock_and_level.y * (1.0 + shiver) + 0.2 * frame.beat);

    // The cube itself: spun as it flies, a little smaller once it is well out.
    int face = int(vertex_index) / 6;
    float3 spin_axis = normalize(float3(h1, h2, h3) - 0.5 + 1e-3);
    float spin = (h4 * 2.0 - 1.0) * 4.0 * local;
    float size = CELL * 0.94 * (1.0 - 0.3 * smoothstep(0.0, 3.0, local));
    float3 normal = rotate_about(FACE_NORMALS[face], spin_axis, spin);
    float3 face_centre = centre + normal * 0.5 * size;
    if (dot(normal, eye - face_centre) <= 0.0) {
        return collapsed();
    }
    float3 corner = rotate_about(CORNERS[FACE_CORNERS[vertex_index]], spin_axis, spin);
    float3 world_position = centre + corner * size;

    float3 forward = normalize(-eye);
    float3 right = normalize(cross(forward, float3(0.0, 1.0, 0.0)));
    float3 up = cross(right, forward);
    float3 from_eye = world_position - eye;
    float3 in_view = float3(dot(from_eye, right), dot(from_eye, up), dot(from_eye, forward));

    CubeVertexToFragment output;
    output.clip_position = float4(in_view.x / (HALF_FIELD_OF_VIEW_TAN * frame.aspect),
                                  -in_view.y / HALF_FIELD_OF_VIEW_TAN,
                                  FAR_PLANE * (in_view.z - NEAR_PLANE) / (FAR_PLANE - NEAR_PLANE),
                                  in_view.z);
    output.world_normal = normal;
    float hue_jitter = (h2 - 0.5) * 0.03;
    float3 base = spectrum_colour(saturate(u + hue_jitter));
    output.base_colour = base * (0.72 + 0.25 * (1.0 - f)) * (0.92 + 0.16 * h5);
    return output;
}
"""
)

PAINT_BALL_FRAGMENT_SLANG = (
    _SHARED_SLANG
    + """
float3 filmic(float3 x) {
    // A compact filmic curve: soft shoulder, slightly lifted toe.
    float3 a = x * (2.51 * x + 0.03);
    float3 b = x * (2.43 * x + 0.59) + 0.14;
    return saturate(a / b);
}

float4 main(CubeVertexToFragment fragment) : SV_Target0 {
    float3 n = normalize(fragment.world_normal);
    float3 sky = float3(0.75, 0.81, 1.0);
    float3 ground = float3(0.23, 0.17, 0.28);
    float3 hemisphere = lerp(ground, sky, n.y * 0.5 + 0.5) * 0.4;
    float3 key = float3(1.0, 0.85, 0.66) * 1.15 * max(0.0, dot(n, normalize(float3(-6.0, 7.0, 6.0))));
    float3 rim = float3(0.56, 0.65, 1.0) * 0.45 * max(0.0, dot(n, normalize(float3(7.0, -2.0, -6.0))));
    float3 lit = fragment.base_colour * (hemisphere + key + rim);
    float3 encoded = pow(filmic(lit), 1.0 / 2.2);
    return float4(encoded, 1.0);
}
"""
)


def half_pair_word(clock: float, level: float) -> int:
    halves = numpy.array([clock, level], dtype=numpy.float16).view(numpy.uint16)
    return int(halves[0]) | (int(halves[1]) << 16)


@node.meta(
    category="Paint Pulse",
    description="A ball of small cubes, coloured by the spectrum, that scatters and gathers with each band of the music.",
)
@node(execution="continuous", interval_ms=16)
class PaintBall:
    """Motion in, a rendered frame of the ball out (transparent where there is no ball)."""

    width: Annotated[int, NumberRange(minimum=64), Description("The frame's width in pixels.")] = 1280
    height: Annotated[int, NumberRange(minimum=64), Description("The frame's height in pixels.")] = 720
    flow: Annotated[bool, Description("Whether flown cubes drift on a flow field.")] = True
    pullback: Annotated[
        float, NumberRange(minimum=1.0), Description("How far an explosion throws the camera back.")
    ] = 2.6
    push_out: Annotated[
        float, NumberRange(minimum=1.0), Description("How stiffly the camera is thrown back.")
    ] = 60.0
    spring_back: Annotated[
        float, NumberRange(minimum=0.5), Description("How stiffly the camera glides home.")
    ] = 5.0
    hold: Annotated[
        float, NumberRange(minimum=0.0), Description("How long the camera's reach holds.")
    ] = 0.35
    motion: InputPin[
        Annotated[MusicMotion, StreamDelivery("newest"), Description("From SpectrumListener")]
    ]
    camera: InputPin[
        Annotated[BallCamera, StreamDelivery("newest"), Description("From OrbitCamera")]
    ]
    ball: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(TEXTURE_FORMAT),
            Description("The ball, on a transparent background"),
        ]
    ]

    def __init__(self) -> None:
        self.latest_motion = SILENCE
        self.latest_camera = BallCamera()
        self.bursts_seen: int | None = None
        self.burst = 0.0
        self.camera_pullback = 1.0
        self.pullback_speed = 0.0
        self.held_scatter = 0.0
        self.last_scatter = 0.0
        self.last_time_ns: int | None = None

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.output_ring = NodeOutputTextureRing(TEXTURE_FORMAT, RENDER_ATTACHMENT_AND_SAMPLED)
        self.kernel = ctx.gpu_full_access.create_graphics_kernel(
            color_attachment_formats=[TEXTURE_FORMAT],
            vertex_source=PAINT_BALL_VERTEX_SLANG,
            fragment_source=PAINT_BALL_FRAGMENT_SLANG,
            push_constant_size=FRAME_PUSH_CONSTANT_SIZE,
            cull_mode="none",
            label="paint-ball",
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        motion = self.motion.read()
        if motion is not None:
            self.latest_motion = motion

        now_ns = ctx.time
        dt = 0.0 if self.last_time_ns is None else (now_ns - self.last_time_ns) / NANOSECONDS_PER_SECOND
        self.last_time_ns = now_ns
        camera = self.camera.read()
        if camera is not None:
            self.latest_camera = camera
        bursts = self.latest_camera.burst
        if self.bursts_seen is not None and bursts != self.bursts_seen:
            self.burst = 1.0
        self.bursts_seen = bursts
        self.burst = max(0.0, self.burst - min(dt, 0.1) / BURST_SECONDS)
        yaw = self.latest_camera.yaw % math.tau
        pitch = self.latest_camera.pitch
        distance = max(4.0, self.latest_camera.distance)

        clocks = list(self.latest_motion.clocks)[:BAND_COUNT]
        levels = list(self.latest_motion.levels)[:BAND_COUNT]
        beat = self.latest_motion.beat
        words = [half_pair_word(c, lv) for c, lv in zip(clocks, levels)]

        # The camera is blown back with the cubes: a jump in how far the widest bands have flown
        # throws it outward on a stiff spring, the reach holds a moment, and a soft spring glides
        # it home, so the move reads as the camera's own.
        widest = sorted(clocks, reverse=True)[:6]
        scatter = max(sum(widest) / (len(widest) * 3.0), self.burst)
        step = min(dt, 0.05)
        # The camera spring: how far it is thrown, how hard, how softly it comes home and how long
        # the reach holds. Each damping is set near critical for its stiffness.
        spring_out = (self.push_out, 1.4 * self.push_out**0.5)
        spring_in = (self.spring_back, 1.6 * self.spring_back**0.5)
        hold_falls_per_second = 1.0 / max(0.05, self.hold * 8.0)
        self.held_scatter = max(scatter, self.held_scatter - hold_falls_per_second * step)
        reach = min(1.0, max(0.0, (self.held_scatter - PULLBACK_FROM_SCATTER) / (PULLBACK_FULL_AT_SCATTER - PULLBACK_FROM_SCATTER)))
        target = 1.0 + (self.pullback - 1.0) * reach * reach * (3.0 - 2.0 * reach)
        jump = scatter - self.last_scatter
        self.last_scatter = scatter
        if jump > 0.02:
            self.pullback_speed += THROW_PER_SCATTER_JUMP * jump
        stiffness, damping = spring_out if target > self.camera_pullback else spring_in
        self.pullback_speed += (stiffness * (target - self.camera_pullback) - damping * self.pullback_speed) * step
        self.camera_pullback = min(
            self.pullback + 0.4, max(0.85, self.camera_pullback + self.pullback_speed * step)
        )
        distance *= self.camera_pullback

        texture = self.output_ring.next_texture_for_this_frame(ctx.gpu_limited_access, self.width, self.height)
        self.kernel.draw(
            bindings={},
            color_targets=[texture],
            extent=(self.width, self.height),
            vertex_count=CUBE_VERTEX_COUNT,
            instance_count=GRID_SIDE**3,
            push_constants=struct.pack(
                FRAME_PUSH_CONSTANT_FORMAT,
                (now_ns / NANOSECONDS_PER_SECOND) % 1000.0,
                yaw,
                pitch,
                self.width / self.height,
                beat,
                1.0 if self.flow else 0.0,
                self.burst * self.burst * (3.0 - 2.0 * self.burst),
                distance,
                *words,
            ),
        )
        self.ball.write(
            TextureToWrite.of_texture(texture, RENDERED_FRAME_COLOUR),
            timestamp_ns=now_ns,
        )
