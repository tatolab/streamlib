# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Nodes written as one function, which the helper-process tests run as a helper runs them."""

import threading
from dataclasses import dataclass
from typing import Callable

from tatolab.stream import (
    Input,
    InputPin,
    PinGroup,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    computed,
    node,
)

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


PUBLISHING_BUILDS_LET_GO = threading.Event()
"""Set while the publishing probe's builds run; a test clears it to hold each build until it sets
it again."""
PUBLISHING_BUILDS_LET_GO.set()


class PublishingOfALevel:
    """A value built from one level that would publish it on its own, recording its open and its
    close; its build waits while a test holds the builds."""

    def __init__(self, level: float) -> None:
        PUBLISHING_BUILDS_LET_GO.wait()
        self.level = level
        WHAT_THE_LEVEL_PUBLISHING_PROBE_DID.append(("opened", level))

    def close(self) -> None:
        WHAT_THE_LEVEL_PUBLISHING_PROBE_DID.append(("closed", self.level))


@node(execution="manual")
def LevelPublishingProbe(*, level: Input[float] = Input(0.25)) -> None:
    """Paces itself: opens a publishing of its level, which publishes on its own, and returns no
    per-frame function."""
    computed(lambda: PublishingOfALevel(level.get()))


@node(execution="manual")
def PacesItselfAndReturnsANumberProbe(*, level: Input[float] = Input(0.25)) -> None:
    """Paces itself, and returns a number nothing would use."""
    return 4.0  # type: ignore[return-value]


@node(execution="manual")
def PacesItselfAndReturnsAFunctionProbe(*, level: Input[float] = Input(0.25)) -> None:
    """Paces itself, and returns a per-frame function nothing would call."""
    return lambda frame_ctx: None  # type: ignore[return-value]


WHAT_THE_LAYER_STACKING_PROBE_DID: "list[tuple[str, object]]" = []
"""Each `("body", None)`, `("counted", count)`, `("frame", (count, gains))` and
`("read", (slot, level))` the layer stacking probe did, in order."""


@dataclass(frozen=True)
class StackedLayer:
    """One layer of the stacking probe: the level its wire carries, and its gain."""

    level: InputPin[float]
    gain: float = 1.0


class CountOfLayers:
    """A value built from how many layers the probe holds, which records its build."""

    def __init__(self, count: int) -> None:
        self.count = count
        WHAT_THE_LAYER_STACKING_PROBE_DID.append(("counted", count))


@node(execution="continuous", interval_ms=5)
def LayerStackingProbe(
    *, layers: PinGroup[StackedLayer]
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Builds a count of its layers from how many it holds, so a gain set builds no count, and
    records each frame the count built, each layer's gain and each level a layer's wire
    carried."""
    WHAT_THE_LAYER_STACKING_PROBE_DID.append(("body", None))
    layer_count = computed(lambda: len(layers.get()))
    count_of_layers = computed(lambda: CountOfLayers(layer_count.get()))

    def each_frame(frame_ctx: RuntimeContextLimitedAccess) -> None:
        held = layers.get()
        WHAT_THE_LAYER_STACKING_PROBE_DID.append(
            ("frame", (count_of_layers.get().count, tuple(layer.gain for layer in held)))
        )
        for slot, layer in enumerate(held):
            level = layer.level.read()
            if level is not None:
                WHAT_THE_LAYER_STACKING_PROBE_DID.append(("read", (slot, level)))

    return each_frame
