# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
# streamlib:lint-logging:allow-file — a describe has no parent log channel, so its
# records are written to raw stderr, which the parent mirrors into its own log.

"""Logging from a processor interpreter: describe-time records to stderr, the rest to its parent."""

from __future__ import annotations

import sys
import threading
from datetime import datetime, timezone
from typing import Any, Optional

from tatolab.runtime._engine import (
    capture_this_helper_processes_engine_log_records,
    drain_the_engine_log_records_this_helper_captured,
)
from tatolab.runtime._parent_process_bridge import ParentProcessBridge


# How long the engine-log forwarder parks waiting for a record before it looks
# at its own stop flag again. It bounds nothing a user sees: a record arriving
# wakes the wait at once.
SECONDS_THE_ENGINE_LOG_FORWARDER_WAITS_FOR_A_RECORD = 0.25

# How long the forwarder is given to make its last pass at shutdown. A
# forwarder still going then is wedged in a write to a parent that has stopped
# reading, which no second sender would get past either, so what it was
# carrying is lost with it rather than holding up the helper's exit.
SECONDS_TO_STOP_THE_ENGINE_LOG_FORWARDER = 1.0


def write_a_describe_time_log_record_to_stderr(
    level: str, message: str, attrs: "Optional[dict[str, Any]]"
) -> None:
    """Write one record a described module logs at import as a line on stderr,
    which the parent mirrors into its own log; a describe has no parent channel."""
    rendered_attrs = "".join(f" {name}={value!r}" for name, value in (attrs or {}).items())
    sys.stderr.write(f"{level.upper()} {message}{rendered_attrs}\n")
    sys.stderr.flush()


class ParentProcessLogSink:
    """Sends this helper's records to the parent's unified log pipeline.

    Fire-and-forget: the parent stamps the authoritative receipt time and
    enqueues, and never replies. Attribution is a process constant — one
    helper hosts exactly one processor.
    """

    def __init__(self, bridge: ParentProcessBridge, processor_id: str) -> None:
        self._bridge = bridge
        self._processor_id = processor_id
        self._next_sequence_number = 0
        self._sequence_lock = threading.Lock()

    def __call__(
        self, level: str, message: str, attrs: "Optional[dict[str, Any]]"
    ) -> None:
        self._send(
            source="python",
            level=level,
            message=message,
            attrs=attrs or {},
            # Advisory only — the parent's receipt stamp is what orders the
            # merged stream. Wall clock is what a human reads.
            source_ts=datetime.now(timezone.utc).isoformat(),
            pipeline_id=None,
            processor_id=self._processor_id,
        )

    def send_a_captured_engine_record(self, record: "dict[str, Any]") -> None:
        """Send one engine `tracing` record this helper captured.

        It travels as the Rust record it is — its own target, its own level —
        so a call site inside a helper reads in the runtime's log exactly as
        the same call site reads from the app process. Its stamp is when the
        engine made it, not when this thread got to it.
        """
        self._send(
            source="rust",
            level=record["level"],
            message=record["message"],
            attrs=record["attrs"],
            source_ts=_wall_clock_nanoseconds_as_iso8601(
                record["emitted_at_wall_clock_nanoseconds"]
            ),
            pipeline_id=record["pipeline_id"],
            # One helper hosts one processor, so a record that names none is
            # still this processor's.
            processor_id=record["processor_id"] or self._processor_id,
            target=record["target"],
            rhi_op=record["rhi_op"],
        )

    def _send(
        self,
        *,
        source: str,
        level: str,
        message: str,
        attrs: "dict[str, Any]",
        source_ts: str,
        pipeline_id: "Optional[str]",
        processor_id: "Optional[str]",
        target: "Optional[str]" = None,
        rhi_op: "Optional[str]" = None,
    ) -> None:
        # Numbered and sent under one lock. Two threads send a helper's
        # records — its processor's and the engine-log forwarder's — and a
        # number taken here but sent after the next one's frame would put the
        # sequence out of order on the wire, where a reader takes a step
        # backwards for records lost.
        with self._sequence_lock:
            self._next_sequence_number += 1
            record: "dict[str, Any]" = {
                "rpc": "escalate_request",
                "op": "log",
                "source": source,
                "source_seq": str(self._next_sequence_number),
                "source_ts": source_ts,
                "level": level,
                "message": message,
                "intercepted": False,
                "channel": None,
                "pipeline_id": pipeline_id,
                "processor_id": processor_id,
                "attrs": attrs,
            }
            # Named only where there is one to name: the two columns an engine
            # record fills are absent from every `tatolab.stream.log` document,
            # which is the document helpers have always sent.
            if target is not None:
                record["target"] = target
            if rhi_op is not None:
                record["rhi_op"] = rhi_op
            self._bridge.send(record)


def _wall_clock_nanoseconds_as_iso8601(wall_clock_nanoseconds: int) -> str:
    return datetime.fromtimestamp(
        wall_clock_nanoseconds / 1_000_000_000, timezone.utc
    ).isoformat()


class CapturedEngineLogRecordForwarder:
    """Sends the engine's own records, made inside this helper, to the parent.

    A helper hosts no engine and writes no log of its own, so the records the
    engine and iceoryx2 make here queue in a ring the engine owns and this
    thread drains. The crossing is one-way by construction: a `tracing` event
    lands on whatever thread emitted it — a garbage-collector finalizer's, a
    `Drop` path's — and Rust calling Python from one of those deadlocks.
    """

    def __init__(self, sink: ParentProcessLogSink) -> None:
        self._sink = sink
        self._capturing = False
        self._stopping = threading.Event()
        self._thread = threading.Thread(
            target=self._forward_until_stopped,
            name="streamlib-engine-log-forwarder",
            daemon=True,
        )

    def capture_and_start(self) -> None:
        """Capture this process's engine records and start sending them on.

        A process that cannot capture keeps running without them, and says so:
        the records are a diagnostic, and a processor that works is not worth
        failing over the logging around it.
        """
        try:
            capture_this_helper_processes_engine_log_records()
        except Exception as capture_failure:
            self._sink(
                "warn",
                "this helper could not capture the engine's own log records, so they stay "
                "in this process; its Python records are unaffected",
                {"error": str(capture_failure)},
            )
            return
        self._capturing = True
        self._thread.start()

    def stop_after_forwarding_what_is_left(self) -> None:
        """Stop the thread, waiting for it to send what the ring still holds.

        Called on every way out of `main`, so the records explaining a helper
        that could not start are in the parent's hands before it exits.

        The thread makes that last pass itself rather than being raced for it:
        a record it has taken from the ring but not yet sent is in no ring for
        a second drainer to find, and a second drainer would block behind the
        first on the bridge's write lock — so a forwarder wedged in a write
        would hold up the helper's exit instead of costing it the diagnostics
        the wedged write was carrying.
        """
        if not self._capturing:
            return
        self._stopping.set()
        self._thread.join(timeout=SECONDS_TO_STOP_THE_ENGINE_LOG_FORWARDER)

    def _forward_until_stopped(self) -> None:
        while not self._stopping.is_set():
            self._forward_what_the_ring_holds(
                wait_seconds=SECONDS_THE_ENGINE_LOG_FORWARDER_WAITS_FOR_A_RECORD
            )
        # The pass that empties the ring for the last time, so the records a
        # helper made on its way out travel with the ones before them.
        self._forward_what_the_ring_holds(wait_seconds=0.0)

    def _forward_what_the_ring_holds(self, wait_seconds: float) -> None:
        records, dropped = drain_the_engine_log_records_this_helper_captured(wait_seconds)
        for record in records:
            self._sink.send_a_captured_engine_record(record)
        if dropped:
            self._sink(
                "warn",
                f"dropped {dropped} engine log records before this helper could send them",
                {"dropped": dropped},
            )
