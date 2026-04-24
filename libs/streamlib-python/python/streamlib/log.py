# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Polyglot unified logging for the Python subprocess SDK.

Public API:

    from streamlib import log
    log.info("captured frame", frame_index=42)
    log.error("decode failed", error=str(e))

Records are serialized as `{op: "log", ...}` escalate-IPC payloads and
enqueued into a bounded local queue. A daemon writer thread drains the
queue and fires each payload over the escalate channel — fire-and-forget,
no correlated response. The host handler routes into the unified JSONL
pathway (see `streamlib.core.logging.polyglot_sink`).

The hot path is intentionally cheap — a dataclass construction plus a
`Queue.put_nowait` — so `streamlib.log.info(...)` from inside `process()`
doesn't stall the frame loop. ISO8601 formatting and JSON encoding happen
on the writer thread.

See `docs/architecture/polyglot-logging.md` and issue #430 for the
cross-runtime design.
"""

from __future__ import annotations

import contextvars
import itertools
import os
import queue
import threading
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any, Dict, Optional


# ============================================================================
# Public constants
# ============================================================================

#: Default bounded-queue capacity. Oldest record is dropped when full.
DEFAULT_QUEUE_CAPACITY = 65536

#: Emit a synthetic `dropped=N` heartbeat every N accumulated drops.
_HEARTBEAT_DROP_THRESHOLD = 1000

#: Emit a synthetic `dropped=N` heartbeat at least once per this many
#: nanoseconds while drops are outstanding.
_HEARTBEAT_INTERVAL_NS = 1_000_000_000

_LEVELS = ("trace", "debug", "info", "warn", "error")


# ============================================================================
# Processor-context vars — read on the hot path, set by subprocess_runner
# ============================================================================


_pipeline_id: contextvars.ContextVar[Optional[str]] = contextvars.ContextVar(
    "streamlib_pipeline_id", default=None
)
_processor_id: contextvars.ContextVar[Optional[str]] = contextvars.ContextVar(
    "streamlib_processor_id", default=None
)


def set_pipeline_id(pipeline_id: Optional[str]) -> None:
    """Set the pipeline-id context var used by subsequent log calls."""
    _pipeline_id.set(pipeline_id)


def set_processor_id(processor_id: Optional[str]) -> None:
    """Set the processor-id context var used by subsequent log calls."""
    _processor_id.set(processor_id)


# ============================================================================
# Queued record — intermediate representation between hot path and writer
# ============================================================================


@dataclass(slots=True)
class _QueuedRecord:
    level: str
    message: str
    attrs: Dict[str, Any]
    intercepted: bool
    channel: Optional[str]
    pipeline_id: Optional[str]
    processor_id: Optional[str]
    source_seq: int
    source_ts_ns: int


# ============================================================================
# Module state — the bounded queue, counters, writer thread handle
# ============================================================================


_seq_counter = itertools.count(0)
_queue: "queue.Queue[_QueuedRecord]" = queue.Queue(maxsize=DEFAULT_QUEUE_CAPACITY)

# Drop counter — incremented on the hot path when the queue is full.
# Read and reset by the writer thread when emitting a drop heartbeat.
_drop_lock = threading.Lock()
_drop_count: int = 0
_last_heartbeat_ns: int = 0

# Writer-thread handle, populated by install().
_writer_thread: Optional[threading.Thread] = None
_writer_stop = threading.Event()
_channel_ref: Any = None  # Optional[EscalateChannel] — avoid import cycle


# ============================================================================
# Hot path — build payload and enqueue
# ============================================================================


def _emit(
    level: str,
    message: str,
    attrs: Dict[str, Any],
    *,
    intercepted: bool = False,
    channel: Optional[str] = None,
) -> None:
    """Enqueue a single log record. Hot path — no format, no IPC wait."""
    global _drop_count
    rec = _QueuedRecord(
        level=level,
        message=message,
        attrs=attrs,
        intercepted=intercepted,
        channel=channel,
        pipeline_id=_pipeline_id.get(),
        processor_id=_processor_id.get(),
        source_seq=next(_seq_counter),
        source_ts_ns=time.time_ns(),
    )
    try:
        _queue.put_nowait(rec)
    except queue.Full:
        with _drop_lock:
            _drop_count += 1


def trace(message: str, **attrs: Any) -> None:
    """Emit a trace-level record."""
    _emit("trace", message, attrs)


def debug(message: str, **attrs: Any) -> None:
    """Emit a debug-level record."""
    _emit("debug", message, attrs)


def info(message: str, **attrs: Any) -> None:
    """Emit an info-level record."""
    _emit("info", message, attrs)


def warn(message: str, **attrs: Any) -> None:
    """Emit a warn-level record."""
    _emit("warn", message, attrs)


def error(message: str, **attrs: Any) -> None:
    """Emit an error-level record."""
    _emit("error", message, attrs)


def emit_intercepted(
    level: str,
    message: str,
    channel: str,
    attrs: Optional[Dict[str, Any]] = None,
) -> None:
    """Enqueue a record captured by an interceptor (stdout, stderr, logging, fd*).

    Called by `_log_interceptors` — not part of the processor-author API.
    """
    if level not in _LEVELS:
        level = "warn"
    _emit(level, message, attrs or {}, intercepted=True, channel=channel)


# ============================================================================
# Writer thread — drain queue, format payload, send over escalate IPC
# ============================================================================


def _format_source_ts(source_ts_ns: int) -> str:
    """Format a `time.time_ns()` value as an ISO8601 UTC string."""
    seconds, nanos = divmod(source_ts_ns, 1_000_000_000)
    dt = datetime.fromtimestamp(seconds, tz=timezone.utc)
    # Python's isoformat includes microseconds only; pad ns explicitly.
    micros, _ = divmod(nanos, 1000)
    return dt.replace(microsecond=micros).isoformat().replace("+00:00", "Z")


def _build_payload(rec: _QueuedRecord) -> Dict[str, Any]:
    return {
        "op": "log",
        "source": "python",
        "source_seq": str(rec.source_seq),
        "source_ts": _format_source_ts(rec.source_ts_ns),
        "level": rec.level,
        "message": rec.message,
        "attrs": rec.attrs,
        "intercepted": rec.intercepted,
        "channel": rec.channel,
        "pipeline_id": rec.pipeline_id,
        "processor_id": rec.processor_id,
    }


def _build_drop_heartbeat(drops: int) -> Dict[str, Any]:
    """Build a synthetic `dropped=N` record. Bypasses the queue."""
    seq = next(_seq_counter)
    return {
        "op": "log",
        "source": "python",
        "source_seq": str(seq),
        "source_ts": _format_source_ts(time.time_ns()),
        "level": "warn",
        "message": f"dropped {drops} log records (subprocess queue saturated)",
        "attrs": {"dropped": drops},
        "intercepted": False,
        "channel": None,
        "pipeline_id": _pipeline_id.get(),
        "processor_id": _processor_id.get(),
    }


def _maybe_emit_heartbeat() -> None:
    """Emit a drop-heartbeat if one is due."""
    global _drop_count, _last_heartbeat_ns
    with _drop_lock:
        drops = _drop_count
        now = time.time_ns()
        if drops == 0:
            return
        elapsed = now - _last_heartbeat_ns
        if drops < _HEARTBEAT_DROP_THRESHOLD and elapsed < _HEARTBEAT_INTERVAL_NS:
            return
        _drop_count = 0
        _last_heartbeat_ns = now
    _send_direct(_build_drop_heartbeat(drops))


def _send_direct(payload: Dict[str, Any]) -> bool:
    """Send a payload via the escalate channel. Returns False on fatal IO error."""
    channel = _channel_ref
    if channel is None:
        return True
    try:
        channel.log_fire_and_forget(payload)
        return True
    except (BrokenPipeError, OSError, ValueError):
        return False


def _writer_loop() -> None:
    """Drain queue and send each payload until shutdown."""
    while not _writer_stop.is_set():
        try:
            rec = _queue.get(timeout=0.1)
        except queue.Empty:
            _maybe_emit_heartbeat()
            continue
        if not _send_direct(_build_payload(rec)):
            # Bridge pipe broken — subprocess is going away. Drain without
            # sending so the main thread can exit cleanly.
            _writer_stop.set()
            break
        _maybe_emit_heartbeat()

    # Drain remaining records on shutdown so flush() has the expected effect.
    while True:
        try:
            rec = _queue.get_nowait()
        except queue.Empty:
            break
        _send_direct(_build_payload(rec))
    _maybe_emit_heartbeat()


# ============================================================================
# Install / shutdown — called by subprocess_runner
# ============================================================================


def install(channel: Any, *, install_interceptors: bool = True) -> None:
    """Start the writer thread and (optionally) install subprocess-side interceptors.

    Called by `subprocess_runner.main` after `install_channel(escalate_channel)`.
    Idempotent — a second call is a no-op.
    """
    global _channel_ref, _writer_thread, _last_heartbeat_ns
    if _writer_thread is not None and _writer_thread.is_alive():
        return
    _channel_ref = channel
    _last_heartbeat_ns = time.time_ns()
    _writer_stop.clear()
    _writer_thread = threading.Thread(
        target=_writer_loop,
        name="streamlib-log-writer",
        daemon=True,
    )
    _writer_thread.start()

    if install_interceptors:
        from . import _log_interceptors

        _log_interceptors.install()


def shutdown(timeout: float = 2.0) -> None:
    """Stop the writer thread and flush remaining records.

    Called during subprocess teardown. Safe to call multiple times.
    """
    global _writer_thread, _channel_ref
    _writer_stop.set()
    if _writer_thread is not None:
        _writer_thread.join(timeout=timeout)
    _writer_thread = None
    _channel_ref = None


# ============================================================================
# Test helpers — not part of the public API
# ============================================================================


def _reset_for_tests() -> None:
    """Reset module state between pytest cases. NOT for production use."""
    global _queue, _seq_counter, _drop_count, _last_heartbeat_ns
    global _writer_thread, _channel_ref
    shutdown(timeout=1.0)
    _queue = queue.Queue(maxsize=DEFAULT_QUEUE_CAPACITY)
    _seq_counter = itertools.count(0)
    with _drop_lock:
        _drop_count = 0
        _last_heartbeat_ns = 0
    _writer_thread = None
    _channel_ref = None
    _pipeline_id.set(None)
    _processor_id.set(None)


def _queue_size_for_tests() -> int:
    """Current queue depth. NOT for production use."""
    return _queue.qsize()


def _drop_count_for_tests() -> int:
    """Current drop count. NOT for production use."""
    with _drop_lock:
        return _drop_count
