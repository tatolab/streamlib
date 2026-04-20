# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

import numpy as np

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess


class GrayscaleProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        print("[GrayscaleProcessor] setup")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return

        # Resolve input surface → zero-copy IOSurface handle
        input_surface = ctx.gpu_limited_access.resolve_surface(frame["surface_id"])
        input_surface.lock(read_only=True)
        input_pixels = input_surface.as_numpy()  # numpy VIEW, no copy

        # Compute grayscale (BGRA order)
        gray = (
            0.114 * input_pixels[:, :, 0].astype(np.float32) +
            0.587 * input_pixels[:, :, 1].astype(np.float32) +
            0.299 * input_pixels[:, :, 2].astype(np.float32)
        ).astype(np.uint8)

        input_surface.unlock(read_only=True)

        # TODO(#325/#369): surface allocation is a privileged op — once the
        # polyglot escalate IPC grows an acquire_texture op, replace this
        # AttributeError-at-runtime access pattern with a proper
        # `ctx.escalate_acquire_pixel_buffer(...)` call. For now this mirrors
        # the Deno halftone example's pending-escalate pattern.
        w, h = input_surface.width, input_surface.height
        new_surface_id, output_surface = ctx.gpu_full_access.acquire_surface(  # type: ignore[attr-defined]
            width=w, height=h, format="bgra"
        )

        # Write grayscale to output surface (zero-copy via IOSurface)
        output_surface.lock(read_only=False)
        output_pixels = output_surface.as_numpy()
        output_pixels[:, :, 0] = gray  # B
        output_pixels[:, :, 1] = gray  # G
        output_pixels[:, :, 2] = gray  # R
        output_pixels[:, :, 3] = 255   # A
        output_surface.unlock(read_only=False)

        # Release IOSurface refs
        input_surface.release()
        output_surface.release()

        # Forward frame with new surface_id pointing to processed output
        frame["surface_id"] = new_surface_id
        ctx.outputs.write("video_out", frame)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print("[GrayscaleProcessor] teardown")
