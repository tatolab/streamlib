# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The Python probe nodes the live input tests set inputs on while their stream plays."""

import fcntl
import json
import os
from typing import Callable, Optional

from tatolab.stream import (
    Input,
    RuntimeContextLimitedAccess,
    computed,
    log,
    node,
)

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
        path = os.environ.get(STUB_EXCLUSIVE_DEVICE_PATH_ENVIRONMENT_VARIABLE)
        if path is None:
            raise RuntimeError(
                f"{STUB_EXCLUSIVE_DEVICE_PATH_ENVIRONMENT_VARIABLE} names no file to lock as the "
                "stub exclusive device; the test hands it to the runtime's environment"
            )
        self._descriptor = os.open(path, os.O_RDWR | os.O_CREAT)
        try:
            fcntl.flock(self._descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(self._descriptor)
            raise

    def close(self) -> None:
        os.close(self._descriptor)


class StubDeviceHeldUntilTeardown:
    """A `StubExclusiveDevice` opened when it is built, reporting its open, and closed with a
    report of the node's teardown when the node's scope closes it."""

    def __init__(self) -> None:
        try:
            self._device: StubExclusiveDevice | None = StubExclusiveDevice()
        except BlockingIOError:
            report("STUB_DEVICE_REFUSED", {"pid": os.getpid()})
            raise
        report("STUB_DEVICE_OPENED", {"pid": os.getpid()})

    def close(self) -> None:
        report("PROBE_TEARDOWN", {"pid": os.getpid()})
        if self._device is not None:
            self._device.close()
            self._device = None
            report("STUB_DEVICE_CLOSED", {"pid": os.getpid()})


@node(execution="continuous", interval_ms=10)
def CountsItsBuildsHoldingAStubDevice(
    *, level: Input[float] = Input(0.25)
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports each setup it runs and each `level` it reads, and holds a `StubExclusiveDevice`
    in a computed value, which its scope closes when the node stops."""
    report("PROBE_SETUP", {"pid": os.getpid()})
    device = computed(StubDeviceHeldUntilTeardown)
    level_last_reported: list[Optional[float]] = [None]

    def each_frame(frame_ctx: RuntimeContextLimitedAccess) -> None:
        device.get()
        read = level.get()
        if read != level_last_reported[0]:
            level_last_reported[0] = read
            report("PROBE_READ_LEVEL", {"pid": os.getpid(), "level": float(read)})

    return each_frame


class ReadingOfALevel:
    """A value built from one level, which reports its build and its close."""

    def __init__(self, level: float) -> None:
        self.level = level
        report("LEVEL_BUILT", {"pid": os.getpid(), "level": float(level)})

    def close(self) -> None:
        report("LEVEL_CLOSED", {"pid": os.getpid(), "level": float(self.level)})


@node(execution="continuous", interval_ms=10)
def reads_its_level_as_one_function(
    *, level: Input[float] = Input(0.25)
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports its body running, builds a `ReadingOfALevel` of its level, and reports each level
    the reading its frames read holds."""
    report("NODE_FUNCTION_BODY_RAN", {"pid": os.getpid()})
    reading = computed(lambda: ReadingOfALevel(level.get()))
    level_last_reported: list[Optional[float]] = [None]

    def each_frame(frame_ctx: RuntimeContextLimitedAccess) -> None:
        read = reading.get().level
        if read != level_last_reported[0]:
            level_last_reported[0] = read
            report("NODE_FUNCTION_READ_LEVEL", {"pid": os.getpid(), "level": float(read)})

    return each_frame
