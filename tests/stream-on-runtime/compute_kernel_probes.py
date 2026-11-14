# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

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

from tatolab.stream import (
    ComputeKernel,
    GpuContextFullAccess,
    GpuContextLimitedAccess,
    GpuSurfaceHandle,
    KernelDispatchBatch,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)

SURFACE_WIDTH = 64
SURFACE_HEIGHT = 64

RESULT_MARKER = "MARKER:PROBE_RESULT "

# The two bindings differ in name and in kind, so a dispatch that resolved
# them by slot order rather than by name would bind them backwards.
SOURCE_BINDING = "source_image"
OUTPUT_BINDING = "output_image"

# Opaque and asymmetric across channels: a probe that read the wrong
# channel order, or somebody else's memory, cannot match by accident.
FILLED_SOURCE_RGBA = (10, 20, 30, 255)
DISCARDED_SOURCE_RGBA = (200, 210, 220, 255)

# One row of RGBA32F entries — the LUT shape the owner ruling named.
LUT_WIDTH = 256

READ_ONE_WRITE_ANOTHER_SLANG = """\
[[vk::binding(0, 0)]] Sampler2D source_image;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> output_image;
struct PushConstants { float bias; };
[[vk::push_constant]] ConstantBuffer<PushConstants> pc;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    int2 at = int2(invocation.xy);
    uint width;
    uint height;
    output_image.GetDimensions(width, height);
    if (at.x >= int(width) || at.y >= int(height)) { return; }
    float2 uv = (float2(at) + 0.5) / float2(width, height);
    float4 source = source_image.SampleLevel(uv, 0.0);
    output_image[at] = float4(1.0 - source.rgb + pc.bias, source.a);
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

    # Declared, not merely assigned: `setup` assigns them inside a nested
    # closure, which a type checker does not walk for attribute inference.
    gpu_full_access: GpuContextFullAccess
    gpu_limited_access: GpuContextLimitedAccess

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            gpu = ctx.gpu_full_access
            # Held for probes whose observation needs the capability itself
            # (a refusal at construction is observed by constructing), and
            # the Limited surface beside it for the by-id verbs — resolving
            # a surface and asking whether it takes a write-back.
            self.gpu_full_access = gpu
            self.gpu_limited_access = ctx.gpu_limited_access
            kernel = gpu.create_compute_kernel(
                source=READ_ONE_WRITE_ANOTHER_SLANG,
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


@node.meta(description="Reads one surface and writes another through a compute kernel")
@node.process(execution="manual")
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


@node.meta(description="Every way of getting the bindings wrong, refused by name")
@node.process(execution="manual")
class BindingRefusalProbe(_ComputeKernelProbeBase):
    """Unknown, missing and wrong-target refusals, each raising before any
    GPU work is submitted and each naming the shader's own bindings."""

    def observe(self, kernel, source, output) -> dict:
        group_count = (SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1)
        push_constants = b"\x00\x00\x00\x00"

        wrong_declaration = _refusal_of(
            lambda: self.gpu_full_access.create_compute_kernel(
                source=READ_ONE_WRITE_ANOTHER_SLANG,
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


@node.meta(description="A texture's pixels reach the CPU through the staged door")
@node.process(execution="manual")
class TextureBackedPixelsReachTheCpuProbe(_ComputeKernelProbeBase):
    """The staged CPU door end to end, with numpy as the only consumer.

    Fills an acquired texture through the CPU write door, dispatches a
    kernel that samples it, and reads the kernel's own output back through
    the CPU read door. No CUDA runtime and no GPU package take part: if
    either door were still refusing a texture backing, or the block-edge
    publish never landed in the surface's own allocation, the kernel would
    read something other than what was written and this reports it.
    """

    def observe(self, kernel, source, output) -> dict:
        source.lock(read_only=False)
        source.as_numpy()[:] = FILLED_SOURCE_RGBA
        source.unlock()

        # Re-read through a fresh scope: the edit was staged, so seeing it
        # here is the proof the block edge published it into the surface.
        source.lock(read_only=True)
        published_pixel = source.as_numpy()[11, 13].tolist()
        source.unlock()

        kernel.dispatch(
            bindings={SOURCE_BINDING: source, OUTPUT_BINDING: output},
            group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            push_constants=b"\x00\x00\x00\x00",
        )

        output.lock(read_only=True)
        kernel_output_pixel = output.as_numpy()[11, 13].tolist()
        output.unlock()

        return {
            "surface_id": output.surface_id,
            "width": output.width,
            "height": output.height,
            "source_surface_id": source.surface_id,
            "published_pixel": published_pixel,
            "kernel_output_pixel": kernel_output_pixel,
        }


@node.meta(description="A raise inside the CPU door over a texture propagates")
@node.process(execution="manual")
class TextureCpuDoorRaiseProbe(_ComputeKernelProbeBase):
    """A raise inside the CPU door over a texture, and the pixel the frame
    holds afterwards — the staged door's own on Linux, the stores made before
    the raise on macOS, where the door is the IOSurface itself."""

    def observe(self, kernel, source, output) -> dict:
        del kernel, output
        source.lock(read_only=False)
        source.as_numpy()[:] = FILLED_SOURCE_RGBA
        source.unlock()

        # A scope of its own over the same surface: leaving `with` closes
        # the handle it entered, and the re-read below needs `source` open.
        raised = None
        try:
            with self.gpu_limited_access.resolve_surface(source.surface_id) as scoped:
                scoped.lock(read_only=False)
                scoped.as_numpy()[:] = DISCARDED_SOURCE_RGBA
                raise RuntimeError("the edit does not finish")
        except RuntimeError as propagated:
            raised = str(propagated)

        source.lock(read_only=True)
        pixel_after_the_raise = source.as_numpy()[11, 13].tolist()
        source.unlock()
        return {
            "raised": raised,
            "pixel_after_the_raise": pixel_after_the_raise,
        }


@node.meta(description="An acquired texture takes a write-back without spelling copy usage")
@node.process(execution="manual")
class AcquiredTextureImpliesCopyUsageProbe(_ComputeKernelProbeBase):
    """The LUT flow the ruling widened #1758 to cover, spelled the way an
    author would: acquire with one usage token, fill it through the CPU
    write door, and read it back.

    `usage=["texture_binding"]` alone is enough because the engine implies
    both copy bits — so the door answers rather than refusing about a flag
    the author had no reason to name. Filling and re-reading is what makes
    that end to end: a mask that merely parsed right would still fail here
    if the copy the door records could not run against the allocation.
    """

    def observe(self, kernel, source, output) -> dict:
        del kernel, source, output
        lut = self.gpu_full_access.acquire_texture(
            LUT_WIDTH, 1, "rgba32_float", ["texture_binding"]
        )
        takes_a_write_back = self.gpu_limited_access.surface_can_take_write_back(
            lut.surface_id
        )

        # A ramp, so a read that answered the wrong row, a stale staging or
        # somebody else's memory cannot match by accident.
        curve = [
            [step / LUT_WIDTH, 1.0 - step / LUT_WIDTH, 0.25, 1.0]
            for step in range(LUT_WIDTH)
        ]
        lut.lock(read_only=False)
        lut.as_numpy()[0, :, :] = curve
        lut.unlock()

        lut.lock(read_only=True)
        published = lut.as_numpy()[0, :, :].tolist()
        lut.unlock()
        return {
            "surface_id": lut.surface_id,
            "takes_a_write_back": takes_a_write_back,
            "the_curve_published": published == curve,
            "first_and_last_entries": [published[0], published[-1]],
        }


BRIGHTEN_SLANG = """\
[[vk::binding(0, 0)]] Sampler2D unbrightened_image;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> brightened_image;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    float4 source = unbrightened_image.Load(int3(invocation.xy, 0));
    brightened_image[invocation.xy] = float4(source.rgb + 40.0 / 255.0, source.a);
}
"""

DOUBLE_SLANG = """\
[[vk::binding(0, 0)]] Sampler2D brightened_image;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> doubled_image;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    float4 source = brightened_image.Load(int3(invocation.xy, 0));
    doubled_image[invocation.xy] = float4(source.rgb * 2.0, source.a);
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
            self.brighten = gpu.create_compute_kernel(source=BRIGHTEN_SLANG)
            self.double = gpu.create_compute_kernel(source=DOUBLE_SLANG)
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


@node.meta(description="A two-pass filter dispatched as one batch")
@node.process(execution="manual")
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


@node.meta(description="A raise inside a batch scope submits nothing and propagates")
@node.process(execution="manual")
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


@node.meta(description="Every way of getting a batch wrong, refused by name")
@node.process(execution="manual")
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


@node.meta(description="Every way of getting the kernel's source wrong, refused by name")
@node.process(execution="manual")
class ShaderSourceRefusalProbe(_ComputeKernelProbeBase):
    """Slang source and pre-compiled SPIR-V are alternatives, and a shader that
    does not compile says so with the compiler's own diagnostic."""

    def observe(self, kernel, source, output) -> dict:
        del kernel, source, output
        gpu = self.gpu_full_access
        return {
            "neither": _refusal_of(lambda: gpu.create_compute_kernel()),
            "both": _refusal_of(
                lambda: gpu.create_compute_kernel(
                    source=READ_ONE_WRITE_ANOTHER_SLANG,
                    spirv=b"\x03\x02\x23\x07",
                )
            ),
            "does_not_compile": _refusal_of(
                lambda: gpu.create_compute_kernel(
                    source="[numthreads(1, 1, 1)]\nvoid main() { no_such_function(); }\n",
                )
            ),
            "missing_entry_point": _refusal_of(
                lambda: gpu.create_compute_kernel(
                    source=READ_ONE_WRITE_ANOTHER_SLANG,
                    push_constant_size=4,
                    entry_point="sharpen",
                )
            ),
            "glsl_source": _refusal_of(
                lambda: gpu.create_compute_kernel(
                    source="#version 450\nlayout(local_size_x = 1) in;\nvoid main() {}\n",
                )
            ),
        }
