# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The Python probe node whose compute kernel is a computed value sized by its `width` input,
which the computed kernel tests set while its stream plays."""

import json
import os
import statistics
import time

from tatolab.runtime._engine import NodeScopeBeingDeclared, computed
from tatolab.stream import ComputeKernel, RuntimeContextFullAccess, log, node

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
class RebuildsAKernelSizedByItsWidth:
    """Builds its fill kernel in a computed value that reads `width`, and a second kernel in its
    scope's body, both on the limited GPU capability; dispatches both every frame and reports each
    build, each close and each width a frame first dispatched, with how long each frame's
    dispatches took while a build was and was not in flight."""

    width: int = 8

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        gpu = ctx.gpu_limited_access
        self._target = gpu.acquire_texture(WIDEST_WORKGROUP, 1, "rgba8_unorm", TARGET_USAGE)
        # Counted by the build, on the scope's worker; a frame's dispatch overlapped a build when
        # one started before it ended that had not finished before it began.
        self._builds_started = 0
        self._builds_finished = 0
        self._dispatch_seconds: "dict[str, list[float]]" = {
            "while_a_build_ran": [],
            "while_no_build_ran": [],
        }
        self._width_last_written = int(self.width)
        declaring = NodeScopeBeingDeclared()
        self._width = declaring.input(self._width_last_written)

        def build_the_kernel() -> KernelOfAWidth:
            self._builds_started += 1
            try:
                width = self._width.get()
                kernel = gpu.create_compute_kernel(source=fill_kernel_of_workgroup_width(width))
            finally:
                self._builds_finished += 1
            report("KERNEL_BUILT", {"pid": os.getpid(), "width": width})
            return KernelOfAWidth(width, kernel)

        def body():
            self._body_kernel = gpu.create_compute_kernel(
                source=fill_kernel_of_workgroup_width(1)
            )
            report("BODY_KERNEL_CREATED", {"pid": os.getpid()})
            return computed(build_the_kernel)

        self._kernel = declaring.declare(body)
        self._scope = declaring.start()
        self._width_last_dispatched: "int | None" = None
        report("PROBE_SETUP", {"pid": os.getpid()})

    def process(self, ctx) -> None:
        width = int(self.width)
        if width != self._width_last_written:
            self._width_last_written = width
            self._scope.write([(self._width, width)])
        self._scope.run_frame(self._dispatch_one_frame)

    def _dispatch_one_frame(self) -> None:
        built = self._kernel.get()
        builds_finished_before = self._builds_finished
        started = time.perf_counter()
        built.kernel.dispatch(bindings={"filled_image": self._target}, group_count=(1, 1, 1))
        self._body_kernel.dispatch(bindings={"filled_image": self._target}, group_count=(1, 1, 1))
        elapsed = time.perf_counter() - started
        if self._builds_started > builds_finished_before:
            self._dispatch_seconds["while_a_build_ran"].append(elapsed)
        else:
            self._dispatch_seconds["while_no_build_ran"].append(elapsed)
        if built.width != self._width_last_dispatched:
            self._width_last_dispatched = built.width
            report("FRAME_DISPATCHED_WIDTH", {"pid": os.getpid(), "width": built.width})
            report("DISPATCH_SECONDS", self._dispatch_seconds_so_far())

    def _dispatch_seconds_so_far(self) -> "dict[str, object]":
        def summary(seconds: "list[float]") -> "dict[str, object]":
            if not seconds:
                return {"frames": 0}
            return {
                "frames": len(seconds),
                "median": statistics.median(seconds),
                "max": max(seconds),
            }

        return {name: summary(seconds) for name, seconds in self._dispatch_seconds.items()}

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        self._scope.close()
        self._body_kernel.close()
        report("PROBE_TEARDOWN", {"pid": os.getpid()})
