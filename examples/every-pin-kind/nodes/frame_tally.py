# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node that holds a setting: `report_every_frames`, a field of the class.

Its picture input is a Texture, and its `tally` output is a `Tally` bundle.
"""

from dataclasses import dataclass
from typing import Annotated

from tatolab.stream import (
    Description,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    log,
    node,
)


@dataclass(frozen=True)
class Tally:
    """How many pictures a `FrameTally` has seen, and the size of the latest."""

    frames_seen: int
    width: int
    height: int


@node.meta(
    description="Counts the pictures it sees and reports the count and the picture's size every report_every_frames pictures",
)
@node
class FrameTally:
    """A count of the pictures that reach it, written as a `Tally` and logged."""

    report_every_frames: Annotated[
        int, NumberRange(minimum=1), Description("How many pictures between two reports.")
    ] = 30
    picture: InputPin[
        Annotated[Texture, StreamDelivery("newest"), Description("The pictures to count")]
    ]
    tally: OutputPin[
        Annotated[Tally, Description("How many pictures it has seen, and the size of the latest")]
    ]

    def __init__(self) -> None:
        self.frames_seen = 0

    def process(self, runtime_context: RuntimeContextLimitedAccess) -> None:
        frame = self.picture.read()
        if frame is None:
            return
        self.frames_seen += 1
        if self.frames_seen % max(1, self.report_every_frames) != 0:
            return
        log.info(
            "frames tallied",
            frames_seen=self.frames_seen,
            width=frame.width,
            height=frame.height,
        )
        self.tally.write(
            Tally(self.frames_seen, frame.width, frame.height),
            timestamp_ns=frame.timestamp_ns,
        )
