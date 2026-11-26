# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The Python probe node the live input tests set inputs on while its stream plays."""

import fcntl
import json
import os

from tatolab.stream import RuntimeContextFullAccess, log, node

#: Names the file whose exclusive lock stands in for one exclusive device, as a camera is.
STUB_EXCLUSIVE_DEVICE_PATH_ENVIRONMENT_VARIABLE = (
    "STREAMLIB_TEST_STUB_EXCLUSIVE_DEVICE_PATH"
)


def report(marker_name: str, payload: "dict[str, object]") -> None:
    log.info(f"MARKER:{marker_name} {json.dumps(payload)}")


class StubExclusiveDevice:
    """An exclusive lock on the file the environment names, which one holder in any process
    may take at once; a second holder is refused, as a busy device refuses it."""

    def __init__(self) -> None:
        self._descriptor = os.open(
            os.environ[STUB_EXCLUSIVE_DEVICE_PATH_ENVIRONMENT_VARIABLE],
            os.O_RDWR | os.O_CREAT,
        )
        try:
            fcntl.flock(self._descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(self._descriptor)
            raise

    def close(self) -> None:
        os.close(self._descriptor)


@node(execution="continuous", interval_ms=10)
class CountsItsBuildsHoldingAStubDevice:
    """Reports each setup and teardown it runs and each `level` it reads, and holds a
    `StubExclusiveDevice` from its setup to its teardown."""

    level: float = 0.25

    def __init__(self) -> None:
        self._level_last_reported: float | None = None
        self._device: StubExclusiveDevice | None = None

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report("PROBE_SETUP", {"pid": os.getpid()})
        try:
            self._device = StubExclusiveDevice()
        except BlockingIOError:
            report("STUB_DEVICE_REFUSED", {"pid": os.getpid()})
            raise
        report("STUB_DEVICE_OPENED", {"pid": os.getpid()})

    def process(self, ctx) -> None:
        if self.level != self._level_last_reported:
            self._level_last_reported = self.level
            report("PROBE_READ_LEVEL", {"pid": os.getpid(), "level": float(self.level)})

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        report("PROBE_TEARDOWN", {"pid": os.getpid()})
        if self._device is not None:
            self._device.close()
            self._device = None
            report("STUB_DEVICE_CLOSED", {"pid": os.getpid()})
