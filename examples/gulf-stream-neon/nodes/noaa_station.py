# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Real readings from NOAA's Virginia Key tide station, as records.

Importable as `nodes.noaa_station:NoaaStationFeed`. In `live` mode it polls
the NOAA CO-OPS data API for station 8723214's latest water level, wind,
temperatures and pressure, appends every reading to `data/station_log.jsonl`
and publishes the newest. In `replay` mode, the one a take is recorded in, it
plays back the logged 24-hour snapshot in `data/virginia_key_snapshot.json`,
time-lapsed across the take, so every take reads the same real numbers.
"""

from __future__ import annotations

import calendar
import json
import threading
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass

from tatolab.stream import Output, Records, RuntimeContextFullAccess, RuntimeContextLimitedAccess, log, node

from nodes.neon_world import DATA_FOLDER, TakeClock

STATION_ID = "8723214"
API = "https://api.tidesandcurrents.noaa.gov/api/prod/datagetter"
PRODUCTS = ("water_level", "wind", "air_temperature", "water_temperature", "air_pressure")
STATION_RECORD_NAMES = [
    "station",
    "observed_at_unix",
    "water_level_m",
    "wind_speed_mps",
    "wind_from_degrees",
    "wind_gust_mps",
    "air_temperature_c",
    "water_temperature_c",
    "air_pressure_hpa",
    "live",
]


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


@dataclass
class NoaaStationFeedConfig:
    """NoaaStationFeed's settings: `live` polls NOAA, `replay` plays the logged snapshot."""

    mode: str = "replay"
    poll_seconds: float = 360.0
    replay_hours_per_take: float = 24.0
    take_seconds: float = 24.0


@node.meta(
    category="Gulf Stream Neon",
    description="Real readings from NOAA tide station 8723214 Virginia Key, Miami: water level, wind, temperatures. Live from the CO-OPS API, or replayed from a logged snapshot.",
)
@node.process(execution="continuous", interval_ms=33)
class NoaaStationFeed:
    """NOAA CO-OPS station 8723214 as records, live or replayed."""

    def __init__(self, config: NoaaStationFeedConfig) -> None:
        self.config = config

    station = Output(Records, names=STATION_RECORD_NAMES, description="The station's newest reading")

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.take_clock = TakeClock()
        snapshot = json.loads((DATA_FOLDER / "virginia_key_snapshot.json").read_text())
        responses = {product: block["response"] for product, block in snapshot["last_24h"].items()}
        self.timeline = _timeline_of(responses)
        self.latest_live: dict | None = None
        self.stop = threading.Event()
        self.poller: threading.Thread | None = None
        if self.config.mode == "live":
            self.poller = threading.Thread(target=self._poll, daemon=True)
            self.poller.start()
        log.info(
            "station feed",
            mode=self.config.mode,
            readings=len(self.timeline),
            snapshot_fetched_at=snapshot["fetched_at_utc"],
        )

    def configure(self, config: NoaaStationFeedConfig) -> None:
        """Switched live: start polling NOAA; switched to replay: the poller stops."""
        self.config = config
        if config.mode == "live" and (self.poller is None or not self.poller.is_alive()):
            self.stop = threading.Event()
            self.poller = threading.Thread(target=self._poll, daemon=True)
            self.poller.start()
        elif config.mode != "live":
            self.stop.set()

    def _poll(self) -> None:
        log_path = DATA_FOLDER / "station_log.jsonl"
        while not self.stop.is_set():
            try:
                reading = fetch_latest()
                self.latest_live = reading
                with log_path.open("a") as station_log:
                    station_log.write(json.dumps({"fetched_at_unix": time.time(), **reading}) + "\n")
                log.info("station reading", **reading)
            except Exception as failure:  # noqa: BLE001 - a feed that is down keeps the last reading
                log.warn("station unreachable, holding the last reading", reason=str(failure))
            self.stop.wait(self.config.poll_seconds)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.config.mode == "live" and self.latest_live is not None:
            reading, live = self.latest_live, 1.0
        else:
            seconds = self.take_clock.seconds_at(ctx)
            fraction = (max(seconds, 0.0) / self.config.take_seconds) % 1.0
            span = self.config.replay_hours_per_take / 24.0
            start = 1.0 - span
            index = int((start + fraction * span) * (len(self.timeline) - 1))
            reading, live = self.timeline[index], 0.0
        record = {name: 0.0 for name in STATION_RECORD_NAMES}
        record.update({k: float(v) for k, v in reading.items() if k in record})
        record["station"] = float(STATION_ID)
        record["live"] = live
        ctx.outputs.write(self.station, record, timestamp_ns=ctx.time)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        self.stop.set()
