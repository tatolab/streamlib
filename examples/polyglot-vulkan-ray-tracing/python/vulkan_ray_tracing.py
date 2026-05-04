# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot Vulkan ray-tracing processor — Python.

End-to-end gate for the subprocess `VulkanContext.dispatch_ray_tracing`
runtime (#667). The host pre-allocates a storage-image-capable
`HostVulkanTexture` (transitioned to `GENERAL`), registers it via
`GpuContext::register_texture_with_layout` under a known UUID, and
installs a ``RayTracingKernelBridge`` wired to its
``VulkanRayTracingKernel`` + ``VulkanAccelerationStructure``. This
processor receives a trigger Videoframe, builds a single-triangle BLAS
+ identity TLAS via escalate IPC, registers the RT kernel, and
dispatches one ``vkCmdTraceRaysKHR`` against the host's storage image.

The traced scene is one triangle at the origin in the XY plane. The
ray-gen shader points a static camera at +Z = 2.5 looking toward
the origin; primary rays that miss render the variant-dependent sky
gradient, primary rays that hit the triangle render a barycentric
RGB palette. Variant 0 is the Python palette (teal-magenta sky);
variant 1 is the Deno palette (violet-orange sky), so the host's
PNG readback distinguishes which subprocess actually ran.

Config keys:
    vulkan_surface_uuid (str, required)
        Surface-share UUID the host registered the storage image under.
    width (int, required)
        Storage-image width in pixels.
    height (int, required)
        Storage-image height in pixels.
    variant (int, required)
        0 → Python palette, 1 → Deno palette.
    rgen_spv_hex (str, required)
        Hex-encoded SPIR-V bytecode for the ray-generation shader.
    rmiss_spv_hex (str, required)
        Hex-encoded SPIR-V bytecode for the miss shader.
    rchit_spv_hex (str, required)
        Hex-encoded SPIR-V bytecode for the closest-hit shader.
"""

from __future__ import annotations

import struct
from typing import Optional

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.vulkan import VulkanContext


# ---------------------------------------------------------------------------
# Scene geometry — one triangle at the origin in the XY plane. Vertices
# are at radius ≈ 0.6 so the triangle fills the central two-thirds of
# the static-camera frame.
# ---------------------------------------------------------------------------

_TRIANGLE_VERTICES = [
    0.0, 0.6, 0.0,    # top
    -0.6, -0.4, 0.0,  # bottom-left
    0.6, -0.4, 0.0,   # bottom-right
]
_TRIANGLE_INDICES = [0, 1, 2]


# Stage-flag bitmask — match the wire format and the host's
# RayTracingShaderStageFlags layout.
_STAGE_RAYGEN = 0b00_0001
_STAGE_MISS = 0b00_0010
_STAGE_CHIT = 0b00_0100

# Sentinel for "absent stage index" in the wire format. JTD has no
# Option<uint32>, so the wire form uses 0xFFFFFFFF.
_NO_STAGE = 0xFFFFFFFF


def _vertices_blob(vs):
    return struct.pack(f"<{len(vs)}f", *vs)


def _indices_blob(idx):
    return struct.pack(f"<{len(idx)}I", *idx)


def _identity_transform():
    # Row-major 3×4 identity, exactly 12 floats.
    return [
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
    ]


class VulkanRayTracingProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._uuid = str(cfg["vulkan_surface_uuid"])
        self._width = int(cfg["width"])
        self._height = int(cfg["height"])
        self._variant = int(cfg["variant"])
        self._rgen: bytes = bytes.fromhex(str(cfg["rgen_spv_hex"]))
        self._rmiss: bytes = bytes.fromhex(str(cfg["rmiss_spv_hex"]))
        self._rchit: bytes = bytes.fromhex(str(cfg["rchit_spv_hex"]))
        self._vk = VulkanContext.from_runtime(ctx)
        self._dispatched = False
        self._error: Optional[str] = None
        print(
            f"[VulkanRT/py] setup uuid={self._uuid} "
            f"size={self._width}x{self._height} "
            f"rgen_bytes={len(self._rgen)} rmiss_bytes={len(self._rmiss)} "
            f"rchit_bytes={len(self._rchit)} variant={self._variant}",
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
                f"[VulkanRT/py] trace dispatched into surface '{self._uuid}'",
                flush=True,
            )
        except Exception as e:
            self._error = str(e)
            print(
                f"[VulkanRT/py] dispatch failed: {e}", flush=True,
            )

    def _dispatch_once(self) -> None:
        # 1. Build the BLAS (single triangle in XY plane).
        blas_id = self._vk.build_blas(
            vertices=_vertices_blob(_TRIANGLE_VERTICES),
            indices=_indices_blob(_TRIANGLE_INDICES),
            label="polyglot-rt-py-triangle",
        )
        print(f"[VulkanRT/py] BLAS as_id={blas_id}", flush=True)

        # 2. Build the TLAS with one identity-transformed instance.
        tlas_id = self._vk.build_tlas(
            instances=[
                {
                    "blas_id": blas_id,
                    "transform": _identity_transform(),
                    "custom_index": 0,
                    "mask": 0xFF,
                    "sbt_record_offset": 0,
                    # `1 = TRIANGLE_FACING_CULL_DISABLE` + `4 = FORCE_OPAQUE`
                    "flags": 0b101,
                },
            ],
            label="polyglot-rt-py-tlas",
        )
        print(f"[VulkanRT/py] TLAS as_id={tlas_id}", flush=True)

        # 3. Push constants: layout is `{u32 variant}`.
        push_consts = struct.pack("<I", self._variant)

        # 4. Dispatch the trace. RT dispatch doesn't go through
        #    `acquire_write` — the host's storage image is a
        #    `STORAGE_BINDING`-only `HostVulkanTexture` registered via
        #    `register_texture_with_layout` on the in-tree side, never
        #    exported to surface-share. The host bridge resolves the
        #    UUID from its own surface map.
        self._vk.dispatch_ray_tracing(
            storage_image=self._uuid,
            tlas_id=tlas_id,
            width=self._width,
            height=self._height,
            stages=[
                {"stage": "ray_gen", "spv": self._rgen, "entry_point": "main"},
                {"stage": "miss", "spv": self._rmiss, "entry_point": "main"},
                {"stage": "closest_hit", "spv": self._rchit, "entry_point": "main"},
            ],
            groups=[
                # Group 0: ray-gen.
                {
                    "kind": "general",
                    "general_stage": 0,
                    "closest_hit_stage": _NO_STAGE,
                    "any_hit_stage": _NO_STAGE,
                    "intersection_stage": _NO_STAGE,
                },
                # Group 1: miss.
                {
                    "kind": "general",
                    "general_stage": 1,
                    "closest_hit_stage": _NO_STAGE,
                    "any_hit_stage": _NO_STAGE,
                    "intersection_stage": _NO_STAGE,
                },
                # Group 2: triangle hit (closest-hit only).
                {
                    "kind": "triangles_hit",
                    "general_stage": _NO_STAGE,
                    "closest_hit_stage": 2,
                    "any_hit_stage": _NO_STAGE,
                    "intersection_stage": _NO_STAGE,
                },
            ],
            binding_decls=[
                {
                    "binding": 0,
                    "kind": "acceleration_structure",
                    "stages": _STAGE_RAYGEN,
                },
                {
                    "binding": 1,
                    "kind": "storage_image",
                    "stages": _STAGE_RAYGEN,
                },
            ],
            bindings=[
                {
                    "binding": 0,
                    "kind": "acceleration_structure",
                    "target_id": "<tlas>",  # resolves to tlas_id
                },
                {
                    "binding": 1,
                    "kind": "storage_image",
                    "target_id": "<self>",  # resolves to surface uuid
                },
            ],
            push_constants=push_consts,
            push_constant_stages=_STAGE_RAYGEN | _STAGE_MISS,
            max_recursion_depth=1,
            label="polyglot-rt-py",
        )
        print(
            f"[VulkanRT/py] trace_rays complete for surface '{self._uuid}'",
            flush=True,
        )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print(
            f"[VulkanRT/py] teardown dispatched={self._dispatched} "
            f"error={self._error}",
            flush=True,
        )
