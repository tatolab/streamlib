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

from dataclasses import dataclass

import numpy
from tatolab.stream import (
    AudioWindowContract,
    Channels,
    Input,
    Output,
    RuntimeContextLimitedAccess,
    node,
)

SAMPLE_RATE = 48_000
BLOCK = AudioWindowContract(sample_rate=SAMPLE_RATE, channels=2, dtype="f32", window_size=512)
SOURCE_NAMES = ("song", "computer", "mic")


@dataclass
class SourceSwitchConfig:
    """Which source plays: 0 the song, 1 the computer's audio, 2 the mic."""

    source: int = 0


@node.meta(
    category="Paint Pulse",
    description="Pass on one of three sounds, the song, the computer's audio or a mic, chosen by `source`.",
)
@node.process
class SourceSwitch:
    """Three sounds in, the chosen one out."""

    song = Input(Channels, delivery="ordered", window=BLOCK, description="Source 0")
    computer = Input(Channels, delivery="ordered", window=BLOCK, description="Source 1")
    mic = Input(Channels, delivery="ordered", window=BLOCK, description="Source 2")
    audio = Output(Channels, rate=SAMPLE_RATE, dtype="f32", description="The chosen source, for listening to")
    monitor = Output(Channels, rate=SAMPLE_RATE, dtype="f32", description="The chosen source to play out loud: the song only")

    def __init__(self, config: SourceSwitchConfig) -> None:
        self.source = 0
        self.samples_written = 0
        self.samples_monitored = 0
        self.configure(config)

    def configure(self, config: SourceSwitchConfig) -> None:
        self.source = min(len(SOURCE_NAMES) - 1, max(0, int(config.source)))

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        for index, port in enumerate((self.song, self.computer, self.mic)):
            while True:
                block = ctx.inputs.read(port)
                if block is None:
                    break
                if index != self.source:
                    continue
                samples = numpy.ascontiguousarray(numpy.from_dlpack(block), dtype=numpy.float32)
                ctx.outputs.write(
                    self.audio,
                    samples,
                    start_sample=self.samples_written,
                    timestamp_ns=block.timestamp_ns,
                )
                self.samples_written += samples.shape[1]
                if index == 0:
                    ctx.outputs.write(
                        self.monitor,
                        samples,
                        start_sample=self.samples_monitored,
                        timestamp_ns=block.timestamp_ns,
                    )
                    self.samples_monitored += samples.shape[1]
