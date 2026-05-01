# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot continuous processor — Python reference example for issue #542.

Demonstrates `execution: continuous` with monotonic-clock-driven
pacing. The subprocess runner's continuous-mode dispatch was reworked
in this issue to use `MonotonicTimer` (timerfd) instead of
`time.sleep`. This processor is the in-tree exemplar that exercises
that path.

Each ``process()`` call:

1. Records `streamlib.monotonic_now_ns()` as the tick timestamp.
2. Increments the tick counter.
3. Writes (counter, first_tick_ns, last_tick_ns) into the first 24
   bytes of a host-pre-registered cpu-readback surface.

After the runtime stops, the host reads the surface back and asserts
both that the counter is in the expected range AND that the implied
average inter-tick interval is bounded — i.e. the runner's new
timerfd dispatch paces correctly, neither faster nor much slower than
the manifest's `interval_ms`.

Config keys:
    cpu_readback_surface_id (int, required): host-assigned u64
        surface id.
"""

from __future__ import annotations

import struct

import streamlib
from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess
from streamlib.adapters.cpu_readback import CpuReadbackContext


class PolyglotContinuousProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._surface_id = int(cfg["cpu_readback_surface_id"])
        self._cpu_readback = CpuReadbackContext.from_runtime(ctx)
        self._tick_count = 0
        self._first_tick_ns = 0
        self._last_tick_ns = 0
        streamlib.log.info(
            "PolyglotContinuousProcessor setup",
            surface_id=self._surface_id,
        )

    def process(self, _ctx: RuntimeContextLimitedAccess) -> None:
        now_ns = streamlib.monotonic_now_ns()
        if self._tick_count == 0:
            self._first_tick_ns = now_ns
        self._last_tick_ns = now_ns
        self._tick_count += 1

        try:
            with self._cpu_readback.acquire_write(self._surface_id) as view:
                plane = view.plane(0)
                # Layout: u32 count, _padding (4B for alignment), u64
                # first_tick_ns, u64 last_tick_ns. 24 bytes total.
                # Use memoryview directly — no numpy dep needed.
                struct.pack_into(
                    "<IIQQ",
                    plane.bytes,
                    0,
                    self._tick_count & 0xFFFFFFFF,
                    0,
                    self._first_tick_ns,
                    self._last_tick_ns,
                )
        except Exception as e:
            streamlib.log.warn(
                "PolyglotContinuousProcessor write failed",
                error=str(e),
                tick=self._tick_count,
            )

    def teardown(self, _ctx: RuntimeContextFullAccess) -> None:
        streamlib.log.info(
            "PolyglotContinuousProcessor teardown",
            ticks=self._tick_count,
            first_ns=self._first_tick_ns,
            last_ns=self._last_tick_ns,
        )
