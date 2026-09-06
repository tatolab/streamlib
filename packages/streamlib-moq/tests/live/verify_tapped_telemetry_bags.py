#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Judge the telemetry bags that came back off the relay.

The data track's arm of the MoQ live proof, and the counterpart of the media
arms' PSNR lock: a bag that crossed a real draft-16 relay is compared with the
bag `moq_live_telemetry_processors.TelemetryBagSource` wrote, key for key and
byte for byte.

Three things are asserted per bag, and each fails a different way of losing
data that a liveness check would report as fine:

  the bag is exactly its three keys   — the envelope leaked into it, or a key
                                        was dropped
  `blob` is the frame's own bytes     — `bytes` came back a `str`, a byte was
                                        corrupted, or one bag was replayed as
                                        every bag
  `stamp_ns` is the frame's stamp     — something restamped on the way, so the
                                        producer's instant did not survive

The stamp comparison reads the payload and the transport frame's header, which
are two independent copies of one instant written by the source: they agree
only if the publisher's envelope carried the stamp and the subscriber restated
it. Ordering is *not* asserted — a subscriber that joins mid-group replays the
open group, which is MoQ's behaviour and accepted rather than masked.

Takes what `streamlib tap` returned — one file per tap round, because a
single tap collects over a window of about half a second and a sample that
narrow cannot show an intermittent corruption. Prints the report as JSON; exit
status is the verdict.
"""

import argparse
import json
import sys
from typing import Any

from moq_live_telemetry_processors import telemetry_blob_for_frame

# The engine's own decoder rather than a msgpack library: it is what reads a
# bag off the wire everywhere else, so a drift in the framing fails here rather
# than answering differently and quietly.
from streamlib._engine import decode_tapped_channel_bag_frame_to_python_object

#: The transport frame every tapped bag arrives inside: a 64-byte port key,
#: then the timestamp. Read here so the frame's stamp can be compared with the
#: one the source wrote into the payload.
FRAME_PORT_KEY_BYTES = 64

THE_BAGS_KEYS = {"frame", "stamp_ns", "blob"}


def frame_timestamp_ns(framed_bag_bytes: bytes) -> int:
    """When the transport says the bag was published."""
    stamp_at = FRAME_PORT_KEY_BYTES
    return int.from_bytes(
        framed_bag_bytes[stamp_at : stamp_at + 8], "little", signed=True
    )


def faults_in_one_bag(index: int, bag: Any, frame_stamp_ns: int) -> "list[str]":
    """Everything wrong with one received bag, named."""
    faults: "list[str]" = []
    if not isinstance(bag, dict):
        return [f"bag {index} is not a named map but a {type(bag).__name__}"]
    if set(bag) != THE_BAGS_KEYS:
        faults.append(
            f"bag {index} carries {sorted(bag)}, not the "
            f"{sorted(THE_BAGS_KEYS)} the source wrote"
        )
        return faults
    frame = bag["frame"]
    if not isinstance(frame, int):
        return [f"bag {index}: `frame` is a {type(frame).__name__}, not a count"]
    blob = bag["blob"]
    if not isinstance(blob, bytes):
        faults.append(
            f"bag {index} (frame {frame}): `blob` came back a "
            f"{type(blob).__name__} rather than bytes"
        )
    elif blob != telemetry_blob_for_frame(frame):
        faults.append(
            f"bag {index} (frame {frame}): `blob` is {blob.hex()}, and frame "
            f"{frame} was written with {telemetry_blob_for_frame(frame).hex()}"
        )
    if bag["stamp_ns"] != frame_stamp_ns:
        faults.append(
            f"bag {index} (frame {frame}): the source stamped "
            f"{bag['stamp_ns']} and the frame arrived stamped {frame_stamp_ns}, "
            f"so the producer's instant did not survive the round trip"
        )
    return faults


def report_for(tapped_bags: "list[dict[str, Any]]") -> "dict[str, Any]":
    """The verdict over every bag the tap returned."""
    faults: "list[str]" = []
    frames: "list[int]" = []
    for index, tapped in enumerate(tapped_bags):
        if tapped.get("hex_truncated"):
            faults.append(
                f"bag {index} arrived truncated at the tap's preview bound, so "
                f"what came back cannot be compared with what was sent"
            )
            continue
        framed = bytes.fromhex(tapped["hex_preview"])
        bag = decode_tapped_channel_bag_frame_to_python_object(framed)
        bag_faults = faults_in_one_bag(index, bag, frame_timestamp_ns(framed))
        faults.extend(bag_faults)
        if not bag_faults and isinstance(bag, dict):
            frames.append(bag["frame"])
    return {
        "verdict": "FAIL" if faults else "PASS",
        "bags_compared": len(tapped_bags),
        "bags_matching_what_was_sent": len(frames),
        "frames": frames,
        "faults": faults,
    }


def main(argv: "list[str]") -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "tapped_bags_json",
        nargs="+",
        help="what `streamlib tap` returned, one file per round",
    )
    parser.add_argument(
        "--minimum-bags",
        type=int,
        default=1,
        help="fewer received than this is a failure, not a thin pass",
    )
    arguments = parser.parse_args(argv[1:])

    bags: "list[dict[str, Any]]" = []
    for tapped_bags_path in arguments.tapped_bags_json:
        with open(tapped_bags_path) as tapped_bags_file:
            tap_result = json.load(tapped_bags_file)
        bags.extend(tap_result["bags"] if isinstance(tap_result, dict) else tap_result)

    report = report_for(bags)
    if len(bags) < arguments.minimum_bags:
        report["verdict"] = "FAIL"
        report["faults"].insert(
            0,
            f"the tap returned {len(bags)} bags and the run asked for at least "
            f"{arguments.minimum_bags}; a data track nothing came back on is a "
            f"loss, not a quiet pass",
        )
    print(json.dumps(report, indent=2))
    return 0 if report["verdict"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
