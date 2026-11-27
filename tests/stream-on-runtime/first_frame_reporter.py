# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node that announces the process it is running in.

Copied into a project directory beside its entry file, which is the import shape
a node interpreter resolves from its project directory when the node
does not live in a package. It reports on its first frame rather than at
startup: an interpreter that started but never received traffic has not made
the pipeline live.
"""

import os
from typing import Annotated, Callable

from tatolab.stream import InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, log, node


@node(execution="reactive")
def ReportsItsProcessOnFirstFrame(
    *,
    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Announces its own process the first time a frame reaches it."""

    announced = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal announced
        if announced or video_from_upstream.read() is None:
            return
        announced = True
        log.info(f"MARKER:LIVE {os.getpid()}")

    return each_frame
