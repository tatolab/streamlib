# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""One camera flown over the real map: raymarched Deco skyline, mirror water, synthwave sky.

Importable as `nodes.neon_horizon_renderer:NeonHorizonRenderer`. One compute
kernel casts a ray per pixel. It raymarches a signed-distance skyline of
stepped Art Deco towers standing on the real coast, wherever the relief says
there is a city, each edged in neon; it lands the rest on the sea, whose
wind-driven wave normals reflect a second raymarch of the skyline and the
sky (Schlick fresnel) over the neon seafloor and the current glowing through.
At the reveal a scan sweeps north and turns the sea into the real chart:
ETOPO depth tints, hillshade, the coastline and a half-degree graticule.
The picture is HDR, `rgba16_float`, with the ray's distance in alpha, for the
lens nodes that follow: bloom, god rays, depth of field.
"""

from __future__ import annotations

import math
import struct
from dataclasses import dataclass
from typing import Annotated

from tatolab.stream import (
    Image,
    ImageFrameToWrite,
    Input,
    NodeOutputTextureRing,
    NumberRange,
    Output,
    Records,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    node,
)

from nodes.neon_world import (
    HDR_LAYER_FORMAT,
    LATITUDE_NORTH,
    LATITUDE_SOUTH,
    LONGITUDE_EAST,
    LONGITUDE_WEST,
    RENDERED_FRAME_COLOUR,
    WORLD_SLANG,
    TakeClock,
    camera_basis,
    stage_moment,
    sun_direction,
    take_moment_at,
)

RENDERER_SLANG = WORLD_SLANG + """
[[vk::binding(0, 0)]] Sampler2D contour_layer;
[[vk::binding(1, 0)]] Sampler2D current_layer;
[[vk::binding(2, 0)]] Sampler2D relief_layer;
[[vk::binding(3, 0)]] [format("rgba16f")] RWTexture2D<float4> scene;

struct RendererDials {
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
    float reveal;
    float3 sun_direction;
    float drop;
    float wind_east;
    float wind_north;
    float skyline_height;
    float exposure;
};
[[vk::push_constant]] ConstantBuffer<RendererDials> dials;

static const float LATITUDE_SOUTH = MAP_LATITUDE_SOUTH;
static const float LATITUDE_NORTH = MAP_LATITUDE_NORTH;
static const float LONGITUDE_WEST = MAP_LONGITUDE_WEST;
static const float LONGITUDE_EAST = MAP_LONGITUDE_EAST;

static const float CELL = 0.011;
static const float SKY_DISTANCE = 1000.0;

static const float2 GOLDEN_TAPS[12] = {
    float2(1.0, 0.0), float2(-0.737, 0.676), float2(0.087, -0.996), float2(0.608, 0.794),
    float2(-0.984, -0.174), float2(0.819, -0.574), float2(-0.259, 0.966), float2(-0.5, -0.866),
    float2(0.966, 0.259), float2(-0.866, 0.5), float2(0.342, -0.94), float2(0.0, 1.0)
};

// ---------------------------------------------------------------- the sky

float3 sky_colour(float3 direction, bool with_sun) {
    float elevation = direction.z;
    float tod = dials.time_of_day;
    float3 zenith = lerp(lerp(TATO_VIOLET * 0.45, DEEP_BAND * 1.1, smoothstep(0.0, 0.5, tod)),
                         TATO_INK * 0.5, smoothstep(0.5, 1.0, tod));
    float3 middle = lerp(lerp(TATO_PINK * 0.95, TATO_VIOLET * 0.8, smoothstep(0.0, 0.55, tod)),
                         DEEP_BAND * 0.45, smoothstep(0.55, 1.0, tod));
    float3 horizon = lerp(lerp(TATO_YELLOW * 1.15, TATO_PINK * 1.1, smoothstep(0.0, 0.5, tod)),
                          TATO_VIOLET * 0.22, smoothstep(0.5, 1.0, tod));
    float3 colour = lerp(horizon, middle, smoothstep(0.0, 0.12, elevation));
    colour = lerp(colour, zenith, smoothstep(0.1, 0.5, elevation));

    float3 sun = dials.sun_direction;
    float sun_cos = dot(direction, sun);

    // Long bands of cloud near the horizon, their undersides lit by the sun.
    float2 cloud_uv = direction.xy / max(elevation + 0.08, 0.03);
    float bands = fbm(float2(cloud_uv.x * 0.9 + dials.seconds * 0.02, cloud_uv.y * 3.5), 5);
    float cloud = smoothstep(0.52, 0.78, bands) * smoothstep(0.0, 0.04, elevation) * (1.0 - smoothstep(0.12, 0.3, elevation));
    float3 cloud_lit = lerp(TATO_VIOLET * 0.35, TATO_CREAM * 1.5, pow(saturate(sun_cos * 0.5 + 0.5), 6.0)) * (1.0 - 0.6 * tod);
    colour = lerp(colour, cloud_lit + TATO_PINK * 0.15 * (1.0 - 0.7 * tod), cloud * 0.85);

    // Stars come out after the sun goes.
    float2 star_cell = floor(float2(atan2(direction.x, direction.y) * 300.0, elevation * 480.0));
    float star = step(0.9965, hash21(star_cell)) * smoothstep(0.55, 1.0, tod) * smoothstep(0.03, 0.2, elevation);
    star *= 0.6 + 0.4 * sin(dials.seconds * 4.0 + hash21(star_cell + 5.0) * 30.0);
    colour += TATO_CREAM * star * 2.0 * (1.0 - cloud);

    // The sun: a striped disc sinking into the haze, too bright to look at.
    float3 flattened = normalize(float3(direction.x, direction.y, 0.0) + 1e-5);
    float3 sun_flat = normalize(float3(sun.x, sun.y, 0.0));
    float azimuth_offset = acos(clamp(dot(flattened, sun_flat), -1.0, 1.0));
    float2 on_disc = float2(azimuth_offset, elevation - sun.z) / 0.12;
    float disc = 1.0 - smoothstep(0.95, 1.0, length(on_disc));
    float stripes = (on_disc.y < 0.2) ? step(0.36 + 0.28 * (0.2 - on_disc.y), frac(on_disc.y * 5.5 + dials.seconds * 0.12)) : 1.0;
    float3 sun_colour = lerp(TATO_PINK * 4.0, TATO_YELLOW * 5.5, saturate(on_disc.y * 0.6 + 0.5));
    float halo = exp(-length(on_disc) * 1.3);
    if (!with_sun) { return colour + TATO_PINK * halo * 0.4; }
    colour = lerp(colour, sun_colour, disc * stripes * step(0.0, elevation) * (1.0 - cloud * 0.7));
    colour += lerp(TATO_PINK, TATO_YELLOW, 0.3) * halo * 0.7 * (1.0 - 0.5 * smoothstep(0.6, 1.0, tod));
    return colour;
}

// ---------------------------------------------------------------- the skyline

float4 relief_at_world(float2 w) {
    return relief_layer.SampleLevel(world_to_uv(w), 0.0);
}

float box_distance(float3 p, float3 half_size) {
    float3 q = abs(p) - half_size;
    return length(max(q, 0.0)) + min(max(q.x, max(q.y, q.z)), 0.0);
}

struct Tower {
    float2 centre;
    float half_width;
    float height;
    float seed;
    bool exists;
};

Tower tower_of(float2 cell) {
    Tower tower;
    tower.centre = (cell + 0.5) * CELL;
    float seed = hash21(cell * 1.31 + 7.7);
    tower.seed = seed;
    float4 here = relief_at_world(tower.centre);
    float density = here.b;
    float tall = pow(hash21(cell + 19.1), 3.0);
    tower.exists = here.g > 0.5 && density > 0.04 && hash21(cell + 3.3) < 0.25 + 0.75 * density;
    tower.half_width = CELL * (0.24 + 0.14 * hash21(cell + 11.0));
    tower.height = dials.skyline_height * (0.12 + 0.88 * tall) * (0.35 + 0.65 * density) * (1.0 - dials.reveal);
    return tower;
}

// A stepped Deco tower: a base, two setbacks and a spire, in the tower's own frame.
float tower_distance(float3 local, Tower tower) {
    float s = tower.half_width;
    float h = tower.height;
    float d = box_distance(local - float3(0.0, 0.0, h * 0.30), float3(s, s, h * 0.30));
    d = min(d, box_distance(local - float3(0.0, 0.0, h * 0.40), float3(s * 0.78, s * 0.78, h * 0.40)));
    d = min(d, box_distance(local - float3(0.0, 0.0, h * 0.46), float3(s * 0.52, s * 0.52, h * 0.46)));
    float spire = box_distance(local - float3(0.0, 0.0, h * 0.5), float3(s * 0.07, s * 0.07, h * 0.58));
    return min(d, spire);
}

struct SkylineHit {
    bool hit;
    float distance;
    float3 position;
    float2 cell;
};

// March the ray through the city's cells: each step goes no further than
// the nearest tower of this cell or the cell's own wall, so none is skipped.
SkylineHit march_skyline(float3 origin, float3 direction, float far, int max_steps) {
    SkylineHit result;
    result.hit = false;
    result.distance = far;
    result.position = origin;
    result.cell = float2(0.0, 0.0);
    float ceiling = dials.skyline_height * 1.2;
    if (ceiling <= 0.0 || dials.reveal > 0.999) { return result; }
    float t = 0.0;
    if (origin.z > ceiling) {
        if (direction.z >= 0.0) { return result; }
        t = (origin.z - ceiling) / -direction.z;
    }
    float t_end = far;
    if (direction.z < 0.0) { t_end = min(t_end, origin.z / -direction.z); }
    if (direction.z > 0.0) { t_end = min(t_end, (ceiling - origin.z) / direction.z); }
    for (int step = 0; step < max_steps && t < t_end; step++) {
        float3 p = origin + direction * t;
        float2 cell = floor(p.xy / CELL);
        Tower tower = tower_of(cell);
        float d = 1e3;
        if (tower.exists) {
            d = tower_distance(p - float3(tower.centre, 0.0), tower);
        }
        if (d < 0.00005 + t * 0.0004) {
            result.hit = true;
            result.distance = t;
            result.position = p;
            result.cell = cell;
            return result;
        }
        // The distance to the wall of this cell along the ray.
        float2 inside = p.xy / CELL - cell;
        float2 to_wall = float2(
            direction.x > 0.0 ? (1.0 - inside.x) : inside.x,
            direction.y > 0.0 ? (1.0 - inside.y) : inside.y) * CELL;
        float2 speed = max(abs(direction.xy), 1e-5);
        float wall = min(to_wall.x / speed.x, to_wall.y / speed.y) + CELL * 0.01;
        t += clamp(min(d, wall), 0.00003, CELL);
    }
    return result;
}

float3 shade_tower(SkylineHit hit, float3 direction) {
    Tower tower = tower_of(hit.cell);
    float3 local = hit.position - float3(tower.centre, 0.0);
    float e = 0.00004;
    float3 normal = normalize(float3(
        tower_distance(local + float3(e, 0, 0), tower) - tower_distance(local - float3(e, 0, 0), tower),
        tower_distance(local + float3(0, e, 0), tower) - tower_distance(local - float3(0, e, 0), tower),
        tower_distance(local + float3(0, 0, e), tower) - tower_distance(local - float3(0, 0, e), tower)));
    float s = tower.half_width;
    float h = tower.height;
    float night = smoothstep(0.3, 0.9, dials.time_of_day);

    // The facade: deep violet stone lit by the sky above and the sun's rim.
    float3 colour = TATO_INK * (0.6 + 0.6 * normal.z) + TATO_VIOLET * 0.05;
    float rim = pow(saturate(dot(normal, dials.sun_direction) * 0.5 + 0.5), 4.0) * (1.0 - night);
    colour += lerp(TATO_YELLOW, TATO_CREAM, 0.4) * rim * 0.9;

    // Windows: a grid of warm rooms, some dark, a few flickering.
    float2 face = abs(normal.x) > 0.5 ? local.yz : local.xz;
    float2 room = float2(face.x / (s * 0.16), local.z / (h * 0.025 + 0.0003));
    float2 room_cell = floor(room);
    float2 in_room = frac(room);
    float window = step(0.18, in_room.x) * step(in_room.x, 0.82) * step(0.25, in_room.y) * step(in_room.y, 0.8);
    float lit_room = step(0.42, hash21(room_cell + tower.seed * 91.0));
    float flicker = 0.75 + 0.25 * sin(dials.seconds * 6.0 + hash21(room_cell) * 40.0);
    float3 room_colour = lerp(TATO_CREAM, TATO_YELLOW, hash21(room_cell + 2.0));
    colour += room_colour * window * lit_room * flicker * (1.2 + 2.2 * night) * step(abs(normal.z), 0.5);

    // Neon on every edge and setback: the Deco outline that blooms.
    float3 neon = tower.seed < 0.4 ? TATO_PINK : (tower.seed < 0.7 ? TATO_YELLOW : TATO_MINT);
    float3 edge_local = abs(local);
    float edge = 0.0;
    float tiers[3] = { 1.0, 0.78, 0.52 };
    float tops[3] = { 0.60, 0.80, 0.92 };
    for (int tier = 0; tier < 3; tier++) {
        float w = s * tiers[tier];
        float top = h * tops[tier];
        float corner = max(abs(edge_local.x - w), abs(edge_local.y - w));
        float on_corner = step(local.z, top) * (1.0 - smoothstep(0.0, s * 0.05, corner));
        float rim_line = (1.0 - smoothstep(0.0, h * 0.012 + 0.00008, abs(local.z - top)))
                         * step(max(edge_local.x, edge_local.y), w * 1.02);
        edge = max(edge, max(on_corner, rim_line));
    }
    float pulse = 1.0 + 1.5 * dials.beat * dials.drop;
    colour += neon * edge * (6.0 + 8.0 * night) * pulse * dials.glow;
    // A beacon at the spire's tip.
    float beacon = exp(-length(local - float3(0.0, 0.0, h * 1.08)) / (s * 0.15));
    colour += TATO_PINK * beacon * 12.0 * (0.5 + 0.5 * sin(dials.seconds * 3.0 + tower.seed * 20.0));
    return colour;
}

// ---------------------------------------------------------------- the sea

float3 layer_at(float2 uv, float footprint) {
    float3 sharp = contour_layer.SampleLevel(uv, 0.0).rgb + current_layer.SampleLevel(uv, 0.0).rgb;
    float3 soft = float3(0.0, 0.0, 0.0);
    float3 wide = float3(0.0, 0.0, 0.0);
    float near_radius = max(footprint * 0.6, 0.0004);
    float far_radius = max(footprint * 2.5, 0.003);
    for (int tap = 0; tap < 12; tap++) {
        float2 near_uv = uv + GOLDEN_TAPS[tap] * near_radius * (0.5 + 0.5 * float(tap % 3));
        float2 far_uv = uv + GOLDEN_TAPS[tap] * far_radius * (0.4 + 0.3 * float(tap % 3));
        soft += contour_layer.SampleLevel(near_uv, 0.0).rgb + current_layer.SampleLevel(near_uv, 0.0).rgb;
        wide += contour_layer.SampleLevel(far_uv, 0.0).rgb + 1.6 * current_layer.SampleLevel(far_uv, 0.0).rgb;
    }
    soft /= 12.0;
    wide /= 12.0;
    float filtered = saturate(footprint / 0.0008);
    float3 base = lerp(sharp, soft, filtered * 0.85);
    return base + wide * (0.12 + 0.3 * dials.beat) * dials.glow;
}

// Wind-driven waves: a few directional swells and chop, their slope as a normal.
float3 wave_normal(float2 w, float distance) {
    float2 wind = float2(dials.wind_east, dials.wind_north);
    float wind_speed = length(wind);
    float2 along = wind_speed > 0.1 ? wind / wind_speed : float2(0.6, 0.8);
    float2 across = float2(-along.y, along.x);
    float2 slope = float2(0.0, 0.0);
    float amplitude = 0.006 * (0.6 + 0.08 * wind_speed);
    float frequency = 420.0;
    float t = dials.seconds;
    for (int wave = 0; wave < 5; wave++) {
        float angle = (float(wave) - 2.0) * 0.45;
        float2 heading = along * cos(angle) + across * sin(angle);
        float phase = dot(w, heading) * frequency + t * sqrt(frequency) * 0.35 + float(wave) * 1.7;
        slope += heading * cos(phase) * amplitude * frequency;
        frequency *= 1.7;
        amplitude *= 0.55;
    }
    float2 n = w * 2600.0 + t * along * 1.3;
    float e = 0.15;
    float chop_x = value_noise(n + float2(e, 0.0)) - value_noise(n - float2(e, 0.0));
    float chop_y = value_noise(n + float2(0.0, e)) - value_noise(n - float2(0.0, e));
    slope += float2(chop_x, chop_y) * 0.06;
    // Far away the waves are finer than a pixel: flatten them into a mirror.
    slope *= 0.06 / (1.0 + distance * 18.0);
    return normalize(float3(-slope, 1.0));
}

// ---------------------------------------------------------------- the chart

float2 latitude_longitude_of(float2 w) {
    float2 uv = (w / WORLD_HALF_EXTENT) * 0.5 + 0.5;
    return float2(lerp(LATITUDE_SOUTH, LATITUDE_NORTH, uv.y), lerp(LONGITUDE_WEST, LONGITUDE_EAST, uv.x));
}

float3 chart_colour(float2 w, float4 relief, float footprint) {
    float depth_m = -relief.r * 1000.0;
    float3 colour;
    if (relief.g > 0.5) {
        colour = TATO_INK * 1.4 + TATO_CREAM * 0.08 * saturate(relief.b * 2.0);
    } else {
        float3 shoal = lerp(TATO_MINT, TATO_CREAM, 0.25);
        float3 shelf = TATO_BLUE * 0.85;
        float3 slope = lerp(TATO_BLUE, DEEP_BAND, 0.6);
        float3 deep = DEEP_BAND * 0.8;
        colour = lerp(shoal, shelf, smoothstep(2.0, 40.0, depth_m));
        colour = lerp(colour, slope, smoothstep(40.0, 400.0, depth_m));
        colour = lerp(colour, deep, smoothstep(400.0, 2200.0, depth_m));
    }
    // Hillshade from the relief's gradient, lit from the north-west.
    float step_world = 0.004;
    float east = relief_at_world(w + float2(step_world, 0.0)).r - relief_at_world(w - float2(step_world, 0.0)).r;
    float north = relief_at_world(w + float2(0.0, step_world)).r - relief_at_world(w - float2(0.0, step_world)).r;
    float3 normal = normalize(float3(-east * 60.0, -north * 60.0, 1.0));
    float shade = saturate(dot(normal, normalize(float3(-0.6, 0.6, 0.55))));
    colour *= 0.55 + 0.75 * shade;
    // The coastline, crisp and white.
    float coast_km = relief.a;
    float coast_width_km = max(0.08, footprint * 333.0 * 1.2);
    colour = lerp(colour, TATO_CREAM, 1.0 - smoothstep(coast_width_km * 0.5, coast_width_km, coast_km));
    // The half-degree graticule.
    float2 degrees = latitude_longitude_of(w) * 2.0;
    float2 to_line = abs(frac(degrees + 0.5) - 0.5) / 2.0;
    float per_world_degree = (LATITUDE_NORTH - LATITUDE_SOUTH) / (2.0 * WORLD_HALF_EXTENT);
    float line_width = max(footprint * 2.0 * WORLD_HALF_EXTENT * per_world_degree * 1.3, 0.0004);
    float grid = 1.0 - smoothstep(line_width * 0.5, line_width, min(to_line.x, to_line.y));
    colour = lerp(colour, TATO_CREAM, grid * 0.3);
    return colour * 0.9;
}

// ---------------------------------------------------------------- the frame

[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (any(float2(id.xy) >= dials.frame_size)) { return; }
    float2 pixel = float2(id.xy) + 0.5;
    float2 ndc = (pixel - 0.5 * dials.frame_size) / (0.5 * dials.frame_size.y);
    ndc.y = -ndc.y;
    float3 origin = dials.camera_position;
    float3 direction = normalize(dials.forward * dials.focal + dials.right * ndc.x + dials.up * ndc.y);
    float pixel_angle = 2.0 / (dials.frame_size.y * dials.focal);

    float sea_distance = direction.z < -1e-5 ? origin.z / -direction.z : SKY_DISTANCE;
    SkylineHit tower_hit = march_skyline(origin, direction, min(sea_distance, 6.0), 160);

    float3 colour;
    float distance;
    if (tower_hit.hit) {
        distance = tower_hit.distance;
        colour = shade_tower(tower_hit, direction);
        float haze = 1.0 - exp(-distance * 0.5);
        colour = lerp(colour, sky_colour(float3(direction.xy, 0.0), false) * 0.8, haze * 0.7);
    } else if (direction.z < -1e-5) {
        distance = sea_distance;
        float2 hit = origin.xy + direction.xy * distance;
        float grazing = max(-direction.z, 0.03);
        float footprint = distance * pixel_angle / grazing / (2.0 * WORLD_HALF_EXTENT);
        float inside = 1.0 - smoothstep(WORLD_HALF_EXTENT * 0.9, WORLD_HALF_EXTENT, max(abs(hit.x), abs(hit.y)));
        float2 uv = world_to_uv(hit);
        float4 relief = relief_layer.SampleLevel(uv, 0.0);
        float3 emitted = layer_at(uv, footprint) * inside * (0.7 + 0.5 * dials.time_of_day);

        float3 neon;
        if (relief.g > 0.5) {
            neon = emitted;
        } else {
            float3 normal = wave_normal(hit, distance);
            float3 reflected = reflect(direction, normal);
            reflected.z = abs(reflected.z);
            float cos_theta = saturate(dot(-direction, normal));
            float fresnel = 0.02 + 0.98 * pow(1.0 - cos_theta, 5.0);
            // Ray-marched reflections: the skyline mirrored in the water, else the sky.
            float3 mirrored;
            SkylineHit reflected_hit = march_skyline(float3(hit, 0.0002), reflected, 2.5, 96);
            if (reflected_hit.hit) {
                mirrored = shade_tower(reflected_hit, reflected);
            } else {
                mirrored = sky_colour(reflected, false);
            }
            // The sun's glint: a hot specular path that breaks on every wave.
            float glint = pow(saturate(dot(reflected, dials.sun_direction)), 900.0) * 60.0
                        + pow(saturate(dot(reflected, dials.sun_direction)), 60.0) * 1.2;
            float3 sun_tint = lerp(TATO_PINK, TATO_YELLOW, 0.55);
            mirrored += sun_tint * glint * smoothstep(-0.06, 0.05, dials.sun_direction.z);
            neon = emitted * (1.0 - 0.6 * saturate(fresnel)) + mirrored * saturate(fresnel * 0.75 + 0.04);
        }
        float haze = 1.0 - exp(-distance * 0.45);
        float3 horizon_haze = sky_colour(float3(direction.xy, 0.0), false);
        neon = lerp(neon, horizon_haze, smoothstep(0.35, 1.0, haze));

        // The reveal: a scan sweeps north and the neon becomes the real chart.
        float sweep = lerp(-WORLD_HALF_EXTENT - 0.4, WORLD_HALF_EXTENT + 0.4, dials.reveal);
        float charted = smoothstep(sweep + 0.12, sweep - 0.12, hit.y) * step(0.001, dials.reveal);
        float3 chart = chart_colour(hit, relief, footprint) * inside;
        float3 trails = current_layer.SampleLevel(uv, 0.0).rgb;
        chart += TATO_CREAM * dot(trails, float3(0.3, 0.4, 0.3)) * 2.2;
        colour = lerp(neon, chart, charted);
        float front = exp(-pow((hit.y - sweep) / 0.03, 2.0)) * step(0.001, dials.reveal) * step(dials.reveal, 0.999);
        colour += TATO_YELLOW * front * 6.0 * inside;
    } else {
        distance = SKY_DISTANCE;
        colour = sky_colour(direction, true);
    }

    scene[id.xy] = float4(colour * dials.exposure, distance);
}
"""
for _name, _value in (
    ("MAP_LATITUDE_SOUTH", LATITUDE_SOUTH),
    ("MAP_LATITUDE_NORTH", LATITUDE_NORTH),
    ("MAP_LONGITUDE_WEST", LONGITUDE_WEST),
    ("MAP_LONGITUDE_EAST", LONGITUDE_EAST),
):
    RENDERER_SLANG = RENDERER_SLANG.replace(_name, repr(float(_value)))

RENDERER_DIALS = struct.Struct("<ff ff 3ff 3ff 3ff 3ff 3ff ffff")
LAYER_USAGE = ["storage_binding", "texture_binding"]


@dataclass
class NeonHorizonRendererConfig:
    """NeonHorizonRenderer's settings: the picture's extent and lens, and the skyline's height.

    In `take` mode the take's choreography flies the camera; in `stage` mode the
    stage view steers it, and `time_of_day` (0 golden hour to 1 night), `morph`
    (0 the real chart to 1 neon) and `glow` are the stage's sliders, set live.
    """

    width: int = 1080
    height: int = 1920
    vertical_field_of_view_degrees: float = 62.0
    exposure: Annotated[float, NumberRange(minimum=0.05, maximum=2.0)] = 0.5
    skyline_height: Annotated[float, NumberRange(minimum=0.0, maximum=0.15)] = 0.055
    mode: str = "take"
    time_of_day: Annotated[float, NumberRange(minimum=0.0, maximum=1.0)] = 0.35
    morph: Annotated[float, NumberRange(minimum=0.0, maximum=1.0)] = 1.0
    glow: Annotated[float, NumberRange(minimum=0.0, maximum=3.0)] = 1.0


@node.meta(
    category="Gulf Stream Neon",
    description="The HDR picture: a raymarched Art Deco skyline on the real coast, mirror water with ray-marched reflections, the synthwave sky, and the reveal into the real chart. Distance rides in alpha.",
)
@node.process
class NeonHorizonRenderer:
    """The camera over the real map: skyline, sea, sky and the chart reveal, in HDR."""

    def __init__(self, config: NeonHorizonRendererConfig) -> None:
        self.config = config

    contours = Input(Image, delivery="newest", description="From BathymetryRelief")
    relief = Input(Image, delivery="newest", description="From BathymetryRelief")
    trails = Input(Image, delivery="newest", description="From GulfStreamCurrent")
    soundtrack = Input(Records, delivery="newest", description="beat, energy: from SoundtrackPulse")
    station = Input(Records, delivery="newest", description="wind: from NoaaStationFeed")
    stage = Input(Records, delivery="newest", description="The stage view's camera: orbit, zoom and fly")
    scene = Output(Image, description="The HDR picture, rgba16_float, distance in alpha")

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.take_clock = TakeClock()
        self.kernel = ctx.gpu_full_access.create_compute_kernel(
            source=RENDERER_SLANG, push_constant_size=RENDERER_DIALS.size
        )
        self.scene_textures = NodeOutputTextureRing(HDR_LAYER_FORMAT, LAYER_USAGE)
        self.focal = 1.0 / math.tan(math.radians(self.config.vertical_field_of_view_degrees) / 2.0)
        self.latest = {"contours": None, "relief": None, "trails": None}
        self.latest_soundtrack: dict = {}
        self.latest_station: dict = {}
        self.latest_stage: dict = {}
        self.last_rendered_ns: int | None = None

    def configure(self, config: NeonHorizonRendererConfig) -> None:
        """A setting changed live: the stage's sliders land here."""
        self.config = config
        self.focal = 1.0 / math.tan(math.radians(config.vertical_field_of_view_degrees) / 2.0)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        for name in ("contours", "relief", "trails"):
            frame = ctx.inputs.read(getattr(self, name))
            if frame is not None:
                self.latest[name] = frame
        soundtrack = ctx.inputs.read(self.soundtrack)
        if soundtrack is not None:
            self.latest_soundtrack = soundtrack
        station = ctx.inputs.read(self.station)
        if station is not None:
            self.latest_station = station
        stage = ctx.inputs.read(self.stage)
        if stage is not None:
            self.latest_stage = stage
        if any(frame is None for frame in self.latest.values()):
            return
        frame_ns = self.latest["trails"].timestamp_ns
        if frame_ns == self.last_rendered_ns:
            return
        # On a stepped take every layer of a step lands before it is drawn; live, the newest of each will do.
        if ctx.stream_clock.mode == "stepped" and self.latest["contours"].timestamp_ns != frame_ns:
            return
        self.last_rendered_ns = frame_ns

        width, height = self.config.width, self.config.height
        seconds = self.take_clock.seconds_at(ctx, frame_ns)
        on_stage = self.config.mode == "stage"
        if on_stage:
            moment = stage_moment(self.latest_stage, self.config.time_of_day, 1.0 - self.config.morph)
        else:
            moment = take_moment_at(seconds, width / height)
        forward, right, up = camera_basis(
            moment.camera_yaw_degrees, moment.camera_pitch_degrees, moment.camera_roll_degrees
        )
        beat = float(self.latest_soundtrack.get("beat", 0.0))
        energy = float(self.latest_soundtrack.get("energy", 0.0))
        wind_speed = float(self.latest_station.get("wind_speed_mps", 5.0))
        wind_from = math.radians(float(self.latest_station.get("wind_from_degrees", 110.0)))
        gpu = ctx.gpu_limited_access
        texture = self.scene_textures.next_texture_for_this_frame(gpu, width, height)
        self.kernel.dispatch(
            bindings={
                "contour_layer": self.latest["contours"].surface_id,
                "current_layer": self.latest["trails"].surface_id,
                "relief_layer": self.latest["relief"].surface_id,
                "scene": texture,
            },
            group_count=((width + 7) // 8, (height + 7) // 8, 1),
            push_constants=RENDERER_DIALS.pack(
                width,
                height,
                seconds,
                moment.time_of_day,
                moment.camera_x,
                moment.camera_y,
                moment.camera_altitude,
                self.focal,
                *forward,
                self.config.glow * (1.0 + 0.4 * moment.drop + 0.3 * energy),
                *right,
                beat,
                *up,
                moment.reveal,
                *sun_direction(moment.time_of_day),
                moment.drop,
                -wind_speed * math.sin(wind_from),
                -wind_speed * math.cos(wind_from),
                self.config.skyline_height,
                self.config.exposure,
            ),
        )
        ctx.outputs.write(
            self.scene,
            ImageFrameToWrite.of_texture(texture, RENDERED_FRAME_COLOUR),
            timestamp_ns=frame_ns,
        )
