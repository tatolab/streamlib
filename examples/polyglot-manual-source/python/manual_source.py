# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot manual source — Python reference example for issue #542.

Demonstrates the canonical `execution: manual` worker-thread idiom:

1. ``start()`` spawns a worker thread and returns promptly.
2. The worker uses `streamlib.MonotonicTimer` for drift-free pacing
   (NOT `time.sleep`).
3. Each tick, the worker writes an incrementing frame counter into a
   host-visible output file (atomic: write tmp + rename). The host
   reads this file post-stop to verify frames flowed.
4. ``stop()`` flips a shutdown flag, joins the worker thread, and
   returns. The runtime exits cleanly without falling through to
   the host's SIGKILL fallback.

If ``start()`` instead held a ``while True`` loop synchronously, the
subprocess runner's command loop would never iterate — no
``stop`` / ``teardown`` lifecycle message would land, and the host
falls through to a 5-second SIGKILL after timeout. That's the
failure mode this example exists to rule out.

Why a file and not the iceoryx2 output port: a polyglot source that
publishes ``Videoframe`` payloads needs to allocate pixel buffers via
escalate IPC ``acquire_pixel_buffer`` and call ``outputs.write`` from
a thread the host reads — concurrent escalate IPC from a worker
thread is not safe under the current bridge protocol (the runner's
outer command loop and the worker would both read from stdin). A
host-visible file sidesteps that out-of-scope plumbing and keeps the
example focused on the worker-thread idiom.
"""

from __future__ import annotations

import os
import threading
from pathlib import Path
from typing import Optional

import streamlib
from streamlib import RuntimeContextFullAccess


class PolyglotManualSource:
    """Manual-mode polyglot source.

    Config keys:
        output_file (str, required): host-visible file path the
            worker thread writes the latest frame count into.
        interval_ms (int, default 33): tick interval. 33ms ≈ 30fps.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._output_file = Path(str(cfg["output_file"]))
        self._interval_ns = int(cfg.get("interval_ms", 33)) * 1_000_000
        self._frame_count = 0
        self._stop_event = threading.Event()
        self._worker: Optional[threading.Thread] = None
        # Initialize the file so the host always finds something to read.
        self._write_count_atomic(0)
        streamlib.log.info(
            "PolyglotManualSource setup",
            output_file=str(self._output_file),
            interval_ns=self._interval_ns,
        )

    def start(self, _ctx: RuntimeContextFullAccess) -> None:
        # SHARP EDGE: this method MUST return promptly. The subprocess
        # runner calls start() inline on the command loop thread; if
        # it blocks (e.g. a `while True:` loop here), no subsequent
        # `stop` / `teardown` lifecycle message can be received and
        # the host falls through to a 5-second SIGKILL after timeout.
        # Spawn a worker, return immediately.
        self._stop_event.clear()
        self._worker = threading.Thread(
            target=self._worker_loop,
            name="polyglot-manual-source-worker",
            daemon=True,
        )
        self._worker.start()
        streamlib.log.info("PolyglotManualSource start: worker spawned")

    def _worker_loop(self) -> None:
        # MonotonicTimer is the canonical drift-free pacing primitive.
        # Replaces `time.sleep(interval_s)` which drifts and doesn't
        # match streamlib's monotonic-clock pacing philosophy.
        with streamlib.MonotonicTimer(self._interval_ns) as timer:
            while not self._stop_event.is_set():
                expirations = timer.wait(100)  # 100ms timeout bounds shutdown latency
                if expirations < 0:
                    streamlib.log.error("MonotonicTimer wait failed; worker exiting")
                    return
                if expirations == 0:
                    continue
                for _ in range(expirations):
                    if self._stop_event.is_set():
                        return
                    self._frame_count += 1
                    self._write_count_atomic(self._frame_count)

    def _write_count_atomic(self, count: int) -> None:
        """Write `count` to the output file atomically (write-tmp + rename)."""
        try:
            tmp = self._output_file.with_suffix(self._output_file.suffix + ".tmp")
            tmp.write_text(str(count))
            os.replace(tmp, self._output_file)
        except Exception as e:
            streamlib.log.warn(
                "PolyglotManualSource write failed",
                error=str(e),
                count=count,
            )

    def stop(self, _ctx: RuntimeContextFullAccess) -> None:
        self._stop_event.set()
        worker = self._worker
        if worker is not None:
            worker.join(timeout=2.0)
            if worker.is_alive():
                streamlib.log.warn("PolyglotManualSource worker did not exit within 2s")
        # Final write so the host always sees the last value.
        self._write_count_atomic(self._frame_count)
        streamlib.log.info(
            "PolyglotManualSource stop: worker joined",
            frames_emitted=self._frame_count,
        )
