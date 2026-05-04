# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot Vulkan graphics processor — Python.

End-to-end gate for the subprocess `VulkanContext.dispatch_graphics`
runtime (#656). The host pre-allocates a render-target-capable
DMA-BUF surface AND an exportable `VulkanTimelineSemaphore`,
registers both with surface-share, and installs a
``GraphicsKernelBridge`` wired to its ``VulkanGraphicsKernel``. This
processor receives a trigger Videoframe, opens the host surface
through ``VulkanContext.acquire_write``, and calls
``VulkanContext.dispatch_graphics`` — which routes through escalate
IPC's ``register_graphics_kernel`` + ``run_graphics_draw`` ops to
the host's ``VulkanGraphicsKernel.offscreen_render``. The triangle
is fabricated by the vertex shader from ``gl_VertexIndex`` (no
vertex buffer required), with a per-vertex color picked by the
``variant`` push constant.

Config keys:
    vulkan_surface_uuid (str, required)
        Surface-share UUID the host registered the render-target image
        + timeline semaphore under.
    width (int, required)
        Surface width in pixels.
    height (int, required)
        Surface height in pixels.
    variant (int, required)
        0 → Python palette (R/G/B), 1 → Deno palette (cyan/magenta/yellow).
    vertex_spv_hex (str, required)
        Hex-encoded SPIR-V bytecode for the triangle vertex shader.
    fragment_spv_hex (str, required)
        Hex-encoded SPIR-V bytecode for the triangle fragment shader.
"""

from __future__ import annotations

import struct
from typing import Optional

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.vulkan import VkImageLayout, VulkanContext


def _triangle_pipeline_state() -> dict:
    """The pipeline state the host's `VulkanGraphicsKernel` is built
    with. Single Rgba8Unorm color attachment, no depth, no blend, no
    vertex inputs (gl_VertexIndex pattern), TriangleList topology,
    dynamic viewport+scissor.
    """
    return {
        "topology": "triangle_list",
        "vertex_input_bindings": [],
        "vertex_input_attributes": [],
        "rasterization_polygon_mode": "fill",
        "rasterization_cull_mode": "none",
        "rasterization_front_face": "counter_clockwise",
        "rasterization_line_width": 1.0,
        "multisample_samples": 1,
        "depth_stencil_enabled": False,
        "depth_compare_op": "always",
        "depth_write": False,
        "color_blend_enabled": False,
        "color_write_mask": 0b1111,
        "color_blend_src_color_factor": "one",
        "color_blend_dst_color_factor": "zero",
        "color_blend_color_op": "add",
        "color_blend_src_alpha_factor": "one",
        "color_blend_dst_alpha_factor": "zero",
        "color_blend_alpha_op": "add",
        "attachment_color_formats": ["rgba8_unorm"],
        "dynamic_state": "viewport_scissor",
    }


# Vertex stage visibility for push constants — bit 0 in the
# `GraphicsShaderStageFlags` newtype.
_STAGE_VERTEX = 1


class VulkanGraphicsProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._uuid = str(cfg["vulkan_surface_uuid"])
        self._width = int(cfg["width"])
        self._height = int(cfg["height"])
        self._variant = int(cfg["variant"])
        self._vert: bytes = bytes.fromhex(str(cfg["vertex_spv_hex"]))
        self._frag: bytes = bytes.fromhex(str(cfg["fragment_spv_hex"]))
        self._vk = VulkanContext.from_runtime(ctx)
        self._dispatched = False
        self._error: Optional[str] = None
        print(
            f"[VulkanGraphics/py] setup uuid={self._uuid} "
            f"size={self._width}x{self._height} "
            f"vert_bytes={len(self._vert)} frag_bytes={len(self._frag)} "
            f"variant={self._variant}",
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
                f"[VulkanGraphics/py] triangle drawn into surface '{self._uuid}'",
                flush=True,
            )
        except Exception as e:
            self._error = str(e)
            print(
                f"[VulkanGraphics/py] dispatch failed: {e}", flush=True,
            )

    def _dispatch_once(self) -> None:
        # Push constants: layout is `{u32 variant}` (vertex stage only).
        push_consts = struct.pack("<I", self._variant)

        with self._vk.acquire_write(self._uuid) as view:
            print(
                f"[VulkanGraphics/py] acquired vk_image=0x{view.vk_image:x} "
                f"layout={view.vk_image_layout}",
                flush=True,
            )
            self._vk.dispatch_graphics(
                color_target=self._uuid,
                extent_width=self._width,
                extent_height=self._height,
                vertex_spv=self._vert,
                fragment_spv=self._frag,
                pipeline_state=_triangle_pipeline_state(),
                bindings=[],
                binding_decls=[],
                vertex_buffers=[],
                push_constants=push_consts,
                push_constant_stages=_STAGE_VERTEX,
                descriptor_sets_in_flight=2,
                frame_index=0,
                viewport={
                    "x": 0.0,
                    "y": 0.0,
                    "width": float(self._width),
                    "height": float(self._height),
                    "min_depth": 0.0,
                    "max_depth": 1.0,
                },
                scissor={
                    "x": 0,
                    "y": 0,
                    "width": self._width,
                    "height": self._height,
                },
                draw={
                    "kind": "draw",
                    "vertex_count": 3,
                    "instance_count": 1,
                    "first_vertex": 0,
                    "first_instance": 0,
                    "index_count": 0,
                    "first_index": 0,
                    "vertex_offset": 0,
                },
                label="polyglot-triangle-py",
            )
        # Producer-side cross-process release (#643). The graphics
        # kernel's offscreen_render leaves the image in
        # COLOR_ATTACHMENT_OPTIMAL after the render pass.
        self._vk.release_for_cross_process(
            self._uuid, VkImageLayout.COLOR_ATTACHMENT_OPTIMAL
        )
        print(
            f"[VulkanGraphics/py] published cross-process release "
            f"layout=COLOR_ATTACHMENT_OPTIMAL for surface '{self._uuid}'",
            flush=True,
        )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print(
            f"[VulkanGraphics/py] teardown dispatched={self._dispatched} "
            f"error={self._error}",
            flush=True,
        )
