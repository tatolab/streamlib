# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probes that export a frame or a texture as a native handle — a DMA-BUF, an
opaque fd, a texture handle, an IOSurface — and read it back, or are refused."""

import json
import os
import sys

import numpy

from device_exchange_probe_shared_surfaces_and_reporting import (
    FILL_CONSTANT_SLANG,
    NATURAL_DLPACK_DEVICE,
    OPAQUE_FD_FLAVOUR_USAGE,
    PADDED_SURFACE_HEIGHT,
    PADDED_SURFACE_WIDTH,
    SURFACE_HEIGHT,
    SURFACE_WIDTH,
    report_device_exchange_probe_observation,
)
from tatolab.stream import (
    RuntimeContextFullAccess,
    node,
)


@node(execution="manual")
class DmaBufExportProbe:
    """Round-trips a surface's DMA-BUF fd out of and back into the graph.

    Export answers from the fds the checkout delivered; import adopts a
    foreign fd as a fresh registration the graph can resolve. Both ends run
    in the child, and the pixels prove the adopted mapping is the same
    memory the export named.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

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


# A render-attachment usage set that takes the explicit-DRM-modifier DMA-BUF
# arm (storage included so the same kernel can write both flavours).
RENDER_TARGET_FLAVOUR_USAGE = [
    "render_attachment",
    "storage_binding",
    "texture_binding",
    "copy_src",
]


@node(execution="manual")
class TextureHandleRoundTripProbe:
    """A kernel output crosses the process boundary as the texture itself.

    Both handle flavours where the format allows: an OPAQUE_FD storage
    texture an engine kernel wrote, and an explicit-DRM-modifier DMA-BUF
    render target whose fd goes to native code. The resolve is the
    cross-process import — the child rebuilds the engine's tiled image on
    its own device, which is what a token-for-a-texture could never do.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        observation = {}
        # Raw handles mint only via the Full surface, on every path: the
        # per-frame capability offers neither spelling.
        observation["limited_surface_mints_no_raw_handle"] = not hasattr(
            ctx.gpu_limited_access, "export_opaque_fd"
        ) and not hasattr(ctx.gpu_limited_access, "export_dma_buf")
        fill_kernel = ctx.gpu_limited_access.create_compute_kernel(
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


@node(execution="manual")
class PrivilegedCapabilityProbe:
    """What the privileged capability answers from a helper process."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

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

        with ctx.gpu_full_access.acquire_texture(
            SURFACE_WIDTH, SURFACE_HEIGHT, "rgba8_unorm", ["copy_src"]
        ) as acquired_texture:
            observation["acquired_texture_surface_id"] = acquired_texture.surface_id
            observation["acquired_texture_extent"] = [
                acquired_texture.width,
                acquired_texture.height,
            ]
        return observation


@node(execution="manual")
class PooledTextureExportProbe:

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

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


@node(execution="manual")
class OpaqueFdExportHandoffProbe:
    """Hands a kernel-written texture's OPAQUE_FD export to a foreign process.

    The receiver — the Rust rig test driving this stream — gets the fd over
    SCM_RIGHTS plus the export's metadata as JSON, imports on its own
    device with only that bundle, and byte-compares the kernel's pixels.
    This is the export contract consumed end-to-end: if the fd or any
    metadata field is wrong in a way an importer rejects, the foreign side
    fails, not this probe. The socket path arrives in
    STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import socket
        import struct

        socket_path = os.environ.get("STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET")
        if socket_path is None:
            return {"failure": "STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET is not set"}
        observation = {}
        fill_kernel = ctx.gpu_limited_access.create_compute_kernel(
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


@node(execution="manual")
class OpaqueFdPixelBufferExportHandoffProbe:
    """Hands a pooled pixel buffer's OPAQUE_FD export to a foreign process.

    On a device without DMA-BUF export a pooled frame is an OPAQUE_FD
    allocation, and `export_opaque_fd` hands its memory out with no image
    recipe; the receiver — the Rust rig test driving this stream — imports
    the fd at the stated memory type index on its own device and compares
    the bytes this probe wrote. On a DMA-BUF device the export refuses, and
    the refusal travels instead. The socket path arrives in
    STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        import socket
        import struct

        socket_path = os.environ.get("STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET")
        if socket_path is None:
            return {"failure": "STREAMLIB_TEST_OPAQUE_FD_HANDOFF_SOCKET is not set"}
        observation = {}
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as pixel_buffer:
            pixel_buffer.lock(read_only=False)
            pixels = pixel_buffer.as_numpy()
            # Every pixel differs, so a foreign read at the wrong offset
            # cannot match; the Rust receiver recomputes the same formula.
            rows, columns, channels = numpy.indices(pixels.shape)
            pixels[:] = (rows * 7 + columns * 3 + channels * 61 + 1) % 256
            bytes_per_row = pixels.strides[0]
            del pixels
            pixel_buffer.unlock()

            handed_fds: list[int] = []
            try:
                export = ctx.gpu_full_access.export_opaque_fd(pixel_buffer)
            except RuntimeError as refusal:
                metadata = {"export_refusal": str(refusal)}
            else:
                handed_fds.append(export.fd)
                metadata = {
                    "allocation_byte_size": export.allocation_byte_size,
                    "width": export.width,
                    "height": export.height,
                    "format": export.format,
                    "bytes_per_row": bytes_per_row,
                    "vk_image_tiling": export.vk_image_tiling,
                    "vk_image_usage_flags": export.vk_image_usage_flags,
                    "vk_image_mip_levels": export.vk_image_mip_levels,
                    "vk_image_array_layers": export.vk_image_array_layers,
                    "vk_image_samples": export.vk_image_samples,
                    "dedicated_allocation": export.dedicated_allocation,
                    "vk_memory_type_index": export.vk_memory_type_index,
                    "exporting_device_uuid_hex": export.exporting_device_uuid.hex(),
                }
            metadata_wire = json.dumps(metadata).encode()
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as handoff:
                handoff.settimeout(60.0)
                handoff.connect(socket_path)
                socket.send_fds(
                    handoff,
                    [struct.pack("!I", len(metadata_wire)) + metadata_wire],
                    handed_fds,
                )
                # Holding the pixel buffer until the verdict keeps its pool
                # slot from being rehanded under the foreign read.
                observation["foreign_verdict"] = handoff.recv(64).decode()
            # The kernel dup'd the fd on the SCM_RIGHTS crossing; this side's
            # copy is still the caller's to close.
            observation["fd_closes_cleanly"] = all(
                os.close(handed_fd) is None for handed_fd in handed_fds
            )
        return observation


class _NativeIOSurfaceShim:
    """The native side of an IOSurface raw handle, as a C consumer spells it:
    look the surface up from the port, lock it, read its rows — and the Mach
    calls a caller owning the send right makes."""

    KERN_SUCCESS = 0
    KERN_INVALID_NAME = 15
    MACH_PORT_RIGHT_SEND = 0
    IOSURFACE_LOCK_READ_ONLY = 1

    def __init__(self) -> None:
        import ctypes
        import ctypes.util

        self._ctypes = ctypes
        iosurface = ctypes.CDLL(ctypes.util.find_library("IOSurface"))
        core_foundation = ctypes.CDLL(ctypes.util.find_library("CoreFoundation"))
        self._system = ctypes.CDLL(ctypes.util.find_library("System"))
        iosurface.IOSurfaceLookupFromMachPort.restype = ctypes.c_void_p
        iosurface.IOSurfaceLookupFromMachPort.argtypes = [ctypes.c_uint32]
        iosurface.IOSurfaceLock.restype = ctypes.c_int32
        iosurface.IOSurfaceLock.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p]
        iosurface.IOSurfaceUnlock.restype = ctypes.c_int32
        iosurface.IOSurfaceUnlock.argtypes = [ctypes.c_void_p, ctypes.c_uint32, ctypes.c_void_p]
        iosurface.IOSurfaceGetBaseAddress.restype = ctypes.c_void_p
        iosurface.IOSurfaceGetBaseAddress.argtypes = [ctypes.c_void_p]
        for size_query in ("IOSurfaceGetBytesPerRow", "IOSurfaceGetWidth", "IOSurfaceGetHeight"):
            getattr(iosurface, size_query).restype = ctypes.c_size_t
            getattr(iosurface, size_query).argtypes = [ctypes.c_void_p]
        core_foundation.CFRelease.argtypes = [ctypes.c_void_p]
        self._system.mach_port_deallocate.restype = ctypes.c_int
        self._system.mach_port_deallocate.argtypes = [ctypes.c_uint32, ctypes.c_uint32]
        self._system.mach_port_get_refs.restype = ctypes.c_int
        self._system.mach_port_get_refs.argtypes = [
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_uint32),
        ]
        self._iosurface = iosurface
        self._core_foundation = core_foundation
        self._this_task = ctypes.c_uint32.in_dll(self._system, "mach_task_self_").value

    def read_rows_through_the_port(self, port: int, width: int, height: int) -> dict:
        """The surface the port names, looked up and read under a read-only
        lock: its geometry and its first `width` pixels of every row."""
        ctypes = self._ctypes
        surface = self._iosurface.IOSurfaceLookupFromMachPort(port)
        if not surface:
            return {"looked_up": False}
        try:
            assert self._iosurface.IOSurfaceLock(surface, self.IOSURFACE_LOCK_READ_ONLY, None) == 0
            try:
                bytes_per_row = self._iosurface.IOSurfaceGetBytesPerRow(surface)
                base_address = self._iosurface.IOSurfaceGetBaseAddress(surface)
                rows = numpy.ctypeslib.as_array(
                    ctypes.cast(base_address, ctypes.POINTER(ctypes.c_uint8)),
                    shape=(height * bytes_per_row,),
                ).reshape(height, bytes_per_row)[:, : width * 4].reshape(height, width, 4).copy()
            finally:
                self._iosurface.IOSurfaceUnlock(surface, self.IOSURFACE_LOCK_READ_ONLY, None)
            return {
                "looked_up": True,
                "width": self._iosurface.IOSurfaceGetWidth(surface),
                "height": self._iosurface.IOSurfaceGetHeight(surface),
                "bytes_per_row": bytes_per_row,
                "rows": rows,
            }
        finally:
            self._core_foundation.CFRelease(surface)

    def send_right_references(self, port: int) -> int:
        """How many send-right references this task holds under `port` — zero
        once the name is gone."""
        references = self._ctypes.c_uint32(0)
        kern_return = self._system.mach_port_get_refs(
            self._this_task, port, self.MACH_PORT_RIGHT_SEND, self._ctypes.byref(references)
        )
        if kern_return == self.KERN_INVALID_NAME:
            return 0
        assert kern_return == self.KERN_SUCCESS, kern_return
        return references.value

    def deallocate(self, port: int) -> int:
        return self._system.mach_port_deallocate(self._this_task, port)


def _a_pattern_no_two_pixels_share(height: int, width: int) -> numpy.ndarray:
    rows, columns = numpy.indices((height, width))
    pattern = numpy.empty((height, width, 4), dtype=numpy.uint8)
    pattern[:, :, 0] = rows * 7
    pattern[:, :, 1] = columns
    pattern[:, :, 2] = columns >> 8
    pattern[:, :, 3] = 255
    return pattern


def _export_iosurface_and_read_it_natively(ctx, shim, surface) -> dict:
    """Store a pattern through the CPU door, export the surface's IOSurface,
    and read it back through the port as native code would; then show each
    export is a fresh send right the caller owns and gives back."""
    height, width = surface.height, surface.width
    pattern = _a_pattern_no_two_pixels_share(height, width)
    surface.lock(read_only=False)
    surface.as_numpy()[:, :, :] = pattern
    surface.unlock()

    first_export = ctx.gpu_full_access.export_iosurface(surface)
    second_export = ctx.gpu_full_access.export_iosurface(surface)
    native_read = shim.read_rows_through_the_port(first_export.port, width, height)
    second_native_read = shim.read_rows_through_the_port(second_export.port, width, height)
    ports = [first_export.port, second_export.port]
    references_while_held = [shim.send_right_references(port) for port in ports]
    deallocations = [shim.deallocate(port) for port in ports]
    references_after_given_back = [shim.send_right_references(port) for port in ports]

    rows = native_read.pop("rows", None)
    second_rows = second_native_read.get("rows")
    return {
        **native_read,
        "pixels_match": rows is not None and bool(numpy.array_equal(rows, pattern)),
        "second_port_reads_the_same_pixels": second_rows is not None
        and bool(numpy.array_equal(second_rows, pattern)),
        "ports_are_real": all(port != 0 for port in ports),
        "each_export_is_its_own_right": ports[0] != ports[1],
        "references_while_held": references_while_held,
        "deallocations": deallocations,
        "references_after_given_back": references_after_given_back,
        "export": {
            "width": first_export.width,
            "height": first_export.height,
            "format": first_export.format,
            "bytes_per_row": first_export.bytes_per_row,
            "allocation_byte_size": first_export.allocation_byte_size,
            "vk_image_tiling": first_export.vk_image_tiling,
            "vk_image_usage_flags": first_export.vk_image_usage_flags,
            "vk_image_mip_levels": first_export.vk_image_mip_levels,
            "vk_image_array_layers": first_export.vk_image_array_layers,
            "vk_image_samples": first_export.vk_image_samples,
        },
    }


@node(execution="manual")
class IOSurfaceExportProbe:
    """A pixel buffer and a texture whose rows pad, each exported as an
    IOSurface port and read back by native code in this same helper."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        shim = _NativeIOSurfaceShim()
        observation = {}
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as pixel_buffer:
            observation["pixel_buffer"] = _export_iosurface_and_read_it_natively(
                ctx, shim, pixel_buffer
            )
        with ctx.gpu_full_access.acquire_texture(
            PADDED_SURFACE_WIDTH,
            PADDED_SURFACE_HEIGHT,
            "rgba8_unorm",
            RENDER_TARGET_FLAVOUR_USAGE,
        ) as texture:
            observation["texture"] = _export_iosurface_and_read_it_natively(ctx, shim, texture)
        return observation


def _refusal_of(export_call) -> str | None:
    try:
        export_call()
    except RuntimeError as refusal:
        return str(refusal)
    return None


@node(execution="manual")
class RawHandleOffItsPlatformRefusesProbe:
    """Every raw-handle flavour exists on both floors; off its own it refuses,
    naming its peer."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        report_device_exchange_probe_observation(lambda: self._probe(ctx))

    def _probe(self, ctx: RuntimeContextFullAccess) -> dict:
        with ctx.gpu_limited_access.acquire_pixel_buffer(
            SURFACE_WIDTH, SURFACE_HEIGHT
        ) as pixel_buffer:
            if sys.platform == "darwin":
                return {
                    "export_dma_buf": _refusal_of(
                        lambda: ctx.gpu_full_access.export_dma_buf(pixel_buffer)
                    ),
                    "export_opaque_fd": _refusal_of(
                        lambda: ctx.gpu_full_access.export_opaque_fd(pixel_buffer)
                    ),
                }
            return {
                "export_iosurface": _refusal_of(
                    lambda: ctx.gpu_full_access.export_iosurface(pixel_buffer)
                ),
            }
