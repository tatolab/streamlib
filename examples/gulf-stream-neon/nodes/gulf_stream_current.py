# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The current: a million GPU particles riding the real Gulf Stream north, with trails.

Importable as `nodes.gulf_stream_current:GulfStreamCurrent`. Its body uploads
a HYCOM surface-current snapshot of the map's square into a held texture; the
particles live in a storage buffer this node holds for its whole life. Each
frame one kernel fades the trail texture and a second advects every particle
through the real flow, time-lapsed, with curl-noise eddies below the model's
grid and the station's wind as a surface drift, and draws its streak.
"""

from __future__ import annotations

import math
import struct
from typing import Annotated, Callable

from tatolab.stream import (
    Description,
    GpuContextLimitedAccess,
    Input,
    InputPin,
    NodeOutputTextureRing,
    NumberRange,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    TextureToWrite,
    computed,
    log,
    node,
)

from nodes.neon_world import (
    HDR_LAYER_FORMAT,
    METRES_PER_WORLD_UNIT,
    RENDERED_FRAME_COLOUR,
    WORLD_SLANG,
    StageLookReading,
    SoundtrackPulseReading,
    StageCamera,
    StationReading,
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
    float eddy_x;
    float eddy_y;
    float eddy_age;
    float eddy_strength;
    float burst_now;
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
    // A dropped eddy: a vortex that spins up, rides the current and fades.
    float2 to_eddy = w - float2(dials.eddy_x, dials.eddy_y);
    float spin = dials.eddy_strength * exp(-dials.eddy_age * 0.5) * smoothstep(0.0, 0.3, dials.eddy_age);
    float2 vortex = float2(-to_eddy.y, to_eddy.x) / (dot(to_eddy, to_eddy) + 0.0009) * spin * 0.01;
    return water + eddies + wind + vortex;
}

float3 streak_colour(float speed01, float seed) {
    // The Tato current: violet in the slack water, blue as it gathers, mint at the core.
    float3 c = lerp(TATO_VIOLET, TATO_BLUE, smoothstep(0.1, 0.45, speed01));
    c = lerp(c, TATO_MINT, smoothstep(0.5, 0.85, speed01));
    c = lerp(c, TATO_CREAM, smoothstep(0.93, 1.0, speed01) * 0.6);
    return lerp(c, TATO_YELLOW, step(0.985, seed));
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
    // A burst: one particle in sixteen is thrown into a ring where the stage was clicked.
    bool bursting = (index % 16u) == 0u;
    if (dials.burst_now > 0.5 && bursting) {
        float angle = hash11(float(index) * 0.37) * 6.2831853;
        float radius = 0.004 + 0.035 * sqrt(hash11(float(index) * 0.91 + 3.1));
        position = float2(dials.eddy_x, dials.eddy_y) + float2(cos(angle), sin(angle)) * radius;
        age = 0.0;
        life = 3.5 + 2.5 * hash11(float(index) * 0.53);
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
    // An eddy eases in over its first quarter second rather than popping, then fades.
    float burst_glow = (bursting ? 1.0 : 0.0) * smoothstep(0.0, 0.25, dials.eddy_age) * exp(-dials.eddy_age * 0.45) * step(dials.eddy_age, 6.0) * dials.eddy_strength;
    colour += lerp(TATO_MINT, TATO_CREAM, 0.3) * burst_glow * 0.035 * fade;

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
CURRENT_DIALS = struct.Struct("<17f")
FIELD_FORMAT = "rgba32_float"
LAYER_USAGE = ["storage_binding", "texture_binding"]


def workgroups(items: int, size: int) -> int:
    return (items + size - 1) // size


class ParticleBuffer:
    """The storage buffer that holds `count` particles."""

    def __init__(self, gpu: GpuContextLimitedAccess, count: int) -> None:
        self.count = count
        self.buffer = gpu.acquire_storage_buffer((count, 4), "float32")
        log.info("particle buffer acquired", particles=count)


@node.meta(
    category="Gulf Stream Neon",
    description="A million GPU particles advected through a real HYCOM surface-current snapshot, with curl-noise eddies, the station's wind and streak trails.",
)
@node(execution="reactive")
def GulfStreamCurrent(
    *,
    particle_count: Input[
        Annotated[
            int,
            NumberRange(minimum=1024, maximum=4_194_304),
            Description("How many particles the buffer holds; a change builds the buffer again."),
        ]
    ] = Input(2_097_152),
    trail_decay: Input[
        Annotated[
            float, NumberRange(minimum=0.0, maximum=0.99), Description("How much of a trail each step keeps.")
        ]
    ] = Input(0.9273),
    turbulence: Input[
        Annotated[
            float, NumberRange(minimum=0.0, maximum=5.0), Description("How hard the curl-noise eddies stir.")
        ]
    ] = Input(1.0),
    time_lapse: Input[
        Annotated[
            float, NumberRange(minimum=0.0, maximum=40000.0), Description("How many times faster than life the current runs.")
        ]
    ] = Input(9000.0),
    brightness: Input[
        Annotated[
            float, NumberRange(minimum=0.0, maximum=4.0), Description("How bright the trails glow.")
        ]
    ] = Input(1.0),
    active_particles: Input[
        Annotated[
            int,
            NumberRange(minimum=1024, maximum=4_194_304),
            Description("How many of the particles move, at most `particle_count`."),
        ]
    ] = Input(2_097_152),
    step_seconds: Input[
        Annotated[
            float, NumberRange(minimum=0.0), Description("The seconds one step advances the particles.")
        ]
    ] = Input(1.0 / 60.0),
    soundtrack: InputPin[
        Annotated[
            SoundtrackPulseReading,
            StreamDelivery("newest"),
            Description("beat, energy: from SoundtrackPulse"),
        ]
    ],
    station: InputPin[
        Annotated[
            StationReading,
            StreamDelivery("newest"),
            Description("wind_speed_mps, wind_from_degrees: from NoaaStationFeed"),
        ]
    ],
    stage: InputPin[
        Annotated[
            StageCamera,
            StreamDelivery("newest"),
            Description("The stage view: its camera, and where it was clicked to drop an eddy"),
        ]
    ],
    look: InputPin[
        Annotated[
            StageLookReading,
            StreamDelivery("newest"),
            Description("The stage's smoothed look: current_speed scales the time-lapse"),
        ]
    ],
    trails: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(HDR_LAYER_FORMAT),
            Description("The particle trails in world space, HDR"),
        ]
    ],
    ctx: RuntimeContextFullAccess,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Particles riding the real Florida Current north, with trails."""

    take_clock = TakeClock()
    gpu = ctx.gpu_full_access
    size = WORLD_TEXTURE_SIZE
    fade_kernel = ctx.gpu_limited_access.create_compute_kernel(source=FADE_SLANG, push_constant_size=FADE_DIALS.size)
    current_kernel = ctx.gpu_limited_access.create_compute_kernel(source=CURRENT_SLANG, push_constant_size=CURRENT_DIALS.size)
    field = gpu.acquire_texture(size, size, FIELD_FORMAT, LAYER_USAGE)
    field.lock(read_only=False)
    field.as_numpy().reshape(size * size, 4)[:] = baked_currents(size)
    field.unlock()
    gpu_limited = ctx.gpu_limited_access
    particles = computed(lambda: ParticleBuffer(gpu_limited, particle_count.get()))
    trail_texture = gpu.acquire_texture(size, size, HDR_LAYER_FORMAT, LAYER_USAGE)
    output_textures = NodeOutputTextureRing(HDR_LAYER_FORMAT, LAYER_USAGE)
    frame_index = 0
    latest_soundtrack = SoundtrackPulseReading()
    latest_station: StationReading | None = None
    latest_stage: StageCamera | None = None
    latest_look = StageLookReading()
    bursts_seen: int | None = None
    eddy = (0.0, 0.0)
    eddy_age = 99.0
    eddy_strength = 0.0
    field_on_cpu = baked_currents(size).reshape(size, size, 4)
    log.info("current snapshot uploaded")

    def active() -> int:
        return max(1024, min(active_particles.get(), particles.get().count))

    def follow_the_eddy() -> bool:
        """Start an eddy where the stage was last clicked, and carry the one there is north on the real current."""
        nonlocal bursts_seen, eddy, eddy_age, eddy_strength
        step = step_seconds.get()
        newest_stage = latest_stage
        started = False
        if newest_stage is not None and bursts_seen is not None and newest_stage.bursts > bursts_seen:
            eddy = (newest_stage.burst_x, newest_stage.burst_y)
            eddy_age = 0.0
            eddy_strength = 1.0
            started = True
        if newest_stage is not None:
            bursts_seen = newest_stage.bursts
        size = field_on_cpu.shape[0]
        column = int(min(max((eddy[0] / 2.5 * 0.5 + 0.5) * size, 0), size - 1))
        row = int(min(max((0.5 - eddy[1] / 2.5 * 0.5) * size, 0), size - 1))
        east, north = field_on_cpu[row, column, 0], field_on_cpu[row, column, 1]
        scale = time_lapse.get() / METRES_PER_WORLD_UNIT
        eddy = (eddy[0] + float(east) * scale * step, eddy[1] + float(north) * scale * step)
        eddy_age += step
        return started

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal frame_index, latest_look, latest_soundtrack, latest_stage, latest_station
        newest_soundtrack = soundtrack.read()
        if newest_soundtrack is not None:
            latest_soundtrack = newest_soundtrack
        newest_station = station.read()
        if newest_station is not None:
            latest_station = newest_station
        newest_stage = stage.read()
        if newest_stage is not None:
            latest_stage = newest_stage
        newest_look = look.read()
        if newest_look is not None:
            latest_look = newest_look
        burst_now = follow_the_eddy()
        seconds = take_clock.seconds_at(ctx)
        moment = take_moment_at(seconds)
        beat = latest_soundtrack.beat
        energy = latest_soundtrack.energy
        speed = moment.current_speed * (1.0 + 0.3 * energy)
        # Wind is reported as where it blows from; the drift goes the other way.
        newest_station = latest_station or StationReading()
        wind_speed = newest_station.wind_speed_mps
        wind_from = math.radians(newest_station.wind_from_degrees)
        wind_east = -wind_speed * math.sin(wind_from)
        wind_north = -wind_speed * math.cos(wind_from)
        size = WORLD_TEXTURE_SIZE
        gpu = ctx.gpu_limited_access
        fade_kernel.dispatch(
            bindings={"trails": trail_texture},
            group_count=(workgroups(size, 8), workgroups(size, 8), 1),
            push_constants=FADE_DIALS.pack(size, trail_decay.get() if frame_index else 0.0),
        )
        current_kernel.dispatch(
            bindings={"particles": particles.get().buffer, "trails": trail_texture, "field": field},
            group_count=(workgroups(active(), 256), 1, 1),
            push_constants=CURRENT_DIALS.pack(
                size,
                active(),
                seconds,
                step_seconds.get(),
                speed,
                turbulence.get(),
                beat,
                frame_index,
                time_lapse.get() * latest_look.current_speed / METRES_PER_WORLD_UNIT,
                wind_east,
                wind_north,
                # Light per step, so the trails glow the same at any frame rate.
                brightness.get() * step_seconds.get() * 30.0,
                eddy[0],
                eddy[1],
                eddy_age,
                eddy_strength,
                1.0 if burst_now else 0.0,
            ),
        )
        frame_index += 1
        texture = output_textures.next_texture_for_this_frame(gpu, size, size)
        gpu.copy_surface_to_surface(trail_texture.surface_id, texture)
        trails.write(
            TextureToWrite.of_texture(texture, RENDERED_FRAME_COLOUR),
            timestamp_ns=ctx.time,
        )

    return each_frame
