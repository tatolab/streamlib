# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The current: a GPU particle river riding the shelf edge north, with trails.

Importable as `nodes.gulf_stream_current:GulfStreamCurrent`. The particles live
in a storage buffer this node holds for its whole life; each frame one compute
kernel fades the trail texture and a second advects every particle through the
flow field and draws its streak into it.
"""

from __future__ import annotations

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
    WORLD_TEXTURE_SIZE,
    take_moment_at,
)

LAYER_FORMAT = "rgba16_float"

FADE_SLANG = """
[[vk::binding(0, 0)]] [format("rgba16f")] RWTexture2D<float4> trails;

struct FadeDials { float texture_size; float decay; };
[[vk::push_constant]] ConstantBuffer<FadeDials> dials;

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (any(float2(id.xy) >= dials.texture_size)) { return; }
    float3 kept = dials.decay > 0.0 ? trails[id.xy].rgb * dials.decay : float3(0.0, 0.0, 0.0);
    trails[id.xy] = float4(kept, 1.0);
}
"""

CURRENT_SLANG = WORLD_SLANG + """
[[vk::binding(0, 0)]] RWStructuredBuffer<float4> particles;
[[vk::binding(1, 0)]] [format("rgba16f")] RWTexture2D<float4> trails;

struct CurrentDials {
    float texture_size;
    float particle_count;
    float seconds;
    float step_seconds;
    float speed;
    float turbulence;
    float beat;
    float frame_index;
};
[[vk::push_constant]] ConstantBuffer<CurrentDials> dials;

float core_x(float y) { return shelf_edge_x(y) + 0.07; }

float stream_potential(float2 w) {
    return fbm(w * 2.6 + float2(0.0, -dials.seconds * 0.25), 4);
}

float2 flow_at(float2 w) {
    float d = (w.x - core_x(w.y)) / 0.13;
    float along = exp(-d * d);
    float slope = (core_x(w.y + 0.01) - core_x(w.y - 0.01)) / 0.02;
    float north = dials.speed * (0.06 + 0.5 * along);
    float2 flow = float2(north * slope, north);
    // Curl noise: divergence-free eddies, strongest at the stream's flanks.
    float e = 0.01;
    float dpdx = (stream_potential(w + float2(e, 0.0)) - stream_potential(w - float2(e, 0.0))) / (2.0 * e);
    float dpdy = (stream_potential(w + float2(0.0, e)) - stream_potential(w - float2(0.0, e))) / (2.0 * e);
    flow += float2(dpdy, -dpdx) * dials.turbulence * (0.025 + 0.05 * (1.0 - along));
    return flow;
}

float3 streak_colour(float along, float seed) {
    float3 c = lerp(MAGENTA, HOT_PINK, smoothstep(0.0, 0.45, along));
    c = lerp(c, CYAN, smoothstep(0.55, 0.95, along));
    return lerp(c, PEACH, step(0.97, seed));
}

float2 world_to_texel(float2 w) {
    return world_to_uv(w) * dials.texture_size;
}

[numthreads(64, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint index = id.x;
    if (float(index) >= dials.particle_count) { return; }
    float4 p = particles[index];
    float2 position = p.xy;
    float age = p.z;
    float life = p.w;

    bool outside = any(abs(position) > WORLD_HALF_EXTENT * 0.98);
    bool first_frame = dials.frame_index < 0.5;
    if (first_frame || !(life > 0.0) || age > life || outside) {
        float salt = float(index) * 0.618 + dials.frame_index * 1.37;
        float r1 = hash11(salt * 1.1 + 0.3);
        float r2 = hash11(salt * 2.3 + 1.7);
        float r3 = hash11(salt * 3.7 + 2.9);
        float r4 = hash11(salt * 5.3 + 4.1);
        position.y = -WORLD_HALF_EXTENT + 2.0 * WORLD_HALF_EXTENT * r1;
        position.x = core_x(position.y) + (r2 + r3 - 1.0) * 0.36;
        age = first_frame ? r4 * 3.0 : 0.0;
        life = 2.0 + 4.0 * r4;
    }

    float2 velocity = flow_at(position);
    float2 next = position + velocity * dials.step_seconds;
    age += dials.step_seconds;

    float d = (position.x - core_x(position.y)) / 0.13;
    float along = exp(-d * d);
    float seed = hash11(float(index) * 0.731);
    float fade = sin(3.14159 * saturate(age / life));
    float brightness = (0.018 + 0.05 * along) * fade * (1.0 + 1.6 * dials.beat * along);
    float3 colour = streak_colour(along, seed) * brightness;

    // The streak spans this step and the one before, so it reads as motion.
    float2 from_texel = world_to_texel(position - velocity * dials.step_seconds);
    float2 to_texel = world_to_texel(next);
    float span = length(to_texel - from_texel);
    int steps = clamp(int(ceil(span)), 1, 24);
    int size = int(dials.texture_size);
    for (int s = 0; s <= steps; s++) {
        int2 texel = int2(lerp(from_texel, to_texel, float(s) / float(steps)));
        if (all(texel >= int2(0, 0)) && all(texel < int2(size, size))) {
            trails[texel] = float4(trails[texel].rgb + colour, 1.0);
        }
    }
    particles[index] = float4(next, age, life);
}
"""

FADE_DIALS = struct.Struct("<ff")
CURRENT_DIALS = struct.Struct("<ffffffff")


def workgroups(items: int, size: int) -> int:
    return (items + size - 1) // size


@dataclass
class GulfStreamCurrentConfig:
    """GulfStreamCurrent's settings."""

    particle_count: int = 160_000
    trail_decay: float = 0.9
    turbulence: float = 1.0
    step_seconds: float = 1.0 / 30.0


@node.process
class GulfStreamCurrent:
    """Particles advected through a flow field along the shelf edge, with curl-noise eddies."""

    def __init__(self, config: GulfStreamCurrentConfig) -> None:
        self.config = config

    soundtrack = Input(Records, delivery="newest", description="From SoundtrackPulse")
    current_to_downstream = Output(Image, description="The particle trails in world space, HDR")

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.take_clock = TakeClock()
        gpu = ctx.gpu_full_access
        self.fade_kernel = gpu.create_compute_kernel(
            source=FADE_SLANG, push_constant_size=FADE_DIALS.size
        )
        self.current_kernel = gpu.create_compute_kernel(
            source=CURRENT_SLANG, push_constant_size=CURRENT_DIALS.size
        )
        self.particles = gpu.acquire_storage_buffer(
            (self.config.particle_count, 4), "float32"
        )
        self.trails = gpu.acquire_texture(
            WORLD_TEXTURE_SIZE,
            WORLD_TEXTURE_SIZE,
            LAYER_FORMAT,
            ["storage_binding", "texture_binding"],
        )
        self.output_textures = NodeOutputTextureRing(
            LAYER_FORMAT, ["storage_binding", "texture_binding"]
        )
        self.frame_index = 0
        self.latest_soundtrack: dict = {}

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        soundtrack = ctx.inputs.read(self.soundtrack)
        if soundtrack is not None:
            self.latest_soundtrack = soundtrack
        seconds = self.take_clock.seconds_at(ctx)
        moment = take_moment_at(seconds)
        beat = float(self.latest_soundtrack.get("beat", 0.0))
        energy = float(self.latest_soundtrack.get("energy", 0.0))
        speed = moment.current_speed * (1.0 + 0.35 * energy)
        size = WORLD_TEXTURE_SIZE
        gpu = ctx.gpu_limited_access
        self.fade_kernel.dispatch(
            bindings={"trails": self.trails},
            group_count=(workgroups(size, 8), workgroups(size, 8), 1),
            push_constants=FADE_DIALS.pack(
                size, self.config.trail_decay if self.frame_index else 0.0
            ),
        )
        self.current_kernel.dispatch(
            bindings={"particles": self.particles, "trails": self.trails},
            group_count=(workgroups(self.config.particle_count, 64), 1, 1),
            push_constants=CURRENT_DIALS.pack(
                size,
                self.config.particle_count,
                seconds,
                self.config.step_seconds,
                speed,
                self.config.turbulence,
                beat,
                self.frame_index,
            ),
        )
        self.frame_index += 1
        texture = self.output_textures.next_texture_for_this_frame(gpu, size, size)
        gpu.copy_surface_to_surface(self.trails.surface_id, texture)
        ctx.outputs.write(
            self.current_to_downstream,
            ImageFrameToWrite.of_texture(texture, RENDERED_FRAME_COLOUR),
            timestamp_ns=ctx.time,
        )
