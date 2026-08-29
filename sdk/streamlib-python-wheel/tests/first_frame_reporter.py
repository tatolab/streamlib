# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A processor that announces the process it is running in.

Copied into an app directory beside its entry file, which is the import shape a
helper child resolves off `PYTHONPATH` when the processor does not live in a
package. It reports on its first frame rather than at startup: a child that
spawned but never received traffic has not made the pipeline live.
"""

import os

from streamlib import input, log, processor  # noqa: A004 — streamlib's port decorator


@processor
class ReportsItsProcessOnFirstFrame:
    """Announces its own process the first time a frame reaches it."""

    def __init__(self) -> None:
        self.announced = False

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        if self.announced or ctx.inputs.read("video_from_upstream") is None:
            return
        self.announced = True
        log.info(f"MARKER:LIVE {os.getpid()}")
