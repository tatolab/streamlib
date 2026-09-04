# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A processor that reports what its own helper process loaded before it.

Imported by the child that hosts it and by the app that registers it. Its only
job is to be a reason for a helper to exist: the claim under test is that the
capability-extension hooks ran in that child, before this module was imported.
"""

import sys

from streamlib import log, output, processor


@processor(execution="continuous", interval_ms=100)
class ReportsTheExtensionItsHelperLoaded:
    """Announces, once, which extension modules its own process imported."""

    def __init__(self) -> None:
        self.announced = False

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        # Reports and writes nothing: the port exists to make this a processor,
        # and nothing downstream is wired, so a write would only raise.
        if not self.announced:
            loaded = "streamlib_test_extension" in sys.modules
            log.info(f"MARKER:HELPER_LOADED_THE_EXTENSION={loaded}")
            self.announced = True
