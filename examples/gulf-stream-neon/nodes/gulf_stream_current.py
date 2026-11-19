# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The current: a million GPU particles riding the real Gulf Stream north, with trails.

Importable as `nodes.gulf_stream_current:GulfStreamCurrent`. `setup()` uploads
a HYCOM surface-current snapshot of the map's square into a held texture; the
particles live in a storage buffer this node holds for its whole life. Each
frame one kernel fades the trail texture and a second advects every particle
through the real flow, time-lapsed, with curl-noise eddies below the model's
grid and the station's wind as a surface drift, and draws its streak.
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
    log,
    node,
)

from nodes.neon_world import (
    HDR_LAYER_FORMAT,
    METRES_PER_WORLD_UNIT,
    RENDERED_FRAME_COLOUR,
    WORLD_SLANG,
    TakeClock,
    take_moment_at,
)
from nodes.ocean_data import baked_currents

# The trails are drawn finer than the seafloor: the camera skims them at a few hundred metres.
WORLD_TEXTURE_SIZE = 4096

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
[[vk::binding(2, 0)]] Sampler2D field;

struct CurrentDials {
    float texture_size;
    float particle_count;
    float seconds;
    float step_seconds;
    float speed;
    float turbulence;
    float beat;
    float frame_index;
    float world_units_per_metre_second;
    float wind_east;
    float wind_north;
    float brightness;
};
[[vk::push_constant]] ConstantBuffer<CurrentDials> dials;

float4 field_at(float2 w) {
    return field.SampleLevel(world_to_uv(w), 0.0);
}

float stream_potential(float2 w) {
    return fbm(w * 7.0 + float2(0.0, -dials.seconds * 0.35), 4);
}

// The real current, time-lapsed into world units a second, plus eddies and the wind's drift.
float2 flow_at(float2 w, float4 here) {
    float2 water = here.xy * dials.world_units_per_metre_second * dials.speed;
    float e = 0.004;
    float dpdx = (stream_potential(w + float2(e, 0.0)) - stream_potential(w - float2(e, 0.0))) / (2.0 * e);
    float dpdy = (stream_potential(w + float2(0.0, e)) - stream_potential(w - float2(0.0, e))) / (2.0 * e);
    float calm = 1.0 - saturate(here.w / 1.2);
    float2 eddies = float2(dpdy, -dpdx) * dials.turbulence * (0.006 + 0.02 * calm);
    float2 wind = float2(dials.wind_east, dials.wind_north) * 0.0009;
    return water + eddies + wind;
}

float3 streak_colour(float speed01, float seed) {
    float3 c = lerp(MAGENTA, HOT_PINK, smoothstep(0.1, 0.5, speed01));
    c = lerp(c, CORAL, smoothstep(0.5, 0.72, speed01));
    c = lerp(c, CYAN, smoothstep(0.78, 1.0, speed01));
    return lerp(c, PEACH, step(0.985, seed));
}

float2 world_to_texel(float2 w) {
    return world_to_uv(w) * dials.texture_size;
}

[numthreads(256, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint index = id.x;
    if (float(index) >= dials.particle_count) { return; }
    float4 p = particles[index];
    float2 position = p.xy;
    float age = p.z;
    float life = p.w;

    float4 here = field_at(position);
    bool outside = any(abs(position) > WORLD_HALF_EXTENT * 0.985);
    bool aground = here.z < 0.5;
    bool first_frame = dials.frame_index < 0.5;
    if (first_frame || !(life > 0.0) || age > life || outside || aground) {
        // Respawn where the water is: rejection-sample the snapshot, keeping fast water more often.
        float salt = float(index) * 0.618 + dials.frame_index * 1.37;
        for (int attempt = 0; attempt < 8; attempt++) {
            float r1 = hash11(salt * 1.1 + 0.3 + float(attempt) * 7.1);
            float r2 = hash11(salt * 2.3 + 1.7 + float(attempt) * 3.3);
            float r3 = hash11(salt * 3.7 + 2.9 + float(attempt) * 5.9);
            float2 candidate = (float2(r1, r2) * 2.0 - 1.0) * WORLD_HALF_EXTENT * 0.98;
            float4 there = field_at(candidate);
            position = candidate;
            if (there.z > 0.5 && r3 < 0.08 + 0.92 * saturate(there.w / 1.3)) { break; }
        }
        float r4 = hash11(salt * 5.3 + 4.1);
        age = first_frame ? r4 * 3.0 : 0.0;
        life = 1.5 + 3.5 * r4;
        here = field_at(position);
    }

    float2 velocity = flow_at(position, here);
    float2 next = position + velocity * dials.step_seconds;
    age += dials.step_seconds;

    float speed01 = saturate(here.w / 1.5);
    float seed = hash11(float(index) * 0.731);
    float fade = sin(PI * saturate(age / life));
    float lit = (0.0002 + 0.0028 * speed01 * speed01 * speed01) * fade * (1.0 + 1.8 * dials.beat * speed01) * dials.brightness;
    float3 colour = streak_colour(speed01, seed) * lit;

    // The streak spans this step and the one before, so it reads as motion.
    float2 from_texel = world_to_texel(position - velocity * dials.step_seconds);
    float2 to_texel = world_to_texel(next);
    float span = length(to_texel - from_texel);
    int steps = clamp(int(ceil(span)), 1, 16);
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
CURRENT_DIALS = struct.Struct("<12f")
FIELD_FORMAT = "rgba32_float"
LAYER_USAGE = ["storage_binding", "texture_binding"]


def workgroups(items: int, size: int) -> int:
    return (items + size - 1) // size


@dataclass
class GulfStreamCurrentConfig:
    """GulfStreamCurrent's settings."""

    particle_count: int = 2_097_152
    trail_decay: float = 0.86
    turbulence: float = 1.0
    time_lapse: float = 9000.0
    brightness: float = 1.0
    step_seconds: float = 1.0 / 30.0


@node.meta(
    category="Gulf Stream Neon",
    description="A million GPU particles advected through a real HYCOM surface-current snapshot, with curl-noise eddies, the station's wind and streak trails.",
)
@node.process
class GulfStreamCurrent:
    """Particles riding the real Florida Current north, with trails."""

    def __init__(self, config: GulfStreamCurrentConfig) -> None:
        self.config = config

    soundtrack = Input(Records, delivery="newest", description="beat, energy: from SoundtrackPulse")
    station = Input(Records, delivery="newest", description="wind_speed_mps, wind_from_degrees: from NoaaStationFeed")
    trails = Output(Image, description="The particle trails in world space, HDR")

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.take_clock = TakeClock()
        gpu = ctx.gpu_full_access
        size = WORLD_TEXTURE_SIZE
        self.fade_kernel = gpu.create_compute_kernel(source=FADE_SLANG, push_constant_size=FADE_DIALS.size)
        self.current_kernel = gpu.create_compute_kernel(source=CURRENT_SLANG, push_constant_size=CURRENT_DIALS.size)
        self.field = gpu.acquire_texture(size, size, FIELD_FORMAT, LAYER_USAGE)
        self.field.lock(read_only=False)
        self.field.as_numpy().reshape(size * size, 4)[:] = baked_currents(size)
        self.field.unlock()
        self.particles = gpu.acquire_storage_buffer((self.config.particle_count, 4), "float32")
        self.trail_texture = gpu.acquire_texture(size, size, HDR_LAYER_FORMAT, LAYER_USAGE)
        self.output_textures = NodeOutputTextureRing(HDR_LAYER_FORMAT, LAYER_USAGE)
        self.frame_index = 0
        self.latest_soundtrack: dict = {}
        self.latest_station: dict = {}
        log.info("current snapshot uploaded", particles=self.config.particle_count)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        soundtrack = ctx.inputs.read(self.soundtrack)
        if soundtrack is not None:
            self.latest_soundtrack = soundtrack
        station = ctx.inputs.read(self.station)
        if station is not None:
            self.latest_station = station
        seconds = self.take_clock.seconds_at(ctx)
        moment = take_moment_at(seconds)
        beat = float(self.latest_soundtrack.get("beat", 0.0))
        energy = float(self.latest_soundtrack.get("energy", 0.0))
        speed = moment.current_speed * (1.0 + 0.3 * energy)
        # Wind is reported as where it blows from; the drift goes the other way.
        wind_speed = float(self.latest_station.get("wind_speed_mps", 0.0))
        wind_from = math.radians(float(self.latest_station.get("wind_from_degrees", 0.0)))
        wind_east = -wind_speed * math.sin(wind_from)
        wind_north = -wind_speed * math.cos(wind_from)
        size = WORLD_TEXTURE_SIZE
        gpu = ctx.gpu_limited_access
        self.fade_kernel.dispatch(
            bindings={"trails": self.trail_texture},
            group_count=(workgroups(size, 8), workgroups(size, 8), 1),
            push_constants=FADE_DIALS.pack(size, self.config.trail_decay if self.frame_index else 0.0),
        )
        self.current_kernel.dispatch(
            bindings={"particles": self.particles, "trails": self.trail_texture, "field": self.field},
            group_count=(workgroups(self.config.particle_count, 256), 1, 1),
            push_constants=CURRENT_DIALS.pack(
                size,
                self.config.particle_count,
                seconds,
                self.config.step_seconds,
                speed,
                self.config.turbulence,
                beat,
                self.frame_index,
                self.config.time_lapse / METRES_PER_WORLD_UNIT,
                wind_east,
                wind_north,
                self.config.brightness,
            ),
        )
        self.frame_index += 1
        texture = self.output_textures.next_texture_for_this_frame(gpu, size, size)
        gpu.copy_surface_to_surface(self.trail_texture.surface_id, texture)
        ctx.outputs.write(
            self.trails,
            ImageFrameToWrite.of_texture(texture, RENDERED_FRAME_COLOUR),
            timestamp_ns=ctx.time,
        )
