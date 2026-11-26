# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The Python probe node the live input tests set inputs on while its stream plays."""

import json
import os

from tatolab.stream import RuntimeContextFullAccess, log, node


def report(marker_name: str, payload: "dict[str, object]") -> None:
    log.info(f"MARKER:{marker_name} {json.dumps(payload)}")


@node(execution="continuous", interval_ms=10)
class CountsItsBuildsHoldingAStubDevice:
    """Reports each setup and teardown it runs and each `level` it reads, and holds a stub
    exclusive device from its setup to its teardown, reporting each open and close of it."""

    level: float = 0.25

    def __init__(self) -> None:
        self._level_last_reported: float | None = None

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report("PROBE_SETUP", {"pid": os.getpid()})
        report("STUB_DEVICE_OPENED", {"pid": os.getpid()})

    def process(self, ctx) -> None:
        if self.level != self._level_last_reported:
            self._level_last_reported = self.level
            report("PROBE_READ_LEVEL", {"pid": os.getpid(), "level": float(self.level)})

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        report("STUB_DEVICE_CLOSED", {"pid": os.getpid()})
        report("PROBE_TEARDOWN", {"pid": os.getpid()})
