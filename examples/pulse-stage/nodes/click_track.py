# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`ClickTrack`: a node that plays a click on every beat it is cued, higher on a bar's
first beat, as stereo audio in real time.

It reads the newest cue from `ClickCue` and starts a click each time the cue's count moves
on, so what you hear follows the same beat the picture pulses to.
"""

from __future__ import annotations

from typing import Annotated, Callable

import numpy
from tatolab.stream import (
    Audio,
    Description,
    Input,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    log,
    node,
)

from nodes.beat import BeatCue

SAMPLE_RATE = 48_000
BLOCK_SIZE = 480
CHANNEL_COUNT = 2
NANOSECONDS_PER_SECOND = 1_000_000_000
CLICK_SECONDS = 0.06
BEAT_HERTZ = 880.0
DOWNBEAT_HERTZ = 1_760.0
#: The most blocks it writes in one call after falling behind; past that it starts over at now.
MOST_BLOCKS_OF_CATCH_UP = 8


@node.meta(
    category="Pulse Stage",
    description="A click on every beat it is cued, higher on the first beat of a bar, played as stereo audio.",
)
@node(execution="continuous", interval_ms=5)
def ClickTrack(
    *,
    volume: Input[
        Annotated[
            float, NumberRange(minimum=0.0, maximum=1.0), Description("How loud the click plays.")
        ]
    ] = Input(0.4),
    cue: InputPin[
        Annotated[BeatCue, StreamDelivery("newest"), Description("count and downbeat, from ClickCue")]
    ],
    audio: OutputPin[Annotated[Audio, Description("The click, in stereo")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Stereo blocks of silence, with a short decaying tone from each new beat."""

    last_count: int | None = None
    click_hertz = BEAT_HERTZ
    samples_into_the_click = int(CLICK_SECONDS * SAMPLE_RATE)
    anchor_ns: int | None = None
    samples_written = 0
    first_sample = 0

    def take_the_newest_cue() -> None:
        nonlocal click_hertz, last_count, samples_into_the_click
        newest_cue = cue.read()
        if newest_cue is None:
            return
        count = newest_cue.count
        if count == last_count:
            return
        last_count = count
        click_hertz = DOWNBEAT_HERTZ if newest_cue.downbeat else BEAT_HERTZ
        samples_into_the_click = 0
        if newest_cue.downbeat:
            log.info("bar", beat=count)

    def next_block() -> numpy.ndarray:
        nonlocal samples_into_the_click
        sample_indices = samples_into_the_click + numpy.arange(BLOCK_SIZE)
        seconds = sample_indices / SAMPLE_RATE
        envelope = numpy.where(seconds < CLICK_SECONDS, numpy.exp(-seconds * 60.0), 0.0)
        tone = (
            numpy.sin(2.0 * numpy.pi * click_hertz * seconds)
            * envelope
            * volume.get()
        )
        samples_into_the_click += BLOCK_SIZE
        return numpy.ascontiguousarray(
            numpy.tile(tone.astype(numpy.float32), (CHANNEL_COUNT, 1))
        )

    def write_one_block() -> None:
        nonlocal samples_written
        assert anchor_ns is not None
        timestamp_ns = (
            anchor_ns
            + (samples_written * NANOSECONDS_PER_SECOND) // SAMPLE_RATE
        )
        audio.write(
            next_block(),
            start_sample=first_sample + samples_written,
            timestamp_ns=timestamp_ns,
            sample_rate=SAMPLE_RATE,
        )
        samples_written += BLOCK_SIZE

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal anchor_ns, first_sample, samples_written
        take_the_newest_cue()
        if anchor_ns is None:
            anchor_ns = ctx.time
        samples_owed = (
            (ctx.time - anchor_ns) * SAMPLE_RATE
        ) // NANOSECONDS_PER_SECOND
        if samples_owed - samples_written > MOST_BLOCKS_OF_CATCH_UP * BLOCK_SIZE:
            log.warn(
                "ClickTrack fell behind and starts again at now",
                blocks_behind=(samples_owed - samples_written) // BLOCK_SIZE,
            )
            first_sample += samples_written
            anchor_ns = ctx.time
            samples_written = 0
            samples_owed = 0
        while samples_written + BLOCK_SIZE <= samples_owed:
            write_one_block()

    return each_frame
