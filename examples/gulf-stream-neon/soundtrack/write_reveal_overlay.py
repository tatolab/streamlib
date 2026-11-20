# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Writes, frame by frame, where the reveal's labels land and what the station read.

    python write_reveal_overlay.py WIDTH HEIGHT FOV OUT.json

For each of the take's 720 frames: the take's reveal level, the frame
position of each labelled place (projected through the same camera the
renderer flies), and the NoaaStationFeed reading of that frame, replayed from
the logged snapshot exactly as the node plays it. The edit draws the labels
and the ticking readout from this, so they sit on the real map and show the
real numbers the stream carried.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

PROJECT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PROJECT))

from nodes.neon_world import project_to_frame, take_moment_at, world_of  # noqa: E402
from nodes.noaa_station import _timeline_of  # noqa: E402

FRAMES = 720
FPS = 30
PLACES = (
    ("MIAMI", -80.19, 25.77, "city"),
    ("MIAMI BEACH", -80.13, 25.82, "city"),
    ("FORT LAUDERDALE", -80.14, 26.12, "city"),
    ("KEY BISCAYNE", -80.16, 25.69, "place"),
    ("BIMINI", -79.28, 25.73, "place"),
    ("FLORIDA STRAITS", -79.75, 25.05, "water"),
    ("GULF STREAM", -79.88, 26.35, "stream"),
    ("NOAA 8723214 · VIRGINIA KEY", -80.1618, 25.7314, "station"),
    ("26°N", -80.62, 26.0, "grid"),
    ("25.5°N", -80.62, 25.5, "grid"),
    ("80°W", -80.0, 24.95, "grid"),
    ("79.5°W", -79.5, 24.95, "grid"),
)


def main(width: int, height: int, fov: float, out: Path) -> None:
    aspect = width / height
    snapshot = json.loads((PROJECT / "data" / "virginia_key_snapshot.json").read_text())
    timeline = _timeline_of({product: block["response"] for product, block in snapshot["last_24h"].items()})
    frames = []
    for index in range(FRAMES):
        seconds = index / FPS
        moment = take_moment_at(seconds, aspect)
        labels = []
        if moment.reveal > 0.001:
            for name, longitude, latitude, kind in PLACES:
                x, y = world_of(longitude, latitude)
                at = project_to_frame(moment, (x, y, 0.0), aspect, fov)
                if at is not None:
                    labels.append({"name": name, "kind": kind, "u": round(at[0], 4), "v": round(at[1], 4)})
        reading = timeline[int(min(max(seconds / 24.0, 0.0), 1.0) * (len(timeline) - 1))]
        frames.append({"reveal": round(moment.reveal, 4), "labels": labels, "station": reading})
    document = {
        "width": width,
        "height": height,
        "fps": FPS,
        "station_name": snapshot["station_name"],
        "station_id": snapshot["station"],
        "snapshot_fetched_at_utc": snapshot["fetched_at_utc"],
        "frames": frames,
    }
    out.write_text(json.dumps(document))
    print(f"wrote {out}: {len(frames)} frames")


if __name__ == "__main__":
    main(int(sys.argv[1]), int(sys.argv[2]), float(sys.argv[3]), Path(sys.argv[4]))
