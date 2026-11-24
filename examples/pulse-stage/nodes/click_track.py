# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`ClickTrack`: a process node that plays a click on every beat it is cued, higher on a bar's
first beat, as stereo audio in real time.

It reads the newest cue from `ClickCue` and starts a click each time the cue's count moves
on, so what you hear follows the same beat the picture pulses to.
"""

from __future__ import annotations

from typing import Annotated

import numpy
from tatolab.stream import (
    Audio,
    Description,
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
class ClickTrack:
    """Stereo blocks of silence, with a short decaying tone from each new beat."""

    volume: Annotated[
        float,
        NumberRange(minimum=0.0, maximum=1.0),
        Description("How loud the click plays."),
    ] = 0.4
    cue: InputPin[
        Annotated[BeatCue, StreamDelivery("newest"), Description("The beat, from ClickCue")]
    ]
    audio: OutputPin[Annotated[Audio, Description("The click, in stereo")]]

    def __init__(self) -> None:
        self.last_count: int | None = None
        self.click_hertz = BEAT_HERTZ
        self.samples_into_the_click = int(CLICK_SECONDS * SAMPLE_RATE)
        self.anchor_ns: int | None = None
        self.samples_written = 0
        self.first_sample = 0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self._take_the_newest_cue()
        if self.anchor_ns is None:
            self.anchor_ns = ctx.time
        samples_owed = (
            (ctx.time - self.anchor_ns) * SAMPLE_RATE
        ) // NANOSECONDS_PER_SECOND
        if samples_owed - self.samples_written > MOST_BLOCKS_OF_CATCH_UP * BLOCK_SIZE:
            log.warn(
                "ClickTrack fell behind and starts again at now",
                blocks_behind=(samples_owed - self.samples_written) // BLOCK_SIZE,
            )
            self.first_sample += self.samples_written
            self.anchor_ns = ctx.time
            self.samples_written = 0
            samples_owed = 0
        while self.samples_written + BLOCK_SIZE <= samples_owed:
            self._write_one_block()

    def _take_the_newest_cue(self) -> None:
        cue = self.cue.read()
        if cue is None:
            return
        if cue.count == self.last_count:
            return
        self.last_count = cue.count
        self.click_hertz = DOWNBEAT_HERTZ if cue.downbeat else BEAT_HERTZ
        self.samples_into_the_click = 0
        if cue.downbeat:
            log.info("bar", beat=cue.count)

    def _next_block(self) -> numpy.ndarray:
        sample_indices = self.samples_into_the_click + numpy.arange(BLOCK_SIZE)
        seconds = sample_indices / SAMPLE_RATE
        envelope = numpy.where(seconds < CLICK_SECONDS, numpy.exp(-seconds * 60.0), 0.0)
        tone = (
            numpy.sin(2.0 * numpy.pi * self.click_hertz * seconds)
            * envelope
            * min(1.0, max(0.0, self.volume))
        )
        self.samples_into_the_click += BLOCK_SIZE
        return numpy.ascontiguousarray(
            numpy.tile(tone.astype(numpy.float32), (CHANNEL_COUNT, 1))
        )

    def _write_one_block(self) -> None:
        assert self.anchor_ns is not None
        timestamp_ns = (
            self.anchor_ns
            + (self.samples_written * NANOSECONDS_PER_SECOND) // SAMPLE_RATE
        )
        self.audio.write(
            self._next_block(),
            timestamp_ns,
            start_sample=self.first_sample + self.samples_written,
            sample_rate=SAMPLE_RATE,
        )
        self.samples_written += BLOCK_SIZE
