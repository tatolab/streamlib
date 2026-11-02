# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The node the microphone A/B measures both sources with.

`microphone_ab.sh` copies this module into each arm's project directory, which
is on every helper's import path.
"""

from __future__ import annotations

import dataclasses
import time
from typing import IO, Annotated

from tatolab.stream import RuntimeContextFullAccess, RuntimeContextLimitedAccess, node
from tatolab.stream.audio_block import AudioBlock

AUDIO_PORT = "audio"

#: Rows buffered between flushes: about a second of blocks.
RECORDED_ROWS_BETWEEN_FLUSHES = 50

RECORDING_COLUMNS = (
    "arrival_ns",
    "first_sample_timestamp_ns",
    "sample_count",
    "sample_rate",
    "lost_since_last_read",
)


@dataclasses.dataclass
class AudioBlockArrivalRecorderConfig:
    """What an `AudioBlockArrivalRecorder` is configured with."""

    recording_path: Annotated[
        str, "The CSV each arrival is appended to, one row per block, with a header."
    ]


@node(description="Records when each audio block arrives, for the microphone A/B harness")
class AudioBlockArrivalRecorder:
    """One CSV row per audio block: when it arrived beside what it says about itself."""

    def __init__(self, config: AudioBlockArrivalRecorderConfig) -> None:
        self._recording_path = config.recording_path
        self._recording: "IO[str] | None" = None
        self._rows_since_flush = 0

    @node.input(delivery_profile="ordered")
    def audio(self) -> None:
        """Audio blocks from the source under test."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._recording = open(self._recording_path, "w", encoding="ascii")
        self._recording.write(",".join(RECORDING_COLUMNS) + "\n")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        recording = self._recording
        if recording is None:
            return
        while True:
            message = ctx.inputs.read_message(AUDIO_PORT, into=AudioBlock)
            if message is None:
                return
            arrival_ns = time.monotonic_ns()
            block = message.payload
            recording.write(
                f"{arrival_ns},{block.first_sample_timestamp_ns},{block.sample_count},"
                f"{block.sample_rate},{message.lost_since_last_read}\n"
            )
            self._rows_since_flush += 1
            if self._rows_since_flush >= RECORDED_ROWS_BETWEEN_FLUSHES:
                recording.flush()
                self._rows_since_flush = 0

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        if self._recording is not None:
            self._recording.close()
            self._recording = None
