# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probes that reach a frame's pixels as a device tensor in their own helper
process: an edit, a lagged reader, the copy door refused, and the row pitch."""

import traceback

import numpy

from device_exchange_probe_shared_surfaces_and_reporting import (
    NATURAL_DLPACK_DEVICE,
    PADDED_SURFACE_HEIGHT,
    PADDED_SURFACE_WIDTH,
    DeviceExchangeFrameProbeBase,
    FramesInOrder,
    report_device_exchange_probe_observation,
)
from tatolab.stream import (
    ImageFrame,
    InputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    node,
)


@node
class GraphFrameToTorchProbe(DeviceExchangeFrameProbeBase):
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


@node
class DeviceEditProbe(DeviceExchangeFrameProbeBase):
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


@node
class WithBlockEditProbe(DeviceExchangeFrameProbeBase):
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


@node
class TensorOutlivesHandleProbe(DeviceExchangeFrameProbeBase):
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


@node
class HostSideProbe(DeviceExchangeFrameProbeBase):
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


@node
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

    The read goes through the Texture pin and that is load-bearing,
    not style: the claim is taken as the read builds its `ImageFrame`. A read
    by name returns the payload bytes and takes no claim at all, so the held
    frame would ride pool depth like any other — and this probe would assert a
    lease it never took.

    Mental-revert: without the checkout lease the producer rehands the held
    slot within a ring cycle and `held_frame_unchanged` reads False.
    """

    video_from_upstream: InputPin[FramesInOrder]

    # Comfortably past the pool's pre-allocated depth, so the producer has
    # cycled its ring several times over while the first frame is still held.
    FRAMES_TO_LAG_BY = 16

    def __init__(self) -> None:
        self._comparisons: "list[bool]" = []
        self._view_of_the_delivered_frame = None
        self._pixels_as_delivered = None
        self._frames_the_producer_ran_ahead = 0
        self._a_later_frame_differed = False
        self._frames_recycled_before_this_probe_read_them = 0
        self._reported = False

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = self.video_from_upstream.read()
        if frame is None or self._reported:
            return
        try:
            self._observe(ctx, frame)
        except BaseException:  # noqa: BLE001 — surfaced through the marker line
            self._reported = True
            self._view_of_the_delivered_frame = None
            report_device_exchange_probe_observation(lambda: {"failure": traceback.format_exc()})

    def _observe(self, ctx: RuntimeContextLimitedAccess, frame: ImageFrame) -> None:
        import torch

        if self._view_of_the_delivered_frame is None:
            surface = ctx.gpu_limited_access.resolve_surface(frame.surface_id)
            surface.lock()
            if surface.__dlpack_device__()[0] != NATURAL_DLPACK_DEVICE:
                surface.unlock()
                surface.close()
                self._reported = True
                report_device_exchange_probe_observation(lambda: {"device_unavailable": "device side not reachable"})
                return
            view = numpy.from_dlpack(surface, device="cpu")
            # Copied, because this is the ground truth the view is compared
            # against — a second view would follow the memory under test.
            self._pixels_as_delivered = view.copy()
            self._view_of_the_delivered_frame = view
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
            self._frames_recycled_before_this_probe_read_them += 1
            self._frames_the_producer_ran_ahead += 1
            if self._frames_the_producer_ran_ahead < self.FRAMES_TO_LAG_BY:
                return
            self._report_the_held_frame()
            return

        with later_frame as surface:
            surface.lock()
            device_pixels = torch.from_dlpack(surface).cpu().numpy()
            host_pixels = numpy.from_dlpack(surface, device="cpu")
            self._comparisons.append(bool((device_pixels == host_pixels).all()))
            # A scene that never changes cannot prove anything about a frame
            # staying still, so the test skips unless something moved.
            if not (host_pixels == self._pixels_as_delivered).all():
                self._a_later_frame_differed = True
            surface.unlock()

        self._frames_the_producer_ran_ahead += 1
        if self._frames_the_producer_ran_ahead < self.FRAMES_TO_LAG_BY:
            return
        self._report_the_held_frame()

    def _report_the_held_frame(self) -> None:
        """The frame held since the first `process()` still reads as delivered.

        Read from `view_of_the_delivered_frame`, a mapping taken while the
        handle was open and still live because the lease — not the handle —
        is what keeps the slot.
        """
        held_frame_unchanged = bool(
            (self._view_of_the_delivered_frame == self._pixels_as_delivered).all()
        )
        self._reported = True
        observation = {
            "comparisons": self._comparisons,
            "frames_the_producer_ran_ahead": self._frames_the_producer_ran_ahead,
            "frames_recycled_before_this_probe_read_them": (
                self._frames_recycled_before_this_probe_read_them
            ),
            "held_frame_unchanged": held_frame_unchanged,
            "a_later_frame_differed": self._a_later_frame_differed,
        }
        self._view_of_the_delivered_frame = None
        report_device_exchange_probe_observation(lambda: observation)


@node
class CopyRequestRefusedAtBothDoorsProbe(DeviceExchangeFrameProbeBase):
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


@node(execution="manual")
class DeviceTensorStridesFollowTheRowPitchProbe:
    """Both backings at a width whose GPU-image rows pad: the device tensor's
    row stride is the surface's own pitch, so a store at the last pixel of a
    row lands where the host view finds it rather than shearing into the next
    row."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

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
