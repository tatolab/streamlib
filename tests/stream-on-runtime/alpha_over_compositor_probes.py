# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The sources and the probe the `AlphaOverCompositor` scenarios wire around it.

Every node runs in a processor interpreter of its own. The probe reports one
`MARKER:PROBE_RESULT` JSON line holding the composite's pixels; the test
compares them with a reference composite built with numpy.
"""

import json
import os
import struct
import traceback
from typing import Any, Callable, TypedDict

from tatolab.stream import (
    ColorInfo,
    ComputeKernel,
    GpuContextLimitedAccess,
    Image,
    ImageFrame,
    ImageFrameToWrite,
    Input,
    NodeOutputTextureRing,
    Output,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)

RESULT_MARKER = "MARKER:PROBE_RESULT "

#: Small enough that the whole composite fits one marker line.
CANVAS_WIDTH = 32
CANVAS_HEIGHT = 24

#: How often each colour source publishes.
SOURCE_INTERVAL_MS = 20

#: How long after its first composite the probe reports, so every source that
#: publishes at all has been composited by then, whichever helper started last.
SETTLING_SECONDS_AFTER_THE_FIRST_COMPOSITE = 3.0

#: How long the probe waits for a first composite before reporting none.
FIRST_COMPOSITE_DEADLINE_SECONDS = 8.0

_FILL_KERNEL_GLSL = """\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0, rgba8) uniform writeonly image2D filled_frame;
layout(push_constant) uniform FillColour { vec4 premultiplied_rgba; } fill;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(filled_frame);
    if (at.x >= extent.x || at.y >= extent.y) { return; }
    imageStore(filled_frame, at, fill.premultiplied_rgba);
}
"""


class SolidPremultipliedColourSourceConfig(TypedDict):
    width: int
    height: int
    premultiplied_rgba: "list[int]"


@node(
    execution="continuous",
    interval_ms=SOURCE_INTERVAL_MS,
    description="Publishes frames of one premultiplied RGBA colour",
)
class SolidPremultipliedColourSource:
    """Every frame is `premultiplied_rgba`, each channel 0-255, at `width`x`height`."""

    video = Output(Image, formats=("rgba8_unorm",))

    def __init__(self, config: SolidPremultipliedColourSourceConfig) -> None:
        self.width = config["width"]
        self.height = config["height"]
        self.fill_push_constants = struct.pack(
            "<4f", *(channel / 255 for channel in config["premultiplied_rgba"])
        )
        self.frame_ring = NodeOutputTextureRing("rgba8_unorm", ["storage_binding", "texture_binding"])

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.fill_kernel: ComputeKernel = ctx.gpu_full_access.create_compute_kernel(
            source=_FILL_KERNEL_GLSL,
            push_constant_size=len(self.fill_push_constants),
            bindings={"filled_frame": "storage_image"},
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame_texture = self.frame_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, self.width, self.height
        )
        self.fill_kernel.dispatch(
            bindings={"filled_frame": frame_texture},
            group_count=((self.width + 7) // 8, (self.height + 7) // 8, 1),
            push_constants=self.fill_push_constants,
        )
        ctx.outputs.write(
            self.video,
            ImageFrameToWrite.of_texture(frame_texture, ColorInfo()),
            timestamp_ns=ctx.time,
        )


@node(execution="manual", description="Declares a video output and never publishes on it")
class NeverPublishingVideoSource:
    """Stands for a layer that is wired and has never sent a frame."""

    video = Output(Image)


def _pixels_of(gpu: GpuContextLimitedAccess, surface_id: str) -> "list[Any]":
    with gpu.resolve_surface(surface_id) as surface:
        surface.lock(read_only=True)
        pixels = surface.as_numpy().tolist()
        surface.unlock()
    return pixels


@node(
    execution="continuous",
    interval_ms=10,
    description="Reports the pixels of the composite it holds once the sources have settled",
)
class AlphaOverCompositeProbe:
    """Holds the newest composite and reports its pixels, or reports that none arrived."""

    composite_from_upstream = Input(Image, delivery="newest")

    def __init__(self) -> None:
        self.reported = False
        self.started_at_ns: "int | None" = None
        self.first_composite_at_ns: "int | None" = None
        self.composites_received = 0
        self.newest_composite: "ImageFrame | None" = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.reported:
            return
        if self.started_at_ns is None:
            self.started_at_ns = ctx.time
        composite = ctx.inputs.read(self.composite_from_upstream)
        if composite is not None:
            self.composites_received += 1
            self.newest_composite = composite
            if self.first_composite_at_ns is None:
                self.first_composite_at_ns = ctx.time

        if self.first_composite_at_ns is None:
            if ctx.time - self.started_at_ns >= FIRST_COMPOSITE_DEADLINE_SECONDS * 1e9:
                self._report(lambda: {"composites_received": 0})
            return
        if ctx.time - self.first_composite_at_ns < SETTLING_SECONDS_AFTER_THE_FIRST_COMPOSITE * 1e9:
            return
        newest_composite = self.newest_composite
        assert newest_composite is not None
        gpu = ctx.gpu_limited_access
        self._report(
            lambda: {
                "composites_received": self.composites_received,
                "extent": [newest_composite.width, newest_composite.height],
                "pixels": _pixels_of(gpu, newest_composite.surface_id),
            }
        )

    def _report(self, observe: "Callable[[], dict[str, Any]]") -> None:
        self.reported = True
        try:
            observation = observe()
        except BaseException:  # noqa: BLE001 — re-raised by the asserting test
            observation = {"failure": traceback.format_exc()}
        log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))
