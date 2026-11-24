# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A typed sink holding a number and a title, for `test_stream_graph_load_on_tatolabd.py`.

A test whose subject is only what a graph document can carry through a load, how deep it nests
or which numbers and strings it holds, names this node.
"""

from typing import Annotated

from tatolab.stream import InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, node


@node
class ValueSink:
    """Reads each frame it receives, and holds a number and a title."""

    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]]
    value: int = 0
    title: str = ""

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        self.video_from_upstream.read()
