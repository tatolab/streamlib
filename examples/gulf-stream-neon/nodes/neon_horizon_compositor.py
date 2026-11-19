# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""One camera flown over both world layers, under a sky that sets.

Importable as `nodes.neon_horizon_compositor:NeonHorizonCompositor`. A single
compute kernel casts a ray per pixel: below the horizon it lands on the sea
plane and samples the seafloor and the current there, with a bloom that widens
with distance; above it, the sky runs golden hour to night with a striped
sunset sun. The take's camera and time of day come from `take_moment_at`.
"""

from __future__ import annotations

import math
import struct
from dataclasses import dataclass

from tatolab.stream import (
    Image,
    ImageFrameToWrite,
    Input,
    NodeOutputTextureRing,
    Output,
    Records,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    node,
)

from nodes.neon_world import (
    TakeClock,
    NANOSECONDS_PER_SECOND,
    RENDERED_FRAME_COLOUR,
    WORLD_SLANG,
    take_moment_at,
)

HORIZON_SLANG = WORLD_SLANG + """
[[vk::binding(0, 0)]] Sampler2D contour_layer;
[[vk::binding(1, 0)]] Sampler2D current_layer;
[[vk::binding(2, 0)]] [format("rgba8")] RWTexture2D<float4> program;

struct HorizonDials {
    float2 frame_size;
    float seconds;
    float time_of_day;
    float3 camera_position;
    float focal;
    float3 forward;
    float glow;
    float3 right;
    float beat;
    float3 up;
    float exposure;
};
[[vk::push_constant]] ConstantBuffer<HorizonDials> dials;

static const float2 GOLDEN_TAPS[12] = {
    float2(1.0, 0.0), float2(-0.737, 0.676), float2(0.087, -0.996), float2(0.608, 0.794),
    float2(-0.984, -0.174), float2(0.819, -0.574), float2(-0.259, 0.966), float2(-0.5, -0.866),
    float2(0.966, 0.259), float2(-0.866, 0.5), float2(0.342, -0.94), float2(0.0, 1.0)
};

float3 sky_colour(float3 direction, bool with_sun) {
    float elevation = direction.z;
    float tod = dials.time_of_day;
    float3 zenith = lerp(lerp(float3(0.20, 0.12, 0.42), float3(0.13, 0.04, 0.27), smoothstep(0.0, 0.5, tod)),
                         NIGHT * 0.55, smoothstep(0.5, 1.0, tod));
    float3 middle = lerp(lerp(CORAL, MAGENTA * 0.9, smoothstep(0.0, 0.55, tod)),
                         float3(0.22, 0.05, 0.36), smoothstep(0.55, 1.0, tod));
    float3 horizon = lerp(lerp(PEACH * 1.25, HOT_PINK * 1.1, smoothstep(0.0, 0.5, tod)),
                          MAGENTA * 0.55, smoothstep(0.5, 1.0, tod));
    float3 colour = lerp(horizon, middle, smoothstep(0.0, 0.12, elevation));
    colour = lerp(colour, zenith, smoothstep(0.1, 0.5, elevation));

    // Stars come out after the sun goes.
    float2 star_cell = floor(float2(atan2(direction.x, direction.y) * 260.0, elevation * 420.0));
    float star = step(0.9965, hash21(star_cell)) * smoothstep(0.55, 1.0, tod) * smoothstep(0.03, 0.2, elevation);
    star *= 0.6 + 0.4 * sin(dials.seconds * 4.0 + hash21(star_cell + 5.0) * 30.0);
    colour += float3(1.0, 0.85, 0.95) * star * 1.5;

    // The sun sets in the north-west, a striped disc sinking into the haze.
    float sun_elevation = lerp(0.16, -0.2, smoothstep(0.0, 0.95, tod));
    float3 sun_direction = normalize(float3(-0.16, 0.99, sun_elevation));
    float3 flattened = normalize(float3(direction.x, direction.y, 0.0));
    float3 sun_flat = normalize(float3(sun_direction.x, sun_direction.y, 0.0));
    float azimuth_offset = acos(clamp(dot(flattened, sun_flat), -1.0, 1.0));
    float2 on_disc = float2(azimuth_offset, elevation - sun_elevation) / 0.13;
    float disc = 1.0 - smoothstep(0.96, 1.0, length(on_disc));
    float stripes = (on_disc.y < 0.15) ? step(0.38 + 0.25 * (0.15 - on_disc.y), frac(on_disc.y * 5.5 + dials.seconds * 0.12)) : 1.0;
    float3 sun_colour = lerp(HOT_PINK * 1.9, PEACH * 1.9, saturate(on_disc.y * 0.6 + 0.5));
    if (!with_sun) { return colour + HOT_PINK * exp(-length(on_disc) * 1.4) * 0.3; }
    colour = lerp(colour, sun_colour, disc * stripes * step(0.0, elevation));
    float halo = exp(-length(on_disc) * 1.4) * (1.0 - 0.5 * smoothstep(0.6, 1.0, tod));
    colour += HOT_PINK * halo * 0.55;
    return colour;
}

float3 layer_at(float2 uv, float footprint) {
    float3 sharp = contour_layer.SampleLevel(uv, 0.0).rgb + current_layer.SampleLevel(uv, 0.0).rgb;
    float3 soft = float3(0.0, 0.0, 0.0);
    float3 wide = float3(0.0, 0.0, 0.0);
    float near_radius = max(footprint * 0.6, 0.0006);
    float far_radius = max(footprint * 2.5, 0.0035);
    for (int tap = 0; tap < 12; tap++) {
        float2 near_uv = uv + GOLDEN_TAPS[tap] * near_radius * (0.5 + 0.5 * float(tap % 3));
        float2 far_uv = uv + GOLDEN_TAPS[tap] * far_radius * (0.4 + 0.3 * float(tap % 3));
        soft += contour_layer.SampleLevel(near_uv, 0.0).rgb + current_layer.SampleLevel(near_uv, 0.0).rgb;
        wide += contour_layer.SampleLevel(far_uv, 0.0).rgb + 1.6 * current_layer.SampleLevel(far_uv, 0.0).rgb;
    }
    soft /= 12.0;
    wide /= 12.0;
    // Far away a texel is smaller than a pixel: lean on the filtered taps.
    float filtered = saturate(footprint / 0.0012);
    float3 base = lerp(sharp, soft, filtered * 0.85);
    return base + wide * (0.35 + 0.5 * dials.beat) * dials.glow;
}

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (any(float2(id.xy) >= dials.frame_size)) { return; }
    float2 pixel = float2(id.xy) + 0.5;
    float2 ndc = (pixel - 0.5 * dials.frame_size) / (0.5 * dials.frame_size.y);
    ndc.y = -ndc.y;
    float3 direction = normalize(dials.forward * dials.focal + dials.right * ndc.x + dials.up * ndc.y);
    float pixel_angle = 2.0 / (dials.frame_size.y * dials.focal);

    float3 colour;
    if (direction.z < -0.0005) {
        float distance = dials.camera_position.z / -direction.z;
        float2 hit = dials.camera_position.xy + direction.xy * distance;
        float footprint = distance * pixel_angle / max(-direction.z, 0.04) / (2.0 * WORLD_HALF_EXTENT);
        float inside = 1.0 - smoothstep(WORLD_HALF_EXTENT * 0.86, WORLD_HALF_EXTENT, max(abs(hit.x), abs(hit.y)));
        float3 ground = layer_at(world_to_uv(hit), footprint) * inside * (1.0 + 0.35 * dials.time_of_day);
        // A glossy sea: it mirrors the sky at a grazing angle.
        float fresnel = pow(1.0 - saturate(-direction.z), 6.0);
        float3 mirrored = sky_colour(float3(direction.xy, -direction.z), false);
        float haze = 1.0 - exp(-distance * 0.16);
        colour = lerp(ground, mirrored * 0.75, saturate(fresnel * 0.85 + haze * 0.55));
        colour = lerp(colour, sky_colour(float3(direction.xy, 0.0), false), smoothstep(0.65, 1.0, haze) * 0.8);
        // The setting sun's glint: a broken column of light on the water.
        float sun_elevation = lerp(0.16, -0.2, smoothstep(0.0, 0.95, dials.time_of_day));
        float2 sun_flat = normalize(float2(-0.16, 0.99));
        float glint_offset = abs(dot(normalize(direction.xy), float2(sun_flat.y, -sun_flat.x)));
        float ripple = step(0.45, value_noise(float2(hit.x * 40.0, hit.y * 4.0 + dials.seconds * 0.6)));
        float glint = exp(-glint_offset * glint_offset * 900.0) * ripple * smoothstep(-0.05, 0.1, sun_elevation)
                      * smoothstep(0.35, 0.0, -direction.z);
        colour += lerp(HOT_PINK, PEACH, 0.4) * glint * 0.9;
    } else {
        colour = sky_colour(direction, true);
    }

    // Film finish: tone curve, vignette, scanlines and grain.
    colour = 1.0 - exp(-colour * dials.exposure);
    float2 centred = pixel / dials.frame_size - 0.5;
    colour *= 1.0 - 0.45 * dot(centred, centred);
    colour *= 0.97 + 0.03 * sin(pixel.y * 3.14159 * 0.5);
    float grain = hash21(pixel + frac(dials.seconds * 7.13) * 917.0) - 0.5;
    colour += grain * 0.035;
    program[id.xy] = float4(saturate(colour), 1.0);
}
"""

HORIZON_DIALS = struct.Struct("<ff ff 3ff 3ff 3ff 3ff")


@dataclass
class NeonHorizonCompositorConfig:
    """NeonHorizonCompositor's settings: the program frame's extent and lens."""

    width: int = 1080
    height: int = 1920
    vertical_field_of_view_degrees: float = 62.0
    exposure: float = 1.15


def camera_basis(yaw_degrees: float, pitch_degrees: float, roll_degrees: float = 0.0):
    yaw = math.radians(yaw_degrees)
    pitch = math.radians(pitch_degrees)
    forward = (math.sin(yaw) * math.cos(pitch), math.cos(yaw) * math.cos(pitch), -math.sin(pitch))
    right = (math.cos(yaw), -math.sin(yaw), 0.0)
    up = (
        right[1] * forward[2] - right[2] * forward[1],
        right[2] * forward[0] - right[0] * forward[2],
        right[0] * forward[1] - right[1] * forward[0],
    )
    roll = math.radians(roll_degrees)
    c, s = math.cos(roll), math.sin(roll)
    rolled_right = tuple(r * c + u * s for r, u in zip(right, up))
    rolled_up = tuple(u * c - r * s for r, u in zip(right, up))
    return forward, rolled_right, rolled_up


@node.process
class NeonHorizonCompositor:
    """The program picture: the sea plane in perspective, bloom, sky, sun and film finish."""

    def __init__(self, config: NeonHorizonCompositorConfig) -> None:
        self.config = config

    contours_from_upstream = Input(Image, delivery="newest", description="From SeafloorRelief")
    current_from_upstream = Input(Image, delivery="newest", description="From GulfStreamCurrent")
    soundtrack = Input(Records, delivery="newest", description="From SoundtrackPulse")
    program_to_downstream = Output(Image, description="The finished picture")

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.take_clock = TakeClock()
        self.horizon_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=HORIZON_SLANG, push_constant_size=HORIZON_DIALS.size
        )
        self.program_textures = NodeOutputTextureRing(
            "rgba8_unorm", ["storage_binding", "texture_binding"]
        )
        self.focal = 1.0 / math.tan(math.radians(self.config.vertical_field_of_view_degrees) / 2.0)
        self.latest_contours = None
        self.latest_current = None
        self.latest_soundtrack: dict = {}
        self.last_rendered_ns: int | None = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        contours = ctx.inputs.read(self.contours_from_upstream)
        if contours is not None:
            self.latest_contours = contours
        current = ctx.inputs.read(self.current_from_upstream)
        if current is not None:
            self.latest_current = current
        soundtrack = ctx.inputs.read(self.soundtrack)
        if soundtrack is not None:
            self.latest_soundtrack = soundtrack
        if self.latest_contours is None or self.latest_current is None:
            return
        frame_ns = self.latest_current.timestamp_ns
        if frame_ns == self.last_rendered_ns or self.latest_contours.timestamp_ns != frame_ns:
            return
        self.last_rendered_ns = frame_ns

        seconds = self.take_clock.seconds_at(ctx, frame_ns)
        moment = take_moment_at(seconds)
        forward, right, up = camera_basis(
            moment.camera_yaw_degrees, moment.camera_pitch_degrees, moment.camera_roll_degrees
        )
        beat = float(self.latest_soundtrack.get("beat", 0.0))
        width, height = self.config.width, self.config.height
        gpu = ctx.gpu_limited_access
        texture = self.program_textures.next_texture_for_this_frame(gpu, width, height)
        self.horizon_kernel.dispatch(
            bindings={
                "contour_layer": self.latest_contours.surface_id,
                "current_layer": self.latest_current.surface_id,
                "program": texture,
            },
            group_count=((width + 7) // 8, (height + 7) // 8, 1),
            push_constants=HORIZON_DIALS.pack(
                width,
                height,
                seconds,
                moment.time_of_day,
                moment.camera_x,
                moment.camera_y,
                moment.camera_altitude,
                self.focal,
                *forward,
                moment.glow,
                *right,
                beat,
                *up,
                self.config.exposure,
            ),
        )
        ctx.outputs.write(
            self.program_to_downstream,
            ImageFrameToWrite.of_texture(texture, RENDERED_FRAME_COLOUR),
            timestamp_ns=frame_ns,
        )
