# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot Vulkan compute processor — Python.

End-to-end gate for the subprocess `VulkanContext` runtime (#531). The
host pre-allocates a render-target-capable DMA-BUF surface AND an
exportable `VulkanTimelineSemaphore`, registers both with surface-share.
This processor receives a trigger Videoframe, opens the host surface
through ``VulkanContext.acquire_write`` (which imports the DMA-BUF as a
``VkImage`` in the subprocess and imports the timeline via
``from_imported_opaque_fd``), dispatches the Mandelbrot compute kernel
via the cdylib's quarantined ``slpn_vulkan_dispatch_compute`` helper,
and releases — the host adapter advances the timeline so the host's
pre-stop readback sees the writes.

No `pyvulkan` / `vulkan` Python binding required: the cdylib's
`dispatch_compute` accepts SPIR-V bytes + push-constant bytes + group
counts and runs the dispatch on the same `VkDevice` the adapter
manages. Real customers can use whatever Vulkan binding they prefer
through the ``raw_handles()`` escape hatch — the cdylib's runtime
exposes its raw `VkInstance` / `VkDevice` / `VkQueue` for that.

Config keys:
    vulkan_surface_uuid (str, required)
        Surface-share UUID the host registered the render-target image
        + timeline semaphore under.
    width (int, required)
        Surface width in pixels.
    height (int, required)
        Surface height in pixels.
    max_iter (int, required)
        Mandelbrot iteration count (matches the shader's
        ``pc.max_iter`` push-constant slot).
    variant (int, required)
        0 → Python palette (blue→green→red), 1 → Deno palette.
    shader_spv_hex (str, required)
        Hex-encoded SPIR-V bytecode for the Mandelbrot compute shader,
        compiled at example-build-time from
        ``examples/polyglot-vulkan-compute/shaders/mandelbrot.comp``.
"""

from __future__ import annotations

import struct
from typing import Optional

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.vulkan import VulkanContext


class VulkanComputeProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._uuid = str(cfg["vulkan_surface_uuid"])
        self._width = int(cfg["width"])
        self._height = int(cfg["height"])
        self._max_iter = int(cfg["max_iter"])
        self._variant = int(cfg["variant"])
        self._spv: bytes = bytes.fromhex(str(cfg["shader_spv_hex"]))
        self._vk = VulkanContext.from_runtime(ctx)
        self._dispatched = False
        self._error: Optional[str] = None
        print(
            f"[VulkanCompute/py] setup uuid={self._uuid} "
            f"size={self._width}x{self._height} "
            f"spv_bytes={len(self._spv)} variant={self._variant}",
            flush=True,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return
        if self._dispatched:
            return
        try:
            self._dispatch_once()
            self._dispatched = True
            print(
                f"[VulkanCompute/py] Mandelbrot dispatched into surface '{self._uuid}'",
                flush=True,
            )
        except Exception as e:
            self._error = str(e)
            print(
                f"[VulkanCompute/py] dispatch failed: {e}", flush=True,
            )

    def _dispatch_once(self) -> None:
        # Push constants: layout is `{u32 width, u32 height, u32 max_iter, u32 variant}`.
        push_consts = struct.pack(
            "<IIII",
            self._width,
            self._height,
            self._max_iter,
            self._variant,
        )
        # Group size matches the shader's `local_size_x = local_size_y = 16`.
        local = 16
        group_x = (self._width + local - 1) // local
        group_y = (self._height + local - 1) // local

        with self._vk.acquire_write(self._uuid) as view:
            print(
                f"[VulkanCompute/py] acquired vk_image=0x{view.vk_image:x} "
                f"layout={view.vk_image_layout}",
                flush=True,
            )
            self._vk.dispatch_compute(
                self._uuid,
                self._spv,
                push_consts,
                group_x,
                group_y,
                1,
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print(
            f"[VulkanCompute/py] teardown dispatched={self._dispatched} "
            f"error={self._error}",
            flush=True,
        )
