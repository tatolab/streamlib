# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot cpu-readback blur processor — Python.

End-to-end gate for the cpu-readback subprocess runtime (#529). The
host pre-allocates a cpu-readback surface, registers it with the
adapter, and uploads a vertical-color-band input pattern. This
processor receives a trigger Videoframe, opens the host surface
through ``CpuReadbackContext.acquire_write``, applies a Gaussian blur
to the BGRA bytes, and releases — the host adapter flushes the
modified bytes back into the surface's `VkImage`. After the runtime
stops, the host reads the surface back and writes a PNG that should
visually show the blurred input.

The Gaussian uses ``cv2.GaussianBlur`` if OpenCV is available; falls
back to a numpy separable kernel if it isn't (so the example still
runs on hosts without ``opencv-python``). Either path keeps the
identity-of-bytes invariant: the modified ndarray aliases the host
staging buffer, so writes happen in place.

Config keys:
    cpu_readback_surface_id (int, required)
        Host-assigned u64 surface id the host pre-registered with the
        cpu-readback adapter. The processor calls
        ``acquire_write(int(surface_id))`` on that id.
    kernel_size (int, default 11)
        Gaussian kernel side length (odd integer). Bigger = more
        blur. Visual gate works at any size ≥ 5.
    sigma (float, default 4.0)
        Gaussian sigma. Bigger = more blur.
"""

from __future__ import annotations

from typing import Any, Optional

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.cpu_readback import CpuReadbackContext


class CpuReadbackBlurProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._surface_id = int(cfg["cpu_readback_surface_id"])
        # Force kernel size to be odd so cv2.GaussianBlur accepts it.
        self._kernel_size = max(3, int(cfg.get("kernel_size", 11)) | 1)
        self._sigma = float(cfg.get("sigma", 4.0))
        self._cpu_readback = CpuReadbackContext.from_runtime(ctx)
        self._blur_count = 0
        self._error_count = 0
        self._last_error: Optional[str] = None
        print(
            f"[CpuReadbackBlur/py] setup surface_id={self._surface_id} "
            f"kernel={self._kernel_size} sigma={self._sigma}",
            flush=True,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # We don't care about frame contents — frame is the trigger.
        # Drain it so the upstream port doesn't backpressure.
        _frame = ctx.inputs.read("video_in")
        if _frame is None:
            return

        # Apply blur ONCE — repeat invocations would re-blur an already-
        # blurred image, which would still validate the wire path but
        # would obscure the visual gate.
        if self._blur_count > 0:
            return

        try:
            self._apply_blur_once()
            self._blur_count += 1
            print(
                f"[CpuReadbackBlur/py] blur applied (kernel={self._kernel_size}, "
                f"sigma={self._sigma})",
                flush=True,
            )
        except Exception as e:  # surface, don't crash — host gates on PNG output
            self._error_count += 1
            self._last_error = str(e)
            print(
                f"[CpuReadbackBlur/py] blur failed (count={self._error_count}): {e}",
                flush=True,
            )

    def _apply_blur_once(self) -> None:
        with self._cpu_readback.acquire_write(self._surface_id) as view:
            plane = view.plane(0)
            arr = plane.numpy  # (H, W, 4) BGRA, dtype uint8 — aliases staging
            blurred = self._gaussian_blur(arr)
            # In-place copy so the staging buffer ALIAS sees the result.
            # (Reassigning `arr = blurred` would not write back.)
            arr[...] = blurred

    def _gaussian_blur(self, arr: Any) -> Any:
        """Gaussian blur via cv2 if available, numpy fallback otherwise."""
        try:
            import cv2  # type: ignore
            return cv2.GaussianBlur(
                arr, (self._kernel_size, self._kernel_size), self._sigma
            )
        except ImportError:
            return self._numpy_gaussian_blur(arr)

    def _numpy_gaussian_blur(self, arr: Any) -> Any:
        """Pure-numpy separable Gaussian blur fallback. Slower than cv2
        but produces a visually-identical result for our purposes."""
        import numpy as np

        k = self._build_kernel()
        # Separable 1D kernel applied along axis 1 (horizontal) then
        # axis 0 (vertical). Pad with edge replication to avoid dark
        # borders.
        pad = self._kernel_size // 2
        # Don't blur the alpha channel — keep it opaque so the PNG
        # alpha doesn't go translucent at the borders.
        rgb = arr[..., :3].astype(np.float32)
        # Horizontal
        rgb_h = np.zeros_like(rgb)
        padded = np.pad(rgb, ((0, 0), (pad, pad), (0, 0)), mode="edge")
        for i in range(self._kernel_size):
            rgb_h += k[i] * padded[:, i : i + arr.shape[1], :]
        # Vertical
        rgb_v = np.zeros_like(rgb_h)
        padded = np.pad(rgb_h, ((pad, pad), (0, 0), (0, 0)), mode="edge")
        for i in range(self._kernel_size):
            rgb_v += k[i] * padded[i : i + arr.shape[0], :, :]
        out = np.empty_like(arr)
        out[..., :3] = np.clip(rgb_v, 0, 255).astype(np.uint8)
        out[..., 3] = arr[..., 3]
        return out

    def _build_kernel(self):
        import math

        import numpy as np

        ks = self._kernel_size
        sigma = self._sigma
        center = ks // 2
        x = np.arange(ks, dtype=np.float64) - center
        k = np.exp(-(x * x) / (2.0 * sigma * sigma))
        k /= k.sum()
        return k.astype(np.float32)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print(
            f"[CpuReadbackBlur/py] teardown blurs={self._blur_count} "
            f"errors={self._error_count} last_error={self._last_error}",
            flush=True,
        )
