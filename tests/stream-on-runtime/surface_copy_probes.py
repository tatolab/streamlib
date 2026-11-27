# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probes for the engine's surface-to-surface copy, from a node interpreter.

A frame lands in a kernel's input texture through the engine: no array
library takes part in the landing. numpy appears here only to check the
pixels afterwards, the way a test would.
"""

import json
import os
import sys
import traceback
from typing import Annotated, Callable

import numpy

from tatolab.stream import (
    ComputeKernel,
    GpuSurfaceHandle,
    Input,
    InputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    log,
    node,
)

RESULT_MARKER = "MARKER:PROBE_RESULT "

FRAME_WIDTH = 64
FRAME_HEIGHT = 64

INVERT_SLANG = """\
[[vk::binding(0, 0)]] [format("rgba8")] RWTexture2D<float4> landed_frame;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> inverted_frame;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint width;
    uint height;
    inverted_frame.GetDimensions(width, height);
    if (invocation.x >= width || invocation.y >= height) { return; }
    float4 landed = landed_frame[invocation.xy];
    inverted_frame[invocation.xy] = float4(1.0 - landed.rgb, landed.a);
}
"""

TEXTURE_USAGE = ["storage_binding", "copy_src", "copy_dst"]


def _report(probe_body) -> None:
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


def _refusal_of(copy_body) -> str:
    try:
        copy_body()
    except Exception as refusal:  # noqa: BLE001 — the refusal is the subject
        return str(refusal)
    raise AssertionError("the copy was accepted; it should have been refused")


def _pixels_of(surface: GpuSurfaceHandle) -> numpy.ndarray:
    surface.lock(read_only=True)
    pixels = surface.as_numpy().copy()
    surface.unlock()
    return pixels


@node(execution="reactive")
def FrameLandingProbe(
    *,
    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
    skip_copy: Input[bool] = Input(False),
    ctx: RuntimeContextFullAccess,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Lands the first test-pattern frame in an acquired texture with the
    engine copy, inverts it with a kernel, and reports whether the kernel's
    output is the inverted frame.

    `skip_copy` is the negative control: the kernel then reads a texture
    nothing landed a frame in, and the check must fail.
    """
    gpu_full_access = ctx.gpu_full_access
    kernel: ComputeKernel = ctx.gpu_limited_access.create_compute_kernel(
        source=INVERT_SLANG,
        bindings={"landed_frame": "storage_image", "inverted_frame": "storage_image"},
    )
    landing_texture: GpuSurfaceHandle = gpu_full_access.acquire_texture(
        FRAME_WIDTH, FRAME_HEIGHT, "rgba8_unorm", TEXTURE_USAGE
    )
    inverted_texture: GpuSurfaceHandle = gpu_full_access.acquire_texture(
        FRAME_WIDTH, FRAME_HEIGHT, "rgba8_unorm", TEXTURE_USAGE
    )
    reported = False

    def each_frame(frame_ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        frame = video_from_upstream.read()
        if frame is None or reported:
            return
        reported = True
        gpu = frame_ctx.gpu_limited_access
        copies_the_frame = not skip_copy.get()

        def land_and_invert() -> dict:
            claim = gpu.claim_surface_against_producer_reuse(frame.surface_id)
            if copies_the_frame:
                gpu.copy_surface_to_surface(frame.surface_id, landing_texture)
            array_libraries_imported = sorted(
                name for name in ("cupy", "torch") if name in sys.modules
            )
            kernel.dispatch(
                bindings={
                    "landed_frame": landing_texture,
                    "inverted_frame": inverted_texture,
                },
                group_count=(FRAME_WIDTH // 8, FRAME_HEIGHT // 8, 1),
            )
            with gpu.resolve_surface(frame.surface_id) as source:
                source_pixels = _pixels_of(source)
            del claim
            expected = source_pixels.copy()
            expected[:, :, :3] = 255 - expected[:, :, :3]
            inverted = _pixels_of(inverted_texture)
            return {
                "array_libraries_imported": array_libraries_imported,
                "source_is_not_uniform": bool((source_pixels != source_pixels[0, 0]).any()),
                "mismatched_pixels": int((inverted != expected).any(axis=2).sum()),
            }

        _report(land_and_invert)

    return each_frame


@node(execution="reactive")
def CopyRefusalProbe(
    *,
    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Asks for each copy the engine must refuse, and reports what it said."""

    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        frame = video_from_upstream.read()
        if frame is None or reported:
            return
        reported = True
        gpu = ctx.gpu_limited_access

        def refusals() -> dict:
            bgra_texture = gpu.acquire_texture(
                FRAME_WIDTH, FRAME_HEIGHT, "bgra8_unorm", TEXTURE_USAGE
            )
            wider_texture = gpu.acquire_texture(
                FRAME_WIDTH * 2, FRAME_HEIGHT, "rgba8_unorm", TEXTURE_USAGE
            )
            return {
                "format_mismatch": _refusal_of(
                    lambda: gpu.copy_surface_to_surface(frame.surface_id, bgra_texture)
                ),
                "extent_mismatch": _refusal_of(
                    lambda: gpu.copy_surface_to_surface(frame.surface_id, wider_texture)
                ),
            }

        _report(refusals)

    return each_frame
