# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Unit tests for `streamlib.log` — the subprocess-side unified logging
pathway. Covers:

- **Payload shape** — every field from the `{op:"log"}` wire contract is
  present and correctly typed.
- **Hot path** — `streamlib.log.info` must be fast enough to call from
  inside `process()` without stalling the frame loop.
- **Bounded queue** — drops are counted when full, and a synthetic
  heartbeat surfaces the drop count.
- **Interceptors** — `sys.stdout`, `sys.stderr`, and the root `logging`
  handler all emit records with `intercepted=true` and the expected
  `channel`.
- **Line buffering** — multi-line and partial-line writes each produce
  exactly one record per completed line.
- **ContextVars** — `pipeline_id` / `processor_id` propagate from their
  setter to the enqueued payload.
"""

from __future__ import annotations

import logging
import queue
import sys
import threading
import time
from typing import Any, Dict, List

import pytest

from streamlib import log
from streamlib import _log_interceptors


# ============================================================================
# Fake escalate channel — records payloads without hitting real IPC
# ============================================================================


class _FakeEscalateChannel:
    """Collects every payload that would be sent over the bridge."""

    def __init__(self) -> None:
        self.payloads: List[Dict[str, Any]] = []
        self._lock = threading.Lock()

    def log_fire_and_forget(self, payload: Dict[str, Any]) -> None:
        with self._lock:
            self.payloads.append(payload)

    def snapshot(self) -> List[Dict[str, Any]]:
        with self._lock:
            return list(self.payloads)


@pytest.fixture
def fake_channel():
    """Reset `streamlib.log` state, install a fake channel, tear down after."""
    log._reset_for_tests()
    ch = _FakeEscalateChannel()
    log.install(ch, install_interceptors=False)
    yield ch
    log.shutdown(timeout=2.0)
    log._reset_for_tests()


def _wait_for_payloads(channel: _FakeEscalateChannel, expected: int, timeout: float = 2.0):
    """Block until the fake channel has at least `expected` payloads."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if len(channel.snapshot()) >= expected:
            return channel.snapshot()
        time.sleep(0.005)
    return channel.snapshot()


# ============================================================================
# Payload shape
# ============================================================================


def test_info_payload_has_all_required_fields(fake_channel):
    log.info("hello", device="/dev/video0", count=3)
    payloads = _wait_for_payloads(fake_channel, 1)
    assert len(payloads) == 1
    p = payloads[0]
    assert p["op"] == "log"
    assert p["source"] == "python"
    assert p["level"] == "info"
    assert p["message"] == "hello"
    assert p["attrs"] == {"device": "/dev/video0", "count": 3}
    assert p["intercepted"] is False
    assert p["channel"] is None
    # source_seq is a string-encoded integer per the JTD wire contract.
    assert isinstance(p["source_seq"], str)
    int(p["source_seq"])
    # ISO8601 UTC, ending in Z.
    assert isinstance(p["source_ts"], str)
    assert p["source_ts"].endswith("Z")


@pytest.mark.parametrize(
    "level_fn,expected_level",
    [
        (log.trace, "trace"),
        (log.debug, "debug"),
        (log.info, "info"),
        (log.warn, "warn"),
        (log.error, "error"),
    ],
)
def test_level_functions_set_correct_level(fake_channel, level_fn, expected_level):
    level_fn("body")
    payloads = _wait_for_payloads(fake_channel, 1)
    assert len(payloads) == 1
    assert payloads[0]["level"] == expected_level


def test_context_vars_surface_in_payload(fake_channel):
    log.set_pipeline_id("pl-xyz")
    log.set_processor_id("pr-abc")
    log.info("ctx-test")
    payloads = _wait_for_payloads(fake_channel, 1)
    assert payloads[0]["pipeline_id"] == "pl-xyz"
    assert payloads[0]["processor_id"] == "pr-abc"


def test_source_seq_is_monotonic(fake_channel):
    for i in range(5):
        log.info(f"m{i}")
    payloads = _wait_for_payloads(fake_channel, 5)
    seqs = [int(p["source_seq"]) for p in payloads]
    assert seqs == sorted(seqs), f"seqs must be monotonic: {seqs}"
    # Distinct — no duplicates.
    assert len(set(seqs)) == len(seqs)


# ============================================================================
# Hot-path latency — queue-put-only, no formatting
# ============================================================================


def test_hot_path_is_fast():
    """p50 of `log.info(msg, **attrs)` must be under 10µs.

    The issue targets <5µs. We use 10µs here to keep the test stable on
    busy CI boxes while still failing loudly if we accidentally land a
    synchronous IPC wait or JSON encode on the hot path.
    """
    log._reset_for_tests()
    try:
        # No channel installed → the writer thread never runs, but enqueue
        # still works — we're measuring only the producer cost.
        N = 2000
        samples = []
        for _ in range(N):
            t0 = time.perf_counter_ns()
            log.info("hot-path", k=1)
            samples.append(time.perf_counter_ns() - t0)
        samples.sort()
        p50 = samples[N // 2]
        p99 = samples[int(N * 0.99)]
        assert p50 < 10_000, f"p50 too slow: {p50}ns; p99={p99}ns"
    finally:
        log._reset_for_tests()


# ============================================================================
# Bounded queue — drop on full + heartbeat
# ============================================================================


def test_queue_drops_increment_counter_when_full():
    """When the queue is full, `put_nowait` raises and the drop counter
    advances — the hot path never blocks on a full queue."""
    log._reset_for_tests()
    try:
        # Replace the module queue with a tiny one so we can overflow it
        # without enqueueing 65k records.
        log._queue = queue.Queue(maxsize=4)
        for _ in range(20):
            log.info("flood")
        assert log._drop_count_for_tests() >= 16
    finally:
        log._reset_for_tests()


def test_drop_heartbeat_surfaces_dropped_count(fake_channel):
    """Once enough records drop, a synthetic `dropped=N` heartbeat is
    emitted — the host sees the data loss instead of silent truncation."""
    log._reset_for_tests()
    ch = _FakeEscalateChannel()
    # Install with a tiny queue so we can force drops quickly. Must
    # install BEFORE overriding the queue so the writer thread picks up
    # the override.
    log.install(ch, install_interceptors=False)
    try:
        log._queue = queue.Queue(maxsize=2)
        # Burst — >> 1000 drops triggers the heartbeat on first sight.
        for _ in range(2500):
            log.info("burst")
        # The writer thread runs at ~10 Hz; give it a full tick to drain
        # and emit a heartbeat.
        deadline = time.monotonic() + 3.0
        heartbeat = None
        while time.monotonic() < deadline:
            for p in ch.snapshot():
                if p.get("attrs", {}).get("dropped"):
                    heartbeat = p
                    break
            if heartbeat is not None:
                break
            time.sleep(0.05)
        assert heartbeat is not None, "expected a dropped=N heartbeat record"
        assert heartbeat["level"] == "warn"
        assert heartbeat["attrs"]["dropped"] >= 1000
        assert "subprocess queue saturated" in heartbeat["message"]
    finally:
        log.shutdown(timeout=2.0)
        log._reset_for_tests()


# ============================================================================
# sys.stdout / sys.stderr interceptors — line-buffered
# ============================================================================


def test_stdout_interceptor_captures_print(fake_channel):
    # install_interceptors=False by default in the fixture; do it manually.
    _log_interceptors.install()
    try:
        print("hello from print")
        payloads = _wait_for_payloads(fake_channel, 1)
    finally:
        _log_interceptors.uninstall()
    captured = [p for p in payloads if p["channel"] == "stdout"]
    assert len(captured) == 1
    p = captured[0]
    assert p["intercepted"] is True
    assert p["message"] == "hello from print"
    assert p["source"] == "python"


def test_stderr_interceptor_captures_writes(fake_channel):
    _log_interceptors.install()
    try:
        sys.stderr.write("warn-line\n")
        payloads = _wait_for_payloads(fake_channel, 1)
    finally:
        _log_interceptors.uninstall()
    captured = [p for p in payloads if p["channel"] == "stderr"]
    assert len(captured) == 1
    assert captured[0]["message"] == "warn-line"
    assert captured[0]["intercepted"] is True


def test_multi_line_print_yields_one_record_per_line(fake_channel):
    _log_interceptors.install()
    try:
        print("a\nb\nc")
        payloads = _wait_for_payloads(fake_channel, 3)
    finally:
        _log_interceptors.uninstall()
    lines = [p["message"] for p in payloads if p["channel"] == "stdout"]
    assert lines == ["a", "b", "c"], f"got {lines}"


def test_partial_line_writes_buffer_until_newline(fake_channel):
    """A sequence of `write("a")`, `write("b")`, `write("c\\n")` yields
    exactly one record with `message="abc"`."""
    _log_interceptors.install()
    try:
        sys.stdout.write("a")
        sys.stdout.write("b")
        # Nothing has emitted yet.
        time.sleep(0.05)
        pre = [p for p in fake_channel.snapshot() if p["channel"] == "stdout"]
        assert pre == [], f"partial writes leaked records: {pre}"
        sys.stdout.write("c\n")
        payloads = _wait_for_payloads(fake_channel, 1)
    finally:
        _log_interceptors.uninstall()
    lines = [p["message"] for p in payloads if p["channel"] == "stdout"]
    assert lines == ["abc"], f"got {lines}"


# ============================================================================
# Root logging handler — Python-logging records routed through streamlib.log
# ============================================================================


def test_logging_module_routed_through_streamlib_log(fake_channel):
    _log_interceptors.install()
    try:
        logging.getLogger(__name__).warning("via-logging")
        payloads = _wait_for_payloads(fake_channel, 1)
    finally:
        _log_interceptors.uninstall()
    routed = [p for p in payloads if p["channel"] == "logging"]
    assert len(routed) == 1
    p = routed[0]
    assert p["level"] == "warn"
    assert "via-logging" in p["message"]
    assert p["intercepted"] is True
    assert p["attrs"]["logger"] == __name__


# ============================================================================
# Shutdown drains the queue
# ============================================================================


def test_shutdown_drains_pending_records():
    log._reset_for_tests()
    ch = _FakeEscalateChannel()
    log.install(ch, install_interceptors=False)
    try:
        for i in range(50):
            log.info(f"pending-{i}")
    finally:
        log.shutdown(timeout=2.0)
    assert len(ch.snapshot()) == 50
    log._reset_for_tests()
