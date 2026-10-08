# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A processor that announces the process it is running in.

Copied into a project directory beside its entry file, which is the import shape
a processor interpreter resolves from its project directory when the processor
does not live in a package. It reports on its first frame rather than at
startup: an interpreter that started but never received traffic has not made
the pipeline live.
"""

import os

from tatolab.stream import log, node


@node
class ReportsItsProcessOnFirstFrame:
    """Announces its own process the first time a frame reaches it."""

    def __init__(self) -> None:
        self.announced = False

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        if self.announced or ctx.inputs.read("video_from_upstream") is None:
            return
        self.announced = True
        log.info(f"MARKER:LIVE {os.getpid()}")
