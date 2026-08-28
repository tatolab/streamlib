#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A node that plays a known signal and captures it back off the same sink.

The engine carries the audio in both directions: a Python processor publishes
`AudioBlock` bags, `SpeakerSink` plays them into the sink named by
`STREAMLIB_AUDIO_SINK`, and `MicrophoneSource` captures that sink's monitor.
`e2e_audio_loopback.sh` closes the same loop with `pw-play` and `pw-record` and
no StreamLib at all — so when this run fails and that one passes, the rig is
sound and the engine is not.

What consumes the microphone is a stub that discards: a channel's data service
is created by `connect()`, so an unwired output port has nothing to tap. The
tap reads what the *source* published, independently of what the consumer does
with it.
"""

import os

import streamlib
from audio_channel_drain import AudioChannelDrain
from known_audio_signal_source import KnownAudioSignalSource


def main() -> None:
    sink = os.environ["STREAMLIB_AUDIO_SINK"]
    runtime = streamlib.Runtime()

    signal = runtime.add(KnownAudioSignalSource)
    speaker = runtime.add(streamlib.SpeakerSink, config={"device_id": sink})
    runtime.connect(signal.output("audio"), speaker.input("audio"))

    # `<sink>.monitor` is the capture endpoint the session already routes for a
    # sink: what is played into it is readable here, which is the whole loop.
    microphone = runtime.add(
        streamlib.MicrophoneSource, config={"device_id": f"{sink}.monitor"}
    )
    drain = runtime.add(AudioChannelDrain)
    runtime.connect(microphone.output("audio"), drain.input("audio_from_upstream"))

    # Loopback rather than the default every interface: this node exists to be
    # tapped from the machine it runs on, and it carries no authentication.
    runtime.host_control_plane(
        bind_host="127.0.0.1",
        bind_port=int(os.environ.get("CONTROL_PORT", "9000")),
    )
    runtime.run()


if __name__ == "__main__":
    main()
