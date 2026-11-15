# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probes that run a scope over device memory, write through MLX or torch for the
engine to read on the GPU, and hold an array past the texture it came from."""

import numpy

from device_exchange_probe_shared_surfaces_and_reporting import (
    FILL_CONSTANT_SLANG,
    NATURAL_DLPACK_DEVICE,
    OPAQUE_FD_FLAVOUR_USAGE,
    SURFACE_HEIGHT,
    SURFACE_WIDTH,
    DeviceExchangeFrameProbeBase,
    report_device_exchange_probe_observation,
)
from tatolab.stream import (
    RuntimeContextFullAccess,
    node,
)


# The float fill, chosen exact in float16 and doubled exactly by the scope
# demo: (0.25, 0.5, 1.5, 2.0) -> (0.5, 1.0, 3.0, 4.0).
FILL_FLOAT_SLANG = """\
[[vk::binding(0, 0)]] [format("rgba16f")] RWTexture2D<float4> output_image;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint width;
    uint height;
    output_image.GetDimensions(width, height);
    if (invocation.x >= width || invocation.y >= height) { return; }
    output_image[invocation.xy] = float4(0.25, 0.5, 1.5, 2.0);
}
"""

FILL_FLOAT_RGBA = [0.25, 0.5, 1.5, 2.0]
DOUBLED_FLOAT_RGBA = [0.5, 1.0, 3.0, 4.0]


@node.process(execution="manual")
class DeviceTensorScopeDoublesAKernelOutputProbe:
    """The demo: torch doubles a kernel output in place through the scope.

    An rgba16_float output — the common HDR compute shape — reaches torch as
    a float16 tensor, `mul_(2.0)` edits it in place, and leaving the scope
    blits the edit back into the engine's texture. A second scope entry
    re-blits from the texture, so doubled values there prove the write-back
    reached the texture rather than lingering in the staging.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import torch

        fill_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FILL_FLOAT_SLANG,
            bindings={"output_image": "storage_image"},
        )
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba16_float", OPAQUE_FD_FLAVOUR_USAGE
        ) as kernel_output:
            fill_kernel.dispatch(
                bindings={"output_image": kernel_output},
                group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            )
            if kernel_output.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                return {"device_unavailable": "device side not reachable"}
            observation: dict = {"surface_id": kernel_output.surface_id}
            # Deliberately no torch.accelerator.synchronize(): the scope's exit
            # runs a device-wide synchronize before the engine's copy
            # reads the staging, and this probe is what proves it.
            with kernel_output.as_device_tensor() as tensor:
                torch_view = torch.from_dlpack(tensor)
                observation["tensor_dtype"] = str(torch_view.dtype)
                observation["tensor_shape"] = list(torch_view.shape)
                observation["tensor_device"] = str(torch_view.device)
                observation["filled_pixel"] = (
                    torch_view[3, 5].to(torch.float32).cpu().tolist()
                )
                torch_view.mul_(2.0)

            with kernel_output.as_device_tensor() as reread:
                observation["doubled_pixel"] = (
                    torch.from_dlpack(reread)[3, 5].to(torch.float32).cpu().tolist()
                )
            return observation


@node.process(execution="manual")
class DeviceTensorScopeDiscardsOnRaiseProbe:
    """A raise mid-scope leaves the surface holding its pre-scope content.

    The write did not finish, so publishing it would hand downstream a torn
    frame; the scope discards instead, the exception propagates unsuppressed,
    and the surface — and the kernel that writes it — keep working afterwards.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import torch

        fill_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FILL_CONSTANT_SLANG,
            bindings={"output_image": "storage_image"},
        )
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", OPAQUE_FD_FLAVOUR_USAGE
        ) as kernel_output:
            fill_kernel.dispatch(
                bindings={"output_image": kernel_output},
                group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            )
            if kernel_output.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                return {"device_unavailable": "device side not reachable"}
            observation = {}
            exception_seen = None
            try:
                with kernel_output.as_device_tensor() as tensor:
                    torch_view = torch.from_dlpack(tensor)
                    torch_view[:, :, :] = 0
                    # Not publish ordering — the discard needs the garbage
                    # write to have LANDED in the staging, or leaving it
                    # unpublished would prove nothing.
                    torch.accelerator.synchronize()
                    raise ValueError("deliberate mid-scope failure")
            except ValueError as propagated:
                exception_seen = str(propagated)
            observation["exception_propagated"] = exception_seen

            with kernel_output.as_device_tensor() as reread:
                observation["pixel_after_raise"] = (
                    torch.from_dlpack(reread)[3, 5].cpu().tolist()
                )

            # Usable on the next frame: the kernel writes it again and the
            # scope reads the fresh dispatch.
            fill_kernel.dispatch(
                bindings={"output_image": kernel_output},
                group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            )
            with kernel_output.as_device_tensor() as after_redispatch:
                observation["pixel_after_redispatch"] = (
                    torch.from_dlpack(after_redispatch)[3, 5].cpu().tolist()
                )
            return observation


@node.process
class PixelBufferScopeDiscardsOnRaiseProbe(DeviceExchangeFrameProbeBase):
    """One rule for both scopes: the CPU pixel-buffer scope discards a pending
    device write when the block is left by a raise.

    This deliberately changes what shipped: the handle used to publish however
    the block was left, and two scopes with two behaviours is not shippable.
    """

    def _probe(self, ctx, frame) -> dict:
        import torch

        gpu = ctx.gpu_limited_access
        with gpu.resolve_surface(frame.surface_id) as before_handle:
            before_handle.lock()
            pixel_before = numpy.from_dlpack(before_handle, device="cpu")[9, 11].tolist()
            before_handle.unlock()

        exception_seen = None
        try:
            with gpu.resolve_surface(frame.surface_id) as surface:
                surface.lock(read_only=False)
                if surface.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                    return {"device_unavailable": "device side not reachable"}
                tensor = torch.from_dlpack(surface)
                tensor[:, :, :] = 0
                # Not publish ordering — the discard needs the garbage
                # write to have LANDED in the staging.
                torch.accelerator.synchronize()
                raise ValueError("deliberate mid-scope failure")
        except ValueError as propagated:
            exception_seen = str(propagated)

        with gpu.resolve_surface(frame.surface_id) as reread:
            reread.lock()
            pixel_after = numpy.from_dlpack(reread, device="cpu")[9, 11].tolist()
            reread.unlock()
        return {
            "exception_propagated": exception_seen,
            "pixel_before": pixel_before,
            "pixel_after": pixel_after,
        }


@node.process(execution="manual")
class DeviceTensorScopeTakesEveryAcquiredTextureProbe:
    """No usage an author spells at `acquire_texture` can close the scope.

    `parse_texture_usages` implies `copy_src | copy_dst` on every request,
    so a Python author cannot mint the texture the copy guard refuses —
    spelling one token, or the two that used to be too few, all reach the
    same mask. The guard itself is untouched and still refuses a
    registration that genuinely lacks the usage (a foreign one); it is
    covered engine-side, against images built without the bits rather than
    through an acquire that can no longer produce them.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        observation = {}
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", OPAQUE_FD_FLAVOUR_USAGE
        ) as device_gate:
            if device_gate.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                return {"device_unavailable": "device side not reachable"}

        # On Linux bgra8 is not CUDA-mappable, so these acquires land on the
        # NotImportable allocation flavour, whose image carries exactly the
        # usage the request derived — which is the point: even there the
        # implied copy bits ride, so neither spelling is short of them.
        for entry, spelled in (
            ("scope_over_one_token", ["texture_binding"]),
            ("scope_over_copy_src_only", ["texture_binding", "copy_src"]),
        ):
            with ctx.gpu_full_access.acquire_texture(
                SURFACE_WIDTH, SURFACE_HEIGHT, "bgra8_unorm", spelled
            ) as acquired:
                try:
                    with acquired.as_device_tensor():
                        observation[entry] = "entered"
                except RuntimeError as refusal:
                    observation[entry] = f"refused: {refusal}"
        return observation


# Reads its source texel for texel, so the output is exactly what the engine's
# own GPU read of the source saw.
COPY_TEXEL_FOR_TEXEL_SLANG = """\
[[vk::binding(0, 0)]] Sampler2D source_image;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> output_image;
[numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint width;
    uint height;
    output_image.GetDimensions(width, height);
    if (invocation.x >= width || invocation.y >= height) { return; }
    output_image[invocation.xy] = source_image.Load(int3(invocation.xy, 0));
}
"""

DEVICE_WRITE_RGBA = [23, 67, 131, 255]


class _DeviceWriteThenEngineGpuReadProbe:
    """A framework writes a texture through the device-tensor scope with no
    synchronize of its own; the engine's next GPU read — a kernel dispatched
    right after the scope leaves — must see the write.

    For torch nothing but the scope's exit orders its queue ahead of the
    dispatch, so an exit that did not drain it reads the texture's old
    contents. For MLX the `mx.eval` its write contract puts in the scope is
    what orders it, so that variant proves the contract.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _write_the_whole_tensor_in_the_scope(self, device_tensor) -> None:
        raise NotImplementedError

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        usage = ["texture_binding", "storage_binding", "copy_src", "copy_dst"]
        copy_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=COPY_TEXEL_FOR_TEXEL_SLANG,
            bindings={"source_image": "sampled_texture", "output_image": "storage_image"},
        )
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", usage
        ) as source, ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", usage
        ) as output:
            if source.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                return {"device_unavailable": "device side not reachable"}
            with source.as_device_tensor() as device_tensor:
                self._write_the_whole_tensor_in_the_scope(device_tensor)
            copy_kernel.dispatch(
                bindings={"source_image": source, "output_image": output},
                group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            )
            output.lock()
            engine_read = output.as_numpy()
            observation = {
                "engine_read_pixel": engine_read[SURFACE_HEIGHT - 1, SURFACE_WIDTH - 1].tolist(),
                "every_pixel_the_engine_read_is_the_write": bool(
                    (engine_read == DEVICE_WRITE_RGBA).all()
                ),
            }
            output.unlock()
            return observation


@node.process(execution="manual")
class TorchDeviceWriteThenEngineGpuReadProbe(_DeviceWriteThenEngineGpuReadProbe):
    def _write_the_whole_tensor_in_the_scope(self, device_tensor) -> None:
        import torch

        tensor = torch.from_dlpack(device_tensor)
        tensor[:, :] = torch.tensor(DEVICE_WRITE_RGBA, dtype=torch.uint8, device=tensor.device)


def _mlx_or_none():
    try:
        import mlx.core  # pyright: ignore[reportMissingImports]

        return mlx.core
    except ImportError:
        return None


@node.process(execution="manual")
class MlxDeviceWriteThenEngineGpuReadProbe(_DeviceWriteThenEngineGpuReadProbe):
    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        if _mlx_or_none() is None:
            return {"mlx_unavailable": "mlx is not installed in this venv"}
        return super()._probe(ctx)

    def _write_the_whole_tensor_in_the_scope(self, device_tensor) -> None:
        import mlx.core as mx  # pyright: ignore[reportMissingImports]

        array = mx.from_dlpack(device_tensor)
        written = mx.array(DEVICE_WRITE_RGBA, dtype=mx.uint8)
        # Two partial slices, not `array[:] = ...`: MLX turns a whole-array
        # assignment into a new array rather than a store into this one.
        array[: SURFACE_HEIGHT // 2] = written
        array[SURFACE_HEIGHT // 2 :] = written
        # The MLX write contract: evaluated inside the scope. `mx.eval` blocks
        # until the stores have landed; nothing at the scope's exit can
        # evaluate a lazy graph the author never asked for.
        mx.eval(array)


@node.process
class MlxReadsTheFrameProbe(DeviceExchangeFrameProbeBase):
    """`mx.from_dlpack(frame)` over a graph frame is an MLX array over the
    frame's own bytes."""

    def _probe(self, ctx, frame) -> dict:
        mx = _mlx_or_none()
        if mx is None:
            return {"mlx_unavailable": "mlx is not installed in this venv"}
        array = mx.from_dlpack(frame)
        through_the_device = numpy.array(array)
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock()
            through_the_host = numpy.from_dlpack(surface, device="cpu").copy()
            surface.unlock()
        return {
            "array_shape": list(array.shape),
            "array_dtype": str(array.dtype),
            "same_pixels_as_the_host_view": bool((through_the_device == through_the_host).all()),
            "pixels_are_not_all_zero": bool(through_the_host.any()),
        }


MLX_EDIT_VALUE = 7
MLX_ROWS_TO_EDIT = 4


def _frame_pixels_now(ctx, surface_id: str):
    with ctx.gpu_limited_access.resolve_surface(surface_id) as surface:
        surface.lock()
        pixels = numpy.from_dlpack(surface, device="cpu").copy()
        surface.unlock()
    return pixels


@node.process
class MlxWritesTheFrameThroughTheWriteDoorProbe(DeviceExchangeFrameProbeBase):
    """`with frame.writable() as t:` with MLX as the package: an in-place,
    evaluated write reaches every other holder once the block ends."""

    def _probe(self, ctx, frame) -> dict:
        mx = _mlx_or_none()
        if mx is None:
            return {"mlx_unavailable": "mlx is not installed in this venv"}
        before = _frame_pixels_now(ctx, frame.surface_id)
        with frame.writable() as device_tensor:
            array = mx.from_dlpack(device_tensor)
            array[:MLX_ROWS_TO_EDIT] = MLX_EDIT_VALUE
            mx.eval(array)
        after = _frame_pixels_now(ctx, frame.surface_id)
        return {
            "the_frame_did_not_already_carry_the_edit": bool(
                (before[:MLX_ROWS_TO_EDIT] != MLX_EDIT_VALUE).any()
            ),
            "the_edited_rows_carry_the_edit": bool(
                (after[:MLX_ROWS_TO_EDIT] == MLX_EDIT_VALUE).all()
            ),
            "the_rest_of_the_frame_is_untouched": bool(
                (after[MLX_ROWS_TO_EDIT:] == before[MLX_ROWS_TO_EDIT:]).all()
            ),
        }


@node.process
class MlxWriteWithAViewAliveMissesTheFrameProbe(DeviceExchangeFrameProbeBase):
    """The negative control the MLX write contract rests on: with a view of the
    array alive at the write, MLX cannot donate the buffer, so the scatter lands
    in a buffer of MLX's own and the frame never sees it — whatever the scope
    does at its exit."""

    def _probe(self, ctx, frame) -> dict:
        mx = _mlx_or_none()
        if mx is None:
            return {"mlx_unavailable": "mlx is not installed in this venv"}
        before = _frame_pixels_now(ctx, frame.surface_id)
        with frame.writable() as device_tensor:
            array = mx.from_dlpack(device_tensor)
            view_kept_alive = array[1:3]
            array[:MLX_ROWS_TO_EDIT] = MLX_EDIT_VALUE
            mx.eval(array)
            the_array_carries_the_edit = bool(
                (numpy.array(array[:MLX_ROWS_TO_EDIT]) == MLX_EDIT_VALUE).all()
            )
            del view_kept_alive
        after = _frame_pixels_now(ctx, frame.surface_id)
        return {
            "the_array_carries_the_edit": the_array_carries_the_edit,
            "the_frame_is_unchanged": bool((after == before).all()),
        }


@node.process
class MlxWholeArrayAssignmentMissesTheFrameProbe(DeviceExchangeFrameProbeBase):
    """The partial-slice half of the MLX write contract: `a[:] = ...` is a new
    array to MLX, not a store into this one, so the frame never sees it even
    evaluated inside the scope."""

    def _probe(self, ctx, frame) -> dict:
        mx = _mlx_or_none()
        if mx is None:
            return {"mlx_unavailable": "mlx is not installed in this venv"}
        before = _frame_pixels_now(ctx, frame.surface_id)
        with frame.writable() as device_tensor:
            array = mx.from_dlpack(device_tensor)
            array[:] = MLX_EDIT_VALUE
            mx.eval(array)
            the_array_carries_the_edit = bool((numpy.array(array) == MLX_EDIT_VALUE).all())
        after = _frame_pixels_now(ctx, frame.surface_id)
        return {
            "the_array_carries_the_edit": the_array_carries_the_edit,
            "the_frame_is_unchanged": bool((after == before).all()),
        }


SCOPES_IN_ONE_HELPER = 12


@node.process(execution="manual")
class TorchAndMlxScopesAlternateInOneHelperProbe:
    """Many device-tensor scopes in one helper, torch and MLX alternating, each
    over a fresh texture whose arrays are dropped as soon as the scope ends.

    This is the shape that deadlocked when the scope's exit called
    `mx.synchronize()`: MLX's completion handler freeing a DLPack-imported
    array waits for the GIL that call holds. A regression hangs here, and the
    harness's wait for the result turns the hang into a failure.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import torch

        mx = _mlx_or_none()
        if mx is None:
            return {"mlx_unavailable": "mlx is not installed in this venv"}
        usage = ["texture_binding", "storage_binding", "copy_src", "copy_dst"]
        pixels_that_landed = []
        for scope_index in range(SCOPES_IN_ONE_HELPER):
            value = 10 + scope_index
            with ctx.gpu_full_access.acquire_texture(
                SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", usage
            ) as texture:
                if texture.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                    return {"device_unavailable": "device side not reachable"}
                with texture.as_device_tensor() as device_tensor:
                    if scope_index % 2 == 0:
                        tensor = torch.from_dlpack(device_tensor)
                        tensor[:4] = value
                        del tensor
                    else:
                        array = mx.from_dlpack(device_tensor)
                        array[:4] = value
                        mx.eval(array)
                        del array
                texture.lock()
                pixels_that_landed.append(texture.as_numpy()[1, 1].tolist())
                texture.unlock()
        return {
            "scopes_completed": len(pixels_that_landed),
            "every_write_landed": all(
                pixel == [10 + index] * 4 for index, pixel in enumerate(pixels_that_landed)
            ),
            "pixels_that_landed": pixels_that_landed,
        }


TEXTURE_FILL_RGBA = [5, 6, 7, 8]


class _DeviceArrayOutlivesTextureHandleProbe:
    """A device array over an acquired texture outlives the handle: closing
    the handle frees nothing the array addresses, and dropping the array
    afterwards releases the texture — from the capsule's deleter — without
    wedging the helper."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _import(self, handle):
        raise NotImplementedError

    def _checksum(self, array) -> int:
        raise NotImplementedError

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        usage = ["texture_binding", "storage_binding", "copy_src", "copy_dst"]
        texture = ctx.gpu_limited_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", usage
        )
        if texture.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
            texture.close()
            return {"device_unavailable": "device side not reachable"}
        texture.lock(read_only=False)
        texture.as_numpy()[:, :] = TEXTURE_FILL_RGBA
        texture.unlock()
        texture.lock()
        array = self._import(texture)
        checksum_before = self._checksum(array)
        texture.unlock()
        texture.close()
        del texture
        checksum_after = self._checksum(array)
        del array
        # The helper still answers after the deleter released the texture.
        with ctx.gpu_limited_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", usage
        ) as another_texture:
            another_texture_id = another_texture.surface_id
        return {
            "checksum_before": checksum_before,
            "checksum_after": checksum_after,
            "expected_checksum": SURFACE_WIDTH * SURFACE_HEIGHT * sum(TEXTURE_FILL_RGBA),
            "helper_still_answers": bool(another_texture_id),
        }


@node.process(execution="manual")
class TorchTensorOutlivesTextureHandleProbe(_DeviceArrayOutlivesTextureHandleProbe):
    def _import(self, handle):
        import torch

        return torch.from_dlpack(handle)

    def _checksum(self, array) -> int:
        import torch

        return int(array.to(torch.int64).sum().item())


@node.process(execution="manual")
class MlxArrayOutlivesTextureHandleProbe(_DeviceArrayOutlivesTextureHandleProbe):
    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        if _mlx_or_none() is None:
            return {"mlx_unavailable": "mlx is not installed in this venv"}
        return super()._probe(ctx)

    def _import(self, handle):
        import mlx.core as mx  # pyright: ignore[reportMissingImports]

        return mx.from_dlpack(handle)

    def _checksum(self, array) -> int:
        return int(numpy.array(array, dtype=numpy.int64).sum())
