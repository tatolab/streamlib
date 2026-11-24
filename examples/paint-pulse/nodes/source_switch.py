# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`SourceSwitch`: picks which sound the ball follows, the song, the computer's audio or a mic.

Its `monitor` output carries the sound only while the song plays: the computer's audio and a
mic are already audible in the room, so playing them back again doubles them or feeds back.

Every source keeps running; the switch passes on the one `source` names and drops the rest,
each block resampled by the engine to one shape on the way in. Its output keeps one
unbroken sample count across switches, so the listener downstream never sees a gap or a jump.
"""

from __future__ import annotations

from typing import Annotated

import numpy
from tatolab.stream import (
    Audio,
    AudioWindowContract,
    Description,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    node,
)

SAMPLE_RATE = 48_000
BLOCK = AudioWindowContract(sample_rate=SAMPLE_RATE, channels=2, dtype="f32", window_size=512)
SOURCE_NAMES = ("song", "computer", "mic")


@node.meta(
    category="Paint Pulse",
    description="Pass on one of three sounds, the song, the computer's audio or a mic, chosen by `source`.",
)
@node
class SourceSwitch:
    """Three sounds in, the chosen one out."""

    source: Annotated[
        int,
        NumberRange(minimum=0, maximum=len(SOURCE_NAMES) - 1),
        Description("Which source plays: 0 the song, 1 the computer's audio, 2 the mic."),
    ] = 0
    song: InputPin[Annotated[Audio, StreamDelivery("ordered"), BLOCK, Description("Source 0")]]
    computer: InputPin[Annotated[Audio, StreamDelivery("ordered"), BLOCK, Description("Source 1")]]
    mic: InputPin[Annotated[Audio, StreamDelivery("ordered"), BLOCK, Description("Source 2")]]
    audio: OutputPin[Annotated[Audio, Description("The chosen source, for listening to")]]
    monitor: OutputPin[
        Annotated[Audio, Description("The chosen source to play out loud: the song only")]
    ]

    def __init__(self) -> None:
        self.samples_written = 0
        self.samples_monitored = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        for index, pin in enumerate((self.song, self.computer, self.mic)):
            while True:
                block = pin.read()
                if block is None:
                    break
                if index != self.source:
                    continue
                samples = numpy.ascontiguousarray(numpy.from_dlpack(block), dtype=numpy.float32)
                self.audio.write(
                    samples,
                    start_sample=self.samples_written,
                    timestamp_ns=block.timestamp_ns,
                    sample_rate=SAMPLE_RATE,
                )
                self.samples_written += samples.shape[1]
                if index == 0:
                    self.monitor.write(
                        samples,
                        start_sample=self.samples_monitored,
                        timestamp_ns=block.timestamp_ns,
                        sample_rate=SAMPLE_RATE,
                    )
                    self.samples_monitored += samples.shape[1]
