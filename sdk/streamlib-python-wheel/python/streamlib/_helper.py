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
from ._engine import (
    MonotonicTimer,
    ProcessorLinkDataAccess,
    RuntimeContextFullAccess,
)
from ._processor_hosting import apply_configuration, construct_processor_instance

ENTRYPOINT_ENV = "STREAMLIB_ENTRYPOINT"
PROCESSOR_ID_ENV = "STREAMLIB_PROCESSOR_ID"
RUNTIME_ID_ENV = "STREAMLIB_RUNTIME_ID"
ESCALATE_FD_ENV = "STREAMLIB_ESCALATE_FD"
PROTOCOL_VERSION_ENV = "STREAMLIB_PROTOCOL_VERSION"

# The engine and this module ship in one artifact, so the version cannot
# disagree with itself. The handshake stays as an assertion because a stale
# `streamlib` earlier on the child's `sys.path` is still reachable.
PROTOCOL_VERSION = 2

# Upper bound on how long an escalate request waits for its correlated
# response. Generous enough for a cold GPU allocation under load; bounded so
# a wedged parent surfaces as an error instead of a hung processor callback.
ESCALATE_REQUEST_TIMEOUT_SECONDS = 60.0

# How long a blocking wait may park before the lifecycle queue is drained
# again. Teardown latency is bounded by this plus one callback.
LIFECYCLE_POLL_INTERVAL_SECONDS = 0.1
LIFECYCLE_POLL_INTERVAL_MILLISECONDS = 100


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
    """

    _FRAME_LENGTH_PREFIX = struct.Struct(">I")

    def __init__(self, parent_socket: socket.socket) -> None:
        self._socket = parent_socket
        self._read_stream = parent_socket.makefile("rb", buffering=0)
        self._write_stream = parent_socket.makefile("wb", buffering=0)
        self._write_lock = threading.Lock()
        self._lifecycle_commands: "queue.Queue[Optional[dict[str, Any]]]" = queue.Queue()
        self._pending_escalate_responses: "dict[str, _PendingEscalateResponse]" = {}
        self._pending_lock = threading.Lock()
        self._channel_closed = False
        self._reader = threading.Thread(
            target=self._demultiplex_frames_from_parent,
            name="streamlib-parent-bridge",
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
        return cls(socket.socket(fileno=inherited_fd))

    def start_reading(self) -> None:
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
            with self._pending_lock:
                self._pending_escalate_responses.pop(request_id, None)
        # `slot.message` rather than the wait result decides: a delivery can
        # land between the wait timing out and the slot being popped.
        response = slot.message
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

    def next_lifecycle_command(self) -> "Optional[dict[str, Any]]":
        """Block until the parent sends one, or `None` once it is gone."""
        return self._lifecycle_commands.get()

    def next_lifecycle_command_if_waiting(self) -> "tuple[bool, Optional[dict[str, Any]]]":
        """`(True, command)` when one was queued, `(False, None)` otherwise.

        The `None` a closed channel puts on the queue is a command too — the
        first element distinguishes "nothing queued" from "the parent is gone".
        """
        try:
            return True, self._lifecycle_commands.get_nowait()
        except queue.Empty:
            return False, None

    def _demultiplex_frames_from_parent(self) -> None:
        while True:
            frame = self._read_next_frame()
            if frame is None:
                self._wake_every_pending_escalate_caller()
                self._lifecycle_commands.put(None)
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
            self._lifecycle_commands.put(frame)

    def _deliver_escalate_response(self, response: "dict[str, Any]") -> bool:
        request_id = response.get("request_id")
        if not isinstance(request_id, str):
            return False
        with self._pending_lock:
            slot = self._pending_escalate_responses.get(request_id)
        if slot is None:
            return False
        slot.message = response
        slot.arrived.set()
        return True

    def _wake_every_pending_escalate_caller(self) -> None:
        """A closed channel fails every in-flight request rather than hanging it."""
        with self._pending_lock:
            self._channel_closed = True
            orphaned = list(self._pending_escalate_responses.values())
            self._pending_escalate_responses.clear()
        for slot in orphaned:
            slot.message = None
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
        with self._sequence_lock:
            self._next_sequence_number += 1
            sequence_number = self._next_sequence_number
        self._bridge.send(
            {
                "rpc": "escalate_request",
                "op": "log",
                "source": "python",
                "source_seq": str(sequence_number),
                # Advisory only — the parent's receipt stamp is what orders
                # the merged stream. Wall clock is what a human reads.
                "source_ts": datetime.now(timezone.utc).isoformat(),
                "level": level,
                "message": message,
                "intercepted": False,
                "channel": None,
                "pipeline_id": None,
                "processor_id": self._processor_id,
                "attrs": attrs or {},
            }
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
            input_link["notify_service_name"],
            input_link["read_mode"],
            input_link["max_queued_messages"],
            input_link["max_subscribers"],
            input_link["notify_max_notifiers"],
            input_link["link_id"],
            # Absent on every port that declares no window contract, which is
            # unchanged in every respect. Present, it is the values already
            # resolved: a `match_device` sentinel settles in the parent, which is
            # where the device stream is.
            input_link.get("audio_window"),
        )
    for output_link in port_wiring.get("outputs", []):
        link_data_access.wire_output_link(
            output_link["name"],
            output_link["channel_service_name"],
            output_link["dest_notify_service_name"],
            output_link["expected_payload_bytes"],
            output_link["max_payload_bytes_per_channel"],
            output_link["max_queued_messages"],
            output_link["max_subscribers"],
            output_link["notify_max_notifiers"],
            output_link["link_id"],
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
        being driven, exactly as it would in the parent.
        """
        if hook_name not in self._declared_hooks:
            return
        try:
            getattr(self.instance, hook_name)(context)
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
        self._running = False
        self._torn_down = False

    def run_until_the_parent_is_done(self) -> None:
        while not self._torn_down:
            command = self._bridge.next_lifecycle_command()
            if command is None:
                log.info("the parent closed the channel; shutting down")
                return
            self._dispatch(command)

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
        else:
            log.warn("the parent sent an unknown lifecycle command", cmd=verb)

    def _setup(self, command: "dict[str, Any]") -> None:
        try:
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
        except Exception as setup_failure:
            self._bridge.send(
                {
                    "rpc": "error",
                    "error": f"{setup_failure}\n{traceback.format_exc()}",
                }
            )
            return
        self._bridge.send({"rpc": "ready", "protocol_version": PROTOCOL_VERSION})

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
                self._hosted.call_hook("process", self._hosted.limited_access_context)
                self._drain_commands_arriving_mid_run()
                continue
            # Re-read before every wait, never cached across one: the listener
            # owns this fd, and an `unwire_link` taking this processor's last
            # inbound link drops the listener and closes it. Selecting on the
            # stale number raises EBADF — or, once the OS recycles it, waits on
            # something else entirely.
            listener_fd = self._link_data_access.input_listener_fd()
            if listener_fd is not None and listener_fd >= 0:
                readable, _, _ = select.select(
                    [listener_fd], [], [], LIFECYCLE_POLL_INTERVAL_SECONDS
                )
                if readable:
                    self._link_data_access.drain_input_listener()
            else:
                # No inputs left: nothing will ever wake this loop, so the only
                # thing left to wait on is the parent.
                self._park_until_a_command_arrives()
            self._drain_commands_arriving_mid_run()

    def _run_continuous(self, interval_ms: int) -> None:
        assert self._hosted is not None
        if interval_ms <= 0:
            while self._running:
                self._hosted.call_hook("process", self._hosted.limited_access_context)
                self._drain_commands_arriving_mid_run()
            return
        with MonotonicTimer(interval_ms * 1_000_000) as timer:
            while self._running:
                self._hosted.call_hook("process", self._hosted.limited_access_context)
                # A tick already consumed by this iteration is not caught up
                # on — drift-free pacing, not backlog replay.
                if timer.wait(LIFECYCLE_POLL_INTERVAL_MILLISECONDS) < 0:
                    log.error("the interval timer failed; leaving the continuous loop")
                    self._running = False
                self._drain_commands_arriving_mid_run()

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

    def _update_config(self, command: "dict[str, Any]") -> None:
        if self._hosted is not None:
            try:
                apply_configuration(self._hosted.instance, command.get("config"))
            except Exception as configuration_failure:
                log.error(
                    "the processor refused a configuration update",
                    error=str(configuration_failure),
                )
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


def _assert_the_parent_speaks_this_protocol() -> None:
    advertised = os.environ.get(PROTOCOL_VERSION_ENV)
    if advertised is None:
        return
    if advertised != str(PROTOCOL_VERSION):
        raise HelperProcessProtocolError(
            f"the engine speaks helper protocol v{advertised}, this streamlib speaks "
            f"v{PROTOCOL_VERSION}. The engine and the helper ship in one artifact, so "
            f"this means a different streamlib is earlier on this process's sys.path: "
            f"{sys.path[0]!r}"
        )


def main() -> None:
    """Run one processor until its parent tears it down."""
    try:
        _assert_the_parent_speaks_this_protocol()
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
    log.install_helper_process_sink(ParentProcessLogSink(bridge, processor_id))

    try:
        processor_class = load_processor_class(import_path)
        link_data_access = ProcessorLinkDataAccess()
    except Exception as startup_failure:
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
    log.info("helper process exiting")


if __name__ == "__main__":
    main()
