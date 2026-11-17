# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node not yet typed: a class in the old declaration form, its ports declared by family and
its setting held in a config class.

Its picture input is a Texture, as every picture port is; its Records output names no pin
type, and `report_every_frames` is a setting of a node type that is not typed.
"""

from dataclasses import dataclass

from tatolab.stream import (
    Image,
    Input,
    Output,
    Records,
    RuntimeContextLimitedAccess,
    log,
    node,
)

TALLY_RECORD_NAMES = ("frames_seen", "width", "height")


@dataclass
class FrameTallyConfig:
    """FrameTally's settings, as its node's input values in the graph document spell them."""

    report_every_frames: int = 30


@node.meta(
    description="Counts the pictures it sees and reports the count and the picture's size every report_every_frames pictures",
)
@node.process
class FrameTally:
    """A count of the pictures that reach it, written as a record and logged."""

    picture = Input(Image, delivery="newest", description="The pictures to count")
    tally = Output(
        Records,
        names=TALLY_RECORD_NAMES,
        description="How many pictures it has seen, and the size of the latest",
    )

    def __init__(self, config: FrameTallyConfig) -> None:
        self.report_every_frames = max(1, int(config.report_every_frames))
        self.frames_seen = 0

    def process(self, runtime_context: RuntimeContextLimitedAccess) -> None:
        frame = runtime_context.inputs.read(self.picture)
        if frame is None:
            return
        self.frames_seen += 1
        if self.frames_seen % self.report_every_frames != 0:
            return
        log.info(
            "frames tallied",
            frames_seen=self.frames_seen,
            width=frame.width,
            height=frame.height,
        )
        runtime_context.outputs.write(
            self.tally,
            {
                "frames_seen": self.frames_seen,
                "width": frame.width,
                "height": frame.height,
            },
            timestamp_ns=frame.timestamp_ns,
        )
