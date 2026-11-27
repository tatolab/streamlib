# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The sources and the probe the `AlphaOverCompositor` scenarios wire around it.

Every node runs in a node interpreter of its own. The probe reports one
`MARKER:PROBE_RESULT` JSON line holding the composite's pixels; the test
compares them with a reference composite built with numpy.
"""

import json
import os
import struct
import traceback
from typing import Annotated, Any, Callable

from tatolab.stream import (
    ColorInfo,
    ComputeKernel,
    GpuContextLimitedAccess,
    Input,
    InputPin,
    NodeOutputTextureRing,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    TextureToWrite,
    computed,
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

_FILL_KERNEL_SLANG = """\
[[vk::binding(0, 0)]] [format("rgba8")] RWTexture2D<float4> filled_frame;
struct FillColour { float4 premultiplied_rgba; };
[[vk::push_constant]] ConstantBuffer<FillColour> fill;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint width;
    uint height;
    filled_frame.GetDimensions(width, height);
    if (invocation.x >= width || invocation.y >= height) { return; }
    filled_frame[invocation.xy] = fill.premultiplied_rgba;
}
"""


@node.meta(description="Publishes frames of one premultiplied RGBA colour")
@node(execution="continuous", interval_ms=SOURCE_INTERVAL_MS)
def SolidPremultipliedColourSource(
    *,
    video: OutputPin[Annotated[Texture, TexturePixelFormats("rgba8_unorm")]],
    width: Input[int] = Input(64),
    height: Input[int] = Input(64),
    premultiplied_rgba: Input[list[int]] = Input([0, 0, 0, 255]),
    ctx: RuntimeContextFullAccess,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Every frame is `premultiplied_rgba`, each channel 0-255, at `width`x`height`."""
    fill_push_constants = computed(
        lambda: struct.pack("<4f", *(channel / 255 for channel in premultiplied_rgba.get()))
    )
    frame_ring = NodeOutputTextureRing("rgba8_unorm", ["storage_binding", "texture_binding"])
    fill_kernel: ComputeKernel = ctx.gpu_limited_access.create_compute_kernel(
        source=_FILL_KERNEL_SLANG,
        push_constant_size=struct.calcsize("<4f"),
        bindings={"filled_frame": "storage_image"},
    )

    def each_frame(frame_ctx: RuntimeContextLimitedAccess) -> None:
        frame_width, frame_height = width.get(), height.get()
        frame_texture = frame_ring.next_texture_for_this_frame(
            frame_ctx.gpu_limited_access, frame_width, frame_height
        )
        fill_kernel.dispatch(
            bindings={"filled_frame": frame_texture},
            group_count=((frame_width + 7) // 8, (frame_height + 7) // 8, 1),
            push_constants=fill_push_constants.get(),
        )
        video.write(
            TextureToWrite.of_texture(frame_texture, ColorInfo()),
            timestamp_ns=frame_ctx.time,
        )

    return each_frame


@node.meta(description="Declares a video output and never publishes on it")
@node(execution="manual")
def NeverPublishingVideoSource(
    *,
    video: OutputPin[Annotated[Texture, TexturePixelFormats("rgba8_unorm")]],
) -> None:
    """Stands for a layer that is wired and has never sent a frame."""


def _pixels_of(gpu: GpuContextLimitedAccess, surface_id: str) -> "list[Any]":
    with gpu.resolve_surface(surface_id) as surface:
        surface.lock(read_only=True)
        pixels = surface.as_numpy().tolist()
        surface.unlock()
    return pixels


@node.meta(description="Reports the pixels of the composite it holds once the sources have settled")
@node(execution="continuous", interval_ms=10)
def AlphaOverCompositeProbe(
    *,
    composite_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]],
    report_every_seconds: Input[float] = Input(0.0),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Holds the newest composite and reports its pixels, or reports that none arrived; with
    `report_every_seconds` above 0, reports the newest composite again that often after."""

    reported = False
    reported_at_ns: "int | None" = None
    started_at_ns: "int | None" = None
    first_composite_at_ns: "int | None" = None
    composites_received = 0
    _newest_composite: "Texture | None" = None

    def reports_again_now(now_ns: int) -> bool:
        return (
            report_every_seconds.get() > 0
            and reported_at_ns is not None
            and now_ns - reported_at_ns >= report_every_seconds.get() * 1e9
        )

    def report(now_ns: int, observe: "Callable[[], dict[str, Any]]") -> None:
        nonlocal reported, reported_at_ns
        reported = True
        reported_at_ns = now_ns
        try:
            observation = observe()
        except BaseException:  # noqa: BLE001 — re-raised by the asserting test
            observation = {"failure": traceback.format_exc()}
        log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal _newest_composite, composites_received, first_composite_at_ns, started_at_ns
        if reported and not reports_again_now(ctx.time):
            return
        if started_at_ns is None:
            started_at_ns = ctx.time
        composite = composite_from_upstream.read()
        if composite is not None:
            composites_received += 1
            _newest_composite = composite
            if first_composite_at_ns is None:
                first_composite_at_ns = ctx.time

        if first_composite_at_ns is None:
            if ctx.time - started_at_ns >= FIRST_COMPOSITE_DEADLINE_SECONDS * 1e9:
                report(ctx.time, lambda: {"composites_received": 0})
            return
        if ctx.time - first_composite_at_ns < SETTLING_SECONDS_AFTER_THE_FIRST_COMPOSITE * 1e9:
            return
        newest_composite = _newest_composite
        assert newest_composite is not None
        gpu = ctx.gpu_limited_access
        report(
            ctx.time,
            lambda: {
                "composites_received": composites_received,
                "extent": [newest_composite.width, newest_composite.height],
                "pixels": _pixels_of(gpu, newest_composite.surface_id),
            },
        )

    return each_frame
