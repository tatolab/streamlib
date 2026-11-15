# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The helper's end of the escalate socket to its parent process."""

from __future__ import annotations

import json
import os
import queue
import socket
import struct
import threading
import uuid
from typing import Any, Optional

from tatolab.stream import (
    log,
)
from tatolab.runtime._helper_process_environment_variables import ESCALATE_FD_ENV



# Upper bound on how long an escalate request waits for its correlated
# response. Generous enough for a cold GPU allocation under load; bounded so
# a wedged parent surfaces as an error instead of a hung processor callback.
ESCALATE_REQUEST_TIMEOUT_SECONDS = 60.0

# What an answer to each escalate op creates in the app process, as the op that
# releases it and the field that op names it by. Wire contract: an answer this
# helper stopped waiting on is released through it, because nothing else would
# hand it back before the helper stops.
RELEASE_OWED_BY_AN_ANSWER_TO_ESCALATE_OP = {
    "acquire_pixel_buffer": ("release_handle", "handle_id"),
    "acquire_texture": ("release_handle", "handle_id"),
    "acquire_storage_buffer": ("release_handle", "handle_id"),
    "register_acceleration_structure_blas": ("release_handle", "handle_id"),
    "register_acceleration_structure_tlas": ("release_handle", "handle_id"),
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

    def parent_channel_fd(self) -> int:
        """The socket to the parent, for the watch that ends it when the parent
        goes away."""
        return self._socket.fileno()

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
