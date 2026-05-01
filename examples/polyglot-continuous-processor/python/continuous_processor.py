# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot continuous processor — Python reference example for issue #542.

Demonstrates `execution: continuous` with monotonic-clock-driven
pacing. The subprocess runner's continuous-mode dispatch was reworked
in this issue to use `MonotonicTimer` (timerfd) instead of
`time.sleep`. This processor is the in-tree exemplar that exercises
that path.

Each `process()` call:

1. Records `streamlib.monotonic_now_ns()` as the tick timestamp.
2. Increments the tick counter.
3. Updates first/last-tick timestamps in memory — NO per-tick IO.

On `teardown()` the final stats (count, first_ns, last_ns) are
written to a host-visible output file as JSON. The host reads that
post-stop and asserts both that the count is in the expected range
AND that the implied average inter-tick interval is bounded.

Why no cpu-readback / per-tick IO: the goal here is to measure the
runner's pacing accuracy. Per-tick escalate IPC or GPU readback adds
~1–2ms of overhead that masks the timerfd's drift-free behavior in
the measurements. A real polyglot continuous processor doing GPU
work would use the Vulkan or OpenGL adapter, not cpu-readback —
cpu-readback is a last-resort tool, not a hot-path one.

Config keys:
    output_file (str, required): host-visible file path to write
        final tick stats into on teardown.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import streamlib
from streamlib import RuntimeContextFullAccess, RuntimeContextLimitedAccess


class PolyglotContinuousProcessor:
    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._output_file = Path(str(cfg["output_file"]))
        self._tick_count = 0
        self._first_tick_ns = 0
        self._last_tick_ns = 0
        # Initialize the file so the host always finds something to read.
        self._write_stats()
        streamlib.log.info(
            "PolyglotContinuousProcessor setup",
            output_file=str(self._output_file),
        )

    def process(self, _ctx: RuntimeContextLimitedAccess) -> None:
        # Hot path: pure in-memory state update. No IO, no IPC. The
        # whole point of the example is to measure the runner's
        # MonotonicTimer pacing accuracy without confounding overhead.
        now_ns = streamlib.monotonic_now_ns()
        if self._tick_count == 0:
            self._first_tick_ns = now_ns
        self._last_tick_ns = now_ns
        self._tick_count += 1

    def teardown(self, _ctx: RuntimeContextFullAccess) -> None:
        self._write_stats()
        streamlib.log.info(
            "PolyglotContinuousProcessor teardown",
            ticks=self._tick_count,
            first_ns=self._first_tick_ns,
            last_ns=self._last_tick_ns,
        )

    def _write_stats(self) -> None:
        """Atomically write tick stats to the output file (write tmp + rename)."""
        try:
            payload = json.dumps({
                "tick_count": self._tick_count,
                "first_tick_ns": self._first_tick_ns,
                "last_tick_ns": self._last_tick_ns,
            })
            tmp = self._output_file.with_suffix(self._output_file.suffix + ".tmp")
            tmp.write_text(payload)
            os.replace(tmp, self._output_file)
        except Exception as e:
            streamlib.log.warn(
                "PolyglotContinuousProcessor write failed",
                error=str(e),
                tick=self._tick_count,
            )
