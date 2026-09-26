# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for the engine's surface-to-surface copy, from a helper process.

A frame lands in a kernel's input texture through the engine: no array
library takes part in the landing. numpy appears here only to check the
pixels afterwards, the way a test would.
"""

import json
import os
import sys
import traceback
from typing import TypedDict

import numpy

from streamlib import (
    GpuSurfaceHandle,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    processor,
)
from streamlib._engine import ComputeKernel

RESULT_MARKER = "MARKER:PROBE_RESULT "

FRAME_WIDTH = 64
FRAME_HEIGHT = 64

INVERT_GLSL = """\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0, rgba8) uniform readonly image2D landed_frame;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D inverted_frame;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(inverted_frame);
    if (at.x >= extent.x || at.y >= extent.y) { return; }
    vec4 landed = imageLoad(landed_frame, at);
    imageStore(inverted_frame, at, vec4(1.0 - landed.rgb, landed.a));
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


class FrameLandingProbeConfig(TypedDict, total=False):
    skip_copy: bool


@processor
class FrameLandingProbe:
    """Lands the first test-pattern frame in an acquired texture with the
    engine copy, inverts it with a kernel, and reports whether the kernel's
    output is the inverted frame.

    `skip_copy` is the negative control: the kernel then reads a texture
    nothing landed a frame in, and the check must fail.
    """

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> None: ...

    kernel: ComputeKernel
    landing_texture: GpuSurfaceHandle
    inverted_texture: GpuSurfaceHandle

    def __init__(self, config: FrameLandingProbeConfig) -> None:
        self.skip_copy = config.get("skip_copy", False)
        self.reported = False

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        gpu = ctx.gpu_full_access
        self.kernel = gpu.create_compute_kernel(
            source=INVERT_GLSL,
            bindings={"landed_frame": "storage_image", "inverted_frame": "storage_image"},
        )
        self.landing_texture = gpu.acquire_texture(
            FRAME_WIDTH, FRAME_HEIGHT, "rgba8_unorm", TEXTURE_USAGE
        )
        self.inverted_texture = gpu.acquire_texture(
            FRAME_WIDTH, FRAME_HEIGHT, "rgba8_unorm", TEXTURE_USAGE
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None or self.reported:
            return
        self.reported = True
        frame = VideoFrame.from_bag(bag)
        gpu = ctx.gpu_limited_access

        def land_and_invert() -> dict:
            claim = gpu.claim_surface_against_producer_reuse(frame.surface_id)
            if not self.skip_copy:
                gpu.copy_surface_to_surface(frame.surface_id, self.landing_texture)
            array_libraries_imported = sorted(
                name for name in ("cupy", "torch") if name in sys.modules
            )
            self.kernel.dispatch(
                bindings={
                    "landed_frame": self.landing_texture,
                    "inverted_frame": self.inverted_texture,
                },
                group_count=(FRAME_WIDTH // 8, FRAME_HEIGHT // 8, 1),
            )
            with gpu.resolve_surface(frame.surface_id) as source:
                source_pixels = _pixels_of(source)
            del claim
            expected = source_pixels.copy()
            expected[:, :, :3] = 255 - expected[:, :, :3]
            inverted = _pixels_of(self.inverted_texture)
            return {
                "array_libraries_imported": array_libraries_imported,
                "source_is_not_uniform": bool((source_pixels != source_pixels[0, 0]).any()),
                "mismatched_pixels": int((inverted != expected).any(axis=2).sum()),
            }

        _report(land_and_invert)


@processor
class CopyRefusalProbe:
    """Asks for each copy the engine must refuse, and reports what it said."""

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None or self.reported:
            return
        self.reported = True
        frame = VideoFrame.from_bag(bag)
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
