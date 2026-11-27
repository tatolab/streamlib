# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


import json
import time
from typing import Annotated, Any, Callable, TypedDict

from tatolab.stream import (
    Color,
    EncodedVideo,
    Input,
    InputPin,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    SettingOnly,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    node,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_DECLARED_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.runtime._node_the_wheel_hosts import (
    NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG,
    stamp_the_ports_of_a_node_the_wheel_hosts,
)


def built_from_its_config_as_the_wheel_builds_a_promoted_function_node(
    *, output_ports: "tuple[str, ...]" = ()
) -> Any:
    """Stand the class in for a node the wheel builds itself from the config the parent hands
    it, as it builds a promoted function node: no author declares a config."""

    def stand_in(node_class: type) -> type:
        stamp_the_ports_of_a_node_the_wheel_hosts(
            node_class, input_ports=[], output_ports=list(output_ports)
        )
        setattr(node_class, NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG, True)
        return node_class

    return stand_in


@node(execution="reactive")
def PassThroughProbe(
    *,
    tag: Input[str] = Input("untagged"),
    frames_from_upstream: InputPin[Annotated[EncodedVideo, StreamDelivery("newest")]],
    frames_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Copies every JSON record from its input to its output, tagged."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        unit = frames_from_upstream.read()
        if unit is not None:
            record = json.loads(bytes(unit.payload))
            frames_to_downstream.write(json.dumps({**record, "tag": tag.get()}).encode())

    return each_frame


@node(execution="continuous", interval_ms=60_000)
def WritesOnceInSetupProbe(
    *,
    frames_to_downstream: OutputPin[EncodedVideo],
) -> None:
    """Writes one record as its body runs, before any link reaches its output, and never again;
    its interval is long enough that it never runs again inside a test."""

    frames_to_downstream.write(json.dumps({"written": "once"}).encode())


class OuterProbe:

    @node(execution="manual")
    class InnerProbe:
        frames_to_downstream: OutputPin[EncodedVideo]


@node(execution="manual")
def ImportsADmaBufOffLinuxProbe(
    *,
    frames_to_downstream: OutputPin[EncodedVideo],
    ctx: RuntimeContextFullAccess,
) -> None:
    """Adopts a DMA-BUF in its body, off Linux, and fails with the refusal."""

    ctx.gpu_full_access.import_dma_buf(fd=0, width=16, height=16)


@node(execution="manual")
def RefusesSetupProbe(
    *,
    frames_to_downstream: OutputPin[EncodedVideo],
) -> None:
    """Raises out of its body, which the parent must hear about."""

    raise RuntimeError("this node cannot set itself up")


@node(execution="reactive")
def SlowPassThroughProbe(
    *,
    frames_from_upstream: InputPin[Annotated[EncodedVideo, StreamDelivery("newest")]],
    frames_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Copies every message's payload to its output, slower than a burst arrives."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        unit = frames_from_upstream.read()
        if unit is not None:
            time.sleep(0.005)
            frames_to_downstream.write(
                bytes(unit.payload), unit.pts, starts_group=unit.starts_group
            )

    return each_frame


# The hooks the interrupt probes below actually reached, in order. A test reads
# it to tell "the interrupt ended the helper" from "the interrupt was absorbed
# and the rest of the ladder ran".
HOOKS_THE_INTERRUPT_PROBES_REACHED: list[str] = []


@node(execution="continuous", interval_ms=0)
class InterruptedInProcessProbe:
    """Takes a `KeyboardInterrupt` inside `process()`, the way the parent's
    shutdown ladder delivers one to a callback that outran its budget."""

    frames_to_downstream: OutputPin[EncodedVideo]

    def __init__(self) -> None:
        self._already_took_the_interrupt = False

    def process(self, ctx) -> None:
        if not self._already_took_the_interrupt:
            self._already_took_the_interrupt = True
            HOOKS_THE_INTERRUPT_PROBES_REACHED.append("process-interrupted")
            raise KeyboardInterrupt
        time.sleep(0.005)

    def stop(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("stop")

    def teardown(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("teardown")


@node(execution="manual")
class RaisesInStartProbe:
    """Sets up, then raises out of `start()`, which must fail its node."""

    frames_to_downstream: OutputPin[EncodedVideo]

    def start(self, ctx) -> None:
        raise FileNotFoundError('could not read "lower_third.riv"')


@node(execution="manual")
class InterruptedInSetupProbe:
    """Takes a `KeyboardInterrupt` inside `setup()`. Unlike a `setup()` that
    raises on its own, this one is still owed its `teardown()`."""

    frames_to_downstream: OutputPin[EncodedVideo]

    def setup(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("setup-interrupted")
        raise KeyboardInterrupt

    def teardown(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("teardown")


@node(execution="manual")
class RaisesInSetupProbe:
    """A `setup()` that raises on its own, which keeps the no-teardown rule."""

    frames_to_downstream: OutputPin[EncodedVideo]

    def setup(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("setup-raised")
        raise RuntimeError("this probe refuses to set up")

    def stop(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("stop")

    def teardown(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("teardown")


WHEN_THE_PACING_PROBE_PROCESSED_NS: list[int] = []


@node(execution="continuous", interval_ms=250)
def ContinuousPacingProbe(
    *,
    frames_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Records when each frame ran, so the interval the loop kept is
    measurable. The interval under test is the one the parent's `run` names."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        WHEN_THE_PACING_PROBE_PROCESSED_NS.append(time.monotonic_ns())

    return each_frame


# Whether `RaisesUntilToldNotToProbe.process()` raises on its next call; a
# test flips it to end a run of raises.
RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES = [True]


@node(execution="continuous", interval_ms=20)
def RaisesUntilToldNotToProbe(
    *,
    frames_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Raises on every frame until a test clears
    `RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES`, then runs cleanly."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        if RAISES_UNTIL_TOLD_NOT_TO_PROBE_RAISES[0]:
            raise RuntimeError("broke on purpose")

    return each_frame


@node
def builds_an_object_then_answers_its_level_raising_on_a_negative_one(level: float = 0.5) -> float:
    """Answers `level`, raising on a negative one; building an object keeps its
    body from compiling, so it runs as Python."""
    repr(object())
    if level < 0.0:
        raise RuntimeError("the function node broke on purpose")
    return level


@node(execution="continuous", interval_ms=20)
def ReadsTheIslandFeedingItsLevelProbe(
    *,
    level: InputPin[float],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads its `level` input, which a function node island feeds, on every
    `process()`."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        level.read()

    return each_frame


class ReconfigurableProbeConfig(TypedDict, total=False):
    gain: int


@node(execution="manual")
class ReleasesAStructureInTeardownProbe:
    """Builds an acceleration structure in `setup` and lets go of it in
    `teardown`, so the structure's release is owed while teardown answers."""

    frames_to_downstream: OutputPin[EncodedVideo]

    def setup(self, ctx) -> None:
        self._structure = ctx.gpu_full_access.build_triangles_blas(
            [0.0] * 9, [0, 1, 2], label="released-in-teardown"
        )

    def teardown(self, ctx) -> None:
        self._structure = None


@node
def hold_a_level(level: float = 0.75) -> float:
    return level


@node
def double_a_level(x: float = 0.0) -> float:
    return 2.0 * x


# What each frame of the island-reading probe read from `level`, `None`
# when nothing fed it. A test reads it to see which island fed the port.
LEVELS_THE_ISLAND_READING_PROBE_READ: "list[float | None]" = []


@node(execution="continuous", interval_ms=5)
def IslandReadingProbe(
    *,
    level: InputPin[float],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Records the control value each frame reads from `level`."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        LEVELS_THE_ISLAND_READING_PROBE_READ.append(level.read())

    return each_frame


@built_from_its_config_as_the_wheel_builds_a_promoted_function_node(
    output_ports=("frames_to_downstream",)
)
class NotReadyInStartProbe:
    """Reports itself not ready from `start()`."""

    def __init__(self, config: ReconfigurableProbeConfig) -> None:
        pass

    def start(self, ctx) -> None:
        ctx.report_not_ready("`riv_path` is not set", image_width=64, image_height=32)


CONTEXT_THE_REPORTED_FROM_ANOTHER_THREAD_PROBE_KEPT: "list[Any]" = []


@node(execution="manual")
def ReportedFromAnotherThreadProbe(
    *,
    video: OutputPin[Annotated[Texture, TexturePixelFormats("rgba8_unorm")]],
    ctx: RuntimeContextFullAccess,
) -> None:
    """Keeps the context its body is handed in
    `CONTEXT_THE_REPORTED_FROM_ANOTHER_THREAD_PROBE_KEPT`, so a test reports
    the node not ready or ready from a thread of its own."""

    CONTEXT_THE_REPORTED_FROM_ANOTHER_THREAD_PROBE_KEPT.append(ctx)


NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE_IS_NOT_READY = [True]
PROCESS_CALLS_OF_THE_NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE = [0]


@node(execution="continuous", interval_ms=20)
def NotReadyUntilToldOtherwiseProbe(
    *,
    video: OutputPin[Annotated[Texture, TexturePixelFormats("rgba8_unorm")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports itself not ready from each frame until a test clears
    `NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE_IS_NOT_READY`, then ready."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        PROCESS_CALLS_OF_THE_NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE[0] += 1
        if NOT_READY_UNTIL_TOLD_OTHERWISE_PROBE_IS_NOT_READY[0]:
            ctx.report_not_ready("the image asset `poster.png` is missing")
        else:
            ctx.report_ready()

    return each_frame


WHAT_THE_FIELDS_PROBE_SAW: "list[tuple[str, float, Any, int]]" = []


@node(execution="continuous", interval_ms=5)
class FieldsProbe:
    """Records, at `__init__` and at each `process()`, what its fields hold."""

    gain: float = 1.0
    tint: Color = Color(0.0, 0.0, 0.0)
    width: Annotated[int, SettingOnly()] = 4

    def __init__(self) -> None:
        WHAT_THE_FIELDS_PROBE_SAW.append(("init", self.gain, self.tint, self.width))

    def process(self, ctx) -> None:
        WHAT_THE_FIELDS_PROBE_SAW.append(("process", self.gain, self.tint, self.width))


class DeclaredByAnOlderTatolabStreamProbe:
    """A class stamped as an older `tatolab-stream`'s `@node.process` stamped one: declared,
    with its ports, and none of the fields and pins bare `@node` declares."""

    def process(self, ctx) -> None:
        pass


setattr(DeclaredByAnOlderTatolabStreamProbe, NODE_DECLARATION_DECLARED_STAMP, True)
setattr(DeclaredByAnOlderTatolabStreamProbe, NODE_DECLARATION_INPUT_PORTS_STAMP, [])
setattr(DeclaredByAnOlderTatolabStreamProbe, NODE_DECLARATION_OUTPUT_PORTS_STAMP, [])
