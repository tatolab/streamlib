# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A typed sink holding a number and a title, for `test_stream_graph_load_on_tatolabd.py`.

A test whose subject is only what a graph document can carry through a load, how deep it nests
or which numbers and strings it holds, names this node.
"""

from typing import Annotated, Callable

from tatolab.stream import Input, InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, node


@node(execution="reactive")
def ValueSink(
    *,
    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]],
    value: Input[int] = Input(0),
    title: Input[str] = Input(""),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads each frame it receives, and holds a number and a title."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        video_from_upstream.read()

    return each_frame
