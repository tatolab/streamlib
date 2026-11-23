# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The nodes the stream graph builder tests add to a stream.

Their own module rather than the test file because a node's type is the import
path of its class or function, `stream_graph_builder_nodes:<qualname>` — a name a child
process can import.
"""

from dataclasses import dataclass
from typing import Annotated, Literal

from tatolab.stream import (
    Color,
    EncodedVideo,
    InputPin,
    Int8,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    node,
)


@node
def remap_onto_a_range(x: float = 0.0, low: float = 0.0, high: float = 1.0) -> float:
    """Maps 0..1 onto low..high."""
    return low + x * (high - low)


@node
class FrameInverter:
    """Inverts each frame it reads."""

    strength: int = 1

    video_from_upstream: InputPin[Annotated[EncodedVideo, StreamDelivery("newest")]]

    video_to_downstream: OutputPin[EncodedVideo]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = self.video_from_upstream.read()
        if frame is not None:
            self.video_to_downstream.write(frame)


@node
class BrightnessReader:
    """Reads each frame it receives and writes nothing."""

    video_from_upstream: InputPin[Annotated[EncodedVideo, StreamDelivery("newest")]]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.video_from_upstream.read()


class FrameFilters:
    """A namespace, so a node's qualname differs from its short name."""

    @node
    class FrameDarkener:
        """Darkens each frame it reads."""

        video_from_upstream: InputPin[Annotated[EncodedVideo, StreamDelivery("newest")]]

        video_to_downstream: OutputPin[EncodedVideo]

        def process(self, ctx: RuntimeContextLimitedAccess) -> None:
            frame = self.video_from_upstream.read()
            if frame is not None:
                self.video_to_downstream.write(frame)


@node
class RigMonitor:
    """Shows each frame it reads under the name it is set to."""

    name: str = ""

    video: InputPin[Annotated[EncodedVideo, StreamDelivery("newest")]]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.video.read()


@node
def TintedLevel(
    level: Annotated[float, NumberRange(minimum=0.0, maximum=1.0)] = 0.5,
    tint: Color = Color(1.0, 1.0, 1.0),
    voices: Int8 = 1,
    waveform: Literal["sine", "square"] = "sine",
    steps: list[float] = [0.0],  # noqa: B006
    detune: float | None = None,
) -> float:
    """A typed node: its level, scaled by how bright its tint is."""
    return level * (tint.red + tint.green + tint.blue) / 3


@dataclass(frozen=True)
class Tone:
    """A bundle of two members."""

    frequency: float
    gain: float = 1.0


@node
def LouderTone(tone: Tone = Tone(440.0)) -> Tone:
    """A typed node taking and giving a bundle: its tone, its gain raised by one."""
    return Tone(tone.frequency, tone.gain + 1)


@node
def PassesItsValue[PinValue](value: PinValue) -> PinValue:
    """A typed node whose pins take the type linked into `value`."""
    return value
