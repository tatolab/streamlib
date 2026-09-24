# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes that exercise the device half of the pixel exchange from where it
really runs.

Each probe runs in its own helper process, reaches the frame's pixels as device
memory there — CUDA on Linux, Metal on macOS — and reports what it observed as
one `MARKER:PROBE_RESULT` JSON line, the same child→parent log forwarding every
processor's records ride.

On Linux the device path crosses the process boundary twice per surface: the
parent allocates and publishes the export staging, the child imports it, and
every refill is a round trip whose answer is the timeline value to wait for. On
macOS the device view is a no-copy Metal buffer over the frame's own IOSurface.
What is worth breaking a build over on both is that the tensor really is
device-resident, that its pixels are the frame's pixels, and that an edit
published from the child is visible to a second, independent resolve.
"""

import json
import os
import sys
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
DLPACK_DEVICE_METAL = 8

# The device a frame's natural DLPack side lives on, and torch's name for it.
NATURAL_DLPACK_DEVICE = DLPACK_DEVICE_METAL if sys.platform == "darwin" else DLPACK_DEVICE_CUDA
NATURAL_TORCH_DEVICE_TYPE = "mps" if sys.platform == "darwin" else "cuda"


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

    @input(delivery_profile="ordered")
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
            if reported_device[0] != NATURAL_DLPACK_DEVICE:
                surface.unlock()
                return {"device_unavailable": f"__dlpack_device__ reported {reported_device}"}
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
            if surface.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                surface.unlock()
                return {"device_unavailable": "device side not reachable"}
            tensor = torch.from_dlpack(surface)
            tensor[:, :, :] = 0
            tensor[9, 11] = torch.tensor(
                [17, 34, 51, 68], dtype=torch.uint8, device=tensor.device
            )
            # No torch.accelerator.synchronize(): the publish itself orders the
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
            if surface.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                return {"device_unavailable": "device side not reachable"}
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
        if surface.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
            surface.unlock()
            return {"device_unavailable": "device side not reachable"}
        tensor = torch.from_dlpack(surface)
        checksum_before = int(tensor.to(torch.int64).sum().item())
        surface.unlock()
        surface.close()
        del surface

        # The handle is gone; the tensor must still address live memory —
        # its capsule holds the surface, the staging, and the CUDA import.
        torch.accelerator.synchronize()
        return {
            "checksum_before": checksum_before,
            "checksum_after": int(tensor.to(torch.int64).sum().item()),
        }


@processor
class HostSideProbe(_FrameProbeBase):
    def _probe(self, ctx, frame) -> dict:
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock()
            natural_device = surface.__dlpack_device__()
            host_view = numpy.from_dlpack(surface, device="cpu")
            via_as_numpy = surface.as_numpy()
            observation = {
                "natural_device": list(natural_device),
                "host_shape": list(host_view.shape),
                "as_numpy_shape": list(via_as_numpy.shape),
                "same_pixels": bool((host_view == via_as_numpy).all()),
                # One mapping, not two copies that happen to agree.
                "same_host_memory": host_view.ctypes.data == via_as_numpy.ctypes.data,
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

    @input(delivery_profile="ordered")
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
            if surface.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                surface.unlock()
                surface.close()
                self.reported = True
                _report(lambda: {"device_unavailable": "device side not reachable"})
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
                # The same tiled texture, read on the CPU: the staged door
                # routes it over the host-visible export staging, so the
                # device tensor above and this array must agree about the
                # kernel's pixels.
                resolved_texture.lock(read_only=True)
                observation["opaque_cpu_pixel"] = (
                    resolved_texture.as_numpy()[11, 13].tolist()
                )
                observation["opaque_cpu_bytes_per_row"] = (
                    resolved_texture.bytes_per_row
                )
                resolved_texture.unlock()
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
            if device[0] == NATURAL_DLPACK_DEVICE:
                texture_handle.lock()
                tensor = torch.from_dlpack(texture_handle)
                outcomes["texture_tensor_shape"] = list(tensor.shape)
                outcomes["texture_tensor_device"] = str(tensor.device)
                del tensor
                texture_handle.unlock()
        return outcomes


@processor(execution="manual")
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
        _report(lambda: self._probe(ctx))

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



# ---------------------------------------------------------------------------
# Every door, both floors: the host request, `copy=True`, the row pitch, and
# the ordering of a device write ahead of the engine's own GPU read
# ---------------------------------------------------------------------------


@processor
class CopyRequestRefusedAtBothDoorsProbe(_FrameProbeBase):
    """`copy=True` asks for memory the consumer owns; both doors export in
    place, so both refuse by name rather than hand back an alias."""

    def _probe(self, ctx, frame) -> dict:
        observation = {}
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as surface:
            surface.lock()
            try:
                surface.__dlpack__(copy=True)
                observation["handle_refusal"] = "no refusal"
            except BufferError as refusal:
                observation["handle_refusal"] = str(refusal)
            surface.unlock()
            # The scope's refusal needs the scope entered, which needs the
            # device side; the handle's refusal above needs neither.
            if surface.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                observation["scope_device_unavailable"] = "device side not reachable"
                return observation
            with surface.as_device_tensor() as device_tensor:
                try:
                    device_tensor.__dlpack__(copy=True)
                    observation["scope_refusal"] = "no refusal"
                except BufferError as refusal:
                    observation["scope_refusal"] = str(refusal)
        return observation


# Wide enough that a GPU image's IOSurface pads its rows past `width * 4`;
# a pool pixel buffer's rows are packed, so it is the unpadded control.
PADDED_SURFACE_WIDTH = 1000
PADDED_SURFACE_HEIGHT = 8
ROW_END_PIXEL_RGBA = [11, 22, 33, 44]


def _store_at_the_end_of_a_row_and_read_it_back(tensor_scope_or_handle, host_reader) -> dict:
    """Store one pixel at the last column of a row through torch, then read
    that row's end and the next row's start through the host."""
    import torch

    last_row = PADDED_SURFACE_HEIGHT - 2
    observation = {}
    with tensor_scope_or_handle as device_view:
        tensor = torch.from_dlpack(device_view)
        observation["tensor_strides"] = list(tensor.stride())
        tensor[last_row, PADDED_SURFACE_WIDTH - 1] = torch.tensor(
            ROW_END_PIXEL_RGBA, dtype=torch.uint8, device=tensor.device
        )
        del tensor
    host_view, bytes_per_row = host_reader()
    observation["bytes_per_row"] = bytes_per_row
    observation["row_end_through_the_host"] = host_view[
        last_row, PADDED_SURFACE_WIDTH - 1
    ].tolist()
    observation["next_row_start_through_the_host"] = host_view[last_row + 1, 0].tolist()
    return observation


@processor(execution="manual")
class DeviceTensorStridesFollowTheRowPitchProbe:
    """Both backings at a width whose GPU-image rows pad: the device tensor's
    row stride is the surface's own pitch, so a store at the last pixel of a
    row lands where the host view finds it rather than shearing into the next
    row."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        observation = {}
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            PADDED_SURFACE_WIDTH, PADDED_SURFACE_HEIGHT
        ) as pixel_buffer:
            if pixel_buffer.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                return {"device_unavailable": "device side not reachable"}
            pixel_buffer.lock(read_only=False)
            pixel_buffer.as_numpy()[:, :, :] = 0
            pixel_buffer.unlock()

            def pixel_buffer_host_view():
                pixel_buffer.lock()
                host_view = pixel_buffer.as_numpy().copy()
                pitch = pixel_buffer.bytes_per_row
                pixel_buffer.unlock()
                return host_view, pitch

            observation["pixel_buffer"] = _store_at_the_end_of_a_row_and_read_it_back(
                pixel_buffer.as_device_tensor(), pixel_buffer_host_view
            )

        with ctx.gpu_full_access.acquire_texture(
            PADDED_SURFACE_WIDTH,
            PADDED_SURFACE_HEIGHT,
            "rgba8_unorm",
            ["texture_binding", "storage_binding", "copy_src", "copy_dst"],
        ) as texture:
            texture.lock(read_only=False)
            texture.as_numpy()[:, :, :] = 0
            texture.unlock()

            def texture_host_view():
                texture.lock()
                host_view = texture.as_numpy().copy()
                pitch = texture.bytes_per_row
                texture.unlock()
                return host_view, pitch

            observation["texture"] = _store_at_the_end_of_a_row_and_read_it_back(
                texture.as_device_tensor(), texture_host_view
            )
        return observation


# Reads its source texel for texel, so the output is exactly what the engine's
# own GPU read of the source saw.
COPY_TEXEL_FOR_TEXEL_GLSL = """\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D output_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(output_image);
    if (at.x >= extent.x || at.y >= extent.y) { return; }
    imageStore(output_image, at, texelFetch(source_image, at, 0));
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
        _report(lambda: self._probe(ctx))

    def _write_the_whole_tensor_in_the_scope(self, device_tensor) -> None:
        raise NotImplementedError

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        usage = ["texture_binding", "storage_binding", "copy_src", "copy_dst"]
        copy_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=COPY_TEXEL_FOR_TEXEL_GLSL,
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


@processor(execution="manual")
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


@processor(execution="manual")
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


class _TypedFrameProbeBase(_FrameProbeBase):
    """Reads its one frame `into=VideoFrame`, the read that takes the claim the
    frame object's own doors — the bare capsule, `writable()` — ride."""

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.frames_seen >= 1:
            return
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None:
            return
        self.frames_seen += 1
        _report(lambda: self._probe(ctx, frame))


@processor
class MlxReadsTheFrameProbe(_TypedFrameProbeBase):
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


@processor
class MlxWritesTheFrameThroughTheWriteDoorProbe(_TypedFrameProbeBase):
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


@processor
class MlxWriteWithAViewAliveMissesTheFrameProbe(_TypedFrameProbeBase):
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



@processor
class MlxWholeArrayAssignmentMissesTheFrameProbe(_TypedFrameProbeBase):
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


@processor(execution="manual")
class TorchAndMlxScopesAlternateInOneHelperProbe:
    """Many device-tensor scopes in one helper, torch and MLX alternating, each
    over a fresh texture whose arrays are dropped as soon as the scope ends.

    This is the shape that deadlocked when the scope's exit called
    `mx.synchronize()`: MLX's completion handler freeing a DLPack-imported
    array waits for the GIL that call holds. A regression hangs here, and the
    harness's wait for the result turns the hang into a failure.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        _report(lambda: self._probe(ctx))

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
        _report(lambda: self._probe(ctx))

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


@processor(execution="manual")
class TorchTensorOutlivesTextureHandleProbe(_DeviceArrayOutlivesTextureHandleProbe):
    def _import(self, handle):
        import torch

        return torch.from_dlpack(handle)

    def _checksum(self, array) -> int:
        import torch

        return int(array.to(torch.int64).sum().item())


@processor(execution="manual")
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
