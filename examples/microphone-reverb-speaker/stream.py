# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: microphone → reverb → speaker.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest — and loads it again when you edit `nodes/reverb_effect.py`. Nothing
here names a sample rate, a channel count or a block size: the capture device
opens at whatever it opens at, the playback device likewise, and the two
conversions between them are the engine's. `ReverbEffect` declares the one
format it wants and `SpeakerSink` matches its own device.

Nodes live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from tatolab.stream import (
    MicrophoneSource,
    MicrophoneSourceConfig,
    SpeakerSink,
    SpeakerSinkConfig,
    StreamBuilder,
    stream,
)

from nodes.reverb_effect import ReverbEffect

# Devices as the audio backend names them — a CoreAudio device UID on macOS.
# `None` opens the backend's default. A named device the backend
# cannot open raises rather than landing on another device.
MICROPHONE_DEVICE_ID: str | None = None
SPEAKER_DEVICE_ID: str | None = None


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The microphone through a reverb, played on the speaker."""
    microphone_config: MicrophoneSourceConfig = {}
    if MICROPHONE_DEVICE_ID is not None:
        microphone_config["device_id"] = MICROPHONE_DEVICE_ID
    speaker_config: SpeakerSinkConfig = {}
    if SPEAKER_DEVICE_ID is not None:
        speaker_config["device_id"] = SPEAKER_DEVICE_ID

    microphone = stream_builder.add(MicrophoneSource, config=microphone_config)
    reverb = stream_builder.add(
        ReverbEffect,
        config={
            "room_size": 0.7,
            "damping": 0.5,
            "wet_level": 0.25,
            "dry_level": 0.7,
        },
    )
    speaker = stream_builder.add(SpeakerSink, config=speaker_config)

    stream_builder.connect(
        microphone.output("audio"), reverb.input("dry_audio_from_upstream")
    )
    stream_builder.connect(
        reverb.output("reverberated_audio_to_downstream"), speaker.input("audio")
    )
