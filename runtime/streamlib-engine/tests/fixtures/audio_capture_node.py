#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A node carrying one audio built-in, discoverable so its channel can be tapped.

What consumes the microphone is a stub that discards: a channel's data service
is created by `connect()`, so an unwired output port has nothing to tap. The
tap still reads what the *source* published, independently of what the consumer
does with it. `device_id` comes from the environment so the same node can be
pointed at a virtual device.
"""

import os

import streamlib
from audio_channel_drain import AudioChannelDrain


def main() -> None:
    runtime = streamlib.Runtime()
    device_id = os.environ.get("STREAMLIB_AUDIO_DEVICE_ID")
    config = {"device_id": device_id} if device_id else {}
    microphone = runtime.add(streamlib.MicrophoneSource, config=config)
    drain = runtime.add(AudioChannelDrain)
    runtime.connect(microphone.output("audio"), drain.input("audio_from_upstream"))
    runtime.host_control_plane(bind_port=int(os.environ.get("CONTROL_PORT", "9000")))
    runtime.run()


if __name__ == "__main__":
    main()
