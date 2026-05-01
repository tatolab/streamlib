# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot manual source — Python reference example for issues #542 + #604.

Demonstrates the canonical `execution: manual` worker-thread idiom:

1. ``start()`` spawns a worker thread and returns promptly so lifecycle
   commands (``stop``/``teardown``) can land.
2. The worker uses :class:`streamlib.MonotonicTimer` for drift-free
   pacing (NOT ``time.sleep``).
3. Each tick, the worker calls ``ctx.outputs.write(...)`` to publish a
   ``Videoframe`` over iceoryx2 to the destination input port. The
   counting-sink Rust plugin (``examples/polyglot-manual-source/plugin``)
   subscribes to that port and counts frames; the scenario binary reads
   the sink's stats file post-stop to verify frames flowed.
4. ``stop()`` flips a shutdown flag, joins the worker thread, returns.

Pre-#604, worker threads couldn't safely call ``outputs.write`` because
the Python cdylib's ``slpn_output_write`` aliased ``&mut`` against the
context across threads — instant UB once Python's ctypes released the
GIL on the FFI dispatch. #604 adds a [`Mutex`] inside
``PythonNativeContext`` so concurrent ``slpn_output_write`` calls
serialize on the publisher map. The worker idiom below is now safe by
construction.

If ``start()`` instead held a ``while True`` loop synchronously, the
subprocess runner's command loop would never iterate — no
``stop`` / ``teardown`` lifecycle message would land, and the host
falls through to a 5-second SIGKILL after timeout. That's the
failure mode this example exists to rule out.
"""

from __future__ import annotations

import threading
from typing import Optional

import streamlib
from streamlib import RuntimeContextFullAccess


class PolyglotManualSource:
    """Manual-mode polyglot source.

    Config keys:
        interval_ms (int, default 33): tick interval. 33ms ≈ 30fps.
        width (int, default 32): width to claim on the published Videoframe.
        height (int, default 32): height to claim on the published Videoframe.
        surface_id_prefix (str, default "polyglot-manual-source"): prefix
            for the synthetic surface_id field on each frame. Sinks that
            simply count don't need a real GPU surface; using a synthetic
            placeholder lets the example run without a host
            ``GpuContextFullAccess`` allocation.
    """

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        cfg = ctx.config
        self._interval_ns = int(cfg.get("interval_ms", 33)) * 1_000_000
        self._width = int(cfg.get("width", 32))
        self._height = int(cfg.get("height", 32))
        self._surface_id_prefix = str(
            cfg.get("surface_id_prefix", "polyglot-manual-source")
        )
        self._frame_count = 0
        self._stop_event = threading.Event()
        self._worker: Optional[threading.Thread] = None
        # Capture the outputs view so the worker thread can publish without
        # re-entering the lifecycle context. ``NativeOutputs`` holds only
        # the cdylib handle + ctx pointer, both stable across the
        # processor's lifetime.
        self._outputs = ctx.outputs
        streamlib.log.info(
            "PolyglotManualSource setup",
            interval_ns=self._interval_ns,
            width=self._width,
            height=self._height,
        )

    def start(self, _ctx: RuntimeContextFullAccess) -> None:
        # SHARP EDGE: this method MUST return promptly. The subprocess
        # runner calls start() inline on the command loop thread; if it
        # blocks, no subsequent `stop` / `teardown` lifecycle message can
        # be received. Spawn a worker, return immediately.
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
        # Replaces `time.sleep(interval_s)` which drifts and doesn't match
        # streamlib's monotonic-clock pacing philosophy.
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
                    self._publish_frame()

    def _publish_frame(self) -> None:
        """Publish one Videoframe on the `frame_out` port from this worker
        thread. Exercises the cdylib Mutex around the iceoryx2 publisher
        map (#604) — pre-fix, this raced with any other ``slpn_*`` call
        from the runner's main thread and was instant UB."""
        self._frame_count += 1
        ts_ns = streamlib.monotonic_now_ns()
        # The Videoframe schema accepts a synthetic surface_id (it's a
        # string used by the consumer to look up a GPU surface — a sink
        # that just counts doesn't need a real one).
        frame = {
            "surface_id": f"{self._surface_id_prefix}-{self._frame_count}",
            "width": self._width,
            "height": self._height,
            "timestamp_ns": str(ts_ns),
            "frame_index": str(self._frame_count),
        }
        try:
            self._outputs.write("frame_out", frame, timestamp_ns=ts_ns)
        except Exception as e:
            streamlib.log.warn(
                "PolyglotManualSource publish failed",
                error=str(e),
                frame_count=self._frame_count,
            )

    def stop(self, _ctx: RuntimeContextFullAccess) -> None:
        self._stop_event.set()
        worker = self._worker
        if worker is not None:
            worker.join(timeout=2.0)
            if worker.is_alive():
                streamlib.log.warn("PolyglotManualSource worker did not exit within 2s")
        streamlib.log.info(
            "PolyglotManualSource stop: worker joined",
            frames_emitted=self._frame_count,
        )
