# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes that exercise the device half of the pixel exchange from where it
really runs.

Each probe runs in its own helper process, reaches the frame's pixels as CUDA
memory there, and reports what it observed as one `MARKER:PROBE_RESULT` JSON
line — the same child→parent log forwarding every processor's records ride.

The device path crosses the process boundary twice per surface: the parent
allocates and publishes the export staging, the child imports it, and every
refill is a round trip whose answer is the timeline value to wait for. What is
worth breaking a build over is that the tensor really is device-resident, that
its pixels are the frame's pixels, and that an edit published from the child is
visible to a second, independent resolve.
"""

import json
import os
import traceback

import numpy

from streamlib import (
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    processor,
)

SURFACE_WIDTH = 64
SURFACE_HEIGHT = 32

RESULT_MARKER = "MARKER:PROBE_RESULT "

# DLPack device-type discriminants, part of the wire ABI.
DLPACK_DEVICE_CPU = 1
DLPACK_DEVICE_CUDA = 2


def _report(probe_body) -> None:
    """One result line per probe, success or failure — the failure carries the
    traceback so the test fails on the cause rather than a missing marker."""
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


class _FrameProbeBase:
    """Reads exactly one frame bag, then reports through `_report`."""

    @input(delivery_profile="every_sample")
    def video_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.frames_seen = 0

    def _probe(self, ctx: RuntimeContextLimitedAccess, frame: VideoFrame) -> dict:
        raise NotImplementedError

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None or self.frames_seen >= 1:
            return
        self.frames_seen += 1
        _report(lambda: self._probe(ctx, VideoFrame.from_bag(bag)))


@processor
class GraphFrameToTorchProbe(_FrameProbeBase):
    def _probe(self, ctx, frame) -> dict:
        import torch

        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock()
            reported_device = surface.__dlpack_device__()
            if reported_device[0] != DLPACK_DEVICE_CUDA:
                surface.unlock()
                return {"cuda_unavailable": f"__dlpack_device__ reported {reported_device}"}
            tensor = torch.from_dlpack(surface)
            host_view = numpy.from_dlpack(surface, device="cpu")
            observation = {
                "reported_device": list(reported_device),
                "tensor_device": str(tensor.device),
                "tensor_shape": list(tensor.shape),
                "tensor_dtype": str(tensor.dtype),
                # The tensor's pixels are the frame's pixels: compare a
                # sample against the host mapping of the same surface.
                "pixels_match_host": bool(
                    (tensor[3, 5].cpu().numpy() == host_view[3, 5]).all()
                ),
            }
            surface.unlock()
            return observation


@processor
class DeviceEditProbe(_FrameProbeBase):
    def _probe(self, ctx, frame) -> dict:
        import torch

        gpu = ctx.gpu_limited_access
        with gpu.resolve_surface(frame.surface_id) as surface:
            surface.lock(read_only=False)
            if surface.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
                surface.unlock()
                return {"cuda_unavailable": "device side not reachable"}
            tensor = torch.from_dlpack(surface)
            tensor[:, :, :] = 0
            tensor[9, 11] = torch.tensor(
                [17, 34, 51, 68], dtype=torch.uint8, device=tensor.device
            )
            # No torch.cuda.synchronize(): the publish itself orders the
            # consumer's stream before the engine's copy, and this probe
            # is part of what proves it. unlock is the publication point.
            surface.unlock()

        with gpu.resolve_surface(frame.surface_id) as reread:
            reread.lock()
            fresh_view = numpy.from_dlpack(reread, device="cpu")
            observation = {
                "pixel_after_publish": fresh_view[9, 11].tolist(),
                "cleared_pixel": fresh_view[0, 0].tolist(),
            }
            reread.unlock()
            return observation


@processor
class WithBlockEditProbe(_FrameProbeBase):
    def _probe(self, ctx, frame) -> dict:
        import torch

        gpu = ctx.gpu_limited_access
        # The idiomatic spelling: no explicit unlock — the with-block's
        # close is the publication point.
        with gpu.resolve_surface(frame.surface_id) as surface:
            surface.lock(read_only=False)
            if surface.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
                return {"cuda_unavailable": "device side not reachable"}
            tensor = torch.from_dlpack(surface)
            tensor[5, 5] = torch.tensor(
                [99, 88, 77, 66], dtype=torch.uint8, device=tensor.device
            )
            # No sync: close() publishes with engine-side stream ordering.

        with gpu.resolve_surface(frame.surface_id) as reread:
            reread.lock()
            observation = {
                "pixel_after_with_block": numpy.from_dlpack(reread, device="cpu")[
                    5, 5
                ].tolist()
            }
            reread.unlock()
            return observation


@processor
class TensorOutlivesHandleProbe(_FrameProbeBase):
    def _probe(self, ctx, frame) -> dict:
        import torch

        surface = ctx.gpu_limited_access.resolve_surface(frame.surface_id)
        surface.lock()
        if surface.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
            surface.unlock()
            return {"cuda_unavailable": "device side not reachable"}
        tensor = torch.from_dlpack(surface)
        checksum_before = int(tensor.to(torch.int64).sum().item())
        surface.unlock()
        surface.close()
        del surface

        # The handle is gone; the tensor must still address live memory —
        # its capsule holds the surface, the staging, and the CUDA import.
        torch.cuda.synchronize()
        return {
            "checksum_before": checksum_before,
            "checksum_after": int(tensor.to(torch.int64).sum().item()),
        }


@processor
class HostSideProbe(_FrameProbeBase):
    def _probe(self, ctx, frame) -> dict:
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock()
            host_view = numpy.from_dlpack(surface, device="cpu")
            via_as_numpy = surface.as_numpy()
            observation = {
                "host_shape": list(host_view.shape),
                "as_numpy_shape": list(via_as_numpy.shape),
                "same_pixels": bool((host_view[2, 2] == via_as_numpy[2, 2]).all()),
            }
            surface.unlock()
            return observation


@processor
class LaggedConsumerHoldsItsFrameProbe:
    """View identity across ring cycles, plus a frame held past the pool's own
    depth.

    Two claims, and the second is the one #1755 needs. Identity alone cannot
    fail for the reason the issue exists: two views of one recycled slot agree
    with each other by construction. Holding one frame while the producer runs
    well past the pool's depth is what proves the pixels under a surface id are
    still the ones the bag was published with.

    The frame is held through a view whose handle has already been closed,
    which is the stricter half of the same contract: `close()` drops only the
    handle's share of the surface, so a lease that rode `close()` rather than
    the last share would let the producer recycle the slot underneath a live
    array.

    The read is `into=VideoFrame` and that is load-bearing, not style: a claim
    is offered for the duration of a typed read and taken by the type being
    constructed. Reading the bag untyped and calling `VideoFrame.from_bag` on
    it afterwards takes no claim at all, so the held frame would ride pool
    depth like any other — and this probe would assert a lease it never took.

    Mental-revert: without the checkout lease the producer rehands the held
    slot within a ring cycle and `held_frame_unchanged` reads False.
    """

    @input(delivery_profile="every_sample")
    def video_from_upstream(self) -> None: ...

    # Comfortably past the pool's pre-allocated depth, so the producer has
    # cycled its ring several times over while the first frame is still held.
    FRAMES_TO_LAG_BY = 16

    def __init__(self) -> None:
        self.comparisons: "list[bool]" = []
        self.view_of_the_delivered_frame = None
        self.pixels_as_delivered = None
        self.frames_the_producer_ran_ahead = 0
        self.a_later_frame_differed = False
        self.frames_recycled_before_this_probe_read_them = 0
        self.reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None or self.reported:
            return
        try:
            self._observe(ctx, frame)
        except BaseException:  # noqa: BLE001 — surfaced through the marker line
            self.reported = True
            self.view_of_the_delivered_frame = None
            _report(lambda: {"failure": traceback.format_exc()})

    def _observe(self, ctx: RuntimeContextLimitedAccess, frame: VideoFrame) -> None:
        import torch

        if self.view_of_the_delivered_frame is None:
            surface = ctx.gpu_limited_access.resolve_surface(frame.surface_id)
            surface.lock()
            if surface.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
                surface.unlock()
                surface.close()
                self.reported = True
                _report(lambda: {"cuda_unavailable": "device side not reachable"})
                return
            view = numpy.from_dlpack(surface, device="cpu")
            # Copied, because this is the ground truth the view is compared
            # against — a second view would follow the memory under test.
            self.pixels_as_delivered = view.copy()
            self.view_of_the_delivered_frame = view
            # The handle goes now and the view stays: what keeps this frame
            # still from here on is the surface's last share, not the handle.
            surface.unlock()
            surface.close()
            del surface
            return

        # A later frame can recycle before this probe gets to it, and that is
        # the contract rather than a fault: publish-to-claim transit rides pool
        # depth, and this consumer is deliberately slow. Only the *held* frame
        # is protected, by its lease. Counting a recycled frame as a comparison
        # failure would fail the test for the engine behaving as designed.
        try:
            later_frame = ctx.gpu_limited_access.resolve_surface(frame.surface_id)
        except RuntimeError as recycled:
            if "recycled frame" not in str(recycled):
                raise
            self.frames_recycled_before_this_probe_read_them += 1
            self.frames_the_producer_ran_ahead += 1
            if self.frames_the_producer_ran_ahead < self.FRAMES_TO_LAG_BY:
                return
            self._report_the_held_frame()
            return

        with later_frame as surface:
            surface.lock()
            device_pixels = torch.from_dlpack(surface).cpu().numpy()
            host_pixels = numpy.from_dlpack(surface, device="cpu")
            self.comparisons.append(bool((device_pixels == host_pixels).all()))
            # A scene that never changes cannot prove anything about a frame
            # staying still, so the test skips unless something moved.
            if not (host_pixels == self.pixels_as_delivered).all():
                self.a_later_frame_differed = True
            surface.unlock()

        self.frames_the_producer_ran_ahead += 1
        if self.frames_the_producer_ran_ahead < self.FRAMES_TO_LAG_BY:
            return
        self._report_the_held_frame()

    def _report_the_held_frame(self) -> None:
        """The frame held since the first `process()` still reads as delivered.

        Read from `view_of_the_delivered_frame`, a mapping taken while the
        handle was open and still live because the lease — not the handle —
        is what keeps the slot.
        """
        held_frame_unchanged = bool(
            (self.view_of_the_delivered_frame == self.pixels_as_delivered).all()
        )
        self.reported = True
        observation = {
            "comparisons": self.comparisons,
            "frames_the_producer_ran_ahead": self.frames_the_producer_ran_ahead,
            "frames_recycled_before_this_probe_read_them": (
                self.frames_recycled_before_this_probe_read_them
            ),
            "held_frame_unchanged": held_frame_unchanged,
            "a_later_frame_differed": self.a_later_frame_differed,
        }
        self.view_of_the_delivered_frame = None
        _report(lambda: observation)


@processor(execution="manual")
class DmaBufExportProbe:
    """Round-trips a surface's DMA-BUF fd out of and back into the graph.

    Export answers from the fds the checkout delivered; import adopts a
    foreign fd as a fresh registration the graph can resolve. Both ends run
    in the child, and the pixels prove the adopted mapping is the same
    memory the export named.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as exported_surface:
            exported_surface.lock(read_only=False)
            exported_surface.as_numpy()[:, :, :] = 0
            exported_surface.as_numpy()[7, 9] = [21, 43, 65, 87]
            exported_surface.unlock()

            fd, byte_size = ctx.gpu_full_access.export_dma_buf(exported_surface)
            observation = {
                "fd_is_real": fd >= 0,
                "byte_size": byte_size,
                "expected_byte_size": SURFACE_WIDTH * SURFACE_HEIGHT * 4,
            }
            try:
                with ctx.gpu_full_access.import_dma_buf(
                    fd, SURFACE_WIDTH, SURFACE_HEIGHT, byte_size=byte_size
                ) as adopted_surface:
                    observation["adopted_surface_id"] = adopted_surface.surface_id
                    observation["exported_surface_id"] = exported_surface.surface_id
                    adopted_surface.lock()
                    observation["adopted_pixel"] = (
                        adopted_surface.as_numpy()[7, 9].tolist()
                    )
                    adopted_surface.unlock()
            finally:
                # The fd stays the caller's through the import; nothing else
                # will close it.
                observation["fd_closes_cleanly"] = os.close(fd) is None
            return observation


# GLSL that fills its one bound texture with a constant — the smallest
# engine-kernel producer a cross-process texture consumer can stand behind.
# The constant is chosen to be exact in unorm8: 64, 128, 192, 255.
FILL_CONSTANT_GLSL = """\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0, rgba8) uniform writeonly image2D output_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(output_image);
    if (at.x >= extent.x || at.y >= extent.y) { return; }
    imageStore(output_image, at, vec4(64.0 / 255.0, 128.0 / 255.0, 192.0 / 255.0, 1.0));
}
"""

FILL_CONSTANT_RGBA = [64, 128, 192, 255]

# The float fill, chosen exact in float16 and doubled exactly by the scope
# demo: (0.25, 0.5, 1.5, 2.0) -> (0.5, 1.0, 3.0, 4.0).
FILL_FLOAT_GLSL = """\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0, rgba16f) uniform writeonly image2D output_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(output_image);
    if (at.x >= extent.x || at.y >= extent.y) { return; }
    imageStore(output_image, at, vec4(0.25, 0.5, 1.5, 2.0));
}
"""

FILL_FLOAT_RGBA = [0.25, 0.5, 1.5, 2.0]
DOUBLED_FLOAT_RGBA = [0.5, 1.0, 3.0, 4.0]

# The usage sets that pick each cross-process-importable allocation flavour:
# the OPAQUE_FD constructor's fixed set, and a render-attachment set that
# takes the explicit-DRM-modifier DMA-BUF arm (storage included so the same
# kernel can write both flavours).
OPAQUE_FD_FLAVOUR_USAGE = ["texture_binding", "storage_binding", "copy_src", "copy_dst"]
RENDER_TARGET_FLAVOUR_USAGE = [
    "render_attachment",
    "storage_binding",
    "texture_binding",
    "copy_src",
]


@processor(execution="manual")
class TextureHandleRoundTripProbe:
    """A kernel output crosses the process boundary as the texture itself.

    Both handle flavours where the format allows: an OPAQUE_FD storage
    texture an engine kernel wrote, and an explicit-DRM-modifier DMA-BUF
    render target whose fd goes to native code. The resolve is the
    cross-process import — the child rebuilds the engine's tiled image on
    its own device, which is what a token-for-a-texture could never do.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        observation = {}
        # Raw handles mint only via the Full surface, on every path: the
        # per-frame capability offers neither spelling.
        observation["limited_surface_mints_no_raw_handle"] = not hasattr(
            ctx.gpu_limited_access, "export_opaque_fd"
        ) and not hasattr(ctx.gpu_limited_access, "export_dma_buf")
        fill_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FILL_CONSTANT_GLSL,
            bindings={"output_image": "storage_image"},
        )
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", OPAQUE_FD_FLAVOUR_USAGE
        ) as kernel_output:
            fill_kernel.dispatch(
                bindings={"output_image": kernel_output},
                group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            )
            observation["kernel_dispatched"] = True

            # An acquired-by-name texture holds no fd child-side; the raw
            # export refuses until the surface id is resolved.
            try:
                unresolved_export = ctx.gpu_full_access.export_opaque_fd(
                    kernel_output
                )
                observation["unresolved_export_refusal"] = (
                    f"no refusal: export answered {unresolved_export!r}"
                )
                os.close(unresolved_export.fd)
            except RuntimeError as refusal:
                observation["unresolved_export_refusal"] = str(refusal)

            with ctx.gpu_limited_access.resolve_surface(
                kernel_output.surface_id
            ) as resolved_texture:
                observation["opaque_resolved_extent"] = [
                    resolved_texture.width,
                    resolved_texture.height,
                ]
                observation["opaque_resolved_format"] = resolved_texture.format
                # The layout-correctness assertion: reading the kernel's
                # pixels through the cross-process device export only works
                # if the published layout chain — dispatch publish, checkout,
                # acquire barrier, staging refill — named the truth at every
                # step. A wrong layout reads garbage, not FILL_CONSTANT_RGBA.
                import torch

                resolved_texture.lock()
                device_view = torch.from_dlpack(resolved_texture)
                observation["opaque_device_pixel"] = (
                    device_view[11, 13].to("cpu").tolist()
                )
                del device_view
                resolved_texture.unlock()
                # Both outcomes recorded: a refusal that never arrives must
                # fail the assertion that names it, not raise a KeyError.
                try:
                    observation["opaque_pixel_refusal"] = (
                        f"no refusal: bytes_per_row answered "
                        f"{resolved_texture.bytes_per_row}"
                    )
                except RuntimeError as refusal:
                    observation["opaque_pixel_refusal"] = str(refusal)
                try:
                    exported = ctx.gpu_full_access.export_dma_buf(resolved_texture)
                    observation["opaque_export_refusal"] = (
                        f"no refusal: export answered {exported!r}"
                    )
                    os.close(exported[0])
                except RuntimeError as refusal:
                    observation["opaque_export_refusal"] = str(refusal)

                # The raw-handle door for the flavour: the fd plus the
                # allocation-stable shape a foreign import reproduces.
                export = ctx.gpu_full_access.export_opaque_fd(resolved_texture)
                observation["opaque_export_fd_is_real"] = export.fd >= 0
                observation["opaque_export_metadata"] = {
                    "allocation_byte_size": export.allocation_byte_size,
                    "width": export.width,
                    "height": export.height,
                    "format": export.format,
                    "vk_image_tiling": export.vk_image_tiling,
                    "vk_image_usage_flags": export.vk_image_usage_flags,
                    "vk_image_mip_levels": export.vk_image_mip_levels,
                    "vk_image_array_layers": export.vk_image_array_layers,
                    "vk_image_samples": export.vk_image_samples,
                    "dedicated_allocation": export.dedicated_allocation,
                    "vk_memory_type_index": export.vk_memory_type_index,
                    "exporting_device_uuid_hex": export.exporting_device_uuid.hex(),
                }
                observation["opaque_export_fd_closes_cleanly"] = (
                    os.close(export.fd) is None
                )

            # The frame is still usable after a consumer's release: the
            # release republished the layout and signalled its edge.
            with ctx.gpu_limited_access.resolve_surface(
                kernel_output.surface_id
            ) as resolved_again:
                observation["opaque_second_resolve_extent"] = [
                    resolved_again.width,
                    resolved_again.height,
                ]

        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", RENDER_TARGET_FLAVOUR_USAGE
        ) as render_target:
            # The demo shape: an engine kernel writes the texture, and the
            # texture handle itself — the fd native code imports — crosses
            # out, not a linear view of it.
            fill_kernel.dispatch(
                bindings={"output_image": render_target},
                group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            )
            with ctx.gpu_limited_access.resolve_surface(
                render_target.surface_id
            ) as resolved_render_target:
                fd, byte_size = ctx.gpu_full_access.export_dma_buf(
                    resolved_render_target
                )
                observation["rt_export_fd_is_real"] = fd >= 0
                observation["rt_export_byte_size"] = byte_size
                observation["rt_fd_closes_cleanly"] = os.close(fd) is None

                # The mirror of the redirect: a DMA-BUF-flavoured texture
                # refuses the OPAQUE_FD spelling, pointing back.
                try:
                    flavour_export = ctx.gpu_full_access.export_opaque_fd(
                        resolved_render_target
                    )
                    observation["dma_buf_flavour_export_refusal"] = (
                        f"no refusal: export answered {flavour_export!r}"
                    )
                    os.close(flavour_export.fd)
                except RuntimeError as refusal:
                    observation["dma_buf_flavour_export_refusal"] = str(refusal)

        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as pixel_buffer:
            try:
                pixel_export = ctx.gpu_full_access.export_opaque_fd(pixel_buffer)
                observation["pixel_buffer_export_refusal"] = (
                    f"no refusal: export answered {pixel_export!r}"
                )
                os.close(pixel_export.fd)
            except RuntimeError as refusal:
                observation["pixel_buffer_export_refusal"] = str(refusal)
        return observation


@processor(execution="manual")
class PrivilegedCapabilityProbe:
    """What the privileged capability answers from a helper process."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        observation = {}
        # The privileged acquire is a round trip to the parent, and what
        # comes back is a real surface in this child's address space.
        with ctx.gpu_full_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as privileged_surface:
            privileged_surface.lock(read_only=False)
            privileged_surface.as_numpy()[1, 2] = [1, 2, 3, 4]
            observation["privileged_acquire_shape"] = list(
                privileged_surface.as_numpy().shape
            )
            observation["privileged_surface_id"] = privileged_surface.surface_id
            privileged_surface.unlock()

        ctx.gpu_full_access.wait_device_idle()
        observation["waited_for_device_idle"] = True

        try:
            ctx.gpu_full_access.escalate(lambda privileged: None)
        except RuntimeError as refusal:
            observation["escalate_refusal"] = str(refusal)

        # A device texture acquires from a helper process: what comes back is
        # the surface id a kernel dispatch binds and a downstream processor
        # resolves — a name, deliberately not a local mapping. The `with`
        # returns its pool slot at a known point, like the acquire above.
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", ["copy_src"]
        ) as acquired_texture:
            observation["acquired_texture_surface_id"] = acquired_texture.surface_id
            observation["acquired_texture_extent"] = [
                acquired_texture.width,
                acquired_texture.height,
            ]
        return observation


@processor(execution="manual")
class DeviceTensorScopeDoublesAKernelOutputProbe:
    """The demo: torch doubles a kernel output in place through the scope.

    An rgba16_float output — the common HDR compute shape — reaches torch as
    a float16 tensor, `mul_(2.0)` edits it in place, and leaving the scope
    blits the edit back into the engine's texture. A second scope entry
    re-blits from the texture, so doubled values there prove the write-back
    reached the texture rather than lingering in the staging.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import torch

        fill_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FILL_FLOAT_GLSL,
            bindings={"output_image": "storage_image"},
        )
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba16_float", OPAQUE_FD_FLAVOUR_USAGE
        ) as kernel_output:
            fill_kernel.dispatch(
                bindings={"output_image": kernel_output},
                group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            )
            if kernel_output.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
                return {"cuda_unavailable": "device side not reachable"}
            observation: dict = {"surface_id": kernel_output.surface_id}
            # Deliberately no torch.cuda.synchronize(): the scope's exit
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


@processor(execution="manual")
class DeviceTensorScopeDiscardsOnRaiseProbe:
    """A raise mid-scope leaves the surface holding its pre-scope content.

    The write did not finish, so publishing it would hand downstream a torn
    frame; the scope discards instead, the exception propagates unsuppressed,
    and the surface — and the kernel that writes it — keep working afterwards.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import torch

        fill_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FILL_CONSTANT_GLSL,
            bindings={"output_image": "storage_image"},
        )
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", OPAQUE_FD_FLAVOUR_USAGE
        ) as kernel_output:
            fill_kernel.dispatch(
                bindings={"output_image": kernel_output},
                group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            )
            if kernel_output.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
                return {"cuda_unavailable": "device side not reachable"}
            observation = {}
            exception_seen = None
            try:
                with kernel_output.as_device_tensor() as tensor:
                    torch_view = torch.from_dlpack(tensor)
                    torch_view[:, :, :] = 0
                    # Not publish ordering — the discard needs the garbage
                    # write to have LANDED in the staging, or leaving it
                    # unpublished would prove nothing.
                    torch.cuda.synchronize()
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


@processor
class PixelBufferScopeDiscardsOnRaiseProbe(_FrameProbeBase):
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
                if surface.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
                    return {"cuda_unavailable": "device side not reachable"}
                tensor = torch.from_dlpack(surface)
                tensor[:, :, :] = 0
                # Not publish ordering — the discard needs the garbage
                # write to have LANDED in the staging.
                torch.cuda.synchronize()
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


@processor(execution="manual")
class PooledTextureExportProbe:
    """Resurrected from #1737 (removed by #1754, carried by #1757): a pooled
    texture acquired by a Python processor exports a device tensor of correct
    shape through the handle itself.

    The original also asserted a `pooled-texture-` id prefix (the escalate
    acquire now mints a UUID handle id), a read-only tensor (this ticket makes
    texture-backed exports writable), and a lease-bound full-access host-side
    arm (that capability shape no longer exists) — the live substance is the
    export itself.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import torch

        outcomes = {}
        with ctx.gpu_limited_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", OPAQUE_FD_FLAVOUR_USAGE
        ) as texture_handle:
            outcomes["texture_surface_id"] = texture_handle.surface_id
            device = texture_handle.__dlpack_device__()
            outcomes["texture_device"] = list(device)
            if device[0] == DLPACK_DEVICE_CUDA:
                texture_handle.lock()
                tensor = torch.from_dlpack(texture_handle)
                outcomes["texture_tensor_shape"] = list(tensor.shape)
                outcomes["texture_tensor_device"] = str(tensor.device)
                del tensor
                texture_handle.unlock()
        return outcomes


@processor(execution="manual")
class DeviceTensorScopeRefusesAnUnexportableUsageProbe:
    """A texture whose usage forbids a copy refuses at scope entry, by name.

    Recording the copy anyway would be a Vulkan spec violation the driver
    silently tolerates — the engine refuses instead: a sampled-only texture
    cannot blit out (copy_src), and a readable-but-not-writable one cannot
    take the blit back (copy_dst), so the write-in-place scope refuses both
    at `__enter__` rather than discarding edits or corrupting memory.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        observation = {}
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", OPAQUE_FD_FLAVOUR_USAGE
        ) as cuda_gate:
            if cuda_gate.__dlpack_device__()[0] != DLPACK_DEVICE_CUDA:
                return {"cuda_unavailable": "device side not reachable"}

        # bgra8 is not CUDA-mappable, so these acquires land on the
        # NotImportable allocation flavour, whose image carries exactly
        # the requested usage — the rgba8 spelling would take the
        # OPAQUE_FD constructor's fixed usage set and be legal to copy.
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "bgra8_unorm", ["texture_binding"]
        ) as sampled_only:
            try:
                with sampled_only.as_device_tensor():
                    observation["copy_src_refusal"] = "no refusal: the scope entered"
            except RuntimeError as refusal:
                observation["copy_src_refusal"] = str(refusal)

        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "bgra8_unorm", ["texture_binding", "copy_src"]
        ) as readable_only:
            try:
                with readable_only.as_device_tensor():
                    observation["copy_dst_refusal"] = "no refusal: the scope entered"
            except RuntimeError as refusal:
                observation["copy_dst_refusal"] = str(refusal)
        return observation

@processor(execution="manual")
class OpaqueFdExportHandoffProbe:
    """Hands a kernel-written texture's OPAQUE_FD export to a foreign process.

    The receiver — the Rust rig test driving this app — gets the fd over
    SCM_RIGHTS plus the export's metadata as JSON, imports on its own
    device with only that bundle, and byte-compares the kernel's pixels.
    This is the export contract consumed end-to-end: if the fd or any
    metadata field is wrong in a way an importer rejects, the foreign side
    fails, not this probe. The socket path arrives in
    STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import socket
        import struct

        socket_path = os.environ.get("STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET")
        if socket_path is None:
            return {"failure": "STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET is not set"}
        observation = {}
        fill_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FILL_CONSTANT_GLSL,
            bindings={"output_image": "storage_image"},
        )
        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", OPAQUE_FD_FLAVOUR_USAGE
        ) as kernel_output:
            fill_kernel.dispatch(
                bindings={"output_image": kernel_output},
                group_count=(SURFACE_WIDTH // 8, SURFACE_HEIGHT // 8, 1),
            )
            with ctx.gpu_limited_access.resolve_surface(
                kernel_output.surface_id
            ) as resolved_texture:
                export = ctx.gpu_full_access.export_opaque_fd(resolved_texture)
                metadata_wire = json.dumps(
                    {
                        "allocation_byte_size": export.allocation_byte_size,
                        "width": export.width,
                        "height": export.height,
                        "format": export.format,
                        "vk_image_tiling": export.vk_image_tiling,
                        "vk_image_usage_flags": export.vk_image_usage_flags,
                        "vk_image_mip_levels": export.vk_image_mip_levels,
                        "vk_image_array_layers": export.vk_image_array_layers,
                        "vk_image_samples": export.vk_image_samples,
                        "dedicated_allocation": export.dedicated_allocation,
                        "vk_memory_type_index": export.vk_memory_type_index,
                        "exporting_device_uuid_hex": export.exporting_device_uuid.hex(),
                    }
                ).encode()
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as handoff:
                    handoff.settimeout(60.0)
                    handoff.connect(socket_path)
                    socket.send_fds(
                        handoff,
                        [struct.pack("!I", len(metadata_wire)) + metadata_wire],
                        [export.fd],
                    )
                    # The dispatch above already retired, so the memory is
                    # defined for the foreign read; holding the resolve open
                    # until the verdict keeps the checkout lease over it.
                    observation["foreign_verdict"] = handoff.recv(64).decode()
                # The kernel dup'd the fd on the SCM_RIGHTS crossing; this
                # side's copy is still the caller's to close.
                observation["fd_closes_cleanly"] = os.close(export.fd) is None
        return observation

