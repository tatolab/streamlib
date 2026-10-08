# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The one processor of `capability_context_streams.zero_argument_process`.

Its `process` hook takes no ctx. The body reports through the helper log drain,
the channel every other probe's marker reaches `tatolabd`'s standard error by,
so a hook body that ran would be seen there.
"""

from tatolab.stream import log, node


@node(execution="continuous", interval_ms=1)
class ZeroArgumentProcess:
    def process(self) -> None:  # deliberately missing the ctx parameter
        log.info("MARKER:HOOK_BODY_RAN")
