# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The real ocean under the neon: bathymetry and surface currents, baked onto the world grid.

`data/bathymetry.npz` is NOAA NCEI's ETOPO 2022 elevation over the map's
square, and `data/surface_currents.npz` a surface-current snapshot of the
same square (`data/sources.json` names both). Both are resampled here onto the
world textures' texel grid, row 0 north and column 0 west, so a compute
kernel can copy them straight into a texture.
"""

from __future__ import annotations

import functools

import numpy

from nodes.neon_world import (
    DATA_FOLDER,
    LATITUDE_NORTH,
    LATITUDE_SOUTH,
    LONGITUDE_EAST,
    LONGITUDE_WEST,
    world_of,
)

# The coastal cities the skyline rises in, by longitude and latitude, and how big each is.
SKYLINE_CITIES = (
    ("Miami", -80.19, 25.77, 1.0),
    ("Miami Beach", -80.13, 25.80, 0.8),
    ("Brickell", -80.195, 25.76, 1.0),
    ("Fort Lauderdale", -80.12, 26.12, 0.75),
    ("Hollywood", -80.12, 26.01, 0.45),
    ("Boca Raton", -80.08, 26.36, 0.45),
    ("West Palm Beach", -80.05, 26.71, 0.55),
    ("Key Biscayne", -80.16, 25.69, 0.3),
)


def _bilinear(grid: numpy.ndarray, rows: numpy.ndarray, columns: numpy.ndarray) -> numpy.ndarray:
    """`grid` read at fractional `rows` and `columns`, clamped at its edges."""
    height, width = grid.shape
    rows = numpy.clip(rows, 0.0, height - 1.0)
    columns = numpy.clip(columns, 0.0, width - 1.0)
    r0 = numpy.floor(rows).astype(numpy.int64)
    c0 = numpy.floor(columns).astype(numpy.int64)
    r1 = numpy.minimum(r0 + 1, height - 1)
    c1 = numpy.minimum(c0 + 1, width - 1)
    fr = (rows - r0)[:, None]
    fc = (columns - c0)[None, :]
    top = grid[r0][:, c0] * (1.0 - fc) + grid[r0][:, c1] * fc
    bottom = grid[r1][:, c0] * (1.0 - fc) + grid[r1][:, c1] * fc
    return top * (1.0 - fr) + bottom * fr


def _texel_latitudes_and_longitudes(size: int) -> tuple[numpy.ndarray, numpy.ndarray]:
    centres = (numpy.arange(size) + 0.5) / size
    latitudes = LATITUDE_NORTH + (LATITUDE_SOUTH - LATITUDE_NORTH) * centres
    longitudes = LONGITUDE_WEST + (LONGITUDE_EAST - LONGITUDE_WEST) * centres
    return latitudes, longitudes


def _grid_coordinates(axis: numpy.ndarray, wanted: numpy.ndarray) -> numpy.ndarray:
    """The fractional index of each `wanted` value along a regular `axis`, either direction."""
    return (wanted - axis[0]) / (axis[-1] - axis[0]) * (len(axis) - 1)


def _distance_to_mask(mask: numpy.ndarray, steps: int) -> numpy.ndarray:
    """Texels from each texel to the nearest `True` of `mask`, up to `steps`, by repeated growth."""
    distance = numpy.full(mask.shape, float(steps), numpy.float32)
    grown = mask.copy()
    distance[grown] = 0.0
    for step in range(1, steps + 1):
        bigger = grown.copy()
        bigger[1:, :] |= grown[:-1, :]
        bigger[:-1, :] |= grown[1:, :]
        bigger[:, 1:] |= grown[:, :-1]
        bigger[:, :-1] |= grown[:, 1:]
        distance[bigger & ~grown] = float(step)
        grown = bigger
    return distance


@functools.lru_cache(maxsize=2)
def baked_relief(size: int) -> numpy.ndarray:
    """The seafloor on the world grid: (size * size, 4) float32 rows of
    elevation in km, land (0 or 1), city density (0 to 1) and distance to the coast in km.
    """
    bathymetry = numpy.load(DATA_FOLDER / "bathymetry.npz")
    elevation_grid = bathymetry["elevation_m"].astype(numpy.float32)
    latitudes, longitudes = _texel_latitudes_and_longitudes(size)
    rows = _grid_coordinates(bathymetry["lat"].astype(numpy.float64), latitudes)
    columns = _grid_coordinates(bathymetry["lon"].astype(numpy.float64), longitudes)
    elevation = _bilinear(elevation_grid, rows, columns).astype(numpy.float32)

    land = elevation > 0.0
    texel_km = 333.0 / size
    coast_steps = 48
    to_sea = _distance_to_mask(~land, coast_steps) * texel_km
    to_land = _distance_to_mask(land, coast_steps) * texel_km
    distance_to_coast = numpy.where(land, to_sea, to_land).astype(numpy.float32)

    city = numpy.zeros_like(elevation)
    lat_grid = latitudes[:, None]
    lon_grid = longitudes[None, :]
    km_per_degree_latitude = 111.0
    km_per_degree_longitude = 111.0 * numpy.cos(numpy.radians(25.8))
    for _name, longitude, latitude, size_of_city in SKYLINE_CITIES:
        dy = (lat_grid - latitude) * km_per_degree_latitude
        dx = (lon_grid - longitude) * km_per_degree_longitude
        radius = 4.0 + 7.0 * size_of_city
        city = numpy.maximum(city, size_of_city * numpy.exp(-(dx * dx + dy * dy) / (radius * radius)))
    # The skyline hugs the water: towers stand on land within a few km of the coast.
    city *= land * numpy.clip(1.0 - to_sea / 5.0, 0.0, 1.0)

    relief = numpy.stack(
        [elevation / 1000.0, land.astype(numpy.float32), city.astype(numpy.float32), distance_to_coast],
        axis=-1,
    )
    return numpy.ascontiguousarray(relief.reshape(size * size, 4), dtype=numpy.float32)


@functools.lru_cache(maxsize=2)
def baked_currents(size: int) -> numpy.ndarray:
    """The surface current on the world grid: (size * size, 4) float32 rows of
    eastward and northward speed in m/s, 1 where the snapshot has water, and the speed.
    """
    snapshot = numpy.load(DATA_FOLDER / "surface_currents.npz")
    u = snapshot["u"].astype(numpy.float32)
    v = snapshot["v"].astype(numpy.float32)
    water = numpy.isfinite(u) & numpy.isfinite(v)
    latitudes, longitudes = _texel_latitudes_and_longitudes(size)
    rows = _grid_coordinates(snapshot["lat"].astype(numpy.float64), latitudes)
    columns = _grid_coordinates(snapshot["lon"].astype(numpy.float64), longitudes)
    east = _bilinear(numpy.where(water, u, 0.0), rows, columns)
    north = _bilinear(numpy.where(water, v, 0.0), rows, columns)
    valid = _bilinear(water.astype(numpy.float32), rows, columns)
    speed = numpy.sqrt(east * east + north * north)
    field = numpy.stack([east, north, valid, speed], axis=-1).astype(numpy.float32)
    return numpy.ascontiguousarray(field.reshape(size * size, 4))


def snapshot_time() -> str:
    """When the surface-current snapshot is valid, as ISO 8601 UTC."""
    return str(numpy.load(DATA_FOLDER / "surface_currents.npz")["time_utc"])


def city_world_positions() -> list[tuple[str, float, float]]:
    return [(name, *world_of(longitude, latitude)) for name, longitude, latitude, _ in SKYLINE_CITIES]
