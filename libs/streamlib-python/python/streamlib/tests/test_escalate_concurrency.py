# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Concurrency tests for [`EscalateChannel`] (#604).

Pre-#604, ``EscalateChannel.request()`` held a single ``_send_lock``
through both ``bridge_send_message`` and ``_await_response``. The
single-flight invariant was: only one request is in flight at a time;
the runner's outer loop is the sole reader of stdin between escalate
roundtrips. In manual mode, a worker thread that issued an escalate
request would compete with the outer loop for stdin reads, sometimes
stealing a lifecycle command into ``_await_response``'s loop or losing
its correlated response to the outer loop.

#604 replaced that with:

- a dedicated [`BridgeReaderThread`] that owns the escalate fd reader
  and demuxes incoming frames by ``rpc``: ``escalate_response`` →
  [`EscalateChannel.deliver_response`] (looking up the waiter by
  ``request_id``); everything else → a lifecycle queue the runner
  drains on its main thread.
- per-request slots ([`_PendingResponse`]) keyed by ``request_id``
  inside [`EscalateChannel`], so concurrent ``request()`` calls
  register non-aliasing waiters and block on per-slot
  ``threading.Event``s.

These tests drive a fake host over a real ``socket.socketpair`` (so the
production code path — ``socket.makefile("rb"/"wb")`` + length-prefixed
JSON framing — is exercised end-to-end) and assert that:

1. 200+ concurrent ``request()`` calls from N worker threads all
   correlate back to their own caller, even when the host responds
   out-of-order with respect to send order.
2. Lifecycle messages interleaved with escalate responses are routed
   into the lifecycle queue and never observed by ``request()``.
3. EOF on the escalate fd wakes every in-flight ``request()`` with an
   ``EscalateError`` instead of hanging the test runner.
"""

from __future__ import annotations

import json
import queue
import socket
import struct
import threading
import time
from typing import Any, Dict, List, Tuple

import pytest

from streamlib.escalate import (
    BridgeReaderThread,
    EscalateChannel,
    EscalateError,
)


# ============================================================================
# Helpers
# ============================================================================


def _make_pair() -> Tuple[Any, Any, Any, Any]:
    """Set up a socketpair representing the (subprocess, host) bridge.

    Returns ``(subprocess_reader, subprocess_writer, host_reader, host_writer)``.
    The subprocess side is wrapped in unbuffered file objects exactly the
    way ``subprocess_runner._open_escalate_fd_stream`` does so the
    framing exercised by [`bridge_read_message`] / [`bridge_send_message`]
    matches production.
    """
    sub_sock, host_sock = socket.socketpair()
    sub_reader = sub_sock.makefile("rb", buffering=0)
    sub_writer = sub_sock.makefile("wb", buffering=0)
    host_reader = host_sock.makefile("rb", buffering=0)
    host_writer = host_sock.makefile("wb", buffering=0)
    # Caller is responsible for closing the writers and original sockets;
    # tests use try/finally to drive that.
    return sub_reader, sub_writer, host_reader, host_writer, sub_sock, host_sock


def _read_frame(stream) -> Dict[str, Any]:
    len_buf = stream.read(4)
    if len(len_buf) < 4:
        raise EOFError("pair closed")
    (length,) = struct.unpack(">I", len_buf)
    body = stream.read(length)
    if len(body) < length:
        raise EOFError("pair closed mid-message")
    return json.loads(body)


def _send_frame(stream, msg: Dict[str, Any]) -> None:
    body = json.dumps(msg, separators=(",", ":")).encode("utf-8")
    stream.write(struct.pack(">I", len(body)))
    stream.write(body)
    stream.flush()


# ============================================================================
# Tests
# ============================================================================


def test_concurrent_requests_correlate_correctly():
    """200 concurrent request/response cycles across 8 worker threads.
    The host echo loop runs in its own thread and replies immediately,
    so workers fire their next request as soon as the previous resolves
    — putting up to 8 requests in flight at once and exercising the
    cross-talk path that pre-#604 lock-while-reading would corrupt.
    """
    sub_reader, sub_writer, host_reader, host_writer, sub_sock, host_sock = _make_pair()
    channel = EscalateChannel(sub_writer)
    lifecycle_queue: "queue.Queue[Dict[str, Any]]" = queue.Queue()
    reader = BridgeReaderThread(sub_reader, channel, lifecycle_queue)
    reader.start()

    THREADS = 8
    PER_THREAD = 25  # 200 total

    host_stop = threading.Event()

    def _host_loop() -> None:
        while not host_stop.is_set():
            try:
                msg = _read_frame(host_reader)
            except EOFError:
                return
            if msg.get("rpc") != "escalate_request":
                continue
            request_id = msg["request_id"]
            worker_id = msg.get("_worker_id")
            iter_idx = msg.get("_iter")
            _send_frame(
                host_writer,
                {
                    "rpc": "escalate_response",
                    "request_id": request_id,
                    "result": "ok",
                    "echo_tag": f"w{worker_id}-i{iter_idx}",
                },
            )

    host_thread = threading.Thread(
        target=_host_loop, name="test-host-loop", daemon=True
    )
    host_thread.start()

    failures: List[str] = []
    failures_lock = threading.Lock()

    def _worker(worker_id: int) -> None:
        for i in range(PER_THREAD):
            payload = {
                "op": "test_correlation",
                "_worker_id": worker_id,
                "_iter": i,
            }
            try:
                response = channel.request(payload)
            except EscalateError as e:
                with failures_lock:
                    failures.append(f"worker {worker_id} iter {i}: {e}")
                return
            assert response is not None
            tag = response["echo_tag"]
            expected = f"w{worker_id}-i{i}"
            if tag != expected:
                with failures_lock:
                    failures.append(
                        f"worker {worker_id} iter {i}: got tag {tag!r}, expected {expected!r}"
                    )

    workers = [
        threading.Thread(
            target=_worker, args=(w,), name=f"test-worker-{w}", daemon=True
        )
        for w in range(THREADS)
    ]
    try:
        for w in workers:
            w.start()
        for w in workers:
            w.join(timeout=20.0)
            assert not w.is_alive(), "worker hung — likely lost-response regression"

        assert not failures, "concurrent escalate failures: " + "; ".join(failures)
    finally:
        host_stop.set()
        reader.stop()
        for s in (sub_writer, host_writer, sub_reader, host_reader, sub_sock, host_sock):
            try:
                s.close()
            except Exception:
                pass


def test_out_of_order_responses_correlate_correctly():
    """50 requests fired truly concurrently (one per thread), batched
    by the host, then echoed back in reverse order. Any demuxer that
    accidentally relied on FIFO ordering would mismatch tags — this
    test catches that regression.
    """
    sub_reader, sub_writer, host_reader, host_writer, sub_sock, host_sock = _make_pair()
    channel = EscalateChannel(sub_writer)
    lifecycle_queue: "queue.Queue[Dict[str, Any]]" = queue.Queue()
    reader = BridgeReaderThread(sub_reader, channel, lifecycle_queue)
    reader.start()

    REQUESTS = 50

    received: "queue.Queue[Dict[str, Any]]" = queue.Queue()

    def _host_batch() -> None:
        for _ in range(REQUESTS):
            try:
                msg = _read_frame(host_reader)
            except EOFError:
                return
            received.put(msg)

    host_thread = threading.Thread(
        target=_host_batch, name="test-host-batch", daemon=True
    )
    host_thread.start()

    results: Dict[int, str] = {}
    results_lock = threading.Lock()
    failures: List[str] = []
    failures_lock = threading.Lock()
    barrier = threading.Barrier(REQUESTS)

    def _worker(worker_id: int) -> None:
        # Synchronise so all REQUESTS calls go on the wire concurrently.
        barrier.wait()
        try:
            response = channel.request(
                {"op": "test_oo", "_worker_id": worker_id}
            )
        except EscalateError as e:
            with failures_lock:
                failures.append(f"worker {worker_id}: {e}")
            return
        assert response is not None
        with results_lock:
            results[worker_id] = response["echo_tag"]

    workers = [
        threading.Thread(
            target=_worker, args=(w,), name=f"test-oo-worker-{w}", daemon=True
        )
        for w in range(REQUESTS)
    ]
    try:
        for w in workers:
            w.start()

        # Collect the batch the host saw, then echo in REVERSE order.
        collected: List[Dict[str, Any]] = []
        for _ in range(REQUESTS):
            collected.append(received.get(timeout=10.0))
        for msg in reversed(collected):
            request_id = msg["request_id"]
            worker_id = msg["_worker_id"]
            _send_frame(
                host_writer,
                {
                    "rpc": "escalate_response",
                    "request_id": request_id,
                    "result": "ok",
                    "echo_tag": f"w{worker_id}",
                },
            )

        for w in workers:
            w.join(timeout=10.0)
            assert not w.is_alive(), "worker hung — likely lost-response regression"

        assert not failures, "out-of-order escalate failures: " + "; ".join(failures)
        assert len(results) == REQUESTS
        for w_id, tag in results.items():
            assert tag == f"w{w_id}", f"cross-talk: worker {w_id} got tag {tag!r}"
    finally:
        reader.stop()
        for s in (sub_writer, host_writer, sub_reader, host_reader, sub_sock, host_sock):
            try:
                s.close()
            except Exception:
                pass


def test_lifecycle_messages_routed_to_queue_not_request():
    """Lifecycle frames the host sends *between* escalate responses must
    never surface from `request()`. They go to the lifecycle queue."""
    sub_reader, sub_writer, host_reader, host_writer, sub_sock, host_sock = _make_pair()
    channel = EscalateChannel(sub_writer)
    lifecycle_queue: "queue.Queue[Dict[str, Any]]" = queue.Queue()
    reader = BridgeReaderThread(sub_reader, channel, lifecycle_queue)
    reader.start()

    request_done = threading.Event()
    request_result: List[Any] = []

    def _worker() -> None:
        try:
            request_result.append(channel.request({"op": "ping"}))
        except Exception as e:
            request_result.append(e)
        finally:
            request_done.set()

    threading.Thread(target=_worker, name="test-ping-worker", daemon=True).start()

    # Wait for the request to actually arrive on the host side.
    msg = _read_frame(host_reader)
    request_id = msg["request_id"]

    # Inject a lifecycle frame before the escalate response to prove the
    # demuxer routes the lifecycle into the queue and the request keeps
    # waiting on its own slot.
    _send_frame(
        host_writer,
        {"cmd": "on_pause", "capability": "limited"},
    )
    # Give the reader a chance to deliver the lifecycle without racing
    # the response.
    time.sleep(0.05)

    _send_frame(
        host_writer,
        {
            "rpc": "escalate_response",
            "request_id": request_id,
            "result": "ok",
        },
    )

    try:
        assert request_done.wait(timeout=5.0), "request() never returned"
        assert len(request_result) == 1
        assert not isinstance(request_result[0], Exception)

        # Lifecycle queue should hold exactly the on_pause we injected.
        try:
            queued = lifecycle_queue.get(timeout=2.0)
        except queue.Empty:
            pytest.fail("lifecycle frame was lost — should have been routed to queue")
        assert queued.get("cmd") == "on_pause"
    finally:
        reader.stop()
        for s in (sub_writer, host_writer, sub_reader, host_reader, sub_sock, host_sock):
            try:
                s.close()
            except Exception:
                pass


def test_send_failure_surfaces_as_escalate_error():
    """If `bridge_send_message` raises (broken pipe, fd revoked, etc.),
    `request()` must convert the OSError into an EscalateError so callers
    see a single uniform exception type for every channel failure."""

    class _BrokenWriter:
        """Stream stub whose write() raises BrokenPipeError on first call."""

        def write(self, _b: bytes) -> int:
            raise BrokenPipeError("simulated broken pipe")

        def flush(self) -> None:
            pass

    channel = EscalateChannel(_BrokenWriter())
    raised: list[Exception] = []
    try:
        channel.request({"op": "noop"})
    except Exception as e:  # noqa: BLE001 — we want the type comparison
        raised.append(e)

    assert len(raised) == 1
    assert isinstance(raised[0], EscalateError), (
        f"expected EscalateError, got {type(raised[0]).__name__}: {raised[0]}"
    )
    assert "send failed" in str(raised[0])


def test_request_timeout_surfaces_as_escalate_error():
    """If no response arrives within `timeout_s`, the request raises
    EscalateError with a clear timeout message rather than hanging."""
    sub_reader, sub_writer, host_reader, host_writer, sub_sock, host_sock = _make_pair()
    channel = EscalateChannel(sub_writer)
    lifecycle_queue: "queue.Queue[Dict[str, Any]]" = queue.Queue()
    reader = BridgeReaderThread(sub_reader, channel, lifecycle_queue)
    reader.start()

    raised: list[Exception] = []
    started = threading.Event()
    finished = threading.Event()

    def _worker() -> None:
        started.set()
        try:
            channel.request({"op": "blocked"}, timeout_s=0.2)
        except Exception as e:  # noqa: BLE001
            raised.append(e)
        finally:
            finished.set()

    threading.Thread(target=_worker, name="test-timeout-worker", daemon=True).start()

    try:
        # Drain the request frame on the host side without responding.
        # The worker should hit the 200ms timeout.
        assert started.wait(timeout=2.0)
        _ = _read_frame(host_reader)
        assert finished.wait(timeout=5.0), "request() did not honor timeout"
        assert len(raised) == 1
        assert isinstance(raised[0], EscalateError)
        assert "timed out" in str(raised[0])
    finally:
        reader.stop()
        for s in (sub_writer, host_writer, sub_reader, host_reader, sub_sock, host_sock):
            try:
                s.close()
            except Exception:
                pass


def test_late_response_after_timeout_does_not_lose_data():
    """Race-safety: if a response arrives between `event.wait(timeout)`
    returning False and the slot pop, `slot.message` is still populated.
    The current request will see the timeout error, but the next
    request's slot must not be corrupted by the orphaned message — the
    reader's `deliver_response` looks the slot up under the lock and
    returns False if the entry is gone."""
    sub_reader, sub_writer, host_reader, host_writer, sub_sock, host_sock = _make_pair()
    channel = EscalateChannel(sub_writer)
    lifecycle_queue: "queue.Queue[Dict[str, Any]]" = queue.Queue()
    reader = BridgeReaderThread(sub_reader, channel, lifecycle_queue)
    reader.start()

    try:
        # First request times out before the host responds.
        first_raised: list[Exception] = []
        try:
            channel.request({"op": "first"}, timeout_s=0.1)
        except Exception as e:  # noqa: BLE001
            first_raised.append(e)
        assert len(first_raised) == 1
        assert isinstance(first_raised[0], EscalateError)

        # Host now responds to the first request — orphaned, dropped.
        first_msg = _read_frame(host_reader)
        _send_frame(
            host_writer,
            {
                "rpc": "escalate_response",
                "request_id": first_msg["request_id"],
                "result": "ok",
                "stale": True,
            },
        )

        # Second request should NOT see the orphaned response from the
        # first — its own slot is registered under a fresh request_id
        # and the orphan goes through deliver_response → returns False
        # (no slot found) → logged + dropped.
        captured: dict[str, Any] = {}
        done = threading.Event()

        def _second() -> None:
            captured["response"] = channel.request({"op": "second"})
            done.set()

        threading.Thread(target=_second, daemon=True).start()
        second_msg = _read_frame(host_reader)
        _send_frame(
            host_writer,
            {
                "rpc": "escalate_response",
                "request_id": second_msg["request_id"],
                "result": "ok",
                "second": True,
            },
        )
        assert done.wait(timeout=5.0)
        # The second request's response is its own — not the stale
        # first response.
        assert captured["response"] is not None
        assert captured["response"].get("second") is True
        assert "stale" not in captured["response"]
    finally:
        reader.stop()
        for s in (sub_writer, host_writer, sub_reader, host_reader, sub_sock, host_sock):
            try:
                s.close()
            except Exception:
                pass


def test_eof_wakes_in_flight_requests_with_error():
    """If the host closes the escalate fd while a request is waiting, the
    request must wake with [`EscalateError`] rather than hang."""
    sub_reader, sub_writer, host_reader, host_writer, sub_sock, host_sock = _make_pair()
    channel = EscalateChannel(sub_writer)
    lifecycle_queue: "queue.Queue[Dict[str, Any]]" = queue.Queue()
    reader = BridgeReaderThread(sub_reader, channel, lifecycle_queue)
    reader.start()

    raised: List[Any] = []
    done = threading.Event()

    def _worker() -> None:
        try:
            channel.request({"op": "stranded"})
        except EscalateError as e:
            raised.append(e)
        finally:
            done.set()

    threading.Thread(target=_worker, name="test-eof-worker", daemon=True).start()

    # Wait for the request to land on the host side, then close the host
    # end without responding. The reader should hit EOF and call
    # `channel.close()`, which signals the in-flight slot.
    _ = _read_frame(host_reader)
    # `socketpair`'s peer doesn't see EOF until every fd referencing the
    # socket is gone — `makefile("rb", buffering=0)` and
    # `makefile("wb", buffering=0)` each keep a reference, so we have
    # to close all of them plus the underlying socket. A SHUT_RDWR on
    # the socket itself forces the EOF propagation regardless.
    host_writer.close()
    host_reader.close()
    try:
        host_sock.shutdown(socket.SHUT_RDWR)
    except OSError:
        pass
    host_sock.close()

    try:
        assert done.wait(timeout=5.0), "request() did not wake on EOF"
        assert len(raised) == 1
        assert isinstance(raised[0], EscalateError)
    finally:
        reader.stop()
        for s in (sub_writer, sub_reader, sub_sock):
            try:
                s.close()
            except Exception:
                pass
