# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What the device exchange probes share: the surfaces they read, the constant
fill kernel and its pixel, the OPAQUE_FD usage set, and how each reports."""

import json
import os
import sys
import traceback
from typing import Annotated

from tatolab.stream import (
    ImageFrame,
    InputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    log,
)


SURFACE_WIDTH = 64
SURFACE_HEIGHT = 32

RESULT_MARKER = "MARKER:PROBE_RESULT "

# DLPack device-type discriminants, part of the wire ABI.
DLPACK_DEVICE_CPU = 1
DLPACK_DEVICE_CUDA = 2
DLPACK_DEVICE_METAL = 8

# The device a frame's natural DLPack side lives on, and torch's name for it.
NATURAL_DLPACK_DEVICE = DLPACK_DEVICE_METAL if sys.platform == "darwin" else DLPACK_DEVICE_CUDA
NATURAL_TORCH_DEVICE_TYPE = "mps" if sys.platform == "darwin" else "cuda"

#: An input that takes every frame in order.
FramesInOrder = Annotated[Texture, StreamDelivery("ordered")]


def report_device_exchange_probe_observation(probe_body) -> None:
    """One result line per probe, success or failure — the failure carries the
    traceback so the test fails on the cause rather than a missing marker."""
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


class DeviceExchangeFrameProbeBase:
    """Reads exactly one Texture frame, held against producer reuse while it lives, then reports through `report_device_exchange_probe_observation`."""

    video_from_upstream: InputPin[FramesInOrder]

    def __init__(self) -> None:
        self._frames_seen = 0

    def _probe(self, ctx: RuntimeContextLimitedAccess, frame: ImageFrame) -> dict:
        raise NotImplementedError

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = self.video_from_upstream.read()
        if frame is None or self._frames_seen >= 1:
            return
        self._frames_seen += 1
        report_device_exchange_probe_observation(lambda: self._probe(ctx, frame))


# Slang that fills its one bound texture with a constant — the smallest
# engine-kernel producer a cross-process texture consumer can stand behind.
# The constant is chosen to be exact in unorm8: 64, 128, 192, 255.
FILL_CONSTANT_SLANG = """\
[[vk::binding(0, 0)]] [format("rgba8")] RWTexture2D<float4> output_image;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint width;
    uint height;
    output_image.GetDimensions(width, height);
    if (invocation.x >= width || invocation.y >= height) { return; }
    output_image[invocation.xy] = float4(64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0, 1.0);
}
"""

FILL_CONSTANT_RGBA = [64, 128, 192, 255]


# The usage set that picks the OPAQUE_FD cross-process-importable allocation
# flavour: the OPAQUE_FD constructor's fixed set.
OPAQUE_FD_FLAVOUR_USAGE = ["texture_binding", "storage_binding", "copy_src", "copy_dst"]


# Wide enough that a GPU image's IOSurface pads its rows past `width * 4`;
# a pool pixel buffer's rows are packed, so it is the unpadded control.
PADDED_SURFACE_WIDTH = 1000
PADDED_SURFACE_HEIGHT = 8
