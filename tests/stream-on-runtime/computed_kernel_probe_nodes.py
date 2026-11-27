# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The Python probe node whose compute kernel is a computed value sized by its `width` input,
which the computed kernel tests set while its stream plays."""

import json
import math
import os
import statistics
import time
from typing import Callable

from tatolab.stream import (
    ComputeKernel,
    Input,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    computed,
    log,
    node,
)

#: The widest workgroup the tests set; the target is wide enough for every one of them.
WIDEST_WORKGROUP = 128

#: The texture every kernel fills, and what it is acquired for.
TARGET_USAGE = ["texture_binding", "storage_binding", "copy_src", "copy_dst"]


def report(marker_name: str, payload: "dict[str, object]") -> None:
    log.info(f"MARKER:{marker_name} {json.dumps(payload)}")


def fill_kernel_of_workgroup_width(width: int) -> str:
    """A kernel whose workgroup is `width` wide: each width is a different shader."""
    return f"""
[[vk::binding(0, 0)]] [format("rgba8")] RWTexture2D<float4> filled_image;
[numthreads({width}, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {{
    filled_image[invocation.xy] = float4(1.0, 0.0, 0.0, 1.0);
}}
"""


class KernelOfAWidth:
    """The computed value: a kernel and the width it was built for, closed when replaced."""

    def __init__(self, width: int, kernel: ComputeKernel) -> None:
        self.width = width
        self.kernel = kernel

    def close(self) -> None:
        self.kernel.close()
        report("KERNEL_CLOSED", {"pid": os.getpid(), "width": self.width})


@node(execution="continuous", interval_ms=10)
def RebuildsAKernelSizedByItsWidth(
    *,
    width: Input[int] = Input(8),
    ctx: RuntimeContextFullAccess,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Builds its fill kernel in a computed value that reads `width`, and a second kernel in its
    body, both on the limited GPU capability; dispatches both every frame and reports each
    build, each close and each width a frame first dispatched, with how long each frame's
    dispatches took while a build was and was not in flight."""
    gpu = ctx.gpu_limited_access
    target = gpu.acquire_texture(WIDEST_WORKGROUP, 1, "rgba8_unorm", TARGET_USAGE)
    # Counted by the build, on the scope's worker; a frame's dispatch overlapped a build when
    # one started before it ended that had not finished before it began.
    builds = {"started": 0, "finished": 0}
    dispatch_seconds: "dict[str, list[float]]" = {
        "while_a_build_ran": [],
        "while_no_build_ran": [],
    }

    def build_the_kernel() -> KernelOfAWidth:
        builds["started"] += 1
        try:
            built_width = width.get()
            kernel = gpu.create_compute_kernel(source=fill_kernel_of_workgroup_width(built_width))
        finally:
            builds["finished"] += 1
        report("KERNEL_BUILT", {"pid": os.getpid(), "width": built_width})
        return KernelOfAWidth(built_width, kernel)

    body_kernel = gpu.create_compute_kernel(source=fill_kernel_of_workgroup_width(1))
    report("BODY_KERNEL_CREATED", {"pid": os.getpid()})
    kernel_of_the_width = computed(build_the_kernel)
    width_last_dispatched: "int | None" = None
    report("PROBE_SETUP", {"pid": os.getpid()})

    def dispatch_seconds_so_far() -> "dict[str, object]":
        def summary(seconds: "list[float]") -> "dict[str, object]":
            if not seconds:
                return {"frames": 0}
            return {
                "frames": len(seconds),
                "median": statistics.median(seconds),
                # Nearest rank, so one slow frame among ten never sets it.
                "p90": sorted(seconds)[math.ceil(0.9 * len(seconds)) - 1],
                "max": max(seconds),
            }

        return {name: summary(seconds) for name, seconds in dispatch_seconds.items()}

    def each_frame(frame_ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal width_last_dispatched
        built = kernel_of_the_width.get()
        builds_finished_before = builds["finished"]
        started = time.perf_counter()
        built.kernel.dispatch(bindings={"filled_image": target}, group_count=(1, 1, 1))
        body_kernel.dispatch(bindings={"filled_image": target}, group_count=(1, 1, 1))
        elapsed = time.perf_counter() - started
        if builds["started"] > builds_finished_before:
            dispatch_seconds["while_a_build_ran"].append(elapsed)
        else:
            dispatch_seconds["while_no_build_ran"].append(elapsed)
        if built.width != width_last_dispatched:
            width_last_dispatched = built.width
            report("FRAME_DISPATCHED_WIDTH", {"pid": os.getpid(), "width": built.width})
            report("DISPATCH_SECONDS", dispatch_seconds_so_far())

    return each_frame
