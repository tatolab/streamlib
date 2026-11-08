# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
# streamlib:lint-logging:allow-file — bootstrap; the pre-install fatals below
# are written to raw stderr because the log channel does not exist yet.

"""The bootstrap a processor interpreter runs — one Python processor, one process.

Every `@node.process` class runs in a processor interpreter: the stream's own venv
interpreter running this file by path, with the lend directory (the one holding
`tatolab/runtime/`) and then the project directory on `PYTHONPATH`. It imports
the class by the import path the parent derived from it, opens that processor's
own iceoryx2 ports from the wiring the parent sends, and drives its lifecycle.
Run with `--describe <import path>...`, it instead prints each class's
declaration as one JSON document on stdout and exits; run with
`--describe-installed-modules`, it does the same for every class an installed
distribution declares in the `tatolab.nodes` entry-point group.

Startup order is load-bearing. Up to the lent runtime's import this file uses
the standard library only and parses as Python 3.7, so an interpreter that
cannot load the runtime is refused naming what it is. The escalate socket comes
up next so logging has somewhere to go, and the user's module is imported last
so anything it raises is already reportable. Fatals before the channel exists
go to raw stderr, which the parent captures off fd2.
"""

from __future__ import annotations

import importlib
import json
import os
import platform
import queue
import signal
import socket
import stat
import struct
import sys
import sysconfig
import threading
import time
import traceback
import uuid
from datetime import datetime, timezone
from typing import Any, NoReturn, Optional

OLDEST_PYTHON_THE_LENT_RUNTIME_LOADS_IN = (3, 12)

DESCRIBE_ARGUMENT = "--describe"
DESCRIBE_INSTALLED_MODULES_ARGUMENT = "--describe-installed-modules"
DESCRIBE_ARGUMENTS = (DESCRIBE_ARGUMENT, DESCRIBE_INSTALLED_MODULES_ARGUMENT)

#: The entry-point group an installed distribution declares its node classes in,
#: each entry's value a `module:qualname` import path.
INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP = "tatolab.nodes"

_described_document_fd: Optional[int] = None


def _reserve_stdout_for_the_described_document() -> int:
    """Hand back a duplicate of fd 1 and point fd 1 and `sys.stdout` at stderr,
    so what an import prints cannot corrupt the one JSON document the parent
    parses off stdout. Reserved once; a second call returns the same duplicate."""
    global _described_document_fd
    if _described_document_fd is None:
        if sys.stdout is not None:
            sys.stdout.flush()
        _described_document_fd = os.dup(1)
        os.dup2(2, 1)
        sys.stdout = sys.stderr
    return _described_document_fd


def _this_interpreter_is_free_threaded() -> bool:
    return bool(sysconfig.get_config_var("Py_GIL_DISABLED"))


def _drop_this_bootstraps_own_directory_from_sys_path() -> None:
    """CPython puts the directory of a file run by path first on `sys.path`.
    Here that is the lent `tatolab/runtime/`, whose modules would otherwise
    shadow top-level names the project imports."""
    own_directory = os.path.realpath(os.path.dirname(os.path.abspath(__file__)))
    sys.path[:] = [
        entry
        for entry in sys.path
        if os.path.realpath(entry or os.curdir) != own_directory
    ]


def _refuse_an_interpreter_that_cannot_load_the_lent_runtime(import_error: str) -> NoReturn:
    refusal = (
        "[streamlib] this interpreter cannot load the lent runtime (tatolab.runtime), "
        "so it cannot run a processor:\n"
        f"  interpreter: {sys.executable}\n"
        f"  implementation: {platform.python_implementation()}\n"
        f"  version: {' '.join(sys.version.split())}\n"
        f"  free-threaded: {'yes' if _this_interpreter_is_free_threaded() else 'no'}\n"
        f"  architecture: {platform.machine()}\n"
        f"  import error: {import_error.rstrip()}\n"
    )
    os.write(2, refusal.encode("utf-8", "backslashreplace"))
    sys.exit(1)


def _import_the_lent_runtime_or_refuse() -> None:
    if platform.python_implementation() != "CPython":
        _refuse_an_interpreter_that_cannot_load_the_lent_runtime(
            "not attempted: the runtime is a CPython extension module"
        )
    if sys.version_info < OLDEST_PYTHON_THE_LENT_RUNTIME_LOADS_IN:
        _refuse_an_interpreter_that_cannot_load_the_lent_runtime(
            "not attempted: the runtime needs CPython "
            + ".".join(str(part) for part in OLDEST_PYTHON_THE_LENT_RUNTIME_LOADS_IN)
            + " or newer"
        )
    if _this_interpreter_is_free_threaded():
        _refuse_an_interpreter_that_cannot_load_the_lent_runtime(
            "not attempted: the runtime is built for the GIL-enabled CPython"
        )
    try:
        importlib.import_module("tatolab.runtime._engine")
    except Exception as import_failure:
        _refuse_an_interpreter_that_cannot_load_the_lent_runtime(
            "".join(traceback.format_exception_only(type(import_failure), import_failure))
        )


if __name__ == "__main__":
    _drop_this_bootstraps_own_directory_from_sys_path()
    # Before the lent runtime and `tatolab.stream` import, so nothing their
    # imports print reaches the document; only the interpreter's own site hooks
    # run earlier.
    if sys.argv[1:2] and sys.argv[1] in DESCRIBE_ARGUMENTS:
        _reserve_stdout_for_the_described_document()
    _import_the_lent_runtime_or_refuse()

from tatolab.stream import (
    NodeLinkDataAccess,
    RuntimeContextFullAccess,
    log,
)
from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP,
    NODE_DECLARATION_LANGUAGE_STAMP,
    FunctionNode,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_DECLARED_STAMP,
    NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
    NODE_DECLARATION_DESCRIPTION_STAMP,
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_KIND_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
    NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP,
    NODE_DECLARATION_SCHEDULING_PRIORITY_STAMP,
)

from tatolab.runtime._engine import (
    capture_this_helper_processes_engine_log_records,
    drain_the_engine_log_records_this_helper_captured,
    engine_build_id_compiled_into_this_extension,
    limited_access_view_of_runtime_context_for_helper_process,
    note_execution_mode_from_parent_on_runtime_context,
    note_pause_state_from_parent_on_runtime_context,
    note_stop_request_from_parent_on_runtime_context,
    open_node_link_data_access_for_helper_process,
    open_runtime_context_full_access_for_helper_process,
    read_each_unlinked_input_port_as_its_pin_value,
    run_the_scheduling_loop_of_this_helper,
)
from tatolab.runtime._function_node_compilation import (
    FunctionNodeIslandsFedToTheirPorts,
    feed_each_input_port_from_its_function_node_island,
)
from tatolab.runtime._function_node_pool import (
    FunctionNodePool,
    the_processor_class_the_function_node_pool_runs_as,
)
from tatolab.runtime._function_node_run_as_its_helpers_processor import (
    the_processor_class_a_promoted_function_node_runs_as,
)
from tatolab.runtime._processor_hosting import (
    apply_configuration,
    construct_processor_instance,
)

if sys.platform == "darwin":
    from tatolab.runtime._engine import (
        note_this_helper_processes_callbacks_returned_after_its_parent_went_away,
        watch_for_this_helper_processes_parent_going_away,
    )

ENTRYPOINT_ENV = "STREAMLIB_ENTRYPOINT"
PROCESSOR_ID_ENV = "STREAMLIB_PROCESSOR_ID"
RUNTIME_ID_ENV = "STREAMLIB_RUNTIME_ID"
ESCALATE_FD_ENV = "STREAMLIB_ESCALATE_FD"
# The socket the helper's native code escalates over, read by the engine
# module itself; this bootstrap only keeps it out of what the processor starts.
NATIVE_ESCALATE_FD_ENV = "STREAMLIB_NATIVE_ESCALATE_FD"
ENGINE_BUILD_ID_ENV = "STREAMLIB_ENGINE_BUILD_ID"

# Upper bound on how long an escalate request waits for its correlated
# response. Generous enough for a cold GPU allocation under load; bounded so
# a wedged parent surfaces as an error instead of a hung processor callback.
ESCALATE_REQUEST_TIMEOUT_SECONDS = 60.0

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


# =============================================================================
# Logging
# =============================================================================


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


# =============================================================================
# Loading the processor class
# =============================================================================


def load_processor_class(import_path: str) -> type:
    """Import the class `import_path` names, as `module:qualname`.

    The qualname is walked attribute by attribute, so a class nested inside
    another resolves — `stream_builder.add` deliberately admits `Outer.Inner`, because
    a fresh interpreter can reach it.
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
# Describing processor classes
# =============================================================================

#: Each key of a described process node type the class's own `@node.process` stamp fills, in wire order.
DESCRIBED_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS = (
    ("description", NODE_DECLARATION_DESCRIPTION_STAMP),
    ("kind", NODE_DECLARATION_KIND_STAMP),
    ("execution", NODE_DECLARATION_EXECUTION_STAMP),
    ("scheduling_priority", NODE_DECLARATION_SCHEDULING_PRIORITY_STAMP),
    ("paced_by_real_time", NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP),
    ("config_schema", NODE_DECLARATION_CONFIG_SCHEMA_STAMP),
    ("input_ports", NODE_DECLARATION_INPUT_PORTS_STAMP),
    ("output_ports", NODE_DECLARATION_OUTPUT_PORTS_STAMP),
)


#: Each key of a described function node type its `@node.function` stamp fills, in wire order.
DESCRIBED_FUNCTION_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS = (
    ("description", NODE_DECLARATION_DESCRIPTION_STAMP),
    ("kind", NODE_DECLARATION_KIND_STAMP),
    ("language", NODE_DECLARATION_LANGUAGE_STAMP),
    ("config_schema", NODE_DECLARATION_CONFIG_SCHEMA_STAMP),
    ("input_ports", NODE_DECLARATION_INPUT_PORTS_STAMP),
    ("output_ports", NODE_DECLARATION_OUTPUT_PORTS_STAMP),
    ("function_signature", NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP),
    ("state_fields", NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP),
)


class ProcessorClassNotDescribable(Exception):
    """A requested import path names nothing this interpreter can describe."""


def describe_one_processor_class(import_path: str) -> "dict[str, Any]":
    """The declaration the `@node.process` class at `import_path` carries, as JSON data.

    Refused unless `import_path` names exactly that class: one whose own
    `__module__:__qualname__` is the path, since that is the path its processor
    interpreter imports it back by.
    """
    module_name, _, qualname = import_path.partition(":")
    if not module_name or not qualname:
        raise ProcessorClassNotDescribable("an import path is `module:qualname`")
    try:
        importlib.import_module(module_name)
    except (Exception, SystemExit):
        raise ProcessorClassNotDescribable(
            f"its module `{module_name}` did not import:\n{traceback.format_exc()}"
        ) from None
    try:
        return _describe_the_processor_class_its_imported_module_defines(import_path)
    except ProcessorClassNotDescribable:
        raise
    except (Exception, SystemExit):
        raise ProcessorClassNotDescribable(
            f"reading its declaration raised:\n{traceback.format_exc()}"
        ) from None


def _describe_the_processor_class_its_imported_module_defines(
    import_path: str,
) -> "dict[str, Any]":
    try:
        resolved: Any = load_processor_class(import_path)
    except HelperProcessProtocolError as unresolvable:
        raise ProcessorClassNotDescribable(str(unresolvable)) from None
    if isinstance(resolved, FunctionNode):
        described_keys_and_their_stamps = DESCRIBED_FUNCTION_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS
        the_decorator_that_stamps_them = "@node.function"
        what_it_names = "function"
    elif isinstance(resolved, type):
        described_keys_and_their_stamps = DESCRIBED_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS
        the_decorator_that_stamps_them = "@node.process"
        what_it_names = "class"
    else:
        raise ProcessorClassNotDescribable(
            f"it names a {type(resolved).__name__}, not a class or a `@node.function`"
        )
    if getattr(resolved, NODE_DECLARATION_DECLARED_STAMP, False) is not True:
        raise ProcessorClassNotDescribable(
            f"the {what_it_names} `{resolved.__qualname__}` carries no "
            f"`{the_decorator_that_stamps_them}` declaration, so it is not a node"
        )
    own_import_path = (
        f"{getattr(resolved, '__module__', None)}:{getattr(resolved, '__qualname__', None)}"
    )
    if own_import_path != import_path:
        raise ProcessorClassNotDescribable(
            f"the {what_it_names} it names identifies as `{own_import_path}`; a node is named by "
            f"its own import path, the one its processor interpreter imports it back by"
        )
    described_node_type: "dict[str, Any]" = {
        "import_path": import_path,
        "short_name": resolved.__name__,
    }
    for (
        described_node_type_key,
        node_declaration_stamp,
    ) in described_keys_and_their_stamps:
        try:
            described_node_type[described_node_type_key] = getattr(
                resolved, node_declaration_stamp
            )
        except AttributeError:
            raise ProcessorClassNotDescribable(
                f"the {what_it_names} `{resolved.__qualname__}` carries no "
                f"`{node_declaration_stamp}`, which `{the_decorator_that_stamps_them}` sets; a "
                f"{what_it_names} stamped by hand is not a node"
            ) from None
    try:
        json.dumps(described_node_type, allow_nan=False)
    except (TypeError, ValueError) as not_json:
        raise ProcessorClassNotDescribable(
            f"its declaration is not JSON data: {not_json}"
        ) from None
    return described_node_type


def _end_the_describe_without_finalizing_the_interpreter(exit_status: int) -> NoReturn:
    """Exit at once, the document already written.

    Finalizing would join every non-daemon thread and run every atexit handler
    a described module left, and the parent refuses a describe that has not
    exited within its bound however complete its document. The parent kills
    the describe's process group, so nothing the module started outlives it.
    """
    for standard_stream in (sys.__stdout__, sys.stderr):
        if standard_stream is not None:
            standard_stream.flush()
    os._exit(exit_status)


def _end_the_describe_once_its_parent_closes_stdin() -> None:
    """End the describe and its process group when the stdin pipe its parent
    holds open reaches end-of-file: the parent is gone, and on macOS nothing
    else ends a describe whose module blocks at import.

    Armed only on a pipe, so a describe run by hand with stdin on a terminal or
    `/dev/null` is not ended at once. Only a describe leading its own process
    group, as the engine starts it, takes the group down with it.
    """
    try:
        if not stat.S_ISFIFO(os.fstat(0).st_mode):
            return
    except OSError:
        return

    def wait_for_the_end_of_stdin() -> None:
        try:
            while os.read(0, 4096):
                pass
        except OSError:
            return
        if os.getpgrp() == os.getpid():
            os.killpg(os.getpgrp(), signal.SIGKILL)
            # Darwin delivers a SIGKILL sent to the sender's own group after
            # killpg returns, where Linux delivers it before; an exit here would
            # race it, so wait for it to land.
            time.sleep(5)
        os._exit(1)

    threading.Thread(
        target=wait_for_the_end_of_stdin, name="describe-parent-watch", daemon=True
    ).start()


def describe_processor_classes_onto_stdout(import_paths: "list[str]") -> int:
    """Print one JSON document describing each of `import_paths`; 1 if any was refused.

    Wire contract with the engine: stdout carries exactly
    `{"described_node_types": [...], "refused_node_types": [{"import_path": ...,
    "refusal": ...}]}`, and every refusal is also a block on stderr beginning
    `cannot describe <import path>: `.
    """
    described_document_fd = _reserve_stdout_for_the_described_document()
    described_node_types: "list[dict[str, Any]]" = []
    refused_node_types: "list[dict[str, str]]" = []
    for import_path in import_paths:
        try:
            described_node_types.append(describe_one_processor_class(import_path))
        except ProcessorClassNotDescribable as refusal:
            refused_node_types.append({"import_path": import_path, "refusal": str(refusal)})
            sys.stderr.write(f"cannot describe {import_path}: {refusal}\n")
            sys.stderr.flush()
    described_document = json.dumps(
        {
            "described_node_types": described_node_types,
            "refused_node_types": refused_node_types,
        },
        allow_nan=False,
    )
    with os.fdopen(described_document_fd, "w", encoding="utf-8") as described_document_output:
        described_document_output.write(described_document + "\n")
    return 1 if refused_node_types else 0


def node_classes_installed_distributions_declare() -> "list[dict[str, str]]":
    """Every `tatolab.nodes` entry point an installed distribution declares,
    once per import path, in the order `importlib.metadata` finds them."""
    from importlib import metadata

    installed_modules: "list[dict[str, str]]" = []
    seen_import_paths: "set[str]" = set()
    for entry_point in metadata.entry_points(group=INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP):
        import_path = entry_point.value.split("[", 1)[0].strip().replace(" ", "")
        if import_path in seen_import_paths:
            continue
        seen_import_paths.add(import_path)
        distribution = entry_point.dist
        installed_modules.append(
            {
                "import_path": import_path,
                "entry_point": entry_point.name,
                "distribution": distribution.name if distribution is not None else "",
                "distribution_version": (
                    distribution.version if distribution is not None else ""
                ),
            }
        )
    return installed_modules


def describe_installed_modules_onto_stdout() -> int:
    """Print one JSON document describing every class installed distributions
    declare in `tatolab.nodes`; always 0, since an installed module that cannot
    be described is reported in the document and never fails the stream.

    Wire contract with the engine: the `--describe` document, plus
    `"installed_modules": [{"import_path": ..., "entry_point": ...,
    "distribution": ..., "distribution_version": ...}]` naming where each
    described or refused class came from.
    """
    described_document_fd = _reserve_stdout_for_the_described_document()
    described_node_types: "list[dict[str, Any]]" = []
    refused_node_types: "list[dict[str, str]]" = []
    try:
        installed_modules = node_classes_installed_distributions_declare()
    except Exception:
        installed_modules = []
        sys.stderr.write(
            f"cannot list the `{INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP}` entry points:\n"
            f"{traceback.format_exc()}"
        )
    for installed_module in installed_modules:
        import_path = installed_module["import_path"]
        try:
            described_node_types.append(describe_one_processor_class(import_path))
        except ProcessorClassNotDescribable as refusal:
            refused_node_types.append({"import_path": import_path, "refusal": str(refusal)})
            sys.stderr.write(
                f"cannot describe {import_path}, which {installed_module['distribution']} "
                f"declares in `{INSTALLED_MODULE_NODES_ENTRY_POINT_GROUP}`: {refusal}\n"
            )
    sys.stderr.flush()
    described_document = json.dumps(
        {
            "described_node_types": described_node_types,
            "refused_node_types": refused_node_types,
            "installed_modules": installed_modules,
        },
        allow_nan=False,
    )
    with os.fdopen(described_document_fd, "w", encoding="utf-8") as described_document_output:
        described_document_output.write(described_document + "\n")
    return 0


# =============================================================================
# Opening this processor's own ports
# =============================================================================


def wire_link_data_access(
    link_data_access: NodeLinkDataAccess, port_wiring: "dict[str, Any]"
) -> None:
    """Open this processor's publishers and subscribers, one call per link.

    Inputs are wired before outputs so a helper that is both a source and a
    destination is ready to receive before it can publish.
    """
    for input_link in port_wiring.get("inputs", []):
        link_data_access.wire_input_link(
            input_link["name"],
            input_link["channel_service_name"],
            input_link["inbound_link_name"],
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
            input_port_borrow_quota=input_link.get("input_port_borrow_quota"),
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
    link_data_access: NodeLinkDataAccess, command: "dict[str, Any]"
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
    link_data_access: NodeLinkDataAccess,
    runtime_id: str,
    processor_id: str,
    bridge: ParentProcessBridge,
) -> HostedProcessor:
    """Build the user's object and the two contexts its hooks receive.

    The bridge's escalate round trip is what the GPU surface crosses to the
    parent on — without it, `ctx.gpu_limited_access` refuses by name.
    """
    full_access_context = open_runtime_context_full_access_for_helper_process(
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
        limited_access_view_of_runtime_context_for_helper_process(full_access_context),
    )


class HelperProcessLifecycle:
    """Drives one hosted processor from the commands its parent sends."""

    def __init__(
        self,
        bridge: ParentProcessBridge,
        processor_class: type,
        runtime_id: str,
        processor_id: str,
        link_data_access: NodeLinkDataAccess,
    ) -> None:
        self._bridge = bridge
        self._processor_class = processor_class
        self._runtime_id = runtime_id
        self._processor_id = processor_id
        self._link_data_access = link_data_access
        self._hosted: Optional[HostedProcessor] = None
        self._set_up_succeeded = False
        self._setup_raised_on_its_own = False
        self._running = False
        self._torn_down = False
        self._board_service_name_of_a_stream_clock_that_may_step: "Optional[str]" = None
        self._paused = False
        self._scheduling_loop_runs = False
        self._pause_hook_last_called: "Optional[str]" = None
        self._function_node_islands_it_reads: "Optional[FunctionNodeIslandsFedToTheirPorts]" = None

    def run_until_the_parent_is_done(self) -> None:
        while not self._torn_down:
            try:
                command = self._bridge.next_lifecycle_command()
                if command is None:
                    log.info("the parent closed the channel; shutting down")
                    self._tear_down_after_the_parent_went_away()
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

    def _tear_down_after_the_parent_went_away(self) -> None:
        """Run the `stop` and `teardown()` a parent that is gone can no longer
        ask for, so a processor still releases what it holds — unless its
        `setup()` raised on its own, which the ladder never tears down."""
        if self._hosted is None or self._torn_down or self._setup_raised_on_its_own:
            return
        self._stop()
        if sys.platform == "darwin":
            note_this_helper_processes_callbacks_returned_after_its_parent_went_away()
        self._teardown()

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
        elif verb == "feed_island":
            # Answered on the island's link, like `wire_link`.
            self._feed_island(command)
        elif verb == "unfeed_island":
            # Unanswered, like `unwire_link`.
            self._unfeed_island(command)
        elif verb == "set_pin_values":
            # Unanswered, like `unwire_link`.
            self._set_pin_values(command)
        else:
            log.warn("the parent sent an unknown lifecycle command", cmd=verb)

    def _setup(self, command: "dict[str, Any]") -> None:
        try:
            configuration = command.get("config")
            if self._processor_class is FunctionNodePool:
                self._processor_class = the_processor_class_the_function_node_pool_runs_as(
                    configuration or {}
                )
                configuration = None
            # Declared ahead of any wiring: a processor added to a running
            # graph has ports before it has links, and reads and writes on
            # them must not raise in between.
            self._link_data_access.declare_ports(
                [
                    port["name"]
                    for port in getattr(
                        self._processor_class, NODE_DECLARATION_INPUT_PORTS_STAMP, []
                    )
                ],
                [
                    port["name"]
                    for port in getattr(
                        self._processor_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP, []
                    )
                ],
            )
            # Opened before any link is wired, so every link mirrors its losses
            # from its first. Only a stand-in parent sends no board.
            loss_count_board = command.get("loss_count_board")
            if loss_count_board is not None:
                self._link_data_access.open_loss_count_board(
                    loss_count_board["service_name"],
                    loss_count_board["output_ports"],
                    [
                        (entry["breadcrumb_number"], entry["resumes_from_f64_bits"])
                        for entry in loss_count_board.get("function_node_states", [])
                    ],
                )
            stream_clock = command.get("stream_clock") or {}
            stream_clock_board_service_name = stream_clock.get("board_service_name")
            if stream_clock_board_service_name is not None:
                self._link_data_access.adopt_the_stream_clock_board(
                    stream_clock_board_service_name
                )
                if stream_clock.get("mode") in ("stepped", "scaled") and not self._paced_by_real_time():
                    self._board_service_name_of_a_stream_clock_that_may_step = (
                        stream_clock_board_service_name
                    )
            port_wiring = command.get("ports") or {}
            wire_link_data_access(self._link_data_access, port_wiring)
            self._function_node_islands_it_reads = (
                feed_each_input_port_from_its_function_node_island(
                    self._link_data_access,
                    port_wiring.get("islands") or [],
                    load_processor_class,
                )
            )
            read_each_unlinked_input_port_as_its_pin_value(
                link_data_access=self._link_data_access,
                pin_values=dict(port_wiring.get("pins") or {}),
            )
            self._hosted = construct_hosted_processor(
                self._processor_class,
                configuration,
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
            self._setup_raised_on_its_own = True
            self._bridge.send(
                {
                    "rpc": "error",
                    "error": f"{setup_failure}\n{traceback.format_exc()}",
                }
            )
            return
        self._set_up_succeeded = True
        ready: "dict[str, Any]" = {"rpc": "ready"}
        if self._function_node_islands_it_reads is not None and (
            self._function_node_islands_it_reads.ran_compiled_by_node_id
        ):
            ready["function_nodes_ran_compiled"] = (
                self._function_node_islands_it_reads.ran_compiled_by_node_id
            )
        self._bridge.send(ready)

    def _run(self, command: "dict[str, Any]") -> None:
        if self._hosted is None:
            log.warn("the parent sent `run` before `setup`")
            return
        execution_mode = command.get("execution", "reactive")
        try:
            note_execution_mode_from_parent_on_runtime_context(
                self._hosted.full_access_context, execution_mode
            )
        except ValueError as refusal:
            log.error(f"the processor cannot run: {refusal}")
            return
        self._running = True
        note_stop_request_from_parent_on_runtime_context(
            self._hosted.full_access_context, False
        )
        if execution_mode in ("reactive", "continuous"):
            self._run_the_scheduling_loop(execution_mode, int(command.get("interval_ms") or 0))
        elif execution_mode == "manual":
            self._hosted.call_hook("start", self._hosted.full_access_context)
        else:
            log.warn("the parent named an unknown execution mode", mode=execution_mode)

    def _run_the_scheduling_loop(self, execution_mode: str, interval_ms: int) -> None:
        """Hand the processor to the engine's scheduling loop for
        `execution_mode` until a lifecycle command stops it."""
        board_service_name = self._board_service_name_of_a_stream_clock_that_may_step
        if (
            execution_mode == "continuous"
            and board_service_name is not None
            and self._link_data_access.stream_clock_board_publish_listener_fd() is None
        ):
            self._link_data_access.listen_for_each_stream_clock_board_publish(board_service_name)
        self._scheduling_loop_runs = True
        try:
            run_the_scheduling_loop_of_this_helper(
                processor_interpreter=self,
                link_data_access=self._link_data_access,
                lifecycle_command_arrival_fd=self._bridge.lifecycle_command_arrival_fd(),
                execution_mode=execution_mode,
                interval_ms=interval_ms,
                paced_by_real_time=self._paced_by_real_time(),
                stream_clock_may_step=board_service_name is not None,
                paused=self._paused,
            )
        finally:
            self._scheduling_loop_runs = False

    def _paced_by_real_time(self) -> bool:
        return getattr(self._processor_class, NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP, False) is True

    def call_hook_from_the_scheduling_loop(self, hook_name: str) -> None:
        """Make the callback the engine's scheduling loop asks for."""
        assert self._hosted is not None
        if hook_name in ("on_pause", "on_resume"):
            self._call_the_pause_hook_once_per_transition(hook_name)
            return
        self._hosted.call_hook(hook_name, self._hosted.limited_access_context)

    def take_the_lifecycle_commands_that_woke_the_scheduling_loop(self) -> "tuple[bool, bool]":
        """Dispatch every lifecycle command waiting, answering whether the
        processor still runs and whether it is paused."""
        self._bridge.clear_lifecycle_command_arrivals()
        self._drain_commands_arriving_mid_run()
        return (self._running and not self._torn_down, self._paused)

    def _call_the_pause_hook_once_per_transition(self, hook_name: str) -> None:
        """Call `on_pause` or `on_resume`, never the one called last again: a
        pause noted before the loop started is not announced twice."""
        assert self._hosted is not None
        if hook_name == self._pause_hook_last_called:
            return
        self._pause_hook_last_called = hook_name
        self._hosted.call_hook(hook_name, self._hosted.limited_access_context)

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
            note_stop_request_from_parent_on_runtime_context(
                self._hosted.full_access_context, True
            )
            self._hosted.call_hook("stop", self._hosted.full_access_context)
        self._bridge.send({"rpc": "stopped"})

    def _teardown(self) -> None:
        self._running = False
        self._torn_down = True
        if self._hosted is not None:
            note_stop_request_from_parent_on_runtime_context(
                self._hosted.full_access_context, True
            )
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
        """Record the pause the parent announced; while a scheduling loop runs,
        the loop makes the callback itself."""
        self._paused = verb == "on_pause"
        if self._hosted is not None:
            note_pause_state_from_parent_on_runtime_context(
                self._hosted.full_access_context, verb == "on_pause"
            )
            if not self._scheduling_loop_runs:
                self._call_the_pause_hook_on_the_step_ledger(verb)
        self._bridge.send({"rpc": "ok"})

    def _call_the_pause_hook_on_the_step_ledger(self, hook_name: str) -> None:
        """Call `hook_name` outside a scheduling loop, counted in flight on the
        step ledger while it runs when the stream's clock may step."""
        if self._board_service_name_of_a_stream_clock_that_may_step is None:
            self._call_the_pause_hook_once_per_transition(hook_name)
            return
        self._link_data_access.note_a_step_callback_started()
        try:
            self._call_the_pause_hook_once_per_transition(hook_name)
        finally:
            self._link_data_access.note_a_step_callback_returned(None)

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

    def _feed_island(self, command: "dict[str, Any]") -> None:
        island = command.get("island") or {}
        link_id = island.get("link_id")
        input_port = island.get("input_port")
        if not self._set_up_succeeded or self._function_node_islands_it_reads is None:
            self._answer_the_parents_wire_link(
                link_id, "this processor's setup did not succeed, so no island feeds its inputs"
            )
            return
        try:
            self._function_node_islands_it_reads.feed_an_input_port_from_its_island(island)
        except Exception as feed_failure:
            log.error(
                "this processor could not evaluate an island the engine handed it after setup",
                link_id=link_id,
                input_port=input_port,
                error=str(feed_failure),
            )
            self._answer_the_parents_wire_link(link_id, str(feed_failure))
            return
        self._answer_the_parents_wire_link(link_id, None)

    def _unfeed_island(self, command: "dict[str, Any]") -> None:
        input_port = command.get("port")
        if self._function_node_islands_it_reads is None or not isinstance(input_port, str):
            return
        try:
            self._function_node_islands_it_reads.stop_feeding_an_input_port(input_port)
        except Exception as unfeed_failure:
            log.error(
                "this processor could not stop evaluating an island unlinked from it",
                link_id=command.get("link_id"),
                input_port=input_port,
                error=str(unfeed_failure),
            )

    def _set_pin_values(self, command: "dict[str, Any]") -> None:
        try:
            read_each_unlinked_input_port_as_its_pin_value(
                link_data_access=self._link_data_access,
                pin_values=dict(command.get("pins") or {}),
            )
        except Exception as pin_values_refused:
            log.error(
                "this processor could not read the pin values the engine handed it",
                error=str(pin_values_refused),
            )

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


def _write_a_bootstrap_fatal_to_raw_stderr(bootstrap_failure: object) -> None:
    # Pre-install fatal: there is no channel to report it on, so this goes to
    # raw stderr, which the parent captures.
    sys.stderr.write(f"[streamlib] {bootstrap_failure}\n")
    sys.stderr.flush()


def main(arguments: "list[str]") -> int:
    """Describe the classes `--describe` names, or the installed modules'
    classes, or run one processor until its parent tears it down."""
    try:
        _refuse_an_engine_built_other_than_the_parents()
    except HelperProcessProtocolError as bootstrap_failure:
        _write_a_bootstrap_fatal_to_raw_stderr(bootstrap_failure)
        return 1
    if arguments[:1] == [DESCRIBE_ARGUMENT]:
        if len(arguments) == 1:
            _write_a_bootstrap_fatal_to_raw_stderr(
                f"`{DESCRIBE_ARGUMENT}` takes one or more import paths, `module:qualname`"
            )
            return 1
        _end_the_describe_once_its_parent_closes_stdin()
        log.install_helper_process_sink(write_a_describe_time_log_record_to_stderr)
        try:
            describe_exit_status = describe_processor_classes_onto_stdout(arguments[1:])
        except BaseException:
            traceback.print_exc()
            describe_exit_status = 1
        _end_the_describe_without_finalizing_the_interpreter(describe_exit_status)
    if arguments == [DESCRIBE_INSTALLED_MODULES_ARGUMENT]:
        _end_the_describe_once_its_parent_closes_stdin()
        log.install_helper_process_sink(write_a_describe_time_log_record_to_stderr)
        try:
            describe_exit_status = describe_installed_modules_onto_stdout()
        except BaseException:
            traceback.print_exc()
            describe_exit_status = 1
        _end_the_describe_without_finalizing_the_interpreter(describe_exit_status)
    if arguments:
        _write_a_bootstrap_fatal_to_raw_stderr(
            f"a processor interpreter takes no arguments, `{DESCRIBE_ARGUMENT}` and "
            f"import paths, or `{DESCRIBE_INSTALLED_MODULES_ARGUMENT}` alone; it was given "
            f"{arguments!r}"
        )
        return 1
    run_one_processor_until_its_parent_tears_it_down()
    return 0


def _keep_the_native_escalate_socket_from_what_the_processor_starts() -> None:
    """Mark the inherited native escalate socket close-on-exec, as the bridge's own is."""
    raw_fd = os.environ.get(NATIVE_ESCALATE_FD_ENV)
    if not raw_fd:
        return
    try:
        os.set_inheritable(int(raw_fd), False)
    except (ValueError, OSError) as unusable:
        raise HelperProcessProtocolError(
            f"{NATIVE_ESCALATE_FD_ENV}={raw_fd!r} is not a descriptor this helper holds"
        ) from unusable


def run_one_processor_until_its_parent_tears_it_down() -> None:
    """Run the processor `STREAMLIB_ENTRYPOINT` names until its parent tears it down."""
    try:
        import_path = _required_environment(ENTRYPOINT_ENV)
        processor_id = _required_environment(PROCESSOR_ID_ENV)
        runtime_id = os.environ.get(RUNTIME_ID_ENV, "")
        bridge = ParentProcessBridge.open_from_inherited_fd()
        _keep_the_native_escalate_socket_from_what_the_processor_starts()
    except HelperProcessProtocolError as bootstrap_failure:
        _write_a_bootstrap_fatal_to_raw_stderr(bootstrap_failure)
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

    # Before anything a processor brings runs, and on macOS only: Linux binds
    # the helper to its parent with `PR_SET_PDEATHSIG` at spawn.
    if sys.platform == "darwin":
        try:
            watch_for_this_helper_processes_parent_going_away(bridge.parent_channel_fd())
        except RuntimeError as watch_failure:
            engine_log_forwarder.stop_after_forwarding_what_is_left()
            log.error(
                "the helper could not watch its parent, so it could outlive it",
                error=str(watch_failure),
            )
            sys.exit(1)

    try:
        processor_class = load_processor_class(import_path)
        link_data_access = open_node_link_data_access_for_helper_process()
        if isinstance(processor_class, FunctionNode):
            processor_class = the_processor_class_a_promoted_function_node_runs_as(
                processor_class, import_path, processor_id, link_data_access
            )
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
    sys.exit(main(sys.argv[1:]))
