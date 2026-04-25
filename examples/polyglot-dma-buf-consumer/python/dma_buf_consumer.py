# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Linux polyglot DMA-BUF consumer processor.

Subscribes to a video input port, resolves the upstream surface_id through
the surface-share service (DMA-BUF FD via SCM_RIGHTS, Vulkan-imported into the
subprocess), locks the resulting handle for read, and probes the first byte
of the imported buffer to confirm the cross-process import worked end-to-end.
Then forwards the frame unmodified to the downstream output so the rest of
the pipeline (e.g. display) keeps moving.

Stays dep-light on purpose — only ``ctypes`` for the byte probe so the
example doesn't pull numpy / pillow / etc. The point is to exercise the
control plane and CPU-mapped readback, not to do anything with the pixels.

Config keys (all optional):
    force_bad_surface_id (bool, default false)
        Negative test mode. Replaces the upstream surface_id with a synthetic
        UUID that the surface-share service won't resolve, exercising the
        consumer's failure-handling path. Frames still propagate downstream
        so the rest of the pipeline doesn't deadlock.
    log_every (int, default 60)
        Throttle for periodic resolve-success / resolve-failure log lines.
"""

import ctypes

from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess


_BOGUS_SURFACE_ID = "00000000-0000-0000-0000-000000000000"


class DmaBufConsumer:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self._force_bad_id = bool(ctx.config.get("force_bad_surface_id", False))
        self._log_every = int(ctx.config.get("log_every", 60))
        self._resolve_count = 0
        self._error_count = 0
        self._first_byte = None
        mode = "negative (force_bad_surface_id)" if self._force_bad_id else "normal"
        print(f"[DmaBufConsumer] setup mode={mode} log_every={self._log_every}", flush=True)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_in")
        if frame is None:
            return

        upstream_id = frame.get("surface_id")
        if upstream_id is None:
            return

        surface_id = _BOGUS_SURFACE_ID if self._force_bad_id else upstream_id

        try:
            handle = ctx.gpu_limited_access.resolve_surface(surface_id)
            handle.lock(read_only=True)
            try:
                base = handle._lib.slpn_gpu_surface_base_address(handle._handle_ptr)
                if not base:
                    raise RuntimeError("base address null after lock")
                self._first_byte = int(ctypes.c_uint8.from_address(base).value)
                self._resolve_count += 1
                if (self._resolve_count <= 3
                        or self._resolve_count % self._log_every == 0):
                    print(
                        f"[DmaBufConsumer] resolved surface "
                        f"{handle.width}x{handle.height} stride={handle.bytes_per_row} "
                        f"first_byte=0x{self._first_byte:02x} count={self._resolve_count}",
                        flush=True,
                    )
            finally:
                handle.unlock(read_only=True)
                handle.release()
        except RuntimeError as e:
            self._error_count += 1
            if (self._error_count <= 3
                    or self._error_count % self._log_every == 0):
                print(
                    f"[DmaBufConsumer] resolve_surface failed for "
                    f"surface_id={surface_id!r}: {e} count={self._error_count}",
                    flush=True,
                )

        ctx.outputs.write("video_out", frame)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        print(
            f"[DmaBufConsumer] teardown resolves={self._resolve_count} "
            f"errors={self._error_count} last_first_byte={self._first_byte}",
            flush=True,
        )
