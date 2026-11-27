# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The speaker node: `from tatolab.speaker import SpeakerSink`.

The node runs in its own helper process like any Python node. Its native
half opens the playback stream through the engine's audio device backend, which
the module host table exports, so PipeWire, ALSA and CoreAudio are the
engine's own arms and no second audio device path exists. A native thread reads
each block off the lent `audio` port into a ring the device callback fills
from, with no Python attached: Python runs only when an input changes, never
per block.
"""

from __future__ import annotations

from typing import Annotated

from tatolab.stream import (
    Audio,
    AudioWindowContract,
    Description,
    Input,
    InputPin,
    PinControl,
    RuntimeContextFullAccess,
    StreamDelivery,
    computed,
    log,
    node,
)

from . import _native

__all__ = [
    "AUDIO_INPUT_PORT",
    "SPEAKER_CHANNELS",
    "SPEAKER_SAMPLE_FORMAT",
    "SPEAKER_SAMPLE_RATE",
    "SPEAKER_WINDOW_SIZE",
    "SpeakerSink",
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


@node.meta(
    description=(
        "Plays timestamped Channels blocks on the machine's audio backend "
        "(discarding where no backend exists)."
    ),
)
@node(execution="manual", scheduling="realtime", paced_by_real_time=True)
def SpeakerSink(
    *,
    device_id: Input[
        Annotated[
            str | None,
            Description(
                "Backend-named playback device. Unset: the backend's default device. A name the "
                "backend cannot open fails the open rather than landing on a different device — "
                "a wrong device id is a wiring error."
            ),
        ]
    ] = Input(None),
    audio: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            AudioWindowContract(
                sample_rate=SPEAKER_SAMPLE_RATE,
                channels=SPEAKER_CHANNELS,
                dtype=SPEAKER_SAMPLE_FORMAT,
                window_size=SPEAKER_WINDOW_SIZE,
            ),
            PinControl("meter"),
            Description("Timestamped blocks of samples to play"),
        ]
    ],
    ctx: RuntimeContextFullAccess,
) -> None:
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

    The open playback stream is a computed value: setting `device_id` while the
    stream plays opens the new device while the old one keeps playing, then
    closes the old one; a device that fails to open shows on the node, and the
    old one keeps playing.

    On macOS the backend is CoreAudio and `device_id` is a CoreAudio device UID.
    Without a `device_id` playback follows the system default output as it
    changes.
    """
    del audio  # Lent to the native half, which drains it.
    module_host_table = ctx.open_module_host_table(input_ports=[AUDIO_INPUT_PORT])

    def open_the_speaker() -> _native.SpeakerPlayback:
        named = device_id.get()
        playback = _native.SpeakerPlayback(
            module_host_table,
            sample_rate=SPEAKER_SAMPLE_RATE,
            channels=SPEAKER_CHANNELS,
            sample_format=SPEAKER_SAMPLE_FORMAT,
            device_id=named,
        )
        log.info(
            f"SpeakerSink: playback stream opened on {named or '<default>'} at "
            f"{playback.sample_rate} Hz × {playback.channels} ch, {playback.sample_format}, "
            f"device period {playback.device_period_in_per_channel_samples or 'unreported'}"
        )
        playback.start()
        return playback

    computed(open_the_speaker)
