#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A node carrying one audio built-in, discoverable so its channel can be tapped.

What consumes the microphone is a stub that discards: a channel's data service
is created by a link, so an unwired output port has nothing to tap. The
tap still reads what the *source* published, independently of what the consumer
does with it. `device_id` comes from the environment so the same node can be
pointed at a virtual device.
"""

import os

import streamlib
from audio_channel_drain import AudioChannelDrain
from streamlib import Stream, compile_stream_to_graph, stream


@stream
def microphone_into_an_audio_channel_drain(stream: Stream) -> None:
    device_id = os.environ.get("STREAMLIB_AUDIO_DEVICE_ID")
    microphone = stream.add(
        streamlib.MicrophoneSource,
        config={"device_id": device_id} if device_id else {},
    )
    drain = stream.add(AudioChannelDrain)
    stream.connect(microphone.output("audio"), drain.input("audio_from_upstream"))


def main() -> None:
    graph = compile_stream_to_graph(microphone_into_an_audio_channel_drain)
    runtime = streamlib.Runtime()
    runtime.load(graph)
    # Loopback rather than the default every interface: this node exists to be
    # tapped from the machine it runs on, and it carries no authentication.
    runtime.host_control_plane(
        bind_host="127.0.0.1",
        bind_port=int(os.environ.get("CONTROL_PORT", "9000")),
    )
    runtime.run()


if __name__ == "__main__":
    main()
