# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Nodes written as one function, which the helper-process tests run as a helper runs them."""

from typing import Callable

from tatolab.stream import Input, RuntimeContextFullAccess, RuntimeContextLimitedAccess, computed, node

WHAT_THE_LEVEL_READING_PROBE_DID: "list[tuple[str, object]]" = []
"""Each `("body", None)`, `("built", level)`, `("closed", level)` and `("frame", (level, gain))`
the probe did, in order."""


class ReadingOfALevel:
    """A value built from one level, which records its build and its close."""

    def __init__(self, level: float) -> None:
        self.level = level
        WHAT_THE_LEVEL_READING_PROBE_DID.append(("built", level))

    def close(self) -> None:
        WHAT_THE_LEVEL_READING_PROBE_DID.append(("closed", self.level))


@node(execution="continuous", interval_ms=5)
def LevelReadingProbe(
    *,
    level: Input[float] = Input(0.25),
    gain: Input[float] = Input(1.0),
    ctx: RuntimeContextFullAccess,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Builds a reading of its level, and records each frame the reading's level and its gain."""
    WHAT_THE_LEVEL_READING_PROBE_DID.append(("body", None))
    reading = computed(lambda: ReadingOfALevel(level.get()))

    def each_frame(frame_ctx: RuntimeContextLimitedAccess) -> None:
        WHAT_THE_LEVEL_READING_PROBE_DID.append(("frame", (reading.get().level, gain.get())))

    return each_frame


@node(execution="continuous", interval_ms=5)
def ReadsItsLevelInItsBodyProbe(*, level: Input[float] = Input(0.25)) -> None:
    """Reads its level in its body, where no input is read."""
    level.get()


@node(execution="continuous", interval_ms=5)
def ReturnsANumberProbe(*, level: Input[float] = Input(0.25)) -> float:
    """Returns a number where a node function returns its per-frame function."""
    return 4.0


WHAT_THE_LEVEL_PUBLISHING_PROBE_DID: "list[tuple[str, float]]" = []
"""Each `("opened", level)` and `("closed", level)` the publishing probe did, in order."""


class PublishingOfALevel:
    """A value built from one level that would publish it on its own, recording its open and its
    close."""

    def __init__(self, level: float) -> None:
        self.level = level
        WHAT_THE_LEVEL_PUBLISHING_PROBE_DID.append(("opened", level))

    def close(self) -> None:
        WHAT_THE_LEVEL_PUBLISHING_PROBE_DID.append(("closed", self.level))


@node(execution="manual")
def LevelPublishingProbe(*, level: Input[float] = Input(0.25)) -> None:
    """Paces itself: opens a publishing of its level, which publishes on its own, and returns no
    per-frame function."""
    computed(lambda: PublishingOfALevel(level.get()))
