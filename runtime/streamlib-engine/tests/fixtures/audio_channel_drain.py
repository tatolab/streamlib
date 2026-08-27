# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A consumer that exists so the channel does.

A channel's data service is created by `connect()`, not by declaring an output
port, so an unwired source publishes into nothing and there is nothing to tap.
This reads and discards: what is under test is what the source published, which
the tap reads independently of anything downstream doing with it.
"""

from streamlib import input, processor


@processor
class AudioChannelDrain:
    @input(delivery_profile="lossless")
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        ctx.inputs.read("audio_from_upstream")
