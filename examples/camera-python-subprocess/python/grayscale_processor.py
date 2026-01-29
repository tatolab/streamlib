# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

import numpy as np


class GrayscaleProcessor:
    def setup(self, ctx):
        print("[GrayscaleProcessor] setup")

    def process(self, ctx):
        # Videoframe msgpack array field order (from Rust struct declaration):
        #   [0] frame_index (str), [1] height (u32), [2] surface_id (str),
        #   [3] timestamp_ns (str), [4] width (u32)
        SURFACE_ID = 2
        HEIGHT = 1
        WIDTH = 4

        frame = ctx.inputs.read("video_in")
        if frame is None:
            return

        # Resolve input surface → zero-copy IOSurface handle
        input_surface = ctx.gpu.resolve_surface(frame[SURFACE_ID])
        input_surface.lock(read_only=True)
        input_pixels = input_surface.as_numpy()  # numpy VIEW, no copy

        # Compute grayscale (BGRA order)
        gray = (
            0.114 * input_pixels[:, :, 0].astype(np.float32) +
            0.587 * input_pixels[:, :, 1].astype(np.float32) +
            0.299 * input_pixels[:, :, 2].astype(np.float32)
        ).astype(np.uint8)

        input_surface.unlock(read_only=True)

        # Acquire new output surface from Rust pool
        w, h = input_surface.width, input_surface.height
        new_surface_id, output_surface = ctx.gpu.acquire_surface(
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
        frame[SURFACE_ID] = new_surface_id
        ctx.outputs.write("video_out", frame)

    def teardown(self, ctx):
        print("[GrayscaleProcessor] teardown")
