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

from typing import Annotated, Callable

import numpy
from tatolab.stream import (
    Audio,
    AudioWindowContract,
    Description,
    Input,
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
@node(execution="reactive")
def SourceSwitch(
    *,
    source: Input[
        Annotated[
            int,
            NumberRange(minimum=0, maximum=len(SOURCE_NAMES) - 1),
            Description("Which source plays: 0 the song, 1 the computer's audio, 2 the mic."),
        ]
    ] = Input(0),
    song: InputPin[Annotated[Audio, StreamDelivery("ordered"), BLOCK, Description("Source 0")]],
    computer: InputPin[Annotated[Audio, StreamDelivery("ordered"), BLOCK, Description("Source 1")]],
    mic: InputPin[Annotated[Audio, StreamDelivery("ordered"), BLOCK, Description("Source 2")]],
    audio: OutputPin[Annotated[Audio, Description("The chosen source, for listening to")]],
    monitor: OutputPin[
        Annotated[Audio, Description("The chosen source to play out loud: the song only")]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Three sounds in, the chosen one out."""
    samples_written = 0
    samples_monitored = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal samples_written, samples_monitored
        chosen = source.get()
        for index, pin in enumerate((song, computer, mic)):
            while True:
                block = pin.read()
                if block is None:
                    break
                if index != chosen:
                    continue
                samples = numpy.ascontiguousarray(numpy.from_dlpack(block), dtype=numpy.float32)
                audio.write(
                    samples,
                    start_sample=samples_written,
                    timestamp_ns=block.timestamp_ns,
                    sample_rate=SAMPLE_RATE,
                )
                samples_written += samples.shape[1]
                if index == 0:
                    monitor.write(
                        samples,
                        start_sample=samples_monitored,
                        timestamp_ns=block.timestamp_ns,
                        sample_rate=SAMPLE_RATE,
                    )
                    samples_monitored += samples.shape[1]

    return each_frame
