# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes whose audio input port declares a window contract.

The consumer the contract exists for: a model that wants exact-size blocks at
its own rate gets them as ordinary user code, with no private buffering. Every
one of these runs in its own child process, so what they report is the child's
own stage — the same Rust code the parent's mailboxes run — honouring a contract
that crossed the wiring envelope.
"""

import json

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    AudioBlock,
    AudioWindowContract,
    input,
    log,
    processor,
)

CONTIGUOUS_RESULT_MARKER = "MARKER:WINDOWS_SEEN "
ROLLING_RESULT_MARKER = "MARKER:ROLLING_WINDOWS_SEEN "

WINDOWS_REPORTED = 6


def _reading(block) -> dict:
    return {
        "sample_rate": block.sample_rate,
        "channels": block.channels,
        "sample_count": block.sample_count,
        "dtype": block.dtype,
        "first_sample_timestamp_ns": block.first_sample_timestamp_ns,
        "shape": list(block.samples.shape),
    }


@processor
class ExactWindowProbe:
    """Reports the shape and stamp of the first few windows it is handed."""

    def __init__(self) -> None:
        self.readings = []

    @input(
        delivery_profile="ordered",
        audio_window=AudioWindowContract(
            sample_rate=16_000,
            channels=1,
            dtype="f32",
            window_size=512,
            hop=512,
        ),
    )
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        # Drained past the report so the probe measures the stage rather than
        # its own backlog draining away as counted drops.
        block = ctx.inputs.read("audio_from_upstream", into=AudioBlock)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(CONTIGUOUS_RESULT_MARKER + json.dumps(self.readings))


@processor
class RollingWindowProbe:
    """A hop below the window: consecutive windows overlap by the difference."""

    def __init__(self) -> None:
        self.readings = []

    @input(
        delivery_profile="ordered",
        audio_window=AudioWindowContract(
            sample_rate=16_000,
            channels=1,
            dtype="f32",
            window_size=512,
            hop=160,
        ),
    )
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        block = ctx.inputs.read("audio_from_upstream", into=AudioBlock)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(ROLLING_RESULT_MARKER + json.dumps(self.readings))
