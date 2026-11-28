# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The one node of `capability_context_streams.zero_argument_process`.

Its per-frame function takes no ctx. The body reports through the helper log drain, the channel
every other probe's marker reaches `tatolabd`'s standard error by, so a per-frame body that ran
would be seen there.
"""

from typing import Callable

from tatolab.stream import log, node


@node(execution="continuous", interval_ms=1)
def ZeroArgumentProcess() -> Callable[[], None]:
    def each_frame() -> None:  # deliberately missing the ctx parameter
        log.info("MARKER:HOOK_BODY_RAN")

    return each_frame
