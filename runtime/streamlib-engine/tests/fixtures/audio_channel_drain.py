# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A consumer that exists so the channel does.

Reads and discards: what is under test is what the source published, which the
tap reads independently of anything downstream doing with it.
"""

from tatolab.stream import RuntimeContextLimitedAccess, node


@node
class AudioChannelDrain:
    @node.input(delivery_profile="ordered")
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("audio_from_upstream")
