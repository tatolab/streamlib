# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What every Gulf Stream Neon node agrees on: the map, its palette and the take.

The world is a real square of ocean off Miami, 24.30-27.30 N by 81.00-77.67 W,
about 333 km a side, mapped onto world units from -2.5 to 2.5 with x east and
y north. The seafloor and the current are drawn into world-space textures over
that square, so the renderer can fly one camera over both. The take is the
camera's choreography and the sky's time of day, a pure function of stream
time, so every node of a step reads the same moment.
"""

from __future__ import annotations

import functools
import math
from dataclasses import dataclass
from pathlib import Path

import numpy
from tatolab.stream import ColorInfo

PROJECT_FOLDER = Path(__file__).resolve().parent.parent
DATA_FOLDER = PROJECT_FOLDER / "data"

# The world-space layers' extent in texels, and the square of world they cover.
WORLD_TEXTURE_SIZE = 2048
WORLD_HALF_EXTENT = 2.5

# The square of the real map the world covers.
LATITUDE_SOUTH = 24.30
LATITUDE_NORTH = 27.30
LONGITUDE_WEST = -81.00
LONGITUDE_EAST = -77.67
METRES_PER_WORLD_UNIT = 333_000.0 / (2.0 * WORLD_HALF_EXTENT)

NANOSECONDS_PER_SECOND = 1_000_000_000

RENDERED_FRAME_COLOUR = ColorInfo(primaries="bt709", transfer="srgb", range="full")
HDR_LAYER_FORMAT = "rgba16_float"


def world_of(longitude: float, latitude: float) -> tuple[float, float]:
    """A longitude and latitude in degrees, as world x (east) and y (north)."""
    u = (longitude - LONGITUDE_WEST) / (LONGITUDE_EAST - LONGITUDE_WEST)
    v = (latitude - LATITUDE_SOUTH) / (LATITUDE_NORTH - LATITUDE_SOUTH)
    return (u * 2.0 - 1.0) * WORLD_HALF_EXTENT, (v * 2.0 - 1.0) * WORLD_HALF_EXTENT


# Shared Slang: the palette, hashing, noise and the map's world-to-texture mapping.
WORLD_SLANG = """
static const float WORLD_HALF_EXTENT = 2.5;
static const float PI = 3.14159265;

// The Tato inks, from tatolab.com's tokens, and the home page's deep band.
static const float3 TATO_BLUE = float3(0.165, 0.357, 1.0);
static const float3 TATO_PINK = float3(1.0, 0.239, 0.545);
static const float3 TATO_YELLOW = float3(1.0, 0.839, 0.039);
static const float3 TATO_CREAM = float3(1.0, 0.957, 0.878);
static const float3 TATO_INK = float3(0.078, 0.063, 0.122);
static const float3 TATO_MINT = float3(0.184, 0.839, 0.627);
static const float3 TATO_VIOLET = float3(0.357, 0.180, 0.902);
static const float3 DEEP_BAND = float3(0.051, 0.102, 0.290);

float hash21(float2 p) {
    float3 q = frac(float3(p.xyx) * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return frac((q.x + q.y) * q.z);
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

float world_to_uv_component(float w) {
    return (w + WORLD_HALF_EXTENT) / (2.0 * WORLD_HALF_EXTENT);
}

// Texture row 0 is the north edge, column 0 the west edge.
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


@functools.lru_cache(maxsize=1)
def stream_core_path() -> tuple[numpy.ndarray, numpy.ndarray]:
    """The Gulf Stream's core in world units: for each world y, the x of the fastest water.

    Read from the surface-current snapshot in `data/`: along each latitude row,
    the longitude of the strongest northward flow in the Straits, smoothed.
    """
    snapshot = numpy.load(DATA_FOLDER / "surface_currents.npz")
    latitudes = snapshot["lat"].astype(numpy.float64)
    longitudes = snapshot["lon"].astype(numpy.float64)
    northward = numpy.nan_to_num(snapshot["v"].astype(numpy.float64), nan=-9.0)
    in_straits = (longitudes > -80.4) & (longitudes < -78.9)
    columns = numpy.where(in_straits)[0]
    core_longitudes = longitudes[columns[numpy.argmax(northward[:, columns], axis=1)]]
    kernel = numpy.ones(9) / 9.0
    padded = numpy.pad(core_longitudes, 4, mode="edge")
    smoothed = numpy.convolve(padded, kernel, mode="valid")
    xs = numpy.array([world_of(lon, 0.0)[0] for lon in smoothed])
    ys = numpy.array([world_of(LONGITUDE_WEST, lat)[1] for lat in latitudes])
    order = numpy.argsort(ys)
    return ys[order], xs[order]


def stream_core_x(y: float) -> float:
    ys, xs = stream_core_path()
    return float(numpy.interp(y, ys, xs))


@dataclass(frozen=True)
class TakeMoment:
    """One instant of the take: where the camera is, what the sky does, and how far the reveal is."""

    camera_x: float
    camera_y: float
    camera_altitude: float
    camera_pitch_degrees: float
    camera_yaw_degrees: float
    camera_roll_degrees: float
    time_of_day: float
    current_speed: float
    drop: float
    reveal: float
    focus_distance: float


MIAMI = world_of(-80.19, 25.77)
MIAMI_BEACH = world_of(-80.13, 25.80)
OVERHEAD_CENTRE = world_of(-79.95, 25.80)
OVERHEAD_CENTRE_TALL = world_of(-79.73, 25.62)


def take_moment_at(seconds: float, aspect: float = 9.0 / 16.0) -> TakeMoment:
    """The take, bar by bar at 120 BPM (a bar is 2 s).

    0-2 s inside the stream, skimming its core, then a pull-back; 2-4 s a low
    golden drift past the Miami skyline; 4-5 s a rise straight up; 5-8.6 s the
    reveal, the real map from above; 8.6-10 s the dive, as the sun goes down;
    10-20 s the drop, low and fast up the stream, banking; 20-24 s a rise into
    the night, turning back to the city lights for the end. `aspect` is the frame's width over its height: a
    wide frame needs less height to hold the map.
    """
    t = seconds
    pull_back = smoothstep(0.0, 2.2, t)
    drift = smoothstep(1.6, 2.6, t) * (1.0 - smoothstep(3.9, 4.6, t))
    rise = smoothstep(3.9, 5.0, t) * (1.0 - smoothstep(8.5, 9.8, t))
    drop = smoothstep(9.85, 10.1, t) * (1.0 - smoothstep(19.6, 21.5, t))
    finale = smoothstep(19.6, 23.5, t)

    # Inside the stream: low over the core, rushing north.
    travel_y = -1.25 + 0.42 * t - 0.25 * pull_back
    camera_y = travel_y
    camera_x = stream_core_x(camera_y + 0.15) + lerp(0.0, -0.03, pull_back)
    altitude = lerp(0.03, 0.14, pull_back * pull_back)
    pitch = lerp(5.0, 11.0, pull_back)
    yaw = lerp(-12.0, -20.0, pull_back)

    # The golden drift: slow, between the stream and the beach, the skyline ahead and left.
    drift_y = lerp(-0.42, -0.30, smoothstep(1.6, 4.6, t))
    camera_x = lerp(camera_x, lerp(MIAMI_BEACH[0] + 0.10, MIAMI_BEACH[0] + 0.075, smoothstep(2.0, 4.0, t)), drift)
    camera_y = lerp(camera_y, drift_y, drift)
    altitude = lerp(altitude, 0.016, drift)
    pitch = lerp(pitch, 2.5, drift)
    yaw = lerp(yaw, lerp(-30.0, -20.0, smoothstep(2.0, 4.0, t)), drift)

    # The dive and the drop: down to the water, then fast and low up the core.
    drop_y = -0.95 + 0.16 * (t - 9.0)
    run_x = stream_core_x(drop_y + 0.3) - 0.05 - 0.12 * smoothstep(12.0, 15.0, t) * (1.0 - smoothstep(16.5, 19.0, t))
    dive = smoothstep(8.6, 10.0, t)
    camera_x = lerp(camera_x, run_x + 0.03 * math.sin(t * 0.9), dive)
    camera_y = lerp(camera_y, drop_y, dive)
    altitude = lerp(altitude, 0.10 + 0.025 * math.sin(t * 0.7), dive)
    pitch = lerp(pitch, 13.0, dive)
    yaw = lerp(yaw, -6.0 + 10.0 * math.sin(0.4 * t), dive)

    # The finale: up into the night, looking back down the coast.
    camera_x = lerp(camera_x, MIAMI[0] + 0.62, finale)
    altitude = lerp(altitude, 0.32, finale)
    pitch = lerp(pitch, 15.0, finale)
    yaw = lerp(yaw, -150.0, finale)

    # The reveal: straight up over the Straits until the map holds the frame.
    half_height_needed = 1.5 if aspect < 1.0 else 0.80
    overhead_centre = OVERHEAD_CENTRE_TALL if aspect < 1.0 else OVERHEAD_CENTRE
    overhead_altitude = half_height_needed / math.tan(math.radians(31.0 if aspect < 1.0 else 24.0))
    hold = smoothstep(5.0, 8.6, t)
    camera_x = lerp(camera_x, overhead_centre[0], rise)
    camera_y = lerp(camera_y, overhead_centre[1] + 0.05 * hold, rise)
    altitude = lerp(altitude, overhead_altitude * lerp(1.0, 0.9, hold), rise * rise)
    pitch = lerp(pitch, 89.9, smoothstep(4.0, 5.0, t) * (1.0 - smoothstep(8.6, 9.6, t)))
    yaw = lerp(yaw, 0.0, rise)

    roll = 6.0 * math.sin(t * 0.9 + 0.5) * drop
    reveal = smoothstep(5.05, 5.9, t) * (1.0 - smoothstep(8.45, 8.85, t))

    time_of_day = 0.10 + 0.90 * smoothstep(7.5, 10.0, t)
    speed = 1.0 + 0.4 * smoothstep(8.0, 10.0, t) + 0.8 * drop
    focus = lerp(0.05, 0.6, pull_back)
    focus = lerp(focus, 0.42, drift)
    focus = lerp(focus, 9.0, rise)
    focus = lerp(focus, 0.35, drop)
    return TakeMoment(
        camera_x=camera_x,
        camera_y=camera_y,
        camera_altitude=altitude,
        camera_pitch_degrees=pitch,
        camera_yaw_degrees=yaw,
        camera_roll_degrees=roll,
        time_of_day=time_of_day,
        current_speed=speed,
        drop=drop,
        reveal=reveal,
        focus_distance=focus,
    )


# Where the stage view's camera starts: low over the Straits, Miami Beach ahead and left.
STAGE_HOME = {
    "target_x": world_of(-80.02, 25.86)[0],
    "target_y": world_of(-80.02, 25.86)[1],
    "distance": 0.32,
    "yaw_degrees": -22.0,
    "pitch_degrees": 16.0,
}


@dataclass(frozen=True)
class StageCamera:
    """The stage view's camera: the point it orbits (`target_x`, `target_y`, in world units),
    how far away (`distance`), the heading and tilt (`yaw_degrees` from north, `pitch_degrees`
    down), and how many eddies it has dropped (`bursts`) and where the last one fell."""

    target_x: float = STAGE_HOME["target_x"]
    target_y: float = STAGE_HOME["target_y"]
    distance: float = STAGE_HOME["distance"]
    yaw_degrees: float = STAGE_HOME["yaw_degrees"]
    pitch_degrees: float = STAGE_HOME["pitch_degrees"]
    bursts: int = 0
    burst_x: float = 0.0
    burst_y: float = 0.0


#: The stage's camera at home, before the hand has moved it.
STAGE_CAMERA_AT_HOME = StageCamera()


@dataclass(frozen=True)
class Look:
    """The stage's look: the real map to neon morph, the time of day, the skyline, the neon and
    the current's speed."""

    morph: float = 0.0
    time_of_day: float = 0.3
    skyline_height: float = 0.055
    neon: float = 1.0
    current_speed: float = 1.0


@dataclass(frozen=True)
class SoundtrackPulseReading:
    """The soundtrack at one step: the kick's envelope and the loudness, 0 to 1, and the glow
    the loudness lends the contours."""

    beat: float = 0.0
    energy: float = 0.0
    glow: float = 1.0


@dataclass(frozen=True)
class StationReading:
    """One reading of a NOAA station, each value 0 where the station gave none, and `live` 1
    when it came from the station's live feed rather than the replayed snapshot."""

    station: float = 0.0
    observed_at_unix: float = 0.0
    water_level_m: float = 0.0
    wind_speed_mps: float = 0.0
    wind_from_degrees: float = 0.0
    wind_gust_mps: float = 0.0
    air_temperature_c: float = 0.0
    water_temperature_c: float = 0.0
    air_pressure_hpa: float = 0.0
    live: float = 0.0


def stage_moment(stage: StageCamera, time_of_day: float, morph: float) -> TakeMoment:
    """The camera the stage view steers: an orbit around `stage`'s point on the sea."""
    view = {key: getattr(stage, key) for key in STAGE_HOME}
    pitch = min(max(view["pitch_degrees"], 2.0), 89.9)
    distance = min(max(view["distance"], 0.02), 6.0)
    forward, _right, _up = camera_basis(view["yaw_degrees"], pitch)
    return TakeMoment(
        camera_x=view["target_x"] - forward[0] * distance,
        camera_y=view["target_y"] - forward[1] * distance,
        camera_altitude=max(0.006, -forward[2] * distance),
        camera_pitch_degrees=pitch,
        camera_yaw_degrees=view["yaw_degrees"],
        camera_roll_degrees=0.0,
        time_of_day=min(max(time_of_day, 0.0), 1.0),
        current_speed=1.0,
        drop=0.0,
        reveal=min(max(morph, 0.0), 1.0),
        focus_distance=distance,
    )


def frame_point_on_the_sea(moment: TakeMoment, u: float, v: float, aspect: float, vertical_fov_degrees: float):
    """The world point on the sea seen at frame position (u, v), v down, or None above the horizon."""
    forward, right, up = camera_basis(
        moment.camera_yaw_degrees, moment.camera_pitch_degrees, moment.camera_roll_degrees
    )
    focal = 1.0 / math.tan(math.radians(vertical_fov_degrees) / 2.0)
    x = (u - 0.5) * 2.0 * aspect
    y = (0.5 - v) * 2.0
    direction = [forward[i] * focal + right[i] * x + up[i] * y for i in range(3)]
    if direction[2] >= -1e-6:
        return None
    t = moment.camera_altitude / -direction[2]
    return moment.camera_x + direction[0] * t, moment.camera_y + direction[1] * t


def camera_basis(yaw_degrees: float, pitch_degrees: float, roll_degrees: float = 0.0):
    """Forward, right and up for a camera at `yaw` from north, pitched down by `pitch`."""
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


def sun_direction(time_of_day: float) -> tuple[float, float, float]:
    """Where the sun is: low in the north-west, sinking through the take."""
    elevation = lerp(0.16, -0.2, smoothstep(0.0, 0.95, time_of_day))
    x, y, z = -0.45, 0.89, elevation
    length = math.sqrt(x * x + y * y + z * z)
    return x / length, y / length, z / length


def project_to_frame(moment: TakeMoment, point: tuple[float, float, float], aspect: float, vertical_fov_degrees: float):
    """Where a world point lands in the frame, as (u, v) from 0 to 1 with v down, or None behind the camera."""
    forward, right, up = camera_basis(
        moment.camera_yaw_degrees, moment.camera_pitch_degrees, moment.camera_roll_degrees
    )
    relative = (
        point[0] - moment.camera_x,
        point[1] - moment.camera_y,
        point[2] - moment.camera_altitude,
    )
    depth = sum(a * b for a, b in zip(relative, forward))
    if depth <= 1e-6:
        return None
    focal = 1.0 / math.tan(math.radians(vertical_fov_degrees) / 2.0)
    x = sum(a * b for a, b in zip(relative, right)) / depth * focal
    y = sum(a * b for a, b in zip(relative, up)) / depth * focal
    return 0.5 + 0.5 * x / aspect, 0.5 - 0.5 * y


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
