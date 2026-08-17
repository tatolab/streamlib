# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for named N-binding compute dispatch, from where a kernel really runs.

A kernel is an object: built in `setup()` where the capability is Full,
dispatched per frame in `process()`. Every probe runs in its own helper
process and reports one `MARKER:PROBE_RESULT` JSON line.

What is worth breaking a build over is that a Python processor can read one
surface and write a different one — the pass the v1 wire could not express —
and that every way of getting the bindings wrong is refused by name before any
GPU work is submitted.
"""

import json
import os
import traceback

from streamlib import (
    GpuContextFullAccess,
    GpuSurfaceHandle,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    processor,
)
from streamlib._engine import ComputeKernel, KernelDispatchBatch

SURFACE_WIDTH = 64
SURFACE_HEIGHT = 64

RESULT_MARKER = "MARKER:PROBE_RESULT "

# The two bindings differ in name and in kind, so a dispatch that resolved
# them by slot order rather than by name would bind them backwards.
SOURCE_BINDING = "source_image"
OUTPUT_BINDING = "output_image"

READ_ONE_WRITE_ANOTHER_GLSL = """\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D output_image;
layout(push_constant) uniform PushConstants { float bias; } pc;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(output_image);
    if (at.x >= extent.x || at.y >= extent.y) { return; }
    vec2 uv = (vec2(at) + 0.5) / vec2(extent);
    vec4 source = texture(source_image, uv);
    imageStore(output_image, at, vec4(1.0 - source.rgb + pc.bias, source.a));
}
"""


def _report(probe_body) -> None:
    """One result line per probe, success or failure — the failure carries the
    traceback so the test fails on the cause rather than a missing marker."""
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


def _refusal_of(dispatch_body) -> str:
    """The message a wrong dispatch raises, or a failure if it did not raise."""
    try:
        dispatch_body()
    except Exception as refusal:  # noqa: BLE001 — the refusal is the subject
        return str(refusal)
    raise AssertionError("the dispatch was accepted; it should have been refused")


class _ComputeKernelProbeBase:
    """Builds the conformance kernel in `setup`, reports from `setup`.

    Nothing upstream is needed: the probe acquires both surfaces itself, which
    is the point — a kernel output is an engine-owned texture the processor
    names, not something handed to it.
    """

    # Declared, not merely assigned: `setup` assigns it inside a nested
    # closure, which a type checker does not walk for attribute inference.
    gpu_full_access: GpuContextFullAccess

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            gpu = ctx.gpu_full_access
            # Held for probes whose observation needs the capability itself
            # (a refusal at construction is observed by constructing).
            self.gpu_full_access = gpu
            kernel = gpu.create_compute_kernel(
                source=READ_ONE_WRITE_ANOTHER_GLSL,
                push_constant_size=4,
                # The declaration asserts what the shader must reflect; a
                # disagreement refuses at construction.
                bindings={
                    SOURCE_BINDING: "sampled_texture",
                    OUTPUT_BINDING: "storage_image",
                },
            )
            source = gpu.acquire_texture(
                SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm",
                ["texture_binding", "storage_binding", "copy_src", "copy_dst"],
            )
            output = gpu.acquire_texture(
                SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm",
                ["texture_binding", "storage_binding", "copy_src", "copy_dst"],
            )
            return self.observe(kernel, source, output)

        _report(observe)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        pass

    def observe(self, kernel, source, output) -> dict:
        raise NotImplementedError


@processor(
    execution="manual",
    description="Reads one surface and writes another through a compute kernel",
)
class ReadOneWriteAnotherProbe(_ComputeKernelProbeBase):
    """The pass the v1 wire could not express."""

    def observe(self, kernel, source, output) -> dict:
        kernel.dispatch(
            bindings={SOURCE_BINDING: source, OUTPUT_BINDING: output},
            group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            push_constants=b"\x00\x00\x00\x00",
        )
        return {
            "dispatched": True,
            "binding_names": list(kernel.binding_names),
            "source_surface_id": source.surface_id,
            "output_surface_id": output.surface_id,
            "surfaces_are_distinct": source.surface_id != output.surface_id,
        }


@processor(
    execution="manual",
    description="Every way of getting the bindings wrong, refused by name",
)
class BindingRefusalProbe(_ComputeKernelProbeBase):
    """Unknown, missing and wrong-target refusals, each raising before any
    GPU work is submitted and each naming the shader's own bindings."""

    def observe(self, kernel, source, output) -> dict:
        group_count = (SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1)
        push_constants = b"\x00\x00\x00\x00"

        wrong_declaration = _refusal_of(
            lambda: self.gpu_full_access.create_compute_kernel(
                source=READ_ONE_WRITE_ANOTHER_GLSL,
                push_constant_size=4,
                bindings={"sharpen_amount": "storage_buffer"},
            )
        )

        unknown = _refusal_of(
            lambda: kernel.dispatch(
                bindings={
                    SOURCE_BINDING: source,
                    OUTPUT_BINDING: output,
                    "sharpen_amount": output,
                },
                group_count=group_count,
                push_constants=push_constants,
            )
        )
        missing = _refusal_of(
            lambda: kernel.dispatch(
                bindings={SOURCE_BINDING: source},
                group_count=group_count,
                push_constants=push_constants,
            )
        )
        wrong_push_constant_size = _refusal_of(
            lambda: kernel.dispatch(
                bindings={SOURCE_BINDING: source, OUTPUT_BINDING: output},
                group_count=group_count,
                push_constants=b"\x00",
            )
        )
        unregistered_surface = _refusal_of(
            lambda: kernel.dispatch(
                bindings={SOURCE_BINDING: source, OUTPUT_BINDING: "no-such-surface"},
                group_count=group_count,
                push_constants=push_constants,
            )
        )
        return {
            "wrong_declaration": wrong_declaration,
            "unknown": unknown,
            "missing": missing,
            "wrong_push_constant_size": wrong_push_constant_size,
            "unregistered_surface": unregistered_surface,
            "binding_names": list(kernel.binding_names),
        }


@processor(
    execution="manual",
    description="A device texture's pixels are not addressable in this process",
)
class TextureIsNotLocallyMappedProbe(_ComputeKernelProbeBase):
    """`acquire_texture` mints a name, not a mapping. Asking for the pixels
    says so, rather than answering with somebody else's memory."""

    def observe(self, kernel, source, output) -> dict:
        del kernel
        try:
            output.lock(read_only=True)
            output.as_numpy()
            pixels_refusal = None
        except Exception as refusal:  # noqa: BLE001 — the refusal is the subject
            pixels_refusal = str(refusal)
        return {
            "surface_id": output.surface_id,
            "width": output.width,
            "height": output.height,
            "pixels_refusal": pixels_refusal,
            "source_surface_id": source.surface_id,
        }


BRIGHTEN_GLSL = """\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D unbrightened_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D brightened_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    vec4 source = texelFetch(unbrightened_image, at, 0);
    imageStore(brightened_image, at, vec4(source.rgb + 40.0 / 255.0, source.a));
}
"""

DOUBLE_GLSL = """\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D brightened_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D doubled_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    vec4 source = texelFetch(brightened_image, at, 0);
    imageStore(doubled_image, at, vec4(source.rgb * 2.0, source.a));
}
"""

GROUP_COUNT = (SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1)


class _TwoPassProbeBase:
    """Builds the two-pass chain the batch scope exists for, and the three
    surfaces it runs over.

    Written the way the change file spells it: two kernels constructed in
    `setup()`, an intermediate surface neither the source nor the output.
    """

    # Declared, not merely assigned: `setup` assigns them inside a nested
    # closure, which a type checker does not walk for attribute inference.
    gpu_full_access: GpuContextFullAccess
    brighten: ComputeKernel
    double: ComputeKernel
    source: GpuSurfaceHandle
    intermediate: GpuSurfaceHandle
    output: GpuSurfaceHandle

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            gpu = ctx.gpu_full_access
            self.gpu_full_access = gpu
            self.brighten = gpu.create_compute_kernel(source=BRIGHTEN_GLSL)
            self.double = gpu.create_compute_kernel(source=DOUBLE_GLSL)
            usage = ["texture_binding", "storage_binding", "copy_src", "copy_dst"]
            self.source = gpu.acquire_texture(
                SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", usage
            )
            self.intermediate = gpu.acquire_texture(
                SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", usage
            )
            self.output = gpu.acquire_texture(
                SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", usage
            )
            return self.observe()

        _report(observe)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        pass

    def brighten_then_double(self, batch: KernelDispatchBatch) -> None:
        """The two passes, in order, into whatever batch is handed in."""
        batch.dispatch(
            self.brighten,
            bindings={
                "unbrightened_image": self.source,
                "brightened_image": self.intermediate,
            },
            group_count=GROUP_COUNT,
        )
        batch.dispatch(
            self.double,
            bindings={
                "brightened_image": self.intermediate,
                "doubled_image": self.output,
            },
            group_count=GROUP_COUNT,
        )

    def observe(self) -> dict:
        raise NotImplementedError


@processor(
    execution="manual",
    description="A two-pass filter dispatched as one batch",
)
class TwoPassBatchProbe(_TwoPassProbeBase):
    """The demo: a chain whose intermediate is written by pass 1 and read by
    pass 2, inside one scope."""

    def observe(self) -> dict:
        with self.gpu_full_access.kernel_dispatch_batch() as batch:
            self.brighten_then_double(batch)
        first_scope_returned = True

        # A second scope over the same surfaces: the engine's batch recorder
        # is shared and long-lived, so if the first scope had left it open
        # this is where `begin()` refuses.
        second_scope_error = None
        try:
            with self.gpu_full_access.kernel_dispatch_batch() as second:
                self.brighten_then_double(second)
        except Exception as refused:  # noqa: BLE001 — reported, then asserted on
            second_scope_error = str(refused)

        return {
            "first_scope_returned": first_scope_returned,
            "second_scope_error": second_scope_error,
            "source_surface_id": self.source.surface_id,
            "intermediate_surface_id": self.intermediate.surface_id,
            "output_surface_id": self.output.surface_id,
        }


@processor(
    execution="manual",
    description="A raise inside a batch scope submits nothing and propagates",
)
class BatchExceptionProbe(_TwoPassProbeBase):
    """Half of a multi-pass filter is not what the author wrote, so a raise
    inside the scope discards the whole batch — and the exception is not
    swallowed on the way out."""

    def observe(self) -> dict:
        class TheProbesOwnFailure(Exception):
            pass

        propagated = None
        try:
            with self.gpu_full_access.kernel_dispatch_batch() as batch:
                self.brighten_then_double(batch)
                raise TheProbesOwnFailure("the block did not finish")
        except TheProbesOwnFailure as raised:
            propagated = str(raised)

        # The capability is unharmed: a fresh batch runs after the discarded
        # one, which is what a stranded recorder in the parent would refuse.
        with self.gpu_full_access.kernel_dispatch_batch() as batch:
            self.brighten_then_double(batch)

        return {
            "propagated": propagated,
            "dispatched_after_the_raise": True,
        }


@processor(
    execution="manual",
    description="Every way of getting a batch wrong, refused by name",
)
class BatchRefusalProbe(_TwoPassProbeBase):
    """A batch refuses in the caller's own stack: a wrong binding at the
    `dispatch` line, a repeated kernel at the line that repeats it, and a
    closed scope when it is dispatched into again."""

    def observe(self) -> dict:
        with self.gpu_full_access.kernel_dispatch_batch() as batch:
            unknown = _refusal_of(
                lambda: batch.dispatch(
                    self.brighten,
                    bindings={
                        "unbrightened_image": self.source,
                        "brightened_image": self.intermediate,
                        "sharpen_amount": self.output,
                    },
                    group_count=GROUP_COUNT,
                )
            )
            batch.dispatch(
                self.brighten,
                bindings={
                    "unbrightened_image": self.source,
                    "brightened_image": self.intermediate,
                },
                group_count=GROUP_COUNT,
            )
            same_kernel_twice = _refusal_of(
                lambda: batch.dispatch(
                    self.brighten,
                    bindings={
                        "unbrightened_image": self.intermediate,
                        "brightened_image": self.output,
                    },
                    group_count=GROUP_COUNT,
                )
            )
            closed_batch = batch

        after_the_scope = _refusal_of(
            lambda: closed_batch.dispatch(
                self.double,
                bindings={
                    "brightened_image": self.intermediate,
                    "doubled_image": self.output,
                },
                group_count=GROUP_COUNT,
            )
        )

        # Never entered: nothing would ever send what it collected, so it
        # refuses rather than swallowing the work.
        never_entered = _refusal_of(
            lambda: self.gpu_full_access.kernel_dispatch_batch().dispatch(
                self.double,
                bindings={
                    "brightened_image": self.intermediate,
                    "doubled_image": self.output,
                },
                group_count=GROUP_COUNT,
            )
        )
        return {
            "unknown": unknown,
            "same_kernel_twice": same_kernel_twice,
            "after_the_scope": after_the_scope,
            "never_entered": never_entered,
        }


@processor(
    execution="manual",
    description="Every way of getting the kernel's source wrong, refused by name",
)
class ShaderSourceRefusalProbe(_ComputeKernelProbeBase):
    """GLSL source and pre-compiled SPIR-V are alternatives, and a shader that
    does not compile says so with the compiler's own diagnostic."""

    def observe(self, kernel, source, output) -> dict:
        del kernel, source, output
        gpu = self.gpu_full_access
        return {
            "neither": _refusal_of(lambda: gpu.create_compute_kernel()),
            "both": _refusal_of(
                lambda: gpu.create_compute_kernel(
                    source=READ_ONE_WRITE_ANOTHER_GLSL,
                    spirv=b"\x03\x02\x23\x07",
                )
            ),
            "does_not_compile": _refusal_of(
                lambda: gpu.create_compute_kernel(
                    source="#version 450\nvoid main() { no_such_function(); }\n",
                )
            ),
            "non_main_entry_point": _refusal_of(
                lambda: gpu.create_compute_kernel(
                    source=READ_ONE_WRITE_ANOTHER_GLSL,
                    push_constant_size=4,
                    entry_point="sharpen",
                )
            ),
        }
