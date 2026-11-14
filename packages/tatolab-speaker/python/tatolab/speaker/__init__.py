# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The speaker node: `from tatolab.speaker import SpeakerSink`.

The node runs in its own helper process like any Python processor. Its native
half opens the playback stream through the engine's audio device backend, which
the module host table exports, so PipeWire, ALSA and CoreAudio are the
engine's own arms and no second audio device path exists. A native thread reads
each block off the lent `audio` port into a ring the device callback fills
from, with no Python attached: Python runs at setup, start, stop and teardown,
never per block.
"""

from __future__ import annotations

import dataclasses
from typing import Annotated

from tatolab.stream import AudioWindowContract, Channels, Input, RuntimeContextFullAccess, log, node

from . import _native

__all__ = [
    "AUDIO_INPUT_PORT",
    "SPEAKER_CHANNELS",
    "SPEAKER_SAMPLE_FORMAT",
    "SPEAKER_SAMPLE_RATE",
    "SPEAKER_WINDOW_SIZE",
    "SpeakerSink",
    "SpeakerSinkConfig",
]

AUDIO_INPUT_PORT = "audio"

SPEAKER_SAMPLE_RATE = 48_000
"""The rate every block on `audio` is converted to and played at, in hertz."""

SPEAKER_CHANNELS = 2
"""The channel count every block on `audio` is converted to and played at."""

SPEAKER_SAMPLE_FORMAT = "f32"
"""The scalar encoding every block on `audio` is converted to and played as."""

SPEAKER_WINDOW_SIZE = 480
"""Per-channel samples in each window the engine cuts `audio` into: ten
milliseconds at `SPEAKER_SAMPLE_RATE`, which the playback ring absorbs whatever
the device's own period is."""


@dataclasses.dataclass
class SpeakerSinkConfig:
    """What a `SpeakerSink` is configured with."""

    device_id: Annotated[
        "str | None",
        "Backend-named playback device. Absent: the backend's default device. A name the "
        "backend cannot open raises rather than landing on a different device — a wrong "
        "device id is a wiring error.",
    ] = None


@node.meta(
    description=(
        "Plays timestamped Channels blocks on the machine's audio backend "
        "(discarding where no backend exists)."
    ),
)
@node.process(execution="manual", scheduling="realtime", paced_by_real_time=True)
class SpeakerSink:
    """Plays timestamped Channels blocks on the machine's audio backend
    (discarding where no backend exists).

    The engine's backend chain is probed in this node's helper process with no
    configuration dial; where no audio backend exists at all the samples are
    discarded, so a pipeline authored on a workstation runs unchanged in a
    headless container. `audio` takes Channels blocks at any rate, channel
    count and sample type: the engine converts every block to 48 kHz stereo
    `f32`, and the backend plays that on whatever the device opened at.
    The device is never left waiting on the graph — a period the graph had no
    samples for is silence, and the count of it is reported.

    On macOS the backend is CoreAudio and `device_id` is a CoreAudio device UID.
    Without a `device_id` playback follows the system default output as it
    changes.
    """

    def __init__(self, config: SpeakerSinkConfig) -> None:
        self._device_id = config.device_id
        self._playback: "_native.SpeakerPlayback | None" = None

    audio = Input(
        Channels,
        delivery="ordered",
        window=AudioWindowContract(
            sample_rate=SPEAKER_SAMPLE_RATE,
            channels=SPEAKER_CHANNELS,
            dtype=SPEAKER_SAMPLE_FORMAT,
            window_size=SPEAKER_WINDOW_SIZE,
        ),
        description="Timestamped blocks of samples to play",
    )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend `audio` to the native half and open the playback stream, so a
        device that cannot be opened refuses the node here."""
        module_host_table = ctx.open_module_host_table(input_ports=[AUDIO_INPUT_PORT])
        self._playback = _native.SpeakerPlayback(
            module_host_table,
            sample_rate=SPEAKER_SAMPLE_RATE,
            channels=SPEAKER_CHANNELS,
            sample_format=SPEAKER_SAMPLE_FORMAT,
            device_id=self._device_id,
        )
        log.info(
            f"SpeakerSink: playback stream opened on "
            f"{self._device_id or '<default>'} at {self._playback.sample_rate} Hz × "
            f"{self._playback.channels} ch, {self._playback.sample_format}, device period "
            f"{self._playback.device_period_in_per_channel_samples or 'unreported'}"
        )

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._opened_playback().start()

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        if self._playback is not None:
            self._playback.stop()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        playback = self._playback
        if playback is None:
            return
        playback.stop()
        log.info(
            f"SpeakerSink: teardown. played_blocks={playback.played_block_count} "
            f"refused_blocks={playback.refused_block_count} "
            f"underrun_bytes={playback.underrun_byte_count} "
            f"silence_before_playback_began_bytes="
            f"{playback.silence_before_playback_began_byte_count} "
            f"playback_device_failure={playback.failure_that_ended_the_playback}"
        )
        self._playback = None

    def _opened_playback(self) -> "_native.SpeakerPlayback":
        if self._playback is None:
            raise RuntimeError("SpeakerSink: start() ran before setup() opened the playback stream")
        return self._playback
