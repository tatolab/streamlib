# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The microphone node: `from tatolab.microphone import MicrophoneSource`.

The node runs in its own helper process like any Python processor. Its native
half opens the capture stream through the engine's audio device backend, which
the module host table exports, so PipeWire, ALSA and CoreAudio are the
engine's own arms and no second audio device path exists. The device callback
hands each block to a native thread that publishes it on `audio` with no
Python attached: Python runs at setup, start, stop and teardown, never per
block.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated

from tatolab.stream import Channels, Output, RuntimeContextFullAccess, log, node

from . import _native

__all__ = ["MicrophoneSource", "MicrophoneSourceConfig"]

AUDIO_OUTPUT_PORT = "audio"


@dataclasses.dataclass
class MicrophoneSourceConfig:
    """What a `MicrophoneSource` is configured with."""

    device_id: Annotated[
        "str | None",
        "Backend-named capture device. Absent: the backend's default device. A name the "
        "backend cannot open raises rather than landing on a different device — a wrong "
        "device id is a wiring error. On the PipeWire backend, `<sink>.monitor` captures "
        "what that sink is playing.",
    ] = None


@node.meta(
    description=(
        "Captures audio from the machine's audio backend as timestamped sample blocks "
        "(silence where no backend exists)."
    ),
)
@node.process(execution="manual", scheduling="realtime", paced_by_real_time=True)
class MicrophoneSource:
    """Captures audio from the machine's audio backend as timestamped sample blocks
    (silence where no backend exists).

    The engine's backend chain is probed in this node's helper process with no
    configuration dial; where no audio backend exists at all the blocks are
    silence, so a pipeline authored on a workstation runs unchanged in a headless
    container. Blocks arrive on `audio` as f32 Channels blocks at the device's
    rate and channel count, an i16 device widened to f32, each stamped with the
    device's instant of its first sample.

    On macOS the backend is CoreAudio and `device_id` is a CoreAudio device UID;
    naming one that is not attached is refused with the UIDs that are. Only a
    stream that holds this node opens a microphone, so only such a stream can
    put the platform's access prompt in front of the user, and it asks only when
    the stream's load allows: a stream the runtime re-loads on its own never
    asks, and is refused at `setup()` until microphone access has been answered.
    """

    def __init__(self, config: MicrophoneSourceConfig) -> None:
        self._device_id = config.device_id
        self._capture: "_native.MicrophoneCapture | None" = None

    audio = Output(
        Channels, dtype="f32", description="Timestamped planar blocks of capture samples"
    )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend `audio` to the native half and open the capture stream, so a
        device that cannot be opened refuses the node here."""
        module_host_table = ctx.open_module_host_table(output_ports=[AUDIO_OUTPUT_PORT])
        self._capture = _native.MicrophoneCapture(module_host_table, self._device_id)
        log.info(
            f"MicrophoneSource: capture stream opened on "
            f"{self._device_id or '<default>'} at {self._capture.sample_rate} Hz × "
            f"{self._capture.channels} ch, {self._capture.sample_format}"
        )

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._opened_capture().start()

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        if self._capture is not None:
            self._capture.stop()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        capture = self._capture
        if capture is None:
            return
        capture.stop()
        log.info(
            f"MicrophoneSource: teardown. published_blocks={capture.published_block_count} "
            f"dropped_blocks={capture.dropped_block_count} "
            f"capture_device_failure={capture.failure_that_ended_the_capture}"
        )
        self._capture = None

    def _opened_capture(self) -> "_native.MicrophoneCapture":
        if self._capture is None:
            raise RuntimeError(
                "MicrophoneSource: start() ran before setup() opened the capture stream"
            )
        return self._capture
