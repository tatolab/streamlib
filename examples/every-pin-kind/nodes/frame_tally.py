# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node written as one function with one value input: `report_every_frames`.

Its picture input is a Texture, and its `tally` output is a `Tally` bundle.
"""

from dataclasses import dataclass
from typing import Annotated, Callable

from tatolab.stream import (
    Description,
    Input,
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
@node(execution="reactive")
def FrameTally(
    *,
    report_every_frames: Input[
        Annotated[int, NumberRange(minimum=1), Description("How many pictures between two reports.")]
    ] = Input(30),
    picture: InputPin[
        Annotated[Texture, StreamDelivery("newest"), Description("The pictures to count")]
    ],
    tally: OutputPin[
        Annotated[Tally, Description("How many pictures it has seen, and the size of the latest")]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """A count of the pictures that reach it, written as a `Tally` and logged."""
    frames_seen = 0

    def each_frame(runtime_context: RuntimeContextLimitedAccess) -> None:
        nonlocal frames_seen
        frame = picture.read()
        if frame is None:
            return
        frames_seen += 1
        if frames_seen % max(1, report_every_frames.get()) != 0:
            return
        log.info(
            "frames tallied",
            frames_seen=frames_seen,
            width=frame.width,
            height=frame.height,
        )
        tally.write(
            Tally(frames_seen, frame.width, frame.height),
            timestamp_ns=frame.timestamp_ns,
        )

    return each_frame
