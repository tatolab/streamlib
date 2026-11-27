# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Real readings from NOAA's Virginia Key tide station, as one bundle each.

Importable as `nodes.noaa_station:NoaaStationFeed`. In `live` mode it polls
the NOAA CO-OPS data API for station 8723214's latest water level, wind,
temperatures and pressure, appends every reading to `data/station_log.jsonl`
and publishes the newest. In `replay` mode, the one a take is recorded in, it
plays back the logged 24-hour snapshot in `data/virginia_key_snapshot.json`,
time-lapsed across the take, so every take reads the same real numbers.
"""

from __future__ import annotations

import calendar
import dataclasses
import json
import threading
import time
import urllib.parse
import urllib.request
from typing import Annotated, Callable, Literal

from tatolab.stream import (
    Description,
    Input,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    computed,
    log,
    node,
)

from nodes.neon_world import DATA_FOLDER, StationReading, TakeClock

STATION_ID = "8723214"
API = "https://api.tidesandcurrents.noaa.gov/api/prod/datagetter"
PRODUCTS = ("water_level", "wind", "air_temperature", "water_temperature", "air_pressure")
STATION_READING_NAMES = frozenset(field.name for field in dataclasses.fields(StationReading))


def _unix_of(stamp: str) -> float:
    return float(calendar.timegm(time.strptime(stamp, "%Y-%m-%d %H:%M")))


def _reading_of(product: str, row: dict) -> dict:
    if product == "wind":
        return {"wind_speed_mps": float(row["s"]), "wind_from_degrees": float(row["d"]), "wind_gust_mps": float(row["g"])}
    key = {
        "water_level": "water_level_m",
        "air_temperature": "air_temperature_c",
        "water_temperature": "water_temperature_c",
        "air_pressure": "air_pressure_hpa",
    }[product]
    return {key: float(row["v"])}


def _timeline_of(responses: dict) -> list[dict]:
    """The readings of every product merged by time: one row per six-minute stamp."""
    by_stamp: dict[str, dict] = {}
    for product, response in responses.items():
        for row in response.get("data", []):
            try:
                by_stamp.setdefault(row["t"], {"observed_at_unix": _unix_of(row["t"])}).update(_reading_of(product, row))
            except (KeyError, ValueError):
                continue
    timeline = [by_stamp[stamp] for stamp in sorted(by_stamp)]
    held: dict = {}
    for row in timeline:
        held.update(row)
        row.update(held)
    return timeline


def fetch_latest() -> dict:
    """One live reading of every product, merged."""
    reading: dict = {}
    for product in PRODUCTS:
        query = {
            "date": "latest",
            "station": STATION_ID,
            "product": product,
            "units": "metric",
            "time_zone": "gmt",
            "format": "json",
            "application": "gulf_stream_neon",
        }
        if product == "water_level":
            query["datum"] = "MLLW"
        with urllib.request.urlopen(f"{API}?{urllib.parse.urlencode(query)}", timeout=15) as answer:
            rows = json.load(answer).get("data", [])
        if rows:
            reading.update(_reading_of(product, rows[-1]))
            reading["observed_at_unix"] = max(reading.get("observed_at_unix", 0.0), _unix_of(rows[-1]["t"]))
    return reading


class LiveStationPoller:
    """A thread that asks NOAA for the station's newest reading every `poll_seconds`, and
    stops when closed."""

    def __init__(self, poll_seconds: float) -> None:
        self.latest: dict | None = None
        self._poll_seconds = poll_seconds
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._poll, daemon=True)
        self._thread.start()

    def _poll(self) -> None:
        log_path = DATA_FOLDER / "station_log.jsonl"
        while not self._stop.is_set():
            try:
                reading = fetch_latest()
                self.latest = reading
                with log_path.open("a") as station_log:
                    station_log.write(json.dumps({"fetched_at_unix": time.time(), **reading}) + "\n")
                log.info("station reading", **reading)
            except Exception as failure:  # noqa: BLE001 - a feed that is down keeps the last reading
                log.warn("station unreachable, holding the last reading", reason=str(failure))
            self._stop.wait(self._poll_seconds)

    def close(self) -> None:
        self._stop.set()


@node.meta(
    category="Gulf Stream Neon",
    description="Real readings from NOAA tide station 8723214 Virginia Key, Miami: water level, wind, temperatures. Live from the CO-OPS API, or replayed from a logged snapshot.",
)
@node(execution="continuous", interval_ms=16)
def NoaaStationFeed(
    *,
    mode: Input[
        Annotated[
            Literal["replay", "live"],
            Description("`live` polls NOAA; `replay` plays the logged snapshot."),
        ]
    ] = Input("replay"),
    poll_seconds: Input[
        Annotated[
            float, NumberRange(minimum=1.0), Description("How often `live` asks NOAA, in seconds.")
        ]
    ] = Input(360.0),
    replay_hours_per_take: Input[
        Annotated[
            float,
            NumberRange(minimum=0.0, maximum=24.0),
            Description("How many of the snapshot's last hours one take replays."),
        ]
    ] = Input(24.0),
    take_seconds: Input[
        Annotated[float, NumberRange(minimum=0.1), Description("How long one take is, in seconds.")]
    ] = Input(24.0),
    station: OutputPin[Annotated[StationReading, Description("The station's newest reading")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """NOAA CO-OPS station 8723214, live or replayed.

    Switched live, a poller starts asking NOAA; switched to replay, it stops.
    """
    take_clock = TakeClock()
    snapshot = json.loads((DATA_FOLDER / "virginia_key_snapshot.json").read_text())
    responses = {product: block["response"] for product, block in snapshot["last_24h"].items()}
    timeline = _timeline_of(responses)
    poller = computed(
        lambda: LiveStationPoller(poll_seconds.get()) if mode.get() == "live" else None
    )
    log.info(
        "station feed",
        readings=len(timeline),
        snapshot_fetched_at=snapshot["fetched_at_utc"],
    )

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        live_poller = poller.get()
        if live_poller is not None and live_poller.latest is not None:
            reading, live = live_poller.latest, 1.0
        else:
            seconds = take_clock.seconds_at(ctx)
            fraction = (max(seconds, 0.0) / take_seconds.get()) % 1.0
            span = replay_hours_per_take.get() / 24.0
            start = 1.0 - span
            index = int((start + fraction * span) * (len(timeline) - 1))
            reading, live = timeline[index], 0.0
        values = {k: float(v) for k, v in reading.items() if k in STATION_READING_NAMES}
        values.update(station=float(STATION_ID), live=live)
        station.write(StationReading(**values), timestamp_ns=ctx.time)

    return each_frame
