# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
# streamlib:lint-logging:allow-file — bootstrap; the pre-install fatals below
# are written to raw stderr because the log channel does not exist yet.

"""The helper process one Python processor runs in.

Every `@processor` class runs here — its own interpreter, its own GIL, one
processor per process. The parent execs `sys.executable -m streamlib._helper`;
this module imports the class by the import path the parent derived from it,
opens that processor's own iceoryx2 ports from the wiring the parent sends,
and drives its lifecycle.

Startup order is load-bearing: the escalate socket comes up first so logging
has somewhere to go, and the user's module is imported last so anything it
raises is already reportable. Fatals before the channel exists go to raw
stderr, which the parent captures off fd2.
"""

from __future__ import annotations

import importlib
import json
import os
import queue
import select
import socket
import struct
import sys
import threading
import traceback
import uuid
from datetime import datetime, timezone
from typing import Any, Optional

from . import log
from ._capability_extensions import (
    load_installed_capability_extensions_once_per_process,
)
from ._engine import (
    MonotonicTimer,
    ProcessorLinkDataAccess,
    RuntimeContextFullAccess,
    capability_extension_host_for_the_helper_process,
    capture_this_helper_processes_engine_log_records,
    drain_the_engine_log_records_this_helper_captured,
    engine_build_id_compiled_into_this_extension,
)
from ._processor_hosting import apply_configuration, construct_processor_instance

ENTRYPOINT_ENV = "STREAMLIB_ENTRYPOINT"
PROCESSOR_ID_ENV = "STREAMLIB_PROCESSOR_ID"
RUNTIME_ID_ENV = "STREAMLIB_RUNTIME_ID"
ESCALATE_FD_ENV = "STREAMLIB_ESCALATE_FD"
ENGINE_BUILD_ID_ENV = "STREAMLIB_ENGINE_BUILD_ID"

# Upper bound on how long an escalate request waits for its correlated
# response. Generous enough for a cold GPU allocation under load; bounded so
# a wedged parent surfaces as an error instead of a hung processor callback.
ESCALATE_REQUEST_TIMEOUT_SECONDS = 60.0

# How long a continuous processor's interval wait may park before the lifecycle
# queue is drained again. Teardown latency there is bounded by this plus one
# callback.
LIFECYCLE_POLL_INTERVAL_MILLISECONDS = 100

# The interval a continuous processor declaring none runs at: the same 100 µs
# the engine's native continuous runner waits between calls.
CONTINUOUS_INTERVAL_FLOOR_NANOSECONDS = 100_000

# How long the engine-log forwarder parks waiting for a record before it looks
# at its own stop flag again. It bounds nothing a user sees: a record arriving
# wakes the wait at once.
SECONDS_THE_ENGINE_LOG_FORWARDER_WAITS_FOR_A_RECORD = 0.25

# How long the forwarder is given to make its last pass at shutdown. A
# forwarder still going then is wedged in a write to a parent that has stopped
# reading, which no second sender would get past either, so what it was
# carrying is lost with it rather than holding up the helper's exit.
SECONDS_TO_STOP_THE_ENGINE_LOG_FORWARDER = 1.0

# How long `teardown` waits, before answering, for the releases already queued to
# reach the parent. The parent gives a helper up the moment it answers, and a
# release still queued then is never taken; bounded well inside the shutdown
# ladder's five-second teardown budget.
RELEASES_SENT_BEFORE_TEARDOWN_ANSWERS_TIMEOUT_SECONDS = 1.0

# What an answer to each escalate op creates in the app process, as the op that
# releases it and the field that op names it by. Wire contract: an answer this
# helper stopped waiting on is released through it, because nothing else would
# hand it back before the helper stops.
RELEASE_OWED_BY_AN_ANSWER_TO_ESCALATE_OP = {
    "acquire_pixel_buffer": ("release_handle", "handle_id"),
    "acquire_texture": ("release_handle", "handle_id"),
    "register_acceleration_structure_blas": ("release_handle", "handle_id"),
    "register_acceleration_structure_tlas": ("release_handle", "handle_id"),
    "create_processor_owned_window": ("close_processor_owned_window", "window_id"),
}


class HelperProcessProtocolError(Exception):
    """The parent sent something this helper cannot act on."""


class EscalateRequestError(RuntimeError):
    """An escalate request to the parent failed — send, timeout, or refusal."""


class _PendingEscalateResponse:
    """One slot per in-flight escalate request — an event and its landing pad."""

    __slots__ = ("arrived", "message")

    def __init__(self) -> None:
        self.arrived = threading.Event()
        self.message: "Optional[dict[str, Any]]" = None


# =============================================================================
# The framed socket to the parent
# =============================================================================


class ParentProcessBridge:
    """The one socket a helper process talks to its parent over.

    Lifecycle commands and escalate traffic share it in both directions, so a
    frame is classified on its `rpc` tag and never on whether a reply came
    back — fire-and-forget ops (logging) produce none, and treating that as
    "not escalate" would push every log record into the lifecycle queue and
    break the setup handshake.

    A single reader thread owns the read half, routing `escalate_response`
    frames to their waiting caller by `request_id` and feeding everything
    else to the lifecycle queue the main thread drains. Requests are safe
    from any thread, concurrently — each waits on its own slot, so callers
    cannot steal each other's responses.

    Every command queued also makes a pipe readable, so a loop parked on a
    file descriptor wakes for a command the moment it arrives.

    A release worker sends every release that must not wait on its answer where
    it was asked for — what a freed object owes, and what an answer nobody was
    still waiting on created — one round trip at a time.
    """

    _FRAME_LENGTH_PREFIX = struct.Struct(">I")

    def __init__(self, parent_socket: socket.socket) -> None:
        self._socket = parent_socket
        self._read_stream = parent_socket.makefile("rb", buffering=0)
        self._write_stream = parent_socket.makefile("wb", buffering=0)
        self._write_lock = threading.Lock()
        self._lifecycle_commands: "queue.Queue[Optional[dict[str, Any]]]" = queue.Queue()
        self._pending_escalate_responses: "dict[str, _PendingEscalateResponse]" = {}
        # The op of each request whose caller stopped waiting before its answer
        # arrived, for the ops an answer creates something for.
        self._abandoned_escalate_ops_by_request_id: "dict[str, str]" = {}
        self._pending_lock = threading.Lock()
        # `SimpleQueue`, never `Queue`: its `put` is safe from a finalizer that
        # interrupts another `put` on the same thread. An `Event` is set once the
        # worker reaches it; `None` ends the worker.
        self._releases_owed_to_the_parent: (
            "queue.SimpleQueue[dict[str, Any] | threading.Event | None]"
        ) = queue.SimpleQueue()
        self._channel_closed = False
        self._the_parent_is_gone = threading.Event()
        # Non-inheritable by default, so nothing the processor starts holds it.
        (
            self._lifecycle_command_arrival_read_fd,
            self._lifecycle_command_arrival_write_fd,
        ) = os.pipe()
        os.set_blocking(self._lifecycle_command_arrival_read_fd, False)
        os.set_blocking(self._lifecycle_command_arrival_write_fd, False)
        self._reader = threading.Thread(
            target=self._demultiplex_frames_from_parent,
            name="streamlib-parent-bridge",
            daemon=True,
        )
        self._release_worker = threading.Thread(
            target=self._send_every_release_owed_to_the_parent,
            name="streamlib-parent-release",
            daemon=True,
        )

    @classmethod
    def open_from_inherited_fd(cls) -> "ParentProcessBridge":
        """Wrap the socketpair end the parent set [`ESCALATE_FD_ENV`] to."""
        raw_fd = os.environ.get(ESCALATE_FD_ENV)
        if not raw_fd:
            raise HelperProcessProtocolError(
                f"{ESCALATE_FD_ENV} is not set, so this helper has no channel to its "
                f"parent; it is only ever started by the engine's spawn host"
            )
        try:
            inherited_fd = int(raw_fd)
        except ValueError as unparseable:
            raise HelperProcessProtocolError(
                f"{ESCALATE_FD_ENV} must be a file-descriptor number, got {raw_fd!r}"
            ) from unparseable
        # The parent cleared `FD_CLOEXEC` on this fd to hand it over and
        # `socket.socket(fileno=)` leaves the flag as it found it, so anything
        # this helper starts — `os.system`, `posix_spawn`, a fork worker —
        # would inherit the channel every privileged operation rides.
        os.set_inheritable(inherited_fd, False)
        return cls(socket.socket(fileno=inherited_fd))

    def start_reading(self) -> None:
        """Start the reader and the release worker."""
        self._release_worker.start()
        self._reader.start()

    def send(self, message: "dict[str, Any]") -> None:
        """Write one length-prefixed frame, whole.

        A parent that has already closed its end is not an error to report —
        there is nowhere left to report it. Teardown races here by design: the
        parent drops the bridge as soon as it has the reply it waited for, and
        a child still on its way out may have a record or a `done` in hand.
        """
        payload = _encode_frame_payload(message)
        with self._write_lock:
            try:
                self._write_stream.write(self._FRAME_LENGTH_PREFIX.pack(len(payload)))
                self._write_stream.write(payload)
                self._write_stream.flush()
            except OSError:
                pass

    def request_from_parent(
        self,
        op: "dict[str, Any]",
        *,
        timeout_seconds: float = ESCALATE_REQUEST_TIMEOUT_SECONDS,
    ) -> "dict[str, Any]":
        """Send one escalate request and block until its correlated response.

        Raises [`EscalateRequestError`] on a timeout, a channel that closed
        mid-flight, or a refusal the parent reported — one exception type,
        so a caller never has to tell an OS-level failure from a semantic
        one. A broken write surfaces the same way: `send` never raises, but
        the reader sees the socket's EOF and fails every in-flight request.
        """
        request_id = str(uuid.uuid4())
        slot = _PendingEscalateResponse()
        with self._pending_lock:
            if self._channel_closed:
                raise EscalateRequestError("the channel to the parent is closed")
            self._pending_escalate_responses[request_id] = slot
        try:
            self.send({"rpc": "escalate_request", "request_id": request_id, **op})
            arrived = slot.arrived.wait(timeout=timeout_seconds)
        finally:
            # Read under the lock the reader delivers under, so an answer is
            # either in hand here or finds no slot and is released by the
            # reader — never both, and never neither. Whatever ended the wait,
            # a timeout or an interrupt, abandons the answer the same way.
            with self._pending_lock:
                self._pending_escalate_responses.pop(request_id, None)
                response = slot.message
                if (
                    response is None
                    and not self._channel_closed
                    and op.get("op") in RELEASE_OWED_BY_AN_ANSWER_TO_ESCALATE_OP
                ):
                    self._abandoned_escalate_ops_by_request_id[request_id] = op["op"]
        if response is None:
            if not arrived:
                raise EscalateRequestError(
                    f"the parent did not answer {op.get('op')!r} within "
                    f"{timeout_seconds}s"
                )
            raise EscalateRequestError(
                "the channel to the parent closed before the response arrived"
            )
        if response.get("result") == "ok":
            return response
        raise EscalateRequestError(
            response.get("message") or f"the parent refused {op.get('op')!r}"
        )

    def release_to_parent_without_waiting(self, release_op: "dict[str, Any]") -> None:
        """Queue one release escalate for the release worker and return at once.

        The door a release owed by a freed object takes. That object can be
        freed by a garbage-collector finalizer on any thread, this bridge's
        reader included — and a round trip there would wait out its whole
        timeout, because only the reader delivers the answer.
        """
        self._releases_owed_to_the_parent.put(release_op)

    def wait_for_every_release_queued_so_far(self, timeout_seconds: float) -> bool:
        """Block until the release worker has sent every release queued before
        this call, or `timeout_seconds` pass. `False` when some may not have."""
        if not self._release_worker.is_alive():
            return False
        every_earlier_release_sent = threading.Event()
        self._releases_owed_to_the_parent.put(every_earlier_release_sent)
        return every_earlier_release_sent.wait(timeout=timeout_seconds)

    def _send_every_release_owed_to_the_parent(self) -> None:
        while True:
            release_op = self._releases_owed_to_the_parent.get()
            if release_op is None:
                return
            if isinstance(release_op, threading.Event):
                release_op.set()
                continue
            try:
                self.request_from_parent(release_op)
            except Exception as release_failure:
                # Any failure, not only a refused escalate: a worker that died
                # here would leave every later release queued behind it.
                log.warn(
                    "the parent did not take a release from this helper; what it "
                    "names may stay allocated until the runtime stops",
                    op=release_op.get("op"),
                    error=str(release_failure),
                )

    def lifecycle_command_arrival_fd(self) -> int:
        """A descriptor that is readable once a command has been queued.

        Readable is a hint, not a count: it can stay readable after the command
        that made it so was already taken, so a waiter that wakes clears it with
        [`clear_lifecycle_command_arrivals`] and then drains the queue, in that
        order.
        """
        return self._lifecycle_command_arrival_read_fd

    def clear_lifecycle_command_arrivals(self) -> None:
        """Empty the arrival pipe, ahead of draining the queue it signals."""
        while True:
            try:
                if not os.read(self._lifecycle_command_arrival_read_fd, 4096):
                    return
            except BlockingIOError:
                return

    def next_lifecycle_command(self) -> "Optional[dict[str, Any]]":
        """Block until the parent sends one, or `None` once it is gone."""
        if not self._the_parent_is_gone.is_set():
            return self._lifecycle_commands.get()
        # Drained before the latch is believed: the reader sets it before it
        # queues the end of the channel, so commands the parent sent before it
        # let go — a `stop` and a `teardown` it wrote on its way out — are
        # still waiting here and are still owed.
        try:
            return self._lifecycle_commands.get_nowait()
        except queue.Empty:
            return None

    def next_lifecycle_command_if_waiting(self) -> "tuple[bool, Optional[dict[str, Any]]]":
        """`(True, command)` when one was queued, `(False, None)` otherwise.

        The `None` a closed channel puts on the queue is a command too — the
        first element distinguishes "nothing queued" from "the parent is gone".
        """
        try:
            return True, self._lifecycle_commands.get_nowait()
        except queue.Empty:
            return (True, None) if self._the_parent_is_gone.is_set() else (False, None)

    def _demultiplex_frames_from_parent(self) -> None:
        while True:
            frame = self._read_next_frame()
            if frame is None:
                self._wake_every_pending_escalate_caller()
                self._releases_owed_to_the_parent.put(None)
                # Latched rather than queued once: a mid-run drain consumes
                # whatever is on the queue, and a single end-of-channel
                # consumed there would leave the outer read blocked on a
                # queue no writer is left to fill.
                self._the_parent_is_gone.set()
                self._queue_lifecycle_command(None)
                return
            if frame.get("rpc") == "escalate_response":
                # Never forwarded to the lifecycle queue: it would be read as
                # the answer to whatever command is in flight.
                if not self._deliver_escalate_response(frame):
                    log.warn(
                        "the parent answered an escalate request this helper "
                        "is no longer waiting on",
                        request_id=frame.get("request_id"),
                    )
                continue
            self._queue_lifecycle_command(frame)

    def _queue_lifecycle_command(self, command: "Optional[dict[str, Any]]") -> None:
        # Queued before the pipe is written: a waiter clears the pipe and then
        # drains the queue, so a byte written first could be cleared by a drain
        # that ran before the command was there to take.
        self._lifecycle_commands.put(command)
        try:
            os.write(self._lifecycle_command_arrival_write_fd, b"\x01")
        except BlockingIOError:
            # A full pipe is already readable, which is all the byte says.
            pass

    def _deliver_escalate_response(self, response: "dict[str, Any]") -> bool:
        request_id = response.get("request_id")
        if not isinstance(request_id, str):
            return False
        with self._pending_lock:
            slot = self._pending_escalate_responses.get(request_id)
            if slot is not None:
                slot.message = response
                slot.arrived.set()
                return True
            abandoned_op = self._abandoned_escalate_ops_by_request_id.pop(request_id, None)
        if abandoned_op is None:
            return False
        self._release_what_an_abandoned_answer_created(abandoned_op, response)
        return True

    def _release_what_an_abandoned_answer_created(
        self, abandoned_op: str, response: "dict[str, Any]"
    ) -> None:
        created_id = response.get("handle_id")
        if response.get("result") != "ok" or not isinstance(created_id, str) or not created_id:
            return
        release_op, released_id_field = RELEASE_OWED_BY_AN_ANSWER_TO_ESCALATE_OP[abandoned_op]
        log.warn(
            "the parent answered an escalate request after this helper stopped "
            "waiting on it; releasing what the answer created",
            op=abandoned_op,
            request_id=response.get("request_id"),
        )
        self.release_to_parent_without_waiting(
            {"op": release_op, released_id_field: created_id}
        )

    def _wake_every_pending_escalate_caller(self) -> None:
        """A closed channel fails every in-flight request rather than hanging it."""
        with self._pending_lock:
            self._channel_closed = True
            orphaned = list(self._pending_escalate_responses.values())
            self._pending_escalate_responses.clear()
            self._abandoned_escalate_ops_by_request_id.clear()
        # An answer already delivered is left in its slot: its caller has not
        # read it yet, and what it created is released through that caller.
        for slot in orphaned:
            slot.arrived.set()

    def _read_next_frame(self) -> "Optional[dict[str, Any]]":
        length_prefix = self._read_exactly(self._FRAME_LENGTH_PREFIX.size)
        if length_prefix is None:
            return None
        (payload_length,) = self._FRAME_LENGTH_PREFIX.unpack(length_prefix)
        payload = self._read_exactly(payload_length)
        if payload is None:
            return None
        return _decode_frame_payload(payload)

    def _read_exactly(self, byte_count: int) -> "Optional[bytes]":
        collected = bytearray()
        while len(collected) < byte_count:
            try:
                chunk = self._read_stream.read(byte_count - len(collected))
            except OSError:
                return None
            if not chunk:
                return None
            collected.extend(chunk)
        return bytes(collected)


def _encode_frame_payload(message: "dict[str, Any]") -> bytes:
    return json.dumps(message, separators=(",", ":")).encode("utf-8")


def _decode_frame_payload(payload: bytes) -> "dict[str, Any]":
    return json.loads(payload.decode("utf-8"))


# =============================================================================
# Logging
# =============================================================================


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
            # record fills are absent from every `streamlib.log` document,
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


# =============================================================================
# Loading the processor class
# =============================================================================


def load_processor_class(import_path: str) -> type:
    """Import the class `import_path` names, as `module:qualname`.

    The qualname is walked attribute by attribute, so a class nested inside
    another resolves — `rt.add` deliberately admits `Outer.Inner`, because a
    fresh interpreter can reach it.
    """
    module_name, _, qualname = import_path.partition(":")
    if not module_name or not qualname:
        raise HelperProcessProtocolError(
            f"{ENTRYPOINT_ENV} must be `module:qualname`, got {import_path!r}"
        )
    resolved: Any = importlib.import_module(module_name)
    walked = module_name
    for attribute_name in qualname.split("."):
        try:
            resolved = getattr(resolved, attribute_name)
        except AttributeError as missing:
            raise HelperProcessProtocolError(
                f"{import_path!r} does not resolve: {walked} has no attribute "
                f"{attribute_name!r}"
            ) from missing
        walked = f"{walked}.{attribute_name}"
    return resolved


# =============================================================================
# Opening this processor's own ports
# =============================================================================


def wire_link_data_access(
    link_data_access: ProcessorLinkDataAccess, port_wiring: "dict[str, Any]"
) -> None:
    """Open this processor's publishers and subscribers, one call per link.

    Inputs are wired before outputs so a helper that is both a source and a
    destination is ready to receive before it can publish.
    """
    for input_link in port_wiring.get("inputs", []):
        link_data_access.wire_input_link(
            input_link["name"],
            input_link["channel_service_name"],
            # Not derived from the channel: a link carrying from another
            # runtime rides a channel hashed from the source port's mesh
            # address, and the address is what a read hands back.
            input_link["inbound_link_name"],
            input_link["stamp_clock"],
            input_link["notify_service_name"],
            input_link["read_mode"],
            input_link["channel_service_creation_depth"],
            input_link["input_port_ring_depth"],
            input_link["max_subscribers"],
            input_link["notify_max_notifiers"],
            input_link["link_id"],
            # Absent on every port that declares no window contract, which is
            # unchanged in every respect. Present, it is the values already
            # resolved: a `match_device` sentinel settles in the parent, which is
            # where the device stream is.
            input_link.get("audio_window"),
            # Read with `get` so a helper with its board open names the key the
            # binding was not given, rather than raising a bare `KeyError`.
            loss_count_slot=input_link.get("loss_count_slot"),
            wiring_generation=input_link.get("wiring_generation"),
        )
    for output_link in port_wiring.get("outputs", []):
        link_data_access.wire_output_link(
            output_link["name"],
            output_link["channel_service_name"],
            output_link["dest_notify_service_name"],
            output_link["expected_payload_bytes"],
            output_link["max_payload_bytes_per_channel"],
            output_link["channel_service_creation_depth"],
            output_link["max_subscribers"],
            output_link["notify_max_notifiers"],
            output_link["link_id"],
            output_port_wiring_generation=output_link.get("output_port_wiring_generation"),
        )


def unwire_link_data_access(
    link_data_access: ProcessorLinkDataAccess, command: "dict[str, Any]"
) -> None:
    """Release this processor's own port for one link the engine disconnected.

    The engine reclaims what it holds and cannot reach what this process opened
    from the envelope, so it names the local port and direction and this side
    drops it. Left open, the port is still counted against its channel when the
    same link reconnects.
    """
    link_id = command["link_id"]
    direction = command["direction"]
    if direction == "output":
        link_data_access.unwire_output_link(command["port"], link_id)
    elif direction == "input":
        link_data_access.unwire_input_link(link_id)
    else:
        log.warn(
            "the parent asked to unwire a link in an unknown direction",
            direction=direction,
            link_id=link_id,
        )


# =============================================================================
# The lifecycle loop
# =============================================================================


class HostedProcessor:
    """One processor, its contexts, and the hooks it actually defined.

    Which hooks exist is resolved once here rather than rediscovered per tick,
    the same way the parent's host does it.
    """

    LIFECYCLE_HOOKS = (
        "setup",
        "teardown",
        "process",
        "start",
        "stop",
        "on_pause",
        "on_resume",
    )

    def __init__(
        self,
        processor_instance: Any,
        full_access_context: RuntimeContextFullAccess,
        limited_access_context: Any,
    ) -> None:
        self.instance = processor_instance
        self.full_access_context = full_access_context
        self.limited_access_context = limited_access_context
        self._declared_hooks = {
            hook for hook in self.LIFECYCLE_HOOKS if hasattr(processor_instance, hook)
        }

    def call_hook(self, hook_name: str, context: Any) -> None:
        """Call `hook_name` if the class defined one, logging what it raised.

        A hook raising here is not fatal to the helper — the processor keeps
        being driven, exactly as it would in the parent. Nor is the
        `KeyboardInterrupt` the parent's shutdown ladder delivers to a callback
        that outran its budget: the bag in flight is lost and the rungs behind
        it still run.
        """
        if hook_name not in self._declared_hooks:
            return
        try:
            getattr(self.instance, hook_name)(context)
        except KeyboardInterrupt:
            log.warn(
                f"{hook_name}() was interrupted by the engine's shutdown ladder",
                hook=hook_name,
            )
        except Exception as hook_failure:
            log.error(
                f"{hook_name}() raised",
                error=str(hook_failure),
                traceback=traceback.format_exc(),
            )

    def call_hook_letting_failure_propagate(self, hook_name: str, context: Any) -> None:
        """Call `hook_name`, leaving what it raised to the caller.

        `setup` is the one hook whose failure the parent must hear about: it
        is the handshake the parent is blocked on, and a processor that could
        not set itself up must not report ready.
        """
        if hook_name not in self._declared_hooks:
            return
        getattr(self.instance, hook_name)(context)


def construct_hosted_processor(
    processor_class: type,
    configuration: "Optional[dict[str, Any]]",
    link_data_access: ProcessorLinkDataAccess,
    runtime_id: str,
    processor_id: str,
    bridge: ParentProcessBridge,
) -> HostedProcessor:
    """Build the user's object and the two contexts its hooks receive.

    The bridge's escalate round trip is what the GPU surface crosses to the
    parent on — without it, `ctx.gpu_limited_access` refuses by name.
    """
    full_access_context = RuntimeContextFullAccess.open_for_helper_process(
        configuration or {},
        link_data_access,
        runtime_id,
        processor_id,
        bridge.request_from_parent,
        bridge.release_to_parent_without_waiting,
    )
    return HostedProcessor(
        construct_processor_instance(processor_class, configuration, link_data_access),
        full_access_context,
        full_access_context.limited_access_view_for_helper_process(),
    )


class HelperProcessLifecycle:
    """Drives one hosted processor from the commands its parent sends."""

    def __init__(
        self,
        bridge: ParentProcessBridge,
        processor_class: type,
        runtime_id: str,
        processor_id: str,
        link_data_access: ProcessorLinkDataAccess,
    ) -> None:
        self._bridge = bridge
        self._processor_class = processor_class
        self._runtime_id = runtime_id
        self._processor_id = processor_id
        self._link_data_access = link_data_access
        self._hosted: Optional[HostedProcessor] = None
        self._set_up_succeeded = False
        self._running = False
        self._torn_down = False

    def run_until_the_parent_is_done(self) -> None:
        while not self._torn_down:
            try:
                command = self._bridge.next_lifecycle_command()
                if command is None:
                    log.info("the parent closed the channel; shutting down")
                    return
                self._dispatch(command)
            except KeyboardInterrupt:
                # The ladder's interrupt can land anywhere the main thread is,
                # not only inside a hook — a `poll`, a queue wait, a native
                # call returning. Wherever it lands it leaves the execution
                # loop and never the process: `stop` and `teardown` are already
                # queued behind it.
                self._running = False
                log.warn(
                    "the engine's shutdown ladder interrupted this processor; "
                    "the bag in flight is lost"
                )

    def _dispatch(self, command: "dict[str, Any]") -> None:
        verb = command.get("cmd", "")
        if verb == "setup":
            self._setup(command)
        elif verb == "run":
            # Deliberately unanswered: `run` enters the execution loop, and a
            # reply here would be read as the answer to whatever the parent
            # sends next.
            self._run(command)
        elif verb == "stop":
            self._stop()
        elif verb == "teardown":
            self._teardown()
        elif verb in ("on_pause", "on_resume"):
            self._note_pause(verb)
        elif verb == "update_config":
            self._update_config(command)
        elif verb == "unwire_link":
            # Deliberately unanswered, like `run`: the parent sends this from
            # its compiler while it holds the graph write lock, so it cannot
            # wait, and a reply nobody reads becomes the answer to whatever it
            # sends next.
            self._unwire_link(command)
        elif verb == "wire_link":
            # A link the engine handed over after `setup` read the envelope.
            # Answered, unlike `unwire_link`: the parent reports the link wired
            # only once this side says its port is open, and the answer rides
            # its own link-scoped rpc tag so it never becomes the reply to the
            # next lifecycle command.
            self._wire_link(command)
        else:
            log.warn("the parent sent an unknown lifecycle command", cmd=verb)

    def _setup(self, command: "dict[str, Any]") -> None:
        try:
            # Declared ahead of any wiring: a processor added to a running
            # graph has ports before it has links, and reads and writes on
            # them must not raise in between.
            self._link_data_access.declare_ports(
                [
                    port["name"]
                    for port in getattr(
                        self._processor_class, "__streamlib_processor_input_ports__", []
                    )
                ],
                [
                    port["name"]
                    for port in getattr(
                        self._processor_class, "__streamlib_processor_output_ports__", []
                    )
                ],
            )
            # Opened before any link is wired, so every link mirrors its losses
            # from its first. Only a stand-in parent sends no board.
            loss_count_board = command.get("loss_count_board")
            if loss_count_board is not None:
                self._link_data_access.open_loss_count_board(
                    loss_count_board["service_name"], loss_count_board["output_ports"]
                )
            wire_link_data_access(self._link_data_access, command.get("ports") or {})
            self._hosted = construct_hosted_processor(
                self._processor_class,
                command.get("config"),
                self._link_data_access,
                self._runtime_id,
                self._processor_id,
                self._bridge,
            )
            self._hosted.call_hook_letting_failure_propagate(
                "setup", self._hosted.full_access_context
            )
        except KeyboardInterrupt:
            # The ladder interrupted a setup that outran its budget. The parent
            # hears a refusal rather than nothing, and this helper stays up to
            # take the `teardown` the same ladder sends next.
            self._bridge.send(
                {
                    "rpc": "error",
                    "error": "setup() was interrupted by the engine's shutdown ladder",
                }
            )
            return
        except Exception as setup_failure:
            self._bridge.send(
                {
                    "rpc": "error",
                    "error": f"{setup_failure}\n{traceback.format_exc()}",
                }
            )
            return
        self._set_up_succeeded = True
        self._bridge.send({"rpc": "ready"})

    def _run(self, command: "dict[str, Any]") -> None:
        if self._hosted is None:
            log.warn("the parent sent `run` before `setup`")
            return
        execution_mode = command.get("execution", "reactive")
        self._running = True
        if execution_mode == "reactive":
            self._run_reactive()
        elif execution_mode == "continuous":
            self._run_continuous(int(command.get("interval_ms") or 0))
        elif execution_mode == "manual":
            self._hosted.call_hook("start", self._hosted.full_access_context)
        else:
            log.warn("the parent named an unknown execution mode", mode=execution_mode)

    def _run_reactive(self) -> None:
        assert self._hosted is not None
        while self._running and not self._torn_down:
            if self._link_data_access.any_input_port_has_data():
                # Drained on every pass, not only when idle: a processor that
                # runs slower than its upstream never reaches the wait below,
                # and a listener nobody drains fills its socket within seconds,
                # after which every upstream notify fails and is logged.
                self._link_data_access.drain_input_listener()
                self._hosted.call_hook("process", self._hosted.limited_access_context)
                self._drain_commands_arriving_mid_run()
                continue
            # Re-read before every wait, never cached across one: the listener
            # owns this fd, and an `unwire_link` taking this processor's last
            # inbound link drops the listener and closes it. Polling the stale
            # number reports it invalid — or, once the OS recycles it, waits on
            # something else entirely.
            listener_fd = self._link_data_access.input_listener_fd()
            if listener_fd is not None and listener_fd >= 0:
                self._wait_for_a_notify_or_a_command(listener_fd)
            else:
                # No inputs left: nothing will ever wake this loop, so the only
                # thing left to wait on is the parent.
                self._park_until_a_command_arrives()
            self._drain_commands_arriving_mid_run()

    def _run_continuous(self, interval_ms: int) -> None:
        assert self._hosted is not None
        interval_ns = (
            interval_ms * 1_000_000
            if interval_ms > 0
            else CONTINUOUS_INTERVAL_FLOOR_NANOSECONDS
        )
        with MonotonicTimer(interval_ns) as timer:
            # Once at the start and then once per tick, as the native runner
            # paces a continuous processor.
            self._hosted.call_hook("process", self._hosted.limited_access_context)
            self._drain_commands_arriving_mid_run()
            while self._running:
                expirations = timer.wait(LIFECYCLE_POLL_INTERVAL_MILLISECONDS)
                if expirations < 0:
                    log.error("the interval timer failed; leaving the continuous loop")
                    self._running = False
                elif expirations > 0:
                    # Once per wake however many ticks it covered — drift-free
                    # pacing, not backlog replay.
                    self._hosted.call_hook("process", self._hosted.limited_access_context)
                self._drain_commands_arriving_mid_run()

    def _wait_for_a_notify_or_a_command(self, listener_fd: int) -> None:
        """Park until upstream notifies or the parent sends a command.

        `poll`, never `select`: `select` refuses a descriptor of 1024 or above,
        and a live rewire can recreate the listener there. No timeout — only
        this thread replaces the listener, and only by dispatching a command,
        which wakes the wait first.
        """
        command_arrival_fd = self._bridge.lifecycle_command_arrival_fd()
        poller = select.poll()
        poller.register(listener_fd, select.POLLIN)
        poller.register(command_arrival_fd, select.POLLIN)
        for ready_fd, _ in poller.poll():
            if ready_fd == listener_fd:
                self._link_data_access.drain_input_listener()
            elif ready_fd == command_arrival_fd:
                self._bridge.clear_lifecycle_command_arrivals()

    def _park_until_a_command_arrives(self) -> None:
        command = self._bridge.next_lifecycle_command()
        if command is None:
            self._running = False
            return
        self._dispatch_mid_run(command)

    def _drain_commands_arriving_mid_run(self) -> None:
        while self._running:
            was_waiting, command = self._bridge.next_lifecycle_command_if_waiting()
            if not was_waiting:
                return
            if command is None:
                self._running = False
                return
            self._dispatch_mid_run(command)

    def _dispatch_mid_run(self, command: "dict[str, Any]") -> None:
        if command.get("cmd") == "run":
            log.warn("the parent sent `run` while this processor was already running")
            return
        self._dispatch(command)

    def _stop(self) -> None:
        self._running = False
        if self._hosted is not None:
            self._hosted.call_hook("stop", self._hosted.full_access_context)
        self._bridge.send({"rpc": "stopped"})

    def _teardown(self) -> None:
        self._running = False
        self._torn_down = True
        if self._hosted is not None:
            self._hosted.call_hook("teardown", self._hosted.full_access_context)
        try:
            every_release_sent = self._bridge.wait_for_every_release_queued_so_far(
                RELEASES_SENT_BEFORE_TEARDOWN_ANSWERS_TIMEOUT_SECONDS
            )
        except KeyboardInterrupt:
            # The ladder's interrupt ends the wait, never the answer: the
            # parent is still owed `done`.
            every_release_sent = False
        if not every_release_sent:
            log.warn(
                "this helper answered teardown with releases still unsent; what they "
                "name may stay allocated until the runtime stops"
            )
        self._bridge.send({"rpc": "done"})

    def _note_pause(self, verb: str) -> None:
        if self._hosted is not None:
            self._hosted.full_access_context.note_pause_state_from_parent(
                verb == "on_pause"
            )
            self._hosted.call_hook(verb, self._hosted.limited_access_context)
        self._bridge.send({"rpc": "ok"})

    def _unwire_link(self, command: "dict[str, Any]") -> None:
        try:
            unwire_link_data_access(self._link_data_access, command)
        except Exception as unwire_failure:
            # The link is going away either way, and this processor's own
            # callbacks are unaffected — the cost of failing is a port that
            # stays counted against its channel until this process exits.
            log.error(
                "this processor could not release a disconnected link's port",
                link_id=command.get("link_id"),
                error=str(unwire_failure),
            )

    def _wire_link(self, command: "dict[str, Any]") -> None:
        direction = command.get("direction")
        link = command.get("link") or {}
        link_id = link.get("link_id")
        if not self._set_up_succeeded:
            # The engine can hand a link over while `setup` is still running;
            # it reaches this loop only once `setup` has answered, and a
            # processor whose setup failed must not report a port open.
            self._answer_the_parents_wire_link(
                link_id, "this processor's setup did not succeed, so it opens no port"
            )
            return
        if direction == "input":
            port_wiring = {"inputs": [link]}
        elif direction == "output":
            port_wiring = {"outputs": [link]}
        else:
            log.warn(
                "the parent asked to wire a link in an unknown direction",
                direction=direction,
                link_id=link_id,
            )
            self._answer_the_parents_wire_link(link_id, f"unknown link direction {direction!r}")
            return
        try:
            wire_link_data_access(self._link_data_access, port_wiring)
        except Exception as wire_failure:
            log.error(
                "this processor could not open its port for a link wired after setup",
                link_id=link_id,
                direction=direction,
                error=str(wire_failure),
            )
            self._answer_the_parents_wire_link(link_id, str(wire_failure))
            return
        self._answer_the_parents_wire_link(link_id, None)

    def _answer_the_parents_wire_link(self, link_id: "Optional[str]", refusal: "Optional[str]") -> None:
        """Tell the parent whether this processor's port for one link is open.

        Wire contract: the two rpc tags are link-scoped rather than lifecycle
        ones, so the parent's bridge routes an answer to the link it names
        instead of reading it as the reply to whatever command it sends next.
        Only this answer makes the engine report the link `wired`; a refusal
        carries its reason into `graph`, where the caller of a live `connect`
        reads it. A frame naming no link is answered to nothing, so there is
        nothing to send.
        """
        if link_id is None:
            log.warn("the parent asked to wire a link it did not name")
            return
        if refusal is None:
            self._bridge.send({"rpc": "link_wired", "link_id": link_id})
        else:
            self._bridge.send(
                {"rpc": "link_wire_failed", "link_id": link_id, "reason": refusal}
            )

    def _update_config(self, command: "dict[str, Any]") -> None:
        # Answered with the cause rather than logged: the parent keeps the
        # graph's previous configuration only when it hears the refusal.
        if self._hosted is None or not self._set_up_succeeded:
            self._bridge.send(
                {
                    "rpc": "error",
                    "error": "this processor's setup did not succeed, so it takes no "
                    "configuration",
                }
            )
            return
        try:
            apply_configuration(self._hosted.instance, command.get("config"))
        except Exception as configuration_failure:
            self._bridge.send({"rpc": "error", "error": str(configuration_failure)})
            return
        self._bridge.send({"rpc": "ok"})


# =============================================================================
# Entry point
# =============================================================================


def _required_environment(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise HelperProcessProtocolError(
            f"{name} is not set; a helper process is only ever started by the "
            f"engine's spawn host, which always sets it"
        )
    return value


def _refuse_an_engine_built_other_than_the_parents() -> None:
    """Parent and helper import one wheel, so differing ids mean this process
    imported another build — which would otherwise surface as every iceoryx2
    service open failing on a corrupted service."""
    helper_engine_build_id = engine_build_id_compiled_into_this_extension()
    parent_engine_build_id = os.environ.get(ENGINE_BUILD_ID_ENV)
    if not parent_engine_build_id:
        raise HelperProcessProtocolError(
            f"{ENGINE_BUILD_ID_ENV} is not set, so this helper cannot tell whether the "
            f"engine it imported (build {helper_engine_build_id}) is its parent's; a "
            f"helper process is only ever started by the engine's spawn host, which "
            f"always sets it"
        )
    if parent_engine_build_id != helper_engine_build_id:
        raise HelperProcessProtocolError(
            f"this helper imported engine build {helper_engine_build_id} from "
            f"{os.path.dirname(os.path.abspath(__file__))!r}, but its parent is engine "
            f"build {parent_engine_build_id}. Parent and helper must import the same "
            f"streamlib wheel; a different streamlib is earlier on this process's "
            f"sys.path, or the wheel was rebuilt under a running app"
        )


def main() -> None:
    """Run one processor until its parent tears it down."""
    try:
        _refuse_an_engine_built_other_than_the_parents()
        import_path = _required_environment(ENTRYPOINT_ENV)
        processor_id = _required_environment(PROCESSOR_ID_ENV)
        runtime_id = os.environ.get(RUNTIME_ID_ENV, "")
        bridge = ParentProcessBridge.open_from_inherited_fd()
    except HelperProcessProtocolError as bootstrap_failure:
        # Pre-install fatal: there is no channel to report it on, so this goes
        # to raw stderr, which the parent captures.
        sys.stderr.write(f"[streamlib] {bootstrap_failure}\n")
        sys.stderr.flush()
        sys.exit(1)

    bridge.start_reading()
    log_sink = ParentProcessLogSink(bridge, processor_id)
    log.install_helper_process_sink(log_sink)

    # Before this helper opens anything: the engine's own records, iceoryx2's
    # included, reach nobody in a child until the capture is up, and what an
    # iceoryx2 node or port refuses on the way up is exactly what explains a
    # helper that never gets further.
    engine_log_forwarder = CapturedEngineLogRecordForwarder(log_sink)
    engine_log_forwarder.capture_and_start()

    # Before the processor's own module is imported: its class may reach for a
    # stack an extension in the same wheel brings up, and a hook that fails
    # here is reportable on the channel the sink above just installed.
    try:
        load_installed_capability_extensions_once_per_process(
            capability_extension_host_for_the_helper_process
        )
    except Exception as extension_failure:
        engine_log_forwarder.stop_after_forwarding_what_is_left()
        log.error(
            "the helper could not load a capability extension",
            entrypoint=import_path,
            error=str(extension_failure),
            traceback=traceback.format_exc(),
        )
        sys.exit(1)

    try:
        processor_class = load_processor_class(import_path)
        link_data_access = ProcessorLinkDataAccess()
    except Exception as startup_failure:
        engine_log_forwarder.stop_after_forwarding_what_is_left()
        log.error(
            "the helper could not load its processor",
            entrypoint=import_path,
            error=str(startup_failure),
            traceback=traceback.format_exc(),
        )
        sys.exit(1)

    log.info("helper process started", entrypoint=import_path, pid=os.getpid())
    HelperProcessLifecycle(
        bridge, processor_class, runtime_id, processor_id, link_data_access
    ).run_until_the_parent_is_done()
    engine_log_forwarder.stop_after_forwarding_what_is_left()
    log.info("helper process exiting")


if __name__ == "__main__":
    main()
