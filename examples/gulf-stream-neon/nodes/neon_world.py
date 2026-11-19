# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What every Gulf Stream Neon node agrees on: the world, its palette and the take.

The seafloor and the current are drawn into world-space textures that cover
the same square of ocean, so the compositor can fly one camera over both. The
take is the camera's choreography and the sky's time of day, a pure function
of stream time, so every node of a step reads the same moment.
"""

from __future__ import annotations

import math
from dataclasses import dataclass

from tatolab.stream import ColorInfo

# The world-space layers' extent in texels, and the square of world they cover.
WORLD_TEXTURE_SIZE = 2048
WORLD_HALF_EXTENT = 2.5

NANOSECONDS_PER_SECOND = 1_000_000_000

RENDERED_FRAME_COLOUR = ColorInfo(primaries="bt709", transfer="srgb", range="full")

# Shared Slang: hashing, value noise, the coastline and the shelf edge. The
# Gulf Stream hugs the drop where the shallow Florida shelf falls into the
# Straits, so the current's core follows `shelf_edge_x`.
WORLD_SLANG = """
static const float WORLD_HALF_EXTENT = 2.5;

static const float3 CORAL = float3(1.0, 0.435, 0.380);
static const float3 PEACH = float3(1.0, 0.702, 0.420);
static const float3 HOT_PINK = float3(1.0, 0.180, 0.533);
static const float3 MAGENTA = float3(0.784, 0.125, 1.0);
static const float3 CYAN = float3(0.0, 0.898, 0.831);
static const float3 NIGHT = float3(0.071, 0.039, 0.180);

float hash21(float2 p) {
    p = frac(p * float2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return frac(p.x * p.y);
}

float hash11(float p) {
    p = frac(p * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return frac(p);
}

float value_noise(float2 p) {
    float2 i = floor(p);
    float2 f = frac(p);
    float2 u = f * f * (3.0 - 2.0 * f);
    float a = hash21(i);
    float b = hash21(i + float2(1.0, 0.0));
    float c = hash21(i + float2(0.0, 1.0));
    float d = hash21(i + float2(1.0, 1.0));
    return lerp(lerp(a, b, u.x), lerp(c, d, u.x), u.y);
}

float fbm(float2 p, int octaves) {
    float sum = 0.0;
    float amplitude = 0.5;
    for (int octave = 0; octave < octaves; octave++) {
        sum += amplitude * value_noise(p);
        p = float2(p.x * 1.6 - p.y * 1.2, p.x * 1.2 + p.y * 1.6);
        amplitude *= 0.5;
    }
    return sum;
}

// West of this x is land: a gently wandering mainland coast.
float coast_x(float y) {
    return -1.15 + 0.10 * sin(0.9 * y + 0.7) + 0.05 * sin(2.3 * y + 1.9);
}

// The thin barrier island offshore of the coast, with a bay behind it.
float barrier_island_x(float y) {
    return coast_x(y) + 0.30 + 0.04 * sin(1.7 * y);
}

// Where the shelf drops into the deep: the line the current rides.
float shelf_edge_x(float y) {
    return -0.35 + 0.16 * sin(0.75 * y + 0.4) + 0.05 * sin(2.1 * y + 2.2);
}

float world_to_uv_component(float w) {
    return (w + WORLD_HALF_EXTENT) / (2.0 * WORLD_HALF_EXTENT);
}

float2 world_to_uv(float2 w) {
    return float2(world_to_uv_component(w.x), 1.0 - world_to_uv_component(w.y));
}

float2 uv_to_world(float2 uv) {
    return float2(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0) * WORLD_HALF_EXTENT;
}
"""


def smoothstep(edge0: float, edge1: float, x: float) -> float:
    t = min(1.0, max(0.0, (x - edge0) / (edge1 - edge0)))
    return t * t * (3.0 - 2.0 * t)


def lerp(a: float, b: float, t: float) -> float:
    return a + (b - a) * t


@dataclass(frozen=True)
class TakeMoment:
    """One instant of the take: where the camera is and what the sky does."""

    camera_x: float
    camera_y: float
    camera_altitude: float
    camera_pitch_degrees: float
    camera_yaw_degrees: float
    camera_roll_degrees: float
    time_of_day: float
    current_speed: float
    glow: float


def shelf_edge_x(y: float) -> float:
    return -0.35 + 0.16 * math.sin(0.75 * y + 0.4) + 0.05 * math.sin(2.1 * y + 2.2)


def take_moment_at(seconds: float) -> TakeMoment:
    """The take, bar by bar at 120 BPM (a bar is 2 s).

    0-2 s inside the stream, skimming the core, then a pull-back; 2-4 s a wide
    golden-hour drift; 4-8 s the map from above; 8-10 s the build, as the sun
    goes down; 10-20 s the drop, low and fast, banking; 20-24 s a rise into
    the night for the end.
    """
    t = seconds
    travel_y = -2.1 + 0.11 * t + 0.25 * smoothstep(10.0, 20.0, t)
    edge = shelf_edge_x(travel_y + 0.55)

    pull_back = smoothstep(0.0, 2.0, t)
    above = smoothstep(3.8, 4.7, t) * (1.0 - smoothstep(7.3, 8.3, t))
    drop = smoothstep(9.9, 10.15, t) * (1.0 - smoothstep(19.6, 21.5, t))
    finale = smoothstep(19.6, 23.0, t)

    altitude = lerp(0.045, 0.42, pull_back * pull_back)
    altitude = lerp(altitude, 0.3, smoothstep(8.0, 10.0, t))
    altitude = lerp(altitude, 0.16, drop)
    altitude = lerp(altitude, 0.95, finale)
    pitch = lerp(4.0, 17.0, pull_back)
    pitch = lerp(pitch, 10.0, drop)
    pitch = lerp(pitch, 27.0, finale)

    camera_x = lerp(edge + 0.05, edge - 0.05, pull_back)
    camera_x = lerp(camera_x, edge + 0.02 + 0.06 * math.sin(t * 0.9), drop)
    camera_x = lerp(camera_x, edge - 0.25, finale)
    rush = 0.55 * (1.0 - smoothstep(0.0, 2.0, t)) * t
    camera_y = travel_y + rush - 0.55 * pull_back - 0.3 * finale

    altitude = lerp(altitude, 2.7, above)
    pitch = lerp(pitch, 89.0, above)
    camera_x = lerp(camera_x, -0.6, above)
    camera_y = lerp(camera_y, travel_y + 0.25, above)

    yaw = 6.0 * math.sin(0.35 * t) * (1.0 - above) - 10.0 * finale
    roll = (7.0 * math.sin(t * 0.9 + 0.5)) * drop

    time_of_day = 0.12 + 0.88 * smoothstep(7.8, 10.0, t)
    speed = 1.0 + 0.5 * smoothstep(8.0, 10.0, t) + 1.2 * drop
    glow = 1.0 + 0.5 * drop
    return TakeMoment(
        camera_x=camera_x,
        camera_y=camera_y,
        camera_altitude=altitude,
        camera_pitch_degrees=pitch,
        camera_yaw_degrees=yaw,
        camera_roll_degrees=roll,
        time_of_day=time_of_day,
        current_speed=speed,
        glow=glow,
    )


class TakeClock:
    """Seconds into the take, the same in every node of a step.

    On a stepped clock it is the step index times the step; on a real-time
    clock, time since this node first asked.
    """

    def __init__(self) -> None:
        self.origin_ns: int | None = None

    def origin(self, ctx) -> int:
        if self.origin_ns is None:
            reading = ctx.stream_clock
            step_index = reading.step_index
            if reading.mode == "stepped" and step_index is not None and reading.step_ns:
                self.origin_ns = ctx.time - step_index * reading.step_ns
            else:
                self.origin_ns = ctx.time
        return self.origin_ns

    def seconds_at(self, ctx, timestamp_ns: int | None = None) -> float:
        origin = self.origin(ctx)
        stamp = ctx.time if timestamp_ns is None else timestamp_ns
        return (stamp - origin) / NANOSECONDS_PER_SECOND
