# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Grayscale processor — converts video frames to grayscale.

Demonstrates the canonical polyglot allocation path: subprocess holds a
limited-access GPU capability, and asks the host to allocate output pixel
buffers via ``escalate_acquire_pixel_buffer``. The returned ``handle_id``
is then resolved locally with ``gpu_limited_access.resolve_surface`` for
zero-copy write access — the same shape the input frame takes.
"""

from typing import List, Optional

import numpy as np

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess


_OUTPUT_POOL_SIZE = 3
# BT.601 luma weights for the BGRA channel order produced by the camera
# processor. Order matches input_pixels[..., :3] (B, G, R).
_BGR_LUMA_WEIGHTS = np.array([0.114, 0.587, 0.299], dtype=np.float32)


class GrayscaleProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self._output_pool: List[str] = []
        self._output_pool_index = 0
        self._output_pool_width = 0
        self._output_pool_height = 0
        self._frame_index = 0
        print("[GrayscaleProcessor] setup", flush=True)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return

        input_surface = ctx.gpu_limited_access.resolve_surface(frame["surface_id"])
        input_surface.lock(read_only=True)
        try:
            input_pixels = input_surface.as_numpy()
            height, width = input_pixels.shape[:2]
            # BT.601 luma over BGR; one matmul beats three scalar adds and
            # produces a single contiguous uint8 buffer.
            gray = (input_pixels[:, :, :3].astype(np.float32) @ _BGR_LUMA_WEIGHTS).astype(np.uint8)
        finally:
            input_surface.unlock(read_only=True)
            input_surface.release()

        self._ensure_output_pool(ctx, width, height)
        handle_id = self._output_pool[self._output_pool_index]
        self._output_pool_index = (self._output_pool_index + 1) % len(self._output_pool)

        output_surface = ctx.gpu_limited_access.resolve_surface(handle_id)
        output_surface.lock(read_only=False)
        try:
            output_pixels = output_surface.as_numpy()
            output_pixels[:, :, 0] = gray
            output_pixels[:, :, 1] = gray
            output_pixels[:, :, 2] = gray
            output_pixels[:, :, 3] = 255
        finally:
            output_surface.unlock(read_only=False)
            output_surface.release()

        self._frame_index += 1
        frame["surface_id"] = handle_id
        frame["width"] = width
        frame["height"] = height
        frame["frame_index"] = str(self._frame_index)
        ctx.outputs.write("video_out", frame)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        self._release_output_pool(ctx)
        print("[GrayscaleProcessor] teardown", flush=True)

    def _ensure_output_pool(
        self,
        ctx: RuntimeContextLimitedAccess,
        width: int,
        height: int,
    ) -> None:
        if (
            len(self._output_pool) == _OUTPUT_POOL_SIZE
            and self._output_pool_width == width
            and self._output_pool_height == height
        ):
            return
        self._release_output_pool(ctx)
        for _ in range(_OUTPUT_POOL_SIZE):
            ok = ctx.escalate_acquire_pixel_buffer(width, height, "bgra")
            self._output_pool.append(ok["handle_id"])
        self._output_pool_width = width
        self._output_pool_height = height
        self._output_pool_index = 0
        print(
            f"[GrayscaleProcessor] output pool ready: {_OUTPUT_POOL_SIZE}x {width}x{height}",
            flush=True,
        )

    def _release_output_pool(
        self,
        ctx: "RuntimeContextLimitedAccess | RuntimeContextFullAccess",
    ) -> None:
        drained = self._output_pool
        self._output_pool = []
        self._output_pool_width = 0
        self._output_pool_height = 0
        self._output_pool_index = 0
        for handle_id in drained:
            try:
                ctx.escalate_release_handle(handle_id)
            except Exception as e:
                print(
                    f"[GrayscaleProcessor] escalate_release_handle({handle_id}) failed: {e}",
                    flush=True,
                )
